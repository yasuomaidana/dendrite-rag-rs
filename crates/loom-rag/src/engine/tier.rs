//! The shared tier: sending journaled commits (edge mode) and pushing the local index.

use super::*;

impl Engine {
    /// Sends commits journaled while the shared tier was unreachable (edge mode); returns
    /// how many are still waiting.
    pub fn sync(&self) -> Result<usize> {
        Ok(self.ledger.sync_now()?)
    }

    #[cfg(feature = "postgres")]
    fn collect_push_embeddings(&self, chunks: &[Chunk]) -> Result<Vec<(String, String, Vec<f32>)>> {
        let Some(e) = &self.embedder else {
            return Ok(Vec::new());
        };
        let model = e.model_id().to_string();
        let mut hashes: Vec<String> = chunks.iter().map(|c| c.content_hash.clone()).collect();
        hashes.sort();
        hashes.dedup();
        let keys: Vec<String> = hashes.iter().map(|h| cache_key(&model, h)).collect();
        let mut embeddings = Vec::new();
        for (h, v) in hashes.iter().zip(self.ledger.embeddings(&keys)?) {
            if let Some(v) = v {
                embeddings.push((model.clone(), h.clone(), v));
            }
        }
        Ok(embeddings)
    }

    #[cfg(feature = "postgres")]
    fn collect_push_meta(&self) -> Result<Vec<(String, String)>> {
        let mut meta = Vec::new();
        for key in ["merkle_root", "last_indexed", "embed_model"] {
            if let Some(v) = self.ledger.meta(key)? {
                meta.push((key.to_string(), v));
            }
        }
        Ok(meta)
    }

    #[cfg(feature = "postgres")]
    fn collect_local_index(&self) -> Result<crate::store::push::LocalIndex> {
        use crate::store::push::LocalIndex;
        let mut chunks = Vec::new();
        self.ledger.for_each_chunk(&mut |c| chunks.push(c))?;
        let embeddings = self.collect_push_embeddings(&chunks)?;
        let vectors = self
            .vectors
            .ids()
            .filter_map(|id| Some((id.to_string(), self.vectors.tag(id)?.to_string())))
            .collect();
        let meta = self.collect_push_meta()?;
        Ok(LocalIndex {
            files: self.ledger.files()?,
            chunks,
            embeddings,
            vectors,
            communities: self.ledger.communities()?,
            meta,
        })
    }

    /// Uploads this project's index to the shared tier, records the current commit's chunks
    /// and compacts the tier (`docs/specs/store.md` § Push and compaction). With `dry_run`
    /// the same work runs in a transaction that is rolled back.
    #[cfg(feature = "postgres")]
    pub fn push(&self, dry_run: bool) -> Result<crate::store::push::PushReport> {
        use crate::store::push::GitState;
        let other = |e: String| Error::Other(anyhow::anyhow!(e));
        let url = self
            .cfg
            .storage
            .pg_url
            .as_deref()
            .ok_or_else(|| other("push needs the shared tier URL (storage pg_url)".to_string()))?;
        let git = GitState::read(&self.root).map_err(other)?;
        let repo = crate::store::repo_name(&self.root, self.cfg.storage.repo.as_deref());
        let local = self.collect_local_index()?;
        let pg = crate::store::pg::Pg::connect(url, &repo).map_err(other)?;
        crate::store::push::push(&pg, local, &git, dry_run).map_err(|e| other(format!("postgres: {e}")))
    }
}
