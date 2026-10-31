//! **WHICH END OF THE WINDOW `gauge/history` KEEPS WHEN IT CLIPS.**
//!
//! `Store::observations_between` is the one read behind `gauge/history`, and it
//! had no test at all — not for its ordering, not for its cap. That absence is
//! why the defect below lived: the query read `ORDER BY id ASC LIMIT ?5`, so a
//! series longer than the cap came back as the window's FIRST rows and a
//! sparkline labelled "7d" was drawn from hours that had already gone by.
//!
//! The cadence is what makes that reachable rather than theoretical. The busy
//! probe interval is 120 s (`config::default_probe_interval_busy`), so one axis
//! holds ~5,040 observations in a week against what was the request's default
//! cap of 1,000 (`server::GAUGE_HISTORY_DEFAULT_LIMIT`, retired with the
//! `gauge/history` request by R2.S3c / R4.S9; the store read survives and is
//! what the `gauge-history` shape hydrates from).
//!
//! Two properties, and the file is deliberately both halves:
//!
//! 1. **CLIPPED** — the cap keeps the NEWEST rows in the window.
//! 2. **UNCLIPPED** — a series under the cap is whole, ascending, and starts at
//!    the window's first row. This is the POSITIVE SENTINEL: it fails if the
//!    fixture never lands, if the window filter excludes everything, or if the
//!    reverse were applied to the wrong end — so a green in (1) cannot be a
//!    green that simply read nothing.

use threads::store::{ObservationRow, Store};

const ACCOUNT: &str = "anthropic:sam";
const AXIS: &str = "five_hour_pct";

/// `n` observations two minutes apart — the BUSY cadence, which is the one the
/// cap actually bites at. `pct` carries the index so a row's identity is
/// readable in an assertion failure.
fn seed(s: &Store, n: usize) {
    for i in 0..n {
        let mins = i * 2;
        s.record_observation(&ObservationRow {
            id: 0, // assigned by the insert
            account: ACCOUNT.into(),
            axis: AXIS.into(),
            pct: i as f64,
            resets_at: None,
            source: "probe".into(),
            observed_at: format!("2026-10-01T{:02}:{:02}:00Z", mins / 60, mins % 60),
        })
        .unwrap();
    }
}

/// A window wide enough to admit every row `seed` writes.
const SINCE: &str = "2026-10-01T00:00:00Z";
const UNTIL: &str = "2026-10-02T00:00:00Z";

/// **(1) THE CLIP TAKES THE NEWEST ROWS.**
///
/// 50 observations, a cap of 10. The answer must be observations 40..=49 in
/// ascending order — not 0..=9, which is what `ORDER BY id ASC LIMIT` gave.
#[test]
fn a_clipped_series_is_the_newest_rows_in_the_window() {
    let s = Store::open_in_memory().unwrap();
    seed(&s, 50);

    let got = s
        .observations_between(ACCOUNT, AXIS, SINCE, UNTIL, 10)
        .unwrap();

    assert_eq!(got.len(), 10, "the cap must bite at exactly the cap");

    let pcts: Vec<f64> = got.iter().map(|r| r.pct).collect();
    assert_eq!(
        pcts,
        (40..50).map(|i| i as f64).collect::<Vec<f64>>(),
        "a clipped series must be the NEWEST rows of the window, ascending. \
         Getting 0..10 here is the defect this file exists for: the cap kept \
         the window's first rows and the sparkline showed a window that had \
         already passed"
    );
}

/// The declared contract — *oldest first* — survives the `DESC` the cap needs.
/// Asserted as a standalone property because a caller's x-axis depends on it
/// and a reversal that leaked would invert every history view silently.
#[test]
fn a_clipped_series_is_still_ascending_by_id() {
    let s = Store::open_in_memory().unwrap();
    seed(&s, 50);

    let got = s
        .observations_between(ACCOUNT, AXIS, SINCE, UNTIL, 10)
        .unwrap();

    assert!(got.len() >= 2, "need at least two rows to have an order");
    assert!(
        got.windows(2).all(|w| w[0].id < w[1].id),
        "ids must ascend; the SQL orders DESC under the limit and the answer is \
         reversed back, so a descending answer here means the reverse was lost: {:?}",
        got.iter().map(|r| r.id).collect::<Vec<i64>>()
    );
    assert!(
        got.windows(2).all(|w| w[0].observed_at <= w[1].observed_at),
        "timestamps must ascend with the ids"
    );
}

/// **(2) THE POSITIVE SENTINEL.**
///
/// Under the cap nothing is dropped, so the answer is the whole window starting
/// at its FIRST row. This is what proves the clipped assertions above are
/// reading real rows: if the fixture never landed, if the `observed_at` window
/// excluded everything, or if the reverse were unconditional on the wrong end,
/// this test fails and the suite cannot be green while test (1) passes
/// vacuously.
#[test]
fn an_unclipped_series_is_whole_and_starts_at_the_windows_first_row() {
    let s = Store::open_in_memory().unwrap();
    seed(&s, 12);

    let got = s
        .observations_between(ACCOUNT, AXIS, SINCE, UNTIL, 1_000)
        .unwrap();

    assert_eq!(got.len(), 12, "nothing may be dropped below the cap");
    assert_eq!(
        got.first().map(|r| r.pct),
        Some(0.0),
        "an unclipped series starts at the window's first row"
    );
    assert_eq!(
        got.last().map(|r| r.pct),
        Some(11.0),
        "and ends at its last"
    );
    assert!(
        got.windows(2).all(|w| w[0].id < w[1].id),
        "ascending, as declared"
    );
}

/// The window filter still bounds the read — the `DESC` must not be read as
/// "newest rows in the TABLE". A cap larger than the window's contents may not
/// reach past `until`.
#[test]
fn the_window_still_bounds_what_the_newest_end_can_reach() {
    let s = Store::open_in_memory().unwrap();
    seed(&s, 50); // 00:00 .. 01:38, two minutes apart

    // Rows 0..=9 only: 00:00 through 00:18.
    let got = s
        .observations_between(ACCOUNT, AXIS, "2026-10-01T00:00:00Z", "2026-10-01T00:18:00Z", 40)
        .unwrap();

    assert_eq!(got.len(), 10, "the window admits ten rows, the cap forty");
    assert_eq!(
        got.last().map(|r| r.pct),
        Some(9.0),
        "the newest row IN THE WINDOW, never the newest in the table"
    );
    assert_eq!(
        got.first().map(|r| r.pct),
        Some(0.0),
        "and the window's oldest, since nothing was clipped"
    );
}
