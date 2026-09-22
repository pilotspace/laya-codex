//! Parity tests against the official Python reference (`fixtures/laya_parity.json`).
//!
//! The model weights are local files (`~/.cache/laya-codex/models/laya-base` or
//! `$LAYA_MODEL_DIR`). When they are absent the tests are skipped with a message so that
//! weight-less CI (Linux) still passes; on a developer machine they must be green.

use std::path::{Path, PathBuf};

use laya_core::{Chunk, Lang, Scorer};
use laya_model::{DeviceKind, LayaModel, LayaScorer, QType, Question, SequenceBuilder};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixtures {
    max_len: usize,
    head_max_len: usize,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    state: String,
    input_ids: Vec<u32>,
    markers: Vec<usize>,
    qtype: usize,
    temperature: f32,
    logits: Vec<f32>,
    probs: Vec<f32>,
}

fn model_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("LAYA_MODEL_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".cache/laya-codex/models/laya-base"))
        })?;
    if dir.join("model.safetensors").is_file() {
        Some(dir)
    } else {
        eprintln!("SKIP: model dir {} not found", dir.display());
        None
    }
}

fn fixtures() -> Fixtures {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/laya_parity.json");
    let text = std::fs::read_to_string(&path).expect("fixtures/laya_parity.json");
    serde_json::from_str(&text).expect("valid fixtures")
}

/// The three questions used by `spike/make_parity_fixtures.py`, in fixture order.
/// They are spelled out here because the `choice` criteria order matters and JSON maps
/// do not preserve it.
fn questions() -> Vec<Question> {
    vec![
        Question::noul(
            "Is this source code relevant to the software change: \"O(1) fast path for HGET\"?",
        ),
        Question::choice(
            "How much of this code should the agent read?",
            &[
                ("tight", Some("only a few lines")),
                ("function", Some("the whole function")),
                ("block", Some("the surrounding block")),
            ],
        ),
        Question::score(
            "Rate relevance to: add TLS support to the server",
            &["irrelevant", "somewhat", "relevant", "essential"],
        ),
    ]
}

fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "length mismatch: {a:?} vs {b:?}");
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

#[test]
fn sequence_builder_matches_reference_byte_for_byte() {
    let Some(dir) = model_dir() else { return };
    let fx = fixtures();
    let sb = SequenceBuilder::from_dir(&dir).expect("tokenizer + agent config");
    assert_eq!(sb.max_len(), fx.max_len);
    assert_eq!(sb.head_max_len(), fx.head_max_len);
    for (case, q) in fx.cases.iter().zip(questions()) {
        assert_eq!(q.qtype as usize, case.qtype);
        let seq = sb.build(&case.state, &q).expect("build sequence");
        assert_eq!(
            seq.ids, case.input_ids,
            "input_ids differ for {:?}",
            q.qtype
        );
        assert_eq!(
            seq.markers, case.markers,
            "markers differ for {:?}",
            q.qtype
        );
    }
}

#[test]
fn state_token_budget_truncates_from_the_right() {
    let Some(dir) = model_dir() else { return };
    let sb = SequenceBuilder::from_dir(&dir).expect("tokenizer");
    let text = "fn main() { println!(\"hello\"); }\n".repeat(60);
    let full = sb.encode_state(&text, None).unwrap();
    let cut = sb.encode_state(&text, Some(16)).unwrap();
    assert!(full.len() > 16);
    assert_eq!(cut, full[..16]);
    let q = Question::noul("q");
    let seq = sb.build_from_state_ids(&cut, &q).unwrap();
    assert_eq!(seq.markers.len(), 2);
    // [CLS] head [SEP] opts [SEP] state(16) [SEP]
    assert!(seq.ids.len() < 16 + 64);
    assert_eq!(seq.ids[seq.ids.len() - 17..seq.ids.len() - 1], cut[..]);
}

#[test]
fn head_budget_shrinks_long_options_evenly() {
    let Some(dir) = model_dir() else { return };
    let sb = SequenceBuilder::from_dir(&dir).expect("tokenizer");
    let long = "word ".repeat(60);
    let crit: Vec<(String, Option<String>)> = (0..6)
        .map(|i| (format!("k{i}"), Some(long.clone())))
        .collect();
    let crit_ref: Vec<(&str, Option<&str>)> = crit
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_deref()))
        .collect();
    let q = Question::choice("pick", &crit_ref);
    let seq = sb.build("state", &q).unwrap();
    assert_eq!(seq.markers.len(), 6);
    // per = max(4, (192 - 16) / 6) = 29 tokens per option including the [MASK]
    for w in seq.markers.windows(2) {
        assert_eq!(w[1] - w[0], 29);
    }
}

fn check_parity(model: &LayaModel, logit_tol: f32, prob_tol: f32) {
    let fx = fixtures();
    let items: Vec<(Question, &str)> = questions()
        .into_iter()
        .zip(fx.cases.iter().map(|c| c.state.as_str()))
        .collect();
    let refs: Vec<(&Question, &str)> = items.iter().map(|(q, s)| (q, *s)).collect();
    let out = model.decide(&refs).expect("forward");
    assert_eq!(out.len(), fx.cases.len());
    for (i, (case, d)) in fx.cases.iter().zip(&out).enumerate() {
        let dl = max_abs_diff(&d.logits, &case.logits);
        let dp = max_abs_diff(&d.probs, &case.probs);
        eprintln!(
            "case {i}: T={} logits {:?} vs {:?} (max |Δ| {dl:.2e}); probs {:?} vs {:?} (max |Δ| {dp:.2e})",
            case.temperature, d.logits, case.logits, d.probs, case.probs
        );
        assert!(
            (d.temperature - case.temperature).abs() < 1e-6,
            "temperature bucket mismatch"
        );
        assert!(
            dl <= logit_tol,
            "case {i}: logits off by {dl} (> {logit_tol})"
        );
        assert!(dp <= prob_tol, "case {i}: probs off by {dp} (> {prob_tol})");
    }
}

#[test]
fn cpu_f32_parity_with_python_reference() {
    let Some(dir) = model_dir() else { return };
    let model = LayaModel::load(&dir, DeviceKind::Cpu).expect("load model on cpu");
    check_parity(&model, 2e-3, 1e-3);
}

#[cfg(feature = "metal")]
#[test]
fn metal_parity_with_python_reference() {
    let Some(dir) = model_dir() else { return };
    if !laya_model::metal_available() {
        eprintln!("SKIP: metal not available at runtime");
        return;
    }
    let model = LayaModel::load(&dir, DeviceKind::Metal).expect("load model on metal");
    check_parity(&model, 5e-2, 2e-2);
}

#[test]
fn noul_returns_calibrated_p_true_per_state() {
    let Some(dir) = model_dir() else { return };
    let fx = fixtures();
    let model = LayaModel::load(&dir, DeviceKind::Cpu).expect("load");
    let q = "Is this source code relevant to the software change: \"O(1) fast path for HGET\"?";
    let states = vec![
        fx.cases[0].state.clone(),
        "unrelated: README badges\n".to_string(),
    ];
    let p = model.noul(q, &states).expect("noul");
    assert_eq!(p.len(), 2);
    assert!((p[0] - fx.cases[0].probs[1]).abs() < 1e-3, "p={p:?}");
    assert!(p.iter().all(|v| (0.0..=1.0).contains(v)));
}

#[test]
fn scorer_trait_scores_chunks_in_order() {
    let Some(dir) = model_dir() else { return };
    let fx = fixtures();
    let model = LayaModel::load(&dir, DeviceKind::Cpu).expect("load");
    let scorer = LayaScorer::new(model);
    assert_eq!(scorer.max_state_tokens, 256);
    let chunk = Chunk {
        path: "src/storage/hash.rs".into(),
        start_line: 10,
        end_line: 30,
        lang: Lang::Rust,
        symbol: "hget".into(),
        kind: "function_item".into(),
        defines: vec!["hget".into()],
        text: "fn hget(&self, key: &[u8]) -> Option<Bytes> {\n    self.map.get(key).cloned()\n}"
            .into(),
    };
    // The rendered state is exactly fixture case 0's state, and the default template is the
    // fixture question, so the score must equal the reference P(true).
    assert_eq!(LayaScorer::render_state(&chunk), fx.cases[0].state);
    let p = scorer
        .score("O(1) fast path for HGET", &[&chunk])
        .expect("score");
    assert_eq!(p.len(), 1);
    assert!((p[0] - fx.cases[0].probs[1]).abs() < 1e-3, "p={p:?}");
    assert_eq!(QType::Noul as usize, 2);
}
