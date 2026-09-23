//! `laya doctor`: pass/warn/fail checks, each with a fix hint and a hard time bound.
//!
//! Check functions take their inputs (paths, probe results) as arguments so they are testable
//! without a daemon or Moon; [`run`] gathers the real inputs. Only `fail` sets exit code 1.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use laya_core::Store;
use serde::Serialize;
use serde_json::Value;

use crate::client::Client;
use crate::config::{Config, MOON_FIX, is_executable, model_candidates};
use crate::hook::DaemonApi;
use crate::init::{HOOK_EVENTS, is_laya_hook};
use crate::protocol::{Request, Response};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub level: Level,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Check {
    fn new(name: &'static str, level: Level, detail: impl Into<String>, fix: Option<String>) -> Self {
        Check { name, level, detail: detail.into(), fix }
    }
}

/// Moon binary: `running` = something already answers PING on the Moon port.
pub fn check_moon(bin: &Path, tried: &[PathBuf], running: bool, probe_timeout: Duration) -> Check {
    let fix = || Some(MOON_FIX.to_string());
    if !is_executable(bin) {
        let list = tried.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ");
        return if running {
            Check::new("moon", Level::Warn, format!("a Moon answers on its port, but no binary was found (tried {list}); laya cannot restart it"), fix())
        } else {
            Check::new("moon", Level::Fail, format!("moon binary not found; tried {list}"), fix())
        };
    }
    let state = if running { "running" } else { "not running; starts on demand" };
    match probe(bin, probe_timeout) {
        Ok(()) => Check::new("moon", Level::Pass, format!("{} ({state})", bin.display()), None),
        Err(e) => {
            let level = if running { Level::Warn } else { Level::Fail };
            Check::new("moon", level, format!("{} is not runnable: {e}", bin.display()), Some(format!("rebuild moon or point LAYA_MOON_BIN at a working build. {MOON_FIX}")))
        }
    }
}

/// Run `bin --help` and require a successful exit within `timeout` (killed otherwise).
fn probe(bin: &Path, timeout: Duration) -> Result<(), String> {
    let mut child = Command::new(bin)
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot execute: {e}"))?;
    let t0 = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(s)) if s.success() => return Ok(()),
            Ok(Some(s)) => return Err(format!("`--help` exited with {s}")),
            Ok(None) if t0.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("`--help` did not exit within {timeout:?}"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Model directory: `dir` is the resolved one (`None` = none found among `searched`).
pub fn check_model(use_model: bool, dir: Option<&Path>, searched: &[PathBuf]) -> Check {
    if !use_model {
        return Check::new("model", Level::Pass, "disabled (LAYA_NO_MODEL=1): lexical-only ranking", None);
    }
    let Some(dir) = dir else {
        let list = searched.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ");
        let base = searched.last().map(|p| p.display().to_string()).unwrap_or_else(|| "$LAYA_HOME/models/laya-base".into());
        return Check::new(
            "model",
            Level::Warn,
            format!("no model found (looked for model.safetensors in {list}); ranking is lexical-only"),
            Some(format!("hf download convaiinnovations/laya --local-dir {base}  (or set LAYA_MODEL_DIR; LAYA_NO_MODEL=1 silences this)")),
        );
    };
    if let Err(e) = laya_model::ModelFiles::resolve(dir) {
        return Check::new("model", Level::Warn, format!("{} is incomplete ({e}); the daemon falls back to lexical-only", dir.display()),
            Some("re-download the model directory, or set LAYA_MODEL_DIR to a complete one".into()));
    }
    if cfg!(target_os = "macos") {
        Check::new("model", Level::Pass, format!("{} (Metal)", dir.display()), None)
    } else {
        Check::new("model", Level::Warn, format!("{} found, but it runs on CPU here: too slow for interactive re-ranking", dir.display()),
            Some("set LAYA_NO_MODEL=1 for lexical-only ranking".into()))
    }
}

/// `LAYA_HOME` exists (or can be created) and is writable.
pub fn check_home(home: &Path) -> Check {
    let probe = home.join(format!(".doctor-probe-{}", std::process::id()));
    let res = std::fs::create_dir_all(home).and_then(|()| std::fs::write(&probe, b"ok")).and_then(|()| std::fs::remove_file(&probe));
    match res {
        Ok(()) => Check::new("home", Level::Pass, format!("{} is writable", home.display()), None),
        Err(e) => Check::new("home", Level::Fail, format!("{} is not writable: {e}", home.display()),
            Some("make it writable, or set LAYA_HOME to a writable directory".into())),
    }
}

/// Daemon ping result.
pub fn check_daemon(ping: Result<Response, String>, socket: &Path) -> Check {
    let ours = env!("CARGO_PKG_VERSION");
    match ping {
        Ok(Response::Pong { version, .. }) if version != ours => Check::new("daemon", Level::Warn,
            format!("running daemon is v{version}, this binary is v{ours}"),
            Some("restart it so it runs this build: `laya stop` (it restarts on demand)".into())),
        Ok(Response::Pong { model_ready, .. }) => {
            let model = if model_ready { "model ready" } else { "model loading or lexical-only" };
            Check::new("daemon", Level::Pass, format!("up at {} ({model})", socket.display()), None)
        }
        Ok(other) => Check::new("daemon", Level::Warn, format!("unexpected ping reply {other:?}"), Some("restart it: `laya stop`".into())),
        Err(e) => Check::new("daemon", Level::Warn, format!("not running at {} ({e}); it starts on demand at the first hook", socket.display()),
            Some("`laya doctor --start` starts it now; if it does not come up, see $LAYA_HOME/daemon.log".into())),
    }
}

/// Indexed file count of the repo in Moon.
pub fn check_index(files: Result<usize, String>, root: &Path) -> Check {
    let fix = Some(format!("laya index {}", root.display()));
    match files {
        Ok(0) => Check::new("index", Level::Warn, format!("{} has no indexed files", root.display()), fix),
        Ok(n) => Check::new("index", Level::Pass, format!("{n} files indexed for {}", root.display()), None),
        Err(e) => Check::new("index", Level::Warn, format!("cannot read the index: {e}"), fix),
    }
}

/// laya hooks in `.claude/settings.local.json` / `.claude/settings.json`.
pub fn check_hooks(root: &Path) -> Check {
    let init_fix = Some(format!("laya init --repo {}", root.display()));
    let mut cmds: Vec<(&str, String)> = Vec::new();
    for name in ["settings.local.json", "settings.json"] {
        let path = root.join(".claude").join(name);
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        if text.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                return Check::new("hooks", Level::Fail, format!("{} is not valid JSON: {e}", path.display()),
                    Some(format!("fix it by hand, or `laya init --force --repo {}` (backs it up to .bak)", root.display())));
            }
        };
        for (event, ..) in HOOK_EVENTS {
            let handlers = v["hooks"][event].as_array().into_iter().flatten().flat_map(|g| g["hooks"].as_array().into_iter().flatten());
            cmds.extend(handlers.filter_map(|h| h["command"].as_str()).filter(|c| is_laya_hook(c)).map(|c| (event, c.to_string())));
        }
    }
    let missing: Vec<&str> = HOOK_EVENTS.iter().map(|e| e.0).filter(|e| !cmds.iter().any(|(ev, _)| ev == e)).collect();
    if !missing.is_empty() {
        return Check::new("hooks", Level::Fail, format!("laya hooks missing for {} in {}/.claude", missing.join(", "), root.display()), init_fix);
    }
    if let Some((_, exe)) = cmds.iter().map(|(_, c)| (c, hook_exe(c))).find(|(_, exe)| !exe_found(exe)) {
        return Check::new("hooks", Level::Fail, format!("a laya hook runs {exe}, which is not an executable"), init_fix);
    }
    Check::new("hooks", Level::Pass, format!("all {} events -> {}", HOOK_EVENTS.len(), cmds[0].1), None)
}

/// The executable of a laya hook command: env assignments dropped, shell quotes removed.
fn hook_exe(cmd: &str) -> String {
    let mut head = cmd.trim().strip_suffix(" hook").unwrap_or(cmd).trim();
    while let Some((tok, rest)) = head.split_once(' ')
        && tok.contains('=')
        && !tok.starts_with('\'')
        && !tok.contains('/')
    {
        head = rest.trim_start();
    }
    match head.strip_prefix('\'').and_then(|h| h.strip_suffix('\'')) {
        Some(inner) => inner.replace(r"'\''", "'"),
        None => head.to_string(),
    }
}

fn exe_found(exe: &str) -> bool {
    if exe.contains('/') {
        return is_executable(Path::new(exe));
    }
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| is_executable(&d.join(exe))))
}

/// `mcpServers.laya` in `.mcp.json`.
pub fn check_mcp(root: &Path) -> Check {
    let path = root.join(".mcp.json");
    let v: Value = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    match v["mcpServers"]["laya"]["command"].as_str() {
        Some(cmd) => Check::new("mcp", Level::Pass, format!("mcpServers.laya -> {cmd}"), None),
        None => Check::new("mcp", Level::Warn, format!("no laya server in {} (the laya_search tool is unavailable)", path.display()),
            Some(format!("laya init --repo {}", root.display()))),
    }
}

/// Run `f` on a worker thread; a check that overruns `timeout` fails instead of hanging doctor.
pub fn bounded(name: &'static str, timeout: Duration, f: impl FnOnce() -> Check + Send + 'static) -> Check {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(timeout) {
        Ok(c) => c,
        Err(RecvTimeoutError::Timeout) => Check::new(name, Level::Fail, format!("check timed out after {timeout:?}"), None),
        Err(RecvTimeoutError::Disconnected) => Check::new(name, Level::Fail, "check crashed", None),
    }
}

#[must_use]
pub fn exit_code(checks: &[Check]) -> i32 {
    i32::from(checks.iter().any(|c| c.level == Level::Fail))
}

/// Human-readable report: one line per check, fix hints indented under non-passing checks.
pub fn render(checks: &[Check]) -> String {
    let mut out = String::new();
    for c in checks {
        let tag = match c.level {
            Level::Pass => "PASS",
            Level::Warn => "WARN",
            Level::Fail => "FAIL",
        };
        out.push_str(&format!("{tag}  {:<7} {}\n", c.name, c.detail));
        if let Some(fix) = &c.fix {
            out.push_str(&format!("               fix: {fix}\n"));
        }
    }
    let n = |l: Level| checks.iter().filter(|c| c.level == l).count();
    out.push_str(&format!("{} passed, {} warnings, {} failed\n", n(Level::Pass), n(Level::Warn), n(Level::Fail)));
    out
}

/// Gather the real inputs and run every check. `start` lets the daemon ping autostart it.
pub fn run(cfg: &Config, root: &Path, start: bool) -> Vec<Check> {
    let sup = laya_store::MoonSupervisor::new(&cfg.moon_bin, cfg.moon_port, cfg.moon_dir());
    let moon_running = sup.is_running();
    let mut checks = Vec::new();

    let home = cfg.home.clone();
    checks.push(bounded("home", Duration::from_secs(3), move || check_home(&home)));
    let (bin, tried) = (cfg.moon_bin.clone(), cfg.moon_tried.clone());
    checks.push(bounded("moon", Duration::from_secs(4), move || check_moon(&bin, &tried, moon_running, Duration::from_secs(2))));
    let (use_model, dir, searched) = (cfg.use_model, cfg.model_dir.clone(), model_candidates(&cfg.home));
    checks.push(bounded("model", Duration::from_secs(3), move || check_model(use_model, dir.as_deref(), &searched)));

    let socket = cfg.socket_path();
    let wait = if start { Duration::from_secs(20) } else { Duration::from_secs(2) };
    checks.push(bounded("daemon", wait + Duration::from_secs(2), move || {
        // Autostart (with --start) on the first ping only, then poll without spawning again.
        let first = Client::new(&socket, Duration::from_secs(1), start).call(Request::Ping);
        let client = Client::new(&socket, Duration::from_secs(1), false);
        let t0 = Instant::now();
        let mut next = Some(first);
        let ping = loop {
            match next.take().unwrap_or_else(|| client.call(Request::Ping)) {
                Ok(r) => break Ok(r),
                Err(e) if !start || t0.elapsed() >= wait => break Err(format!("{e:#}")),
                Err(_) => std::thread::sleep(Duration::from_millis(250)),
            }
        };
        check_daemon(ping, &socket)
    }));

    // Re-checked here: `--start` may have brought Moon up with the daemon.
    let (port, repo) = (cfg.moon_port, root.to_path_buf());
    checks.push(bounded("index", Duration::from_secs(5), move || {
        let files = if sup.is_running() {
            let mut sc = laya_store::StoreConfig::local(port);
            sc.query_timeout = Duration::from_secs(3);
            sc.max_retries = 0;
            laya_store::MoonStore::new(sc)
                .and_then(|s| s.list_files(&laya_store::repo_id(&repo)))
                .map(|f| f.len())
                .map_err(|e| e.to_string())
        } else {
            Err("Moon is not running".to_string())
        };
        check_index(files, &repo)
    }));

    let r = root.to_path_buf();
    checks.push(bounded("hooks", Duration::from_secs(3), move || check_hooks(&r)));
    let r = root.to_path_buf();
    checks.push(bounded("mcp", Duration::from_secs(3), move || check_mcp(&r)));
    checks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("laya-doctor-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn script(p: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    const T: Duration = Duration::from_secs(2);

    #[test]
    fn moon_missing_fails_naming_every_path_tried() {
        let tried = [PathBuf::from("/nope/a/moon"), PathBuf::from("/nope/b/moon")];
        let c = check_moon(&tried[1], &tried, false, T);
        assert_eq!(c.level, Level::Fail);
        assert!(c.detail.contains("/nope/a/moon") && c.detail.contains("/nope/b/moon"), "{}", c.detail);
        assert!(c.fix.as_deref().unwrap().contains("LAYA_MOON_BIN"));
        // Already running without a binary: works now, cannot be restarted.
        assert_eq!(check_moon(&tried[1], &tried, true, T).level, Level::Warn);
    }

    #[test]
    fn moon_binary_is_probed_for_runnability() {
        let d = scratch("moon");
        let ok = d.join("moon-ok");
        script(&ok, "exit 0");
        assert_eq!(check_moon(&ok, std::slice::from_ref(&ok), false, T).level, Level::Pass);
        assert_eq!(check_moon(&ok, std::slice::from_ref(&ok), true, T).level, Level::Pass);

        let bad = d.join("moon-bad");
        script(&bad, "exit 3");
        let c = check_moon(&bad, std::slice::from_ref(&bad), false, T);
        assert_eq!(c.level, Level::Fail, "{c:?}");

        let hang = d.join("moon-hang");
        script(&hang, "sleep 30");
        let t0 = std::time::Instant::now();
        let c = check_moon(&hang, std::slice::from_ref(&hang), false, Duration::from_millis(300));
        assert_eq!(c.level, Level::Fail, "{c:?}");
        assert!(t0.elapsed() < Duration::from_secs(5));

        let plain = d.join("moon-plain");
        std::fs::write(&plain, "").unwrap();
        assert_eq!(check_moon(&plain, std::slice::from_ref(&plain), false, T).level, Level::Fail);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn model_checks_disabled_missing_incomplete_and_complete() {
        let d = scratch("model");
        assert_eq!(check_model(false, None, &[]).level, Level::Pass);

        let searched = [d.join("models/laya-code"), d.join("models/laya-base")];
        let c = check_model(true, None, &searched);
        assert_eq!(c.level, Level::Warn);
        assert!(c.detail.contains("lexical-only") && c.detail.contains("laya-base"), "{}", c.detail);
        assert!(c.fix.as_deref().unwrap().contains("hf download"));

        let m = d.join("m");
        for f in ["model.safetensors", "encoder/config.json", "rl_agent_config.json"] {
            std::fs::create_dir_all(m.join(f).parent().unwrap()).unwrap();
            std::fs::write(m.join(f), "").unwrap();
        }
        let c = check_model(true, Some(&m), &[]);
        assert_eq!(c.level, Level::Warn);
        assert!(c.detail.contains("tokenizer"), "{}", c.detail);

        std::fs::create_dir_all(m.join("tokenizer")).unwrap();
        std::fs::write(m.join("tokenizer/tokenizer.json"), "").unwrap();
        let c = check_model(true, Some(&m), &[]);
        let want = if cfg!(target_os = "macos") { Level::Pass } else { Level::Warn };
        assert_eq!(c.level, want, "{c:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn home_must_be_writable() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch("home");
        assert_eq!(check_home(&d.join("new/home")).level, Level::Pass);
        assert!(d.join("new/home").is_dir());
        let ro = d.join("ro");
        std::fs::create_dir_all(&ro).unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
        let c = check_home(&ro);
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(c.level, Level::Fail);
        assert!(c.fix.as_deref().unwrap().contains("LAYA_HOME"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn daemon_down_is_a_warning_and_version_skew_is_flagged() {
        let sock = Path::new("/tmp/x.sock");
        let me = env!("CARGO_PKG_VERSION").to_string();
        assert_eq!(check_daemon(Ok(Response::Pong { model_ready: true, version: me }), sock).level, Level::Pass);
        assert_eq!(check_daemon(Ok(Response::Pong { model_ready: false, version: "0.0.1".into() }), sock).level, Level::Warn);
        let c = check_daemon(Err("connection refused".into()), sock);
        assert_eq!(c.level, Level::Warn);
        assert!(c.detail.contains("not running"), "{}", c.detail);
    }

    #[test]
    fn index_counts_files() {
        let root = Path::new("/r");
        let c = check_index(Ok(12), root);
        assert_eq!(c.level, Level::Pass);
        assert!(c.detail.contains("12 files"));
        let c = check_index(Ok(0), root);
        assert_eq!(c.level, Level::Warn);
        assert!(c.fix.as_deref().unwrap().contains("laya index /r"));
        assert_eq!(check_index(Err("moon down".into()), root).level, Level::Warn);
    }

    #[test]
    fn hooks_and_mcp_reflect_laya_init() {
        let root = scratch("hooks");
        assert_eq!(check_hooks(&root).level, Level::Fail);
        assert_eq!(check_mcp(&root).level, Level::Warn);

        // A laya binary that exists, so the command's executable check passes.
        let exe = root.join("bin/laya");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        script(&exe, "exit 0");
        crate::init::run(&root, &exe, false, false, false).unwrap();
        assert_eq!(check_hooks(&root), Check { name: "hooks", level: Level::Pass, detail: check_hooks(&root).detail, fix: None });
        assert_eq!(check_mcp(&root).level, Level::Pass);

        // A hook pointing at a binary that no longer exists.
        std::fs::remove_file(&exe).unwrap();
        let c = check_hooks(&root);
        assert_eq!(c.level, Level::Fail);
        assert!(c.detail.contains("bin/laya"), "{}", c.detail);

        // Partially installed: only some events.
        let s = root.join(".claude/settings.local.json");
        std::fs::write(&s, r#"{"hooks": {"UserPromptSubmit": [{"hooks": [{"type": "command", "command": "laya hook"}]}]}}"#).unwrap();
        let c = check_hooks(&root);
        assert_eq!(c.level, Level::Fail);
        assert!(c.detail.contains("SessionStart") && !c.detail.contains("UserPromptSubmit"), "{}", c.detail);

        std::fs::write(&s, "{ broken").unwrap();
        let c = check_hooks(&root);
        assert_eq!(c.level, Level::Fail);
        assert!(c.detail.contains("settings.local.json"), "{}", c.detail);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bounded_turns_a_hang_into_a_failure() {
        let t0 = std::time::Instant::now();
        let c = bounded("slow", Duration::from_millis(100), || {
            std::thread::sleep(Duration::from_secs(5));
            Check::new("slow", Level::Pass, "late", None)
        });
        assert_eq!(c.level, Level::Fail);
        assert!(c.detail.contains("timed out"));
        assert!(t0.elapsed() < Duration::from_secs(2));
        assert_eq!(bounded("fast", T, || Check::new("fast", Level::Pass, "ok", None)).level, Level::Pass);
    }

    #[test]
    fn exit_code_and_render() {
        let pass = Check::new("a", Level::Pass, "fine", None);
        let warn = Check::new("b", Level::Warn, "meh", Some("do x".into()));
        let fail = Check::new("c", Level::Fail, "bad", Some("do y".into()));
        assert_eq!(exit_code(&[pass.clone(), warn.clone()]), 0);
        assert_eq!(exit_code(&[pass.clone(), fail.clone()]), 1);
        let out = render(&[pass, warn, fail]);
        assert!(out.contains("PASS") && out.contains("WARN") && out.contains("FAIL"), "{out}");
        assert!(out.contains("do x") && out.contains("do y"), "{out}");
        let j = serde_json::to_value(Check::new("a", Level::Warn, "d", None)).unwrap();
        assert_eq!(j, serde_json::json!({"name": "a", "level": "warn", "detail": "d"}));
    }
}
