//! **CAULDRON SUPERVISES AND RESTARTS WHAT IS DOWN — the real binary, a real
//! daemon, real children, a real kill.**
//!
//! Every test here starts a THROWAWAY cauldron: its own `CAULDRON_HOME` under
//! the system temp dir, its own environment name, its own kernel-assigned
//! ports. Nothing reads or writes `~/.cauldron`, nothing signals a live
//! daemon, nothing touches launchd. The services are two hosted fake daemons
//! (a python script installed as an `artifact`), so no compiler runs inside a
//! test and no shared target directory is written.
//!
//! Claims, each forced red once (see the R5.S1 report):
//!
//! 1. `a_killed_service_is_restarted_and_its_sibling_is_left_alone` — two
//!    registered + pinned services come up under the daemon; one is SIGKILLed;
//!    the daemon relaunches THAT one (a new pid, listening again, a `launch`
//!    record with `why = down`), and the other keeps its pid.
//! 2. `a_service_that_keeps_dying_is_relaunched_with_backoff_not_at_poll_rate`
//!    — a second death soon after a relaunch is NOT relaunched on the next
//!    pass: the daemon says it is backing off and the second relaunch lands
//!    no sooner than `backoff_base_secs` after the first. The longest period
//!    in the path is that backoff (3 s here; poll is 1 s), and the gap is
//!    measured against it.
//! 3. `with_auto_restart_off_a_killed_service_stays_down` — the opposing
//!    case: the same kill under `auto_restart = false` is reported and NOT
//!    relaunched, outlasting 4 poll periods, so the restart in (1) is the
//!    policy's doing and not something else bringing the port back. The kill
//!    waits for the daemon's FIRST PASS to re-attach svc-a (R5.S8, ledger
//!    415): a kill before that pass is met by the first-launch rule.
//! 7. `requested_restarts_and_stops_leave_no_zombie_under_the_holder_past_3_polls`
//!    — R5.S8 / D-4: a REGISTERING and a hosted service, each restarted 3x
//!    and stopped 1x on request, leave no `Z` child of the holder, both at
//!    each stop record and after 3 poll periods.
//! 6. `a_service_up_past_stable_after_15s_has_its_backoff_reset_by_the_daemon`
//!    — the STABLE RESET wired through `tend()`, not just the decision
//!    (R5.S1b inspection F-A: `if false && entry.observed_live(..)` in
//!    `tend` left every test green). A crash-looping service that then stays
//!    up past `stable_after_secs` relaunches as consecutive launch 1 with the
//!    base wait, and the daemon says it reset. Paired, in the same test, with
//!    a run SHORTER than `stable_after_secs` that must NOT reset: the climb
//!    goes 1, 2, 3 — so an unconditional reset in `tend` is red too.
//! 5. `cauldron_test_refuses_a_service_still_launched_outside_before_building_it`
//!    — `cauldron test` reaches `start_one` without passing the daemon's
//!    launch chokepoint, so it carries the `[migration]` refusal itself (R5.S1
//!    inspection F-2). Paired with the same STAMPED fake minus `[migration]`,
//!    which `test` builds and launches — so the refusal is what stops it, not
//!    the unstamped-build refusal that made F-2 latent.
//! 8. `r9c3_a_build_that_binds_then_dies_after_2s_inside_stable_after_8s_is_never_good_and_auto_rolls_back`
//!    — R9.C3 (gap C-3): a build held up past `stable_after_secs` is recorded
//!    good only AFTER that period (not at bind) and is not rolled back; a
//!    build that binds and dies 2 s later is never good, each death counts,
//!    and after `max_start_failures` the daemon re-pins to the good build.
//! 9. `r9c3_a_build_that_binds_then_dies_after_2s_with_no_good_target_parks_inside_stable_after_8s`
//!    — the same dying build with nothing proven behind it PARKS.
//! 10. `r9c4_a_registering_service_the_daemon_launched_is_good_only_after_stable_after_8s_and_one_dying_after_2s_never`
//!    — R9.C4 (gap C-4): the daemon records a REGISTERING service good once
//!    it has stayed up past `stable_after_secs` (not at registration); a
//!    registering build that dies 2 s in is never good.
//! 11. `r9c4_cauldron_start_does_not_record_a_build_that_binds_then_dies_after_2s_good_inside_stable_after_8s`
//!    — `cauldron start` (no holder) applies the same stable-period rule;
//!    paired with `stable_after_secs = 0`, where the rule records at start.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_wait.rs"]
mod launch_wait;
use launch_wait::START_TIMEOUT_MS;

#[path = "support/first_exec.rs"]
mod first_exec;

const FAKE: &str = r#"#!/usr/bin/env python3
# A fake hosted daemon: binds the port it is told and sits. Appends its pid to
# a file the test reads, so every incarnation can be cleaned up.
import os, socket, sys, time
# Asked for its stamp (a declaration with `stamp_args`), it states a commit, so
# its build is STAMPED and `start_one` will launch it. Used by test 5.
if sys.argv[1:] == ["--build-stamp"]:
    print("commit=5eed5eed5eed5eed5eed5eed5eed5eed5eed5eed provenance=pin dirty=0 epoch=1")
    sys.exit(0)
port = int(os.environ["FAKE_PORT"])
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", port)); s.listen(8)
with open(os.environ["FAKE_PIDS"], "a") as f:
    f.write(f"{os.getpid()}\n")
# ACCEPT AND CLOSE every connection. A listener that never accepts fills its
# backlog (8) with the daemon's 1/s liveness probes plus the test's own
# `listening()` polls, after which connect times out and the daemon reads a
# healthy fake as DOWN — seconds after it came up. Every test before 6 killed
# its fake before that happened; 6 holds one up for 15 s and found it.
while True:
    c, _ = s.accept()
    c.close()
"#;

struct Sandbox {
    root: PathBuf,
    env: String,
    daemon: Option<Child>,
    log: PathBuf,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Some(mut d) = self.daemon.take() {
            // R5.S8: the holder's own children first, by PARENTAGE. A fake the
            // holder forked but which has not yet written its pid to
            // `fake-pids` (a test that panicked mid-launch) is not in that
            // file, and killing the holder first reparents it to init — one
            // such fake outlived a forced-red run of test 7.
            for (p, _) in children_of(d.id()) {
                let _ = Command::new("/bin/kill").args(["-9", &p.to_string()]).status();
            }
            let _ = d.kill();
            let _ = d.wait();
        }
        // Every incarnation of every fake, whatever state the test died in.
        if let Ok(t) = std::fs::read_to_string(self.root.join("fake-pids")) {
            for p in t.lines() {
                let _ = Command::new("/bin/kill").args(["-9", p.trim()]).status();
            }
        }
        if std::env::var_os("KEEP_SANDBOX").is_none() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn listening(port: u16) -> bool {
    let sa: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    TcpStream::connect_timeout(&sa, Duration::from_millis(200)).is_ok()
}

fn alive(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Poll `f` until it returns `Some`, or panic naming `what` after `budget`.
fn wait_for<T>(what: &str, budget: Duration, mut f: impl FnMut() -> Option<T>) -> T {
    let t0 = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        if t0.elapsed() > budget {
            panic!("timed out after {budget:?} waiting for: {what}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

impl Sandbox {
    fn new(tag: &str, supervisor: &str) -> Sandbox {
        let root = std::env::temp_dir().join(format!(
            "cauldron-r5s1-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        // Premise: nothing about this root is the operator's.
        assert!(!root.starts_with(dirs_home().join(".cauldron")), "sandbox must not be ~/.cauldron");
        let env = format!("r5s1{tag}");
        std::fs::create_dir_all(root.join("services")).unwrap();
        std::fs::create_dir_all(root.join("data").join(&env)).unwrap();
        std::fs::write(root.join("data").join(&env).join("env.toml"), format!("[supervisor]\n{supervisor}\n"))
            .unwrap();
        let fake = root.join("fake-daemon");
        std::fs::write(&fake, launch_wait::slow_launch_py(&first_exec::slow_first_exec_py(FAKE, &root.join("first-exec-marks")), &root.join("f2g-marks"))).unwrap();
        Command::new("/bin/chmod").args(["+x"]).arg(&fake).status().unwrap();
        let log = root.join("daemon.log");
        Sandbox { root, env, daemon: None, log }
    }

    fn cauldron(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_cauldron"))
            .args(args)
            .env("CAULDRON_HOME", &self.root)
            .env("CAULDRON_ENV", &self.env)
            .output()
            .expect("spawn cauldron");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }

    /// Declare, build, and pin one hosted fake on its own port.
    fn register(&self, name: &str) -> u16 {
        let port = self.declare(name, "");
        let label = self.build(name);
        let (rc, out) = self.cauldron(&["pin", name, &label]);
        assert_eq!(rc, 0, "pin {name}: {out}");
        port
    }

    /// Build `name` and return the label `versions` reports.
    fn build(&self, name: &str) -> String {
        let (rc, out) = self.cauldron(&["build", name]);
        assert_eq!(rc, 0, "build {name}: {out}");
        let (rc, out) = self.cauldron(&["versions", name, "--json"]);
        assert_eq!(rc, 0, "versions {name}: {out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        let label = v[0]["label"].as_str().expect("a label").to_string();
        self.pay_build(name, &label);
        label
    }

    /// R9.F1: pay the first exec of the copy `cauldron build` just made of
    /// the fixture (a NEW file, so a new assessment — see
    /// `support/first_exec.rs`), here, outside every deadline: the daemon's
    /// first launch of it is then not charged the scan inside its own launch
    /// deadline, where an overrun counts as a failed start. Both fakes
    /// answer `--build-stamp` and exit without binding.
    fn pay_build(&self, name: &str, label: &str) {
        let dir = self.root.join("data").join("shared").join("builds").join(name).join(label);
        let exes = first_exec::executables_under(&dir);
        assert!(!exes.is_empty(), "sentinel: `build` left no executable in {}", dir.display());
        for x in exes {
            first_exec::pay_first_exec(Command::new(&x).arg("--build-stamp"), &format!("{name}'s build {label}"));
        }
    }

    /// R9.F1: place this environment's own launch path the documented way
    /// (`promote`, the same byte copy `daemon` would make at start) and pay
    /// its first exec here, so the holder's exec onto it is not its first.
    /// Once per sandbox: a second `promote` would write a NEW file.
    fn place_launch_path(&self) {
        let own = self.root.join("bin").join(format!("cauldron-{}", self.env));
        if own.is_file() {
            return;
        }
        let (rc, out) = self.cauldron(&["promote"]);
        assert_eq!(rc, 0, "promote: {out}");
        assert!(own.is_file(), "`promote` did not place {}: {out}", own.display());
        first_exec::pay_first_exec(
            Command::new(&own).arg("envs").env("CAULDRON_HOME", &self.root).env("CAULDRON_ENV", &self.env),
            "the holder's launch path",
        );
    }

    /// Write the declaration of one hosted fake, with `extra` appended.
    fn declare(&self, name: &str, extra: &str) -> u16 {
        let port = free_port();
        std::fs::write(
            self.root.join("services").join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\nkind = \"daemon\"\nregisters = false\nprobe = \"connect\"\n\
                 stop_signal = \"TERM\"\nstart_timeout_ms = {START_TIMEOUT_MS}\n\
                 [launch]\naddr = \"127.0.0.1:{port}\"\n\
                 [env]\nFAKE_PORT = \"{port}\"\nFAKE_PIDS = \"{}\"\n\
                 [build]\nartifact = \"{}\"\nbin = \"fake-daemon\"\n",
                self.root.join("fake-pids").display(),
                self.root.join("fake-daemon").display()
            ) + extra,
        )
        .unwrap();
        port
    }

    fn start_daemon(&mut self) {
        self.place_launch_path();
        let log = std::fs::File::create(&self.log).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_cauldron"))
            .arg("daemon")
            .env("CAULDRON_HOME", &self.root)
            .env("CAULDRON_ENV", &self.env)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .expect("spawn daemon");
        self.daemon = Some(child);
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// The pid the seam table records for `name`, if any.
    fn pid_of(&self, name: &str) -> Option<u32> {
        let t = std::fs::read_to_string(self.root.join("data").join(&self.env).join("seams.json")).ok()?;
        let v: serde_json::Value = serde_json::from_str(&t).ok()?;
        v["seams"][name]["pid"].as_u64().map(|p| p as u32)
    }

    /// Up = the table names a pid, that pid is alive, and the port answers.
    fn up(&self, name: &str, port: u16) -> Option<u32> {
        let pid = self.pid_of(name)?;
        (alive(pid) && listening(port)).then_some(pid)
    }

    fn supervisions(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.root.join("data").join(&self.env).join("supervisions.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn launches(&self, name: &str) -> usize {
        self.supervisions()
            .iter()
            .filter(|r| r["service"] == name && r["act"] == "launch")
            .count()
    }
}

fn dirs_home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn kill9(pid: u32) {
    assert!(Command::new("/bin/kill").args(["-9", &pid.to_string()]).status().unwrap().success());
    wait_for("the killed pid to be gone", Duration::from_secs(10), || (!alive(pid)).then_some(()));
}

const RESTARTING: &str = "auto_restart = true\nauto_rollback = false\npoll_secs = 1\ndown_after = 1\n\
                          max_start_failures = 3\nbackoff_base_secs = 3\nbackoff_max_secs = 30\n\
                          stable_after_secs = 600";

/// How long a POSITIVE wait ("svc up", "relaunched") may take before it is a
/// failure. DERIVED, not guessed (R9.C1 inspect F3, doctrine 4 in reverse — a
/// positive wait must outlast the longest period in its path, or it reads a
/// slow machine as a regression). The path to "up" is: holder start, then up
/// to `max_start_failures` (3) launches each bounded by the declared
/// `start_timeout_ms` ([`launch_wait::START_TIMEOUT`], 60 s since R9.F2g —
/// the largest launch deadline any fixture here sits in, above
/// `HOSTED_BIND_WAIT`), with backoff between them capped at
/// `backoff_max_secs` (30 s) — 3 x 60 + 30 = 210 s — plus 30 s for the
/// holder itself and a python3 interpreter starting under load (measured at ~20 s on a loaded run by the R9.C1 inspector; the
/// old 40 s bound failed at base and tip alike). A longer bound costs time
/// only on a run that is failing anyway.
const BUDGET: Duration = Duration::from_secs(3 * launch_wait::START_TIMEOUT_SECS + 30 + 30);

/// **The declared period on the path to a service's FIRST "up" under a fresh
/// holder (R9.F1)**: the holder's first pass (`poll_secs` 1 x `down_after` 1
/// = 1 s), then one launch bounded by the longest launch deadline any fixture
/// here can sit in — a registering one's `start_timeout_ms`,
/// [`launch_wait::START_TIMEOUT`] (60 s since R9.F2g; `HOSTED_BIND_WAIT`,
/// 45 s in `cli/src/daemon.rs`, bounds a hosted fake and is shorter) — then,
/// if that launch failed, one backoff of at most `backoff_max_secs` (30 s)
/// and a launch that succeeds. 1 + 60 + 30 = 91 s.
const FIRST_UP_DECLARED: Duration = Duration::from_secs(1 + launch_wait::START_TIMEOUT_SECS + 30);

/// A FIRST wait: [`FIRST_UP_DECLARED`] plus the measured worst-case first
/// exec (`support/first_exec.rs`) of the two fresh files on that path — the
/// holder's own launch path and the service's build copy. Both are paid
/// before the clock starts (`place_launch_path`, `pay_build`); they are still
/// budgeted, so a pre-pay that stops reaching one reads as slow, not red.
fn first_up() -> Duration {
    first_exec::first_wait(FIRST_UP_DECLARED, 2)
}

fn both_up(sb: &mut Sandbox) -> (u16, u16, u32, u32) {
    let pa = sb.register("svc-a");
    let pb = sb.register("svc-b");
    sb.start_daemon();
    let a = wait_for("svc-a up under the daemon", first_up(), || sb.up("svc-a", pa));
    let b = wait_for("svc-b up under the daemon", first_up(), || sb.up("svc-b", pb));
    // Premise of every claim below: the DAEMON launched both (not a stray
    // `start`), so a relaunch later is the daemon's too. Waited for, not read
    // once: the daemon writes its `launch` record AFTER the row the sighting
    // above reads, and under a slow launch (R9.F2g, a forced 15 s first
    // launch) the sighting won that race and this read found no record yet.
    wait_for("the daemon's own launch record of svc-a and svc-b", first_up(), || {
        (sb.launches("svc-a") >= 1 && sb.launches("svc-b") >= 1).then_some(())
    });
    (pa, pb, a, b)
}

#[test]
fn a_killed_service_is_restarted_and_its_sibling_is_left_alone() {
    let mut sb = Sandbox::new("restart", RESTARTING);
    let (pa, pb, a1, b1) = both_up(&mut sb);
    let launches_before = sb.launches("svc-a");

    kill9(a1);
    let a2 = wait_for("svc-a relaunched with a NEW pid", BUDGET, || {
        sb.up("svc-a", pa).filter(|p| *p != a1)
    });
    assert_ne!(a1, a2);
    assert!(sb.launches("svc-a") > launches_before, "no new launch record:\n{}", sb.log());
    let recs = sb.supervisions();
    let last = recs
        .iter()
        .rev()
        .find(|r| r["service"] == "svc-a" && r["act"] == "launch")
        .expect("a launch record");
    assert_eq!(last["why"], "down", "the relaunch must be recorded as a DOWN restart: {last}");
    assert_eq!(last["pid"].as_u64(), Some(a2 as u64), "{last}");
    let log = sb.log();
    assert!(log.contains("`svc-a` is DOWN"), "the daemon must say it found svc-a down:\n{log}");

    // The sibling: same pid, still listening, never relaunched.
    assert_eq!(sb.up("svc-b", pb), Some(b1), "svc-b was disturbed:\n{log}");
    assert_eq!(sb.launches("svc-b"), 1, "svc-b was relaunched:\n{log}");

    // And `cauldron status` lists both, up, supervised by cauldron.
    let (_, st) = sb.cauldron(&["status", "svc-a", "svc-b"]);
    for n in ["svc-a", "svc-b"] {
        let line = st.lines().find(|l| l.starts_with(n)).unwrap_or_else(|| panic!("{n} missing:\n{st}"));
        assert!(line.contains("live:unverified") && line.contains("cauldron"), "{line}");
    }
}

#[test]
fn a_service_that_keeps_dying_is_relaunched_with_backoff_not_at_poll_rate() {
    let mut sb = Sandbox::new("backoff", RESTARTING);
    let (pa, _pb, a1, _b1) = both_up(&mut sb);

    kill9(a1);
    let a2 = wait_for("first relaunch", BUDGET, || sb.up("svc-a", pa).filter(|p| *p != a1));
    let t_first = Instant::now();
    // Dies again at once — inside the 3 s backoff the first relaunch set.
    kill9(a2);
    let a3 = wait_for("second relaunch", BUDGET, || sb.up("svc-a", pa).filter(|p| *p != a2));
    let gap = t_first.elapsed();
    assert_ne!(a2, a3);
    let log = sb.log();
    assert!(
        log.contains("`svc-a` is DOWN in") && log.contains("backing off"),
        "the second death must be met with a stated backoff, not an immediate relaunch:\n{log}"
    );
    // poll_secs = 1 and down_after = 1: without backoff the second relaunch
    // lands within ~1-2 s. With it, not before backoff_base_secs (3 s) after
    // the first launch. 2.5 s leaves margin for when `t_first` was taken.
    assert!(gap >= Duration::from_millis(2500), "second relaunch after {gap:?}, inside the 3 s backoff:\n{log}");
}

#[test]
fn with_auto_restart_off_a_killed_service_stays_down() {
    let mut sb = Sandbox::new("norestart", "auto_restart = false\npoll_secs = 1\ndown_after = 1");
    let pa = sb.register("svc-a");
    let pb = sb.register("svc-b");
    // auto_restart = false: the daemon launches nothing, so bring them up by
    // hand with `start`, which refuses while a daemon holds the environment —
    // so start first, then the daemon.
    let (rc, out) = sb.cauldron(&["start", "svc-a", "svc-b"]);
    assert_eq!(rc, 0, "{out}");
    let a1 = wait_for("svc-a up", first_up(), || sb.up("svc-a", pa));
    wait_for("svc-b up", first_up(), || sb.up("svc-b", pb));
    sb.start_daemon();
    wait_for("daemon holding", first_up(), || sb.log().contains("auto_restart is OFF").then_some(()));
    // R5.S8 (ledger 415, R6.S6b-2): WAIT FOR THE FIRST SUPERVISION PASS TO
    // HAVE SEEN svc-a LIVE before killing it. The hold line above is printed
    // BEFORE the first pass; a kill landing between the two is met by a first
    // pass that has neither launched nor found svc-a, and the first-launch
    // rule (R6.S3: each pinned service is launched once per incarnation)
    // brings it back — correctly — and this test went red on scheduling. The
    // marker is the daemon's own re-attach line, which `tend` prints only on
    // the pass that finds svc-a live with no pid of its own yet.
    wait_for("the first pass to re-attach svc-a", first_up(), || {
        sb.log().contains("re-attached `svc-a`").then_some(())
    });
    kill9(a1);
    // Outlast 4 poll periods (poll_secs = 1, down_after = 1).
    std::thread::sleep(Duration::from_secs(5));
    assert!(!listening(pa), "svc-a came back with auto_restart = false:\n{}", sb.log());
    let log = sb.log();
    assert!(log.contains("auto_restart = false` here"), "the daemon must say why it left svc-a down:\n{log}");
}

/// 4. A service whose declaration says an OUTSIDE launcher still owns it is
///    never launched by cauldron: `pin` refuses it, and a pin written into
///    `env.toml` by hand reaches the daemon's launch path and is refused
///    THERE — the port stays empty. The opposing case is every other test in
///    this file: the same fake without `[migration]` pins and launches.
#[test]
fn a_service_still_launched_outside_is_never_launched_by_cauldron() {
    let mut sb = Sandbox::new("outside", RESTARTING);
    let port = sb.declare(
        "svc-o",
        "[migration]\nlaunched_by = \"launchd:com.example.test\"\nsteps = [\"bootout\", \"delete this\"]\n",
    );
    let label = sb.build("svc-o");
    let (rc, out) = sb.cauldron(&["pin", "svc-o", &label]);
    println!("{out}");
    assert_ne!(rc, 0, "pin must refuse a service an outside launcher owns:\n{out}");
    assert!(out.contains("still launched by `launchd:com.example.test`"), "{out}");

    // Bypass `pin`: write the pin the way a hand edit would.
    let manifest = sb.root.join("data").join(&sb.env).join("env.toml");
    let mut m = std::fs::read_to_string(&manifest).unwrap();
    m.push_str(&format!("\n[services.svc-o]\nversion = \"{label}\"\n"));
    std::fs::write(&manifest, m).unwrap();
    sb.start_daemon();
    wait_for("the daemon's launch path to refuse svc-o", first_up(), || {
        sb.log().contains("still launched by `launchd:com.example.test`").then_some(())
    });
    assert!(!listening(port), "cauldron launched a service an outside launcher owns:\n{}", sb.log());
    assert_eq!(sb.launches("svc-o"), 0, "{}", sb.log());

    let (_, st) = sb.cauldron(&["status", "svc-o"]);
    let line = st.lines().find(|l| l.starts_with("svc-o")).unwrap_or_else(|| panic!("{st}"));
    assert!(line.contains("outside:down") && line.contains("launchd:com.example.test"), "{line}");
}

impl Sandbox {
    /// A fake whose build is STAMPED (`stamp_args`), so a launch path can
    /// actually launch it — the unstamped artifact `declare` writes is refused
    /// by `start_one` for naming no commit, which would mask a missing
    /// `[migration]` check. `tail` is appended after the `[build]` table.
    fn declare_stamped(&self, name: &str, tail: &str) -> u16 {
        let port = free_port();
        std::fs::write(
            self.root.join("services").join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\nkind = \"daemon\"\nregisters = false\nprobe = \"connect\"\n\
                 stop_signal = \"TERM\"\nstart_timeout_ms = 3000\n\
                 [launch]\naddr = \"127.0.0.1:{port}\"\n\
                 [env]\nFAKE_PORT = \"{port}\"\nFAKE_PIDS = \"{}\"\n\
                 [build]\nartifact = \"{}\"\nbin = \"fake-daemon\"\nstamp_args = [\"--build-stamp\"]\n",
                self.root.join("fake-pids").display(),
                self.root.join("fake-daemon").display()
            ) + tail,
        )
        .unwrap();
        port
    }

    /// Every pid any fake incarnation recorded: the fake writes its pid the
    /// moment it has bound, so a non-empty list means something LAUNCHED it.
    fn fake_pids(&self) -> Vec<String> {
        std::fs::read_to_string(self.root.join("fake-pids"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Build records of `name` in the TEST environment's registry.
    fn builds_of(&self, name: &str) -> usize {
        let (_, out) = self.cauldron(&["versions", name, "--json"]);
        serde_json::from_str::<serde_json::Value>(&out).ok().and_then(|v| v.as_array().map(Vec::len)).unwrap_or(0)
    }
}

/// 5. See the file doc. The test environment is the sandbox's own `<env>-t`,
///    named explicitly so nothing defaults toward a name the operator uses.
#[test]
fn cauldron_test_refuses_a_service_still_launched_outside_before_building_it() {
    let sb = Sandbox::new("testverb", RESTARTING);
    let test_env = format!("{}-t", sb.env);

    // THE OPPOSING CASE FIRST, so the refusal below cannot be vacuous: the
    // stamped fake WITHOUT `[migration]` is built and LAUNCHED by `test`.
    let port_ok = sb.declare_stamped("svc-in", "");
    let (rc, out) = sb.cauldron(&["test", "svc-in", "--test-env", &test_env]);
    println!("svc-in: rc={rc}\n{out}");
    assert!(out.contains("cauldron: built svc-in as 5eed5eed5eed"), "the stamped fixture did not build as a commit:\n{out}");
    // SPAWNED: this is `start_one`'s own post-spawn timeout (a hosted fake
    // never registers a seam), printed only after it forked the binary. The
    // fake's pid file is not the evidence here: a freshly installed script's
    // first exec can take seconds on this machine, longer than the 3 s
    // start timeout, so whether it wrote its pid before being killed is
    // scheduling, and a guard must not depend on it.
    assert!(
        out.contains("`svc-in` did not write its seam") && out.contains("killed pid"),
        "`cauldron test` did not reach the launch of the stamped fake, so the refusal below would be vacuous:\n{out}"
    );
    let _ = port_ok;

    // THE CLAIM: the same fake, owned by an outside launcher, is refused —
    // and refused before the build, so the refusal leaves nothing behind.
    let pids_before = sb.fake_pids().len();
    let port = sb.declare_stamped(
        "svc-o",
        "[migration]\nlaunched_by = \"launchd:com.example.test\"\nsteps = [\"bootout\", \"delete this\"]\n",
    );
    let (rc, out) = sb.cauldron(&["test", "svc-o", "--test-env", &test_env]);
    println!("svc-o: rc={rc}\n{out}");
    assert!(out.contains("still launched by `launchd:com.example.test`"), "`cauldron test` did not refuse a [migration] service:\n{out}");
    assert_ne!(rc, 0, "{out}");
    assert!(!out.contains("cauldron: built svc-o"), "`cauldron test` built a [migration] service before refusing it:\n{out}");
    assert_eq!(sb.builds_of("svc-o"), 0, "a build record was left behind:\n{out}");
    assert!(!out.contains("`svc-o` did not write its seam"), "`cauldron test` spawned a [migration] service:\n{out}");
    // `cauldron test` is synchronous: a launch it made would already have run
    // its whole start_timeout_ms (3 s) before the command returned.
    assert!(!listening(port), "cauldron test launched a service an outside launcher owns:\n{out}");
    assert_eq!(sb.fake_pids().len(), pids_before, "a fake was launched for svc-o:\n{out}");
}

/// The `consecutive launch N` the daemon announced for each launch of `name`,
/// in order — read from the daemon's own launch line, which is where the
/// backoff exponent it is about to use is stated.
fn launch_numbers(log: &str, name: &str) -> Vec<u32> {
    let site = format!("`{name}` is DOWN in");
    log.lines()
        .filter(|l| l.contains(&site))
        .filter_map(|l| l.split("(consecutive launch ").nth(1))
        .filter_map(|t| t.split(';').next()?.trim().parse().ok())
        .collect()
}

/// The wait the daemon announced after the LAST launch of `name`.
fn last_announced_wait(log: &str, name: &str) -> Option<String> {
    let site = format!("`{name}` is DOWN in");
    let l = log.lines().filter(|l| l.contains(&site) && l.contains("consecutive launch")).last()?;
    Some(l.split("waits ").nth(1)?.trim_end_matches(')').to_string())
}

/// 6. See the file doc. The period outlasted (doctrine 4) is
///    `stable_after_secs = 15`: the short run is killed after 3 s, the long
///    run is held until the daemon says it reset (or 30 s, so that a
///    disabled reset is caught by the BEHAVIOUR assertion below rather than
///    only by a missing log line).
#[test]
fn a_service_up_past_stable_after_15s_has_its_backoff_reset_by_the_daemon() {
    let mut sb = Sandbox::new(
        "stable",
        "auto_restart = true\nauto_rollback = false\npoll_secs = 1\ndown_after = 1\n\
         max_start_failures = 3\nbackoff_base_secs = 3\nbackoff_max_secs = 60\n\
         stable_after_secs = 15",
    );
    let pa = sb.register("svc-a");
    sb.start_daemon();
    let a1 = wait_for("svc-a up under the daemon", first_up(), || sb.up("svc-a", pa));

    // A crash loop: dies at once, then again after a 3 s run — both far
    // short of 15 s.
    kill9(a1);
    let a2 = wait_for("relaunch 2", BUDGET, || sb.up("svc-a", pa).filter(|p| *p != a1));
    std::thread::sleep(Duration::from_secs(3));
    kill9(a2);
    let a3 = wait_for("relaunch 3", BUDGET, || sb.up("svc-a", pa).filter(|p| *p != a2));
    // OPPOSING ARM: a 3 s run is not stable. The climb continued; a `tend`
    // that reset on every live pass would read 1, 2, 1 here.
    assert_eq!(
        launch_numbers(&sb.log(), "svc-a"),
        vec![1, 2, 3],
        "a run shorter than stable_after_secs reset the backoff:\n{}",
        sb.log()
    );

    // THE CLAIM: held up past 15 s, the daemon resets the backoff.
    let reset = "`svc-a` has stayed up for 15s after 3 consecutive launch(es); its restart backoff is reset";
    let t_up = Instant::now();
    wait_for("the reset, or 30 s up", Duration::from_secs(60), || {
        (sb.log().contains(reset) || t_up.elapsed() > Duration::from_secs(30)).then_some(())
    });
    assert_eq!(sb.up("svc-a", pa), Some(a3), "svc-a did not stay up through its stable run:\n{}", sb.log());
    kill9(a3);
    wait_for("relaunch 4", BUDGET, || sb.up("svc-a", pa).filter(|p| *p != a3));
    let log = sb.log();
    // BEHAVIOUR: the relaunch after a stable run is consecutive launch 1, and
    // the wait it schedules is the 3 s base — not launch 4 waiting 24 s.
    assert_eq!(launch_numbers(&log, "svc-a"), vec![1, 2, 3, 1], "the stable run did not reset the backoff:\n{log}");
    assert_eq!(last_announced_wait(&log, "svc-a").as_deref(), Some("3s"), "the wait after a stable run is not the base:\n{log}");
    // And the daemon said so — its own line, printed only inside tend's reset.
    assert!(log.contains(reset), "the daemon never announced the reset:\n{log}");
}

// ═══════════════════════ R5.S8 (D-4): A STOPPED CHILD IS REAPED ═══════════
//
// D-4, found at R3.S3's tts cut-over (2026-10-10): "39035 is left a zombie
// under the supervisor (unreaped)" after one requested restart — one per
// request. The holder's reaper (`daemon::FORKED` + `daemon::reap`, 791f5dfa)
// only ever held HOSTED children: `launch_hosted` hands its `Child` to the
// reaper, but `start_one` — the REGISTERING launch path, which is how `tts`
// runs — dropped its handle at the end of the function. A dropped `Child` is
// never waited on, so every registering service the holder ever stopped stayed
// a `<defunct>` entry under it until the holder itself exited (which every
// deploy does — why a `ps` after Deploys 2-6 showed none).

/// A REGISTERING fake: it binds `CAULDRON_BIND`, writes its OWN seam row into
/// `CAULDRON_SEAMS` (under the table's lock file, with the boot token it was
/// handed — the same row `ServiceEnv::register` writes), and answers
/// `WARNINGS <shape>` on its socket. It is the `registers = true` half of the
/// launch chokepoint, which the hosted `FAKE` above can never reach.
const REGISTERING_FAKE: &str = r#"#!/usr/bin/env python3
import json, os, socket, sys, threading, time
if sys.argv[1:] == ["--build-stamp"]:
    print("commit=5eed5eed5eed5eed5eed5eed5eed5eed5eed5eed provenance=pin dirty=0 epoch=1")
    sys.exit(0)
E = os.environ
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
host, port = E["CAULDRON_BIND"].rsplit(":", 1)
s.bind((host, int(port))); s.listen(16)
addr = "%s:%d" % s.getsockname()
seams = E["CAULDRON_SEAMS"]
lock = seams[: -len(".json")] + ".lock" if seams.endswith(".json") else seams + ".lock"
root = os.path.normpath(E["CAULDRON_DATA_ROOT"])
while True:
    try:
        fd = os.open(lock, os.O_CREAT | os.O_EXCL | os.O_WRONLY); break
    except FileExistsError:
        time.sleep(0.02)
try:
    try:
        t = json.load(open(seams))
    except FileNotFoundError:
        t = {"table_version": 1, "env": E["CAULDRON_ENV"], "data_root": root, "seams": {}}
    svc = E["CAULDRON_SERVICE"]
    t["seams"][svc] = {
        "service": svc, "addr": addr, "pid": os.getpid(),
        "boot_token": E["CAULDRON_BOOT_TOKEN"], "version": E["CAULDRON_VERSION"],
        "commit": E["CAULDRON_COMMIT"], "dirty": E["CAULDRON_DIRTY"] in ("1", "true"),
        "binary": E.get("CAULDRON_BINARY", ""), "env": E["CAULDRON_ENV"], "data_root": root,
        "data_dir": E["CAULDRON_SERVICE_DATA_DIR"], "started_at": int(time.time()),
        "probe": "connect", "registered": True, "pid_source": "self",
    }
    tmp = seams + ".tmp.%d" % os.getpid()
    with open(tmp, "w") as f:
        json.dump(t, f, indent=2); f.write("\n")
    os.rename(tmp, seams)
finally:
    os.close(fd); os.remove(lock)
with open(E["FAKE_PIDS"], "a") as f:
    f.write("%d\n" % os.getpid())
since = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
def serve(c):
    try:
        c.settimeout(1.0)
        line = c.makefile().readline().split()
        if line[:1] == ["WARNINGS"]:
            shape = line[1] if len(line) > 1 else "warnings"
            c.sendall((json.dumps({"shape": shape, "shape_version": 1, "producer": svc,
                                   "counts_since": since, "warn": 0, "error": 0}) + "\n").encode())
    except Exception:
        pass
    finally:
        c.close()
while True:
    c, _ = s.accept()
    threading.Thread(target=serve, args=(c,), daemon=True).start()
"#;

impl Sandbox {
    /// Declare, build and pin one REGISTERING fake (`registers = true`, a
    /// stamped build so `start_one` will launch it).
    fn register_registering(&self, name: &str) {
        let fake = self.root.join("registering-fake");
        std::fs::write(&fake, launch_wait::slow_launch_py(&first_exec::slow_first_exec_py(REGISTERING_FAKE, &self.root.join("first-exec-marks")), &self.root.join("f2g-marks")))
            .unwrap();
        Command::new("/bin/chmod").args(["+x"]).arg(&fake).status().unwrap();
        std::fs::write(
            self.root.join("services").join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\nkind = \"daemon\"\nregisters = true\nprobe = \"connect\"\n\
                 stop_signal = \"TERM\"\nstart_timeout_ms = {START_TIMEOUT_MS}\n\
                 [env]\nFAKE_PIDS = \"{}\"\n\
                 [build]\nartifact = \"{}\"\nbin = \"registering-fake\"\nstamp_args = [\"--build-stamp\"]\n",
                self.root.join("fake-pids").display(),
                fake.display()
            ),
        )
        .unwrap();
        let label = self.build(name);
        let (rc, out) = self.cauldron(&["pin", name, &label]);
        assert_eq!(rc, 0, "pin {name}: {out}");
    }

    /// Up = the table names a pid, it is alive, and the address the row
    /// names answers a connect.
    fn up_at_row(&self, name: &str) -> Option<u32> {
        let t = std::fs::read_to_string(self.root.join("data").join(&self.env).join("seams.json")).ok()?;
        let v: serde_json::Value = serde_json::from_str(&t).ok()?;
        let row = &v["seams"][name];
        let pid = row["pid"].as_u64()? as u32;
        let sa: SocketAddr = row["addr"].as_str()?.parse().ok()?;
        (alive(pid) && TcpStream::connect_timeout(&sa, Duration::from_millis(200)).is_ok()).then_some(pid)
    }

    /// The holder's own address, off the row it wrote for itself.
    fn holder_addr(&self) -> Option<String> {
        let t = std::fs::read_to_string(self.root.join("data").join(&self.env).join("seams.json")).ok()?;
        let v: serde_json::Value = serde_json::from_str(&t).ok()?;
        v["seams"]["cauldron"]["addr"].as_str().map(str::to_string)
    }

    /// One line to the holder's seam, one line back — the same exchange
    /// `cauldron restart` has with a held environment, for the verb (`STOP`)
    /// no CLI verb sends yet.
    fn ask_holder(&self, line: &str) -> String {
        use std::io::{BufRead, Write};
        let addr = self.holder_addr().expect("the holder's row");
        let s = TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_secs(2)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut w = s.try_clone().unwrap();
        w.write_all(format!("{line}\n").as_bytes()).unwrap();
        let mut out = String::new();
        std::io::BufReader::new(s).read_line(&mut out).unwrap();
        out.trim().to_string()
    }

    fn acts(&self, name: &str, act: &str) -> usize {
        self.supervisions().iter().filter(|r| r["service"] == name && r["act"] == act).count()
    }
}

/// `(pid, stat)` of every process whose PARENT is `ppid`, from `ps -axo` —
/// macOS has no `--ppid`. The direct child of the test is the holder, and the
/// holder is the parent of every service it forked.
fn children_of(ppid: u32) -> Vec<(u32, String)> {
    let out = Command::new("/bin/ps").args(["-axo", "pid=,ppid=,stat="]).output().expect("ps");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() >= 3 && f[1].parse::<u32>().ok()? == ppid).then(|| (f[0].parse().unwrap(), f[2].to_string()))
        })
        .collect()
}

fn zombies_of(ppid: u32) -> Vec<(u32, String)> {
    children_of(ppid).into_iter().filter(|(_, st)| st.starts_with('Z')).collect()
}

/// 7. R5.S8 / D-4. Three REQUESTED restarts and one REQUESTED stop of each of
///    a registering and a hosted service, under a throwaway holder, leave NO
///    `Z` child of that holder — measured after outlasting 3 poll periods
///    (poll_secs = 1), so a zombie reaped "eventually" by a later pass is not
///    what is being credited for the registering arm: before the fix, that
///    arm's four zombies persist for the life of the holder.
#[test]
fn requested_restarts_and_stops_leave_no_zombie_under_the_holder_past_3_polls() {
    let mut sb = Sandbox::new("reap", RESTARTING);
    sb.register_registering("svc-r");
    let ph = sb.register("svc-h");
    sb.start_daemon();
    let holder = sb.daemon.as_ref().unwrap().id();
    let mut r = wait_for("svc-r up under the daemon", first_up(), || sb.up_at_row("svc-r"));
    let mut h = wait_for("svc-h up under the daemon", first_up(), || sb.up("svc-h", ph));

    // SENTINEL (doctrine 9): the instrument sees the holder's children at all,
    // and both services ARE its children — so an empty zombie list below is a
    // statement about these processes, not a filter that matches nothing.
    let kids: Vec<u32> = children_of(holder).into_iter().map(|(p, _)| p).collect();
    assert!(kids.contains(&r) && kids.contains(&h), "services are not children of holder {holder}: {kids:?}");
    // And the registering one really came up through `start_one`: its row is
    // its own (`registered = true`, pid source `self`).
    let t: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(sb.root.join("data").join(&sb.env).join("seams.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(t["seams"]["svc-r"]["registered"], true, "{}", t["seams"]["svc-r"]);
    assert_eq!(t["seams"]["svc-h"]["registered"], false, "{}", t["seams"]["svc-h"]);

    let mut stopped = Vec::new();
    for i in 1..=3 {
        for (name, cur) in [("svc-r", &mut r), ("svc-h", &mut h)] {
            let launches = sb.acts(name, "launch");
            let stops = sb.acts(name, "stop");
            let (rc, out) = sb.cauldron(&["restart", name]);
            assert_eq!(rc, 0, "restart {i} of {name}: {out}");
            let old = *cur;
            // AT THE STOP, not eventually: the stop record is written by
            // `stop_by_seam` after it reaped, and the relaunch (a python
            // interpreter start, then a seam write) is still ahead of the
            // end-of-pass `reap`. So the old pid read here, a few ms after
            // its stop record, is `<defunct>` unless the stop path reaped it.
            let t0 = Instant::now();
            while sb.acts(name, "stop") <= stops {
                assert!(t0.elapsed() < BUDGET, "no stop record for restart {i} of {name}:\n{}", sb.log());
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                !zombies_of(holder).iter().any(|(p, _)| *p == old),
                "restart {i} of {name}: stopped pid {old} is <defunct> under holder {holder} at its stop record"
            );
            *cur = wait_for(&format!("{name} restarted ({i}) with a new pid"), BUDGET, || {
                let p = if name == "svc-r" { sb.up_at_row(name) } else { sb.up(name, ph) };
                p.filter(|p| *p != old && sb.acts(name, "launch") > launches)
            });
            stopped.push(old);
        }
    }
    for (name, cur) in [("svc-r", r), ("svc-h", h)] {
        let stops = sb.acts(name, "stop");
        assert_eq!(sb.ask_holder(&format!("STOP {name}")), "OK queued");
        wait_for(&format!("{name} stopped on request"), BUDGET, || {
            (sb.acts(name, "stop") > stops && !alive_not_zombie(cur)).then_some(())
        });
        stopped.push(cur);
    }
    let reqs = sb.supervisions().iter().filter(|r| r["why"] == "requested" && r["act"] == "stop").count();
    assert_eq!(reqs, 8, "expected 8 requested stops (3 restarts + 1 stop, x2):\n{}", sb.log());

    // Outlast 3 poll periods (poll_secs = 1).
    std::thread::sleep(Duration::from_secs(3));
    let z = zombies_of(holder);
    assert!(
        z.is_empty(),
        "holder {holder} has {} unreaped child(ren) {z:?} after {} requested stops of {stopped:?}:\n{}",
        z.len(),
        stopped.len(),
        sb.log()
    );
}

/// Alive and NOT a zombie. `kill -0` succeeds on a zombie, so `alive` alone
/// cannot say a stopped child has gone.
fn alive_not_zombie(pid: u32) -> bool {
    let out = Command::new("/bin/ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().unwrap();
    let st = String::from_utf8_lossy(&out.stdout).trim().to_string();
    !st.is_empty() && !st.starts_with('Z')
}

// ═══════════ R9.C3 (gap C-3): BOUND IS NOT GOOD; A CRASH LOOP ENDS ═══════════
//
// C-3: "A build that binds its port and then panics after 20 s is recorded
// 'good'. It relaunches forever and auto-rollback never fires." The launch
// counted a bind as a successful start, which cleared the failure count and
// wrote the build into `good` — so the crash loop could never reach
// `max_start_failures`, and the broken build became its own rollback target.
//
// The period outlasted (doctrine 4) is `stable_after_secs = 8` (C3_STABLE).
// The dying fake lives C3_DIES_AFTER = 2 s, well inside it; the good fake is
// held past it. Both are named in the test names.

const C3_STABLE: u64 = 8;
const C3_DIES_AFTER: u64 = 2;

fn c3_policy() -> String {
    format!(
        "auto_restart = true\nauto_rollback = true\npoll_secs = 1\ndown_after = 1\n\
         max_start_failures = 3\nbackoff_base_secs = 1\nbackoff_max_secs = 2\n\
         stable_after_secs = {C3_STABLE}"
    )
}

/// The FAKE, except that it binds, writes its pid, serves for `secs` seconds
/// and then exits 3 — the "binds its port, then panics" build. A different
/// script, so its build is a different label from the FAKE's.
fn dying_fake(secs: u64) -> String {
    FAKE.replace(
        "while True:\n    c, _ = s.accept()\n    c.close()\n",
        &format!(
            "deadline = time.time() + {secs}\ns.settimeout(0.2)\nwhile time.time() < deadline:\n    \
             try:\n        c, _ = s.accept(); c.close()\n    except socket.timeout:\n        pass\n\
             sys.exit(3)\n"
        ),
    )
}

impl Sandbox {
    /// `name`'s proven-good list as `env.toml` records it, newest first.
    fn good(&self, name: &str) -> Vec<String> {
        ::env::decl::Manifest::load(&self.root.join("data").join(&self.env).join("env.toml"))
            .map(|m| m.good(name))
            .unwrap_or_default()
    }

    /// What `env.toml` pins `name` to.
    fn pinned(&self, name: &str) -> Option<String> {
        ::env::decl::Manifest::load(&self.root.join("data").join(&self.env).join("env.toml"))
            .ok()?
            .pin(name)
    }

    /// Overwrite the artifact with `script`, build, and return the NEW label.
    fn build_script(&self, name: &str, script: &str) -> String {
        let (_, before) = self.cauldron(&["versions", name, "--json"]);
        std::fs::write(
            self.root.join("fake-daemon"),
            launch_wait::slow_launch_py(&first_exec::slow_first_exec_py(script, &self.root.join("first-exec-marks")), &self.root.join("f2g-marks")),
        )
        .unwrap();
        let (rc, out) = self.cauldron(&["build", name]);
        assert_eq!(rc, 0, "build {name}: {out}");
        let (rc, out) = self.cauldron(&["versions", name, "--json"]);
        assert_eq!(rc, 0, "versions {name}: {out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        let new: Vec<String> = v
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|b| b["label"].as_str().map(str::to_string))
            .filter(|l| !before.contains(l.as_str()))
            .collect();
        assert_eq!(new.len(), 1, "expected exactly one new build label: {new:?}\n{out}");
        self.pay_build(name, &new[0]);
        new[0].clone()
    }

    fn repins(&self, name: &str) -> Vec<serde_json::Value> {
        self.supervisions()
            .into_iter()
            .filter(|r| r["service"] == name && r["act"] == "repin")
            .collect()
    }
}

/// The daemon's own "came up and died before staying up" count for `name`.
fn unstable_deaths(log: &str, name: &str) -> usize {
    log.lines()
        .filter(|l| l.contains(&format!("`{name}` came up on")) && l.contains("died before staying up"))
        .count()
}

/// THE CLAIM AND ITS PAIR, in one environment, because the rollback needs a
/// proven target: a build held up past `stable_after_secs = 8` is recorded
/// good — AFTER that period, not at bind — and is not rolled back; a build
/// that binds and dies 2 s later is never recorded good, each of its deaths
/// counts, and after `max_start_failures = 3` the daemon re-pins to the good
/// one and runs it.
#[test]
fn r9c3_a_build_that_binds_then_dies_after_2s_inside_stable_after_8s_is_never_good_and_auto_rolls_back() {
    let mut sb = Sandbox::new("c3roll", &c3_policy());
    let port = sb.declare("svc-a", "");
    let good = sb.build_script("svc-a", FAKE);
    let (rc, out) = sb.cauldron(&["pin", "svc-a", &good]);
    assert_eq!(rc, 0, "{out}");
    sb.start_daemon();
    let g1 = wait_for("the good build up", first_up(), || sb.up("svc-a", port));
    let t_up = Instant::now();

    // ── PAIRED ARM, first half: BOUND IS NOT GOOD. Up and listening, but not
    // yet up 8 s, so `good` is still empty. (The old launch wrote it here.)
    // Held 3 s first — three polls at poll_secs = 1 — so a write on the first
    // LIVE pass is caught too, not only one on the launch path (forcing F3).
    std::thread::sleep(Duration::from_secs(3));
    assert!(t_up.elapsed() < Duration::from_secs(C3_STABLE - 3), "premise: checked well inside the stable period");
    assert!(sb.good("svc-a").is_empty(), "recorded good at bind, before stable_after_secs:\n{}", sb.log());

    // ── PAIRED ARM, second half: up past 8 s, the daemon records it good.
    let recorded = wait_for("the good build recorded good", BUDGET, || {
        (sb.good("svc-a").first() == Some(&good)).then_some(())
    });
    let _ = recorded;
    assert!(
        t_up.elapsed() >= Duration::from_secs(C3_STABLE - 2),
        "recorded good after only {:?} up — the stable period is {C3_STABLE}s:\n{}",
        t_up.elapsed(),
        sb.log()
    );
    assert_eq!(sb.up("svc-a", port), Some(g1), "the good build did not stay up:\n{}", sb.log());
    assert!(sb.repins("svc-a").is_empty(), "a stable build was rolled back:\n{}", sb.log());

    // ── THE CLAIM: the dying build. Pinned while the daemon holds; the daemon
    // restarts onto it, and it binds and dies every launch.
    let bad = sb.build_script("svc-a", &dying_fake(C3_DIES_AFTER));
    assert_ne!(bad, good);
    let (rc, out) = sb.cauldron(&["pin", "svc-a", &bad]);
    assert_eq!(rc, 0, "{out}");
    let rb = wait_for("an auto-rollback re-pin", Duration::from_secs(120), || {
        sb.repins("svc-a").into_iter().find(|r| r["why"] == "rollback")
    });
    let log = sb.log();
    assert_eq!(rb["was_version"], bad.as_str(), "{rb}");
    assert_eq!(rb["version"], good.as_str(), "{rb}");
    assert!(log.contains("AUTO-ROLLBACK: `svc-a`"), "{log}");
    // R9.C4 (R9.C3-2): the rollback fires after EXACTLY `max_start_failures = 3`
    // launches of the bad build — the re-pin restart is the first of them. It
    // was 4, because `restart_now` did not mark its launch unproven.
    assert_eq!(sb.launches_of("svc-a", &bad), 3, "rolled back after a launch count other than max_start_failures = 3:\n{log}");
    // Each counted death is a launch that bound and then died inside 8 s.
    assert!(unstable_deaths(&log, "svc-a") >= 3, "the deaths were not counted:\n{log}");
    // And the dying build was never written good, though it bound every time.
    assert!(!sb.good("svc-a").contains(&bad), "a build that died 2 s after binding was recorded good: {:?}\n{log}", sb.good("svc-a"));
    assert_eq!(sb.pinned("svc-a").as_deref(), Some(good.as_str()), "env.toml is not re-pinned to the good build");
    // The good build runs again.
    wait_for("the good build relaunched after the rollback", BUDGET, || sb.up("svc-a", port));
}

/// The same dying build with NOTHING proven behind it: no rollback target, so
/// after 3 unstable launches it PARKS and is not relaunched (outlasting 5
/// polls and the 2 s backoff ceiling), and `good` stays empty throughout.
#[test]
fn r9c3_a_build_that_binds_then_dies_after_2s_with_no_good_target_parks_inside_stable_after_8s() {
    let mut sb = Sandbox::new("c3park", &c3_policy());
    let port = sb.declare("svc-a", "");
    let bad = sb.build_script("svc-a", &dying_fake(C3_DIES_AFTER));
    let (rc, out) = sb.cauldron(&["pin", "svc-a", &bad]);
    assert_eq!(rc, 0, "{out}");
    sb.start_daemon();
    wait_for("the dying build up once", first_up(), || sb.up("svc-a", port));
    wait_for("the dying build PARKED", Duration::from_secs(90), || {
        sb.log().contains("PARKED `svc-a`").then_some(())
    });
    let launches_at_park = sb.launches("svc-a");
    std::thread::sleep(Duration::from_secs(5));
    let log = sb.log();
    assert!(launches_at_park >= 3, "parked after only {launches_at_park} launches:\n{log}");
    assert_eq!(sb.launches("svc-a"), launches_at_park, "relaunched after parking:\n{log}");
    assert!(!listening(port), "svc-a is up after parking:\n{log}");
    assert!(unstable_deaths(&log, "svc-a") >= 3, "{log}");
    assert!(sb.good("svc-a").is_empty(), "the dying build was recorded good: {:?}\n{log}", sb.good("svc-a"));
    assert!(sb.repins("svc-a").is_empty(), "rolled back with no good target:\n{log}");
}

// ═══════ R9.C4 (gap C-4): GOOD ON EVERY LAUNCH PATH, AFTER THE STABLE PERIOD ═══════
//
// C-4: "services the daemon launches by registration never get a good-build
// record" — the daemon's stable-period record (R9.C3) covered hosted rows only,
// so a registering service (tts) had no proven-good build at all and a
// rollback could only land on whatever dev build `cauldron start` had once
// recorded at bind. And `cauldron start` still recorded good at bind.
//
// The period outlasted (doctrine 4) is `stable_after_secs = 8` (C3_STABLE);
// the dying builds live C3_DIES_AFTER = 2 s.

/// The REGISTERING fake, but a different commit (so a different build label)
/// that registers, serves for `secs` seconds and then exits 3.
fn dying_registering_fake(secs: u64) -> String {
    REGISTERING_FAKE
        .replace("5eed5eed5eed5eed5eed5eed5eed5eed5eed5eed", "c4dec4dec4dec4dec4dec4dec4dec4dec4dec4de")
        .replace(
            "while True:\n    c, _ = s.accept()\n    threading.Thread(target=serve, args=(c,), daemon=True).start()\n",
            &format!(
                "deadline = time.time() + {secs}\ns.settimeout(0.2)\nwhile time.time() < deadline:\n    \
                 try:\n        c, _ = s.accept()\n    except socket.timeout:\n        continue\n    \
                 c.settimeout(None)\n    threading.Thread(target=serve, args=(c,), daemon=True).start()\n\
                 os._exit(3)\n"
            ),
        )
}

impl Sandbox {
    /// Declare `name` as a REGISTERING service whose artifact is `script`,
    /// build it, pin it, and return the new label.
    fn pin_registering(&self, name: &str, script: &str) -> String {
        let fake = self.root.join(format!("registering-fake-{name}"));
        std::fs::write(&fake, launch_wait::slow_launch_py(&first_exec::slow_first_exec_py(script, &self.root.join("first-exec-marks")), &self.root.join("f2g-marks"))).unwrap();
        Command::new("/bin/chmod").args(["+x"]).arg(&fake).status().unwrap();
        std::fs::write(
            self.root.join("services").join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\nkind = \"daemon\"\nregisters = true\nprobe = \"connect\"\n\
                 stop_signal = \"TERM\"\nstart_timeout_ms = {START_TIMEOUT_MS}\n\
                 [env]\nFAKE_PIDS = \"{}\"\n\
                 [build]\nartifact = \"{}\"\nbin = \"registering-fake-{name}\"\nstamp_args = [\"--build-stamp\"]\n",
                self.root.join("fake-pids").display(),
                fake.display()
            ),
        )
        .unwrap();
        let label = self.build(name);
        let (rc, out) = self.cauldron(&["pin", name, &label]);
        assert_eq!(rc, 0, "pin {name}: {out}");
        label
    }

    /// Launch records of `name` onto `version`.
    fn launches_of(&self, name: &str, version: &str) -> usize {
        self.supervisions()
            .iter()
            .filter(|r| r["service"] == name && r["act"] == "launch" && r["version"] == version)
            .count()
    }
}

/// THE CLAIM AND ITS PAIR: a REGISTERING service the daemon launched is
/// recorded good once it has stayed up past `stable_after_secs = 8` — and not
/// at registration — while a registering build that registers and dies 2 s
/// later is never recorded good (it parks: nothing proven behind it).
#[test]
fn r9c4_a_registering_service_the_daemon_launched_is_good_only_after_stable_after_8s_and_one_dying_after_2s_never() {
    let mut sb = Sandbox::new("c4reg", &c3_policy());
    let stable = sb.pin_registering("svc-r", REGISTERING_FAKE);
    let dying = sb.pin_registering("svc-d", &dying_registering_fake(C3_DIES_AFTER));
    assert_ne!(stable, dying, "premise: two different builds");
    sb.start_daemon();
    // A first wait (R9.F1): under load a fresh python fake was measured
    // taking over 10 s to register on its first exec, and svc-d's launches
    // are tended first. `pin_registering` pays both build copies' first
    // exec (`pay_build`), and the wait is the declared first-up period plus
    // the measured worst first exec; it was a fixed 90 s.
    wait_for("svc-r registered and up", first_up(), || sb.up_at_row("svc-r"));
    let t_up = Instant::now();

    // REGISTERED IS NOT GOOD. Held 3 polls (poll_secs = 1) so a write on the
    // first live pass is caught as well as one on the launch path.
    std::thread::sleep(Duration::from_secs(3));
    assert!(t_up.elapsed() < Duration::from_secs(C3_STABLE - 3), "premise: checked well inside the stable period");
    assert!(sb.good("svc-r").is_empty(), "recorded good at registration, before stable_after_secs:\n{}", sb.log());

    // THE CLAIM: up past 8 s, the daemon records the registering build good.
    wait_for("svc-r recorded good after the stable period", BUDGET, || {
        (sb.good("svc-r").first() == Some(&stable)).then_some(())
    });
    assert!(
        t_up.elapsed() >= Duration::from_secs(C3_STABLE - 2),
        "recorded good after only {:?} up — the stable period is {C3_STABLE}s:\n{}",
        t_up.elapsed(),
        sb.log()
    );

    // THE OPPOSING ARM: the dying registering build is never good.
    wait_for("svc-d PARKED", Duration::from_secs(120), || sb.log().contains("PARKED `svc-d`").then_some(()));
    let log = sb.log();
    assert!(unstable_deaths(&log, "svc-d") >= 3, "svc-d's deaths were not counted:\n{log}");
    assert!(sb.good("svc-d").is_empty(), "a registering build that died 2 s after registering was recorded good: {:?}\n{log}", sb.good("svc-d"));
}

/// `cauldron start` with NO holder: a build that binds and dies 2 s later is
/// not recorded good, because it has not been up `stable_after_secs = 8`
/// (the old `start` recorded it at bind). Paired with the same verb in an
/// environment whose `stable_after_secs = 0`, where the very same rule DOES
/// record at start — so the empty list above is the rule's answer, not a
/// `start` that never records or a `good()` reading the wrong file.
#[test]
fn r9c4_cauldron_start_does_not_record_a_build_that_binds_then_dies_after_2s_good_inside_stable_after_8s() {
    // THE CLAIM.
    let sb = Sandbox::new("c4start", &c3_policy());
    let port = sb.declare("svc-a", "");
    let bad = sb.build_script("svc-a", &dying_fake(C3_DIES_AFTER));
    let (rc, out) = sb.cauldron(&["pin", "svc-a", &bad]);
    assert_eq!(rc, 0, "{out}");
    let (rc, out) = sb.cauldron(&["start", "svc-a"]);
    assert_eq!(rc, 0, "start: {out}");
    assert!(out.contains("cauldron: started svc-a"), "premise: start launched it:\n{out}");
    assert!(sb.good("svc-a").is_empty(), "`cauldron start` recorded a build good at bind: {:?}\n{out}", sb.good("svc-a"));
    wait_for("the dying build gone", BUDGET, || (!listening(port)).then_some(()));
    assert!(sb.good("svc-a").is_empty(), "{:?}", sb.good("svc-a"));
    assert!(out.contains("not recorded GOOD yet"), "start did not say why nothing was recorded:\n{out}");

    // THE PAIR: stable_after_secs = 0 — start records at once, by the same rule.
    let sb0 = Sandbox::new(
        "c4start0",
        "auto_restart = true\nauto_rollback = false\npoll_secs = 1\ndown_after = 1\nstable_after_secs = 0",
    );
    sb0.declare("svc-a", "");
    let good = sb0.build_script("svc-a", FAKE);
    let (rc, out) = sb0.cauldron(&["pin", "svc-a", &good]);
    assert_eq!(rc, 0, "{out}");
    let (rc, out) = sb0.cauldron(&["start", "svc-a"]);
    assert_eq!(rc, 0, "start: {out}");
    assert_eq!(sb0.good("svc-a"), vec![good.clone()], "with stable_after_secs = 0 start did not record good:\n{out}");
}
