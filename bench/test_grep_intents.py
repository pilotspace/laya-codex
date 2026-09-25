"""Unit tests for bench/grep_intents.py: `python3 -m unittest bench/test_grep_intents.py`."""
import os
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
        text = ("<!-- laya-codex search: ... -->\n3 matching lines in 3 files (definitions: 1, test files: 1). "
                "Complete: every matching line is listed below.\n\n"
                "src/digest.ts\n  in generateDigest:\n    18: export const generateDigest = 1  [definition]\n\n"
                "src/digest.test.ts (test file)\n  in describe('d') > it('x'):\n    7: generateDigest()\n\n"
                "Not shown (matching lines per file): src/other.ts (1), and 2 more files.\n"
                "\nDefinition:\n### src/digest.ts:18-20\n```ts\n    99: not a hit\n```\n")
        lines, files = gi.parse_search(text)
        self.assertEqual(lines, {("src/digest.ts", 18), ("src/digest.test.ts", 7)})
        self.assertEqual(files, {"src/digest.ts", "src/digest.test.ts", "src/other.ts"})

    def test_coverage_counts_lines_in_files_the_answer_named(self):
        g = {"hits": [["a.py", 1], ["a.py", 2], ["b.py", 5]], "files": ["a.py", "b.py"], "used_files": ["a.py"],
             "answer_lines": [2]}
        c = gi.coverage(g, "a.py\n  top level:\n    1: x\n")
        self.assertEqual((c["line_cov"], c["cited_cov"], c["file_cov"]), (0.5, 0.0, 1.0))
        self.assertEqual(c["missing"], [("a.py", 2)])


if __name__ == "__main__":
    unittest.main()
