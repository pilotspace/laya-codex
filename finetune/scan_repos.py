"""List local git repos with >= N non-merge commits and their source-language mix (read-only).

    python3 finetune/scan_repos.py [root] [min_commits]
"""
import os
import subprocess
import sys
from collections import Counter

ROOT = sys.argv[1] if len(sys.argv) > 1 else os.path.expanduser(os.environ.get("LAYA_REPOS_ROOT", "~/src"))
MIN = int(sys.argv[2]) if len(sys.argv) > 2 else 300
EXT = {".rs", ".py", ".ts", ".tsx", ".js", ".go", ".java", ".kt", ".c", ".cpp", ".swift", ".rb"}


def run(*a):
    try:
        r = subprocess.run(a, capture_output=True, text=True, timeout=60)
    except subprocess.TimeoutExpired:
        return ""
    return r.stdout if r.returncode == 0 else ""


def main():
    seen = set()
    for dirpath, dirnames, _ in os.walk(ROOT):
        depth = dirpath[len(ROOT):].count(os.sep)
        dirnames[:] = [d for d in dirnames if d not in ("node_modules", ".claude", "target", ".venv", "venv", "vendor")]
        if depth > 3:
            dirnames[:] = []
            continue
        if not os.path.exists(os.path.join(dirpath, ".git")):
            continue
        top = run("git", "-C", dirpath, "rev-parse", "--show-toplevel").strip()
        if not top or top in seen:
            continue
        seen.add(top)
        n = int(run("git", "-C", top, "rev-list", "--no-merges", "--count", "HEAD").strip() or 0)
        if n < MIN:
            continue
        c = Counter(os.path.splitext(p)[1] for p in run("git", "-C", top, "ls-files").splitlines())
        mix = " ".join("%s:%d" % (e, k) for e, k in c.most_common() if e in EXT)
        print("%6d  %s  | %s" % (n, top, mix), flush=True)


if __name__ == "__main__":
    main()
