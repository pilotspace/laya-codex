| metric | baseline | laya |
|---|---|---|
| reading_tokens | 2894.8 | 1760.4 |
| injected_tokens | 0.0 | 2835.9 |
| total_input_tokens | 52895.4 | 47025.8 |
| output_tokens | 1813.0 | 1579.2 |
| wall_s | 19.7 | 18.9 |
| num_turns | 6.7 | 4.8 |
| cost_usd | 0.055 | 0.055 |
| recall | 0.667 | 0.825 |
| precision | 0.783 | 0.588 |
| hit_any | 0.950 | 0.900 |
| recall_all_turns | 0.917 | 0.867 |
| **laya vs baseline** | reading+injected +58.8% · wall -4.5% · median wall ratio 1.02 · median total-input ratio 0.98 |

| arm | sessions | tool calls/session | Grep | Read | Glob | mcp__laya-codex__search | other | index_started | inject | note_ranged_read | note_read |
|---|---|---|---|---|---|---|---|---|---|---|---|
| baseline | 20 | 4.70 | 3.30 | 1.30 | 0.10 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| laya | 20 | 2.85 | 1.20 | 0.90 | 0.00 | 0.75 | 0.00 | 1.00 | 2.00 | 0.85 | 0.05 |
