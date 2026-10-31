//! **R9.PR1 — ONE PRESSURE FUNCTION, PUBLISHED PER EMAIL.**
//!
//! `pressure = ((cap − u) / cap) / t`, `t = (weekly_reset_at − now) / 7d`,
//! `cap = min(share, 0.95)` — `threads::budgets::pressure_value`. Placement
//! ranks it (`budgets.rs` unit tests `r9pr1_*`); the accounts shape publishes
//! it under `shared.by_email.<email>` (the shape test below). Every assertion
//! here was forced red; forcings and red runs are in substrate
//! an internal report §5.

#[path = "support/keychain.rs"]
mod keychain;

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use threads::budgets::{
    pressure_cap, pressure_value, Budgets, PressureValue, PRESSURE_INFINITE_WITHIN_SECS,
};
use threads::gauge::Axis;
use threads::store::Store;

const WEEK: i64 = 7 * 24 * 3600;

fn now() -> DateTime<Utc> {
    "2026-10-21T12:00:00Z".parse().unwrap()
}

fn finite(v: Option<PressureValue>) -> f64 {
    match v {
        Some(PressureValue::Finite(x)) => x,
        other => panic!("expected a finite value, got {other:?}"),
    }
}

fn close(got: f64, want: f64, what: &str) {
    assert!((got - want).abs() < 1e-9, "{what}: want {want}, got {got}");
}

/// The value at `secs_left` seconds before the reset.
fn at(cap: f64, used: f64, secs_left: i64) -> Option<PressureValue> {
    pressure_value(cap, used, now() + Duration::seconds(secs_left), now())
}

// ── (e) THE FORMULA'S SHAPE, AT cap = 1.0 ──────────────────────────────────
//
// The operator's four numbers. They hold for the FORMULA at cap = 1.0; the
// production cap for a 1.0 share is 0.95 (test (c)) — the conflict REV 2
// item 4 rules on. Opposed: a constant cannot produce 1.0, 1.6, 0.4 and 6.0,
// and a pool-relative value (one per pool) cannot either.

#[test]
fn e_half_used_at_midweek_is_one() {
    close(finite(at(1.0, 0.5, WEEK / 2)), 1.0, "50% at midweek");
}

#[test]
fn e_twenty_percent_used_at_midweek_is_one_point_six() {
    close(finite(at(1.0, 0.2, WEEK / 2)), 1.6, "20% at midweek");
}

#[test]
fn e_eighty_percent_used_at_midweek_is_point_four() {
    close(finite(at(1.0, 0.8, WEEK / 2)), 0.4, "80% at midweek");
}

#[test]
fn e_seventy_percent_used_with_five_percent_left_is_six() {
    close(finite(at(1.0, 0.7, WEEK / 20)), 6.0, "70% with 5% of the week left");
}

// ── (d) ∞ AT THE RESET ──────────────────────────────────────────────────────

/// ∞ at the reset, and inside the final minute — and NOT one second outside
/// it, and NOT for an account with no room left under its cap (which must not
/// top a ranking because its clock ran out).
#[test]
fn d_at_the_reset_the_value_is_infinite_and_only_there() {
    assert_eq!(at(1.0, 0.5, 0), Some(PressureValue::Infinite), "at reset");
    assert_eq!(at(0.95, 0.5, -10), Some(PressureValue::Infinite), "past reset");
    assert_eq!(
        at(0.95, 0.5, PRESSURE_INFINITE_WITHIN_SECS),
        Some(PressureValue::Infinite),
        "at the epsilon"
    );
    assert!(
        matches!(at(0.95, 0.5, PRESSURE_INFINITE_WITHIN_SECS + 1), Some(PressureValue::Finite(_))),
        "one second outside the epsilon is a number"
    );
    assert!(
        matches!(at(0.95, 0.95, 0), Some(PressureValue::Finite(_))),
        "no room under the cap: never ∞"
    );
}

/// The same through the reading the daemon publishes: 30 s left → ∞, and the
/// wire row says so with `infinite: true` beside a `null` number.
#[test]
fn d_the_reading_publishes_infinite_inside_the_last_minute() {
    let b = Budgets::new(Store::open_in_memory().unwrap());
    let n = now();
    b.observe(
        "edge",
        Axis::WeeklyAll,
        40.0,
        Some((n + Duration::seconds(30)).to_rfc3339()),
        "usage_poll",
    )
    .unwrap();
    assert_eq!(b.pressure_reading("edge", n).value, Some(PressureValue::Infinite));
    let rows = b.pressure_rows(&["edge".to_string()], n);
    let row = &rows["accounts"][0];
    assert_eq!(row["infinite"], json!(true), "{row}");
    assert!(row["pressure"].is_null(), "{row}");
    assert_eq!(row["ranked"], json!(true), "{row}");
}

// ── (c) THE 95% CAP ─────────────────────────────────────────────────────────

#[test]
fn c_the_cap_is_the_share_or_the_95_percent_cutoff_whichever_is_lower() {
    close(pressure_cap(1.0), 0.95, "share 1.0");
    close(pressure_cap(0.9), 0.9, "share 0.9");
    close(pressure_cap(0.97), 0.95, "share 0.97");
}

/// Share 1.0 under the PRODUCTION cap: 50% at midweek is 0.947…, not 1.0,
/// and 70% with 5% left is 5.263…, not 6.0 — measured through the reading.
#[test]
fn c_a_whole_share_is_measured_against_the_95_percent_cutoff() {
    let b = Budgets::new(Store::open_in_memory().unwrap());
    let n = now();
    for (a, pct, left) in [("mid", 50.0, WEEK / 2), ("late", 70.0, WEEK / 20)] {
        b.observe(
            a,
            Axis::WeeklyAll,
            pct,
            Some((n + Duration::seconds(left)).to_rfc3339()),
            "usage_poll",
        )
        .unwrap();
    }
    close(
        finite(b.pressure_reading("mid", n).value),
        (0.45 / 0.95) / 0.5,
        "share 1.0, 50% at midweek",
    );
    close(
        finite(b.pressure_reading("late", n).value),
        (0.25 / 0.95) / 0.05,
        "share 1.0, 70% with 5% left",
    );
}

// ── (b) THE SHARE CASE (PRESSURE-compare.md counterexample 1) ───────────────

/// Share 0.9, 85% used, 1.4 days (t = 0.2) left: `((0.9 − 0.85)/0.9)/0.2` =
/// 0.2777… — the tokens available to the POOL, not the account's total (the
/// absolute `(1 − u)/t` reads 0.75).
#[test]
fn b_share_point_nine_eighty_five_percent_used() {
    close(finite(at(pressure_cap(0.9), 0.85, WEEK / 5)), (0.05 / 0.9) / 0.2, "pure");
    let b = Budgets::new(Store::open_in_memory().unwrap());
    let n = now();
    b.observe(
        "gmail",
        Axis::WeeklyAll,
        85.0,
        Some((n + Duration::seconds(WEEK / 5)).to_rfc3339()),
        "usage_poll",
    )
    .unwrap();
    b.store_ref().set_pool_share("gmail", 0.9, None).unwrap();
    close(
        finite(b.pressure_reading("gmail", n).value),
        (0.05 / 0.9) / 0.2,
        "through the reading",
    );
}

// ── (a) THE SHAPE PUBLISHES THE SAME VALUE, PER EMAIL ───────────────────────

async fn subscribe_accounts(
    addr: std::net::SocketAddr,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    use tokio_tungstenite::tungstenite::Message;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap();
    let _welcome = ws.next().await;
    ws.send(Message::Text(
        json!({"type":"request","id":"1","method":"shapes/accounts/subscribe",
               "params":{"consumer":{"name":"threads-tests","shape_version":"shapes.md@rev4"}}})
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    ws
}

/// Every `shared.by_email` seen on the link within the quiet window, latest last.
async fn by_email_frames(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Vec<Value> {
    use tokio_tungstenite::tungstenite::Message;
    let mut out = Vec::new();
    loop {
        let got = tokio::time::timeout(std::time::Duration::from_millis(900), ws.next()).await;
        let Ok(Some(Ok(Message::Text(t)))) = got else {
            return out;
        };
        let v: Value = serde_json::from_str(&t).unwrap();
        if v["type"] == "shape" && v["shape"] == "accounts" {
            // An export carries `shared`; a live `shared` event carries it in
            // `event.shared` (`shapewire::wire`, kind "shared").
            let shared = if v["event"]["kind"] == "shared" { &v["event"]["shared"] } else { &v["shared"] };
            if let Some(m) = shared.get("by_email") {
                if m.is_object() {
                    out.push(m.clone());
                }
            }
        }
    }
}

/// **`accounts.by_email.<email>.pressure` IS PLACEMENT'S VALUE, PER ACCOUNT.**
/// Two accounts, two emails (from `login-hint`), two shares (0.9 and 1.0),
/// two different values. For each, the published entry equals
/// `pressure_reading(..).value` at the entry's own `as_of` — the value
/// `pool_pressure` ranks (`budgets.rs` `r9pr1_placement_and_the_published_value_are_one_number_per_account`).
///
/// THE MUTATIONS: (1) drop the `by_email` update from the `Fact::Pressure`
/// fold arm — no entry carries the new values; (2) key it by the account
/// name instead of the email — the email lookups miss.
#[tokio::test]
async fn a_the_shape_publishes_placements_value_under_each_email() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.path().join("claude-home"));
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    let mut accounts = BTreeMap::new();
    for (name, email) in [("first", "First@Example.com"), ("second", "second@example.com")] {
        let dir = tmp.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("login-hint"), format!("{email}\n")).unwrap();
        accounts.insert(name.to_string(), dir.to_string_lossy().to_string());
    }
    let config = threads::config::Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    };
    threads::clock::testing::ticking();
    keychain::install();
    let m = threads::build_manager(config).unwrap();
    let (addr, _h) =
        threads::chassis::serve(std::sync::Arc::new(m.clone()), "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
    let mut ws = subscribe_accounts(addr).await;
    let _ = by_email_frames(&mut ws).await;

    let n = Utc::now();
    m.store.set_pool_share("first", 0.9, Some("t")).unwrap();
    for (a, pct, left) in [("first", 85.0, WEEK / 5), ("second", 50.0, WEEK / 2)] {
        m.budgets
            .observe(
                a,
                Axis::WeeklyAll,
                pct,
                Some((n + Duration::seconds(left)).to_rfc3339()),
                "usage_poll",
            )
            .unwrap();
    }
    m.produce_pressure();
    let frames = by_email_frames(&mut ws).await;
    let last = frames.last().unwrap_or_else(|| panic!("no by_email on the link"));

    let mut seen = Vec::new();
    for (account, email) in [("first", "first@example.com"), ("second", "second@example.com")] {
        let e = &last[email];
        assert_eq!(e["account"], json!(account), "{email} → {account}: {last}");
        assert_eq!(e["infinite"], json!(false), "{last}");
        let as_of: DateTime<Utc> = e["as_of"].as_str().unwrap().parse().unwrap();
        let placement = finite(m.budgets.pressure_reading(account, as_of).value);
        let published = e["pressure"].as_f64().unwrap_or_else(|| panic!("{last}"));
        assert_eq!(published, placement, "{email}: shape {published}, placement {placement}");
        seen.push(published);
    }
    assert!(
        (seen[0] - seen[1]).abs() > 0.1,
        "two accounts, two shares, two values — per account, not per pool: {seen:?}"
    );
    assert!(
        last.get("first").is_none() && last.get("second").is_none(),
        "keyed by EMAIL, never by account name: {last}"
    );
    drop(tmp);
}
