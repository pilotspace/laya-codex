"""Build production-shaped reranking lists from fixed commits of the TRAIN repos (never a held-out repo).

For each commit that fixes something (common.is_fix: a fix/bug word in the subject, or a body that closes
an issue) with one parent and 1-MAX_FILES source files, oldest first among the most recent --max-commits:

  checkout  the parent revision in a scratch worktree, and index it incrementally with the production
            indexer (laya-candgen: laya-parse tree-sitter chunks into Moon)
  task      common.commit_task: the subject (conventional prefix and PR number removed), and for half of the
            commits (chosen by sha) the first prose paragraph of the body too
  list      what the production retriever hands to Laya for that task: the task focus and the top 24
            lexical candidates (BM25 + definitions + path, RRF, prose demoted), in lexical order
  labels    common.label_candidates against `git diff -U0 -M parent commit`: a candidate overlapping a
            changed line 1.0, another chunk of a changed file FILE_SOFT, anything else 0.0 (hard negatives:
            the lexical candidates the fix did not touch)

One JSON line per commit in WORK/data_v3/<repo>.jsonl, split train/val by commit hash (10% val). Lists
without any positive are kept (flag `has_pos`) for the recall statistics; training skips them.
Held-out evaluation lists (--heldout pilot-space) go to heldout-<repo>.jsonl and are never merged.

    python3 finetune/build_data.py --candgen PATH --moon-bin PATH [--max-commits 1200] [--workers 4]
"""
import argparse
import json
import os
import subprocess
import sys
import time
from collections import Counter
from multiprocessing import Pool

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import common  # noqa: E402
from repos import HELDOUT, TRAIN, check_leakage  # noqa: E402

MAX_FILES = 8
OUT = os.path.join(common.WORK, "data_v3")
PORT_BASE = 16561


def list_fix_commits(repo, limit):
    """Most recent `limit` fixed commits, returned oldest first (so the scratch checkout only moves forward)."""
    raw = common.git(repo, "log", "--no-merges", "--name-only", "--format=%x1e%H%x1f%P%x1f%s%x1f%b%x1d", timeout=600)
    out = []
    for rec in raw.split("\x1e")[1:]:
        head, _, files = rec.partition("\x1d")
        sha, parents, subj, body = (head.split("\x1f") + ["", "", "", ""])[:4]
        parents = parents.split()
        src = [f for f in files.splitlines() if f.strip() and common.is_source(f.strip())]
        if len(parents) != 1 or not (1 <= len(src) <= MAX_FILES) or not common.informative(subj):
            continue
        if not common.is_fix(subj, body):
            continue
        out.append({"sha": sha, "parent": parents[0], "subject": subj, "body": body})
    return list(reversed(out[:limit]))


class CandGen:
    """One laya-candgen process (and its own Moon) per repo."""

    def __init__(self, binary, moon_bin, port, home):
        self.p = subprocess.Popen([binary, "--moon-bin", moon_bin, "--port", str(port), "--home", home],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1)

    def call(self, req):
        self.p.stdin.write(json.dumps(req) + "\n")
        self.p.stdin.flush()
        line = self.p.stdout.readline()
        if not line:
            raise RuntimeError("laya-candgen exited (%s)" % self.p.poll())
        return json.loads(line)

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(timeout=120)
        except Exception:
            self.p.kill()


def checkout(repo, wt, rev):
    if not os.path.isdir(os.path.join(wt, ".git")) and not os.path.isfile(os.path.join(wt, ".git")):
        common.git(repo, "worktree", "prune", timeout=120)
        common.git(repo, "worktree", "add", "-f", "--detach", wt, rev, timeout=600)
    else:
        common.git(wt, "checkout", "-q", "-f", "--detach", rev, timeout=600)
        common.git(wt, "clean", "-q", "-f", "-d", timeout=600)


def split_of(sha):
    return "val" if int(sha[:8], 16) % 10 == 0 else "train"


def with_body(sha, body):
    return bool((body or "").strip()) and int(sha[8:16], 16) % 2 == 0


def build_repo(job):
    name, repo, max_commits, port, candgen, moon_bin, out_path = job
    t0 = time.time()
    stats = Counter()
    commits = list_fix_commits(repo, max_commits)
    stats["fix_commits"] = len(commits)
    wt = os.path.join(common.WORK, "wt", name)
    home = os.path.join(common.WORK, "candgen-home", name)
    os.makedirs(os.path.dirname(wt), exist_ok=True)
    cg = CandGen(candgen, moon_bin, port, home)
    tmp = out_path + ".tmp"
    try:
        with open(tmp, "w") as out:
            for i, c in enumerate(commits):
                try:
                    checkout(repo, wt, c["parent"])
                    diff = common.git(repo, "diff", "-U0", "-M", "--no-color", "--no-ext-diff", c["parent"], c["sha"],
                                      timeout=120)
                except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
                    stats["git_fail"] += 1
                    continue
                idx = cg.call({"op": "index", "root": wt, "repo": name})
                if "error" in idx:
                    stats["index_fail"] += 1
                    continue
                task = common.commit_task(c["subject"], c["body"], with_body(c["sha"], c["body"]))
                res = cg.call({"op": "query", "repo": name, "prompt": task, "id": c["sha"]})
                if "error" in res:
                    stats["query_fail"] += 1
                    continue
                cands = res["candidates"]
                if not cands:
                    stats["no_candidates"] += 1
                    continue
                fd = common.parse_diff_u0(diff)
                labels = common.label_candidates(cands, fd)
                for cand, (y, kind) in zip(cands, labels):
                    cand["label"], cand["label_kind"] = y, kind
                kinds = Counter(k for _, k in labels)
                has_pos = kinds["hunk"] + kinds["file"] > 0
                split = split_of(c["sha"])
                row = {"repo": name, "sha": c["sha"], "parent": c["parent"], "split": split, "task": task,
                       "with_body": task != common.commit_task(c["subject"], "", False), "focus": res["focus"],
                       "has_pos": has_pos, "n_hunk": kinds["hunk"], "n_file": kinds["file"],
                       "touched_files": sorted(p for p, f in fd.items() if not f["new_file"]), "candidates": cands}
                out.write(json.dumps(row) + "\n")
                stats["lists_" + split] += 1
                stats["lists_with_pos_" + split] += int(has_pos)
                stats["cand_hunk"] += kinds["hunk"]
                stats["cand_file"] += kinds["file"]
                stats["cand_other"] += kinds["other"]
                if (i + 1) % 100 == 0:
                    print("[%s] %d/%d commits %.0fs %s" % (name, i + 1, len(commits), time.time() - t0, dict(stats)),
                          flush=True)
        os.replace(tmp, out_path)
    finally:
        cg.close()
        try:
            common.git(repo, "worktree", "remove", "--force", wt, timeout=600)
        except Exception:
            pass
    stats["secs"] = round(time.time() - t0)
    print("[%s] done %s" % (name, dict(stats)), flush=True)
    return name, dict(stats)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--candgen", required=True, help="laya-candgen binary (finetune/candgen)")
    ap.add_argument("--moon-bin", required=True)
    ap.add_argument("--max-commits", type=int, default=1200)
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument("--only", nargs="*")
    ap.add_argument("--heldout", nargs="*", default=[], help="held-out repos to build evaluation lists for")
    ap.add_argument("--port-base", type=int, default=PORT_BASE)
    args = ap.parse_args()
    check_leakage()
    os.makedirs(OUT, exist_ok=True)
    held = {os.path.realpath(p) for p in HELDOUT.values()}
    jobs = []
    for name, repo in sorted(TRAIN.items()):
        assert os.path.realpath(repo) not in held
        if args.only is None or name in args.only:
            jobs.append((name, repo, os.path.join(OUT, "%s.jsonl" % name)))
    for name in args.heldout:
        jobs.append((name, HELDOUT[name], os.path.join(OUT, "heldout-%s.jsonl" % name)))
    jobs = [(n, r, args.max_commits, args.port_base + i, args.candgen, args.moon_bin, o)
            for i, (n, r, o) in enumerate(jobs)]
    with Pool(max(1, min(args.workers, len(jobs)))) as pool:
        res = pool.map(build_repo, jobs, chunksize=1)
    stats_path = os.path.join(common.WORK, "data_v3_stats.json")
    prev = json.load(open(stats_path)) if os.path.exists(stats_path) else {}
    prev.update({n: s for n, s in res})
    json.dump(prev, open(stats_path, "w"), indent=1)
    print(json.dumps(prev, indent=1))


if __name__ == "__main__":
    main()
