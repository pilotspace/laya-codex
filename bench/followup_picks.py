"""Could picked lists and snippets on the follow-up have spared Claude its follow-up lookups? (offline replay)

    python3 bench/followup_picks.py --run v13=<run root> --run v14=<run root> \
        --repos <dir with the benchmark checkouts> [--out bench/results/followup-picks-v13-v14.json]

A run root holds <repo>/{runs.jsonl,raw}. Raw transcripts are not committed; the checkouts must be
at the benchmark's commits. CPU only, read-only.

The follow-up hook could inject, for names Claude met on prompt 1 (the names in backticks in its
first answer plus the definitions prompt 1 inlined: the candidate set "A"), the lines that use each
name (`file:lines`, the `search` name-lookup semantics of crates/laya-cli/src/mcp.rs), and short
code snippets. Each real follow-up tool use is checked against what such an injection could hold:
- `search` / Grep for names: needs, for every name it asks about, that name's list in the
  lookup's scope (path, glob, type); a lookup that finds nothing in scope needs the whole list;
- Read: needs its line range as a snippet, and only if the range holds a use of a candidate name;
- Glob, a `search` in words, and a Grep that only lists structure (`#[test]`, `def test_`,
  `describe(`) are never covered.
A model call is removable if every tool use it made is covered (followup_completeness.py's rule).

Pickers, against a character budget per follow-up:
- the oracle: the subset of coverable calls that removes the most calls within the budget (an
  upper bound for any picker, the Laya model included);
- a perfect name picker: the best <= k candidate names, whole lists, no snippets;
- rules with no model: all names, the first 3 or 5 answer names, the answer's focus names (defined
  in a file of the answer's FILES line), focus + test uses of the others, smallest lists first.

Cost: a removed lookup call saves its read of the cached prefix, its uncached input, its output, and
the tokens it adds to the conversation (its tool_use and tool_result), which the next call writes to
the cache and every later call reads again; each token is counted once, at runs.PRICES_PER_M. The
injection costs 0.455 tokens per char (billed follow-up cache writes regressed on injected chars,
v13 + v14, R^2 0.99), written once and read by every later call of the turn.

Parsing fixes over followup_completeness.lookup_names (tested in test_followup_picks.py):
`\\bparse` is the name `parse`, not `bparse`; `content-type` and `middleware/etag` stay whole; a
Grep for test or definition structure only (`fn test_`) asks for no name.
"""
import argparse
import fnmatch
import itertools
import json
import os
import random
import re
import statistics as st
import subprocess
from collections import Counter, defaultdict

import followup_completeness as fc
import grep_intents as gi
from runs import PRICES_PER_M

TOKENS_PER_CHAR = 0.455
PLAN_SAVING = 0.005          # the plan's G2 cost-neutral rule: injection cost < calls removed x $0.005
BUDGETS = (1000, 2000, 4000, 8000, 10 ** 9)
FMT = {"compact": 0, "text": 1}
PRICE = {k: v / 1e6 for k, v in PRICES_PER_M.items()}


def bname(b):
    return "inf" if b >= 10 ** 9 else f"{b // 1000}k"


# ---------------------------------------------------------------- `search` name lookup (mcp.rs port)
FRAMING = set("""a an the of for to in on at by with from and or is are where what which who how does do find show
list all every its use uses usage usages used caller callers call calls called reference references refs test tests
covering cover covers definition definitions defined define defines declaration implementation implemented def fn
class function struct enum trait impl interface type const let var pub async export static mod""".split())


def is_ident_char(c):
    return c.isascii() and (c.isalnum() or c in "_$")


def is_identifier(s):
    return bool(s) and s[0].isascii() and (s[0].isalpha() or s[0] in "_$") and all(is_ident_char(c) for c in s)


def is_literal(s):
    """A header or flag (`content-type`) or a module path (`middleware/etag`): matched as plain text."""
    return (len(s) >= 3 and ("-" in s or "/" in s) and any(c.isascii() and c.isalpha() for c in s)
            and all(is_ident_char(c) or c in "-/.@" for c in s))


def code_shaped(s):
    return any(c in "_$-/" or c.isdigit() or (c.isascii() and c.isupper()) for c in s)


def starts_a_part(before, first):
    if before is None or not is_ident_char(before):
        return True
    return before == "_" or first == "_" or (first.isupper() and before.isalnum())


def ends_a_part(last, after):
    if after is None or not is_ident_char(after):
        return True
    return after == "_" or last == "_" or (after.isupper() and not last.isupper())


def has_name(line, name):
    """`name` in `line` at a word, `_` or camelCase boundary on both sides; literals as plain text."""
    if not is_identifier(name):
        return name in line
    at = line.find(name)
    while at >= 0:
        before = line[at - 1] if at > 0 else None
        end = at + len(name)
        if starts_a_part(before, name[0]) and ends_a_part(name[-1], line[end] if end < len(line) else None):
            return True
        at = line.find(name, at + 1)
    return False


def clean_token(raw):
    t = raw.replace("\\b", "").replace("\\", "").strip("`\"'^()?!;:.*=")
    if len(t) > 1 and t.endswith("$"):
        t = t[:-1]
    if is_literal(t):
        return t
    return re.split(r"[.:]", t)[-1] or None


def identifier_query(q):
    """The names a `search` query looks up, or None when it describes code in words."""
    listed = "|" in q
    names = []
    for raw in re.split(r"[\s|,]", q):
        tok = clean_token(raw)
        if not tok or (not listed and tok.lower() in FRAMING):
            continue
        if len(tok) > 128 or (not is_identifier(tok) and not is_literal(tok)):
            return None
        if tok not in names:
            names.append(tok)
    if not names or len(names) > 8:
        return None
    return names if len(names) == 1 or listed or all(code_shaped(n) for n in names) else None


STRUCTURAL = {"test", "tests", "test_", "tokio", "cfg", "describe", "it", "fn", "def", "async", "await", "mod", "impl",
              "pub", "class", "function", "export", "const", "let", "self", "expect", "assert"}
TOKEN = re.compile(r"[A-Za-z_$][\w$]*(?:[-/][A-Za-z_$][\w$]*)*")


def grep_names(pattern):
    """(names, structural): the names a Grep pattern looks up, and whether it (also) lists test or
    definition structure. Regex escapes (`\\b`, `\\s`, `\\(`), classes and groups are dropped first,
    `a-b` / `a/b` literals stay whole, and structure words are not names."""
    p = re.sub(r"\\[bBwWdDsSAzZ]", " ", pattern)
    p = re.sub(r"(?<!\\)\[(?:\\.|[^\]\\])*\]", " ", p)  # character classes, not escaped brackets
    p = re.sub(r"\\(.)", r" \1 ", p).replace(" . ", " ").replace(" ( ", " ").replace(" ) ", " ")
    p = re.sub(r"\(\?[:=!<]+", " ", p)
    names, structural = [], False
    for t in TOKEN.findall(p):
        if t.lower() in STRUCTURAL or (t.startswith("test_") and len(t) <= 6):
            structural = True
            continue
        if not is_literal(t) and (len(t) < 3 or t in fc.KEYWORDS):
            continue
        if t not in names:
            names.append(t)
    return names, structural


# ---------------------------------------------------------------- the checkout
class Repo:
    """A benchmark checkout: every file (rg's view, hidden files included) and each name's uses."""

    def __init__(self, path, name):
        self.name, self.dir = name, path
        self._lines, self._uses = {}, {}

    def lines(self, f):
        if f not in self._lines:
            try:
                with open(os.path.join(self.dir, f), encoding="utf-8", errors="replace") as h:
                    self._lines[f] = h.read().split("\n")
            except OSError:
                self._lines[f] = None
        return self._lines[f]

    def uses(self, name):
        """{file: [line numbers]} where `name` occurs with has_name semantics."""
        if name not in self._uses:
            out = subprocess.run(["rg", "-n", "--no-heading", "--hidden", "-g", "!.git", "-F", "-s", name, "."],
                                 cwd=self.dir, capture_output=True, text=True, timeout=120).stdout
            found = defaultdict(list)
            for line in out.splitlines():
                parts = line.split(":", 2)
                if len(parts) == 3 and parts[1].isdigit() and has_name(parts[2], name):
                    found[parts[0].removeprefix("./")].append(int(parts[1]))
            self._uses[name] = {f: sorted(v) for f, v in sorted(found.items())}
        return self._uses[name]

    def def_files(self, name):
        """Files with a line that defines `name`."""
        if not is_identifier(name):
            return []
        rx = re.compile(r"^\s*(?:(?:pub(?:\([a-z]+\))?|export|default|async|static|public|private|protected|unsafe|"
                        r"readonly)\s+)*(?:(?:fn|def|class|struct|enum|trait|interface|type|function|const|let|var|mod|"
                        r"union)\s+" + re.escape(name) + r"\b|#?" + re.escape(name) + r"\s*(?:<[^>]*>)?\s*[(=:])")
        out = []
        for f, lns in self.uses(name).items():
            text = self.lines(f) or []
            if any(0 < ln <= len(text) and rx.search(text[ln - 1]) for ln in lns):
                out.append(f)
        return out


PREFIXES = ("/Users/Shared/laya-bench-repos/", "/Users/tindang/bench-repos/")


def relp(p, repo, repo_dir=None):
    """A tool's path relative to the checkout (the benchmark ran in one of PREFIXES)."""
    if not p:
        return ""
    if repo_dir and p.startswith(repo_dir.rstrip("/") + "/"):
        p = p[len(repo_dir.rstrip("/")):]
    for pre in PREFIXES:
        if p.startswith(pre + repo):
            p = p[len(pre + repo):]
            break
    return p.lstrip("/").removeprefix("./").rstrip("/")


def expand_braces(g):
    m = re.search(r"\{([^{}]*)\}", g)
    if not m:
        return [g]
    return [x for alt in m.group(1).split(",") for x in expand_braces(g[:m.start()] + alt + g[m.end():])]


def glob_ok(path, scope_dir, globs):
    """rg --glob, approximately: a negated glob excludes; positive globs (if any) must match one."""
    pos, neg = [], []
    for g in globs:
        for e in expand_braces(g):
            (neg if e.startswith("!") else pos).append(e.lstrip("!"))
    rel_scope = path[len(scope_dir) + 1:] if scope_dir and path.startswith(scope_dir + "/") else path

    def match(g):
        g2 = g.replace("**/", "*").replace("/**", "/*")
        if "/" not in g:
            return fnmatch.fnmatchcase(os.path.basename(path), g2)
        return any(fnmatch.fnmatchcase(p, g2) for p in (path, rel_scope)) or fnmatch.fnmatchcase(path, "*/" + g2)
    return not any(match(g) for g in neg) and (not pos or any(match(g) for g in pos))


TYPE_EXT = {"rust": (".rs",), "py": (".py", ".pyi"), "ts": (".ts", ".tsx", ".mts", ".cts"),
            "js": (".js", ".jsx", ".mjs", ".cjs")}


def in_scope(path, scopes, globs, ftype):
    if not any(s == "" or path == s or path.startswith(s + "/") for s in scopes):
        return False
    if ftype and not path.endswith(TYPE_EXT.get(ftype, ("." + ftype,))):
        return False
    return glob_ok(path, scopes[0] if len(scopes) == 1 else "", globs)


# ---------------------------------------------------------------- what each follow-up use needs
def compact_chars(name, f, lns):
    """`name`'s list for one file as `file:l1,l2; ` (at most 40 lines), with the name's header."""
    return len(f"{f}:{','.join(map(str, lns[:40]))}" + (f"(+{len(lns) - 40})" if len(lns) > 40 else "") + "; ") \
        + len(name) + 4


def text_chars(repo, f, lns):
    """The same list with each line's text (`  12: code`, 100 chars a line)."""
    text = repo.lines(f) or []
    n = len(f) + 1 + (20 if len(lns) > 40 else 0)
    for ln in lns[:40]:
        n += len(f" {ln}: {(text[ln - 1].strip() if 0 < ln <= len(text) else '')[:100]}\n")
    return n


def requirement(tool, repo, cands):
    """What injection would cover one tool use: `items` {(name, file): (compact, text) chars} for a
    name lookup, `snippet` (file, a, b, chars) for a Read, or `miss` with the reason none can."""
    name, inp = tool["name"], tool["input"]
    r = {"tool": name, "items": {}, "snippet": None, "miss": None, "names": [], "kind": None}
    if name == "Read":
        f = relp(inp.get("file_path", ""), repo.name, repo.dir)
        r["kind"] = "read"
        text = repo.lines(f)
        if text is None:
            r["miss"] = "stale: file missing"
            return r
        a = inp.get("offset") or 1
        b = min(a + (inp.get("limit") or 2000) - 1, len(text))
        a = max(1, min(a, len(text)))
        r["snippet"] = (f, a, b, sum(len(x) + 1 for x in text[a - 1:b]) + len(f) + 20)
        r["anchored"] = [n for n in cands if any(a <= ln <= b for ln in repo.uses(n).get(f, []))]
        r["in_file"] = [n for n in cands if f in repo.uses(n)]
        return r
    if name == "Glob":
        r["kind"], r["miss"] = "glob", "no identifier (Glob)"
        return r
    if name.startswith("mcp__laya-codex__search"):
        r["kind"] = "search"
        names = identifier_query(inp.get("query", ""))
        if names is None:
            r["miss"] = "no identifier (search in words)"
            return r
        scopes = [relp(s.strip(), repo.name, repo.dir) for s in (inp.get("path") or "").split("|")]
        globs, ftype = [], None
    elif name == "Grep":
        r["kind"] = "grep"
        names, structural = grep_names(inp.get("pattern", ""))
        if not names:
            r["miss"] = "structural grep (list tests/defs in a file)" if structural else "no identifier (grep regex)"
            return r
        scopes = [relp(inp.get("path", ""), repo.name, repo.dir)]
        globs, ftype = ([inp["glob"]] if inp.get("glob") else []), inp.get("type")
    else:
        r["kind"], r["miss"] = "other", "other tool"
        return r
    r["names"] = names
    r["n_uses"] = {n: sum(len(v) for v in repo.uses(n).values()) for n in names}
    for n in names:
        uses = repo.uses(n)
        if not uses:
            r["miss"] = r["miss"] or "stale/absent: name not in checkout"
            continue
        files = {f: lns for f, lns in uses.items() if in_scope(f, scopes, globs, ftype)} or uses
        for f, lns in files.items():
            r["items"][(n, f)] = (compact_chars(n, f, lns), text_chars(repo, f, lns))
    return r


def injected_use_names(text):
    """Names in an injection's 'Definitions and uses' / 'Related by references' lines."""
    out = []
    for m in re.finditer(r"(?:use of|defines|calls|test using|used by) `([^`]+)`", text):
        for n in fc.IDENT.findall(m.group(1)):
            if n not in out and n not in fc.KEYWORDS:
                out.append(n)
    return out


def call_savings(p):
    """Per lookup call of a prompt (every API call but the last), the dollars removing it saves.
    Lookup calls share the prompt's non-final output tokens in proportion to their streamed counts."""
    api = p["api"]
    n = len(api)
    total_out = p["result_usage"].get("output_tokens") or 0
    iters = p["result_usage"].get("iterations") or []
    lookup_out = total_out - ((iters[-1].get("output_tokens") or 0) if iters else 0)
    partial = sum(c["usage_last"].get("output_tokens") or 0 for c in api[:-1]) or 1
    out = []
    for i, c in enumerate(api[:-1]):
        u, nxt = c["usage"], api[i + 1]["usage"]
        out_tok = lookup_out if n == 2 else lookup_out * (c["usage_last"].get("output_tokens") or 0) / partial
        added = (nxt.get("cache_creation_input_tokens") or 0) + (nxt.get("input_tokens") or 0)
        later = n - (i + 2)
        out.append({"saving": PRICE["cache_read_input_tokens"] * (u.get("cache_read_input_tokens") or 0)
                    + PRICE["input_tokens"] * (u.get("input_tokens") or 0)
                    + PRICE["output_tokens"] * out_tok
                    + PRICE["cache_creation_input_tokens"] * added + PRICE["cache_read_input_tokens"] * added * later,
                    "output": PRICE["output_tokens"] * out_tok})
    return out


def answer_files(answer):
    m = re.search(r"^FILES:\s*(.*)$", answer, re.M)
    return [x.strip() for x in m.group(1).split(",")] if m else []


def session_record(tag, repo_name, task, arm, p1, p2, repo):
    """One session's follow-up: candidate names, what each tool use needs, per-call savings."""
    ans = fc.answer_names(p1)
    defs = fc.inlined_definitions(p1) if p1["inj_text"] else []
    ranked1 = fc.ranked_symbols(p1) + injected_use_names(p1["inj_text"]) if p1["inj_text"] else []
    ranked2 = fc.ranked_symbols(p2) + injected_use_names(p2["inj_text"]) if p2["inj_text"] else []
    a = [c for c in dict.fromkeys(ans + defs) if repo.uses(c)]  # no use in the checkout: nothing to list
    b = [c for c in dict.fromkeys(a + ranked1) if repo.uses(c)]
    c_ = [c for c in dict.fromkeys(b + ranked2) if repo.uses(c)]
    reqs = []
    for ci, ids in enumerate(p2["calls"]):
        for tid in ids:
            r = requirement(p2["tools"][tid], repo, c_)
            r.update(id=tid, call=ci, input=p2["tools"][tid]["input"])
            reqs.append(r)
    afiles = answer_files(p1["answer"])
    cinfo = {}
    for c in c_:
        uses = repo.uses(c)
        dfs = repo.def_files(c)
        cinfo[c] = {"files": {f: (compact_chars(c, f, lns), text_chars(repo, f, lns), gi.is_test_path(f))
                              for f, lns in uses.items()},
                    "n_lines": sum(len(v) for v in uses.values()), "in_answer": c in ans,
                    "focus": c in ans and any(f == x or f.endswith(x) or x.endswith(f) for x in afiles for f in dfs)}
    sav = call_savings(p2)
    return {"tag": tag, "repo": repo_name, "task": task, "arm": arm, "cands": a, "cand_sets": {"A": a, "B": b, "C": c_},
            "n_ans": len(ans), "n_defs": len(defs), "cinfo": cinfo, "reqs": reqs, "n_calls_p2": len(p2["calls"]),
            "savings": [x["saving"] for x in sav], "outputs": [x["output"] for x in sav]}


# ---------------------------------------------------------------- pickers
def use_items(r, fmt, cset):
    """{item: chars} one use needs from an injection drawn from `cset`, or None if none can cover it."""
    if r["miss"]:
        return None
    if r["kind"] == "read":
        if not set(r["anchored"]) & cset:
            return None
        f, a, b, chars = r["snippet"]
        return {("SNIP", f, a, b): chars}
    if not set(r["names"]) <= cset:
        return None
    return {k: v[FMT[fmt]] for k, v in r["items"].items()}


def any_name_items(r, fmt, cset):
    """The same with no candidate limit (any name Claude asked for): the ceiling of the list design."""
    if r["kind"] == "read":
        if r["miss"]:
            return None
        f, a, b, chars = r["snippet"]
        return {("SNIP", f, a, b): chars}
    if r["miss"]:
        return None
    return {k: v[FMT[fmt]] for k, v in r["items"].items()}


def units_of(s, unit):
    if unit == "use":
        return [[r] for r in s["reqs"]]
    by = defaultdict(list)
    for r in s["reqs"]:
        by[r["call"]].append(r)
    return list(by.values())


def oracle(s, budget, unit, fmt="compact", snippets=True, itemfn=use_items, max_snips=None, cset="A"):
    """The most units (uses or calls) one injection of at most `budget` chars covers, exhaustively:
    (covered, chars, chosen items)."""
    names = set(s["cand_sets"][cset])
    cand = []
    for us in units_of(s, unit):
        need = {}
        for r in us:
            it = itemfn(r, fmt, names)
            if it is None or (not snippets and r["kind"] == "read"):
                break
            need.update(it)
        else:
            cand.append(need)
    if len(cand) > 16:
        raise SystemExit(f"{s['task']}: {len(cand)} coverable units, too many for the exhaustive oracle")
    best = (0, 0, {})
    for m in range(1, 1 << len(cand)):
        chosen = {}
        for i, need in enumerate(cand):
            if m >> i & 1:
                chosen.update(need)
        if max_snips is not None and sum(1 for k in chosen if k[0] == "SNIP") > max_snips:
            continue
        cnt, cost = bin(m).count("1"), sum(chosen.values())
        if cost <= budget and (cnt > best[0] or (cnt == best[0] and cost < best[1])):
            best = (cnt, cost, chosen)
    return best


def covered_by(s, chosen, fmt="compact"):
    """(uses, calls) an injection of `chosen` items covers."""
    names = set(s["cand_sets"]["C"])
    by = defaultdict(list)
    n_uses = 0
    for r in s["reqs"]:
        it = use_items(r, fmt, names)
        ok = it is not None and all(k in chosen for k in it)
        n_uses += ok
        by[r["call"]].append(ok)
    return n_uses, sum(all(v) for v in by.values())


def names_only_k(s, k, budget):
    """Calls covered by the best <= k candidate names, whole lists, no snippets: a perfect name picker."""
    needed = [c for c in s["cand_sets"]["A"] if any(c in r["names"] for r in s["reqs"])]
    best = 0
    for kk in range(1, k + 1):
        for combo in itertools.combinations(needed, kk):
            chosen = {(c, f): v[0] for c in combo for f, v in s["cinfo"][c]["files"].items()}
            if sum(chosen.values()) <= budget:
                best = max(best, covered_by(s, chosen)[1])
    return best


RULES = ("all", "first3", "first5", "focus", "focus+tests", "smallest")


def heuristic_items(s, rule, budget):
    """A rule's names in order, each name's whole list if it still fits: (chosen items, chars)."""
    ci = s["cinfo"]
    ans = [c for c in s["cands"] if ci[c]["in_answer"]]
    order = {"all": list(s["cands"]), "first3": ans[:3], "first5": ans[:5],
             "focus": [c for c in ans if ci[c]["focus"]], "focus+tests": [c for c in ans if ci[c]["focus"]],
             "smallest": sorted(ans, key=lambda c: ci[c]["n_lines"])}[rule]
    chosen, used = {}, 0

    def add(items):
        nonlocal used
        cost = sum(items.values())
        if items and used + cost <= budget:
            chosen.update(items)
            used += cost
    for c in order:
        add({(c, f): v[0] for f, v in ci[c]["files"].items()})
    if rule == "focus+tests":  # then the test-file uses of every other answer name
        for c in ans:
            if not ci[c]["focus"]:
                add({(c, f): v[0] for f, v in ci[c]["files"].items() if v[2]})
    return chosen, used


# ---------------------------------------------------------------- cost
def injection_price(chars, later_reads):
    """Dollars for `chars` injected: one cache write, then one cache read per later call of the turn."""
    return chars * TOKENS_PER_CHAR * (PRICE["cache_creation_input_tokens"] + PRICE["cache_read_input_tokens"]
                                      * max(0, later_reads))


def break_even_chars(saving, later_reads=1):
    """Injected chars per removed call at which the injection costs what the call saved."""
    return saving / (TOKENS_PER_CHAR * (PRICE["cache_creation_input_tokens"]
                                        + PRICE["cache_read_input_tokens"] * later_reads))


# ---------------------------------------------------------------- report
def bootstrap(ss, fn, n=2000, seed=1):
    """Pooled covered/total calls and a 95% CI, resampling sessions."""
    rnd = random.Random(seed)
    vals = [(fn(s), len({r["call"] for r in s["reqs"]})) for s in ss]
    out = []
    for _ in range(n):
        smp = [vals[rnd.randrange(len(vals))] for _ in vals]
        out.append(sum(a for a, _ in smp) / max(1, sum(b for _, b in smp)))
    out.sort()
    return [round(sum(a for a, _ in vals) / sum(b for _, b in vals), 4), round(out[int(0.025 * n)], 4),
            round(out[int(0.975 * n)], 4)]


def calls_of(ss):
    return sum(len({r["call"] for r in s["reqs"]}) for s in ss)


def taxonomy(ss, budget=8000):
    """Why each follow-up tool use is or isn't covered by the oracle at `budget`."""
    tax = Counter()
    for s in ss:
        _, _, chosen = oracle(s, budget, "use")
        ca, cc = set(s["cand_sets"]["A"]), set(s["cand_sets"]["C"])
        for r in s["reqs"]:
            it = use_items(r, "compact", ca)
            if it is not None and all(k in chosen for k in it):
                tax["covered"] += 1
            elif r["miss"]:
                tax[r["miss"]] += 1
            elif r["kind"] == "read":
                where = " (test file)" if gi.is_test_path(r["snippet"][0]) else " (source file)"
                if set(r["anchored"]) & ca:
                    tax["Read: snippet over budget" + where] += 1
                elif set(r["in_file"]) & ca:
                    tax["Read: range holds no use of a candidate name" + where] += 1
                else:
                    tax["Read: file holds no candidate name" + where] += 1
            elif not set(r["names"]) <= ca:
                missing = [n for n in r["names"] if n not in ca]
                if set(r["names"]) <= cc:
                    tax["name not in the answer, but in prompt-1 or follow-up injection symbols"] += 1
                elif all(r["n_uses"][n] > 100 for n in missing):
                    tax["name not in the answer: a generic word (> 100 uses)"] += 1
                else:
                    tax["name not in the answer or any injection (Claude thought of it on the follow-up)"] += 1
            elif sum(it.values()) > budget:
                tax[f"too many uses (list > {budget} chars alone)"] += 1
            else:
                tax["coverable, lost to budget contention"] += 1
    return dict(tax.most_common())


def report(sessions, heldout):
    groups = [(t, a) for a in ("laya", "gate", "baseline") for t in sorted({s["tag"] for s in sessions})
              if any(s["tag"] == t and s["arm"] == a for s in sessions)]
    grp = {f"{t} {a}": [s for s in sessions if s["tag"] == t and s["arm"] == a] for t, a in groups}
    pooled = [s for s in sessions if s["arm"] == "laya"]
    held = grp[f"{heldout} laya"]
    out = {"groups": {}, "oracle": {}}
    for g, ss in grp.items():
        out["groups"][g] = {"sessions": len(ss), "followups_with_lookups": sum(1 for s in ss if s["reqs"]),
                            "lookup_calls": calls_of(ss), "tool_uses": sum(len(s["reqs"]) for s in ss),
                            "candidates_mean": round(st.mean(len(s["cands"]) for s in ss), 1)}
        out["oracle"][g] = {fmt: {unit: {bname(b): sum(oracle(s, b, unit, fmt)[0] for s in ss) for b in BUDGETS}
                                  for unit in ("call", "use")} for fmt in FMT}
    variants = (("lists only (no snippets)", dict(snippets=False)),
                ("lists + at most 1 snippet (the plan's shape)", dict(max_snips=1)),
                ("lists + snippets [primary]", {}),
                ("B: + prompt-1 ranked symbols", dict(cset="B")),
                ("C: + the follow-up injection's symbols", dict(cset="C")),
                ("any name Claude asked for (no candidate limit)", dict(itemfn=any_name_items)))
    out["oracle_variants"] = {}
    for label, ss in (("pooled", pooled), ("held-out", held)):
        out["oracle_variants"][label] = {"calls": calls_of(ss), **{
            v: {bname(b): sum(oracle(s, b, "call", **kw)[0] for s in ss) for b in BUDGETS} for v, kw in variants}}
    gate = grp.get(f"{heldout} gate", [])
    out["oracle_ci"] = {label: {bname(b): bootstrap(ss, lambda s, b=b: oracle(s, b, "call")[0]) for b in BUDGETS[1:]}
                        for label, ss in (("pooled", pooled), ("held-out", held), ("held-out laya+gate", held + gate))}
    out["every_lookup_covered"] = {g: {"followups_with_lookups": sum(1 for s in ss if s["reqs"]), **{
        bname(b): sum(1 for s in ss if s["reqs"] and oracle(s, b, "call")[0] == len({r["call"] for r in s["reqs"]}))
        for b in (4000, 8000)}} for g, ss in grp.items() if not g.endswith("baseline")}
    out["name_picker"] = {label: {"calls": calls_of(ss), **{f"k{k}@{bname(b)}": sum(names_only_k(s, k, b) for s in ss)
                                                            for k in (1, 2, 3) for b in (2000, 4000, 8000)}}
                          for label, ss in (("pooled", pooled), ("held-out", held))}
    out["heuristics"] = {}
    for label, ss in (("pooled", pooled), ("held-out", held)):
        out["heuristics"][label] = {"calls": calls_of(ss), **{rule: {bname(b): {
            "removable": sum(covered_by(s, heuristic_items(s, rule, b)[0])[1] for s in ss),
            "chars_mean": round(st.mean(heuristic_items(s, rule, b)[1] for s in ss))} for b in BUDGETS[:-1]}
            for rule in RULES}}

    # cost
    out["saving_per_removed_call"] = {}
    for g, ss in list(grp.items()) + [("pooled", pooled)]:
        sv = [x for s in ss for x in s["savings"]]
        sv_out = [x - o for s in ss for x, o in zip(s["savings"], s["outputs"])]
        out["saving_per_removed_call"][g] = {"calls": len(sv), "mean": round(st.mean(sv), 5),
                                             "median": round(st.median(sv), 5),
                                             "mean_without_its_output": round(st.mean(sv_out), 5)}
    measured = out["saving_per_removed_call"]["pooled"]["mean"]
    out["break_even_chars_per_removed_call"] = {"measured saving": round(break_even_chars(measured)),
                                                "plan $0.005": round(break_even_chars(PLAN_SAVING))}
    n_calls = calls_of(pooled)

    def costrow(picks):
        """picks: per pooled session (chars injected, calls removed)."""
        inj = [injection_price(ch, s["n_calls_p2"] - rem) for s, (ch, rem) in zip(pooled, picks)]
        rem = sum(p[1] for p in picks) / len(pooled)
        return {"chars_mean": round(st.mean(p[0] for p in picks)), "calls_removed_per_followup": round(rem, 3),
                "share_of_calls": round(rem * len(pooled) / n_calls, 3), "injection_usd": round(st.mean(inj), 5),
                "net_usd_measured_saving": round(st.mean(inj) - rem * measured, 5),
                "net_usd_plan_saving": round(st.mean(inj) - rem * PLAN_SAVING, 5)}
    out["cost_per_followup"] = {}
    for b in BUDGETS:
        out["cost_per_followup"][f"oracle {bname(b)}"] = costrow(
            [(lambda o: (o[1], o[0]))(oracle(s, b, "call")) for s in pooled])
        out["cost_per_followup"][f"oracle, lists + at most 1 snippet, {bname(b)}"] = costrow(
            [(lambda o: (o[1], o[0]))(oracle(s, b, "call", max_snips=1)) for s in pooled])
    for rule in RULES:
        for b in (2000, 4000, 8000):
            out["cost_per_followup"][f"{rule} {bname(b)}"] = costrow(
                [(lambda ch: (ch[1], covered_by(s, ch[0])[1]))(heuristic_items(s, rule, b)) for s in pooled])
    out["miss_taxonomy"] = {"uses": sum(len(s["reqs"]) for s in pooled), "budget": 8000, "counts": taxonomy(pooled)}
    return out


def load(runs, repos_dir):
    sessions, repos = [], {}
    for tag, root in runs:
        for repo_name in sorted(os.listdir(root)):
            path = os.path.join(root, repo_name, "runs.jsonl")
            if not os.path.isfile(path):
                continue
            repo = repos.setdefault(repo_name, Repo(os.path.join(repos_dir, repo_name), repo_name))
            with open(path) as f:
                rows = [json.loads(x) for x in f if x.strip()]
            for row in rows:
                with open(os.path.join(root, repo_name, "raw", f"{row['task_id']}_{row['arm']}.jsonl")) as g:
                    ps = fc.parse_session([json.loads(x) for x in g if x.strip()])
                if len(ps) >= 2:
                    sessions.append(session_record(tag, repo_name, row["task_id"], row["arm"], ps[0], ps[1], repo))
    return sessions


def print_summary(out):
    for g, d in out["groups"].items():
        o = out["oracle"][g]["compact"]["call"]
        print(f"{g:13s} lookup calls {d['lookup_calls']:3d} | oracle calls removable " +
              "  ".join(f"{b} {o[b] / d['lookup_calls']:4.0%}" for b in o))
    for label, d in out["oracle_ci"].items():
        print(f"oracle CI {label:20s} " + "  ".join(f"{b} {v[0]:.1%} [{v[1]:.1%}, {v[2]:.1%}]" for b, v in d.items()))
    for label, d in out["oracle_variants"].items():
        for v, row in d.items():
            if v != "calls":
                print(f"{label:9s} {v:48s} " + "  ".join(f"{b} {c / d['calls']:4.0%}" for b, c in row.items()))
    for label, d in out["name_picker"].items():
        print(f"name picker {label:9s} "
              + "  ".join(f"{k} {v / d['calls']:4.0%}" for k, v in d.items() if k != "calls"))
    print("saving per removed call", out["saving_per_removed_call"]["pooled"],
          "break-even chars", out["break_even_chars_per_removed_call"])
    for k, v in out["miss_taxonomy"]["counts"].items():
        print(f"  {v:4d} {v / out['miss_taxonomy']['uses']:6.1%}  {k}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--run", action="append", required=True, help="TAG=ROOT, ROOT holding <repo>/{runs.jsonl,raw}")
    ap.add_argument("--repos", required=True, help="dir with the benchmark checkouts")
    ap.add_argument("--heldout", default="v14", help="the run tag on held-out tasks")
    ap.add_argument("--out")
    a = ap.parse_args()
    runs = [tuple(x.split("=", 1)) for x in a.run]
    out = {"runs": [t for t, _ in runs], "heldout": a.heldout, "tokens_per_char": TOKENS_PER_CHAR,
           "prices_per_m": PRICES_PER_M, "plan_saving_per_call": PLAN_SAVING,
           **report(load(runs, a.repos), a.heldout)}
    print_summary(out)
    if a.out:
        with open(a.out, "w") as f:
            json.dump(out, f, indent=1)
            f.write("\n")


if __name__ == "__main__":
    main()
