| metric | baseline | laya | laya-lex |
|---|---|---|---|
| reading_tokens | 3145.8 | 1967.3 | 2467.8 |
| injected_tokens | 0.0 | 1674.0 | 1630.2 |
| total_input_tokens | 72826.8 | 60770.3 | 71407.4 |
| output_tokens | 2826.9 | 2174.7 | 2421.3 |
| wall_s | 31.7 | 26.7 | 32.4 |
| num_turns | 9.3 | 5.8 | 6.7 |
| cost_usd | 0.123 | 0.116 | 0.132 |
| recall | 0.717 | 0.946 | 0.983 |
| precision | 0.728 | 0.767 | 0.812 |
| hit_any | 1.000 | 1.000 | 1.000 |
| recall_all_turns | 0.988 | 0.988 | 1.000 |
| **laya vs baseline** | reading+injected +15.7% · wall -15.9% · median wall ratio 0.86 · median total-input ratio 0.81 |
| **laya-lex vs baseline** | reading+injected +30.3% · wall +2.3% · median wall ratio 0.88 · median total-input ratio 0.97 |

| arm | sessions | tool calls/session | Grep | Read | Glob | mcp__laya-codex__search | other | index_started | inject | note_ranged_read | note_read |
|---|---|---|---|---|---|---|---|---|---|---|---|
| baseline | 20 | 7.30 | 3.85 | 2.95 | 0.50 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| laya | 20 | 3.85 | 1.55 | 1.65 | 0.00 | 0.65 | 0.00 | 2.00 | 2.00 | 1.45 | 0.20 |
| laya-lex | 20 | 4.60 | 1.45 | 2.40 | 0.00 | 0.75 | 0.00 | 2.00 | 2.00 | 2.05 | 0.35 |
