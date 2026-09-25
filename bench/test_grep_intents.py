"""Unit tests for bench/grep_intents.py: `python3 -m unittest bench/test_grep_intents.py`."""
import json
import os
import shutil
import tempfile
import time
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import grep_intents as gi  # noqa: E402


def record(**kw):
    g = {"path": "", "glob": "", "pattern": "", "files": [], "scope": "repo", "idents": [], "ident_shaped": False,
         "def_shaped": False}
    g.update(kw)
    idents, shaped, def_shaped = gi.pattern_idents(g["pattern"])
    g.setdefault("idents", idents)
    g["idents"], g["ident_shaped"], g["def_shaped"] = idents, shaped, def_shaped
    return g


class Patterns(unittest.TestCase):
    def test_alternatives_split_at_the_top_level_only(self):
        self.assertEqual(gi.split_alternatives(r"(a|b)|c\|d"), ["(a|b)", r"c\|d"])

    def test_identifiers_come_out_of_agent_regexes(self):
        self.assertEqual(gi.pattern_idents(r"def json\(|jsonlib"), (["json", "jsonlib"], True, True))
        self.assertEqual(gi.pattern_idents(r"\.run_tick\("), (["run_tick"], True, False))
        self.assertEqual(gi.pattern_idents(r"%2F|/.*\?.*=.*/")[1], False)

    def test_plain_text_names_are_kept_for_the_search_query(self):
        self.assertEqual(gi.pattern_literals(r"permissionsPolicy|Permissions-Policy|none"), ["Permissions-Policy"])
        self.assertEqual(gi.pattern_literals(r"middleware/etag|from '\.\./etag'"), ["middleware/etag"])


class Hits(unittest.TestCase):
    def test_content_mode_with_paths_and_context_lines(self):
        hits, files = gi.parse_hits("a/x.py:3:def f():\na/x.py-4-    pass\nb/y.py:9:f()", None)
        self.assertEqual(hits, [("a/x.py", 3), ("b/y.py", 9)])
        self.assertEqual(files, ["a/x.py", "b/y.py"])

    def test_single_file_scope_has_no_path_prefix(self):
        hits, _ = gi.parse_hits("920:            text = x\n921-            more", "httpx/_models.py")
        self.assertEqual(hits, [("httpx/_models.py", 920)])

    def test_files_with_matches(self):
        self.assertEqual(gi.parse_hits("Found 2 files\nsrc/a.ts\nsrc/b.ts", None), ([], ["src/a.ts", "src/b.ts"]))


class Intents(unittest.TestCase):
    def test_a_test_file_probe_is_a_tests_intent(self):
        g = record(pattern=r"it\(|describe\(", path="src/etag/index.test.ts", scope="file")
        self.assertEqual(gi.classify(g), "tests")

    def test_excluding_tests_with_a_negated_glob_is_not_a_tests_intent(self):
        g = record(pattern=r"\.replace\(", path="src/client", glob="!*.test.ts", scope="dir")
        self.assertEqual(gi.classify(g), "callers-uses")

    def test_names_inside_one_known_file(self):
        g = record(pattern="raw_path", path="httpx/_transports/asgi.py", scope="file")
        self.assertEqual(gi.classify(g), "in-file")

    def test_non_code_targets(self):
        g = record(pattern="method-not-allowed", glob="{package.json,jsr.json}")
        self.assertEqual(gi.classify(g), "non-code")


class SearchOutput(unittest.TestCase):
    def test_lines_and_files_are_read_back_from_a_lookup_result(self):
        text = ("`generateDigest`: 5 lines in 4 files (1 test) of 90 files read; first 3 shown, the rest counted below.\n"
                "src/digest.ts\n 18: export const generateDigest = 1 [def]\n  19: return generateDigest\n"
                "src/digest.test.ts (test)\n ‹it('x')›\n  7: generateDigest()\n"
                "Not shown: src/other.ts (1), and 2 more files.\n"
                "Docs: README.md (2).\n"
                "\n### src/digest.ts:18-20\n```ts\n    99: not a hit\n```\n")
        lines, files = gi.parse_search(text)
        self.assertEqual(lines, {("src/digest.ts", 18), ("src/digest.ts", 19), ("src/digest.test.ts", 7)})
        self.assertEqual(files, {"src/digest.ts", "src/digest.test.ts", "src/other.ts", "README.md"})
        self.assertTrue(gi.is_lookup(text))
        self.assertFalse(gi.is_lookup("No match for `x` in the 3 files read.\n"))
        self.assertFalse(gi.is_lookup("### src/a.rs:1-9\n```rust\nfn a() {}\n```\n"))

    def test_coverage_counts_lines_in_files_the_answer_named(self):
        g = {"hits": [["a.py", 1], ["a.py", 2], ["b.py", 5]], "files": ["a.py", "b.py"], "used_files": ["a.py"],
             "answer_lines": [2]}
        c = gi.coverage(g, "a.py\n  top level:\n    1: x\n")
        self.assertEqual((c["line_cov"], c["cited_cov"], c["file_cov"]), (0.5, 0.0, 1.0))
        self.assertEqual(c["missing"], [("a.py", 2)])


def _stream(prompts):
    """Raw stream-json lines for prompts of (injection, [(msg_id, tool_id, name, input, result)], answer)."""
    lines = []
    for injection, calls, answer in prompts:
        out = json.dumps({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": injection}})
        lines.append({"type": "system", "subtype": "hook_response", "hook_event": "UserPromptSubmit", "output": out})
        lines.append({"type": "system", "subtype": "init", "tools": ["Grep", "Read"]})
        for msg, tid, name, inp, res in calls:
            block = {"type": "tool_use", "id": tid, "name": name, "input": inp}
            msg_line = {"type": "assistant", "message": {"id": msg, "content": [block]}}
            lines += [msg_line, msg_line]  # the stream repeats a message's blocks
            lines.append({"type": "user", "message": {"content": [
                {"type": "tool_result", "tool_use_id": tid, "content": res}]}})
        lines.append({"type": "result", "subtype": "success", "result": answer})
    return "\n".join(json.dumps(l) for l in lines) + "\n"


class RawLogs(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.dir)
        raw = os.path.join(self.dir, "httpx", "raw")
        os.makedirs(raw)
        repo = "/x/repos/httpx"
        injection = "### pkg/a.py:1-10 — def f\n```python\ndef f(): pass\n```\n- pkg/b.py:5: f() — use of `f`\n"
        self.path = os.path.join(raw, "t1_mcp.jsonl")
        with open(self.path, "w") as f:
            f.write(_stream([
                (injection, [
                    ("m1", "g1", "Grep", {"pattern": r"f\(", "path": repo + "/pkg", "output_mode": "content"},
                     "pkg/a.py:3:    f()\npkg/c.py:7:    f(1)"),
                    ("m2", "r1", "Read", {"file_path": repo + "/pkg/c.py", "offset": 1, "limit": 20}, "1\tx"),
                ], "It is called in pkg/c.py:7."),
                ("", [
                    ("m3", "g2", "Grep", {"pattern": "def test_", "path": repo + "/tests/test_c.py",
                                          "output_mode": "content"}, "4:def test_f():"),
                ], "FILES: tests/test_c.py"),
            ]))

    def test_prompts_calls_results_and_answers_are_paired(self):
        prompts = gi.sessions(self.path)
        self.assertEqual(len(prompts), 2)
        self.assertEqual([c["name"] for c in prompts[0]["calls"]], ["Grep", "Read"], "repeated blocks deduped")
        self.assertIn("pkg/c.py:7", prompts[0]["calls"][0]["result"])
        self.assertEqual(prompts[1]["answer"], "FILES: tests/test_c.py")

    def test_grep_records_carry_scope_hits_injection_and_intent(self):
        g1, g2 = list(gi.grep_records(self.path))
        self.assertEqual((g1["repo"], g1["task"], g1["arm"], g1["prompt"], g1["step"]), ("httpx", "t1", "mcp", 1, 1))
        self.assertEqual((g1["path"], g1["scope"], g1["intent"]), ("pkg", "dir", "callers-uses"))
        self.assertEqual(g1["hits"], [("pkg/a.py", 3), ("pkg/c.py", 7)])
        self.assertEqual(g1["hits_in_injection"], 1, "a.py:3 is inside the inlined 1-10 block")
        self.assertEqual(g1["idents_in_injection"], ["f"])
        self.assertEqual(g1["used_files"], ["pkg/c.py"])
        self.assertEqual(g1["answer_lines"], [7])
        self.assertEqual(g1["next"], "Read:pkg/c.py")
        self.assertEqual((g2["prompt"], g2["scope"], g2["intent"]), (2, "file", "tests"))
        self.assertEqual(g2["hits"], [("tests/test_c.py", 4)])


class McpClient(unittest.TestCase):
    def test_a_server_that_never_answers_times_out(self):
        d = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, d)
        silent = os.path.join(d, "silent")
        with open(silent, "w") as f:
            f.write("#!/bin/sh\nsleep 30\n")
        os.chmod(silent, 0o755)
        t0 = time.time()
        with self.assertRaises(TimeoutError):
            gi.Mcp(silent, d, dict(os.environ), timeout=0.5)
        self.assertLess(time.time() - t0, 5)


if __name__ == "__main__":
    unittest.main()
