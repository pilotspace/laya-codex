| metric | baseline | rc | v040 |
|---|---|---|---|
| reading_tokens | 2478.5 | 1397.0 | 1792.3 |
| injected_tokens | 0.0 | 2573.1 | 2444.7 |
| total_input_tokens | 43472.2 | 34708.2 | 36357.1 |
| output_tokens | 1887.0 | 1477.2 | 1515.2 |
| wall_s | 21.2 | 16.2 | 16.9 |
| num_turns | 7.2 | 4.5 | 4.3 |
| cost_usd | 0.047 | 0.045 | 0.047 |
| recall | 0.767 | 0.942 | 0.942 |
| precision | 0.942 | 0.920 | 0.845 |
| hit_any | 1.000 | 1.000 | 1.000 |
| recall_all_turns | 0.983 | 0.983 | 0.983 |
| **rc vs baseline** | reading+injected +60.2% · wall -23.4% · median wall ratio 0.74 · median total-input ratio 0.79 |
| **v040 vs baseline** | reading+injected +71.0% · wall -20.1% · median wall ratio 0.81 · median total-input ratio 0.82 |

| arm | sessions | tool calls/session | Grep | Read | Glob | mcp__laya-codex__search | other | index_started | inject | note_ranged_read |
|---|---|---|---|---|---|---|---|---|---|---|
| baseline | 20 | 5.25 | 3.95 | 1.20 | 0.10 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| rc | 20 | 2.45 | 1.45 | 0.50 | 0.05 | 0.45 | 0.00 | 1.00 | 2.00 | 0.50 |
| v040 | 20 | 2.30 | 1.30 | 0.45 | 0.00 | 0.55 | 0.00 | 1.00 | 2.00 | 0.45 |
