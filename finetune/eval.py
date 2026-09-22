"""Held-out evaluation of any Laya model dir(s) with the spike protocol (spike/laya_spike.py), extended:

- same corpus (40-line windows at HEAD), same BM25, same task selection -> identical 40 moon tasks as the spike
- any number of repos (moon + second held-out repo) and model dirs (laya-base vs laya-code)
- state truncated to --state-budget tokens (256 = the production budget; 0 = spike behaviour, fill 512 context)
- rows: bm25, laya (Laya-only re-rank of BM25 top-k), rrf (BM25 (+) Laya reciprocal-rank fusion), gate (P>=0.5 policy)
  with P@10, Hit@10, MRR, R@10 (file-level gold, as in the spike)
- calibration over all candidates: precision at P>=0.5, base rate, mean P, ECE (15 bins), AUROC pooled and per task

Scores are cached per (model, repo, budget) so models can be run separately and merged.

    python3 finetune/eval.py --models laya-base=~/.cache/laya-codex/models/laya-base laya-code=~/.cache/laya-codex/models/laya-code \
        --repos moon=~/workspaces/tind-repo/moon pilot-space=~/workspaces/tind-repo/pilot-space --out spike/results/finetune_eval.json
"""
import argparse
import json
import os
import sys
import time
from collections import defaultdict

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(os.path.dirname(HERE), "spike"))
import common  # noqa: E402
from laya_spike import BM25, load_corpus, load_tasks, metrics, tokenize  # noqa: E402

CACHE = os.path.join(common.WORK, "eval_cache")


def candidates(repo, n, k):
    chunks = load_corpus(repo)
    bm = BM25([tokenize(c["path"] + " " + c["text"]) for c in chunks])
    tasks = load_tasks(repo, n, {c["path"] for c in chunks})
    for t in tasks:
        t["cand"] = [i for i, _ in bm.search(tokenize(t["task"]), k)]
    return chunks, [t for t in tasks if t["cand"]]


def score_model(model_dir, device, chunks, tasks, budget):
    """Calibrated P(relevant) per candidate via the reference RLAgent of `model_dir` (its own temperature)."""
    import importlib
    import torch
    sys.path.insert(0, model_dir)
    for m in ("rl_agent_api", "rl_common"):
        sys.modules.pop(m, None)
    api = importlib.import_module("rl_agent_api")
    from rl_common import QTYPES, build_sequence, collate_items, temp_bucket
    agent = api.RLAgent(model_dir, device=device)
    T = agent.temperature_by_options.get(temp_bucket(2, 2), agent.temperature[2])
    out, lat = [], []
    for t in tasks:
        q = {"t": "noul", "ins": common.QUESTIONS[0].format(task=t["task"]), "crit": None}
        items = []
        for i in t["cand"]:
            c = chunks[i]
            if budget:
                st, _ = common.make_state(agent.tok, c["path"], c["start"], c["end"], c["text"], budget=budget)
            else:
                st = "file: %s (lines %d-%d)\n%s" % (c["path"], c["start"], c["end"], c["text"])
            ids, markers = build_sequence(agent.tok, st, q, agent.cfg["max_len"], agent.cfg["head_max_len"])
            items.append({"ids": ids, "markers": markers, "qtype": QTYPES["noul"], "target": [0.0, 0.0], "label": -1,
                          "episode": 0, "ep_step": 0, "ep_len": 1, "src": "eval"})
        b = collate_items([items], agent.tok.pad_token_id)
        t0 = time.perf_counter()
        with torch.no_grad():
            logits, _ = agent.model(b["input_ids"].to(agent.device), b["attention_mask"].to(agent.device),
                                    b["marker_pos"].to(agent.device), b["marker_mask"].to(agent.device),
                                    b["qtype"].to(agent.device))
        z = logits.float().cpu().numpy()[:, :2]
        lat.append(time.perf_counter() - t0)
        zz = z / T
        p = 1.0 / (1.0 + np.exp(-(zz[:, 1] - zz[:, 0])))
        out.append({"sha": t["sha"], "p": p.tolist(), "raw": z.tolist()})
    del agent
    return out, T, lat


def summarize(chunks, tasks, scores):
    from rl_common import auroc, ece_score
    res = defaultdict(list)
    ps, ys, per_task_auc = [], [], []
    for t, s in zip(tasks, scores):
        assert t["sha"] == s["sha"]
        cand, p = t["cand"], np.array(s["p"])
        gold = set(t["gold"])
        res["bm25"].append(metrics(cand, chunks, gold))
        order = [cand[i] for i in np.argsort(-p, kind="stable")]
        res["laya"].append(metrics(order, chunks, gold))
        rrf = {c: 1 / (60 + r) for r, c in enumerate(cand)}
        for r, c in enumerate(order):
            rrf[c] += 1 / (60 + r)
        res["rrf"].append(metrics(sorted(cand, key=lambda c: -rrf[c]), chunks, gold))
        keep = [cand[i] for i in np.argsort(-p, kind="stable") if p[i] >= 0.5][:10]
        if len(keep) < 3:
            keep = (keep + [c for c in cand if c not in keep])[:10]
        res["gate"].append(metrics(keep, chunks, gold))
        y = np.array([float(chunks[c]["path"] in gold) for c in cand])
        ps.append(p)
        ys.append(y)
        if 0 < y.sum() < len(y):
            per_task_auc.append(auroc(p, y.astype(int)))
    out = {}
    for m, rows in res.items():
        a = np.array(rows).mean(0)
        out[m] = dict(zip(["P@10", "Hit@10", "MRR", "R@10"], [round(float(x), 4) for x in a]))
    p, y = np.concatenate(ps), np.concatenate(ys)
    hi = p >= 0.5
    out["calibration"] = {
        "n_candidates": int(len(p)), "base_rate": round(float(y.mean()), 4), "mean_p": round(float(p.mean()), 4),
        "n_p>=0.5": int(hi.sum()), "prec_at_p>=0.5": round(float(y[hi].mean()), 4) if hi.any() else None,
        "recall_at_p>=0.5": round(float(y[hi].sum() / max(1, y.sum())), 4),
        "ece_15bin": round(ece_score(p, y), 4), "auroc_pooled": round(auroc(p, y.astype(int)), 4),
        "auroc_per_task_mean": round(float(np.mean(per_task_auc)), 4) if per_task_auc else None,
        "max_p": round(float(p.max()), 4), "p95_p": round(float(np.percentile(p, 95)), 4),
        "precision_at": {str(th): {"n": int((p >= th).sum()), "prec": round(float(y[p >= th].mean()), 4) if (p >= th).any() else None}
                         for th in (0.2, 0.3, 0.4, 0.5)},
        "reliability_10bin": [{"p_mean": round(float(p[s].mean()), 3), "freq": round(float(y[s].mean()), 3), "n": int(s.sum())}
                              for lo, hi in zip(np.linspace(0, 1, 11)[:-1], np.linspace(0, 1, 11)[1:])
                              for s in [(p >= lo) & (p < hi if hi < 1 else p <= hi)] if s.any()]}
    out["per_task_rr"] = {m: [round(float(r[2]), 4) for r in rows] for m, rows in res.items()}
    out["per_task_p10"] = {m: [round(float(r[0]), 4) for r in rows] for m, rows in res.items()}
    out["per_task_r10"] = {m: [round(float(r[3]), 4) for r in rows] for m, rows in res.items()}
    return out


def paired_bootstrap(a, b, n=10000, seed=0):
    """Mean of (a - b) over tasks with a 95% paired-bootstrap CI and P(diff <= 0)."""
    d = np.asarray(a) - np.asarray(b)
    rng = np.random.default_rng(seed)
    bs = d[rng.integers(0, len(d), (n, len(d)))].mean(1)
    return {"mean_diff": round(float(d.mean()), 4), "ci95": [round(float(np.percentile(bs, 2.5)), 4),
                                                             round(float(np.percentile(bs, 97.5)), 4)],
            "p_diff_le_0": round(float((bs <= 0).mean()), 4)}


def compare(report, new="laya-code@256", old="laya-base@256"):
    """Paired per-task MRR differences for the go criterion (same tasks, same candidates)."""
    out = {}
    for repo, r in report["results"].items():
        ms = r["models"]
        if new not in ms or old not in ms or "per_task_rr" not in ms[new] or "per_task_rr" not in ms[old]:
            continue
        a, b = ms[new]["per_task_rr"], ms[old]["per_task_rr"]
        out[repo] = {"rrf_new_vs_rrf_old": paired_bootstrap(a["rrf"], b["rrf"]),
                     "rrf_new_vs_bm25": paired_bootstrap(a["rrf"], a["bm25"]),
                     "laya_new_vs_laya_old": paired_bootstrap(a["laya"], b["laya"]),
                     "laya_new_vs_bm25": paired_bootstrap(a["laya"], a["bm25"]),
                     "rrf_p10_new_vs_old": paired_bootstrap(ms[new]["per_task_p10"]["rrf"], ms[old]["per_task_p10"]["rrf"]),
                     "rrf_r10_new_vs_old": paired_bootstrap(ms[new]["per_task_r10"]["rrf"], ms[old]["per_task_r10"]["rrf"])}
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--models", nargs="+", required=True, help="name=dir")
    ap.add_argument("--repos", nargs="+", required=True, help="name=path")
    ap.add_argument("--budgets", type=int, nargs="+", default=[256])
    ap.add_argument("--n", type=int, default=40)
    ap.add_argument("--k", type=int, default=32)
    ap.add_argument("--device", default=None)
    ap.add_argument("--out", default=None)
    ap.add_argument("--no-cache", action="store_true")
    args = ap.parse_args()
    import torch
    device = args.device or ("mps" if torch.backends.mps.is_available() else "cpu")
    os.makedirs(CACHE, exist_ok=True)
    report = {"protocol": {"n_tasks": args.n, "k": args.k, "question": common.QUESTIONS[0], "gold": "file-level",
                           "corpus": "40-line windows (stride 30) at HEAD", "fusion": "RRF k=60 over BM25 and Laya ranks",
                           "gate": "keep P>=0.5, top-10, fall back to BM25 order if <3 pass"}, "results": {}}
    for rspec in args.repos:
        rname, rpath = rspec.split("=", 1)
        rpath = os.path.expanduser(rpath)
        chunks, tasks = candidates(rpath, args.n, args.k)
        print("[%s] corpus chunks=%d tasks=%d" % (rname, len(chunks), len(tasks)), flush=True)
        rep = report["results"].setdefault(rname, {"n_tasks": len(tasks), "n_chunks": len(chunks), "models": {}})
        for mspec in args.models:
            mname, mdir = mspec.split("=", 1)
            mdir = os.path.expanduser(mdir)
            for budget in args.budgets:
                key = "%s@%s" % (mname, budget or "full")
                cpath = os.path.join(CACHE, "%s_%s_%s.json" % (mname, rname, budget or "full"))
                shas = [t["sha"] for t in tasks]
                cached = json.load(open(cpath)) if os.path.exists(cpath) and not args.no_cache else None
                if cached and cached["shas"] == shas and cached["mtime"] == os.path.getmtime(os.path.join(mdir, "model.safetensors")) \
                        and cached.get("cfg_mtime") == os.path.getmtime(os.path.join(mdir, "rl_agent_config.json")):
                    scores, T, lat = cached["scores"], cached["T"], cached["lat"]
                else:
                    scores, T, lat = score_model(mdir, device, chunks, tasks, budget)
                    json.dump({"shas": shas, "scores": scores, "T": T, "lat": lat,
                               "mtime": os.path.getmtime(os.path.join(mdir, "model.safetensors")),
                               "cfg_mtime": os.path.getmtime(os.path.join(mdir, "rl_agent_config.json"))}, open(cpath, "w"))
                s = summarize(chunks, tasks, scores)
                s["noul_temperature"] = round(float(T), 4)
                s["latency_ms_per_task_k%d" % args.k] = {"p50": round(float(np.percentile(np.array(lat) * 1000, 50)), 1),
                                                         "device": device}
                rep["models"][key] = s
                print("  %-18s bm25 MRR %.3f | laya MRR %.3f | rrf MRR %.3f | gate MRR %.3f | prec@P>=.5 %s base %.3f "
                      "ECE %.3f AUROC %.3f" % (key, s["bm25"]["MRR"], s["laya"]["MRR"], s["rrf"]["MRR"], s["gate"]["MRR"],
                                               s["calibration"]["prec_at_p>=0.5"], s["calibration"]["base_rate"],
                                               s["calibration"]["ece_15bin"], s["calibration"]["auroc_pooled"]), flush=True)
    if args.out:
        prev = json.load(open(args.out)) if os.path.exists(args.out) else {}
        for r, v in report["results"].items():  # merge with earlier partial runs
            old = prev.get("results", {}).get(r, {}).get("models", {})
            v["models"] = dict(old, **v["models"])
        prev_res = prev.get("results", {})
        prev_res.update(report["results"])
        report["results"] = prev_res
        for k, v in prev.items():
            if k not in report:
                report[k] = v
        report["paired_bootstrap"] = compare(report)
        json.dump(report, open(args.out, "w"), indent=1)
        print("wrote", args.out)


if __name__ == "__main__":
    main()
