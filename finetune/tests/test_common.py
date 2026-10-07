import os
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

import common  # noqa: E402

BASE = common.BASE_MODEL


def test_parse_diff_old_ranges_modify_insert_delete():
    diff = """diff --git a/src/a.rs b/src/a.rs
index 111..222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -10,3 +10,4 @@ fn x() {
@@ -50,0 +52,2 @@ fn y() {
@@ -80 +83 @@
diff --git a/new.py b/new.py
new file mode 100644
--- /dev/null
+++ b/new.py
@@ -0,0 +1,5 @@
diff --git a/old.go b/renamed.go
similarity index 90%
rename from old.go
rename to renamed.go
--- a/old.go
+++ b/renamed.go
@@ -5,2 +5,2 @@
"""
    files = common.parse_diff_u0(diff)
    assert files["src/a.rs"]["old_lines"] == {10, 11, 12, 50, 51, 80}
    assert files["src/a.rs"]["new_file"] is False
    assert files["new.py"]["new_file"] is True
    assert files["new.py"]["old_lines"] == set()
    assert files["old.go"]["old_lines"] == {5, 6}
    assert files["old.go"]["new_path"] == "renamed.go"


def test_windows_match_spike_corpus_layout():
    lines = ["l%d" % i for i in range(1, 101)]
    w = common.windows(lines)
    assert [(s, e) for s, e, _ in w] == [(1, 40), (31, 70), (61, 100)]
    assert common.windows(["a"] * 10) == [(1, 10, "\n".join(["a"] * 10))]
    assert common.windows([]) == []


def test_overlap():
    assert common.overlaps(1, 40, {40})
    assert not common.overlaps(41, 80, {40})
    assert not common.overlaps(1, 40, set())


def test_clean_subject_and_informative():
    assert common.clean_subject("feat(core): add thing (#123)") == "feat(core): add thing"
    assert common.informative("fix(parser): handle nested generics in impl blocks")
    assert not common.informative("Merge branch 'main' into dev")
    assert not common.informative("bump version to 1.2.3 for release")
    assert not common.informative("wip")
    assert not common.informative('Revert "feat: add a big feature to the thing"')


def test_task_text_uses_short_first_body_line_only():
    assert common.task_text("fix: handle empty frames in decoder", "Frames of len 0 crashed.\n\nmore") == \
        "fix: handle empty frames in decoder. Frames of len 0 crashed."
    assert common.task_text("fix: handle empty frames in decoder", "Signed-off-by: x") == "fix: handle empty frames in decoder"
    assert common.task_text("fix: handle empty frames in decoder", "x" * 300) == "fix: handle empty frames in decoder"


def _model_dir(path):
    os.makedirs(os.path.join(path, "tokenizer"))
    open(os.path.join(path, "tokenizer", "tokenizer.json"), "w").write("{}")
    open(os.path.join(path, "rl_common.py"), "w").write("")
    return path


def test_find_model_dir_prefers_env_then_base_then_laya_code(tmp_path):
    home = str(tmp_path)
    models = os.path.join(home, ".cache", "laya-codex", "models")
    snap = _model_dir(os.path.join(home, ".cache", "huggingface", "hub", "models--tindang--laya-code", "snapshots", "abc"))
    assert common.find_model_dir({}, home) == snap
    code = _model_dir(os.path.join(models, "laya-code"))
    assert common.find_model_dir({}, home) == code
    base = _model_dir(os.path.join(models, "laya-base"))
    assert common.find_model_dir({}, home) == base
    assert common.find_model_dir({"LAYA_CODEX_FT_BASE": "/x/y"}, home) == "/x/y"


def test_find_model_dir_without_any_model_is_the_laya_base_path(tmp_path):
    home = str(tmp_path)
    assert common.find_model_dir({}, home) == os.path.join(home, ".cache", "laya-codex", "models", "laya-base")


@pytest.fixture(scope="module")
def tok():
    path = os.path.join(BASE, "tokenizer", "tokenizer.json")
    if not os.path.isfile(path):
        pytest.skip("no laya tokenizer (set LAYA_CODEX_FT_BASE, or install laya-base or laya-code)")
    from transformers import AutoTokenizer
    return AutoTokenizer.from_pretrained(os.path.join(BASE, "tokenizer"))


def test_state_truncated_to_budget(tok):
    text = "\n".join("let value_%d = compute_something(%d);" % (i, i) for i in range(200))
    st, visible_end = common.make_state(tok, "src/x.rs", 1, 200, text, budget=256)
    assert st.startswith("file: src/x.rs (lines 1-200)\n")
    n = len(tok(st, add_special_tokens=False)["input_ids"])
    assert 240 <= n <= 256
    assert 1 < visible_end < 200


def test_short_state_not_truncated(tok):
    st, visible_end = common.make_state(tok, "a.py", 3, 5, "x = 1\ny = 2\nz = 3", budget=256)
    assert st == "file: a.py (lines 3-5)\nx = 1\ny = 2\nz = 3"
    assert visible_end == 5


def test_encode_matches_reference_build_sequence(tok):
    sys.path.insert(0, BASE)
    from rl_common import build_sequence
    st, _ = common.make_state(tok, "a.py", 1, 3, "def f():\n    return 1\n", budget=256)
    q = common.QUESTIONS[0].format(task="fix f")
    ids, markers = common.encode(tok, q, st)
    ref_ids, ref_markers = build_sequence(tok, st, {"t": "noul", "ins": q, "crit": None}, 512, 192)
    assert ids == ref_ids and markers == ref_markers
    assert len(markers) == 2


def test_primary_question_is_the_inference_question():
    assert common.QUESTIONS[0] == 'Is this source code relevant to the software change: "{task}"?'
    assert len(common.QUESTIONS) == 3


def test_bm25_score_doc_matches_index_scores():
    sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(HERE)), "spike"))
    from laya_spike import BM25, tokenize
    docs = [tokenize(t) for t in ["fn parse_header(buf) { decode frame }", "struct Cache { lru: Vec<u8> }",
                                  "fn evict_lru(cache) { cache.lru.pop() }", "unrelated text about docs"]]
    bm = BM25(docs)
    q = tokenize("evict lru cache entries")
    for i, s in bm.search(q, 10):
        assert abs(common.bm25_score_doc(bm, q, docs[i]) - s) < 1e-9
    assert common.bm25_score_doc(bm, q, tokenize("nothing matches here")) == 0.0


# ----------------------------------------------------------------------------- production-shaped inputs
def test_render_state_matches_rust_scorer():
    assert common.render_state("src/a.rs", 3, 9, "fn a() {}") == "file: src/a.rs (lines 3-9)\nfn a() {}"


def test_state_ids_truncate_tokens_like_rust(tok):
    st = common.render_state("x.py", 1, 200, "\n".join("value_%d = compute(%d)" % (i, i) for i in range(200)))
    ids = common.state_ids(tok, st)
    assert len(ids) == common.STATE_TOKENS == 128
    assert ids == tok(st, add_special_tokens=False)["input_ids"][:128]
    # [MASK] in code is blanked so it cannot forge an option marker
    assert tok.mask_token_id not in common.state_ids(tok, "a [MASK] b")


def test_encode_ids_matches_reference_when_state_fits(tok):
    sys.path.insert(0, BASE)
    from rl_common import build_sequence
    st = common.render_state("a.py", 1, 3, "def f():\n    return 1\n")
    q = common.QUESTIONS[0].format(task="fix f")
    ids, markers = common.encode_ids(tok, q, common.state_ids(tok, st))
    ref_ids, ref_markers = build_sequence(tok, st, {"t": "noul", "ins": q, "crit": None}, 512, 192)
    assert (ids, markers) == (ref_ids, ref_markers)


def test_encode_ids_keeps_only_the_state_budget(tok):
    st = common.render_state("x.py", 1, 200, "\n".join("value_%d = compute(%d)" % (i, i) for i in range(200)))
    q = common.QUESTIONS[0].format(task="fix compute")
    ids, markers = common.encode_ids(tok, q, common.state_ids(tok, st))
    head = common.encode_ids(tok, q, [])[0]
    assert len(ids) == len(head) + 128 and len(markers) == 2


# ----------------------------------------------------------------------------- longer windows (student)
def _long_state():
    return common.render_state("x.py", 1, 400, "\n".join("value_%d = compute(%d)" % (i, i) for i in range(400)))


def test_encode_ids_honours_the_model_max_len(tok):
    # a long focus fills the head; at max_len 512 a 384-token window is cut, at 704 it is not
    q = common.QUESTIONS[0].format(task=" ".join("word%d" % i for i in range(60)))
    sids = common.state_ids(tok, _long_state(), budget=384)
    head = common.encode_ids(tok, q, [])[0]  # ends with the state's closing [SEP]
    cut, _ = common.encode_ids(tok, q, sids)
    assert len(cut) == 512 and len(cut) - len(head) < 384
    ids, markers = common.encode_ids(tok, q, sids, max_len=704)
    assert ids == head[:-1] + list(sids) + [tok.sep_token_id] and len(markers) == 2


def test_encode_ids_matches_reference_at_a_longer_max_len(tok):
    sys.path.insert(0, BASE)
    from rl_common import build_sequence
    st = _long_state()
    q = common.QUESTIONS[0].format(task="fix compute")
    ids, markers = common.encode_ids(tok, q, common.state_ids(tok, st, budget=10_000), max_len=704)
    ref_ids, ref_markers = build_sequence(tok, st, {"t": "noul", "ins": q, "crit": None}, 704, 192)
    assert (ids, markers) == (ref_ids, ref_markers)
    assert len(ids) == 704


def test_required_max_len_never_cuts_the_window(tok):
    # the head is at most HEAD_MAX_LEN tokens of question + options, plus [CLS] and two [SEP]; the state
    # then needs its own closing [SEP]
    assert common.required_max_len(384) == common.HEAD_MAX_LEN + 3 + 384 + 1 == 580
    q = common.QUESTIONS[0].format(task=" ".join("word%d" % i for i in range(400)))
    head = common.encode_ids(tok, q, [], max_len=10_000)[0]
    assert len(head) == common.HEAD_MAX_LEN + 3 + 1
    for w in (128, 256, 384):
        sids = common.state_ids(tok, _long_state(), budget=w)
        ids, markers = common.encode_ids(tok, q, sids, max_len=common.required_max_len(w))
        assert ids[-w - 1:-1] == list(sids) and len(markers) == 2


def test_is_fix_commit():
    assert common.is_fix("fix(tui): handle resize of the viewport", "")
    assert common.is_fix("Handle empty frames in the decoder", "Fixes #123")
    assert common.is_fix("Resolve crash when config is missing", "")
    assert not common.is_fix("feat: add a new provider for gemini", "Adds the provider.")
    assert not common.is_fix("docs: prefix for the readme", "")  # 'prefix' is not 'fix'


def test_commit_task_strips_conventional_prefix_and_trailers():
    assert common.commit_task("fix(tui): handle resize of the viewport (#412)", "", False) == \
        "tui: handle resize of the viewport"
    assert common.commit_task("fix: handle empty frames in decoder", "", False) == "handle empty frames in decoder"
    assert common.commit_task("fix(16): prevent agent teleportation", "", False) == "prevent agent teleportation"
    body ="Frames of length 0 crashed the reader.\n\nSigned-off-by: x <x@y>\n- [x] tests"
    assert common.commit_task("fix: handle empty frames in decoder", body, True) == \
        "handle empty frames in decoder. Frames of length 0 crashed the reader."
    long_body = "word " * 200
    assert len(common.commit_task("fix: handle empty frames in decoder", long_body, True)) <= 400


def test_label_candidates_hunk_file_other():
    diff = {"src/a.rs": {"new_path": "src/a.rs", "new_file": False, "old_lines": {15}}}
    cands = [{"path": "src/a.rs", "start": 10, "end": 20}, {"path": "src/a.rs", "start": 30, "end": 40},
             {"path": "src/b.rs", "start": 10, "end": 20}]
    labels = common.label_candidates(cands, diff)
    assert labels == [(1.0, "hunk"), (common.FILE_SOFT, "file"), (0.0, "other")]


def test_label_candidates_uses_old_path_of_a_rename():
    diff = {"old.go": {"new_path": "new.go", "new_file": False, "old_lines": {5}}}
    labels = common.label_candidates([{"path": "old.go", "start": 1, "end": 9}], diff)
    assert labels == [(1.0, "hunk")]
