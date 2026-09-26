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
Claude Code the most relevant code spans for a prompt. Lexical search finds the candidates
(tree-sitter chunks, then BM25, definitions and path matches fused into a top 24), laya-code scores
the first 16, and laya-codex ranks them by `0.5 · lexical rank score + 0.5 · P`.

This revision (the second) was trained for exactly that job: on the candidate lists the laya-codex
retriever produces, with the input the laya-codex scorer builds. The first revision
(`f3d6bd2344e4750dd917f95d40ceacfa81bb81db`) was trained on 40-line windows ranked by a separate
BM25, and in the laya-codex offline replay it tied keyword ranking (63 vs 62 gold files inlined of
115). This revision inlines 70.

## Model details

| | |
|---|---|
| Base model | `convaiinnovations/laya`, root checkpoint (revision `1c5edc17a7acd8701df6fc341c0d179f1c62c982`), Apache-2.0 |
| Warm start | the previous laya-code revision `f3d6bd2344e4750dd917f95d40ceacfa81bb81db` (itself fine-tuned from the base) |
| Architecture | unchanged: same safetensors keys, shapes and dtypes as the base (205 F16 tensors + `temperature` F32), 842,609,210 bytes |
| What changed | `model.safetensors` (fine-tuned weights) and `rl_agent_config.json` (`model_name`, `noul:2` temperature **0.8057**, was 0.9410 in the previous revision and 1.9834 in the base; `finetune` block). `encoder/config.json`, `tokenizer/*`, `rl_agent_api.py` and `rl_common.py` are byte-identical to the base. The `training` block of `rl_agent_config.json` is inherited from the base and describes the base's training, not this fine-tune. |
| Context | 512 tokens (`max_len`), 192 for the question head (`head_max_len`) |
| Runtime | Python: `rl_agent_api.RLAgent` from this repo (same as the base). Rust: `laya-model` crate of laya-codex (candle; Metal F16 on macOS, CPU F32 elsewhere), parity-tested against the Python reference with these weights (7 of 7 tests pass on CPU and Metal) |
| License | Apache-2.0 (see `LICENSE` and `NOTICE`) |

## Training data

Weak supervision from the git history of fixed commits. Nothing was hand-labelled.

- **7 training repositories** (mixed Rust, Python, TypeScript and JavaScript): openai/codex,
  TinDang97/velos, MervinPraison/PraisonAI, earendil-works/pi (`pi-mono`),
  ets-labs/python-dependency-injector, Netflix/dispatch and pilotspace/hydroa (`ai-proxy`).
  Source: `finetune/repos.py`. The first revision also used `ai-guard` (a local checkout of
  Portkey-AI/gateway); that checkout is no longer available and it was dropped.
- **Commits**: per repository, the most recent (up to 1,200) non-merge commits with one parent,
  1–8 changed source files and an informative subject, that **fix something**: a fix/bug word in the
  subject, or a body that closes an issue. codex stopped at 1,170 of its 1,200.
- **Task text**: the subject without its conventional-commit type, numeric scope or PR number; for
  half of the commits (chosen by hash) the first prose paragraph of the body is appended, so both
  short and descriptive prompts are covered.
- **Candidate lists, shaped like production**: for each commit, the parent revision is checked out
  and indexed with the laya-codex indexer (tree-sitter chunks of 10–50 lines into Moon), and the
  laya-codex retriever is run on the task through `laya-candgen` (`finetune/candgen`): the retriever
  code itself, with a scorer that records what it is handed. Each list is therefore the task focus
  and the top 24 lexical candidates in lexical order, exactly as laya-code sees them in production.
  On httpx the recorded order matches `laya-codex query` in lexical mode.
- **Labels**: a candidate overlapping a line the fix changed (`git diff -U0 -M`, old side) = 1.0;
  another chunk of a changed file = 0.6; anything else = 0 (hard negatives: lexical candidates the
  fix did not touch).
- **Size**: 4,466 lists, 106,986 candidates (2,833 changed-line chunks, 6,408 other chunks of
  changed files, 97,745 negatives). 2,599 lists have a changed chunk among their 24 candidates.
  Split by commit hash (10% validation). Training used the 2,168 training lists with a changed
  chunk among the first 16 candidates, cut to those 16 (what production scores).

  | repo | lists | with a changed chunk in the top 24 | training lists | validation lists (all / with a changed chunk) |
  |---|---|---|---|---|
  | PraisonAI | 1,200 | 753 | 627 | 123 / 82 |
  | ai-proxy | 111 | 85 | 67 | 15 / 13 |
  | codex | 1,170 | 663 | 567 | 105 / 68 |
  | dispatch | 611 | 307 | 267 | 59 / 28 |
  | pi-mono | 1,200 | 649 | 525 | 145 / 78 |
  | python-dependency-injector | 124 | 100 | 79 | 17 / 17 |
  | velos | 50 | 42 | 36 | 6 / 6 |
  | **total** | **4,466** | **2,599** | **2,168** | **470 / 292** |

### Leakage policy

Held out, never used for training, checkpoint selection or calibration: **moon, httpx and hono**
(the laya-codex benchmark repositories the replay reads) and **pilot-space** (the held-out
evaluation repository). `finetune/leakage.py` checks, and the build refuses to start otherwise:

- no training checkout inside a held-out one (or the reverse);
- no shared root commit between a training repo and a held-out repo, or between two training repos
  (forks, clones, mirrors);
- no held-out source file of 512 bytes or more vendored in a training repo at HEAD (same git blob);
- no training list from a held-out repo, from a commit that exists in a held-out repo, or with a
  task equal to a benchmark task.

The check passed on all 4,466 lists. Moon client codebases (helios, helios-mono, lunaris) stay out
of training, as before.

## Training

- Warm start from the previous laya-code revision; same input format, so the laya-codex scorer
  and its settings are unchanged.
- Input exactly as the laya-codex scorer builds it: the primary question over the retriever's task
  focus, the state tokens cut to the first 128, `[MASK]` in code blanked.
- Trainable: top 12 of 28 encoder layers, the final norm and the decision head (173M parameters);
  `act_head` frozen (unused by `noul`).
- One micro-batch = one candidate list of 16. 4 lists per update. AdamW (weight decay 0.01),
  learning rate 2e-5 for the encoder and 1e-4 for the head, 30 warm-up updates, then linear decay.
  Gradient clip 1.0.
- Loss: log loss on the two `noul` logits against the soft label (strictly proper, so the
  probabilities stay calibratable) plus a listwise softmax cross-entropy of the logit margins within
  each list (the order the production blend consumes), weight 1.
- Hardware: M4 Pro 24 GB, PyTorch MPS, fp32 with activation checkpointing (peak about 11 GB).
  550 updates (about one epoch) in 1.9 h of optimisation, about 3 s per list of 16.
- Selected checkpoint: update 540, the best on the validation monitor (292 lists): gold files among
  the first two chunks after the production blend, then the rank of the first changed chunk.

## Calibration

The `noul:2` temperature was refitted by minimum NLL on the **exported F16 weights**, over the first
16 candidates of all 470 validation lists (7,507 candidates): **T = 0.8057** (was 0.9410). NLL
0.2282 → 0.2254, ECE on hard labels 0.061 → 0.045, AUROC 0.834 (unchanged by T), mean P 0.093 for a
base rate of 0.079.

## Evaluation

### Held-out and validation lists

`finetune/eval_lists.py`: each model scored with its own configured temperature and ranked by the
production blend (0.5 lexical rank + 0.5 P over the first 16). "Gold in the first two" is the
share of changed files among the first two chunks (laya-codex inlines about two blocks per prompt).
Differences are paired bootstrap means with 95% intervals.

| set | lists | metric | lexical only | previous laya-code, blend | **this revision, blend** | difference |
|---|---|---|---|---|---|---|
| pilot-space (held out) | 45 | gold in the first two | 0.550 | 0.472 | **0.606** | +0.133 [+0.044, +0.244] |
| pilot-space (held out) | 45 | changed file ranked first | 0.339 | 0.344 | **0.539** | +0.194 [+0.072, +0.328] |
| pilot-space (held out) | 45 | AUROC (changed file vs other) | – | 0.591 | **0.859** | |
| validation (training repos) | 292 | gold in the first two | 0.343 | 0.372 | **0.484** | +0.112 [+0.077, +0.148] |
| validation (training repos) | 292 | changed file ranked first | 0.211 | 0.248 | **0.359** | +0.111 [+0.070, +0.155] |
| validation (training repos) | 292 | AUROC (changed file vs other) | – | 0.679 | **0.836** | |

### laya-codex offline replay (the release gate)

`bench/replay_decide.sh`: the 60 benchmark tasks of moon, httpx and hono replayed through the real
`laya-codex hook` (release build of laya-codex at `1d55bf3`, default settings: w = 0.5, 16 scored
candidates, 128 state tokens, 1.2 s budget), first prompt, 115 gold files. Gold inlined = gold files
whose code was inlined in the injection. The gate was run once for this revision; nothing was tuned
on it.

| arm | gold inlined (of 115) | tasks with gold inlined (of 60) | mean injected chars | hook p95 |
|---|---|---|---|---|
| keywords (lexical only) | 62 | 49 | 4,479 | 0.02–0.05 s |
| blend, previous laya-code | 63 | 48 | 4,403 | 0.53–0.56 s |
| model only, previous laya-code | 47 | 42 | 4,173 | 0.79–0.83 s |
| **blend, this revision (production)** | **70** | **52** | 4,513 | 0.53–0.56 s |
| model only, this revision | 72 | 52 | 4,624 | 0.79–0.83 s |

Per repository (blend, this revision vs previous vs keywords): hono 31 / 27 / 28 of 46, moon
21 / 17 / 18 of 34, httpx 18 / 19 / 16 of 35. The production blend inlines 11% more gold files than
before for 2.5% more injected characters. Model-only is a diagnostic; laya-codex ships the blend.
p95 ranges are across the three repositories on an M4 Pro (Metal, warmed).

## Intended use

- Re-ranking lexical (BM25) candidates of source-code chunks for a natural-language
  software-change task, as a calibrated `P(relevant)`, blended with the lexical rank as laya-codex
  does.

## Out of scope and limitations

- **Validation is in-repo.** Checkpoint selection and the temperature use a validation split of
  the training repositories (different commits, same codebases), which flatters those numbers.
- **The held-out evidence is small.** pilot-space contributes only 45 lists with a changed chunk
  among the candidates, and its intervals are wide. The replay is 60 tasks on 3 repositories, and
  the gain is not uniform: on httpx the new blend inlines one gold file fewer than the previous one.
- **Weak labels.** A fix touching a file does not make every chunk of it relevant; a relevant chunk
  the fix did not touch counts as a negative. Commits that fix something are a narrower task mix
  than what users type.
- **Low probabilities.** Mean P is about 0.09 and precision at P ≥ 0.5 is 0.24 on validation. Use
  rank or score fusion, not "P ≥ 0.5 means relevant".
- **English prompts only.** Training covered Rust, Python, TypeScript and JavaScript; other
  languages are untested.
- **Other tasks untested.** It is not a general Laya replacement: `choice`/`score` questions were not
  trained.
- **Too slow for interactive CPU use.** At 421M parameters, CPU re-ranking of 16 candidates is too
  slow for interactive use. laya-codex runs it on Metal, or falls back to lexical ranking.
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

See `MANIFEST.sha256` for the sha256 of every model file.
