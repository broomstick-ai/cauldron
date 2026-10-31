//! Context-injection unit tests (Q7/§12): message headers, queue-drain prompt
//! bodies, first-turn preamble, and the post-compaction restatement.

use chrono::{TimeZone, Utc};
use threads::context::{
    build_prompt_body, compaction_restatement, first_turn_preamble, message_header, MetadataPolicy,
    PromptSection,
};
use threads::settings::Injection;

fn ts(h: u32, m: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 27, h, m, 0).unwrap()
}

/// A policy with every injection flag at its default and nothing stated yet.
fn policy(thread_id: &str) -> MetadataPolicy<'_> {
    MetadataPolicy {
        inject: Injection::default(),
        thread_id,
        state_thread_id: false,
        context_id: None,
        state_context_id: false,
        last_date: None,
        now: ts(23, 59),
        app: None,
    }
}

fn section(message_id: u64, ts: chrono::DateTime<Utc>, text: &str) -> PromptSection {
    PromptSection {
        message_id,
        ts,
        text: text.into(),
        duration: None,
        idx: None,
        headers: Vec::new(),
    }
}

#[test]
fn header_is_id_date_time_only() {
    // Q7: "the agent-facing header needs only: thread id, message id, date +
    // time" — no project=, no path= (both deliberately dropped).
    let h = message_header(41, ts(15, 30));
    assert!(h.starts_with("[#41 "));
    assert!(h.ends_with(']'));
    assert!(h.contains("2026-07-2")); // local-tz day may differ by one
    assert!(!h.contains("project="));
    assert!(!h.contains("path="));
}

#[test]
fn single_and_combined_bodies() {
    let one = vec![section(7, ts(10, 0), "just one")];
    let (body, _) = build_prompt_body(&one, &policy("t"));
    assert!(body.contains("message: #7"));
    assert!(body.ends_with("just one"));
    assert!(!body.contains("Queued messages:"));

    let many = vec![section(7, ts(10, 0), "first"), section(8, ts(10, 5), "second")];
    let (body, _) = build_prompt_body(&many, &policy("t"));
    assert!(body.starts_with("Queued messages:"));
    assert!(body.contains("message: #7"));
    assert!(body.contains("message: #8"));
    let p7 = body.find("message: #7").unwrap();
    let p8 = body.find("message: #8").unwrap();
    assert!(p7 < p8, "queue order preserved");
}

#[test]
fn first_turn_preamble_names_thread_and_plugin_paths() {
    let pre = first_turn_preamble(
        "uuid-x",
        &["/proj/.mind/plugins/tools".into(), "/repo/core-plugins/core".into()],
    );
    assert!(pre.contains("[thread uuid-x]"));
    // §12: "the agent MUST be told each plugin's path so it can edit the
    // plugin when asked".
    assert!(pre.contains("/proj/.mind/plugins/tools"));
    assert!(pre.contains("/repo/core-plugins/core"));
    assert!(pre.to_lowercase().contains("editable"));

    let bare = first_turn_preamble("uuid-y", &[]);
    assert_eq!(bare, "[thread uuid-y]");
}

#[test]
fn compaction_restatement_carries_everything_locked_in_s12() {
    let text = compaction_restatement(
        "uuid-z",
        "/work/project",
        &["/plugins/a".into()],
        Some((12, 87)),
        ts(9, 15),
    );
    // §12: working path, plugin paths, thread id, date+times, and the
    // pre-compaction message-id RANGE.
    assert!(text.contains("uuid-z"));
    assert!(text.contains("/work/project"));
    assert!(text.contains("/plugins/a"));
    assert!(text.contains("#12"));
    assert!(text.contains("#87"));
    assert!(text.contains("2026-07-2"));

    // Range unknown (imported history) → the line is simply absent.
    let text = compaction_restatement("uuid-z", "/w", &[], None, ts(9, 15));
    assert!(!text.contains("message ids #"));
}
