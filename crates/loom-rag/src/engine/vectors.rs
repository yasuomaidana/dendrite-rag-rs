//! Embeddings and the vector index: keeping one vector per chunk, query embedding
//! (cached), dense ranking and hit resolution.

use super::*;

use std::collections::HashSet;

/// The int8 shortlist is this many times `top_k` (at least `RESCORE_MIN`) before exact
/// rescoring.
const RESCORE_FACTOR: usize = 4;

const RESCORE_MIN: usize = 40;

/// Chunks per embedding call group (and per cache commit).
const EMBED_GROUP: usize = 64;

/// Group size when the embedding phase runs under a time budget.
const EMBED_GROUP_BUDGETED: usize = 8;
/// Lexical candidates of the focus query embedded first under a budget.
const FOCUS_CANDIDATES: usize = 64;

impl Engine {
    /// Makes the vector index hold exactly one vector per stored chunk, embedding only
    /// chunks whose `(model, content_hash)` is not cached. Embedding failures are reported,
    /// not raised, so the lexical index stays usable.
    pub(super) fn sync_vectors(
        &mut self,
        changed: bool,
        deadline: Option<Instant>,
        focus: Option<&str>,
        report: &mut IndexReport,
    ) -> Result<()> {
        let Some(embedder) = &self.embedder else {
            if !self.vectors.is_empty() {
                self.vectors.clear();
                self.vectors.persist()?;
            }
            return Ok(());
        };
        let model = embedder.model_id().to_string();
        let chunk_count = self.ledger.chunk_count()? as usize;
        let same_model = self.ledger.meta("embed_model")?.as_deref() == Some(model.as_str());
        if !changed && same_model && self.vectors.len() == chunk_count {
            // Equal counts can still hide a stale file: compare ids too. The file table
            // lists every chunk id without loading chunk texts.
            let files = self.ledger.files()?;
            if files
                .values()
                .flat_map(|f| &f.chunk_ids)
                .all(|id| self.vectors.contains(id))
            {
                return Ok(());
            }
        }

        // Only ids and content hashes are kept in memory; texts are loaded per group.
        let mut live: BTreeMap<String, String> = BTreeMap::new();
        self.ledger.for_each_chunk(&mut |c| {
            live.insert(c.id, c.content_hash);
        })?;
        let stale: Vec<String> = self
            .vectors
            .ids()
            .filter(|id| !live.contains_key(*id))
            .map(str::to_string)
            .collect();
        for id in &stale {
            self.vectors.remove(id);
        }

        let mut todo: Vec<String> = Vec::new();
        let missing: Vec<(&String, &String)> =
            live.iter().filter(|(id, _)| !self.vectors.contains(id)).collect();
        for (id, hash) in missing {
            match self.ledger.embedding(&cache_key(&model, hash))? {
                Some(v) => self.vectors.upsert(id, hash, &v),
                None => todo.push(id.clone()),
            }
        }

        if let Some(q) = focus.filter(|_| deadline.is_some()) {
            let first: HashSet<String> = self
                .lexical
                .search(q, FOCUS_CANDIDATES)?
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            todo.sort_by_key(|id| !first.contains(id));
        }

        // Embed in groups, committing each group's cache entries so progress survives
        // interruptions.
        // Under a budget, small groups keep the overshoot to a fraction of a second.
        let group = if deadline.is_some() {
            EMBED_GROUP_BUDGETED
        } else {
            EMBED_GROUP
        };
        for (n, ids) in todo.chunks(group).enumerate() {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                report.embed_pending = todo.len() - n * group;
                break;
            }
            let group = self.ledger.chunks(ids)?;
            let texts: Vec<String> = group.iter().map(|c| c.indexed_text()).collect();
            let vectors = match embedder.embed_documents(&texts) {
                Ok(v) => v,
                Err(e) => {
                    report.embed_error = Some(e.to_string());
                    break;
                }
            };
            let mut batch = self.ledger.batch()?;
            for (c, v) in group.iter().zip(&vectors) {
                batch.put_embedding(&cache_key(&model, &c.content_hash), v)?;
                self.vectors.upsert(&c.id, &c.content_hash, v);
            }
            batch.commit()?;
            report.embedded += group.len();
        }

        let mut batch = self.ledger.batch()?;
        batch.set_meta("embed_model", &model)?;
        batch.commit()?;
        self.vectors.persist()?;
        Ok(())
    }

    /// Dense-only search; empty when no embedder is configured.
    pub fn search_dense(&self, query: &str, top_k: usize) -> Result<Vec<Hit>> {
        let ranked = self.dense_ranking(query, top_k)?;
        self.hits(&ranked)
    }

    /// Embeds a query with the configured embedder (`None` without one).
    pub fn embed_query(&self, query: &str) -> Result<Option<Vec<f32>>> {
        let Some(embedder) = &self.embedder else {
            return Ok(None);
        };
        Ok(Some(self.cached_embed_query(embedder.as_ref(), query)?))
    }

    /// Nearest chunks to an already-embedded query. The int8 scan shortlists
    /// `RESCORE_FACTOR × top_k` rows, which are rescored with their exact `f32` vectors
    /// from the embedding cache.
    pub fn search_vector(&self, query: &[f32], top_k: usize) -> Result<Vec<(String, f32)>> {
        let mut shortlist = self
            .vectors
            .search(query, (top_k * RESCORE_FACTOR).max(RESCORE_MIN));
        if let Some(model) = self.embedder.as_ref().map(|e| e.model_id()) {
            let keys: Vec<String> = shortlist
                .iter()
                .map(|(id, _)| cache_key(model, self.vectors.tag(id).unwrap_or_default()))
                .collect();
            for ((_, score), exact) in shortlist.iter_mut().zip(self.ledger.embeddings(&keys)?) {
                if let Some(v) = exact {
                    *score = v.iter().zip(query).map(|(a, b)| a * b).sum();
                }
            }
            shortlist.sort_by(|a, b| b.1.total_cmp(&a.1));
        }
        shortlist.truncate(top_k);
        Ok(shortlist)
    }

    /// Embeds a query, reusing the vector of an identical earlier query with the same model.
    pub(super) fn cached_embed_query(&self, embedder: &dyn Embedder, query: &str) -> Result<Vec<f32>> {
        let Some(cache) = &self.query_cache else {
            return Ok(embedder.embed_query(query)?);
        };
        let key = (embedder.model_id().to_string(), query.to_string());
        if let Some(v) = cache.lock().expect("query cache lock").get(&key) {
            return Ok(v.clone());
        }
        let v = embedder.embed_query(query)?;
        cache.lock().expect("query cache lock").put(key, v.clone());
        Ok(v)
    }

    /// `(chunk_id, cosine)` for the query, best first.
    pub fn dense_ranking(&self, query: &str, top_k: usize) -> Result<Vec<(String, f32)>> {
        let Some(embedder) = &self.embedder else {
            return Ok(Vec::new());
        };
        if self.vectors.is_empty() {
            return Ok(Vec::new());
        }
        let q = self.cached_embed_query(embedder.as_ref(), query)?;
        self.search_vector(&q, top_k)
    }

    /// BM25-only search.
    pub fn search_lexical(&self, query: &str, top_k: usize) -> Result<Vec<Hit>> {
        let ranked = self.lexical.search(query, top_k)?;
        self.hits(&ranked)
    }

    /// Resolves ranked chunk ids into hits, keeping the order.
    pub fn hits(&self, ranked: &[(String, f32)]) -> Result<Vec<Hit>> {
        let ids: Vec<String> = ranked.iter().map(|(id, _)| id.clone()).collect();
        let chunks: BTreeMap<String, Chunk> = self
            .ledger
            .chunks(&ids)?
            .into_iter()
            .map(|c| (c.id.clone(), c))
            .collect();
        Ok(ranked
            .iter()
            .filter_map(|(id, s)| chunks.get(id).map(|c| Hit::from_chunk(c, *s)))
            .collect())
    }

    /// Fetches every embedding the current chunks need that is missing locally (edge mode
    /// reads them from the shared tier), then fills the vector index, so the project can be
    /// searched offline. Returns how many chunks have a vector.
    pub fn pull(&mut self) -> Result<PullReport> {
        let Some(embedder) = &self.embedder else {
            return Ok(PullReport::default());
        };
        let model = embedder.model_id().to_string();
        let mut keys: Vec<String> = Vec::new();
        self.ledger
            .for_each_chunk(&mut |c| keys.push(cache_key(&model, &c.content_hash)))?;
        let mut available = 0;
        for group in keys.chunks(512) {
            available += self
                .ledger
                .embeddings(group)?
                .iter()
                .filter(|v| v.is_some())
                .count();
        }
        let mut report = IndexReport::default();
        self.sync_vectors(true, Some(Instant::now()), None, &mut report)?;
        Ok(PullReport {
            chunks: keys.len(),
            cached: available,
            vectors: self.vectors.len(),
        })
    }
}
