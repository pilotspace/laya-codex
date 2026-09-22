//! Per-session state held by the daemon: last ranking, working set and Read counts.
//! Bounded (LRU by last touch) so a long-running daemon cannot grow without limit.

use std::collections::HashMap;
use std::time::Instant;

use laya_core::{QueryResult, RankedSpan};

use crate::protocol::SessionView;

const MAX_SESSIONS: usize = 256;
const MAX_WORKING_SET: usize = 40;

#[derive(Default)]
struct Session {
    last: Option<QueryResult>,
    working_set: Vec<RankedSpan>,
    reads: HashMap<String, u32>,
    touched: Option<Instant>,
}

#[derive(Default)]
pub struct Sessions {
    map: HashMap<String, Session>,
}

impl Sessions {
    fn get(&mut self, id: &str) -> &mut Session {
        if !self.map.contains_key(id) && self.map.len() >= MAX_SESSIONS
            && let Some(oldest) = self.map.iter().min_by_key(|(_, s)| s.touched).map(|(k, _)| k.clone())
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
            if let Some(existing) = s
                .working_set
                .iter_mut()
                .find(|w| w.path == span.path && w.start_line == span.start_line && w.end_line == span.end_line)
            {
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

    /// Increment and return the Read count for `path` in the session.
    pub fn note_read(&mut self, id: &str, path: &str) -> u32 {
        let c = self.get(id).reads.entry(path.to_string()).or_insert(0);
        *c += 1;
        *c
    }

    pub fn view(&mut self, id: &str) -> SessionView {
        let s = self.get(id);
        SessionView { last: s.last.clone(), working_set: s.working_set.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use laya_core::RankMode;

    fn span(path: &str, start: u32, score: f32) -> RankedSpan {
        RankedSpan { path: path.into(), start_line: start, end_line: start + 9, symbol: String::new(), p_relevant: None, score, text: String::new() }
    }

    fn result(spans: Vec<RankedSpan>) -> QueryResult {
        QueryResult { spans, mode: RankMode::Lexical, elapsed_ms: 1, candidates: 3 }
    }

    #[test]
    fn read_counts_are_per_session_and_path() {
        let mut s = Sessions::default();
        assert_eq!(s.note_read("a", "x.rs"), 1);
        assert_eq!(s.note_read("a", "x.rs"), 2);
        assert_eq!(s.note_read("a", "y.rs"), 1);
        assert_eq!(s.note_read("b", "x.rs"), 1);
    }

    #[test]
    fn working_set_dedupes_keeps_best_score_and_orders() {
        let mut s = Sessions::default();
        s.record_query("a", &result(vec![span("x.rs", 1, 0.2), span("y.rs", 5, 0.9)]));
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
            s.note_read(&format!("s{i}"), "x");
        }
        assert_eq!(s.map.len(), MAX_SESSIONS);
    }
}
