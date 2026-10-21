//! Embedding backends (`docs/specs/retrieval.md` § Dense index).

use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("embedding request failed: {0}")]
    Http(String),
    #[error("embedding response invalid: {0}")]
    Response(String),
    #[error("embedding model unavailable: {0}")]
    Model(String),
}

pub type Result<T> = std::result::Result<T, EmbedError>;

/// Turns text into L2-normalized vectors.
pub trait Embedder: Send + Sync {
    /// Identifies the model; cached vectors are keyed by it.
    fn model_id(&self) -> &str;
    /// Embeds chunks for indexing.
    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    /// Embeds a search query.
    fn embed_query(&self, text: &str) -> Result<Vec<f32>>;
}

/// Which embedder the engine uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// OpenAI-compatible `/v1/embeddings`, normally the LiteLLM gateway.
    #[default]
    Gateway,
    /// In-process static embeddings (feature `model2vec`).
    Model2vec,
    /// Lexical and symbol retrieval only.
    None,
}

/// Embedding settings. Unset fields fall back to the environment, then to defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmbedConfig {
    pub backend: Backend,
    /// Model name; defaults to `local-nomic-embed-text-latest` (gateway) or
    /// `minishlab/potion-code-16M-v2` (model2vec).
    pub model: Option<String>,
    /// Gateway base URL; defaults to `LITELLM_PROXY_URL`, then `http://localhost:4000`.
    pub base_url: Option<String>,
    /// Bearer key; defaults to `LITELLM_API_KEY`, `LITELLM_MASTER_KEY` or `OPENAI_API_KEY`.
    pub api_key: Option<String>,
    /// Prefixes for asymmetric models; nomic models default to `search_document: ` and
    /// `search_query: `.
    pub document_prefix: Option<String>,
    pub query_prefix: Option<String>,
}

pub const DEFAULT_GATEWAY_MODEL: &str = "local-nomic-embed-text-latest";
pub const DEFAULT_MODEL2VEC_MODEL: &str = "minishlab/potion-code-16M-v2";

/// Builds the configured embedder; `Ok(None)` for `Backend::None`.
pub fn from_config(cfg: &EmbedConfig) -> Result<Option<Box<dyn Embedder>>> {
    Ok(match cfg.backend {
        Backend::None => None,
        Backend::Gateway => Some(Box::new(GatewayEmbedder::new(cfg))),
        Backend::Model2vec => Some(model2vec_embedder(cfg)?),
    })
}

#[cfg(feature = "model2vec")]
fn model2vec_embedder(cfg: &EmbedConfig) -> Result<Box<dyn Embedder>> {
    Ok(Box::new(Model2VecEmbedder::load(
        cfg.model.as_deref().unwrap_or(DEFAULT_MODEL2VEC_MODEL),
    )?))
}

#[cfg(not(feature = "model2vec"))]
fn model2vec_embedder(_: &EmbedConfig) -> Result<Box<dyn Embedder>> {
    Err(EmbedError::Model("built without the `model2vec` feature".into()))
}

/// Longest text sent to a model, in bytes; longer chunks are truncated at a char boundary.
const MAX_INPUT_BYTES: usize = 8000;
const BATCH: usize = 32;

/// OpenAI-compatible embeddings endpoint (the Dendrite LiteLLM gateway by default).
pub struct GatewayEmbedder {
    agent: ureq::Agent,
    base: String,
    url: String,
    key: Option<String>,
    model: String,
    document_prefix: String,
    query_prefix: String,
}

impl GatewayEmbedder {
    pub fn new(cfg: &EmbedConfig) -> Self {
        let base = cfg
            .base_url
            .clone()
            .or_else(|| std::env::var("LITELLM_PROXY_URL").ok())
            .unwrap_or_else(|| "http://localhost:4000".into());
        let key = cfg.api_key.clone().or_else(|| {
            ["LITELLM_API_KEY", "LITELLM_MASTER_KEY", "OPENAI_API_KEY"]
                .iter()
                .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        });
        let model = cfg.model.clone().unwrap_or_else(|| DEFAULT_GATEWAY_MODEL.into());
        let nomic = model.contains("nomic");
        let prefix = |set: &Option<String>, nomic_default: &str| {
            set.clone().unwrap_or_else(|| {
                if nomic {
                    nomic_default.into()
                } else {
                    String::new()
                }
            })
        };
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(120)))
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            agent,
            url: format!("{}/v1/embeddings", base.trim_end_matches('/')),
            base: base.trim_end_matches('/').to_string(),
            key,
            document_prefix: prefix(&cfg.document_prefix, "search_document: "),
            query_prefix: prefix(&cfg.query_prefix, "search_query: "),
            model,
        }
    }

    /// Gateway base URL without a trailing slash.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub fn api_key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    fn request(&self, inputs: Vec<String>) -> Result<Vec<Vec<f32>>> {
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
        let n = inputs.len();
        let mut req = self.agent.post(&self.url);
        if let Some(k) = &self.key {
            req = req.header("Authorization", &format!("Bearer {k}"));
        }
        let mut resp = req
            .send_json(Req {
                model: &self.model,
                input: inputs,
            })
            .map_err(|e| EmbedError::Http(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(EmbedError::Http(format!(
                "{status}: {}",
                body.chars().take(300).collect::<String>()
            )));
        }
        let mut body: Resp = resp
            .body_mut()
            .read_json()
            .map_err(|e| EmbedError::Response(e.to_string()))?;
        if body.data.len() != n {
            return Err(EmbedError::Response(format!(
                "asked for {n} vectors, got {}",
                body.data.len()
            )));
        }
        body.data.sort_by_key(|i| i.index);
        Ok(body.data.into_iter().map(|i| normalize(i.embedding)).collect())
    }
}

impl Embedder for GatewayEmbedder {
    fn model_id(&self) -> &str {
        &self.model
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(BATCH) {
            let inputs = batch
                .iter()
                .map(|t| format!("{}{}", self.document_prefix, truncate(t)))
                .collect();
            out.extend(self.request(inputs)?);
        }
        Ok(out)
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let mut v = self.request(vec![format!("{}{}", self.query_prefix, truncate(text))])?;
        Ok(v.remove(0))
    }
}

/// In-process static embeddings: a token-embedding lookup plus mean pooling. No network
/// after the model files are cached (`~/.cache/huggingface`, or a local folder path).
#[cfg(feature = "model2vec")]
pub struct Model2VecEmbedder {
    model: model2vec_rs::model::StaticModel,
    id: String,
}

#[cfg(feature = "model2vec")]
impl Model2VecEmbedder {
    pub fn load(repo_or_path: &str) -> Result<Self> {
        let model = model2vec_rs::model::StaticModel::from_pretrained(repo_or_path, None, Some(true), None)
            .map_err(|e| EmbedError::Model(e.to_string()))?;
        Ok(Self {
            model,
            id: format!("model2vec:{repo_or_path}"),
        })
    }
}

#[cfg(feature = "model2vec")]
impl Embedder for Model2VecEmbedder {
    fn model_id(&self) -> &str {
        &self.id
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let inputs: Vec<String> = texts.iter().map(|t| truncate(t).to_string()).collect();
        Ok(self.model.encode(&inputs).into_iter().map(normalize).collect())
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        Ok(normalize(self.model.encode_single(text)))
    }
}

fn truncate(s: &str) -> &str {
    if s.len() <= MAX_INPUT_BYTES {
        return s;
    }
    let mut end = MAX_INPUT_BYTES;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Scales `v` to unit length (zero vectors are returned unchanged).
pub fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_and_truncates() {
        let v = normalize(vec![3.0, 4.0]);
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
        assert_eq!(normalize(vec![0.0, 0.0]), vec![0.0, 0.0]);
        let long = "é".repeat(MAX_INPUT_BYTES);
        assert!(truncate(&long).len() <= MAX_INPUT_BYTES);
    }

    #[test]
    fn nomic_models_get_task_prefixes() {
        let e = GatewayEmbedder::new(&EmbedConfig::default());
        assert_eq!(e.query_prefix, "search_query: ");
        let e = GatewayEmbedder::new(&EmbedConfig {
            model: Some("bge-m3".into()),
            ..Default::default()
        });
        assert_eq!(e.document_prefix, "");
    }

    /// Runs against a live gateway: `DENDRITE_RAG_LIVE_GATEWAY=1 cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_gateway_embeds() {
        if std::env::var("DENDRITE_RAG_LIVE_GATEWAY").is_err() {
            return;
        }
        let e = GatewayEmbedder::new(&EmbedConfig::default());
        let docs = e
            .embed_documents(&["func Locate(name string) string".into(), "fn main() {}".into()])
            .unwrap();
        assert_eq!(docs.len(), 2);
        let q = e.embed_query("find the binary").unwrap();
        assert_eq!(q.len(), docs[0].len());
    }
}
