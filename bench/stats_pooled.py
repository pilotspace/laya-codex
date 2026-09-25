"""Per-repo and pooled paired bootstrap CIs across several run dirs (benchmark v2 / v8).

    python3 bench/stats_pooled.py <treatment arm> <baseline arm> <run dir> [<run dir> ...]

For every metric it prints, per repo and pooled:
- the change of the ratio of sums (treatment / baseline - 1), as in bench/stats.py, with a 95% CI from a paired
  bootstrap (10k resamples; pooled = *stratified* bootstrap, tasks resampled within each repo so every
  replicate keeps each repo's share of tasks);
- for the pooled row also the repo-balanced change: the mean of the per-repo changes, so a repo with heavier
  sessions (moon) does not dominate; same stratified bootstrap.
Quality: turn-1 recall and both-turn recall means, and the paired difference with a CI.
Only tasks finished by both arms are paired; sessions with a non-zero exit or a timeout are listed.
"""
import json
import os
import random
import sys

from runs import load_runs

B = 10000


def reading(r):
    return (r.get("reading_tokens") or 0) + (r.get("injected_tokens") or 0)


# `reading+injected tokens` -- tokens Claude reads plus tokens laya-codex injects -- is the
# VISION.md token target and the primary token metric; it leads the table. `code-reading tokens`
# stays right beside it, so a smaller-reads/bigger-injection trade shows as a loss up top, not
# just as a win further down.
METRICS = {
    "reading+injected tokens": reading,
    "code-reading tokens": lambda r: r.get("reading_tokens") or 0,
    "total input tokens": lambda r: r.get("total_input_tokens") or 0,
    "wall seconds": lambda r: r.get("wall_s") or 0,
    "turns": lambda r: r.get("num_turns") or 0,
    "cost usd": lambda r: r.get("cost_usd") or 0,
    # Output tokens drive wall time (wall ~ 5.2 + 10.8*output_ktok): a time proxy that is cheap to
    # read off the API usage, unlike wall_s which also carries CLI startup and network noise.
    "output tokens": lambda r: r.get("output_tokens") or 0,
}
QUALITY = {
    "answer recall (turn 1)": lambda r: r["recall"],
    "answer recall (both turns)": lambda r: r.get("recall_all_turns", r["recall"]),
}


def load(run_dir, arm, base):
    # Repeated sessions of a task are averaged into one row (bench/runs.py), so tasks stay the unit.
    # load_runs already dropped unhealthy rows in favor of a healthy rerun where one exists; a pair
    # is still `unhealthy` here only when every attempt at it failed to inject.
    by = load_runs(os.path.join(run_dir, "runs.jsonl"), arms=(arm, base))
    by = {a: by.get(a, {}) for a in (arm, base)}
    tasks = sorted(set(by[arm]) & set(by[base]))
    bad = [(a, t, r["rc"]) for a in (arm, base) for t, r in by[a].items() if any(c != 0 for c in r["rc"])]
    unhealthy = [(a, t) for a in (arm, base) for t, r in by[a].items() if r.get("unhealthy")]
    return [(by[arm][k], by[base][k]) for k in tasks], bad, unhealthy


def ratio(pairs, f):
    sb = sum(f(b) for _, b in pairs)
    return sum(f(t) for t, _ in pairs) / sb - 1 if sb else float("nan")


def mean_diff(pairs, f):
    return sum(f(t) - f(b) for t, b in pairs) / len(pairs)


def ci(boots):
    boots = sorted(x for x in boots if x == x)
    return boots[int(0.025 * len(boots))], boots[int(0.975 * len(boots)) - 1]


def main():
    arm, base, dirs = sys.argv[1], sys.argv[2], sys.argv[3:]
    repos = {os.path.basename(os.path.normpath(d)): load(d, arm, base) for d in dirs}
    rng = random.Random(0)
    # one set of stratified resample indices shared by all metrics (paired across metrics too)
    idx = [{name: [rng.randrange(len(p)) for _ in p] for name, (p, _, _) in repos.items()} for _ in range(B)]
    print("%s vs %s  (paired tasks: %s)" % (arm, base, ", ".join("%s %d" % (n, len(p)) for n, (p, _, _) in repos.items())))
    for n, (_, bad, unhealthy) in repos.items():
        if bad:
            print("  non-zero exits in %s: %s" % (n, bad))
        if unhealthy:
            print("  unhealthy injection (all attempts failed) in %s: %s" % (n, unhealthy))
    print("| metric | " + " | ".join(repos) + " | pooled (ratio of sums) | pooled (repo-balanced) |")
    print("|---" * (len(repos) + 3) + "|")
    for name, f in METRICS.items():
        cells = []
        for rn, (p, _, _) in repos.items():
            boots = [ratio([p[i] for i in ix[rn]], f) for ix in idx]
            lo, hi = ci(boots)
            wins = sum(f(t) < f(b) for t, b in p)
            cells.append("%+.1f%% [%+.1f, %+.1f] %d/%d" % (100 * ratio(p, f), 100 * lo, 100 * hi, wins, len(p)))
        allp = [x for p, _, _ in repos.values() for x in p]
        pooled_boot = [ratio([repos[rn][0][i] for rn in repos for i in ix[rn]], f) for ix in idx]
        lo, hi = ci(pooled_boot)
        wins = sum(f(t) < f(b) for t, b in allp)
        cells.append("**%+.1f%%** [%+.1f, %+.1f] %d/%d" % (100 * ratio(allp, f), 100 * lo, 100 * hi, wins, len(allp)))
        bal = lambda ix: sum(ratio([repos[rn][0][i] for i in ix[rn]], f) for rn in repos) / len(repos)
        point = sum(ratio(p, f) for p, _, _ in repos.values()) / len(repos)
        lo, hi = ci([bal(ix) for ix in idx])
        cells.append("%+.1f%% [%+.1f, %+.1f]" % (100 * point, 100 * lo, 100 * hi))
        print("| %s | %s |" % (name, " | ".join(cells)))
    for name, f in QUALITY.items():
        cells = []
        for rn, (p, _, _) in repos.items():
            lo, hi = ci([mean_diff([p[i] for i in ix[rn]], f) for ix in idx])
            cells.append("%.3f vs %.3f (%+.3f [%+.3f, %+.3f])" % (
                sum(f(t) for t, _ in p) / len(p), sum(f(b) for _, b in p) / len(p), mean_diff(p, f), lo, hi))
        allp = [x for p, _, _ in repos.values() for x in p]
        lo, hi = ci([mean_diff([repos[rn][0][i] for rn in repos for i in ix[rn]], f) for ix in idx])
        cells.append("%.3f vs %.3f (**%+.3f** [%+.3f, %+.3f])" % (
            sum(f(t) for t, _ in allp) / len(allp), sum(f(b) for _, b in allp) / len(allp), mean_diff(allp, f), lo, hi))
        cells.append("")
        print("| %s | %s |" % (name, " | ".join(cells)))


if __name__ == "__main__":
    main()
