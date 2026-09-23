"""Compare Laya checkpoints as BM25 re-rankers on the same moon tasks (zero-shot or fine-tuned).

Same corpus/tasks/BM25 as spike/laya_spike.py; the chunk state is truncated to --state-tokens
tokens, matching production (128). Reports ranking (Laya-only, RRF, weighted 0.5), AUROC over
all candidates, calibration, and latency.

    python3 spike/compare_models.py --repo <moon> --models base=<dir> typed=<dir> code=<dir> --out spike/results/compare.json
"""
import argparse
import json
import sys
import time
from collections import defaultdict

import numpy as np
import torch

sys.path.insert(0, __file__.rsplit("/", 1)[0])
from laya_spike import BM25, load_corpus, load_tasks, metrics, tokenize  # noqa: E402

QUESTION = "Is this source code relevant to the software change: \"{task}\"?"


class Model:
    def __init__(self, model_dir, device, state_tokens):
        sys.path.insert(0, model_dir)
        for mod in ("rl_agent_api", "rl_common"):  # each checkpoint ships its own copy; reload per dir
            sys.modules.pop(mod, None)
        from rl_agent_api import RLAgent
        self.agent = RLAgent(model_dir, device=device)
        sys.path.remove(model_dir)
        self.state_tokens = state_tokens

    def score(self, task, chunks):
        from rl_common import QTYPES, build_sequence, collate_items, temp_bucket
        a = self.agent
        q = {"t": "noul", "ins": QUESTION.format(task=task), "crit": None}
        items = []
        for c in chunks:
            state = "file: %s (lines %d-%d)\n%s" % (c["path"], c["start"], c["end"], c["text"])
            ids = a.tok(state, add_special_tokens=False)["input_ids"][: self.state_tokens]
            state = a.tok.decode(ids)
            seq, markers = build_sequence(a.tok, state, q, a.cfg["max_len"], a.cfg["head_max_len"])
            items.append({"ids": seq, "markers": markers, "qtype": QTYPES["noul"], "target": [0.0, 0.0], "label": -1,
                          "episode": 0, "ep_step": 0, "ep_len": 1, "src": "cmp"})
        b = collate_items([items], a.tok.pad_token_id)
        d = a.device
        with torch.no_grad():
            logits, _ = a.model(b["input_ids"].to(d), b["attention_mask"].to(d), b["marker_pos"].to(d),
                                b["marker_mask"].to(d), b["qtype"].to(d))
        t = a.temperature_by_options.get(temp_bucket(2, 2), a.temperature[2])
        z = logits.float().cpu().numpy()[:, :2] / t
        p = np.exp(z - z.max(1, keepdims=True))
        return (p / p.sum(1, keepdims=True))[:, 1]


def auroc(scores, labels):
    pos, neg = scores[labels == 1], scores[labels == 0]
    if not len(pos) or not len(neg):
        return float("nan")
    return float((pos[:, None] > neg[None, :]).mean() + 0.5 * (pos[:, None] == neg[None, :]).mean())


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    ap.add_argument("--models", nargs="+", required=True, help="name=dir")
    ap.add_argument("--n", type=int, default=40)
    ap.add_argument("--k", type=int, default=24)
    ap.add_argument("--state-tokens", type=int, default=128)
    ap.add_argument("--device", default="mps" if torch.backends.mps.is_available() else "cpu")
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    chunks = load_corpus(args.repo)
    bm = BM25([tokenize(c["path"] + " " + c["text"]) for c in chunks])
    tasks = load_tasks(args.repo, args.n, {c["path"] for c in chunks})
    cands = {t["sha"]: [i for i, _ in bm.search(tokenize(t["task"]), args.k)] for t in tasks}
    print("chunks=%d tasks=%d k=%d state_tokens=%d" % (len(chunks), len(tasks), args.k, args.state_tokens), flush=True)
    base_rows = [metrics(cands[t["sha"]], chunks, t["gold"]) for t in tasks if cands[t["sha"]]]
    report = {"bm25": dict(zip(["P@10", "Hit@10", "MRR", "R@10"], np.round(np.mean(base_rows, 0), 4).tolist()))}
    print("bm25", report["bm25"], flush=True)

    for spec in args.models:
        name, d = spec.split("=", 1)
        m = Model(d, args.device, args.state_tokens)
        m.score("warmup", chunks[:4])
        res, lat, all_p, all_y = defaultdict(list), [], [], []
        for t in tasks:
            cand = cands[t["sha"]]
            if not cand:
                continue
            t0 = time.perf_counter()
            p = m.score(t["task"], [chunks[i] for i in cand])
            if args.device == "mps":
                torch.mps.synchronize()
            lat.append((time.perf_counter() - t0) * 1000)
            y = np.array([float(chunks[i]["path"] in t["gold"]) for i in cand])
            all_p.extend(p.tolist())
            all_y.extend(y.tolist())
            order = [cand[i] for i in np.argsort(-p)]
            res["laya_only"].append(metrics(order, chunks, t["gold"]))
            rrf = {c: 1 / (60 + r) for r, c in enumerate(cand)}
            for r, c in enumerate(order):
                rrf[c] += 1 / (60 + r)
            res["rrf"].append(metrics(sorted(cand, key=lambda c: -rrf[c]), chunks, t["gold"]))
            n = len(cand)
            w = {c: 0.5 * (1 - r / n) + 0.5 * p[r] for r, c in enumerate(cand)}
            res["weighted0.5"].append(metrics(sorted(cand, key=lambda c: -w[c]), chunks, t["gold"]))
        P, Y = np.array(all_p), np.array(all_y)
        bins = np.linspace(0, 1, 11)
        ece = sum(abs(P[(P >= lo) & (P < hi)].mean() - Y[(P >= lo) & (P < hi)].mean()) * ((P >= lo) & (P < hi)).mean()
                  for lo, hi in zip(bins[:-1], bins[1:]) if ((P >= lo) & (P < hi)).any())
        report[name] = {k: dict(zip(["P@10", "Hit@10", "MRR", "R@10"], np.round(np.mean(v, 0), 4).tolist())) for k, v in res.items()}
        report[name].update({"auroc": round(auroc(P, Y), 4), "ece": round(float(ece), 4), "mean_p": round(float(P.mean()), 4),
                             "base_rate": round(float(Y.mean()), 4), "latency_ms_p50": round(float(np.median(lat)), 1)})
        print(name, json.dumps(report[name]), flush=True)
        del m
        if args.device == "mps":
            torch.mps.empty_cache()
    if args.out:
        json.dump(report, open(args.out, "w"), indent=1)


if __name__ == "__main__":
    main()
