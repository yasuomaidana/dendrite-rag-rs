//! The documentation pipeline: lexical and dense rankings over prose, fused with weighted
//! RRF. Prose defines no symbols, so there is no symbol-name ranking, no symbol-graph
//! expansion and no PageRank; spans keep their heading path as their scope.

use super::{
    CONTEXT_HITS, ContextPack, Detail, FusionWeights, Mode, Ranking, SearchOpts, Timings, outline, pack,
};
use crate::corpus::CorpusFilter;
use crate::engine::{Engine, Error, Hit, Result};
use crate::fusion::{RRF_K, rrf};

impl Engine {
    /// Fused ranking of documentation chunks.
    pub(super) fn docs_ranking(
        &self,
        query: &str,
        n: usize,
        mode: Mode,
        w: FusionWeights,
        t: &mut Timings,
    ) -> Result<Ranking> {
        let docs = CorpusFilter::Docs;
        Ok(match mode {
            Mode::Lexical => self.lexical_in(query, n, docs, t)?,
            Mode::Dense => self.dense_in(query, n, docs, t)?,
            Mode::Hybrid => {
                let lexical = self.lexical_in(query, n, docs, t)?;
                // As in the code pipeline, an unreachable embedder degrades to lexical.
                let dense = match self.dense_in(query, n, docs, t) {
                    Ok(d) => d,
                    Err(Error::Embed(_)) => Vec::new(),
                    Err(e) => return Err(e),
                };
                rrf(&[(w.lexical, &lexical), (w.dense, &dense)], RRF_K)
            }
        })
    }

    /// The documentation sections that match `query` best, with touching sections of a
    /// file merged, packed into `token_budget` estimated tokens.
    pub(super) fn docs_context(
        &self,
        query: &str,
        token_budget: usize,
        detail: Detail,
        t: &mut Timings,
    ) -> Result<ContextPack> {
        Ok(pack(self.docs_spans(query, detail, t)?, token_budget))
    }

    /// The documentation sections a docs pack is packed from, best first, at `detail`.
    pub(super) fn docs_spans(&self, query: &str, detail: Detail, t: &mut Timings) -> Result<Vec<Hit>> {
        let hits = self.search_timed(
            query,
            &SearchOpts {
                top_k: CONTEXT_HITS,
                corpus: CorpusFilter::Docs,
                ..SearchOpts::default()
            },
            t,
        )?;
        Ok(match detail {
            Detail::Map => hits.into_iter().map(outline).collect(),
            Detail::Auto | Detail::Full => hits,
        })
    }
}
