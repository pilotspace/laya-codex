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
        Client { socket: socket.to_path_buf(), timeout, autostart }
    }

    fn connect(&self) -> anyhow::Result<UnixStream> {
        match UnixStream::connect(&self.socket) {
            Ok(s) => Ok(s),
            Err(e) => {
                if self.autostart {
                    spawn_daemon()?;
                }
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
        reader.read_line(&mut resp).context("daemon read (timeout?)")?;
        if resp.is_empty() {
            bail!("daemon closed the connection");
        }
        match serde_json::from_str(&resp)? {
            Response::Error { message } => bail!("daemon error: {message}"),
            r => Ok(r),
        }
    }
}

/// Start `laya daemon` detached from the calling process (stdio to /dev/null; it logs itself).
pub fn spawn_daemon() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    Command::new(exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn laya daemon")?;
    Ok(())
}
