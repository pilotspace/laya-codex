//! Tunables for [`crate::Retriever`], deserializable from the daemon/CLI config file. Every
//! field has a default so a partial (or empty) config document is valid.

use std::time::Duration;

use serde::Deserialize;

fn default_k_candidates() -> usize {
    24
}
fn default_top_n() -> usize {
    10
}
fn default_laya_budget() -> Duration {
    Duration::from_millis(1200)
}
fn default_p_threshold() -> f32 {
    0.5
}
fn default_min_keep() -> usize {
    3
}
fn default_max_total_lines() -> u32 {
    400
}
fn default_rrf_k() -> f32 {
    60.0
}
fn default_use_laya() -> bool {
    true
}

fn deserialize_millis<'de, D>(deserializer: D) -> std::result::Result<Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let ms = u64::deserialize(deserializer)?;
    Ok(Duration::from_millis(ms))
}

/// Retrieval tunables. Field names match `docs/architecture.md` §3.2–3.3; `laya_budget` is
/// expressed in the config document as milliseconds (see [`deserialize_millis`]).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RetrieverConfig {
    /// Candidates kept after RRF fusion, materialized via `Store::get_chunks`.
    #[serde(default = "default_k_candidates")]
    pub k_candidates: usize,
    /// Final spans returned to the caller.
    #[serde(default = "default_top_n")]
    pub top_n: usize,
    /// Hard wall-clock budget for the Laya scorer; on miss we fall back to `RankMode::Lexical`.
    #[serde(
        default = "default_laya_budget",
        deserialize_with = "deserialize_millis"
    )]
    pub laya_budget: Duration,
    /// Minimum Laya probability to keep a candidate (dropped below this unless `min_keep` bites).
    #[serde(default = "default_p_threshold")]
    pub p_threshold: f32,
    /// Never let the threshold drop the result set below this many spans.
    #[serde(default = "default_min_keep")]
    pub min_keep: usize,
    /// Total line budget across all returned spans, tail spans truncated to fit.
    #[serde(default = "default_max_total_lines")]
    pub max_total_lines: u32,
    /// Reciprocal Rank Fusion constant (higher = flatter blend across signals).
    #[serde(default = "default_rrf_k")]
    pub rrf_k: f32,
    /// If `false`, skip the Laya gate entirely and return the lexical (RRF) ranking.
    #[serde(default = "default_use_laya")]
    pub use_laya: bool,
    /// Weight of Laya's P(relevant) in the final score. `None` = rank-level RRF of lexical and
    /// Laya orders; `Some(w)` = `(1-w)·lexical_rank_score + w·P`, which uses the calibrated
    /// probability magnitudes instead of only their order.
    #[serde(default)]
    pub laya_weight: Option<f32>,
}

impl Default for RetrieverConfig {
    fn default() -> Self {
        Self {
            k_candidates: default_k_candidates(),
            top_n: default_top_n(),
            laya_budget: default_laya_budget(),
            p_threshold: default_p_threshold(),
            min_keep: default_min_keep(),
            max_total_lines: default_max_total_lines(),
            rrf_k: default_rrf_k(),
            use_laya: default_use_laya(),
            laya_weight: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_spec() {
        let cfg = RetrieverConfig::default();
        assert_eq!(cfg.k_candidates, 24);
        assert_eq!(cfg.top_n, 10);
        assert_eq!(cfg.laya_budget, Duration::from_millis(1200));
        assert_eq!(cfg.p_threshold, 0.5);
        assert_eq!(cfg.min_keep, 3);
        assert_eq!(cfg.max_total_lines, 400);
        assert_eq!(cfg.rrf_k, 60.0);
        assert!(cfg.use_laya);
    }

    #[test]
    fn empty_json_object_deserializes_to_defaults() {
        let cfg: RetrieverConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg, RetrieverConfig::default());
    }

    #[test]
    fn partial_json_overrides_only_named_fields() {
        let cfg: RetrieverConfig =
            serde_json::from_str(r#"{"top_n": 5, "laya_budget": 250, "use_laya": false}"#).unwrap();
        assert_eq!(cfg.top_n, 5);
        assert_eq!(cfg.laya_budget, Duration::from_millis(250));
        assert!(!cfg.use_laya);
        // untouched fields keep their defaults
        assert_eq!(cfg.k_candidates, 24);
        assert_eq!(cfg.p_threshold, 0.5);
    }
}
