"""Shared helpers for benchmark run rows (`runs.jsonl`) and hook logs.

- `load_runs` pairs rows by arm and task. A task run several times (`run_bench.py run --repeat N`)
  becomes one row whose metrics are the mean over its repeats, so the paired bootstrap still
  resamples tasks while each task's value carries less of Claude's run-to-run noise.
- `parse_arm` reads an arm spec `name[:template][@binary]`.
- `read_hook_log` sums injected tokens and lists, per prompt, how the query was ranked.
"""
import json
import os
import re

METRICS = ("wall_s", "reading_tokens", "injected_tokens", "total_input_tokens", "output_tokens", "num_turns",
           "cost_usd", "recall", "precision", "hit_any", "recall_all_turns")
ARM_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")


def tok_estimate(chars):
    return int(chars / 3.5)


def load_runs(path, arms=None):
    """{arm: {task_id: row}} with repeated (arm, task) rows averaged. Refuses rows of one task that
    mix models: a mean over two models is not a measurement of either."""
    groups = {}
    with open(path) as f:
        lines = f.readlines()
    for line in lines:
        if not line.strip():
            continue
        r = json.loads(line)
        if arms is not None and r["arm"] not in arms:
            continue
        groups.setdefault(r["arm"], {}).setdefault(r["task_id"], []).append(r)
    out = {}
    for arm, tasks in groups.items():
        for task, rows in tasks.items():
            models = {r.get("model") for r in rows}
            if len(models) > 1:
                raise ValueError("%s/%s mixes models %s" % (arm, task, sorted(map(str, models))))
            merged = dict(rows[0])
            for k in METRICS:
                vals = [r[k] for r in rows if r.get(k) is not None]
                if vals:
                    merged[k] = sum(vals) / len(vals)
                else:
                    merged.pop(k, None)
            merged["rc"] = [c for r in rows for c in r.get("rc", [])]
            merged["reps"] = len(rows)
            merged.pop("rep", None)
            out.setdefault(arm, {})[task] = merged
    return out


def parse_arm(spec):
    """`name[:template][@binary]` -> (name, template, binary). The template names the
    bench/config/laya-{settings,mcp}.<template>.json pair and defaults to the name; the binary
    defaults to the run's LAYA_CODEX_BIN. `baseline` is stock Claude Code: no template, no binary."""
    rest, _, binary = spec.partition("@")
    name, colon, template = rest.partition(":")
    if not ARM_NAME.match(name) or (colon and not ARM_NAME.match(template)) or ("@" in spec and not binary):
        raise ValueError("bad arm spec %r (want name[:template][@binary])" % spec)
    if name == "baseline":
        if colon or binary:
            raise ValueError("baseline is stock Claude Code: it takes no template or binary")
        return "baseline", None, None
    return name, template or name, binary or None


def read_hook_log(path):
    out = {"injected_tokens": 0, "hook_actions": {}, "rank_modes": [], "scored": [], "offered": [],
           "candidates": [], "prompt_injected_tokens": []}
    if not os.path.exists(path):
        return out
    with open(path) as f:
        lines = f.readlines()
    for line in lines:
        try:
            e = json.loads(line)
        except ValueError:
            continue
        tokens = tok_estimate(int(e.get("injected_chars") or 0))
        out["injected_tokens"] += tokens
        a = e.get("action", "?")
        out["hook_actions"][a] = out["hook_actions"].get(a, 0) + 1
        if e.get("event") == "UserPromptSubmit":
            out["rank_modes"].append(e.get("rank_mode"))
            out["scored"].append(e.get("scored"))
            out["offered"].append(e.get("offered"))
            out["candidates"].append(e.get("candidates"))
            out["prompt_injected_tokens"].append(tokens)
    return out
