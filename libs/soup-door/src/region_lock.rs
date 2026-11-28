//! **Region locks at the door**: the guard every writer of a soup installs
//! on its handle, the revisions a ripple reads, and the refusal of the
//! whole-store acts while a region is held.
//!
//! The rows live in the soup (`env::region_locks`); which regions a write
//! names is the spine's (`spine::regions`). The [`Guard`] joins the two: a
//! commit naming a region another holder holds is refused `region_locked`,
//! before anything is recorded, with who holds it, since when and until when.
//! A ripple's own write is not refused by its own rows.
//!
//! Installed by the `mcp` service's writer for every cauldron it opens, and
//! by `cauldron soup import` when it writes the store itself. A table that
//! cannot be read refuses every commit (it never reads as empty), naming
//! the command that fixes it: `cauldron soup recover`, which sets the
//! damaged file aside under a dated name, and only then reads the soup as
//! holding no lock, when it holds the soup's writer lock itself and no
//! ripple run of the cauldron is live in the ripples journal
//! ([`set_aside_damaged`]).

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};
use spine::envelope::Initiator;
use spine::pool::{NodeId, Pool};
use spine::regions::{RegionGuard, RegionRow};
use spine::schema_def::PoolName;

use crate::reads::DEFAULT_POOL;
use crate::{Code, Error, Result};

/// The guard for cauldron `c` (module docs).
pub struct Guard {
    pub cauldron: env::Cauldron,
}

impl Guard {
    pub fn new(c: &env::Cauldron) -> std::sync::Arc<Guard> {
        std::sync::Arc::new(Guard { cauldron: c.clone() })
    }
}

/// The run a ripple initiator writes for; `None` for every other writer.
pub fn run_of(initiator: &Initiator) -> Option<&str> {
    match initiator {
        Initiator::Ripple { run_id, .. } => Some(run_id.as_str()),
        _ => None,
    }
}

/// The held rows that conflict with `wanted` for a writer of run `run`
/// (`None`: not a lock holder), at `now_ms`, less any row of a run that has
/// already landed (it is done: [`env::region_locks::Table::sweep`]).
pub fn conflicts(c: &env::Cauldron, t: &env::region_locks::Table, now_ms: i64, run: Option<&str>, wanted: &[env::region_locks::Want]) -> Result<Vec<env::region_locks::Row>> {
    let mut landed: BTreeMap<String, bool> = BTreeMap::new();
    let mut out = Vec::new();
    for r in t.conflicts(now_ms, run, wanted) {
        let done = match landed.get(&r.holder.run) {
            Some(d) => *d,
            None => {
                let d = !crate::ripple::run_landed(c, &r.holder.run)?.is_empty();
                landed.insert(r.holder.run.clone(), d);
                d
            }
        };
        if !done {
            out.push(r);
        }
    }
    Ok(out)
}

/// `(pool, region, mode)` of each row.
pub fn wants(rows: &[RegionRow]) -> Vec<env::region_locks::Want> {
    rows.iter().map(|r| (r.pool.as_str().to_string(), r.region.0.clone(), r.mode_word().to_string())).collect()
}

/// The command that sets a damaged table aside ([`set_aside_damaged`]).
pub fn recover_command(c: &env::Cauldron) -> String {
    format!("cauldron soup recover --env {} --cauldron {}", c.env, c.name)
}

/// The table, or the refusal every writer gets when it cannot be read,
/// naming the command that fixes it.
pub fn load(c: &env::Cauldron) -> Result<env::region_locks::Table> {
    env::region_locks::load(c).map_err(|e| {
        Error::new(
            Code::Store,
            format!(
                "{e:#}. The soup's region locks cannot be read, so nothing was written (a write never guesses that no region is held). \
                 To fix it: stop the `mcp` service if it is running (`cauldron --env {env} stop mcp`), then run `{cmd}`; it sets the \
                 damaged file aside under a dated name once no ripple run of this cauldron is live, and says what it did. Then start \
                 the `mcp` service again.",
                env = c.env,
                cmd = recover_command(c)
            ),
        )
    })
}

/// The ripples journal of the cauldron's environment.
fn ripples_journal(c: &env::Cauldron) -> std::path::PathBuf {
    c.data_root.join("ripples").join("runs.jsonl")
}

/// Is process `pid` running? `true` when it cannot be told (a run that may
/// be live is treated as live).
fn pid_running(pid: u64) -> bool {
    if pid == 0 {
        return true;
    }
    match std::process::Command::new("/bin/ps").args(["-o", "stat=", "-p", &pid.to_string()]).stderr(std::process::Stdio::null()).output() {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            let stat = text.trim();
            if stat.is_empty() {
                // `ps -p` exits non-zero and prints nothing for no such pid.
                return out.status.success();
            }
            !stat.starts_with('Z')
        }
        Err(_) => true,
    }
}

/// A ripple run no longer than this since its last journal line can still
/// be live: no model call outlasts 900 s, and a run journals a line before
/// and after each call.
const RUN_QUIET_MAX_MS: i64 = 3_600_000;

/// **The ripple runs of cauldron `c` that may be live**, per the ripples
/// journal: each run whose latest attempt has no `done` line, whose process
/// is running (or cannot be told), and whose last line is less than an hour
/// old. `[{run, dump, pid, last_event, last_at}]`.
pub fn live_ripple_runs(c: &env::Cauldron) -> Vec<Value> {
    let text = std::fs::read_to_string(ripples_journal(c)).unwrap_or_default();
    let mut last: BTreeMap<String, Value> = BTreeMap::new();
    for l in text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        if l["cauldron"].as_str() != Some(c.name.as_str()) || l["eval"] == json!(true) {
            continue;
        }
        if let Some(run) = l["run"].as_str() {
            last.insert(run.to_string(), l);
        }
    }
    let now = env::region_locks::now_ms();
    last.into_iter()
        .filter(|(_, l)| l["event"] != "done")
        .filter(|(_, l)| {
            let at = chrono::DateTime::parse_from_rfc3339(l["at"].as_str().unwrap_or("")).map(|t| t.timestamp_millis()).unwrap_or(now);
            now - at < RUN_QUIET_MAX_MS
        })
        .filter(|(_, l)| pid_running(l["pid"].as_u64().unwrap_or(0)))
        .map(|(run, l)| json!({"run": run, "dump": l["dump"], "pid": l["pid"], "last_event": l["event"], "last_at": l["at"]}))
        .collect()
}

/// **Set a damaged region-lock table aside**, for `cauldron soup recover`.
/// `_writer` is the soup's writer lock, held by the caller (the proof that
/// no other process can write the soup or grant a lock meanwhile). A table
/// that reads is left alone (`None`). One that cannot be read (not JSON,
/// truncated, empty, or of another format) is renamed to
/// `region-locks.json.damaged-<UTC time>` beside it, so the soup then holds
/// no lock, ONLY when no ripple run of the cauldron is live
/// ([`live_ripple_runs`]); otherwise it is refused, naming the runs and
/// what to do, and nothing is changed. Answers what it did.
pub fn set_aside_damaged(c: &env::Cauldron, _writer: &std::fs::File) -> Result<Option<Value>> {
    let why = match env::region_locks::load(c) {
        Ok(_) => return Ok(None),
        Err(e) => format!("{e:#}"),
    };
    let live = live_ripple_runs(c);
    if !live.is_empty() {
        let runs: Vec<String> = live.iter().map(|r| format!("run {} (dump {}, pid {}, last step `{}` at {})", short(r["run"].as_str().unwrap_or("")), r["dump"].as_str().unwrap_or("?"), r["pid"], r["last_event"].as_str().unwrap_or("?"), r["last_at"].as_str().unwrap_or("?"))).collect();
        return Err(Error::new(
            Code::Locked,
            format!(
                "REFUSED: cauldron `{}`'s region-lock table cannot be read ({why}), and a ripple run of it may be live: {}. A live run may \
                 hold regions the file no longer shows, so it was not set aside and nothing was changed. Let the run end, or stop the \
                 ripples service (`cauldron --env {} stop ripples`), then run `{}` again.",
                c.name,
                runs.join("; "),
                c.env,
                recover_command(c)
            ),
        )
        .with_detail(json!({"live_runs": live})));
    }
    let from = env::region_locks::path(c);
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let mut to = from.with_file_name(format!("{}.damaged-{stamp}", env::region_locks::FILE));
    let mut n = 1;
    while to.exists() {
        n += 1;
        to = from.with_file_name(format!("{}.damaged-{stamp}-{n}", env::region_locks::FILE));
    }
    std::fs::rename(&from, &to).map_err(|e| Error::caused(Code::Store, format!("setting {} aside as {}", from.display(), to.display()), &e))?;
    if let Ok(d) = std::fs::File::open(c.soup()) {
        let _ = d.sync_all();
    }
    Ok(Some(json!({
        "outcome": "set_aside",
        "file": from.display().to_string(),
        "moved_to": to.display().to_string(),
        "why": why,
        "now": "no region is held; a ripple run started later takes its locks afresh",
    })))
}

fn short(run: &str) -> &str {
    &run[..run.len().min(12)]
}

impl RegionGuard for Guard {
    fn check(&self, initiator: &Initiator, rows: &[RegionRow]) -> std::result::Result<(), (String, Map<String, Value>)> {
        let fail = |e: Error| (e.message.clone(), Map::from_iter([("why".to_string(), json!("table"))]));
        let table = load(&self.cauldron).map_err(fail)?;
        // Now, after the read: a row cut back on reading is never more than
        // the maximum ahead of it.
        let now = env::region_locks::now_ms();
        // A row read as cut back (too far ahead) is written back cut: this
        // guard runs only in the holder of the soup's writer lock.
        if !table.clamped.is_empty() {
            let _ = env::region_locks::save(&self.cauldron, &table);
        }
        if table.rows.is_empty() {
            return Ok(());
        }
        let refused = conflicts(&self.cauldron, &table, now, run_of(initiator), &wants(rows)).map_err(fail)?;
        if refused.is_empty() {
            return Ok(());
        }
        let detail = env::region_locks::refusal_detail(&refused, now);
        let mut m = Map::new();
        m.insert("why".into(), json!("region_locked"));
        if let Value::Object(d) = detail {
            m.extend(d);
        }
        Err((env::region_locks::refusal_text(&refused, now), m))
    }
}

/// A spine failure as the door's error: a region refusal keeps its code
/// and its facts; anything else is `store`.
pub fn spine_error(e: spine::error::SpineError) -> Error {
    match e {
        spine::error::SpineError::RegionLocked { message, fields } => {
            let code = if fields.get("why") == Some(&json!("table")) { Code::Store } else { Code::RegionLocked };
            let mut detail = Value::Object(fields);
            if let Some(o) = detail.as_object_mut() {
                o.remove("why");
            }
            Error::new(code, message).with_detail(detail)
        }
        other => Error::new(Code::Store, other.to_string()),
    }
}

/// **The revision of every region of the default pool**, and of
/// `~unplaced`: `{region id: rev}` (`spine::regions::region_rev`).
pub fn revisions(c: &env::Cauldron) -> Result<BTreeMap<String, String>> {
    let pool = PoolName::new(DEFAULT_POOL);
    let handle = Pool::open_read_only(&Pool::path_in(&c.soup(), &pool)).map_err(|e| Error::new(Code::Store, e.to_string()))?;
    let snap = handle.snapshot().map_err(|e| Error::new(Code::Store, e.to_string()))?;
    let grammar = handle.grammar().clone();
    let mut out = BTreeMap::new();
    let mut ids: Vec<NodeId> = snap.nodes_of(spine::regions::REGION_TYPE, None).map_err(|e| Error::new(Code::Store, e.to_string()))?.into_iter().map(|n| n.id).collect();
    ids.push(spine::regions::unplaced());
    for id in ids {
        if let Some(rev) = spine::regions::region_rev(&snap, &pool, &grammar, &id).map_err(|e| Error::new(Code::Store, e.to_string()))? {
            out.insert(id.0, rev);
        }
    }
    Ok(out)
}

/// **The head revision of every region of the default pool**: `{region id:
/// head rev}` (`spine::regions::region_head_rev`): each region's boundary
/// and identity, no member. What a ripple's plan depends on, since its
/// prompt shows each region's handle and boundary and never a member; and
/// cheap, as it reads no member.
pub fn head_revisions(c: &env::Cauldron) -> Result<BTreeMap<String, String>> {
    let pool = PoolName::new(DEFAULT_POOL);
    let handle = Pool::open_read_only(&Pool::path_in(&c.soup(), &pool)).map_err(|e| Error::new(Code::Store, e.to_string()))?;
    let snap = handle.snapshot().map_err(|e| Error::new(Code::Store, e.to_string()))?;
    let mut out = BTreeMap::new();
    for n in snap.nodes_of(spine::regions::REGION_TYPE, None).map_err(|e| Error::new(Code::Store, e.to_string()))? {
        if let Some(rev) = spine::regions::region_head_rev(&snap, &pool, &n.id).map_err(|e| Error::new(Code::Store, e.to_string()))? {
            out.insert(n.id.0, rev);
        }
    }
    Ok(out)
}

/// The ancestors of each of `regions` in the default pool (not the regions
/// themselves), each once.
pub fn ancestors(c: &env::Cauldron, regions: &[String]) -> Result<Vec<String>> {
    let pool = PoolName::new(DEFAULT_POOL);
    let handle = Pool::open_read_only(&Pool::path_in(&c.soup(), &pool)).map_err(|e| Error::new(Code::Store, e.to_string()))?;
    let snap = handle.snapshot().map_err(|e| Error::new(Code::Store, e.to_string()))?;
    let mut out: Vec<String> = Vec::new();
    for r in regions {
        for a in spine::regions::ancestors_of(&snap, &pool, &NodeId(r.clone())).map_err(|e| Error::new(Code::Store, e.to_string()))? {
            if !regions.contains(&a.0) && !out.contains(&a.0) {
                out.push(a.0);
            }
        }
    }
    Ok(out)
}

/// **A whole-store act refused while a region is held**: `rename`,
/// `remove`. Called with the store's writer locks already taken, so no new
/// lock can be granted meanwhile. Rows of runs that landed, and expired
/// rows, do not count.
pub fn refuse_held(c: &env::Cauldron, verb: &str) -> Result<()> {
    let t = load(c)?;
    let now = env::region_locks::now_ms();
    let mut held: Vec<env::region_locks::Row> = Vec::new();
    for r in t.live(now) {
        if crate::ripple::run_landed(c, &r.holder.run)?.is_empty() {
            held.push(r.clone());
        }
    }
    if held.is_empty() {
        return Ok(());
    }
    held.retain(|r| r.mode == env::region_locks::WRITE);
    Err(Error::new(
        Code::RegionLocked,
        format!(
            "REFUSED: `{verb}` acts on the whole of cauldron `{}`, and a write is in progress in part of it: {}",
            c.name,
            env::region_locks::refusal_text(&held, now)
        ),
    )
    .with_detail(env::region_locks::refusal_detail(&held, now)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::{import, ImportOptions, ImportOutcome};
    use std::path::PathBuf;

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn target(tag: &str) -> (Tmp, crate::Target) {
        let dir = std::env::temp_dir().join(format!(
            "soup-door-region-lock-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let soup = dir.join("cauldrons").join("kb").join("soup");
        std::fs::create_dir_all(&soup).unwrap();
        std::fs::write(dir.join("cauldrons").join("kb").join("cauldron.toml"), "name = \"kb\"\ncreated_at = \"2026-01-01T00:00:00Z\"\nenabled = true\n").unwrap();
        drop(spine::Cauldron::create(&soup, &[spine::grammar_v6::workspace_v6("ws")], spine::config::SpineConfig::default()).unwrap());
        let e = env::Environment {
            root: dir.clone(),
            root_source: env::paths::Source::CauldronHome,
            name: "dev".into(),
            name_source: env::paths::Source::CauldronEnv,
            data_root: dir.clone(),
        };
        let c = e.find_cauldron("kb").unwrap().expect("found");
        let t = crate::Target { env: e, cauldron: c, resolved_from: None };
        let regions = json!([
            {"id": "R-1", "type": "region", "handle": "Publishing the schema", "boundary": "Inside: anything about the published, machine-readable schema."},
            {"id": "R-2", "type": "region", "handle": "Importing records", "boundary": "Inside: anything about landing externally written records."}
        ]);
        land(&t, regions.as_array().unwrap()).unwrap();
        (Tmp(dir), t)
    }

    fn land(t: &crate::Target, records: &[Value]) -> Result<Value> {
        let o = ImportOptions { remint: true, agent: "test".into(), dispatch: "test".into(), ..Default::default() };
        match import(t, records, &o)? {
            ImportOutcome::Judged { doc, ok: true } => Ok(doc),
            other => panic!("{other:?}"),
        }
    }

    fn idea(handle: &str, region: &str) -> Vec<Value> {
        vec![json!({"id": "x", "type": "idea", "state": "idea", "handle": handle, "edges": [{"kind": "in_region", "to": region}]})]
    }

    fn hold(t: &crate::Target, run: &str, region: &str, ttl_ms: i64) {
        let mut table = env::region_locks::load(&t.cauldron).unwrap();
        let h = env::region_locks::Holder { kind: "ripple".into(), run: run.into(), dump: Some("DM-1".into()) };
        table.acquire(&h, "writing the records it draws from a dump", "ws", &[region.to_string()], &[], env::region_locks::now_ms(), ttl_ms).unwrap();
        env::region_locks::save(&t.cauldron, &table).unwrap();
    }

    /// The row held by `run` past its `until` now, as the clock passing it
    /// would leave it (no sleep: the test does not race the clock).
    fn expire(t: &crate::Target, run: &str) {
        let mut table = env::region_locks::load(&t.cauldron).unwrap();
        let past = env::region_locks::now_ms() - 1;
        for r in table.rows.iter_mut().filter(|r| r.holder.run == run) {
            r.until_ms = past;
            r.until = env::region_locks::stamp(past);
        }
        env::region_locks::save(&t.cauldron, &table).unwrap();
    }

    /// The import the CLI makes on its own is held by the same rows as the
    /// door: into the held region refused `region_locked`, naming the holder
    /// and until when, with nothing written; into the other region it lands;
    /// once the row expires, the held region takes it too. The row is held
    /// for ten minutes and then expired by moving its `until` into the past,
    /// so no step depends on how long the others take (it held for 1.5 s and
    /// slept 1.6 s, and failed whenever the machine was loaded).
    #[test]
    fn a_direct_import_into_a_held_region_is_refused_and_one_elsewhere_lands() {
        let (_tmp, t) = target("import");
        let before = crate::reads::search(&t, "fresh idea", &Default::default()).unwrap()["results"].as_array().unwrap().len();
        hold(&t, "run-held", "R-1", 600_000);
        let err = land(&t, &idea("a fresh idea for the schema", "R-1")).unwrap_err();
        assert_eq!(err.code, Code::RegionLocked, "{err:?}");
        assert!(err.message.contains("region `R-1` is locked by a ripple (run run-held, drawing records from dump DM-1)"), "{}", err.message);
        assert!(err.message.contains("Nothing was written"), "{}", err.message);
        let detail = err.detail.clone().unwrap();
        assert_eq!(detail["locks"][0]["region"], json!("R-1"));
        assert!(detail["retry_after_secs"].as_i64().unwrap() >= 1);
        assert_eq!(crate::reads::search(&t, "fresh idea", &Default::default()).unwrap()["results"].as_array().unwrap().len(), before, "nothing landed");
        land(&t, &idea("a fresh idea for importing", "R-2")).expect("the other region takes it");
        // The listing marks the held region only.
        let regions = crate::reads::regions(&t, &Default::default()).unwrap();
        let by: BTreeMap<String, Value> = regions["regions"].as_array().unwrap().iter().map(|r| (r["id"].as_str().unwrap().to_string(), r.clone())).collect();
        assert_eq!(by["R-1"]["locked"]["holder"]["run"], json!("run-held"));
        assert!(by["R-2"].get("locked").is_none());
        // A whole-store act is refused while it is held.
        let e = refuse_held(&t.cauldron, "rename").unwrap_err();
        assert_eq!(e.code, Code::RegionLocked);
        assert!(e.message.contains("`rename` acts on the whole of cauldron `kb`"), "{}", e.message);
        // Expired: nothing holds it.
        expire(&t, "run-held");
        land(&t, &idea("a fresh idea after the lock", "R-1")).expect("an expired row holds nothing");
        refuse_held(&t.cauldron, "rename").expect("nothing held");
        assert!(crate::reads::regions(&t, &Default::default()).unwrap()["regions"][0].get("locked").is_none());
    }

    /// A table that cannot be read refuses every write (never read as
    /// empty), and the listing says why.
    #[test]
    fn an_unreadable_table_refuses_writes_and_never_reads_as_empty() {
        let (_tmp, t) = target("unreadable");
        std::fs::write(env::region_locks::path(&t.cauldron), "{not json").unwrap();
        let err = land(&t, &idea("refused", "R-2")).unwrap_err();
        assert_eq!(err.code, Code::Store);
        assert!(err.message.contains("cannot be read, so nothing was written"), "{}", err.message);
        assert!(crate::reads::regions(&t, &Default::default()).unwrap()["region_locks_error"].is_string());
    }

    /// **A damaged table, every way**: truncated, garbage, of the wrong
    /// shape, of another format, zero bytes. Each refuses a write naming
    /// `cauldron soup recover`; set aside while a ripple run of the
    /// cauldron is live in the ripples journal, it is refused and left as it
    /// was; with none live, it is moved to a dated name, the soup holds no
    /// lock, and the write lands.
    #[test]
    fn a_damaged_table_is_set_aside_only_when_no_ripple_run_is_live() {
        let damaged: [(&str, &[u8]); 5] = [
            ("truncated", br#"{"format": 1, "rows": [{"pool": "ws", "region": "R-1", "mo"#),
            ("garbage", b"\x00\x13 not a table at all \xff"),
            ("wrong shape", br#"{"format": 1, "rows": [{"pool": 7}]}"#),
            ("other format", br#"{"format": 9, "rows": []}"#),
            ("zero bytes", b""),
        ];
        for (n, (what, bytes)) in damaged.iter().enumerate() {
            let (_tmp, t) = target(&format!("damaged{n}"));
            let file = env::region_locks::path(&t.cauldron);
            std::fs::write(&file, bytes).unwrap();
            let err = land(&t, &idea("refused while damaged", "R-2")).unwrap_err();
            assert_eq!(err.code, Code::Store, "{what}: {err:?}");
            assert!(err.message.contains("cauldron soup recover --env dev --cauldron kb"), "{what}: {}", err.message);
            assert!(refuse_held(&t.cauldron, "rename").is_err(), "{what}: rename refused too");
            // A live run in the ripples journal: refused, nothing changed.
            let journal = t.cauldron.data_root.join("ripples").join("runs.jsonl");
            std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
            let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            let line = |event: &str| json!({"at": at, "run": "run-live", "cauldron": "kb", "dump": "DM-1", "pid": std::process::id(), "event": event}).to_string();
            std::fs::write(&journal, format!("{}\n{}\n", line("started"), line("call"))).unwrap();
            let lock = crate::lock::take_writer_lock(&t.cauldron, "recover").unwrap();
            let e = set_aside_damaged(&t.cauldron, &lock).unwrap_err();
            assert_eq!(e.code, Code::Locked, "{what}: {e:?}");
            assert!(e.message.contains("a ripple run of it may be live: run run-live") && e.message.contains("stop ripples"), "{what}: {}", e.message);
            assert_eq!(std::fs::read(&file).unwrap(), bytes.to_vec(), "{what}: left as it was");
            // The run ended: set aside under a dated name.
            std::fs::write(&journal, format!("{}\n{}\n{}\n", line("started"), line("call"), line("done"))).unwrap();
            let v = set_aside_damaged(&t.cauldron, &lock).unwrap().expect("set aside");
            assert_eq!(v["outcome"], "set_aside", "{what}");
            let moved = std::path::PathBuf::from(v["moved_to"].as_str().unwrap());
            assert!(moved.file_name().unwrap().to_str().unwrap().starts_with("region-locks.json.damaged-"), "{what}: {v}");
            assert_eq!(std::fs::read(&moved).unwrap(), bytes.to_vec(), "{what}: kept whole");
            assert!(!file.exists());
            drop(lock);
            land(&t, &idea("lands once set aside", "R-2")).expect("no lock held");
            refuse_held(&t.cauldron, "rename").expect("nothing held");
            // A readable table is left alone.
            let lock = crate::lock::take_writer_lock(&t.cauldron, "recover").unwrap();
            assert!(set_aside_damaged(&t.cauldron, &lock).unwrap().is_none());
        }
    }

    /// **A far-future `until`** (a hand edit: the year 2058) holds at most
    /// 900 s: a write into the region is refused with a retry-after of at
    /// most 900 s, the writer writes the row back cut, and a table with such
    /// a row cannot be written.
    #[test]
    fn a_far_future_lock_holds_at_most_the_maximum_and_is_written_back_cut() {
        let (_tmp, t) = target("far");
        let now = env::region_locks::now_ms();
        let row = env::region_locks::Row {
            pool: "ws".into(),
            region: "R-2".into(),
            mode: env::region_locks::WRITE.into(),
            holder: env::region_locks::Holder { kind: "ripple".into(), run: "hand-edited".into(), dump: Some("DM-1".into()) },
            purpose: "placing the records drawn from dump DM-1".into(),
            since: env::region_locks::stamp(now),
            until: "2058-01-01T00:00:00Z".into(),
            since_ms: now,
            until_ms: 2_145_916_800_000,
        };
        let table = env::region_locks::Table { rows: vec![row.clone()], ..Default::default() };
        let e = env::region_locks::save(&t.cauldron, &table).unwrap_err();
        assert!(format!("{e:#}").contains("more than 900 s ahead of now"), "{e:#}");
        // Written by hand, as a hand edit would.
        std::fs::write(env::region_locks::path(&t.cauldron), serde_json::to_string(&table).unwrap()).unwrap();
        let err = land(&t, &idea("into a region held far ahead", "R-2")).unwrap_err();
        assert_eq!(err.code, Code::RegionLocked, "{err:?}");
        let retry = err.detail.clone().unwrap()["retry_after_secs"].as_i64().unwrap();
        assert!((1..=900).contains(&retry), "{retry}");
        let on_disk: env::region_locks::Table = serde_json::from_str(&std::fs::read_to_string(env::region_locks::path(&t.cauldron)).unwrap()).unwrap();
        assert!(on_disk.rows[0].until_ms - env::region_locks::now_ms() <= env::region_locks::MAX_AHEAD_MS, "written back cut: {on_disk:?}");
        land(&t, &idea("into the other region", "R-1")).expect("the other region takes it");
    }

    /// The listing marks a region held `write` `locked`, and an ancestor of
    /// one (held `link`) `locked_below`.
    #[test]
    fn the_listing_marks_a_held_region_and_the_one_above_it() {
        let (_tmp, t) = target("listing");
        let mut table = env::region_locks::Table::default();
        let h = env::region_locks::Holder { kind: "ripple".into(), run: "run-held".into(), dump: Some("DM-1".into()) };
        table.acquire(&h, "placing the records drawn from dump DM-1", "ws", &["R-2".to_string()], &["R-1".to_string()], env::region_locks::now_ms(), 60_000).unwrap();
        env::region_locks::save(&t.cauldron, &table).unwrap();
        let regions = crate::reads::regions(&t, &Default::default()).unwrap();
        let by: BTreeMap<String, Value> = regions["regions"].as_array().unwrap().iter().map(|r| (r["id"].as_str().unwrap().to_string(), r.clone())).collect();
        assert_eq!(by["R-2"]["locked"]["holder"]["run"], json!("run-held"));
        assert!(by["R-2"].get("locked_below").is_none());
        assert_eq!(by["R-1"]["locked_below"]["holder"]["run"], json!("run-held"), "{}", by["R-1"]);
        assert!(by["R-1"].get("locked").is_none());
    }

    /// A run whose process is gone, or whose last line is over an hour old,
    /// is not live.
    #[test]
    fn a_dead_or_quiet_run_is_not_live() {
        let (_tmp, t) = target("liveness");
        let journal = t.cauldron.data_root.join("ripples").join("runs.jsonl");
        std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
        let now = chrono::Utc::now();
        let old = (now - chrono::Duration::hours(2)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let fresh = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let lines = [
            json!({"at": old, "run": "quiet", "cauldron": "kb", "dump": "DM-1", "pid": std::process::id(), "event": "call"}),
            json!({"at": fresh, "run": "elsewhere", "cauldron": "other", "dump": "DM-1", "pid": std::process::id(), "event": "call"}),
            json!({"at": fresh, "run": "mine", "cauldron": "kb", "dump": "DM-2", "pid": std::process::id(), "event": "call"}),
        ];
        std::fs::write(&journal, lines.iter().map(|l| l.to_string() + "\n").collect::<String>()).unwrap();
        let live = live_ripple_runs(&t.cauldron);
        assert_eq!(live.iter().map(|r| r["run"].as_str().unwrap()).collect::<Vec<_>>(), vec!["mine"]);
    }

    /// The head revisions: every region (no `~unplaced`); a member written
    /// into a region moves none; a child region narrowing one moves that
    /// one's.
    #[test]
    fn head_revisions_never_move_with_a_member() {
        let (_tmp, t) = target("heads");
        let a = head_revisions(&t.cauldron).unwrap();
        assert_eq!(a.keys().cloned().collect::<Vec<_>>(), vec!["R-1".to_string(), "R-2".to_string()]);
        land(&t, &idea("into the second region", "R-2")).unwrap();
        assert_eq!(head_revisions(&t.cauldron).unwrap(), a, "a member added moves no head");
        land(&t, &[json!({"id": "c", "type": "region", "handle": "Importing in bulk", "boundary": "Inside: anything about importing many records at once, in bulk.", "edges": [{"kind": "narrows", "to": "R-2"}]})]).unwrap();
        let b = head_revisions(&t.cauldron).unwrap();
        assert_eq!(a["R-1"], b["R-1"]);
        assert_ne!(a["R-2"], b["R-2"], "a child region moves its parent's head");
    }

    /// The revisions: every region and `~unplaced`; a write into one region
    /// moves that one only.
    #[test]
    fn revisions_move_for_the_region_written_into_only() {
        let (_tmp, t) = target("revs");
        let a = revisions(&t.cauldron).unwrap();
        assert_eq!(a.keys().cloned().collect::<Vec<_>>(), vec!["R-1".to_string(), "R-2".to_string(), "~unplaced".to_string()]);
        land(&t, &idea("into the second region", "R-2")).unwrap();
        let b = revisions(&t.cauldron).unwrap();
        assert_eq!(a["R-1"], b["R-1"]);
        assert_ne!(a["R-2"], b["R-2"]);
        assert_eq!(a["~unplaced"], b["~unplaced"]);
    }
}
