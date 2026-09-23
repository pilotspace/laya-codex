//! Runtime configuration: environment variables over built-in defaults.
//!
//! | env | default |
//! |---|---|
//! | `LAYA_CODEX_HOME` | `~/.cache/laya-codex` |
//! | `LAYA_CODEX_MODEL_DIR` | `$LAYA_CODEX_HOME/models/laya-code`, else `$LAYA_CODEX_HOME/models/laya-base` |
//! | `LAYA_CODEX_MOON_BIN` | `moon` beside the `laya-codex` binary, else on PATH |
//! | `LAYA_CODEX_MOON_PORT` | `16379` |
//! | `LAYA_CODEX_MOON_START_SECS` | `30` (how long a spawned Moon may take to answer) |
//! | `LAYA_CODEX_NO_MODEL` | unset (set to `1` for lexical-only ranking) |
//! | `LAYA_CODEX_BUDGET_MS` | `1200` (Laya time budget per query) |
//! | `LAYA_CODEX_HOOK_LOG` | unset (JSONL log of hook actions, used by the benchmark) |

use std::path::{Path, PathBuf};

use anyhow::Context;

#[derive(Debug, Clone)]
pub struct Config {
    pub home: PathBuf,
    pub model_dir: Option<PathBuf>,
    pub moon_bin: PathBuf,
    /// Every location considered for the Moon binary, in order (for diagnostics).
    pub moon_tried: Vec<PathBuf>,
    pub moon_port: u16,
    /// How long a freshly spawned Moon may take to answer (it replays its index from disk).
    pub moon_start_timeout: std::time::Duration,
    pub use_model: bool,
    pub budget_ms: u64,
    pub hook_log: Option<PathBuf>,
}

impl Config {
    pub fn from_env() -> Self {
        let home = std::env::var_os("LAYA_CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".cache/laya-codex"));
        let model_dir = std::env::var_os("LAYA_CODEX_MODEL_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                model_candidates(&home)
                    .into_iter()
                    .find(|p| p.join("model.safetensors").exists())
            });
        let (moon_bin, moon_tried) = resolve_moon(
            std::env::var_os("LAYA_CODEX_MOON_BIN").map(PathBuf::from),
            std::env::current_exe().ok().as_deref(),
            std::env::var_os("PATH").as_deref(),
        );
        Config {
            moon_port: env_parse("LAYA_CODEX_MOON_PORT").unwrap_or(16379),
            moon_start_timeout: moon_start_timeout(
                std::env::var("LAYA_CODEX_MOON_START_SECS").ok().as_deref(),
            ),
            use_model: std::env::var("LAYA_CODEX_NO_MODEL")
                .map(|v| v != "1")
                .unwrap_or(true),
            budget_ms: env_parse("LAYA_CODEX_BUDGET_MS").unwrap_or(1200),
            hook_log: std::env::var_os("LAYA_CODEX_HOOK_LOG").map(PathBuf::from),
            home,
            model_dir,
            moon_bin,
            moon_tried,
        }
    }

    pub fn socket_path(&self) -> PathBuf {
        self.home.join("laya.sock")
    }

    pub fn moon_dir(&self) -> PathBuf {
        self.home.join("moon")
    }

    pub fn daemon_log(&self) -> PathBuf {
        self.home.join("daemon.log")
    }

    pub fn daemon_pidfile(&self) -> PathBuf {
        self.home.join("daemon.pid")
    }

    /// Held (`flock`) by the running daemon; see [`crate::sys::DaemonLock`].
    pub fn daemon_lock(&self) -> PathBuf {
        self.home.join("daemon.lock")
    }

    /// Moon's password, in Redis ACL format (mode 0600).
    pub fn moon_acl(&self) -> PathBuf {
        self.home.join("moon.acl")
    }
}

fn env_parse<T: std::str::FromStr>(k: &str) -> Option<T> {
    std::env::var(k).ok()?.parse().ok()
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// Locate the Moon binary: `LAYA_CODEX_MOON_BIN` alone when set, else, relative to the real
/// (symlink-resolved) location of the `laya-codex` executable `exe`, `moon` beside it (where
/// install.sh puts the pinned Moon) and `../libexec/moon` (the Homebrew formula's layout), then on
/// each `PATH` entry. Returns the chosen path (the last candidate when none exists, so the spawn
/// error names it) and every path tried.
pub fn resolve_moon(
    env_bin: Option<PathBuf>,
    exe: Option<&Path>,
    path_var: Option<&std::ffi::OsStr>,
) -> (PathBuf, Vec<PathBuf>) {
    if let Some(bin) = env_bin {
        return (bin.clone(), vec![bin]);
    }
    // The real location: a symlink (Homebrew's bin/laya-codex -> Cellar/...) would otherwise hide
    // the Moon installed with the binary.
    let exe = exe.map(|e| e.canonicalize().unwrap_or_else(|_| e.to_path_buf()));
    let mut tried: Vec<PathBuf> = Vec::new();
    if let Some(dir) = exe.as_deref().and_then(Path::parent) {
        tried.push(dir.join("moon"));
        if let Some(prefix) = dir.parent() {
            tried.push(prefix.join("libexec").join("moon"));
        }
    }
    if let Some(v) = path_var {
        for p in std::env::split_paths(v).map(|d| d.join("moon")) {
            if !tried.contains(&p) {
                tried.push(p);
            }
        }
    }
    match tried.iter().position(|p| is_executable(p)) {
        Some(i) => {
            tried.truncate(i + 1);
            (tried[i].clone(), tried)
        }
        None => (tried.last().cloned().unwrap_or_default(), tried),
    }
}

/// Is `p` a regular file with an execute bit?
pub fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Model directories searched, in order, when `LAYA_CODEX_MODEL_DIR` is unset.
pub fn model_candidates(home: &Path) -> Vec<PathBuf> {
    ["laya-code", "laya-base"]
        .iter()
        .map(|m| home.join("models").join(m))
        .collect()
}

/// The installer; `| sh -s -- --model-only` fetches just the laya-code re-ranker.
pub const INSTALL_SH: &str =
    "https://raw.githubusercontent.com/pilotspace/laya-codex/main/install.sh";

/// How to get a Moon binary.
pub const MOON_FIX: &str = "re-run the installer (https://github.com/pilotspace/laya-codex#install), which puts moon \
     beside laya-codex; or build moon (https://github.com/pilotspace/moon, `cargo build --release`) and put it on PATH, \
     or set LAYA_CODEX_MOON_BIN=/path/to/moon";

/// Actionable error for a Moon binary that cannot be found, naming every path tried.
pub fn moon_missing_message(tried: &[PathBuf]) -> String {
    let list: Vec<String> = tried
        .iter()
        .map(|p| format!("  - {}", p.display()))
        .collect();
    format!(
        "moon binary not found (Moon is the BM25 store laya-codex runs as a sidecar). Tried:\n{}\nFix: {MOON_FIX}",
        list.join("\n")
    )
}

/// Moon's password: read from `$LAYA_CODEX_HOME/moon.acl`, generated on first use. Also makes
/// `LAYA_CODEX_HOME` private (0700).
pub fn moon_password(cfg: &Config) -> anyhow::Result<laya_store::Password> {
    laya_store::create_private_dir(&cfg.home)
        .with_context(|| format!("LAYA_CODEX_HOME {}", cfg.home.display()))?;
    laya_store::load_or_create_acl(&cfg.moon_acl())
        .map_err(|e| anyhow::anyhow!("moon password: {e}"))
}

/// Startup time limit for a spawned Moon: `LAYA_CODEX_MOON_START_SECS` when it is a positive number
/// of seconds, else 30 s. Moon replays its whole index from disk before it answers, so a large
/// index or a slow machine needs far more than a fresh start.
pub fn moon_start_timeout(env: Option<&str>) -> std::time::Duration {
    let secs = env
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(30);
    std::time::Duration::from_secs(secs)
}

/// The supervisor of laya-codex's password-protected Moon.
pub fn supervisor(cfg: &Config) -> anyhow::Result<laya_store::MoonSupervisor> {
    Ok(
        laya_store::MoonSupervisor::new(&cfg.moon_bin, cfg.moon_port, cfg.moon_dir())
            .with_auth(moon_password(cfg)?, cfg.moon_acl())
            .with_spawn_timeout(cfg.moon_start_timeout),
    )
}

/// Client configuration for laya-codex's Moon (authenticates every connection).
pub fn store_config(cfg: &Config) -> anyhow::Result<laya_store::StoreConfig> {
    Ok(laya_store::StoreConfig::local(cfg.moon_port).with_password(moon_password(cfg)?))
}

/// Cheap pre-flight (nothing is spawned): laya-codex's Moon answers on its port, or a runnable binary
/// exists.
pub fn moon_available(cfg: &Config) -> anyhow::Result<()> {
    if is_executable(&cfg.moon_bin) || supervisor(cfg)?.is_running() {
        return Ok(());
    }
    anyhow::bail!("{}", moon_missing_message(&cfg.moon_tried))
}

/// Make sure laya-codex's Moon answers on its port, spawning it if needed; errors say what was tried
/// and how to fix it. laya-codex's Moon already answering is used even if no binary is found; a Moon
/// laya-codex cannot authenticate against is refused (never indexed into).
pub fn ensure_moon(cfg: &Config) -> anyhow::Result<()> {
    use laya_store::MoonProbe;
    let sup = supervisor(cfg)?;
    match sup.probe() {
        MoonProbe::Ready => return Ok(()),
        MoonProbe::Down => {}
        MoonProbe::Unprotected if sup.is_legacy() => {} // replaced by `ensure_running`
        p => anyhow::bail!("moon: {}", laya_store::refusal(cfg.moon_port, p)),
    }
    moon_available(cfg)?;
    sup.ensure_running().map(|_| ()).map_err(|e| {
        anyhow::anyhow!(
            "moon: {e} (binary {}; log {}). Set LAYA_CODEX_MOON_BIN to a working moon build",
            cfg.moon_bin.display(),
            sup.logfile().display()
        )
    })
}

/// Repository root for `start`: nearest ancestor containing `.git`, else `start` itself.
pub fn repo_root(start: &Path) -> PathBuf {
    let start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    start
        .ancestors()
        .find(|p| p.join(".git").exists())
        .map(Path::to_path_buf)
        .unwrap_or(start)
}

/// [`repo_root`] of a user-given path (default: the current directory), which must be an
/// existing directory; otherwise commands would silently work on an empty repo.
pub fn existing_repo_root(start: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let start = start.unwrap_or_else(|| PathBuf::from("."));
    match std::fs::metadata(&start) {
        Ok(m) if m.is_dir() => Ok(repo_root(&start)),
        Ok(_) => anyhow::bail!("{} is not a directory", start.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!("{} does not exist", start.display())
        }
        Err(e) => Err(e).with_context(|| format!("cannot access {}", start.display())),
    }
}

/// Repo-relative `/`-separated path for `p` (absolute or relative to `root`); `None` if outside.
pub fn rel_path(root: &Path, p: &str) -> Option<String> {
    let path = Path::new(p);
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let abs = abs.canonicalize().unwrap_or(abs);
    abs.strip_prefix(root)
        .ok()
        .map(|r| r.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moon_start_timeout_defaults_to_30s_and_honours_the_env_value() {
        use std::time::Duration;
        assert_eq!(moon_start_timeout(None), Duration::from_secs(30));
        assert_eq!(moon_start_timeout(Some("90")), Duration::from_secs(90));
        // Nonsense or zero falls back to the default rather than failing every start.
        assert_eq!(moon_start_timeout(Some("soon")), Duration::from_secs(30));
        assert_eq!(moon_start_timeout(Some("0")), Duration::from_secs(30));
    }

    #[test]
    fn repo_root_finds_git_ancestor() {
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = repo_root(here);
        assert!(root.join("Cargo.toml").exists());
        assert!(here.canonicalize().unwrap().starts_with(&root));
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("laya-cfg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn exe(p: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn resolve_moon_prefers_env_then_path_and_has_no_personal_fallback() {
        let d = scratch("moon");
        let (a, b) = (d.join("a"), d.join("b"));
        let path_var = std::env::join_paths([&a, &b]).unwrap();

        // Env wins and is the only candidate, even when missing.
        let env = d.join("custom/moon");
        let (bin, tried) = resolve_moon(Some(env.clone()), None, Some(&path_var));
        assert_eq!((bin, tried), (env.clone(), vec![env]));

        // Nothing exists: every candidate is reported, the last one is returned.
        let (bin, tried) = resolve_moon(None, None, Some(&path_var));
        assert_eq!(tried, vec![a.join("moon"), b.join("moon")]);
        assert_eq!(bin, b.join("moon"));

        // A non-executable file on PATH is skipped; an executable one is chosen.
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("moon"), "").unwrap();
        exe(&b.join("moon"));
        let (bin, tried) = resolve_moon(None, None, Some(&path_var));
        assert_eq!(bin, b.join("moon"));
        assert_eq!(tried, vec![a.join("moon"), b.join("moon")]);

        // No PATH and nothing beside laya-codex: nothing to try.
        assert_eq!(resolve_moon(None, None, None), (PathBuf::new(), vec![]));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn resolve_moon_prefers_the_one_installed_beside_laya() {
        let d = scratch("moon-beside");
        let (bin_dir, on_path) = (d.join("bin"), d.join("path"));
        let path_var = std::env::join_paths([&on_path]).unwrap();
        exe(&on_path.join("moon"));

        // Nothing beside laya-codex: it is tried first, then PATH wins.
        let (bin, tried) = resolve_moon(None, Some(&bin_dir.join("laya-codex")), Some(&path_var));
        assert_eq!(bin, on_path.join("moon"));
        assert_eq!(
            tried,
            vec![
                bin_dir.join("moon"),
                d.join("libexec/moon"),
                on_path.join("moon")
            ]
        );

        // The installer puts the pinned Moon beside laya-codex: it beats any other moon on PATH.
        exe(&bin_dir.join("moon"));
        let (bin, tried) = resolve_moon(None, Some(&bin_dir.join("laya-codex")), Some(&path_var));
        assert_eq!((bin, tried.len()), (bin_dir.join("moon"), 1));

        // LAYA_CODEX_MOON_BIN still overrides everything.
        let env = d.join("custom/moon");
        assert_eq!(
            resolve_moon(
                Some(env.clone()),
                Some(&bin_dir.join("laya-codex")),
                Some(&path_var)
            )
            .0,
            env
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn resolve_moon_follows_a_symlinked_executable_to_its_real_directory() {
        let d = scratch("moon-symlink");
        let (real, linked) = (d.join("real"), d.join("linked"));
        exe(&real.join("laya-codex"));
        exe(&real.join("moon"));
        std::fs::create_dir_all(&linked).unwrap();
        std::os::unix::fs::symlink(real.join("laya-codex"), linked.join("laya-codex")).unwrap();

        // Run through the symlink: the moon beside the real binary is found.
        let (bin, tried) = resolve_moon(None, Some(&linked.join("laya-codex")), None);
        assert_eq!(bin, real.canonicalize().unwrap().join("moon"), "{tried:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn resolve_moon_finds_the_homebrew_libexec_moon() {
        // Homebrew: bin/laya-codex -> Cellar/laya-codex/<v>/bin/laya-codex, moon in
        // Cellar/laya-codex/<v>/libexec (bin/moon belongs to an unrelated formula).
        let d = scratch("moon-brew");
        let keg = d.join("Cellar/laya-codex/0.2.0");
        let (prefix_bin, on_path) = (d.join("bin"), d.join("path"));
        exe(&keg.join("bin/laya-codex"));
        exe(&keg.join("libexec/moon"));
        exe(&on_path.join("moon"));
        std::fs::create_dir_all(&prefix_bin).unwrap();
        std::os::unix::fs::symlink(keg.join("bin/laya-codex"), prefix_bin.join("laya-codex"))
            .unwrap();
        let path_var = std::env::join_paths([&on_path]).unwrap();

        let linked = prefix_bin.join("laya-codex");
        let (bin, tried) = resolve_moon(None, Some(&linked), Some(&path_var));
        let keg = keg.canonicalize().unwrap();
        assert_eq!(bin, keg.join("libexec/moon"));
        assert_eq!(tried, vec![keg.join("bin/moon"), keg.join("libexec/moon")]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_moon_message_names_paths_and_fixes() {
        let m = moon_missing_message(&[PathBuf::from("/x/moon"), PathBuf::from("/y/moon")]);
        assert!(m.contains("/x/moon") && m.contains("/y/moon"), "{m}");
        assert!(
            m.contains("LAYA_CODEX_MOON_BIN") && m.contains("install"),
            "{m}"
        );
    }

    #[test]
    fn rel_path_handles_abs_rel_and_outside() {
        let root = repo_root(Path::new(env!("CARGO_MANIFEST_DIR")));
        let abs = root.join("Cargo.toml");
        assert_eq!(
            rel_path(&root, abs.to_str().unwrap()).as_deref(),
            Some("Cargo.toml")
        );
        assert_eq!(
            rel_path(&root, "crates/laya-cli/Cargo.toml").as_deref(),
            Some("crates/laya-cli/Cargo.toml")
        );
        assert_eq!(rel_path(&root, "/etc/hosts"), None);
    }
}
