//! Runtime configuration: environment variables over built-in defaults.
//!
//! | env | default |
//! |---|---|
//! | `LAYA_HOME` | `~/.cache/laya-codex` |
//! | `LAYA_MODEL_DIR` | `$LAYA_HOME/models/laya-code`, else `$LAYA_HOME/models/laya-base` |
//! | `LAYA_MOON_BIN` | `moon` on PATH, else `~/workspaces/tind-repo/moon/target/release/moon` |
//! | `LAYA_MOON_PORT` | `16379` |
//! | `LAYA_NO_MODEL` | unset (set to `1` for lexical-only ranking) |
//! | `LAYA_BUDGET_MS` | `1200` (Laya time budget per query) |
//! | `LAYA_HOOK_LOG` | unset (JSONL log of hook actions, used by the benchmark) |

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Config {
    pub home: PathBuf,
    pub model_dir: Option<PathBuf>,
    pub moon_bin: PathBuf,
    pub moon_port: u16,
    pub use_model: bool,
    pub budget_ms: u64,
    pub hook_log: Option<PathBuf>,
}

impl Config {
    pub fn from_env() -> Self {
        let home = std::env::var_os("LAYA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".cache/laya-codex"));
        let model_dir = std::env::var_os("LAYA_MODEL_DIR").map(PathBuf::from).or_else(|| {
            ["laya-code", "laya-base"]
                .iter()
                .map(|m| home.join("models").join(m))
                .find(|p| p.join("model.safetensors").exists())
        });
        let moon_bin = std::env::var_os("LAYA_MOON_BIN").map(PathBuf::from).unwrap_or_else(|| {
            which("moon").unwrap_or_else(|| home_dir().join("workspaces/tind-repo/moon/target/release/moon"))
        });
        Config {
            moon_port: env_parse("LAYA_MOON_PORT").unwrap_or(16379),
            use_model: std::env::var("LAYA_NO_MODEL").map(|v| v != "1").unwrap_or(true),
            budget_ms: env_parse("LAYA_BUDGET_MS").unwrap_or(1200),
            hook_log: std::env::var_os("LAYA_HOOK_LOG").map(PathBuf::from),
            home,
            model_dir,
            moon_bin,
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
}

fn env_parse<T: std::str::FromStr>(k: &str) -> Option<T> {
    std::env::var(k).ok()?.parse().ok()
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn which(bin: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|d| Path::new(d).join(bin))
        .find(|p| p.is_file())
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

/// Stable store namespace for a repo root: first 12 hex chars of blake3(canonical path).
pub fn repo_id(root: &Path) -> String {
    blake3::hash(root.to_string_lossy().as_bytes()).to_hex()[..12].to_string()
}

/// Repo-relative `/`-separated path for `p` (absolute or relative to `root`); `None` if outside.
pub fn rel_path(root: &Path, p: &str) -> Option<String> {
    let path = Path::new(p);
    let abs = if path.is_absolute() { path.to_path_buf() } else { root.join(path) };
    let abs = abs.canonicalize().unwrap_or(abs);
    abs.strip_prefix(root).ok().map(|r| r.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_root_finds_git_ancestor() {
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = repo_root(here);
        assert!(root.join("Cargo.toml").exists());
        assert!(here.canonicalize().unwrap().starts_with(&root));
    }

    #[test]
    fn repo_id_is_stable_and_short() {
        let a = repo_id(Path::new("/x/y"));
        assert_eq!(a.len(), 12);
        assert_eq!(a, repo_id(Path::new("/x/y")));
        assert_ne!(a, repo_id(Path::new("/x/z")));
    }

    #[test]
    fn rel_path_handles_abs_rel_and_outside() {
        let root = repo_root(Path::new(env!("CARGO_MANIFEST_DIR")));
        let abs = root.join("Cargo.toml");
        assert_eq!(rel_path(&root, abs.to_str().unwrap()).as_deref(), Some("Cargo.toml"));
        assert_eq!(rel_path(&root, "crates/laya-cli/Cargo.toml").as_deref(), Some("crates/laya-cli/Cargo.toml"));
        assert_eq!(rel_path(&root, "/etc/hosts"), None);
    }
}
