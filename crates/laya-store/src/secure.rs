//! Moon credentials and private filesystem state.
//!
//! laya-codex protects its Moon sidecar with a random password kept in a Redis-format ACL file
//! (`user default on >PASSWORD ~* &* +@all`, mode 0600). The same file is the password store for
//! the client and is handed to Moon with `--aclfile`.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use laya_core::{Error, Result};

/// Random bytes in a generated password (hex-encoded: 64 characters).
const PASSWORD_BYTES: usize = 32;
/// An ACL file larger than this is not ours.
const MAX_ACL_BYTES: u64 = 64 * 1024;

/// Moon password. `Debug`/`Display` never print it.
#[derive(Clone, PartialEq, Eq)]
pub struct Password(String);

impl Password {
    /// Wrap an existing secret (tests, or a caller with its own credential store).
    #[must_use]
    pub fn new(secret: impl Into<String>) -> Self {
        Password(secret.into())
    }

    /// The secret itself; only for sending `AUTH` or writing the ACL file.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// A fresh `PASSWORD_BYTES`-byte random password from `/dev/urandom`, hex-encoded.
    pub fn generate() -> Result<Self> {
        let mut buf = [0u8; PASSWORD_BYTES];
        File::open("/dev/urandom")?.read_exact(&mut buf)?;
        Ok(Password(buf.iter().map(|b| format!("{b:02x}")).collect()))
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(<redacted>)")
    }
}

/// Create `dir` (and missing parents, also 0700) and make `dir` itself mode 0700.
///
/// Refuses a directory owned by another user: laya-codex keeps its socket, password and data there.
/// A symlinked `dir` is followed (a relocated cache dir); its target is checked the same way.
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let meta = std::fs::metadata(dir)?;
    if !meta.is_dir() {
        return Err(std::io::Error::other(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    if meta.uid() != euid() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} is owned by another user (uid {})",
                dir.display(),
                meta.uid()
            ),
        ));
    }
    if meta.mode() & 0o777 != 0o700 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn euid() -> u32 {
    // SAFETY: `geteuid(2)` has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// The ACL line granting `default` everything with `password`.
fn acl_line(password: &Password) -> String {
    format!("user default on >{} ~* &* +@all\n", password.expose())
}

/// The `default` user's plaintext password in an ACL file's text, if it has one.
fn parse_acl(text: &str) -> Option<Password> {
    text.lines()
        .map(str::split_whitespace)
        .find_map(|mut t| {
            (t.next() == Some("user") && t.next() == Some("default"))
                .then(|| t.find_map(|tok| tok.strip_prefix('>')))
                .flatten()
        })
        .filter(|p| !p.is_empty())
        .map(Password::new)
}

/// Read the password from the ACL file at `path`, creating the file with a fresh random password
/// first when it does not exist.
///
/// Creation is atomic and never clobbers: the file is written in full to a private temp file
/// (`create_new`, mode 0600, no symlink following) and then hard-linked into place, so a racing
/// process either wins (and everyone reads its password) or loses and reads the winner's.
/// Reading refuses symlinks and files owned by another user, and tightens loose permissions.
pub fn load_or_create_acl(path: &Path) -> Result<Password> {
    if let Some(p) = read_acl(path)? {
        return Ok(p);
    }
    let dir = path
        .parent()
        .ok_or_else(|| Error::Store(format!("{} has no parent directory", path.display())))?;
    create_private_dir(dir)?;
    let password = Password::generate()?;
    let tmp = dir.join(format!(
        ".{}.{}.{:016x}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("acl"),
        std::process::id(),
        fastrand::u64(..)
    ));
    let written = (|| -> std::io::Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)?;
        f.write_all(acl_line(&password).as_bytes())?;
        f.sync_all()
    })();
    let linked = written.and_then(|()| std::fs::hard_link(&tmp, path));
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(password),
        // Lost a creation race: use the winner's password.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => read_acl(path)?
            .ok_or_else(|| Error::Store(format!("{} has no password", path.display()))),
        Err(e) => Err(e.into()),
    }
}

/// `Ok(None)` when the file does not exist.
fn read_acl(path: &Path) -> Result<Option<Password>> {
    let mut f = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(Error::Store(format!(
                "cannot open {} (symlinks are refused): {e}",
                path.display()
            )));
        }
    };
    let meta = f.metadata()?;
    if !meta.is_file() || meta.uid() != euid() || meta.len() > MAX_ACL_BYTES {
        return Err(Error::Store(format!(
            "{} is not a regular file owned by this user; delete it so laya-codex can recreate it",
            path.display()
        )));
    }
    if meta.mode() & 0o077 != 0 {
        tracing::warn!(path = %path.display(), "moon ACL file was group/world accessible; tightening to 0600");
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    let mut text = String::new();
    f.read_to_string(&mut text)?;
    parse_acl(&text).map(Some).ok_or_else(|| {
        Error::Store(format!(
            "{} has no password for the default user; delete it (and stop the Moon using it) so laya-codex can recreate it",
            path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("laya-secure-{tag}-"))
            .tempdir()
            .expect("tempdir")
    }

    fn mode(p: &Path) -> u32 {
        std::fs::symlink_metadata(p).expect("meta").mode() & 0o777
    }

    #[test]
    fn generated_passwords_are_long_random_hex_and_never_printed() {
        let a = Password::generate().expect("gen");
        let b = Password::generate().expect("gen");
        assert_eq!(a.expose().len(), 2 * PASSWORD_BYTES);
        assert!(a.expose().bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert!(!format!("{a:?}").contains(a.expose()));
    }

    #[test]
    fn acl_is_created_private_and_reused() {
        let d = scratch("create");
        let home = d.path().join("home");
        let acl = home.join("moon.acl");
        let p1 = load_or_create_acl(&acl).expect("create");
        assert_eq!(mode(&acl), 0o600);
        assert_eq!(mode(&home), 0o700);
        let text = std::fs::read_to_string(&acl).expect("read");
        assert_eq!(
            text,
            format!("user default on >{} ~* &* +@all\n", p1.expose())
        );
        assert_eq!(load_or_create_acl(&acl).expect("reuse"), p1);
        // No temp files are left behind.
        let names: Vec<_> = std::fs::read_dir(&home)
            .expect("ls")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("moon.acl")]);
    }

    #[test]
    fn concurrent_creators_agree_on_one_password() {
        let d = scratch("race");
        let acl = d.path().join("moon.acl");
        let got: Vec<Password> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..8)
                .map(|_| s.spawn(|| load_or_create_acl(&acl).expect("load")))
                .collect();
            hs.into_iter().map(|h| h.join().expect("join")).collect()
        });
        assert!(got.windows(2).all(|w| w[0] == w[1]));
    }

    #[test]
    fn symlinked_acl_is_refused() {
        let d = scratch("symlink");
        let target = d.path().join("elsewhere");
        std::fs::write(&target, "user default on >stolen ~* &* +@all\n").expect("write");
        let acl = d.path().join("moon.acl");
        std::os::unix::fs::symlink(&target, &acl).expect("symlink");
        assert!(load_or_create_acl(&acl).is_err());
    }

    #[test]
    fn loose_permissions_are_tightened_and_passwordless_files_rejected() {
        let d = scratch("perms");
        let acl = d.path().join("moon.acl");
        std::fs::write(&acl, "user default on >abc ~* &* +@all\n").expect("write");
        std::fs::set_permissions(&acl, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        assert_eq!(load_or_create_acl(&acl).expect("load").expose(), "abc");
        assert_eq!(mode(&acl), 0o600);

        std::fs::write(&acl, "user default on nopass ~* &* +@all\n").expect("write");
        let e = load_or_create_acl(&acl).expect_err("no password");
        assert!(e.to_string().contains("no password"), "{e}");
    }

    #[test]
    fn private_dir_tightens_existing_dirs() {
        let d = scratch("dir");
        let p = d.path().join("x");
        std::fs::create_dir(&p).expect("mkdir");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        create_private_dir(&p).expect("private");
        assert_eq!(mode(&p), 0o700);
    }

    #[test]
    fn parse_acl_finds_the_default_users_password() {
        assert_eq!(
            parse_acl("# c\nuser other on >x\nuser default on >pw ~* +@all\n").map(|p| p.0),
            Some("pw".into())
        );
        assert_eq!(parse_acl("user default on nopass"), None);
        assert_eq!(parse_acl(""), None);
    }
}
