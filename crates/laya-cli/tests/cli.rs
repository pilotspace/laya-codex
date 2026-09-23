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
