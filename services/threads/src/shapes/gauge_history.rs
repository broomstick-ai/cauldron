//! # `accounts/gauge-history` — an account's gauge readings over the last week
//!
//! **THE SERIES `accounts.gauge` CANNOT HOLD.** `accounts.gauge` is a
//! `map<opaque>` of CURRENT readings, one per axis, `retained_items: 0`; the
//! board's 7-day sparkline is a SERIES, and the only thing that answered it was
//! the `gauge/history` request (`server.rs`). The operator's ruling — *"We
//! should not have new paths to request data… We should remove the ability to
//! request data"* — leaves one honest home: a shape whose cell IS the series.
//! R2.S3d, item B.
//!
//! ## The name, and why it is not `gauge/history`
//!
//! The brief asked for a shape called `gauge/history`. The chassis refuses that
//! spelling at `Hub::register` (`shapewire/src/hub.rs`, *"shape `…` has a
//! path-shaped name but declares no PARENT"*): a `/` in a shape name IS a
//! nesting edge, and `gauge` is not a shape. The edge this series really has is
//! to `accounts` — it is keyed by account, its subject is the account, and a
//! consumer reaches it from the account it is drawing — so it is NESTED under
//! `accounts`, exactly as `threads/log` is nested under `threads`, and the name
//! follows from the edge: `accounts/gauge-history`. That also keeps it from
//! sharing a spelling with the `gauge/history` METHOD it replaces, which R2.S3c
//! retires.
//!
//! ## The nesting law
//!
//! A row carries ITS OWN series, not another shape's rows: one key per account,
//! one cell, `readings`, every item a reading OF that account. Nothing here is a
//! copy of an `accounts` cell — `accounts.gauge` holds the newest reading as a
//! level; this holds the window as a log, and the two answer different questions
//! (*"where is it now"* vs *"how did it get there"*).
//!
//! ## The window, and how the number was derived
//!
//! **7 days, at the busy `/usage` cadence, for the three percentage axes.** The
//! poller has two legs (`runtime.rs`'s probe loop): the idle whole-pool sweep at
//! `probe_interval_secs` (3,600 s) and the busy leg at `probe_interval_busy_secs`
//! (default [`crate::config::DEFAULT_PROBE_INTERVAL_BUSY_SECS`] = 120 s) on the
//! accounts that have work. The DENSER leg is the one that sizes a week:
//!
//! ```text
//! 7 d × 86,400 s/d ÷ 120 s  = 5,040 readings per axis   (PER_AXIS)
//! × 3 percentage axes       = 15,120 readings per key  (WINDOW = MAX_RETAINED_ITEMS)
//! ```
//!
//! The cell is DUAL-BOUNDED (`Sequence::new`): `WINDOW` items AND 7 days, so a
//! quiet account's week is its readings since a week ago and a busy one's is
//! capped at the count. `store.rs::observations_between` already measured the
//! same 5,040 against the request's default cap of 1,000; this window holds the
//! whole week, so the sparkline's *"truncated"* note is the exception it should
//! be. A daemon configured to poll FASTER than 120 s (the floor is 30 s) can
//! exceed the count before the clock; the hydrate then reads the window
//! `complete: false`, and `covers_since` says how far back it still reaches — rule 2.8's
//! declare-refuse-never-clamp, from the producing end.
//!
//! **Only the percentage axes** (`Axis::PERCENT_AXES`): the request this replaces
//! answered those three and no others, and `cash` is a dollar amount on another
//! scale that no sparkline draws.
//!
//! ## What it costs
//!
//! `Lazy` and `PerKey`: an unwatched daemon pays nothing, and a watched one pays
//! one bounded store read per account a consumer names. The live path is one
//! append per recorded reading. A row frame re-exports the window after each
//! append (the `threads/log` and `narration` precedent: the transition carries
//! the item, the debounced row carries the level); at 120 s per busy axis that
//! is a few frames a minute per watched account.
//!
//! ## Version
//!
//! [`GaugeHistory::VERSION`] is `1`: the shape is new.

use serde::Serialize;
use serde_json::{json, Value};

use crate::event::{Fact, Push};
use crate::gauge::Axis;
use crate::shape::{
    Handles, Hydrate, HydrateRequest, Hydration, Input, Map, Register, Root, Sequence, Shape,
    Stamped, Timestamp, Tracking, Tx,
};
use shapewire::cell::Cells;
use shapewire::declare_cells;
use shapewire::operation::{
    DType::{Enum, List, Scalar as Sc, Struct, Timestamp as Ts},
    Presence::{Optional, Required},
    Scalar::{Float, Int, Str},
    TimeEncoding::Rfc3339,
};

/// The window, in days.
pub const DAYS: i64 = 7;

/// **READINGS PER AXIS IN A WEEK AT THE BUSY CADENCE** — 604,800 s ÷ 120 s =
/// 5,040. Derived, not typed: see the module doc.
pub const PER_AXIS: usize =
    (DAYS as u64 * 86_400 / crate::config::DEFAULT_PROBE_INTERVAL_BUSY_SECS) as usize;

/// The axes this series carries — `Axis::PERCENT_AXES`, as words.
pub const AXES: &[&str] = &["session", "weekly_all", "weekly_fable"];

/// **THE CELL'S CAPACITY AND THE SHAPE'S `MAX_RETAINED_ITEMS`** — 15,120.
pub const WINDOW: usize = PER_AXIS * AXES.len();

/// One recorded gauge reading. `at` — when it was OBSERVED — is not a field: the
/// `Sequence` grafts the item's own stamp onto it on both the export and the
/// transition (`shapewire::fold::stamped_item`), so the two spellings of one
/// reading cannot differ.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Reading {
    pub axis: String,
    pub pct: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
    pub source: String,
}

/// One account's window.
#[derive(Debug)]
pub struct SeriesCells {
    pub readings: Sequence<Reading>,
    /// The newest OBSERVED instant held per axis — the append guard's identity.
    /// Not exported: it is the fold's bookkeeping, and every value in it is the
    /// stamp of an item the window already carries.
    newest: std::collections::BTreeMap<String, Timestamp>,
}

impl Default for SeriesCells {
    fn default() -> Self {
        Self {
            readings: Sequence::new(WINDOW, chrono::Duration::days(DAYS)),
            newest: Default::default(),
        }
    }
}

impl SeriesCells {
    fn to_json(&self, key: &str, retain: shapewire::wire::Retain) -> Value {
        json!({
            "account": key,
            "readings": self.readings.to_json_retained(retain),
        })
    }
}

pub struct GaugeHistoryRoot {
    pub rows: Map<String, SeriesCells>,
    pub window: Register<Value>,
    pub shape_version: Register<u32>,
    shared_dirty: bool,
}

impl Default for GaugeHistoryRoot {
    fn default() -> Self {
        Self {
            rows: Map::new(),
            window: Register::new(),
            shape_version: Register::new(),
            shared_dirty: false,
        }
    }
}

impl GaugeHistoryRoot {
    fn set_shared_constants(&mut self, at: Timestamp) {
        let window = json!({ "days": DAYS, "per_axis": PER_AXIS, "axes": AXES });
        if self.window.now() != Some(&window) {
            self.window.emit(at, window);
            self.shared_dirty = true;
        }
        if self.shape_version.now() != Some(&GaugeHistory::VERSION) {
            self.shape_version.emit(at, GaugeHistory::VERSION);
            self.shared_dirty = true;
        }
    }
}

impl Root for GaugeHistoryRoot {
    fn keys(&self) -> Vec<String> {
        self.rows.keys().cloned().collect()
    }

    fn row(&self, key: &str) -> Option<Value> {
        self.row_at(key, shapewire::wire::Retain::Max)
    }

    fn row_at(&self, key: &str, retain: shapewire::wire::Retain) -> Option<Value> {
        self.rows.get(&key.to_string()).map(|r| r.to_json(key, retain))
    }

    fn cells(&self, key: &str) -> Option<Value> {
        self.rows
            .get(&key.to_string())
            .map(|r| json!({ "readings": r.readings.state() }))
    }

    fn shared(&self) -> Value {
        json!({
            "window": self.window.now(),
            "shape_version": self.shape_version.now(),
        })
    }

    fn shared_cells(&self) -> Value {
        json!({
            "window": self.window.state(),
            "shape_version": self.shape_version.state(),
        })
    }

    fn drain(&mut self) -> crate::shape::Outbox {
        let dirty = self.rows.take_dirty_keys();
        let gone = self.rows.take_removed();
        let added = self.rows.take_added();
        let mut transitions = Vec::new();
        let mut rows = Vec::new();
        for k in dirty {
            let Some(r) = self.rows.get_mut(&k) else {
                continue;
            };
            for item in r.readings.take_pending() {
                transitions.push((k.clone(), "readings", item));
            }
            rows.push(k);
        }
        crate::shape::Outbox {
            rows,
            gone,
            added,
            transitions,
            shared: std::mem::take(&mut self.shared_dirty),
        }
    }

    fn mark_dead(&mut self, reason: &str) {
        self.window.mark_dead(reason);
        self.shape_version.mark_dead(reason);
        self.rows.mark_dead(reason);
        for (_, r) in self.rows.entries_mut() {
            r.readings.mark_dead(reason);
        }
    }
}

/// The hydrate read: one account's week, every percentage axis, oldest first.
#[derive(Default)]
pub struct AccountWeek {
    pub account: String,
}

/// What [`AccountWeek`] answers: the readings, each with its observed instant.
pub struct WeekRead {
    /// `false` when the account is not a pool member — no such key.
    pub member: bool,
    pub readings: Vec<(Timestamp, Reading)>,
    /// Set when an axis hit [`PER_AXIS`] — the week is longer than the window.
    pub clipped: bool,
}

impl HydrateRequest for AccountWeek {
    const NAME: &'static str = "account_week";
    type Response = WeekRead;
}

pub struct GaugeHistory;

impl GaugeHistory {
    pub const VERSION: u32 = 1;
}

declare_cells! {
    /// Per-key cells of `accounts/gauge-history` — one account, one series.
    pub GAUGE_HISTORY_CELLS = [
        ("readings", List(&Struct("Reading")), Required, [
            ("axis", Enum(AXES), Required),
            ("pct", Sc(Float), Required),
            // `None` when the poll could not parse a reset time.
            ("resets_at", Ts(Rfc3339), Optional),
            // `usage_poll` or a stream event's source — an open vocabulary
            // owned by `budgets.observe`'s callers, so a string.
            ("source", Sc(Str), Required),
            // The OBSERVED instant, grafted by the `Sequence`.
            ("at", Ts(Rfc3339), Required),
        ]),
    ]
}

declare_cells! {
    /// Shape-level cells of `accounts/gauge-history`.
    pub GAUGE_HISTORY_SHARED = [
        ("window", Struct("GaugeWindow"), Required, [
            ("days", Sc(Int), Required),
            ("per_axis", Sc(Int), Required),
            ("axes", List(&Enum(AXES)), Required),
        ]),
        ("shape_version", Sc(Int), Required, []),
    ]
}

fn stamp(s: &str) -> Option<Timestamp> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

/// The reading a `GaugeObserved` row carries, or `None` for a row with no
/// observation in it (`observed_pct: null` — the axis has never been read).
fn reading_of(axis: &str, row: &Value) -> Option<(Timestamp, Reading)> {
    let at = stamp(row.get("observed_at")?.as_str()?)?;
    Some((
        at,
        Reading {
            axis: axis.to_string(),
            pct: row.get("observed_pct")?.as_f64()?,
            resets_at: row.get("resets_at").and_then(Value::as_str).map(str::to_string),
            source: row.get("source")?.as_str()?.to_string(),
        },
    ))
}

impl Shape<Input> for GaugeHistory {
    const NAME: &'static str = "accounts/gauge-history";
    const CELLS: Cells = GAUGE_HISTORY_CELLS;
    const SHARED: Cells = GAUGE_HISTORY_SHARED;
    /// Nested under the pool it is a series OF — see the module doc's §The name.
    const PARENT: Option<&'static str> = Some("accounts");
    const TRACKING: Tracking = Tracking::Lazy;
    /// One account at a time: a keyless subscribe would be every account's week
    /// at once, and the board draws the cards it can see.
    const HYDRATE: Hydrate = Hydrate::PerKey;
    const VERSION: u32 = Self::VERSION;
    /// The window — see the module doc for the derivation.
    const MAX_RETAINED_ITEMS: usize = WINDOW;
    /// One `GaugeObserved` appends to one key.
    const BURST: usize = 2;

    type Producer = crate::runtime::ThreadManager;
    type Root = GaugeHistoryRoot;

    fn root(&self) -> GaugeHistoryRoot {
        GaugeHistoryRoot::default()
    }

    fn hydrates(
        &mut self,
        _p: &crate::runtime::ThreadManager,
        root: &mut GaugeHistoryRoot,
        as_of: Timestamp,
    ) {
        root.set_shared_constants(as_of);
        root.rows.mark_read(as_of, true);
        let _ = root.drain();
    }

    fn hydrates_key(
        &mut self,
        p: &crate::runtime::ThreadManager,
        root: &mut GaugeHistoryRoot,
        key: &str,
        as_of: Timestamp,
    ) -> bool {
        let got = p.hydrate(AccountWeek {
            account: key.to_string(),
        });
        let complete = got.is_complete();
        let reason = got.reason();
        let Some(week) = got.into_rows().into_iter().next() else {
            // **A FAILED READ IS NOT AN EMPTY WEEK.** The key is minted and its
            // window left at the `Sequence`'s default, `NotHydrated` — which a
            // consumer renders `?`, never as a week with no readings in it.
            // (`Sequence` has no reason-carrying setter; the reason is logged.)
            let why = reason.unwrap_or_else(|| "this account's week could not be read".into());
            tracing::debug!(account = %key, %why, "gauge history not hydrated");
            root.rows.key(key.to_string());
            let _ = root.drain();
            return true;
        };
        if !week.member {
            return false;
        }
        let row = root.rows.key(key.to_string());
        row.newest.clear();
        for (at, r) in &week.readings {
            let n = row.newest.entry(r.axis.clone()).or_insert(*at);
            if *at > *n {
                *n = *at;
            }
        }
        row.readings
            .read(as_of, week.readings, complete && !week.clipped);
        let _ = root.drain();
        true
    }

    /// **EXHAUSTIVE, WITH NO `_` ARM.** One arm appends — `GaugeObserved`, the
    /// fact `emit_gauge_fact` sends after every RECORDED reading — and everything
    /// else says where it lives.
    fn folds(&mut self, input: &Stamped<Input>, tx: &mut Tx<'_, GaugeHistoryRoot>) {
        match &input.value {
            Input::Fact(Fact::GaugeObserved { account, axis, row }) => {
                if !AXES.contains(&axis.as_str()) {
                    return;
                }
                // Only a key a consumer holds: a reading for an account nobody
                // subscribed is not a row to mint.
                if !tx.rows.contains(account) {
                    return;
                }
                let Some((at, reading)) = reading_of(axis, row) else {
                    return;
                };
                let held = tx.rows.get(account).and_then(|r| r.newest.get(axis).copied());
                // **IDENTITY, NOT ARRIVAL.** The fact re-reads the newest stored
                // observation, so two facts can carry one reading; the observed
                // instant per axis refuses the second, and refuses a reading the
                // hydrate already holds.
                if held.is_some_and(|h| at <= h) {
                    return;
                }
                let r = tx.rows.key(account.clone());
                r.newest.insert(axis.clone(), at);
                r.readings.append(at, reading);
            }

            // ── everything else, and where it lives ──────────────────────
            Input::Push(Push::Event(_))
            | Input::Push(Push::Status { .. })
            | Input::Push(Push::Liveness { .. })
            | Input::Push(Push::Progress { .. })
            | Input::Push(Push::Lifecycle(_))
            | Input::Push(Push::Info { .. })
            | Input::Fact(Fact::ThreadHeader { .. })
            | Input::Fact(Fact::ThreadSettings { .. })
            | Input::Fact(Fact::ThreadPlacement { .. })
            | Input::Fact(Fact::ThreadSeen { .. })
            | Input::Fact(Fact::ThreadCompaction { .. })
            | Input::Fact(Fact::ThreadQueue { .. })
            | Input::Fact(Fact::ThreadChildren { .. })
            | Input::Fact(Fact::ThreadRolled { .. })
            | Input::Fact(Fact::ThreadSubagents { .. })
            | Input::Fact(Fact::ThreadChildActivity { .. })
            // The account's CURRENT state is `accounts`'.
            | Input::Fact(Fact::LimitRecorded { .. })
            | Input::Fact(Fact::LimitCleared { .. })
            | Input::Fact(Fact::PoolShare { .. })
            | Input::Fact(Fact::AccountDisabled { .. })
            | Input::Fact(Fact::LoginHealth { .. })
            | Input::Fact(Fact::CredentialChanged { .. })
            | Input::Fact(Fact::AccountRegistered { .. })
            | Input::Fact(Fact::AccountRemoved { .. })
            | Input::Fact(Fact::Pressure { .. })
            | Input::Fact(Fact::Beat { .. })
            | Input::Fact(Fact::WarningsSampled { .. }) => {}
        }
    }
}

impl Handles<AccountWeek> for crate::runtime::ThreadManager {
    /// The same store read the `gauge/history` request made
    /// (`Store::observations_between`, newest-kept under the cap), once per
    /// percentage axis, merged oldest first.
    fn hydrate(&self, r: AccountWeek) -> Hydration<WeekRead> {
        if !self.account_pool().iter().any(|a| a == &r.account) {
            return Hydration::Complete(vec![WeekRead {
                member: false,
                readings: Vec::new(),
                clipped: false,
            }]);
        }
        // The daemon's clock, which stamps every observation (`store::now_rfc3339`)
        // — a window bounded by a different clock could end before its own
        // newest reading.
        let now = crate::clock::now();
        let since = (now - chrono::Duration::days(DAYS)).to_rfc3339();
        let until = now.to_rfc3339();
        let mut readings = Vec::new();
        let mut clipped = false;
        for axis in Axis::PERCENT_AXES {
            let rows = match self.store.observations_between(
                &r.account,
                axis.as_str(),
                &since,
                &until,
                PER_AXIS,
            ) {
                Ok(rows) => rows,
                Err(e) => {
                    return Hydration::Partial {
                        got: Vec::new(),
                        reason: format!("reading {}'s {} history: {e}", r.account, axis.as_str()),
                    }
                }
            };
            clipped |= rows.len() >= PER_AXIS;
            for o in rows {
                let Some(at) = stamp(&o.observed_at) else {
                    continue;
                };
                readings.push((
                    at,
                    Reading {
                        axis: o.axis,
                        pct: o.pct,
                        resets_at: o.resets_at,
                        source: o.source,
                    },
                ));
            }
        }
        readings.sort_by_key(|(at, _)| *at);
        Hydration::Complete(vec![WeekRead {
            member: true,
            readings,
            clipped,
        }])
    }
}
