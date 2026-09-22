"""Write MODEL_CARD.md for an exported laya-code dir from the recorded artifacts (no hand-typed numbers).

    python3 finetune/model_card.py --model ~/.cache/laya-codex/models/laya-code --eval spike/results/finetune_eval.json
"""
import argparse
import json
import os
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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default=os.path.expanduser("~/.cache/laya-codex/models/laya-code"))
    ap.add_argument("--eval", required=True)
    ap.add_argument("--verdict", default="")
    args = ap.parse_args()
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
