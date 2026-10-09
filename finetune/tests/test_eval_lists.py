import os
import sys

import pytest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import eval_lists  # noqa: E402


def test_model_spec_takes_an_optional_state_window():
    assert eval_lists.parse_model_spec("r1=/m/laya-code-r1") == ("r1", "/m/laya-code-r1", 128)
    assert eval_lists.parse_model_spec("s=/m/student@384") == ("s", "/m/student", 384)
    assert eval_lists.parse_model_spec("s=/m/a@b/student@256") == ("s", "/m/a@b/student", 256)
    with pytest.raises(ValueError):
        eval_lists.parse_model_spec("/m/no-name")
