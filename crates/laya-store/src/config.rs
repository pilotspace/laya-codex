//! Client configuration. Defaults are tuned for a loopback sidecar on the hook path.

use std::time::Duration;

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
        }
    }

    pub(crate) fn url(&self) -> String {
        format!("redis://{}:{}/", self.host, self.port)
    }
}
