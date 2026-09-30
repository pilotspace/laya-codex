| metric | baseline | laya | laya-lex |
|---|---|---|---|
| reading_tokens | 1878.0 | 1039.0 | 1177.8 |
| injected_tokens | 0.0 | 1775.9 | 1707.5 |
| total_input_tokens | 52923.1 | 46267.8 | 47952.0 |
| output_tokens | 1894.5 | 1595.5 | 1662.2 |
| wall_s | 23.2 | 20.5 | 20.3 |
| num_turns | 6.8 | 4.8 | 5.0 |
| cost_usd | 0.054 | 0.061 | 0.064 |
| recall | 0.617 | 0.875 | 0.833 |
| precision | 0.750 | 0.608 | 0.575 |
| hit_any | 0.900 | 0.950 | 0.900 |
| recall_all_turns | 0.883 | 0.917 | 0.867 |
| **laya vs baseline** | reading+injected +49.9% · wall -11.5% · median wall ratio 0.88 · median total-input ratio 0.90 |
| **laya-lex vs baseline** | reading+injected +53.6% · wall -12.2% · median wall ratio 0.95 · median total-input ratio 0.99 |

| arm | sessions | tool calls/session | Grep | Read | Glob | mcp__laya-codex__search | other | index_started | inject | note_ranged_read |
|---|---|---|---|---|---|---|---|---|---|---|
| baseline | 20 | 4.80 | 3.40 | 1.40 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| laya | 20 | 2.80 | 1.35 | 0.85 | 0.00 | 0.60 | 0.00 | 2.00 | 2.00 | 0.85 |
| laya-lex | 20 | 2.95 | 1.45 | 0.80 | 0.05 | 0.65 | 0.00 | 2.00 | 2.00 | 0.80 |
