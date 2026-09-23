---
license: apache-2.0
base_model: convaiinnovations/laya
base_model_relation: finetune
pipeline_tag: text-classification
language: [en]
tags: [laya, code-search, reranker, code-retrieval, calibrated, claude-code, laya-codex]
---

# laya-code

A code-relevance re-ranker fine-tuned from [Laya](https://huggingface.co/convaiinnovations/laya)
(ModernBERT-large encoder + typed-decision head, 421M parameters). Given a task description and a
source-code chunk, it answers one yes/no (`noul`) question with a **calibrated probability**:

```
question: Is this source code relevant to the software change: "{task}"?
state:    file: <path> (lines a-b)\n<code>        (truncated to 128 tokens in production)
```

It is the default re-ranker of [laya-codex](https://github.com/pilotspace/laya-codex), which feeds
Claude Code the most relevant code spans for a prompt (tree-sitter chunks, then Moon BM25
candidates, then laya-code re-ranking).

## Model details

| | |
|---|---|
| Base model | `convaiinnovations/laya`, root checkpoint (revision `1c5edc17a7acd8701df6fc341c0d179f1c62c982`), Apache-2.0 |
| Architecture | unchanged: same safetensors keys, shapes and dtypes as the base (205 F16 tensors + `temperature` F32), 842,609,210 bytes |
| What changed | `model.safetensors` (fine-tuned weights) and `rl_agent_config.json` (`model_name`, `noul:2` temperature **0.9410**, was 1.9834; `finetune` block). `encoder/config.json`, `tokenizer/*`, `rl_agent_api.py` and `rl_common.py` are byte-identical to the base. The `training` block of `rl_agent_config.json` is inherited from the base and describes the base's training, not this fine-tune. |
| Context | 512 tokens (`max_len`), 192 for the question head (`head_max_len`) |
| Runtime | Python: `rl_agent_api.RLAgent` from this repo (same as the base). Rust: `laya-model` crate of laya-codex (candle; Metal F16 on macOS, CPU F32 elsewhere), parity-tested against the Python reference |
| License | Apache-2.0 (see `LICENSE` and `NOTICE`) |

## Training data

Weak supervision from git history. Nothing was hand-labelled.

- **8 training repositories** (mixed Rust, Python, TypeScript and JavaScript): openai/codex,
  TinDang97/velos, MervinPraison/PraisonAI, badlogic/pi-mono, Portkey-AI/gateway (local
  `ai-guard` checkout), TinDang97/python-dependency-injector, Netflix/dispatch and
  pilotspace/hydroa (local `ai-proxy` checkout). Source: `finetune/repos.py`.
- **Held out** (never used for training or calibration): pilotspace/moon and pilot-space.
  Moon client codebases (helios, helios-mono, lunaris) were left out so that Moon vocabulary
  does not leak into the Moon eval. A root-commit check rejects forks and clones of each other
  and of the held-out repos.
- **Examples**: up to 600 non-merge commits per repo (the `build_data.py` default) that touch 1–4 source files and have an
  informative subject (at least 20 characters). The task is the subject plus a short first body
  line. The candidate list is the BM25 top 12 over other files plus the parent-revision windows
  of the touched files (40-line windows, stride 30). Labels: 1.0 for a window that overlaps a
  changed hunk, 0.7 for another window of a touched file, 0 for other files. Extra examples:
  hunks BM25 missed (label 1.0), a random same-file window (label 0.4) and random windows from
  other files (label 0).
- **Size**: 50,926 pairs, split by commit hash into 45,790 train and 5,136 validation pairs.
  Train pairs: 2,779 candidate positives, 5,609 candidate same-file, 24,768 candidate negatives,
  4,656 extra positives, 2,392 same-file, 5,586 random negatives. Per-repo counts (v2 data;
  the warm start used v1 data from the same repos):

  | repo | train commits | val commits | train pairs | val pairs |
  |---|---|---|---|---|
  | PraisonAI | 231 | 18 | 3,711 | 280 |
  | ai-guard | 460 | 40 | 7,404 | 665 |
  | ai-proxy | 200 | 23 | 3,293 | 380 |
  | codex | 446 | 54 | 7,676 | 932 |
  | dispatch | 447 | 53 | 7,347 | 863 |
  | pi-mono | 439 | 61 | 7,386 | 1,013 |
  | python-dependency-injector | 453 | 47 | 7,066 | 744 |
  | velos | 117 | 16 | 1,907 | 259 |
- **Training**: top 8 of 28 encoder layers, the final norm and the decision head. fp32 on an
  M4 Pro (MPS). AdamW; learning rate 2e-5 for the encoder and 1e-4 for the head. 32 sequences
  per update. Log loss against soft targets. 536 updates on v2 data, warm-started from 300
  updates on v1 data (about 2.3 h in total). Checkpoint chosen by lowest validation NLL. The
  `noul:2` temperature was then refitted on the validation split, using the exported F16
  weights.

## Evaluation

All numbers come from files in the laya-codex repository and are quoted as recorded. Gold labels
are file-level: the files the commit touched. Candidates are BM25 windows at HEAD.

### Re-ranker comparison, 128-token state (production setting)

`spike/results/compare_models.json` (`spike/compare_models.py`): the 40 most recent qualifying
moon commits, BM25 top 24, state truncated to 128 tokens, probabilities pooled over all
candidates (base rate 0.309).

| model | Laya-only MRR | Laya-only P@10 | RRF MRR | AUROC | ECE | mean P |
|---|---|---|---|---|---|---|
| BM25 alone | 0.480 (MRR) | 0.340 | – | – | – | – |
| laya-base (`convaiinnovations/laya`) | 0.479 | 0.348 | 0.505 | 0.586 | 0.362 | 0.671 |
| laya-typed-decisions | 0.441 | 0.288 | 0.456 | 0.539 | 0.239 | 0.545 |
| **laya-code** | **0.702** | **0.405** | **0.630** | **0.713** | **0.049** | 0.328 |

### Held-out evaluation, 256-token state (training protocol)

`spike/results/finetune_eval.json` (`finetune/eval.py`): 40 tasks per held-out repo, BM25 top 32,
state truncated to 256 tokens. Paired bootstrap over the tasks.

| repo | model | Laya-only MRR | RRF MRR | RRF P@10 | ECE (15 bins) | AUROC pooled |
|---|---|---|---|---|---|---|
| moon | laya-base | 0.497 | 0.586 | 0.343 | 0.461 | 0.584 |
| moon | laya-code | 0.526 | 0.519 | 0.398 | 0.060 | 0.677 |
| pilot-space | laya-base | 0.519 | 0.762 | 0.323 | 0.509 | 0.536 |
| pilot-space | laya-code | 0.646 | 0.714 | 0.393 | 0.016 | 0.709 |

Under this protocol, laya-code clearly improves calibration and discrimination. It puts more
gold-file spans in the top 10: RRF P@10 rose by +0.055 (95% CI [0.013, 0.098]) on moon and by
+0.070 ([0.033, 0.108]) on pilot-space. Its **RRF MRR did not beat laya-base's**: −0.066
([−0.195, 0.061]) on moon and −0.049 ([−0.172, 0.073]) on pilot-space. For that reason,
laya-codex fuses laya-code by score (`(1−w)·lexical + w·P`, w = 0.5) rather than by RRF.

End to end, the laya-codex paired Claude Code benchmark (20 moon tasks, `docs/RESULTS.md`)
measured −45% code-reading tokens and −25% wall-clock time for the whole pipeline, with no loss
of answer recall. The same document reports that the model's **marginal** contribution over
lexical-only ranking is within run-to-run noise at n = 20. Do not read the pipeline numbers as a
property of this model.

## Intended use

- Re-ranking lexical (BM25) candidates of source-code chunks for a natural-language
  software-change task, as a calibrated `P(relevant)`.
- Gating or fusing retrieval results by probability; P is calibrated to the training
  distribution (ECE ≤ 0.06 on held-out repos).

## Out of scope and limitations

- **Weak labels.** A commit touching a file does not make every window of it relevant, and the
  file-level gold is coarse.
- **Small evaluation.** Each held-out set has 40 tasks, and most CIs are wide. The two
  protocols (128 vs 256 tokens, top 24 vs 32) give different absolute numbers, as shown above.
- **Low probabilities.** P rarely exceeds 0.5 (max 0.49 on moon, 0.50 on pilot-space at 256
  tokens). Use rank or score fusion, or a threshold near 0.4, not "P ≥ 0.5 means relevant".
- **Under-trained.** Only about half an epoch of the v2 data was used, on a shared laptop.
  Validation AUROC was still rising when training stopped.
- **English prompts only.** Training covered Rust, Python, TypeScript and JavaScript; other
  languages are untested.
- **Other tasks untested.** It is not a general Laya replacement: `choice`/`score` questions
  (for example, task scope) were not trained, and zero-shot scope accuracy is poor
  (`spike/results/scope_eval.json`).
- **Too slow for interactive CPU use.** At 421M parameters, CPU re-ranking of 24 candidates is
  too slow for interactive use. laya-codex runs it on Metal, or falls back to lexical ranking.
- **Legal status of training data.** The model was trained on permissively licensed public
  code plus the author's own repositories. It is a classifier and cannot reproduce that code,
  but the legal status of weights trained on source code is not settled.

## License and attribution

Apache-2.0, like the base model. laya-code is a Derivative Work of
[convaiinnovations/laya](https://huggingface.co/convaiinnovations/laya) (Apache-2.0, © Convai
Innovations), which builds on
[answerdotai/ModernBERT-large](https://huggingface.co/answerdotai/ModernBERT-large) (Apache-2.0).
The modified files are `model.safetensors` and `rl_agent_config.json`; every other file is
unchanged from the base. See `NOTICE`.

## Files

See `MANIFEST.sha256` for the sha256 of every uploaded file.
