"""Machine-readable headline for README charts (bench/results/headline-v8.json).

    python3 bench/headline.py --out bench/results/headline-v8.json \
        --run <run dir> <tasks.jsonl> [--run <run dir> <tasks.jsonl> ...] [--raw-root <dir with <repo>/raw>]
        [--version v9 --arm branch --lex branch-lex --with-output]

pct = laya-adaptive vs baseline (ratio of sums - 1, in %), 95% CI from the paired bootstrap of
bench/stats_pooled.py (pooled = stratified by repo). laya_vs_lex = laya-adaptive vs laya-lex.
Read accuracy needs the raw transcripts (not committed); pass --raw-root when they are available.
"""
import argparse
import json
import os
import random
from collections import defaultdict

import read_accuracy
import stats_pooled as sp

# reading_plus_injected (tokens Claude reads plus tokens laya-codex injects) is the VISION.md
# token target and leads every report; reading_tokens stays right beside it for context, never
# reported alone -- a smaller-reads/bigger-injection trade must show as the loss it is.
METRICS = {
    "reading_plus_injected": sp.METRICS["reading+injected tokens"],
    "reading_tokens": sp.METRICS["code-reading tokens"],
    "total_input": sp.METRICS["total input tokens"],
    "wall_clock": sp.METRICS["wall seconds"],
    "turns": sp.METRICS["turns"],
    "cost": sp.METRICS["cost usd"],
}

LABELS = {"reading_plus_injected": "Reading + injected", "reading_tokens": "Code-reading tokens",
          "total_input": "Total input tokens", "wall_clock": "Wall-clock time", "turns": "Turns", "cost": "Cost",
          "output_tokens": "Output tokens"}

# Chart headline: reading_plus_injected first (the target), reading_tokens right beside it.
CHART_METRICS = ["reading_plus_injected", "reading_tokens", "turns", "cost", "total_input", "wall_clock"]
# Neutral by design: v9 shows reading+injected *up* 4.6% against stock even though reading_tokens
# is down -- a title that always claims "reads less" would misstate a run where the target regresses.
CHART_TITLE = "laya-codex vs stock Claude Code: code tokens reaching Claude, turns and cost"


def compare(runs, arm, base, keys, B=10000):
    repos = {name: sp.load(d, arm, base)[0] for name, d in runs}
    rng = random.Random(0)
    idx = [{n: [rng.randrange(len(p)) for _ in p] for n, p in repos.items()} for _ in range(B)]

    def cell(pairs_of, f):
        lo, hi = sp.ci([sp.ratio(pairs_of(ix), f) for ix in idx])
        return {"pct": round(100 * sp.ratio(pairs_of(None), f), 1), "lo": round(100 * lo, 1), "hi": round(100 * hi, 1)}

    pooled = {}
    per_repo = {n: {} for n in repos}
    for k in keys:
        f = METRICS[k]
        pooled[k] = cell(lambda ix: [x for n in repos for x in (repos[n] if ix is None else [repos[n][i] for i in ix[n]])], f)
        for n in repos:
            per_repo[n][k] = cell(lambda ix, n=n: repos[n] if ix is None else [repos[n][i] for i in ix[n]], f)
    q = {}
    for label, f in (("answer_recall", sp.QUALITY["answer recall (turn 1)"]),
                     ("answer_recall_both_turns", sp.QUALITY["answer recall (both turns)"])):
        allp = [x for p in repos.values() for x in p]
        lo, hi = sp.ci([sp.mean_diff([repos[n][i] for n in repos for i in ix[n]], f) for ix in idx])
        q[label] = {base: round(sum(f(b) for _, b in allp) / len(allp), 3), arm: round(sum(f(t) for t, _ in allp) / len(allp), 3),
                    "diff": round(sp.mean_diff(allp, f), 3), "lo": round(lo, 3), "hi": round(hi, 3)}
    return pooled, per_repo, q, sum(len(p) for p in repos.values())


def median_turn(turns):
    """Median turn with a task that never got there counted as the latest; None when the median
    task itself never got there. A mean over only the tasks that got there would favour the arm
    that skips the step (laya-codex tasks whose gold code was injected never need to Read it)."""
    s = sorted(float("inf") if x is None else x for x in turns)
    if not s:
        return None
    m = s[len(s) // 2] if len(s) % 2 else (s[len(s) // 2 - 1] + s[len(s) // 2]) / 2
    return None if m == float("inf") else m


def read_summary(rows, paired, arm, base, lex_arm):
    """Read accuracy (bench/read_accuracy.py rows, per repo and arm) over the paired tasks only --
    the tasks the paired metrics compare -- so every chart describes the same sessions.

    rows: {repo: {arm: [row]}}; paired: {(repo, task_id)}."""
    by = {}
    for a in (base, arm, lex_arm):
        keep = [r for repo, arms in rows.items() for r in arms.get(a, []) if (repo, r["task_id"]) in paired]
        if a != lex_arm and len(keep) != len(paired):
            raise ValueError(f"{a}: read rows for {len(keep)} of {len(paired)} paired tasks")
        by[a] = keep
    b, l = by[base], by[arm]
    mean = lambda rs, k: round(sum(r[k] for r in rs) / max(1, len(rs)), 3)
    # Pooled over every Read: a session with no Reads adds nothing, instead of dropping out of a
    # mean of per-session ratios.
    precision = lambda rs: round(sum(r["reads"] * (r["read_precision"] or 0) for r in rs) / max(1, sum(r["reads"] for r in rs)), 3)
    never = lambda rs, k: sum(1 for r in rs if r[k] is None)
    turn = lambda rs, k: {"median": median_turn([r[k] for r in rs]), "never": never(rs, k)}
    turns = lambda rs, k: sorted((r[k] for r in rs), key=lambda x: (x is None, x or 0))
    return {
        "n": len(b),
        "read_precision": {"baseline": precision(b), "laya": precision(l), "lex": precision(by[lex_arm])},
        "first_gold_read_turn": {"baseline": turn(b, "first_gold_read_turn"), "laya": turn(l, "first_gold_read_turn"),
                                 "lex": turn(by[lex_arm], "first_gold_read_turn")},
        "reads": {  # scripts/charts.py: reads chart
            "gold_seen": {"label": "Relevant code found", "baseline": mean(b, "read_recall"),
                          "laya": mean(l, "read_recall"), "better": "higher", "fmt": "{:.0%}"},
            "seen_turn": {"label": "Turn right code arrives", "better": "lower", "fmt": "{:g}",
                          "baseline": median_turn([r["first_gold_seen_turn"] for r in b]),
                          "laya": median_turn([r["first_gold_seen_turn"] for r in l]),
                          "never_baseline": never(b, "first_gold_seen_turn"), "never_laya": never(l, "first_gold_seen_turn")},
            "read_precision": {"label": "Reads on relevant code", "baseline": precision(b), "laya": precision(l),
                               "better": "higher", "fmt": "{:.0%}"},
            "wasted_read_tokens": {"label": "Wasted read tokens", "baseline": round(mean(b, "wasted_read_tokens")),
                                   "laya": round(mean(l, "wasted_read_tokens")), "better": "lower", "fmt": "{:,.0f}"},
        },
        "journey": {  # scripts/charts.py: journey chart
            "note": "Assistant turn at which a gold (correct) file first entered Claude's context; 0 = before "
                    "Claude's first turn (laya-codex injected its code with the prompt); null = never.",
            "baseline": turns(b, "first_gold_seen_turn"),
            "laya_seen": turns(l, "first_gold_seen_turn"),
            "laya_read": turns(l, "first_gold_read_turn"),
            "lex_seen": turns(by[lex_arm], "first_gold_seen_turn"),
        },
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--run", nargs=2, action="append", metavar=("RUN_DIR", "TASKS"), required=True)
    ap.add_argument("--raw-root", help="dir holding <repo>/raw transcripts (for read accuracy)")
    ap.add_argument("--version", default="v8")
    ap.add_argument("--arm", default="laya-adaptive", help="the laya-codex arm")
    ap.add_argument("--lex", default="laya-lex", help="the same build with keyword-only ranking")
    ap.add_argument("--with-output", action="store_true", help="also report output tokens (runs from v9 on)")
    ap.add_argument("--title", default=CHART_TITLE)
    a = ap.parse_args()
    arm, lex_arm = a.arm, a.lex
    if a.with_output:
        METRICS["output_tokens"] = sp.METRICS["output tokens"]
    runs = [(os.path.basename(os.path.normpath(d)), d) for d, _ in a.run]
    keys = list(METRICS)
    pooled, per_repo, q, n = compare(runs, arm, "baseline", keys)
    lex, lex_repo, lex_q, _ = compare(runs, arm, lex_arm, keys)
    lexb, _, lexb_q, _ = compare(runs, lex_arm, "baseline", keys)
    out = {"version": a.version, "n_tasks": n, "repos": [r for r, _ in runs], "model": "sonnet", "arm": arm,
           "metrics": {k: {"label": LABELS[k], **v} for k, v in pooled.items()},
           # reading_plus_injected leads (the VISION.md target), reading_tokens right beside it;
           # output_tokens (when reported) slots in just ahead of wall_clock, same as CHART_METRICS.
           "chart_metrics": CHART_METRICS[:-1] + (["output_tokens"] if a.with_output else []) + CHART_METRICS[-1:],
           "chart_title": a.title,
           "per_repo": per_repo,
           "answer_recall": {"baseline": q["answer_recall"]["baseline"], "laya": q["answer_recall"][arm],
                             "diff": q["answer_recall"]["diff"], "lo": q["answer_recall"]["lo"], "hi": q["answer_recall"]["hi"]},
           "answer_recall_both_turns": {"baseline": q["answer_recall_both_turns"]["baseline"],
                                        "laya": q["answer_recall_both_turns"][arm],
                                        "diff": q["answer_recall_both_turns"]["diff"],
                                        "lo": q["answer_recall_both_turns"]["lo"], "hi": q["answer_recall_both_turns"]["hi"]},
           "laya_vs_lex": {"note": f"{arm} vs {lex_arm} (same hooks, LAYA_BUDGET_MS=0: lexical-only ranking)",
                           "pooled": {k: lex[k] for k in ("reading_tokens", "total_input", "wall_clock", "cost")},
                           "per_repo": {r: {k: v[k] for k in ("reading_tokens", "wall_clock")} for r, v in lex_repo.items()},
                           "answer_recall": lex_q["answer_recall"]},
           "lex_vs_baseline": {"pooled": {k: lexb[k] for k in ("reading_tokens", "total_input", "wall_clock", "cost")},
                               "answer_recall": lexb_q["answer_recall"]}}
    if a.raw_root:
        rows = {}
        for (name, _), (_, tasks) in zip(runs, a.run):
            rows[name] = defaultdict(list)
            read_accuracy.collect(os.path.join(a.raw_root, name), tasks, rows[name])
        paired = {(name, t["task_id"]) for name, d in runs for t, _ in sp.load(d, arm, "baseline")[0]}
        s = read_summary(rows, paired, arm, "baseline", lex_arm)
        assert s["n"] == n, f"read rows cover {s['n']} tasks, the paired metrics {n}"
        out.update({k: s[k] for k in ("read_precision", "first_gold_read_turn", "reads", "journey")})
    with open(a.out, "w") as f:
        f.write(json.dumps(out, indent=1) + "\n")
    print(json.dumps(out, indent=1))


if __name__ == "__main__":
    main()
