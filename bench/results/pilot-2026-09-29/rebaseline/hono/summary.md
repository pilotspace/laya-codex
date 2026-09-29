| metric | baseline | main |
|---|---|---|
| reading_tokens | 1638.5 | 1047.8 |
| injected_tokens | 0.0 | 1387.8 |
| total_input_tokens | 51226.0 | 38510.2 |
| output_tokens | 1759.8 | 1319.5 |
| wall_s | 22.6 | 17.0 |
| num_turns | 6.2 | 3.8 |
| cost_usd | 0.088 | 0.052 |
| recall | 0.458 | 1.000 |
| precision | 0.708 | 0.854 |
| hit_any | 1.000 | 1.000 |
| recall_all_turns | 1.000 | 1.000 |
| **main vs baseline** | reading+injected +48.6% · wall -24.7% · median wall ratio 0.80 · median total-input ratio 0.83 |

| arm | sessions | tool calls/session | Grep | Read | Glob | mcp__laya-codex__search | other | index_started | inject | note_ranged_read | note_read |
|---|---|---|---|---|---|---|---|---|---|---|---|
| baseline | 4 | 4.25 | 2.75 | 1.25 | 0.25 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| main | 4 | 1.75 | 0.25 | 0.50 | 0.00 | 1.00 | 0.00 | 2.00 | 2.00 | 0.25 | 0.25 |
