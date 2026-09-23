"""Scope-label dataset from git history: one row per commit (task, repo, label, n_src_files).

Train repos -> scope/train.jsonl + scope/val.jsonl (10% by commit hash).
Held-out repos (moon, pilot-space) -> scope/heldout_<name>.jsonl (the most recent --heldout-n labelled commits).

    python3 finetune/build_scope_data.py [--max-commits 2500] [--heldout-n 200]
"""
import argparse
import json
import os
import subprocess
import sys
from collections import Counter
from multiprocessing import Pool

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import common  # noqa: E402
import scope  # noqa: E402
from repos import HELDOUT, TRAIN, check_leakage  # noqa: E402

OUT = os.path.join(common.WORK, "scope")


def files_of_commit(repo, parent, sha):
    """name-status for every commit; -U0 hunks only when exactly one source file changed (cheap on big commits)."""
    ns = common.git(repo, "diff", "--name-status", "-M", "--no-color", parent, sha, timeout=60)
    files = {}
    for line in ns.splitlines():
        parts = line.split("\t")
        st = parts[0]
        if st.startswith("R") and len(parts) == 3:
            files[parts[1]] = {"new_path": parts[2], "new_file": False, "deleted": False, "hunks": []}
        elif len(parts) == 2:
            files[parts[1]] = {"new_path": parts[1], "new_file": st == "A", "deleted": st == "D", "hunks": []}
    src = [p for p, f in files.items() if common.is_source(p) or common.is_source(f["new_path"])]
    if len(src) == 1:
        p = src[0]
        f = files[p]
        paths = [p] if p == f["new_path"] else [p, f["new_path"]]
        diff = common.git(repo, "diff", "-U0", "-M", "--no-color", "--no-ext-diff", parent, sha, "--", *paths, timeout=60)
        h = scope.parse_hunks(diff)
        if p in h:
            f["hunks"] = h[p]["hunks"]
    return files


def build(args):
    name, repo, limit, heldout = args
    raw = common.git(repo, "log", "--no-merges", "-n", str(limit * 3), "--format=%x1e%H%x1f%P%x1f%s%x1f%b", timeout=300)
    rows, stats = [], Counter()
    for rec in raw.split("\x1e")[1:]:
        sha, parents, subj, body = (rec.split("\x1f") + ["", "", "", ""])[:4]
        parents = parents.split()
        if len(parents) != 1 or not common.informative(subj):
            stats["skip_subject_or_merge"] += 1
            continue
        try:
            files = files_of_commit(repo, parents[0], sha)
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
            stats["diff_fail"] += 1
            continue
        lab = scope.label_files(files)
        if lab is None:
            stats["no_source"] += 1
            continue
        split = "heldout" if heldout else ("val" if int(sha[:8], 16) % 10 == 0 else "train")
        rows.append({"repo": name, "sha": sha, "split": split, "task": common.task_text(subj, body), "label": lab,
                     "n_src": scope.n_source_files(files)})
        stats[lab] += 1
        if len(rows) >= limit:
            break
    print("[%s] %d rows %s" % (name, len(rows), dict(stats)), flush=True)
    return name, rows, dict(stats)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--max-commits", type=int, default=2500)
    ap.add_argument("--heldout-n", type=int, default=200)
    ap.add_argument("--workers", type=int, default=6)
    args = ap.parse_args()
    check_leakage()
    os.makedirs(OUT, exist_ok=True)
    jobs = [(n, p, args.max_commits, False) for n, p in sorted(TRAIN.items())]
    jobs += [(n, p, args.heldout_n, True) for n, p in sorted(HELDOUT.items())]
    with Pool(args.workers) as pool:
        res = pool.map(build, jobs)
    outs = {s: open(os.path.join(OUT, "%s.jsonl" % s), "w") for s in ("train", "val")}
    summary = {"per_repo": {}, "balance": {}}
    bal = {}
    for name, rows, stats in res:
        summary["per_repo"][name] = dict(Counter(r["label"] for r in rows), n=len(rows))
        for r in rows:
            if r["split"] == "heldout":
                key = "heldout_" + name
                with open(os.path.join(OUT, key + ".jsonl"), "a") as f:
                    f.write(json.dumps(r) + "\n")
            else:
                key = r["split"]
                outs[key].write(json.dumps(r) + "\n")
            bal.setdefault(key, Counter())[r["label"]] += 1
    for f in outs.values():
        f.close()
    summary["balance"] = {k: {c: v[c] for c in scope.CLASSES} | {"n": sum(v.values())} for k, v in bal.items()}
    json.dump(summary, open(os.path.join(OUT, "stats.json"), "w"), indent=1)
    print(json.dumps(summary["balance"], indent=1))


if __name__ == "__main__":
    main()
