| metric | baseline | laya |
|---|---|---|
| reading_tokens | 3476.8 | 1891.0 |
| injected_tokens | 0.0 | 2664.2 |
| total_input_tokens | 59825.7 | 44087.3 |
| output_tokens | 2150.4 | 1590.2 |
| wall_s | 23.0 | 18.3 |
| num_turns | 8.2 | 4.5 |
| cost_usd | 0.065 | 0.057 |
| recall | 0.575 | 0.908 |
| precision | 0.742 | 0.742 |
| hit_any | 1.000 | 0.950 |
| recall_all_turns | 0.971 | 0.950 |
| **laya vs baseline** | reading+injected +31.0% · wall -20.5% · median wall ratio 0.79 · median total-input ratio 0.71 |

| arm | sessions | tool calls/session | Grep | Read | Glob | mcp__laya-codex__search | other | index_started | inject | note_ranged_read | note_read |
|---|---|---|---|---|---|---|---|---|---|---|---|
| baseline | 20 | 6.20 | 4.05 | 1.80 | 0.35 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| laya | 20 | 2.45 | 0.85 | 1.10 | 0.00 | 0.50 | 0.00 | 1.00 | 2.00 | 0.90 | 0.20 |
