//! **Merging one cauldron into another**: `cauldron merge <source> --into
//! <target>`, a COPY of the source's soup (both pools: `ws` and the
//! cluster pool) and of its dumps into the target, all or nothing, with a
//! plan printed first.
//!
//! # What a merge carries, and how
//!
//! - **Every record**, under a new id in the target (ids are always
//!   re-minted: the source's ids are labels of one batch, so an id the
//!   target already holds never collides). The map from the source's ids to
//!   the target's is printed and saved (`merges/<MG-n>/id-map.json`). Every
//!   edge between the source's records is rewritten through it, and so are
//!   the `[[ID]]` links in a body and the ids a region's or a cluster's
//!   boundary, or an edge's note, names (whole tokens only).
//! - **Nothing twice.** Each record (and dump) of the source has an
//!   ORIGIN: the cauldron it was first written in and its id there (the
//!   deepest `meta.merged_from` a merge gave it, or the source itself). One
//!   whose origin the target already holds (merged before, by this source or
//!   by another route, or the target's own record come back) is not carried
//!   again: the plan lists it under `already_here`, with the merge it came
//!   by, and what points at it points at the target's copy. So a source
//!   merged before carries only what it gained since, and a merge with
//!   nothing new is refused ([`Code::AlreadyMerged`]).
//! - **Regions and clusters are never folded together.** A source region
//!   whose handle (or boundary) reads the same as one the target has is
//!   carried as a region of its own and FLAGGED in the plan, with how to
//!   supersede one by the other afterwards; the same for clusters.
//! - **Dumps**, each as one write of the target's dumps store, in the state
//!   it had (`received`, `processed`, `failed`, or the `removed` tombstone,
//!   which carries no text), under a new `DM-<n>`; a record's `meta.cites`
//!   of a dump is rewritten to the dump's new id. Blobs are content
//!   addressed: bytes the target already holds are shared, others copied. A
//!   dump whose removal was cut short (`removing`) blocks the merge, naming
//!   the command that finishes it.
//! - **Review state, as far as the source's own history vouches for it.**
//!   Each record's `meta.merged_from` records the merge, the source cauldron
//!   (name and id), its id there, who wrote it there (`written_by`: a
//!   ripple's run, or an agent) and its review state there. A record a
//!   ripple wrote that nobody decided stays UNREVIEWED in the target
//!   (`crate::provenance`). A decision (`meta.approved` / `meta.rejected`)
//!   is carried ONLY when the source's own write history vouches for it:
//!   the last transaction that wrote that flag wrote exactly the value the
//!   record holds and was a decision there: the owner's command at a
//!   terminal (`by: "tty"`), or, where the owner opened `approve`
//!   (`env::owner_acts`), the owner's command without one or a session's
//!   `soup_approve` / `soup_reject` (`by: "agent"`), each accepted alike;
//!   or a merge of the source that carried a decision its own source
//!   vouched for. Who decided (`by`), how and when go into
//!   `meta.merged_from.decision`, with the merge that carried it and who ran
//!   that merge (`carried_by: {merge, by}`), and the flag is marked
//!   `carried_by_merge`. Any other flag (put there by editing the files, or
//!   changed since) is not carried: the record arrives without it, a
//!   ripple's unreviewed, with `meta.merged_from.decision_not_carried`
//!   saying why. A record that claims `meta.merged_from` though no merge of
//!   the source created it is carried as written there, without that claim.
//!   The plan counts the decisions that will and will not be carried before
//!   anything lands. A merge that carries a decision is the owner act
//!   `merge` (`env::owner_acts`, through [`merger`]): at a terminal, or
//!   without one where the owner opened it; the merge records who ran it
//!   (`by: "tty" | "agent"`) in its journal and record and on each carried
//!   decision.
//!
//!   The one exception to "`meta.ripple` is written only by a ripple's
//!   write": a ripple's record whose decision is carried lands in the
//!   merge's main write (agent class) with its `meta.ripple` as the source
//!   holds it, so it keeps saying which run wrote it. No other write path
//!   sets it (the door refuses it to every session and import).
//!
//! # What it never carries
//!
//! The source's MCP grant and its automatic-ripples setting (both bound to
//! the source's id; the target keeps its own), the source itself (left
//! untouched; removing it is `cauldron remove`), its write history (who and
//! when are kept per record in `written_by`), and the rest the plan lists
//! under `not_carried`, each with why.
//!
//! # All or nothing
//!
//! Under the target's writer locks (its soup's and its dumps store's) and
//! with the source's held so no writer touches it meanwhile, the merge:
//! judges the whole batch through the soup's own door (nothing written),
//! takes the complete snapshot of the target ([`crate::snapshot`], beside
//! it) before it opens anything of it, copies the blobs it will need into
//! `merges/<MG-n>/staged/`, and writes its JOURNAL
//! (`merges/<MG-n>/journal.json`). Then it HOLDS the target's two stores
//! (`spine::Cauldron::hold`): the spine's durable halted state, under a
//! reason that names the merge and `cauldron soup recover`, which every
//! release of the spine honours, so an older `cauldron` (CLI or service)
//! is refused too while the hold is on (it is lifted by the last soup
//! write, so after a crash past that write an older writer can write until
//! recover finishes the record; [`undo`] then leaves that write alone). Each dump
//! lands (one write each, through the hold), then the records: ONE write of
//! the soup (both pools) for all of them but the records a ripple wrote that
//! nobody decided, which land in a SECOND write whose initiator is of the
//! ripple class (`cauldron-merge:<MG-n>`), so that every reader since
//! ripples shipped, a `cauldron` older than merging included, reads them as
//! unreviewed (it keys on the class of the write that created a record);
//! and the edges from the first write's records to the second's in a third.
//! The last of them lifts the soup's hold in the same transaction as its
//! decided flip; then the dumps store's hold is let go and the merge record
//! and id map are written, every id READ BACK from what landed.
//!
//! A merge cut short is never left stuck. `cauldron soup recover` (and every
//! writer of this release refuses meanwhile, naming it): abandons one cut
//! short before it held the target (nothing of it landed), finishes one
//! that held it, exactly once, and, if it cannot finish it, puts the target
//! back from the snapshot (safe because the hold kept every other writer out
//! since the snapshot was taken), saying so and why.
//!
//! `cauldron merge --undo <MG-n>` puts the snapshot back exactly while the
//! target holds nothing but what it held when the merge began and the
//! merge's own writes, resumably ([`undo`]). A source over
//! [`CEILING_RECORDS`] is refused before anything is held unless the merge
//! is run with `--allow-large` ([`refuse_large`]).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use spine::envelope::Initiator;
use spine::migrate::records::{import_records, RecordsOptions};
use spine::migrate::{Landing, MigrationPlan};
use spine::pool::{EdgeRecord, NodeRecord, Pool};
use spine::schema_def::PoolName;

use crate::dumps::{self, Store};
use crate::provenance::{self, MergeTx};
use crate::reads::DEFAULT_POOL;
use crate::target::Target;
use crate::{refuse, Code, Error, Result};

/// The verb, as the writer lock's refusals name it.
pub const VERB: &str = "merge";

mod stuck;
pub use stuck::{abandon_gate, abandon_orphans, abandon_undo, end_state, merge_settleable, refuse_orphan_hold, settle, AbandonUndo};
/// Where a cauldron keeps its merges: `<cauldron>/merges/` (the legacy
/// cauldron's beside its soup, as its dumps are).
pub const DIR: &str = "merges";
/// A merge's journal, in its directory.
pub const JOURNAL: &str = "journal.json";
/// A finished merge's record.
pub const RECORD: &str = "merge.json";
/// A finished merge's id map.
pub const ID_MAP: &str = "id-map.json";
/// The record an undo leaves in the snapshot's directory.
pub const UNDO: &str = "undo.json";
/// The blobs a merge will need, staged before its journal.
pub const STAGED: &str = "staged";
/// The journal's format.
pub const FORMAT: u32 = 1;
/// The prefix of a merge's id: `MG-<n>`.
pub const PREFIX: &str = "MG";
/// The prefix of a source id used as a batch label: a label never looks
/// like an id the target holds.
const LABEL: &str = "from:";

fn store(err: impl std::fmt::Display) -> Error {
    Error::new(Code::Store, err.to_string())
}

/// `<cauldron>/merges/`.
pub fn dir(c: &env::Cauldron) -> PathBuf {
    let soup = c.soup();
    soup.parent().map(|p| p.join(DIR)).unwrap_or_else(|| soup.join(DIR))
}

fn label(id: &str) -> String {
    format!("{LABEL}{id}")
}

// ------------------------------------------------------------------ pending

/// The merge of `c` that was cut short after its journal (its id and its
/// journal), if there is one.
pub fn pending(c: &env::Cauldron) -> Result<Option<(String, Value)>> {
    let d = dir(c);
    let rd = match std::fs::read_dir(&d) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(store(format!("reading {}: {e}", d.display()))),
    };
    let mut found: Vec<(u64, String, Value)> = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(n) = merge_serial(&name) else { continue };
        let j = entry.path().join(JOURNAL);
        let text = match std::fs::read_to_string(&j) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(store(format!("reading {}: {e}", j.display()))),
        };
        let v: Value = serde_json::from_str(&text).map_err(|e| {
            Error::new(
                Code::NotAtRest,
                format!(
                    "REFUSED: the journal of merge {name} into cauldron `{}` ({}) will not parse ({e}), so whether that merge is \
                     still landing cannot be told, and nothing of this release writes to the cauldron. Nothing was written. Run \
                     `{}`: it sets the journal aside, lifts any hold of a merge on the cauldron, and keeps whatever of the merge \
                     landed.",
                    c.name,
                    j.display(),
                    stuck::recover_cmd(c, "--abandon-merge")
                ),
            )
        })?;
        // `done` and `abandoned` (cut short before it held the target and
        // landed nothing: `recover`) are over.
        if v["state"] == "landing" {
            found.push((n, name, v));
        }
    }
    found.sort_by_key(|x| x.0);
    Ok(found.into_iter().next().map(|(_, n, v)| (n, v)))
}

fn merge_serial(name: &str) -> Option<u64> {
    let n = name.strip_prefix(PREFIX)?.strip_prefix('-')?;
    (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).then(|| n.parse().ok()).flatten()
}

/// **A read of `c` says when a merge into it, or an undo of one, was cut
/// short** (`merge_pending` / `undo_pending` in the read's JSON): until
/// `cauldron soup recover` (or, for an undo, the same undo again) settles
/// it, a read may show part of the merge, or a cauldron partly put back.
pub fn note_pending(c: &env::Cauldron, doc: &mut Value) {
    if !doc.is_object() {
        return;
    }
    if let Ok(Some((id, _))) = pending(c) {
        doc["merge_pending"] = json!({
            "merge": id,
            "note": format!("merge {id} into this cauldron is landing now, or was cut short and is not settled: this read may show part of it. If no merge is running, `cauldron soup recover --env {} --cauldron {}` settles it", c.env, c.name),
        });
    }
    if let Ok(Some(u)) = undo_pending(c) {
        doc["undo_pending"] = json!({
            "merge": u["merge"],
            "note": format!("an undo of merge {} was cut short: this read may show the cauldron partly put back. `cauldron soup recover --env {} --cauldron {}` finishes it", u["merge"].as_str().unwrap_or("?"), c.env, c.name),
        });
    }
}

/// **Refused while a merge into `c` is pending**: one cut short after its
/// journal owns the cauldron until `cauldron soup recover` finishes it.
/// `what` names what was refused (`run \`cauldron soup import\``).
pub fn refuse_pending(c: &env::Cauldron, what: &str) -> Result<()> {
    refuse_pending_as(c, what, true)
}

/// [`refuse_pending`] for a writer that holds the dumps store's writer lock
/// (not the soup's).
pub fn refuse_pending_holding_dumps(c: &env::Cauldron, what: &str) -> Result<()> {
    refuse_pending_as(c, what, false)
}

/// An undo cut short is FINISHED first (the next writer of this release
/// completes it, as recover would: [`finish_undo`]); then a merge cut short
/// refuses.
fn refuse_pending_as(c: &env::Cauldron, what: &str, holding_soup: bool) -> Result<()> {
    finish_undo(c, holding_soup, &format!("the next writer of the cauldron (it was about to {what})"))?;
    if let Some((id, j)) = pending(c)? {
        // What recover will do: abandon a merge cut short before it held the
        // cauldron (nothing of it landed), finish one that held it.
        let held = halted_with(&c.soup(), j["hold"].as_str().unwrap_or("")).unwrap_or(false)
            || provenance::merge_txs(&c.soup()).map(|m| m.values().any(|x| x.merge == id)).unwrap_or(true);
        let will = if held {
            "it finishes the merge (it lands in full, exactly once), or, if it cannot, puts the cauldron back as it was before it"
        } else {
            "the merge was cut short before it held the cauldron, so nothing of it landed, and recover abandons it (run the merge again afterwards)"
        };
        refuse!(
            Code::NotAtRest,
            "REFUSED: merge {id} into cauldron `{}` was cut short after its journal, and until recover settles it nothing else \
             of this release writes to the cauldron, so this did not {what}. Nothing was written. Run `cauldron soup recover \
             --env {} --cauldron {}`: {will}.",
            c.name,
            c.env,
            c.name
        );
    }
    // A hold of a merge or an undo that nothing names any more (its marker
    // or journal gone): refused naming the flag that lifts it.
    stuck::refuse_orphan_hold(c)?;
    Ok(())
}

// --------------------------------------------------------------- the seam

/// **The merge's test seam**: `CAULDRON_MERGE_FAULT=<step>` aborts the
/// process right after that step, in a build with this crate's
/// `fault-injection` feature only. The steps, in order: `snapshot`,
/// `staged`, `journal`, `held` (the target's stores held), `dump` (after the
/// first dump of a pass lands), `dumps`, `soup` (the main write), `ripple`
/// (the ripple part), `record`. Without the feature, a set variable refuses
/// the merge before anything is changed.
pub mod fault {
    use crate::{refuse, Code, Result};

    pub const ENV: &str = "CAULDRON_MERGE_FAULT";
    pub const STEPS: [&str; 9] = ["snapshot", "staged", "journal", "held", "dump", "dumps", "soup", "ripple", "record"];

    pub fn check() -> Result<()> {
        if let Ok(v) = std::env::var(DELAY_ENV) {
            if !cfg!(feature = "fault-injection") {
                refuse!(Code::Usage, "REFUSED: {DELAY_ENV} is set (`{v}`), and this build has no fault seam. Nothing was changed.");
            }
        }
        let Ok(v) = std::env::var(ENV) else { return Ok(()) };
        if !cfg!(feature = "fault-injection") {
            refuse!(Code::Usage, "REFUSED: {ENV} is set (`{v}`), and this build has no fault seam. Nothing was changed.");
        }
        if !STEPS.contains(&v.as_str()) {
            refuse!(Code::Usage, "REFUSED: {ENV}=`{v}` names no step of a merge. Nothing was changed.");
        }
        Ok(())
    }

    /// The undo's steps: `marked` (its marker written), `held` (both stores
    /// held), `recorded` (every file's digest recorded), `restored` (the
    /// files put back, before the undo's record), and `file:<n>` (after the
    /// n-th file the restore moved), from `CAULDRON_UNDO_FAULT`.
    pub const UNDO_ENV: &str = "CAULDRON_UNDO_FAULT";

    /// Refuse an undo when [`UNDO_ENV`] is set in a build without the seam.
    pub fn check_undo() -> Result<()> {
        if let Ok(v) = std::env::var(UNDO_ENV) {
            if !cfg!(feature = "fault-injection") {
                refuse!(Code::Usage, "REFUSED: {UNDO_ENV} is set (`{v}`), and this build has no fault seam. Nothing was changed.");
            }
        }
        Ok(())
    }

    pub fn at_undo(step: &str) {
        #[cfg(feature = "fault-injection")]
        if std::env::var(UNDO_ENV).is_ok_and(|v| v == step) {
            use std::io::Write;
            let _ = writeln!(std::io::stderr(), "{UNDO_ENV}: aborting after `{step}`");
            std::process::abort();
        }
        #[cfg(not(feature = "fault-injection"))]
        let _ = step;
    }

    /// `CAULDRON_MERGE_DELAY_MS=<n>` (a build with the seam only; refused by
    /// [`check`] otherwise): the merge sleeps that long once it holds the
    /// target, so a test can write to the target while a merge runs.
    pub const DELAY_ENV: &str = "CAULDRON_MERGE_DELAY_MS";

    pub fn delay() {
        #[cfg(feature = "fault-injection")]
        if let Some(ms) = std::env::var(DELAY_ENV).ok().and_then(|v| v.parse::<u64>().ok()) {
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
    }

    pub fn at(step: &str) {
        #[cfg(feature = "fault-injection")]
        if std::env::var(ENV).is_ok_and(|v| v == step) {
            use std::io::Write;
            let _ = writeln!(std::io::stderr(), "{ENV}: aborting after `{step}`");
            std::process::abort();
        }
        #[cfg(not(feature = "fault-injection"))]
        let _ = step;
    }
}

// ------------------------------------------------------------- the size

/// **The default ceiling of a merge**: a source of more records than this
/// (both pools together) is refused before anything is read in full, held
/// or written, unless the merge is run with `--allow-large`. A merge is
/// one soup write whose cost grows faster than the batch (the soup's
/// statement compiler), and the target refuses every other writer for most
/// of it; at this size it takes about 14 seconds and 1.2 GB on the machine
/// it was measured on.
pub const CEILING_RECORDS: u64 = 20_000;

/// **The budgets a merge is judged against**: the soup's per-statement
/// fuel (`spine::config::SpineConfig::fuel_ceiling`) and the estimated time
/// a merge of [`CEILING_RECORDS`] ordinary records takes, beyond which it
/// holds the target too long to run unasked. In a build with the
/// `fault-injection` feature, `CAULDRON_MERGE_BUDGET=fuel=<n>,seconds=<n>`
/// lowers them, so a test can reach both with a small source.
fn budgets() -> (u64, u64) {
    let mut fuel = spine::config::SpineConfig::default().fuel_ceiling;
    let mut secs = estimate_parts(CEILING_RECORDS, CEILING_RECORDS, 0).0;
    #[cfg(feature = "fault-injection")]
    if let Ok(v) = std::env::var("CAULDRON_MERGE_BUDGET") {
        for kv in v.split(',') {
            match kv.split_once('=') {
                Some(("fuel", n)) => fuel = n.parse().unwrap_or(fuel),
                Some(("seconds", n)) => secs = n.parse().unwrap_or(secs),
                _ => {}
            }
        }
    }
    (fuel, secs)
}

/// **How large a merge of `src` would be**, counted from its files without
/// reading any record in full: its records (both pools), the ones a ripple
/// wrote, its edges (and those of the ripple part), its dumps and their
/// bytes; what the merge's two soup writes would spend of the soup's fuel;
/// and what it is estimated to cost in time and memory ([`estimate_parts`]).
pub fn size_of(src: &Target) -> Result<Value> {
    let c = &src.cauldron;
    let soup = c.soup();
    let ripple_txs = crate::reads::ripple_commits(c)?;
    let (mut records, mut ripple, mut edges, mut ripple_edges) = (0u64, 0u64, 0u64, 0u64);
    for pool in pools_of(&soup)? {
        let conn = spine::offline::read_only_connection(&Pool::path_in(&soup, &PoolName::new(&pool))).map_err(store)?;
        let mut st = conn.prepare("SELECT created_tx FROM nodes").map_err(store)?;
        let rows = st.query_map([], |r| r.get::<_, String>(0)).map_err(store)?;
        for r in rows {
            records += 1;
            if ripple_txs.contains(&r.map_err(store)?) {
                ripple += 1;
            }
        }
        // An edge lands with the record it leaves: in the ripple part when
        // a ripple wrote that record.
        let mut st = conn.prepare("SELECT n.created_tx FROM edges e LEFT JOIN nodes n ON n.id = e.src").map_err(store)?;
        let rows = st.query_map([], |r| r.get::<_, Option<String>>(0)).map_err(store)?;
        for r in rows {
            edges += 1;
            if r.map_err(store)?.is_some_and(|tx| ripple_txs.contains(&tx)) {
                ripple_edges += 1;
            }
        }
    }
    let dumps = dumps::all_nodes(c)?;
    let blob_bytes: u64 = dumps.iter().filter_map(|n| dumps::blob_of(n).1.get("bytes").and_then(Value::as_u64)).sum();
    // What each of the merge's soup writes spends of the soup's
    // per-statement fuel: about 3 a record and 5 an edge
    // (`spine::ladle::compile`'s FUEL_PER_*), plus about 4 for each record
    // of the target an edge lands on, which the source's files cannot tell
    // (so 10% is kept in hand). The main write and the ripple part are two
    // statements, each with its own fuel. A write over it CANNOT land: the
    // judge that is tried first gives up, the record-by-record judge behind
    // it passes, and then the one landing statement runs out of fuel after
    // all, long after the target was held (measured: 19,001 records with
    // 228,000 edges failed after 25 minutes). So it is refused up front.
    let fuel_main = 3 * (records - ripple) + 5 * (edges - ripple_edges);
    let fuel_ripple = 3 * ripple + 5 * ripple_edges;
    let fuel = fuel_main.max(fuel_ripple);
    let (fuel_ceiling, secs_ceiling) = budgets();
    let over_fuel = fuel * 10 > fuel_ceiling * 9;
    let (secs, mb) = estimate_parts(records, edges, blob_bytes);
    let over_records = records > CEILING_RECORDS;
    let over_time = secs > secs_ceiling;
    Ok(json!({
        "fuel": {
            "estimated": fuel,
            "main_write": fuel_main,
            "ripple_part": fuel_ripple,
            "ceiling": fuel_ceiling,
            "over": over_fuel,
            "note": if over_fuel {
                "over 90% of the soup's per-statement fuel: the write cannot land as one statement, so the merge is refused, --allow-large or not"
            } else {
                "within the soup's per-statement fuel, with 10% kept in hand for what the target holds"
            },
        },
        "records": records,
        "ripple_records": ripple,
        "edges": edges,
        "ripple_edges": ripple_edges,
        "dumps": dumps.len(),
        "dump_bytes": blob_bytes,
        "ceiling": CEILING_RECORDS,
        "seconds_ceiling": secs_ceiling,
        "over_ceiling": over_records || over_time,
        "over": {"records": over_records, "time": over_time, "fuel": over_fuel},
        "estimate": {
            "seconds": secs,
            "memory_mb": mb,
            "basis": ESTIMATE_BASIS,
        },
        "while_it_runs": "the target refuses every other writer (imports, dumps, ripples, and the `mcp` service's sessions, which are answered at once with `busy` and a retry_after) for most of the merge",
    }))
}

/// What [`estimate_parts`] is fitted to.
const ESTIMATE_BASIS: &str = "fitted to merges measured on one machine (Apple silicon, a release build, 200-character bodies, one or two edges a record): 6.7 s and 620 MB at 11,000 records, 13.8 s and 1.2 GB at 22,000, 23.7 s and 1.8 GB at 33,000, 55 s and 3.1 GB at 55,000; each edge beyond one a record adds about 55 µs and 5.5 KB (5,000 records with 30 edges each: 9.4 s and 1.1 GB), and each dump byte is read to be copied and checked; a debug build is several times slower, and a slower machine is slower";

/// Seconds and megabytes a merge of `records` records is estimated to take
/// (fitted to the measurements [`size_of`] names: `a·n + b·n²` seconds,
/// `30 + 0.056·n` MB).
pub fn estimate(records: u64) -> (u64, u64) {
    let n = records as f64;
    let secs = 5.1e-4 * n + 9.1e-9 * n * n;
    let mb = 30.0 + 0.056 * n;
    (secs.ceil() as u64, mb.ceil() as u64)
}

/// [`estimate`], with what else costs: each edge beyond one a record
/// ([`SECS_PER_EDGE`]), and the dumps' bytes, which are read to be staged
/// and checked ([`BYTES_PER_SEC`]).
pub fn estimate_parts(records: u64, edges: u64, dump_bytes: u64) -> (u64, u64) {
    let (secs, mb) = estimate(records);
    let extra_edges = edges.saturating_sub(records) as f64;
    let secs = secs as f64 + SECS_PER_EDGE * extra_edges + dump_bytes as f64 / BYTES_PER_SEC;
    let mb = mb as f64 + MB_PER_EDGE * extra_edges;
    (secs.ceil() as u64, mb.ceil() as u64)
}

/// The time an edge beyond one a record adds to a merge (measured on the
/// same machine, a release build: 5,000 records with 1, 10 and 30 edges
/// each merged in 2.0 s / 290 MB, 4.4 s / 557 MB and 9.4 s / 1,079 MB, so
/// about 51 µs and 5.4 KB an edge; a little more is used here).
const SECS_PER_EDGE: f64 = 5.5e-5;
/// The memory an edge beyond one a record adds (see [`SECS_PER_EDGE`]).
const MB_PER_EDGE: f64 = 0.0055;
/// How fast a dump's bytes are staged and checked (a conservative read
/// rate).
const BYTES_PER_SEC: f64 = 250.0e6;

/// **Refused before anything is held, read in full or written**
/// ([`Code::TooLarge`]), with the size, the estimate, and what to do:
///
/// - over [`CEILING_RECORDS`] records, or estimated to hold the target
///   longer than a merge of that many ordinary records would (edges and
///   dump bytes count), unless `allow_large` is given;
/// - over the soup's per-statement fuel, always: such a write cannot land,
///   and `--allow-large` would only buy a long hold and a failure.
///
/// Answers the size (with `allow_large` said, when it let a large merge
/// through).
pub fn refuse_large(src: &Target, dst: &Target, allow_large: bool, command: &str) -> Result<Value> {
    let mut size = size_of(src)?;
    let detail = |size: &Value| json!({"outcome": "refused", "size": size});
    if size["fuel"]["over"] == json!(true) {
        return Err(Error::new(
            Code::TooLarge,
            format!(
                "REFUSED: the source, cauldron `{}` ({} records, {} edges), cannot be merged as one write: its {} would spend about \
                 {} of the soup's fuel, and one statement gets {} (with 10% kept in hand for what cauldron `{}` holds). Such a \
                 merge would hold the target for a long time and then fail, so it is refused before anything is read in full, held \
                 or written, and --allow-large does not change that. Edges cost the most: merge the source in parts (move some of \
                 its records into another cauldron, and merge each), or drop edges it does not need.",
                src.cauldron.name,
                size["records"],
                size["edges"],
                if size["fuel"]["main_write"].as_u64() >= size["fuel"]["ripple_part"].as_u64() { "main write" } else { "ripple part" },
                size["fuel"]["estimated"],
                size["fuel"]["ceiling"],
                dst.cauldron.name
            ),
        )
        .with_detail(detail(&size)));
    }
    if size["over_ceiling"] != json!(true) {
        return Ok(size);
    }
    let why = if size["over"]["records"] == json!(true) {
        format!("more than a merge takes by default ({CEILING_RECORDS} records)")
    } else {
        format!(
            "under {CEILING_RECORDS} records, but with {} edges and {} bytes of dumps it is estimated to take longer than a merge \
             of that many ordinary records ({} s)",
            size["edges"], size["dump_bytes"], size["seconds_ceiling"]
        )
    };
    if allow_large {
        size["allow_large"] = json!(format!(
            "{why}; run with --allow-large: estimated about {} s and {} MB; the target refuses every other writer meanwhile",
            size["estimate"]["seconds"], size["estimate"]["memory_mb"]
        ));
        return Ok(size);
    }
    Err(Error::new(
        Code::TooLarge,
        format!(
            "REFUSED: the source, cauldron `{}`, holds {} records ({} written by a ripple, {} edges, {} dump(s)), {why}. A merge of \
             it is estimated at about {} s and {} MB on the machine it was measured on (time grows faster than the record count), \
             and cauldron `{}` refuses every other writer for most of that time. Nothing was read in full, held or written. To \
             merge it anyway, knowing that: `{command} --allow-large`. Or split the source first: move part of it into another \
             cauldron and merge each.",
            src.cauldron.name,
            size["records"],
            size["ripple_records"],
            size["edges"],
            size["dumps"],
            size["estimate"]["seconds"],
            size["estimate"]["memory_mb"],
            dst.cauldron.name
        ),
    )
    .with_detail(detail(&size)))
}

// ------------------------------------------------------------ the source

/// **The source, held**: its soup's writer lock and its dumps store's, each
/// taken exclusive (opened as they are, never made and never written), so
/// no writer starts on the source while the merge reads it. Dropped, they
/// are let go.
pub struct SourceHold {
    _files: Vec<std::fs::File>,
}

/// **Hold the source** for a merge, or refuse naming what holds it: a
/// writer (the `mcp` service, an import), a live ripple run, a held region
/// lock, a merge into it cut short, or a soup not at rest. Writes nothing to
/// the source.
pub fn hold_source(t: &Target) -> Result<SourceHold> {
    let c = &t.cauldron;
    crate::lock::refuse_live_soup(&t.env)?;
    let mut files = Vec::new();
    for (path, what) in [(c.soup().join(crate::lock::WRITER_LOCK), "soup"), (dumps::dir(c).join(dumps::WRITER_LOCK), "dumps store")] {
        let fh = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(store(format!("opening {}: {e}", path.display()))),
        };
        match fh.try_lock() {
            Ok(()) => files.push(fh),
            Err(std::fs::TryLockError::WouldBlock) => {
                let says = std::fs::read_to_string(&path).unwrap_or_default();
                let holder = match says.trim().strip_prefix(crate::lock::MCP_SERVICE).and_then(|p| p.trim().parse::<u32>().ok()) {
                    Some(pid) => format!(
                        "the `{}` service (pid {pid}) holds it (the cauldron is granted to the MCP door and a call opened it); \
                         close it first with `cauldron mcp revoke {} --env {}` (grant it again afterwards) or stop the service \
                         with `cauldron --env {} stop mcp`",
                        crate::lock::MCP_SERVICE,
                        c.name,
                        t.env.name,
                        t.env.name
                    ),
                    None => "another writer is writing to it right now; try again when it has finished".to_string(),
                };
                refuse!(
                    Code::Locked,
                    "REFUSED: the source, cauldron `{}`, is held: its {what}'s writer lock ({}) is taken, so it could change \
                     while it is read. {holder}. Nothing was written.",
                    c.name,
                    path.display()
                );
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(store(format!("locking {}: {e}", path.display()))),
        }
    }
    let runs = crate::region_lock::live_ripple_runs(c);
    if !runs.is_empty() {
        return Err(Error::new(
            Code::Locked,
            format!(
                "REFUSED: a ripple run of the source, cauldron `{}`, may still be running ({}), and what it writes would not be \
                 in the merge. Wait for it to finish (`cauldron ripples status --env {}`). Nothing was written.",
                c.name,
                runs.iter().map(|r| format!("run {} on {}", r["run"].as_str().unwrap_or("?"), r["dump"].as_str().unwrap_or("?"))).collect::<Vec<_>>().join(", "),
                t.env.name
            ),
        )
        .with_detail(json!({"live_runs": runs})));
    }
    crate::region_lock::refuse_held(c, VERB)?;
    if pending(c)?.is_some() {
        refuse!(
            Code::NotAtRest,
            "REFUSED: the source, cauldron `{}`, has a merge into it that was cut short; run `cauldron soup recover --env {} \
             --cauldron {}` first. Nothing was written.",
            c.name,
            t.env.name,
            c.name
        );
    }
    let unrest = spine::offline::rest_check(&c.soup()).map_err(|e| store(format!("reading the source's spine: {e}")))?;
    if !unrest.is_empty() {
        refuse!(
            Code::NotAtRest,
            "REFUSED: the source, cauldron `{}`'s soup, is not at rest ({}); run `cauldron soup recover --env {} --cauldron {}` \
             first. Nothing was written.",
            c.name,
            unrest.join("; "),
            t.env.name,
            c.name
        );
    }
    Ok(SourceHold { _files: files })
}

// -------------------------------------------------------------- reading

struct Read {
    pool: String,
    n: NodeRecord,
    edges: Vec<EdgeRecord>,
    created: Option<String>,
}

/// The pool names of the soup at `soup` (every `<pool>.db` but the spine).
fn pools_of(soup: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(soup).map_err(|e| store(format!("listing {}: {e}", soup.display())))? {
        let p = entry.map_err(store)?.path();
        let stem = p.file_stem().and_then(|n| n.to_str()).unwrap_or("").to_string();
        if p.extension().and_then(|x| x.to_str()) == Some("db") && stem != "spine" {
            out.push(stem);
        }
    }
    out.sort();
    Ok(out)
}

fn read_pool(soup: &Path, pool: &str) -> Result<Vec<Read>> {
    let name = PoolName::new(pool);
    let handle = Pool::open_read_only(&Pool::path_in(soup, &name)).map_err(|e| store(format!("reading pool `{pool}`: {e}")))?;
    let snap = handle.snapshot().map_err(store)?;
    let types: Vec<String> = handle.grammar().node_types.keys().cloned().collect();
    let mut out = Vec::new();
    for ty in &types {
        for n in snap.nodes_of(ty, None).map_err(store)? {
            let edges = snap.edges_from(&n.id, None).map_err(store)?;
            let created = snap.created_tx(&n.id).map_err(store)?.map(|h| h.to_hex());
            out.push(Read { pool: pool.to_string(), n, edges, created });
        }
    }
    out.sort_by(|a, b| {
        a.pool
            .cmp(&b.pool)
            .then_with(|| crate::reads::prefix(&a.n.id.0).cmp(crate::reads::prefix(&b.n.id.0)))
            .then_with(|| crate::reads::serial(&a.n.id.0).cmp(&crate::reads::serial(&b.n.id.0)))
            .then_with(|| a.n.id.0.cmp(&b.n.id.0))
    });
    Ok(out)
}

/// Every decided transaction row of the soup at `soup`: hash -> (initiator,
/// time).
fn tx_rows(soup: &Path) -> Result<HashMap<String, (Value, String)>> {
    let conn = spine::offline::read_only_connection(&soup.join(spine::spine_db::SPINE_FILE)).map_err(store)?;
    let mut stmt = conn.prepare("SELECT hash, initiator, ts FROM transactions WHERE decided = 1").map_err(store)?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))).map_err(store)?;
    let mut out = HashMap::new();
    for row in rows {
        let (h, i, ts) = row.map_err(store)?;
        out.insert(h, (serde_json::from_str(&i).unwrap_or(Value::Null), ts));
    }
    Ok(out)
}

/// One record's review decision as the source's own write history records
/// it: the LAST landed transaction that wrote `meta.<flag>` of that record
/// (a CREATE carrying it, a SET of the whole `meta`, or a SET of the flag),
/// what it wrote, and who wrote it when.
#[derive(Debug, Clone)]
struct DecisionWrite {
    tx: String,
    initiator: Value,
    at: String,
    value: Value,
}

/// **Every decision write of the soup at `soup`**, by (pool, record id,
/// flag): the last transaction that wrote each record's `meta.approved` and
/// `meta.rejected` (read-only; only rows whose payload names a flag are
/// parsed).
fn decision_writes(soup: &Path) -> Result<HashMap<(String, String, &'static str), DecisionWrite>> {
    let conn = spine::offline::read_only_connection(&soup.join(spine::spine_db::SPINE_FILE)).map_err(store)?;
    let mut stmt = conn
        .prepare(
            "SELECT hash, initiator, ts, payload FROM transactions WHERE decided = 1 \
             AND (payload LIKE '%\"approved\"%' OR payload LIKE '%\"rejected\"%') ORDER BY seq",
        )
        .map_err(store)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))
        .map_err(store)?;
    let mut out: HashMap<(String, String, &'static str), DecisionWrite> = HashMap::new();
    for row in rows {
        let (tx, initiator, at, payload) = row.map_err(store)?;
        let initiator: Value = serde_json::from_str(&initiator).unwrap_or(Value::Null);
        let payload: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
        for part in payload["parts"].as_array().into_iter().flatten() {
            let pool = part["pool"].as_str().unwrap_or("").to_string();
            for d in part["deltas"].as_array().into_iter().flatten() {
                for flag in ["approved", "rejected"] {
                    let (node, value) = match d["op"].as_str() {
                        Some("create_node") => (d["id"].as_str(), d["fields"]["meta"].get(flag).cloned()),
                        Some("set_field") => {
                            let path: Vec<&str> = d["path"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                            match path.as_slice() {
                                ["meta"] => (d["node"].as_str(), Some(d["value"].get(flag).cloned().unwrap_or(Value::Null))),
                                ["meta", f] if *f == flag => (d["node"].as_str(), Some(d["value"].clone())),
                                _ => (None, None),
                            }
                        }
                        _ => (None, None),
                    };
                    if let (Some(node), Some(value)) = (node, value) {
                        out.insert((pool.clone(), node.to_string(), flag), DecisionWrite { tx: tx.clone(), initiator: initiator.clone(), at: at.clone(), value });
                    }
                }
            }
        }
    }
    Ok(out)
}

/// The commands an owner's decision is written by, as its transaction's
/// `dispatch` names them (`cauldron-mcp`'s DECIDE and ADECIDE take only
/// these).
const DECISION_COMMANDS: &[&str] =
    &["cauldron soup approve", "cauldron ripples approve", "cauldron ripples reject", "cauldron ripples review --interactive"];

/// The MCP tools a session decides with, where the owner opened `approve`
/// to sessions (`env::owner_acts`), as their transaction's `dispatch`
/// names them.
const DECISION_TOOLS: &[&str] = &["mcp soup_approve", "mcp soup_reject"];

/// **Who made a decision**, as the source's write history records it: the
/// transaction's initiator and the flag's own `by`, which must agree.
///
/// - `tty`: the owner's command run with a terminal attached (thread
///   `owner`, one of [`DECISION_COMMANDS`]; the flag says `by: "tty"`, or
///   `by: "owner"` as releases before `owner_acts` wrote it);
/// - `agent`: the owner's command run without one where the owner opened
///   `approve` (thread `owner:agent`, one of [`DECISION_COMMANDS`]), or an
///   MCP session's `soup_approve` / `soup_reject` where the owner opened it
///   to sessions (thread `mcp:…`, one of [`DECISION_TOOLS`]); the flag says
///   `by: "agent"`.
///
/// Anything else is not a decision (`Err` says what it was).
fn decided_by(initiator: &Value, value: &Value) -> std::result::Result<Value, String> {
    let class = initiator["class"].as_str().unwrap_or("");
    let thread = initiator["thread"].as_str().unwrap_or("");
    let dispatch = initiator["dispatch"].as_str().unwrap_or("");
    let flag_by = value.get("by").and_then(Value::as_str).unwrap_or("");
    let written_as = || {
        format!("written as {class} `{thread}`{}", if dispatch.is_empty() { String::new() } else { format!(" by `{dispatch}`") })
    };
    if class != "agent" || dispatch.starts_with(provenance::MERGE_DISPATCH) {
        return Err(format!("it was not a decision ({})", written_as()));
    }
    let by = if thread == "owner" && DECISION_COMMANDS.contains(&dispatch) {
        "tty"
    } else if (thread == "owner:agent" && DECISION_COMMANDS.contains(&dispatch)) || (thread.starts_with("mcp:") && DECISION_TOOLS.contains(&dispatch)) {
        "agent"
    } else {
        return Err(format!("it was not a decision by the owner's commands or a session's decision tools ({})", written_as()));
    };
    let agrees = match by {
        "tty" => flag_by == "tty" || flag_by == "owner",
        _ => flag_by == "agent",
    };
    if !agrees {
        return Err(format!("its transaction records a decision by {by}, but the flag says `by: {flag_by:?}` ({})", written_as()));
    }
    let mut d = json!({"by": by, "how": dispatch, "thread": thread});
    if let Some(sess) = value.get("session").and_then(Value::as_str) {
        d["session"] = json!(sess);
    }
    Ok(d)
}

/// **Does the source's own history vouch for a record's decision?** The
/// flag `flag` of record `id` (in `pool`) holding `value` is carried only
/// when the last transaction that wrote it (`writes`) wrote exactly that
/// value AND was either a decision ([`decided_by`]: the owner's command at
/// a terminal, `by: "tty"`, or, where the owner opened `approve`, without
/// one or through a session's decision tool, `by: "agent"`, carried the
/// same way) or a merge of the source that itself carried a decision its
/// source vouched for (the record's `merged_from.decision`, which only a
/// merge writes). Answers who decided (`by`), how, and when, or why it is
/// not carried.
fn vouch(
    writes: &HashMap<(String, String, &'static str), DecisionWrite>,
    merges: &HashMap<String, MergeTx>,
    pool: &str,
    id: &str,
    flag: &'static str,
    value: &Value,
    meta: &Value,
) -> std::result::Result<Value, String> {
    let Some(w) = writes.get(&(pool.to_string(), id.to_string(), flag)) else {
        return Err(format!("the source's write history holds no write of `meta.{flag}` on {id}: the flag was put there by editing the cauldron's files, not by a decision"));
    };
    if &w.value != value {
        return Err(format!(
            "the last write of `meta.{flag}` on {id} in the source's history (transaction {} at {}) wrote another value than the record holds: it was changed since by editing the cauldron's files",
            &w.tx[..12.min(w.tx.len())],
            w.at
        ));
    }
    if let Some(m) = merges.get(&w.tx) {
        // Carried into the source by a merge: that merge recorded the
        // decision it carried, with who and when.
        return match meta.get("merged_from").and_then(|f| f.get("decision")).filter(|d| d["flag"] == flag && d["by"].is_string()) {
            Some(d) => {
                let by = match d["by"].as_str() {
                    Some("owner") => "tty",
                    Some(b) => b,
                    None => "",
                };
                let mut out = json!({"flag": flag, "by": by, "at": d["at"], "tx": d["tx"], "how": d["how"], "via": {"merge": m.merge, "at": m.at, "tx": w.tx}});
                for k in ["thread", "session"] {
                    if d[k].is_string() {
                        out[k] = d[k].clone();
                    }
                }
                Ok(out)
            }
            None => Err(format!("{id} came into the source by merge {}, which carried no decision for it (its `meta.{flag}` has no recorded decision behind it)", m.merge)),
        };
    }
    match decided_by(&w.initiator, value) {
        Ok(mut d) => {
            d["flag"] = json!(flag);
            d["at"] = json!(w.at);
            d["tx"] = json!(w.tx);
            Ok(d)
        }
        Err(what) => Err(format!("the last write of `meta.{flag}` on {id} in the source was not a decision: {what}")),
    }
}

fn field<'a>(n: &'a NodeRecord, k: &str) -> Option<&'a Value> {
    n.fields.get(k).map(|f| &f.value)
}

fn norm_text(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn truthy(v: Option<&Value>) -> bool {
    v.is_some_and(|a| !a.is_null() && *a != Value::Bool(false))
}

/// Replace every whole token of `text` that `map` names (a token is a run
/// of letters, digits and `-`) by what it maps to. Answers the new text and
/// how many tokens it replaced.
fn retoken(text: &str, map: &BTreeMap<String, String>) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut n = 0;
    let mut tok = String::new();
    let flush = |tok: &mut String, out: &mut String, n: &mut usize| {
        if let Some(to) = map.get(tok.as_str()) {
            out.push_str(to);
            *n += 1;
        } else {
            out.push_str(tok);
        }
        tok.clear();
    };
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            tok.push(ch);
        } else {
            flush(&mut tok, &mut out, &mut n);
            out.push(ch);
        }
    }
    flush(&mut tok, &mut out, &mut n);
    (out, n)
}

/// **Where a record (or a dump) originated**, from a `meta.merged_from`: the
/// cauldron id and id of the deepest one (each merge keeps the earlier
/// provenance under `via`).
fn origin(from: &Value) -> Option<(String, String)> {
    let mut cur = from.as_object().map(|_| from)?;
    while let Some(v) = cur.get("via").filter(|v| v.is_object()) {
        cur = v;
    }
    Some((cur.get("cauldron_id")?.as_str()?.to_string(), cur.get("id")?.as_str()?.to_string()))
}

/// Every `[[label]]` of `text` whose label `map` names becomes `[[its id]]`,
/// in one pass over the text (a link's label looked up in the map), so the
/// cost grows with the text, not with the text times the batch.
fn relink(text: &str, map: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("]]") {
            Some(end) => {
                let inner = &after[..end];
                out.push_str("[[");
                out.push_str(map.get(inner).map(String::as_str).unwrap_or(inner));
                out.push_str("]]");
                rest = &after[end + 2..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// `[[ID]]` links to the source's own records become `[[from:ID]]`, a label
/// of the batch, which the soup's door then rewrites to the new id; a link to
/// a record already in the target names the target's copy.
fn relabel_links(text: &str, ids: &BTreeSet<String>, existing: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("]]") {
            Some(end) => {
                let inner = &after[..end];
                out.push_str("[[");
                if ids.contains(inner) {
                    out.push_str(&label(inner));
                } else if let Some(t) = existing.get(inner) {
                    out.push_str(t);
                } else {
                    out.push_str(inner);
                }
                out.push_str("]]");
                rest = &after[end + 2..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------- the plan

/// How a merge is run.
#[derive(Debug, Clone, Default)]
pub struct Opts {
    /// The plan only: nothing is written, nothing is held.
    pub dry_run: bool,
    /// The command line found its input and output to be an interactive
    /// terminal (`by: "tty"`); otherwise the merge is an agent's (`by:
    /// "agent"`). A merge that carries a decision asks the owner acts' one
    /// gate for the act `merge` (`env::owner_acts::decide`), which a
    /// terminal always passes.
    pub at_terminal: bool,
    /// `CAULDRON_OWNER_ACTS` as the command line that runs the merge has it
    /// (`None` for the `mcp` service, which never reads it: only the
    /// persisted setting opens an agent's merge there).
    pub var: Option<String>,
    /// The command a refusal names, to run at a terminal.
    pub command: String,
}

/// **Who merges, and on what authority**: `by` (`tty` or `agent`), the
/// spine thread the merge's writes are recorded under (`owner` or
/// `owner:agent`, as the owner's decisions are), and where the permission
/// came from.
#[derive(Debug, Clone)]
pub struct Merger {
    pub by: env::owner_acts::By,
    pub thread: String,
    pub allowed: String,
}

impl Merger {
    fn of(by: env::owner_acts::By, allowed: String) -> Merger {
        let thread = match by {
            env::owner_acts::By::Tty => "owner".to_string(),
            env::owner_acts::By::Agent => "owner:agent".to_string(),
        };
        Merger { by, thread, allowed }
    }
}

/// One dump to carry.
#[derive(Debug, Clone)]
struct Carry {
    source_id: String,
    state: String,
    fields: Map<String, Value>,
    /// The blob it names: its file under the store (`blobs/<sha>.<ext>`).
    blob: Option<String>,
}

/// **A merge, planned**: what it would carry and how, what it would not and
/// why, and what blocks it. Read-only to make; [`apply`] lands it.
pub struct Prepared {
    /// The plan, as printed.
    pub doc: Value,
    /// Whether it carries any review decision (and so is the owner act
    /// `merge`: at a terminal, or where the owner opened it).
    pub needs_terminal: bool,
    /// Whether anything blocks it (each reason is in `doc.blocking`).
    pub blocked: bool,
    /// Whether it writes to the target's dumps store.
    pub needs_dumps: bool,
    /// The code a refusal carries when blocked.
    pub code: Code,
    merge: String,
    records: Vec<Value>,
    carries: Vec<Carry>,
    /// Source id -> label, every record of the batch.
    labels: BTreeMap<String, String>,
    /// Source id -> the target's id, for every record already here.
    existing: BTreeMap<String, String>,
    /// The labels of the records that land in the ripple part.
    ripple_part: BTreeSet<String>,
    /// The flag fields the ripple part's records carry (its scope).
    ripple_scope: Vec<String>,
    /// Source dump id -> the target's dump id, for every dump already here.
    existing_dumps: BTreeMap<String, String>,
    source_dumps_dir: PathBuf,
    wants_cluster_pool: bool,
    source: Value,
    /// The target, as the owner acts' gate names it.
    gate: Gate,
}

/// The target as the owner acts' gate asks about it.
#[derive(Debug, Clone)]
struct Gate {
    root: PathBuf,
    env: String,
    cauldron: String,
    id: Option<String>,
}

/// The next merge id of the target: one past every merge it has a
/// directory or a landed transaction for.
fn next_merge_id(c: &env::Cauldron, txs: &HashMap<String, MergeTx>) -> Result<String> {
    let mut top = 0u64;
    if let Ok(rd) = std::fs::read_dir(dir(c)) {
        for e in rd.flatten() {
            if let Some(n) = merge_serial(&e.file_name().to_string_lossy()) {
                top = top.max(n);
            }
        }
    }
    for m in txs.values() {
        if let Some(n) = merge_serial(&m.merge) {
            top = top.max(n);
        }
    }
    Ok(format!("{PREFIX}-{}", top + 1))
}

/// The written-by of a source record: who wrote it there, and (when a merge
/// carried it there) its earlier provenance.
fn written_by(r: &Read, rows: &HashMap<String, (Value, String)>, merges: &HashMap<String, MergeTx>) -> (Value, Option<Value>) {
    let meta = field(&r.n, "meta").cloned().unwrap_or(Value::Null);
    if let Some(h) = &r.created {
        if let Some(m) = merges.get(h) {
            if let Some(from) = meta.get("merged_from").filter(|v| v.is_object()) {
                // The provenance chains to the source's own record of the
                // merge that carried it there: that transaction, who, when.
                let mut via = from.clone();
                via["landed_by"] = json!({"merge": m.merge, "tx": h, "at": m.at});
                return (from.get("written_by").cloned().unwrap_or(json!({"class": "unknown"})), Some(via));
            }
        }
        if let Some((i, at)) = rows.get(h) {
            let class = i["class"].as_str().unwrap_or("unknown");
            let w = match class {
                "ripple" => json!({"class": "ripple", "kind": i["kind"], "run": i["run_id"], "step": i["step_id"], "at": at}),
                "agent" => json!({"class": "agent", "thread": i["thread"], "at": at}),
                "core_mechanical" => json!({"class": "core_mechanical", "operation": i["operation"], "at": at}),
                other => json!({"class": other, "at": at}),
            };
            return (w, None);
        }
    }
    (json!({"class": "unknown"}), None)
}

/// **Plan the merge of `src` into `dst`**, both in one environment, reading
/// both through connections that cannot write. The plan's ids in the target
/// are filled in by [`predict`] (a dry run predicts them on a snapshot).
pub fn prepare(src: &Target, dst: &Target) -> Result<Prepared> {
    let (s, d) = (&src.cauldron, &dst.cauldron);
    if src.env.name != dst.env.name {
        refuse!(Code::Usage, "REFUSED: a merge is between two cauldrons of one environment. Nothing was written.");
    }
    if s.soup() == d.soup() {
        refuse!(Code::Usage, "REFUSED: `{}` is the target itself; a cauldron is not merged into itself. Nothing was written.", s.name);
    }
    let Some(source_id) = s.id() else {
        refuse!(
            Code::Usage,
            "REFUSED: the source, cauldron `{}`, has no cauldron id yet (it was made before cauldrons had ids, and no writer has \
             opened it since), and a merge records where its records came from by that id. Give it one by opening it for \
             writing once, e.g. `cauldron soup recover --env {} --cauldron {}`. Nothing was written.",
            s.name,
            src.env.name,
            s.name
        );
    };
    let (src_soup, dst_soup) = (s.soup(), d.soup());
    let src_pools = pools_of(&src_soup)?;
    let dst_pools = pools_of(&dst_soup)?;
    let rows = tx_rows(&src_soup)?;
    let src_merges = provenance::merge_txs(&src_soup)?;
    let writes = decision_writes(&src_soup)?;
    let dst_merges = provenance::merge_txs(&dst_soup)?;
    let merge = next_merge_id(d, &dst_merges)?;
    let cluster_pool = crate::clusters::POOL;
    let carried_pools = [DEFAULT_POOL, cluster_pool];

    let mut blocking: Vec<Value> = Vec::new();
    let mut not_carried: Vec<Value> = Vec::new();
    let mut notes: Vec<Value> = Vec::new();
    let mut code = Code::MergeBlocked;

    // ---- the source ----
    let mut nodes: Vec<Read> = Vec::new();
    for p in &src_pools {
        if carried_pools.contains(&p.as_str()) {
            nodes.extend(read_pool(&src_soup, p)?);
            continue;
        }
        // A user grammar's pool (issue #28) is not carried in this release.
        // Its records are never left behind silently: a source pool that
        // holds any refuses the whole merge (nothing is half-merged), and
        // an empty one is listed as not carried.
        let held = read_pool(&src_soup, p)?.len();
        if held > 0 {
            blocking.push(json!({
                "what": format!("user pool `{p}` of the source ({held} record(s))"),
                "why": format!(
                    "a merge carries the soup's `ws` pool and its cluster pool, and merging the records of a user grammar is not \
                     built in this release, so carrying the rest would leave pool `{p}`'s records behind (and the edges from them)"
                ),
                "fix": "merge a source whose user pools are empty, or keep the two cauldrons apart; the source is not changed",
            }));
        } else {
            not_carried.push(json!({"what": format!("pool `{p}`"), "why": "an empty user grammar's pool: a merge does not register a grammar in the target (`cauldron soup grammar add` does)"}));
        }
    }
    let src_dumps = dumps::all_nodes(s)?;
    let digest = content_digest(&nodes, &src_dumps);
    let target_id = d.id();

    // ---- the target as it stands ----
    let mut dst_ids: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut dst_regions: Vec<(String, String, String)> = Vec::new();
    let mut dst_clusters: Vec<(String, String, String)> = Vec::new();
    let mut dst_roles: BTreeSet<String> = BTreeSet::new();
    // LINEAGE: every record of the target by where it ORIGINATED (cauldron
    // id and record id): its own, or, for one a merge carried in, the
    // deepest `meta.merged_from` its merge wrote. A source record whose
    // origin is already here is not carried again.
    let mut here: HashMap<(String, String), (String, String, String)> = HashMap::new();
    for p in &dst_pools {
        for r in read_pool(&dst_soup, p)? {
            let meta = field(&r.n, "meta").cloned().unwrap_or(Value::Null);
            let by_merge = r.created.as_ref().and_then(|h| dst_merges.get(h));
            let key = match (by_merge, meta.get("merged_from").and_then(origin)) {
                (Some(_), Some(o)) => Some(o),
                _ => target_id.clone().map(|t| (t, r.n.id.0.clone())),
            };
            if let Some(k) = key {
                let via = by_merge.map(|m| m.merge.clone()).unwrap_or_else(|| "the target's own".into());
                here.entry(k).or_insert((r.n.id.0.clone(), p.clone(), via));
            }
            let text = |k: &str| field(&r.n, k).and_then(Value::as_str).unwrap_or("").to_string();
            match r.n.node_type.as_str() {
                "region" => dst_regions.push((r.n.id.0.clone(), text("handle"), text("boundary"))),
                "cluster" => dst_clusters.push((r.n.id.0.clone(), text("handle"), text("boundary"))),
                "viewer" => {
                    dst_roles.insert(text("role"));
                }
                _ => {}
            }
            dst_ids.insert(r.n.id.0.clone(), (p.clone(), r.n.node_type.clone()));
        }
    }

    // ---- the dumps ----
    // Lineage, as for records: a dump of the source whose origin the target
    // already holds is not carried again; a cite of it names the target's.
    let mut dumps_here: HashMap<(String, String), String> = HashMap::new();
    for n in dumps::all_nodes(d)? {
        let from = n.fields.get("meta").and_then(|m| m.value.get("merged_from")).and_then(origin);
        if let Some(k) = from.or_else(|| target_id.clone().map(|t| (t, n.id.0.clone()))) {
            dumps_here.entry(k).or_insert(n.id.0.clone());
        }
    }
    let mut existing_dumps: BTreeMap<String, String> = BTreeMap::new();
    let mut dumps_already: Vec<Value> = Vec::new();
    let src_dumps_dir = dumps::dir(s);
    let dst_dumps_dir = dumps::dir(d);
    let mut carries: Vec<Carry> = Vec::new();
    let mut dump_rows: Vec<Value> = Vec::new();
    let (mut copied, mut shared, mut tombstones) = (0usize, 0usize, 0usize);
    let mut staged_seen: BTreeSet<String> = BTreeSet::new();
    let mut dump_ids: BTreeSet<String> = BTreeSet::new();
    let mut by_state: BTreeMap<String, usize> = BTreeMap::new();
    let mut src_dumps_sorted = src_dumps.clone();
    src_dumps_sorted.sort_by_key(|n| crate::reads::serial(&n.id.0));
    for n in &src_dumps_sorted {
        let id = n.id.0.clone();
        dump_ids.insert(id.clone());
        let from = n.fields.get("meta").and_then(|m| m.value.get("merged_from")).and_then(origin);
        let key = from.unwrap_or_else(|| (source_id.clone(), id.clone()));
        if let Some(t) = dumps_here.get(&key) {
            existing_dumps.insert(id.clone(), t.clone());
            dumps_already.push(json!({"id": id, "as": t, "origin": {"cauldron_id": key.0, "id": key.1}}));
            continue;
        }
        let state = n.state.clone().unwrap_or_default();
        *by_state.entry(state.clone()).or_default() += 1;
        if dumps::removal_unfinished(n) {
            blocking.push(json!({
                "what": format!("dump {id} of the source"),
                "why": "its removal was cut short (state `removing`): its text is half erased, so it is neither a dump nor a tombstone yet",
                "fix": dumps::remove_command(s, &id),
            }));
            continue;
        }
        let (kind, blob) = dumps::blob_of(n);
        let file = blob.get("file").and_then(Value::as_str).map(str::to_string);
        let how = match (&file, state.as_str()) {
            (_, "removed") => {
                tombstones += 1;
                "none (a tombstone: its text was removed)"
            }
            (None, _) => "none",
            (Some(f), _) => {
                if !src_dumps_dir.join(f).is_file() {
                    blocking.push(json!({
                        "what": format!("dump {id} of the source"),
                        "why": format!("its blob {f} is not in the source's dumps store, so its text cannot be carried"),
                        "fix": format!("read it with `cauldron soup dump-get {id} --env {} --cauldron {}`; remove it from the source with `{}` if it is lost", src.env.name, s.name, dumps::remove_command(s, &id)),
                    }));
                    "missing"
                } else if dst_dumps_dir.join(f).is_file() || staged_seen.contains(f) {
                    shared += 1;
                    "shared (the target holds the same bytes)"
                } else {
                    staged_seen.insert(f.clone());
                    copied += 1;
                    "copied"
                }
            }
        };
        let mut fields = Map::new();
        for (k, f) in &n.fields {
            fields.insert(k.clone(), f.value.clone());
        }
        let mut meta = fields.get("meta").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
        let via = meta.as_object_mut().and_then(|m| m.remove("merged_from"));
        meta["merged_from"] = json!({"merge": merge, "cauldron": s.name, "cauldron_id": source_id, "id": id});
        if let Some(v) = via {
            meta["merged_from"]["via"] = v;
        }
        fields.insert("meta".into(), meta);
        dump_rows.push(json!({"id": id, "state": state, "kind": kind.as_str(), "bytes": blob.get("bytes"), "blob": how}));
        carries.push(Carry { source_id: id, state, fields, blob: if state_has_blob(&n.state) { file } else { None } });
    }

    // ---- the records ----
    // A viewer for a role the target already has is not carried (one viewer
    // per role), and so nothing may point at it.
    let left: BTreeSet<String> = nodes
        .iter()
        .filter(|r| r.n.node_type == "viewer" && dst_roles.contains(field(&r.n, "role").and_then(Value::as_str).unwrap_or("")))
        .map(|r| r.n.id.0.clone())
        .collect();
    // Lineage: a source record whose origin the target already holds (it
    // came here before, by this source or by another route) is not carried
    // again; what points at it points at the target's copy.
    let mut existing: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut already_here: Vec<Value> = Vec::new();
    for r in &nodes {
        if left.contains(&r.n.id.0) {
            continue;
        }
        let (_, via) = written_by(r, &rows, &src_merges);
        let key = via.as_ref().and_then(origin).unwrap_or_else(|| (source_id.clone(), r.n.id.0.clone()));
        if let Some((tid, pool, how)) = here.get(&key) {
            existing.insert(r.n.id.0.clone(), (tid.clone(), pool.clone()));
            already_here.push(json!({
                "id": r.n.id.0, "type": r.n.node_type, "as": tid,
                "via": if how == "the target's own" { format!("{tid} is the target's own record (the source holds a copy of it)") } else { format!("already here via {how}") },
                "origin": {"cauldron_id": key.0, "id": key.1},
            }));
        }
    }
    let src_ids: BTreeSet<String> =
        nodes.iter().map(|r| r.n.id.0.clone()).filter(|i| !left.contains(i) && !existing.contains_key(i)).collect();
    let existing_ids: BTreeMap<String, String> = existing.iter().map(|(k, (t, _))| (k.clone(), t.clone())).collect();
    // A region already in the target that a carried record points into: a
    // live region lock on it (a ripple run writing there) would refuse the
    // merge's write after the snapshot and the hold, so it blocks here,
    // before anything is held.
    {
        let pointed: BTreeSet<&str> = nodes
            .iter()
            .filter(|r| src_ids.contains(&r.n.id.0))
            .flat_map(|r| r.edges.iter().map(|e| e.dst.id.0.as_str()))
            .collect();
        let into: Vec<(String, String)> = nodes
            .iter()
            .filter(|r| r.n.node_type == "region" && pointed.contains(r.n.id.0.as_str()))
            .filter_map(|r| existing.get(&r.n.id.0).cloned())
            .collect();
        if !into.is_empty() {
            let table = crate::region_lock::load(d)?;
            let now = env::region_locks::now_ms();
            let wanted: Vec<env::region_locks::Want> = into.iter().map(|(id, pool)| (pool.clone(), id.clone(), env::region_locks::WRITE.to_string())).collect();
            let held = crate::region_lock::conflicts(d, &table, now, None, &wanted)?;
            if !held.is_empty() {
                blocking.push(json!({
                    "what": format!("region(s) {} of the target", held.iter().map(|r| r.region.clone()).collect::<Vec<_>>().join(", ")),
                    "why": format!("records of the source go into region(s) the target already holds, and a write is in progress there: {}", env::region_locks::refusal_text(&held, now)),
                    "fix": format!("wait for that ripple run to finish (`cauldron ripples status --env {}`), then merge", dst.env.name),
                }));
            }
        }
    }
    let mut edges_from_present: Vec<Value> = Vec::new();
    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    let mut records: Vec<Value> = Vec::new();
    let mut counts: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut edge_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut across = 0usize;
    let mut edges_not_carried: Vec<Value> = Vec::new();
    let mut collisions: Vec<Value> = Vec::new();
    let mut regions: Vec<Value> = Vec::new();
    let mut flagged_regions: Vec<Value> = Vec::new();
    let mut flagged_clusters: Vec<Value> = Vec::new();
    let (mut unreviewed, mut approved, mut rejected, mut not_ripple) = (0usize, 0usize, 0usize, 0usize);
    let mut flags_on_not_ripple = 0usize;
    // The trust rule: which decisions are carried, and which are not, why.
    let mut ripple_part: BTreeSet<String> = BTreeSet::new();
    let mut ripple_scope: BTreeSet<String> = BTreeSet::new();
    let mut carried_decisions: Vec<Value> = Vec::new();
    let mut uncarried_decisions: Vec<Value> = Vec::new();
    let mut claims_dropped: Vec<Value> = Vec::new();
    let mut sealed: Vec<String> = Vec::new();
    let mut viewers_left: Vec<Value> = Vec::new();
    let mut from_target = 0usize;
    let target_id = d.id();
    for r in &nodes {
        let id = r.n.id.0.clone();
        let ty = r.n.node_type.clone();
        let text = |k: &str| field(&r.n, k).and_then(Value::as_str).unwrap_or("").to_string();
        if left.contains(&id) {
            viewers_left.push(json!({"id": id, "role": text("role"), "why": "the target has its own viewer for this role, and a cauldron has one viewer per role; the source's viewer and its `shows` edges are not carried"}));
            continue;
        }
        if existing.contains_key(&id) {
            // Already here: not carried. An edge it gained since, toward a
            // record carried now, is listed (it would be a change to the
            // target's copy, which a merge does not make).
            for e in &r.edges {
                if src_ids.contains(&e.dst.id.0) {
                    edges_from_present.push(json!({"from": id, "as": existing_ids[&id], "kind": e.kind, "to": e.dst.id.0}));
                }
            }
            continue;
        }
        if r.n.sealed {
            sealed.push(id.clone());
        }
        labels.insert(id.clone(), label(&id));
        *counts.entry(r.pool.clone()).or_default().entry(ty.clone()).or_default() += 1;
        if let Some((pool, t)) = dst_ids.get(&id) {
            collisions.push(json!({"id": id, "source_type": ty, "target_pool": pool, "target_type": t}));
        }
        let grammar = crate::clusters::grammar_of_type(&ty);
        let has_meta = grammar.as_ref().and_then(|g| g.node_types.get(&ty)).is_some_and(|t| t.fields.contains_key("meta"));
        let mut rec = Map::new();
        rec.insert("id".into(), json!(label(&id)));
        rec.insert("type".into(), json!(ty));
        if let Some(st) = &r.n.state {
            rec.insert("state".into(), json!(st));
        }
        for (k, f) in &r.n.fields {
            if k == "meta" {
                continue;
            }
            let v = match (k.as_str(), &f.value) {
                ("body", Value::String(b)) => json!(relabel_links(b, &src_ids, &existing_ids)),
                _ => f.value.clone(),
            };
            rec.insert(k.clone(), v);
        }
        // Review state and provenance.
        let (wb, via) = written_by(r, &rows, &src_merges);
        let mut meta_in = field(&r.n, "meta").cloned().unwrap_or(Value::Null);
        let is_ripple = wb["class"] == "ripple";
        // A `merged_from` on a record no merge created claims a provenance
        // nothing vouches for: it is not carried.
        if via.is_none() && meta_in.get("merged_from").is_some() {
            claims_dropped.push(json!({"id": id, "why": "it claims `meta.merged_from`, but the transaction that created it in the source is not a merge, so no merge vouches for that provenance; it arrives as written in the source"}));
        }
        // Each decision is carried only if the source's history vouches for
        // it (`vouch`); otherwise the record arrives without it, saying so.
        let mut decision: Option<Value> = None;
        let mut not_carried_decision: Option<Value> = None;
        for flag in ["rejected", "approved"] {
            let Some(v) = meta_in.get(flag).cloned().filter(|v| truthy(Some(v))) else { continue };
            if decision.is_some() {
                // Both flags set: only the one already carried counts.
                continue;
            }
            match vouch(&writes, &src_merges, &r.pool, &id, flag, &v, &meta_in) {
                Ok(d) => {
                    carried_decisions.push(json!({"id": id, "flag": flag, "by": d["by"], "at": d["at"]}));
                    decision = Some(d);
                }
                Err(why) => {
                    uncarried_decisions.push(json!({"id": id, "claimed": flag, "why": why}));
                    not_carried_decision = Some(json!({"claimed": flag, "why": why}));
                    if let Some(m) = meta_in.as_object_mut() {
                        m.remove(flag);
                    }
                }
            }
        }
        if let (Some(d), Some(m)) = (&decision, meta_in.as_object_mut()) {
            // The other flag, unvouched or not, does not travel beside it.
            let other = if d["flag"] == "approved" { "rejected" } else { "approved" };
            if m.remove(other).is_some_and(|v| truthy(Some(&v))) && not_carried_decision.is_none() {
                not_carried_decision = Some(json!({"claimed": other, "why": format!("the record's `{}` decision is the one carried", d["flag"].as_str().unwrap_or(""))}));
            }
        }
        let was_approved = truthy(meta_in.get("approved"));
        let was_rejected = truthy(meta_in.get("rejected"));
        let review = match (is_ripple, was_approved, was_rejected) {
            (true, _, true) => {
                rejected += 1;
                "rejected"
            }
            (true, true, false) => {
                approved += 1;
                "approved"
            }
            (true, false, false) => {
                unreviewed += 1;
                "unreviewed"
            }
            (false, a, j) => {
                not_ripple += 1;
                if a || j {
                    flags_on_not_ripple += 1;
                }
                "none"
            }
        };
        if via.as_ref().is_some_and(|v| v["cauldron_id"].as_str().is_some() && v["cauldron_id"].as_str() == target_id.as_deref()) {
            from_target += 1;
        }
        // A ripple's record nobody decided lands in the merge's ripple part
        // (a transaction of the ripple class), so every reader, an older
        // one too, reads it as unreviewed. A flag such a record carries
        // (`meta.stop` and the like) is named in that part's scope.
        if review == "unreviewed" && has_meta {
            ripple_part.insert(label(&id));
            for f in spine::flags::FLAG_FIELDS {
                if f.path.len() == 2 && f.path[0] == "meta" && truthy(meta_in.get(f.path[1])) {
                    ripple_scope.insert(f.name.to_string());
                }
            }
        }
        if has_meta {
            let mut meta = meta_in.clone();
            if !meta.is_object() {
                meta = json!({});
            }
            let m = meta.as_object_mut().expect("an object");
            m.remove("merged_from");
            let stamp = json!({"merge": merge, "cauldron_id": source_id, "id": id});
            for flag in ["approved", "rejected"] {
                if let Some(v) = m.get_mut(flag).filter(|v| truthy(Some(v))) {
                    if let Some(o) = v.as_object_mut() {
                        o.insert("carried_by_merge".into(), stamp.clone());
                    } else {
                        *v = json!({"was": v.clone(), "carried_by_merge": stamp.clone()});
                    }
                }
            }
            // A cite of a dump names the dump's id in the target (filled in
            // when the ids are predicted); one the source does not hold
            // blocks the merge, since it would name another dump here.
            if let Some(cites) = m.get("cites").and_then(Value::as_array) {
                for c in cites {
                    if c["kind"] == "dump" {
                        let did = c["id"].as_str().unwrap_or("");
                        if !dump_ids.contains(did) && !existing_dumps.contains_key(did) {
                            blocking.push(json!({
                                "what": format!("record {id} of the source"),
                                "why": format!("it cites dump `{did}`, which the source's dumps store does not hold, so the cite would name an unrelated dump of the target"),
                                "fix": "correct the cite in the source (supersede the record with one that cites what it came from), then merge",
                            }));
                        }
                    }
                }
            }
            let mut from = json!({"merge": merge, "cauldron": s.name, "cauldron_id": source_id, "id": id, "written_by": wb, "review": review});
            if let Some(d) = &decision {
                from["decision"] = d.clone();
            }
            if let Some(n) = &not_carried_decision {
                from["decision_not_carried"] = n.clone();
            }
            if let Some(v) = via {
                from["via"] = v;
            }
            m.insert("merged_from".into(), from);
            rec.insert("meta".into(), meta);
        } else if was_approved || was_rejected || is_ripple {
            notes.push(json!(format!("{id} ({ty}) has no `meta`, so it carries no provenance")));
        }
        // Edges.
        let mut edges = Vec::new();
        for e in &r.edges {
            let (dp, did) = (e.dst.pool.as_str().to_string(), e.dst.id.0.clone());
            let in_batch = carried_pools.contains(&dp.as_str()) && src_ids.contains(&did);
            if let Some((tid, tpool)) = existing.get(&did).filter(|_| carried_pools.contains(&dp.as_str())) {
                // Toward a record already here: the target's copy.
                *edge_counts.entry(e.kind.clone()).or_default() += 1;
                let mut x = Map::new();
                x.insert("kind".into(), json!(e.kind));
                x.insert("to".into(), json!(format!("{tid}@{tpool}")));
                if let Some(note) = &e.note {
                    x.insert("note".into(), json!(note));
                }
                for (k, v) in &e.fields {
                    x.insert(k.clone(), v.clone());
                }
                edges.push(Value::Object(x));
                continue;
            }
            if !in_batch {
                edges_not_carried.push(json!({"from": id, "kind": e.kind, "to": crate::reads::target_text(&PoolName::new(&r.pool), &e.dst), "why": "it points outside what a merge carries (another pool, or a node not carried)"}));
                continue;
            }
            *edge_counts.entry(e.kind.clone()).or_default() += 1;
            if dp != r.pool {
                across += 1;
            }
            let mut x = Map::new();
            x.insert("kind".into(), json!(e.kind));
            x.insert("to".into(), json!(label(&did)));
            if let Some(note) = &e.note {
                x.insert("note".into(), json!(note));
            }
            for (k, v) in &e.fields {
                x.insert(k.clone(), v.clone());
            }
            edges.push(Value::Object(x));
        }
        if !edges.is_empty() {
            rec.insert("edges".into(), Value::Array(edges));
        }
        // Regions and clusters, and their look-alikes in the target.
        if ty == "region" || ty == "cluster" {
            let (h, b) = (text("handle"), text("boundary"));
            let narrows: Vec<String> = r.edges.iter().filter(|e| e.kind == "narrows").map(|e| e.dst.id.0.clone()).collect();
            if ty == "region" {
                regions.push(json!({"id": id, "handle": h, "boundary": b, "narrows": narrows}));
            }
            let pool = if ty == "region" { &dst_regions } else { &dst_clusters };
            for (tid, th, tb) in pool.iter() {
                let why = if norm_text(th) == norm_text(&h) {
                    "the same handle"
                } else if !tb.is_empty() && norm_text(tb) == norm_text(&b) {
                    "the same boundary"
                } else {
                    continue;
                };
                let f = json!({
                    "source": id, "target": tid, "handle": h, "why": format!("{why} as the target's {tid}"),
                    "kept": "both: the source's is carried as a region of its own, nothing is folded together",
                });
                if ty == "region" { flagged_regions.push(f) } else { flagged_clusters.push(f) }
            }
        }
        records.push(Value::Object(rec));
    }
    // The main write's records first, then the ripple part's: the ids the
    // whole batch is judged to get are then the ids the two writes give.
    records.sort_by_key(|r| r["id"].as_str().is_some_and(|l| ripple_part.contains(l)));
    let new_records = records.len();
    let new_dumps = carries.len();
    // A source with nothing in it: refused, as one with nothing new is (a
    // merge would only take a snapshot and record that it carried nothing).
    if new_records == 0 && new_dumps == 0 && already_here.is_empty() && dumps_already.is_empty() {
        code = Code::AlreadyMerged;
        blocking.push(json!({
            "what": "nothing to merge",
            "why": format!("cauldron `{}` (id {source_id}) holds no record and no dump, so this merge would carry nothing", s.name),
            "fix": "nothing to do; merge it once it holds something",
        }));
    }
    if !already_here.is_empty() || !dumps_already.is_empty() {
        if new_records == 0 && new_dumps == 0 {
            code = Code::AlreadyMerged;
            let vias: BTreeSet<String> = already_here.iter().filter_map(|a| a["via"].as_str().map(str::to_string)).collect();
            blocking.push(json!({
                "what": "nothing new",
                "why": format!(
                    "everything cauldron `{}` (id {source_id}) holds is already in `{}` ({} record(s) and {} dump(s), each found by where it came from: {}), so this merge would carry nothing",
                    s.name, d.name, already_here.len(), dumps_already.len(), vias.into_iter().collect::<Vec<_>>().join("; ")
                ),
                "fix": "nothing to do: the source's records are here. A record added to the source later is carried by merging it again then",
            }));
        } else {
            notes.push(json!(format!(
                "{} record(s) and {} dump(s) of the source are already in the target (found by where they came from, listed under `already_here`) and are not carried again; {} new record(s) and {} new dump(s) are",
                already_here.len(), dumps_already.len(), new_records, new_dumps
            )));
        }
    }
    if !edges_from_present.is_empty() {
        not_carried.push(json!({
            "what": "edges from records already here",
            "edges": edges_from_present,
            "why": "the record they leave is already in the target and a merge does not change a record it does not carry; link the target's copy with `soup_link` if the edge belongs there",
        }));
    }
    if flags_on_not_ripple > 0 {
        notes.push(json!(format!("{flags_on_not_ripple} record(s) not written by a ripple carry a review flag; it is carried as it was")));
    }
    if from_target > 0 {
        notes.push(json!(format!(
            "{from_target} record(s) came into the source from this target by an earlier merge; they are carried again as copies, each with its whole provenance (`via`)"
        )));
    }
    if !sealed.is_empty() {
        not_carried.push(json!({"what": "the seal flag", "ids": sealed, "why": "only the soup's own SEAL statement sets it; the records are carried unsealed (seal them again if they must be)"}));
    }
    if !viewers_left.is_empty() {
        not_carried.push(json!({"what": "viewers", "viewers": viewers_left}));
    }
    if !edges_not_carried.is_empty() {
        not_carried.push(json!({"what": "edges", "edges": edges_not_carried}));
    }
    let needs_terminal = !carried_decisions.is_empty();
    if !uncarried_decisions.is_empty() {
        notes.push(json!(format!(
            "{} decision(s) in the source are NOT carried: the source's own write history does not vouch for them (each with why under `review.decisions.not_carried`); those records arrive without them, a ripple's as unreviewed",
            uncarried_decisions.len()
        )));
    }
    if !claims_dropped.is_empty() {
        not_carried.push(json!({"what": "claimed merge provenance", "records": claims_dropped}));
    }
    let grant = crate::grants::Grants::load(&src.env).ok().and_then(|g| g.get(&s.name).map(|g| g.is_for(s))).unwrap_or(false);
    let auto = env::ripples_auto::load(&src.env.data_root, &s.name).ok().flatten().is_some_and(|x| x.on);
    let dst_auto = env::ripples_auto::load(&dst.env.data_root, &d.name).ok().flatten().is_some_and(|x| x.on);
    not_carried.extend([
        json!({"what": "the source's MCP grant", "source_has_one": grant, "why": "a grant opens one cauldron, by its id; a merge never widens access, so the target keeps its own grants and nothing is granted"}),
        json!({"what": "the source's automatic ripples setting", "source_setting": if auto { "on" } else { "off" }, "why": "it is bound to the source's id; the target keeps its own"}),
        json!({"what": "the source itself", "why": "a merge is a copy: the source is read and left untouched. Removing it is `cauldron remove`, separately"}),
        json!({"what": "the source's write history", "why": "the target's history is its own; who wrote each record and when, and its review state, are kept in its `meta.merged_from`"}),
        json!({"what": "the source's ripple runs, journals and spend", "why": "they are the source's; the target's caps and journal count its own runs"}),
        json!({"what": "a record's `rev`, creation time and author as fields", "why": "they are machine-owned and set by the target's own write; the originals are in `meta.merged_from.written_by`"}),
    ]);
    if by_state.get("received").copied().unwrap_or(0) > 0 {
        notes.push(json!(format!(
            "{} dump(s) are `received` (not processed yet) and are carried so; the target's automatic ripples setting is {}{}",
            by_state["received"],
            if dst_auto { "on" } else { "off" },
            if dst_auto { ": the ripples service will process them in the target" } else { "" }
        )));
    }
    let wants_cluster_pool = records.iter().any(|r| r["type"] == "cluster");
    let needs_dumps = !carries.is_empty();
    let mut counts_v = json!(counts);
    counts_v["edges"] = json!(edge_counts);
    counts_v["edges_across_pools"] = json!(across);
    counts_v["dumps"] = json!(by_state);
    counts_v["blobs"] = json!({"copied": copied, "shared": shared, "none": tombstones});
    let supersede = "To keep one of two look-alikes, supersede the other: from a session, `soup_link {links: [{from: <the one to keep>, kind: \"supersedes\", to: <the other>}]}`, or import a new region with a `supersedes` edge; the superseded one stays readable and leaves the listings";
    let doc = json!({
        "env": dst.env.name,
        "merge": merge,
        "source": {"cauldron": s.name, "cauldron_id": source_id, "untouched": true},
        "target": {"cauldron": d.name, "cauldron_id": target_id},
        "content_digest": digest,
        "counts": counts_v,
        "id_collisions": collisions,
        "ids": "every source id is re-minted in the target; `id_map` is filled in once the target's door has judged the batch",
        "regions": regions,
        "flagged": {"regions": flagged_regions, "clusters": flagged_clusters, "supersede": supersede},
        "already_here": {"records": already_here, "dumps": dumps_already, "how": "a source record (or dump) whose origin, the cauldron it was first written in and its id there (the deepest `meta.merged_from`, or the source itself), the target already holds is not carried again; edges, links and cites to it name the target's copy"},
        "dumps": dump_rows,
        "review": {
            "written_by_a_ripple": {"unreviewed": unreviewed, "approved": approved, "rejected": rejected},
            "not_written_by_a_ripple": not_ripple,
            "carried": "a decision (approved or rejected) travels only when the source's own write history vouches for it: the last write of that flag was a decision there (the owner's command at a terminal, `by: tty`; or, where the owner opened approving to agents, the owner's command without one or a session's soup_approve / soup_reject, `by: agent`), or a merge that carried a vouched one; who decided (`by`), how and when are kept in `meta.merged_from.decision`, with the merge that carried it and who ran that merge (`carried_by`), and the flag is marked `carried_by_merge`. Any other flag is not carried and the record arrives without it (a ripple's record unreviewed), saying why in `meta.merged_from.decision_not_carried`. Unreviewed stays unreviewed",
            "decisions": {
                "will_be_carried": carried_decisions.len(),
                "will_not_be_carried": uncarried_decisions.len(),
                "carried": carried_decisions,
                "not_carried": uncarried_decisions,
            },
            "needs_terminal": needs_terminal,
            "owner_act": if needs_terminal {
                json!("carrying decisions is the owner act `merge`: at an interactive terminal, or without one where the owner opened it (`cauldron owner-acts open --only merge`, or CAULDRON_OWNER_ACTS=merge for the command); recorded as `by: tty` or `by: agent`")
            } else {
                json!("no decision is carried, so this merge is no owner act")
            },
            "older_readers": if ripple_part.is_empty() {
                json!("no record here is a ripple's that nobody decided")
            } else {
                json!(format!(
                    "the {} record(s) a ripple wrote that nobody decided land in a write of the ripple class (run `{}`), so a `cauldron` older than merging (0.1.17 and before) lists them as unreviewed too, as it lists any ripple's record",
                    ripple_part.len(),
                    provenance::ripple_run(&merge)
                ))
            },
        },
        "not_carried": not_carried,
        "blocking": blocking,
        "notes": notes,
    });
    let source = json!({"cauldron": s.name, "cauldron_id": source_id, "env": src.env.name, "digest": digest});
    Ok(Prepared {
        blocked: !doc["blocking"].as_array().is_none_or(Vec::is_empty),
        doc,
        needs_terminal,
        needs_dumps,
        code,
        merge,
        records,
        carries,
        labels,
        existing: existing_ids,
        ripple_part,
        ripple_scope: ripple_scope.into_iter().collect(),
        existing_dumps,
        source_dumps_dir: src_dumps_dir,
        wants_cluster_pool,
        source,
        gate: Gate { root: dst.env.root.clone(), env: dst.env.name.clone(), cauldron: d.name.clone(), id: target_id.clone() },
    })
}

fn state_has_blob(state: &Option<String>) -> bool {
    state.as_deref() != Some("removed")
}

/// SHA-256 over the source's records and dumps as read (ids, types, states,
/// fields, edges, seal), each object's keys sorted.
fn content_digest(nodes: &[Read], dumps: &[NodeRecord]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for r in nodes {
        let fields: BTreeMap<&String, &Value> = r.n.fields.iter().map(|(k, f)| (k, &f.value)).collect();
        let edges: Vec<Value> = r.edges.iter().map(|e| json!([e.kind, e.dst.pool.as_str(), e.dst.id.0, e.note, e.fields])).collect();
        h.update(serde_json::to_string(&json!([r.pool, r.n.id.0, r.n.node_type, r.n.state, r.n.sealed, fields, edges])).unwrap_or_default());
        h.update([0u8]);
    }
    h.update([1u8]);
    let mut ds: Vec<&NodeRecord> = dumps.iter().collect();
    ds.sort_by_key(|n| crate::reads::serial(&n.id.0));
    for n in ds {
        let fields: BTreeMap<&String, &Value> = n.fields.iter().map(|(k, f)| (k, &f.value)).collect();
        h.update(serde_json::to_string(&json!([n.id.0, n.state, fields])).unwrap_or_default());
        h.update([0u8]);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

// ------------------------------------------------------------ predicting

/// The dumps id the target's next dump will get, read through a connection
/// that cannot write (`DM-1` for a cauldron with no store).
fn next_dump_offline(c: &env::Cauldron) -> Result<u64> {
    if !dumps::exists(c) {
        return Ok(1);
    }
    let conn = spine::offline::read_only_connection(&dumps::dir(c).join(spine::spine_db::SPINE_FILE)).map_err(store)?;
    let has: i64 = conn
        .query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'id_serials'", [], |r| r.get(0))
        .map_err(store)?;
    if has == 0 {
        return Ok(1);
    }
    let last: Option<i64> = conn
        .query_row("SELECT last FROM id_serials WHERE pool = ?1 AND prefix = ?2", [spine::grammar_dump::POOL, spine::grammar_dump::DUMP_PREFIX], |r| r.get(0))
        .ok();
    Ok(last.unwrap_or(0).max(0) as u64 + 1)
}

/// What the ids come to: the batch as it will land, and the maps.
struct Predicted {
    records: Vec<Value>,
    /// Source id -> target id, per pool.
    id_map: BTreeMap<String, String>,
    dump_map: BTreeMap<String, String>,
    carries: Vec<Carry>,
    retokened: usize,
    refs_dropped: Vec<Value>,
}

fn judge(handle: &spine::Cauldron, records: &[Value]) -> Result<std::result::Result<BTreeMap<String, String>, Value>> {
    let plan = MigrationPlan::new(Initiator::Agent { thread: "cauldron-merge".into(), dispatch: "cauldron merge (judging)".into() });
    let opts = RecordsOptions { pool: PoolName::new(DEFAULT_POOL), routes: crate::clusters::routes(), remint: true, commit: false };
    let run = import_records(handle, &plan, records, &opts).map_err(crate::region_lock::spine_error)?;
    if run.all_accepted() {
        Ok(Ok(run.id_map))
    } else {
        let refused: Vec<Value> = run
            .records
            .iter()
            .filter(|r| r.disposition != "accepted")
            .map(|r| serde_json::to_value(r).unwrap_or(Value::Null))
            .collect();
        Ok(Err(json!(refused)))
    }
}

/// **The ids, predicted** on `handle` (the target's own, under its writer
/// lock, or a snapshot copy's), with the first dump id `first_dump`: the
/// records judged by the target's door, their cites and boundaries
/// rewritten, judged again.
fn predict(p: &Prepared, mut records: Vec<Value>, handle: &spine::Cauldron, first_dump: u64) -> Result<Predicted> {
    let mut dump_map = BTreeMap::new();
    for (i, c) in p.carries.iter().enumerate() {
        dump_map.insert(c.source_id.clone(), format!("{}-{}", spine::grammar_dump::DUMP_PREFIX, first_dump + i as u64));
    }
    for r in records.iter_mut() {
        if let Some(cites) = r.get_mut("meta").and_then(|m| m.get_mut("cites")).and_then(Value::as_array_mut) {
            for c in cites.iter_mut() {
                if c["kind"] == "dump" {
                    // A carried dump's predicted id, or the target's own copy.
                    if let Some(new) = c["id"].as_str().and_then(|i| dump_map.get(i).or_else(|| p.existing_dumps.get(i))) {
                        c["id"] = json!(new);
                    }
                }
            }
        }
    }
    let by_label = match judge(handle, &records)? {
        Ok(m) => m,
        Err(refused) => {
            return Err(Error::new(
                Code::MergeBlocked,
                format!(
                    "REFUSED: the target's door refused {} of the source's records as they would land (each with its reasons \
                     in `detail.records`). Nothing was written.",
                    refused.as_array().map_or(0, Vec::len)
                ),
            )
            .with_detail(json!({"records": refused})))
        }
    };
    let id_map: BTreeMap<String, String> =
        p.labels.iter().filter_map(|(src, lab)| by_label.get(lab).map(|n| (src.clone(), n.clone()))).collect();
    // The ids a boundary or an edge's note names, as whole tokens: a carried
    // record's new id, or the target's copy of one already here.
    let mut names = id_map.clone();
    names.extend(p.existing.iter().map(|(k, v)| (k.clone(), v.clone())));
    let mut retokened = 0;
    for r in records.iter_mut() {
        if let Some(b) = r.get("boundary").and_then(Value::as_str).map(str::to_string) {
            let (t, n) = retoken(&b, &names);
            retokened += n;
            r["boundary"] = json!(t);
        }
        if let Some(es) = r.get_mut("edges").and_then(Value::as_array_mut) {
            for e in es.iter_mut() {
                if let Some(note) = e.get("note").and_then(Value::as_str).map(str::to_string) {
                    let (t, n) = retoken(&note, &names);
                    retokened += n;
                    e["note"] = json!(t);
                }
            }
        }
    }
    if retokened > 0 {
        match judge(handle, &records)? {
            Ok(again) if again == by_label => {}
            Ok(_) => return Err(store("the ids predicted for the merge moved between two judgements of the same batch")),
            Err(refused) => {
                return Err(Error::new(
                    Code::MergeBlocked,
                    "REFUSED: with the ids its boundaries and notes name rewritten to the target's, some of the source's records \
                     no longer fit (each with its reasons in `detail.records`). Nothing was written.",
                )
                .with_detail(json!({"records": refused})))
            }
        }
    }
    // A dump's `refs` name records: rewritten, and a ref to no record of the
    // source is dropped (it would name another record here) and listed.
    let mut carries = p.carries.clone();
    let mut refs_dropped = Vec::new();
    for c in carries.iter_mut() {
        if let Some(refs) = c.fields.get_mut("meta").and_then(|m| m.get_mut("refs")).and_then(Value::as_array_mut) {
            let mut kept = Vec::new();
            for r in refs.iter() {
                match r.as_str().and_then(|x| names.get(x)) {
                    Some(n) => kept.push(json!(n)),
                    None => refs_dropped.push(json!({"dump": c.source_id, "ref": r})),
                }
            }
            *refs = kept;
        }
    }
    Ok(Predicted { records, id_map, dump_map, carries, retokened, refs_dropped })
}

fn fill_ids(doc: &mut Value, pr: &Predicted) {
    doc["id_map"] = json!(pr.id_map);
    doc["dump_map"] = json!(pr.dump_map);
    doc["ids"] = json!("every source id is re-minted in the target: `id_map` (records) and `dump_map` (dumps), source id to target id");
    if let Some(regions) = doc["regions"].as_array_mut() {
        for r in regions.iter_mut() {
            if let Some(n) = r["id"].as_str().and_then(|i| pr.id_map.get(i)) {
                r["new_id"] = json!(n);
            }
            if let Some(ns) = r["narrows"].as_array_mut() {
                for x in ns.iter_mut() {
                    if let Some(n) = x.as_str().and_then(|i| pr.id_map.get(i)) {
                        *x = json!(n);
                    }
                }
            }
        }
    }
    for k in ["regions", "clusters"] {
        if let Some(fl) = doc["flagged"][k].as_array_mut() {
            for f in fl.iter_mut() {
                if let Some(n) = f["source"].as_str().and_then(|i| pr.id_map.get(i)) {
                    f["new_id"] = json!(n);
                }
            }
        }
    }
    if let Some(ds) = doc["dumps"].as_array_mut() {
        for d in ds.iter_mut() {
            if let Some(n) = d["id"].as_str().and_then(|i| pr.dump_map.get(i)) {
                d["new_id"] = json!(n);
            }
        }
    }
    if pr.retokened > 0 {
        doc["notes"].as_array_mut().map(|n| n.push(json!(format!("{} id(s) named in boundaries and edge notes were rewritten to the target's ids", pr.retokened))));
    }
    if !pr.refs_dropped.is_empty() {
        doc["not_carried"].as_array_mut().map(|n| {
            n.push(json!({"what": "dump refs", "refs": pr.refs_dropped, "why": "a dump's `refs` named a record the source does not hold, which would name another record here"}))
        });
    }
}

/// **The dry run**: the plan, with the ids predicted on a snapshot copy of
/// the target (given the cluster pool when the batch needs one), nothing
/// held and nothing written.
pub fn dry_run(src: &Target, dst: &Target) -> Result<Value> {
    let mut p = prepare(src, dst)?;
    let records = std::mem::take(&mut p.records);
    let mut doc = p.doc.clone();
    doc["dry_run"] = json!(true);
    doc["judged_on"] = json!("a read-only snapshot of the target, taken now; nothing was written to either cauldron");
    if !p.blocked {
        let scratch = Scratch::new()?;
        spine::offline::snapshot_copy(&dst.cauldron.soup(), &scratch.0).map_err(|e| store(format!("taking a snapshot of the target: {e}")))?;
        let mut h = spine::Cauldron::open(&scratch.0, spine::config::SpineConfig::default()).map_err(|e| store(format!("opening the snapshot: {e}")))?;
        if p.wants_cluster_pool {
            h.add_pool(&spine::grammar_cauldron::cauldron_v1(crate::clusters::POOL)).map_err(store)?;
        }
        match predict(&p, records, &h, next_dump_offline(&dst.cauldron)?) {
            Ok(pr) => fill_ids(&mut doc, &pr),
            Err(e) => {
                doc["blocking"].as_array_mut().map(|b| b.push(json!({"what": "the target's door", "why": e.message, "detail": e.detail, "fix": "correct those records in the source"})));
            }
        }
    }
    doc["outcome"] = json!(if doc["blocking"].as_array().is_none_or(Vec::is_empty) { "validated" } else { "blocked" });
    Ok(doc)
}

/// A private temporary directory, removed with everything in it on drop.
/// **A private copy of the target's soup to judge a batch on** (a dry run,
/// or a merge that needs the cluster pool the target does not have yet): a
/// `soup/` directory inside one read's private directory
/// (`spine::offline::PrivateDir`: a new, unguessable name in the temporary
/// directory, 0700, removed when dropped, and swept by the next read of any
/// process if this one is killed).
struct Scratch(PathBuf, #[allow(dead_code)] spine::offline::PrivateDir);
impl Scratch {
    fn new() -> Result<Scratch> {
        let d = spine::offline::PrivateDir::new()
            .ok_or_else(|| Error::new(Code::Store, "no private directory could be made in the temporary directory to judge the merge on a copy of the target"))?;
        let p = d.path().join("soup");
        env::private::make_dir(&p).map_err(|err| Error::caused(Code::Store, format!("creating {}", p.display()), &err))?;
        Ok(Scratch(p, d))
    }
}

// ------------------------------------------------------------------ files

/// Write `v` to `path` whole or not at all: a temporary file beside it,
/// synced, renamed over it, and the directory synced.
fn write_atomic(path: &Path, v: &Value) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let text = serde_json::to_string_pretty(v).map_err(store)?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(env::private::FILE_MODE)
        .open(&tmp)
        .map_err(|e| store(format!("writing {}: {e}", tmp.display())))?;
    f.write_all(text.as_bytes()).and_then(|_| f.sync_all()).map_err(|e| store(format!("writing {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path).map_err(|e| store(format!("renaming {} to {}: {e}", tmp.display(), path.display())))?;
    sync_dir(path.parent().unwrap_or(Path::new(".")));
    Ok(())
}

fn sync_dir(d: &Path) {
    if let Ok(f) = std::fs::File::open(d) {
        let _ = f.sync_all();
    }
}

/// Copy `from` to `to` (in a directory that exists) through a temporary
/// file, synced and renamed into place.
fn copy_into(from: &Path, to: &Path, tmp_prefix: &str) -> Result<()> {
    let parent = to.parent().unwrap_or(Path::new("."));
    let tmp = parent.join(format!("{tmp_prefix}{}-{}", std::process::id(), to.file_name().and_then(|n| n.to_str()).unwrap_or("blob")));
    std::fs::copy(from, &tmp).map_err(|e| store(format!("copying {} to {}: {e}", from.display(), tmp.display())))?;
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(env::private::FILE_MODE));
        let f = std::fs::File::open(&tmp).map_err(store)?;
        f.sync_all().map_err(store)?;
    }
    std::fs::rename(&tmp, to).map_err(|e| store(format!("renaming {} to {}: {e}", tmp.display(), to.display())))?;
    sync_dir(parent);
    Ok(())
}

// ------------------------------------------------------------------ apply

/// **Land a planned merge** into `dst` through its soup's writer `handle`
/// (whose holder holds the soup's writer lock: the command line, or the
/// `mcp` service's writer) and, when it carries dumps, the target's dumps
/// store `dumps`. The caller holds the source ([`hold_source`]) for as long
/// as this runs, and checked the terminal when [`Prepared::needs_terminal`].
/// Answers the merge's JSON.
/// **Why a planned merge may not land**, if it may not: something blocks it
/// (each reason in the plan's `blocking`), or it carries a review decision
/// and the owner is not at a terminal. The error carries the plan.
pub fn refusal_of(p: &Prepared, o: &Opts) -> Option<Error> {
    merger(p, o).err()
}

/// **Who merges, or why not**: a plan something blocks is refused (each
/// reason in its `blocking`); a merge that carries a decision asks the owner
/// acts' one gate (`env::owner_acts::decide`, act `merge`): at a terminal
/// always (`by: "tty"`), without one only where the owner opened `merge`
/// (`CAULDRON_OWNER_ACTS` in the command's environment, or the persisted
/// setting; `by: "agent"`). A merge that carries no decision is no owner
/// act and runs from anywhere, recorded as `tty` or `agent` as it ran. The
/// error carries the plan.
pub fn merger(p: &Prepared, o: &Opts) -> Result<Merger> {
    let mut doc = p.doc.clone();
    if p.blocked {
        doc["outcome"] = json!("blocked");
        let why = doc["blocking"].as_array().into_iter().flatten().map(|b| b["why"].as_str().unwrap_or("").to_string()).collect::<Vec<_>>().join("; ");
        return Err(Error::new(p.code, format!("REFUSED: the merge is blocked ({why}). Nothing was written.")).with_detail(doc));
    }
    use env::owner_acts::{self, Act, By};
    if !p.needs_terminal {
        let by = if o.at_terminal { By::Tty } else { By::Agent };
        return Ok(Merger::of(by, "no owner act: the merge carries no decision".into()));
    }
    let g = &p.gate;
    let setting = match owner_acts::load(&g.root) {
        Ok(s) => s,
        // At a terminal the setting is not needed: a terminal may always.
        Err(_) if o.at_terminal => owner_acts::Setting::default(),
        Err(e) => {
            doc["outcome"] = json!("refused");
            return Err(Error::new(Code::NotATerminal, format!("REFUSED: the owner_acts setting could not be read ({e}), so a merge that carries decisions runs only at a terminal. Nothing was written.")).with_detail(doc));
        }
    };
    match owner_acts::decide(Act::Merge, &setting, &g.env, &g.cauldron, g.id.as_deref(), o.at_terminal, o.var.as_deref()) {
        Ok(a) => Ok(Merger::of(a.by(), a.said())),
        Err(r) => {
            doc["outcome"] = json!("refused");
            let bad = r.bad_var.map(|b| format!(" {b}; it opens nothing.")).unwrap_or_default();
            Err(Error::new(
                Code::NotATerminal,
                format!(
                    "REFUSED: this merge carries records whose approval or rejection the source's history vouches for, with \
                     those decisions, and carrying decisions is the owner act `merge`: at an interactive terminal, or without \
                     one only where the owner has opened `merge`. Nothing was written. Ask the user to run, in a terminal on \
                     this machine:\n    {}\n{}{bad}",
                    o.command, r.how_to_allow
                ),
            )
            .with_detail(doc))
        }
    }
}

/// **The target as it was, before the merge**: the complete snapshot
/// ([`crate::snapshot`]) the caller takes with [`snapshot`], under the
/// target's writer locks and before it opens anything of the target.
pub struct Before {
    pub dir: PathBuf,
    pub manifest: Value,
}

/// **Take the snapshot** of the target for the planned merge `p`
/// ([`crate::snapshot::take`]): beside the target's directory, verified,
/// before anything of the merge is opened or written. The caller holds the
/// target's writer locks (its soup's and its dumps store's, the latter from
/// [`crate::dumps::lock_for_merge`]).
pub fn snapshot(p: &Prepared, dst: &Target) -> Result<Before> {
    fault::check()?;
    let at = chrono::Utc::now();
    let dir = crate::snapshot::dir_for(&dst.cauldron, &p.merge, &at);
    let manifest = crate::snapshot::take(&dst.cauldron, &p.merge, &dir)?;
    fault::at("snapshot");
    Ok(Before { dir, manifest })
}

pub fn apply(mut p: Prepared, before: &Before, dst: &Target, handle: &mut spine::Cauldron, mut dumps: Option<&mut Store>, o: &Opts) -> Result<Value> {
    fault::check()?;
    let c = &dst.cauldron;
    let who = merger(&p, o)?;
    refuse_pending(c, "start another merge")?;
    // Each carried decision's note says which merge carried it and who ran
    // that merge (`tty` or `agent`).
    let carried_by = json!({"merge": p.merge, "by": who.by.as_str()});
    for r in p.records.iter_mut() {
        let Some(m) = r.get_mut("meta").and_then(Value::as_object_mut) else { continue };
        let mut carried = false;
        if let Some(d) = m.get_mut("merged_from").and_then(|f| f.get_mut("decision")).and_then(Value::as_object_mut) {
            d.insert("carried_by".into(), carried_by.clone());
            carried = true;
        }
        if carried {
            for flag in ["approved", "rejected"] {
                if let Some(st) = m.get_mut(flag).and_then(|v| v.get_mut("carried_by_merge")).and_then(Value::as_object_mut) {
                    st.insert("by".into(), json!(who.by.as_str()));
                }
            }
        }
    }
    if p.needs_dumps && dumps.is_none() {
        return Err(store("the merge carries dumps and was given no dumps store to write them to"));
    }
    // ---- the ids, judged on the target itself (or a copy given the
    // cluster pool it does not have yet) ----
    let first_dump = match dumps.as_deref() {
        Some(s) => crate::reads::serial(&s.next_id()?),
        None => 1,
    };
    let mut pr = if p.wants_cluster_pool && !crate::clusters::has_pool(c) {
        let scratch = Scratch::new()?;
        spine::offline::snapshot_copy(&c.soup(), &scratch.0).map_err(|e| store(format!("taking a snapshot of the target: {e}")))?;
        let mut h = spine::Cauldron::open(&scratch.0, spine::config::SpineConfig::default()).map_err(store)?;
        h.add_pool(&spine::grammar_cauldron::cauldron_v1(crate::clusters::POOL)).map_err(store)?;
        let records = std::mem::take(&mut p.records);
        predict(&p, records, &h, first_dump)?
    } else {
        let records = std::mem::take(&mut p.records);
        predict(&p, records, handle, first_dump)?
    };
    let mut doc = p.doc.clone();
    fill_ids(&mut doc, &pr);
    let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mdir = dir(c).join(&p.merge);
    env::private::make_dir(&dir(c)).map_err(|e| store(format!("making {}: {e}", dir(c).display())))?;
    env::private::make_dir(&mdir).map_err(|e| store(format!("making {}: {e}", mdir.display())))?;
    // ---- the blobs it will need, staged ----
    let staged = mdir.join(STAGED);
    env::private::make_dir(&staged).map_err(store)?;
    let mut blobs = Vec::new();
    for cr in &pr.carries {
        let Some(f) = &cr.blob else { continue };
        let name = Path::new(f).file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        if blobs.contains(&name) {
            continue;
        }
        let in_target = dumps.as_deref().map(|s| s.blobs_dir().join(&name)).is_some_and(|x| x.is_file());
        if !in_target {
            copy_into(&p.source_dumps_dir.join(f), &staged.join(&name), ".incoming-merge-")?;
        }
        blobs.push(name);
    }
    fault::at("staged");
    // ---- the journal: the commit point ----
    let mut journal = json!({
        "format": FORMAT,
        "merge": p.merge,
        "state": "landing",
        "at": at,
        "by": who.by.as_str(),
        "thread": who.thread,
        "allowed": who.allowed,
        "source": p.source,
        "target": {"cauldron": c.name, "cauldron_id": c.id()},
        // Each dump as the source holds it: its `refs` are source ids, which
        // the landing maps to the ids the records land under.
        "dumps": p.carries.iter().map(|cr| json!({"source_id": cr.source_id, "id": pr.dump_map.get(&cr.source_id), "state": cr.state, "fields": cr.fields, "blob": cr.blob})).collect::<Vec<_>>(),
        "existing": p.existing,
        "id_map": pr.id_map,
        "labels": p.labels,
        "dump_map": pr.dump_map,
        "wants_cluster_pool": p.wants_cluster_pool,
        "snapshot": {"dir": before.dir.display().to_string(), "count": before.manifest["count"], "counters": before.manifest["counters"]},
        "hold": hold_reason(dst, &p.merge),
        "dumps_held": dumps.is_some(),
    });
    // Moved in, not copied: the batch is the largest thing a merge holds.
    // Split into its writes: the main one, the ripple part, and the links
    // from the first's records to the second's.
    let (main, ripple, links) = split_parts(std::mem::take(&mut pr.records), &p.ripple_part);
    journal["records"] = Value::Array(main);
    journal["ripple_records"] = Value::Array(ripple);
    journal["links"] = Value::Array(links);
    journal["ripple_scope"] = json!(p.ripple_scope);
    journal["plan"] = doc;
    let merge_id = p.merge.clone();
    drop(pr);
    drop(p);
    write_atomic(&mdir.join(JOURNAL), &journal)?;
    fault::at("journal");
    // ---- THE HOLD: from here until the merge's last soup commit, every
    // other writer of the target's two stores, of any version, is refused
    // (the spine's durable halted state, which every release honours; this
    // merge's handles own its reason and write through it) ----
    let reason = hold_reason(dst, &merge_id);
    if let Some(st) = dumps.as_deref_mut() {
        st.hold(&reason)?;
    }
    handle.hold(&reason).map_err(|e| store(format!("holding cauldron `{}`'s soup for the merge: {e}", c.name)))?;
    fault::at("held");
    fault::delay();
    roll_forward(dst, handle, dumps.as_deref_mut(), journal)
}

/// **The batch, split into the merge's writes**: the records of the ripple
/// part (`ripple_part`, labels), the rest, and, taken off the rest, each
/// edge from one of the rest to a record of the ripple part (it lands
/// after both, as a link: `{from, from_type, kind, to, to_type, note}`).
fn split_parts(records: Vec<Value>, ripple_part: &BTreeSet<String>) -> (Vec<Value>, Vec<Value>, Vec<Value>) {
    let type_of: BTreeMap<String, String> = records
        .iter()
        .filter_map(|r| Some((r["id"].as_str()?.to_string(), r["type"].as_str()?.to_string())))
        .collect();
    let (mut main, mut ripple, mut links) = (Vec::new(), Vec::new(), Vec::new());
    for mut r in records {
        let lab = r["id"].as_str().unwrap_or("").to_string();
        if ripple_part.contains(&lab) {
            ripple.push(r);
            continue;
        }
        if let Some(es) = r.get_mut("edges").and_then(Value::as_array_mut) {
            es.retain(|e| {
                let to = e["to"].as_str().unwrap_or("");
                if !ripple_part.contains(to) {
                    return true;
                }
                // A declared edge field rides as the note's object, as an
                // edge clause carries it.
                let mut note = e.get("note").cloned().unwrap_or(Value::Null);
                if note.is_null() {
                    let fields: Map<String, Value> = e.as_object().into_iter().flatten().filter(|(k, _)| !["kind", "to", "note"].contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect();
                    if !fields.is_empty() {
                        note = Value::Object(fields);
                    }
                }
                links.push(json!({
                    "from": lab.strip_prefix(LABEL).unwrap_or(&lab),
                    "from_type": type_of.get(&lab),
                    "kind": e["kind"],
                    "to": to.strip_prefix(LABEL).unwrap_or(to),
                    "to_type": type_of.get(to),
                    "note": note,
                }));
                false
            });
            if es.is_empty() {
                if let Some(o) = r.as_object_mut() {
                    o.remove("edges");
                }
            }
        }
        main.push(r);
    }
    (main, ripple, links)
}

/// **The ripple part of a merge, landed**: its records (a ripple's that
/// nobody decided) as ONE statement under a ripple initiator. A ripple's
/// write mints its ids (`CREATE`; the `IMPORT` form that carries ids is not
/// a ripple's), so the ids it will get are read from the soup's serial
/// counters first (nothing else writes meanwhile: the merge holds the soup)
/// for the `[[links]]` between its own records, and checked against what it
/// returns. Each edge names a record of the part (by label), or a node
/// already in the soup as `<id>@<pool>`. Answers the transaction and its
/// sequence number.
fn land_ripple_part(c: &env::Cauldron, handle: &spine::Cauldron, records: &[Value], initiator: Initiator, nonce: &str) -> Result<(String, i64)> {
    use spine::envelope::{Envelope, Instrument, Mode, Provenance, ENVELOPE_FORMAT_VERSION};
    let pool = PoolName::new(DEFAULT_POOL);
    let grammar = handle.grammar(&pool).cloned().ok_or_else(|| store("the soup has no `ws` pool"))?;
    let mut next: BTreeMap<String, u64> = handle.serial_counters(&pool).map_err(store)?.into_iter().map(|(k, v)| (k, v + 1)).collect();
    let mut predicted: BTreeMap<String, String> = BTreeMap::new();
    let mut binding: BTreeMap<String, String> = BTreeMap::new();
    for (i, r) in records.iter().enumerate() {
        let lab = r["id"].as_str().unwrap_or("").to_string();
        let ty = r["type"].as_str().unwrap_or("");
        let prefix = grammar.node_types.get(ty).and_then(|t| t.id_prefix.clone()).ok_or_else(|| store(format!("`{ty}` has no id prefix")))?;
        let n = next.entry(prefix.clone()).or_insert(1);
        predicted.insert(lab.clone(), format!("{prefix}-{n}"));
        *n += 1;
        binding.insert(lab, format!("n{i}"));
    }
    let mut matches = Vec::new();
    let mut expects = Vec::new();
    let mut creates = Vec::new();
    let mut edges = Vec::new();
    let mut params: BTreeMap<String, Value> = BTreeMap::new();
    let mut held: BTreeMap<String, String> = BTreeMap::new();
    let mut snaps: BTreeMap<String, Pool> = BTreeMap::new();
    for (i, r) in records.iter().enumerate() {
        let ty = r["type"].as_str().unwrap_or("");
        let mut fields = Vec::new();
        if let Some(st) = r.get("state").filter(|v| !v.is_null()) {
            fields.push(format!("state: $state_{i}"));
            params.insert(format!("state_{i}"), st.clone());
        }
        for (k, v) in r.as_object().into_iter().flatten() {
            if ["id", "type", "state", "edges"].contains(&k.as_str()) || v.is_null() {
                continue;
            }
            let mut v = v.clone();
            if k == "body" {
                if let Some(b) = v.as_str() {
                    v = json!(relink(b, &predicted));
                }
            }
            fields.push(format!("{k}: ${k}_{i}"));
            params.insert(format!("{k}_{i}"), v);
        }
        creates.push(format!("CREATE (n{i}: {DEFAULT_POOL}/{ty} {{ {} }})", fields.join(", ")));
        for (j, e) in r.get("edges").and_then(Value::as_array).into_iter().flatten().enumerate() {
            let kind = e["kind"].as_str().unwrap_or("");
            let to = e["to"].as_str().unwrap_or("");
            let far = match binding.get(to) {
                Some(b) => b.clone(),
                None => {
                    let (id, at) = to.split_once('@').unwrap_or((to, DEFAULT_POOL));
                    if !held.contains_key(to) {
                        if !snaps.contains_key(at) {
                            let h = Pool::open_read_only(&Pool::path_in(&c.soup(), &PoolName::new(at))).map_err(store)?;
                            snaps.insert(at.to_string(), h);
                        }
                        let snap = snaps[at].snapshot().map_err(store)?;
                        let Some(n) = snap.node(&spine::pool::NodeId(id.to_string())).map_err(store)? else {
                            return Err(store(format!("`{to}` is not a node of the soup")));
                        };
                        let b = format!("e{}", held.len());
                        matches.push(format!("MATCH ({b}: {at}/{} {{ id: $id_{b} }})", n.node_type));
                        expects.push(format!("EXPECT COUNT({b}) = 1"));
                        params.insert(format!("id_{b}"), json!(id));
                        held.insert(to.to_string(), b);
                    }
                    held[to].clone()
                }
            };
            let mut line = format!("CREATE (n{i}) -[{kind}]-> ({far})");
            let mut note = e.get("note").cloned().unwrap_or(Value::Null);
            if note.is_null() {
                let fs: Map<String, Value> = e.as_object().into_iter().flatten().filter(|(k, _)| !["kind", "to", "note"].contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect();
                if !fs.is_empty() {
                    note = Value::Object(fs);
                }
            }
            if !note.is_null() {
                line.push_str(&format!(" NOTE $note_{i}_{j}"));
                params.insert(format!("note_{i}_{j}"), note);
            }
            edges.push(line);
        }
    }
    drop(snaps);
    let ret = format!("RETURN {}", (0..records.len()).map(|i| format!("n{i}.id")).collect::<Vec<_>>().join(", "));
    let env = Envelope {
        statement: matches.into_iter().chain(expects).chain(creates).chain(edges).chain(std::iter::once(ret)).collect::<Vec<_>>().join("\n"),
        bindings: params,
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance { initiator, instrument: Instrument { name: "soup-door".into(), version: env!("CARGO_PKG_VERSION").into() } },
        mode: Mode::Commit,
        idempotency_nonce: Some(nonce.to_string()),
    };
    match handle.submit(env) {
        Ok(spine::Outcome::Committed { tx, seq, returned, .. }) => {
            let row = returned.0.first();
            for (lab, b) in &binding {
                let got = row.and_then(|r| r.get(&format!("{b}.id"))).and_then(Value::as_str).unwrap_or("");
                if predicted.get(lab).map(String::as_str) != Some(got) {
                    // The `[[links]]` between the part's records named the
                    // ids it was going to mint; say so if one moved.
                    return Err(store(format!(
                        "the ripple part's record {lab} landed as {got}, not the {} its links were written with",
                        predicted.get(lab).map(String::as_str).unwrap_or("?")
                    )));
                }
            }
            Ok((tx.to_hex(), seq.0 as i64))
        }
        Ok(_) => Err(store("the merge's ripple part was validated and not committed")),
        Err(e) => Err(crate::region_lock::spine_error(e)),
    }
}

/// **The hold a merge sets on the target's two stores** while it lands: the
/// halted reason every other writer is refused with (an older `cauldron`
/// too, which reads it as the spine's halted state and writes nothing).
fn hold_reason(dst: &Target, merge: &str) -> String {
    let c = &dst.cauldron;
    format!(
        "merge {merge} into cauldron `{}` (id {}) is landing or was cut short: nothing else writes to it until `cauldron soup \
         recover --env {} --cauldron {}` finishes it (run with a `cauldron` that has the `merge` verb; an older one cannot \
         finish it and writes nothing meanwhile)",
        c.name,
        c.id().unwrap_or_else(|| "none".into()),
        dst.env.name,
        c.name
    )
}

/// The records a merge landed, read back from the target itself: every node
/// the merge's soup transaction `tx` created, by the source id its
/// `meta.merged_from` names (a viewer, which has no `meta`, by its role, as
/// the journal's `records` give it).
fn landed_records(c: &env::Cauldron, merge: &str, tx: &str, records: &[Value]) -> Result<BTreeMap<String, String>> {
    landed_by(c, merge, &[tx.to_string()], records)
}

/// [`landed_records`] for every transaction in `txs`.
fn landed_by(c: &env::Cauldron, merge: &str, txs: &[String], records: &[Value]) -> Result<BTreeMap<String, String>> {
    if txs.is_empty() {
        return Ok(BTreeMap::new());
    }
    let roles: BTreeMap<String, String> = records
        .iter()
        .filter(|r| r["type"] == "viewer")
        .filter_map(|r| Some((r["role"].as_str()?.to_string(), r["id"].as_str()?.strip_prefix(LABEL)?.to_string())))
        .collect();
    let soup = c.soup();
    let mut out = BTreeMap::new();
    for p in pools_of(&soup)? {
        let handle = Pool::open_read_only(&Pool::path_in(&soup, &PoolName::new(&p))).map_err(|e| store(format!("reading pool `{p}`: {e}")))?;
        let snap = handle.snapshot().map_err(store)?;
        let types: Vec<String> = handle.grammar().node_types.keys().cloned().collect();
        for ty in &types {
            for n in snap.nodes_of(ty, None).map_err(store)? {
                if !snap.created_tx(&n.id).map_err(store)?.is_some_and(|h| txs.contains(&h.to_hex())) {
                    continue;
                }
                let from = n.fields.get("meta").map(|f| &f.value["merged_from"]);
                if let Some(m) = from.filter(|m| m["merge"] == json!(merge)) {
                    if let Some(src) = m["id"].as_str() {
                        out.insert(src.to_string(), n.id.0.clone());
                    }
                } else if n.node_type == "viewer" {
                    if let Some(src) = n.fields.get("role").and_then(|f| f.value.as_str()).and_then(|r| roles.get(r)) {
                        out.insert(src.clone(), n.id.0.clone());
                    }
                }
            }
        }
    }
    Ok(out)
}

/// The dumps a merge already landed in the target: source id -> id.
fn landed_dumps(c: &env::Cauldron, merge: &str) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for n in dumps::all_nodes(c)? {
        let m = n.fields.get("meta").map(|f| &f.value["merged_from"]);
        if let Some(m) = m.filter(|m| m["merge"] == json!(merge)) {
            if let Some(src) = m["id"].as_str() {
                out.insert(src.to_string(), n.id.0.clone());
            }
        }
    }
    Ok(out)
}

/// **Finish a merge from its journal**: every dump not yet landed lands,
/// then the records if they have not, then the merge record. Each step
/// looks first at what already landed, so running it again after any cut
/// finishes the same merge exactly once; and every id it records is read
/// back from what landed, never taken from the plan: a dump found by the
/// `meta.merged_from` the merge gave it, a record by the soup transaction
/// that created it. The merge's hold ([`hold_reason`]) is owned by
/// `handle` (and `dumps`) throughout, adopted when a cut left it; the
/// records' commit lifts it from the soup in the same transaction, and the
/// dumps store's is let go once the records are in.
pub fn roll_forward(dst: &Target, handle: &mut spine::Cauldron, mut dumps: Option<&mut Store>, mut j: Value) -> Result<Value> {
    let c = &dst.cauldron;
    let merge = j["merge"].as_str().unwrap_or("").to_string();
    let mdir = dir(c).join(&merge);
    let source = j["source"].clone();
    // The thread the merge's writes are recorded under (`owner`, or
    // `owner:agent` for an agent's merge), and who ran it.
    let by = j["thread"].as_str().unwrap_or("owner:agent").to_string();
    let merged_by = j["by"].as_str().unwrap_or("agent").to_string();
    let reason = j["hold"].as_str().unwrap_or("").to_string();
    let dispatch_head = format!("{}{}", provenance::MERGE_DISPATCH, source["cauldron"].as_str().unwrap_or("?"));
    let interrupted = |e: Error| -> Error {
        Error::new(
            e.code,
            format!(
                "{}. Merge {merge} into cauldron `{}` began landing and is not finished; nothing else writes to the cauldron until \
                 `cauldron soup recover --env {} --cauldron {}` finishes it (it lands in full, exactly once, or, if it cannot, puts \
                 the cauldron back as it was before the merge).",
                e.message, c.name, dst.env.name, c.name
            ),
        )
    };
    // The hold, owned (adopted when a cut left it).
    if !reason.is_empty() && handle.held() != Some(reason.as_str()) {
        handle.adopt_hold(&reason).map_err(|e| interrupted(store(e)))?;
    }
    if let Some(st) = dumps.as_deref_mut() {
        if !reason.is_empty() {
            st.adopt_hold(&reason).map_err(interrupted)?;
        }
    }
    // The batch is taken out of the journal, not copied: it is the largest
    // thing a merge holds.
    let mut records = match j["records"].take() {
        Value::Array(a) => a,
        _ => Vec::new(),
    };
    let planned: BTreeMap<String, String> = serde_json::from_value(j["id_map"].clone()).unwrap_or_default();
    let planned_dumps: BTreeMap<String, String> = serde_json::from_value(j["dump_map"].clone()).unwrap_or_default();
    // The merge's soup writes already landed, by part (`main`, `ripple`,
    // `links`).
    let parts_of = |c: &env::Cauldron| -> Result<BTreeMap<String, (String, i64)>> {
        Ok(provenance::merge_txs(&c.soup())?.into_iter().filter(|(_, m)| m.merge == merge).map(|(h, m)| (m.part, (h, m.seq))).collect())
    };
    let landed_parts = parts_of(c).map_err(interrupted)?;
    let mut ripple_records = match j["ripple_records"].take() {
        Value::Array(a) => a,
        _ => Vec::new(),
    };
    let links: Vec<Value> = j["links"].as_array().cloned().unwrap_or_default();
    // What the read-back needs of every record: its label, type and role.
    let all_records: Vec<Value> =
        records.iter().chain(ripple_records.iter()).map(|r| json!({"id": r["id"], "type": r["type"], "role": r["role"]})).collect();
    let already = landed_parts.get("main").cloned();
    // A dump's `refs` name records: the ids they landed under when the
    // records are in, else the ids the plan judged (which hold: nothing else
    // has written the soup since, under the merge's writer lock and then its
    // hold, and they are checked against what lands below).
    let mut ref_map = match &already {
        Some(_) => landed_by(c, &merge, &landed_parts.values().map(|x| x.0.clone()).collect::<Vec<_>>(), &all_records).map_err(interrupted)?,
        None => planned.clone(),
    };
    // A ref to a record already in the target names the target's copy.
    let existing: BTreeMap<String, String> = serde_json::from_value(j["existing"].clone()).unwrap_or_default();
    ref_map.extend(existing);
    // ---- the dumps, one write each ----
    let carries = j["dumps"].as_array().cloned().unwrap_or_default();
    let mut dump_map: BTreeMap<String, String> = BTreeMap::new();
    if !carries.is_empty() {
        let st = dumps.as_deref_mut().ok_or_else(|| interrupted(store("the merge carries dumps and was given no dumps store")))?;
        let landed = landed_dumps(c, &merge).map_err(interrupted)?;
        let dump_by = dumps::By {
            thread: by.clone(),
            dispatch: format!("{dispatch_head} [merge {merge} dump]"),
            instrument: "soup-door".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        };
        let mut first = true;
        for d in &carries {
            let src_id = d["source_id"].as_str().unwrap_or("").to_string();
            if let Some(have) = landed.get(&src_id) {
                dump_map.insert(src_id, have.clone());
                continue;
            }
            if let Some(f) = d["blob"].as_str() {
                let name = Path::new(f).file_name().and_then(|n| n.to_str()).unwrap_or("");
                let into = st.blobs_dir().join(name);
                if !into.is_file() {
                    copy_into(&mdir.join(STAGED).join(name), &into, ".incoming-merge-").map_err(interrupted)?;
                }
            }
            let mut fields = d["fields"].as_object().cloned().unwrap_or_default();
            if let Some(refs) = fields.get_mut("meta").and_then(|m| m.get_mut("refs")).and_then(Value::as_array_mut) {
                // The journal's refs are source ids (each one the source holds).
                *refs = refs.iter().filter_map(|r| r.as_str().and_then(|x| ref_map.get(x)).map(|n| json!(n))).collect();
            }
            let got = st.carry(d["state"].as_str().unwrap_or(""), &fields, &dump_by).map_err(interrupted)?;
            dump_map.insert(src_id, got);
            if first {
                fault::at("dump");
                first = false;
            }
        }
    }
    fault::at("dumps");
    // ---- the records: ONE write of the soup for everything but the records
    // a ripple wrote that nobody decided, which land in a SECOND write, of
    // the ripple class (so that every reader since ripples shipped, an older
    // `cauldron` too, reads them as unreviewed), and the edges from the
    // first's records to the second's in a third; the last of them lifts the
    // hold in the same transaction ----
    let digest = source["digest"].as_str().unwrap_or("").to_string();
    let source_id = source["cauldron_id"].as_str().unwrap_or("").to_string();
    let nonce = |part: &str| format!("cauldron-merge:{}:{merge}{part}", c.id().unwrap_or_else(|| c.name.clone()));
    let last = if !links.is_empty() {
        "links"
    } else if !ripple_records.is_empty() {
        "ripple"
    } else if !records.is_empty() {
        "main"
    } else {
        ""
    };
    // Each record's cite of a dump names the dump's id as it LANDED.
    let landed_of: BTreeMap<String, String> = planned_dumps
        .iter()
        .filter_map(|(src, planned)| dump_map.get(src).map(|got| (planned.clone(), got.clone())))
        .collect();
    let cite_landed = |rs: &mut Vec<Value>| {
        for r in rs.iter_mut() {
            if let Some(cites) = r.get_mut("meta").and_then(|m| m.get_mut("cites")).and_then(Value::as_array_mut) {
                for ct in cites.iter_mut() {
                    if ct["kind"] == "dump" {
                        if let Some(got) = ct["id"].as_str().and_then(|i| landed_of.get(i)) {
                            ct["id"] = json!(got);
                        }
                    }
                }
            }
        }
    };
    let opts = RecordsOptions { pool: PoolName::new(DEFAULT_POOL), routes: crate::clusters::routes(), remint: true, commit: true };
    let land = |handle: &mut spine::Cauldron, plan: MigrationPlan, rs: &[Value], lift: bool| -> Result<(String, i64)> {
        if lift {
            handle.lift_hold_at_next_commit();
        }
        let run = import_records(handle, &plan, rs, &opts).map_err(crate::region_lock::spine_error)?;
        match &run.landing {
            Some(Landing::Committed { tx, seq, .. }) => Ok((tx.to_hex(), *seq)),
            other => Err(store(match other {
                Some(Landing::Refused { witnesses }) => format!("the soup refused the write: {}", json!(witnesses)),
                _ => format!(
                    "{} record(s) were refused: {}",
                    run.records.iter().filter(|r| r.disposition != "accepted").count(),
                    json!(run.records.iter().filter(|r| r.disposition != "accepted").collect::<Vec<_>>())
                ),
            })),
        }
    };
    let mut parts = landed_parts.clone();
    // The main write.
    if !records.is_empty() && !parts.contains_key("main") {
        if j["wants_cluster_pool"] == json!(true) {
            crate::clusters::ensure_pool(handle, c).map_err(interrupted)?;
        }
        cite_landed(&mut records);
        let mut plan = MigrationPlan::new(Initiator::Agent {
            thread: by.clone(),
            dispatch: format!("{dispatch_head} {}", provenance::mark(&merge, &source_id, &digest, records.len())),
        });
        // One merge is one write per part, replayed (never landed twice) if
        // it is sent again.
        plan.idempotency_nonce = Some(nonce(""));
        let got = land(handle, plan, &records, last == "main").map_err(interrupted)?;
        parts.insert("main".into(), got);
    }
    fault::at("soup");
    let main_map = match parts.get("main") {
        Some((tx, _)) => landed_records(c, &merge, tx, &records).map_err(interrupted)?,
        None => BTreeMap::new(),
    };
    // The ripple part: its records' edges and links to the main write's
    // records name the ids those landed under.
    if !ripple_records.is_empty() && !parts.contains_key("ripple") {
        cite_landed(&mut ripple_records);
        let clusters: BTreeSet<&str> =
            records.iter().filter(|r| r["type"] == spine::grammar_cauldron::CLUSTER).filter_map(|r| r["id"].as_str()).collect();
        let pool_of = |label: &str| -> &'static str {
            if clusters.contains(label) { crate::clusters::POOL } else { DEFAULT_POOL }
        };
        let by_label: BTreeMap<String, String> = main_map.iter().map(|(src, id)| (label(src), id.clone())).collect();
        for r in ripple_records.iter_mut() {
            if let Some(es) = r.get_mut("edges").and_then(Value::as_array_mut) {
                for e in es.iter_mut() {
                    if let Some(id) = e["to"].as_str().and_then(|t| by_label.get(t).map(|id| (t.to_string(), id.clone()))) {
                        e["to"] = json!(format!("{}@{}", id.1, pool_of(&id.0)));
                    }
                }
            }
            if let Some(b) = r.get("body").and_then(Value::as_str) {
                let body = relink(b, &by_label);
                r["body"] = json!(body);
            }
        }
        let scope: Vec<String> = serde_json::from_value(j["ripple_scope"].clone()).unwrap_or_default();
        let kind = if scope.is_empty() { spine::envelope::RippleKind::TypeOne } else { spine::envelope::RippleKind::TypeTwo };
        let initiator = Initiator::Ripple {
            kind,
            run_id: provenance::ripple_run(&merge),
            step_id: provenance::part_mark("ripple", &merge, &source_id, &digest, ripple_records.len(), "records"),
            scope,
        };
        if last == "ripple" {
            handle.lift_hold_at_next_commit();
        }
        let got = land_ripple_part(c, handle, &ripple_records, initiator, &nonce(":ripple")).map_err(interrupted)?;
        parts.insert("ripple".into(), got);
    }
    fault::at("ripple");
    let ripple_map = match parts.get("ripple") {
        Some((tx, _)) => landed_records(c, &merge, tx, &ripple_records).map_err(interrupted)?,
        None => BTreeMap::new(),
    };
    // The links from the main write's records to the ripple part's.
    if !links.is_empty() && !parts.contains_key("links") {
        let mut ends: BTreeMap<String, String> = BTreeMap::new();
        let mut matches = Vec::new();
        let mut expects = Vec::new();
        let mut creates = Vec::new();
        let mut params: BTreeMap<String, Value> = BTreeMap::new();
        for (i, l) in links.iter().enumerate() {
            let mut bind = |src: &str, ty: &str, map: &BTreeMap<String, String>| -> Result<String> {
                let Some(id) = map.get(src) else { return Err(store(format!("the link's end {src} did not land"))) };
                if let Some(b) = ends.get(id) {
                    return Ok(b.clone());
                }
                let b = format!("n{}", ends.len());
                let pool = if ty == spine::grammar_cauldron::CLUSTER { crate::clusters::POOL } else { DEFAULT_POOL };
                matches.push(format!("MATCH ({b}: {pool}/{ty} {{ id: $id_{b} }})"));
                expects.push(format!("EXPECT COUNT({b}) = 1"));
                params.insert(format!("id_{b}"), json!(id));
                ends.insert(id.clone(), b.clone());
                Ok(b)
            };
            let a = bind(l["from"].as_str().unwrap_or(""), l["from_type"].as_str().unwrap_or(""), &main_map).map_err(interrupted)?;
            let b = bind(l["to"].as_str().unwrap_or(""), l["to_type"].as_str().unwrap_or(""), &ripple_map).map_err(interrupted)?;
            let mut line = format!("CREATE ({a}) -[{}]-> ({b})", l["kind"].as_str().unwrap_or(""));
            if !l["note"].is_null() {
                line.push_str(&format!(" NOTE $note_{i}"));
                params.insert(format!("note_{i}"), l["note"].clone());
            }
            creates.push(line);
        }
        if last == "links" {
            handle.lift_hold_at_next_commit();
        }
        let env = spine::envelope::Envelope {
            statement: matches.into_iter().chain(expects).chain(creates).collect::<Vec<_>>().join("\n"),
            bindings: params,
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
            provenance: spine::envelope::Provenance {
                initiator: Initiator::Agent {
                    thread: by.clone(),
                    dispatch: format!("{dispatch_head} {}", provenance::part_mark("links", &merge, &source_id, &digest, links.len(), "edges")),
                },
                instrument: spine::envelope::Instrument { name: "soup-door".into(), version: env!("CARGO_PKG_VERSION").into() },
            },
            mode: spine::envelope::Mode::Commit,
            idempotency_nonce: Some(nonce(":links")),
        };
        match handle.submit(env) {
            Ok(spine::Outcome::Committed { tx, seq, .. }) => {
                parts.insert("links".into(), (tx.to_hex(), seq.0 as i64));
            }
            Ok(_) => return Err(interrupted(store("the merge's links were validated and not committed"))),
            Err(e) => return Err(interrupted(crate::region_lock::spine_error(e))),
        }
    }
    let (tx, seq) = match parts.get("main").or_else(|| parts.get("ripple")) {
        Some((tx, seq)) => (Some(tx.clone()), *seq),
        None => (None, 0),
    };
    // What landed, read back from the target.
    let mut id_map = main_map;
    id_map.extend(ripple_map);
    let records = all_records;
    let as_planned = id_map == planned && dump_map == planned_dumps;
    // The holds let go: the dumps store's, and the soup's if no records
    // commit lifted it (a merge of dumps alone).
    if let Some(st) = dumps.as_deref_mut() {
        st.release_hold().map_err(interrupted)?;
    }
    if handle.held().is_some() {
        handle.release_hold().map_err(|e| interrupted(store(e)))?;
    }
    // ---- the merge record and the saved map ----
    let ids = json!({"merge": merge, "source": source, "records": id_map, "dumps": dump_map, "read_back_from": "the target, after the merge landed"});
    write_atomic(&mdir.join(ID_MAP), &ids).map_err(interrupted)?;
    let (soup_counter, dumps_counter) = (handle.committed_commit_counter().unwrap_or(0), dumps::counter(c).unwrap_or(0));
    // The change counters as the merge leaves them, for the record. An undo
    // does not compare against them (a recover may finish the merge after
    // another writer wrote, so "as the merge left it" is not knowable from
    // here): it compares the target's content outside the merge's own writes
    // with the snapshot's (`refuse_changed`).
    let undo = format!("cauldron merge --undo {merge} --into {} --env {}", c.name, dst.env.name);
    let record = json!({
        "merge": merge,
        "at": j["at"],
        "finished_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "by": merged_by,
        "thread": by,
        "allowed": j["allowed"],
        "source": source,
        "target": j["target"],
        "soup_tx": tx,
        "soup_seq": seq,
        "soup_writes": parts.iter().map(|(k, (h, q))| (k.clone(), json!({"tx": h, "seq": q}))).collect::<Map<String, Value>>(),
        "records": records.len(),
        "dumps": dump_map,
        "id_map": format!("{DIR}/{merge}/{ID_MAP}"),
        "landed_as_planned": as_planned,
        "snapshot": j["snapshot"],
        "after": {"soup_counter": soup_counter, "dumps_counter": dumps_counter},
        "undo": undo,
    });
    write_atomic(&mdir.join(RECORD), &record).map_err(interrupted)?;
    fault::at("record");
    let _ = std::fs::remove_dir_all(mdir.join(STAGED));
    let mut done = j.clone();
    done["state"] = json!("done");
    if let Some(m) = done.as_object_mut() {
        m.remove("ripple_records");
        m.remove("links");
    }
    if let Some(m) = done.as_object_mut() {
        // The journal keeps what landed, not the text a second time.
        m.remove("records");
        m.remove("dumps");
    }
    write_atomic(&mdir.join(JOURNAL), &done).map_err(interrupted)?;
    let mut doc = j["plan"].clone();
    doc["dry_run"] = json!(false);
    doc["outcome"] = json!("merged");
    doc["id_map"] = json!(id_map);
    doc["dump_map"] = json!(dump_map);
    doc["ids"] = json!("every source id is re-minted in the target: `id_map` (records) and `dump_map` (dumps), source id to target id, read back from what landed");
    doc["landed"] = json!({"soup_tx": tx, "seq": seq, "records": records.len(), "dumps": carries.len()});
    doc["record"] = json!(mdir.join(RECORD).display().to_string());
    doc["id_map_file"] = json!(mdir.join(ID_MAP).display().to_string());
    doc["snapshot"] = j["snapshot"].clone();
    doc["undo"] = json!(undo);
    doc["snapshot_note"] = json!(format!(
        "the snapshot is the whole target as it was before this merge (its soup, its dumps with every blob, its merges, the \
         region lock table), verified file by file, beside the cauldron's directory. `{undo}` puts it back exactly, as long as \
         nothing has written to the target since. It holds a copy of the target's dumps as they were: `cauldron soup \
         dump-remove` does not reach it, so move it away once you no longer need it"
    ));
    Ok(doc)
}

/// **`cauldron soup recover`'s half for a merge**: a merge into `dst` cut
/// short after its journal, through `handle` (the soup's, under its writer
/// lock), opening the target's dumps store when the merge carries dumps or
/// held it. `None` when no merge was pending. It never leaves the target
/// stuck:
///
/// - **abandoned**: the merge was cut short before it held the target and
///   before anything of it landed (an older writer may have written since,
///   so what it judged no longer holds): its hold is let go wherever it was
///   set, nothing is put back (nothing of it is there), and the merge can be
///   run again;
/// - **finished**: it lands in full, exactly once ([`roll_forward`]);
/// - **restored**: it cannot be finished (a staged blob lost, the disk
///   refusing, the soup refusing the batch): while the hold has kept every
///   other writer out since the snapshot, the target's soup, dumps store and
///   merges are put back exactly as the snapshot holds them, and it says so
///   and why;
/// - otherwise (it cannot be finished, and something else may have written
///   since, so putting the snapshot back could lose that) it refuses,
///   saying what failed and what the two choices are.
pub fn recover(dst: &Target, handle: &mut spine::Cauldron) -> Result<Option<Value>> {
    let c = &dst.cauldron;
    let Some((merge, j)) = pending(c)? else { return Ok(None) };
    let reason = j["hold"].as_str().unwrap_or("").to_string();
    let holder = format!("cli {} merge-recover", std::process::id());
    let soup_held = !reason.is_empty() && handle.adopt_hold(&reason).map_err(store)?;
    let landed_any = provenance::merge_txs(&c.soup())?.values().any(|m| m.merge == merge) || !landed_dumps(c, &merge)?.is_empty();
    let dumps_halted = dumps::exists(c) && halted_with(&dumps::dir(c), &reason)?;
    let carries = j["dumps"].as_array().is_some_and(|d| !d.is_empty());
    let mdir = dir(c).join(&merge);
    if !soup_held && !landed_any {
        if dumps_halted {
            let mut st = Store::open_for_merge_held(c, &holder, &reason)?;
            st.release_hold()?;
        }
        let mut gone = j.clone();
        gone["state"] = json!("abandoned");
        if let Some(m) = gone.as_object_mut() {
            m.remove("records");
            m.remove("dumps");
        }
        write_atomic(&mdir.join(JOURNAL), &gone)?;
        let _ = std::fs::remove_dir_all(mdir.join(STAGED));
        return Ok(Some(json!({
            "merge": merge,
            "outcome": "abandoned",
            "note": format!(
                "merge {merge} was cut short before it held cauldron `{}` and before any of it landed, so none of it is in the \
                 cauldron, and whatever was written to the cauldron since is kept. Run the merge again to bring the source's \
                 records in. Its snapshot ({}) is no longer needed; move it away.",
                c.name,
                j["snapshot"]["dir"].as_str().unwrap_or("?")
            ),
        })));
    }
    let mut store = if carries || j["dumps_held"] == json!(true) || dumps_halted {
        Some(Store::open_for_merge_held(c, &holder, &reason)?)
    } else {
        None
    };
    match roll_forward(dst, handle, store.as_mut(), j.clone()) {
        Ok(doc) => Ok(Some(json!({"merge": merge, "outcome": "finished", "landed": doc["landed"], "record": doc["record"]}))),
        Err(failed) => {
            drop(store);
            // Safe to put back only while nothing else can have written: the
            // soup still held by this merge, and its dumps store held too or
            // not written since the snapshot.
            let soup_still_held = halted_with(&c.soup(), &reason)?;
            let dumps_untouched = halted_with(&dumps::dir(c), &reason)?
                || crate::snapshot::counters(c)?["dumps"] == j["snapshot"]["counters"]["dumps"];
            let snap = PathBuf::from(j["snapshot"]["dir"].as_str().unwrap_or(""));
            let have_snapshot = snap.join(crate::snapshot::MANIFEST).is_file();
            if !(soup_still_held && dumps_untouched && have_snapshot) {
                let why = if !have_snapshot {
                    format!("because the merge's snapshot ({}) is not there any more", snap.display())
                } else {
                    "because something other than the merge may have written to the cauldron since it was taken (putting it back \
                     would lose that)"
                        .to_string()
                };
                return Err(Error::new(
                    failed.code,
                    format!(
                        "{} Recover could not finish it, and did not put the snapshot back, {why}. Run `cauldron soup recover` again \
                         once what failed is fixed; or, to lift the merge's hold and keep what of it landed (listed), run `cauldron \
                         soup recover --env {} --cauldron {} --abandon-merge --keep-partial-merge`.",
                        failed.message,
                        c.env,
                        c.name
                    ),
                ));
            }
            let s = crate::snapshot::scope(c);
            let only = vec![s.parts[0].clone(), dumps::DIR.to_string(), DIR.to_string()];
            let aside = snap.join(format!("abandoned-{}", chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ")));
            let restored = crate::snapshot::restore_except(c, &snap, &aside, Some(&only), &[format!("{}/{}", s.parts[0], env::region_locks::FILE)])?;
            Ok(Some(json!({
                "merge": merge,
                "outcome": "restored",
                "why": failed.message,
                "restored": restored,
                "note": format!(
                    "merge {merge} could not be finished ({}), so recover put cauldron `{}` back exactly as it was before the \
                     merge, from its snapshot ({}): its soup, its dumps store and its merges. None of the merge is in the \
                     cauldron. What the merge had landed was moved into {} (nothing was deleted). Fix what failed and run the \
                     merge again.",
                    failed.message,
                    c.name,
                    snap.display(),
                    aside.display()
                ),
            })))
        }
    }
}

/// **Abandon a merge cut short, on the owner's word** (`cauldron soup
/// recover --abandon-merge`): its hold on both stores is lifted, what of it
/// landed STAYS (each soup write by part, each dump), its journal is marked
/// abandoned with that list, and nothing is put back. The caller holds the
/// soup's writer lock; the dumps store's is taken here.
pub fn abandon(dst: &Target) -> Result<Value> {
    let c = &dst.cauldron;
    let Some((merge, j)) = pending(c)? else { return Ok(json!({"outcome": "none"})) };
    let _dumps = dumps::lock_for_merge(c, &format!("cli {} merge-abandon", std::process::id()))?;
    let reason = j["hold"].as_str().unwrap_or("").to_string();
    let parts: BTreeMap<String, Value> = provenance::merge_txs(&c.soup())?
        .into_iter()
        .filter(|(_, m)| m.merge == merge)
        .map(|(h, m)| (m.part.clone(), json!({"tx": h, "seq": m.seq})))
        .collect();
    let landed_dumps = landed_dumps(c, &merge)?;
    release_store(&c.soup(), &reason)?;
    release_store(&dumps::dir(c), &reason)?;
    let landed = json!({"soup_writes": parts, "dumps": landed_dumps});
    let mut gone = j.clone();
    gone["state"] = json!("abandoned");
    gone["abandoned_by"] = json!("cauldron soup recover --abandon-merge");
    gone["landed"] = landed.clone();
    if let Some(m) = gone.as_object_mut() {
        m.remove("records");
        m.remove("ripple_records");
        m.remove("links");
        m.remove("dumps");
    }
    write_atomic(&dir(c).join(&merge).join(JOURNAL), &gone)?;
    Ok(json!({
        "merge": merge,
        "outcome": "abandoned-on-request",
        "landed": landed,
        "note": format!(
            "merge {merge} was not finished: its hold on cauldron `{}` is lifted and writes go on. What of it landed stays \
             (`landed`: its soup writes by part, its dumps by source id); nothing was put back. The snapshot the merge took \
             ({}) holds the cauldron as it was before it, if it is still there.",
            c.name,
            j["snapshot"]["dir"].as_str().unwrap_or("?")
        ),
    }))
}

/// **Sweep the merges that never began landing**: a `merges/<MG-n>/` with
/// neither a journal nor a record was cut short before its journal, so
/// nothing of it landed; what it staged (copies of the source's blobs) is
/// removed. So is a snapshot cut short while it was being taken
/// (`<cauldron>.taking-<pid>/` beside the cauldron: a partial private copy
/// of the target, never used; no snapshot is being taken while the caller
/// holds the soup's writer lock, which every merge holds while it takes
/// one). Answers the merge ids and the directories swept. A complete
/// snapshot is never removed here: [`unused_snapshots`] lists the ones no
/// merge used.
pub fn sweep_unstarted(c: &env::Cauldron) -> Result<Vec<String>> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir(c)) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if merge_serial(&name).is_none() || e.path().join(JOURNAL).exists() || e.path().join(RECORD).exists() {
                continue;
            }
            std::fs::remove_dir_all(e.path()).map_err(|err| store(format!("sweeping {}: {err}", e.path().display())))?;
            out.push(name);
        }
    }
    let base = env::private::data_dir(c);
    let me = base.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    let taking = format!("{me}.taking-");
    if let Ok(rd) = std::fs::read_dir(base.parent().unwrap_or(Path::new("."))) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let is_real_dir = std::fs::symlink_metadata(e.path()).is_ok_and(|m| m.is_dir());
            if is_real_dir && name.strip_prefix(&taking).is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit())) {
                std::fs::remove_dir_all(e.path()).map_err(|err| store(format!("sweeping {}: {err}", e.path().display())))?;
                out.push(name);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// **The snapshots beside `c` that no merge used**: a merge's snapshot
/// whose merge never wrote its journal (it was cut short before it began
/// landing, or its forwarding service died after the snapshot), named so
/// they can be moved away. A snapshot a merge did use is its undo's and is
/// not listed.
pub fn unused_snapshots(c: &env::Cauldron) -> Vec<String> {
    let base = env::private::data_dir(c);
    let me = base.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    let mut out = Vec::new();
    for e in std::fs::read_dir(base.parent().unwrap_or(Path::new("."))).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(rest) = name.strip_prefix(&format!("{me}.")) else { continue };
        let Some((merge, _)) = rest.split_once("-before-") else { continue };
        if merge_serial(merge).is_none() || !e.path().join(crate::snapshot::MANIFEST).is_file() {
            continue;
        }
        // Taken of another cauldron that had this name (it was renamed, and
        // this one made since): that one's undo may need it, so not listed.
        if let (Some(theirs), Some(ours)) = (snapshot_cauldron_id(&e.path()), c.id()) {
            if theirs != ours {
                continue;
            }
        }
        let mdir = dir(c).join(merge);
        let used = mdir.join(JOURNAL).exists() || mdir.join(RECORD).exists() || e.path().join(UNDO).exists() || e.path().join(UNDOING).exists();
        if !used {
            out.push(e.path().display().to_string());
        }
    }
    out.sort();
    out
}

/// Is the spine in `dir` halted with exactly `reason`? (read-only)
fn halted_with(dir: &Path, reason: &str) -> Result<bool> {
    if reason.is_empty() || !dir.join(spine::spine_db::SPINE_FILE).is_file() {
        return Ok(false);
    }
    let conn = spine::offline::read_only_connection(&dir.join(spine::spine_db::SPINE_FILE)).map_err(store)?;
    let (h, r): (i64, Option<String>) =
        conn.query_row("SELECT halted, halted_reason FROM spine_state WHERE singleton = 1", [], |x| Ok((x.get(0)?, x.get(1)?))).map_err(store)?;
    Ok(h != 0 && r.as_deref() == Some(reason))
}

/// A merge's own directory, relative to the snapshot scope's base
/// (`merges/<MG-n>`).
fn own_dir(_c: &env::Cauldron, merge: &str) -> String {
    format!("{DIR}/{merge}")
}

/// The marker an undo writes before it moves anything, BESIDE the snapshot
/// scope (in the cauldron's directory, never in `soup/`, `dumps/` or
/// `merges/`, which the undo puts back), and a copy in the snapshot.
pub const UNDOING: &str = "merge-undoing.json";

/// The marker's path for `c`.
fn undoing_path(c: &env::Cauldron) -> PathBuf {
    env::private::data_dir(c).join(UNDOING)
}

/// **The undo of a merge into `c` that was cut short**, if there is one:
/// its marker.
pub fn undo_pending(c: &env::Cauldron) -> Result<Option<Value>> {
    let p = undoing_path(c);
    match std::fs::read_to_string(&p) {
        Ok(t) => serde_json::from_str(&t).map(Some).map_err(|e| unreadable_marker(c, &p, &e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(unreadable_marker(c, &p, &e.to_string())),
    }
}

/// The refusal for an undo marker that cannot be read: it names the way out.
fn unreadable_marker(c: &env::Cauldron, p: &Path, why: &str) -> Error {
    Error::new(
        Code::NotAtRest,
        format!(
            "REFUSED: the marker of an undo of a merge into cauldron `{}` ({}) cannot be read ({why}), so the undo can be neither \
             finished nor told apart from a cauldron at rest, and nothing of this release writes to the cauldron. Nothing was \
             written. Run `{}`: it reads the copy of the marker the undo keeps in its snapshot where it can, returns what the undo \
             had put back, lifts its hold, and says what it could and could not tell.",
            c.name,
            p.display(),
            stuck::recover_cmd(c, "--abandon-undo")
        ),
    )
}

/// The hold an undo of `merge` puts on the target's two stores while it
/// puts them back.
fn undo_hold_reason(c: &env::Cauldron, merge: &str) -> String {
    format!(
        "cauldron merge --undo {merge}: cauldron `{}` is being put back as it was before merge {merge}; `cauldron soup recover \
         --env {} --cauldron {}` (or the same undo again) finishes it",
        c.name, c.env, c.name
    )
}

/// Hold one store (its spine at `dir`) under `reason`, adopting a hold
/// already there under it. A store with no spine is not held.
fn hold_store(dir: &Path, reason: &str) -> Result<()> {
    if !dir.join(spine::spine_db::SPINE_FILE).is_file() {
        return Ok(());
    }
    let mut h = spine::Cauldron::open(dir, spine::config::SpineConfig::default()).map_err(|e| store(format!("opening {} to hold it: {e}", dir.display())))?;
    if !h.adopt_hold(reason).map_err(store)? {
        h.hold(reason).map_err(|e| store(format!("holding {}: {e}", dir.display())))?;
    }
    Ok(())
}

/// Let go of the hold `reason` on the store at `dir`, if it is there.
fn release_store(dir: &Path, reason: &str) -> Result<()> {
    if !halted_with(dir, reason)? {
        return Ok(());
    }
    let mut h = spine::Cauldron::open(dir, spine::config::SpineConfig::default()).map_err(|e| store(format!("opening {}: {e}", dir.display())))?;
    if h.adopt_hold(reason).map_err(store)? {
        h.release_hold().map_err(store)?;
    }
    Ok(())
}

/// **`cauldron merge --undo <merge> --into <target>`**: put the target back
/// exactly as it was before `merge`, from the snapshot that merge took,
/// ONLY IF nothing has written to the target since the merge finished: its
/// two stores' change counters and every other file of the snapshot's scope
/// (blobs, the region lock table, merge records) must be as the merge left
/// them. Otherwise refused ([`Code::ChangedSince`]), saying what changed
/// and what the choice is. The caller holds the target's writer locks (its
/// soup's, and its dumps store's from [`crate::dumps::lock_for_merge`]).
///
/// **Resumable, and never over another write.** Before it moves anything
/// it writes a marker ([`UNDOING`], beside the cauldron's stores, and a
/// copy in the snapshot), then HOLDS both stores as a merge does (every
/// writer of any version is refused; this release's name the undo), then
/// records every file of the target with its digest, then puts the files
/// back in an order that keeps each store whole
/// ([`crate::snapshot::restore_guarded`], the merge's own record last). An
/// undo cut short is finished by the same undo again, by `cauldron soup
/// recover`, or by the next writer of this release that opens the
/// cauldron, as long as its snapshot is there and whole; when it is not,
/// they all refuse naming `cauldron soup recover --abandon-undo`
/// ([`abandon_undo`]). Until then `cauldron soup recover`, `cauldron
/// status` and `cauldron merge --status` say where it stands.
///
/// What the restore replaces or removes is not deleted: it is moved beside
/// the cauldron (`<name>.<MG-n>-undone-<time>/`, outside the snapshot, so
/// an abandoned undo can return it), with the merge's own record, and an
/// `undo.json` in the snapshot says what was done.
pub fn undo(dst: &Target, merge: &str, by: &str, var: Option<&str>) -> Result<Value> {
    fault::check_undo()?;
    let c = &dst.cauldron;
    if merge_serial(merge).is_none() {
        refuse!(Code::Usage, "REFUSED: `{merge}` is not a merge id (MG-<n>). Nothing was changed.");
    }
    if let Some(m) = undo_pending(c)? {
        let other = m["merge"].as_str().unwrap_or("?");
        if other != merge {
            refuse!(
                Code::NotAtRest,
                "REFUSED: an undo of merge {other} into cauldron `{}` was cut short and is not finished; finish it first with \
                 `cauldron merge --undo {other} --into {} --env {}` (or `cauldron soup recover --env {} --cauldron {}`). Nothing \
                 was changed.",
                c.name,
                c.name,
                dst.env.name,
                dst.env.name,
                c.name
            );
        }
        return resume_undo(c, m, "the same undo, run again");
    }
    if let Some((id, _)) = pending(c)? {
        refuse!(
            Code::NotAtRest,
            "REFUSED: merge {id} into cauldron `{}` was cut short and is not finished; finish it first with `cauldron soup \
             recover --env {} --cauldron {}`, then undo. Nothing was changed.",
            c.name,
            dst.env.name,
            c.name
        );
    }
    let mdir = dir(c).join(merge);
    let record: Value = match std::fs::read_to_string(mdir.join(RECORD)) {
        Ok(t) => serde_json::from_str(&t).map_err(|e| store(format!("{} will not parse: {e}", mdir.join(RECORD).display())))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(done) = undone(c, merge) {
                refuse!(
                    Code::NotFound,
                    "REFUSED: merge {merge} into cauldron `{}` was already undone ({}). Nothing was changed.",
                    c.name,
                    done["at"].as_str().unwrap_or("?")
                );
            }
            refuse!(
                Code::NotFound,
                "REFUSED: cauldron `{}` holds no finished merge {merge} (no {}). Nothing was changed. The merges it holds: {}.",
                c.name,
                mdir.join(RECORD).display(),
                recorded(c).iter().filter_map(|r| r["merge"].as_str().map(str::to_string)).collect::<Vec<_>>().join(", ")
            )
        }
        Err(e) => return Err(store(format!("reading {}: {e}", mdir.join(RECORD).display()))),
    };
    undo_gate(dst, merge, &record, by == "tty", var)?;
    let Some(snap) = record["snapshot"]["dir"].as_str().map(PathBuf::from) else {
        refuse!(Code::Usage, "REFUSED: merge {merge} recorded no snapshot to undo it from. Nothing was changed.");
    };
    if !snap.join(crate::snapshot::MANIFEST).is_file() {
        refuse!(
            Code::NotFound,
            "REFUSED: the snapshot merge {merge} took ({}) is not there any more, so the merge cannot be undone exactly. Nothing \
             was changed: the cauldron keeps the merge, and writes go on.",
            snap.display()
        );
    }
    // The snapshot checked whole before it is read for the comparison: a
    // copy missing from it would read as content the target gained.
    if let Err(e) = crate::snapshot::verify(&snap) {
        return Err(Error::new(
            e.code,
            format!(
                "{} So merge {merge} cannot be undone exactly from it: cauldron `{}` keeps the merge, nothing was changed, and \
                 writes go on. To undo it, put back an intact copy of the snapshot at {}.",
                e.message,
                c.name,
                snap.display()
            ),
        ));
    }
    refuse_changed(c, merge, &record, &snap)?;
    let at = chrono::Utc::now();
    // What the undo replaces goes BESIDE the cauldron, not into the
    // snapshot: should the snapshot go while the undo is cut short, `soup
    // recover --abandon-undo` can still return it.
    let aside = undone_dir(c, merge, &at);
    let reason = undo_hold_reason(c, merge);
    let mut marker = json!({
        "format": FORMAT,
        "merge": merge,
        "env": dst.env.name,
        "cauldron": c.name,
        "state": "verified",
        "at": at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "by": by,
        "snapshot": snap.display().to_string(),
        "aside": aside.display().to_string(),
        "hold": reason,
        "note": "an undo of this merge began here; the same undo again, `cauldron soup recover`, or the next writer of this release that opens the cauldron finishes it",
    });
    // The marker first: from here on every writer of this release is
    // refused (or finishes the undo), and a cut is resumable.
    write_atomic(&undoing_path(c), &marker)?;
    let _ = crate::snapshot::write_json(&snap.join(UNDOING), &marker);
    fault::at_undo("marked");
    resume_undo(c, std::mem::take(&mut marker), "")
}

/// **Undoing an owner's merge is the same owner act as the merge**
/// (`env::owner_acts`, act `merge`): a merge run at a terminal (`by: tty`),
/// or one that carried decisions, is undone at an interactive terminal, or
/// without one only where the owner opened `merge` (the persisted setting,
/// or `CAULDRON_OWNER_ACTS=merge` for the command). An agent's merge that
/// carried no decision was no owner act, and its undo is none either. An
/// undo already begun (cut short) is finished by anyone, as every writer
/// finishes it.
fn undo_gate(dst: &Target, merge: &str, record: &Value, at_terminal: bool, var: Option<&str>) -> Result<()> {
    let owners = record["by"] == "tty" || record["allowed"].as_str().is_some_and(|a| !a.starts_with("no owner act"));
    if !owners || at_terminal {
        return Ok(());
    }
    use env::owner_acts::{self, Act};
    let c = &dst.cauldron;
    let setting = owner_acts::load(&dst.env.root).unwrap_or_default();
    match owner_acts::decide(Act::Merge, &setting, &dst.env.name, &c.name, c.id().as_deref(), false, var) {
        Ok(_) => Ok(()),
        Err(r) => Err(Error::new(
            Code::NotATerminal,
            format!(
                "REFUSED: merge {merge} into cauldron `{}` was the owner's (run {}{}), and undoing it is the same owner act `merge`: at \
                 an interactive terminal, or without one only where the owner has opened `merge`. Nothing was changed. Ask the user \
                 to run, in a terminal on this machine:\n    cauldron merge --undo {merge} --into {} --env {}\n{}{}",
                c.name,
                if record["by"] == "tty" { "at a terminal" } else { "where `merge` was opened" },
                if record["allowed"].as_str().is_some_and(|a| !a.starts_with("no owner act")) { ", carrying decisions" } else { "" },
                c.name,
                dst.env.name,
                r.how_to_allow,
                r.bad_var.map(|b| format!(" {b}; it opens nothing.")).unwrap_or_default()
            ),
        )
        .with_detail(json!({"merge": merge, "merged_by": record["by"], "owner_act": "merge"}))),
    }
}

/// Why an undo's marker `m` cannot be its own, if it cannot: its snapshot,
/// its `-undone-` directory or its hold do not resolve to what an undo
/// writes, or its state is not one an undo writes. One check for every
/// reader (the undo, recover, the writers, `--abandon-undo`).
pub(crate) fn marker_unsound(c: &env::Cauldron, m: &Value) -> Option<String> {
    let resolves = |k: &str| m[k].as_str().map(PathBuf::from).is_some_and(|p| p.is_absolute() && p.parent().is_some_and(Path::is_dir));
    if !resolves("snapshot") || !resolves("aside") || !m["hold"].as_str().is_some_and(|h| h.starts_with(stuck::UNDO_HOLD_PREFIX)) || m["merge"].as_str().and_then(merge_serial).is_none() {
        return Some(format!("its marker ({}) names a snapshot, a directory or a hold that cannot be its own", undoing_path(c).display()));
    }
    if !["verified", "restoring"].contains(&m["state"].as_str().unwrap_or("")) {
        return Some(format!("its marker says it is `{}`, which no undo writes", m["state"]));
    }
    None
}

/// **Carry an undo on from its marker `m`** (the caller holds the target's
/// writer locks): from `verified` (nothing moved yet: the change check is
/// made again under the hold, since a writer of an older release could
/// have written before the hold was taken, and the undo is refused, its
/// hold let go and its marker removed, when one did), or from `restoring`
/// (some files may be back already: [`crate::snapshot::restore_guarded`]
/// finishes it, never over a write that came after it began). `finished_by`
/// names what finished an undo cut short (empty for one that ran through).
pub fn resume_undo(c: &env::Cauldron, mut m: Value, finished_by: &str) -> Result<Value> {
    let merge = m["merge"].as_str().unwrap_or("").to_string();
    let snap = PathBuf::from(m["snapshot"].as_str().unwrap_or(""));
    let aside = PathBuf::from(m["aside"].as_str().unwrap_or(""));
    let reason = m["hold"].as_str().unwrap_or("").to_string();
    let soup_dir = c.soup();
    let dumps_dir = dumps::dir(c);
    // A marker that cannot be its own (edited, or written by something
    // else) is damaged: the stuck refusal names the way out.
    if let Some(why) = marker_unsound(c, &m) {
        return Err(stuck::stuck_undo(c, &m, &why, finished_by));
    }
    // An undo run through from `undo` has just checked the snapshot whole;
    // one resumed checks it again before it moves anything.
    let fresh = finished_by.is_empty();
    let mut checked_now = false;
    if m["state"] == "verified" {
        // Hold both stores first, then look again: whatever wrote before the
        // hold is seen now, and nothing can write after it.
        hold_store(&soup_dir, &reason)?;
        hold_store(&dumps_dir, &reason)?;
        fault::at_undo("held");
        // Nothing is moved yet. When the undo can no longer go on (its
        // record, or its snapshot, gone or damaged; or the target written
        // before the hold), it is DROPPED, saying so: its hold let go and
        // its marker removed, the cauldron exactly as the merge left it.
        let drop_it = |why: Error| -> Error {
            if let Err(e) = release_store(&soup_dir, &reason).and_then(|_| release_store(&dumps_dir, &reason)) {
                return e;
            }
            let _ = std::fs::remove_file(undoing_path(c));
            let _ = std::fs::remove_file(snap.join(UNDOING));
            dropped(c, &m, why, finished_by)
        };
        let mdir = dir(c).join(&merge);
        let Some(record) = std::fs::read_to_string(mdir.join(RECORD)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
            return Err(drop_it(Error::new(
                Code::NotFound,
                format!("the record of merge {merge} ({}) is gone or cannot be read", mdir.join(RECORD).display()),
            )));
        };
        let checked = if snap.join(crate::snapshot::MANIFEST).is_file() {
            if fresh {
                std::fs::read_to_string(snap.join(crate::snapshot::MANIFEST))
                    .map_err(|e| store(format!("reading {}: {e}", snap.join(crate::snapshot::MANIFEST).display())))
                    .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| store(format!("the snapshot's manifest will not parse: {e}"))))
            } else {
                crate::snapshot::verify(&snap)
            }
        } else {
            Err(Error::new(Code::NotFound, format!("the snapshot merge {merge} took ({}) is not there any more", snap.display())))
        };
        let want = match checked {
            Ok(man) => man["files"].clone(),
            Err(e) => return Err(drop_it(e)),
        };
        if let Err(e) = refuse_changed(c, &merge, &record, &snap) {
            return Err(drop_it(e));
        }
        let start = crate::snapshot::files_now(c)?;
        m["state"] = json!("restoring");
        m["start"] = crate::snapshot::files_json(&start);
        // What the snapshot holds, so an undo abandoned later can tell the
        // copies it put back from anything else's write, snapshot or not.
        m["want"] = Value::Object(
            want.as_array()
                .into_iter()
                .flatten()
                .filter_map(|f| Some((f["path"].as_str()?.to_string(), json!({"sha256": f["sha256"], "bytes": f["bytes"]}))))
                .collect(),
        );
        write_atomic(&undoing_path(c), &m)?;
        let _ = crate::snapshot::write_json(&snap.join(UNDOING), &m);
        fault::at_undo("recorded");
        checked_now = fresh;
    }
    let start = crate::snapshot::files_from_json(&m["start"]);
    if m["state"] != "restoring" || start.is_empty() {
        return Err(stuck::stuck_undo(c, &m, "its marker does not say which files the cauldron held when it began", finished_by));
    }
    if !snap.join(crate::snapshot::MANIFEST).is_file() {
        return Err(stuck::stuck_undo(c, &m, &format!("its snapshot ({}) is not there any more", snap.display()), finished_by));
    }
    if !checked_now {
        if let Err(e) = crate::snapshot::verify(&snap) {
            return Err(stuck::stuck_undo(c, &m, &format!("its snapshot is damaged ({})", e.message), finished_by));
        }
    }
    let last = vec![own_dir(c, &merge)];
    // A rename since the merge: the manifest keeps the cauldron's new name.
    let keep: Vec<String> = if renamed_only(c, &snap) { vec![env::cauldron::CAULDRON_MANIFEST.to_string()] } else { Vec::new() };
    let restored = crate::snapshot::restore_guarded(c, &snap, &aside, &start, &last, &keep)?;
    fault::at_undo("restored");
    let kept = restored["kept"].as_array().cloned().unwrap_or_default();
    let mut doc = json!({
        "env": c.env,
        "cauldron": c.name,
        "merge": merge,
        "outcome": "undone",
        "at": m["at"],
        "finished_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "by": m["by"],
        "snapshot": snap.display().to_string(),
        "restored": restored,
        "note": format!(
            "cauldron `{}` is exactly as it was before merge {merge}: its soup, its dumps with every blob, its merges and its \
             region lock table, file for file. What the merge had added, with its record, was moved into {} (nothing was \
             deleted); move the snapshot away once you no longer need it.",
            c.name,
            aside.display()
        ),
    });
    if !finished_by.is_empty() {
        doc["resumed"] = json!(format!("this undo was cut short and was finished by {finished_by}"));
    }
    if !kept.is_empty() {
        doc["note"] = json!(format!(
            "cauldron `{}` was put back as it was before merge {merge}, except {} file(s) something else wrote after the undo \
             began, which were left exactly as they are (listed in `restored.kept`; each belongs to a store the undo had \
             already put back whole). What the merge had added was moved into {} (nothing was deleted).",
            c.name,
            kept.len(),
            aside.display()
        ));
    }
    crate::snapshot::write_json(&snap.join(UNDO), &doc)?;
    let _ = std::fs::remove_file(undoing_path(c));
    let _ = std::fs::remove_file(snap.join(UNDOING));
    Ok(doc)
}

/// Where an undo of `merge` begun `at` moves what it replaces: beside the
/// cauldron's directory (`<name>.<MG-n>-undone-<time>/`), outside the
/// snapshot.
fn undone_dir(c: &env::Cauldron, merge: &str, at: &chrono::DateTime<chrono::Utc>) -> PathBuf {
    let base = env::private::data_dir(c);
    let name = base.file_name().and_then(|n| n.to_str()).unwrap_or("cauldron").to_string();
    base.parent().unwrap_or(Path::new(".")).join(format!("{name}.{merge}-undone-{}", at.format("%Y%m%dT%H%M%S%3fZ")))
}

/// An undo dropped before it moved anything: what it says, by what found
/// it, and why.
fn dropped(c: &env::Cauldron, m: &Value, why: Error, finished_by: &str) -> Error {
    let merge = m["merge"].as_str().unwrap_or("?");
    if why.code == Code::ChangedSince && finished_by.is_empty() {
        return why;
    }
    let found = if finished_by.is_empty() { String::new() } else { format!(" Found by {finished_by}, which wrote nothing; run it again.") };
    Error::new(
        why.code,
        format!(
            "REFUSED: the undo of merge {merge} into cauldron `{}` (begun {} by {}) was DROPPED before it moved anything: {}. \
             Nothing of it was put back, so the cauldron is exactly as merge {merge} left it; the undo's hold is lifted and its \
             marker removed, and writes go on.{found} To undo the merge after all, {}run `cauldron merge --undo {merge} --into {} \
             --env {}` again.",
            c.name,
            m["at"].as_str().unwrap_or("?"),
            m["by"].as_str().unwrap_or("?"),
            why.message.trim_end_matches('.'),
            if why.code == Code::ChangedSince { "" } else { "put back what is missing (above), then " },
            c.name,
            c.env
        ),
    )
    .with_detail(json!({"merge": merge, "undo_dropped": true, "why": why.message, "detail": why.detail}))
}

/// **The merge's own writes in `c`**, as [`crate::snapshot::outside`]
/// leaves them out: its soup transactions (every part), its dumps (found by
/// the `meta.merged_from` it gave them) and their writes, its directory.
fn merge_writes(c: &env::Cauldron, merge: &str) -> Result<crate::snapshot::MergeWrites> {
    let soup_txs = provenance::merge_txs(&c.soup())?.into_iter().filter(|(_, m)| m.merge == merge).map(|(h, _)| h).collect();
    let dump_ids = landed_dumps(c, merge)?.into_values().collect();
    Ok(crate::snapshot::MergeWrites { soup_txs, dump_ids, dump_dispatch_end: format!("[merge {merge} dump]"), own_dir: own_dir(c, merge) })
}

/// **Refused unless the target's content outside the merge is exactly what
/// it was when the merge began**: the snapshot's content (read from its
/// copies, nothing written) against the target's now, leaving out exactly
/// the merge's own writes ([`merge_writes`]): its transactions, the records
/// and dumps they created, the edges from or to those, the blobs only its
/// dumps name, and its own record. Anything else that differs (a record,
/// an edge, a dump, a write of either store, a blob, the region lock table,
/// another file) was written by something else since the merge began
/// (after a crash and a recover, a release that knows nothing of merges
/// too), and putting the snapshot back would remove it: refused, listing
/// what changed by count and id.
fn refuse_changed(c: &env::Cauldron, merge: &str, _record: &Value, snap: &Path) -> Result<()> {
    // A snapshot that is not there reads as an empty cauldron, which would
    // make the target's own records look like later writes.
    if !snap.join(crate::snapshot::MANIFEST).is_file() || !snap.join(crate::snapshot::FILES).is_dir() {
        refuse!(
            Code::NotFound,
            "REFUSED: the snapshot merge {merge} took ({}) is not there any more (or holds no copies), so what the cauldron held \
             before the merge cannot be compared, and the merge cannot be undone exactly. Nothing was changed.",
            snap.display()
        );
    }
    let soup_name = crate::snapshot::scope(c).parts[0].clone();
    let before = crate::snapshot::outside(&snap.join(crate::snapshot::FILES), &soup_name, None)?;
    let writes = merge_writes(c, merge)?;
    let now = crate::snapshot::outside(&env::private::data_dir(c), &soup_name, Some(&writes))?;
    let mut now = now;
    if renamed_only(c, snap) {
        // A rename since the merge changed the manifest's name, nothing else.
        let rel = env::cauldron::CAULDRON_MANIFEST;
        now["files"][rel] = before["files"][rel].clone();
    }
    let diff = crate::snapshot::outside_changed(&before, &now);
    if diff.as_object().is_none_or(|o| o.is_empty()) {
        return Ok(());
    }
    Err(changed_since(c, merge, diff, snap))
}

fn changed_since(c: &env::Cauldron, merge: &str, diff: Value, snap: &Path) -> Error {
    let mdir = dir(c).join(merge);
    let said = crate::snapshot::outside_said(&diff);
    let ids: Vec<String> = diff["records"]["added_ids"]
        .as_array()
        .into_iter()
        .chain(diff["records"]["changed_ids"].as_array())
        .flatten()
        .filter_map(|v| v.as_str().map(|s| s.rsplit('/').next().unwrap_or(s).to_string()))
        .take(20)
        .collect();
    let see = if ids.is_empty() {
        format!("`cauldron soup changes --env {} --cauldron {}` lists the writes since", c.env, c.name)
    } else {
        format!("`cauldron soup get {} --env {} --cauldron {}` shows them", ids.join(" "), c.env, c.name)
    };
    Error::new(
        Code::ChangedSince,
        format!(
            "REFUSED: cauldron `{}` holds writes that are not merge {merge}'s, made since it began: {}. Putting the snapshot \
             back would remove them, and an undo never removes anything it cannot prove the merge created. Nothing was \
             changed. See them first: {see}. What can be done safely: keep the merge, and hide what you do not want of it by \
             writing a record that supersedes it (`cauldron soup reject` takes only records a ripple wrote); the id map {} \
             names exactly the records the merge created. Undoing the merge while keeping the later writes is not offered: a \
             later write can name the merge's records (an edge, `supersedes`, a citation, a [[link]]), and writing it again onto \
             the cauldron as it was would give it another transaction and other ids, so it would no longer be the same write. \
             To lose the later writes instead (and write them again by hand), put the snapshot back with nothing writing to \
             the cauldron: move its `soup/`, `dumps/` and `merges/` aside first, then copy everything under {} in their place.",
            c.name,
            said.join("; "),
            mdir.join(ID_MAP).display(),
            snap.join(crate::snapshot::FILES).display()
        ),
    )
    .with_detail(json!({"merge": merge, "changed_since": said, "changed_outside_the_merge": diff, "snapshot": snap.display().to_string()}))
}

/// The snapshots merge `merge` of `c` took (beside the cauldron's
/// directory), newest last.
fn snapshots_of(c: &env::Cauldron, merge: &str) -> Vec<PathBuf> {
    let base = env::private::data_dir(c);
    let name = base.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    let prefix = format!("{name}.{merge}-before-");
    let id = c.id();
    // Under this cauldron's name; or, taken before it was renamed, under
    // another name, recognised by the cauldron id its manifest records
    // (and never one a cauldron of another id took under this name).
    let mut out: Vec<(String, PathBuf)> = std::fs::read_dir(base.parent().unwrap_or(Path::new(".")))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            if n.contains(".taking-") {
                return None;
            }
            let (_, rest) = n.split_once('.')?;
            let (m, stamp) = rest.split_once("-before-")?;
            if m != merge {
                return None;
            }
            let theirs = snapshot_cauldron_id(&e.path());
            let ours = match (&theirs, &id) {
                (Some(t), Some(i)) => t == i,
                _ => n.starts_with(&prefix),
            };
            ours.then(|| (stamp.to_string(), e.path()))
        })
        .collect();
    out.sort();
    out.into_iter().map(|x| x.1).collect()
}

/// The cauldron id a snapshot's manifest records, if it can be read.
fn snapshot_cauldron_id(snap: &Path) -> Option<String> {
    let text = std::fs::read_to_string(snap.join(crate::snapshot::MANIFEST)).ok()?;
    // The id is near the top; the file list after it can be long.
    let v: Value = serde_json::from_str(&text).ok()?;
    v["cauldron_id"].as_str().map(str::to_string)
}

/// A cauldron manifest's text without its `name` line: what a rename
/// changes, and all it changes.
fn manifest_without_name(text: &str) -> String {
    text.lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.starts_with("name ") || t.starts_with("name="))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Was `c` renamed since the snapshot at `snap` was taken, and is that the
/// only change to its manifest? Then the manifest is the cauldron's own
/// (its name), not the merge's: the comparison leaves it out and the undo
/// leaves it as it is.
fn renamed_only(c: &env::Cauldron, snap: &Path) -> bool {
    let rel = env::cauldron::CAULDRON_MANIFEST;
    let (Ok(then), Ok(now)) = (std::fs::read_to_string(snap.join(crate::snapshot::FILES).join(rel)), std::fs::read_to_string(env::private::data_dir(c).join(rel))) else {
        return false;
    };
    then != now && manifest_without_name(&then) == manifest_without_name(&now)
}

/// The record an undo of `merge` left in its snapshot (`undo.json`), if
/// it was undone.
fn undone(c: &env::Cauldron, merge: &str) -> Option<Value> {
    // The newest snapshot of that id only: a merge id is used again once a
    // merge is undone (the next merge counts from what the target holds).
    snapshots_of(c, merge).last().and_then(|d| std::fs::read_to_string(d.join(UNDO)).ok()).and_then(|t| serde_json::from_str(&t).ok())
}

/// **Where merge `merge` into `c` stands** (`cauldron merge --status`),
/// read only: `planned` (a directory, no journal: cut short before it began
/// landing), `landing` (cut short after its journal: `cauldron soup
/// recover` finishes it), `abandoned`, `restored` (recover put the target
/// back), `merged` (finished; undoable or not, and why), `undoing` (an undo
/// cut short: the same undo, recover or the next writer finishes it, with
/// how far it got), `undone`, or `unknown`.
pub fn status(c: &env::Cauldron, merge: &str) -> Result<Value> {
    if merge_serial(merge).is_none() {
        refuse!(Code::Usage, "REFUSED: `{merge}` is not a merge id (MG-<n>).");
    }
    let mdir = dir(c).join(merge);
    let mut doc = json!({"env": c.env, "cauldron": c.name, "merge": merge});
    let read = |p: &Path| -> Option<Value> { std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str(&t).ok()) };
    let journal = read(&mdir.join(JOURNAL));
    let record = read(&mdir.join(RECORD));
    let snaps = snapshots_of(c, merge);
    doc["snapshots"] = json!(snaps.iter().map(|p| p.display().to_string()).collect::<Vec<_>>());
    if let Some(u) = undo_pending(c)?.filter(|u| u["merge"] == json!(merge)) {
        doc["state"] = json!("undoing");
        doc["stores"] = end_state(c, merge);
        doc["undo"] = json!({"began": u["at"], "by": u["by"], "step": u["state"], "snapshot": u["snapshot"]});
        let (soup_held, dumps_held) = (halted_with(&c.soup(), u["hold"].as_str().unwrap_or(""))?, halted_with(&dumps::dir(c), u["hold"].as_str().unwrap_or(""))?);
        doc["undo"]["soup"] = json!(if soup_held { "held by the undo, not put back yet" } else if u["state"] == "restoring" { "put back (or not held)" } else { "not held" });
        doc["undo"]["dumps"] = json!(if dumps_held { "held by the undo, not put back yet" } else if u["state"] == "restoring" { "put back (or not held)" } else { "not held" });
        doc["note"] = json!(format!(
            "an undo of {merge} was cut short ({}); nothing else of this release writes to cauldron `{}` until it is finished: \
             run `cauldron merge --undo {merge} --into {} --env {}` or `cauldron soup recover --env {} --cauldron {}`",
            u["state"].as_str().unwrap_or("?"),
            c.name,
            c.name,
            c.env,
            c.env,
            c.name
        ));
        return Ok(doc);
    }
    if record.is_none() && journal.as_ref().is_none_or(|j| j["state"] != "landing") {
        if let Some(u) = undone(c, merge) {
            doc["state"] = json!("undone");
            doc["undo"] = json!({"at": u["finished_at"].as_str().or(u["at"].as_str()), "by": u["by"], "kept": u["restored"]["kept"], "aside": u["restored"]["aside"]});
            return Ok(doc);
        }
    }
    match (&journal, &record) {
        (Some(j), _) if j["state"] == "landing" => {
            doc["state"] = json!("landing");
            doc["note"] = json!(format!(
                "merge {merge} was cut short after it began landing; `cauldron soup recover --env {} --cauldron {}` finishes it \
                 (or abandons it, if it was cut before it held the cauldron)",
                c.env, c.name
            ));
        }
        (Some(j), _) if j["state"] == "abandoned" => {
            doc["state"] = json!("abandoned");
            doc["stores"] = end_state(c, merge);
        }
        (_, Some(r)) => {
            // What the stores hold, not only that a record is there.
            let held = end_state(c, merge);
            doc["state"] = json!(if held["state"] == "merged" { "merged" } else { "torn" });
            if held["state"] != "merged" {
                doc["note"] = json!(format!(
                    "the record of merge {merge} is here, but the cauldron does not hold the whole merge. {}.{}",
                    held["note"].as_str().unwrap_or(""),
                    held["repair"].as_str().map(|r| format!(" What you can do: {r}")).unwrap_or_default()
                ));
            }
            doc["stores"] = held;
            doc["merged"] = json!({"at": r["finished_at"], "by": r["by"], "records": r["records"], "source": r["source"]["cauldron"], "id_map": r["id_map"]});
            // Under the cauldron's name now (a rename since the merge
            // changes it).
            // Offered only while a snapshot of it is there to undo from.
            if !snaps.is_empty() {
                doc["undo_command"] = json!(format!("cauldron merge --undo {merge} --into {} --env {}", c.name, c.env));
            }
        }
        (Some(j), None) if j["state"] == "done" => {
            // Finished, but its record is gone: what the stores hold says
            // where it stands.
            let held = end_state(c, merge);
            doc["state"] = json!(if held["state"] == "before-merge" { "unknown" } else { "partial" });
            doc["note"] = json!(format!(
                "merge {merge} finished, but its record ({}) is not there: it cannot be undone. The cauldron holds {}; {}.",
                mdir.join(RECORD).display(),
                held["soup"].as_str().unwrap_or(""),
                held["dumps"].as_str().unwrap_or("")
            ));
            doc["stores"] = held;
        }
        (None, None) if mdir.is_dir() => {
            doc["state"] = json!("planned");
            doc["note"] = json!("cut short before its journal: nothing of it landed; `cauldron soup recover` sweeps what it staged");
        }
        _ => {
            if snaps.iter().any(|d| d.join("abandoned").exists() || std::fs::read_dir(d).into_iter().flatten().flatten().any(|e| e.file_name().to_string_lossy().starts_with("abandoned-"))) {
                doc["state"] = json!("restored");
            } else {
                // No journal and no record, but the stores may still hold
                // what of it landed (its journal set aside or removed).
                let held = end_state(c, merge);
                if held["state"] == "before-merge" {
                    refuse!(Code::NotFound, "REFUSED: cauldron `{}` holds no merge {merge}.", c.name);
                }
                doc["state"] = json!("partial");
                doc["note"] = json!(format!(
                    "neither the journal nor the record of merge {merge} is here, but the cauldron holds what of it landed ({}; {}): \
                     it cannot be undone, and merging the source again carries only what is not here",
                    held["soup"].as_str().unwrap_or(""),
                    held["dumps"].as_str().unwrap_or("")
                ));
                doc["stores"] = held;
            }
        }
    }
    Ok(doc)
}

/// **Finish an undo cut short, if there is one** (`cauldron soup recover`,
/// and every writer of this release before it writes): with the soup's
/// writer lock held by the caller when `holding_soup`, else the dumps
/// store's; the other is taken here (refused, naming recover, when it is
/// busy). Answers what it did, or `None`.
pub fn finish_undo(c: &env::Cauldron, holding_soup: bool, finished_by: &str) -> Result<Option<Value>> {
    let Some(m) = undo_pending(c)? else { return Ok(None) };
    let _other = if holding_soup {
        dumps::lock_for_merge(c, &format!("cli {} merge-undo", std::process::id())).map(Some)
    } else {
        crate::lock::take_writer_lock(c, VERB).map(Some)
    }
    .map_err(|e| {
        Error::new(
            Code::NotAtRest,
            format!(
                "REFUSED: an undo of merge {} into cauldron `{}` was cut short, and it could not be finished now ({}). Nothing \
                 else may write to the cauldron until it is: run `cauldron soup recover --env {} --cauldron {}`. Nothing was \
                 written.",
                m["merge"].as_str().unwrap_or("?"),
                c.name,
                e.message,
                c.env,
                c.name
            ),
        )
    })?;
    resume_undo(c, m, finished_by).map(Some)
}

/// The merges recorded in the target (each `merges/MG-<n>/merge.json`),
/// oldest first.
pub fn recorded(c: &env::Cauldron) -> Vec<Value> {
    let mut out: Vec<(u64, Value)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir(c)) {
        for e in rd.flatten() {
            let Some(n) = merge_serial(&e.file_name().to_string_lossy()) else { continue };
            if let Some(v) = std::fs::read_to_string(e.path().join(RECORD)).ok().and_then(|t| serde_json::from_str(&t).ok()) {
                out.push((n, v));
            }
        }
    }
    out.sort_by_key(|x| x.0);
    out.into_iter().map(|x| x.1).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_tokens_only_are_rewritten() {
        let map = BTreeMap::from([("R-1".to_string(), "R-7".to_string()), ("R-12".to_string(), "R-9".to_string())]);
        assert_eq!(retoken("Outside: R-1 and R-12, not R-123 or XR-1.", &map), ("Outside: R-7 and R-9, not R-123 or XR-1.".to_string(), 2));
    }

    #[test]
    fn links_to_the_source_s_records_become_labels() {
        let ids = BTreeSet::from(["IS-1".to_string()]);
        let here = BTreeMap::from([("IS-3".to_string(), "IS-7".to_string())]);
        assert_eq!(relabel_links("see [[IS-1]], [[IS-2]] and [[IS-3]]", &ids, &here), "see [[from:IS-1]], [[IS-2]] and [[IS-7]]");
    }

    #[test]
    fn links_are_rewritten_in_one_pass() {
        let map = BTreeMap::from([("from:IS-1".to_string(), "IS-9".to_string()), ("from:T-2".to_string(), "T-4".to_string())]);
        assert_eq!(relink("see [[from:IS-1]], [[from:T-2]], [[from:X-1]] and [[unclosed", &map), "see [[IS-9]], [[T-4]], [[from:X-1]] and [[unclosed");
        // Replacing one label never rewrites what another put in.
        let map = BTreeMap::from([("a".to_string(), "b".to_string()), ("b".to_string(), "c".to_string())]);
        assert_eq!(relink("[[a]] [[b]]", &map), "[[b]] [[c]]");
    }

    #[test]
    fn a_merge_id_is_mg_and_a_number() {
        assert_eq!(merge_serial("MG-3"), Some(3));
        assert_eq!(merge_serial("MG-"), None);
        assert_eq!(merge_serial("MG-x"), None);
        assert_eq!(merge_serial("other"), None);
    }
}
