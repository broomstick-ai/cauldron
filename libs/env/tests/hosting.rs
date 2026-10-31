//! **HOSTING — the supervisor writes a row for a child it forked but that
//! cannot write its own.**
//!
//! This file replaces `tests/adoption.rs`. The feature it tested is deleted;
//! three of its claims are not, because they were never really about adoption,
//! and they are carried over here against the mechanism that replaced it:
//!
//! 1. **`live` is not weakened.** A row with no boot token can never read
//!    `live`, however healthy its listener is and whatever its declaration
//!    asks for. The token is the identity proof; a row with none has nothing
//!    for an answer to match.
//! 2. **The warnings aggregate stays honest.** A hosted service is NAMED as an
//!    uncovered gap — never counted as covered on the strength of having been
//!    launched by cauldron, and never filed as a violation it did not commit.
//! 3. **A pid cauldron inferred is a pid cauldron may not kill.**
//!
//! And it adds the claim that is new, which is the point of the whole change:
//!
//! 4. **A hosted row IS signalable.** `adopt`'s whole content was that
//!    cauldron would not touch the process; hosting's whole content is that it
//!    will. A supervisor that may not signal what it started cannot restart
//!    anything, which is the job.
//!
//! Each test prints the refusal text, so the proof run records the actual
//! words rather than an assertion that some error occurred.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use env::seams::{self, Identity, PidSource, Seam, SeamState};
use env::warnings::{self, Aggregate, Compliance, Export};

/// The commit the REGISTERED fixtures name on both sides. A fixture pair that
/// disagreed would fail every `live` assertion for a reason none of them is
/// about.
const COMMIT: &str = "4197fc6e1d2a3b4c5d6e7f8091a2b3c4d5e6f701";

// ───────────────────────────────────────────────────────────────── fixtures

/// A REGISTERED row: what `ServiceEnv::register` writes.
fn registered(addr: &str, token: &str, probe: &str) -> Seam {
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
        registered: true,
        pid_source: PidSource::SELF.into(),
        health: String::new(),
    }
}

/// A HOSTED row: what `record_hosted` writes. Note the EMPTY boot token — it is
/// not an omission, it is the recorded fact that this binary cannot echo one.
fn hosted(addr: &str, probe: &str) -> Seam {
    Seam {
        service: "threads".into(),
        addr: addr.into(),
        pid: 999_999,
        boot_token: String::new(),
        version: "unstamped-abc123".into(),
        // EMPTY, like the boot token beside it: an image that cannot state its
        // own commit gets an empty one rather than one guessed from a log.
        // `probe` never compares an empty commit in either direction.
        commit: String::new(),
        dirty: false,
        binary: "/nowhere/threads".into(),
        env: "dev".into(),
        data_root: "/tmp/fake/data/dev".into(),
        data_dir: "/tmp/fake/data/dev/threads".into(),
        started_at: 0,
        probe: probe.into(),
        registered: false,
        pid_source: PidSource::CHILD.into(),
        health: String::new(),
    }
}

/// A listener that answers one line to whatever it is asked.
fn answering(reply: String) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    env::egress::grant_addr(l.local_addr().unwrap()).unwrap(); // REL-6a6: given
    let addr = l.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        while let Ok((s, _)) = l.accept() {
            let mut w = s.try_clone().unwrap();
            let mut line = String::new();
            let _ = BufReader::new(s).read_line(&mut line);
            let mut body = reply.clone();
            body.push('\n');
            let _ = w.write_all(body.as_bytes());
            let _ = w.flush();
        }
    });
    addr
}

/// A listener that accepts and says nothing — a bind check passes, a handshake
/// cannot.
fn silent() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    env::egress::grant_addr(l.local_addr().unwrap()).unwrap(); // REL-6a6: given
    let addr = l.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        while let Ok((s, _)) = l.accept() {
            held.push(s);
        }
    });
    addr
}

fn export(producer: &str, shape: &str, warn: u64, error: u64) -> Export {
    Export {
        shape: shape.into(),
        shape_version: warnings::SHAPE_VERSION,
        producer: producer.into(),
        counts_since: "1700000000".into(),
        warn,
        error,
        sink_errors: 0,
        sink_attached: true,
        kinds: Vec::new(),
    }
}

fn say(what: &str, e: anyhow::Error) -> String {
    let t = format!("{e:#}");
    println!("  REFUSED [{what}]: {t}");
    t
}

// ══════════════════════════════════════ 1. `live` IS NOT WEAKENED ═══════════

/// **THE HEADLINE.** A row with no boot token never reads `live`.
///
/// Three listeners, all alive and answering, and the only variable is the
/// token. This is the test that goes red if somebody "fixes" hosting by
/// relaxing the handshake instead of naming the weaker verdict.
#[test]
fn a_row_with_no_boot_token_never_reads_live() {
    // (a) A hosted row over a silent listener: bind check only.
    let s = hosted(&silent(), "connect");
    let v = seams::probe(&s);
    println!("hosted/connect/silent -> {}", v.state.as_str());
    assert_eq!(v.state, SeamState::LiveUnverified);

    // (b) A hosted row whose declaration asked for the STRONG probe anyway.
    // `record_hosted` refuses that combination, so this fixture is what a
    // hand-edited file would look like — and the ceiling still holds.
    let s = hosted(&silent(), "seam");
    let v = seams::probe(&s);
    println!("hosted/seam/silent -> {}", v.state.as_str());
    assert_eq!(
        v.state,
        SeamState::LiveUnverified,
        "a hand-edited `probe = \"seam\"` on a row with no token must not reach `live`"
    );
    assert!(
        v.detail.contains("NO BOOT TOKEN"),
        "the verdict has to say WHY it is not live: {}",
        v.detail
    );

    // (c) A hosted row over a listener that answers a perfectly well-formed
    // identity carrying an EMPTY token. Both sides empty, so a naive `==`
    // would compare equal and a row that proved nothing would read `live`.
    let id = Identity {
        service: "threads".into(),
        env: "dev".into(),
        data_root: "/tmp/fake/data/dev".into(),
        boot_token: String::new(),
        version: "unstamped-abc123".into(),
        commit: String::new(),
        pid: 999_999,
        addr: "127.0.0.1:1".into(),
    };
    let addr = answering(serde_json::to_string(&id).unwrap());
    let mut s = hosted(&addr, "seam");
    s.addr = addr.clone();
    let v = seams::probe(&s);
    println!("hosted/seam/empty-token-answer -> {}", v.state.as_str());
    assert_eq!(
        v.state,
        SeamState::LiveUnverified,
        "empty == empty must never be a match"
    );
}

/// The other direction, so the test above is not passing vacuously: a
/// registered row over a listener that answers the right token DOES read live.
#[test]
fn live_is_still_reachable_for_a_registered_row() {
    let token = "tok-abc";
    let id = Identity {
        service: "echo".into(),
        env: "dev".into(),
        data_root: "/tmp/fake/data/dev".into(),
        boot_token: token.into(),
        version: "v1".into(),
        commit: COMMIT.into(),
        pid: std::process::id(),
        addr: "127.0.0.1:1".into(),
    };
    let addr = answering(serde_json::to_string(&id).unwrap());
    let s = registered(&addr, token, "seam");
    let v = seams::probe(&s);
    println!("registered/seam/matching-token -> {}", v.state.as_str());
    assert_eq!(v.state, SeamState::Live, "{}", v.detail);
}

// ═══════════════════════ 2. THE AGGREGATE NAMES ITS HOLE ═══════════════════

/// A hosted service is a NAMED GAP: in the denominator, never the numerator,
/// and not a violation.
#[test]
fn a_hosted_service_is_a_named_gap_and_not_a_violation() {
    let s = hosted(&silent(), "connect");
    let v = seams::probe(&s);
    let c = warnings::check(&s, &v, Some(warnings::SHAPE));
    assert!(matches!(c, Compliance::Unregistered { .. }), "{c:?}");
    assert!(!c.is_violation(), "a process that never signed the contract is not in breach");
    assert!(c.is_unregistered_gap());
    let missing = c.missing().expect("a gap always names itself");
    println!("  GAP: {missing}");
    assert!(missing.contains("HOSTED"));
    assert!(
        missing.contains("register"),
        "the sentence must say what closing it looks like"
    );

    let mut agg = Aggregate::default();
    agg.add("threads", warnings::SHAPE, &c);
    assert_eq!(agg.unregistered, 1);
    assert_eq!(agg.violations, 0, "exit 4 must keep meaning `a breach`");
    assert_eq!(agg.coverage(), (0, 1), "denominator, never numerator");
    assert!(!agg.is_complete());
    let why = agg.incompleteness().expect("an incomplete total says so");
    println!("  INCOMPLETENESS: {why}");
    assert!(why.contains("HOSTED"));
}

/// **NOTHING IS PUT ON A HOSTED SOCKET.** The old `warnings_askable = true`
/// path would ASK an unregistered process and credit a good answer; it is
/// deleted along with the knob, and this test is what says so.
///
/// The listener here answers a PERFECT export. If `check` asked, it would be
/// credited — so a pass here means the ask was never made.
#[test]
fn a_hosted_service_is_never_contacted_even_if_it_would_answer() {
    let good = export("threads", warnings::SHAPE, 7, 3);
    let addr = answering(serde_json::to_string(&good).unwrap());
    let mut s = hosted(&addr, "connect");
    s.addr = addr;
    let v = seams::probe(&s);
    let c = warnings::check(&s, &v, Some(warnings::SHAPE));
    assert!(
        matches!(c, Compliance::Unregistered { .. }),
        "an unregistered row is a gap by construction, not by asking: {c:?}"
    );
    assert!(
        c.export().is_none(),
        "counts were credited to a process the supervisor must not have spoken to"
    );
    let mut agg = Aggregate::default();
    agg.add("threads", warnings::SHAPE, &c);
    assert_eq!(agg.warn, 0, "the 7 warns on that socket must not be in the total");
    assert_eq!(agg.error, 0);
    println!("  a perfect export on an unregistered row contributed 0/0, as it must");
}

/// The three gap kinds stay distinguishable: a violation, an absence and a
/// hosted hole are three different sentences with three different consequences.
#[test]
fn the_three_gap_kinds_are_distinguishable() {
    let mut agg = Aggregate::default();

    // A violation: registered, running, will not answer.
    let dead = registered("127.0.0.1:1", "tok", "connect");
    agg.add(
        "echo",
        warnings::SHAPE,
        &Compliance::NotServed { shape: warnings::SHAPE.into(), detail: "refused".into() },
    );
    let _ = dead;

    // An absence: nothing running to ask.
    agg.add("tally", warnings::SHAPE, &Compliance::NoProducer { state: "down".into() });

    // A hosted hole.
    agg.add(
        "threads",
        warnings::SHAPE,
        &Compliance::Unregistered { service: "threads".into(), shape: warnings::SHAPE.into() },
    );

    assert_eq!(agg.violations, 1);
    assert_eq!(agg.unregistered, 1);
    assert_eq!(agg.gaps.len(), 3);
    let kinds: Vec<&str> = agg.gaps.iter().map(|g| g.kind.as_ref()).collect();
    println!("  gap kinds: {kinds:?}");
    assert!(kinds.contains(&"violation"));
    assert!(kinds.contains(&"no-producer"));
    assert!(kinds.contains(&"unregistered"));
}

// ═════════════════ 3+4. WHAT MAY BE SIGNALLED, AND WHAT MAY NOT ════════════

/// **THE CHANGE, STATED AS A TEST.** A hosted row IS signalable.
///
/// Under `adopt` this was the headline refusal: cauldron would not signal the
/// live daemon however healthy it probed, because stopping it was another
/// supervisor's job. There is no other supervisor. Cauldron forks it, so the
/// pid is `child`, so it may be signalled — and a supervisor that cannot
/// signal its own child cannot restart anything.
#[test]
fn a_hosted_row_is_signalable_because_the_pid_came_from_the_fork() {
    let s = hosted("127.0.0.1:1", "connect");
    assert_eq!(s.pid_source, PidSource::CHILD);
    assert!(
        s.signalable(),
        "a child's pid is proof in exactly the way a self-reported one is"
    );
    assert!(s.refusal_to_signal().is_none());
    println!("  hosted row with pid_source=child: signalable, no refusal");
}

/// And a registered row still is, so the test above is not passing because
/// everything is signalable.
#[test]
fn a_registered_row_is_signalable() {
    let s = registered("127.0.0.1:1", "tok", "seam");
    assert!(s.signalable());
}

/// **AN INFERRED PID IS STILL NEVER SIGNALABLE.** The rule that survived
/// adoption's deletion, and the one that matters: the row below is what a
/// pre-hosting `seams.json` on this machine actually contains, or what a hand
/// edit would produce.
#[test]
fn an_inferred_pid_is_never_signalable() {
    for source in ["socket-owner", "pgrep-exact", "declared", "unknown", ""] {
        let mut s = hosted("127.0.0.1:1", "connect");
        s.pid_source = source.into();
        assert!(
            !s.signalable(),
            "`{source}` is not proof and must not reach a signal"
        );
        let why = s.refusal_to_signal().expect("not signalable means there is a reason");
        println!("  REFUSED [pid_source={source:?}]: {why}");
        assert!(why.contains("INFERRED") || why.contains("inferred"));
    }
    // And the live production row is exactly that case, by construction: it
    // was written by `adopt` with `pid_source = "socket-owner"`.
    println!(
        "  -> the two rows in ~/.cauldron/data/prod/seams.json today carry \
         pid_source=socket-owner, so nothing in this build would signal them"
    );
}

// ══════════════════════ 5. WHAT `record_hosted` REFUSES ════════════════════

#[test]
fn record_hosted_refuses_everything_it_cannot_establish_honestly() {
    let tmp = std::env::temp_dir().join(format!("cauldron-hosting-{}", std::process::id()));
    let data_root = tmp.join("data").join("dev");
    std::fs::create_dir_all(data_root.join("threads")).unwrap();
    let seams_path = data_root.join("seams.json");
    let live = silent();

    let base = |probe: &str, pid: u32, addr: &str| seams::Hosted {
        service: "threads".into(),
        addr: addr.into(),
        probe: probe.into(),
        pid,
        version: "v".into(),
        commit: String::new(),
        dirty: false,
        binary: "/nowhere".into(),
        data_dir: data_root.join("threads"),
        health: String::new(),
    };

    // (a) the strong probe, which it could never satisfy
    let t = say(
        "probe=seam",
        seams::record_hosted("dev", &data_root, &seams_path, &base("seam", 1, &live), "")
            .unwrap_err(),
    );
    assert!(t.contains("does not link"));

    // (b) a probe mode that is not one
    let t = say(
        "probe=magic",
        seams::record_hosted("dev", &data_root, &seams_path, &base("magic", 1, &live), "")
            .unwrap_err(),
    );
    assert!(t.contains("not a probe mode"));

    // (c) pid 0 — the row's whole claim is that the supervisor knows the pid
    let t = say(
        "pid=0",
        seams::record_hosted("dev", &data_root, &seams_path, &base("connect", 0, &live), "")
            .unwrap_err(),
    );
    assert!(t.contains("pid 0"));

    // (d) a dead address: a row is a statement a consumer can reach the
    // service, and writing one before the child has bound is a lie for as long
    // as the boot takes
    let t = say(
        "dead address",
        seams::record_hosted(
            "dev",
            &data_root,
            &seams_path,
            &base("connect", 1, "127.0.0.1:1"),
            "",
        )
        .unwrap_err(),
    );
    assert!(t.contains("nothing is listening"));

    // (e) and the happy path WRITES a row that is `registered = false`
    let w = seams::record_hosted("dev", &data_root, &seams_path, &base("connect", 4242, &live), "")
        .unwrap();
    assert!(!w.registered, "a row this function wrote is never `registered`");
    assert_eq!(w.pid_source, PidSource::CHILD);
    assert!(w.boot_token.is_empty());
    println!("  WROTE: {} at {} registered={}", w.service, w.addr, w.registered);
    let _ = std::fs::remove_dir_all(&tmp);
}

// ── R9.C7h: THE HOLDER READS `migrating` AS ALIVE, NOT READY ──────────────

/// **(d) THE HOLDER DOES NOT RELAUNCH A SERVICE THAT SAYS `migrating`.** The
/// tend loop relaunches a row whose verdict is `Unhealthy` and leaves a
/// `live`/`live:unverified` one alone (`cli/src/daemon.rs`, `tend`). So the
/// verdict is the decision, and this pins it on a real socket:
/// * `503 {"status":"migrating"}` → `live:unverified` (not relaunched);
/// * opposed: `503 {"status":"recovery_stuck"}` — the same service once its
///   stuck bound has fired — → `unhealthy` (relaunched), and so is a bare
///   503. Forced red: `HealthAnswer::alive_not_ready` returning `false`.
#[test]
fn the_holder_reads_503_migrating_as_alive_not_ready_and_recovery_stuck_as_unhealthy() {
    let row = |body: &str, status_line: &str| {
        let addr = answering(format!("{status_line}\r\ncontent-type: application/json\r\n\r\n{body}"));
        let mut s = hosted(&addr, "http");
        s.health = format!("http://{addr}/health");
        s
    };
    let migrating = row(r#"{"status":"migrating","migration":{"state":"migrating","step":"queue two records"}}"#, "HTTP/1.1 503 Service Unavailable");
    let v = seams::probe(&migrating);
    assert_eq!(v.state, SeamState::LiveUnverified, "{}", v.detail);
    assert!(v.detail.contains("ALIVE, NOT READY"), "{}", v.detail);

    let stuck = row(r#"{"status":"recovery_stuck","migration":{"state":"stuck"}}"#, "HTTP/1.1 503 Service Unavailable");
    let v = seams::probe(&stuck);
    assert_eq!(v.state, SeamState::Unhealthy, "{}", v.detail);
    assert!(v.detail.contains("recovery_stuck"), "{}", v.detail);

    let bare = row("", "HTTP/1.1 503 Service Unavailable");
    assert_eq!(seams::probe(&bare).state, SeamState::Unhealthy);

    let ok = row(r#"{"status":"ok"}"#, "HTTP/1.1 200 OK");
    assert_eq!(seams::probe(&ok).state, SeamState::LiveUnverified);
}
