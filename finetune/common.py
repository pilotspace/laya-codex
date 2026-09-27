"""Shared pieces for laya-code fine-tuning: input format (identical to inference), git diff parsing, chunking.

Input format contract (must match the Rust scorer, crates/laya-model/src/{scorer,sequence}.rs):
  question = QUESTIONS[0] with {task} = the retriever's task focus
  state    = render_state(path, a, b, text) = "file: {path} (lines {a}-{b})\n{text}"
  ids      = state tokens ("[MASK]" blanked) cut to the first STATE_TOKENS (128, the daemon default), then
             [CLS] "noul question: ..." [SEP] [MASK] " false: ..." [MASK] " true: ..." [SEP] state [SEP]
             (encode_ids: a port of SequenceBuilder::build_from_state_ids; max_len 512, head_max_len 192)

The older window helpers (windows, make_state, encode, bm25_score_doc) serve eval.py's spike protocol.
"""
import glob
import os
import re
import subprocess
import sys


def find_model_dir(env=os.environ, home=os.path.expanduser("~")):
    """Model dir whose rl_common.py and tokenizer the tools import: LAYA_CODEX_FT_BASE when set, else the first
    of laya-base, the installed laya-code and the Hugging Face cache snapshot of tindang/laya-code that has both
    (their tokenizer and reference code are byte-identical); the laya-base path when none does."""
    if env.get("LAYA_CODEX_FT_BASE"):
        return os.path.expanduser(env["LAYA_CODEX_FT_BASE"])
    models = os.path.join(home, ".cache", "laya-codex", "models")
    hub = os.path.join(env.get("HF_HOME") or os.path.join(home, ".cache", "huggingface"), "hub")
    cands = [os.path.join(models, "laya-base"), os.path.join(models, "laya-code")] + \
        sorted(glob.glob(os.path.join(hub, "models--tindang--laya-code", "snapshots", "*")))
    for c in cands:
        if os.path.isfile(os.path.join(c, "tokenizer", "tokenizer.json")) and os.path.isfile(os.path.join(c, "rl_common.py")):
            return c
    return cands[0]


BASE_MODEL = find_model_dir()
WORK = os.path.expanduser(os.environ.get("LAYA_CODEX_FT_WORK", "~/.cache/laya-codex/finetune"))
if BASE_MODEL not in sys.path:
    sys.path.insert(0, BASE_MODEL)

STATE_TOKENS = 128  # LAYA_CODEX_STATE_TOKENS default in the daemon
STATE_BUDGET = STATE_TOKENS
MAX_LEN, HEAD_MAX_LEN = 512, 192
FILE_SOFT = 0.6  # label of a candidate from a changed file that does not overlap a changed line
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


def render_state(path, start, end, text):
    """`LayaScorer::render_state`."""
    return "file: %s (lines %d-%d)\n%s" % (path, start, end, text)


def state_ids(tok, state, budget=STATE_TOKENS):
    """`SequenceBuilder::encode_state(state, Some(budget))`: tokens of the state, "[MASK]" blanked, first `budget`."""
    return tok(state.replace(tok.mask_token, " "), add_special_tokens=False)["input_ids"][:budget]


_HEAD_CACHE = {}


def _head(tok, question):
    key = (id(tok), question)
    if key not in _HEAD_CACHE:
        from rl_common import render_options
        mask = tok.mask_token
        head = tok("noul question: %s" % question.replace(mask, " "), add_special_tokens=False)["input_ids"]
        opts = [[tok.mask_token_id] + tok(" " + o.replace(mask, " "), add_special_tokens=False)["input_ids"][:48]
                for o in render_options({"t": "noul", "ins": question, "crit": None})]
        budget = HEAD_MAX_LEN - sum(len(o) for o in opts)
        if budget < 16:
            per = max(4, (HEAD_MAX_LEN - 16) // len(opts))
            opts = [o[:per] for o in opts]
            budget = HEAD_MAX_LEN - sum(len(o) for o in opts)
        ids = [tok.cls_token_id] + head[:max(8, budget)] + [tok.sep_token_id]
        markers = []
        for o in opts:
            markers.append(len(ids))
            ids += o
        ids.append(tok.sep_token_id)
        if len(_HEAD_CACHE) > 4096:
            _HEAD_CACHE.clear()
        _HEAD_CACHE[key] = (ids, markers)
    return _HEAD_CACHE[key]


def encode_ids(tok, question, sids):
    """`SequenceBuilder::build_from_state_ids` for a noul question: (ids, marker positions)."""
    head, markers = _head(tok, question)
    room = max(0, MAX_LEN - len(head) - 1)
    ids = (head + list(sids[:room]) + [tok.sep_token_id])[:MAX_LEN]
    return ids, [m for m in markers if m < MAX_LEN]


# ----------------------------------------------------------------------------- fixed commits -> labels
_FIX = re.compile(r"\b(fix(e[sd]|ing)?|bug(s|fix)?|hotfix|crash(es|ed|ing)?|regression|broken|issue)\b", re.I)
_ISSUE_REF = re.compile(r"\b(fix(e[sd])?|close[sd]?|resolve[sd]?)\s*:?\s+(#\d+|https://github\.com/\S+/issues/\d+)", re.I)
_CONVENTIONAL = re.compile(r"^(\w+)(?:\(([^)]*)\))?!?:\s*")
_SKIP_BODY = re.compile(r"^(#|```|[-*>|]|\d+\.\s|https?://|\[x\]|\[ \])", re.I)
MAX_TASK_CHARS = 400


def is_fix(subject, body):
    """A commit that fixes something: a fix/bug word in the subject, or a body that closes an issue."""
    return bool(_FIX.search(subject) or _ISSUE_REF.search(body or ""))


def commit_task(subject, body, with_body):
    """Task text of a fixed commit: the subject without `type(scope):` / `(#123)`, plus (with_body) the first
    prose paragraph of the body, capped at MAX_TASK_CHARS."""
    subj = clean_subject(subject)
    m = _CONVENTIONAL.match(subj)
    if m:
        scope = m.group(2) if m.group(2) and not re.fullmatch(r"[\d.\-\s]+", m.group(2)) else ""
        subj = ("%s: " % scope if scope else "") + subj[m.end():]
    subj = subj.strip()
    if not with_body:
        return subj[:MAX_TASK_CHARS]
    para = []
    for line in (body or "").splitlines():
        s = line.strip()
        if not s:
            if para:
                break
            continue
        if _TRAILER.match(s) or _SKIP_BODY.match(s):
            if para:
                break
            continue
        para.append(s)
    text = subj.rstrip(".")
    if para:
        text = "%s. %s" % (text, " ".join(para))
    if len(text) > MAX_TASK_CHARS:
        text = text[:MAX_TASK_CHARS].rsplit(" ", 1)[0]
    return text


def label_candidates(cands, diff_files):
    """(label, kind) per candidate at the parent revision: `hunk` (overlaps a changed line) 1.0, `file` (another
    chunk of a changed file) FILE_SOFT, `other` 0.0. `diff_files` is parse_diff_u0 output (keyed by old path)."""
    out = []
    for c in cands:
        f = diff_files.get(c["path"])
        if f is None or f["new_file"]:
            out.append((0.0, "other"))
        elif overlaps(c["start"], c["end"], f["old_lines"]):
            out.append((1.0, "hunk"))
        else:
            out.append((FILE_SOFT, "file"))
    return out


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
