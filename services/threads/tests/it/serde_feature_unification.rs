//! **PL-0: `serde_json` feature unification, regression probes.**
//!
//! The revision-1 pin (protoledger `5cf091b`, branch `r31/pl0-import`) forced
//! `serde_json/arbitrary_precision` and `preserve_order` on every crate in the
//! build. Under `arbitrary_precision` a float reaching serde's buffered paths
//! (`#[serde(flatten)]`, `#[serde(untagged)]`, `#[serde(tag = "…")]`) from TEXT
//! fails with `invalid type: map, expected f64`, and stored ledger rows stopped
//! decoding. protoledger R1.S4d (`c13d166`) dropped both: serde_json is built
//! with `raw_value` only. These probes keep that true under every future pin.
//!
//! The first two tests are the instrument check, re-scoped from the
//! revision-1 file (whose sentinels asserted the features were ON): they prove
//! the features are OFF in this binary, behaviourally, and that the hazard the
//! site tests guard against does not bite here. `tests/protoledger_features.rs`
//! checks the same thing from cargo's resolve.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// **Both features are off in this binary.** With `arbitrary_precision` a
/// number keeps its digits ("0.10" stays "0.10"); with `preserve_order` an
/// object keeps insertion order. Each is paired with its opposite so a
/// constant answer cannot pass.
#[test]
fn sentinel_arbitrary_precision_and_preserve_order_are_off_in_this_binary() {
    let n: Value = serde_json::from_str("0.10").unwrap();
    assert_eq!(n.to_string(), "0.1", "arbitrary_precision is ON in this binary");
    let o: Value = serde_json::from_str(r#"{"b":1,"a":2}"#).unwrap();
    assert_eq!(o.to_string(), r#"{"a":2,"b":1}"#, "preserve_order is ON in this binary");
}

/// **The hazard does not bite here.** A float inside an internally tagged
/// enum, read from text, round-trips. Under `arbitrary_precision` this exact
/// value fails with `expected f64` (revision-1 measurement, `9ac62491`).
#[test]
fn sentinel_a_float_in_a_tagged_enum_read_from_text_round_trips() {
    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    #[serde(tag = "kind")]
    enum Probe {
        P { x: f64, n: u64 },
    }
    let text = serde_json::to_string(&Probe::P { x: 0.25, n: 7 }).unwrap();
    let back = serde_json::from_str::<Probe>(&text).unwrap_or_else(|e| panic!("the feature bites again: {e}\n{text}"));
    assert_eq!(back, Probe::P { x: 0.25, n: 7 });
}

fn round_trip_text<T>(v: &T) -> T
where
    T: Serialize + for<'de> Deserialize<'de>,
{
    let text = serde_json::to_string(v).unwrap();
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("did not round-trip from text: {e}\n{text}"))
}

/// Site `services/threads/src/pool.rs:252` (`#[serde(flatten)] resource`, an
/// internally tagged `Resource`). The float `share` is a SIBLING of the flatten
/// and must survive the real parser, `Pool::from_json`.
#[test]
fn pool_entry_flatten_round_trips_a_fractional_share() {
    use threads::pool::{Pool, PoolEntry};
    let mut e = PoolEntry::claude_account("a", "/tmp/pl0-a").unwrap();
    e.share = Some(0.35);
    let pool = Pool { entries: vec![e] };
    let text = pool.to_json().unwrap();
    let back = Pool::from_json(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert_eq!(back, pool);
    assert_eq!(back.entries[0].share, Some(0.35));
}

/// Sites `services/threads/src/settings.rs:353` (`ThreadSettings.injection`) and
/// `:1195` (`SettingsPatch.injection`). The settings blob carries floats beside
/// the flatten: `max_budget_usd` and every `thresholds` axis.
#[test]
fn thread_settings_flatten_round_trips_floats_beside_the_injection_flags() {
    use threads::{Thresholds, ThreadSettings};
    let mut s = ThreadSettings::new("/tmp/pl0", "opus");
    s.max_budget_usd = Some(2.75);
    s.thresholds = Thresholds { session: Some(0.85), fable: Some(0.5), weekly: Some(95.5), cash: Some(12.25) };
    s.injection.thread_id = false;
    let back = round_trip_text(&s);
    assert_eq!(back, s);
    assert_eq!(back.thresholds.weekly, Some(95.5));
    assert!(!back.injection.thread_id, "the flattened flag came back");
}

#[test]
fn settings_patch_flatten_reads_floats_and_flags_from_text() {
    use threads::settings::SettingsPatch;
    let p: SettingsPatch = serde_json::from_str(
        r#"{"max_budget_usd":1.5,"thresholds":{"session":0.9,"weekly":80.25},"inject_thread_id":false}"#,
    )
    .unwrap();
    assert_eq!(p.max_budget_usd, Some(Some(1.5)));
    let t = p.thresholds.expect("thresholds");
    assert_eq!((t.session, t.weekly), (Some(0.9), Some(80.25)));
    assert_eq!(p.injection.inject_thread_id, Some(false));
}

/// Site `libs/shapewire/src/envelope.rs:578` (`HealthWire.extras`,
/// `BTreeMap<String, Value>` flattened). `/health` is read back by the
/// supervisor; a producer extra carrying a float and an integer must survive.
#[test]
fn health_wire_flattened_extras_round_trip_numbers() {
    let text = r#"{"boot_id":"b","daemon":"threads","graceful_shutdown":true,"proto":2,"status":"ok","version":"v","load":0.75,"n":3}"#;
    let h: shapewire::envelope::HealthWire = serde_json::from_str(text).unwrap();
    assert_eq!(h.extras.get("load"), Some(&json!(0.75)));
    assert_eq!(h.extras.get("n"), Some(&json!(3)));
    assert_eq!(serde_json::to_string(&h).unwrap(), text, "health bytes moved");
}

/// Site `services/threads/src/event.rs:97` — NOT one of the plan's ten (it is
/// `#[serde(tag = "kind")]`, not flatten/untagged), but the same buffered path,
/// and it is the LEDGER: `Store::rows_to_events` (`store.rs:2204`) decodes every
/// stored row with `serde_json::from_str::<ThreadEvent>`. Each variant here
/// carries a float that real traffic writes.
#[test]
fn thread_event_rows_with_floats_decode_from_stored_text() {
    use threads::ThreadEvent;
    let rows = [
        r#"{"kind":"rate_limit_event","rate_limit_type":"five_hour","utilization":0.42,"resets_at":1760000000,"status":"allowed","is_using_overage":false}"#,
        r#"{"kind":"result_turn","subtype":"success","is_error":false,"total_cost_usd":0.1234,"num_turns":3,"duration_ms":1200,"model_usage":{"opus":{"costUSD":0.1234}}}"#,
    ];
    for row in rows {
        let ev: ThreadEvent =
            serde_json::from_str(row).unwrap_or_else(|e| panic!("a stored row no longer decodes: {e}\n{row}"));
        assert!(!matches!(ev, ThreadEvent::Unknown { .. }), "{row}");
        let back = round_trip_text(&ev);
        assert_eq!(back, ev);
    }
}
