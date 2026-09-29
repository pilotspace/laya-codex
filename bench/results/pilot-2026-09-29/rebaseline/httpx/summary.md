| metric | baseline | main |
|---|---|---|
| reading_tokens | 2430.8 | 1835.8 |
| injected_tokens | 0.0 | 1745.5 |
| total_input_tokens | 57962.0 | 63000.2 |
| output_tokens | 2185.2 | 1998.2 |
| wall_s | 23.2 | 23.1 |
| num_turns | 7.8 | 7.0 |
| cost_usd | 0.102 | 0.070 |
| recall | 0.500 | 0.750 |
| precision | 0.750 | 0.750 |
| hit_any | 1.000 | 1.000 |
| recall_all_turns | 0.917 | 0.833 |
| **main vs baseline** | reading+injected +47.3% · wall -0.8% · median wall ratio 1.12 · median total-input ratio 1.09 |

| arm | sessions | tool calls/session | Grep | Read | Glob | mcp__laya-codex__search | other | index_started | inject | note_ranged_read |
|---|---|---|---|---|---|---|---|---|---|---|
| baseline | 4 | 5.75 | 4.00 | 1.75 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| main | 4 | 5.00 | 3.00 | 1.25 | 0.00 | 0.75 | 0.00 | 2.00 | 2.00 | 1.25 |
