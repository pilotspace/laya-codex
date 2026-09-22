import os
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

import common  # noqa: E402

BASE = os.path.expanduser("~/.cache/laya-codex/models/laya-base")


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


@pytest.fixture(scope="module")
def tok():
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
