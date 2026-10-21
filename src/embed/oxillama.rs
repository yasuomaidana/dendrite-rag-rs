//! In-process GGUF embedders on OxiLLaMa (pure Rust, no C/C++): decoder embedding models
//! such as Qwen3-Embedding, with the pooling they were trained with.
//!
//! The model is a `.gguf` file, a hub repository (its `Q8_0` file is preferred, then `f16`,
//! then any `.gguf`), or `<repo>/<file>.gguf`. Hub files are cached in
//! `~/.cache/huggingface`. The tokenizer comes from the GGUF metadata.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use oxillama_runtime::{EngineConfig, InferenceEngine, PoolingMode};

use super::{EmbedConfig, EmbedError, Embedder, Pooling, Result, truncate};

/// Context window for one input. Inputs are cut at 8,000 bytes, well inside it.
const CONTEXT: usize = 4096;

pub struct OxillamaEmbedder {
    /// `InferenceEngine::embed_with` needs `&mut`; one forward pass runs at a time.
    engine: Mutex<InferenceEngine>,
    id: String,
    pooling: PoolingMode,
    document_prefix: String,
    query_prefix: String,
}

fn model_err(e: impl std::fmt::Display) -> EmbedError {
    EmbedError::Model(e.to_string())
}

/// Picks the GGUF to load from a repository's file names.
fn pick_gguf(files: &[String]) -> Option<&String> {
    let ggufs: Vec<&String> = files.iter().filter(|f| f.ends_with(".gguf")).collect();
    let find = |tag: &str| {
        ggufs
            .iter()
            .find(|f| f.to_ascii_lowercase().contains(tag))
            .copied()
    };
    find("q8_0")
        .or_else(|| find("f16"))
        .or_else(|| ggufs.first().copied())
}

/// The local path of the configured model.
fn gguf_path(model: &str) -> Result<PathBuf> {
    let local = Path::new(model);
    if local.is_file() {
        return Ok(local.to_path_buf());
    }
    let api = hf_hub::api::sync::ApiBuilder::from_env()
        .with_progress(false)
        .build()
        .map_err(model_err)?;
    // `<owner>/<repo>/<file>.gguf` names one file of a repository.
    if let Some((repo, file)) = model.ends_with(".gguf").then(|| model.rsplit_once('/')).flatten() {
        return api
            .model(repo.to_string())
            .get(file)
            .map_err(|e| EmbedError::Model(format!("{model}: {e}")));
    }
    let repo = api.model(model.to_string());
    let info = repo
        .info()
        .map_err(|e| EmbedError::Model(format!("{model}: {e}")))?;
    let files: Vec<String> = info.siblings.into_iter().map(|s| s.rfilename).collect();
    let file = pick_gguf(&files)
        .ok_or_else(|| EmbedError::Model(format!("{model}: the repository holds no .gguf file")))?;
    repo.get(file)
        .map_err(|e| EmbedError::Model(format!("{model}/{file}: {e}")))
}

impl OxillamaEmbedder {
    pub fn load(cfg: &EmbedConfig) -> Result<Self> {
        let model = cfg.model();
        let path = gguf_path(&model)?;
        let mut engine = InferenceEngine::new(EngineConfig {
            model_path: path.to_string_lossy().into_owned(),
            context_size: Some(CONTEXT),
            ..EngineConfig::default()
        });
        engine
            .load_model()
            .map_err(|e| EmbedError::Model(format!("{}: {e}", path.display())))?;
        let (document_prefix, query_prefix) = cfg.prefixes();
        Ok(Self {
            engine: Mutex::new(engine),
            id: format!("oxillama:{model}"),
            pooling: match cfg.pooling() {
                Pooling::Mean => PoolingMode::Mean,
                Pooling::Cls => PoolingMode::Cls,
                Pooling::Last => PoolingMode::Last,
            },
            document_prefix,
            query_prefix,
        })
    }

    fn embed(&self, prefix: &str, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let inputs: Vec<String> = texts.iter().map(|t| format!("{prefix}{}", truncate(t))).collect();
        let refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
        let mut engine = self.engine.lock().map_err(model_err)?;
        // embed_batch_with returns L2-normalized vectors.
        engine.embed_batch_with(&refs, self.pooling).map_err(model_err)
    }
}

impl Embedder for OxillamaEmbedder {
    fn model_id(&self) -> &str {
        &self.id
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed(&self.document_prefix, texts)
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        Ok(self.embed(&self.query_prefix, &[text.to_string()])?.remove(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::Backend;

    #[test]
    fn prefers_q8_then_f16() {
        let files = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let f = files(&["README.md", "m-f16.gguf", "m-Q8_0.gguf"]);
        assert_eq!(pick_gguf(&f).unwrap(), "m-Q8_0.gguf");
        let f = files(&["m-Q4_K_M.gguf", "m-f16.gguf"]);
        assert_eq!(pick_gguf(&f).unwrap(), "m-f16.gguf");
        assert!(pick_gguf(&files(&["README.md"])).is_none());
    }

    #[test]
    fn a_missing_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("broken.gguf");
        std::fs::write(&bad, b"not a gguf").unwrap();
        let err = OxillamaEmbedder::load(&EmbedConfig {
            backend: Backend::Oxillama,
            model: Some(bad.to_string_lossy().into_owned()),
            ..Default::default()
        });
        assert!(err.is_err());
    }

    /// Downloads Qwen3-Embedding-0.6B Q8_0 (~640 MB) once:
    /// `DENDRITE_RAG_LIVE_OXILLAMA=1 cargo test --release --features oxillama -- --ignored`.
    #[test]
    #[ignore]
    fn live_qwen3_embedding_ranks_the_matching_code_first() {
        if std::env::var("DENDRITE_RAG_LIVE_OXILLAMA").is_err() {
            return;
        }
        let e = OxillamaEmbedder::load(&EmbedConfig {
            backend: Backend::Oxillama,
            ..Default::default()
        })
        .unwrap();
        let docs = e
            .embed_documents(&[
                "func LocateBinary(name string) (string, error) { return exec.LookPath(name) }".into(),
                "func ParseInvoice(r io.Reader) (*Invoice, error) { return decode(r) }".into(),
            ])
            .unwrap();
        let q = e.embed_query("find the sidecar executable on PATH").unwrap();
        assert_eq!(q.len(), docs[0].len());
        let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        assert!(dot(&q, &docs[0]) > dot(&q, &docs[1]));
    }
}
