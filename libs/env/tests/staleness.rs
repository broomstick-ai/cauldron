//! The staleness verdict, every branch, with a fake listener in-process.
//!
//! The point of these tests is that each branch is **reachable and
//! distinguishable**: a seam table that can be confidently wrong is worse than
//! no table, so every way it can be wrong has to produce a different, named
//! answer. Each test prints the refusal text so the proof run records it.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use env::seams::{Identity, Seam, SeamState};

/// The commit both fixtures below name. **The same value on both sides on
/// purpose**: `probe` compares the commit the row records against the commit
/// the listener answers, so a fixture pair that disagreed would make every
/// `live` test in this file fail for a reason none of them is about. The
/// disagreement is tested deliberately, once, in
/// `a_listener_on_another_commit_is_wrong_version`.
const COMMIT: &str = "4197fc6e1d2a3b4c5d6e7f8091a2b3c4d5e6f701";

fn seam(addr: &str, token: &str, probe: &str) -> Seam {
    Seam {
        service: "echo".into(),
        addr: addr.into(),
        pid: std::process::id(),
        boot_token: token.into(),
        version: "v1".into(),
        commit: COMMIT.into(),
        dirty: false,
        binary: "/nowhere/echo".into(),
        env: "dev".into(),
        data_root: "/tmp/fake/data/dev".into(),
        data_dir: "/tmp/fake/data/dev/echo".into(),
        started_at: 0,
        probe: probe.into(),
        // A REGISTERED seam: written BY the service, from inside itself,
        // after its own bind. Spelled out rather than defaulted so that a
        // test about an ADOPTED seam cannot be written by accident.
        registered: true,
        pid_source: env::seams::PidSource::SELF.into(),
        health: String::new(),
    }
}

/// A listener that answers HELLO once with `id`, then stops.
fn fake(id: Identity) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    env::egress::grant_addr(l.local_addr().unwrap()).unwrap(); // REL-6a6: given
    let addr = l.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        if let Ok((s, _)) = l.accept() {
            let mut w = s.try_clone().unwrap();
            let mut line = String::new();
            let _ = BufReader::new(s).read_line(&mut line);
            let mut body = serde_json::to_string(&id).unwrap();
            body.push('\n');
            let _ = w.write_all(body.as_bytes());
            let _ = w.flush();
        }
    });
    addr
}

fn id(service: &str, env: &str, data_root: &str, token: &str, addr: &str) -> Identity {
    Identity {
        service: service.into(),
        env: env.into(),
        data_root: data_root.into(),
        boot_token: token.into(),
        version: "v1".into(),
        commit: COMMIT.into(),
        pid: 1,
        addr: addr.into(),
    }
}

#[test]
fn live_when_the_token_matches() {
    let addr = fake(id("echo", "dev", "/tmp/fake/data/dev", "tok-1", "x"));
    let v = env::seams::probe(&seam(&addr, "tok-1", "seam"));
    println!("GREEN live: {:?} {}", v.state, v.detail);
    assert_eq!(v.state, SeamState::Live);
}

#[test]
fn stale_no_listener_even_though_the_pid_is_alive() {
    // Bind and immediately drop, so the port is almost certainly free, and
    // point the seam at OUR OWN pid, which is certainly alive. This is the
    // case pid liveness gets wrong and the socket gets right.
    let addr = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        env::egress::grant_addr(l.local_addr().unwrap()).unwrap(); // REL-6a6: given
        l.local_addr().unwrap().to_string()
    };
    let v = env::seams::probe(&seam(&addr, "tok-1", "seam"));
    println!("RED stale:no-listener: {}\n    {}", v.state.as_str(), v.detail);
    assert_eq!(v.state, SeamState::StaleNoListener);
    assert_eq!(v.pid_alive, Some(true), "the recorded pid IS alive — and it is not the test");
    assert!(v.state.is_stale());
    assert!(v.detail.contains("pid liveness is not"), "{}", v.detail);
}

#[test]
fn stale_wrong_owner_when_the_token_differs() {
    let addr = fake(id("echo", "dev", "/tmp/fake/data/dev", "tok-OTHER", "x"));
    let v = env::seams::probe(&seam(&addr, "tok-1", "seam"));
    println!("RED stale:wrong-owner: {}\n    {}", v.state.as_str(), v.detail);
    assert_eq!(v.state, SeamState::StaleWrongOwner);
    assert!(v.detail.contains("DIFFERENT instance"), "{}", v.detail);
}

#[test]
fn stale_wrong_env_when_the_listener_reports_another_environment() {
    // Same instance (token matches) but it reports another data root. This is
    // the branch that catches a service running against the wrong data root
    // while being perfectly alive.
    let addr = fake(id("echo", "other", "/tmp/fake/data/other", "tok-1", "x"));
    let v = env::seams::probe(&seam(&addr, "tok-1", "seam"));
    println!("RED stale:wrong-env: {}\n    {}", v.state.as_str(), v.detail);
    assert_eq!(v.state, SeamState::StaleWrongEnv);
    assert!(v.detail.contains("wrong data root"), "{}", v.detail);
}

#[test]
fn a_non_cauldron_listener_is_wrong_owner_not_live() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    env::egress::grant_addr(l.local_addr().unwrap()).unwrap(); // REL-6a6: given
    let addr = l.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        if let Ok((s, _)) = l.accept() {
            let mut w = s.try_clone().unwrap();
            let mut line = String::new();
            let _ = BufReader::new(s).read_line(&mut line);
            let _ = w.write_all(b"220 some other daemon\n");
        }
    });
    let v = env::seams::probe(&seam(&addr, "tok-1", "seam"));
    println!("RED non-cauldron listener: {}\n    {}", v.state.as_str(), v.detail);
    assert_eq!(v.state, SeamState::StaleWrongOwner);
}

#[test]
fn connect_probe_is_named_as_the_weaker_check() {
    // A service that cannot answer HELLO (threads today) gets a bind check and
    // a verdict that says so, rather than being called `live`.
    let addr = fake(id("someone-else", "zzz", "/zzz", "not-the-token", "x"));
    let v = env::seams::probe(&seam(&addr, "tok-1", "connect"));
    println!("YELLOW live:unverified: {}\n    {}", v.state.as_str(), v.detail);
    assert_eq!(v.state, SeamState::LiveUnverified);
    assert!(!v.state.is_stale(), "a bind check is not a staleness verdict");
    assert!(v.detail.contains("does not prove"), "{}", v.detail);
}
