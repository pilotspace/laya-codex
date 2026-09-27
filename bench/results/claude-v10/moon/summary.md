| metric | baseline | laya | laya-lex |
|---|---|---|---|
| reading_tokens | 12858.9 | 8264.9 | 8215.3 |
| injected_tokens | 0.0 | 2002.6 | 2132.1 |
| total_input_tokens | 483325.9 | 313446.6 | 344937.6 |
| output_tokens | 5774.5 | 5050.9 | 5657.5 |
| wall_s | 1360.4 | 743.8 | 125.0 |
| num_turns | 17.9 | 12.3 | 13.0 |
| cost_usd | 0.447 | 0.355 | 0.389 |
| recall | 0.900 | 0.811 | 0.911 |
| precision | 0.301 | 0.314 | 0.436 |
| hit_any | 0.933 | 0.933 | 1.000 |
| recall_all_turns | 0.900 | 0.867 | 0.933 |
| **laya vs baseline** | reading+injected -20.2% · wall -45.3% · median wall ratio 0.70 · median total-input ratio 0.55 |
| **laya-lex vs baseline** | reading+injected -19.5% · wall -90.8% · median wall ratio 0.60 · median total-input ratio 0.64 |

| arm | sessions | tool calls/session | Grep | Read | Glob | mcp__laya-codex__search | other | index_started | inject | note_ranged_read | note_read |
|---|---|---|---|---|---|---|---|---|---|---|---|
| baseline | 15 | 15.80 | 10.20 | 5.47 | 0.13 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| laya | 15 | 10.33 | 4.07 | 4.33 | 0.20 | 1.73 | 0.00 | 2.00 | 2.00 | 3.73 | 0.60 |
| laya-lex | 16 | 10.75 | 3.94 | 4.94 | 0.06 | 1.81 | 0.00 | 2.00 | 2.00 | 4.56 | 0.38 |
