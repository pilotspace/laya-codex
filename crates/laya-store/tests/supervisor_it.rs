//! MoonSupervisor: spawn / health / restart / stop of the local moon sidecar.

mod common;

use std::net::TcpListener;
use std::path::Path;
use std::time::{Duration, Instant};

use laya_core::Store;
use laya_store::{MoonStore, MoonSupervisor, StoreConfig, SupervisorStatus};

/// Stops whatever moon a test left running on its port, even when the test panics.
struct Cleanup(MoonSupervisor);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = self.0.stop();
    }
}

fn setup() -> Option<(MoonSupervisor, tempfile::TempDir, u16)> {
    let bin = common::moon_bin()?;
    let dir = tempfile::tempdir().expect("tempdir");
    let port = common::free_port();
    Some((MoonSupervisor::new(bin, port, dir.path()), dir, port))
}

fn read_pid(p: &Path) -> u32 {
    std::fs::read_to_string(p)
        .expect("pidfile")
        .trim()
        .parse()
        .expect("pid")
}

#[test]
fn spawns_when_down_is_idempotent_and_stops() {
    let Some((sup, dir, port)) = setup() else {
        return;
    };
    let _c = Cleanup(MoonSupervisor::new(
        common::moon_bin().expect("bin"),
        port,
        dir.path(),
    ));
    assert!(!sup.is_running());

    let status = sup.ensure_running().expect("spawn");
    let SupervisorStatus::Spawned { pid } = status else {
        panic!("expected Spawned, got {status:?}")
    };
    assert!(sup.is_running());
    assert!(common::ping(port));
    assert_eq!(read_pid(&sup.pidfile()), pid);
    assert!(
        sup.logfile().exists(),
        "moon.log must be created in the data dir"
    );

    assert_eq!(
        sup.ensure_running().expect("second"),
        SupervisorStatus::AlreadyRunning
    );

    sup.stop().expect("stop");
    assert!(!sup.is_running());
    assert!(!sup.pidfile().exists());
    sup.stop().expect("stop when already stopped is ok");
}

#[test]
fn restarts_after_crash_and_keeps_the_index() {
    let Some((sup, dir, port)) = setup() else {
        return;
    };
    let _c = Cleanup(MoonSupervisor::new(
        common::moon_bin().expect("bin"),
        port,
        dir.path(),
    ));
    let SupervisorStatus::Spawned { pid } = sup.ensure_running().expect("spawn") else {
        panic!()
    };

    let store = MoonStore::new(StoreConfig::local(port)).expect("store");
    let repo = "eeeeeeeeeeee";
    store.ensure_index(repo).expect("index");
    let c = common::chunk("s.rs", 1, "", &["Sup"], "fn supervised_marker() {}");
    store
        .put_file(repo, "s.rs", "h", std::slice::from_ref(&c))
        .expect("put");
    std::thread::sleep(Duration::from_millis(1200)); // appendfsync everysec

    // SAFETY: plain FFI call; `pid` is the moon we spawned and still own.
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    let t = Instant::now();
    while sup.is_running() && t.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!sup.is_running());

    let SupervisorStatus::Spawned { pid: pid2 } = sup.ensure_running().expect("respawn") else {
        panic!()
    };
    assert_ne!(pid, pid2);
    let hits = store
        .bm25(repo, &common::terms(&["supervised_marker"]), 5)
        .expect("bm25 after restart");
    assert_eq!(hits[0].0, c.id());
}

#[test]
fn stop_works_from_a_fresh_supervisor_via_pidfile_and_moon_outlives_its_spawner() {
    let Some((sup, dir, port)) = setup() else {
        return;
    };
    let bin = common::moon_bin().expect("bin");
    let _c = Cleanup(MoonSupervisor::new(bin.clone(), port, dir.path()));
    sup.ensure_running().expect("spawn");
    drop(sup); // the moon is detached: dropping its spawner must not stop it
    assert!(common::ping(port));

    let other = MoonSupervisor::new(bin, port, dir.path());
    assert_eq!(
        other.ensure_running().expect("ensure"),
        SupervisorStatus::AlreadyRunning
    );
    other.stop().expect("stop via pidfile");
    assert!(!common::ping(port));
}

#[test]
fn missing_binary_fails_fast() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sup = MoonSupervisor::new("/nonexistent/moon", common::free_port(), dir.path());
    let t = Instant::now();
    assert!(sup.ensure_running().is_err());
    assert!(t.elapsed() < Duration::from_secs(1));
}

#[test]
fn port_held_by_a_foreign_listener_is_an_error_within_the_deadline() {
    let Some((_, dir, _)) = setup() else { return };
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = l.local_addr().expect("addr").port();
    let sup = MoonSupervisor::new(common::moon_bin().expect("bin"), port, dir.path());
    let t = Instant::now();
    let r = sup.ensure_running();
    assert!(r.is_err(), "{r:?}");
    assert!(
        t.elapsed() < Duration::from_secs(4),
        "took {:?}",
        t.elapsed()
    );
    assert!(!sup.pidfile().exists());
}
