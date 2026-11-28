//! Automatic processing: the real service binary with the scripted backend
//! and the mcp door's ripple seam in this process. Each test ends every dump
//! in exactly one record set, or parked `failed`, or waiting as it should:
//! the word from the door, the scan, the `since` rule, kills at every point
//! (the word lost, the service killed during the call, after the put, before
//! the dump's state flipped, during the scan, before and after a run), off
//! before the next call, bounded retries, and a cap that defers.

use std::time::{Duration, Instant};

use env::ripples_auto::{self as ra, Settings};
use serde_json::{json, Value};

use crate::support::*;

/// Turn automatic processing on for `kb`, taking dumps after `since`.
pub(crate) fn on(s: &Scratch, since: u64, sessions: bool) {
    on_for(s, "kb", since, sessions);
}

/// Turn automatic processing on for `name`, taking dumps after `since`.
pub(crate) fn on_for(s: &Scratch, name: &str, since: u64, sessions: bool) {
    let e = s.env();
    let c = e.find_cauldron(name).unwrap().unwrap();
    ra::store(
        &s.data(),
        &Settings {
            cauldron: name.into(),
            on: true,
            since_at: cauldron_ripples::auto::now(),
            since_dump: since,
            backend: "threads".into(),
            model: None,
            default_model: Some(ra::DEFAULT_MODEL.into()),
            include_sessions: sessions,
            scrub_local: true,
            changed_at: cauldron_ripples::auto::now(),
            off_at: None,
            bound_to: Some(ra::Binding::of(&e, &c)),
        },
    )
    .unwrap();
}

fn off(s: &Scratch) {
    let mut x = ra::load(&s.data(), "kb").unwrap().unwrap();
    x.on = false;
    x.off_at = Some(cauldron_ripples::auto::now());
    ra::store(&s.data(), &x).unwrap();
}

pub(crate) fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn start(s: &Scratch, door: &Door, script: &std::path::Path, extra: &[(&str, String)]) -> Service {
    let mut vars = vec![
        ("CAULDRON_RIPPLES_INFERENCE", format!("scripted:{}", script.display())),
        ("CAULDRON_RIPPLES_SCAN_MS", "300".to_string()),
        ("CAULDRON_RIPPLES_RETRY_MS", "100".to_string()),
    ];
    for (k, v) in extra {
        vars.retain(|(x, _)| x != k);
        vars.push((k, v.clone()));
    }
    let svc = Service::start(s, door, &vars);
    // The door, in this process, tells the service when a dump lands.
    env::egress::grant_addr(svc.seam).unwrap();
    svc
}

pub(crate) fn auto_lines(s: &Scratch) -> Vec<Value> {
    ra::read(&s.data())
}

pub(crate) fn outcomes(s: &Scratch, dump: &str) -> Vec<String> {
    auto_lines(s).iter().filter(|l| l["dump"] == dump && l["event"] == "outcome").map(|l| l["class"].as_str().unwrap().to_string()).collect()
}

/// A dump written straight to the store, as `cauldron soup dump` does when
/// no service holds it: no side effect runs.
fn dump_direct(s: &Scratch, door: &Door, text: &str) -> String {
    use std::io::Write;
    door.release_dumps();
    let t = s.target();
    let store = soup_door::dumps::Store::open(&t.cauldron, "cli test").unwrap();
    let mut w = store.blob(soup_door::dumps::Kind::Text).unwrap();
    w.write_all(text.as_bytes()).unwrap();
    let by = soup_door::dumps::By { thread: "cli".into(), dispatch: "test".into(), instrument: "test".into(), version: "0".into() };
    let make = |blob: &soup_door::dumps::Blob| {
        Ok(soup_door::dumps::NewDump {
            env: "dev".into(),
            content: soup_door::dumps::Content::Text { text: text.to_string(), from: None },
            blob: blob.clone(),
            title: None,
            cite: None,
            refs: vec![],
            client: "cli".into(),
            session: None,
        })
    };
    let (_, landed) = store.commit(w, || Ok(()), make, &by, |e| e).unwrap();
    landed.id
}

/// **The word from the door, and the `since` rule.** A dump from before it
/// was turned on waits; a text dump after it is processed at the door's
/// word, long before the next scan; a session dump is not taken without
/// `--include-sessions`; the dump's listing and `STATUS` say so.
#[test]
fn a1_a_dump_after_it_was_turned_on_is_processed_at_the_doors_word() {
    let s = Scratch::new("a1");
    let door = Door::start(&s);
    let old = door.dump("a note from before it was turned on");
    on(&s, ra::serial(&old), false);
    let script = s.script("a1", json!([{"output": ideas(2), "usage": usage(5000, 300)}]));
    // A scan only at start, so what follows is the word's doing.
    let svc = start(&s, &door, &script, &[("CAULDRON_RIPPLES_SCAN_MS", "600000".into())]);
    let k = Service::key(&s);
    wait_for("the start's scan", 30, || svc.log().contains("cauldron-ripples listening"));
    std::thread::sleep(Duration::from_millis(500));
    assert!(calls(&script).is_empty(), "a dump from before is never processed automatically");
    let new = door.dump("the cache key should hash the whole tree");
    let session = door.dump_session(&transcript());
    // The dump flips when its records land; the outcome line follows.
    wait_for("the new dump processed", 30, || s.dump_state(&new)["state"] == "processed" && !outcomes(&s, &new).is_empty());
    assert_eq!(calls(&script).len(), 1);
    assert_eq!(s.ripple_nodes().len(), 2);
    let d = s.dump_state(&new);
    assert_eq!(d["processed"]["records"].as_array().unwrap().len(), 2, "{d}");
    assert_eq!(s.dump_state(&old)["state"], "received");
    assert_eq!(s.dump_state(&session)["state"], "received");
    assert_eq!(outcomes(&s, &new), ["done"]);
    let started: Vec<Value> = s.journal().into_iter().filter(|l| l["event"] == "started").collect();
    assert_eq!(started[0]["automatic"], json!(true));
    let st = svc.say(&format!("STATUS {k} {}", json!({"cauldron": "kb"})));
    let a = &st["automatic"]["cauldrons"][0];
    assert_eq!((a["automatic"].clone(), a["backend"].clone(), a["waiting"].clone()), (json!("on"), json!("threads"), json!([])), "{st}");
    assert_eq!(a["processed"][0]["dump"], json!(new), "{st}");
    assert_eq!(a["not_taken"][0]["dump"], json!(session), "{st}");
    assert_eq!(st["runs"][0]["automatic"], json!(true), "{st}");
    // Off: a new dump waits.
    off(&s);
    let later = door.dump("written while it is off");
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(s.dump_state(&later)["state"], "received");
    assert_eq!(calls(&script).len(), 1);
}

/// **The scan finds what the word missed**: dumps that landed while the
/// service was down (the word lost), and a dump written straight to the
/// store while it runs (no word at all). Oldest first, one record set each.
#[test]
fn a2_the_scan_finds_dumps_the_word_missed() {
    let s = Scratch::new("a2");
    let door = Door::start(&s);
    on(&s, 0, false);
    // The service is down: the door's word goes nowhere.
    let first = door.dump("first note, landed while the service was down");
    let second = door.dump("second note, landed while the service was down");
    let script = s.script("a2", json!([{"output": ideas(1)}, {"output": ideas(1)}, {"output": ideas(1)}]));
    let svc = start(&s, &door, &script, &[]);
    wait_for("both processed", 30, || s.dump_state(&first)["state"] == "processed" && s.dump_state(&second)["state"] == "processed");
    let order: Vec<String> = auto_lines(&s).iter().filter(|l| l["event"] == "attempt").map(|l| l["dump"].as_str().unwrap().to_string()).collect();
    assert_eq!(order, [first.clone(), second.clone()], "oldest first");
    let third = dump_direct(&s, &door, "written straight to the store, as the CLI does with no service holding it");
    wait_for("the scan found it", 30, || s.dump_state(&third)["state"] == "processed");
    assert_eq!(calls(&script).len(), 3);
    assert_eq!(s.ripple_nodes().len(), 3, "one record set each");
    drop(svc);
}

/// **Killed during the model call**, then started again: the call the kill
/// left is cancelled, the attempt counts as a failed one, and the next
/// attempt lands one record set.
#[test]
fn a3_killed_during_the_call() {
    let s = Scratch::new("a3");
    let door = Door::start(&s);
    on(&s, 0, false);
    let slow = s.script("a3a", json!([{"output": ideas(2), "sleep_ms": 60000, "sent": true}]));
    let mut svc = start(&s, &door, &slow, &[]);
    let id = door.dump("a note whose first call is killed");
    wait_for("the call is in flight", 30, || s.journal().iter().any(|l| l["event"] == "sent"));
    svc.child.kill().unwrap();
    svc.child.wait().unwrap();
    assert!(s.ripple_nodes().is_empty());
    assert_eq!(s.dump_state(&id)["state"], "received");
    let fast = s.script("a3b", json!([{"output": ideas(2)}]));
    let svc = start(&s, &door, &fast, &[]);
    wait_for("processed after the restart", 30, || s.dump_state(&id)["state"] == "processed" && outcomes(&s, &id).contains(&"done".to_string()));
    assert_eq!(s.ripple_nodes().len(), 2, "one record set");
    assert_eq!(calls(&slow).len() + calls(&fast).len(), 2);
    let k = Service::key(&s);
    let st = svc.say(&format!("STATUS {k} {}", json!({})));
    assert_eq!(st["possibly_running"], json!([]), "{st}");
    assert_eq!(st["per_dump"][0]["calls"], 2, "both calls charged: {st}");
    assert_eq!(outcomes(&s, &id), ["done"]);
}

/// **Killed at each of the worker's own points, and after the put**: during
/// the scan, after the attempt was journaled, after the batch landed and the
/// dump was marked, after the run ended. Each restart ends with one record
/// set and no second model call for a dump that landed.
#[test]
fn a4_killed_at_each_point_of_an_automatic_run() {
    for point in ["auto-scan", "auto-attempt", "after-put", "auto-ran"] {
        let s = Scratch::new(&format!("a4-{point}"));
        let door = Door::start(&s);
        on(&s, 0, false);
        let id = door.dump(&format!("a note killed at {point}"));
        let script = s.script("a4", json!([{"output": ideas(2)}, {"output": ideas(2)}]));
        let mut svc = start(&s, &door, &script, &[("CAULDRON_RIPPLES_FAULT", point.to_string())]);
        let status = svc.child.wait().unwrap();
        assert!(!status.success(), "{point}: the fault point stops the process");
        let landed_before = s.ripple_nodes().len();
        let calls_before = calls(&script).len();
        let svc = start(&s, &door, &script, &[("CAULDRON_RIPPLES_FAULT", String::new())]);
        wait_for(&format!("{point}: processed"), 30, || s.dump_state(&id)["state"] == "processed");
        std::thread::sleep(Duration::from_millis(800));
        assert_eq!(s.ripple_nodes().len(), 2, "{point}: one record set");
        if landed_before == 2 {
            assert_eq!(calls(&script).len(), calls_before, "{point}: no call after the batch landed");
        } else {
            assert_eq!(calls(&script).len(), calls_before + 1, "{point}");
        }
        assert_eq!(s.dump_state(&id)["processed"]["records"].as_array().unwrap().len(), 2, "{point}");
        drop(svc);
    }
}

/// **The batch landed, and the dump's state never flipped** (the mcp
/// service died between the two writes): the next automatic run finds the
/// run landed, marks the dump, and calls no model.
#[test]
fn a5_landed_before_the_state_flip() {
    use spine::envelope::{Envelope, Instrument, Mode, Provenance, ENVELOPE_FORMAT_VERSION};
    let s = Scratch::new("a5");
    let door = Door::start(&s);
    on(&s, 0, false);
    // The service is down, so this dump waits.
    let id = door.dump("text whose records landed before the dump was marked");
    let d = s.dump_state(&id);
    let created = s.target().cauldron.meta().unwrap().created_at.unwrap();
    let run = cauldron_ripples::engine::run_id("dev", "kb", &created, &id, d["received_at"].as_str().unwrap());
    let c = spine::Cauldron::open(&s.data().join("cauldrons/kb/soup"), spine::config::SpineConfig::default()).unwrap();
    let env = Envelope {
        statement: "MATCH (r: ws/region { id: $r }) CREATE (i: ws/idea { state: \"idea\", handle: $h }) CREATE (i) -[in_region]-> (r) RETURN i.id".into(),
        bindings: [("r".to_string(), json!("R-1")), ("h".to_string(), json!("landed before the flip"))].into_iter().collect(),
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance { initiator: cauldron_mcp::ripple::initiator(&run, "put"), instrument: Instrument { name: "test".into(), version: "0".into() } },
        mode: Mode::Commit,
        idempotency_nonce: Some("a5".into()),
    };
    assert!(matches!(c.submit(env).unwrap(), spine::Outcome::Committed { .. }));
    drop(c);
    let landed = s.ripple_nodes();
    let script = s.script("a5", json!([]));
    let svc = start(&s, &door, &script, &[]);
    wait_for("marked", 30, || s.dump_state(&id)["state"] == "processed");
    assert!(calls(&script).is_empty(), "no model call");
    assert_eq!(s.ripple_nodes(), landed, "one record set");
    assert_eq!(s.dump_state(&id)["processed"]["records"], json!(landed));
    drop(svc);
}

/// **Failures are retried with a wait, a bounded number of times, then
/// parked `failed`** with the reason, in the dump's own listing; never tried
/// again automatically, and never a storm.
#[test]
fn a6_failures_are_retried_then_parked() {
    let s = Scratch::new("a6");
    let door = Door::start(&s);
    on(&s, 0, false);
    let script = s.script("a6", json!(vec![json!({"fail": "failed", "message": "the turn ended without an answer"}); 10]));
    let svc = start(&s, &door, &script, &[]);
    let id = door.dump("a note no call can answer");
    wait_for("waiting after its first failure", 30, || s.dump_state(&id)["auto"]["state"] == "retrying");
    let a = s.dump_state(&id)["auto"].clone();
    assert_eq!((a["failed_attempts"].clone(), a["of"].clone(), a["last_code"].clone()), (json!(1), json!(3), json!("inference_failed")), "{a}");
    wait_for("parked", 30, || s.dump_state(&id)["state"] == "failed");
    let d = s.dump_state(&id);
    assert_eq!((d["failure"]["code"].clone(), d["failure"]["attempts"].clone(), d["failure"]["by"].clone()), (json!("inference_failed"), json!(3), json!("automatic")), "{d}");
    assert!(d["failure"]["what_to_do"].as_str().unwrap().contains("cauldron ripples run"), "{d}");
    assert!(d.get("auto").is_none(), "{d}");
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(calls(&script).len(), 3, "three attempts, one call each, and no more");
    assert_eq!(outcomes(&s, &id), ["retry", "retry", "parked"]);
    // Two waits, the second four times the first.
    let k = Service::key(&s);
    let st = svc.say(&format!("STATUS {k} {}", json!({"cauldron": "kb"})));
    let f = &st["automatic"]["cauldrons"][0]["failed"][0];
    assert_eq!((f["dump"].clone(), f["code"].clone()), (json!(id), json!("inference_failed")), "{st}");
    // Still processed by hand.
    let fine = s.script("a6b", json!([{"output": ideas(1)}]));
    drop(svc);
    let svc = Service::start(&s, &door, &[("CAULDRON_RIPPLES_INFERENCE", format!("scripted:{}", fine.display())), ("CAULDRON_RIPPLES_SCAN_MS", "600000".into())]);
    // Its three calls are spent, so the owner gives this run more.
    let k = Service::key(&s);
    let a = svc.say(&format!("RUN {k} {}", json!({"cauldron": "kb", "dump": id, "force_budget": 100000})));
    assert_eq!(a["ok"], json!(true), "{a}");
    let run = a["run"].as_str().unwrap().to_string();
    wait_for("the run by hand", 30, || svc.say(&format!("STATUS {k} {}", json!({"run": run})))["state"] == "done");
    let out = svc.say(&format!("STATUS {k} {}", json!({"run": run})))["result"].clone();
    assert_eq!(out["outcome"], "processed", "{out}");
    let d = s.dump_state(&id);
    assert_eq!(d["state"], "processed");
    // The park's failure is over: not listed beside `processed`.
    assert!(d.get("failure").is_none(), "{d}");
}

/// **Over a cap the dump stays `received`**, with `deferred: budget` in its
/// listing and the time it is tried again (the next window); nothing sent.
#[test]
fn a7_a_cap_defers_with_a_visible_reason() {
    let s = Scratch::new("a7");
    let door = Door::start(&s);
    on(&s, 0, false);
    let script = s.script("a7", json!([{"output": ideas(1)}]));
    let svc = start(&s, &door, &script, &[]);
    // Longer than the dump's cap of input tokens leaves room for.
    let id = door.dump(&"a long note about caches and trees. ".repeat(20_000));
    wait_for("deferred", 30, || s.dump_state(&id)["auto"]["state"] == "deferred");
    let d = s.dump_state(&id);
    assert_eq!((d["state"].clone(), d["auto"]["reason"].clone(), d["auto"]["detail"].clone()), (json!("received"), json!("budget"), json!("budget")), "{d}");
    assert!(d["auto"]["retry_after"].as_str().unwrap().ends_with("T00:00:00.000Z"), "{d}");
    std::thread::sleep(Duration::from_millis(1000));
    assert!(calls(&script).is_empty());
    assert_eq!(outcomes(&s, &id), ["deferred"], "not tried again before the window resets");
    let k = Service::key(&s);
    let st = svc.say(&format!("STATUS {k} {}", json!({"cauldron": "kb"})));
    let q = &st["automatic"]["cauldrons"][0]["deferred"][0];
    assert_eq!((q["dump"].clone(), q["reason"].clone()), (json!(id), json!("budget")), "{st}");
}

/// **Off takes effect before the next call**: turned off during the first
/// call, whose answer is refused, the run makes no retry call and writes
/// nothing; the dump stays `received`.
#[test]
fn a8_off_before_the_next_call() {
    let s = Scratch::new("a8");
    let door = Door::start(&s);
    on(&s, 0, false);
    let bad = json!({"records": [{"label": "n1", "type": "idea", "state": "idea", "handle": "x", "regions": ["R-1"], "extra": 1}]});
    let script = s.script("a8", json!([{"output": bad, "sleep_ms": 2000}, {"output": ideas(1)}]));
    let svc = start(&s, &door, &script, &[]);
    let id = door.dump("a note whose run is turned off midway");
    wait_for("the first call", 30, || calls(&script).len() == 1);
    off(&s);
    wait_for("the run ended", 30, || !outcomes(&s, &id).is_empty());
    assert_eq!(outcomes(&s, &id), ["skip"]);
    assert_eq!(calls(&script).len(), 1, "no call after off");
    assert!(s.ripple_nodes().is_empty());
    assert_eq!(s.dump_state(&id)["state"], "received");
    let done: Vec<Value> = s.journal().into_iter().filter(|l| l["event"] == "done").collect();
    assert_eq!((done[0]["outcome"].clone(), done[0]["reason"].clone()), (json!("deferred"), json!("off")));
    drop(svc);
}

/// **The receipt**: `soup_dump` says whether automatic processing is on and
/// takes the dump; with `wait_secs` it answers the records it made.
#[test]
fn a9_the_receipt_says_so_and_waits_for_the_records() {
    let s = Scratch::new("a9");
    let door = Door::start(&s);
    let call = |args: Value| {
        let mut args = args;
        args["_ctx"] = json!({"client": "test"});
        door.door.call(cauldron_mcp::catalogue::find("soup_dump").unwrap(), args)
    };
    let r = call(json!({"cauldron": "kb", "text": "a note while it is off"})).unwrap();
    assert_eq!((r["automatic"]["on"].clone(), r["automatic"]["takes_this_dump"].clone()), (json!(false), json!(false)), "{r}");
    assert!(r["automatic"]["note"].as_str().unwrap().contains("cauldron ripples run DM-1"), "{r}");
    on(&s, 1, false);
    let r = call(json!({"cauldron": "kb", "text": "nothing runs this one", "wait_secs": 1})).unwrap();
    assert_eq!(r["automatic"]["takes_this_dump"], json!(true), "{r}");
    assert!(r["automatic"]["service"].as_str().unwrap().contains("not running"), "{r}");
    assert_eq!((r["processing"]["state"].clone(), r["processing"]["done"].clone()), (json!("received"), json!(false)), "{r}");
    let script = s.script("a9", json!([{"output": ideas(2)}, {"output": ideas(3)}]));
    let svc = start(&s, &door, &script, &[("CAULDRON_RIPPLES_SCAN_MS", "600000".into())]);
    wait_for("the start's scan took DM-2", 30, || s.dump_state("DM-2")["state"] == "processed");
    let r = call(json!({"cauldron": "kb", "text": "the cache key should hash the tree", "wait_secs": 30})).unwrap();
    let p = &r["processing"];
    assert_eq!((p["state"].clone(), p["done"].clone(), p["records"].as_array().map(Vec::len)), (json!("processed"), json!(true), Some(3)), "{r}");
    assert_eq!(p["records"], s.dump_state(r["dump_id"].as_str().unwrap())["processed"]["records"]);
    // The receipt names the review commands for those records.
    let id = r["dump_id"].as_str().unwrap();
    assert_eq!(p["review"]["list"], json!(format!("cauldron ripples review --env dev --cauldron kb --dump {id}")), "{r}");
    assert_eq!(p["review"]["approve"], json!(format!("cauldron ripples approve --dump {id} --env dev --cauldron kb")), "{r}");
    assert!(p["review"]["note"].as_str().unwrap().contains("interactive terminal"), "{r}");
    let bad = call(json!({"cauldron": "kb", "text": "x", "wait_secs": 121})).unwrap_err();
    assert_eq!(bad.code, "usage", "{bad:?}");
    drop(svc);
}

/// **"On" belongs to one cauldron.** Turned on for `kb`, which is then
/// removed (moved away, its grant dropped) by a path that leaves the setting
/// behind, and made again under the same name and granted: the new `kb` is
/// never processed automatically (no call, nothing written), and `STATUS`
/// says the setting belonged to a cauldron that is gone and how to clear
/// it. Turned on for the new `kb`, its dumps are processed, and what the
/// old `kb`'s dumps of the same ids came to (`done`) suppresses nothing.
#[test]
fn a10_a_cauldron_made_again_under_the_name_does_not_inherit_on() {
    let s = Scratch::new("a10");
    let door = Door::start(&s);
    on(&s, 0, false);
    let script = s.script("a10", json!([{"output": ideas(1)}, {"output": ideas(1)}, {"output": ideas(1)}]));
    let svc = start(&s, &door, &script, &[]);
    let old = door.dump("a note in the first kb");
    // The dump flips when its records land; the outcome line follows.
    wait_for("the first kb's dump processed", 30, || s.dump_state(&old)["state"] == "processed" && !outcomes(&s, &old).is_empty());
    assert_eq!(outcomes(&s, &old), ["done"]);
    drop(svc);
    // Removed as `cauldron remove` moves it, the setting left behind.
    door.release_dumps();
    let away = s.data().join("cauldrons").join(".removed");
    std::fs::create_dir_all(&away).unwrap();
    std::fs::rename(s.data().join("cauldrons").join("kb"), away.join("kb-old")).unwrap();
    soup_door::grants::forget(&s.env(), "kb").unwrap();
    assert!(ra::load(&s.data(), "kb").unwrap().unwrap().on, "the setting is left behind");
    // Made again and granted: ordinary use, never turned on.
    s.make_kb();
    let door = Door::start(&s);
    let r = door.door.call(cauldron_mcp::catalogue::find("soup_dump").unwrap(), json!({"_ctx": {"client": "test"}, "cauldron": "kb", "text": "a note in the new kb"})).unwrap();
    let first = r["dump_id"].as_str().unwrap().to_string();
    assert_eq!(first, old, "the new kb's dumps start again at the same id");
    assert_eq!((r["automatic"]["on"].clone(), r["automatic"]["takes_this_dump"].clone()), (json!(false), json!(false)), "{r}");
    let second = door.dump("another note in the new kb");
    let svc = start(&s, &door, &script, &[]);
    let k = Service::key(&s);
    wait_for("the start's scan", 30, || svc.log().contains("cauldron-ripples listening"));
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(calls(&script).len(), 1, "no call for the new kb");
    assert_eq!((s.dump_state(&first)["state"].clone(), s.dump_state(&second)["state"].clone()), (json!("received"), json!("received")));
    let st = svc.say(&format!("STATUS {k} {}", json!({"cauldron": "kb"})));
    let a = &st["automatic"]["cauldrons"][0];
    assert_eq!(a["automatic"], "void", "{st}");
    assert!(a["why"].as_str().unwrap().contains("gone"), "{st}");
    assert!(a["what_to_do"].as_str().unwrap().contains("cauldron ripples off kb --env dev"), "{st}");
    // The DUMP word for the void setting wakes nothing.
    assert_eq!(svc.say(&format!("DUMP {k} {}", json!({"cauldron": "kb", "dump": second})))["automatic"], "off");
    // Turned on for the new kb (since 0): both its dumps run, the first
    // although the old kb's dump of that id is `done` in the journal.
    on(&s, 0, false);
    wait_for("the new kb's dumps processed", 30, || s.dump_state(&first)["state"] == "processed" && s.dump_state(&second)["state"] == "processed");
    assert_eq!(calls(&script).len(), 3);
    drop(svc);
}

/// Every file under `root`, read as bytes: does any hold `needle`?
pub fn files_holding(root: &std::path::Path, needle: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut walk = vec![root.to_path_buf()];
    while let Some(p) = walk.pop() {
        if p.is_dir() {
            for e in std::fs::read_dir(&p).unwrap().flatten() {
                walk.push(e.path());
            }
        } else if let Ok(b) = std::fs::read(&p) {
            if b.windows(needle.len()).any(|w| w == needle.as_bytes()) {
                out.push(p.display().to_string());
            }
        }
    }
    out
}

/// **No model answer, note or backend message is written down.** Markers
/// planted in a scripted model's refused answer (quoted by the schema's
/// error), in its `notes`, and in a failing backend's message: after the
/// automatic runs (one parked as refused, one parked after three failures,
/// one processed), no file under the cauldron root holds any of them: not
/// the run journal, not the automatic journal, not the dump's own record
/// (`meta.failure`), not the soup. The outcomes carry codes and fixed
/// reasons only.
#[test]
fn a11_no_model_or_backend_words_are_written_down() {
    let s = Scratch::new("a11");
    let door = Door::start(&s);
    on(&s, 0, false);
    let mark = |what: &str| ["PLANTED", "-", what, "-", "MARKER"].concat();
    let (in_answer, in_notes, in_failure, in_good_notes) = (mark("ANSWER"), mark("NOTES"), mark("FAILURE"), mark("GOOD"));
    // A handle over the schema's length: the schema's error quotes it.
    let bad = json!({"records": [{"label": "n1", "type": "idea", "state": "idea", "handle": format!("{in_answer} {}", "x".repeat(260)), "regions": ["R-1"]}], "notes": in_notes});
    let mut good = ideas(1);
    good["notes"] = json!(in_good_notes);
    let fail = json!({"fail": "failed", "message": format!("the backend said {in_failure}")});
    let script = s.script("a11", json!([{"output": bad.clone()}, {"output": bad}, fail.clone(), fail.clone(), fail, {"output": good}]));
    let svc = start(&s, &door, &script, &[]);
    let refused = door.dump("a note whose answer is refused twice");
    wait_for("parked as refused", 30, || s.dump_state(&refused)["state"] == "failed");
    let failing = door.dump("a note whose calls fail");
    wait_for("parked after three failures", 60, || s.dump_state(&failing)["state"] == "failed");
    let fine = door.dump("a note processed with notes");
    wait_for("processed", 30, || s.dump_state(&fine)["state"] == "processed" && !outcomes(&s, &fine).is_empty());
    drop(svc);
    assert_eq!(calls(&script).len(), 6);
    let d = s.dump_state(&refused);
    assert_eq!(d["failure"]["code"], "answer_refused", "{d}");
    assert_eq!(s.dump_state(&failing)["failure"]["code"], "inference_failed");
    for m in [&in_answer, &in_notes, &in_failure, &in_good_notes] {
        let held = files_holding(&s.root(), m);
        assert!(held.is_empty(), "{m} written to {held:?}");
    }
    // The marker did reach the model's retry prompt only as the scrubbed
    // refusal (the scripted backend's own log is outside the root).
    assert!(calls(&script)[1]["prompt"].as_str().unwrap().contains("previous answer was refused"));
}

/// **`--debug-keep-answer`, by hand only**: the answer and its notes are
/// kept in one owner-only file whose path the outcome names, and nowhere
/// else; without it, nowhere at all.
#[test]
fn a12_debug_keep_answer_keeps_the_answer_in_one_private_file() {
    use cauldron_ripples::engine::{self, Caps, Opts};
    use std::os::unix::fs::PermissionsExt;
    let s = Scratch::new("a12");
    let door = Door::start(&s);
    let marker = ["PLANTED", "-KEPT-", "MARKER"].concat();
    let mut good = ideas(1);
    good["notes"] = json!(marker);
    let script = s.script("a12", json!([{"output": good.clone()}, {"output": good}]));
    let ctx = engine(&s, &door, &script, Caps::default());
    let plain = engine::run(&ctx, &ask(&door.dump("a note, no keeping")));
    assert_eq!(plain["outcome"], "processed", "{plain}");
    assert_eq!(plain["notes"], json!(marker), "the caller by hand is shown the notes");
    assert!(plain.get("debug_file").is_none());
    assert!(files_holding(&s.root(), &marker).is_empty());
    let p = engine::prepare(&ctx, &ask(&door.dump("a note, kept"))).unwrap();
    let kept = engine::execute_with(&ctx, &p, &Opts { keep_answer: true, ..Opts::default() });
    let file = kept["debug_file"].as_str().unwrap().to_string();
    assert!(file.starts_with(&s.ripples_dir().join("debug").display().to_string()), "{file}");
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(files_holding(&s.root(), &marker), [file.clone()]);
    // An automatic run never keeps it, even when asked.
    let p = engine::prepare(&ctx, &ask(&door.dump("a third note"))).unwrap();
    let auto = engine::execute_with(&ctx, &p, &Opts { keep_answer: true, automatic: true, ..Opts::default() });
    assert!(auto.get("debug_file").is_none(), "{auto}");
}

/// **A dump in flight reads `running`**, not `retrying` with a cut-short
/// attempt, in `STATUS` while its call is under way; after, `processed`.
#[test]
fn a13_a_dump_in_flight_reads_running() {
    let s = Scratch::new("a13");
    let door = Door::start(&s);
    on(&s, 0, false);
    let script = s.script("a13", json!([{"output": ideas(1), "sleep_ms": 2000}]));
    let svc = start(&s, &door, &script, &[]);
    let k = Service::key(&s);
    let id = door.dump("a note whose call takes a while");
    wait_for("the call", 30, || calls(&script).len() == 1);
    let st = svc.say(&format!("STATUS {k} {}", json!({"cauldron": "kb"})));
    let a = &st["automatic"]["cauldrons"][0];
    assert_eq!((a["running"][0]["dump"].clone(), a["running"][0]["attempt"].clone(), a["retrying"].clone()), (json!(id), json!(1), json!([])), "{st}");
    // A run by hand meanwhile: refused, saying who holds the slot.
    let busy = svc.say(&format!("RUN {k} {}", json!({"cauldron": "kb", "dump": id})));
    assert_eq!((busy["code"].clone(), busy["held_by"].clone()), (json!("busy"), json!("automatic")), "{busy}");
    assert!(busy["message"].as_str().unwrap().contains("try again in a moment") || busy["message"].as_str().unwrap().contains("Try again in a moment"), "{busy}");
    wait_for("processed", 30, || s.dump_state(&id)["state"] == "processed" && !outcomes(&s, &id).is_empty());
    let st = svc.say(&format!("STATUS {k} {}", json!({"cauldron": "kb"})));
    assert_eq!((st["automatic"]["cauldrons"][0]["running"].clone(), st["automatic"]["cauldrons"][0]["processed"][0]["dump"].clone()), (json!([]), json!(id)), "{st}");
    drop(svc);
}

/// **An automatic journal that cannot be written stops the scan**, with no
/// spin and no spend: a run whose attempt cannot be journaled never
/// starts, nothing is sent, the run journal and the dump's record stay as
/// they are, and the next scan tries again.
#[test]
fn a14_an_unwritable_automatic_journal_runs_nothing_and_does_not_spin() {
    let s = Scratch::new("a14");
    let door = Door::start(&s);
    on(&s, 0, false);
    // A directory where the journal file would be: every append fails.
    std::fs::create_dir_all(s.ripples_dir().join("auto.jsonl")).unwrap();
    let script = s.script("a14", json!([{"output": ideas(1)}]));
    let svc = start(&s, &door, &script, &[]);
    let id = door.dump("a note while the journal cannot be written");
    wait_for("the warning", 30, || svc.log().contains("cannot write the automatic journal"));
    let runs = s.journal().len();
    std::thread::sleep(Duration::from_millis(1500));
    assert!(calls(&script).is_empty(), "nothing was sent");
    assert_eq!(s.journal().len(), runs, "the run journal does not grow");
    let d = s.dump_state(&id);
    assert_eq!(d["state"], "received");
    assert!(d.get("auto").is_none(), "{d}");
    // Scans every 300 ms over 1.5 s: a few warnings, not thousands.
    assert!(svc.log().matches("cannot write the automatic journal").count() < 20, "{}", svc.log());
    drop(svc);
}

/// **Dumps that wait for good never hide a newer one.** The `mcp` service
/// lists `received` dumps a page at a time, oldest first (500; 2 here):
/// three dumps deferred until the next window fill the first pages, and the
/// newest, behind them, is still found and processed.
#[test]
fn a15_waiting_dumps_do_not_hide_a_newer_one() {
    let s = Scratch::new("a15");
    let door = Door::start(&s);
    on(&s, 0, false);
    let long = "a long note about caches and trees. ".repeat(20_000);
    let held: Vec<String> = (0..3).map(|_| door.dump(&long)).collect();
    let script = s.script("a15", json!([{"output": ideas(1)}]));
    let svc = start(&s, &door, &script, &[("CAULDRON_RIPPLES_PAGE", "2".into())]);
    wait_for("all three deferred", 30, || held.iter().all(|id| s.dump_state(id)["auto"]["state"] == "deferred"));
    let newest = door.dump("a short note behind the long ones");
    wait_for("the newest processed", 30, || s.dump_state(&newest)["state"] == "processed");
    assert_eq!(calls(&script).len(), 1);
    drop(svc);
}

/// **A busy backend or a taken run slot never strands a dump.** A run by
/// hand holds the one run slot while a dump lands (its word arrives then):
/// `STATUS` says the dump waits for that run, and it starts when the run
/// ends. Its first two calls find the backend busy (as the inference
/// service answers while another request holds it): each ends the
/// automatic attempt at once as `deferred` (reason `busy`, on the dump's
/// record and in `STATUS`), charged nothing and not a failed attempt; the
/// next scan picks it up, and it is processed with one call charged. (And
/// `kb`, made with no id, is given one by the door, its grant and setting
/// carried onto it.)
#[test]
fn a16_a_busy_refusal_leaves_the_dump_to_the_next_scan() {
    let s = Scratch::new("a16");
    let door = Door::start(&s);
    let by_hand = door.dump("a note run by hand");
    on(&s, ra::serial(&by_hand), false);
    let word = ["held", "-by-", "client", "-seven"].concat();
    let script = s.script("a16", json!([{"output": ideas(1), "sleep_ms": 1500}, {"fail": "busy", "message": word}, {"fail": "busy"}, {"output": ideas(1)}]));
    let svc = start(&s, &door, &script, &[("CAULDRON_RIPPLES_SCAN_MS", "1000".into())]);
    let k = Service::key(&s);
    let a = svc.say(&format!("RUN {k} {}", json!({"cauldron": "kb", "dump": by_hand})));
    assert_eq!(a["done"], json!(false), "{a}");
    wait_for("the run by hand calls", 30, || calls(&script).len() == 1);
    let id = door.dump("a note that lands while the slot is taken");
    let st = svc.say(&format!("STATUS {k} {}", json!({"cauldron": "kb"})));
    let q = &st["automatic"]["cauldrons"][0];
    assert_eq!((q["waiting"].clone(), q["waits_for"]["run_slot"]["dump"].clone()), (json!([id.clone()]), json!(by_hand.clone())), "{st}");
    // The first busy answer: deferred `busy`, shown, not a failure.
    wait_for("a busy outcome", 30, || !outcomes(&s, &id).is_empty());
    let first = auto_lines(&s).into_iter().find(|l| l["event"] == "outcome" && l["dump"] == id.as_str()).unwrap();
    assert_eq!((first["class"].clone(), first["reason"].clone(), first["failed_attempts"].clone()), (json!("deferred"), json!("busy"), json!(0)), "{first}");
    assert!(!first.to_string().contains(&word), "the backend's words are not written: {first}");
    let rec = s.dump_state(&id);
    if rec["state"] == "received" {
        assert_eq!((rec["auto"]["state"].clone(), rec["auto"]["reason"].clone()), (json!("deferred"), json!("busy")), "{rec}");
    }
    // Then the next scans: processed.
    wait_for("processed", 30, || s.dump_state(&id)["state"] == "processed" && outcomes(&s, &id).len() == 3);
    assert_eq!(outcomes(&s, &id), ["deferred", "deferred", "done"]);
    assert_eq!(calls(&script).len(), 4);
    let per = cauldron_ripples::journal::per_dump(&s.journal(), Some("kb"));
    let mine = per.iter().find(|d| d["dump"] == id.as_str()).unwrap();
    assert_eq!(mine["calls"], json!(1), "the busy answers were charged nothing: {per:?}");
    drop(svc);
    assert!(files_holding(&s.root(), &word).is_empty());
    // kb was made without an id (as before ids); the door, its first
    // writer, gave it one and carried its grant and its setting onto it.
    let id = s.env().find_cauldron("kb").unwrap().unwrap().id().expect("an id given by the door");
    let g = soup_door::grants::Grants::load(&s.env()).unwrap();
    assert_eq!(g.get("kb").unwrap().cauldron_id.as_deref(), Some(id.as_str()));
    assert_eq!(ra::load(&s.data(), "kb").unwrap().unwrap().bound_to.unwrap().id.as_deref(), Some(id.as_str()));
}

/// **A flood of 500 dumps still respects the caps, and holds no region it
/// does not need.** 500 dumps, each declaring a region, land while
/// automatic processing is on: exactly the daily cap's 30 are processed
/// with one call each, every other one is deferred `daily_cap` with nothing
/// sent and without ever taking a region lock, and no lock is held at the
/// end.
#[test]
fn a17_a_flood_of_500_dumps_respects_the_caps_and_holds_nothing_after() {
    // The daily cap is a window per UTC day (`journal::today`): a run that
    // crosses midnight rightly processes the next day's share too, so the
    // counts below hold only for a run inside one UTC day.
    let utc_day = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() / 86_400;
    let day = utc_day();
    let s = Scratch::new("a17");
    let door = Door::start(&s);
    on(&s, 0, false);
    let entries: Vec<Value> = (0..40).map(|_| json!({"output": ideas(1), "usage": usage(1000, 100)})).collect();
    let script = s.script("a17", json!(entries));
    let svc = start(&s, &door, &script, &[("CAULDRON_RIPPLES_SCAN_MS", "50".into())]);
    let ids: Vec<String> = (0..500).map(|i| door.dump(&format!("the published schema needs a drift test, note {i}"))).collect();
    wait_for("every dump has an outcome", 600, || {
        let lines = auto_lines(&s);
        ids.iter().all(|id| lines.iter().any(|l| l["dump"] == id.as_str() && l["event"] == "outcome"))
    });
    if utc_day() != day {
        eprintln!("a17: the UTC day changed during the run, so the daily window reset and the counts are two days' worth; nothing is asserted");
        return;
    }
    let processed = ids.iter().filter(|id| s.dump_state(id)["state"] == "processed").count();
    assert_eq!(processed, 30, "the daily cap of 30 dumps");
    assert_eq!(calls(&script).len(), 30, "one call each, none for a deferred dump");
    let deferred: Vec<&String> = ids.iter().filter(|id| s.dump_state(id)["auto"]["reason"] == "budget" || s.dump_state(id)["auto"]["detail"] == "daily_cap").collect();
    assert_eq!(deferred.len(), 470, "every other dump deferred at the daily cap");
    let locked_runs: std::collections::BTreeSet<String> = s.journal().iter().filter(|l| l["event"] == "locked").map(|l| l["run"].as_str().unwrap().to_string()).collect();
    assert_eq!(locked_runs.len(), 30, "only the runs that called took a lock");
    let now = env::region_locks::now_ms();
    assert!(env::region_locks::load(&s.target().cauldron).unwrap().live(now).is_empty(), "nothing held after");
    let k = Service::key(&s);
    let st = svc.say(&format!("STATUS {k} {}", json!({"cauldron": "kb"})));
    assert_eq!(st["today"]["per_cauldron"]["kb"]["dumps"], 30, "{}", st["today"]);
    assert_eq!(st["region_locks"], json!([]), "{}", st["region_locks"]);
}

/// **A cauldron turned on under the old default is told it moved.** A
/// setting written before 0.1.17 (no `default_model`, no `--model`) runs on
/// opus now: `STATUS` says so with the cost and the command that keeps
/// sonnet, and its first run after the upgrade writes one
/// `default_model_changed` line, not one per run. A setting that recorded a
/// model keeps it and says nothing.
#[test]
fn dm1_the_default_model_change_is_said_once_and_an_explicit_model_is_kept() {
    let s = Scratch::new("dm1");
    let door = Door::start(&s);
    on(&s, 0, false);
    let mut x = ra::load(&s.data(), "kb").unwrap().unwrap();
    x.default_model = None;
    ra::store(&s.data(), &x).unwrap();
    let script = s.script("dm1", json!([{"output": ideas(1)}, {"output": ideas(1)}]));
    let svc = start(&s, &door, &script, &[]);
    let k = Service::key(&s);
    let st = svc.say(&format!("STATUS {k} {}", json!({"cauldron": "kb"})));
    let a = &st["automatic"]["cauldrons"][0];
    assert_eq!((a["model"].clone(), a["default_changed"]["was"].clone(), a["default_changed"]["now"].clone()), (json!("opus"), json!("sonnet"), json!("opus")), "{st}");
    assert!(a["default_changed"]["note"].as_str().unwrap().contains("costs more"), "{st}");
    assert!(a["default_changed"]["keep_sonnet"].as_str().unwrap().contains("--model sonnet"), "{st}");
    let first = door.dump("the first note after the upgrade");
    wait_for("the first processed", 30, || s.dump_state(&first)["state"] == "processed" && !outcomes(&s, &first).is_empty());
    let second = door.dump("the second note after the upgrade");
    wait_for("the second processed", 30, || s.dump_state(&second)["state"] == "processed" && !outcomes(&s, &second).is_empty());
    let said: Vec<Value> = auto_lines(&s).into_iter().filter(|l| l["event"] == "default_model_changed").collect();
    assert_eq!(said.len(), 1, "said once: {said:?}");
    assert_eq!((said[0]["dump"].clone(), said[0]["was"].clone(), said[0]["now"].clone()), (json!(first), json!("sonnet"), json!("opus")));
    // An explicit model stays, and nothing is said.
    let mut x = ra::load(&s.data(), "kb").unwrap().unwrap();
    x.model = Some("sonnet".into());
    ra::store(&s.data(), &x).unwrap();
    let st = svc.say(&format!("STATUS {k} {}", json!({"cauldron": "kb"})));
    let a = &st["automatic"]["cauldrons"][0];
    assert_eq!((a["model"].clone(), a.get("default_changed")), (json!("sonnet"), None), "{st}");
    drop(svc);
}
