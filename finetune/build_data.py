"""Build (task, chunk, label) pairs from git history of the TRAIN repos (never moon / pilot-space).

For each non-merge commit touching 1-4 source files with an informative subject:
  task      = subject (+ short first body line)
  positives = 40-line windows of the PARENT revision whose visible (<=256-token) part overlaps the old-side
              lines of `git diff -U0 parent commit` (label 1.0, at most MAX_POS per commit)
  cand list = (v2) the BM25 top-CAND_K over other files (snapshot index at an ancestor revision) MERGED with the
              parent-revision windows of the touched files, i.e. what inference re-ranks. Labels: hunk window 1.0,
              other window of a touched file FILE_SOFT (file-level relevant: the eval gold is file-level), other file 0.
  pos       = hunk windows BM25 did not surface (<= MAX_POS; label 1.0)
  same-file = random other windows of touched files (soft label SOFT, "right file, wrong place"; <= MAX_SAME)
  rand neg  = 2 random windows from other files
v1 (first run) had no candidate list: hunk positives vs BM25 hard negatives from other files only; it taught the model
that lexically matching windows of the right file are negatives, contradicting the file-level objective.
Split: train / val by commit hash (val = 10%), so no commit contributes to both.

    python3 finetune/build_data.py [--max-commits 600] [--workers 8]
"""
import argparse
import json
import os
import random
import subprocess
import sys
import time
from collections import Counter
from multiprocessing import Pool

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(os.path.dirname(HERE), "spike"))

import common  # noqa: E402
from repos import HELDOUT, TRAIN, check_leakage  # noqa: E402

MAX_POS, MAX_SAME, SOFT = 3, 1, 0.4
CAND_K, FILE_SOFT = 12, 0.7  # v2: inference-shaped BM25 candidate lists with file-level soft labels
SNAP_EVERY = 150
OUT = os.path.join(common.WORK, "data_v2")


class Blobs:
    """Persistent `git cat-file --batch` reader (one process per repo, avoids thousands of `git show`)."""

    def __init__(self, repo):
        self.p = subprocess.Popen(["git", "-C", repo, "cat-file", "--batch"], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)

    def get(self, rev, path, max_bytes=400_000):
        if "\n" in path:
            return None
        self.p.stdin.write(("%s:%s\n" % (rev, path)).encode())
        self.p.stdin.flush()
        header = self.p.stdout.readline().decode(errors="ignore").split()
        if len(header) != 3:
            return None  # "<obj> missing" / ambiguous
        size = int(header[2])
        data = self.p.stdout.read(size)
        self.p.stdout.read(1)
        if header[1] != "blob" or size > max_bytes or b"\0" in data[:8000]:
            return None
        return data.decode("utf-8", "ignore")

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(timeout=10)
        except Exception:
            self.p.kill()


def list_commits(repo, limit):
    raw = common.git(repo, "log", "--no-merges", "-n", str(limit), "--name-only",
                     "--format=%x1e%H%x1f%P%x1f%s%x1f%b%x1d", timeout=300)
    out = []
    for rec in raw.split("\x1e")[1:]:
        head, _, files = rec.partition("\x1d")
        sha, parents, subj, body = (head.split("\x1f") + ["", "", "", ""])[:4]
        parents = parents.split()
        files = [f for f in files.splitlines() if f.strip()]
        src = [f for f in files if common.is_source(f)]
        if len(parents) != 1 or not (1 <= len(src) <= 4) or not common.informative(subj):
            continue
        out.append({"sha": sha, "parent": parents[0], "subject": subj, "body": body, "files": files})
    return out


def build_snapshot(repo, blobs, rev):
    from laya_spike import BM25, tokenize
    chunks = []
    for path in common.git(repo, "ls-tree", "-r", "--name-only", rev).splitlines():
        if not common.is_source(path):
            continue
        text = blobs.get(rev, path, max_bytes=200_000)
        if not text:
            continue
        for s, e, body in common.windows(text.splitlines()):
            chunks.append({"path": path, "start": s, "end": e, "text": body})
    bm = BM25([tokenize(c["path"] + " " + c["text"]) for c in chunks]) if chunks else None
    return chunks, bm


def build_repo(args):
    name, repo, max_commits, seed = args
    from laya_spike import tokenize
    from transformers import AutoTokenizer
    tok = AutoTokenizer.from_pretrained(os.path.join(common.BASE_MODEL, "tokenizer"))
    rng = random.Random(seed)
    t0 = time.time()
    commits = list_commits(repo, 6000)
    blobs = Blobs(repo)
    rows, stats, used = [], Counter(), 0
    snap_rev, snap_used, chunks, bm = None, 0, None, None
    for ci, c in enumerate(commits):
        if used >= max_commits:
            break
        if snap_rev is None or used - snap_used >= SNAP_EVERY:
            # snapshot at the parent of a commit ~SNAP_EVERY eligible commits older: files there predate this window
            snap_rev, snap_used = commits[min(len(commits) - 1, ci + SNAP_EVERY)]["parent"], used
            chunks, bm = build_snapshot(repo, blobs, snap_rev)
            stats["snapshots"] += 1
        if not chunks:
            continue
        try:
            diff = common.git(repo, "diff", "-U0", "-M", "--no-color", "--no-ext-diff", c["parent"], c["sha"], timeout=60)
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
            stats["diff_fail"] += 1
            continue
        fd = common.parse_diff_u0(diff)
        touched = set(fd) | {f["new_path"] for f in fd.values()} | set(c["files"])
        task = common.task_text(c["subject"], c["body"])
        split = "val" if int(c["sha"][:8], 16) % 10 == 0 else "train"
        pos, same, tw = [], [], []   # tw: every parent-revision window of the touched files
        for old_path, f in fd.items():
            if f["new_file"] or not f["old_lines"] or not common.is_source(old_path):
                continue
            text = blobs.get(c["parent"], old_path)
            if not text:
                continue
            for s, e, body in common.windows(text.splitlines()):
                st, vis_end = common.make_state(tok, old_path, s, e, body)
                hunk = common.overlaps(s, vis_end, f["old_lines"])
                tw.append((old_path, s, e, st, body, hunk))
                if hunk:
                    pos.append((old_path, s, e, st))
                elif not common.overlaps(s, e, f["old_lines"]):
                    same.append((old_path, s, e, st))
        if not pos:
            stats["no_pos"] += 1
            continue
        rng.shuffle(pos)
        rng.shuffle(same)
        q = tokenize(task)
        hits = [(sc, chunks[i]) for i, sc in bm.search(q, 80) if chunks[i]["path"] not in touched][:50]
        others = [ch for ch in rng.sample(chunks, min(20, len(chunks))) if ch["path"] not in touched][:2]
        base = {"repo": name, "sha": c["sha"], "split": split, "task": task}
        # (1) inference-shaped candidate list: the same BM25 over other files (snapshot) + touched files (parent),
        #     top CAND_K; labels: hunk window 1.0, other window of a touched file FILE_SOFT, other file 0.0
        merged = [(sc, "o", ch["path"], ch["start"], ch["end"], None, ch["text"]) for sc, ch in hits]
        for p_, s_, e_, st_, body_, hunk_ in tw:
            sc = common.bm25_score_doc(bm, q, tokenize(p_ + " " + body_))
            if sc > 0:
                merged.append((sc, "h" if hunk_ else "t", p_, s_, e_, st_, body_))
        merged.sort(key=lambda x: -x[0])
        seen = set()
        for sc, kind, p_, s_, e_, st_, body_ in merged[:CAND_K]:
            if st_ is None:
                st_, _ = common.make_state(tok, p_, s_, e_, body_)
            y = {"h": 1.0, "t": FILE_SOFT, "o": 0.0}[kind]
            rows.append(dict(base, path=p_, start=s_, end=e_, state=st_, label=y, kind={"h": "cand_pos", "t": "cand_file", "o": "cand_neg"}[kind]))
            seen.add((p_, s_))
        # (2) hunk positives BM25 did not surface (the semantic cases), random same-file windows, random negatives
        for p_, s_, e_, st_ in [x for x in pos if (x[0], x[1]) not in seen][:MAX_POS]:
            rows.append(dict(base, path=p_, start=s_, end=e_, state=st_, label=1.0, kind="pos"))
        for p_, s_, e_, st_ in [x for x in same if (x[0], x[1]) not in seen][:MAX_SAME]:
            rows.append(dict(base, path=p_, start=s_, end=e_, state=st_, label=SOFT, kind="same_file"))
        for ch in others:
            st_, _ = common.make_state(tok, ch["path"], ch["start"], ch["end"], ch["text"])
            rows.append(dict(base, path=ch["path"], start=ch["start"], end=ch["end"], state=st_, label=0.0, kind="rand_neg"))
        used += 1
        stats["commits_" + split] += 1
    blobs.close()
    os.makedirs(OUT, exist_ok=True)
    with open(os.path.join(OUT, "%s.jsonl" % name), "w") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")
    stats.update(Counter("%s_%s" % (r["split"], r["kind"]) for r in rows))
    stats["eligible_commits"] = len(commits)
    print("[%s] %d rows, %d commits used, %.0fs  %s" % (name, len(rows), used, time.time() - t0, dict(stats)), flush=True)
    return name, dict(stats), len(rows)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--max-commits", type=int, default=600)
    ap.add_argument("--workers", type=int, default=8)
    ap.add_argument("--only", nargs="*")
    args = ap.parse_args()
    check_leakage()
    held = {os.path.realpath(p) for p in HELDOUT.values()}
    jobs = []
    for i, (name, repo) in enumerate(sorted(TRAIN.items())):
        assert os.path.realpath(repo) not in held
        if args.only and name not in args.only:
            continue
        jobs.append((name, repo, args.max_commits, 1000 + i))
    with Pool(min(args.workers, len(jobs))) as pool:
        res = pool.map(build_repo, jobs)
    # merge
    train, val = open(os.path.join(common.WORK, "train.jsonl"), "w"), open(os.path.join(common.WORK, "val.jsonl"), "w")
    total = Counter()
    for name in sorted(TRAIN):
        path = os.path.join(OUT, "%s.jsonl" % name)
        if not os.path.exists(path):
            continue
        for line in open(path):
            r = json.loads(line)
            assert "moon" not in r["repo"] and r["repo"] not in HELDOUT
            (val if r["split"] == "val" else train).write(line)
            total["%s_%s" % (r["split"], r["kind"])] += 1
            total[r["split"]] += 1
    train.close()
    val.close()
    summary = {"per_repo": {n: s for n, s, _ in res}, "totals": dict(total), "heldout": sorted(HELDOUT),
               "soft_label_same_file": SOFT, "soft_label_cand_file": FILE_SOFT, "cand_k": CAND_K,
               "max_pos_per_commit": MAX_POS}
    json.dump(summary, open(os.path.join(common.WORK, "data_stats.json"), "w"), indent=1)
    print(json.dumps(summary["totals"], indent=1))


if __name__ == "__main__":
    main()
