//! **THE INFORMATION-UPDATE CHANNEL** — the daemon half.
//!
//! > "The threads module, which was updated, is no longer doing live updates.
//! >  The only updates I'm getting are when I trigger a refresh... let's update
//! >  the surface of the threads WebSocket broadcast system and just subscribe
//! >  to it from the... text user interface, so that we're just seeing the live
//! >  information as it's happening. If I have it open, that means I want to see
//! >  live stuff happening in there as it's happening. **All information updates
//! >  should get relayed out as a broadcast and picked up by my text user
//! >  interface.**"
//!
//! The last sentence is the requirement; the first two are symptoms of not
//! having it. What made the symptom possible is worth stating plainly, because
//! it is not what it looks like: the client was ALREADY holding a WebSocket and
//! ALREADY subscribed to the firehose. It was not a missing subscription. The
//! daemon simply had nothing to say — every push it could emit was about a
//! THREAD (`event`, `liveness`, `status`) or about an operation the client had
//! itself started (`progress`). The entire ACCOUNT PICTURE — usage readings,
//! login health, pool shares — changed in silence.
//!
//! So these tests are about the daemon's SILENCE, and the shape of them follows
//! from that: each one makes the daemon change a fact and asserts that a
//! firehose subscriber HEARS about it. A test that only checked the frame's
//! serialization would have passed on the old code too, because the old code's
//! defect was never a malformed frame — it was no frame at all.
//!
//! **THE CHOKEPOINT IS THE DESIGN, AND `the_bell_is_the_chokepoint` IS THE TEST
//! THAT PROTECTS IT.** `Manager::account_picture_changed` already existed as the
//! daemon's internal "an input to placement moved" bell, with every cause
//! calling it; it only ever woke the retry sweep, inside the process. The
//! broadcast is hung THERE rather than on the nine call sites, so a tenth cause
//! is announced by calling the function it already has to call. That is the
//! difference between a channel that stays complete and one that decays into
//! "the few somebody remembered to wire".
//!
//! Nothing here runs a real claude turn, spawns a probe, or touches the live
//! daemon.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use threads::event::Push;
use threads::{build_manager, Config, ThreadManager};

// ── fixtures ─────────────────────────────────────────────────────────────

fn config_at(dir: &std::path::Path) -> Config {
    // Probing and calibration are OFF: these tests are about what the daemon
    // SAYS when a fact changes, and a background sweep that shells out to
    // `claude` would both slow them down and inject announcements nobody asked
    // for. Copied from `lifecycle_announcement.rs`'s fixture rather than shared
    // — integration tests are separate crates, and a `mod common` would couple
    // two files that are otherwise free to move independently.
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

fn manager(dir: &std::path::Path) -> ThreadManager {
    std::fs::create_dir_all(dir.join("accounts")).unwrap();
    build_manager(config_at(dir)).unwrap()
}

/// One `Info` frame as a firehose subscriber receives it.
#[derive(Debug, Clone, PartialEq)]
struct Info {
    domain: String,
    reason: String,
    account: Option<String>,
}

type Seen = Arc<Mutex<Vec<Info>>>;

/// Every information update that reaches a firehose subscriber.
///
/// Deliberately reads the BROADCAST, not the emitting function's return value.
/// The bug being fixed was a fact that never left the process, so a test that
/// observed anything short of the channel would not have been able to see it.
fn watch(m: &ThreadManager) -> Seen {
    let mut rx = m.subscribe();
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let out = seen.clone();
    tokio::spawn(async move {
        while let Ok(p) = rx.recv().await {
            if let Push::Info {
                domain,
                reason,
                account,
                ..
            } = p
            {
                out.lock().unwrap().push(Info {
                    domain,
                    reason,
                    account,
                });
            }
        }
    });
    seen
}

async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..200 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

// ── 1. the bell rings OUTWARD ────────────────────────────────────────────

/// **THE CHOKEPOINT.** `account_picture_changed` is the one function every
/// account-picture cause already called, and it now broadcasts.
///
/// This is the test that makes the design hold rather than merely describe
/// itself: if someone re-routes a cause around this function, or strips the
/// emit out of it, the announcement stops and this goes red. The alternative
/// design — an emit at each of the nine call sites — cannot be protected this
/// way, because the failure it invites is a TENTH site that was never added,
/// and no test can assert the absence of a call somebody did not write.
#[tokio::test]
async fn the_bell_is_the_chokepoint() {
    let dir = tempfile::tempdir().unwrap();
    let m = manager(dir.path());
    let seen = watch(&m);

    m.account_picture_changed("limit_recorded");

    assert!(
        eventually(|| !seen.lock().unwrap().is_empty()).await,
        "ringing the account-picture bell must reach firehose subscribers — it \
         used to wake only the retry sweep, INSIDE this process, which is \
         exactly why the board could not see any of it"
    );
    let got = seen.lock().unwrap().clone();
    assert_eq!(got[0].domain, "accounts");
    assert_eq!(
        got[0].reason, "limit_recorded",
        "the cause travels with the frame, so a client can say WHAT moved \
         rather than only that something did"
    );
}

/// Every reason the bell is rung under is carried through verbatim. The tags
/// are the daemon's own words for what happened at each call site, and a client
/// switches on them.
#[tokio::test]
async fn every_cause_names_itself() {
    let dir = tempfile::tempdir().unwrap();
    let m = manager(dir.path());
    let seen = watch(&m);

    // The nine causes wired at the call sites, in the daemon's own vocabulary.
    let causes = [
        "credit_exhausted",
        "login_expired",
        "limit_recorded",
        "limit_cleared",
        "login_restored",
        "login_dead",
        "login_revived",
        "pool_share",
        "deleted",
        "rotated",
    ];
    for c in causes {
        m.account_picture_changed(c);
    }

    assert!(
        eventually(|| seen.lock().unwrap().len() >= causes.len()).await,
        "every cause must produce its own frame; got {:?}",
        seen.lock().unwrap()
    );
    let got: Vec<String> = seen.lock().unwrap().iter().map(|i| i.reason.clone()).collect();
    for c in causes {
        assert!(got.contains(&c.to_string()), "cause {c} was not announced");
    }
}

// ── 2. the gauge, which is the one he was actually looking at ────────────

/// **A USAGE READING IS AN INFORMATION UPDATE.** This is the highest-frequency
/// one the daemon makes about the board — `rate_limit_event` arrives on every
/// turn — and until this change it was also the quietest, which is precisely
/// the combination the operator was staring at: numbers moving constantly in
/// the daemon and never once on his screen between refreshes.
///
/// Emitted on the `gauge` domain rather than `accounts` deliberately: a client
/// that only cares about readings should not have to re-read login health to
/// get them, and the reverse.
#[tokio::test]
async fn a_gauge_reading_is_announced_on_its_own_domain() {
    let dir = tempfile::tempdir().unwrap();
    let m = manager(dir.path());
    let seen = watch(&m);

    m.emit_info(
        "gauge",
        "usage_poll",
        Some("cc1"),
        Some("cc1: weekly_all is now 62%".into()),
    );

    assert!(eventually(|| !seen.lock().unwrap().is_empty()).await);
    let got = seen.lock().unwrap().clone();
    assert_eq!(got[0].domain, "gauge");
    assert_eq!(got[0].reason, "usage_poll");
    assert_eq!(
        got[0].account.as_deref(),
        Some("cc1"),
        "the login is named so a client can attribute the reading without \
         re-reading the whole board to find out which row moved"
    );
}

// ── 3. the line a consumer reads ─────────────────────────────────────────
//
// **RE-CUT AT R2.S4.** These two tests serialised the legacy
// `protocol::ServerMsg::Info` frame. That frame went with the firehose; the
// fact is carried by the `narration` shape's `info` channel, so they now read
// the LINE a narration subscriber is handed for a real `Push::Info`.

/// Every DISTINCT `info` line `narration` hands a subscriber of its `info`
/// channel, after `act` runs. Found by `kind`, wherever the frame nests it, so the test
/// does not depend on how a `Sequence` frames an append.
async fn info_lines_after(m: &ThreadManager, act: impl FnOnce(&ThreadManager)) -> Vec<serde_json::Value> {
    fn collect(v: &serde_json::Value, out: &mut Vec<serde_json::Value>) {
        match v {
            serde_json::Value::Object(o) => {
                if o.get("kind").and_then(|k| k.as_str()) == Some("info") {
                    out.push(v.clone());
                    return;
                }
                for x in o.values() {
                    collect(x, out);
                }
            }
            serde_json::Value::Array(a) => a.iter().for_each(|x| collect(x, out)),
            _ => {}
        }
    }
    threads::clock::testing::ticking();
    m.hub.register(threads::shapes::narration::Narration, m.clone());
    let mut rx = m.hub.connect(911);
    m.hub
        .subscribe("narration", Some(threads::shapes::narration::CHANNEL_INFO), 911, None)
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    while rx.try_recv().is_ok() {}
    act(m);
    tokio::time::sleep(Duration::from_millis(150)).await;
    // One frame can carry the same line twice (the live event and the row it
    // landed in), so each frame's lines are de-duplicated before they count.
    let mut out = Vec::new();
    while let Ok(f) = rx.try_recv() {
        let mut in_frame = Vec::new();
        collect(&serde_json::to_value(&f.wire).unwrap(), &mut in_frame);
        for l in in_frame {
            if !out.contains(&l) {
                out.push(l);
            }
        }
    }
    out
}

/// The line names the domain a client re-reads, and carries no value.
///
/// **IT CARRIES NO VALUE, AND THAT IS THE CONTRACT.** A payload here would be a
/// second copy of a query's schema, free to drift from the query it duplicates
/// — and it would give a client two sources for one fact with no rule for
/// which wins. The announcement is re-read instead, so a coalesced burst, a
/// dropped frame and a concurrent poll all converge on the same board.
///
/// Before R2.S4: the legacy frame's key set was exactly
/// `{account, at, detail, domain, reason, type}`. Now: the narration line's is
/// exactly `{account, at, detail, domain, kind, reason}` — `kind` is the line's
/// tag where `type` was the frame's, and nothing else was added.
///
/// **Forced red** (R2.S4) by adding `value: Option<u8>` to `narration::Line::Info`:
/// `left: ["account", "at", "detail", "domain", "kind", "reason", "value"]`.
#[tokio::test]
async fn the_info_line_announces_and_does_not_carry() {
    let dir = tempfile::tempdir().unwrap();
    let m = manager(dir.path());
    let lines = info_lines_after(&m, |m| {
        m.emit_info("gauge", "usage_poll", Some("cc1"), Some("cc1: weekly_all is now 62%".to_string()))
    })
    .await;
    assert_eq!(lines.len(), 1, "one push, one line: {lines:?}");
    let v = &lines[0];
    assert_eq!(v["domain"], "gauge");
    assert_eq!(v["reason"], "usage_poll");
    assert_eq!(v["account"], "cc1");

    let obj = v.as_object().unwrap();
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["account", "at", "detail", "domain", "kind", "reason"],
        "a field carrying a VALUE would make this channel a second, drifting \
         copy of the query it announces — see Push::Info"
    );
}

/// Optional fields that are absent are absent as VALUES — `null` on the line,
/// never an invented empty string a client would have to special-case.
///
/// Before R2.S4: the legacy frame OMITTED the keys. Now: the narration line
/// declares `account` and `detail` `Optional` (`shapes/narration.rs`), and an
/// absent one is `null`. The assertion moved with the carrier; the rule it
/// protects — no fabricated value — did not.
///
/// **Forced red** (R2.S4) by folding `detail` as `Some(detail.unwrap_or_default())`:
/// `absent detail must be null, not invented: {…"detail":""…}`.
#[tokio::test]
async fn the_optional_fields_are_null_when_absent() {
    let dir = tempfile::tempdir().unwrap();
    let m = manager(dir.path());
    let lines = info_lines_after(&m, |m| m.emit_info("accounts", "pool_share", None, None)).await;
    assert_eq!(lines.len(), 1, "one push, one line: {lines:?}");
    let v = &lines[0];
    assert!(v["account"].is_null(), "absent account must be null, not invented: {v}");
    assert!(v["detail"].is_null(), "absent detail must be null, not invented: {v}");
    assert_eq!(v["domain"], "accounts");
}

// ── 4. delivery rules ────────────────────────────────────────────────────

/// **NO THREAD OWNS IT.** The push carries no thread id, by the same rule as
/// `Progress` and `Lifecycle`: "the account picture moved" is a fact about the
/// POOL, which no thread owns and no per-thread subscription can ask for.
///
/// The server maps `Push::Info` to an EMPTY thread id and the empty string
/// matches no per-thread subscription, so this is enforced by construction
/// rather than by a filter someone has to remember. Pinned here so that a
/// future edit which starts stamping a thread id on these frames — and thereby
/// starts leaking pool facts into per-thread subscriptions — fails.
#[tokio::test]
async fn an_info_frame_belongs_to_no_thread() {
    let dir = tempfile::tempdir().unwrap();
    let m = manager(dir.path());
    let mut rx = m.subscribe();

    m.account_picture_changed("pool_share");

    let push = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("an info push must arrive")
        .expect("the channel must stay open");
    match push {
        Push::Info { domain, .. } => assert_eq!(domain, "accounts"),
        other => panic!("expected Push::Info, got {other:?}"),
    }
}

/// No subscriber is a normal state, not an error. The daemon spends most of its
/// life with nothing attached, and an emit that logged or failed there would
/// turn the quiet case into noise — the same policy `emit_progress` and
/// `emit_liveness` already follow.
#[tokio::test]
async fn announcing_into_an_empty_room_is_fine() {
    let dir = tempfile::tempdir().unwrap();
    let m = manager(dir.path());
    // No `subscribe()` at all.
    m.account_picture_changed("limit_recorded");
    m.emit_info("gauge", "usage_poll", Some("cc1"), None);
    // Reaching here without a panic is the assertion.
}
