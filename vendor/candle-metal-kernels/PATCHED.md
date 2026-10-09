# candle-metal-kernels 0.11.0, patched for macOS before 15

This is candle-metal-kernels 0.11.0 from crates.io (https://github.com/huggingface/candle),
licensed MIT OR Apache-2.0, with one change in `src/metal/residency_set.rs`:

- `ResidencySet::new` returns an empty set when the `MTLResidencySetDescriptor` class is missing.
  Residency sets arrived in macOS 15. Unpatched, candle panics while opening any Metal device on
  macOS 14 and earlier, and laya-codex loses its model there and ranks with keywords only.
  candle already handles an empty set (`raw()` returns `None`, so nothing is added to the
  command queue).

The root `Cargo.toml` applies it with `[patch.crates-io]`. Drop this directory and that entry once a
candle release guards the class itself; candle is pinned to `=0.11.0` in `crates/laya-model`.
