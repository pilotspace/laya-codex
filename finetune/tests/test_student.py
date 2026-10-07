"""The student init dir: answerdotai/ModernBERT-base (here a tiny ModernBERT) + a random laya decision head, in
the laya-code layout that train.py, export.py and crates/laya-model load."""
import json
import os
import shutil
import sys

import pytest
import torch

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

import common  # noqa: E402

TEMPLATE = common.BASE_MODEL


def tiny_hf(path, hidden=64, layers=3):
    """A ModernBertForMaskedLM checkpoint as the HF hub ships it (model.*, head.*, decoder.bias; F32)."""
    from transformers import ModernBertConfig, ModernBertForMaskedLM
    torch.manual_seed(3)
    cfg = ModernBertConfig(vocab_size=50368, hidden_size=hidden, intermediate_size=2 * hidden, num_hidden_layers=layers,
                           num_attention_heads=2, global_attn_every_n_layers=3, local_attention=16,
                           pad_token_id=50283, bos_token_id=50281, eos_token_id=50282, cls_token_id=50281,
                           sep_token_id=50282)
    ModernBertForMaskedLM(cfg).save_pretrained(path)
    for f in ("tokenizer.json", "tokenizer_config.json"):
        shutil.copy(os.path.join(TEMPLATE, "tokenizer", f), os.path.join(path, f))
    return path


@pytest.fixture(scope="module")
def dirs(tmp_path_factory):
    if not os.path.isfile(os.path.join(TEMPLATE, "rl_common.py")):
        pytest.skip("no laya model dir (set LAYA_CODEX_FT_BASE)")
    import student
    root = tmp_path_factory.mktemp("student")
    hf = tiny_hf(str(root / "hf"))
    out = str(root / "init")
    student.make_student_dir(hf, TEMPLATE, out, max_len=600, seed=5)
    return hf, out


def test_layout_loads_strictly_with_the_reference_model(dirs):
    import train
    hf, out = dirs
    for f in ("encoder/config.json", "tokenizer/tokenizer.json", "rl_common.py", "rl_agent_api.py",
              "rl_agent_config.json", "model.safetensors"):
        assert os.path.isfile(os.path.join(out, f)), f
    model, tok, cfg, _ = train.load_base_model(out, torch.device("cpu"))  # load_state_dict(strict=True)
    assert model.encoder.config.hidden_size == 64 and len(model.encoder.layers) == 3
    assert tok.mask_token_id == 50284


def test_encoder_weights_are_the_pretrained_ones_and_dtypes_match_laya_code(dirs):
    from safetensors import safe_open
    from safetensors.torch import load_file
    hf, out = dirs
    src = load_file(os.path.join(hf, "model.safetensors"))
    f = safe_open(os.path.join(out, "model.safetensors"), "pt")
    keys = list(f.keys())
    enc = [k for k in keys if k.startswith("encoder.")]
    assert len(enc) == len([k for k in src if k.startswith("model.")])
    for k in enc:
        assert torch.equal(f.get_tensor(k), src["model." + k[len("encoder."):]].half()), k
    assert {f.get_slice(k).get_dtype() for k in keys if k != "temperature"} == {"F16"}
    assert f.get_slice("temperature").get_dtype() == "F32"
    assert not any(k.startswith(("head.dense", "decoder")) for k in keys)  # the MLM head is dropped
    # the same non-encoder keys as laya-code (head, type_emb, scorer, act_head, temperature)
    g = safe_open(os.path.join(TEMPLATE, "model.safetensors"), "pt")
    assert sorted(k for k in keys if not k.startswith("encoder.")) == sorted(
        k for k in g.keys() if not k.startswith("encoder."))


def test_agent_config_is_the_student_one(dirs):
    _, out = dirs
    cfg = json.load(open(os.path.join(out, "rl_agent_config.json")))
    base = json.load(open(os.path.join(TEMPLATE, "rl_agent_config.json")))
    assert cfg["max_len"] == 600 and cfg["head_max_len"] == base["head_max_len"] == common.HEAD_MAX_LEN
    assert cfg["encoder"] == "answerdotai/ModernBERT-base"
    # a random head has no fitted temperature yet: calibrate.py writes it after training
    assert cfg["temperature"] == [1.0, 1.0, 1.0] and set(cfg["temperature_by_options"].values()) == {1.0}
    assert "finetune" not in cfg and "training" not in cfg


def test_head_is_seeded(dirs, tmp_path):
    import student
    from safetensors.torch import load_file
    hf, out = dirs
    again = str(tmp_path / "again")
    student.make_student_dir(hf, TEMPLATE, again, max_len=600, seed=5)
    a, b = load_file(os.path.join(out, "model.safetensors")), load_file(os.path.join(again, "model.safetensors"))
    assert all(torch.equal(a[k], b[k]) for k in a)
    other = str(tmp_path / "other")
    student.make_student_dir(hf, TEMPLATE, other, max_len=600, seed=6)
    c = load_file(os.path.join(other, "model.safetensors"))
    assert not torch.equal(a["scorer.1.weight"], c["scorer.1.weight"])
    assert torch.equal(a["encoder.embeddings.tok_embeddings.weight"], c["encoder.embeddings.tok_embeddings.weight"])


def test_refuses_a_max_len_that_cuts_the_window(dirs, tmp_path):
    import student
    hf, _ = dirs
    with pytest.raises(ValueError):
        student.make_student_dir(hf, TEMPLATE, str(tmp_path / "short"), max_len=512, seed=5, window=384)
