"""Injection forensics: where does Claude still spend reading tokens, turns and time after laya
injected its context, and what would each candidate optimisation remove?

Offline only: reads benchmark transcripts (`claude -p --output-format stream-json --verbose
--include-hook-events`), the task gold files, and the benchmarked repo checkout (to size spans
that were listed but not inlined). Never talks to the laya daemon or to Claude.

Every post-injection Read is split *per returned line* (Read results carry startLine/numLines)
into one class, in this priority order:

  a  redundant    line lies inside a span whose full code was already inlined
  d  expansion    same file as an inlined span, within +-100 lines of it (context the chunk lacked)
  b  map-hit      line lies inside a range listed in "Ranked locations" / "Related by references"
                  but not inlined
  b2 map-file     file is listed in the map / related section, line outside every listed range
  c  gold-miss    gold file that the injection does not mention at all (retrieval miss)
  f  other        any other file (exploration)
  h  hook-file    Claude Code persisted an oversized hook output to a file and Claude Read it

Grep/Glob calls are split into
  e  grep-locate  the pattern names an identifier that is present in the injection (symbol in
                  the map/related list or identifier in the inlined code): Claude verifying or
                  locating usages of something it was already shown
  g  grep-explore any other pattern

A turn is one API call (one assistant message id); it takes the class that carries most of its
tool-result tokens ("answer" for the final text-only turn). Time per turn comes from event
timestamps (cycle = end of this turn's tool results - end of the previous turn's).

Baseline sessions are classified against the injection the reference laya arm received for the
same task, i.e. "how much of what stock Claude read would laya have handed it up front".

    python3 bench/forensics.py <run dir> [--tasks bench/tasks.jsonl] [--repo <checkout>]
        [--ref-arm laya-refs] [--json out.json] [--counterfactual]
"""
import argparse
import datetime as dt
import json
import os
import re
import sys
from collections import defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_REPO = os.environ.get("LAYA_CODEX_BENCH_REPO", "moon")  # the benchmarked moon checkout
CLASSES = ["a_redundant", "d_expansion", "b_map_hit", "b2_map_file", "c_gold_miss", "f_other", "h_hook_file",
           "e_grep_locate", "g_grep_explore"]
READ_CLASSES = CLASSES[:7]
NEAR = 100  # lines: "adjacent to an inlined span"
HOOK_CAP_CHARS = 10_000  # Claude Code persists larger hook additionalContext to a file (observed in v1)
# Sonnet list prices, $/M tokens
P_IN, P_CW, P_CR, P_OUT = 3.0, 3.75, 0.30, 15.0

KEYWORDS = set("""fn pub struct impl let mut use mod self Self crate enum trait const static async await match
where type return true false Some None Ok Err if else for while loop in as ref dyn move super unsafe extern
def class import from self return None True False the and not""".split())


def tok(text):
    return int(len(text) / 3.5)


def ts(s):
    return dt.datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp() if s else None


def rel(path, repo):
    p = path or ""
    for pre in (repo.rstrip("/") + "/", "./"):
        if p.startswith(pre):
            p = p[len(pre):]
    return p


# ---------------------------------------------------------------- injection parsing

def hook_context(output):
    try:
        return json.loads(output)["hookSpecificOutput"].get("additionalContext", "") or ""
    except (ValueError, KeyError, TypeError):
        return output or ""


HEAD_RE = re.compile(r"^### (\S+?):(\d+)-(\d+)(?: — (.*?))?(?: \(p=([\d.]+)\))?$")
MAP_RE = re.compile(r"^\d+\. (\S+) — (.*)$")
REL_RE = re.compile(r"^- (\S+?):(\d+)-(\d+)(?: (.*?))? — (.*)$")
PART_RE = re.compile(r"^(\d+)-(\d+)(?: (.*))?$")
IDENT_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


def parse_injection(ctx):
    """-> dict(inlined=[(path,a,b,sym,code)], mapped=[(path,a,b,sym,rank)], related=[(path,a,b,sym)],
    idents=set, chars=int). `rank` is the 1-based position of the span in the ranked list."""
    inj = {"inlined": [], "mapped": [], "related": [], "idents": set(), "chars": len(ctx), "text": ctx}
    lines = ctx.splitlines()
    section, i, rank = None, 0, 0
    while i < len(lines):
        ln = lines[i]
        if ln.startswith("Ranked locations:"):
            section = "map"
        elif ln.startswith("Related by references:"):
            section = "rel"
        elif ln.startswith("### "):
            section = None
            m = HEAD_RE.match(ln)
            if m:
                code = []
                j = i + 1
                if j < len(lines) and lines[j].startswith("```"):
                    j += 1
                    while j < len(lines) and not lines[j].startswith("```"):
                        code.append(lines[j])
                        j += 1
                inj["inlined"].append((m.group(1), int(m.group(2)), int(m.group(3)), m.group(4) or "", "\n".join(code)))
                i = j
        elif section == "map":
            m = MAP_RE.match(ln)
            if m:
                for part in m.group(2).split("; "):
                    pm = PART_RE.match(part.strip())
                    if pm:
                        rank += 1
                        inj["mapped"].append((m.group(1), int(pm.group(1)), int(pm.group(2)), pm.group(3) or "", rank))
        elif section == "rel":
            m = REL_RE.match(ln)
            if m:
                inj["related"].append((m.group(1), int(m.group(2)), int(m.group(3)), m.group(4) or ""))
        i += 1
    if not inj["mapped"]:  # full renderer (v1): the inlined spans are the ranked list
        inj["mapped"] = [(p, a, b, s, k + 1) for k, (p, a, b, s, _) in enumerate(inj["inlined"])]
    for p, a, b, s, code in inj["inlined"]:
        inj["idents"].update(IDENT_RE.findall(code))
        inj["idents"].update(IDENT_RE.findall(s))
    for p, a, b, s, *_ in inj["mapped"] + inj["related"]:
        inj["idents"].update(IDENT_RE.findall(s))
        inj["idents"].add(os.path.splitext(os.path.basename(p))[0])
    return inj


def merge_injections(injs):
    out = {"inlined": [], "mapped": [], "related": [], "idents": set(), "chars": 0, "text": ""}
    for j in injs:
        for k in ("inlined", "mapped", "related"):
            out[k] += j[k]
        out["idents"] |= j["idents"]
        out["chars"] += j["chars"]
    return out


def inj_files(inj):
    return {"inlined": {p for p, *_ in inj["inlined"]}, "mapped": {p for p, *_ in inj["mapped"]},
            "related": {p for p, *_ in inj["related"]}}


# ---------------------------------------------------------------- session parsing

def parse_session(path, repo):
    """Walk one transcript. Returns dict with injections, calls (in order) and result stats."""
    s = {"injections": [], "calls": [], "results": [], "turn_end_ts": {}, "turn_start_ts": {}, "msg_order": [],
         "final_texts": []}
    names = {}
    prompt_no = 0
    cur_msg = None
    last_ts = None
    for line in open(path):
        try:
            e = json.loads(line)
        except ValueError:
            continue
        t = e.get("type")
        et = ts(e.get("timestamp"))
        if t == "system" and e.get("subtype") == "hook_response" and e.get("hook_event") == "UserPromptSubmit":
            prompt_no += 1
            s["injections"].append((prompt_no, parse_injection(hook_context(e.get("output")))))
        elif t == "user" and isinstance(e.get("message", {}).get("content"), str):
            pass
        elif t == "assistant":
            mid = e["message"].get("id")
            if mid != cur_msg:
                cur_msg = mid
                s["msg_order"].append((mid, max(prompt_no, 1)))
                s["turn_start_ts"][mid] = et
            s["turn_end_ts"][mid] = et
            for c in e["message"].get("content", []) or []:
                if c.get("type") == "tool_use":
                    call = {"id": c["id"], "name": c["name"], "input": c.get("input") or {}, "msg": mid,
                            "turn": len(s["msg_order"]), "prompt": max(prompt_no, 1), "tokens": 0, "text": "",
                            "meta": None, "error": False}
                    names[c["id"]] = call
                    s["calls"].append(call)
                elif c.get("type") == "text":
                    s["final_texts"].append((len(s["msg_order"]), c.get("text", "")))
        elif t == "user":
            for c in e.get("message", {}).get("content", []) or []:
                if isinstance(c, dict) and c.get("type") == "tool_result" and c.get("tool_use_id") in names:
                    call = names[c["tool_use_id"]]
                    body = c.get("content")
                    call["text"] = body if isinstance(body, str) else json.dumps(body)
                    call["tokens"] = tok(call["text"])
                    call["meta"] = e.get("tool_use_result")
                    call["error"] = bool(c.get("is_error"))
                    if et and call["msg"] in s["turn_end_ts"]:
                        s["turn_end_ts"][call["msg"]] = max(s["turn_end_ts"][call["msg"]], et)
        elif t == "result":
            s["results"].append({"num_turns": e.get("num_turns"), "duration_ms": e.get("duration_ms"),
                                 "usage": e.get("usage") or {}, "result": e.get("result") or "",
                                 "cost": e.get("total_cost_usd"), "end_ts": last_ts})
        if et:
            last_ts = et
    return s


def read_range(call, repo):
    """(repo-relative path, first line, last line) actually returned by a Read, or None."""
    m = call["meta"]
    p = rel(call["input"].get("file_path", ""), repo)
    if isinstance(m, dict) and isinstance(m.get("file"), dict):
        f = m["file"]
        start = int(f.get("startLine") or 1)
        n = int(f.get("numLines") or 0)
        return p, start, start + max(n, 1) - 1
    nums = [int(x) for x in re.findall(r"^\s*(\d+)\t", call["text"], re.M)]  # cat -n style fallback
    if nums:
        return p, min(nums), max(nums)
    return None


def within(line, spans, pad=0):
    return any(a - pad <= line <= b + pad for a, b in spans)


def classify_read(call, inj, gold, repo):
    """-> {class: n_lines}, path, gold?"""
    rr = read_range(call, repo)
    if rr is None:
        return {"f_other": 1}, rel(call["input"].get("file_path", ""), repo), False
    p, lo, hi = rr
    if "/tool-results/hook-" in p:
        return {"h_hook_file": hi - lo + 1}, p, False
    inl = [(a, b) for q, a, b, *_ in inj["inlined"] if q == p]
    listed = [(a, b) for q, a, b, *_ in inj["mapped"] + inj["related"] if q == p]
    files = set().union(*inj_files(inj).values())
    out = defaultdict(int)
    for ln in range(lo, hi + 1):
        if within(ln, inl):
            out["a_redundant"] += 1
        elif within(ln, inl, NEAR):
            out["d_expansion"] += 1
        elif within(ln, listed):
            out["b_map_hit"] += 1
        elif p in files:
            out["b2_map_file"] += 1
        elif p in gold:
            out["c_gold_miss"] += 1
        else:
            out["f_other"] += 1
    return dict(out), p, p in gold


def pattern_idents(pattern):
    p = re.sub(r"\\[a-zA-Z]", " ", pattern or "")
    return [w for w in IDENT_RE.findall(p) if len(w) >= 3 and w not in KEYWORDS]


def strong_ident(w):
    return "_" in w or re.search(r"[a-z][A-Z]", w) is not None or len(w) >= 8


def classify_search(call, inj):
    words = pattern_idents(call["input"].get("pattern", ""))
    hits = [w for w in words if w in inj["idents"] and strong_ident(w)]
    return ("e_grep_locate" if hits else "g_grep_explore"), hits


def grep_files(call, repo):
    m = call["meta"]
    if isinstance(m, dict):
        fs = list(m.get("filenames") or [])
        if not fs and m.get("content"):
            fs = sorted({rel(x, repo) for x in re.findall(r"^([^\s:]+\.[A-Za-z]+)[:-]\d+[:-]", m["content"], re.M)})
            if not fs and call["input"].get("path", "").endswith(tuple(".rs .py .ts .go".split())):
                fs = [rel(call["input"]["path"], repo)]
        return [rel(f, repo) for f in fs]
    return []


# ---------------------------------------------------------------- per-session analysis

def grade_named(answer):
    m = re.findall(r"FILES:\s*(.+)", answer or "")
    return [p.strip().strip("`").lstrip("./") for p in (m[-1].split(",") if m else []) if p.strip()]


def match(n, g):
    return n == g or g.endswith("/" + n) or n.endswith(g)


def analyse(path, task, repo, ref_inj=None):
    s = parse_session(path, repo)
    gold = set(task["gold"])
    own = merge_injections([j for _, j in s["injections"]])
    inj = own if s["injections"] else (ref_inj or merge_injections([]))
    files = inj_files(inj)
    allf = set().union(*files.values())
    cls_tok, cls_calls = defaultdict(int), defaultdict(int)
    turn_tok = defaultdict(lambda: defaultdict(int))
    call_rows = []
    gold_seen_turn = {}
    named_seen_turn = {}
    named = grade_named(s["results"][0]["result"] if s["results"] else "")
    for g in gold:
        if g in files["inlined"]:
            gold_seen_turn[g] = 0
    for c in s["calls"]:
        if c["name"] == "Read":
            lines, p, isgold = classify_read(c, inj, gold, repo)
            total = sum(lines.values()) or 1
            for k, v in lines.items():
                share = c["tokens"] * v / total
                cls_tok[k] += share
                turn_tok[c["turn"]][k] += share
            primary = max(lines, key=lines.get)
            cls_calls[primary] += 1
            if isgold and p not in gold_seen_turn:
                gold_seen_turn[p] = c["turn"]
            for n in named:
                if match(n, p) and n not in named_seen_turn:
                    named_seen_turn[n] = c["turn"]
            call_rows.append({"turn": c["turn"], "tool": "Read", "arg": p, "range": read_range(c, repo)[1:] if read_range(c, repo) else None,
                              "tokens": c["tokens"], "class": primary, "lines": lines, "gold": isgold,
                              "full": "offset" not in c["input"] and "limit" not in c["input"]})
        elif c["name"] in ("Grep", "Glob") or c["name"].startswith("mcp__laya"):
            k, hits = classify_search(c, inj) if c["name"] != "Glob" else ("g_grep_explore", [])
            if c["name"].startswith("mcp__laya"):
                k = "g_grep_explore"
            cls_tok[k] += c["tokens"]
            cls_calls[k] += 1
            turn_tok[c["turn"]][k] += c["tokens"]
            gf = grep_files(c, repo)
            call_rows.append({"turn": c["turn"], "tool": c["name"], "arg": c["input"].get("pattern") or c["input"].get("query"),
                              "tokens": c["tokens"], "class": k, "hits": hits,
                              "gold_revealed": sorted(set(gf) & gold), "new_gold_revealed": sorted((set(gf) & gold) - allf)})
    n_msgs = len(s["msg_order"])
    # turn class + time
    res = s["results"]
    dur = sum((r["duration_ms"] or 0) for r in res) / 1000.0
    turn_class, turn_time = {}, {}
    prev_end = None
    for idx, (mid, pno) in enumerate(s["msg_order"], 1):
        end = s["turn_end_ts"].get(mid)
        start = prev_end if prev_end else None
        turn_time[idx] = (end - start) if (end and start) else None
        prev_end = end
        tt = turn_tok.get(idx)
        turn_class[idx] = max(tt, key=tt.get) if tt else "answer"
    # first turn: whatever of the reported duration the timestamps do not explain
    known = sum(v for v in turn_time.values() if v)
    if 1 in turn_time and turn_time[1] is None:
        turn_time[1] = max(0.0, dur - known) if len(res) <= 1 else None
    cls_turns = defaultdict(int)
    cls_time = defaultdict(float)
    for idx, k in turn_class.items():
        cls_turns[k] += 1
        cls_time[k] += turn_time.get(idx) or 0.0
    last_gold = max(gold_seen_turn.values()) if gold_seen_turn else None
    last_named = max(named_seen_turn.values()) if named_seen_turn else None
    tail_turns = (n_msgs - last_gold) if last_gold is not None else None
    tail_time = sum((turn_time.get(i) or 0) for i in range(last_gold + 1, n_msgs + 1)) if last_gold is not None else None
    tail_tokens = sum(sum(turn_tok[i].values()) for i in range(last_gold + 1, n_msgs + 1)) if last_gold is not None else None
    cov = {g: ("inlined" if g in files["inlined"] else "mapped" if g in files["mapped"] else
               "related" if g in files["related"] else "absent") for g in gold}
    named_hit_gold = [n for n in named if any(match(n, g) for g in gold)]
    u = defaultdict(int)
    for r in res:
        for k in ("input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens", "output_tokens"):
            u[k] += (r["usage"] or {}).get(k) or 0
    return {
        "task": task["id"], "task_idents": sorted(set(IDENT_RE.findall(task["task"]))), "n_gold": len(gold), "api_calls": n_msgs, "num_turns": sum((r["num_turns"] or 0) for r in res),
        "wall_s": dur, "cost": sum((r["cost"] or 0) for r in res), "usage": dict(u),
        "inj_chars": own["chars"], "inj_tokens": tok("x" * own["chars"]), "inj_persisted": any(j["chars"] > HOOK_CAP_CHARS for _, j in s["injections"]),
        "n_inlined": len(inj["inlined"]), "n_mapped": len(inj["mapped"]), "n_related": len(inj["related"]),
        "cls_tok": dict(cls_tok), "cls_calls": dict(cls_calls), "cls_turns": dict(cls_turns), "cls_time": dict(cls_time),
        "reading_tokens": sum(cls_tok.values()),
        "gold_cov": cov, "named": named, "named_in_inj": [n for n in named if any(match(n, f) for f in allf)],
        "named_in_inlined": [n for n in named if any(match(n, f) for f in files["inlined"])],
        "recall": len({g for g in gold if any(match(n, g) for n in named)}) / len(gold),
        "named_hit_gold": named_hit_gold,
        "first_gold_read_turn": min((t for t in gold_seen_turn.values() if t > 0), default=None),
        "last_gold_turn": last_gold, "last_named_turn": last_named, "tail_turns": tail_turns, "tail_time": tail_time,
        "tail_tokens": tail_tokens, "calls": call_rows, "turn_time": turn_time, "turn_class": turn_class,
        "injected_files": {k: sorted(v) for k, v in files.items()},
        "_inj": inj,
    }


# ---------------------------------------------------------------- counterfactuals

_file_cache = {}


def file_lines(repo, p):
    if p not in _file_cache:
        try:
            _file_cache[p] = open(os.path.join(repo, p), errors="ignore").read().splitlines()
        except OSError:
            _file_cache[p] = []
    return _file_cache[p]


def span_tokens(repo, p, a, b):
    ls = file_lines(repo, p)
    return tok("\n".join(ls[a - 1:b])) + 12  # + heading / fence overhead


ITEM_RE = re.compile(r"^\s*(pub(\([^)]*\))?\s+)?(async\s+)?(unsafe\s+)?(const\s+)?(fn|struct|enum|trait|macro_rules!)\s|^\s*(async\s+)?def\s")


def enclosing_item(repo, p, a, b, max_lines=400):
    """Approximate enclosing item (fn/impl/... for Rust, def/class for Python) of lines a..b:
    nearest item header at or above `a`, extended to its matching close brace (Rust) or to the
    next line with indentation <= the header's (Python). Returns (start, end) or (a, b)."""
    ls = file_lines(repo, p)
    if not ls:
        return a, b
    head = None
    for i in range(min(a, len(ls)) - 1, max(-1, a - 1 - max_lines), -1):
        if ITEM_RE.match(ls[i]):
            head = i
            break
    if head is None:
        return a, b
    if p.endswith(".py"):
        ind = len(ls[head]) - len(ls[head].lstrip())
        end = head + 1
        while end < len(ls) and (not ls[end].strip() or len(ls[end]) - len(ls[end].lstrip()) > ind):
            end += 1
        return head + 1, max(b, end)
    depth, seen, end = 0, False, None
    for i in range(head, min(len(ls), head + max_lines)):
        code = re.sub(r'"(\\.|[^"\\])*"', '""', ls[i].split("//")[0])
        depth += code.count("{") - code.count("}")
        seen = seen or "{" in code
        if seen and depth <= 0:
            end = i + 1
            break
        if not seen and code.rstrip().endswith(";"):
            end = i + 1
            break
    if end is None or end < a:
        return a, b
    return head + 1, max(b, end)


def counterfactual(rows, repo, sessions_inj):
    """Estimate, per laya session, the reading tokens / API turns / seconds removed by each option.
    A turn is removed only when every tool call in it is removed. Seconds removed = measured cycle
    time of removed turns. Added injection tokens are priced as (cache write + cache read on every
    remaining turn), expressed in dollars and in 'effective input tokens'."""
    out = {}

    def removed(r, pred):
        by_turn = defaultdict(list)
        for c in r["calls"]:
            by_turn[c["turn"]].append(c)
        tokens = sum(c["tokens"] * pred(c) for c in r["calls"])  # pred -> fraction removed
        turns = [t for t, cs in by_turn.items() if all(pred(c) >= 0.8 for c in cs)]
        secs = sum(r["turn_time"].get(t) or 0 for t in turns)
        return tokens, len(turns), secs

    def frac(c, keys):
        if c["tool"] != "Read":
            return 0.0
        tot = sum(c["lines"].values()) or 1
        return sum(v for k, v in c["lines"].items() if k in keys) / tot

    for K in (3, 5, 6, 8, 10):
        tot = defaultdict(float)
        for r in rows:
            inj = sessions_inj[r["task"]]
            already = {(p, a, b) for p, a, b, *_ in inj["inlined"]}  # full renderer: nothing to add
            extra = [m for m in inj["mapped"] if 3 < m[4] <= K and (m[0], m[1], m[2]) not in already]
            extra_ranges = defaultdict(list)
            for p, a, b, s, rank in extra:
                extra_ranges[p].append((a, b))
            add_tok = sum(span_tokens(repo, p, a, b) for p, a, b, s, rank in extra)

            def pred(c):
                if c["tool"] != "Read" or not c.get("range"):
                    return 0.0
                lo, hi = c["range"]
                rr = extra_ranges.get(c["arg"], [])
                inside = sum(1 for ln in range(lo, hi + 1) if within(ln, rr, 0))
                return inside / max(1, hi - lo + 1)
            t, n, sec = removed(r, pred)
            remaining = max(1, r["api_calls"] - n)
            tot["reading_saved"] += t
            tot["turns_saved"] += n
            tot["secs_saved"] += sec
            tot["inj_added"] += add_tok
            tot["usd_added"] += add_tok * (P_CW + P_CR * (remaining - 1)) / 1e6
        out["inline_top_%d" % K] = dict(tot)
    # whole enclosing item instead of chunk for the inlined spans: removes expansion reads that
    # fall inside the enclosing item; adds (item - chunk) tokens.
    tot = defaultdict(float)
    for r in rows:
        inj = sessions_inj[r["task"]]
        items = defaultdict(list)
        add = 0
        for p, a, b, s, code in inj["inlined"]:
            ia, ib = enclosing_item(repo, p, a, b)
            items[p].append((ia, ib))
            add += max(0, span_tokens(repo, p, ia, ib) - span_tokens(repo, p, a, b))

        def pred(c):
            if c["tool"] != "Read" or not c.get("range"):
                return 0.0
            lo, hi = c["range"]
            rr = items.get(c["arg"], [])
            return sum(1 for ln in range(lo, hi + 1) if within(ln, rr, 0)) / max(1, hi - lo + 1)
        t, n, sec = removed(r, pred)
        remaining = max(1, r["api_calls"] - n)
        tot["reading_saved"] += t
        tot["turns_saved"] += n
        tot["secs_saved"] += sec
        tot["inj_added"] += add
        tot["usd_added"] += add * (P_CW + P_CR * (remaining - 1)) / 1e6
    out["enclosing_item_spans"] = dict(tot)
    # perfect retrieval of gold-miss reads (upper bound): every c_gold_miss line removed
    for name, keys in (("oracle_gold_miss", {"c_gold_miss"}), ("oracle_redundant", {"a_redundant"}),
                       ("oracle_expansion", {"d_expansion"}), ("oracle_map_hit", {"b_map_hit"})):
        tot = defaultdict(float)
        for r in rows:
            t, n, sec = removed(r, lambda c: frac(c, keys))
            tot["reading_saved"] += t
            tot["turns_saved"] += n
            tot["secs_saved"] += sec
        out[name] = dict(tot)
    # grep-locate removed (definitions + call sites listed in the injection)
    tot = defaultdict(float)
    for r in rows:
        t, n, sec = removed(r, lambda c: 1.0 if c["class"] == "e_grep_locate" else 0.0)
        tot["reading_saved"] += t
        tot["turns_saved"] += n
        tot["secs_saved"] += sec
    out["no_grep_locate"] = dict(tot)
    # stop at last gold discovery (verification tail cut, upper bound)
    tot = defaultdict(float)
    for r in rows:
        if r["tail_turns"] is None:
            continue
        tail = [t for t in range(r["last_gold_turn"] + 1, r["api_calls"])]  # keep the answer turn
        tot["reading_saved"] += sum(c["tokens"] for c in r["calls"] if c["turn"] in tail)
        tot["turns_saved"] += len(tail)
        tot["secs_saved"] += sum(r["turn_time"].get(t) or 0 for t in tail)
    out["cut_verification_tail"] = dict(tot)
    # inline the "Related by references" ranges as code
    tot = defaultdict(float)
    for r in rows:
        inj = sessions_inj[r["task"]]
        rel_ranges = defaultdict(list)
        for p, a, b, s in inj["related"]:
            rel_ranges[p].append((a, b))
        add = sum(span_tokens(repo, p, a, b) for p, a, b, s in inj["related"])

        def pred(c):
            if c["tool"] != "Read" or not c.get("range"):
                return 0.0
            lo, hi = c["range"]
            rr = rel_ranges.get(c["arg"], [])
            return sum(1 for ln in range(lo, hi + 1) if within(ln, rr, 0)) / max(1, hi - lo + 1)
        t, n, sec = removed(r, pred)
        remaining = max(1, r["api_calls"] - n)
        tot["reading_saved"] += t
        tot["turns_saved"] += n
        tot["secs_saved"] += sec
        tot["inj_added"] += add
        tot["usd_added"] += add * (P_CW + P_CR * (remaining - 1)) / 1e6
    out["inline_related"] = dict(tot)
    # narrow whole-file Reads of >250-line files to ~150 lines (outline + region, capped at
    # NARROW_TOK); assume half of them trigger one follow-up ranged Read (FOLLOWUP_TOK, +1 turn of
    # MEDIAN_TURN_S). Net numbers are reported (negative turns/seconds = added).
    NARROW_TOK, FOLLOWUP_TOK, MEDIAN_TURN_S = 1800, 1300, 3.5
    tot = defaultdict(float)
    for r in rows:
        for c in r["calls"]:
            if c["tool"] == "Read" and c.get("full") and c.get("range") and c["range"][1] - c["range"][0] >= 250:
                tot["reading_saved"] += max(0, c["tokens"] - NARROW_TOK) - 0.5 * FOLLOWUP_TOK
                tot["turns_saved"] -= 0.5
                tot["secs_saved"] -= 0.5 * MEDIAN_TURN_S
    out["narrow_full_reads"] = dict(tot)
    # pre-list definitions + usages (file:line: text) of identifiers named in the task text: removes
    # grep-locate calls whose pattern hits a task identifier; their result size is added to the
    # injection instead.
    tot = defaultdict(float)
    for r in rows:
        tid = set(r.get("task_idents") or [])
        pred = lambda c: 1.0 if c["class"] == "e_grep_locate" and set(c.get("hits") or []) & tid else 0.0
        t, n, sec = removed(r, pred)
        remaining = max(1, r["api_calls"] - n)
        tot["reading_saved"] += t
        tot["turns_saved"] += n
        tot["secs_saved"] += sec
        tot["inj_added"] += t
        tot["usd_added"] += t * (P_CW + P_CR * (remaining - 1)) / 1e6
    out["task_identifier_usages"] = dict(tot)
    return out


# ---------------------------------------------------------------- reporting

def load_run(run_dir, tasks, repo, ref_arm):
    raw = os.path.join(run_dir, "raw")
    by_arm = defaultdict(dict)
    files = sorted(os.listdir(raw))
    ref_inj = {}
    for name in files:
        tid, arm = name[:-6].split("_", 1)
        if arm == ref_arm and tid in tasks:
            s = parse_session(os.path.join(raw, name), repo)
            ref_inj[tid] = merge_injections([j for _, j in s["injections"]])
    for name in files:
        tid, arm = name[:-6].split("_", 1)
        if tid not in tasks:
            continue
        by_arm[arm][tid] = analyse(os.path.join(raw, name), tasks[tid], repo, ref_inj.get(tid))
    return by_arm, ref_inj


def fmt(x, d=0):
    return ("%%.%df" % d) % x if x is not None else "-"


def report(by_arm, ref_inj, repo, cf=False):
    arms = sorted(by_arm, key=lambda a: (a != "baseline", a))
    common = sorted(set.intersection(*[set(v) for v in by_arm.values()]))
    n = len(common)
    print("paired tasks: %d; arms: %s\n" % (n, ", ".join(arms)))
    print("### Reading tokens / calls / turns / seconds per class (mean per task)\n")
    print("| arm | class | tokens | % of reading | calls | turns | seconds |")
    print("|---|---|---|---|---|---|---|")
    for a in arms:
        rs = [by_arm[a][t] for t in common]
        total = sum(r["reading_tokens"] for r in rs) or 1
        for k in CLASSES + ["answer"]:
            tk = sum(r["cls_tok"].get(k, 0) for r in rs)
            ca = sum(r["cls_calls"].get(k, 0) for r in rs)
            tu = sum(r["cls_turns"].get(k, 0) for r in rs)
            se = sum(r["cls_time"].get(k, 0) for r in rs)
            if tk or ca or tu:
                print("| %s | %s | %s | %.1f%% | %.2f | %.2f | %.1f |" % (a, k, fmt(tk / n), 100 * tk / total, ca / n, tu / n, se / n))
        print("| %s | **total** | %s | 100%% | | %.2f api calls | %.1f wall |" % (
            a, fmt(total / n), sum(r["api_calls"] for r in rs) / n, sum(r["wall_s"] for r in rs) / n))
    print("\n### Gold coverage of the injection (gold files, all tasks)\n")
    print("| arm | inlined | map only | related only | absent | tasks with 0 gold injected | FILES answer fully inside injection | ...inside inlined files | answer recall |")
    print("|---|---|---|---|---|---|---|---|---|")
    for a in arms:
        if a == "baseline":
            continue
        rs = [by_arm[a][t] for t in common]
        c = defaultdict(int)
        for r in rs:
            for v in r["gold_cov"].values():
                c[v] += 1
        ng = sum(c.values())
        zero = sum(1 for r in rs if all(v == "absent" for v in r["gold_cov"].values()))
        full = sum(1 for r in rs if r["named"] and len(r["named_in_inj"]) == len(r["named"]))
        fulli = sum(1 for r in rs if r["named"] and len(r["named_in_inlined"]) == len(r["named"]))
        print("| %s | %d (%.0f%%) | %d | %d | %d (%.0f%%) | %d/%d | %d/%d | %d/%d | %.3f |" % (
            a, c["inlined"], 100 * c["inlined"] / ng, c["mapped"], c["related"], c["absent"], 100 * c["absent"] / ng,
            zero, n, full, n, fulli, n, sum(r["recall"] for r in rs) / n))
    print("\n### Discovery timing (API-call index; 0 = gold already inlined)\n")
    print("| arm | first gold Read | last new gold file | api calls | tail calls after last gold | tail seconds | tail reading tok | tail share of wall |")
    print("|---|---|---|---|---|---|---|---|")
    for a in arms:
        rs = [by_arm[a][t] for t in common]
        f = [r["first_gold_read_turn"] for r in rs if r["first_gold_read_turn"]]
        lg = [r for r in rs if r["last_gold_turn"] is not None]
        print("| %s | %.1f (%d/%d) | %.1f | %.1f | %.1f | %.1f | %s | %.0f%% |" % (
            a, sum(f) / max(1, len(f)), len(f), n, sum(r["last_gold_turn"] for r in lg) / max(1, len(lg)),
            sum(r["api_calls"] for r in lg) / max(1, len(lg)), sum(r["tail_turns"] for r in lg) / max(1, len(lg)),
            sum(r["tail_time"] for r in lg) / max(1, len(lg)), fmt(sum(r["tail_tokens"] for r in lg) / max(1, len(lg))),
            100 * sum(r["tail_time"] for r in lg) / max(1e-9, sum(r["wall_s"] for r in lg))))
    if cf:
        ref = [a for a in arms if a != "baseline"]
        for a in ref:
            rs = [by_arm[a][t] for t in common]
            res = counterfactual(rs, repo, {t: by_arm[a][t]["_inj"] for t in common})
            base = sum(r["reading_tokens"] for r in rs) / n
            print("\n### Counterfactuals for %s (estimates; mean per task; reading tokens %.0f, api calls %.1f, wall %.1fs)\n" % (
                a, base, sum(r["api_calls"] for r in rs) / n, sum(r["wall_s"] for r in rs) / n))
            print("| option | reading tok saved | api calls saved | seconds saved | injected tok added | $ added (cache w+r) |")
            print("|---|---|---|---|---|---|")
            for k, v in res.items():
                print("| %s | %s | %.2f | %.1f | %s | %s |" % (k, fmt(v.get("reading_saved", 0) / n), v.get("turns_saved", 0) / n,
                                                         v.get("secs_saved", 0) / n, fmt(v.get("inj_added", 0) / n),
                                                         fmt(v.get("usd_added", 0) / n, 4)))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("run_dir")
    ap.add_argument("--tasks", default=os.path.join(HERE, "tasks.jsonl"))
    ap.add_argument("--repo", default=DEFAULT_REPO)
    ap.add_argument("--ref-arm", default=None, help="laya arm whose injection baseline calls are classified against")
    ap.add_argument("--json", default=None)
    ap.add_argument("--counterfactual", action="store_true")
    args = ap.parse_args()
    tasks = {json.loads(l)["id"]: json.loads(l) for l in open(args.tasks)}
    arms = {n[:-6].split("_", 1)[1] for n in os.listdir(os.path.join(args.run_dir, "raw"))}
    ref = args.ref_arm or next((a for a in ("laya-refs", "laya-compact", "laya") if a in arms), None)
    by_arm, ref_inj = load_run(args.run_dir, tasks, args.repo, ref)
    report(by_arm, ref_inj, args.repo, args.counterfactual)
    if args.json:
        slim = {a: {t: {k: v for k, v in r.items() if k != "_inj"} for t, r in rs.items()} for a, rs in by_arm.items()}
        json.dump(slim, open(args.json, "w"), indent=1, default=str)


if __name__ == "__main__":
    sys.exit(main())
