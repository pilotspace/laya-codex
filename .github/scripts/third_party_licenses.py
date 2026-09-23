#!/usr/bin/env python3
"""Write the license texts of every crate linked into the `laya` binary (THIRD-PARTY-LICENSES.txt).

Reads `cargo metadata --filter-platform <triple>` and walks normal + build dependencies of the
workspace members (dev-dependencies are not shipped). For each crate it copies the LICENSE* /
COPYING* / NOTICE* files from its registry source, plus the bundled C sources' licenses of
onig_sys (Oniguruma) and libmimalloc-sys (mimalloc). A crate without any license file gets its
SPDX expression and authors only; the script exits 1 when a crate declares no license at all.
`cargo metadata` unifies features across targets, so the list is a superset of what one target
links (e.g. the Metal crates also appear for Linux).

    cargo metadata --format-version 1 --locked --filter-platform x86_64-unknown-linux-gnu \
        | python3 .github/scripts/third_party_licenses.py > THIRD-PARTY-LICENSES.txt
"""
import glob
import json
import os
import sys

TOP_LEVEL = ("LICENSE", "LICENCE", "COPYING", "NOTICE", "UNLICENSE")
NESTED = {
    "onig_sys": ["oniguruma/COPYING"],
    "libmimalloc-sys": ["c_src/mimalloc/*/LICENSE"],
}


def shipped_packages(meta):
    members = set(meta["workspace_members"])
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    seen, stack = set(), list(members)
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        for dep in nodes[pid]["deps"]:
            if any(k["kind"] in (None, "build") for k in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    pkgs = {p["id"]: p for p in meta["packages"]}
    return sorted((pkgs[i] for i in seen - members), key=lambda p: (p["name"], p["version"]))


def license_files(pkg):
    root = os.path.dirname(pkg["manifest_path"])
    files = []
    if pkg.get("license_file"):
        files.append(os.path.join(root, pkg["license_file"]))
    if os.path.isdir(root):
        files += [os.path.join(root, f) for f in sorted(os.listdir(root)) if f.upper().startswith(TOP_LEVEL)]
    for pattern in NESTED.get(pkg["name"], []):
        files += sorted(glob.glob(os.path.join(root, pattern)))
    return [f for f in dict.fromkeys(files) if os.path.isfile(f)]


def main():
    meta = json.load(sys.stdin)
    out, missing = sys.stdout, []
    pkgs = shipped_packages(meta)
    out.write("Third-party software linked into laya (%d crates)\n\n" % len(pkgs))
    for p in pkgs:
        spdx = p.get("license") or ""
        if not spdx and not p.get("license_file"):
            missing.append("%s %s" % (p["name"], p["version"]))
        out.write("=" * 78 + "\n%s %s  (%s)\n%s\n" % (p["name"], p["version"], spdx or "see file",
                                                      p.get("repository") or ""))
        files = license_files(p)
        if not files:
            out.write("\n[no license file shipped in the crate; license: %s; authors: %s;\n"
                      " standard text: https://spdx.org/licenses/]\n\n" % (spdx, ", ".join(p.get("authors") or []) or "?"))
        for f in files:
            out.write("\n--- %s\n\n" % os.path.relpath(f, os.path.dirname(p["manifest_path"])))
            with open(f, encoding="utf-8", errors="replace") as fh:
                out.write(fh.read().rstrip() + "\n")
        out.write("\n")
    if missing:
        sys.stderr.write("crates without a declared license: %s\n" % ", ".join(missing))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
