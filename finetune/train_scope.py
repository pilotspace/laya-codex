"""Fine-tune laya-code on task-scope `choice` data mixed with its noul relevance data (replay), -> laya-code-v2.

Each micro-batch = `--noul-per-batch` noul relevance items (v2 data, stratified like train.py) + scope items
(tempered class-balanced sampling, weight per class ~ count**alpha), so >= 50% of sequences are noul replay.
Scope wordings trained: scope.WORDINGS[w] for w in --wordings (uniform), option order fixed = scope.CLASSES
(exactly the inference layout). Loss: log loss over each item's own options (masked logits).
Selection: lowest scope val NLL among checkpoints whose noul monitor AUROC >= start - --max-auroc-drop.

    python3 finetune/train_scope.py --init ~/.cache/laya-codex/models/laya-code --max-hours 1.0
"""
import argparse
import json
import math
import os
import random
import sys
import time

import numpy as np
import torch
import torch.nn.functional as F

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import common  # noqa: E402
import scope  # noqa: E402
from rl_common import QTYPES, auroc, build_sequence, collate_items  # noqa: E402
from train import (StratifiedSampler, atomic_save, enable_checkpointing, load_base_model, load_rows,  # noqa: E402
                   make_items, predict, set_trainable, trainable_names, trainable_state, val_metrics)

CKPT_DIR = os.environ.get("LAYA_SCOPE_CKPT", os.path.join(common.WORK, "ckpt_scope"))
K = len(scope.CLASSES)


def mixed_loss(logits, target):
    """logits [n, kmax] already masked (-1e4) outside each item's options; target zero-padded -> mean log loss."""
    per = -(target * F.log_softmax(logits.float(), -1)).sum(-1)
    return per.mean(), per


def class_sampling_weights(labels, alpha=0.5):
    cnt = {c: labels.count(c) for c in set(labels)}
    return [cnt[c] ** alpha / cnt[c] for c in labels]


def scope_items(tok, rows, wordings, rng, cfg):
    items = []
    for r in rows:
        w = scope.WORDINGS[rng.choice(wordings)]
        ids, markers = build_sequence(tok, scope.state_text(w, r["task"], r["repo"]), scope.question(w, r["task"]),
                                      cfg["max_len"], cfg["head_max_len"])
        if len(markers) != K:
            continue
        y = scope.CLASSES.index(r["label"])
        items.append({"ids": ids, "markers": markers, "qtype": QTYPES["choice"], "target": [float(i == y) for i in range(K)],
                      "label": y, "episode": 0, "ep_step": 0, "ep_len": 1, "src": r["repo"]})
    return items


@torch.no_grad()
def scope_logits(model, tok, rows, wording, device, cfg, bs=32):
    was = model.training
    model.train(False)
    out = []
    for s in range(0, len(rows), bs):
        its = scope_items(tok, rows[s:s + bs], [wording], random.Random(0), cfg)
        b = collate_items([its], tok.pad_token_id)
        lg, _ = model(b["input_ids"].to(device), b["attention_mask"].to(device), b["marker_pos"].to(device),
                      b["marker_mask"].to(device), b["qtype"].to(device))
        out.append(lg.float().cpu().numpy()[:, :K])
    model.train(was)
    return np.concatenate(out)


def scope_val(model, tok, rows, wordings, device, cfg):
    from scope_eval import metrics, softmax
    res = {}
    for w in wordings:
        m = metrics(softmax(scope_logits(model, tok, rows, w, device, cfg), 1.0), rows)
        res[w] = {k: m[k] for k in ("accuracy", "macro_f1", "nll", "spearman_expected_idx_vs_n_src")}
    res["nll_mean"] = round(float(np.mean([res[w]["nll"] for w in wordings])), 4)
    res["macro_f1_mean"] = round(float(np.mean([res[w]["macro_f1"] for w in wordings])), 4)
    return res


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--init", default=os.path.expanduser("~/.cache/laya-codex/models/laya-code"))
    ap.add_argument("--device", default="mps" if torch.backends.mps.is_available() else "cpu")
    ap.add_argument("--top-layers", type=int, default=8)
    ap.add_argument("--micro", type=int, default=8)
    ap.add_argument("--noul-per-batch", type=int, default=4)
    ap.add_argument("--accum", type=int, default=4)
    ap.add_argument("--lr-enc", type=float, default=1e-5)
    ap.add_argument("--lr-head", type=float, default=5e-5)
    ap.add_argument("--warmup", type=int, default=30)
    ap.add_argument("--steps", type=int, default=2000)
    ap.add_argument("--max-hours", type=float, default=1.0)
    ap.add_argument("--alpha", type=float, default=0.5)
    ap.add_argument("--wordings", nargs="+", default=["w1_amount", "w3_scope"])
    ap.add_argument("--eval-every", type=int, default=100)
    ap.add_argument("--ckpt-every", type=int, default=50)
    ap.add_argument("--noul-val-n", type=int, default=600)
    ap.add_argument("--max-auroc-drop", type=float, default=0.01)
    ap.add_argument("--seed", type=int, default=21)
    ap.add_argument("--resume", action="store_true")
    args = ap.parse_args()
    assert 2 * args.noul_per_batch >= args.micro, "noul replay must be >= 50% of each micro-batch"
    torch.manual_seed(args.seed)
    os.makedirs(CKPT_DIR, exist_ok=True)
    device = torch.device(args.device)
    model, tok, cfg, _ = load_base_model(args.init, device)
    enc_p, head_p = set_trainable(model, args.top_layers)
    enable_checkpointing(model)
    opt = torch.optim.AdamW([{"params": enc_p, "lr": args.lr_enc, "weight_decay": 0.01},
                             {"params": head_p, "lr": args.lr_head, "weight_decay": 0.01}])
    base_lrs = [args.lr_enc, args.lr_head]

    noul_train = load_rows(os.path.join(common.WORK, "train.jsonl"))
    noul_val = random.Random(7).sample(load_rows(os.path.join(common.WORK, "val.jsonl")), args.noul_val_n)
    sc_dir = os.path.join(common.WORK, "scope")
    sc_train = load_rows(os.path.join(sc_dir, "train.jsonl"))
    sc_val = load_rows(os.path.join(sc_dir, "val.jsonl"))
    n = args.noul_per_batch
    comp = {"pos": max(1, round(n * 0.125)), "soft": max(1, round(n * 0.25))}
    comp["neg"] = n - comp["pos"] - comp["soft"]
    nsampler = StratifiedSampler(noul_train, comp, args.seed)
    sw = class_sampling_weights([r["label"] for r in sc_train], args.alpha)
    rng = random.Random(args.seed + 1)
    step, log, best, secs, start_auc = 0, [], None, 0.0, None
    last = os.path.join(CKPT_DIR, "last.pt")
    if args.resume and os.path.exists(last):
        ck = torch.load(last, map_location="cpu", weights_only=False)
        assert set(ck["params"]) == set(trainable_names(model))
        with torch.no_grad():
            for nm, p in model.named_parameters():
                if nm in ck["params"]:
                    p.copy_(ck["params"][nm].to(device))
        opt.load_state_dict(ck["opt"])
        nsampler.load_state_dict(ck["sampler"])
        rng.setstate(ck["rng"])
        step, log, best, secs, start_auc = ck["step"], ck["log"], ck["best"], ck["secs"], ck["start_auc"]
        print("resumed at step %d" % step, flush=True)
    else:
        nm0 = val_metrics(predict(model, tok, noul_val, device, False, bs=16), noul_val)
        sv0 = scope_val(model, tok, sc_val, args.wordings, device, cfg)
        start_auc = nm0["auroc_pos_vs_neg"]
        log.append({"step": 0, "noul": nm0, "scope": sv0})
        print("step 0 noul %s\n       scope %s" % (nm0, sv0), flush=True)
    progress = lambda s, t: max(s / args.steps, t / (args.max_hours * 3600))  # noqa: E731
    finished = progress(step, secs) >= 1.0
    model.train()
    t0, ema = time.time(), None
    while not finished:
        ts = time.time()
        sc = 1.0 if step < args.warmup else max(0.1, 1.0 - 0.9 * progress(step, secs))
        if step < args.warmup:
            sc = (step + 1) / args.warmup
        for g, lr in zip(opt.param_groups, base_lrs):
            g["lr"] = lr * sc
        tot = 0.0
        parts = {"noul": 0.0, "scope": 0.0}
        for _ in range(args.accum):
            nrows = [noul_train[i] for i in nsampler.batch()]
            srows = rng.choices(sc_train, weights=sw, k=args.micro - n)
            items = make_items(tok, nrows, rng) + scope_items(tok, srows, args.wordings, rng, cfg)
            b = collate_items([items], tok.pad_token_id)
            lg, _ = model(b["input_ids"].to(device), b["attention_mask"].to(device), b["marker_pos"].to(device),
                          b["marker_mask"].to(device), b["qtype"].to(device))
            loss, per = mixed_loss(lg, b["target"].to(device))
            (loss / args.accum).backward()
            tot += loss.item() / args.accum
            isn = (b["qtype"] == QTYPES["noul"]).cpu()
            parts["noul"] += per.detach().cpu()[isn].mean().item() / args.accum
            parts["scope"] += per.detach().cpu()[~isn].mean().item() / args.accum
        if not math.isfinite(tot):
            opt.zero_grad(set_to_none=True)
            step += 1
            continue
        torch.nn.utils.clip_grad_norm_(enc_p + head_p, 1.0)
        opt.step()
        opt.zero_grad(set_to_none=True)
        step += 1
        secs += time.time() - ts
        finished = progress(step, secs) >= 1.0
        ema = tot if ema is None else 0.98 * ema + 0.02 * tot
        if step % 25 == 0:
            print("step %d loss %.4f (noul %.3f scope %.3f) ema %.4f lr %.2e | %.1fs/step progress %.3f" % (
                step, tot, parts["noul"], parts["scope"], ema, opt.param_groups[0]["lr"], (time.time() - t0) / 25,
                progress(step, secs)), flush=True)
            t0 = time.time()
        if step % args.eval_every == 0 or finished:
            if device.type == "mps":
                torch.mps.empty_cache()
            nm = val_metrics(predict(model, tok, noul_val, device, False, bs=16), noul_val)
            sv = scope_val(model, tok, sc_val, args.wordings, device, cfg)
            if device.type == "mps":
                torch.mps.empty_cache()
            ok = nm["auroc_pos_vs_neg"] >= start_auc - args.max_auroc_drop
            log.append({"step": step, "noul": nm, "scope": sv, "noul_ok": ok})
            print("step %d noul %s ok=%s\n       scope %s" % (step, nm, ok, sv), flush=True)
            if ok and (best is None or sv["nll_mean"] < best["scope_nll"]):
                best = {"step": step, "scope_nll": sv["nll_mean"], "scope_macro_f1": sv["macro_f1_mean"],
                        "noul_auroc": nm["auroc_pos_vs_neg"]}
                atomic_save({"params": trainable_state(model), "step": step, "best": best}, os.path.join(CKPT_DIR, "best.pt"))
            t0 = time.time()
        if step % args.ckpt_every == 0 or finished:
            atomic_save({"params": trainable_state(model), "opt": opt.state_dict(), "sampler": nsampler.state_dict(),
                         "rng": rng.getstate(), "step": step, "log": log, "best": best, "secs": secs,
                         "start_auc": start_auc, "args": vars(args)}, last)
    json.dump({"log": log, "best": best, "args": vars(args)}, open(os.path.join(CKPT_DIR, "train_log.json"), "w"), indent=1)
    print("done. best %s" % best, flush=True)


if __name__ == "__main__":
    main()
