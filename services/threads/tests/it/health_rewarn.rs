//! **WARN-1 D-D — A STANDING HEALTH VIOLATION IS WARNED ONCE, AND AN ID RETURNS
//! AS NEWS ONLY AFTER TWO CLEAN PASSES.**
//!
//! an internal report §3 (R10.H2 row): `Health::apply` re-printed the
//! WHOLE standing set whenever one id joined it, so every standing id was
//! warned again each time; and it forgot an id on the first pass it was
//! absent, so an id that dropped out for one pass (a ledger skipped
//! mid-append, a failed `ps`) re-warned on its return.
//!
//! These drive `Health::apply` with constructed `Report`s, so the passes are
//! exact and there is no period to outlast (doctrine 4: the period lives in the
//! loop, not in `apply`). Warnings are counted by a thread-local capture
//! (`support/capture.rs`) on `health_check`'s own target; the assertions count
//! how many WARN records NAME a given id.
//!
//! Who is in the degraded regime (doctrine 10): a pass is degraded when
//! `skipped_ledgers > 0` (I2/I3's ledgers) or the process table failed (I1's
//! pid half, I5). Both are exercised. The `procs_failed` arm is asserted
//! through `Health::pass`'s own setter by
//! `a_failed_ps_through_pass_is_degraded_and_does_not_age_an_absent_id` below
//! (WARN-1b), which drives `pass()` on a real manager with a failing process
//! source; the lib's unit test builds its `Report` by hand and does not reach it.

#[path = "support/capture.rs"]
mod capture;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use threads::health_check::{Health, Invariant, Limits, Proc, Report};

fn health() -> Arc<Health> {
    Health::with(Limits::default(), Arc::new(|| Ok(Vec::<Proc>::new())))
}

fn rep(inv: Invariant, ids: &[&str], skipped: usize) -> Report {
    let mut v = BTreeMap::new();
    if !ids.is_empty() {
        v.insert(inv, ids.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>());
    }
    Report { violations: v, skipped_ledgers: skipped, ..Default::default() }
}

/// WARN records from `health_check` for `code` whose message names `id`.
fn warned(recs: &std::sync::Mutex<Vec<capture::Rec>>, code: &str, id: &str) -> usize {
    recs.lock()
        .unwrap()
        .iter()
        .filter(|r| r.target == "threads::health_check" && r.level == tracing::Level::WARN)
        .filter(|r| r.fields.get("invariant").map(String::as_str) == Some(code))
        .filter(|r| r.message.contains(id))
        .count()
}

/// **A STANDING SET IS NOT RE-WARNED ACROSS FOUR PASSES, N = 3 IDS ON TWO
/// INVARIANTS.** Pass 1: I2 {a}, I3 {x}. Pass 2: I2 {a, b} — only `b` is news.
/// Pass 3: I2 {a, b, c} — only `c`. Pass 4: unchanged — nothing.
///
/// Opposed: `b` and `c` MUST be warned once each (a `apply` that never warns
/// fails), and `a` and `x` exactly once (the old whole-set re-print warns `a`
/// three times).
#[test]
fn a_standing_set_is_warned_once_per_id_across_four_passes() {
    let (recs, _g) = capture::install();
    let h = health();
    let both = |i2: &[&str]| {
        let mut r = rep(Invariant::I2, i2, 0);
        r.violations.insert(Invariant::I3, ["x".to_string()].into_iter().collect());
        r
    };
    h.apply(&both(&["thread=a"]));
    h.apply(&both(&["thread=a", "thread=b"]));
    h.apply(&both(&["thread=a", "thread=b", "thread=c"]));
    h.apply(&both(&["thread=a", "thread=b", "thread=c"]));
    assert_eq!(warned(&recs, "I2", "thread=a"), 1, "the standing id `a` was re-warned when b and c joined");
    assert_eq!(warned(&recs, "I2", "thread=b"), 1, "`b` is news exactly once");
    assert_eq!(warned(&recs, "I2", "thread=c"), 1, "`c` is news exactly once");
    assert_eq!(warned(&recs, "I3", "x"), 1, "a standing I3 is warned once over four passes");
    assert_eq!(h.standing().get("I2").map(Vec::len), Some(3), "standing still reports the whole current set");
}

/// **AN ID THAT DROPS OUT FOR ONE CLEAN PASS IS NOT NEWS ON RETURN; ONE ABSENT
/// FOR TWO CLEAN PASSES IS.** N = 2 ids so the hysteresis is per id, not per
/// invariant: `a` drops for one pass, `b` stays throughout.
///
/// Opposed both ways: a one-pass dropout re-warning fails the first half (the
/// old code), and an `apply` that never forgets fails the second.
#[test]
fn an_id_returns_as_news_only_after_two_clean_passes() {
    let (recs, _g) = capture::install();
    let h = health();
    let i1 = |ids: &[&str]| rep(Invariant::I1, ids, 0);
    h.apply(&i1(&["thread=a", "thread=b"]));
    h.apply(&i1(&["thread=b"])); // a absent: clean pass 1
    assert_eq!(h.standing().get("I1").cloned(), Some(vec!["thread=b".to_string()]), "standing is this pass's truth");
    h.apply(&i1(&["thread=a", "thread=b"])); // a back after ONE clean pass
    assert_eq!(warned(&recs, "I1", "thread=a"), 1, "a one-pass dropout re-warned on return");
    h.apply(&i1(&["thread=b"])); // a absent: clean 1
    h.apply(&i1(&["thread=b"])); // a absent: clean 2 -> forgotten
    h.apply(&i1(&["thread=a", "thread=b"]));
    assert_eq!(warned(&recs, "I1", "thread=a"), 2, "after two clean passes absent, `a` returning IS news");
    assert_eq!(warned(&recs, "I1", "thread=b"), 1, "`b` never left and was warned once");
}

/// **A DEGRADED PASS NEVER COUNTS TOWARD FORGETTING.** `a` is absent for two
/// passes that skipped a ledger (`skipped_ledgers > 0`) and then for ONE clean
/// pass: three absences, one of them clean, so its return is not news. The
/// counterpart: one more clean absence and it is.
#[test]
fn a_degraded_pass_never_counts_toward_forgetting() {
    let (recs, _g) = capture::install();
    let h = health();
    h.apply(&rep(Invariant::I3, &["child=a"], 0));
    h.apply(&rep(Invariant::I3, &[], 1)); // degraded
    h.apply(&rep(Invariant::I3, &[], 1)); // degraded
    h.apply(&rep(Invariant::I3, &[], 0)); // clean 1
    h.apply(&rep(Invariant::I3, &["child=a"], 0));
    assert_eq!(warned(&recs, "I3", "child=a"), 1, "an id that vanished only on degraded passes re-warned");
    h.apply(&rep(Invariant::I3, &[], 0)); // clean 1
    h.apply(&rep(Invariant::I3, &[], 1)); // degraded: no count, no reset
    h.apply(&rep(Invariant::I3, &[], 0)); // clean 2 -> forgotten
    h.apply(&rep(Invariant::I3, &["child=a"], 0));
    assert_eq!(warned(&recs, "I3", "child=a"), 2, "two clean absences around a degraded pass forget it");
}

/// A daemon with an empty store, for driving `Health::pass` for real. No
/// thread exists, so an orphaned (ppid 1) claude holding ANY session is I5.
async fn empty_manager() -> (threads::ThreadManager, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.path().join("claude-home"));
    let mut accounts = BTreeMap::new();
    accounts.insert("first".to_string(), tmp.path().join("first").to_string_lossy().to_string());
    let m = threads::build_manager(threads::Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    })
    .unwrap();
    (m, tmp)
}

/// A process source that answers a fixed SCRIPT, one entry per call: `Some`
/// is a process table, `None` is a failed `ps`. Past the end it fails.
fn scripted(script: Vec<Option<Vec<Proc>>>) -> threads::health_check::ProcSource {
    let q = std::sync::Mutex::new(std::collections::VecDeque::from(script));
    Arc::new(move || match q.lock().unwrap().pop_front().flatten() {
        Some(p) => Ok(p),
        None => Err(std::io::Error::other("ps: scripted failure")),
    })
}

/// **WARN-1b (WARN-1 inspect F1): A FAILED `ps`, THROUGH `Health::pass`'s OWN
/// SETTER, IS A DEGRADED PASS AND DOES NOT AGE AN ABSENT ID.**
///
/// The tests above build their `Report`s by hand, so none of them reaches the
/// one line that marks a real failed `ps` (`rep.procs_failed = procs_failed;`
/// in `Health::pass`). This one drives `pass()` on a real manager with a
/// scripted process source: pass 1 sees an orphaned claude (`pid=4242`, ppid
/// 1, session `no-such-thread`) — I5, warned once; passes 2 and 3 FAIL to read
/// the table; pass 4 sees it again. Two degraded passes must not forget it, so
/// pass 4 is not news: one warning in total.
///
/// Opposed (doctrine 2): the same four passes with 2 and 3 reading an EMPTY
/// table (clean) DO forget it, and pass 4 re-warns — so the capture can see a
/// second warning, and a `pass` that never marks anything clean cannot pass.
///
/// Forced red by `let _ = procs_failed;` in place of the setter (WARN-1b
/// report §5): the failed passes then read as clean, `pid=4242` is forgotten,
/// and the first half sees 2 warnings.
#[tokio::test]
async fn a_failed_ps_through_pass_is_degraded_and_does_not_age_an_absent_id() {
    let (m, _tmp) = empty_manager().await;
    let orphan = || vec![Proc { pid: 4242, ppid: 1, age_s: 3600, args: "claude --session-id no-such-thread".into() }];
    let id = "pid=4242";

    // ── failed `ps` on passes 2 and 3: degraded, nothing ages ──────────────
    {
        let (recs, _g) = capture::install();
        let h = Health::with(Limits::default(), scripted(vec![Some(orphan()), None, None, Some(orphan())]));
        let r1 = h.pass(&m).unwrap();
        assert!(!r1.procs_failed && !r1.degraded(), "a readable table is a clean pass: {r1:?}");
        assert_eq!(r1.ids(Invariant::I5).len(), 1, "the orphan is I5 on a real pass: {r1:?}");
        for n in [2, 3] {
            let r = h.pass(&m).unwrap();
            assert!(r.procs_failed, "pass {n}: a failed `ps` must be marked by `pass` itself: {r:?}");
            assert!(r.degraded(), "pass {n}: a failed `ps` is a degraded pass: {r:?}");
            assert!(r.ids(Invariant::I5).is_empty(), "pass {n}: a failed table sees no I5: {r:?}");
        }
        h.pass(&m).unwrap();
        assert_eq!(warned(&recs, "I5", id), 1, "an id absent only on failed-`ps` passes re-warned on return");
    }

    // ── counterpart: passes 2 and 3 read an EMPTY table, clean, it is forgotten
    {
        let (recs, _g) = capture::install();
        let h = Health::with(Limits::default(), scripted(vec![Some(orphan()), Some(vec![]), Some(vec![]), Some(orphan())]));
        for _ in 0..4 {
            let r = h.pass(&m).unwrap();
            assert!(!r.procs_failed, "every table in this half was readable: {r:?}");
        }
        assert_eq!(warned(&recs, "I5", id), 2, "two CLEAN absences must forget the id, so its return is news");
    }
}
