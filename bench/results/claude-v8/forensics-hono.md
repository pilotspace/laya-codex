paired tasks: 20; arms: baseline, laya-adaptive, laya-lex

### Reading tokens / calls / turns / seconds per class (mean per task)

| arm | class | tokens | % of reading | calls | turns | seconds |
|---|---|---|---|---|---|---|
| baseline | a_redundant | 523 | 11.6% | 0.60 | 0.45 | 2.3 |
| baseline | d_expansion | 840 | 18.6% | 0.65 | 0.60 | 1.7 |
| baseline | b_map_hit | 280 | 6.2% | 0.20 | 0.15 | 0.4 |
| baseline | b2_map_file | 1079 | 23.8% | 0.70 | 0.60 | 1.7 |
| baseline | c_gold_miss | 598 | 13.2% | 0.70 | 0.55 | 2.3 |
| baseline | f_other | 272 | 6.0% | 0.25 | 0.20 | 1.1 |
| baseline | e_grep_locate | 344 | 7.6% | 1.90 | 1.05 | 3.6 |
| baseline | g_grep_explore | 590 | 13.0% | 4.15 | 2.70 | 9.0 |
| baseline | answer | 0 | 0.0% | 0.00 | 2.00 | 17.0 |
| baseline | **total** | 4525 | 100% | | 8.30 api calls | 41.1 wall |
| laya-adaptive | a_redundant | 141 | 5.8% | 0.10 | 0.10 | 0.0 |
| laya-adaptive | d_expansion | 383 | 15.8% | 0.60 | 0.55 | 2.0 |
| laya-adaptive | b_map_hit | 228 | 9.4% | 0.25 | 0.25 | 0.8 |
| laya-adaptive | b2_map_file | 554 | 22.9% | 0.65 | 0.60 | 0.9 |
| laya-adaptive | c_gold_miss | 565 | 23.3% | 0.65 | 0.60 | 2.0 |
| laya-adaptive | f_other | 117 | 4.8% | 0.25 | 0.25 | 1.0 |
| laya-adaptive | e_grep_locate | 273 | 11.3% | 1.50 | 1.25 | 5.8 |
| laya-adaptive | g_grep_explore | 162 | 6.7% | 1.60 | 1.05 | 5.1 |
| laya-adaptive | answer | 0 | 0.0% | 0.00 | 2.00 | 18.5 |
| laya-adaptive | **total** | 2422 | 100% | | 6.65 api calls | 38.8 wall |
| laya-lex | a_redundant | 164 | 5.8% | 0.15 | 0.15 | 0.2 |
| laya-lex | d_expansion | 463 | 16.5% | 0.80 | 0.75 | 1.4 |
| laya-lex | b_map_hit | 286 | 10.2% | 0.30 | 0.30 | 0.8 |
| laya-lex | b2_map_file | 813 | 29.0% | 0.90 | 0.90 | 2.6 |
| laya-lex | c_gold_miss | 341 | 12.1% | 0.35 | 0.35 | 0.8 |
| laya-lex | f_other | 174 | 6.2% | 0.40 | 0.25 | 0.7 |
| laya-lex | e_grep_locate | 289 | 10.3% | 1.85 | 1.50 | 10.6 |
| laya-lex | g_grep_explore | 278 | 9.9% | 1.85 | 1.45 | 6.9 |
| laya-lex | answer | 0 | 0.0% | 0.00 | 2.00 | 17.2 |
| laya-lex | **total** | 2808 | 100% | | 7.65 api calls | 43.1 wall |

### Gold coverage of the injection (gold files, all tasks)

| arm | inlined | map only | related only | absent | tasks with 0 gold injected | FILES answer fully inside injection | ...inside inlined files | answer recall |
|---|---|---|---|---|---|---|---|---|
| laya-adaptive | 26 (57%) | 8 | 3 | 9 (20%) | 2/20 | 14/20 | 5/20 | 0.829 |
| laya-lex | 28 (61%) | 6 | 4 | 8 (17%) | 1/20 | 12/20 | 6/20 | 0.854 |

### Discovery timing (API-call index; 0 = gold already inlined)

| arm | first gold Read | last new gold file | api calls | tail calls after last gold | tail seconds | tail reading tok | tail share of wall |
|---|---|---|---|---|---|---|---|
| baseline | 2.4 (11/20) | 2.1 | 8.3 | 6.2 | 33.0 | 2954 | 80% |
| laya-adaptive | 1.4 (11/20) | 1.6 | 6.7 | 5.1 | 28.8 | 1308 | 74% |
| laya-lex | 2.2 (10/20) | 2.0 | 7.7 | 5.6 | 31.1 | 1637 | 72% |
