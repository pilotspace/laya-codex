"""Leakage check between the training repos / training rows and the held-out repos.

Held out: every repository the replay gate or the held-out evaluation reads (moon, httpx and hono,
the benchmark repos, and pilot-space). Nothing from them may reach training:

- repos: no train repo inside a held-out checkout (or the reverse), no shared root commit (forks,
  clones, mirrors) with a held-out repo or with another train repo, and no vendored copy of a
  held-out source file (same git blob, at least MIN_VENDOR_BYTES) at HEAD;
- rows: no row from a held-out repo, no row built from a commit that exists in a held-out repo, and
  no task equal (after normalisation) to a benchmark task.

    python3 finetune/leakage.py [--data DIR]    # exit 1 and print the problems on any leak
"""
import argparse
import json
import os
import re
import subprocess
import sys

MIN_VENDOR_BYTES = 512
SOURCE_EXT = (".rs", ".py", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".go", ".java", ".c", ".h", ".cc", ".cpp", ".hpp",
              ".rb", ".php", ".kt", ".swift", ".cs")


def _git(repo, *args, check=True):
    return subprocess.run(["git", "-C", repo, *args], capture_output=True, text=True, timeout=300, check=check,
                          errors="ignore").stdout


def root_commits(repo):
    return set(_git(repo, "rev-list", "--max-parents=0", "HEAD").split())


def source_blobs(repo, min_bytes=MIN_VENDOR_BYTES):
    """{blob sha: path} of source files at HEAD of at least `min_bytes` (skips licences, stubs, empties)."""
    out = {}
    for line in _git(repo, "ls-tree", "-r", "-l", "HEAD").splitlines():
        meta, _, path = line.partition("\t")
        parts = meta.split()
        if len(parts) != 4 or parts[1] != "blob" or not path.endswith(SOURCE_EXT):
            continue
        if parts[3].isdigit() and int(parts[3]) >= min_bytes:
            out[parts[2]] = path
    return out


def check_repos(train, heldout):
    """train / heldout: {name: checkout path}. Returns {"ok", "problems", "stats"}."""
    problems, stats = [], {}
    held_roots = {n: root_commits(p) for n, p in heldout.items()}
    held_blobs = {n: source_blobs(p) for n, p in heldout.items()}
    seen = {}
    for name, path in train.items():
        real = os.path.realpath(path)
        for hn, hp in heldout.items():
            hp = os.path.realpath(hp)
            if real == hp or real.startswith(hp + os.sep) or hp.startswith(real + os.sep):
                problems.append("train repo %s overlaps held-out repo %s on disk" % (name, hn))
        roots = root_commits(path)
        for hn, hr in held_roots.items():
            if roots & hr:
                problems.append("train repo %s shares history with held-out repo %s" % (name, hn))
        for on, orr in seen.items():
            if roots & orr:
                problems.append("train repos %s and %s share history (fork/clone)" % (name, on))
        seen[name] = roots
        blobs = source_blobs(path)
        for hn, hb in held_blobs.items():
            shared = sorted(set(blobs) & set(hb))
            stats["%s~%s shared source blobs" % (name, hn)] = len(shared)
            if shared:
                problems.append("train repo %s has vendored held-out %s source (%d files, e.g. %s)" % (
                    name, hn, len(shared), blobs[shared[0]]))
    return {"ok": not problems, "problems": problems, "stats": stats}


def normalize_task(text):
    return " ".join(re.sub(r"[^\w`'@ ]+", " ", text.lower()).split())


def check_rows(rows, heldout_names, bench_tasks, heldout_paths=None):
    """rows: dicts with repo, sha, task. heldout_paths: {name: checkout} to look each sha up in."""
    problems = []
    bench = {normalize_task(t) for t in bench_tasks}
    shas = sorted({r["sha"] for r in rows})
    for r in rows:
        if r["repo"] in heldout_names:
            problems.append("row from held-out repo %s (%s)" % (r["repo"], r["sha"][:10]))
        if normalize_task(r["task"]) in bench:
            problems.append("row task equals a benchmark task: %r" % r["task"])
    for hn, hp in (heldout_paths or {}).items():
        if not shas:
            break
        p = subprocess.run(["git", "-C", hp, "cat-file", "--batch-check"], input="\n".join(shas) + "\n",
                           capture_output=True, text=True, timeout=300)
        for line in p.stdout.splitlines():
            parts = line.split()
            if len(parts) >= 2 and parts[1] == "commit":
                problems.append("row commit %s exists in held-out repo %s" % (parts[0][:10], hn))
    return {"ok": not problems, "problems": sorted(set(problems)), "rows": len(rows), "commits": len(shas)}


def bench_tasks(bench_dir):
    out = []
    for f in sorted(os.listdir(bench_dir)):
        if f.endswith(".jsonl"):
            out += [json.loads(l)["task"] for l in open(os.path.join(bench_dir, f)) if l.strip()]
    return out


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    sys.path.insert(0, here)
    from repos import HELDOUT, TRAIN
    import common
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", default=os.path.join(common.WORK, "data_v3"), help="dir of built *.jsonl lists")
    ap.add_argument("--bench", default=os.path.join(os.path.dirname(here), "bench", "tasks-v8"))
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    rep = {"repos": check_repos(TRAIN, HELDOUT), "heldout": sorted(HELDOUT), "train": sorted(TRAIN)}
    rows = []
    if os.path.isdir(args.data):
        for f in sorted(os.listdir(args.data)):
            if f.endswith(".jsonl") and not f.startswith("heldout-"):
                rows += [json.loads(l) for l in open(os.path.join(args.data, f)) if l.strip()]
    rep["rows"] = check_rows(rows, set(HELDOUT), bench_tasks(args.bench), heldout_paths=HELDOUT)
    rep["ok"] = rep["repos"]["ok"] and rep["rows"]["ok"]
    text = json.dumps(rep, indent=1)
    if args.out:
        open(args.out, "w").write(text)
    print(text)
    sys.exit(0 if rep["ok"] else 1)


if __name__ == "__main__":
    main()
