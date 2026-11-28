//! **Recover**: settle a soup an interrupted import left not at rest.
//!
//! An import killed part way (Ctrl-C, a crash, a machine that stopped)
//! leaves its transaction row un-cleared and its locks held; the spine's own
//! recovery pass, which `spine::Cauldron::open` runs, settles that from the
//! row alone: a row that was never decided is ABORTED (nothing of it
//! landed, nothing lands), a decided row is ROLLED FORWARD (all of it lands,
//! exactly once), and orphaned locks are released.
//!
//! It runs that open only when it can prove no other process holds the
//! store: no live soup daemon in the environment, and the soup's writer
//! lock taken exclusive. A soup already at rest is not opened at all.
//!
//! With that writer lock held, it also sets aside a region-lock table that
//! cannot be read, once no ripple run of the cauldron is live
//! (`crate::region_lock::set_aside_damaged`), and says so
//! (`doc["region_locks"]`, present only when it did).

use std::path::Path;

use serde_json::{json, Value};

use crate::lock::{refuse_live_soup, take_writer_lock};
use crate::reads::{prefix, serial};
use crate::target::Target;
use crate::{Code, Error, Result};

/// What a recover came to: its JSON, and whether the soup is now at rest.
#[derive(Debug, Clone)]
pub struct RecoverReport {
    pub doc: Value,
    pub settled: bool,
}

fn store(err: impl std::fmt::Display) -> Error {
    Error::new(Code::Store, err.to_string())
}

/// Recover the target's soup. `doc["interrupted_import"]` is `landed` (with
/// how many records and their ids), `rolled-back`, `landed-and-rolled-back`
/// or `none`; `doc["instruction"]` says in words what to do next.
pub fn recover(t: &Target) -> Result<RecoverReport> {
    recover_with(t, false)
}

/// What `cauldron soup recover` was asked to give up rather than finish.
#[derive(Debug, Clone, Default)]
pub struct Abandon {
    /// `--abandon-merge`: a merge cut short (`crate::merge::abandon`), a
    /// merge journal that cannot be read, a merge's hold no journal names.
    pub merge: bool,
    /// `--keep-partial-merge`: abandon a merge recover could still settle,
    /// keeping what of it landed.
    pub keep_partial_merge: bool,
    /// `--abandon-undo`: an undo cut short, keeping the merge
    /// (`crate::merge::abandon_undo`), with what the owner chose.
    pub undo: bool,
    pub undo_opts: crate::merge::AbandonUndo,
    /// Who asks: both abandons are the owner act `merge`
    /// (`crate::merge::abandon_gate`).
    pub at_terminal: bool,
    pub var: Option<String>,
    /// `--settle-merge <MG-n>`: complete a cauldron a merge left torn
    /// (`crate::merge::settle`).
    pub settle: Option<String>,
}

/// [`recover`]; with `abandon_merge`, a merge cut short is NOT finished: its
/// hold on both stores is lifted and what of it landed stays, listed
/// (`crate::merge::abandon`). The way out for a merge recover can neither
/// finish nor put back.
pub fn recover_with(t: &Target, abandon_merge: bool) -> Result<RecoverReport> {
    recover_abandoning(t, Abandon { merge: abandon_merge, at_terminal: true, ..Abandon::default() })
}

/// [`recover_with`], with either or both of the ways out ([`Abandon`]).
pub fn recover_abandoning(t: &Target, abandon: Abandon) -> Result<RecoverReport> {
    let abandon_merge = abandon.merge;
    let (e, c) = (&t.env, &t.cauldron);
    let soup = c.soup();
    refuse_live_soup(e)?;
    let _lock = take_writer_lock(c, "recover")?;
    let locks = crate::region_lock::set_aside_damaged(c, &_lock)?;
    let locks_said = locks.as_ref().map(|v| {
        format!(
            "The region-lock table could not be read ({}), and no ripple run of this cauldron was live, so it was set aside as {}; the soup now holds no region lock. ",
            v["why"].as_str().unwrap_or(""),
            v["moved_to"].as_str().unwrap_or("")
        )
    });
    // An undo of a merge cut short is finished first (`crate::merge`): it
    // held the stores, and until it is done the soup is not as either the
    // merge or the undo left it.
    // On request, an undo that cannot be finished is given up instead,
    // keeping the merge; so are merge journals that cannot be read and
    // holds nothing names.
    if let Some(m) = &abandon.settle {
        crate::merge::abandon_gate(t, &format!("--settle-merge {m}"), false, abandon.at_terminal, abandon.var.as_deref())?;
        let settled = crate::merge::settle(c, m)?;
        let doc = json!({
            "env": e.name,
            "cauldron": c.name,
            "outcome": settled["outcome"],
            "interrupted_import": "none",
            "merge": settled,
            "instruction": settled["note"],
        });
        return Ok(RecoverReport { doc, settled: true });
    }
    if abandon.undo {
        let flag = format!(
            "--abandon-undo{}{}",
            if abandon.undo_opts.lose_undo { " --lose-undo" } else { "" },
            if abandon.undo_opts.set_aside_later_writes { " --set-aside-later-writes" } else { "" }
        );
        crate::merge::abandon_gate(t, &flag, true, abandon.at_terminal, abandon.var.as_deref())?;
    }
    if abandon_merge {
        let flag = if abandon.keep_partial_merge { "--abandon-merge --keep-partial-merge" } else { "--abandon-merge" };
        crate::merge::abandon_gate(t, flag, false, abandon.at_terminal, abandon.var.as_deref())?;
        if crate::merge::merge_settleable(c) && !abandon.keep_partial_merge {
            return Err(Error::new(
                Code::NotAtRest,
                format!(
                    "REFUSED: the merge cut short in cauldron `{}` can still be settled by `cauldron soup recover --env {} --cauldron {}` \
                     (it finishes the merge, or puts the cauldron back as it was before it), so it was not abandoned. Nothing was \
                     changed. To abandon it anyway, keeping whatever part of it landed (possibly dumps without their records), add \
                     --keep-partial-merge.",
                    c.name, e.name, c.name
                ),
            ));
        }
    }
    let undo_abandoned = if abandon.undo { Some(crate::merge::abandon_undo(c, abandon.undo_opts)?) } else { None };
    let orphans = if abandon_merge { crate::merge::abandon_orphans(c)? } else { None };
    let undo = crate::merge::finish_undo(c, true, "`cauldron soup recover`")?;
    let before = spine::offline::rest_check(&soup).map_err(|err| store(format!("REFUSED: cannot read {}'s spine: {err}", c.name)))?;
    let mut doc = json!({
        "env": e.name,
        "cauldron": c.name,
        "before": before,
    });
    if let Some(u) = &undo_abandoned {
        doc["merge_undo"] = u.clone();
    }
    if let Some(o) = &orphans {
        doc["merge"] = o.clone();
    }
    if let Some(u) = &undo {
        doc["merge_undo"] = json!({"merge": u["merge"], "outcome": "finished", "kept": u["restored"]["kept"], "note": u["note"]});
    }
    // Only when it set a damaged table aside: a readable one leaves the
    // output as it always was.
    if let Some(v) = &locks {
        doc["region_locks"] = v.clone();
    }
    // A merge cut short before its journal landed nothing; the blobs it
    // staged from its source are swept (`crate::merge`).
    let swept = crate::merge::sweep_unstarted(c)?;
    if !swept.is_empty() {
        doc["merges_swept"] = json!(swept);
    }
    // Snapshots a merge took and never used: named, never removed.
    let unused = crate::merge::unused_snapshots(c);
    if !unused.is_empty() {
        doc["snapshots_unused"] = json!(unused);
        doc["snapshots_unused_note"] = json!("each is a full private copy of the cauldron as it was before a merge that never began landing; nothing uses it, so move it away");
    }
    let nodes_cmd = format!("cauldron soup nodes --env {} --cauldron {}", e.name, c.name);
    // A merge cut short after its journal is finished here too
    // (`crate::merge`), whether or not the soup's spine is at rest.
    let mut merge_pending = crate::merge::pending(c)?.is_some();
    let mut merge_abandoned = false;
    if merge_pending && abandon_merge {
        doc["merge"] = crate::merge::abandon(t)?;
        merge_pending = false;
        merge_abandoned = true;
    }
    // Its hold lifted, the soup is looked at again.
    let before = if merge_abandoned {
        let b = spine::offline::rest_check(&soup).map_err(|err| store(format!("REFUSED: cannot read {}'s spine: {err}", c.name)))?;
        doc["before"] = json!(b);
        b
    } else {
        before
    };
    // A hold of a merge or an undo that nothing left names: refused here,
    // naming the flag that lifts it (opening the spine would not).
    // Either store: the soup can be at rest while the dumps store is held.
    if !merge_pending {
        crate::merge::refuse_orphan_hold(c)?;
    }
    if before.is_empty() && !merge_pending && (undo_abandoned.as_ref().is_some_and(|u| u["outcome"] != "none") || orphans.is_some() || merge_abandoned) {
        doc["outcome"] = json!(match undo_abandoned.as_ref().and_then(|u| u["outcome"].as_str()).filter(|o| *o != "none") {
            Some(o) => o.to_string(),
            None => "merge-abandoned".to_string(),
        });
        doc["interrupted_import"] = json!("none");
        doc["instruction"] = json!(format!(
            "{}{}The soup is at rest.",
            undo_abandoned.as_ref().filter(|u| u["outcome"] != "none").map(|u| format!("{} ", u["note"].as_str().unwrap_or(""))).unwrap_or_default(),
            orphans.as_ref().or(if merge_abandoned { Some(&doc["merge"]) } else { None }).map(|o| format!("{} ", o["note"].as_str().unwrap_or(""))).unwrap_or_default()
        ));
        return Ok(RecoverReport { doc, settled: true });
    }
    if before.is_empty() && !merge_pending && undo.is_some() {
        doc["outcome"] = json!("undo-finished");
        doc["interrupted_import"] = json!("none");
        doc["instruction"] = json!(format!(
            "An undo of merge {} was cut short, and recover finished it: {} The soup is at rest.",
            undo.as_ref().map(|u| u["merge"].as_str().unwrap_or("?").to_string()).unwrap_or_default(),
            undo.as_ref().map(|u| u["note"].as_str().unwrap_or("").to_string()).unwrap_or_default()
        ));
        return Ok(RecoverReport { doc, settled: true });
    }
    if before.is_empty() && !merge_pending {
        doc["outcome"] = json!("at-rest");
        doc["interrupted_import"] = json!("none");
        doc["note"] = json!(if locks.is_some() {
            "the soup was already at rest; the spine was not opened and no record was written; the damaged region-lock table was set aside"
        } else {
            "the soup was already at rest; the spine was not opened and no data was written"
        });
        doc["instruction"] = json!(format!(
            "{}Nothing to recover: no interrupted import is waiting to be settled. This cannot tell you whether an \
             import you stopped earlier landed (one stopped after it finished landed in full, one stopped before \
             it began writing landed nothing, and an earlier recover printed its own outcome), so check with \
             `{nodes_cmd}` (newest first) before importing that batch again.",
            locks_said.clone().unwrap_or_default()
        ));
        return Ok(RecoverReport { doc, settled: true });
    }
    let mut cauldron = spine::Cauldron::open(&soup, spine::config::SpineConfig::default())
        .map_err(|err| store(format!("recovering cauldron `{}`'s soup: {err}", c.name)))?;
    let rec = cauldron.recovered_at_open().clone();
    let merged = if merge_pending {
        cauldron.set_region_guard(crate::region_lock::Guard::new(c));
        crate::merge::recover(t, &mut cauldron)?
    } else {
        None
    };
    let hexes = |v: &[spine::envelope::TxHash]| v.iter().map(|h| h.to_hex()).collect::<Vec<_>>();
    doc["rolled_forward"] = json!(hexes(&rec.rolled_forward));
    doc["cleared"] = json!(hexes(&rec.cleared_only));
    doc["aborted"] = json!(hexes(&rec.aborted));
    doc["live_tranches"] = json!(cauldron.tranches_at_open().tranches.len());
    // A row rolled forward, or found landed and only cleared, is an import
    // that LANDED; a row aborted (here, or by another recovery) landed
    // nothing.
    let landed_tx: Vec<String> = hexes(&rec.rolled_forward).into_iter().chain(hexes(&rec.cleared_only)).collect();
    let rolled_back = rec.aborted.len() + rec.aborted_elsewhere.len();
    drop(cauldron);
    let after = spine::offline::rest_check(&soup).map_err(|err| store(format!("cannot read {}'s spine after recovery: {err}", c.name)))?;
    let settled = after.is_empty();
    doc["after"] = json!(after);
    doc["outcome"] = json!(if settled { "recovered" } else { "not-at-rest" });

    let landed = landed_nodes(&soup, &landed_tx).map_err(|err| store(format!("cannot read which records landed in {}'s soup: {err}", c.name)))?;
    let mut steps = Vec::new();
    match (landed_tx.is_empty(), rolled_back) {
        (false, 0) => doc["interrupted_import"] = json!("landed"),
        (true, n) if n > 0 => doc["interrupted_import"] = json!("rolled-back"),
        (false, _) => doc["interrupted_import"] = json!("landed-and-rolled-back"),
        (true, _) => doc["interrupted_import"] = json!("none"),
    }
    if !landed_tx.is_empty() {
        doc["landed_records"] = json!(landed.len());
        doc["landed_ids"] = landed_summary(&landed);
        steps.push(format!(
            "The interrupted import LANDED: recover finished it, and its {} record(s) are now in the soup ({}). \
             Do NOT import that batch again: under --remint every record would land a second time under new \
             ids, and this release cannot delete nodes. An import run with --remint printed no id map; `{nodes_cmd}` lists the landed nodes \
             newest first.",
            landed.len(),
            id_ranges_text(&landed)
        ));
    }
    if rolled_back > 0 {
        steps.push(
            "The interrupted import was ROLLED BACK: none of its records landed. It is safe to import that batch \
             again."
                .to_string(),
        );
    }
    if landed_tx.is_empty() && rolled_back == 0 {
        steps.push("No interrupted import was found to finish or roll back.".to_string());
    }
    if !settled {
        steps.push(format!(
            "The soup is still NOT at rest ({}); do not import anything until a recover reports `recovered`.",
            doc["after"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join("; ")).unwrap_or_default()
        ));
    }
    if let Some(said) = locks_said {
        steps.insert(0, said.trim_end().to_string());
    }
    if let Some(m) = merged {
        let id = m["merge"].as_str().unwrap_or("?");
        steps.push(match m["outcome"].as_str() {
            Some("finished") => format!(
                "Merge {id} was cut short after it began landing, and recover FINISHED it: it is now in the cauldron in full \
                 (its record: {}). Do not run that merge again; it would be refused as already merged.",
                m["record"].as_str().unwrap_or("?")
            ),
            // Abandoned or put back: the merge's own note says what and why.
            _ => m["note"].as_str().unwrap_or("").to_string(),
        });
        doc["merge"] = m;
    }
    doc["instruction"] = json!(steps.join(" "));
    Ok(RecoverReport { doc, settled })
}

/// The human text of a recover result: the outcome and its instruction.
pub fn say(doc: &Value) -> String {
    format!(
        "{} (interrupted import: {})\n{}",
        doc["outcome"].as_str().unwrap_or("?"),
        doc["interrupted_import"].as_str().unwrap_or("?"),
        doc["instruction"].as_str().unwrap_or("")
    )
}

/// Every node, `(pool, id)`, that one of the transactions `tx` (hex hashes)
/// created, read through read-only connections to each pool file of the
/// soup at `soup`; sorted by pool, id prefix, then serial.
pub fn landed_nodes(soup: &Path, tx: &[String]) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    if tx.is_empty() {
        return Ok(out);
    }
    let listing = |err: std::io::Error| Error::caused(Code::Store, format!("listing {}", soup.display()), &err);
    let sql = |err: &dyn std::error::Error| Error::new(Code::Store, err.to_string());
    for entry in std::fs::read_dir(soup).map_err(listing)? {
        let p = entry.map_err(listing)?.path();
        let stem = p.file_stem().and_then(|n| n.to_str()).unwrap_or("").to_string();
        if p.extension().and_then(|x| x.to_str()) != Some("db") || stem == "spine" {
            continue;
        }
        let conn = spine::offline::read_only_connection(&p).map_err(store)?;
        let mut stmt = conn.prepare("SELECT id FROM nodes WHERE created_tx = ?1").map_err(|e| sql(&e))?;
        for h in tx {
            for id in stmt.query_map([h], |r| r.get::<_, String>(0)).map_err(|e| sql(&e))? {
                out.push((stem.clone(), id.map_err(|e| sql(&e))?));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| prefix(&a.1).cmp(prefix(&b.1))).then_with(|| serial(&a.1).cmp(&serial(&b.1))).then_with(|| a.1.cmp(&b.1)));
    Ok(out)
}

/// Per pool and id prefix: how many landed, the first and last id, and
/// whether the serials between them are all present (a contiguous range).
fn landed_groups(landed: &[(String, String)]) -> Vec<(String, String, usize, String, String, bool)> {
    let mut groups: Vec<(String, String, usize, String, String, bool)> = Vec::new();
    for (pool, id) in landed {
        match groups.last_mut() {
            Some(g) if g.0 == *pool && g.1 == prefix(id) => {
                g.5 &= serial(id) == serial(&g.4) + 1;
                g.2 += 1;
                g.4 = id.clone();
            }
            _ => groups.push((pool.clone(), prefix(id).to_string(), 1, id.clone(), id.clone(), true)),
        }
    }
    groups
}

/// The JSON form of the landed groups.
pub fn landed_summary(landed: &[(String, String)]) -> Value {
    Value::Array(
        landed_groups(landed)
            .into_iter()
            .map(|(pool, prefix, count, first, last, contiguous)| {
                json!({"pool": pool, "prefix": prefix, "count": count, "first": first, "last": last, "contiguous": contiguous})
            })
            .collect(),
    )
}

/// The text form of the landed groups: `IS-2..IS-5001 (5000) in ws`.
pub fn id_ranges_text(landed: &[(String, String)]) -> String {
    if landed.is_empty() {
        return "no node ids found for it".to_string();
    }
    landed_groups(landed)
        .into_iter()
        .map(|(pool, _, count, first, last, contiguous)| {
            let span = if count == 1 { first } else { format!("{first}..{last}") };
            let gaps = if contiguous { "" } else { ", not contiguous" };
            format!("{span} ({count}{gaps}) in {pool}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}
