//! OpenAI-compatible `/v1/embeddings` (LiteLLM gateway, vLLM, LM Studio, `oxillama serve`).

use serde::{Deserialize, Serialize};

use super::http::{Client, expect_len};
use super::{EmbedConfig, Embedder, Result, normalize, prepare};

pub struct OpenAiEmbedder {
    client: Client,
    url: String,
    model: String,
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
            },
        )?;
        expect_len(body.data.len(), n)?;
        body.data.sort_by_key(|i| i.index);
        Ok(body.data.into_iter().map(|i| normalize(i.embedding)).collect())
    }
}

impl Embedder for OpenAiEmbedder {
    fn model_id(&self) -> &str {
        // Bare model name: indexes built before backends were selectable keep their vectors.
        &self.model
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
}
