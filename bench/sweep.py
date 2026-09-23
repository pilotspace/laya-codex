"""Sweep retriever/model configurations on the dev set (restarting the daemon per config).

    python3 bench/sweep.py --repo <clone> --tasks bench/dev_tasks.jsonl --out bench/results/retrieval_dev.jsonl
"""
import argparse
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
LAYA = os.environ.get("LAYA_BIN") or os.path.abspath(os.path.join(HERE, "..", "target", "release", "laya"))
MODELS = os.path.expanduser("~/.cache/laya-codex/models")

CONFIGS = [
    ("code-rrf-s256-k24-t0", {"LAYA_MODEL_DIR": MODELS + "/laya-code", "LAYA_P_THRESHOLD": "0"}),
    ("code-w0.5-s256-k24-t0", {"LAYA_MODEL_DIR": MODELS + "/laya-code", "LAYA_P_THRESHOLD": "0", "LAYA_WEIGHT": "0.5"}),
    ("code-w0.5-s256-k24-t0.35", {"LAYA_MODEL_DIR": MODELS + "/laya-code", "LAYA_P_THRESHOLD": "0.35", "LAYA_WEIGHT": "0.5", "LAYA_MIN_KEEP": "4"}),
    ("code-w0.7-s256-k24-t0", {"LAYA_MODEL_DIR": MODELS + "/laya-code", "LAYA_P_THRESHOLD": "0", "LAYA_WEIGHT": "0.7"}),
    ("code-w0.5-s128-k24-t0", {"LAYA_MODEL_DIR": MODELS + "/laya-code", "LAYA_P_THRESHOLD": "0", "LAYA_WEIGHT": "0.5", "LAYA_STATE_TOKENS": "128"}),
    ("code-w0.7-s128-k24-t0", {"LAYA_MODEL_DIR": MODELS + "/laya-code", "LAYA_P_THRESHOLD": "0", "LAYA_WEIGHT": "0.7", "LAYA_STATE_TOKENS": "128"}),
    ("code-w1.0-s128-k24-t0", {"LAYA_MODEL_DIR": MODELS + "/laya-code", "LAYA_P_THRESHOLD": "0", "LAYA_WEIGHT": "1.0", "LAYA_STATE_TOKENS": "128"}),
    ("base-w0.5-s256-k24-t0", {"LAYA_MODEL_DIR": MODELS + "/laya-base", "LAYA_P_THRESHOLD": "0", "LAYA_WEIGHT": "0.5"}),
]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    ap.add_argument("--tasks", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--budget-ms", default="5000")
    ap.add_argument("--only", default="")
    ap.add_argument("--dump-dir", default="", help="save per-task query results (for bench/size_sweep.py)")
    args = ap.parse_args()
    for tag, env in CONFIGS:
        if args.only and args.only not in tag:
            continue
        subprocess.run([LAYA, "stop"], capture_output=True)
        e = dict(os.environ, **env)
        subprocess.run([sys.executable, os.path.join(HERE, "eval_retrieval.py"), "score", "--repo", args.repo,
                        "--tasks", args.tasks, "--budget-ms", args.budget_ms, "--tag", tag, "--out", args.out]
                       + (["--dump", os.path.join(args.dump_dir, tag + ".jsonl")] if args.dump_dir else []), env=e, check=False)
    subprocess.run([LAYA, "stop"], capture_output=True)


if __name__ == "__main__":
    main()
