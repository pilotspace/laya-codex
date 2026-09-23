"""Scope `choice` evaluation for any model dir(s) x wording(s) on the held-out scope sets.

Metrics: accuracy, macro-F1, per-class P/R/F1, confusion (rows = true, cols = predicted), NLL, top-label ECE,
Brier, mean predicted class index (E[idx] under p) vs true source-file count (Spearman + mean n_src per predicted
class), and trivial baselines (majority class, train prior).

    python3 finetune/scope_eval.py --models laya-base=<dir> laya-typed=<dir> laya-code=<dir> --out spike/results/scope_eval.json
"""
import argparse
import json
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import common  # noqa: E402
import scope  # noqa: E402

DATA = os.path.join(common.WORK, "scope")
K = len(scope.CLASSES)


def load(path):
    return [json.loads(l) for l in open(path)]


def logits_for(agent, rows, w, bs=32):
    """Raw choice logits [n, 4] (option order = scope.CLASSES) through the reference build_sequence of the model dir."""
    import torch
    from rl_common import QTYPES, build_sequence, collate_items
    out = []
    for s in range(0, len(rows), bs):
        items = []
        for r in rows[s:s + bs]:
            q = scope.question(w, r["task"])
            ids, markers = build_sequence(agent.tok, scope.state_text(w, r["task"], r["repo"]), q, agent.cfg["max_len"],
                                          agent.cfg["head_max_len"])
            assert len(markers) == K
            items.append({"ids": ids, "markers": markers, "qtype": QTYPES["choice"], "target": [0.0] * K, "label": -1,
                          "episode": 0, "ep_step": 0, "ep_len": 1, "src": "scope"})
        b = collate_items([items], agent.tok.pad_token_id)
        d = agent.device
        with torch.no_grad():
            lg, _ = agent.model(b["input_ids"].to(d), b["attention_mask"].to(d), b["marker_pos"].to(d),
                                b["marker_mask"].to(d), b["qtype"].to(d))
        out.append(lg.float().cpu().numpy()[:, :K])
    return np.concatenate(out)


def softmax(z, T):
    z = z / T
    z = z - z.max(1, keepdims=True)
    e = np.exp(z)
    return e / e.sum(1, keepdims=True)


def spearman(a, b):
    from rl_common import spearman as sp
    return sp(np.asarray(a, float), np.asarray(b, float))


def metrics(p, rows):
    y = np.array([scope.CLASSES.index(r["label"]) for r in rows])
    pred = p.argmax(1)
    conf = np.zeros((K, K), int)
    for t, q in zip(y, pred):
        conf[t, q] += 1
    per = {}
    f1s = []
    for c in range(K):
        tp, fp, fn = conf[c, c], conf[:, c].sum() - conf[c, c], conf[c, :].sum() - conf[c, c]
        pr = tp / (tp + fp) if tp + fp else 0.0
        rc = tp / (tp + fn) if tp + fn else 0.0
        f1 = 2 * pr * rc / (pr + rc) if pr + rc else 0.0
        f1s.append(f1)
        per[scope.CLASSES[c]] = {"precision": round(pr, 4), "recall": round(rc, 4), "f1": round(f1, 4), "support": int(conf[c].sum()),
                                 "n_pred": int(conf[:, c].sum())}
    top = p.max(1)
    correct = (pred == y).astype(float)
    edges = np.linspace(0, 1, 11)
    ece = sum(abs(top[s].mean() - correct[s].mean()) * s.mean() for lo, hi in zip(edges[:-1], edges[1:])
              for s in [(top > lo) & (top <= hi)] if s.any())
    onehot = np.eye(K)[y]
    n_src = np.array([r["n_src"] for r in rows])
    eidx = (p * np.arange(K)).sum(1)
    return {"n": int(len(y)), "accuracy": round(float(correct.mean()), 4), "macro_f1": round(float(np.mean(f1s)), 4),
            "nll": round(float(-np.log(np.clip(p[np.arange(len(y)), y], 1e-12, 1)).mean()), 4),
            "ece_top1": round(float(ece), 4), "brier": round(float(((p - onehot) ** 2).sum(1).mean()), 4),
            "mean_p_per_class": dict(zip(scope.CLASSES, np.round(p.mean(0), 4).tolist())),
            "per_class": per, "confusion_rows_true_cols_pred": conf.tolist(),
            "spearman_expected_idx_vs_n_src": round(spearman(eidx, n_src), 4),
            "spearman_true_idx_vs_n_src": round(spearman(y, n_src), 4),
            "mean_n_src_by_pred": {scope.CLASSES[c]: (round(float(n_src[pred == c].mean()), 2) if (pred == c).any() else None)
                                   for c in range(K)},
            "median_n_src_by_pred": {scope.CLASSES[c]: (float(np.median(n_src[pred == c])) if (pred == c).any() else None)
                                     for c in range(K)}}


def baselines(rows, train_prior):
    y = np.array([scope.CLASSES.index(r["label"]) for r in rows])
    maj = np.zeros((len(y), K))
    maj[:, int(np.argmax(train_prior))] = 1.0
    prior = np.tile(train_prior, (len(y), 1))
    return {"majority_cross": metrics(np.clip(maj, 1e-6, 1), rows)["macro_f1"], "train_prior_nll": metrics(prior, rows)["nll"],
            "majority_accuracy": round(float((y == int(np.argmax(train_prior))).mean()), 4)}


def load_agent(model_dir, device):
    import importlib
    sys.path.insert(0, model_dir)
    for m in ("rl_agent_api", "rl_common"):
        sys.modules.pop(m, None)
    api = importlib.import_module("rl_agent_api")
    agent = api.RLAgent(model_dir, device=device)
    sys.path.remove(model_dir)
    return agent


def choice_T(agent):
    from rl_common import temp_bucket
    return agent.temperature_by_options.get(temp_bucket(0, K), agent.temperature[0])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--models", nargs="+", required=True)
    ap.add_argument("--wordings", nargs="+", default=list(scope.WORDINGS))
    ap.add_argument("--sets", nargs="+", default=["heldout_moon", "heldout_pilot-space"])
    ap.add_argument("--device", default=None)
    ap.add_argument("--section", default="zero_shot")
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    import torch
    device = args.device or ("mps" if torch.backends.mps.is_available() else "cpu")
    stats = json.load(open(os.path.join(DATA, "stats.json")))
    tr = stats["balance"]["train"]
    prior = np.array([tr[c] for c in scope.CLASSES], float)
    prior /= prior.sum()
    sets = {s: load(os.path.join(DATA, s + ".jsonl")) for s in args.sets}
    report = {"classes": scope.CLASSES, "wordings": scope.WORDINGS, "balance": stats["balance"],
              "baselines": {s: baselines(r, prior) for s, r in sets.items()}, args.section: {}}
    for spec in args.models:
        name, d = spec.split("=", 1)
        agent = load_agent(os.path.expanduser(d), device)
        T = choice_T(agent)
        for wn in args.wordings:
            w = scope.WORDINGS[wn]
            for sn, rows in sets.items():
                z = logits_for(agent, rows, w)
                m = metrics(softmax(z, T), rows)
                m["T_choice"] = round(float(T), 4)
                report[args.section].setdefault(name, {}).setdefault(wn, {})[sn] = m
                print("%-12s %-10s %-20s acc %.3f macroF1 %.3f nll %.3f ece %.3f rho(E[idx],n_src) %.3f pred %s" % (
                    name, wn, sn, m["accuracy"], m["macro_f1"], m["nll"], m["ece_top1"],
                    m["spearman_expected_idx_vs_n_src"], [m["per_class"][c]["n_pred"] for c in scope.CLASSES]), flush=True)
        del agent
        if device == "mps":
            torch.mps.empty_cache()
    if args.out:
        prev = json.load(open(args.out)) if os.path.exists(args.out) else {}
        for k, v in report.items():
            if k in (args.section,) and k in prev:
                prev[k].update(v)
            else:
                prev[k] = v
        json.dump(prev, open(args.out, "w"), indent=1)
        print("wrote", args.out)


if __name__ == "__main__":
    main()
