//! Metal replacements for a candle op on the scorer's hot path, bit-identical to it.
//!
//! - [`rope_thd`]: candle's kernel indexes with 64-bit division per element, which made the
//!   rotary embedding about 9% of a forward pass. Same arithmetic, 32-bit grid indexing.
//!
//! The kernels are compiled once per device with Metal's default options, which are the fast
//! math settings candle compiles its own kernels with; the tests check bit equality against
//! candle's ops.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use candle_core::backend::BackendStorage;
use candle_core::metal_backend::DeviceId;
use candle_core::{
    CpuStorage, CustomOp3, DType, Layout, MetalDevice, MetalStorage, Result, Shape, Tensor,
};
use candle_metal_kernels::metal::{ComputeCommandEncoder, ComputePipeline, Library};
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

#define LAYA_KERNELS(SUFFIX, T) \
kernel void laya_rope_thd_##SUFFIX( \
    constant uint &t [[buffer(0)]], constant uint &h [[buffer(1)]], \
    constant uint &d [[buffer(2)]], constant uint &bt [[buffer(3)]], \
    device const T *src [[buffer(4)]], device const T *cos [[buffer(5)]], \
    device const T *sin [[buffer(6)]], device T *dst [[buffer(7)]], \
    uint3 gid [[thread_position_in_grid]] \
) { rope_thd<T>(t, h, d, bt, src, cos, sin, dst, gid); }

LAYA_KERNELS(f32, float)
LAYA_KERNELS(f16, half)
LAYA_KERNELS(bf16, bfloat)
"#;

fn wrap(e: candle_metal_kernels::MetalKernelError) -> candle_core::Error {
    candle_core::Error::wrap(e)
}

/// Compute pipeline `name` from [`SOURCE`] on `device`, compiled once per device.
fn pipeline(device: &MetalDevice, name: &'static str) -> Result<ComputePipeline> {
    type Cache = (
        HashMap<DeviceId, Library>,
        HashMap<(DeviceId, &'static str), ComputePipeline>,
    );
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| candle_core::Error::Msg("laya metal pipeline cache poisoned".into()))?;
    let (libraries, pipelines) = &mut *cache;
    if let Some(p) = pipelines.get(&(device.id(), name)) {
        return Ok(p.clone());
    }
    let library = match libraries.get(&device.id()) {
        Some(l) => l.clone(),
        None => {
            let l = device
                .device()
                .new_library_with_source(SOURCE, None)
                .map_err(wrap)?;
            libraries.insert(device.id(), l.clone());
            l
        }
    };
    let function = library.get_function(name, None).map_err(wrap)?;
    let p = device
        .device()
        .new_compute_pipeline_state_with_function(&function)
        .map_err(wrap)?;
    pipelines.insert((device.id(), name), p.clone());
    Ok(p)
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

/// Fits a 32-bit index, as the kernels use.
fn check_u32(n: usize, what: &str) -> Result<u32> {
    u32::try_from(n).map_err(|_| candle_core::Error::Msg(format!("{what}: {n} exceeds u32")))
}

fn no_cpu<T>() -> Result<T> {
    candle_core::bail!("laya metal op called on a cpu tensor")
}

struct RopeThd;

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
        let p = pipeline(device, kernel_for(ROPE_KERNELS, dtype)?)?;
        let output = device
            .new_buffer_builder()
            .with_size_for(el, dtype)
            .with_label("laya_rope_thd")
            .build()?;
        let size = dtype.size_in_bytes();
        let guard = device.command_encoder()?;
        let enc: &ComputeCommandEncoder = guard.as_ref();
        enc.set_compute_pipeline_state(&p);
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

/// Rotate-half rotary embedding of a contiguous `(b, t, h, d)` tensor with `(t, d / 2)` cos/sin
/// tables (as `candle_nn::rotary_emb::rope_thd`, bit for bit).
pub fn rope_thd(x: &Tensor, cos: &Tensor, sin: &Tensor) -> Result<Tensor> {
    if !x.is_contiguous() || !cos.is_contiguous() || !sin.is_contiguous() {
        candle_core::bail!("laya rope: inputs must be contiguous");
    }
    x.apply_op3_no_bwd(cos, sin, &RopeThd)
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
            let got = rope_thd(&x, &cos, &sin).unwrap();
            assert_eq!(got.dims(), want.dims());
            assert_eq!(max_abs_diff(&got, &want), 0.0, "{dtype:?}");
        }
    }
}
