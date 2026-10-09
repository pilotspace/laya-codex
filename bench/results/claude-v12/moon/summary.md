| metric | baseline | laya | laya-lex |
|---|---|---|---|
| reading_tokens | 6839.8 | 2600.2 | 2270.9 |
| injected_tokens | 0.0 | 3277.2 | 3396.4 |
| total_input_tokens | 121760.8 | 69445.1 | 66803.5 |
| output_tokens | 2954.1 | 1931.0 | 1930.5 |
| wall_s | 28.5 | 21.1 | 19.7 |
| num_turns | 8.7 | 3.7 | 3.6 |
| cost_usd | 0.128 | 0.135 | 0.131 |
| recall | 0.988 | 0.896 | 0.908 |
| precision | 0.352 | 0.356 | 0.350 |
| hit_any | 1.000 | 1.000 | 1.000 |
| recall_all_turns | 1.000 | 0.975 | 0.975 |
| **laya vs baseline** | reading+injected -14.1% · wall -26.1% · median wall ratio 0.72 · median total-input ratio 0.52 |
| **laya-lex vs baseline** | reading+injected -17.1% · wall -31.1% · median wall ratio 0.68 · median total-input ratio 0.50 |

| arm | sessions | tool calls/session | Grep | Read | Glob | mcp__laya-codex__search | other | index_started | inject | note_ranged_read |
|---|---|---|---|---|---|---|---|---|---|---|
| baseline | 20 | 6.70 | 4.65 | 2.00 | 0.05 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| laya | 20 | 1.70 | 0.30 | 0.40 | 0.00 | 1.00 | 0.00 | 2.00 | 2.00 | 0.40 |
| laya-lex | 20 | 1.60 | 0.35 | 0.40 | 0.00 | 0.85 | 0.00 | 2.00 | 2.00 | 0.40 |
