//! [`MoonSupervisor`]: keep a local moon sidecar alive.
//!
//! `ensure_running` is both "start" and "restart after crash": it PINGs, and if nothing answers
//! it spawns a detached moon (own process group, so a Ctrl-C to the CLI does not kill it) with
//! `--appendonly yes` so data and the FT index survive restarts, then waits for PING.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use laya_core::{Error, Result};

const PING_TIMEOUT: Duration = Duration::from_millis(200);
const POLL: Duration = Duration::from_millis(20);
const STOP_GRACE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorStatus {
    /// Something already answered PING on the port.
    AlreadyRunning,
    /// A new moon was spawned and answered PING.
    Spawned { pid: u32 },
}

#[derive(Debug)]
pub struct MoonSupervisor {
    bin: PathBuf,
    port: u16,
    dir: PathBuf,
    spawn_timeout: Duration,
    /// The moon we spawned, kept so it can be reaped; `None` if another process spawned it.
    child: Mutex<Option<Child>>,
}

impl MoonSupervisor {
    /// `dir` holds moon's data (AOF), `moon.log` and `moon.pid`.
    pub fn new(bin: impl Into<PathBuf>, port: u16, dir: impl AsRef<Path>) -> Self {
        Self {
            bin: bin.into(),
            port,
            dir: dir.as_ref().to_path_buf(),
            spawn_timeout: Duration::from_secs(3),
            child: Mutex::new(None),
        }
    }

    /// Override the default 3 s wait for a freshly spawned moon to answer PING.
    #[must_use]
    pub fn with_spawn_timeout(mut self, d: Duration) -> Self {
        self.spawn_timeout = d;
        self
    }

    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    #[must_use]
    pub fn pidfile(&self) -> PathBuf {
        self.dir.join("moon.pid")
    }

    #[must_use]
    pub fn logfile(&self) -> PathBuf {
        self.dir.join("moon.log")
    }

    /// Does a RESP server answer `PING` on `127.0.0.1:port` within 200 ms?
    #[must_use]
    pub fn is_running(&self) -> bool {
        ping(self.port)
    }

    fn child(&self) -> std::sync::MutexGuard<'_, Option<Child>> {
        self.child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Make sure moon answers on the port, spawning it if needed.
    pub fn ensure_running(&self) -> Result<SupervisorStatus> {
        if self.is_running() {
            return Ok(SupervisorStatus::AlreadyRunning);
        }
        std::fs::create_dir_all(&self.dir)?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.logfile())?;
        let mut cmd = Command::new(&self.bin);
        cmd.arg("--bind")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(self.port.to_string())
            .arg("--dir")
            .arg(&self.dir)
            .args(["--shards", "1", "--appendonly", "yes"])
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        let mut child = cmd.spawn().map_err(|e| {
            Error::StoreUnavailable(format!("cannot spawn {}: {e}", self.bin.display()))
        })?;
        let pid = child.id();

        let t = Instant::now();
        loop {
            if self.is_running() {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    // Our moon died but something answers: another process won the spawn race.
                    return Ok(SupervisorStatus::AlreadyRunning);
                }
                write_pidfile(&self.pidfile(), pid)?;
                *self.child() = Some(child);
                tracing::info!(pid, port = self.port, "moon spawned");
                return Ok(SupervisorStatus::Spawned { pid });
            }
            if let Ok(Some(status)) = child.try_wait() {
                return Err(Error::StoreUnavailable(format!(
                    "moon exited during startup ({status}); log tail: {}",
                    log_tail(&self.logfile())
                )));
            }
            if t.elapsed() >= self.spawn_timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::StoreUnavailable(format!(
                    "moon did not answer PING on port {} within {:?}; log tail: {}",
                    self.port,
                    self.spawn_timeout,
                    log_tail(&self.logfile())
                )));
            }
            std::thread::sleep(POLL);
        }
    }

    /// Stop the moon on our port: SIGTERM, wait up to 3 s for it to stop answering, then SIGKILL.
    ///
    /// The pid comes from the child we spawned or from the pidfile. A pid is only signalled while
    /// a server still answers on our port, which guards against signalling a recycled pid after
    /// moon already exited.
    pub fn stop(&self) -> Result<()> {
        let pidfile = self.pidfile();
        let pid = self.child().as_ref().map(Child::id).or_else(|| {
            std::fs::read_to_string(&pidfile)
                .ok()
                .and_then(|s| s.trim().parse().ok())
        });
        if self.is_running() {
            let Some(pid) = pid else {
                return Err(Error::StoreUnavailable(format!(
                    "a server answers on port {} but no pid is known (no {})",
                    self.port,
                    pidfile.display()
                )));
            };
            signal(pid, libc::SIGTERM)?;
            let t = Instant::now();
            while self.is_running() && t.elapsed() < STOP_GRACE {
                std::thread::sleep(POLL);
            }
            if self.is_running() {
                tracing::warn!(pid, "moon ignored SIGTERM; sending SIGKILL");
                signal(pid, libc::SIGKILL)?;
            }
        }
        if let Some(mut c) = self.child().take() {
            let _ = c.wait(); // reap; returns immediately once it exited
        }
        match std::fs::remove_file(&pidfile) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

fn signal(pid: u32, sig: libc::c_int) -> Result<()> {
    let pid = libc::pid_t::try_from(pid)
        .map_err(|_| Error::StoreUnavailable(format!("invalid pid {pid}")))?;
    // SAFETY: `kill(2)` has no memory-safety preconditions; it only takes integers.
    let rc = unsafe { libc::kill(pid, sig) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        Ok(()) // already gone
    } else {
        Err(err.into())
    }
}

fn write_pidfile(path: &Path, pid: u32) -> Result<()> {
    let tmp = path.with_extension("pid.tmp");
    std::fs::write(&tmp, format!("{pid}\n"))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn log_tail(path: &Path) -> String {
    let s = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = s.lines().rev().take(5).collect();
    lines.into_iter().rev().collect::<Vec<_>>().join(" | ")
}

/// Raw RESP PING with a 200 ms budget; no client state needed.
pub(crate) fn ping(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut s) = TcpStream::connect_timeout(&addr, PING_TIMEOUT) else {
        return false;
    };
    if s.set_read_timeout(Some(PING_TIMEOUT)).is_err()
        || s.set_write_timeout(Some(PING_TIMEOUT)).is_err()
        || s.write_all(b"*1\r\n$4\r\nPING\r\n").is_err()
    {
        return false;
    }
    let mut buf = [0u8; 7];
    s.read_exact(&mut buf).is_ok() && &buf == b"+PONG\r\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_is_false_when_nothing_listens() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .expect("port");
        assert!(!ping(port));
    }

    #[test]
    fn pidfile_is_written_atomically() {
        let d = std::env::temp_dir().join(format!("laya-store-pid-{}", std::process::id()));
        std::fs::create_dir_all(&d).expect("dir");
        let p = d.join("moon.pid");
        write_pidfile(&p, 42).expect("write");
        assert_eq!(std::fs::read_to_string(&p).expect("read"), "42\n");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn log_tail_keeps_last_lines() {
        let d = std::env::temp_dir().join(format!("laya-store-log-{}", std::process::id()));
        std::fs::create_dir_all(&d).expect("dir");
        let p = d.join("moon.log");
        std::fs::write(&p, "1\n2\n3\n4\n5\n6\n7\n").expect("write");
        assert_eq!(log_tail(&p), "3 | 4 | 5 | 6 | 7");
        let _ = std::fs::remove_dir_all(&d);
    }
}
