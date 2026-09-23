//! Model directory layout and configuration files.
//!
//! A model directory (`laya-base`, `laya-code`, ...) contains:
//! - `model.safetensors` — the full `DecisionModel` state dict (prefix `encoder.` for ModernBERT)
//! - `encoder/config.json` — HF ModernBERT config
//! - `tokenizer/tokenizer.json` — HF fast tokenizer
//! - `rl_agent_config.json` — sequence budgets and calibration temperatures

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{ModelError, Result};

/// Resolved paths of the files inside a model directory.
#[derive(Debug, Clone)]
pub struct ModelFiles {
    /// `model.safetensors`
    pub weights: PathBuf,
    /// `encoder/config.json`
    pub encoder_config: PathBuf,
    /// `tokenizer/tokenizer.json`
    pub tokenizer: PathBuf,
    /// `rl_agent_config.json`
    pub agent_config: PathBuf,
}

impl ModelFiles {
    /// Resolve and verify the layout of `dir`.
    pub fn resolve(dir: &Path) -> Result<Self> {
        let files = Self {
            weights: dir.join("model.safetensors"),
            encoder_config: dir.join("encoder").join("config.json"),
            tokenizer: dir.join("tokenizer").join("tokenizer.json"),
            agent_config: dir.join("rl_agent_config.json"),
        };
        for p in [
            &files.weights,
            &files.encoder_config,
            &files.tokenizer,
            &files.agent_config,
        ] {
            if !p.is_file() {
                return Err(ModelError::MissingFile(p.clone()));
            }
        }
        Ok(files)
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let text = std::fs::read_to_string(path).map_err(|source| ModelError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&text).map_err(|e| ModelError::Config {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })
}

#[derive(Debug, Clone, Deserialize)]
struct RopeEntry {
    rope_theta: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct RopeParameters {
    full_attention: RopeEntry,
    sliding_attention: RopeEntry,
}

/// HF ModernBERT configuration (the subset needed for inference).
///
/// Accepts both the transformers ≥5 layout (`rope_parameters.{full,sliding}_attention.rope_theta`,
/// `layer_types`) and the older flat layout (`global_rope_theta`, `local_rope_theta`).
#[derive(Debug, Clone, Deserialize)]
pub struct EncoderConfig {
    /// Vocabulary size (rows of the token embedding).
    pub vocab_size: usize,
    /// Model width `d`.
    pub hidden_size: usize,
    /// Number of transformer layers.
    pub num_hidden_layers: usize,
    /// Attention heads (head dim = hidden / heads).
    pub num_attention_heads: usize,
    /// GeGLU inner width (the `Wi` projection has `2 * intermediate_size` outputs).
    pub intermediate_size: usize,
    /// Maximum positions the rope tables could cover.
    pub max_position_embeddings: usize,
    /// LayerNorm epsilon (transformers ≥5 name); see [`norm_eps`](Self::norm_eps).
    #[serde(default)]
    norm_eps: Option<f64>,
    /// LayerNorm epsilon (older name).
    #[serde(default)]
    layer_norm_eps: Option<f64>,
    /// Padding token id.
    pub pad_token_id: u32,
    /// Every n-th layer (starting at 0) uses global attention.
    pub global_attn_every_n_layers: usize,
    /// Sliding window size (tokens attend to `|i-j| <= local_attention / 2`).
    pub local_attention: usize,
    /// Explicit per-layer attention kinds (`full_attention` / `sliding_attention`), if present.
    #[serde(default)]
    pub layer_types: Option<Vec<String>>,
    #[serde(default)]
    rope_parameters: Option<RopeParameters>,
    #[serde(default)]
    global_rope_theta: Option<f64>,
    #[serde(default)]
    local_rope_theta: Option<f64>,
}

impl EncoderConfig {
    /// Load and validate `encoder/config.json`.
    pub fn load(path: &Path) -> Result<Self> {
        let cfg: Self = read_json(path)?;
        let bad = |reason: &str| ModelError::Config {
            path: path.to_path_buf(),
            reason: reason.to_string(),
        };
        if !cfg.hidden_size.is_multiple_of(cfg.num_attention_heads) {
            return Err(bad("hidden_size must be a multiple of num_attention_heads"));
        }
        if !(cfg.hidden_size / cfg.num_attention_heads).is_multiple_of(2) {
            return Err(bad("head dim must be even for rotary embeddings"));
        }
        if cfg.rope_parameters.is_none()
            && (cfg.global_rope_theta.is_none() || cfg.local_rope_theta.is_none())
        {
            return Err(bad(
                "missing rope_parameters or global_rope_theta/local_rope_theta",
            ));
        }
        if let Some(t) = &cfg.layer_types {
            if t.len() != cfg.num_hidden_layers {
                return Err(bad("layer_types length != num_hidden_layers"));
            }
            if let Some(unknown) = t
                .iter()
                .find(|k| k.as_str() != "full_attention" && k.as_str() != "sliding_attention")
            {
                return Err(bad(&format!("unknown layer type {unknown:?}")));
            }
        }
        Ok(cfg)
    }

    /// LayerNorm epsilon (`norm_eps`, falling back to `layer_norm_eps`, then `1e-5`).
    pub fn norm_eps(&self) -> f64 {
        self.norm_eps.or(self.layer_norm_eps).unwrap_or(1e-5)
    }

    /// Width of one attention head.
    pub fn head_dim(&self) -> usize {
        self.hidden_size / self.num_attention_heads
    }

    /// Rope base for global-attention layers.
    pub fn global_rope_theta(&self) -> f64 {
        self.rope_parameters
            .as_ref()
            .map(|r| r.full_attention.rope_theta)
            .or(self.global_rope_theta)
            .unwrap_or(160_000.0)
    }

    /// Rope base for sliding-window layers.
    pub fn local_rope_theta(&self) -> f64 {
        self.rope_parameters
            .as_ref()
            .map(|r| r.sliding_attention.rope_theta)
            .or(self.local_rope_theta)
            .unwrap_or(10_000.0)
    }

    /// Whether layer `idx` uses global (full) attention.
    pub fn is_global(&self, idx: usize) -> bool {
        match &self.layer_types {
            Some(t) => t[idx] == "full_attention",
            None => idx.is_multiple_of(self.global_attn_every_n_layers),
        }
    }

    /// Half window: tokens attend to keys with `|i - j| <= local_window()`.
    pub fn local_window(&self) -> usize {
        self.local_attention / 2
    }
}

/// `rl_agent_config.json`: sequence budgets, head depth and calibration temperatures.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    /// Number of head transformer layers.
    #[serde(default = "default_head_layers")]
    pub head_layers: usize,
    /// Total sequence budget (tokens).
    pub max_len: usize,
    /// Budget for `question + options` (tokens).
    pub head_max_len: usize,
    /// Per-question-type temperature fallback, indexed by [`crate::QType`].
    #[serde(default = "default_temperature")]
    pub temperature: Vec<f32>,
    /// Per-`type:cardinality` temperature (e.g. `noul:2`), preferred over `temperature`.
    #[serde(default)]
    pub temperature_by_options: HashMap<String, f32>,
}

fn default_head_layers() -> usize {
    2
}

fn default_temperature() -> Vec<f32> {
    vec![1.0, 1.0, 1.0]
}

impl AgentConfig {
    /// Load and validate `rl_agent_config.json`.
    pub fn load(path: &Path) -> Result<Self> {
        let cfg: Self = read_json(path)?;
        let bad = |reason: &str| ModelError::Config {
            path: path.to_path_buf(),
            reason: reason.to_string(),
        };
        if cfg.head_max_len + 3 > cfg.max_len {
            return Err(bad("head_max_len must leave room for the state"));
        }
        if cfg.temperature.len() < 3 {
            return Err(bad("temperature must have 3 entries (choice, score, noul)"));
        }
        Ok(cfg)
    }

    /// Calibration temperature for a question of type `qtype` with `k` options
    /// (port of `rl_common.temp_bucket` + the lookup in `rl_agent_api.py`).
    pub fn temperature_for(&self, qtype: crate::QType, k: usize) -> f32 {
        let size = if k <= 2 {
            "2"
        } else if k <= 5 {
            "3-5"
        } else if k <= 10 {
            "6-10"
        } else {
            "11+"
        };
        let key = format!("{}:{}", qtype.name(), size);
        self.temperature_by_options
            .get(&key)
            .copied()
            .unwrap_or(self.temperature[qtype as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temperature_bucket_lookup_prefers_cardinality_key() {
        let cfg = AgentConfig {
            head_layers: 2,
            max_len: 512,
            head_max_len: 192,
            temperature: vec![1.5, 1.25, 1.75],
            temperature_by_options: HashMap::from([
                ("noul:2".to_string(), 1.98f32),
                ("choice:3-5".to_string(), 1.76),
            ]),
        };
        assert_eq!(cfg.temperature_for(crate::QType::Noul, 2), 1.98);
        assert_eq!(cfg.temperature_for(crate::QType::Choice, 4), 1.76);
        assert_eq!(cfg.temperature_for(crate::QType::Choice, 12), 1.5);
        assert_eq!(cfg.temperature_for(crate::QType::Score, 3), 1.25);
    }

    #[test]
    fn encoder_config_accepts_nested_rope_layout() {
        let json = r#"{"vocab_size":8,"hidden_size":64,"num_hidden_layers":3,"num_attention_heads":2,
            "intermediate_size":16,"max_position_embeddings":128,"norm_eps":1e-5,"pad_token_id":0,
            "global_attn_every_n_layers":3,"local_attention":8,
            "layer_types":["full_attention","sliding_attention","sliding_attention"],
            "rope_parameters":{"full_attention":{"rope_theta":160000.0},"sliding_attention":{"rope_theta":10000.0}}}"#;
        let cfg: EncoderConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.global_rope_theta(), 160000.0);
        assert_eq!(cfg.local_rope_theta(), 10000.0);
        assert!(cfg.is_global(0));
        assert!(!cfg.is_global(1));
        assert_eq!(cfg.local_window(), 4);
        assert_eq!(cfg.head_dim(), 32);
        assert_eq!(cfg.norm_eps(), 1e-5);
    }

    #[test]
    fn encoder_config_accepts_both_eps_names_at_once() {
        let json = r#"{"vocab_size":8,"hidden_size":64,"num_hidden_layers":1,"num_attention_heads":2,
            "intermediate_size":16,"max_position_embeddings":128,"norm_eps":2e-5,"layer_norm_eps":2e-5,
            "pad_token_id":0,"global_attn_every_n_layers":3,"local_attention":8,
            "global_rope_theta":160000.0,"local_rope_theta":10000.0}"#;
        let cfg: EncoderConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.norm_eps(), 2e-5);
        assert_eq!(cfg.global_rope_theta(), 160000.0);
    }
}
