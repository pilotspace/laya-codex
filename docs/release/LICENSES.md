# License audit — laya-codex v0.1.0

Checked 2026-09-23 against the license files and metadata listed per row (not from memory).
Project license: **Apache-2.0** (`LICENSE`, `NOTICE`; `license = "Apache-2.0"` in
`[workspace.package]`, `license.workspace = true` in all six crates).

**Verdict: no blocker.** Every Rust dependency that ships in the `laya` binary is under a
permissive license compatible with Apache-2.0; no copyleft-only dependency is linked. The Laya
base model is Apache-2.0, so `laya-code` may be redistributed under Apache-2.0 with attribution.
Two follow-ups (not blockers) are listed at the end.

## 1. Models

| component | license | source checked | compatible | notes |
|---|---|---|---|---|
| Laya base (`convaiinnovations/laya`, revision `1c5edc17a7acd8701df6fc341c0d179f1c62c982`) | Apache-2.0 | HF API `cardData.license = apache-2.0`, tag `license:apache-2.0`; model card front matter `license: apache-2.0`, footer "Apache 2.0 · Convai Innovations"; tag `commercial-use`. The HF repo has no separate LICENSE file. | yes | Local copy `~/.cache/laya-codex/models/laya-base/README.md` is byte-identical to the current HF card. |
| Laya reference code (`rl_agent_api.py`, `rl_common.py`; GitHub `NandhaKishorM/laya`) | Apache-2.0 | GitHub API license `Apache-2.0` | yes | Shipped unchanged inside the laya-code checkpoint (same sha256 as in laya-base). |
| ModernBERT-large encoder config + tokenizer (`answerdotai/ModernBERT-large`) | Apache-2.0 | HF API `cardData.license = apache-2.0` | yes | Laya's backbone; `encoder/config.json` and `tokenizer/*` are unchanged in laya-code. |
| **laya-code** (fine-tuned from Laya base) | **Apache-2.0** | derivative of the rows above | yes | Apache-2.0 §4 lets us distribute a Derivative Work under Apache-2.0 provided we (a) include the license, (b) mark modified files, (c) keep upstream attribution. The HF package does this (`release/hf-laya-code/README.md` states which files changed, `NOTICE` carries attribution). Weights are not in this repo or its release archives. |

Training data of laya-code (weak labels from the git history of 8 repos; the code is used as
classifier input, the model cannot generate text): openai/codex (Apache-2.0), TinDang97/velos
(no license file, author's own repo), MervinPraison/PraisonAI (MIT), badlogic/pi-mono (MIT),
local `ai-guard` = a checkout of Portkey-AI/gateway (MIT), TinDang97/python-dependency-injector
(BSD-3-Clause, fork of ets-labs/python-dependency-injector), Netflix/dispatch (Apache-2.0), local
`ai-proxy` = pilotspace/hydroa (no license file, author's organisation). All public; none is
copyleft. The two repos without a license file are owned by the author/his organisation, so
their use needs no third-party permission. The legal status of weights trained on source code
is not settled law; the model card says so.

## 2. Runtime companion (not linked, not bundled)

| component | license | source checked | compatible | notes |
|---|---|---|---|---|
| Moon (`pilotspace/moon`) | declared Apache-2.0 (`Cargo.toml`: `license = "Apache-2.0"`, v0.8.9 at HEAD) | GitHub API reports `NOASSERTION` ("Other"); `LICENSE` at HEAD diffed against apache.org's `LICENSE-2.0.txt` | yes (separate program) | laya-codex starts `moon` as a separate process and talks RESP over TCP; it does not link, vendor or ship Moon. Its license therefore does not constrain laya-codex's license. **Follow-up 1** below. |

## 3. Rust dependencies linked into `laya`

253 third-party crates are reachable through normal/build dependencies of the workspace
(`cargo metadata --locked`, all targets; dev-dependencies excluded). License expressions:

| license expression | crates |
|---|---|
| MIT OR Apache-2.0 (incl. `MIT/Apache-2.0`, `Apache-2.0 OR MIT`, `Apache-2.0/MIT`, `Apache-2.0 / MIT`) | 129 |
| MIT | 67 |
| Unicode-3.0 (ICU4X / idna data crates) | 20 |
| Unlicense OR MIT / `Unlicense/MIT` | 8 |
| Apache-2.0 | 5 |
| Zlib OR Apache-2.0 OR MIT / Apache-2.0 OR MIT OR Zlib | 8 |
| Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | 4 |
| BSD-2-Clause OR Apache-2.0 OR MIT | 2 |
| MIT OR Apache-2.0 OR LGPL-2.1-or-later (`r-efi`, UEFI only, never built for our targets; MIT chosen) | 2 |
| CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception (`blake3`) | 1 |
| CC0-1.0 OR MIT-0 OR Apache-2.0 (`constant_time_eq`) | 1 |
| (MIT OR Apache-2.0) AND Unicode-3.0 (`unicode-ident`) | 1 |
| Apache-2.0 OR BSL-1.0 (`ryu`) | 1 |
| BSL-1.0 (`xxhash-rust`) | 1 |
| BSD-3-Clause (`redis`) | 1 |
| ISC (`libloading`) | 1 |
| Zlib (`foldhash`) | 1 |

All are Apache-2.0-compatible. Key components:

| component | version | license | notes |
|---|---|---|---|
| candle-core, candle-nn, candle-metal-kernels, candle-ug | 0.11.0 | MIT OR Apache-2.0 | Hugging Face |
| tokenizers | 0.22.2 | Apache-2.0 | no NOTICE file in the crate |
| safetensors | 0.4.5, 0.8.0 | Apache-2.0 | |
| onig / onig_sys | 6.5.3 / 69.9.3 | MIT (bindings) + **BSD-2-Clause** (bundled Oniguruma C library, `oniguruma/COPYING`, © K.Kosako) | binary redistribution must reproduce the Oniguruma notice — done via `THIRD-PARTY-LICENSES.txt` in release archives and `NOTICE` |
| mimalloc / libmimalloc-sys | 0.1.52 / 0.1.49 | MIT (bindings © Octavian Oncescu; C library © Microsoft, Daan Leijen) | |
| gemm (+ gemm-*) | 0.18.2, 0.19.0 | MIT | candle CPU matmul |
| metal, objc2-*, block2 | — | MIT / MIT OR Apache-2.0 / Zlib OR Apache-2.0 OR MIT | macOS only in practice (see script note on feature unification) |
| tree-sitter, tree-sitter-language | 0.27.0, 0.1.8 | MIT | |
| tree-sitter-rust 0.24.2, -python 0.25.0, -typescript 0.23.2, -javascript 0.25.0, -go 0.25.0, -java 0.23.5, -c 0.24.2, -cpp 0.23.4, -c-sharp 0.23.5, -ruby 0.23.1, -php 0.24.2, -kotlin-ng 1.1.0, -swift 0.7.3 | | MIT (all 13 grammar crates) | cpp, java, kotlin-ng, ruby, typescript ship no license file in the crate; release archives record the SPDX id + authors |
| redis (redis-rs) | 1.7.0 | BSD-3-Clause | |
| clap, serde, serde_json, anyhow, thiserror, rayon, libc, blake3 | | MIT OR Apache-2.0 (blake3: CC0/Apache) | |
| ignore, walkdir, memchr, aho-corasick | | Unlicense OR MIT | |

No dependency ships a `NOTICE` file (checked every crate source directory), so Apache-2.0 §4(d)
imposes no extra NOTICE content from dependencies.

Release archives include `THIRD-PARTY-LICENSES.txt`, generated by
`.github/scripts/third_party_licenses.py` from `cargo metadata --filter-platform <target>`: every
crate's LICENSE/COPYING/NOTICE files plus Oniguruma's and mimalloc's C-library licenses. MIT,
BSD and ISC require the notice to travel with binaries; this file satisfies that.

## 4. Not shipped

- Python tooling in `finetune/`, `spike/`, `bench/` (PyTorch BSD-3-Clause, transformers
  Apache-2.0, numpy BSD-3-Clause) runs locally and is not a dependency of the Rust binary.
- `bench/results/` contains benchmark transcripts over a Moon clone (Apache-2.0 per its
  Cargo.toml); `crates/laya-parse/tests/fixtures/` are hand-written samples.

## 5. Follow-ups (not blockers)

1. **Moon's LICENSE text is not the canonical Apache-2.0 text.** At `pilotspace/moon` HEAD the
   `LICENSE` file is a paraphrase of Apache-2.0 (e.g. §4 "You may add Your own license statement…
   sublicense, and/or sell", reworded "Contribution" and patent clauses), which is why GitHub
   cannot detect it. The local clone used by the benchmark (`~/workspaces/tind-repo/moon`,
   v0.1.12) has a **GPL-3.0** `LICENSE` while its `Cargo.toml` says Apache-2.0. Moon belongs to
   the same author, so the fix is on Moon's side: replace `LICENSE` with the verbatim
   apache.org text. Until then, tell users Moon is licensed per its own repository; laya-codex
   is unaffected because it never links or ships Moon.
2. `bench/results/` transcripts quote Moon source; keep Moon's attribution if that directory is
   published, and prefer publishing it only once Moon's LICENSE is fixed.
