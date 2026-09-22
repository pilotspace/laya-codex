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

/// Extract [`PromptSignals`] from a raw user prompt.
pub fn extract_signals(prompt: &str) -> PromptSignals {
    let terms = ident::terms(prompt);
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
