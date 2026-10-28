//! Embedding backends (`docs/specs/retrieval.md` § Dense index).
//!
//! The project owner picks the backend and the model (in the program's settings); nothing
//! here prefers one over another.
//!
//! | backend | where the model runs | wire / runtime |
//! |---|---|---|
//! | `auto` (default) | the first available of: candle on a GPU, a TEI server, Ollama, the LiteLLM gateway, candle on the CPU, model2vec | see [`resolve_auto`] |
//! | `openai` | any OpenAI-compatible server: the LiteLLM gateway, vLLM, LM Studio, `oxillama serve` | `POST /v1/embeddings` |
//! | `ollama` | an Ollama server | `POST /api/embed` |
//! | `hf` | Hugging Face Inference (serverless or a dedicated endpoint) or a TEI server | `POST …/feature-extraction`, TEI `POST /embed` |
//! | `candle` | in process (feature `candle`) | safetensors BERT / NomicBERT / JinaBERT |
//! | `oxillama` | in process (feature `oxillama`), CPU only for embeddings | GGUF decoder embedders, e.g. Qwen3-Embedding |
//! | `model2vec` | in process (feature `model2vec`) | static token embeddings |
//! | `none` | — | lexical and symbol retrieval only |

use serde::{Deserialize, Serialize};

pub mod hf;
mod http;
pub mod info;
pub mod ollama;
pub mod openai;

pub use info::{BackendInfo, backends};

#[cfg(feature = "candle")]
pub mod candle;
#[cfg(feature = "candle")]
pub mod jina_code;
#[cfg(feature = "model2vec")]
pub mod model2vec;
#[cfg(feature = "oxillama")]
pub mod oxillama;
#[cfg(feature = "candle")]
mod qwen3;
#[cfg(feature = "candle")]
pub(crate) mod tokenize;

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
    /// The first available GPU-capable backend; see [`resolve_auto`].
    #[default]
    Auto,
    /// OpenAI-compatible `/v1/embeddings`; by default the LiteLLM gateway.
    #[serde(alias = "gateway", alias = "litellm")]
    Openai,
    /// Ollama's native `/api/embed`.
    Ollama,
    /// Hugging Face Inference or a Text Embeddings Inference server.
    #[serde(alias = "huggingface", alias = "tei")]
    Hf,
    /// In-process safetensors encoders (feature `candle`).
    Candle,
    /// In-process GGUF embedders (feature `oxillama`). oxillama-runtime 0.1.4 runs
    /// multi-token prefill, and therefore every embedding, on the CPU.
    Oxillama,
    /// In-process static embeddings (feature `model2vec`).
    Model2vec,
    /// Lexical and symbol retrieval only.
    #[serde(alias = "off")]
    None,
}

impl Backend {
    pub const ALL: [Backend; 8] = [
        Backend::Auto,
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
            Backend::Auto => "auto",
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
            Backend::Auto => None,
            Backend::Candle => Some("jinaai/jina-embeddings-v2-base-code"),
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
            Backend::Auto => {
                "first available: candle on a GPU, a TEI server (HF_TEI_ENDPOINT), Ollama, candle on the CPU"
            }
            Backend::Openai => {
                "OpenAI-compatible /v1/embeddings: LiteLLM gateway, vLLM, LM Studio, oxillama serve"
            }
            Backend::Ollama => "Ollama server, /api/embed",
            Backend::Hf => "Hugging Face Inference (serverless or dedicated endpoint) or a TEI server",
            Backend::Candle => {
                "in process: safetensors BERT, NomicBERT or JinaBERT encoders (CPU, Metal, Accelerate)"
            }
            Backend::Oxillama => {
                "in process: GGUF decoder embedders such as Qwen3-Embedding (pure Rust, CPU only: no GPU prefill)"
            }
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
    /// Target vector dimensions (Matryoshka Representation Learning).
    pub dimensions: Option<usize>,
    /// Token budget capping an indexing run.
    pub budget_tokens: Option<usize>,
}

impl EmbedConfig {
    pub fn model(&self) -> String {
        self.model
            .clone()
            .or_else(|| self.backend.default_model().map(str::to_string))
            .unwrap_or_default()
    }

    /// The model id identifying stored vectors; includes `@dims` when `dimensions` is set.
    pub fn model_id(&self) -> String {
        let m = self.model();
        match self.dimensions {
            Some(d) => format!("{m}@{d}"),
            None => m,
        }
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

pub(crate) mod auto;
pub use auto::{gateway_reachable, ollama_reachable, resolve_auto};

/// Builds the configured embedder; `Ok(None)` for `Backend::None`. `auto` is resolved
/// first, and the choice is reported on stderr.
pub fn from_config(cfg: &EmbedConfig) -> Result<Option<Box<dyn Embedder>>> {
    if cfg.backend == Backend::Auto {
        let (resolved, reason) = resolve_auto(cfg, ollama_reachable, gateway_reachable);
        eprintln!(
            "loom-rag: embedder auto -> {} ({reason})",
            resolved.backend.as_str()
        );
        return from_config(&resolved);
    }
    if !cfg.backend.compiled() {
        return Err(EmbedError::Model(format!(
            "this build was made without the `{0}` feature (rebuild with `--features {0}`)",
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

/// Loaded embedders by configuration; `None` is a configuration without dense retrieval.
type LoadedEmbedders =
    std::sync::Mutex<std::collections::HashMap<String, Option<std::sync::Arc<dyn Embedder>>>>;

/// Embedders already loaded in this process, by configuration.
static LOADED: std::sync::OnceLock<LoadedEmbedders> = std::sync::OnceLock::new();

/// A loaded embedder shared by every engine the process opens with the same configuration.
struct Shared(std::sync::Arc<dyn Embedder>);

impl Embedder for Shared {
    fn model_id(&self) -> &str {
        self.0.model_id()
    }
    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.0.embed_documents(texts)
    }
    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.0.embed_query(text)
    }
}

/// [`from_config`], loading each configuration once per process. A server that closes idle
/// projects and reopens them later keeps the model it already loaded instead of reading the
/// weights and starting the GPU again for every reopen. Failures are not remembered.
pub fn shared_from_config(cfg: &EmbedConfig) -> Result<Option<Box<dyn Embedder>>> {
    shared_with(cfg, from_config)
}

fn shared_with(
    cfg: &EmbedConfig,
    load: impl FnOnce(&EmbedConfig) -> Result<Option<Box<dyn Embedder>>>,
) -> Result<Option<Box<dyn Embedder>>> {
    let key = format!("{cfg:?}");
    let mut loaded = LOADED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !loaded.contains_key(&key) {
        let embedder = load(cfg)?.map(std::sync::Arc::<dyn Embedder>::from);
        loaded.insert(key.clone(), embedder);
    }
    Ok(loaded[&key]
        .clone()
        .map(|e| Box::new(Shared(e)) as Box<dyn Embedder>))
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

#[cfg(test)]
mod tests;

#[cfg(test)]
mod shared_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixed;
    impl Embedder for Fixed {
        fn model_id(&self) -> &str {
            "fixed"
        }
        fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![1.0]).collect())
        }
        fn embed_query(&self, _: &str) -> Result<Vec<f32>> {
            Ok(vec![1.0])
        }
    }

    #[test]
    fn each_configuration_is_loaded_once_and_failures_are_retried() {
        let loads = AtomicUsize::new(0);
        let cfg = EmbedConfig {
            model: Some("shared-test-model-a".into()),
            ..EmbedConfig::default()
        };
        let load = |_: &EmbedConfig| {
            loads.fetch_add(1, Ordering::SeqCst);
            Ok(Some(Box::new(Fixed) as Box<dyn Embedder>))
        };
        let first = shared_with(&cfg, load).unwrap().unwrap();
        let second = shared_with(&cfg, load).unwrap().unwrap();
        assert_eq!(loads.load(Ordering::SeqCst), 1, "the model was loaded twice");
        assert_eq!(first.model_id(), second.model_id());

        let mut other = cfg.clone();
        other.model = Some("shared-test-model-b".into());
        shared_with(&other, load).unwrap();
        assert_eq!(
            loads.load(Ordering::SeqCst),
            2,
            "a different configuration must load its own model"
        );

        // A failed load is not cached: the next call tries again.
        let mut broken = cfg.clone();
        broken.model = Some("shared-test-model-broken".into());
        let failing = |_: &EmbedConfig| -> Result<Option<Box<dyn Embedder>>> {
            Err(EmbedError::Model("no weights".into()))
        };
        assert!(shared_with(&broken, failing).is_err());
        assert!(shared_with(&broken, load).unwrap().is_some());
    }
}
