//! PL-1's guards. Every store test writes into a temp dir, drops the store,
//! DELETES `index.db` and reopens, so what is read back comes from the ledger
//! files through protoledger's own reader and rebuilt index, never from the
//! writer's memory (doctrine 3: a probe must not witness its own write).

use std::path::Path;

use serde_json::{json, Map, Value};

use super::store::{store_root_in, PlStore, SPAWNED_BY};
use super::write::{self, AppFact, NetworkJoin, PeerDirection, PeerMessage, Prompt, SettingsSnapshot, Spawn};
use super::{Event, Kind, CT};
use crate::settings::ThreadSettings;

const UPSTREAM_EXAMPLE_NS: &str = "example.orchestrator";

fn snapshot(model: &str, cwd: &str, system_prompt: Option<&str>) -> SettingsSnapshot {
    let s = ThreadSettings::new(cwd, model);
    SettingsSnapshot::of(&s, system_prompt.map(str::to_string))
}

/// Every inject flag cauldron's `Injection` serialises, by name.
fn inject_names() -> Vec<String> {
    match serde_json::to_value(crate::settings::Injection::default()).unwrap() {
        Value::Object(m) => m.keys().cloned().collect(),
        other => panic!("Injection did not serialise to an object: {other}"),
    }
}

/// Reopen from the files alone: drop, delete the derived index, open.
fn reopen_from_ledgers(store: PlStore) -> PlStore {
    let root = store.root().to_path_buf();
    drop(store);
    std::fs::remove_file(root.join("index.db")).expect("index.db existed");
    PlStore::open_at(&root).unwrap()
}

fn ct(e: &Event) -> &Value {
    e.meta.get(CT).unwrap_or_else(|| panic!("{} has no {CT} meta: {e:?}", e.id))
}

// ---- guard 1: settings snapshots are FULL ---------------------------------

fn assert_full_snapshot(e: &Event, model: &str, cwd: &str, system_prompt: Option<&str>) {
    assert_eq!(e.kind, Kind::Settings);
    assert_eq!(e.body.get("complete"), Some(&json!(true)), "a snapshot says complete: {e:?}");
    assert_eq!(e.body.get("model"), Some(&json!(model)), "model: {e:?}");
    assert_eq!(e.body.get("cwd"), Some(&json!(cwd)), "cwd: {e:?}");
    assert_eq!(
        e.body.get("system_prompt"),
        Some(&system_prompt.map(|s| json!(s)).unwrap_or(Value::Null)),
        "system_prompt present (null when none): {e:?}"
    );
    let inject = ct(e).get("inject").and_then(Value::as_object).unwrap_or_else(|| panic!("inject flags: {e:?}"));
    let names = inject_names();
    assert!(names.len() >= 10, "sentinel: Injection has its flags ({names:?})");
    for n in &names {
        assert!(inject.get(n).is_some_and(Value::is_boolean), "inject flag {n} missing from {e:?}");
    }
}

#[test]
fn settings_snapshot_carries_system_prompt_model_cwd_and_every_inject_flag() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = PlStore::open_at(&dir.path().join("protoledger")).unwrap();
    s.create_thread("t1", "2026-10-25T10:00:00Z", Map::new()).unwrap();
    s.write_settings("t1", "2026-10-25T10:00:00Z", &snapshot("claude-opus-5-5", "/w/a", Some("You are t1.")))
        .unwrap();
    // A change of the model ONLY: the second snapshot must still carry the
    // unchanged cwd, system prompt and flags (a patch would drop them).
    s.write_settings("t1", "2026-10-25T10:01:00Z", &snapshot("claude-sonnet-5", "/w/a", Some("You are t1.")))
        .unwrap();
    // No system prompt: still present, as null.
    s.write_settings("t1", "2026-10-25T10:02:00Z", &snapshot("claude-sonnet-5", "/w/b", None)).unwrap();
    let s = reopen_from_ledgers(s);
    let ev = s.read_events("t1").unwrap();
    assert_eq!(ev.len(), 3, "{ev:?}");
    assert_full_snapshot(&ev[0], "claude-opus-5-5", "/w/a", Some("You are t1."));
    assert_full_snapshot(&ev[1], "claude-sonnet-5", "/w/a", Some("You are t1."));
    assert_full_snapshot(&ev[2], "claude-sonnet-5", "/w/b", None);
}

// ---- guard 2: a spawn-time network join is its own event -------------------

fn spawn_of(id: &str, parent: &str, networks: &[&str]) -> Spawn {
    Spawn {
        thread_id: id.into(),
        created_at: "2026-10-25T10:00:00Z".into(),
        parent: parent.into(),
        parent_event: Some(format!("{parent}:call:1")),
        spawn_request_id: Some(format!("req-{id}")),
        budget_key: Some(parent.into()),
        settings: snapshot("claude-sonnet-5", "/w", Some(&format!("You are {id}."))),
        networks: networks.iter().map(|n| n.to_string()).collect(),
    }
}

fn joins(ev: &[Event]) -> Vec<(String, bool)> {
    ev.iter()
        .filter(|e| e.kind == Kind::App && e.str("type") == Some(write::network_joined_type().as_str()))
        .map(|e| {
            let d = e.body.get("data").unwrap();
            (d["network"].as_str().unwrap().to_string(), d["at_spawn"].as_bool().unwrap())
        })
        .collect()
}

#[test]
fn spawn_time_network_join_is_its_own_event_per_network_and_none_without_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = PlStore::open_at(&dir.path().join("protoledger")).unwrap();
    s.create_thread("p", "2026-10-25T09:00:00Z", Map::new()).unwrap();
    let appended = s.spawn(&spawn_of("c-net", "p", &["net-a", "net-b"])).unwrap();
    assert_eq!(appended.len(), 3, "settings + one append per network: {appended:?}");
    s.spawn(&spawn_of("c-solo", "p", &[])).unwrap();
    // A later join is the same event, not at spawn.
    s.write_network_join("c-solo", "2026-10-25T11:00:00Z", &NetworkJoin { network: "net-a".into(), at_spawn: false })
        .unwrap();
    let mut s = reopen_from_ledgers(s);

    let net = s.read_events("c-net").unwrap();
    assert_eq!(net[0].kind, Kind::Settings, "{net:?}");
    assert_eq!(joins(&net), vec![("net-a".into(), true), ("net-b".into(), true)], "{net:?}");
    // Not folded into the spawn: the header names the parent, never a network.
    let links = s.links("c-net").unwrap();
    assert_eq!(links.len(), 1, "only the spawned-by link: {links:?}");
    assert_eq!(links[0].rel, SPAWNED_BY);
    assert_eq!(links[0].agent.as_deref(), Some("p"));
    let hm = s.header_meta("c-net").unwrap();
    assert!(hm.get("networks").is_none(), "networks are events, not header meta: {hm}");
    assert_eq!(hm["parent"], json!("p"));

    // Opposition: no network at spawn => no join event at spawn.
    let solo = s.read_events("c-solo").unwrap();
    assert_eq!(joins(&solo), vec![("net-a".into(), false)], "{solo:?}");
}

// ---- guard 3: the namespace is cauldron.threads -----------------------------

/// Every namespace an event uses: its meta keys and its app type's prefix.
fn namespaces(e: &Event) -> Vec<String> {
    let mut v: Vec<String> = e.meta.keys().cloned().collect();
    if let Some(t) = e.str("type") {
        v.push(t.split('/').next().unwrap_or(t).to_string());
    }
    v
}

#[test]
fn namespace_is_cauldron_threads_never_the_upstream_fixture_s() {
    assert_eq!(CT, "cauldron.threads");
    // Sentinel: the predicate finds the upstream namespace when it IS there
    // (an event shaped like the fixture's).
    let fixture_like = Event::new("x", Kind::App)
        .with("type", format!("{UPSTREAM_EXAMPLE_NS}/summary_updated"))
        .meta_ns(UPSTREAM_EXAMPLE_NS, json!({}));
    assert_eq!(namespaces(&fixture_like), vec![UPSTREAM_EXAMPLE_NS.to_string(), UPSTREAM_EXAMPLE_NS.to_string()]);

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("protoledger");
    let mut s = PlStore::open_at(&root).unwrap();
    write_everything(&mut s);
    let s = reopen_from_ledgers(s);
    let mut seen = 0;
    for t in ["coord", "peer-1", "peer-2"] {
        for e in s.read_events(t).unwrap() {
            for ns in namespaces(&e) {
                assert_eq!(ns, CT, "{t}: {e:?}");
                seen += 1;
            }
        }
    }
    assert!(seen >= 10, "the writers wrote namespaced facts ({seen})");
    // Headers too, and nothing anywhere spells the upstream example.
    for f in std::fs::read_dir(root.join("ledgers")).unwrap() {
        let text = std::fs::read_to_string(f.unwrap().path()).unwrap();
        let header: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        if let Some(m) = header["agent"]["meta"].as_object() {
            assert!(m.keys().all(|k| k == CT), "header meta namespaces: {header}");
        }
        assert!(!text.contains(UPSTREAM_EXAMPLE_NS), "upstream's example namespace leaked: {text}");
    }
}

// ---- guard 4: the round trip, N >= 2 threads --------------------------------

const TS: &str = "2026-10-25T10:00:00Z";

fn prompt(id: i64, raw: &str) -> Prompt {
    let mut headers = Map::new();
    headers.insert("x-source".into(), json!("voice"));
    Prompt {
        composed: format!("[#{id} 2026-10-25 10:00]\n{raw}"),
        raw_text: raw.into(),
        message_id: Some(id),
        queue_id: Some(40 + id),
        headers,
        bare: false,
        turn: Some("t1".into()),
    }
}

fn peer(direction: PeerDirection, peer: &str, text: &str) -> PeerMessage {
    PeerMessage {
        direction,
        peer: peer.into(),
        network: "audit-net".into(),
        text: text.into(),
        message_id: Some(2),
        queue_id: Some(57),
        mail_kind: Some("direct".into()),
        turn: Some("t2".into()),
    }
}

/// What [`write_everything`] writes, per thread, as the pure builders make it
/// (id and seq filled in as the store assigns them).
fn expected() -> Vec<(&'static str, Vec<Event>)> {
    let coord = vec![
        write::settings_event("", TS, &snapshot("claude-opus-5-5", "/w", Some("You are the coordinator."))),
        write::prompt_event("", TS, &prompt(1, "Split the audit.")),
        write::app_fact_event("", TS, &AppFact::Title { text: "Split the audit".into(), by: Some("aui".into()) }),
        write::app_fact_event(
            "",
            TS,
            &AppFact::Summary { summary: "Two peers on it.".into(), by: Some("aui".into()) },
        ),
        write::network_join_event("", TS, &NetworkJoin { network: "audit-net".into(), at_spawn: false }),
        write::peer_message_event("", TS, &peer(PeerDirection::In, "peer-1", "Rust half done.")),
        write::peer_message_event("", TS, &peer(PeerDirection::In, "peer-2", "npm half done.")),
    ];
    let child = |id: &str, text: &str| {
        let sp = spawn_of(id, "coord", &["audit-net"]);
        vec![
            write::settings_event("", &sp.created_at, &sp.settings),
            write::network_join_event("", &sp.created_at, &NetworkJoin { network: "audit-net".into(), at_spawn: true }),
            write::prompt_event("", TS, &prompt(1, &format!("Audit {id}."))),
            write::peer_message_event("", TS, &peer(PeerDirection::Out, "coord", text)),
        ]
    };
    let fill = |v: Vec<Event>| {
        v.into_iter()
            .enumerate()
            .map(|(i, mut e)| {
                e.seq = i as u64 + 1;
                e.id = format!("ct:{}:{}", write::id_word(&e), e.seq);
                e
            })
            .collect::<Vec<_>>()
    };
    vec![
        ("coord", fill(coord)),
        ("peer-1", fill(child("peer-1", "Rust half done."))),
        ("peer-2", fill(child("peer-2", "npm half done."))),
    ]
}

fn write_everything(s: &mut PlStore) {
    let mut meta = Map::new();
    meta.insert("account".into(), json!("acct-a"));
    s.create_thread("coord", TS, meta).unwrap();
    s.write_settings("coord", TS, &snapshot("claude-opus-5-5", "/w", Some("You are the coordinator."))).unwrap();
    s.write_prompt("coord", TS, &prompt(1, "Split the audit.")).unwrap();
    s.write_app("coord", TS, &AppFact::Title { text: "Split the audit".into(), by: Some("aui".into()) }).unwrap();
    s.write_app("coord", TS, &AppFact::Summary { summary: "Two peers on it.".into(), by: Some("aui".into()) })
        .unwrap();
    s.write_network_join("coord", TS, &NetworkJoin { network: "audit-net".into(), at_spawn: false }).unwrap();
    for (id, text) in [("peer-1", "Rust half done."), ("peer-2", "npm half done.")] {
        s.spawn(&spawn_of(id, "coord", &["audit-net"])).unwrap();
        s.write_prompt(id, TS, &prompt(1, &format!("Audit {id}."))).unwrap();
        s.write_peer_message(id, TS, &peer(PeerDirection::Out, "coord", text)).unwrap();
        s.write_peer_message("coord", TS, &peer(PeerDirection::In, id, text)).unwrap();
    }
}

#[test]
fn every_writer_round_trips_through_store_read_across_three_threads() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = PlStore::open_at(&dir.path().join("protoledger")).unwrap();
    write_everything(&mut s);
    let s = reopen_from_ledgers(s);

    let want = expected();
    let mut kinds = std::collections::BTreeSet::new();
    for (thread, events) in &want {
        let got = s.read_events(thread).unwrap();
        assert_eq!(&got, events, "{thread}: read back != written");
        kinds.extend(got.iter().map(|e| e.kind.as_str()));
    }
    // Every writer is represented in what was read.
    assert_eq!(
        kinds.into_iter().collect::<Vec<_>>(),
        vec!["app", "peer_message", "settings", "text", "title"],
        "each writer's kind"
    );
    let mut spawned = s.spawned_by("coord").unwrap();
    spawned.sort();
    assert_eq!(spawned, vec!["peer-1".to_string(), "peer-2".to_string()]);
    assert_eq!(s.spawned_by("peer-1").unwrap(), Vec::<String>::new(), "opposition: a leaf spawned nobody");
    // The index's title comes from the neutral `title` event.
    assert_eq!(s.store().agent("coord").unwrap().unwrap().title.as_deref(), Some("Split the audit"));
    // The composed prompt is the body; its parts are CT meta.
    let p = &s.read_events("coord").unwrap()[1];
    assert_eq!(p.str("text"), Some("[#1 2026-10-25 10:00]\nSplit the audit."));
    assert_eq!(ct(p)["raw_text"], json!("Split the audit."));
    assert_eq!(ct(p)["queue_id"], json!(41));
    assert_eq!(ct(p)["_bare"], json!(false));
}

// ---- the data-root refusal --------------------------------------------------

#[test]
fn store_root_refuses_a_data_dir_outside_the_declared_root() {
    let d = Path::new("/x/code/data/prod/threads/store");
    assert_eq!(store_root_in(d, None).unwrap(), d.join("protoledger"));
    assert_eq!(store_root_in(d, Some(Path::new("/x/code/data"))).unwrap(), d.join("protoledger"));
    assert!(store_root_in(Path::new("/tmp/t/store"), Some(Path::new("/x/code/data"))).is_err());
    assert!(store_root_in(Path::new("/x/code2/store"), Some(Path::new("/x/code"))).is_err(), "component-wise");
}

// ---- PL-1b: G1 never varied an inject flag's VALUE --------------------------
//
// The PL-1 inspect's finding: every G1 snapshot used `Injection::default()` and
// the expected names came from the writer's own serialisation, so a writer
// that ignored `settings.injection` and always wrote defaults passed. Here two
// flags are flipped AWAY from their defaults (one true→false, one false→true),
// a third is left at its default, and every value is asserted as a LITERAL
// read back from the ledger files; a second, all-default snapshot pins the
// opposite values, so neither "always defaults" nor "always the flipped set"
// passes both.

#[test]
fn settings_snapshot_carries_each_inject_flag_by_value() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = PlStore::open_at(&dir.path().join("protoledger")).unwrap();
    s.create_thread("t1", "2026-10-25T10:00:00Z", Map::new()).unwrap();
    let mut flipped = ThreadSettings::new("/w/a", "claude-opus-5-5");
    assert!(flipped.injection.first_turn_preamble && !flipped.injection.context_id, "sentinel: the defaults this test flips");
    flipped.injection.first_turn_preamble = false;
    flipped.injection.context_id = true;
    s.write_settings("t1", "2026-10-25T10:00:00Z", &SettingsSnapshot::of(&flipped, None)).unwrap();
    let plain = ThreadSettings::new("/w/a", "claude-opus-5-5");
    s.write_settings("t1", "2026-10-25T10:01:00Z", &SettingsSnapshot::of(&plain, None)).unwrap();
    let s = reopen_from_ledgers(s);
    let ev = s.read_events("t1").unwrap();
    assert_eq!(ev.len(), 2, "{ev:?}");
    let flag = |e: &Event, name: &str| ct(e).get("inject").and_then(|i| i.get(name)).cloned();
    assert_eq!(flag(&ev[0], "inject_first_turn_preamble"), Some(json!(false)), "flipped true→false: {:?}", ev[0]);
    assert_eq!(flag(&ev[0], "inject_context_id"), Some(json!(true)), "flipped false→true: {:?}", ev[0]);
    assert_eq!(flag(&ev[0], "inject_system_notices"), Some(json!(true)), "left at its default: {:?}", ev[0]);
    assert_eq!(flag(&ev[1], "inject_first_turn_preamble"), Some(json!(true)), "defaults again: {:?}", ev[1]);
    assert_eq!(flag(&ev[1], "inject_context_id"), Some(json!(false)), "defaults again: {:?}", ev[1]);
}

// ---- PL-3: the carry --------------------------------------------------------

use super::shadow::{carry, conversation_drift, enabled, Conv};

fn line(uuid: &str, text: &str) -> String {
    format!("{{\"type\":\"assistant\",\"uuid\":\"{uuid}\",\"text\":\"{text}\"}}")
}

#[test]
fn carry_takes_exactly_the_lines_after_the_last_shared_record() {
    let (a, b, c, d) = (line("u-a", "a"), line("u-b", "b"), line("u-c", "c"), line("u-d", "d"));
    // The projection holds a and b, plus a line of its own (cauldron's prompt).
    let shadow = format!("{a}\n{}\n{b}\n", line("ct-1", "prompt"));
    // Claude's file: a, b (b rewritten in place by the scrub: same uuid), c, d,
    // then a torn final line.
    let b_scrubbed = line("u-b", "b (scrubbed)");
    let real = format!("{a}\n{b_scrubbed}\n{c}\n{d}\n{{\"type\":\"assist");
    assert_eq!(carry(&shadow, &real), format!("{c}\n{d}\n"));
    // Nothing new: nothing carried (and a second carry after the first is empty).
    let after = format!("{shadow}{c}\n{d}\n");
    assert_eq!(carry(&after, &real), "");
    // No record in common (a new thread's empty projection): the whole file.
    assert_eq!(carry("", &format!("{a}\n{b}\n")), format!("{a}\n{b}\n"));
    // A uuid-less trailing line already carried is not carried twice.
    let tail = "{\"type\":\"last-prompt\"}";
    assert_eq!(carry(&format!("{after}{tail}\n"), &format!("{a}\n{c}\n{d}\n{tail}\n")), "");
}

#[test]
fn the_flag_is_exactly_one_and_drift_is_any_difference() {
    assert!(enabled(Some("1")));
    for v in [None, Some(""), Some("0"), Some("true"), Some("yes")] {
        assert!(!enabled(v), "{v:?} must not turn the shadow on");
    }
    let x = vec![Conv::AssistantText("hi".into()), Conv::ToolCall { id: "t1".into(), name: "Read".into() }];
    assert_eq!(conversation_drift(&x, &x.clone()), None);
    assert!(conversation_drift(&x, &x[..1]).is_some(), "a dropped row is drift");
    assert!(conversation_drift(&x, &[x[1].clone(), x[0].clone()]).is_some(), "a reordering is drift");
}
