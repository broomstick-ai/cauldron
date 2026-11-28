//! **Writing back what you read.**
//!
//! A record read with `soup_get` (or `cauldron soup get`) carries more than
//! a write may: keys a read computes, and keys only the owner or the door
//! sets. This module is the one place that says which, per key, so a
//! get-then-put round trip never needs a hand-made strip list:
//!
//! - [`IGNORED`]: keys a READ ADDS that are not stored on the record
//!   (`superseded_by`, `unreviewed`, a top-level `rejected: true`, a merged
//!   record's `merged`, a cluster's `claims` list). Every record write (`soup_put`, `cauldron
//!   soup import`) drops them before judging and names each one it dropped
//!   in the answer's `ignored` list. Dropping one changes nothing a write
//!   could have done: none of them is a stored field.
//! - [`REFUSED`]: keys a write may NOT carry, which a read can show:
//!   the review flags (`meta.approved`, `meta.rejected`, written only by
//!   the owner's decision, which since `owner_acts` may be an agent's where
//!   the owner opened it, through `soup_approve` / `soup_reject`, never a
//!   record write),
//!   `meta.ripple` (the door's, on a ripple's records only; the one other
//!   writer is `cauldron merge`, which keeps it on a ripple's record whose
//!   decision it carries), `meta.merged_from` (a merge's, on the records it
//!   carries) and an `id`
//!   the soup already holds (a write only creates). These stay refused,
//!   by name, so nothing about who reviewed or who wrote a record can be
//!   written by a session; a client strips them itself, from this list.
//!
//! [`document`] is that list as JSON, published as
//! `schema/soup/write-back.json` (a test keeps the file and this module the
//! same). It is door policy beside the grammar: the workspace@6 and
//! cauldron@1 schema files are unchanged.

use serde_json::{json, Value};

/// One key a read shows that a write treats specially.
pub struct Key {
    /// The key, dotted from the record's top level.
    pub key: &'static str,
    /// The record types it appears on (`*`: any).
    pub on: &'static str,
    /// What a write does with it, and why.
    pub why: &'static str,
}

/// Keys a read adds and a write drops (and lists in `ignored`).
pub const IGNORED: &[Key] = &[
    Key {
        key: "superseded_by",
        on: "*",
        why: "computed by reads from the `supersedes` edges pointing at the record; to supersede, write a new record with a `supersedes` edge",
    },
    Key {
        key: "unreviewed",
        on: "*",
        why: "computed by reads: a record a ripple wrote that the owner has not approved or rejected",
    },
    Key {
        key: "rejected",
        on: "*",
        why: "the top-level `rejected: true` reads add to a record the owner rejected (the flag itself is `meta.rejected`, which is refused); dropped only when it is `true`, as reads write it",
    },
    Key {
        key: "merged",
        on: "*",
        why: "computed by reads from the transaction that created the record: the merge that carried it there and where it came from (`cauldron merge`); the record's own provenance is `meta.merged_from`, which is refused",
    },
    Key {
        key: "claims",
        on: "cluster",
        why: "computed by reads from the cluster's `claims` edges, with each region's handle; a claim is written as a `claims` edge or with soup_claim",
    },
];

/// Keys a read can show that a write refuses: strip them before writing.
pub const REFUSED: &[Key] = &[
    Key {
        key: "meta.approved",
        on: "*",
        why: "the owner's review flag, set only by the owner's decision (`cauldron ripples approve`, `cauldron soup approve` at a terminal, or, where the owner opened approving to agents, those commands without one and the MCP tool `soup_approve`), never by a record write; refused in any casing, set or null",
    },
    Key {
        key: "meta.rejected",
        on: "*",
        why: "the owner's rejection flag, set only by the owner's decision (`cauldron ripples reject` at a terminal, or, where the owner opened approving to agents, without one and the MCP tool `soup_reject`), never by a record write; refused in any casing, set or null",
    },
    Key {
        key: "meta.ripple",
        on: "*",
        why: "ripple provenance, stamped by the door on the records a ripple writes and only there (refused: ripple_provenance)",
    },
    Key {
        key: "meta.merged_from",
        on: "*",
        why: "merge provenance (where a record came from, who wrote it there, the decision a merge carried), written only by `cauldron merge` on the records it carries; refused in any casing, dotted or nested, anywhere a record write, a link, a claim, a cite or a ripple's batch carries it (refused: merged_from)",
    },
    Key {
        key: "id",
        on: "*",
        why: "a write only creates: an id the soup holds is refused (`duplicate_node_id`; under remint, `label-shadows-existing`); give the record a new label (remint) or the next free id, and a `supersedes` edge to the old one if it replaces it",
    },
];

/// What a read may have cut, which a round trip would write back cut.
pub const CUT: &[Key] = &[
    Key {
        key: "edges[].note",
        on: "*",
        why: "a note stored before the 1,000-character bound reads back cut to 1,000 (named in the read's `truncated`); written back, the shorter note is what lands",
    },
    Key {
        key: "body, handle and other text fields",
        on: "*",
        why: "a read bounded by `max_chars` cuts long fields and names them in `truncated`; read again with a larger bound before writing a record back",
    },
];

fn keys(list: &[Key]) -> Value {
    Value::Array(list.iter().map(|k| json!({"key": k.key, "on": k.on, "why": k.why})).collect())
}

/// The write-back list as JSON: the content of `schema/soup/write-back.json`.
pub fn document() -> Value {
    json!({
        "$comment": "GENERATED from libs/soup-door/src/writeback.rs. Do not edit; a test keeps the two the same.",
        "description": "Writing back a record read from the soup (soup_get, cauldron soup get) with soup_put or cauldron soup import. `ignored`: keys reads add, which every record write drops and names in its answer's `ignored` list. `refused`: keys a write refuses, which a client strips itself before writing back. `cut`: what a read may have shortened.",
        "ignored": keys(IGNORED),
        "refused": keys(REFUSED),
        "cut": keys(CUT),
    })
}

/// Drop the keys reads add ([`IGNORED`]) from each record. Answers the
/// records to judge and, for each key dropped, `{record, key}` (the record
/// as its `id` names it, or `#<index>`). Every other key, including any
/// other spelling of these, is left for the grammar and the door to judge.
pub fn strip_read_only(records: &[Value]) -> (Vec<Value>, Vec<Value>) {
    let mut ignored = Vec::new();
    let out = records
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let mut r = r.clone();
            let label = r.get("id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("#{i}"));
            let is_cluster = r.get("type").and_then(Value::as_str) == Some(spine::grammar_cauldron::CLUSTER);
            if let Some(o) = r.as_object_mut() {
                for k in IGNORED {
                    let drop = match k.key {
                        "rejected" => o.get("rejected") == Some(&Value::Bool(true)),
                        "claims" => is_cluster && o.get("claims").is_some_and(Value::is_array),
                        key => o.contains_key(key),
                    };
                    if drop {
                        o.remove(k.key);
                        ignored.push(json!({"record": label, "key": k.key}));
                    }
                }
            }
            r
        })
        .collect();
    (out, ignored)
}

/// `doc` with `ignored` set when anything was dropped.
pub fn note_ignored(doc: &mut Value, ignored: &[Value]) {
    if !ignored.is_empty() && doc.is_object() {
        doc["ignored"] = Value::Array(ignored.to_vec());
        doc["ignored_why"] = json!("keys a read adds (not stored on a record) were dropped before judging; see schema/soup/write-back.json");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published file is this module's list, byte for byte.
    #[test]
    fn the_published_write_back_list_is_this_module() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/soup/write-back.json");
        let want = format!("{}\n", serde_json::to_string_pretty(&document()).unwrap());
        let have = std::fs::read_to_string(&path).unwrap_or_default();
        if std::env::var_os("CAULDRON_WRITE_BACK_REGEN").is_some() {
            std::fs::write(&path, &want).unwrap();
            return;
        }
        assert_eq!(have, want, "schema/soup/write-back.json is stale: run this test with CAULDRON_WRITE_BACK_REGEN=1");
    }

    #[test]
    fn what_reads_add_is_dropped_and_named_and_nothing_else() {
        let recs = vec![
            json!({"id": "I-3", "type": "idea", "superseded_by": ["I-4"], "unreviewed": true, "rejected": true, "meta": {"approved": true}}),
            json!({"id": "CL-1", "type": "cluster", "claims": [{"region": "R-1"}]}),
            json!({"id": "I-5", "type": "idea", "claims": [], "rejected": false, "Unreviewed": true}),
        ];
        let (out, ignored) = strip_read_only(&recs);
        assert_eq!(out[0], json!({"id": "I-3", "type": "idea", "meta": {"approved": true}}), "the flag is not dropped: it stays refused");
        assert_eq!(out[1], json!({"id": "CL-1", "type": "cluster"}));
        assert_eq!(out[2], recs[2], "not a cluster, not `true`, another casing: left for the door to judge");
        let keys: Vec<String> = ignored.iter().map(|v| format!("{}:{}", v["record"].as_str().unwrap(), v["key"].as_str().unwrap())).collect();
        assert_eq!(keys, ["I-3:superseded_by", "I-3:unreviewed", "I-3:rejected", "CL-1:claims"]);
    }
}
