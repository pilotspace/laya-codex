//! `laya-codex init`: enable laya-codex for a repository by merging the Claude Code hooks into
//! `.claude/settings.local.json` and the `laya-codex` MCP server into `.mcp.json`.
//!
//! Merging keeps every unrelated key, hook and server; only existing laya-codex entries are replaced, so
//! a re-run yields byte-identical files. Unparseable JSON is never overwritten without `--force`
//! (which backs the original up to `*.bak` first). Both files are planned before either is written.
//!
//! The repository is untrusted input (it may be a fresh clone): a target file, its `.claude`
//! directory or a `.bak` path that is a symlink, is not a regular file, or resolves outside the
//! repo root is refused before anything is read or written. Files are replaced by creating a
//! uniquely named temp file with `O_CREAT|O_EXCL` (which never follows a symlink) and renaming it
//! over the target (which replaces the directory entry, never writing through it).

use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail};
use serde_json::{Map, Value, json};

/// Hook events laya-codex handles: (event, matcher, timeout seconds). Mirrors the README.
pub const HOOK_EVENTS: [(&str, Option<&str>, u64); 4] = [
    ("SessionStart", None, 5),
    ("UserPromptSubmit", None, 8),
    ("PreToolUse", Some("Read|Agent|Task"), 5),
    ("PostToolUse", Some("Edit|Write|MultiEdit|NotebookEdit"), 5),
];

/// The program name written when `laya-codex` on `PATH` is the running binary.
const BARE: &str = "laya-codex";

/// Settings files larger than this are refused rather than read.
const MAX_JSON_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Create,
    Update,
    Unchanged,
    /// The existing file was not mergeable JSON; it is backed up to `backup` and replaced.
    Replace {
        backup: PathBuf,
    },
}

/// One file `laya-codex init` will write (or would, under `--dry-run`).
#[derive(Debug, Clone)]
pub struct Planned {
    pub path: PathBuf,
    pub action: Action,
    /// Full new content (pretty JSON with a trailing newline).
    pub content: String,
}

/// The program laya-codex's hooks and MCP server should run: the bare `laya-codex` when the
/// first `laya-codex` on `path_env` is (after resolving symlinks) the same file as `exe`, so the
/// committed `.mcp.json` stays portable (and a Homebrew install does not pin its versioned Cellar
/// path, since bin/laya-codex links to it); otherwise `exe` itself. A cwd-relative `PATH` entry (`.`, empty) ahead of the
/// match makes the lookup depend on where Claude Code runs, so it also yields `exe`.
pub fn program_for(exe: &Path, path_env: Option<&OsStr>) -> PathBuf {
    let fallback = exe.to_path_buf();
    let (Ok(me), Some(path)) = (exe.canonicalize(), path_env) else {
        return fallback;
    };
    for dir in std::env::split_paths(path) {
        if dir.is_relative() {
            return fallback;
        }
        let candidate = dir.join(BARE);
        if is_executable_file(&candidate) {
            return if candidate.canonicalize().is_ok_and(|c| c == me) {
                PathBuf::from(BARE)
            } else {
                fallback
            };
        }
    }
    fallback
}

fn is_executable_file(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The shell command Claude Code runs for every laya-codex hook, for the program [`program_for`] picks
/// from the current `PATH`.
pub fn hook_command(exe: &Path, adaptive: bool) -> String {
    hook_command_for(
        &program_for(exe, std::env::var_os("PATH").as_deref()),
        adaptive,
    )
}

/// The hook command running `program` (no `PATH` lookup).
fn hook_command_for(program: &Path, adaptive: bool) -> String {
    let prefix = if adaptive {
        "LAYA_CODEX_ADAPTIVE=1 "
    } else {
        ""
    };
    format!("{prefix}{} hook", shell_quote(&program.to_string_lossy()))
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+:@%,=".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// One shell word and whether any part of it was quoted.
#[derive(Default)]
struct Word {
    text: String,
    quoted: bool,
}

/// Split a simple command into words (quotes removed). `None` for anything beyond a plain
/// command line — operators, substitutions, globs, comments, unbalanced quotes — which
/// laya-codex never writes, so such a command is never mistaken for laya-codex's.
fn shell_words(cmd: &str) -> Option<Vec<Word>> {
    let mut words = Vec::new();
    let mut cur: Option<Word> = None;
    let mut chars = cmd.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => words.extend(cur.take()),
            '\'' | '"' => {
                let w = cur.get_or_insert_with(Word::default);
                w.quoted = true;
                loop {
                    match chars.next()? {
                        q if q == c => break,
                        '$' | '`' | '\\' if c == '"' => return None,
                        ch => w.text.push(ch),
                    }
                }
            }
            // `'\''` is how `shell_quote` embeds a quote: a backslash escapes one character.
            '\\' => {
                let w = cur.get_or_insert_with(Word::default);
                w.quoted = true;
                match chars.next()? {
                    '\n' | '\r' => return None,
                    ch => w.text.push(ch),
                }
            }
            c if ";&|<>()$`*?[]{}#!\n\r".contains(c) => return None,
            c => cur.get_or_insert_with(Word::default).text.push(c),
        }
    }
    words.extend(cur);
    Some(words)
}

/// `NAME=value` as the shell reads a leading environment assignment (the name unquoted).
fn is_assignment(w: &Word) -> bool {
    !w.quoted
        && w.text.split_once('=').is_some_and(|(name, _)| {
            name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// Is `cmd` a laya-codex hook command? Exactly: optional `NAME=value` assignments (laya-codex
/// writes `LAYA_CODEX_ADAPTIVE=1`), then a program that is `laya-codex` or a path whose file name
/// is `laya-codex`, then `hook` and nothing else. `laya-codex hook`, `/abs/laya-codex hook` and
/// `X=1 '/a b/laya-codex' hook` match; `echo laya-codex hook`, `mytool wrap-laya hook` and
/// `laya-codex hook && x` do not, and neither does 0.1.x's `laya hook` (a clean break).
pub fn is_laya_hook(cmd: &str) -> bool {
    let Some(words) = shell_words(cmd) else {
        return false;
    };
    let mut rest = words.iter().skip_while(|w| is_assignment(w));
    let is_laya = |w: &Word| {
        !w.text.ends_with('/') && Path::new(&w.text).file_name() == Some(OsStr::new(BARE))
    };
    rest.next().is_some_and(is_laya)
        && rest.next().is_some_and(|w| w.text == "hook")
        && rest.next().is_none()
}

fn object(v: Value) -> Result<Map<String, Value>, String> {
    match v {
        Value::Object(m) => Ok(m),
        _ => Err("the top level is not a JSON object".into()),
    }
}

/// Merge laya-codex's hooks (running `cmd`) into a settings object; `Err` if its shape is not mergeable.
pub fn merge_settings(existing: Value, cmd: &str) -> Result<Value, String> {
    let mut root = object(existing)?;
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks.as_object_mut().ok_or("`hooks` is not an object")?;
    for (event, matcher, timeout) in HOOK_EVENTS {
        let groups = hooks.entry(event).or_insert_with(|| json!([]));
        let groups = groups
            .as_array_mut()
            .ok_or_else(|| format!("`hooks.{event}` is not an array"))?;
        // Drop laya-codex handlers everywhere; drop a group only if it held nothing but laya-codex handlers.
        groups.retain_mut(|g| {
            let Some(hs) = g.get_mut("hooks").and_then(Value::as_array_mut) else {
                return true;
            };
            let before = hs.len();
            hs.retain(|h| !h["command"].as_str().is_some_and(is_laya_hook));
            !(hs.is_empty() && hs.len() < before)
        });
        let mut group = Map::new();
        if let Some(m) = matcher {
            group.insert("matcher".into(), json!(m));
        }
        group.insert(
            "hooks".into(),
            json!([{"type": "command", "command": cmd, "timeout": timeout}]),
        );
        groups.push(Value::Object(group));
    }
    Ok(Value::Object(root))
}

/// Set `mcpServers.laya` (running `program`) in an `.mcp.json` object; `Err` if its shape is not
/// mergeable.
pub fn merge_mcp(existing: Value, program: &Path) -> Result<Value, String> {
    let mut root = object(existing)?;
    let servers = root.entry("mcpServers").or_insert_with(|| json!({}));
    let servers = servers
        .as_object_mut()
        .ok_or("`mcpServers` is not an object")?;
    servers.insert(
        "laya-codex".into(),
        json!({"command": program.to_string_lossy(), "args": ["mcp"]}),
    );
    Ok(Value::Object(root))
}

/// Plan the new content of the JSON file at `path` via `merge`. Nothing is written.
pub fn plan_file(
    path: &Path,
    force: bool,
    merge: impl Fn(Value) -> Result<Value, String>,
) -> anyhow::Result<Planned> {
    let pretty = |v: &Value| serde_json::to_string_pretty(v).map(|s| s + "\n");
    let planned = |action, content| Planned {
        path: path.to_path_buf(),
        action,
        content,
    };
    let text = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let new = merge(json!({})).map_err(anyhow::Error::msg)?;
            return Ok(planned(Action::Create, pretty(&new)?));
        }
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let parsed = if text.trim().is_empty() {
        Ok(json!({}))
    } else {
        serde_json::from_str::<Value>(&text).map_err(|e| e.to_string())
    };
    match parsed.and_then(|old| merge(old.clone()).map(|new| (old, new))) {
        Ok((old, new)) if old == new => Ok(planned(Action::Unchanged, text)),
        Ok((_, new)) => Ok(planned(Action::Update, pretty(&new)?)),
        Err(reason) => {
            let backup = backup_path(path);
            if !force {
                bail!(
                    "{} cannot be merged ({reason}); fix it by hand, or re-run with --force to back it up to {} and replace it",
                    path.display(),
                    backup.display()
                );
            }
            let new = merge(json!({})).map_err(anyhow::Error::msg)?;
            Ok(planned(Action::Replace { backup }, pretty(&new)?))
        }
    }
}

fn backup_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".bak");
    PathBuf::from(s)
}

/// Refuse `path` unless every directory between `root` and it is a real (non-symlink) directory
/// or absent, everything resolves inside `root_canon`, and `path` itself is absent or a regular,
/// non-symlink file of sane size. Nothing is created or read.
fn check_target(root: &Path, root_canon: &Path, path: &Path) -> anyhow::Result<()> {
    let rel = path
        .strip_prefix(root)
        .with_context(|| format!("{} is not inside {}", path.display(), root.display()))?;
    let mut dir = root.to_path_buf();
    let parents: Vec<Component> = rel
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .collect();
    for c in parents {
        anyhow::ensure!(
            matches!(c, Component::Normal(_)),
            "{} is not a plain path under the repo",
            path.display()
        );
        dir.push(c);
        match std::fs::symlink_metadata(&dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(e).with_context(|| format!("inspect {}", dir.display())),
            Ok(m) if m.file_type().is_symlink() => bail!(
                "{} is a symlink; refusing to write through it (a repository must not redirect laya-codex init outside itself). Replace it with a real directory and re-run",
                dir.display()
            ),
            Ok(m) if !m.is_dir() => bail!("{} is not a directory", dir.display()),
            Ok(_) => {
                let canon = dir
                    .canonicalize()
                    .with_context(|| format!("resolve {}", dir.display()))?;
                anyhow::ensure!(
                    canon.starts_with(root_canon),
                    "{} resolves outside the repository ({})",
                    dir.display(),
                    canon.display()
                );
            }
        }
    }
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("inspect {}", path.display())),
        Ok(m) if m.file_type().is_symlink() => bail!(
            "{} is a symlink; refusing to write through it (a repository must not redirect laya-codex init outside itself). Replace it with a regular file and re-run",
            path.display()
        ),
        Ok(m) if !m.is_file() => bail!("{} is not a regular file", path.display()),
        Ok(m) if m.len() > MAX_JSON_BYTES => bail!(
            "{} is larger than {} MiB; refusing to read it",
            path.display(),
            MAX_JSON_BYTES >> 20
        ),
        Ok(_) => Ok(()),
    }
}

/// Atomically replace `target` with `content`: a fresh, uniquely named temp file in the same
/// directory (`O_CREAT|O_EXCL`, so a planted file or symlink is never opened), then a rename over
/// the target (which swaps the directory entry and never writes through a symlink). `mode` is the
/// exact permission bits to give it; `None` means the default for new files.
fn replace_file(target: &Path, content: &[u8], mode: Option<u32>) -> std::io::Result<()> {
    use std::hash::{BuildHasher, Hasher};
    let dir = target.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    let name = target.file_name().ok_or(std::io::ErrorKind::InvalidInput)?;
    for _ in 0..16 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u32(std::process::id());
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        let mut tmp_name = OsStr::new(".").to_owned();
        tmp_name.push(name);
        tmp_name.push(format!(".{:016x}.laya-tmp", h.finish()));
        let tmp = dir.join(tmp_name);
        let mut f = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o666)
            .open(&tmp)
        {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            r => r?,
        };
        let res = f
            .write_all(content)
            .and_then(|()| match mode {
                Some(m) => f.set_permissions(std::fs::Permissions::from_mode(m)),
                None => Ok(()),
            })
            .and_then(|()| f.sync_all())
            .and_then(|()| {
                drop(f);
                std::fs::rename(&tmp, target)
            });
        if res.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        return res;
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no free temp file name",
    ))
}

/// Permission bits of the regular file at `path`, if there is one.
fn file_mode(path: &Path) -> Option<u32> {
    std::fs::symlink_metadata(path)
        .ok()
        .filter(|m| m.is_file())
        .map(|m| m.permissions().mode() & 0o7777)
}

/// Write a planned file atomically (temp file + rename). Refuses a symlinked or non-regular
/// target, backup or parent directory; `run` has already checked all of them against the repo.
pub fn apply(p: &Planned) -> anyhow::Result<()> {
    if p.action == Action::Unchanged {
        return Ok(());
    }
    let dir = p.path.parent().context("no parent directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    // Re-check the files right before writing: the plan may be older than the tree.
    let dir_canon = dir
        .canonicalize()
        .with_context(|| format!("resolve {}", dir.display()))?;
    check_target(dir, &dir_canon, &p.path)?;
    let mode = file_mode(&p.path);
    if let Action::Replace { backup } = &p.action {
        check_target(dir, &dir_canon, backup)?;
        let original =
            std::fs::read(&p.path).with_context(|| format!("read {}", p.path.display()))?;
        replace_file(backup, &original, mode)
            .with_context(|| format!("back up to {}", backup.display()))?;
    }
    replace_file(&p.path, p.content.as_bytes(), mode)
        .with_context(|| format!("write {}", p.path.display()))
}

/// Plan both files for `root`; write them unless `dry_run`. Nothing is written unless both plan
/// cleanly, and a failed second write rolls the first one back. Uses the current `PATH` to
/// decide between the bare `laya-codex` and `exe`'s path (see [`program_for`]), and says so on stderr
/// when the written path is machine-specific.
pub fn run(
    root: &Path,
    exe: &Path,
    adaptive: bool,
    dry_run: bool,
    force: bool,
) -> anyhow::Result<Vec<Planned>> {
    let path_env = std::env::var_os("PATH");
    let plans = run_with(root, exe, path_env.as_deref(), adaptive, dry_run, force)?;
    if program_for(exe, path_env.as_deref()) != Path::new(BARE) {
        eprintln!(
            "note: `laya-codex` on PATH is not {}, so .mcp.json and the hooks use that machine-specific path; \
             don't commit .mcp.json as is, or put this laya-codex first on PATH and re-run `laya-codex init`",
            exe.display()
        );
    }
    Ok(plans)
}

/// [`run`] with an explicit `PATH` (for [`program_for`]) and no stderr note.
pub fn run_with(
    root: &Path,
    exe: &Path,
    path_env: Option<&OsStr>,
    adaptive: bool,
    dry_run: bool,
    force: bool,
) -> anyhow::Result<Vec<Planned>> {
    let program = program_for(exe, path_env);
    anyhow::ensure!(
        program.to_str().is_some(),
        "the laya-codex executable path {} is not valid UTF-8; install laya-codex under a UTF-8 path",
        program.display()
    );
    let cmd = hook_command_for(&program, adaptive);
    let root_canon = root
        .canonicalize()
        .with_context(|| format!("resolve {}", root.display()))?;
    let settings = root.join(".claude").join("settings.local.json");
    let mcp = root.join(".mcp.json");
    for path in [&settings, &mcp] {
        check_target(root, &root_canon, path)?;
    }
    let plans = vec![
        plan_file(&settings, force, |v| merge_settings(v, &cmd))?,
        plan_file(&mcp, force, |v| merge_mcp(v, &program))?,
    ];
    for p in &plans {
        if let Action::Replace { backup } = &p.action {
            check_target(root, &root_canon, backup)?;
        }
    }
    if dry_run {
        return Ok(plans);
    }
    let originals: Vec<Option<(Vec<u8>, Option<u32>)>> = plans
        .iter()
        .map(|p| std::fs::read(&p.path).ok().map(|b| (b, file_mode(&p.path))))
        .collect();
    for (i, p) in plans.iter().enumerate() {
        if let Err(e) = apply(p) {
            for (done, orig) in plans[..i].iter().zip(&originals) {
                let _ = match orig {
                    Some((bytes, mode)) => replace_file(&done.path, bytes, *mode),
                    None if done.action != Action::Unchanged => std::fs::remove_file(&done.path),
                    None => Ok(()),
                };
            }
            return Err(e);
        }
    }
    Ok(plans)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "/opt/laya-codex/bin/laya-codex";

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("laya-init-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn settings(root: &Path) -> PathBuf {
        root.join(".claude/settings.local.json")
    }

    fn read_json(p: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    fn laya_cmds(v: &Value, event: &str) -> Vec<String> {
        v["hooks"][event]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
            .filter_map(|h| h["command"].as_str().map(str::to_string))
            .filter(|c| is_laya_hook(c))
            .collect()
    }

    #[test]
    fn hook_command_quotes_only_when_needed_and_prefixes_adaptive() {
        assert_eq!(
            hook_command(Path::new(EXE), false),
            "/opt/laya-codex/bin/laya-codex hook"
        );
        assert_eq!(
            hook_command(Path::new(EXE), true),
            "LAYA_CODEX_ADAPTIVE=1 /opt/laya-codex/bin/laya-codex hook"
        );
        assert_eq!(
            hook_command(Path::new("/My Tools/it's/laya-codex"), false),
            r"'/My Tools/it'\''s/laya-codex' hook"
        );
        // Whatever init writes, re-init recognises as its own.
        for p in [
            "laya-codex",
            EXE,
            "/My Tools/it's/laya-codex",
            "/a;b/$(x)/laya-codex",
            "/q\"uote/laya-codex",
        ] {
            for adaptive in [false, true] {
                let c = hook_command_for(Path::new(p), adaptive);
                assert!(is_laya_hook(&c), "{c}");
            }
        }
    }

    #[test]
    fn recognises_laya_hook_commands_only() {
        for c in [
            "laya-codex hook",
            "/opt/laya-codex/bin/laya-codex hook",
            "LAYA_CODEX_ADAPTIVE=1 /x/laya-codex hook",
            "'/a b/laya-codex' hook",
            " laya-codex hook ",
            r"'/My Tools/it'\''s/laya-codex' hook",
            "\"/a b/laya-codex\" hook",
            "LAYA_CODEX_ADAPTIVE=1  laya-codex\thook",
        ] {
            assert!(is_laya_hook(c), "{c}");
        }
        for c in [
            "",
            "laya-codex",
            "laya-codex mcp",
            "/x/notlaya hook",
            "echo hook",
            "laya-codex hook && rm -rf /",
            "my-hook",
            // Only laya-codex's own program counts, not any command that ends in `laya-codex hook`.
            "mytool wrap-laya hook",
            "echo laya-codex hook",
            "mytool laya-codex hook",
            "'/x/laya-codex hook'",
            "laya-codex/ hook",
            // 0.1.x wrote `laya hook`; laya-codex does not own (or remove) those.
            "laya hook",
            "/opt/laya/bin/laya hook",
            "laya-codex hook; rm -rf ~",
            "laya-codex hook $(evil)",
            "laya-codex hook --other",
            "'LAYA_CODEX_ADAPTIVE=1' laya-codex hook",
        ] {
            assert!(!is_laya_hook(c), "{c}");
        }
    }

    #[test]
    fn fresh_repo_gets_all_hooks_and_mcp_server() {
        let root = scratch("fresh");
        let plans = run(&root, Path::new(EXE), false, false, false).unwrap();
        assert!(plans.iter().all(|p| p.action == Action::Create));
        let s = read_json(&settings(&root));
        for (event, matcher, timeout) in HOOK_EVENTS {
            let groups = s["hooks"][event].as_array().unwrap();
            assert_eq!(groups.len(), 1, "{event}");
            assert_eq!(groups[0]["matcher"].as_str(), matcher, "{event}");
            assert_eq!(
                groups[0]["hooks"],
                json!([{"type": "command", "command": "/opt/laya-codex/bin/laya-codex hook", "timeout": timeout}])
            );
        }
        let m = read_json(&root.join(".mcp.json"));
        assert_eq!(
            m,
            json!({"mcpServers": {"laya-codex": {"command": EXE, "args": ["mcp"]}}})
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn merge_keeps_unrelated_keys_and_hooks_and_replaces_old_laya_entries() {
        let root = scratch("merge");
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        let before = json!({
            "permissions": {"allow": ["Bash(ls)"]},
            "hooks": {
                "PreToolUse": [
                    {"matcher": "Bash", "hooks": [{"type": "command", "command": "guard.sh"}]},
                    {"matcher": "Read|Agent|Task", "hooks": [{"type": "command", "command": "laya-codex hook", "timeout": 5}]},
                    {"matcher": "Write", "hooks": [{"type": "command", "command": "fmt.sh"}, {"type": "command", "command": "LAYA_CODEX_ADAPTIVE=1 laya-codex hook"}]}
                ],
                "Stop": [{"hooks": [{"type": "command", "command": "notify.sh"}]}]
            },
            "zeta": 1
        });
        std::fs::write(
            settings(&root),
            serde_json::to_string_pretty(&before).unwrap(),
        )
        .unwrap();
        std::fs::write(root.join(".mcp.json"), r#"{"mcpServers": {"other": {"command": "x"}, "laya-codex": {"command": "laya-codex", "args": ["mcp"], "env": {}}}}"#).unwrap();

        let plans = run(&root, Path::new(EXE), true, false, false).unwrap();
        assert!(plans.iter().all(|p| p.action == Action::Update));
        let s = read_json(&settings(&root));
        assert_eq!(s["permissions"], before["permissions"]);
        assert_eq!(s["zeta"], 1);
        assert_eq!(s["hooks"]["Stop"], before["hooks"]["Stop"]);
        let pre = s["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre[0], before["hooks"]["PreToolUse"][0]);
        // The laya-codex handler is removed from the mixed group, which keeps its own hook.
        assert_eq!(
            pre[1],
            json!({"matcher": "Write", "hooks": [{"type": "command", "command": "fmt.sh"}]})
        );
        assert_eq!(pre.len(), 3);
        for (event, ..) in HOOK_EVENTS {
            assert_eq!(
                laya_cmds(&s, event),
                vec!["LAYA_CODEX_ADAPTIVE=1 /opt/laya-codex/bin/laya-codex hook"],
                "{event}"
            );
        }
        // Key order of the user's file is preserved.
        let keys: Vec<&String> = s.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["permissions", "hooks", "zeta"]);
        let m = read_json(&root.join(".mcp.json"));
        assert_eq!(m["mcpServers"]["other"], json!({"command": "x"}));
        assert_eq!(
            m["mcpServers"]["laya-codex"],
            json!({"command": EXE, "args": ["mcp"]})
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rerun_is_idempotent() {
        let root = scratch("idem");
        run(&root, Path::new(EXE), false, false, false).unwrap();
        let first = (
            std::fs::read(settings(&root)).unwrap(),
            std::fs::read(root.join(".mcp.json")).unwrap(),
        );
        let plans = run(&root, Path::new(EXE), false, false, false).unwrap();
        assert!(plans.iter().all(|p| p.action == Action::Unchanged));
        let second = (
            std::fs::read(settings(&root)).unwrap(),
            std::fs::read(root.join(".mcp.json")).unwrap(),
        );
        assert_eq!(first, second);
        // Switching to adaptive and back replaces, never duplicates.
        run(&root, Path::new(EXE), true, false, false).unwrap();
        run(&root, Path::new(EXE), false, false, false).unwrap();
        assert_eq!(std::fs::read(settings(&root)).unwrap(), first.0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn invalid_json_is_refused_without_force_and_backed_up_with_force() {
        let root = scratch("invalid");
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::write(settings(&root), "{ not json").unwrap();
        let err = run(&root, Path::new(EXE), false, false, false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("settings.local.json") && msg.contains("--force"),
            "{msg}"
        );
        assert_eq!(
            std::fs::read_to_string(settings(&root)).unwrap(),
            "{ not json"
        );
        assert!(
            !root.join(".mcp.json").exists(),
            "nothing is written when any file is refused"
        );

        // Valid JSON of the wrong shape is refused the same way.
        std::fs::write(root.join(".mcp.json"), "[1]").unwrap();
        std::fs::write(settings(&root), r#"{"hooks": []}"#).unwrap();
        assert!(run(&root, Path::new(EXE), false, false, false).is_err());

        std::fs::write(settings(&root), "{ not json").unwrap();
        let plans = run(&root, Path::new(EXE), false, false, true).unwrap();
        let bak = PathBuf::from(format!("{}.bak", settings(&root).display()));
        assert_eq!(
            plans[0].action,
            Action::Replace {
                backup: bak.clone()
            }
        );
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "{ not json");
        assert_eq!(
            laya_cmds(&read_json(&settings(&root)), "UserPromptSubmit").len(),
            1
        );
        assert_eq!(
            read_json(&root.join(".mcp.json"))["mcpServers"]["laya-codex"]["command"],
            EXE
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_file_is_treated_as_empty_object() {
        let root = scratch("empty");
        std::fs::write(root.join(".mcp.json"), "  \n").unwrap();
        let plans = run(&root, Path::new(EXE), false, false, false).unwrap();
        assert_eq!(plans[1].action, Action::Update);
        assert_eq!(
            read_json(&root.join(".mcp.json"))["mcpServers"]["laya-codex"]["args"],
            json!(["mcp"])
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let root = scratch("dry");
        std::fs::write(root.join(".mcp.json"), r#"{"mcpServers": {}}"#).unwrap();
        let plans = run(&root, Path::new(EXE), false, true, false).unwrap();
        assert_eq!(
            plans.iter().map(|p| p.action.clone()).collect::<Vec<_>>(),
            [Action::Create, Action::Update]
        );
        assert!(
            plans[0]
                .content
                .contains("/opt/laya-codex/bin/laya-codex hook")
        );
        assert!(!root.join(".claude").exists());
        assert_eq!(
            std::fs::read_to_string(root.join(".mcp.json")).unwrap(),
            r#"{"mcpServers": {}}"#
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A file outside the repo that a hostile checkout tries to get overwritten.
    fn victim(tag: &str) -> PathBuf {
        let v = scratch(&format!("{tag}-victim")).join("victim.json");
        std::fs::write(&v, "precious").unwrap();
        v
    }

    fn refused(root: &Path, force: bool) -> String {
        let err = run(root, Path::new(EXE), false, false, force).unwrap_err();
        format!("{err:#}")
    }

    #[test]
    fn a_symlinked_settings_file_is_refused_and_nothing_is_written() {
        let root = scratch("symlink");
        let v = victim("symlink");
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::os::unix::fs::symlink(&v, settings(&root)).unwrap();
        for force in [false, true] {
            let msg = refused(&root, force);
            assert!(
                msg.contains("symlink") && msg.contains("settings.local.json"),
                "{msg}"
            );
        }
        // Dry runs refuse too (they would otherwise read and print the link's target).
        assert!(run(&root, Path::new(EXE), false, true, false).is_err());
        assert_eq!(std::fs::read_to_string(&v).unwrap(), "precious");
        assert!(!root.join(".mcp.json").exists());
        // A link that stays inside the repo is refused as well.
        std::fs::remove_file(settings(&root)).unwrap();
        std::fs::write(root.join("shared.json"), "{}").unwrap();
        std::os::unix::fs::symlink(root.join("shared.json"), settings(&root)).unwrap();
        assert!(refused(&root, false).contains("symlink"));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(v.parent().unwrap());
    }

    #[test]
    fn a_symlinked_mcp_json_is_refused() {
        let root = scratch("mcplink");
        let v = victim("mcplink");
        std::os::unix::fs::symlink(&v, root.join(".mcp.json")).unwrap();
        assert!(refused(&root, true).contains(".mcp.json"));
        assert_eq!(std::fs::read_to_string(&v).unwrap(), "precious");
        assert!(!settings(&root).exists());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(v.parent().unwrap());
    }

    #[test]
    fn a_symlinked_claude_dir_is_refused() {
        let root = scratch("dirlink");
        let outside = scratch("dirlink-outside");
        std::os::unix::fs::symlink(&outside, root.join(".claude")).unwrap();
        let msg = refused(&root, true);
        assert!(msg.contains(".claude") && msg.contains("symlink"), "{msg}");
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
        assert!(!root.join(".mcp.json").exists());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn a_symlinked_backup_path_is_refused() {
        let root = scratch("baklink");
        let v = victim("baklink");
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::write(settings(&root), "{ not json").unwrap();
        let bak = PathBuf::from(format!("{}.bak", settings(&root).display()));
        std::os::unix::fs::symlink(&v, &bak).unwrap();
        assert!(refused(&root, true).contains(".bak"));
        assert_eq!(std::fs::read_to_string(&v).unwrap(), "precious");
        assert_eq!(
            std::fs::read_to_string(settings(&root)).unwrap(),
            "{ not json"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(v.parent().unwrap());
    }

    #[test]
    fn a_non_regular_target_is_refused() {
        let root = scratch("nonreg");
        std::fs::create_dir_all(settings(&root)).unwrap();
        assert!(refused(&root, true).contains("not a regular file"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn planted_temp_file_symlinks_are_never_written_through() {
        let root = scratch("tmplink");
        let v = victim("tmplink");
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        // The temp names the old implementation used, pre-planted by a hostile checkout.
        std::os::unix::fs::symlink(&v, root.join(".claude/.settings.local.json.laya-tmp")).unwrap();
        std::os::unix::fs::symlink(&v, root.join("..mcp.json.laya-tmp")).unwrap();
        run(&root, Path::new(EXE), false, false, false).unwrap();
        assert_eq!(std::fs::read_to_string(&v).unwrap(), "precious");
        assert_eq!(
            laya_cmds(&read_json(&settings(&root)), "SessionStart").len(),
            1
        );
        assert!(read_json(&root.join(".mcp.json"))["mcpServers"]["laya-codex"].is_object());
        // No temp files are left behind.
        let leftovers: Vec<_> = std::fs::read_dir(root.join(".claude"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("laya-tmp") && n != ".settings.local.json.laya-tmp")
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(v.parent().unwrap());
    }

    #[test]
    fn an_existing_files_permissions_are_kept() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("perms");
        std::fs::write(root.join(".mcp.json"), "{}").unwrap();
        std::fs::set_permissions(
            root.join(".mcp.json"),
            std::fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        run(&root, Path::new(EXE), false, false, false).unwrap();
        let mode = std::fs::metadata(root.join(".mcp.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o640);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn fake_exe(p: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn path_of(dirs: &[&Path]) -> std::ffi::OsString {
        std::env::join_paths(dirs).unwrap()
    }

    #[test]
    fn program_is_bare_laya_only_when_path_resolves_to_this_binary() {
        let d = scratch("prog");
        let exe = d.join("install/laya-codex");
        fake_exe(&exe);
        let bin = d.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(&exe, bin.join("laya-codex")).unwrap();
        let other = d.join("other");
        fake_exe(&other.join("laya-codex"));
        let empty = d.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let bare = PathBuf::from("laya-codex");

        let p = path_of(&[&empty, &bin]);
        assert_eq!(program_for(&exe, Some(&p)), bare);
        // The first `laya-codex` on PATH is what the shell runs: a different binary there loses.
        let p = path_of(&[&other, &bin]);
        assert_eq!(program_for(&exe, Some(&p)), exe);
        // Not on PATH, no PATH, or a cwd-relative entry ahead of the match: machine path.
        assert_eq!(program_for(&exe, Some(&path_of(&[&empty]))), exe);
        assert_eq!(program_for(&exe, None), exe);
        let p = path_of(&[Path::new("."), &bin]);
        assert_eq!(program_for(&exe, Some(&p)), exe);
        // A non-executable `laya-codex` is skipped like the shell does.
        let noexec = d.join("noexec");
        std::fs::create_dir_all(&noexec).unwrap();
        std::fs::write(noexec.join("laya-codex"), "").unwrap();
        let p = path_of(&[&noexec, &bin]);
        assert_eq!(program_for(&exe, Some(&p)), bare);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn run_writes_the_bare_command_when_laya_on_path_is_this_binary() {
        let root = scratch("portable");
        let d = scratch("portable-bin");
        let exe = d.join("install/laya-codex");
        fake_exe(&exe);
        std::fs::create_dir_all(d.join("bin")).unwrap();
        std::os::unix::fs::symlink(&exe, d.join("bin/laya-codex")).unwrap();
        let path = path_of(&[&d.join("bin")]);
        run_with(&root, &exe, Some(&path), true, false, false).unwrap();
        assert_eq!(
            read_json(&root.join(".mcp.json")),
            json!({"mcpServers": {"laya-codex": {"command": "laya-codex", "args": ["mcp"]}}})
        );
        for (event, ..) in HOOK_EVENTS {
            assert_eq!(
                laya_cmds(&read_json(&settings(&root)), event),
                ["LAYA_CODEX_ADAPTIVE=1 laya-codex hook"]
            );
        }
        // Re-running from a machine where laya-codex is not on PATH replaces, never duplicates.
        run_with(&root, &exe, None, false, false, false).unwrap();
        assert_eq!(
            laya_cmds(&read_json(&settings(&root)), "SessionStart"),
            [format!("{} hook", exe.display())]
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unwritable_dir_is_an_error_not_a_panic() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("ro");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();
        let r = run(&root, Path::new(EXE), false, false, false);
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(r.is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
