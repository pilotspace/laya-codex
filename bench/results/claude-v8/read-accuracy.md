
moon
| arm | n | Read calls | read precision | read recall (saw gold code) | wasted read tokens | first gold Read at turn |
|---|---|---|---|---|---|---|
| baseline | 20 | 5.60 | 0.369 | 0.858 | 4884 | 8.15 (20/20 runs Read a gold file) |
| laya-adaptive | 20 | 5.20 | 0.559 | 0.883 | 2621 | 3.95 (20/20 runs Read a gold file) |
| laya-lex | 20 | 5.15 | 0.561 | 0.896 | 2052 | 3.00 (20/20 runs Read a gold file) |

httpx
| arm | n | Read calls | read precision | read recall (saw gold code) | wasted read tokens | first gold Read at turn |
|---|---|---|---|---|---|---|
| baseline | 20 | 3.35 | 0.581 | 0.758 | 587 | 4.82 (17/20 runs Read a gold file) |
| laya-adaptive | 20 | 2.75 | 0.538 | 0.917 | 592 | 3.53 (15/20 runs Read a gold file) |
| laya-lex | 20 | 2.40 | 0.458 | 0.875 | 792 | 4.20 (15/20 runs Read a gold file) |

hono
| arm | n | Read calls | read precision | read recall (saw gold code) | wasted read tokens | first gold Read at turn |
|---|---|---|---|---|---|---|
| baseline | 20 | 3.10 | 0.833 | 0.833 | 542 | 4.60 (20/20 runs Read a gold file) |
| laya-adaptive | 20 | 2.50 | 0.791 | 0.917 | 347 | 2.47 (17/20 runs Read a gold file) |
| laya-lex | 20 | 2.90 | 0.796 | 0.950 | 537 | 3.05 (19/20 runs Read a gold file) |

pooled (3 run dirs)
| arm | n | Read calls | read precision | read recall (saw gold code) | wasted read tokens | first gold Read at turn |
|---|---|---|---|---|---|---|
| baseline | 60 | 4.02 | 0.595 | 0.817 | 2005 | 5.91 (57/60 runs Read a gold file) |
| laya-adaptive | 60 | 3.48 | 0.624 | 0.906 | 1187 | 3.35 (52/60 runs Read a gold file) |
| laya-lex | 60 | 3.48 | 0.604 | 0.907 | 1127 | 3.35 (54/60 runs Read a gold file) |
