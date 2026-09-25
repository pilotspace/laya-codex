import os
import subprocess
import sys

import pytest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import leakage  # noqa: E402

SRC = "def handler(request):\n" + "".join("    value_%d = request.get(%d)\n" % (i, i) for i in range(40))


def git(repo, *args):
    env = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@t", GIT_COMMITTER_NAME="t",
               GIT_COMMITTER_EMAIL="t@t", GIT_AUTHOR_DATE="2020-01-01T00:00:00", GIT_COMMITTER_DATE="2020-01-01T00:00:00")
    return subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True, text=True, env=env).stdout


def make_repo(path, files, msg="init"):
    path.mkdir(parents=True)
    git(path, "init", "-q", "-b", "main")
    for name, text in files.items():
        f = path / name
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text(text)
    git(path, "add", "-A")
    git(path, "commit", "-q", "-m", msg)
    return str(path)


def test_independent_repos_pass(tmp_path):
    train = {"a": make_repo(tmp_path / "a", {"a.py": SRC.replace("handler", "alpha")}, "a")}
    held = {"h": make_repo(tmp_path / "h", {"h.py": SRC.replace("handler", "omega")}, "h")}
    report = leakage.check_repos(train, held)
    assert report["ok"], report


def test_clone_of_heldout_is_rejected(tmp_path):
    held = {"h": make_repo(tmp_path / "h", {"h.py": SRC}, "h")}
    subprocess.run(["git", "clone", "-q", held["h"], str(tmp_path / "fork")], check=True)
    report = leakage.check_repos({"fork": str(tmp_path / "fork")}, held)
    assert not report["ok"]
    assert any("shares history" in p for p in report["problems"])


def test_vendored_heldout_source_is_rejected(tmp_path):
    held = {"h": make_repo(tmp_path / "h", {"pkg/core.py": SRC}, "h")}
    train = {"t": make_repo(tmp_path / "t", {"third_party/core.py": SRC, "t.py": "x = 1\n"}, "t")}
    report = leakage.check_repos(train, held)
    assert not report["ok"]
    assert any("vendored" in p for p in report["problems"])


def test_train_inside_heldout_dir_is_rejected(tmp_path):
    held = {"h": make_repo(tmp_path / "h", {"h.py": SRC}, "h")}
    inner = make_repo(tmp_path / "h" / "sub", {"s.py": SRC.replace("handler", "sub")}, "s")
    report = leakage.check_repos({"sub": inner}, held)
    assert not report["ok"]
    assert any("overlaps" in p for p in report["problems"])


def test_two_train_repos_sharing_history_are_rejected(tmp_path):
    a = make_repo(tmp_path / "a", {"a.py": SRC}, "a")
    subprocess.run(["git", "clone", "-q", a, str(tmp_path / "b")], check=True)
    report = leakage.check_repos({"a": a, "b": str(tmp_path / "b")}, {})
    assert not report["ok"]


def test_rows_from_heldout_repo_or_with_benchmark_task_are_rejected():
    bench = ["Fixed `iter_text` adding an empty string", "Allow URLs where username contains '@'."]
    ok = [{"repo": "codex", "sha": "a" * 40, "task": "handle resize in the tui"}]
    assert leakage.check_rows(ok, {"moon", "httpx"}, bench)["ok"]
    bad_repo = [{"repo": "httpx", "sha": "b" * 40, "task": "x"}]
    assert not leakage.check_rows(bad_repo, {"moon", "httpx"}, bench)["ok"]
    bad_task = [{"repo": "codex", "sha": "c" * 40, "task": "fixed `iter_text` adding an empty string."}]
    assert not leakage.check_rows(bad_task, {"moon", "httpx"}, bench)["ok"]


def test_rows_with_a_heldout_commit_are_rejected(tmp_path):
    held = make_repo(tmp_path / "h", {"h.py": SRC}, "h")
    sha = git(held, "rev-parse", "HEAD").strip()
    rows = [{"repo": "codex", "sha": sha, "task": "something else entirely"}]
    report = leakage.check_rows(rows, {"h"}, [], heldout_paths={"h": held})
    assert not report["ok"]
    assert any("commit" in p for p in report["problems"])


@pytest.mark.parametrize("a,b", [("Fix: the Thing!", "fix the thing"), ("  A  b ", "a b")])
def test_normalize_task(a, b):
    assert leakage.normalize_task(a) == b
