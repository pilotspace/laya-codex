"""Per-negative-kind AUROC of a training checkpoint on a val sample (sanity check: is discrimination learned?).

    python3 finetune/diagnose.py --ckpt ~/.cache/laya-codex/finetune/ckpt/best.pt --n 240 --device cpu
"""
import argparse
import json
import os
import random
import sys

import numpy as np
import torch

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import common  # noqa: E402
from rl_common import auroc  # noqa: E402
from train import load_base_model, predict  # noqa: E402


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ckpt", default=None)
    ap.add_argument("--n", type=int, default=240)
    ap.add_argument("--device", default="cpu")
    args = ap.parse_args()
    rows = [json.loads(l) for l in open(os.path.join(common.WORK, "val.jsonl"))]
    rng = random.Random(3)
    by = {k: [r for r in rows if r["kind"] == k] for k in ("pos", "hard_neg", "rand_neg", "same_file")}
    sample = sum((rng.sample(v, min(len(v), args.n // 4)) for v in by.values()), [])
    device = torch.device(args.device)
    model, tok, _, _ = load_base_model(common.BASE_MODEL, device)
    if args.ckpt:
        ck = torch.load(args.ckpt, map_location="cpu", weights_only=False)
        with torch.no_grad():
            for n, p in model.named_parameters():
                if n in ck["params"]:
                    p.copy_(ck["params"][n].to(device))
        print("loaded step", ck.get("step"))
    z = predict(model, tok, sample, device, amp=False, bs=16)
    s = z[:, 1] - z[:, 0]
    kinds = np.array([r["kind"] for r in sample])
    for neg in ("hard_neg", "rand_neg", "same_file"):
        m = (kinds == "pos") | (kinds == neg)
        print("pos vs %-9s AUROC %.3f" % (neg, auroc(s[m], (kinds[m] == "pos").astype(int))))
    for k in by:
        print("%-9s mean margin %.3f sd %.3f" % (k, s[kinds == k].mean(), s[kinds == k].std()))


if __name__ == "__main__":
    main()
