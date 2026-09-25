"""Repo selection for laya-code fine-tuning (single source of truth for train / held-out split).

Held out (never a training example, checked by finetune/leakage.py): moon, httpx and hono (the
benchmark repos the replay gate reads) and pilot-space (the held-out evaluation repo).
"""
import os
import subprocess

# Directory holding one checkout per repo below (set LAYA_CODEX_REPOS_ROOT).
ROOT = os.path.expanduser(os.environ.get("LAYA_CODEX_REPOS_ROOT", "~/src"))
# Directory holding the benchmark checkouts (moon, httpx, hono); defaults to ROOT.
BENCH_ROOT = os.path.expanduser(os.environ.get("LAYA_CODEX_BENCH_REPOS", ROOT))

HELDOUT = {
    "moon": os.path.join(BENCH_ROOT, "moon"),
    "httpx": os.path.join(BENCH_ROOT, "httpx"),
    "hono": os.path.join(BENCH_ROOT, "hono"),
    "pilot-space": os.path.join(ROOT, "pilot-space"),
}

# mixed languages, no forks/clones of each other or of a held-out repo (checked by check_leakage()).
# Excluded on purpose: helios / helios-mono / lunaris are Moon *client* codebases (59-146 files mention moon);
# not moon's source, but same domain vocabulary -> would flatter the moon eval. dify / clickai/* are clones of
# each other; ai-proxy-builds/* are copies of ai-proxy. ai-guard (a local checkout of Portkey-AI/gateway that
# trained the first laya-code) is no longer available and is left out.
TRAIN = {
    "codex": os.path.join(ROOT, "codex"),                                      # Rust + TS
    "velos": os.path.join(ROOT, "velos"),                                      # Rust
    "PraisonAI": os.path.join(ROOT, "PraisonAI"),                              # Py + TS
    "pi-mono": os.path.join(ROOT, "pi-mono"),                                  # TS
    "python-dependency-injector": os.path.join(ROOT, "python-dependency-injector"),  # Py
    "dispatch": os.path.join(ROOT, "repo-sample/python/dispatch"),             # Py + JS
    "ai-proxy": os.path.join(ROOT, "ai-proxy"),                                # Py + TSX
}


def check_leakage():
    """Raise if a train repo overlaps, shares history with or vendors source of a held-out repo (leakage.py)."""
    from leakage import check_repos
    rep = check_repos(TRAIN, HELDOUT)
    if not rep["ok"]:
        raise RuntimeError("; ".join(rep["problems"]))
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
