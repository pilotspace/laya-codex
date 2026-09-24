paired tasks: 20; arms: baseline, laya-adaptive, laya-lex

### Reading tokens / calls / turns / seconds per class (mean per task)

| arm | class | tokens | % of reading | calls | turns | seconds |
|---|---|---|---|---|---|---|
| baseline | a_redundant | 436 | 13.2% | 0.65 | 0.50 | 1.6 |
| baseline | d_expansion | 921 | 27.8% | 1.00 | 0.75 | 2.2 |
| baseline | b_map_hit | 164 | 5.0% | 0.20 | 0.15 | 0.4 |
| baseline | b2_map_file | 389 | 11.7% | 0.65 | 0.50 | 1.8 |
| baseline | c_gold_miss | 113 | 3.4% | 0.20 | 0.20 | 0.9 |
| baseline | f_other | 181 | 5.5% | 0.65 | 0.50 | 1.8 |
| baseline | e_grep_locate | 856 | 25.8% | 4.10 | 2.85 | 8.6 |
| baseline | g_grep_explore | 257 | 7.7% | 3.20 | 2.20 | 7.9 |
| baseline | answer | 0 | 0.0% | 0.00 | 2.00 | 12.2 |
| baseline | **total** | 3318 | 100% | | 9.65 api calls | 39.6 wall |
| laya-adaptive | a_redundant | 92 | 4.1% | 0.05 | 0.05 | 0.0 |
| laya-adaptive | d_expansion | 537 | 23.6% | 0.95 | 0.85 | 2.3 |
| laya-adaptive | b_map_hit | 139 | 6.1% | 0.35 | 0.30 | 1.4 |
| laya-adaptive | b2_map_file | 418 | 18.4% | 0.75 | 0.70 | 1.9 |
| laya-adaptive | c_gold_miss | 152 | 6.7% | 0.20 | 0.20 | 0.8 |
| laya-adaptive | f_other | 206 | 9.1% | 0.45 | 0.30 | 1.3 |
| laya-adaptive | e_grep_locate | 470 | 20.7% | 2.50 | 1.95 | 7.9 |
| laya-adaptive | g_grep_explore | 258 | 11.4% | 2.05 | 1.50 | 5.3 |
| laya-adaptive | answer | 0 | 0.0% | 0.00 | 2.00 | 14.2 |
| laya-adaptive | **total** | 2273 | 100% | | 7.85 api calls | 38.4 wall |
| laya-lex | a_redundant | 109 | 4.7% | 0.10 | 0.10 | 0.2 |
| laya-lex | d_expansion | 550 | 23.8% | 0.65 | 0.65 | 2.4 |
| laya-lex | b_map_hit | 250 | 10.8% | 0.40 | 0.35 | 1.0 |
| laya-lex | b2_map_file | 444 | 19.2% | 0.85 | 0.70 | 2.9 |
| laya-lex | c_gold_miss | 33 | 1.4% | 0.05 | 0.05 | 0.1 |
| laya-lex | f_other | 157 | 6.8% | 0.35 | 0.30 | 1.1 |
| laya-lex | e_grep_locate | 471 | 20.4% | 2.15 | 1.30 | 5.3 |
| laya-lex | g_grep_explore | 298 | 12.9% | 1.90 | 1.45 | 5.2 |
| laya-lex | answer | 0 | 0.0% | 0.00 | 2.00 | 12.4 |
| laya-lex | **total** | 2313 | 100% | | 6.90 api calls | 32.7 wall |

### Gold coverage of the injection (gold files, all tasks)

| arm | inlined | map only | related only | absent | tasks with 0 gold injected | FILES answer fully inside injection | ...inside inlined files | answer recall |
|---|---|---|---|---|---|---|---|---|
| laya-adaptive | 28 (80%) | 0 | 1 | 6 (17%) | 1/20 | 15/20 | 12/20 | 0.917 |
| laya-lex | 28 (80%) | 0 | 2 | 5 (14%) | 1/20 | 16/20 | 13/20 | 0.817 |

### Discovery timing (API-call index; 0 = gold already inlined)

| arm | first gold Read | last new gold file | api calls | tail calls after last gold | tail seconds | tail reading tok | tail share of wall |
|---|---|---|---|---|---|---|---|
| baseline | 3.7 (3/20) | 0.6 | 9.7 | 9.1 | 36.0 | 3207 | 91% |
| laya-adaptive | 3.8 (4/20) | 0.8 | 7.9 | 7.2 | 33.7 | 1977 | 86% |
| laya-lex | 3.5 (2/20) | 0.4 | 6.9 | 6.6 | 30.0 | 2254 | 91% |
