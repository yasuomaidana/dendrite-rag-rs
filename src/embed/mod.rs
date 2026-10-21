//! Embedding backends (`docs/specs/retrieval.md` § Dense index).
//!
//! The project owner picks the backend and the model (`.dendrite/rag.toml` or the
//! environment, see [`crate::settings`]); nothing here prefers one over another.
//!
//! | backend | where the model runs | wire / runtime |
//! |---|---|---|
//! | `openai` | any OpenAI-compatible server: the LiteLLM gateway, vLLM, LM Studio, `oxillama serve` | `POST /v1/embeddings` |
//! | `ollama` | an Ollama server | `POST /api/embed` |
//! | `hf` | Hugging Face Inference (serverless or a dedicated endpoint) or a TEI server | `POST …/feature-extraction`, TEI `POST /embed` |
//! | `candle` | in process (feature `candle`) | safetensors BERT / NomicBERT / JinaBERT |
//! | `oxillama` | in process (feature `oxillama`) | GGUF decoder embedders, e.g. Qwen3-Embedding |
//! | `model2vec` | in process (feature `model2vec`) | static token embeddings |
//! | `none` | — | lexical and symbol retrieval only |

use serde::{Deserialize, Serialize};

pub mod hf;
mod http;
pub mod ollama;
pub mod openai;

#[cfg(feature = "candle")]
pub mod candle;
#[cfg(feature = "candle")]
pub mod jina_code;
#[cfg(feature = "model2vec")]
pub mod model2vec;
#[cfg(feature = "oxillama")]
pub mod oxillama;

pub use openai::OpenAiEmbedder;

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
    /// OpenAI-compatible `/v1/embeddings`; by default the LiteLLM gateway.
    #[default]
    #[serde(alias = "gateway", alias = "litellm")]
    Openai,
    /// Ollama's native `/api/embed`.
    Ollama,
    /// Hugging Face Inference or a Text Embeddings Inference server.
    #[serde(alias = "huggingface", alias = "tei")]
    Hf,
    /// In-process safetensors encoders (feature `candle`).
    Candle,
    /// In-process GGUF embedders (feature `oxillama`).
    Oxillama,
    /// In-process static embeddings (feature `model2vec`).
    Model2vec,
    /// Lexical and symbol retrieval only.
    #[serde(alias = "off")]
    None,
}

impl Backend {
    pub const ALL: [Backend; 7] = [
        Backend::Openai,
        Backend::Ollama,
        Backend::Hf,
        Backend::Candle,
        Backend::Oxillama,
        Backend::Model2vec,
        Backend::None,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Openai => "openai",
            Backend::Ollama => "ollama",
            Backend::Hf => "hf",
            Backend::Candle => "candle",
            Backend::Oxillama => "oxillama",
            Backend::Model2vec => "model2vec",
            Backend::None => "none",
        }
    }

    /// Parses a backend name (aliases included).
    pub fn parse(s: &str) -> Option<Backend> {
        serde_json::from_value(serde_json::Value::String(s.trim().to_ascii_lowercase())).ok()
    }

    /// Whether this build can construct the backend.
    pub fn compiled(self) -> bool {
        // In-process backends need their cargo feature; the others are always built.
        let features = [
            (Backend::Candle, cfg!(feature = "candle")),
            (Backend::Oxillama, cfg!(feature = "oxillama")),
            (Backend::Model2vec, cfg!(feature = "model2vec")),
        ];
        features
            .iter()
            .find(|(b, _)| *b == self)
            .is_none_or(|&(_, on)| on)
    }

    /// The model used when none is configured.
    pub fn default_model(self) -> Option<&'static str> {
        match self {
            Backend::Openai => Some("local-nomic-embed-text-latest"),
            Backend::Ollama => Some("nomic-embed-text"),
            Backend::Hf => Some("BAAI/bge-base-en-v1.5"),
            Backend::Candle => Some("nomic-ai/nomic-embed-text-v1.5"),
            Backend::Oxillama => Some("Qwen/Qwen3-Embedding-0.6B-GGUF"),
            Backend::Model2vec => Some("minishlab/potion-code-16M-v2"),
            Backend::None => None,
        }
    }

    /// The endpoint used when none is configured (HTTP backends only).
    pub fn default_url(self) -> Option<String> {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        match self {
            Backend::Openai => {
                Some(env("LITELLM_PROXY_URL").unwrap_or_else(|| "http://localhost:4000".into()))
            }
            Backend::Ollama => Some(
                env("OLLAMA_HOST")
                    .map(|h| {
                        if h.contains("://") {
                            h
                        } else {
                            format!("http://{h}")
                        }
                    })
                    .unwrap_or_else(|| "http://localhost:11434".into()),
            ),
            _ => None,
        }
    }

    /// Environment variables holding the bearer key, in order of preference.
    fn key_env(self) -> &'static [&'static str] {
        match self {
            Backend::Openai => &["LITELLM_API_KEY", "LITELLM_MASTER_KEY", "OPENAI_API_KEY"],
            Backend::Hf => &["HF_TOKEN", "HUGGING_FACE_HUB_TOKEN"],
            _ => &[],
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Backend::Openai => {
                "OpenAI-compatible /v1/embeddings: LiteLLM gateway, vLLM, LM Studio, oxillama serve"
            }
            Backend::Ollama => "Ollama server, /api/embed",
            Backend::Hf => "Hugging Face Inference (serverless or dedicated endpoint) or a TEI server",
            Backend::Candle => {
                "in process: safetensors BERT, NomicBERT or JinaBERT encoders (CPU, Metal, Accelerate)"
            }
            Backend::Oxillama => "in process: GGUF decoder embedders such as Qwen3-Embedding (pure Rust)",
            Backend::Model2vec => "in process: static token embeddings, fastest, lowest quality",
            Backend::None => "no dense retrieval: lexical and symbol rankings only",
        }
    }
}

/// How per-token hidden states become one vector (in-process and raw HF backends).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pooling {
    Mean,
    Cls,
    Last,
}

/// Embedding settings. Unset fields take model or backend defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EmbedConfig {
    pub backend: Backend,
    /// Model name, hub repository or local path; see [`Backend::default_model`].
    pub model: Option<String>,
    /// Server URL for HTTP backends; see [`Backend::default_url`].
    pub url: Option<String>,
    /// Bearer key. Prefer `api_key_env`, which keeps secrets out of files.
    #[serde(skip_serializing)]
    pub api_key: Option<String>,
    /// Environment variable that holds the bearer key.
    pub api_key_env: Option<String>,
    /// Text prepended to documents and queries for asymmetric models; known model
    /// families get theirs automatically (see [`model_prefixes`]).
    pub document_prefix: Option<String>,
    pub query_prefix: Option<String>,
    /// Pooling for in-process and token-level outputs; see [`model_pooling`].
    pub pooling: Option<Pooling>,
    /// Texts per request or forward pass (default 32, in process 16).
    pub batch: Option<usize>,
}

impl EmbedConfig {
    pub fn model(&self) -> String {
        self.model
            .clone()
            .or_else(|| self.backend.default_model().map(str::to_string))
            .unwrap_or_default()
    }

    pub fn url(&self) -> Option<String> {
        self.url
            .clone()
            .map(|u| u.trim_end_matches('/').to_string())
            .or_else(|| self.backend.default_url())
    }

    pub fn api_key(&self) -> Option<String> {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        self.api_key
            .clone()
            .or_else(|| self.api_key_env.as_deref().and_then(env))
            .or_else(|| self.backend.key_env().iter().find_map(|k| env(k)))
            .or_else(|| (self.backend == Backend::Hf).then(hf_token_file).flatten())
    }

    /// `(document, query)` prefixes: configured, else the model family's.
    pub fn prefixes(&self) -> (String, String) {
        let (d, q) = model_prefixes(&self.model());
        (
            self.document_prefix.clone().unwrap_or_else(|| d.into()),
            self.query_prefix.clone().unwrap_or_else(|| q.into()),
        )
    }

    pub fn pooling(&self) -> Pooling {
        self.pooling.unwrap_or_else(|| model_pooling(&self.model()))
    }

    pub fn batch(&self, default: usize) -> usize {
        self.batch.filter(|&b| b > 0).unwrap_or(default)
    }
}

/// The token `huggingface-cli login` stores: `$HF_HOME/token`, else
/// `~/.cache/huggingface/token`.
fn hf_token_file() -> Option<String> {
    let home = std::env::var_os("HF_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".cache/huggingface")))?;
    let token = std::fs::read_to_string(home.join("token")).ok()?;
    Some(token.trim().to_string()).filter(|t| !t.is_empty())
}

/// Task prefixes the model was trained with, by model family.
pub fn model_prefixes(model: &str) -> (&'static str, &'static str) {
    let m = model.to_ascii_lowercase();
    if m.contains("nomic-embed") {
        ("search_document: ", "search_query: ")
    } else if m.contains("qwen3-embedding") {
        (
            "",
            "Instruct: Given a code search query, retrieve relevant code and documentation\nQuery: ",
        )
    } else if m.contains("e5-") {
        ("passage: ", "query: ")
    } else if (m.contains("bge-") && m.contains("-en"))
        || m.contains("mxbai-embed")
        || m.contains("arctic-embed")
    {
        ("", "Represent this sentence for searching relevant passages: ")
    } else {
        ("", "")
    }
}

/// Pooling the model was trained with, by model family.
pub fn model_pooling(model: &str) -> Pooling {
    let m = model.to_ascii_lowercase();
    if m.contains("qwen3-embedding") || m.contains("gte-qwen") {
        Pooling::Last
    } else if m.contains("bge-") || m.contains("arctic-embed") || m.contains("mxbai-embed") {
        Pooling::Cls
    } else {
        Pooling::Mean
    }
}

/// Builds the configured embedder; `Ok(None)` for `Backend::None`.
pub fn from_config(cfg: &EmbedConfig) -> Result<Option<Box<dyn Embedder>>> {
    if !cfg.backend.compiled() {
        return Err(EmbedError::Model(format!(
            "this dendrite-rag was built without the `{0}` feature (cargo install --features {0})",
            cfg.backend.as_str()
        )));
    }
    Ok(match cfg.backend {
        Backend::None => None,
        Backend::Openai => Some(Box::new(OpenAiEmbedder::new(cfg))),
        Backend::Ollama => Some(Box::new(ollama::OllamaEmbedder::new(cfg))),
        Backend::Hf => Some(Box::new(hf::HfEmbedder::new(cfg)?)),
        #[cfg(feature = "candle")]
        Backend::Candle => Some(Box::new(candle::CandleEmbedder::load(cfg)?)),
        #[cfg(feature = "oxillama")]
        Backend::Oxillama => Some(Box::new(oxillama::OxillamaEmbedder::load(cfg)?)),
        #[cfg(feature = "model2vec")]
        Backend::Model2vec => Some(Box::new(model2vec::Model2VecEmbedder::load(cfg)?)),
        #[allow(unreachable_patterns)]
        _ => unreachable!("compiled() rejected the backend"),
    })
}

/// Longest text sent to a model, in bytes; longer chunks are truncated at a char boundary.
pub(crate) const MAX_INPUT_BYTES: usize = 8000;

pub(crate) fn truncate(s: &str) -> &str {
    truncate_to(s, MAX_INPUT_BYTES)
}

pub(crate) fn truncate_to(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Prefixed and truncated inputs, ready for a model.
pub(crate) fn prepare(prefix: &str, texts: &[String]) -> Vec<String> {
    texts.iter().map(|t| format!("{prefix}{}", truncate(t))).collect()
}

/// Scales `v` to unit length (zero vectors are returned unchanged).
pub fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
    v
}

/// Pools a `[tokens][dim]` matrix.
pub fn pool(tokens: &[Vec<f32>], pooling: Pooling) -> Vec<f32> {
    match (pooling, tokens) {
        (_, []) => Vec::new(),
        (Pooling::Cls, [first, ..]) => first.clone(),
        (Pooling::Last, [.., last]) => last.clone(),
        (Pooling::Mean, [first, ..]) => {
            let mut sum = vec![0.0; first.len()];
            for t in tokens {
                sum.iter_mut().zip(t).for_each(|(s, x)| *s += x);
            }
            sum.iter_mut().for_each(|s| *s /= tokens.len() as f32);
            sum
        }
    }
}

/// What `dendrite-rag embedders` reports for one backend.
#[derive(Debug, Clone, Serialize)]
pub struct BackendInfo {
    pub backend: &'static str,
    pub compiled: bool,
    pub default_model: Option<&'static str>,
    pub default_url: Option<String>,
    pub description: &'static str,
}

pub fn backends() -> Vec<BackendInfo> {
    Backend::ALL
        .iter()
        .map(|&b| BackendInfo {
            backend: b.as_str(),
            compiled: b.compiled(),
            default_model: b.default_model(),
            default_url: b.default_url(),
            description: b.describe(),
        })
        .collect()
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
    fn backend_names_and_aliases_parse() {
        assert_eq!(Backend::parse("gateway"), Some(Backend::Openai));
        assert_eq!(Backend::parse(" OpenAI "), Some(Backend::Openai));
        assert_eq!(Backend::parse("tei"), Some(Backend::Hf));
        assert_eq!(Backend::parse("huggingface"), Some(Backend::Hf));
        assert_eq!(Backend::parse("off"), Some(Backend::None));
        assert_eq!(Backend::parse("oxillama"), Some(Backend::Oxillama));
        assert_eq!(Backend::parse("word2vec"), None);
        for b in Backend::ALL {
            assert_eq!(Backend::parse(b.as_str()), Some(b));
        }
    }

    #[test]
    fn model_families_get_their_prefixes_and_pooling() {
        let cfg = EmbedConfig::default();
        assert_eq!(cfg.prefixes().1, "search_query: ");
        let cfg = EmbedConfig {
            backend: Backend::Ollama,
            model: Some("bge-m3".into()),
            ..Default::default()
        };
        assert_eq!(cfg.prefixes(), (String::new(), String::new()));
        let cfg = EmbedConfig {
            model: Some("Qwen/Qwen3-Embedding-0.6B-GGUF".into()),
            query_prefix: Some("q: ".into()),
            ..Default::default()
        };
        assert_eq!(cfg.prefixes(), (String::new(), "q: ".into()));
        assert_eq!(cfg.pooling(), Pooling::Last);
        assert_eq!(model_pooling("BAAI/bge-base-en-v1.5"), Pooling::Cls);
        assert_eq!(model_pooling("nomic-ai/nomic-embed-text-v1.5"), Pooling::Mean);
    }

    #[test]
    fn pools_token_states() {
        let t = vec![vec![1.0, 0.0], vec![3.0, 2.0]];
        assert_eq!(pool(&t, Pooling::Mean), vec![2.0, 1.0]);
        assert_eq!(pool(&t, Pooling::Cls), vec![1.0, 0.0]);
        assert_eq!(pool(&t, Pooling::Last), vec![3.0, 2.0]);
        assert!(pool(&[], Pooling::Mean).is_empty());
    }

    #[test]
    fn missing_features_are_reported_not_substituted() {
        for b in Backend::ALL.into_iter().filter(|b| !b.compiled()) {
            let err = from_config(&EmbedConfig {
                backend: b,
                ..Default::default()
            })
            .err()
            .expect("an uncompiled backend must fail");
            assert!(err.to_string().contains(b.as_str()), "{err}");
        }
    }
}
