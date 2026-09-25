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
        "For callers or tests of a name, laya-codex `search` with `name|other` lists all.\n"
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

/// An exact identifier lookup: every line naming one of `idents` in the files on disk under
/// `scope`, files in relevance order.
#[derive(Debug, Clone, PartialEq)]
pub struct IdentMatches {
    pub idents: Vec<String>,
    /// Repo-relative file or directory the lookup was restricted to.
    pub scope: Option<String>,
    /// Files read and searched.
    pub files_read: usize,
    /// Files in scope that were not searched (over 1 MiB, unreadable or not UTF-8).
    pub files_skipped: usize,
    /// Every file in scope was read (no time limit hit), so the lines found are all there are.
    pub scan_complete: bool,
    pub files: Vec<FileMatches>,
    /// Code of the most relevant definition, shown after the list when it fits.
    pub definition: Option<RankedSpan>,
}

/// Cap on a lookup answer (it was the 9,500-char hook cap). Pilot at 8591f05: answers ran
/// 1.6-7.9k chars, about 2.4x the Greps they replaced, and the extra reading outweighed the Greps
/// and Reads they saved. v9 replay: 4,000 kept full coverage for 51% of Greps, 6,000 for 56-59%;
/// with the per-file limit the median answer is about 1.2x its Grep.
pub const MATCH_MAX_CHARS: usize = 6_000;
/// Room kept for the summary and for the list of files not shown.
const MATCH_SUMMARY_MAX: usize = 300;
const MATCH_TAIL_MAX: usize = 700;
/// At most this many lines of one file are shown; the rest are counted.
const MATCH_FILE_LINES: usize = 40;
/// Enclosing symbols are cut to this many chars (long `it('…')` test names).
const SYMBOL_MAX_CHARS: usize = 40;

/// Render an [`IdentMatches`] compactly: one summary line, then per file its path and the
/// matching lines as `line: text`, each marked with its enclosing function or test only where
/// that adds something (see [`group_lines`]), definitions marked `[def]`, test files `(test)`,
/// within [`MATCH_MAX_CHARS`]. When code matches, docs (Markdown and the like) are only counted.
/// Files that do not fit are listed with their match counts, so the file list stays complete;
/// the summary says "complete" only when every listed line is shown, every file in scope was
/// read and every shown file has its enclosing functions.
pub fn render_matches(m: &IdentMatches) -> String {
    let names = format!("`{}`", m.idents.join("|"));
    let under = m
        .scope
        .as_deref()
        .map(|s| format!(" under {s}"))
        .unwrap_or_default();
    let n = m.files_read;
    if m.files.is_empty() {
        let mut out = format!("No match for {names} in the {n} files read{under}.\n");
        out.push_str(&coverage_notes(m));
        return out;
    }
    let code_matched = m
        .files
        .iter()
        .any(|f| !crate::sizing::is_prose_path(&f.path));
    let listed: Vec<bool> = m
        .files
        .iter()
        .map(|f| !code_matched || !crate::sizing::is_prose_path(&f.path))
        .collect();
    let budget = MATCH_MAX_CHARS - MATCH_SUMMARY_MAX - MATCH_TAIL_MAX;
    let (body, shown) = match_body(&m.files, &listed, budget);
    let mut out = match_summary(m, &names, &under, &listed, &shown);
    out.push_str(&body);
    out.push_str(&match_tail(&m.files, &listed, &shown));
    if let Some(def) = &m.definition {
        let block = format!("\n{}", render_span(def));
        if out.len() + block.len() <= MATCH_MAX_CHARS {
            out.push_str(&block);
        }
    }
    out
}

/// `symbol`'s last segment without keywords, cut to [`SYMBOL_MAX_CHARS`]:
/// `class Response > def iter_text` -> `iter_text`.
pub(crate) fn short_symbol(symbol: &str) -> String {
    const KEYWORDS: &[&str] = &[
        "pub(crate) ",
        "pub ",
        "export ",
        "default ",
        "async ",
        "static ",
        "def ",
        "fn ",
        "class ",
        "function ",
        "struct ",
        "enum ",
        "trait ",
        "impl ",
        "interface ",
        "type ",
        "const ",
        "let ",
        "var ",
        "mod ",
    ];
    let mut s = symbol.rsplit(" > ").next().unwrap_or_default().trim();
    while let Some(rest) = KEYWORDS.iter().find_map(|k| s.strip_prefix(k)) {
        s = rest.trim_start();
    }
    if s.chars().count() <= SYMBOL_MAX_CHARS {
        return s.to_string();
    }
    let cut: String = s.chars().take(SYMBOL_MAX_CHARS - 1).collect();
    let cut = match cut.rfind(' ') {
        Some(at) if at > 10 => &cut[..at],
        _ => cut.as_str(),
    };
    format!("{cut}…")
}

/// The group's first line opens the symbol itself (`def test_x():` in `def test_x`, `it('y'` in
/// `it('y')`), so its other lines can nest under it without a marker.
fn opens(line: &str, short: &str) -> bool {
    match short.split_once('(') {
        Some((call, _)) if !call.is_empty() => line.trim_start().starts_with(&format!("{call}(")),
        _ => !short.is_empty() && line.contains(short),
    }
}

/// The rendered lines of one group. Top level (or a file listed flat): `  n: text`. Otherwise
/// the enclosing symbol is said once and only when the lines don't already show it: a group
/// whose first line opens the symbol nests the rest under it; a single line gets `  ‹symbol›`
/// appended; several lines get a `  ‹symbol›` header and are nested under it.
fn group_lines(f: &FileMatches, g: &MatchGroup) -> Vec<String> {
    let line = |l: &MatchLine, indent: &str, tag: &str| {
        let mark = if l.definition { " [def]" } else { "" };
        format!("{indent}{}: {}{mark}{tag}\n", l.line, l.text)
    };
    let short = short_symbol(&g.symbol);
    if !f.grouped || short.is_empty() {
        return g.lines.iter().map(|l| line(l, " ", "")).collect();
    }
    let first = &g.lines[0];
    if opens(&first.text, &short) {
        let mut out = vec![line(first, " ", "")];
        out.extend(g.lines[1..].iter().map(|l| line(l, "  ", "")));
        return out;
    }
    if g.lines.len() == 1 {
        return vec![line(first, " ", &format!(" ‹{short}›"))];
    }
    let mut out = vec![format!(" ‹{short}›\n")];
    out.extend(g.lines.iter().map(|l| line(l, "  ", "")));
    out
}

/// The listed files, file after file, while they fit in `budget` chars; also how many lines of
/// each file made it in. A symbol header is only added together with its first line.
fn match_body(files: &[FileMatches], listed: &[bool], budget: usize) -> (String, Vec<usize>) {
    let mut body = String::new();
    let mut shown = vec![0; files.len()];
    'files: for (i, f) in files.iter().enumerate() {
        if !listed[i] {
            continue;
        }
        let test = if crate::related::is_test_path(&f.path) {
            " (test)"
        } else {
            ""
        };
        let heading = format!("{}{test}\n", f.path);
        if body.len() + heading.len() > budget {
            break;
        }
        body.push_str(&heading);
        'groups: for g in &f.groups {
            let mut pending = String::new();
            for text in group_lines(f, g) {
                if !text.trim_start().starts_with(|c: char| c.is_ascii_digit()) {
                    pending = text; // a header: added with the next line
                    continue;
                }
                if shown[i] == MATCH_FILE_LINES {
                    break 'groups;
                }
                if body.len() + pending.len() + text.len() > budget {
                    break 'files;
                }
                body.push_str(&pending);
                pending.clear();
                body.push_str(&text);
                shown[i] += 1;
            }
        }
    }
    (body, shown)
}

fn match_summary(
    m: &IdentMatches,
    names: &str,
    under: &str,
    listed: &[bool],
    shown_per_file: &[usize],
) -> String {
    let total: usize = m.files.iter().map(FileMatches::line_count).sum();
    let listed_total: usize = m
        .files
        .iter()
        .zip(listed)
        .filter(|(_, l)| **l)
        .map(|(f, _)| f.line_count())
        .sum();
    let shown: usize = shown_per_file.iter().sum();
    let tests = m
        .files
        .iter()
        .filter(|f| crate::related::is_test_path(&f.path))
        .count();
    let tests = match tests {
        0 => String::new(),
        1 => " (1 test)".to_string(),
        t => format!(" ({t} tests)"),
    };
    let flat = m
        .files
        .iter()
        .zip(shown_per_file)
        .filter(|(f, n)| **n > 0 && !f.grouped)
        .count();
    let docs_counted = listed.iter().any(|l| !l);
    let mut s = format!(
        "{names}: {total} lines in {} files{tests} of {} files read{under}",
        m.files.len(),
        m.files_read
    );
    if shown == listed_total && m.scan_complete && m.files_skipped == 0 && flat == 0 {
        s.push_str(if docs_counted {
            "; complete, docs counted.\n"
        } else {
            "; complete.\n"
        });
    } else if shown < listed_total {
        s.push_str(&format!("; first {shown} shown, the rest counted below.\n"));
    } else {
        s.push_str(".\n");
    }
    s.push_str(&coverage_notes(m));
    if flat > 0 {
        s.push_str(&format!(
            "Time limit: {flat} files listed without enclosing functions.\n"
        ));
    }
    s
}

/// What the search did not cover: files skipped, and a scan cut by the time limit.
fn coverage_notes(m: &IdentMatches) -> String {
    let mut s = String::new();
    if m.files_skipped > 0 {
        s.push_str(&format!(
            "{} files not searched (over 1 MiB, unreadable or not UTF-8).\n",
            m.files_skipped
        ));
    }
    if !m.scan_complete {
        s.push_str(&format!(
            "Stopped at the time limit after reading {} files: the list may be incomplete.\n",
            m.files_read
        ));
    }
    s
}

/// Listed files not (fully) shown, then docs that were only counted, with their match counts,
/// within [`MATCH_TAIL_MAX`] chars.
fn match_tail(files: &[FileMatches], listed: &[bool], shown: &[usize]) -> String {
    let not_shown: Vec<String> = files
        .iter()
        .zip(listed.iter().zip(shown))
        .filter(|(f, (l, s))| **l && **s < f.line_count())
        .map(|(f, (_, s))| {
            if *s == 0 {
                format!("{} ({})", f.path, f.line_count())
            } else {
                format!("{} ({} of {} shown)", f.path, s, f.line_count())
            }
        })
        .collect();
    let docs: Vec<String> = files
        .iter()
        .zip(listed)
        .filter(|(_, l)| !**l)
        .map(|(f, _)| format!("{} ({})", f.path, f.line_count()))
        .collect();
    let mut tail = String::new();
    let mut room = MATCH_TAIL_MAX;
    for (label, items) in [("Not shown: ", not_shown), ("Docs: ", docs)] {
        if items.is_empty() {
            continue;
        }
        let line = list_within(label, &items, room);
        room = room.saturating_sub(line.len());
        tail.push_str(&line);
    }
    tail
}

/// `label` + `items` joined by ", " within `room` chars, ending with "and N more files." when cut.
fn list_within(label: &str, items: &[String], room: usize) -> String {
    let mut out = String::from(label);
    for (k, item) in items.iter().enumerate() {
        let more = format!("and {} more files.", items.len() - k);
        if out.len() + item.len() + 2 + more.len() + 4 > room {
            if k > 0 {
                out.push_str(", ");
            }
            out.push_str(&more);
            out.push('\n');
            return out;
        }
        if k > 0 {
            out.push_str(", ");
        }
        out.push_str(item);
    }
    out.push_str(".\n");
    out
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
            files_read: 812,
            files_skipped: 0,
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
    fn lists_matches_compactly_by_file_with_short_enclosing_symbols() {
        let out = render_matches(&lookup(digest_files()));
        assert!(!out.contains("<!--"), "no preamble: {out}");
        assert!(
            out.starts_with(
                "`generateDigest`: 4 lines in 3 files (1 test) of 812 files read; complete.\n"
            ),
            "{out}"
        );
        // The opener of its own definition carries no marker; one line in another symbol gets
        // the symbol inline; top-level lines get nothing.
        assert!(
            out.contains(
                "src/middleware/etag/digest.ts\n 18: export const generateDigest = async ( [def]\n"
            ),
            "{out}"
        );
        assert!(
            out.contains(" 7: import { generateDigest } from './digest'\n"),
            "{out}"
        );
        assert!(
            out.contains(
                " 104: const hash = await generateDigest(res.clone().body, generator) ‹etag›\n"
            ),
            "{out}"
        );
        assert!(
            out.contains("src/middleware/etag/digest.test.ts (test)\n"),
            "{out}"
        );
        assert!(
            out.find("digest.ts\n").unwrap() < out.find("index.ts").unwrap(),
            "rank order kept"
        );
    }

    #[test]
    fn several_lines_in_one_symbol_share_a_header_and_an_opener_nests_its_lines() {
        let f = file(
            "tests/models/test_responses.py",
            vec![
                (
                    "def test_iter_text",
                    vec![
                        line(600, "def test_iter_text():", false),
                        line(607, "for part in response.iter_text():", false),
                    ],
                ),
                (
                    "class Response > def iter_lines",
                    vec![
                        line(929, "for text in self.iter_text():", false),
                        line(931, "yield self.iter_text()", false),
                    ],
                ),
            ],
        );
        let out = render_matches(&lookup(vec![f]));
        assert!(
            out.contains(" 600: def test_iter_text():\n  607: for part in response.iter_text():\n"),
            "an opener's own lines nest under it: {out}"
        );
        assert!(
            out.contains(" ‹iter_lines›\n  929: for text in self.iter_text():\n  931: yield"),
            "a shared header, short symbol: {out}"
        );
    }

    #[test]
    fn at_most_forty_lines_of_one_file_are_shown_and_the_rest_counted() {
        // v9 replay: a 25-line limit cost 7 points of full coverage, 40 lines cost none of the
        // 6,000-char cap's; a file's 41st line is rarely the one used.
        let f = file(
            "src/big.rs",
            vec![(
                "",
                (1..=60)
                    .map(|n| line(n, "generateDigest()", false))
                    .collect(),
            )],
        );
        let out = render_matches(&lookup(vec![
            f,
            file(
                "src/b.rs",
                vec![("", vec![line(3, "generateDigest", false)])],
            ),
        ]));
        assert!(out.contains(" 40: generateDigest()\nsrc/b.rs\n"), "{out}");
        assert!(!out.contains(" 41: "), "{out}");
        assert!(
            out.contains("Not shown: src/big.rs (40 of 60 shown)."),
            "{out}"
        );
        assert!(out.contains("first 41 shown"), "{out}");
    }

    #[test]
    fn short_symbols_drop_the_outer_path_keywords_and_long_names() {
        assert_eq!(short_symbol("class Response > def iter_text"), "iter_text");
        assert_eq!(short_symbol("impl Store for MoonStore > fn get"), "get");
        assert_eq!(short_symbol("async def aiter_text"), "aiter_text");
        assert_eq!(short_symbol("mod tests > fn test_replay"), "test_replay");
        assert_eq!(
            short_symbol(
                "describe('CORS') > it('Append \"Origin\" to Vary header on OPTIONS preflight')"
            ),
            "it('Append \"Origin\" to Vary header on…"
        );
        assert_eq!(short_symbol(""), "");
    }

    #[test]
    fn docs_are_counted_not_listed_when_code_matches() {
        let mut files = digest_files();
        files.push(file(
            "CHANGELOG.md",
            vec![(
                "Fixed",
                vec![line(93, "* generateDigest keeps bytes", false)],
            )],
        ));
        let out = render_matches(&lookup(files));
        assert!(!out.contains("93: "), "{out}");
        assert!(out.contains("Docs: CHANGELOG.md (1)"), "{out}");
        assert!(
            out.contains("5 lines in 4 files (1 test) of 812 files read; complete, docs counted."),
            "{out}"
        );
        let docs_only = render_matches(&lookup(vec![file(
            "README.md",
            vec![("", vec![line(3, "generateDigest", false)])],
        )]));
        assert!(docs_only.contains(" 3: generateDigest"), "{docs_only}");
    }

    #[test]
    fn a_lookup_with_no_match_says_so_definitively() {
        let mut m = lookup(vec![]);
        m.scope = Some("src/client".into());
        let out = render_matches(&m);
        assert!(
            out.contains("No match for `generateDigest` in the 812 files read under src/client."),
            "{out}"
        );
    }

    #[test]
    fn an_interrupted_scan_never_claims_completeness() {
        let mut m = lookup(digest_files());
        m.scan_complete = false;
        let out = render_matches(&m);
        assert!(!out.contains("; complete"), "{out}");
        assert!(out.contains("may be incomplete"), "{out}");
    }

    #[test]
    fn skipped_files_rule_out_complete() {
        let mut m = lookup(digest_files());
        m.files_skipped = 2;
        let out = render_matches(&m);
        assert!(!out.contains("; complete"), "{out}");
        assert!(out.contains("2 files not searched"), "{out}");
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
        assert!(out.len() <= MATCH_MAX_CHARS, "{} chars", out.len());
        assert!(out.contains("1200 lines in 60 files"), "{out}");
        assert!(!out.contains("; complete"), "not every line is shown");
        assert!(out.contains("src/mod0/file.rs"), "top file shown");
        assert!(
            out.contains("Not shown: src/mod") && out.contains(" (20), "),
            "files not shown are listed with their counts: {out}"
        );
        assert!(out.trim_end().ends_with("more files."), "{out}");
    }

    #[test]
    fn a_file_listed_without_its_enclosing_functions_is_never_complete() {
        let mut files = digest_files();
        files[1].grouped = false;
        let out = render_matches(&lookup(files));
        assert!(!out.contains("; complete"), "{out}");
        assert!(
            out.contains("1 files listed without enclosing functions"),
            "{out}"
        );
        assert!(
            out.contains("src/middleware/etag/index.ts\n 7: import"),
            "flat lines: {out}"
        );
        assert!(!out.contains("‹etag›"), "{out}");
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
        assert!(out.len() <= MATCH_MAX_CHARS, "{} chars", out.len());
        let tail = out.split("Not shown: ").nth(1).unwrap();
        assert!(tail.contains("(1), and "), "{tail}");
        assert!(tail.trim_end().ends_with("more files."), "{tail}");
    }

    #[test]
    fn every_injection_footer_says_which_lookups_search_answers_in_one_short_line() {
        // Pilot at 8591f05: every `search` call opened the tests-and-callers prompt, right after
        // this line; at 184 chars it was 6% of what the hook injected.
        for footer in [COMPACT_FOOTER, NO_CODE_FOOTER] {
            let hint = footer.lines().last().unwrap();
            assert!(hint.contains("`search`"), "{hint}");
            assert!(hint.contains("callers") && hint.contains("tests"), "{hint}");
            assert!(
                hint.chars().count() <= 80,
                "{} chars: {hint}",
                hint.chars().count()
            );
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
        assert!(out.find("[def]").unwrap() < out.find("### ").unwrap());

        m.definition.as_mut().unwrap().text = "x\n".repeat(3_000);
        let out = render_matches(&m);
        assert!(!out.contains("### "), "an oversized block is left out");
        assert!(out.len() <= MATCH_MAX_CHARS);
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
