"""Top-level directory mix of source files per repo (to choose the `module` key for scope labels)."""
import os
import sys
from collections import Counter

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import common  # noqa: E402
from repos import HELDOUT, TRAIN  # noqa: E402

for name, path in list(TRAIN.items()) + list(HELDOUT.items()):
    files = [f for f in common.git(path, "ls-files").splitlines() if common.is_source(f)]
    top = Counter(f.split("/")[0] if "/" in f else "." for f in files)
    two = Counter("/".join(f.split("/")[:2]) if f.count("/") >= 2 else f.split("/")[0] for f in files)
    print("%-28s n=%5d top=%s" % (name, len(files), top.most_common(5)))
    print("%-28s        top2=%s" % ("", two.most_common(6)))
