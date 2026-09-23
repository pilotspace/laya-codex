//! Micro-profile of the tensor ops that dominate the encoder, to see what candle achieves
//! on this device (used to steer the performance work; not a benchmark of the model).
//!
//! `cargo run --release -p laya-model --features metal --example profile_ops`

use std::time::Instant;

use candle_core::{D, DType, Device, Tensor};
use candle_nn::Module;

fn timeit(
    dev: &Device,
    name: &str,
    flops: f64,
    iters: usize,
    mut f: impl FnMut() -> candle_core::Result<Tensor>,
) -> anyhow::Result<()> {
    let _ = f()?;
    dev.synchronize()?;
    let t = Instant::now();
    let mut last = None;
    for _ in 0..iters {
        last = Some(f()?);
    }
    dev.synchronize()?;
    drop(last);
    let dt = t.elapsed().as_secs_f64() / iters as f64;
    println!(
        "{name:<48} {:>8.2} ms  {:>7.2} TFLOPS",
        dt * 1e3,
        flops / dt / 1e12
    );
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let dev = if candle_core::utils::metal_is_available() {
        Device::new_metal(0)?
    } else {
        Device::Cpu
    };
    let dtype = if dev.is_cpu() { DType::F32 } else { DType::F16 };
    println!("device {:?} dtype {:?}", dev, dtype);
    let (b, s, d, h, hd, inter) = (16usize, 303usize, 1024usize, 16usize, 64usize, 2624usize);
    let n = b * s;
    let x = Tensor::randn(0f32, 1.0, (b, s, d), &dev)?.to_dtype(dtype)?;
    let x2 = x.reshape((n, d))?;
    let w_qkv = Tensor::randn(0f32, 0.02, (3 * d, d), &dev)?.to_dtype(dtype)?;
    let w_i = Tensor::randn(0f32, 0.02, (2 * inter, d), &dev)?.to_dtype(dtype)?;
    let w_o = Tensor::randn(0f32, 0.02, (d, inter), &dev)?.to_dtype(dtype)?;
    let iters = 10;

    // Linear via candle_nn::Linear (x @ w.t()).
    let lin = candle_nn::Linear::new(w_qkv.clone(), None);
    timeit(
        &dev,
        "Linear 3d (n,1024)x(1024,3072) w.t()",
        2.0 * n as f64 * d as f64 * 3.0 * d as f64,
        iters,
        || lin.forward(&x),
    )?;
    // Pre-transposed contiguous weight.
    let w_qkv_t = w_qkv.t()?.contiguous()?;
    timeit(
        &dev,
        "matmul (n,1024)x(1024,3072) contiguous",
        2.0 * n as f64 * d as f64 * 3.0 * d as f64,
        iters,
        || x2.matmul(&w_qkv_t),
    )?;
    let lin_i = candle_nn::Linear::new(w_i.clone(), None);
    timeit(
        &dev,
        "Linear Wi (n,1024)x(1024,5248)",
        2.0 * n as f64 * d as f64 * 2.0 * inter as f64,
        iters,
        || lin_i.forward(&x),
    )?;
    let w_i_t = w_i.t()?.contiguous()?;
    timeit(
        &dev,
        "matmul Wi contiguous",
        2.0 * n as f64 * d as f64 * 2.0 * inter as f64,
        iters,
        || x2.matmul(&w_i_t),
    )?;
    let hbig = Tensor::randn(0f32, 1.0, (n, inter), &dev)?.to_dtype(dtype)?;
    let lin_o = candle_nn::Linear::new(w_o.clone(), None);
    timeit(
        &dev,
        "Linear Wo (n,2624)x(2624,1024)",
        2.0 * n as f64 * inter as f64 * d as f64,
        iters,
        || lin_o.forward(&hbig),
    )?;

    // Attention pieces.
    let q = Tensor::randn(0f32, 1.0, (b, h, s, hd), &dev)?.to_dtype(dtype)?;
    let k = Tensor::randn(0f32, 1.0, (b, h, s, hd), &dev)?.to_dtype(dtype)?;
    let v = Tensor::randn(0f32, 1.0, (b, h, s, hd), &dev)?.to_dtype(dtype)?;
    let mask = Tensor::zeros((b, 1, s, s), dtype, &dev)?;
    let att_flops = 2.0 * (b * h) as f64 * s as f64 * s as f64 * hd as f64;
    timeit(&dev, "q @ k^T (contiguous k^T)", att_flops, iters, || {
        q.matmul(&k.transpose(D::Minus2, D::Minus1)?.contiguous()?)
    })?;
    timeit(&dev, "q @ k^T (strided k^T)", att_flops, iters, || {
        q.matmul(&k.transpose(D::Minus2, D::Minus1)?)
    })?;
    let att = q.matmul(&k.transpose(D::Minus2, D::Minus1)?.contiguous()?)?;
    timeit(&dev, "mask add + softmax_last_dim", 0.0, iters, || {
        candle_nn::ops::softmax_last_dim(&att.broadcast_add(&mask)?)
    })?;
    let p = candle_nn::ops::softmax_last_dim(&att)?;
    timeit(&dev, "p @ v", att_flops, iters, || p.matmul(&v))?;
    timeit(&dev, "transpose+reshape to (b,s,d)", 0.0, iters, || {
        q.transpose(1, 2)?.reshape((b, s, d))
    })?;
    timeit(
        &dev,
        "full manual attention",
        2.0 * att_flops,
        iters,
        || laya_model_profile::attention(&q, &k, &v, &mask, 0.125),
    )?;
    #[cfg(feature = "metal")]
    {
        let mask_full = mask.broadcast_as((b, h, s, s))?.contiguous()?;
        timeit(
            &dev,
            "sdpa fused (full mask)",
            2.0 * att_flops,
            iters,
            || candle_nn::ops::sdpa(&q, &k, &v, Some(&mask_full), false, 0.125, 1.0),
        )?;
        timeit(&dev, "sdpa fused (no mask)", 2.0 * att_flops, iters, || {
            candle_nn::ops::sdpa(&q, &k, &v, None, false, 0.125, 1.0)
        })?;
        // Does the kernel honour a stride-0 (broadcast) mask? Compare against the
        // materialized one using a real sliding-window band.
        let band = laya_model::nn_probe::window_band(s, 64, dtype, &dev)?; // (1,1,s,s)
        let band_full = band.broadcast_as((b, h, s, s))?.contiguous()?;
        let band_bcast = band.broadcast_as((b, h, s, s))?;
        let ref_out = candle_nn::ops::sdpa(&q, &k, &v, Some(&band_full), false, 0.125, 1.0)?;
        let bc_out = candle_nn::ops::sdpa(&q, &k, &v, Some(&band_bcast), false, 0.125, 1.0)?;
        let manual = laya_model_profile::attention(&q, &k, &v, &band, 0.125)?
            .reshape((b, s, h, hd))?
            .transpose(1, 2)?
            .contiguous()?;
        let diff = |a: &Tensor, b: &Tensor| -> anyhow::Result<f32> {
            Ok((a.to_dtype(DType::F32)? - b.to_dtype(DType::F32)?)?
                .abs()?
                .flatten_all()?
                .max(0)?
                .to_scalar::<f32>()?)
        };
        println!(
            "sdpa(full mask) vs manual: max|d| = {:.3e}",
            diff(&ref_out, &manual)?
        );
        println!(
            "sdpa(bcast mask) vs full: max|d| = {:.3e}",
            diff(&bc_out, &ref_out)?
        );
        timeit(
            &dev,
            "sdpa fused (broadcast band mask)",
            2.0 * att_flops,
            iters,
            || candle_nn::ops::sdpa(&q, &k, &v, Some(&band_bcast), false, 0.125, 1.0),
        )?;
        let mask_b1 = band.broadcast_as((b, 1, s, s))?.contiguous()?;
        let mask_b1h = mask_b1.broadcast_as((b, h, s, s))?;
        let b1_out = candle_nn::ops::sdpa(&q, &k, &v, Some(&mask_b1h), false, 0.125, 1.0)?;
        println!(
            "sdpa((b,1,s,s) bcast heads) vs full: max|d| = {:.3e}",
            diff(&b1_out, &ref_out)?
        );
        timeit(
            &dev,
            "sdpa fused ((b,1,s,s) bcast heads)",
            2.0 * att_flops,
            iters,
            || candle_nn::ops::sdpa(&q, &k, &v, Some(&mask_b1h), false, 0.125, 1.0),
        )?;
        timeit(&dev, "materialize (b,h,s,s) mask", 0.0, iters, || {
            band.broadcast_as((b, h, s, s))?.contiguous()
        })?;

        // ---- copy-free layouts: rope in (b,s,h,hd) layout, strided views into sdpa,
        // ---- and cat-over-heads instead of a generic 4-D transpose copy.
        let qkv = Tensor::randn(0f32, 1.0, (b, s, 3 * d), &dev)?.to_dtype(dtype)?;
        timeit(
            &dev,
            "narrow(2)+contiguous (b,s,d) [copy2d]",
            0.0,
            iters,
            || qkv.narrow(2, 0, d)?.contiguous(),
        )?;
        timeit(
            &dev,
            "split_qkv reference (3 transposes)",
            0.0,
            iters,
            || {
                let part = |i: usize| -> candle_core::Result<Tensor> {
                    qkv.narrow(2, i * d, d)?
                        .reshape((b, s, h, hd))?
                        .transpose(1, 2)?
                        .contiguous()
                };
                let _ = part(0)?;
                let _ = part(1)?;
                part(2)
            },
        )?;
        let cos_r = Tensor::randn(0f32, 1.0, (s, hd / 2), &dev)?.to_dtype(dtype)?;
        let sin_r = Tensor::randn(0f32, 1.0, (s, hd / 2), &dev)?.to_dtype(dtype)?;
        let q_bshd = qkv.narrow(2, 0, d)?.contiguous()?.reshape((b, s, h, hd))?;
        let q_ref =
            candle_nn::rotary_emb::rope(&q_bshd.transpose(1, 2)?.contiguous()?, &cos_r, &sin_r)?;
        let q_thd = candle_nn::rotary_emb::rope_thd(&q_bshd, &cos_r, &sin_r)?;
        println!(
            "rope_thd(b,s,h,hd) vs rope(b,h,s,hd): max|d| = {:.3e}",
            diff(&q_thd.transpose(1, 2)?.contiguous()?, &q_ref)?
        );
        timeit(&dev, "rope_thd (b,s,h,hd)", 0.0, iters, || {
            candle_nn::rotary_emb::rope_thd(&q_bshd, &cos_r, &sin_r)
        })?;
        // sdpa with strided (transposed-view) q/k/v.
        let k_bshd = qkv.narrow(2, d, d)?.contiguous()?.reshape((b, s, h, hd))?;
        let v_bshd = qkv
            .narrow(2, 2 * d, d)?
            .contiguous()?
            .reshape((b, s, h, hd))?;
        let (qv, kv, vv) = (
            q_bshd.transpose(1, 2)?,
            k_bshd.transpose(1, 2)?,
            v_bshd.transpose(1, 2)?,
        );
        let ref_c = candle_nn::ops::sdpa(
            &qv.contiguous()?,
            &kv.contiguous()?,
            &vv.contiguous()?,
            Some(&mask_b1h),
            false,
            0.125,
            1.0,
        )?;
        match candle_nn::ops::sdpa(&qv, &kv, &vv, Some(&mask_b1h), false, 0.125, 1.0) {
            Ok(strided) => {
                println!(
                    "sdpa(strided views) vs contiguous: max|d| = {:.3e}",
                    diff(&strided, &ref_c)?
                );
                timeit(
                    &dev,
                    "sdpa fused (strided q/k/v views)",
                    2.0 * att_flops,
                    iters,
                    || candle_nn::ops::sdpa(&qv, &kv, &vv, Some(&mask_b1h), false, 0.125, 1.0),
                )?;
            }
            Err(e) => println!("sdpa(strided views) rejected: {e}"),
        }
        // Output (b,h,s,hd) -> (b,s,h*hd): generic transpose copy vs cat over heads.
        let out = ref_c.clone();
        let t_ref = out.transpose(1, 2)?.reshape((b, s, d))?;
        let heads: Vec<Tensor> = (0..h)
            .map(|i| out.narrow(1, i, 1)?.squeeze(1))
            .collect::<candle_core::Result<_>>()?;
        let t_cat = Tensor::cat(&heads, 2)?;
        println!(
            "cat-over-heads vs transpose: max|d| = {:.3e}",
            diff(&t_cat, &t_ref)?
        );
        timeit(
            &dev,
            "out transpose+reshape [generic copy]",
            0.0,
            iters,
            || out.transpose(1, 2)?.reshape((b, s, d)),
        )?;
        timeit(&dev, "out cat over heads [copy2d x h]", 0.0, iters, || {
            let heads: Vec<Tensor> = (0..h)
                .map(|i| out.narrow(1, i, 1)?.squeeze(1))
                .collect::<candle_core::Result<_>>()?;
            Tensor::cat(&heads, 2)
        })?;
        // Is the generic copy faster in f32?
        let out32 = out.to_dtype(DType::F32)?;
        timeit(&dev, "out transpose+reshape f32", 0.0, iters, || {
            out32.transpose(1, 2)?.reshape((b, s, d))
        })?;
        // B: b*h `slice_set` blits into a preallocated (b, s, h, hd) buffer.
        let heads_to_rows = |out: &Tensor| -> candle_core::Result<Tensor> {
            let dst = Tensor::zeros((b, s, h, hd), out.dtype(), out.device())?;
            for bi in 0..b {
                let dst_b = dst.narrow(0, bi, 1)?;
                let out_b = out.narrow(0, bi, 1)?;
                for i in 0..h {
                    let src = out_b.narrow(1, i, 1)?.reshape((1, s, 1, hd))?;
                    dst_b.slice_set(&src, 2, i)?;
                }
            }
            dst.reshape((b, s, d))
        };
        println!(
            "slice_set assembly vs transpose: max|d| = {:.3e}",
            diff(&heads_to_rows(&out)?, &t_ref)?
        );
        timeit(
            &dev,
            "out slice_set per (b,h) [copy2d x b*h]",
            0.0,
            iters,
            || heads_to_rows(&out),
        )?;
        let wo = Tensor::randn(0f32, 0.02, (d, d), &dev)?.to_dtype(dtype)?;
        let wo_t = wo.t()?.contiguous()?; // (d_in, d_out)
        timeit(
            &dev,
            "Wo via transpose copy + matmul",
            2.0 * n as f64 * d as f64 * d as f64,
            iters,
            || out.transpose(1, 2)?.reshape((n, d))?.matmul(&wo_t),
        )?;
        timeit(
            &dev,
            "Wo via slice_set + matmul",
            2.0 * n as f64 * d as f64 * d as f64,
            iters,
            || heads_to_rows(&out)?.reshape((n, d))?.matmul(&wo_t),
        )?;
    }
    // Elementwise.
    let big = Tensor::randn(0f32, 1.0, (n, 2 * inter), &dev)?.to_dtype(dtype)?;
    timeit(&dev, "gelu_erf * gate (n,2624)", 0.0, iters, || {
        big.narrow(1, 0, inter)?.gelu_erf()? * big.narrow(1, inter, inter)?
    })?;
    timeit(
        &dev,
        "gelu_erf * gate contiguous chunks",
        0.0,
        iters,
        || {
            big.narrow(1, 0, inter)?.contiguous()?.gelu_erf()?
                * big.narrow(1, inter, inter)?.contiguous()?
        },
    )?;
    timeit(&dev, "chunk contiguous only", 0.0, iters, || {
        big.narrow(1, 0, inter)?.contiguous()
    })?;
    timeit(&dev, "gelu_erf contiguous only", 0.0, iters, || {
        hbig.gelu_erf()
    })?;
    timeit(&dev, "mul contiguous only", 0.0, iters, || &hbig * &hbig)?;
    let wln = Tensor::ones(d, dtype, &dev)?;
    let bln = Tensor::zeros(d, dtype, &dev)?;
    timeit(&dev, "layer_norm (n,1024)", 0.0, iters, || {
        candle_nn::ops::layer_norm(&x2, &wln, &bln, 1e-5)
    })?;
    let cos = Tensor::randn(0f32, 1.0, (s, hd / 2), &dev)?.to_dtype(dtype)?;
    let sin = cos.clone();
    timeit(&dev, "rope (b,h,s,hd)", 0.0, iters, || {
        candle_nn::rotary_emb::rope(&q, &cos, &sin)
    })?;
    Ok(())
}

mod laya_model_profile {
    use candle_core::{D, Result, Tensor};
    pub fn attention(
        q: &Tensor,
        k: &Tensor,
        v: &Tensor,
        mask: &Tensor,
        scale: f64,
    ) -> Result<Tensor> {
        let (b, h, s, hd) = q.dims4()?;
        let att = (q.matmul(&k.transpose(D::Minus2, D::Minus1)?.contiguous()?)? * scale)?;
        let att = att.broadcast_add(mask)?;
        let att = candle_nn::ops::softmax_last_dim(&att)?;
        let out = att.matmul(v)?;
        out.transpose(1, 2)?.reshape((b, s, h * hd))
    }
}
