//! The writer: one thread per open cauldron, the lock, the proofs, and what
//! every write answers and records.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use soup_door::grants::Scope;

use crate::support::*;

fn task(label: &str, handle: &str, region: &str) -> Value {
    json!({"id": label, "type": "task", "state": "ready", "handle": handle, "edges": [{"kind": "in_region", "to": region}]})
}

/// Is `<soup>/writer.lock` free (can this process take it)?
fn lock_free(s: &Scratch, c: &str) -> bool {
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(s.soup(c).join("writer.lock")).unwrap();
    f.try_lock().is_ok()
}

/// A call made right after [`lock_free`] probed the lock: a child another
/// test spawns can hold a copy of the probe's descriptor for the instant
/// between its fork and its exec, so the door's open can still find the lock
/// held (`locked`) although nothing writes. Asked again, briefly, for that
/// case only (as `Scratch::import` does); any other answer is returned.
fn call_after_probe(svc: &Service, tool: &str, args: Value, c: &Value) -> Value {
    for _ in 0..50 {
        let r = svc.call(tool, args.clone(), c);
        if r["structuredContent"]["refused"]["code"] != json!("locked") {
            return r;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    svc.call(tool, args, c)
}

#[test]
fn w1_eight_clients_twenty_puts_each_all_land_once() {
    let s = Scratch::new("w1", &["kb"]);
    s.import("kb", &example());
    let before = s.node_count("kb");
    s.grant("kb", &Scope::ALL, &[]);
    let svc = Arc::new(Service::start(&s));
    let work = s.project("p");
    let threads: Vec<_> = (0..8)
        .map(|client| {
            let svc = svc.clone();
            let work = work.clone();
            std::thread::spawn(move || {
                let mut ids = Vec::new();
                for i in 0..20 {
                    let c = json!({"client": format!("client{client}"), "cwd": work.display().to_string(), "session_id": format!("s{client}")});
                    let r = svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t", &format!("client {client} put {i}"), "R-1")]}), &c);
                    let doc = ok(&r);
                    assert_eq!(doc["outcome"], "landed", "{doc}");
                    ids.push(doc["id_map"]["t"].as_str().unwrap().to_string());
                }
                ids
            })
        })
        .collect();
    let ids: Vec<String> = threads.into_iter().flat_map(|t| t.join().expect("a client failed")).collect();
    assert_eq!(ids.len(), 160);
    assert_eq!(ids.iter().collect::<BTreeSet<_>>().len(), 160, "an id was given twice");
    assert_eq!(s.node_count("kb"), before + 160, "not every put landed exactly once");
    // One audit line per put, never record text.
    let calls = s.calls();
    let puts: Vec<&Value> = calls.iter().filter(|l| l["tool"] == "soup_put").collect();
    assert_eq!(puts.len(), 160);
    assert!(puts.iter().all(|l| l["outcome"] == "landed" && l["tx"].as_str().is_some_and(|t| t.len() == 64)));
    assert!(puts.iter().all(|l| l["counts"]["accepted"] == 1 && l["cauldron"] == "kb"));
    let opens: Vec<&Value> = calls.iter().filter(|l| l["tool"] == "open").collect();
    assert_eq!(opens.len(), 1, "the cauldron was opened once");
    assert!(opens[0]["client"].as_str().is_some_and(|c| c.starts_with("client")), "{}", opens[0]);
    let text = std::fs::read_to_string(s.mcp_dir().join("calls.jsonl")).unwrap();
    assert!(!text.contains("client 3 put"), "record text in the audit log");
    assert!(!text.contains(svc.bearer.trim_start_matches("Bearer ")), "the secret in the audit log");
    // The writer holds the lock, says so, and the session was cited.
    assert!(!lock_free(&s, "kb"), "the service does not hold the writer lock");
    let held = std::fs::read_to_string(s.soup("kb").join("writer.lock")).unwrap();
    assert_eq!(held.trim(), format!("mcp {}", svc.child.id()));
    let got = ok(&svc.call("soup_get", json!({"cauldron": "kb", "ids": [ids[0]]}), &ctx(&work)));
    let cites = &got["records"][0]["meta"]["cites"];
    assert!(cites.as_array().unwrap().iter().any(|c| c["kind"] == "session"), "{got}");
}

#[test]
fn w2_partial_dry_run_refusals_by_id_cites_and_the_duplicate_guard() {
    let s = Scratch::new("w2", &["kb"]);
    s.import("kb", &example());
    s.grant("kb", &Scope::ALL, &[]);
    let svc = Service::start(&s);
    let c = json!({"client": "test", "cwd": s.project("p").display().to_string(), "session_id": "sess-42"});
    let held = s.node_count("kb");
    let bad = vec![
        task("a", "A good task", "R-1"),
        json!({"id": "b", "type": "task", "state": "no-such-state", "handle": "A bad task", "edges": [{"kind": "in_region", "to": "R-1"}]}),
        json!({"id": "c", "type": "task", "state": "ready", "handle": "A task that refers to the bad one", "edges": [{"kind": "in_region", "to": "R-1"}, {"kind": "ref", "to": "b"}]}),
    ];

    // All or nothing: the bad record is named by id, nothing lands.
    let (code, msg, detail) = refused(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": bad}), &c));
    assert_eq!(code, "records_refused", "{msg}");
    let recs = detail["records"].as_array().unwrap();
    let b = recs.iter().find(|r| r["id"] == "b").expect("b is named");
    assert_eq!(b["disposition"], "refused");
    assert_eq!(s.node_count("kb"), held);

    // A dry run judges and lands nothing.
    let d = ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "dry_run": true, "records": [task("a", "A good task", "R-1")]}), &c));
    assert_eq!(d["outcome"], "validated");
    assert_eq!(s.node_count("kb"), held);

    // partial: `a` lands; `b` and its dependent `c` are dropped, by id.
    let d = ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "partial": true, "records": bad}), &c));
    assert_eq!(d["outcome"], "landed", "{d}");
    assert_eq!(d["accepted"], 1);
    let dropped: BTreeSet<&str> = d["dropped"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(dropped, BTreeSet::from(["b", "c"]), "{d}");
    let c_reason = d["dropped"].as_array().unwrap().iter().find(|r| r["id"] == "c").unwrap()["reasons"][0]["code"].clone();
    assert_eq!(c_reason, "target-refused", "{d}");
    assert_eq!(s.node_count("kb"), held + 1);
    let a_id = d["id_map"]["a"].as_str().unwrap().to_string();

    // The same partial batch again: already landed, refused with when.
    let (code, msg, detail) = refused(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "partial": true, "records": bad}), &c));
    assert_eq!(code, "batch_already_landed");
    assert!(msg.contains("allow_duplicate: true") && !msg.contains("--allow-duplicate"), "{msg}");
    assert!(detail["landed_before"].as_array().is_some_and(|l| !l.is_empty()));
    // The guard holds across sessions; `allow_duplicate` lands a second copy.
    let one = vec![task("x", "Cited from a url", "R-2")];
    let cite = json!({"kind": "url", "url": "https://example.com/spec"});
    let d = ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "cite": cite, "records": one}), &c));
    let x = d["id_map"]["x"].as_str().unwrap().to_string();
    let mut other = c.clone();
    other["session_id"] = json!("sess-43");
    let (code, _, _) = refused(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "cite": cite, "records": one}), &other));
    assert_eq!(code, "batch_already_landed");
    let d = ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "cite": cite, "allow_duplicate": true, "records": one}), &other));
    assert_ne!(d["id_map"]["x"].as_str().unwrap(), x);

    // The cite and the session ride in meta.cites, each once.
    let got = ok(&svc.call("soup_get", json!({"cauldron": "kb", "ids": [x]}), &c));
    let cites = got["records"][0]["meta"]["cites"].as_array().unwrap().clone();
    assert_eq!(cites, vec![cite.clone(), json!({"kind": "session", "id": "sess-42"})], "{got}");

    // soup_link: an edge between two existing nodes, read back by soup_get.
    let d = ok(&svc.call("soup_link", json!({"cauldron": "kb", "links": [{"from": a_id, "kind": "ref", "to": x, "note": "related work"}]}), &c));
    assert_eq!(d["outcome"], "linked", "{d}");
    assert_eq!(d["links"][0]["status"], "linked");
    let got = ok(&svc.call("soup_get", json!({"cauldron": "kb", "ids": [a_id]}), &c));
    let edges = got["records"][0]["edges"].as_array().unwrap();
    assert!(edges.iter().any(|e| e["kind"] == "ref" && e["to"] == x.as_str() && e["note"] == "related work"), "{got}");
    // A link to a node that does not exist, or of a kind the grammar does not
    // know, refuses the whole set and names the link.
    let n = s.node_count("kb");
    let (code, _, detail) = refused(&svc.call(
        "soup_link",
        json!({"cauldron": "kb", "links": [{"from": a_id, "kind": "ref", "to": "T-1"}, {"from": a_id, "kind": "ref", "to": "T-999"}, {"from": a_id, "kind": "loves", "to": "T-1"}]}),
        &c,
    ));
    assert_eq!(code, "links_refused");
    let ls = detail["links"].as_array().unwrap();
    assert_eq!((ls[0]["status"].as_str(), ls[1]["refused"]["reason"].as_str(), ls[2]["refused"]["reason"].as_str()), (Some("accepted"), Some("unknown-node"), Some("unknown-kind")));
    let got = ok(&svc.call("soup_get", json!({"cauldron": "kb", "ids": [a_id]}), &c));
    assert!(!got["records"][0]["edges"].as_array().unwrap().iter().any(|e| e["to"] == "T-1"), "a refused set landed a link");
    assert_eq!(s.node_count("kb"), n);
    // The grammar's own refusal of a link (a kind between types it does not join).
    let (code, _, detail) = refused(&svc.call("soup_link", json!({"cauldron": "kb", "links": [{"from": a_id, "kind": "relates", "to": "T-1"}]}), &c));
    assert_eq!(code, "links_refused");
    assert_eq!(detail["links"][0]["refused"]["reason"], "door-refused", "{detail}");

    // The audit log: every write call, refusals too, with no text.
    let calls = s.calls();
    let outcomes: Vec<&str> = calls.iter().filter(|l| l["tool"] == "soup_put").map(|l| l["outcome"].as_str().unwrap()).collect();
    assert_eq!(outcomes, ["records_refused", "validated", "landed", "batch_already_landed", "landed", "batch_already_landed", "landed"]);
    let text = std::fs::read_to_string(s.mcp_dir().join("calls.jsonl")).unwrap();
    for leak in ["A good task", "example.com", "related work"] {
        assert!(!text.contains(leak), "`{leak}` is in the audit log");
    }
}

#[test]
fn w3_revoke_closes_the_cauldron_and_frees_the_lock() {
    let s = Scratch::new("w3", &["kb"]);
    s.import("kb", &example());
    s.grant("kb", &Scope::ALL, &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    assert!(lock_free(&s, "kb"), "the service took the lock at boot");
    ok(&call_after_probe(&svc, "soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t", "Before the revoke", "R-1")]}), &c));
    assert!(!lock_free(&s, "kb"));
    assert_eq!(ok(&svc.call("soup_status", json!({"cauldron": "kb"}), &c))["writer"]["open"], true);

    // Losing `write` closes it, with no call needed.
    s.grant("kb", &[Scope::Read], &[]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !lock_free(&s, "kb") {
        assert!(std::time::Instant::now() < deadline, "the lock was not freed after the grant lost write");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(ok(&svc.call("soup_status", json!({"cauldron": "kb"}), &c))["writer"]["open"], false);
    assert_eq!(refused(&svc.call("soup_put", json!({"cauldron": "kb", "records": [task("t", "x", "R-1")]}), &c)).0, "not_granted");

    // Granted again, it opens again; revoked, it closes, and the next call is
    // refused.
    s.grant("kb", &Scope::ALL, &[]);
    ok(&call_after_probe(&svc, "soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t", "After the regrant", "R-1")]}), &c));
    assert!(!lock_free(&s, "kb"));
    s.revoke("kb");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !lock_free(&s, "kb") {
        assert!(std::time::Instant::now() < deadline, "the lock was not freed after the revoke");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(refused(&svc.call("soup_status", json!({"cauldron": "kb"}), &c)).0, "not_granted");
    let closes: Vec<Value> = s.calls().into_iter().filter(|l| l["tool"] == "close").collect();
    assert_eq!(closes.len(), 2, "{closes:?}");

    // Another writer holding the lock: the door refuses `locked`, writes nothing.
    s.grant("kb", &Scope::ALL, &[]);
    let other = std::fs::OpenOptions::new().write(true).open(s.soup("kb").join("writer.lock")).unwrap();
    other.try_lock().unwrap();
    let n = s.node_count("kb");
    assert_eq!(refused(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t", "Locked out", "R-1")]}), &c)).0, "locked");
    assert_eq!(s.node_count("kb"), n);
    drop(other);
    ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t", "Lock released", "R-1")]}), &c));

    // Stopping the service frees the lock too.
    let st = svc.stop();
    assert!(st.success(), "{st}");
    assert!(lock_free(&s, "kb"));
}

#[test]
fn w4_a_service_killed_mid_put_is_recovered_at_the_next_open() {
    let s = Scratch::new("w4", &["kb"]);
    s.import("kb", &example());
    s.grant("kb", &Scope::ALL, &[]);
    let soup = s.soup("kb");
    let svc = Arc::new(Service::start(&s));
    let c = ctx(&s.project("p"));
    // Open the writer first, so the big put below is all write.
    ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t", "Opens the writer", "R-1")]}), &c));
    let held = s.node_count("kb");

    // THE KILL: a big put, SIGKILLed the moment its transaction row is on disk.
    let big = load_batch(1500, "killed");
    let caller = {
        let svc = svc.clone();
        let c = c.clone();
        std::thread::spawn(move || std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": big}), &c))))
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    loop {
        let unrest = spine::offline::rest_check(&soup).unwrap_or_default();
        if !unrest.is_empty() {
            std::process::Command::new("/bin/kill").args(["-KILL", &svc.child.id().to_string()]).status().unwrap();
            break;
        }
        assert!(!caller.is_finished(), "the put finished before its row was seen; nothing was killed");
        assert!(std::time::Instant::now() < deadline, "the put never wrote its row");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let _ = caller.join();
    let svc = Arc::try_unwrap(svc).ok().expect("one owner");
    drop(svc);
    assert!(!spine::offline::rest_check(&soup).unwrap().is_empty(), "the killed put left the soup at rest");
    assert_eq!(s.node_count("kb"), held, "a killed put landed part of its batch");
    assert!(lock_free(&s, "kb"), "the kernel did not drop the killed holder's lock");

    // RESTART: status says the soup is not at rest; the next write opens it,
    // which recovers it; status then says what the recovery did.
    let svc = Service::start(&s);
    let st = ok(&svc.call("soup_status", json!({"cauldron": "kb"}), &c));
    assert_eq!((st["writer"]["open"].clone(), st["writer"]["at_rest"].clone()), (json!(false), json!(false)), "{st}");
    let d = ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t", "After the kill", "R-1")]}), &c));
    assert_eq!(d["outcome"], "landed");
    let st = ok(&svc.call("soup_status", json!({"cauldron": "kb"}), &c));
    let rec = &st["writer"]["recovered_at_open"];
    let after = s.node_count("kb");
    eprintln!("w4: the killed put was {}", rec["interrupted"]);
    match rec["interrupted"].as_str() {
        Some("landed") => {
            assert_eq!(rec["landed_records"], 1501, "{rec}");
            assert_eq!(after, held + 1501 + 1, "a decided put rolls forward whole");
        }
        Some("rolled-back") => {
            assert_eq!(rec["aborted"].as_array().unwrap().len(), 1, "{rec}");
            assert_eq!(after, held + 1, "nothing of the killed put landed");
        }
        other => panic!("the open recovered nothing ({other:?}): {st}"),
    }
    assert!(spine::offline::rest_check(&soup).unwrap().is_empty());
    let open = s.calls().into_iter().filter(|l| l["tool"] == "open").last().unwrap();
    assert_eq!(open["recovered_at_open"]["interrupted"], rec["interrupted"]);
}

/// The DECIDED branch of the recovery at open, made deterministic: the
/// spine's fault seam stops the service at the put's commit decision (the
/// process aborts there, as a kill would; built only under
/// `--features fault-injection`). The next start's first write opens the
/// soup, which finishes the put whole, and `recovered_at_open` says
/// `landed` with every id; the batch is then known as landed.
#[cfg(feature = "fault-injection")]
#[test]
fn w5_a_put_stopped_after_its_decision_lands_whole_at_the_next_open() {
    let s = Scratch::new("w5", &["kb"]);
    s.import("kb", &example());
    s.grant("kb", &Scope::ALL, &[]);
    let soup = s.soup("kb");
    let held = s.node_count("kb");
    let c = ctx(&s.project("p"));
    let batch = load_batch(40, "decided");

    let svc = Service::start_with(&s, &[("SPINE_FAULT", "decided")]);
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "soup_put",
        "arguments": {"cauldron": "kb", "remint": true, "records": batch, "_ctx": c}}});
    // The process aborts inside the call: the connection closes unanswered.
    let killed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| svc.post(&body.to_string(), &[])));
    let mut svc = svc;
    let st = svc.child.wait().unwrap();
    assert!(!st.success(), "the fault seam did not stop the service: {st}; {:?}", killed.as_ref().map(|r| r.status));
    assert!(svc.log().contains("SPINE_FAULT"), "{}", svc.log());
    drop(svc);
    assert!(!spine::offline::rest_check(&soup).unwrap().is_empty(), "the stopped put left the soup at rest");
    assert_eq!(s.node_count("kb"), held, "nothing lands before the recovery");
    assert!(lock_free(&s, "kb"), "the kernel did not drop the stopped holder's lock");

    let svc = Service::start(&s);
    let st = ok(&svc.call("soup_status", json!({"cauldron": "kb"}), &c));
    assert_eq!((st["writer"]["open"].clone(), st["writer"]["at_rest"].clone()), (json!(false), json!(false)), "{st}");
    // The same batch: the open recovers first, so it is refused as landed.
    let (code, _, detail) = refused(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": load_batch(40, "decided")}), &c));
    assert_eq!(code, "batch_already_landed", "{detail}");
    assert_eq!(detail["landed_before"][0]["records"], 41, "{detail}");
    let st = ok(&svc.call("soup_status", json!({"cauldron": "kb"}), &c));
    let rec = &st["writer"]["recovered_at_open"];
    assert_eq!(rec["interrupted"], "landed", "{st}");
    assert_eq!(rec["landed_records"], 41, "{rec}");
    assert_eq!(rec["rolled_forward"].as_array().unwrap().len(), 1, "{rec}");
    assert_eq!(rec["aborted"], json!([]), "{rec}");
    let tasks = rec["landed_ids"].as_array().unwrap().iter().find(|g| g["count"] == 40).unwrap_or_else(|| panic!("no group of 40: {rec}"));
    assert_eq!(tasks["contiguous"], json!(true));
    assert_eq!(s.node_count("kb"), held + 41, "a decided put rolls forward whole, once");
    assert!(spine::offline::rest_check(&soup).unwrap().is_empty());
    let open = s.calls().into_iter().filter(|l| l["tool"] == "open").last().unwrap();
    assert_eq!(open["recovered_at_open"]["interrupted"], "landed");
}

#[test]
fn w6_writes_queued_before_a_revoke_are_refused_and_none_lands_after_it() {
    let s = Scratch::new("w6", &["kb"]);
    s.import("kb", &example());
    let before = s.node_count("kb");
    s.grant("kb", &Scope::ALL, &[]);
    let svc = Arc::new(Service::start(&s));
    let work = s.project("p");
    // Eight large puts at once: one is applied, the rest wait in the queue.
    let batch = load_batch(300, "w6").len() as i64;
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let svc = svc.clone();
            let c = ctx(&work);
            std::thread::spawn(move || svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": load_batch(300, &format!("w6 client {i}"))}), &c))
        })
        .collect();
    // Revoke as soon as the first has landed.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while s.node_count("kb") == before {
        assert!(std::time::Instant::now() < deadline, "no put landed");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    s.revoke("kb");
    let at_revoke = s.node_count("kb");
    let results: Vec<Value> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    let after = s.node_count("kb");
    let landed = results.iter().filter(|r| r["isError"] == json!(false)).count() as i64;
    let refusals: Vec<(String, String, Value)> = results.iter().filter(|r| r["isError"] == json!(true)).map(refused).collect();
    eprintln!("w6: {landed} landed, {} refused; nodes {before} -> {at_revoke} at the revoke -> {after}", refusals.len());
    assert!(refusals.iter().all(|(code, _, _)| code == "not_granted"), "{refusals:?}");
    assert!(refusals.iter().all(|(_, m, _)| m.contains("Nothing from this call was written") || m.contains("Nothing was written")), "{refusals:?}");
    assert_eq!(after, before + landed * batch, "a refused put wrote something");
    // At most the one write already being applied lands after the revoke.
    assert!(after - at_revoke <= batch, "{} nodes landed after the revoke returned", after - at_revoke);
    assert!(refusals.len() >= 3, "too few puts were still queued to prove anything: {landed} landed");
    // And the lock is let go.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !lock_free(&s, "kb") {
        assert!(std::time::Instant::now() < deadline, "the lock was not freed after the revoke");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Every directory and file under `root` with its mode (links skipped).
fn modes_under(root: &std::path::Path) -> Vec<(std::path::PathBuf, bool, u32)> {
    use std::os::unix::fs::PermissionsExt;
    let mut out = vec![];
    let mut todo = vec![root.to_path_buf()];
    while let Some(d) = todo.pop() {
        out.push((d.clone(), true, std::fs::symlink_metadata(&d).unwrap().permissions().mode() & 0o777));
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let md = std::fs::symlink_metadata(e.path()).unwrap();
            if md.is_dir() {
                todo.push(e.path());
            } else if md.is_file() {
                out.push((e.path(), false, md.permissions().mode() & 0o777));
            }
        }
    }
    out.sort();
    out
}

/// A cauldron made before its data was private (directories 0755, files
/// 0644) is tightened by the service when it opens the cauldron to write:
/// every directory 0700, every file 0600. Reads leave it as it is.
#[test]
fn w7_the_service_makes_a_cauldron_it_opens_private() {
    use std::os::unix::fs::PermissionsExt;
    let s = Scratch::new("w6", &["kb"]);
    s.import("kb", &example());
    let dir = s.data().join("cauldrons").join("kb");
    for (p, d, _) in modes_under(&dir) {
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(if d { 0o755 } else { 0o644 })).unwrap();
    }
    s.grant("kb", &Scope::ALL, &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    let before = modes_under(&dir);
    assert!(ok(&svc.call("soup_nodes", json!({"cauldron": "kb"}), &c))["total"].as_u64().unwrap() > 0);
    assert_eq!(modes_under(&dir), before, "a read changed a mode");
    ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t", "Opened to write", "R-1")]}), &c));
    let after = modes_under(&dir);
    assert!(after.iter().filter(|(_, d, _)| !d).count() >= 4, "{after:?}");
    for (p, d, m) in &after {
        assert_eq!(*m, if *d { 0o700 } else { 0o600 }, "{}", p.display());
    }
    assert_eq!(svc.stop().code(), Some(0));
}

/// **`merged_from` is refused on every write path**: where a record came
/// from by a merge is written only by `cauldron merge`. A session's
/// `soup_put` (in the record, any casing, dotted or nested, and in its
/// `cite`), a link (`soup_link`), a claim (`soup_claim`), a dump's `cite`
/// (`soup_dump`), and a ripple's batch (`RPUT`): each refused naming the
/// key, and nothing is written.
#[test]
fn w8_merged_from_is_refused_on_every_write_path() {
    let s = Scratch::new("w8", &["kb"]);
    s.import("kb", &example());
    s.grant("kb", &Scope::ALL, &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    let before = s.node_count("kb");
    let from = json!({"merge": "MG-1", "cauldron_id": "elsewhere", "id": "X-1", "written_by": {"class": "ripple", "run": "r"}});
    let says = |r: &Value, key: &str| {
        let (code, msg, _) = refused(r);
        assert_eq!(code, "usage", "{msg}");
        assert!(msg.contains(key) && msg.contains("cauldron merge"), "{msg}");
    };
    // soup_put: in the record.
    for (meta, key) in [
        (json!({"merged_from": from}), "meta.merged_from"),
        (json!({"Merged_From": {}}), "meta.Merged_From"),
        (json!({"meta.merged_from": {}}), "meta.meta.merged_from"),
        (json!({"cites": [{"kind": "url", "url": "https://example.org/a", "merged_from": {}}]}), "meta.cites[0].merged_from"),
    ] {
        let mut t = task("t", "carries a provenance", "R-1");
        t["meta"] = meta;
        says(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [t]}), &c), key);
    }
    let mut t = task("t", "carries a provenance at the top", "R-1");
    t["merged_from"] = from.clone();
    says(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [t]}), &c), "merged_from");
    // soup_put: in its cite.
    says(
        &svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t", "cited", "R-1")], "cite": {"kind": "url", "url": "https://example.org/b", "merged_from": from}}), &c),
        "merged_from",
    );
    // soup_link and soup_claim take no such key.
    let r = svc.call("soup_link", json!({"cauldron": "kb", "links": [{"from": "R-2", "kind": "ref", "to": "R-1", "merged_from": from}]}), &c);
    let (code, msg, _) = refused(&r);
    assert_eq!(code, "usage", "{msg}");
    assert!(msg.contains("merged_from"), "{msg}");
    let r = svc.call("soup_claim", json!({"cauldron": "kb", "cluster": "CL-1", "region": "R-1", "merged_from": from}), &c);
    let (code, msg, _) = refused(&r);
    assert_eq!(code, "usage", "{msg}");
    assert!(msg.contains("merged_from"), "{msg}");
    // soup_dump: in its cite.
    says(&svc.call("soup_dump", json!({"cauldron": "kb", "text": "a note", "cite": {"kind": "url", "url": "https://example.org/c", "merged_from": {}}}), &c), "merged_from");
    // The edge and citation tools: one link given flat, an unlink, a
    // citation added or matched for removal. Each names the key, the reason
    // and the write-back list.
    for (tool, args) in [
        ("soup_link", json!({"cauldron": "kb", "from": "R-2", "kind": "ref", "to": "R-1", "merged_from": from})),
        ("soup_unlink", json!({"cauldron": "kb", "links": [{"from": "R-2", "kind": "ref", "to": "R-1", "Merged_From": {}}]})),
        ("soup_cite", json!({"cauldron": "kb", "id": "R-1", "add": {"kind": "url", "url": "https://example.org/d", "merged_from": {}}})),
        ("soup_cite", json!({"cauldron": "kb", "id": "R-1", "remove": {"kind": "url", "meta.merged_from": {}}})),
    ] {
        let r = svc.call(tool, args, &c);
        let (code, msg, _) = refused(&r);
        assert_eq!(code, "usage", "{tool}: {msg}");
        assert!(msg.contains("refused: merged_from") && msg.contains("write-back.json") && msg.contains("cauldron merge"), "{tool}: {msg}");
    }
    assert_eq!(s.node_count("kb"), before, "nothing was written");
    // A ripple's batch.
    ok(&svc.call("soup_dump", json!({"cauldron": "kb", "text": "a note for a ripple"}), &c));
    let key = std::fs::read_to_string(s.mcp_dir().join("ripples.secret")).unwrap().trim().to_string();
    let say = |verb: &str, args: Value| -> Value { serde_json::from_str(&seam_say(svc.seam, &format!("{verb} {key} {args}"))).unwrap() };
    let lock = say("RLOCK", json!({"cauldron": "kb", "run": "run-a", "dump": "DM-1", "regions": ["R-1"], "ttl_secs": 120}));
    assert_eq!(lock["outcome"], "locked", "{lock}");
    let put = json!({"cauldron": "kb", "dump": "DM-1", "run": "run-a", "step": "put", "package": "dump", "version": "v1", "model": "sonnet", "revs": lock["revs"],
        "records": [{"id": "n1", "type": "idea", "state": "idea", "handle": "a ripple's idea that claims a merge", "body": "b",
                     "meta": {"merged_from": from}, "edges": [{"kind": "in_region", "to": "R-1"}]}]});
    let p = say("RPUT", put);
    assert_eq!(p["ok"], json!(false), "{p}");
    assert!(p.to_string().contains("a ripple never writes `meta.merged_from`"), "{p}");
    assert_eq!(s.node_count("kb"), before, "the ripple's batch wrote nothing");
}
