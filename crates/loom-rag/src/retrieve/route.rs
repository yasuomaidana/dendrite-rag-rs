//! How strongly code and documentation match a query: the best dense and lexical scores of
//! each corpus and the rank-weighted share of documentation among the best candidates of
//! both. The evaluation uses it (`loom-eval probe`) to look for a rule that routes questions
//! to a corpus; on the golden sets none separates code from documentation questions without
//! sending some documentation questions to code alone, so the corpus stays the caller's
//! choice (eval/RESULTS.md).

use super::{Mode, Ranking, Timings};
use crate::corpus::{Corpus, CorpusFilter};
use crate::engine::{Engine, Error, Result};

/// Candidates per corpus the probe looks at.
const PROBE: usize = 10;

/// The best scores each corpus reaches for a query.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Probe {
    /// Best cosine similarity among code and among documentation chunks (`None` without
    /// vectors, or when the corpus is empty).
    pub dense_code: Option<f32>,
    pub dense_docs: Option<f32>,
    /// Best BM25 score in each corpus.
    pub lexical_code: Option<f32>,
    pub lexical_docs: Option<f32>,
    /// Share of documentation among the best hybrid candidates of both corpora, weighted by
    /// rank (1/(rank+1)): scale-free, so it holds for every embedder and without one.
    pub docs_share: f32,
}

fn best(r: &Ranking) -> Option<f32> {
    r.first().map(|(_, s)| *s)
}

impl Engine {
    /// Rank-weighted share of documentation among the top [`PROBE`] hybrid candidates.
    pub fn docs_share(&self, query: &str, t: &mut Timings) -> Result<f32> {
        let w = self.fusion_weights();
        let ranked = self.code_ranking(query, PROBE, Mode::Hybrid, w, CorpusFilter::All, t)?;
        let symbols = self.symbol_index()?;
        let (mut docs, mut all) = (0.0, 0.0);
        for (i, (id, _)) in ranked.iter().take(PROBE).enumerate() {
            let weight = 1.0 / (i as f32 + 1.0);
            all += weight;
            if symbols.corpus_of(id) == Some(Corpus::Docs) {
                docs += weight;
            }
        }
        Ok(if all > 0.0 { docs / all } else { 0.0 })
    }

    /// The best dense and lexical scores of code and of documentation for `query`.
    pub fn probe(&self, query: &str, t: &mut Timings) -> Result<Probe> {
        let dense = |corpus| match self.dense_in(query, PROBE, corpus, &mut Timings::default()) {
            Ok(r) => Ok(best(&r)),
            Err(Error::Embed(_)) => Ok(None),
            Err(e) => Err(e),
        };
        let probe = Probe {
            dense_code: dense(CorpusFilter::Code)?,
            dense_docs: dense(CorpusFilter::Docs)?,
            lexical_code: best(&self.lexical_in(query, PROBE, CorpusFilter::Code, t)?),
            lexical_docs: best(&self.lexical_in(query, PROBE, CorpusFilter::Docs, t)?),
            docs_share: self.docs_share(query, t)?,
        };
        Ok(probe)
    }
}
