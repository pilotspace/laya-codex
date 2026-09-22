"""Shared pieces for laya-code fine-tuning: input format (identical to inference), git diff parsing, chunking.

Input format contract (must match the Rust port and spike/laya_spike.py):
  question = QUESTIONS[0] (paraphrases QUESTIONS[1:] only for training robustness)
  state    = "file: {path} (lines {a}-{b})\n{text}", truncated to STATE_BUDGET tokens (prefix kept)
  ids      = reference rl_common.build_sequence(tok, state, noul question, max_len=512, head_max_len=192)
"""
import os
import re
import subprocess
import sys

BASE_MODEL = os.path.expanduser("~/.cache/laya-codex/models/laya-base")
WORK = os.path.expanduser("~/.cache/laya-codex/finetune")
if BASE_MODEL not in sys.path:
    sys.path.insert(0, BASE_MODEL)

STATE_BUDGET = 256
MAX_LEN, HEAD_MAX_LEN = 512, 192
WIN, STRIDE = 40, 30  # same as spike/laya_spike.py corpus

QUESTIONS = [
    'Is this source code relevant to the software change: "{task}"?',
    'Does this code need to be read or modified to implement the change: "{task}"?',
    'Is this code chunk relevant to the coding task: "{task}"?',
]

SRC_EXT = {".rs", ".py", ".ts", ".tsx", ".js", ".go", ".java", ".c", ".h", ".cc", ".cpp", ".hpp", ".rb", ".php",
           ".kt", ".swift", ".cs"}
SKIP_PARTS = ("/vendor/", "node_modules/", "/dist/", "/build/", "/target/", "__snapshots__", "/generated/", ".min.")

_HUNK = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+\d+(?:,\d+)? @@")
_BAD_SUBJ = re.compile(r"^(merge|bump|release|chore\(release|chore\(deps|revert|wip\b|initial commit|update readme|"
                       r"fix typo|format|fmt|lint|v?\d+\.\d+)", re.I)
_TRAILER = re.compile(r"^[A-Za-z-]+-by:|^Co-authored|^Change-Id:|^\(cherry picked|^Signed-off", re.I)


def is_source(path):
    return os.path.splitext(path)[1] in SRC_EXT and not any(p in "/" + path for p in SKIP_PARTS) \
        and not path.startswith(("target/", "node_modules/", "vendor/", "dist/"))


def git(repo, *args, timeout=120):
    return subprocess.run(["git", "-C", repo, *args], capture_output=True, text=True, timeout=timeout, check=True,
                          errors="ignore").stdout


# ----------------------------------------------------------------------------- tasks
def clean_subject(s):
    return re.sub(r"\s*\(#\d+\)\s*$", "", s).strip()


def informative(subj):
    s = clean_subject(subj)
    return len(s) >= 20 and not _BAD_SUBJ.match(s)


def task_text(subject, body):
    subj = clean_subject(subject)
    first = next((l.strip() for l in (body or "").splitlines() if l.strip()), "")
    if first and len(first) <= 120 and not _TRAILER.match(first) and not first.startswith(("*", "-", "#", "```")):
        return "%s. %s" % (subj.rstrip("."), first)
    return subj


# ----------------------------------------------------------------------------- diffs
def parse_diff_u0(diff):
    """`git diff -U0 -M` text -> {old_path: {"new_path", "new_file", "old_lines": set(1-based old-side lines)}}.

    Pure insertions (-a,0) mark old line a (and a+1): the code the change is inserted next to."""
    files, cur = {}, None
    for line in diff.splitlines():
        if line.startswith("diff --git "):
            m = re.match(r"diff --git a/(.*) b/(.*)$", line)
            cur = {"old_path": m.group(1), "new_path": m.group(2), "new_file": False, "old_lines": set()}
            files[cur["old_path"]] = cur
        elif cur is None:
            continue
        elif line.startswith("new file mode") or line == "--- /dev/null":
            cur["new_file"] = True
        elif line.startswith("@@"):
            m = _HUNK.match(line)
            if not m:
                continue
            a, n = int(m.group(1)), int(m.group(2)) if m.group(2) is not None else 1
            if n > 0:
                cur["old_lines"].update(range(a, a + n))
            elif a > 0:
                cur["old_lines"].update({a, a + 1})
    for f in files.values():
        if f["new_file"]:
            f["old_lines"] = set()
    return files


# ----------------------------------------------------------------------------- chunks
def windows(lines):
    """40-line windows, stride 30 -> [(start, end, text)] 1-based inclusive; same layout as the spike corpus."""
    out = []
    for s in range(0, max(1, len(lines)), STRIDE):
        body = "\n".join(lines[s:s + WIN])
        if body.strip():
            out.append((s + 1, min(len(lines), s + WIN), body))
        if s + WIN >= len(lines):
            break
    return out


def overlaps(start, end, lines):
    return any(start <= l <= end for l in lines)


def make_state(tok, path, start, end, text, budget=STATE_BUDGET):
    """State string truncated to `budget` tokens (keeps the prefix). Returns (state, last visible line number)."""
    header = "file: %s (lines %d-%d)\n" % (path, start, end)
    st = header + text
    enc = tok(st, add_special_tokens=False, return_offsets_mapping=True)
    if len(enc["input_ids"]) > budget:
        cut = enc["offset_mapping"][budget - 1][1]
        st = st[:cut]
        while len(tok(st, add_special_tokens=False)["input_ids"]) > budget:  # re-tokenization can merge differently
            st = st[:-8]
    visible = st[len(header):] if len(st) > len(header) else ""
    visible_end = min(end, start + visible.count("\n")) if visible else start
    return st, visible_end


def encode(tok, question, state):
    """Token ids + option marker positions for a noul question: exactly the reference inference layout."""
    from rl_common import build_sequence
    return build_sequence(tok, state, {"t": "noul", "ins": question, "crit": None}, MAX_LEN, HEAD_MAX_LEN)


def bm25_score_doc(bm, q, doc_tokens):
    """Score a document that is NOT in `bm`'s index with `bm`'s statistics (idf, avgdl) — same formula as BM25.search.

    Used to merge parent-revision windows of touched files into a snapshot index's candidate list."""
    from collections import Counter
    tf, dl, s = Counter(doc_tokens), len(doc_tokens), 0.0
    for t in set(q):
        if t in bm.idf and tf[t]:
            f = tf[t]
            s += bm.idf[t] * f * (bm.k1 + 1) / (f + bm.k1 * (1 - bm.b + bm.b * dl / bm.avg))
    return s
