//! Shared test fixtures.

use std::fs;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::embed::{EmbedError, Embedder, Result, normalize};

/// A small Go + Markdown project.
pub fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("pkg/sidecar")).unwrap();
    fs::create_dir_all(root.join("pkg/rag")).unwrap();
    fs::write(
        root.join("pkg/sidecar/sidecar.go"),
        "package sidecar\n\n// LocateBinary finds a sidecar executable.\nfunc LocateBinary(name string) string { return name }\n",
    )
    .unwrap();
    fs::write(
        root.join("pkg/rag/indexer.go"),
        "package rag\n\nfunc IndexProject(root string) error { return nil }\n",
    )
    .unwrap();
    fs::write(root.join("README.md"), "# Demo\n\nA project used in tests.\n").unwrap();
    dir
}

/// Feature-hashed bag of code tokens: deterministic, and texts sharing identifiers land
/// close together. Setting `fail` makes every document call error.
pub struct HashEmbedder {
    pub fail: AtomicBool,
    /// Makes query embedding fail (the gateway went down after indexing).
    pub fail_queries: AtomicBool,
    pub calls: AtomicUsize,
}

impl HashEmbedder {
    pub fn new() -> Self {
        Self {
            fail: false.into(),
            fail_queries: false.into(),
            calls: 0.into(),
        }
    }

    pub fn vector(text: &str) -> Vec<f32> {
        let mut v = vec![0f32; 64];
        for t in crate::lexical::tokenize(text) {
            let h = blake3::hash(t.text.as_bytes());
            v[h.as_bytes()[0] as usize % 64] += 1.0;
        }
        normalize(v)
    }
}

impl Embedder for HashEmbedder {
    fn model_id(&self) -> &str {
        "hash-64"
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(EmbedError::Http("gateway down".into()));
        }
        self.calls.fetch_add(texts.len(), Ordering::SeqCst);
        Ok(texts.iter().map(|t| Self::vector(t)).collect())
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        if self.fail_queries.load(Ordering::SeqCst) {
            return Err(EmbedError::Http("gateway down".into()));
        }
        Ok(Self::vector(text))
    }
}
