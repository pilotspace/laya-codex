| metric | laya | laya-ws6 |
|---|---|---|
| reading_tokens | 4222.2 | 5409.6 |
| injected_tokens | 1570.2 | 1122.2 |
| total_input_tokens | 142799.7 | 255638.5 |
| output_tokens | 3528.0 | 6796.5 |
| wall_s | 47.6 | 83.7 |
| num_turns | 8.3 | 11.1 |
| cost_usd | 0.183 | 0.318 |
| recall | 0.764 | 0.889 |
| precision | 0.694 | 0.750 |
| hit_any | 0.917 | 1.000 |
| recall_all_turns | 0.917 | 1.000 |
| **laya-ws6 vs laya** | reading+injected +12.8% · wall +75.9% · median wall ratio 1.59 · median total-input ratio 1.59 |

| arm | sessions | tool calls/session | Grep | Read | Glob | mcp__laya-codex__search | other | index_started | inject | note_ranged_read | note_read |
|---|---|---|---|---|---|---|---|---|---|---|---|
| laya | 12 | 6.33 | 1.33 | 3.92 | 0.08 | 1.00 | 0.00 | 2.00 | 2.00 | 3.00 | 0.92 |
| laya-ws6 | 12 | 9.08 | 2.08 | 6.00 | 0.08 | 0.92 | 0.00 | 2.00 | 2.00 | 5.08 | 0.92 |
