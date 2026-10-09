---
license: apache-2.0
base_model: answerdotai/ModernBERT-base
base_model_relation: finetune
pipeline_tag: text-classification
language: [en]
tags: [laya, code-search, reranker, code-retrieval, calibrated, claude-code, laya-codex, modernbert]
---

# laya-code-r2

A code-relevance re-ranker for [laya-codex](https://github.com/pilotspace/laya-codex): the
[answerdotai/ModernBERT-base](https://huggingface.co/answerdotai/ModernBERT-base) encoder with the typed-decision head of
[Laya](https://huggingface.co/convaiinnovations/laya), trained for one job. Given a task and a
source-code chunk, it answers one yes/no (`noul`) question with a calibrated probability:

```
question: Is this source code relevant to the software change: "{task}"?
state:    file: <path> (lines a-b)\n<code>        (cut to 128 tokens when laya-codex serves it)
```

laya-codex finds 24 candidates lexically (tree-sitter chunks, BM25, definitions and path matches),
this model scores the top 12, and laya-codex ranks them by `0.5 · lexical rank score + 0.5 · P`. It
replaces laya-code-r1 (ModernBERT-large, branch `r1`) as laya-codex's default: in the offline
replay it inlines **141 of 238** gold files against
132 for r1, with the hook at **165 ms** instead of 517 ms (p50).

**Where it is published.** This model goes on the `r2` branch of `tindang/laya-code`; it is not published yet. laya-codex's installer pins the sha256 of its `MANIFEST.sha256` (`f5d20914a7b4ff44eedbb95418ac1fa07af128c576a59607e97dc7fdb570a623`) and will pin the upload commit. The `main` branch (the first laya-code) and the `r1` branch are
unchanged.

## Model details

| | |
|---|---|
| Encoder | `answerdotai/ModernBERT-base`, revision `8949b909ec900327062f0ebf497f51aef5e6f0c8`, pretrained weights (Apache-2.0) |
| Decision head | Laya's, from its reference code `rl_common.py`; no Laya weights. Initialisation: random, the reference DecisionModel initialisation under seed 13 |
| Architecture | answerdotai/ModernBERT-base encoder (22 layers, hidden 768) + the laya decision head (2 transformer layers, type_emb, scorer, act_head), laya-code safetensors layout; 170 tensors (F16, `temperature` F32), 327,984,730 bytes |
| Context | 704 tokens (`max_len`) |
| Calibration | `noul:2` temperature **0.8383**, fitted at the 128-token window it is served at |
| Serving | `serving` block of `rl_agent_config.json`: 128 tokens per candidate, top 12 scored per prompt, 12 per search. laya-codex reads it; `LAYA_CODEX_STATE_TOKENS` and `LAYA_CODEX_SCORE_TOP` override it |
| Runtime | Python: `rl_agent_api.RLAgent` from this repo. Rust: the `laya-model` crate of laya-codex (candle; Metal F16 on macOS, CPU F32 elsewhere), parity-tested against the Python reference: 7 of 7 on CPU and Metal (w128 export; same weights, tokenizer and temperature as laya-code-r2) |
| License | Apache-2.0 (see `LICENSE` and `NOTICE`) |

## Training data

The candidate lists laya-code-r1 was trained on, unchanged (4,466 lists). Weak supervision from
the git history of fixed commits; nothing was hand-labelled.

- **7 training repositories:** openai/codex (`codex`), TinDang97/velos (`velos`), MervinPraison/PraisonAI (`PraisonAI`), earendil-works/pi (`pi-mono`), ets-labs/python-dependency-injector (`python-dependency-injector`), Netflix/dispatch (`dispatch`), pilotspace/hydroa (`ai-proxy`).
- **Lists shaped like production:** for each fixing commit, the parent revision is indexed with the
  laya-codex indexer and its retriever produces the task focus and the top 24 lexical candidates,
  exactly as the re-ranker sees them in laya-codex.
- **Labels:** a candidate overlapping a line the fix changed = 1.0; another chunk of a changed file = 0.6;
  anything else = 0.
- **Held out** (never used for training, checkpoint selection or calibration): TinDang97/pilot-space (`pilot-space`), pilotspace/moon (`moon`), encode/httpx (`httpx`), honojs/hono (`hono`).
  `finetune/leakage.py` checks for shared roots, vendored files and benchmark tasks; the check passed.

## Training

- Distillation: none (kd_weight 0): trained on the gold labels only. The distilled arm (student-kd, teacher laya-code-r1 at W=128) was trained side by side and not chosen.
- Windows: each list drawn at one of [128, 256, 384] state tokens; lists seen per window {'128': 2891, '256': 2944, '384': 2837}.
- Trainable: the top `--top-layers` encoder layers (every layer here) and the decision head. Command:
  `python3 finetune/train.py --base <student-init> --top-layers 22 --lists-per-step 4 --lr-enc 5e-05 --lr-head 0.0003 --warmup 50 --list-weight 1 --windows 128 256 384 --val-windows 128 256 384 --val-n 400 --grad-ckpt --pad-multiple 64 --epochs 4 --max-hours 10 --eval-every 271 --ckpt-every 50 --seed 13 --ckpt-dir <student-nokd> --kd-weight 0`.
- Updates: 2168 of 2168 planned (4 epochs of 2168 lists, 4 per update): the run completed; the selected checkpoint is from the first epoch.
- Selected: update 542: best mean blend top2_gold over the 128/256/384 validation windows (0.5902), then MRR (0.6479).
- M4 Pro 24 GB, MPS, fp32, activation checkpointing, batches padded to a multiple of 64 tokens; 5.7 h of optimisation.

Validation at the selected update (in-repository validation lists of the training repositories,
gold = a changed chunk; blend = the production ranking):

| window | AUROC | blend top-2 gold | model-only top-2 gold |
|---|---|---|---|
| 128 | 0.865 | 0.581 | 0.634 |
| 256 | 0.868 | 0.593 | 0.653 |
| 384 | 0.871 | 0.597 | 0.659 |

laya-code-r1 on the same lists at 128 tokens: AUROC 0.827, blend top-2 gold 0.497.

Calibration: noul:2 T 0.8383 at W=128 (fitted on 470 validation lists, 7507 candidates; NLL 0.2704 -> 0.2694, ECE 0.1082 -> 0.1135, AUROC 0.8646).

## Offline replay (held-out benchmark repositories)

bench/replay_hooks.py through the real UserPromptSubmit hook, tasks-v8 (60) + tasks-heldout (60), first prompts, memo off; gold = files the task's fix changed. The 238 gold files are in moon, httpx and hono, which training never saw.

Through the default wiring (feat/r2-default, no LAYA_CODEX_STATE_TOKENS / LAYA_CODEX_SCORE_TOP set):

| model | gold inlined | gold in the top 2 | mean injected chars | hook p50 | hook p90 | scored per prompt |
|---|---|---|---|---|---|---|
| laya-code-r2 | 141 of 238 | 141 | 4,504 | 165 ms | 171 ms | 12 |
| laya-code-r1 | 132 | 132 | 4,485 | 517 ms | 541 ms | 16 |
| keywords only | 118 | 118 | 4,441 | 7 ms | 10 ms | 0 |

Every prompt ran in `laya` mode for both models. The screen that chose the setting (the student at
other windows and candidate counts, under a tighter time budget; r1 and keywords as served):

| arm | gold inlined | mean injected chars | hook p50 |
|---|---|---|---|
| keywords | 118 of 238 | 4,441 | 7 ms |
| r1 | 132 of 238 | 4,485 | 527 ms |
| nokd-best-w128-k24 | 145 of 238 | 4,456 | 322 ms |
| nokd-best-w256-k16 | 142 of 238 | 4,498 | 366 ms |
| nokd-best-w384-k12 | 142 of 238 | 4,527 | 399 ms |

Scoring 24 candidates found the most (145), but 12 at 128
tokens keeps nearly all of it at about half the time (141 of 238, hook
167 ms p50 and 180 ms p90, arms rotated on one warm index), so
laya-codex serves 128 × 12.

## Intended use

- Re-ranking lexical code-search candidates for a natural-language change request, inside
  laya-codex (or with its scorer's exact input: the question above over `file: <path> (lines a-b)`
  and the chunk, cut to 128 tokens).
- Its probabilities are meant to be blended with the lexical order, not used alone as a relevance
  threshold.

Out of scope: general text classification, retrieval over whole repositories without a candidate
stage, non-code documents, and safety-relevant decisions.

## Limits

- The validation lists come from the training repositories (other commits); the only held-out test
  is the replay above: 120 tasks, 238 gold files, three repositories. laya-codex's pipeline was
  tuned on one of the two task sets (tasks-v8).
- Labels are weak: a commit changing a file does not make every chunk of it relevant.
- Calibration is only moderate: hard ECE 0.1135 after fitting the temperature, on the validation lists. Treat P as a ranking score
  first.
- It was trained on change requests (commit messages). Bare identifier lookups, which laya-codex's
  `search` tool also ranks, were not in training.
- The held-out list evaluation on pilot-space that r1 reported was not run for this model.
- Latency figures are from one Apple M4 Pro (Metal, warm). On CPU the model is much slower, and
  laya-codex falls back to keyword order when it misses its time budget.
- No paid end-to-end benchmark with Claude Code has measured this model yet.

## Files

`model.safetensors`, `encoder/config.json` and `tokenizer/*` (ModernBERT-base's configuration and
tokenizer), `rl_agent_config.json` (temperatures, `finetune` record, `serving` block),
`rl_agent_api.py` and `rl_common.py` (Laya's reference implementation, unchanged), and
`MANIFEST.sha256` (sha256 of each file; laya-codex's installer checks every download against it).
