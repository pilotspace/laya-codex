"""Unit tests for the benchmark harness helpers: `python3 -m unittest bench/test_bench.py`."""
import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import runs  # noqa: E402


def row(arm, task, rep=0, **kw):
    r = {"arm": arm, "task_id": task, "rep": rep, "rc": [0], "wall_s": 10.0, "reading_tokens": 100,
         "injected_tokens": 0, "total_input_tokens": 1000, "output_tokens": 50, "num_turns": 4,
         "cost_usd": 0.1, "recall": 1.0, "precision": 0.5, "hit_any": 1.0}
    r.update(kw)
    return r


def write(rows):
    f = tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False)
    f.write("\n".join(json.dumps(r) for r in rows) + "\n")
    f.close()
    return f.name


class LoadRuns(unittest.TestCase):
    def test_repeats_of_a_task_are_averaged_not_overwritten(self):
        path = write([row("a", "t1", 0, wall_s=10.0, recall=1.0), row("a", "t1", 1, wall_s=20.0, recall=0.0),
                      row("b", "t1", 0, wall_s=8.0)])
        by = runs.load_runs(path)
        self.assertEqual(by["a"]["t1"]["wall_s"], 15.0)
        self.assertEqual(by["a"]["t1"]["recall"], 0.5)
        self.assertEqual(by["a"]["t1"]["reps"], 2)
        self.assertEqual(by["b"]["t1"]["reps"], 1)

    def test_rows_without_rep_are_one_repeat_each_as_in_older_runs(self):
        old = row("a", "t1")
        del old["rep"]
        by = runs.load_runs(write([old]))
        self.assertEqual(by["a"]["t1"]["reps"], 1)
        self.assertEqual(by["a"]["t1"]["wall_s"], 10.0)

    def test_every_return_code_is_kept_so_failures_stay_visible(self):
        by = runs.load_runs(write([row("a", "t1", 0, rc=[0, 0]), row("a", "t1", 1, rc=[0, "timeout"])]))
        self.assertEqual(by["a"]["t1"]["rc"], [0, 0, 0, "timeout"])

    def test_optional_metrics_average_over_the_repeats_that_have_them(self):
        by = runs.load_runs(write([row("a", "t1", 0, recall_all_turns=1.0), row("a", "t1", 1, recall_all_turns=0.5)]))
        self.assertEqual(by["a"]["t1"]["recall_all_turns"], 0.75)
        self.assertNotIn("recall_all_turns", runs.load_runs(write([row("a", "t1")]))["a"]["t1"])

    def test_filter_by_arm(self):
        by = runs.load_runs(write([row("a", "t1"), row("b", "t1")]), arms=("a",))
        self.assertEqual(list(by), ["a"])

    def test_model_is_kept_and_mixed_models_are_refused(self):
        self.assertEqual(runs.load_runs(write([row("a", "t1", model="sonnet")]))["a"]["t1"]["model"], "sonnet")
        with self.assertRaises(ValueError):
            runs.load_runs(write([row("a", "t1", 0, model="sonnet"), row("a", "t1", 1, model="haiku")]))


class ArmSpec(unittest.TestCase):
    def test_plain_arm_uses_its_own_template_and_the_default_binary(self):
        self.assertEqual(runs.parse_arm("laya-adaptive"), ("laya-adaptive", "laya-adaptive", None))

    def test_named_arm_with_template_and_binary(self):
        self.assertEqual(runs.parse_arm("v030:laya-adaptive@/opt/v030/laya-codex"),
                         ("v030", "laya-adaptive", "/opt/v030/laya-codex"))

    def test_binary_without_template(self):
        self.assertEqual(runs.parse_arm("laya-lex@/x/laya-codex"), ("laya-lex", "laya-lex", "/x/laya-codex"))

    def test_baseline_takes_no_template_or_binary(self):
        self.assertEqual(runs.parse_arm("baseline"), ("baseline", None, None))
        with self.assertRaises(ValueError):
            runs.parse_arm("baseline@/x")

    def test_bad_names_are_refused(self):
        for bad in ("", "a b", "../x", "a:"):
            with self.assertRaises(ValueError):
                runs.parse_arm(bad)


class HookLog(unittest.TestCase):
    def test_rank_modes_of_the_prompts_in_order(self):
        log = write([
            {"event": "SessionStart", "action": "index_started", "injected_chars": 0},
            {"event": "UserPromptSubmit", "action": "inject", "injected_chars": 350, "rank_mode": "laya",
             "scored": 16, "offered": 16, "candidates": 24},
            {"event": "PreToolUse", "action": "narrow_read", "injected_chars": 35},
            {"event": "UserPromptSubmit", "action": "inject", "injected_chars": 70, "rank_mode": "laya-partial",
             "scored": 8, "offered": 16, "candidates": 24},
        ])
        h = runs.read_hook_log(log)
        self.assertEqual(h["rank_modes"], ["laya", "laya-partial"])
        self.assertEqual(h["scored"], [16, 8])
        self.assertEqual(h["injected_tokens"], 100 + 10 + 20)
        self.assertEqual(h["prompt_injected_tokens"], [100, 20])
        self.assertEqual(h["hook_actions"], {"index_started": 1, "inject": 2, "narrow_read": 1})

    def test_older_builds_without_rank_fields_record_unknown(self):
        h = runs.read_hook_log(write([{"event": "UserPromptSubmit", "action": "inject", "injected_chars": 7}]))
        self.assertEqual(h["rank_modes"], [None])

    def test_missing_log(self):
        h = runs.read_hook_log("/nonexistent/hooklog.jsonl")
        self.assertEqual((h["injected_tokens"], h["rank_modes"]), (0, []))


class RunBench(unittest.TestCase):
    def setUp(self):
        import run_bench
        self.rb = run_bench

    def test_repeat_zero_keeps_the_pre_repeat_plan_and_file_names(self):
        tasks = [{"id": "t1"}, {"id": "t2"}]
        one = self.rb.make_plan(tasks, ["baseline", "laya"], 1)
        two = self.rb.make_plan(tasks, ["baseline", "laya"], 2)
        self.assertEqual(two[: len(one)], one)
        self.assertEqual(len(two), 8)
        self.assertEqual([r for _, _, r in two], [0] * 4 + [1] * 4)
        self.assertEqual(self.rb.run_name("t1", "laya", 0), "t1_laya")
        self.assertEqual(self.rb.run_name("t1", "laya", 2), "t1_laya_r2")

    def test_only_arms_with_their_own_binary_get_their_own_home_and_port(self):
        specs = [("baseline", None, None), ("laya-adaptive", "laya-adaptive", None),
                 ("v030", "laya-adaptive", "/x/v030"), ("new", "laya-adaptive", "/x/new")]
        env = self.rb.arm_env("/tmp/out", specs)
        self.assertEqual(env["baseline"], {})
        self.assertEqual(env["laya-adaptive"], {})
        self.assertEqual(env["v030"]["LAYA_CODEX_HOME"], "/tmp/out/homes/v030")
        self.assertNotEqual(env["v030"]["LAYA_CODEX_MOON_PORT"], env["new"]["LAYA_CODEX_MOON_PORT"])

    def test_configs_are_rendered_per_arm_with_that_arms_binary(self):
        out = tempfile.mkdtemp()
        fake_bin = os.path.join(out, "laya-codex")
        open(fake_bin, "w").close()
        cfg = self.rb.render_configs(out, [("baseline", None, None), ("v030", "laya-adaptive", fake_bin)])
        settings = open(os.path.join(cfg, "laya-settings.v030.json")).read()
        self.assertIn(fake_bin, settings)
        self.assertNotIn("@LAYA_CODEX_BIN@", settings)
        self.assertTrue(os.path.exists(os.path.join(cfg, "laya-mcp.v030.json")))


if __name__ == "__main__":
    unittest.main()
