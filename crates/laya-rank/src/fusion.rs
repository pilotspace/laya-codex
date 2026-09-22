//! Reciprocal Rank Fusion: combine several best-first ranked id lists into one ranking.
//!
//! This is the fusion the spike found best (`spike/laya_spike.py`, `rrf_*` rows): BM25 top-32
//! fused with the Laya rank via RRF beat either signal alone (MRR 0.591 vs BM25 0.480). We reuse
//! the same primitive both for candidate generation (BM25 ⊕ defining-chunks ⊕ path signal) and
//! for the Laya gate (lexical rank ⊕ Laya rank).

use std::collections::{HashMap, HashSet};

/// Fuse `signals` (each a best-first list of ids, possibly containing duplicates across lists
/// but not within a single list) by Reciprocal Rank Fusion: `score(id) = Σ 1 / (rrf_k + rank)`
/// over every signal that contains `id`, rank 0-based. Returns ids sorted by score descending;
/// ties keep the order the id was first seen in, scanning `signals` left to right (a stable
/// tie-break, not an arbitrary hash order).
pub fn fuse_ranked_lists(signals: &[&[String]], rrf_k: f32) -> Vec<(String, f32)> {
    let mut scores: HashMap<&str, f32> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();

    for signal in signals {
        for (rank, id) in signal.iter().enumerate() {
            *scores.entry(id.as_str()).or_insert(0.0) += 1.0 / (rrf_k + rank as f32);
            if seen.insert(id.as_str()) {
                order.push(id.clone());
            }
        }
    }

    let mut fused: Vec<(String, f32)> = order
        .into_iter()
        .map(|id| {
            let score = scores[id.as_str()];
            (id, score)
        })
        .collect();
    fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    fused
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn single_signal_preserves_its_order() {
        let a = v(&["x", "y", "z"]);
        let fused = fuse_ranked_lists(&[&a], 60.0);
        let ids: Vec<&str> = fused.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, vec!["x", "y", "z"]);
    }

    #[test]
    fn agreement_across_signals_wins() {
        // "b" is rank 1 in both lists; "a" and "c" only appear in one each. RRF should put the
        // item both signals agree on above either signal's sole top pick.
        let a = v(&["a", "b", "c"]);
        let b = v(&["b", "c", "a"]);
        // hand-rolled third signal so "b" is unambiguously boosted by appearing everywhere.
        let c = v(&["c", "b", "a"]);
        let fused = fuse_ranked_lists(&[&a, &b, &c], 60.0);
        assert_eq!(fused[0].0, "b");
    }

    #[test]
    fn empty_signals_yield_empty_fusion() {
        let empty: Vec<String> = vec![];
        let fused = fuse_ranked_lists(&[&empty, &empty], 60.0);
        assert!(fused.is_empty());
    }

    #[test]
    fn duplicate_across_signals_accumulates_score() {
        let a = v(&["x"]);
        let b = v(&["x"]);
        let solo = v(&["y"]);
        let fused_dup = fuse_ranked_lists(&[&a, &b], 60.0);
        let fused_solo = fuse_ranked_lists(&[&solo], 60.0);
        assert!(fused_dup[0].1 > fused_solo[0].1);
    }

    #[test]
    fn lower_rrf_k_sharpens_top_rank_dominance() {
        let a = v(&["x", "y"]);
        let sharp = fuse_ranked_lists(&[&a], 1.0);
        let flat = fuse_ranked_lists(&[&a], 60.0);
        let ratio_sharp = sharp[0].1 / sharp[1].1;
        let ratio_flat = flat[0].1 / flat[1].1;
        assert!(ratio_sharp > ratio_flat);
    }
}
