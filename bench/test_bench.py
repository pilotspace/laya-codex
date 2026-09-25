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
         "cost_usd": 0.1, "recall": 1.0, "precision": 0.5, "hit_any": 1.0, "tool_calls": {}, "hook_actions": {}}
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
        self.assertEqual(h["prompt_actions"], ["inject", "inject"])

    def test_older_builds_without_rank_fields_record_unknown(self):
        h = runs.read_hook_log(write([{"event": "UserPromptSubmit", "action": "inject", "injected_chars": 7}]))
        self.assertEqual(h["rank_modes"], [None])

    def test_missing_log(self):
        h = runs.read_hook_log("/nonexistent/hooklog.jsonl")
        self.assertEqual((h["injected_tokens"], h["rank_modes"], h["prompt_actions"]), (0, [], []))

    def test_prompt_actions_records_the_failure_that_hit_each_prompt(self):
        log = write([
            {"event": "UserPromptSubmit", "action": "inject", "injected_chars": 100},
            {"event": "UserPromptSubmit", "action": "query_failed", "injected_chars": 0},
        ])
        h = runs.read_hook_log(log)
        self.assertEqual(h["prompt_actions"], ["inject", "query_failed"])


class LoadRunsInjectionHealth(unittest.TestCase):
    """runs.load_runs prefers a healthy rerun over an unhealthy row for the same (arm, task)."""

    def test_unhealthy_row_dropped_once_a_healthy_row_exists(self):
        by = runs.load_runs(write([
            row("laya", "t1", 0, injection_ok=False, wall_s=50.0),
            row("laya", "t1", 1, injection_ok=True, wall_s=10.0),
        ]))
        self.assertEqual(by["laya"]["t1"]["wall_s"], 10.0)
        self.assertEqual(by["laya"]["t1"]["reps"], 1)
        self.assertNotIn("unhealthy", by["laya"]["t1"])

    def test_all_unhealthy_rows_are_kept_and_flagged(self):
        by = runs.load_runs(write([row("laya", "t1", 0, injection_ok=False, wall_s=50.0)]))
        self.assertEqual(by["laya"]["t1"]["wall_s"], 50.0)
        self.assertTrue(by["laya"]["t1"]["unhealthy"])

    def test_baseline_rows_without_the_field_are_never_dropped(self):
        by = runs.load_runs(write([row("baseline", "t1", 0), row("baseline", "t1", 1)]))
        self.assertEqual(by["baseline"]["t1"]["reps"], 2)
        self.assertNotIn("unhealthy", by["baseline"]["t1"])

    def test_older_rows_without_injection_ok_are_unaffected(self):
        old = row("laya", "t1", 0)
        del old["rc"]
        by = runs.load_runs(write([row("laya", "t1", 0), row("laya", "t1", 1)]))
        self.assertEqual(by["laya"]["t1"]["reps"], 2)
        self.assertNotIn("unhealthy", by["laya"]["t1"])


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

    def test_injection_ok_when_every_prompt_got_a_clean_inject(self):
        self.assertTrue(self.rb.injection_health(["inject", "inject"], 2))

    def test_injection_not_ok_on_a_daemon_failure_action(self):
        self.assertFalse(self.rb.injection_health(["inject", "query_failed"], 2))
        self.assertFalse(self.rb.injection_health(["daemon_unavailable"], 1))

    def test_injection_not_ok_when_fewer_userpromptsubmit_entries_than_prompts_sent(self):
        # e.g. the session died after turn 1 and the follow-up's hook never ran
        self.assertFalse(self.rb.injection_health(["inject"], 2))

    def test_injection_ok_on_ordinary_non_failure_skips(self):
        # already_in_context / no_spans / skip_prompt are normal outcomes, not daemon failures
        self.assertTrue(self.rb.injection_health(["inject", "already_in_context"], 2))

    def test_arm_versions_are_cached_per_resolved_binary_and_baseline_is_none(self):
        calls = []

        def fake_version(path):
            calls.append(path)
            return "laya-codex 0.3.0"

        specs = [("baseline", None, None), ("laya-adaptive", "laya-adaptive", None),
                  ("v030", "laya-adaptive", "/x/v030")]
        versions = self.rb.arm_versions(specs, version_of=fake_version)
        self.assertIsNone(versions["baseline"])
        self.assertEqual(versions["laya-adaptive"], "laya-codex 0.3.0")
        self.assertEqual(versions["v030"], "laya-codex 0.3.0")
        self.assertEqual(len(calls), 2)  # default binary once, v030's own binary once
        self.assertIn("/x/v030", calls)

    def test_rerun_unhealthy_excludes_unhealthy_rows_from_done_but_still_counts_their_spend(self):
        path = write([row("laya", "t1", 0, injection_ok=False, cost_usd=0.5),
                      row("laya", "t2", 0, injection_ok=True, cost_usd=0.3),
                      row("baseline", "t1", 0, cost_usd=0.2)])
        done, spent = self.rb.done_set(path, rerun_unhealthy=True)
        self.assertEqual(done, {("laya", "t2", 0), ("baseline", "t1", 0)})
        self.assertAlmostEqual(spent, 1.0)

        done2, spent2 = self.rb.done_set(path, rerun_unhealthy=False)
        self.assertEqual(done2, {("laya", "t1", 0), ("laya", "t2", 0), ("baseline", "t1", 0)})
        self.assertAlmostEqual(spent2, 1.0)

    def test_done_set_of_a_missing_file_is_empty(self):
        done, spent = self.rb.done_set("/nonexistent/runs.jsonl", rerun_unhealthy=True)
        self.assertEqual((done, spent), (set(), 0.0))


class StatsPooledMetrics(unittest.TestCase):
    def test_reading_plus_injected_leads_output_tokens_is_appended_last(self):
        import stats_pooled
        keys = list(stats_pooled.METRICS)
        self.assertEqual(keys[-1], "output tokens")
        self.assertEqual(keys[:-1], ["reading+injected tokens", "code-reading tokens", "total input tokens",
                                     "wall seconds", "turns", "cost usd"])
        self.assertEqual(stats_pooled.METRICS["output tokens"]({"output_tokens": 123}), 123)
        self.assertEqual(stats_pooled.METRICS["output tokens"]({"output_tokens": None}), 0)


class WarmUp(unittest.TestCase):
    def setUp(self):
        import run_bench
        self.rb = run_bench

    def fake(self, modes):
        calls = []

        class P:
            def __init__(self, out):
                self.stdout, self.returncode = out, 0

        def run(cmd, **kw):
            calls.append(cmd[1])
            if cmd[1] == "query":
                return P(json.dumps({"mode": modes.pop(0) if modes else "lexical", "spans": []}))
            return P("")
        return run, calls

    def test_indexes_then_queries_until_the_model_ranks(self):
        run, calls = self.fake(["lexical", "lexical", "laya"])
        mode = self.rb.warm_arm("/bin/laya", {}, "/repo", run=run, sleep=lambda s: None, timeout_s=60)
        self.assertEqual(mode, "laya")
        self.assertEqual(calls, ["index", "query", "query", "query"])

    def test_gives_up_at_the_deadline_and_reports_the_last_mode(self):
        run, calls = self.fake([])
        t = [0.0]

        def clock():
            t[0] += 10
            return t[0]
        mode = self.rb.warm_arm("/bin/laya", {}, "/repo", run=run, sleep=lambda s: None, timeout_s=30,
                                clock=clock)
        self.assertEqual(mode, "lexical")
        self.assertLessEqual(calls.count("query"), 4)

    def test_rank_mode_counts(self):
        rows = [{"rank_modes": ["laya", "lexical"]}, {"rank_modes": ["laya", None]}, {}]
        self.assertEqual(self.rb.rank_mode_counts(rows), {"laya": 2, "lexical": 1, "unknown": 1})

    def test_report_compares_with_the_first_arm_when_there_is_no_baseline(self):
        out = tempfile.mkdtemp()
        rows = [row("branch", "t1", wall_s=20.0), row("mcp", "t1", wall_s=15.0)]
        with open(os.path.join(out, "runs.jsonl"), "w") as f:
            f.write("\n".join(json.dumps(r) for r in rows) + "\n")
        import argparse
        import contextlib
        import io
        with contextlib.redirect_stdout(io.StringIO()):
            self.rb.report(argparse.Namespace(out=out))
        s = json.load(open(os.path.join(out, "summary.json")))
        self.assertEqual(s["reference"], "branch")
        self.assertEqual(s["arms"]["mcp"]["wall_change_pct"], -25.0)
        self.assertNotIn("wall_change_pct", s["arms"]["branch"])
        self.assertIn("mcp vs branch", open(os.path.join(out, "summary.md")).read())


class TokenMetricIsPrimary(unittest.TestCase):
    """VISION.md: the token target is tokens read plus tokens injected -- it must lead every
    report, with tokens-read shown beside it, not the other way round."""

    def test_stats_metric_order_leads_with_reading_plus_injected(self):
        import stats
        keys = list(stats.METRICS)
        self.assertEqual(keys[0], "reading+injected tokens")
        self.assertEqual(keys[1], "code-reading tokens")

    def test_stats_reading_tolerates_missing_fields(self):
        import stats
        self.assertEqual(stats.METRICS["reading+injected tokens"]({}), 0)
        self.assertEqual(stats.METRICS["reading+injected tokens"]({"reading_tokens": None, "injected_tokens": 5}), 5)

    def test_stats_pooled_metric_order_leads_with_reading_plus_injected(self):
        import stats_pooled
        keys = list(stats_pooled.METRICS)
        self.assertEqual(keys[0], "reading+injected tokens")
        self.assertEqual(keys[1], "code-reading tokens")

    def test_stats_pooled_reading_tolerates_missing_fields(self):
        import stats_pooled
        self.assertEqual(stats_pooled.METRICS["reading+injected tokens"]({}), 0)

    def test_headline_metric_order_leads_with_reading_plus_injected(self):
        import headline
        keys = list(headline.METRICS)
        self.assertEqual(keys[0], "reading_plus_injected")
        self.assertEqual(keys[1], "reading_tokens")
        self.assertEqual(headline.LABELS["reading_plus_injected"], "Reading + injected")

    def test_headline_chart_leads_with_reading_plus_injected(self):
        import headline
        self.assertEqual(headline.CHART_METRICS[0], "reading_plus_injected")
        self.assertIn("reading_tokens", headline.CHART_METRICS)

    def test_headline_title_does_not_assert_a_direction_the_data_may_not_show(self):
        import headline
        self.assertNotIn("reads less code", headline.CHART_TITLE.lower())


class StatsCliIsImportSafe(unittest.TestCase):
    """stats.py must be importable (for METRICS) without running its CLI as a side effect."""

    def test_stats_runs_only_from_main(self):
        import stats
        self.assertTrue(hasattr(stats, "main"))


class ToolCallLedger(unittest.TestCase):
    def test_calls_per_session_split_by_named_tool_and_other(self):
        import ledger
        rows = [
            row("baseline", "t1", tool_calls={"Grep": 5, "Read": 3}),
            row("baseline", "t2", tool_calls={"Grep": 6, "Read": 2, "Bash": 1}),
            row("branch", "t1", tool_calls={"Grep": 3, "Read": 2, "mcp__laya-codex__search": 1}),
        ]
        t = ledger.tool_ledger(rows)
        self.assertAlmostEqual(t["baseline"]["Grep"], 5.5)
        self.assertAlmostEqual(t["baseline"]["Read"], 2.5)
        self.assertAlmostEqual(t["baseline"]["other"], 0.5)  # Bash, unnamed
        self.assertAlmostEqual(t["baseline"]["total"], 8.5)
        self.assertAlmostEqual(t["branch"]["mcp__laya-codex__search"], 1.0)
        self.assertAlmostEqual(t["branch"]["other"], 0.0)

    def test_missing_tool_calls_field_is_zero_not_an_error(self):
        import ledger
        rows = [row("baseline", "t1")]
        del rows[0]["tool_calls"]
        t = ledger.tool_ledger(rows)
        self.assertEqual(t["baseline"]["total"], 0)

    def test_empty_rows_yields_empty_ledger_no_zero_division(self):
        import ledger
        self.assertEqual(ledger.tool_ledger([]), {})

    def test_hook_actions_per_session(self):
        import ledger
        rows = [
            row("branch", "t1", hook_actions={"inject": 2, "already_ranged": 1}),
            row("branch", "t2", hook_actions={"inject": 1}),
        ]
        h = ledger.hook_action_ledger(rows)
        self.assertAlmostEqual(h["branch"]["inject"], 1.5)
        self.assertAlmostEqual(h["branch"]["already_ranged"], 0.5)

    def test_missing_hook_actions_field_is_empty_not_an_error(self):
        import ledger
        rows = [row("baseline", "t1")]
        del rows[0]["hook_actions"]
        h = ledger.hook_action_ledger(rows)
        self.assertEqual(h["baseline"], {})

    def test_load_rows_pools_several_run_dirs(self):
        import ledger
        d1 = tempfile.mkdtemp()
        d2 = tempfile.mkdtemp()
        open(os.path.join(d1, "runs.jsonl"), "w").write(json.dumps(row("baseline", "t1")) + "\n")
        open(os.path.join(d2, "runs.jsonl"), "w").write(json.dumps(row("baseline", "t2")) + "\n")
        rs = ledger.load_rows([d1, d2])
        self.assertEqual({r["task_id"] for r in rs}, {"t1", "t2"})

    def test_v9_ledger_reproduces_the_committed_numbers(self):
        """bench/results/claude-v9: baseline 8.45 tool calls/session (Grep 5.20), branch 5.10
        (Grep 2.85, search 0.02) -- the numbers VISION.md and the team's verified facts record."""
        import ledger
        here = os.path.dirname(os.path.abspath(__file__))
        dirs = [os.path.join(here, "results", "claude-v9", r) for r in ("moon", "httpx", "hono")]
        rs = ledger.load_rows(dirs)
        t = ledger.tool_ledger(rs)
        self.assertAlmostEqual(t["baseline"]["total"], 8.45, places=2)
        self.assertAlmostEqual(t["baseline"]["Grep"], 5.20, places=2)
        self.assertAlmostEqual(t["branch"]["total"], 5.10, places=2)
        self.assertAlmostEqual(t["branch"]["Grep"], 2.85, places=2)
        self.assertAlmostEqual(t["branch"]["mcp__laya-codex__search"], 0.02, places=2)


class ReplayParse(unittest.TestCase):
    def test_inlined_blocks_and_gold_coverage(self):
        import replay_hooks
        ctx = ("1. src/foo.rs — does the thing\n"
               "### src/foo.rs:10-20\ncode about src/bar.py:5\n"
               "### src/baz.ts:1-5\nmore code\n")
        gold = ["src/foo.rs", "src/qux.rs"]
        r = replay_hooks.parse(ctx, gold)
        self.assertEqual(r["inlined"], ["src/foo.rs", "src/baz.ts"])
        self.assertEqual(r["gold_inlined"], ["src/foo.rs"])
        self.assertEqual(r["gold_top2"], ["src/foo.rs"])
        self.assertIn("src/foo.rs", r["gold_named"])

    def test_gold_named_but_not_in_top_two_is_excluded_from_top2(self):
        import replay_hooks
        ctx = "### a.py:1-2\nx\n### b.py:1-2\nx\n### gold.py:1-2\nx\n"
        r = replay_hooks.parse(ctx, ["gold.py"])
        self.assertEqual(r["gold_inlined"], ["gold.py"])
        self.assertEqual(r["gold_top2"], [])

    def test_no_injection_is_empty_not_an_error(self):
        import replay_hooks
        r = replay_hooks.parse("", ["gold.py"])
        self.assertEqual(r["inlined"], [])
        self.assertEqual(r["gold_inlined"], [])
        self.assertEqual(r["gold_top2"], [])

    def test_percentile_p50_and_p95(self):
        import replay_hooks
        values = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0]
        self.assertAlmostEqual(replay_hooks.percentile(values, 0.50), 0.6)
        self.assertAlmostEqual(replay_hooks.percentile(values, 0.95), 1.0)

    def test_percentile_of_one_value(self):
        import replay_hooks
        self.assertEqual(replay_hooks.percentile([0.42], 0.95), 0.42)

    def test_summary_prints_gold_top2_and_p95_columns(self):
        import contextlib
        import io
        import replay_hooks
        path = write([
            {"label": "blend", "repo": "httpx", "task_id": "t1", "turn": 1, "gold": ["a.py"],
             "hook_s": 0.5, "action": "inject", "rank_mode": "laya", "scored": 16, "offered": 16,
             "candidates": 24, "chars": 100, "inlined": ["a.py"], "gold_inlined": ["a.py"],
             "gold_named": ["a.py"], "gold_top2": ["a.py"]},
        ])
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            replay_hooks.summary([path])
        out = buf.getvalue()
        self.assertIn("gold in top 2", out)
        self.assertIn("hook s p95", out)
        self.assertIn("1/1", out)

    def test_summary_tolerates_rows_without_gold_top2_from_older_replays(self):
        import contextlib
        import io
        import replay_hooks
        path = write([
            {"label": "old", "repo": "httpx", "task_id": "t1", "turn": 1, "gold": ["a.py"],
             "hook_s": 0.5, "action": "inject", "rank_mode": "laya", "chars": 100,
             "inlined": ["a.py"], "gold_inlined": ["a.py"], "gold_named": ["a.py"]},
        ])
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            replay_hooks.summary([path])  # must not raise KeyError


class PilotMode(unittest.TestCase):
    def setUp(self):
        import run_bench
        self.rb = run_bench

    def test_pilot_subset_is_three_tasks_evenly_spread_across_the_file(self):
        tasks = [{"id": "t%02d" % i} for i in range(20)]
        subset = self.rb.pilot_subset(tasks)
        self.assertEqual(len(subset), 3)
        self.assertEqual(subset[0]["id"], "t00")
        self.assertEqual(subset[-1]["id"], "t19")

    def test_pilot_subset_is_deterministic(self):
        tasks = [{"id": "t%02d" % i} for i in range(17)]
        self.assertEqual(self.rb.pilot_subset(tasks), self.rb.pilot_subset(tasks))

    def test_pilot_subset_smaller_than_n_returns_everything(self):
        tasks = [{"id": "a"}, {"id": "b"}]
        self.assertEqual(self.rb.pilot_subset(tasks), tasks)

    def test_pilot_subset_of_one_task(self):
        tasks = [{"id": "solo"}]
        self.assertEqual(self.rb.pilot_subset(tasks), tasks)

    def test_pilot_subset_custom_n(self):
        tasks = [{"id": "t%02d" % i} for i in range(10)]
        subset = self.rb.pilot_subset(tasks, n=5)
        self.assertEqual(len(subset), 5)

    def test_pilot_requires_max_total_usd(self):
        import argparse
        args = argparse.Namespace(pilot=True, max_total_usd=0.0)
        with self.assertRaises(SystemExit):
            self.rb.check_pilot_args(args)

    def test_pilot_with_budget_passes(self):
        import argparse
        args = argparse.Namespace(pilot=True, max_total_usd=5.0)
        self.rb.check_pilot_args(args)  # no raise

    def test_non_pilot_run_does_not_require_a_budget(self):
        import argparse
        args = argparse.Namespace(pilot=False, max_total_usd=0.0)
        self.rb.check_pilot_args(args)  # no raise

    def test_budget_reached(self):
        self.assertTrue(self.rb.budget_reached(5.0, 5.0))
        self.assertTrue(self.rb.budget_reached(5.01, 5.0))
        self.assertFalse(self.rb.budget_reached(4.99, 5.0))
        self.assertFalse(self.rb.budget_reached(1.0, 0.0))  # no cap set


class ReportIncludesLedger(unittest.TestCase):
    """The routine per-run summary (bench/run_bench.py report) must carry the tool-call ledger too,
    not just token/wall/cost means -- the plan asks summaries to report it, not a separate tool."""

    def test_summary_md_includes_the_tool_call_ledger(self):
        import argparse
        import contextlib
        import io
        import run_bench
        out = tempfile.mkdtemp()
        rows = [
            row("baseline", "t1", tool_calls={"Grep": 4}, hook_actions={}),
            row("laya", "t1", tool_calls={"Grep": 1, "mcp__laya-codex__search": 1}, hook_actions={"inject": 1}),
        ]
        with open(os.path.join(out, "runs.jsonl"), "w") as f:
            f.write("\n".join(json.dumps(r) for r in rows) + "\n")
        with contextlib.redirect_stdout(io.StringIO()):
            run_bench.report(argparse.Namespace(out=out))
        md = open(os.path.join(out, "summary.md")).read()
        self.assertIn("mcp__laya-codex__search", md)
        self.assertIn("tool calls/session", md)


if __name__ == "__main__":
    unittest.main()
