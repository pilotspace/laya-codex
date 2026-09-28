| metric | laya | laya-ws6 |
|---|---|---|
| reading_tokens | 2947.8 | 3204.3 |
| injected_tokens | 1711.1 | 1224.9 |
| total_input_tokens | 154521.6 | 157235.3 |
| output_tokens | 2974.1 | 2833.3 |
| wall_s | 56.6 | 51.8 |
| num_turns | 8.9 | 8.2 |
| cost_usd | 0.162 | 0.159 |
| recall | 0.816 | 0.798 |
| precision | 0.649 | 0.605 |
| hit_any | 0.895 | 0.895 |
| recall_all_turns | 0.860 | 0.860 |
| **laya-ws6 vs laya** | reading+injected -4.9% · wall -8.4% · median wall ratio 0.99 · median total-input ratio 1.01 |

| arm | sessions | tool calls/session | Grep | Read | Glob | mcp__laya-codex__search | other | index_started | inject | note_ranged_read | note_read |
|---|---|---|---|---|---|---|---|---|---|---|---|
| laya | 19 | 6.95 | 2.21 | 2.89 | 0.26 | 1.58 | 0.00 | 2.00 | 2.00 | 2.68 | 0.21 |
| laya-ws6 | 19 | 6.16 | 1.95 | 2.95 | 0.00 | 1.26 | 0.00 | 2.00 | 2.00 | 2.79 | 0.16 |
