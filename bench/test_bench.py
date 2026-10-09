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
        # 0.456 tokens per injected char: 350 -> 160, 35 -> 16, 70 -> 32
        self.assertEqual(h["injected_tokens"], 160 + 16 + 32)
        self.assertEqual(h["prompt_injected_tokens"], [160, 32])
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


class TokenEstimate(unittest.TestCase):
    """Characters to tokens, per kind of text, calibrated on billed cache writes (v12-v14)."""

    def test_rates_are_the_calibrated_tokens_per_char(self):
        self.assertEqual(runs.TOKENS_PER_CHAR, {"Read": 0.424, "Grep": 0.460, "Glob": 0.554, "search": 0.445,
                                                "injected": 0.456, "answer": 0.455})

    def test_each_kind_converts_at_its_own_rate(self):
        self.assertEqual(runs.tok_estimate(1000, "Read"), 424)
        self.assertEqual(runs.tok_estimate(1000, "Grep"), 460)
        self.assertEqual(runs.tok_estimate(1000, "search"), 445)
        self.assertEqual(runs.tok_estimate(1000, "injected"), 456)
        self.assertEqual(runs.tok_estimate(0, "Read"), 0)

    def test_code_is_no_longer_counted_at_3_5_chars_per_token(self):
        # 3.5 chars per token under-counted code ~1.6x against what the API bills.
        self.assertGreater(runs.tok_estimate(3500, "injected"), 1500)

    def test_unknown_kind_is_refused(self):
        with self.assertRaises(KeyError):
            runs.tok_estimate(10, "prose")


class ParseStreamReading(unittest.TestCase):
    """run_bench.parse_stream counts each tool result at its tool's rate."""

    def stream(self, results):
        uses = [{"type": "assistant", "message": {"id": "m%d" % i, "content": [
            {"type": "tool_use", "id": "t%d" % i, "name": name, "input": {}}]}} for i, (name, _) in enumerate(results)]
        res = [{"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "t%d" % i, "content": body}]}} for i, (_, body) in enumerate(results)]
        return [json.dumps(e) for pair in zip(uses, res) for e in pair]

    def test_tool_results_are_counted_per_tool_kind(self):
        import run_bench
        listed = [{"type": "text", "text": "x" * 100}]
        out = run_bench.parse_stream(self.stream([("Read", "a" * 1000), ("Grep", "b" * 1000), ("Glob", "c" * 1000),
                                                  ("mcp__laya-codex__search", listed), ("Bash", "d" * 1000)]))
        search = runs.tok_estimate(len(json.dumps(listed)), "search")
        self.assertEqual(out["reading_tokens"], 424 + 460 + 554 + search)
        self.assertEqual(out["read_bytes"], 3000 + len(json.dumps(listed)))


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

    def test_every_arm_runs_first_equally_often_and_each_pair_in_both_orders(self):
        arms = ["baseline", "laya", "gate"]
        tasks = [{"id": "t%d" % i} for i in range(20)]
        plan = self.rb.make_plan(tasks, arms, 1)
        orders = [[a for a, t, _ in plan if t is task] for task in tasks]
        self.assertTrue(all(sorted(o) == sorted(arms) for o in orders))
        firsts = [sum(o[0] == a for o in orders) for a in arms]
        self.assertLessEqual(max(firsts) - min(firsts), 1, firsts)
        for a in arms:
            for b in arms:
                if a < b:
                    before = sum(o.index(a) < o.index(b) for o in orders)
                    self.assertLessEqual(abs(before - 10), 2, (a, b, before))

    def test_each_arm_and_repeat_gets_its_own_prompt_cache_prefix(self):
        # Identical first requests from two arms share Anthropic's prompt cache, so whichever arm
        # ran second was billed cache reads for the first arm's writes.
        tags = {}
        for arm in ("baseline", "laya", "gate"):
            for rep in (0, 1):
                flags = self.rb.arm_flags(arm, "/cfg", rep)
                tag = flags[flags.index("--append-system-prompt") + 1]
                self.assertNotIn(arm, tag)  # neutral: the arm name must not steer the model
                tags[(arm, rep)] = tag
        self.assertEqual(len(set(tags.values())), 6)
        self.assertEqual(self.rb.arm_flags("laya", "/cfg", 1), self.rb.arm_flags("laya", "/cfg", 1))

    def test_each_session_gets_its_own_memo_salt_and_the_memo_stays_on(self):
        # The follow-up must hit its own session's score cache (as in real use), but no session
        # may read another arm's, task's, repeat's or attempt's cache.
        base = {"PATH": "/bin"}
        env = lambda arm, task, rep, sid: self.rb.session_env(arm, task, rep, sid, "/h.jsonl", "medium",
                                                              {"LAYA_CODEX_HOME": "/home"}, base)
        e = env("laya", "t1", 0, "s1")
        self.assertNotIn("LAYA_CODEX_MEMO", e)  # memo on: follow-ups are served from the cache
        self.assertEqual(e["LAYA_CODEX_HOOK_LOG"], "/h.jsonl")
        self.assertEqual(e["CLAUDE_EFFORT"], "medium")
        self.assertEqual(e["LAYA_CODEX_HOME"], "/home")
        self.assertEqual(e["PATH"], "/bin")
        salts = {env(a, t, r, s)["LAYA_CODEX_MEMO_SALT"]
                 for a in ("laya", "gate") for t in ("t1", "t2") for r in (0, 1) for s in ("s1", "s2")}
        self.assertEqual(len(salts), 16)
        self.assertTrue(all(salts))
        self.assertEqual(env("laya", "t1", 0, "s1"), e)

    def test_an_explicit_memo_off_still_reaches_every_session(self):
        e = self.rb.session_env("laya", "t1", 0, "s1", "/h.jsonl", "medium", None, {"LAYA_CODEX_MEMO": "0"})
        self.assertEqual(e["LAYA_CODEX_MEMO"], "0")
        self.assertIn("LAYA_CODEX_MEMO_SALT", e)

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

    def test_headline_metric_order_leads_with_cost_then_reading_plus_injected(self):
        import headline
        keys = list(headline.METRICS)
        self.assertEqual(keys[:3], ["cost", "reading_plus_injected", "reading_tokens"])
        self.assertEqual(headline.LABELS["reading_plus_injected"], "Reading + injected")

    def test_headline_chart_leads_with_cost_and_keeps_code_tokens_beside_it(self):
        import headline
        self.assertEqual(headline.CHART_METRICS[:3], ["cost", "reading_plus_injected", "reading_tokens"])
        self.assertEqual(headline.CHART_METRICS[-1], "wall_clock")

    def test_headline_title_does_not_assert_a_direction_the_data_may_not_show(self):
        import headline
        self.assertNotIn("reads less code", headline.CHART_TITLE.lower())


class ReadSummary(unittest.TestCase):
    """The read charts describe the same paired tasks as the savings chart, and no task drops out
    of a figure because the thing it measures never happened."""

    def rrow(self, task, reads=1, precision=1.0, recall=1.0, wasted=0, read_turn=None, seen_turn=None):
        return {"task_id": task, "reads": reads, "read_precision": precision, "read_recall": recall,
                "wasted_read_tokens": wasted, "first_gold_read_turn": read_turn, "first_gold_seen_turn": seen_turn}

    def test_only_paired_tasks_count(self):
        import headline
        rows = {"moon": {"baseline": [self.rrow("a", seen_turn=4), self.rrow("stalled", seen_turn=1)],
                         "laya": [self.rrow("a", seen_turn=0), self.rrow("stalled", seen_turn=0)],
                         "laya-lex": [self.rrow("a", seen_turn=0)]}}
        s = headline.read_summary(rows, {("moon", "a")}, "laya", "baseline", "laya-lex")
        self.assertEqual(s["n"], 1)
        self.assertEqual(s["journey"]["baseline"], [4])
        self.assertEqual(s["journey"]["laya_seen"], [0])

    def test_precision_pools_reads_so_sessions_without_reads_do_not_shift_it(self):
        import headline
        rows = {"r": {"baseline": [self.rrow("a", reads=1, precision=1.0), self.rrow("b", reads=3, precision=0.0),
                                   self.rrow("c", reads=0, precision=None)],
                      "laya": [self.rrow(t) for t in "abc"], "laya-lex": []}}
        s = headline.read_summary(rows, {("r", t) for t in "abc"}, "laya", "baseline", "laya-lex")
        self.assertAlmostEqual(s["reads"]["read_precision"]["baseline"], 0.25)

    def test_turn_is_a_median_where_never_counts_as_last(self):
        import headline
        rows = {"r": {"baseline": [self.rrow("a", seen_turn=2), self.rrow("b", seen_turn=None), self.rrow("c", seen_turn=None)],
                      "laya": [self.rrow("a", seen_turn=0), self.rrow("b", seen_turn=0), self.rrow("c", seen_turn=None)],
                      "laya-lex": []}}
        s = headline.read_summary(rows, {("r", t) for t in "abc"}, "laya", "baseline", "laya-lex")
        seen = s["reads"]["seen_turn"]
        self.assertIsNone(seen["baseline"])  # the middle task never saw gold code
        self.assertEqual(seen["laya"], 0)
        self.assertEqual((seen["never_baseline"], seen["never_laya"]), (2, 1))

    def test_first_read_turn_reports_the_tasks_that_never_read_gold(self):
        import headline
        rows = {"r": {"baseline": [self.rrow("a", read_turn=5), self.rrow("b", read_turn=4)],
                      "laya": [self.rrow("a", read_turn=3), self.rrow("b", read_turn=None)], "laya-lex": []}}
        s = headline.read_summary(rows, {("r", "a"), ("r", "b")}, "laya", "baseline", "laya-lex")
        self.assertEqual(s["first_gold_read_turn"]["laya"], {"median": None, "never": 1})
        self.assertEqual(s["first_gold_read_turn"]["baseline"], {"median": 4.5, "never": 0})

    def test_an_arm_missing_a_paired_task_is_refused(self):
        import headline
        rows = {"r": {"baseline": [self.rrow("a")], "laya": [], "laya-lex": []}}
        with self.assertRaises(ValueError):
            headline.read_summary(rows, {("r", "a")}, "laya", "baseline", "laya-lex")

    def test_a_run_without_a_keyword_only_arm_reports_no_lex_figures(self):
        # v13 ran stock and laya-codex only; the lex fields are left out, not zero-filled.
        import headline
        rows = {"r": {"baseline": [self.rrow("a", seen_turn=3)], "laya": [self.rrow("a", seen_turn=0)]}}
        s = headline.read_summary(rows, {("r", "a")}, "laya", "baseline", None)
        self.assertNotIn("lex", s["read_precision"])
        self.assertNotIn("lex", s["first_gold_read_turn"])
        self.assertNotIn("lex_seen", s["journey"])
        self.assertEqual(s["journey"]["laya_seen"], [0])


class ReadsChart(unittest.TestCase):
    def test_subtitle_states_the_paired_task_count_and_never_is_drawn_as_text(self):
        sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts"))
        import charts
        d = {"n_tasks": 51, "reads": {"seen_turn": {"label": "Turn right code arrives", "baseline": None,
                                                    "laya": 0, "better": "lower", "fmt": "{:g}"},
                                      "gold_seen": {"label": "Relevant code found", "baseline": 0.7, "laya": 0.8,
                                                    "better": "higher", "fmt": "{:.0%}"}}}
        out = charts.reads(d, charts.THEMES["light"])
        self.assertIn("51 paired tasks", out)
        self.assertIn(">never<", out)

    def test_journey_legend_says_never_when_the_median_task_never_got_there(self):
        sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts"))
        import charts
        d = {"n_tasks": 3, "journey": {"baseline": [2, 3, 4], "laya_seen": [0, 0, None],
                                       "laya_read": [2, None, None], "lex_seen": []}}
        out = charts.journey(d, charts.THEMES["light"])
        self.assertIn("first correct Read (median never)", out)
        self.assertNotIn("inf", out)

    def test_savings_subtitle_fits_the_chart_width(self):
        sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts"))
        import charts
        import re
        d = {"n_tasks": 60, "repos": ["moon", "httpx", "hono"], "model": "claude-sonnet-5-5", "effort": "medium",
             "metrics": {"cost": {"label": "Cost", "pct": -13.7, "lo": -17.8, "hi": -9.1},
                         "tok": {"label": "Tokens", "pct": 9.5, "lo": -1.6, "hi": 22.6}}}
        sub = [s for s in re.findall(r">([^<]+)</text>", charts.savings(d, charts.THEMES["light"])) if "tasks" in s][0]
        self.assertLessEqual(len(sub), 110, sub)  # about 6 px per character at 12 px, 760 px wide

    def test_savings_subtitle_names_the_resolved_model_and_effort(self):
        # "sonnet" is an alias that moved from Sonnet 5 to Sonnet 5.5 between runs; the chart must
        # name the model the run actually used.
        sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts"))
        import charts
        d = {"n_tasks": 51, "repos": ["httpx"], "model": "claude-sonnet-5", "effort": "medium",
             "metrics": {"cost": {"label": "Cost", "pct": -8.8, "lo": -16.3, "hi": -1.9}}}
        out = charts.savings(d, charts.THEMES["light"])
        self.assertIn("claude-sonnet-5 · medium effort", out)


class InsightCharts(unittest.TestCase):
    """The insight charts name the run they come from, so they can't pass for a newer one."""

    def setUp(self):
        sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts"))
        import insight_charts
        self.ic = insight_charts
        self.dir = tempfile.mkdtemp()
        for repo in insight_charts.REPOS:
            os.makedirs(os.path.join(self.dir, repo))
            with open(os.path.join(self.dir, repo, "runs.jsonl"), "w") as f:
                for arm, wall, out in (("baseline", 40.0, 4000), ("laya", 30.0, 3000), ("laya-lex", 35.0, 3500)):
                    f.write(json.dumps(row(arm, "t", wall_s=wall, output_tokens=out, num_turns=10)) + "\n")
                f.write(json.dumps(row("laya-lex", "unpaired", wall_s=99.0, output_tokens=9000)) + "\n")

    def test_time_and_turns_charts_name_the_run_and_use_the_given_arm(self):
        t = self.ic.THEMES["light"]
        time_svg = self.ic.time_chart(self.dir, "laya", "benchmark v10", t)
        turns_svg = self.ic.turns_chart(self.dir, "laya", "benchmark v10", t)
        self.assertIn("9 benchmark v10 sessions", time_svg)
        self.assertIn("benchmark v10, 3 paired tasks", turns_svg)
        self.assertNotIn("v2", time_svg + turns_svg)

    def test_time_legend_counts_the_laya_arms_the_run_has(self):
        t = self.ic.THEMES["light"]
        self.assertIn("laya-codex (both arms)", self.ic.time_chart(self.dir, "laya", "benchmark v10", t))
        one = tempfile.mkdtemp()
        for repo in self.ic.REPOS:
            os.makedirs(os.path.join(one, repo))
            with open(os.path.join(one, repo, "runs.jsonl"), "w") as f:
                for arm, wall, out in (("baseline", 40.0, 4000), ("laya", 30.0, 3000)):
                    f.write(json.dumps(row(arm, "t", wall_s=wall, output_tokens=out, num_turns=10)) + "\n")
        svg_one = self.ic.time_chart(one, "laya", "benchmark v13", t)
        self.assertIn(">laya-codex<", svg_one)
        self.assertNotIn("both arms", svg_one)

    def test_followup_chart_compares_the_two_labelled_replays(self):
        rows = [{"label": lab, "repo": repo, "turn": 2, "chars": c}
                for repo in self.ic.REPOS for lab, c in (("v030", 4000), ("candidate", 1000))]
        before, after = write(rows), write(rows)
        svg = self.ic.followup_chart((before, "v030", "v0.3.0"), (after, "candidate", "v0.4.0"), self.ic.THEMES["light"])
        self.assertIn("v0.4.0", svg)
        self.assertIn("(−75%)", svg)


class StaleGold(unittest.TestCase):
    """Tasks are past commits run on a later checkout: a gold file whose change is gone from that
    checkout can't be found by any search, so recall is also reported without it."""

    def test_classify_by_how_much_of_the_added_code_is_still_there(self):
        import stale_gold
        added = ["let budget = mmap_budget(cfg);", "enforce_budget(&mut seg, budget)?;"]
        self.assertEqual(stale_gold.classify(added, "fn f() {\n    let budget = mmap_budget(cfg);\n"
                                                    "    enforce_budget(&mut seg, budget)?;\n}")[0], "present")
        self.assertEqual(stale_gold.classify(added, "fn f() { rewritten_entirely(); }")[0], "drifted")
        self.assertEqual(stale_gold.classify([], "import os\n")[0], "removal only")
        self.assertEqual(stale_gold.classify(added, None)[0], "file gone")

    def test_trivial_lines_do_not_count_as_evidence(self):
        import stale_gold
        self.assertEqual(stale_gold.meaningful_added(["}", "  });", "// note", "x = compute_total(a)"]),
                         ["x = compute_total(a)"])

    def test_recall_on_findable_gold_skips_stale_files_and_tasks(self):
        import headline
        findable = {"t1": {"src/a.py"}, "t2": set()}
        rows = [(row("laya", "t1", named=["src/a.py"]), row("baseline", "t1", named=["src/b.py"])),
                (row("laya", "t2", named=[]), row("baseline", "t2", named=[]))]
        pairs = headline.findable_pairs(rows, findable)
        self.assertEqual(len(pairs), 1)
        f = headline.findable_recall(findable, both_turns=False)
        self.assertEqual((f(pairs[0][0]), f(pairs[0][1])), (1.0, 0.0))

    def test_both_turns_counts_the_second_answer(self):
        import headline
        findable = {"t1": {"src/a.py", "tests/test_a.py"}}
        r = row("laya", "t1", named=["src/a.py"], turn2={"named": ["tests/test_a.py"]})
        self.assertEqual(headline.findable_recall(findable, both_turns=True)(r), 1.0)
        self.assertEqual(headline.findable_recall(findable, both_turns=False)(r), 0.5)


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


class DropStalls(unittest.TestCase):
    """A task whose session hung in any arm is dropped from every arm, so pairs stay complete."""

    def rows(self):
        return [
            {"arm": "baseline", "task_id": "ok", "rc": [0, 0], "prompt_wall_s": [20.0, 15.0]},
            {"arm": "laya", "task_id": "ok", "rc": [0, 0], "prompt_wall_s": [18.0, 12.0]},
            {"arm": "baseline", "task_id": "slow", "rc": [0, 0], "prompt_wall_s": [916.0, 12.0]},
            {"arm": "laya", "task_id": "slow", "rc": [0, 0], "prompt_wall_s": [30.0, 10.0]},
            {"arm": "baseline", "task_id": "failed", "rc": [1, 1], "prompt_wall_s": [40.0, 30.0]},
            {"arm": "laya", "task_id": "failed", "rc": [0, 0], "prompt_wall_s": [25.0, 20.0]},
        ]

    def test_long_prompts_and_failed_exits_drop_the_task_from_every_arm(self):
        import drop_stalls
        self.assertEqual(drop_stalls.stalled_tasks(self.rows(), 300), {"slow", "failed"})

    def test_the_copy_keeps_only_complete_pairs(self):
        import subprocess
        with tempfile.TemporaryDirectory() as d:
            src, out = os.path.join(d, "run"), os.path.join(d, "out")
            os.makedirs(src)
            with open(os.path.join(src, "runs.jsonl"), "w") as f:
                for r in self.rows():
                    f.write(json.dumps(r) + "\n")
            here = os.path.dirname(os.path.abspath(__file__))
            subprocess.run([sys.executable, os.path.join(here, "drop_stalls.py"), src, out],
                           check=True, capture_output=True)
            kept = [json.loads(l) for l in open(os.path.join(out, "runs.jsonl"))]
            self.assertEqual({(r["arm"], r["task_id"]) for r in kept}, {("baseline", "ok"), ("laya", "ok")})


class OneProcessSession(unittest.TestCase):
    """A task's prompts run as one live Claude session, as a user types them. Resuming the
    session per prompt (`claude -p --resume`) rebuilt the first message differently when prompt 1
    was answered in one API call, so the follow-up rewrote the whole prompt cache -- a charge only
    the arm that answers without tools (laya-codex) ever paid."""

    class FakeProc:
        def __init__(self, cmd, **kw):
            self.cmd, self.sent, self.returncode = cmd, [], None
            outer = self

            class In:
                def write(self, s):
                    outer.sent.append(json.loads(s))

                def flush(self):
                    pass

                def close(self):
                    pass

            self.stdin = In()
            self.stdout = self._out()

        def _out(self):
            # Output for prompt k appears only once prompt k has been sent.
            for k in range(2):
                while len(self.sent) <= k:
                    yield json.dumps({"type": "never"}) + "\n"  # would mean a prompt was sent late
                yield json.dumps({"type": "assistant", "message": {"id": "m%d" % k, "content": []}}) + "\n"
                yield json.dumps({"type": "result", "result": "answer %d" % k, "total_cost_usd": 0.01}) + "\n"

        def wait(self, timeout=None):
            self.returncode = 0
            return 0

        def kill(self):
            pass

    def test_two_prompts_run_in_one_process_without_resume(self):
        import run_bench
        procs = []

        def popen(cmd, **kw):
            procs.append(self.FakeProc(cmd, **kw))
            return procs[-1]

        class Args:
            model, effort, max_usd, timeout, repo = "claude-sonnet-5-5", "medium", 2.0, 60, "/tmp"
        out = run_bench._claude_session(["first?", "second?"], "baseline", Args, "/cfg", {}, "sid-1", popen=popen)
        self.assertEqual(len(procs), 1)
        cmd = procs[0].cmd
        self.assertNotIn("--resume", cmd)
        self.assertEqual(cmd[cmd.index("--input-format") + 1], "stream-json")
        self.assertEqual(cmd[cmd.index("--session-id") + 1], "sid-1")
        self.assertIn("--append-system-prompt", cmd)
        self.assertEqual([m["message"]["content"] for m in procs[0].sent], ["first?", "second?"])
        self.assertEqual(len(out), 2)
        for k, (lines, rc, wall) in enumerate(out):
            self.assertEqual(json.loads(lines[-1])["result"], "answer %d" % k)
            self.assertNotIn("never", "".join(lines))
            self.assertEqual(rc, 0)

    def test_a_live_sessions_cost_is_its_last_cumulative_total_not_the_sum(self):
        # In one live session each result's total_cost_usd is the session's running total
        # (v13: 120 of 120 second results), so summing them counted prompt 1 twice.
        import run_bench

        def result(cost, out):
            return json.dumps({"type": "result", "result": "FILES: a.py", "total_cost_usd": cost, "num_turns": 1,
                               "usage": {"output_tokens": out}})

        class Args:
            model, effort, max_usd, timeout, repo, turns = "claude-sonnet-5-5", "medium", 2.0, 60, "/tmp", 2
        with tempfile.TemporaryDirectory() as d:
            Args.out = d
            real = run_bench._claude_session
            run_bench._claude_session = lambda *a, **k: [([result(0.08, 100)], 0, 1.0), ([result(0.11, 50)], 0, 1.0)]
            try:
                row, _ = run_bench.run_one("baseline", {"id": "t1", "task": "x", "gold": ["a.py"]}, Args, d)
            finally:
                run_bench._claude_session = real
        self.assertAlmostEqual(row["cost_usd"], 0.11)
        self.assertEqual(row["prompt_cost_usd"], [0.08, 0.03])
        self.assertEqual(row["output_tokens"], 150)  # usage stays per prompt

    def test_grep_intents_splits_a_one_process_session_at_each_result(self):
        import grep_intents
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "t_baseline.jsonl")
            events = [{"type": "system", "subtype": "init"}]
            for k in range(2):
                events += [{"type": "assistant", "message": {"id": "m%d" % k, "content": [
                              {"type": "tool_use", "id": "u%d" % k, "name": "Grep", "input": {"pattern": "x"}}]}},
                           {"type": "result", "result": "answer %d" % k}]
            open(p, "w").write("".join(json.dumps(e) + "\n" for e in events))
            prompts = grep_intents.sessions(p)
        self.assertEqual([len(q["calls"]) for q in prompts], [1, 1])
        self.assertEqual([q["answer"] for q in prompts], ["answer 0", "answer 1"])


class RepoOutsideClaudeHome(unittest.TestCase):
    """Claude Code loads every CLAUDE.md above the working directory, so a benchmark repo under a
    directory holding one (such as ~/.claude) feeds the operator's personal instructions to both arms."""

    def test_a_claude_md_above_the_repo_is_reported(self):
        import run_bench
        with tempfile.TemporaryDirectory() as d:
            os.makedirs(os.path.join(d, "jobs", "repo"))
            open(os.path.join(d, "CLAUDE.md"), "w").write("personal rules")
            open(os.path.join(d, "jobs", "repo", "CLAUDE.md"), "w").write("the repo's own file is fine")
            found = run_bench.claude_md_above(os.path.join(d, "jobs", "repo"))
        self.assertEqual([os.path.basename(os.path.dirname(f)) for f in found], [os.path.basename(d)])

    def test_a_dot_claude_claude_md_above_the_repo_is_reported(self):
        # Claude Code also reads <dir>/.claude/CLAUDE.md in every directory above the working one,
        # which is where a home directory keeps the user's global file: ~/.claude/CLAUDE.md.
        import run_bench
        with tempfile.TemporaryDirectory() as d:
            os.makedirs(os.path.join(d, ".claude"))
            os.makedirs(os.path.join(d, "bench-repos", "repo"))
            open(os.path.join(d, ".claude", "CLAUDE.md"), "w").write("personal rules")
            found = run_bench.claude_md_above(os.path.join(d, "bench-repos", "repo"), stop=d)
        self.assertEqual(found, [os.path.join(d, ".claude", "CLAUDE.md")])

    def test_a_clean_path_reports_nothing(self):
        import run_bench
        with tempfile.TemporaryDirectory() as d:
            os.makedirs(os.path.join(d, "repo"))
            self.assertEqual(run_bench.claude_md_above(os.path.join(d, "repo"), stop=d), [])


if __name__ == "__main__":
    unittest.main()
