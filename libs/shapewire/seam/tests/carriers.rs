//! **R2.S3d'S THREE CARRIERS, READ BY THE REFERENCE** — `view::context_id`,
//! `view::message_pages` and `view::gauge_series`.
//!
//! Replayed off the PRODUCER'S OWN recording, `conformance/r2s3d_carriers.jsonl`
//! (`threads-dev` at 2001414: `threads` keyed `dev-idle-00`, four messages,
//! never compacted; `accounts/gauge-history` keyed `dev-healthy`, its store
//! seeded before boot with two `weekly_all`, one `session` and one `cash`
//! reading — re-recorded at the R2.S3d inspect so the series is not `[]`, which
//! a reader returning a constant empty list would have passed). The values
//! asserted are the frames', so a reader that invented them is red.
//!
//! The one synthetic frame here is the gauge TRANSITION in the last test: the
//! dev daemon polls no `/usage`, so a LIVE append cannot be recorded from it.
//! It is built on the recorded export's own envelope, its fields spelled by the
//! inventory's consts, and it says so.

use std::path::Path;

use seam_reference::held::{Fact, Held, LinkState};
use seam_reference::inventory::accounts_gauge_history::readings as r;
use seam_reference::view;
use serde_json::{json, Value};

fn recorded() -> (Held, Vec<Value>) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance/r2s3d_carriers.jsonl");
    let text = std::fs::read_to_string(&path).expect("the recorded transcript");
    let mut h = Held::new();
    h.link = LinkState::Up {
        boot_id: "transcript".into(),
        since: "2026-10-07T00:00:00Z".parse().expect("a fixed stamp"),
    };
    let mut frames = Vec::new();
    for raw in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        if let Some(f) = v.get("frame") {
            h.fold(f);
            frames.push(f.clone());
        }
    }
    assert!(!frames.is_empty(), "the transcript carried no frame, so nothing below is evidence");
    (h, frames)
}

/// **THE CONTEXT ID AND THE PAGES ARE THE FRAME'S.**
///
/// FORCED RED (R2.S3d): (a) `view::context_id` reading `inventory::threads::
/// LAST_MESSAGE_ID` instead of `CONTEXT_ID` — the cell holds a number, so the
/// read is `NotHydrated`; (b) `view::message_pages` reading `to` off `FROM` —
/// the page reads `0..0` where the frame says `0..4`.
#[test]
fn the_context_id_and_the_pages_are_read_off_the_threads_frame() {
    let (h, frames) = recorded();
    let row = frames
        .iter()
        .find(|f| f["shape"] == "threads")
        .map(|f| f["row"].clone())
        .expect("the threads export");

    assert_eq!(
        view::context_id(&h, "dev-idle-00").known().as_deref(),
        Some("dev-idle-00.0"),
        "never compacted: the first context, as the producer derived it"
    );

    let pages = view::message_pages(&h, "dev-idle-00")
        .known()
        .expect("the pages cell is known on the export");
    assert_eq!(pages.len(), 1, "four messages are one open page: {pages:?}");
    assert_eq!(pages[0].id, "dev-idle-00.m0-4");
    assert_eq!((pages[0].from, pages[0].to), (0, 4));
    assert_eq!(
        Some(pages[0].to),
        row["last_message_id"].as_u64(),
        "the open page ends at the row's own newest message"
    );
}

/// **THE RECORDED WEEK IS THE FRAME'S THREE READINGS; A KEY NEVER OPENED IS
/// NOT A WEEK.**
///
/// FORCED RED (R2.S3d) by taking `GAUGE_HISTORY` out of `Held::new`'s mirror
/// list (the export is dropped: `None` where the frame says three), and by
/// `view::gauge_series` answering `Known(vec![])` for every key.
#[test]
fn the_recorded_week_is_read_and_an_unopened_account_is_not() {
    let (h, _) = recorded();
    let week = view::gauge_series(&h, "dev-healthy").known();
    let got: Option<Vec<(String, f64)>> =
        week.map(|v| v.into_iter().map(|g| (g.axis, g.pct)).collect());
    assert_eq!(
        got,
        Some(vec![
            ("weekly_all".to_string(), 20.0),
            ("weekly_all".to_string(), 35.5),
            ("session".to_string(), 12.0),
        ]),
        "the seeded week, oldest first, percentage axes only (no cash)"
    );
    assert!(
        matches!(view::gauge_series(&h, "dev-ratelimited"), Fact::NotHydrated { .. }),
        "no export for this key — not a week with nothing in it"
    );
}

/// **A READING APPENDED ON THE FOLD IS IN THE SERIES.**
///
/// FORCED RED (R2.S3d) by `view::gauge_series` answering `Known(vec![])` for
/// every key: the appended reading is not read.
#[test]
fn a_reading_appended_on_the_fold_is_in_the_series() {
    let (mut h, frames) = recorded();
    let export = frames
        .iter()
        .find(|f| f["shape"] == "accounts/gauge-history")
        .expect("the gauge export");
    // SYNTHETIC — see the module doc. The recorded export's envelope, one
    // `transition` on `readings`, each field named by the inventory.
    let at = "2026-10-10T14:00:00Z";
    let mut item = serde_json::Map::new();
    item.insert(r::AXIS.into(), json!("weekly_all"));
    item.insert(r::PCT.into(), json!(42.5));
    item.insert(r::SOURCE.into(), json!("usage_poll"));
    item.insert(r::AT.into(), json!(at));
    h.fold(&json!({
        "type": "shape",
        "shape": export["shape"],
        "key": export["key"],
        "op": "live",
        "at": at,
        "epoch": export["epoch"],
        "event": {"kind": "transition", "cell": "readings", "item": Value::Object(item)},
    }));

    let series = view::gauge_series(&h, "dev-healthy")
        .known()
        .expect("still known after the append");
    assert_eq!(series.len(), 4, "three recorded, one appended: {series:?}");
    let last = series.last().unwrap();
    assert_eq!(last.axis, "weekly_all");
    assert_eq!(last.pct, 42.5);
    assert_eq!(last.at.to_rfc3339(), "2026-10-10T14:00:00+00:00");
    assert_eq!(last.resets_at, None, "optional, and absent");
}
