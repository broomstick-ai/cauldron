//! **Offline access to a cauldron directory**: what a process that is
//! NOT the soup daemon needs before it touches a cauldron — a connection that
//! cannot write, and the question "is this spine at rest?".
//!
//! Why the question matters. [`crate::Cauldron::open`] is the only open a
//! production consumer has, and it runs the recovery pass and the tranche
//! pass before it returns: they roll forward, abort, sweep orphan locks and
//! reset tranche deadlines. That is right for the daemon that owns the
//! cauldron. A one-shot outside writer (`cauldron soup import`) should open
//! only a spine on which those passes have NOTHING to do, so the one write
//! it makes is its own commit. [`rest_check`] reads the spine file through a
//! read-only connection and names every reason the passes would act; an
//! empty answer means they will find nothing.

use crate::error::{Result, SpineError};
use crate::spine_db::{SPINE_FILE, SPINE_SCHEMA_VERSION};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

/// A connection to `path` that CANNOT write, and that creates nothing beside
/// the file.
///
/// A plain read-only open of a WAL database still creates `-wal` and `-shm`
/// beside it, so which open is used depends on whether a writer is there:
///
/// - with a `-wal` present, a writer's side files already exist and
///   `mode=ro` reads through them without creating any, taking part in
///   SQLite's own locking (a checkpoint waits for this reader);
/// - with no `-wal`, everything is in the main file. It is NOT read in place:
///   an `immutable=1` connection takes no lock, so a writer that opens the
///   file meanwhile, commits, and checkpoints its log back into the main
///   file as its last connection closes rewrites pages under a read in
///   flight. That read then sees a mix of old and new pages: SQLite answers
///   `database disk image is malformed`, or, worse, a wrong answer that
///   happens to parse. (Seen by readers hammering a store while a service
///   wrote to it.) So the file is first copied to a private file in a
///   directory made for this one read in the temporary directory, which
///   only its owner can enter and which is removed before the read returns
///   ([`ReadDir`]; a clone, on APFS: no data is copied), and the copy
///   is kept only if no `-wal` appeared and the original's identity (inode,
///   size, modification and change times) did not change across the copy;
///   the copy is opened `immutable=1` and unlinked at once, so the reader
///   holds a snapshot no writer can touch and leaves nothing behind. If the
///   file changed during the copy, it is tried again (a writer is now there,
///   so the next attempt usually reads through its log), a bounded number
///   of times; after that it is read `mode=ro`, which is correct but may
///   leave `-wal` and `-shm` beside the file.
pub fn read_only_connection(path: &Path) -> Result<Connection> {
    read_only_connection_in(path, &std::env::temp_dir())
}

/// [`read_only_connection`], with its private directories made in `base`.
fn read_only_connection_in(path: &Path, base: &Path) -> Result<Connection> {
    let wal = side_file(path, "-wal");
    for attempt in 0..SNAPSHOT_ATTEMPTS {
        if wal.exists() {
            return open_uri(path, "mode=ro");
        }
        if let Some(conn) = settled_copy(path, &wal, base)? {
            return Ok(conn);
        }
        std::thread::sleep(std::time::Duration::from_millis(1 + attempt as u64));
    }
    open_uri(path, "mode=ro")
}

/// How many times [`read_only_connection`] copies a file that changed during
/// the copy before it reads through SQLite's locking instead.
const SNAPSHOT_ATTEMPTS: usize = 20;

fn side_file(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(suffix);
    std::path::PathBuf::from(p)
}

/// What changes when a file is written: any write between two readings of
/// it changes its modification and change times.
fn identity(path: &Path) -> Result<(u64, u64, u64, i64, i64, i64, i64)> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path)
        .map_err(|e| SpineError::hard(format!("cannot open `{}` read-only: {e}", path.display())))?;
    Ok((m.dev(), m.ino(), m.size(), m.mtime(), m.mtime_nsec(), m.ctime(), m.ctime_nsec()))
}

/// A private copy of `path` taken while nothing wrote to it, opened
/// `immutable=1` and already unlinked; `None` when a write was seen during
/// the copy, or when no private directory can be had for the copy (then the
/// caller reads through SQLite's locking instead).
fn settled_copy(path: &Path, wal: &Path, base: &Path) -> Result<Option<Connection>> {
    let before = identity(path)?;
    let Some(dir) = ReadDir::new(base) else {
        return Ok(None);
    };
    let copy = dir.path.join("copy.db");
    if let Err(e) = std::fs::copy(path, &copy) {
        if !dir.path.is_dir() {
            // Our own directory went away under us: try again.
            return Ok(None);
        }
        return Err(SpineError::hard(format!("cannot open `{}` read-only (copying it to read: {e})", path.display())));
    }
    let settled = !wal.exists() && identity(path)? == before;
    if !settled {
        return Ok(None);
    }
    let conn = open_uri(&copy, "immutable=1");
    let conn = conn.map_err(|e| SpineError::hard(format!("cannot open `{}` read-only: {e}", path.display())))?;
    Ok(Some(conn))
    // `dir` drops here: the copy (already open) and the directory go.
}

/// The prefix of every directory a read makes for its private copy:
/// `spine-read-<pid>.<n>.<random>` in the temporary directory.
const READ_DIR_PREFIX: &str = "spine-read-";

/// The file in a read's directory whose lock says the read is alive.
const READ_DIR_LOCK: &str = ".lock";

/// A directory of the sweep's age with no lock file is a crash between its
/// `mkdir` and its lock, never a live read.
const UNLOCKED_STALE: std::time::Duration = std::time::Duration::from_secs(60);

/// **One read's private directory**, made for that read and removed when it
/// is dropped (on every return, error and unwind included), so a read leaves
/// nothing in the temporary directory and two reads never share one.
///
/// - Its name, `spine-read-<pid>.<n>.<random>`, is new: it is made
///   with a non-recursive create, mode 0700, which fails if anything is
///   already at that path (a name someone else made first is never used:
///   another name is tried), and it is used only while it is a real
///   directory (not a symlink) of this process's user that nobody else can
///   enter. The random part comes from the standard library's per-process
///   hash keys, so the name is not predictable from the pid and the clock.
/// - It holds a lock file, locked for the read's life. A process that dies
///   mid-read (SIGKILL, a crash) leaves its directory, and the OS drops the
///   lock with the process: the first read of every later process sweeps
///   every directory of this user whose lock it can take (or that has no
///   lock file and is older than a minute), so a leftover lasts until the
///   next read, never longer.
struct ReadDir {
    path: std::path::PathBuf,
    _lock: std::fs::File,
}

impl ReadDir {
    fn new(base: &Path) -> Option<ReadDir> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        for _ in 0..8 {
            let path = base.join(format!(
                "{READ_DIR_PREFIX}{}.{}.{:016x}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed),
                random_u64()
            ));
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {}
                // Someone made this name first: never use it, take another.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return None,
            }
            // Made by this create, so its owner is this process's user.
            let m = std::fs::symlink_metadata(&path).ok()?;
            if !m.file_type().is_dir() || m.mode() & 0o077 != 0 {
                return None;
            }
            let lock = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path.join(READ_DIR_LOCK))
                .ok()
                .filter(|f| f.try_lock().is_ok());
            // Not locked: another process's sweep found the lock file in the
            // instant between its creation and this lock, took it, and is
            // removing the directory. Never use it; take another name.
            let Some(lock) = lock else {
                remove_read_dir(&path);
                continue;
            };
            sweep_once(base, m.uid());
            return Some(ReadDir { path, _lock: lock });
        }
        None
    }
}

impl Drop for ReadDir {
    fn drop(&mut self) {
        remove_read_dir(&self.path);
    }
}

/// **A private scratch directory for one read that needs more than one
/// file** (a merge's dry run judges its batch on a copy of the target's
/// soup): the same directory a read's private copy gets (a new,
/// unguessable name in the temporary directory, mode 0700, locked for its
/// life), removed when dropped, and swept by the next process's first read
/// if this one dies. `None` when no such directory can be had.
pub struct PrivateDir(ReadDir);

impl PrivateDir {
    pub fn new() -> Option<PrivateDir> {
        ReadDir::new(&std::env::temp_dir()).map(PrivateDir)
    }

    /// The directory (put what the read needs in a subdirectory of it).
    pub fn path(&self) -> &Path {
        &self.0.path
    }
}

/// Remove a read's directory: the files directly in it (a symlink is
/// removed as a link, never followed), each real subdirectory with what is
/// in it (a private scratch's copy of a soup), then the directory.
fn remove_read_dir(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let is_dir = std::fs::symlink_metadata(e.path()).is_ok_and(|m| m.file_type().is_dir());
            if is_dir {
                let _ = std::fs::remove_dir_all(e.path());
            } else {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let _ = std::fs::remove_dir(dir);
}

/// 64 bits no other process can guess: the standard library seeds each
/// `RandomState` from the OS's random source.
fn random_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    h.finish()
}

/// Once per process: sweep what reads of dead processes left in `base`.
fn sweep_once(base: &Path, uid: u32) {
    static DONE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    DONE.get_or_init(|| sweep(base, uid));
}

/// Remove every read directory of user `uid` in `base` that no live read
/// holds: its lock can be taken (the process that made it is gone), or it
/// has no lock file and is older than [`UNLOCKED_STALE`]. Only a real
/// directory owned by `uid` is touched; anything else is left alone.
fn sweep(base: &Path, uid: u32) {
    use std::os::unix::fs::MetadataExt;
    let Ok(entries) = std::fs::read_dir(base) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        // `spine-read-<pid>.…`: the 0.1.16 directory (`spine-read-<uid>`,
        // shared by every read of a user) has no dot and is left alone.
        if !name.strip_prefix(READ_DIR_PREFIX).is_some_and(|rest| rest.contains('.')) {
            continue;
        }
        let dir = e.path();
        let Ok(m) = std::fs::symlink_metadata(&dir) else { continue };
        if !m.file_type().is_dir() || m.uid() != uid {
            continue;
        }
        let stale = match std::fs::OpenOptions::new().write(true).open(dir.join(READ_DIR_LOCK)) {
            // Held across the removal, so no read can be holding it.
            Ok(f) => match f.try_lock() {
                Ok(()) => {
                    remove_read_dir(&dir);
                    continue;
                }
                Err(_) => false,
            },
            Err(_) => m.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > UNLOCKED_STALE),
        };
        if stale {
            remove_read_dir(&dir);
        }
    }
}

fn open_uri(path: &Path, mode: &str) -> Result<Connection> {
    let text = path
        .display()
        .to_string()
        .replace('%', "%25")
        .replace('?', "%3f")
        .replace('#', "%23");
    Connection::open_with_flags(
        format!("file:{text}?{mode}"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| SpineError::hard(format!("cannot open `{}` read-only: {e}", path.display())))
}

/// **Every reason the open-time passes would ACT on the cauldron at `dir`**,
/// read through a read-only connection. Empty = at rest: not halted, the
/// spine at this binary's schema version, no un-cleared transaction row, no
/// lock row, no live tranche, no outstanding quiesce, and every pool file
/// carrying the seal column. `Err` when the spine cannot be read at all —
/// which a caller treats as "cannot prove it is at rest".
pub fn rest_check(dir: &Path) -> Result<Vec<String>> {
    let spine = dir.join(SPINE_FILE);
    if !spine.is_file() {
        return Err(SpineError::not_found(format!("`{}` holds no `{SPINE_FILE}`", dir.display())));
    }
    let conn = read_only_connection(&spine)?;
    let mut why = Vec::new();
    let has = |table: &str| -> Result<bool> {
        let n: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    };
    let count = |sql: &str| -> Result<i64> { Ok(conn.query_row(sql, [], |r| r.get(0))?) };

    let version: i64 = if conn
        .query_row(
            "SELECT count(*) FROM pragma_table_info('spine_state') WHERE name = 'spine_schema_version'",
            [],
            |r| r.get::<_, i64>(0),
        )?
        > 0
    {
        count("SELECT spine_schema_version FROM spine_state WHERE singleton = 1")?
    } else {
        0
    };
    if version != i64::from(SPINE_SCHEMA_VERSION) {
        why.push(format!(
            "the spine is at schema version {version}, this binary's is {SPINE_SCHEMA_VERSION} (an open would \
             upgrade or refuse it)"
        ));
    }
    let halted: i64 = count("SELECT halted FROM spine_state WHERE singleton = 1")?;
    if halted != 0 {
        why.push("the spine is HALTED".to_string());
    }
    let pending = count("SELECT count(*) FROM transactions WHERE cleared = 0")?;
    if pending > 0 {
        why.push(format!("{pending} transaction row(s) are not cleared (in flight, or stranded for recovery)"));
    }
    if has("locks")? {
        let locks = count("SELECT count(*) FROM locks")?;
        if locks > 0 {
            why.push(format!("{locks} lock row(s) are held"));
        }
    }
    if has("tranches")? {
        let live = count("SELECT count(*) FROM tranches WHERE state IN ('queued','open','staging','landing')")?;
        if live > 0 {
            why.push(format!("{live} tranche(s) are live"));
        }
    }
    if has("quiescers")? {
        let q = count("SELECT count(*) FROM quiescers")?;
        if q > 0 {
            why.push(format!("the spine is quiesced ({q} holder(s))"));
        }
    }
    drop(conn);
    for entry in std::fs::read_dir(dir)
        .map_err(|e| SpineError::hard(format!("cannot list `{}`: {e}", dir.display())))?
    {
        let p = entry.map_err(|e| SpineError::hard(format!("cannot list `{}`: {e}", dir.display())))?.path();
        if p.extension().and_then(|e| e.to_str()) != Some("db") || p.file_name().and_then(|n| n.to_str()) == Some(SPINE_FILE) {
            continue;
        }
        let (is_pool, sealed): (i64, i64) = read_only_connection(&p)
            .and_then(|c| {
                let is_pool: i64 = c.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'pool_meta'", [], |r| r.get(0))?;
                let sealed: i64 = c.query_row("SELECT count(*) FROM pragma_table_info('nodes') WHERE name = 'sealed'", [], |r| r.get(0))?;
                Ok((is_pool, sealed))
            })
            .map_err(|e| crate::user_grammar::unreadable(&p, e))?;
        // An empty or foreign file is not a pool that predates the seal flag:
        // it is no pool at all.
        if is_pool == 0 {
            return Err(crate::user_grammar::unreadable(&p, SpineError::hard(format!("`{}` holds no pool (no `pool_meta` table)", p.display()))));
        }
        if sealed == 0 {
            why.push(format!("pool file `{}` predates the seal flag (an open would add the column)", p.display()));
        }
    }
    Ok(why)
}

/// **Is `<dir>/<pool>.db` this soup's own pool file?** Answers `None` when
/// it is, else the sentence that says why not. Every part a write landed in
/// the pool is recorded twice: in the spine (`tx_parts`, under the pool's
/// name, `landed`) and in the pool file (`applied_parts`, same transaction
/// as its deltas). So the pool file is bound to its spine without anything
/// stored beside either: every part the spine says landed in the pool must
/// be in the file (a file that lacks one is an older copy, or a pool made
/// again under the same name), and every part the file holds must be one
/// the spine wrote to this pool (a file that holds another is another
/// cauldron's). A part the file holds whose spine row has not yet been
/// marked landed is a write in flight, and counts as the spine's. Read
/// through connections that cannot write.
pub fn pool_bound(dir: &Path, pool: &str) -> Result<Option<String>> {
    let b = binding(dir, pool)?;
    Ok(b.why(pool))
}

/// What [`pool_bound`] found: the parts the file holds that the spine never
/// wrote to the pool (`foreign`), and the parts the spine landed there that
/// the file lacks (`lost`: hash, part, seq, ts), less those the owner
/// accepted as lost (`cauldron soup grammar rebind`).
#[derive(Debug, Clone, Default)]
pub struct Binding {
    pub foreign: Vec<(String, i64)>,
    pub lost: Vec<(String, i64, i64, String)>,
}

impl Binding {
    /// `None` when the file is the pool's own; else why not, and what to do.
    pub fn why(&self, pool: &str) -> Option<String> {
        let (f, l) = (self.foreign.len(), self.lost.len());
        match (f, l) {
            (0, 0) => None,
            (0, l) => Some(format!(
                "this soup's spine records {l} write(s) to pool `{pool}` that the file does not hold (the last at {}), so it is \
                 an older copy of the pool's file, such as one restored from a backup taken before those writes. Either \
                 restore the whole soup directory from one backup, or accept this copy, losing those writes: `cauldron soup \
                 grammar rebind {pool} --env <env> --cauldron <name>` names what they wrote, and with `--yes` records them \
                 as lost (their ids are never minted again)",
                self.lost.last().map(|x| x.3.as_str()).unwrap_or("?")
            )),
            (f, _) => Some(format!(
                "it holds {f} write(s) this soup's spine never made to pool `{pool}`: it is another cauldron's pool file, or \
                 this soup's `spine.db` is older than the pool file (a spine restored alone). Put back this cauldron's own \
                 pool file, or restore the whole soup directory (spine and every pool) from one backup"
            )),
        }
    }
}

/// [`pool_bound`]'s facts.
pub fn binding(dir: &Path, pool: &str) -> Result<Binding> {
    use std::collections::BTreeSet;
    let spine = read_only_connection(&dir.join(SPINE_FILE))?;
    let forgiven = crate::user_registry::forgiven(&spine, pool)?;
    let mut written: BTreeSet<(String, i64)> = BTreeSet::new();
    let mut landed: Vec<(String, i64, i64, String)> = Vec::new();
    {
        let mut q = spine.prepare(
            "SELECT t.hash, p.part, p.landed, t.seq, t.ts FROM tx_parts p JOIN transactions t ON t.seq = p.seq WHERE p.pool = ?1 ORDER BY t.seq",
        )?;
        let rows = q.query_map([pool], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, String>(4)?))
        })?;
        for row in rows {
            let (h, part, l, seq, ts) = row?;
            if l != 0 && !forgiven.contains(&(h.clone(), part)) {
                landed.push((h.clone(), part, seq, ts));
            }
            written.insert((h, part));
        }
    }
    let file = dir.join(format!("{pool}.db"));
    let conn = read_only_connection(&file)?;
    let mut applied: BTreeSet<(String, i64)> = BTreeSet::new();
    {
        let mut q = conn.prepare("SELECT tx_hash, part FROM applied_parts")?;
        let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            applied.insert(row?);
        }
    }
    Ok(Binding {
        foreign: applied.iter().filter(|x| !written.contains(*x)).cloned().collect(),
        lost: landed.into_iter().filter(|(h, p, _, _)| !applied.contains(&(h.clone(), *p))).collect(),
    })
}

/// Has this soup's spine ever recorded a write to pool `pool`? (A pool name
/// with a history is never made again as a new, empty pool: its ids would be
/// minted again, and edges into its old records would point at new ones.)
pub fn pool_has_history(dir: &Path, pool: &str) -> Result<bool> {
    // Parts the owner accepted as lost (`rebind`) left no record behind, and
    // their ids are kept as lost and their counters stay: they are not a
    // history a new, empty pool of the name would collide with.
    let spine = read_only_connection(&dir.join(SPINE_FILE))?;
    let forgiven = crate::user_registry::forgiven(&spine, pool)?;
    let mut q = spine.prepare("SELECT t.hash, p.part FROM tx_parts p JOIN transactions t ON t.seq = p.seq WHERE p.pool = ?1 AND p.landed = 1")?;
    let rows = q.query_map([pool], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    for r in rows {
        if !forgiven.contains(&r?) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// **The commit order of every landed transaction** of the cauldron at
/// `dir`, read through a read-only connection: transaction hash (hex, as
/// [`crate::pool::Snapshot::created_tx`] gives it) -> the committed-commit
/// counter at its commit. What "newest first" means for an outside reader
/// (`cauldron soup nodes`): the spine's own order, never a timestamp.
/// A transaction that has not landed has no entry.
pub fn commit_order(dir: &Path) -> Result<std::collections::HashMap<String, i64>> {
    let conn = read_only_connection(&dir.join(SPINE_FILE))?;
    let mut stmt = conn.prepare("SELECT hash, committed_at FROM transactions WHERE committed_at IS NOT NULL")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let mut out = std::collections::HashMap::new();
    for row in rows {
        let (hash, at) = row?;
        out.insert(hash, at);
    }
    Ok(out)
}

/// **Copy the cauldron at `src` into the empty directory `dst`** without
/// writing to `src`: every `*.db` file (the spine and each pool) is read
/// through [`read_only_connection`] and written out with `VACUUM INTO`,
/// whose only write is the new file under `dst`. Each copy is a consistent
/// snapshot of its own file; the files are taken one after another, so a
/// writer active meanwhile can leave the set mid-commit, which
/// [`rest_check`] on the copy then names. What it is for: a dry run that
/// must not open the live store with [`crate::Cauldron::open`] (whose
/// passes write) runs that open on the copy instead.
pub fn snapshot_copy(src: &Path, dst: &Path) -> Result<()> {
    if !src.join(SPINE_FILE).is_file() {
        return Err(SpineError::not_found(format!("`{}` holds no `{SPINE_FILE}`", src.display())));
    }
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    for entry in std::fs::read_dir(src).map_err(|e| SpineError::hard(format!("cannot list `{}`: {e}", src.display())))? {
        let p = entry.map_err(|e| SpineError::hard(format!("cannot list `{}`: {e}", src.display())))?.path();
        if p.extension().and_then(|e| e.to_str()) == Some("db") {
            files.push(p);
        }
    }
    // The pools first and the spine last: the spine copy can only be AHEAD
    // of a pool copy, so its serial counters never fall behind a node the
    // pool copy holds (a re-mint judged on the copy predicts no id the copy
    // already uses), and a commit caught in flight is an un-cleared row the
    // rest check on the copy names.
    files.sort_by_key(|p| (p.file_name().and_then(|n| n.to_str()) == Some(SPINE_FILE), p.clone()));
    for f in files {
        let out = dst.join(f.file_name().expect("a listed file has a name"));
        if out.exists() {
            return Err(SpineError::usage(format!("`{}` already exists; a snapshot copy goes into an empty directory", out.display())));
        }
        let conn = read_only_connection(&f)?;
        conn.execute("VACUUM INTO ?1", [out.to_str().ok_or_else(|| SpineError::usage(format!("`{}` is not UTF-8", out.display())))?])
            .map_err(|e| SpineError::hard(format!("cannot copy `{}`: {e}", f.display())))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// Readers hammering a WAL file while a writer lands write after write,
    /// each on a connection of its own that it closes (so its log is
    /// checkpointed into the main file and removed, as the spine's are).
    /// Every write moves value between rows and keeps the total, so a read
    /// that mixed pages of two states would see another total (reading the
    /// main file in place, this saw both wrong totals and `database disk
    /// image is malformed`). Zero refusals and every total right.
    #[test]
    fn reads_while_writes_land_are_never_refused_or_torn() {
        let d = std::env::temp_dir().join(format!(
            "spine-offline-hammer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("pool.db");
        const ROWS: i64 = 3000;
        {
            let c = Connection::open(&p).unwrap();
            c.pragma_update(None, "journal_mode", "WAL").unwrap();
            c.execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY, v INTEGER, pad BLOB); CREATE INDEX tv ON t(v, pad);").unwrap();
            let tx = c.unchecked_transaction().unwrap();
            for i in 0..ROWS {
                tx.execute("INSERT INTO t VALUES (?1, 100, randomblob(150))", [i]).unwrap();
            }
            tx.commit().unwrap();
        }
        assert!(!super::side_file(&p, "-wal").exists(), "the writer's close removed its log");
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let (p, stop) = (p.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut n = 0u64;
                while !stop.load(Ordering::SeqCst) {
                    let c = Connection::open(&p).unwrap();
                    c.busy_timeout(std::time::Duration::from_secs(10)).unwrap();
                    let a = (n * 7919) as i64 % ROWS;
                    c.execute_batch(&format!(
                        "BEGIN IMMEDIATE; UPDATE t SET v = v - 5, pad = randomblob(150) WHERE id % 11 = {}; \
                         UPDATE t SET v = v + 5, pad = randomblob(150) WHERE id IN (SELECT id + 1 FROM t WHERE id % 11 = {} AND id + 1 < {ROWS}); \
                         UPDATE t SET v = v + 5 * (SELECT count(*) FROM t WHERE id % 11 = {} AND id + 1 >= {ROWS}) WHERE id = {a}; COMMIT;",
                        n % 11, n % 11, n % 11
                    ))
                    .unwrap();
                    drop(c);
                    n += 1;
                }
                n
            })
        };
        let t0 = std::time::Instant::now();
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let p = p.clone();
                std::thread::spawn(move || {
                    let (mut reads, mut failures) = (0u64, Vec::new());
                    while t0.elapsed() < std::time::Duration::from_secs(6) {
                        let got = (|| -> Result<i64, String> {
                            let c = super::read_only_connection(&p).map_err(|e| e.to_string())?;
                            let mut st = c.prepare("SELECT v, length(pad) FROM t INDEXED BY tv").map_err(|e| e.to_string())?;
                            let mut rows = st.query([]).map_err(|e| e.to_string())?;
                            let (mut total, mut k) = (0i64, 0u64);
                            while let Some(r) = rows.next().map_err(|e| e.to_string())? {
                                total += r.get::<_, i64>(0).map_err(|e| e.to_string())?;
                                k += 1;
                                if k % 25 == 0 {
                                    std::thread::sleep(std::time::Duration::from_micros(100));
                                }
                            }
                            Ok(total)
                        })();
                        reads += 1;
                        match got {
                            Ok(total) if total == ROWS * 100 => {}
                            Ok(total) => failures.push(format!("torn read: total {total}")),
                            Err(e) => failures.push(e),
                        }
                    }
                    (reads, failures)
                })
            })
            .collect();
        let mut all = Vec::new();
        let mut reads = 0;
        for r in readers {
            let (n, f) = r.join().unwrap();
            reads += n;
            all.extend(f);
        }
        stop.store(true, Ordering::SeqCst);
        let writes = writer.join().unwrap();
        let _ = std::fs::remove_dir_all(&d);
        eprintln!("{reads} reads during {writes} writes");
        assert!(writes > 50 && reads > 50, "{reads} reads, {writes} writes");
        assert!(all.is_empty(), "{} of {reads} reads failed: {:?}", all.len(), &all[..all.len().min(5)]);
    }

    /// A fresh base for one test's private directories, under the
    /// temporary directory, and the entries in it.
    fn base(tag: &str) -> std::path::PathBuf {
        let b = std::env::temp_dir().join(format!("spine-offline-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&b);
        std::fs::create_dir_all(&b).unwrap();
        b
    }

    fn entries(d: &std::path::Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(d).unwrap().flatten().map(|e| e.file_name().into_string().unwrap()).collect();
        v.sort();
        v
    }

    fn store(d: &std::path::Path) -> std::path::PathBuf {
        let p = d.join("pool.db");
        let c = Connection::open(&p).unwrap();
        c.pragma_update(None, "journal_mode", "WAL").unwrap();
        c.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (1);").unwrap();
        p
    }

    /// No side file beside the store, and nothing at all in the temporary
    /// directory, is left by a read of a file at rest: not while the
    /// connection is open (the copy and its directory are gone before the
    /// read returns), and not after.
    #[test]
    fn a_read_of_a_file_at_rest_leaves_nothing() {
        let d = base("rest");
        let tmp = base("rest-tmp");
        let p = store(&d);
        let before = (entries(&d), super::identity(&p).unwrap());
        let c = super::read_only_connection_in(&p, &tmp).unwrap();
        assert!(entries(&tmp).is_empty(), "left in the temporary directory while open: {:?}", entries(&tmp));
        assert_eq!(c.query_row("SELECT x FROM t", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert!(c.execute("INSERT INTO t VALUES (2)", []).is_err(), "the connection cannot write");
        drop(c);
        assert_eq!((entries(&d), super::identity(&p).unwrap()), before, "the read wrote beside or to the file");
        assert!(entries(&tmp).is_empty(), "left in the temporary directory: {:?}", entries(&tmp));
        // Many reads, from several threads at once: still nothing left.
        std::thread::scope(|sc| {
            for _ in 0..4 {
                sc.spawn(|| {
                    for _ in 0..25 {
                        let c = super::read_only_connection_in(&p, &tmp).unwrap();
                        assert_eq!(c.query_row("SELECT x FROM t", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
                    }
                });
            }
        });
        assert!(entries(&tmp).is_empty(), "left after 100 reads: {:?}", entries(&tmp));
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A read's directory is made new for it, mode 0700, a real directory of
    /// this user, under a name not seen before (two at once never share
    /// one), holding a lock file it keeps locked; dropping it removes it.
    #[test]
    fn a_read_directory_is_private_new_locked_and_removed_on_drop() {
        use std::os::unix::fs::MetadataExt;
        let b = base("dir");
        let one = super::ReadDir::new(&b).expect("made");
        let two = super::ReadDir::new(&b).expect("made");
        assert_ne!(one.path, two.path);
        for d in [&one.path, &two.path] {
            let m = std::fs::symlink_metadata(d).unwrap();
            assert!(m.file_type().is_dir());
            assert_eq!(m.mode() & 0o777, 0o700, "{}", d.display());
            let name = d.file_name().unwrap().to_string_lossy().into_owned();
            assert!(name.starts_with(&format!("spine-read-{}.", std::process::id())), "{name}");
            let lock = std::fs::OpenOptions::new().write(true).open(d.join(super::READ_DIR_LOCK)).unwrap();
            assert!(lock.try_lock().is_err(), "a live read's lock cannot be taken");
        }
        drop(one);
        assert_eq!(entries(&b).len(), 1, "{:?}", entries(&b));
        drop(two);
        assert!(entries(&b).is_empty(), "{:?}", entries(&b));
        let _ = std::fs::remove_dir_all(&b);
    }

    /// Where no private directory can be made (the temporary directory is
    /// not writable), the read still answers, through SQLite's locking.
    #[test]
    fn a_read_with_no_private_directory_reads_through_the_lock() {
        use std::os::unix::fs::PermissionsExt;
        let d = base("nodir");
        let tmp = base("nodir-tmp");
        let p = store(&d);
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert!(super::ReadDir::new(&tmp).is_none());
        let c = super::read_only_connection_in(&p, &tmp).unwrap();
        assert_eq!(c.query_row("SELECT x FROM t", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        drop(c);
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(entries(&tmp).is_empty());
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// **The sweep.** What a read of a process that died mid-read left (its
    /// lock released by the OS, a copy still in it) is removed; a live
    /// read's directory (its lock held), one being made (no lock file yet,
    /// and young), the shared directory of the release before, anything
    /// that is a symlink, and every other name are left alone, and a
    /// symlink's target is never entered.
    #[test]
    fn the_sweep_removes_what_dead_reads_left_and_nothing_else() {
        use std::os::unix::fs::MetadataExt;
        let b = base("sweep");
        let me = std::fs::metadata(&b).unwrap().uid();
        let mk = |name: &str| {
            let d = b.join(name);
            std::fs::create_dir(&d).unwrap();
            d
        };
        let dead = mk("spine-read-1.0.00000000000000aa");
        std::fs::write(dead.join(super::READ_DIR_LOCK), b"").unwrap();
        std::fs::write(dead.join("copy.db"), b"left").unwrap();
        let alive = mk("spine-read-2.0.00000000000000bb");
        let held = std::fs::OpenOptions::new().write(true).create_new(true).open(alive.join(super::READ_DIR_LOCK)).unwrap();
        held.try_lock().unwrap();
        let young = mk("spine-read-3.0.00000000000000cc");
        let legacy = mk(&format!("spine-read-{me}"));
        let target = mk("target");
        std::fs::write(target.join(super::READ_DIR_LOCK), b"").unwrap();
        std::fs::write(target.join("keep"), b"keep").unwrap();
        std::os::unix::fs::symlink(&target, b.join("spine-read-4.0.00000000000000dd")).unwrap();
        let other = mk("not-ours.0.1");
        std::fs::write(other.join(super::READ_DIR_LOCK), b"").unwrap();
        super::sweep(&b, me);
        let left = entries(&b);
        assert!(!dead.exists(), "a dead read's directory is swept: {left:?}");
        for kept in [&alive, &young, &legacy, &target, &other] {
            assert!(kept.exists(), "{} kept: {left:?}", kept.display());
        }
        assert!(b.join("spine-read-4.0.00000000000000dd").symlink_metadata().is_ok(), "a symlink is left alone");
        assert!(target.join("keep").exists(), "a symlink's target is never entered");
        // Another user's sweep touches none of ours.
        drop(held);
        super::sweep(&b, me + 1);
        assert!(alive.exists());
        super::sweep(&b, me);
        assert!(!alive.exists(), "swept once its process let go");
        let _ = std::fs::remove_dir_all(&b);
    }
}
