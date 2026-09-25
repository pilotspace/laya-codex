"""Why do agents Grep when laya-codex is there? Extract every Grep call from Claude Code raw
stream logs (bench `raw/*.jsonl`) and classify what it was looking for.

    python3 bench/grep_intents.py OUT_DIR [OUT_DIR ...] [--arms branch,mcp] [--json out.jsonl]

For each Grep: pattern, path/glob, output_mode, the prompt it answered (1 = task, 2 = tests and
call sites follow-up), its step within that prompt, the files/lines it returned, whether the
identifiers it looked for were already in what laya-codex injected, which returned files the
agent's answer then named, the next tool call, and an intent:

  definition     where an identifier is defined (`def x`, `class X`, `fn x`, `x =` ...)
  callers-uses   an identifier (or a few) searched across a directory or the repository
  tests          test files or test structure (`describe(`, `def test_`, a path under tests/)
  in-file        identifiers looked up inside one file the agent already knows (Grep as a Read)
  exact-text     a non-identifier string or regex across a directory or the repository
  non-code       a JSON/Markdown/YAML/TOML/... target
  other          anything else

`--json` writes one record per Grep. `--replay BIN --repos-dir DIR` sends each Grep's identifiers
(`a|b`) and path to `BIN mcp`'s `search` against an indexed LAYA_CODEX_HOME and reports, by intent,
how much of what the agent used from the Grep the search result holds.
"""
import argparse
import collections
import glob
import json
import os
import re
import sys

NON_CODE_EXT = (".json", ".md", ".mdx", ".yaml", ".yml", ".toml", ".txt", ".cfg", ".ini", ".lock", ".rst", ".html",
                ".css", ".svg")
DEF_WORDS = ("def", "class", "fn", "function", "struct", "enum", "trait", "impl", "interface", "type", "const",
             "let", "var", "export", "pub", "async def", "mod", "static")
TEST_STRUCT = re.compile(r"describe|\bit\\?\(|\btest\\?\(|def test|test_|#\[test\]|#\\\[test|mod tests|expect\\?\(")
IDENT = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
HIT_LINE = re.compile(r"^(?:(?P<path>[^:\n]+?):)?(?P<line>\d+)[:-]")
MENTION = re.compile(r"[\w./-]+\.(?:py|ts|tsx|js|rs|go|java|md|json|toml|yaml|yml)\b")


def is_test_path(p):
    p = p.lower()
    return bool(re.search(r"(^|/)(tests?|__tests__|spec)(/|$)|(^|/)test_[^/]*$|_test\.\w+$|\.(test|spec)\.\w+$|"
                          r"(^|/)tests?\.rs$", p))


def is_non_code(p):
    return p.lower().endswith(NON_CODE_EXT)


def rel(path, repo):
    if not path:
        return ""
    if repo and path.startswith(repo):
        path = path[len(repo):]
    return path.lstrip("/")


def split_alternatives(pattern):
    """Top-level `|` alternatives of a regex (good enough for agent patterns: no nested groups)."""
    out, depth, cur = [], 0, ""
    i = 0
    while i < len(pattern):
        c = pattern[i]
        if c == "\\" and i + 1 < len(pattern):
            cur += pattern[i:i + 2]
            i += 2
            continue
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth = max(0, depth - 1)
        if c == "|" and depth == 0:
            out.append(cur)
            cur = ""
        else:
            cur += c
        i += 1
    out.append(cur)
    return [a for a in out if a]


def strip_regex(alt):
    """An alternative reduced to the text an identifier lookup would carry, or None when it is
    not identifier-shaped. Returns (kind, ident) with kind "def" or "use"."""
    a = alt.strip()
    a = re.sub(r"\\b|\^|\$|\\s[*+]?|\\s|\(\?:|\)$", " ", a)
    a = a.replace("\\(", "(").replace("\\.", ".").replace("\\[", "[").strip()
    a = re.sub(r"\($|\(\)$|\s+\($", "", a).strip()
    a = a.rstrip("(").strip()
    words = a.split()
    kind = "use"
    if len(words) >= 2 and " ".join(words[:-1]) in DEF_WORDS:
        kind, a = "def", words[-1]
    elif len(words) >= 2 and words[0] in DEF_WORDS:
        kind, a = "def", words[1]
    a = a.lstrip(".").rstrip(":=( ").strip()
    if a.endswith(" ="):
        kind, a = "def", a[:-2].strip()
    return (kind, a) if IDENT.match(a) else None


def pattern_idents(pattern):
    """(identifiers, all alternatives identifier-shaped?, any definition-shaped?)."""
    alts = split_alternatives(pattern)
    parsed = [strip_regex(a) for a in alts]
    idents = [p[1] for p in parsed if p]
    return idents, bool(alts) and all(parsed), any(p and p[0] == "def" for p in parsed)


LITERAL = re.compile(r"^[\w$.@/-]{3,}$")


def pattern_literals(pattern):
    """Alternatives that are plain text naming code without being identifiers: a header or flag
    (`Permissions-Policy`) or a module path (`middleware/etag`)."""
    out = []
    for a in split_alternatives(pattern):
        t = a.replace("\\", "").strip("'\"")
        if LITERAL.match(t) and ("-" in t or "/" in t) and re.search(r"[A-Za-z]", t):
            out.append(t)
    return out


def parse_hits(text, scope_file):
    """(path, line) pairs a Grep result returned (content mode), plus the files it listed."""
    hits, files = [], []
    lines = text.splitlines()
    if lines and re.match(r"^Found \d+ files?", lines[0]):
        return hits, [l.strip() for l in lines[1:] if l.strip()]
    for l in lines:
        m = HIT_LINE.match(l)
        if m and (m.group("path") or scope_file):
            p = m.group("path") or scope_file
            if l[m.end() - 1] == ":":  # a match line, not a -A/-B/-C context line
                hits.append((p, int(m.group("line"))))
            if p not in files:
                files.append(p)
            continue
        m = re.match(r"^([^:\s][^:]*):(\d+)$", l.strip())  # count mode
        if m and not l.startswith("Found"):
            files.append(m.group(1))
    return hits, files


def injected_ranges(ctx):
    """Ranges an injection showed: inlined `### path:a-b` blocks and `- path:n:` usage lines."""
    out = []
    for m in re.finditer(r"^### (\S+?):(\d+)-(\d+)", ctx, re.M):
        out.append((m.group(1), int(m.group(2)), int(m.group(3))))
    for m in re.finditer(r"^- (\S+?):(\d+): ", ctx, re.M):
        out.append((m.group(1), int(m.group(2)), int(m.group(2))))
    return out


def classify(g):
    targets = [g["path"], g["glob"]] if (g["path"] or g["glob"]) else []
    pat = g["pattern"]
    if any(t and is_non_code(t.strip("{}*!")) for t in targets) or \
            (g["files"] and all(is_non_code(f) for f in g["files"])):
        return "non-code"
    if any(t and not t.startswith("!") and is_test_path(t) for t in targets) or TEST_STRUCT.search(pat):
        return "tests"
    if g["def_shaped"] and g["ident_shaped"]:
        return "definition"
    if g["scope"] == "file" and g["idents"]:
        return "in-file"
    if g["ident_shaped"]:
        return "callers-uses"
    if g["scope"] != "file":
        return "exact-text"
    return "other"


def sessions(raw_path):
    """Yield per-prompt dicts: injection text, tool calls in order (with results), answer text."""
    prompts, cur = [], None
    results = {}
    seen_blocks = set()
    for line in open(raw_path):
        try:
            e = json.loads(line)
        except ValueError:
            continue
        t, st = e.get("type"), e.get("subtype")
        if t == "system" and st == "hook_response" and e.get("hook_event") == "UserPromptSubmit":
            try:
                ctx = json.loads(e.get("output") or "{}")["hookSpecificOutput"]["additionalContext"]
            except (ValueError, KeyError, TypeError):
                ctx = ""
            cur = {"injection": ctx, "calls": [], "answer": ""}
            prompts.append(cur)
        elif t == "system" and st == "init" and (cur is None or cur["calls"] or cur["answer"]):
            if cur is None or cur["answer"]:
                cur = {"injection": "", "calls": [], "answer": ""}
                prompts.append(cur)
        elif t == "assistant" and cur is not None:
            m = e["message"]
            for i, b in enumerate(m.get("content") or []):
                key = (m.get("id"), b.get("id") or i, b.get("type"))
                if key in seen_blocks:  # stream lines repeat a message's blocks
                    continue
                seen_blocks.add(key)
                if b.get("type") == "tool_use":
                    cur["calls"].append({"msg": m.get("id"), "id": b["id"], "name": b["name"],
                                         "input": b.get("input") or {}})
        elif t == "user":
            c = e.get("message", {}).get("content")
            for b in c if isinstance(c, list) else []:
                if b.get("type") == "tool_result":
                    r = b.get("content")
                    if isinstance(r, list):
                        r = "\n".join(x.get("text", "") for x in r if isinstance(x, dict))
                    results[b.get("tool_use_id")] = r or ""
        elif t == "result" and cur is not None:
            cur["answer"] = e.get("result") or ""
    for p in prompts:
        for c in p["calls"]:
            c["result"] = results.get(c["id"], "")
    return prompts


def grep_records(raw_path, repo_root=None):
    stem = os.path.basename(raw_path)[:-len(".jsonl")]
    task, _, arm = stem.partition("_")
    arm = re.sub(r"_r\d+$", "", arm)
    out_dir = os.path.dirname(os.path.dirname(raw_path))
    repo_name = os.path.basename(out_dir)
    prompts = sessions(raw_path)
    shown = []
    for k, p in enumerate(prompts, 1):
        shown += injected_ranges(p["injection"])
        ctx_so_far = "\n".join(q["injection"] for q in prompts[:k])
        msgs = []
        for c in p["calls"]:
            if c["msg"] not in msgs:
                msgs.append(c["msg"])
        for j, c in enumerate(p["calls"]):
            if c["name"] != "Grep":
                continue
            inp = c["input"]
            root = repo_root or ""
            if not root:
                m = re.search(r"(/[^\"]*?/repos/[^/\"]+)", json.dumps(inp))
                root = m.group(1) if m else ""
            path = rel(inp.get("path", ""), root)
            scope = "repo" if not path else ("file" if re.search(r"\.\w+$", path) else "dir")
            idents, ident_shaped, def_shaped = pattern_idents(inp.get("pattern", ""))
            hits, files = parse_hits(c["result"], path if scope == "file" else None)
            in_ctx = [h for h in hits if any(h[0] == s[0] and s[1] <= h[1] <= s[2] for s in shown)]
            named = set(MENTION.findall(p["answer"]))
            used_files = [f for f in files if f in named or any(n.endswith(f) or f.endswith(n) for n in named)]
            nxt = p["calls"][j + 1] if j + 1 < len(p["calls"]) else None
            g = {
                "repo": repo_name, "task": task, "arm": arm, "prompt": k, "step": j + 1,
                "turn": msgs.index(c["msg"]) + 1, "pattern": inp.get("pattern", ""), "path": path,
                "glob": inp.get("glob", "") or inp.get("type", ""), "output_mode": inp.get("output_mode", "files_with_matches"),
                "context": any(inp.get(f) for f in ("-A", "-B", "-C", "context")), "head_limit": inp.get("head_limit"),
                "scope": scope, "idents": idents, "ident_shaped": ident_shaped, "def_shaped": def_shaped,
                "literals": pattern_literals(inp.get("pattern", "")),
                "hits": hits, "files": files, "result_chars": len(c["result"]),
                "idents_in_injection": [i for i in idents if re.search(r"\b%s\b" % re.escape(i), ctx_so_far)],
                "hits_in_injection": len(in_ctx), "used_files": used_files,
                "next": (nxt["name"] + ":" + rel(nxt["input"].get("file_path", nxt["input"].get("path", "")), root))
                if nxt else None,
                "answer_lines": sorted({int(x) for x in re.findall(r":(\d{1,5})\b", p["answer"])}),
            }
            g["intent"] = classify(g)
            yield g


ANSWERABLE = {"definition", "callers-uses", "tests", "in-file"}


def report(records):
    n = len(records)
    by_intent = collections.Counter(g["intent"] for g in records)
    sessions_n = len({(g["repo"], g["task"], g["arm"]) for g in records})
    print("Grep calls: %d in %d sessions that grepped" % (n, sessions_n))
    print("\n| intent | n | share | prompt 1 | prompt 2 | scoped to one file | idents already injected | then Read same file |")
    print("|---|---|---|---|---|---|---|---|")
    for intent, c in by_intent.most_common():
        rs = [g for g in records if g["intent"] == intent]
        p1 = sum(g["prompt"] == 1 for g in rs)
        f1 = sum(g["scope"] == "file" for g in rs)
        inj = sum(bool(g["idents_in_injection"]) for g in rs)
        rd = sum(bool(g["next"]) and g["next"].startswith("Read:") and
                 any(g["next"][5:] == f or g["next"][5:].endswith(f) for f in (g["files"] or [g["path"]])) for g in rs)
        print("| %s | %d | %.0f%% | %d | %d | %d | %d | %d |" % (intent, c, 100.0 * c / n, p1, c - p1, f1, inj, rd))
    ans = sum(by_intent[i] for i in ANSWERABLE)
    print("\nIdentifier lookups a `search` with exact identifier answers could serve: %d/%d (%.0f%%)" %
          (ans, n, 100.0 * ans / max(n, 1)))
    modes = collections.Counter(g["output_mode"] for g in records)
    print("output_mode: %s" % dict(modes))
    print("with context flags (-A/-B/-C): %d; regex alternation (|): %d; glob/type filter: %d; head_limit: %d" % (
        sum(g["context"] for g in records), sum("|" in g["pattern"] for g in records),
        sum(bool(g["glob"]) for g in records), sum(bool(g["head_limit"]) for g in records)))
    print("scope: %s" % dict(collections.Counter(g["scope"] for g in records)))

# --- offline replay: would laya-codex `search` have answered these Greps? -------------------------

def search_query(g):
    """The `search` call an agent would make instead of this Grep: its identifiers and plain-text
    names (`a-b`, `a/b`), `|`-joined, and its path. None when the pattern names neither (a regex)."""
    names = list(dict.fromkeys(g["idents"] + g.get("literals", [])))
    return "|".join(names) if names else None


def parse_search(text):
    """(path, line) pairs and files (shown or counted) in a `search` identifier-lookup result."""
    lines, files, cur = set(), set(), None
    in_code = False
    for l in text.splitlines():
        if l.startswith("```"):
            in_code = not in_code
            continue
        if in_code or not l.strip() or l.startswith("<!--") or l.startswith("### "):
            continue
        m = re.match(r"^    (\d+): ", l)
        if m and cur:
            lines.add((cur, int(m.group(1))))
            continue
        if l.startswith("Not shown"):
            files.update(re.findall(r"(?:: |, )([^\s,(]+) \(\d+", l))
            continue
        m = re.match(r"^(\S+?)( \(test file\))?$", l)
        if m and ("/" in m.group(1) or "." in m.group(1)) and not m.group(1).endswith(":"):
            cur = m.group(1)
            files.add(cur)
    return lines, files


class Mcp:
    """`laya-codex mcp` over stdio (newline-delimited JSON-RPC)."""

    def __init__(self, bin_, repo, env):
        import subprocess
        self.p = subprocess.Popen([bin_, "mcp", "--repo", repo], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  text=True, cwd=repo, env=env)
        self.n = 0
        self.rpc("initialize", {"protocolVersion": "2025-06-18"})

    def rpc(self, method, params):
        self.n += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params}) + "\n")
        self.p.stdin.flush()
        return json.loads(self.p.stdout.readline())

    def search(self, query, path=None):
        args = {"query": query}
        if path:
            args["path"] = path
        res = self.rpc("tools/call", {"name": "search", "arguments": args}).get("result", {})
        return (res.get("content") or [{}])[0].get("text", ""), bool(res.get("isError"))

    def close(self):
        self.p.stdin.close()
        self.p.wait(timeout=30)


def coverage(g, text):
    """How much of what the agent used from this Grep the `search` result holds.
    used lines = Grep match lines in files the prompt's answer named (all match lines when it named none);
    cited lines = the used lines whose number the answer cites."""
    got_lines, got_files = parse_search(text)
    used_files = set(g["used_files"]) or set(g["files"])
    hits = [tuple(h) for h in g["hits"]]
    used = [h for h in hits if h[0] in used_files] or hits
    cited = [h for h in used if h[1] in set(g["answer_lines"])]
    frac = lambda xs, have: (sum(x in have for x in xs) / len(xs)) if xs else None
    return {"line_cov": frac(used, got_lines), "cited_cov": frac(cited, got_lines),
            "file_cov": frac(sorted(used_files), got_files | {p for p, _ in got_lines}),
            "n_used": len(used), "search_chars": len(text), "truncated": "Not shown" in text,
            "missing": [h for h in used if h not in got_lines][:5]}


def replay(records, bin_, repos_dir, env):
    import time
    out = []
    by_repo = collections.defaultdict(list)
    for g in records:
        by_repo[g["repo"]].append(g)
    for repo, gs in sorted(by_repo.items()):
        m = Mcp(bin_, os.path.join(repos_dir, repo), env)
        for g in gs:
            q = search_query(g)
            r = dict(g, query=q)
            if q is None:
                r.update(answered=False)
            else:
                t0 = time.time()
                text, err = m.search(q, g["path"] or None)
                r.update(answered=not err and "matching lines in" in text, is_error=err,
                         search_s=round(time.time() - t0, 3), **coverage(g, text))
            out.append(r)
        m.close()
    return out


def coverage_report(rows):
    print("\n| intent | Greps | searchable | line coverage (used) | cited-line coverage | file coverage | "
          "fully covered | median chars search / Grep |")
    print("|---|---|---|---|---|---|---|---|")
    order = ["callers-uses", "tests", "in-file", "definition", "exact-text", "non-code", "other"]
    for intent in order + ["ALL"]:
        rs = [r for r in rows if intent == "ALL" or r["intent"] == intent]
        if not rs:
            continue
        ans = [r for r in rs if r.get("answered")]
        mean = lambda k: (sum(r[k] for r in ans if r.get(k) is not None) /
                          max(1, sum(1 for r in ans if r.get(k) is not None)))
        full = sum(1 for r in ans if (r.get("line_cov") in (None, 1.0)) and (r.get("file_cov") in (None, 1.0)))
        med = lambda xs: sorted(xs)[len(xs) // 2] if xs else 0
        print("| %s | %d | %d | %.2f | %.2f | %.2f | %d (%.0f%% of Greps) | %d / %d |" % (
            intent, len(rs), len(ans), mean("line_cov"), mean("cited_cov"), mean("file_cov"), full,
            100.0 * full / len(rs), med([r["search_chars"] for r in ans]), med([r["result_chars"] for r in ans])))
    ts = sorted(r["search_s"] for r in rows if r.get("search_s") is not None)
    if ts:
        print("\nsearch latency: p50 %.2f s, p90 %.2f s, max %.2f s" % (
            ts[len(ts) // 2], ts[int(len(ts) * 0.9)], ts[-1]))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("dirs", nargs="+", help="bench output dirs containing raw/*.jsonl")
    ap.add_argument("--arms", default="", help="comma-separated arms to keep (default: all)")
    ap.add_argument("--json", help="write one JSON record per Grep here")
    ap.add_argument("--replay", metavar="BIN", help="send each Grep's identifiers and path to `BIN mcp` search "
                    "(needs --repos-dir; uses LAYA_CODEX_* from the environment) and report answer coverage")
    ap.add_argument("--repos-dir", help="directory holding the bench repositories by name (for --replay)")
    ap.add_argument("--replay-json", help="write the replay rows here")
    a = ap.parse_args(argv)
    arms = {x for x in a.arms.split(",") if x}
    records = []
    for d in a.dirs:
        for f in sorted(glob.glob(os.path.join(d, "raw", "*.jsonl"))):
            for g in grep_records(f):
                if not arms or g["arm"] in arms:
                    records.append(g)
    if not records:
        print("no Grep calls found", file=sys.stderr)
        return 1
    report(records)
    if a.replay:
        rows = replay(records, a.replay, a.repos_dir, dict(os.environ))
        coverage_report(rows)
        if a.replay_json:
            with open(a.replay_json, "w") as out:
                for r in rows:
                    out.write(json.dumps(r) + "\n")
    if a.json:
        with open(a.json, "w") as out:
            for g in records:
                out.write(json.dumps(g) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
