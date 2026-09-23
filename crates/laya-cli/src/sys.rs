//! OS glue for the daemon: the single-instance lock, socket peer credentials, process checks.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::path::Path;

/// Exclusive `flock` on `$LAYA_HOME/daemon.lock`, held for the daemon's lifetime and released by
/// the kernel when the process exits (however it exits). The fd is close-on-exec, so the Moon the
/// daemon spawns does not inherit it.
#[derive(Debug)]
pub struct DaemonLock {
    _file: File,
}

impl DaemonLock {
    /// `Ok(None)` when another process holds the lock.
    pub fn try_acquire(path: &Path) -> std::io::Result<Option<Self>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        // SAFETY: `flock(2)` on a descriptor we own; no memory is passed.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(Some(DaemonLock { _file: file }));
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(None)
        } else {
            Err(err)
        }
    }
}

/// Does some process hold the daemon lock? Errors (unreadable lock file) count as held, so
/// callers err on the side of leaving state alone.
pub fn lock_is_held(path: &Path) -> bool {
    !matches!(DaemonLock::try_acquire(path), Ok(Some(_)))
}

pub fn euid() -> u32 {
    // SAFETY: `geteuid(2)` has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Effective uid of the process at the other end of a unix socket.
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
pub fn peer_uid(s: &UnixStream) -> std::io::Result<u32> {
    let (mut uid, mut gid): (libc::uid_t, libc::gid_t) = (0, 0);
    // SAFETY: `getpeereid(2)` writes two integers through valid, exclusive pointers.
    let rc = unsafe { libc::getpeereid(s.as_raw_fd(), &mut uid, &mut gid) };
    if rc == 0 {
        Ok(uid)
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Effective uid of the process at the other end of a unix socket.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn peer_uid(s: &UnixStream) -> std::io::Result<u32> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `getsockopt(SO_PEERCRED)` writes at most `len` bytes into `cred`, which is a valid,
    // exclusively borrowed `ucred` of exactly that size.
    let rc = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    };
    if rc == 0 {
        Ok(cred.uid)
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Is `pid` a live process running a `laya` executable (not this process)?
pub fn is_laya_process(pid: u32) -> bool {
    pid != std::process::id() && laya_store::process_basename(pid).as_deref() == Some("laya")
}

/// Is `pid` alive (or at least not known to be gone)?
pub fn is_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks for existence/permission; no memory is passed.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Send SIGTERM to `pid`; a process that is already gone is not an error.
pub fn terminate(pid: u32) -> std::io::Result<()> {
    let pid = libc::pid_t::try_from(pid).map_err(std::io::Error::other)?;
    // SAFETY: `kill(2)` takes plain integers.
    let rc = unsafe { libc::kill(pid, libc::SIGTERM) };
    let err = std::io::Error::last_os_error();
    if rc == 0 || err.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("laya-sys-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn daemon_lock_is_exclusive_and_released_on_drop() {
        let d = scratch("lock");
        let p = d.join("daemon.lock");
        let held = DaemonLock::try_acquire(&p).unwrap().expect("first");
        assert!(DaemonLock::try_acquire(&p).unwrap().is_none(), "second");
        assert!(lock_is_held(&p));
        drop(held);
        assert!(!lock_is_held(&p));
        assert!(DaemonLock::try_acquire(&p).unwrap().is_some());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn daemon_lock_refuses_a_symlink() {
        let d = scratch("lock-link");
        let p = d.join("daemon.lock");
        std::os::unix::fs::symlink(d.join("target"), &p).unwrap();
        assert!(DaemonLock::try_acquire(&p).is_err());
        assert!(!d.join("target").exists());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn peer_uid_is_ours_on_a_socketpair() {
        let (a, _b) = UnixStream::pair().unwrap();
        assert_eq!(peer_uid(&a).unwrap(), euid());
    }

    #[test]
    fn this_test_process_is_alive_but_not_laya() {
        let me = std::process::id();
        assert!(is_alive(me));
        assert!(!is_laya_process(me));
        assert!(!is_alive(u32::MAX / 2));
    }
}
