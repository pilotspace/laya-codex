//! Opening a Metal device must not panic on any macOS laya-codex supports.
//!
//! candle 0.11 creates a residency set with every Metal device, and residency sets arrived in
//! macOS 15: unpatched, opening a device panicked on macOS 14 and earlier, and the daemon lost its
//! model there. CI's macos-14 runner exercises this; on a machine without Metal the open returns an
//! error, which is fine.
#![cfg(all(target_os = "macos", feature = "metal"))]

#[test]
fn opening_a_metal_device_never_panics() {
    match candle_core::Device::new_metal(0) {
        Ok(device) => {
            let t = candle_core::Tensor::new(&[1f32, 2.0, 3.0], &device).expect("upload");
            let sum: f32 = t
                .sum_all()
                .and_then(|s| s.to_scalar())
                .expect("run a kernel");
            assert_eq!(sum, 6.0);
        }
        Err(e) => eprintln!("no Metal device here ({e}); nothing to check"),
    }
}
