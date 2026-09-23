//! Render a [`laya_core::QueryResult`] as the compact markdown block injected by hooks/MCP.

use laya_core::{QueryResult, RankedSpan, Related};

/// Rough token estimate used to fit output inside a caller-supplied budget (no tokenizer
/// dependency here; this matches the estimate the hooks use elsewhere in the project).
const CHARS_PER_TOKEN: f32 = 3.5;

pub(crate) fn estimate_tokens(s: &str) -> usize {
    (s.chars().count() as f32 / CHARS_PER_TOKEN).ceil() as usize
}

const HEADER: &str = "<!-- laya: pre-ranked code spans for this task. \
Prefer these ranges over exploring; use Read with offset/limit to see more of a file. -->\n\n";

/// Render `result` as a markdown block: a one-line header, then one
/// `### path:start-end — symbol (p=0.83)` heading + fenced code block per span, in the input
/// (score-descending) order, then (when non-empty) a "Related by references:" section listing
/// `result.related`. Stops adding spans before the running total would exceed `budget_tokens`
/// (estimated as `chars / 3.5`); the header is always included. Equivalent to
/// `render_context_opts(result, budget_tokens, true)`.
pub fn render_context(result: &QueryResult, budget_tokens: usize) -> String {
    render_context_opts(result, budget_tokens, true)
}

/// [`render_context`] with an explicit switch for the "Related by references:" section, so a
/// hook can turn it off per benchmark arm.
pub fn render_context_opts(
    result: &QueryResult,
    budget_tokens: usize,
    include_related: bool,
) -> String {
    let mut out = String::new();
    out.push_str(HEADER);
    let mut used = estimate_tokens(&out);

    for span in &result.spans {
        let block = render_span(span);
        let block_tokens = estimate_tokens(&block);
        if used + block_tokens > budget_tokens {
            break;
        }
        out.push_str(&block);
        used += block_tokens;
    }
    if include_related {
        append_related(&mut out, &result.related);
    }
    out
}

pub(crate) const COMPACT_HEADER: &str = "<!-- laya-codex: code located for this task by static analysis \
(tree-sitter chunks + BM25 + Laya relevance model). -->\n";

pub(crate) const COMPACT_FOOTER: &str = "Use the code above directly. Search or Read further only for what is \
still missing, and prefer Read with offset/limit around the listed lines.\n";

/// Compact injection: a ranked file map (every span as `path:lines — symbol`, grouped per file)
/// plus the full code of only the first `full_spans` spans, then (when non-empty) a
/// "Related by references:" section listing `result.related`. Roughly half the tokens of
/// [`render_context`] for the same ranking; the map lets the agent jump straight to ranges.
/// Equivalent to `render_compact_opts(result, full_spans, budget_tokens, true)`.
pub fn render_compact(result: &QueryResult, full_spans: usize, budget_tokens: usize) -> String {
    render_compact_opts(result, full_spans, budget_tokens, true)
}

/// [`render_compact`] with an explicit switch for the "Related by references:" section — this is
/// the shipped default renderer (`docs/RESULTS.md`), so the hook uses this switch to run the
/// with/without-expansion benchmark arms.
pub fn render_compact_opts(
    result: &QueryResult,
    full_spans: usize,
    budget_tokens: usize,
    include_related: bool,
) -> String {
    let mut out = String::from(COMPACT_HEADER);
    let listed: Vec<&RankedSpan> = result.spans.iter().collect();
    append_ranked_locations(&mut out, &listed);
    let mut used = estimate_tokens(&out) + estimate_tokens(COMPACT_FOOTER);
    for span in result.spans.iter().take(full_spans) {
        let block = render_span(span);
        let t = estimate_tokens(&block);
        if used + t > budget_tokens {
            break;
        }
        out.push_str(&block);
        used += t;
    }
    out.push_str(COMPACT_FOOTER);
    if include_related {
        append_related(&mut out, &result.related);
    }
    out
}

/// Append a "Ranked locations:" listing (every span as `path:lines — symbol`, grouped per file,
/// files in first-seen order), shared by [`render_compact_opts`] and `sizing::render_sized`.
pub(crate) fn append_ranked_locations(out: &mut String, spans: &[&RankedSpan]) {
    out.push_str("\nRanked locations:\n");
    let mut files: Vec<(&str, Vec<&RankedSpan>)> = Vec::new();
    for s in spans {
        match files.iter_mut().find(|(p, _)| *p == s.path) {
            Some((_, v)) => v.push(s),
            None => files.push((&s.path, vec![s])),
        }
    }
    for (i, (path, group)) in files.iter().enumerate() {
        let parts: Vec<String> = group
            .iter()
            .map(|s| {
                if s.symbol.is_empty() {
                    format!("{}-{}", s.start_line, s.end_line)
                } else {
                    format!("{}-{} {}", s.start_line, s.end_line, s.symbol)
                }
            })
            .collect();
        out.push_str(&format!("{}. {} — {}\n", i + 1, path, parts.join("; ")));
    }
    out.push('\n');
}

const RELATED_HEADER: &str = "\nRelated by references:\n";

/// Append the "Related by references:" section (omitted entirely when `related` is empty).
pub(crate) fn append_related(out: &mut String, related: &[Related]) {
    if related.is_empty() {
        return;
    }
    out.push_str(RELATED_HEADER);
    for r in related {
        out.push_str(&render_related_line(r));
    }
}

/// `- path:start-end symbol — relation` (the symbol segment is omitted when empty, matching
/// [`render_span`]'s heading style).
fn render_related_line(r: &Related) -> String {
    if r.symbol.is_empty() {
        format!(
            "- {}:{}-{} — {}\n",
            r.path, r.start_line, r.end_line, r.relation
        )
    } else {
        format!(
            "- {}:{}-{} {} — {}\n",
            r.path, r.start_line, r.end_line, r.symbol, r.relation
        )
    }
}

pub(crate) fn render_span(span: &RankedSpan) -> String {
    let mut heading = format!("### {}:{}-{}", span.path, span.start_line, span.end_line);
    if !span.symbol.is_empty() {
        heading.push_str(" — ");
        heading.push_str(&span.symbol);
    }
    if let Some(p) = span.p_relevant {
        heading.push_str(&format!(" (p={p:.2})"));
    }
    format!(
        "{heading}\n```{}\n{}\n```\n\n",
        lang_tag(&span.path),
        span.text
    )
}

fn lang_tag(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "rs" => "rust",
        "py" => "python",
        "ts" => "typescript",
        "tsx" => "tsx",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" => "cpp",
        "cs" => "csharp",
        "rb" => "ruby",
        "php" => "php",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        _ => "",
    }
}

#[cfg(test)]
mod compact_tests {
    use super::*;
    use laya_core::RankMode;

    fn sp(path: &str, a: u32, sym: &str) -> RankedSpan {
        RankedSpan {
            path: path.into(),
            start_line: a,
            end_line: a + 20,
            symbol: sym.into(),
            p_relevant: Some(0.4),
            score: 1.0,
            text: "x".repeat(400),
        }
    }

    fn rel(path: &str, a: u32, sym: &str, relation: &str) -> Related {
        Related {
            path: path.into(),
            start_line: a,
            end_line: a + 5,
            symbol: sym.into(),
            relation: relation.into(),
        }
    }

    #[test]
    fn compact_lists_all_spans_grouped_by_file_and_inlines_only_top_k() {
        let r = QueryResult {
            spans: vec![
                sp("a.rs", 1, "fn a"),
                sp("b.rs", 5, ""),
                sp("a.rs", 40, "fn c"),
            ],
            mode: RankMode::Laya,
            elapsed_ms: 1,
            candidates: 3,
            related: Vec::new(),
        };
        let out = render_compact(&r, 1, 10_000);
        assert!(out.contains("1. a.rs — 1-21 fn a; 40-60 fn c"));
        assert!(out.contains("2. b.rs — 5-25"));
        assert_eq!(out.matches("```").count(), 2, "exactly one fenced block");
        assert!(out.len() < render_context(&r, 10_000).len());
    }

    #[test]
    fn compact_respects_budget_for_code_blocks() {
        let r = QueryResult {
            spans: vec![sp("a.rs", 1, "fn a"), sp("b.rs", 5, "fn b")],
            mode: RankMode::Laya,
            elapsed_ms: 1,
            candidates: 2,
            related: Vec::new(),
        };
        let out = render_compact(&r, 2, 150);
        assert!(out.contains("2. b.rs"));
        assert!(out.matches("```").count() <= 2);
    }

    #[test]
    fn compact_appends_related_section_after_the_ranked_list() {
        let r = QueryResult {
            spans: vec![sp("a.rs", 1, "fn a")],
            mode: RankMode::Laya,
            elapsed_ms: 1,
            candidates: 1,
            related: vec![rel("src/x.rs", 10, "fn foo", "calls `bar` (#1)")],
        };
        let out = render_compact(&r, 1, 10_000);
        assert!(out.contains("Related by references:"));
        assert!(out.contains("- src/x.rs:10-15 fn foo — calls `bar` (#1)"));
        // the section comes after the ranked list / code blocks.
        let list_pos = out.find("Ranked locations:").unwrap();
        let related_pos = out.find("Related by references:").unwrap();
        assert!(related_pos > list_pos);
    }

    #[test]
    fn compact_omits_related_section_when_empty() {
        let r = QueryResult {
            spans: vec![sp("a.rs", 1, "fn a")],
            mode: RankMode::Laya,
            elapsed_ms: 1,
            candidates: 1,
            related: Vec::new(),
        };
        let out = render_compact(&r, 1, 10_000);
        assert!(!out.contains("Related by references:"));
    }

    #[test]
    fn compact_related_line_omits_symbol_dash_when_symbol_is_empty() {
        let r = QueryResult {
            spans: vec![sp("a.rs", 1, "fn a")],
            mode: RankMode::Laya,
            elapsed_ms: 1,
            candidates: 1,
            related: vec![rel("src/x.rs", 10, "", "calls `bar` (#1)")],
        };
        let out = render_compact(&r, 1, 10_000);
        assert!(out.contains("- src/x.rs:10-15 — calls `bar` (#1)"));
    }

    #[test]
    fn render_compact_opts_can_disable_related() {
        let r = QueryResult {
            spans: vec![sp("a.rs", 1, "fn a")],
            mode: RankMode::Laya,
            elapsed_ms: 1,
            candidates: 1,
            related: vec![rel("src/x.rs", 10, "fn foo", "calls `bar` (#1)")],
        };
        let with_related = render_compact_opts(&r, 1, 10_000, true);
        let without_related = render_compact_opts(&r, 1, 10_000, false);
        assert!(with_related.contains("Related by references:"));
        assert!(!without_related.contains("Related by references:"));
        // the existing `render_compact` signature keeps working and defaults to including it.
        assert_eq!(render_compact(&r, 1, 10_000), with_related);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use laya_core::{RankMode, Related};

    fn span(
        path: &str,
        start: u32,
        end: u32,
        symbol: &str,
        p: Option<f32>,
        text: &str,
    ) -> RankedSpan {
        RankedSpan {
            path: path.to_string(),
            start_line: start,
            end_line: end,
            symbol: symbol.to_string(),
            p_relevant: p,
            score: p.unwrap_or(1.0),
            text: text.to_string(),
        }
    }

    fn result(spans: Vec<RankedSpan>) -> QueryResult {
        result_with_related(spans, Vec::new())
    }

    fn result_with_related(spans: Vec<RankedSpan>, related: Vec<Related>) -> QueryResult {
        QueryResult {
            spans,
            mode: RankMode::Laya,
            elapsed_ms: 5,
            candidates: 3,
            related,
        }
    }

    #[test]
    fn header_always_present() {
        let out = render_context(&result(vec![]), 10_000);
        assert!(out.starts_with("<!-- laya:"));
        assert!(out.contains("Read with offset/limit"));
    }

    #[test]
    fn renders_heading_with_symbol_and_probability() {
        let out = render_context(
            &result(vec![span(
                "src/a.rs",
                10,
                20,
                "Foo::bar",
                Some(0.83),
                "fn bar() {}",
            )]),
            10_000,
        );
        assert!(out.contains("### src/a.rs:10-20 — Foo::bar (p=0.83)"));
        assert!(out.contains("```rust\nfn bar() {}\n```"));
    }

    #[test]
    fn omits_symbol_dash_when_symbol_is_empty() {
        let out = render_context(
            &result(vec![span("a.py", 1, 5, "", Some(0.5), "x")]),
            10_000,
        );
        assert!(out.contains("### a.py:1-5 (p=0.50)"));
        assert!(!out.contains(" —  ("));
    }

    #[test]
    fn omits_probability_when_none() {
        let out = render_context(&result(vec![span("a.go", 1, 5, "", None, "x")]), 10_000);
        assert!(out.contains("### a.go:1-5\n"));
        assert!(!out.contains("(p="));
    }

    #[test]
    fn picks_language_tag_from_extension() {
        let out = render_context(&result(vec![span("x.py", 1, 2, "", None, "pass")]), 10_000);
        assert!(out.contains("```python"));
    }

    #[test]
    fn unknown_extension_falls_back_to_bare_fence() {
        let out = render_context(
            &result(vec![span("Makefile", 1, 2, "", None, "all:")]),
            10_000,
        );
        assert!(out.contains("```\nall:\n```"));
    }

    #[test]
    fn stops_before_exceeding_token_budget() {
        let big_text = "x".repeat(1000);
        let spans = vec![
            span("a.rs", 1, 5, "", Some(0.9), &big_text),
            span("b.rs", 1, 5, "", Some(0.8), &big_text),
        ];
        // budget only large enough for the header + first span (each big-text span costs ~296
        // estimated tokens; the header costs ~40).
        let out = render_context(&result(spans), 400);
        assert!(out.contains("a.rs"));
        assert!(!out.contains("b.rs"));
    }

    #[test]
    fn tiny_budget_still_returns_header_only() {
        let spans = vec![span("a.rs", 1, 5, "", Some(0.9), &"x".repeat(1000))];
        let out = render_context(&result(spans), 1);
        assert!(out.starts_with("<!-- laya:"));
        assert!(!out.contains("a.rs"));
    }

    #[test]
    fn generous_budget_includes_every_span() {
        let spans = vec![
            span("a.rs", 1, 5, "", Some(0.9), "a"),
            span("b.rs", 1, 5, "", Some(0.8), "b"),
            span("c.rs", 1, 5, "", Some(0.7), "c"),
        ];
        let out = render_context(&result(spans), 10_000);
        assert!(out.contains("a.rs") && out.contains("b.rs") && out.contains("c.rs"));
    }

    fn related_item(path: &str, start: u32, end: u32, symbol: &str, relation: &str) -> Related {
        Related {
            path: path.to_string(),
            start_line: start,
            end_line: end,
            symbol: symbol.to_string(),
            relation: relation.to_string(),
        }
    }

    #[test]
    fn appends_related_section_after_the_ranked_spans() {
        let spans = vec![span("a.rs", 1, 5, "fn a", Some(0.9), "fn a() {}")];
        let related = vec![related_item(
            "src/x.rs",
            10,
            40,
            "fn foo",
            "calls `bar` (#1)",
        )];
        let out = render_context(&result_with_related(spans, related), 10_000);
        assert!(out.contains("Related by references:"));
        assert!(out.contains("- src/x.rs:10-40 fn foo — calls `bar` (#1)"));
        assert!(out.find("### a.rs").unwrap() < out.find("Related by references:").unwrap());
    }

    #[test]
    fn omits_related_section_when_empty() {
        let out = render_context(&result(vec![span("a.rs", 1, 5, "", None, "x")]), 10_000);
        assert!(!out.contains("Related by references:"));
    }

    #[test]
    fn related_line_omits_symbol_dash_when_symbol_is_empty() {
        let related = vec![related_item("src/x.rs", 10, 40, "", "calls `bar` (#1)")];
        let out = render_context(&result_with_related(vec![], related), 10_000);
        assert!(out.contains("- src/x.rs:10-40 — calls `bar` (#1)"));
    }

    #[test]
    fn render_context_opts_can_disable_related() {
        let related = vec![related_item(
            "src/x.rs",
            10,
            40,
            "fn foo",
            "calls `bar` (#1)",
        )];
        let r = result_with_related(vec![], related);
        let with_related = render_context_opts(&r, 10_000, true);
        let without_related = render_context_opts(&r, 10_000, false);
        assert!(with_related.contains("Related by references:"));
        assert!(!without_related.contains("Related by references:"));
        // the existing `render_context` signature keeps working and defaults to including it.
        assert_eq!(render_context(&r, 10_000), with_related);
    }
}
