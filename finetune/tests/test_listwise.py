import os
import sys

import numpy as np
import pytest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import listwise  # noqa: E402


def cand(path, label=0.0):
    return {"path": path, "label": label}


def test_blend_order_matches_retriever_weighted_scores():
    # 4 reached candidates: score = 0.5*(1 - r/4) + 0.5*p
    p = [0.0, 0.0, 0.0, 1.0]
    order = listwise.blend_order(p, w=0.5, score_top=16)
    # scores: 0.5, 0.375, 0.25, 0.125+0.5=0.625 -> [3, 0, 1, 2]
    assert order == [3, 0, 1, 2]


def test_blend_order_keeps_unreached_tail_in_lexical_order():
    p = [0.0] * 3 + [1.0] * 3
    order = listwise.blend_order(p, w=0.5, score_top=2)
    assert order[:2] == [0, 1] and order[2:] == [2, 3, 4, 5]


def test_blend_order_ties_keep_lexical_order():
    assert listwise.blend_order([0.5, 0.5, 0.5], w=0.0, score_top=16) == [0, 1, 2]


def test_file_order_is_first_appearance():
    cands = [cand("a"), cand("b"), cand("a"), cand("c")]
    assert listwise.file_order(cands, [2, 3, 0, 1]) == ["a", "c", "b"]


def test_list_metrics_file_recall_and_mrr():
    lst = {"candidates": [cand("x"), cand("y"), cand("g", 1.0), cand("g", 0.6)], "touched_files": ["g", "z"]}
    lex = listwise.list_metrics(lst, [0, 1, 2, 3])
    assert lex["file_r@1"] == 0.0 and lex["file_r@3"] == 1.0  # z is not among the candidates
    assert lex["mrr_pos"] == pytest.approx(1 / 3)
    top = listwise.list_metrics(lst, [2, 0, 1, 3])
    assert top["file_r@1"] == 1.0 and top["mrr_pos"] == 1.0


def test_top_chunk_gold_counts_files_of_the_first_chunks():
    # the replay inlines ~2 blocks: two chunks of one file carry one gold file, not two
    lst = {"candidates": [cand("g", 1.0), cand("g", 0.6), cand("h", 1.0), cand("x")], "touched_files": ["g", "h"]}
    m = listwise.list_metrics(lst, [0, 1, 2, 3])
    assert m["top2_gold"] == 0.5 and m["top3_gold"] == 1.0
    assert m["file_r@2"] == 1.0  # distinct-file recall would overstate what is inlined


def test_listwise_loss_prefers_ranking_positives_first():
    import torch
    y = torch.tensor([1.0, 0.0, 0.6, 0.0])
    good = torch.tensor([3.0, -2.0, 1.0, -2.0])
    bad = torch.tensor([-2.0, 3.0, -2.0, 1.0])
    assert listwise.listwise_loss(good, y) < listwise.listwise_loss(bad, y)
    assert listwise.listwise_loss(good, torch.zeros(4)) == 0.0  # no positive: no listwise signal


def test_summarize_compares_models_on_the_same_lists():
    lists = [{"candidates": [cand("x"), cand("g", 1.0)], "touched_files": ["g"]} for _ in range(3)]
    probs = [np.array([0.1, 0.9])] * 3
    s = listwise.summarize(lists, probs)
    assert s["blend"]["file_r@1"] == 1.0 and s["lexical"]["file_r@1"] == 0.0
    assert s["model_only"]["file_r@1"] == 1.0
    assert s["n_lists"] == 3
