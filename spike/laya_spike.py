"""Phase-0 spike: does Laya (zero-shot) improve code-chunk ranking over BM25?

Ground truth comes from git history: commit subject = task, files touched by the
commit = relevant files. Corpus = 40-line windows (stride 30) of source files at HEAD.

    python3 spike/laya_spike.py --repo ~/workspaces/tind-repo/moon --model <laya dir> --n 40
"""
import argparse
import json
import math
import os
import re
import subprocess
import sys
import time
from collections import Counter, defaultdict

import numpy as np
import torch

SRC_EXT = {".rs", ".py", ".ts", ".tsx", ".js", ".go", ".java", ".c", ".h", ".cc", ".cpp", ".hpp", ".rb", ".php", ".kt", ".swift", ".cs"}
WIN, STRIDE = 40, 30


def git(repo, *args):
    return subprocess.run(["git", "-C", repo, *args], capture_output=True, text=True, check=True).stdout


def tokenize(text):
    out = []
    for w in re.findall(r"[A-Za-z_][A-Za-z0-9_]*|\d+", text):
        parts = re.sub(r"([a-z0-9])([A-Z])", r"\1 \2", w).replace("_", " ").lower().split()
        out.extend(p for p in parts if len(p) > 1)
        if len(parts) > 1:
            out.append(w.lower())
    return out


def load_corpus(repo):
    chunks = []
    for path in git(repo, "ls-files").splitlines():
        if os.path.splitext(path)[1] not in SRC_EXT or "/vendor/" in path or path.startswith(("target/", "node_modules/")):
            continue
        try:
            lines = open(os.path.join(repo, path), encoding="utf-8", errors="ignore").read().splitlines()
        except OSError:
            continue
        for s in range(0, max(1, len(lines)), STRIDE):
            body = "\n".join(lines[s:s + WIN])
            if body.strip():
                chunks.append({"path": path, "start": s + 1, "end": min(len(lines), s + WIN), "text": body})
            if s + WIN >= len(lines):
                break
    return chunks


class BM25:
    def __init__(self, docs, k1=1.2, b=0.75):
        self.k1, self.b = k1, b
        self.tfs = [Counter(d) for d in docs]
        self.lens = np.array([len(d) for d in docs], dtype=float)
        self.avg = self.lens.mean()
        df = Counter(t for tf in self.tfs for t in tf)
        n = len(docs)
        self.idf = {t: math.log(1 + (n - c + 0.5) / (c + 0.5)) for t, c in df.items()}
        self.post = defaultdict(list)
        for i, tf in enumerate(self.tfs):
            for t in tf:
                self.post[t].append(i)

    def search(self, q, k):
        sc = defaultdict(float)
        for t in set(q):
            if t not in self.idf:
                continue
            for i in self.post[t]:
                f = self.tfs[i][t]
                sc[i] += self.idf[t] * f * (self.k1 + 1) / (f + self.k1 * (1 - self.b + self.b * self.lens[i] / self.avg))
        return sorted(sc.items(), key=lambda x: -x[1])[:k]


def load_tasks(repo, n, corpus_paths):
    tasks = []
    log = git(repo, "log", "--no-merges", "-n", "600", "--name-only", "--format=@@%H%x09%s")
    for block in log.split("@@")[1:]:
        head, *files = [l for l in block.splitlines() if l.strip()]
        sha, subj = head.split("\t", 1)
        gold = {f for f in files if f in corpus_paths}
        subj_clean = re.sub(r"\(#\d+\)$", "", subj).strip()
        if 1 <= len(gold) <= 4 and len(subj_clean) >= 25 and not subj_clean.lower().startswith(("merge", "chore(release", "bump")):
            tasks.append({"sha": sha, "task": subj_clean, "gold": sorted(gold)})
        if len(tasks) >= n:
            break
    return tasks


class Laya:
    def __init__(self, model_dir, device):
        sys.path.insert(0, model_dir)
        from rl_agent_api import RLAgent  # noqa: E402  (reference implementation shipped with the model)
        self.agent = RLAgent(model_dir, device=device)

    def score(self, task, chunks, question):
        """P(relevant) for each chunk: one noul question per chunk, batched in one forward pass."""
        from rl_common import QTYPES, build_sequence, collate_items, temp_bucket
        a = self.agent
        q = {"t": "noul", "ins": question.format(task=task), "crit": None}
        items = []
        for c in chunks:
            state = "file: %s (lines %d-%d)\n%s" % (c["path"], c["start"], c["end"], c["text"])
            ids, markers = build_sequence(a.tok, state, q, a.cfg["max_len"], a.cfg["head_max_len"])
            items.append({"ids": ids, "markers": markers, "qtype": QTYPES["noul"], "target": [0.0, 0.0], "label": -1,
                          "episode": 0, "ep_step": 0, "ep_len": 1, "src": "spike"})
        b = collate_items([items], a.tok.pad_token_id)
        dev = a.device
        with torch.no_grad():
            logits, _ = a.model(b["input_ids"].to(dev), b["attention_mask"].to(dev), b["marker_pos"].to(dev),
                                b["marker_mask"].to(dev), b["qtype"].to(dev))
        z = logits.float().cpu().numpy()[:, :2] / a.temperature_by_options.get(temp_bucket(2, 2), a.temperature[2])
        p = np.exp(z - z.max(1, keepdims=True))
        return (p / p.sum(1, keepdims=True))[:, 1]


QUESTIONS = {
    "q_read": "A coding agent must complete this task: \"{task}\". Does the code in the state need to be read or modified to complete the task?",
    "q_rel": "Is this source code relevant to the software change: \"{task}\"?",
}


def metrics(ranked, chunks, gold):
    top = ranked[:10]
    files = [chunks[i]["path"] for i in top]
    prec = sum(f in gold for f in files) / max(1, len(top))
    hit = float(any(f in gold for f in files))
    rr = next((1.0 / (r + 1) for r, f in enumerate(files) if f in gold), 0.0)
    recall = len(set(files) & set(gold)) / len(gold)
    return prec, hit, rr, recall


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    ap.add_argument("--model", required=True)
    ap.add_argument("--n", type=int, default=40)
    ap.add_argument("--k", type=int, default=32)
    ap.add_argument("--device", default="mps" if torch.backends.mps.is_available() else "cpu")
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    chunks = load_corpus(args.repo)
    bm = BM25([tokenize(c["path"] + " " + c["text"]) for c in chunks])
    tasks = load_tasks(args.repo, args.n, {c["path"] for c in chunks})
    print("corpus chunks=%d tasks=%d device=%s" % (len(chunks), len(tasks), args.device), flush=True)
    laya = Laya(args.model, args.device)
    laya.score("warmup", chunks[:4], QUESTIONS["q_read"])

    res = defaultdict(list)
    lat = []
    calib = []
    for t in tasks:
        cand = [i for i, _ in bm.search(tokenize(t["task"]), args.k)]
        if not cand:
            continue
        res["bm25"].append(metrics(cand, chunks, t["gold"]))
        for qn, qt in QUESTIONS.items():
            t0 = time.perf_counter()
            p = laya.score(t["task"], [chunks[i] for i in cand], qt)
            if args.device == "mps":
                torch.mps.synchronize()
            lat.append(time.perf_counter() - t0)
            order = [cand[i] for i in np.argsort(-p)]
            res["laya_" + qn].append(metrics(order, chunks, t["gold"]))
            # fusion: RRF of bm25 rank and laya rank
            rrf = {c: 1 / (60 + r) for r, c in enumerate(cand)}
            for r, c in enumerate(order):
                rrf[c] += 1 / (60 + r)
            res["rrf_" + qn].append(metrics(sorted(cand, key=lambda c: -rrf[c]), chunks, t["gold"]))
            # threshold policy: keep P>=0.5, top10, fallback to bm25 order when fewer than 3 pass
            keep = [cand[i] for i in np.argsort(-p) if p[i] >= 0.5][:10]
            if len(keep) < 3:
                keep = (keep + [c for c in cand if c not in keep])[:10]
            res["gate_" + qn].append(metrics(keep, chunks, t["gold"]))
            for i, c in enumerate(cand):
                calib.append((qn, float(p[i]), float(chunks[c]["path"] in t["gold"])))

    summary = {}
    print("\n%-12s %6s %6s %6s %6s" % ("method", "P@10", "Hit@10", "MRR", "R@10"))
    for m, rows in res.items():
        a = np.array(rows).mean(0)
        summary[m] = dict(zip(["P@10", "Hit@10", "MRR", "R@10"], [round(float(x), 4) for x in a]))
        print("%-12s %6.3f %6.3f %6.3f %6.3f" % (m, *a))
    lat = np.array(lat) * 1000
    summary["latency_ms_k%d" % args.k] = {"p50": round(float(np.percentile(lat, 50)), 1), "p95": round(float(np.percentile(lat, 95)), 1)}
    for qn in QUESTIONS:
        ps = np.array([(p, y) for q, p, y in calib if q == qn])
        hi, lo = ps[ps[:, 0] >= 0.5], ps[ps[:, 0] < 0.5]
        summary["calib_" + qn] = {"n_pos_pred": len(hi), "prec_at_p>=0.5": round(float(hi[:, 1].mean()), 4) if len(hi) else None,
                                   "base_rate": round(float(ps[:, 1].mean()), 4), "mean_p": round(float(ps[:, 0].mean()), 4)}
    print(json.dumps({k: v for k, v in summary.items() if not k.startswith(("bm25", "laya", "rrf", "gate"))}, indent=1))
    if args.out:
        json.dump({"repo": args.repo, "n_tasks": len(tasks), "k": args.k, "summary": summary}, open(args.out, "w"), indent=1)


if __name__ == "__main__":
    main()
