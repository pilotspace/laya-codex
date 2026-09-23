import os
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import calibrate  # noqa: E402


def test_fit_temperature_recovers_known_scale():
    rng = np.random.default_rng(0)
    true_margin = rng.normal(0, 2.0, 20000)             # calibrated logit margin
    y = (rng.random(20000) < 1 / (1 + np.exp(-true_margin))).astype(float)
    T_true = 2.5
    logits = np.stack([np.zeros_like(true_margin), true_margin * T_true], 1)  # model is overconfident by T_true
    T = calibrate.fit_temperature(logits, y)
    assert abs(T - T_true) / T_true < 0.05


def test_fit_temperature_handles_soft_labels_and_bounds():
    logits = np.array([[0.0, 1.0], [0.0, -1.0], [0.0, 0.2]])
    T = calibrate.fit_temperature(logits, np.array([0.4, 0.4, 0.4]))
    assert 0.05 <= T <= 20.0


def test_nll_and_probs():
    p = calibrate.probs(np.array([[0.0, 0.0], [0.0, 2.0]]), 2.0)
    assert np.allclose(p, [0.5, 1 / (1 + np.exp(-1))])
    assert calibrate.nll(np.array([0.5]), np.array([1.0])) == np.log(2)


def test_paired_bootstrap_detects_clear_improvement_and_noise():
    import eval as ev
    a = np.array([1.0, 0.5, 1.0, 0.33, 1.0] * 8)
    r = ev.paired_bootstrap(a, a - 0.2)
    assert abs(r["mean_diff"] - 0.2) < 1e-9 and r["ci95"][0] > 0 and r["p_diff_le_0"] == 0.0
    r2 = ev.paired_bootstrap(a, a[::-1])
    assert r2["ci95"][0] < 0 < r2["ci95"][1]


def test_fit_temperature_multiclass_recovers_scale():
    rng = np.random.default_rng(1)
    z = rng.normal(0, 1.5, (20000, 4))
    p = np.exp(z) / np.exp(z).sum(1, keepdims=True)
    y = np.array([rng.choice(4, p=pp) for pp in p])
    T = calibrate.fit_temperature_k(z * 3.0, y)
    assert abs(T - 3.0) / 3.0 < 0.05
