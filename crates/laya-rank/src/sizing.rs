//! Adaptive context sizing: full code for the top-ranked files, location pointers for the rest,
//! smaller caps for follow-up prompts, and nothing that was already sent this session.
//!
//! [`size_context`] turns a [`QueryResult`] into a [`SizedContext`] — which spans get their full
//! code inlined, which are map-only (a location pointer), which are already-sent (dropped
//! entirely), and which related items survive after removing overlap with all of the above.
//! [`render_sized`] / [`render_sized_with_keys`] render that into the same compact injection
//! format [`crate::render_compact`] uses.

use std::collections::HashSet;

use laya_core::{QueryResult, RankedSpan, Related};
use serde::{Deserialize, Serialize};

use crate::render::{
    COMPACT_FOOTER, COMPACT_HEADER, NO_CODE_FOOTER, TRUST_LINE, append_ranked_locations,
    append_related, estimate_tokens, render_span,
};
use crate::signals::FollowUpIntent;

/// How many spans get full code (`full_spans`), how many spans total get at least a location
/// pointer (`map_spans`, i.e. `full_spans` is a subset of this), and how many reference
/// neighbours (`related`) are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizingCaps {
    pub map_spans: usize,
    pub full_spans: usize,
    pub related: usize,
}

impl SizingCaps {
    /// The smaller of each cap, to combine two reasons for a smaller injection.
    pub fn min(self, other: SizingCaps) -> SizingCaps {
        SizingCaps {
            map_spans: self.map_spans.min(other.map_spans),
            full_spans: self.full_spans.min(other.full_spans),
            related: self.related.min(other.related),
        }
    }
}

/// Caps for a follow-up prompt ("now find the tests and call sites"). Its query is the session's
/// topic, so the spans not sent yet are the topic's lower-ranked ones: replaying the benchmark v2
/// sessions, the second prompt injected as much as the first but added 5 new correct files of 35
/// (httpx) and 5 of 46 (hono). A follow-up that asks for tests or callers gets no code blocks;
/// its answer is the test pointers and the definitions-and-uses lines. Any other follow-up gets
/// one block.
pub fn follow_up_caps(intent: FollowUpIntent) -> SizingCaps {
    SizingCaps {
        map_spans: 6,
        full_spans: if intent.any() { 0 } else { 1 },
        related: 8,
    }
}

/// Caps for a small repository (opt-in, `LAYA_CODEX_SIZE_BY_REPO=1`): there Claude reads little
/// code on its own (3.3k tokens per two-prompt session on the 92-file httpx), so a second inlined
/// block mostly replaces reading it would not have done.
pub fn small_repo_caps() -> SizingCaps {
    SizingCaps {
        map_spans: 6,
        full_spans: 1,
        related: 6,
    }
}

/// Options for [`size_context_opts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeOpts {
    pub caps: SizingCaps,
    /// Inline documentation files (README, CHANGELOG, `.md`, `.rst`, …). Off unless the task asks
    /// about documentation: a changelog line shares the task's words but is rarely the code to
    /// change. Such files stay in the location list.
    pub inline_prose: bool,
}

const PROSE_EXT: &[&str] = &["md", "markdown", "mdx", "rst", "adoc", "org"];
const PROSE_NAMES: &[&str] = &[
    "readme",
    "changelog",
    "changes",
    "history",
    "license",
    "copying",
    "authors",
    "contributing",
    "notice",
];

/// A documentation file, by name: prose extensions, or a README/CHANGELOG-style name with no
/// extension. Configuration and scripts are not prose (a change can be to them).
pub fn is_prose_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    match name.rsplit_once('.') {
        Some((_, ext)) => PROSE_EXT.contains(&ext),
        None => PROSE_NAMES.contains(&name.as_str()),
    }
}

/// The caps every first prompt is sized with. At most two spans get full code: in benchmark v2
/// (60 tasks) the third inlined block was the largest (~2.5k chars) and the least often a correct
/// file (22%, vs 52% and 33% for the first two). Dropping it cut the injection by a quarter and
/// lost an inlined correct file on 3 of 60 tasks; the span stays in the location listing, so the
/// agent can still Read it.
pub const DEFAULT_CAPS: SizingCaps = SizingCaps {
    map_spans: 10,
    full_spans: 2,
    related: 8,
};

/// Identifies a span by location only (no text/score) — what the daemon persists per session to
/// know what's already been sent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpanKey {
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
}

impl SpanKey {
    fn of(s: &RankedSpan) -> Self {
        SpanKey {
            path: s.path.clone(),
            start_line: s.start_line,
            end_line: s.end_line,
        }
    }
}

/// The result of [`size_context`]: what to inline in full, what to only list, what reference
/// neighbours to append, and what was dropped because it was already sent this session.
#[derive(Debug, Clone, PartialEq)]
pub struct SizedContext {
    pub full: Vec<RankedSpan>,
    pub map: Vec<RankedSpan>,
    pub related: Vec<Related>,
    pub already: Vec<RankedSpan>,
    /// Set by a caller that checked, for this prompt, that every file in `full` still matches the
    /// index (so each inlined block is the file's current content). Only then does the render
    /// say so ([`crate::render::TRUST_LINE`]). [`size_context`] never sets it.
    pub verified_current: bool,
}

fn overlaps(
    path_a: &str,
    start_a: u32,
    end_a: u32,
    path_b: &str,
    start_b: u32,
    end_b: u32,
) -> bool {
    path_a == path_b && start_a <= end_b && start_b <= end_a
}

fn span_overlaps_key(s: &RankedSpan, k: &SpanKey) -> bool {
    overlaps(
        &s.path,
        s.start_line,
        s.end_line,
        &k.path,
        k.start_line,
        k.end_line,
    )
}

fn related_overlaps_span(r: &Related, s: &RankedSpan) -> bool {
    overlaps(
        &r.path,
        r.start_line,
        r.end_line,
        &s.path,
        s.start_line,
        s.end_line,
    )
}

/// Size `result` into a [`SizedContext`] with [`DEFAULT_CAPS`], treating anything overlapping
/// `already` as already sent (dropped from `full`/`map`, reported in `.already`, and
/// never counted against the caps — the next-ranked spans are promoted into their place).
pub fn size_context(result: &QueryResult, already: &[SpanKey]) -> SizedContext {
    let opts = SizeOpts {
        caps: DEFAULT_CAPS,
        inline_prose: true,
    };
    size_context_opts(result, &opts, already)
}

/// [`size_context`] with explicit caps, and documentation files kept out of `full` unless
/// `opts.inline_prose` (a skipped file stays in `map`; the next file takes its place in `full`).
pub fn size_context_opts(
    result: &QueryResult,
    opts: &SizeOpts,
    already: &[SpanKey],
) -> SizedContext {
    let caps = opts.caps;
    let inlinable = |s: &RankedSpan| opts.inline_prose || !is_prose_path(&s.path);

    let mut already_spans: Vec<RankedSpan> = Vec::new();
    let mut candidates: Vec<RankedSpan> = Vec::new();
    for s in &result.spans {
        if already.iter().any(|k| span_overlaps_key(s, k)) {
            already_spans.push(s.clone());
        } else {
            candidates.push(s.clone());
        }
    }

    let (full, map) = select_by_rank(&candidates, &caps, &inlinable);

    let covered: Vec<&RankedSpan> = full
        .iter()
        .chain(map.iter())
        .chain(already_spans.iter())
        .collect();
    // Usage lines (grep-style definition/use lines) are not reference neighbours: a map entry
    // shows only a range, so they stay unless the renderer finds them inside visible code.
    let (usages, neighbours): (Vec<&Related>, Vec<&Related>) = result
        .related
        .iter()
        .partition(|r| crate::render::is_usage(r));
    // One line per range: a test chunk can be both a test pointer and a caller.
    let mut seen: HashSet<(&str, u32, u32)> = HashSet::new();
    let related: Vec<Related> = neighbours
        .into_iter()
        .filter(|r| !covered.iter().any(|s| related_overlaps_span(r, s)))
        .filter(|r| seen.insert((r.path.as_str(), r.start_line, r.end_line)))
        .take(caps.related)
        .chain(usages)
        .cloned()
        .collect();

    SizedContext {
        full,
        map,
        related,
        already: already_spans,
        verified_current: false,
    }
}

/// `full` = the top `caps.full_spans` inlinable candidates, one per file; `map` = the rest, up to
/// a `full + map` total of `caps.map_spans`. By rank only, whatever Laya's probabilities: the
/// ranking already blends them in, and Laya's P scale shifts with prompt wording, so every
/// threshold on it lost gold coverage against the rank at equal code volume.
fn select_by_rank(
    candidates: &[RankedSpan],
    caps: &SizingCaps,
    inlinable: &dyn Fn(&RankedSpan) -> bool,
) -> (Vec<RankedSpan>, Vec<RankedSpan>) {
    let eligible = (0..candidates.len()).filter(|&i| inlinable(&candidates[i]));
    let full_idx = distinct_files(candidates, eligible, caps.full_spans);
    let map_budget = caps.map_spans.saturating_sub(full_idx.len());
    let map: Vec<RankedSpan> = (0..candidates.len())
        .filter(|i| !full_idx.contains(i))
        .take(map_budget)
        .map(|i| candidates[i].clone())
        .collect();
    (
        full_idx.iter().map(|&i| candidates[i].clone()).collect(),
        map,
    )
}

/// Up to `k` of `indices` (in order), at most one per file: full code for one span in each of
/// several files covers more of what the task needs than several spans of one file.
fn distinct_files(
    candidates: &[RankedSpan],
    indices: impl Iterator<Item = usize>,
    k: usize,
) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    for i in indices {
        if out.len() == k {
            break;
        }
        if !out
            .iter()
            .any(|&j| candidates[j].path == candidates[i].path)
        {
            out.push(i);
        }
    }
    out
}

const ALREADY_PREFIX: &str = "\nAlready provided earlier in this session: ";

/// Render `ctx` in the same compact format as [`crate::render_compact`]: header, a
/// "Ranked locations:" listing of `full` then `map` (grouped per file), the full code of every
/// `full` span that fits `budget_tokens`, the footer, an "Already provided earlier in this
/// session: …" line (omitted when `ctx.already` is empty), then the "Related by references:"
/// section (omitted when `ctx.related` is empty).
///
/// If a `full` span's code block would exceed `budget_tokens`, it is *not* cut: it's degraded to
/// a map-only entry (it stays in the "Ranked locations:" listing, just without an inlined code
/// block, exactly like the spec's "degrade to a map entry rather than cutting the code block"
/// says) and excluded from the returned keys. Use [`render_sized_with_keys`] to know exactly
/// which spans got full code after that degradation; [`sized_keys`] reports `ctx.full` without it
/// (i.e. what would be sent absent any budget pressure).
pub fn render_sized(ctx: &SizedContext, budget_tokens: usize) -> String {
    render_sized_with_keys(ctx, budget_tokens).0
}

/// [`render_sized`], additionally returning the [`SpanKey`]s of the spans that actually got a
/// full code block (i.e. `ctx.full` minus anything degraded to a map entry by `budget_tokens`) —
/// what the daemon should record as sent.
pub fn render_sized_with_keys(ctx: &SizedContext, budget_tokens: usize) -> (String, Vec<SpanKey>) {
    let mut out = String::from(COMPACT_HEADER);
    let listed: Vec<&RankedSpan> = ctx.full.iter().chain(ctx.map.iter()).collect();
    append_ranked_locations(&mut out, &listed);

    // The trust claim precedes the code, so reserve its room up front and add it only if at
    // least one block is actually inlined.
    let trust = if ctx.verified_current { TRUST_LINE } else { "" };
    let mut used = estimate_tokens(&out) + estimate_tokens(COMPACT_FOOTER) + estimate_tokens(trust);
    let mut code = String::new();
    let mut rendered_keys = Vec::new();
    let mut inlined: Vec<&RankedSpan> = ctx.already.iter().collect();
    for span in &ctx.full {
        let block = render_span(span);
        let t = estimate_tokens(&block);
        // Reserve room for the footer and the "Already provided" line after the code blocks.
        let reserve = code.len() + trust.len() + COMPACT_FOOTER.len() + 400;
        if used + t > budget_tokens || !crate::render::fits(&out, &block, reserve) {
            // Degrade to a map entry: it's already in the "Ranked locations:" listing above, so
            // simply not inlining its code block is exactly that degradation — no truncated code.
            continue;
        }
        code.push_str(&block);
        used += t;
        rendered_keys.push(SpanKey::of(span));
        inlined.push(span);
    }
    if !code.is_empty() {
        out.push_str(trust);
    }
    out.push_str(&code);
    out.push_str(if code.is_empty() {
        NO_CODE_FOOTER
    } else {
        COMPACT_FOOTER
    });

    if !ctx.already.is_empty() {
        let mut line = String::from(ALREADY_PREFIX);
        for (i, s) in ctx.already.iter().enumerate() {
            let loc = format!(
                "{}{}:{}-{}",
                if i > 0 { ", " } else { "" },
                s.path,
                s.start_line,
                s.end_line
            );
            if !crate::render::fits(&out, &format!("{line}{loc}"), 1) {
                break;
            }
            line.push_str(&loc);
        }
        out.push_str(&line);
        out.push('\n');
    }

    append_related(&mut out, &ctx.related, &inlined);

    (out, rendered_keys)
}

/// The [`SpanKey`]s of `ctx.full` — the spans `size_context` decided should get full code,
/// independent of any rendering budget. See [`render_sized_with_keys`] for the budget-aware,
/// "actually rendered" version.
pub fn sized_keys(ctx: &SizedContext) -> Vec<SpanKey> {
    ctx.full.iter().map(SpanKey::of).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use laya_core::RankMode;

    fn span(
        path: &str,
        start: u32,
        end: u32,
        symbol: &str,
        p: Option<f32>,
        score: f32,
    ) -> RankedSpan {
        RankedSpan {
            path: path.to_string(),
            start_line: start,
            end_line: end,
            symbol: symbol.to_string(),
            p_relevant: p,
            score,
            text: format!("// {path}:{start}"),
        }
    }

    fn related(path: &str, start: u32, end: u32, relation: &str) -> Related {
        Related {
            path: path.to_string(),
            start_line: start,
            end_line: end,
            symbol: String::new(),
            relation: relation.to_string(),
        }
    }

    fn result(mode: RankMode, spans: Vec<RankedSpan>, related: Vec<Related>) -> QueryResult {
        let candidates = spans.len();
        QueryResult {
            spans,
            mode,
            elapsed_ms: 1,
            candidates,
            scored: 0,
            offered: 0,
            related,
        }
    }

    fn key(s: &RankedSpan) -> SpanKey {
        SpanKey::of(s)
    }

    fn opts(caps: SizingCaps, inline_prose: bool) -> SizeOpts {
        SizeOpts { caps, inline_prose }
    }

    #[test]
    fn model_probabilities_do_not_change_what_the_default_sizing_selects() {
        // Laya's probabilities in rank order are low, high, tiny, mid, …: the default sizing
        // takes full code for the first two files by rank and lists the rest, exactly as it does
        // for a keyword-only ranking of the same spans.
        let ps = [
            0.05, 0.9, 0.01, 0.3, 0.02, 0.6, 0.0, 0.15, 0.4, 0.07, 0.2, 0.03,
        ];
        let mut spans: Vec<RankedSpan> = ps
            .iter()
            .enumerate()
            .map(|(i, &p)| {
                span(
                    &format!("f{i}.rs"),
                    1,
                    20,
                    "",
                    Some(p),
                    1.0 - i as f32 / 20.0,
                )
            })
            .collect();
        spans.insert(1, span("f0.rs", 40, 60, "", Some(0.95), 0.99)); // same file as the top span
        let laya = result(RankMode::Laya, spans.clone(), vec![]);
        let ctx = size_context_opts(&laya, &opts(DEFAULT_CAPS, false), &[]);
        let paths = |v: &[RankedSpan]| -> Vec<String> {
            v.iter()
                .map(|s| format!("{}:{}", s.path, s.start_line))
                .collect()
        };
        assert_eq!(paths(&ctx.full), ["f0.rs:1", "f1.rs:1"]);
        assert_eq!(
            paths(&ctx.map),
            [
                "f0.rs:40", "f2.rs:1", "f3.rs:1", "f4.rs:1", "f5.rs:1", "f6.rs:1", "f7.rs:1",
                "f8.rs:1"
            ]
        );
        let lexical = result(RankMode::Lexical, spans, vec![]);
        let lex = size_context_opts(&lexical, &opts(DEFAULT_CAPS, false), &[]);
        assert_eq!(
            (paths(&lex.full), paths(&lex.map)),
            (paths(&ctx.full), paths(&ctx.map))
        );
    }

    #[test]
    fn a_follow_up_that_asks_for_tests_or_callers_inlines_no_code() {
        let spans: Vec<RankedSpan> = (0..10)
            .map(|i| {
                span(
                    &format!("f{i}.rs"),
                    1,
                    50,
                    "",
                    Some(0.9),
                    1.0 - i as f32 / 20.0,
                )
            })
            .collect();
        let r = result(RankMode::Laya, spans, vec![]);
        let caps = follow_up_caps(FollowUpIntent {
            tests: true,
            callers: false,
        });
        let ctx = size_context_opts(&r, &opts(caps, false), &[]);
        assert!(ctx.full.is_empty());
        assert_eq!(ctx.map.len(), 6);
        let plain = follow_up_caps(FollowUpIntent::default());
        let ctx = size_context_opts(&r, &opts(plain, false), &[]);
        assert_eq!(
            ctx.full.len(),
            1,
            "a follow-up with no named intent gets one block"
        );
    }

    #[test]
    fn prose_is_listed_not_inlined_and_the_next_code_file_takes_its_place() {
        let spans = vec![
            span("CHANGELOG.md", 1, 20, "", Some(0.9), 1.0),
            span("src/a.rs", 1, 20, "", Some(0.9), 0.9),
            span("docs/guide.rst", 1, 20, "", Some(0.9), 0.8),
            span("src/b.rs", 1, 20, "", Some(0.9), 0.7),
        ];
        for mode in [RankMode::Laya, RankMode::Lexical] {
            let r = result(mode, spans.clone(), vec![]);
            let ctx = size_context_opts(&r, &opts(DEFAULT_CAPS, false), &[]);
            let full: Vec<&str> = ctx.full.iter().map(|s| s.path.as_str()).collect();
            assert_eq!(full, ["src/a.rs", "src/b.rs"]);
            assert!(
                ctx.map.iter().any(|s| s.path == "CHANGELOG.md"),
                "still listed"
            );
            let asked = size_context_opts(&r, &opts(DEFAULT_CAPS, true), &[]);
            assert_eq!(
                asked.full[0].path, "CHANGELOG.md",
                "inlined when the task asks for docs"
            );
        }
    }

    #[test]
    fn size_context_keeps_its_behaviour() {
        let spans = vec![
            span("README.md", 1, 20, "", Some(0.9), 1.0),
            span("src/a.rs", 1, 20, "", Some(0.9), 0.9),
        ];
        let r = result(RankMode::Laya, spans, vec![]);
        let ctx = size_context(&r, &[]);
        assert_eq!(ctx.full.len(), 2);
    }

    #[test]
    fn an_unscored_tail_still_sizes_by_rank_and_never_inlines_a_tail_span() {
        // The budget stopped the model after two candidates: the tail has no p, and selection
        // stays by rank, so a tail span never jumps into `full`.
        let spans = vec![
            span("a.rs", 1, 20, "", Some(0.9), 1.0),
            span("b.rs", 1, 20, "", Some(0.1), 0.9),
            span("c.rs", 1, 20, "", None, 0.0),
            span("d.rs", 1, 20, "", None, 0.0),
        ];
        let r = result(RankMode::Laya, spans, vec![]);
        let ctx = size_context(&r, &[]);
        let full: Vec<&str> = ctx.full.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(full, ["a.rs", "b.rs"]);
    }

    #[test]
    fn smaller_caps_take_the_minimum_of_each() {
        let a = SizingCaps {
            map_spans: 6,
            full_spans: 1,
            related: 8,
        };
        let b = SizingCaps {
            map_spans: 8,
            full_spans: 0,
            related: 6,
        };
        assert_eq!(
            a.min(b),
            SizingCaps {
                map_spans: 6,
                full_spans: 0,
                related: 6
            }
        );
    }

    #[test]
    fn a_render_without_code_blocks_does_not_point_at_code_above() {
        let spans: Vec<RankedSpan> = (0..3)
            .map(|i| span(&format!("f{i}.rs"), 1, 20, "", Some(0.9), 1.0))
            .collect();
        let r = result(RankMode::Laya, spans, vec![]);
        let caps = follow_up_caps(FollowUpIntent {
            tests: true,
            callers: true,
        });
        let ctx = size_context_opts(&r, &opts(caps, false), &[]);
        let (text, keys) = render_sized_with_keys(&ctx, 3000);
        assert!(keys.is_empty());
        assert!(!text.contains("code above"), "{text}");
        assert!(text.contains(NO_CODE_FOOTER), "{text}");
        let with_code = size_context(&r, &[]);
        assert!(render_sized(&with_code, 3000).contains(COMPACT_FOOTER));
    }

    #[test]
    fn a_related_range_listed_twice_is_shown_once_with_its_first_relation() {
        let spans = vec![span("src/a.rs", 1, 20, "", Some(0.9), 1.0)];
        let r = result(
            RankMode::Laya,
            spans,
            vec![
                related("tests/t.rs", 5, 30, "test using `a`"),
                related("src/b.rs", 1, 9, "calls `a` (#1)"),
                related("tests/t.rs", 5, 30, "calls `a` (#1)"),
            ],
        );
        let ctx = size_context(&r, &[]);
        let rel: Vec<&str> = ctx.related.iter().map(|r| r.relation.as_str()).collect();
        assert_eq!(rel, ["test using `a`", "calls `a` (#1)"]);
        assert_eq!(ctx.related[1].path, "src/b.rs");
    }

    #[test]
    fn prose_paths() {
        for p in [
            "README.md",
            "docs/a.rst",
            "CHANGELOG",
            "notes.adoc",
            "x/LICENSE",
            "a.mdx",
        ] {
            assert!(is_prose_path(p), "{p}");
        }
        for p in [
            "src/a.rs",
            "package.json",
            "build.sh",
            "q.sql",
            "Makefile",
            "readme.rs",
            "CMakeLists.txt",
            "requirements.txt",
        ] {
            assert!(!is_prose_path(p), "{p}");
        }
    }

    #[test]
    fn full_code_goes_to_distinct_files_in_both_modes() {
        let spans = vec![
            span("a.rs", 1, 20, "", Some(0.9), 1.0),
            span("a.rs", 40, 60, "", Some(0.9), 0.9),
            span("b.rs", 1, 20, "", Some(0.9), 0.8),
            span("c.rs", 1, 20, "", Some(0.9), 0.7),
        ];
        let lexical: Vec<RankedSpan> = spans
            .iter()
            .map(|s| RankedSpan {
                p_relevant: None,
                ..s.clone()
            })
            .collect();
        for r in [
            result(RankMode::Laya, spans, vec![]),
            result(RankMode::Lexical, lexical, vec![]),
        ] {
            let ctx = size_context(&r, &[]);
            let full: Vec<(&str, u32)> = ctx
                .full
                .iter()
                .map(|s| (s.path.as_str(), s.start_line))
                .collect();
            assert_eq!(full, vec![("a.rs", 1), ("b.rs", 1)]);
            assert!(
                ctx.map
                    .iter()
                    .any(|s| s.path == "a.rs" && s.start_line == 40),
                "second a.rs span stays in the map"
            );
        }
    }

    #[test]
    fn usage_lines_survive_map_overlap_and_the_related_cap() {
        let spans: Vec<RankedSpan> = (0..5)
            .map(|i| span(&format!("f{i}.rs"), 1, 50, "", Some(0.9), 1.0))
            .collect();
        let mut rel: Vec<Related> = (0..20)
            .map(|i| related(&format!("r{i}.rs"), 1, 9, "calls `x` (#1)"))
            .collect();
        rel.push(Related {
            symbol: "x();".into(),
            ..related("f4.rs", 7, 7, "use of `x`")
        });
        let ctx = size_context(&result(RankMode::Laya, spans, rel), &[]);
        assert!(
            ctx.map.iter().any(|s| s.path == "f4.rs"),
            "f4.rs is map-only"
        );
        assert!(
            ctx.related
                .iter()
                .any(|r| r.path == "f4.rs" && r.relation == "use of `x`")
        );
        assert_eq!(
            ctx.related
                .iter()
                .filter(|r| !crate::render::is_usage(r))
                .count(),
            8
        );
    }

    #[test]
    fn sized_render_stays_under_the_hook_cap_and_reports_only_inlined_keys() {
        let spans: Vec<RankedSpan> = ["a.rs", "b.rs", "c.rs"]
            .iter()
            .map(|p| RankedSpan {
                text: "z".repeat(4_000),
                ..span(p, 1, 200, "", Some(0.9), 1.0)
            })
            .collect();
        let related: Vec<Related> = (0..100)
            .map(|i| related(&format!("r{i}.rs"), 1, 9, "calls `x` (#1)"))
            .collect();
        let ctx = size_context(&result(RankMode::Laya, spans, related), &[]);
        let (out, keys) = render_sized_with_keys(&ctx, 100_000);
        assert!(
            out.len() <= crate::render::MAX_INJECT_CHARS,
            "{} chars",
            out.len()
        );
        assert_eq!(
            keys.len(),
            out.matches("```").count() / 2,
            "keys = inlined blocks only"
        );
        assert!(keys.len() < 3, "not every 4k-char block fits");
    }

    #[test]
    fn trust_line_is_claimed_only_for_verified_inlined_code() {
        let spans = vec![span("a.rs", 1, 20, "fn a", Some(0.9), 1.0)];
        let mut ctx = size_context(&result(RankMode::Laya, spans, vec![]), &[]);
        assert!(
            !ctx.verified_current,
            "size_context never claims a disk check"
        );
        let unverified = render_sized(&ctx, 10_000);
        assert!(
            !unverified.contains(crate::render::TRUST_LINE),
            "{unverified}"
        );

        ctx.verified_current = true;
        let verified = render_sized(&ctx, 10_000);
        assert!(verified.contains(crate::render::TRUST_LINE), "{verified}");
        assert!(
            verified.find(crate::render::TRUST_LINE) < verified.find("### a.rs"),
            "the claim precedes the code it covers"
        );

        let map_only = SizedContext {
            map: std::mem::take(&mut ctx.full),
            ..ctx
        };
        let out = render_sized(&map_only, 10_000);
        assert!(
            !out.contains(crate::render::TRUST_LINE),
            "no code, no claim: {out}"
        );
    }

    // ---- caps ----

    /// Tighter caps than the default, to exercise the caps themselves.
    const SMALL_CAPS: SizingCaps = SizingCaps {
        map_spans: 5,
        full_spans: 1,
        related: 4,
    };

    #[test]
    fn default_caps_inline_at_most_two_blocks() {
        // Benchmark v2: the third inlined block was the largest (~2.5k chars) and the least
        // often a correct file (22%); dropping it cut the injection by 25% and lost the
        // inlined correct file on 3 of 60 tasks, which still list it as a location.
        assert_eq!(
            DEFAULT_CAPS,
            SizingCaps {
                map_spans: 10,
                full_spans: 2,
                related: 8
            }
        );
    }

    #[test]
    fn full_is_capped_at_caps_full_spans() {
        let spans: Vec<RankedSpan> = (0..5)
            .map(|i| {
                span(
                    &format!("f{i}.rs"),
                    1,
                    5,
                    "",
                    Some(0.9),
                    0.9 - i as f32 * 0.01,
                )
            })
            .collect();
        let r = result(RankMode::Laya, spans, vec![]);
        let ctx = size_context_opts(&r, &opts(SMALL_CAPS, true), &[]); // full_spans cap = 1
        assert_eq!(ctx.full.len(), 1);
        assert_eq!(ctx.full[0].path, "f0.rs");
    }

    // ---- caps applied end to end ----

    #[test]
    fn full_plus_map_never_exceeds_caps_map_spans() {
        let spans: Vec<RankedSpan> = (0..20)
            .map(|i| {
                span(
                    &format!("f{i}.rs"),
                    1,
                    5,
                    "",
                    Some(0.9),
                    1.0 - i as f32 * 0.01,
                )
            })
            .collect();
        let r = result(RankMode::Laya, spans, vec![]);
        for caps in [SMALL_CAPS, DEFAULT_CAPS] {
            let ctx = size_context_opts(&r, &opts(caps, true), &[]);
            assert!(ctx.full.len() <= caps.full_spans);
            assert!(ctx.full.len() + ctx.map.len() <= caps.map_spans);
        }
    }

    // ---- promotion after already-sent spans ----

    #[test]
    fn already_sent_spans_are_excluded_and_the_next_one_is_promoted() {
        let spans = vec![
            span("a.rs", 1, 5, "", Some(0.9), 0.9),
            span("b.rs", 1, 5, "", Some(0.8), 0.8),
        ];
        let r = result(RankMode::Laya, spans.clone(), vec![]);
        let already = vec![key(&spans[0])];
        let ctx = size_context_opts(&r, &opts(SMALL_CAPS, true), &already); // full cap=1
        assert_eq!(ctx.already.len(), 1);
        assert_eq!(ctx.already[0].path, "a.rs");
        // "b.rs" is promoted into the single full slot that "a.rs" would otherwise have taken.
        assert_eq!(
            ctx.full.iter().map(|s| s.path.as_str()).collect::<Vec<_>>(),
            vec!["b.rs"]
        );
    }

    #[test]
    fn already_sent_match_is_by_overlap_not_exact_range() {
        let spans = vec![span("a.rs", 10, 20, "", Some(0.9), 0.9)];
        let r = result(RankMode::Laya, spans, vec![]);
        // caller's "already sent" range only partially overlaps the ranked span's range.
        let already = vec![SpanKey {
            path: "a.rs".into(),
            start_line: 15,
            end_line: 25,
        }];
        let ctx = size_context(&r, &already);
        assert!(ctx.full.is_empty());
        assert_eq!(ctx.already.len(), 1);
    }

    #[test]
    fn full_is_the_top_n_and_map_is_the_rest() {
        let spans = vec![
            span("a.rs", 1, 5, "", None, 0.9),
            span("b.rs", 1, 5, "", None, 0.8),
            span("c.rs", 1, 5, "", None, 0.7),
        ];
        let r = result(RankMode::Lexical, spans, vec![]);
        let ctx = size_context_opts(&r, &opts(SMALL_CAPS, true), &[]); // full=1, map total=5
        assert_eq!(
            ctx.full.iter().map(|s| s.path.as_str()).collect::<Vec<_>>(),
            vec!["a.rs"]
        );
        assert_eq!(
            ctx.map.iter().map(|s| s.path.as_str()).collect::<Vec<_>>(),
            vec!["b.rs", "c.rs"]
        );
    }

    // ---- related filtering ----

    #[test]
    fn related_overlapping_full_map_or_already_is_dropped() {
        let spans = vec![
            span("a.rs", 1, 10, "", Some(0.9), 0.9),
            span("b.rs", 1, 10, "", Some(0.9), 0.85),
        ];
        let r = result(
            RankMode::Laya,
            spans.clone(),
            vec![
                related("a.rs", 1, 10, "calls `x` (#1)"), // overlaps full
                related("b.rs", 1, 10, "calls `y` (#1)"), // overlaps map (depending on caps)
                related("c.rs", 1, 10, "calls `z` (#1)"), // survives
            ],
        );
        let already = vec![key(&spans[1])];
        let ctx = size_context(&r, &already);
        assert_eq!(
            ctx.related
                .iter()
                .map(|x| x.path.as_str())
                .collect::<Vec<_>>(),
            vec!["c.rs"]
        );
    }

    #[test]
    fn related_is_capped_at_caps_related() {
        let related_items: Vec<Related> = (0..20)
            .map(|i| related(&format!("r{i}.rs"), 1, 5, "x"))
            .collect();
        let r = result(RankMode::Laya, vec![], related_items);
        let ctx = size_context_opts(&r, &opts(SMALL_CAPS, true), &[]); // related cap 4
        assert_eq!(ctx.related.len(), 4);
    }

    // ---- render_sized ----

    #[test]
    fn render_sized_contains_header_listing_code_and_footer() {
        let ctx = SizedContext {
            full: vec![span("a.rs", 1, 5, "fn a", Some(0.9), 0.9)],
            map: vec![span("b.rs", 1, 5, "fn b", Some(0.3), 0.3)],
            related: vec![related("src/x.rs", 10, 40, "calls `bar` (#1)")],
            already: vec![],
            verified_current: false,
        };
        let out = render_sized(&ctx, 10_000);
        assert!(out.starts_with("<!-- laya-codex:"));
        assert!(out.contains("Ranked locations:"));
        assert!(out.contains("1. a.rs — 1-5 fn a"));
        assert!(out.contains("2. b.rs — 1-5 fn b"));
        assert!(out.contains("```")); // full code block for a.rs
        assert!(out.contains("Use the code above directly"));
        assert!(out.contains("Related by references:"));
        assert!(out.contains("- src/x.rs:10-40 — calls `bar` (#1)"));
        assert!(!out.contains("Already provided"));
    }

    #[test]
    fn render_sized_shows_already_line_only_when_non_empty() {
        let ctx = SizedContext {
            full: vec![],
            map: vec![],
            related: vec![],
            already: vec![
                span("a.rs", 1, 5, "", None, 0.0),
                span("b.rs", 10, 20, "", None, 0.0),
            ],
            verified_current: false,
        };
        let out = render_sized(&ctx, 10_000);
        assert!(out.contains("Already provided earlier in this session: a.rs:1-5, b.rs:10-20"));
    }

    #[test]
    fn sized_keys_returns_ctx_full_regardless_of_budget() {
        let ctx = SizedContext {
            full: vec![
                span("a.rs", 1, 5, "", Some(0.9), 0.9),
                span("b.rs", 1, 5, "", Some(0.9), 0.8),
            ],
            map: vec![],
            related: vec![],
            already: vec![],
            verified_current: false,
        };
        let keys = sized_keys(&ctx);
        assert_eq!(keys, vec![key(&ctx.full[0]), key(&ctx.full[1])]);
    }

    #[test]
    fn budget_degradation_demotes_a_full_span_to_a_map_entry_not_a_cut_block() {
        let big = "x".repeat(2000);
        let ctx = SizedContext {
            full: vec![
                span("a.rs", 1, 5, "fn a", Some(0.9), 0.9),
                span("b.rs", 1, 5, "fn b", Some(0.8), 0.8),
            ],
            map: vec![],
            related: vec![],
            already: vec![],
            verified_current: false,
        };
        let mut ctx = ctx;
        ctx.full[1].text = big;
        // budget fits the header/listing/footer plus a.rs's tiny code block, but not b.rs's huge one.
        let (out, keys) = render_sized_with_keys(&ctx, 220);
        assert!(
            out.contains("2. b.rs — 1-5 fn b"),
            "b.rs stays in the listing (map entry)"
        );
        assert_eq!(
            keys,
            vec![key(&ctx.full[0])],
            "only a.rs actually got a full code block"
        );
        // no truncated/partial code fence for b.rs's block:
        assert!(!out.contains("xxxx"));
    }

    #[test]
    fn render_sized_equals_render_sized_with_keys_string() {
        let ctx = SizedContext {
            full: vec![span("a.rs", 1, 5, "", Some(0.9), 0.9)],
            map: vec![],
            related: vec![],
            already: vec![],
            verified_current: false,
        };
        assert_eq!(
            render_sized(&ctx, 10_000),
            render_sized_with_keys(&ctx, 10_000).0
        );
    }
}
