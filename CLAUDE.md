# laya-codex — working rules for AI assistants

Read [docs/VISION.md](docs/VISION.md) before proposing work. It defines the product, the targets
and the only three levers in scope. The active plan is
[docs/plans/2026-09-refocus.md](docs/plans/2026-09-refocus.md).

## Stay on the levers

Before starting any change, name which lever it moves and which number it should change:

1. **Pull** — Claude answers its lookups with laya-codex `search` instead of Grep
   (tool calls per session, Grep per session).
2. **Precision** — Laya ranks the right code first, so a smaller injection carries the gold file
   (gold inlined vs injected tokens in the offline replay).
3. **Honest measurement** — benchmark and replay tooling that reports the targets as defined.

If a change moves none of them, or touches anything VISION.md lists as out of scope, stop and ask the owner.
Do not add opt-in flags, new render modes, new heuristics or new distribution work "while here".

## Architecture in one breath

tree-sitter chunks (10–50 lines) → Moon BM25 store → lexical candidates (BM25 ⊕ defining ⊕ path,
top 24) → Laya rerank (`laya-code` cross-encoder, 1.2 s budget, 0.5/0.5 blend) → push via the
`UserPromptSubmit` hook, pull via the MCP `search` tool. Lexical finds candidates and Laya orders
them; keep both. Hooks always fail open.

| crate | owns |
|---|---|
| `laya-parse` | tree-sitter chunking, line-window fallback |
| `laya-store` | Moon client, BM25, supervisor, auth |
| `laya-model` | the Laya scorer (candle, Metal/CPU) |
| `laya-rank` | retriever, fusion, sizing, rendering |
| `laya-cli` | `laya-codex` binary: daemon, hooks, MCP server, doctor, trace |

## Evidence rules

- Claims about speed, tokens or quality come from the paired benchmark (`bench/run_bench.py`,
  `bench/stats_pooled.py`) or the offline replay, never from a single session.
- The token target is tokens read **plus** tokens injected. Report both.
- Record the rank mode (`laya`, `laya-partial`, `lexical`) for every prompt you compare.
- Paid benchmark runs need the owner's approval, with the cost estimate stated.

## Commands

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --release --locked
cargo build --release --locked -p laya-cli     # target/release/laya-codex
sh scripts/test-install.sh && sh scripts/test-plugin.sh
```

End-to-end checks use the real model: Metal's first run compiles kernels (~10 s), so warm the
daemon before timing anything. Scratch daemons need their own `LAYA_CODEX_HOME` and Moon port.

## Workflow

- Red/green TDD: a failing test first, then the change.
- One workstream per branch and PR. Ask before opening a PR, merging, closing PRs, pushing tags,
  publishing releases or uploading models.
- Squash-merge in dependency order; resolve conflicts by merging `main` into the branch; never
  force-push.
- Commit messages: `<type>(<scope>): <summary>`, a body explaining why, no plan IDs or AI
  references.
- Keep `plugin/.claude-plugin/plugin.json` at the Cargo workspace version.
- Personal or tool-specific instructions go in `CLAUDE.local.md` (ignored), not here.
