//! Where one hook-sized forward pass spends its time, for any Laya model dir.
//!
//! Part 1 (`e2e`): wall-clock of `noul_ids_within` exactly as the hook calls it (in-order
//! micro-batches of 8), plus single batches of 8, 4 and 12 rows, after a warm-up.
//! Part 2 (`ops`): every op of one encoder layer and one head layer at the model's real shapes,
//! each timed in a pipelined loop (one sync per loop, as in a real forward), multiplied by the
//! number of times it runs per forward. The sum is compared with the measured batch time; the
//! difference is what the per-op view cannot see (dispatch gaps, host syncs).
//!
//! ```text
//! cargo run --release -p laya-model --features metal --example profile_forward -- \
//!     --model ~/.cache/laya-codex/models/laya-code-student-nokd-best-w128 --window 128 --k 12
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use candle_core::{DType, Device, Tensor};
use candle_nn::{Linear, Module};
use laya_model::{DeviceKind, EncoderConfig, LayaModel, ModelFiles, Question};

/// A held-out task with a typical question prefix (about 55 tokens with specials).
const TASK: &str = "Fixes #2666: None is the default value of file for httpx.NetRCAuth";

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn corpus(dir: &std::path::Path) -> anyhow::Result<String> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d)? {
            let p = e?.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                files.push(p);
            }
        }
    }
    files.sort();
    let mut text = String::new();
    for f in files {
        text.push_str(&std::fs::read_to_string(&f)?);
        text.push('\n');
    }
    Ok(text)
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() as f64 * p) as usize).min(sorted.len() - 1)]
}

fn summary(mut t: Vec<f64>) -> String {
    t.sort_by(f64::total_cmp);
    format!(
        "p50 {:6.1} ms  p90 {:6.1} ms  min {:6.1} ms",
        pct(&t, 0.5),
        pct(&t, 0.9),
        t[0]
    )
}

fn e2e(model: &LayaModel, rows: &[Vec<u32>], question: &str, iters: usize) -> anyhow::Result<()> {
    let far = || Instant::now() + Duration::from_secs(60);
    let k = rows.len();
    let mut cases: Vec<(String, Vec<f64>)> = Vec::new();
    let names = [
        format!("hook path K={k} (in-order batches of 8)"),
        "one batch of 8".to_string(),
        "one batch of 4".to_string(),
        format!("one batch of {k}"),
    ];
    for n in &names {
        cases.push((n.clone(), Vec::new()));
    }
    for _ in 0..iters {
        let t = Instant::now();
        model.noul_ids_within(question, rows, far())?;
        cases[0].1.push(t.elapsed().as_secs_f64() * 1e3);
        let t = Instant::now();
        model.noul_ids_within(question, &rows[..8.min(k)], far())?;
        cases[1].1.push(t.elapsed().as_secs_f64() * 1e3);
        let t = Instant::now();
        model.noul_ids_within(question, &rows[..4.min(k)], far())?;
        cases[2].1.push(t.elapsed().as_secs_f64() * 1e3);
        let t = Instant::now();
        model.noul_ids(question, rows, None)?;
        cases[3].1.push(t.elapsed().as_secs_f64() * 1e3);
    }
    for (n, t) in cases {
        println!("{n:<44} {}", summary(t));
    }
    Ok(())
}

/// Times `f` pipelined: `iters` calls, one device sync at the end. Returns ms per call.
fn timeit(
    dev: &Device,
    iters: usize,
    mut f: impl FnMut() -> candle_core::Result<Tensor>,
) -> anyhow::Result<f64> {
    for _ in 0..3 {
        f()?;
    }
    dev.synchronize()?;
    let mut best = f64::INFINITY;
    for _ in 0..5 {
        let t = Instant::now();
        let mut last = None;
        for _ in 0..iters {
            last = Some(f()?);
        }
        dev.synchronize()?;
        drop(last);
        best = best.min(t.elapsed().as_secs_f64() * 1e3 / iters as f64);
    }
    Ok(best)
}

fn heads_to_rows(out: &Tensor) -> candle_core::Result<Tensor> {
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

#[cfg_attr(not(feature = "metal"), allow(unused_variables))]
fn ops(dev: &Device, cfg: &EncoderConfig, b: usize, s: usize, batch_ms: f64) -> anyhow::Result<()> {
    let dtype = if dev.is_cpu() { DType::F32 } else { DType::F16 };
    let (d, inter, h, hd) = (
        cfg.hidden_size,
        cfg.intermediate_size,
        cfg.num_attention_heads,
        cfg.head_dim(),
    );
    let layers = cfg.num_hidden_layers;
    let head_layers = 2usize;
    let hh = (d / 64).max(1); // head attention heads
    let rnd = |shape: &[usize], std: f32| -> candle_core::Result<Tensor> {
        Tensor::randn(0f32, std, shape, dev)?.to_dtype(dtype)
    };
    let x = rnd(&[b, s, d], 1.0)?;
    let lin = |o: usize, i: usize, bias: bool| -> candle_core::Result<Linear> {
        Ok(Linear::new(
            rnd(&[o, i], 0.02)?,
            if bias { Some(rnd(&[o], 0.02)?) } else { None },
        ))
    };
    let w_qk = lin(2 * d, d, false)?;
    let w_v = lin(d, d, false)?;
    let w_o = lin(d, d, false)?;
    let w_i = lin(inter, d, false)?;
    let w_mo = lin(d, inter, false)?;
    let h_in = lin(2 * d, d, true)?;
    let h_v = lin(d, d, true)?;
    let h_o = lin(d, d, true)?;
    let h_1 = lin(4 * d, d, true)?;
    let h_2 = lin(d, 4 * d, true)?;
    let ln_w = Tensor::ones(d, dtype, dev)?;
    let ln_b = Tensor::zeros(d, dtype, dev)?;
    let hid = rnd(&[b, s, inter], 1.0)?;
    let hid4 = rnd(&[b, s, 4 * d], 1.0)?;
    let qk = rnd(&[b, s, 2 * d], 1.0)?;
    let v = rnd(&[b, s, d], 1.0)?;
    let cos = rnd(&[s, hd / 2], 1.0)?;
    let sin = rnd(&[s, hd / 2], 1.0)?;
    let mask = Tensor::zeros((b, 1, s, s), dtype, dev)?;
    let att_out = rnd(&[b, h, s, hd], 1.0)?;
    let iters = 50;

    let mut rows: Vec<(&str, f64, usize)> = Vec::new();
    let mut add = |name: &'static str, ms: f64, n: usize| rows.push((name, ms, n));
    add(
        "layer_norm (b,s,d)",
        timeit(dev, iters, || {
            candle_nn::ops::layer_norm(&x, &ln_w, &ln_b, 1e-5)
        })?,
        2 * layers + 1 + 2 * head_layers,
    );
    add(
        "enc qk linear d->2d",
        timeit(dev, iters, || w_qk.forward(&x))?,
        layers,
    );
    add(
        "enc v linear d->d",
        timeit(dev, iters, || w_v.forward(&x))?,
        layers,
    );
    let qk4 = qk.reshape((b, s, 2 * h, hd))?;
    add(
        "rope_thd (b,s,2h,hd)",
        timeit(dev, iters, || {
            candle_nn::rotary_emb::rope_thd(&qk4, &cos, &sin)
        })?,
        layers,
    );
    #[cfg(feature = "metal")]
    if dev.is_metal() {
        add(
            "laya rope_thd (32-bit grid)",
            timeit(dev, iters, || {
                laya_model::nn_probe::rope_thd(&qk4, &cos, &sin)
            })?,
            0,
        );
    }
    #[cfg(feature = "metal")]
    if dev.is_metal() {
        let q = qk4.narrow(2, 0, h)?.transpose(1, 2)?;
        let k = qk4.narrow(2, h, h)?.transpose(1, 2)?;
        let vv = v.reshape((b, s, h, hd))?.transpose(1, 2)?;
        let m = mask.broadcast_as((b, h, s, s))?;
        add(
            "sdpa (strided views, bcast mask)",
            timeit(dev, iters, || {
                candle_nn::ops::sdpa(&q, &k, &vv, Some(&m), false, 0.125, 1.0)
            })?,
            layers + head_layers,
        );
    }
    add(
        "heads_to_rows (zeros + b*h copy2d)",
        timeit(dev, iters, || heads_to_rows(&att_out))?,
        layers + head_layers,
    );
    add(
        "  of which zeros (b,s,h,hd)",
        timeit(dev, iters, || Tensor::zeros((b, s, h, hd), dtype, dev))?,
        0,
    );
    add(
        "enc wo linear d->d",
        timeit(dev, iters, || w_o.forward(&x))?,
        layers,
    );
    add(
        "residual add (b,s,d)",
        timeit(dev, iters, || &x + &x)?,
        2 * layers + 2 * head_layers,
    );
    add(
        "enc wi_input/wi_gate d->inter",
        timeit(dev, iters, || w_i.forward(&x))?,
        2 * layers,
    );
    add(
        "gelu_erf (b,s,inter)",
        timeit(dev, iters, || hid.gelu_erf())?,
        layers,
    );
    add(
        "mul (b,s,inter)",
        timeit(dev, iters, || &hid * &hid)?,
        layers,
    );
    add(
        "enc wo_mlp inter->d",
        timeit(dev, iters, || w_mo.forward(&hid))?,
        layers,
    );
    add(
        "head in_proj qk d->2d +bias",
        timeit(dev, iters, || h_in.forward(&x))?,
        head_layers,
    );
    add(
        "head in_proj v d->d +bias",
        timeit(dev, iters, || h_v.forward(&x))?,
        head_layers,
    );
    add(
        "head out_proj d->d +bias",
        timeit(dev, iters, || h_o.forward(&x))?,
        head_layers,
    );
    add(
        "head linear1 d->4d +bias",
        timeit(dev, iters, || h_1.forward(&x))?,
        head_layers,
    );
    add(
        "head relu (b,s,4d)",
        timeit(dev, iters, || hid4.relu())?,
        head_layers,
    );
    add(
        "head linear2 4d->d +bias",
        timeit(dev, iters, || h_2.forward(&hid4))?,
        head_layers,
    );
    let tiny = Tensor::zeros(16, dtype, dev)?;
    add(
        "one tiny dispatch (add of 16 elems)",
        timeit(dev, 400, || &tiny + &tiny)?,
        0,
    );
    let _ = hh;

    println!(
        "\n### ops at b={b} s={s} ({} tokens), {layers} encoder layers + {head_layers} head layers",
        b * s
    );
    println!("| op | ms/call | calls/forward | ms/forward | share of batch |");
    println!("|---|---|---|---|---|");
    let mut total = 0.0;
    for (name, ms, n) in &rows {
        let per = ms * *n as f64;
        total += per;
        println!(
            "| {name} | {ms:.3} | {n} | {per:.2} | {:.1}% |",
            100.0 * per / batch_ms
        );
    }
    println!(
        "| **sum of ops** | | | {total:.2} | {:.1}% |",
        100.0 * total / batch_ms
    );
    println!("| measured batch (p50) | | | {batch_ms:.2} | 100% |");
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let dir = PathBuf::from(arg(&args, "--model").expect("--model <dir>"));
    let w: usize = arg(&args, "--window")
        .and_then(|s| s.parse().ok())
        .unwrap_or(128);
    let k: usize = arg(&args, "--k").and_then(|s| s.parse().ok()).unwrap_or(12);
    let iters: usize = arg(&args, "--iters")
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let part = arg(&args, "--part").unwrap_or_else(|| "all".into());
    let kind = match arg(&args, "--device").as_deref() {
        Some("cpu") => DeviceKind::Cpu,
        _ => DeviceKind::Metal,
    };
    let source = PathBuf::from(arg(&args, "--source").unwrap_or_else(|| "crates".into()));
    let model = LayaModel::load(&dir, kind)?;
    let question = format!("Is this source code relevant to the software change: \"{TASK}\"?");
    let seqs = model.sequences();
    let ids = seqs.encode_state(&corpus(&source)?, None)?;
    let stride = (ids.len() - w) / k;
    let rows: Vec<Vec<u32>> = (0..k)
        .map(|i| ids[i * stride..i * stride + w].to_vec())
        .collect();
    let seq_len = seqs
        .build_from_state_ids(&rows[0], &Question::noul(&question))?
        .ids
        .len();
    println!(
        "model {} on {:?}; W={w} K={k}; sequence length {seq_len}",
        dir.display(),
        model.device_kind()
    );

    // Warm-up: kernel compilation, every shape.
    let t = Instant::now();
    while t.elapsed().as_secs_f64() < 12.0 {
        model.noul_ids_within(&question, &rows, Instant::now() + Duration::from_secs(60))?;
        model.noul_ids(&question, &rows, None)?;
        model.noul_ids_within(
            &question,
            &rows[..4.min(k)],
            Instant::now() + Duration::from_secs(60),
        )?;
    }
    let mut b8 = Vec::new();
    let mut b4 = Vec::new();
    if part == "all" || part == "e2e" {
        e2e(&model, &rows, &question, iters)?;
    }
    if part == "all" || part == "ops" {
        for _ in 0..iters {
            let t = Instant::now();
            model.noul_ids_within(
                &question,
                &rows[..8.min(k)],
                Instant::now() + Duration::from_secs(60),
            )?;
            b8.push(t.elapsed().as_secs_f64() * 1e3);
            let t = Instant::now();
            model.noul_ids_within(
                &question,
                &rows[..4.min(k)],
                Instant::now() + Duration::from_secs(60),
            )?;
            b4.push(t.elapsed().as_secs_f64() * 1e3);
        }
        b8.sort_by(f64::total_cmp);
        b4.sort_by(f64::total_cmp);
        let files = ModelFiles::resolve(&dir)?;
        let cfg = EncoderConfig::load(&files.encoder_config)?;
        let dev = if kind == DeviceKind::Cpu {
            Device::Cpu
        } else {
            Device::new_metal(0)?
        };
        ops(&dev, &cfg, 8.min(k), seq_len, pct(&b8, 0.5))?;
        ops(&dev, &cfg, 4.min(k), seq_len, pct(&b4, 0.5))?;
    }
    Ok(())
}
