//! Native inference for the Laya typed-decision model (ModernBERT-large encoder + decision head)
//! using candle: CPU f32 everywhere, Metal f16 on macOS (cargo feature `metal`).
//!
//! The Python reference (`rl_agent_api.py` / `rl_common.py` in the model directory) is the
//! specification; `tests/parity.rs` checks logits and calibrated probabilities against
//! `fixtures/laya_parity.json`.
//!
//! ```no_run
//! use std::path::Path;
//! use laya_model::{DeviceKind, LayaModel};
//!
//! let model = LayaModel::load(Path::new("~/.cache/laya-codex/models/laya-base"), DeviceKind::Auto)?;
//! let p = model.noul("Is this code relevant to: add TLS?", &["fn main() {}".to_string()])?;
//! assert!(p[0] >= 0.0 && p[0] <= 1.0);
//! # Ok::<(), laya_model::ModelError>(())
//! ```

pub mod config;
mod encoder;
pub mod error;
mod head;
mod model;
mod nn;
/// Internal ops exposed for the `profile_ops` example (not a stable API).
#[doc(hidden)]
pub mod nn_probe {
    pub use crate::nn::window_band;
}
mod scorer;
pub mod sequence;

pub use config::{AgentConfig, EncoderConfig, ModelFiles};
pub use error::{ModelError, Result};
pub use model::{Decision, DeviceKind, LayaModel, LoadOptions, metal_available};
pub use scorer::{DEFAULT_MAX_STATE_TOKENS, DEFAULT_QUESTION_TEMPLATE, LayaScorer};
pub use sequence::{BuiltSequence, QType, Question, SequenceBuilder};
