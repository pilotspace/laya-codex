//! Unix-socket client for the daemon with hard timeouts. When the daemon is not running the
//! client starts it in the background and fails this call (hooks fail open; the next call wins).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, bail};

use crate::hook::DaemonApi;
use crate::protocol::{Request, Response};

pub struct Client {
    socket: PathBuf,
    timeout: Duration,
    autostart: bool,
}

impl Client {
    pub fn new(socket: &Path, timeout: Duration, autostart: bool) -> Self {
        Client {
            socket: socket.to_path_buf(),
            timeout,
            autostart,
        }
    }

    fn connect(&self) -> anyhow::Result<UnixStream> {
        match UnixStream::connect(&self.socket) {
            Ok(s) => Ok(s),
            Err(e) => {
                if !self.autostart {
                    return Err(e).context("daemon not reachable");
                }
                let marker = crate::config::Config::from_env().home.join("daemon.spawn");
                if !claim_spawn(&marker, SPAWN_BACKOFF) {
                    return Err(e).context("daemon not reachable (a start is already in progress)");
                }
                spawn_daemon()?;
                Err(e).context("daemon not reachable (starting it in the background)")
            }
        }
    }
}

impl DaemonApi for Client {
    fn call(&self, req: Request) -> anyhow::Result<Response> {
        let mut stream = self.connect()?;
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_write_timeout(Some(Duration::from_millis(200)))?;
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        stream.write_all(line.as_bytes())?;
        let mut reader = BufReader::new(stream);
        let mut resp = String::new();
        reader
            .read_line(&mut resp)
            .context("daemon read (timeout?)")?;
        if resp.is_empty() {
            bail!("daemon closed the connection");
        }
        match serde_json::from_str(&resp)? {
            Response::Error { message } => bail!("daemon error: {message}"),
            r => Ok(r),
        }
    }
}

/// Minimum time between daemon start attempts across all laya processes. Without it every
/// hook or retry during a slow (or failing) start spawns another daemon.
const SPAWN_BACKOFF: Duration = Duration::from_secs(10);

/// Claim the right to start the daemon: false if another process started one within `backoff`
/// (per the `marker` file's mtime), else touches the marker and returns true. Best effort: two
/// racing callers may both win, which is harmless because the daemon binds its socket exclusively.
fn claim_spawn(marker: &Path, backoff: Duration) -> bool {
    let recent = std::fs::metadata(marker)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < backoff);
    if recent {
        return false;
    }
    if let Some(dir) = marker.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(marker, std::process::id().to_string());
    true
}

/// Start `laya daemon` detached from the calling process (stdio to /dev/null; it logs itself).
pub fn spawn_daemon() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let log = crate::config::Config::from_env().daemon_log();
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let err = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)?;
    Command::new(exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(err))
        .spawn()
        .context("spawn laya daemon")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawns_are_rate_limited_across_processes() {
        let dir = std::env::temp_dir().join(format!("laya-spawn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("daemon.spawn");
        let _ = std::fs::remove_file(&marker);
        let backoff = Duration::from_secs(10);
        assert!(claim_spawn(&marker, backoff), "first caller spawns");
        assert!(
            !claim_spawn(&marker, backoff),
            "a caller within the backoff does not"
        );
        assert!(
            claim_spawn(&marker, Duration::ZERO),
            "after the backoff a caller spawns again"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
