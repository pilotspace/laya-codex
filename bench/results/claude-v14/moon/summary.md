| metric | baseline | gate | laya |
|---|---|---|---|
| reading_tokens | 3047.5 | 1291.3 | 1338.9 |
| injected_tokens | 0.0 | 1977.2 | 2088.9 |
| total_input_tokens | 97802.9 | 63719.4 | 66034.3 |
| output_tokens | 2443.9 | 1824.6 | 1835.7 |
| wall_s | 22.5 | 18.7 | 20.0 |
| num_turns | 8.2 | 4.0 | 4.3 |
| cost_usd | 0.101 | 0.077 | 0.061 |
| recall | 0.846 | 0.887 | 0.846 |
| precision | 0.582 | 0.484 | 0.487 |
| hit_any | 1.000 | 1.000 | 1.000 |
| recall_all_turns | 0.929 | 0.912 | 0.908 |
| **gate vs baseline** | reading+injected +7.3% · wall -16.9% · median wall ratio 0.87 · median total-input ratio 0.70 |
| **laya vs baseline** | reading+injected +12.5% · wall -11.2% · median wall ratio 0.82 · median total-input ratio 0.58 |

| arm | sessions | tool calls/session | Grep | Read | Glob | mcp__laya-codex__search | other | index_started | inject | low_confidence | note_ranged_read | note_read |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| baseline | 20 | 6.15 | 4.40 | 1.55 | 0.20 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| gate | 20 | 2.00 | 0.80 | 0.40 | 0.00 | 0.80 | 0.00 | 1.00 | 1.90 | 0.10 | 0.40 | 0.00 |
| laya | 20 | 2.35 | 0.90 | 0.45 | 0.00 | 1.00 | 0.00 | 1.00 | 2.00 | 0.00 | 0.30 | 0.05 |
