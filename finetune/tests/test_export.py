import os
import sys

import pytest
import torch

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import export  # noqa: E402


def test_merge_keeps_keys_shapes_dtypes_and_replaces_values():
    base = {"a.weight": torch.zeros(2, 3, dtype=torch.float16), "temperature": torch.ones(3, dtype=torch.float32),
            "b.bias": torch.zeros(4, dtype=torch.float16)}
    trained = {"a.weight": torch.full((2, 3), 0.5, dtype=torch.float32)}
    out = export.merge_state(base, trained)
    assert list(out) == list(base)
    for k in base:
        assert out[k].dtype == base[k].dtype and out[k].shape == base[k].shape
    assert torch.all(out["a.weight"] == 0.5)
    assert torch.all(out["b.bias"] == 0)


def test_merge_rejects_unknown_or_misshaped_keys():
    base = {"a.weight": torch.zeros(2, 3, dtype=torch.float16)}
    with pytest.raises(KeyError):
        export.merge_state(base, {"zzz": torch.zeros(1)})
    with pytest.raises(ValueError):
        export.merge_state(base, {"a.weight": torch.zeros(3, 2)})


def test_merge_rejects_non_finite_or_fp16_overflow():
    base = {"a.weight": torch.zeros(2, dtype=torch.float16)}
    with pytest.raises(ValueError):
        export.merge_state(base, {"a.weight": torch.tensor([1.0, float("nan")])})
    with pytest.raises(ValueError):
        export.merge_state(base, {"a.weight": torch.tensor([1.0, 1e6])})
