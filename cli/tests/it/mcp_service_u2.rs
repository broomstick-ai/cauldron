//! User grammars after the inspection (issue #28, U2): what a read gives a
//! write takes back, through every door; a grammar added while the `mcp`
//! service holds the soup, with no grant touched; and links, citations and
//! region filters on user records.

use super::*;

const FILMS: &str = r#"{"format": "cauldron-grammar/1", "kind": "film-list", "version": 1,
  "node_types": {"film": {"id_prefix": "FM", "fields": {"handle": {"required": true, "kind": "text", "min_chars": 1, "max_chars": 235}}}},
  "edge_kinds": {"based_on": {"from": [{"at": "local", "node_type": "film"}], "to": [{"at": "reach", "kind": "reading-list", "node_type": "book"}]}}}"#;

/// The text of an MCP answer, as JSON.
pub(super) fn text(r: &Value) -> Value {
    serde_json::from_str(r["content"][0]["text"].as_str().unwrap_or("null")).unwrap_or(Value::Null)
}

/// The JSON document a terminal run printed (after whatever came first).
pub(super) fn printed(o: &Out) -> Value {
    let at = o.out.find("{\n").unwrap_or_else(|| panic!("no JSON in {}", o.out));
    serde_json::from_str(&o.out[at..]).unwrap_or_else(|e| panic!("{e}: {}", o.out))
}

/// A soup `kb` with the reading list added, and a region, a decision in it,
/// an author, a book reaching both, a question, and a cluster claiming the
/// region.
pub(super) fn books(tag: &str) -> Scratch {
    let s = Scratch::new(tag, &["kb"]);
    let file = s.0.join("reading-list.grammar.json");
    std::fs::write(&file, READING_LIST).unwrap();
    let o = s.run_with(&["soup", "grammar", "add", file.to_str().unwrap(), "--env", "dev", "--cauldron", "kb"], &[], &[("CAULDRON_OWNER_ACTS", "grammar")]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let batch = s.write(
        "books.json",
        &json!([
            {"id": "R-1", "type": "region", "handle": "Reading", "boundary": "Everything about the books this project reads and what they changed."},
            {"id": "DEC-1", "type": "decision", "state": "decided", "handle": "Read the classics first", "edges": [{"kind": "in_region", "to": "R-1"}]},
            {"id": "Q-1", "type": "question", "state": "open", "handle": "Which edition?", "edges": [{"kind": "in_region", "to": "R-1"}]},
            {"id": "AU-1", "type": "author", "handle": "A. Writer", "meta": {"cites": [{"kind": "url", "url": "https://example.com/a"}]}},
            {"id": "BK-1", "type": "book", "state": "reading", "handle": "A book about graphs", "year": 1999,
             "edges": [{"kind": "written_by", "to": "AU-1"}, {"kind": "about", "to": "R-1", "why": "it shaped the region"}, {"kind": "about", "to": "DEC-1"}]},
            {"id": "CL-1", "type": "cluster", "handle": "Shelf", "boundary": "The books on the shelf and the decisions they led to.",
             "edges": [{"kind": "claims", "to": "R-1"}]}
        ]),
    );
    let o = s.import("kb", &batch, &[], &[]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    s
}

/// The ids every round trip reads: two user kinds, two built-in kinds and
/// a cluster.
const READ: [&str; 5] = ["BK-1", "AU-1", "DEC-1", "Q-1", "CL-1"];

/// A record as a write gets it back: what reads compute and what a door
/// adds (`meta`, which a service write cites the session in) left out.
fn same(r: &Value, through_service: bool) -> Value {
    let mut r = r.clone();
    let m = r.as_object_mut().unwrap();
    for k in ["id", "superseded_by", "unreviewed"] {
        m.remove(k);
    }
    if through_service {
        m.remove("meta");
    }
    r
}

/// Each record read, given a batch label in place of its id: the one
/// change a write-back needs (a write only creates), and nothing else.
fn relabelled(records: &[Value], tag: &str) -> Vec<Value> {
    records
        .iter()
        .map(|r| {
            let mut r = r.clone();
            r["id"] = json!(format!("{tag}-{}", r["id"].as_str().unwrap().to_lowercase()));
            r
        })
        .collect()
}

/// The ids the labels became, from a write's answer.
fn minted(answer: &Value, sent: &[Value]) -> Vec<String> {
    sent.iter().map(|r| answer["id_map"][r["id"].as_str().unwrap()].as_str().unwrap_or_else(|| panic!("no id for {}: {answer}", r["id"])).to_string()).collect()
}

pub(super) fn by_id(records: &Value, id: &str) -> Value {
    records["records"].as_array().unwrap().iter().find(|r| r["id"] == id).cloned().unwrap_or_else(|| panic!("no {id} in {records}"))
}

/// **A record read through any read door is written back unchanged
/// through any write door** (inspection F1): read with `cauldron soup get`
/// (plain ids and `<ID>@<pool>`) and with `soup_get`, written with a direct
/// `cauldron soup import`, with one the command line sends to the service,
/// and with `soup_put`; for user kinds (`book`, `author`), built-in ones
/// (`decision`, `question`) and a cluster. Only the id is changed (to a
/// label, under remint); each lands, the `pool` a read adds to a user record
/// is named in `ignored`, and the new record reads back as the old one did.
// RED: `pool` not in `writeback::IGNORED` — every write of a user record
// read back is refused `undeclared_field: … no field `pool``.
#[test]
fn u2_a_record_read_through_any_door_is_written_back_through_any_door() {
    let s = books("u2wb");
    let cli = |ids: &[&str]| s.run(&[&["soup", "get"][..], ids, &["--env", "dev", "--cauldron", "kb"]].concat(), &[]).json();
    let plain = cli(&READ);
    let at = cli(&["BK-1@reading-list", "AU-1@reading-list", "DEC-1@ws", "Q-1@ws", "CL-1@cauldron"]);
    assert!(plain.get("missing").is_none() && at.get("missing").is_none(), "{plain}\n{at}");
    let originals: Vec<Value> = READ.iter().map(|id| by_id(&plain, id)).collect();
    for id in READ {
        assert_eq!(same(&by_id(&at, id), false), same(&by_id(&plain, id), false), "`{id}@<pool>` reads the record `{id}` reads");
    }
    assert_eq!(by_id(&plain, "BK-1")["pool"], "reading-list");
    // Check one write's answer and read its records back.
    let check = |door: &str, answer: &Value, sent: &[Value], through_service: bool| {
        assert_eq!(answer["outcome"], "landed", "{door}: {answer}");
        let ignored: Vec<String> = answer["ignored"].as_array().into_iter().flatten().map(|i| format!("{}:{}", i["record"].as_str().unwrap(), i["key"].as_str().unwrap())).collect();
        for r in sent.iter().filter(|r| r.get("pool").is_some()) {
            assert!(ignored.contains(&format!("{}:pool", r["id"].as_str().unwrap())), "{door}: the read's `pool` is dropped and named: {ignored:?}");
        }
        let new = minted(answer, sent);
        let ids: Vec<&str> = new.iter().map(String::as_str).collect();
        let back = cli(&ids);
        for (orig, id) in originals.iter().zip(&new) {
            let got = by_id(&back, id);
            assert_eq!(same(&got, through_service), same(orig, through_service), "{door}: {id} reads back as {} did", orig["id"]);
            if through_service {
                for c in orig["meta"]["cites"].as_array().into_iter().flatten() {
                    assert!(got["meta"]["cites"].as_array().unwrap().contains(c), "{door}: {id} keeps its citations: {got}");
                }
            }
        }
    };
    // The command line, directly (nothing holds the soup).
    for (door, read) in [("get | import", &plain), ("get @pool | import", &at)] {
        let recs: Vec<Value> = READ.iter().map(|id| by_id(read, id)).collect();
        let sent = relabelled(&recs, if door.contains('@') { "at-direct" } else { "direct" });
        let o = s.import("kb", &s.write("wb.json", &json!(sent)), &["--remint"], &[]);
        assert_eq!(o.rc, 0, "{door}: {}{}", o.out, o.err);
        check(door, &o.json(), &sent, false);
    }
    // Through the service: soup_get, soup_put, and an import it is sent.
    s.grant("kb");
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let mcp = text(&svc.call(&s, "soup_get", json!({"cauldron": "kb", "ids": READ})));
    assert!(mcp.get("missing").is_none(), "{mcp}");
    for id in READ {
        assert_eq!(same(&by_id(&mcp, id), false), same(&by_id(&plain, id), false), "soup_get reads `{id}` as `soup get` does");
    }
    for (door, read) in [("get | soup_put", &plain), ("get @pool | soup_put", &at), ("soup_get | soup_put", &mcp)] {
        let recs: Vec<Value> = READ.iter().map(|id| by_id(read, id)).collect();
        let sent = relabelled(&recs, &format!("put{}", door.len()));
        let r = svc.call(&s, "soup_put", json!({"cauldron": "kb", "remint": true, "records": sent}));
        assert_ne!(r["isError"], json!(true), "{door}: {r}");
        check(door, &text(&r), &sent, true);
    }
    for (door, read) in [
        ("get | import (sent to the service)", &plain),
        ("get @pool | import (sent to the service)", &at),
        ("soup_get | import (sent to the service)", &mcp),
    ] {
        let recs: Vec<Value> = READ.iter().map(|id| by_id(read, id)).collect();
        let sent = relabelled(&recs, &format!("fwd{}", door.len()));
        let o = s.import("kb", &s.write("wb.json", &json!(sent)), &["--remint"], &pids);
        assert_eq!(o.rc, 0, "{door}: {}{}", o.out, o.err);
        assert!(o.json().get("forwarded_to").is_some(), "{door}: {}", o.out);
        check(door, &o.json(), &sent, true);
    }
    svc.stop();
    // What soup_get read, written by the command line directly.
    let recs: Vec<Value> = READ.iter().map(|id| by_id(&mcp, id)).collect();
    let sent = relabelled(&recs, "mcp-direct");
    let o = s.import("kb", &s.write("wb.json", &json!(sent)), &["--remint"], &[]);
    assert_eq!(o.rc, 0, "soup_get | import: {}{}", o.out, o.err);
    check("soup_get | import", &o.json(), &sent, false);
}

/// **A grammar added while the `mcp` service holds the soup is made through
/// the service, and no grant changes** (inspection F2): the grants file is
/// byte-identical before and after; the service writes the new type at
/// once; an agent's add is taken only where the owner's persisted setting
/// opens `grammar` (the variable alone does not reach the service); and a
/// read-only grant, which never holds the soup, lets the add run directly,
/// its grant (scopes and project) untouched. Nothing printed says to
/// revoke or grant. With `CAULDRON_OLD_MCP` (a service from before the
/// `GRAMMAR` verb), the refusal says to stop, add and start, and doing that
/// leaves the grants byte-identical too.
// RED: (a) the `GRAMMAR` seam verb removed from the service — the add is
// refused while the service holds the soup; (b) the old refusal text — it
// names `mcp revoke` and a bare `mcp grant`, which reset the grant.
#[test]
fn u2_a_grammar_added_while_the_service_holds_the_soup_changes_no_grant() {
    let s = books("u2grant");
    std::fs::write(s.0.join("film.grammar.json"), FILMS).unwrap();
    let ff = s.0.join("film.grammar.json");
    let f = ff.to_str().unwrap();
    let o = s.run(&["create", "ro", "--env", "dev"], &[]);
    assert_eq!(o.rc, 0, "{}", o.err);
    s.grant("kb");
    let p = soup_door::grants::canonical_project(&s.project()).unwrap();
    soup_door::grants::grant(&s.env(), "ro", &[Scope::Read], &[p], "tty").unwrap();
    let grants = || std::fs::read(s.mcp_dir().join("grants.json")).unwrap();
    let before = grants();
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let r = svc.call(&s, "soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t", "Opens the writer")]}));
    assert_ne!(r["isError"], json!(true), "{r}");
    // An agent, with only the variable: the service holds to the persisted setting.
    let o = s.run_with(&["soup", "grammar", "add", f, "--env", "dev", "--cauldron", "kb"], &pids, &[("CAULDRON_OWNER_ACTS", "grammar")]);
    assert_ne!(o.rc, 0, "{}", o.out);
    assert!(o.err.contains("owner_act_closed") && o.err.contains("owner-acts open --only grammar"), "{}", o.err);
    assert!(!s.soup("kb").join("film-list.db").exists(), "nothing was added");
    // At a terminal: sent to the service, which adds it through its own handle.
    let o = s.on_tty(&["soup", "grammar", "add", f, "--env", "dev", "--cauldron", "kb"], &pids, &[], None);
    assert_eq!(o.rc, 0, "{}", o.out);
    let v = printed(&o);
    assert_eq!((v["outcome"].clone(), v["by"].clone(), v["forwarded_to"]["pid"].clone()), (json!("added"), json!("tty"), json!(svc.pid())), "{v}");
    assert!(!o.out.contains("revoke") && !o.out.contains("mcp grant"), "{}", o.out);
    let film = svc.call(&s, "soup_put", json!({"cauldron": "kb", "remint": true, "records": [
        {"id": "f", "type": "film", "handle": "The film", "edges": [{"kind": "based_on", "to": "BK-1"}]}]}));
    assert_ne!(film["isError"], json!(true), "the service writes the new type at once: {film}");
    let st = text(&svc.call(&s, "soup_status", json!({"cauldron": "kb"})));
    assert!(st["pools"].as_array().unwrap().iter().any(|p| p["pool"] == "film-list"), "{st}");
    // The same add again: already there.
    let o = s.on_tty(&["soup", "grammar", "add", f, "--env", "dev", "--cauldron", "kb"], &pids, &[], None);
    assert_eq!(printed(&o)["outcome"], "already-added", "{}", o.out);
    // A read-only grant: the service never holds that soup; the add is direct.
    let rl = s.0.join("reading-list.grammar.json");
    let o = s.on_tty(&["soup", "grammar", "add", rl.to_str().unwrap(), "--env", "dev", "--cauldron", "ro"], &pids, &[], None);
    assert_eq!(o.rc, 0, "{}", o.out);
    assert!(printed(&o).get("forwarded_to").is_none(), "{}", o.out);
    let audit: Vec<Value> = s.calls().into_iter().filter(|c| c["tool"] == "soup_grammar_add").collect();
    assert_eq!(audit.iter().map(|c| c["outcome"].as_str().unwrap()).collect::<Vec<_>>(), ["owner_act_closed", "added", "already-added"], "{audit:?}");
    svc.stop();
    assert_eq!(grants(), before, "the grants file is byte-identical");
    assert_eq!(s.grant_row("ro").unwrap()["scopes"], json!(["read"]));

    let Some(old) = std::env::var_os("CAULDRON_OLD_MCP") else {
        eprintln!("u2-grant: CAULDRON_OLD_MCP names no binary; the older service's refusal is not checked");
        return;
    };
    let mut more: Value = serde_json::from_str(FILMS).unwrap();
    more["kind"] = json!("song-list");
    more["node_types"] = json!({"song": {"id_prefix": "SG", "fields": {"handle": {"required": true, "kind": "text", "min_chars": 1, "max_chars": 235}}}});
    more["edge_kinds"] = json!({});
    let sf = s.write("song.grammar.json", &more);
    let svc = Service::start_bin(&s, Path::new(&old));
    let pids = [svc.pid()];
    let r = svc.call(&s, "soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t2", "Opens the older writer")]}));
    assert_ne!(r["isError"], json!(true), "{r}");
    let o = s.on_tty(&["soup", "grammar", "add", sf.to_str().unwrap(), "--env", "dev", "--cauldron", "kb"], &pids, &[], None);
    assert_ne!(o.rc, 0, "{}", o.out);
    assert!(o.out.contains("cauldron --env dev stop mcp") && o.out.contains("cauldron --env dev start mcp"), "{}", o.out);
    assert!(!o.out.contains("mcp revoke") && o.out.contains("do NOT revoke"), "{}", o.out);
    assert!(!s.soup("kb").join("song-list.db").exists());
    // Followed: stop, add, start; the grants are as they were.
    svc.stop();
    let o = s.on_tty(&["soup", "grammar", "add", sf.to_str().unwrap(), "--env", "dev", "--cauldron", "kb"], &[], &[], None);
    assert_eq!(printed(&o)["outcome"], "added", "{}", o.out);
    let svc = Service::start_bin(&s, Path::new(&old));
    let st = text(&svc.call(&s, "soup_status", json!({"cauldron": "kb"})));
    assert!(st.is_object(), "{st}");
    svc.stop();
    assert_eq!(grants(), before, "stop, add, start: the grants file is byte-identical");
}

/// **Links, citations and region filters on user records** (inspection
/// F3), through the command line and through the service: a link from a
/// user record its grammar declares is written, and removed; one from a
/// workspace record to a user record is refused `no-edge-into-a-user-kind`;
/// an edge kind the user grammar lacks is refused naming it; a citation is
/// added to a user record whose type declares `meta`, and refused
/// `no-meta-field` on one whose type does not; a region filter with a user
/// type is refused saying user kinds have no region membership. None says
/// the records do not exist.
// RED: `edges::endpoints` reading `ws` only — every one of these is
// refused `unknown-node` ("is not a node"), and the region filter answers
// "there is no region `R-1`".
#[test]
fn u2_links_citations_and_region_filters_reach_user_records_or_say_why_not() {
    let s = books("u2link");
    std::fs::write(s.0.join("film.grammar.json"), FILMS).unwrap();
    let o = s.run_with(&["soup", "grammar", "add", s.0.join("film.grammar.json").to_str().unwrap(), "--env", "dev", "--cauldron", "kb"], &[], &[("CAULDRON_OWNER_ACTS", "grammar")]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let fm = s.write("film.json", &json!([{"id": "FM-1", "type": "film", "handle": "The film", "edges": [{"kind": "based_on", "to": "BK-1"}]}]));
    assert_eq!(s.import("kb", &fm, &[], &[]).rc, 0);
    let k = ["--env", "dev", "--cauldron", "kb"];
    let run = |args: &[&str]| s.run(&[args, &k[..]].concat(), &[]);
    let o = run(&["soup", "link", "BK-1", "about", "Q-1", "--note", "asks it"]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    assert_eq!((o.json()["outcome"].clone(), o.json()["links"][0]["from_pool"].clone()), (json!("linked"), json!("reading-list")), "{}", o.out);
    let book = s.record("BK-1");
    assert!(book["edges"].as_array().unwrap().iter().any(|e| e["kind"] == "about" && e["to"] == "Q-1@ws" && e["note"] == "asks it"), "{book}");
    let o = run(&["soup", "link", "BK-1", "about", "Q-1"]);
    assert_eq!(o.json()["outcome"], "already-linked", "{}", o.out);
    let o = run(&["soup", "unlink", "BK-1", "about", "DEC-1"]);
    assert_eq!(o.json()["outcome"], "unlinked", "{}{}", o.out, o.err);
    let o = run(&["soup", "link", "BK-1", "written_by", "AU-1"]);
    assert_eq!(o.json()["outcome"], "already-linked", "user to user, as written: {}{}", o.out, o.err);
    let refused = |args: &[&str], reason: &str| {
        let o = run(args);
        assert_eq!(o.rc, 1, "{args:?}: {}{}", o.out, o.err);
        assert_eq!(o.json()["links"][0]["refused"]["reason"], reason, "{args:?}: {}", o.out);
        assert!(!o.out.contains("is not a node") && !o.out.contains("unknown-node"), "{}", o.out);
        o.json()["links"][0]["refused"]["detail"].as_str().unwrap_or("").to_string()
    };
    let d = refused(&["soup", "link", "DEC-1", "relates", "BK-1"], "no-edge-into-a-user-kind");
    assert!(d.contains("a built-in grammar declares no edge into a user kind"), "{d}");
    let d = refused(&["soup", "link", "BK-1", "ref", "Q-1"], "unknown-kind");
    assert!(d.contains("pool `reading-list`") && d.contains("reading-list@1"), "{d}");
    refused(&["soup", "link", "FM-1", "based_on", "DEC-1"], "door-refused");
    let o = run(&["soup", "cite", "BK-1", "--add", r#"{"kind":"url","url":"https://example.com/b"}"#]);
    assert_eq!((o.json()["outcome"].clone(), o.json()["pool"].clone()), (json!("cited"), json!("reading-list")), "{}{}", o.out, o.err);
    assert_eq!(s.record("BK-1")["meta"]["cites"][0]["url"], "https://example.com/b");
    let o = run(&["soup", "cite", "FM-1", "--add", r#"{"kind":"url","url":"https://example.com/f"}"#]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("declares no `meta` field on `film`"), "{}", o.err);
    for verb in [&["soup", "nodes", "--type", "book", "--region", "R-1"][..], &["soup", "search", "graphs", "--type", "book", "--region", "R-1"][..]] {
        let o = run(verb);
        assert_ne!(o.rc, 0, "{verb:?}");
        assert!(o.err.contains("user kinds have no region membership in this release") && !o.err.contains("there is no region"), "{verb:?}: {}", o.err);
    }
    // The same through the service.
    s.grant("kb");
    let svc = Service::start(&s);
    let r = svc.call(&s, "soup_link", json!({"cauldron": "kb", "from": "BK-1", "kind": "about", "to": "DEC-1"}));
    assert_ne!(r["isError"], json!(true), "{r}");
    assert_eq!(text(&r)["outcome"], "linked", "{r}");
    let r = svc.call(&s, "soup_unlink", json!({"cauldron": "kb", "from": "BK-1", "kind": "about", "to": "Q-1"}));
    assert_eq!(text(&r)["outcome"], "unlinked", "{r}");
    let r = svc.call(&s, "soup_link", json!({"cauldron": "kb", "from": "Q-1", "kind": "relates", "to": "BK-1"}));
    assert_eq!(r["isError"], json!(true), "{r}");
    assert!(r.to_string().contains("no-edge-into-a-user-kind"), "{r}");
    let r = svc.call(&s, "soup_cite", json!({"cauldron": "kb", "id": "AU-1", "add": {"kind": "url", "url": "https://example.com/a2"}}));
    assert_eq!(text(&r)["outcome"], "cited", "{r}");
    let r = svc.call(&s, "soup_nodes", json!({"cauldron": "kb", "type": "book", "region": "R-1"}));
    assert_eq!(r["isError"], json!(true), "{r}");
    assert!(r.to_string().contains("no region membership"), "{r}");
    let r = svc.call(&s, "soup_nodes", json!({"cauldron": "kb", "type": "book"}));
    assert_eq!(text(&r)["total"], 1, "{r}");
    svc.stop();
}

/// **A user pool file swapped in from another cauldron is refused, and
/// says the way out** (inspection F8, F4, at the command line): reads that
/// reach a user pool and every write are refused naming the file and what
/// to do; `grammar list` shows the pool's problem instead of failing; a
/// read of the workspace's own records still answers. The file put back,
/// everything answers as before.
// RED: `grammars::pins` and the spine's open not judging the pool files —
// `soup get BK-1` answers the other cauldron's book.
#[test]
fn u2_a_pool_file_from_another_cauldron_is_refused_with_the_way_out() {
    let s = books("u2swap");
    let o = s.run(&["create", "other", "--env", "dev"], &[]);
    assert_eq!(o.rc, 0, "{}", o.err);
    let file = s.0.join("reading-list.grammar.json");
    let o = s.run_with(&["soup", "grammar", "add", file.to_str().unwrap(), "--env", "dev", "--cauldron", "other"], &[], &[("CAULDRON_OWNER_ACTS", "grammar")]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let theirs = s.write("theirs.json", &json!([{"id": "b", "type": "book", "handle": "Another cauldron's book"}]));
    assert_eq!(s.import("other", &theirs, &["--remint"], &[]).rc, 0);
    let mine = s.soup("kb").join("reading-list.db");
    let kept = s.0.join("reading-list.kept");
    std::fs::rename(&mine, &kept).unwrap();
    for side in ["-wal", "-shm"] {
        let _ = std::fs::rename(s.soup("kb").join(format!("reading-list.db{side}")), s.0.join(format!("reading-list.kept{side}")));
    }
    std::fs::copy(s.soup("other").join("reading-list.db"), &mine).unwrap();
    let k = ["--env", "dev", "--cauldron", "kb"];
    let o = s.run(&[&["soup", "get", "BK-1"][..], &k].concat(), &[]);
    assert_ne!(o.rc, 0, "{}", o.out);
    assert!(o.err.contains("is not this soup's own") && o.err.contains("Put back this cauldron's own pool file"), "{}", o.err);
    let o = s.import("kb", &s.write("t.json", &json!([task("t", "A write while it is so")])), &["--remint"], &[]);
    assert_ne!(o.rc, 0, "{}", o.out);
    assert!(o.err.contains("is not this soup's own"), "{}", o.err);
    let list = s.run(&[&["soup", "grammar", "list"][..], &k].concat(), &[]);
    assert_eq!(list.rc, 0, "{}", list.err);
    let v = list.json();
    assert!(v["refused"].as_str().unwrap().contains("is not this soup's own"), "{v}");
    assert!(v["pools"].as_array().unwrap().iter().any(|p| p["pool"] == "reading-list" && p["problems"][0]["detail"].as_str().is_some_and(|d| d.contains("is not this soup's own"))), "{v}");
    let ws = s.run(&[&["soup", "get", "DEC-1"][..], &k].concat(), &[]);
    assert_eq!((ws.rc, ws.json()["records"][0]["id"].clone()), (0, json!("DEC-1")), "the workspace's own records still read: {}", ws.err);
    std::fs::remove_file(&mine).unwrap();
    std::fs::rename(&kept, &mine).unwrap();
    for side in ["-wal", "-shm"] {
        let _ = std::fs::rename(s.0.join(format!("reading-list.kept{side}")), s.soup("kb").join(format!("reading-list.db{side}")));
    }
    let o = s.run(&[&["soup", "get", "BK-1"][..], &k].concat(), &[]);
    assert_eq!((o.rc, o.json()["records"][0]["handle"].clone()), (0, json!("A book about graphs")), "{}{}", o.out, o.err);
}
