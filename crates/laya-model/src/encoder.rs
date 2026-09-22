//! ModernBERT encoder (`encoder.*` weights of the Laya state dict).
//!
//! Faithful to `transformers`' `ModernBertModel`: token embedding → LayerNorm; 28 pre-norm layers
//! with alternating global / sliding-window attention (rope θ 160000 / 10000, no biases),
//! GeGLU MLP; final LayerNorm. Layer 0 has no attention norm (identity).

use candle_core::{DType, Device, Result, Tensor};
use candle_nn::{Embedding, Linear, Module, VarBuilder};

use crate::config::EncoderConfig;
use crate::nn::{AttnMasks, Norm, QkvProj, Rope, apply, linear, self_attention};

/// Rotary cos/sin tables `(max_pos, head_dim / 2)`, computed in f32 and cast once.
#[derive(Debug, Clone)]
struct RopeTable {
    cos: Tensor,
    sin: Tensor,
}

impl RopeTable {
    fn new(
        theta: f64,
        head_dim: usize,
        max_pos: usize,
        dtype: DType,
        device: &Device,
    ) -> Result<Self> {
        let half = head_dim / 2;
        let mut cos = Vec::with_capacity(max_pos * half);
        let mut sin = Vec::with_capacity(max_pos * half);
        for pos in 0..max_pos {
            for i in 0..half {
                let inv_freq = 1.0 / theta.powf((2 * i) as f64 / head_dim as f64);
                let angle = pos as f64 * inv_freq;
                cos.push(angle.cos() as f32);
                sin.push(angle.sin() as f32);
            }
        }
        Ok(Self {
            cos: Tensor::from_vec(cos, (max_pos, half), device)?.to_dtype(dtype)?,
            sin: Tensor::from_vec(sin, (max_pos, half), device)?.to_dtype(dtype)?,
        })
    }

    /// Tables for the first `s` positions.
    fn slice(&self, s: usize) -> Result<(Tensor, Tensor)> {
        Ok((self.cos.narrow(0, 0, s)?, self.sin.narrow(0, 0, s)?))
    }
}

#[derive(Debug, Clone)]
struct Layer {
    attn_norm: Option<Norm>,
    wqkv: QkvProj,
    wo: Linear,
    mlp_norm: Norm,
    /// First half of `mlp.Wi` (the GeGLU input branch).
    wi_input: Linear,
    /// Second half of `mlp.Wi` (the GeGLU gate branch).
    wi_gate: Linear,
    wo_mlp: Linear,
    global: bool,
}

/// ModernBERT backbone.
#[derive(Debug, Clone)]
pub struct ModernBert {
    tok_emb: Embedding,
    emb_norm: Norm,
    layers: Vec<Layer>,
    final_norm: Norm,
    rope_global: RopeTable,
    rope_local: RopeTable,
    heads: usize,
    head_dim: usize,
}

impl ModernBert {
    /// Load from `vb` (already prefixed with `encoder`). `max_pos` bounds the rope tables.
    pub fn load(vb: VarBuilder, cfg: &EncoderConfig, max_pos: usize) -> Result<Self> {
        let d = cfg.hidden_size;
        let eps = cfg.norm_eps();
        let tok_emb = candle_nn::embedding(cfg.vocab_size, d, vb.pp("embeddings.tok_embeddings"))?;
        let emb_norm = Norm::load(vb.pp("embeddings.norm"), d, eps, false)?;
        let mut layers = Vec::with_capacity(cfg.num_hidden_layers);
        for i in 0..cfg.num_hidden_layers {
            let lvb = vb.pp(format!("layers.{i}"));
            let attn_norm = if lvb.pp("attn_norm").contains_tensor("weight") {
                Some(Norm::load(lvb.pp("attn_norm"), d, eps, false)?)
            } else {
                None
            };
            // `Wi` is split into its two row halves at load time so that both GeGLU branches
            // come out of their matmul contiguous (a `narrow` on the fused output would make
            // the following elementwise kernels strided, which is 2.5x slower on Metal).
            let inter = cfg.intermediate_size;
            let wi = lvb.get((2 * inter, d), "mlp.Wi.weight")?;
            layers.push(Layer {
                attn_norm,
                wqkv: QkvProj::from_packed(lvb.get((3 * d, d), "attn.Wqkv.weight")?, None)?,
                wo: linear(lvb.pp("attn.Wo"), d, d, false)?,
                mlp_norm: Norm::load(lvb.pp("mlp_norm"), d, eps, false)?,
                wi_input: Linear::new(wi.narrow(0, 0, inter)?.contiguous()?, None),
                wi_gate: Linear::new(wi.narrow(0, inter, inter)?.contiguous()?, None),
                wo_mlp: linear(lvb.pp("mlp.Wo"), inter, d, false)?,
                global: cfg.is_global(i),
            });
        }
        let final_norm = Norm::load(vb.pp("final_norm"), d, eps, false)?;
        let max_pos = max_pos.min(cfg.max_position_embeddings).max(1);
        Ok(Self {
            tok_emb,
            emb_norm,
            layers,
            final_norm,
            rope_global: RopeTable::new(
                cfg.global_rope_theta(),
                cfg.head_dim(),
                max_pos,
                vb.dtype(),
                vb.device(),
            )?,
            rope_local: RopeTable::new(
                cfg.local_rope_theta(),
                cfg.head_dim(),
                max_pos,
                vb.dtype(),
                vb.device(),
            )?,
            heads: cfg.num_attention_heads,
            head_dim: cfg.head_dim(),
        })
    }

    /// `last_hidden_state` for `ids` of shape `(b, s)` (u32) and the batch's masks.
    pub fn forward(&self, ids: &Tensor, masks: &AttnMasks) -> Result<Tensor> {
        let mut prof = StageProfiler::from_env(ids.device());
        let s = ids.dim(1)?;
        let (cos_g, sin_g) = self.rope_global.slice(s)?;
        let (cos_l, sin_l) = self.rope_local.slice(s)?;
        let mut x = self.emb_norm.forward(&self.tok_emb.forward(ids)?)?;
        prof.tick("embed")?;
        let scale = 1.0 / (self.head_dim as f64).sqrt();
        for layer in &self.layers {
            // Attention block (pre-norm; layer 0 has identity norm).
            let normed = match &layer.attn_norm {
                Some(n) => n.forward(&x)?,
                None => x.clone(),
            };
            prof.tick("attn_norm")?;
            let (qk, v) = layer.wqkv.forward(&normed)?;
            prof.tick("wqkv")?;
            let (rope, mask) = if layer.global {
                (
                    Rope {
                        cos: &cos_g,
                        sin: &sin_g,
                    },
                    &masks.global,
                )
            } else {
                (
                    Rope {
                        cos: &cos_l,
                        sin: &sin_l,
                    },
                    &masks.local,
                )
            };
            let att = self_attention(&qk, &v, self.heads, self.head_dim, Some(rope), mask, scale)?;
            prof.tick("attention")?;
            x = (x + apply(&layer.wo, &att)?)?;
            prof.tick("wo+res")?;
            // GeGLU MLP block.
            let normed = layer.mlp_norm.forward(&x)?;
            prof.tick("mlp_norm")?;
            let input = apply(&layer.wi_input, &normed)?;
            let gate = apply(&layer.wi_gate, &normed)?;
            prof.tick("wi")?;
            let h = (input.gelu_erf()? * gate)?;
            prof.tick("geglu")?;
            x = (x + apply(&layer.wo_mlp, &h)?)?;
            prof.tick("wo_mlp+res")?;
        }
        let out = self.final_norm.forward(&x)?;
        prof.tick("final_norm")?;
        prof.report("encoder");
        Ok(out)
    }
}

/// Per-stage wall-clock accumulator, enabled by `LAYA_PROFILE=1`. Each tick synchronizes the
/// device, so it measures true kernel time at the cost of pipelining; it is a diagnostic only.
pub(crate) struct StageProfiler {
    device: Option<Device>,
    last: std::time::Instant,
    stages: Vec<(&'static str, f64)>,
}

impl StageProfiler {
    pub(crate) fn from_env(device: &Device) -> Self {
        let enabled = std::env::var_os("LAYA_PROFILE").is_some_and(|v| v != "0" && !v.is_empty());
        Self {
            device: enabled.then(|| device.clone()),
            last: std::time::Instant::now(),
            stages: Vec::new(),
        }
    }

    pub(crate) fn tick(&mut self, name: &'static str) -> Result<()> {
        let Some(dev) = &self.device else {
            return Ok(());
        };
        dev.synchronize()?;
        let now = std::time::Instant::now();
        let dt = now.duration_since(self.last).as_secs_f64() * 1e3;
        self.last = now;
        match self.stages.iter_mut().find(|(n, _)| *n == name) {
            Some((_, t)) => *t += dt,
            None => self.stages.push((name, dt)),
        }
        Ok(())
    }

    pub(crate) fn report(&self, what: &str) {
        if self.device.is_none() {
            return;
        }
        let total: f64 = self.stages.iter().map(|(_, t)| t).sum();
        eprintln!("laya-model profile [{what}] total {total:.1} ms");
        for (name, t) in &self.stages {
            eprintln!(
                "  {name:<12} {t:>8.1} ms  {:>5.1}%",
                100.0 * t / total.max(1e-9)
            );
        }
    }
}
