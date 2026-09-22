//! Error type of the model crate.

use std::path::PathBuf;

/// Everything that can go wrong while loading or running the Laya model.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    /// A required file of the model directory is missing.
    #[error("model file not found: {0}")]
    MissingFile(PathBuf),
    /// Filesystem failure while reading a model file.
    #[error("io error reading {path}: {source}")]
    Io {
        /// File being read.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// A config file exists but is malformed or inconsistent.
    #[error("invalid config {path}: {reason}")]
    Config {
        /// File being parsed.
        path: PathBuf,
        /// What was wrong.
        reason: String,
    },
    /// The `tokenizers` crate failed (load or encode).
    #[error("tokenizer error: {0}")]
    Tokenizer(String),
    /// The requested compute device cannot be used.
    #[error("device unavailable: {0}")]
    Device(String),
    /// Tensor computation error (candle).
    #[error("tensor error: {0}")]
    Candle(#[from] candle_core::Error),
    /// The question's options do not fit into the head budget (markers were truncated).
    #[error("options do not fit into head_max_len: {0}")]
    OptionsDoNotFit(String),
    /// The caller-supplied deadline expired before all micro-batches were scored.
    #[error("deadline exceeded")]
    Deadline,
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, ModelError>;

impl From<ModelError> for laya_core::Error {
    fn from(e: ModelError) -> Self {
        match e {
            ModelError::Deadline => laya_core::Error::Deadline,
            other => laya_core::Error::Model(other.to_string()),
        }
    }
}

impl From<tokenizers::Error> for ModelError {
    fn from(e: tokenizers::Error) -> Self {
        ModelError::Tokenizer(e.to_string())
    }
}
