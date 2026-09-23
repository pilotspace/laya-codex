"""Learnability reference for the scope task: multinomial logistic regression on bag-of-words (+ repo-free) features,
trained on scope/train.jsonl, evaluated on the held-out sets. Pure numpy (full-batch gradient descent, L2).

    python3 finetune/scope_text_baseline.py
"""
import json
import os
import re
import sys
from collections import Counter

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import scope  # noqa: E402
from scope_eval import DATA, K, load, metrics  # noqa: E402


def toks(t):
    w = re.findall(r"[a-z][a-z0-9]+", t.lower())
    return w + ["%s_%s" % (a, b) for a, b in zip(w, w[1:])] + ["len%d" % min(len(w) // 5, 6)]


def featurize(rows, vocab):
    X = np.zeros((len(rows), len(vocab) + 1), np.float32)
    for i, r in enumerate(rows):
        for t in set(toks(r["task"])):
            j = vocab.get(t)
            if j is not None:
                X[i, j] = 1.0
        X[i, -1] = 1.0
    return X


def fit(X, y, l2=1e-3, lr=0.5, iters=400, weights=None):
    W = np.zeros((X.shape[1], K), np.float32)
    Y = np.eye(K)[y]
    sw = np.ones(len(y)) if weights is None else weights
    for _ in range(iters):
        z = X @ W
        z -= z.max(1, keepdims=True)
        p = np.exp(z)
        p /= p.sum(1, keepdims=True)
        g = X.T @ ((p - Y) * sw[:, None]) / sw.sum() + l2 * W
        W -= lr * g
    return W


def predict(X, W):
    z = X @ W
    z -= z.max(1, keepdims=True)
    p = np.exp(z)
    return p / p.sum(1, keepdims=True)


def main():
    tr = load(os.path.join(DATA, "train.jsonl"))
    df = Counter(t for r in tr for t in set(toks(r["task"])))
    vocab = {t: i for i, t in enumerate(sorted(t for t, c in df.items() if c >= 3))}
    X = featurize(tr, vocab)
    y = np.array([scope.CLASSES.index(r["label"]) for r in tr])
    out = {}
    for mode in ("natural", "class_balanced"):
        w = None
        if mode == "class_balanced":
            cnt = np.bincount(y, minlength=K).astype(float)
            w = (1.0 / cnt[y]) * len(y) / K
        W = fit(X, y, weights=w)
        for s in ("val", "heldout_moon", "heldout_pilot-space"):
            rows = load(os.path.join(DATA, s + ".jsonl"))
            m = metrics(predict(featurize(rows, vocab), W), rows)
            out.setdefault(mode, {})[s] = {k: m[k] for k in ("accuracy", "macro_f1", "nll", "spearman_expected_idx_vs_n_src")}
            print("%-15s %-20s acc %.3f macroF1 %.3f nll %.3f rho %.3f" % (mode, s, m["accuracy"], m["macro_f1"], m["nll"],
                                                                          m["spearman_expected_idx_vs_n_src"]))
    json.dump({"vocab": len(vocab), "results": out}, open(os.path.join(DATA, "text_baseline.json"), "w"), indent=1)


if __name__ == "__main__":
    main()
