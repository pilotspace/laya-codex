//! Per-session state held by the daemon: last ranking, working set and Read counts.
//! Bounded (LRU by last touch) so a long-running daemon cannot grow without limit.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use laya_core::{QueryResult, RankedSpan};

use crate::protocol::SessionView;

const MAX_SESSIONS: usize = 256;
const MAX_WORKING_SET: usize = 40;
const MAX_SENT: usize = 200;

#[derive(Default)]
struct Session {
    last: Option<QueryResult>,
    working_set: Vec<RankedSpan>,
    reads: HashMap<String, u32>,
    /// Spans whose full code was already injected this session: (path, start, end).
    sent: Vec<(String, u32, u32)>,
    /// Files read without offset/limit: entirely in the agent's context.
    full_reads: HashSet<String>,
    /// Last self-contained prompt: what thin follow-ups in this session are about.
    topic: Option<String>,
    touched: Option<Instant>,
}

#[derive(Default)]
pub struct Sessions {
    map: HashMap<String, Session>,
}

impl Sessions {
    fn get(&mut self, id: &str) -> &mut Session {
        if !self.map.contains_key(id)
            && self.map.len() >= MAX_SESSIONS
            && let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, s)| s.touched)
                .map(|(k, _)| k.clone())
        {
            self.map.remove(&oldest);
        }
        let s = self.map.entry(id.to_string()).or_default();
        s.touched = Some(Instant::now());
        s
    }

    /// Record a query result: it becomes `last` and its spans merge into the working set.
    pub fn record_query(&mut self, id: &str, result: &QueryResult) {
        let s = self.get(id);
        s.last = Some(result.clone());
        for span in &result.spans {
            if let Some(existing) = s.working_set.iter_mut().find(|w| {
                w.path == span.path
                    && w.start_line == span.start_line
                    && w.end_line == span.end_line
            }) {
                if span.score > existing.score {
                    *existing = span.clone();
                }
            } else {
                s.working_set.push(span.clone());
            }
        }
        s.working_set.sort_by(|a, b| b.score.total_cmp(&a.score));
        s.working_set.truncate(MAX_WORKING_SET);
    }

    /// Increment and return the Read count for `path` in the session; `full` marks a whole-file read.
    pub fn note_read(&mut self, id: &str, path: &str, full: bool) -> u32 {
        let s = self.get(id);
        if full {
            s.full_reads.insert(path.to_string());
        }
        let c = s.reads.entry(path.to_string()).or_insert(0);
        *c += 1;
        *c
    }

    /// Record spans whose full code was injected (deduplicated, bounded to the newest `MAX_SENT`).
    pub fn mark_sent(&mut self, id: &str, keys: &[(String, u32, u32)]) {
        let s = self.get(id);
        for k in keys {
            if !s.sent.contains(k) {
                s.sent.push(k.clone());
            }
        }
        let overflow = s.sent.len().saturating_sub(MAX_SENT);
        s.sent.drain(..overflow);
    }

    /// Whether the session has a topic, i.e. a later prompt can be a follow-up of it.
    pub fn has_topic(&self, id: &str) -> bool {
        self.map.get(id).is_some_and(|s| s.topic.is_some())
    }

    /// Text to retrieve for `prompt`: the prompt itself if it is self-contained (or the first in
    /// the session, which then becomes the topic). A follow-up ("now find the tests for it") is
    /// retrieved as the topic plus only the identifiers and paths it names: its prose ("tests",
    /// "call sites", "main") is conversational and pulls in unrelated code, while the session
    /// delta and reference expansion already surface the topic's next spans and callers.
    pub fn effective_query(&mut self, id: &str, prompt: &str) -> String {
        let s = self.get(id);
        match &s.topic {
            Some(topic) if laya_rank::is_follow_up(prompt) => {
                let sig = laya_rank::extract_signals(prompt);
                let named: Vec<String> = sig.identifiers.into_iter().chain(sig.paths).collect();
                if named.is_empty() {
                    topic.clone()
                } else {
                    format!("{topic}\n{}", named.join(" "))
                }
            }
            _ => {
                s.topic = Some(prompt.to_string());
                prompt.to_string()
            }
        }
    }

    /// Everything already in the agent's context: sent spans plus whole files it read.
    pub fn already(&mut self, id: &str) -> Vec<(String, u32, u32)> {
        let s = self.get(id);
        let mut out = s.sent.clone();
        out.extend(s.full_reads.iter().map(|p| (p.clone(), 1, u32::MAX)));
        out
    }

    /// The agent's context was compacted or cleared: nothing sent or read earlier is still in it.
    pub fn reset_context(&mut self, id: &str) {
        let s = self.get(id);
        s.sent.clear();
        s.full_reads.clear();
    }

    pub fn view(&mut self, id: &str) -> SessionView {
        let s = self.get(id);
        SessionView {
            last: s.last.clone(),
            working_set: s.working_set.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use laya_core::RankMode;

    fn span(path: &str, start: u32, score: f32) -> RankedSpan {
        RankedSpan {
            path: path.into(),
            start_line: start,
            end_line: start + 9,
            symbol: String::new(),
            p_relevant: None,
            score,
            text: String::new(),
        }
    }

    fn result(spans: Vec<RankedSpan>) -> QueryResult {
        QueryResult {
            spans,
            mode: RankMode::Lexical,
            elapsed_ms: 1,
            candidates: 3,
            scored: 0,
            offered: 0,
            related: vec![],
        }
    }

    #[test]
    fn read_counts_are_per_session_and_path() {
        let mut s = Sessions::default();
        assert_eq!(s.note_read("a", "x.rs", false), 1);
        assert_eq!(s.note_read("a", "x.rs", false), 2);
        assert_eq!(s.note_read("a", "y.rs", false), 1);
        assert_eq!(s.note_read("b", "x.rs", false), 1);
    }

    #[test]
    fn already_combines_sent_spans_and_full_reads_per_session() {
        let mut s = Sessions::default();
        s.mark_sent("a", &[("x.rs".into(), 1, 20), ("x.rs".into(), 1, 20)]);
        s.note_read("a", "y.rs", true);
        s.note_read("a", "z.rs", false);
        let mut got = s.already("a");
        got.sort();
        assert_eq!(
            got,
            vec![
                ("x.rs".to_string(), 1, 20),
                ("y.rs".to_string(), 1, u32::MAX)
            ]
        );
        assert!(s.already("b").is_empty());
    }

    #[test]
    fn sent_is_bounded_to_newest() {
        let mut s = Sessions::default();
        let keys: Vec<_> = (0..(MAX_SENT as u32 + 5))
            .map(|i| ("f.rs".to_string(), i, i))
            .collect();
        s.mark_sent("a", &keys);
        let got = s.already("a");
        assert_eq!(got.len(), MAX_SENT);
        assert_eq!(got[0].1, 5);
    }

    #[test]
    fn thin_follow_ups_are_queried_with_the_session_topic() {
        let mut s = Sessions::default();
        let task = "fix(vector): address three post-review issues in mmap budget accounting";
        assert_eq!(s.effective_query("a", task), task);
        let follow = "Now, for the same change, identify the tests that cover this code.";
        assert_eq!(
            s.effective_query("a", follow),
            task,
            "a follow-up's prose is not searched"
        );
        // A terse follow-up naming code keeps the topic and adds the name…
        let q = s.effective_query("a", "and enforce_budget()?");
        assert!(q.starts_with(task) && q.contains("enforce_budget"), "{q}");
        // …while a question that names code with real content is self-contained.
        let named = "now where is enforce_budget() called from src/vector/store.rs?";
        assert_eq!(s.effective_query("a", named), named);
        // A new self-contained task replaces the topic.
        let other = "gate unused graph merge params under graph feature in shard autovacuum";
        assert_eq!(s.effective_query("a", other), other);
        assert_eq!(s.effective_query("a", follow), other);
        // Other sessions are unaffected.
        assert_eq!(s.effective_query("b", follow), follow);
    }

    #[test]
    fn working_set_dedupes_keeps_best_score_and_orders() {
        let mut s = Sessions::default();
        s.record_query(
            "a",
            &result(vec![span("x.rs", 1, 0.2), span("y.rs", 5, 0.9)]),
        );
        s.record_query("a", &result(vec![span("x.rs", 1, 0.7)]));
        let v = s.view("a");
        assert_eq!(v.working_set.len(), 2);
        assert_eq!(v.working_set[0].path, "y.rs");
        assert!((v.working_set[1].score - 0.7).abs() < 1e-6);
        assert_eq!(v.last.unwrap().spans.len(), 1);
    }

    #[test]
    fn session_count_is_bounded() {
        let mut s = Sessions::default();
        for i in 0..(MAX_SESSIONS + 10) {
            s.note_read(&format!("s{i}"), "x", false);
        }
        assert_eq!(s.map.len(), MAX_SESSIONS);
    }
}
