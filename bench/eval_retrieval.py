"""Offline retrieval eval of the real laya pipeline (daemon + Moon + Laya), no Claude involved.

Dev tasks exclude the benchmark tasks so ranking can be tuned without touching the final test set.

    python3 bench/eval_retrieval.py devset --repo <clone> --bench bench/tasks.jsonl --out bench/dev_tasks.jsonl
    python3 bench/eval_retrieval.py score --repo <clone> --tasks bench/dev_tasks.jsonl [--budget-ms 0] [--tag name]
"""
import argparse
import json
import os
import statistics
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from run_bench import SRC_EXT, git  # noqa: E402

LAYA = os.environ.get("LAYA_BIN") or os.path.abspath(os.path.join(HERE, "..", "target", "release", "laya"))


def devset(args):
    bench_ids = {json.loads(l)["id"] for l in open(args.bench)}
    tracked = set(git(args.repo, "ls-files").splitlines())
    log = git(args.repo, "log", "--no-merges", "-n", "3000", "--name-only", "--format=@@%H%x09%s")
    out = []
    import re
    for block in log.split("@@")[1:]:
        head, *files = [l for l in block.splitlines() if l.strip()]
        sha, subj = head.split("\t", 1)
        if sha[:10] in bench_ids:
            continue
        gold = sorted({f for f in files if f in tracked and f.endswith(SRC_EXT)})
        subj = re.sub(r"\(#\d+\)$", "", subj).strip()
        if not (1 <= len(gold) <= 4 and len(subj) >= 25) or subj.lower().startswith(("merge", "chore(release", "bump", "test(", "style", "docs")):
            continue
        if re.search(r"clippy|lint|fmt|typo|RED\b", subj, re.I):
            continue
        out.append({"id": sha[:10], "task": subj, "gold": gold})
        if len(out) >= args.n:
            break
    with open(args.out, "w") as f:
        for t in out:
            f.write(json.dumps(t) + "\n")
    print("wrote %d dev tasks" % len(out))


def query(repo, prompt, budget_ms):
    env = dict(os.environ, LAYA_BUDGET_MS=str(budget_ms))
    t0 = time.time()
    p = subprocess.run([LAYA, "query", prompt, "--repo", repo, "--json"], capture_output=True, text=True, env=env, timeout=120)
    ms = (time.time() - t0) * 1000
    if p.returncode != 0:
        return None, ms
    return json.loads(p.stdout), ms


def evaluate(args):
    tasks = [json.loads(l) for l in open(args.tasks)][: args.limit or None]
    rows, lat, modes = [], [], {}
    text_share = []
    dump = open(args.dump, "w") if args.dump else None
    for t in tasks:
        r, ms = query(args.repo, t["task"], args.budget_ms)
        if r is None:
            continue
        if dump:
            dump.write(json.dumps({"task": t["task"], "gold": t["gold"], "result": r}) + "\n")
        lat.append(r["elapsed_ms"])
        modes[r["mode"]] = modes.get(r["mode"], 0) + 1
        files = [s["path"] for s in r["spans"]][:10]
        gold = set(t["gold"])
        hit = float(any(f in gold for f in files))
        rr = next((1 / (i + 1) for i, f in enumerate(files) if f in gold), 0.0)
        rec = len(set(files) & gold) / len(gold)
        prec = sum(f in gold for f in files) / max(1, len(files))
        text_share.append(sum(not f.endswith(SRC_EXT) for f in files) / max(1, len(files)))
        lines = sum(s["end_line"] - s["start_line"] + 1 for s in r["spans"])
        related = [x["path"] for x in r.get("related", [])]
        rec_map = len((set(files) | set(related)) & gold) / len(gold)
        new_hits = len((set(related) - set(files)) & gold)
        rows.append((prec, hit, rr, rec, lines, rec_map, len(related), new_hits))
    n = len(rows)
    m = [sum(x[i] for x in rows) / n for i in range(8)]
    res = {"tag": args.tag, "budget_ms": args.budget_ms, "n": n, "P@10": round(m[0], 4), "Hit@10": round(m[1], 4),
           "MRR": round(m[2], 4), "R@10": round(m[3], 4), "R@map(spans+related)": round(m[5], 4),
           "related_per_query": round(m[6], 2), "gold_files_added_by_related": round(m[7] * n),
           "mean_lines_returned": round(m[4], 1),
           "non_code_share": round(sum(text_share) / n, 4), "modes": modes,
           "elapsed_ms_p50": statistics.median(lat), "elapsed_ms_p95": sorted(lat)[int(0.95 * (len(lat) - 1))]}
    print(json.dumps(res))
    if args.out:
        with open(args.out, "a") as f:
            f.write(json.dumps(res) + "\n")


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    d = sub.add_parser("devset")
    d.add_argument("--repo", required=True)
    d.add_argument("--bench", required=True)
    d.add_argument("--n", type=int, default=60)
    d.add_argument("--out", required=True)
    e = sub.add_parser("score")
    e.add_argument("--repo", required=True)
    e.add_argument("--tasks", required=True)
    e.add_argument("--budget-ms", type=int, default=1200)
    e.add_argument("--limit", type=int, default=0)
    e.add_argument("--tag", default="")
    e.add_argument("--out", default=None)
    e.add_argument("--dump", default=None, help="write per-task results (task, gold, QueryResult) as JSONL")
    args = ap.parse_args()
    {"devset": devset, "score": evaluate}[args.cmd](args)


if __name__ == "__main__":
    main()
