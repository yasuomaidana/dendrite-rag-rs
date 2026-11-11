//! The environment overrides of the settings: `DENDRITE_RAG_*` variables, applied after the file.

use anyhow::{Context, bail};

use loom_rag::config::VectorIndexChoice;
use loom_rag::embed::{Backend, EmbedConfig, Pooling};
use loom_rag::rerank::{RerankBackend, RerankConfig};

use super::Settings;

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

/// Reads environment variables; a closure so tests can supply their own.
type Var<'a, F> = &'a F;

impl Settings {
    pub(super) fn apply_env(&mut self, var: impl Fn(&str) -> Option<String>) -> anyhow::Result<()> {
        let var = &var;
        self.apply_embed_env(var)?;
        self.apply_rerank_env(var)?;
        env_parse(var, &mut self.index.max_file_bytes, "DENDRITE_RAG_MAX_FILE_BYTES")?;
        env_parse(var, &mut self.index.query_cache, "DENDRITE_RAG_QUERY_CACHE")?;
        self.apply_fusion_env(var)?;
        self.apply_lexical_env(var)?;
        self.apply_storage_env(var)?;
        self.apply_ask_env(var)?;
        self.apply_graph_llm_env(var)?;
        self.apply_vector_index_env(var)?;
        self.validate()?;
        Ok(())
    }

    fn apply_embed_env<F: Fn(&str) -> Option<String>>(&mut self, var: Var<F>) -> anyhow::Result<()> {
        apply_backend!(
            var,
            "DENDRITE_RAG_EMBED_BACKEND",
            Backend,
            EmbedConfig,
            &mut self.embed
        );
        let e = &mut self.embed;
        set(var, &mut e.model, "DENDRITE_RAG_EMBED_MODEL");
        set(var, &mut e.url, "DENDRITE_RAG_EMBED_URL");
        set(var, &mut e.api_key_env, "DENDRITE_RAG_EMBED_API_KEY_ENV");
        set(var, &mut e.document_prefix, "DENDRITE_RAG_EMBED_DOCUMENT_PREFIX");
        set(var, &mut e.query_prefix, "DENDRITE_RAG_EMBED_QUERY_PREFIX");
        if let Some(p) = var("DENDRITE_RAG_EMBED_POOLING") {
            e.pooling = Some(parse_pooling(&p)?);
        }
        env_parse(var, &mut e.batch, "DENDRITE_RAG_EMBED_BATCH")?;
        env_parse(var, &mut e.dimensions, "DENDRITE_RAG_EMBED_DIMENSIONS")?;
        env_parse(var, &mut e.budget_tokens, "DENDRITE_RAG_EMBED_BUDGET_TOKENS")
    }

    fn apply_rerank_env<F: Fn(&str) -> Option<String>>(&mut self, var: Var<F>) -> anyhow::Result<()> {
        apply_backend!(
            var,
            "DENDRITE_RAG_RERANK_BACKEND",
            RerankBackend,
            RerankConfig,
            &mut self.rerank
        );
        set(var, &mut self.rerank.model, "DENDRITE_RAG_RERANK_MODEL");
        set(var, &mut self.rerank.url, "DENDRITE_RAG_RERANK_URL");
        set(
            var,
            &mut self.rerank.api_key_env,
            "DENDRITE_RAG_RERANK_API_KEY_ENV",
        );
        env_parse(var, &mut self.rerank.abstain, "DENDRITE_RAG_RERANK_ABSTAIN")?;
        if let Some(a) = self.rerank.abstain
            && !(0.0..=1.0).contains(&a)
        {
            bail!("rerank.abstain must be within 0..=1, got {a}");
        }
        env_parse(var, &mut self.rerank.batch, "DENDRITE_RAG_RERANK_BATCH")
    }

    fn apply_fusion_env<F: Fn(&str) -> Option<String>>(&mut self, var: Var<F>) -> anyhow::Result<()> {
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
        Ok(())
    }

    fn apply_lexical_env<F: Fn(&str) -> Option<String>>(&mut self, var: Var<F>) -> anyhow::Result<()> {
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
        Ok(())
    }

    fn apply_storage_env<F: Fn(&str) -> Option<String>>(&mut self, var: Var<F>) -> anyhow::Result<()> {
        env_choice(
            var,
            &mut self.storage.mode,
            "DENDRITE_RAG_STORAGE_MODE",
            loom_rag::store::StorageMode::parse,
            "local, remote or edge",
        )?;
        env_choice(
            var,
            &mut self.storage.ledger,
            "DENDRITE_RAG_STORAGE_LEDGER",
            loom_rag::store::LedgerChoice::parse,
            "redb or sqlite",
        )?;
        set(var, &mut self.storage.repo, "DENDRITE_RAG_STORAGE_REPO");
        Ok(())
    }

    fn apply_ask_env<F: Fn(&str) -> Option<String>>(&mut self, var: Var<F>) -> anyhow::Result<()> {
        set(var, &mut self.ask.local_model, "DENDRITE_RAG_ASK_LOCAL_MODEL");
        set(var, &mut self.ask.paid_model, "DENDRITE_RAG_ASK_PAID_MODEL");
        env_parse(var, &mut self.ask.docs_share, "DENDRITE_RAG_ASK_DOCS_SHARE")?;
        set(var, &mut self.pg_url, "DENDRITE_RAG_PG_URL");
        env_parse(var, &mut self.graph.resolution, "DENDRITE_RAG_GRAPH_RESOLUTION")
    }

    fn apply_graph_llm_env<F: Fn(&str) -> Option<String>>(&mut self, var: Var<F>) -> anyhow::Result<()> {
        let llm = &mut self.graph.llm;
        env_bool(var, &mut llm.enabled, "DENDRITE_RAG_GRAPH_LLM_ENABLED")?;
        set(var, &mut llm.model, "DENDRITE_RAG_GRAPH_LLM_MODEL");
        env_bool(var, &mut llm.summaries, "DENDRITE_RAG_GRAPH_LLM_SUMMARIES")?;
        env_bool(var, &mut llm.concepts, "DENDRITE_RAG_GRAPH_LLM_CONCEPTS")
    }

    fn apply_vector_index_env<F: Fn(&str) -> Option<String>>(&mut self, var: Var<F>) -> anyhow::Result<()> {
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
        env_parse(var, &mut self.index.hnsw_ef_search, "DENDRITE_RAG_HNSW_EF_SEARCH")
    }
}

fn parse_pooling(p: &str) -> anyhow::Result<Pooling> {
    serde_json::from_value::<Pooling>(serde_json::Value::String(p.to_ascii_lowercase()))
        .with_context(|| format!("DENDRITE_RAG_EMBED_POOLING={p:?}: expected mean, cls or last"))
}

/// Sets `field` to `key` when the variable is set.
fn set<F: Fn(&str) -> Option<String>>(var: Var<F>, field: &mut Option<String>, key: &str) {
    if let Some(v) = var(key) {
        *field = Some(v);
    }
}

/// Reads a true/false variable (`true`, `1`, `yes`, `on` and their opposites) into `field` when it is set.
fn env_bool<F: Fn(&str) -> Option<String>>(var: Var<F>, field: &mut bool, key: &str) -> anyhow::Result<()> {
    if let Some(v) = var(key) {
        *field = match v.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => true,
            "false" | "0" | "no" | "off" => false,
            _ => bail!("{key}={v:?}: expected true or false"),
        };
    }
    Ok(())
}

/// Reads `key` from the environment into `field` when it is set.
fn env_parse<F, T>(var: Var<F>, field: &mut Option<T>, key: &str) -> anyhow::Result<()>
where
    F: Fn(&str) -> Option<String>,
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    if let Some(v) = var(key) {
        *field = Some(v.parse().with_context(|| format!("{key}={v:?}"))?);
    }
    Ok(())
}

/// Applies a `*_BACKEND` variable to `section`: naming a different backend restarts the section
/// from that backend's defaults. An error lists the `names` `key` accepts.
fn apply_backend_by<F: Fn(&str) -> Option<String>, C, B: PartialEq>(
    var: Var<F>,
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
fn env_choice<F: Fn(&str) -> Option<String>, T>(
    var: Var<F>,
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
