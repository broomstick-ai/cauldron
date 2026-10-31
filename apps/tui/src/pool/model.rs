//! The pool board's held state and its projection into cards.
//!
//! [`Fold`] holds what the daemon's shapes said, frame by frame — the same
//! fold the Python board's `shapes.py::ShapeMirror` performs: `op: read`
//! replaces one key's row (a keyless `read` with `epoch.state: dead` is the
//! death notice), `op: live` applies a `row` / `gone` / `transition` /
//! `shared` event. Nothing here touches a file or a socket.
//!
//! [`project`] turns the two folds the board subscribes to (`accounts` and
//! `threads/non-idle`) into [`Board`], the only thing the view draws.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde_json::Value;
use shapewire::subtree::{is_active, Rollup};

/// The shapes this board reads. Nothing else is subscribed and no read
/// request is ever sent (board-rebuild.md §1: "no new ways to request data").
pub const ACCOUNTS: &str = "accounts";
pub const NON_IDLE: &str = "threads/non-idle";
/// The one key of `threads/non-idle`.
pub const FLEET: &str = "fleet";
/// **KEYED, FOR EVERY ROSTER THREAD AND EVERY PARENT ONE NAMES** (TUI-P1d).
/// Two cells are read off it: `rolled`, the daemon's status rolled up over
/// the thread's whole subtree (R9.TS1) — the ONE source of a card's status, the
/// same cell the thread view reads — and `status.detail`, the daemon's reason
/// a parked thread is parked (`threads/non-idle` dropped `detail` on purpose,
/// a paragraph on every export).
pub const THREADS: &str = "threads";
/// **KEYED, FOR NAMES** — every roster thread and every parent a roster
/// thread names. A hash id is never drawn (DIR-16); a name not yet read is
/// `a thread`.
pub const SUMMARY: &str = "threads/summary";

/// One shape's held state.
#[derive(Debug, Clone, Default)]
pub struct Fold {
    pub name: String,
    pub rows: BTreeMap<String, Value>,
    pub shared: Value,
    /// True once any `op: read` for a key arrived.
    pub exported: bool,
    /// `Some(reason)` once the producer said the shape task died. The rows are
    /// KEPT (a board that empties itself on a dead fold looks like a pool whose
    /// accounts all went away); the view marks them last-known.
    pub dead: Option<String>,
    pub last_frame_at: Option<DateTime<Utc>>,
}

impl Fold {
    pub fn new(name: &str) -> Self {
        Self { name: name.to_string(), shared: Value::Null, ..Default::default() }
    }

    /// Apply one wire frame. Returns true if anything a reader can see moved.
    /// A frame for another shape is ignored, not an error.
    pub fn on_frame(&mut self, f: &Value) -> bool {
        if f["type"] != "shape" || f["shape"].as_str() != Some(self.name.as_str()) {
            return false;
        }
        if f["epoch"]["state"] == "dead" {
            self.dead = Some(
                f["epoch"]["dead_reason"].as_str().unwrap_or("the shape task died").to_string(),
            );
        } else if f["epoch"]["state"] == "known" {
            self.dead = None;
        }
        match f["op"].as_str() {
            Some("read") => {
                self.stamp(&f["as_of"]);
                if let Some(s) = f.get("shared").filter(|s| s.is_object()) {
                    self.shared = s.clone();
                }
                if let Some(k) = f["key"].as_str() {
                    let row = if f["row"].is_object() { f["row"].clone() } else { Value::Null };
                    self.rows.insert(k.to_string(), row);
                    self.exported = true;
                }
                // A `rows` map (detail=rows exports) carries several keys at once.
                if let Some(rows) = f["rows"].as_object() {
                    for (k, r) in rows {
                        self.rows.insert(k.clone(), r.clone());
                    }
                    self.exported = true;
                }
                true
            }
            Some("live") => {
                self.stamp(&f["at"]);
                let ev = &f["event"];
                let key = f["key"].as_str();
                match (ev["kind"].as_str(), key) {
                    (Some("shared"), _) => {
                        if ev["shared"].is_object() {
                            self.shared = ev["shared"].clone();
                        }
                        true
                    }
                    (Some("row"), Some(k)) if ev["row"].is_object() => {
                        self.rows.insert(k.to_string(), ev["row"].clone());
                        true
                    }
                    (Some("gone"), Some(k)) => self.rows.remove(k).is_some(),
                    (Some("transition"), Some(k)) => {
                        let (Some(cell), true) = (ev["cell"].as_str(), ev["item"].is_object())
                        else {
                            return false;
                        };
                        let Some(row) = self.rows.get_mut(k).and_then(|r| r.as_object_mut())
                        else {
                            return false;
                        };
                        let mut item = ev["item"].clone();
                        if item.get("at").is_none() {
                            item["at"] = f["at"].clone();
                        }
                        let list = row.entry(cell.to_string()).or_insert(Value::Array(vec![]));
                        if !list.is_array() {
                            *list = Value::Array(vec![]);
                        }
                        list.as_array_mut().unwrap().push(item);
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn stamp(&mut self, v: &Value) {
        if let Some(t) = v.as_str().and_then(parse_ts) {
            self.last_frame_at = Some(t);
        }
    }
}

pub fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|t| t.with_timezone(&Utc))
}

/// A timestamp field that may be RFC 3339 text or epoch milliseconds.
fn ts(v: &Value) -> Option<DateTime<Utc>> {
    match v {
        Value::String(s) => parse_ts(s),
        Value::Number(n) => n.as_i64().and_then(DateTime::from_timestamp_millis),
        _ => None,
    }
}

/// The login slot's state, `credential.slot_state` on the `accounts` shape
/// (`SlotHealth`, threads `accounts.rs`). Typed so that the view cannot draw a
/// wiped slot with the live slot's words: there is one match, in
/// [`Slot::badge`], and no string passes through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slot {
    Live,
    NoLogin,
    Wiped,
    Stranded,
    Expired,
    CloneOf(String),
    /// The credential cell is not known yet, or carries a value this face
    /// does not recognise. Drawn as `?`, never as live.
    Unknown,
}

impl Slot {
    pub fn from_credential(c: &Value) -> Self {
        match c["slot_state"].as_str() {
            Some("live") => Slot::Live,
            Some("no-login") => Slot::NoLogin,
            Some("wiped") => Slot::Wiped,
            Some("stranded") => Slot::Stranded,
            Some("expired") => Slot::Expired,
            Some("clone-of") => Slot::CloneOf(
                c["clone_of"].as_str().or(c["of"].as_str()).unwrap_or("?").to_string(),
            ),
            _ => Slot::Unknown,
        }
    }

    /// The badge word and whether it is an alarm (red/amber) or fine.
    pub fn badge(&self) -> (String, Tone) {
        match self {
            Slot::Live => ("login ok".into(), Tone::Good),
            Slot::NoLogin => ("NO LOGIN".into(), Tone::Warn),
            Slot::Wiped => ("WIPED".into(), Tone::Bad),
            Slot::Stranded => ("STRANDED".into(), Tone::Bad),
            Slot::Expired => ("EXPIRED".into(), Tone::Bad),
            Slot::CloneOf(of) => (format!("CLONE of {of}"), Tone::Bad),
            Slot::Unknown => ("login ?".into(), Tone::Muted),
        }
    }

    /// The board-warning note for an ENABLED account in a bad login state.
    pub fn warning(&self) -> Option<(&'static str, Tone)> {
        match self {
            Slot::Wiped => Some(("login was WIPED — sign it in again", Tone::Bad)),
            Slot::Stranded => Some(("login is STRANDED — its slot lost the credential", Tone::Bad)),
            Slot::Expired => Some(("login has EXPIRED — sign it in again", Tone::Bad)),
            Slot::NoLogin => Some(("has no login yet", Tone::Warn)),
            Slot::CloneOf(_) => Some(("login is a clone of another slot", Tone::Bad)),
            Slot::Live | Slot::Unknown => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Good,
    Warn,
    Bad,
    Muted,
    Plain,
}

/// One quota axis reading from `row.gauge.<axis>`.
#[derive(Debug, Clone, PartialEq)]
pub struct Gauge {
    pub axis: String,
    /// 0–100, as the daemon publishes it. `None` = no reading on this axis.
    pub pct: Option<f64>,
    pub resets_at: Option<DateTime<Utc>>,
    pub confidence: Option<f64>,
}

/// The axes drawn, in order, and the label each is drawn with.
pub const AXES: &[(&str, &str)] =
    &[("session", "session"), ("weekly-all", "weekly"), ("weekly-fable", "fable")];

/// Disabled is independent of everything else on the card (board-rebuild.md
/// §1): a disabled account is OUT of the pool and its card collapses to one
/// line. The variant carries only what that line draws, so a disabled card
/// has no gauges to rank by construction.
#[derive(Debug, Clone, PartialEq)]
pub enum Standing {
    Enabled,
    Disabled { at: Option<DateTime<Utc>>, note: Option<String> },
}

/// A thread homed on an account — the `threads/non-idle` roster entry whose
/// `account` is this card.
#[derive(Debug, Clone, PartialEq)]
pub struct Homed {
    pub id: String,
    pub state: String,
    pub executing: bool,
    pub queued: u64,
    /// **THE THREAD THAT SPAWNED THIS ONE**, off the roster row's `parent_id`.
    /// `None` is a root — or a daemon too old to say, which reads the same.
    pub parent_id: Option<String>,
    /// When it entered this state (`since`).
    pub since: Option<DateTime<Utc>>,
    /// Its name, off `threads/summary`; `None` until that row is read.
    pub name: Option<String>,
    /// The parent's name, when it has a parent and the name has been read.
    pub parent_name: Option<String>,
    /// `status.detail` off the keyed `threads` row — WHY it is parked. Read
    /// only for parked entries.
    pub detail: Option<String>,
    /// Every ancestor, nearest first, off the roster row (R9.UI1).
    pub ancestors: Vec<String>,
    /// **THE STATUS THIS THREAD SHOWS** — read, never computed (TUI-P1d,
    /// TC-12): its own when it is active itself, else `active` when the
    /// daemon's `rolled.status` says so, else its own word. Exactly the rule
    /// the thread view's `Store::subtree_rollup` reads after TUI-P1c, so the
    /// board and the thread view cannot disagree. No `rolled` cell held yet is
    /// its own word. There is no parked rollup: an error does not roll up
    /// (R9.TS1).
    pub rollup: Rollup,
    /// The same reading for its PARENT, which is usually not on the roster (a
    /// parent waiting on children is idle) — what the parent line draws.
    pub parent_rollup: Option<Rollup>,
}

impl Homed {
    /// The name to draw: the summary's title, else `a thread`. Never the id.
    pub fn label(&self) -> String {
        self.name.clone().unwrap_or_else(|| "a thread".to_string())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub label: String,
    pub standing: Standing,
    pub slot: Slot,
    /// `credential.is_live_login`: this account IS the machine's login.
    pub is_live_login: bool,
    pub registered: Option<bool>,
    pub gauges: Vec<Gauge>,
    pub limit_until: Option<DateTime<Utc>>,
    pub limit_prose: Option<String>,
    pub not_answering: Option<String>,
    pub share: Option<f64>,
    pub pressure: Option<f64>,
    /// **∞ PRESSURE** (R9.PR1): the daemon's `pressure` blob says
    /// `infinite: true` with `pressure: null` — the final 60 s before the
    /// weekly reset with room left under the cap. Placement ranks it FIRST, so
    /// the card says `∞`, never nothing and never unranked.
    pub infinite: bool,
    pub unranked_reason: Option<String>,
    pub login_expires: Option<DateTime<Utc>>,
    pub homed: Vec<Homed>,
}

impl Card {
    pub fn disabled(&self) -> bool {
        matches!(self.standing, Standing::Disabled { .. })
    }
    pub fn running(&self) -> usize {
        self.homed.iter().filter(|h| h.executing).count()
    }
    pub fn retrying(&self) -> usize {
        self.homed.iter().filter(|h| h.state == "retrying").count()
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Totals {
    pub running: usize,
    /// Of `running`, how many are managed subagents (have a parent).
    pub running_sub: usize,
    /// Of `parked`, how many are subagents, and how many have no parent.
    pub parked_sub: usize,
    pub parked_root: usize,
    pub queued_msgs: u64,
    /// Threads parked in the POOL (parked threads belong to the pool, not to
    /// an account; `account` on them is only where they last sat).
    pub parked: usize,
    pub roster_known: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Board {
    pub cards: Vec<Card>,
    pub totals: Totals,
    pub exported: bool,
    pub dead: Option<String>,
    /// The `shared.machine_login` sentence, when there is a machine login.
    pub machine_login: Option<String>,
    pub warn_below_days: f64,
}

/// The title `threads/summary` holds for `id`, if read and non-empty.
pub fn title_of(summary: &Fold, id: &str) -> Option<String> {
    summary
        .rows
        .get(id)
        .and_then(|r| r["title"]["title"].as_str())
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|t| !t.is_empty())
}

/// **THE KEYS THIS BOARD WANTS HELD**, given the roster: a name and the
/// `threads` row (its `rolled` cell, and a parked one's reason) for every
/// roster thread and every parent one names. The app subscribes what it has
/// not asked for yet.
pub fn wanted(non_idle: &Fold) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for (_, h) in roster(non_idle).unwrap_or_default() {
        out.push((SUMMARY, h.id.clone()));
        out.push((THREADS, h.id.clone()));
        if let Some(p) = &h.parent_id {
            out.push((SUMMARY, p.clone()));
            out.push((THREADS, p.clone()));
        }
    }
    out
}

/// **THE DAEMON'S `rolled.status` FOR `id`**, off the keyed `threads` fold —
/// `active`, `idle` or `error` (R9.TS1). `None` until that row is read.
pub fn rolled_status<'a>(threads: &'a Fold, id: &str) -> Option<&'a str> {
    threads.rows.get(id).and_then(|r| r["rolled"]["status"].as_str())
}

/// **THE STATUS A THREAD SHOWS, READ OFF THE DAEMON** (TUI-P1d): the same three
/// arms as the thread view's `Store::subtree_rollup` — itself active, else the
/// daemon's `rolled` says active, else its own word. Nothing here walks the
/// roster.
pub fn shown(state: &str, executing: bool, rolled: Option<&str>) -> Rollup {
    if is_active(state, executing) {
        Rollup::SelfActive
    } else if rolled == Some(shapewire::rolled::ACTIVE) {
        Rollup::SubtreeActive
    } else {
        Rollup::Own
    }
}

/// The roster entries from the `threads/non-idle` fold.
pub fn roster(non_idle: &Fold) -> Option<Vec<(String, Homed)>> {
    let row = non_idle.rows.get(FLEET)?;
    let list = row["threads"].as_array()?;
    Some(
        list.iter()
            .map(|t| {
                (
                    t["account"].as_str().unwrap_or("").to_string(),
                    Homed {
                        id: t["id"].as_str().unwrap_or("?").to_string(),
                        state: t["state"].as_str().unwrap_or("?").to_string(),
                        executing: t["executing"].as_bool().unwrap_or(false),
                        queued: t["queued"].as_u64().unwrap_or(0),
                        parent_id: t["parent_id"].as_str().filter(|p| !p.is_empty()).map(str::to_string),
                        since: ts(&t["since"]),
                        name: None,
                        parent_name: None,
                        detail: None,
                        ancestors: t["ancestors"]
                            .as_array()
                            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                            .unwrap_or_default(),
                        rollup: Rollup::Own,
                        parent_rollup: None,
                    },
                )
            })
            .collect(),
    )
}

fn str_of(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

/// Fold → cards. Pure; `now` is only used to drop a limit that has passed.
pub fn project(
    accounts: &Fold,
    non_idle: &Fold,
    threads: &Fold,
    summary: &Fold,
    now: DateTime<Utc>,
) -> Board {
    // The roster, each entry decorated with what the keyed folds hold — its
    // status and its parent's READ off the daemon's `rolled` cell (TUI-P1d),
    // never rolled up here over the roster.
    let raw = roster(non_idle);
    let roll = |id: &str, state: &str, executing: bool| -> Rollup {
        shown(state, executing, rolled_status(threads, id))
    };
    let own = |id: &str| -> (String, bool) {
        raw.as_deref()
            .unwrap_or_default()
            .iter()
            .find(|(_, x)| x.id == id)
            .map(|(_, x)| (x.state.clone(), x.executing))
            // Not on the roster is idle: the roster's membership predicate.
            .unwrap_or_else(|| ("idle".to_string(), false))
    };
    let roster = raw.clone().map(|r| {
        r.into_iter()
            .map(|(a, mut h)| {
                h.rollup = roll(&h.id, &h.state, h.executing);
                h.parent_rollup = h.parent_id.as_deref().map(|p| {
                    let (s, e) = own(p);
                    roll(p, &s, e)
                });
                h.name = title_of(summary, &h.id);
                h.parent_name = h.parent_id.as_deref().and_then(|p| title_of(summary, p));
                h.detail = threads
                    .rows
                    .get(&h.id)
                    .and_then(|r| r["status"]["detail"].as_str())
                    .map(str::to_string);
                (a, h)
            })
            .collect::<Vec<_>>()
    });
    let folds_into = &accounts.shared["folds_into"];
    let home_of = |acct: &str| -> String {
        folds_into[acct].as_str().unwrap_or(acct).to_string()
    };

    let mut totals = Totals { roster_known: roster.is_some(), ..Default::default() };
    if let Some(r) = &roster {
        for (_, h) in r {
            let sub = h.parent_id.is_some();
            if h.executing {
                totals.running += 1;
                totals.running_sub += sub as usize;
            }
            if h.state == "parked" {
                totals.parked += 1;
                if sub {
                    totals.parked_sub += 1;
                } else {
                    totals.parked_root += 1;
                }
            }
            totals.queued_msgs += h.queued;
        }
    }

    let cards = accounts
        .rows
        .iter()
        .filter(|(k, _)| k.as_str() != "default")
        .map(|(label, row)| {
            let disabled = row["disabled"]["disabled"].as_bool().unwrap_or(false)
                || row["share"]["disabled"].as_bool().unwrap_or(false)
                || row["pressure"]["disabled"].as_bool().unwrap_or(false);
            let standing = if disabled {
                Standing::Disabled {
                    at: ts(&row["disabled"]["disabled_at"]).or(ts(&row["share"]["disabled_at"])),
                    note: str_of(&row["disabled"]["note"]).or(str_of(&row["share"]["disabled_note"])),
                }
            } else {
                Standing::Enabled
            };
            let gauges = AXES
                .iter()
                .map(|(axis, _)| {
                    let g = &row["gauge"][*axis];
                    Gauge {
                        axis: axis.to_string(),
                        pct: g["observed_pct"].as_f64(),
                        resets_at: ts(&g["resets_at"]),
                        confidence: g["confidence"].as_f64(),
                    }
                })
                .collect();
            let limit_until = ts(&row["limit"]["until"]).filter(|u| *u > now);
            let homed = roster
                .as_ref()
                .map(|r| {
                    r.iter()
                        .filter(|(a, _)| home_of(a) == *label)
                        .map(|(_, h)| h.clone())
                        .collect()
                })
                .unwrap_or_default();
            Card {
                label: label.clone(),
                standing,
                slot: Slot::from_credential(&row["credential"]),
                is_live_login: row["credential"]["is_live_login"].as_bool().unwrap_or(false),
                registered: row["registered"].as_bool(),
                gauges,
                limit_until,
                limit_prose: if limit_until.is_some() { str_of(&row["limit"]["prose"]) } else { None },
                not_answering: if row["health"]["dead_since"].is_null() {
                    None
                } else {
                    Some(str_of(&row["health"]["dead_reason"]).unwrap_or_else(|| "no reason given".into()))
                },
                share: row["share"]["share"].as_f64().or(row["pressure"]["share"].as_f64()),
                pressure: row["pressure"]["pressure"].as_f64(),
                infinite: row["pressure"]["infinite"].as_bool().unwrap_or(false),
                unranked_reason: str_of(&row["pressure"]["unranked_reason"]),
                login_expires: ts(&row["credential"]["refresh_expires_at_ms"]),
                homed,
            }
        })
        .collect();

    Board {
        cards,
        totals,
        exported: accounts.exported,
        dead: accounts.dead.clone(),
        machine_login: {
            let ml = &accounts.shared["machine_login"];
            match (str_of(&ml["hosted_by"]), ml["unhosted"].as_bool()) {
                (Some(acct), _) => Some(format!("machine login is {acct}")),
                (None, Some(true)) => Some("machine login is signed in as an account NOT in the pool".into()),
                _ => None,
            }
        },
        warn_below_days: accounts.shared["warn_below_days"].as_f64().unwrap_or(7.0),
    }
}
