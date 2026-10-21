//! Per-project settings: `<root>/.dendrite/rag.toml`, overridden by the environment.
//!
//! ```toml
//! [embed]
//! backend = "ollama"            # openai | ollama | hf | candle | oxillama | model2vec | none
//! model = "nomic-embed-text"
//! url = "http://localhost:11434"
//! # api_key_env = "HF_TOKEN"   document_prefix / query_prefix / pooling / batch
//!
//! [rerank]
//! model = "bge-reranker-v2-m3"
//!
//! [index]
//! max_file_bytes = 1048576
//! ```
//!
//! Environment overrides: `DENDRITE_RAG_EMBED_BACKEND`, `_MODEL`, `_URL`, `_API_KEY_ENV`,
//! `_DOCUMENT_PREFIX`, `_QUERY_PREFIX`, `_POOLING`, `_BATCH` (all `DENDRITE_RAG_EMBED_*`),
//! `DENDRITE_RAG_RERANK_MODEL`, `DENDRITE_RAG_RERANK_URL`, `DENDRITE_RAG_MAX_FILE_BYTES`, and
//! `DENDRITE_RAG_CONFIG` to read another file. A backend chosen in the environment
//! discards the file's model, URL, prefixes and pooling, which belong to the file's backend.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use crate::embed::{Backend, EmbedConfig, Pooling};
use crate::engine::Config;
use crate::rerank::RerankConfig;

/// Project-relative location of the settings file.
pub const FILE: &str = ".dendrite/rag.toml";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub embed: EmbedConfig,
    pub rerank: RerankConfig,
    pub index: IndexSettings,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IndexSettings {
    pub max_file_bytes: Option<u64>,
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
        Ok(toml::from_str(text)?)
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
        set(&mut self.rerank.model, "DENDRITE_RAG_RERANK_MODEL");
        set(&mut self.rerank.url, "DENDRITE_RAG_RERANK_URL");
        if let Some(n) = var("DENDRITE_RAG_MAX_FILE_BYTES") {
            self.index.max_file_bytes = Some(
                n.parse()
                    .with_context(|| format!("DENDRITE_RAG_MAX_FILE_BYTES={n:?}"))?,
            );
        }
        Ok(())
    }

    /// The engine configuration these settings describe.
    pub fn config(&self) -> Config {
        let mut cfg = Config {
            embed: self.embed.clone(),
            rerank: self.rerank.clone(),
            ..Config::default()
        };
        if let Some(n) = self.index.max_file_bytes {
            cfg.walk.max_file_bytes = n;
        }
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
        assert_eq!(s.embed.backend, Backend::Openai);
    }
}
