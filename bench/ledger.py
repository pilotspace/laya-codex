"""Per-arm tool-call and hook-action ledger, pooled across one or more run dirs.

VISION.md's time target follows tool calls (about 2 s each): this is the ledger behind "laya-codex
sessions still make 5.1 tool calls (2.85 Grep, 2.17 Read) against 8.45 for stock Claude, and call
laya-codex `search` 0.02 times". Every `runs.jsonl` row already carries `tool_calls` (a dict of
tool name to call count, summed over the session's prompts) and `hook_actions` (likewise for the
`UserPromptSubmit`/`PreToolUse` hook's own actions); this module turns those into per-session means.

    python3 bench/ledger.py <run dir> [<run dir> ...]
"""
import json
import os
import sys

# The tools VISION.md and the plan name explicitly; anything else (Bash, WebFetch, ...) is "other".
NAMED_TOOLS = ("Grep", "Read", "Glob", "mcp__laya-codex__search")


def load_rows(paths):
    """Pool the runs.jsonl rows of one or more run dirs (or direct .jsonl paths). Missing files are
    skipped, not an error: a pooled ledger over several repos should not fail because one repo's
    run has not landed yet."""
    rows = []
    for p in paths:
        path = os.path.join(p, "runs.jsonl") if os.path.isdir(p) else p
        if not os.path.exists(path):
            continue
        with open(path) as f:
            rows += [json.loads(l) for l in f if l.strip()]
    return rows


def _by_arm(rows):
    out = {}
    for r in rows:
        out.setdefault(r["arm"], []).append(r)
    return out


def tool_ledger(rows, tools=NAMED_TOOLS):
    """{arm: {tool: mean calls/session, ..., "other": mean calls/session of unnamed tools,
    "total": mean calls/session of every tool}}. A row without `tool_calls` (older runs.jsonl, or a
    session that made none) counts as zero calls, not an error."""
    out = {}
    for arm, rs in _by_arm(rows).items():
        n = len(rs)
        counts = {t: 0 for t in tools}
        other = total = 0
        for r in rs:
            for name, c in (r.get("tool_calls") or {}).items():
                total += c
                if name in counts:
                    counts[name] += c
                else:
                    other += c
        cell = {t: counts[t] / n for t in tools}
        cell["other"] = other / n
        cell["total"] = total / n
        out[arm] = cell
    return out


def hook_action_ledger(rows):
    """{arm: {action: mean count/session}}. A row without `hook_actions` counts as no actions."""
    out = {}
    for arm, rs in _by_arm(rows).items():
        n = len(rs)
        totals = {}
        for r in rs:
            for action, c in (r.get("hook_actions") or {}).items():
                totals[action] = totals.get(action, 0) + c
        out[arm] = {a: c / n for a, c in totals.items()}
    return out


def render(rows, tools=NAMED_TOOLS):
    tl = tool_ledger(rows, tools)
    hl = hook_action_ledger(rows)
    arms = sorted(tl, key=lambda a: (a != "baseline", a))
    actions = sorted({a for h in hl.values() for a in h})
    sessions = {}
    for r in rows:
        sessions[r["arm"]] = sessions.get(r["arm"], 0) + 1
    header = ["arm", "sessions", "tool calls/session"] + list(tools) + ["other"] + (actions or ["(no hook actions)"])
    lines = ["| " + " | ".join(header) + " |", "|---" * len(header) + "|"]
    for arm in arms:
        cell, hcell = tl[arm], hl.get(arm, {})
        row = ["%s" % arm, "%d" % sessions.get(arm, 0), "%.2f" % cell["total"]]
        row += ["%.2f" % cell[t] for t in tools]
        row.append("%.2f" % cell["other"])
        row += (["%.2f" % hcell.get(a, 0.0) for a in actions] if actions else ["–"])
        lines.append("| " + " | ".join(row) + " |")
    return "\n".join(lines)


def main(argv):
    if not argv:
        sys.exit("usage: python3 bench/ledger.py <run dir> [<run dir> ...]")
    print(render(load_rows(argv)))


if __name__ == "__main__":
    main(sys.argv[1:])
