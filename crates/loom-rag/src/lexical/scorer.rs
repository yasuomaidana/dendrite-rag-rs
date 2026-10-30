//! BM25 with configured parameters over Tantivy's postings: Tantivy fixes k1 and b and its
//! IDF, so a non-default [`Bm25Params`] scores each `(field, term)` with this query instead.
//! Field lengths are Tantivy's quantized field norms, as its own scorer uses.

use tantivy::fieldnorm::FieldNormReader;
use tantivy::postings::{Postings, SegmentPostings};
use tantivy::query::{
    BooleanQuery, BoostQuery, EmptyScorer, EnableScoring, Explanation, Occur, Query, Scorer, Weight,
};
use tantivy::schema::{Field, IndexRecordOption};
use tantivy::{DocId, DocSet, Score, SegmentReader, TantivyError, Term};

use crate::bm25::Bm25Params;

/// The bag-of-terms query the PostgreSQL BM25 also runs: every distinct code term of the
/// query, in every field, summed with the field's boost.
pub(super) fn bag_of_terms(fields: &[(Field, f32)], terms: &[String], params: Bm25Params) -> BooleanQuery {
    let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
    for &(field, boost) in fields {
        for t in terms {
            let q = Bm25TermQuery {
                term: Term::from_field_text(field, t),
                params,
            };
            clauses.push((Occur::Should, Box::new(BoostQuery::new(Box::new(q), boost))));
        }
    }
    BooleanQuery::new(clauses)
}

/// One term in one field, scored with `params`.
#[derive(Debug, Clone)]
struct Bm25TermQuery {
    term: Term,
    params: Bm25Params,
}

impl Query for Bm25TermQuery {
    fn weight(&self, scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        let (idf, avg_len) = match scoring {
            EnableScoring::Enabled {
                statistics_provider: stats,
                ..
            } => {
                let n = stats.total_num_docs()? as f64;
                let df = stats.doc_freq(&self.term)? as f64;
                let tokens = stats.total_num_tokens(self.term.field())? as f64;
                let avg = if n > 0.0 { (tokens / n) as f32 } else { 0.0 };
                (self.params.idf(n, df), avg)
            }
            EnableScoring::Disabled { .. } => (0.0, 0.0),
        };
        Ok(Box::new(Bm25TermWeight {
            term: self.term.clone(),
            params: self.params,
            idf,
            avg_len,
        }))
    }

    fn query_terms<'a>(&'a self, visitor: &mut dyn FnMut(&'a Term, bool)) {
        visitor(&self.term, false);
    }
}

struct Bm25TermWeight {
    term: Term,
    params: Bm25Params,
    idf: f32,
    avg_len: f32,
}

impl Weight for Bm25TermWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        let field = self.term.field();
        let postings = reader
            .inverted_index(field)?
            .read_postings(&self.term, IndexRecordOption::WithFreqs)?;
        let Some(postings) = postings else {
            return Ok(Box::new(EmptyScorer));
        };
        Ok(Box::new(Bm25TermScorer {
            postings,
            norms: reader.get_fieldnorms_reader(field)?,
            weight: self.idf * boost,
            params: self.params,
            avg_len: self.avg_len,
        }))
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        let mut scorer = self.scorer(reader, 1.0)?;
        if scorer.seek(doc) != doc {
            return Err(TantivyError::InvalidArgument(format!(
                "document {doc} does not match"
            )));
        }
        Ok(Explanation::new(
            "bm25 with configured parameters",
            scorer.score(),
        ))
    }
}

struct Bm25TermScorer {
    postings: SegmentPostings,
    norms: FieldNormReader,
    weight: f32,
    params: Bm25Params,
    avg_len: f32,
}

impl DocSet for Bm25TermScorer {
    fn advance(&mut self) -> DocId {
        self.postings.advance()
    }

    fn seek(&mut self, target: DocId) -> DocId {
        self.postings.seek(target)
    }

    fn doc(&self) -> DocId {
        self.postings.doc()
    }

    fn size_hint(&self) -> u32 {
        self.postings.size_hint()
    }
}

impl Scorer for Bm25TermScorer {
    fn score(&mut self) -> Score {
        let doc = self.postings.doc();
        let tf = self.postings.term_freq() as f32;
        let len = self.norms.fieldnorm(doc) as f32;
        self.weight * self.params.tf(tf, len, self.avg_len)
    }
}

#[cfg(test)]
mod tests {
    use super::super::LexicalIndex;
    use crate::bm25::Bm25Params;
    use crate::chunk::{ChunkOptions, chunk_file};
    use crate::lang::Lang;
    use rstest::{fixture, rstest};

    fn index(files: &[(&str, &str)]) -> (tempfile::TempDir, LexicalIndex) {
        let dir = tempfile::tempdir().unwrap();
        let idx = LexicalIndex::open(dir.path()).unwrap();
        let mut w = idx.writer().unwrap();
        for (path, src) in files {
            for c in chunk_file(path, Lang::Go, src, ChunkOptions::default()) {
                w.add(&c).unwrap();
            }
        }
        w.commit().unwrap();
        (dir, idx)
    }

    #[fixture]
    fn corpus() -> (tempfile::TempDir, LexicalIndex) {
        let long = format!(
            "package a\n\n// ledger ledger ledger\nfunc Long() {{\n{}}}\n",
            "\tx := compute(value, other, more, words, here)\n".repeat(30)
        );
        index(&[
            ("a/long.go", long.as_str()),
            ("b/short.go", "package b\n\n// ledger\nfunc Short() {}\n"),
            ("c/other.go", "package c\n\nfunc Unrelated() {}\n"),
        ])
    }

    #[rstest]
    fn matches_tantivy_scores_at_its_parameters(corpus: (tempfile::TempDir, LexicalIndex)) {
        let (_d, idx) = corpus;
        let tantivy = idx.search("ledger", 10, &Bm25Params::default()).unwrap();
        // Off the defaults by a rounding error: the bag-of-terms scorer runs instead.
        let ours = Bm25Params {
            k1: 1.2 + 1e-6,
            ..Bm25Params::default()
        };
        let custom = idx.search("ledger", 10, &ours).unwrap();
        assert_eq!(tantivy.len(), 2);
        assert_eq!(
            tantivy.iter().map(|(id, _)| id).collect::<Vec<_>>(),
            custom.iter().map(|(id, _)| id).collect::<Vec<_>>()
        );
        for ((_, a), (_, b)) in tantivy.iter().zip(&custom) {
            assert!((a - b).abs() < 1e-3 * a.abs(), "{a} vs {b}");
        }
    }

    #[rstest]
    fn length_normalization_follows_b(corpus: (tempfile::TempDir, LexicalIndex)) {
        let (_d, idx) = corpus;
        let top = |b: f32| {
            let p = Bm25Params {
                b,
                ..Bm25Params::default()
            };
            let hits = idx.search("ledger", 10, &p).unwrap();
            hits[0].0.clone()
        };
        let short = idx.search("Short", 1, &Bm25Params::default()).unwrap()[0]
            .0
            .clone();
        let long = idx.search("Long", 1, &Bm25Params::default()).unwrap()[0]
            .0
            .clone();
        assert_eq!(top(0.0), long, "without length normalization three mentions win");
        assert_eq!(top(1.0), short, "full normalization favours the short chunk");
    }

    #[rstest]
    fn small_q_weights_rare_terms_more(corpus: (tempfile::TempDir, LexicalIndex)) {
        let (_d, idx) = corpus;
        // `ledger` is in 2 of 3 chunks, `unrelated` in 1: a smaller q widens the gap.
        let gap = |q: f32| {
            let p = Bm25Params {
                q,
                k1: 1.2 + 1e-6,
                ..Bm25Params::default()
            };
            let rare = idx.search("unrelated", 1, &p).unwrap()[0].1;
            let common = idx.search("ledger", 1, &p).unwrap()[0].1;
            rare / common
        };
        assert!(gap(0.2) > gap(1.0), "{} vs {}", gap(0.2), gap(1.0));
    }
}
