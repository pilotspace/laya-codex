| metric | baseline | laya | laya-lex |
|---|---|---|---|
| reading_tokens | 2892.2 | 1786.8 | 2037.4 |
| injected_tokens | 0.0 | 1735.5 | 1672.7 |
| total_input_tokens | 67499.9 | 60345.4 | 61911.6 |
| output_tokens | 2505.3 | 2158.5 | 2201.6 |
| wall_s | 28.3 | 26.5 | 25.5 |
| num_turns | 8.8 | 6.2 | 6.5 |
| cost_usd | 0.113 | 0.113 | 0.112 |
| recall | 0.692 | 0.850 | 0.833 |
| precision | 0.750 | 0.579 | 0.557 |
| hit_any | 0.950 | 0.900 | 0.900 |
| recall_all_turns | 0.917 | 0.883 | 0.883 |
| **laya vs baseline** | reading+injected +21.8% · wall -6.4% · median wall ratio 0.98 · median total-input ratio 0.97 |
| **laya-lex vs baseline** | reading+injected +28.3% · wall -9.9% · median wall ratio 0.93 · median total-input ratio 0.89 |

| arm | sessions | tool calls/session | Grep | Read | Glob | mcp__laya-codex__search | other | index_started | inject | note_ranged_read | note_read |
|---|---|---|---|---|---|---|---|---|---|---|---|
| baseline | 20 | 6.85 | 3.90 | 2.80 | 0.15 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| laya | 20 | 4.25 | 1.90 | 1.60 | 0.00 | 0.75 | 0.00 | 2.00 | 2.00 | 1.55 | 0.05 |
| laya-lex | 20 | 4.45 | 1.90 | 1.65 | 0.05 | 0.85 | 0.00 | 2.00 | 2.00 | 1.65 | 0.00 |
