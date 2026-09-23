import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import scope  # noqa: E402

DIFF_ONE_FN = """diff --git a/src/a.rs b/src/a.rs
index 1..2 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -10,3 +10,4 @@ fn x() {
@@ -40,2 +41,2 @@ fn x() {
"""

DIFF_ONE_FILE_SPREAD = """diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -10,3 +10,4 @@
@@ -200,2 +201,2 @@
"""

DIFF_THREE_HUNKS = """diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -10 +10 @@
@@ -20 +20 @@
@@ -30 +30 @@
"""

DIFF_NEW_FILE = """diff --git a/src/new.py b/src/new.py
new file mode 100644
--- /dev/null
+++ b/src/new.py
@@ -0,0 +1,5 @@
"""


def test_parse_hunks():
    f = scope.parse_hunks(DIFF_ONE_FN)
    assert list(f) == ["src/a.rs"]
    assert f["src/a.rs"]["hunks"] == [(10, 3), (40, 2)]
    assert not f["src/a.rs"]["new_file"]
    assert scope.parse_hunks(DIFF_NEW_FILE)["src/new.py"]["new_file"]


def test_function_vs_file():
    assert scope.label_from_diff(DIFF_ONE_FN) == "function"
    assert scope.label_from_diff(DIFF_ONE_FILE_SPREAD) == "file"
    assert scope.label_from_diff(DIFF_THREE_HUNKS) == "file"
    assert scope.label_from_diff(DIFF_NEW_FILE) == "file"


def _files(*paths):
    return "".join("diff --git a/%s b/%s\n--- a/%s\n+++ b/%s\n@@ -1 +1 @@\n" % (p, p, p, p) for p in paths)


def test_module_and_cross():
    assert scope.label_from_diff(_files("codex-rs/core/src/a.rs", "codex-rs/core/src/tools/b.rs")) == "module"
    assert scope.label_from_diff(_files("codex-rs/core/src/a.rs", "codex-rs/tui/src/b.rs")) == "cross"
    assert scope.label_from_diff(_files("a/x.py", "b/y.py", "c/z.py", "a/w.py")) == "cross"
    assert scope.label_from_diff(_files("src/dispatch/incident/a.py", "src/dispatch/incident/b.py", "src/dispatch/incident/c.py")) == "module"


def test_non_source_files_ignored_and_empty():
    d = _files("src/a.rs") + _files("README.md", "Cargo.toml")
    assert scope.label_from_diff(d) in ("function", "file")
    assert scope.label_from_diff(_files("README.md")) is None


def test_module_key():
    assert scope.module_key("codex-rs/core/src/tools/x.rs") == "core"
    assert scope.module_key("src/dispatch/incident/models.py") == "dispatch/incident"
    assert scope.module_key("packages/coding-agent/src/core/x.ts") == "coding-agent"
    assert scope.module_key("src/command/x.rs") == "command"
    assert scope.module_key("main.rs") == "."
    assert scope.module_key("backend/src/pilot_space/api/x.py") == "pilot_space/api"


def test_question_rendering_is_a_4_option_choice():
    for w in scope.WORDINGS.values():
        q = scope.question(w, "fix the thing")
        assert q["t"] == "choice" and list(q["crit"]) == scope.CLASSES
        assert "fix the thing" in q["ins"] or "{task}" not in w["ins"]
