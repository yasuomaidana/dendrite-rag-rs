//! Optional cross-encoder reranking (`docs/specs/retrieval.md` § Optional rerank).

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::embed::{EmbedError, Result};

/// Scores `(query, document)` pairs; higher is more relevant.
pub trait Reranker: Send + Sync {
    fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>>;
}

/// Reranking settings (`[rerank]` in `.dendrite/rag.toml`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RerankConfig {
    /// Cross-encoder model; reranking is off when unset.
    pub model: Option<String>,
    /// Server with a Cohere-style `/v1/rerank`; defaults to `LITELLM_PROXY_URL`, then
    /// `http://localhost:4000`.
    pub url: Option<String>,
    /// Environment variable that holds the bearer key; defaults to the LiteLLM keys.
    pub api_key_env: Option<String>,
}

/// Cohere-style `POST {base}/v1/rerank`, as served by the LiteLLM gateway.
pub struct GatewayReranker {
    agent: ureq::Agent,
    url: String,
    key: Option<String>,
    model: String,
}

impl GatewayReranker {
    /// `None` when no model is configured.
    pub fn new(cfg: &RerankConfig) -> Option<Self> {
        let model = cfg.model.clone()?;
        // Same URL and key resolution as the OpenAI-compatible embedder.
        let endpoint = crate::embed::EmbedConfig {
            backend: crate::embed::Backend::Openai,
            url: cfg.url.clone(),
            api_key_env: cfg.api_key_env.clone(),
            ..Default::default()
        };
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            .http_status_as_error(false)
            .build()
            .into();
        Some(Self {
            agent,
            url: endpoint.url().unwrap_or_default() + "/v1/rerank",
            key: endpoint.api_key(),
            model,
        })
    }
}

#[derive(Serialize)]
struct Req<'a> {
    model: &'a str,
    query: &'a str,
    documents: &'a [String],
    top_n: usize,
}

#[derive(Deserialize)]
struct Item {
    index: usize,
    relevance_score: f32,
}

#[derive(Deserialize)]
struct Resp {
    results: Vec<Item>,
}

/// Maps a rerank response onto per-document scores (documents missing from it get -inf).
fn scores(resp: Resp, n: usize) -> Result<Vec<f32>> {
    let mut out = vec![f32::NEG_INFINITY; n];
    for item in resp.results {
        *out.get_mut(item.index)
            .ok_or_else(|| EmbedError::Response(format!("rerank index {} out of range", item.index)))? =
            item.relevance_score;
    }
    Ok(out)
}

impl Reranker for GatewayReranker {
    fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>> {
        let mut req = self.agent.post(&self.url);
        if let Some(k) = &self.key {
            req = req.header("Authorization", &format!("Bearer {k}"));
        }
        let mut resp = req
            .send_json(Req {
                model: &self.model,
                query,
                documents,
                top_n: documents.len(),
            })
            .map_err(|e| EmbedError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(EmbedError::Http(format!(
                "{status}: {}",
                body.chars().take(300).collect::<String>()
            )));
        }
        let body: Resp = resp
            .body_mut()
            .read_json()
            .map_err(|e| EmbedError::Response(e.to_string()))?;
        scores(body, documents.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_results_by_index() {
        let resp: Resp = serde_json::from_str(
            r#"{"results":[{"index":2,"relevance_score":0.9},{"index":0,"relevance_score":0.1}]}"#,
        )
        .unwrap();
        assert_eq!(scores(resp, 3).unwrap(), vec![0.1, f32::NEG_INFINITY, 0.9]);
        let bad: Resp = serde_json::from_str(r#"{"results":[{"index":5,"relevance_score":1}]}"#).unwrap();
        assert!(scores(bad, 3).is_err());
    }
}
