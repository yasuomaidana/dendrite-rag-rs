//! Okapi BM25 parameters and formula, shared by the Tantivy scorer, the PostgreSQL query
//! and `rank` (`docs/specs/retrieval.md` § Lexical index).
//!
//! The IDF is Lucene's non-negative form, `ln(1 + (N - df + 0.5) / (df + 0.5))`, with the
//! outer logarithm generalized to the Tsallis q-logarithm
//! `ln_q(x) = (x^(1-q) - 1) / (1 - q)`: `q = 1` is classic BM25 exactly, and `q < 1`
//! separates very rare terms (unique identifiers) from merely rare ones more sharply.

use serde::{Deserialize, Serialize};

/// BM25 parameters (`[lexical]` in `.dendrite/rag.toml`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Bm25Params {
    /// Term-frequency saturation.
    pub k1: f32,
    /// Document-length normalization, 0 (none) to 1 (full).
    pub b: f32,
    /// Exponent of the q-logarithm in the IDF; 1 is the natural logarithm.
    pub q: f32,
}

impl Default for Bm25Params {
    fn default() -> Self {
        Self {
            k1: 1.2,
            b: 0.75,
            q: 1.0,
        }
    }
}

impl Bm25Params {
    /// Whether these are Tantivy's built-in parameters, which its own scorer applies.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Rejects values outside the ranges the formula is defined for.
    pub fn validate(&self) -> Result<(), String> {
        if !(self.k1 >= 0.0 && self.k1.is_finite()) {
            return Err(format!("bm25 k1 must be >= 0, got {}", self.k1));
        }
        if !(0.0..=1.0).contains(&self.b) {
            return Err(format!("bm25 b must be within 0..=1, got {}", self.b));
        }
        if !(self.q > 0.0 && self.q <= 1.0) {
            return Err(format!("bm25 q must be within (0, 1], got {}", self.q));
        }
        Ok(())
    }

    /// Inverse document frequency of a term in `df` of `n` documents.
    pub fn idf(&self, n: f64, df: f64) -> f32 {
        ln_q(1.0 + (n - df + 0.5) / (df + 0.5), self.q as f64) as f32
    }

    /// The term-frequency factor for `tf` occurrences in a field of `len` tokens, where
    /// fields average `avg_len` tokens.
    pub fn tf(&self, tf: f32, len: f32, avg_len: f32) -> f32 {
        let norm = if avg_len > 0.0 {
            1.0 - self.b + self.b * len / avg_len
        } else {
            1.0
        };
        tf * (self.k1 + 1.0) / (tf + self.k1 * norm)
    }
}

/// The Tsallis q-logarithm; the natural logarithm at `q = 1`.
pub fn ln_q(x: f64, q: f64) -> f64 {
    if (q - 1.0).abs() < 1e-9 {
        x.ln()
    } else {
        (x.powf(1.0 - q) - 1.0) / (1.0 - q)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn q_one_is_the_classic_idf() {
        let p = Bm25Params::default();
        let classic = (1.0 + (1000.0 - 3.0 + 0.5) / (3.0 + 0.5f64)).ln() as f32;
        assert!((p.idf(1000.0, 3.0) - classic).abs() < 1e-6);
        assert!(
            (ln_q(5.0, 0.999_999_999_9) - 5f64.ln()).abs() < 1e-6,
            "continuous at 1"
        );
    }

    #[test]
    fn small_q_separates_rare_terms_more() {
        let classic = Bm25Params::default();
        let q = Bm25Params {
            q: 0.1,
            ..Bm25Params::default()
        };
        let ratio = |p: &Bm25Params| p.idf(50_000.0, 1.0) / p.idf(50_000.0, 50.0);
        assert!(
            ratio(&q) > 2.0 * ratio(&classic),
            "{} vs {}",
            ratio(&q),
            ratio(&classic)
        );
    }

    #[test]
    fn tf_saturates_and_b_zero_ignores_length() {
        let p = Bm25Params::default();
        assert!(p.tf(100.0, 10.0, 10.0) < p.k1 + 1.0);
        let flat = Bm25Params { b: 0.0, ..p };
        assert_eq!(flat.tf(2.0, 5.0, 10.0), flat.tf(2.0, 50.0, 10.0));
        assert!(Bm25Params { b: 1.5, ..p }.validate().is_err());
        assert!(Bm25Params { q: 0.0, ..p }.validate().is_err());
    }
}
