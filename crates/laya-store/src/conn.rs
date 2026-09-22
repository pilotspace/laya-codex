//! Connection pool + resilient executor: timeouts, jittered retries, reconnects and the breaker.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::Duration;

use laya_core::Error;
use redis::{Connection, ErrorKind, RedisError, RedisResult};

use crate::breaker::{BreakerState, CircuitBreaker};
use crate::config::StoreConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpKind {
    Query,
    Bulk,
}

struct Pooled {
    generation: u64,
    con: Connection,
}

pub(crate) struct Executor {
    cfg: StoreConfig,
    client: redis::Client,
    slots: Box<[Mutex<Option<Pooled>>]>,
    next: AtomicUsize,
    /// Bumped on every transport failure. Pooled connections from an older generation were
    /// opened before the failure (e.g. to a Moon that has since restarted) and are discarded
    /// instead of each burning a retry.
    generation: AtomicU64,
    breaker: CircuitBreaker,
}

/// Transport-level failure: the connection is unusable and the server may be down.
fn is_transient(e: &RedisError) -> bool {
    e.is_io_error()
        || e.is_timeout()
        || e.is_connection_dropped()
        || e.is_connection_refusal()
        || e.is_unrecoverable_error()
        // A parse error means the stream is desynchronised; the connection must be replaced.
        || e.kind() == ErrorKind::Parse
}

impl Executor {
    pub(crate) fn new(cfg: StoreConfig) -> laya_core::Result<Self> {
        let bad = |e: RedisError| Error::Store(format!("invalid moon address: {e}"));
        let info = redis::IntoConnectionInfo::into_connection_info(cfg.url()).map_err(bad)?;
        // Skip the `CLIENT SETINFO` handshake: it costs a round trip per (re)connect and is
        // pure telemetry. With it gone, a connect is just the TCP handshake.
        let redis_settings = info.redis_settings().clone().set_skip_set_lib_name();
        let client = redis::Client::open(info.set_redis_settings(redis_settings)).map_err(bad)?;
        let slots = (0..cfg.pool_size.max(1))
            .map(|_| Mutex::new(None))
            .collect();
        let breaker = CircuitBreaker::new(cfg.breaker_threshold, cfg.breaker_cooldown);
        Ok(Self {
            cfg,
            client,
            slots,
            next: AtomicUsize::new(0),
            generation: AtomicU64::new(0),
            breaker,
        })
    }

    pub(crate) fn config(&self) -> &StoreConfig {
        &self.cfg
    }

    pub(crate) fn breaker_state(&self) -> BreakerState {
        self.breaker.state()
    }

    /// Take a free pooled slot, or wait on one if all are busy.
    fn slot(&self) -> MutexGuard<'_, Option<Pooled>> {
        let n = self.slots.len();
        let start = self.next.fetch_add(1, Ordering::Relaxed) % n;
        for i in 0..n {
            match self.slots[(start + i) % n].try_lock() {
                Ok(g) => return g,
                // A panic mid-op may have left the connection with unread replies: drop it.
                Err(TryLockError::Poisoned(p)) => {
                    let mut g = p.into_inner();
                    *g = None;
                    return g;
                }
                Err(TryLockError::WouldBlock) => {}
            }
        }
        self.slots[start]
            .lock()
            .unwrap_or_else(|p: PoisonError<_>| {
                let mut g = p.into_inner();
                *g = None;
                g
            })
    }

    fn attempt<T>(
        &self,
        timeout: Duration,
        f: &mut impl FnMut(&mut Connection) -> RedisResult<T>,
    ) -> RedisResult<T> {
        let mut guard = self.slot();
        let generation = self.generation.load(Ordering::Acquire);
        if guard.as_ref().is_some_and(|p| p.generation < generation) {
            *guard = None;
        }
        let con = match guard.as_mut() {
            Some(p) => &mut p.con,
            None => {
                let con = self
                    .client
                    .get_connection_with_timeout(self.cfg.connect_timeout)?;
                &mut guard.insert(Pooled { generation, con }).con
            }
        };
        let r = con
            .set_read_timeout(Some(timeout))
            .and_then(|()| con.set_write_timeout(Some(timeout)))
            .and_then(|()| f(con));
        if let Err(e) = &r
            && is_transient(e)
        {
            *guard = None; // reconnect on next use
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
        r
    }

    fn backoff(&self, attempt: u32) -> Duration {
        let base = self
            .cfg
            .backoff_base
            .saturating_mul(1u32 << attempt.min(10));
        base.mul_f64(0.5 + fastrand::f64())
    }

    /// Run one logical operation. Transport failures are retried (with reconnect) and feed the
    /// breaker once per operation; server error replies map to `Error::Store` and prove the
    /// server is alive.
    pub(crate) fn run<T>(
        &self,
        kind: OpKind,
        mut f: impl FnMut(&mut Connection) -> RedisResult<T>,
    ) -> laya_core::Result<T> {
        if !self.breaker.try_acquire() {
            return Err(Error::StoreUnavailable(
                "circuit open (moon unreachable)".into(),
            ));
        }
        let timeout = match kind {
            OpKind::Query => self.cfg.query_timeout,
            OpKind::Bulk => self.cfg.bulk_timeout,
        };
        let mut attempt = 0u32;
        loop {
            match self.attempt(timeout, &mut f) {
                Ok(v) => {
                    self.breaker.on_success();
                    return Ok(v);
                }
                Err(e) if is_transient(&e) => {
                    let retry =
                        attempt < self.cfg.max_retries && (kind == OpKind::Bulk || !e.is_timeout());
                    if !retry {
                        self.breaker.on_failure();
                        tracing::warn!(error = %e, attempts = attempt + 1, ?kind, "moon op failed");
                        return Err(Error::StoreUnavailable(e.to_string()));
                    }
                    std::thread::sleep(self.backoff(attempt));
                    attempt += 1;
                }
                Err(e) => {
                    self.breaker.on_success();
                    return Err(Error::Store(e.to_string()));
                }
            }
        }
    }
}
