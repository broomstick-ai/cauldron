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

/// **THE ONE MASKER** (V1-CLEAN1 F1, V1-CLEAN2 B1). An account key as a face
/// may draw it: a key that is not an address is drawn as it is; an address
/// (anything holding `@`) is MASKED to the first character of each side,
/// `p•••@e•••` — never the address, never its local part, never its domain.
///
/// Every path by which an account key reaches the screen, a dump or a log
/// goes through here: the pool board and Status › Tokens (`Card::title`),
/// Config › Accounts and its modal titles (`App::account_name`), an
/// account's id (`menu::account_id`, the slug of this), and notes quoting a
/// key ([`masked_in`]). Nothing else masks an account.
pub fn shown_name(key: &str) -> String {
    match key.split_once('@') {
        None => key.to_string(),
        Some((local, domain)) => {
            let first = |s: &str| s.chars().next().map_or(String::new(), |c| c.to_string());
            format!("{}•••@{}•••", first(local), first(domain))
        }
    }
}

/// `text` with every occurrence of account key `key` drawn as
/// [`shown_name`] — for prose the face did not write (a daemon's refusal, a
/// tool's stderr) that may quote the key — and then every OTHER address in
/// it masked too ([`masked_text`]).
pub fn masked_in(text: &str, key: &str) -> String {
    if key.is_empty() {
        return masked_text(text);
    }
    masked_text(&text.replace(key, &shown_name(key)))
}

/// `text` with every ADDRESS in it drawn through [`shown_name`] — for prose
/// that may quote an address the face does not know the key of (a daemon's
/// refusal, a producer's pending mark). An address is a run of the
/// characters an address is written with (letters, digits, `._%+-`) holding
/// one `@` with something on both sides; whatever surrounds it — a method
/// path's `/`, brackets, a sentence's full stop — is kept, and a name the
/// masker already drew (`a•••@e•••`) is left as it is. V1-ACCT: this replaced
/// V1.6's second masker (`accounts::mask_emails`, `a…@e….com`); the format
/// is [`shown_name`]'s alone.
pub fn masked_text(text: &str) -> String {
    let addr = |c: char| c.is_alphanumeric() || "._%+-@".contains(c);
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        // A trailing `.` or `-` ends a sentence, not an address.
        let core_len = run.trim_end_matches(['.', '-']).len();
        let (core, tail) = run.split_at(core_len);
        match core.split_once('@') {
            Some((l, d)) if !l.is_empty() && !d.is_empty() => {
                out.push_str(&shown_name(core));
                out.push_str(tail);
            }
            _ => out.push_str(run),
        }
        run.clear();
    };
    for c in text.chars() {
        if addr(c) {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// **THE PRE-SALT DIGEST** (V1-CLEAN2 to V1-ACCT): FNV-1a, 64-bit, over the
/// key alone. It is kept ONLY so that settings written under the ids it made
/// are found and moved (`menu::legacy_account_ids`). Nothing is drawn or
/// stored from it any more: anyone could compute it from a guessed address
/// and confirm the guess against a tag on screen or an id on disk
/// (V1-ACCT-I N3).
pub fn unsalted_digest(key: &str) -> u64 {
    key.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// The six hex digits of [`unsalted_digest`] an account id carried before
/// V1-ACCT2: read-migration only.
pub fn unsalted_digest_hex(key: &str) -> String {
    format!("{:06x}", unsalted_digest(key) & 0xff_ffff)
}

/// **A KEY'S DIGEST, KEYED BY THIS INSTALL'S SECRET** (V1-ACCT2, V1-ACCT-I
/// N3): SipHash-2-4 under the 128-bit key of [`install_key`]. Stable across
/// runs on one install (the key is stored beside the settings it keys), and
/// meaningless without the key, so a tag on screen or an id in
/// `settings.toml` can no longer be checked against a guessed address. Its
/// low 24 bits, as six hex digits, are the tail of an address-keyed
/// account's id (`cauldron::menu::account_id`) and the source of
/// [`shown_among`]'s disambiguator — so both say the same thing.
pub fn key_digest(key: &str) -> u64 {
    use std::hash::Hasher;
    let (k0, k1) = install_key::get();
    #[allow(deprecated)] // std's SipHasher is SipHash-2-4 with a caller's key: exactly what is wanted here.
    let mut h = std::hash::SipHasher::new_with_keys(k0, k1);
    h.write(key.as_bytes());
    h.finish()
}

/// The six hex digits of [`key_digest`] an account id carries.
pub fn digest_hex(key: &str) -> String {
    format!("{:06x}", key_digest(key) & 0xff_ffff)
}

/// **THIS INSTALL'S DIGEST KEY** (V1-ACCT2): 128 random bits in
/// `<data>/shared/tui/install.key` — beside the global `settings.toml` and
/// `accounts.json` whose ids it keys — written (0600, temp then rename) the
/// first time a face that may write opens its settings there, and read on
/// every open after.
///
/// One key per PROCESS: the first install a process binds is the one every
/// digest in it uses, and an install directory with no key yet that the
/// same process opens later is given that key (so the ids this process
/// writes there are ids that directory's key makes). Before any bind (the
/// standalone pool board with no environment, a `--dump`, a unit test) the
/// key is a random one held in memory only. A later bind that finds a key
/// file replaces it (nothing persisted was keyed by it); one that creates the
/// file writes THAT key, so a digest drawn before the bind still holds.
pub mod install_key {
    use std::path::Path;
    use std::sync::RwLock;

    /// The file's name, in the global settings directory.
    pub const FILE: &str = "install.key";

    /// `(key, bound)`: `bound` once it came from (or was written to) a file.
    static KEY: RwLock<Option<((u64, u64), bool)>> = RwLock::new(None);

    fn random() -> (u64, u64) {
        let b = uuid::Uuid::new_v4().into_bytes();
        let c = uuid::Uuid::new_v4().into_bytes();
        // A v4 uuid fixes 6 of its 128 bits: two of them, xor-folded, leave
        // every bit of the key random.
        let lo = u64::from_le_bytes(b[..8].try_into().expect("8")) ^ u64::from_le_bytes(c[8..].try_into().expect("8")).rotate_left(7);
        let hi = u64::from_le_bytes(b[8..].try_into().expect("8")) ^ u64::from_le_bytes(c[..8].try_into().expect("8")).rotate_left(13);
        (lo, hi)
    }

    fn parse(text: &str) -> Option<(u64, u64)> {
        let t = text.trim();
        if t.len() != 32 || !t.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Some((u64::from_str_radix(&t[..16], 16).ok()?, u64::from_str_radix(&t[16..], 16).ok()?))
    }

    fn write(path: &Path, k: (u64, u64)) -> std::io::Result<()> {
        use std::io::Write;
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension(format!("key.tmp-{}", std::process::id()));
        {
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create(true).truncate(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
            let mut f = o.open(&tmp)?;
            f.write_all(format!("{:016x}{:016x}\n", k.0, k.1).as_bytes())?;
            f.sync_all()?;
        }
        // Never over a key another face wrote first: the first one stays.
        match std::fs::hard_link(&tmp, path) {
            Ok(()) => {
                let _ = std::fs::remove_file(&tmp);
                Ok(())
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(e)
            }
        }
    }

    /// The key every digest in this process uses now.
    pub fn get() -> (u64, u64) {
        if let Some((k, _)) = *KEY.read().unwrap_or_else(|e| e.into_inner()) {
            return k;
        }
        let mut g = KEY.write().unwrap_or_else(|e| e.into_inner());
        g.get_or_insert_with(|| (random(), false)).0
    }

    /// **BIND THE INSTALL IN `dir`** (the global settings directory): read
    /// its key, or — when `create` and it has none — write one (this
    /// process's, if it already bound one). `Err` only when a key file is
    /// there and unreadable or when writing one failed; the process then
    /// keeps the key it had.
    pub fn bind(dir: &Path, create: bool) -> std::io::Result<()> {
        let path = dir.join(FILE);
        let mut g = KEY.write().unwrap_or_else(|e| e.into_inner());
        let on_disk = match std::fs::read_to_string(&path) {
            Ok(t) => Some(parse(&t).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{} is not a key", path.display())))?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        match (*g, on_disk) {
            // One key per process: the first bound install's.
            (Some((_, true)), Some(_)) => Ok(()),
            (Some((k, true)), None) => {
                if create {
                    write(&path, k).or_else(|e| if path.exists() { Ok(()) } else { Err(e) })?;
                }
                Ok(())
            }
            (_, Some(k)) => {
                *g = Some((k, true));
                Ok(())
            }
            (_, None) if create => {
                // The key this process has used so far, if any: ids it drew
                // before this bind stay the ids it writes.
                let k = g.map_or_else(random, |(k, _)| k);
                match write(&path, k) {
                    Ok(()) => {
                        *g = Some((k, true));
                        Ok(())
                    }
                    // Another face wrote one between the read and the link.
                    Err(_) if path.exists() => {
                        let k = std::fs::read_to_string(&path).ok().and_then(|t| parse(&t)).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "install key"))?;
                        *g = Some((k, true));
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            }
            (_, None) => Ok(()),
        }
    }
}

/// **[`shown_name`], TOLD APART FROM ITS TWINS** (V1-ACCT, V1-CLEAN2-I Q1).
/// Two addresses with the same first letters on both sides mask the same
/// (`c•••@b•••`); when another of `keys` shows the same, the name carries the
/// shortest prefix (at least three digits) of its id's digest that no twin
/// shares — `c•••@b••• ·d64` — and nothing more of the address. A name with
/// no twin is [`shown_name`] exactly.
pub fn shown_among<'a>(key: &str, keys: impl IntoIterator<Item = &'a str>) -> String {
    let shown = shown_name(key);
    if shown == key {
        return shown;
    }
    let twins: Vec<String> = keys.into_iter().filter(|k| *k != key && shown_name(k) == shown).map(digest_hex).collect();
    if twins.is_empty() {
        return shown;
    }
    let mine = digest_hex(key);
    let n = (3..=6).find(|&n| twins.iter().all(|t| t[..n] != mine[..n])).unwrap_or(6);
    format!("{shown} ·{}", &mine[..n])
}

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
                // Daemon prose: masked before it is held (V1-ACCT-I N5).
                masked_text(f["epoch"]["dead_reason"].as_str().unwrap_or("the shape task died")),
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
    /// What the card is drawn under: [`shown_among`] its board's other keys
    /// (V1-ACCT) — set by [`project`], never the raw key.
    pub shown: String,
}

impl Card {
    /// **THE NAME A CARD IS DRAWN UNDER — NEVER A RAW EMAIL** (V1-CLEAN1,
    /// V1.5-I F1). `label` is the `accounts` ROW KEY, and the producer's
    /// account id IS its email (`services/threads/src/accounts.rs`, "R12.1 —
    /// AN ACCOUNT IS ITS EMAIL"); once R12.2 rekeys the rows, the key is an
    /// address. `label` stays the key every action is sent with; what is
    /// DRAWN goes through [`shown_name`].
    pub fn title(&self) -> String {
        self.shown.clone()
    }
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
                // Daemon prose that may quote an account key: masked here, once,
                // for every surface that draws a card (V1-ACCT-I N5).
                limit_prose: if limit_until.is_some() { str_of(&row["limit"]["prose"]).map(|p| masked_text(&p)) } else { None },
                not_answering: if row["health"]["dead_since"].is_null() {
                    None
                } else {
                    Some(masked_text(&str_of(&row["health"]["dead_reason"]).unwrap_or_else(|| "no reason given".into())))
                },
                share: row["share"]["share"].as_f64().or(row["pressure"]["share"].as_f64()),
                pressure: row["pressure"]["pressure"].as_f64(),
                infinite: row["pressure"]["infinite"].as_bool().unwrap_or(false),
                unranked_reason: str_of(&row["pressure"]["unranked_reason"]).map(|w| masked_text(&w)),
                login_expires: ts(&row["credential"]["refresh_expires_at_ms"]),
                homed,
                shown: String::new(),
            }
        })
        .collect::<Vec<Card>>();
    // Drawn names last, when every key is known: twins are told apart.
    let keys: Vec<String> = cards.iter().map(|c| c.label.clone()).collect();
    let mut cards = cards;
    for c in &mut cards {
        c.shown = shown_among(&c.label, keys.iter().map(String::as_str));
    }

    Board {
        cards,
        totals,
        exported: accounts.exported,
        // Whoever filled the fold (its own `on_frame`, or the seam's mirror
        // through `live::pool_of`), the daemon's reason is masked (N5).
        dead: accounts.dead.as_deref().map(masked_text),
        machine_login: {
            let ml = &accounts.shared["machine_login"];
            match (str_of(&ml["hosted_by"]), ml["unhosted"].as_bool()) {
                (Some(acct), _) => Some(format!("machine login is {}", shown_among(&acct, keys.iter().map(String::as_str)))),
                (None, Some(true)) => Some("machine login is signed in as an account NOT in the pool".into()),
                _ => None,
            }
        },
        warn_below_days: accounts.shared["warn_below_days"].as_f64().unwrap_or(7.0),
    }
}
