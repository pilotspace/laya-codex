"""Write a laya-code model card from the recorded artifacts (no hand-typed numbers).

The first laya-code (a Laya fine-tune) from its model dir and eval results:

    python3 finetune/model_card.py --model ~/.cache/laya-codex/models/laya-code --eval spike/results/finetune_eval.json

A student run (ModernBERT-base, e.g. laya-code-r2) from its run manifest alone, as the Hugging Face README:

    python3 finetune/model_card.py --run finetune/runs/r2.json --out release/hf-laya-code/README.md
"""
import argparse
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import common  # noqa: E402
from repos import HELDOUT, TRAIN  # noqa: E402


def table(res):
    lines = ["| repo | model | row | P@10 | Hit@10 | MRR | R@10 |", "|---|---|---|---|---|---|---|"]
    for repo, r in res.items():
        for m, s in r["models"].items():
            for row in ("bm25", "laya", "rrf", "gate"):
                if row == "bm25" and not m.startswith("laya-base@256"):
                    continue
                v = s[row]
                lines.append("| %s | %s | %s | %.3f | %.3f | %.3f | %.3f |" % (repo, m, row, v["P@10"], v["Hit@10"], v["MRR"], v["R@10"]))
    return "\n".join(lines)


def calib_table(res):
    lines = ["| repo | model | T | base rate | mean P | n P>=0.5 | prec@P>=0.5 | recall@P>=0.5 | ECE | AUROC pooled | AUROC/task |",
             "|---|---|---|---|---|---|---|---|---|---|---|"]
    for repo, r in res.items():
        for m, s in r["models"].items():
            c = s["calibration"]
            lines.append("| %s | %s | %.3f | %.3f | %.3f | %d | %s | %s | %.3f | %.3f | %s |" % (
                repo, m, s["noul_temperature"], c["base_rate"], c["mean_p"], c["n_p>=0.5"], c["prec_at_p>=0.5"],
                c["recall_at_p>=0.5"], c["ece_15bin"], c["auroc_pooled"], c["auroc_per_task_mean"]))
    return "\n".join(lines)


PENDING = "pending"


def _pct(x):
    return "%.1f%%" % (100.0 * x)


def student_card(run):
    """The Hugging Face README of a student run, from its manifest (finetune/runs/<run>.json) alone.

    Raises KeyError when the manifest lacks a section the card reports (the replay, the serving block, ...)."""
    m, tr, rp = run["model"], run["training"], run["replay"]
    init, serving = m["init"], m["serving"]
    wiring = rp["default_wiring"]
    r2, r1 = wiring["laya-code-r2"], wiring["laya-code-r1"]
    screen = rp["g1_screen"]
    kw = screen["keywords"]
    lat = rp["latency_side_by_side"]["student_w128_k12"]
    sel = str(int(tr["selected"].split()[1].rstrip(":")))
    val = tr["validation_monitor"][sel]
    r1val = tr["r1_on_the_same_validation_lists"]
    name = m["exported"]
    rev = run["published"]["revision"]
    published = not rev.startswith(PENDING)
    train_repos = run["repos"]["train"]
    held = run["repos"]["heldout"]
    lists = sum(f["lists"] for k, f in run["data"]["files"].items() if not k.startswith("heldout-"))
    ece = re.search(r"ECE [\d.]+ -> ([\d.]+)", run["finalize"]["results"]["temperature"]).group(1)
    W = serving["state_tokens"]
    K = serving["score_top"]
    if published:
        where = ("This model is commit `%s` on the `r2` branch of `tindang/laya-code`. laya-codex's installer pins "
                 "that commit and the sha256 of its `MANIFEST.sha256` (`%s`). To download it by hand: "
                 "`hf download tindang/laya-code --revision %s --local-dir <dir>`." % (rev, m["manifest_sha256"], rev))
    else:
        where = ("This model goes on the `r2` branch of `tindang/laya-code`; it is not published yet. laya-codex's "
                 "installer pins the sha256 of its `MANIFEST.sha256` (`%s`) and will pin the upload commit." % m["manifest_sha256"])
    screen_rows = "\n".join(
        "| %s | %d of %d | %s | %d ms |" % (k, v["gold_inlined"], v["gold_total"], format(v["mean_injected_chars"], ","),
                                             v["hook_ms_p50"])
        for k, v in screen.items())
    val_rows = "\n".join(
        "| %s | %.3f | %.3f | %.3f |" % (w, v["auroc"], v["blend_top2"], v["model_only_top2"]) for w, v in val.items())
    return f"""---
license: apache-2.0
base_model: {init['encoder']}
base_model_relation: finetune
pipeline_tag: text-classification
language: [en]
tags: [laya, code-search, reranker, code-retrieval, calibrated, claude-code, laya-codex, modernbert]
---

# {name}

A code-relevance re-ranker for [laya-codex](https://github.com/pilotspace/laya-codex): the
[{init['encoder']}](https://huggingface.co/{init['encoder']}) encoder with the typed-decision head of
[Laya](https://huggingface.co/convaiinnovations/laya), trained for one job. Given a task and a
source-code chunk, it answers one yes/no (`noul`) question with a calibrated probability:

```
question: Is this source code relevant to the software change: "{{task}}"?
state:    file: <path> (lines a-b)\\n<code>        (cut to {W} tokens when laya-codex serves it)
```

laya-codex finds 24 candidates lexically (tree-sitter chunks, BM25, definitions and path matches),
this model scores the top {K}, and laya-codex ranks them by `0.5 · lexical rank score + 0.5 · P`. It
replaces laya-code-r1 (ModernBERT-large, branch `r1`) as laya-codex's default: in the offline
replay it inlines **{r2['gold_inlined']} of {r2['gold_total']}** gold files against
{r1['gold_inlined']} for r1, with the hook at **{r2['hook_ms_p50']} ms** instead of {r1['hook_ms_p50']} ms (p50).

**Where it is published.** {where} The `main` branch (the first laya-code) and the `r1` branch are
unchanged.

## Model details

| | |
|---|---|
| Encoder | `{init['encoder']}`, revision `{init['encoder_revision']}`, pretrained weights (Apache-2.0) |
| Decision head | Laya's, from its reference code `rl_common.py`; no Laya weights. Initialisation: {init['head']} |
| Architecture | {m['architecture']}; {init['tensors']} tensors (F16, `temperature` F32), {format(m['model.safetensors_bytes'], ',')} bytes |
| Context | {m['max_len']} tokens (`max_len`) |
| Calibration | `noul:2` temperature **{m['noul_temperature']:.4f}**, fitted at the {W}-token window it is served at |
| Serving | `serving` block of `rl_agent_config.json`: {serving['state_tokens']} tokens per candidate, top {serving['score_top']} scored per prompt, {serving['search_score_top']} per search. laya-codex reads it; `LAYA_CODEX_STATE_TOKENS` and `LAYA_CODEX_SCORE_TOP` override it |
| Runtime | Python: `rl_agent_api.RLAgent` from this repo. Rust: the `laya-model` crate of laya-codex (candle; Metal F16 on macOS, CPU F32 elsewhere), parity-tested against the Python reference: {run['finalize']['results']['parity']} |
| License | Apache-2.0 (see `LICENSE` and `NOTICE`) |

## Training data

The candidate lists laya-code-r1 was trained on, unchanged ({lists:,} lists). Weak supervision from
the git history of fixed commits; nothing was hand-labelled.

- **{len(train_repos)} training repositories:** {", ".join("%s (`%s`)" % (v['github'], k) for k, v in train_repos.items())}.
- **Lists shaped like production:** for each fixing commit, the parent revision is indexed with the
  laya-codex indexer and its retriever produces the task focus and the top 24 lexical candidates,
  exactly as the re-ranker sees them in laya-codex.
- **Labels:** a candidate overlapping a line the fix changed = 1.0; another chunk of a changed file = 0.6;
  anything else = 0.
- **Held out** (never used for training, checkpoint selection or calibration): {", ".join("%s (`%s`)" % (v['github'], k) for k, v in held.items())}.
  `finetune/leakage.py` checks for shared roots, vendored files and benchmark tasks; the check passed.

## Training

- Distillation: {tr['distillation']}
- Windows: {tr['windows']}.
- Trainable: the top `--top-layers` encoder layers (every layer here) and the decision head. Command:
  `{tr['command']}`.
- Updates: {tr['updates']}.
- Selected: {tr['selected']}.
- {tr['hardware']}; {tr['optimisation_seconds'] / 3600:.1f} h of optimisation.

Validation at the selected update (in-repository validation lists of the training repositories,
gold = a changed chunk; blend = the production ranking):

| window | AUROC | blend top-2 gold | model-only top-2 gold |
|---|---|---|---|
{val_rows}

laya-code-r1 on the same lists at {r1val['window']} tokens: AUROC {r1val['auroc']:.3f}, blend top-2 gold {r1val['blend_top2']:.3f}.

Calibration: {run['finalize']['results']['temperature']}.

## Offline replay (held-out benchmark repositories)

{rp['protocol']}. The 238 gold files are in moon, httpx and hono, which training never saw.

Through the default wiring ({wiring['binary']}):

| model | gold inlined | gold in the top 2 | mean injected chars | hook p50 | hook p90 | scored per prompt |
|---|---|---|---|---|---|---|
| {name} | {r2['gold_inlined']} of {r2['gold_total']} | {r2['gold_in_top2']} | {format(r2['mean_injected_chars'], ',')} | {r2['hook_ms_p50']} ms | {r2['hook_ms_p90']} ms | {", ".join(r2['scored'])} |
| laya-code-r1 | {r1['gold_inlined']} | {r1['gold_in_top2']} | {format(r1['mean_injected_chars'], ',')} | {r1['hook_ms_p50']} ms | {r1['hook_ms_p90']} ms | {", ".join(r1['scored'])} |
| keywords only | {kw['gold_inlined']} | {kw['gold_in_top2']} | {format(kw['mean_injected_chars'], ',')} | {kw['hook_ms_p50']} ms | {kw['hook_ms_p90']} ms | 0 |

Every prompt ran in `laya` mode for both models. The screen that chose the setting (the student at
other windows and candidate counts, under a tighter time budget; r1 and keywords as served):

| arm | gold inlined | mean injected chars | hook p50 |
|---|---|---|---|
{screen_rows}

Scoring 24 candidates found the most ({screen['nokd-best-w128-k24']['gold_inlined']}), but {K} at {W}
tokens keeps nearly all of it at about half the time ({lat['gold_inlined']} of 238, hook
{lat['hook_ms_p50']:.0f} ms p50 and {lat['hook_ms_p90']:.0f} ms p90, arms rotated on one warm index), so
laya-codex serves {W} × {K}.

## Intended use

- Re-ranking lexical code-search candidates for a natural-language change request, inside
  laya-codex (or with its scorer's exact input: the question above over `file: <path> (lines a-b)`
  and the chunk, cut to {W} tokens).
- Its probabilities are meant to be blended with the lexical order, not used alone as a relevance
  threshold.

Out of scope: general text classification, retrieval over whole repositories without a candidate
stage, non-code documents, and safety-relevant decisions.

## Limits

- The validation lists come from the training repositories (other commits); the only held-out test
  is the replay above: 120 tasks, 238 gold files, three repositories. laya-codex's pipeline was
  tuned on one of the two task sets (tasks-v8).
- Labels are weak: a commit changing a file does not make every chunk of it relevant.
- Calibration is only moderate: hard ECE {ece} after fitting the temperature, on the validation lists. Treat P as a ranking score
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
"""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default=os.path.expanduser("~/.cache/laya-codex/models/laya-code"))
    ap.add_argument("--eval", default=None, help="eval results of the first laya-code (its card)")
    ap.add_argument("--verdict", default="")
    ap.add_argument("--run", default=None, help="a student run manifest (finetune/runs/r2.json): writes its HF README")
    ap.add_argument("--out", default=None, help="with --run: the README path")
    args = ap.parse_args()
    if args.run:
        out = args.out or os.path.join(os.path.dirname(os.path.abspath(args.run)), "README.md")
        with open(out, "w") as f:
            f.write(student_card(json.load(open(args.run))))
        print("wrote", out)
        return
    if not args.eval:
        ap.error("--eval is required without --run")
    cfg = json.load(open(os.path.join(args.model, "rl_agent_config.json")))
    stats = json.load(open(os.path.join(common.WORK, "data_stats.json")))
    tlog = json.load(open(os.path.join(common.WORK, "ckpt", "train_log.json")))
    ev = json.load(open(args.eval))
    fit = cfg["finetune"]["noul_temperature_fit"]
    a = tlog["args"]
    t = stats["totals"]
    md = f"""# laya-code

Code-relevance judge for laya-codex: answers the noul question
`{common.QUESTIONS[0]}` over a state `file: <path> (lines a-b)\\n<code>` (state truncated to {common.STATE_BUDGET} tokens),
with a calibrated probability.

Fine-tuned from **laya-base** (convaiinnovations/laya: ModernBERT-large encoder + typed-decision head).
**Same architecture, same safetensors keys, shapes and dtypes as laya-base** (205 tensors F16 + `temperature` F32),
so the candle port loads it unchanged. Only the weights and the `noul:2` temperature differ.

## Files
`model.safetensors` (F16 + F32 temperature), `encoder/config.json`, `tokenizer/`, `rl_agent_config.json`
(`temperature[2]` = `temperature_by_options["noul:2"]` = **{cfg['temperature'][2]:.4f}**; laya-base was 1.9834),
`rl_agent_api.py`, `rl_common.py` (reference implementation, unchanged).

## Data (weak supervision from git history)
- Train repos ({len(TRAIN)}): {", ".join(sorted(TRAIN))}. Mixed Rust / Python / TypeScript / JS.
- Held out (never used for training or calibration): {", ".join(sorted(HELDOUT))}. Moon-client repos
  (helios, helios-mono, lunaris) excluded to avoid domain leakage into the moon eval; root-commit check rejects forks/clones.
- Per non-merge commit touching 1-4 source files with an informative subject (>= 20 chars):
  task = subject (+ short first body line). Windows = 40 lines, stride 30 (same as the eval corpus), state truncated
  to 256 tokens. **Candidate list** (what inference re-ranks): BM25 top-{stats['cand_k']} over other files (snapshot index
  at an ancestor revision) merged with the **parent-revision** windows of the touched files, scored with the same BM25
  statistics; labels: window whose visible part overlaps the old-side lines of `git diff -U0` = 1.0, other window of a
  touched file = {stats['soft_label_cand_file']} (file-level relevant), other file = 0. Plus hunk windows BM25 missed
  (label 1.0, <= {stats['max_pos_per_commit']}), one random non-overlapping same-file window
  (soft {stats['soft_label_same_file']}), 2 random other-file windows (0).
- Pairs: **{t['train'] + t['val']}** total = train {t['train']} (cand-pos {t['train_cand_pos']}, cand-file {t['train_cand_file']},
  cand-neg {t['train_cand_neg']}, extra pos {t['train_pos']}, same-file {t['train_same_file']}, rand-neg {t['train_rand_neg']})
  + val {t['val']} (split by commit hash, 10%). The v1 dataset (44,036 pairs: hunk positives vs BM25 hard negatives
  from other files) was used for the first 300 updates (see training history).
- Questions: primary inference question 60%, two paraphrases 20% each (robustness).

## Training
- Hardware: M4 Pro 24 GB (shared with other jobs), PyTorch MPS, **fp32** (bf16 autocast on MPS was slower and
  numerically loose; fp16 crashes an MPS matmul).
- Trainable: top {a['top_layers']} of 28 encoder layers + final norm + decision head + type_emb + scorer
  (act_head frozen, unused by noul). Activation checkpointing.
- AdamW (wd 0.01), lr encoder {a['lr_enc']}, head {a['lr_head']}, warmup {a['warmup']}, linear decay to 10% over a
  {a['max_hours']} h wall-clock budget; micro-batch {a['micro']} x accum {a['accum']} = {a['micro'] * a['accum']} sequences/update;
  stratified batches with fixed class composition (natural mix); grad clip 1.0.
- Loss: log loss (strictly proper) on the 2 noul logits vs soft target.
- Selected checkpoint: step {tlog['best']['step']} (lowest val NLL on a {a['val_n']}-row monitor subset).
  Val monitor log: {json.dumps([dict(step=e['step'], **{k: e['val'][k] for k in ('nll', 'auroc_pos_vs_neg')}) for e in tlog['log']])}

## Calibration (val split, {fit['after']['n']} pairs, fitted on the exported F16 weights)
- before (T={fit['before']['T']}): NLL {fit['before']['nll']}, ECE {fit['before']['ece_hard']}, mean P {fit['before']['mean_p']}
- after  (T={fit['after']['T']}): NLL {fit['after']['nll']}, ECE {fit['after']['ece_hard']}, mean P {fit['after']['mean_p']},
  AUROC pos-vs-neg {fit['after']['auroc_pos_vs_neg']}, precision at P>=0.5 {fit['after']['prec_at_p>=0.5_hard']} (base rate {fit['after']['base_rate']})

## Held-out evaluation (spike protocol, `finetune/eval.py`; results in `spike/results/finetune_eval.json`)
40 most recent qualifying commits per repo; corpus = 40-line windows at HEAD; candidates = BM25 top-{ev['protocol']['k']};
gold = files touched by the commit (file-level). `@256` = state truncated to 256 tokens (production budget),
`@full` = spike behaviour (state fills the 512-token context).

{table(ev['results'])}

Calibration over all candidates (file-level labels):

{calib_table(ev['results'])}

{args.verdict}

## Limits
- Labels are weak: a commit touching a file does not mean every window of it matters; file-level eval gold is coarse.
- Eval corpus is at HEAD (after the commits), as in the spike; the same bias applies to every row.
- Trained for about half an epoch because of the shared-machine budget; more steps would likely help.
"""
    open(os.path.join(args.model, "MODEL_CARD.md"), "w").write(md)
    print("wrote", os.path.join(args.model, "MODEL_CARD.md"))


if __name__ == "__main__":
    main()
