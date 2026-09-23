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
    "a", "about", "above", "after", "again", "all", "also", "am", "an", "and", "any", "are", "as", "at",
    "be", "because", "been", "before", "being", "below", "between", "both", "but", "by", "can", "could",
    "did", "do", "does", "doing", "done", "down", "during", "each", "either", "else", "etc", "ever",
    "every", "few", "for", "from", "further", "had", "has", "have", "having", "he", "her", "here", "his",
    "how", "however", "i", "if", "in", "into", "is", "it", "its", "itself", "just", "let", "lets", "me",
    "might", "more", "most", "much", "must", "my", "no", "nor", "not", "now", "of", "off", "on", "once",
    "one", "only", "or", "other", "our", "out", "over", "own", "please", "same", "shall", "she", "should",
    "so", "some", "such", "than", "that", "the", "their", "them", "then", "there", "these", "they",
    "this", "those", "through", "thus", "to", "too", "under", "until", "up", "upon", "us", "very", "via",
    "was", "we", "were", "what", "when", "where", "whether", "which", "while", "who", "whom", "whose",
    "why", "will", "with", "within", "without", "would", "yes", "yet", "you", "your", "yours",
    // instruction / meta vocabulary of agent prompts. Words that are also common code-domain
    // vocabulary (path, file, line, read, list, code, source, change, ...) are deliberately NOT
    // here: "fast-path" or "read path" are task content, and common words rarely win the
    // rarest-first BM25 term selection anyway.
    "answer", "briefly", "codebase", "comma", "describe", "efficient", "efficiently", "exactly",
    "explain", "find", "following", "give", "help", "identify", "implements", "look", "need", "needs",
    "please", "project", "relevant", "repository", "separated", "show", "tell", "understand", "want",
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

/// Words that refer back to earlier conversation ("the same change", "where is it called").
const CONTINUATION_MARKERS: &[&str] = &[
    "same", "this", "that", "these", "those", "it", "its", "them", "above", "previous", "earlier",
    "also", "again", "now", "there",
];

/// Whether `prompt` reads as a follow-up that depends on earlier context: almost no task
/// content, or little content plus a word pointing back ("now find the tests for the same change").
pub fn is_follow_up(prompt: &str) -> bool {
    let content = content_terms(prompt);
    let refers_back = ident::words(prompt).any(|w| CONTINUATION_MARKERS.contains(&w.to_ascii_lowercase().as_str()));
    content < 4 || (content < 10 && refers_back)
}

/// Extract [`PromptSignals`] from a raw user prompt.
pub fn extract_signals(prompt: &str) -> PromptSignals {
    let terms: Vec<String> = ident::terms(prompt).into_iter().filter(|t| !is_stopword(t)).collect();
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
        for kept in ["vector", "mmap", "budget", "post", "review", "issues", "address"] {
            assert!(s.terms.contains(&kept.to_string()), "{kept} missing from {:?}", s.terms);
        }
        for dropped in ["repository", "find", "explain", "efficient", "comma", "separated", "answer", "relevant", "briefly", "exactly"] {
            assert!(!s.terms.contains(&dropped.to_string()), "{dropped} kept in {:?}", s.terms);
        }
    }

    #[test]
    fn other_phrasings_of_instructions_are_dropped_too() {
        let s = extract_signals("Can you please show me where in the codebase we should look to understand how the WAL replay handles torn writes?");
        assert_eq!(content_terms("Can you please show me where in the codebase we should look"), 0);
        for kept in ["wal", "replay", "handles", "torn", "writes"] {
            assert!(s.terms.contains(&kept.to_string()), "{kept} missing from {:?}", s.terms);
        }
    }

    #[test]
    fn code_domain_words_are_kept() {
        let s = extract_signals("drop dead cross-shard fast-path metrics; fix the read path and file lines");
        for kept in ["path", "fast", "read", "file", "lines"] {
            assert!(s.terms.contains(&kept.to_string()), "{kept} missing from {:?}", s.terms);
        }
    }

    #[test]
    fn identifiers_and_paths_survive_the_stoplist() {
        let s = extract_signals("why does find_files() in src/code.rs skip Source::File?");
        assert!(s.identifiers.contains(&"find_files".to_string()));
        assert!(s.paths.contains(&"src/code.rs".to_string()));
        assert!(s.terms.contains(&"find_files".to_string()), "joined identifier term kept: {:?}", s.terms);
    }

    #[test]
    fn follow_ups_are_recognised() {
        assert!(is_follow_up("Now, for the same change, identify the tests that cover this code and the main call sites that invoke it."));
        assert!(is_follow_up("where is it called?"));
        assert!(is_follow_up("add tests"));
        assert!(!is_follow_up("fix(shard): gate unused graph-merge params under graph feature"));
        assert!(!is_follow_up("why does the wal replay skip torn segment headers after crash recovery"));
        assert_eq!(content_terms("fix(shard): gate unused graph-merge params under graph feature"), 8);
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
}
