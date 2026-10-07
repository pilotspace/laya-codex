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


# ----------------------------------------------------------------------------- windows + distillation
@pytest.fixture(scope="module")
def tok():
    import common
    if not os.path.isfile(os.path.join(common.BASE_MODEL, "tokenizer", "tokenizer.json")):
        pytest.skip("no laya tokenizer (set LAYA_CODEX_FT_BASE)")
    from transformers import AutoTokenizer
    return AutoTokenizer.from_pretrained(os.path.join(common.BASE_MODEL, "tokenizer"))


def _lst():
    body = "\n".join("value_%d = compute(%d)" % (i, i) for i in range(300))
    return {"focus": "fix compute", "candidates": [
        {"path": "a.py", "start": 1, "end": 300, "text": body, "label": 1.0},
        {"path": "b.py", "start": 5, "end": 6, "text": "x = 1\ny = 2", "label": 0.0}]}


def test_state_ids_are_cut_once_and_sliced_per_window(tok):
    import common
    lst = _lst()
    sids = listwise.list_state_ids(tok, lst, cap=384)
    assert len(sids[0]) == 384 and len(sids[1]) < 20
    q = listwise.question(lst)
    for w, ml in ((128, 512), (256, 704), (384, 704)):
        want = [common.encode_ids(tok, q, common.state_ids(tok, common.render_state(
            c["path"], c["start"], c["end"], c["text"]), budget=w), max_len=ml) for c in lst["candidates"]]
        assert listwise.build_list(tok, lst, sids, window=w, max_len=ml) == want


def test_encode_list_default_is_the_r1_view(tok):
    lst = _lst()
    assert listwise.encode_list(tok, lst) == listwise.build_list(
        tok, lst, listwise.list_state_ids(tok, lst, cap=128), window=128, max_len=512)
    assert listwise.encode_list(tok, lst, window=384, max_len=704) == listwise.build_list(
        tok, lst, listwise.list_state_ids(tok, lst, cap=384), window=384, max_len=704)


def test_build_list_refuses_a_window_beyond_the_cap(tok):
    lst = _lst()
    with pytest.raises(ValueError):
        listwise.build_list(tok, lst, listwise.list_state_ids(tok, lst, cap=128), window=256, max_len=704)


def test_kd_loss_is_minimal_at_the_calibrated_teacher():
    import torch
    t = torch.tensor([[0.0, 2.0], [0.0, -1.0], [0.5, 0.0], [0.0, 0.3]])
    T = 0.8
    best = torch.stack([torch.zeros(4), (t[:, 1] - t[:, 0]) / T], -1)  # student margin = teacher margin / T
    s = best.clone().requires_grad_(True)
    loss = listwise.kd_loss(s, t, T)
    loss.backward()
    assert s.grad.abs().max() < 1e-5
    for other in (torch.zeros(4, 2), -best, best * 2):
        assert listwise.kd_loss(other, t, T) > loss.detach() + 1e-4


def test_kd_loss_pointwise_only_without_list_weight():
    import torch
    s = torch.tensor([[0.0, 1.0], [0.0, -1.0]])
    t = torch.tensor([[0.0, 1.0], [0.0, 1.0]])
    pt = torch.sigmoid(torch.tensor([1.0, 1.0]))
    target = torch.stack([1 - pt, pt], -1)
    want = -(target * torch.log_softmax(s, -1)).sum(-1).mean()
    assert torch.allclose(listwise.kd_loss(s, t, 1.0, list_weight=0.0), want)
