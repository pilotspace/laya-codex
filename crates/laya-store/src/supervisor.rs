//! [`MoonSupervisor`]: keep a local moon sidecar alive.
//!
//! `ensure_running` is both "start" and "restart after crash": it probes the port, and if nothing
//! answers it spawns a detached moon (own process group, so a Ctrl-C to the CLI does not kill it)
//! with `--appendonly yes` so data and the FT index survive restarts, then waits until it answers.
//!
//! With [`MoonSupervisor::with_auth`] Moon is spawned with laya's ACL file and password, and only
//! a Moon that rejects anonymous clients *and* accepts laya's password counts as running
//! ([`MoonProbe::Ready`]). A Moon without a password on the port is refused, except one an older
//! laya started from the same data dir (its pidfile names a live `moon`), which is replaced.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use laya_core::{Error, Result};

use crate::secure::{Password, create_private_dir};

const PING_TIMEOUT: Duration = Duration::from_millis(200);
const POLL: Duration = Duration::from_millis(20);
const STOP_GRACE: Duration = Duration::from_secs(3);
/// Longest reply line the probe reads.
const MAX_REPLY: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorStatus {
    /// laya's Moon already answered on the port.
    AlreadyRunning,
    /// A new moon was spawned and answered.
    Spawned { pid: u32 },
}

/// What answers on the Moon port, from laya's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoonProbe {
    /// Nothing answers RESP there (closed port, or a non-RESP listener).
    Down,
    /// Ours: without auth, it answers PING; with auth, it rejects an anonymous PING and answers
    /// PING after `AUTH <password>`.
    Ready,
    /// It answers an anonymous PING although laya expects a password: not laya's Moon.
    Unprotected,
    /// It requires a password but rejects laya's.
    WrongPassword,
}

#[derive(Debug, Clone)]
struct Auth {
    password: Password,
    aclfile: PathBuf,
}

#[derive(Debug)]
pub struct MoonSupervisor {
    bin: PathBuf,
    port: u16,
    dir: PathBuf,
    auth: Option<Auth>,
    spawn_timeout: Duration,
    /// The moon we spawned, kept so it can be reaped; `None` if another process spawned it.
    child: Mutex<Option<Child>>,
}

impl MoonSupervisor {
    /// `dir` holds moon's data (AOF), `moon.log` and `moon.pid`; it is created mode 0700.
    pub fn new(bin: impl Into<PathBuf>, port: u16, dir: impl AsRef<Path>) -> Self {
        Self {
            bin: bin.into(),
            port,
            dir: dir.as_ref().to_path_buf(),
            auth: None,
            spawn_timeout: Duration::from_secs(3),
            child: Mutex::new(None),
        }
    }

    /// Protect the Moon with `password`: it is spawned with `--aclfile aclfile` (which must hold
    /// the same password, see [`crate::load_or_create_acl`]) and `--requirepass`, and a Moon on
    /// the port only counts as running when it demands and accepts this password.
    ///
    /// `--requirepass` is required because Moon (8bba3ced) treats every connection as
    /// authenticated while it is unset, even with an ACL file. It makes the password visible to
    /// local users via `ps`.
    #[must_use]
    pub fn with_auth(mut self, password: Password, aclfile: impl Into<PathBuf>) -> Self {
        self.auth = Some(Auth {
            password,
            aclfile: aclfile.into(),
        });
        self
    }

    /// Override the default 3 s wait for a freshly spawned moon to answer.
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

    /// Is laya's Moon ([`MoonProbe::Ready`]) answering on `127.0.0.1:port`?
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.probe() == MoonProbe::Ready
    }

    /// Classify what answers on the port (200 ms per step).
    #[must_use]
    pub fn probe(&self) -> MoonProbe {
        probe(self.port, self.auth.as_ref().map(|a| &a.password))
    }

    /// Would [`Self::ensure_running`] replace the Moon on the port (a password-less Moon an older
    /// laya started from this data dir)?
    #[must_use]
    pub fn is_legacy(&self) -> bool {
        self.auth.is_some() && self.probe() == MoonProbe::Unprotected && self.legacy_pid().is_some()
    }

    fn child(&self) -> std::sync::MutexGuard<'_, Option<Child>> {
        self.child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Error for a Moon on the port that laya must not use.
    fn refuse(&self, p: MoonProbe) -> Error {
        Error::StoreUnavailable(refusal(self.port, p))
    }

    /// Pid of a password-less Moon an older laya started from our data dir: the pidfile names a
    /// live process whose executable is called `moon`.
    fn legacy_pid(&self) -> Option<u32> {
        let pid: u32 = std::fs::read_to_string(self.pidfile())
            .ok()?
            .trim()
            .parse()
            .ok()?;
        (process_basename(pid)? == "moon").then_some(pid)
    }

    /// Stop a legacy Moon (see [`Self::legacy_pid`]) so an authenticated one can take the port.
    fn replace_legacy(&self, pid: u32) -> Result<()> {
        tracing::warn!(
            pid,
            port = self.port,
            "stopping the password-less moon started by an older laya"
        );
        self.terminate(pid)?;
        if self.probe() != MoonProbe::Down {
            return Err(Error::StoreUnavailable(format!(
                "the password-less moon (pid {pid}) on port {} did not stop",
                self.port
            )));
        }
        let _ = std::fs::remove_file(self.pidfile());
        Ok(())
    }

    /// SIGTERM `pid`, wait up to 3 s for the port to go quiet, then SIGKILL (and wait again).
    fn terminate(&self, pid: u32) -> Result<()> {
        let quiet = |grace: Duration| {
            let t = Instant::now();
            while self.probe() != MoonProbe::Down && t.elapsed() < grace {
                std::thread::sleep(POLL);
            }
            self.probe() == MoonProbe::Down
        };
        signal(pid, libc::SIGTERM)?;
        if !quiet(STOP_GRACE) {
            tracing::warn!(pid, "moon ignored SIGTERM; sending SIGKILL");
            signal(pid, libc::SIGKILL)?;
            quiet(STOP_GRACE);
        }
        Ok(())
    }

    /// Make sure laya's Moon answers on the port, spawning it if needed.
    ///
    /// Errors without spawning when the port is served by a Moon laya cannot authenticate
    /// against, unless it is a password-less Moon an older laya started from the same data dir:
    /// that one is stopped and replaced, keeping its data dir (hence the index).
    pub fn ensure_running(&self) -> Result<SupervisorStatus> {
        match self.probe() {
            MoonProbe::Ready => return Ok(SupervisorStatus::AlreadyRunning),
            MoonProbe::Down => {}
            p @ MoonProbe::Unprotected => match self.legacy_pid() {
                Some(pid) if self.auth.is_some() => self.replace_legacy(pid)?,
                _ => return Err(self.refuse(p)),
            },
            p @ MoonProbe::WrongPassword => return Err(self.refuse(p)),
        }
        create_private_dir(&self.dir)?;
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
            .args(["--shards", "1", "--appendonly", "yes"]);
        if let Some(a) = &self.auth {
            cmd.arg("--aclfile")
                .arg(&a.aclfile)
                .arg("--requirepass")
                .arg(a.password.expose());
        }
        cmd.stdin(Stdio::null())
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
            let p = self.probe();
            if p != MoonProbe::Down && matches!(child.try_wait(), Ok(Some(_))) {
                // Our moon died but something answers: another process won the spawn race.
                return match p {
                    MoonProbe::Ready => Ok(SupervisorStatus::AlreadyRunning),
                    p => Err(self.refuse(p)),
                };
            }
            if p == MoonProbe::Ready {
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
                    "moon did not answer on port {} within {:?}; log tail: {}",
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
        if self.probe() != MoonProbe::Down {
            let Some(pid) = pid else {
                return Err(Error::StoreUnavailable(format!(
                    "a server answers on port {} but no pid is known (no {})",
                    self.port,
                    pidfile.display()
                )));
            };
            self.terminate(pid)?;
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

/// Why laya refuses the Moon on `port` (for `Unprotected` / `WrongPassword`), with the fix.
#[must_use]
pub fn refusal(port: u16, p: MoonProbe) -> String {
    match p {
        MoonProbe::Unprotected => format!(
            "port {port} is served by a Moon without laya's password; stop it or set LAYA_MOON_PORT"
        ),
        _ => format!(
            "port {port} is served by a Moon that rejects laya's password (another LAYA_HOME?); stop it or set LAYA_MOON_PORT"
        ),
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

/// Executable file name of a live process (`ps -o comm=`), `None` if it is gone or unknown.
#[must_use]
pub fn process_basename(pid: u32) -> Option<String> {
    let out = Command::new("ps")
        .args(["-o", "comm=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let comm = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Path::new(&comm)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
}

/// Send one RESP command; the first line of the reply, `None` on any transport failure.
fn roundtrip(s: &mut TcpStream, args: &[&str]) -> Option<String> {
    let mut req = format!("*{}\r\n", args.len());
    for a in args {
        req.push_str(&format!("${}\r\n{a}\r\n", a.len()));
    }
    s.write_all(req.as_bytes()).ok()?;
    let mut line = Vec::with_capacity(64);
    let mut b = [0u8; 1];
    while !line.ends_with(b"\r\n") {
        if line.len() >= MAX_REPLY || s.read(&mut b).ok()? == 0 {
            return None;
        }
        line.push(b[0]);
    }
    line.truncate(line.len() - 2);
    Some(String::from_utf8_lossy(&line).into_owned())
}

/// Classify the server on `127.0.0.1:port`, 200 ms per step; no client state needed.
pub(crate) fn probe(port: u16, password: Option<&Password>) -> MoonProbe {
    probe_addr(SocketAddr::from(([127, 0, 0, 1], port)), password)
}

/// [`probe`] for any address.
pub(crate) fn probe_addr(addr: SocketAddr, password: Option<&Password>) -> MoonProbe {
    let Ok(mut s) = TcpStream::connect_timeout(&addr, PING_TIMEOUT) else {
        return MoonProbe::Down;
    };
    if s.set_read_timeout(Some(PING_TIMEOUT)).is_err()
        || s.set_write_timeout(Some(PING_TIMEOUT)).is_err()
    {
        return MoonProbe::Down;
    }
    let Some(reply) = roundtrip(&mut s, &["PING"]) else {
        return MoonProbe::Down;
    };
    classify(&reply, password, || {
        password.is_some_and(|p| {
            roundtrip(&mut s, &["AUTH", p.expose()]).as_deref() == Some("+OK")
                && roundtrip(&mut s, &["PING"]).as_deref() == Some("+PONG")
        })
    })
}

/// Classify an anonymous PING `reply`; `authenticates` runs `AUTH` + `PING` when needed.
fn classify(
    reply: &str,
    password: Option<&Password>,
    authenticates: impl FnOnce() -> bool,
) -> MoonProbe {
    match (reply, password) {
        ("+PONG", None) => MoonProbe::Ready,
        ("+PONG", Some(_)) => MoonProbe::Unprotected,
        (r, _) if !r.starts_with('-') => MoonProbe::Down, // not a RESP server we understand
        (_, None) => MoonProbe::WrongPassword,
        (_, Some(_)) if authenticates() => MoonProbe::Ready,
        (_, Some(_)) => MoonProbe::WrongPassword,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_is_down_when_nothing_listens() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .expect("port");
        assert_eq!(probe(port, None), MoonProbe::Down);
        assert_eq!(probe(port, Some(&Password::new("x"))), MoonProbe::Down);
    }

    #[test]
    fn anonymous_pong_is_unprotected_when_laya_expects_a_password() {
        let pw = Password::new("pw");
        let never = || panic!("must not authenticate");
        assert_eq!(classify("+PONG", None, never), MoonProbe::Ready);
        assert_eq!(classify("+PONG", Some(&pw), never), MoonProbe::Unprotected);
        assert_eq!(classify("HTTP/1.1 400", Some(&pw), never), MoonProbe::Down);
        assert_eq!(
            classify("-NOAUTH Authentication required.", None, never),
            MoonProbe::WrongPassword
        );
        let noauth = "-NOAUTH Authentication required.";
        assert_eq!(classify(noauth, Some(&pw), || true), MoonProbe::Ready);
        assert_eq!(
            classify(noauth, Some(&pw), || false),
            MoonProbe::WrongPassword
        );
    }

    #[test]
    fn refusal_names_the_port_and_the_fix() {
        let m = refusal(16379, MoonProbe::Unprotected);
        assert!(m.contains("port 16379") && m.contains("without laya's password"));
        assert!(m.contains("LAYA_MOON_PORT"));
        assert!(refusal(1, MoonProbe::WrongPassword).contains("rejects laya's password"));
    }

    #[test]
    fn process_basename_names_this_test_binary() {
        let me = process_basename(std::process::id()).expect("self");
        assert!(!me.is_empty() && !me.contains('/'), "{me}");
        assert_eq!(process_basename(u32::MAX / 2), None);
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
