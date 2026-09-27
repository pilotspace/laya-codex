"""Copy a benchmark run without stalled tasks, for time and token comparisons.

    python3 bench/drop_stalls.py <run dir> <out dir> [--stall-s 300]

A task is dropped from every arm when any arm's session exited non-zero or had one prompt longer
than --stall-s seconds. Such sessions are Claude API hangs, not laya-codex: the hooks time out
after at most 8 s. Benchmark v10 had stalls of up to 4 h in every arm, which swamp a mean wall
time. The as-run rows stay in <run dir>/runs.jsonl.
"""
import argparse
import json
import os


def stalled_tasks(rows, stall_s):
    return {r["task_id"] for r in rows
            if any(c for c in (r.get("rc") or [])) or max(r.get("prompt_wall_s") or [0]) > stall_s}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("run_dir")
    ap.add_argument("out_dir")
    ap.add_argument("--stall-s", type=float, default=300)
    args = ap.parse_args()
    rows = [json.loads(l) for l in open(os.path.join(args.run_dir, "runs.jsonl"))]
    bad = stalled_tasks(rows, args.stall_s)
    os.makedirs(args.out_dir, exist_ok=True)
    with open(os.path.join(args.out_dir, "runs.jsonl"), "w") as f:
        for r in rows:
            if r["task_id"] not in bad:
                f.write(json.dumps(r) + "\n")
    print("%s: %d rows, dropped tasks %s" % (args.run_dir, len(rows), sorted(bad)))


if __name__ == "__main__":
    main()
