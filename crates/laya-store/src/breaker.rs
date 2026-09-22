//! Consecutive-failure circuit breaker (closed -> open -> half-open -> closed).
//!
//! Only *transport* failures (refused, reset, timeout) count. A server error reply proves the
//! server is alive and counts as success. While open, callers fail fast without touching the
//! network so the hook path never waits on a dead sidecar.

use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    Closed,
    Open,
    HalfOpen,
}

#[derive(Debug)]
enum Inner {
    Closed { failures: u32 },
    Open { until: Instant },
    /// One probe is in flight; `since` lets a lost probe (panicked caller) be replaced.
    HalfOpen { since: Instant },
}

#[derive(Debug)]
pub struct CircuitBreaker {
    threshold: u32,
    cooldown: Duration,
    inner: Mutex<Inner>,
}

impl CircuitBreaker {
    #[must_use]
    pub fn new(threshold: u32, cooldown: Duration) -> Self {
        Self { threshold: threshold.max(1), cooldown, inner: Mutex::new(Inner::Closed { failures: 0 }) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // The critical sections cannot leave `Inner` half-updated, so a poisoned lock is safe to reuse.
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// May a call proceed now? Transitions open -> half-open once the cooldown elapsed,
    /// admitting exactly one probe.
    #[must_use]
    pub fn try_acquire(&self) -> bool {
        self.try_acquire_at(Instant::now())
    }

    pub(crate) fn try_acquire_at(&self, now: Instant) -> bool {
        let mut g = self.lock();
        match *g {
            Inner::Closed { .. } => true,
            Inner::Open { until } if now >= until => {
                *g = Inner::HalfOpen { since: now };
                true
            }
            Inner::Open { .. } => false,
            Inner::HalfOpen { since } if now.duration_since(since) >= self.cooldown => {
                *g = Inner::HalfOpen { since: now };
                true
            }
            Inner::HalfOpen { .. } => false,
        }
    }

    pub fn on_success(&self) {
        *self.lock() = Inner::Closed { failures: 0 };
    }

    pub fn on_failure(&self) {
        self.on_failure_at(Instant::now());
    }

    pub(crate) fn on_failure_at(&self, now: Instant) {
        let mut g = self.lock();
        *g = match *g {
            Inner::Closed { failures } if failures + 1 < self.threshold => Inner::Closed { failures: failures + 1 },
            _ => Inner::Open { until: now + self.cooldown },
        };
    }

    #[must_use]
    pub fn state(&self) -> BreakerState {
        match *self.lock() {
            Inner::Closed { .. } => BreakerState::Closed,
            Inner::Open { .. } => BreakerState::Open,
            Inner::HalfOpen { .. } => BreakerState::HalfOpen,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CD: Duration = Duration::from_secs(10);

    #[test]
    fn opens_after_threshold_consecutive_failures() {
        let b = CircuitBreaker::new(5, CD);
        let t = Instant::now();
        for _ in 0..4 {
            b.on_failure_at(t);
        }
        assert_eq!(b.state(), BreakerState::Closed);
        assert!(b.try_acquire_at(t));
        b.on_failure_at(t);
        assert_eq!(b.state(), BreakerState::Open);
        assert!(!b.try_acquire_at(t + Duration::from_secs(9)));
    }

    #[test]
    fn success_resets_failure_count() {
        let b = CircuitBreaker::new(3, CD);
        let t = Instant::now();
        b.on_failure_at(t);
        b.on_failure_at(t);
        b.on_success();
        b.on_failure_at(t);
        b.on_failure_at(t);
        assert_eq!(b.state(), BreakerState::Closed);
    }

    #[test]
    fn half_open_admits_one_probe_then_closes_on_success() {
        let b = CircuitBreaker::new(1, CD);
        let t = Instant::now();
        b.on_failure_at(t);
        let later = t + CD;
        assert!(b.try_acquire_at(later));
        assert_eq!(b.state(), BreakerState::HalfOpen);
        assert!(!b.try_acquire_at(later), "second caller must fail fast while probing");
        b.on_success();
        assert_eq!(b.state(), BreakerState::Closed);
        assert!(b.try_acquire_at(later));
    }

    #[test]
    fn failed_probe_reopens() {
        let b = CircuitBreaker::new(1, CD);
        let t = Instant::now();
        b.on_failure_at(t);
        assert!(b.try_acquire_at(t + CD));
        b.on_failure_at(t + CD);
        assert_eq!(b.state(), BreakerState::Open);
        assert!(!b.try_acquire_at(t + CD + Duration::from_secs(1)));
    }

    #[test]
    fn lost_probe_is_replaced_after_cooldown() {
        let b = CircuitBreaker::new(1, CD);
        let t = Instant::now();
        b.on_failure_at(t);
        assert!(b.try_acquire_at(t + CD));
        assert!(b.try_acquire_at(t + CD + CD));
    }
}
