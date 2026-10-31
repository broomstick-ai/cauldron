//! # `accounts/gauge-history` — an account's week of gauge readings, as a shape
//!
//! R2.S3d, item B: the carrier for the board's 7-day sparkline, which only the
//! `gauge/history` request answered. Four properties, each forced red by the
//! mutation named in its docstring:
//!
//! 1. a cold subscriber receives the WEEK off the hydrate (the same store read
//!    the request made), percentage axes only;
//! 2. a recorded reading is APPENDED on the live fold, as a `transition` and on
//!    the row;
//! 3. the append is guarded by IDENTITY — a second fact for one reading appends
//!    nothing;
//! 4. the window is the number the module doc derives: 5,040 a week per axis at
//!    the 120 s busy cadence, ×3 axes = 15,120 retained items, published as
//!    `max.retained_items`.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use threads::config::Config;
use threads::gauge::Axis;
use threads::shape::{ConnFrames, Derived, Hub, Input, Op, Shape, ShapeInfo};
use threads::shapes::{accounts, gauge_history as gh};
use threads::{build_manager, ThreadManager};

async fn start() -> (ThreadManager, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.path().join("claude-home"));
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude")),
    );
    let mut accts = BTreeMap::new();
    accts.insert(
        "first".to_string(),
        tmp.path().join("first").to_string_lossy().to_string(),
    );
    threads::clock::testing::ticking();
    let manager = build_manager(Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts: accts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    })
    .unwrap();
    let hub: Arc<Hub> = manager.hub.clone();
    // PARENTS FIRST: `accounts/gauge-history` declares `PARENT = Some("accounts")`.
    hub.register(accounts::Accounts, manager.clone());
    hub.register(gh::GaugeHistory, manager.clone());
    (manager, tmp)
}

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
}

fn drain(f: &mut ConnFrames) -> Vec<Arc<threads::shape::ShapeFrame>> {
    let mut all = Vec::new();
    while let Ok(x) = f.try_recv() {
        all.push(x);
    }
    all
}

fn newest_row(frames: &[Arc<threads::shape::ShapeFrame>], key: &str) -> Value {
    frames
        .iter()
        .filter(|f| f.wire.key.as_deref() == Some(key))
        .filter_map(|f| match (&f.wire.op, &f.wire.event, &f.wire.row) {
            (Op::Live, Some(Derived::Row { row }), _) => Some(row.clone()),
            (Op::Read, _, Some(row)) => Some(row.clone()),
            _ => None,
        })
        .last()
        .unwrap_or_else(|| panic!("no row for `{key}` among {} frames", frames.len()))
}

fn transitions(frames: &[Arc<threads::shape::ShapeFrame>], key: &str) -> Vec<Value> {
    frames
        .iter()
        .filter(|f| f.wire.key.as_deref() == Some(key))
        .filter_map(|f| match &f.wire.event {
            Some(Derived::Transition { cell, item }) if cell == "readings" => Some(item.clone()),
            _ => None,
        })
        .collect()
}

fn readings(row: &Value) -> Vec<Value> {
    row["readings"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("`readings` is a list: {row}"))
}

fn observe(m: &ThreadManager, axis: Axis, pct: f64) {
    m.budgets
        .observe("first", axis, pct, None, "usage_poll")
        .unwrap();
}

async fn watch(m: &ThreadManager, conn: u64) -> ConnFrames {
    let f = m.hub.connect(conn);
    m.hub
        .subscribe(gh::GaugeHistory::NAME, Some("first"), conn, None)
        .expect("a keyed subscribe to one account's week");
    settle().await;
    f
}

/// **THE COLD SUBSCRIBER RECEIVES THE WEEK.**
///
/// Four readings recorded before anything subscribes — two on `weekly_all`, one
/// on `session`, one on `cash` — so only the hydrate can put them on the row,
/// and only the three percentage-axis ones belong there.
///
/// **MUTATION.** `GaugeHistory::hydrates_key` skips the `read` (the window stays
/// empty): red, `0` readings where the store holds 3. **MUTATION 2.** Drop the
/// `PERCENT_AXES` restriction in the handler (read `Axis::ALL`): red, the `cash`
/// reading appears.
#[tokio::test]
async fn the_export_is_the_accounts_week_on_the_percentage_axes() {
    let (m, tmp) = start().await;
    observe(&m, Axis::WeeklyAll, 10.0);
    observe(&m, Axis::Session, 40.0);
    observe(&m, Axis::Cash, 3.0);
    observe(&m, Axis::WeeklyAll, 12.5);

    let mut f = watch(&m, 41).await;
    let row = newest_row(&drain(&mut f), "first");
    let r = readings(&row);

    assert_eq!(r.len(), 3, "the three percentage-axis readings, as recorded: {row}");
    let weekly: Vec<f64> = r
        .iter()
        .filter(|x| x["axis"] == json!("weekly_all"))
        .map(|x| x["pct"].as_f64().unwrap())
        .collect();
    assert_eq!(weekly, [10.0, 12.5], "oldest first, the recorded values: {row}");
    assert!(
        r.iter().all(|x| x["at"].is_string() && x["source"] == json!("usage_poll")),
        "every reading carries its observed instant and source: {row}"
    );
    assert!(
        r.iter().all(|x| x["axis"] != json!("cash")),
        "cash is a dollar scale no sparkline draws: {row}"
    );
    drop(tmp);
}

/// **A RECORDED READING IS APPENDED ON THE FOLD — ONCE.**
///
/// After the export, the producer records a reading and sends its own fact
/// (`emit_gauge_fact`, the path `/usage` takes). The connection must receive a
/// `transition` carrying it and a row one reading longer. Then the SAME fact
/// again — the fact re-reads the newest stored observation, so a second emit
/// carries the reading already held — and nothing more may be appended.
///
/// **MUTATION 1.** Delete the `GaugeObserved` arm's body (`=> {}`): red, no
/// transition and the row stays at 1. **MUTATION 2.** Delete the identity guard
/// (`if held.is_some_and(|h| at <= h) { return; }`): red, the duplicate is
/// appended and the row says 3.
#[tokio::test]
async fn a_recorded_reading_is_appended_once_on_the_live_fold() {
    let (m, tmp) = start().await;
    observe(&m, Axis::WeeklyAll, 20.0);
    let mut f = watch(&m, 42).await;
    let before = readings(&newest_row(&drain(&mut f), "first"));
    assert_eq!(before.len(), 1, "the hydrated reading");

    observe(&m, Axis::WeeklyAll, 33.0);
    m.emit_gauge_fact("first", Axis::WeeklyAll);
    settle().await;
    let frames = drain(&mut f);
    let t = transitions(&frames, "first");
    assert_eq!(t.len(), 1, "one reading recorded, one transition: {t:?}");
    assert_eq!(t[0]["pct"], json!(33.0));
    assert_eq!(t[0]["axis"], json!("weekly_all"));
    let after = readings(&newest_row(&frames, "first"));
    assert_eq!(after.len(), 2, "the row carries the appended reading: {after:?}");

    // The same reading, announced again.
    m.emit_gauge_fact("first", Axis::WeeklyAll);
    settle().await;
    let frames = drain(&mut f);
    assert!(
        transitions(&frames, "first").is_empty(),
        "a second fact for one reading appends nothing"
    );
    let again = frames
        .iter()
        .any(|fr| fr.wire.key.as_deref() == Some("first"))
        .then(|| readings(&newest_row(&frames, "first")))
        .unwrap_or(after);
    assert_eq!(again.len(), 2, "still two readings: {again:?}");
    drop(tmp);
}

/// **AN ACCOUNT OUTSIDE THE POOL IS NO KEY.** A week is about a pool member;
/// a name the daemon does not hold must not mint an empty series that reads as
/// "no usage this week".
///
/// **MUTATION.** `hydrates_key` ignores `week.member` and returns `true`: red,
/// the subscribe is served a row for `nobody`.
#[tokio::test]
async fn an_account_outside_the_pool_is_no_key() {
    let (m, tmp) = start().await;
    let mut f = m.hub.connect(43);
    let _ = m.hub.subscribe(gh::GaugeHistory::NAME, Some("nobody"), 43, None);
    settle().await;
    let frames = drain(&mut f);
    assert!(
        !frames.iter().any(|fr| fr.wire.key.as_deref() == Some("nobody") && fr.wire.row.is_some()),
        "no row for an account the pool does not hold"
    );
    drop(tmp);
}

/// **THE WINDOW IS THE DERIVED NUMBER, AND THE CHASSIS PUBLISHES IT.**
///
/// **MUTATION.** `DEFAULT_PROBE_INTERVAL_BUSY_SECS = 60` (the cadence doubles):
/// red, `PER_AXIS` is 10,080 and `retained_items` 30,240.
#[test]
fn the_window_is_a_week_at_the_busy_cadence_on_three_axes() {
    assert_eq!(gh::PER_AXIS, 7 * 86_400 / 120, "a week of 120 s readings");
    assert_eq!(gh::PER_AXIS, 5_040);
    assert_eq!(gh::WINDOW, 15_120, "three percentage axes");
    let info = ShapeInfo::declared_by::<Input, gh::GaugeHistory>();
    assert_eq!(info.max.retained_items, 15_120, "published, not just held");
    assert_eq!(info.name, "accounts/gauge-history");
}

/// **THE HELD WINDOW IS EXACTLY `WINDOW`, AND THE OLDEST IS WHAT GOES.**
///
/// `WINDOW + 1` readings folded straight through `GaugeHistory::folds` (ten
/// seconds apart — 42 hours, so the 7-day clock evicts nothing and the COUNT
/// is the bound under test). The held series must be 15,120 long, start at the
/// SECOND reading and end at the last one.
///
/// **FORCED RED (R2.S3d inspect):** (a) `Sequence::new(WINDOW + 1, …)` in
/// `SeriesCells::default` — 15,121 held; (b) OPPOSED, the fold refusing to
/// append once the window is full (the NEWEST dropped instead of the oldest) —
/// the count is right and the ends are wrong.
#[test]
fn the_held_window_is_exactly_window_and_the_oldest_is_dropped() {
    use threads::shape::{Root, Stamped, Tx};
    let mut root = gh::GaugeHistoryRoot::default();
    root.rows.key("first".to_string());
    let base: chrono::DateTime<chrono::Utc> = "2026-10-01T00:00:00Z".parse().unwrap();
    let mut shape = gh::GaugeHistory;
    for i in 0..=gh::WINDOW {
        let at = base + chrono::Duration::seconds(10 * i as i64);
        let fact = Input::Fact(threads::event::Fact::GaugeObserved {
            account: "first".into(),
            axis: "weekly_all".into(),
            row: json!({
                "observed_pct": i as f64,
                "observed_at": at.to_rfc3339(),
                "source": "usage_poll",
                "resets_at": null,
            }),
        });
        let mut tx = Tx::new(&mut root, at);
        shape.folds(&Stamped::new(at, fact), &mut tx);
    }
    let row = root.row("first").expect("the key is held");
    let r = readings(&row);
    assert_eq!(r.len(), 15_120, "the held window is WINDOW, exactly");
    assert_eq!(r[0]["pct"], json!(1.0), "the OLDEST reading (0) is the one dropped");
    assert_eq!(
        r.last().unwrap()["pct"],
        json!(gh::WINDOW as f64),
        "the NEWEST reading is held"
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
