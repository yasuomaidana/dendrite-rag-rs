//! OpenAI-compatible `/v1/embeddings` (LiteLLM gateway, vLLM, LM Studio, `oxillama serve`).

use serde::{Deserialize, Serialize};

use super::http::{Client, expect_len};
use super::{EmbedConfig, Embedder, Result, normalize, prepare};

pub struct OpenAiEmbedder {
    client: Client,
    url: String,
    model: String,
    model_id: String,
    dimensions: Option<usize>,
    document_prefix: String,
    query_prefix: String,
    batch: usize,
}

impl OpenAiEmbedder {
    pub fn new(cfg: &EmbedConfig) -> Self {
        let base = cfg.url().unwrap_or_default();
        let url = if base.ends_with("/v1") {
            format!("{base}/embeddings")
        } else {
            format!("{base}/v1/embeddings")
        };
        let (document_prefix, query_prefix) = cfg.prefixes();
        Self {
            client: Client::new(cfg.api_key()),
            url,
            model: cfg.model(),
            model_id: cfg.model_id(),
            dimensions: cfg.dimensions,
            document_prefix,
            query_prefix,
            batch: cfg.batch(32),
        }
    }

    fn request(&self, input: Vec<String>) -> Result<Vec<Vec<f32>>> {
        #[derive(Serialize)]
        struct Req<'a> {
            model: &'a str,
            input: Vec<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            dimensions: Option<usize>,
        }
        #[derive(Deserialize)]
        struct Item {
            index: usize,
            embedding: Vec<f32>,
        }
        #[derive(Deserialize)]
        struct Resp {
            data: Vec<Item>,
        }
        let n = input.len();
        let mut body: Resp = self.client.post(
            &self.url,
            &Req {
                model: &self.model,
                input,
                dimensions: self.dimensions,
            },
        )?;
        expect_len(body.data.len(), n)?;
        body.data.sort_by_key(|i| i.index);
        let mut out = Vec::with_capacity(body.data.len());
        for i in body.data {
            let mut emb = i.embedding;
            if let Some(target) = self.dimensions {
                if emb.len() > target {
                    emb.truncate(target);
                } else if emb.len() < target {
                    return Err(super::EmbedError::Response(format!(
                        "model returned {} dimensions, expected at least {target}",
                        emb.len()
                    )));
                }
            }
            out.push(normalize(emb));
        }
        Ok(out)
    }
}

impl Embedder for OpenAiEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
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
    fn posts_openai_requests_and_orders_by_index() {
        let stub =
            HttpStub::serve(r#"{"data":[{"index":1,"embedding":[0,2]},{"index":0,"embedding":[3,4]}]}"#);
        let e = OpenAiEmbedder::new(&EmbedConfig {
            backend: Backend::Openai,
            url: Some(stub.url.clone() + "/v1/"),
            model: Some("nomic-embed-text".into()),
            api_key: Some("k".into()),
            ..Default::default()
        });
        let v = e.embed_documents(&["a".into(), "b".into()]).unwrap();
        assert_eq!(v, vec![vec![0.6, 0.8], vec![0.0, 1.0]]);
        let req = stub.request();
        assert!(req.head.starts_with("POST /v1/embeddings "), "{}", req.head);
        assert!(req.head.to_ascii_lowercase().contains("authorization: bearer k"));
        assert_eq!(req.body["model"], "nomic-embed-text");
        assert_eq!(req.body["input"][0], "search_document: a");
    }

    #[test]
    fn short_answers_are_errors() {
        let stub = HttpStub::serve(r#"{"data":[{"index":0,"embedding":[1]}]}"#);
        let e = OpenAiEmbedder::new(&EmbedConfig {
            backend: Backend::Openai,
            url: Some(stub.url.clone()),
            ..Default::default()
        });
        assert!(e.embed_documents(&["a".into(), "b".into()]).is_err());
    }

    #[test]
    fn dimensions_truncate_and_renormalize_and_set_model_id() {
        // Provider returns 4-d vector [1.0, 1.0, 1.0, 1.0]; target is 2-d.
        // Truncated: [1.0, 1.0] -> Normalized: [1/sqrt(2), 1/sqrt(2)] ~ [0.7071, 0.7071].
        let stub = HttpStub::serve(r#"{"data":[{"index":0,"embedding":[1.0, 1.0, 1.0, 1.0]}]}"#);
        let e = OpenAiEmbedder::new(&EmbedConfig {
            backend: Backend::Openai,
            url: Some(stub.url.clone()),
            model: Some("text-embedding-3-large".into()),
            dimensions: Some(2),
            ..Default::default()
        });
        assert_eq!(e.model_id(), "text-embedding-3-large@2");
        let v = e.embed_documents(&["test code".into()]).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].len(), 2);
        let expected = 1.0 / (2.0f32).sqrt();
        assert!((v[0][0] - expected).abs() < 1e-5);
        assert!((v[0][1] - expected).abs() < 1e-5);
        let req = stub.request();
        assert_eq!(req.body["dimensions"], 2);
    }

    /// Runs against a live gateway: `LOOM_LIVE_GATEWAY=1 cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_gateway_embeds() {
        if std::env::var("LOOM_LIVE_GATEWAY").is_err() {
            return;
        }
        let e = OpenAiEmbedder::new(&EmbedConfig {
            backend: Backend::Openai,
            ..Default::default()
        });
        let docs = e
            .embed_documents(&["func Locate(name string) string".into(), "fn main() {}".into()])
            .unwrap();
        assert_eq!(docs.len(), 2);
        let q = e.embed_query("find the binary").unwrap();
        assert_eq!(q.len(), docs[0].len());
    }

    /// Runs against a live gateway with dimensions: `LOOM_LIVE_GATEWAY=1 cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_gateway_embeds_with_dimensions() {
        if std::env::var("LOOM_LIVE_GATEWAY").is_err() {
            return;
        }
        let e = OpenAiEmbedder::new(&EmbedConfig {
            backend: Backend::Openai,
            model: Some("text-embedding-3-large".into()),
            dimensions: Some(1024),
            ..Default::default()
        });
        assert_eq!(e.model_id(), "text-embedding-3-large@1024");
        let docs = e
            .embed_documents(&["func Locate(name string) string".into()])
            .unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].len(), 1024);
    }
}
