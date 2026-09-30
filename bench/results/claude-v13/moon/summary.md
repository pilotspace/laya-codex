| metric | baseline | laya |
|---|---|---|
| reading_tokens | 4158.1 | 1164.0 |
| injected_tokens | 0.0 | 2052.6 |
| total_input_tokens | 117144.6 | 65897.6 |
| output_tokens | 2769.7 | 1841.4 |
| wall_s | 26.8 | 19.1 |
| num_turns | 8.8 | 3.6 |
| cost_usd | 0.123 | 0.094 |
| recall | 0.938 | 0.908 |
| precision | 0.351 | 0.381 |
| hit_any | 1.000 | 1.000 |
| recall_all_turns | 0.975 | 0.975 |
| **laya vs baseline** | reading+injected -22.6% · wall -28.7% · median wall ratio 0.69 · median total-input ratio 0.53 |

| arm | sessions | tool calls/session | Grep | Read | Glob | mcp__laya-codex__search | other | index_started | inject | note_ranged_read |
|---|---|---|---|---|---|---|---|---|---|---|
| baseline | 20 | 6.75 | 5.20 | 1.45 | 0.10 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| laya | 20 | 1.60 | 0.25 | 0.30 | 0.00 | 1.05 | 0.00 | 1.00 | 2.00 | 0.30 |
