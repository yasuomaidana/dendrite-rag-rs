//! Context packs over code and documentation: each corpus through its own pipeline, with a
//! share of the budget kept for documentation. In one mixed ranking the code pipeline's hits
//! and their graph neighbours filled the budget before the documentation that answered the
//! question got in (Dendrite's AGENTS.md ranked 7th behind six code chunks at 2,500 tokens),
//! and each pipeline beats the mixed ranking on its own corpus (eval/RESULTS.md). The mixed
//! ranking still decides which corpus leads: the pipelines' scores are not comparable, and
//! alternating them put code first on documentation questions (MRR 0.40 against 0.83).

use std::collections::HashMap;

use super::{CANDIDATES, ContextPack, Detail, Mode, Timings, pack};
use crate::corpus::CorpusFilter;
use crate::engine::{Engine, Hit, Result};

/// Default share of a mixed pack's budget kept for documentation; room the documentation
/// leaves goes to code, and room code leaves goes back to documentation. It leans on code,
/// which documentation can contradict (eval/RESULTS.md).
pub const DEFAULT_DOCS_SHARE: f32 = 0.2;

impl Engine {
    /// A documentation pack within `docs_share` (clamped to 0..=1) of `token_budget` and a
    /// code pack in the rest, each in its own order, interleaved by the mixed ranking.
    pub(super) fn mixed_context(
        &self,
        query: &str,
        token_budget: usize,
        detail: Detail,
        docs_share: f32,
        t: &mut Timings,
    ) -> Result<ContextPack> {
        let docs_spans = self.docs_spans(query, detail, t)?;
        let share = if docs_share.is_nan() {
            DEFAULT_DOCS_SHARE
        } else {
            docs_share.clamp(0.0, 1.0)
        };
        let reserve = (token_budget as f32 * share) as usize;
        let mut docs = pack(docs_spans.clone(), reserve);
        let code = self.code_context(query, token_budget - docs.tokens, CorpusFilter::Code, detail, t)?;
        let room = token_budget - code.tokens;
        if room > docs.tokens {
            let wider = pack(docs_spans, room);
            if wider.tokens > docs.tokens {
                docs = wider;
            }
        }
        let joint = self.code_ranking(
            query,
            CANDIDATES,
            Mode::Hybrid,
            self.fusion_weights(),
            CorpusFilter::All,
            t,
        )?;
        let rank: HashMap<String, usize> = joint
            .into_iter()
            .enumerate()
            .map(|(i, (id, _))| (id, i))
            .collect();
        Ok(ContextPack {
            tokens: code.tokens + docs.tokens,
            spans: interleave(code.spans, docs.spans, |h| rank.get(&h.chunk_id).copied()),
            timings: Timings::default(),
        })
    }
}

/// Both lists, each in its own order, taking next whichever head ranks better by `rank`
/// (unranked heads last, code on ties).
fn interleave(code: Vec<Hit>, docs: Vec<Hit>, rank: impl Fn(&Hit) -> Option<usize>) -> Vec<Hit> {
    let mut out = Vec::with_capacity(code.len() + docs.len());
    let (mut code, mut docs) = (code.into_iter().peekable(), docs.into_iter().peekable());
    loop {
        let next = match (code.peek(), docs.peek()) {
            (None, None) => return out,
            (Some(_), None) => code.next(),
            (None, Some(_)) => docs.next(),
            (Some(c), Some(d)) => {
                let key = |h: &Hit| rank(h).unwrap_or(usize::MAX);
                if key(d) < key(c) { docs.next() } else { code.next() }
            }
        };
        out.extend(next);
    }
}
