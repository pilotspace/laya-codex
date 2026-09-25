//! Render a [`laya_core::QueryResult`] as the compact markdown block injected by hooks/MCP.

use laya_core::{QueryResult, RankedSpan, Related};

/// Rough token estimate used to fit output inside a caller-supplied budget (no tokenizer
/// dependency here; this matches the estimate the hooks use elsewhere in the project).
const CHARS_PER_TOKEN: f32 = 3.5;

pub(crate) fn estimate_tokens(s: &str) -> usize {
    (s.chars().count() as f32 / CHARS_PER_TOKEN).ceil() as usize
}

const HEADER: &str = "<!-- laya-codex: pre-ranked code spans for this task. \
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
    let mut inlined: Vec<&RankedSpan> = Vec::new();

    for span in &result.spans {
        let block = render_span(span);
        let block_tokens = estimate_tokens(&block);
        if used + block_tokens > budget_tokens || !fits(&out, &block, 0) {
            break;
        }
        out.push_str(&block);
        used += block_tokens;
        inlined.push(span);
    }
    if include_related {
        append_related(&mut out, &result.related, &inlined);
    }
    out
}

pub(crate) const COMPACT_HEADER: &str = "<!-- laya-codex: code located for this task by static analysis \
(tree-sitter chunks + BM25 + Laya relevance model). -->\n";

/// Said once, before the inlined code, only when the caller checked every inlined file against
/// the index for this prompt ([`crate::SizedContext::verified_current`]). Benchmark v2: agents
/// re-Read about 0.5 inlined blocks and grepped for about 1 already-given symbol per session.
pub const TRUST_LINE: &str = "The code blocks below are the exact current contents of those \
line ranges (checked against the files on disk for this prompt). Answer from them; don't Read \
or grep to re-check them.\n\n";

/// Appended to an identifier's definition line when every indexed use of it is shown, either
/// listed under "Definitions and uses:" or inside inlined code. [`append_related`] drops it when
/// the size cap cuts one of the identifier's lines.
pub(crate) const COMPLETE_USES: &str = " (all indexed uses shown)";

/// Ends the footer of every injection: which lookups the MCP `search` tool answers. Benchmark v3: 72%
/// of the remaining Greps came with the tests-and-callers follow-up, mostly for identifiers the
/// agent already knew; the tool description alone moved none of them (pilot, 0 calls).
macro_rules! search_hint {
    () => {
        "For the definition, callers or tests of a name, the laya-codex `search` tool with the \
name (e.g. `foo|Bar`) lists every matching line with its enclosing function or test, in one call.\n"
    };
}

pub(crate) const COMPACT_FOOTER: &str = concat!(
    "Use the code above directly. Search or Read further only for what is \
still missing, and prefer Read with offset/limit around the listed lines.\n",
    search_hint!()
);

/// The footer of an injection that inlines no code (a follow-up answered by lists).
pub(crate) const NO_CODE_FOOTER: &str = concat!(
    "For code you still need, Read with offset/limit around the listed lines.\n",
    search_hint!()
);

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
    let mut inlined: Vec<&RankedSpan> = Vec::new();
    for span in distinct_file_spans(&result.spans, full_spans) {
        let block = render_span(span);
        let t = estimate_tokens(&block);
        if used + t > budget_tokens || !fits(&out, &block, COMPACT_FOOTER.len()) {
            continue; // degrade to its map entry; a smaller later block may still fit
        }
        out.push_str(&block);
        used += t;
        inlined.push(span);
    }
    out.push_str(COMPACT_FOOTER);
    if include_related {
        append_related(&mut out, &result.related, &inlined);
    }
    out
}

/// Hard cap on everything a hook injects. Claude Code replaces hook output longer than 10,000
/// characters with a file path plus a preview, and the agent then Reads that file back, paying
/// for the context twice (seen for 16/20 injections of an earlier full-injection variant).
pub const MAX_INJECT_CHARS: usize = 9_500;

/// Whether appending `piece` to `out` keeps it (plus `reserve` chars still to come) under the cap.
pub(crate) fn fits(out: &str, piece: &str, reserve: usize) -> bool {
    out.len() + piece.len() + reserve <= MAX_INJECT_CHARS
}

/// The first span of each of the top `k` distinct files, in rank order: inlining one span from
/// each of three files covers more gold files than three spans that may share a file
/// (dev set: inlined-file recall 0.41 -> 0.46-0.52 at the same token cost).
pub(crate) fn distinct_file_spans(spans: &[RankedSpan], k: usize) -> Vec<&RankedSpan> {
    let mut out: Vec<&RankedSpan> = Vec::new();
    for s in spans {
        if out.len() == k {
            break;
        }
        if !out.iter().any(|o| o.path == s.path) {
            out.push(s);
        }
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
const USAGES_HEADER: &str = "\nDefinitions and uses:\n";

/// A single-line `Related` produced by the usage list (the definition or a use of an identifier
/// at one line, `symbol` holding that source line), as opposed to a span-level neighbour.
pub(crate) fn is_usage(r: &Related) -> bool {
    r.start_line == r.end_line
        && (r.relation.starts_with("definition of `") || r.relation.starts_with("use of `"))
}

/// Append the "Definitions and uses:" (grep-style lines) and "Related by references:" sections,
/// in that order, each omitted when empty. Lines are added while the output stays within
/// [`MAX_INJECT_CHARS`]; usages go first because they answer the Greps an agent would run next.
/// Usage lines inside `inlined` code blocks are skipped: the agent already sees them.
pub(crate) fn append_related(out: &mut String, related: &[Related], inlined: &[&RankedSpan]) {
    let (usages, neighbours): (Vec<&Related>, Vec<&Related>) =
        related.iter().partition(|r| is_usage(r));
    let visible = |r: &Related| {
        inlined
            .iter()
            .any(|s| s.path == r.path && (s.start_line..=s.end_line).contains(&r.start_line))
    };
    let shown: Vec<&Related> = usages.into_iter().filter(|r| !visible(r)).collect();
    let cut = identifiers_cut_by_the_cap(out, &shown);
    append_section(
        out,
        USAGES_HEADER,
        shown.iter().map(|r| {
            let claimed = r.relation.ends_with(COMPLETE_USES);
            let relation = if claimed && cut.contains(&usage_ident(&r.relation)) {
                r.relation.trim_end_matches(COMPLETE_USES)
            } else {
                r.relation.as_str()
            };
            usage_line(r, relation)
        }),
    );
    append_section(
        out,
        RELATED_HEADER,
        neighbours.iter().map(|r| render_related_line(r)),
    );
}

fn usage_line(r: &Related, relation: &str) -> String {
    format!(
        "- {}:{}: {} — {}\n",
        r.path, r.start_line, r.symbol, relation
    )
}

/// The identifier a usage relation is about: the text between the first pair of backticks.
fn usage_ident(relation: &str) -> &str {
    relation.split('`').nth(1).unwrap_or_default()
}

/// Identifiers with at least one usage line that would not fit under [`MAX_INJECT_CHARS`] after
/// `out` (lines as [`append_section`] adds them, claims included). Their "all uses shown" claim
/// would be false, so it is dropped.
fn identifiers_cut_by_the_cap<'a>(out: &str, lines: &[&'a Related]) -> Vec<&'a str> {
    let mut len = out.len();
    let mut cut = Vec::new();
    for (i, r) in lines.iter().enumerate() {
        let need = usage_line(r, &r.relation).len() + if i == 0 { USAGES_HEADER.len() } else { 0 };
        if cut.is_empty() && len + need <= MAX_INJECT_CHARS {
            len += need;
        } else {
            cut.push(usage_ident(&r.relation));
        }
    }
    cut
}

fn append_section(out: &mut String, header: &str, lines: impl Iterator<Item = String>) {
    let mut started = false;
    for line in lines {
        let need = if started { 0 } else { header.len() };
        if !fits(out, &line, need) {
            break;
        }
        if !started {
            out.push_str(header);
            started = true;
        }
        out.push_str(&line);
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

/// One line containing a looked-up name (whole, or as a part of a longer name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchLine {
    pub line: u32,
    /// The source line, trimmed (and cut to a readable length by the caller).
    pub text: String,
    /// The line defines one of the identifiers (its enclosing chunk defines it here).
    pub definition: bool,
}

/// The matching lines inside one enclosing symbol of a file (`symbol` empty = top level).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchGroup {
    pub symbol: String,
    pub lines: Vec<MatchLine>,
}

/// Every matching line of one file, grouped by enclosing symbol in line order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMatches {
    pub path: String,
    /// The groups carry enclosing symbols. `false`: one flat group (the file could not be parsed
    /// in time), shown without group headings.
    pub grouped: bool,
    pub groups: Vec<MatchGroup>,
}

impl FileMatches {
    pub fn line_count(&self) -> usize {
        self.groups.iter().map(|g| g.lines.len()).sum()
    }
}

/// An exact identifier lookup: every line naming one of `idents` in the indexed files under
/// `scope`, files in relevance order.
#[derive(Debug, Clone, PartialEq)]
pub struct IdentMatches {
    pub idents: Vec<String>,
    /// Repo-relative file or directory the lookup was restricted to.
    pub scope: Option<String>,
    pub files_searched: usize,
    /// Every file in scope was read (no time limit hit), so the lines found are all there are.
    pub scan_complete: bool,
    pub files: Vec<FileMatches>,
    /// Code of the most relevant definition, shown after the list when it fits.
    pub definition: Option<RankedSpan>,
}

/// Render an [`IdentMatches`] the way Grep prints matches (`line: text` under each file), grouped
/// by enclosing symbol, definitions marked, test files flagged, within [`MAX_INJECT_CHARS`].
/// Files that do not fit are listed with their match counts, so the file list stays complete;
/// the summary says "complete" only when every matching line is shown and the scan read every
/// file in scope.
pub fn render_matches(m: &IdentMatches) -> String {
    let names = m
        .idents
        .iter()
        .map(|i| format!("`{i}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let under = m
        .scope
        .as_deref()
        .map(|s| format!(" under {s}"))
        .unwrap_or_default();
    let n = m.files_searched;
    let mut out = format!(
        "<!-- laya-codex search: lines naming {names} (whole or inside a longer name) in the {n} \
indexed files{under}, by file and enclosing function or test, most relevant first. -->\n"
    );
    if m.files.is_empty() {
        out.push_str(&if m.scan_complete {
            format!("No match for {names} in the {n} indexed files{under}.\n")
        } else {
            format!(
                "No match for {names} in the {n} files read{under} before the time \
limit; the search may be incomplete.\n"
            )
        });
        return out;
    }

    let budget = MAX_INJECT_CHARS.saturating_sub(out.len() + MATCH_SUMMARY_MAX + MATCH_TAIL_MAX);
    let (body, shown) = match_body(&m.files, budget);
    let total: usize = m.files.iter().map(FileMatches::line_count).sum();
    out.push_str(&match_summary(m, total, &shown));
    out.push_str(&body);
    out.push_str(&match_tail(&m.files, &shown));
    if let Some(def) = &m.definition {
        let block = format!("\nDefinition:\n{}", render_span(def));
        if fits(&out, &block, 0) {
            out.push_str(&block);
        }
    }
    out
}

/// Room kept for the summary line and for the list of files not shown.
const MATCH_SUMMARY_MAX: usize = 400;
const MATCH_TAIL_MAX: usize = 1_500;

/// The grouped lines, file after file, while they fit in `budget` chars; also how many lines of
/// each file made it in.
fn match_body(files: &[FileMatches], budget: usize) -> (String, Vec<usize>) {
    let mut body = String::new();
    let mut shown = vec![0; files.len()];
    'files: for (i, f) in files.iter().enumerate() {
        let test = if crate::related::is_test_path(&f.path) {
            " (test file)"
        } else {
            ""
        };
        let heading = format!("\n{}{test}\n", f.path);
        if body.len() + heading.len() > budget {
            break;
        }
        body.push_str(&heading);
        for g in &f.groups {
            let group = if !f.grouped {
                String::new()
            } else if g.symbol.is_empty() {
                "  top level:\n".to_string()
            } else {
                format!("  in {}:\n", g.symbol)
            };
            let mut started = false;
            for l in &g.lines {
                let mark = if l.definition { "  [definition]" } else { "" };
                let text = format!("    {}: {}{mark}\n", l.line, l.text);
                let need = text.len() + if started { 0 } else { group.len() };
                if body.len() + need > budget {
                    break 'files;
                }
                if !started {
                    body.push_str(&group);
                    started = true;
                }
                body.push_str(&text);
                shown[i] += 1;
            }
        }
    }
    (body, shown)
}

fn match_summary(m: &IdentMatches, total: usize, shown_per_file: &[usize]) -> String {
    let shown: usize = shown_per_file.iter().sum();
    let defs = m
        .files
        .iter()
        .flat_map(|f| f.groups.iter().flat_map(|g| g.lines.iter()))
        .filter(|l| l.definition)
        .count();
    let tests = m
        .files
        .iter()
        .filter(|f| crate::related::is_test_path(&f.path))
        .count();
    let mut s = format!(
        "{total} matching lines in {} files (definitions: {defs}, test files: {tests}). ",
        m.files.len()
    );
    let flat = m
        .files
        .iter()
        .zip(shown_per_file)
        .filter(|(f, n)| **n > 0 && !f.grouped)
        .count();
    if shown == total && m.scan_complete && flat == 0 {
        s.push_str("Complete: every matching line is listed below.\n");
    } else if shown < total {
        s.push_str(&format!(
            "The first {shown} are shown (most relevant files first); the other files are counted at the end.\n"
        ));
    } else {
        s.push('\n');
    }
    if !m.scan_complete {
        s.push_str(&format!(
            "Stopped at the time limit after reading {} files: the list may be incomplete.\n",
            m.files_searched
        ));
    }
    if flat > 0 {
        s.push_str(&format!(
            "Time limit: {flat} files are listed without their enclosing functions.\n"
        ));
    }
    s
}

/// Files not (fully) shown, with their match counts, within [`MATCH_TAIL_MAX`] chars.
fn match_tail(files: &[FileMatches], shown: &[usize]) -> String {
    let rest: Vec<(&FileMatches, usize)> = files
        .iter()
        .zip(shown)
        .filter(|(f, s)| **s < f.line_count())
        .map(|(f, s)| (f, *s))
        .collect();
    if rest.is_empty() {
        return String::new();
    }
    let mut tail = String::from("\nNot shown (matching lines per file): ");
    for (k, (f, s)) in rest.iter().enumerate() {
        let item = if *s == 0 {
            format!("{} ({})", f.path, f.line_count())
        } else {
            format!("{} ({} of {} shown)", f.path, s, f.line_count())
        };
        let left = rest.len() - k;
        let more = format!("and {left} more files.");
        if tail.len() + item.len() + 2 + more.len() + 32 > MATCH_TAIL_MAX {
            if k > 0 {
                tail.push_str(", ");
            }
            tail.push_str(&more);
            tail.push('\n');
            return tail;
        }
        if k > 0 {
            tail.push_str(", ");
        }
        tail.push_str(&item);
    }
    tail.push_str(".\n");
    tail
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
mod match_tests {
    use super::*;

    fn line(n: u32, text: &str, definition: bool) -> MatchLine {
        MatchLine {
            line: n,
            text: text.into(),
            definition,
        }
    }

    fn file(path: &str, groups: Vec<(&str, Vec<MatchLine>)>) -> FileMatches {
        FileMatches {
            path: path.into(),
            grouped: true,
            groups: groups
                .into_iter()
                .map(|(s, lines)| MatchGroup {
                    symbol: s.into(),
                    lines,
                })
                .collect(),
        }
    }

    fn lookup(files: Vec<FileMatches>) -> IdentMatches {
        IdentMatches {
            idents: vec!["generateDigest".into()],
            scope: None,
            files_searched: 812,
            scan_complete: true,
            files,
            definition: None,
        }
    }

    fn digest_files() -> Vec<FileMatches> {
        vec![
            file(
                "src/middleware/etag/digest.ts",
                vec![(
                    "generateDigest",
                    vec![line(18, "export const generateDigest = async (", true)],
                )],
            ),
            file(
                "src/middleware/etag/index.ts",
                vec![
                    (
                        "",
                        vec![line(7, "import { generateDigest } from './digest'", false)],
                    ),
                    (
                        "etag",
                        vec![line(
                            104,
                            "const hash = await generateDigest(res.clone().body, generator)",
                            false,
                        )],
                    ),
                ],
            ),
            file(
                "src/middleware/etag/digest.test.ts",
                vec![(
                    "describe > it",
                    vec![line(12, "await generateDigest(stream, gen)", false)],
                )],
            ),
        ]
    }

    #[test]
    fn lists_every_match_grep_style_grouped_by_file_and_enclosing_symbol() {
        let out = render_matches(&lookup(digest_files()));
        assert!(out.contains("`generateDigest`"), "{out}");
        assert!(out.contains("812 indexed files"), "{out}");
        assert!(
            out.contains("4 matching lines in 3 files"),
            "counts are stated: {out}"
        );
        assert!(
            out.contains("Complete"),
            "every line shown and every file read: {out}"
        );
        let index = out.find("src/middleware/etag/index.ts").unwrap();
        let etag = out.find("in etag:").unwrap();
        let hit = out.find("104: const hash = await generateDigest").unwrap();
        assert!(
            index < etag && etag < hit,
            "file, then symbol, then line: {out}"
        );
        assert!(
            out.contains("7: import { generateDigest } from './digest'"),
            "{out}"
        );
        assert!(out.contains("top level:"), "{out}");
        assert!(
            out.contains("18: export const generateDigest = async (  [definition]"),
            "{out}"
        );
        assert!(
            out.contains("src/middleware/etag/digest.test.ts (test file)"),
            "{out}"
        );
        assert!(
            out.find("digest.ts\n").unwrap() < out.find("index.ts").unwrap(),
            "rank order kept"
        );
    }

    #[test]
    fn a_lookup_with_no_match_says_so_definitively() {
        let mut m = lookup(vec![]);
        m.scope = Some("src/client".into());
        let out = render_matches(&m);
        assert!(
            out.contains("No match for `generateDigest` in the 812 indexed files under src/client"),
            "{out}"
        );
    }

    #[test]
    fn an_interrupted_scan_never_claims_completeness() {
        let mut m = lookup(digest_files());
        m.scan_complete = false;
        let out = render_matches(&m);
        assert!(!out.contains("Complete"), "{out}");
        assert!(out.contains("may be incomplete"), "{out}");
    }

    #[test]
    fn overflow_stays_under_the_cap_and_lists_the_files_not_shown_with_counts() {
        let files: Vec<FileMatches> = (0..60)
            .map(|f| {
                file(
                    &format!("src/mod{f}/file.rs"),
                    vec![(
                        "fn caller",
                        (1..=20)
                            .map(|n| {
                                line(
                                    n,
                                    &format!("let x = generateDigest({n}); // padding text"),
                                    false,
                                )
                            })
                            .collect(),
                    )],
                )
            })
            .collect();
        let out = render_matches(&lookup(files));
        assert!(out.len() <= MAX_INJECT_CHARS, "{} chars", out.len());
        assert!(out.contains("1200 matching lines in 60 files"), "{out}");
        assert!(!out.contains("Complete"), "not every line is shown");
        assert!(out.contains("src/mod0/file.rs"), "top file shown");
        assert!(
            out.contains("src/mod59/file.rs (20)"),
            "the last file is listed with its count: {out}"
        );
    }

    #[test]
    fn a_file_listed_without_its_enclosing_functions_is_never_complete() {
        let mut files = digest_files();
        files[1].grouped = false;
        let out = render_matches(&lookup(files));
        assert!(!out.contains("Complete"), "{out}");
        assert!(
            out.contains("1 files are listed without their enclosing functions"),
            "{out}"
        );
        assert!(
            out.contains("src/middleware/etag/index.ts\n    7: import"),
            "flat lines, no group headings: {out}"
        );
    }

    #[test]
    fn a_long_list_of_files_not_shown_ends_with_how_many_more() {
        let files: Vec<FileMatches> = (0..400)
            .map(|f| {
                file(
                    &format!("src/deeply/nested/module{f}/file.rs"),
                    vec![("", vec![line(1, &"y".repeat(150), false)])],
                )
            })
            .collect();
        let out = render_matches(&lookup(files));
        assert!(out.len() <= MAX_INJECT_CHARS, "{} chars", out.len());
        let tail = out.split("Not shown").nth(1).unwrap();
        assert!(tail.contains("(1), and "), "{tail}");
        assert!(tail.trim_end().ends_with("more files."), "{tail}");
    }

    #[test]
    fn every_injection_footer_says_which_lookups_search_answers() {
        // Benchmark v3: 72% of the Greps left came with the tests-and-callers follow-up, 76% of
        // them for identifiers the agent already knew. The footer is the last thing it reads.
        for footer in [COMPACT_FOOTER, NO_CODE_FOOTER] {
            assert!(footer.contains("`search`"), "{footer}");
            for needle in ["callers", "tests", "every matching line"] {
                assert!(footer.contains(needle), "{needle:?} missing: {footer}");
            }
            assert!(footer.len() < 400, "kept short: {} chars", footer.len());
        }
    }

    #[test]
    fn a_definition_block_follows_the_list_only_when_it_fits() {
        let mut m = lookup(digest_files());
        m.definition = Some(RankedSpan {
            path: "src/middleware/etag/digest.ts".into(),
            start_line: 18,
            end_line: 20,
            symbol: "generateDigest".into(),
            p_relevant: None,
            score: 0.0,
            text: "export const generateDigest = async (\n  stream\n) => {}".into(),
        });
        let out = render_matches(&m);
        assert!(
            out.contains("### src/middleware/etag/digest.ts:18-20"),
            "{out}"
        );
        assert!(out.find("[definition]").unwrap() < out.find("### ").unwrap());

        m.definition.as_mut().unwrap().text = "x\n".repeat(6_000);
        let out = render_matches(&m);
        assert!(!out.contains("### "), "an oversized block is left out");
        assert!(out.len() <= MAX_INJECT_CHARS);
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

    fn usage(path: &str, line: u32, text: &str, relation: &str) -> Related {
        Related {
            path: path.into(),
            start_line: line,
            end_line: line,
            symbol: text.into(),
            relation: relation.into(),
        }
    }

    #[test]
    fn completeness_claim_survives_only_if_every_line_of_the_identifier_is_shown() {
        let claimed = format!("definition of `x`{COMPLETE_USES}");
        let lines = vec![
            usage("a.rs", 1, "fn x() {}", &claimed),
            usage("b.rs", 7, &"x(); ".repeat(20), "use of `x`"),
        ];
        let mut roomy = String::new();
        append_related(&mut roomy, &lines, &[]);
        assert!(roomy.contains(COMPLETE_USES), "{roomy}");

        // Leave room for the definition line only: the use line is cut, so "all uses shown"
        // would be false and must not be printed.
        let def_line = format!("- a.rs:1: fn x() {{}} — {claimed}\n");
        let mut tight = "z".repeat(MAX_INJECT_CHARS - USAGES_HEADER.len() - def_line.len());
        append_related(&mut tight, &lines, &[]);
        assert!(
            tight.contains("definition of `x`"),
            "the definition line still fits"
        );
        assert!(
            !tight.contains(COMPLETE_USES),
            "claim kept although a use was cut"
        );
        assert!(tight.len() <= MAX_INJECT_CHARS);
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
            scored: 0,
            offered: 0,
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
            scored: 0,
            offered: 0,
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
            scored: 0,
            offered: 0,
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
            scored: 0,
            offered: 0,
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
            scored: 0,
            offered: 0,
            related: vec![rel("src/x.rs", 10, "", "calls `bar` (#1)")],
        };
        let out = render_compact(&r, 1, 10_000);
        assert!(out.contains("- src/x.rs:10-15 — calls `bar` (#1)"));
    }

    fn big(path: &str, a: u32) -> RankedSpan {
        RankedSpan {
            text: "y".repeat(3_000),
            ..sp(path, a, "fn big")
        }
    }

    #[test]
    fn compact_output_never_exceeds_the_hook_char_cap() {
        let related: Vec<Related> = (0..80)
            .map(|i| {
                rel(
                    &format!("src/r{i}.rs"),
                    10,
                    "fn caller_with_a_long_name",
                    "calls `bar` (#1)",
                )
            })
            .collect();
        let r = QueryResult {
            spans: vec![
                big("a.rs", 1),
                big("b.rs", 1),
                big("c.rs", 1),
                sp("d.rs", 1, "fn d"),
            ],
            mode: RankMode::Laya,
            elapsed_ms: 1,
            candidates: 4,
            scored: 0,
            offered: 0,
            related,
        };
        let out = render_compact(&r, 3, 100_000);
        assert!(out.len() <= MAX_INJECT_CHARS, "{} chars", out.len());
        assert_eq!(out.matches("```").count() % 2, 0, "no code block is cut");
        assert!(out.contains(COMPACT_FOOTER.trim_end()), "footer survives");
        assert!(out.contains("4. d.rs"), "the map survives");
    }

    #[test]
    fn compact_inlines_the_top_distinct_files() {
        let r = QueryResult {
            spans: vec![
                sp("a.rs", 1, "fn a"),
                sp("a.rs", 40, "fn a2"),
                sp("b.rs", 5, "fn b"),
                sp("c.rs", 1, "fn c"),
            ],
            mode: RankMode::Laya,
            elapsed_ms: 1,
            candidates: 4,
            scored: 0,
            offered: 0,
            related: Vec::new(),
        };
        let out = render_compact(&r, 3, 100_000);
        for inlined in ["### a.rs:1-21", "### b.rs:5-25", "### c.rs:1-21"] {
            assert!(out.contains(inlined), "{inlined} missing");
        }
        assert!(
            !out.contains("### a.rs:40-60"),
            "second span of an inlined file is map-only"
        );
    }

    #[test]
    fn usages_render_grep_style_before_related() {
        let usage = Related {
            path: "src/shard/mod.rs".into(),
            start_line: 212,
            end_line: 212,
            symbol: "let lsn = recover_shard_v3(&dir, target_lsn)?;".into(),
            relation: "use of `recover_shard_v3`".into(),
        };
        let r = QueryResult {
            spans: vec![sp("a.rs", 1, "fn a")],
            mode: RankMode::Laya,
            elapsed_ms: 1,
            candidates: 1,
            scored: 0,
            offered: 0,
            related: vec![rel("src/x.rs", 10, "fn foo", "calls `bar` (#1)"), usage],
        };
        let out = render_compact(&r, 1, 10_000);
        assert!(out.contains("Definitions and uses:\n- src/shard/mod.rs:212: let lsn = recover_shard_v3(&dir, target_lsn)?; — use of `recover_shard_v3`"), "{out}");
        assert!(
            out.find("Definitions and uses:").unwrap()
                < out.find("Related by references:").unwrap()
        );
        assert!(!out.contains("212-212"));
    }

    #[test]
    fn render_compact_opts_can_disable_related() {
        let r = QueryResult {
            spans: vec![sp("a.rs", 1, "fn a")],
            mode: RankMode::Laya,
            elapsed_ms: 1,
            candidates: 1,
            scored: 0,
            offered: 0,
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
            scored: 0,
            offered: 0,
            related,
        }
    }

    #[test]
    fn header_always_present() {
        let out = render_context(&result(vec![]), 10_000);
        assert!(out.starts_with("<!-- laya-codex:"));
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
        assert!(out.starts_with("<!-- laya-codex:"));
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
