"""Re-fit the noul temperature of an exported model dir on the validation split (min NLL) and write it into
rl_agent_config.json (`temperature_by_options["noul:2"]` and `temperature[2]`).

Fit on the EXPORTED weights (F16-rounded), so the temperature matches exactly what the Rust port will load.

    python3 finetune/calibrate.py --model ~/.cache/laya-codex/models/laya-code
"""
import argparse
import json
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import common  # noqa: E402

T_MIN, T_MAX = 0.05, 20.0


def probs(logits, T):
    z = np.asarray(logits, dtype=np.float64) / T
    return 1.0 / (1.0 + np.exp(-(z[:, 1] - z[:, 0])))


def nll(p, y):
    p = np.clip(p, 1e-9, 1 - 1e-9)
    return float(-(y * np.log(p) + (1 - y) * np.log(1 - p)).mean())


def fit_temperature(logits, y):
    """argmin_T NLL(softmax(logits/T), y); y may be soft. Grid over log T, then golden-section refinement."""
    y = np.asarray(y, dtype=np.float64)
    f = lambda lt: nll(probs(logits, np.exp(lt)), y)  # noqa: E731
    grid = np.linspace(np.log(T_MIN), np.log(T_MAX), 121)
    i = int(np.argmin([f(g) for g in grid]))
    lo, hi = grid[max(0, i - 1)], grid[min(len(grid) - 1, i + 1)]
    g = (np.sqrt(5) - 1) / 2
    a, b = hi - g * (hi - lo), lo + g * (hi - lo)
    for _ in range(60):
        if f(a) < f(b):
            hi = b
        else:
            lo = a
        a, b = hi - g * (hi - lo), lo + g * (hi - lo)
    return float(np.exp((lo + hi) / 2))


def report(logits, rows, T):
    from rl_common import auroc, ece_score
    y = np.array([float(r["label"]) for r in rows])
    p = probs(logits, T)
    hard = (y == 0) | (y == 1)
    hi = p >= 0.5
    return {"T": round(T, 4), "nll": round(nll(p, y), 4), "ece_hard": round(ece_score(p[hard], y[hard]), 4),
            "auroc_pos_vs_neg": round(auroc(p[hard], y[hard].astype(int)), 4), "mean_p": round(float(p.mean()), 4),
            "base_rate": round(float(y.mean()), 4),
            "prec_at_p>=0.5_hard": round(float(y[hard & hi].mean()), 4) if (hard & hi).any() else None,
            "n": int(len(y))}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default=os.path.expanduser("~/.cache/laya-codex/models/laya-code"))
    ap.add_argument("--device", default=None)
    ap.add_argument("--val", default=os.path.join(common.WORK, "val.jsonl"))
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()
    import torch
    from train import load_base_model, predict  # same loader + same input path as training
    device = torch.device(args.device or ("mps" if torch.backends.mps.is_available() else "cpu"))
    rows = [json.loads(l) for l in open(args.val)]
    model, tok, cfg, _ = load_base_model(args.model, device)
    logits = predict(model, tok, rows, device, amp=False)
    np.save(os.path.join(common.WORK, "val_logits_%s.npy" % os.path.basename(args.model.rstrip("/"))), logits)
    T_old = cfg.get("temperature_by_options", {}).get("noul:2", cfg["temperature"][2])
    T = fit_temperature(logits, np.array([float(r["label"]) for r in rows]))
    before, after = report(logits, rows, T_old), report(logits, rows, T)
    print("before", before)
    print("after ", after)
    if args.dry_run:
        return
    path = os.path.join(args.model, "rl_agent_config.json")
    cfg = json.load(open(path))
    cfg["temperature"][2] = T
    cfg.setdefault("temperature_by_options", {})["noul:2"] = T
    cfg["model_name"] = "laya-code"
    cfg["finetune"] = {"base": "laya-base (convaiinnovations/laya)", "task": "code relevance (noul)",
                       "noul_temperature_fit": {"split": "val", "before": before, "after": after}}
    tmp = path + ".tmp"
    json.dump(cfg, open(tmp, "w"), indent=2)
    os.replace(tmp, path)
    print("wrote T=%.4f to %s" % (T, path))


if __name__ == "__main__":
    main()
