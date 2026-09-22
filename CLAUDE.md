<!-- ADD:BEGIN — managed by `add.py sync-guidelines`; do not edit inside -->
## ADD — how to work in this repo

This project uses **ADD (AI-Driven Development)**. The engine is installed.
To begin: run `python3 .add/tooling/cli.py status` — your resume point (read from
the `.add/` bundle, not the repo), which names the next beat to work.

**Size before ceremony.** The floor is checked first: security · data · architecture, a new contract surface, or frozen scope always takes a Task, and security is a HARD-STOP.
Under that floor, a change of at most 3 adjacent files with zero contract-shaping unknowns goes direct — an inline card, red→green, then a commit plus one `add learn` line and no node; anything larger takes a Task, and effort rises with the rung while review never falls.

Open Claude Code and run `/add` — the skill drives intake -> milestone -> build.

Edit outside the markers, not inside.
<!-- ADD:END -->
