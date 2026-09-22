//! `laya-store`: the [`laya_core::Store`] implementation backed by a Moon sidecar
//! (pilotspace/moon, Redis-compatible RESP server with BM25 `FT.SEARCH`).
//!
//! - [`MoonStore`] — resilient client: connect/op timeouts, jittered retries, circuit breaker,
//!   small connection pool, OR-BM25 emulated by pipelining per-term searches.
//! - [`MoonSupervisor`] — health-check / spawn / stop the local `moon` process.
//!
//! See `MOON_NOTES.md` for Moon behaviours this crate works around.

pub mod breaker;
mod config;
mod conn;
pub mod keys;
pub mod query;
mod store;

pub use breaker::{BreakerState, CircuitBreaker};
pub use config::StoreConfig;
pub use keys::repo_id;
pub use store::MoonStore;
