//! Span shaping: merge adjacent/overlapping chunks from the same file, cap to `top_n`, then
//! enforce the total-line budget across the final set (§2d of the task brief).

use std::collections::HashMap;

use laya_core::{Chunk, RankedSpan};

/// A scored candidate chunk, ready for span shaping.
#[derive(Debug, Clone)]
pub(crate) struct Scored {
    pub chunk: Chunk,
    pub score: f32,
    pub p_relevant: Option<f32>,
}

/// Chunks from the same file are merged when the gap between them is at most this many lines.
const MAX_MERGE_GAP: i64 = 3;
/// Merged spans never exceed this many lines (even if the merge candidates would fit the gap).
const MAX_MERGED_LINES: u32 = 50;

#[derive(Debug, Clone)]
struct Merged {
    path: String,
    start_line: u32,
    end_line: u32,
    symbol: String,
    text: String,
    score: f32,
    p_relevant: Option<f32>,
}

impl Merged {
    fn from_scored(s: Scored) -> Self {
        Merged {
            path: s.chunk.path,
            start_line: s.chunk.start_line,
            end_line: s.chunk.end_line,
            symbol: s.chunk.symbol,
            text: s.chunk.text,
            score: s.score,
            p_relevant: s.p_relevant,
        }
    }

    fn line_count(&self) -> u32 {
        self.end_line.saturating_sub(self.start_line) + 1
    }
}

/// Merge -> keep top_n (by score desc) -> enforce the total line budget (truncating tail spans).
/// Output order is stable: score descending.
pub(crate) fn shape_spans(
    scored: Vec<Scored>,
    top_n: usize,
    max_total_lines: u32,
) -> Vec<RankedSpan> {
    let mut merged = merge_same_file(scored);
    merged.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    merged.truncate(top_n);
    enforce_line_budget(merged, max_total_lines)
}

fn merge_same_file(scored: Vec<Scored>) -> Vec<Merged> {
    let mut by_path: HashMap<String, Vec<Merged>> = HashMap::new();
    for s in scored {
        by_path
            .entry(s.chunk.path.clone())
            .or_default()
            .push(Merged::from_scored(s));
    }

    let mut out = Vec::new();
    for (_path, mut group) in by_path {
        group.sort_by_key(|m| (m.start_line, m.end_line));
        let mut merged_group: Vec<Merged> = Vec::new();
        for m in group {
            let mergeable = merged_group.last().is_some_and(|last| {
                let gap = m.start_line as i64 - last.end_line as i64 - 1;
                let would_be_len = last.end_line.max(m.end_line) - last.start_line + 1;
                gap <= MAX_MERGE_GAP && would_be_len <= MAX_MERGED_LINES
            });
            if mergeable {
                merge_into(merged_group.last_mut().expect("checked above"), m);
            } else {
                merged_group.push(m);
            }
        }
        out.extend(merged_group);
    }
    out
}

/// Fold `next` (which starts at or after `last.start_line`, per the caller's sort) into `last`.
fn merge_into(last: &mut Merged, next: Merged) {
    let overlap = last.end_line as i64 - next.start_line as i64 + 1;
    if overlap > 0 {
        let skip = overlap as usize;
        let remainder: Vec<&str> = next.text.split('\n').skip(skip).collect();
        if !remainder.is_empty() {
            last.text.push('\n');
            last.text.push_str(&remainder.join("\n"));
        }
    } else {
        last.text.push('\n');
        last.text.push_str(&next.text);
    }
    last.end_line = last.end_line.max(next.end_line);
    last.score = last.score.max(next.score);
    last.p_relevant = match (last.p_relevant, next.p_relevant) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
    if last.symbol.is_empty() {
        last.symbol = next.symbol;
    }
}

fn enforce_line_budget(spans: Vec<Merged>, max_total_lines: u32) -> Vec<RankedSpan> {
    let mut out = Vec::new();
    let mut used: u32 = 0;
    for span in spans {
        let remaining = max_total_lines.saturating_sub(used);
        if remaining == 0 {
            break;
        }
        let lines = span.line_count();
        if lines <= remaining {
            used += lines;
            out.push(to_ranked(span));
        } else {
            let truncated = truncate_span(span, remaining);
            out.push(to_ranked(truncated));
            break; // budget exhausted: drop the rest of the tail
        }
    }
    out
}

fn truncate_span(mut span: Merged, keep_lines: u32) -> Merged {
    let keep = keep_lines.max(1) as usize;
    let kept: Vec<&str> = span.text.split('\n').take(keep).collect();
    span.text = kept.join("\n");
    span.end_line = span.start_line + keep_lines.saturating_sub(1);
    span
}

fn to_ranked(m: Merged) -> RankedSpan {
    RankedSpan {
        path: m.path,
        start_line: m.start_line,
        end_line: m.end_line,
        symbol: m.symbol,
        p_relevant: m.p_relevant,
        score: m.score,
        text: m.text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use laya_core::Lang;

    fn chunk(path: &str, start: u32, end: u32, text: &str) -> Chunk {
        Chunk {
            path: path.to_string(),
            start_line: start,
            end_line: end,
            lang: Lang::Rust,
            symbol: String::new(),
            kind: "function_item".to_string(),
            defines: vec![],
            text: text.to_string(),
        }
    }

    fn scored(path: &str, start: u32, end: u32, text: &str, score: f32) -> Scored {
        Scored {
            chunk: chunk(path, start, end, text),
            score,
            p_relevant: Some(score),
        }
    }

    #[test]
    fn merges_adjacent_chunks_within_gap() {
        // gap of 2 lines (end=10, next start=13) is within the <=3 threshold.
        let a = scored("a.rs", 1, 10, "L1..L10", 0.9);
        let b = scored("a.rs", 13, 20, "L13..L20", 0.8);
        let out = shape_spans(vec![a, b], 10, 400);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].start_line, 1);
        assert_eq!(out[0].end_line, 20);
    }

    #[test]
    fn does_not_merge_chunks_beyond_gap() {
        let a = scored("a.rs", 1, 10, "L1..L10", 0.9);
        let b = scored("a.rs", 20, 30, "L20..L30", 0.8);
        let out = shape_spans(vec![a, b], 10, 400);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn does_not_merge_across_files() {
        let a = scored("a.rs", 1, 10, "a", 0.9);
        let b = scored("b.rs", 1, 10, "b", 0.8);
        let out = shape_spans(vec![a, b], 10, 400);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn does_not_merge_when_result_would_exceed_50_lines() {
        let a = scored("a.rs", 1, 40, "a", 0.9);
        let b = scored("a.rs", 41, 60, "b", 0.8); // adjacent (gap 0) but merged len = 60 > 50
        let out = shape_spans(vec![a, b], 10, 400);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn merges_overlapping_chunks_without_duplicating_text() {
        let a = scored("a.rs", 1, 5, "l1\nl2\nl3\nl4\nl5", 0.9);
        let b = scored("a.rs", 4, 8, "l4\nl5\nl6\nl7\nl8", 0.8);
        let out = shape_spans(vec![a, b], 10, 400);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].start_line, 1);
        assert_eq!(out[0].end_line, 8);
        assert_eq!(out[0].text, "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8");
    }

    #[test]
    fn merged_span_keeps_max_score_and_p_relevant() {
        let a = scored("a.rs", 1, 5, "a", 0.4);
        let b = scored("a.rs", 6, 10, "b", 0.9);
        let out = shape_spans(vec![a, b], 10, 400);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].score, 0.9);
        assert_eq!(out[0].p_relevant, Some(0.9));
    }

    #[test]
    fn keeps_only_top_n_by_score() {
        let spans: Vec<Scored> = (0..5)
            .map(|i| scored(&format!("f{i}.rs"), 1, 5, "x", i as f32))
            .collect();
        let out = shape_spans(spans, 2, 400);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].score, 4.0);
        assert_eq!(out[1].score, 3.0);
    }

    #[test]
    fn output_order_is_score_descending() {
        let a = scored("a.rs", 1, 5, "a", 0.2);
        let b = scored("b.rs", 1, 5, "b", 0.9);
        let c = scored("c.rs", 1, 5, "c", 0.5);
        let out = shape_spans(vec![a, b, c], 10, 400);
        let scores: Vec<f32> = out.iter().map(|s| s.score).collect();
        assert_eq!(scores, vec![0.9, 0.5, 0.2]);
    }

    #[test]
    fn enforces_total_line_budget_by_truncating_tail_span() {
        let a_text = "x\n".repeat(29) + "x";
        let b_text = "y\n".repeat(29) + "y";
        let a = scored("a.rs", 1, 30, &a_text, 0.9); // 30 lines
        let b = scored("b.rs", 1, 30, &b_text, 0.8); // 30 lines
        let out = shape_spans(vec![a, b], 10, 40); // budget only fits 30 + 10
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].start_line, 1);
        assert_eq!(out[0].end_line, 30);
        assert_eq!(out[1].start_line, 1);
        assert_eq!(out[1].end_line, 10); // truncated to fit remaining 10-line budget
        let total_lines: u32 = out.iter().map(|s| s.end_line - s.start_line + 1).sum();
        assert_eq!(total_lines, 40);
    }

    #[test]
    fn drops_spans_entirely_once_budget_is_exhausted() {
        let a_text = "x\n".repeat(39) + "x";
        let a = scored("a.rs", 1, 40, &a_text, 0.9);
        let b = scored("b.rs", 1, 10, "y", 0.8);
        let out = shape_spans(vec![a, b], 10, 40); // "a" alone consumes the whole budget
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "a.rs");
    }

    #[test]
    fn a_single_very_large_chunk_passes_through_unmerged() {
        let big = scored("a.rs", 1, 500, "big", 0.9);
        let out = shape_spans(vec![big], 10, 1000);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].end_line, 500);
    }

    #[test]
    fn empty_input_yields_empty_output() {
        let out = shape_spans(vec![], 10, 400);
        assert!(out.is_empty());
    }
}
