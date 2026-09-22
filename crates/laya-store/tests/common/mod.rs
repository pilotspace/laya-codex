//! Test harness: spawns a private `moon` on a random free port with a temp dir and kills it on
//! drop. `MOON_BIN` overrides the binary path; tests skip (with a message) when it is missing.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use laya_core::{Chunk, Lang};

pub const DEFAULT_MOON_BIN: &str = "/Users/tindang/workspaces/tind-repo/moon/target/release/moon";

pub fn moon_bin() -> Option<PathBuf> {
    let p = std::env::var_os("MOON_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_MOON_BIN));
    if p.is_file() {
        Some(p)
    } else {
        eprintln!(
            "SKIP: moon binary not found at {} (set MOON_BIN)",
            p.display()
        );
        None
    }
}

pub fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().expect("addr").port()
}

pub fn ping(port: u16) -> bool {
    let Ok(mut s) =
        TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(200))
    else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
    if s.write_all(b"*1\r\n$4\r\nPING\r\n").is_err() {
        return false;
    }
    let mut buf = [0u8; 7];
    s.read_exact(&mut buf).is_ok() && &buf == b"+PONG\r\n"
}

/// Max concurrently running test moons per test binary. Each moon is a thread-per-core server;
/// a dozen starting at once starves the machine and turns 250 ms timeouts into flakes.
const MAX_MOONS: usize = 3;
static RUNNING: Mutex<usize> = Mutex::new(0);
static FREED: Condvar = Condvar::new();

struct Permit;

impl Permit {
    fn acquire() -> Self {
        let mut n = RUNNING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *n >= MAX_MOONS {
            n = FREED
                .wait(n)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *n += 1;
        Permit
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        *RUNNING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) -= 1;
        FREED.notify_one();
    }
}

pub struct TestMoon {
    pub port: u16,
    pub dir: tempfile::TempDir,
    bin: PathBuf,
    child: Option<Child>,
    _permit: Permit,
}

impl TestMoon {
    /// `None` (test should return early) if the binary is missing.
    pub fn start() -> Option<Self> {
        let bin = moon_bin()?;
        let permit = Permit::acquire();
        let dir = tempfile::tempdir().expect("tempdir");
        let mut m = TestMoon {
            port: free_port(),
            dir,
            bin,
            child: None,
            _permit: permit,
        };
        m.spawn();
        Some(m)
    }

    fn spawn(&mut self) {
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.path().join("moon.log"))
            .expect("log");
        let child = Command::new(&self.bin)
            .args([
                "--port",
                &self.port.to_string(),
                "--shards",
                "1",
                "--appendonly",
                "yes",
                "--dir",
            ])
            .arg(self.dir.path())
            .stdin(Stdio::null())
            .stdout(log.try_clone().expect("log"))
            .stderr(log)
            .spawn()
            .expect("spawn moon");
        self.child = Some(child);
        let t = Instant::now();
        while !ping(self.port) {
            if let Some(Ok(Some(status))) = self.child.as_mut().map(Child::try_wait) {
                let log =
                    std::fs::read_to_string(self.dir.path().join("moon.log")).unwrap_or_default();
                panic!("moon exited early ({status}) on port {}: {log}", self.port);
            }
            assert!(
                t.elapsed() < Duration::from_secs(10),
                "moon did not come up on port {}",
                self.port
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Hard-kill the server (simulates a crash).
    pub fn kill(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        let t = Instant::now();
        while ping(self.port) && t.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Restart on the same port and data dir.
    pub fn restart(&mut self) {
        self.kill();
        self.spawn();
    }

    pub fn raw(&self) -> redis::Connection {
        redis::Client::open(format!("redis://127.0.0.1:{}/", self.port))
            .expect("client")
            .get_connection()
            .expect("connect")
    }
}

impl Drop for TestMoon {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[macro_export]
macro_rules! require_moon {
    () => {
        match common::TestMoon::start() {
            Some(m) => m,
            None => return,
        }
    };
}

pub fn chunk(path: &str, start: u32, symbol: &str, defines: &[&str], text: &str) -> Chunk {
    Chunk {
        path: path.to_string(),
        start_line: start,
        end_line: start + text.lines().count().max(1) as u32 - 1,
        lang: Lang::Rust,
        symbol: symbol.to_string(),
        kind: "function_item".to_string(),
        defines: defines.iter().map(|s| s.to_string()).collect(),
        text: text.to_string(),
    }
}

pub fn terms(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}
