//! `laya-codex doctor`: pass/warn/fail checks, each with a fix hint and a hard time bound.
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
use crate::config::{Config, INSTALL_SH, MOON_FIX, is_executable, model_candidates};
use crate::hook::DaemonApi;
use crate::init::{HOOK_EVENTS, command_program, is_laya_hook};
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
    fn new(
        name: &'static str,
        level: Level,
        detail: impl Into<String>,
        fix: Option<String>,
    ) -> Self {
        Check {
            name,
            level,
            detail: detail.into(),
            fix,
        }
    }
}

/// Moon binary: `running` = something already answers PING on the Moon port.
pub fn check_moon(bin: &Path, tried: &[PathBuf], running: bool, probe_timeout: Duration) -> Check {
    let fix = || Some(MOON_FIX.to_string());
    if !is_executable(bin) {
        let list = tried
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return if running {
            Check::new(
                "moon",
                Level::Warn,
                format!(
                    "a Moon answers on its port, but no binary was found (tried {list}); laya-codex cannot restart it"
                ),
                fix(),
            )
        } else {
            Check::new(
                "moon",
                Level::Fail,
                format!("moon binary not found; tried {list}"),
                fix(),
            )
        };
    }
    let state = if running {
        "running"
    } else {
        "not running; starts on demand"
    };
    match probe(bin, probe_timeout) {
        Ok(()) => Check::new(
            "moon",
            Level::Pass,
            format!("{} ({state})", bin.display()),
            None,
        ),
        Err(e) => {
            let level = if running { Level::Warn } else { Level::Fail };
            Check::new(
                "moon",
                level,
                format!("{} is not runnable: {e}", bin.display()),
                Some(format!(
                    "rebuild moon or point LAYA_CODEX_MOON_BIN at a working build. {MOON_FIX}"
                )),
            )
        }
    }
}

/// The server on the Moon port: `probe` is `(what answers, it is a password-less Moon an older
/// laya-codex started from this LAYA_CODEX_HOME)`, or why laya-codex's password could not be loaded.
pub fn check_auth(probe: Result<(laya_store::MoonProbe, bool), String>, port: u16) -> Check {
    use laya_store::MoonProbe;
    let other = || {
        Some(format!(
            "stop the other Moon on port {port}, or set LAYA_CODEX_MOON_PORT to a free port"
        ))
    };
    match probe {
        Err(e) => Check::new(
            "auth",
            Level::Fail,
            format!("cannot load laya-codex's Moon password: {e}"),
            Some("delete $LAYA_CODEX_HOME/moon.acl and run `laya-codex stop`; both are recreated".into()),
        ),
        Ok((MoonProbe::Ready, _)) => Check::new(
            "auth",
            Level::Pass,
            format!("laya-codex's password-protected Moon answers on port {port}"),
            None,
        ),
        Ok((MoonProbe::Down, _)) => Check::new(
            "auth",
            Level::Pass,
            format!("port {port} is free; laya-codex starts its password-protected Moon on demand"),
            None,
        ),
        Ok((MoonProbe::Unprotected, true)) => Check::new(
            "auth",
            Level::Warn,
            format!(
                "port {port} is served by a Moon an older laya-codex started without a password; the next daemon start replaces it"
            ),
            Some(
                "`laya-codex stop` (the daemon restarts on demand and replaces it, keeping the index)"
                    .into(),
            ),
        ),
        Ok((p, _)) => Check::new("auth", Level::Fail, laya_store::refusal(port, p), other()),
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
        return Check::new(
            "model",
            Level::Pass,
            "disabled (LAYA_CODEX_NO_MODEL=1): lexical-only ranking",
            None,
        );
    }
    let Some(dir) = dir else {
        let list = searched
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        // The laya-code re-ranker goes where it is looked for first.
        let dest = searched
            .first()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "$LAYA_CODEX_HOME/models/laya-code".into());
        return Check::new(
            "model",
            Level::Warn,
            format!(
                "no model found (looked for model.safetensors in {list}); ranking is lexical-only"
            ),
            Some(format!(
                "download and verify the laya-code re-ranker (~850 MB): `curl -fsSL {INSTALL_SH} | sh -s -- --model-only`, \
                 or `hf download tindang/laya-code --local-dir {dest}` (or set LAYA_CODEX_MODEL_DIR; LAYA_CODEX_NO_MODEL=1 silences this)"
            )),
        );
    };
    if let Err(e) = laya_model::ModelFiles::resolve(dir) {
        return Check::new(
            "model",
            Level::Warn,
            format!(
                "{} is incomplete ({e}); the daemon falls back to lexical-only",
                dir.display()
            ),
            Some(
                "re-download the model directory, or set LAYA_CODEX_MODEL_DIR to a complete one"
                    .into(),
            ),
        );
    }
    if cfg!(target_os = "macos") {
        Check::new(
            "model",
            Level::Pass,
            format!("{} (Metal)", dir.display()),
            None,
        )
    } else {
        Check::new(
            "model",
            Level::Warn,
            format!(
                "{} found, but it runs on CPU here: too slow for interactive re-ranking",
                dir.display()
            ),
            Some("set LAYA_CODEX_NO_MODEL=1 for lexical-only ranking".into()),
        )
    }
}

/// `LAYA_CODEX_HOME` exists (or can be created) and is writable.
pub fn check_home(home: &Path) -> Check {
    let probe = home.join(format!(".doctor-probe-{}", std::process::id()));
    let res = std::fs::create_dir_all(home)
        .and_then(|()| std::fs::write(&probe, b"ok"))
        .and_then(|()| std::fs::remove_file(&probe));
    match res {
        Ok(()) => Check::new(
            "home",
            Level::Pass,
            format!("{} is writable", home.display()),
            None,
        ),
        Err(e) => Check::new(
            "home",
            Level::Fail,
            format!("{} is not writable: {e}", home.display()),
            Some("make it writable, or set LAYA_CODEX_HOME to a writable directory".into()),
        ),
    }
}

/// Daemon ping result.
pub fn check_daemon(ping: Result<Response, String>, socket: &Path) -> Check {
    let ours = env!("CARGO_PKG_VERSION");
    match ping {
        Ok(Response::Pong { version, .. }) if version != ours => Check::new("daemon", Level::Warn,
            format!("running daemon is v{version}, this binary is v{ours}"),
            Some("restart it so it runs this build: `laya-codex stop` (it restarts on demand)".into())),
        Ok(Response::Pong { model_ready, .. }) => {
            let model = if model_ready { "model ready" } else { "model loading or lexical-only" };
            Check::new("daemon", Level::Pass, format!("up at {} ({model})", socket.display()), None)
        }
        Ok(other) => Check::new("daemon", Level::Warn, format!("unexpected ping reply {other:?}"), Some("restart it: `laya-codex stop`".into())),
        Err(e) => Check::new("daemon", Level::Warn, format!("not running at {} ({e}); it starts on demand at the first hook", socket.display()),
            Some("`laya-codex doctor --start` starts it now; if it does not come up, see $LAYA_CODEX_HOME/daemon.log".into())),
    }
}

/// Indexed file count of the repo in Moon.
pub fn check_index(files: Result<usize, String>, root: &Path) -> Check {
    let fix = Some(format!("laya-codex index {}", root.display()));
    match files {
        Ok(0) => Check::new(
            "index",
            Level::Warn,
            format!("{} has no indexed files", root.display()),
            fix,
        ),
        Ok(n) => Check::new(
            "index",
            Level::Pass,
            format!("{n} files indexed for {}", root.display()),
            None,
        ),
        Err(e) => Check::new(
            "index",
            Level::Warn,
            format!("cannot read the index: {e}"),
            fix,
        ),
    }
}

/// laya-codex hooks in `.claude/settings.local.json` / `.claude/settings.json`.
/// The Claude Code settings scope that enables the laya-codex plugin for `root` ("local",
/// "project" or "user"), or `None`. The most specific scope that mentions the plugin decides, as
/// in Claude Code: local settings override project settings, which override user settings.
pub fn plugin_enabled(root: &Path, home: &Path) -> Option<String> {
    let scopes = [
        ("local", root.join(".claude/settings.local.json")),
        ("project", root.join(".claude/settings.json")),
        ("user", home.join(".claude/settings.json")),
    ];
    for (scope, path) in scopes {
        let Some(v) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        else {
            continue;
        };
        let decided = v["enabledPlugins"].as_object().and_then(|m| {
            m.iter()
                .find(|(k, _)| k.starts_with("laya-codex@"))
                .and_then(|(_, on)| on.as_bool())
        });
        if let Some(on) = decided {
            return on.then(|| scope.to_string());
        }
    }
    None
}

fn user_home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// laya-codex's four hooks, from `laya-codex init` in the repo's `.claude` settings or from the plugin.
pub fn check_hooks(root: &Path) -> Check {
    check_hooks_with(root, &user_home())
}

pub fn check_hooks_with(root: &Path, home: &Path) -> Check {
    let init_fix = Some(format!("laya-codex init --repo {}", root.display()));
    let mut cmds: Vec<(&str, String)> = Vec::new();
    for name in ["settings.local.json", "settings.json"] {
        let path = root.join(".claude").join(name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                return Check::new(
                    "hooks",
                    Level::Fail,
                    format!("{} is not valid JSON: {e}", path.display()),
                    Some(format!(
                        "fix it by hand, or `laya-codex init --force --repo {}` (backs it up to .bak)",
                        root.display()
                    )),
                );
            }
        };
        for (event, ..) in HOOK_EVENTS {
            let handlers = v["hooks"][event]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|g| g["hooks"].as_array().into_iter().flatten());
            cmds.extend(
                handlers
                    .filter_map(|h| h["command"].as_str())
                    .filter(|c| is_laya_hook(c))
                    .map(|c| (event, c.to_string())),
            );
        }
    }
    if cmds.is_empty()
        && let Some(scope) = plugin_enabled(root, home)
    {
        return Check::new(
            "hooks",
            Level::Pass,
            format!(
                "all {} events via the laya-codex plugin ({scope} settings)",
                HOOK_EVENTS.len()
            ),
            None,
        );
    }
    let missing: Vec<&str> = HOOK_EVENTS
        .iter()
        .map(|e| e.0)
        .filter(|e| !cmds.iter().any(|(ev, _)| ev == e))
        .collect();
    if !missing.is_empty() {
        return Check::new(
            "hooks",
            Level::Fail,
            format!(
                "laya-codex hooks missing for {} in {}/.claude",
                missing.join(", "),
                root.display()
            ),
            init_fix.map(|f| {
                format!("{f}, or install the Claude Code plugin: /plugin marketplace add pilotspace/laya-codex")
            }),
        );
    }
    if let Some((_, exe)) = cmds
        .iter()
        .map(|(_, c)| (c, hook_exe(c)))
        .find(|(_, exe)| !exe_found(exe))
    {
        return Check::new(
            "hooks",
            Level::Fail,
            format!("a laya-codex hook runs {exe}, which is not an executable"),
            init_fix,
        );
    }
    Check::new(
        "hooks",
        Level::Pass,
        format!("all {} events -> {}", HOOK_EVENTS.len(), cmds[0].1),
        None,
    )
}

/// The executable of a laya-codex hook command, parsed the way `is_laya_hook` reads it:
/// `NAME=value` assignments dropped (values may be paths), shell quotes removed.
fn hook_exe(cmd: &str) -> String {
    command_program(cmd).unwrap_or_else(|| cmd.trim().to_string())
}

fn exe_found(exe: &str) -> bool {
    if exe.contains('/') {
        return is_executable(Path::new(exe));
    }
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| is_executable(&d.join(exe))))
}

/// `mcpServers.laya-codex` in `.mcp.json`, or the plugin's MCP server.
pub fn check_mcp(root: &Path) -> Check {
    check_mcp_with(root, &user_home())
}

pub fn check_mcp_with(root: &Path, home: &Path) -> Check {
    let path = root.join(".mcp.json");
    let v: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    match v["mcpServers"]["laya-codex"]["command"].as_str() {
        Some(cmd) => Check::new(
            "mcp",
            Level::Pass,
            format!("mcpServers.laya-codex -> {cmd}"),
            None,
        ),
        None if plugin_enabled(root, home).is_some() => Check::new(
            "mcp",
            Level::Pass,
            "laya-codex server via the laya-codex plugin",
            None,
        ),
        None => Check::new(
            "mcp",
            Level::Warn,
            format!(
                "no laya-codex server in {} (the search tool is unavailable)",
                path.display()
            ),
            Some(format!("laya-codex init --repo {}", root.display())),
        ),
    }
}

/// Run `f` on a worker thread; a check that overruns `timeout` fails instead of hanging doctor.
pub fn bounded(
    name: &'static str,
    timeout: Duration,
    f: impl FnOnce() -> Check + Send + 'static,
) -> Check {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(timeout) {
        Ok(c) => c,
        Err(RecvTimeoutError::Timeout) => Check::new(
            name,
            Level::Fail,
            format!("check timed out after {timeout:?}"),
            None,
        ),
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
    out.push_str(&format!(
        "{} passed, {} warnings, {} failed\n",
        n(Level::Pass),
        n(Level::Warn),
        n(Level::Fail)
    ));
    out
}

/// Gather the real inputs and run every check. `start` lets the daemon ping autostart it.
pub fn run(cfg: &Config, root: &Path, start: bool) -> Vec<Check> {
    let sup = crate::config::supervisor(cfg).map_err(|e| format!("{e:#}"));
    let probe = sup
        .as_ref()
        .map(|s| (s.probe(), s.is_legacy()))
        .map_err(Clone::clone);
    let moon_running = matches!(probe, Ok((laya_store::MoonProbe::Ready, _)));
    let mut checks = Vec::new();

    let home = cfg.home.clone();
    checks.push(bounded("home", Duration::from_secs(3), move || {
        check_home(&home)
    }));
    let (bin, tried) = (cfg.moon_bin.clone(), cfg.moon_tried.clone());
    checks.push(bounded("moon", Duration::from_secs(4), move || {
        check_moon(&bin, &tried, moon_running, Duration::from_secs(2))
    }));
    checks.push(check_auth(probe, cfg.moon_port));
    let (use_model, dir, searched) = (
        cfg.use_model,
        cfg.model_dir.clone(),
        model_candidates(&cfg.home),
    );
    checks.push(bounded("model", Duration::from_secs(3), move || {
        check_model(use_model, dir.as_deref(), &searched)
    }));

    let socket = cfg.socket_path();
    let wait = if start {
        Duration::from_secs(20)
    } else {
        Duration::from_secs(2)
    };
    checks.push(bounded(
        "daemon",
        wait + Duration::from_secs(2),
        move || {
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
        },
    ));

    // Re-checked here: `--start` may have brought Moon up with the daemon.
    let repo = root.to_path_buf();
    let sc = crate::config::store_config(cfg).map_err(|e| format!("{e:#}"));
    checks.push(bounded("index", Duration::from_secs(5), move || {
        let files = match (sup, sc) {
            (Ok(sup), Ok(mut sc)) if sup.is_running() => {
                sc.query_timeout = Duration::from_secs(3);
                sc.max_retries = 0;
                laya_store::MoonStore::new(sc)
                    .and_then(|s| s.list_files(&laya_store::repo_id(&repo)))
                    .map(|f| f.len())
                    .map_err(|e| e.to_string())
            }
            (Err(e), _) | (_, Err(e)) => Err(e),
            _ => Err("laya-codex's Moon is not running".to_string()),
        };
        check_index(files, &repo)
    }));

    let r = root.to_path_buf();
    checks.push(bounded("hooks", Duration::from_secs(3), move || {
        check_hooks(&r)
    }));
    let r = root.to_path_buf();
    checks.push(bounded("mcp", Duration::from_secs(3), move || {
        check_mcp(&r)
    }));
    checks
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hook_exe_skips_env_assignments_whose_values_are_paths() {
        assert_eq!(
            hook_exe(
                "LAYA_CODEX_HOME=/tmp/h LAYA_CODEX_MOON_BIN=/tmp/w.sh /usr/local/bin/laya-codex hook"
            ),
            "/usr/local/bin/laya-codex"
        );
    }

    #[test]
    fn hook_exe_reads_plain_quoted_and_escaped_programs() {
        assert_eq!(hook_exe("laya-codex hook"), "laya-codex");
        assert_eq!(
            hook_exe("LAYA_CODEX_ADAPTIVE=1 laya-codex hook"),
            "laya-codex"
        );
        assert_eq!(hook_exe("X=1 '/a b/laya-codex' hook"), "/a b/laya-codex");
        assert_eq!(hook_exe(r"'/it'\''s/laya-codex' hook"), "/it's/laya-codex");
        // A path containing `=` is a program, not an assignment.
        assert_eq!(hook_exe("/opt/a=b/laya-codex hook"), "/opt/a=b/laya-codex");
    }

    #[test]
    fn hooks_with_env_prefixed_commands_pass_when_the_program_exists() {
        let root = scratch("envprefix");
        let exe = root.join("bin/laya-codex");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        script(&exe, "");
        let cmd = format!(
            "LAYA_CODEX_HOME={} LAYA_CODEX_MEMO=0 {} hook",
            root.join("home").display(),
            exe.display()
        );
        let hooks: serde_json::Map<String, Value> = HOOK_EVENTS
            .iter()
            .map(|(e, ..)| {
                (
                    e.to_string(),
                    json!([{"hooks": [{"type": "command", "command": cmd}]}]),
                )
            })
            .collect();
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::write(
            root.join(".claude/settings.local.json"),
            json!({ "hooks": hooks }).to_string(),
        )
        .unwrap();
        let c = check_hooks_with(&root, &root.join("nohome"));
        assert_eq!(c.level, Level::Pass, "{}", c.detail);
    }

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
        assert!(
            c.detail.contains("/nope/a/moon") && c.detail.contains("/nope/b/moon"),
            "{}",
            c.detail
        );
        assert!(c.fix.as_deref().unwrap().contains("LAYA_CODEX_MOON_BIN"));
        // Already running without a binary: works now, cannot be restarted.
        assert_eq!(check_moon(&tried[1], &tried, true, T).level, Level::Warn);
    }

    #[test]
    fn moon_binary_is_probed_for_runnability() {
        let d = scratch("moon");
        let ok = d.join("moon-ok");
        script(&ok, "exit 0");
        assert_eq!(
            check_moon(&ok, std::slice::from_ref(&ok), false, T).level,
            Level::Pass
        );
        assert_eq!(
            check_moon(&ok, std::slice::from_ref(&ok), true, T).level,
            Level::Pass
        );

        let bad = d.join("moon-bad");
        script(&bad, "exit 3");
        let c = check_moon(&bad, std::slice::from_ref(&bad), false, T);
        assert_eq!(c.level, Level::Fail, "{c:?}");

        let hang = d.join("moon-hang");
        script(&hang, "sleep 30");
        let t0 = std::time::Instant::now();
        let c = check_moon(
            &hang,
            std::slice::from_ref(&hang),
            false,
            Duration::from_millis(300),
        );
        assert_eq!(c.level, Level::Fail, "{c:?}");
        assert!(t0.elapsed() < Duration::from_secs(5));

        let plain = d.join("moon-plain");
        std::fs::write(&plain, "").unwrap();
        assert_eq!(
            check_moon(&plain, std::slice::from_ref(&plain), false, T).level,
            Level::Fail
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn model_checks_disabled_missing_incomplete_and_complete() {
        let d = scratch("model");
        assert_eq!(check_model(false, None, &[]).level, Level::Pass);

        let searched = [d.join("models/laya-code"), d.join("models/laya-base")];
        let c = check_model(true, None, &searched);
        assert_eq!(c.level, Level::Warn);
        assert!(
            c.detail.contains("lexical-only") && c.detail.contains("laya-base"),
            "{}",
            c.detail
        );
        let fix = c.fix.as_deref().unwrap();
        assert!(
            fix.contains("https://raw.githubusercontent.com/pilotspace/laya-codex/main/install.sh | sh -s -- --model-only"),
            "{fix}"
        );
        let hf = format!(
            "hf download tindang/laya-code --local-dir {}",
            searched[0].display()
        );
        assert!(fix.contains(&hf), "{fix}");

        let m = d.join("m");
        for f in [
            "model.safetensors",
            "encoder/config.json",
            "rl_agent_config.json",
        ] {
            std::fs::create_dir_all(m.join(f).parent().unwrap()).unwrap();
            std::fs::write(m.join(f), "").unwrap();
        }
        let c = check_model(true, Some(&m), &[]);
        assert_eq!(c.level, Level::Warn);
        assert!(c.detail.contains("tokenizer"), "{}", c.detail);

        std::fs::create_dir_all(m.join("tokenizer")).unwrap();
        std::fs::write(m.join("tokenizer/tokenizer.json"), "").unwrap();
        let c = check_model(true, Some(&m), &[]);
        let want = if cfg!(target_os = "macos") {
            Level::Pass
        } else {
            Level::Warn
        };
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
        assert!(c.fix.as_deref().unwrap().contains("LAYA_CODEX_HOME"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn daemon_down_is_a_warning_and_version_skew_is_flagged() {
        let sock = Path::new("/tmp/x.sock");
        let me = env!("CARGO_PKG_VERSION").to_string();
        assert_eq!(
            check_daemon(
                Ok(Response::Pong {
                    model_ready: true,
                    version: me
                }),
                sock
            )
            .level,
            Level::Pass
        );
        assert_eq!(
            check_daemon(
                Ok(Response::Pong {
                    model_ready: false,
                    version: "0.0.1".into()
                }),
                sock
            )
            .level,
            Level::Warn
        );
        let c = check_daemon(Err("connection refused".into()), sock);
        assert_eq!(c.level, Level::Warn);
        assert!(c.detail.contains("not running"), "{}", c.detail);
    }

    #[test]
    fn the_moon_on_the_port_must_be_layas_protected_one() {
        use laya_store::MoonProbe;
        let ok = |p| check_auth(Ok((p, false)), 7).level;
        assert_eq!(ok(MoonProbe::Ready), Level::Pass);
        assert_eq!(ok(MoonProbe::Down), Level::Pass);
        let c = check_auth(Ok((MoonProbe::Unprotected, false)), 7);
        assert_eq!(c.level, Level::Fail);
        assert!(
            c.detail.contains("port 7") && c.detail.contains("without laya-codex's password"),
            "{}",
            c.detail
        );
        assert!(c.fix.as_deref().unwrap().contains("LAYA_CODEX_MOON_PORT"));
        assert_eq!(ok(MoonProbe::WrongPassword), Level::Fail);
        // A Moon an older laya-codex started is replaced at the next daemon start.
        let c = check_auth(Ok((MoonProbe::Unprotected, true)), 7);
        assert_eq!(c.level, Level::Warn);
        assert!(c.fix.as_deref().unwrap().contains("laya-codex stop"));
        let c = check_auth(Err("moon.acl is a symlink".into()), 7);
        assert_eq!(c.level, Level::Fail);
    }

    #[test]
    fn index_counts_files() {
        let root = Path::new("/r");
        let c = check_index(Ok(12), root);
        assert_eq!(c.level, Level::Pass);
        assert!(c.detail.contains("12 files"));
        let c = check_index(Ok(0), root);
        assert_eq!(c.level, Level::Warn);
        assert!(c.fix.as_deref().unwrap().contains("laya-codex index /r"));
        assert_eq!(
            check_index(Err("moon down".into()), root).level,
            Level::Warn
        );
    }

    #[test]
    fn hooks_and_mcp_reflect_laya_init() {
        let root = scratch("hooks");
        let home = scratch("hooks-home"); // never the developer's real ~/.claude
        let check_hooks = |r: &Path| check_hooks_with(r, &home);
        let check_mcp = |r: &Path| check_mcp_with(r, &home);
        assert_eq!(check_hooks(&root).level, Level::Fail);
        assert_eq!(check_mcp(&root).level, Level::Warn);

        // A laya-codex binary that exists, so the command's executable check passes.
        let exe = root.join("bin/laya-codex");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        script(&exe, "exit 0");
        crate::init::run(&root, &exe, false, false, false).unwrap();
        assert_eq!(
            check_hooks(&root),
            Check {
                name: "hooks",
                level: Level::Pass,
                detail: check_hooks(&root).detail,
                fix: None
            }
        );
        let m = check_mcp(&root);
        assert_eq!(m.level, Level::Pass);
        assert!(
            m.detail.starts_with("mcpServers.laya-codex -> "),
            "{}",
            m.detail
        );

        // A hook pointing at a binary that no longer exists.
        std::fs::remove_file(&exe).unwrap();
        let c = check_hooks(&root);
        assert_eq!(c.level, Level::Fail);
        assert!(c.detail.contains("bin/laya-codex"), "{}", c.detail);

        // Partially installed: only some events.
        let s = root.join(".claude/settings.local.json");
        std::fs::write(&s, r#"{"hooks": {"UserPromptSubmit": [{"hooks": [{"type": "command", "command": "laya-codex hook"}]}]}}"#).unwrap();
        let c = check_hooks(&root);
        assert_eq!(c.level, Level::Fail);
        assert!(
            c.detail.contains("SessionStart") && !c.detail.contains("UserPromptSubmit"),
            "{}",
            c.detail
        );

        std::fs::write(&s, "{ broken").unwrap();
        let c = check_hooks(&root);
        assert_eq!(c.level, Level::Fail);
        assert!(c.detail.contains("settings.local.json"), "{}", c.detail);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_claude_code_plugin_counts_as_hooks_and_mcp() {
        let root = scratch("plugin-root");
        let home = scratch("plugin-home");
        let key = "laya-codex@laya-codex";
        let write = |p: &Path, v: Value| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, v.to_string()).unwrap();
        };
        assert_eq!(plugin_enabled(&root, &home), None);
        assert_eq!(check_hooks_with(&root, &home).level, Level::Fail);

        // Enabled for the user (all projects): hooks and MCP come from the plugin.
        write(
            &home.join(".claude/settings.json"),
            json!({"enabledPlugins": {key: true}}),
        );
        assert_eq!(plugin_enabled(&root, &home).as_deref(), Some("user"));
        let c = check_hooks_with(&root, &home);
        assert_eq!(c.level, Level::Pass, "{}", c.detail);
        assert!(c.detail.contains("plugin"), "{}", c.detail);
        assert_eq!(check_mcp_with(&root, &home).level, Level::Pass);

        // A project's local settings override the user's choice.
        write(
            &root.join(".claude/settings.local.json"),
            json!({"enabledPlugins": {key: false}}),
        );
        assert_eq!(plugin_enabled(&root, &home), None);
        assert_eq!(check_hooks_with(&root, &home).level, Level::Fail);

        // Enabled at project scope.
        write(
            &root.join(".claude/settings.local.json"),
            json!({"enabledPlugins": {key: true}}),
        );
        assert_eq!(plugin_enabled(&root, &home).as_deref(), Some("local"));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&home);
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
        assert_eq!(
            bounded("fast", T, || Check::new("fast", Level::Pass, "ok", None)).level,
            Level::Pass
        );
    }

    #[test]
    fn exit_code_and_render() {
        let pass = Check::new("a", Level::Pass, "fine", None);
        let warn = Check::new("b", Level::Warn, "meh", Some("do x".into()));
        let fail = Check::new("c", Level::Fail, "bad", Some("do y".into()));
        assert_eq!(exit_code(&[pass.clone(), warn.clone()]), 0);
        assert_eq!(exit_code(&[pass.clone(), fail.clone()]), 1);
        let out = render(&[pass, warn, fail]);
        assert!(
            out.contains("PASS") && out.contains("WARN") && out.contains("FAIL"),
            "{out}"
        );
        assert!(out.contains("do x") && out.contains("do y"), "{out}");
        let j = serde_json::to_value(Check::new("a", Level::Warn, "d", None)).unwrap();
        assert_eq!(
            j,
            serde_json::json!({"name": "a", "level": "warn", "detail": "d"})
        );
    }
}
