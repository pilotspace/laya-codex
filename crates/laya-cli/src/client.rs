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
        let _ = laya_store::create_private_dir(dir);
    }
    let _ = std::fs::write(marker, std::process::id().to_string());
    true
}

/// Start `laya daemon` detached from the calling process (stdio to /dev/null; it logs itself).
pub fn spawn_daemon() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let log = crate::config::Config::from_env().daemon_log();
    if let Some(dir) = log.parent() {
        laya_store::create_private_dir(dir)?;
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

/// How `laya stop` stopped the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// The daemon acknowledged `Request::Shutdown` and exited.
    ViaSocket,
    /// SIGTERM to the pidfile's pid, verified to be a `laya` process (a daemon whose socket is
    /// dead, or an older daemon that does not know `Shutdown`).
    ViaSignal(u32),
    NotRunning,
}

/// How long `laya stop` waits for the daemon to go away.
const STOP_WAIT: Duration = Duration::from_secs(5);

/// Stop the daemon: ask it over the socket; only when that fails, SIGTERM the pidfile's pid, and
/// only if that pid is verified to be a live `laya` process (never a recycled pid). Stale
/// pidfile/socket are removed only when no daemon holds the lock.
pub fn stop_daemon(socket: &Path, pidfile: &Path, lock: &Path) -> anyhow::Result<Stopped> {
    let asked = Client::new(socket, Duration::from_secs(3), false).call(Request::Shutdown);
    if asked.is_ok() {
        wait_until(STOP_WAIT, || UnixStream::connect(socket).is_err());
        return Ok(Stopped::ViaSocket);
    }
    let pid: Option<u32> = std::fs::read_to_string(pidfile)
        .ok()
        .and_then(|s| s.trim().parse().ok());
    if let Some(pid) = pid.filter(|&p| crate::sys::is_laya_process(p)) {
        crate::sys::terminate(pid).with_context(|| format!("SIGTERM daemon pid {pid}"))?;
        if !wait_until(STOP_WAIT, || !crate::sys::is_alive(pid)) {
            bail!("daemon pid {pid} did not exit within {STOP_WAIT:?}");
        }
        remove_stale(socket, pidfile, lock);
        return Ok(Stopped::ViaSignal(pid));
    }
    remove_stale(socket, pidfile, lock);
    Ok(Stopped::NotRunning)
}

/// Remove a leftover pidfile and dead socket, unless a daemon holds the lock (it owns them).
fn remove_stale(socket: &Path, pidfile: &Path, lock: &Path) {
    if crate::sys::lock_is_held(lock) {
        return;
    }
    let _ = std::fs::remove_file(pidfile);
    if UnixStream::connect(socket).is_err() {
        let _ = std::fs::remove_file(socket);
    }
}

/// Poll `done` every 50 ms for up to `timeout`.
fn wait_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let t0 = std::time::Instant::now();
    while !done() {
        if t0.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
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

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("laya-stop-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn stop_asks_the_daemon_over_the_socket() {
        let d = scratch("socket");
        let sock = d.join("laya.sock");
        let l = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let s2 = sock.clone();
        let t = std::thread::spawn(move || {
            let (c, _) = l.accept().unwrap();
            let mut line = String::new();
            BufReader::new(c.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            assert_eq!(line.trim(), r#"{"op":"shutdown"}"#);
            (&c).write_all(b"{\"status\":\"ok\"}\n").unwrap();
            drop(l);
            std::fs::remove_file(&s2).unwrap();
        });
        let r = stop_daemon(&sock, &d.join("daemon.pid"), &d.join("daemon.lock")).unwrap();
        t.join().unwrap();
        assert_eq!(r, Stopped::ViaSocket);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn stop_never_signals_a_pid_that_is_not_a_laya_daemon() {
        let d = scratch("stale");
        let pidfile = d.join("daemon.pid");
        // A recycled pid: this test process is alive but is not `laya`.
        std::fs::write(&pidfile, std::process::id().to_string()).unwrap();
        let r = stop_daemon(&d.join("laya.sock"), &pidfile, &d.join("daemon.lock")).unwrap();
        assert_eq!(r, Stopped::NotRunning);
        assert!(
            !pidfile.exists(),
            "a stale pidfile is removed when the lock is free"
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn stop_keeps_the_pidfile_while_a_daemon_holds_the_lock() {
        let d = scratch("locked");
        let (pidfile, lock) = (d.join("daemon.pid"), d.join("daemon.lock"));
        std::fs::write(&pidfile, std::process::id().to_string()).unwrap();
        let _held = crate::sys::DaemonLock::try_acquire(&lock)
            .unwrap()
            .expect("lock");
        let r = stop_daemon(&d.join("laya.sock"), &pidfile, &lock).unwrap();
        assert_eq!(r, Stopped::NotRunning);
        assert!(pidfile.exists());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
