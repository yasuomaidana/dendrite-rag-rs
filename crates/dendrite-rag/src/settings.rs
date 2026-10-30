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
//! # abstain = 0.3               # empty context when the best reranked hit is less likely relevant
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
//! [lexical]                     # BM25 (docs/specs/retrieval.md § Lexical index)
//! k1 = 1.2                      # term-frequency saturation
//! b = 0.75                      # length normalization, 0 to 1
//! q = 1.0                       # q-logarithm of the IDF, (0, 1]; 1 is classic BM25
//!
//! [storage]
//! mode = "local"                # local (default) | remote (shared PostgreSQL) | edge
//! # ledger = "sqlite"           # local ledger: redb (default) | sqlite (feature `sqlite`)
//! # repo = "github.com/acme/app" # name in the shared tier; default: the origin URL
//!
//! [graph]
//! resolution = 1.0              # Leiden resolution: higher gives more, smaller communities
//!
//! [ask]
//! # local_model = "local-coder" paid_model = "gemini-2.5-pro"
//! docs_share = 0.2              # share of an `all` context pack kept for documentation (0 to 1)
//!
//! [graph.llm]
//! enabled = false
//! model = "gpt-4o-mini"
//! summaries = false
//! concepts = false
//! ```
//!
//! Environment overrides: `DENDRITE_RAG_EMBED_BACKEND`, `_MODEL`, `_URL`, `_API_KEY_ENV`,
//! `_DOCUMENT_PREFIX`, `_QUERY_PREFIX`, `_POOLING`, `_BATCH`, `_DIMENSIONS`, `_BUDGET_TOKENS` (all `DENDRITE_RAG_EMBED_*`),
//! `DENDRITE_RAG_RERANK_BACKEND`, `_MODEL`, `_URL`, `_API_KEY_ENV`, `_BATCH`, `_ABSTAIN`, `DENDRITE_RAG_MAX_FILE_BYTES`,
//! `DENDRITE_RAG_QUERY_CACHE`, `DENDRITE_RAG_GRAPH_RESOLUTION`, `DENDRITE_RAG_ASK_LOCAL_MODEL`, `_PAID_MODEL`,
//! `_DOCS_SHARE` (all `DENDRITE_RAG_ASK_*`), `DENDRITE_RAG_STORAGE_MODE`, `DENDRITE_RAG_STORAGE_REPO`, `DENDRITE_RAG_STORAGE_LEDGER`,
//! `DENDRITE_RAG_PG_URL` (the shared tier's URL: environment only, never the file),
//! `DENDRITE_RAG_FUSION_LEXICAL`, `_DENSE`, `_SYMBOL`, `_COMMUNITY`, `DENDRITE_RAG_BM25_K1`, `_B`, `_Q`,
//! `DENDRITE_RAG_GRAPH_LLM_ENABLED`,
//! `_MODEL`, `_SUMMARIES`, `_CONCEPTS` (all `DENDRITE_RAG_GRAPH_LLM_*`), and `DENDRITE_RAG_CONFIG` to read another file. A backend chosen in the environment
//! discards the file's model, URL, prefixes and pooling, which belong to the file's backend.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use loom_rag::config::{GraphLlmSettings, VectorIndexChoice};
use loom_rag::embed::{Backend, EmbedConfig, Pooling};
use loom_rag::engine::Config;
use loom_rag::rerank::{RerankBackend, RerankConfig};

/// [`apply_backend_by`] for a section whose backend enum has `parse`, `ALL` and `as_str`.
macro_rules! apply_backend {
    ($var:expr, $key:expr, $backend:ident, $cfg:ident, $section:expr) => {
        apply_backend_by(
            $var,
            $key,
            $backend::parse,
            $backend::ALL.iter().map(|b| b.as_str()).collect(),
            $section,
            |c| c.backend,
            |backend| $cfg {
                backend,
                ..Default::default()
            },
        )?
    };
}

use loom_rag::retrieve::DEFAULT_DOCS_SHARE;

/// Project-relative location of the settings file.
pub const FILE: &str = ".dendrite/rag.toml";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub embed: EmbedConfig,
    pub rerank: RerankConfig,
    pub index: IndexSettings,
    pub graph: GraphSettings,
    pub fusion: loom_rag::retrieve::FusionConfig,
    pub lexical: loom_rag::bm25::Bm25Params,
    pub storage: StorageSettings,
    pub ask: AskSettings,
    /// `DENDRITE_RAG_PG_URL`; never read from or written to the file.
    #[serde(skip)]
    pub pg_url: Option<String>,
}

/// `[ask]`: the gateway models `ask` uses (`docs/specs/ask.md`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AskSettings {
    pub local_model: Option<String>,
    pub paid_model: Option<String>,
    /// Share of an `all` context pack kept for documentation, 0 to 1 (default
    /// [`DEFAULT_DOCS_SHARE`]); `ask` and `context` use it when a request names none.
    pub docs_share: Option<f32>,
}

impl AskSettings {
    /// The documentation share for a request: `requested`, else the project's, else the
    /// engine's default.
    pub fn docs_share(&self, requested: Option<f32>) -> f32 {
        requested.or(self.docs_share).unwrap_or(DEFAULT_DOCS_SHARE)
    }
}

/// Whether `share` is a documentation share: a number from 0 to 1.
pub fn valid_docs_share(share: f32) -> bool {
    (0.0..=1.0).contains(&share)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageSettings {
    pub mode: Option<loom_rag::store::StorageMode>,
    pub repo: Option<String>,
    /// The local index's ledger: redb (default) or sqlite (feature `sqlite`).
    pub ledger: Option<loom_rag::store::LedgerChoice>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GraphSettings {
    pub resolution: Option<f64>,
    pub llm: GraphLlmSettings,
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
        apply_backend!(
            &var,
            "DENDRITE_RAG_EMBED_BACKEND",
            Backend,
            EmbedConfig,
            &mut self.embed
        );
        let e = &mut self.embed;
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
        env_parse(&var, &mut e.batch, "DENDRITE_RAG_EMBED_BATCH")?;
        env_parse(&var, &mut e.dimensions, "DENDRITE_RAG_EMBED_DIMENSIONS")?;
        env_parse(&var, &mut e.budget_tokens, "DENDRITE_RAG_EMBED_BUDGET_TOKENS")?;
        apply_backend!(
            &var,
            "DENDRITE_RAG_RERANK_BACKEND",
            RerankBackend,
            RerankConfig,
            &mut self.rerank
        );
        set(&mut self.rerank.model, "DENDRITE_RAG_RERANK_MODEL");
        set(&mut self.rerank.url, "DENDRITE_RAG_RERANK_URL");
        set(&mut self.rerank.api_key_env, "DENDRITE_RAG_RERANK_API_KEY_ENV");
        env_parse(&var, &mut self.rerank.abstain, "DENDRITE_RAG_RERANK_ABSTAIN")?;
        if let Some(a) = self.rerank.abstain
            && !(0.0..=1.0).contains(&a)
        {
            bail!("rerank.abstain must be within 0..=1, got {a}");
        }
        env_parse(&var, &mut self.rerank.batch, "DENDRITE_RAG_RERANK_BATCH")?;
        env_parse(
            &var,
            &mut self.index.max_file_bytes,
            "DENDRITE_RAG_MAX_FILE_BYTES",
        )?;
        env_parse(&var, &mut self.index.query_cache, "DENDRITE_RAG_QUERY_CACHE")?;
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
        for (key, field) in [
            ("DENDRITE_RAG_BM25_K1", &mut self.lexical.k1),
            ("DENDRITE_RAG_BM25_B", &mut self.lexical.b),
            ("DENDRITE_RAG_BM25_Q", &mut self.lexical.q),
        ] {
            if let Some(v) = var(key) {
                *field = v.parse().with_context(|| format!("{key}={v:?}"))?;
            }
        }
        if let Err(e) = self.lexical.validate() {
            bail!("lexical: {e}");
        }
        env_choice(
            &var,
            &mut self.storage.mode,
            "DENDRITE_RAG_STORAGE_MODE",
            loom_rag::store::StorageMode::parse,
            "local, remote or edge",
        )?;
        env_choice(
            &var,
            &mut self.storage.ledger,
            "DENDRITE_RAG_STORAGE_LEDGER",
            loom_rag::store::LedgerChoice::parse,
            "redb or sqlite",
        )?;
        set(&mut self.storage.repo, "DENDRITE_RAG_STORAGE_REPO");
        set(&mut self.ask.local_model, "DENDRITE_RAG_ASK_LOCAL_MODEL");
        set(&mut self.ask.paid_model, "DENDRITE_RAG_ASK_PAID_MODEL");
        env_parse(&var, &mut self.ask.docs_share, "DENDRITE_RAG_ASK_DOCS_SHARE")?;
        set(&mut self.pg_url, "DENDRITE_RAG_PG_URL");
        env_parse(&var, &mut self.graph.resolution, "DENDRITE_RAG_GRAPH_RESOLUTION")?;
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
        env_parse(
            &var,
            &mut self.index.hnsw_ef_search,
            "DENDRITE_RAG_HNSW_EF_SEARCH",
        )?;
        self.validate()?;
        Ok(())
    }

    /// Validates the settings consistency.
    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(s) = self.ask.docs_share
            && !valid_docs_share(s)
        {
            bail!("ask.docs_share must be a number from 0 to 1, got {s}");
        }
        if let Some(r) = self.graph.resolution
            && !(r > 0.0 && r.is_finite())
        {
            bail!("graph.resolution must be a positive number, got {r}");
        }
        if self.index.vector_index == Some(VectorIndexChoice::Hnsw) && !cfg!(feature = "hnsw") {
            bail!("vector index 'hnsw' requires cargo feature 'hnsw'");
        }
        if let Some(d) = self.embed.dimensions {
            if d == 0 {
                bail!("embed.dimensions must be a positive number");
            }
            // Only the OpenAI-compatible request carries `dimensions`; elsewhere it would be
            // ignored and the vectors would keep the model's size under a `@dims` name.
            if !matches!(self.embed.backend, Backend::Openai | Backend::Auto) {
                bail!(
                    "embed.dimensions needs the openai backend (or auto), got {}",
                    self.embed.backend.as_str()
                );
            }
        }
        if self.graph.llm.enabled && self.graph.llm.model.as_deref().unwrap_or("").trim().is_empty() {
            bail!(
                "graph.llm is enabled but no model is configured (set graph.llm.model or DENDRITE_RAG_GRAPH_LLM_MODEL)"
            );
        }
        Ok(())
    }

    /// [`Settings::config`] for the project at `root`, with the index in [`INDEX_DIR`].
    pub fn config_at(&self, root: &Path) -> Config {
        Config {
            index_dir: Some(root.join(INDEX_DIR)),
            ..self.config()
        }
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
        cfg.bm25 = self.lexical;
        // Dendrite keeps its own state under `.dendrite/`; only the index is ours.
        cfg.walk.exclude.push(".dendrite/**".to_string());
        cfg.storage = loom_rag::store::StorageConfig {
            mode: self.storage.mode.unwrap_or_default(),
            repo: self.storage.repo.clone(),
            pg_url: self.pg_url.clone(),
            ledger: self.storage.ledger.unwrap_or_default(),
        };
        cfg
    }
}

/// Where Dendrite keeps a project's index, under the project root.
pub const INDEX_DIR: &str = ".dendrite/rag";

/// The engine configuration of the project at `root`: defaults, its `.dendrite/rag.toml`,
/// then the environment, with the index in [`INDEX_DIR`].
pub fn resolve(root: &Path) -> anyhow::Result<Config> {
    Ok(Settings::load(root)?.config_at(root))
}

/// Reads `key` from the environment into `field` when it is set.
fn env_parse<T: std::str::FromStr>(
    var: &impl Fn(&str) -> Option<String>,
    field: &mut Option<T>,
    key: &str,
) -> anyhow::Result<()>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    if let Some(v) = var(key) {
        *field = Some(v.parse().with_context(|| format!("{key}={v:?}"))?);
    }
    Ok(())
}

/// Applies a `*_BACKEND` variable to `section`: naming a different backend restarts the section
/// from that backend's defaults. An error lists the `names` `key` accepts.
fn apply_backend_by<C, B: PartialEq>(
    var: &impl Fn(&str) -> Option<String>,
    key: &str,
    parse: fn(&str) -> Option<B>,
    names: Vec<&str>,
    section: &mut C,
    backend_of: fn(&C) -> B,
    restart: fn(B) -> C,
) -> anyhow::Result<()> {
    let Some(raw) = var(key) else {
        return Ok(());
    };
    let backend =
        parse(&raw).ok_or_else(|| anyhow::anyhow!("{key}={raw:?}: expected one of {}", names.join(", ")))?;
    if backend != backend_of(section) {
        *section = restart(backend);
    }
    Ok(())
}

/// Reads `key` into `field` through `parse` when it is set; `expected` names the accepted values.
fn env_choice<T>(
    var: &impl Fn(&str) -> Option<String>,
    field: &mut Option<T>,
    key: &str,
    parse: fn(&str) -> Option<T>,
    expected: &str,
) -> anyhow::Result<()> {
    if let Some(raw) = var(key) {
        *field = Some(parse(&raw).ok_or_else(|| anyhow::anyhow!("{key}={raw:?}: expected {expected}"))?);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
