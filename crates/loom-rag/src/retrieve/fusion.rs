//! Weighted Reciprocal Rank Fusion (`docs/specs/retrieval.md` § Fusion).

use std::collections::HashMap;

/// Default RRF smoothing constant (Cormack et al., 2009).
pub const RRF_K: f32 = 60.0;

/// Fuses ranked lists: `score(d) = Σ_i w_i / (k + rank_i(d))`, ranks starting at 1.
/// Only ranks matter, so lists with incomparable scores (BM25, cosine) combine without
/// calibration. Ties are broken by id for determinism.
pub fn rrf(lists: &[(f32, &[(String, f32)])], k: f32) -> Vec<(String, f32)> {
    let mut scores: HashMap<&str, f32> = HashMap::new();
    for (weight, list) in lists {
        for (rank, (id, _)) in list.iter().enumerate() {
            *scores.entry(id.as_str()).or_default() += weight / (k + rank as f32 + 1.0);
        }
    }
    let mut out: Vec<(String, f32)> = scores.into_iter().map(|(id, s)| (id.to_string(), s)).collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(ids: &[&str]) -> Vec<(String, f32)> {
        ids.iter().map(|id| (id.to_string(), 0.0)).collect()
    }

    #[test]
    fn items_ranked_well_in_several_lists_win() {
        let lexical = list(&["a", "b", "c"]);
        let dense = list(&["c", "a", "d"]);
        let fused = rrf(&[(1.0, &lexical), (1.0, &dense)], RRF_K);
        let ids: Vec<&str> = fused.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["a", "c", "b", "d"]);
        let expected_a = 1.0 / 61.0 + 1.0 / 62.0;
        assert!((fused[0].1 - expected_a).abs() < 1e-6);
    }

    #[test]
    fn weights_shift_the_order_and_ties_are_deterministic() {
        let lexical = list(&["a"]);
        let symbol = list(&["b"]);
        let fused = rrf(&[(1.0, &lexical), (1.5, &symbol)], RRF_K);
        assert_eq!(fused[0].0, "b");
        let fused = rrf(&[(1.0, &list(&["y"])), (1.0, &list(&["x"]))], RRF_K);
        assert_eq!(fused[0].0, "x");
        assert!(rrf(&[], RRF_K).is_empty());
    }
}
