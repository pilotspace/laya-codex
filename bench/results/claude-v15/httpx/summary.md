| metric | baseline | rc | v040 |
|---|---|---|---|
| reading_tokens | 2511.8 | 1432.3 | 1358.2 |
| injected_tokens | 0.0 | 2761.3 | 2885.8 |
| total_input_tokens | 41002.2 | 38883.9 | 42354.8 |
| output_tokens | 1731.5 | 1561.2 | 1575.1 |
| wall_s | 20.2 | 17.6 | 18.4 |
| num_turns | 6.5 | 4.7 | 5.1 |
| cost_usd | 0.044 | 0.047 | 0.049 |
| recall | 0.600 | 0.758 | 0.642 |
| precision | 0.775 | 0.637 | 0.549 |
| hit_any | 0.950 | 0.900 | 0.850 |
| recall_all_turns | 0.871 | 0.871 | 0.808 |
| **rc vs baseline** | reading+injected +67.0% · wall -12.9% · median wall ratio 0.88 · median total-input ratio 0.91 |
| **v040 vs baseline** | reading+injected +69.0% · wall -9.1% · median wall ratio 0.94 · median total-input ratio 1.09 |

| arm | sessions | tool calls/session | Grep | Read | Glob | mcp__laya-codex__search | other | index_started | inject | note_ranged_read | note_read |
|---|---|---|---|---|---|---|---|---|---|---|---|
| baseline | 20 | 4.55 | 3.15 | 1.25 | 0.15 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| rc | 20 | 2.65 | 1.60 | 0.40 | 0.00 | 0.65 | 0.00 | 1.00 | 2.00 | 0.35 | 0.05 |
| v040 | 20 | 3.10 | 1.70 | 0.75 | 0.05 | 0.60 | 0.00 | 1.00 | 2.00 | 0.55 | 0.10 |
