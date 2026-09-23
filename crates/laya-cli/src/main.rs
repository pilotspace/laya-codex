//! `laya` — code retrieval for Claude Code: tree-sitter chunks, Moon BM25, Laya re-ranking.

mod client;
mod config;
mod daemon;
mod hook;
mod indexer;
mod mcp;
mod protocol;
mod session;

use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use crate::client::Client;
use crate::config::{Config, repo_root};
use crate::hook::{DaemonApi, HookCtx};
use crate::protocol::{Request, Response};

#[derive(Parser)]
#[command(name = "laya", version, about = "Ranked code retrieval for Claude Code (tree-sitter + Moon + Laya)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Index (incrementally) a repository into Moon.
    Index { path: Option<PathBuf> },
    /// Rank code spans for a prompt (starts the daemon if needed).
    Query {
        prompt: String,
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long)]
        top: Option<usize>,
        #[arg(long)]
        json: bool,
    },
    /// Run the daemon in the foreground (normally started on demand).
    Daemon,
    /// Stop a running daemon.
    Stop,
    /// Daemon/model status.
    Status,
    /// Claude Code hook entry point: reads the hook JSON on stdin, prints the hook output.
    Hook,
    /// MCP server over stdio exposing `laya_search`.
    Mcp {
        #[arg(long)]
        repo: Option<PathBuf>,
    },
}

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const INJECT_TOKENS: usize = 3500;

fn main() {
    let cli = Cli::parse();
    let cfg = Config::from_env();
    let res = match cli.cmd {
        Cmd::Index { path } => cmd_index(&cfg, path),
        Cmd::Query { prompt, repo, top, json } => cmd_query(&cfg, &prompt, repo, top, json),
        Cmd::Daemon => cmd_daemon(&cfg),
        Cmd::Stop => cmd_stop(&cfg),
        Cmd::Status => cmd_status(&cfg),
        Cmd::Hook => {
            cmd_hook(&cfg);
            Ok(())
        }
        Cmd::Mcp { repo } => {
            let root = repo_root(&repo.unwrap_or_else(|| PathBuf::from(".")));
            let c = Client::new(&cfg.socket_path(), Duration::from_millis(cfg.budget_ms + 3000), true);
            mcp::serve(&c, root, INJECT_TOKENS, cfg.budget_ms)
        }
    };
    if let Err(e) = res {
        eprintln!("laya: {e:#}");
        std::process::exit(1);
    }
}

fn cmd_index(cfg: &Config, path: Option<PathBuf>) -> anyhow::Result<()> {
    let root = repo_root(&path.unwrap_or_else(|| PathBuf::from(".")));
    let sup = laya_store::MoonSupervisor::new(&cfg.moon_bin, cfg.moon_port, cfg.moon_dir());
    sup.ensure_running().map_err(|e| anyhow::anyhow!("moon: {e}"))?;
    let mut sc = laya_store::StoreConfig::local(cfg.moon_port);
    sc.bulk_timeout = Duration::from_secs(30);
    let store = laya_store::MoonStore::new(sc)?;
    let id = laya_store::repo_id(&root);
    let stats = indexer::index_repo(&root, &store, &id)?;
    println!("{}", json!({"repo": root, "repo_id": id, "stats": stats}));
    Ok(())
}

/// Wait until the daemon answers (starting it if needed); optionally until the model is loaded.
fn wait_ready(c: &Client, want_model: bool, timeout: Duration) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        if let Ok(Response::Pong { model_ready, .. }) = c.call(Request::Ping)
            && (model_ready || !want_model)
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

fn cmd_query(cfg: &Config, prompt: &str, repo: Option<PathBuf>, top: Option<usize>, as_json: bool) -> anyhow::Result<()> {
    let root = repo_root(&repo.unwrap_or_else(|| PathBuf::from(".")));
    let c = Client::new(&cfg.socket_path(), Duration::from_secs(60), true);
    wait_ready(&c, cfg.use_model && cfg.model_dir.is_some(), Duration::from_secs(90));
    let req = Request::Query { repo: root.to_string_lossy().into_owned(), session: None, prompt: prompt.to_string(),
        budget_ms: Some(cfg.budget_ms), top_n: top };
    match c.call(req)? {
        Response::Query { result } if as_json => println!("{}", serde_json::to_string_pretty(&result)?),
        Response::Query { result } => {
            println!("mode={:?} candidates={} elapsed={}ms", result.mode, result.candidates, result.elapsed_ms);
            println!("{}", laya_rank::render_context(&result, 100_000));
        }
        other => anyhow::bail!("unexpected response {other:?}"),
    }
    Ok(())
}

fn cmd_daemon(cfg: &Config) -> anyhow::Result<()> {
    std::fs::create_dir_all(&cfg.home)?;
    std::fs::write(cfg.home.join("daemon.pid"), std::process::id().to_string())?;
    daemon::run(cfg)
}

fn cmd_stop(cfg: &Config) -> anyhow::Result<()> {
    let pid = std::fs::read_to_string(cfg.home.join("daemon.pid")).context("no daemon pidfile")?;
    let status = std::process::Command::new("kill").arg(pid.trim()).status()?;
    let _ = std::fs::remove_file(cfg.socket_path());
    println!("stopped daemon pid {} ({status})", pid.trim());
    Ok(())
}

fn cmd_status(cfg: &Config) -> anyhow::Result<()> {
    let c = Client::new(&cfg.socket_path(), Duration::from_secs(2), false);
    let r = c.call(Request::Ping);
    println!("{}", json!({"socket": cfg.socket_path(), "model_dir": cfg.model_dir, "moon_port": cfg.moon_port,
        "daemon": match r { Ok(Response::Pong { model_ready, version }) => json!({"up": true, "model_ready": model_ready, "version": version}),
                            _ => json!({"up": false}) }}));
    Ok(())
}

/// Hook entry point: never fails, never blocks past its timeouts, prints nothing on error.
fn cmd_hook(cfg: &Config) {
    let t0 = Instant::now();
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        return;
    }
    let Ok(input) = serde_json::from_str::<Value>(&raw) else { return };
    let cwd = input["cwd"].as_str().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let root = repo_root(&cwd);
    let client = Client::new(&cfg.socket_path(), Duration::from_millis(cfg.budget_ms + 1500), true);
    let ctx = HookCtx { api: &client, root, budget_ms: cfg.budget_ms, inject_tokens: INJECT_TOKENS,
        // Defaults match the benchmarked configuration (calibrated laya-code, compact injection).
        read_p: std::env::var("LAYA_READ_P").ok().and_then(|v| v.parse().ok()).unwrap_or(0.4),
        compact: std::env::var("LAYA_RENDER").map(|v| v != "full").unwrap_or(true),
        related: std::env::var("LAYA_RELATED").map(|v| v != "0").unwrap_or(true) };
    let outcome = hook::handle(&input, &ctx);
    if let Some(out) = &outcome.output {
        println!("{out}");
    }
    if let Some(log) = &cfg.hook_log {
        let line = json!({"ts_ms": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
            "session_id": input["session_id"], "event": input["hook_event_name"], "tool": input["tool_name"],
            "action": outcome.action, "injected_chars": outcome.injected_chars, "elapsed_ms": t0.elapsed().as_millis() as u64});
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
            use std::io::Write;
            let _ = writeln!(f, "{line}");
        }
    }
}
