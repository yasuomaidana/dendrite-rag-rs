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
        let Some(model) = self.embedder.as_ref().map(|e| e.model_id().to_string()) else {
            return self.drop_all_vectors();
        };
        if !changed && self.vectors_are_current(&model)? {
            return Ok(());
        }
        let todo = self.chunks_to_embed(&model, deadline, focus)?;
        self.embed_chunks(&model, &todo, deadline, report)?;
        let mut batch = self.ledger.batch()?;
        batch.set_meta("embed_model", &model)?;
        batch.commit()?;
        self.vectors.persist()?;
        Ok(())
    }

    /// Without an embedder the index holds no vectors.
    fn drop_all_vectors(&mut self) -> Result<()> {
        if !self.vectors.is_empty() {
            self.vectors.clear();
            self.vectors.persist()?;
        }
        Ok(())
    }

    /// Embeds `todo` in groups until the time or token budget runs out.
    fn embed_chunks(
        &mut self,
        model: &str,
        todo: &[String],
        deadline: Option<Instant>,
        report: &mut IndexReport,
    ) -> Result<()> {
        let budget_tokens = self.cfg.embed.budget_tokens;
        if let Some(b) = budget_tokens {
            report.budget_tokens_remaining = Some(b);
        }
        let Some(embedder) = self.embedder.as_deref() else {
            return Ok(());
        };
        let mut pass = EmbedPass {
            embedder,
            ledger: self.ledger.as_ref(),
            vectors: self.vectors.as_mut(),
            model,
            deadline,
            budget_tokens,
            // Under a budget (time or tokens), small groups keep the overshoot small.
            group_size: if deadline.is_some() || budget_tokens.is_some() {
                EMBED_GROUP_BUDGETED
            } else {
                EMBED_GROUP
            },
            tokens_spent: 0,
        };
        pass.run(todo, report)
    }

    /// The chunks that need a new embedding, the ones `focus` matches first when the run is under a
    /// deadline. Vectors of chunks that no longer exist are dropped and cached embeddings are taken.
    fn chunks_to_embed(
        &mut self,
        model: &str,
        deadline: Option<Instant>,
        focus: Option<&str>,
    ) -> Result<Vec<String>> {
        let live = self.drop_stale_vectors()?;
        let mut todo = self.uncached_chunks(&live, model)?;
        if let Some(q) = focus.filter(|_| deadline.is_some()) {
            self.focus_first(&mut todo, q)?;
        }
        Ok(todo)
    }

    /// Puts the chunks `focus` matches lexically before the rest of the backlog.
    fn focus_first(&self, todo: &mut [String], focus: &str) -> Result<()> {
        let first: HashSet<String> = self
            .lexical
            .search(focus, FOCUS_CANDIDATES, &self.cfg.bm25)?
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        todo.sort_by_key(|id| !first.contains(id));
        Ok(())
    }

    /// True when the vector index already holds exactly the stored chunks for `model`.
    fn vectors_are_current(&self, model: &str) -> Result<bool> {
        let chunk_count = self.ledger.chunk_count()? as usize;
        let same_model = self.ledger.meta("embed_model")?.as_deref() == Some(model);
        if !same_model || self.vectors.len() != chunk_count {
            return Ok(false);
        }
        // Equal counts can still hide a stale file: compare ids too. The file table
        // lists every chunk id without loading chunk texts.
        let files = self.ledger.files()?;
        Ok(files
            .values()
            .flat_map(|f| &f.chunk_ids)
            .all(|id| self.vectors.contains(id)))
    }

    /// Removes the vectors of chunks that no longer exist; returns the live chunks' ids and
    /// content hashes (only those are kept in memory; texts are loaded per group).
    fn drop_stale_vectors(&mut self) -> Result<BTreeMap<String, String>> {
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
        Ok(live)
    }

    /// Takes the cached embedding of every chunk without a vector; the chunks the cache does
    /// not have are returned to be embedded.
    fn uncached_chunks(&mut self, live: &BTreeMap<String, String>, model: &str) -> Result<Vec<String>> {
        let mut todo: Vec<String> = Vec::new();
        let missing: Vec<(&String, &String)> =
            live.iter().filter(|(id, _)| !self.vectors.contains(id)).collect();
        for (id, hash) in missing {
            match self.ledger.embedding(&cache_key(model, hash))? {
                Some(v) => self.vectors.upsert(id, hash, &v),
                None => todo.push(id.clone()),
            }
        }
        Ok(todo)
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
        let ranked = self.lexical.search(query, top_k, &self.cfg.bm25)?;
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

/// Whether the embedding loop goes on after a group.
enum Step {
    Next,
    Stop,
}

/// One embedding run over the chunks that have no vector: groups of chunks are embedded and committed
/// until the time or token budget runs out.
struct EmbedPass<'a> {
    embedder: &'a dyn Embedder,
    ledger: &'a dyn LedgerStore,
    vectors: &'a mut dyn VectorIndex,
    model: &'a str,
    deadline: Option<Instant>,
    budget_tokens: Option<usize>,
    group_size: usize,
    tokens_spent: usize,
}

impl EmbedPass<'_> {
    /// Embeds in groups, committing each group's cache entries so progress survives interruptions.
    fn run(&mut self, todo: &[String], report: &mut IndexReport) -> Result<()> {
        for (n, ids) in todo.chunks(self.group_size).enumerate() {
            let pending = todo.len() - n * self.group_size;
            if self.exhausted() {
                report.embed_pending = pending;
                break;
            }
            if let Step::Stop = self.group(ids, n, report)? {
                report.embed_pending = pending;
                break;
            }
        }
        Ok(())
    }

    fn exhausted(&self) -> bool {
        self.deadline.is_some_and(|d| Instant::now() >= d)
            || self.budget_tokens.is_some_and(|b| self.tokens_spent >= b)
    }

    fn group(&mut self, ids: &[String], n: usize, report: &mut IndexReport) -> Result<Step> {
        let group = self.ledger.chunks(ids)?;
        let texts: Vec<String> = group.iter().map(|c| c.indexed_text()).collect();
        let estimated_tokens: usize = texts
            .iter()
            .map(|t| (t.chars().count() as f32 / 4.0 * 1.15).ceil() as usize + 8)
            .sum();
        if let Some(b) = self.budget_tokens
            && n > 0
            && self.tokens_spent + estimated_tokens > b
        {
            return Ok(Step::Stop);
        }
        let vectors = match self.embedder.embed_documents(&texts) {
            Ok(v) => v,
            Err(e) => {
                // Clients decide whether to report coverage from `embed_pending`, so a
                // failed group must count as pending, not as done.
                report.embed_error = Some(e.to_string());
                return Ok(Step::Stop);
            }
        };
        self.tokens_spent += estimated_tokens;
        report.embed_tokens = self.tokens_spent;
        if let Some(b) = self.budget_tokens {
            report.budget_tokens_remaining = Some(b.saturating_sub(self.tokens_spent));
        }
        let mut batch = self.ledger.batch()?;
        for (c, v) in group.iter().zip(&vectors) {
            batch.put_embedding(&cache_key(self.model, &c.content_hash), v)?;
            self.vectors.upsert(&c.id, &c.content_hash, v);
        }
        batch.commit()?;
        report.embedded += group.len();
        Ok(Step::Next)
    }
}
