| metric | laya | laya-ws6 |
|---|---|---|
| reading_tokens | 8523.2 | 10829.1 |
| injected_tokens | 1994.3 | 1475.3 |
| total_input_tokens | 325603.3 | 410368.7 |
| output_tokens | 5425.8 | 5579.3 |
| wall_s | 67.1 | 68.2 |
| num_turns | 12.7 | 14.8 |
| cost_usd | 0.372 | 0.424 |
| recall | 0.898 | 0.898 |
| precision | 0.387 | 0.365 |
| hit_any | 1.000 | 1.000 |
| recall_all_turns | 0.926 | 0.926 |
| **laya-ws6 vs laya** | reading+injected +17.0% · wall +1.5% · median wall ratio 0.96 · median total-input ratio 1.25 |

| arm | sessions | tool calls/session | Grep | Read | Glob | mcp__laya-codex__search | other | index_started | inject | note_ranged_read | note_read |
|---|---|---|---|---|---|---|---|---|---|---|---|
| laya | 18 | 10.67 | 3.50 | 4.89 | 0.00 | 2.28 | 0.00 | 2.00 | 2.00 | 4.44 | 0.44 |
| laya-ws6 | 18 | 12.78 | 4.11 | 5.94 | 0.22 | 2.50 | 0.00 | 2.00 | 2.00 | 5.17 | 0.78 |
