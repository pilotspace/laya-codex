| metric | baseline | gate | laya |
|---|---|---|---|
| reading_tokens | 2426.1 | 1343.3 | 1217.7 |
| injected_tokens | 0.0 | 2441.2 | 2903.8 |
| total_input_tokens | 39825.2 | 39758.9 | 37617.4 |
| output_tokens | 1730.7 | 1549.2 | 1487.3 |
| wall_s | 19.8 | 18.2 | 17.7 |
| num_turns | 6.6 | 5.3 | 4.6 |
| cost_usd | 0.043 | 0.042 | 0.040 |
| recall | 0.550 | 0.704 | 0.696 |
| precision | 0.700 | 0.637 | 0.544 |
| hit_any | 0.900 | 0.900 | 0.850 |
| recall_all_turns | 0.792 | 0.871 | 0.821 |
| **gate vs baseline** | reading+injected +56.0% · wall -8.1% · median wall ratio 0.94 · median total-input ratio 1.03 |
| **laya vs baseline** | reading+injected +69.9% · wall -10.7% · median wall ratio 0.91 · median total-input ratio 1.05 |

| arm | sessions | tool calls/session | Grep | Read | Glob | mcp__laya-codex__search | other | index_started | inject | low_confidence | note_ranged_read | note_read |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| baseline | 20 | 4.60 | 3.40 | 1.10 | 0.10 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| gate | 20 | 3.30 | 1.80 | 0.70 | 0.10 | 0.70 | 0.00 | 1.00 | 1.60 | 0.40 | 0.60 | 0.10 |
| laya | 20 | 2.60 | 1.50 | 0.55 | 0.00 | 0.55 | 0.00 | 1.00 | 2.00 | 0.00 | 0.55 | 0.00 |
