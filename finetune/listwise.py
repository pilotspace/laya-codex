"""Production-shaped lists (build_data.py) -> model inputs, the production blend, and ranking metrics.

The blend is `crates/laya-rank/src/retriever.rs` (`laya_gate` + `weighted_scores`): the model scores the first
`score_top` (16) lexical candidates; each gets (1-w)*(1 - rank/n) + w*P with n = the number scored; they are
sorted by that score (stable) and the unscored tail follows in lexical order. Metrics are file-level like the
replay gate (does a changed file come first) plus the reciprocal rank of the first changed chunk.
"""
import json
import os

import numpy as np

import common

W, SCORE_TOP = 0.5, 16
KS = (1, 2, 3, 5)


def load_lists(split=None, names=None, data_dir=None, with_pos_only=True, heldout=False):
    data_dir = data_dir or os.path.join(common.WORK, "data_v3")
    out = []
    for f in sorted(os.listdir(data_dir)):
        if not f.endswith(".jsonl") or f.startswith("heldout-") != heldout:
            continue
        name = f[len("heldout-"):-6] if heldout else f[:-6]
        if names and name not in names:
            continue
        for line in open(os.path.join(data_dir, f)):
            r = json.loads(line)
            if split and r["split"] != split and not heldout:
                continue
            if with_pos_only and not r["has_pos"]:
                continue
            out.append(r)
    return out


def question(lst):
    return common.QUESTIONS[0].format(task=lst["focus"])


def list_state_ids(tok, lst, cap=common.STATE_TOKENS):
    """Per candidate, the first `cap` state tokens (`SequenceBuilder::encode_state(state, Some(cap))`) as int32
    arrays: tokenized once, then sliced to any window <= cap (a prefix of the cap is the window's own encoding)."""
    out = StateIds(cap)
    for c in lst["candidates"]:
        ids = common.state_ids(tok, common.render_state(c["path"], c["start"], c["end"], c["text"]), budget=cap)
        out.append(np.asarray(ids, np.int32))
    return out


class StateIds(list):
    """list_state_ids output: one int32 array per candidate, plus the cap they were cut to."""

    def __init__(self, cap):
        super().__init__()
        self.cap = cap


def build_list(tok, lst, sids, window=common.STATE_TOKENS, max_len=common.MAX_LEN):
    """[(ids, markers)] for every candidate at a `window`-token state and the model's `max_len`, exactly as the
    Rust scorer builds them with LAYA_CODEX_STATE_TOKENS=window. `sids` comes from list_state_ids."""
    if window > sids.cap:
        raise ValueError("window %d is beyond the %d state tokens kept" % (window, sids.cap))
    q = question(lst)
    return [common.encode_ids(tok, q, a[:window].tolist(), max_len=max_len) for a in sids]


def encode_list(tok, lst, window=common.STATE_TOKENS, max_len=common.MAX_LEN):
    """[(ids, markers)] for every candidate of a list, exactly as the Rust scorer builds them."""
    return build_list(tok, lst, list_state_ids(tok, lst, cap=window), window=window, max_len=max_len)


def blend_order(p, w=W, score_top=SCORE_TOP):
    """Candidate indices in final order (retriever.rs laya_gate with weighted_scores)."""
    n_all = len(p)
    reached = n_all if score_top == 0 else min(score_top, n_all)
    n = max(reached, 1)
    score = [(1 - w) * (1 - r / n) + w * float(p[r]) for r in range(reached)]
    head = sorted(range(reached), key=lambda r: -score[r])  # sorted() is stable: ties keep lexical order
    return head + list(range(reached, n_all))


def file_order(cands, order):
    seen = []
    for i in order:
        if cands[i]["path"] not in seen:
            seen.append(cands[i]["path"])
    return seen


def list_metrics(lst, order):
    cands = lst["candidates"]
    gold = set(lst["touched_files"]) & {c["path"] for c in cands}
    files = file_order(cands, order)
    m = {}
    for k in KS:
        m["file_r@%d" % k] = len(gold & set(files[:k])) / len(gold) if gold else 0.0
    # gold files among the first k chunks: the replay inlines about two blocks per prompt, so top2_gold is
    # the offline stand-in for "gold inlined"
    for k in (1, 2, 3):
        m["top%d_gold" % k] = len(gold & {cands[i]["path"] for i in order[:k]}) / len(gold) if gold else 0.0
    rr = 0.0
    for pos, i in enumerate(order):
        if cands[i]["label"] > 0:
            rr = 1.0 / (pos + 1)
            break
    m["mrr_pos"] = rr
    return m


def _mean(ms):
    return {k: round(float(np.mean([m[k] for m in ms])), 4) for k in ms[0]} if ms else {}


def summarize(lists, probs, w=W, score_top=SCORE_TOP):
    """Lexical, production blend and model-only ranking metrics over the same lists (probs: calibrated P per list)."""
    lex, blend, model = [], [], []
    for lst, p in zip(lists, probs):
        n = len(lst["candidates"])
        lex.append(list_metrics(lst, list(range(n))))
        blend.append(list_metrics(lst, blend_order(p, w, score_top)))
        model.append(list_metrics(lst, sorted(range(n), key=lambda i: -float(p[i]))))
    return {"n_lists": len(lists), "lexical": _mean(lex), "blend": _mean(blend), "model_only": _mean(model)}


def listwise_loss(margin, y):
    """Softmax cross-entropy of the list's margins (logit true - logit false) against y normalised to a
    distribution. Zero for a list without positives."""
    import torch
    total = y.sum()
    if total <= 0:
        return margin.sum() * 0.0
    return -(y / total * torch.log_softmax(margin, -1)).sum()


def kd_loss(logits2, teacher_logits2, T, list_weight=1.0):
    """Distillation from a teacher's calibrated probabilities over one list. `logits2`: the student's raw noul
    logits [n, 2]; `teacher_logits2`: the teacher's raw logits [n, 2] and its noul temperature T.

    pointwise: cross-entropy of the student's noul distribution against the teacher's P = sigmoid(margin_t / T)
    (what the production blend consumes); listwise: cross-entropy of the student's list softmax of margins
    against softmax(margin_t / T) (the teacher's ranking). Both are minimal at margin_s = margin_t / T."""
    import torch
    t = torch.as_tensor(teacher_logits2, dtype=torch.float32, device=logits2.device)
    mt = (t[:, 1] - t[:, 0]) / T
    pt = torch.sigmoid(mt)
    loss = -(torch.stack([1 - pt, pt], -1) * torch.log_softmax(logits2, -1)).sum(-1).mean()
    if list_weight:
        loss = loss + list_weight * listwise_loss(logits2[:, 1] - logits2[:, 0], torch.softmax(mt, -1))
    return loss
