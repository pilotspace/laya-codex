//! Render a [`laya_core::QueryResult`] as the compact markdown block injected by hooks/MCP.

use laya_core::{QueryResult, RankedSpan};

/// Rough token estimate used to fit output inside a caller-supplied budget (no tokenizer
/// dependency here; this matches the estimate the hooks use elsewhere in the project).
const CHARS_PER_TOKEN: f32 = 3.5;

fn estimate_tokens(s: &str) -> usize {
    (s.chars().count() as f32 / CHARS_PER_TOKEN).ceil() as usize
}

const HEADER: &str = "<!-- laya: pre-ranked code spans for this task. \
Prefer these ranges over exploring; use Read with offset/limit to see more of a file. -->\n\n";

/// Render `result` as a markdown block: a one-line header, then one
/// `### path:start-end — symbol (p=0.83)` heading + fenced code block per span, in the input
/// (score-descending) order. Stops adding spans before the running total would exceed
/// `budget_tokens` (estimated as `chars / 3.5`); the header is always included.
pub fn render_context(result: &QueryResult, budget_tokens: usize) -> String {
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
    out
}

fn render_span(span: &RankedSpan) -> String {
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
mod tests {
    use super::*;
    use laya_core::RankMode;

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
        QueryResult {
            spans,
            mode: RankMode::Laya,
            elapsed_ms: 5,
            candidates: 3,
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
}
