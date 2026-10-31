//! **THE CRATE DEFAULT IS THE AUI DEFAULT.**
//!
//! AUI's `build_thread_settings`
//! (`~/code/harness/apps/aui/src/threads_client.rs`) builds its settings blob
//! by hand and sends TEN keys: `cwd`, `model`, `account`,
//! `append_system_prompt`, `add_dirs`, `permission_mode`, `plugin_roots`,
//! `inject_context`, `app_id`, `metadata`. Every other field of
//! [`ThreadSettings`] arrives absent and lands on its serde default.
//!
//! That makes the `#[serde(default = "default_true")]` on
//! `scrub_api_error_details` the thing the operator's audio interface actually
//! runs with — there is no second place to set it, and no way to forget to.
//! The operator said the flag defaults true AND that "our audio user interface
//! will also call it as true"; on this wiring those are the same sentence, and
//! this test is what keeps them the same sentence if AUI's blob ever changes.

use threads::settings::ThreadSettings;

/// AUI's blob, key for key, as `build_thread_settings` emits it.
fn aui_blob() -> serde_json::Value {
    serde_json::json!({
        "cwd": "/Users/testuser/minds/mythcellaneous",
        "model": "opus",
        "account": "auto",
        "append_system_prompt": "you are the audio interface",
        "add_dirs": ["/Users/testuser/minds"],
        "permission_mode": serde_json::Value::Null,
        "plugin_roots": [
            "/Users/testuser/minds/mythcellaneous/.mind/plugins",
            "/Users/testuser/code/harness/core-plugins"
        ],
        "inject_context": true,
        "app_id": "aui",
        "metadata": { "aui_project": "mythcellaneous" }
    })
}

#[test]
fn aui_s_exact_settings_blob_defaults_the_scrub_flag_on() {
    let blob = aui_blob();
    assert_eq!(
        blob.as_object().unwrap().len(),
        10,
        "AUI sends exactly ten keys; if this count moved, re-read \
         build_thread_settings before touching the assertion below"
    );
    assert!(
        !blob.as_object().unwrap().contains_key("scrub_api_error_details"),
        "AUI does not send the flag — that is WHY the crate default is the AUI default"
    );

    let s: ThreadSettings = serde_json::from_value(blob).expect("AUI's blob must deserialize");
    assert!(
        s.scrub_api_error_details,
        "an AUI thread scrubs API-error details by default"
    );

    // The other defaults AUI relies on, pinned in the same breath so a change
    // to one is not mistaken for a change to all.
    assert!(s.limit_kill_mid_turn, "the mid-turn limit interrupt (df4ae3a)");
    // AUI sends `inject_context: true` explicitly. It is RETIRED as a control —
    // one flag per injected thing now (`settings::Injection`) — but still
    // accepted, and `true` means "inject normally", so it must land on exactly
    // the crate defaults. If this ever stopped being a no-op, every AUI thread's
    // injection would change on the strength of a key AUI has always sent.
    assert_eq!(
        s.injection,
        threads::settings::Injection::default(),
        "`inject_context: true` is a no-op translation, not a reconfiguration"
    );
    assert_eq!(s.app_id.as_deref(), Some("aui"));
    // …and because `app_id` says `aui`, the CREATOR DEFAULT raises the context
    // id on this thread. This is the one place AUI's blob gains something it did
    // not ask for, and it is the operator's instruction: "I mean threads started
    // from AUI and TUI."
    let mut with_defaults = s.clone();
    with_defaults.apply_creator_defaults(&aui_blob());
    assert!(
        with_defaults.injection.context_id,
        "an AUI thread gets the context id without AUI sending the key"
    );
}

/// A settings blob written before the flag existed — every thread already in
/// the operator's database — reads back with the scrub ON. This is the whole
/// backfill: no migration script, no daemon restart.
#[test]
fn a_settings_blob_from_before_the_flag_existed_scrubs() {
    let old = serde_json::json!({ "cwd": "/tmp", "model": "sonnet" });
    let s: ThreadSettings = serde_json::from_value(old).unwrap();
    assert!(s.scrub_api_error_details);
}

/// It is a FLAG, not a constant: it must survive a round trip in both
/// directions, and be patchable over `threads.update`.
#[test]
fn the_flag_can_be_turned_off_deliberately_and_survives_a_round_trip() {
    let off = serde_json::json!({
        "cwd": "/tmp", "model": "sonnet", "scrub_api_error_details": false
    });
    let s: ThreadSettings = serde_json::from_value(off).unwrap();
    assert!(!s.scrub_api_error_details, "an explicit false is honoured");

    let round: ThreadSettings = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
    assert!(!round.scrub_api_error_details, "…and it persists");

    // …and `threads.update` can put it back. Both this field and
    // `limit_kill_mid_turn` were missing from `ThreadSettings::apply`, which
    // made their own doc comments ("settable per thread over threads.update")
    // untrue; this pins the wiring.
    let mut s = s;
    let patch: threads::settings::SettingsPatch = serde_json::from_value(serde_json::json!({
        "scrub_api_error_details": true,
        "limit_kill_mid_turn": false
    }))
    .unwrap();
    let applied = s.apply(patch);
    assert!(s.scrub_api_error_details);
    assert!(!s.limit_kill_mid_turn);
    assert_eq!(applied["scrub_api_error_details"], true);
    assert_eq!(applied["limit_kill_mid_turn"], false);
}
