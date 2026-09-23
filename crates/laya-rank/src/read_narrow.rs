//! The guarded `PreToolUse(Read)` rewrite (`docs/architecture.md` §3.5, D3): narrow a large-file
//! Read to the ranked range, but only when we have real evidence (a Laya probability ≥ threshold
//! for that file) — never on lexical-only ranking, where we stay conservative and pass through.

use laya_core::{Chunk, QueryResult, RankMode};

use crate::signals::PromptSignals;

/// Thresholds for the guarded Read rewrite.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadPolicy {
    /// Only narrow files with at least this many lines.
    pub min_file_lines: u32,
    /// Minimum Laya probability a span in the target file must have to count as evidence
    /// (used by [`read_narrowing`] only; [`ReadPolicy::read_region`] takes any ranked span).
    pub p_threshold: f32,
    /// Lines of context to pad on each side of the qualifying range.
    pub context_lines: u32,
    /// Hard cap on the rewritten `limit`.
    pub max_lines: u32,
    /// Leave the Read alone unless narrowing hides at least this many lines.
    pub min_saved_lines: u32,
    /// Outline cap: at most this many items.
    pub outline_items: usize,
    /// Outline cap: at most about this many characters.
    pub outline_chars: usize,
}

impl Default for ReadPolicy {
    fn default() -> Self {
        Self {
            min_file_lines: 250,
            p_threshold: 0.7,
            context_lines: 5,
            max_lines: 200,
            min_saved_lines: 100,
            outline_items: 40,
            outline_chars: 1500,
        }
    }
}

impl ReadPolicy {
    /// Region to show for the first whole-file Read of `path` (a `total_lines`-line file whose
    /// indexed `chunks` are current), as `(offset, limit, basis)`; `None` = leave the Read alone.
    ///
    /// 1. `basis = "ranking"`: the file's spans in the session's last ranking, at any probability
    ///    and in either mode (the agent chose to open a ranked file, which is evidence enough
    ///    when an outline and a guaranteed full re-read come with it).
    /// 2. `basis = "lexical"`: otherwise, the chunks whose symbol/definitions/text share the most
    ///    discriminating task terms (in-file IDF, so a term in every chunk counts 0), each
    ///    expanded to its enclosing item. No match → `None`: code is never hidden on a guess.
    ///
    /// The best candidate anchors the window; weaker ones join while the union fits in
    /// `max_lines`. Also `None` for files under `min_file_lines` or when the window would hide
    /// fewer than `min_saved_lines`.
    pub fn read_region(
        &self,
        ranking: Option<&QueryResult>,
        path: &str,
        chunks: &[Chunk],
        signals: &PromptSignals,
        total_lines: u32,
    ) -> Option<(u32, u32, &'static str)> {
        if total_lines < self.min_file_lines {
            return None;
        }
        let (cands, basis) = match ranked_candidates(ranking, path) {
            Some(c) => (c, "ranking"),
            None => (lexical_candidates(chunks, signals)?, "lexical"),
        };
        let (offset, limit) = self.window(&cands, total_lines)?;
        (total_lines - limit >= self.min_saved_lines).then_some((offset, limit, basis))
    }

    /// One line per item of the file (`  start-end label`; `*` marks items overlapping `window`
    /// = `(offset, limit)`), in file order. Items are chunks merged when consecutive chunks
    /// share a symbol, labelled by symbol path or, for anonymous chunks, their definitions
    /// (chunks with neither are skipped). Over the caps, shown items come first, then the
    /// best task matches, then file order; a final line counts what was left out.
    pub fn outline(&self, chunks: &[Chunk], signals: &PromptSignals, window: (u32, u32)) -> String {
        let (lo, hi) = (
            window.0,
            window.0.saturating_add(window.1).saturating_sub(1),
        );
        let scores = chunk_scores(chunks, signals);
        let items: Vec<(Item, f32)> = items(chunks)
            .into_iter()
            .filter(|it| !it.label.is_empty())
            .map(|it| {
                let s = scores[it.first..=it.last]
                    .iter()
                    .copied()
                    .fold(0.0, f32::max);
                (it, s)
            })
            .collect();
        let shown: Vec<bool> = items
            .iter()
            .map(|(it, _)| it.start <= hi && it.end >= lo)
            .collect();
        let lines: Vec<String> = items
            .iter()
            .zip(&shown)
            .map(|((it, _), &s)| {
                let mark = if s { '*' } else { ' ' };
                format!("{mark} {}-{} {}", it.start, it.end, clip(&it.label, 80))
            })
            .collect();
        let mut order: Vec<usize> = (0..items.len()).collect();
        order.sort_by(|&a, &b| {
            shown[b]
                .cmp(&shown[a])
                .then(items[b].1.total_cmp(&items[a].1))
                .then(a.cmp(&b))
        });
        let (mut keep, mut chars) = (Vec::new(), 0usize);
        for i in order {
            let len = lines[i].len() + 1;
            if keep.len() >= self.outline_items || chars + len > self.outline_chars {
                break;
            }
            chars += len;
            keep.push(i);
        }
        keep.sort_unstable();
        let mut out: Vec<String> = keep.iter().map(|&i| lines[i].clone()).collect();
        if keep.len() < items.len() {
            out.push(format!("  … {} more items", items.len() - keep.len()));
        }
        out.join("\n")
    }

    /// `(offset, limit)` covering the best candidate plus every weaker one whose union still
    /// fits, padded by `context_lines` and clamped to the file. A best candidate larger than
    /// `max_lines` (a giant item) is cut to `max_lines` starting at its anchor.
    fn window(&self, cands: &[Candidate], total_lines: u32) -> Option<(u32, u32)> {
        let best = cands.first()?;
        let budget = self.max_lines.saturating_sub(2 * self.context_lines).max(1);
        let (mut lo, mut hi) = (best.start, best.end);
        for c in &cands[1..] {
            let (l, h) = (lo.min(c.start), hi.max(c.end));
            if h - l < budget {
                (lo, hi) = (l, h);
            }
        }
        let start = lo.saturating_sub(self.context_lines).max(1);
        let end = hi.saturating_add(self.context_lines).min(total_lines);
        if start > end {
            return None;
        }
        if end - start < self.max_lines {
            return Some((start, end - start + 1));
        }
        let limit = self.max_lines.min(total_lines);
        let offset = best
            .anchor
            .saturating_sub(self.context_lines)
            .max(1)
            .min(total_lines - limit + 1);
        Some((offset, limit))
    }
}

/// A region worth showing, best first: its extent and the line the window must keep.
#[derive(Debug, Clone, Copy)]
struct Candidate {
    start: u32,
    end: u32,
    anchor: u32,
}

fn ranked_candidates(ranking: Option<&QueryResult>, path: &str) -> Option<Vec<Candidate>> {
    let mut spans: Vec<_> = ranking?.spans.iter().filter(|s| s.path == path).collect();
    spans.sort_by(|a, b| b.score.total_cmp(&a.score));
    let c: Vec<Candidate> = spans
        .iter()
        .filter(|s| s.start_line >= 1 && s.end_line >= s.start_line)
        .map(|s| Candidate {
            start: s.start_line,
            end: s.end_line,
            anchor: s.start_line,
        })
        .collect();
    (!c.is_empty()).then_some(c)
}

/// Chunks scoring at least half the best lexical score, best first, expanded to their items.
fn lexical_candidates(chunks: &[Chunk], signals: &PromptSignals) -> Option<Vec<Candidate>> {
    let scores = chunk_scores(chunks, signals);
    let best = scores.iter().copied().fold(0.0, f32::max);
    if best <= 0.0 {
        return None;
    }
    let items = items(chunks);
    let item_of = |i: usize| items.iter().find(|it| it.first <= i && i <= it.last);
    let mut idx: Vec<usize> = (0..chunks.len())
        .filter(|&i| scores[i] >= best * 0.5)
        .collect();
    idx.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]).then(a.cmp(&b)));
    Some(
        idx.into_iter()
            .map(|i| {
                let c = &chunks[i];
                let (start, end) =
                    item_of(i).map_or((c.start_line, c.end_line), |it| (it.start, it.end));
                Candidate {
                    start,
                    end,
                    anchor: c.start_line,
                }
            })
            .collect(),
    )
}

/// Lexical relevance of each chunk to the task: sum over the distinct task terms it contains of
/// the term's in-file IDF `ln(n / df)` (doubled when the term is in the chunk's symbol or
/// definitions), plus `2 ln n` (twice the rarest term's weight) per identifier named in the
/// task that the chunk defines. Test code is halved unless the task is about tests: tests
/// repeat the words of the code they test, and the agent opening a file wants the code.
fn chunk_scores(chunks: &[Chunk], signals: &PromptSignals) -> Vec<f32> {
    use std::collections::{HashMap, HashSet};
    let query: HashSet<&str> = signals.terms.iter().map(String::as_str).collect();
    let n = chunks.len();
    if query.is_empty() || n == 0 {
        return vec![0.0; n];
    }
    // Per chunk: matched query terms, and whether each matched in the symbol/definitions.
    let matched: Vec<HashMap<&str, bool>> = chunks
        .iter()
        .map(|c| {
            let mut m: HashMap<&str, bool> = HashMap::new();
            let head = format!("{} {}", c.symbol, c.defines.join(" "));
            for (src, strong) in [(head.as_str(), true), (c.text.as_str(), false)] {
                for t in laya_core::ident::terms(src) {
                    if let Some(q) = query.get(t.as_str()) {
                        *m.entry(q).or_insert(false) |= strong;
                    }
                }
            }
            m
        })
        .collect();
    let mut df: HashMap<&str, usize> = HashMap::new();
    for m in &matched {
        for t in m.keys() {
            *df.entry(t).or_insert(0) += 1;
        }
    }
    let about_tests = query.contains("test") || query.contains("tests");
    let max_idf = (n as f32).ln(); // 0 for a one-chunk file: nowhere else to look
    chunks
        .iter()
        .zip(&matched)
        .map(|(c, m)| {
            let lexical: f32 = m
                .iter()
                .map(|(t, &strong)| {
                    let idf = (n as f32 / df[t] as f32).ln();
                    if strong { 2.0 * idf } else { idf }
                })
                .sum();
            let named = signals
                .identifiers
                .iter()
                .filter(|i| c.defines.contains(i))
                .count() as f32;
            let score = lexical + 2.0 * max_idf * named;
            if !about_tests && is_test(c) {
                score * 0.5
            } else {
                score
            }
        })
        .collect()
}

/// Test code by symbol path: inside a `mod tests`/`mod test`, or a `test_`-prefixed fn/def.
fn is_test(c: &Chunk) -> bool {
    c.symbol.split(" > ").any(|seg| {
        matches!(seg, "mod tests" | "mod test")
            || seg.starts_with("fn test_")
            || seg.starts_with("def test_")
    })
}

/// Consecutive chunks sharing a non-empty symbol, merged; `first..=last` index `chunks`
/// (which must be sorted by `start_line`).
#[derive(Debug)]
struct Item {
    start: u32,
    end: u32,
    first: usize,
    last: usize,
    label: String,
}

fn items(chunks: &[Chunk]) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::new();
    for (i, c) in chunks.iter().enumerate() {
        if let Some(prev) = out.last_mut()
            && !c.symbol.is_empty()
            && chunks[prev.last].symbol == c.symbol
        {
            prev.end = prev.end.max(c.end_line);
            prev.last = i;
            continue;
        }
        let label = if c.symbol.is_empty() {
            c.defines
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            c.symbol.clone()
        };
        out.push(Item {
            start: c.start_line,
            end: c.end_line,
            first: i,
            last: i,
            label,
        });
    }
    out
}

/// `s` cut to at most `max` chars, with `…` when cut.
fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// Compute a narrowed `(offset, limit)` for a `Read` of `path` (a `total_lines`-line file), or
/// `None` to leave the Read untouched. Rewrites only when *all* hold:
/// - `total_lines >= policy.min_file_lines`;
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
    if total_lines < policy.min_file_lines {
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

    // ---- read_region / outline -------------------------------------------------------

    fn ch(start: u32, end: u32, symbol: &str, defines: &[&str], text: &str) -> Chunk {
        Chunk {
            path: "a.rs".into(),
            start_line: start,
            end_line: end,
            lang: laya_core::Lang::Rust,
            symbol: symbol.into(),
            kind: "function_item".into(),
            defines: defines.iter().map(|d| d.to_string()).collect(),
            refs: vec![],
            text: text.into(),
        }
    }

    /// A 1000-line file of 20 fifty-line items; item `i` defines `item{i}` and says `filler`.
    fn file_chunks() -> Vec<Chunk> {
        (0..20)
            .map(|i| {
                let s = i * 50 + 1;
                ch(
                    s,
                    s + 49,
                    &format!("fn item{i}"),
                    &[&format!("item{i}")],
                    "filler code here",
                )
            })
            .collect()
    }

    fn sig(prompt: &str) -> PromptSignals {
        crate::signals::extract_signals(prompt)
    }

    #[test]
    fn region_prefers_ranked_spans_of_the_file_at_any_probability_and_mode() {
        let r = result(
            RankMode::Lexical,
            vec![span("b.rs", 1, 50, None), span("a.rs", 300, 340, None)],
        );
        let got = ReadPolicy::default().read_region(
            Some(&r),
            "a.rs",
            &file_chunks(),
            &sig("unrelated words"),
            1000,
        );
        assert_eq!(got, Some((295, 51, "ranking")));
    }

    #[test]
    fn region_keeps_the_best_ranked_span_when_the_union_is_too_wide() {
        let r = result(
            RankMode::Laya,
            vec![
                RankedSpan {
                    score: 0.2,
                    ..span("a.rs", 10, 30, Some(0.2))
                },
                RankedSpan {
                    score: 0.9,
                    ..span("a.rs", 700, 760, Some(0.9))
                },
                RankedSpan {
                    score: 0.5,
                    ..span("a.rs", 780, 800, Some(0.5))
                },
            ],
        );
        let (offset, limit, _) = ReadPolicy::default()
            .read_region(Some(&r), "a.rs", &file_chunks(), &sig(""), 1000)
            .unwrap();
        assert_eq!((offset, offset + limit - 1), (695, 805));
    }

    #[test]
    fn region_falls_back_to_the_best_lexical_chunk() {
        let (offset, limit, basis) = ReadPolicy::default()
            .read_region(
                None,
                "a.rs",
                &file_chunks(),
                &sig("fix the bug in item7 please"),
                1000,
            )
            .unwrap();
        assert_eq!(basis, "lexical");
        assert!(offset <= 351 && offset + limit > 400, "{offset}+{limit}");
        assert!(limit <= 200);
    }

    #[test]
    fn region_is_none_without_discriminating_evidence() {
        let p = ReadPolicy::default();
        let c = file_chunks();
        // No task terms at all.
        assert_eq!(p.read_region(None, "a.rs", &c, &sig(""), 1000), None);
        // Only terms that occur in every chunk: the best chunk would be a guess.
        assert_eq!(
            p.read_region(None, "a.rs", &c, &sig("filler code here"), 1000),
            None
        );
        // Terms absent from the file.
        assert_eq!(
            p.read_region(None, "a.rs", &c, &sig("websocket handshake"), 1000),
            None
        );
        // Ranked spans in other files only.
        let r = result(RankMode::Laya, vec![span("b.rs", 1, 50, Some(0.9))]);
        assert_eq!(p.read_region(Some(&r), "a.rs", &c, &sig(""), 1000), None);
    }

    #[test]
    fn region_is_none_for_small_files_or_when_little_would_be_hidden() {
        let p = ReadPolicy::default();
        let c: Vec<Chunk> = file_chunks().into_iter().take(5).collect(); // 250 lines
        let r = result(RankMode::Laya, vec![span("a.rs", 10, 200, Some(0.9))]);
        assert_eq!(p.read_region(Some(&r), "a.rs", &c, &sig(""), 249), None);
        // 250 lines, window of 200: only 50 hidden, not worth an outline and a likely re-read.
        assert_eq!(p.read_region(Some(&r), "a.rs", &c, &sig(""), 250), None);
    }

    #[test]
    fn lexical_region_expands_to_the_enclosing_item() {
        // One long function split into three chunks sharing a symbol; the match is in the middle.
        let mut c: Vec<Chunk> = file_chunks()
            .into_iter()
            .filter(|x| !(501..=551).contains(&x.start_line))
            .collect();
        c.extend([
            ch(501, 530, "fn long_one", &[], "filler"),
            ch(531, 560, "fn long_one", &[], "replay the wal segment"),
            ch(561, 600, "fn long_one", &[], "filler"),
        ]);
        c.sort_by_key(|x| x.start_line);
        let (offset, limit, _) = ReadPolicy::default()
            .read_region(None, "a.rs", &c, &sig("replay wal segment"), 1000)
            .unwrap();
        assert_eq!((offset, offset + limit - 1), (496, 605));
    }

    #[test]
    fn named_definitions_beat_tests_that_repeat_the_task_words() {
        let mut c = file_chunks();
        c[4] = ch(
            201,
            250,
            "impl Daemon > fn read_plan",
            &["read_plan"],
            "stale",
        );
        c[18] = ch(
            901,
            950,
            "mod tests > fn read_plan_fails_open_on_stale_index",
            &["read_plan_fails_open_on_stale_index"],
            "read_plan stale index stale index read plan",
        );
        let p = ReadPolicy::default();
        let (offset, _, _) = p
            .read_region(
                None,
                "a.rs",
                &c,
                &sig("fix the stale index check in read_plan"),
                1000,
            )
            .unwrap();
        assert_eq!(offset, 196, "the implementation, not its test");
        // A task about the tests goes to the tests.
        c[4].defines.clear();
        let (offset, _, _) = p
            .read_region(
                None,
                "a.rs",
                &c,
                &sig("the read plan stale index tests fail"),
                1000,
            )
            .unwrap();
        assert_eq!(offset, 896);
    }

    #[test]
    fn a_giant_item_is_capped_around_the_matching_chunk() {
        let mut c: Vec<Chunk> = (0..20)
            .map(|i| ch(i * 50 + 1, i * 50 + 50, "impl Huge", &[], "filler"))
            .collect();
        c[15].text = "replay wal segment".into();
        let (offset, limit, _) = ReadPolicy::default()
            .read_region(None, "a.rs", &c, &sig("replay wal segment"), 1000)
            .unwrap();
        assert_eq!(limit, 200);
        assert!(offset <= 751 && offset + limit > 800, "{offset}+{limit}");
    }

    #[test]
    fn outline_lists_items_with_line_ranges_and_marks_the_shown_ones() {
        let o = ReadPolicy::default().outline(&file_chunks(), &sig(""), (51, 50));
        let lines: Vec<&str> = o.lines().collect();
        assert_eq!(lines.len(), 20);
        assert_eq!(lines[0], "  1-50 fn item0");
        assert_eq!(lines[1], "* 51-100 fn item1");
        assert_eq!(lines[2], "  101-150 fn item2");
    }

    #[test]
    fn outline_merges_split_items_and_labels_anonymous_chunks_by_definitions() {
        let c = vec![
            ch(1, 20, "", &[], "use x;"),
            ch(21, 40, "", &["MAX", "Config"], "const MAX"),
            ch(41, 90, "fn long", &[], "a"),
            ch(91, 120, "fn long", &[], "b"),
        ];
        let o = ReadPolicy::default().outline(&c, &sig(""), (500, 10));
        assert_eq!(o, "  21-40 MAX, Config\n  41-120 fn long");
    }

    #[test]
    fn outline_is_capped_and_keeps_shown_and_matching_items_first() {
        let c: Vec<Chunk> = (0..100)
            .map(|i| {
                let s = i * 10 + 1;
                ch(
                    s,
                    s + 9,
                    &format!("impl SomeLongTypeName > fn method_number_{i}"),
                    &[&format!("method_number_{i}")],
                    "x",
                )
            })
            .collect();
        let p = ReadPolicy::default();
        let o = p.outline(&c, &sig("fix method_number_97 now"), (501, 20));
        assert!(o.len() <= p.outline_chars + 40, "{}", o.len());
        assert!(o.lines().count() <= p.outline_items + 1);
        assert!(o.contains("* 501-510") && o.contains("971-980"), "{o}");
        assert!(o.lines().last().unwrap().contains("more items"), "{o}");
        let starts: Vec<u32> = o
            .lines()
            .filter_map(|l| l.get(2..)?.split('-').next()?.parse().ok())
            .collect();
        assert!(starts.windows(2).all(|w| w[0] < w[1]), "file order: {o}");
    }
}
