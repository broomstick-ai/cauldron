//! **V1.5: THE FIXTURE SEED'S ACCOUNTS, AS THE DAEMON WOULD SAY THEM.**
//!
//! Status › Tokens no longer reads the seed's `global.json` accounts: it draws
//! the daemon's `accounts` pool off the face's one connection, and Status's
//! running and parked counts are known only while that connection is UP. The
//! tests that pinned the old fake board keep their fixture by standing in for
//! the daemon here: a DETACHED `Live` whose link reads up, with the seed's
//! accounts folded into its mirror as `accounts` exports (and an empty
//! `threads/non-idle` roster) through `Live::fold_by_hand` — the real fold,
//! no socket.
//!
//! The daemon's `pressure` cell is written here from the seed's own weekly
//! reading (`dash::Account::pressure`): this file plays the PRODUCER, which
//! is where pressure is worked out; the face only draws it.

use chrono::Utc;
use serde_json::{json, Value};
use tui::cauldron::app::App;
use tui::cauldron::live::Live;

const AT: &str = "2026-10-31T11:00:00Z";

fn known() -> Value {
    json!({"state": "known", "complete": true, "folded_at": AT})
}

fn epoch() -> Value {
    json!({"init_at": AT, "reinit": 0, "lost": 0, "state": "known"})
}

/// The seed's axis name → the daemon's `gauge` axis.
fn axis(a: &str) -> &str {
    match a {
        "weekly" => "weekly-all",
        "fable" => "weekly-fable",
        other => other,
    }
}

/// The seed's accounts as `accounts` exports, their resets that far from now.
pub fn frames(a: &App) -> Vec<Value> {
    frames_with(a, |_| {})
}

/// [`frames`], each account's export handed to `edit` before it is folded —
/// for a test that needs the daemon to say something the seed would not.
pub fn frames_with(a: &App, edit: impl Fn(&mut Value)) -> Vec<Value> {
    let now = Utc::now();
    let mut out = Vec::new();
    let names: Vec<String> = a.dash.threads.accounts.iter().map(|x| x.label.clone()).collect();
    out.push(json!({"type": "shape", "shape": "accounts", "op": "read", "as_of": AT, "epoch": epoch(),
        "members": {"state": "known", "keys": names, "complete": true}}));
    for acct in &a.dash.threads.accounts {
        let gauge: serde_json::Map<String, Value> = acct
            .axes
            .iter()
            .map(|x| {
                // 30 s of slack, so a reset drawn a moment later still reads
                // the seed's `1h 05m`.
                let secs = tui::cauldron::dash::hours_of(&x.resets).map_or(0.0, |h| h * 3600.0) + 30.0;
                let at = now + chrono::TimeDelta::milliseconds((secs * 1000.0) as i64);
                (axis(&x.axis).to_string(), json!({"observed_pct": x.pct as f64, "resets_at": at.to_rfc3339(), "confidence": 1.0}))
            })
            .collect();
        // The daemon's `limit` cell: as the PRODUCER, this file holds an
        // account at its limit when an axis reads 100 %, until that axis
        // resets — the face reads `limit`, never the gauge (V1-CLEAN1).
        let until = gauge.values().find(|g| g["observed_pct"].as_f64().is_some_and(|p| p >= 100.0)).map(|g| g["resets_at"].clone()).unwrap_or(Value::Null);
        let slot = if acct.login == "EXPIRED" { "expired" } else { "live" };
        // The seed's `4d` login: the daemon's refresh expiry that far ahead.
        let expires = tui::cauldron::dash::hours_of(&acct.login).map(|h| (now + chrono::TimeDelta::milliseconds((h * 3600.0 * 1000.0) as i64 + 30_000)).timestamp_millis());
        let cells: serde_json::Map<String, Value> =
            ["registered", "share", "disabled", "gauge", "limit", "health", "credential", "pressure"].iter().map(|c| (c.to_string(), known())).collect();
        let mut export = json!({
            "type": "shape", "shape": "accounts", "key": acct.label, "op": "read", "as_of": AT, "epoch": epoch(),
            "cells": cells,
            "row": {
                "account": acct.label, "registered": true,
                "share": {"share": 0.5}, "disabled": {"disabled": false},
                "gauge": gauge, "limit": {"until": until}, "health": {"dead_since": null},
                "credential": {"slot_state": slot, "is_live_login": false, "subscription_type": acct.subscription, "refresh_expires_at_ms": expires},
                "pressure": {"pressure": acct.pressure(), "share": 0.5},
            },
        });
        edit(&mut export);
        out.push(export);
    }
    out.push(json!({"type": "shape", "shape": "threads/non-idle", "key": "fleet", "op": "read", "as_of": AT, "epoch": epoch(),
        "cells": {"threads": known()}, "row": {"pool": "fleet", "threads": []}}));
    out
}

/// Put the stand-in daemon behind `a`: its link UP, the seed's accounts its
/// pool. Call after the App is opened (and again after anything replaces its
/// `live`).
pub fn seed_live(a: &mut App) {
    seed_live_with(a, |_| {})
}

/// [`seed_live`], each account's export edited first ([`frames_with`]).
pub fn seed_live_with(a: &mut App, edit: impl Fn(&mut Value)) {
    let mut live = Live::detached(String::new());
    for f in frames_with(a, edit) {
        live.fold_by_hand(&f);
    }
    live.store.link = tui_render::LinkState::Connected;
    a.attach_live(live);
}

/// `a` with [`seed_live`] behind it — for a file's own `open` helper.
pub fn seeded(mut a: App) -> App {
    seed_live(&mut a);
    a
}
