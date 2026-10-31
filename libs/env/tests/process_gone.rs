//! **C-1: `Verdict::process_gone` — for a row cauldron owns, GONE is the pid,
//! not the port.** And the three ways a pid answers `kill -0` while NOT being
//! the row's process (absent, zombie, reused), each read as gone.
//!
//! Every live-pid fixture here is a REAL child process of this test, and each
//! "reads as gone" claim is paired with the same fixture reading as running,
//! so a constant `true` or `false` fails one side (doctrine 2).

use std::net::TcpListener;
use std::process::{Command, Stdio};

use env::decl::Declaration;
use env::seams::{self, PidSource, Seam, SeamState};

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

fn row(addr: &str, pid: u32, pid_source: &str, started_at: u64) -> Seam {
    Seam {
        service: "svc".into(),
        addr: addr.into(),
        pid,
        boot_token: String::new(),
        version: "v1".into(),
        commit: String::new(),
        dirty: false,
        binary: "/nowhere/svc".into(),
        env: "dev".into(),
        data_root: "/tmp/fake/data/dev".into(),
        data_dir: "/tmp/fake/data/dev/svc".into(),
        started_at,
        probe: "connect".into(),
        registered: false,
        pid_source: pid_source.into(),
        health: String::new(),
    }
}

/// An address nothing listens on: bind, read the port, drop.
fn quiet_addr() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    env::egress::grant_addr(l.local_addr().unwrap()).unwrap(); // REL-6a6: given
    let a = l.local_addr().unwrap().to_string();
    drop(l);
    a
}

fn kill0(pid: u32) -> bool {
    Command::new("/bin/kill").args(["-0", &pid.to_string()]).stderr(Stdio::null()).status().unwrap().success()
}

/// THE C-1 SHAPE: port quiet, pid alive (a service mid-drain). An OWNED row
/// is NOT gone — paired with the same row once the process has exited.
#[test]
fn a_quiet_port_with_a_live_owned_pid_is_not_gone_until_the_pid_is() {
    let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    let r = row(&quiet_addr(), child.id(), PidSource::CHILD, now());
    let v = seams::probe(&r);
    assert_eq!(v.state, SeamState::StaleNoListener, "premise: nothing listens");
    assert!(v.state.is_stale(), "premise: the OLD test (`is_stale`) reads this as gone");
    assert_eq!(v.pid_alive, Some(true));
    assert!(!v.process_gone(&r), "a draining service with its port closed was read as gone");
    assert!(v.owned_and_exiting(&r));

    child.kill().unwrap();
    child.wait().unwrap();
    let v = seams::probe(&r);
    assert_eq!(v.pid_alive, Some(false));
    assert!(v.process_gone(&r), "an exited process must read as gone");
    assert!(!v.owned_and_exiting(&r));
}

/// A row cauldron does NOT own keeps the old test: its pid proves nothing,
/// so a quiet port is all there is.
#[test]
fn a_row_with_an_inferred_pid_is_gone_when_its_port_is_quiet() {
    let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    let r = row(&quiet_addr(), child.id(), "inferred", now());
    let v = seams::probe(&r);
    assert_eq!(v.pid_alive, Some(true));
    assert!(v.process_gone(&r));
    assert!(!v.owned_and_exiting(&r), "a pid cauldron may not signal is never 'ours and exiting'");
    child.kill().unwrap();
    child.wait().unwrap();
}

/// A ZOMBIE answers `kill -0` and is gone. The daemon is the parent of what
/// it forks, so an exited service is a zombie until it is reaped — read as
/// alive, every stop by the daemon would wait out its whole grace and then
/// SIGKILL a corpse. Sentinel: `kill -0` really does succeed on it, so the
/// two instruments disagree on the same pid (doctrine 9).
#[test]
fn an_unreaped_zombie_reads_as_gone_although_kill_0_succeeds() {
    let mut child = Command::new("/usr/bin/true").spawn().unwrap();
    let pid = child.id();
    // Wait for it to exit WITHOUT reaping it.
    let t0 = std::time::Instant::now();
    loop {
        let st = Command::new("/bin/ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().unwrap();
        if String::from_utf8_lossy(&st.stdout).trim().starts_with('Z') {
            break;
        }
        assert!(t0.elapsed().as_secs() < 10, "never became a zombie");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(kill0(pid), "sentinel: kill -0 must succeed on the zombie, or this proves nothing");
    let r = row(&quiet_addr(), pid, PidSource::CHILD, now());
    assert_eq!(seams::row_pid_alive(&r), Some(false), "a zombie is not a running service");
    child.wait().unwrap();
}

/// A REUSED pid — a process that started after the row was written — is not
/// the row's process, and must not hold a stop open or be signalled. Paired:
/// the same live process under a row written AFTER it started is alive.
#[test]
fn a_pid_younger_than_its_row_is_a_reused_pid_and_reads_as_gone() {
    let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    let pid = child.id();
    assert!(kill0(pid));
    // Row written 100 s ago; this process is a second old: reused.
    let reused = row(&quiet_addr(), pid, PidSource::CHILD, now() - 100);
    assert_eq!(seams::row_pid_alive(&reused), Some(false));
    assert!(seams::probe(&reused).process_gone(&reused));
    // Row written now, after the process started: it IS the row's process.
    let ours = row(&quiet_addr(), pid, PidSource::CHILD, now());
    assert_eq!(seams::row_pid_alive(&ours), Some(true));
    // A row with no `started_at` skips the age check.
    let legacy = row(&quiet_addr(), pid, PidSource::CHILD, 0);
    assert_eq!(seams::row_pid_alive(&legacy), Some(true));
    child.kill().unwrap();
    child.wait().unwrap();
}

// ── the declared grace ────────────────────────────────────────────────────

fn parse(body: &str) -> Declaration {
    toml::from_str(body).unwrap()
}

const MIN: &str = "name = \"svc\"\nkind = \"daemon\"\n[build]\nartifact = \"/bin/true\"\nbin = \"true\"\n";

#[test]
fn stop_grace_defaults_to_15_reads_a_declared_value_and_refuses_zero() {
    assert_eq!(parse(MIN).stop_grace_secs, 15);
    let d = parse(&format!("stop_grace_secs = 75\n{MIN}"));
    assert_eq!(d.stop_grace_secs, 75);
    d.validate().unwrap();
    let z = parse(&format!("stop_grace_secs = 0\n{MIN}"));
    let e = z.validate().unwrap_err().to_string();
    assert!(e.contains("stop_grace_secs = 0"), "{e}");
}

/// Doctrine 10, the population: the service the gap is about declares a
/// grace that covers its measured 55 s clean stop, and the voice daemon —
/// which does not drain — is left on the default. Read from the shipped
/// files, not restated.
#[test]
fn the_shipped_threads_declaration_gives_its_55s_drain_at_least_60s() {
    let services = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../services");
    let threads = env::decl::load(&services, "threads").unwrap();
    assert!(threads.stop_grace_secs >= 60, "threads declares {}", threads.stop_grace_secs);
    let tts = env::decl::load(&services, "tts").unwrap();
    assert_eq!(tts.stop_grace_secs, 15, "counterpart: tts is on the default");
}
