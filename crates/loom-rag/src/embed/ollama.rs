//! Ollama's native embedding endpoint, `POST /api/embed`.

use serde::{Deserialize, Serialize};

use super::http::{Client, expect_len};
use super::{EmbedConfig, Embedder, Result, impl_batched_http_embedder, normalize, prepare};

pub struct OllamaEmbedder {
    client: Client,
    url: String,
    model: String,
    id: String,
    document_prefix: String,
    query_prefix: String,
    batch: usize,
}

impl OllamaEmbedder {
    pub fn new(cfg: &EmbedConfig) -> Self {
        let model = cfg.model();
        let (document_prefix, query_prefix) = cfg.prefixes();
        Self {
            client: Client::new(cfg.api_key()),
            url: format!("{}/api/embed", cfg.url().unwrap_or_default()),
            id: format!("ollama:{model}"),
            model,
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
            /// Cut inputs longer than the model's context instead of failing the batch.
            truncate: bool,
        }
        #[derive(Deserialize)]
        struct Resp {
            embeddings: Vec<Vec<f32>>,
        }
        let n = input.len();
        let body: Resp = self.client.post(
            &self.url,
            &Req {
                model: &self.model,
                input,
                truncate: true,
            },
        )?;
        expect_len(body.embeddings.len(), n)?;
        Ok(body.embeddings.into_iter().map(normalize).collect())
    }
}

impl_batched_http_embedder!(OllamaEmbedder, id);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::Backend;
    use crate::testutil::HttpStub;

    #[test]
    fn posts_api_embed() {
        let stub = HttpStub::serve(r#"{"model":"nomic-embed-text","embeddings":[[3,4]]}"#);
        let e = OllamaEmbedder::new(&EmbedConfig {
            backend: Backend::Ollama,
            url: Some(stub.url.clone()),
            ..Default::default()
        });
        assert_eq!(e.model_id(), "ollama:nomic-embed-text");
        assert_eq!(e.embed_query("where").unwrap(), vec![0.6, 0.8]);
        let req = stub.request();
        assert!(req.head.starts_with("POST /api/embed "), "{}", req.head);
        assert_eq!(req.body["input"][0], "search_query: where");
        assert_eq!(req.body["truncate"], true);
    }

    /// Needs a local Ollama with nomic-embed-text: `LOOM_LIVE_OLLAMA=1 cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_ollama_embeds() {
        if std::env::var("LOOM_LIVE_OLLAMA").is_err() {
            return;
        }
        let e = OllamaEmbedder::new(&EmbedConfig {
            backend: Backend::Ollama,
            ..Default::default()
        });
        let d = e
            .embed_documents(&["func Locate(name string) string".into()])
            .unwrap();
        let q = e.embed_query("find a sidecar binary").unwrap();
        assert_eq!(d[0].len(), 768);
        assert_eq!(q.len(), 768);
    }
}
