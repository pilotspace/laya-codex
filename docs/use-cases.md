# Example use cases

laya-codex works inside your normal Claude Code workflow. You don't call it or change how you
prompt. This page shows the situations where it helps most, what it adds to Claude's context in
each one, and where it helps less.

The excerpts come from real laya-codex output on this repository, trimmed for length (`…`).
They were captured with ranking by keywords only, so they show the baseline behaviour; with the
Laya model (the default on macOS), the order is sharper and less noise gets in.

---

## 1. Fix a bug in code you didn't write

**You ask:**
> Bug: laya-codex init writes through a symlinked .claude directory into another repo. Find
> where init checks its target paths and fix the check.

**Without laya-codex**, Claude greps for `symlink` and `init` and opens a few files, usually
`main.rs` first, before it finds the check.

**With laya-codex**, the check is in the context before Claude's first turn:

```markdown
Ranked locations:
1. crates/laya-cli/src/init.rs — 394-425 fn apply; 285-325 fn check_target; …
2. crates/laya-cli/src/doctor.rs — 339-377 fn check_hooks_with
3. crates/laya-store/src/secure.rs — 51-91 fn create_private_dir

### crates/laya-cli/src/init.rs:394-425 — fn apply
```rust
pub fn apply(p: &Planned) -> anyhow::Result<()> { …
```

Definitions and uses:
- crates/laya-cli/src/init.rs:288: fn check_target(root: &Path, root_canon: &Path, path: &Path) … — definition of `check_target`
- crates/laya-cli/src/init.rs:472: check_target(root, &root_canon, path)?; — use of `check_target`
```

Claude goes straight to `check_target`. In the benchmark (60 tasks, three repositories), a
correct file was in context before the first turn in 46 of 60 tasks. Stock Claude Code first had
a correct file at turn 5.5 (median).

## 2. Keep going in the same session: tests and call sites

**You follow up:**
> Now, for the same change, identify the tests that cover this code and the main call sites.

laya-codex knows what this session already has, so it doesn't send that code again. It adds
what's new, such as the test modules:

```markdown
1. crates/laya-cli/src/init.rs — 285-325 fn check_target; … 844-868 mod tests > fn a_symlinked_backup_path_is_refused
2. crates/laya-store/src/secure.rs — 51-91 fn create_private_dir

Already provided earlier in this session: crates/laya-cli/src/init.rs:394-425, crates/laya-cli/src/doctor.rs:339-377, …
```

This is why reading costs keep falling over a long session instead of growing. Follow-up
prompts that only say "the same change" are searched together with the session's topic, so
they don't lose track of it.

## 3. Check the impact before changing a signature

**You ask:**
> What would break if I changed the signature of rel_path in the config module?

The "Definitions and uses" list works like a grep that Claude doesn't have to run:

```markdown
Definitions and uses:
- crates/laya-cli/src/daemon.rs:17: use crate::config::{Config, rel_path, repo_root}; — use of `rel_path`
- crates/laya-cli/src/hook.rs:16: use crate::config::rel_path; — use of `rel_path`
Related by references:
- crates/laya-cli/src/main.rs:387-434 fn hook_inner — calls `repo_root` (#1)
- crates/laya-cli/src/daemon.rs:1-43 — calls `repo_root` (#1)
```

Claude starts its answer from the real call sites instead of guessing which modules import the
function.

## 4. Get to know an unfamiliar codebase

**You ask:**
> How does this service decide when to retry a failed payment?

On a codebase you have never seen, the ranked map works as a table of contents for the question:
- the files and functions that matter;
- the code of the top three;
- the callers and callees one step away.

Claude's first answer already cites the right functions, and you can open them from the paths
it gives.

## 5. Work in large files

laya-codex never changes Claude's Reads. The injected map gives line ranges, so Claude can read
only those lines of a large file. When Claude does read a file whole, laya-codex notes it and
leaves that file's code out of later injections in the session: Claude already has all of it.

## 6. Long sessions and compaction

When Claude Code compacts a long conversation, earlier injected code drops out of the context.
laya-codex notices the compaction and adds the session's working set back, so the next prompt
doesn't have to go looking for the same code again.

## 7. Subagents

When Claude hands a task to a subagent (the Agent/Task tool), laya-codex passes along the code it
already found. The subagent starts with context instead of repeating the search.

## 8. Follow-up searches by Claude itself

Claude can call the `search` MCP tool that laya-codex provides for follow-up lookups, such as
"where is the retry budget configured?". It returns ranked code locations in one call, instead of
a series of grep and Read calls.

## 9. Teams

Install the plugin at *project* scope (`/plugin install laya-codex@laya-codex`, then choose
project). That records it in the repository's `.claude/settings.json`, and every teammate who
uses Claude Code in that repository gets the same setup. Each machine keeps its own local index,
so no code is shared or uploaded.

---

## When it helps less

- **Tiny repositories.** If the whole codebase fits in a few files, Claude finds the code
  quickly on its own, and laya-codex has little to add.
- **Questions that aren't about code**, such as writing a new README from scratch or general
  questions. laya-codex adds only what it finds relevant, and sometimes that's nothing.
- **Very generic wording.** Names like `apply` or `run` match in many places, so some of what
  gets added is noise. Naming the module or the symptom helps.
- **Languages it doesn't parse.** Supported: Rust, Python, TypeScript/TSX, JavaScript, Go,
  Java, C, C++, C#, Ruby, PHP, Kotlin and Swift. Files in other languages aren't indexed.
- **Linux, by default.** There the model runs on the CPU, so laya-codex ranks by keywords. It
  still helps, as in the excerpts above, but it is less precise than on macOS.

## Get started

```sh
curl -fsSL https://raw.githubusercontent.com/pilotspace/laya-codex/main/install.sh | sh
```

Then, inside Claude Code: `/plugin marketplace add pilotspace/laya-codex` and
`/plugin install laya-codex@laya-codex`. See the [README](../README.md) for other install
options and the benchmark behind these numbers.
