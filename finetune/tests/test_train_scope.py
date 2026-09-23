import math
import os
import sys

import torch

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import train_scope  # noqa: E402


def test_mixed_loss_equals_per_type_log_loss():
    # row 0: noul (2 options, padded to 4 and masked like DecisionModel.forward does), row 1: 4-way choice
    logits = torch.tensor([[1.0, -1.0, 0.0, 0.0], [0.5, 0.1, -0.3, 2.0]])
    mask = torch.tensor([[True, True, False, False], [True, True, True, True]])
    logits = logits.masked_fill(~mask, -1e4)
    target = torch.tensor([[0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]])
    loss, per = train_scope.mixed_loss(logits, target)
    l0 = -torch.log_softmax(torch.tensor([1.0, -1.0]), -1)[1]
    l1 = -torch.log_softmax(torch.tensor([0.5, 0.1, -0.3, 2.0]), -1)[3]
    assert torch.allclose(per, torch.stack([l0, l1]), atol=1e-6)
    assert math.isclose(loss.item(), (l0 + l1).item() / 2, rel_tol=1e-6)


def test_tempered_class_weights():
    w = train_scope.class_sampling_weights(["cross"] * 9 + ["function"], alpha=0.5)
    # per-class mass proportional to count**0.5 -> 3:1
    mass_cross = sum(x for x, c in zip(w, ["cross"] * 9 + ["function"]) if c == "cross")
    assert abs(mass_cross / (sum(w) - mass_cross) - 3.0) < 1e-9
