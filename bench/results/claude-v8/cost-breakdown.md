
moon  (tasks finished by all arms: 20; means per task)
| arm | turns | cache read tok ($) | cache write tok ($) | uncached in ($) | output tok ($) | priced $ | reported $ | reading tok | injected tok |
|---|---|---|---|---|---|---|---|---|---|
| baseline | 18.8 | 417k ($0.125) | 37k ($0.139) | $0.000 | 5.7k ($0.085) | $0.349 | $0.463 | 14.0k | 0.0k |
| laya-adaptive | 16.8 | 407k ($0.122) | 31k ($0.115) | $0.000 | 6.1k ($0.091) | $0.329 | $0.385 | 8.8k | 3.4k |
| laya-lex | 13.0 | 309k ($0.093) | 29k ($0.107) | $0.000 | 4.9k ($0.073) | $0.273 | $0.339 | 7.4k | 3.5k |

httpx  (tasks finished by all arms: 20; means per task)
| arm | turns | cache read tok ($) | cache write tok ($) | uncached in ($) | output tok ($) | priced $ | reported $ | reading tok | injected tok |
|---|---|---|---|---|---|---|---|---|---|
| baseline | 12.7 | 170k ($0.051) | 12k ($0.044) | $0.000 | 3.3k ($0.049) | $0.144 | $0.171 | 3.3k | 0.0k |
| laya-adaptive | 9.3 | 162k ($0.049) | 14k ($0.052) | $0.000 | 3.2k ($0.048) | $0.150 | $0.177 | 2.3k | 3.4k |
| laya-lex | 8.5 | 139k ($0.042) | 14k ($0.051) | $0.000 | 2.9k ($0.044) | $0.137 | $0.162 | 2.3k | 3.4k |

hono  (tasks finished by all arms: 20; means per task)
| arm | turns | cache read tok ($) | cache write tok ($) | uncached in ($) | output tok ($) | priced $ | reported $ | reading tok | injected tok |
|---|---|---|---|---|---|---|---|---|---|
| baseline | 11.2 | 137k ($0.041) | 15k ($0.056) | $0.000 | 3.5k ($0.052) | $0.149 | $0.186 | 4.5k | 0.0k |
| laya-adaptive | 7.6 | 129k ($0.039) | 14k ($0.054) | $0.000 | 3.3k ($0.050) | $0.142 | $0.176 | 2.4k | 3.1k |
| laya-lex | 8.6 | 158k ($0.047) | 16k ($0.060) | $0.000 | 4.0k ($0.059) | $0.167 | $0.212 | 2.8k | 3.2k |
