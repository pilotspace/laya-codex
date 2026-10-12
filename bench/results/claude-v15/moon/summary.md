| metric | baseline | rc | v040 |
|---|---|---|---|
| reading_tokens | 5319.4 | 2110.8 | 2238.6 |
| injected_tokens | 0.0 | 3356.8 | 3290.2 |
| total_input_tokens | 100591.8 | 66646.4 | 68821.4 |
| output_tokens | 2558.6 | 1884.2 | 1924.7 |
| wall_s | 23.9 | 21.6 | 19.0 |
| num_turns | 8.6 | 4.3 | 4.3 |
| cost_usd | 0.106 | 0.092 | 0.091 |
| recall | 0.833 | 0.833 | 0.846 |
| precision | 0.570 | 0.497 | 0.449 |
| hit_any | 0.950 | 1.000 | 1.000 |
| recall_all_turns | 0.904 | 0.887 | 0.883 |
| **rc vs baseline** | reading+injected +2.8% · wall -9.8% · median wall ratio 0.74 · median total-input ratio 0.67 |
| **v040 vs baseline** | reading+injected +3.9% · wall -20.5% · median wall ratio 0.81 · median total-input ratio 0.71 |

| arm | sessions | tool calls/session | Grep | Read | Glob | mcp__laya-codex__search | other | index_started | inject | note_ranged_read |
|---|---|---|---|---|---|---|---|---|---|---|
| baseline | 20 | 6.60 | 4.80 | 1.75 | 0.05 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| rc | 20 | 2.30 | 1.15 | 0.30 | 0.00 | 0.85 | 0.00 | 1.00 | 2.00 | 0.30 |
| v040 | 20 | 2.35 | 0.95 | 0.50 | 0.00 | 0.90 | 0.00 | 1.00 | 2.00 | 0.50 |
