//! `laya-candgen --moon-bin PATH --port N --home DIR`: starts its own password-protected Moon under
//! `DIR/moon` on `127.0.0.1:N`, then answers JSON lines on stdin, one JSON line each on stdout:
//!
//! ```text
//! {"op":"index","root":"/checkout","repo":"name"}        -> {"ok":true,"files":..,"indexed":..,"removed":..,"failed":..}
//! {"op":"query","repo":"name","prompt":"...","id":"x"}   -> {"id":"x","focus":"...","candidates":[{rank,path,start,end,...}]}
//! ```
//!
//! Moon is stopped when stdin closes or on any error. Driven by `finetune/build_data.py`.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};
use laya_candgen::{candidates, chunk_json, index_repo};
use laya_store::{MoonStore, MoonSupervisor, StoreConfig, load_or_create_acl};
use serde_json::{Value, json};

struct MoonGuard(MoonSupervisor);

impl Drop for MoonGuard {
    fn drop(&mut self) {
        let _ = self.0.stop();
    }
}

fn arg(args: &[String], name: &str) -> anyhow::Result<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
        .with_context(|| format!("{name} is required"))
}

fn handle(store: &MoonStore, req: &Value) -> anyhow::Result<Value> {
    let s = |k: &str| req[k].as_str().with_context(|| format!("missing {k}"));
    match s("op")? {
        "index" => {
            let st = index_repo(&PathBuf::from(s("root")?), store, s("repo")?)?;
            Ok(
                json!({"ok": true, "files": st.files_seen, "indexed": st.indexed,
                      "removed": st.removed, "failed": st.failed}),
            )
        }
        "query" => {
            let got = candidates(store, s("repo")?, s("prompt")?)?;
            let (focus, cands) = match got {
                Some(r) => (
                    r.focus,
                    r.chunks
                        .iter()
                        .enumerate()
                        .map(|(i, c)| chunk_json(i, c))
                        .collect(),
                ),
                None => (String::new(), Vec::new()),
            };
            Ok(json!({"id": req["id"], "focus": focus, "candidates": cands}))
        }
        other => bail!("unknown op {other}"),
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let moon_bin = arg(&args, "--moon-bin")?;
    let port: u16 = arg(&args, "--port")?.parse()?;
    let home = PathBuf::from(arg(&args, "--home")?);
    laya_store::create_private_dir(&home)?;
    let acl = home.join("moon.acl");
    let password = load_or_create_acl(&acl).map_err(|e| anyhow::anyhow!("acl: {e}"))?;
    let sup = MoonSupervisor::new(&moon_bin, port, home.join("moon"))
        .with_auth(password.clone(), &acl)
        .with_spawn_timeout(Duration::from_secs(120));
    sup.ensure_running()
        .map_err(|e| anyhow::anyhow!("moon: {e}"))?;
    let _guard = MoonGuard(sup);
    // Same client settings as the daemon, except a longer query timeout: a batch job would rather
    // wait than get a truncated candidate list.
    let mut cfg = StoreConfig::local(port).with_password(password);
    cfg.query_timeout = Duration::from_secs(5);
    cfg.bulk_timeout = Duration::from_secs(30);
    let store = MoonStore::new(cfg)?;
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = serde_json::from_str(&line)?;
        let resp = handle(&store, &req)
            .unwrap_or_else(|e| json!({"id": req["id"], "error": e.to_string()}));
        writeln!(out, "{resp}")?;
        out.flush()?;
    }
    Ok(())
}
