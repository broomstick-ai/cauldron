//! **The ways out of a merge or an undo that cannot be finished.**
//!
//! Every file the merge and undo machinery depends on can be moved, deleted
//! or changed by something outside cauldron: the snapshot (or one file in
//! it), the undo's marker, the merge's journal or record, the staged blobs,
//! the directory an undo moves what it replaces into. Whatever is missing,
//! there is a command that gets the target writable again without
//! hand-editing files, and every refusal names it:
//!
//! - an undo cut short that cannot be finished (its snapshot gone or
//!   damaged, its marker unreadable): `cauldron soup recover
//!   --abandon-undo` RETURNS what the undo had already put back (it keeps
//!   what it replaced in a directory beside the cauldron, outside the
//!   snapshot, so that survives the snapshot going), lifts the undo's hold
//!   and sets its marker aside: the cauldron is as the merge left it, and it
//!   says exactly what it returned and anything it could not;
//! - a merge cut short whose journal is gone or unreadable, or a hold of a
//!   merge or an undo with nothing left that names it: `cauldron soup
//!   recover --abandon-merge` / `--abandon-undo` lifts it, saying so.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use super::{dir, halted_with, merge_serial, pending, release_store, snapshots_of, undo_pending, undoing_path, JOURNAL, UNDOING};
use crate::provenance;
use crate::{dumps, refuse, Code, Error, Result};

fn store(err: impl std::fmt::Display) -> Error {
    Error::new(Code::Store, err.to_string())
}

/// The start of the reason an undo holds the stores with
/// (`super::undo_hold_reason`).
pub(crate) const UNDO_HOLD_PREFIX: &str = "cauldron merge --undo ";

/// The words in the reason a merge holds the stores with
/// (`super::hold_reason`).
const MERGE_HOLD_WORDS: &str = "is landing or was cut short";

/// The reason the spine at `dir` is halted with, if it is halted (read only;
/// `None` for a store with no spine).
pub(crate) fn halt_reason(dir: &Path) -> Result<Option<String>> {
    if !dir.join(spine::spine_db::SPINE_FILE).is_file() {
        return Ok(None);
    }
    // Read only, in place: one row, no private copy of the file.
    let conn = rusqlite::Connection::open_with_flags(
        dir.join(spine::spine_db::SPINE_FILE),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(store)?;
    let (h, r): (i64, Option<String>) =
        conn.query_row("SELECT halted, halted_reason FROM spine_state WHERE singleton = 1", [], |x| Ok((x.get(0)?, x.get(1)?))).map_err(store)?;
    Ok((h != 0).then(|| r.unwrap_or_default()))
}

/// The merge id a hold reason of a merge or an undo names.
fn merge_of_reason(reason: &str) -> Option<String> {
    let rest = reason.strip_prefix(UNDO_HOLD_PREFIX).or_else(|| reason.strip_prefix("merge "))?;
    let id: String = rest.chars().take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-').collect();
    merge_serial(&id).map(|_| id)
}

/// A hold of an undo on either store of `c`: `(store dir, reason)`.
fn undo_holds(c: &env::Cauldron) -> Result<Vec<(PathBuf, String)>> {
    let mut out = Vec::new();
    for d in [c.soup(), dumps::dir(c)] {
        if let Some(r) = halt_reason(&d)? {
            if r.starts_with(UNDO_HOLD_PREFIX) {
                out.push((d, r));
            }
        }
    }
    Ok(out)
}

/// A hold of a merge on either store of `c`: `(store dir, reason)`.
fn merge_holds(c: &env::Cauldron) -> Result<Vec<(PathBuf, String)>> {
    let mut out = Vec::new();
    for d in [c.soup(), dumps::dir(c)] {
        if let Some(r) = halt_reason(&d)? {
            if r.starts_with("merge ") && r.contains(MERGE_HOLD_WORDS) {
                out.push((d, r));
            }
        }
    }
    Ok(out)
}

/// The commands that settle `c`, for refusals.
pub(crate) fn recover_cmd(c: &env::Cauldron, flag: &str) -> String {
    format!("cauldron soup recover --env {} --cauldron {}{}", c.env, c.name, if flag.is_empty() { String::new() } else { format!(" {flag}") })
}

/// **A hold with nothing left that names it** (`cauldron soup recover`
/// before it opens the soup): a store halted by an undo with no marker,
/// or by a merge with no journal still landing. Refused, naming the flag
/// that lifts it, rather than reported as an import not at rest.
pub fn refuse_orphan_hold(c: &env::Cauldron) -> Result<()> {
    // Only a cauldron something was merged into can hold a merge's or an
    // undo's hold: every other writer pays nothing here.
    if !dir(c).is_dir() {
        return Ok(());
    }
    if undo_pending(c).ok().flatten().is_none() {
        if let Some((d, r)) = undo_holds(c)?.into_iter().next() {
            refuse!(
                Code::NotAtRest,
                "REFUSED: {} of cauldron `{}` is held by an undo of merge {} (\"{}\"), and that undo's marker ({}) is gone, so \
                 the undo can be neither finished nor told apart from a cauldron at rest. Nothing was written. To lift the hold \
                 and keep the cauldron as it now is, run `{}`: it says what it could tell of how far the undo got.",
                d.display(),
                c.name,
                merge_of_reason(&r).unwrap_or_else(|| "?".into()),
                r,
                undoing_path(c).display(),
                recover_cmd(c, "--abandon-undo")
            );
        }
    }
    if matches!(pending(c), Ok(None)) {
        if let Some((d, r)) = merge_holds(c)?.into_iter().next() {
            refuse!(
                Code::NotAtRest,
                "REFUSED: {} of cauldron `{}` is held by merge {}, and no journal of that merge is still landing (it is gone or \
                 was changed), so recover cannot finish it. Nothing was written. To lift the hold and keep what of the merge \
                 landed, run `{}`.",
                d.display(),
                c.name,
                merge_of_reason(&r).unwrap_or_else(|| "?".into()),
                recover_cmd(c, "--abandon-merge")
            );
        }
    }
    Ok(())
}

/// **An undo cut short that cannot be finished**: the refusal every writer,
/// recover and the undo itself give, naming both ways out. `why` says what
/// is missing or damaged. The undo's hold and marker are kept.
pub(crate) fn stuck_undo(c: &env::Cauldron, m: &Value, why: &str, finished_by: &str) -> Error {
    let merge = m["merge"].as_str().unwrap_or("?");
    let snap = m["snapshot"].as_str().unwrap_or("?");
    let tried = if finished_by.is_empty() { String::new() } else { format!(" (finishing it was tried by {finished_by})") };
    Error::new(
        Code::NotAtRest,
        format!(
            "REFUSED: an undo of merge {merge} into cauldron `{}` was cut short, and it cannot be finished{tried}: \
             {why}. Until it is settled nothing of this release writes to the cauldron. Nothing was written. \
             Two ways out: put the snapshot back exactly as it was at {snap} and run `{}` (which finishes the undo); or keep \
             the merge and make the cauldron writable again with `{}`, which returns the cauldron to exactly what the merge left \
             (or refuses, changing nothing, and says why), lifts the undo's hold, and says what it did.",
            c.name,
            recover_cmd(c, ""),
            recover_cmd(c, "--abandon-undo")
        ),
    )
    .with_detail(json!({
        "merge": merge,
        "undo_stuck": {"why": why, "snapshot": snap, "step": m["state"]},
        "ways_out": [recover_cmd(c, ""), recover_cmd(c, "--abandon-undo")],
    }))
}

/// The marker of an undo cut short, read from the cauldron's directory, or
/// (when that is gone or unreadable) from the copy the undo keeps in its
/// snapshot, found by the merge its hold names. `(marker, where from,
/// what was wrong with the cauldron's copy)`.
fn read_marker(c: &env::Cauldron) -> (Option<Value>, String, Option<String>) {
    let damaged = match undo_pending(c) {
        Ok(Some(m)) => return (Some(m), undoing_path(c).display().to_string(), None),
        Ok(None) => None,
        Err(e) => Some(e.message),
    };
    // The merges a hold names first; then every snapshot beside the
    // cauldron (an undo whose stores are both put back holds nothing), the
    // newest marker copy of a merge this cauldron still has a directory for.
    let mut merges: Vec<String> = undo_holds(c).unwrap_or_default().iter().filter_map(|(_, r)| merge_of_reason(r)).collect();
    let base = env::private::data_dir(c);
    let name = base.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    let mut others: Vec<(String, String)> = std::fs::read_dir(base.parent().unwrap_or(Path::new(".")))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            let (merge, stamp) = n.strip_prefix(&format!("{name}."))?.split_once("-before-")?;
            (merge_serial(merge).is_some() && e.path().join(UNDOING).is_file()).then(|| (stamp.to_string(), merge.to_string()))
        })
        .collect();
    others.sort();
    merges.extend(others.into_iter().rev().map(|(_, m)| m));
    for merge in merges {
        for snap in snapshots_of(c, &merge).into_iter().rev() {
            let p = snap.join(UNDOING);
            if let Some(v) = std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
                if v["merge"] == json!(merge) {
                    return (Some(v), p.display().to_string(), damaged);
                }
            }
        }
    }
    (None, String::new(), damaged)
}

/// Which part of the cauldron a path of the snapshot scope is in.
fn part_of(rel: &str, soup_name: &str) -> &'static str {
    if rel.starts_with(&format!("{}/", dumps::DIR)) {
        "dumps"
    } else if rel.starts_with(&format!("{soup_name}/")) {
        "soup"
    } else {
        "rest"
    }
}

/// The copy of `rel` the undo moved aside into `aside` whose digest is
/// `want`, if it is there (`rel`, `rel.1`, `rel.2`, ...).
fn aside_copy(aside: &Path, rel: &str, want: &(String, u64)) -> Option<PathBuf> {
    let mut cands = vec![aside.join(rel)];
    cands.extend((1..64).map(|n| aside.join(format!("{rel}.{n}"))));
    cands.into_iter().filter(|p| p.is_file()).find(|p| crate::snapshot::digest(p).ok().as_ref() == Some(want))
}

/// Move the file at `from` to `into/rel` (a new name when that is taken).
fn move_into(from: &Path, into: &Path, rel: &str) -> Result<()> {
    env::private::make_dir(into).map_err(|e| store(format!("making {}: {e}", into.display())))?;
    let mut at = into.to_path_buf();
    for part in Path::new(rel).parent().into_iter().flat_map(|p| p.components()) {
        at = at.join(part);
        env::private::make_dir(&at).map_err(|e| store(format!("making {}: {e}", at.display())))?;
    }
    let mut to = into.join(rel);
    let mut n = 1;
    while to.exists() {
        to = into.join(format!("{rel}.{n}"));
        n += 1;
    }
    std::fs::rename(from, &to).map_err(|e| store(format!("moving {} to {}: {e}", from.display(), to.display())))
}

/// What the owner chose for `cauldron soup recover --abandon-undo`.
#[derive(Debug, Clone, Copy, Default)]
pub struct AbandonUndo {
    /// `--lose-undo`: abandon an undo that could still be finished (its
    /// snapshot whole), keeping the merge and losing the undo.
    pub lose_undo: bool,
    /// `--set-aside-later-writes`: a store the undo had already put back
    /// was written since; return it to the merged state anyway, moving the
    /// files those later writes changed into the abandon directory.
    pub set_aside_later_writes: bool,
}

/// A path of the snapshot scope that is never compared: SQLite's shared
/// memory and rollback files, the undo's own temporary copies. A `-wal` is
/// compared only when it holds something (an empty one is what any reader,
/// this command's own included, leaves).
fn not_compared(rel: &str, size: u64, recorded: bool) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    rel.ends_with("-shm") || rel.ends_with("-journal") || name.starts_with(".restoring-") || (rel.ends_with("-wal") && size == 0 && !recorded)
}

/// **What merge `merge` carried, and where each part of it is now**, read
/// from the stores (never from a marker). What it carried comes from its
/// record (the soup writes it made, by part, and the dumps it landed), or,
/// with the record gone, from its journal's plan; what is here, from the
/// soup's transactions the merge tagged and the dumps whose
/// `meta.merged_from` names it.
struct Holding {
    /// The soup writes the merge made (`main`, `ripple`, `links`); `None`
    /// when neither its record nor its journal can be read.
    soup_carried: Option<BTreeSet<String>>,
    soup_here: BTreeSet<String>,
    /// The dumps it carried, source id -> target id (`None`: unknown).
    dumps_carried: Option<BTreeMap<String, String>>,
    dumps_here: usize,
    records: Option<u64>,
    record: bool,
    /// What `--settle-merge` wrote into the record, if it settled it.
    settled: Value,
}

fn holding(c: &env::Cauldron, merge: &str) -> Holding {
    let read = |p: PathBuf| -> Option<Value> { std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str(&t).ok()) };
    let soup_here: BTreeSet<String> =
        provenance::merge_txs(&c.soup()).map(|m| m.into_values().filter(|x| x.merge == merge).map(|x| x.part).collect()).unwrap_or_default();
    let dumps_here = super::landed_dumps(c, merge).map(|m| m.len()).unwrap_or(0);
    let record = read(dir(c).join(merge).join(super::RECORD));
    let journal = read(dir(c).join(merge).join(JOURNAL));
    let map = |v: &Value| -> Option<BTreeMap<String, String>> { serde_json::from_value(v.clone()).ok() };
    let (soup_carried, dumps_carried, records) = match (&record, &journal) {
        (Some(r), _) => {
            let soup = match r["soup_writes"].as_object() {
                Some(o) => Some(o.keys().cloned().collect()),
                // A record without `soup_writes`: one write, when it carried records.
                None => r["records"].as_u64().map(|n| if n > 0 { BTreeSet::from(["main".to_string()]) } else { BTreeSet::new() }),
            };
            (soup, map(&r["dumps"]), r["records"].as_u64())
        }
        // No record: what the plan meant to carry. The plan's records are
        // one or more soup writes; which parts, the plan does not say, so
        // any of them counts.
        (None, Some(j)) => {
            let n = j["id_map"].as_object().map(|o| o.len() as u64);
            (n.map(|n| if n > 0 { BTreeSet::from(["any".to_string()]) } else { BTreeSet::new() }), map(&j["dump_map"]), n)
        }
        (None, None) => (None, None, None),
    };
    let settled = record.as_ref().map(|r| r["settled"].clone()).unwrap_or(Value::Null);
    Holding { soup_carried, soup_here, dumps_carried, dumps_here, records, record: record.is_some(), settled }
}

impl Holding {
    /// The soup holds every write the merge made (none, for a merge that
    /// carried dumps alone).
    fn soup_whole(&self) -> bool {
        match &self.soup_carried {
            Some(want) if want.contains("any") => !self.soup_here.is_empty(),
            Some(want) => want.is_subset(&self.soup_here),
            None => !self.soup_here.is_empty(),
        }
    }
    fn dumps_whole(&self) -> bool {
        self.dumps_carried.as_ref().is_none_or(|d| self.dumps_here >= d.len())
    }
    fn nothing_here(&self) -> bool {
        self.soup_here.is_empty() && self.dumps_here == 0
    }
    /// Did the merge write records to the soup at all?
    fn wrote_soup(&self) -> Option<bool> {
        self.soup_carried.as_ref().map(|s| !s.is_empty())
    }
}

/// Every cite of a dump the dumps store does not hold, the records of
/// every pool of the soup: the record, the dump id it cites, and whether
/// that cite can be changed (`cauldron soup cite`): not on a record a ripple
/// wrote, whose `dump` cite is its review trail.
struct Dangling {
    record: String,
    cites: String,
    editable: bool,
}

fn dangling_cites(c: &env::Cauldron) -> Vec<Dangling> {
    let have: BTreeSet<String> = dumps::all_nodes(c).map(|v| v.into_iter().map(|n| n.id.0).collect()).unwrap_or_default();
    let mut out = Vec::new();
    for pool in super::pools_of(&c.soup()).unwrap_or_default() {
        let p = spine::pool::Pool::path_in(&c.soup(), &spine::schema_def::PoolName::new(&pool));
        // Read on a private copy (the spine's offline reader): a status
        // read during an undo must leave nothing beside the pool's files.
        let Ok(conn) = spine::offline::read_only_connection(&p) else { continue };
        let Ok(mut st) = conn.prepare("SELECT node_id, value FROM node_fields WHERE field = 'meta' ORDER BY node_id") else { continue };
        let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) else { continue };
        for (id, v) in rows.flatten() {
            let v: Value = serde_json::from_str(&v).unwrap_or(Value::Null);
            for cite in v["cites"].as_array().into_iter().flatten() {
                if cite["kind"] == "dump" {
                    if let Some(d) = cite["id"].as_str().filter(|d| !have.contains(*d)) {
                        let editable = !crate::edges::ripple_written(c, &id).unwrap_or(false);
                        out.push(Dangling { record: id.clone(), cites: d.to_string(), editable });
                    }
                }
            }
        }
    }
    out
}

/// Where the merge stands in the stores themselves, judged from what it
/// CARRIED (records only, dumps only, or both): `merged`, when every soup
/// write it made, every dump it landed and its record are here (a merge
/// that carried dumps alone wrote nothing to the soup, and needs nothing
/// there); `before-merge`, when nothing of it is; `torn` otherwise, with
/// what is where, the records that cite a dump that is not there, and what
/// the owner can do. What `merge --status`, the abandon and the settle
/// report.
pub fn end_state(c: &env::Cauldron, merge: &str) -> Value {
    let h = holding(c, merge);
    let state = if h.record && h.soup_whole() && h.dumps_whole() {
        "merged"
    } else if !h.record && h.nothing_here() {
        "before-merge"
    } else {
        "torn"
    };
    let soup = match (h.wrote_soup(), h.soup_whole(), h.soup_here.is_empty()) {
        (Some(false), _, true) => "the merge wrote no records (it carried dumps only)".to_string(),
        (_, true, false) => "holds the merge's writes".to_string(),
        (_, false, false) => format!(
            "holds part of the merge's writes ({} of {})",
            h.soup_here.iter().cloned().collect::<Vec<_>>().join(", "),
            h.soup_carried.as_ref().map(|s| s.iter().cloned().collect::<Vec<_>>().join(", ")).unwrap_or_else(|| "?".into())
        ),
        _ => "does not hold the merge's writes".to_string(),
    };
    let carried = h.dumps_carried.as_ref().map(|d| d.len());
    let composition = match (h.wrote_soup(), carried) {
        (Some(true), Some(n)) if n > 0 => "records and dumps",
        (Some(true), _) => "records only",
        (Some(false), Some(n)) if n > 0 => "dumps only",
        (Some(false), _) => "nothing",
        (None, _) => "unknown (its record and journal are gone)",
    };
    let mut doc = json!({
        "state": state,
        "carried": {"what": composition, "records": h.records, "soup_writes": h.soup_carried, "dumps": carried},
        "soup": soup,
        "soup_writes_here": h.soup_here,
        "dumps": format!("{} of the merge's dumps{}", h.dumps_here, carried.map(|n| format!(" (it carried {n})")).unwrap_or_default()),
        "record": if h.record { "present" } else { "absent" },
    });
    if state == "merged" && !h.settled.is_null() {
        doc["settled"] = h.settled.clone();
        doc["note"] = json!(format!(
            "settled as it stands ({}): part of merge {merge} was taken away by hand and is listed under `settled.lost`",
            h.settled["at"].as_str().unwrap_or("?")
        ));
    }
    if state == "torn" {
        let dangling = dangling_cites(c);
        let by_target: BTreeMap<String, String> = h.dumps_carried.clone().unwrap_or_default().into_iter().map(|(s, t)| (t, s)).collect();
        let source = std::fs::read_to_string(dir(c).join(merge).join(super::RECORD))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|r| r["source"]["cauldron"].as_str().map(str::to_string));
        let mut what = vec![];
        if !h.soup_here.is_empty() && !h.dumps_whole() {
            what.push("records of the merge may cite dumps that are not there".to_string());
        }
        if !h.record {
            what.push("its record is gone, so it cannot be undone".to_string());
        } else if h.nothing_here() {
            what.push("its record describes writes the cauldron does not hold".to_string());
        } else if !h.soup_whole() && h.dumps_here > 0 {
            what.push("dumps of the merge are here without the records it wrote".to_string());
        }
        doc["note"] = json!(format!(
            "the cauldron holds part of merge {merge} and not the rest (soup {}, dumps {}, record {}){}",
            doc["soup"].as_str().unwrap_or(""),
            doc["dumps"].as_str().unwrap_or(""),
            doc["record"].as_str().unwrap_or(""),
            if what.is_empty() { String::new() } else { format!(": {}", what.join("; ")) }
        ));
        if !dangling.is_empty() {
            doc["missing_cites"] = json!(dangling
                .iter()
                .map(|d| json!({"record": d.record, "cites": d.cites, "source_dump": by_target.get(&d.cites), "can_be_changed": d.editable}))
                .collect::<Vec<_>>());
            let (env_, name) = (c.env.clone(), c.name.clone());
            let list = |v: &[&Dangling]| v.iter().take(10).map(|d| format!("{} cites {}", d.record, d.cites)).collect::<Vec<_>>().join(", ") + if v.len() > 10 { ", ..." } else { "" };
            let fix: Vec<&Dangling> = dangling.iter().filter(|d| d.editable).collect();
            let trail: Vec<&Dangling> = dangling.iter().filter(|d| !d.editable).collect();
            let copy_out = |d: &Dangling| match (by_target.get(&d.cites), &source) {
                (Some(s), Some(src_c)) => format!("the source still holds that note as {s} (`cauldron soup dump-get {s} --raw --env {env_} --cauldron {src_c}`)"),
                _ => "the note is wherever you keep it".to_string(),
            };
            let mut say = vec![format!(
                "{} cite(s) of a dump that is not in cauldron `{name}`. Merging the source again does NOT repair them: those records \
                 are already here (by where they came from) and are not carried again, and their dumps would land under new ids \
                 nothing cites",
                dangling.len()
            )];
            if let Some(d) = fix.first() {
                say.push(format!(
                    "These can be repaired by hand ({}): copy the note in and re-point the cite. For {}: {}; save it to a file, run `cauldron \
                     soup dump <that file> --env {env_} --cauldron {name}` (it answers a new id, DM-n), then `cauldron soup cite {} \
                     --remove '{{\"kind\":\"dump\",\"id\":\"{}\"}}' --env {env_} --cauldron {name}` and `cauldron soup cite {} --add \
                     '{{\"kind\":\"dump\",\"id\":\"DM-n\"}}' --env {env_} --cauldron {name}` (or only the `--remove`, to drop the cite)",
                    list(&fix),
                    d.record,
                    copy_out(d),
                    d.record,
                    d.cites,
                    d.record
                ));
            }
            if let Some(d) = trail.first() {
                say.push(format!(
                    "{} {} written by a ripple: a `dump` cite is that record's review trail and is never changed, so {} keep citing a \
                     dump that is gone ({} for {}, to read)",
                    list(&trail),
                    if trail.len() == 1 { "was" } else { "were" },
                    if trail.len() == 1 { "it will" } else { "they will" },
                    copy_out(d),
                    d.record
                ));
            }
            say.push(format!(
                "{}`{}` writes what was lost into the merge's record{}, and the cauldron is no longer torn. No dump will take a \
                 cited id.",
                if fix.is_empty() { String::new() } else { "Once no cite that can be changed is missing, ".into() },
                recover_cmd(c, &format!("--settle-merge {merge}")),
                if trail.is_empty() { "" } else { " (the ripple records' cites as lost with it)" }
            ));
            doc["repair"] = json!(say.join(". "));
        } else if h.dumps_here > 0 && h.soup_here.is_empty() && h.wrote_soup() == Some(true) {
            let ids: Vec<String> = super::landed_dumps(c, merge).map(|m| m.into_values().collect()).unwrap_or_default();
            doc["repair"] = json!(format!(
                "the dumps merge {merge} landed ({}) are here and the records it wrote are not; nothing cites those dumps. `{}` \
                 writes what was lost into the merge's record and keeps the dumps (remove any you do not want with `cauldron soup \
                 dump-remove <id> --env {} --cauldron {}`).",
                ids.join(", "),
                recover_cmd(c, &format!("--settle-merge {merge}")),
                c.env,
                c.name
            ));
        } else if h.record && h.nothing_here() {
            doc["repair"] = json!(format!(
                "nothing of the merge is in the stores but its record, so `{}` moves the record aside (nothing deleted), and the \
                 cauldron is as it was before the merge.",
                recover_cmd(c, &format!("--settle-merge {merge}"))
            ));
        } else if h.record {
            doc["repair"] = json!(format!(
                "no record cites a dump that is not here, so `{}` writes what was lost into the merge's record, and the cauldron is no \
                 longer torn.",
                recover_cmd(c, &format!("--settle-merge {merge}"))
            ));
        }
    }
    doc
}

/// **No dump id is handed out twice**: every dump id a record of the soup
/// cites is at or below the dumps store's last minted serial, which is
/// raised (never lowered) to the highest cited. Run under both writer locks
/// whenever an abandon cannot leave the stores consistent, so the next dump
/// never takes an id a record already cites. Answers the serial, raised or
/// not.
fn keep_dump_ids_ahead(c: &env::Cauldron) -> Result<Value> {
    let mut cited: u64 = 0;
    for pool in super::pools_of(&c.soup())? {
        let p = spine::pool::Pool::path_in(&c.soup(), &spine::schema_def::PoolName::new(&pool));
        let conn = rusqlite::Connection::open_with_flags(&p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(store)?;
        let mut st = conn.prepare("SELECT value FROM node_fields WHERE field = 'meta'").map_err(store)?;
        for v in st.query_map([], |r| r.get::<_, String>(0)).map_err(store)? {
            let v: Value = serde_json::from_str(&v.map_err(store)?).unwrap_or(Value::Null);
            for cite in v["cites"].as_array().into_iter().flatten() {
                if let Some(n) = cite["id"].as_str().and_then(|i| i.strip_prefix("DM-")).and_then(|n| n.parse::<u64>().ok()) {
                    cited = cited.max(n);
                }
            }
        }
    }
    let db = dumps::dir(c).join(spine::spine_db::SPINE_FILE);
    if cited == 0 || !db.is_file() {
        return Ok(json!({"highest_cited": cited, "raised": false}));
    }
    let conn = rusqlite::Connection::open(&db).map_err(store)?;
    let last: i64 = conn
        .query_row("SELECT last FROM id_serials WHERE pool = ?1 AND prefix = ?2", [spine::grammar_dump::POOL, spine::grammar_dump::DUMP_PREFIX], |r| r.get(0))
        .unwrap_or(0);
    if (last as u64) >= cited {
        return Ok(json!({"highest_cited": cited, "last_minted": last, "raised": false}));
    }
    conn.execute(
        "INSERT INTO id_serials (pool, prefix, last) VALUES (?1, ?2, ?3) ON CONFLICT(pool, prefix) DO UPDATE SET last = MAX(last, excluded.last)",
        rusqlite::params![spine::grammar_dump::POOL, spine::grammar_dump::DUMP_PREFIX, cited as i64],
    )
    .map_err(store)?;
    Ok(json!({"highest_cited": cited, "last_minted": cited, "was": last, "raised": true}))
}

/// **Can this undo still be finished?** Its marker readable, recorded
/// where it began (or nothing moved yet), and its snapshot whole.
fn finishable(c: &env::Cauldron, m: &Value) -> bool {
    if super::marker_unsound(c, m).is_some() {
        return false;
    }
    let snap = PathBuf::from(m["snapshot"].as_str().unwrap_or(""));
    let began = m["state"] == "verified" || (m["state"] == "restoring" && m["start"].as_object().is_some_and(|o| !o.is_empty()));
    began && snap.is_absolute() && snap.join(crate::snapshot::MANIFEST).is_file() && crate::snapshot::verify(&snap).is_ok()
}

/// **`cauldron soup recover --abandon-undo`**: settle an undo cut short
/// that cannot be finished, KEEPING THE MERGE: the cauldron ends exactly as
/// the merge left it, or the command refuses and changes nothing. Never a
/// cauldron with one store from each side. The caller holds the soup's
/// writer lock (and has checked the owner act); the dumps store's is taken
/// here.
///
/// - An undo that CAN be finished (marker readable, snapshot whole) is
///   refused, pointing at `cauldron soup recover`, unless `--lose-undo`.
/// - The cauldron's files are listed before anything here opens a
///   database; SQLite's `-shm`, an empty `-wal` and the undo's own
///   temporary copies are never compared, so nothing this command (or a
///   reader) leaves is taken for a write.
/// - Each part (the dumps store, the soup, the rest) is judged against the
///   digests the undo recorded where it began and the snapshot's: a file as
///   it began is the merge's; one as the snapshot holds it, or missing where
///   the undo moved it aside, is the undo's and is returned from the
///   `-undone-` directory. A file that is neither was written by something
///   else after the undo put that store back: refused, listing them, unless
///   `--set-aside-later-writes` (they are moved into the abandon directory,
///   not deleted).
/// - All parts are returned, or none. When a copy is missing (the snapshot
///   and the `-undone-` directory both gone), nothing is returned: the hold
///   is lifted, the dumps store's id serial is raised past every dump a
///   record cites, and the answer says the cauldron is TORN and what is
///   where.
/// - Before the holds are lifted the marker says the files are returned, so
///   a run cut short after that only lifts them; one cut short before is
///   finished by running it again (a returned file is as it began).
pub fn abandon_undo(c: &env::Cauldron, o: AbandonUndo) -> Result<Value> {
    // The files first, before anything here opens a database.
    let now_all = crate::snapshot::files_now(c)?;
    let (marker, from, damaged) = read_marker(c);
    let holds = undo_holds(c)?;
    if marker.is_none() && holds.is_empty() && damaged.is_none() {
        return Ok(json!({"outcome": "none", "note": "no undo of a merge into this cauldron was cut short; nothing to abandon"}));
    }
    let merge = marker
        .as_ref()
        .and_then(|m| m["merge"].as_str().map(str::to_string))
        .or_else(|| holds.iter().find_map(|(_, r)| merge_of_reason(r)))
        .unwrap_or_else(|| "MG-unknown".into());
    if let Some(m) = &marker {
        // Finishable only from the cauldron's own marker: writers and recover
        // read that one, not the snapshot's copy.
        if m["state"] != "returned" && from == undoing_path(c).display().to_string() && finishable(c, m) && !o.lose_undo {
            refuse!(
                Code::NotAtRest,
                "REFUSED: the undo of merge {merge} into cauldron `{}` can still be finished (its marker is readable and its snapshot \
                 is whole), so it was not abandoned. Nothing was changed. Run `{}` to finish it: the cauldron is then as it was \
                 before the merge. To keep the merge instead and LOSE the undo, run `{} --lose-undo`.",
                c.name,
                recover_cmd(c, ""),
                recover_cmd(c, "--abandon-undo")
            );
        }
    }
    let _dumps = dumps::lock_for_merge(c, &format!("cli {} merge-abandon-undo", std::process::id()))?;
    let at = chrono::Utc::now();
    let base = env::private::data_dir(c);
    let name = base.file_name().and_then(|n| n.to_str()).unwrap_or("cauldron").to_string();
    let out_dir = base.parent().unwrap_or(Path::new(".")).join(format!("{name}.{merge}-undo-abandoned-{}", at.format("%Y%m%dT%H%M%S%3fZ")));
    let soup_name = crate::snapshot::scope(c).parts[0].clone();
    let reason = marker.as_ref().and_then(|m| m["hold"].as_str().map(str::to_string));
    let start = marker.as_ref().map(|m| crate::snapshot::files_from_json(&m["start"])).unwrap_or_default();
    let already = marker.as_ref().is_some_and(|m| m["state"] == "returned");
    let mut parts: BTreeMap<&'static str, Value> = BTreeMap::new();
    let mut returned_total = 0usize;
    let mut torn: Option<String> = None;
    if already {
        for p in ["dumps", "soup", "rest"] {
            parts.insert(p, json!({"outcome": "returned: as the merge left it (by the run cut short before this one)"}));
        }
    } else if !start.is_empty() {
        let aside = PathBuf::from(marker.as_ref().and_then(|m| m["aside"].as_str()).unwrap_or(""));
        let want: Option<BTreeMap<String, (String, u64)>> = marker
            .as_ref()
            .filter(|m| m["want"].is_object())
            .map(|m| crate::snapshot::files_from_json(&m["want"]))
            .or_else(|| {
                let snap = PathBuf::from(marker.as_ref()?["snapshot"].as_str()?);
                let man: Value = serde_json::from_str(&std::fs::read_to_string(snap.join(crate::snapshot::MANIFEST)).ok()?).ok()?;
                Some(
                    man["files"]
                        .as_array()?
                        .iter()
                        .filter_map(|f| Some((f["path"].as_str()?.to_string(), (f["sha256"].as_str()?.to_string(), f["bytes"].as_u64()?))))
                        .collect(),
                )
            });
        let empty = BTreeMap::new();
        let w = want.as_ref().unwrap_or(&empty);
        let held = |part: &str| -> bool {
            let r = reason.clone().unwrap_or_default();
            match part {
                "soup" => halted_with(&c.soup(), &r).unwrap_or(false),
                "dumps" => halted_with(&dumps::dir(c), &r).unwrap_or(false),
                _ => halted_with(&c.soup(), &r).unwrap_or(false) && halted_with(&dumps::dir(c), &r).unwrap_or(false),
            }
        };
        let spine = spine::spine_db::SPINE_FILE;
        // (part, rel, copy to return, a file is there now)
        let mut todo: Vec<(&'static str, String, Option<PathBuf>, bool)> = Vec::new();
        let (mut later, mut missing) = (Vec::new(), Vec::new());
        for part in ["dumps", "soup", "rest"] {
            let is_held = held(part);
            let mut paths: Vec<&String> = start.keys().chain(now_all.keys()).chain(w.keys()).filter(|p| part_of(p, &soup_name) == part).collect::<BTreeSet<_>>().into_iter().collect();
            paths.sort_by_key(|p| (p.contains(&format!("/{spine}")), (*p).clone()));
            let mut n_part = 0;
            for rel in paths {
                let (s, n, wv) = (start.get(rel), now_all.get(rel), w.get(rel));
                let recorded = s.is_some() || wv.is_some();
                if n.is_some_and(|x| not_compared(rel, x.1, recorded)) || (n.is_none() && !recorded) {
                    continue;
                }
                if s == n {
                    continue;
                }
                let undos = (n.is_none() && s.is_some()) || (n.is_some() && n == wv) || (is_held && n.is_none());
                if !undos {
                    // Written by something else after the undo put this
                    // store back: set aside (only on request), and the
                    // merged copy returned in its place.
                    later.push(rel.clone());
                }
                match s {
                    Some(d) => match aside_copy(&aside, rel, d) {
                        Some(p) => todo.push((part, rel.clone(), Some(p), n.is_some())),
                        None => missing.push(rel.clone()),
                    },
                    None => todo.push((part, rel.clone(), None, n.is_some())),
                }
                n_part += 1;
            }
            parts.insert(part, json!({"to_return": n_part}));
        }
        if !missing.is_empty() {
            if from == undoing_path(c).display().to_string() && marker.as_ref().is_some_and(|m| finishable(c, m)) {
                refuse!(
                    Code::NotAtRest,
                    "REFUSED: the merge cannot be kept: the undo of merge {merge} already put back {} file(s) whose merged copies are \
                     no longer in {} . Nothing was changed. Its snapshot is whole, so finish the undo instead: `{}`.",
                    missing.len(),
                    aside.display(),
                    recover_cmd(c, "")
                );
            }
            torn = Some(format!(
                "neither side can be had whole: the undo had already put back {} file(s) whose merged copies are gone from {}, and its \
                 snapshot (the other side) is gone or damaged too",
                missing.len(),
                aside.display()
            ));
            for p in ["dumps", "soup", "rest"] {
                parts.insert(p, json!({"outcome": "left as it is", "why": "the cauldron cannot be made whole either way"}));
            }
        } else {
            if !later.is_empty() && !o.set_aside_later_writes {
                return Err(Error::new(
                    Code::ChangedSince,
                    format!(
                        "REFUSED: something wrote to cauldron `{}` after the undo of merge {merge} put part of it back ({} file(s): {}). \
                         Returning the cauldron to the merged state would set those writes aside, so it was not abandoned. Nothing was \
                         changed. To do it anyway (the files are moved into a directory beside the cauldron, not deleted), run `{} \
                         --set-aside-later-writes`; or put the snapshot back exactly as it was and run `{}` to finish the undo.",
                        c.name,
                        later.len(),
                        later.iter().take(10).cloned().collect::<Vec<_>>().join(", "),
                        recover_cmd(c, "--abandon-undo"),
                        recover_cmd(c, "")
                    ),
                )
                .with_detail(json!({"merge": merge, "written_since": later})));
            }
            env::private::make_dir(&out_dir).map_err(|e| store(format!("making {}: {e}", out_dir.display())))?;
            let mut by_part: BTreeMap<&str, usize> = BTreeMap::new();
            for (part, rel, copy, there) in &todo {
                let target = base.join(rel);
                if *there && target.exists() {
                    let into = if later.contains(rel) { out_dir.join("written-since") } else { out_dir.join("replaced") };
                    move_into(&target, &into, rel)?;
                }
                if let Some(p) = copy {
                    let mut at = base.clone();
                    for comp in Path::new(rel).parent().into_iter().flat_map(|p| p.components()) {
                        at = at.join(comp);
                        env::private::make_dir(&at).map_err(|e| store(format!("making {}: {e}", at.display())))?;
                    }
                    std::fs::rename(p, &target).map_err(|e| store(format!("returning {} to {}: {e}", p.display(), target.display())))?;
                }
                *by_part.entry(part).or_default() += 1;
            }
            for p in ["dumps", "soup", "rest"] {
                let n = by_part.get(p).copied().unwrap_or(0);
                returned_total += n;
                parts.insert(p, json!({"outcome": if n == 0 { "as the merge left it (the undo had not reached it)" } else { "returned: as the merge left it" }, "returned": n}));
            }
            // From here a run cut short only lifts the holds.
            // Written where the marker was read from, when that is a file
            // (the cauldron's marker, or the snapshot's copy).
            if let Some(mut m) = marker.clone() {
                m["state"] = json!("returned");
                let at = PathBuf::from(&from);
                if at.is_file() {
                    super::write_atomic(&at, &m)?;
                }
            }
        }
    } else if marker.as_ref().is_some_and(|m| m["state"] != "verified" && m["state"] != "returned") {
        // A marker whose state is not one an undo writes, and no record of
        // where it began: what it moved cannot be told from it.
        torn = Some("the undo's marker does not say where it began, so what it had put back cannot be told".into());
    }
    if marker.is_none() {
        torn = torn.or_else(|| {
            Some(format!(
                "the undo's marker is gone{}, so how far the undo got cannot be told",
                damaged.as_ref().map(|d| format!(" or unreadable ({d})")).unwrap_or_default()
            ))
        });
    }
    env::private::make_dir(&out_dir).map_err(|e| store(format!("making {}: {e}", out_dir.display())))?;
    // Lift every hold of an undo, the marker's or any other left behind.
    let mut lifted = Vec::new();
    let mut reasons: BTreeSet<String> = holds.iter().map(|(_, r)| r.clone()).collect();
    reasons.extend(reason.clone());
    for r in &reasons {
        for d in [c.soup(), dumps::dir(c)] {
            if halted_with(&d, r)? {
                release_store(&d, r)?;
                lifted.push(d.display().to_string());
            }
        }
    }
    // Whatever came of it, no dump id a record cites is minted again.
    let ids = keep_dump_ids_ahead(c)?;
    if undoing_path(c).exists() {
        move_into(&undoing_path(c), &out_dir, UNDOING)?;
    }
    if let Some(snap) = marker.as_ref().and_then(|m| m["snapshot"].as_str()).map(PathBuf::from) {
        if snap.join(UNDOING).is_file() {
            move_into(&snap.join(UNDOING), &out_dir, "snapshot-copy-of-merge-undoing.json")?;
        }
    }
    let end = end_state(c, &merge);
    let snap_ok = marker.as_ref().and_then(|m| m["snapshot"].as_str()).is_some_and(|s| crate::snapshot::verify(Path::new(s)).is_ok());
    let again = match end["state"].as_str() {
        Some("merged") if snap_ok => format!("The merge can still be undone: `cauldron merge --undo {merge} --into {} --env {}`.", c.name, c.env),
        Some("merged") => format!(
            "The merge can be undone again only once its snapshot is back exactly as it was ({}).",
            marker.as_ref().and_then(|m| m["snapshot"].as_str()).unwrap_or("?")
        ),
        _ => String::new(),
    };
    let kept = match (end["state"].as_str(), &torn) {
        (Some("merged"), None) => format!(
            "cauldron `{}` is exactly as merge {merge} left it: soup, dumps and the merge's record (the undo had put back nothing, or everything it put back was returned{})",
            c.name,
            if o.set_aside_later_writes && returned_total > 0 { "; writes made since were set aside" } else { "" }
        ),
        (Some(s), _) => format!(
            "cauldron `{}` is {s}: {}{}",
            c.name,
            [torn.clone(), end["note"].as_str().map(str::to_string)].into_iter().flatten().collect::<Vec<_>>().join("; "),
            end["repair"].as_str().map(|r| format!(". What you can do: {r}")).unwrap_or_default()
        ),
        (None, _) => format!("cauldron `{}`'s state could not be read", c.name),
    };
    let outcome = if end["state"] == "merged" && torn.is_none() { "undo-abandoned" } else { "undo-abandoned-torn" };
    let doc = json!({
        "merge": merge,
        "outcome": outcome,
        "end_state": end,
        "marker_read_from": from,
        "parts": parts,
        "returned": returned_total,
        "holds_lifted": lifted,
        "dump_ids": ids,
        "not_restored": torn.iter().cloned().collect::<Vec<_>>(),
        "set_aside_in": out_dir.display().to_string(),
        "note": format!(
            "The undo of merge {merge} was abandoned: its hold on cauldron `{}` is lifted and writes go on. {kept}. {}{}What was \
             replaced, and the undo's marker, are in {} (nothing was deleted).",
            c.name,
            if ids["raised"] == json!(true) { format!("The dumps store's next id was raised past DM-{}, which a record cites, so no dump takes a cited id. ", ids["highest_cited"]) } else { String::new() },
            if again.is_empty() { String::new() } else { format!("{again} ") },
            out_dir.display()
        ),
    });
    crate::snapshot::write_json(&out_dir.join("undo-abandoned.json"), &doc)?;
    Ok(doc)
}

/// **Journals of merges that cannot be read**, and holds of merges no
/// journal still landing names: what `cauldron soup recover
/// --abandon-merge` settles when [`super::abandon`] has no pending merge to
/// abandon. A journal that will not parse is set aside
/// (`journal.json.damaged-<time>`), and every merge's hold on either store
/// is lifted. Answers what it did (`None`: nothing of the sort).
pub fn abandon_orphans(c: &env::Cauldron) -> Result<Option<Value>> {
    let mut damaged = Vec::new();
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ").to_string();
    for e in std::fs::read_dir(dir(c)).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if merge_serial(&name).is_none() {
            continue;
        }
        let j = e.path().join(JOURNAL);
        let Ok(text) = std::fs::read_to_string(&j) else { continue };
        if serde_json::from_str::<Value>(&text).is_err() {
            let to = e.path().join(format!("{JOURNAL}.damaged-{stamp}"));
            std::fs::rename(&j, &to).map_err(|err| store(format!("setting {} aside: {err}", j.display())))?;
            damaged.push(json!({"merge": name, "journal": j.display().to_string(), "moved_to": to.display().to_string()}));
        }
    }
    if pending(c)?.is_some() {
        // A merge still landing is abandoned by `super::abandon`.
        return Ok((!damaged.is_empty()).then(|| json!({"damaged_journals": damaged})));
    }
    let _dumps = dumps::lock_for_merge(c, &format!("cli {} merge-abandon", std::process::id()))?;
    let mut lifted = Vec::new();
    for (d, r) in merge_holds(c)? {
        release_store(&d, &r)?;
        lifted.push(json!({"store": d.display().to_string(), "merge": merge_of_reason(&r)}));
    }
    if damaged.is_empty() && lifted.is_empty() {
        return Ok(None);
    }
    Ok(Some(json!({
        "outcome": "abandoned-on-request",
        "damaged_journals": damaged,
        "holds_lifted": lifted,
        "note": format!(
            "{}{}Whatever of the merge landed stays; nothing was put back. Writes to cauldron `{}` go on.",
            if damaged.is_empty() { String::new() } else { format!("{} merge journal(s) could not be read and were set aside (listed). ", damaged.len()) },
            if lifted.is_empty() { String::new() } else { format!("The hold of a merge no journal still names was lifted on {} store(s). ", lifted.len()) },
            c.name
        ),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hold_reason_names_its_merge() {
        assert_eq!(merge_of_reason("cauldron merge --undo MG-3: cauldron `b` is being put back").as_deref(), Some("MG-3"));
        assert_eq!(merge_of_reason("merge MG-12 into cauldron `b` (id x) is landing or was cut short").as_deref(), Some("MG-12"));
        assert_eq!(merge_of_reason("an import"), None);
        assert_eq!(part_of("dumps/blobs/a", "soup"), "dumps");
        assert_eq!(part_of("soup/ws.db", "soup"), "soup");
        assert_eq!(part_of("merges/MG-1/merge.json", "soup"), "rest");
    }
}

/// Was the merge (or the undo) these flags would give up the owner's? A
/// merge run at a terminal, or one that carried decisions, or an undo run
/// at a terminal; and, conservatively, one whose record and marker say
/// nothing any more.
fn owners(c: &env::Cauldron, undo: bool) -> bool {
    let read = |p: PathBuf| -> Option<Value> { std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str(&t).ok()) };
    let said = |v: &Value| v["by"] == "tty" || v["allowed"].as_str().is_some_and(|a| !a.starts_with("no owner act"));
    if undo {
        let (m, _, _) = read_marker(c);
        let merge = m.as_ref().and_then(|m| m["merge"].as_str().map(str::to_string));
        let record = merge.as_ref().and_then(|id| read(dir(c).join(id).join(super::RECORD)));
        let held = !undo_holds(c).unwrap_or_default().is_empty();
        match (&m, &record) {
            // Nothing to give up: not an act at all.
            (None, _) if !held && undo_pending(c).is_ok() => false,
            (None, None) => true,
            _ => m.as_ref().is_some_and(|m| m["by"] == "tty") || record.as_ref().is_some_and(said),
        }
    } else {
        match pending(c) {
            Ok(Some((_, j))) => said(&j),
            Ok(None) => !merge_holds(c).unwrap_or_default().is_empty(),
            Err(_) => true,
        }
    }
}

/// **`--abandon-undo` and `--abandon-merge` are owner acts, as the merge and
/// the undo are** (`env::owner_acts`, act `merge`): giving up the owner's
/// merge or undo needs an interactive terminal, or `merge` opened (the
/// persisted setting, or `CAULDRON_OWNER_ACTS=merge` for the command).
/// An agent's merge that carried no decision, and its undo, are no owner
/// act.
pub fn abandon_gate(t: &crate::target::Target, flag: &str, undo: bool, at_terminal: bool, var: Option<&str>) -> Result<()> {
    let c = &t.cauldron;
    if at_terminal || !owners(c, undo) {
        return Ok(());
    }
    use env::owner_acts::{self, Act};
    let setting = owner_acts::load(&t.env.root).unwrap_or_default();
    match owner_acts::decide(Act::Merge, &setting, &t.env.name, &c.name, c.id().as_deref(), false, var) {
        Ok(_) => Ok(()),
        Err(r) => Err(Error::new(
            Code::NotATerminal,
            format!(
                "REFUSED: `{flag}` gives up the owner's {} into cauldron `{}`, and that is the same owner act `merge` as the merge and \
                 the undo: at an interactive terminal, or without one only where the owner has opened `merge`. Nothing was changed. \
                 Ask the user to run, in a terminal on this machine:\n    {}\n{}{}",
                if undo { "undo of a merge" } else { "merge" },
                c.name,
                recover_cmd(c, flag),
                r.how_to_allow,
                r.bad_var.map(|b| format!(" {b}; it opens nothing.")).unwrap_or_default()
            ),
        )),
    }
}

/// **Could `cauldron soup recover` still settle the merge cut short?** Its
/// journal readable, what it staged there (when it carries dumps), and its
/// snapshot's manifest: recover then finishes it, or puts the target back.
pub fn merge_settleable(c: &env::Cauldron) -> bool {
    let Ok(Some((id, j))) = pending(c) else { return false };
    let staged_ok = j["dumps"].as_array().is_none_or(|d| d.is_empty()) || dir(c).join(&id).join(super::STAGED).is_dir();
    let snap = PathBuf::from(j["snapshot"]["dir"].as_str().unwrap_or(""));
    staged_ok && snap.is_absolute() && snap.join(crate::snapshot::MANIFEST).is_file()
}

/// **`cauldron soup recover --settle-merge <MG-n>`**: complete a cauldron
/// left TORN by a merge or an undo cut short with files taken away by hand
/// (`merge --status` says `torn`), to the one side its stores already hold
/// whole. When neither the soup nor the dumps store holds the merge any
/// more but its record is still there, the record (`merges/<MG-n>/`) is
/// moved beside the cauldron (`<name>.<MG-n>-settled-<time>/`, not
/// deleted): the cauldron is then exactly as before the merge, and says so.
/// When the stores themselves disagree (the soup holds the merge's records,
/// the dumps store not its dumps), no side can be had from what is left:
/// refused, saying what is where (the dumps store's next id is already past
/// every cited dump). The caller holds the soup's writer lock and has
/// checked the owner act.
pub fn settle(c: &env::Cauldron, merge: &str) -> Result<Value> {
    if merge_serial(merge).is_none() {
        refuse!(Code::Usage, "REFUSED: `{merge}` is not a merge id (MG-<n>). Nothing was changed.");
    }
    if undo_pending(c).ok().flatten().is_some() || pending(c).ok().flatten().is_some() {
        refuse!(
            Code::NotAtRest,
            "REFUSED: a merge or an undo into cauldron `{}` is still cut short; settle that first (`{}`). Nothing was changed.",
            c.name,
            recover_cmd(c, "")
        );
    }
    let end = end_state(c, merge);
    match end["state"].as_str() {
        Some("torn") => {}
        Some(s) => {
            return Ok(json!({"merge": merge, "outcome": "nothing-to-settle", "end_state": end, "note": format!("merge {merge} is `{s}` in cauldron `{}`: nothing to settle", c.name)}))
        }
        None => refuse!(Code::Store, "REFUSED: where merge {merge} stands could not be read. Nothing was changed."),
    }
    let h = holding(c, merge);
    let mdir = dir(c).join(merge);
    if h.nothing_here() && mdir.join(super::RECORD).is_file() {
        let _dumps = dumps::lock_for_merge(c, &format!("cli {} merge-settle", std::process::id()))?;
        let base = env::private::data_dir(c);
        let name = base.file_name().and_then(|n| n.to_str()).unwrap_or("cauldron").to_string();
        let to = base.parent().unwrap_or(Path::new(".")).join(format!("{name}.{merge}-settled-{}", chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ")));
        std::fs::rename(&mdir, &to).map_err(|e| store(format!("moving {} to {}: {e}", mdir.display(), to.display())))?;
        let ids = keep_dump_ids_ahead(c)?;
        return Ok(json!({
            "merge": merge,
            "outcome": "settled-before-merge",
            "end_state": end_state(c, merge),
            "moved": to.display().to_string(),
            "dump_ids": ids,
            "note": format!(
                "neither store of cauldron `{}` holds merge {merge} any more, so its record was the only part of it left: moved to {} \
                 (nothing deleted). The cauldron is as it was before the merge, and the merge is no longer listed; merging the \
                 source again brings it back in full.",
                c.name,
                to.display()
            ),
        }));
    }
    let _dumps = dumps::lock_for_merge(c, &format!("cli {} merge-settle", std::process::id()))?;
    let ids = keep_dump_ids_ahead(c)?;
    // Part of the merge is here, its record too, and no record cites a dump
    // that is not here: the merge is settled AS IT STANDS. What is gone is
    // moved, in its record, from what it carried to what was lost (the
    // record as it was is kept beside it), so the record describes exactly
    // what the cauldron holds of the merge.
    let dangling = dangling_cites(c);
    if h.record && !h.nothing_here() && dangling.iter().all(|d| !d.editable) {
        let at = chrono::Utc::now();
        let path = mdir.join(super::RECORD);
        let text = std::fs::read_to_string(&path).map_err(|e| store(format!("reading {}: {e}", path.display())))?;
        let mut r: Value = serde_json::from_str(&text).map_err(|e| store(format!("{} will not parse: {e}", path.display())))?;
        let here = super::landed_dumps(c, merge)?;
        let (mut kept_d, mut lost_d) = (Map::new(), Map::new());
        for (src, tgt) in r["dumps"].as_object().cloned().unwrap_or_default() {
            if here.contains_key(&src) { kept_d.insert(src, tgt) } else { lost_d.insert(src, tgt) };
        }
        let (mut kept_s, mut lost_s) = (Map::new(), Map::new());
        for (part, v) in r["soup_writes"].as_object().cloned().unwrap_or_default() {
            if h.soup_here.contains(&part) { kept_s.insert(part, v) } else { lost_s.insert(part, v) };
        }
        let before = mdir.join(format!("{}.before-settle-{}", super::RECORD, at.format("%Y%m%dT%H%M%S%3fZ")));
        super::write_atomic(&before, &serde_json::from_str::<Value>(&text).unwrap_or(Value::Null))?;
        r["dumps"] = Value::Object(kept_d);
        r["soup_writes"] = Value::Object(kept_s);
        let left: Vec<Value> = dangling.iter().map(|d| json!({"record": d.record, "cites": d.cites, "why": "a ripple's record: its dump cite is its review trail and is never changed"})).collect();
        let mut lost = json!({"dumps": lost_d, "soup_writes": lost_s, "cites_left_missing": left});
        if let Some(prev) = r["settled"]["lost"].as_object() {
            for k in ["dumps", "soup_writes"] {
                for (a, b) in prev.get(k).and_then(Value::as_object).cloned().unwrap_or_default() {
                    lost[k][a] = b;
                }
            }
        }
        r["settled"] = json!({
            "at": at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "how": "cauldron soup recover --settle-merge: part of the merge was taken away by hand; the record now names only what the cauldron holds",
            "lost": lost,
            "record_before": before.display().to_string(),
        });
        super::write_atomic(&path, &r)?;
        let end = end_state(c, merge);
        return Ok(json!({
            "merge": merge,
            "outcome": "settled-as-it-stands",
            "end_state": end,
            "lost": r["settled"]["lost"],
            "record_before": before.display().to_string(),
            "dump_ids": ids,
            "note": format!(
                "merge {merge} in cauldron `{}` is settled as it stands: {} of its dump(s) and {} of its soup write(s) were not here \
                 (taken away by hand). {}Its record now names only what the cauldron holds, and what was lost (the record as it was: \
                 {}).",
                c.name,
                r["settled"]["lost"]["dumps"].as_object().map(|o| o.len()).unwrap_or(0),
                r["settled"]["lost"]["soup_writes"].as_object().map(|o| o.len()).unwrap_or(0),
                if left.is_empty() {
                    "No record cites what is gone. ".to_string()
                } else {
                    format!(
                        "{} ripple record(s) still cite a dump that is gone (their review trail, never changed: {}); no dump will take those ids. ",
                        left.len(),
                        dangling.iter().map(|d| format!("{} cites {}", d.record, d.cites)).collect::<Vec<_>>().join(", ")
                    )
                },
                before.display()
            ),
        }));
    }
    let end = end_state(c, merge);
    Err(Error::new(
        Code::NotAtRest,
        format!(
            "REFUSED: merge {merge} in cauldron `{}` cannot be settled yet: {}. Neither side of the merge can be made whole from \
             what the stores hold, and a record that can be repaired still cites a dump that is not there. Nothing was changed; no \
             dump will take an id a record cites{}.{}",
            c.name,
            end["note"].as_str().unwrap_or(""),
            if ids["highest_cited"].as_u64().unwrap_or(0) > 0 { format!(" (the dumps store's next id is past DM-{})", ids["highest_cited"]) } else { String::new() },
            end["repair"].as_str().map(|r| format!(" What you can do: {r}")).unwrap_or_default()
        ),
    )
    .with_detail(json!({"merge": merge, "end_state": end, "dump_ids": ids})))
}
