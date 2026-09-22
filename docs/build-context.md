# laya-codex — shared build context (read first)

Owner/orchestrator: the lead session. Architecture: `docs/architecture.md` (v1.0, decisions D1–D7).
Research: `docs/research/*.md`. Spike: `spike/`, results in `spike/results/`.

## Product in one paragraph
`laya` is a single Rust binary that indexes a repo with tree-sitter into 10–50-line AST-aligned
chunks, stores them in **Moon** (Redis-compatible server, pilotspace/moon, run as a sidecar),
retrieves candidates with BM25 (Moon `FT.SEARCH`) plus symbol matches, re-ranks them with the
**Laya** model (ModernBERT-large typed-decision classifier, run in-process via candle), and hands
the top-10 spans to Claude Code through hooks (UserPromptSubmit injection, guarded
PreToolUse(Read) narrowing, PreToolUse(Agent|Task) handoff, PostToolUse re-index) and an MCP
server. Goal: −50% codebase-reading tokens, −30% task time, no drop in task success.

## Hard facts established so far (verified in this repo)
- Rust 1.94 stable, macOS arm64 (M4 Pro, 24 GB). Edition 2024 workspace at repo root.
- Crate versions on crates.io today: tree-sitter 0.27.0, candle-core/candle-nn/candle-transformers
  0.11.0, tokenizers 1.0.0-rc.2 (0.2x stable also exists), redis 1.7.0, rmcp 3.4.0.
- **Laya model** files: `~/.cache/laya-codex/models/laya-base/` (`model.safetensors` bf16 843 MB,
  `encoder/config.json` ModernBERT-large: 28 layers, hidden 1024, 16 heads, intermediate 2624,
  global attention every 3rd layer (rope θ 160000), local sliding window 128 (rope θ 10000),
  `tokenizer/tokenizer.json`, `rl_agent_config.json`, reference Python `rl_agent_api.py` +
  `rl_common.py`). The reference implementation is the spec for the port.
- Laya head (from `rl_common.py`): `h = encoder(...).last_hidden_state + type_emb(qtype)`;
  2 × `nn.TransformerEncoderLayer(d=1024, nhead=16, ff=4096, norm_first=True, activation=relu,
  batch_first)` with key padding mask; gather `h` at option `[MASK]` marker positions;
  `scorer = LayerNorm → Linear(d,d) → GELU(exact erf) → Linear(d,1)`; logits masked;
  calibrated prob = softmax(logits / T) with T from `temperature_by_options[bucket]`
  (noul → bucket `noul:2`, T=1.9834). Input layout from `build_sequence`:
  `[CLS] "<type> question: <ins>" [SEP] ([MASK] " <option text>")* [SEP] <state> [SEP]`,
  max_len 512, head_max_len 192, option text ≤ 48 tokens, noul options are
  `false: no, the statement does not hold` / `true: yes, the statement holds`.
- Parity fixtures: `fixtures/laya_parity.json` (CPU fp32 reference: input_ids, markers, raw
  logits, calibrated probs for noul/choice/score cases).
- **Spike result (zero-shot, moon repo, 40 commits):** BM25 MRR 0.480 → BM25⊕Laya RRF 0.591;
  Laya's P≥0.5 is not calibrated on code (precision at P≥0.5 = base rate). PyTorch MPS fp32
  latency 4.7 s for 32×~450-token sequences. ⇒ fine-tune track + latency work are required.
- **Moon**: binary `~/workspaces/tind-repo/moon/target/release/moon` (built). Start:
  `moon --port <p> --dir <dir> --shards 1`. `FT.CREATE idx ON HASH PREFIX 1 <pfx> SCHEMA f TEXT g TAG`
  works; `FT.SEARCH idx "a b"` is **AND-only** (`|` OR unsupported), returns `__bm25_score`.
  BM25 is additive over terms ⇒ emulate OR by pipelining one FT.SEARCH per term and summing.
  TAG filter: `@field:{value}` (single value only). Moon's analyzer lowercases, stems and drops
  stop words; we pre-normalize terms with `laya_core::ident::terms` on both sides.

## Claude Code hook facts (verified empirically with claude 2.1.280, `bench/probe_hook.py`)
- stdin common: `session_id`, `transcript_path`, `cwd`, `hook_event_name`, `permission_mode`, `prompt_id`.
- UserPromptSubmit: `prompt`. Output `{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"..."}}` → reaches the model.
- SessionStart: `source` (`startup`, `resume`, `clear`, `compact`).
- PreToolUse: `tool_name`, `tool_input`, `tool_use_id`. `updatedInput` **replaces** the whole input
  (a partial object fails validation) → send the full original input plus changes. With
  `permissionDecision: "allow"`, a Read rewritten to `offset`/`limit` returned only those lines, and
  PreToolUse `additionalContext` reached the model.

## Contracts (crate `laya-core`, already committed — do not change without the lead)
`Chunk`, `Candidate`, `RankedSpan`, `QueryResult`, `RankMode`, `Error`, traits `Store`, `Scorer`,
`ident::terms`. See `crates/laya-core/src/lib.rs`.

## Crate ownership
| crate | owner track | public API it must expose |
|---|---|---|
| laya-core | lead | (done) |
| laya-parse | A | `detect_lang(path)->Option<Lang>`, `chunk_source(path,&str)->Vec<Chunk>`, `walk_repo(root)->Vec<PathBuf>` (gitignore-aware), `file_hash(&[u8])->String` |
| laya-model | B | `LayaModel::load(dir, DeviceKind)`, `LayaModel::noul(question, states)->Vec<f32>`, `LayaScorer` implementing `Scorer` |
| laya-store | C | `MoonStore` implementing `Store`, `MoonSupervisor` (spawn/health/restart moon) |
| laya-rank | E | `Retriever` (candidate gen + Laya gate + span shaping) over `dyn Store` + `dyn Scorer` |
| laya-cli | lead (after merge) | bin `laya`: `index`, `query`, `daemon`, `hook`, `mcp` |
| spike/, finetune/ | D (Python) | fine-tuned model dir `~/.cache/laya-codex/models/laya-code/` (same layout/keys as laya-base) |

## Engineering rules (all tracks)
- Red/green TDD: write the failing test first, then make it pass. Commit per logical step with
  the commit format in the user's CLAUDE.md (`type(scope): summary`, body, `author: Tin Dang`),
  message written to `tmp/<name>.txt`, `git commit -F`.
- Build/test only your crate: `cargo test -p <crate>` (other crates may be mid-change).
- Design for failure on every IO: timeouts, bounded retries with jitter, circuit breaker, fail-open.
- No `unwrap()` on IO/model paths outside tests. `thiserror` in libs.
- No network at runtime (grammars are statically linked, models are local files).
- Do not edit files owned by another track, `Cargo.lock` conflicts are resolved by the lead.
- Finish with `Status: DONE | DONE_WITH_CONCERNS | BLOCKED` and a short summary of API + tests.
