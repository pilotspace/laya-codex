//! Small tensor building blocks shared by the encoder and the head.

use candle_core::{D, DType, Device, Result, Tensor};
use candle_nn::{Linear, Module, VarBuilder};

/// LayerNorm over the last dim using candle's fused kernel (mean removed, affine).
///
/// Norms without a bias get a zero bias tensor so the fused path is always taken.
#[derive(Debug, Clone)]
pub struct Norm {
    weight: Tensor,
    bias: Tensor,
    eps: f32,
}

impl Norm {
    /// Load `weight` (and `bias` when `with_bias`) from `vb`.
    pub fn load(vb: VarBuilder, size: usize, eps: f64, with_bias: bool) -> Result<Self> {
        let weight = vb.get(size, "weight")?;
        let bias = if with_bias {
            vb.get(size, "bias")?
        } else {
            Tensor::zeros(size, weight.dtype(), weight.device())?
        };
        Ok(Self {
            weight,
            bias,
            eps: eps as f32,
        })
    }

    /// `(x - mean) / sqrt(var + eps) * weight + bias` over the last dim.
    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        candle_nn::ops::layer_norm(&x.contiguous()?, &self.weight, &self.bias, self.eps)
    }
}

/// Load a linear layer (`weight` of shape `(out, in)`, optional `bias`).
pub fn linear(vb: VarBuilder, in_dim: usize, out_dim: usize, with_bias: bool) -> Result<Linear> {
    if with_bias {
        candle_nn::linear(in_dim, out_dim, vb)
    } else {
        candle_nn::linear_no_bias(in_dim, out_dim, vb)
    }
}

/// Additive mask value for "cannot attend": finite so masked rows never turn into NaN.
pub fn mask_value(dtype: DType) -> f64 {
    match dtype {
        DType::F16 | DType::BF16 => -1.0e4,
        _ => -1.0e9,
    }
}

/// Precomputed additive masks for one padded micro-batch, both `(b, 1, s, s)` and contiguous
/// (the fused Metal attention kernel accepts them broadcast over heads without a copy).
#[derive(Debug, Clone)]
pub struct AttnMasks {
    /// 0 for real keys, `mask_value` for padding.
    pub global: Tensor,
    /// Padding mask plus the sliding-window band.
    pub local: Tensor,
}

impl AttnMasks {
    /// Build from a 0/1 attention mask of shape `(b, s)` and a precomputed window band `(1, 1, s, s)`.
    pub fn new(attention_mask: &Tensor, window: &Tensor, dtype: DType) -> Result<Self> {
        let (b, s) = attention_mask.dims2()?;
        let neg = mask_value(dtype);
        let inv = (1.0 - attention_mask.to_dtype(dtype)?)?;
        let global = (inv * neg)?
            .reshape((b, 1, 1, s))?
            .broadcast_as((b, 1, s, s))?
            .contiguous()?;
        let local = global.broadcast_add(window)?;
        Ok(Self { global, local })
    }
}

/// Sliding-window band `(1, 1, s, s)`: 0 where `|i - j| <= half_window`, else `mask_value`.
pub fn window_band(s: usize, half_window: usize, dtype: DType, device: &Device) -> Result<Tensor> {
    let neg = mask_value(dtype) as f32;
    let mut m = vec![0f32; s * s];
    for i in 0..s {
        for j in 0..s {
            if i.abs_diff(j) > half_window {
                m[i * s + j] = neg;
            }
        }
    }
    Tensor::from_vec(m, (1, 1, s, s), device)?.to_dtype(dtype)
}

/// Rotary cos/sin tables of shape `(s, hd / 2)` (contiguous, model dtype) for one batch.
#[derive(Debug, Clone, Copy)]
pub struct Rope<'a> {
    /// `cos(pos * inv_freq)`.
    pub cos: &'a Tensor,
    /// `sin(pos * inv_freq)`.
    pub sin: &'a Tensor,
}

/// A packed `[q | k | v]` projection split into its `[q | k]` and `[v]` parts.
///
/// Built once at load time from a fused `(3d, d)` weight (and optional `(3d)` bias) so that
/// each forward produces `qk` and `v` directly as contiguous tensors; slicing the fused
/// `(b, s, 3d)` output instead would cost two extra copies per layer.
#[derive(Debug, Clone)]
pub struct QkvProj {
    qk: Linear,
    v: Linear,
}

impl QkvProj {
    /// Split `weight` `(3d, d)` and `bias` `(3d)` by rows into `(2d, ·)` and `(d, ·)`.
    pub fn from_packed(weight: Tensor, bias: Option<Tensor>) -> Result<Self> {
        let three_d = weight.dim(0)?;
        let d = three_d / 3;
        if d * 3 != three_d {
            candle_core::bail!("packed qkv weight rows {three_d} not divisible by 3");
        }
        let split = |t: &Tensor| -> Result<(Tensor, Tensor)> {
            Ok((
                t.narrow(0, 0, 2 * d)?.contiguous()?,
                t.narrow(0, 2 * d, d)?.contiguous()?,
            ))
        };
        let (w_qk, w_v) = split(&weight)?;
        let (b_qk, b_v) = match &bias {
            Some(b) => {
                let (x, y) = split(b)?;
                (Some(x), Some(y))
            }
            None => (None, None),
        };
        Ok(Self {
            qk: Linear::new(w_qk, b_qk),
            v: Linear::new(w_v, b_v),
        })
    }

    /// `x: (b, s, d)` → `(qk: (b, s, 2d), v: (b, s, d))`, both contiguous.
    pub fn forward(&self, x: &Tensor) -> Result<(Tensor, Tensor)> {
        Ok((self.qk.forward(x)?, self.v.forward(x)?))
    }
}

/// Multi-head self-attention from a split projection (see [`QkvProj`]).
///
/// - `qk`: `(b, s, 2 * heads * head_dim)`, packed `[q | k]` on the last dim
/// - `v`: `(b, s, heads * head_dim)`
/// - `rope`: rotary tables applied to `q` and `k` (rotate-half convention), or `None`
/// - `mask`: additive `(b, 1, s, s)` in the model dtype
///
/// Returns `(b, s, heads * head_dim)` in the `(s, h, hd)` order expected by the output
/// projection. Semantics are `softmax(rope(q) rope(k)^T * scale + mask) v` per head.
///
/// Two layouts are used, chosen by device:
/// - **Metal**: the fused `sdpa` kernel reads `q/k/v` and the mask through their strides, so
///   `q` and `k` are rotated once in `(b, s, 2h, hd)` layout (`rope_thd`, a free reshape of
///   `qk`) and handed over as transposed *views*; only the head-to-row assembly at the end
///   touches memory. A generic 4-D transpose copy on Metal costs more than the attention
///   itself, which is why the layout avoids it entirely.
/// - **CPU**: contiguous `(b, h, s, hd)` tensors (the gemm backend needs a single batch stride)
///   and the explicit matmul → mask → softmax → matmul chain.
pub fn self_attention(
    qk: &Tensor,
    v: &Tensor,
    heads: usize,
    head_dim: usize,
    rope: Option<Rope<'_>>,
    mask: &Tensor,
    scale: f64,
) -> Result<Tensor> {
    let (b, s, two_d) = qk.dims3()?;
    let d = heads * head_dim;
    if two_d != 2 * d || v.dims3()? != (b, s, d) {
        candle_core::bail!(
            "self_attention: qk {:?} / v {:?} do not match {heads} heads x {head_dim}",
            qk.shape(),
            v.shape()
        );
    }
    if qk.device().is_metal() {
        let qk = qk.reshape((b, s, 2 * heads, head_dim))?;
        let qk = match rope {
            Some(r) => candle_nn::rotary_emb::rope_thd(&qk, r.cos, r.sin)?,
            None => qk,
        };
        let q = qk.narrow(2, 0, heads)?.transpose(1, 2)?;
        let k = qk.narrow(2, heads, heads)?.transpose(1, 2)?;
        let v = v.reshape((b, s, heads, head_dim))?.transpose(1, 2)?;
        attend_metal(&q, &k, &v, mask, scale)
    } else {
        let (mut q, mut k, v) = (
            to_bhsd(qk, 0, heads, head_dim)?,
            to_bhsd(qk, d, heads, head_dim)?,
            to_bhsd(v, 0, heads, head_dim)?,
        );
        if let Some(r) = rope {
            q = candle_nn::rotary_emb::rope(&q, r.cos, r.sin)?;
            k = candle_nn::rotary_emb::rope(&k, r.cos, r.sin)?;
        }
        attend_cpu(&q, &k, &v, mask, scale)
    }
}

/// Multi-head attention of a few query rows over a whole sequence, without rotary embedding.
///
/// - `q`: `(b, lq, heads * head_dim)` contiguous
/// - `k`, `v`: `(b, s, heads * head_dim)` contiguous
/// - `mask`: additive `(b, 1, lq, s)` in the model dtype (may be a strided view)
///
/// Returns `(b, lq, heads * head_dim)`. Each query row's result is the same as that row of
/// [`self_attention`] over the whole sequence: attention is independent per query row.
pub fn attention(
    q: &Tensor,
    k: &Tensor,
    v: &Tensor,
    heads: usize,
    head_dim: usize,
    mask: &Tensor,
    scale: f64,
) -> Result<Tensor> {
    let (b, lq, d) = q.dims3()?;
    let s = k.dim(1)?;
    if d != heads * head_dim || k.dims3()? != (b, s, d) || v.dims3()? != (b, s, d) {
        candle_core::bail!(
            "attention: q {:?} / k {:?} / v {:?} do not match {heads} heads x {head_dim}",
            q.shape(),
            k.shape(),
            v.shape()
        );
    }
    if q.device().is_metal() {
        let split = |t: &Tensor, l: usize| t.reshape((b, l, heads, head_dim))?.transpose(1, 2);
        attend_metal(&split(q, lq)?, &split(k, s)?, &split(v, s)?, mask, scale)
    } else {
        attend_cpu(
            &to_bhsd(q, 0, heads, head_dim)?,
            &to_bhsd(k, 0, heads, head_dim)?,
            &to_bhsd(v, 0, heads, head_dim)?,
            mask,
            scale,
        )
    }
}

/// `(b, l, ·)` → contiguous `(b, h, l, hd)` of the `h * hd` columns starting at `offset`.
fn to_bhsd(t: &Tensor, offset: usize, heads: usize, head_dim: usize) -> Result<Tensor> {
    let (b, l, _) = t.dims3()?;
    t.narrow(2, offset, heads * head_dim)?
        .reshape((b, l, heads, head_dim))?
        .transpose(1, 2)?
        .contiguous()
}

/// Explicit `softmax(q k^T * scale + mask) v` over contiguous `(b, h, l, hd)` tensors, returned
/// as `(b, lq, h * hd)`.
fn attend_cpu(q: &Tensor, k: &Tensor, v: &Tensor, mask: &Tensor, scale: f64) -> Result<Tensor> {
    let (b, h, lq, hd) = q.dims4()?;
    let att = (q.matmul(&k.transpose(D::Minus2, D::Minus1)?.contiguous()?)? * scale)?;
    let att = att.broadcast_add(mask)?;
    let att = candle_nn::ops::softmax_last_dim(&att)?;
    att.matmul(v)?.transpose(1, 2)?.reshape((b, lq, h * hd))
}

/// Fused attention on Metal over `(b, h, l, hd)` views; returns `(b, lq, h * hd)`.
///
/// The fused kernel needs at least two query rows: with one it dispatches the single-query
/// kernel, which ignores the mask, so a single row takes the explicit path instead.
fn attend_metal(q: &Tensor, k: &Tensor, v: &Tensor, mask: &Tensor, scale: f64) -> Result<Tensor> {
    let (b, h, lq, _) = q.dims4()?;
    let s = k.dim(2)?;
    if lq < 2 {
        return attend_cpu(
            &q.contiguous()?,
            &k.contiguous()?,
            &v.contiguous()?,
            mask,
            scale,
        );
    }
    let mask = mask.broadcast_as((b, h, lq, s))?;
    let out = candle_nn::ops::sdpa(q, k, v, Some(&mask), false, scale as f32, 1.0)?;
    heads_to_rows(&out)
}

/// `(b, h, s, hd)` contiguous → `(b, s, h * hd)` contiguous.
///
/// Every `(batch, head)` tile is an `(s, hd)` block that is contiguous on both sides, so the
/// transpose is assembled from `b * h` 2-D copies (`slice_set` → `copy2d` blits). On Metal this
/// is ~3x faster than the generic strided-copy kernel that `transpose(1, 2).contiguous()`
/// would dispatch, at the same result bit-for-bit.
fn heads_to_rows(out: &Tensor) -> Result<Tensor> {
    let (b, h, s, hd) = out.dims4()?;
    let dst = Tensor::zeros((b, s, h, hd), out.dtype(), out.device())?;
    for bi in 0..b {
        let dst_b = dst.narrow(0, bi, 1)?;
        let out_b = out.narrow(0, bi, 1)?;
        for i in 0..h {
            let src = out_b.narrow(1, i, 1)?.reshape((1, s, 1, hd))?;
            dst_b.slice_set(&src, 2, i)?;
        }
    }
    dst.reshape((b, s, h * hd))
}

/// Apply a module and keep the result contiguous.
pub fn apply(m: &impl Module, x: &Tensor) -> Result<Tensor> {
    m.forward(x)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Devices to test on: always the CPU (f32), plus Metal (f16) when built and available.
    pub(crate) fn devices() -> Vec<(Device, DType)> {
        let mut out = vec![(Device::Cpu, DType::F32)];
        #[cfg(feature = "metal")]
        if candle_core::utils::metal_is_available() {
            out.push((Device::new_metal(0).expect("metal device"), DType::F16));
        }
        out
    }

    pub(crate) fn max_abs_diff(a: &Tensor, b: &Tensor) -> f32 {
        (a.to_dtype(DType::F32).unwrap() - b.to_dtype(DType::F32).unwrap())
            .unwrap()
            .abs()
            .unwrap()
            .flatten_all()
            .unwrap()
            .max(0)
            .unwrap()
            .to_scalar::<f32>()
            .unwrap()
    }

    /// Random normal tensor made on the CPU (so every device sees the same values).
    pub(crate) fn randn(shape: &[usize], dev: &Device, dtype: DType) -> Tensor {
        Tensor::randn(0f32, 1.0, shape, &Device::Cpu)
            .unwrap()
            .to_dtype(dtype)
            .unwrap()
            .to_device(dev)
            .unwrap()
    }

    /// Padding mask (b, s) with row `r` padded after `lens[r]` tokens.
    pub(crate) fn padding(lens: &[usize], s: usize, dev: &Device) -> Tensor {
        let mut m = vec![0f32; lens.len() * s];
        for (r, &l) in lens.iter().enumerate() {
            m[r * s..r * s + l].fill(1.0);
        }
        Tensor::from_vec(m, (lens.len(), s), dev).unwrap()
    }

    #[test]
    fn attention_for_a_subset_of_queries_matches_those_rows_of_full_attention() {
        let (b, s, heads, hd) = (2usize, 37usize, 2usize, 64usize);
        let d = heads * hd;
        for (dev, dtype) in devices() {
            let (qk, v) = (
                randn(&[b, s, 2 * d], &dev, dtype),
                randn(&[b, s, d], &dev, dtype),
            );
            let band = window_band(s, 8, dtype, &dev).unwrap();
            let masks = AttnMasks::new(&padding(&[s, 23], s, &dev), &band, dtype).unwrap();
            let full = self_attention(&qk, &v, heads, hd, None, &masks.global, 0.125).unwrap();
            let k = qk.narrow(2, d, d).unwrap().contiguous().unwrap();
            // One, two and three query rows per batch row (one row takes another code path on
            // Metal: the fused single-query kernel ignores masks).
            for rows in [vec![5usize], vec![0, 22], vec![1, 9, 30]] {
                let n = rows.len();
                let ids: Vec<u32> = rows.iter().map(|&r| r as u32).collect();
                let idx = Tensor::from_vec(ids, n, &dev).unwrap();
                let q = qk
                    .narrow(2, 0, d)
                    .unwrap()
                    .contiguous()
                    .unwrap()
                    .index_select(&idx, 1)
                    .unwrap();
                let mask = masks.global.narrow(2, 0, n).unwrap();
                let got = attention(&q, &k, &v, heads, hd, &mask, 0.125).unwrap();
                let want = full.index_select(&idx, 1).unwrap();
                assert_eq!(got.dims(), want.dims());
                let tol = if dtype == DType::F32 { 1e-5 } else { 2e-3 };
                let diff = max_abs_diff(&got, &want);
                assert!(diff <= tol, "{dev:?} rows {rows:?}: max |d| {diff}");
            }
        }
    }
}
