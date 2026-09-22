//! Code-aware term normalization shared by indexing and querying, so both sides agree.

/// Split text into lowercase search terms: identifiers are split on camelCase, snake_case and
/// digits boundaries; the joined form of multi-part identifiers is kept too
/// (`HashWithTtl` -> `hash`, `with`, `ttl`, `hashwithttl`). Terms shorter than 2 chars are dropped.
pub fn terms(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for word in words(text) {
        let parts = split_ident(word);
        let multi = parts.len() > 1;
        for p in &parts {
            if p.len() > 1 {
                out.push(p.clone());
            }
        }
        if multi {
            out.push(word.to_ascii_lowercase());
        }
    }
    out
}

/// Identifier-like tokens in `text` (`[A-Za-z_][A-Za-z0-9_]*`), original case.
pub fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty() && !w.as_bytes()[0].is_ascii_digit())
}

fn split_ident(word: &str) -> Vec<String> {
    let mut parts = Vec::new();
    for seg in word.split('_').filter(|s| !s.is_empty()) {
        let chars: Vec<char> = seg.chars().collect();
        let mut cur = String::new();
        for (i, &c) in chars.iter().enumerate() {
            let boundary = i > 0
                && ((c.is_ascii_uppercase()
                    && (chars[i - 1].is_ascii_lowercase()
                        || chars[i - 1].is_ascii_digit()
                        || (i + 1 < chars.len() && chars[i + 1].is_ascii_lowercase() && chars[i - 1].is_ascii_uppercase())))
                    || (c.is_ascii_digit() != chars[i - 1].is_ascii_digit()));
            if boundary && !cur.is_empty() {
                parts.push(std::mem::take(&mut cur).to_ascii_lowercase());
            }
            cur.push(c);
        }
        if !cur.is_empty() {
            parts.push(cur.to_ascii_lowercase());
        }
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_camel_snake_and_keeps_joined() {
        assert_eq!(terms("HashWithTtl"), vec!["hash", "with", "ttl", "hashwithttl"]);
        assert_eq!(terms("parse_config"), vec!["parse", "config", "parse_config"]);
        assert_eq!(terms("HTTPServer"), vec!["http", "server", "httpserver"]);
    }

    #[test]
    fn drops_single_chars_and_numbers() {
        assert_eq!(terms("a + 42 b"), Vec::<String>::new());
        assert_eq!(terms("utf8"), vec!["utf", "utf8"]);
    }
}
