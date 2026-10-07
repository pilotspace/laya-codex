"""Student training paths: a code window drawn per list, distillation from cached teacher logits, resume and export.

The end-to-end tests run train.py on CPU with a tiny ModernBERT student and a handful of toy lists."""
import json
import os
import random
import sys

import numpy as np
import pytest
import torch

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
sys.path.insert(0, HERE)

import common  # noqa: E402

TEMPLATE = common.BASE_MODEL
needs_model = pytest.mark.skipif(not os.path.isfile(os.path.join(TEMPLATE, "rl_common.py")),
                                 reason="no laya model dir (set LAYA_CODEX_FT_BASE)")


# ----------------------------------------------------------------------------- windows
def test_draw_window_from_a_set_is_seeded_and_covers_the_set():
    import train
    a = [train.draw_window(random.Random(1), windows=[128, 256, 384]) for _ in range(5)]
    b = [train.draw_window(random.Random(1), windows=[128, 256, 384]) for _ in range(5)]
    assert a == b
    rng = random.Random(2)
    seen = [train.draw_window(rng, windows=[128, 256, 384]) for _ in range(300)]
    assert set(seen) == {128, 256, 384}


def test_draw_window_from_a_range_is_uniform_and_inclusive():
    import train
    rng = random.Random(3)
    seen = [train.draw_window(rng, window_range=(128, 384)) for _ in range(3000)]
    assert min(seen) == 128 and max(seen) == 384
    assert abs(np.mean(seen) - 256) < 6


def test_window_settings_check_the_model_max_len():
    import train
    assert train.window_plan([128, 256, 384], None, 704) == ([128, 256, 384], None, 384)
    assert train.window_plan(None, (128, 384), 704) == (None, (128, 384), 384)
    with pytest.raises(ValueError):
        train.window_plan([128, 384], None, 512)  # 384 would be cut by max_len 512
    with pytest.raises(ValueError):
        train.window_plan(None, (300, 200), 704)


# ----------------------------------------------------------------------------- teacher cache
def lst(repo, sha, n):
    return {"repo": repo, "sha": sha, "candidates": [{"label": 0.0}] * n}


# ----------------------------------------------------------------------------- MPS cache release
# Every new sequence length makes the MPS allocator cache another set of blocks: scoring 150 lists grew the driver
# allocation from 2 GB to 22.5 GB with 1.6 GB live (and swapped). Releasing the cache every 10 batches held it at
# <= 3.7 GB. predict_lists and the training loop must release it periodically.
def test_predict_lists_releases_the_device_cache_periodically(monkeypatch):
    import train
    calls = []
    monkeypatch.setattr(train, "release_cache", lambda device: calls.append(device.type))

    class Tiny(torch.nn.Module):
        def forward(self, ids, att, mpos, mmask, qtype):
            return torch.zeros(ids.shape[0], 2), None

    enc = [([([1, 2, 3 + i % 7], [0, 1])] * 4, np.zeros(4, np.float32)) for i in range(30)]  # 120 sequences
    out = train.predict_lists(Tiny(), enc, torch.device("cpu"), 0, bs=4, release_every=10)
    assert len(out) == 30 and len(calls) == 120 // 4 // 10 + 1  # every 10 batches, and once at the end


def test_release_cache_is_a_no_op_off_mps():
    import train
    train.release_cache(torch.device("cpu"))


def test_teacher_cache_round_trip_and_lookup(tmp_path):
    import teacher
    logits = {"r:a": np.arange(6, dtype=np.float32).reshape(3, 2), "r:b": np.ones((2, 2), np.float32)}
    meta = {"teacher": "/m", "model.safetensors_sha256": "abc", "window": 128, "T": 0.8}
    p = str(tmp_path / "t.npz")
    teacher.save(p, logits, meta)
    cache = teacher.load(p)
    assert cache.meta == meta and len(cache) == 2
    assert np.array_equal(cache.lookup(lst("r", "a", 3)), logits["r:a"])
    with pytest.raises(KeyError):
        cache.lookup(lst("r", "zzz", 3))
    with pytest.raises(ValueError):
        cache.lookup(lst("r", "b", 3))  # the teacher scored another candidate list
    cache.check([lst("r", "a", 3), lst("r", "b", 2)])
    with pytest.raises(KeyError):
        cache.check([lst("r", "a", 3), lst("q", "a", 3)])


# ----------------------------------------------------------------------------- end to end (tiny, CPU)
WORDS = ["cache", "evict", "parse", "header", "socket", "retry", "token", "stream", "config", "buffer"]


def toy_lists(rng, n, split, repo="toy"):
    out = []
    for i in range(n):
        gold = WORDS[i % len(WORDS)]
        cands = []
        for j in range(5):
            w = gold if j == i % 5 else WORDS[(i + j + 3) % len(WORDS)]
            body = "\n".join("def %s_%d(x):\n    return x + %d" % (w, k, rng.randint(0, 99)) for k in range(30))
            cands.append({"path": "src/%s_%d.py" % (w, j), "start": 1, "end": 60, "text": body,
                          "label": 1.0 if w == gold else 0.0})
        out.append({"repo": repo, "sha": "%s%04d" % (split, i), "split": split, "has_pos": True, "focus": "fix %s" % gold,
                    "task": "fix %s" % gold, "touched_files": [c["path"] for c in cands if c["label"] > 0],
                    "candidates": cands})
    return out


@pytest.fixture(scope="module")
def setup(tmp_path_factory):
    if not os.path.isfile(os.path.join(TEMPLATE, "rl_common.py")):
        pytest.skip("no laya model dir (set LAYA_CODEX_FT_BASE)")
    import student
    from test_student import tiny_hf
    root = tmp_path_factory.mktemp("e2e")
    hf = tiny_hf(str(root / "hf"))
    init = str(root / "init")
    student.make_student_dir(hf, TEMPLATE, init, max_len=common.required_max_len(64), seed=1, window=64)
    data = root / "data"
    data.mkdir()
    rng = random.Random(0)
    with open(data / "toy.jsonl", "w") as f:
        for r in toy_lists(rng, 6, "train") + toy_lists(rng, 4, "val"):
            f.write(json.dumps(r) + "\n")
    return root, init, str(data)


def run_train(argv):
    import train
    train.main(argv)


@needs_model
def test_teacher_cache_is_built_from_the_teacher_at_its_window(setup):
    import listwise
    import teacher
    import train
    root, init, data = setup
    out = str(root / "teacher.npz")
    teacher.main(["--teacher", init, "--data", data, "--out", out, "--window", "16", "--device", "cpu"])
    cache = teacher.load(out)
    lists = train.training_lists(data)
    assert len(cache) == len(lists) == 6 and cache.meta["window"] == 16 and cache.meta["T"] == 1.0
    model, tok, cfg, _ = train.load_base_model(init, torch.device("cpu"))
    enc = train.encode_lists(tok, lists[:1], window=16, max_len=cfg["max_len"])
    z = train.predict_lists(model, enc, torch.device("cpu"), tok.pad_token_id)[0]
    assert np.allclose(cache.lookup(lists[0]), z, atol=1e-5)
    assert listwise.SCORE_TOP >= 5


@needs_model
def test_train_with_windows_and_distillation_then_resume_and_export(setup):
    import export
    root, init, data = setup
    tc = str(root / "teacher.npz")
    if not os.path.exists(tc):
        import teacher
        teacher.main(["--teacher", init, "--data", data, "--out", tc, "--window", "16", "--device", "cpu"])
    ck = str(root / "ckpt")
    common_args = ["--base", init, "--data", data, "--ckpt-dir", ck, "--device", "cpu", "--top-layers", "3",
                   "--lists-per-step", "2", "--windows", "16", "32", "64", "--val-windows", "32", "64",
                   "--kd-weight", "0.5", "--teacher-cache", tc, "--eval-every", "2", "--ckpt-every", "1",
                   "--val-n", "4", "--warmup", "1", "--grad-ckpt"]
    run_train(common_args + ["--steps", "3"])
    last = torch.load(os.path.join(ck, "last.pt"), map_location="cpu", weights_only=False)
    assert last["step"] == 3
    assert set(last["windows_seen"]) <= {16, 32, 64} and sum(last["windows_seen"].values()) == 6
    log = json.load(open(os.path.join(ck, "train_log.json")))
    assert set(log["log"][0]["val"]) == {"32", "64"}  # validation at every --val-windows, step 0 included
    assert log["log"][0]["val"]["64"]["n_lists"] == 4
    assert log["kd"]["weight"] == 0.5 and log["kd"]["teacher_window"] == 16
    assert os.path.exists(os.path.join(ck, "best.pt"))
    # resume continues the same run (windows drawn after the checkpoint come from the restored rng)
    run_train(common_args + ["--steps", "4", "--resume"])
    last2 = torch.load(os.path.join(ck, "last.pt"), map_location="cpu", weights_only=False)
    assert last2["step"] == 4 and sum(last2["windows_seen"].values()) == 8
    # export keeps the student's layout (keys, shapes, dtypes) and its max_len
    out = str(root / "exported")
    export.main(["--base", init, "--ckpt", os.path.join(ck, "best.pt"), "--out", out])
    n, dtypes = export.check_same_layout(init, out)
    assert dtypes == ["F16", "F32"]
    assert json.load(open(os.path.join(out, "rl_agent_config.json")))["max_len"] == common.required_max_len(64)


@needs_model
def test_distillation_requires_a_teacher_entry_for_every_training_list(setup, tmp_path):
    import teacher
    root, init, data = setup
    bad = str(tmp_path / "partial.npz")
    teacher.save(bad, {"toy:train0000": np.zeros((5, 2), np.float32)}, {"window": 16, "T": 1.0})
    with pytest.raises(KeyError):
        run_train(["--base", init, "--data", data, "--ckpt-dir", str(tmp_path / "ck"), "--device", "cpu",
                   "--steps", "1", "--windows", "16", "--val-windows", "16", "--kd-weight", "1",
                   "--teacher-cache", bad, "--top-layers", "1"])


@needs_model
def test_windows_beyond_the_model_max_len_are_refused(setup, tmp_path):
    root, init, data = setup
    with pytest.raises(ValueError):
        run_train(["--base", init, "--data", data, "--ckpt-dir", str(tmp_path / "ck"), "--device", "cpu",
                   "--steps", "1", "--windows", "128", "--val-windows", "16", "--top-layers", "1"])
