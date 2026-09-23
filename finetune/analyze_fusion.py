"""Why does fusion move the way it does? Uses eval.py score caches (no model): per repo, for each cached model,
Spearman(Laya score, BM25 rank), first-gold rank under BM25 / Laya / RRF, and alternative fusions
(RRF with different k, weighted RRF, BM25-score x P).

    python3 finetune/analyze_fusion.py --repo moon=<moon checkout> --models laya-base probe-s150
"""
import argparse
import json
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(os.path.dirname(HERE), "spike"))
from eval import CACHE, candidates  # noqa: E402
from rl_common import spearman  # noqa: E402


def rr(order, chunks, gold):
    return next((1.0 / (r + 1) for r, c in enumerate(order[:10]) if chunks[c]["path"] in gold), 0.0)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    ap.add_argument("--models", nargs="+", required=True)
    ap.add_argument("--budget", default="256")
    args = ap.parse_args()
    rname, rpath = args.repo.split("=", 1)
    chunks, tasks = candidates(os.path.expanduser(rpath), 40, 32)
    for m in args.models:
        sc = json.load(open(os.path.join(CACHE, "%s_%s_%s.json" % (m, rname, args.budget))))["scores"]
        rows, rhos = [], []
        for t, s in zip(tasks, sc):
            cand, p, gold = t["cand"], np.array(s["p"]), set(t["gold"])
            raw = np.array(s["raw"])
            margin = raw[:, 1] - raw[:, 0]
            rhos.append(spearman(-np.arange(len(cand)), margin))
            lorder = [cand[i] for i in np.argsort(-margin, kind="stable")]
            lr = {c: r for r, c in enumerate(lorder)}
            out = [rr(cand, chunks, gold), rr(lorder, chunks, gold)]
            for k, w in ((60, 1.0), (10, 1.0), (60, 2.0), (60, 0.5)):
                f = {c: 1 / (k + r) + w / (k + lr[c]) for r, c in enumerate(cand)}
                out.append(rr(sorted(cand, key=lambda c: -f[c]), chunks, gold))
            rows.append(out)
        a = np.array(rows).mean(0)
        print("%-12s %-8s rho(laya,bm25)=%.3f  MRR bm25 %.3f laya %.3f | rrf60 %.3f rrf10 %.3f rrf60 w2 %.3f rrf60 w.5 %.3f" % (
            m, rname, np.nanmean(rhos), *a))


if __name__ == "__main__":
    main()
