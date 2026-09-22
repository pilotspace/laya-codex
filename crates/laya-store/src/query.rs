//! Pure query-side logic: term preparation, FT.SEARCH reply parsing and OR-BM25 fusion.
//!
//! Moon's `FT.SEARCH` is AND-only. BM25 is a sum of per-term contributions, so OR semantics
//! are emulated by running one single-term search per term and summing per document. Verified
//! against Moon: for a doc containing `parse` and `beta`, the AND query scored 4.928537 and the
//! per-term scores were 2.464268 + 2.464268 (see `MOON_NOTES.md` and the `bm25_ranks_docs_matching_more_terms_first_and_sums_like_and`
//! integration test).

use redis::Value;
use std::collections::HashMap;

/// Longest term we send; longer tokens are almost certainly noise (hashes, base64).
pub const MAX_TERM_LEN: usize = 64;

/// Normalize, sanitize, dedupe and cap query terms.
///
/// - lowercases; drops terms with chars outside `[a-z0-9_]` (they would be parsed as Moon
///   query syntax), shorter than 2 or longer than [`MAX_TERM_LEN`];
/// - dedupes keeping first occurrence;
/// - if more than `cap` remain, keeps the longest ones (length is a cheap proxy for rarity:
///   long identifiers are specific, short ones like `id`/`fn` are frequent). Ties keep input order.
#[must_use]
pub fn prepare_terms(terms: &[String], cap: usize) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for t in terms {
        let t = t.to_ascii_lowercase();
        let ok = (2..=MAX_TERM_LEN).contains(&t.len())
            && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if ok && seen.insert(t.clone()) {
            out.push(t);
        }
    }
    if out.len() > cap {
        // Stable sort: equal lengths keep caller order.
        let mut ranked: Vec<(usize, String)> = out.into_iter().enumerate().collect();
        ranked.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
        ranked.truncate(cap);
        ranked.sort_by_key(|(i, _)| *i);
        out = ranked.into_iter().map(|(_, t)| t).collect();
    }
    out
}

/// Parse a RESP2 `FT.SEARCH` reply into `(key, score)` pairs.
///
/// Moon replies `[count, key, [field, value, ...], key, [...], ...]` and always includes
/// `__bm25_score` in the field list, even with `NOCONTENT`. Keys without a score get 0.
pub fn parse_search_reply(v: &Value) -> Result<Vec<(String, f32)>, String> {
    let Value::Array(items) = v else {
        return Err(format!("unexpected FT.SEARCH reply: {v:?}"));
    };
    let mut out = Vec::new();
    let mut it = items.iter().skip(1).peekable();
    while let Some(item) = it.next() {
        let key = value_str(item)
            .ok_or_else(|| format!("unexpected key in FT.SEARCH reply: {item:?}"))?;
        let mut score = 0.0f32;
        if let Some(Value::Array(fields)) = it.peek() {
            for pair in fields.chunks(2) {
                if let [k, val] = pair
                    && value_str(k).as_deref() == Some("__bm25_score")
                {
                    score = value_str(val).and_then(|s| s.parse().ok()).unwrap_or(0.0);
                }
            }
            it.next();
        }
        out.push((key, score));
    }
    Ok(out)
}

/// Sum per-term hit lists into an OR ranking: best score first, ties by id, top `limit`.
#[must_use]
pub fn fuse_scores(per_term: &[Vec<(String, f32)>], limit: usize) -> Vec<(String, f32)> {
    let mut acc: HashMap<&str, f32> = HashMap::new();
    for hits in per_term {
        for (id, s) in hits {
            *acc.entry(id.as_str()).or_insert(0.0) += *s;
        }
    }
    let mut out: Vec<(String, f32)> = acc.into_iter().map(|(k, s)| (k.to_string(), s)).collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out.truncate(limit);
    out
}

/// Server errors that mean "this term contributes nothing" rather than a failure.
#[must_use]
pub fn is_benign_term_error(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("empty query after analysis") || m.contains("no such index")
}

pub(crate) fn value_str(v: &Value) -> Option<String> {
    match v {
        Value::BulkString(b) => Some(String::from_utf8_lossy(b).into_owned()),
        Value::SimpleString(s) => Some(s.clone()),
        Value::VerbatimString { text, .. } => Some(text.clone()),
        Value::Int(i) => Some(i.to_string()),
        Value::Double(d) => Some(d.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn bulk(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }

    #[test]
    fn prepare_dedupes_lowercases_and_sanitizes() {
        let got = prepare_terms(
            &s(&["Parse", "parse", "a", "@x", "foo-bar", "ok_1", ""]),
            24,
        );
        assert_eq!(got, s(&["parse", "ok_1"]));
    }

    #[test]
    fn prepare_caps_keeping_longest_in_input_order() {
        let got = prepare_terms(&s(&["id", "configuration", "fn", "loader", "xy"]), 3);
        // Keeps the 3 longest ({configuration, loader} + first 2-char tie `id`), in input order.
        assert_eq!(got, s(&["id", "configuration", "loader"]));
    }

    #[test]
    fn prepare_drops_overlong_terms() {
        let long = "a".repeat(MAX_TERM_LEN + 1);
        assert!(prepare_terms(&[long], 24).is_empty());
    }

    #[test]
    fn parses_moon_reply_with_scores() {
        let v = Value::Array(vec![
            Value::Int(2),
            bulk("lc:r:c:a"),
            Value::Array(vec![bulk("__bm25_score"), bulk("2.5")]),
            bulk("lc:r:c:b"),
            Value::Array(vec![
                bulk("path"),
                bulk("x"),
                bulk("__bm25_score"),
                bulk("1.25"),
            ]),
        ]);
        let got = parse_search_reply(&v).expect("parse");
        assert_eq!(
            got,
            vec![("lc:r:c:a".into(), 2.5), ("lc:r:c:b".into(), 1.25)]
        );
    }

    #[test]
    fn parses_reply_without_field_arrays() {
        let v = Value::Array(vec![Value::Int(1), bulk("k1")]);
        assert_eq!(
            parse_search_reply(&v).expect("parse"),
            vec![("k1".into(), 0.0)]
        );
        assert_eq!(
            parse_search_reply(&Value::Array(vec![Value::Int(0)])).expect("parse"),
            vec![]
        );
        assert!(parse_search_reply(&Value::Nil).is_err());
    }

    #[test]
    fn fuse_sums_scores_or_semantics() {
        let a = vec![("x".to_string(), 2.0), ("y".to_string(), 1.0)];
        let b = vec![("y".to_string(), 3.0), ("z".to_string(), 0.5)];
        let got = fuse_scores(&[a, b], 2);
        assert_eq!(got, vec![("y".into(), 4.0), ("x".into(), 2.0)]);
    }

    #[test]
    fn fuse_ties_break_by_id() {
        let a = vec![("b".to_string(), 1.0), ("a".to_string(), 1.0)];
        assert_eq!(
            fuse_scores(&[a], 10),
            vec![("a".into(), 1.0), ("b".into(), 1.0)]
        );
    }

    #[test]
    fn benign_errors_recognized() {
        assert!(is_benign_term_error("ERR empty query after analysis"));
        assert!(is_benign_term_error("ERR no such index"));
        assert!(!is_benign_term_error("ERR syntax error"));
    }
}
