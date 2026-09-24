//! Turn a raw prompt into the three retrieval signals candidate generation fuses:
//! BM25 terms, explicit identifiers (for `Store::chunks_defining`) and file-path mentions
//! (for the path-boost signal, resolved against `Store::list_files`).

use laya_core::ident;

/// Signals extracted from a prompt, ready to hand to `Retriever` candidate generation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PromptSignals {
    /// Normalized BM25 query terms (`laya_core::ident::terms`).
    pub terms: Vec<String>,
    /// Explicit identifiers mentioned in the prompt (original case, deduped, sorted):
    /// CamelCase (`FooBar`), snake_case (`parse_config`), call sites (`foo()`), and scoped
    /// paths (`Foo::bar` splits into `Foo` and `bar`).
    pub identifiers: Vec<String>,
    /// File-path fragments mentioned in the prompt (`src/x.rs`, `x.py`), deduped and sorted.
    pub paths: Vec<String>,
}

/// Suffixes that make a `name.ext` token look like a source/config file mention.
const PATH_EXTENSIONS: &[&str] = &[
    "rs", "py", "ts", "tsx", "js", "jsx", "mjs", "cjs", "go", "java", "c", "h", "cc", "cpp", "cxx",
    "hpp", "cs", "rb", "php", "kt", "kts", "swift", "md", "toml", "json", "yaml", "yml",
];

/// Words that carry no retrieval signal in a coding-agent prompt: English function words and
/// the instruction/meta vocabulary agents and users wrap tasks in ("find the source code…",
/// "be efficient", "comma-separated paths"). BM25 picks the *rarest* query terms, and prose like
/// `comma` or `efficient` is rare in code, so without this list the wrapper outranks the task.
/// Only BM25 terms are filtered; identifiers (`find_files`) and path mentions are kept.
const PROMPT_STOPLIST: &[&str] = &[
    // function words
    "a",
    "about",
    "above",
    "after",
    "again",
    "all",
    "also",
    "am",
    "an",
    "and",
    "any",
    "are",
    "as",
    "at",
    "be",
    "because",
    "been",
    "before",
    "being",
    "below",
    "between",
    "both",
    "but",
    "by",
    "can",
    "could",
    "did",
    "do",
    "does",
    "doing",
    "done",
    "down",
    "during",
    "each",
    "either",
    "else",
    "etc",
    "ever",
    "every",
    "few",
    "for",
    "from",
    "further",
    "had",
    "has",
    "have",
    "having",
    "he",
    "her",
    "here",
    "his",
    "how",
    "however",
    "i",
    "if",
    "in",
    "into",
    "is",
    "it",
    "its",
    "itself",
    "just",
    "let",
    "lets",
    "me",
    "might",
    "more",
    "most",
    "much",
    "must",
    "my",
    "no",
    "nor",
    "not",
    "now",
    "of",
    "off",
    "on",
    "once",
    "one",
    "only",
    "or",
    "other",
    "our",
    "out",
    "over",
    "own",
    "please",
    "same",
    "shall",
    "she",
    "should",
    "so",
    "some",
    "such",
    "than",
    "that",
    "the",
    "their",
    "them",
    "then",
    "there",
    "these",
    "they",
    "this",
    "those",
    "through",
    "thus",
    "to",
    "too",
    "under",
    "until",
    "up",
    "upon",
    "us",
    "very",
    "via",
    "was",
    "we",
    "were",
    "what",
    "when",
    "where",
    "whether",
    "which",
    "while",
    "who",
    "whom",
    "whose",
    "why",
    "will",
    "with",
    "within",
    "without",
    "would",
    "yes",
    "yet",
    "you",
    "your",
    "yours",
    // instruction / meta vocabulary of agent prompts. Words that are also common code-domain
    // vocabulary (path, file, line, read, list, code, source, change, ...) are deliberately NOT
    // here: "fast-path" or "read path" are task content, and common words rarely win the
    // rarest-first BM25 term selection anyway.
    "answer",
    "briefly",
    "codebase",
    "comma",
    "describe",
    "efficient",
    "efficiently",
    "exactly",
    "explain",
    "find",
    "following",
    "give",
    "help",
    "identify",
    "implements",
    "look",
    "need",
    "needs",
    "please",
    "project",
    "relevant",
    "repository",
    "separated",
    "show",
    "tell",
    "understand",
    "want",
];

fn is_stopword(term: &str) -> bool {
    PROMPT_STOPLIST.contains(&term)
}

/// Distinct BM25 terms left after the stoplist: how much task content a prompt carries on its
/// own (a follow-up like "now find the tests for it" carries little).
pub fn content_terms(prompt: &str) -> usize {
    let mut t = extract_signals(prompt).terms;
    t.sort();
    t.dedup();
    t.len()
}

/// Words that unambiguously point back to earlier conversation ("for the same change", "the
/// previous function"). Pronouns like "it"/"this" are not enough: new tasks use them too
/// ("fix it so that the wal replay…").
const BACK_REFERENCES: &[&str] = &["same", "above", "previous", "earlier", "aforementioned"];

/// Whether `prompt` is a follow-up that depends on the session topic: almost no task content,
/// or an explicit back-reference without naming any code (identifiers or paths make a prompt
/// self-contained). Content-term counts alone do not work: agent prompts carry long instruction
/// tails ("End your answer with … FILES: <comma-separated paths>").
pub fn is_follow_up(prompt: &str) -> bool {
    if content_terms(prompt) < 4 {
        return true;
    }
    let sig = extract_signals(prompt);
    let refers_back =
        ident::words(prompt).any(|w| BACK_REFERENCES.contains(&w.to_ascii_lowercase().as_str()));
    refers_back && sig.identifiers.is_empty() && sig.paths.is_empty()
}

/// What a follow-up asks for beyond the session's topic: the tests that cover it and the code
/// that calls it. Used to render a follow-up as answers to those questions instead of more
/// blocks of the topic's lower-ranked code.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FollowUpIntent {
    pub tests: bool,
    pub callers: bool,
}

impl FollowUpIntent {
    pub fn any(&self) -> bool {
        self.tests || self.callers
    }
}

const TEST_WORDS: &[&str] = &["test", "tests", "tested", "testing", "coverage"];
const CALLER_WORDS: &[&str] = &[
    "caller",
    "callers",
    "callsite",
    "callsites",
    "invoke",
    "invokes",
    "invoked",
    "usage",
    "usages",
];

/// [`FollowUpIntent`] of `prompt`, from whole words. Callers: the words above, "call site(s)",
/// "used by" / "referenced by", or "where" with "used" / "referenced". Everyday words such as
/// "uses", "cover" or "spec" alone are not enough.
pub fn follow_up_intent(prompt: &str) -> FollowUpIntent {
    let lower = prompt.to_ascii_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let pair = |a: &str, b: &[&str]| words.windows(2).any(|w| w[0] == a && b.contains(&w[1]));
    let used = |w: &&str| *w == "used" || *w == "referenced";
    let callers = words.iter().any(|w| CALLER_WORDS.contains(w))
        || pair("call", &["site", "sites"])
        || pair("used", &["by"])
        || pair("referenced", &["by"])
        || (words.contains(&"where") && words.iter().any(used));
    FollowUpIntent {
        tests: words.iter().any(|w| TEST_WORDS.contains(w)),
        callers,
    }
}

/// Quoted passages with at least this many distinct content terms are a task, not a name.
const MIN_QUOTED_TASK_TERMS: usize = 3;

/// Phrases that mark a sentence as an instruction about the answer or the process rather
/// than the code ("Be efficient: …", "End your answer with … of the form").
const OUTPUT_FORMAT_MARKERS: &[&str] = &[
    "your answer",
    "your response",
    "of the form",
    "answer with",
    "respond with",
    "reply with",
    "output format",
    "be efficient",
    "be concise",
    "be brief",
    "read only what",
];

/// The part of `prompt` that describes the task, for BM25 terms and the Laya scorer.
///
/// Agent and benchmark prompts wrap the task in instructions ("find the source code that …
/// FILES: <paths>") whose code-domain words (`source`, `change`, `files`, `paths`) the stoplist
/// must keep, because in a task they are content. So structure decides instead: a quoted
/// passage of at least [`MIN_QUOTED_TASK_TERMS`] content terms is the task; otherwise
/// sentences that instruct the answer's format are dropped. Falls back to the whole prompt
/// when nothing would remain.
pub fn task_focus(prompt: &str) -> String {
    let quoted: Vec<&str> = quoted_passages(prompt)
        .into_iter()
        .filter(|q| content_terms(q) >= MIN_QUOTED_TASK_TERMS)
        .collect();
    if !quoted.is_empty() {
        return quoted.join("\n");
    }
    let kept: Vec<&str> = sentences(prompt)
        .into_iter()
        .filter(|s| !is_output_instruction(s))
        .collect();
    if kept.is_empty() || content_terms(&kept.join(" ")) == 0 {
        return prompt.to_string();
    }
    kept.join(" ")
}

/// Text between straight (`"…"`) or curly (`“…”`) double quotes, trimmed, non-empty.
fn quoted_passages(prompt: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut open: Option<(usize, char)> = None;
    for (i, c) in prompt.char_indices() {
        match (open, c) {
            (None, '"') => open = Some((i + 1, '"')),
            (None, '\u{201c}') => open = Some((i + c.len_utf8(), '\u{201d}')),
            (Some((start, close)), c) if c == close => {
                let q = prompt[start..i].trim();
                if !q.is_empty() {
                    out.push(q);
                }
                open = None;
            }
            _ => {}
        }
    }
    out
}

/// Sentences of `prompt`: split after `.`/`!`/`?` followed by whitespace, and at line breaks.
fn sentences(prompt: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = prompt.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let next_is_space = chars.peek().is_none_or(|&(_, n)| n.is_whitespace());
        let end = match c {
            '\n' => Some(i),
            '.' | '!' | '?' if next_is_space => Some(i + 1),
            _ => None,
        };
        if let Some(end) = end {
            let s = prompt[start..end].trim();
            if !s.is_empty() {
                out.push(s);
            }
            start = end;
        }
    }
    let s = prompt[start..].trim();
    if !s.is_empty() {
        out.push(s);
    }
    out
}

fn is_output_instruction(sentence: &str) -> bool {
    let lower = sentence.to_lowercase();
    lower.starts_with("files:") || OUTPUT_FORMAT_MARKERS.iter().any(|m| lower.contains(m))
}

/// Extract [`PromptSignals`] from a raw user prompt.
pub fn extract_signals(prompt: &str) -> PromptSignals {
    let terms: Vec<String> = ident::terms(prompt)
        .into_iter()
        .filter(|t| !is_stopword(t))
        .collect();
    let mut identifiers = Vec::new();
    let mut paths = Vec::new();

    for raw in prompt.split_whitespace() {
        let token = raw.trim_matches(|c: char| {
            !(c.is_ascii_alphanumeric()
                || c == '_'
                || c == '/'
                || c == '.'
                || c == ':'
                || c == '('
                || c == ')')
        });
        if token.is_empty() {
            continue;
        }
        if let Some(path) = path_mention(token) {
            paths.push(path);
            continue; // a path mention is not also treated as an identifier
        }
        identifiers.extend(scoped_identifiers(token));
    }

    identifiers.sort();
    identifiers.dedup();
    paths.sort();
    paths.dedup();

    PromptSignals {
        terms,
        identifiers,
        paths,
    }
}

/// If `token` looks like a path/filename mention (has a known source/config extension and a
/// non-empty base name), return its normalized (trailing punctuation stripped) form.
fn path_mention(token: &str) -> Option<String> {
    let core = token.trim_end_matches([',', ';', ')', '(', '.']);
    let (_, ext) = core.rsplit_once('.')?;
    if !PATH_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()) {
        return None;
    }
    let name_part = core.rsplit('/').next().unwrap_or(core);
    let base = name_part
        .rsplit_once('.')
        .map(|(b, _)| b)
        .unwrap_or(name_part);
    if base.is_empty() {
        return None;
    }
    Some(core.to_string())
}

/// Explicit identifiers in `token`: split on `::` (scoped paths), strip `()` call markers, and
/// keep segments that look intentional (mixed-case, snake_case, or an explicit call site) rather
/// than ordinary prose words.
fn scoped_identifiers(token: &str) -> Vec<String> {
    let core = token.trim_end_matches([',', ';', '.']);
    // `::` scoping is itself an explicit signal (like a `()` call site), so every segment of a
    // scoped path counts as an identifier regardless of its casing (`foo::bar`, `Foo::Bar`).
    let is_scoped = core.contains("::");
    let mut out = Vec::new();
    for part in core.split("::") {
        if let Some(name) = part.strip_suffix("()") {
            if !name.is_empty() && is_identifier_shaped(name) {
                out.push(name.to_string());
            }
        } else if (!part.is_empty() && is_scoped && is_identifier_shaped(part))
            || is_explicit_identifier(part)
        {
            out.push(part.to_string());
        }
    }
    out
}

fn is_identifier_shaped(word: &str) -> bool {
    word.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A word counts as an *explicit* identifier (as opposed to ordinary prose) when it mixes
/// case with a real internal hump (`FooBar`, `fooBar`, `HTTPServer`) or uses `snake_case`. A
/// single leading-capital word (`Foo`, `Store`) is ordinary Title Case prose and is left to the
/// BM25 `terms` signal; an all-caps acronym (`TODO`, `NASA`) with no lowercase letter is too.
fn is_explicit_identifier(word: &str) -> bool {
    if !is_identifier_shaped(word) {
        return false;
    }
    let has_underscore = word.contains('_');
    let has_lower = word.chars().any(|c| c.is_ascii_lowercase());
    let has_internal_upper = word.chars().skip(1).any(|c| c.is_ascii_uppercase());
    has_underscore || (has_internal_upper && has_lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_prompt_yields_no_signals() {
        let s = extract_signals("");
        assert!(s.terms.is_empty());
        assert!(s.identifiers.is_empty());
        assert!(s.paths.is_empty());
    }

    #[test]
    fn only_stopwords_yields_terms_but_no_identifiers_or_paths() {
        let s = extract_signals("the quick fix for a bug");
        assert!(!s.terms.is_empty());
        assert!(s.identifiers.is_empty());
        assert!(s.paths.is_empty());
    }

    #[test]
    fn instruction_boilerplate_is_not_searched() {
        let prompt = "In this repository, find the source code that implements or would need to change for the \
            following change, and briefly explain how it works:\n\n\"fix(vector): address three post-review issues \
            in mmap budget\"\n\nBe efficient: read only what you need. End your answer with one line exactly of \
            the form\nFILES: <comma-separated repo-relative paths of the most relevant source files>";
        let s = extract_signals(prompt);
        for kept in [
            "vector", "mmap", "budget", "post", "review", "issues", "address",
        ] {
            assert!(
                s.terms.contains(&kept.to_string()),
                "{kept} missing from {:?}",
                s.terms
            );
        }
        for dropped in [
            "repository",
            "find",
            "explain",
            "efficient",
            "comma",
            "separated",
            "answer",
            "relevant",
            "briefly",
            "exactly",
        ] {
            assert!(
                !s.terms.contains(&dropped.to_string()),
                "{dropped} kept in {:?}",
                s.terms
            );
        }
    }

    #[test]
    fn other_phrasings_of_instructions_are_dropped_too() {
        let s = extract_signals(
            "Can you please show me where in the codebase we should look to understand how the WAL replay handles torn writes?",
        );
        assert_eq!(
            content_terms("Can you please show me where in the codebase we should look"),
            0
        );
        for kept in ["wal", "replay", "handles", "torn", "writes"] {
            assert!(
                s.terms.contains(&kept.to_string()),
                "{kept} missing from {:?}",
                s.terms
            );
        }
    }

    #[test]
    fn code_domain_words_are_kept() {
        let s = extract_signals(
            "drop dead cross-shard fast-path metrics; fix the read path and file lines",
        );
        for kept in ["path", "fast", "read", "file", "lines"] {
            assert!(
                s.terms.contains(&kept.to_string()),
                "{kept} missing from {:?}",
                s.terms
            );
        }
    }

    #[test]
    fn identifiers_and_paths_survive_the_stoplist() {
        let s = extract_signals("why does find_files() in src/code.rs skip Source::File?");
        assert!(s.identifiers.contains(&"find_files".to_string()));
        assert!(s.paths.contains(&"src/code.rs".to_string()));
        assert!(
            s.terms.contains(&"find_files".to_string()),
            "joined identifier term kept: {:?}",
            s.terms
        );
    }

    #[test]
    fn follow_up_intent_names_tests_and_callers() {
        let bench = "Now, for the same change, identify the tests that cover this code and the main \
                     call sites that invoke it. Be efficient: read only what you need.";
        assert_eq!(
            follow_up_intent(bench),
            FollowUpIntent {
                tests: true,
                callers: true
            }
        );
        assert_eq!(
            follow_up_intent("and where is it used?"),
            FollowUpIntent {
                tests: false,
                callers: true
            }
        );
        assert_eq!(
            follow_up_intent("add a test for the same thing"),
            FollowUpIntent {
                tests: true,
                callers: false
            }
        );
        assert_eq!(follow_up_intent("ok, do it"), FollowUpIntent::default());
        assert!(!follow_up_intent("ok, do it").any());
        // Words inside identifiers or other words do not count.
        assert!(!follow_up_intent("update the contest and the callsign").any());
        // Everyday uses of "uses", "cover", "spec" and "used" are not a request for callers or tests.
        assert!(!follow_up_intent("which of the above uses less memory?").any());
        assert!(!follow_up_intent("does that cover the timeout case?").any());
        assert!(!follow_up_intent("what does the spec say about retries?").any());
        assert!(!follow_up_intent("what algorithm is used there?").any());
        assert!(follow_up_intent("where is it used?").callers);
        assert!(follow_up_intent("is this used by the client?").callers);
    }

    #[test]
    fn follow_ups_are_recognised() {
        let bench_follow_up = "Now, for the same change, identify the tests that cover this code and the main call sites \
            that invoke it. Be efficient: read only what you need. End your answer with one line exactly of the form\n\
            FILES: <comma-separated repo-relative paths of the most relevant source files>";
        assert!(is_follow_up(bench_follow_up));
        assert!(is_follow_up(
            "what about the previous function's error handling and its retry loop timing budget"
        ));
        // New tasks that merely use "it"/"this" are not follow-ups.
        assert!(!is_follow_up(
            "fix it so that the wal replay handles torn writes in segment headers"
        ));
        assert!(!is_follow_up(
            "this crashes: the replica sync loop deadlocks when the primary restarts mid snapshot"
        ));
        // Naming code makes a prompt self-contained even with a back-reference.
        assert!(!is_follow_up(
            "same issue but in parse_config() inside src/config.rs"
        ));
        assert!(is_follow_up(
            "Now, for the same change, identify the tests that cover this code and the main call sites that invoke it."
        ));
        assert!(is_follow_up("where is it called?"));
        assert!(is_follow_up("add tests"));
        assert!(!is_follow_up(
            "fix(shard): gate unused graph-merge params under graph feature"
        ));
        assert!(!is_follow_up(
            "why does the wal replay skip torn segment headers after crash recovery"
        ));
        assert_eq!(
            content_terms("fix(shard): gate unused graph-merge params under graph feature"),
            8
        );
    }

    #[test]
    fn extracts_camel_case_identifier() {
        let s = extract_signals("update HashWithTtl to expire lazily");
        assert!(s.identifiers.contains(&"HashWithTtl".to_string()));
    }

    #[test]
    fn extracts_snake_case_identifier() {
        let s = extract_signals("fix parse_config panics on empty input");
        assert!(s.identifiers.contains(&"parse_config".to_string()));
    }

    #[test]
    fn extracts_call_site_identifier_regardless_of_case() {
        let s = extract_signals("why does retry() never back off?");
        assert!(s.identifiers.contains(&"retry".to_string()));
    }

    #[test]
    fn extracts_scoped_identifier_both_segments() {
        let s = extract_signals("Store::bm25 should OR terms, not AND them");
        assert!(s.identifiers.contains(&"Store".to_string()));
        assert!(s.identifiers.contains(&"bm25".to_string()));
    }

    #[test]
    fn plain_prose_words_are_not_identifiers() {
        let s = extract_signals("Foo bar baz should not explode");
        // "Foo" alone (no case mixing, no underscore, no parens) is ordinary prose.
        assert!(!s.identifiers.contains(&"Foo".to_string()));
    }

    #[test]
    fn extracts_path_with_directory() {
        let s = extract_signals("the bug is in src/rank/retriever.rs near the top");
        assert!(s.paths.contains(&"src/rank/retriever.rs".to_string()));
    }

    #[test]
    fn extracts_bare_filename_path() {
        let s = extract_signals("update x.py to use the new client");
        assert!(s.paths.contains(&"x.py".to_string()));
    }

    #[test]
    fn strips_trailing_sentence_punctuation_from_path() {
        let s = extract_signals("Look at parser.rs.");
        assert!(s.paths.contains(&"parser.rs".to_string()));
        assert!(!s.paths.iter().any(|p| p.ends_with('.')));
    }

    #[test]
    fn path_mention_is_not_also_counted_as_identifier() {
        let s = extract_signals("fix src/rank/retriever.rs");
        assert!(!s.identifiers.iter().any(|i| i.contains("rs")));
    }

    const WRAPPED: &str = "In this repository, find the source code that implements or would need \
        to change for the following change, and briefly explain how it works:\n\n\"Enforce the \
        mmap budget when sealing vector segments\"\n\nBe efficient: read only what you need. End \
        your answer with one line exactly of the form\nFILES: <comma-separated repo-relative \
        paths of the most relevant source files>";

    #[test]
    fn a_quoted_task_is_the_focus_of_a_wrapped_prompt() {
        let focus = task_focus(WRAPPED);
        assert_eq!(
            focus,
            "Enforce the mmap budget when sealing vector segments"
        );
        let terms = extract_signals(&focus).terms;
        for wrapper_word in [
            "source", "code", "change", "files", "paths", "form", "relative",
        ] {
            assert!(
                !terms.iter().any(|t| t == wrapper_word),
                "{wrapper_word} leaked"
            );
        }
    }

    #[test]
    fn curly_quotes_count_as_a_quoted_task() {
        let p = "Please look into this: \u{201c}retry the upload when the socket resets\u{201d}";
        assert_eq!(task_focus(p), "retry the upload when the socket resets");
    }

    #[test]
    fn short_quotes_do_not_replace_the_prompt() {
        let p = "rename \"foo\" to \"bar\" in the config loader";
        assert_eq!(task_focus(p), p);
    }

    #[test]
    fn output_format_sentences_are_dropped() {
        let p = "Fix the wal replay ordering after a crash. Be efficient: read only what you need. \
                 End your answer with one line of the form\nFILES: <paths>";
        assert_eq!(task_focus(p), "Fix the wal replay ordering after a crash.");
    }

    #[test]
    fn task_sentences_about_reading_or_files_are_kept() {
        let p = "Make the read path skip files that end with a slash. Keep the fast path as is.";
        assert_eq!(task_focus(p), p);
    }

    #[test]
    fn a_prompt_that_is_all_instructions_is_kept_whole() {
        let p = "Be concise. Reply with the answer only.";
        assert_eq!(task_focus(p), p);
    }
}
