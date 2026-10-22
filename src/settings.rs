//! Per-project settings: `<root>/.dendrite/rag.toml`, overridden by the environment.
//!
//! ```toml
//! [embed]
//! backend = "ollama"            # auto (default) | openai | ollama | hf | candle | oxillama | model2vec | none
//! model = "nomic-embed-text"
//! url = "http://localhost:11434"
//! # api_key_env = "HF_TOKEN"   document_prefix / query_prefix / pooling / batch
//!
//! [rerank]
//! backend = "gateway"            # gateway (default) | candle | none
//! model = "bge-reranker-v2-m3"
//!
//! [index]
//! max_file_bytes = 1048576
//! query_cache = 256
//!
//! [fusion]                      # hybrid weights; unset keys keep the embedder's defaults
//! lexical = 1.0
//! dense = 1.0
//! symbol = 1.5
//! community = 0.0               # boost for code in the best-matching communities
//!
//! [storage]
//! mode = "local"                # local (default) | remote (shared PostgreSQL) | edge
//! # repo = "github.com/acme/app" # name in the shared tier; default: the origin URL
//!
//! [graph]
//! resolution = 1.0              # Leiden resolution: higher gives more, smaller communities
//!
//! [graph.llm]
//! enabled = false
//! model = "gpt-4o-mini"
//! summaries = false
//! concepts = false
//! ```
//!
//! Environment overrides: `DENDRITE_RAG_EMBED_BACKEND`, `_MODEL`, `_URL`, `_API_KEY_ENV`,
//! `_DOCUMENT_PREFIX`, `_QUERY_PREFIX`, `_POOLING`, `_BATCH` (all `DENDRITE_RAG_EMBED_*`),
//! `DENDRITE_RAG_RERANK_BACKEND`, `_MODEL`, `_URL`, `_API_KEY_ENV`, `_BATCH`, `DENDRITE_RAG_MAX_FILE_BYTES`,
//! `DENDRITE_RAG_QUERY_CACHE`, `DENDRITE_RAG_GRAPH_RESOLUTION`, `DENDRITE_RAG_STORAGE_MODE`, `DENDRITE_RAG_STORAGE_REPO`,
//! `DENDRITE_RAG_PG_URL` (the shared tier's URL: environment only, never the file),
//! `DENDRITE_RAG_FUSION_LEXICAL`, `_DENSE`, `_SYMBOL`, `_COMMUNITY`, `DENDRITE_RAG_GRAPH_LLM_ENABLED`,
//! `_MODEL`, `_SUMMARIES`, `_CONCEPTS` (all `DENDRITE_RAG_GRAPH_LLM_*`), and `DENDRITE_RAG_CONFIG` to read another file. A backend chosen in the environment
//! discards the file's model, URL, prefixes and pooling, which belong to the file's backend.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use crate::embed::{Backend, EmbedConfig, Pooling};
use crate::engine::Config;
use crate::rerank::{RerankBackend, RerankConfig};

/// Project-relative location of the settings file.
pub const FILE: &str = ".dendrite/rag.toml";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub embed: EmbedConfig,
    pub rerank: RerankConfig,
    pub index: IndexSettings,
    pub graph: GraphSettings,
    pub fusion: crate::retrieve::FusionConfig,
    pub storage: StorageSettings,
    /// `DENDRITE_RAG_PG_URL`; never read from or written to the file.
    #[serde(skip)]
    pub pg_url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageSettings {
    pub mode: Option<crate::store::StorageMode>,
    pub repo: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GraphSettings {
    pub resolution: Option<f64>,
    pub llm: GraphLlmSettings,
}

/// Optional LLM layer for graph community summaries and concepts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GraphLlmSettings {
    pub enabled: bool,
    pub model: Option<String>,
    pub summaries: bool,
    pub concepts: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VectorIndexChoice {
    #[default]
    Auto,
    Flat,
    Hnsw,
}

impl VectorIndexChoice {
    pub const ALL: [VectorIndexChoice; 3] = [
        VectorIndexChoice::Auto,
        VectorIndexChoice::Flat,
        VectorIndexChoice::Hnsw,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Flat => "flat",
            Self::Hnsw => "hnsw",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "flat" => Some(Self::Flat),
            "hnsw" => Some(Self::Hnsw),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IndexSettings {
    pub max_file_bytes: Option<u64>,
    pub query_cache: Option<usize>,
    pub vector_index: Option<VectorIndexChoice>,
    pub hnsw_ef_search: Option<usize>,
}

impl Settings {
    /// The settings file for `root`: `DENDRITE_RAG_CONFIG`, else `<root>/.dendrite/rag.toml`.
    pub fn path(root: &Path) -> PathBuf {
        match std::env::var_os("DENDRITE_RAG_CONFIG").filter(|v| !v.is_empty()) {
            Some(p) => PathBuf::from(p),
            None => root.join(FILE),
        }
    }

    /// Reads the settings file of `root` (defaults when there is none) and applies the
    /// environment.
    pub fn load(root: &Path) -> anyhow::Result<Self> {
        let path = Self::path(root);
        let mut s = match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).with_context(|| format!("{}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e).with_context(|| format!("{}", path.display())),
        };
        s.apply_env(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))?;
        Ok(s)
    }

    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let s: Self = toml::from_str(text)?;
        if s.index.vector_index == Some(VectorIndexChoice::Hnsw) && !cfg!(feature = "hnsw") {
            bail!("vector index 'hnsw' requires cargo feature 'hnsw'");
        }
        Ok(s)
    }

    fn apply_env(&mut self, var: impl Fn(&str) -> Option<String>) -> anyhow::Result<()> {
        let e = &mut self.embed;
        if let Some(b) = var("DENDRITE_RAG_EMBED_BACKEND") {
            let Some(backend) = Backend::parse(&b) else {
                let names: Vec<_> = Backend::ALL.iter().map(|b| b.as_str()).collect();
                bail!(
                    "DENDRITE_RAG_EMBED_BACKEND={b:?}: expected one of {}",
                    names.join(", ")
                );
            };
            if backend != e.backend {
                *e = EmbedConfig {
                    backend,
                    ..Default::default()
                };
            }
        }
        let set = |field: &mut Option<String>, key: &str| {
            if let Some(v) = var(key) {
                *field = Some(v);
            }
        };
        set(&mut e.model, "DENDRITE_RAG_EMBED_MODEL");
        set(&mut e.url, "DENDRITE_RAG_EMBED_URL");
        set(&mut e.api_key_env, "DENDRITE_RAG_EMBED_API_KEY_ENV");
        set(&mut e.document_prefix, "DENDRITE_RAG_EMBED_DOCUMENT_PREFIX");
        set(&mut e.query_prefix, "DENDRITE_RAG_EMBED_QUERY_PREFIX");
        if let Some(p) = var("DENDRITE_RAG_EMBED_POOLING") {
            e.pooling = Some(
                serde_json::from_value::<Pooling>(serde_json::Value::String(p.to_ascii_lowercase()))
                    .with_context(|| {
                        format!("DENDRITE_RAG_EMBED_POOLING={p:?}: expected mean, cls or last")
                    })?,
            );
        }
        if let Some(n) = var("DENDRITE_RAG_EMBED_BATCH") {
            e.batch = Some(
                n.parse()
                    .with_context(|| format!("DENDRITE_RAG_EMBED_BATCH={n:?}"))?,
            );
        }
        let r = &mut self.rerank;
        if let Some(b) = var("DENDRITE_RAG_RERANK_BACKEND") {
            let Some(backend) = RerankBackend::parse(&b) else {
                let names: Vec<_> = RerankBackend::ALL.iter().map(|b| b.as_str()).collect();
                bail!(
                    "DENDRITE_RAG_RERANK_BACKEND={b:?}: expected one of {}",
                    names.join(", ")
                );
            };
            if backend != r.backend {
                *r = RerankConfig {
                    backend,
                    ..Default::default()
                };
            }
        }
        set(&mut self.rerank.model, "DENDRITE_RAG_RERANK_MODEL");
        set(&mut self.rerank.url, "DENDRITE_RAG_RERANK_URL");
        set(&mut self.rerank.api_key_env, "DENDRITE_RAG_RERANK_API_KEY_ENV");
        if let Some(n) = var("DENDRITE_RAG_RERANK_BATCH") {
            self.rerank.batch = Some(
                n.parse()
                    .with_context(|| format!("DENDRITE_RAG_RERANK_BATCH={n:?}"))?,
            );
        }
        if let Some(n) = var("DENDRITE_RAG_MAX_FILE_BYTES") {
            self.index.max_file_bytes = Some(
                n.parse()
                    .with_context(|| format!("DENDRITE_RAG_MAX_FILE_BYTES={n:?}"))?,
            );
        }
        if let Some(n) = var("DENDRITE_RAG_QUERY_CACHE") {
            self.index.query_cache = Some(
                n.parse()
                    .with_context(|| format!("DENDRITE_RAG_QUERY_CACHE={n:?}"))?,
            );
        }
        for (key, field) in [
            ("DENDRITE_RAG_FUSION_LEXICAL", &mut self.fusion.lexical),
            ("DENDRITE_RAG_FUSION_DENSE", &mut self.fusion.dense),
            ("DENDRITE_RAG_FUSION_SYMBOL", &mut self.fusion.symbol),
            ("DENDRITE_RAG_FUSION_COMMUNITY", &mut self.fusion.community),
        ] {
            if let Some(v) = var(key) {
                *field = Some(v.parse().with_context(|| format!("{key}={v:?}"))?);
            }
        }
        let f = &self.fusion;
        for (name, w) in [
            ("lexical", f.lexical),
            ("dense", f.dense),
            ("symbol", f.symbol),
            ("community", f.community),
        ] {
            if let Some(w) = w
                && !(w >= 0.0 && w.is_finite())
            {
                bail!("fusion.{name} must be a non-negative number, got {w}");
            }
        }
        if let Some(m) = var("DENDRITE_RAG_STORAGE_MODE") {
            let Some(mode) = crate::store::StorageMode::parse(&m) else {
                bail!("DENDRITE_RAG_STORAGE_MODE={m:?}: expected local, remote or edge");
            };
            self.storage.mode = Some(mode);
        }
        set(&mut self.storage.repo, "DENDRITE_RAG_STORAGE_REPO");
        set(&mut self.pg_url, "DENDRITE_RAG_PG_URL");
        if let Some(r) = var("DENDRITE_RAG_GRAPH_RESOLUTION") {
            self.graph.resolution = Some(
                r.parse()
                    .with_context(|| format!("DENDRITE_RAG_GRAPH_RESOLUTION={r:?}"))?,
            );
        }
        let parse_bool = |v: &str, name: &str| -> anyhow::Result<bool> {
            match v.trim().to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" | "on" => Ok(true),
                "false" | "0" | "no" | "off" => Ok(false),
                _ => bail!("{name}={v:?}: expected true or false"),
            }
        };
        if let Some(v) = var("DENDRITE_RAG_GRAPH_LLM_ENABLED") {
            self.graph.llm.enabled = parse_bool(&v, "DENDRITE_RAG_GRAPH_LLM_ENABLED")?;
        }
        if let Some(v) = var("DENDRITE_RAG_GRAPH_LLM_MODEL") {
            self.graph.llm.model = Some(v);
        }
        if let Some(v) = var("DENDRITE_RAG_GRAPH_LLM_SUMMARIES") {
            self.graph.llm.summaries = parse_bool(&v, "DENDRITE_RAG_GRAPH_LLM_SUMMARIES")?;
        }
        if let Some(v) = var("DENDRITE_RAG_GRAPH_LLM_CONCEPTS") {
            self.graph.llm.concepts = parse_bool(&v, "DENDRITE_RAG_GRAPH_LLM_CONCEPTS")?;
        }
        if let Some(v) = var("DENDRITE_RAG_VECTOR_INDEX") {
            let Some(choice) = VectorIndexChoice::parse(&v) else {
                let names: Vec<_> = VectorIndexChoice::ALL.iter().map(|c| c.as_str()).collect();
                bail!(
                    "DENDRITE_RAG_VECTOR_INDEX={v:?}: expected one of {}",
                    names.join(", ")
                );
            };
            self.index.vector_index = Some(choice);
        }
        if let Some(n) = var("DENDRITE_RAG_HNSW_EF_SEARCH") {
            self.index.hnsw_ef_search = Some(
                n.parse()
                    .with_context(|| format!("DENDRITE_RAG_HNSW_EF_SEARCH={n:?}"))?,
            );
        }
        self.validate()?;
        Ok(())
    }

    /// Validates the settings consistency.
    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(r) = self.graph.resolution
            && !(r > 0.0 && r.is_finite())
        {
            bail!("graph.resolution must be a positive number, got {r}");
        }
        if self.index.vector_index == Some(VectorIndexChoice::Hnsw) && !cfg!(feature = "hnsw") {
            bail!("vector index 'hnsw' requires cargo feature 'hnsw'");
        }
        if self.graph.llm.enabled && self.graph.llm.model.as_deref().unwrap_or("").trim().is_empty() {
            bail!(
                "graph.llm is enabled but no model is configured (set graph.llm.model or DENDRITE_RAG_GRAPH_LLM_MODEL)"
            );
        }
        Ok(())
    }

    /// The engine configuration these settings describe.
    pub fn config(&self) -> Config {
        let mut cfg = Config {
            embed: self.embed.clone(),
            rerank: self.rerank.clone(),
            graph_llm: self.graph.llm.clone(),
            ..Config::default()
        };
        if let Some(n) = self.index.max_file_bytes {
            cfg.walk.max_file_bytes = n;
        }
        if let Some(n) = self.index.query_cache {
            cfg.query_cache = n;
        }
        if let Some(r) = self.graph.resolution {
            cfg.graph_resolution = r;
        }
        if let Some(vi) = self.index.vector_index {
            cfg.vector_index = vi;
        }
        if let Some(ef) = self.index.hnsw_ef_search {
            cfg.hnsw_ef_search = ef;
        }
        cfg.fusion = self.fusion;
        cfg.storage = crate::store::StorageConfig {
            mode: self.storage.mode.unwrap_or_default(),
            repo: self.storage.repo.clone(),
            pg_url: self.pg_url.clone(),
        };
        cfg
    }
}

impl Config {
    /// The configuration of the project at `root`: defaults, its `.dendrite/rag.toml`,
    /// then the environment.
    pub fn resolve(root: &Path) -> anyhow::Result<Self> {
        Ok(Settings::load(root)?.config())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| m.get(k).cloned()
    }

    const FILE_TEXT: &str = r#"
[embed]
backend = "ollama"
model = "jina/jina-embeddings-v2-base-code"
url = "http://gpu-box:11434"
pooling = "mean"

[rerank]
model = "bge-reranker-v2-m3"

[index]
max_file_bytes = 2048
"#;

    #[test]
    fn reads_the_project_file() {
        let s = Settings::parse(FILE_TEXT).unwrap();
        assert_eq!(s.embed.backend, Backend::Ollama);
        assert_eq!(s.embed.url(), Some("http://gpu-box:11434".into()));
        let cfg = s.config();
        assert_eq!(cfg.walk.max_file_bytes, 2048);
        assert_eq!(cfg.rerank.model.as_deref(), Some("bge-reranker-v2-m3"));
    }

    #[test]
    fn rejects_unknown_keys_and_backends() {
        assert!(Settings::parse("[embed]\nbakend = \"ollama\"\n").is_err());
        assert!(Settings::parse("[embed]\nbackend = \"word2vec\"\n").is_err());
        let mut s = Settings::default();
        assert!(
            s.apply_env(env(&[("DENDRITE_RAG_EMBED_BACKEND", "word2vec")]))
                .is_err()
        );
    }

    #[test]
    fn environment_overrides_the_file() {
        let mut s = Settings::parse(FILE_TEXT).unwrap();
        s.apply_env(env(&[("DENDRITE_RAG_EMBED_MODEL", "nomic-embed-text")]))
            .unwrap();
        assert_eq!(s.embed.backend, Backend::Ollama);
        assert_eq!(s.embed.model(), "nomic-embed-text");
        assert_eq!(s.embed.url(), Some("http://gpu-box:11434".into()));

        // Another backend: the file's ollama model and URL no longer apply.
        let mut s = Settings::parse(FILE_TEXT).unwrap();
        s.apply_env(env(&[
            ("DENDRITE_RAG_EMBED_BACKEND", "hf"),
            ("DENDRITE_RAG_EMBED_POOLING", "CLS"),
        ]))
        .unwrap();
        assert_eq!(s.embed.backend, Backend::Hf);
        assert_eq!(s.embed.model(), "BAAI/bge-base-en-v1.5");
        assert_eq!(s.embed.url, None);
        assert_eq!(s.embed.pooling, Some(Pooling::Cls));
    }

    #[test]
    fn missing_file_means_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let s = Settings::load(dir.path()).unwrap();
        if std::env::var("DENDRITE_RAG_EMBED_BACKEND").is_err() {
            assert_eq!(s.embed.backend, Backend::Auto);
        }
    }

    #[test]
    fn parses_query_cache_and_applies_env() {
        let toml_with_cache = r#"
[embed]
backend = "ollama"

[index]
query_cache = 512
"#;
        let mut s = Settings::parse(toml_with_cache).unwrap();
        assert_eq!(s.index.query_cache, Some(512));
        assert_eq!(s.config().query_cache, 512);

        // Environment overrides file.
        s.apply_env(env(&[("DENDRITE_RAG_QUERY_CACHE", "0")])).unwrap();
        assert_eq!(s.index.query_cache, Some(0));
        assert_eq!(s.config().query_cache, 0);

        // Env on default settings.
        let mut s2 = Settings::default();
        s2.apply_env(env(&[("DENDRITE_RAG_QUERY_CACHE", "1024")]))
            .unwrap();
        assert_eq!(s2.index.query_cache, Some(1024));
        assert_eq!(s2.config().query_cache, 1024);
    }

    #[test]
    fn parses_rerank_backend_and_applies_env() {
        let toml_with_rerank = r#"
[embed]
backend = "none"

[rerank]
backend = "candle"
model = "BAAI/bge-reranker-base"
"#;
        let mut s = Settings::parse(toml_with_rerank).unwrap();
        assert_eq!(s.rerank.backend, RerankBackend::Candle);
        assert_eq!(s.rerank.model.as_deref(), Some("BAAI/bge-reranker-base"));

        // Environment overrides file.
        s.apply_env(env(&[("DENDRITE_RAG_RERANK_BACKEND", "none")]))
            .unwrap();
        assert_eq!(s.rerank.backend, RerankBackend::None);
        // Changed backend drops file model
        assert_eq!(s.rerank.model, None);

        // Reject unknown backend
        let mut s2 = Settings::default();
        assert!(
            s2.apply_env(env(&[("DENDRITE_RAG_RERANK_BACKEND", "word2vec")]))
                .is_err()
        );
    }

    #[test]
    fn graph_resolution_from_file_and_env() {
        let mut s = Settings::parse("[graph]\nresolution = 0.5\n").unwrap();
        assert_eq!(s.config().graph_resolution, 0.5);
        s.apply_env(env(&[("DENDRITE_RAG_GRAPH_RESOLUTION", "2")]))
            .unwrap();
        assert_eq!(s.config().graph_resolution, 2.0);
        assert!(
            s.apply_env(env(&[("DENDRITE_RAG_GRAPH_RESOLUTION", "0")]))
                .is_err()
        );
    }

    #[test]
    fn graph_llm_settings_and_env_overrides() {
        let toml_text = r#"
[graph.llm]
enabled = true
model = "deepseek-chat"
summaries = true
concepts = false
"#;
        let mut s = Settings::parse(toml_text).unwrap();
        assert!(s.graph.llm.enabled);
        assert_eq!(s.graph.llm.model.as_deref(), Some("deepseek-chat"));
        assert!(s.graph.llm.summaries);
        assert!(!s.graph.llm.concepts);
        assert!(s.validate().is_ok());

        // Environment overrides
        s.apply_env(env(&[
            ("DENDRITE_RAG_GRAPH_LLM_ENABLED", "false"),
            ("DENDRITE_RAG_GRAPH_LLM_MODEL", "gpt-4o-mini"),
            ("DENDRITE_RAG_GRAPH_LLM_SUMMARIES", "0"),
            ("DENDRITE_RAG_GRAPH_LLM_CONCEPTS", "true"),
        ]))
        .unwrap();
        assert!(!s.graph.llm.enabled);
        assert_eq!(s.graph.llm.model.as_deref(), Some("gpt-4o-mini"));
        assert!(!s.graph.llm.summaries);
        assert!(s.graph.llm.concepts);

        // Enabled without model fails validation
        let mut s_no_model = Settings::parse("[graph.llm]\nenabled = true\n").unwrap();
        assert!(s_no_model.validate().is_err());
        assert!(
            s_no_model
                .apply_env(env(&[("DENDRITE_RAG_GRAPH_LLM_ENABLED", "true")]))
                .is_err()
        );

        // Unknown keys rejected
        assert!(Settings::parse("[graph.llm]\nunknown_key = true\n").is_err());
        assert!(Settings::parse("[graph]\nunknown_key = true\n").is_err());
    }

    #[test]
    fn parses_vector_index_and_ef_search_and_applies_env() {
        let toml_text = r#"
[index]
vector_index = "flat"
hnsw_ef_search = 128
"#;
        let mut s = Settings::parse(toml_text).unwrap();
        assert_eq!(s.index.vector_index, Some(VectorIndexChoice::Flat));
        assert_eq!(s.index.hnsw_ef_search, Some(128));
        let cfg = s.config();
        assert_eq!(cfg.vector_index, VectorIndexChoice::Flat);
        assert_eq!(cfg.hnsw_ef_search, 128);

        s.apply_env(env(&[
            ("DENDRITE_RAG_VECTOR_INDEX", "auto"),
            ("DENDRITE_RAG_HNSW_EF_SEARCH", "256"),
        ]))
        .unwrap();
        assert_eq!(s.index.vector_index, Some(VectorIndexChoice::Auto));
        assert_eq!(s.index.hnsw_ef_search, Some(256));
        assert_eq!(s.config().vector_index, VectorIndexChoice::Auto);
        assert_eq!(s.config().hnsw_ef_search, 256);

        // Unknown vector index
        let mut s2 = Settings::default();
        assert!(
            s2.apply_env(env(&[("DENDRITE_RAG_VECTOR_INDEX", "invalid")]))
                .is_err()
        );

        #[cfg(not(feature = "hnsw"))]
        {
            let mut s3 = Settings::default();
            assert!(
                s3.apply_env(env(&[("DENDRITE_RAG_VECTOR_INDEX", "hnsw")]))
                    .is_err()
            );
            assert!(Settings::parse("[index]\nvector_index = \"hnsw\"\n").is_err());
        }

        #[cfg(feature = "hnsw")]
        {
            let mut s3 = Settings::default();
            assert!(
                s3.apply_env(env(&[("DENDRITE_RAG_VECTOR_INDEX", "hnsw")]))
                    .is_ok()
            );
            assert!(Settings::parse("[index]\nvector_index = \"hnsw\"\n").is_ok());
        }
    }

    #[test]
    fn fusion_weights_from_file_and_env() {
        let mut s = Settings::parse("[fusion]\ndense = 2.0\n").unwrap();
        s.apply_env(env(&[("DENDRITE_RAG_FUSION_COMMUNITY", "0.25")]))
            .unwrap();
        let w = s
            .config()
            .fusion
            .resolve(crate::retrieve::FusionWeights::default());
        assert_eq!((w.lexical, w.dense, w.symbol, w.community), (1.0, 2.0, 1.5, 0.25));
        assert!(
            s.apply_env(env(&[("DENDRITE_RAG_FUSION_LEXICAL", "-1")]))
                .is_err()
        );
        assert!(Settings::parse("[fusion]\nbogus = 1\n").is_err());
    }
}
