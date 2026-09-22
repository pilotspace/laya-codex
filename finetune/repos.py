"""Repo selection for laya-code fine-tuning (single source of truth for train / held-out split).

moon is the primary held-out eval repo and must never contribute a training example.
pilot-space is the second held-out eval repo.
"""
import os
import subprocess

ROOT = os.path.expanduser("~/workspaces/tind-repo")

HELDOUT = {
    "moon": os.path.join(ROOT, "moon"),
    "pilot-space": os.path.join(ROOT, "pilot-space"),
}

# mixed languages, no forks/clones of each other or of a held-out repo (checked by check_leakage()).
# Excluded on purpose: helios / helios-mono / lunaris are Moon *client* codebases (59-146 files mention moon);
# not moon's source, but same domain vocabulary -> would flatter the moon eval. dify / clickai/* are clones of
# each other; ai-proxy-builds/* are copies of ai-proxy.
TRAIN = {
    "codex": os.path.join(ROOT, "codex"),                                      # Rust + TS
    "velos": os.path.join(ROOT, "velos"),                                      # Rust
    "PraisonAI": os.path.join(ROOT, "PraisonAI"),                              # Py + TS
    "pi-mono": os.path.join(ROOT, "pi-mono"),                                  # TS
    "ai-guard": os.path.join(ROOT, "ai-guard"),                                # TS
    "python-dependency-injector": os.path.join(ROOT, "python-dependency-injector"),  # Py
    "dispatch": os.path.join(ROOT, "repo-sample/python/dispatch"),             # Py + JS
    "ai-proxy": os.path.join(ROOT, "ai-proxy"),                                # Py + TSX
}


def _git(repo, *args):
    return subprocess.run(["git", "-C", repo, *args], capture_output=True, text=True, timeout=120, check=True).stdout


def root_commits(repo):
    return set(_git(repo, "rev-list", "--max-parents=0", "HEAD").split())


def check_leakage():
    """Raise if any training repo is (inside) a held-out repo or shares a root commit with one or with another train repo."""
    held_roots = {}
    for name, path in HELDOUT.items():
        held_roots[name] = root_commits(path)
    seen = {}
    for name, path in TRAIN.items():
        real = os.path.realpath(path)
        for hn, hp in HELDOUT.items():
            hp = os.path.realpath(hp)
            if real == hp or real.startswith(hp + os.sep) or hp.startswith(real + os.sep):
                raise RuntimeError("train repo %s overlaps held-out repo %s on disk" % (name, hn))
        roots = root_commits(path)
        for hn, hr in held_roots.items():
            if roots & hr:
                raise RuntimeError("train repo %s shares history with held-out repo %s" % (name, hn))
        for on, orr in seen.items():
            if roots & orr:
                raise RuntimeError("train repos %s and %s share history (fork/clone)" % (name, on))
        seen[name] = roots
    return True


def moon_refs(repo):
    """Number of source files at HEAD that mention moon (domain-overlap audit, not a hard failure)."""
    r = subprocess.run(["git", "-C", repo, "grep", "-ilw", "moon", "HEAD", "--", "*.rs", "*.py", "*.ts", "*.tsx",
                        "*.go", "*.js"], capture_output=True, text=True, timeout=120)
    return len(r.stdout.splitlines())


if __name__ == "__main__":
    for n, p in TRAIN.items():
        print("moon refs %-28s %d" % (n, moon_refs(p)))
    check_leakage()
    print("leakage check OK: %d train repos, held-out %s" % (len(TRAIN), sorted(HELDOUT)))
