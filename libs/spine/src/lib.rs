//! `spine` — the transaction spine, and ladle's write and bulk-read forms.
//!
//! A CAULDRON is a directory of SQLite database files: one file per POOL
//! plus one `spine.db` — the central pending-transactions record, named
//! THE TRANSACTION SPINE. There is exactly ONE write path — the door —
//! and it speaks ladle's write statement.
//!
//! # Idioms inherited from `soup`, deliberately (shared contract)
//!
//! Library first (no binary in this build); **no transaction handle ever
//! escapes the crate** (every connection is private, every public call
//! opens and closes its own transactions); bundled SQLite in WAL mode;
//! loud failure with structured payloads; nothing written beside the
//! database files. Cross-file commits do NOT use ATTACH: each pool part
//! lands as its own SQLite transaction, and the COMMIT POINT of the whole
//! design is the spine row flip. The storage engine's cross-file behaviour is
//! verified by forcing the failure (`tests/atomicity_kill.rs`), not assumed.
//!
//! # The frozen contract
//!
//! Every public signature here is a frozen contract. Every unimplemented
//! body fails loudly by name as [`error::SpineError::NotBuilt`] — no stub
//! pretends to work (`tests/smoke.rs` is the conformance check).
//! Interior mutability (locking around the private connections) is an
//! implementation choice; `submit(&self, ...)` is deliberately `&self` so the
//! handle can be shared across writer threads in the contention tests.

#[macro_use]
pub mod error;

pub mod checkers;
pub mod config;
pub mod delta;
pub mod envelope;
pub mod erase;
pub mod exec;
pub mod fault;
pub mod flags;
pub mod grammar_cauldron;
pub mod grammar_dump;
pub mod grammar_v6;
pub mod ladle;
pub mod locks;
pub mod migrate;
pub mod offline;
pub mod pool;
pub mod project;
pub mod read;
pub mod recover;
pub mod regions;
pub mod schema_def;
pub mod schema_publish;
pub mod spine_db;
pub mod tranche;
pub mod user_grammar;
pub mod user_registry;
pub mod witness;

use crate::config::SpineConfig;
use crate::envelope::{Bindings, Envelope, TxHash};
use crate::error::{Result, SpineError};
use crate::pool::Pool;
pub use crate::recover::{LiftedQuiesce, Quiesce, QuiesceError, RecoveryReport, TranchePassReport};
use crate::schema_def::{PoolGrammar, PoolKind, PoolName};
pub use crate::spine_db::Quiescer;
use crate::spine_db::{CommittedCommit, Seq, SpineDb, SPINE_FILE};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A handle on one cauldron directory (the cauldron is a directory of
/// database files; the layout is fixed — `<pool>.db` per pool plus
/// `spine.db`, nothing else).
///
/// # It holds NO open connection, deliberately
///
/// Interior mutability was left open and the answer is:
/// there is none, because there is no interior state. `Cauldron` carries the
/// root path, the configuration and the pinned grammars — all `Send + Sync`
/// — and EVERY call opens the SQLite connections it needs and drops them.
///
/// Three reasons, in order of weight. (1) `submit(&self)` is `&self` so the
/// handle can be shared across writer threads; a `rusqlite::Connection` is
/// `Send` but NOT `Sync`, so a stored connection would make `Cauldron`
/// un-shareable and the contention tests unwritable. (2) The alternative —
/// a `Mutex<SpineDb>` — would have to be held across `locks::acquire`'s
/// bounded polling, which serializes every in-process writer and deadlocks
/// the moment one thread waits for a lock only another thread can release.
/// (3) The locks are TABLES in `spine.db`, so
/// connection-per-call makes two threads behave exactly like two processes,
/// which is the case the arbitration exists for and the case the tests must
/// exercise. The price is one file open per phase, paid on a path that
/// already writes the spine file twice per commit (the recovery record's
/// honest price); it is reported, not hidden.
///
/// `Clone`: the handle is data — a path, a config, the pinned
/// grammars, the open-time report — so the scheduler's lander thread takes
/// its own copy (`tranche_tick` spawns it and returns).
#[derive(Clone)]
pub struct Cauldron {
    root: PathBuf,
    config: SpineConfig,
    /// Every pool's pinned grammar, read once at create/open. Data, not a
    /// handle — the grammar is what every part validates under
    /// and it cannot change while a cauldron is open (no migration machinery
    /// in this build; a pin that moves under the handle is refused by name
    /// before the decided flip — `exec::check_pins_and_occupancy`).
    grammars: BTreeMap<PoolName, PoolGrammar>,
    /// What the recovery pass that runs inside [`Cauldron::open`] did.
    /// Empty for a handle from
    /// [`Cauldron::create`] (a fresh cauldron has nothing to recover) and
    /// for one from the test-only [`Cauldron::open_unrecovered`].
    recovered_at_open: RecoveryReport,
    /// What THE TRANCHE PASS inside [`Cauldron::open`] found and did: every
    /// non-terminal tranche, its reset deadline, its
    /// position, whether its overlay was rebuilt. Empty for a created
    /// cauldron and for the test-only unrecovered open.
    tranches_at_open: TranchePassReport,
    /// The guard asked about every commit's regions
    /// ([`regions::RegionGuard`]), when the writer installed one.
    region_guard: Option<std::sync::Arc<dyn regions::RegionGuard>>,
    /// Each user pool file's identity (device, inode) when this handle
    /// judged it (open, or `add_pool`): a commit touching the pool refuses
    /// if the file at that path is another one now (renamed in, restored
    /// over), since only the open judged and bound it.
    user_files: BTreeMap<PoolName, (u64, u64)>,
    /// THE HOLD this handle owns ([`Cauldron::hold`]), if any, and whether
    /// its next commit lifts it.
    hold: Option<String>,
    lift_hold: bool,
}

/// A file's (device, inode), or `None` when it is not there.
pub(crate) fn file_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

impl Cauldron {
    /// Install `guard`: from now on every commit through this handle names
    /// its regions to it first ([`regions`]), and a refusal writes nothing.
    pub fn set_region_guard(&mut self, guard: std::sync::Arc<dyn regions::RegionGuard>) {
        self.region_guard = Some(guard);
    }

    /// The installed region guard, if any.
    pub fn region_guard(&self) -> Option<&std::sync::Arc<dyn regions::RegionGuard>> {
        self.region_guard.as_ref()
    }

    /// Create a cauldron directory: `spine.db` (with lock tables) plus one
    /// pool file per grammar, each grammar pinned into its pool.
    /// Refuses an existing directory with database files, a duplicate pool
    /// name, and the reserved pool name `spine` (the projected namespace).
    pub fn create(dir: &Path, grammars: &[PoolGrammar], config: SpineConfig) -> Result<Cauldron> {
        config.check()?;
        if grammars.is_empty() {
            return Err(SpineError::usage(
                "a cauldron is created with at least one pool grammar; an empty cauldron has no \
                 door to speak to (a pool carries a pinned grammar)",
            ));
        }
        // The grammar set is judged BEFORE anything is written: duplicates and
        // the reserved name refuse rather than half-build a directory.
        let mut pinned: BTreeMap<PoolName, PoolGrammar> = BTreeMap::new();
        for g in grammars {
            g.check_wellformed()?;
            if pinned.insert(g.pool.clone(), g.clone()).is_some() {
                return Err(SpineError::usage(format!(
                    "cauldron create names the pool `{}` twice; one pool, one file, one grammar",
                    g.pool
                )));
            }
        }
        if dir.exists() {
            if !dir.is_dir() {
                return Err(SpineError::usage(format!(
                    "`{}` exists and is not a directory; a cauldron IS a directory",
                    dir.display()
                )));
            }
            for entry in read_dir(dir)? {
                if entry.extension().and_then(|e| e.to_str()) == Some("db") {
                    return Err(SpineError::usage(format!(
                        "refusing to create a cauldron over `{}`: it already holds `{}`",
                        dir.display(),
                        entry.display()
                    )));
                }
            }
        } else {
            std::fs::create_dir_all(dir).map_err(|e| {
                SpineError::hard(format!("cannot create cauldron `{}`: {e}", dir.display()))
            })?;
        }

        // A cauldron that opens is a cauldron with every file (soup idiom,
        // inherited): a failure anywhere removes what this call created, so
        // there is never a half-built cauldron to open.
        let built = (|| -> Result<()> {
            SpineDb::create(&dir.join(SPINE_FILE))?;
            for g in grammars {
                Pool::create(dir, g)?;
            }
            Ok(())
        })();
        if let Err(e) = built {
            remove_db_file(&dir.join(SPINE_FILE));
            for g in grammars {
                remove_db_file(&Pool::path_in(dir, &g.pool));
            }
            return Err(e);
        }
        Ok(Cauldron {
            root: dir.to_path_buf(),
            config,
            grammars: pinned,
            recovered_at_open: RecoveryReport::default(),
            tranches_at_open: TranchePassReport::default(),
            region_guard: None,
            user_files: BTreeMap::new(),
            hold: None,
            lift_hold: false,
        })
    }

    /// **Add a pool to an existing cauldron**, as one additive act: a pool
    /// file for `grammar` is built whole in a staging directory inside the
    /// cauldron (`.adding-<pool>`, which no open reads: it is not a `*.db`
    /// file at the top), and then linked into place in one step, so a
    /// reader finds either no such pool or the whole of it. Nothing else in
    /// the cauldron is read or written: no spine row, no other pool, no
    /// counter. Answers `true` when it added the pool, `false` when the
    /// cauldron already has it pinned to exactly this grammar (including one
    /// another handle added meanwhile); a pool of that name pinned to any
    /// other grammar is refused.
    ///
    /// THE CALLER HOLDS THE CAULDRON'S WRITER: this is a write, and the one
    /// write the door does not arbitrate (two adders of one pool would race
    /// on the rename; the loser's file is never moved over the winner's,
    /// and it then finds the pool added). A crash at any point leaves either
    /// no pool and a staging directory (which the next add removes and
    /// builds again; an open never reads it) or the whole pool.
    ///
    /// THE CLUSTER POOL, OR A USER GRAMMAR: the one built-in grammar a
    /// cauldron may gain after it was made is `cauldron@1` under its own
    /// name, `cauldron`
    /// ([`crate::grammar_cauldron::cauldron_v1`] of
    /// [`crate::grammar_cauldron::POOL`]); any other grammar or name is
    /// refused, so this is not a second, general door to the pool set
    /// (`Cauldron::create` makes every other pool). A user grammar (a kind
    /// with a hyphen) is taken when [`crate::user_grammar::check_user`]
    /// finds nothing against the pins held now (the caller judges the FILE
    /// with [`crate::user_grammar::parse_file`] first). A staging directory left
    /// by an add that died after its rename (a second name for the live pool
    /// file) is removed whenever the pool is found already in place.
    pub fn add_pool(&mut self, grammar: &PoolGrammar) -> Result<bool> {
        grammar.check_wellformed()?;
        let name = grammar.pool.clone();
        let only = crate::grammar_cauldron::cauldron_v1(crate::grammar_cauldron::POOL);
        // A USER grammar (its kind has a hyphen, which no built-in kind has)
        // may join too, when it meets every rule of
        // [`crate::user_grammar::check_user`] against the pins this cauldron
        // holds now AND the user pools it registered whose files are out
        // (their kinds, types and prefixes stay taken), and takes no id
        // prefix another pool has minted under. Its pool file is pinned
        // exactly as a built-in pool is, so an older binary reads it as a
        // pool; the spine records it in `crate::user_registry`.
        if crate::user_grammar::is_user_kind(grammar.kind.as_str()) {
            let held = self.held_and_registered()?;
            let pins: Vec<&PoolGrammar> = held.iter().collect();
            let mut problems = crate::user_grammar::check_user(&crate::user_grammar::UserGrammar::from_pin(grammar), &pins);
            problems.extend(crate::user_grammar::history_problems(&self.root, grammar)?);
            if let Some(first) = problems.first() {
                return Err(SpineError::usage(format!(
                    "user grammar `{}@{}` cannot be added as pool `{name}`: {} problem(s), the first `{}` at `{}`: {}",
                    grammar.kind,
                    grammar.version,
                    problems.len(),
                    first.rule,
                    first.at,
                    first.detail
                )));
            }
        } else if grammar != &only {
            return Err(SpineError::usage(format!(
                "a pool can be added to an existing cauldron only as `{}` pinned to `{}` v{} (the cluster pool); `{name}` \
                 pinned to `{}` v{} is not one, and is made only with the cauldron",
                only.pool, only.kind, only.version, grammar.kind, grammar.version
            )));
        }
        if let Some(pinned) = self.grammars.get(&name) {
            return if pinned == grammar {
                self.clear_staging(&name);
                Ok(false)
            } else {
                Err(SpineError::usage(format!(
                    "this cauldron already has a pool `{name}` pinned to `{}` v{}; it is not replaced by `{}` v{}",
                    pinned.kind, pinned.version, grammar.kind, grammar.version
                )))
            };
        }
        let target = Pool::path_in(&self.root, &name);
        // A pool name this spine has written to, whose file is gone, is not
        // made again empty: its ids would be minted again from 1, and an
        // edge another pool holds into one of its old records would read
        // as one into a new record.
        if !target.exists()
            && crate::user_grammar::is_user_kind(grammar.kind.as_str())
            && crate::offline::pool_has_history(&self.root, name.as_str())?
        {
            return Err(SpineError::usage(format!(
                "{}: this soup's spine records writes to a pool `{name}` whose file is no longer in the soup, so it is not \
                 made again as a new, empty pool (its ids would be minted again, and edges other pools hold into its old \
                 records would point at new ones). Put back the pool file this cauldron wrote: a copy older than its last write is taken by `cauldron soup grammar rebind {name}`, which records the records it lacks as lost (after which, if it holds no record, it can be removed and added again)",
                crate::user_grammar::POOL_HAS_HISTORY
            )));
        }
        if !target.exists() {
            let staging = self.stage_pool(grammar)?;
            let built = Pool::path_in(&staging, &name);
            // Never over a file that appeared meanwhile: link, then unlink
            // the staged name, so an existing target refuses the link.
            match std::fs::hard_link(&built, &target) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => {
                    return Err(SpineError::hard(format!(
                        "moving the new pool file into `{}`: {e}",
                        target.display()
                    )))
                }
            }
            std::fs::File::open(&self.root).and_then(|d| d.sync_all()).map_err(|e| {
                SpineError::hard(format!("syncing `{}` after adding pool `{name}`: {e}", self.root.display()))
            })?;
            let _ = std::fs::remove_dir_all(&staging);
        } else {
            self.clear_staging(&name);
        }
        // The pool on disk, whoever put it there, must be this grammar's.
        let found = Pool::open(&target)?;
        if found.grammar() != grammar {
            return Err(SpineError::usage(format!(
                "pool file `{}` holds `{}` v{}, not the `{}` v{} asked for; refusing rather than guessing",
                target.display(),
                found.grammar().kind,
                found.grammar().version,
                grammar.kind,
                grammar.version
            )));
        }
        drop(found);
        if crate::user_grammar::is_user_kind(grammar.kind.as_str()) {
            let spine = self.spine()?;
            crate::user_registry::register(spine.conn(), grammar)?;
            if let Some(id) = file_identity(&target) {
                self.user_files.insert(name.clone(), id);
            }
        }
        let added = !self.grammars.contains_key(&name);
        self.grammars.insert(name, grammar.clone());
        Ok(added)
    }

    /// The pins of the pool files this handle opened, and the grammars of
    /// the user pools the spine registered whose files are not in the soup
    /// (`crate::user_registry::registered_grammars`): what a new user
    /// grammar is judged against.
    pub fn held_and_registered(&self) -> Result<Vec<PoolGrammar>> {
        let mut out: Vec<PoolGrammar> = self.grammars.values().cloned().collect();
        let spine = self.spine()?;
        for g in crate::user_registry::registered_grammars(spine.conn())? {
            if !self.grammars.contains_key(&g.pool) {
                out.push(g);
            }
        }
        Ok(out)
    }

    /// **Remove a user pool** (`cauldron soup grammar remove`): only one
    /// whose file holds no record, which no other pool's grammar reaches,
    /// and that this spine registered. Its file (and log) is moved to
    /// `<soup>/.removed/<pool>.<n>/`, never deleted; the registry keeps the
    /// name as removed. No id is minted twice afterwards: a pool with no
    /// record minted none, and a prefix another pool minted under is never
    /// taken (`user_grammar::PREFIX_HAS_HISTORY`). With `apply` false,
    /// judges and answers what it would do, moving nothing.
    pub fn remove_user_pool(&mut self, pool: &PoolName, apply: bool) -> Result<serde_json::Value> {
        let spine = self.spine()?;
        let reg = crate::user_registry::read(spine.conn())?.unwrap_or_default();
        let Some(row) = reg.get(pool.as_str()).filter(|r| r.is_live()) else {
            return Err(SpineError::not_found(format!("this soup has no registered user pool `{pool}` (`cauldron soup grammar list`)")));
        };
        let file = Pool::path_in(&self.root, pool);
        let records = if file.exists() { crate::user_registry::record_count(&file)? } else { 0 };
        let landed: i64 = spine.conn().query_row("SELECT count(*) FROM tx_parts WHERE pool = ?1 AND landed = 1", [pool.as_str()], |r| r.get(0))?;
        if records > 0 || (!file.exists() && landed > 0) {
            return Err(SpineError::usage(format!(
                "user pool `{pool}` holds records ({}), so it is not removed: a record is never deleted, and its id must keep \
                 naming it. A grammar with a mistake that has records stays; write a corrected grammar as a new kind with its \
                 own prefixes",
                if file.exists() { format!("{records}") } else { format!("its file is out, and the spine records {landed} write(s) to it") }
            )));
        }
        let kind = row.grammar.kind.clone();
        for g in self.held_and_registered()? {
            if g.pool == *pool {
                continue;
            }
            let reaches = g.edge_kinds.iter().find(|(_, e)| e.to.iter().any(|t| matches!(t, crate::schema_def::TypeRef::Reach { kind: k, .. } if *k == kind)));
            if let Some((edge, _)) = reaches {
                return Err(SpineError::usage(format!(
                    "pool `{}`'s edge `{edge}` reaches kind `{kind}`, so `{pool}` is not removed while `{}` is registered",
                    g.pool, g.pool
                )));
            }
        }
        let mut doc = serde_json::json!({"pool": pool.as_str(), "schema": format!("{}@{}", row.kind, row.version), "records": 0});
        if !apply {
            doc["outcome"] = serde_json::json!("would-remove");
            return Ok(doc);
        }
        let mut moved = None;
        if file.exists() {
            let base = self.root.join(".removed");
            std::fs::create_dir_all(&base).map_err(|e| SpineError::hard(format!("making `{}`: {e}", base.display())))?;
            let mut n = 1;
            let to = loop {
                let d = base.join(format!("{pool}.{n}"));
                if std::fs::create_dir(&d).is_ok() {
                    break d;
                }
                n += 1;
                if n > 10_000 {
                    return Err(SpineError::hard(format!("no free name under `{}`", base.display())));
                }
            };
            for side in ["", "-wal", "-shm"] {
                let from = PathBuf::from(format!("{}{side}", file.display()));
                if from.exists() {
                    let dest = to.join(format!("{pool}.db{side}"));
                    std::fs::rename(&from, &dest).map_err(|e| SpineError::hard(format!("moving `{}`: {e}", from.display())))?;
                }
            }
            moved = Some(to);
        }
        crate::user_registry::mark_removed(spine.conn(), pool.as_str())?;
        self.grammars.remove(pool);
        self.user_files.remove(pool);
        doc["outcome"] = serde_json::json!("removed");
        doc["moved_to"] = serde_json::json!(moved.map(|p| p.display().to_string()));
        Ok(doc)
    }

    /// Refused when a user pool file `names` holds is not the file this
    /// handle judged (`user_files`): it was renamed in or restored over
    /// while the soup was open. Nothing is written.
    pub(crate) fn check_user_files(&self, names: &[PoolName]) -> Result<()> {
        for p in names {
            let Some(want) = self.user_files.get(p) else { continue };
            let path = Pool::path_in(&self.root, p);
            if file_identity(&path).as_ref() != Some(want) {
                return Err(SpineError::hard(format!(
                    "user pool file `{}` is not the file this soup was opened with (it was moved, replaced or restored while \
                     the soup was open); nothing was written. Close and open the soup again (`cauldron --env <env> stop mcp` \
                     and `start mcp` for the service), so the file is judged and bound before anything writes to it",
                    path.display()
                )));
            }
        }
        Ok(())
    }

    /// The live registered user pools whose files are not in the soup
    /// (moved out): their grammars.
    pub fn missing_user_pools(&self) -> Result<Vec<PoolGrammar>> {
        let spine = self.spine()?;
        Ok(crate::user_registry::registered_grammars(spine.conn())?.into_iter().filter(|g| !self.grammars.contains_key(&g.pool)).collect())
    }

    /// The staging half of [`Cauldron::add_pool`]: the whole pool file built
    /// in `.adding-<pool>` (a stale one, left by an add that died, removed
    /// first), and the directory's path. `pub` ONLY under `test-support`, so
    /// a test can stop exactly where a crash before the rename would.
    #[cfg(feature = "test-support")]
    pub fn stage_pool(&self, grammar: &PoolGrammar) -> Result<PathBuf> {
        self.stage_pool_interior(grammar)
    }
    #[cfg(not(feature = "test-support"))]
    fn stage_pool(&self, grammar: &PoolGrammar) -> Result<PathBuf> {
        self.stage_pool_interior(grammar)
    }

    /// Remove `.adding-<name>` if an add that died left it (best effort: no
    /// open reads it, so a failure here leaves nothing wrong).
    fn clear_staging(&self, name: &PoolName) {
        let staging = self.root.join(format!(".adding-{name}"));
        if staging.exists() {
            let _ = std::fs::remove_dir_all(&staging);
        }
    }

    fn stage_pool_interior(&self, grammar: &PoolGrammar) -> Result<PathBuf> {
        let staging = self.root.join(format!(".adding-{}", grammar.pool));
        if staging.exists() {
            std::fs::remove_dir_all(&staging).map_err(|e| {
                SpineError::hard(format!("removing the staging left by an earlier add, `{}`: {e}", staging.display()))
            })?;
        }
        std::fs::create_dir(&staging)
            .map_err(|e| SpineError::hard(format!("creating `{}`: {e}", staging.display())))?;
        Pool::build_whole(&staging, grammar)?;
        Ok(staging)
    }

    /// Open an existing cauldron directory, discovering its pools by their
    /// files and loading each pinned grammar — and then RUN RECOVERY
    /// over every un-cleared row, so a transaction stranded by a dead
    /// process heals on the next open. What the pass did is kept on the
    /// handle, [`Cauldron::recovered_at_open`]; a pass that finished
    /// something also says so on stderr, because a stranger's commit
    /// finished quietly is the kind of silence this crate refuses.
    ///
    /// Safe beside live doors by arbitration: every flip is
    /// conditional, so a second process opening while a first is
    /// mid-commit costs at most one spurious restart of an un-decided row
    /// (`recover::recover`'s docs). A recovery that cannot complete — a
    /// decided row whose record has been altered, storage that refuses —
    /// refuses the OPEN, loudly, with that error: fail-closed, the posture
    /// the halt has; a handle on an un-recoverable record would only
    /// hand every door a `Busy` behind the stranded row.
    pub fn open(dir: &Path, config: SpineConfig) -> Result<Cauldron> {
        let mut cauldron = Self::open_interior(dir, config, spine_db::SPINE_SCHEMA_VERSION)?;
        let report = recover::recover(&cauldron)?;
        // THE TRANCHE PASS, after the ordinary pass and
        // before the door: deadlines reset to boot + timeout, overlays
        // rebuilt, positions recomputed — it lands nothing (the first
        // `tranche_tick` does). `draining` is left as found.
        let tranches = recover::tranche_pass(&cauldron, tranche::wall_ms())?;
        if !tranches.tranches.is_empty() {
            eprintln!(
                "spine: tranche pass at open of `{}`: {} live tranche(s) — {} (the first \
                 tick lands what was landing)",
                dir.display(),
                tranches.tranches.len(),
                tranches
                    .tranches
                    .iter()
                    .map(|t| format!(
                        "{} attempt {} {}{} position {}",
                        t.tranche,
                        t.attempt,
                        t.state.as_str(),
                        if t.draining { " draining" } else { "" },
                        t.position
                    ))
                    .collect::<Vec<_>>()
                    .join("; ")
            );
        }
        cauldron.tranches_at_open = tranches;
        let finished = report.rolled_forward.len() + report.cleared_only.len();
        if finished > 0 || !report.aborted.is_empty() {
            eprintln!(
                "spine: recovery at open of `{}`: {} stranded row(s) rolled forward, {} \
                 cleared, {} un-decided attempt(s) aborted",
                dir.display(),
                report.rolled_forward.len(),
                report.cleared_only.len(),
                report.aborted.len()
            );
        }
        cauldron.recovered_at_open = report;
        Ok(cauldron)
    }

    /// Open WITHOUT the recovery pass — the handle [`Cauldron::open`] used to
    /// return before open ran recovery. `pub` ONLY under `test-support`: the crate's own
    /// suites stage and inspect crash windows (a decided-and-un-cleared row,
    /// its locks still held) and recover them by hand at a chosen moment,
    /// which an open that heals would erase before any assertion could look.
    /// No production consumer can reach it; the only open they have heals.
    #[cfg(feature = "test-support")]
    pub fn open_unrecovered(dir: &Path, config: SpineConfig) -> Result<Cauldron> {
        Self::open_interior(dir, config, spine_db::SPINE_SCHEMA_VERSION)
    }

    /// THE TRANCHE PASS BY HAND, `pub` ONLY
    /// under `test-support`: on a handle from [`Cauldron::open_unrecovered`]
    /// a test runs the ordinary pass ([`Cauldron::recover`]), then acts —
    /// a LIVE ordinary door enrolling a queued row under its own real,
    /// un-cleared `transactions` seq, the concurrent enroller `open` cannot
    /// exclude — and only then runs this, the second half of `open`
    /// verbatim. That is the window between the two passes, held
    /// deterministically in one process with no race and no seam variable.
    /// No production consumer can reach it; their only open runs both
    /// passes back to back.
    #[cfg(feature = "test-support")]
    pub fn tranche_pass_by_hand(&self) -> Result<recover::TranchePassReport> {
        recover::tranche_pass(self, tranche::wall_ms())
    }

    /// THE OLDER-BINARY SIMULATION, `pub` ONLY under
    /// `test-support` and compiled out of every other build: open as a binary
    /// whose spine schema version is `binary_version` would — the schema
    /// fence judged against that version, and then the ordinary recovery
    /// pass run as a binary that does not know a tranche row from any other
    /// (no skip). Against a spine carrying tranches the fence refuses it by
    /// name; with the fence removed, that pass aborts the anchor.
    #[cfg(feature = "test-support")]
    pub fn open_as_older_binary(
        dir: &Path,
        config: SpineConfig,
        binary_version: u32,
    ) -> Result<(Cauldron, RecoveryReport)> {
        let cauldron = Self::open_interior(dir, config, binary_version)?;
        let report = recover::recover_pass(&cauldron, recover::TrancheRows::Unknown)?;
        Ok((cauldron, report))
    }

    /// The discovery half of `open`: files, pins — and the spine SCHEMA
    /// FENCE, whose only write is the in-place upgrade of a
    /// spine from before the tranche record (`SpineDb::fence_and_upgrade`).
    fn open_interior(dir: &Path, config: SpineConfig, binary_version: u32) -> Result<Cauldron> {
        config.check()?;
        if !dir.is_dir() {
            return Err(SpineError::not_found(format!(
                "no cauldron directory at `{}`",
                dir.display()
            )));
        }
        let spine_path = dir.join(SPINE_FILE);
        if !spine_path.exists() {
            return Err(SpineError::not_found(format!(
                "`{}` holds no `{SPINE_FILE}`; it is not a cauldron",
                dir.display()
            )));
        }
        // Probe it rather than assume it: a file that will not answer is
        // never treated as an empty spine (soup idiom). Then the fence: a
        // spine newer than this binary is refused by name before anything
        // reads a row of it.
        {
            let mut spine = SpineDb::open(&spine_path)?;
            spine.set_busy_timeout(config.busy_timeout)?;
            spine.fence_and_upgrade(binary_version)?;
        }

        // Pools are DISCOVERED by their files, and each one's pinned grammar
        // is loaded from the file itself — the pin is the pool's own record,
        // never a caller's claim about it.
        let mut grammars: BTreeMap<PoolName, PoolGrammar> = BTreeMap::new();
        for entry in read_dir(dir)? {
            if entry.extension().and_then(|e| e.to_str()) != Some("db") {
                continue; // skips `-wal` / `-shm` and anything else
            }
            if entry.file_name().and_then(|n| n.to_str()) == Some(SPINE_FILE) {
                continue;
            }
            let pool = Pool::open(&entry).map_err(|e| user_grammar::unreadable(&entry, e))?;
            let name = pool.name().clone();
            let expected = Pool::path_in(dir, &name);
            if expected != entry {
                return Err(SpineError::hard(format!(
                    "pool file `{}` holds pool `{name}`, whose file must be `{}`; refusing rather \
                     than guessing (the layout is `<pool>.db`)",
                    entry.display(),
                    expected.display()
                )));
            }
            grammars.insert(name, pool.grammar().clone());
        }
        if grammars.is_empty() {
            return Err(SpineError::hard(format!(
                "`{}` has a spine but no pool files; a cauldron with no pool has no door to speak \
                 to, and an empty answer here would hide a damaged directory",
                dir.display()
            )));
        }
        // A user grammar's pool file is judged, every time the soup is
        // opened, by the rules `add_pool` judged it by, and must be bound to
        // this spine (`user_grammar::judge_pins`, `offline::pool_bound`): a
        // hand edit or a file copied in is refused, never routed.
        {
            let pins: Vec<&PoolGrammar> = grammars.values().collect();
            if let Some(why) = user_grammar::refusal(dir, &pins)? {
                return Err(SpineError::hard(why));
            }
            // A registry row whose digest is of an earlier form, for a pin it
            // matches, is rewritten to this release's form (a migration).
            let spine = SpineDb::open(&spine_path)?;
            user_registry::migrate_digests(spine.conn(), &pins)?;
        }
        let user_files = grammars
            .keys()
            .filter(|p| !user_grammar::BUILT_IN_POOLS.contains(&p.as_str()) && user_grammar::is_user_kind(grammars[*p].kind.as_str()))
            .filter_map(|p| file_identity(&Pool::path_in(dir, p)).map(|id| (p.clone(), id)))
            .collect();
        Ok(Cauldron {
            root: dir.to_path_buf(),
            config,
            grammars,
            recovered_at_open: RecoveryReport::default(),
            tranches_at_open: TranchePassReport::default(),
            region_guard: None,
            user_files,
            hold: None,
            lift_hold: false,
        })
    }

    /// What the recovery pass inside [`Cauldron::open`] did — which rows it
    /// rolled forward, cleared, aborted, or left to a live door. Empty
    /// for a created cauldron and for the test-only unrecovered open.
    pub fn recovered_at_open(&self) -> &RecoveryReport {
        &self.recovered_at_open
    }

    /// What THE TRANCHE PASS inside [`Cauldron::open`] found and did: every
    /// non-terminal tranche in queue order, with its
    /// reset deadline (`boot + timeout`), derived position, `draining` as
    /// found, and whether its overlay was rebuilt. Empty for a created
    /// cauldron and for the test-only unrecovered open.
    pub fn tranches_at_open(&self) -> &TranchePassReport {
        &self.tranches_at_open
    }

    /// The one write door (SETTLED: exactly one write path).
    /// One statement, one commit, across any number of pools — lands
    /// atomically or refuses whole with witnesses.
    /// Envelope mode `Validate` is the dry run: same validator, nothing
    /// lands. A halted spine refuses everything
    /// ([`error::SpineError::Halted`], fail-closed).
    pub fn submit(&self, envelope: Envelope) -> Result<Outcome> {
        exec::execute(self, envelope)
    }

    /// The bulk read form: the same grammar with no mutation clauses,
    /// executed as a lock-free validated snapshot read — holding nothing,
    /// blocking nobody, escalating cold after the bound. Kept a SEPARATE
    /// entry from [`Cauldron::submit`] (a deliberate choice):
    /// reads carry no provenance and land no row, so folding them into the
    /// write door would stamp what it never records.
    pub fn read(&self, request: &ReadRequest) -> Result<ReadOutcome> {
        read::read_request(self, request)
    }

    /// Run recovery from the spine row alone: abort
    /// un-decided, roll decided un-landed parts FORWARD from the row's
    /// payload, never re-match text. Idempotent.
    pub fn recover(&self) -> Result<recover::RecoveryReport> {
        recover::recover(self)
    }

    /// Quiesce (the recovery hook): no new commits begin; in-flight commits
    /// finish; returns drained, with the [`Quiesce`] token that owns it. A
    /// quiesce is owned by THE CALL that took it, through its token: the
    /// cauldron stays quiesced until every outstanding
    /// token has been resumed, `Ok` means quiesced-by-you-and-drained, and
    /// the handle holds nothing — a token travels wherever its caller sends
    /// it. On `Err`, [`QuiesceError::standing`] carries the token if this
    /// call's quiescer still stands (the drain did not finish in time); the
    /// gate stays up until that token is resumed.
    pub fn quiesce(&self) -> std::result::Result<Quiesce, QuiesceError> {
        recover::quiesce(self)
    }

    /// Lift the quiesce this token was issued for. `Ok(true)`: lifted.
    /// `Ok(false)`: it no longer stood, nothing was lifted now, and any other
    /// party's quiesce stands. `Err`: nothing was lifted and the token is
    /// still the caller's to resume (`recover::resume`).
    pub fn resume(&self, token: &Quiesce) -> Result<bool> {
        recover::resume(self, token)
    }

    /// THE BY-HAND LIFT: lift EVERY outstanding quiesce, whoever took it —
    /// the named, loud repair for a quiescer whose process died holding its
    /// token. Never automatic: nothing in the crate calls this; the user
    /// does, with a `reason` that is required and recorded in what it
    /// announces. Reports each quiescer it lifted — holder and since when —
    /// and announces the same on stderr. `resume_all` with nothing standing
    /// lifts nothing and says so (an empty list), never an error: the state
    /// asked for is the state reached. See `recover::resume_all`.
    pub fn resume_all(&self, reason: &str) -> Result<LiftedQuiesce> {
        recover::resume_all(self, reason)
    }

    // ---- the spine exposures (cheaply readable facts the validated read
    //      is built on) — the two original ones, plus a THIRD ----

    /// The committed-commit counter — increments only when a commit's LAST
    /// part has landed.
    pub fn committed_commit_counter(&self) -> Result<u64> {
        self.spine()?.committed_commit_counter()
    }

    /// THE SERIAL COUNTERS of one pool: id prefix -> the highest
    /// serial the spine has minted or carried for it. A prefix with no row
    /// has minted nothing (its next serial is 1). The number a CREATE would
    /// draw next is `last + 1`; the published-record import's `remint`
    /// carries exactly those ids (`migrate::records`). Read-only.
    pub fn serial_counters(&self, pool: &PoolName) -> Result<BTreeMap<String, u64>> {
        let spine = self.spine()?;
        let conn = spine.conn();
        let has: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'id_serials'",
            [],
            |r| r.get(0),
        )?;
        let mut out = BTreeMap::new();
        if has == 0 {
            return Ok(out);
        }
        let mut stmt = conn.prepare("SELECT prefix, last FROM id_serials WHERE pool = ?1")?;
        let rows = stmt.query_map([pool.as_str()], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (prefix, last) = row?;
            out.insert(prefix, u64::try_from(last).unwrap_or(0));
        }
        Ok(out)
    }

    /// One commit's touched-pool set.
    pub fn touched_pools(&self, tx: &TxHash) -> Result<Vec<PoolName>> {
        self.spine()?.touched_pools(tx)
    }

    /// Transactions currently in landing state, queryable by pool
    /// (`None` = all). "By pool" filters on each landing's TOUCHED set —
    /// its delta-bearing parts (`spine_db::InLanding::touched`).
    pub fn in_landing(&self, pool: Option<&PoolName>) -> Result<Vec<spine_db::InLanding>> {
        self.spine()?.in_landing(pool)
    }

    /// THE THIRD EXPOSURE: the
    /// commits whose committed-at counter value lies in `(c_start, c_end]`
    /// and which TOUCH at least one of `pools` (have a delta-bearing part
    /// there). One indexed query.
    ///
    /// It exists because the validated read's check — "no commit in the
    /// window touched YOUR pools" — is unimplementable from the other two
    /// exposures: a scalar counter says how many commits landed, never
    /// which. The spine already owned the fact; this only makes it cheaply
    /// readable, which is all an exposure is asked to do.
    pub fn commits_touching(
        &self,
        pools: &[PoolName],
        c_start: u64,
        c_end: u64,
    ) -> Result<Vec<CommittedCommit>> {
        self.spine()?.commits_touching(pools, c_start, c_end)
    }

    // ---- the spine exposures the soup daemon needs (the test for every
    //      exposure: it EXPOSES A FACT THE SPINE ALREADY
    //      OWNS — no new state, no new write, one read of a record that
    //      already exists) ----

    /// One node hydrated whole — record, out-edges, `created_tx` — with the
    /// committed-commit cut it is consistent at, never below a commit it
    /// shows ([`read::node_view`]; the validated window, run even for one
    /// pool). `Ok(None)` when the node is absent; `NotFound` for an unknown
    /// pool or the projected `spine` namespace; `Busy` past `retry_bound`.
    pub fn node_view(&self, id: &pool::QualifiedNodeId) -> Result<Option<read::NodeView>> {
        read::node_view(self, id)
    }

    /// One spine row by sequence number, read back whole AS STORED — no
    /// filter on `committed_at` or on its lifecycle flags (the feed asks only
    /// for the seqs [`Cauldron::commits_touching`] named). A sequence number
    /// the spine never issued is `NotFound` naming it, never `Hard`.
    pub fn transaction(&self, seq: Seq) -> Result<spine_db::TxRow> {
        use rusqlite::OptionalExtension;
        let spine = self.spine()?;
        // Rows are kept forever, so "exists" read here cannot turn false
        // before the row read below.
        let exists: Option<i64> = spine
            .conn()
            .query_row(
                "SELECT 1 FROM transactions WHERE seq = ?1",
                rusqlite::params![seq.0],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(SpineError::not_found(format!(
                "the spine holds no transaction at seq {}",
                seq.0
            )));
        }
        spine.row_at(seq)
    }

    /// One spine row by hash identity: [`SpineDb::find_by_hash`]'s
    /// AUTHORITATIVE row (the decided row if there is one, else the
    /// un-cleared attempt, else the latest abort — `authoritative_seq`'s
    /// rule, by reference). `None` only for an identity with no row at all.
    pub fn transaction_by_hash(&self, hash: &TxHash) -> Result<Option<spine_db::TxRow>> {
        self.spine()?.find_by_hash(hash)
    }

    /// The fail-closed halted state, if entered: `Some(reason)` means effectively read-only — every
    /// submit refuses until the system is repaired out-of-band.
    pub fn halted(&self) -> Result<Option<String>> {
        self.spine()?.halted()
    }

    // ---- THE HOLD: one writer's work that spans several commits ----

    /// **Hold the cauldron** for a piece of work that lands as more than one
    /// commit (a merge: its dumps, then its records): the spine enters its
    /// durable halted state with `reason` as the halted reason, so every
    /// other writer is refused, by every version of this crate since the
    /// halt existed, naming `reason`, in this process and in any other, and
    /// across a crash. This handle's own writes pass the hold (it owns the
    /// reason); [`Cauldron::lift_hold_at_next_commit`] makes the commit that
    /// ends the work lift it in the same transaction as its decided flip,
    /// and [`Cauldron::release_hold`] lifts it outright. Reads are not held.
    ///
    /// Refused (`Usage`) when the spine is already halted for another
    /// reason; a hold already set with exactly `reason` (a crash left it) is
    /// adopted.
    pub fn hold(&mut self, reason: &str) -> Result<()> {
        if reason.trim().is_empty() {
            return Err(SpineError::usage("a hold needs a reason that says whose it is and what ends it"));
        }
        let mut spine = SpineDb::open(&self.root.join(SPINE_FILE))?;
        spine.set_busy_timeout(self.config.busy_timeout)?;
        if !spine.set_hold(reason)? {
            let other = spine.halted_raw()?.unwrap_or_default();
            return Err(SpineError::usage(format!("the spine is already halted for another reason ({other}); it is not held over that")));
        }
        self.hold = Some(reason.to_string());
        self.lift_hold = false;
        Ok(())
    }

    /// **Adopt a hold a crash left**: when the spine is halted with exactly
    /// `reason`, this handle owns it from now on (its writes pass it).
    /// Answers whether it was held so; a spine halted for another reason is
    /// left as it is and answers `false`.
    pub fn adopt_hold(&mut self, reason: &str) -> Result<bool> {
        let spine = self.spine()?;
        if spine.halted_raw()?.as_deref() == Some(reason) {
            self.hold = Some(reason.to_string());
            self.lift_hold = false;
            return Ok(true);
        }
        Ok(false)
    }

    /// The hold this handle owns, if any.
    pub fn held(&self) -> Option<&str> {
        self.hold.as_deref()
    }

    /// The next commit through this handle lifts its hold, in the same
    /// transaction as that commit's decided flip.
    pub fn lift_hold_at_next_commit(&mut self) {
        self.lift_hold = self.hold.is_some();
    }

    /// **Lift this handle's hold** now (when it is still set). Answers
    /// whether it was set.
    pub fn release_hold(&mut self) -> Result<bool> {
        let Some(reason) = self.hold.take() else { return Ok(false) };
        self.lift_hold = false;
        let mut spine = SpineDb::open(&self.root.join(SPINE_FILE))?;
        spine.set_busy_timeout(self.config.busy_timeout)?;
        spine.clear_hold(&reason)
    }

    /// This cauldron's configuration (config, never constants).
    pub fn config(&self) -> &SpineConfig {
        &self.config
    }

    /// Every pool in this cauldron, by name — the pinned grammars loaded at
    /// create/open. Read-only; nothing in this build changes a pin.
    pub fn pools(&self) -> impl Iterator<Item = &PoolName> {
        self.grammars.keys()
    }

    /// One pool's pinned grammar (each part validates under its
    /// own pool's pin; the spine itself is version-blind).
    pub fn grammar(&self, pool: &PoolName) -> Option<&PoolGrammar> {
        self.grammars.get(pool)
    }

    /// One pool's KIND — what another pool's reach names. `None`
    /// for a pool this cauldron does not hold.
    pub fn kind_of(&self, pool: &PoolName) -> Option<&PoolKind> {
        self.grammars.get(pool).map(|g| &g.kind)
    }

    /// Every pool of one kind, by name, in name order ("which pools are of
    /// kind K" — the question a reach by kind asks of the cauldron;
    /// two workspaces are two pools of kind `workspace`). Empty for a kind no
    /// pool declares, including the projected kind `spine`, which is no
    /// pool's and is answered by `project::projected_grammar` instead.
    pub fn pools_of_kind(&self, kind: &PoolKind) -> Vec<&PoolName> {
        self.grammars
            .iter()
            .filter(|(_, g)| g.kind == *kind)
            .map(|(name, _)| name)
            .collect()
    }

    // ---- crate interior: the connections every call opens and drops ----

    /// A fresh handle on `spine.db`. See the type's docs for why nothing is
    /// cached: one connection per call is what makes `&Cauldron` shareable
    /// across threads and what makes two threads behave like two processes.
    pub(crate) fn spine(&self) -> Result<SpineDb> {
        let spine = SpineDb::open(&self.root.join(SPINE_FILE))?;
        spine.set_busy_timeout(self.config.busy_timeout)?;
        Ok(spine.with_hold(self.hold.clone(), self.lift_hold))
    }

    /// A fresh handle on one pool file, refusing by name a pool this cauldron
    /// does not have (never an empty answer for a missing pool).
    pub(crate) fn open_pool(&self, pool: &PoolName) -> Result<Pool> {
        if !self.grammars.contains_key(pool) {
            return Err(SpineError::not_found(format!(
                "this cauldron has no pool `{pool}`"
            )));
        }
        let handle = Pool::open(&Pool::path_in(&self.root, pool))?;
        handle.set_busy_timeout(self.config.busy_timeout)?;
        Ok(handle)
    }

    /// The grammar lookup the ladle compiler and the claim derivation take
    /// (`&dyn Fn(&PoolName) -> Option<PoolGrammar>`), closed over this
    /// cauldron's pins.
    pub(crate) fn grammar_lookup(&self) -> impl Fn(&PoolName) -> Option<PoolGrammar> + '_ {
        move |p: &PoolName| self.grammars.get(p).cloned()
    }

    /// The by-KIND lookup the ladle compiler takes beside the by-name one
    /// (`&dyn Fn(&PoolKind) -> Vec<PoolGrammar>`): every pool of a kind with
    /// its pinned grammar, in name order — what a reach by kind is judged
    /// against.
    pub(crate) fn kind_lookup(&self) -> impl Fn(&PoolKind) -> Vec<PoolGrammar> + '_ {
        move |k: &PoolKind| {
            self.grammars
                .values()
                .filter(|g| g.kind == *k)
                .cloned()
                .collect()
        }
    }
}

/// One directory listing, sorted, with a loud failure rather than a silent
/// empty listing (soup idiom: a store that will not answer is never empty).
fn read_dir(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let entries = std::fs::read_dir(dir)
        .map_err(|e| SpineError::hard(format!("cannot read `{}`: {e}", dir.display())))?;
    for entry in entries {
        let entry =
            entry.map_err(|e| SpineError::hard(format!("cannot read `{}`: {e}", dir.display())))?;
        out.push(entry.path());
    }
    out.sort();
    Ok(out)
}

/// Remove a database file and its WAL siblings, best-effort — used only to
/// undo a half-built cauldron.
fn remove_db_file(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}

/// What a successful [`Cauldron::submit`] returns.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// The statement landed — one commit, one spine row.
    Committed {
        tx: TxHash,
        seq: Seq,
        /// The RETURN projection.
        returned: Returned,
        /// True when the hash identity had already landed and this is the
        /// ORIGINAL outcome replayed (exactly-once, SETTLED).
        deduplicated: bool,
    },
    /// Mode was `Validate`: the statement passed grammar and premises;
    /// nothing landed, no row written (one validator, two callers).
    Validated { tx: TxHash },
}

/// A bulk-read request: statement text restricted to
/// MATCH/WHERE/RETURN, parameters, and the envelope format version. No
/// provenance — reads land no row (a deliberate choice).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadRequest {
    pub statement: String,
    pub bindings: Bindings,
    pub format_version: u32,
}

/// What a bulk read returns, honest about how it completed.
#[derive(Debug, Clone)]
pub struct ReadOutcome {
    pub rows: Returned,
    /// Discarded-and-retried attempts before the consistent cut.
    pub retries: u32,
    /// Complete-claim-set attempts the escalation abandoned because the
    /// match set moved under them (the restart shape); zero unless
    /// escalated, and zero on an escalation whose first set stood.
    pub restarts: u32,
    /// True when the read escalated to real read locks (expected cold).
    pub escalated: bool,
}

/// RETURN projection rows: one map per row, column name (alias or source
/// text, frozen grammar) to JSON value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Returned(pub Vec<BTreeMap<String, serde_json::Value>>);
