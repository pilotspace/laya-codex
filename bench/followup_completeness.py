"""Would a follow-up 'completeness' list have spared Claude its follow-up lookups? (offline replay)

    python3 bench/followup_completeness.py --root <run root with <repo>/{runs.jsonl,raw}> \
        --repos <dir with the benchmark checkouts> [--out bench/results/followup-completeness-v13.json]

For each laya-codex session, the prompt-2 hook could inject, for every name in a set chosen from
prompt 1 (the ranked symbols, the inlined definitions, or the names Claude put in backticks in
its first answer, which the hook can read from the transcript), every indexed line that uses the
name: an exact word scan like `search` name lookups. A name's list is complete if it has at most
`cap` lines and fits the character budget. Each real prompt-2 lookup is then checked:
- `search` / Grep for names: covered if every name it asks about has a complete list;
- Read: never covered (it wants code, not a list).
A model call is removable if every tool call it made is covered. Raw transcripts are not
committed; the checkouts must be at the benchmark's commits.
"""
import argparse
import json
import os
import re
import statistics as st
import subprocess
from collections import Counter, defaultdict

IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]{2,}")
KEYWORDS = set("""fn def class struct enum trait impl mod pub use self Self async await function const let var
interface type export import from return test tests mut ref static where for in if else match true false none
None True False new this super extends implements string number boolean void any unknown str int bool len""".split())
RG_TYPES = {"moon": ["-t", "rust"], "httpx": ["-t", "py"], "hono": ["-t", "ts", "-t", "js"]}


def parse_session(lines):
    """Per prompt: the hook's injected text, Claude's tool calls (by id), the calls' tool ids, the answer,
    every API call in order (`api`: request id, first and last usage, tool count) and the prompt's
    `result_usage`."""
    out, cur, pending = [], None, ""
    for d in lines:
        t = d.get("type")
        if t == "system" and d.get("subtype") == "hook_response" and d.get("hook_event") == "UserPromptSubmit":
            raw = d.get("stdout") or d.get("output") or ""
            try:
                pending = json.loads(raw)["hookSpecificOutput"]["additionalContext"]
            except (ValueError, KeyError, TypeError):
                pending = ""
        elif t == "system" and d.get("subtype") == "init":
            cur = {"inj_text": pending, "tools": {}, "calls": [], "rids": {}, "answer": "", "api": [], "apis": {},
                   "result_usage": {}}
            pending = ""
        elif cur is None:
            continue
        elif t == "assistant":
            rid = d.get("request_id") or d["message"].get("id")
            usage = d["message"].get("usage") or {}
            if rid not in cur["apis"]:
                cur["apis"][rid] = {"rid": rid, "usage": usage, "usage_last": usage, "tools": 0}
                cur["api"].append(cur["apis"][rid])
            cur["apis"][rid]["usage_last"] = usage
            for b in d["message"].get("content") or []:
                if b.get("type") == "tool_use":
                    cur["apis"][rid]["tools"] += 1
                    cur["tools"][b["id"]] = {"name": b["name"], "input": b.get("input") or {}}
                    if rid not in cur["rids"]:
                        cur["rids"][rid] = len(cur["calls"])
                        cur["calls"].append([])
                    cur["calls"][cur["rids"][rid]].append(b["id"])
        elif t == "result":
            cur["answer"] = d.get("result") or ""
            cur["result_usage"] = d.get("usage") or {}
            del cur["rids"], cur["apis"]
            out.append(cur)
            cur = None
    return out


def _add(out, names):
    for n in names:
        if n not in KEYWORDS and n not in out:
            out.append(n)
    return out


def ranked_symbols(p1):
    """Names in prompt 1's 'Ranked locations' headers, e.g. 'fn enforce_budget'."""
    out = []
    for line in p1["inj_text"].splitlines():
        m = re.match(r"\s*\d+\. (\S+) — (.*)", line)
        if m:
            for seg in m.group(2).split(";"):
                _add(out, IDENT.findall(re.sub(r"^\s*\d+-\d+\s*", "", seg)))
    return out


def inlined_definitions(p1):
    """Names defined in prompt 1's inlined code."""
    code = "\n".join(re.findall(r"```[a-z]*\n(.*?)```", p1["inj_text"], re.S))
    defs = re.findall(r"\b(?:fn|def|class|struct|enum|trait|interface|type|function|const)\s+([A-Za-z_]\w{2,})", code)
    return _add([], defs)


def answer_names(p1):
    """Identifiers Claude put in backticks in its first answer (file paths skipped)."""
    out = []
    for tick in re.findall(r"`([^`\n]{2,80})`", p1["answer"]):
        if "/" not in tick and not re.search(r"\.(rs|py|ts|tsx|js)$", tick):
            _add(out, IDENT.findall(tick))
    return out


RULES = {
    "ranked": ranked_symbols,
    "ranked+inlined": lambda p1: _add(ranked_symbols(p1), inlined_definitions(p1)),
    "answer": answer_names,
    "answer+ranked": lambda p1: _add(answer_names(p1), ranked_symbols(p1)),
}


def lookup_names(tool):
    name, inp = tool["name"], tool["input"]
    if name == "mcp__laya-codex__search":
        parts = [x.strip() for x in inp.get("query", "").split("|") if x.strip()]
        if parts and all(re.fullmatch(r"[A-Za-z_][\w:.]*", x) for x in parts):
            return "search-name", {IDENT.findall(x)[-1] for x in parts if IDENT.findall(x)}
        return "search-words", {n for n in IDENT.findall(inp.get("query", "")) if n not in KEYWORDS}
    if name == "Grep":
        return "grep", {n for n in IDENT.findall(inp.get("pattern", "")) if n not in KEYWORDS}
    if name == "Read":
        return "read", set()
    return name.lower(), set()


def render(name, rows):
    """One name's list as the hook would inject it: file:lines, no code."""
    by_file = defaultdict(list)
    for path, line, _ in rows:
        by_file[path].append(str(line))
    return f"`{name}`: " + "; ".join(f"{p}:{','.join(v)}" for p, v in by_file.items()) + "\n"


def complete_names(names, scan, cap, budget):
    done, used, too_long = set(), 0, set()
    for n in names:
        rows = scan(n)
        if not rows:
            continue
        if len(rows) > cap:
            too_long.add(n)
            continue
        txt = render(n, rows)
        if used + len(txt) > budget:
            continue
        used += len(txt)
        done.add(n)
    return done, used


def evaluate(p2, complete, mentioned=None, too_long=frozenset()):
    covered, misses = {}, Counter()
    for tid, tool in p2["tools"].items():
        kind, ns = lookup_names(tool)
        covered[tid] = bool(ns) and kind != "read" and ns <= complete
        if covered[tid]:
            continue
        if kind == "read":
            misses["Read (wants code)"] += 1
        elif not ns:
            misses[f"{kind}: no identifier"] += 1
        elif mentioned is not None and ns - mentioned:
            misses[f"{kind}: name not mentioned in prompt 1"] += 1
        elif ns & too_long:
            misses[f"{kind}: name has too many uses"] += 1
        else:
            misses[f"{kind}: list over budget"] += 1
    removable = sum(1 for ids in p2["calls"] if ids and all(covered.get(i) for i in ids))
    return {"tools": len(covered), "all_covered": bool(covered) and all(covered.values()),
            "calls": len(p2["calls"]), "calls_removable": removable, "misses": misses}


def make_scan(repo_dir, rg_types):
    cache = {}

    def scan(name):
        if name not in cache:
            out = subprocess.run(["rg", "-n", "-w", "--no-heading", "-F", name, *rg_types, "."],
                                 cwd=repo_dir, capture_output=True, text=True).stdout
            rows = []
            for line in out.splitlines():
                path, ln, txt = line.split(":", 2)
                rows.append((path.removeprefix("./"), int(ln), txt.strip()))
            cache[name] = rows
        return cache[name]
    return scan


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--root", required=True)
    ap.add_argument("--repos", required=True)
    ap.add_argument("--repo-names", default="moon,httpx,hono")
    ap.add_argument("--arm", default="laya")
    ap.add_argument("--out")
    a = ap.parse_args()
    sessions = []
    for repo in a.repo_names.split(","):
        scan = make_scan(os.path.join(a.repos, repo), RG_TYPES[repo])
        with open(os.path.join(a.root, repo, "runs.jsonl")) as f:
            for r in (json.loads(x) for x in f if x.strip()):
                if r["arm"] != a.arm:
                    continue
                with open(os.path.join(a.root, repo, "raw", f"{r['task_id']}_{a.arm}.jsonl")) as g:
                    ps = parse_session([json.loads(x) for x in g if x.strip()])
                if len(ps) >= 2:
                    sessions.append((scan, ps[0], ps[1], len(ps[1]["inj_text"])))
    results = []
    for rule, fn in RULES.items():
        for cap in (25, 60):
            for budget in (2000, 4000, 7000):
                tot, rows = Counter(), []
                for scan, p1, p2, _ in sessions:
                    names = fn(p1)
                    done, used = complete_names(names, scan, cap, budget)
                    too_long = {n for n in names if len(scan(n)) > cap}
                    mentioned = set(answer_names(p1)) | set(ranked_symbols(p1)) | set(inlined_definitions(p1))
                    e = evaluate(p2, done, mentioned, too_long)
                    tot.update(e["misses"])
                    rows.append((e, used))
                with_lookups = [e for e, _ in rows if e["tools"]]
                res = {"rule": rule, "cap": cap, "budget": budget,
                       "followups_with_lookups": len(with_lookups),
                       "all_covered": sum(e["all_covered"] for e in with_lookups),
                       "calls": sum(e["calls"] for e, _ in rows),
                       "calls_removable": sum(e["calls_removable"] for e, _ in rows),
                       "list_chars_mean": round(st.mean(u for _, u in rows)),
                       "today_prompt2_chars_mean": round(st.mean(s[3] for s in sessions)),
                       "misses": dict(tot)}
                results.append(res)
                print(f"{rule:15s} cap {cap:3d} budget {budget:5d} | every lookup covered {res['all_covered']:3d}/"
                      f"{res['followups_with_lookups']} | calls removable {res['calls_removable']:3d}/{res['calls']} | "
                      f"list chars {res['list_chars_mean']:5d} (prompt 2 today {res['today_prompt2_chars_mean']})")
    if a.out:
        with open(a.out, "w") as f:
            json.dump({"sessions": len(sessions), "results": results}, f, indent=1)
            f.write("\n")


if __name__ == "__main__":
    main()
