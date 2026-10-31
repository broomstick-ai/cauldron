//! **THE CONSUMER LEDGER — the file half, proved against a real file.**
//!
//! The defect, from 2026-09-22: two board processes were running against this
//! daemon, one of them three days old and spinning at 98% CPU on superseded
//! code, and nothing anywhere could have told the operator that. `shapes.list`
//! counted two subscribers without saying who either of them was.
//!
//! These tests cover the FILE. The registry half — the thing `shapes.list`
//! actually answers from — is unit-tested in `src/consumers.rs` and proved
//! end-to-end over a real socket in `tests/shapes.rs`.

use serde_json::Value;
use threads::consumers::{CloseReason, ConsumerIdentity, ConsumerLedger};

fn id(name: &str, build: &str) -> ConsumerIdentity {
    ConsumerIdentity {
        name: name.into(),
        shape_version: "2026-09-20".into(),
        pid: Some(4411),
        build: Some(build.into()),
    }
}

/// Every record in the file, in order, with unparseable lines kept as `Null` so
/// a test can notice one rather than skip past it.
fn records(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or(Value::Null))
        .collect()
}

fn of_kind<'a>(rs: &'a [Value], kind: &str) -> Vec<&'a Value> {
    rs.iter().filter(|r| r["event"] == kind).collect()
}

/// **REPLAY THE PAIRING INVARIANT.** Which `(boot, conn, shape)` triples opened
/// and never closed — the question a reader of this file actually asks, and the
/// one that rots if a disconnect goes missing.
fn still_open(rs: &[Value]) -> Vec<(String, u64, String)> {
    let mut open: Vec<(String, u64, String)> = Vec::new();
    for r in rs {
        let (Some(b), Some(c), Some(s)) = (
            r["boot"].as_str(),
            r["conn"].as_u64(),
            r["shape"].as_str(),
        ) else {
            continue;
        };
        let k = (b.to_string(), c, s.to_string());
        match r["event"].as_str() {
            Some("connect") => open.push(k),
            Some("disconnect") => open.retain(|e| *e != k),
            _ => {}
        }
    }
    open
}

/// **A SUBSCRIBE RECORDS WHO ASKED, AND AN UNSUBSCRIBE CLOSES IT.**
///
/// The four identity fields all reach the line, because each was argued for
/// individually: `name` and `shape_version` because the operator asked for
/// them, `pid` because knowing a stale consumer exists and being able to end it
/// are different things, and `build` because on 2026-09-22 both processes would
/// have declared the SAME `shape_version` and differed only by three days of
/// code.
///
/// **THE MUTATION.** Drop `"consumer": identity` from the `connect` record in
/// `ConsumerLedger::connected`, or stop calling `disconnected` from
/// `Hub::unsubscribe`, and this goes red.
#[test]
fn a_subscribe_records_who_asked_and_an_unsubscribe_closes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("chassis").join("threads.producer.jsonl");
    let l = ConsumerLedger::new();
    l.attach(path.clone(), "threads", "boot-a", "0.1.0");

    assert!(
        !path.exists(),
        "LAZY: a chassis nobody subscribes to writes nothing at all — not even \
         a directory"
    );

    l.connected("accounts", Some("cc1"), 7, Some(id("threads-tui", "a1b2c3d")));
    l.flush();

    let rs = records(&path);
    assert_eq!(
        rs[0]["event"], "chassis_start",
        "the generation stamps itself before anything else"
    );
    assert_eq!(rs[0]["chassis"], "threads");
    assert_eq!(rs[0]["boot"], "boot-a");
    assert_eq!(rs[0]["version"], "0.1.0");

    let c = of_kind(&rs, "connect");
    assert_eq!(c.len(), 1);
    assert_eq!(c[0]["shape"], "accounts");
    assert_eq!(c[0]["key"], "cc1");
    assert_eq!(c[0]["conn"], 7);
    assert_eq!(c[0]["consumer"]["name"], "threads-tui");
    assert_eq!(c[0]["consumer"]["shape_version"], "2026-09-20");
    assert_eq!(
        c[0]["consumer"]["pid"], 4411,
        "the field that makes the finding actionable"
    );
    assert_eq!(
        c[0]["consumer"]["build"], "a1b2c3d",
        "the field the motivating incident actually needed: two processes \
         claiming one contract version, three days of code apart"
    );
    assert!(c[0]["at"].is_string());

    // AND THE CLOSE.
    l.disconnected("accounts", 7, CloseReason::Unsubscribed);
    l.flush();
    let rs = records(&path);
    let d = of_kind(&rs, "disconnect");
    assert_eq!(d.len(), 1);
    assert_eq!(d[0]["reason"], "unsubscribed");
    assert_eq!(d[0]["conn"], 7);
    assert!(
        still_open(&rs).is_empty(),
        "the pairing invariant: nothing is left open"
    );
    assert!(l.consumers("accounts").is_empty());
}

/// **THE CONNECTION ENDING CLOSES EVERY SHAPE IT HELD** — the transport is what
/// closes an entry, and one socket can hold several subscriptions.
///
/// Operator ruling: *"the chassis should handle this. It's built on a websocket
/// daemon so we can detect disconnects & reconnects."* There is no reaper and
/// no TTL anywhere in this module, deliberately; `Hub::dropped` is reached from
/// `drive`'s `ConnectionGuard::drop`, which runs on a clean close, a read
/// error, a failed write, a PANIC and an ABORT alike.
///
/// **THE MUTATION.** Make `ConsumerLedger::dropped` close only the first shape
/// it finds and the second assertion goes red.
#[test]
fn a_connection_that_ends_closes_every_entry_it_held() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("threads.producer.jsonl");
    let l = ConsumerLedger::new();
    l.attach(path.clone(), "threads", "boot-a", "0.1.0");
    l.connected("accounts", None, 7, Some(id("threads-tui", "a1b2c3d")));
    l.connected("threads", None, 7, Some(id("threads-tui", "a1b2c3d")));
    // A second connection that must NOT be touched by the first one's ending.
    l.connected("accounts", None, 8, Some(id("aui", "beefbee")));

    l.dropped(7);
    l.flush();

    assert_eq!(
        l.consumers("accounts").len(),
        1,
        "conn 8 is still there — a connection ending closes ITS entries"
    );
    assert_eq!(l.consumers("accounts")[0].conn, 8);
    assert!(l.consumers("threads").is_empty());

    let rs = records(&path);
    let closed: Vec<&Value> = of_kind(&rs, "disconnect");
    assert_eq!(closed.len(), 2, "BOTH shapes conn 7 held are closed");
    assert!(closed.iter().all(|d| d["conn"] == 7
        && d["reason"] == "connection_closed"));
    assert_eq!(
        still_open(&rs),
        vec![("boot-a".to_string(), 8u64, "accounts".to_string())],
        "and exactly the live one is still open"
    );
}

/// **A CHASSIS THAT DIED RUNNING NO CODE DOES NOT LEAVE AN ENTRY OPEN
/// FOREVER.**
///
/// This is the one case the transport cannot cover, and it is not exotic: a
/// `SIGKILL`, a runtime panic, or the self-update `execv` — which this daemon
/// does on purpose and which has fired on 12 of the last 13 updates — all end
/// the process with subscriptions open and no `disconnect` line for any of
/// them. Left alone, a reader pairing connects with disconnects would report a
/// consumer that has not existed for days.
///
/// So the NEXT generation sweeps: at its first write it replays what is on
/// disk, finds every triple that opened and never closed, and closes each one
/// explicitly against the boot id that opened it. A reader needs no rule about
/// generations to get the right answer.
///
/// **THE MUTATION.** Delete the `self.sweep()` call in `LedgerFile::append` and
/// the final assertion goes red with a dead consumer still open.
#[test]
fn an_entry_left_open_by_a_chassis_that_died_is_closed_by_the_next_one() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("threads.producer.jsonl");

    // ── GENERATION A subscribes a consumer and then dies where it stands.
    {
        let a = ConsumerLedger::new();
        a.attach(path.clone(), "threads", "boot-a", "0.1.0");
        a.connected("accounts", Some("cc1"), 7, Some(id("threads-tui", "old")));
        a.flush();
    }
    let rs = records(&path);
    assert_eq!(
        still_open(&rs),
        vec![("boot-a".to_string(), 7u64, "accounts".to_string())],
        "fixture: the dead generation really did leave it open"
    );

    // ── GENERATION B comes up and its first event triggers the sweep.
    let b = ConsumerLedger::new();
    b.attach(path.clone(), "threads", "boot-b", "0.1.0");
    b.connected("accounts", Some("cc1"), 1, Some(id("threads-tui", "new")));
    b.flush();

    let rs = records(&path);
    let swept: Vec<&Value> = of_kind(&rs, "disconnect")
        .into_iter()
        .filter(|d| d["reason"] == "chassis_restart")
        .collect();
    assert_eq!(swept.len(), 1, "the orphan is closed, explicitly");
    assert_eq!(swept[0]["boot"], "boot-a", "against the boot that opened it");
    assert_eq!(
        swept[0]["closed_by"], "boot-b",
        "and it says who closed it, so the line is not mistaken for the dead \
         chassis having done something it could not do"
    );
    assert_eq!(swept[0]["conn"], 7);

    assert_eq!(
        still_open(&rs),
        vec![("boot-b".to_string(), 1u64, "accounts".to_string())],
        "THE WHOLE POINT: after the sweep the only open entry is the live one"
    );
}

/// **A RE-SUBSCRIBE FIT DOES NOT BECOME TEN THOUSAND LINES.**
///
/// > *"note that this ledger can utilize some compression mechanisms, like
/// > count of the same events, so we don't turn a re-subscribe fit into 10,000
/// > lines in a log."*
///
/// This is load-bearing rather than tidy, because of what the chassis contract
/// settled: **there is no re-emit and no replay.** A consumer that loses its
/// state recovers by re-subscribing, so a consumer stuck in a bad state
/// re-subscribes in a loop BY CONSTRUCTION. It is the expected failure mode.
///
/// Both spellings of the cycle are exercised, because they take different paths
/// through the compressor: a bare repeat `shapes.subscribe` (one event by
/// `docs/shapes.md` §2.5), and an `unsubscribe` followed by a `subscribe`
/// (two verbs, one cycle, collapsed by the one-event lookahead). Naively
/// appended, either would ALTERNATE and never compress at all.
///
/// **THE MUTATION.** Make `LedgerFile::append` write every record instead of
/// folding a run, or make `connected` emit a `disconnect`/`connect` pair for a
/// repeat subscribe, and the line counts explode.
#[test]
fn a_resubscribe_fit_does_not_become_ten_thousand_lines() {
    const FIT: usize = 3_000;

    // ── SPELLING ONE: the same connection re-subscribing over and over.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("threads.producer.jsonl");
    let l = ConsumerLedger::new();
    l.attach(path.clone(), "threads", "boot-a", "0.1.0");
    for _ in 0..FIT {
        l.connected("accounts", Some("cc1"), 7, Some(id("threads-tui", "a1b2c3d")));
    }
    l.flush();

    let rs = records(&path);
    assert!(
        rs.len() < 20,
        "{FIT} cycles must not be {FIT} lines; got {}",
        rs.len()
    );
    assert_eq!(
        of_kind(&rs, "connect").len(),
        1,
        "ONE subscription throughout — `docs/shapes.md` §2.5, and it is also \
         what makes the rest compressible"
    );
    let repeats: u64 = of_kind(&rs, "repeat")
        .iter()
        .map(|r| r["repeats"].as_u64().unwrap())
        .sum();
    assert_eq!(
        repeats,
        FIT as u64 - 2,
        "NOTHING IS LOST — the count is exact: one `connect`, one \
         `resubscribe`, and every further cycle counted"
    );
    assert_eq!(of_kind(&rs, "resubscribe").len(), 1);
    for r in of_kind(&rs, "repeat") {
        assert_eq!(r["of"]["event"], "resubscribe", "it says what it repeats");
        assert!(r["first_repeat_at"].is_string());
        assert!(r["last_repeat_at"].is_string(), "and over what span");
    }
    assert_eq!(
        l.consumers("accounts").len(),
        1,
        "and the registry never stopped answering with one consumer"
    );

    // ── SPELLING TWO: unsubscribe, then subscribe, over and over. The
    //    one-event lookahead is what keeps this from alternating.
    let tmp2 = tempfile::tempdir().unwrap();
    let path2 = tmp2.path().join("threads.producer.jsonl");
    let l2 = ConsumerLedger::new();
    l2.attach(path2.clone(), "threads", "boot-a", "0.1.0");
    l2.connected("accounts", Some("cc1"), 7, Some(id("threads-tui", "a1b2c3d")));
    for _ in 0..FIT {
        l2.disconnected("accounts", 7, CloseReason::Unsubscribed);
        l2.connected("accounts", Some("cc1"), 7, Some(id("threads-tui", "a1b2c3d")));
    }
    l2.flush();
    let rs2 = records(&path2);
    assert!(
        rs2.len() < 20,
        "an unsubscribe/subscribe fit is the SAME cycle wearing two verbs; got {}",
        rs2.len()
    );
    assert!(
        of_kind(&rs2, "disconnect").is_empty(),
        "the subscription never actually ended, so nothing claims it did"
    );
}

/// **`since` IS THE SUBSCRIPTION'S OWN AGE AND SURVIVES EVERY RECOVERY CYCLE.**
///
/// It is where the operator's *"three days old"* is read from — the single
/// number that identifies a stale consumer. Resetting it on every re-subscribe
/// would erase it on precisely the consumer most likely to be cycling, which is
/// the one he was looking for.
///
/// **This was a real bug**, found by re-reading the code against this file's
/// own claim: the first cut carried no `since` on the deferred close, so the
/// unsubscribe-then-subscribe cycle fell through to `now`. The repeat-subscribe
/// cycle preserved it and the other did not, which is exactly the kind of
/// disagreement nobody notices.
///
/// **THE MUTATION.** Drop `since` from `PendingClose` and take `now` in the
/// `(None, true)` arm, and the second block goes red.
#[test]
fn the_subscriptions_age_survives_both_spellings_of_a_recovery_cycle() {
    let l = ConsumerLedger::new();

    // Spelling one: a repeat subscribe.
    l.connected("accounts", None, 7, Some(id("threads-tui", "a1b2c3d")));
    let opened = l.consumers("accounts")[0].since.clone();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    l.connected("accounts", None, 7, Some(id("threads-tui", "a1b2c3d")));
    assert_eq!(
        l.consumers("accounts")[0].since, opened,
        "a re-subscribe is the same subscription; it did not just start"
    );

    // Spelling two: unsubscribe, then subscribe.
    l.disconnected("accounts", 7, CloseReason::Unsubscribed);
    l.connected("accounts", None, 7, Some(id("threads-tui", "a1b2c3d")));
    assert_eq!(
        l.consumers("accounts")[0].since, opened,
        "THE SAME CYCLE WEARING TWO VERBS. The two spellings must not disagree \
         about how old the subscription is"
    );

    // …and a genuinely NEW subscription does get `now`, or the field would be
    // meaningless in the other direction.
    l.connected("accounts", None, 8, Some(id("threads-tui", "a1b2c3d")));
    let other = l
        .consumers("accounts")
        .into_iter()
        .find(|c| c.conn == 8)
        .unwrap();
    assert!(
        other.since > opened,
        "a new connection is a new subscription: {} vs {opened}",
        other.since
    );
}

/// **A DIFFERENT CONSUMER ON THE SAME SOCKET IS NOT MERGED INTO A RESUBSCRIBE.**
///
/// This is the boundary of the compression, and it is where compression would
/// do damage rather than good: a consumer that came back as a DIFFERENT BUILD
/// is exactly the fact the operator could not see on 2026-09-22. Folding that
/// into "same subscription, carry on" would have hidden the incident inside the
/// mechanism added to reveal it.
///
/// **THE MUTATION.** Drop the identity comparison in `connected`'s case 2 and
/// this goes red.
#[test]
fn a_different_consumer_on_one_connection_is_never_folded_into_a_resubscribe() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("threads.producer.jsonl");
    let l = ConsumerLedger::new();
    l.attach(path.clone(), "threads", "boot-a", "0.1.0");
    l.connected("accounts", None, 7, Some(id("threads-tui", "three-days-old")));
    let old_since = l.consumers("accounts")[0].since.clone();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    l.connected("accounts", None, 7, Some(id("threads-tui", "todays-build")));
    l.flush();

    // AND IT DOES NOT INHERIT THE OLD BUILD'S AGE. Letting it would report
    // today's build as three days old — the exact reading he needed, wrong.
    assert!(
        l.consumers("accounts")[0].since > old_since,
        "a different consumer is a different subscription: {} vs {old_since}",
        l.consumers("accounts")[0].since
    );

    let rs = records(&path);
    assert!(
        of_kind(&rs, "resubscribe").is_empty(),
        "a build change is not a re-subscribe"
    );
    let d = of_kind(&rs, "disconnect");
    assert_eq!(d.len(), 1);
    assert_eq!(d[0]["reason"], "resubscribed");
    let c = of_kind(&rs, "connect");
    assert_eq!(c.len(), 2, "the old build's departure and the new one's arrival");
    assert_eq!(c[0]["consumer"]["build"], "three-days-old");
    assert_eq!(c[1]["consumer"]["build"], "todays-build");
    assert!(still_open(&rs).len() == 1);
    assert_eq!(
        l.consumers("accounts")[0].build.as_deref(),
        Some("todays-build")
    );
}

/// **AND IF IT GROWS ANYWAY, IT ROTATES.**
///
/// Compression is the primary bound, but one cycle deliberately does not
/// compress: a consumer that drops its SOCKET and reconnects gets a fresh
/// connection id, and merging two connections into one consumer is an assertion
/// the chassis cannot make — an unidentified pair certainly is not knowably the
/// same consumer. That case, and simply years of ordinary use, is what the
/// size backstop is for.
///
/// `LEDGER_GENERATIONS` is 1, so the total is bounded at twice the threshold
/// with nothing to prune later.
///
/// **THE MUTATION.** Delete the `rotate_if_needed` call in
/// `LedgerFile::write_line` and the file grows past the bound.
#[test]
fn the_file_rotates_rather_than_growing_without_bound() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("threads.producer.jsonl");
    let rotated = tmp.path().join("threads.producer.jsonl.1");
    let l = ConsumerLedger::new();
    // A threshold a test can actually cross. See `attach_with_max_bytes`.
    l.attach_with_max_bytes(path.clone(), "threads", "boot-a", "0.1.0", 4_000);

    // Distinct connections, so nothing compresses and the file really grows.
    for conn in 0..200u64 {
        l.connected("accounts", None, conn, Some(id("threads-tui", "a1b2c3d")));
        l.disconnected("accounts", conn, CloseReason::ConnectionClosed);
    }
    l.flush();

    let live = std::fs::metadata(&path).unwrap().len();
    assert!(
        live <= 4_000,
        "the live file is bounded by the threshold; got {live}"
    );
    assert!(rotated.exists(), "and the previous generation is kept, once");
    let total = live + std::fs::metadata(&rotated).unwrap().len();
    assert!(
        total <= 2 * 4_000,
        "ONE generation, so the whole thing is bounded at twice the threshold: \
         got {total}"
    );
    assert!(
        !tmp.path().join("threads.producer.jsonl.2").exists(),
        "and there is no second generation to prune later"
    );
}

/// **THE PATH IS NAMED, AND IT HONOURS AN OVERRIDE.**
///
/// `~/.cauldron/data/<env>/<chassis>/links/<chassis>.producer.jsonl`. The
/// `.producer.` segment is the half the operator called *"both ends"* — it
/// reserves `<name>.consumer.jsonl` for the consumer half rather than leaving it
/// to be retrofitted into an ambiguous name.
///
/// **IT MOVED out of `~/.cauldron/chassis/` on 2026-10-07**, for two reasons
/// this test now pins. `chassis/` named a MECHANISM, and the operator ruled
/// *"chassis is a concept not an application"* — a directory in his data root is
/// owned by a service, not by an idea. And the old path was not per-environment,
/// so a ledger describing `prod`'s wiring would have outlived `rm -rf`ing `prod`.
///
/// The `CAULDRON_HOME` override is not a convenience: without it every test
/// that spins a daemon would append to the operator's real ledger and
/// interleave with every other test binary, which would make the file useless
/// for the one thing it is for.
#[test]
fn the_ledger_path_is_under_cauldron_home_and_names_the_producer_half() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("CAULDRON_HOME", tmp.path());

    // `CAULDRON_ENV` is pinned rather than left ambient. The resolver falls back
    // to `$ENV`, which a shell may already export for its own reasons, so an
    // unpinned test would assert against whatever environment the runner happened
    // to be in and pass or fail for a reason that has nothing to do with the code.
    std::env::set_var("CAULDRON_ENV", "prod");
    let p = threads::consumers::producer_ledger_path("threads").unwrap();
    assert_eq!(
        p,
        tmp.path()
            .join("data")
            .join("prod")
            .join("threads")
            .join("links")
            .join("threads.producer.jsonl"),
        "the ledger is per-environment and lives inside the service that writes it"
    );

    // THE ENVIRONMENT IS A REAL SEGMENT, not a constant spelled `prod`. Asserting
    // only the line above would pass just as well if the path hardcoded `prod`,
    // which is the mistake this second assertion exists to catch.
    std::env::set_var("CAULDRON_ENV", "dev");
    let d = threads::consumers::producer_ledger_path("threads").unwrap();
    assert_eq!(
        d,
        tmp.path()
            .join("data")
            .join("dev")
            .join("threads")
            .join("links")
            .join("threads.producer.jsonl"),
        "a different environment is a different ledger directory"
    );

    // A name that cannot be one path component is REFUSED rather than sanitised:
    // a silently rewritten environment name is a silently wrong ledger directory.
    std::env::set_var("CAULDRON_ENV", "../escape");
    assert!(
        threads::consumers::producer_ledger_path("threads").is_none(),
        "an unusable environment name yields no ledger path at all"
    );

    std::env::remove_var("CAULDRON_ENV");
    std::env::remove_var("CAULDRON_HOME");
}

/// **A CONNECTION TASK THAT IS ABORTED, OR PANICS, STILL CLOSES ITS ENTRIES.**
///
/// `drive` used to close its subscriptions with a bare
/// `manager.hub.dropped(conn_id)` as its LAST STATEMENT, and that statement has
/// two ways not to run:
///
/// * **a PANIC** anywhere in the loop unwinds straight past it, and
/// * **an ABORT** — `handle.abort()` is rung 2 of the supervisor's escalation
///   ladder and is what this daemon did on every stop before 2026-09-02 —
///   drops the task at its suspension point and runs nothing after it.
///
/// Either would leave `shapes.list` reporting a consumer that no longer exists,
/// which is the confidently-wrong state this whole module exists to prevent.
/// The fix is that the cleanup lives in a `Drop` on the task's own stack
/// (`ConnectionGuard` in `server::drive`), because a `Drop` runs in both cases:
/// this crate unwinds rather than aborting on panic, and an aborted future
/// drops its locals at the suspension point.
///
/// This test proves the MECHANISM the guard rests on, against the real ledger.
/// The WIRING — that `drive` actually holds such a guard — is proved over a
/// socket by `tests/shapes.rs::shapes_list_says_who_is_subscribed_and_which_build_each_one_is`.
///
/// **THE MUTATION.** Move the cleanup out of the guard to after the `await`,
/// which is the shape the code had before, and both legs go red.
#[tokio::test]
async fn an_aborted_or_panicking_connection_task_still_closes_its_entries() {
    use std::sync::Arc;

    struct ConnectionGuard(Arc<ConsumerLedger>, u64);
    impl Drop for ConnectionGuard {
        fn drop(&mut self) {
            self.0.dropped(self.1);
        }
    }

    // ── ABORT.
    let l = Arc::new(ConsumerLedger::new());
    let l2 = Arc::clone(&l);
    let h = tokio::spawn(async move {
        let _guard = ConnectionGuard(Arc::clone(&l2), 7);
        l2.connected("accounts", None, 7, Some(id("threads-tui", "a1b2c3d")));
        // The suspension point an abort lands on. Nothing after it ever runs.
        std::future::pending::<()>().await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(l.consumers("accounts").len(), 1, "fixture: it subscribed");
    h.abort();
    let _ = h.await;
    assert!(
        l.consumers("accounts").is_empty(),
        "AN ABORTED CONNECTION TASK IS NOT A SUBSCRIBER. Nothing after the \
         suspension point runs, so the close cannot live there"
    );

    // ── PANIC.
    let l = Arc::new(ConsumerLedger::new());
    let l2 = Arc::clone(&l);
    let h = tokio::spawn(async move {
        let _guard = ConnectionGuard(Arc::clone(&l2), 9);
        l2.connected("accounts", None, 9, Some(id("threads-tui", "a1b2c3d")));
        panic!("the connection task fell over");
    });
    assert!(h.await.is_err(), "fixture: it really panicked");
    assert!(
        l.consumers("accounts").is_empty(),
        "A PANICKED CONNECTION TASK IS NOT A SUBSCRIBER EITHER — the unwind \
         runs the guard and skips everything else"
    );
}

/// **A SECOND KEY IS A `widen`, AND NOT RULE 2.5's RECOVERY ACT.**
///
/// # Two acts, one word
///
/// `ConsumerLedger::connected`'s case 2 — *this connection already holds this
/// shape, same consumer* — covered both *"I doubted my state and re-subscribed"*
/// and *"I now also want key `b`"*, and recorded both as `resubscribe`. Those are
/// not the same event and they are not read for the same reason: `resubscribe` is
/// the column an operator scans to find a consumer stuck in a reconnect loop,
/// which is the EXPECTED failure mode of a design with no re-emit verb, and a
/// healthy consumer widening its interest was being filed in it.
///
/// The same arm also overwrote `LiveEntry.key`, so `shapes.list` reported the
/// consumer as holding `cc2` when it held both — the registry half, asserted in
/// `shapewire::ledger`'s own unit tests.
///
/// **THE MUTATION.** Merge the two arms back — record `EVENT_RESUBSCRIBE` whether or
/// not `already_held` — and the `widen` assertion here goes red; the `resubscribe`
/// count assertion catches the reverse mistake, a widen recorded where a genuine
/// repeat belongs.
#[test]
fn a_second_key_on_one_connection_is_a_widen_and_not_a_resubscribe() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("threads.producer.jsonl");
    let l = ConsumerLedger::new();
    l.attach(path.clone(), "threads", "boot-a", "0.1.0");
    let who = id("threads-tui", "a1b2c3d");

    l.connected("accounts", Some("cc1"), 7, Some(who.clone()));
    // WIDEN: a key this subscription does not hold.
    l.connected("accounts", Some("cc2"), 7, Some(who.clone()));
    // AND A GENUINE REPEAT of a key it does hold — rule 2.5's recovery act, which
    // must still be recorded as one.
    l.connected("accounts", Some("cc2"), 7, Some(who.clone()));
    l.flush();

    let rs = records(&path);
    let words: Vec<&str> = rs
        .iter()
        .filter_map(|r| r["event"].as_str())
        .filter(|w| *w != "chassis_start")
        .collect();
    assert_eq!(
        words,
        vec!["connect", "widen", "resubscribe"],
        "three acts, three words, in order — the middle one used to be spelled \
         `resubscribe` and is a different event: {rs:#?}"
    );

    let w = of_kind(&rs, "widen");
    assert_eq!(w.len(), 1);
    assert_eq!(w[0]["key"], "cc2", "the NEW key this call added");
    assert_eq!(
        w[0]["keys"],
        serde_json::json!(["cc1", "cc2"]),
        "and every key the subscription holds after it — the field that stops \
         `shapes.list` reporting the last key named as the only one held"
    );
    assert_eq!(w[0]["conn"], 7);
    assert_eq!(w[0]["consumer"]["build"], "a1b2c3d");

    assert_eq!(
        of_kind(&rs, "connect").len(),
        1,
        "still ONE subscription — a widen is not a new one, and the pairing \
         invariant every reader of this file depends on must not gain a row"
    );
    assert!(
        of_kind(&rs, "disconnect").is_empty(),
        "and nothing closed: {rs:#?}"
    );
    assert!(
        still_open(&rs).len() == 1,
        "one open subscription, as the replay sees it: {:?}",
        still_open(&rs)
    );
}
