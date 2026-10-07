"""Tests for the follow-up picks replay: `python3 -m pytest bench/test_followup_picks.py`."""
import json
import os
import shutil
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import followup_completeness as fc  # noqa: E402
import followup_picks as fp  # noqa: E402


class NameMatching(unittest.TestCase):
    """A port of `search`'s name lookup (crates/laya-cli/src/mcp.rs)."""

    def test_a_name_matches_at_word_underscore_and_camel_case_boundaries(self):
        self.assertTrue(fp.has_name("def test_raise_for_status():", "raise_for_status"))
        self.assertTrue(fp.has_name("class AsyncHTTPTransport:", "HTTPTransport"))
        self.assertFalse(fp.has_name("unquote(x)", "quote"))
        self.assertFalse(fp.has_name("self.cookies = {}", "cookie"))

    def test_literals_match_as_plain_text(self):
        self.assertTrue(fp.has_name('h.get("content-type")', "content-type"))

    def test_search_queries_name_code_or_describe_it(self):
        self.assertEqual(fp.identifier_query("callers of parse_url"), ["parse_url"])
        self.assertEqual(fp.identifier_query("a_b|Foo::bar"), ["a_b", "bar"])
        self.assertEqual(fp.identifier_query("middleware/etag"), ["middleware/etag"])
        self.assertIsNone(fp.identifier_query("wal replay after a crash"))


class GrepNames(unittest.TestCase):
    """The three parsing fixes over followup_completeness.lookup_names."""

    def test_a_word_boundary_escape_is_not_part_of_the_name(self):
        # the old replay read `\bparse` as the name "bparse"
        self.assertEqual(fc.lookup_names({"name": "Grep", "input": {"pattern": r"\bparse\("}}), ("grep", {"bparse"}))
        self.assertEqual(fp.grep_names(r"\bparse\("), (["parse"], False))
        self.assertEqual(fp.grep_names(r"\bClient\b|\bsend_request\s*\("), (["Client", "send_request"], False))

    def test_hyphen_and_slash_literals_stay_whole(self):
        # the old replay split a header or a module path into words
        self.assertEqual(fc.lookup_names({"name": "Grep", "input": {"pattern": "content-type"}})[1], {"content"})
        self.assertEqual(fp.grep_names("content-type|middleware/etag"), (["content-type", "middleware/etag"], False))

    def test_test_and_definition_structure_is_not_a_name(self):
        # the old replay looked up "test_" as if it were a name
        self.assertEqual(fc.lookup_names({"name": "Grep", "input": {"pattern": r"fn test_"}})[1], {"test_"})
        self.assertEqual(fp.grep_names(r"#\[test\]|fn test_"), ([], True))
        self.assertEqual(fp.grep_names(r"def test_.*send"), (["send"], True))
        self.assertEqual(fp.grep_names(r"describe\(|it\("), ([], True))
        self.assertEqual(fp.grep_names(r"#\[test\]"), ([], True))           # an escaped bracket is not a class


class Scope(unittest.TestCase):
    def test_path_type_and_glob(self):
        self.assertTrue(fp.in_scope("src/a.rs", ["src"], [], "rust"))
        self.assertFalse(fp.in_scope("src/a.py", ["src"], [], "rust"))
        self.assertFalse(fp.in_scope("lib/a.rs", ["src"], [], None))
        self.assertTrue(fp.in_scope("src/x.test.ts", [""], ["*.test.ts"], None))
        self.assertFalse(fp.in_scope("src/x.ts", [""], ["*.{test,spec}.ts"], None))
        self.assertFalse(fp.in_scope("src/x.test.ts", [""], ["!*.test.ts"], None))


FILES = {
    "src/parse.rs": "pub fn parse(input: &str) -> Ast {\n    helper_parse(input)\n}\n"
                    "fn helper_parse(s: &str) -> Ast {\n"
                    "    todo!()\n}\n",
    "src/lib.rs": "mod parse;\npub use parse::parse;\n",
    "tests/parse_test.rs": "#[test]\nfn test_parse() {\n    parse(\"x\");\n}\n",
}


def hook(text):
    return {"type": "system", "subtype": "hook_response", "hook_event": "UserPromptSubmit",
            "stdout": json.dumps({"hookSpecificOutput": {"additionalContext": text}})}


def usage(read, write, out=10):
    return {"input_tokens": 2, "cache_creation_input_tokens": write, "cache_read_input_tokens": read,
            "output_tokens": out}


def call(rid, u, *tools):
    return {"type": "assistant", "request_id": rid,
            "message": {"usage": u, "content": [{"type": "tool_use", "id": i, "name": n, "input": inp}
                                                for i, n, inp in tools]}}


INIT = {"type": "system", "subtype": "init"}
INJ1 = ("Ranked locations:\n1. src/parse.rs — 1-3 fn parse\n\n"
        "### src/parse.rs:1-3 — fn parse\n```rust\npub fn parse() {}\n```\n")


def session(repo_dir):
    p = lambda f: os.path.join(repo_dir, f)  # noqa: E731
    return [
        hook(INJ1), INIT, {"type": "result", "result": "`parse` calls `helper_parse`.\nFILES: src/parse.rs"},
        hook("Ranked locations:\n"), INIT,
        call("r1", usage(10_000, 1_000), ("t1", "mcp__laya-codex__search", {"query": "parse"})),
        call("r2", usage(11_000, 500, 20), ("t2", "Read", {"file_path": p("tests/parse_test.rs")}),
             ("t3", "Grep", {"pattern": r"\bhelper_parse\b", "path": p("src")})),
        call("r3", usage(11_500, 400), ("t4", "Grep", {"pattern": "unrelated_name"})),
        {"type": "assistant", "request_id": "r4", "message": {"usage": usage(11_900, 300, 400),
                                                              "content": [{"type": "text", "text": "done"}]}},
        {"type": "result", "result": "done", "usage": {"output_tokens": 460, "iterations": [{"output_tokens": 400}]}},
    ]


class WithRepo(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.dir = tempfile.mkdtemp()
        for f, text in FILES.items():
            os.makedirs(os.path.dirname(os.path.join(cls.dir, f)), exist_ok=True)
            with open(os.path.join(cls.dir, f), "w") as h:
                h.write(text)
        cls.repo = fp.Repo(cls.dir, "toy")
        p1, p2 = fc.parse_session(session(cls.dir))
        cls.s = fp.session_record("v0", "toy", "t", "laya", p1, p2, cls.repo)

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.dir)

    def test_uses_are_found_with_search_semantics(self):
        # `parse` is also a part of `helper_parse` and `test_parse`, as in `search`
        self.assertEqual(self.repo.uses("parse"), {"src/parse.rs": [1, 2, 4], "src/lib.rs": [1, 2],
                                                   "tests/parse_test.rs": [2, 3]})
        self.assertEqual(self.repo.def_files("helper_parse"), ["src/parse.rs"])

    def test_candidates_come_from_the_answer_and_the_inlined_definitions(self):
        self.assertEqual(self.s["cand_sets"]["A"], ["parse", "helper_parse"])

    def test_requirements(self):
        r = {x["id"]: x for x in self.s["reqs"]}
        self.assertEqual(r["t1"]["names"], ["parse"])
        self.assertEqual({k[1] for k in r["t1"]["items"]}, {"src/parse.rs", "src/lib.rs", "tests/parse_test.rs"})
        self.assertEqual(r["t2"]["anchored"], ["parse"])          # the Read range holds a use of `parse`
        self.assertEqual(r["t3"]["names"], ["helper_parse"])
        self.assertEqual({k[1] for k in r["t3"]["items"]}, {"src/parse.rs"})
        self.assertEqual(r["t4"]["miss"], "stale/absent: name not in checkout")

    def test_the_oracle_removes_whole_calls_within_the_budget(self):
        cov, chars, chosen = fp.oracle(self.s, 10 ** 9, "call")
        self.assertEqual(cov, 2)                                   # r1 and r2; r3 asks for an absent name
        self.assertTrue(any(k[0] == "SNIP" for k in chosen))
        cov_lists, _, _ = fp.oracle(self.s, 10 ** 9, "call", snippets=False)
        self.assertEqual(cov_lists, 1)                             # r2 also Reads
        need_r1 = sum(v[0] for k, v in {x["id"]: x for x in self.s["reqs"]}["t1"]["items"].items())
        self.assertEqual(fp.oracle(self.s, need_r1, "call")[0], 1)
        self.assertEqual(fp.oracle(self.s, need_r1 - 1, "call")[0], 0)

    def test_a_structural_grep_is_never_covered(self):
        r = fp.requirement({"name": "Grep", "input": {"pattern": r"#\[test\]"}}, self.repo, {"parse"})
        self.assertEqual(r["miss"], "structural grep (list tests/defs in a file)")

    def test_a_perfect_name_picker_gets_whole_lists_only(self):
        self.assertEqual(fp.names_only_k(self.s, 1, 10 ** 9), 1)

    def test_per_call_saving_counts_each_token_once(self):
        # lookup call r2 (of 4 API calls): its prefix read, its output, and the tokens it adds
        # (written by r3, read again by r4); the lookups' 60 output tokens split 10:20:10
        out_r2 = 60 * 20 / 40
        added = 400 + 2
        want = (0.20 * 11_000 + 2.0 * 2 + 10.0 * out_r2 + 4.0 * added + 0.20 * added * 1) / 1e6
        self.assertAlmostEqual(self.s["savings"][1], want)
        self.assertEqual(len(self.s["savings"]), 3)


class Cost(unittest.TestCase):
    def test_injection_price_and_break_even(self):
        # written once at $4/M, read by each later call of the turn at $0.20/M
        self.assertAlmostEqual(fp.injection_price(1000, 2), 1000 * 0.455 * (4.0 + 0.4) / 1e6)
        self.assertAlmostEqual(fp.break_even_chars(0.0093), 0.0093 / (0.455 * 4.2 / 1e6))


if __name__ == "__main__":
    unittest.main()
