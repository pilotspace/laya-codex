//! Typed-decision head (`type_emb`, `head.layers.*`, `scorer.*` weights).
//!
//! Port of `DecisionModel.forward` after the encoder:
//! `h += type_emb[qtype]`; 2 × pre-norm `nn.TransformerEncoderLayer` (MHA with packed
//! `in_proj`, ReLU FFN, key padding mask); gather the option `[MASK]` positions;
//! `scorer = LayerNorm → Linear → GELU(erf) → Linear(d, 1)`.

use candle_core::{DType, Result, Tensor};
use candle_nn::{Linear, Module, VarBuilder};

use crate::nn::{Norm, QkvProj, apply, linear, self_attention};

/// Bias-ful LayerNorm epsilon of `nn.TransformerEncoderLayer` / `nn.LayerNorm` defaults.
const HEAD_NORM_EPS: f64 = 1e-5;
/// Number of question types (`choice`, `score`, `noul`).
const N_QTYPES: usize = 3;

#[derive(Debug, Clone)]
struct HeadLayer {
    norm1: Norm,
    in_proj: QkvProj,
    out_proj: Linear,
    norm2: Norm,
    linear1: Linear,
    linear2: Linear,
}

/// The decision head.
#[derive(Debug, Clone)]
pub struct DecisionHead {
    type_emb: Tensor,
    layers: Vec<HeadLayer>,
    scorer_norm: Norm,
    scorer_l1: Linear,
    scorer_l2: Linear,
    heads: usize,
    head_dim: usize,
}

impl DecisionHead {
    /// Load from the state-dict root. `d` is the encoder width, `n_layers` the head depth.
    pub fn load(vb: VarBuilder, d: usize, n_layers: usize) -> Result<Self> {
        let heads = (d / 64).max(1);
        let mut layers = Vec::with_capacity(n_layers);
        for i in 0..n_layers {
            let lvb = vb.pp(format!("head.layers.{i}"));
            layers.push(HeadLayer {
                norm1: Norm::load(lvb.pp("norm1"), d, HEAD_NORM_EPS, true)?,
                in_proj: QkvProj::from_packed(
                    lvb.get((3 * d, d), "self_attn.in_proj_weight")?,
                    Some(lvb.get(3 * d, "self_attn.in_proj_bias")?),
                )?,
                out_proj: linear(lvb.pp("self_attn.out_proj"), d, d, true)?,
                norm2: Norm::load(lvb.pp("norm2"), d, HEAD_NORM_EPS, true)?,
                linear1: linear(lvb.pp("linear1"), d, 4 * d, true)?,
                linear2: linear(lvb.pp("linear2"), 4 * d, d, true)?,
            });
        }
        Ok(Self {
            type_emb: vb.get((N_QTYPES, d), "type_emb.weight")?,
            layers,
            scorer_norm: Norm::load(vb.pp("scorer.0"), d, HEAD_NORM_EPS, true)?,
            scorer_l1: linear(vb.pp("scorer.1"), d, d, true)?,
            scorer_l2: linear(vb.pp("scorer.3"), d, 1, true)?,
            heads,
            head_dim: d / heads,
        })
    }

    /// Raw option logits.
    ///
    /// - `h`: encoder output `(b, s, d)`
    /// - `qtype`: `(b)` u32 question-type index per row
    /// - `key_mask`: additive `(b, 1, s, s)` padding mask (see [`crate::nn::AttnMasks`])
    /// - `flat_markers`: `(b * kmax)` u32 indices into the flattened `(b * s)` token axis
    ///
    /// Returns `(b * kmax)` logits as f32 on the model device.
    pub fn forward(
        &self,
        h: &Tensor,
        qtype: &Tensor,
        key_mask: &Tensor,
        flat_markers: &Tensor,
    ) -> Result<Tensor> {
        let (b, s, d) = h.dims3()?;
        let scale = 1.0 / (self.head_dim as f64).sqrt();
        let type_vec = self.type_emb.index_select(qtype, 0)?.reshape((b, 1, d))?;
        let mut x = h.broadcast_add(&type_vec)?;
        for layer in &self.layers {
            let (qk, v) = layer.in_proj.forward(&layer.norm1.forward(&x)?)?;
            let att = self_attention(&qk, &v, self.heads, self.head_dim, None, key_mask, scale)?;
            x = (x + apply(&layer.out_proj, &att)?)?;
            let ff = apply(&layer.linear1, &layer.norm2.forward(&x)?)?.relu()?;
            x = (x + apply(&layer.linear2, &ff)?)?;
        }
        let m = x.reshape((b * s, d))?.index_select(flat_markers, 0)?;
        let z = self
            .scorer_l1
            .forward(&self.scorer_norm.forward(&m)?)?
            .gelu_erf()?;
        let logits = self.scorer_l2.forward(&z)?; // (b * kmax, 1)
        logits.squeeze(1)?.to_dtype(DType::F32)
    }
}
