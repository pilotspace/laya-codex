//! Timeouts, retries, reconnects and the circuit breaker.

mod common;

use std::net::TcpListener;
use std::time::{Duration, Instant};

use common::{chunk, terms};
use laya_core::{Error, Store};
use laya_store::{BreakerState, MoonStore, StoreConfig};

const R: &str = "dddddddddddd";

fn fast_cfg(port: u16) -> StoreConfig {
    StoreConfig {
        breaker_cooldown: Duration::from_millis(300),
        ..StoreConfig::local(port)
    }
}

#[test]
fn construction_is_lazy_and_dead_server_fails_fast_then_opens() {
    let port = common::free_port(); // nothing listens here
    let s = MoonStore::new(fast_cfg(port)).expect("lazy construction never does IO");
    let t = Instant::now();
    for _ in 0..5 {
        match s.memo_get("x") {
            Err(Error::StoreUnavailable(_)) => {}
            other => panic!("expected StoreUnavailable, got {other:?}"),
        }
    }
    assert!(
        t.elapsed() < Duration::from_secs(2),
        "refused connects must be fast: {:?}",
        t.elapsed()
    );
    assert_eq!(s.breaker_state(), BreakerState::Open);

    let t = Instant::now();
    assert!(matches!(
        s.bm25(R, &terms(&["xx"]), 5),
        Err(Error::StoreUnavailable(_))
    ));
    assert!(
        t.elapsed() < Duration::from_millis(5),
        "open breaker must not touch the network"
    );
}

#[test]
fn query_times_out_against_a_hung_server() {
    // Accepts connections but never answers.
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = l.local_addr().expect("addr").port();
    let _hold = std::thread::spawn(move || {
        let mut socks = Vec::new();
        for s in l.incoming().flatten() {
            socks.push(s);
        }
    });
    let cfg = StoreConfig {
        query_timeout: Duration::from_millis(100),
        ..fast_cfg(port)
    };
    let s = MoonStore::new(cfg).expect("store");
    let t = Instant::now();
    let r = s.memo_get("x");
    assert!(matches!(r, Err(Error::StoreUnavailable(_))), "{r:?}");
    // Timeouts are not retried on the query path: one timeout plus slack.
    assert!(
        t.elapsed() < Duration::from_millis(400),
        "took {:?}",
        t.elapsed()
    );
}

#[test]
fn server_errors_do_not_trip_the_breaker() {
    let m = require_moon!();
    let s = MoonStore::new(fast_cfg(m.port)).expect("store");
    let mut con = m.raw();
    let _: () = redis::cmd("SET")
        .arg(format!("lc:{R}:files"))
        .arg("not a set")
        .query(&mut con)
        .expect("set");
    for _ in 0..8 {
        assert!(matches!(s.list_files(R), Err(Error::Store(_))));
    }
    assert_eq!(s.breaker_state(), BreakerState::Closed);
}

#[test]
fn reconnects_transparently_after_moon_restart() {
    let mut m = require_moon!();
    let s = MoonStore::new(fast_cfg(m.port)).expect("store");
    s.memo_put("a", "1", 0).expect("put");
    m.restart();
    // The pooled connection is dead; the retry must reconnect without surfacing an error.
    s.memo_put("b", "2", 0).expect("put after restart");
    assert_eq!(s.breaker_state(), BreakerState::Closed);
}

#[test]
fn breaker_opens_when_moon_dies_and_recovers_after_restart() {
    let mut m = require_moon!();
    let s = MoonStore::new(fast_cfg(m.port)).expect("store");
    s.ensure_index(R).expect("ensure");
    let a = chunk("a.rs", 1, "", &[], "fn breaker_marker() {}");
    s.put_file(R, "a.rs", "h", std::slice::from_ref(&a))
        .expect("put");
    std::thread::sleep(Duration::from_millis(1200)); // let the AOF fsync

    m.kill();
    for _ in 0..5 {
        assert!(matches!(
            s.bm25(R, &terms(&["breaker_marker"]), 5),
            Err(Error::StoreUnavailable(_))
        ));
    }
    assert_eq!(s.breaker_state(), BreakerState::Open);
    let t = Instant::now();
    assert!(matches!(
        s.file_hash(R, "a.rs"),
        Err(Error::StoreUnavailable(_))
    ));
    assert!(t.elapsed() < Duration::from_millis(5));

    m.restart();
    std::thread::sleep(Duration::from_millis(350)); // > cooldown -> half-open probe allowed
    let got = s
        .bm25(R, &terms(&["breaker_marker"]), 5)
        .expect("recovered");
    assert_eq!(got[0].0, a.id());
    assert_eq!(s.breaker_state(), BreakerState::Closed);
}
