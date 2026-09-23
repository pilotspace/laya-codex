//! Password-protected Moon: the supervisor spawns Moon with laya-codex's ACL file and password, the
//! store authenticates every connection, and a Moon laya-codex cannot authenticate against is refused.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use laya_core::Store;
use laya_store::{
    MoonProbe, MoonStore, MoonSupervisor, Password, StoreConfig, SupervisorStatus,
    load_or_create_acl,
};

/// Stops the moon a test started, even when the test panics.
struct Cleanup(MoonSupervisor);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = self.0.stop();
    }
}

fn protected(bin: &Path, port: u16, home: &Path) -> (MoonSupervisor, Password) {
    let acl = home.join("moon.acl");
    let pw = load_or_create_acl(&acl).expect("acl");
    let sup = MoonSupervisor::new(bin, port, home.join("moon")).with_auth(pw.clone(), acl);
    (sup, pw)
}

fn store(port: u16, pw: Option<&Password>) -> MoonStore {
    let mut cfg = StoreConfig {
        max_retries: 1,
        ..StoreConfig::local(port)
    };
    cfg.password = pw.cloned();
    MoonStore::new(cfg).expect("store")
}

fn process_args(pid: u32) -> String {
    let out = std::process::Command::new("ps")
        .args(["-o", "args=", "-p", &pid.to_string()])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn protected_moon_serves_ft_search_to_laya_only_and_survives_restarts() {
    let Some(bin) = common::moon_bin() else {
        return;
    };
    let home = tempfile::tempdir().expect("tempdir");
    let port = common::free_port();
    let (sup, pw) = protected(&bin, port, home.path());
    let _c = Cleanup(protected(&bin, port, home.path()).0);

    let SupervisorStatus::Spawned { pid } = sup.ensure_running().expect("spawn") else {
        panic!("expected a spawn")
    };
    assert_eq!(sup.probe(), MoonProbe::Ready);
    assert!(sup.is_running());
    assert!(process_args(pid).contains("--aclfile"));
    // An anonymous client gets NOAUTH, not PONG.
    assert!(!common::ping(port), "unauthenticated PING must be rejected");
    let anon = store(port, None);
    assert!(anon.ensure_index("aaaaaaaaaaaa").is_err());

    // FT.CREATE / HSET / FT.SEARCH as the authenticated default user.
    let s = store(port, Some(&pw));
    let repo = "aaaaaaaaaaaa";
    s.ensure_index(repo).expect("ft.create");
    let c = common::chunk("a.rs", 1, "", &["Auth"], "fn authenticated_marker() {}");
    s.put_file(repo, "a.rs", "h", std::slice::from_ref(&c))
        .expect("put");
    let hits = s
        .bm25(repo, &common::terms(&["authenticated_marker"]), 5)
        .expect("ft.search");
    assert_eq!(hits[0].0, c.id());
    std::thread::sleep(Duration::from_millis(1200)); // appendfsync everysec

    // Crash + respawn: pooled connections are dead; reconnects must authenticate again.
    // SAFETY: plain FFI call; `pid` is the moon this test spawned.
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    let t = Instant::now();
    while sup.probe() != MoonProbe::Down && t.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(matches!(
        sup.ensure_running().expect("respawn"),
        SupervisorStatus::Spawned { .. }
    ));
    let hits = s
        .bm25(repo, &common::terms(&["authenticated_marker"]), 5)
        .expect("ft.search after reconnect");
    assert_eq!(hits[0].0, c.id());
}

#[test]
fn circuit_breaker_probe_reconnects_with_auth() {
    let Some(bin) = common::moon_bin() else {
        return;
    };
    let home = tempfile::tempdir().expect("tempdir");
    let port = common::free_port();
    let (sup, pw) = protected(&bin, port, home.path());
    let _c = Cleanup(protected(&bin, port, home.path()).0);
    sup.ensure_running().expect("spawn");
    let s = MoonStore::new(StoreConfig {
        max_retries: 0,
        breaker_threshold: 1,
        breaker_cooldown: Duration::from_millis(200),
        password: Some(pw),
        ..StoreConfig::local(port)
    })
    .expect("store");
    s.ensure_index("bbbbbbbbbbbb").expect("index");
    sup.stop().expect("stop");
    assert!(s.list_files("bbbbbbbbbbbb").is_err());
    assert_eq!(s.breaker_state(), laya_store::BreakerState::Open);
    sup.ensure_running().expect("respawn");
    std::thread::sleep(Duration::from_millis(250));
    s.list_files("bbbbbbbbbbbb")
        .expect("half-open probe authenticates");
}

#[test]
fn unprotected_moon_on_the_port_is_refused() {
    let m = require_moon!();
    let home = tempfile::tempdir().expect("tempdir");
    let bin = common::moon_bin().expect("bin");
    let (sup, pw) = protected(&bin, m.port, home.path());

    assert_eq!(sup.probe(), MoonProbe::Unprotected);
    assert!(!sup.is_running());
    let e = sup.ensure_running().expect_err("must refuse").to_string();
    assert!(
        e.contains(&format!("port {}", m.port))
            && e.contains("without laya-codex's password")
            && e.contains("LAYA_CODEX_MOON_PORT"),
        "{e}"
    );
    // The client refuses it too (Moon without a password accepts any AUTH, so it checks).
    let e = store(m.port, Some(&pw))
        .ensure_index("cccccccccccc")
        .expect_err("store must refuse")
        .to_string();
    assert!(e.contains("without laya-codex's password"), "{e}");
    let mut raw = m.raw();
    let n: usize = redis::cmd("DBSIZE").query(&mut raw).expect("dbsize");
    assert_eq!(n, 0, "nothing may be written into an unprotected moon");
    // Nothing was spawned or recorded.
    assert!(!sup.pidfile().exists());
}

#[test]
fn moon_with_another_password_is_refused() {
    let Some(bin) = common::moon_bin() else {
        return;
    };
    let (a, b) = (
        tempfile::tempdir().expect("a"),
        tempfile::tempdir().expect("b"),
    );
    let port = common::free_port();
    let (sup_a, _) = protected(&bin, port, a.path());
    let _c = Cleanup(protected(&bin, port, a.path()).0);
    sup_a.ensure_running().expect("spawn");

    let (sup_b, _) = protected(&bin, port, b.path());
    assert_eq!(sup_b.probe(), MoonProbe::WrongPassword);
    let e = sup_b.ensure_running().expect_err("must refuse").to_string();
    assert!(
        e.contains("rejects laya-codex's password") && e.contains("LAYA_CODEX_MOON_PORT"),
        "{e}"
    );
}

#[test]
fn legacy_passwordless_moon_started_by_laya_is_replaced_and_keeps_its_data() {
    let Some(bin) = common::moon_bin() else {
        return;
    };
    let home = tempfile::tempdir().expect("tempdir");
    let port = common::free_port();
    // An older laya-codex: same data dir, no password.
    let old = MoonSupervisor::new(&bin, port, home.path().join("moon"));
    let _c = Cleanup(MoonSupervisor::new(&bin, port, home.path().join("moon")));
    let SupervisorStatus::Spawned { pid: old_pid } = old.ensure_running().expect("old") else {
        panic!()
    };
    let legacy = store(port, None);
    let repo = "dddddddddddd";
    legacy.ensure_index(repo).expect("index");
    let c = common::chunk("l.rs", 1, "", &["Legacy"], "fn legacy_marker() {}");
    legacy
        .put_file(repo, "l.rs", "h", std::slice::from_ref(&c))
        .expect("put");
    std::thread::sleep(Duration::from_millis(1200)); // appendfsync everysec

    let (sup, pw) = protected(&bin, port, home.path());
    let _c2 = Cleanup(protected(&bin, port, home.path()).0);
    let SupervisorStatus::Spawned { pid } = sup.ensure_running().expect("upgrade") else {
        panic!("expected the legacy moon to be replaced")
    };
    assert_ne!(pid, old_pid);
    assert_eq!(sup.probe(), MoonProbe::Ready);
    let hits = store(port, Some(&pw))
        .bm25(repo, &common::terms(&["legacy_marker"]), 5)
        .expect("data survives the upgrade");
    assert_eq!(hits[0].0, c.id());
}
