//! Metal replacements for three candle ops on the scorer's hot path, bit-identical to them.
//!
//! - [`rope_thd`]: candle's kernel indexes with 64-bit division per element, which made the
//!   rotary embedding about 9% of a forward pass. Same arithmetic, 32-bit grid indexing.
//! - [`add_bias`]: a linear layer's bias goes through candle's strided broadcast kernel (64-bit
//!   `%` per dimension per element). Same `x + b`, one 2-D grid.
//! - [`sdpa_rows`]: candle's fused attention writes `(b, h, l, hd)`, which then needs a
//!   `b * h`-blit transpose. The same kernel takes output strides, so it writes the
//!   `(b, l, h, hd)` layout the output projection reads, directly.
//!
//! The kernels are compiled once per device with Metal's default options, which are the fast
//! math settings candle compiles its own kernels with; the tests check bit equality against
//! candle's ops. If the library or one of its kernels cannot be built on this OS (the `bfloat`
//! kernels need MSL 3.1, macOS 14), the failure is remembered and [`rope_thd`] and [`add_bias`]
//! use candle's op instead: slower, never wrong, and never retried per call.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use candle_core::backend::BackendStorage;
use candle_core::metal_backend::DeviceId;
use candle_core::{
    CpuStorage, CustomOp2, CustomOp3, DType, Layout, MetalDevice, MetalStorage, Result, Shape,
    Tensor,
};
use candle_metal_kernels::metal::{ComputeCommandEncoder, ComputePipeline, Library};
use candle_metal_kernels::{SdpaDType, call_sdpa_full};
use objc2_metal::MTLSize;

const SOURCE: &str = r#"
#include <metal_stdlib>
using namespace metal;

// Rotate-half rotary embedding over a contiguous (b, t, h, d) tensor with (t, d / 2) tables.
// One thread per (i_d < d / 2, head, b * t row); arithmetic as candle's `rope_thd`.
template<typename T>
METAL_FUNC void rope_thd(
    constant uint &t, constant uint &h, constant uint &d, constant uint &bt,
    device const T *src, device const T *cos, device const T *sin, device T *dst, uint3 gid
) {
    const uint half_d = d / 2;
    if (gid.x >= half_d || gid.y >= h || gid.z >= bt) {
        return;
    }
    const uint i_d = gid.x;
    const uint i_t = gid.z % t;
    const uint i1 = (gid.z * h + gid.y) * d + i_d;
    const uint i2 = i1 + half_d;
    const uint i_cs = i_t * half_d + i_d;
    T c = cos[i_cs];
    T s = sin[i_cs];
    dst[i1] = src[i1] * c - src[i2] * s;
    dst[i2] = src[i1] * s + src[i2] * c;
}

// `out = x + bias` over contiguous (rows, cols) with a (cols) bias; arithmetic as candle's add.
template<typename T>
METAL_FUNC void add_bias(
    constant uint &cols, constant uint &rows,
    device const T *x, device const T *bias, device T *out, uint2 gid
) {
    if (gid.x >= cols || gid.y >= rows) {
        return;
    }
    const uint i = gid.y * cols + gid.x;
    out[i] = x[i] + bias[gid.x];
}

#define LAYA_KERNELS(SUFFIX, T) \
kernel void laya_rope_thd_##SUFFIX( \
    constant uint &t [[buffer(0)]], constant uint &h [[buffer(1)]], \
    constant uint &d [[buffer(2)]], constant uint &bt [[buffer(3)]], \
    device const T *src [[buffer(4)]], device const T *cos [[buffer(5)]], \
    device const T *sin [[buffer(6)]], device T *dst [[buffer(7)]], \
    uint3 gid [[thread_position_in_grid]] \
) { rope_thd<T>(t, h, d, bt, src, cos, sin, dst, gid); } \
kernel void laya_add_bias_##SUFFIX( \
    constant uint &cols [[buffer(0)]], constant uint &rows [[buffer(1)]], \
    device const T *x [[buffer(2)]], device const T *bias [[buffer(3)]], \
    device T *out [[buffer(4)]], uint2 gid [[thread_position_in_grid]] \
) { add_bias<T>(cols, rows, x, bias, out, gid); }

LAYA_KERNELS(f32, float)
LAYA_KERNELS(f16, half)
#if defined(__HAVE_BFLOAT__)
LAYA_KERNELS(bf16, bfloat)
#endif
"#;

fn wrap(e: candle_metal_kernels::MetalKernelError) -> candle_core::Error {
    candle_core::Error::wrap(e)
}

/// Compiled libraries and pipelines per device and source; a failure is stored as `None` so it
/// is attempted once, not on every call.
#[derive(Default)]
struct Registry {
    libraries: HashMap<(DeviceId, usize), Option<Library>>,
    pipelines: HashMap<(DeviceId, usize, &'static str), Option<ComputePipeline>>,
    /// Library compiles per (device, source), for the tests.
    #[cfg(test)]
    compiles: HashMap<(DeviceId, usize), usize>,
}

fn registry() -> std::sync::MutexGuard<'static, Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    // A panic while holding the lock leaves the maps consistent (inserts are single calls).
    REGISTRY
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Compute pipeline `name` from `source` on `device`, built once per (device, source, name).
/// `None` when the library or the function cannot be built here (logged once).
fn pipeline(
    device: &MetalDevice,
    source: &'static str,
    name: &'static str,
) -> Option<ComputePipeline> {
    let lib_key = (device.id(), source.as_ptr() as usize);
    let key = (lib_key.0, lib_key.1, name);
    let mut guard = registry();
    let reg = &mut *guard;
    if let Some(p) = reg.pipelines.get(&key) {
        return p.clone();
    }
    let library = reg.libraries.entry(lib_key).or_insert_with(|| {
        #[cfg(test)]
        {
            *reg.compiles.entry(lib_key).or_default() += 1;
        }
        match device.device().new_library_with_source(source, None) {
            Ok(l) => Some(l),
            Err(e) => {
                tracing::warn!(error = %e, "laya metal kernels did not compile; using candle's ops");
                None
            }
        }
    });
    let built = library.as_ref().and_then(|lib| {
        let f = lib.get_function(name, None).ok()?;
        match device.device().new_compute_pipeline_state_with_function(&f) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!(kernel = name, error = %e, "laya metal kernel unavailable; using candle's op");
                None
            }
        }
    });
    reg.pipelines.insert(key, built.clone());
    built
}

/// How many times `source` was compiled on `device` (tests: failures must be cached).
#[cfg(test)]
fn compile_attempts(device: &MetalDevice, source: &'static str) -> usize {
    let lib_key = (device.id(), source.as_ptr() as usize);
    registry().compiles.get(&lib_key).copied().unwrap_or(0)
}

/// The Metal device of `t`, if it lives on one.
fn metal_device(t: &Tensor) -> Option<MetalDevice> {
    match t.device() {
        candle_core::Device::Metal(m) => Some(m.clone()),
        _ => None,
    }
}

/// The crate's pipeline for `names[dtype]` on `t`'s device, if it is available.
fn pipeline_for(
    t: &Tensor,
    source: &'static str,
    names: [&'static str; 3],
) -> Option<ComputePipeline> {
    let name = kernel_for(names, t.dtype()).ok()?;
    pipeline(&metal_device(t)?, source, name)
}

/// Kernel names per dtype: `[f32, f16, bf16]`.
fn kernel_for(names: [&'static str; 3], dtype: DType) -> Result<&'static str> {
    Ok(match dtype {
        DType::F32 => names[0],
        DType::F16 => names[1],
        DType::BF16 => names[2],
        other => candle_core::bail!("laya metal kernels do not support {other:?}"),
    })
}

const ROPE_KERNELS: [&str; 3] = [
    "laya_rope_thd_f32",
    "laya_rope_thd_f16",
    "laya_rope_thd_bf16",
];
const BIAS_KERNELS: [&str; 3] = [
    "laya_add_bias_f32",
    "laya_add_bias_f16",
    "laya_add_bias_bf16",
];

/// Fits a 32-bit index, as the kernels use.
fn check_u32(n: usize, what: &str) -> Result<u32> {
    u32::try_from(n).map_err(|_| candle_core::Error::Msg(format!("{what}: {n} exceeds u32")))
}

fn no_cpu<T>() -> Result<T> {
    candle_core::bail!("laya metal op called on a cpu tensor")
}

struct RopeThd {
    pipeline: ComputePipeline,
}

impl CustomOp3 for RopeThd {
    fn name(&self) -> &'static str {
        "laya-rope-thd"
    }

    fn cpu_fwd(
        &self,
        _: &CpuStorage,
        _: &Layout,
        _: &CpuStorage,
        _: &Layout,
        _: &CpuStorage,
        _: &Layout,
    ) -> Result<(CpuStorage, Shape)> {
        no_cpu()
    }

    fn metal_fwd(
        &self,
        src: &MetalStorage,
        l_src: &Layout,
        cos: &MetalStorage,
        l_cos: &Layout,
        sin: &MetalStorage,
        l_sin: &Layout,
    ) -> Result<(MetalStorage, Shape)> {
        let dtype = src.dtype();
        if cos.dtype() != dtype || sin.dtype() != dtype {
            candle_core::bail!("laya rope: dtype mismatch");
        }
        let (b, t, h, d) = l_src.shape().dims4()?;
        if l_cos.dims() != [t, d / 2] || l_sin.dims() != [t, d / 2] || d % 2 != 0 {
            candle_core::bail!(
                "laya rope: tables {:?} for x {:?}",
                l_cos.dims(),
                l_src.dims()
            );
        }
        let el = b * t * h * d;
        check_u32(el, "laya rope elements")?;
        let device = src.device();
        let output = device
            .new_buffer_builder()
            .with_size_for(el, dtype)
            .with_label("laya_rope_thd")
            .build()?;
        let size = dtype.size_in_bytes();
        let guard = device.command_encoder()?;
        let enc: &ComputeCommandEncoder = guard.as_ref();
        enc.set_compute_pipeline_state(&self.pipeline);
        enc.set_bytes(0, &(t as u32));
        enc.set_bytes(1, &(h as u32));
        enc.set_bytes(2, &(d as u32));
        enc.set_bytes(3, &((b * t) as u32));
        enc.set_input_buffer(4, Some(src.buffer()), l_src.start_offset() * size);
        enc.set_input_buffer(5, Some(cos.buffer()), l_cos.start_offset() * size);
        enc.set_input_buffer(6, Some(sin.buffer()), l_sin.start_offset() * size);
        enc.set_output_buffer(7, Some(&output), 0);
        let width = (d / 2).clamp(1, 32);
        let height = h.clamp(1, (256 / width).max(1));
        enc.dispatch_threads(
            MTLSize {
                width: d / 2,
                height: h,
                depth: b * t,
            },
            MTLSize {
                width,
                height,
                depth: 1,
            },
        );
        Ok((
            MetalStorage::new(output, device.clone(), el, dtype),
            l_src.shape().clone(),
        ))
    }
}

struct AddBias {
    pipeline: ComputePipeline,
}

impl CustomOp2 for AddBias {
    fn name(&self) -> &'static str {
        "laya-add-bias"
    }

    fn cpu_fwd(
        &self,
        _: &CpuStorage,
        _: &Layout,
        _: &CpuStorage,
        _: &Layout,
    ) -> Result<(CpuStorage, Shape)> {
        no_cpu()
    }

    fn metal_fwd(
        &self,
        x: &MetalStorage,
        l_x: &Layout,
        bias: &MetalStorage,
        l_bias: &Layout,
    ) -> Result<(MetalStorage, Shape)> {
        let dtype = x.dtype();
        if bias.dtype() != dtype {
            candle_core::bail!("laya add_bias: dtype mismatch");
        }
        let cols = *l_x.dims().last().unwrap_or(&0);
        if l_bias.dims() != [cols] || cols == 0 {
            candle_core::bail!(
                "laya add_bias: bias {:?} for x {:?}",
                l_bias.dims(),
                l_x.dims()
            );
        }
        let el = l_x.shape().elem_count();
        let rows = el / cols;
        check_u32(el, "laya add_bias elements")?;
        let device = x.device();
        let output = device
            .new_buffer_builder()
            .with_size_for(el, dtype)
            .with_label("laya_add_bias")
            .build()?;
        let size = dtype.size_in_bytes();
        let guard = device.command_encoder()?;
        let enc: &ComputeCommandEncoder = guard.as_ref();
        enc.set_compute_pipeline_state(&self.pipeline);
        enc.set_bytes(0, &(cols as u32));
        enc.set_bytes(1, &(rows as u32));
        enc.set_input_buffer(2, Some(x.buffer()), l_x.start_offset() * size);
        enc.set_input_buffer(3, Some(bias.buffer()), l_bias.start_offset() * size);
        enc.set_output_buffer(4, Some(&output), 0);
        let width = cols.clamp(1, 64);
        let height = rows.clamp(1, (256 / width).max(1));
        enc.dispatch_threads(
            MTLSize {
                width: cols,
                height: rows,
                depth: 1,
            },
            MTLSize {
                width,
                height,
                depth: 1,
            },
        );
        Ok((
            MetalStorage::new(output, device.clone(), el, dtype),
            l_x.shape().clone(),
        ))
    }
}

/// candle's fused attention kernel with its output written as `(b, l, h, hd)`.
struct SdpaRows {
    mask: Tensor,
    scale: f32,
}

impl CustomOp3 for SdpaRows {
    fn name(&self) -> &'static str {
        "laya-sdpa-rows"
    }

    fn cpu_fwd(
        &self,
        _: &CpuStorage,
        _: &Layout,
        _: &CpuStorage,
        _: &Layout,
        _: &CpuStorage,
        _: &Layout,
    ) -> Result<(CpuStorage, Shape)> {
        no_cpu()
    }

    fn metal_fwd(
        &self,
        q: &MetalStorage,
        q_l: &Layout,
        k: &MetalStorage,
        k_l: &Layout,
        v: &MetalStorage,
        v_l: &Layout,
    ) -> Result<(MetalStorage, Shape)> {
        let (b, h, lq, hd) = q_l.shape().dims4()?;
        let (kb, kh, kl, khd) = k_l.shape().dims4()?;
        if (kb, kh, khd) != (b, h, hd) || v_l.dims() != [b, h, kl, hd] {
            candle_core::bail!(
                "laya sdpa_rows: q {:?} k {:?} v {:?}",
                q_l.dims(),
                k_l.dims(),
                v_l.dims()
            );
        }
        // The full kernel only: the single-query one ignores the mask.
        if lq < 2 || lq > kl || ![32, 64, 72, 80, 96, 128, 256].contains(&hd) {
            candle_core::bail!("laya sdpa_rows: unsupported lq {lq} / kl {kl} / head dim {hd}");
        }
        let dtype = q.dtype();
        let itype = match dtype {
            DType::F32 => SdpaDType::F32,
            DType::F16 => SdpaDType::F16,
            DType::BF16 => SdpaDType::BF16,
            other => candle_core::bail!("laya sdpa_rows: unsupported dtype {other:?}"),
        };
        if k.dtype() != dtype || v.dtype() != dtype || self.mask.dtype() != dtype {
            candle_core::bail!("laya sdpa_rows: dtype mismatch");
        }
        let (mask_s, mask_l) = self.mask.storage_and_layout();
        let candle_core::Storage::Metal(mask_s) = &*mask_s else {
            candle_core::bail!("laya sdpa_rows: mask is not on the metal device");
        };
        // The kernel binds the mask buffer without an offset.
        if mask_l.dims() != [b, h, lq, kl] || mask_l.start_offset() != 0 {
            candle_core::bail!(
                "laya sdpa_rows: mask {:?} at offset {}",
                mask_l.dims(),
                mask_l.start_offset()
            );
        }
        let el = b * lq * h * hd;
        let device = q.device();
        let output = device
            .new_buffer_builder()
            .with_size_for(el, dtype)
            .with_label("laya_sdpa_rows")
            .build()?;
        let size = dtype.size_in_bytes();
        let guard = device.command_encoder()?;
        guard.set_label("laya_sdpa_rows");
        call_sdpa_full(
            device.device(),
            &guard,
            device.kernels(),
            q_l.start_offset() * size,
            q_l.dims(),
            q_l.stride(),
            q.buffer(),
            k_l.start_offset() * size,
            k_l.dims(),
            k_l.stride(),
            k.buffer(),
            v_l.start_offset() * size,
            v.buffer(),
            v_l.stride(),
            Some(itype),
            Some(mask_s.buffer()),
            Some(mask_l.stride()),
            &output,
            // (batch, head, row) strides of a (b, lq, h, hd) output.
            &[lq * h * hd, hd, h * hd],
            self.scale,
            false,
            itype,
        )
        .map_err(wrap)?;
        Ok((
            MetalStorage::new(output, device.clone(), el, dtype),
            Shape::from_dims(&[b, lq, h, hd]),
        ))
    }
}

/// Rotate-half rotary embedding of a contiguous `(b, t, h, d)` tensor with `(t, d / 2)` cos/sin
/// tables (as `candle_nn::rotary_emb::rope_thd`, bit for bit; candle's op itself where the
/// kernel is unavailable).
pub fn rope_thd(x: &Tensor, cos: &Tensor, sin: &Tensor) -> Result<Tensor> {
    rope_thd_with(SOURCE, x, cos, sin)
}

fn rope_thd_with(source: &'static str, x: &Tensor, cos: &Tensor, sin: &Tensor) -> Result<Tensor> {
    if !x.is_contiguous() || !cos.is_contiguous() || !sin.is_contiguous() {
        candle_core::bail!("laya rope: inputs must be contiguous");
    }
    match pipeline_for(x, source, ROPE_KERNELS) {
        Some(pipeline) => x.apply_op3_no_bwd(cos, sin, &RopeThd { pipeline }),
        None => candle_nn::rotary_emb::rope_thd(x, cos, sin),
    }
}

/// `x + bias` over the last dim (as `x.broadcast_add(bias)`, bit for bit). A non-contiguous `x`,
/// or a device where the kernel is unavailable, takes candle's broadcast path.
pub fn add_bias(x: &Tensor, bias: &Tensor) -> Result<Tensor> {
    add_bias_with(SOURCE, x, bias)
}

fn add_bias_with(source: &'static str, x: &Tensor, bias: &Tensor) -> Result<Tensor> {
    if !x.is_contiguous() || !bias.is_contiguous() {
        return x.broadcast_add(bias);
    }
    match pipeline_for(x, source, BIAS_KERNELS) {
        Some(pipeline) => x.apply_op2_no_bwd(bias, &AddBias { pipeline }),
        None => x.broadcast_add(bias),
    }
}

/// `softmax(q k^T * scale + mask) v` with candle's fused kernel, written as `(b, lq, h, hd)`.
///
/// `q`: `(b, h, lq, hd)`, `k` and `v`: `(b, h, kl, hd)` (any strides), `mask`: additive
/// `(b, h, lq, kl)` (any strides, e.g. broadcast over heads). Needs `lq >= 2`.
pub fn sdpa_rows(q: &Tensor, k: &Tensor, v: &Tensor, mask: &Tensor, scale: f32) -> Result<Tensor> {
    let mask = if mask.layout().start_offset() == 0 {
        mask.clone()
    } else {
        mask.contiguous()?
    };
    q.apply_op3_no_bwd(k, v, &SdpaRows { mask, scale })
}

#[cfg(test)]
mod tests {
    use candle_core::Device;

    use super::*;
    use crate::nn::tests::{max_abs_diff, randn};

    fn metal() -> Option<Device> {
        candle_core::utils::metal_is_available().then(|| Device::new_metal(0).unwrap())
    }

    #[test]
    fn rope_thd_is_bit_identical_to_candles() {
        let Some(dev) = metal() else { return };
        for dtype in [DType::F16, DType::F32] {
            // (b, t, h, d) with an odd sequence length, as the packed q|k heads of the encoder.
            let (b, t, h, d) = (3usize, 37usize, 6usize, 64usize);
            let x = randn(&[b, t, h, d], &dev, dtype);
            let cos = randn(&[t, d / 2], &dev, dtype);
            let sin = randn(&[t, d / 2], &dev, dtype);
            let want = candle_nn::rotary_emb::rope_thd(&x, &cos, &sin).unwrap();
            assert!(
                pipeline_for(&x, SOURCE, ROPE_KERNELS).is_some(),
                "crate kernel, not fallback"
            );
            let got = rope_thd(&x, &cos, &sin).unwrap();
            assert_eq!(got.dims(), want.dims());
            assert_eq!(max_abs_diff(&got, &want), 0.0, "{dtype:?}");
        }
    }

    #[test]
    fn add_bias_is_bit_identical_to_broadcast_add() {
        let Some(dev) = metal() else { return };
        for dtype in [DType::F16, DType::F32] {
            for shape in [vec![5usize, 37, 96], vec![11, 96]] {
                let x = randn(&shape, &dev, dtype);
                let bias = randn(&[96], &dev, dtype);
                let want = x.broadcast_add(&bias).unwrap();
                assert!(pipeline_for(&x, SOURCE, BIAS_KERNELS).is_some());
                let got = add_bias(&x, &bias).unwrap();
                assert_eq!(got.dims(), want.dims());
                assert_eq!(max_abs_diff(&got, &want), 0.0, "{dtype:?} {shape:?}");
            }
        }
    }

    /// Metal source that does not compile, standing in for an OS whose Metal compiler rejects the
    /// crate's kernels (e.g. `bfloat` before MSL 3.1).
    const BROKEN_SOURCE: &str = "this is not metal";

    #[test]
    fn a_library_that_fails_to_compile_falls_back_to_candle_and_is_compiled_once() {
        let Some(dev) = metal() else { return };
        let (b, t, h, d) = (2usize, 9usize, 4usize, 64usize);
        let x = randn(&[b, t, h, d], &dev, DType::F16);
        let cos = randn(&[t, d / 2], &dev, DType::F16);
        let sin = randn(&[t, d / 2], &dev, DType::F16);
        let bias = randn(&[d], &dev, DType::F16);
        for _ in 0..3 {
            let want = candle_nn::rotary_emb::rope_thd(&x, &cos, &sin).unwrap();
            let got = rope_thd_with(BROKEN_SOURCE, &x, &cos, &sin).unwrap();
            assert_eq!(max_abs_diff(&got, &want), 0.0);
            let want = x.broadcast_add(&bias).unwrap();
            let got = add_bias_with(BROKEN_SOURCE, &x, &bias).unwrap();
            assert_eq!(max_abs_diff(&got, &want), 0.0);
        }
        let candle_core::Device::Metal(m) = &dev else {
            unreachable!()
        };
        assert_eq!(
            compile_attempts(m, BROKEN_SOURCE),
            1,
            "the failure is cached"
        );
        // A kernel missing from a library that compiled (bf16 before MSL 3.1) is unavailable too.
        assert!(pipeline(m, SOURCE, "laya_kernel_that_does_not_exist").is_none());
        assert!(pipeline(m, SOURCE, "laya_rope_thd_f16").is_some());
    }

    #[test]
    fn bf16_kernels_match_candle_where_the_device_supports_bf16() {
        let Some(dev) = metal() else { return };
        let (b, t, h, d) = (2usize, 9usize, 4usize, 64usize);
        let x = randn(&[b, t, h, d], &dev, DType::BF16);
        let cos = randn(&[t, d / 2], &dev, DType::BF16);
        let sin = randn(&[t, d / 2], &dev, DType::BF16);
        let Ok(want) = candle_nn::rotary_emb::rope_thd(&x, &cos, &sin) else {
            eprintln!("SKIP: candle has no bf16 rope on this device");
            return;
        };
        // candle's own bf16 kernels need the same MSL support, so ours must have been built too.
        assert!(pipeline_for(&x, SOURCE, ROPE_KERNELS).is_some());
        assert!(pipeline_for(&x, SOURCE, BIAS_KERNELS).is_some());
        assert_eq!(max_abs_diff(&rope_thd(&x, &cos, &sin).unwrap(), &want), 0.0);
        let bias = randn(&[d], &dev, DType::BF16);
        let want = x.broadcast_add(&bias).unwrap();
        assert_eq!(max_abs_diff(&add_bias(&x, &bias).unwrap(), &want), 0.0);
    }

    #[test]
    fn sdpa_rows_is_bit_identical_to_sdpa_then_transpose() {
        let Some(dev) = metal() else { return };
        let (b, s, h, hd) = (3usize, 37usize, 4usize, 64usize);
        // Strided views of a packed q|k projection, as the encoder hands them over.
        let qk = randn(&[b, s, 2 * h, hd], &dev, DType::F16);
        let v = randn(&[b, s, h, hd], &dev, DType::F16);
        let mask = randn(&[b, 1, s, s], &dev, DType::F16);
        for lq in [s, 2] {
            let q = qk.narrow(2, 0, h).unwrap().narrow(1, 0, lq).unwrap();
            let q = q.transpose(1, 2).unwrap();
            let k = qk.narrow(2, h, h).unwrap().transpose(1, 2).unwrap();
            let vv = v.transpose(1, 2).unwrap();
            let m = mask
                .narrow(2, 0, lq)
                .unwrap()
                .broadcast_as((b, h, lq, s))
                .unwrap();
            let want = candle_nn::ops::sdpa(&q, &k, &vv, Some(&m), false, 0.125, 1.0)
                .unwrap()
                .transpose(1, 2)
                .unwrap()
                .contiguous()
                .unwrap();
            let got = sdpa_rows(&q, &k, &vv, &m, 0.125).unwrap();
            assert_eq!(got.dims(), &[b, lq, h, hd]);
            assert!(got.is_contiguous());
            assert_eq!(max_abs_diff(&got, &want), 0.0, "lq {lq}");
        }
    }
}
