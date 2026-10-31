//! PL-2b — `threads/{thread_id}/source_census`, the read-only per-thread
//! source census, driven through the real `/ws` door.
//!
//! Every request here goes over a WebSocket to the in-process chassis, so the
//! path grammar (`WS_METHODS` → subject binding → the arm) is exercised exactly
//! as a consumer exercises it. No claude turn is ever spawned. The account's
//! transcript store is a config dir under each test's own tempdir, and
//! `THREADS_CLAUDE_HOME` points the machine-default store at a scratch dir, so
//! nothing here can read or write inside the operator's `~/.claude`.
//!
//! The only writes in this file are FIXTURE SETUP (planting a transcript,
//! planting `turns/<id>/` files) and the CREATION SENTINEL. The code under test
//! writes nothing, and the "creates nothing" assertions say so over the WHOLE
//! test tempdir (data dir + the account's transcript store), not one guessed
//! path: `tree()` snapshots every entry before and after the census.
//!
//! **PL-2c: a "creates nothing" assertion must be able to see a creation.** The
//! PL-2b asserts each checked ONE path the census never had a reason to touch
//! (a made-up slug, a `turns/` dir for an id that errors first), so a census
//! that wrote anywhere else passed all three. Each one now (a) plants a
//! sentinel at the depth the census would write and asserts `tree()` saw it,
//! then removes it and asserts the tree is back, and only then (b) asserts the
//! census left the tree as it found it.
//!
//! **PL-2d: the snapshot sees EDITS too, not only creations and removals.**
//! `tree()` records each file's (size, mtime), so a census that appended to
//! the transcript it found is caught; `sentinel_sees_an_edit` proves it before
//! the snapshot is trusted. The assertion is therefore "creates, removes or
//! modifies nothing".

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use threads::accounts::projects_dir_for;
use threads::config::Config;
use threads::settings::ThreadSettings;
use threads::{build_manager, ThreadManager};

const ACCOUNT: &str = "aa-first";

fn config(tmp: &Path) -> Config {
    // A FIXED scratch dir, never a per-test one: the variable is process-wide
    // and these tests run in parallel. Every assertion below reads the
    // REGISTERED account's own store (`<tmp>/aa-first`), never this one.
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    let mut accounts = BTreeMap::new();
    accounts.insert(
        ACCOUNT.to_string(),
        tmp.join(ACCOUNT).to_string_lossy().to_string(),
    );
    Config {
        data_dir: Some(tmp.to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

async fn start() -> (ThreadManager, SocketAddr, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    threads::clock::testing::ticking();
    let manager = build_manager(config(tmp.path())).unwrap();
    let chassis = threads::chassis::chassis(Arc::new(manager.clone()));
    let listener = shapewire::server::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let (addr, h) = chassis
        .serve_on_with_shutdown(listener, std::future::pending::<()>())
        .unwrap();
    std::mem::forget(h);
    (manager, addr, tmp)
}

struct Client {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

impl Client {
    async fn connect(addr: SocketAddr) -> Self {
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
            .await
            .unwrap();
        let mut c = Self { ws };
        let _welcome = c.next().await.expect("a welcome");
        c
    }

    async fn next(&mut self) -> Option<Value> {
        use tokio_tungstenite::tungstenite::Message;
        loop {
            let got =
                tokio::time::timeout(std::time::Duration::from_secs(5), self.ws.next()).await;
            match got {
                Err(_) | Ok(None) => return None,
                Ok(Some(Ok(Message::Text(t)))) => return serde_json::from_str(&t).ok(),
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(_))) => return None,
            }
        }
    }

    async fn request(&mut self, id: &str, method: &str, params: Value) -> Value {
        use tokio_tungstenite::tungstenite::Message;
        let frame = json!({"type":"request","id":id,"method":method,"params":params});
        self.ws
            .send(Message::Text(frame.to_string().into()))
            .await
            .unwrap();
        loop {
            let Some(f) = self.next().await else {
                panic!("no response to {method}");
            };
            if f["type"] == "response" && f["correlate"] == id {
                return f;
            }
        }
    }

    /// The census for `id`, which must SUCCEED — the payload.
    async fn census(&mut self, id: &str) -> Value {
        let r = self
            .request(&format!("c-{id}"), &format!("threads/{id}/source_census"), json!({}))
            .await;
        assert_eq!(r["outcome"]["type"], json!("success"), "census of {id}: {r}");
        r["outcome"]["payload"].clone()
    }
}

fn new_thread(m: &ThreadManager, cwd: &Path) -> String {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    m.create_thread(None, s).unwrap()
}

/// FIXTURE: plant `<store>/projects/<slug>/<session>.jsonl`.
fn plant_transcript(store: &Path, slug: &str, session: &str) -> PathBuf {
    let store = store.to_string_lossy().to_string();
    let dir = projects_dir_for(Some(&store), Path::new("/nonexistent-home")).join(slug);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{session}.jsonl"));
    std::fs::write(&path, format!("{{\"sessionId\":\"{session}\",\"type\":\"user\"}}\n")).unwrap();
    path
}

/// FIXTURE: plant `<data_dir>/turns/<thread>/<name>` for each name.
fn plant_turns(data_dir: &Path, thread: &str, names: &[&str]) -> PathBuf {
    let dir = data_dir.join("turns").join(thread);
    std::fs::create_dir_all(&dir).unwrap();
    for n in names {
        std::fs::write(dir.join(n), b"{}\n").unwrap();
    }
    dir
}

/// A file's (size, mtime); `None` for a directory, whose mtime moves whenever
/// an entry is added or removed (creation and removal are already seen by the
/// key set).
type Stamp = Option<(u64, std::time::SystemTime)>;

/// Every entry under `root`, recursively, as a path relative to `root`, with
/// each FILE's (size, mtime) — the instrument for "creates, removes or modifies
/// nothing". Panics on any read error rather than skipping: a snapshot that
/// omits what it cannot read is a claim, not a count.
fn tree(root: &Path) -> BTreeMap<PathBuf, Stamp> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Stamp>) {
        for e in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("tree: {}: {e}", dir.display())) {
            let e = e.unwrap();
            let p = e.path();
            let md = e.metadata().unwrap();
            let stamp = if md.is_dir() { None } else { Some((md.len(), md.modified().unwrap())) };
            out.insert(p.strip_prefix(root).unwrap().to_path_buf(), stamp);
            if e.file_type().unwrap().is_dir() {
                walk(root, &p, out);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// POSITIVE SENTINEL for `tree()`: create `rel` (and any missing parents)
/// under `root`, assert the snapshot SAW it, remove what was created, and
/// assert the snapshot is back to `before`. Proves the instrument can observe a
/// creation at that depth, in that scope, before it is trusted to report none.
fn sentinel_sees_a_creation(root: &Path, before: &BTreeMap<PathBuf, Stamp>, rel: &Path) {
    // The topmost ancestor that does not exist yet is what we remove after.
    let mut top = rel.to_path_buf();
    while let Some(parent) = top.parent() {
        if parent.as_os_str().is_empty() || root.join(parent).exists() {
            break;
        }
        top = parent.to_path_buf();
    }
    assert!(!root.join(rel).exists(), "sentinel path must be new: {}", rel.display());
    std::fs::create_dir_all(root.join(rel).parent().unwrap()).unwrap();
    std::fs::write(root.join(rel), b"sentinel\n").unwrap();
    let seen = tree(root);
    assert!(seen.contains_key(rel), "tree() missed the sentinel {}", rel.display());
    assert_ne!(&seen, before, "tree() did not change on a creation");
    if top == rel {
        std::fs::remove_file(root.join(&top)).unwrap();
    } else {
        std::fs::remove_dir_all(root.join(&top)).unwrap();
    }
    assert_eq!(&tree(root), before, "sentinel cleanup left the tree changed");
}

/// POSITIVE SENTINEL for `tree()` seeing an EDIT (PL-2d): append to the
/// existing file `rel`, assert the snapshot changed AT `rel` with the key set
/// unchanged, then restore its bytes and mtime and assert the tree is back to
/// `before`. Proves the instrument sees a modification before it is trusted to
/// report none.
fn sentinel_sees_an_edit(root: &Path, before: &BTreeMap<PathBuf, Stamp>, rel: &Path) {
    use std::io::Write;
    let path = root.join(rel);
    let bytes = std::fs::read(&path).unwrap();
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"{\"sentinel\":1}\n").unwrap();
    let seen = tree(root);
    assert_eq!(
        seen.keys().collect::<Vec<_>>(),
        before.keys().collect::<Vec<_>>(),
        "an edit is not a creation"
    );
    assert_ne!(seen.get(rel), before.get(rel), "tree() missed the edit to {}", rel.display());
    std::fs::write(&path, &bytes).unwrap();
    std::fs::File::options().write(true).open(&path).unwrap().set_modified(mtime).unwrap();
    assert_eq!(&tree(root), before, "edit-sentinel cleanup left the tree changed");
}

/// The census left `root` exactly as it found it: no entry added, removed or
/// modified (a file's size or mtime changed).
fn assert_created_nothing(root: &Path, before: &BTreeMap<PathBuf, Stamp>, what: &str) {
    let after = tree(root);
    let added: Vec<_> = after.keys().filter(|k| !before.contains_key(*k)).collect();
    let removed: Vec<_> = before.keys().filter(|k| !after.contains_key(*k)).collect();
    let modified: Vec<_> = after
        .iter()
        .filter(|(k, v)| before.get(*k).is_some_and(|b| b != *v))
        .map(|(k, _)| k)
        .collect();
    assert!(
        added.is_empty() && removed.is_empty() && modified.is_empty(),
        "{what}: the census must create, remove or modify nothing — added {added:?}, removed {removed:?}, modified {modified:?}"
    );
}

// ── 1. transcript {path, exists} — exists, paired with absent ───────────────

/// A thread whose session has a transcript in its account's `projects/*`
/// reports `exists: true` and that exact path; a thread with no file there
/// reports `exists: false, path: null`. The pair is what rules out a constant.
#[tokio::test]
async fn transcript_presence_is_reported_and_an_absent_one_is_null_not_true() {
    let (m, addr, tmp) = start().await;
    let store = tmp.path().join(ACCOUNT);
    let with = new_thread(&m, tmp.path());
    let without = new_thread(&m, tmp.path());
    let planted = plant_transcript(&store, "-tmp-pl2b-census", &with);

    let mut c = Client::connect(addr).await;
    let before = tree(tmp.path());
    // The instrument can see a transcript appear at the depth the census reads.
    sentinel_sees_a_creation(
        tmp.path(),
        &before,
        &Path::new(ACCOUNT).join("projects").join("-pl2c-sentinel").join(format!("{without}.jsonl")),
    );
    // PL-2d: ...and can see the transcript the census READS being edited.
    sentinel_sees_an_edit(tmp.path(), &before, planted.strip_prefix(tmp.path()).unwrap());
    let a = c.census(&with).await;
    assert_eq!(a["transcript"]["exists"], json!(true), "{a}");
    assert_eq!(
        a["transcript"]["path"],
        json!(planted.to_string_lossy().to_string()),
        "{a}"
    );
    assert_eq!(a["transcript"]["store_error"], Value::Null, "{a}");

    let b = c.census(&without).await;
    assert_eq!(b["transcript"]["exists"], json!(false), "{b}");
    assert_eq!(b["transcript"]["path"], Value::Null, "{b}");
    assert_eq!(b["transcript"]["store_error"], Value::Null, "{b}");
    // Read-only: looking for it did not create it.
    let would_be = projects_dir_for(Some(&store.to_string_lossy()), Path::new("/nonexistent-home"))
        .join("-tmp-pl2b-census")
        .join(format!("{without}.jsonl"));
    assert!(!would_be.exists(), "the census must not create a transcript");
    assert_created_nothing(tmp.path(), &before, "transcript census");
    drop(tmp);
}

// ── 2. turns/ capture count — N mixed, paired with absent and .mcp-only ─────

/// `turns/<id>/` with three files of mixed kinds counts 3 (the raw count — v1
/// does not tell `.jsonl` from `.mcp.json`); a thread with NO `turns/<id>/`
/// counts 0 and the census does not create the dir; a thread whose only
/// capture is an `.mcp.json` sidecar counts 1.
#[tokio::test]
async fn turns_capture_count_is_the_raw_entry_count_and_absent_is_zero() {
    let (m, addr, tmp) = start().await;
    let mixed = new_thread(&m, tmp.path());
    let absent = new_thread(&m, tmp.path());
    let mcp_only = new_thread(&m, tmp.path());
    plant_turns(
        tmp.path(),
        &mixed,
        &["cookie-a.jsonl", "cookie-b.jsonl", "cookie-b.mcp.json"],
    );
    plant_turns(tmp.path(), &mcp_only, &["cookie-c.mcp.json"]);

    let mut c = Client::connect(addr).await;
    let before = tree(tmp.path());
    // The instrument can see `turns/<absent>/` appear.
    sentinel_sees_a_creation(
        tmp.path(),
        &before,
        &Path::new("turns").join(&absent).join("sentinel.jsonl"),
    );
    // The `.mcp.json`-only case first: a count that skipped sidecars answers 0
    // here, which is the paired case it changes.
    let e = c.census(&mcp_only).await;
    assert_eq!(e["turns"]["count"], json!(1), ".mcp.json-only: {e}");

    let a = c.census(&mixed).await;
    assert_eq!(a["turns"]["count"], json!(3), "mixed .jsonl/.mcp.json: {a}");

    let b = c.census(&absent).await;
    assert_eq!(b["turns"]["count"], json!(0), "absent turns dir: {b}");
    assert!(
        !tmp.path().join("turns").join(&absent).exists(),
        "the census must not create turns/<id>/"
    );
    assert_created_nothing(tmp.path(), &before, "turns census");

    drop(tmp);
}

// ── 3. account + cwd agree with threads/list's own row ──────────────────────

/// Cross-checked against the EXISTING route, not a second hard-coded fixture:
/// whatever `threads/list` says this thread's account and cwd are, the census
/// says the same. A sentinel first proves the two threads compared are
/// genuinely different rows (different cwd), so agreement is not `x == x`.
#[tokio::test]
async fn account_and_cwd_match_threads_list_for_the_same_thread() {
    let (m, addr, tmp) = start().await;
    let cwd_a = tmp.path().join("proj-a");
    let cwd_b = tmp.path().join("proj-b");
    std::fs::create_dir_all(&cwd_a).unwrap();
    std::fs::create_dir_all(&cwd_b).unwrap();
    let a = new_thread(&m, &cwd_a);
    let b = new_thread(&m, &cwd_b);

    let mut c = Client::connect(addr).await;
    let r = c.request("list", "threads/list", json!({})).await;
    assert_eq!(r["outcome"]["type"], json!("success"), "{r}");
    let rows = r["outcome"]["payload"]["threads"].as_array().unwrap().clone();
    let row = |id: &str| {
        rows.iter()
            .find(|t| t["thread_id"] == json!(id))
            .unwrap_or_else(|| panic!("{id} not in threads/list: {rows:?}"))
            .clone()
    };
    let (la, lb) = (row(&a), row(&b));
    // Sentinel: the two rows differ, and each names a real account.
    assert_ne!(la["cwd"], lb["cwd"], "fixture: two distinct cwds");
    assert_eq!(la["account"], json!(ACCOUNT), "fixture: {la}");

    for (id, listed) in [(&a, &la), (&b, &lb)] {
        let got = c.census(id).await;
        assert_eq!(got["thread_id"], json!(id), "{got}");
        assert_eq!(got["account"], listed["account"], "account vs threads/list: {got} / {listed}");
        assert_eq!(got["cwd"], listed["cwd"], "cwd vs threads/list: {got} / {listed}");
    }
    drop(tmp);
}

// ── 4. unknown thread → error, not a zero-filled row ────────────────────────

/// An id that names no thread is REFUSED. A zero-filled row (`exists: false`,
/// `count: 0`) would be indistinguishable from a real thread with no sources,
/// which is the one answer a census must never fake. Paired with a known thread
/// on the same connection, which succeeds, so the refusal is about the id.
#[tokio::test]
async fn an_unknown_thread_is_an_error_not_a_zero_filled_row() {
    let (m, addr, tmp) = start().await;
    let known = new_thread(&m, tmp.path());
    let mut c = Client::connect(addr).await;
    let before = tree(tmp.path());
    sentinel_sees_a_creation(
        tmp.path(),
        &before,
        &Path::new("turns").join("no-such-thread-pl2b").join("sentinel.jsonl"),
    );

    let r = c
        .request("u", "threads/no-such-thread-pl2b/source_census", json!({}))
        .await;
    assert_eq!(r["outcome"]["type"], json!("error"), "{r}");
    assert!(
        r["outcome"]["error"].as_str().unwrap_or("").contains("no such thread"),
        "{r}"
    );
    assert!(r["outcome"]["payload"].is_null(), "no row rides an error: {r}");
    assert!(
        !tmp.path().join("turns").join("no-such-thread-pl2b").exists(),
        "an unknown id creates nothing"
    );
    assert_created_nothing(tmp.path(), &before, "unknown-id census");

    let ok = c.census(&known).await;
    assert_eq!(ok["thread_id"], json!(known), "{ok}");
    drop(tmp);
}

// ── 5. an unreadable turns/<id>/ is an ERROR, not 0 captures (PL-2c) ────────

/// Restores a chmod-000 directory's mode on drop, so a failing assertion still
/// leaves the tempdir removable.
struct Restore(PathBuf);
impl Drop for Restore {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// A `turns/<id>/` that EXISTS but cannot be listed — mode 000, or a plain file
/// where the directory should be — is REFUSED, naming the dir; it is never
/// counted `0`, which is the answer for a thread with genuinely no captures.
/// Paired on the same connection with a readable dir (counts 1) and an absent
/// one (counts 0), so the refusal is about readability, not about every
/// thread.
#[tokio::test]
async fn an_unreadable_turns_dir_is_an_error_not_zero_captures() {
    use std::os::unix::fs::PermissionsExt;
    let (m, addr, tmp) = start().await;
    let locked = new_thread(&m, tmp.path());
    let not_a_dir = new_thread(&m, tmp.path());
    let fine = new_thread(&m, tmp.path());
    let absent = new_thread(&m, tmp.path());
    let empty = new_thread(&m, tmp.path());

    let locked_dir = plant_turns(tmp.path(), &locked, &["cookie-l.jsonl"]);
    std::fs::set_permissions(&locked_dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _restore = Restore(locked_dir.clone());
    // Fixture sentinel: the dir really is unreadable to this process (it would
    // not be under root), so a success below cannot be a readable-dir answer.
    assert!(std::fs::read_dir(&locked_dir).is_err(), "fixture: {} must be unreadable", locked_dir.display());

    std::fs::create_dir_all(tmp.path().join("turns")).unwrap();
    std::fs::write(tmp.path().join("turns").join(&not_a_dir), b"not a dir\n").unwrap();
    plant_turns(tmp.path(), &fine, &["cookie-f.jsonl"]);
    // PL-2d: a readable EMPTY `turns/<id>/` — exists, lists, holds nothing.
    let empty_dir = plant_turns(tmp.path(), &empty, &[]);
    assert!(
        empty_dir.is_dir() && std::fs::read_dir(&empty_dir).unwrap().next().is_none(),
        "fixture: {} must exist, be readable and be empty",
        empty_dir.display()
    );

    let mut c = Client::connect(addr).await;
    for (id, why) in [(&locked, "mode 000"), (&not_a_dir, "a file, not a dir")] {
        let r = c
            .request(&format!("x-{id}"), &format!("threads/{id}/source_census"), json!({}))
            .await;
        assert_eq!(r["outcome"]["type"], json!("error"), "{why}: must refuse, not count 0: {r}");
        let msg = r["outcome"]["error"].as_str().unwrap_or("");
        assert!(msg.contains("unreadable") && msg.contains(id.as_str()), "{why}: names the dir: {r}");
        assert!(r["outcome"]["payload"].is_null(), "{why}: no row rides an error: {r}");
    }

    // The pair: readable counts what is there, absent counts 0.
    let f = c.census(&fine).await;
    assert_eq!(f["turns"]["count"], json!(1), "readable turns dir: {f}");
    let a = c.census(&absent).await;
    assert_eq!(a["turns"]["count"], json!(0), "absent turns dir: {a}");
    // PL-2d: readable but empty is SUCCESS with 0, not a refusal.
    let z = c.census(&empty).await;
    assert_eq!(z["turns"]["count"], json!(0), "readable empty turns dir: {z}");

    drop(_restore);
    drop(tmp);
}

// ── 6. an unreadable transcript STORE is store_error, not absent (PL-2d) ────

/// PL-2c-1: `<store>/projects` that EXISTS but cannot be listed (mode 000), or
/// a slug dir under it that cannot be searched (mode 000), is reported as
/// `transcript.store_error` naming the path — never `exists: false,
/// store_error: null`, which is the answer for a store that genuinely holds no
/// transcript. Paired, for the SAME thread, with the store absent (null error),
/// readable (found, null error), and readable again after each lock is lifted.
#[tokio::test]
async fn an_unreadable_transcript_store_is_a_store_error_not_absent() {
    use std::os::unix::fs::PermissionsExt;
    let (m, addr, tmp) = start().await;
    let store = tmp.path().join(ACCOUNT);
    let id = new_thread(&m, tmp.path());
    let projects = projects_dir_for(Some(&store.to_string_lossy()), Path::new("/nonexistent-home"));
    let mut c = Client::connect(addr).await;

    // Absent store: read fine, holds nothing — no error.
    assert!(!projects.exists(), "fixture: {} must not exist yet", projects.display());
    let a = c.census(&id).await;
    assert_eq!(a["transcript"]["exists"], json!(false), "absent store: {a}");
    assert_eq!(a["transcript"]["store_error"], Value::Null, "absent store: {a}");

    // Readable store holding the transcript.
    let planted = plant_transcript(&store, "-tmp-pl2d-slug", &id);
    let found = json!(planted.to_string_lossy().to_string());
    let r = c.census(&id).await;
    assert_eq!(r["transcript"]["path"], found, "readable: {r}");
    assert_eq!(r["transcript"]["store_error"], Value::Null, "readable: {r}");

    for (locked, why) in [(projects.clone(), "projects/ mode 000"), (planted.parent().unwrap().to_path_buf(), "slug dir mode 000")] {
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let restore = Restore(locked.clone());
        // Fixture sentinel: really unreadable to this process (not under root).
        assert!(std::fs::read_dir(&locked).is_err(), "fixture: {} must be unreadable", locked.display());
        let u = c.census(&id).await;
        assert_eq!(u["transcript"]["exists"], json!(false), "{why}: {u}");
        assert_eq!(u["transcript"]["path"], Value::Null, "{why}: {u}");
        let err = u["transcript"]["store_error"].as_str().unwrap_or_else(|| panic!("{why}: store_error must be set: {u}"));
        assert!(
            err.contains("unreadable") && err.contains(&locked.to_string_lossy().to_string()),
            "{why}: names the path: {u}"
        );
        drop(restore);
        let back = c.census(&id).await;
        assert_eq!(back["transcript"]["path"], found, "{why} lifted: {back}");
        assert_eq!(back["transcript"]["store_error"], Value::Null, "{why} lifted: {back}");
    }

    // PL-2d inspect F-1 — the MIXED layout: a locked SIBLING slug dir beside
    // the readable hit. The hit is still found, by the census (no error) and by
    // the lossy `locate_transcript` every pre-census caller uses.
    let sibling = projects.join("-tmp-pl2d-locked-sibling");
    std::fs::create_dir_all(&sibling).unwrap();
    std::fs::set_permissions(&sibling, std::fs::Permissions::from_mode(0o000)).unwrap();
    let restore = Restore(sibling.clone());
    assert!(std::fs::read_dir(&sibling).is_err(), "fixture: {} must be unreadable", sibling.display());
    let mx = c.census(&id).await;
    assert_eq!(mx["transcript"]["path"], found, "readable hit beside a locked sibling: {mx}");
    assert_eq!(mx["transcript"]["exists"], json!(true), "readable hit beside a locked sibling: {mx}");
    assert_eq!(mx["transcript"]["store_error"], Value::Null, "readable hit beside a locked sibling: {mx}");
    assert_eq!(
        threads::accounts::locate_transcript(Some(&store.to_string_lossy()), Path::new("/nonexistent-home"), &id),
        Some(planted.clone()),
        "lossy locate_transcript: a locked sibling must not hide the hit"
    );
    // ...and with the hit's own dir locked too, nothing readable is left: error.
    let hit_dir = planted.parent().unwrap().to_path_buf();
    std::fs::set_permissions(&hit_dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    let restore_hit = Restore(hit_dir.clone());
    let all = c.census(&id).await;
    assert_eq!(all["transcript"]["path"], Value::Null, "all unreadable: {all}");
    assert!(all["transcript"]["store_error"].as_str().is_some_and(|e| e.contains("unreadable")), "all unreadable: {all}");
    drop(restore_hit);
    drop(restore);
    drop(tmp);
}

// ── 7. a store (SQL) failure is not "no such thread" (PL-2d) ────────────────

/// `Store::get_thread` maps only "no row" to `None`. A KNOWN thread whose row
/// cannot be read (here: the `threads` table's schema broken under the running
/// manager) is refused with the SQL error, never with "no such thread". The
/// pair — an unknown id on a HEALTHY store is "no such thread" — is test 4, and
/// is repeated here before the break so both answers come from one store.
#[tokio::test]
async fn a_store_failure_is_an_error_not_no_such_thread() {
    let (m, addr, tmp) = start().await;
    let known = new_thread(&m, tmp.path());
    let mut c = Client::connect(addr).await;
    let ok = c.census(&known).await;
    assert_eq!(ok["thread_id"], json!(known), "fixture: healthy census: {ok}");
    let u = c.request("u0", "threads/no-such-thread-pl2d/source_census", json!({})).await;
    assert!(u["outcome"]["error"].as_str().unwrap_or("").contains("no such thread"), "healthy unknown: {u}");

    let db = tmp.path().join("threads.db");
    assert!(db.is_file(), "fixture: {} must be the manager's store", db.display());
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("ALTER TABLE threads RENAME COLUMN settings_json TO settings_json_pl2d;")
        .unwrap();

    let r = c
        .request("k", &format!("threads/{known}/source_census"), json!({}))
        .await;
    assert_eq!(r["outcome"]["type"], json!("error"), "a broken store must refuse: {r}");
    let msg = r["outcome"]["error"].as_str().unwrap_or("");
    assert!(!msg.contains("no such thread"), "a SQL failure is not an unknown id: {r}");
    assert!(msg.contains("settings_json"), "names the SQL failure: {r}");
    drop(tmp);
}
