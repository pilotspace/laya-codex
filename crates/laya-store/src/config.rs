//! Client configuration. Defaults are tuned for a loopback sidecar on the hook path.

use std::time::Duration;

use crate::secure::Password;

#[derive(Debug, Clone)]
pub struct StoreConfig {
    pub host: String,
    pub port: u16,
    /// TCP connect budget. Loopback connects take microseconds; refusals are instant.
    pub connect_timeout: Duration,
    /// Read/write timeout for query-path ops (bm25, get_chunks, memo, ...).
    pub query_timeout: Duration,
    /// Read/write timeout for bulk writes (put_file, delete_file).
    pub bulk_timeout: Duration,
    /// Retries after the first attempt for transport errors (refused, reset, broken pipe).
    /// Timeouts are retried only for bulk ops: a query that timed out once would blow the
    /// hook budget again.
    pub max_retries: u32,
    /// Base of the exponential backoff; each wait is `base * 2^attempt` scaled by a random
    /// factor in `[0.5, 1.5)`.
    pub backoff_base: Duration,
    /// Consecutive failed operations that open the breaker.
    pub breaker_threshold: u32,
    /// How long the breaker stays open before a half-open probe.
    pub breaker_cooldown: Duration,
    /// Connections kept to Moon; lets a long bulk write run beside hook queries.
    pub pool_size: usize,
    /// Max distinct terms per bm25 call (one FT.SEARCH each).
    pub max_terms: usize,
    /// Hits fetched per term before summation (at least the caller's `limit`). Larger is more
    /// exact for docs matching many common terms; smaller is faster.
    pub per_term_limit: usize,
    /// Query cost budget in `Σ df²` over the searched terms. Moon scores a term in O(df²), so
    /// this bounds bm25 latency: ~1.9 ns per unit on an M4 Pro, i.e. the default 2e7 ≈ 40 ms
    /// server time. Rarest terms are chosen first; frequent terms beyond the budget are skipped.
    pub df_sq_budget: u64,
    /// Max occurrences of one term indexed per chunk (0 = unlimited). Moon re-scans a term's
    /// whole posting list for every *repeated* occurrence in a document, so indexing cost grows
    /// with tf x df; capping tf trades a little BM25 tf signal for much faster indexing.
    pub max_tf: u32,
    /// Sent with `AUTH` on every new connection (including reconnects) when set.
    pub password: Option<Password>,
}

impl StoreConfig {
    /// Defaults for a Moon on `127.0.0.1:port`.
    #[must_use]
    pub fn local(port: u16) -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port,
            connect_timeout: Duration::from_millis(200),
            query_timeout: Duration::from_millis(250),
            bulk_timeout: Duration::from_secs(5),
            max_retries: 2,
            backoff_base: Duration::from_millis(10),
            breaker_threshold: 5,
            breaker_cooldown: Duration::from_secs(10),
            pool_size: 4,
            max_terms: 24,
            per_term_limit: 200,
            df_sq_budget: 20_000_000,
            max_tf: 2,
            password: None,
        }
    }

    /// The same configuration authenticating with `password`.
    #[must_use]
    pub fn with_password(mut self, password: Password) -> Self {
        self.password = Some(password);
        self
    }

    pub(crate) fn url(&self) -> String {
        format!("redis://{}:{}/", self.host, self.port)
    }
}
