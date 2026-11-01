//! The Mesh's SERVICE CONTROLS (8e4fca10 msg 403, Q5: "yes I like that.
//! service level") and the SUPERVISION LOG they write.
//!
//! Every service on Status › Mesh — the threads daemon among them — carries
//! `restart`, `stop` and `rollback`, each asked first in the confirm modal.
//! Confirmed, the act is RECORDED here: who asked, what, on which service,
//! and why — always `requested`, the word the supervisor's own records use
//! for an act a person asked for. The record is what acts: the face's mesh
//! is the seed's FAKE readings with every record replayed over them in
//! order, so a restart shows the service running, up since the record's time, a stop shows it
//! stopped, and a rollback shows it on the newest version proven good before
//! the one it ran. NOTHING here touches a real process: this face has no
//! supervisor to ask (the backend spec's `services/{svc}/restart|stop|
//! rollback`, not built), and it would never act on one from a demo build.
//!
//! ```text
//! <root>/data/<env>/tui/soup/supervisions.jsonl   one record a line, append-only
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::cauldron::dash::{Node, ThreadsBoard};

/// The threads daemon's name as a service: its band is the daemon's own
/// board, not a seeded service row.
pub const DAEMON: &str = "threads daemon";

/// What was asked of a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Restart,
    Stop,
    Rollback,
}

impl Op {
    pub const ALL: [Op; 3] = [Op::Restart, Op::Stop, Op::Rollback];
    pub fn word(self) -> &'static str {
        match self {
            Op::Restart => "restart",
            Op::Stop => "stop",
            Op::Rollback => "rollback",
        }
    }
}

/// One line of the supervision log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    /// When it was asked: an RFC 3339 instant in the local offset in force
    /// then (`2026-11-01T01:42:53-07:00`, see [`stamp`]), so it names ONE
    /// instant even in a repeated hour. A log written before UPTIME-DST holds
    /// `YYYY-MM-DD HH:MM:SS` local, which [`instant`] still reads.
    pub at: String,
    /// Who asked: `operator` from this face.
    pub who: String,
    /// The device the service runs on.
    pub device: String,
    pub service: String,
    pub op: Op,
    /// Why it was done: `requested` for every act asked from this face.
    pub why: String,
    /// The version it ran before, and the one it runs after (a rollback's
    /// target; the same for a restart or a stop).
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: String,
}

pub fn path(dir: &Path) -> PathBuf {
    dir.join("supervisions.jsonl")
}

/// Every record in the log, oldest first; none when there is no log.
pub fn load(dir: &Path) -> Result<Vec<Record>> {
    let p = path(dir);
    match std::fs::read_to_string(&p) {
        Ok(t) => t.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).with_context(|| format!("parse a line of {}", p.display()))).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("read {}", p.display())),
    }
}

/// Append `r` to the log under its lock.
pub fn append(dir: &Path, r: &Record) -> Result<()> {
    use std::io::Write;
    let p = path(dir);
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    env::lock::with_lock(&p, || {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&p).with_context(|| format!("open {}", p.display()))?;
        writeln!(f, "{}", serde_json::to_string(r)?)?;
        f.sync_all()?;
        Ok(())
    })
}

/// What `op` would do to service `service` on `device` as the mesh stands:
/// `(from, to)` versions, or why it cannot be done.
pub fn plan(mesh: &[Node], threads: &ThreadsBoard, device: &str, service: &str, op: Op) -> Result<(String, String), String> {
    if service == DAEMON {
        let up = threads.link == "live";
        return match op {
            Op::Stop if !up => Err("the threads daemon is already stopped".into()),
            Op::Rollback => Err("no version of the threads daemon is recorded as proven good".into()),
            _ => Ok((String::new(), String::new())),
        };
    }
    let s = mesh.iter().filter(|n| n.node == device).flat_map(|n| n.services.iter()).find(|s| s.name == service).ok_or_else(|| format!("{service} is not on {device}"))?;
    match op {
        Op::Stop if s.status == "stopped" => Err(format!("{service} is already stopped")),
        Op::Rollback => match before(&s.proven, &s.version) {
            Some(p) => Ok((s.version.clone(), p.version.clone())),
            None => Err(format!("no version of {service} before {} is recorded as proven good", s.version)),
        },
        _ => Ok((s.version.clone(), s.version.clone())),
    }
}

/// The newest version proven good that is OLDER than `current` (0258 J1:
/// "goes back" never goes forward). `proven` is newest first; when `current`
/// is in it, what is after it, and older by date; when it is not (a build
/// not proven yet), the newest proven one.
fn before<'a>(proven: &'a [crate::cauldron::dash::Proven], current: &str) -> Option<&'a crate::cauldron::dash::Proven> {
    match proven.iter().position(|p| p.version == current) {
        Some(i) => {
            let at = &proven[i].at;
            proven[i + 1..].iter().filter(|p| p.version != current && p.at <= *at).rev().max_by(|a, b| a.at.cmp(&b.at))
        }
        None => proven.iter().rev().max_by(|a, b| a.at.cmp(&b.at)),
    }
}

/// Whether `service` on `device` is stopped as the mesh stands: a restart of
/// it only starts it (0258 J9).
pub fn stopped(mesh: &[Node], threads: &ThreadsBoard, device: &str, service: &str) -> bool {
    if service == DAEMON {
        return threads.link != "live";
    }
    mesh.iter().filter(|n| n.node == device).flat_map(|n| n.services.iter()).any(|s| s.name == service && s.status == "stopped")
}

/// Replay `records`, oldest first, over the seed's mesh and daemon board.
/// A record whose service is gone is skipped: the log stays as it was.
pub fn apply(mesh: &mut [Node], threads: &mut ThreadsBoard, records: &[Record]) {
    for r in records {
        if r.service == DAEMON {
            match r.op {
                Op::Restart => {
                    threads.link = "live".into();
                    threads.started = Some(r.at.clone());
                    threads.warnings = 0;
                }
                Op::Stop => threads.link = "offline".into(),
                Op::Rollback => {}
            }
            continue;
        }
        let Some(s) = mesh.iter_mut().filter(|n| n.node == r.device).flat_map(|n| n.services.iter_mut()).find(|s| s.name == r.service) else { continue };
        match r.op {
            Op::Restart | Op::Rollback => {
                s.status = "running".into();
                // Up since the record (0258 J3): its uptime is read off
                // `at` whenever it is drawn, never a frozen `0s`.
                s.started = Some(r.at.clone());
                s.pid = Some(fake_pid(r));
                s.errors = 0;
                s.warnings = 0;
                if r.op == Op::Rollback && !r.to.is_empty() {
                    s.version = r.to.clone();
                }
            }
            Op::Stop => {
                s.status = "stopped".into();
                s.started = None;
                s.uptime = "—".into();
                s.pid = None;
            }
        }
    }
}

/// How long ago the daemon last answered, as drawn: counted from the record
/// that restarted it when one did, else the seed's reading.
pub fn answered(t: &ThreadsBoard) -> Option<String> {
    match &t.started {
        Some(at) => Some(since(at)),
        None => t.age.clone(),
    }
}

/// **UPTIME-DST: A DURATION IS A DIFFERENCE OF INSTANTS.** How long ago `at`
/// was, as drawn (`3h 5m`); `—` when it cannot be read. Counted from the UTC
/// instant `at` names to [`now`], never by subtracting two local wall-clock
/// readings: across a DST change those are an hour off the elapsed time (a
/// 3h 5m uptime read `2h 5m` across the 2026-11-01 fall-back).
fn since(at: &str) -> String {
    since_at(at, now())
}

/// [`since`] at an explicit `now`.
pub fn since_at(at: &str, now: chrono::DateTime<chrono::Utc>) -> String {
    instant(at).map_or_else(|| "—".into(), |t| span((now - t).num_seconds().max(0)))
}

/// The instant a record's `at` names. RFC 3339 is exact. A legacy local
/// `YYYY-MM-DD HH:MM:SS` (written before UPTIME-DST) is resolved in this
/// machine's zone: a time in a repeated hour reads as its FIRST occurrence
/// (the JOBS-DST policy), and one in a skipped hour, which no clock ever
/// wrote, is unreadable.
pub fn instant(at: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::TimeZone;
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(at) {
        return Some(t.with_timezone(&chrono::Utc));
    }
    let naive = chrono::NaiveDateTime::parse_from_str(at, "%Y-%m-%d %H:%M:%S").ok()?;
    // The earliest INSTANT, chosen here: chrono's `earliest()` on `Local`
    // returned the PST reading of a fall-back 01:xx, which is the later one.
    match chrono::Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(t) => Some(t.with_timezone(&chrono::Utc)),
        chrono::LocalResult::Ambiguous(a, b) => Some(a.with_timezone(&chrono::Utc).min(b.with_timezone(&chrono::Utc))),
        chrono::LocalResult::None => None,
    }
}

/// A record's `at` for DISPLAY: its instant as this machine's local
/// `YYYY-MM-DD HH:MM:SS` (the column the log has always shown); the text
/// itself when it cannot be read.
pub fn shown_at(at: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(at) {
        Ok(t) => t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string(),
        Err(_) => at.to_string(),
    }
}

/// The `at` a record made at `t` carries: RFC 3339, seconds, in the local
/// offset in force at `t`.
pub fn stamp(t: chrono::DateTime<chrono::Utc>) -> String {
    t.with_timezone(&chrono::Local).to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

/// **THE FACE'S "NOW" FOR SUPERVISION** (UPTIME-DST, on JOBS-DST's pattern):
/// the real UTC clock, except that an ISOLATED process (the test-only
/// marker, or a cargo test harness — [`env::egress::process_isolation`])
/// reads it from `CAULDRON_TEST_NOW` (unix seconds) when that is set, so a
/// test can pin a moment across a DST change. His own face is never
/// isolated, so it never reads the variable. The zone is injected the
/// ordinary way: `TZ`, which `chrono::Local` honours.
pub fn now() -> chrono::DateTime<chrono::Utc> {
    static PINNED: std::sync::OnceLock<Option<i64>> = std::sync::OnceLock::new();
    let pinned = *PINNED.get_or_init(|| {
        env::egress::process_isolation()?;
        std::env::var("CAULDRON_TEST_NOW").ok()?.trim().parse().ok()
    });
    pinned.and_then(|s| chrono::DateTime::from_timestamp(s, 0)).unwrap_or_else(chrono::Utc::now)
}

/// How long `s` has been up, as drawn: from the record that started it when
/// one did (`—` when its time cannot be read), else the seed's reading.
pub fn uptime(s: &crate::cauldron::dash::Service) -> String {
    s.started.as_deref().map_or_else(|| s.uptime.clone(), since)
}

/// `secs` the way the seed writes an uptime: `12s`, `4m`, `3h 5m`, `6d 2h`.
pub fn span(secs: i64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3_600, secs % 3_600 / 60);
    match () {
        _ if d > 0 => format!("{d}d {h}h"),
        _ if h > 0 => format!("{h}h {m}m"),
        _ if m > 0 => format!("{m}m"),
        _ => format!("{secs}s"),
    }
}

/// A new process id for a (fake) start, the same every time the log is read.
fn fake_pid(r: &Record) -> u32 {
    let h = r.at.bytes().chain(r.service.bytes()).fold(17u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    10_000 + h % 80_000
}

/// The record of `op` on `service`, asked from this face now.
pub fn record(device: &str, service: &str, op: Op, (from, to): (String, String)) -> Record {
    Record { at: stamp(now()), who: "operator".into(), device: device.into(), service: service.into(), op, why: "requested".into(), from, to }
}
