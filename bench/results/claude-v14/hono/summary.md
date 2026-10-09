| metric | baseline | gate | laya |
|---|---|---|---|
| reading_tokens | 1778.0 | 1738.4 | 1590.0 |
| injected_tokens | 0.0 | 2077.8 | 2440.9 |
| total_input_tokens | 37882.5 | 34485.8 | 34655.4 |
| output_tokens | 1725.3 | 1501.1 | 1469.3 |
| wall_s | 19.2 | 18.5 | 16.3 |
| num_turns | 6.5 | 4.6 | 4.3 |
| cost_usd | 0.041 | 0.041 | 0.039 |
| recall | 0.792 | 0.942 | 0.942 |
| precision | 0.958 | 0.883 | 0.863 |
| hit_any | 1.000 | 1.000 | 1.000 |
| recall_all_turns | 0.983 | 0.983 | 0.983 |
| **gate vs baseline** | reading+injected +114.6% · wall -3.4% · median wall ratio 0.88 · median total-input ratio 0.89 |
| **laya vs baseline** | reading+injected +126.7% · wall -14.8% · median wall ratio 0.86 · median total-input ratio 0.92 |

| arm | sessions | tool calls/session | Grep | Read | Glob | mcp__laya-codex__search | other | index_started | inject | low_confidence | note_ranged_read |
|---|---|---|---|---|---|---|---|---|---|---|---|
| baseline | 20 | 4.55 | 3.65 | 0.85 | 0.05 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| gate | 20 | 2.60 | 1.65 | 0.50 | 0.00 | 0.45 | 0.00 | 1.00 | 1.60 | 0.40 | 0.50 |
| laya | 20 | 2.35 | 1.50 | 0.45 | 0.00 | 0.40 | 0.00 | 1.00 | 2.00 | 0.00 | 0.45 |
