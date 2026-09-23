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
        let mask = mask.broadcast_as((b, heads, s, s))?;
        let out = candle_nn::ops::sdpa(&q, &k, &v, Some(&mask), false, scale as f32, 1.0)?;
        heads_to_rows(&out)
    } else {
        let to_bhsd = |t: &Tensor, offset: usize| -> Result<Tensor> {
            t.narrow(2, offset, d)?
                .reshape((b, s, heads, head_dim))?
                .transpose(1, 2)?
                .contiguous()
        };
        let (mut q, mut k, v) = (to_bhsd(qk, 0)?, to_bhsd(qk, d)?, to_bhsd(v, 0)?);
        if let Some(r) = rope {
            q = candle_nn::rotary_emb::rope(&q, r.cos, r.sin)?;
            k = candle_nn::rotary_emb::rope(&k, r.cos, r.sin)?;
        }
        let att = (q.matmul(&k.transpose(D::Minus2, D::Minus1)?.contiguous()?)? * scale)?;
        let att = att.broadcast_add(mask)?;
        let att = candle_nn::ops::softmax_last_dim(&att)?;
        att.matmul(&v)?.transpose(1, 2)?.reshape((b, s, d))
    }
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
