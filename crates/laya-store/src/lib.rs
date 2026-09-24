//! `laya-store`: the [`laya_core::Store`] implementation backed by a Moon sidecar
//! (pilotspace/moon, Redis-compatible RESP server with BM25 `FT.SEARCH`).
//!
//! - [`MoonStore`] — resilient client: connect/op timeouts, jittered retries, circuit breaker,
//!   small connection pool, OR-BM25 emulated by pipelining per-term searches.
//! - [`MoonSupervisor`] — health-check / spawn / stop the local `moon` process, password
//!   protected (see [`secure`]).
//!
//! See `MOON_NOTES.md` for Moon behaviours this crate works around.

pub mod breaker;
mod config;
mod conn;
pub mod keys;
pub mod query;
pub mod secure;
mod store;
mod supervisor;

pub use breaker::{BreakerState, CircuitBreaker};
pub use config::StoreConfig;
pub use keys::repo_id;
pub use secure::{Password, create_private_dir, load_or_create_acl};
pub use store::{MoonStore, is_writes_paused};
pub use supervisor::{MoonProbe, MoonSupervisor, SupervisorStatus, process_basename, refusal};
