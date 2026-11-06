//! The code pipeline: lexical, dense and symbol-name rankings fused with weighted RRF and
//! raised by matching community summaries; context packs are expanded one hop through the
//! symbol graph.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use super::{
    CONTEXT_HITS, ContextPack, Detail, FusionWeights, Mode, Ranking, SearchOpts, Timings, dedup, ms_since,
    outline, pack,
};
use crate::corpus::CorpusFilter;
use crate::engine::{Engine, Error, Hit, Result};
use crate::fusion::{RRF_K, rrf};

/// Communities whose summaries rank highest that get the boost.
const BOOSTED_COMMUNITIES: usize = 2;
/// Hits whose symbol-graph neighbours are pulled into a context pack.
const EXPAND_TOP: usize = 8;
const NEIGHBOURS_PER_HIT: usize = 4;
const HOP_DECAY: f32 = 0.5;
/// A neighbour in another community than its hit decays once more.
const CROSS_COMMUNITY_DECAY: f32 = 0.5;

impl Engine {
    /// Fused ranking of the chunks `corpus` admits (code, or code and documentation).
    pub(super) fn code_ranking(
        &self,
        query: &str,
        n: usize,
        mode: Mode,
        w: FusionWeights,
        corpus: CorpusFilter,
        t: &mut Timings,
    ) -> Result<Ranking> {
        // Summaries never compete with code inside a ranking: they form their own list,
        // expanded to their communities' code (hybrid only).
        let split = |ranked: Ranking| -> (Ranking, Ranking) {
            ranked
                .into_iter()
                .partition(|(id, _)| !crate::graph::summary::is_summary(id))
        };
        Ok(match mode {
            Mode::Lexical => split(self.lexical_in(query, n, corpus, t)?).0,
            Mode::Dense => split(self.dense_in(query, n, corpus, t)?).0,
            Mode::Hybrid => {
                let (lexical, lexical_summaries) = split(self.lexical_in(query, n, corpus, t)?);
                // Dense retrieval is best effort in hybrid mode: an unreachable embedder
                // degrades to lexical + symbols rather than failing the query.
                let (dense, dense_summaries) = match self.dense_in(query, n, corpus, t) {
                    Ok(d) => split(d),
                    Err(Error::Embed(_)) => Default::default(),
                    Err(e) => return Err(e),
                };
                let started = Instant::now();
                let symbol = self.keep(self.symbol_index()?.rank_query(query, n), n, corpus)?;
                t.symbol_ms += ms_since(started);
                let summaries = rrf(&[(1.0, &lexical_summaries), (1.0, &dense_summaries)], RRF_K);
                let fused = rrf(
                    &[(w.lexical, &lexical), (w.dense, &dense), (w.symbol, &symbol)],
                    RRF_K,
                );
                self.boost_communities(fused, &summaries, w.community)?
            }
        })
    }

    /// Raises code in the communities whose summaries match the query best: the summary
    /// says which part of the codebase a broad question is about, and the code the other
    /// rankings already found there moves up.
    fn boost_communities(&self, mut fused: Ranking, summaries: &Ranking, boost: f32) -> Result<Ranking> {
        let matched: Vec<u32> = summaries
            .iter()
            .take(BOOSTED_COMMUNITIES)
            .filter_map(|(id, _)| crate::graph::summary::community_of(id))
            .collect();
        if matched.is_empty() || boost == 0.0 {
            return Ok(fused);
        }
        for (id, score) in &mut fused {
            if let Some(c) = self.ledger.community_of_chunk(id)?
                && matched.contains(&c)
            {
                *score *= 1.0 + boost;
            }
        }
        fused.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(fused)
    }

    /// The most relevant code for `query`, expanded through the symbol graph, de-duplicated
    /// and packed into `token_budget` estimated tokens.
    pub(super) fn code_context(
        &self,
        query: &str,
        token_budget: usize,
        corpus: CorpusFilter,
        detail: Detail,
        t: &mut Timings,
    ) -> Result<ContextPack> {
        let hits = self.search_timed(
            query,
            &SearchOpts {
                top_k: CONTEXT_HITS,
                corpus,
                ..SearchOpts::default()
            },
            t,
        )?;
        let started = Instant::now();
        let symbols = self.symbol_index()?;

        // 1. Expand the best hits one hop through the symbol graph, preferring neighbours
        //    in the hit's own community.
        let mut scores: HashMap<String, f32> = hits.iter().map(|h| (h.chunk_id.clone(), h.score)).collect();
        for h in hits.iter().take(EXPAND_TOP) {
            let home = self.ledger.community_of_chunk(&h.chunk_id)?;
            for n in symbols.neighbours(&h.chunk_id, NEIGHBOURS_PER_HIT) {
                let same = home.is_none() || self.ledger.community_of_chunk(&n)? == home;
                let s = h.score * HOP_DECAY * if same { 1.0 } else { CROSS_COMMUNITY_DECAY };
                let e = scores.entry(n).or_insert(0.0);
                *e = e.max(s);
            }
        }

        // The hits keep their fused order: re-weighting spans by file PageRank (the
        // repository-map ranking) moved central files above the answer and cost 0.18 MRR on
        // code questions, so it is not applied (eval/RESULTS.md).
        let ranked: Vec<(String, f32)> = scores.into_iter().collect();
        let duplicates: HashMap<&str, Vec<String>> = hits
            .iter()
            .filter(|h| !h.duplicates.is_empty())
            .map(|h| (h.chunk_id.as_str(), h.duplicates.clone()))
            .collect();
        let retrieved: HashSet<&str> = hits.iter().map(|h| h.chunk_id.as_str()).collect();
        let mut spans: Vec<Hit> = self
            .hits(&ranked)?
            .into_iter()
            .map(|mut s| {
                // Spans are rebuilt from ids: the hits' copies come back from the search.
                if let Some(d) = duplicates.get(s.chunk_id.as_str()) {
                    s.duplicates = d.clone();
                }
                s
            })
            .collect();
        spans.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });
        // Neighbours can repeat the text of a hit or of each other; fold them while the
        // text is still whole.
        let spans: Vec<Hit> = dedup(spans)
            .into_iter()
            .map(|s| match detail {
                Detail::Map => outline(s),
                // A neighbour is context for a hit, not an answer: its outline is enough.
                Detail::Auto if !retrieved.contains(s.chunk_id.as_str()) => outline(s),
                Detail::Auto | Detail::Full => s,
            })
            .collect();
        t.expand_ms += ms_since(started);

        // 3. Pack greedily, then merge spans that touch within a file, so the prompt reads
        //    continuous code.
        Ok(pack(spans, token_budget))
    }
}
