//! Typed-decision head (`type_emb`, `head.layers.*`, `scorer.*` weights).
//!
//! Port of `DecisionModel.forward` after the encoder:
//! `h += type_emb[qtype]`; 2 × pre-norm `nn.TransformerEncoderLayer` (MHA with packed
//! `in_proj`, ReLU FFN, key padding mask); gather the option `[MASK]` positions;
//! `scorer = LayerNorm → Linear → GELU(erf) → Linear(d, 1)`.
//!
//! Only the option positions are scored, so the last layer computes its keys and values over
//! the whole sequence but its queries, output projection and FFN only at those positions:
//! every op after the key/value projections is row-wise, and attention is independent per
//! query row, so the scored rows are the same as with the whole sequence.

use candle_core::{DType, Result, Tensor};
use candle_nn::{Linear, Module, VarBuilder};

use crate::nn::{Norm, QkvProj, apply, attention, linear, self_attention};

/// Bias-ful LayerNorm epsilon of `nn.TransformerEncoderLayer` / `nn.LayerNorm` defaults.
const HEAD_NORM_EPS: f64 = 1e-5;
/// Number of question types (`choice`, `score`, `noul`).
const N_QTYPES: usize = 3;

/// A head layer's attention input projection.
#[derive(Debug, Clone)]
enum InProj {
    /// `[q | k]` and `v` over every position (all layers but the last).
    Packed(QkvProj),
    /// Separate `q`, `k`, `v`: the last layer projects `q` at the option positions only.
    Split { q: Linear, k: Linear, v: Linear },
}

#[derive(Debug, Clone)]
struct HeadLayer {
    norm1: Norm,
    in_proj: InProj,
    out_proj: Linear,
    norm2: Norm,
    linear1: Linear,
    linear2: Linear,
}

impl HeadLayer {
    /// Residual MLP block: `x + linear2(relu(linear1(norm2(x))))`.
    fn mlp(&self, x: Tensor) -> Result<Tensor> {
        let ff = apply(&self.linear1, &self.norm2.forward(&x)?)?.relu()?;
        x + apply(&self.linear2, &ff)?
    }
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
            let w = lvb.get((3 * d, d), "self_attn.in_proj_weight")?;
            let bias = lvb.get(3 * d, "self_attn.in_proj_bias")?;
            let in_proj = if i + 1 == n_layers {
                let part = |j: usize| -> Result<Linear> {
                    Ok(Linear::new(
                        w.narrow(0, j * d, d)?.contiguous()?,
                        Some(bias.narrow(0, j * d, d)?.contiguous()?),
                    ))
                };
                InProj::Split {
                    q: part(0)?,
                    k: part(1)?,
                    v: part(2)?,
                }
            } else {
                InProj::Packed(QkvProj::from_packed(w, Some(bias))?)
            };
            layers.push(HeadLayer {
                norm1: Norm::load(lvb.pp("norm1"), d, HEAD_NORM_EPS, true)?,
                in_proj,
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
    /// - `key_mask`: additive `(b, 1, s, s)` padding mask, the same for every query row (see
    ///   [`crate::nn::AttnMasks`])
    /// - `flat_markers`: `(b * kmax)` u32 indices into the flattened `(b * s)` token axis,
    ///   `kmax` per row
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
        let n_markers = flat_markers.dim(0)?;
        let kmax = n_markers / b.max(1);
        if kmax * b != n_markers || kmax > s {
            candle_core::bail!("head: {n_markers} markers for {b} rows of {s} tokens");
        }
        let scale = 1.0 / (self.head_dim as f64).sqrt();
        let type_vec = self.type_emb.index_select(qtype, 0)?.reshape((b, 1, d))?;
        let mut x = h.broadcast_add(&type_vec)?;
        // Rows of a contiguous `(b, s, d)` tensor at the markers, as `(b * kmax, d)`.
        let at_markers = |t: &Tensor| t.reshape((b * s, d))?.index_select(flat_markers, 0);
        let mut m = None;
        for layer in &self.layers {
            let normed = layer.norm1.forward(&x)?;
            match &layer.in_proj {
                InProj::Packed(proj) => {
                    let (qk, v) = proj.forward(&normed)?;
                    let att =
                        self_attention(&qk, &v, self.heads, self.head_dim, None, key_mask, scale)?;
                    x = layer.mlp((x + apply(&layer.out_proj, &att)?)?)?;
                }
                InProj::Split { q, k, v } => {
                    let (k, v) = (k.forward(&normed)?, v.forward(&normed)?);
                    let q = q.forward(&at_markers(&normed)?)?.reshape((b, kmax, d))?;
                    let mask = key_mask.narrow(2, 0, kmax)?;
                    let att = attention(&q, &k, &v, self.heads, self.head_dim, &mask, scale)?;
                    let xm = at_markers(&x)?.reshape((b, kmax, d))?;
                    let xm = layer.mlp((xm + apply(&layer.out_proj, &att)?)?)?;
                    m = Some(xm.reshape((b * kmax, d))?);
                }
            }
        }
        // Only the last layer is split; with no layers at all the markers are gathered here.
        let m = match m {
            Some(m) => m,
            None => at_markers(&x)?,
        };
        let z = self
            .scorer_l1
            .forward(&self.scorer_norm.forward(&m)?)?
            .gelu_erf()?;
        let logits = self.scorer_l2.forward(&z)?; // (b * kmax, 1)
        logits.squeeze(1)?.to_dtype(DType::F32)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use candle_core::{D, Device};

    use super::*;
    use crate::nn::AttnMasks;
    use crate::nn::tests::{devices, padding};

    const D_MODEL: usize = 128;

    /// Random head weights (and a type embedding) under their state-dict names.
    fn weights(n_layers: usize) -> HashMap<String, Tensor> {
        let d = D_MODEL;
        let mut w = HashMap::new();
        let mut put = |name: String, shape: &[usize], std: f32| {
            let t = Tensor::randn(0f32, std, shape, &Device::Cpu).unwrap();
            w.insert(name, t);
        };
        put("type_emb.weight".into(), &[N_QTYPES, d], 1.0);
        for i in 0..n_layers {
            let p = format!("head.layers.{i}");
            for n in ["norm1", "norm2"] {
                put(format!("{p}.{n}.weight"), &[d], 0.2);
                put(format!("{p}.{n}.bias"), &[d], 0.2);
            }
            put(format!("{p}.self_attn.in_proj_weight"), &[3 * d, d], 0.1);
            put(format!("{p}.self_attn.in_proj_bias"), &[3 * d], 0.1);
            put(format!("{p}.self_attn.out_proj.weight"), &[d, d], 0.1);
            put(format!("{p}.self_attn.out_proj.bias"), &[d], 0.1);
            put(format!("{p}.linear1.weight"), &[4 * d, d], 0.1);
            put(format!("{p}.linear1.bias"), &[4 * d], 0.1);
            put(format!("{p}.linear2.weight"), &[d, 4 * d], 0.1);
            put(format!("{p}.linear2.bias"), &[d], 0.1);
        }
        put("scorer.0.weight".into(), &[d], 0.2);
        put("scorer.0.bias".into(), &[d], 0.2);
        put("scorer.1.weight".into(), &[d, d], 0.1);
        put("scorer.1.bias".into(), &[d], 0.1);
        put("scorer.3.weight".into(), &[1, d], 0.1);
        put("scorer.3.bias".into(), &[1], 0.1);
        w
    }

    fn ln(x: &Tensor, w: &Tensor, b: &Tensor) -> Tensor {
        let mean = x.mean_keepdim(D::Minus1).unwrap();
        let xc = x.broadcast_sub(&mean).unwrap();
        let var = xc.sqr().unwrap().mean_keepdim(D::Minus1).unwrap();
        let inv = (var + HEAD_NORM_EPS)
            .unwrap()
            .sqrt()
            .unwrap()
            .recip()
            .unwrap();
        xc.broadcast_mul(&inv)
            .unwrap()
            .broadcast_mul(w)
            .unwrap()
            .broadcast_add(b)
            .unwrap()
    }

    fn dense(x: &Tensor, w: &Tensor, b: &Tensor) -> Tensor {
        x.broadcast_matmul(&w.t().unwrap())
            .unwrap()
            .broadcast_add(b)
            .unwrap()
    }

    /// The head written out op by op in f32 on the CPU, every layer over every position (the
    /// Python reference's order of work).
    fn reference(
        w: &HashMap<String, Tensor>,
        n_layers: usize,
        h: &Tensor,
        qtypes: &[u32],
        lens: &[usize],
        markers: &[Vec<usize>],
    ) -> Vec<f32> {
        let (b, s, d) = h.dims3().unwrap();
        let (heads, hd) = (d / 64, 64);
        let g = |n: &str| w[n].clone();
        let te = g("type_emb.weight");
        let mut x = h.clone();
        let rows: Vec<Tensor> = qtypes
            .iter()
            .map(|&q| te.get(q as usize).unwrap().reshape((1, 1, d)).unwrap())
            .collect();
        x = x.broadcast_add(&Tensor::cat(&rows, 0).unwrap()).unwrap();
        let mut neg = vec![0f32; b * s];
        for (r, &l) in lens.iter().enumerate() {
            neg[r * s + l..(r + 1) * s].fill(-1e9);
        }
        let neg = Tensor::from_vec(neg, (b, 1, 1, s), &Device::Cpu).unwrap();
        for i in 0..n_layers {
            let p = |n: &str| g(&format!("head.layers.{i}.{n}"));
            let n1 = ln(&x, &p("norm1.weight"), &p("norm1.bias"));
            let qkv = dense(
                &n1,
                &p("self_attn.in_proj_weight"),
                &p("self_attn.in_proj_bias"),
            );
            let part = |j: usize| {
                qkv.narrow(2, j * d, d)
                    .unwrap()
                    .reshape((b, s, heads, hd))
                    .unwrap()
                    .transpose(1, 2)
                    .unwrap()
                    .contiguous()
                    .unwrap()
            };
            let (q, k, v) = (part(0), part(1), part(2));
            let att = (q.matmul(&k.t().unwrap()).unwrap() / (hd as f64).sqrt()).unwrap();
            let att = candle_nn::ops::softmax_last_dim(&att.broadcast_add(&neg).unwrap()).unwrap();
            let o = att
                .matmul(&v)
                .unwrap()
                .transpose(1, 2)
                .unwrap()
                .reshape((b, s, d))
                .unwrap();
            x = (x + dense(
                &o,
                &p("self_attn.out_proj.weight"),
                &p("self_attn.out_proj.bias"),
            ))
            .unwrap();
            let n2 = ln(&x, &p("norm2.weight"), &p("norm2.bias"));
            let ff = dense(&n2, &p("linear1.weight"), &p("linear1.bias"))
                .relu()
                .unwrap();
            x = (x + dense(&ff, &p("linear2.weight"), &p("linear2.bias"))).unwrap();
        }
        let mut out = Vec::new();
        for (r, ms) in markers.iter().enumerate() {
            for &m in ms {
                let row = x.get(r).unwrap().get(m).unwrap().reshape((1, d)).unwrap();
                let z = ln(&row, &g("scorer.0.weight"), &g("scorer.0.bias"));
                let z = dense(&z, &g("scorer.1.weight"), &g("scorer.1.bias"))
                    .gelu_erf()
                    .unwrap();
                let z = dense(&z, &g("scorer.3.weight"), &g("scorer.3.bias"));
                out.push(z.flatten_all().unwrap().to_vec1::<f32>().unwrap()[0]);
            }
        }
        out
    }

    #[test]
    fn head_logits_match_the_layer_by_layer_reference() {
        let (b, s) = (3usize, 41usize);
        let lens = [41usize, 30, 17];
        // Mixed option counts in one batch, as `decide` builds them (absent options gather
        // [CLS] and are dropped); one row with a single option.
        let markers = vec![vec![3usize, 9, 20, 33], vec![2, 25], vec![16]];
        let qtypes = [0u32, 2, 1];
        let kmax = markers.iter().map(Vec::len).max().unwrap();
        for n_layers in [1usize, 2] {
            let w = weights(n_layers);
            let h_cpu = Tensor::randn(0f32, 1.0, (b, s, D_MODEL), &Device::Cpu).unwrap();
            let want = reference(&w, n_layers, &h_cpu, &qtypes, &lens, &markers);
            for (dev, dtype) in devices() {
                let tensors: HashMap<String, Tensor> = w
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_dtype(dtype).unwrap()))
                    .collect();
                let vb = VarBuilder::from_tensors(tensors, dtype, &dev);
                let head = DecisionHead::load(vb, D_MODEL, n_layers).unwrap();
                let h = h_cpu.to_dtype(dtype).unwrap().to_device(&dev).unwrap();
                let qtype = Tensor::from_vec(qtypes.to_vec(), b, &dev).unwrap();
                let band = crate::nn::window_band(s, 8, dtype, &dev).unwrap();
                let masks = AttnMasks::new(&padding(&lens, s, &dev), &band, dtype).unwrap();
                let mut flat = vec![0u32; b * kmax];
                for (r, ms) in markers.iter().enumerate() {
                    for j in 0..kmax {
                        flat[r * kmax + j] = (r * s + ms.get(j).copied().unwrap_or(0)) as u32;
                    }
                }
                let flat = Tensor::from_vec(flat, b * kmax, &dev).unwrap();
                let logits = head
                    .forward(&h, &qtype, &masks.global, &flat)
                    .unwrap()
                    .to_vec1::<f32>()
                    .unwrap();
                let got: Vec<f32> = markers
                    .iter()
                    .enumerate()
                    .flat_map(|(r, ms)| (0..ms.len()).map(move |j| (r, j)))
                    .map(|(r, j)| logits[r * kmax + j])
                    .collect();
                let tol = if dtype == DType::F32 { 1e-4 } else { 2e-2 };
                for (g, w) in got.iter().zip(&want) {
                    assert!(
                        (g - w).abs() <= tol,
                        "{dev:?} {n_layers} layers: {got:?} vs {want:?}"
                    );
                }
            }
        }
    }
}
