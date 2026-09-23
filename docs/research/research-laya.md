# Deep-Dive: `convaiinnovations/laya` — Is It a Code Reranker?

**Bottom line up front (VERIFIED across all primary sources):** Laya is **not** a code retrieval/reranking/embedding model of any kind. It is a general-purpose, multi-domain "typed decision" classifier (choice / ordinal score / boolean) built on a ModernBERT-large backbone, trained via RL for calibrated probability outputs on tasks like email triage, moderation, intent routing, sentiment, and generic "search relevance." No code, no programming languages, no source-code training data or benchmark is mentioned anywhere in the model card, config, GitHub repo, PyPI description, or eval files. Using it as the ranking brain for a code-span retriever would require re-purposing a mismatched architecture with no code-domain evidence of quality, and its cross-encoder-style cost profile (one full 421M-param forward pass per candidate) makes it structurally unsuited to scoring 50–500 chunks per query at interactive latency on CPU.

---

## 1. What Laya actually is (VERIFIED, source: model card https://huggingface.co/convaiinnovations/laya, README raw, GitHub https://github.com/NandhaKishorM/laya, HF API)

- Self-description: "multilingual, non-autoregressive **System 1 decision model**" — given a `state` (text/email/ticket/JSON) plus a set of **typed questions** (`choice`, `score`, `noul`/boolean), it returns calibrated answer probabilities in one forward pass, no text generation.
- Three checkpoints in one repo (via subfolders), all Apache-2.0:
  | Checkpoint | Encoder | Params | Context |
  |---|---|---|---|
  | `laya` (root) | ModernBERT-large | 421,293,830 (VERIFIED, `safetensors.total` from HF API) | 512 tok (head_max_len 192) |
  | `laya-multilingual` | mmBERT-base | 322M (claimed in README; UNVERIFIED against API — separate model repo) | 1,024 tok |
  | `laya-typed-decisions` | ModernBERT-large, fine-tuned | 421M | 1,024 tok |
- Training: "RLCD" — REINFORCE against strictly proper scoring rules (log/spherical/RPS), group-mean baseline, to incentivize honest calibrated probability reporting. No pretraining corpus disclosed; no paper linked (README explicitly has no academic citation — only a Dev.to write-up and a live demo space).
- License: **Apache 2.0** (VERIFIED, `cardData.license` + PyPI classifiers).
- Downstream task families actually evaluated (VERIFIED, `eval/results.md` + `eval/results.json`, raw-fetched): conversation outcomes, email triage, emotion/tone, inference & fact-checking, instruction-following, intent & routing, moderation & safety, reading comprehension, response-quality scoring, robustness checks, **search relevance** (n=733, acc 0.628 — generic text/doc relevance, not code), sentiment/rating, topic classification. Zero-shot families: emotion/tone, instruction-following, moderation, sentiment. **No "code" task family exists.**
- Org's other models (VERIFIED, HF org page https://huggingface.co/convaiinnovations): Qwen FastAPI wrappers, a travel-agent MCP demo, a kidney-exchange PPO toy model, a "shoeguard safety" SLM, and MedGemma ECG models. Nothing code-retrieval related in the org's history.

## 2. Architecture detail (VERIFIED, `encoder/config.json`, `rl_agent_config.json`, `rl_agent_api.py` raw-fetched)

- Backbone config: `architectures: ["ModernBertForMaskedLM"]`, `model_type: modernbert`, hidden 1024, 28 layers, 16 heads, vocab 50368, `max_position_embeddings: 8192`, RoPE (local/global alternating attention every 3 layers, local window 128). This is the **stock ModernBERT-large encoder config** — no code-specific vocab or architecture modifications visible.
- On top of the encoder sits a **custom decision head** (not a standard `transformers` class): defined in repo files `rl_agent_api.py` / `rl_common.py` (raw-fetched). It works by:
  1. Rendering the question's answer options as `[MASK]`-marked tokens appended/interleaved with the state text (`build_sequence`, `marker_pos`).
  2. Running one ModernBERT forward pass over `state + question + option markers`.
  3. Gathering per-option logits at the marker positions, applying **per-question-type temperature calibration** (`temperature_by_options`, e.g. different temperature for `choice:2` vs `choice:11+` vs `score:3-5`), then softmax → calibrated probabilities.
  4. A second small head (`act`) outputs an "act vs. escalate" probability.
- This means: **it is a cross-encoder-style, single-pass joint encoder over (state, question)** — not a bi-encoder that produces reusable embeddings, and not a lightweight logistic scorer on top of frozen embeddings. Every question against every state is a fresh full 421M-parameter forward pass.
- `transformersInfo.auto_model: "AutoModel"` in the HF API is misleading for practical use — the functional model requires the custom `laya` PyPI package (`pip install laya`) and its `RLAgent`/`Router` classes, not a bare `AutoModel.from_pretrained()` call. Standard `transformers` auto-loading only gets you the raw ModernBERT MLM backbone, not the trained decision head behavior.

## 3. Input / Output (VERIFIED, README + `rl_agent_api.py`)

- **Input:** one `state` string (arbitrary text/email/JSON) + a dict of **typed questions**, each with `type` (`choice`/`score`/`noul`), `instructions`, and `criteria` (label list for `choice`).
- **Output:** per question — for `choice`: selected key + full probability dict over provided labels; for `score`: ordinal distribution; for `noul`: calibrated P(true) ∈ [0,1]; plus a global `act_probability` (confidence-gated "act vs escalate").
- There is **no notion of "rank N candidates against a query and return top-k with continuous relevance scores"** as a first-class primitive — you'd have to synthesize a `choice` question with up to `max_prefixes: 6` options at a time (`rl_agent_config.json`), or issue one `noul` "is this relevant?" question per candidate. Neither maps cleanly onto scoring 50–500 code spans per query in one pass.

## 4. License, weight formats, quantization (VERIFIED, file listing via HF API `siblings` + `safetensors` metadata)

- Formats present: **PyTorch `.safetensors` only** (`model.safetensors`, F16 421,293,827 + 3 F32 params, plus per-subfolder copies for multilingual/typed-decisions). **No ONNX, no GGUF, no TFLite** in the official repo.
- Tokenizer: standard HF fast tokenizer (`tokenizer.json`, `PreTrainedTokenizerFast`, special tokens `[CLS]/[SEP]/[MASK]/[PAD]/[UNK]`) — this part alone is compatible with the Rust `tokenizers` crate.
- No official quantized release. A **third-party, unofficial** MLX FP16 (and a "quantized MLX," ~843MB) conversion exists at `aac6fef/laya-mlx` (VERIFIED via fetch, explicitly labeled "independent port," not from convaiinnovations) — MLX is Apple-Silicon-only and not consumable from Rust without a further bespoke port.

## 5. Benchmarks / latency (VERIFIED, `eval/results.json`, README, GitHub README — **all on Tesla T4 GPU, not CPU, not Apple Silicon**)

| Batch (questions) | `laya` p50 | `laya-multilingual` p50 |
|---|---|---|
| 1 | 38.4–39.5 ms | 32.8 ms |
| 10 | 156–158.6 ms (~15.6 ms/q) | 72.3 ms (~7.2 ms/q) |
| 50 | 721.4–771 ms (~14.4 ms/q) | 337 ms (~6.8 ms/q) |

Throughput cited: 103–332 questions/sec on a single T4. **No CPU or Apple Silicon / Metal benchmark exists anywhere in the sources** (UNVERIFIED — extrapolated in §7 below).

Accuracy (in-task, calibrated): overall 0.753 acc / ECE 0.030 / Brier 0.308, ranging from 0.991 (intent/routing) down to 0.362–0.442 (sentiment, zero-shot) and 0.628 (search relevance). Zero-shot (held-out task families): overall 0.651 acc, ECE 0.204 (much worse calibration off-distribution). Model card itself flags: base checkpoints near-chance zero-shot on typed-decisions (0.362), high-cardinality choice (50+ options) weak (0.425 vs. comparator's 0.870), ships over-confident and needs target-domain temperature recalibration.

## 6. Can it run from Rust? (analysis based on VERIFIED file listing)

| Path | Feasibility | Notes |
|---|---|---|
| `candle` | Backbone yes, head no out-of-box | `candle-transformers` has a ModernBERT implementation, so the 421M encoder itself is portable. The custom decision head (marker-gather + per-qtype temperature + act head) is **not implemented anywhere in candle** and would need hand-porting from `rl_agent_api.py`/`rl_common.py` (a few hundred lines, moderate but non-trivial effort; logic is simple linear/softmax so technically tractable). |
| `ort` (ONNX Runtime) | Needs manual export | No ONNX weights published. Would require `torch.onnx.export` of the **combined custom model** (encoder + head), which is more involved than exporting a stock `transformers` model because the head isn't a standard `nn.Module` registered with HF's export tooling. Achievable but is a real engineering task, not a conversion command. |
| `llama.cpp` bindings | Not applicable | Not a causal decoder-only LM; llama.cpp has no ModernBERT/custom-classifier-head support. No GGUF exists officially. |
| `tokenizers` crate | Yes | `tokenizer.json` is a standard fast tokenizer file, directly loadable. |
| MLX (`aac6fef/laya-mlx`) | Dead end for Rust | Apple-only, unofficial, not a Rust-consumable format. |

**Conclusion: there is no ready-made conversion path.** Getting Laya running from Rust means reimplementing the custom head from source and either (a) porting to candle or (b) hand-exporting to ONNX — both are "build it yourself" efforts on a model whose actual quality on code is unverified.

## 7. Estimated inference cost for N=50–500 code-chunk scoring, Apple Silicon CPU (UNVERIFIED — extrapolated, no direct benchmark exists)

Because the architecture is cross-encoder-style (one full 421M-param, up to 512–1024-token forward pass **per candidate**, not per query), cost scales linearly with N with no reusable embedding/cache:
- Using the T4 GPU numbers as a floor (~14–16 ms/question when batched on GPU), and typical CPU/GPU slowdown factors of 5–15× for a ~400M-param BERT-large-class encoder at ~512 tokens on Apple Silicon CPU (no MPS/Metal acceleration in stock PyTorch CPU path unless deliberately routed through `mps` backend, which the published code does not target), a rough estimate is **~70–250 ms per candidate span on M-series CPU**, even with batching to amortize some overhead.
- N=50 → roughly **3.5–12.5 s** per query; N=500 → roughly **35–125 s** per query. This is almost certainly unacceptable for an interactive "give Claude Code the top-10 spans" workflow.
- Metal/MPS acceleration could reduce this materially, but (a) is unproven for this custom head, (b) still doesn't fix the fundamental O(N) full-forward-pass cost, which is the wrong shape for reranking — a true cross-encoder reranker (e.g., bge-reranker, jina-reranker-v2) has the same O(N) cost profile but is purpose-built and typically much smaller/faster per pair (many are 100–280M param and specifically optimized for short pair-scoring latency), and a bi-encoder embedding model (CodeRankEmbed, nomic-embed-code, jina-code-embeddings) is O(1) per query against precomputed candidate embeddings — orders of magnitude cheaper for N in the hundreds.

## 8. Other convaiinnovations models / related papers (VERIFIED via org page + web search)

- `laya-multilingual`, `laya-typed-decisions` — sibling checkpoints, same architecture family, no code focus.
- Unrelated org models: Qwen FastAPI wrappers, travel-agent MCP, kidney-exchange PPO, shoeguard safety SLM, MedGemma ECG (medical). None touch code retrieval.
- No arXiv/academic paper found for Laya itself. A search hit on an unrelated arXiv paper ("Are Language Models Consequentialist or Deontological Moral Reasoners?") is not connected to this model (UNVERIFIED relevance, excluded).
- Secondary coverage: AI Weekly newsletter blurb and a personal blog (elsolitario.org) both just restate the model card numbers — no independent evaluation, no code-domain testing, in either.

## 9. Gaps, risks, and red flags

- **Not code-aware, no code-domain evidence.** No code training data, no code benchmark, no programming-language mention anywhere. Using it for code-span ranking is applying a general decision classifier to a domain it has never been shown to handle.
- **Suspicious popularity signal.** HF API shows `likes: 2364` but **`downloads: 0`** (VERIFIED, API JSON, `createdAt: 2026-09-18`, `lastModified: 2026-09-20` — repo is 5 days old at time of research). A 5-day-old model with zero downloads but 2,364 likes is a strong anomaly (org-page fetch separately reported "2.37k downloads" — inconsistent with the direct API's `downloads: 0`, itself worth flagging as unreliable/uncached metadata this early). Treat popularity as **unverified/likely inflated**, not evidence of production maturity.
- **No maturity signal:** no independent adopters, no production case studies, no academic peer review, custom untested (by us) Python-only reference implementation.
- **Architecture mismatch, not just "weak at code":** even if Laya were retrained on code, its `choice`/`score`/`noul` question interface with `max_prefixes: 6` options doesn't naturally express "rank top-10 of N=50-500 chunks with confidence ≥ 0.5" — you'd be fighting the API shape as much as the domain.
- **Self-reported weaknesses** (from the model card itself, so credible): near-chance zero-shot on unseen task types, poor high-cardinality choice accuracy, weakest at ordinal `score` questions, ships over-confident pre-calibration.

## 10. Recommendation

**Do not use Laya for this purpose.** It fails architectural fit (cross-encoder-style full-model forward pass per candidate, wrong I/O shape for top-k ranking), has zero code-domain evidence, has no CPU/Apple Silicon path (no ONNX/GGUF, custom head needs hand-porting to any Rust runtime), and its popularity/maturity signals are unverifiable or contradictory this early in its life (5 days old, likes/downloads mismatch).

For a Rust code indexer surfacing top-10 spans to Claude Code, brief better-fit alternatives (not deep-dived here — flagged as follow-up):
- **Bi-encoder embedding + ANN, then optional light rerank** is the right shape for N=50–500 (O(1) per query against precomputed chunk embeddings): `nomic-embed-code`, `CodeRankEmbed`, or `jina-code-embeddings` — all purpose-trained on code, several ship ONNX/ orted variants usable via `ort` from Rust.
- If a cross-encoder rerank stage is wanted for the final top-k: `jina-reranker-v2-base-multilingual` (has code-capable variants) or `bge-reranker-v2-m3` — both smaller, purpose-built, better documented ONNX export paths than Laya.
- These were **not deep-dived** in this report (out of scope per task instructions — "brief only"); a follow-up research pass would be needed before committing to one, particularly to re-verify current ONNX/candle compatibility and license terms.

## Unresolved / not covered by this research

- No direct CPU or Apple Silicon/Metal benchmark exists for Laya from the vendor — §7 numbers are extrapolated, not measured. If Laya were ever reconsidered, this would need an actual local benchmark.
- Did not deep-dive the `laya-multilingual` or `laya-typed-decisions` checkpoints' eval files individually beyond what the README/GitHub summarized.
- Did not verify the alternatives (jina-reranker-v2, bge-reranker, CodeRankEmbed, nomic-embed-code) against their own primary sources — only named per task scope ("brief only"); their claims here are from general knowledge, not re-verified in this pass.
- Could not resolve the downloads (0 via API) vs. likes (2,364) vs. org-page-reported ("2.37k downloads") discrepancy — flagged as a data-quality risk rather than resolved.
