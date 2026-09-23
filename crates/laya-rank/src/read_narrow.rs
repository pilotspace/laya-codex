//! The guarded `PreToolUse(Read)` rewrite (`docs/architecture.md` §3.5, D3): narrow a large-file
//! Read to the ranked range, but only when we have real evidence (a Laya probability ≥ threshold
//! for that file) — never on lexical-only ranking, where we stay conservative and pass through.

use laya_core::{QueryResult, RankMode};

/// Thresholds for the guarded Read rewrite.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadPolicy {
    /// Only narrow files strictly larger than this many lines.
    pub min_file_lines: u32,
    /// Minimum Laya probability a span in the target file must have to count as evidence.
    pub p_threshold: f32,
    /// Lines of context to pad on each side of the qualifying range.
    pub context_lines: u32,
    /// Hard cap on the rewritten `limit`.
    pub max_lines: u32,
}

impl Default for ReadPolicy {
    fn default() -> Self {
        Self {
            min_file_lines: 300,
            p_threshold: 0.7,
            context_lines: 5,
            max_lines: 200,
        }
    }
}

/// Compute a narrowed `(offset, limit)` for a `Read` of `path` (a `total_lines`-line file), or
/// `None` to leave the Read untouched. Rewrites only when *all* hold:
/// - `total_lines > policy.min_file_lines`;
/// - the query actually ran Laya (`result.mode == RankMode::Laya`) — a lexical-only ranking is
///   not evidence enough, so we always stay conservative and return `None` when Laya didn't run;
/// - at least one span in `path` has `p_relevant >= policy.p_threshold`.
///
/// The range covers `min(start) - context_lines ..= max(end) + context_lines` over the
/// qualifying spans, clamped to the file and capped at `policy.max_lines`.
pub fn read_narrowing(
    result: &QueryResult,
    path: &str,
    total_lines: u32,
    policy: &ReadPolicy,
) -> Option<(u32, u32)> {
    if total_lines <= policy.min_file_lines {
        return None;
    }
    if result.mode != RankMode::Laya {
        return None; // stay conservative: no calibrated evidence to narrow on
    }

    let mut min_start = None;
    let mut max_end = 0u32;
    for span in result.spans.iter().filter(|s| s.path == path) {
        let Some(p) = span.p_relevant else { continue };
        if p < policy.p_threshold {
            continue;
        }
        min_start = Some(min_start.map_or(span.start_line, |m: u32| m.min(span.start_line)));
        max_end = max_end.max(span.end_line);
    }
    let min_start = min_start?;

    let offset = min_start.saturating_sub(policy.context_lines).max(1);
    let mut end = max_end
        .saturating_add(policy.context_lines)
        .min(total_lines);
    let mut limit = end - offset + 1;
    if limit > policy.max_lines {
        limit = policy.max_lines;
        end = offset + limit - 1;
    }
    let _ = end; // kept for clarity of derivation; offset+limit is the returned contract
    Some((offset, limit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use laya_core::RankedSpan;

    fn span(path: &str, start: u32, end: u32, p: Option<f32>) -> RankedSpan {
        RankedSpan {
            path: path.to_string(),
            start_line: start,
            end_line: end,
            symbol: String::new(),
            p_relevant: p,
            score: p.unwrap_or(0.0),
            text: String::new(),
        }
    }

    fn result(mode: RankMode, spans: Vec<RankedSpan>) -> QueryResult {
        let candidates = spans.len();
        QueryResult {
            spans,
            mode,
            elapsed_ms: 1,
            candidates,
            related: Vec::new(),
        }
    }

    #[test]
    fn small_file_is_never_narrowed() {
        let r = result(RankMode::Laya, vec![span("a.rs", 10, 20, Some(0.9))]);
        assert_eq!(
            read_narrowing(&r, "a.rs", 100, &ReadPolicy::default()),
            None
        );
    }

    #[test]
    fn lexical_mode_never_narrows_even_with_high_lexical_rank() {
        // Laya never ran, so p_relevant is None on every span: stay conservative.
        let r = result(RankMode::Lexical, vec![span("a.rs", 10, 20, None)]);
        assert_eq!(
            read_narrowing(&r, "a.rs", 1000, &ReadPolicy::default()),
            None
        );
    }

    #[test]
    fn below_threshold_probability_does_not_narrow() {
        let r = result(RankMode::Laya, vec![span("a.rs", 10, 20, Some(0.4))]);
        assert_eq!(
            read_narrowing(&r, "a.rs", 1000, &ReadPolicy::default()),
            None
        );
    }

    #[test]
    fn qualifying_span_narrows_with_context_padding() {
        let r = result(RankMode::Laya, vec![span("a.rs", 100, 120, Some(0.8))]);
        let got = read_narrowing(&r, "a.rs", 1000, &ReadPolicy::default()).unwrap();
        assert_eq!(got, (95, 31)); // 100-5 .. 120+5 inclusive => offset 95, limit 31
    }

    #[test]
    fn offset_never_goes_below_line_one() {
        let r = result(RankMode::Laya, vec![span("a.rs", 2, 10, Some(0.9))]);
        let (offset, _) = read_narrowing(&r, "a.rs", 1000, &ReadPolicy::default()).unwrap();
        assert_eq!(offset, 1);
    }

    #[test]
    fn covers_multiple_qualifying_spans_in_the_same_file() {
        let r = result(
            RankMode::Laya,
            vec![
                span("a.rs", 400, 420, Some(0.9)),
                span("a.rs", 100, 110, Some(0.75)),
            ],
        );
        // Uncapped policy here so the assertions isolate "covers the union of qualifying
        // spans" from the separate max_lines-cap behavior (see `caps_the_limit_at_policy_max_lines`).
        let policy = ReadPolicy {
            max_lines: 10_000,
            ..ReadPolicy::default()
        };
        let (offset, limit) = read_narrowing(&r, "a.rs", 1000, &policy).unwrap();
        assert_eq!(offset, 95); // min(100) - 5
        assert_eq!(offset + limit - 1, 425); // max(420) + 5
    }

    #[test]
    fn ignores_spans_from_other_files() {
        let r = result(
            RankMode::Laya,
            vec![
                span("b.rs", 1, 900, Some(0.99)),
                span("a.rs", 100, 110, Some(0.4)),
            ],
        );
        assert_eq!(
            read_narrowing(&r, "a.rs", 1000, &ReadPolicy::default()),
            None
        );
    }

    #[test]
    fn caps_the_limit_at_policy_max_lines() {
        let r = result(RankMode::Laya, vec![span("a.rs", 1, 800, Some(0.9))]);
        let (offset, limit) = read_narrowing(&r, "a.rs", 1000, &ReadPolicy::default()).unwrap();
        assert_eq!(limit, 200);
        assert_eq!(offset, 1);
    }

    #[test]
    fn end_is_clamped_to_file_length() {
        let r = result(RankMode::Laya, vec![span("a.rs", 395, 400, Some(0.9))]);
        let (offset, limit) = read_narrowing(&r, "a.rs", 400, &ReadPolicy::default()).unwrap();
        assert_eq!(offset + limit - 1, 400);
    }
}
