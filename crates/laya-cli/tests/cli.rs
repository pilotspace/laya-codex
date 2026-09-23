//! End-to-end checks of the `laya` binary that need no Moon and no daemon.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("laya-cli-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `laya` with a private LAYA_HOME, no Moon and no model, so nothing global is touched.
fn laya(home: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_laya"));
    c.env("LAYA_HOME", home)
        .env("LAYA_MOON_BIN", home.join("no-such-moon"))
        .env("LAYA_MOON_PORT", "1")
        .env("LAYA_NO_MODEL", "1")
        .stdin(Stdio::null());
    c
}

fn describe(o: &Output) -> String {
    format!(
        "status {:?}\nstdout: {}\nstderr: {}",
        o.status,
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn writing_to_a_closed_stdout_exits_quietly() {
    let home = scratch("epipe");
    // The read end is closed before the child starts, so its first write gets EPIPE.
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let out = laya(&home)
        .arg("status")
        .stdout(writer)
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    assert!(
        !stderr.contains("panicked") && !stderr.contains("Broken pipe"),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// Runs `laya <args>` and expects a quick failure whose message names `path` and says why.
fn expect_bad_repo(home: &Path, args: &[&str], path: &Path, why: &str) {
    let t0 = std::time::Instant::now();
    let out = laya(home).args(args).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{args:?}: {}", describe(&out));
    assert!(
        stderr.contains(&path.display().to_string()) && stderr.contains(why),
        "{args:?}: {}",
        describe(&out)
    );
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(10),
        "{args:?} took {:?}",
        t0.elapsed()
    );
}

#[test]
fn commands_reject_a_repo_that_is_missing_or_not_a_directory() {
    let home = scratch("badrepo");
    let missing = home.join("no-such-repo");
    let file = home.join("a-file.rs");
    std::fs::write(&file, "fn a() {}\n").unwrap();
    for (p, why) in [(&missing, "does not exist"), (&file, "is not a directory")] {
        let s = p.to_str().unwrap();
        expect_bad_repo(&home, &["index", s], p, why);
        expect_bad_repo(&home, &["query", "x", "--repo", s], p, why);
        expect_bad_repo(&home, &["init", "--no-index", "--repo", s], p, why);
    }
    // Nothing was started on the way.
    assert!(!home.join("laya.sock").exists());
    let _ = std::fs::remove_dir_all(&home);
}
