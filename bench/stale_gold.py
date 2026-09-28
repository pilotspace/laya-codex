"""Which gold files can a search still find? Benchmark tasks are past commits (the task id is the
commit), run on a checkout pinned 40 or more commits later. A gold file whose change was since
rewritten, or whose change only deleted code, carries no trace of the task in that checkout, so
no tool can find it from the task text.

    python3 bench/stale_gold.py --out bench/results/stale-gold-v8.json \\
        --repo moon <moon checkout> bench/tasks-v8/moon.jsonl \\
        --repo httpx <httpx checkout> bench/tasks-v8/httpx.jsonl \\
        --repo hono <hono checkout> bench/tasks-v8/hono.jsonl

Per gold file: the share of the lines the commit added that are still in the checkout's copy.
present (>= half still there), drifted (less), removal only (the commit added nothing there),
file gone. Read-only: runs `git show` on the checkouts, which must be at the pinned commits
(docs/RESULTS.md, benchmark v2 design).
"""
import argparse
import json
import os
import subprocess

PRESENT_SHARE = 0.5
TRIVIAL = {"}", "});", "})", "};", "} else {", "else:", "pass", "return;", "return", "end"}


def meaningful_added(lines):
    """Added lines that can identify the change: not blank, not a lone brace or keyword, not a comment."""
    out = []
    for line in lines:
        s = line.strip()
        if len(s) < 8 or s in TRIVIAL or s.startswith(("//", "#", "*", "/*")):
            continue
        out.append(line)
    return out


def classify(added, head_text):
    """(kind, share of added lines still present) for one gold file."""
    if head_text is None:
        return "file gone", 0.0
    if not added:
        return "removal only", 0.0
    share = sum(1 for a in added if a.strip() in head_text) / len(added)
    return ("present" if share >= PRESENT_SHARE else "drifted"), round(share, 2)


def added_lines(checkout, sha, path):
    diff = subprocess.run(["git", "-C", checkout, "show", "--format=", "-U0", sha, "--", path],
                          capture_output=True, text=True, errors="ignore", check=True).stdout
    return meaningful_added(l[1:] for l in diff.splitlines() if l.startswith("+") and not l.startswith("+++"))


def audit(checkout, tasks_path):
    out = {}
    for line in open(tasks_path):
        if not line.strip():
            continue
        t = json.loads(line)
        gold = {}
        for g in t["gold"]:
            path = os.path.join(checkout, g)
            head = open(path, errors="ignore").read() if os.path.isfile(path) else None
            kind, share = classify(added_lines(checkout, t["id"], g), head)
            gold[g] = {"kind": kind, "present": share}
        out[t["id"]] = gold
    return out


def findable(audit_json):
    """{task_id: set of gold files still present} from a stale-gold JSON."""
    return {tid: {g for g, v in gold.items() if v["kind"] == "present"}
            for repo in audit_json["repos"].values() for tid, gold in repo.items()}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--repo", nargs=3, action="append", required=True, metavar=("NAME", "CHECKOUT", "TASKS"))
    a = ap.parse_args()
    repos, heads = {}, {}
    for name, checkout, tasks in a.repo:
        heads[name] = subprocess.run(["git", "-C", checkout, "rev-parse", "HEAD"], capture_output=True, text=True,
                                     check=True).stdout.strip()
        repos[name] = audit(checkout, tasks)
    kinds = {}
    for repo in repos.values():
        for gold in repo.values():
            for v in gold.values():
                kinds[v["kind"]] = kinds.get(v["kind"], 0) + 1
    stale_tasks = sum(1 for repo in repos.values() for gold in repo.values()
                      if not any(v["kind"] == "present" for v in gold.values()))
    doc = {"note": __doc__.split("\n\n")[0], "checkouts": heads, "gold_files": kinds,
           "tasks_without_findable_gold": stale_tasks, "repos": repos}
    with open(a.out, "w") as f:
        f.write(json.dumps(doc, indent=1) + "\n")
    print(json.dumps({k: doc[k] for k in ("checkouts", "gold_files", "tasks_without_findable_gold")}, indent=1))


if __name__ == "__main__":
    main()
