| metric | baseline | laya | laya-lex |
|---|---|---|---|
| reading_tokens | 3282.3 | 1881.3 | 2128.1 |
| injected_tokens | 0.0 | 2664.2 | 2633.0 |
| total_input_tokens | 56103.9 | 44487.4 | 48758.6 |
| output_tokens | 2071.5 | 1598.8 | 1621.5 |
| wall_s | 23.1 | 20.6 | 19.0 |
| num_turns | 7.5 | 4.3 | 4.8 |
| cost_usd | 0.060 | 0.065 | 0.065 |
| recall | 0.546 | 0.921 | 0.896 |
| precision | 0.697 | 0.741 | 0.749 |
| hit_any | 1.000 | 0.950 | 0.950 |
| recall_all_turns | 0.971 | 0.938 | 0.938 |
| **laya vs baseline** | reading+injected +38.5% · wall -10.8% · median wall ratio 0.90 · median total-input ratio 0.77 |
| **laya-lex vs baseline** | reading+injected +45.1% · wall -17.5% · median wall ratio 0.80 · median total-input ratio 0.81 |

| arm | sessions | tool calls/session | Grep | Read | Glob | mcp__laya-codex__search | other | index_started | inject | note_ranged_read | note_read |
|---|---|---|---|---|---|---|---|---|---|---|---|
| baseline | 20 | 5.45 | 3.25 | 1.80 | 0.40 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| laya | 20 | 2.35 | 0.65 | 1.20 | 0.00 | 0.50 | 0.00 | 2.00 | 2.00 | 0.85 | 0.15 |
| laya-lex | 20 | 2.75 | 0.85 | 1.30 | 0.00 | 0.60 | 0.00 | 2.00 | 2.00 | 0.90 | 0.20 |
