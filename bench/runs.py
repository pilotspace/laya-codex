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
# claude-sonnet-5-5 list prices, dollars per million tokens; Claude Code writes the one-hour cache.
# They reproduce every v13 and v14 result's modelUsage costUSD exactly.
PRICES_PER_M = {"input_tokens": 2.0, "cache_creation_input_tokens": 4.0, "cache_read_input_tokens": 0.20,
                "output_tokens": 10.0}


def priced(usage):
    """Dollars for an API `usage` dict at PRICES_PER_M."""
    return sum((usage.get(k) or 0) * p for k, p in PRICES_PER_M.items()) / 1e6


# Tokens per character of each kind of text the benchmark counts, calibrated on what the API bills
# (benchmarks v12-v14, claude-sonnet-5-5). Tool results: regress the cache write (plus uncached
# input) of the API call after a tool result on that result's characters, per tool, with the call
# count, tool-input characters, thinking and text as covariates (n = 1241 calls, R^2 0.98; 95% CIs
# Read 0.416-0.434, Grep 0.451-0.470, Glob 0.494-0.624, search 0.436-0.455). Injected code and the
# first answer: regress the follow-up's first cache write on the hook log's injected characters and
# the answer's characters (n = 241, R^2 0.98; injected 0.444-0.468, answer 0.433-0.478). `search`
# is laya-codex's MCP tool, counted on its JSON-encoded result as parse_stream sees it. Each run
# alone gives the same code rates within 0.02 (the answer rate within 0.04). The 3.5 chars per
# token used until 2026-10-07 counted code 1.5-1.6x low.
TOKENS_PER_CHAR = {"Read": 0.424, "Grep": 0.460, "Glob": 0.554, "search": 0.445, "injected": 0.456, "answer": 0.455}


def tok_estimate(chars, kind):
    """Estimated tokens of `chars` characters of `kind` text (a TOKENS_PER_CHAR key)."""
    return int(round(chars * TOKENS_PER_CHAR[kind]))


def load_runs(path, arms=None):
    """{arm: {task_id: row}} with repeated (arm, task) rows averaged. Refuses rows of one task that
    mix models: a mean over two models is not a measurement of either.

    A row whose laya-codex injection failed (`injection_ok`: False, see run_bench.py) is dropped once
    a healthy rerun of the same (arm, task) exists (`--rerun-unhealthy` reruns it; the old row stays
    in runs.jsonl). Rows without the field (baseline, or runs from before this field existed) are
    never dropped by this. If every row of a task is unhealthy, they are kept (nothing better to
    average) and the merged row is marked `unhealthy: True`."""
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
        for task, all_rows in tasks.items():
            healthy = [r for r in all_rows if r.get("injection_ok") is not False]
            rows = healthy or all_rows
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
            if not healthy:
                merged["unhealthy"] = True
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
           "candidates": [], "prompt_injected_tokens": [], "prompt_actions": []}
    if not os.path.exists(path):
        return out
    with open(path) as f:
        lines = f.readlines()
    for line in lines:
        try:
            e = json.loads(line)
        except ValueError:
            continue
        tokens = tok_estimate(int(e.get("injected_chars") or 0), "injected")
        out["injected_tokens"] += tokens
        a = e.get("action", "?")
        out["hook_actions"][a] = out["hook_actions"].get(a, 0) + 1
        if e.get("event") == "UserPromptSubmit":
            out["rank_modes"].append(e.get("rank_mode"))
            out["scored"].append(e.get("scored"))
            out["offered"].append(e.get("offered"))
            out["candidates"].append(e.get("candidates"))
            out["prompt_injected_tokens"].append(tokens)
            out["prompt_actions"].append(a)
    return out
