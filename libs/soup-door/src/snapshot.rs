//! **A cauldron as it was before a merge**: the complete snapshot `cauldron
//! merge` takes of its target, and the exact restore `cauldron merge --undo`
//! (and `cauldron soup recover`, when a merge cannot be finished) makes from
//! it.
//!
//! # What is in it
//!
//! Everything under the cauldron's directory that a merge can change: its
//! soup (both pools, the spine, the region lock table `region-locks.json`),
//! its dumps store with every blob, its `merges/` directory, its `events/`
//! and its `cauldron.toml`. Two things are left out, each named in the
//! manifest with why: the writer lock files (`writer.lock`, coordination
//! rather than data: the merge holds them while the snapshot is taken and
//! the restore holds them while it puts the files back), and the
//! directories a merge never touches (`tui/`, the face's own state, and
//! `ripples/`, the ripple runs' own journals).
//!
//! # How it is taken
//!
//! Under the target's writer locks (its soup's and its dumps store's), before
//! anything of the merge is opened, so nothing writes while it is copied.
//! Each file is copied with `std::fs::copy`, which on APFS (macOS) makes a
//! CLONE (`fclonefileat`): the copy shares the original's blocks until one of
//! the two is written, so a snapshot of a large dumps store costs almost no
//! space or time; elsewhere it is a byte copy. A clone, not a hard link: a
//! hard link would be the same file, so anything that wrote into it later
//! would change the snapshot too.
//!
//! It is verified as it is taken: every file's size and SHA-256 are read
//! from the original and from the copy, must agree, and are recorded in the
//! snapshot's `manifest.json` with the file count and total bytes. A restore
//! checks every copy against the manifest again before it moves anything.
//!
//! It is stored BESIDE the cauldron's directory, under a dated name
//! (`cauldrons/<name>.<MG-n>-before-<UTC time>/`, which no cauldron name can
//! be, so no listing takes it for a cauldron), directories 0700 and files
//! 0600. It holds a copy of the target's dumps as they were, so `cauldron
//! soup dump-remove` does not reach it: move it away once it is no longer
//! needed.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::{refuse, Code, Error, Result};

/// The snapshot's manifest, in its directory.
pub const MANIFEST: &str = "manifest.json";
/// The copied files, under the snapshot's directory, by their path in the
/// cauldron's directory.
pub const FILES: &str = "files";
/// The manifest's format.
pub const FORMAT: u32 = 1;

fn store(err: impl std::fmt::Display) -> Error {
    Error::new(Code::Store, err.to_string())
}

/// **What a snapshot covers**: the directory it is relative to and the
/// entries of it that are in it (each a directory or a file, absent ones
/// skipped).
pub struct Scope {
    pub base: PathBuf,
    pub parts: Vec<String>,
}

/// The scope of `c`: for a cauldron of its own directory, `soup/`,
/// `dumps/`, `merges/`, `events/` and `cauldron.toml` in it; for the legacy
/// cauldron, its soup, dumps store and merges in `data/<env>/soup/`.
pub fn scope(c: &env::Cauldron) -> Scope {
    let base = env::private::data_dir(c);
    let soup = c.soup();
    let soup_name = soup.file_name().and_then(|n| n.to_str()).unwrap_or("soup").to_string();
    let mut parts = vec![soup_name, crate::dumps::DIR.to_string(), crate::merge::DIR.to_string()];
    if c.dir().is_some() {
        parts.push("events".into());
        parts.push(env::cauldron::CAULDRON_MANIFEST.into());
    }
    Scope { base, parts }
}

/// What a snapshot never holds, each with why (the manifest lists them).
pub fn left_out(c: &env::Cauldron) -> Vec<Value> {
    let mut out = vec![json!({
        "what": "the writer lock files (writer.lock)",
        "why": "coordination, not data: the merge holds them while the snapshot is taken, and a restore holds them while it puts the files back",
    })];
    if c.dir().is_some() {
        out.push(json!({"what": "tui/", "why": "the face's own state; a merge never changes it"}));
        out.push(json!({"what": "ripples/", "why": "the ripple runs' own journals; a merge never changes them"}));
    }
    out
}

fn is_lock(rel: &str) -> bool {
    rel.rsplit('/').next() == Some(crate::lock::WRITER_LOCK)
}

/// A database file of a store (`*.db`, and SQLite's `-wal` and `-shm` beside
/// one): its content is what the stores' change counters count.
fn is_db(rel: &str) -> bool {
    rel.ends_with(".db") || rel.ends_with(".db-wal") || rel.ends_with(".db-shm") || rel.ends_with(".db-journal")
}

/// Every directory and file of the scope (relative paths, `/`-separated),
/// the writer locks left out. A symbolic link is listed as a file only if it
/// is one (never followed).
fn walk(s: &Scope) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
    let mut dirs = BTreeSet::new();
    let mut files = BTreeSet::new();
    let mut todo: Vec<String> = Vec::new();
    for p in &s.parts {
        let abs = s.base.join(p);
        match std::fs::symlink_metadata(&abs) {
            Ok(m) if m.is_dir() => todo.push(p.clone()),
            Ok(m) if m.is_file() => {
                if !is_lock(p) {
                    files.insert(p.clone());
                }
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(store(format!("reading {}: {e}", abs.display()))),
        }
    }
    while let Some(d) = todo.pop() {
        dirs.insert(d.clone());
        let abs = s.base.join(&d);
        let rd = std::fs::read_dir(&abs).map_err(|e| store(format!("listing {}: {e}", abs.display())))?;
        for entry in rd {
            let entry = entry.map_err(store)?;
            let name = entry.file_name().to_string_lossy().to_string();
            let rel = format!("{d}/{name}");
            let m = std::fs::symlink_metadata(entry.path()).map_err(store)?;
            if m.is_dir() {
                todo.push(rel);
            } else if m.is_file() && !is_lock(&rel) {
                files.insert(rel);
            }
        }
    }
    Ok((dirs, files))
}

/// SHA-256 (hex) and size of the file at `p`.
pub(crate) fn digest(p: &Path) -> Result<(String, u64)> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(p).map_err(|e| store(format!("reading {}: {e}", p.display())))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut n = 0u64;
    loop {
        let k = f.read(&mut buf).map_err(|e| store(format!("reading {}: {e}", p.display())))?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
        n += k as u64;
    }
    Ok((h.finalize().iter().map(|b| format!("{b:02x}")).collect(), n))
}

fn private_file(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(env::private::FILE_MODE))
        .map_err(|e| store(format!("making {} private: {e}", p.display())))
}

fn sync_dir(d: &Path) {
    if let Ok(f) = std::fs::File::open(d) {
        let _ = f.sync_all();
    }
}

/// Copy `from` to `to` (which must not exist): a clone on APFS, a byte copy
/// elsewhere; made 0600 and synced.
fn clone_file(from: &Path, to: &Path) -> Result<()> {
    std::fs::copy(from, to).map_err(|e| store(format!("copying {} to {}: {e}", from.display(), to.display())))?;
    private_file(to)?;
    std::fs::File::open(to).and_then(|f| f.sync_all()).map_err(|e| store(format!("syncing {}: {e}", to.display())))?;
    Ok(())
}

/// The change counters of `c`'s two stores (`soup`, `dumps`): every landed
/// write moves one of them, so two equal pairs mean no write between.
pub fn counters(c: &env::Cauldron) -> Result<Value> {
    let soup = {
        let conn = spine::offline::read_only_connection(&c.soup().join(spine::spine_db::SPINE_FILE)).map_err(store)?;
        conn.query_row("SELECT committed_commits FROM counters WHERE singleton = 1", [], |r| r.get::<_, i64>(0)).map_err(store)?
    };
    let dumps = crate::dumps::counter(c)?;
    Ok(json!({"soup": soup, "dumps": dumps}))
}

/// **The snapshot's directory** for merge `merge` of `c` taken `at` (an
/// RFC 3339 time): beside the cauldron's directory.
pub fn dir_for(c: &env::Cauldron, merge: &str, at: &chrono::DateTime<chrono::Utc>) -> PathBuf {
    let base = env::private::data_dir(c);
    let name = base.file_name().and_then(|n| n.to_str()).unwrap_or("cauldron").to_string();
    let stamp = at.format("%Y%m%dT%H%M%S%3fZ");
    base.parent().unwrap_or(Path::new(".")).join(format!("{name}.{merge}-before-{stamp}"))
}

/// **Take the snapshot** of `c` for merge `merge` into `dir` (which must not
/// exist yet), under the target's writer locks the caller holds. Answers the
/// manifest (also written to `dir/manifest.json`).
pub fn take(c: &env::Cauldron, merge: &str, dir: &Path) -> Result<Value> {
    if dir.exists() {
        refuse!(Code::Store, "REFUSED: {} already exists; a snapshot is taken into a new directory. Nothing was written.", dir.display());
    }
    let s = scope(c);
    let (dirs, files) = walk(&s)?;
    let tmp = dir.with_extension(format!("taking-{}", std::process::id()));
    env::private::make_dir(&tmp).map_err(|e| store(format!("making {}: {e}", tmp.display())))?;
    let into = tmp.join(FILES);
    env::private::make_dir(&into).map_err(store)?;
    for d in &dirs {
        env::private::make_dir(&into.join(d)).map_err(|e| store(format!("making {}: {e}", into.join(d).display())))?;
    }
    let mut listed = Vec::new();
    let mut total = 0u64;
    for f in &files {
        let (src, dst) = (s.base.join(f), into.join(f));
        clone_file(&src, &dst)?;
        let (want, n) = digest(&src)?;
        let (got, m) = digest(&dst)?;
        if want != got || n != m {
            refuse!(
                Code::Store,
                "REFUSED: the snapshot's copy of {f} does not match the original ({n} bytes, sha256 {want}; the copy {m} bytes, \
                 sha256 {got}), so the snapshot is not taken and the merge does not start. Nothing of the merge was written."
            );
        }
        total += n;
        listed.push(json!({"path": f, "bytes": n, "sha256": want}));
    }
    for d in dirs.iter().rev() {
        sync_dir(&into.join(d));
    }
    let manifest = json!({
        "format": FORMAT,
        "what": "the target of a merge, as it was before the merge landed",
        "merge": merge,
        "env": c.env,
        "cauldron": c.name,
        "cauldron_id": c.id(),
        "taken_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "of": s.base.display().to_string(),
        "parts": s.parts,
        "left_out": left_out(c),
        "copied_by": "std::fs::copy: a clone (copy-on-write, fclonefileat) on APFS, a byte copy elsewhere; each copy checked against its original by size and SHA-256",
        "counters": counters(c)?,
        "count": {"files": listed.len(), "bytes": total, "dirs": dirs.len()},
        "dirs": dirs,
        "files": listed,
    });
    write_json(&tmp.join(MANIFEST), &manifest)?;
    sync_dir(&tmp);
    std::fs::rename(&tmp, dir).map_err(|e| store(format!("renaming {} to {}: {e}", tmp.display(), dir.display())))?;
    sync_dir(dir.parent().unwrap_or(Path::new(".")));
    Ok(manifest)
}

/// Write `v` to `path` (0600), synced.
pub fn write_json(path: &Path, v: &Value) -> Result<()> {
    use std::io::Write;
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

/// **Read a snapshot's manifest and check every copy against it** (size and
/// SHA-256), or refuse naming the first that is missing or differs.
pub fn verify(dir: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(dir.join(MANIFEST))
        .map_err(|e| store(format!("reading the snapshot's manifest {}: {e}", dir.join(MANIFEST).display())))?;
    let m: Value = serde_json::from_str(&text).map_err(|e| store(format!("the snapshot's manifest will not parse: {e}")))?;
    for f in m["files"].as_array().into_iter().flatten() {
        let rel = f["path"].as_str().unwrap_or("");
        let p = dir.join(FILES).join(rel);
        let (got, n) = digest(&p).map_err(|e| {
            Error::new(Code::Store, format!("REFUSED: the snapshot {} is damaged: {rel} cannot be read ({}). Nothing was changed.", dir.display(), e.message))
        })?;
        if Some(got.as_str()) != f["sha256"].as_str() || Some(n) != f["bytes"].as_u64() {
            refuse!(
                Code::Store,
                "REFUSED: the snapshot {} is damaged: its copy of {rel} is not the file it recorded ({n} bytes, not {}). Nothing was changed.",
                dir.display(),
                f["bytes"]
            );
        }
    }
    Ok(m)
}

/// **Put `c` back exactly as the snapshot in `dir` holds it**, under the
/// target's writer locks the caller holds: every copy is checked against the
/// manifest first ([`verify`]); then each file that differs from its copy is
/// moved aside into `aside` and the copy put in its place (a clone, made
/// 0600, renamed into place), each file the snapshot does not hold is moved
/// aside, and each directory it did not have is removed once empty. The
/// stores' spine files are put back LAST (the dumps store's, then the
/// soup's), so a restore cut short leaves a soup whose own spine still says
/// what it said (for `cauldron soup recover`'s put-back, still held by the
/// merge, which stays pending, so every writer stays refused until recover
/// runs again). An undo uses [`restore_guarded`], which records where it
/// began and is resumable on its own. `only`: restore only these parts
/// (`None`: all). Answers what it did.
pub fn restore(c: &env::Cauldron, dir: &Path, aside: &Path, only: Option<&[String]>) -> Result<Value> {
    restore_except(c, dir, aside, only, &[])
}

/// [`restore`], leaving the files `keep` names exactly as they are now
/// (neither put back nor moved aside): `cauldron soup recover` keeps the
/// live region lock table, which a running ripple may have written since.
pub fn restore_except(c: &env::Cauldron, dir: &Path, aside: &Path, only: Option<&[String]>, keep: &[String]) -> Result<Value> {
    let m = verify(dir)?;
    let s = scope(c);
    let within = |rel: &str| {
        !keep.iter().any(|k| k == rel)
            && match only {
                None => true,
                Some(ps) => ps.iter().any(|p| rel == p || rel.starts_with(&format!("{p}/"))),
            }
    };
    let want: BTreeMap<String, (String, u64)> = m["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| Some((f["path"].as_str()?.to_string(), (f["sha256"].as_str()?.to_string(), f["bytes"].as_u64()?))))
        .filter(|(p, _)| within(p))
        .collect();
    let want_dirs: BTreeSet<String> =
        m["dirs"].as_array().into_iter().flatten().filter_map(|d| d.as_str().map(str::to_string)).filter(|d| within(d)).collect();
    let (have_dirs, have_files) = walk(&s)?;
    let have_files: BTreeSet<String> = have_files.into_iter().filter(|f| within(f)).collect();
    env::private::make_dir(aside).map_err(|e| store(format!("making {}: {e}", aside.display())))?;
    let move_aside = |rel: &str| -> Result<()> {
        let to = aside.join(rel);
        let mut at = aside.to_path_buf();
        for part in Path::new(rel).parent().into_iter().flat_map(|p| p.components()) {
            at = at.join(part);
            env::private::make_dir(&at).map_err(|e| store(format!("making {}: {e}", at.display())))?;
        }
        std::fs::rename(s.base.join(rel), &to).map_err(|e| store(format!("moving {} aside: {e}", s.base.join(rel).display())))
    };
    let (mut put_back, mut moved, mut kept) = (0usize, 0usize, 0usize);
    // Files the snapshot does not hold: aside.
    for f in have_files.difference(&want.keys().cloned().collect()) {
        move_aside(f)?;
        moved += 1;
    }
    for d in &want_dirs {
        env::private::make_dir(&s.base.join(d)).map_err(|e| store(format!("making {}: {e}", s.base.join(d).display())))?;
    }
    // The spine files last: the dumps store's, then the soup's.
    let spine = spine::spine_db::SPINE_FILE;
    let soup_name = s.parts[0].clone();
    let rank = |p: &str| -> u8 {
        if p == format!("{soup_name}/{spine}") {
            2
        } else if p.ends_with(&format!("/{spine}")) {
            1
        } else {
            0
        }
    };
    let mut order: Vec<&String> = want.keys().collect();
    order.sort_by_key(|p| (rank(p), p.to_string()));
    for f in order {
        let (sha, n) = &want[f];
        let target = s.base.join(f);
        if have_files.contains(f) {
            if digest(&target).is_ok_and(|(h, k)| &h == sha && k == *n) {
                kept += 1;
                continue;
            }
        }
        let parent = target.parent().unwrap_or(Path::new("."));
        let tmp = parent.join(format!(".restoring-{}-{}", std::process::id(), target.file_name().and_then(|x| x.to_str()).unwrap_or("f")));
        clone_file(&dir.join(FILES).join(f), &tmp)?;
        let (h, k) = digest(&tmp)?;
        if &h != sha || k != *n {
            refuse!(Code::Store, "REFUSED: restoring {f}: the copy put back is not the snapshot's ({k} bytes). The restore stopped here; run it again.");
        }
        if target.exists() {
            move_aside(f)?;
            moved += 1;
        }
        std::fs::rename(&tmp, &target).map_err(|e| store(format!("putting {} back: {e}", target.display())))?;
        sync_dir(parent);
        put_back += 1;
    }
    // Directories the snapshot did not have, deepest first, once empty.
    let mut extra: Vec<&String> = have_dirs.iter().filter(|d| within(d) && !want_dirs.contains(*d)).collect();
    extra.sort_by_key(|d| std::cmp::Reverse(d.matches('/').count()));
    let mut left = Vec::new();
    for d in extra {
        let p = s.base.join(d);
        if std::fs::remove_dir(&p).is_err() {
            left.push(d.clone());
        }
    }
    Ok(json!({"put_back": put_back, "unchanged": kept, "moved_aside": moved, "aside": aside.display().to_string(), "dirs_left": left}))
}

/// **What a merge's own writes are**, to leave out of [`outside`]: the soup
/// transactions it landed (its parts), the records they created, the dumps
/// it carried (by id in the target) and their transactions, and its own
/// directory under `merges/`.
#[derive(Debug, Clone, Default)]
pub struct MergeWrites {
    pub soup_txs: BTreeSet<String>,
    pub dump_ids: BTreeSet<String>,
    /// The end of the dispatch of each of its dumps' writes (`[merge MG-n dump]`).
    pub dump_dispatch_end: String,
    pub own_dir: String,
}

fn h(v: &Value) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(v.to_string().as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// **The content of a cauldron outside one merge's writes**, read from its
/// files at `base` (the live cauldron's directory, or a snapshot's `files/`)
/// with connections that cannot write: every record of every pool (type,
/// state, fields, seal) with the edges from it, every decided soup
/// transaction, every dump (state and fields) and dumps-store transaction,
/// every blob by digest, and every other file of the scope by digest
/// (the region lock table, `events/`, `cauldron.toml`, other merges'
/// records). With `leave_out`, the merge's own transactions, the records
/// they created, the edges from or to those, its dumps and their writes, the
/// blobs only its dumps name, and its own directory are left out; so a
/// target nothing else wrote since the merge began reads exactly as its
/// snapshot does.
pub fn outside(base: &Path, soup_name: &str, leave_out: Option<&MergeWrites>) -> Result<Value> {
    let soup = base.join(soup_name);
    let dumps_dir = base.join(crate::dumps::DIR);
    let none = MergeWrites::default();
    let lo = leave_out.unwrap_or(&none);
    // Soup transactions.
    let mut soup_txs = BTreeSet::new();
    if soup.join(spine::spine_db::SPINE_FILE).is_file() {
        let conn = spine::offline::read_only_connection(&soup.join(spine::spine_db::SPINE_FILE)).map_err(store)?;
        let mut st = conn.prepare("SELECT hash FROM transactions WHERE decided = 1").map_err(store)?;
        let rows = st.query_map([], |r| r.get::<_, String>(0)).map_err(store)?;
        for r in rows {
            let r = r.map_err(store)?;
            if !lo.soup_txs.contains(&r) {
                soup_txs.insert(r);
            }
        }
    }
    // Records and edges, every pool.
    let mut records = BTreeMap::new();
    let mut edges = BTreeSet::new();
    let mut merged_nodes: BTreeSet<String> = BTreeSet::new();
    let mut pools = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&soup) {
        for e in rd.flatten() {
            let p = e.path();
            let stem = p.file_stem().and_then(|n| n.to_str()).unwrap_or("").to_string();
            if p.extension().and_then(|x| x.to_str()) == Some("db") && stem != "spine" {
                pools.push(stem);
            }
        }
    }
    pools.sort();
    let mut all_edges: Vec<(String, String, String, String, Value)> = Vec::new();
    for pool in &pools {
        let name = spine::schema_def::PoolName::new(pool);
        let handle = spine::pool::Pool::open_read_only(&spine::pool::Pool::path_in(&soup, &name)).map_err(|e| store(format!("reading pool `{pool}`: {e}")))?;
        let snap = handle.snapshot().map_err(store)?;
        let types: Vec<String> = handle.grammar().node_types.keys().cloned().collect();
        for ty in &types {
            for n in snap.nodes_of(ty, None).map_err(store)? {
                let key = format!("{pool}/{}", n.id.0);
                let created = snap.created_tx(&n.id).map_err(store)?.map(|x| x.to_hex());
                if created.as_ref().is_some_and(|c| lo.soup_txs.contains(c)) {
                    merged_nodes.insert(key);
                    merged_nodes.insert(n.id.0.clone());
                    continue;
                }
                let fields: BTreeMap<&String, &Value> = n.fields.iter().map(|(k, f)| (k, &f.value)).collect();
                records.insert(key.clone(), h(&json!({"type": n.node_type, "state": n.state, "sealed": n.sealed, "fields": fields})));
                for e in snap.edges_from(&n.id, None).map_err(store)? {
                    all_edges.push((key.clone(), e.kind.clone(), e.dst.to_string(), e.note.clone().unwrap_or_default(), json!(e.fields)));
                }
            }
        }
    }
    for (from, kind, to, note, fields) in all_edges {
        let to_id = to.split('@').next().unwrap_or(&to).to_string();
        if merged_nodes.contains(&to) || merged_nodes.contains(&to_id) {
            continue;
        }
        edges.insert(format!("{from} -[{kind}]-> {to} {}", h(&json!([note, fields]))));
    }
    // Dumps, their writes, and the blobs.
    let mut dumps = BTreeMap::new();
    let mut dump_txs = BTreeSet::new();
    let mut merged_shas = BTreeSet::new();
    let mut other_shas = BTreeSet::new();
    let pool = spine::schema_def::PoolName::new(spine::grammar_dump::POOL);
    let pool_file = spine::pool::Pool::path_in(&dumps_dir, &pool);
    if pool_file.is_file() {
        let p = spine::pool::Pool::open_read_only(&pool_file).map_err(|e| store(format!("reading the dumps store: {e}")))?;
        let snap = p.snapshot().map_err(store)?;
        for n in snap.nodes_of(spine::grammar_dump::DUMP, None).map_err(store)? {
            let sha = crate::dumps::blob_of(&n).1.get("sha256").and_then(Value::as_str).map(str::to_string);
            if lo.dump_ids.contains(&n.id.0) {
                merged_shas.extend(sha);
                continue;
            }
            other_shas.extend(sha);
            let fields: BTreeMap<&String, &Value> = n.fields.iter().map(|(k, f)| (k, &f.value)).collect();
            dumps.insert(n.id.0.clone(), h(&json!({"state": n.state, "fields": fields})));
        }
    }
    if dumps_dir.join(spine::spine_db::SPINE_FILE).is_file() {
        let conn = spine::offline::read_only_connection(&dumps_dir.join(spine::spine_db::SPINE_FILE)).map_err(store)?;
        let mut st = conn
            .prepare("SELECT hash, COALESCE(json_extract(initiator, '$.dispatch'), '') FROM transactions WHERE decided = 1")
            .map_err(store)?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(store)?;
        for r in rows {
            let (hash, dispatch) = r.map_err(store)?;
            if !lo.dump_dispatch_end.is_empty() && dispatch.ends_with(&lo.dump_dispatch_end) {
                continue;
            }
            dump_txs.insert(hash);
        }
    }
    // Every other file of the scope, the blobs included (by digest); the
    // stores' database files are compared by what they hold, above.
    let mut files = BTreeMap::new();
    let mut blobs = BTreeMap::new();
    let parts = [soup_name.to_string(), crate::dumps::DIR.to_string(), crate::merge::DIR.to_string(), "events".to_string(), env::cauldron::CAULDRON_MANIFEST.to_string()];
    let (_, all) = walk(&Scope { base: base.to_path_buf(), parts: parts.to_vec() })?;
    for f in all {
        if is_db(&f) {
            continue;
        }
        if !lo.own_dir.is_empty() && (f == lo.own_dir || f.starts_with(&format!("{}/", lo.own_dir))) {
            continue;
        }
        if let Some(name) = f.strip_prefix(&format!("{}/{}/", crate::dumps::DIR, crate::dumps::BLOBS)) {
            let stem = name.split('.').next().unwrap_or("").to_string();
            // A blob only the merge's dumps name is the merge's.
            if merged_shas.contains(&stem) && !other_shas.contains(&stem) && leave_out.is_some() {
                continue;
            }
            blobs.insert(name.to_string(), digest(&base.join(&f))?.0);
            continue;
        }
        files.insert(f.clone(), digest(&base.join(&f))?.0);
    }
    Ok(json!({"soup_txs": soup_txs, "records": records, "edges": edges, "dumps": dumps, "dump_txs": dump_txs, "blobs": blobs, "files": files}))
}

/// **What differs between two [`outside`]s** (the snapshot's, then the
/// target's now): per kind, the ids (counts and ids, never text). Empty:
/// nothing but the merge has written to the target since it began.
pub fn outside_changed(before: &Value, now: &Value) -> Value {
    let mut out = serde_json::Map::new();
    let set = |v: &Value| -> BTreeSet<String> { v.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect() };
    let map = |v: &Value| -> BTreeMap<String, String> {
        v.as_object().map(|o| o.iter().map(|(k, x)| (k.clone(), x.as_str().unwrap_or("").to_string())).collect()).unwrap_or_default()
    };
    for k in ["soup_txs", "edges", "dump_txs"] {
        let (a, b) = (set(&before[k]), set(&now[k]));
        let added: Vec<String> = b.difference(&a).map(|x| if k.ends_with("txs") { x[..12.min(x.len())].to_string() } else { x.rsplit_once(' ').map(|y| y.0.to_string()).unwrap_or(x.clone()) }).collect();
        let removed: Vec<String> = a.difference(&b).map(|x| if k.ends_with("txs") { x[..12.min(x.len())].to_string() } else { x.rsplit_once(' ').map(|y| y.0.to_string()).unwrap_or(x.clone()) }).collect();
        if !added.is_empty() || !removed.is_empty() {
            out.insert(k.into(), json!({"added": added.len(), "removed": removed.len(), "added_ids": added.iter().take(50).collect::<Vec<_>>(), "removed_ids": removed.iter().take(50).collect::<Vec<_>>()}));
        }
    }
    for k in ["records", "dumps", "blobs", "files"] {
        let (a, b) = (map(&before[k]), map(&now[k]));
        let added: Vec<&String> = b.keys().filter(|x| !a.contains_key(*x)).collect();
        let removed: Vec<&String> = a.keys().filter(|x| !b.contains_key(*x)).collect();
        let changed: Vec<&String> = b.iter().filter(|(x, v)| a.get(*x).is_some_and(|w| w != *v)).map(|(x, _)| x).collect();
        if !added.is_empty() || !removed.is_empty() || !changed.is_empty() {
            out.insert(
                k.into(),
                json!({"added": added.len(), "removed": removed.len(), "changed": changed.len(),
                       "added_ids": added.iter().take(50).collect::<Vec<_>>(), "removed_ids": removed.iter().take(50).collect::<Vec<_>>(), "changed_ids": changed.iter().take(50).collect::<Vec<_>>()}),
            );
        }
    }
    Value::Object(out)
}

/// [`outside_changed`] in words, one sentence per kind.
pub fn outside_said(d: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let what = |k: &str| match k {
        "soup_txs" => "soup write(s) that are not the merge's",
        "records" => "record(s) that are not the merge's",
        "edges" => "edge(s) between records that are not the merge's",
        "dumps" => "dump(s) that are not the merge's",
        "dump_txs" => "dumps-store write(s) that are not the merge's",
        "blobs" => "blob(s) no dump of the merge names",
        _ => "other file(s) of the cauldron",
    };
    for (k, v) in d.as_object().into_iter().flatten() {
        let mut parts = Vec::new();
        for (n, ids) in [("added", "added_ids"), ("changed", "changed_ids"), ("removed", "removed_ids")] {
            let c = v[n].as_u64().unwrap_or(0);
            if c > 0 {
                let list: Vec<&str> = v[ids].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                parts.push(format!("{c} {n} ({}{})", list.join(", "), if c as usize > list.len() { ", ..." } else { "" }));
            }
        }
        out.push(format!("{}: {}", what(k), parts.join("; ")));
    }
    out
}

/// **Every file of `c`'s snapshot scope now, with its SHA-256 and size**
/// (the writer locks left out): what a resumable undo records before it
/// moves anything, so a run after it was cut short can tell a file it has
/// not reached yet (still as recorded) from one it already put back (as
/// the snapshot holds it) and from one something else wrote since (neither:
/// left alone).
pub fn files_now(c: &env::Cauldron) -> Result<BTreeMap<String, (String, u64)>> {
    let s = scope(c);
    let (_, files) = walk(&s)?;
    let mut out = BTreeMap::new();
    for f in files {
        let d = digest(&s.base.join(&f))?;
        out.insert(f, d);
    }
    Ok(out)
}

/// [`files_now`] as JSON (`{path: {sha256, bytes}}`), and back.
pub fn files_json(m: &BTreeMap<String, (String, u64)>) -> Value {
    Value::Object(m.iter().map(|(k, (h, n))| (k.clone(), json!({"sha256": h, "bytes": n}))).collect())
}

pub fn files_from_json(v: &Value) -> BTreeMap<String, (String, u64)> {
    v.as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(k, x)| Some((k.clone(), (x["sha256"].as_str()?.to_string(), x["bytes"].as_u64()?))))
                .collect()
        })
        .unwrap_or_default()
}

/// **Put `c` back as the snapshot in `dir` holds it, resumably and never
/// over anything else's write**: the restore an undo makes, given `start`
/// (every file of the scope as it was, with its digest, when the undo
/// began, recorded before anything was moved). Each file is put back (or
/// moved aside into `aside`, when the snapshot does not hold it) ONLY while
/// it is exactly as `start` recorded it, or missing where `start` had it
/// (moved aside by an earlier run cut short before it put the copy in); a
/// file already as the snapshot holds it is left; any other file was
/// written by something else after the undo began and is LEFT AS IT IS and
/// listed under `kept`. Running it again after it was cut short at any
/// point finishes it, and changes nothing once it is done.
///
/// The order keeps every store whole for its readers: the dumps store
/// first, its spine file last within it; then the soup, its spine file
/// last; then the rest; then the paths `last` names (the undone merge's own
/// record). A store's spine is HELD by the undo until its own spine file is
/// put back (the snapshot's, which is not held), so until then no writer of
/// any version writes to it, and after it the store is whole as it was
/// before the merge. The caller holds the target's writer locks and has
/// checked the snapshot ([`verify`]).
pub fn restore_guarded(c: &env::Cauldron, dir: &Path, aside: &Path, start: &BTreeMap<String, (String, u64)>, last: &[String], keep: &[String]) -> Result<Value> {
    let m: Value = serde_json::from_str(&std::fs::read_to_string(dir.join(MANIFEST)).map_err(|e| store(format!("reading {}: {e}", dir.join(MANIFEST).display())))?)
        .map_err(|e| store(format!("the snapshot's manifest will not parse: {e}")))?;
    let s = scope(c);
    let want: BTreeMap<String, (String, u64)> = m["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| Some((f["path"].as_str()?.to_string(), (f["sha256"].as_str()?.to_string(), f["bytes"].as_u64()?))))
        .collect();
    let want_dirs: BTreeSet<String> = m["dirs"].as_array().into_iter().flatten().filter_map(|d| d.as_str().map(str::to_string)).collect();
    let (have_dirs, have_files) = walk(&s)?;
    let soup_name = s.parts[0].clone();
    let dumps_name = crate::dumps::DIR.to_string();
    let spine = spine::spine_db::SPINE_FILE;
    let is_spine = |p: &str, store: &str| p == format!("{store}/{spine}") || p.starts_with(&format!("{store}/{spine}-"));
    // The group a path is restored in.
    let group = |p: &str| -> u8 {
        if last.iter().any(|l| p == l || p.starts_with(&format!("{l}/"))) {
            5
        } else if is_spine(p, &dumps_name) {
            1
        } else if p.starts_with(&format!("{dumps_name}/")) {
            0
        } else if is_spine(p, &soup_name) {
            3
        } else if p.starts_with(&format!("{soup_name}/")) {
            2
        } else {
            4
        }
    };
    let mut all: BTreeSet<String> = want.keys().cloned().collect();
    all.extend(have_files.iter().cloned());
    all.extend(start.keys().cloned());
    let mut order: Vec<String> = all.into_iter().collect();
    // Within a store's spine group, the side files before the spine file.
    order.sort_by_key(|p| (group(p), p.ends_with(&format!("/{spine}")), p.clone()));
    env::private::make_dir(aside).map_err(|e| store(format!("making {}: {e}", aside.display())))?;
    let move_aside = |rel: &str| -> Result<()> {
        let mut at = aside.to_path_buf();
        for part in Path::new(rel).parent().into_iter().flat_map(|p| p.components()) {
            at = at.join(part);
            env::private::make_dir(&at).map_err(|e| store(format!("making {}: {e}", at.display())))?;
        }
        let mut to = aside.join(rel);
        let mut n = 1;
        while to.exists() {
            to = aside.join(format!("{rel}.{n}"));
            n += 1;
        }
        std::fs::rename(s.base.join(rel), &to).map_err(|e| store(format!("moving {} aside: {e}", s.base.join(rel).display())))
    };
    for d in &want_dirs {
        env::private::make_dir(&s.base.join(d)).map_err(|e| store(format!("making {}: {e}", s.base.join(d).display())))?;
    }
    let (mut put_back, mut moved, mut unchanged, mut ops) = (0usize, 0usize, 0usize, 0usize);
    let mut kept: Vec<String> = Vec::new();
    for f in &order {
        // Left exactly as it is (a renamed cauldron's manifest).
        if keep.contains(f) {
            continue;
        }
        // A copy this restore (or one before it, cut short by a full disk or
        // a crash) was putting in place: its own, removed, never "kept".
        if f.rsplit('/').next().is_some_and(|n| n.starts_with(".restoring-")) {
            let _ = std::fs::remove_file(s.base.join(f));
            continue;
        }
        let target = s.base.join(f);
        let now = if have_files.contains(f) || target.is_file() { Some(digest(&target)?) } else { None };
        let snap = want.get(f);
        let began = start.get(f);
        if now.as_ref() == snap {
            if now.is_some() {
                unchanged += 1;
            }
            continue;
        }
        // SQLite's shared-memory index, and an empty write-ahead log, that
        // neither the undo's start nor the snapshot holds: what any reader
        // leaves beside a database (this undo's own checks included), not
        // a write. Left in place, never listed as kept.
        if snap.is_none() && began.is_none() && (f.ends_with("-shm") || (f.ends_with("-wal") && now.as_ref().is_some_and(|n| n.1 == 0))) {
            continue;
        }
        // The same files as the snapshot copied them (a store open while it
        // was taken): a shared-memory index SQLite rebuilds from its
        // database, and a write-ahead log with nothing in it, now gone or
        // as empty. Neither holds a write: never listed as "kept".
        let empty_wal = |x: Option<&(String, u64)>| x.is_none_or(|x| x.1 == 0);
        let no_write = f.ends_with("-shm") || (f.ends_with("-wal") && empty_wal(snap) && empty_wal(now.as_ref()) && empty_wal(began));
        let ours = match (&now, began) {
            (Some(n), Some(b)) => n == b,
            // Missing where the undo began with it: an earlier run moved it
            // aside and was cut short before it put the copy in.
            (None, Some(_)) => true,
            // A file that appeared after the undo began, or one missing
            // that the undo never had: something else's.
            (Some(_), None) => false,
            (None, None) => snap.is_none(),
        };
        if !ours {
            if !no_write {
                kept.push(f.clone());
            }
            continue;
        }
        match snap {
            None => {
                if now.is_some() {
                    move_aside(f)?;
                    moved += 1;
                    ops += 1;
                    crate::merge::fault::at_undo(&format!("file:{ops}"));
                }
            }
            Some((sha, n)) => {
                let parent = target.parent().unwrap_or(Path::new("."));
                env::private::make_dir(parent).map_err(|e| store(format!("making {}: {e}", parent.display())))?;
                let tmp = parent.join(format!(".restoring-{}-{}", std::process::id(), target.file_name().and_then(|x| x.to_str()).unwrap_or("f")));
                let _ = std::fs::remove_file(&tmp);
                if let Err(e) = clone_file(&dir.join(FILES).join(f), &tmp) {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(e);
                }
                let (h, k) = digest(&tmp)?;
                if &h != sha || k != *n {
                    let _ = std::fs::remove_file(&tmp);
                    refuse!(Code::Store, "REFUSED: restoring {f}: the copy put back is not the snapshot's ({k} bytes). The undo stopped here; run it again.");
                }
                if now.is_some() {
                    move_aside(f)?;
                    moved += 1;
                }
                std::fs::rename(&tmp, &target).map_err(|e| store(format!("putting {} back: {e}", target.display())))?;
                sync_dir(parent);
                put_back += 1;
                ops += 1;
                crate::merge::fault::at_undo(&format!("file:{ops}"));
            }
        }
    }
    // Directories the snapshot did not have, deepest first, once empty.
    let mut extra: Vec<&String> = have_dirs.iter().filter(|d| !want_dirs.contains(*d)).collect();
    extra.sort_by_key(|d| std::cmp::Reverse(d.matches('/').count()));
    let mut left = Vec::new();
    for d in extra {
        if std::fs::remove_dir(s.base.join(d)).is_err() && s.base.join(d).is_dir() {
            left.push(d.clone());
        }
    }
    Ok(json!({"put_back": put_back, "unchanged": unchanged, "moved_aside": moved, "aside": aside.display().to_string(), "kept": kept, "dirs_left": left}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_writer_locks_and_the_databases_are_named() {
        assert!(is_lock("soup/writer.lock") && is_lock("dumps/writer.lock"));
        assert!(!is_lock("soup/region-locks.json"));
        assert!(is_db("soup/ws.db") && is_db("soup/spine.db-wal") && !is_db("dumps/blobs/x.txt"));
    }

    #[test]
    fn what_changed_outside_a_merge_is_said_by_count_and_id() {
        let a = json!({"soup_txs": ["tx-first-write"], "records": {"ws/T-1": "x"}, "edges": [], "dumps": {}, "dump_txs": [], "blobs": {}, "files": {"soup/region-locks.json": "y"}});
        assert!(outside_changed(&a, &a).as_object().unwrap().is_empty());
        let b = json!({"soup_txs": ["tx-first-write", "tx-later-write"], "records": {"ws/T-1": "z", "ws/T-9": "w"}, "edges": [], "dumps": {"DM-4": "v"}, "dump_txs": [], "blobs": {}, "files": {}});
        let d = outside_changed(&a, &b);
        assert_eq!((d["records"]["added"].clone(), d["records"]["changed"].clone()), (json!(1), json!(1)), "{d}");
        assert_eq!(d["soup_txs"]["added_ids"], json!(["tx-later-wri"]));
        let said = outside_said(&d).join("; ");
        assert!(said.contains("record(s) that are not the merge's: 1 added (ws/T-9); 1 changed (ws/T-1)") && said.contains("region-locks.json"), "{said}");
    }
}
