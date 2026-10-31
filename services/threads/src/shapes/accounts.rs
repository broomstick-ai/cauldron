//! # The `accounts` shape — what the account board draws, as one fold
//!
//! The board USED TO issue five RPCs on every `sub_ack`/`info` and then join
//! them by label, dropping whatever did not match. It chased placement per
//! thread because no live frame said WHERE, and it kept a 5s lease on a 1 Hz
//! beat because a frozen source otherwise shows a confident stale count. This
//! shape replaced all of that with held state plus row upserts, and at stage
//! S3 every one of those mechanisms was DELETED from the board rather than
//! left dormant — so this fold is not the board's preferred source, it is its
//! only one.
//!
//! ## Keys
//!
//! `pool().names()` — "this is the crate's account list" (`pool.rs` module doc)
//! — union every account a NON-IDLE thread row names, after alias resolution.
//!
//! **`default` is never a key.** Operator ruling: *the machine login is a NOTE
//! on the account it `folds_into`.* Rev 3 §3.1 gave it a `registered: false`
//! row of its own; that is a DEFECT, not an option — `accounts.rs::mark_live_login`
//! (`:3173`) exists precisely to stop the machine default and the pool account
//! it is signed in as being drawn as two cards with one identity, and a
//! `default` row would reintroduce exactly that duplication from the other
//! side. So the machine default's credential facts land on
//! [`AccountRow::machine_default`] of the account it folds into, and a thread
//! placed on `default` is folded under that same account.
//!
//! ## Tracking
//!
//! `Lazy`. The one expensive input is the credential sampler (P6: four
//! `security` invocations), and it should not run on a machine nobody is
//! watching — `hub.tracked("accounts")` is its gate. The cost is that the first
//! export after a board reconnect waits for init; a reconnect already re-reads
//! five RPCs today.
//!
//! ## What is NOT here
//!
//! * **No threads (R2.S8, version 2).** The `threads` cell — a map of thread id
//!   to six fields of `crate::shapes::threads`' row, keyed under the account a
//!   thread was placed on — was the one violation of the nesting law on this
//!   producer and is GONE. The roster lives in `crate::shapes::nonidle`
//!   (`threads/non-idle`), keyed by the pool, each entry naming its account.
//!   What this fold still keeps about threads is [`AccountRow::placed`]: a bare
//!   set of thread ids per key, carried on NO wire, because the key set and the
//!   `moves` rule below are both statements about where threads are.
//! * **`Liveness` is not folded.** Placement is decided by the `Status` edge
//!   live and by `ThreadRows` at init, so the read path and the live path agree
//!   by definition. No TTL, no
//!   lease, no stray-beat ghost. The 1 Hz beat is transport noise to this fold.
//!   (`Liveness.account` was retired at S3, as this note predicted it would be:
//!   its one reader was `live.py::_note_account`, and that module is deleted.
//!   `Status.account` stays and is what this fold reads — the edge is where a
//!   placement is decided, and a beat repeating it is a second source for one
//!   fact.)
//! * **No clock.** Every `.emit()` takes the input's stamp. There is no `age`,
//!   no `days_left`, no `age_seconds`: cells carry the timestamp and the
//!   consumer subtracts at render.
//! * **No `Counter`.** `threads` retains its whole ledger, so every apparent
//!   counter is either a difference of durable positions (`unread`, a
//!   `Register`) or a `count` over held rows (derived).

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{json, Value};

use crate::event::{Fact, Push};
use shapewire::cell::Cells;
use shapewire::declare_cells;
use shapewire::operation::{
    DType::{Enum, List, Map as MapOf, Opaque, Path, Ref, Scalar as Sc, Struct, Timestamp as Ts},
    Presence::{Optional, Required},
    Scalar::{Bool, Float, Str},
    TimeEncoding::{EpochMillis, Rfc3339},
};
use crate::shape::{
    CellState, Handles, Hydrate, HydrateRequest, Hydration, Input, Map, Op, Register, Root,
    Sequence, Shape, Stamped, Timestamp, Tracking, Tx,
};

/// The machine default's label. It is never a key in this shape.
const DEFAULT_LABEL: &str = crate::accounts::DEFAULT_ACCOUNT;

// ─────────────────────────────────────────────────────────────────────────────
// Row types — each is the ONE type both the fact and the hydrate response carry
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LimitRow {
    pub until: String,
    pub axis: Option<String>,
    pub prose: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct HealthRow {
    pub healthy: bool,
    pub dead_since: Option<String>,
    pub dead_reason: Option<String>,
}

/// **The credential picture for one POOL account.**
///
/// `clone_of` is the ruling-6 addition: `SlotHealth::CloneOf { of }`
/// (`accounts.rs:2723`) reaches the board as `health.state == "clone-of"` plus
/// `health.of`, and drives `health_badge` "CLONE of X" (`model.py:190-193`,
/// drawn at `render.py:626`) and `health_note` (`render.py:1074`). Rev 3 §3.2
/// listed `slot_state` and not `of`, so the badge could not have been drawn
/// from this shape at all. It is a field, not a second cell, because it is an
/// attribute OF the slot state — the two can never disagree if they are read
/// out of the same enum.
///
/// **No `days_left`.** The wire carries `access_expires_at_ms` and the consumer
/// subtracts, which is the `age` ruling applied where the board used to call
/// `fmt_days`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CredentialRow {
    pub mode: Option<String>,
    pub subscription_type: Option<String>,
    pub access_expires_at_ms: Option<i64>,
    pub refresh_expires_at_ms: Option<i64>,
    pub access_expired: bool,
    pub identity: Option<crate::accounts::IdentityFingerprint>,
    pub secret_sha8: Option<String>,
    pub keychain_service: String,
    pub is_live_login: bool,
    pub slot_state: String,
    /// RULING 6 — the drawn field rev 3 had no cell for.
    pub clone_of: Option<String>,
    /// RULING 5/6 — which account this slot folds into, carried so a consumer
    /// never has to re-derive the identity match `mark_live_login` already did.
    pub folds_into: Option<String>,
    pub live_store_expires_at_ms: Option<i64>,
    pub live_store_state: Option<String>,
}

/// **The machine default, as a NOTE on the account it folds into** (ruling 5).
///
/// Not a row, not a card, not a key. `is_live_login` on the host row already
/// says "this account is what the machine default is signed in as"; this says
/// what that machine-default store itself looks like, so the board can draw the
/// live-store lines it draws today without a second card claiming the same
/// identity.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MachineDefaultNote {
    pub label: String,
    pub state: String,
    pub access_expires_at_ms: Option<i64>,
    pub refresh_expires_at_ms: Option<i64>,
    pub access_expired: bool,
    pub subscription_type: Option<String>,
    pub identity: Option<crate::accounts::IdentityFingerprint>,
    pub secret_sha8: Option<String>,
    pub keychain_service: String,
}

/// **IS THIS ACCOUNT IN THE POOL AT ALL — the inclusion decision, as its own
/// row in its own cell.**
///
/// It is not a field on the `share` cell, and that is the whole point. Until
/// 2026-09-20 disabled WAS `share == 0`, so a consumer learned it by comparing a
/// float against zero inside a payload about quantity. The operator ruled the
/// two apart:
///
/// > *"I want disabled to be independent of share. Whether to include it in the
/// > pool supersedes how much should be made available to the pool."*
///
/// **Supersedes** is drawable: a board reads `disabled` for whether the account
/// is in play and `share` for how much of it is lent, and the two never have to
/// be reconciled because neither is derived from the other. An account disabled
/// on a 0.6 share reports `disabled: true` HERE and `share: 0.6` THERE, at the
/// same instant, both true.
///
/// `disabled_at` and `note` are `Some` only while `disabled` is true — enabling
/// deletes the state, so there is no instant and no prose to carry.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct DisabledRow {
    pub disabled: bool,
    pub disabled_at: Option<String>,
    pub note: Option<String>,
}

/// **THE MACHINE LOGIN, AS A SHAPE-LEVEL FACT — including the case where
/// nothing hosts it.**
///
/// This closes the gap `SHAPES-TUI.md` §7.1 shipped at S2 and named precisely:
/// the board draws a *"this is your machine login, and no named account carries
/// it yet — press `n` to give it a slot"* affordance, and under this shape it
/// could not, because
///
/// * `default` is never a key (ruling 5), so there is no card to hang it on;
/// * `shared.folds_into` is `{}` BOTH when there is no machine-default store at
///   all and when there is one that nothing hosts;
/// * the only discriminator on the wire was the free-text `reason` on each row's
///   `machine_default` cell — `"no machine-default store was read"` versus
///   `"the machine-default store is not signed in as this account"`.
///
/// The client refused to branch on prose, correctly: branching on a reason
/// string is a consumer re-deriving a daemon fact, which is the thing `caps.py`
/// exists to forbid. So the daemon says it structurally instead, and says it
/// once, keylessly, where the other non-per-account facts already live.
///
/// **Three states, told apart by construction and not by text:**
///
/// | cell | meaning |
/// |---|---|
/// | `Known(None)` | no machine-default store was read at all |
/// | `Known(Some{ hosted_by: Some(a) })` | the machine login is signed in as pool account `a` |
/// | `Known(Some{ hosted_by: None, unhosted: true })` | **a machine login exists and NO pool account carries it** — draw the affordance |
///
/// `unhosted` is carried explicitly rather than left as `hosted_by.is_none()`.
/// The two can never disagree (it is set from the same `Option` one line apart)
/// and the redundancy buys the thing that matters: a consumer written against
/// this field cannot accidentally render the affordance for the `Known(None)`
/// case by testing an absent value, because an absent `MachineLogin` has no
/// `unhosted` field to be false.
///
/// `default` is STILL not a key. The affordance is a shape-level fact about the
/// machine, which is exactly why it belongs in `shared` and not in a row.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MachineLogin {
    /// The slot label — `default` today, read rather than assumed.
    pub label: String,
    /// The pool account this machine login is signed in as, or `None`.
    pub hosted_by: Option<String>,
    /// **The affordance's one condition**: a machine login was read and no pool
    /// account carries it.
    pub unhosted: bool,
}

/// **`accounts.by_email.<email>` — THE PRESSURE, KEYED BY EMAIL** (R9.PR1).
///
/// The value is the ONE pressure ([`crate::budgets::pressure_value`]) that
/// placement ranks — copied off the same `PressureRow` the per-row `pressure`
/// cell carries, never recomputed here. Keyed by the R12 account id
/// ([`crate::accounts::AccountId`], `accounts.rs` "R12.1 — AN ACCOUNT IS ITS
/// EMAIL") read from the slot's `login-hint`
/// ([`crate::accounts::recorded_login_address`]) — R12.2's rekey has not
/// landed, so that is the only email the daemon holds for a slot today; when it
/// lands this map is re-keyed off the rekeyed row key instead. An account
/// with no parseable `login-hint` has no entry (its per-row `pressure` cell
/// still carries the number).
///
/// **∞ on the wire:** `pressure: null` + `infinite: true`. `pressure: null` +
/// `infinite: false` is UNRANKED, and `unranked_reason` says why.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EmailPressure {
    /// The row key (account name) this email belongs to.
    pub account: String,
    pub pressure: Option<f64>,
    pub infinite: bool,
    pub unranked_reason: Option<String>,
    /// The instant the value was computed for (the `now` of the formula).
    pub as_of: String,
}

impl EmailPressure {
    /// Lift the entry off one `PressureRow` as it reached this shape.
    fn from_row(account: &str, row: &Value, as_of: &str) -> Self {
        Self {
            account: account.to_string(),
            pressure: row.get("pressure").and_then(Value::as_f64),
            infinite: row.get("infinite").and_then(Value::as_bool).unwrap_or(false),
            unranked_reason: row
                .get("unranked_reason")
                .and_then(Value::as_str)
                .map(str::to_string),
            as_of: as_of.to_string(),
        }
    }
}

/// The email key for an account's slot: `login-hint`, normalised by
/// [`crate::accounts::AccountId::parse`]. `None` = no hint, or not an email.
fn email_of(login_dir: Option<&str>) -> Option<String> {
    let addr = crate::accounts::recorded_login_address(login_dir)?;
    crate::accounts::AccountId::parse(&addr)
        .ok()
        .map(|id| id.as_str().to_string())
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Off,
    On,
}

/// One placement move. **Named `moves`, not `failovers`**, because
/// `AccountSwitched.reason` distinguishes a deliberate placement from a
/// failover and the count includes both. A `Sequence` item, `since_init`
/// always, qualified on the wire by `covers_since`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MoveItem {
    pub thread_id: String,
    pub direction: Direction,
    pub other: String,
    pub reason: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// The row and the root
// ─────────────────────────────────────────────────────────────────────────────

/// Eleven cells. Four of the six cell kinds are used; `Counter` and `Edge` have
/// no instance here and the module docs say why.
#[derive(Debug)]
pub struct AccountRow {
    pub registered: Register<bool>,
    pub login_dir: Register<Option<String>>,
    /// One element of `budgets.pool_share_rows`' `shares` array, verbatim.
    ///
    /// **HOW MUCH, never WHETHER.** The `disabled` cell below is the other
    /// question and it is genuinely another cell — own fact, own producer, own
    /// hydrate handler — since the operator separated them on 2026-09-20. The
    /// `disabled` key inside this blob is still there because `pool_share_row`
    /// is one builder for several surfaces, but a fold consumer reads the CELL,
    /// which moves on its own fact and its own stamp.
    pub share: Register<Value>,
    /// **IS THIS ACCOUNT IN THE POOL AT ALL** — see [`DisabledRow`]. Its own
    /// cell because it is its own fact: it moves on `Fact::AccountDisabled` and
    /// on nothing else, so a share edit cannot dirty it and a disable cannot
    /// pretend to be a share edit.
    pub disabled: Register<DisabledRow>,
    /// Per-axis, ordered by `observed_at` rather than by the input stamp, so a
    /// late-arriving older reading is IGNORED rather than applied. (The
    /// critique's last "could not determine" asked which rule wins when a newer
    /// input carries an older `observed_at`: the ordering key wins. That is
    /// what `Register::emit_ordered` implements and it is stated here.)
    pub gauge: Map<String, Register<Value>>,
    pub limit: Register<Option<LimitRow>>,
    pub health: Register<HealthRow>,
    pub credential: Register<CredentialRow>,
    /// PRODUCED by P15, not derived by a consumer. One element of
    /// `budgets.pressure_table`' `accounts` array, plus its `as_of`.
    pub pressure: Register<Value>,
    /// **WHICH NON-IDLE THREADS ARE PLACED HERE — BOOKKEEPING, NOT A CELL.**
    ///
    /// This was the `threads` cell until R2.S8: six fields of
    /// `crate::shapes::threads`' row under each thread id, the one violation of
    /// the nesting law on this producer. That roster is `threads/non-idle` now,
    /// and the cell is gone from the manifest, the row and every export — which
    /// is why [`Accounts::VERSION`] is 2.
    ///
    /// What is left is the bare set of ids, and it is carried on NO wire. It is
    /// kept because three things this shape DOES publish are statements about
    /// where threads are: the key set (an account a non-idle thread names is a
    /// key even when it is not registered — "a login that left the roster with
    /// work on it is a fact"), the removal of such a key once its last thread
    /// goes idle, and `moves`' append-only-when-observed rule (F8/F9). Dropping
    /// the set would have changed all three silently; keeping it changes none.
    pub placed: BTreeSet<String>,
    pub moves: Sequence<MoveItem>,
    /// Ruling 5. `Some` only on the account the machine default folds into.
    pub machine_default: Register<Option<MachineDefaultNote>>,
}

impl Default for AccountRow {
    fn default() -> Self {
        Self {
            registered: Register::new(),
            login_dir: Register::new(),
            share: Register::new(),
            disabled: Register::new(),
            gauge: Map::new(),
            limit: Register::new(),
            health: Register::new(),
            credential: Register::new(),
            pressure: Register::new(),
            placed: BTreeSet::new(),
            moves: Sequence::new(32, chrono::Duration::hours(24)),
            machine_default: Register::new(),
        }
    }
}

impl AccountRow {
    fn to_json(&self, key: &str) -> Value {
        json!({
            "account": key,
            // `null` for a cell that is not known — never a fabricated `false`,
            // which read as "not registered" under a slot the `cells` block said
            // was `not_hydrated`. Same repair as `threads`' three counters.
            "registered": self.registered.now(),
            "login_dir": self.login_dir.now().cloned().flatten(),
            "share": self.share.now().cloned(),
            "disabled": self.disabled.now().cloned(),
            "gauge": Value::Object(
                self.gauge
                    .iter()
                    .map(|(a, c)| (a.clone(), c.now().cloned().unwrap_or(Value::Null)))
                    .collect(),
            ),
            "limit": self.limit.now().cloned().flatten(),
            "health": self.health.now().cloned(),
            "credential": self.credential.now().cloned(),
            "pressure": self.pressure.now().cloned(),
            "moves": self.moves.to_json(),
            "machine_default": self.machine_default.now().cloned().flatten(),
        })
    }

    fn cells_json(&self) -> Value {
        json!({
            "registered": self.registered.state(),
            "login_dir": self.login_dir.state(),
            "share": self.share.state(),
            "disabled": self.disabled.state(),
            "gauge": self.gauge.state(),
            "limit": self.limit.state(),
            "health": self.health.state(),
            "credential": self.credential.state(),
            "pressure": self.pressure.state(),
            "moves": self.moves.state(),
            "machine_default": self.machine_default.state(),
        })
    }

    fn mark_dead(&mut self, reason: &str) {
        self.registered.mark_dead(reason);
        self.login_dir.mark_dead(reason);
        self.share.mark_dead(reason);
        self.disabled.mark_dead(reason);
        self.gauge.mark_dead(reason);
        self.limit.mark_dead(reason);
        self.health.mark_dead(reason);
        self.credential.mark_dead(reason);
        self.pressure.mark_dead(reason);
        self.moves.mark_dead(reason);
        self.machine_default.mark_dead(reason);
    }
}

/// **The held state — a keyed `Map` PLUS shape-level cells.**
///
/// Ruling 6, and the critique's F5.2 ("a keyed shape whose root is a `Map` has
/// no place for a shape-level scalar") is the reason this is a struct rather
/// than the bare `Map<String, AccountRow>` rev 3 declared. Three drawn things
/// are not per-account:
///
/// * **`warn_below_days`** — the amber threshold `_title_right` uses
///   (`render.py:640-646`), asserted onto the `accounts.health` payload by
///   `tests/ws_protocol.rs` *"so a reader never hardcodes it"*. Copying it into
///   every row would be the same constant N times; hardcoding it in the board
///   would break the assertion's intent.
/// * **`folds_into`** — the alias relationship (`model.py:424`, `:471-478`),
///   which is a fact about a PAIR, not about one account.
/// * the death/`shared` frames both need somewhere keyless to live.
///
/// They ride on every export under `shared` and move as a `shared` derived
/// event.
pub struct AccountsRoot {
    pub rows: Map<String, AccountRow>,
    pub warn_below_days: Register<f64>,
    /// alias → the account it folds into. Today that is exactly
    /// `{"default": "<the pool account the machine store is signed in as>"}`.
    ///
    /// **It cannot answer §7.1's question and never could**: it is empty both
    /// when there is no machine login and when there is one nothing hosts. That
    /// is what [`Self::machine_login`] is for, and the two are set from the same
    /// slot read one line apart so they cannot drift.
    pub folds_into: Register<BTreeMap<String, String>>,
    /// **THE MACHINE LOGIN, AND WHETHER ANYTHING HOSTS IT** — see
    /// [`MachineLogin`]. `SHAPES-TUI.md` §7.1's gap, closed structurally on the
    /// daemon side so a client renders the "press `n` to give it a slot"
    /// affordance from a typed field instead of from a reason string.
    pub machine_login: Register<Option<MachineLogin>>,
    /// **email → the one pressure value** — see [`EmailPressure`] (R9.PR1).
    pub by_email: Register<BTreeMap<String, EmailPressure>>,
    shared_dirty: bool,
    dead: Option<String>,
}

impl Default for AccountsRoot {
    fn default() -> Self {
        Self {
            rows: Map::new(),
            warn_below_days: Register::new(),
            folds_into: Register::new(),
            machine_login: Register::new(),
            by_email: Register::new(),
            shared_dirty: false,
            dead: None,
        }
    }
}

impl AccountsRoot {
    /// **Where a placement actually lands.** `default` folds into the pool
    /// account the machine store is signed in as (ruling 5); everything else is
    /// itself.
    pub fn resolve(&self, account: &str) -> Option<String> {
        if let Some(m) = self.folds_into.now() {
            if let Some(target) = m.get(account) {
                return Some(target.clone());
            }
        }
        if account == DEFAULT_LABEL {
            // The machine default with no identity match has nowhere to fold.
            // It still does not get a card — it gets counted as unplaced, which
            // is the honest answer and is visible on `shapes.list`.
            return None;
        }
        Some(account.to_string())
    }

    fn set_shared_folds(&mut self, at: Timestamp, m: BTreeMap<String, String>) {
        if self.folds_into.now() == Some(&m) {
            return;
        }
        self.folds_into.emit(at, m);
        self.shared_dirty = true;
    }

    /// Idempotent like its siblings: a re-read that finds the same machine login
    /// must not mark `shared` dirty, or every hydrate would emit a `shared`
    /// event that changed nothing.
    fn set_shared_machine_login(&mut self, at: Timestamp, v: Option<MachineLogin>) {
        if self.machine_login.now() == Some(&v) {
            return;
        }
        self.machine_login.emit(at, v);
        self.shared_dirty = true;
    }

    /// Put one account's pressure under its email (R9.PR1). Any entry the
    /// same account held under ANOTHER email (its `login-hint` changed) is
    /// dropped, so an account is never listed twice. Idempotent like its
    /// siblings.
    fn set_shared_pressure(&mut self, at: Timestamp, email: Option<String>, e: EmailPressure) {
        let mut m = self.by_email.now().cloned().unwrap_or_default();
        m.retain(|k, v| v.account != e.account || Some(k) == email.as_ref());
        if let Some(email) = email {
            m.insert(email, e);
        }
        if self.by_email.now() == Some(&m) {
            return;
        }
        self.by_email.emit(at, m);
        self.shared_dirty = true;
    }

    /// The email of a row, off its `login_dir` cell.
    fn email_of_row(&self, key: &str) -> Option<String> {
        let dir = self.rows.get(&key.to_string())?.login_dir.now().cloned().flatten();
        email_of(dir.as_deref())
    }

    fn set_warn_below_days(&mut self, at: Timestamp, v: f64) {
        if self.warn_below_days.now() == Some(&v) {
            return;
        }
        self.warn_below_days.emit(at, v);
        self.shared_dirty = true;
    }
}

impl Root for AccountsRoot {
    fn keys(&self) -> Vec<String> {
        self.rows.keys().cloned().collect()
    }

    fn row(&self, key: &str) -> Option<Value> {
        self.rows.get(&key.to_string()).map(|r| r.to_json(key))
    }

    fn cells(&self, key: &str) -> Option<Value> {
        self.rows.get(&key.to_string()).map(|r| r.cells_json())
    }

    fn shared(&self) -> Value {
        json!({
            "warn_below_days": self.warn_below_days.now(),
            "folds_into": self.folds_into.now(),
            "machine_login": self.machine_login.now().cloned().flatten(),
            "by_email": self.by_email.now(),
        })
    }

    fn shared_cells(&self) -> Value {
        json!({
            "warn_below_days": self.warn_below_days.state(),
            "folds_into": self.folds_into.state(),
            "machine_login": self.machine_login.state(),
            "by_email": self.by_email.state(),
        })
    }

    fn drain(&mut self) -> crate::shape::Outbox {
        let dirty = self.rows.take_dirty_keys();
        let gone = self.rows.take_removed();
        // **A KEY THIS FOLD MINTED IS A KEY THAT JOINED THE POOL.** Under a
        // `Whole` hydrate holding and membership are one act (`Map::key` marks
        // both), so this is the arrival event and nothing more is owed.
        let added = self.rows.take_added();
        let mut transitions = Vec::new();
        let mut rows = Vec::new();
        for k in dirty {
            let Some(r) = self.rows.get_mut(&k) else {
                continue;
            };
            for item in r.moves.take_pending() {
                transitions.push((k.clone(), "moves", item));
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
        self.dead = Some(reason.to_string());
        self.warn_below_days.mark_dead(reason);
        self.folds_into.mark_dead(reason);
        self.machine_login.mark_dead(reason);
        self.by_email.mark_dead(reason);
        self.rows.mark_dead(reason);
        for (_, r) in self.rows.entries_mut() {
            r.mark_dead(reason);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The hydrate requests — nine, each bounded `ThreadManager: Handles<R>`
// ─────────────────────────────────────────────────────────────────────────────

macro_rules! request {
    ($name:ident, $wire:literal, $resp:ty) => {
        #[derive(Default)]
        pub struct $name;
        impl HydrateRequest for $name {
            const NAME: &'static str = $wire;
            type Response = $resp;
        }
    };
}

#[derive(Debug, Clone)]
pub struct RosterRow {
    pub name: String,
    pub login_dir: Option<String>,
}
#[derive(Debug, Clone)]
pub struct KeyedValue {
    pub account: String,
    pub value: Value,
}
#[derive(Debug, Clone)]
pub struct GaugeRow {
    pub account: String,
    pub axis: String,
    pub row: Value,
}
#[derive(Debug, Clone)]
pub struct DisabledEntry {
    pub account: String,
    pub row: DisabledRow,
}
#[derive(Debug, Clone)]
pub struct LimitEntry {
    pub account: String,
    pub limit: Option<LimitRow>,
}
#[derive(Debug, Clone)]
pub struct HealthEntry {
    pub account: String,
    pub health: HealthRow,
}
#[derive(Debug, Clone)]
pub struct ThreadEntry {
    pub thread_id: String,
    pub account: String,
    pub state: String,
    pub executing: bool,
    pub queued: u64,
    pub unread: u64,
    pub detail: Option<String>,
    pub since: Option<Timestamp>,
}

request!(PoolRoster, "pool_roster", RosterRow);
request!(PoolShares, "pool_shares", KeyedValue);
request!(DisabledAccounts, "account_disabled", DisabledEntry);
request!(GaugeReadings, "gauge_readings", GaugeRow);
request!(AccountLimits, "account_limits", LimitEntry);
request!(LoginHealthRows, "login_health", HealthEntry);
request!(LoginSlots, "login_slots", crate::accounts::SlotReport);
request!(PressureRows, "pressure", KeyedValue);
request!(ThreadRows, "thread_rows", ThreadEntry);

// ─────────────────────────────────────────────────────────────────────────────
// The shape
// ─────────────────────────────────────────────────────────────────────────────

pub struct Accounts;

// ─────────────────────────────────────────────────────────────────────────────
// THE CELL MANIFEST — what this shape carries, declared (`shapewire::cell`)
// ─────────────────────────────────────────────────────────────────────────────
//
// One entry per key of `AccountRow::cells_json` and of `AccountsRoot::shared_cells`;
// `tests/cell_manifest.rs` asserts the key sets are equal. The value shapes are
// read off the row types at the top of this file. `share`, `gauge` and `pressure`
// are `Opaque`: each is a blob another subsystem (`budgets`) spells, carried
// verbatim, and a consumer that read inside one would be re-deriving that
// subsystem's meaning. The `threads` map that sat between `pressure` and `moves`
// is gone (R2.S8): it is `threads/non-idle`'s roster now, and this shape is
// version 2 because a consumer built against version 1 read it.
declare_cells! {
    /// Per-key cells of `accounts`.
    pub ACCOUNTS_CELLS = [
        ("registered", Sc(Bool), Required, []),
        ("login_dir", Path, Optional, []),
        ("share", Opaque, Required, []),
        ("disabled", Struct("DisabledRow"), Required, [
            ("disabled", Sc(Bool), Required),
            ("disabled_at", Ts(Rfc3339), Optional),
            ("note", Sc(Str), Optional),
        ]),
        ("gauge", MapOf(&Opaque), Required, []),
        ("limit", Struct("LimitRow"), Optional, [
            ("until", Ts(Rfc3339), Required),
            ("axis", Sc(Str), Optional),
            ("prose", Sc(Str), Optional),
        ]),
        ("health", Struct("HealthRow"), Required, [
            ("healthy", Sc(Bool), Required),
            ("dead_since", Ts(Rfc3339), Optional),
            ("dead_reason", Sc(Str), Optional),
        ]),
        ("credential", Struct("CredentialRow"), Required, [
            ("mode", Sc(Str), Optional),
            ("subscription_type", Sc(Str), Optional),
            ("access_expires_at_ms", Ts(EpochMillis), Optional),
            ("refresh_expires_at_ms", Ts(EpochMillis), Optional),
            ("access_expired", Sc(Bool), Required),
            ("identity", Struct("IdentityFingerprint"), Optional),
            ("secret_sha8", Sc(Str), Optional),
            ("keychain_service", Sc(Str), Required),
            ("is_live_login", Sc(Bool), Required),
            ("slot_state", Sc(Str), Required),
            ("clone_of", Ref("account"), Optional),
            ("folds_into", Ref("account"), Optional),
            ("live_store_expires_at_ms", Ts(EpochMillis), Optional),
            ("live_store_state", Sc(Str), Optional),
        ]),
        ("pressure", Opaque, Required, []),
        ("moves", List(&Struct("MoveItem")), Required, [
            ("thread_id", Ref("thread"), Required),
            ("direction", Enum(&["off", "on"]), Required),
            ("other", Ref("account"), Required),
            ("reason", Sc(Str), Required),
            ("at", Ts(Rfc3339), Required),
        ]),
        ("machine_default", Struct("MachineDefaultNote"), Optional, [
            ("label", Sc(Str), Required),
            ("state", Sc(Str), Required),
            ("access_expires_at_ms", Ts(EpochMillis), Optional),
            ("refresh_expires_at_ms", Ts(EpochMillis), Optional),
            ("access_expired", Sc(Bool), Required),
            ("subscription_type", Sc(Str), Optional),
            ("identity", Struct("IdentityFingerprint"), Optional),
            ("keychain_service", Sc(Str), Required),
        ]),
    ]
}

declare_cells! {
    /// Shape-level cells of `accounts` — the `shared_cells` block.
    pub ACCOUNTS_SHARED = [
        ("warn_below_days", Sc(Float), Required, []),
        ("folds_into", MapOf(&Ref("account")), Required, []),
        ("machine_login", Struct("MachineLogin"), Optional, [
            ("label", Sc(Str), Required),
            ("hosted_by", Ref("account"), Optional),
            ("unhosted", Sc(Bool), Required),
        ]),
        ("by_email", MapOf(&Struct("EmailPressure")), Required, [
            ("account", Ref("account"), Required),
            ("pressure", Sc(Float), Optional),
            ("infinite", Sc(Bool), Required),
            ("unranked_reason", Sc(Str), Optional),
            ("as_of", Ts(Rfc3339), Required),
        ]),
    ]
}

impl Shape<crate::shape::Input> for Accounts {
    const NAME: &'static str = "accounts";
    /// **VERSION 2 — THE `threads` CELL IS GONE (R2.S8).** Version 1 carried it
    /// and a consumer built against version 1 read it (`running()` in the seam
    /// reference, and every face vendored from it). `SERVES` stays the trait's
    /// empty default, deliberately: serving version 1 "with a warning" would be
    /// serving a contract whose one moved cell this producer no longer has, so
    /// a version-1 link is REFUSED at subscribe, naming `[2]` — rule 2.6's
    /// third state, which is the rule working.
    const VERSION: u32 = 2;
    const CELLS: Cells = ACCOUNTS_CELLS;
    const SHARED: Cells = ACCOUNTS_SHARED;
    /// The key-level nesting edges: `moves[].thread_id` names keys of
    /// `threads` (registered AFTER this shape — a mutual edge, which is why
    /// `Hub::dangling_refs` and not `register` decides existence), and the
    /// `clone_of` / `folds_into` / `hosted_by` refs name keys of this shape.
    const REFERS: &'static [(&'static str, &'static str)] =
        &[("thread", "threads"), ("account", "accounts")];
    const TRACKING: Tracking = Tracking::Lazy;
    /// **DECLARED, NOT DEFAULTED**, even though `Fleet` IS the default.
    ///
    /// This is the one existing call site of `KeyScope`, and a const whose only
    /// instance is an omission is a const nobody can see. `Fleet` is the right
    /// answer and it is an answer, not an absence: the key set is
    /// `pool().names()` union every account a non-idle thread row names, which is
    /// the whole universe of accounts this producer knows — so a keyless subscribe
    /// IS an inventory, and a keyed subscribe to an account that has not appeared
    /// yet is standing interest in a name a pool reload may mint. Neither of those
    /// is true of an `Active` or an `OnDemand` shape, and the next shape written
    /// here has to choose rather than inherit.
    const HYDRATE: Hydrate = Hydrate::Whole;
    /// A `pool_reloaded` legitimately touches every key at once — five today —
    /// so the warning must sit just above that and not above 16, or it would
    /// never fire on the fold it exists to catch.
    const BURST: usize = 8;

    type Producer = crate::runtime::ThreadManager;
    type Root = AccountsRoot;

    fn root(&self) -> AccountsRoot {
        AccountsRoot::default()
    }

    fn hydrates(
        &mut self,
        p: &crate::runtime::ThreadManager,
        root: &mut AccountsRoot,
        as_of: Timestamp,
    ) {
        // The threshold is the daemon's own constant. Putting it in `shared`
        // rather than in every row is ruling 6's home; the consumer reads it
        // off the export and never hardcodes it.
        root.set_warn_below_days(as_of, crate::accounts::HEALTH_WARN_BELOW_DAYS);

        // ── the roster: the CORRECT COUNT OF ACCOUNTS ────────────────────
        //
        // A `Complete(vec![])` for a populated pool is a handler that LIED, and
        // "4 accounts rendered as 0" is the defect that makes that worth
        // saying: `tests/shapes.rs` asserts `rows.len() == pool.names().len()`.
        let roster = p.hydrate(PoolRoster);
        let roster_complete = roster.is_complete();
        let roster_reason = roster.reason();
        for r in roster.rows() {
            let row = root.rows.key(r.name.clone());
            row.registered.read(as_of, true);
            row.login_dir.read(as_of, r.login_dir.clone());
        }
        root.rows.mark_read(as_of, roster_complete);
        if let (false, Some(reason)) = (roster_complete, roster_reason) {
            root.rows.mark_not_hydrated(reason);
        }

        // ── the credential slots, and the machine default as a NOTE ──────
        let slots = p.hydrate(LoginSlots);
        let slots_complete = slots.is_complete();
        let slots_reason = slots.reason().unwrap_or_else(|| "no slot read".into());
        let slot_rows = slots.into_rows();
        let mut folds: BTreeMap<String, String> = BTreeMap::new();
        let machine_default = slot_rows.iter().find(|s| s.is_machine_default).cloned();
        if let Some(md) = machine_default.as_ref() {
            if let Some(target) = md.folds_into.clone() {
                folds.insert(md.label.clone(), target);
            }
        }
        root.set_shared_folds(as_of, folds.clone());
        // §7.1: the SAME slot read, said structurally. `folds` above is empty
        // in two different situations and this field tells them apart, which is
        // the whole gap S2 shipped.
        root.set_shared_machine_login(
            as_of,
            machine_default.as_ref().map(|md| MachineLogin {
                label: md.label.clone(),
                hosted_by: md.folds_into.clone(),
                unhosted: md.folds_into.is_none(),
            }),
        );
        for s in &slot_rows {
            if s.is_machine_default {
                // RULING 5: never its own row. It becomes a note on its host,
                // below, or it is simply absent when nothing hosts it.
                continue;
            }
            let row = root.rows.key(s.label.clone());
            row.credential.read(as_of, credential_row(s));
            if !slots_complete {
                row.credential.read_partial(as_of, credential_row(s));
            }
        }
        // The note lands on the host named by `folds_into` — and on NOBODY
        // else. Every other row's `machine_default` is `NotHydrated` WITH A
        // REASON rather than `Known(None)`, because those are different facts:
        // "the machine default does not fold into this account" is a statement
        // about a relationship that was checked, and an empty `Known` would
        // read as "checked, and it does, and it is blank".
        let host = machine_default.as_ref().and_then(|m| m.folds_into.clone());
        if let (Some(md), Some(host)) = (machine_default.as_ref(), host.clone()) {
            let row = root.rows.key(host);
            row.machine_default.read(as_of, Some(note_row(md)));
        }
        let others: Vec<String> = root
            .rows
            .keys()
            .filter(|k| Some(*k) != host.as_ref())
            .cloned()
            .collect();
        let why = match machine_default.as_ref() {
            Some(_) => "the machine-default store is not signed in as this account",
            None => "no machine-default store was read",
        };
        for k in others {
            root.rows.key(k).machine_default.not_hydrated(why);
        }
        // **Per-KEY `Partial` semantics, which the design left to invention**
        // (the critique's F8): a key with no slot row is `NotHydrated{reason}`,
        // NOT `Known` with an empty credential. Empty renders as empty;
        // not-hydrated renders `?`. Conflating them is what makes a `?` on a
        // screen unreadable.
        let seen: Vec<String> = slot_rows.iter().map(|s| s.label.clone()).collect();
        let missing: Vec<String> = root
            .rows
            .keys()
            .filter(|k| !seen.contains(k))
            .cloned()
            .collect();
        for k in missing {
            root.rows
                .key(k)
                .credential
                .not_hydrated(slots_reason.clone());
        }

        // ── shares, gauges, limits, health, pressure ─────────────────────
        hydrate_keyed(root, as_of, p.hydrate(PoolShares), |row, as_of, v| {
            row.share.read(as_of, v)
        });
        let pressure = p.hydrate(PressureRows);
        hydrate_keyed(root, as_of, pressure.clone(), |row, as_of, v| {
            row.pressure.read(as_of, v)
        });
        // R9.PR1: the same rows, keyed by email. `as_of` is the hydrate's.
        let stamp = as_of.to_rfc3339();
        let mut by_email: BTreeMap<String, EmailPressure> = BTreeMap::new();
        for r in pressure.rows() {
            if let Some(email) = root.email_of_row(&r.account) {
                by_email.insert(email, EmailPressure::from_row(&r.account, &r.value, &stamp));
            }
        }
        if root.by_email.now() != Some(&by_email) {
            root.by_email.read(as_of, by_email);
            root.shared_dirty = true;
        }

        // INCLUSION, read separately from the share it is now independent of.
        // A `Partial` here leaves the keys it did not answer `NotHydrated` with
        // the store's error on them, which is what makes an unreadable
        // `account_disabled` render as `?` rather than as "in the pool".
        let disabled = p.hydrate(DisabledAccounts);
        let disabled_reason = disabled.reason();
        let mut answered: Vec<String> = Vec::new();
        for d in disabled.rows() {
            root.rows.key(d.account.clone()).disabled.read(as_of, d.row.clone());
            answered.push(d.account.clone());
        }
        if let Some(reason) = disabled_reason {
            let unanswered: Vec<String> = root
                .rows
                .keys()
                .filter(|k| !answered.contains(k))
                .cloned()
                .collect();
            for k in unanswered {
                root.rows.key(k).disabled.not_hydrated(reason.clone());
            }
        }

        let gauges = p.hydrate(GaugeReadings);
        let gauge_reason = gauges.reason();
        for g in gauges.rows() {
            root.rows
                .key(g.account.clone())
                .gauge
                .key(g.axis.clone())
                .read(as_of, g.row.clone());
        }
        for k in root.keys() {
            let row = root.rows.key(k);
            match &gauge_reason {
                Some(r) if row.gauge.is_empty() => row.gauge.mark_not_hydrated(r.clone()),
                _ => row.gauge.mark_read(as_of, true),
            }
        }

        let limits = p.hydrate(AccountLimits);
        for l in limits.rows() {
            root.rows
                .key(l.account.clone())
                .limit
                .read(as_of, l.limit.clone());
        }

        let health = p.hydrate(LoginHealthRows);
        for h in health.rows() {
            root.rows
                .key(h.account.clone())
                .health
                .read(as_of, h.health.clone());
        }

        // ── where the non-idle threads are placed — bookkeeping only ─────
        //
        // No cell is filled here any more (R2.S8): the roster is
        // `threads/non-idle`'s. This read still mints a key for every account a
        // non-idle thread names, and records which threads are placed on it, so
        // the key set and `moves`' observed rule are what they were. `default`
        // is resolved through `folds_into` exactly as it is live.
        let threads = p.hydrate(ThreadRows);
        for t in threads.rows() {
            let Some(key) = root.resolve(&t.account) else {
                continue;
            };
            root.rows.key(key).placed.insert(t.thread_id.clone());
        }

        // `moves` has no hydrate entry at all and says so: it covers only what
        // this epoch has seen, and the export's `covers_since` bounds every
        // count a consumer derives from it.
        for k in root.keys() {
            root.rows.key(k).moves.mark_since_init(as_of);
        }

        // The hydrate walked `rows.key(..)` many times, which marks keys dirty.
        // A hydrate publishes nothing — the export that follows IS the
        // publication — so the outbox is cleared here rather than flushed.
        let _ = root.drain();
    }

    /// Declared so it is seeded at zero and is ALWAYS on `shapes.list`, which
    /// is what it was before the chassis moved. `tests/fake_daemon.py` encodes
    /// that expectation too.
    const COUNTERS: &'static [&'static str] = &["unplaced_status"];

    /// **`unplaced_status` — a `Status` push the daemon could not attribute.**
    ///
    /// Named HERE, by the shape that owns the number, rather than sitting as a
    /// hard-coded field in the chassis's `ShapeInfo` (docs/shapes.md 6.2,
    /// coupling 3). The chassis counts it and flattens it onto `shapes.list`
    /// under exactly this name, so the board reads what it always read.
    ///
    /// Counted before the pre-init return, deliberately: a status the daemon
    /// could not attribute is a fact about the DAEMON, not about whether this
    /// shape happened to be hydrated when it arrived, and dropping those would
    /// make the number quietly depend on init timing.
    fn counts(&self, input: &Stamped<Input>) -> Option<&'static str> {
        match &input.value {
            Input::Push(Push::Status { account: None, .. }) => Some("unplaced_status"),
            _ => None,
        }
    }

    fn folds(&mut self, input: &Stamped<Input>, tx: &mut Tx<'_, AccountsRoot>) {
        let at = tx.at();
        match &input.value {
            // ── placement, seen through the LEVEL ────────────────────────
            Input::Push(Push::Status {
                thread_id,
                state,
                executing: _,
                queued: _,
                unread: _,
                detail: _,
                account,
            }) => {
                let Some(account) = account else {
                    // `account: None` means the daemon genuinely does not know.
                    // A `"?"` key is NOT created — an unplaced thread is not an
                    // account — and the count is visible on `shapes.list`.
                    return;
                };
                let Some(key) = tx.resolve(account) else {
                    return;
                };
                // A move seen through the level: if this thread is currently
                // under another key, it left that key. This is F4's edge, and
                // it is why `move_thread`/`clear_park`/`update_settings` now
                // emit a `Status`.
                //
                // **`placed` IS NOT A CELL**, so moving an id in or out of it
                // publishes nothing and is reached with `get_mut`, which does
                // not mark the row dirty. What IS published is a KEY appearing
                // (an account a thread names that this shape did not hold) or
                // going (an unregistered account whose last thread went idle) —
                // exactly the two key-set changes the `threads` cell used to
                // drive, now driven by the bookkeeping that outlived it.
                let elsewhere: Vec<String> = tx
                    .rows
                    .iter()
                    .filter(|(k, r)| **k != key && r.placed.contains(thread_id))
                    .map(|(k, _)| k.clone())
                    .collect();
                for k in elsewhere {
                    let Some(row) = tx.rows.get_mut(&k) else {
                        continue;
                    };
                    row.placed.remove(thread_id);
                    if row.placed.is_empty() && !row.registered.now().copied().unwrap_or(false) {
                        tx.rows.remove(&k);
                    }
                }
                if state == "idle" {
                    // Only `idle` removes. Parked, retrying and queued threads
                    // stay placed — they are not free.
                    let Some(row) = tx.rows.get_mut(&key) else {
                        return;
                    };
                    row.placed.remove(thread_id);
                    if row.placed.is_empty() && !row.registered.now().copied().unwrap_or(false) {
                        tx.rows.remove(&key);
                    }
                    return;
                }
                match tx.rows.get_mut(&key) {
                    Some(row) => {
                        row.placed.insert(thread_id.clone());
                    }
                    // A key this shape did not hold: minting it IS a published
                    // change (an `added` key and its row), as it always was.
                    None => {
                        tx.rows.key(key).placed.insert(thread_id.clone());
                    }
                }
            }

            // ── the OCCURRENCE behind the level ──────────────────────────
            Input::Push(Push::Event(se)) => {
                let crate::event::ThreadEvent::AccountSwitched { from, to, reason } = &se.event
                else {
                    return;
                };
                let (Some(from_key), Some(to_key)) = (tx.resolve(from), tx.resolve(to)) else {
                    return;
                };
                if from_key == to_key {
                    return;
                }
                // **Append only when the row was OBSERVED under `from`** — the
                // critique's F8/F9 rule. Without it, a switch for a thread this
                // shape never held (idle, or already hydrated under `to`)
                // invents a move out of nothing, and `moves` is exactly the
                // cell a consumer counts.
                let observed = tx
                    .rows
                    .get(&from_key)
                    .is_some_and(|r| r.placed.contains(&se.thread_id));
                if !observed {
                    return;
                }
                let moved = tx.rows.key(from_key.clone()).placed.remove(&se.thread_id);
                tx.rows.key(from_key.clone()).moves.emit(
                    at,
                    MoveItem {
                        thread_id: se.thread_id.clone(),
                        direction: Direction::Off,
                        other: to_key.clone(),
                        reason: reason.clone(),
                    },
                );
                let dest = tx.rows.key(to_key.clone());
                if moved {
                    dest.placed.insert(se.thread_id.clone());
                }
                dest.moves.emit(
                    at,
                    MoveItem {
                        thread_id: se.thread_id.clone(),
                        direction: Direction::On,
                        other: from_key,
                        reason: reason.clone(),
                    },
                );
            }

            // ── the produced facts ───────────────────────────────────────
            Input::Fact(Fact::GaugeObserved { account, axis, row }) => {
                let Some(key) = tx.resolve(account) else {
                    return;
                };
                let observed_at = row
                    .get("observed_at")
                    .and_then(Value::as_str)
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.with_timezone(&chrono::Utc));
                let cell = tx.rows.key(key).gauge.key(axis.clone());
                match observed_at {
                    Some(o) => cell.emit_ordered(at, o, row.clone()),
                    None => cell.emit(at, row.clone()),
                }
            }
            Input::Fact(Fact::LimitRecorded {
                account,
                until,
                axis,
                prose,
            }) => {
                let Some(key) = tx.resolve(account) else {
                    return;
                };
                tx.rows.key(key).limit.emit(
                    at,
                    Some(LimitRow {
                        until: until.clone(),
                        axis: axis.clone(),
                        prose: prose.clone(),
                    }),
                );
            }
            Input::Fact(Fact::LimitCleared { account }) => {
                let Some(key) = tx.resolve(account) else {
                    return;
                };
                tx.rows.key(key).limit.emit(at, None);
            }
            Input::Fact(Fact::PoolShare { account, row }) => {
                let Some(key) = tx.resolve(account) else {
                    return;
                };
                // **A CLEARED OVERRIDE LETS GO OF A KEY NOTHING ELSE HOLDS**
                // (R2.S3g). The same rule `placed` applies when a thread goes
                // idle: an unregistered key stays only while something holds
                // it. An override row was one such thing — `aa-owned` was
                // minted by a share alone — so once `pool_share_set {share:
                // null}` deletes that row (the fact's `source` is no longer
                // `override`), a key that is not registered, has no thread
                // placed on it and is not disabled has nothing left and goes.
                // A REGISTERED key is held by registration and stays.
                let overridden = row.get("source").and_then(Value::as_str) == Some("override");
                // And a non-override fact never MINTS a key: a clear on an
                // account this shape does not hold must not create the very
                // row it exists to remove.
                if !overridden {
                    let Some(r) = tx.rows.get(&key) else {
                        return;
                    };
                    let held = r.registered.now().copied().unwrap_or(false)
                        || !r.placed.is_empty()
                        || r.disabled.now().is_some_and(|d| d.disabled);
                    if !held {
                        tx.rows.remove(&key);
                        return;
                    }
                }
                tx.rows.key(key).share.emit(at, row.clone());
            }
            // **ITS OWN ARM, and the share arm above is untouched by it.** Two
            // facts, two cells, two stamps: a disable that arrives without a
            // share fact moves only `disabled`, and a share edit on a disabled
            // account moves only `share`. That is the independence the operator
            // asked for, expressed where a consumer can see it.
            Input::Fact(Fact::AccountDisabled {
                account,
                disabled,
                disabled_at,
                note,
            }) => {
                let Some(key) = tx.resolve(account) else {
                    return;
                };
                tx.rows.key(key).disabled.emit(
                    at,
                    DisabledRow {
                        disabled: *disabled,
                        disabled_at: disabled_at.clone(),
                        note: note.clone(),
                    },
                );
            }
            Input::Fact(Fact::Pressure { account, row, as_of }) => {
                let Some(key) = tx.resolve(account) else {
                    return;
                };
                tx.rows.key(key.clone()).pressure.emit(at, row.clone());
                // R9.PR1: the same value, keyed by email.
                let email = tx.email_of_row(&key);
                tx.set_shared_pressure(at, email, EmailPressure::from_row(&key, row, as_of));
            }
            Input::Fact(Fact::LoginHealth {
                account,
                healthy,
                dead_since,
                dead_reason,
            }) => {
                let Some(key) = tx.resolve(account) else {
                    return;
                };
                tx.rows.key(key).health.emit(
                    at,
                    HealthRow {
                        healthy: *healthy,
                        dead_since: dead_since.clone(),
                        dead_reason: dead_reason.clone(),
                    },
                );
            }
            Input::Fact(Fact::CredentialChanged { account, slot }) => {
                if slot.is_machine_default {
                    // RULING 5 again, on the live path: a note, never a row.
                    let mut folds = tx.folds_into.now().cloned().unwrap_or_default();
                    match slot.folds_into.clone() {
                        Some(host) => {
                            folds.insert(slot.label.clone(), host.clone());
                            tx.set_shared_folds(at, folds);
                            tx.rows
                                .key(host)
                                .machine_default
                                .emit(at, Some(note_row(slot)));
                        }
                        None => {
                            folds.remove(&slot.label);
                            tx.set_shared_folds(at, folds);
                        }
                    }
                    // §7.1 on the LIVE path, and it is the transition that
                    // matters most: the operator signs the machine store into an
                    // account nothing hosts, or gives an unhosted one a slot.
                    // Both are exactly the moment the affordance has to appear
                    // or disappear, and both arrive here. Set from the same
                    // `slot.folds_into` the `folds` map above is set from, one
                    // line apart, so the two cannot disagree.
                    tx.set_shared_machine_login(
                        at,
                        Some(MachineLogin {
                            label: slot.label.clone(),
                            hosted_by: slot.folds_into.clone(),
                            unhosted: slot.folds_into.is_none(),
                        }),
                    );
                    return;
                }
                let _ = account;
                tx.rows
                    .key(slot.label.clone())
                    .credential
                    .emit(at, credential_row(slot));
            }
            Input::Fact(Fact::AccountRegistered { name, login_dir }) => {
                let row = tx.rows.key(name.clone());
                row.registered.emit(at, true);
                row.login_dir.emit(at, login_dir.clone());
            }
            Input::Fact(Fact::AccountRemoved { name }) => {
                let row = tx.rows.key(name.clone());
                row.registered.emit(at, false);
                // The key survives while it still holds a non-idle thread: a
                // login that left the roster with work on it is a fact the
                // board must be able to draw, and hiding it would be the
                // "reconcile rather than hide" rule broken.
                if row.placed.is_empty() {
                    tx.rows.remove(name);
                }
            }

            // **EXHAUSTIVE, WITH NO `_` ARM.** A new `Push` variant is a
            // compile error here, so ignoring it is a conscious act rather
            // than an accident.
            //
            // **AND IT BIT, AS DESIGNED, ON 2026-09-26 AND AGAIN WHEN
            // `ThreadQueue` LANDED.** The SIX thread facts below would not
            // compile until this arm named them, which is the whole value of the
            // rule: a fact about a thread is not a fact about an account, and the
            // decision to ignore it was made here rather than absorbed by a
            // wildcard.
            //
            // They are `crate::shapes::threads`'. None of the six is a fact about
            // an account: a header, a settings blob, a seen cursor and a
            // compaction marker are per-thread detail; `ThreadPlacement` is the
            // durable placement column, whose EDGE (`Status`) is what decides
            // which card a thread's row sits on, so folding the column here as
            // well would be a second writer for one placement; and
            // `ThreadQueue` carries one thread's undelivered rows, which is
            // per-thread detail of exactly the kind this shape does not keep.
            //
            // **The `Status` EDGE remains the one input this shape folds about a
            // thread**, and it carries the counts this shape draws. The queue's
            // CONTENTS are `crate::shapes::threads`' `queue` cell, per key, read
            // by whoever is looking at that thread.
            Input::Push(Push::Liveness { .. })
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
            // The link beat is `crate::shapes::heartbeat`'s alone — it is about
            // the LINK carrying frames, not about anything this shape holds.
            | Input::Fact(Fact::Beat { .. })
            // The producer's own warning count is `crate::shapes::warnings`'
            // business and nothing else's.
            | Input::Fact(Fact::WarningsSampled { .. }) => {}
        }
    }
}

fn hydrate_keyed(
    root: &mut AccountsRoot,
    as_of: Timestamp,
    h: Hydration<KeyedValue>,
    apply: impl Fn(&mut AccountRow, Timestamp, Value),
) {
    for r in h.rows() {
        apply(root.rows.key(r.account.clone()), as_of, r.value.clone());
    }
}

fn slot_state(h: &crate::accounts::SlotHealth) -> (String, Option<String>) {
    match h {
        // R9.T7: a declared keychain login with no item draws the same word;
        // the row's availability verdict is what says it is unavailable.
        crate::accounts::SlotHealth::NoLogin
        | crate::accounts::SlotHealth::NoLoginDeclared { .. } => ("no-login".into(), None),
        crate::accounts::SlotHealth::Wiped => ("wiped".into(), None),
        // RULING 6: the `of` is the whole point of the badge.
        crate::accounts::SlotHealth::CloneOf { of } => ("clone-of".into(), Some(of.clone())),
        crate::accounts::SlotHealth::Stranded => ("stranded".into(), None),
        crate::accounts::SlotHealth::Expired => ("expired".into(), None),
        crate::accounts::SlotHealth::Live => ("live".into(), None),
    }
}

pub(crate) fn credential_row(s: &crate::accounts::SlotReport) -> CredentialRow {
    let (state, clone_of) = slot_state(&s.health);
    CredentialRow {
        mode: s.credential_mode.clone(),
        subscription_type: s.subscription_type.clone(),
        access_expires_at_ms: s.access_expires_at_ms,
        refresh_expires_at_ms: s.refresh_expires_at_ms,
        access_expired: s.access_expired,
        identity: s.identity.clone(),
        secret_sha8: s.secret_sha8.clone(),
        keychain_service: s.keychain_service.clone(),
        is_live_login: s.is_live_login,
        slot_state: state,
        clone_of,
        folds_into: s.folds_into.clone(),
        live_store_expires_at_ms: s.live_store_access_expires_at_ms,
        live_store_state: s.live_store_health.as_ref().map(|h| slot_state(h).0),
    }
}

pub(crate) fn note_row(s: &crate::accounts::SlotReport) -> MachineDefaultNote {
    MachineDefaultNote {
        label: s.label.clone(),
        state: slot_state(&s.health).0,
        access_expires_at_ms: s.access_expires_at_ms,
        refresh_expires_at_ms: s.refresh_expires_at_ms,
        access_expired: s.access_expired,
        subscription_type: s.subscription_type.clone(),
        identity: s.identity.clone(),
        secret_sha8: s.secret_sha8.clone(),
        keychain_service: s.keychain_service.clone(),
    }
}

/// Re-exported for the export-shape test and the board: the wire spelling of
/// the two ops, so a test cannot assert a string the code does not produce.
pub const OP_READ: Op = Op::Read;
pub const OP_LIVE: Op = Op::Live;

/// Only for the death path's docstring reference.
pub(crate) fn _cell_state_is_three_valued(_: &CellState) {}

// ─────────────────────────────────────────────────────────────────────────────
// P12 — `impl Handles<_> for ThreadManager`, the nine hydrate handlers
// ─────────────────────────────────────────────────────────────────────────────
//
// Every one of these is SYNCHRONOUS `rusqlite` on the read pool, and they run
// on the shape task under `block_in_place` — the critique's "handler execution
// model" finding. Their sum is the lazy-init cost, paid once per
// subscriber-epoch.
//
// **Each answers through the SAME builder its live fact goes through**
// (`budgets::pool_share_row`, `budgets::gauge_axis_row`,
// `budgets::pressure_table`, `accounts::collect_slot_reports`). That is not
// tidiness: if the read path and the live path had two builders, a mirror would
// drift from a re-read and neither would be wrong on its own. One builder makes
// the drift unrepresentable.

use crate::runtime::ThreadManager;

impl Handles<PoolRoster> for ThreadManager {
    /// **THE CORRECT COUNT OF ACCOUNTS.**
    ///
    /// It answers `Complete(one row per pool entry)` — four today — and
    /// `Empty{reason}` ONLY when the pool genuinely has no entries. It can
    /// never answer `Complete(vec![])` for a populated pool, because the vector
    /// is built from `pool.entries` rather than from a filter that could
    /// silently match nothing. "4 accounts rendered as 0" is the board defect
    /// this closes, and `tests/shapes.rs` asserts the length against
    /// `pool.names().len()`.
    fn hydrate(&self, _: PoolRoster) -> Hydration<RosterRow> {
        let pool = self.pool();
        if pool.entries.is_empty() {
            return Hydration::Empty {
                reason: "pool.json has no entries and the registry names no login directories",
            };
        }
        Hydration::Complete(
            pool.entries
                .iter()
                .map(|e| RosterRow {
                    name: e.name.clone(),
                    login_dir: e.login_dir().map(str::to_string),
                })
                .collect(),
        )
    }
}

impl Handles<PoolShares> for ThreadManager {
    /// Never `Empty`: a name with no stored row is a row with `source: unset`,
    /// which is a statement ("the operator has said nothing about this
    /// account"), not an absence.
    fn hydrate(&self, _: PoolShares) -> Hydration<KeyedValue> {
        Hydration::Complete(
            self.account_pool()
                .into_iter()
                .map(|n| KeyedValue {
                    value: self.budgets.pool_share_row(&n),
                    account: n,
                })
                .collect(),
        )
    }
}

impl Handles<DisabledAccounts> for ThreadManager {
    /// **ONE read of `account_disabled` for the whole pool**, not one per
    /// account: the table is EMPTY on almost every machine and usually holds one
    /// row on the rest, so a per-account query would be N round trips to learn
    /// nothing N-1 times.
    ///
    /// Never `Empty`. An empty table is a complete answer — "no account is
    /// disabled" — and every pool member gets an explicit `disabled: false`
    /// row. That is the hydrate half of the operator's own distinction that the
    /// chassis is built around: EMPTY RENDERS AS EMPTY, NOT-HYDRATED RENDERS
    /// `?`. A handler that answered `Empty{reason}` for a pool with nothing
    /// disabled would put a `?` on every card in the ordinary case.
    ///
    /// `Partial` on a read failure, with the pool's known-good rows and the
    /// error as the reason — an unreadable table must never hydrate as "nobody
    /// is disabled", which is the CON-5 direction and would silently put an
    /// account the operator took out back onto a board as live.
    fn hydrate(&self, _: DisabledAccounts) -> Hydration<DisabledEntry> {
        let listed = match self.store.list_disabled_accounts() {
            Ok(v) => v,
            Err(e) => {
                return Hydration::Partial {
                    got: Vec::new(),
                    reason: format!("{e}"),
                }
            }
        };
        let by_name: BTreeMap<String, (String, Option<String>)> = listed
            .into_iter()
            .map(|(a, at, note)| (a, (at, note)))
            .collect();
        Hydration::Complete(
            self.account_pool()
                .into_iter()
                .map(|n| {
                    let row = match by_name.get(&n) {
                        Some((at, note)) => DisabledRow {
                            disabled: true,
                            disabled_at: Some(at.clone()),
                            note: note.clone(),
                        },
                        None => DisabledRow::default(),
                    };
                    DisabledEntry { account: n, row }
                })
                .collect(),
        )
    }
}

impl Handles<GaugeReadings> for ThreadManager {
    fn hydrate(&self, _: GaugeReadings) -> Hydration<GaugeRow> {
        let mut out = Vec::new();
        let mut failed = None;
        for n in self.account_pool() {
            for axis in crate::gauge::Axis::ALL {
                match self.budgets.gauge_axis_row(&n, axis) {
                    Ok(row) => out.push(GaugeRow {
                        account: n.clone(),
                        axis: axis.as_str().to_string(),
                        row,
                    }),
                    Err(e) => failed = Some(format!("{e}")),
                }
            }
        }
        match failed {
            Some(reason) => Hydration::Partial { got: out, reason },
            None if out.is_empty() => Hydration::Empty {
                reason: "no account has any observation yet",
            },
            None => Hydration::Complete(out),
        }
    }
}

impl Handles<AccountLimits> for ThreadManager {
    /// The `accounts.list` builder's limit columns, read through
    /// `AccountState::effective_limit_until` so a STALE window cannot leak its
    /// prose — exactly as the WS arm gates it (`server.rs:870`).
    ///
    /// **And so an ELAPSED one cannot either.** This handler is where the
    /// 2026-10-06 reading came from: `alice-example-com`'s window ended on
    /// 2 October and every hydrate after it published the window and the OAuth
    /// failure sentence the bar was written from, so the board read `barred=true`
    /// for four days. The accessor now answers `None` for an ended window
    /// (`AccountState::effective_limit_until`), and because the prose and the
    /// axis are gated on the same `Option`, all three cells go quiet together.
    fn hydrate(&self, _: AccountLimits) -> Hydration<LimitEntry> {
        let states = match self.store.list_account_states() {
            Ok(v) => v,
            Err(e) => {
                return Hydration::Partial {
                    got: Vec::new(),
                    reason: format!("{e}"),
                }
            }
        };
        let by_name: BTreeMap<String, crate::accounts::AccountState> =
            states.into_iter().map(|s| (s.name.clone(), s)).collect();
        let now = crate::store::now_rfc3339();
        Hydration::Complete(
            self.account_pool()
                .into_iter()
                .map(|n| {
                    let limit = by_name.get(&n).and_then(|s| {
                        s.effective_limit_until(&now).map(|until| LimitRow {
                            until: until.to_string(),
                            axis: s.limit_axis.clone(),
                            prose: s.last_limit_prose.clone(),
                        })
                    });
                    LimitEntry { account: n, limit }
                })
                .collect(),
        )
    }
}

impl Handles<LoginHealthRows> for ThreadManager {
    /// **ONE read, not the 265 ms N+1.** The design assumed the health columns
    /// were separable from `collect_activity`'s per-thread walk; they are —
    /// they come from `list_account_states`, which is a single statement. The
    /// walk is what costs, and the walk is not needed here.
    fn hydrate(&self, _: LoginHealthRows) -> Hydration<HealthEntry> {
        let states = match self.store.list_account_states() {
            Ok(v) => v,
            Err(e) => {
                return Hydration::Partial {
                    got: Vec::new(),
                    reason: format!("{e}"),
                }
            }
        };
        let by_name: BTreeMap<String, crate::accounts::AccountState> =
            states.into_iter().map(|s| (s.name.clone(), s)).collect();
        Hydration::Complete(
            self.account_pool()
                .into_iter()
                .map(|n| {
                    let health = by_name
                        .get(&n)
                        .map(|s| HealthRow {
                            healthy: s.healthy(),
                            dead_since: s.dead_since.clone(),
                            dead_reason: s.dead_reason.clone(),
                        })
                        .unwrap_or(HealthRow {
                            healthy: true,
                            dead_since: None,
                            dead_reason: None,
                        });
                    HealthEntry { account: n, health }
                })
                .collect(),
        )
    }
}

impl Handles<LoginSlots> for ThreadManager {
    /// The keychain read. The ONLY handler that can answer `Partial`, and the
    /// only expensive one.
    fn hydrate(&self, _: LoginSlots) -> Hydration<crate::accounts::SlotReport> {
        let Ok(home) = crate::accounts::home_dir() else {
            return Hydration::Partial {
                got: Vec::new(),
                reason: "HOME is not set, so no login store can be located".into(),
            };
        };
        let data_dir = self.data_dir();
        let mut registry = match crate::accounts::Registry::load(&data_dir.join("accounts.toml")) {
            Ok(r) => r,
            Err(e) => {
                return Hydration::Partial {
                    got: Vec::new(),
                    reason: format!("{e}"),
                }
            }
        };
        // `ensure_default` here is IN-MEMORY ONLY and is what `health_json`
        // already does: the machine default must be READ so its identity can be
        // matched, and ruling 5 is about never DRAWING it, not about never
        // looking at it. Nothing is written back to `accounts.toml`.
        registry.ensure_default();
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut reports = crate::accounts::collect_slot_reports(
            &registry,
            &data_dir,
            &home,
            now_ms,
            crate::accounts::HEALTH_WARN_BELOW_DAYS,
        );
        let pinned = crate::accounts::read_live_login_pointer(&data_dir);
        crate::accounts::mark_live_login(&mut reports, pinned.as_deref());
        if reports.is_empty() {
            return Hydration::Empty {
                reason: "no slot directories on disk",
            };
        }
        Hydration::Complete(reports)
    }
}

impl Handles<PressureRows> for ThreadManager {
    fn hydrate(&self, _: PressureRows) -> Hydration<KeyedValue> {
        let table = self
            .budgets
            .pressure_table(&self.account_pool(), chrono::Utc::now());
        // H179: the same predicate placement acts on, on the row the board draws.
        let unavailable = self.account_unavailability();
        Hydration::Complete(
            table
                .accounts
                .iter()
                .map(|r| KeyedValue {
                    account: r.account.clone(),
                    value: crate::runtime::with_availability(
                        serde_json::to_value(r).unwrap_or(Value::Null),
                        &r.account,
                        &unavailable,
                    ),
                })
                .collect(),
        )
    }
}

impl Handles<ThreadRows> for ThreadManager {
    fn hydrate(&self, _: ThreadRows) -> Hydration<ThreadEntry> {
        match self.thread_rows_non_idle() {
            // **THE CAP WAS HIT, SO THIS IS A STATEMENT ABOUT THE READ.**
            // `Complete` here would be the handler that lied — `docs/shapes.md`
            // 4.3's one failure types cannot see. Checked BEFORE the emptiness
            // test, because a truncated read of zero drawable rows is still a
            // truncated read.
            Ok(n) if n.truncated.is_some() => Hydration::Partial {
                reason: n.truncated.unwrap_or_default(),
                got: n.rows,
            },
            Ok(n) if n.rows.is_empty() => Hydration::Empty {
                reason: "no thread is running, queued, parked or retrying",
            },
            Ok(n) => Hydration::Complete(n.rows),
            Err(e) => Hydration::Partial {
                got: Vec::new(),
                reason: format!("{e}"),
            },
        }
    }
}
