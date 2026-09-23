"""Task-scope labels from git history + the scope `choice` question (wordings the Rust side can copy verbatim).

Classes (from the source files a commit touches; non-source files ignored):
  function: 1 existing source file, <= 2 hunks, all within a 60-line old-side span (proxy for "one enclosing function")
  file:     1 source file otherwise (incl. new files)
  module:   2-3 source files with the same module key
  cross:    >= 4 source files, or 2-3 spanning different module keys

Module key (deviation from a literal top-level directory, which puts ~everything in one "module" because almost every
repo nests code under a container dir: codex-rs/, crates/, packages/, apps/, src/, frontend/, backend/): drop leading
container dirs, then keep up to 2 dir components, stopping at src/lib/tests/test.
"""
import re

import common

CLASSES = ["function", "file", "module", "cross"]
CONTAINERS = {"src", "crates", "packages", "apps", "libs", "lib", "pkg", "internal", "codex-rs", "frontend", "backend",
              "source", "sources"}
STOP = {"src", "lib", "tests", "test", "__tests__", "spec"}
FN_SPAN, FN_HUNKS = 60, 2

_HUNK = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+\d+(?:,\d+)? @@")

WORDINGS = {
    # w1: coordinator's draft; task in the instructions, task + repo hint as state
    "w1_amount": {
        "ins": 'How much of the codebase must be read or changed to complete this task: "{task}"?',
        "crit": {"function": "a single function or method", "file": "one file",
                 "module": "a few related files in one module", "cross": "many files across modules"},
        "state": "task: {task}\nrepository: {repo}"},
    # w2: concrete file counts in the options (they mirror the labelling rule)
    "w2_files": {
        "ins": "How many source files will a developer modify to implement this change request?",
        "crit": {"function": "one function in one file", "file": "one file, in several places",
                 "module": "two or three files in the same directory", "cross": "four or more files, or files in different directories"},
        "state": "change request: {task}\nrepository: {repo}"},
    # w3: scope phrasing, no repo hint
    "w3_scope": {
        "ins": 'What is the scope of the code change needed for: "{task}"?',
        "crit": {"function": "the edit stays inside one function or method", "file": "the edit is confined to one file",
                 "module": "a few related files in one module", "cross": "many files across several modules"},
        "state": "{task}"},
}


def question(w, task):
    return {"t": "choice", "ins": w["ins"].format(task=task), "crit": dict(w["crit"])}


def state_text(w, task, repo):
    return w["state"].format(task=task, repo=repo)


def parse_hunks(diff):
    """`git diff -U0 -M` -> {old_path: {"new_path", "new_file", "deleted", "hunks": [(old_start, old_len)]}}."""
    files, cur = {}, None
    for line in diff.splitlines():
        if line.startswith("diff --git "):
            m = re.match(r"diff --git a/(.*) b/(.*)$", line)
            cur = {"new_path": m.group(2), "new_file": False, "deleted": False, "hunks": []}
            files[m.group(1)] = cur
        elif cur is None:
            continue
        elif line.startswith("new file mode") or line == "--- /dev/null":
            cur["new_file"] = True
        elif line.startswith("deleted file mode") or line == "+++ /dev/null":
            cur["deleted"] = True
        elif line.startswith("@@"):
            m = _HUNK.match(line)
            if m:
                cur["hunks"].append((int(m.group(1)), int(m.group(2)) if m.group(2) is not None else 1))
    return files


def module_key(path):
    dirs = path.split("/")[:-1]
    while dirs and dirs[0] in CONTAINERS:
        dirs = dirs[1:]
    if not dirs:
        return "."
    key = [dirs[0]]
    if len(dirs) > 1 and dirs[1] not in STOP:
        key.append(dirs[1])
    return "/".join(key)


def label_files(files):
    src = {p: f for p, f in files.items() if common.is_source(p) or common.is_source(f["new_path"])}
    n = len(src)
    if n == 0:
        return None
    if n == 1:
        f = next(iter(src.values()))
        if f["new_file"] or f["deleted"] or not f["hunks"] or len(f["hunks"]) > FN_HUNKS:
            return "file"
        lo = min(a for a, _ in f["hunks"])
        hi = max(a + max(k, 1) - 1 for a, k in f["hunks"])
        return "function" if hi - lo + 1 <= FN_SPAN else "file"
    if n >= 4:
        return "cross"
    keys = {module_key(f["new_path"] if f["new_file"] else p) for p, f in src.items()}
    return "module" if len(keys) == 1 else "cross"


def label_from_diff(diff):
    return label_from_files(parse_hunks(diff))


def label_from_files(files):
    return label_files(files)


def n_source_files(files):
    return sum(1 for p, f in files.items() if common.is_source(p) or common.is_source(f["new_path"]))
