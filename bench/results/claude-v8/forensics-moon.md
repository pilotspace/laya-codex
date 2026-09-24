paired tasks: 20; arms: baseline, laya-adaptive, laya-lex

### Reading tokens / calls / turns / seconds per class (mean per task)

| arm | class | tokens | % of reading | calls | turns | seconds |
|---|---|---|---|---|---|---|
| baseline | a_redundant | 948 | 6.8% | 0.50 | 0.30 | 1.4 |
| baseline | d_expansion | 2012 | 14.3% | 1.15 | 0.70 | 2.5 |
| baseline | b_map_hit | 634 | 4.5% | 0.30 | 0.10 | 0.5 |
| baseline | b2_map_file | 4117 | 29.3% | 0.95 | 0.90 | 3.3 |
| baseline | c_gold_miss | 437 | 3.1% | 0.45 | 0.35 | 1.3 |
| baseline | f_other | 3262 | 23.2% | 2.25 | 1.85 | 8.3 |
| baseline | e_grep_locate | 1464 | 10.4% | 5.35 | 2.85 | 11.0 |
| baseline | g_grep_explore | 1157 | 8.2% | 5.80 | 2.45 | 9.2 |
| baseline | answer | 0 | 0.0% | 0.00 | 2.00 | 17.9 |
| baseline | **total** | 14030 | 100% | | 11.50 api calls | 57.8 wall |
| laya-adaptive | a_redundant | 529 | 6.0% | 0.30 | 0.20 | 0.5 |
| laya-adaptive | d_expansion | 1380 | 15.6% | 1.20 | 0.90 | 2.3 |
| laya-adaptive | b_map_hit | 498 | 5.6% | 0.50 | 0.45 | 1.4 |
| laya-adaptive | b2_map_file | 2216 | 25.1% | 1.50 | 1.35 | 5.7 |
| laya-adaptive | c_gold_miss | 645 | 7.3% | 0.40 | 0.25 | 0.8 |
| laya-adaptive | f_other | 1326 | 15.0% | 1.30 | 1.05 | 4.8 |
| laya-adaptive | e_grep_locate | 1344 | 15.2% | 6.15 | 3.40 | 17.0 |
| laya-adaptive | g_grep_explore | 886 | 10.0% | 3.40 | 2.00 | 9.8 |
| laya-adaptive | answer | 0 | 0.0% | 0.00 | 2.00 | 20.2 |
| laya-adaptive | **total** | 8824 | 100% | | 11.60 api calls | 65.9 wall |
| laya-lex | a_redundant | 489 | 6.6% | 0.35 | 0.20 | 0.7 |
| laya-lex | d_expansion | 1461 | 19.7% | 1.40 | 1.20 | 4.5 |
| laya-lex | b_map_hit | 389 | 5.3% | 0.40 | 0.25 | 0.2 |
| laya-lex | b2_map_file | 1973 | 26.6% | 1.25 | 0.95 | 3.4 |
| laya-lex | c_gold_miss | 504 | 6.8% | 0.55 | 0.40 | 2.1 |
| laya-lex | f_other | 670 | 9.0% | 1.20 | 0.95 | 4.2 |
| laya-lex | e_grep_locate | 1120 | 15.1% | 3.35 | 2.00 | 8.1 |
| laya-lex | g_grep_explore | 803 | 10.8% | 2.50 | 1.55 | 6.3 |
| laya-lex | answer | 0 | 0.0% | 0.00 | 2.00 | 17.7 |
| laya-lex | **total** | 7409 | 100% | | 9.50 api calls | 49.3 wall |

### Gold coverage of the injection (gold files, all tasks)

| arm | inlined | map only | related only | absent | tasks with 0 gold injected | FILES answer fully inside injection | ...inside inlined files | answer recall |
|---|---|---|---|---|---|---|---|---|
| laya-adaptive | 19 (56%) | 4 | 2 | 9 (26%) | 1/20 | 6/20 | 2/20 | 0.950 |
| laya-lex | 20 (59%) | 3 | 2 | 9 (26%) | 1/20 | 7/20 | 4/20 | 0.958 |

### Discovery timing (API-call index; 0 = gold already inlined)

| arm | first gold Read | last new gold file | api calls | tail calls after last gold | tail seconds | tail reading tok | tail share of wall |
|---|---|---|---|---|---|---|---|
| baseline | 2.9 (7/20) | 1.6 | 11.5 | 9.8 | 50.3 | 10531 | 87% |
| laya-adaptive | 2.9 (7/20) | 1.2 | 11.6 | 10.4 | 59.2 | 7425 | 90% |
| laya-lex | 2.1 (7/20) | 0.9 | 9.5 | 8.6 | 44.3 | 6055 | 90% |
