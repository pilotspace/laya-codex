//! `LayaModel`: loading, device selection, length-bucketed batching, calibration.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;

use crate::config::{AgentConfig, EncoderConfig, ModelFiles};
use crate::encoder::ModernBert;
use crate::error::{ModelError, Result};
use crate::head::DecisionHead;
use crate::nn::{AttnMasks, window_band};
use crate::sequence::{BuiltSequence, QType, Question, SequenceBuilder};

/// Which compute device to run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    /// Plain CPU (f32).
    Cpu,
    /// Apple Metal GPU (requires the `metal` cargo feature and a Metal device at runtime).
    Metal,
    /// Metal when available, otherwise CPU.
    Auto,
}

/// Whether the crate was built with Metal support and a Metal device exists.
pub fn metal_available() -> bool {
    cfg!(feature = "metal") && candle_core::utils::metal_is_available()
}

/// Tunables for [`LayaModel::load_with`].
#[derive(Debug, Clone)]
pub struct LoadOptions {
    /// Compute dtype. Default: f32 on CPU, f16 on Metal (the weights are stored as f16).
    pub dtype: Option<DType>,
    /// Padded-token budget of one micro-batch (`max_len_in_batch * rows`).
    pub max_batch_tokens: usize,
    /// Maximum rows of one micro-batch.
    pub max_batch_rows: usize,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            dtype: None,
            max_batch_tokens: 32 * 512,
            max_batch_rows: 64,
        }
    }
}

/// Calibrated answer distribution of one question.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    /// Raw logits per option (uncalibrated).
    pub logits: Vec<f32>,
    /// `softmax(logits / temperature)`.
    pub probs: Vec<f32>,
    /// Temperature used (from the `type:cardinality` bucket).
    pub temperature: f32,
}

/// The Laya typed-decision model: ModernBERT encoder + decision head, loaded from a model dir.
pub struct LayaModel {
    device: Device,
    dtype: DType,
    kind: DeviceKind,
    sequences: SequenceBuilder,
    agent: AgentConfig,
    encoder: ModernBert,
    head: DecisionHead,
    local_window: usize,
    opts: LoadOptions,
    /// Serializes forward passes (one in-flight batch per model keeps device memory bounded).
    run_lock: Mutex<()>,
    /// Sliding-window bands keyed by padded sequence length.
    bands: Mutex<HashMap<usize, Tensor>>,
    /// Measured speed (smoothed nanoseconds per padded token, `0` = not measured yet), used to
    /// predict whether the next micro-batch fits before a deadline.
    ns_per_token: AtomicU64,
    /// Set after the first run, whose time includes one-time device setup.
    warmed: AtomicBool,
}

impl std::fmt::Debug for LayaModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayaModel")
            .field("device", &self.kind)
            .field("dtype", &self.dtype)
            .field("max_len", &self.agent.max_len)
            .finish_non_exhaustive()
    }
}

impl LayaModel {
    /// Load the model from `dir` (see [`ModelFiles`] for the layout) on the requested device.
    ///
    /// The weights file is memory-mapped; it must not be modified while the model is alive.
    pub fn load(dir: &Path, kind: DeviceKind) -> Result<Self> {
        Self::load_with(dir, kind, LoadOptions::default())
    }

    /// [`load`](Self::load) with explicit dtype / batching options.
    pub fn load_with(dir: &Path, kind: DeviceKind, opts: LoadOptions) -> Result<Self> {
        let started = Instant::now();
        let files = ModelFiles::resolve(dir)?;
        let enc_cfg = EncoderConfig::load(&files.encoder_config)?;
        let agent = AgentConfig::load(&files.agent_config)?;
        let sequences = SequenceBuilder::from_files(&files.tokenizer, &agent)?;
        let (device, kind) = select_device(kind)?;
        let dtype = opts.dtype.unwrap_or(if device.is_cpu() {
            DType::F32
        } else {
            DType::F16
        });
        // SAFETY: `from_mmaped_safetensors` maps the file read-only; the contract (documented on
        // `load`) is that nobody truncates or rewrites the weights file while this model lives.
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[&files.weights], dtype, &device)? };
        let encoder = ModernBert::load(vb.pp("encoder"), &enc_cfg, agent.max_len)?;
        let head = DecisionHead::load(vb.clone(), enc_cfg.hidden_size, agent.head_layers)?;
        tracing::info!(
            dir = %dir.display(),
            device = ?kind,
            ?dtype,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "laya model loaded"
        );
        // One line on stderr per process so a binary without a tracing subscriber still
        // shows which backend it ended up on (the CPU path is ~10x slower than Metal).
        static ANNOUNCED: std::sync::Once = std::sync::Once::new();
        ANNOUNCED.call_once(|| {
            eprintln!(
                "laya-model: loaded {} on {:?} ({:?}, metal feature {}) in {:.1}s",
                dir.display(),
                kind,
                dtype,
                if cfg!(feature = "metal") { "on" } else { "off" },
                started.elapsed().as_secs_f64()
            );
        });
        Ok(Self {
            device,
            dtype,
            kind,
            sequences,
            agent,
            encoder,
            head,
            local_window: enc_cfg.local_window(),
            opts,
            run_lock: Mutex::new(()),
            bands: Mutex::new(HashMap::new()),
            ns_per_token: AtomicU64::new(0),
            warmed: AtomicBool::new(false),
        })
    }

    /// Device actually in use (`Auto` is resolved).
    pub fn device_kind(&self) -> DeviceKind {
        self.kind
    }

    /// Compute dtype in use.
    pub fn dtype(&self) -> DType {
        self.dtype
    }

    /// Tokenizer and sequence budgets.
    pub fn sequences(&self) -> &SequenceBuilder {
        &self.sequences
    }

    /// Agent configuration (budgets, temperatures).
    pub fn agent_config(&self) -> &AgentConfig {
        &self.agent
    }

    /// Calibrated `P(true)` of the `noul` `question` for each state, in input order.
    pub fn noul(&self, question: &str, states: &[String]) -> Result<Vec<f32>> {
        let q = Question::noul(question);
        let items: Vec<(&Question, &str)> = states.iter().map(|s| (&q, s.as_str())).collect();
        Ok(self
            .decide(&items)?
            .into_iter()
            .map(|d| d.probs[1])
            .collect())
    }

    /// [`noul`](Self::noul) over pre-tokenized states (see [`SequenceBuilder::encode_state`]),
    /// with an optional deadline after which remaining micro-batches are abandoned.
    pub fn noul_ids(
        &self,
        question: &str,
        state_ids: &[Vec<u32>],
        deadline: Option<Instant>,
    ) -> Result<Vec<f32>> {
        let q = Question::noul(question);
        let seqs = state_ids
            .iter()
            .map(|ids| self.sequences.build_checked(ids, &q))
            .collect::<Result<Vec<_>>>()?;
        let refs: Vec<(&BuiltSequence, QType)> = seqs.iter().map(|s| (s, QType::Noul)).collect();
        Ok(self
            .decide_sequences(&refs, deadline)?
            .into_iter()
            .map(|d| d.probs[1])
            .collect())
    }

    /// [`noul_ids`](Self::noul_ids) that scores states in input order (most promising first)
    /// and stops before `deadline`: a micro-batch runs only if it is predicted to finish in
    /// time at the measured speed. `None` = not reached. Never overruns the deadline by more
    /// than one mispredicted micro-batch, so the model is free again soon after it.
    pub fn noul_ids_within(
        &self,
        question: &str,
        state_ids: &[Vec<u32>],
        deadline: Instant,
    ) -> Result<Vec<Option<f32>>> {
        let q = Question::noul(question);
        let seqs = state_ids
            .iter()
            .map(|ids| self.sequences.build_checked(ids, &q))
            .collect::<Result<Vec<_>>>()?;
        let refs: Vec<(&BuiltSequence, QType)> = seqs.iter().map(|s| (s, QType::Noul)).collect();
        let mut out: Vec<Option<Decision>> = vec![None; refs.len()];
        let _guard = self
            .run_lock
            .lock()
            .map_err(|_| ModelError::Device("model lock poisoned".into()))?;
        for batch in plan_batches_in_order(&refs, self.opts.max_batch_tokens, IN_ORDER_ROWS) {
            let tokens = batch.len * batch.rows.len();
            let speed = self.ns_per_token.load(Ordering::Relaxed);
            let now = Instant::now();
            if !batch_fits(now, deadline, tokens, speed) {
                if now < deadline {
                    // Skipped on prediction alone: lower the estimate so an outlier heals.
                    self.ns_per_token
                        .store(decayed_estimate(speed), Ordering::Relaxed);
                }
                break;
            }
            self.run_batch(&refs, &batch, &mut out)?;
        }
        tracing::debug!(
            scored = out.iter().filter(|d| d.is_some()).count(),
            total = refs.len(),
            "laya scored within deadline"
        );
        Ok(out.into_iter().map(|d| d.map(|d| d.probs[1])).collect())
    }

    /// Calibrated distribution over `criteria` for a `choice` question, per state.
    pub fn choice(
        &self,
        question: &str,
        criteria: &[(&str, Option<&str>)],
        states: &[String],
    ) -> Result<Vec<Vec<f32>>> {
        let q = Question::choice(question, criteria);
        let items: Vec<(&Question, &str)> = states.iter().map(|s| (&q, s.as_str())).collect();
        Ok(self.decide(&items)?.into_iter().map(|d| d.probs).collect())
    }

    /// Score arbitrary `(question, state)` pairs (states are tokenized here).
    pub fn decide(&self, items: &[(&Question, &str)]) -> Result<Vec<Decision>> {
        let seqs = items
            .iter()
            .map(|(q, state)| {
                let ids = self.sequences.encode_state(state, None)?;
                self.sequences.build_checked(&ids, q)
            })
            .collect::<Result<Vec<_>>>()?;
        let refs: Vec<(&BuiltSequence, QType)> = seqs
            .iter()
            .zip(items)
            .map(|(s, (q, _))| (s, q.qtype))
            .collect();
        self.decide_sequences(&refs, None)
    }

    /// Run prebuilt sequences. Rows are sorted by length and padded per micro-batch under the
    /// [`LoadOptions`] token budget; results come back in input order.
    ///
    /// If `deadline` passes before a micro-batch starts, `Err(ModelError::Deadline)` is returned.
    pub fn decide_sequences(
        &self,
        seqs: &[(&BuiltSequence, QType)],
        deadline: Option<Instant>,
    ) -> Result<Vec<Decision>> {
        let mut out: Vec<Option<Decision>> = vec![None; seqs.len()];
        if seqs.is_empty() {
            return Ok(Vec::new());
        }
        let _guard = self
            .run_lock
            .lock()
            .map_err(|_| ModelError::Device("model lock poisoned".into()))?;
        let started = Instant::now();
        for batch in plan_batches(seqs, self.opts.max_batch_tokens, self.opts.max_batch_rows) {
            if let Some(dl) = deadline
                && Instant::now() >= dl
            {
                tracing::warn!(
                    scored = out.iter().filter(|d| d.is_some()).count(),
                    total = seqs.len(),
                    "laya deadline exceeded"
                );
                return Err(ModelError::Deadline);
            }
            self.run_batch(seqs, &batch, &mut out)?;
        }
        tracing::debug!(
            rows = seqs.len(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "laya batch scored"
        );
        Ok(out
            .into_iter()
            .map(|d| d.expect("every row is scored by exactly one micro-batch"))
            .collect())
    }

    /// Run one micro-batch, store its decisions in `out` and update the speed estimate.
    /// Callers hold `run_lock`.
    fn run_batch(
        &self,
        seqs: &[(&BuiltSequence, QType)],
        batch: &MicroBatch,
        out: &mut [Option<Decision>],
    ) -> Result<()> {
        let started = Instant::now();
        let logits = self.forward_batch(seqs, batch)?;
        for (row, &i) in batch.rows.iter().enumerate() {
            let (seq, qtype) = seqs[i];
            let k = seq.markers.len();
            let z = &logits[row * batch.kmax..row * batch.kmax + k];
            let temperature = self.agent.temperature_for(qtype, k);
            out[i] = Some(Decision {
                logits: z.to_vec(),
                probs: softmax_scaled(z, temperature),
                temperature,
            });
        }
        let tokens = (batch.len * batch.rows.len()).max(1) as u64;
        let measured = started.elapsed().as_nanos() as u64 / tokens;
        let first_run = !self.warmed.swap(true, Ordering::Relaxed);
        let old = self.ns_per_token.load(Ordering::Relaxed);
        self.ns_per_token.store(
            updated_estimate(old, measured, first_run),
            Ordering::Relaxed,
        );
        Ok(())
    }

    /// One padded micro-batch through encoder + head; returns `(rows * kmax)` logits, masked
    /// with `-1e4` for absent options (as the reference does).
    fn forward_batch(
        &self,
        seqs: &[(&BuiltSequence, QType)],
        batch: &MicroBatch,
    ) -> Result<Vec<f32>> {
        let (b, s, kmax) = (batch.rows.len(), batch.len, batch.kmax);
        let pad = self.sequences.pad_id();
        let mut ids = vec![pad; b * s];
        let mut mask = vec![0f32; b * s];
        let mut qtypes = Vec::with_capacity(b);
        let mut markers = vec![0u32; b * kmax];
        let mut present = vec![false; b * kmax];
        for (r, &i) in batch.rows.iter().enumerate() {
            let (seq, qtype) = seqs[i];
            ids[r * s..r * s + seq.ids.len()].copy_from_slice(&seq.ids);
            mask[r * s..r * s + seq.ids.len()].fill(1.0);
            qtypes.push(qtype as u32);
            for (j, &m) in seq.markers.iter().enumerate() {
                markers[r * kmax + j] = (r * s + m) as u32;
                present[r * kmax + j] = true;
            }
            // Absent markers gather row `r`'s [CLS]; they are masked out below.
            for j in seq.markers.len()..kmax {
                markers[r * kmax + j] = (r * s) as u32;
            }
        }
        let dev = &self.device;
        let mut prof = crate::encoder::StageProfiler::from_env(dev);
        let ids_t = Tensor::from_vec(ids, (b, s), dev)?;
        let mask_t = Tensor::from_vec(mask, (b, s), dev)?;
        let qtype_t = Tensor::from_vec(qtypes, b, dev)?;
        let markers_t = Tensor::from_vec(markers, b * kmax, dev)?;
        let band = self.band(s)?;
        let masks = AttnMasks::new(&mask_t, &band, self.dtype)?;
        prof.tick("inputs+masks")?;
        let h = self.encoder.forward(&ids_t, &masks)?;
        prof.tick("encoder")?;
        let logits = self.head.forward(&h, &qtype_t, &masks.global, &markers_t)?;
        prof.tick("head")?;
        let mut logits = logits.to_vec1::<f32>()?;
        prof.tick("to_host")?;
        prof.report(&format!("batch b={b} s={s}"));
        for (z, ok) in logits.iter_mut().zip(&present) {
            if !ok {
                *z = -1.0e4;
            }
        }
        Ok(logits)
    }

    fn band(&self, s: usize) -> Result<Tensor> {
        let mut bands = self
            .bands
            .lock()
            .map_err(|_| ModelError::Device("band cache poisoned".into()))?;
        if let Some(t) = bands.get(&s) {
            return Ok(t.clone());
        }
        let t = window_band(s, self.local_window, self.dtype, &self.device)?;
        bands.insert(s, t.clone());
        Ok(t)
    }
}

fn select_device(kind: DeviceKind) -> Result<(Device, DeviceKind)> {
    match kind {
        DeviceKind::Cpu => Ok((Device::Cpu, DeviceKind::Cpu)),
        DeviceKind::Metal => {
            if !cfg!(feature = "metal") {
                return Err(ModelError::Device(
                    "laya-model was built without the `metal` feature".into(),
                ));
            }
            if !candle_core::utils::metal_is_available() {
                return Err(ModelError::Device("no Metal device available".into()));
            }
            Ok((Device::new_metal(0)?, DeviceKind::Metal))
        }
        DeviceKind::Auto => {
            if metal_available() {
                match Device::new_metal(0) {
                    Ok(d) => return Ok((d, DeviceKind::Metal)),
                    Err(e) => tracing::warn!(error = %e, "metal init failed, falling back to cpu"),
                }
            } else if cfg!(target_os = "macos") {
                // The CPU path is ~10x slower; on a Mac this is almost always a build mistake
                // (the `metal` cargo feature was not enabled on the laya-model dependency).
                let reason = if cfg!(feature = "metal") {
                    "no Metal device was found"
                } else {
                    "laya-model was built without the `metal` cargo feature"
                };
                tracing::warn!(reason, "DeviceKind::Auto resolved to CPU on macOS");
                eprintln!("laya-model: warning: running on CPU because {reason}");
            }
            Ok((Device::Cpu, DeviceKind::Cpu))
        }
    }
}

/// Rows of one padded micro-batch.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MicroBatch {
    rows: Vec<usize>,
    len: usize,
    kmax: usize,
}

/// Length-bucketed batching: sort rows by length, then greedily fill micro-batches so that
/// `max_len * rows <= max_tokens` and `rows <= max_rows` (port of `predict_items`).
/// Rows per in-order micro-batch: small enough that a deadline can stop between batches, and
/// throughput on Metal is flat from 8 to 24 rows (`examples/bench.rs`).
const IN_ORDER_ROWS: usize = 8;

/// Micro-batches in input order (the caller's most promising rows first), at most `max_rows`
/// rows and `max_tokens` padded tokens each. Unlike [`plan_batches`], rows are not sorted by
/// length: a deadline that stops between batches must drop the least promising rows.
fn plan_batches_in_order(
    seqs: &[(&BuiltSequence, QType)],
    max_tokens: usize,
    max_rows: usize,
) -> Vec<MicroBatch> {
    let mut batches = Vec::new();
    let mut i = 0;
    while i < seqs.len() {
        let mut j = i;
        let mut len = 0;
        while j < seqs.len() && j - i < max_rows.max(1) {
            let l = seqs[j].0.ids.len().max(len);
            if l * (j - i + 1) > max_tokens && j > i {
                break;
            }
            len = l;
            j += 1;
        }
        let kmax = seqs[i..j]
            .iter()
            .map(|s| s.0.markers.len())
            .max()
            .unwrap_or(0)
            .max(1);
        batches.push(MicroBatch {
            rows: (i..j).collect(),
            len: len.max(1),
            kmax,
        });
        i = j;
    }
    batches
}

/// Whether a micro-batch of `tokens` padded tokens is predicted to finish by `deadline` at
/// the measured speed. Unknown speed (`0`) → try it.
fn batch_fits(now: Instant, deadline: Instant, tokens: usize, ns_per_token: u64) -> bool {
    if now >= deadline {
        return false;
    }
    let predicted = Duration::from_nanos(ns_per_token.saturating_mul(tokens as u64));
    now + predicted <= deadline
}

/// Speed estimate after a new measurement: 3/4 old + 1/4 new (the first one is taken as is).
/// The first run of a loaded model is ignored: it includes one-time setup (Metal compiles its
/// kernels, ~10 s), which would make every later batch look too slow to start.
fn updated_estimate(old: u64, measured: u64, first_run: bool) -> u64 {
    if first_run {
        old
    } else if old == 0 {
        measured
    } else {
        (old * 3 + measured) / 4
    }
}

/// Estimate after a batch was skipped on prediction alone: lowered by a quarter, so an outlier
/// cannot keep the model from ever running (and so being measured) again.
fn decayed_estimate(old: u64) -> u64 {
    old - old / 4
}

fn plan_batches(
    seqs: &[(&BuiltSequence, QType)],
    max_tokens: usize,
    max_rows: usize,
) -> Vec<MicroBatch> {
    let mut order: Vec<usize> = (0..seqs.len()).collect();
    order.sort_by_key(|&i| seqs[i].0.ids.len());
    let mut batches = Vec::new();
    let mut i = 0;
    while i < order.len() {
        let mut j = i;
        let mut len = 0;
        while j < order.len() && j - i < max_rows.max(1) {
            let l = seqs[order[j]].0.ids.len().max(len);
            if l * (j - i + 1) > max_tokens && j > i {
                break;
            }
            len = l;
            j += 1;
        }
        let rows: Vec<usize> = order[i..j].to_vec();
        let kmax = rows
            .iter()
            .map(|&r| seqs[r].0.markers.len())
            .max()
            .unwrap_or(0)
            .max(1);
        batches.push(MicroBatch {
            rows,
            len: len.max(1),
            kmax,
        });
        i = j;
    }
    batches
}

/// `softmax(z / t)` computed in f64 for stability, returned as f32.
fn softmax_scaled(z: &[f32], t: f32) -> Vec<f32> {
    let scaled: Vec<f64> = z.iter().map(|&v| v as f64 / t as f64).collect();
    let max = scaled.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let exps: Vec<f64> = scaled.iter().map(|v| (v - max).exp()).collect();
    let sum: f64 = exps.iter().sum();
    exps.iter().map(|e| (e / sum) as f32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq(n: usize, k: usize) -> BuiltSequence {
        BuiltSequence {
            ids: vec![1; n],
            markers: (0..k).collect(),
        }
    }

    #[test]
    fn batches_are_length_bucketed_under_token_budget() {
        let s = [
            seq(500, 2),
            seq(10, 2),
            seq(300, 3),
            seq(12, 2),
            seq(490, 2),
        ];
        let items: Vec<(&BuiltSequence, QType)> = s.iter().map(|x| (x, QType::Noul)).collect();
        let b = plan_batches(&items, 1000, 64);
        assert_eq!(b[0].rows, vec![1, 3, 2]); // 10, 12, 300 -> 3 * 300 <= 1000
        assert_eq!(b[0].len, 300);
        assert_eq!(b[0].kmax, 3);
        assert_eq!(b[1].rows, vec![4, 0]); // 490, 500 -> 2 * 500 <= 1000
        assert_eq!(b[1].len, 500);
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn in_order_batches_keep_the_callers_order() {
        let s = [
            seq(500, 2),
            seq(10, 2),
            seq(300, 3),
            seq(12, 2),
            seq(490, 2),
        ];
        let items: Vec<(&BuiltSequence, QType)> = s.iter().map(|x| (x, QType::Noul)).collect();
        let rows = |b: Vec<MicroBatch>| b.into_iter().map(|x| x.rows).collect::<Vec<_>>();
        // Row cap: most promising first, never re-sorted by length.
        assert_eq!(
            rows(plan_batches_in_order(&items, 10_000, 2)),
            vec![vec![0, 1], vec![2, 3], vec![4]]
        );
        // Token cap: 3 rows padded to 500 would be 1,500 > 1,000.
        let b = plan_batches_in_order(&items, 1000, 8);
        assert_eq!(b[0].rows, vec![0, 1]);
        assert_eq!(b[0].len, 500);
        assert_eq!(b[1].rows, vec![2, 3]);
        assert_eq!(b[1].kmax, 3);
        assert_eq!(b[2].rows, vec![4]);
    }

    #[test]
    fn a_batch_runs_only_if_its_predicted_time_fits_before_the_deadline() {
        let now = Instant::now();
        let deadline = now + std::time::Duration::from_millis(100);
        assert!(batch_fits(now, deadline, 1000, 0), "unknown speed: try it");
        assert!(batch_fits(now, deadline, 1000, 50_000), "50 ms fits");
        assert!(!batch_fits(now, deadline, 1000, 200_000), "200 ms does not");
        assert!(!batch_fits(deadline, deadline, 1, 1), "deadline reached");
    }

    #[test]
    fn speed_estimate_smooths_new_measurements() {
        assert_eq!(updated_estimate(0, 400, false), 400, "first measurement");
        assert_eq!(updated_estimate(400, 800, false), 500, "3/4 old + 1/4 new");
    }

    #[test]
    fn the_first_run_of_a_loaded_model_does_not_set_the_speed() {
        // The first run on a device compiles kernels (~10 s on Metal): not the model's speed.
        assert_eq!(updated_estimate(0, 3_000_000, true), 0);
    }

    #[test]
    fn a_skipped_batch_lowers_the_estimate_so_it_cannot_lock_the_model_out() {
        let now = Instant::now();
        let deadline = now + std::time::Duration::from_millis(1000);
        let mut ns = 3_000_000; // an outlier: 8 rows x 320 tokens predicted at ~7.7 s
        let mut skips = 0;
        while !batch_fits(now, deadline, 2560, ns) {
            ns = decayed_estimate(ns);
            skips += 1;
            assert!(skips < 50, "the estimate never recovers");
        }
        assert!(skips > 0);
    }

    #[test]
    fn a_single_oversized_row_still_gets_its_own_batch() {
        let s = [seq(600, 2)];
        let items: Vec<(&BuiltSequence, QType)> = s.iter().map(|x| (x, QType::Noul)).collect();
        let b = plan_batches(&items, 100, 64);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].len, 600);
    }

    #[test]
    fn max_rows_is_respected() {
        let s: Vec<BuiltSequence> = (0..5).map(|_| seq(8, 2)).collect();
        let items: Vec<(&BuiltSequence, QType)> = s.iter().map(|x| (x, QType::Noul)).collect();
        let b = plan_batches(&items, 10_000, 2);
        assert_eq!(
            b.iter().map(|x| x.rows.len()).collect::<Vec<_>>(),
            vec![2, 2, 1]
        );
    }

    #[test]
    fn softmax_scaled_matches_reference_case() {
        let p = softmax_scaled(&[0.352_485_2, 0.501_488_4], 1.983_399_5);
        assert!((p[0] - 0.481_227_55).abs() < 1e-6);
        assert!((p[1] - 0.518_772_5).abs() < 1e-6);
    }
}
