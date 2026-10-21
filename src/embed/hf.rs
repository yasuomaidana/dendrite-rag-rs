//! Hugging Face Inference (serverless `feature-extraction` or a dedicated endpoint) and
//! Text Embeddings Inference servers. All accept `{"inputs": [...]}`.

use serde::Serialize;
use serde_json::Value;

use super::http::{Client, expect_len};
use super::{EmbedConfig, EmbedError, Embedder, Pooling, Result, normalize, pool, prepare};

/// Serverless Inference for a hub model.
const SERVERLESS: &str = "https://router.huggingface.co/hf-inference/models";

pub struct HfEmbedder {
    client: Client,
    url: String,
    id: String,
    pooling: Pooling,
    document_prefix: String,
    query_prefix: String,
    batch: usize,
}

impl HfEmbedder {
    pub fn new(cfg: &EmbedConfig) -> Result<Self> {
        let model = cfg.model();
        let url = endpoint(cfg.url().as_deref(), &model);
        let key = cfg.api_key();
        if url.starts_with(SERVERLESS) && key.is_none() {
            return Err(EmbedError::Model(
                "Hugging Face Inference needs a token: set HF_TOKEN, run `huggingface-cli login`, or set embed.api_key_env".into(),
            ));
        }
        let (document_prefix, query_prefix) = cfg.prefixes();
        Ok(Self {
            client: Client::new(key),
            url,
            id: format!("hf:{model}"),
            pooling: cfg.pooling(),
            document_prefix,
            query_prefix,
            batch: cfg.batch(32),
        })
    }

    fn request(&self, inputs: Vec<String>) -> Result<Vec<Vec<f32>>> {
        #[derive(Serialize)]
        struct Req {
            inputs: Vec<String>,
            truncate: bool,
        }
        let n = inputs.len();
        let body: Value = self.client.post(
            &self.url,
            &Req {
                inputs,
                truncate: true,
            },
        )?;
        let vectors = parse(body, self.pooling)?;
        expect_len(vectors.len(), n)?;
        Ok(vectors.into_iter().map(normalize).collect())
    }
}

/// The URL to POST to: a configured full URL (`…/embed`, `…/pipeline/…`, a dedicated
/// endpoint) as given, any other configured server as TEI (`/embed`), else serverless.
fn endpoint(url: Option<&str>, model: &str) -> String {
    match url {
        Some(u)
            if u.ends_with("/embed")
                || u.contains("/pipeline/")
                || u.contains(".endpoints.huggingface.cloud") =>
        {
            u.to_string()
        }
        Some(u) => format!("{u}/embed"),
        None => format!("{SERVERLESS}/{model}/pipeline/feature-extraction"),
    }
}

/// Sentence vectors (`[[f32]]`) as they are; token states (`[[[f32]]]`, models without a
/// pooling head) pooled.
fn parse(body: Value, pooling: Pooling) -> Result<Vec<Vec<f32>>> {
    let bad = |e: serde_json::Error| EmbedError::Response(e.to_string());
    let token_level = body.get(0).and_then(|v| v.get(0)).is_some_and(Value::is_array);
    if token_level {
        let states: Vec<Vec<Vec<f32>>> = serde_json::from_value(body).map_err(bad)?;
        Ok(states.iter().map(|t| pool(t, pooling)).collect())
    } else {
        serde_json::from_value(body).map_err(bad)
    }
}

impl Embedder for HfEmbedder {
    fn model_id(&self) -> &str {
        &self.id
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(self.batch) {
            out.extend(self.request(prepare(&self.document_prefix, batch))?);
        }
        Ok(out)
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        Ok(self
            .request(prepare(&self.query_prefix, &[text.to_string()]))?
            .remove(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::Backend;
    use crate::testutil::HttpStub;

    #[test]
    fn endpoints() {
        assert_eq!(
            endpoint(None, "BAAI/bge-base-en-v1.5"),
            "https://router.huggingface.co/hf-inference/models/BAAI/bge-base-en-v1.5/pipeline/feature-extraction"
        );
        assert_eq!(endpoint(Some("http://tei:8080"), "m"), "http://tei:8080/embed");
        assert_eq!(
            endpoint(Some("http://tei:8080/embed"), "m"),
            "http://tei:8080/embed"
        );
        let dedicated = "https://abc.us-east-1.aws.endpoints.huggingface.cloud";
        assert_eq!(endpoint(Some(dedicated), "m"), dedicated);
    }

    #[test]
    fn serverless_without_a_token_is_an_error() {
        let err = HfEmbedder::new(&EmbedConfig {
            backend: Backend::Hf,
            api_key_env: Some("DENDRITE_RAG_TEST_UNSET_TOKEN".into()),
            ..Default::default()
        });
        // A developer machine may hold a token; only a missing one must fail.
        let cfg = EmbedConfig {
            backend: Backend::Hf,
            api_key_env: Some("DENDRITE_RAG_TEST_UNSET_TOKEN".into()),
            ..Default::default()
        };
        if cfg.api_key().is_none() {
            assert!(err.is_err());
        }
    }

    #[test]
    fn tei_sentence_vectors() {
        let stub = HttpStub::serve("[[3,4],[0,1]]");
        let e = HfEmbedder::new(&EmbedConfig {
            backend: Backend::Hf,
            url: Some(stub.url.clone()),
            model: Some("BAAI/bge-small-en-v1.5".into()),
            ..Default::default()
        })
        .unwrap();
        let v = e.embed_documents(&["a".into(), "b".into()]).unwrap();
        assert_eq!(v, vec![vec![0.6, 0.8], vec![0.0, 1.0]]);
        let req = stub.request();
        assert!(req.head.starts_with("POST /embed "), "{}", req.head);
        assert_eq!(req.body["inputs"][1], "b");
        assert_eq!(e.model_id(), "hf:BAAI/bge-small-en-v1.5");
    }

    #[test]
    fn token_states_are_pooled() {
        let v = parse(serde_json::json!([[[1.0, 0.0], [3.0, 2.0]]]), Pooling::Mean).unwrap();
        assert_eq!(v, vec![vec![2.0, 1.0]]);
    }

    /// Needs HF_TOKEN: `DENDRITE_RAG_LIVE_HF=1 cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_hf_inference_embeds() {
        if std::env::var("DENDRITE_RAG_LIVE_HF").is_err() {
            return;
        }
        let e = HfEmbedder::new(&EmbedConfig {
            backend: Backend::Hf,
            ..Default::default()
        })
        .unwrap();
        let d = e
            .embed_documents(&["func Locate(name string) string".into()])
            .unwrap();
        let q = e.embed_query("find a sidecar binary").unwrap();
        assert_eq!(d[0].len(), 768);
        assert_eq!(q.len(), 768);
    }
}
