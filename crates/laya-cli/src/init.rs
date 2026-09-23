//! `laya init`: enable laya for a repository by merging the Claude Code hooks into
//! `.claude/settings.local.json` and the `laya` MCP server into `.mcp.json`.
//!
//! Merging keeps every unrelated key, hook and server; only existing laya entries are replaced, so
//! a re-run yields byte-identical files. Unparseable JSON is never overwritten without `--force`
//! (which backs the original up to `*.bak` first). Both files are planned before either is written.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde_json::{Map, Value, json};

/// Hook events laya handles: (event, matcher, timeout seconds). Mirrors the README.
pub const HOOK_EVENTS: [(&str, Option<&str>, u64); 4] = [
    ("SessionStart", None, 5),
    ("UserPromptSubmit", None, 8),
    ("PreToolUse", Some("Read|Agent|Task"), 5),
    ("PostToolUse", Some("Edit|Write|MultiEdit|NotebookEdit"), 5),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Create,
    Update,
    Unchanged,
    /// The existing file was not mergeable JSON; it is backed up to `backup` and replaced.
    Replace { backup: PathBuf },
}

/// One file `laya init` will write (or would, under `--dry-run`).
#[derive(Debug, Clone)]
pub struct Planned {
    pub path: PathBuf,
    pub action: Action,
    /// Full new content (pretty JSON with a trailing newline).
    pub content: String,
}

/// The shell command Claude Code runs for every laya hook.
pub fn hook_command(exe: &Path, adaptive: bool) -> String {
    let prefix = if adaptive { "LAYA_ADAPTIVE=1 " } else { "" };
    format!("{prefix}{} hook", shell_quote(&exe.to_string_lossy()))
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+:@%,=".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Is `cmd` a laya hook command (`laya hook`, `/abs/laya hook`, `LAYA_X=1 '/a b/laya' hook`)?
pub fn is_laya_hook(cmd: &str) -> bool {
    let Some(head) = cmd.trim().strip_suffix(" hook") else { return false };
    let head = head.trim_end().trim_end_matches(['\'', '"']);
    head.rsplit(['/', ' ', '\'', '"']).next() == Some("laya")
}

fn object(v: Value) -> Result<Map<String, Value>, String> {
    match v {
        Value::Object(m) => Ok(m),
        _ => Err("the top level is not a JSON object".into()),
    }
}

/// Merge laya's hooks (running `cmd`) into a settings object; `Err` if its shape is not mergeable.
pub fn merge_settings(existing: Value, cmd: &str) -> Result<Value, String> {
    let mut root = object(existing)?;
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks.as_object_mut().ok_or("`hooks` is not an object")?;
    for (event, matcher, timeout) in HOOK_EVENTS {
        let groups = hooks.entry(event).or_insert_with(|| json!([]));
        let groups = groups.as_array_mut().ok_or_else(|| format!("`hooks.{event}` is not an array"))?;
        // Drop laya handlers everywhere; drop a group only if it held nothing but laya handlers.
        groups.retain_mut(|g| {
            let Some(hs) = g.get_mut("hooks").and_then(Value::as_array_mut) else { return true };
            let before = hs.len();
            hs.retain(|h| !h["command"].as_str().is_some_and(is_laya_hook));
            !(hs.is_empty() && hs.len() < before)
        });
        let mut group = Map::new();
        if let Some(m) = matcher {
            group.insert("matcher".into(), json!(m));
        }
        group.insert("hooks".into(), json!([{"type": "command", "command": cmd, "timeout": timeout}]));
        groups.push(Value::Object(group));
    }
    Ok(Value::Object(root))
}

/// Set `mcpServers.laya` in an `.mcp.json` object; `Err` if its shape is not mergeable.
pub fn merge_mcp(existing: Value, exe: &Path) -> Result<Value, String> {
    let mut root = object(existing)?;
    let servers = root.entry("mcpServers").or_insert_with(|| json!({}));
    let servers = servers.as_object_mut().ok_or("`mcpServers` is not an object")?;
    servers.insert("laya".into(), json!({"command": exe.to_string_lossy(), "args": ["mcp"]}));
    Ok(Value::Object(root))
}

/// Plan the new content of the JSON file at `path` via `merge`. Nothing is written.
pub fn plan_file(path: &Path, force: bool, merge: impl Fn(Value) -> Result<Value, String>) -> anyhow::Result<Planned> {
    let pretty = |v: &Value| serde_json::to_string_pretty(v).map(|s| s + "\n");
    let planned = |action, content| Planned { path: path.to_path_buf(), action, content };
    let text = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let new = merge(json!({})).map_err(anyhow::Error::msg)?;
            return Ok(planned(Action::Create, pretty(&new)?));
        }
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let parsed = if text.trim().is_empty() { Ok(json!({})) } else { serde_json::from_str::<Value>(&text).map_err(|e| e.to_string()) };
    match parsed.and_then(|old| merge(old.clone()).map(|new| (old, new))) {
        Ok((old, new)) if old == new => Ok(planned(Action::Unchanged, text)),
        Ok((_, new)) => Ok(planned(Action::Update, pretty(&new)?)),
        Err(reason) => {
            let backup = PathBuf::from(format!("{}.bak", path.display()));
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

/// Write a planned file (atomically: temp file + rename; through a symlink to its target).
pub fn apply(p: &Planned) -> anyhow::Result<()> {
    if p.action == Action::Unchanged {
        return Ok(());
    }
    let is_link = std::fs::symlink_metadata(&p.path).is_ok_and(|m| m.file_type().is_symlink());
    let target = if is_link { std::fs::canonicalize(&p.path).context("resolve symlink")? } else { p.path.clone() };
    if let Action::Replace { backup } = &p.action {
        std::fs::copy(&target, backup).with_context(|| format!("back up to {}", backup.display()))?;
    }
    let dir = target.parent().context("no parent directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let name = target.file_name().context("no file name")?.to_string_lossy();
    let tmp = dir.join(format!(".{name}.laya-tmp"));
    let res = std::fs::write(&tmp, &p.content)
        .and_then(|()| match std::fs::metadata(&target) {
            Ok(m) => std::fs::set_permissions(&tmp, m.permissions()),
            Err(_) => Ok(()),
        })
        .and_then(|()| std::fs::rename(&tmp, &target));
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res.with_context(|| format!("write {}", target.display()))
}

/// Plan both files for `root`; write them unless `dry_run`. Nothing is written unless both plan
/// cleanly, and a failed second write rolls the first one back.
pub fn run(root: &Path, exe: &Path, adaptive: bool, dry_run: bool, force: bool) -> anyhow::Result<Vec<Planned>> {
    let cmd = hook_command(exe, adaptive);
    let plans = vec![
        plan_file(&root.join(".claude").join("settings.local.json"), force, |v| merge_settings(v, &cmd))?,
        plan_file(&root.join(".mcp.json"), force, |v| merge_mcp(v, exe))?,
    ];
    if dry_run {
        return Ok(plans);
    }
    let originals: Vec<Option<Vec<u8>>> = plans.iter().map(|p| std::fs::read(&p.path).ok()).collect();
    for (i, p) in plans.iter().enumerate() {
        if let Err(e) = apply(p) {
            for (done, orig) in plans[..i].iter().zip(&originals) {
                let _ = match orig {
                    Some(bytes) => std::fs::write(&done.path, bytes),
                    None => std::fs::remove_file(&done.path),
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

    const EXE: &str = "/opt/laya/bin/laya";

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
        assert_eq!(hook_command(Path::new(EXE), false), "/opt/laya/bin/laya hook");
        assert_eq!(hook_command(Path::new(EXE), true), "LAYA_ADAPTIVE=1 /opt/laya/bin/laya hook");
        assert_eq!(hook_command(Path::new("/My Tools/it's/laya"), false), r"'/My Tools/it'\''s/laya' hook");
    }

    #[test]
    fn recognises_laya_hook_commands_only() {
        for c in ["laya hook", "/opt/laya/bin/laya hook", "LAYA_ADAPTIVE=1 /x/laya hook", "'/a b/laya' hook", " laya hook "] {
            assert!(is_laya_hook(c), "{c}");
        }
        for c in ["", "laya", "laya mcp", "/x/notlaya hook", "echo hook", "laya hook && rm -rf /", "my-hook"] {
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
            assert_eq!(groups[0]["hooks"], json!([{"type": "command", "command": "/opt/laya/bin/laya hook", "timeout": timeout}]));
        }
        let m = read_json(&root.join(".mcp.json"));
        assert_eq!(m, json!({"mcpServers": {"laya": {"command": EXE, "args": ["mcp"]}}}));
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
                    {"matcher": "Read|Agent|Task", "hooks": [{"type": "command", "command": "laya hook", "timeout": 5}]},
                    {"matcher": "Write", "hooks": [{"type": "command", "command": "fmt.sh"}, {"type": "command", "command": "LAYA_ADAPTIVE=1 laya hook"}]}
                ],
                "Stop": [{"hooks": [{"type": "command", "command": "notify.sh"}]}]
            },
            "zeta": 1
        });
        std::fs::write(settings(&root), serde_json::to_string_pretty(&before).unwrap()).unwrap();
        std::fs::write(root.join(".mcp.json"), r#"{"mcpServers": {"other": {"command": "x"}, "laya": {"command": "laya", "args": ["mcp"], "env": {}}}}"#).unwrap();

        let plans = run(&root, Path::new(EXE), true, false, false).unwrap();
        assert!(plans.iter().all(|p| p.action == Action::Update));
        let s = read_json(&settings(&root));
        assert_eq!(s["permissions"], before["permissions"]);
        assert_eq!(s["zeta"], 1);
        assert_eq!(s["hooks"]["Stop"], before["hooks"]["Stop"]);
        let pre = s["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre[0], before["hooks"]["PreToolUse"][0]);
        // The laya handler is removed from the mixed group, which keeps its own hook.
        assert_eq!(pre[1], json!({"matcher": "Write", "hooks": [{"type": "command", "command": "fmt.sh"}]}));
        assert_eq!(pre.len(), 3);
        for (event, ..) in HOOK_EVENTS {
            assert_eq!(laya_cmds(&s, event), vec!["LAYA_ADAPTIVE=1 /opt/laya/bin/laya hook"], "{event}");
        }
        // Key order of the user's file is preserved.
        let keys: Vec<&String> = s.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["permissions", "hooks", "zeta"]);
        let m = read_json(&root.join(".mcp.json"));
        assert_eq!(m["mcpServers"]["other"], json!({"command": "x"}));
        assert_eq!(m["mcpServers"]["laya"], json!({"command": EXE, "args": ["mcp"]}));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rerun_is_idempotent() {
        let root = scratch("idem");
        run(&root, Path::new(EXE), false, false, false).unwrap();
        let first = (std::fs::read(settings(&root)).unwrap(), std::fs::read(root.join(".mcp.json")).unwrap());
        let plans = run(&root, Path::new(EXE), false, false, false).unwrap();
        assert!(plans.iter().all(|p| p.action == Action::Unchanged));
        let second = (std::fs::read(settings(&root)).unwrap(), std::fs::read(root.join(".mcp.json")).unwrap());
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
        assert!(msg.contains("settings.local.json") && msg.contains("--force"), "{msg}");
        assert_eq!(std::fs::read_to_string(settings(&root)).unwrap(), "{ not json");
        assert!(!root.join(".mcp.json").exists(), "nothing is written when any file is refused");

        // Valid JSON of the wrong shape is refused the same way.
        std::fs::write(root.join(".mcp.json"), "[1]").unwrap();
        std::fs::write(settings(&root), r#"{"hooks": []}"#).unwrap();
        assert!(run(&root, Path::new(EXE), false, false, false).is_err());

        std::fs::write(settings(&root), "{ not json").unwrap();
        let plans = run(&root, Path::new(EXE), false, false, true).unwrap();
        let bak = PathBuf::from(format!("{}.bak", settings(&root).display()));
        assert_eq!(plans[0].action, Action::Replace { backup: bak.clone() });
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "{ not json");
        assert_eq!(laya_cmds(&read_json(&settings(&root)), "UserPromptSubmit").len(), 1);
        assert_eq!(read_json(&root.join(".mcp.json"))["mcpServers"]["laya"]["command"], EXE);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_file_is_treated_as_empty_object() {
        let root = scratch("empty");
        std::fs::write(root.join(".mcp.json"), "  \n").unwrap();
        let plans = run(&root, Path::new(EXE), false, false, false).unwrap();
        assert_eq!(plans[1].action, Action::Update);
        assert_eq!(read_json(&root.join(".mcp.json"))["mcpServers"]["laya"]["args"], json!(["mcp"]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let root = scratch("dry");
        std::fs::write(root.join(".mcp.json"), r#"{"mcpServers": {}}"#).unwrap();
        let plans = run(&root, Path::new(EXE), false, true, false).unwrap();
        assert_eq!(plans.iter().map(|p| p.action.clone()).collect::<Vec<_>>(), [Action::Create, Action::Update]);
        assert!(plans[0].content.contains("/opt/laya/bin/laya hook"));
        assert!(!root.join(".claude").exists());
        assert_eq!(std::fs::read_to_string(root.join(".mcp.json")).unwrap(), r#"{"mcpServers": {}}"#);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn writes_through_a_symlinked_settings_file() {
        let root = scratch("symlink");
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        let real = root.join("shared-settings.json");
        std::fs::write(&real, r#"{"model": "x"}"#).unwrap();
        std::os::unix::fs::symlink(&real, settings(&root)).unwrap();
        run(&root, Path::new(EXE), false, false, false).unwrap();
        assert!(std::fs::symlink_metadata(settings(&root)).unwrap().file_type().is_symlink());
        let v = read_json(&real);
        assert_eq!(v["model"], "x");
        assert_eq!(laya_cmds(&v, "SessionStart").len(), 1);
        let _ = std::fs::remove_dir_all(&root);
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
