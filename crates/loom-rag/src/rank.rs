//! Hybrid ranking of documents the caller supplies, with no project index
//! (`docs/specs/retrieval.md` § Ranking supplied documents): code-aware BM25 over their
//! texts, cosine similarity with the project's embedder, weighted RRF; for example, an agent
//! catalogue ranked against a task.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::bm25::Bm25Params;
use crate::engine::{Engine, Error, Result, cache_key};
use crate::fusion::{RRF_K, rrf};
use crate::lexical::{code_terms, sanitize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ranked {
    pub id: String,
    /// Weighted RRF of the lexical and dense rankings.
    pub score: f32,
    /// BM25 score; 0 when no query term occurs.
    pub lexical: f32,
    /// Cosine similarity; absent when the embedder is off or unreachable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dense: Option<f32>,
}

/// Okapi BM25 of each document for `query`, over the code tokenizer's terms.
fn bm25(documents: &[Document], query: &str, params: &Bm25Params) -> Vec<f32> {
    let docs: Vec<HashMap<String, f32>> = documents
        .iter()
        .map(|d| {
            let mut tf = HashMap::new();
            for t in code_terms(&d.text) {
                *tf.entry(t).or_insert(0.0) += 1.0;
            }
            tf
        })
        .collect();
    let lens: Vec<f32> = docs.iter().map(|tf| tf.values().sum()).collect();
    let n = docs.len() as f32;
    let avg = (lens.iter().sum::<f32>() / n.max(1.0)).max(1.0);
    let mut terms = code_terms(&sanitize(query));
    terms.sort();
    terms.dedup();
    docs.iter()
        .zip(&lens)
        .map(|(tf, len)| {
            terms
                .iter()
                .filter_map(|t| {
                    let f = *tf.get(t)?;
                    let df = docs.iter().filter(|d| d.contains_key(t)).count() as f64;
                    Some(params.idf(n as f64, df) * params.tf(f, *len, avg))
                })
                .sum()
        })
        .collect()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
    let d = norm(a) * norm(b);
    if d == 0.0 { 0.0 } else { dot / d }
}

impl Engine {
    /// Ranks `documents` against `query`, best first; at most `top_k` (all when 0). Document
    /// embeddings are cached by content like chunk embeddings, so a fixed catalogue is
    /// embedded once. An unreachable embedder leaves a lexical ranking.
    pub fn rank(&self, documents: &[Document], query: &str, top_k: usize) -> Result<Vec<Ranked>> {
        let lexical = bm25(documents, query, &self.cfg.bm25);
        let dense = match self.embed_documents_cached(documents) {
            Ok(Some(vectors)) => match self.embed_query(query) {
                Ok(Some(q)) => Some(vectors.iter().map(|v| cosine(&q, v)).collect::<Vec<f32>>()),
                Ok(None) | Err(Error::Embed(_)) => None,
                Err(e) => return Err(e),
            },
            Ok(None) | Err(Error::Embed(_)) => None,
            Err(e) => return Err(e),
        };
        let ranking = |scores: &[f32]| -> Vec<(String, f32)> {
            let mut r: Vec<(String, f32)> = documents
                .iter()
                .zip(scores)
                .map(|(d, &s)| (d.id.clone(), s))
                .collect();
            r.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            r
        };
        let w = self.fusion_weights();
        let lexical_ranking = ranking(&lexical);
        let dense_ranking = dense.as_deref().map(ranking).unwrap_or_default();
        let fused = rrf(&[(w.lexical, &lexical_ranking), (w.dense, &dense_ranking)], RRF_K);
        let by_id: HashMap<&str, usize> = documents
            .iter()
            .enumerate()
            .map(|(i, d)| (d.id.as_str(), i))
            .collect();
        let mut out: Vec<Ranked> = fused
            .into_iter()
            .map(|(id, score)| {
                let i = by_id[id.as_str()];
                Ranked {
                    lexical: lexical[i],
                    dense: dense.as_ref().map(|d| d[i]),
                    id,
                    score,
                }
            })
            .collect();
        if top_k > 0 {
            out.truncate(top_k);
        }
        Ok(out)
    }

    /// Embeddings of the documents' texts, from the cache where possible; `None` without
    /// an embedder.
    fn embed_documents_cached(&self, documents: &[Document]) -> Result<Option<Vec<Vec<f32>>>> {
        let Some(embedder) = &self.embedder else {
            return Ok(None);
        };
        let model = embedder.model_id().to_string();
        let keys: Vec<String> = documents
            .iter()
            .map(|d| cache_key(&model, &blake3::hash(d.text.as_bytes()).to_hex()[..32]))
            .collect();
        let mut vectors = self.ledger.embeddings(&keys)?;
        let missing: Vec<usize> = (0..documents.len()).filter(|&i| vectors[i].is_none()).collect();
        if !missing.is_empty() {
            let texts: Vec<String> = missing.iter().map(|&i| documents[i].text.clone()).collect();
            let fresh = embedder.embed_documents(&texts)?;
            let mut batch = self.ledger.batch()?;
            for (&i, v) in missing.iter().zip(fresh) {
                batch.put_embedding(&keys[i], &v)?;
                vectors[i] = Some(v);
            }
            batch.commit()?;
        }
        Ok(Some(vectors.into_iter().map(|v| v.unwrap_or_default()).collect()))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::*;
    use crate::engine::Config;
    use crate::testutil::HashEmbedder;
    use rstest::{fixture, rstest};

    #[fixture]
    fn catalogue() -> Vec<Document> {
        [
            (
                "aider",
                "rapid file editing git commits test driven development refactoring",
            ),
            (
                "openhands",
                "containerized sandbox full-stack feature implementation browser debugging",
            ),
            (
                "agy",
                "macro architecture system design security invariants token governance",
            ),
        ]
        .iter()
        .map(|(id, text)| Document {
            id: id.to_string(),
            text: text.to_string(),
        })
        .collect()
    }

    #[rstest]
    fn ranks_supplied_documents_and_caches_their_embeddings(catalogue: Vec<Document>) {
        let dir = tempfile::tempdir().unwrap();
        let embedder = HashEmbedder::new();
        let calls = embedder.calls.clone();
        let e = Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(embedder))).unwrap();
        let r = e.rank(&catalogue, "design the security architecture", 0).unwrap();
        assert_eq!(r[0].id, "agy");
        assert_eq!(r.len(), 3);
        assert!(r[0].lexical > 0.0 && r[0].dense.is_some());
        let after_first = calls.load(Ordering::SeqCst);
        e.rank(&catalogue, "refactoring with git commits", 1).unwrap();
        // Only the new query is embedded: the catalogue comes from the cache.
        assert_eq!(calls.load(Ordering::SeqCst), after_first + 1);
        assert_eq!(
            e.rank(&catalogue, "refactoring with git commits", 1).unwrap()[0].id,
            "aider"
        );
    }

    #[rstest]
    fn falls_back_to_lexical_without_an_embedder(catalogue: Vec<Document>) {
        let dir = tempfile::tempdir().unwrap();
        let e = Engine::open_with_embedder(dir.path(), Config::default(), None).unwrap();
        let r = e.rank(&catalogue, "browser sandbox", 2).unwrap();
        assert_eq!(r[0].id, "openhands");
        assert!(r[0].dense.is_none());
        assert_eq!(r.len(), 2);
    }
}
