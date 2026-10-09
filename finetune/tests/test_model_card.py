"""The Hugging Face card of a student run is written from its run manifest (finetune/runs/r2.json) alone."""
import copy
import json
import os
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

import model_card  # noqa: E402

RUN = os.path.join(os.path.dirname(HERE), "runs", "r2.json")


@pytest.fixture
def run():
    return json.load(open(RUN))


def test_the_card_names_the_model_its_base_and_how_laya_codex_serves_it(run):
    md = model_card.student_card(run)
    assert md.startswith("---\nlicense: apache-2.0\nbase_model: answerdotai/ModernBERT-base\n")
    assert "# laya-code-r2" in md
    s = run["model"]["serving"]
    assert "%d tokens" % s["state_tokens"] in md
    assert "top %d" % s["score_top"] in md
    assert "%.4f" % run["model"]["noul_temperature"] in md
    assert run["model"]["init"]["encoder_revision"] in md


def test_the_card_reports_the_replay_and_latency_from_the_run(run):
    md = model_card.student_card(run)
    w = run["replay"]["default_wiring"]
    r2, r1 = w["laya-code-r2"], w["laya-code-r1"]
    kw = run["replay"]["g1_screen"]["keywords"]
    for want in ("%d of %d" % (r2["gold_inlined"], r2["gold_total"]), "| %d |" % r1["gold_inlined"],
                 "| %d |" % kw["gold_inlined"], "%d ms" % r2["hook_ms_p50"], "%d ms" % r1["hook_ms_p50"]):
        assert want in md, want


def test_every_number_comes_from_the_manifest(run):
    other = copy.deepcopy(run)
    other["replay"]["default_wiring"]["laya-code-r2"]["gold_inlined"] = 199
    other["model"]["serving"]["score_top"] = 9
    md = model_card.student_card(other)
    assert "199 of 238" in md and "top 9" in md
    assert "**141 of 238**" not in md


def test_the_card_lists_training_and_held_out_repositories_and_its_limits(run):
    md = model_card.student_card(run)
    for repo in list(run["repos"]["train"]) + list(run["repos"]["heldout"]):
        assert repo in md, repo
    assert "## Intended use" in md and "## Limits" in md


def test_a_pending_upload_is_said_and_a_published_revision_is_pinned(run):
    assert "not published yet" in model_card.student_card(run)
    done = copy.deepcopy(run)
    done["published"]["revision"] = "a" * 40
    md = model_card.student_card(done)
    assert "hf download tindang/laya-code --revision %s" % ("a" * 40) in md
    assert "not published yet" not in md


def test_a_manifest_without_the_replay_is_refused(run):
    bad = copy.deepcopy(run)
    del bad["replay"]["default_wiring"]
    with pytest.raises(KeyError):
        model_card.student_card(bad)
