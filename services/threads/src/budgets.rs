//! The BUDGETS module (§15 of the CCW-migration design doc).
//!
//! Budgets are **keyed** and **separate** (DEC-4): a thread carries an
//! OPTIONAL `budget_key`; no key means no budget; several threads may share
//! one key; keys are flat, shareable, re-pointable, and outlive their threads
//! (DEC-14). `BudgetRow.parent_key` reserves §14's nested workflow hierarchy
//! WITHOUT building it — nothing in v1 reads that column.
//!
//! ## The two things this module holds, which are not the same thing
//!
//! * A **BUDGET is a meter** (§15.2): how much a keyed consumer MAY consume.
//!   There is one meter per AXIS — session, weekly all-model and weekly fable
//!   in percent, plus **cash in dollars** — each resetting with its own window,
//!   and the budget trips when ANY of them exhausts. The percentage meters are
//!   denominated in **single-account units pooled across accounts** (DEC-5): a
//!   20% budget means 20% *of one account*, drawable from any account in the
//!   key's scope or split across them. The cash meter pools **dollars**: `$20`
//!   is $20 in total, not $20 per account.
//! * A **THRESHOLD is a gate** (§15.3): a per-thread ceiling on the same four
//!   gauges. It is not metered and not consumed. Accounts are a POOL here too
//!   (DEC-10): the thread runs on whichever account is under its thresholds,
//!   and only WAITS when none is.
//!
//! Budgets and thresholds **compose (AND)**: a thread must satisfy both.
//!
//! ## The operator's round-7 corrections, all of which live here
//!
//! 1. **Scope is explicit.** A budget key names the accounts it applies to and
//!    the crate NEVER defaults to all of them — the operating system will run
//!    different accounts for different businesses. A client may choose to send
//!    every account; that is the client's default, not this crate's.
//! 2. **Trips finish by default.** `on_trip` defaults to `finish`; `kill` is a
//!    per-budget toggle for applications that accept a torn-down mid-turn.
//! 3. **Continuity outranks throughput — but it is a CONSTRAINT ON THE FIELD,
//!    never a licence to stop placing.** An account switch restarts the
//!    conversation, so a thread with history is ranked over the accounts it
//!    already has a session on, and parks only when NONE of them is admissible.
//!    It is re-ranked on EVERY turn like everything else: *"it needs to go
//!    through placement every single time. We can't have this momentum."* The
//!    only momentum is a five-minute prompt-cache tie-break inside
//!    [`Budgets::place`], which picks WHICH account and never WHETHER.
//! 4. **Enforcement fails CLOSED.** When this module errors, nothing runs
//!    except threads whose budgeting is explicitly turned off — the caller
//!    ([`crate::runtime`]) owns that policy; everything here simply returns a
//!    real `Err` instead of a silently permissive answer.
//!
//! ## A budget key is DISABLED, never deleted (Q-34, msg #751)
//!
//! > "I don't think we should delete a budget key. I think we should disable a
//! > budget key so we can still query and see information about the budget,
//! > like how much of the budget did it spend, etc. So no deleting." … and on
//! > what a disabled key does: "whenever the budget key is used, we just deny."
//!
//! So there is no `delete` in this module, and three properties hold together:
//!
//! * **Disabled DENIES.** [`Budgets::admit`] refuses every request naming a
//!   disabled key before it looks at scope, thresholds or meters, and the turn
//!   PARKS with cause [`ParkCause::Disabled`] (CON-4: park, never drop). It is
//!   a gate, not an error — no number is fabricated to justify it.
//! * **History survives.** Disabling touches ONE column ([`BudgetConfig::disabled`],
//!   persisted as the `disabled_at` instant). Every charge, observation,
//!   estimate and meter stays exactly as readable as it was: `budgets.get` on a
//!   disabled key still reports spend to date on every axis, the per-account
//!   breakdown, calibration state and config. That is the entire point of the
//!   operator's answer.
//! * **Nothing is ever unmetered.** The hole this closed was `budgets.delete`
//!   silently removing enforcement from every thread still pointed at the key.
//!   Deletion is gone, and its last remnant — a thread naming a key that has NO
//!   config at all — now parks with [`ParkCause::UnknownKey`] instead of
//!   running free.
//!
//! ## Why a budget can be honest about not knowing
//!
//! A budget is expressed in percent, but charged in TOKENS against a limit
//! that is **continuously re-estimated** (DEC-12) — Anthropic balances limits
//! against demand, plans change, and the limit may differ per account, so
//! there is no constant to divide by. The denominator therefore moves; the
//! meter stores tokens and divides at READ time, which is exactly why
//! re-basing never double-charges.
//!
//! ## The uncalibrated refusal is SUPERSEDED by a prior (msg #1055)
//!
//! Until msg #1055, an (account, axis) with no accepted sample reported
//! [`MeterStatus::Calibrating`] and **gated nothing** — CCW's INTENT #202 rule.
//! That rule is retired. It was the honest answer while the only alternative
//! was fabricating a denominator, but it also meant every percentage budget was
//! decorative until a sample happened to land, and the operator cancelled the
//! dedicated calibration run that was going to produce one:
//!
//! > "I thought you didn't need to run a calibration because we're going to do
//! > the math before we ever test anything." (#1047) … "Either that or you can
//! > set a prior and that'll work too… we just need to have it be the most
//! > simple startup possible." (#1055)
//!
//! So the denominator is never absent now. It is a conservative PRIOR — a
//! measured lower bound on tokens-per-percent, derived and defended in
//! [`gauge`]'s "conservative PRIOR" note — shrunk towards this account's own
//! accepted samples as they accumulate ([`gauge::blend_rate`]). Under-estimating
//! the pool over-states the percent consumed, so an unmeasured percentage budget
//! refuses EARLY, never late, and it refuses from the first turn.
//!
//! Nothing is ever spent to obtain a sample. The machinery feeds entirely on
//! the overlap-window samples ordinary use already produces
//! ([`Budgets::calibrate_overlap_window`]), and the samples ride the durable
//! `limit_estimates` row, so the estimate only improves across restarts.
//!
//! [`MeterStatus::Calibrating`] survives with a NEW meaning: "gating on the
//! prior, not yet on measurement". It is a label for a reader, never a refusal.
//!
//! Thresholds, by contrast, read the observed percent directly and therefore
//! bite from the very first observation, with no estimate at all.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::accounts::Placeable;
use crate::error::{Error, Result};
use crate::gauge::{self, Axis, EffectiveReset, ResetBasis};
use crate::store::{BudgetRow, ChargeRow, ObservationRow, Store};

/// What a budget trip does to the in-flight turn.
///
/// **Default FINISH** (round-7 item 3, which SUPERSEDES DEC-13's default-kill):
/// on a trip the in-flight execution is allowed to complete, because killing
/// mid-turn can leave half-applied edits behind. `Kill` remains available as a
/// **per-budget toggle** for applications that accept that danger — the
/// operator runs loops with transaction boundaries and round-completion checks
/// and wants the option there. Either way CON-4 binds: the operator's prompt is
/// parked, never dropped, and the next turn is gated by the exhausted meter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TripAction {
    #[default]
    Finish,
    Kill,
}

impl TripAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            TripAction::Kill => "kill",
            TripAction::Finish => "finish",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "kill" => Some(TripAction::Kill),
            "finish" => Some(TripAction::Finish),
            _ => None,
        }
    }
}

/// One budget key's config. Every key carries its own (DEC-11) — there is no
/// one-shot-grant concept, so raising a limit is simply a config edit here.
///
/// Round-7 widened this in three ways: a limit on **every** axis (item 4/5 —
/// three percentage meters plus a dollar meter), an explicit **account scope**
/// (item 6), and `on_trip` defaulting to **finish** (item 3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetConfig {
    pub key: String,
    /// RESERVED (DEC-14): §14's nested workflow budgets will hang off this.
    /// v1 stores it and never reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_key: Option<String>,
    /// Percent of ONE ACCOUNT's session limit, pooled (DEC-5). `None` = this
    /// meter is unlimited.
    #[serde(default)]
    pub session_pct: Option<f64>,
    /// Percent of ONE ACCOUNT's weekly (all-models) limit, pooled.
    #[serde(default)]
    pub weekly_pct: Option<f64>,
    /// Percent of ONE ACCOUNT's weekly FABLE limit, pooled (round-7 item 4:
    /// budgets exist on all three percentage axes, not two).
    #[serde(default)]
    pub fable_pct: Option<f64>,
    /// The CASH limit in DOLLARS (round-7 item 5). Not a percentage, and
    /// POOLED across accounts: `$20` is $20 in total, not $20 per account.
    #[serde(default)]
    pub cash_usd: Option<f64>,
    /// Which accounts this key applies to (round-7 item 6). The crate NEVER
    /// defaults this to "all accounts" — the operator will run different
    /// accounts for different businesses, so scope is always explicit here.
    /// A client (AUI) may choose to pass every account; that is the client's
    /// default, not the crate's.
    #[serde(default)]
    pub accounts: Vec<String>,
    #[serde(default)]
    pub on_trip: TripAction,
    /// DISABLED (Q-34, msg #751). A disabled key denies every admission that
    /// names it and meters everything it is still charged for. Keys are never
    /// deleted, so this — not removal — is how a key is taken out of service.
    #[serde(default)]
    pub disabled: bool,
    /// RFC3339 instant the key was disabled, for the record. DERIVED: the
    /// store owns it (first disable stamps it, enabling clears it), so writing
    /// it here has no effect — set [`Self::disabled`] instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_at: Option<String>,
}

impl BudgetConfig {
    /// A new, UNSCOPED config. It cannot be persisted until `accounts` names
    /// at least one account — see [`Budgets::set_config`].
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            parent_key: None,
            session_pct: None,
            weekly_pct: None,
            fable_pct: None,
            cash_usd: None,
            accounts: Vec::new(),
            on_trip: TripAction::default(),
            disabled: false,
            disabled_at: None,
        }
    }

    /// **Can this key actually refuse or stop anything?**
    ///
    /// A key carries two separable powers, and INTENT 72b turns on keeping them
    /// separate: a key is how spend is ATTRIBUTED, and a ceiling on a key is how
    /// spend is GATED. A live key with no ceiling on any axis has the first and
    /// not the second — it meters everything charged to it and can never stop a
    /// turn. That is the shape every thread gets by default, so that forcing a
    /// budget onto the whole fleet changes ACCOUNTING rather than BEHAVIOUR.
    ///
    /// A DISABLED key does enforce: it denies every admission that names it
    /// (Q-34), which is a refusal even with no ceiling set.
    pub fn enforces(&self) -> bool {
        self.disabled || Axis::ALL.iter().any(|a| self.limit(*a).is_some())
    }

    /// The configured ceiling for one meter axis, in that axis's own unit
    /// (percent for the three percentage axes, dollars for cash).
    pub fn limit(&self, axis: Axis) -> Option<f64> {
        match axis {
            Axis::Session => self.session_pct,
            Axis::WeeklyAll => self.weekly_pct,
            Axis::WeeklyFable => self.fable_pct,
            Axis::Cash => self.cash_usd,
        }
    }

    /// The accounts this key actually applies to within `pool` — the explicit
    /// scope intersected with what is currently registered.
    ///
    /// Generic over the element type so the SAME scoping rule serves both
    /// callers without either one converting: metering passes `String` (it must
    /// cover every account this key has historically drawn on), and admission
    /// passes [`Placeable`] (it must only ever yield accounts threads owns —
    /// CON-27). Because it filters and clones rather than constructing, a
    /// `Placeable` in must mean a `Placeable` out; scoping can narrow the
    /// placement pool but can never widen it back to a bare name.
    pub fn scoped_accounts<T: AsRef<str> + Clone>(&self, pool: &[T]) -> Vec<T> {
        pool.iter()
            .filter(|a| self.accounts.iter().any(|s| s == a.as_ref()))
            .cloned()
            .collect()
    }

    fn from_row(r: BudgetRow) -> Self {
        Self {
            key: r.key,
            parent_key: r.parent_key,
            session_pct: r.session_pct,
            weekly_pct: r.weekly_pct,
            fable_pct: r.fable_pct,
            cash_usd: r.cash_usd,
            accounts: r.accounts,
            on_trip: TripAction::parse(&r.on_trip).unwrap_or_default(),
            disabled: r.disabled_at.is_some(),
            disabled_at: r.disabled_at,
        }
    }

    /// `disabled_at` is derived from the CALLER's `disabled` flag against
    /// whatever the store already holds: the first disable stamps `now`, a
    /// repeat disable keeps the original instant (so "when did we stop
    /// spending?" never moves), and enabling clears it.
    fn to_row(
        &self,
        created_at: String,
        updated_at: String,
        prior_disabled_at: Option<String>,
    ) -> BudgetRow {
        let disabled_at = if self.disabled {
            prior_disabled_at.or_else(|| Some(updated_at.clone()))
        } else {
            None
        };
        BudgetRow {
            disabled_at,
            key: self.key.clone(),
            parent_key: self.parent_key.clone(),
            session_pct: self.session_pct,
            weekly_pct: self.weekly_pct,
            fable_pct: self.fable_pct,
            cash_usd: self.cash_usd,
            accounts: self.accounts.clone(),
            on_trip: self.on_trip.as_str().to_string(),
            created_at,
            updated_at,
        }
    }
}

/// **The budget key every thread gets when it names none** (INTENT 72b: "force
/// a budget on each consumer — every single thread has to be associated with
/// the budget").
///
/// It is a key, not a ceiling. Threads carrying it are metered and attributed;
/// nothing about it can stop a turn until the operator puts a limit on it. That
/// separation is what makes forcing budgets onto 129 live threads a change to
/// accounting rather than a change to behaviour.
pub const DEFAULT_BUDGET_KEY: &str = "aui:default";

/// **The weekly THRESHOLD every thread gets when it names none — 95%.**
///
/// Not a budget. [`BudgetRow::weekly_pct`] measures OUR OWN charged tokens
/// against a ceiling we set, and on 2026-08-14 it would have caught nothing:
/// the account was at 100% in TOTAL, not 100% from us. The gauge that bites is
/// this one — [`Thresholds::weekly`] reads the raw observed percent through
/// [`Budgets::observed_value`] and gates admission on it, whoever spent it.
///
/// 95 rather than 100 so there is MARGIN. At 100 the only signal is the cliff
/// itself; at 95 a thread parks (or fails over) while the account can still
/// serve, which is the difference between a placement decision and an incident.
///
/// It is a default, not a floor: an explicit `"weekly": null` over
/// `threads.update` still ungates the thread. That is why [`Thresholds::weekly`]
/// alone among the four axes is serialized even when `None` — see the note
/// there, and the identical argument on [`crate::settings::ThreadSettings`]'s
/// `budget_key`.
pub const DEFAULT_WEEKLY_THRESHOLD_PCT: f64 = 95.0;

/// Per-thread threshold GATES (§15.3, DEC-6). A thread cannot be admitted
/// while a gauge it is gated on is above its threshold on every account in
/// the pool (DEC-10). `None` = ungated on that gauge.
///
/// Round-7 item 5: thresholds exist on EVERY usage limit, so the cash limit
/// has one too — in dollars, not percent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]

pub struct Thresholds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<f64>,
    /// §14's "60% Fable budget threshold" lives here — it is a THRESHOLD in
    /// §15's vocabulary, not a budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fable: Option<f64>,
    /// **Defaults to [`DEFAULT_WEEKLY_THRESHOLD_PCT`] (95), not to `None`.**
    ///
    /// The one gate that would have caught 2026-08-14. Every other axis stays
    /// ungated by default; this one does not, because the weekly pool is the
    /// one that actually ran out and the only prior warning of it was a poller
    /// reading nobody consumed.
    ///
    /// **The `skip_serializing_if` is deliberately ABSENT here**, exactly as it
    /// is on `ThreadSettings::budget_key`, and for the same reason: with a
    /// non-`None` serde default, an OMITTED key means "inherit 95". If a
    /// deliberate `"weekly": null` were serialized by being left out, it would
    /// read back as 95 on the next load and silently undo the operator's act.
    /// So absent means inherit, explicit `null` means ungated, and the two stay
    /// distinguishable on disk.
    ///
    /// This is also the whole backfill. Every live thread's blob carries
    /// `"thresholds":{}` — no `weekly` key — so all of them inherit 95 the
    /// moment their settings are next read. No migration, no restart.
    #[serde(default = "default_weekly_threshold")]
    pub weekly: Option<f64>,
    /// The cash gate, in DOLLARS. Inert until a cash gauge is observable —
    /// an unobserved gauge never blocks (CON-5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cash: Option<f64>,
}

/// [`DEFAULT_WEEKLY_THRESHOLD_PCT`], as a serde default.
fn default_weekly_threshold() -> Option<f64> {
    Some(DEFAULT_WEEKLY_THRESHOLD_PCT)
}

/// **Hand-written, not derived, so `Thresholds::default()` and
/// `serde_json::from_str("{}")` agree.** A derived `Default` would give
/// `weekly: None` while an empty JSON object gives 95, and every consumer that
/// builds settings in Rust (`ThreadSettings::new`, the tests, the importer)
/// would then be ungated while every consumer that came over the wire was
/// gated. One default, both doors.
impl Default for Thresholds {
    fn default() -> Self {
        Self {
            session: None,
            fable: None,
            weekly: default_weekly_threshold(),
            cash: None,
        }
    }
}

impl Thresholds {
    /// Every axis ungated — the shape a deliberate opt-OUT has, and what the
    /// crate's own tests use when they mean "no gates at all".
    pub fn ungated() -> Self {
        Self {
            session: None,
            fable: None,
            weekly: None,
            cash: None,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.configured().is_empty()
    }
    /// `(axis, threshold)` pairs that are actually configured, each in its own
    /// axis's unit.
    pub fn configured(&self) -> Vec<(Axis, f64)> {
        let mut v = Vec::new();
        if let Some(p) = self.session {
            v.push((Axis::Session, p));
        }
        if let Some(p) = self.weekly {
            v.push((Axis::WeeklyAll, p));
        }
        if let Some(p) = self.fable {
            v.push((Axis::WeeklyFable, p));
        }
        if let Some(d) = self.cash {
            v.push((Axis::Cash, d));
        }
        v
    }
}

/// Where one meter stands. `Unset` = no ceiling configured for that axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeterStatus {
    Unset,
    /// An account this key has consumed on has no accepted sample yet, so its
    /// share is priced by the conservative PRIOR rather than by measurement.
    ///
    /// REPOINTED (msg #1055). This used to mean "the meter under-reports and
    /// MUST NOT gate". It now means only "the number behind this meter is the
    /// prior" — the meter gates normally, and `Exhausted` outranks this status,
    /// so a prior-priced meter that is over its limit reports `exhausted` and
    /// trips.
    Calibrating,
    Ok,
    Exhausted,
}

impl MeterStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            MeterStatus::Unset => "unset",
            MeterStatus::Calibrating => "calibrating",
            MeterStatus::Ok => "ok",
            MeterStatus::Exhausted => "exhausted",
        }
    }
}

/// One account's slice of one meter — the account-specific half of DEC-5.
#[derive(Debug, Clone)]
pub struct AccountMeter {
    pub account: String,
    /// RFC3339 start of the window currently being metered.
    pub window_start: String,
    /// RFC3339 instant this window resets, when the gauge has told us.
    pub window_resets_at: Option<String>,
    pub tokens: u64,
    /// Dollars charged in this window (the cash axis's raw quantity; recorded
    /// on every axis's rows, but only the cash meter divides by it).
    pub dollars: f64,
    /// What 100% of this account's axis is estimated to be, in tokens.
    /// Always `None` on the cash axis — dollars need no denominator.
    pub limit_tokens: Option<f64>,
    /// This account's contribution to the pooled meter, in the axis's unit —
    /// `None` while a percentage axis is uncalibrated (never fabricated).
    pub consumed: Option<f64>,
    pub calibration_samples: u64,
    /// The latest observed raw gauge value for this (account, axis), in the
    /// axis's unit.
    pub observed: Option<f64>,
}

/// One of a budget's independent meters — one per axis (round-7 item 4/5).
/// Each resets with its OWN window and the budget trips when ANY exhausts.
#[derive(Debug, Clone)]
pub struct Meter {
    pub axis: Axis,
    /// The configured ceiling in this axis's unit, or `None` when unset.
    pub limit: Option<f64>,
    /// Consumption in this axis's unit, POOLED across the key's scoped
    /// accounts: percentage axes pool single-account units (DEC-5 — 10% on one
    /// account plus 10% on another is 20%); the cash axis pools DOLLARS ($20
    /// is $20 in total, round-7 item 5).
    pub consumed: f64,
    pub consumed_tokens: u64,
    pub consumed_usd: f64,
    pub status: MeterStatus,
    pub accounts: Vec<AccountMeter>,
}

impl Meter {
    pub fn exhausted(&self) -> bool {
        self.status == MeterStatus::Exhausted
    }

    pub fn to_json(&self) -> Value {
        json!({
            "axis": self.axis.as_str(),
            "unit": self.axis.unit().as_str(),
            "limit": self.limit,
            // Back-compatible aliases: percentage axes kept `limit_pct` /
            // `consumed_pct`, and clients read them.
            "limit_pct": (self.axis.unit() == gauge::Unit::Percent).then_some(self.limit).flatten(),
            "limit_usd": (self.axis.unit() == gauge::Unit::Dollars).then_some(self.limit).flatten(),
            "consumed": self.consumed,
            "consumed_pct": self.consumed,
            "consumed_tokens": self.consumed_tokens,
            "consumed_usd": self.consumed_usd,
            "status": self.status.as_str(),
            "accounts": self.accounts.iter().map(|a| json!({
                "account": a.account,
                "window_start": a.window_start,
                "window_resets_at": a.window_resets_at,
                "tokens": a.tokens,
                "dollars": a.dollars,
                "limit_tokens": a.limit_tokens,
                "consumed": a.consumed,
                "pct_of_account": a.consumed,
                "calibration_samples": a.calibration_samples,
                "observed": a.observed,
                "observed_pct": a.observed,
            })).collect::<Vec<_>>(),
        })
    }
}

/// A budget key's full live state — the payload `budgets.get` returns and the
/// single accounting authority AUI reads instead of keeping its own meter
/// (DEC-7).
#[derive(Debug, Clone)]
pub struct BudgetSnapshot {
    pub config: BudgetConfig,
    /// One meter per [`Axis::ALL`], in that order.
    pub meters: Vec<Meter>,
    /// The accounts actually metered — the key's explicit scope intersected
    /// with the registry, plus any account it has historically drawn on.
    pub metered_accounts: Vec<String>,
}

impl BudgetSnapshot {
    pub fn meter(&self, axis: Axis) -> &Meter {
        self.meters
            .iter()
            .find(|m| m.axis == axis)
            .expect("every axis has a meter")
    }

    /// The first axis that has tripped, if any — ANY meter trips the budget.
    fn tripped_axis(&self) -> Option<Axis> {
        self.meters.iter().find(|m| m.exhausted()).map(|m| m.axis)
    }

    pub fn to_json(&self) -> Value {
        let mut meters = serde_json::Map::new();
        for m in &self.meters {
            meters.insert(m.axis.as_str().to_string(), m.to_json());
        }
        json!({
            "key": self.config.key,
            "config": {
                "session_pct": self.config.session_pct,
                "weekly_pct": self.config.weekly_pct,
                "fable_pct": self.config.fable_pct,
                "cash_usd": self.config.cash_usd,
                "accounts": self.config.accounts,
                "on_trip": self.config.on_trip.as_str(),
                // Q-34: a disabled key is still fully reported — that is the
                // whole point of disabling instead of deleting.
                "disabled": self.config.disabled,
                "disabled_at": self.config.disabled_at,
                // Reserved, surfaced so a client can see it is inert.
                "parent_key": self.config.parent_key,
            },
            // Hoisted out of `config` as well, because it changes what every
            // number below MEANS: a disabled key denies every turn regardless
            // of how much headroom its meters show.
            "disabled": self.config.disabled,
            "disabled_at": self.config.disabled_at,
            "metered_accounts": self.metered_accounts,
            "meters": Value::Object(meters),
            // Named aliases, kept because clients already read them.
            "session": self.meter(Axis::Session).to_json(),
            "weekly": self.meter(Axis::WeeklyAll).to_json(),
            "weekly_fable": self.meter(Axis::WeeklyFable).to_json(),
            "cash": self.meter(Axis::Cash).to_json(),
            "tripped": self.tripped_axis().is_some(),
            "tripped_axis": self.tripped_axis().map(|a| a.as_str()),
        })
    }
}

/// Why a turn is parked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkCause {
    /// A threshold gate is above its ceiling on every account (DEC-6/DEC-10).
    Threshold,
    /// A budget meter is exhausted.
    Budget,
    /// No registered account falls inside this budget key's explicit scope
    /// (round-7 item 6 — the crate never widens a scope to "all accounts").
    Scope,
    /// An admissible account exists, but the thread has conversation history
    /// and none of the admissible accounts carries it. Switching would restart
    /// the conversation, so the thread WAITS (round-7 item 7 — the operator
    /// chose continuity over throughput).
    Continuity,
    /// The thread's budget key is DISABLED (Q-34, msg #751: "whenever the
    /// budget key is used, we just deny"). Unconditional — it outranks every
    /// other gate, needs no gauge and no calibration, and holds until the key
    /// is re-enabled.
    Disabled,
    /// The thread names a budget key that has NO config at all (a typo, or a
    /// client that never called `budgets.set`). It used to run UNMETERED, which
    /// was the hole `budgets.delete` drove a truck through; nothing runs on a
    /// key that cannot be metered.
    UnknownKey,
    /// The budgets module itself failed. Round-7 item 1: enforcement fails
    /// CLOSED — nothing runs but threads with `budgeting_off` — and the
    /// failure is surfaced loudly rather than silently admitted.
    Internal,
    /// No account can SERVE A TURN right now — every candidate is inside an
    /// observed usage-limit window, or is another label for a login that is
    /// ([`AdmissionRequest::unavailable`]). Distinct from `Threshold` because no
    /// gauge ceiling was crossed: this is the 429 the caller just took, reaching
    /// the gate as an input instead of being re-decided in a private selector.
    /// It clears on a reset, not on a config change.
    Unavailable,
    /// **THE SHARE CLAMP** (2026-08-27). Every account that could otherwise
    /// have taken this thread has spent past the share the operator lent the
    /// pool, so there is nowhere to PLACE it — *"it needs to be like hard
    /// clamped — not available for placement as well."*
    ///
    /// Distinct from [`ParkCause::Threshold`] on purpose, and the distinction
    /// is not cosmetic: a threshold park says "raise a ceiling", and this one
    /// says "you lent the pool 60% of that account and the pool has spent it".
    /// Also distinct from [`ParkCause::Unavailable`], which clears on a vendor
    /// reset; this clears on a reset OR on the operator lending more.
    ///
    /// It is NOT the per-account threshold veto the operator struck. Nothing
    /// here reads a threshold.
    Share,
    /// **THE DISABLE** (2026-09-08). Every account that could otherwise have taken
    /// this thread is DISABLED — the operator set its share to a true zero, which
    /// removes it from the candidate set before ranking. *"The simplest is just
    /// to be able to set a true 0% share on an account, just to disable usage on
    /// it, and then we should just not consider it for placement. It should be
    /// out of the placement selection."*
    ///
    /// **DISTINCT FROM [`ParkCause::Share`], AND THAT IS THE WHOLE POINT.** The
    /// clamp says "you lent the pool 60% of that account and the pool has spent
    /// it" — a fact about consumption, which clears on a weekly reset with no
    /// action from him. This says "you took that account out of the pool
    /// yourself", which clears when and only when he puts it back. Reporting a
    /// disable as a clamp would send him to wait for a reset that will never
    /// change anything.
    ///
    /// Distinct from [`ParkCause::Unavailable`] and from the dead-login bar for
    /// the same reason in the other direction: those are the system reporting a
    /// TEMPORARY FAILURE it observed and will retry out of. Disabling is a
    /// DELIBERATE CHOICE, it has no timer, and nothing in this crate ever lifts
    /// it.
    ///
    /// # Why `AccountDisabled` and not `Disabled`
    ///
    /// [`ParkCause::Disabled`] above was here first and means something else
    /// entirely: the thread's BUDGET KEY is disabled. Two causes named
    /// `Disabled` in one enum would be a coin flip for every future reader, and
    /// the wire strings would have collided outright. The SUBJECT is what tells
    /// them apart — a key versus an account — so the subject is in the name, and
    /// the pre-existing cause keeps the bare word and the bare wire string it
    /// has always emitted.
    AccountDisabled,
}

impl ParkCause {
    pub fn as_str(&self) -> &'static str {
        match self {
            ParkCause::Threshold => "threshold",
            ParkCause::Budget => "budget",
            ParkCause::Scope => "scope",
            ParkCause::Continuity => "continuity",
            ParkCause::Disabled => "disabled",
            ParkCause::UnknownKey => "unknown_key",
            ParkCause::Internal => "internal",
            ParkCause::Unavailable => "unavailable",
            ParkCause::Share => "share",
            ParkCause::AccountDisabled => "account_disabled",
        }
    }
}

/// The admission decision (CON-4: a block PARKS, it never drops).
#[derive(Debug, Clone, PartialEq)]
pub enum Admission {
    /// Proceed. `switch_to` names an account the thread must move to because
    /// its current one is over a threshold or outside the budget's scope
    /// (DEC-10 — "consume up to the threshold on one account, then move to
    /// another"). A switch is only ever proposed when it does NOT cost the
    /// conversation: either the thread is fresh, or it already has history on
    /// the target (round-7 item 7).
    ///
    /// [`Placeable`], not `String`: a decision to MOVE a thread is a placement,
    /// and CON-27 puts the machine default outside the set of things a
    /// placement can name.
    Admit { switch_to: Option<Placeable> },
    Park {
        cause: ParkCause,
        reason: String,
        budget_key: Option<String>,
    },
}

/// Everything the gate needs about one thread.
#[derive(Debug, Clone)]
pub struct AdmissionRequest<'a> {
    pub budget_key: Option<&'a str>,
    pub thresholds: &'a Thresholds,
    /// Every attached account — the POOL (DEC-10).
    ///
    /// **[`Placeable`], and that is CON-27's load-bearing edge.** Everything
    /// this gate can hand back as a destination is derived from this slice by
    /// filtering — scope, then thresholds and availability, then health — and
    /// filtering only ever narrows. So the machine default being unable to
    /// enter here is what makes it unable to leave through `switch_to`, on
    /// every branch, including ones written after this comment.
    pub accounts: &'a [Placeable],
    /// Where the thread is RIGHT NOW — an observation, so a bare name. It can
    /// legitimately be an account that is no longer placeable (a thread already
    /// sitting on the machine default is exactly that case), which is why it is
    /// not a [`Placeable`]: making it one would make the unplaceable thread
    /// unrepresentable instead of movable.
    pub current_account: &'a str,
    /// A pinned thread never moves accounts (§10 retry policy).
    pub pinned: bool,
    /// Accounts this thread already holds a conversation on. EMPTY means a
    /// fresh thread, which may start on any admissible in-scope account
    /// (round-7 item 7).
    pub history_accounts: &'a [String],
    /// **The cause, carried IN.** Accounts that cannot serve a turn at all
    /// right now for a reason no gauge measures: a login inside an observed
    /// usage-limit window, or another registry label for one
    /// (`accounts::unavailable_accounts` computes it).
    ///
    /// This field is why the 429 retry path has no selector of its own. The
    /// thread that just took a rate limit stamps the account's limit window and
    /// then asks THIS gate where to run; the fact it learned is an input to the
    /// one ranking that already exists, not a second ranking beside it. An
    /// account named here is never `clear`, so it is never placed on AND never
    /// counts as somewhere the thread may stay — a 429 is not a preference.
    pub unavailable: &'a [String],
    /// Accounts that must never be a placement TARGET but which a thread
    /// already sitting on one may keep using: logins proven dead
    /// (`accounts::unhealthy_accounts`).
    ///
    /// The asymmetry with `unavailable` is deliberate. A dead flag is inferred
    /// from one error string, and a turn that SUCCEEDS on the account is the
    /// only thing that clears it — move every thread off and the flag is
    /// permanent. So it filters the candidate list placement chooses from, and
    /// nothing else. (`select_failover` made exactly this distinction; it is
    /// the second half of what that function knew.)
    pub unhealthy: &'a [String],
    /// **What this thread is about to spend, and how many threads it is.** The
    /// input to the expected-deaths ranking (INTENT 83). See [`Demand`].
    pub demand: Demand,
}

/// Everything PLACEMENT needs. Placement is not gating: it answers "of the
/// accounts this thread MAY use, which one should it use?", and it has an
/// answer even for a thread with no budget key and no thresholds at all
/// (which is every AUI thread — see [`Budgets::place`]).
#[derive(Debug, Clone)]
pub struct PlacementRequest<'a> {
    /// The thread's gates. Empty is normal and fully supported: an ungated
    /// thread is ranked on raw usage against a notional full account.
    pub thresholds: &'a Thresholds,
    /// The accounts already known to be permissible — in scope, alive, and
    /// under every threshold. Placement NEVER widens this set.
    ///
    /// [`Placeable`] (CON-27). Ranking cannot conjure a candidate, so the
    /// machine default's absence from this slice is the machine default's
    /// absence from the answer.
    pub candidates: &'a [Placeable],
    /// The account the thread is on now, when it is already on one. It wins an
    /// otherwise exact tie (a placement that moves a thread for no measurable
    /// reason is pure churn) and it suppresses the round-robin cursor, which
    /// exists to spread NEW placements rather than to shuffle settled ones.
    pub prefer: Option<&'a str>,
    /// **The accounts this thread already holds a conversation on** — its
    /// `thread_accounts` rows, bare names. Empty for a fresh thread, and empty
    /// is the ordinary case for every caller that is not re-ranking a live
    /// conversation.
    ///
    /// It is a RANKING INPUT and never a filter. Nothing here adds a candidate
    /// and nothing here removes one: it decides only which of the candidates
    /// [`Budgets::admit`] already cleared may win a TIE (see
    /// [`PlacementKey::has_session`]).
    ///
    /// # Why it is not the candidate list, which is the defect it was split out of
    ///
    /// Until 2026-09-18 the re-rank branch of [`Budgets::admit`] built its
    /// candidate field as this list INTERSECTED with the placeable targets, so
    /// an account with no rows was removed before one criterion was computed for
    /// it. You needed history to be ranked and you only got history by being
    /// placed, and for a thread that already existed the loop never opened —
    /// *"if an account is added to the pool, there's a condition where it can
    /// never be used."* Passing the same fact HERE, as a tie-break, keeps
    /// everything continuity was actually buying and drops the part that was
    /// only an exclusion.
    pub sessions: &'a [String],
    /// **The objective's numerator.** What this thread is about to spend and
    /// how many threads die with it (INTENT 83). See [`Demand`].
    pub demand: Demand,
}

/// **WHAT PLACEMENT IS MINIMISING, in the operator's own terms.**
///
/// INTENT 83, `{117d80e2-7c74-4054-96f1-e113ab37c245, 2951}`: *"You're trying to minimize the number of threads that
/// died. And so you have to treat all of those sub-agents as threads. You're not
/// going to know beforehand, unfortunately, exactly how many sub-agents there
/// will be, but you can use the estimate."*
///
/// So a placement is not scored by how much budget an account has; it is scored
/// by **how many threads are expected to die there**, and every subagent counts
/// as a thread. A turn that fans out to 1,200 subagents is 1,201 deaths, not
/// one.
///
/// Both fields are ESTIMATES read off the daemon's own history
/// ([`Budgets::estimate_demand`]) and neither is ever presented as a calibrated
/// number. Only the ORDERING they induce across accounts is used, and the
/// estimate self-corrects because the ledger records what actually happened.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Demand {
    /// Expected TOKENS this thread's next turn consumes on whatever account it
    /// lands on — the whole execution, parent and subagents together, because
    /// that is how `usage_ledger` records it and how the account is billed.
    ///
    /// Exponentially weighted over the thread's own recent turns, and
    /// explicitly NOT ARIMA: the series is short, ragged and non-stationary,
    /// and the only thing riding on it is an ordering.
    pub tokens_per_turn: f64,
    /// Expected number of SUBAGENTS the turn spawns — the second half of the
    /// death count. `1.0 + subagents` threads die if the turn is cut off.
    pub subagents: f64,
}

impl Default for Demand {
    /// A thread with no history: one thread, no subagents, and a turn size of
    /// [`DEFAULT_TURN_TOKENS`]. Never zero — a demand of zero would make every
    /// account infinitely safe and delete the measurement.
    fn default() -> Self {
        Self {
            tokens_per_turn: DEFAULT_TURN_TOKENS,
            subagents: 0.0,
        }
    }
}

impl Demand {
    /// **The death count: the thread, plus every subagent, each one a thread.**
    ///
    /// Idx `2928`: *"I've had some threads that have had 1,200 sub-agents run.
    /// So it's not really one thread, is it? It's a lot of threads. Masquerading
    /// as one thread."*
    pub fn threads_at_risk(&self) -> f64 {
        1.0 + self.subagents.max(0.0)
    }

    /// Fold in **the subagents running RIGHT NOW** — the second variable the
    /// operator named (idx `2928`: *"a moving average on the number of
    /// sub-agents. And the number of sub-agents running right now"*).
    ///
    /// The max, not the sum and not a replacement. The historical average is a
    /// forecast of what the next turn will do; the live count is a fact about
    /// what this turn is already doing. Neither dominates the other in general,
    /// and taking the larger is the direction that does not under-count threads
    /// at risk — a thread that has never fanned out before but has 300 children
    /// alive at this instant is 301 threads whatever its history says.
    pub fn with_live_subagents(mut self, running: usize) -> Self {
        self.subagents = self.subagents.max(running as f64);
        self
    }

    /// **Expected number of threads that die if this turn runs against
    /// `reachable` tokens of budget** — the whole objective function.
    ///
    /// `P(death)` is the survival model in its simplest honest form: treat the
    /// work still to be done as exponentially distributed with mean
    /// `tokens_per_turn`, and the probability it outruns `reachable` tokens is
    /// `exp(-reachable / tokens_per_turn)`. It has no tunable parameter, it is
    /// monotone in both arguments, and it is right at both ends — an account
    /// with no reachable budget kills with certainty, an account with unbounded
    /// budget kills with probability zero, and a thread that spends nothing
    /// never dies anywhere.
    ///
    /// **It is never presented as a calibrated probability.** Only the ordering
    /// it induces is used, and that ordering is invariant to any monotone
    /// rescaling of either argument — which is what makes it safe to build on
    /// an estimator with a handful of samples behind it.
    pub fn expected_deaths(&self, reachable: Reach) -> f64 {
        let mean = self.tokens_per_turn.max(1.0);
        match reachable {
            // The absence of evidence. It cannot be scored, so it is ranked by
            // `place` behind every account that HAS been measured rather than
            // being given a number here.
            Reach::Unknown => f64::NAN,
            Reach::Known(tokens) => {
                let r = tokens.max(0.0);
                self.threads_at_risk() * (-r / mean).exp()
            }
        }
    }
}

/// The turn size assumed for a thread that has never run — deliberately a
/// plain, boring constant rather than a fleet average, because its only job is
/// to be non-zero and identical across the accounts being compared (it cancels
/// out of the ordering entirely when nothing is known).
pub const DEFAULT_TURN_TOKENS: f64 = 100_000.0;

/// How many recent turns the estimator looks back over. Bounded so a thread
/// with thirty thousand turns cannot turn a placement into a table scan.
const DEMAND_SAMPLES: usize = 20;

/// How fast the exponential weighting forgets: the newest sample carries this
/// share of the estimate, the one before it `(1-α)α`, and so on. At 0.4 the
/// most recent three turns hold ~78% of the weight — recent enough to follow a
/// thread that has just started fanning out, smooth enough that one anomalous
/// turn does not move the placement on its own.
const DEMAND_ALPHA: f64 = 0.4;

/// The exponentially weighted mean of `samples`, NEWEST FIRST.
///
/// **Explicitly not ARIMA** (INTENT 83's own words). There is no trend term, no
/// seasonality and no differencing, because there is nothing here to fit them
/// on: a dozen ragged samples from a non-stationary process, feeding an
/// ordering that a monotone rescaling cannot change. Weights are renormalised
/// over however many samples actually exist, so a thread with two turns behind
/// it gets an honest mean of two rather than a mean silently shrunk toward
/// zero.
fn ew_mean(samples: &[u64]) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let mut acc = 0.0;
    let mut norm = 0.0;
    let mut w = 1.0;
    for s in samples {
        acc += w * (*s as f64);
        norm += w;
        w *= 1.0 - DEMAND_ALPHA;
    }
    (norm > 0.0).then(|| acc / norm)
}

/// **THE REACHABLE BUDGET of one account: what can actually be spent on it
/// before the NEAREST limit closes, in tokens.**
///
/// The min across the quota axes, never their sum and never a lexicographic
/// walk of them: an account with a week of headroom and a nearly-closed session
/// can only serve what the session allows, so that is what it is worth. This is
/// also the whole of why placement keeps work away from an account nearing a
/// session limit — it is not a rule about session limits, it is the
/// measurement (INTENT 83, idx `2951`: *"that should just be a natural falling out of… the
/// measurement that it will hit the second limit"*).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reach {
    /// Some axis has been measured, and this is the tightest one's room, in
    /// tokens.
    Known(f64),
    /// NO quota axis has ever been read for this account. Not zero, and not
    /// unlimited — unmeasured, which ranks behind anything measured.
    Unknown,
}

/// One axis's room to spare on one account, in THAT AXIS'S OWN UNIT.
///
/// The unit never leaves the axis: two `Slack`s are only ever compared when
/// they came from the same axis, which is what stops dollars being added to
/// percentage points (see [`Budgets::place`]).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Slack {
    /// A real reading exists and this is the room it leaves.
    Known(f64),
    /// The gauge has NEVER reported this axis for this account. It is NOT
    /// zero usage and it is NOT full headroom — it is the absence of evidence,
    /// and it ranks behind any account we have actually measured.
    Unknown,
}

/// **How much of a difference in EXPECTED DEATHS is worth deciding a placement
/// on**, in threads. A hundredth of one thread.
///
/// It is stated in the objective's OWN UNITS on purpose, and that is what makes
/// the fan-out term earn its keep rather than cancelling out of the ranking.
/// The same small difference in survival probability sits below this line for a
/// lone thread — which therefore ties, stays put, and costs no churn — and far
/// above it for a thread that fans out to hundreds, where the identical
/// difference is several whole threads. One constant, one behaviour, and no
/// special case anywhere for "big threads".
///
/// Small rather than generous because the gauge noise it has to absorb is
/// already absorbed upstream: a whole percent of quota is a fifth of a nominal
/// turn, so two accounts inside the gauge's resolution score within far less
/// than this of each other anyway.
const DEATHS_DEADBAND: f64 = 0.01;
/// How much of a difference in **POOL PRESSURE** is worth deciding a placement
/// on, in multiples of the required burn rate — see
/// [`Budgets::pool_pressure`] for the quantity itself (unused share DIVIDED by
/// window time remaining, so 1.0 is exactly on pace and the scale is
/// multiplicative, not additive).
///
/// 0.15 — fifteen percent of a burn rate — because the inputs resolve coarser
/// than that near the middle of a window: the gauge reports whole percents and
/// the reset clock moves ~0.006 of a weekly window per hour, so two accounts
/// within fifteen points of the same burn rate are inside the measurement's own
/// noise and the cheaper criteria below should keep deciding them.
///
/// A RATIO needs a wider band than the difference it replaces, and that is not
/// a fudge. Pace was bounded in −1..+1 so five points was a twentieth of the
/// whole scale; pressure is unbounded above, and near a reset its denominator
/// goes small and magnifies gauge noise into large absolute swings. The band is
/// stated in the criterion's own units either way.
///
/// **The OFF SWITCH is this constant**, in exactly the terms the deadband it
/// replaces stated its own (2026-08-21: the change must be reversible; the
/// retired `PACING_DEADBAND` carried the identical clause and this criterion
/// inherits the obligation). Set it to `f64::INFINITY` and the criterion is
/// SKIPPED outright — no mode, no flag, no migration, the ranking falls
/// through to the next axis exactly as it did before pressure existed, and the
/// wave is undone by editing one number.
///
/// Skipped outright rather than merely widened, and that difference matters. A
/// very large but finite band still lets [`narrow`] drop every candidate whose
/// pressure is [`Slack::Unknown`], so a "disabled" criterion would go on
/// silently filtering the field on absence of evidence. Infinity short-circuits
/// the call, which is the only way the off switch is genuinely a no-op.
const PRESSURE_DEADBAND: f64 = 0.15;
/// How much of a difference in **LANDING CAPACITY** is worth deciding a
/// placement on, as a fraction of the room the pool was actually offered — see
/// [`Budgets::landing_capacity`].
///
/// 0.10, and it is deliberately WIDER than the pressure band below it. The cap
/// runs FIRST, so a narrow band here would let a couple of points of session
/// noise decide the placement before pressure was ever consulted, which would
/// invert the operator's stated order (veto, then cap, then rank) into one
/// where the cap silently does the ranking too.
///
/// **The OFF SWITCH is this constant**, on the same terms as
/// [`PRESSURE_DEADBAND`]: `f64::INFINITY` skips the criterion outright.
const LANDING_CAP_DEADBAND: f64 = 0.10;
/// **How recently a turn must have ended on an account for MOMENTUM to apply.**
///
/// Five minutes, and the number is not arbitrary: it is the prompt cache's own
/// lifetime. Momentum exists for exactly one reason — a warm cache makes the
/// next turn on the same account cheaper — so the moment the cache is cold the
/// criterion is measuring nothing and must stop voting.
///
/// This is what DEMOTES momentum from the near-unconditional "stay put" it used
/// to be. It decides WHICH account among those that already cleared the veto
/// and the cap, and it never decides WHETHER to place: it runs after both, and
/// it can only ever choose between candidates every gate already passed.
///
/// **The OFF SWITCH is this constant**: set it to zero and no account is ever
/// warm, so the criterion never fires.
const MOMENTUM_WINDOW_SECS: i64 = 300;
/// The share of a login offered to the pool when the operator has never said —
/// the whole thing, which is exactly what every account got before
/// `account_pool_shares` existed. This is the "no backfill, no shim" default.
const DEFAULT_POOL_SHARE: f64 = 1.0;
/// The smallest fraction of a window that may sit in [`Budgets::pool_pressure`]'s
/// DENOMINATOR.
///
/// A required burn rate divides by the time left, and time left goes to zero at
/// every reset — so without a floor the criterion produces infinities and then
/// `NaN`, and an infinity in a ranking is the same failure as a veto expressed
/// as a weight: it stops being comparable. One minute of a weekly window.
/// Pressure at the floor is already astronomically larger than any account with
/// real time left, so the floor changes no ordering; it only keeps the number
/// finite.
const PRESSURE_MIN_WINDOW_FRACTION: f64 = 60.0 / (7.0 * 24.0 * 3600.0);

// ── R9.PR1 — ONE PRESSURE FUNCTION (operator ruling, r42) ────────────────────
//
// > *"they should be the same number, and it should for sure be the amount of
// > the account tokens that are available to the pool, not the total amount on
// > the account."*
//
// `pressure = ((cap − u) / cap) / t`, `t = (weekly_reset_at − now) / 7d`,
// `cap = min(share, 0.95)`. Placement ([`Budgets::pool_pressure`]) and the
// accounts shape's per-email field (`crate::shapes::accounts`) both read the
// value [`pressure_value`] returns, through [`Budgets::pressure_reading`] — there
// is no second copy of this arithmetic anywhere in the crate.

/// The weekly cutoff pressure measures room TO: the default weekly threshold
/// (`DEFAULT_WEEKLY_THRESHOLD_PCT`), the line `admit` refuses work at. An
/// account lent its whole share (1.0) therefore has 0.95 of its week to give
/// the pool, not 1.0 (R9.PR1, executor act-on-recommendation, ledgered r42).
pub const PRESSURE_WEEKLY_CUTOFF: f64 = DEFAULT_WEEKLY_THRESHOLD_PCT / 100.0;

/// **Within this many seconds of the reset the value is ∞.** One minute — the
/// exact region the old denominator floor ([`PRESSURE_MIN_WINDOW_FRACTION`])
/// clamped, so outside the final minute no number moves because of it. Inside
/// it a ratio is noise: `resets_at` resolves to the second, the gauge polls
/// every 120 s busy, and the old floor already made the number ~10⁴× any
/// account with real time left.
pub const PRESSURE_INFINITE_WITHIN_SECS: i64 = 60;

/// **The one pressure value.** `Infinite` is a first-class answer (time is up
/// and the account still has room), not an error: JSON has no Infinity, so the
/// wire carries it as `pressure: null` + `infinite: true`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PressureValue {
    Finite(f64),
    Infinite,
}

impl PressureValue {
    /// The number, when it is one.
    pub fn finite(self) -> Option<f64> {
        match self {
            PressureValue::Finite(v) => Some(v),
            PressureValue::Infinite => None,
        }
    }

    pub fn is_infinite(self) -> bool {
        matches!(self, PressureValue::Infinite)
    }

    /// The value as the RANKER orders it. `Infinite` is `f64::MAX`: above every
    /// finite reading and still a finite `f64`, so `Slack::value` and
    /// [`narrow`]'s `best − deadband` treat it exactly as they would ∞ (only
    /// other `Infinite` accounts survive its band) without either having to
    /// special-case a non-finite number.
    fn rank_key(self) -> f64 {
        match self {
            PressureValue::Finite(v) => v,
            PressureValue::Infinite => f64::MAX,
        }
    }
}

/// `cap = min(share, PRESSURE_WEEKLY_CUTOFF)`.
pub fn pressure_cap(share: f64) -> f64 {
    share.min(PRESSURE_WEEKLY_CUTOFF)
}

/// **THE pressure function** — `((cap − u) / cap) / t`.
///
/// * `cap` — the fraction of the week the pool may spend (production passes
///   [`pressure_cap`]`(share)`; a parameter so the formula is testable alone).
/// * `used_fraction` — the weekly all-models observation as a fraction
///   (negative readings are treated as 0).
/// * `t = (reset_at − now) / 7d`, clamped to `[PRESSURE_MIN_WINDOW_FRACTION, 1]`.
///
/// `Some(Infinite)` when at most [`PRESSURE_INFINITE_WITHIN_SECS`] remain
/// (including at or past the reset) AND the account still has room under its
/// cap. With no room left (`u ≥ cap`) the clamped finite value is returned
/// instead — an over-cap account racing its reset must not become the top of a
/// ranking. `None` when the arithmetic is not a number (a zero cap), which the
/// caller reports as `not_finite`.
pub fn pressure_value(
    cap: f64,
    used_fraction: f64,
    reset_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Option<PressureValue> {
    let remaining = (reset_at - now).num_seconds();
    // The NUMERATOR: unused fraction of the cap. Deliberately unclamped below
    // zero so overage past the cap stays readable.
    let consumed = used_fraction.max(0.0);
    let unused_of_cap = (cap - consumed) / cap;
    if remaining <= PRESSURE_INFINITE_WITHIN_SECS && unused_of_cap > 0.0 {
        return Some(PressureValue::Infinite);
    }
    let window = Axis::WeeklyAll.window_secs() as f64;
    let frac_left = (remaining as f64 / window).clamp(PRESSURE_MIN_WINDOW_FRACTION, 1.0);
    let v = unused_of_cap / frac_left;
    v.is_finite().then_some(PressureValue::Finite(v))
}
/// The same, in dollars: the cash gauge resolves to the cent.
const PLACEMENT_DEADBAND_USD: f64 = 0.01;
/// How many thread rows the "fewest threads placed here" tie-break scans. Far
/// past any realistic thread count, and bounded so a runaway table cannot turn
/// admission into a table scan of unbounded size.
const PLACEMENT_THREAD_SCAN: usize = 10_000;
/// The persistent round-robin cursor's key. It lives in the watermark table
/// because that is the only general-purpose scalar the store exposes and the
/// schema is owned by another module; nothing enumerates that table, and
/// `import.rs` only ever reads sources of the form `<root>|<thread-id>`.
const PLACEMENT_CURSOR: &str = "placement:round-robin-cursor";
/// How many recent weekly-all observations the FABLE axis looks through for a
/// reset instant to borrow ([`ResetBasis::Borrowed`]). Small on purpose: the
/// borrow is legal only because the two weekly axes describe the same week, and
/// a weekly reading far enough back to be describing a different week is not a
/// thing to borrow from. The newest LIVE instant in this many rows wins.
const RESET_BORROW_SCAN: usize = 8;

/// **WHY an account has no pressure reading.** Three distinguishable causes
/// behind one [`Slack::Unknown`], named so the accounts interface can say which
/// one it is instead of guessing.
///
/// The ranker does not care — every one of these ranks behind any measured
/// account, identically. A HUMAN cares a great deal: "we have never measured
/// this account" and "the reading we have is from a window that already rolled"
/// call for different actions, and rendering both as an em-dash throws away the
/// only thing that would tell him which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnrankedReason {
    /// The gauge has never reported the weekly axis for this account. The
    /// absence of evidence — NOT zero usage, and NOT full headroom.
    NoReading,
    /// An observation exists but yields no window to measure against, on any
    /// basis (observed, borrowed or assumed).
    NoResetWindow,
    /// The window this observation belongs to has already reset and the gauge
    /// has not caught up. The reading is real but stale.
    WindowRolled,
    /// The store read failed. A ranking input lost, not a gate — the same
    /// policy `axis_slack` and `pool_pressure` already apply — but the reader
    /// is told it was a fault rather than an absence.
    ReadFailed,
    /// The arithmetic produced a non-finite value. Should be unreachable given
    /// the window floor; present because "a non-finite reading is not a
    /// reading" is enforced rather than assumed.
    NotFinite,
    /// **DISABLED.** The operator set this account's share to a true zero, so it
    /// is out of the candidate set and the pressure arithmetic never ran for it.
    ///
    /// This is the ONE reason on this list that is not a measurement problem.
    /// The other five say "we tried and could not"; this one says "we did not
    /// try, on purpose, because you told us not to". A disabled account still
    /// appears in every payload, carrying every reading it has — it is not
    /// removed from the interface, only from placement.
    ///
    /// **It DOMINATES the other five.** A disabled account with no gauge reading
    /// reports `disabled`, not `no_reading`: the reason a human needs is the one
    /// he can act on, and "no usage reading yet" would send him to poll a gauge
    /// on an account he deliberately took out of service.
    Disabled,
    /// **UNAVAILABLE — the account is inside an observed usage-limit window and
    /// cannot serve a turn at all.**
    ///
    /// Added 2026-09-20, on the operator's own report:
    ///
    /// > *"I'm seeing pressure of -49.63x on the threads dashboard for an
    /// > account that is at its limit for the week. Let's exclude it from the
    /// > scaled pressure mechanism while it's unavailable. It makes the others
    /// > not make sense because it distorts the scale so much."*
    ///
    /// **The number was not wrong; it was MEANINGLESS AND LOUD.** Pressure is
    /// `(share − consumed) ÷ share ÷ frac_left`. An account at 100% against a
    /// 0.9 share has a NEGATIVE numerator, and as its reset approaches
    /// `frac_left` shrinks toward the window floor, so the value diverges —
    /// −49.63 when he looked, −56.66 a few minutes later. Three healthy
    /// accounts sitting between 0.92 and 0.99 are indistinguishable on any
    /// relative display that also has to accommodate −56, so ONE unusable
    /// account destroyed the scale for the three usable ones.
    ///
    /// **Like `Disabled`, this is not a measurement problem.** The five
    /// measurement reasons say "we tried and could not". `Disabled` says "we did
    /// not try, because you told us not to". This one says "we cannot, until the
    /// window resets" — and it carries [`PressureReading::limit_until`] so the
    /// interface can say WHEN instead of only WHY.
    ///
    /// **Precedence: below `Disabled`, above the five.** A disabled account that
    /// also happens to be at its limit reads `disabled`, because that is the one
    /// he can act on and the limit is irrelevant to an account nothing will be
    /// placed on anyway. Above the measurement reasons for the same reason
    /// `Disabled` is: "no usage reading yet" would send him to poll a gauge on
    /// an account whose gauge is precisely what said no.
    ///
    /// **Read from `AccountState::effective_limit_until`, never from the raw
    /// column.** A `limit_until` stamped against a login the label no longer
    /// points at is not a bar — that is what stranded a thread for five and a
    /// half hours on 2026-08-02 — and the whole crate reads the effective form
    /// so that what the operator is shown and what placement believes cannot
    /// disagree. Since 2026-10-06 that form also refuses a window that has
    /// ENDED, so an elapsed bar cannot reach this reason either.
    ///
    /// # IT IS NO LONGER EVERY BAR (2026-10-06)
    ///
    /// > *"note that the threads tui should display pressure on accounts even if
    /// > they are at the session limit. They should only not display pressure if
    /// > they are at the weekly limit."*
    ///
    /// The quotation at the top of this doc is about a WEEKLY limit, and the
    /// argument generalises only as far as the cadence does. A `session` bar lifts
    /// within the five-hour pool, and its pressure is not even distorted — the
    /// number is computed from the WEEKLY axis, which a session bar says nothing
    /// about — so suppressing it threw away a reading the operator wanted and
    /// replaced it with a sentence about a wait that was nearly over.
    ///
    /// So this reason is now reached through
    /// [`crate::accounts::AccountState::limit_hides_pressure`], which carries the
    /// per-axis verdicts: everything except `session` suppresses, and `session`
    /// keeps its number and reports `unranked_reason: None`. `limit_until` and
    /// `PressureReading::unavailable` are UNCHANGED and still report the bar on
    /// every axis — the bar is still a fact, and still excludes the account from
    /// placement; only the suppression of the NUMBER asks the axis.
    ///
    /// The interface's wording for this token ("off the scale until its window
    /// resets") is correspondingly MORE accurate than it was, because it no
    /// longer fires for a session bar — but it is still wrong for the `login` and
    /// `credit` axes, where there is no window and a human has to act. The row
    /// carries `limit_axis`, so a reader can branch on it exactly as
    /// `main.rs`'s `accounts status` already does.
    Unavailable,
    /// **OVER ITS LENT SHARE, AND SETTLED — the overage is final for this
    /// window and no further turn will land here, so the number describes
    /// nothing anyone can act on.**
    ///
    /// Added 2026-09-22, on the operator's own ruling, quoted in full because
    /// the SECOND half of it is the half that keeps getting optimised away:
    ///
    /// > *"I think it's fine as is — don't clamp, just detect when weekly >
    /// > share and running == 0. The limit was passed because the last thread
    /// > to run on it didn't get evicted early, which is known behavior and not
    /// > an issue for now. But if those conditions I've stated are true you can
    /// > assume that placement will not place there again, and you can just
    /// > remove the pressure from view instead of clamping it. If it went over
    /// > I do want to see negative pressure while the last thread is still
    /// > running. That's honest."*
    ///
    /// # THE DEFECT THIS NAMES
    ///
    /// An account that spent past its share reads NEGATIVE pressure, and the
    /// magnitude grows without meaning as its reset approaches — the numerator
    /// `(share − consumed)` is fixed and negative while `frac_left` shrinks
    /// toward [`PRESSURE_MIN_WINDOW_FRACTION`]. The observed `−157.38`
    /// corresponded exactly to 427 s remaining; the floor bounds it near
    /// `−1120` and no further. Three healthy accounts sitting between 0.92 and
    /// 0.99 cannot be told apart on a display that also has to fit `−1120`.
    ///
    /// # AND THE TWO CONDITIONS, BOTH LOAD-BEARING
    ///
    /// 1. **The account is over its lent share** — `observed_pct > share × 100`.
    /// 2. **Nothing of ours is running on it** — [`Budgets::nothing_running_on`].
    ///
    /// **Condition 2 is not an optimisation and removing it is a REGRESSION.**
    /// While a thread is still executing on an over-share account the negative
    /// pressure is TRUE and the operator asked to keep seeing it: the overage
    /// exists *precisely because* that thread was not evicted early, so hiding
    /// the number would hide a live fact about a turn still burning. Only once
    /// the account is quiet does the reading stop describing anything — at that
    /// point [`Budgets::admit`] will not place there again this window, the
    /// usage ledger for the account is complete, and the number can only move
    /// by the clock running down.
    ///
    /// # WHY THIS IS VISIBILITY AND NOT A CLAMP
    ///
    /// He rejected clamping by name. A clamp keeps a number that is a BOUND
    /// presented as a MEASUREMENT, and clamped values still ORDER, so the
    /// account keeps a rank it has no business holding. Removing the reading
    /// from view says the true thing: there is no rate to report.
    /// [`PRESSURE_MIN_WINDOW_FRACTION`] stays exactly where it is — it is the
    /// floor that keeps the arithmetic finite, it is not a second clamp, and
    /// this change does not touch magnitude at all.
    ///
    /// # THE SYMMETRIC POSITIVE CASE IS DELIBERATELY LEFT ALONE
    ///
    /// An untouched account a minute from its reset reads roughly `+10,080`.
    /// That is not a lie — it is what "burn a full week's share in sixty
    /// seconds" actually evaluates to — and the operator accepted it in terms
    /// (*"cute"*). Do not "fix" it here.
    ///
    /// # Precedence: below `Disabled`, below `Unavailable`, and above the five
    /// only VACUOUSLY
    ///
    /// Below [`Self::Disabled`] for the reason `Disabled` beats everything: it
    /// is his own act and he is the only one who can reverse it. Below
    /// [`Self::Unavailable`] because a bar is the stronger statement of the two
    /// — "cannot serve a turn at all, until this instant" implies "will not be
    /// placed on again this window" — and because `unavailable` carries
    /// [`PressureReading::limit_until`], a WHEN, where this reason has only a
    /// WHY. It also beats a fact DERIVED from a reading with a fact the system
    /// RECORDED, which is the right direction.
    ///
    /// Above the five measurement reasons it never actually competes: this
    /// reason requires a weekly observation AND a live window to be decidable
    /// at all, and every one of the five is an exit taken because one of those
    /// is missing. The ordering holds by CONSTRUCTION rather than by
    /// convention, which is why it is applied at the tail rather than in the
    /// ladder at the top of [`Budgets::pressure_reading`].
    OverShareSettled,
}

impl UnrankedReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            UnrankedReason::NoReading => "no_reading",
            UnrankedReason::NoResetWindow => "no_reset_window",
            UnrankedReason::WindowRolled => "window_rolled",
            UnrankedReason::ReadFailed => "read_failed",
            UnrankedReason::NotFinite => "not_finite",
            UnrankedReason::Disabled => "disabled",
            // The same word `accounts.list` and `accounts.status` already use
            // for this state, and the same word `ParkCause::Unavailable`
            // serialises to. One state, one name, on every surface.
            UnrankedReason::Unavailable => "unavailable",
            // SETTLED, not merely over-share: the word carries the second
            // condition, and it is the crate's own word for it —
            // `Budgets::unsettled_since` already defines "settled" as "no
            // execution of ours is still in flight here, so the usage ledger
            // for this account is complete". A reader who looks that word up
            // finds the predicate.
            UnrankedReason::OverShareSettled => "over_share_settled",
        }
    }
}

/// **THE OPERATOR'S TWO ANSWERS ABOUT ONE ACCOUNT, HELD APART ON PURPOSE:**
/// *is it in the pool at all*, and *how much of it may the pool use*.
///
/// # Why it is two fields and not one number with a magic value in it
///
/// This type was born on 2026-09-08 out of a failure the operator diagnosed
/// himself:
///
/// > *"I tried changing the share to like 0.001%, but I think that just means
/// > that there's still a non-zero pressure on it… the simplest is just to be
/// > able to set a true 0% share on an account, just to disable usage on it, and
/// > then we should just not consider it for placement. It should be out of the
/// > placement selection. That's the cleanest."*
///
/// He is right, and the reason is arithmetic. [`Budgets::pool_pressure`] is a
/// RATE and the share is its DENOMINATOR — `(share − consumed) ÷ share ÷
/// frac_left`. Shrinking the share does not shrink the pressure; on an untouched
/// account it does not move it at all (the share cancels), and on a partly-used
/// one it AMPLIFIES it without bound. A 0.001% share is not "almost disabled"; it
/// is an account screaming to be drained. There is no small number that means
/// "off", which is why "off" is not a number here.
///
/// # And why, on 2026-09-20, "off" stopped being the ZERO either
///
/// The first cut expressed "off" as a `Disabled` variant reached by storing a
/// true `0.0` in `pool_shares.share`. That answered the arithmetic and kept the
/// two questions welded together in one column. The operator separated them:
///
/// > *"I want disabled to be independent of share. Whether to include it in the
/// > pool supersedes how much should be made available to the pool."*
///
/// **Supersedes** is the word that fixes the precedence: [`Self::lent`] is
/// `None` whenever the account is disabled, whatever [`Self::share`] says. And
/// independence is what makes a disable NON-DESTRUCTIVE — the share he lent
/// survives the round trip, so an enable has nothing to reconstruct. That is
/// what retired `restore_share`, `pool_shares.prior_share` and the `restore_to`
/// wire field: they existed only to remember a number the disable had destroyed.
///
/// **The cost of the old conflation is on the record.** The share-zero disable
/// shipped 2026-09-08; four days later the operator's own `pool_shares` row for
/// `alice-example-com` still read `1.0e-05` — the 0.001% he tried BEFORE the
/// feature existed. A capability behind a magic value in a number field was live
/// and he never found it.
///
/// # What survived unchanged: a zero never reaches the arithmetic
///
/// [`Self::lent`] is still the ONLY door to the `f64`, it is still `None`
/// exactly when the account is out of the pool, and every arithmetic site in
/// this file still goes through it. The guarantee is now stronger, not weaker:
/// [`Self::share`] is constructed in `(0.0, 1.0]` (the store refuses anything
/// else), so there is no zero anywhere in this type for a caller to divide by
/// even if it reached past `lent`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PoolShare {
    /// The fraction of the account lent to the pool, guaranteed in `(0.0, 1.0]`
    /// — finite, strictly positive, and therefore always safe as a denominator.
    ///
    /// **It means the same thing on a disabled account as on a live one**: this
    /// is what the operator lent, not what the pool is currently drawing. Read
    /// through [`Self::share`], and never as the answer to "is this account
    /// available" — that is [`Self::lent`].
    share: f64,
    /// **The operator took this account OUT of the pool.** Not a measurement,
    /// not a bar, not a limit: an act, with no clock on it and nothing that
    /// lifts it. Reversed only by him.
    disabled: bool,
}

impl PoolShare {
    /// The ordinary case: in the pool, at this share.
    pub fn pooled(share: f64) -> Self {
        Self { share, disabled: false }
    }

    /// Out of the pool, still worth `share` when he puts it back. The share is
    /// carried rather than dropped precisely so that putting it back is not a
    /// question anybody has to answer.
    fn withheld(share: f64) -> Self {
        Self { share, disabled: true }
    }

    /// **The number the arithmetic may use, or nothing.** `None` is the disabled
    /// account, and it is the only door to a usable `f64` — which is what makes
    /// "a disabled account never reaches a denominator" a property of the type
    /// rather than a rule somebody has to remember at four call sites.
    ///
    /// This is deliberately NOT `Some` with a zero for a disabled account, and
    /// deliberately not the share either: a disabled account is not "available
    /// at 0.6", it is unavailable, and a caller that wants the quantity anyway
    /// has to say so by name ([`Self::share`]).
    pub fn lent(self) -> Option<f64> {
        (!self.disabled).then_some(self.share)
    }

    pub fn is_disabled(self) -> bool {
        self.disabled
    }

    /// **WHAT THE OPERATOR LENT — the quantity, independent of the inclusion
    /// decision, and always a real share in `(0.0, 1.0]`.**
    ///
    /// This is what every human-facing surface prints, for a disabled account as
    /// much as for a live one. It used to be `for_display`, and it used to
    /// answer `0.0` for a disabled account because the zero WAS the disable and
    /// hiding it would have hidden the state. Independence made both halves of
    /// that wrong: there is no zero to show, and the state has its own boolean
    /// beside this number on every surface that carries it.
    ///
    /// Still never a denominator. [`Self::lent`] is the accessor every
    /// arithmetic site uses, and the split is the entire reason this type exists.
    pub fn share(self) -> f64 {
        self.share
    }
}

/// **The full pressure reading for one account** — what the ranker collapses to
/// a single comparable number, before it collapses it.
///
/// `pressure` is `None` exactly when the account is UNRANKED, and
/// `unranked_reason` is `Some` exactly then; the two are maintained together by
/// [`Budgets::pressure_reading`] and asserted together by
/// `tests/accounts_pressure.rs`.
/// **One row of the pressure table, typed.**
///
/// Field ORDER here is irrelevant to the wire (both surfaces go through
/// `serde_json::to_value`, which sorts), but field NAMES and value types are
/// the contract: they are what `accounts.pressure`, `threads accounts pressure
/// --json` and `Fact::Pressure` all carry.
#[derive(Debug, Clone, Serialize)]
pub struct PressureRow {
    pub account: String,
    pub ranked: bool,
    pub pressure: Option<f64>,
    /// **The value is ∞** (R9.PR1): within `PRESSURE_INFINITE_WITHIN_SECS` of
    /// the weekly reset with room still under the cap. JSON has no Infinity, so
    /// `pressure` is `null` and THIS is the flag; `ranked` stays `true`.
    /// Additive on the wire.
    pub infinite: bool,
    pub rank: Option<usize>,
    pub unranked_reason: Option<&'static str>,
    /// The operator took this account out of the pool. INDEPENDENT of `share`
    /// since 2026-09-20 — a disabled account carries the share it was lent,
    /// so a client must branch on this and never on `share == 0`, which cannot
    /// happen any more.
    pub disabled: bool,
    /// Inside an observed usage-limit window. Its own field beside `disabled`
    /// because they are different facts with different remedies, and a client
    /// that collapsed them would offer "enable this account" for a bar that no
    /// enable can lift.
    pub unavailable: bool,
    /// When the bar lifts, or `null`. Additive on the wire (2026-09-20).
    pub limit_until: Option<String>,
    /// **This account has spent past the share it was lent** —
    /// `observed_pct > share × 100`. Additive on the wire (2026-09-22).
    ///
    /// Its own boolean for the same reason `disabled` and `unavailable` are:
    /// so no client has to reconstruct the comparison from two floats and get
    /// the boundary wrong. It is the FACT, and it is reported in BOTH of the
    /// two cases that fact produces — the row that still carries a negative
    /// `pressure` because a thread is still running there, and the row whose
    /// `pressure` is `null` with `unranked_reason: "over_share_settled"`
    /// because nothing is. Which of the two this row is, is exactly what
    /// `ranked` and `unranked_reason` already say; this field does not repeat
    /// them.
    pub over_share: bool,
    pub share: f64,
    /// Repeated per row as well as at the top of the table. That is the
    /// pre-existing wire shape and it is kept deliberately: a row handed to a
    /// consumer on its own (which is exactly what `Fact::Pressure` does) still
    /// says which axis it is about.
    pub axis: &'static str,
    pub observed_pct: Option<f64>,
    pub observed_at: Option<String>,
    pub resets_at: Option<String>,
    pub reset_basis: Option<&'static str>,
}

/// The whole table, as `accounts.pressure` answers it.
#[derive(Debug, Clone, Serialize)]
pub struct PressureTable {
    pub axis: &'static str,
    pub now: String,
    pub deadband: f64,
    pub ranked_count: usize,
    pub accounts: Vec<PressureRow>,
}

#[derive(Debug, Clone)]
pub struct PressureReading {
    pub account: String,
    /// **THE value** ([`pressure_value`], R9.PR1) — what placement ranks and what
    /// the accounts shape publishes per email. `None` = UNRANKED.
    pub value: Option<PressureValue>,
    /// The required burn rate as a number: `value`'s finite projection. `None`
    /// when UNRANKED, which is not zero — or when `value` is `Infinite`, which
    /// JSON cannot carry as a number (see `PressureRow::infinite`).
    pub pressure: Option<f64>,
    /// Set iff `pressure` is `None`.
    pub unranked_reason: Option<UnrankedReason>,
    /// The share the pressure is divided by — the denominator, so a reader can
    /// see why a 40%-consumed account on a 0.5 share reads as hot.
    ///
    /// **ALWAYS A REAL SHARE IN `(0.0, 1.0]`, on a disabled account too.**
    /// It used to be able to read `0.0`, which meant DISABLED; since 2026-09-20
    /// disabled is [`Self::disabled`]'s job alone and this is only ever the
    /// quantity the operator lent. A disabled account therefore reports its real
    /// share here with `pressure: null` beside it — the number is what he lent,
    /// the null is that nothing is being ranked against it.
    pub share: f64,
    /// **The operator DISABLED this account.** Reported as its own boolean rather
    /// than left to be inferred from `share == 0.0`, so no client has to compare
    /// a float against zero to find out whether an account is in the pool.
    pub disabled: bool,
    /// **THE ACCOUNT IS INSIDE AN OBSERVED USAGE-LIMIT WINDOW** and cannot serve
    /// a turn. A THIRD question, and deliberately not merged with either of the
    /// other two: membership (`disabled`), allocation (`share`) and availability
    /// (this) are three different facts with three different remedies — reverse
    /// it, change it, or wait for it.
    pub unavailable: bool,
    /// When the bar lifts, or `None`. From
    /// [`crate::accounts::AccountState::effective_limit_until`], so a window
    /// stamped against a login this label no longer points at is not reported as
    /// a bar. Carried so a reader can say *"at its limit until 00:59"* rather
    /// than only *"unranked"*.
    pub limit_until: Option<String>,
    /// **THE ACCOUNT HAS SPENT PAST ITS LENT SHARE** — `observed_pct > share ×
    /// 100`, on the weekly all-models axis, which is the only axis pressure is
    /// measured on.
    ///
    /// `false` whenever there is no observation to decide it with: this is a
    /// POSITIVE claim about a reading, never a default. It is the FIRST of the
    /// two conditions behind [`UnrankedReason::OverShareSettled`] and it is
    /// reported on its own because it stays true, and stays interesting, in the
    /// case where the reading is KEPT — an over-share account with a thread
    /// still executing on it, which carries its real negative pressure.
    pub over_share: bool,
    pub observed_pct: Option<f64>,
    pub observed_at: Option<String>,
    pub resets_at: Option<String>,
    pub reset_basis: Option<crate::gauge::ResetBasis>,
}

impl PressureReading {
    /// **THE TWO STANDING EXCLUSIONS DOMINATE every reason there is no number.**
    ///
    /// Every exit from [`Budgets::pressure_reading`] goes through this, which is
    /// why a disabled or limit-barred account can never be reported as
    /// `no_reading` or `window_rolled` — reasons that describe a measurement
    /// problem and would send the operator to fix a gauge on an account he
    /// switched off on purpose, or on one whose gauge is exactly what said no.
    /// The READINGS gathered on the way here are KEPT: these exclusions take an
    /// account out of RANKING, not out of the interface.
    ///
    /// The argument is an `Option<UnrankedReason>` rather than two booleans so
    /// the PRECEDENCE (`Disabled` over `Unavailable` over the five) is decided
    /// exactly once, at the top of `pressure_reading`, and cannot be re-decided
    /// differently at one of the six exits.
    fn excluded_over(mut self, excluded: Option<UnrankedReason>) -> Self {
        if let Some(reason) = excluded {
            self.value = None;
            self.pressure = None;
            self.unranked_reason = Some(reason);
        }
        self
    }
}

impl Slack {
    /// The reading, when there is one that can be compared at all.
    fn value(self) -> Option<f64> {
        match self {
            Slack::Known(v) if v.is_finite() => Some(v),
            // A non-finite reading is not a reading.
            Slack::Known(_) | Slack::Unknown => None,
        }
    }
}

/// Parse an optional RFC3339 stamp into UTC. `None` for absent or unparseable
/// — the two are the same thing to every caller here, since neither yields an
/// instant to rank on.
fn parse_rfc3339_utc(s: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Narrow the field by ONE axis, in that axis's own unit.
///
/// Deliberately a filter rather than a comparator. "Within a deadband is a
/// tie" is not transitive — a≈b and b≈c does not give a≈c — so as a pairwise
/// `sort_by` it is not an ordering at all and Rust is entitled to panic on it.
/// Measured against the leader it is well defined: everything within
/// `deadband` of the best reading stays in contention and the next criterion
/// decides between them.
///
/// An account with no reading on this axis is dropped whenever any other
/// candidate has one — absence of evidence is not evidence of room. When
/// NOBODY has a reading the axis is silent and the field is untouched.
fn narrow(front: &mut Vec<PlacementKey>, deadband: f64, slack: impl Fn(&PlacementKey) -> Slack) {
    let best = front
        .iter()
        .filter_map(|k| slack(k).value())
        .fold(None::<f64>, |acc, v| Some(acc.map_or(v, |a| a.max(v))));
    let Some(best) = best else { return };
    front.retain(|k| slack(k).value().is_some_and(|v| v >= best - deadband));
}

/// [`narrow`], unless the criterion has been SWITCHED OFF.
///
/// An infinite deadband disables the criterion outright: the call is skipped
/// and the field is handed on untouched. This is the reversibility requirement
/// the operator set, and it is a skip rather than a very wide band for one
/// concrete reason — [`narrow`] does not only filter on the deadband, it also
/// drops every candidate whose reading is [`Slack::Unknown`]. A criterion
/// "disabled" by widening its band would go on silently filtering the field on
/// absence of evidence, which is not off. This is off.
fn narrow_unless_disabled(
    front: &mut Vec<PlacementKey>,
    deadband: f64,
    slack: impl Fn(&PlacementKey) -> Slack,
) {
    if !deadband.is_finite() {
        return;
    }
    narrow(front, deadband, slack);
}

/// The same filter for a criterion where SMALL is good — expected deaths.
///
/// `NaN` is the unmeasured account and never compares true, so it drops out
/// whenever anyone has a real score and survives untouched when nobody does,
/// which is the same "absence of evidence is not evidence of room" rule
/// [`narrow`] applies.
fn narrow_min(front: &mut Vec<PlacementKey>, deadband: f64, score: impl Fn(&PlacementKey) -> f64) {
    let best = front
        .iter()
        .map(&score)
        .filter(|v| v.is_finite())
        .fold(None::<f64>, |acc, v| Some(acc.map_or(v, |a: f64| a.min(v))));
    let Some(best) = best else { return };
    front.retain(|k| {
        let v = score(k);
        v.is_finite() && v <= best + deadband
    });
}

/// One candidate's placement inputs. They are applied in order, one at a
/// time, and never combined into a single number — the whole defect being
/// fixed here is that summing them made a dollar worth a percentage point.
#[derive(Debug, Clone)]
struct PlacementKey {
    /// **The objective**: how many threads — this one plus its estimated
    /// subagents — are expected to die if the turn runs here. `NaN` when this
    /// account has never been measured on any quota axis, which ranks it behind
    /// every account that has.
    deaths: f64,
    /// **THE CAP**: the graded landing capacity
    /// ([`Budgets::landing_capacity`]) — how much of the session room the pool
    /// was actually offered is still free, as a fraction of that room. Applied
    /// SECOND, after the objective and before the rank, because the operator's
    /// order is veto, then cap, then rank. A capacity, not a bit: it falls
    /// smoothly to zero at the line the veto enforces absolutely.
    cap: Slack,
    /// **THE RANK**: the required burn rate ([`Budgets::pool_pressure`]) —
    /// unused fraction of the POOL SHARE divided by the fraction of the window
    /// still to run. 1.0 is on pace, larger must burn faster or lose the
    /// remainder to its reset, negative has already spent past its share.
    /// Ranked after the cap and never mixed into it, silent when no usable
    /// window has been observed.
    pressure: Slack,
    /// Room on the cash axis, in DOLLARS, ranked strictly after the objective
    /// and never mixed into it.
    cash: Slack,
    /// How many threads are already placed here (the thread being placed does
    /// not count against its own current account).
    placed: usize,
    /// **MOMENTUM**: true for the account the thread is already on AND whose
    /// prompt cache is still warm ([`Budgets::momentum_is_warm`]). A tiebreak
    /// about COST, applied after the veto and the cap have both had their say,
    /// so it can only ever choose among accounts that already cleared them.
    momentum: bool,
    /// True for the account the thread is already on, warm cache or not — the
    /// ANTI-CHURN tie-break, and a different criterion from [`Self::momentum`]
    /// above it despite the overlapping condition. Momentum is about the cost
    /// of a cold cache and expires; this is about not moving a thread when
    /// every measurable criterion has already tied, and never expires.
    is_current: bool,
    /// **CONTINUITY**: does this thread already hold a conversation on this
    /// account ([`PlacementRequest::sessions`])? The FIRST of the tie-breaks,
    /// ahead of spread, and it is where continuity lives now that it is no
    /// longer a filter over the candidate list.
    ///
    /// It can only ever decide a field in which every measured criterion —
    /// deaths, the cap, pressure, cash — has already tied inside its own
    /// deadband. An account the thread has never run on therefore wins only by
    /// BEATING every session-bearing candidate on something measurable, never
    /// by edging one out on fairness or on the rotation cursor.
    has_session: bool,
    /// The round-robin offset, so repeated NEW placements alternate rather
    /// than all landing on the same name.
    rotation: usize,
    /// The candidate itself. [`Placeable`] rather than `String` so the winner
    /// of the ranking IS the value handed back — the answer never passes
    /// through a bare name where a different one could be substituted (CON-27).
    name: Placeable,
}

/// A tripped budget, for the mid-turn enforcement path.
#[derive(Debug, Clone)]
pub struct Trip {
    pub budget_key: String,
    pub axis: Axis,
    /// Consumption and ceiling in the tripped axis's own unit.
    pub consumed: f64,
    pub limit: f64,
    pub action: TripAction,
}

impl Trip {
    /// One-line human reason, unit-aware.
    ///
    /// Values print at their REAL precision — see [`exact`]. Fixed-width
    /// rounding turned a live trip into "cash meter at $0.01 of its $0.00
    /// limit" (observed 2026-07-29 against a $0.005 limit), which reads as a
    /// meter tripping against nothing.
    pub fn reason(&self) -> String {
        match self.axis.unit() {
            gauge::Unit::Dollars => format!(
                "budget {} exhausted: {} meter at ${} of its ${} limit",
                self.budget_key,
                self.axis.as_str(),
                exact(self.consumed),
                exact(self.limit)
            ),
            gauge::Unit::Percent => format!(
                "budget {} exhausted: {} meter at {}% of its {}% limit",
                self.budget_key,
                self.axis.as_str(),
                exact(self.consumed),
                exact(self.limit)
            ),
        }
    }
}

/// Render a configured or measured value at the precision it actually has.
///
/// Every human-readable budget line goes through this. Fixed-width formatting
/// was actively misleading on both sides of the comparison: `{:.0}%` printed a
/// 3.5% threshold as `4%` (so the gate looked off by one) and `${:.2}` printed
/// a $0.005 limit as `$0.00` (so a real trip looked like it fired against a
/// zero limit). Trailing zeros are trimmed so a plain `4%` still prints as
/// `4%`, not `4.000000%`.
fn exact(v: f64) -> String {
    if !v.is_finite() {
        return v.to_string();
    }
    // Six decimals is past any limit a human configures and past the
    // sub-cent resolution of the cash axis; the trim keeps round values round.
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    // A value that rounds to nothing prints as plain `0` — never `-0`, and
    // never an empty string.
    if s.is_empty() || s == "-" || s == "-0" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

/// The outcome of one calibration attempt against the NEWEST overlap window
/// (`Budgets::calibrate_overlap_window`) — surfaced so tests (and, if ever
/// needed, a live diagnostic) can see WHY a poll did or did not extend
/// calibration, without the crate ever fabricating a sample.
///
/// `#[allow(dead_code)]`: today's tests only match the discriminant (and
/// sometimes `reason`), but every field is genuine diagnostic payload a
/// future live-status surface would want — not vestigial.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub(crate) enum WindowVerdict {
    /// A trustworthy sample was recorded.
    Sampled {
        tokens_per_percent: f64,
        window: gauge::OverlapWindow,
        tokens: u64,
    },
    /// A complete, bracketed window existed but was refused — and why.
    Rejected {
        reason: gauge::WindowRejection,
        window: gauge::OverlapWindow,
        tokens: u64,
    },
    /// No window exists yet to evaluate (too few polls, or nothing bracketed).
    NoWindow,
}

/// The budgets service: all budget/threshold/gauge state, over the store.
/// Cheap to clone (the store is an `Arc`).
///
/// # There is no `POINTER_LABEL` here any more
///
/// A `pub const POINTER_LABEL: &str = "default"` used to live at this spot,
/// with one reader — a guard in [`Budgets::record_period_estimate`] refusing to
/// record under it. It is DELETED, and not because the invariant relaxed. It is
/// deleted because it was this module knowing the concept of a default account,
/// which the operator has ruled against: *"I do not want default even in the
/// purview of the system. Why does it even see that there is a default?"*
///
/// The invariant it protected now holds by construction and over ALL SIX
/// writers rather than one: every account this module is handed comes from
/// [`crate::pool::Pool`], the machine default is not a pool member, so there is
/// nothing named `default` to record under. The audit that found the old guard
/// covered `limit_estimate_periods` and none of `usage_observations`,
/// `limit_estimates`, `accounts`, `budget_configs` or `usage_ledger` is exactly
/// why a name check was the wrong shape for it.
#[derive(Clone)]
pub struct Budgets {
    store: Store,
    /// The pool, for the DECLARED share on an entry. Empty unless
    /// [`Budgets::with_pool`] or [`Budgets::with_pool_handle`] was called — a
    /// store-only `Budgets` resolves shares from `pool_shares` alone, which is
    /// what every unit test wants and what the crate did before `pool.json`
    /// existed.
    ///
    /// **A CELL, NOT AN `Arc<Pool>`, AND IT IS THE MANAGER'S OWN CELL.** This
    /// held a snapshot taken once at construction, which was correct for exactly
    /// as long as the manager's pool was also a boot-time constant. It is not:
    /// the roster reloads on a rename, a delete and a new account (see
    /// [`crate::runtime::ThreadManager::reload_pool`]). A private snapshot here
    /// would keep answering with the old blob after a reload, so
    /// `accounts.pool_share.get` and the ranking's share clamp would read a
    /// DECLARED share for an entry that no longer exists — or miss one for an
    /// entry that just arrived. Sharing the cell is what makes "one pool" true.
    pool: crate::pool::PoolCell,
}

impl Budgets {
    pub fn new(store: Store) -> Self {
        Self {
            store,
            pool: crate::pool::cell(crate::pool::Pool::default()),
        }
    }

    /// Attach a pool this `Budgets` OWNS, so [`Self::pool_share`] can see a
    /// share DECLARED in `pool.json` as well as one overridden in the store.
    ///
    /// For a holder with nobody to share a cell with — a CLI verb that loaded
    /// the blob itself, a unit test that built one inline. It wraps the pool in
    /// a private cell, which means it never reloads; that is correct for a
    /// process that reads the blob once and exits, and wrong for the daemon,
    /// which uses [`Self::with_pool_handle`] instead.
    pub fn with_pool(mut self, pool: Arc<crate::pool::Pool>) -> Self {
        self.pool = Arc::new(std::sync::RwLock::new(pool));
        self
    }

    /// **Attach the manager's OWN pool cell**, so a reload reaches this module
    /// too. The daemon's path; see the field's docs for why sharing rather than
    /// snapshotting is the whole point.
    pub fn with_pool_handle(mut self, pool: crate::pool::PoolCell) -> Self {
        self.pool = pool;
        self
    }

    /// The pool as it stands right now. A snapshot per call — never cached in a
    /// field, which is the defect [`Self::with_pool_handle`] exists to fix.
    fn pool(&self) -> Arc<crate::pool::Pool> {
        crate::pool::snapshot(&self.pool)
    }

    /// The store this module accounts against. For CLI verbs that already hold
    /// a `Budgets` and need the ONE writer next to the reader — `pool-share`
    /// sets the row and reads the effective value back through the same builder
    /// the WS uses, and opening a second `Store` to do it would let the two
    /// halves of one command see different snapshots.
    pub fn store_ref(&self) -> &Store {
        &self.store
    }

    // ── config (mutable at runtime over the WS protocol, DEC-11) ─────────

    pub fn get_config(&self, key: &str) -> Result<Option<BudgetConfig>> {
        Ok(self.store.get_budget(key)?.map(BudgetConfig::from_row))
    }

    pub fn list_configs(&self) -> Result<Vec<BudgetConfig>> {
        Ok(self
            .store
            .list_budgets()?
            .into_iter()
            .map(BudgetConfig::from_row)
            .collect())
    }

    /// Create or update a key's config. Keys outlive their threads (DEC-14),
    /// so this is also how a key is minted.
    ///
    /// VALIDATES before writing (round-7 item 2 — hardening): the account scope
    /// must be explicit and non-empty, and every limit must be a finite,
    /// non-negative number. A config that cannot be enforced honestly is
    /// refused here rather than blowing up inside the admission gate later.
    pub fn set_config(&self, cfg: &BudgetConfig) -> Result<()> {
        Self::validate_config(cfg)?;
        let now = crate::store::now_rfc3339();
        let prior = self.store.get_budget(&cfg.key)?;
        let created = prior
            .as_ref()
            .map(|r| r.created_at.clone())
            .unwrap_or_else(|| now.clone());
        let prior_disabled_at = prior.and_then(|r| r.disabled_at);
        self.store
            .upsert_budget(&cfg.to_row(created, now, prior_disabled_at))
    }

    /// The rules a persisted budget config must satisfy.
    fn validate_config(cfg: &BudgetConfig) -> Result<()> {
        if cfg.key.trim().is_empty() {
            return Err(Error::Config("a budget key may not be empty".into()));
        }
        // Round-7 item 6. The crate refuses to guess a scope: the operating
        // system will run different accounts for different businesses, so
        // "all accounts" must be a decision a CLIENT makes and sends.
        if cfg.accounts.is_empty() {
            return Err(Error::Config(format!(
                "budget {} must name the accounts it applies to: the crate does \
                 not default to all accounts (pass `accounts: [...]`)",
                cfg.key
            )));
        }
        if let Some(dup) = cfg
            .accounts
            .iter()
            .enumerate()
            .find(|(i, a)| cfg.accounts[..*i].contains(a))
        {
            return Err(Error::Config(format!(
                "budget {}: account {:?} is listed twice",
                cfg.key, dup.1
            )));
        }
        for axis in Axis::ALL {
            if let Some(v) = cfg.limit(axis) {
                if !v.is_finite() || v < 0.0 {
                    return Err(Error::Config(format!(
                        "budget {}: {} limit must be a finite, non-negative number (got {v})",
                        cfg.key,
                        axis.as_str()
                    )));
                }
            }
        }
        Ok(())
    }

    /// DISABLE or re-enable a key (Q-34, msg #751). **This replaces deletion,
    /// which no longer exists anywhere in the crate.**
    ///
    /// Disabling is a GATE, not a demolition: the config, the charge ledger,
    /// the meters, the calibration state and every thread pointer stay exactly
    /// where they were and stay queryable — while [`Budgets::admit`] denies
    /// every turn that names the key, so no spend can be added to it either.
    /// Re-enabling is the exact inverse and unparks the waiting work (the
    /// caller's resume sweep re-runs the same gate).
    ///
    /// Idempotent in both directions. Returns the resulting config, or
    /// `Err` when there is no such key — silently "disabling" a key that does
    /// not exist would be the one outcome that looks like success and enforces
    /// nothing.
    pub fn set_disabled(&self, key: &str, disabled: bool) -> Result<BudgetConfig> {
        let Some(row) = self.store.get_budget(key)? else {
            return Err(Error::Config(format!("no such budget key: {key}")));
        };
        // A repeat disable must not move the instant it was first disabled at.
        let stamp = match (disabled, row.disabled_at.clone()) {
            (true, Some(prior)) => Some(prior),
            (true, None) => Some(crate::store::now_rfc3339()),
            (false, _) => None,
        };
        self.store.set_budget_disabled(key, stamp.as_deref())?;
        self.get_config(key)?
            .ok_or_else(|| Error::Config(format!("no such budget key: {key}")))
    }

    pub fn any_configured(&self) -> Result<bool> {
        self.store.any_budget_configured()
    }

    /// Does ANY key on the box carry a ceiling (or a denial) that could stop a
    /// turn? See [`BudgetConfig::enforces`].
    ///
    /// Distinct from [`Self::any_configured`], and the distinction became
    /// load-bearing the moment every thread was given a default key. "A budget
    /// exists" stopped being evidence that anything is gated, and the gauge
    /// pollers ask the second question, not the first: polling `/usage` costs
    /// real requests, so it must be driven by what is actually ENFORCED rather
    /// than by the mere existence of an attribution key on every thread.
    pub fn any_enforcing(&self) -> Result<bool> {
        Ok(self.list_configs()?.iter().any(|c| c.enforces()))
    }

    /// **Make sure the fleet-wide default budget exists AND that its scope is
    /// the pool this daemon actually booted with** (INTENT 72b).
    ///
    /// # CALLED AT BOOT **AND ON EVERY POOL RELOAD** — and the second half of
    /// that sentence is newer than the first
    ///
    /// This shipped called from `ThreadManager::recover` alone, on a premise
    /// stated in these docs and true when it was written: *"`self.pool` is built
    /// at construction and is read-only for the process's life, so 'the pool as
    /// of boot' and 'the pool' are the same fact."* [`crate::pool::PoolCell`]
    /// retired that premise. The roster now re-derives from disk on
    /// `accounts.rename`, `accounts.delete`, `accounts.login.complete` and once
    /// per watcher tick, and each of those can ADD a name.
    ///
    /// A boot-only reconciliation under a reloadable pool reproduces the exact
    /// defect described below, just with a shorter fuse: the account joins the
    /// roster, gets fingerprinted, gets pressure-ranked, gets polled — and is
    /// still cut by [`Self::admit`]'s first filter for every thread carrying
    /// this key, until a restart. So the rule is *the scope follows the pool*,
    /// and the one writer of the pool
    /// ([`crate::runtime::ThreadManager::reload_pool`]) calls this on the
    /// changed path. It is idempotent and cheap on the ordinary path — one row
    /// read, and a write only when a name is genuinely missing.
    ///
    /// Three properties are deliberate:
    ///
    /// * **It never overwrites a CEILING.** If the key already exists —
    ///   including after the operator has put real limits on it — every axis,
    ///   the trip action and the disable are left exactly as they are. Boot
    ///   must never quietly undo a limit he set.
    /// * **The account scope is explicit at this call site.** The crate never
    ///   defaults a budget to "all accounts" (see [`Self::validate_config`]),
    ///   because different accounts may run different businesses. The DEFAULT
    ///   key is the one case where every registered account is the right scope,
    ///   and naming that here — rather than relaxing the rule inside the crate —
    ///   keeps the rule intact for every other key.
    /// * **The scope is RECONCILED, not merely seeded**, and it widens only.
    ///
    /// # THE DEFECT THAT ADDED THE RECONCILIATION (2026-09-18)
    ///
    /// Seeding once was not enough, and the gap it left is the whole of the
    /// operator's report:
    ///
    /// > *"I've been using the account directly in claude-code but it doesn't
    /// > have history in threads so it's not getting selected. So if an account
    /// > is added to the pool, there's a condition where it can never be
    /// > used."*
    ///
    /// The scope written on the boot that created this key is the scope it
    /// keeps forever. `bob-example-com` was registered on 2026-09-17 and the
    /// `aui:default` row still read
    /// `["owner-account","owner-gmail","alice-example-com"]`, so
    /// [`Self::admit`] removed it at `scoped_accounts` — the FIRST filter, above
    /// thresholds, above targets, above history and above the ranker. Every
    /// AUI thread carries this key, so for every one of them the account did not
    /// exist.
    ///
    /// At 2026-09-18T19:34:57Z thread `bcccfa79` was consequently ported onto an
    /// account at **91% weekly** against its own 95% threshold while cc2 sat at
    /// **1%**, measured and ranked first on pressure. Its `account_switched`
    /// event names the field it chose from and cc2 is in neither half of it:
    /// *"admissible now: [Placeable(\"owner-account\")]; blocked:
    /// alice-example-com: weekly_all at 100% …; owner-gmail: weekly_all at
    /// 100% …"*. `ThreadManager::failover_target` re-asked with history cleared,
    /// exactly as designed, and could not help: its own contract lists `scope`
    /// among the causes no account change can clear, and the second ask
    /// re-applies the scope verbatim.
    ///
    /// # WHY WIDENING HERE IS NOT THE RULE THE CRATE REFUSES
    ///
    /// It is the same statement this function already made, applied again. The
    /// paragraph above ("every registered account is the right scope" for THIS
    /// key) is the ruling; making it only at creation is what made it stale.
    /// [`Self::purge_account_from_scopes`] already re-derives this key's scope
    /// from the pool for the same reason and says so in the same words — *"this
    /// is the same statement applied to a row written before the pool
    /// existed"*.
    ///
    /// # IT WIDENS ONLY, AND ONLY THIS KEY
    ///
    /// * A name in the persisted scope that is NOT in today's pool is left
    ///   there. Removing a name is [`Self::purge_account_from_scopes`]'s job, it
    ///   is told WHICH name by a caller that knows what the name means, and this
    ///   must not quietly acquire that power: a pool read that came back short
    ///   for any reason would otherwise silently narrow the fleet at boot.
    /// * Every OTHER key is untouched. *Narrowing a budget to a scope is a thing
    ///   the operator is allowed to mean*, and a key he scoped by hand is his.
    ///
    /// With no accounts registered there is nothing to scope to and this does
    /// nothing; a thread whose key has no config runs ungated, so nothing is
    /// blocked by the absence.
    ///
    /// Returns whether the row CHANGED — created or re-scoped. Both cases log
    /// their own line, because a migration that runs once should be visible
    /// once rather than silently.
    pub fn ensure_default(&self, accounts: &[String]) -> Result<bool> {
        if accounts.is_empty() {
            return Ok(false);
        }
        let Some(mut cfg) = self.get_config(DEFAULT_BUDGET_KEY)? else {
            let mut cfg = BudgetConfig::new(DEFAULT_BUDGET_KEY);
            cfg.accounts = accounts.to_vec();
            // Every axis stays None. This key METERS and cannot REFUSE.
            cfg.on_trip = TripAction::Finish;
            self.set_config(&cfg)?;
            tracing::info!(
                key = DEFAULT_BUDGET_KEY,
                accounts = accounts.len(),
                "created the fleet-wide default budget — attribution only, no ceiling on any axis"
            );
            return Ok(true);
        };
        // Only the SCOPE is touched. `cfg` was read whole, so every ceiling, the
        // trip action and the disabled state ride back out of `set_config`
        // exactly as they went in — the never-overwrite rule, narrowed to the
        // one field that is not the operator's to hold stale.
        let added: Vec<String> = accounts
            .iter()
            .filter(|a| !cfg.accounts.iter().any(|s| s == *a))
            .cloned()
            .collect();
        if added.is_empty() {
            return Ok(false);
        }
        cfg.accounts.extend(added.iter().cloned());
        self.set_config(&cfg)?;
        tracing::info!(
            key = DEFAULT_BUDGET_KEY,
            ?added,
            scope = ?cfg.accounts,
            "the pool holds accounts the fleet-wide budget's scope did not — widened it. \
             Until this ran, every thread carrying this key was unable to see them at all: \
             scope is the first filter in the admission gate and failover re-applies it"
        );
        Ok(true)
    }

    /// **THE LAST PLACE `"default"` SURVIVED A REBOOT.**
    ///
    /// [`Self::ensure_default`] never overwrites, and that is correct — boot
    /// must not undo a ceiling the operator set. The cost of that correctness is
    /// that the `aui:default` config written on some earlier boot still carries
    /// whatever scope it was created with, and on this machine it was created
    /// back when the machine default WAS a registered account. So the literal
    /// string `"default"` is still in the persisted `accounts` list, still comes
    /// back out of `budgets.get`, and no amount of removing the concept from
    /// placement touches it, because it is data and not code.
    ///
    /// It is harmless to PLACEMENT — nothing named `default` is in the pool, so
    /// the scope entry matches no account and gates nothing. It is NOT harmless
    /// to the operator, who reads `budgets.get` and is shown an account this
    /// module is not supposed to know exists:
    ///
    /// > *"We do not want the threads module to have any concept of the
    /// > default."*
    ///
    /// Run at boot, idempotent, and it reports which keys it rewrote so a
    /// migration that runs once is visible once rather than silently.
    ///
    /// # THIS MODULE IS NOT TOLD WHICH NAME, AND THAT IS DELIBERATE
    ///
    /// `account` is a PARAMETER. The first draft of this function referenced
    /// `accounts::DEFAULT_ACCOUNT` directly and
    /// `tests/default_out_of_placement.rs::the_placement_modules_do_not_name_
    /// the_machine_default` failed it on the spot — a source guard that bans
    /// the constant from executable code in `budgets.rs` and `pool.rs`,
    /// standing since the 2026-08-27 ruling:
    ///
    /// > *"I do not want default even in the purview of the system. Why does it
    /// > even see that there is a default?"*
    ///
    /// The guard was right and this is the fix rather than an exemption. A
    /// module that names the default to remove it still knows the default. What
    /// this module knows is how to remove A NAME it is handed; which name that
    /// is, is the caller's fact. `ThreadManager::recover` supplies it, and the
    /// concept stays where it belongs.
    ///
    /// # It will not strand a budget to purge a name
    ///
    /// A scope emptied by this removal would be refused by
    /// [`Self::validate_config`] and, worse, a budget scoped to nothing meters
    /// nothing. Two outcomes, deliberately different:
    ///
    /// * The FLEET-WIDE key re-scopes to the CURRENT POOL. That is not widening
    ///   the rule — [`Self::ensure_default`] already names every registered
    ///   account as this one key's correct scope, and this is the same statement
    ///   applied to a row written before the pool existed.
    /// * Any OTHER key is left exactly as it is, at ERROR. A key the operator
    ///   scoped by hand to that account alone is a question for him; silently
    ///   re-pointing his budget at accounts he did not name would be a worse
    ///   answer than leaving the residue and saying so.
    pub fn purge_account_from_scopes(
        &self,
        account: &str,
        pool: &[String],
    ) -> Result<Vec<String>> {
        let mut changed = Vec::new();
        for mut cfg in self.list_configs()? {
            if !cfg.accounts.iter().any(|a| a == account) {
                continue;
            }
            cfg.accounts.retain(|a| a != account);
            if cfg.accounts.is_empty() {
                if cfg.key == DEFAULT_BUDGET_KEY && !pool.is_empty() {
                    cfg.accounts = pool.to_vec();
                } else {
                    tracing::error!(
                        key = %cfg.key,
                        %account,
                        "this budget is scoped to that account and NOTHING ELSE, and that account \
                         is not a pool member — so this key already meters nothing. It is left \
                         exactly as it is rather than re-pointed at accounts you did not name. \
                         Re-scope it with `budgets/<key>/set`, or disable it."
                    );
                    continue;
                }
            }
            self.set_config(&cfg)?;
            tracing::info!(
                key = %cfg.key,
                %account,
                scope = ?cfg.accounts,
                "removed a non-pool account from this budget's scope — this row was the last \
                 place the name survived a reboot"
            );
            changed.push(cfg.key);
        }
        Ok(changed)
    }

    // ── metering ─────────────────────────────────────────────────────────

    /// Charge one model's consumption against `key` on `account`.
    ///
    /// The SAME consumption feeds EVERY applicable meter (they are independent,
    /// not alternative), and all usage inside a turn — including Claude Code's
    /// own subagent spawns, which already land in `modelUsage` — charges the
    /// parent thread's key (DEC-18: one budget per thread, no parent/child
    /// split).
    ///
    /// `model` matters because the weekly-FABLE meter counts fable-model usage
    /// only (round-7 item 4); session, weekly-all and cash take everything.
    /// `cost_usd` is what the CASH meter consumes (round-7 item 5).
    ///
    /// **`tokens` and `cost_usd` are SIGNED.** A negative charge is a
    /// CORRECTION, and it is the only way a provisional over-charge can be
    /// undone: the turn epilogue reconciles the live per-message estimate
    /// against the authoritative `result_turn` totals by charging the
    /// difference, which is negative when the estimate ran ahead. See
    /// [`crate::store::ChargeRow`].
    ///
    /// A DISABLED key is still charged (Q-34). Disabling stops turns from
    /// STARTING; it does not un-spend money already spent, and a turn that was
    /// already in flight when the key was disabled must still land on the
    /// ledger or the accounting the operator wants to keep querying would be
    /// wrong. Nothing new can start, so this can only ever be the tail of an
    /// in-flight turn.
    pub fn charge(
        &self,
        key: &str,
        thread_id: &str,
        account: &str,
        model: &str,
        tokens: i64,
        cost_usd: f64,
    ) -> Result<()> {
        if tokens == 0 && cost_usd == 0.0 {
            return Ok(());
        }
        let ts = crate::store::now_rfc3339();
        for axis in Axis::BUDGET_AXES {
            if !axis.accepts_model(model) {
                continue;
            }
            self.store.record_charge(&ChargeRow {
                budget_key: key.to_string(),
                thread_id: thread_id.to_string(),
                account: account.to_string(),
                axis: axis.as_str().to_string(),
                tokens,
                cost_usd,
                ts: ts.clone(),
            })?;
        }
        Ok(())
    }

    /// The reset instant to place `(account, axis)`'s window on, and how much
    /// that instant is worth — see [`ResetBasis`].
    ///
    /// Three rungs, best first:
    ///
    /// 1. **Observed** — the gauge reported it for this very axis.
    /// 2. **Borrowed** — fable only, from the same account's weekly all-models
    ///    axis, because the two are the same week. Only a LIVE instant is
    ///    borrowable: a weekly reset already in the past cannot place a window
    ///    that is currently open.
    /// 3. **Assumed** — nothing to borrow, so the window is taken to have
    ///    opened when we observed it and to run the axis's full length.
    ///
    /// Nothing here is written back to `usage_observations.resets_at`; the
    /// derivation is per-read, so a later real observation wins automatically.
    fn effective_reset_of(
        &self,
        account: &str,
        axis: Axis,
        obs: &ObservationRow,
        now: DateTime<Utc>,
    ) -> Option<EffectiveReset> {
        if let Some(at) = parse_rfc3339_utc(obs.resets_at.as_deref()) {
            return Some(EffectiveReset {
                at,
                basis: ResetBasis::Observed,
            });
        }
        if axis == Axis::WeeklyFable {
            if let Some(at) = self.borrowed_weekly_reset(account, now) {
                return Some(EffectiveReset {
                    at,
                    basis: ResetBasis::Borrowed,
                });
            }
        }
        let started = parse_rfc3339_utc(Some(obs.observed_at.as_str()))?;
        Some(EffectiveReset {
            at: axis.assumed_reset(started),
            basis: ResetBasis::Assumed,
        })
    }

    /// The same, reading the newest observation itself. `None` when the gauge
    /// has never reported this axis at all — an absence no assumption can fill,
    /// because there is no observation instant to anchor the window to.
    ///
    /// **Tests only, since the meter stopped re-reading the row.** The
    /// production path reads that observation ONCE per (account, axis) and
    /// hands it to [`Self::effective_reset_of`] — see [`Self::window_start_of`]
    /// for why. This wrapper is kept because the ladder it exercises (observed,
    /// borrowed, assumed) is worth testing through its own front door rather
    /// than through a hand-assembled row.
    #[cfg(test)]
    fn effective_reset(
        &self,
        account: &str,
        axis: Axis,
        now: DateTime<Utc>,
    ) -> Option<EffectiveReset> {
        let obs = self
            .store
            .recent_observations(account, axis.as_str(), 1)
            .ok()?
            .into_iter()
            .next()?;
        self.effective_reset_of(account, axis, &obs, now)
    }

    /// The newest LIVE reset instant on this account's weekly all-models axis —
    /// what the fable axis borrows when it has none of its own.
    fn borrowed_weekly_reset(&self, account: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.store
            .recent_observations(account, Axis::WeeklyAll.as_str(), RESET_BORROW_SCAN)
            .ok()?
            .into_iter()
            .filter_map(|o| parse_rfc3339_utc(o.resets_at.as_deref()))
            .find(|at| *at > now)
    }

    /// The start of the window currently being metered for `(account, axis)`.
    ///
    /// When a reset instant is known — observed, or borrowed from the same
    /// account's other weekly axis — the window is
    /// `[resets_at - window_len, resets_at)`; when the reset instant advances
    /// (a rollover), the start advances with it and the meter resets by
    /// construction.
    ///
    /// **A MISSING reset instant and an EXPIRED one are no longer the same
    /// case.** They used to share one `_` arm and both metered against a
    /// rolling window ending now, which silently mis-placed the window of every
    /// freshly rolled pool: the upstream gauge reports no reset time until the
    /// first token is spent, so a pool at 0% was metered as though it had been
    /// open for a full window, and up to a week of old charges were counted
    /// against a budget that had just started. Now a missing instant means
    /// *assume a fresh window* and the meter starts at the observation; an
    /// instant in the past still means *the data is old* and falls back to the
    /// rolling window, and so does an assumed window whose own end has already
    /// gone by, which is the same staleness wearing different clothes.
    pub fn window_start(&self, account: &str, axis: Axis, now: DateTime<Utc>) -> DateTime<Utc> {
        let latest = self
            .store
            .recent_observations(account, axis.as_str(), 1)
            .ok()
            .and_then(|v| v.into_iter().next());
        self.window_start_of(account, axis, latest.as_ref(), now)
    }

    /// **Has `key`'s `axis` window rolled over since `since`?** (R9.L4.)
    ///
    /// True when EVERY account the key meters ([`Self::metered_accounts`], the
    /// same set the meter sums over) is now metering a window that opened
    /// after `since` — read with [`Self::window_start_of`], the very rollover
    /// the meter itself uses. Every, not any: the meter POOLS the key's
    /// accounts, so until each of them has rolled, part of the spend counted
    /// at `since` may still be in the meter. A key with no config has nothing
    /// to roll and answers `false`.
    ///
    /// A store fault propagates (the caller fails closed), unlike
    /// [`Self::window_start`], which degrades a fault to the rolling window.
    pub fn rolled_over_since<T: AsRef<str>>(
        &self,
        key: &str,
        axis: Axis,
        since: DateTime<Utc>,
        pool: &[T],
    ) -> Result<bool> {
        let Some(cfg) = self.get_config(key)? else {
            return Ok(false);
        };
        let pool: Vec<String> = pool.iter().map(|a| a.as_ref().to_string()).collect();
        let names = self.metered_accounts(&cfg, &pool)?;
        if names.is_empty() {
            return Ok(false);
        }
        let now = crate::clock::now();
        for account in &names {
            let latest = self
                .store
                .recent_observations(account, axis.as_str(), 1)?
                .into_iter()
                .next();
            if self.window_start_of(account, axis, latest.as_ref(), now) <= since {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// [`Self::window_start`] with the newest observation ALREADY IN HAND.
    ///
    /// It exists because [`Self::meter`] needs three different fields off that
    /// one row — the window placement, the reset instant it reports, and the
    /// raw observed value — and used to fetch the row THREE TIMES to get them,
    /// once through `window_start`, once inline for `resets_at` and once
    /// through `observed_value`. Three identical `SELECT ... ORDER BY
    /// observed_at DESC LIMIT 1` statements, per account, per axis: 36 of the
    /// 83 round trips one `check_trip` made, and `check_trip` runs on EVERY
    /// token-usage line the CLI streams. Reading the same row once is not an
    /// optimisation with a correctness cost attached — the three readings could
    /// never legitimately disagree, and if a concurrent write landed between
    /// them they COULD, which is a defect rather than a feature.
    fn window_start_of(
        &self,
        account: &str,
        axis: Axis,
        latest: Option<&ObservationRow>,
        now: DateTime<Utc>,
    ) -> DateTime<Utc> {
        let len = ChronoDuration::seconds(axis.window_secs());
        match latest.and_then(|o| self.effective_reset_of(account, axis, o, now)) {
            // A live instant places the window exactly. For an ASSUMED one this
            // lands the start on the observation itself, which is what assuming
            // the window opened there means.
            Some(r) if r.at > now => r.at - len,
            // Stale, or never observed at all: the rolling window, honest and
            // never longer than the real one.
            _ => now - len,
        }
    }

    /// The latest raw observed value for `(account, axis)` in that axis's unit
    /// (percent, or dollars on the cash axis), or `Ok(None)` when the gauge has
    /// never reported it. `None` is never treated as "over" — enforcement is
    /// never built on a gauge we cannot read (CON-5).
    ///
    /// **A STORE fault is not an absence.** This used to end in `.ok()`, which
    /// collapsed "the database is unreadable" into "the gauge has never
    /// reported this axis" — and the threshold loop in [`Self::admit`] reads
    /// that as *never gate*, so a SQLite fault ADMITTED. That was the one
    /// genuine fail-OPEN seam left in the module. The error propagates now: the
    /// admission path takes its fail-CLOSED branch on a fault (threads with
    /// `budgeting_off` still run), while a genuine absence still admits.
    pub fn observed_value(&self, account: &str, axis: Axis) -> Result<Option<f64>> {
        Ok(self
            .store
            .recent_observations(account, axis.as_str(), 1)?
            .into_iter()
            .next()
            .map(|o| o.pct))
    }

    /// **WHEN THIS ACCOUNT WAS LAST MEASURED**, on ANY axis — the newest
    /// `observed_at` across all of them, or `None` when the gauge has never
    /// reported it.
    ///
    /// The maximum across axes rather than one chosen axis, because a poll
    /// records every axis `/usage` reported and a per-axis answer would call an
    /// account stale whenever one axis happened to be unmapped that tick.
    ///
    /// **A STORE FAULT READS AS ABSENCE HERE, and unlike
    /// [`Self::observed_value`] that is correct.** There the caller is the
    /// admission path and an absence means *never gate*, so collapsing a fault
    /// into an absence was a fail-OPEN. The only PRODUCTION caller of this is
    /// [`crate::runtime::ThreadManager::placement_poll`]'s staleness test, where
    /// an absence means *take a reading* — the strictly safe direction, costing
    /// one free `/usage` call.
    ///
    /// Compared as STRINGS, which is correct for RFC3339 UTC and is how every
    /// other range comparison over this column already works.
    pub fn last_observed_at(&self, account: &str) -> Option<String> {
        Axis::ALL
            .iter()
            .filter_map(|axis| {
                self.store
                    .recent_observations(account, axis.as_str(), 1)
                    .ok()?
                    .into_iter()
                    .next()
                    .map(|o| o.observed_at)
            })
            .max()
    }

    // ── the prior, the blend and the dual-currency adapter ───────────────
    //
    // See `gauge`'s "conservative PRIOR" note for the provenance of the number
    // and for why erring LOW is the safe direction. This half is the ladder:
    // rung 1 (this account's own accepted samples) is the durable
    // `limit_estimates` row the blend shrinks towards, rungs 2 and 3 are here.

    /// The prior tokens-per-percent for `(account, axis)` — the estimate to use
    /// before this account has measured this axis for itself.
    ///
    /// Rung 2: the TIGHTEST tokens-per-percent any other account has measured
    /// on the SAME axis. Accounts usually share a plan, so a sibling's
    /// calibration is real evidence about this one, and taking the smallest
    /// keeps the conservative direction. Same-axis only, deliberately: the
    /// three percentage pools are not interchangeable, and letting a
    /// weekly-fable measurement set a session prior couples two axes that
    /// nothing says are proportional.
    ///
    /// Rung 3: [`gauge::PRIOR_TOKENS_PER_PERCENT`].
    fn axis_prior(&self, account: &str, axis: Axis) -> Result<f64> {
        let mut tightest: Option<f64> = None;
        for e in self.store.list_estimates()? {
            if e.axis != axis.as_str() || e.account == account || e.sample_count == 0 {
                continue;
            }
            if e.tokens_per_percent.is_finite() && e.tokens_per_percent > 0.0 {
                tightest = Some(tightest.map_or(e.tokens_per_percent, |t: f64| {
                    t.min(e.tokens_per_percent)
                }));
            }
        }
        Ok(tightest.unwrap_or(gauge::PRIOR_TOKENS_PER_PERCENT))
    }

    /// The blended tokens-per-percent for `(account, axis)`, and the number of
    /// accepted samples behind it.
    ///
    /// This is the number the meter divides by, and it is never absent: an
    /// account with no samples gets the prior, an account with four or more
    /// gets its own measured mean, and in between the two are interpolated by
    /// [`gauge::blend_rate`].
    fn blended_tokens_per_percent(&self, account: &str, axis: Axis) -> Result<(f64, u64)> {
        let est = self.store.get_estimate(account, axis.as_str())?;
        let measured = est
            .as_ref()
            .map(|e| (e.tokens_per_percent, e.sample_count))
            .filter(|(_, n)| *n > 0);
        // Skip the fleet scan entirely once this account's own measurement has
        // fully displaced the prior — the commonest case on a warm daemon.
        let prior = match measured {
            Some((mean, n)) if gauge::prior_weight(n) <= 0.0 => mean,
            _ => self.axis_prior(account, axis)?,
        };
        Ok((
            gauge::blend_rate(prior, measured),
            measured.map(|(_, n)| n).unwrap_or(0),
        ))
    }

    /// The dual-currency adapter for `(account, axis)` — ONE estimator serving
    /// percent, tokens and dollars, carrying its own confidence (msg #876).
    ///
    /// `usd_per_token` is REALISED from the account's own ledger wherever it has
    /// one: total cost over total tokens, all-time, so the mix of models and
    /// cache hits the operator actually runs is what prices a token. That is the
    /// dollars half of "long-term memory of measurements so the code
    /// self-optimizes"; before any spend exists it falls back to
    /// [`gauge::PRIOR_USD_PER_TOKEN`].
    ///
    /// `None` for the CASH axis: dollars are measured directly and need no
    /// denominator, and inventing one would put the estimator underneath
    /// numbers that are already exact.
    fn denominator(&self, account: &str, axis: Axis) -> Result<Option<gauge::Denominator>> {
        if axis.unit() == gauge::Unit::Dollars {
            return Ok(None);
        }
        let (tokens_per_percent, samples) = self.blended_tokens_per_percent(account, axis)?;
        Ok(Some(gauge::Denominator {
            tokens_per_percent,
            usd_per_token: self.realised_usd_per_token(account)?,
            samples,
        }))
    }

    /// Dollars per token as this account has actually spent them, or the
    /// conservative prior rate until it has spent anything.
    fn realised_usd_per_token(&self, account: &str) -> Result<f64> {
        let realised = self
            .store
            .spend_summary(true)?
            .into_iter()
            .find(|s| s.key == account)
            .filter(|s| s.total_tokens > 0 && s.cost_usd > 0.0)
            .map(|s| s.cost_usd / s.total_tokens as f64)
            .filter(|r| r.is_finite() && *r > 0.0);
        Ok(realised.unwrap_or(gauge::PRIOR_USD_PER_TOKEN))
    }

    /// A reading for `(account, axis)` derived from a gauge reading in the
    /// OTHER denomination, when this axis's own gauge has never reported.
    ///
    /// This is the adapter doing gate work: a limit written in dollars can be
    /// admitted against a percent reading, and vice versa, both through the same
    /// estimator. Where several foreign axes are readable the LARGEST converted
    /// value wins, which is the reading that refuses earliest.
    ///
    /// Returns `(value in this axis's own unit, the axis it came from)`.
    fn cross_denomination_reading(&self, account: &str, axis: Axis) -> Result<Option<(f64, Axis)>> {
        let want = gauge::Denom::of(axis.unit());
        let mut best: Option<(f64, Axis)> = None;
        for other in Axis::ALL {
            if other == axis || other.unit() == axis.unit() {
                continue;
            }
            let Some(raw) = self.observed_value(account, other)? else {
                continue;
            };
            // The estimator always belongs to the PERCENTAGE axis of the pair —
            // it is the one with a token denominator to convert through.
            let est_axis = if axis.unit() == gauge::Unit::Percent {
                axis
            } else {
                other
            };
            let Some(adapter) = self.denominator(account, est_axis)? else {
                continue;
            };
            let Some(v) = adapter.convert(raw, gauge::Denom::of(other.unit()), want) else {
                continue;
            };
            if best.is_none_or(|(b, _)| v > b) {
                best = Some((v, other));
            }
        }
        Ok(best)
    }

    /// The accounts a key's meters cover: its EXPLICIT scope intersected with
    /// the registry (round-7 item 6), plus any account it has historically
    /// drawn on — narrowing a scope must not make past spend evaporate.
    fn metered_accounts(&self, cfg: &BudgetConfig, pool: &[String]) -> Result<Vec<String>> {
        let mut names = cfg.scoped_accounts(pool);
        for a in self.store.charge_accounts(&cfg.key)? {
            if !names.contains(&a) {
                names.push(a);
            }
        }
        Ok(names)
    }

    fn meter(
        &self,
        cfg: &BudgetConfig,
        axis: Axis,
        names: &[String],
        now: DateTime<Utc>,
    ) -> Result<Meter> {
        let limit = cfg.limit(axis);
        let dollars_axis = axis.unit() == gauge::Unit::Dollars;

        let mut rows = Vec::new();
        let mut consumed = 0.0;
        let mut consumed_tokens = 0u64;
        let mut consumed_usd = 0.0;
        let mut prior_only = false;
        for account in names {
            // ONE read of this (account, axis)'s newest observation, feeding
            // all three things the meter wants from it. See
            // [`Self::window_start_of`] for why it used to be three.
            let latest = self
                .store
                .recent_observations(account, axis.as_str(), 1)?
                .into_iter()
                .next();
            let start = self.window_start_of(account, axis, latest.as_ref(), now);
            let start_s = start.to_rfc3339();
            let tokens = self
                .store
                .charged_tokens(&cfg.key, axis.as_str(), account, &start_s)?;
            let dollars = self
                .store
                .charged_dollars(&cfg.key, axis.as_str(), account, &start_s)?;
            consumed_tokens += tokens;
            consumed_usd += dollars;

            // The cash axis is measured DIRECTLY in dollars, so it needs no
            // denominator and can never report `calibrating` (round-7 item 5).
            let (limit_tokens, share, samples) = if dollars_axis {
                (None, (dollars > 0.0).then_some(dollars), 0)
            } else {
                // UN-GATED (msg #1055). There is always a denominator now: this
                // account's own accepted samples where it has them, the
                // conservative prior where it does not, and the blend in
                // between. What used to happen here — `limit_tokens` coming back
                // `None`, `share` staying `None`, the meter flagging itself
                // `calibrating` and the budget declining to bite — is gone.
                let (tpp, samples) = self.blended_tokens_per_percent(account, axis)?;
                let lt = tpp * 100.0;
                let pct = (lt > 0.0).then(|| tokens as f64 / lt * 100.0);
                if tokens > 0 && samples == 0 {
                    // Spend priced by the PRIOR rather than by measurement. The
                    // meter still gates (that is the point); this only labels
                    // the status so a reader can tell the two apart.
                    prior_only = true;
                }
                (
                    (lt > 0.0).then_some(lt),
                    pct.filter(|_| tokens > 0),
                    samples,
                )
            };
            if let Some(s) = share {
                consumed += s;
            }

            let resets_at = latest.as_ref().and_then(|o| o.resets_at.clone());
            rows.push(AccountMeter {
                account: account.clone(),
                window_start: start_s,
                window_resets_at: resets_at,
                tokens,
                dollars,
                limit_tokens,
                consumed: share,
                calibration_samples: samples,
                observed: latest.as_ref().map(|o| o.pct),
            });
        }

        let status = match limit {
            None => MeterStatus::Unset,
            // Over the line is over the line, whether the line was drawn by
            // measurement or by the prior.
            Some(l) if consumed >= l => MeterStatus::Exhausted,
            // REPOINTED: `calibrating` is a label now, not a refusal. It says
            // "this meter is gating on the prior, not yet on measurement" —
            // and `Exhausted` above is checked first precisely so that saying
            // so never suppresses a trip.
            Some(_) if prior_only => MeterStatus::Calibrating,
            Some(_) => MeterStatus::Ok,
        };

        Ok(Meter {
            axis,
            limit,
            consumed,
            consumed_tokens,
            consumed_usd,
            status,
            accounts: rows,
        })
    }

    /// A key's full live state — one meter per axis (round-7 item 4/5).
    ///
    /// Generic over the pool's element type for the same reason
    /// [`Self::check_trip`] is: metering asks with `String`, admission asks with
    /// [`Placeable`], and reporting spend can never hand back a place to run.
    pub fn snapshot<T: AsRef<str>>(&self, key: &str, pool: &[T]) -> Result<Option<BudgetSnapshot>> {
        let Some(cfg) = self.get_config(key)? else {
            return Ok(None);
        };
        let now = crate::clock::now();
        let pool: Vec<String> = pool.iter().map(|a| a.as_ref().to_string()).collect();
        let names = self.metered_accounts(&cfg, &pool)?;
        let mut meters = Vec::with_capacity(Axis::ALL.len());
        for axis in Axis::ALL {
            meters.push(self.meter(&cfg, axis, &names, now)?);
        }
        Ok(Some(BudgetSnapshot {
            config: cfg,
            meters,
            metered_accounts: names,
        }))
    }

    /// Has this key tripped? Used both pre-flight and mid-turn.
    ///
    /// Generic over the pool's element type because both a metering caller
    /// (`String`, the visible pool) and the admission gate ([`Placeable`], the
    /// placement pool) ask this question. Safe in either direction: it reads
    /// meters and answers yes/no, and it never returns an account, so no
    /// destination can leak out of the widened type.
    pub fn check_trip<T: AsRef<str>>(&self, key: &str, pool: &[T]) -> Result<Option<Trip>> {
        let pool: Vec<String> = pool.iter().map(|a| a.as_ref().to_string()).collect();
        let Some(snap) = self.snapshot(key, &pool)? else {
            return Ok(None);
        };
        let Some(axis) = snap.tripped_axis() else {
            return Ok(None);
        };
        let meter = snap.meter(axis);
        Ok(Some(Trip {
            budget_key: key.to_string(),
            axis,
            consumed: meter.consumed,
            limit: meter.limit.unwrap_or(0.0),
            action: snap.config.on_trip,
        }))
    }

    // ── the gauge feed ───────────────────────────────────────────────────

    /// Record one gauge observation and immediately try to extend the
    /// calibration for that (account, axis) — DEC-12's "re-estimate after
    /// every thread execution" is driven from here, because the free
    /// `rate_limit_event` push arrives on every turn.
    pub fn observe(
        &self,
        account: &str,
        axis: Axis,
        pct: f64,
        resets_at: Option<String>,
        source: &str,
    ) -> Result<()> {
        let now = crate::store::now_rfc3339();
        self.store.record_observation(&ObservationRow {
            id: 0,
            account: account.to_string(),
            axis: axis.as_str().to_string(),
            pct,
            resets_at,
            source: source.to_string(),
            observed_at: now.clone(),
        })?;
        // A 429 is a CLIFF, not a measured delta: it tells us the axis is
        // exhausted, not how many tokens moved the needle. Pairing it would
        // poison the estimate, so the backstop layer observes but never
        // calibrates.
        //
        // `subagent_limit_hit` is the SAME cliff observed one level down (a
        // subagent's refusal reaching the parent's stream as a
        // `system/task_updated` patch, see `runtime::subagent_limit_signal`).
        // It carries a distinct source so the ledger can tell the two apart,
        // and it must be excluded here for exactly the same reason — a new
        // source string that is not listed here is a `tokens_per_percent`
        // poisoned by a synthetic 100.
        if source == "limit_hit" || source == "subagent_limit_hit" {
            return Ok(());
        }
        self.calibrate_overlap_window(account, axis, crate::clock::now())?;
        Ok(())
    }

    /// Try to extend calibration for `(account, axis)` using the operator's
    /// OVERLAP-WINDOW method (msg #620) — see `gauge`'s module docs for the
    /// method in full. Evaluates the newest overlap window this daemon's own
    /// executions have completed since the last folded-in sample; returns the
    /// verdict for callers (tests, the live driver) that want to see WHY a
    /// poll did or did not produce a sample, without ever fabricating one.
    pub(crate) fn calibrate_overlap_window(
        &self,
        account: &str,
        axis: Axis,
        now: DateTime<Utc>,
    ) -> Result<WindowVerdict> {
        let existing = self.store.get_estimate(account, axis.as_str())?;
        let since_id = existing.as_ref().map(|e| e.last_obs_id).unwrap_or(0);
        let observations = self.store.observations_from(account, axis.as_str(), since_id)?;
        if observations.len() < 2 {
            return Ok(WindowVerdict::NoWindow);
        }
        let from_ts = observations.first().unwrap().observed_at.clone();
        let to_ts = now.to_rfc3339();
        let intervals = self.store.execution_intervals(account, &from_ts, &to_ts)?;
        let parsed: Vec<(DateTime<Utc>, DateTime<Utc>)> = intervals
            .iter()
            .filter_map(|(s, e)| Some((gauge::parse_instant(s)?, gauge::parse_instant(e)?)))
            .collect();
        let windows = gauge::merge_windows(parsed);
        // Only the NEWEST window is ever evaluated per poll: a permanently
        // rejected older window (e.g. one that caught idle movement) must
        // never block calibration from ever advancing again, and re-deriving
        // the same verdict on a window that stays newest is cheap and
        // idempotent.
        let Some(&window) = windows.last() else {
            return Ok(WindowVerdict::NoWindow);
        };
        let after_id = observations
            .iter()
            .filter(|o| gauge::parse_instant(&o.observed_at).is_some_and(|t| t >= window.end))
            .map(|o| o.id)
            .min();
        let Some(after_id) = after_id else {
            return Ok(WindowVerdict::NoWindow); // not bracketed by a poll yet
        };
        if after_id <= since_id {
            return Ok(WindowVerdict::NoWindow); // already folded into the estimate
        }
        let model_filter = axis.is_model_scoped().then_some("fable");
        let tokens = self.store.account_tokens_between(
            account,
            &window.start.to_rfc3339(),
            &window.end.to_rfc3339(),
            model_filter,
        )?;
        let evidence = gauge::WindowEvidence {
            axis,
            all_windows: &windows,
            observations: &observations,
            tokens,
            unsettled_since: self.unsettled_since(account)?,
        };
        match gauge::evaluate_window(window, evidence) {
            Ok(sample) => {
                let (mean, count) = gauge::merge_calibration(
                    existing.map(|e| (e.tokens_per_percent, e.sample_count)),
                    sample,
                );
                self.store.upsert_estimate(&crate::store::EstimateRow {
                    account: account.to_string(),
                    axis: axis.as_str().to_string(),
                    tokens_per_percent: mean,
                    sample_count: count,
                    last_obs_id: after_id,
                    updated_at: now.to_rfc3339(),
                })?;
                // THE ESTIMATE JUST CHANGED, so the bucket for the period it
                // changed IN is updated too — the operator's design, verbatim:
                // "every time the update changes for that period, update it."
                // The row above is overwritten forever and remembers nothing;
                // this is where the history lives.
                self.record_period_estimate(account, axis, mean, count, now)?;
                Ok(WindowVerdict::Sampled { tokens_per_percent: sample, window, tokens })
            }
            Err(reason) => Ok(WindowVerdict::Rejected { reason, window, tokens }),
        }
    }

    /// Fold the CURRENT estimate for `(account, axis)` into the bucket for the
    /// reset window it is currently inside — the per-period history behind
    /// [`Self::estimate_period_history`].
    ///
    /// # The period is a real reset window, not a clock tick
    ///
    /// The operator asked for this to be bucketed by the limit's own period,
    /// not sampled on a timer: "every usage limit that we have, maybe you
    /// update our estimate for that usage limit in that period … And then when
    /// the period is over, just leave it and move forward and start recording
    /// the next one."
    ///
    /// The window is the reset instant the newest gauge observation REPORTS for
    /// this (account, axis), compared with [`gauge::same_window`], whose
    /// tolerance is what stops a reset that is only ever reported to the MINUTE
    /// from opening a fresh bucket every poll. Same window ⇒ the open row is
    /// rewritten in place. A window that has rolled ⇒ the old row is left
    /// exactly as it stands, forever, and a new one opens. Freezing needs no
    /// code: it is what not writing means.
    ///
    /// With NO reported reset instant there is no period, and nothing is
    /// recorded — a bucket is never invented for a window we cannot name.
    ///
    /// # It is NOT [`Self::window_start`]'s notion, and the doc used to say it was
    ///
    /// This paragraph claimed "there is deliberately no second notion of it: …
    /// the same value [`Self::window_start`] meters against". That is false, and
    /// it was false in the direction that hides a gap. `window_start` goes
    /// through [`Self::effective_reset_of`], a THREE-rung ladder —
    /// [`ResetBasis::Observed`], then [`ResetBasis::Borrowed`] for the fable
    /// axis, then [`ResetBasis::Assumed`]; this function reads rung one only.
    ///
    /// Rung three is a real difference and the sentence below is the one that
    /// governs it: an `Assumed` reset is `observed_at + axis length`, which IS
    /// inventing a window we cannot name, so declining to bucket it is correct
    /// and is kept.
    ///
    /// **Rung two is a gap, and it is left as one deliberately.**
    /// [`ResetBasis::Borrowed`] exists because the fable axis often reports no
    /// reset of its own, and the weekly-all instant it borrows describes the same
    /// week — a window that CAN be named. So every fable period on a reset-less
    /// poll is silently absent from [`Self::estimate_period_history`]. Closing it
    /// is a behaviour change to the calibration record rather than a cleanup, and
    /// it belongs with the clock-seam question the calibration bracket already
    /// has open; it is reported rather than taken here.
    ///
    /// # The machine default cannot reach this, and no longer by a name check
    ///
    /// This function used to open with `if account == POINTER_LABEL { return }`
    /// — a literal `"default"` in the placement module, refusing a pointer
    /// label whose history would otherwise silently re-attribute itself when
    /// the operator re-points it. The live database carries the proof of that
    /// hazard: a `default` row frozen since 2026-08-03 whose fingerprint now
    /// names an account whose own row disagrees with it by 42%.
    ///
    /// The GUARD is gone; the guarantee is stronger. Accounts reach this
    /// function from [`crate::pool::Pool`] only, and the machine default is not
    /// a pool member, so it is not reachable here by any path. That also closes
    /// the five sibling writers the old guard never covered — `observe`,
    /// `gauge_state`, `set_account_identity`, `ensure_default` and the ledger
    /// — each of which happily recorded under `default` while this one line
    /// stood alone and looked like an invariant.
    ///
    /// Returns the `period_end` written, or `None` when nothing was recorded.
    pub fn record_period_estimate(
        &self,
        account: &str,
        axis: Axis,
        tokens_per_percent: f64,
        sample_count: u64,
        now: DateTime<Utc>,
    ) -> Result<Option<String>> {
        // The window we are in RIGHT NOW, from the newest reading.
        let Some(period_end) = self
            .store
            .recent_observations(account, axis.as_str(), 1)?
            .into_iter()
            .next()
            .and_then(|o| o.resets_at)
        else {
            return Ok(None);
        };
        let state = self
            .store
            .list_account_states()?
            .into_iter()
            .find(|a| a.name == account);
        let identity = login_key(account, state.as_ref().and_then(|a| a.identity.as_deref()));
        let config_dir = state.as_ref().and_then(|a| a.config_dir.clone());
        let ts = now.to_rfc3339();
        let mut row = crate::store::PeriodEstimateRow {
            identity,
            axis: axis.as_str().to_string(),
            period_end,
            account_label: account.to_string(),
            config_dir,
            tokens_per_percent,
            limit_tokens: tokens_per_percent * 100.0,
            sample_count,
            updates: 1,
            first_recorded_at: ts.clone(),
            updated_at: ts,
        };
        let open = self
            .store
            .latest_period_estimate(&row.identity, &row.axis)?;
        // THE BOUNDARY. Is the newest bucket the one we are still inside?
        if let Some(open) =
            open.filter(|o| gauge::same_window(Some(&o.period_end), Some(&row.period_end)))
        {
            // Yes — still live. Keep the bucket's ORIGINAL window instant: a
            // reset reported a minute off is the same period, not a new one.
            row.period_end = open.period_end;
            if self.store.update_period_estimate(&row)? > 0 {
                return Ok(Some(row.period_end));
            }
        }
        // No — the period is over. The row above is left exactly as it stands
        // and never written again; this opens the next one.
        self.store.open_period_estimate(&row)?;
        Ok(Some(row.period_end))
    }

    /// The per-period estimate history, one entry per (login, axis, window) —
    /// what capacity looked like on each account over time, which is the
    /// question the live `limit_estimates` table structurally cannot answer.
    ///
    /// `account_label` is reported but is NOT the key; `identity` is. The
    /// account CLASS the purchasing decision turns on (business email vs
    /// personal) is deliberately absent: nothing in this process holds an
    /// email address to derive it from.
    pub fn estimate_period_history(&self) -> Result<Value> {
        let rows = self.store.list_period_estimates()?;
        Ok(Value::Array(
            rows.into_iter()
                .map(|p| {
                    json!({
                        "identity": p.identity,
                        "account_label": p.account_label,
                        "config_dir": p.config_dir,
                        "axis": p.axis,
                        "period_end": p.period_end,
                        "tokens_per_percent": p.tokens_per_percent,
                        "limit_tokens": p.limit_tokens,
                        "samples": p.sample_count,
                        "updates": p.updates,
                        "first_recorded_at": p.first_recorded_at,
                        "updated_at": p.updated_at,
                    })
                })
                .collect::<Vec<_>>(),
        ))
    }

    /// The earliest start of an execution of ours that is STILL RUNNING on
    /// `account` — the instant from which the usage ledger is knowably
    /// incomplete, because a turn in flight has spent tokens it has not
    /// recorded yet (the row is written from its `result_turn`).
    ///
    /// `running_turns` carries no account column, so the thread's most
    /// recently served account (`thread_accounts`, ordered newest first) is the
    /// best evidence available, and an UNKNOWN one counts against calibrating:
    /// a turn we cannot attribute is treated as if it were on this account. The
    /// cost of being wrong that way is a sample we decline to take; the cost of
    /// the opposite default is a sample that is wrong, which is worse.
    fn unsettled_since(&self, account: &str) -> Result<Option<DateTime<Utc>>> {
        let mut earliest: Option<DateTime<Utc>> = None;
        for r in self.store.list_running_turns()? {
            // A row whose process is gone is not in flight — it is a leftover
            // that `recover()` clears at the next daemon start. Letting one
            // count would block this account's calibration forever.
            if !crate::runtime::pid_alive(r.pid) {
                continue;
            }
            let serving = self
                .store
                .thread_accounts(&r.thread_id)
                .unwrap_or_default()
                .first()
                .cloned();
            if serving.is_some_and(|a| a != account) {
                continue;
            }
            if let Some(t) = gauge::parse_instant(&r.started_at) {
                earliest = Some(earliest.map_or(t, |e| e.min(t)));
            }
        }
        Ok(earliest)
    }

    /// **IS THIS ACCOUNT QUIET — is there no execution of ours still in flight
    /// on it?** The second of the two conditions behind
    /// [`UnrankedReason::OverShareSettled`], and the operator's `executing ==
    /// 0`.
    ///
    /// # It DELEGATES, and that is the point
    ///
    /// [`Self::unsettled_since`] already answers exactly this question — it
    /// returns the earliest start of a still-running turn attributed to
    /// `account`, and `None` when there is none — and it already gets the three
    /// hard parts right:
    ///
    /// * **A dead pid is not in flight.** A `running_turns` row whose process
    ///   is gone is a leftover `recover()` clears at the next daemon start;
    ///   counting one would make an account look busy forever and would pin a
    ///   divergent negative pressure on the board permanently, which is the
    ///   opposite of the defect this is fixing.
    /// * **`running_turns` carries no account column**, so attribution goes
    ///   through the thread's most recently served account
    ///   (`thread_accounts`, newest first).
    /// * **An UNATTRIBUTABLE running turn counts AGAINST quiet** — it is
    ///   treated as if it were on this account. That default is the safe one in
    ///   both callers and for the same reason: the cost of being wrong that way
    ///   is a reading we keep showing, and the cost of the opposite default is
    ///   a live fact hidden. A turn we cannot name might be the very thread
    ///   whose overage the operator is looking at.
    ///
    /// Writing a second loop here would have forked the word "running" inside
    /// one file, which is how the two halves drift.
    ///
    /// # Cost, because this sits on the ranking path
    ///
    /// One `list_running_turns` scan plus one `thread_accounts` read per row in
    /// flight. That is why the caller evaluates it ONLY after the over-share
    /// test has already said yes ([`Self::pressure_reading`]'s tail): being
    /// over one's lent share is the rare state, so a healthy four-account
    /// placement pays nothing at all for this.
    ///
    /// A store FAULT answers "not quiet" (`false`) rather than propagating —
    /// the same CON-5 direction `pressure_reading` takes for a limit read. An
    /// exclusion may only ever come from a value actually read, so a failed
    /// read must never be the thing that removes a reading from view.
    fn nothing_running_on(&self, account: &str) -> bool {
        match self.unsettled_since(account) {
            Ok(v) => v.is_none(),
            Err(e) => {
                tracing::warn!(
                    %account,
                    error = %e,
                    "running-turn read failed; treating the account as busy so its pressure stays visible"
                );
                false
            }
        }
    }

    /// Estimation state for every (account, axis) — the "estimation state"
    /// half of the AUI query surface.
    ///
    /// `tokens_per_percent` stays the RAW measured mean, which is what the
    /// overlap-window sampler wrote and what a reader auditing the samples
    /// wants. `effective_tokens_per_percent` is what the meter actually divides
    /// by after the prior is blended in, alongside the confidence behind it.
    pub fn estimation_state(&self) -> Result<Value> {
        let ests = self.store.list_estimates()?;
        let mut out = Vec::with_capacity(ests.len());
        for e in ests {
            let axis = Axis::parse(&e.axis);
            let effective = match axis {
                Some(a) => self.blended_tokens_per_percent(&e.account, a)?.0,
                None => e.tokens_per_percent,
            };
            out.push(json!({
                "account": e.account,
                "axis": e.axis,
                "tokens_per_percent": e.tokens_per_percent,
                "limit_tokens": e.tokens_per_percent * 100.0,
                "effective_tokens_per_percent": effective,
                "effective_limit_tokens": effective * 100.0,
                "samples": e.sample_count,
                "confidence": (e.sample_count as f64 / gauge::PRIOR_SAMPLES).clamp(0.0, 1.0),
                "updated_at": e.updated_at,
                "calibrated": e.sample_count > 0,
            }));
        }
        Ok(Value::Array(out))
    }

    /// **ONE per-axis gauge row — the single implementation the query and the
    /// shape fact both go through.**
    ///
    /// Lifted out of [`Self::gauge_state`] for P1, and the lift is the point.
    /// `Fact::GaugeObserved` has to carry exactly what `accounts.status` and
    /// `gauge.status` render for that axis, or the live path and the hydrate
    /// path feed one cell two different shapes and a mirror silently disagrees
    /// with a re-read. Calling the same function from both is how that is made
    /// impossible rather than merely tested for; `gauge_state` is now this
    /// function in a loop, so the existing wire bytes are unchanged by
    /// construction.
    pub fn gauge_axis_row(&self, account: &str, axis: Axis) -> Result<Value> {
        let obs = self.store.recent_observations(account, axis.as_str(), 1)?;
        let est = self.store.get_estimate(account, axis.as_str())?;
        // `limit_tokens` is the EFFECTIVE denominator — prior-blended,
        // and therefore present on every percentage axis from first
        // boot, which is what the meter is actually gating on.
        // `calibrated` still distinguishes measurement from the prior.
        let adapter = self.denominator(account, axis)?;
        Ok(json!({
            "observed_pct": obs.first().map(|o| o.pct),
            "resets_at": obs.first().and_then(|o| o.resets_at.clone()),
            "source": obs.first().map(|o| o.source.clone()),
            "observed_at": obs.first().map(|o| o.observed_at.clone()),
            "limit_tokens": adapter.map(|d| d.limit_tokens()),
            "usd_per_token": adapter.map(|d| d.usd_per_token),
            "confidence": adapter.map(|d| d.confidence()),
            "calibration_samples": est.as_ref().map(|e| e.sample_count).unwrap_or(0),
            "calibrated": est.as_ref().is_some_and(|e| e.sample_count > 0),
        }))
    }

    /// Live per-account gauge readings for all four axes (`Axis::ALL` —
    /// session, weekly-all, weekly-fable and cash).
    pub fn gauge_state(&self, accounts: &[String]) -> Result<Value> {
        let mut out = Vec::new();
        for a in accounts {
            let mut axes = serde_json::Map::new();
            for axis in Axis::ALL {
                axes.insert(axis.as_str().to_string(), self.gauge_axis_row(a, axis)?);
            }
            out.push(json!({ "account": a, "axes": Value::Object(axes) }));
        }
        Ok(Value::Array(out))
    }

    // ── placement: WHICH permissible account (rank 7, §7 items 1-4) ──────

    /// Choose the account a thread should run on, out of the accounts it is
    /// ALREADY permitted to use.
    ///
    /// **Placement is not gating.** The caller has already decided which
    /// accounts are allowed (pins, health, scope, thresholds); this only
    /// ranks them. That distinction is the whole point: a thread with no
    /// budget key and no thresholds — which is every AUI thread — has nothing
    /// to enforce and still has to be put SOMEWHERE, and until now that
    /// somewhere was "whichever account sorts first alphabetically", forever.
    ///
    /// # The objective: the fewest expected THREAD DEATHS, subagents included
    ///
    /// INTENT 83 (idx `2951`): *"You're trying to minimize the number of threads
    /// that died. And so you have to treat all of those sub-agents as threads.
    /// You're not going to know beforehand, unfortunately, exactly how many
    /// sub-agents there will be, but you can use the estimate."*
    ///
    /// So the ranking is:
    ///
    /// 1. **Expected deaths** — [`Demand::expected_deaths`] against each
    ///    account's [`Reach`], which is the min across the session, weekly-all
    ///    and weekly-fable axes of what can be spent before the nearest limit
    ///    closes. Fewest deaths wins; anything within
    ///    [`DEATHS_DEADBAND`] of the best is a tie and falls through.
    /// 2. **The CAP** ([`Self::landing_capacity`], 2026-08-26) — the graded
    ///    landing capacity on the SESSION axis: how much of the room the pool
    ///    was actually offered is still free. A capacity, not a bit. Thresholds
    ///    elsewhere in this crate are a binary veto, so placement used to be
    ///    blind right up to a ceiling and then fall off it; this grades the
    ///    same line so the field steers away before arriving.
    /// 3. **The RANK — pool pressure** ([`Self::pool_pressure`], 2026-08-26,
    ///    replacing the weekly-pace SUBTRACTION that shipped on 2026-08-21) —
    ///    the required burn rate, unused fraction of the account's POOL SHARE
    ///    DIVIDED by the fraction of its window still to run. Drain the largest
    ///    first. Unused budget on a window that resets SOON must burn fast and
    ///    attracts the turn; a nearly-spent budget with days still to run is
    ///    the fleet's reserve and repels it. Inside the cap band only, never
    ///    mixed into the objective, and exactly silent when no account has a
    ///    usable window — an account whose gauge reports no reset time is
    ///    ranked on an ASSUMED fresh window rather than dropped, which is what
    ///    stops the emptiest account being deprioritised for being empty.
    ///
    ///    **The order is VETO, then CAP, then RANK**, and it is an order rather
    ///    than a blend. The veto is not in this function at all — it is the
    ///    threshold loop in [`Self::admit`], and `candidates` is what it left
    ///    behind. That is the structural reason it needs no infinite weight: a
    ///    veto expressed as a weight WOULD need to be infinite, and any finite
    ///    number would eventually let enough pressure drag a background loop
    ///    into the operator's working session.
    /// 4. **Cash**, in dollars, strictly after and never mixed into the token
    ///    measurement. The cash axis is not a quota window — it is the
    ///    operator's own spending ceiling — so it does not belong in "what can
    ///    be spent before the nearest limit closes", and summing it in let a
    ///    $50 threshold contribute 50 "points" against percentages.
    /// 5. **Fewest threads already placed** here, then **momentum**
    ///    ([`Self::momentum_is_warm`]) — the account the thread is already on,
    ///    but ONLY while its prompt cache is still warm — then a persistent
    ///    round-robin cursor, then the name. Deterministic throughout — no
    ///    jitter, ever: randomised starts fragment the merged spend windows and
    ///    dirty the idle gaps that calibration is measured in.
    ///
    ///    Momentum decides WHICH account, never WHETHER to place. It is here,
    ///    at the bottom, precisely so it cannot outrank the cap or the veto:
    ///    every candidate it chooses between has already cleared both.
    ///
    /// # There is no special case, and there must never be one
    ///
    /// Nothing here names a "large thread" or a "nearly-closed account". Both
    /// behaviours fall out of the measurement, which is exactly the constraint
    /// the operator set (*"that should just be a natural falling out of… the
    /// measurement that it will hit the second limit"*):
    ///
    /// * An account near ANY limit has a small [`Reach`], because reach is the
    ///   MIN across the axes — so it scores worse for every thread and stops
    ///   attracting work, with no tradeoff parameter to tune.
    /// * A big thread — either a big burn or a big fan-out — has a large
    ///   `expected_deaths` SPREAD across accounts, so the deadband stops
    ///   absorbing the difference and the measurement starts discriminating. A
    ///   thread that spawns 1,200 subagents turns a 0.001 difference in
    ///   survival into 1.2 expected deaths and is therefore placed carefully; a
    ///   lone thread with the same difference stays exactly where it is. The
    ///   fan-out term is what makes those two cases differ, and it is the only
    ///   thing that does.
    ///
    /// An axis with NO reading for an account is UNKNOWN, not "empty".
    /// Treating it as empty (the old `None => headroom += limit`) is
    /// arithmetically identical to reading 0%, so a never-polled account
    /// ranked as maximally free and won every placement — exactly the state a
    /// freshly registered account is in. Unknown ranks behind any account we
    /// have measured, which is safe here precisely because the gates already
    /// ran: a measured candidate that is still in `candidates` is, by
    /// construction, under every threshold the thread carries.
    ///
    /// Returns `None` only when `candidates` is empty.
    pub fn place(&self, req: &PlacementRequest<'_>) -> Result<Option<Placeable>> {
        // ── THE SHARE CLAMP, BEFORE THE SHORT-CIRCUIT ──
        //
        // `admit` already removes over-share accounts from the target set, so
        // for that caller this is idempotent and costs one gauge read. It is
        // here for the OTHER caller: `ThreadManager::auto_place` ranks
        // `selectable_pool()` directly at thread-CREATE time and never passes
        // through `admit`, so without this a brand-new thread could be placed
        // straight onto an account the operator has clamped.
        //
        // It runs ABOVE the `len() == 1` short-circuit deliberately. That
        // short-circuit returns the only candidate without consulting anything,
        // which is exactly the shape of "if all the other accounts are blocked"
        // — the case the clamp exists for. A filter placed below it would have
        // been correct in every test with two free accounts and wrong in the
        // only case the operator actually described.
        //
        // An all-clamped field returns `None`. `admit` turns that into a
        // `ParkCause::Share` park; `auto_place` falls back to pool order, which
        // only decides where a thread is CREATED — its first turn still has to
        // pass the gate, and the gate parks it.
        //
        // ── AND THE DISABLE, WHICH RUNS FIRST ──
        //
        // A DISABLED account (a true 0% share) is removed from the candidate set
        // HERE, before a single criterion is computed for it — not ranked last,
        // not present with a clamped pressure, absent. *"It should be out of the
        // placement selection. That's the cleanest."*
        //
        // It is ordered ahead of the clamp deliberately, so a disabled account
        // never reaches `over_share` at all: `place` is the one function both
        // callers pass through, so an exclusion applied here is an exclusion
        // applied everywhere, and the same short-circuit argument that put the
        // clamp above `len() == 1` applies with more force to a disabled account — "if all
        // the other accounts are blocked" must not be how work reaches the
        // account he switched off.
        let mut unclamped: Vec<Placeable> = Vec::with_capacity(req.candidates.len());
        for c in req.candidates {
            if self.is_disabled(c.name()) {
                continue;
            }
            if !self.over_share(c.name())? {
                unclamped.push(c.clone());
            }
        }
        let req = &PlacementRequest {
            candidates: &unclamped,
            ..req.clone()
        };
        match req.candidates.len() {
            0 => return Ok(None),
            1 => return Ok(Some(req.candidates[0].clone())),
            _ => {}
        }
        let counts = self.placement_counts();
        let cursor = self.placement_cursor();
        // One clock for the whole field: pace is a comparison across accounts,
        // and each candidate reading its own `now` would let wall-clock drift
        // inside one placement masquerade as a difference between accounts.
        let now = crate::clock::now();
        // Rotation offsets are assigned over a STABLE order, so the cursor
        // means the same thing from one call to the next.
        let mut names: Vec<Placeable> = req.candidates.to_vec();
        names.sort();
        names.dedup();
        let n = names.len();

        let mut front: Vec<PlacementKey> = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let is_current = req.prefer == Some(name.name());
                PlacementKey {
                    deaths: req
                        .demand
                        .expected_deaths(self.reachable_tokens(name, req.thresholds)),
                    cap: self.landing_capacity(name, req.thresholds),
                    pressure: self.pool_pressure(name, now),
                    cash: self.axis_slack(name, Axis::Cash, req.thresholds.cash),
                    // The thread being placed is itself sitting on `prefer`.
                    // If it counted against that account, spread would push
                    // every thread off the account it is already on and then
                    // straight back — the criterion has to weigh the world
                    // WITHOUT the thread being weighed.
                    placed: counts
                        .get(name.name())
                        .copied()
                        .unwrap_or(0)
                        // The DISCOUNT is on `prefer`, NOT on momentum. Whether
                        // the thread counts against its own current account is
                        // a fact about the census; whether its cache is warm is
                        // a different question with a different answer, and
                        // tying them would make spread wobble with the clock.
                        .saturating_sub(usize::from(is_current)),
                    // MOMENTUM: the account the thread is on AND a cache still
                    // warm. Evaluated lazily and short-circuited on
                    // `is_current`, which is false for almost every candidate —
                    // a ledger query per losing candidate would be a table scan
                    // bought for nothing.
                    momentum: is_current && self.momentum_is_warm(name.name(), now),
                    is_current,
                    // A bare-name comparison, exactly as `thread_accounts`
                    // stores it. Matching HERE rather than filtering the
                    // candidate list upstream is the whole of the 2026-09-18
                    // fix: an account this thread has never run on is a
                    // candidate that must earn its place, not a candidate that
                    // was never there.
                    has_session: req.sessions.iter().any(|h| h.as_str() == name.name()),
                    rotation: (i + cursor) % n,
                    name: name.clone(),
                }
            })
            .collect();

        // THE OBJECTIVE, first and alone: the fewest expected thread deaths.
        // An unmeasured account (`NaN`) is dropped whenever any candidate has a
        // real score, and the field is untouched when nobody does.
        narrow_min(&mut front, DEATHS_DEADBAND, |k| k.deaths);
        // ── then the CAP, then the RANK. The operator's order, and it is an
        // order, not a blend: VETO, then CAP, then RANK.
        //
        // The veto already ran — it is the threshold loop in `admit`, and it is
        // ABSOLUTE. Nothing below this line can reinstate a candidate it
        // refused, because `candidates` is what the veto left behind and
        // narrowing only ever removes. That is the structural reason a veto
        // here needs no infinite weight: it is not a weight at all. Expressed
        // as one it would need to be infinite, and any finite number would let
        // enough pressure drag a background loop into a working session.
        //
        // THE CAP is second: the graded landing capacity, so the field steers
        // away from a ceiling before reaching it instead of falling off it.
        narrow_unless_disabled(&mut front, LANDING_CAP_DEADBAND, |k| k.cap);
        // THE RANK is third, inside the cap band: drain the LARGEST pressure
        // first — the account that must burn fastest to exhaust the share it
        // was given before its reset destroys the remainder.
        //
        // Even distribution is NOT aimed at here and must not be. It is the
        // FIXED POINT of always draining the largest: take from the most
        // pressured account and its pressure falls below the next one's, so the
        // field levels itself. Aiming at evenness directly would be a second,
        // conflicting objective.
        narrow_unless_disabled(&mut front, PRESSURE_DEADBAND, |k| k.pressure);
        // Then cash, in its own unit, still never mixed into the one above.
        narrow(&mut front, PLACEMENT_DEADBAND_USD, |k| k.cash);

        // ── CONTINUITY, AND EVERYTHING MEASURABLE HAS ALREADY SPOKEN ──
        //
        // Every criterion above this line is a MEASUREMENT with a deadband
        // calibrated to its own noise floor. Everything below it is
        // determinism: fairness, cache warmth, anti-churn, a rotation cursor
        // and finally alphabetical order. This is the seam, and continuity
        // belongs on this side of it.
        //
        // The rule: if any candidate still standing is one this thread already
        // holds a conversation on, the ones it does not are dropped. So an
        // account the thread has never run on wins only by BEATING every
        // session-bearing candidate on deaths, the cap, pressure or cash — and
        // never by edging one out on a tie-break.
        //
        // # WHY THIS REPLACED A FILTER, AND WHAT THE FILTER WAS COSTING
        //
        // `admit`'s re-rank branch used to build its candidate field as the
        // thread's history INTERSECTED with the placeable targets. That is the
        // same fact expressed as a VETO, and expressed that way it made a new
        // account permanently unreachable by every thread that already existed:
        // history is written when a turn ends on an account, so you needed to
        // have been placed somewhere to be rankable for it. The operator hit it
        // with `bob-example-com` on 2026-09-18 — *"if an account is added to
        // the pool, there's a condition where it can never be used"* — and it
        // is the same unconditional stickiness the `on_admissible` gate was
        // removed for, one layer further in.
        //
        // # AND WHY DEMOTING IT COSTS NOTHING THE FILTER WAS BUYING
        //
        // The filter read as "an account switch restarts the conversation
        // unless the thread already holds a session on the target". It does
        // not. `ThreadManager::move_thread` PORTS the transcript to whatever
        // target it is handed (DEC-56 — moving an account column without
        // carrying the conversation is the thing that is forbidden, and it is
        // forbidden on every path) and then derives `resumable` from the port's
        // own outcome: *"the target now holds a session for this thread id,
        // which is exactly what `thread_accounts` records"*. A destination that
        // already appears in history buys nothing the port does not also buy.
        // INTENT 83 struck the cost argument in the same words: *"the
        // justification you used that it's going to be more expensive to move
        // it… that's not true because you're gonna move it beforehand anyway."*
        //
        // What a switch does cost is a COLD PROMPT CACHE, and that cost is
        // identical for a dormant session and for a fresh account — a cache
        // older than five minutes is gone either way. The criterion that prices
        // it is [`Self::momentum_is_warm`], it is five minutes wide, it is two
        // lines below, and it was already the operator's whole rule: *"the only
        // time that momentum should play in is if a thread is getting started
        // back up within five minutes of when it ended."*
        //
        // # ORDERED ABOVE SPREAD, DELIBERATELY
        //
        // Spread is a fairness criterion over the pool CENSUS — how many
        // threads sit where — and it knows nothing about this thread's
        // conversation. Below spread, an untouched new account would collect
        // threads simply for being empty, buying a cold cache with a criterion
        // that never measured one. Above it, spread still decides between two
        // accounts the thread has sessions on, which is what it is for.
        //
        // NO NEW CONSTANT, and that is a decision rather than an omission. A
        // margin of its own here would be a second, independently-tuned answer
        // to "is this difference real" sitting on top of four deadbands that
        // already answer it in their own units — the shape the ranker warns
        // against where it refuses to aim at even distribution directly.
        if front.iter().any(|k| k.has_session) {
            front.retain(|k| k.has_session);
        }

        // Then determinism: spread, momentum, rotate, and finally the name —
        // which is where the old rule started, and why every thread ended up
        // on the same account.
        if let Some(fewest) = front.iter().map(|k| k.placed).min() {
            front.retain(|k| k.placed == fewest);
        }
        // MOMENTUM, demoted to exactly here: last of the substantive criteria,
        // strictly after the veto (in `admit`) and the cap (above). It picks
        // WHICH account among ones already cleared; it never decides WHETHER.
        if front.iter().any(|k| k.momentum) {
            front.retain(|k| k.momentum);
        }
        // ANTI-CHURN, and it is NOT the same criterion as the one above it.
        //
        // The single `stay` flag that used to live here was doing two jobs at
        // once, and gating that flag on cache warmth would have silently
        // deleted the second one. Momentum is about COST: a warm prompt cache
        // makes the next turn on this account cheaper, and it expires after
        // [`MOMENTUM_WINDOW_SECS`]. This is about CHURN: moving a thread for no
        // measurable reason is worse than leaving it alone, whatever the cache
        // is doing, because every criterion above has already declared the
        // field a tie. Without it a settled thread ping-pongs on the rotation
        // cursor forever.
        //
        // Strictly weaker than momentum and strictly after it, so a warm
        // account still wins outright; this only decides a field where nothing
        // measurable separates the candidates at all.
        if front.iter().any(|k| k.is_current) {
            front.retain(|k| k.is_current);
        }
        let best = front
            .iter()
            .min_by(|x, y| (x.rotation, &x.name).cmp(&(y.rotation, &y.name)))
            .map(|k| k.name.clone());

        // The cursor turns on genuinely NEW placements only. Turning it on
        // every admission would let it flip a settled thread's account from
        // under it on a turn where nothing about the accounts changed.
        if req.prefer.is_none() {
            self.advance_placement_cursor(cursor);
        }
        Ok(best)
    }

    /// **THE REACHABLE BUDGET: what can actually be spent on this account
    /// before the NEAREST quota window closes, in tokens.**
    ///
    /// The min across the three quota axes — session, weekly-all, weekly-fable
    /// — never their sum and never a lexicographic walk of them. An account
    /// with a week of headroom and a nearly-closed session can serve only what
    /// the session allows, and pretending otherwise is what let a thread be
    /// placed onto an account that could not carry it through the turn.
    ///
    /// The three axes are measured in percent of three DIFFERENT pools, so
    /// taking a min over the raw percentages would be comparing unlike things.
    /// Each axis's slack is converted into tokens through that
    /// (account, axis)'s own denominator — the same dual-currency adapter the
    /// meters use, prior-backed so it is never absent — and the min is taken in
    /// tokens, which is a unit all three share.
    ///
    /// **EVERY OBSERVED AXIS COUNTS, gated or not.** An axis with a threshold
    /// is measured against that threshold; an axis without one is measured
    /// against a notional full account (100%). An axis with no reading is
    /// silent either way.
    ///
    /// This used to narrow: gated on at least one axis ⇒ only the gated axes
    /// count. That clause contradicted the paragraph above it — "an account
    /// with a week of headroom and a nearly-closed session can serve only what
    /// the session allows, and pretending otherwise is what let a thread be
    /// placed onto an account that could not carry it through the turn" is an
    /// argument for counting the session axis whether or not anyone gated on
    /// it. The contradiction was harmless only while thresholds were rare and
    /// deliberate.
    ///
    /// It stopped being harmless on 2026-08-15, when `Thresholds::weekly`
    /// acquired a fleet-wide default: "gated on at least one axis" became
    /// universally true, so ranking narrowed to the WEEKLY axis alone across
    /// the whole fleet and placement went blind to a session sitting at 92%.
    /// A ceiling the operator set is a reason to measure an axis MORE closely,
    /// never a reason to stop measuring the other two.
    pub fn reachable_tokens(&self, account: &str, th: &Thresholds) -> Reach {
        let ceilings = [
            (Axis::Session, th.session),
            (Axis::WeeklyAll, th.weekly),
            (Axis::WeeklyFable, th.fable),
        ];
        let mut tightest: Option<f64> = None;
        for (axis, limit) in ceilings {
            let Slack::Known(pct) = self.axis_slack(account, axis, limit) else {
                continue;
            };
            // The denominator is prior-backed and therefore never absent, but a
            // fault reading it must not silently price this axis at zero and
            // condemn a healthy account: an unreadable denominator makes the
            // AXIS silent, exactly like an unobserved gauge.
            let per_pct = match self.denominator(account, axis) {
                Ok(Some(d)) => d.tokens_per_percent,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(
                        %account, axis = axis.as_str(), error = %e,
                        "denominator read failed; this axis does not price the reachable budget"
                    );
                    continue;
                }
            };
            if !per_pct.is_finite() || per_pct <= 0.0 {
                continue;
            }
            let tokens = pct.max(0.0) * per_pct;
            tightest = Some(tightest.map_or(tokens, |t: f64| t.min(tokens)));
        }
        // No quota axis has ever been read on this account: unknown, not free.
        tightest.map_or(Reach::Unknown, Reach::Known)
    }

    /// **THE ESTIMATOR** behind [`Demand`] — what this thread's next turn will
    /// cost and how many threads it is, read off the daemon's own history.
    ///
    /// Both halves are EXPONENTIALLY WEIGHTED means over the thread's most
    /// recent turns, newest heaviest, and **explicitly not ARIMA**: the series
    /// is a handful of ragged, non-stationary samples, the only thing riding on
    /// it is an ordering across accounts, and it self-corrects because
    /// `usage_ledger` records what was actually consumed.
    ///
    /// * **Burn** — total tokens per completed execution, from the delta rows
    ///   of `usage_ledger` (`accounting_rev = 1`; the cumulative-snapshot rows
    ///   are inflated by an unknown factor and are excluded from every derived
    ///   number). Subagent spend is already inside those rows, because the
    ///   whole claude process bills to the parent thread — so the burn is the
    ///   WHOLE execution, parent and children together, which is exactly what
    ///   an account has to be able to carry.
    /// * **Fan-out** — subagents per turn, counted from the `agent_spawn`
    ///   events the ledger already records, bracketed by `turn_started`.
    ///
    /// A thread with no history gets [`Demand::default`]. Never an error: a
    /// missing estimate must degrade the ranking, never refuse a placement.
    pub fn estimate_demand(&self, thread_id: &str) -> Demand {
        let tokens = self
            .store
            .recent_turn_tokens(thread_id, DEMAND_SAMPLES)
            .unwrap_or_else(|e| {
                tracing::warn!(thread = %thread_id, error = %e, "burn history unreadable; using the default turn size");
                Vec::new()
            });
        let fanout = self
            .store
            .recent_turn_fanout(thread_id, DEMAND_SAMPLES)
            .unwrap_or_else(|e| {
                tracing::warn!(thread = %thread_id, error = %e, "fan-out history unreadable; assuming no subagents");
                Vec::new()
            });
        Demand {
            tokens_per_turn: ew_mean(&tokens).unwrap_or(DEFAULT_TURN_TOKENS).max(1.0),
            subagents: ew_mean(&fanout).unwrap_or(0.0).max(0.0),
        }
    }

    /// Room on one axis for one account, in that axis's own unit.
    ///
    /// The ceiling is a per-thread CONSTANT across accounts, so it cancels out
    /// of any within-axis comparison — which is why an UNGATED axis can be
    /// ranked at all: it falls back to a notional full account (100%, or $0
    /// spent) and therefore ranks on raw usage, lowest first.
    fn axis_slack(&self, account: &str, axis: Axis, limit: Option<f64>) -> Slack {
        match self.observed_value(account, axis) {
            Ok(Some(v)) => {
                let ceiling = limit.unwrap_or(match axis.unit() {
                    gauge::Unit::Percent => 100.0,
                    gauge::Unit::Dollars => 0.0,
                });
                Slack::Known(ceiling - v)
            }
            Ok(None) => Slack::Unknown,
            // A ranking INPUT, not a gate — the same reasoning as
            // `placement_counts` below. The fail-CLOSED decision on an
            // unreadable store belongs to the threshold loop in `admit`, which
            // sees this very error as an `Err` and refuses; downgrading it to
            // `Unknown` here only costs spread, and it must not turn a
            // *placement* pass into a second, differently-worded refusal.
            Err(e) => {
                tracing::warn!(%account, axis = axis.as_str(), error = %e, "gauge read failed; ranking this axis as unknown");
                Slack::Unknown
            }
        }
    }

    /// **The fraction of this ACCOUNT that was given to the pool manager** —
    /// the denominator of [`Self::pool_pressure`] and the boundary
    /// [`Self::over_share`] clamps at.
    ///
    /// The operator does not hand the daemon whole accounts; he hands it a
    /// share and keeps the rest to spend by hand. *"It needs to change the
    /// denominator somewhere so that it's measuring pressure against the 95%
    /// and really fully trying to exploit the 95%, but not calculating pressure
    /// as though there's 100%. Because then if I only gave 10% of an account,
    /// well, then there's going to be disproportionate pressure of 10x on that
    /// 10% account when really, the amount available to it is only the 10% to
    /// begin with."*
    ///
    /// **It DIVIDES, and that is the whole point.** Read the failure case
    /// again: 10x pressure on a 10% account is what you get when the share is
    /// applied as a numerator — scaling the unused budget up — instead of as
    /// the denominator that normalises it against what was actually offered.
    /// Dividing, an untouched 10% account and an untouched 100% account are
    /// both exactly on pace and both score 1.0.
    ///
    /// [`DEFAULT_POOL_SHARE`] when no row exists, which is every account until
    /// the operator sets one and is byte-for-byte the behaviour that shipped
    /// before the table existed. An unreadable store degrades to the same
    /// default — a ranking input lost, never a gate, on the same policy as
    /// [`Self::axis_slack`].
    ///
    /// # Keyed on the NAME, which is a correction
    ///
    /// This read used to resolve the account's LOGIN FINGERPRINT and key on
    /// that. The operator overruled it on 2026-08-27: *"I think it's actually
    /// better to key on the account name instead of on the login slot, because
    /// the login slot is subject to change."* Re-pointing a label at a
    /// different login — which `add-account --force` does routinely — silently
    /// moved or lost the share under the old key. The name is the stable thing
    /// he manages, and a share is a statement about what he is lending the
    /// pool, not about a credential.
    ///
    /// # Two writers, one precedence, stated here and nowhere else
    ///
    /// The `pool_shares` STORE ROW is the operator's runtime override and WINS.
    /// The `share` field on the `pool.json` ENTRY is the declared share, used
    /// when no row exists. Absent both, [`DEFAULT_POOL_SHARE`] — the whole
    /// account, which is byte-for-byte the behaviour that shipped before shares
    /// existed and is the "no backfill, no shim" default.
    ///
    /// # This number has TWO effects, as of 2026-08-27
    ///
    /// It is still the DENOMINATOR of the ranking, unchanged. It is ALSO the
    /// boundary [`Self::over_share`] refuses NEW placement past. The operator
    /// added the second effect explicitly — *"it needs to have an effect on
    /// pressure, but it also needs to have an effect on whether placement is
    /// possible"* — and it is NOT the per-account threshold veto he struck
    /// (*"I don't know why you're going to need a per account veto"*): that was
    /// about thresholds, and no per-account threshold has been reintroduced.
    /// See [`Self::over_share`] for the exact asymmetry between running and new
    /// work.
    ///
    /// # This number stopped carrying a THIRD effect on 2026-09-20
    ///
    /// Between 2026-09-08 and 2026-09-20 a stored `0.0` here WAS the disable, so
    /// this one read answered two questions. The operator split them — *"I want
    /// disabled to be independent of share"* — and inclusion moved to its own
    /// table. This function now reads BOTH and reports both, which is why it
    /// still returns a [`PoolShare`] and not an `f64`: `lent()` must stay the one
    /// door to the arithmetic, and it must stay closed for a disabled account.
    /// Nothing below zero, nothing above one and no zero at all disables: those
    /// are nonsense, still loud, and still ignored.
    ///
    /// **Two store reads per call, and that is accepted.** This is on the
    /// ranking path, called once per candidate per placement. Both are indexed
    /// single-row lookups on the read pool and `account_disabled` is empty on
    /// almost every machine; the alternative — folding inclusion back into the
    /// share column to save a query — is the defect this change exists to undo.
    pub fn pool_share(&self, account: &str) -> PoolShare {
        // A LENT share is strictly positive. `Store::set_pool_share` refuses
        // everything else, so anything outside this band is a hand-edited table.
        let sane = |s: f64| s.is_finite() && s > 0.0 && s <= 1.0;
        let share = match self.store.pool_share(account) {
            Ok(Some(s)) if sane(s) => s,
            Ok(Some(s)) => {
                // Either a row that would claim more account than exists, a
                // NEGATIVE share, or a `0.0` left by the pre-2026-09-20 disable
                // that the migration somehow did not reach. A zero here must
                // NEVER be read back as "disabled": that meaning is gone, and
                // reviving it in a fallback is how a retired convention
                // reappears years later as an unexplained special case.
                tracing::warn!(
                    %account, share = s,
                    "pool share on disk is outside (0.0, 1.0]; using the whole account"
                );
                DEFAULT_POOL_SHARE
            }
            // No override: fall through to what the blob DECLARES.
            Ok(None) => self
                .pool()
                .declared_share(account)
                .filter(|s| sane(*s))
                .unwrap_or(DEFAULT_POOL_SHARE),
            Err(e) => {
                tracing::warn!(%account, error = %e, "pool share unreadable; using the whole account");
                DEFAULT_POOL_SHARE
            }
        };
        // A FAULT IS NOT A DISABLING, and the direction is deliberate: a disable
        // is something the operator SAID, so it may only ever come from a value
        // we actually read. Degrading an unreadable store into "disabled" would
        // take an account out of the pool on the strength of a failure — the
        // same class of error as barring an account because a gauge did not
        // answer (CON-5). `unwrap_or(None)` is that rule, spelled once.
        match self.store.account_disabled_at(account).unwrap_or(None) {
            Some(_) => PoolShare::withheld(share),
            None => PoolShare::pooled(share),
        }
    }

    /// **Is this account DISABLED — out of placement selection entirely?**
    ///
    /// The one predicate the exclusion is expressed in, so "disabled" has a single
    /// definition that [`Self::place`], [`Self::admit`] and every reader share.
    /// It is a pure read of the row the operator's own act wrote; it consults no
    /// gauge, no health flag, no clock and — since 2026-09-20 — no share, which
    /// is what makes it impossible for a disabled account to expire, retry or
    /// lift itself, and impossible for a share edit to enable or disable one.
    pub fn is_disabled(&self, account: &str) -> bool {
        self.pool_share(account).is_disabled()
    }

    // ── `restore_share` WAS HERE, AND IT IS GONE (2026-09-20) ────────────────
    //
    // It answered "what share would an enable put this account back at" with a
    // four-rung ladder: what it is on, else the `pool_shares.prior_share` memo
    // the disable stashed, else what `pool.json` declares, else the whole
    // account. All four rungs existed for one reason — a disable OVERWROTE the
    // share with zero, so the number had to be reconstructed afterwards.
    //
    // A disable destroys nothing now. The share an enable comes back at is the
    // share the account never stopped having, which is `pool_share().share()`
    // and is already on every payload as `share`. A second field answering the
    // same question is exactly the "two answers to one question" this crate
    // keeps paying for, so `restore_to` and `prior` left the wire with it, and
    // `Store::pool_share_prior` left with them. The memo column survives only so
    // `Store::migrate_share_zero_disable` can spend it once.
    //
    // Recorded rather than silently deleted: the ladder was right, and the whole
    // point is that the problem it climbed no longer exists.

    /// **THE HARD CLAMP: has this account spent past the share it was given?**
    ///
    /// The operator, 2026-08-27, after being shown that overage merely reads
    /// negative in the ranking:
    ///
    /// > *"if all the other accounts are blocked, we don't want that account to
    /// > even be available for placement, right? So overage we've determined is
    /// > okay while the current workload is finishing. But then it needs to be
    /// > like hard clamped — not available for placement as well. So it needs
    /// > to have an effect on pressure, but it also needs to have an effect on
    /// > whether placement is possible."*
    ///
    /// So the share boundary is BOTH a pressure term and a veto. Negative
    /// pressure discourages placement, which he called necessary but not
    /// sufficient: a discouragement loses to an empty field, and *"if all the
    /// other accounts are blocked"* is exactly the case where a ranking term
    /// has nothing left to lose to.
    ///
    /// # The asymmetry, which is the whole ruling
    ///
    /// **Work already running on an over-share account runs to completion.**
    /// What stops is NEW placement onto it. This is not "kill the account", and
    /// implementing it as one would have thrown away the overage he explicitly
    /// permitted (*"overage we've determined is okay while the current workload
    /// is finishing"*).
    ///
    /// Mechanically it is the shape [`AdmissionRequest::unhealthy`] already
    /// has, which is why no new concept was added to `admit`: the account is
    /// removed from the TARGET set and left in the CLEARED set. Concretely —
    ///
    /// * a turn in flight is never re-admitted, so it is not touched at all;
    /// * a thread WITH history there is re-ranked over a field the clamp has
    ///   already been subtracted from, so it MOVES to another account it holds
    ///   a session on if one will take it — and when none will, `on_admissible`
    ///   leaves it exactly where it is and its next turn runs there. Starting a
    ///   NEW turn on the clamped account is not "the current workload
    ///   finishing"; being unable to start one anywhere else is;
    /// * a FRESH thread is placed elsewhere, and if there is nowhere else it
    ///   PARKS under [`ParkCause::Share`] rather than landing there.
    ///
    /// # What it measures
    ///
    /// The weekly all-models axis, the same axis [`Self::pool_pressure`]
    /// divides — one boundary, read once, so the term and the veto can never
    /// disagree about where the line is. `consumed >= share`.
    ///
    /// **UNKNOWN IS NOT OVER.** No observation means no evidence of overage,
    /// and a gauge that has never reported must not bar an account; that
    /// direction is CON-5 and it is also the only direction in which a
    /// never-polled account does not lock the whole fleet out on boot.
    ///
    /// At the default share of 1.0 this fires only on a weekly reading at or
    /// past 100%, which `config.gauge_bar_pct` already bars by default — so an
    /// unconfigured pool behaves exactly as it does today.
    ///
    /// # A DISABLED account is not "over its share"
    ///
    /// It has no share. DISABLED is a separate, stronger exclusion applied
    /// before this one ([`Self::is_disabled`]), so this is never the reason a
    /// disabled account is refused — and answering `true` here would file the
    /// operator's own deliberate act under the clamp's reason string ("has spent
    /// past the share it was lent"), which is a different fact with a different
    /// remedy. Note the arithmetic below would have said `true`: with a share of
    /// zero, `consumed >= 0.0` is true for every reading there has ever been.
    /// That accident is exactly the kind of "it happens to work" this task was
    /// told not to ship.
    fn over_share(&self, account: &str) -> Result<bool> {
        let Some(share) = self.pool_share(account).lent() else {
            return Ok(false);
        };
        Ok(match self.observed_value(account, Axis::WeeklyAll)? {
            Some(v) => (v / 100.0).max(0.0) >= share,
            None => false,
        })
    }

    /// **THE REQUIRED BURN RATE: how fast this account must now spend the share
    /// it was given, if that share is to be exhausted by its reset.**
    ///
    /// ```text
    /// pressure = (unused fraction of the SHARE) ÷ (fraction of the window remaining)
    /// ```
    ///
    /// **A DIVISION, and deliberately not the subtraction it replaces.** The
    /// operator, on being shown `unused − time_left`: *"I don't see why it's a
    /// subtraction and not a division."* He is right, and the difference is not
    /// cosmetic. A subtraction is an additive gap between two fractions, so it
    /// says the same thing about 2% unused with 1% of the window left as about
    /// 51% unused with half the window left — both are +0.01 — when the first
    /// account must burn at twice its nominal rate and the second is exactly on
    /// schedule. A division answers the question actually being asked, which is
    /// a RATE: 1.0 is on pace, above 1.0 is an account that has to spend faster
    /// than nominal or lose the remainder to its reset, below 1.0 is an account
    /// running ahead of its clock whose remainder is the fleet's burst reserve.
    ///
    /// # The share is the DENOMINATOR
    ///
    /// `unused` is not measured against the whole account. It is measured
    /// against [`Self::pool_share`] — what the operator actually gave the pool:
    ///
    /// ```text
    /// unused_of_share = (share − consumed) ÷ share
    /// ```
    ///
    /// so a login offering 95% reads 1.0 when untouched, 0.0 when it has spent
    /// exactly its 95%, and NEGATIVE past that. Getting this backwards is the
    /// failure the operator named by name: *"if I only gave 10% of an account,
    /// well, then there's going to be disproportionate pressure of 10x on that
    /// 10% account when really, the amount available to it is only the 10% to
    /// begin with."* Normalised this way there is no 10x — a fresh 10% account
    /// and a fresh 100% account are both on pace at 1.0, and the small account
    /// attracts work in proportion to what it was actually offered.
    ///
    /// # Overage stays VISIBLE, and is never blocked here
    ///
    /// The result is NOT clamped at zero. An account that has spent past its
    /// share reads negative, and the size of that negative number is how far
    /// past it went — which the operator asked to be able to see, because the
    /// remainder is his to spend by hand. Clamping would have destroyed exactly
    /// the reading he wants. Overage repels work (it sorts last) but refuses
    /// nothing: the thing that refuses a turn is a THRESHOLD, gated in
    /// [`Self::admit`], and this criterion never gets a vote on admission.
    ///
    /// # What it is measured on, and when it is silent
    ///
    /// The weekly all-models axis ONLY — the pool the operator watched
    /// evaporate, the axis every thread is gated on by default
    /// ([`DEFAULT_WEEKLY_THRESHOLD_PCT`]), and the one whose window is long
    /// enough for "time remaining" to mean anything. Both inputs come off the
    /// SAME newest observation, so the percent and the clock describe one
    /// instant. Thresholds stay percentages throughout (CON-41): nothing here
    /// prices tokens and nothing here touches dollars — the only cost figures
    /// available are API list prices, which mean nothing on a Max account.
    ///
    /// The term VANISHES rather than guessing when there is no observation, or
    /// when the window is stale (a reset instant already in the past, meaning
    /// the window rolled and the gauge has not caught up). A MISSING
    /// `resets_at` is not one of those cases — an absent instant yields an
    /// ASSUMED window opening at the observation ([`ResetBasis::Assumed`]), so
    /// a fresh empty account ranks on pace instead of being dropped from the
    /// front for being empty, which was the 2026-08-25 inversion.
    ///
    /// # Weekly wants EXHAUSTION, and the target is exactly one death
    ///
    /// This is the exhaustion half of the objective, and it points the opposite
    /// way from [`Self::landing_capacity`] on purpose. The operator's target is
    /// *"ideally no more than one and ideally no less than one"* death — which
    /// is minimising TO one, not minimising deaths. Pressure supplies the "no
    /// less than one": left alone, the expected-deaths objective would settle
    /// for zero by never fully spending anything, and a week that ends with
    /// budget unspent is a week whose capacity was thrown away. Pressure pulls
    /// toward burning the share out by its reset; the deaths objective above it
    /// is what keeps that from costing more than the one.
    ///
    /// A store FAULT is a ranking input lost, not a gate — same policy and same
    /// reasoning as [`Self::axis_slack`]: `admit`'s threshold loop has already
    /// seen any real store error as an `Err` and failed CLOSED.
    fn pool_pressure(&self, account: &str, now: DateTime<Utc>) -> Slack {
        // THE RANKER'S VIEW IS A PROJECTION OF THE READER'S VIEW, not a second
        // computation of it. Everything that decides the number lives in
        // [`Self::pressure_reading`]; this is the two-state collapse the
        // placement key wants. `accounts.pressure` renders the same struct.
        // There is exactly one implementation of this arithmetic and it is not
        // this function — which is the entire point, because the accounts
        // interface asked for these values and recomputing them in Python would
        // have forked it a third time.
        match self.pressure_reading(account, now).value {
            Some(v) => Slack::Known(v.rank_key()),
            None => Slack::Unknown,
        }
    }

    /// **THE PRESSURE READING — the ranked value and everything needed to render
    /// it honestly.**
    ///
    /// [`Self::pool_pressure`] is a private two-state collapse: a number, or
    /// `Unknown`. That is all the ranker needs and strictly less than a reader
    /// needs, because `Unknown` has three distinguishable causes and a reader
    /// that cannot tell them apart will invent one. This returns the cause.
    ///
    /// # UNRANKED IS NOT ZERO, and the payload is shaped so it cannot become
    /// zero
    ///
    /// [`Slack::Unknown`] means *the absence of evidence*. Zero pressure means
    /// *this account is exactly out of share with a full window left* — the
    /// busiest possible reading. They are opposite facts and the wire shape
    /// keeps them apart by construction: an unranked account's `pressure` field
    /// is JSON `null`, never a number, and it carries `ranked: false` plus a
    /// machine-readable `unranked_reason`. A client that renders `null` as a
    /// blank is correct; a client that renders it as `0` has to have written
    /// `?? 0` on purpose, and `unranked_reason` is there so it never wants to.
    /// `tests/accounts_pressure.rs::an_unranked_account_never_renders_as_zero`
    /// forces this by asserting the field is `Value::Null` and NOT `0.0`.
    ///
    /// # DISABLED, and why it is a reason rather than a number
    ///
    /// A disabled account ([`PoolShare::Disabled`]) is UNRANKED with reason
    /// `disabled`, and the division below never runs for it — there is no
    /// denominator, so there is nothing to guard against. It keeps every reading
    /// this function gathered on the way: the operator asked to be able to SEE a
    /// disabled account, and an account that vanished from this payload is how he
    /// loses track of one. `disabled` also OVERRIDES the five measurement reasons
    /// ([`PressureReading::excluded_over`]), because it is the one a human can act
    /// on.
    ///
    /// # UNAVAILABLE, and why a MEANINGFUL number still had to go
    ///
    /// An account inside an observed usage-limit window gets the same treatment,
    /// for a different reason. Nothing is wrong with its arithmetic: it is at
    /// 100% against a 0.9 share with minutes to its reset, so the numerator is
    /// negative, the denominator is shrinking, and the required burn rate
    /// diverges. The operator watched it read −49.63 and reported it — *"let's
    /// exclude it from the scaled pressure mechanism while it's unavailable. It
    /// makes the others not make sense because it distorts the scale so much"* —
    /// because three usable accounts sitting between 0.92 and 0.99 cannot be
    /// told apart on a display that also has to fit −56.
    ///
    /// So a barred account is UNRANKED with reason `unavailable` and carries
    /// `limit_until` so a reader can say WHEN rather than only WHY. Unlike the
    /// disabled case the exclusion is applied at the END rather than as an early
    /// return, because there is no zero to keep out of the arithmetic here —
    /// only a number not worth reporting. See [`UnrankedReason::Unavailable`].
    pub fn pressure_reading(&self, account: &str, now: DateTime<Utc>) -> PressureReading {
        let share = self.pool_share(account);
        // Read ONCE, at the top, so every exit below reports the same fact — and
        // so the disabled state is established before any reading is consulted, not
        // inferred from one.
        let disabled = share.is_disabled();
        // ── AVAILABILITY, THE THIRD QUESTION, ESTABLISHED THE SAME WAY ──
        //
        // `effective_limit_until` and NOT the raw `limit_until` column: a window
        // stamped against a login this label no longer points at is not a bar,
        // and reading the raw column here would resurrect the defect that
        // stranded a thread for five and a half hours on 2026-08-02.
        //
        // A read failure is NOT a bar, the same direction `pool_share` takes for
        // a disable and for the same reason: an exclusion may only ever come
        // from a value actually read (CON-5).
        //
        // **THE EXPIRY FILTER THAT USED TO BE THE SECOND LINE OF THIS
        // EXPRESSION IS GONE, AND NOTHING CHANGED HERE.** This site was the one
        // reader in the crate that got expiry right — it compared against `now`
        // itself, which is why `pressure.unavailable` read `false` for
        // `alice-example-com` all four days its four-day-old window was being
        // published to the board. It got it right by DUPLICATING the predicate,
        // and the duplicate is what let the accessor stay wrong: every other
        // reader trusted the accessor and only this one did not have to. Expiry
        // now lives in `effective_limit_until` with staleness, where there is
        // exactly one of it.
        let now_s = now.to_rfc3339();
        let state = self.store.account_state(account).unwrap_or(None);
        let limit_until = state
            .as_ref()
            .and_then(|st| st.effective_limit_until(&now_s).map(str::to_string));
        let unavailable = limit_until.is_some();
        // ── AND THE SECOND QUESTION ABOUT THE SAME COLUMN, WHICH USED TO BE
        //    ANSWERED BY THE BOOLEAN ABOVE ────────────────────────────────────
        //
        // > *"note that the threads tui should display pressure on accounts even
        // > if they are at the session limit. They should only not display
        // > pressure if they are at the weekly limit."*
        //
        // `unavailable` answers "may work be PLACED here"; this answers "is the
        // rank still worth showing". They are not the same question and they had
        // been the same boolean — so a session bar, which lifts within five
        // hours, blanked the number exactly as a weekly one did and the board had
        // nothing to render at any axis. The axis is on the row this function
        // already reads, one field along from the instant; the rule and the
        // verdict for each of the five values it holds are on
        // `AccountState::limit_hides_pressure`.
        //
        // **IT CHANGES NOTHING ABOUT PLACEMENT, AND IT CANNOT.** The bar's
        // enforcement is not here: `ThreadManager::barred_accounts` (built from
        // `accounts::unavailable_accounts`, which asks `AccountState::available`)
        // is handed to admission as `PlacementRequest::unavailable`, and `admit`
        // drops those accounts from the candidate set before `place` ranks
        // anything. So a session-barred account that now carries a pressure
        // number is still never a candidate for that number to order. `unavailable`
        // below is untouched and still reports the bar.
        let pressure_hidden = state
            .as_ref()
            .is_some_and(|st| st.limit_hides_pressure(&now_s));
        // THE PRECEDENCE, DECIDED ONCE. Disabled over unavailable over the five
        // measurement reasons — see `UnrankedReason::Unavailable`. Every exit
        // below passes this same value to `excluded_over`, so no exit can
        // disagree about which of two true facts is the one to report.
        //
        // It reads `pressure_hidden`, not `unavailable`: `unranked_reason` means
        // "why is there no pressure number", and for a session-barred account the
        // bar is no longer the reason — so naming it would be a false answer, and
        // whatever the real reason is (no reading, a rolled window) is the true
        // one and the one he can act on.
        let excluded = if disabled {
            Some(UnrankedReason::Disabled)
        } else if pressure_hidden {
            Some(UnrankedReason::Unavailable)
        } else {
            None
        };
        let mut out = PressureReading {
            account: account.to_string(),
            // THE QUANTITY, not the availability. On a disabled account this is
            // still the share he lent — independent of the disable since
            // 2026-09-20 — and `disabled` beside it is the availability. A
            // reader that wants "is this account in the pool" reads the boolean;
            // one that wants "how much of it did he lend" reads this.
            share: share.share(),
            disabled,
            unavailable,
            limit_until,
            // Nothing has been read yet, so the honest answer is "no, as far as
            // anything known here goes". Set below, once and only from a real
            // observation — see the field's own docstring on why this is a
            // positive claim rather than a default.
            over_share: false,
            value: None,
            pressure: None,
            unranked_reason: Some(UnrankedReason::NoReading),
            observed_pct: None,
            observed_at: None,
            resets_at: None,
            reset_basis: None,
        };
        let obs = match self
            .store
            .recent_observations(account, Axis::WeeklyAll.as_str(), 1)
        {
            Ok(v) => v.into_iter().next(),
            Err(e) => {
                tracing::warn!(%account, error = %e, "weekly observation read failed; ranking pressure as unknown");
                out.unranked_reason = Some(UnrankedReason::ReadFailed);
                return out.excluded_over(excluded);
            }
        };
        let Some(obs) = obs else {
            return out.excluded_over(excluded);
        };
        out.observed_pct = Some(obs.pct);
        out.observed_at = Some(obs.observed_at.clone());
        let Some(reset) = self.effective_reset_of(account, Axis::WeeklyAll, &obs, now) else {
            out.unranked_reason = Some(UnrankedReason::NoResetWindow);
            return out.excluded_over(excluded);
        };
        out.resets_at = Some(reset.at.to_rfc3339());
        out.reset_basis = Some(reset.basis);
        if reset.at <= now {
            // The window rolled and the gauge has not caught up. A stale window
            // would divide by a negative remainder, so it is not ranked — and
            // saying WHICH of the three it is lets the interface say "the
            // reading is stale" instead of "no data", which are different
            // things to a human deciding whether to hit refresh.
            out.unranked_reason = Some(UnrankedReason::WindowRolled);
            return out.excluded_over(excluded);
        }
        // ── THE GUARD, AND IT IS STRUCTURAL RATHER THAN NUMERIC ──
        //
        // A disabled account leaves here BEFORE the rate is computed. Not clamped
        // afterwards, not floored, not caught by the finiteness check at the
        // bottom — which, note, WOULD have caught it: `(0.0 − 0.5) / 0.0` is
        // −∞ and `Slack::value` rejects non-finite readings, so the wire would
        // have looked right while a divide-by-zero sat one branch away and
        // reported itself as `not_finite`. That is the failure this task was
        // told to make impossible, so the number is never formed at all.
        let Some(lent) = share.lent() else {
            return out.excluded_over(Some(UnrankedReason::Disabled));
        };
        // The weekly used fraction. The rate itself — numerator, denominator,
        // the ∞ region and the 0.95 cap — is [`pressure_value`]'s alone (R9.PR1).
        let consumed = (obs.pct / 100.0).max(0.0);
        // ── CONDITION ONE OF THE VISIBILITY RULE, SPELLED THE WAY HE SAID IT ──
        //
        // *"detect when weekly > share"*. STRICTLY greater, and not
        // [`Self::over_share`]'s `>=`: at exactly the share the rate is exactly
        // `0.0`, which is a finite, orderable, perfectly honest reading ("this
        // account has spent precisely what it was lent") and nothing about it
        // diverges. `over_share` is `>=` because it gates ADMISSION, where
        // "exactly at the share" must already refuse — a different question
        // with a different boundary, which is why this does not call it.
        //
        // It was once arithmetically identical to `pressure < 0.0` and was
        // deliberately NOT written that way: the condition is a fact about the
        // ACCOUNT (it spent more than it was lent), not a property of a derived
        // number. R9.PR1 proved the point — the rate now measures room to
        // `min(share, 0.95)`, so the two differ, and this still follows the
        // fact and not the formula.
        out.over_share = consumed > lent;
        // **THE ONE PRESSURE FUNCTION** (R9.PR1). Placement and the accounts
        // shape's per-email field both read `out.value`; `out.pressure` is its
        // finite projection for the legacy wire row. A NaN (zero cap) is `None`
        // and reported `not_finite`.
        out.value = pressure_value(pressure_cap(lent), consumed, reset.at, now);
        out.pressure = out.value.and_then(PressureValue::finite);
        out.unranked_reason = out.value.is_none().then_some(UnrankedReason::NotFinite);
        // **NO `disabled` GUARD ON THIS LINE, AND THAT IS DELIBERATE.**
        //
        // It was here once, on the reasoning that every exit should look the
        // same. Mutation testing killed that reasoning: with a second guard on
        // this line, replacing the `share.lent()` guard above with a raw zero
        // produced IDENTICAL output — `−∞` fails the finiteness check, the
        // reason is then overwritten to `disabled`, and the divide-by-zero became
        // unobservable from any test. A redundant guard that hides the failure
        // of the load-bearing one is worse than no guard: the `lent()` check is
        // the single thing keeping a zero out of this arithmetic, and it is left
        // alone up there so that removing it is visible.
        //
        // **THAT IS WHY THE EXIT BELOW PASSES `unavailable` AND NOT `excluded`**
        // (2026-09-20). `excluded` is `Some(Disabled)` for a disabled account,
        // so handing it to the final exit would re-create the exact masking this
        // paragraph is about — a deleted `lent()` guard would be laundered into
        // a `disabled` reason on the way out. Each exclusion is applied where it
        // is load-bearing and nowhere else.
        debug_assert!(
            !disabled,
            "a disabled account must have left through the `lent()` guard above; \
             reaching the rate arithmetic means the exclusion is broken"
        );
        // ── AND THE ONE EXIT THAT COMPUTED A NUMBER ──
        //
        // `unavailable` is applied HERE rather than as an early return, and the
        // asymmetry with `disabled` is deliberate. A disabled account has no
        // denominator, so its exclusion is STRUCTURAL: the number must never be
        // formed. A limit-barred account has a perfectly good denominator and
        // the arithmetic above is sound — the number it produces is
        // MEANINGLESS, not dangerous. Letting it run and discarding the result
        // costs one division and keeps every reading gathered on the way, which
        // is what makes a barred account still legible on the board it was
        // wrecking.
        //
        // It is written as `unavailable.then_some(..)` rather than as `excluded`
        // for the reason in the paragraph above: `excluded` carries `Disabled`
        // too, and a disabled account must not be able to reach a reason through
        // this line. The PRECEDENCE is still decided once — a disabled account
        // that is also barred leaves through the `lent()` guard reporting
        // `disabled`, which is the order `excluded` declares.
        //
        // **THE MUTATION.** Delete this line and the −56 comes back: the row
        // reports `ranked: true` and a divergent number, and the three healthy
        // accounts between 0.92 and 0.99 become indistinguishable again.
        //
        // ── AND THE TAIL LADDER (2026-09-22) ─────────────────────────────────
        //
        // A SECOND tail exclusion joined `unavailable` here, so the single
        // `then_some` became a ladder. It is written out rather than folded
        // into the `excluded` value decided at the top for the reason the
        // paragraphs above give: `excluded` carries `Disabled`, and a disabled
        // account must not be able to reach a reason through this line. Only
        // the two TAIL reasons appear below.
        //
        // The PRECEDENCE is still decided in exactly one place — the doc on
        // `UnrankedReason::OverShareSettled` — and the order below is that
        // order: `Unavailable` first, because a bar is the stronger statement
        // and carries a WHEN. `Disabled` is not here because a disabled account
        // left through the `lent()` guard long before this line, which is the
        // ladder at the top being obeyed rather than re-litigated.
        //
        // **WHY `OverShareSettled` CANNOT BE DECIDED AT THE TOP**, where the
        // other two are: it needs `obs.pct` and a live window, neither of which
        // exists until here. That is also why it cannot conflict with any of
        // the five measurement reasons — every one of those is an exit taken
        // because an observation or a window is MISSING, and this line is only
        // reached when both are present. Its place below them is vacuous, by
        // construction.
        let settled_overage = out.over_share && self.nothing_running_on(account);
        // **THE SECOND CONDITION IS LOAD-BEARING. DO NOT DROP IT.**
        //
        // `out.over_share` alone would be cheaper, would pass a careless test,
        // and would be WRONG — it would hide the negative pressure of an
        // over-share account that still has a thread executing on it, which is
        // the one case the operator explicitly asked to keep seeing: *"If it
        // went over I do want to see negative pressure while the last thread is
        // still running. That's honest."* The overage exists precisely because
        // that thread was not evicted early; hiding the number would hide a
        // live fact about a turn still burning.
        //
        // **THE MUTATION.** Replace `settled_overage` with `out.over_share` and
        // `an_over_share_account_with_a_thread_still_running_keeps_its_negative_pressure`
        // goes red.
        //
        // ── AND THE AXIS SPLIT REACHES HERE TOO (2026-10-06) ─────────────────
        //
        // **THIS IS THE SITE THAT MATTERS FOR THE OPERATOR'S ASK**, because it is
        // the only exit that computed a number to suppress. It reads
        // `pressure_hidden` rather than `unavailable`, exactly as the ladder at
        // the top now does — a latent copy of a predicate that has just been
        // split is the next defect, and this file has two copies of this ladder
        // by design (the top one for the five measurement exits, this one for the
        // exit that has a number). They must move together, and a session-barred
        // account keeping its number here while the top still blanked its REASON
        // would publish `pressure: 0.94` beside `unranked_reason: "unavailable"`.
        let tail = if pressure_hidden {
            Some(UnrankedReason::Unavailable)
        } else if settled_overage {
            Some(UnrankedReason::OverShareSettled)
        } else {
            None
        };
        out.excluded_over(tail)
    }

    /// **THE WIRE SHAPE OF PRESSURE** — one builder for the `accounts.pressure`
    /// WS arm and the `threads accounts pressure --json` CLI verb, for the same
    /// no-drift reason as [`Self::pool_share_rows`].
    ///
    /// Accounts are returned in the pool's own order, NOT sorted by pressure:
    /// the ranker consults pressure as one of several criteria behind a
    /// deadband ([`PRESSURE_DEADBAND`]), so a list sorted by this number alone
    /// would read as "the placement order" and would be wrong. `rank` is
    /// supplied separately and only over the accounts that HAVE a reading —
    /// an unranked account has no rank, which is the honest answer and is why
    /// the field is null rather than last place.
    pub fn pressure_rows(&self, accounts: &[String], now: DateTime<Utc>) -> Value {
        // **THE F7 RULE, STATED AND OBEYED.** A typed row reaches the wire
        // through `serde_json::to_value` on BOTH surfaces, never through
        // `to_string(&typed)` on one and `to_value` on the other. `serde_json`
        // is built without `preserve_order`, so a `Value` object is a
        // `BTreeMap` with sorted keys while a struct serialised directly keeps
        // DECLARATION order — and `tests/ws_protocol.rs` compares CLI stdout
        // BYTES to `serde_json::to_string(&ws)`. Going through `to_value` on
        // both sides is what keeps those bytes equal across this lift.
        serde_json::to_value(self.pressure_table(accounts, now)).unwrap_or(Value::Null)
    }

    /// **THE TYPED PRESSURE TABLE — P15's one implementation.**
    ///
    /// The operator reversed rev 2's "pressure is derived; the shape holds the
    /// inputs": **pressure is PRODUCED.** The producer computes it, stamps it,
    /// and emits it as `Fact::Pressure`; no consumer holds the formula. That
    /// reversal creates a second caller for what was a query-only builder, and
    /// a second caller is exactly how two answers to one question get born —
    /// so the `serde_json::Value` shape was lifted into a type that both the
    /// query and the fact are projections of.
    ///
    /// The serialisation is byte-for-byte what `json!` produced before the
    /// lift: same key names, same value types, `axis` deliberately repeated on
    /// every row as well as at the top, and `rank` a bare number or `null`
    /// rather than a nested object. `tests/ws_protocol.rs::
    /// pressure_is_byte_identical_to_the_cli` is the guard — and it did not
    /// exist before this change, which the critique's F7 flagged: P15's
    /// "answers byte-identically" was a claim the suite would not have caught
    /// breaking.
    pub fn pressure_table(&self, accounts: &[String], now: DateTime<Utc>) -> PressureTable {
        let readings: Vec<PressureReading> = accounts
            .iter()
            .map(|a| self.pressure_reading(a, now))
            .collect();

        // Rank ONLY the ranked. Higher pressure = more required burn rate = the
        // account the pool most needs to spend, so rank 1 is the highest value.
        let mut ranked: Vec<(&str, f64)> = readings
            .iter()
            .filter_map(|r| r.value.map(|v| (r.account.as_str(), v.rank_key())))
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let rank_of: std::collections::HashMap<&str, usize> = ranked
            .iter()
            .enumerate()
            .map(|(i, (n, _))| (*n, i + 1))
            .collect();

        let rows: Vec<PressureRow> = readings
            .iter()
            .map(|r| PressureRow {
                account: r.account.clone(),
                ranked: r.value.is_some(),
                // NULL when unranked. Never 0.0. See `pressure_reading`.
                // Also NULL when ∞ — then `infinite` says so (R9.PR1).
                pressure: r.pressure,
                infinite: r.value.is_some_and(PressureValue::is_infinite),
                rank: rank_of.get(r.account.as_str()).copied(),
                unranked_reason: r.unranked_reason.map(|u| u.as_str()),
                // DISABLED IS ITS OWN FIELD. A client must never have to
                // infer it from `share == 0`, and it must never be confused
                // with a bar: a barred account is `unavailable`/`dead` on
                // `accounts.status`, is the system reporting a failure, and
                // clears itself. This is the operator's own choice and
                // clears only when he reverses it.
                disabled: r.disabled,
                // UNAVAILABLE IS ITS OWN FIELD TOO, beside `disabled` and for
                // the same reason: three questions — is it in the pool, how
                // much of it is lent, can it serve a turn right now — with
                // three different remedies. A client that collapsed the first
                // and the third would offer "enable this account" for a bar an
                // enable cannot lift.
                unavailable: r.unavailable,
                limit_until: r.limit_until.clone(),
                // AND THE OVERAGE IS ITS OWN FIELD, for the same reason again.
                // Note it rides on rows that are still RANKED: an over-share
                // account with a thread executing on it keeps its negative
                // pressure, by the operator's explicit ruling, and this is what
                // lets a reader say WHY that number is negative without
                // re-deriving it from `observed_pct` and `share`.
                over_share: r.over_share,
                share: r.share,
                axis: Axis::WeeklyAll.as_str(),
                observed_pct: r.observed_pct,
                observed_at: r.observed_at.clone(),
                resets_at: r.resets_at.clone(),
                reset_basis: r.reset_basis.map(|b| b.as_str()),
            })
            .collect();

        PressureTable {
            axis: Axis::WeeklyAll.as_str(),
            now: now.to_rfc3339(),
            deadband: PRESSURE_DEADBAND,
            ranked_count: ranked.len(),
            accounts: rows,
        }
    }

    /// **THE WIRE SHAPE OF THE SHARE — one builder, both surfaces.**
    ///
    /// `Store::set_pool_share` existed from 2026-08-27 and had NO caller
    /// anywhere except a test: the operator could not set a per-account share
    /// from the CLI, from the WS, or from the accounts interface. The number was
    /// reachable only by hand-editing SQLite. This is the read half of closing
    /// that hole, and it is built HERE rather than in either surface so that
    /// `threads accounts pool-share --json` and the `accounts.pool_share.get` WS
    /// arm cannot drift — the same argument `accounts::activity_json` settles
    /// for `accounts.status`.
    ///
    /// Every field the operator needs to understand WHY the effective number is
    /// what it is:
    ///
    /// * `share` — the EFFECTIVE fraction, exactly what [`Self::pool_share`]
    ///   hands the ranker and the clamp. This is the only number that decides
    ///   anything.
    /// * `override` — the `pool_shares` STORE ROW, or `null`. Present means the
    ///   operator set it at runtime and it WINS.
    /// * `declared` — the `share` field on the `pool.json` entry, or `null`.
    /// * `source` — which of the three the effective number came from
    ///   (`override` | `declared` | `unset`), so a reader never has to
    ///   re-derive the precedence rule client-side.
    ///
    /// An absent row and a deliberate `1.0` stay distinguishable, which is the
    /// property `Store::pool_share` goes out of its way to preserve and which a
    /// payload reporting only the effective number would have thrown away.
    ///
    /// * `disabled` — **INDEPENDENT OF EVERY NUMBER ABOVE** since 2026-09-20.
    ///   The operator took this account out of the pool; `share` beside it is
    ///   still what he lent, unchanged by the act and unchanged when he reverses
    ///   it. A client reads the boolean for availability and the number for
    ///   weight, and neither has to be inferred from the other.
    /// * `disabled_at` — when he did it, or `null`. For the record only;
    ///   nothing expires on it.
    ///
    /// **`restore_to` and `prior` ARE GONE**, and their absence is the change.
    /// They answered "what would an enable put this back at" because a disable
    /// used to overwrite the share with zero. It does not, so the answer is
    /// `share` and there is nothing else to say. See the note where
    /// `restore_share` used to live.
    pub fn pool_share_rows(&self, accounts: &[String]) -> Value {
        let rows: Vec<Value> = accounts.iter().map(|n| self.pool_share_row(n)).collect();
        json!({ "shares": rows })
    }

    /// **ONE pool-share row — the single implementation behind
    /// `accounts.pool_share.get/set`, `accounts.disable/enable`, the CLI, and
    /// `Fact::PoolShare`.**
    ///
    /// Same reason as [`Self::gauge_axis_row`]: P3 and P10 must emit precisely
    /// what the query answers, and the only way to guarantee that is for there
    /// to be one function. `pool_share_rows` is now this in a loop, so the wire
    /// bytes it produced before this refactor are the bytes it produces after.
    pub fn pool_share_row(&self, name: &str) -> Value {
        // `(0.0, 1.0]`, the same bound the setter enforces. It was widened to
        // include zero while a zero was a real value the operator could set; it
        // is not one any more, so a `0.0` still on disk is a pre-migration
        // relic and must NOT be reported as an override he made.
        let sane = |s: f64| s.is_finite() && s > 0.0 && s <= 1.0;
        {
            {
                let stored = self.store.pool_share(name).ok().flatten();
                let over = stored.filter(|s| sane(*s));
                let declared = self.pool().declared_share(name).filter(|s| sane(*s));
                let note = self
                    .store
                    .pool_share_note(name)
                    .ok()
                    .flatten();
                let source = if over.is_some() {
                    "override"
                } else if declared.is_some() {
                    "declared"
                } else {
                    // "unset", not "default": the operator has said nothing
                    // about this account, so the whole of it is offered. The
                    // word is also deliberately NOT `default` — that name means
                    // something specific and unwelcome in this crate, and
                    // `tests/default_out_of_placement.rs` bans the literal from
                    // executable code here. It caught this string, and the
                    // rename is the better payload anyway.
                    "unset"
                };
                let effective = self.pool_share(name);
                json!({
                    "account": name,
                    "share": effective.share(),
                    "disabled": effective.is_disabled(),
                    // WHEN he took it out, for the record. `null` when it is in
                    // the pool, and the two are maintained together by
                    // `Store::set_account_disabled` — there is no state in
                    // which one is set and the other is not.
                    "disabled_at": self.store.account_disabled_at(name).ok().flatten(),
                    // WHY, in his own words, and absent means absent. Kept apart
                    // from the share's own `note` below on purpose: they are
                    // notes about two different acts, and merging them would
                    // make "no longer a paid account" read as an explanation of
                    // a number he set months earlier.
                    "disabled_note": self.store.account_disabled_note(name).ok().flatten(),
                    "override": over,
                    "declared": declared,
                    "note": note,
                    "source": source,
                })
            }
        }
    }

    /// **THE LANDING CAP: how much of the room it was offered this account has
    /// left, as a fraction of that room.**
    ///
    /// ```text
    /// capacity = (ceiling − consumed) ÷ ceiling,  ceiling = min(session threshold, pool share)
    /// ```
    ///
    /// **A CAPACITY, not a bit**, and that is the entire change. Thresholds in
    /// this crate are a BINARY veto: `admit`'s loop compares the reading to the
    /// ceiling and the account is either fine or refused, with nothing in
    /// between. So placement is blind right up to the cliff and then falls off
    /// it. This grades the same line continuously — 1.0 untouched, falling
    /// smoothly, reaching 0.0 exactly where the veto fires — so the ranking
    /// steers away from a ceiling BEFORE arriving at it rather than discovering
    /// it at the edge.
    ///
    /// It does not replace the veto and it cannot. The veto still runs first
    /// and still refuses absolutely; this only orders accounts that already
    /// cleared it.
    ///
    /// # The SESSION axis, and why not the weekly one
    ///
    /// *Weekly wants exhaustion; session wants a soft landing.* Opposite signs,
    /// deliberately — so the two must not both be allowed to grade the same
    /// axis. Weekly belongs entirely to [`Self::pool_pressure`], which wants it
    /// spent OUT by its reset. Session belongs here, where the goal is to come
    /// down gently and not slam into a five-hour ceiling mid-turn. Letting the
    /// cap read the weekly axis too would have quietly re-ranked weekly a
    /// second time, ahead of pressure and WITHOUT the pool share dividing —
    /// undoing the denominator this wave exists to install.
    ///
    /// # The share bounds the ceiling
    ///
    /// The pool's landing room is the smaller of the threshold the operator set
    /// and the share he offered: a login that gave 10% of itself has 10% to land
    /// in, whatever the threshold says. Normalised by that same ceiling, so —
    /// exactly as in [`Self::pool_pressure`] — a 10% account is not punished for
    /// being small. An untouched 10% account and an untouched 100% account both
    /// read 1.0.
    ///
    /// Silent ([`Slack::Unknown`]) when the session gauge has never reported for
    /// this account: absence of evidence is not evidence of room.
    ///
    /// A DISABLED account has no ceiling to grade, because it has no room: it left
    /// the candidate set before this criterion was reached, and `Unknown` is the
    /// honest answer for something that is not being ranked. Guarded explicitly
    /// rather than left to fall out of `ceiling > 0.0` — that test would have
    /// produced the right value for the wrong reason, and a later edit to the
    /// ceiling formula would have silently taken it away.
    fn landing_capacity(&self, account: &str, th: &Thresholds) -> Slack {
        let Some(share) = self.pool_share(account).lent() else {
            return Slack::Unknown;
        };
        // An UNGATED session axis still has a ceiling — the share itself. That
        // is what makes this rankable for the ungated thread, which is every
        // AUI thread.
        let ceiling = th
            .session
            .filter(|s| s.is_finite())
            .map(|s| s / 100.0)
            .unwrap_or(1.0)
            .min(share);
        if !(ceiling > 0.0) {
            // A ceiling of zero admits nothing, so there is no capacity to
            // grade and the veto in `admit` is already the whole answer.
            return Slack::Unknown;
        }
        match self.observed_value(account, Axis::Session) {
            Ok(Some(v)) => {
                let consumed = (v / 100.0).max(0.0);
                Slack::Known((ceiling - consumed) / ceiling)
            }
            Ok(None) => Slack::Unknown,
            Err(e) => {
                tracing::warn!(%account, error = %e, "session gauge read failed; ranking the landing cap as unknown");
                Slack::Unknown
            }
        }
    }

    /// **Is this account's prompt cache still warm?** — the momentum clock.
    ///
    /// True only if a turn ENDED on this account within
    /// [`MOMENTUM_WINDOW_SECS`]. Momentum exists for exactly one reason —
    /// prompt-cache cost — so once the cache is cold the criterion is measuring
    /// nothing and stops voting.
    ///
    /// **Momentum decides WHICH account, never WHETHER to place.** It is a
    /// tiebreak among candidates that have ALREADY cleared the veto and the
    /// cap, it runs after both, and it can only ever choose between accounts
    /// every gate has already passed — so there is no reading of it that lets a
    /// warm cache drag a turn onto an account that would otherwise have been
    /// refused.
    ///
    /// An unreadable ledger reads as COLD. The failure direction matters: cold
    /// costs a cache miss, whereas defaulting to warm would pin a thread to an
    /// account on the strength of a fact the store could not confirm.
    fn momentum_is_warm(&self, account: &str, now: DateTime<Utc>) -> bool {
        let last = match self.store.last_turn_end_on(account) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(%account, error = %e, "turn history unreadable; momentum reads cold");
                None
            }
        };
        parse_rfc3339_utc(last.as_deref())
            .is_some_and(|t| (now - t).num_seconds().abs() <= MOMENTUM_WINDOW_SECS)
    }

    /// How many threads currently sit on each account — the direct, inspectable
    /// expression of "spread", and correct on the FIRST placement rather than
    /// in expectation the way randomisation would be.
    fn placement_counts(&self) -> HashMap<String, usize> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        match self.store.list_threads(PLACEMENT_THREAD_SCAN) {
            Ok(rows) => {
                for r in rows {
                    if let Some(a) = r.account {
                        *counts.entry(a).or_insert(0) += 1;
                    }
                }
            }
            // A tie-break INPUT, not a gate. Losing it costs spread, not
            // correctness, so it must not fail closed an admission that every
            // actual gate already allowed — the remaining criteria are still
            // deterministic without it.
            Err(e) => tracing::warn!(
                error = %e,
                "placement: thread census unavailable; spreading by cursor and name only"
            ),
        }
        counts
    }

    fn placement_cursor(&self) -> usize {
        self.store
            .get_watermark(PLACEMENT_CURSOR)
            .ok()
            .flatten()
            .unwrap_or(0) as usize
    }

    fn advance_placement_cursor(&self, cursor: usize) {
        let _ = self
            .store
            .set_watermark(PLACEMENT_CURSOR, "", (cursor as u64).wrapping_add(1));
    }

    // ── the admission gate (CON-4, DEC-8) ────────────────────────────────

    /// Decide whether a thread may take a turn right now.
    ///
    /// Order: the KEY ITSELF (is it disabled, does it even exist — Q-34), then
    /// SCOPE (which accounts may this budget draw on at all — round-7 item 6),
    /// then THRESHOLDS (they need no calibration, so they are the part that
    /// bites immediately) and AVAILABILITY ([`AdmissionRequest::unavailable`] —
    /// the caller's observed 429s, which no gauge measures), then the BUDGET
    /// meters. Accounts are a POOL (DEC-5/DEC-10): a thread is admissible if
    /// SOME in-scope account clears every gauge it is gated on and can actually
    /// answer.
    ///
    /// **This is the only account ranking in the crate.** The 429 retry path
    /// used to carry its own (`accounts::select_failover`, deleted): it ranked
    /// the registry itself, so it bypassed scope, the budget trip and the
    /// placement deadbands, and it split ownership of "where does this thread
    /// run" across two files. It now stamps the limit window and asks HERE,
    /// with `unavailable` carrying what it learned.
    ///
    /// **EVERY THREAD IS RE-RANKED ON EVERY ADMISSION**, and a thread with a
    /// conversation is not exempt. Moving is not free — an account switch
    /// restarts the conversation unless the thread already has a session on the
    /// target — so round-7 item 7 binds the FIELD rather than the decision: a
    /// thread WITH history is ranked over the accounts it already has a session
    /// on plus wherever it is legally sitting, and parks only when none of
    /// those is admissible. A FRESH thread is ranked over everything placeable.
    /// Neither is ever pinned in place merely because where it sits is still
    /// legal; that was the stickiness defect and it is gone.
    pub fn admit(&self, req: &AdmissionRequest<'_>) -> Result<Admission> {
        // ── scope (round-7 item 6) ──
        let cfg = match req.budget_key {
            Some(k) => self.get_config(k)?,
            None => None,
        };

        // ── the key itself (Q-34, msg #751) ──
        // A key that is DISABLED denies, full stop — before scope, before any
        // gauge is read, and no matter how much headroom the meters show. And a
        // key with no config at all is not a free pass either: it cannot be
        // metered, so nothing runs on it. Both PARK (CON-4), so the work waits
        // for the key to come back rather than being lost.
        if let Some(key) = req.budget_key {
            match &cfg {
                Some(c) if c.disabled => {
                    return Ok(Admission::Park {
                        cause: ParkCause::Disabled,
                        reason: format!(
                            "budget {key} is disabled{}: every turn on this key is denied \
                             until it is enabled again (its spend and history stay readable \
                             meanwhile)",
                            c.disabled_at
                                .as_deref()
                                .map(|t| format!(" (since {t})"))
                                .unwrap_or_default()
                        ),
                        budget_key: Some(key.to_string()),
                    })
                }
                // THE CRATE'S OWN DEFAULT KEY IS EXEMPT FROM THIS PARK, and
                // only this key (INTENT 72b).
                //
                // `unknown_key` exists to catch a key the OPERATOR named that
                // does not exist — a typo, or a client that never called
                // `budgets.set` — so that nothing runs unmetered on it. Every
                // one of those is still caught, unchanged.
                //
                // But `aui:default` is not a key anybody typed: it is the key
                // this crate puts on a thread that named none. Parking on its
                // absence would mean the crate stops a thread because its own
                // seed row has not been written yet — which turned a change
                // meant to alter accounting into one that halts the fleet, and
                // did so with a park reason (`unknown_key`) that told the
                // operator to go create something he never asked for. A thread
                // holding only the inherited default therefore falls through and
                // is gated by its thresholds exactly as an unkeyed thread was.
                None if key != DEFAULT_BUDGET_KEY => {
                    return Ok(Admission::Park {
                        cause: ParkCause::UnknownKey,
                        reason: format!(
                            "budget {key} has no config: a thread never runs UNMETERED on a \
                             key that does not exist — create it (`budgets/<key>/set`) or unkey the \
                             thread (`threads/<id>/budget_assign` with a null key)"
                        ),
                        budget_key: Some(key.to_string()),
                    })
                }
                None | Some(_) => {}
            }
        }

        // No key ⇒ no budget ⇒ thresholds gate over the whole pool. A key with
        // a config narrows the pool to its EXPLICIT scope.
        let candidates: Vec<Placeable> = match &cfg {
            Some(c) => c.scoped_accounts(req.accounts),
            None => req.accounts.to_vec(),
        };
        if candidates.is_empty() {
            // Names, not `Placeable`'s derived Debug — this string is read by
            // the operator, and "the pool is [Placeable(\"x\")]" helps nobody.
            let pool_names: Vec<&str> = req.accounts.iter().map(|a| a.name()).collect();
            let reason = match &cfg {
                Some(c) => format!(
                    "budget {} is scoped to {:?}, none of which is a placeable account ({:?})",
                    c.key, c.accounts, pool_names
                ),
                // "Placeable", not "registered": the machine default IS
                // registered and always will be (`ensure_default` runs every
                // boot), it simply is not somewhere threads may run (CON-27).
                // Saying "registered" here would send the operator to add an
                // account he can already see in `accounts list`.
                None => "no placeable account is registered (the machine default is not one; \
                         threads runs on the accounts that have their own config dir)"
                    .to_string(),
            };
            return Ok(Admission::Park {
                cause: ParkCause::Scope,
                reason,
                budget_key: req.budget_key.map(str::to_string),
            });
        }

        // ── availability + thresholds ──
        let configured = req.thresholds.configured();
        let mut clear: Vec<Placeable> = Vec::new();
        let mut blocked_detail: Vec<String> = Vec::new();
        // Did any account fall out on a GAUGE CEILING, as opposed to merely
        // being unusable? It decides which cause an empty field parks under:
        // `unavailable` is the one a caller acts on by waiting for a reset,
        // where `threshold` is the one it acts on by moving the ceiling.
        let mut any_threshold = false;
        for account in &candidates {
            let mut over: Option<String> = None;
            for (axis, limit) in &configured {
                // Unobserved: never gate on a gauge we cannot read (CON-5).
                // This loop no longer accumulates a headroom score either —
                // an unreadable gauge used to add its full threshold, which is
                // arithmetically a reading of 0%, so the gate quietly invented
                // an empty account. Ranking lives in `place` now, and it can
                // tell "unknown" from "empty".
                // A store fault is NOT an absence: `observed_value` returns an
                // Err now, `?` carries it out of `admit`, and `runtime.rs`
                // fails CLOSED on it (audit F2).
                //
                // Failing a direct reading, the DUAL-CURRENCY adapter (msg
                // #876) will convert a reading taken in the other denomination
                // through this account's estimator — so a limit written in
                // dollars is admissible against a percent gauge, and vice
                // versa. Still CON-5: with nothing observed in EITHER
                // denomination there is no reading and the gate stands aside.
                let (v, via) = match self.observed_value(account, *axis)? {
                    Some(v) => (v, None),
                    None => match self.cross_denomination_reading(account, *axis)? {
                        Some((v, from)) => (v, Some(from)),
                        None => continue,
                    },
                };
                if v >= *limit {
                    any_threshold = true;
                }
                if v >= *limit && over.is_none() {
                    // Report BOTH sides at their real precision. Rounding the
                    // configured value made a threshold of 3.5 print as
                    // "session at 4% ≥ threshold 4%", which reads as an
                    // off-by-one to anyone auditing the ledger — the gate
                    // looks like it fired at equality when it actually fired
                    // half a point above the line.
                    // A converted reading says so, and names what it was
                    // converted from — a dollar figure nobody's cash gauge ever
                    // reported must never read as if it had been observed.
                    let source = match via {
                        Some(from) => format!(" (converted from {})", from.as_str()),
                        None => String::new(),
                    };
                    over = Some(match axis.unit() {
                        gauge::Unit::Dollars => format!(
                            "{account}: {} at ${}{source} ≥ threshold ${}",
                            axis.as_str(),
                            exact(v),
                            exact(*limit)
                        ),
                        gauge::Unit::Percent => format!(
                            "{account}: {} at {}%{source} ≥ threshold {}%",
                            axis.as_str(),
                            exact(v),
                            exact(*limit)
                        ),
                    });
                }
            }
            // THE CALLER'S OBSERVED FACT. A gauge cannot see this: an account
            // inside a usage-limit window cannot serve a turn at all, whatever
            // its meters read. Applied after the thresholds so a real ceiling
            // still gets to be the reason it names — that text is what tells an
            // operator which number to move.
            if over.is_none() && req.unavailable.iter().any(|u| u.as_str() == account.name()) {
                over = Some(format!(
                    "{account}: cannot serve a turn right now (inside an observed usage-limit \
                     window, or another label for a login that is)"
                ));
            }
            match over {
                Some(detail) => blocked_detail.push(detail),
                None => clear.push(account.clone()),
            }
        }
        if clear.is_empty() {
            // Name the cause the caller can actually act on. Nothing crossed a
            // ceiling ⇒ this is not a threshold park at all, it is "there is no
            // usable account", which clears on a reset rather than on a config
            // change — and which failover treats as a reason to look elsewhere.
            let (cause, reason) = if any_threshold {
                (
                    ParkCause::Threshold,
                    format!(
                        "every in-scope account is over a threshold ({})",
                        blocked_detail.join("; ")
                    ),
                )
            } else {
                (
                    ParkCause::Unavailable,
                    format!(
                        "no in-scope account is usable right now ({})",
                        blocked_detail.join("; ")
                    ),
                )
            };
            // ── STARVED BY THE SCOPE, NOT BY THE ACCOUNTS ──
            //
            // Registering an account does not join it to any budget, so a
            // freshly added login belongs to no scope and is never a candidate
            // — it is not barred, it is simply not looked at. The park above
            // then reads "no in-scope account is usable", which is true and
            // which sends the operator to wait for a reset that will not help,
            // because the account he just added is sitting there available.
            //
            // On 2026-08-06 that cost thirteen hours and roughly a thousand
            // refused attempts per thread. The accounts were fine; the scope
            // was three names long and the fourth had just been added. Nothing
            // said so, on any surface, at any point.
            //
            // So SAY IT, by name. This changes no placement decision — an
            // out-of-scope account is still not a candidate, because narrowing
            // a budget to a scope is a thing the operator is allowed to mean.
            // It only ends the silence, which is the part that was indefensible.
            let starved: Vec<&str> = req
                .accounts
                .iter()
                .filter(|a| !candidates.iter().any(|c| c.name() == a.name()))
                .map(Placeable::name)
                .filter(|n| {
                    !req.unavailable.iter().any(|u| u == n)
                        && !req.unhealthy.iter().any(|u| u == n)
                })
                .collect();
            let reason = if starved.is_empty() {
                reason
            } else {
                let key = req.budget_key.unwrap_or(DEFAULT_BUDGET_KEY);
                tracing::warn!(
                    budget = key, available_but_out_of_scope = ?starved,
                    "a thread is parked with usable accounts it is not allowed to see"
                );
                format!(
                    "{reason} — but {} IS available and is NOT in budget {key}'s scope; \
                     nothing here will clear until it is added (`budgets/<key>/set` with an \
                     `accounts` list that includes it) or the thread is unkeyed",
                    starved.join(", ")
                )
            };
            return Ok(Admission::Park {
                cause,
                reason,
                budget_key: req.budget_key.map(str::to_string),
            });
        }

        // ── which account, and at what cost to the conversation ──
        // Note this can be FALSE simply because the thread sits somewhere no
        // longer placeable — a thread on the machine default is exactly that
        // (CON-27). That is the intended reading: "you may not stay here", and
        // the continuity/failover branches below carry it off by porting its
        // conversation rather than by reassigning its column.
        let on_admissible = clear.iter().any(|a| a.name() == req.current_account);
        // What placement may CHOOSE from — `clear` minus the accounts nothing
        // may be moved onto. Two reasons an account is here-but-not-a-target,
        // and they share a shape deliberately:
        //
        // * a DEAD LOGIN. A thread already on one is still "admissible where it
        //   is" and its next successful turn still clears the flag; it simply
        //   stops being a destination.
        // * THE SHARE CLAMP (2026-08-27, `Self::over_share`). The account has
        //   spent past what the operator lent the pool. *"Overage we've
        //   determined is okay while the current workload is finishing. But
        //   then it needs to be like hard clamped — not available for placement
        //   as well."* Staying in `clear` is what "the current workload
        //   finishes" MEANS here: `on_admissible` is computed over `clear`, so
        //   a thread with history on the account is not moved off it, and a
        //   turn in flight is never re-admitted at all. Leaving `targets` is
        //   what "not available for placement" means.
        //
        // The clamp is applied where `unhealthy` is applied, and not one line
        // earlier, precisely so it cannot become a per-account THRESHOLD veto —
        // the thing the operator struck. It never touches `clear`, so it can
        // never be the reason a thread is refused where it already sits.
        // * THE DISABLE (2026-09-08, `Self::is_disabled`). The operator set a true
        //   0% share, which takes the account out of placement selection
        //   entirely. It shares the here-but-not-a-target shape for the same
        //   reason the clamp does — a thread already sitting there is not
        //   stranded, it is re-ranked over a field the disabled account has been subtracted
        //   from and moves off by itself if anything will take it — and it is
        //   tracked in its OWN list because disabling and clamping are different
        //   facts with different remedies, and the park below has to be able to
        //   say which happened.
        let mut clamped: Vec<&str> = Vec::new();
        let mut disabled: Vec<&str> = Vec::new();
        let mut targets: Vec<Placeable> = Vec::new();
        for a in &clear {
            if req.unhealthy.iter().any(|u| u.as_str() == a.name()) {
                continue;
            }
            // Ordered ahead of the clamp, exactly as in `place`, so the two read
            // the same way — but the ORDER is not what makes a disabled account
            // safe from being called over-share. [`PoolShare::lent`] is: it
            // returns `None` for a disabled account, which is `over_share`'s
            // first line and its early `false`. Swapping these two statements
            // changes no answer, and that was measured rather than assumed.
            //
            // The comment this replaces claimed the order was the guarantee
            // ("never reaches the gauge read that would decide it"). It is not,
            // and the distinction matters in the direction a future editor will
            // meet it: delete the ordering and nothing breaks, so the ordering is
            // not load-bearing and must not be defended as though it were; change
            // `lent()` to hand out a share for a disabled account and BOTH call
            // sites break at once, whatever order they are written in. One
            // derivation, and it lives in the type.
            if self.is_disabled(a.name()) {
                disabled.push(a.name());
                continue;
            }
            if self.over_share(a.name())? {
                clamped.push(a.name());
                continue;
            }
            targets.push(a.clone());
        }
        let switch_to = if req.pinned {
            // A pin is a pin: it is never re-placed, only honoured or waited
            // for. (Checked FIRST now — placement below is entitled to move a
            // fresh thread that is already somewhere admissible, and a pinned
            // thread must never be one of them.)
            if on_admissible {
                None
            } else {
                return Ok(Admission::Park {
                    // A pinned thread never moves, so this cause is pure
                    // reporting — but "threshold" on an account that merely
                    // took a 429 sends the operator to raise a ceiling that
                    // was never crossed.
                    cause: if !any_threshold
                        && req.unavailable.iter().any(|u| u == req.current_account)
                    {
                        ParkCause::Unavailable
                    } else {
                        ParkCause::Threshold
                    },
                    reason: format!(
                        "pinned account {} is not admissible ({})",
                        req.current_account,
                        detail_or(&blocked_detail, "out of this budget's account scope")
                    ),
                    budget_key: req.budget_key.map(str::to_string),
                });
            }
        } else if req.history_accounts.is_empty() {
            // A FRESH thread has no conversation to lose, so it is PLACED —
            // whether or not the account it happens to be sitting on is
            // admissible. Selection is not the same thing as gating: a thread
            // with nothing to enforce (no key, no thresholds) still has to be
            // put somewhere, and "wherever it already was" is how every thread
            // ended up on the alphabetically first account and stayed there.
            // `prefer` keeps an exact tie exactly where it is, so this costs
            // no churn when there is nothing to choose.
            let best = self.place(&PlacementRequest {
                thresholds: req.thresholds,
                candidates: &targets,
                prefer: on_admissible.then_some(req.current_account),
                // Empty by the branch condition, and passed explicitly rather
                // than defaulted: a fresh thread holds no conversation
                // anywhere, so the continuity tie-break has nothing to say and
                // must be seen to have nothing to say.
                sessions: req.history_accounts,
                demand: req.demand,
            })?;
            // ── THE CLAMP'S TEETH ──
            //
            // With no target left, `best` is `None` and `switch_to` would be
            // `None` — which reads as "stay where you are". For a fresh thread
            // ALREADY SITTING on the clamped account that is a placement onto
            // it by omission, and it is exactly the case the operator named:
            // *"if all the other accounts are blocked, we don't want that
            // account to even be available for placement."* So it PARKS.
            //
            // Gated on `clamped` being non-empty, so this changes nothing about
            // the pre-existing empty-target case (every candidate unhealthy),
            // which still admits in place — a thread on a dead login keeps
            // running there, because its next success is what clears the flag.
            //
            // THE DISABLE IS NAMED SEPARATELY AND NAMED FIRST. Disabling and
            // clamp both empty `targets`, and reporting a disable as a clamp would
            // tell the operator to wait for a weekly reset that will never
            // change anything — the account is out because he took it out. When
            // both are in play the cause is the disable, because that is the one
            // he can clear right now, and the clamped accounts are still named
            // so the reason stays complete.
            if best.is_none() && !(disabled.is_empty() && clamped.is_empty()) {
                let clamp_note = if clamped.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; past the share it was lent (clears on the weekly reset, or on \
                         lending more): {}",
                        clamped.join(", ")
                    )
                };
                let (cause, reason) = if disabled.is_empty() {
                    (
                        ParkCause::Share,
                        format!(
                            "every account that could take this thread has spent past the share \
                             it was lent ({}) — work already running there finishes, but nothing \
                             new is placed on it. Raise a share (`pool.json`, or the \
                             `pool_shares` row) or wait for the weekly window to reset",
                            clamped.join(", ")
                        ),
                    )
                } else {
                    (
                        ParkCause::AccountDisabled,
                        format!(
                            "every account that could take this thread is DISABLED ({}) — you \
                             took it out of the pool, which removes it from placement selection \
                             entirely whatever share it is lent. This is not a limit and not a \
                             failure, and nothing will lift it on a timer: call `accounts/<account>/enable` \
                             (or the accounts interface) to put it back, at the share it still \
                             has{clamp_note}",
                            disabled.join(", ")
                        ),
                    )
                };
                return Ok(Admission::Park {
                    cause,
                    reason,
                    budget_key: req.budget_key.map(str::to_string),
                });
            }
            best.filter(|b| b.name() != req.current_account)
        } else {
            // ── A THREAD WITH A CONVERSATION IS RE-RANKED, EVERY TURN ──
            //
            // This branch used to be preceded by `else if on_admissible {
            // None }` — an unconditional stickiness gate that returned before
            // the ranker was consulted, so a thread was placed once when it was
            // fresh and then never again while its account remained merely
            // LEGAL. Pressure, the cap, spread and the whole cascade were
            // computed for it and then not applied. That gate is REMOVED. It
            // was never a decision the operator made: it arrived as a judgement
            // call during the pool rework, and it contradicts the rule he has
            // stated three times —
            //
            // > *"every single time we start a new turn… it needs to go through
            // > placement every single time. We can't have this momentum… The
            // > only time that momentum should play in is if a thread is getting
            // > started back up within five minutes of when it ended. And the
            // > reason why is because of caching."*
            //
            // and again, on the test for the bug: *"if it always sends to the
            // same account, there's a bug."* Momentum survives as exactly what
            // he described and no more — a TIE-BREAK inside `place`, gated on
            // [`Self::momentum_is_warm`], which decides WHICH account and never
            // WHETHER to place. It is not this branch's business.
            //
            // Legality is now a FLOOR, not a short circuit: the ranker runs
            // first, and `on_admissible` is what the thread falls back to when
            // the ranker has nowhere better to offer.
            //
            // ── THE CONTINUITY FLOOR, AND IT IS UNCHANGED ──
            //
            // Does this thread hold a conversation on any account it could be
            // placed on? Bare names off `thread_accounts`, intersected with
            // TARGETS (placeable) — which is why history may stay a `String`
            // slice: an account named in history that is not placeable simply
            // matches nothing here.
            //
            // This is the SAME predicate the old `known.is_empty()` expressed,
            // preserved exactly, because one thing downstream depends on it and
            // must keep depending on it. A thread that may NOT stay where it is
            // and holds no session on any target is being moved off an account
            // it is not allowed to remain on — and that move must go through
            // `ThreadManager::failover_target`, whose contract carries
            // `port_must_succeed`: a transcript that cannot be carried ABORTS
            // the move rather than forking the conversation across two accounts
            // that may both serve turns. The `switch_to` path below ports
            // best-effort, which is right for a thread that is merely being
            // placed better and wrong for one that is being evicted. A thread
            // whose only history is the machine default is exactly this case
            // (DEC-56, CON-27), and it still parks here and still leaves through
            // failover.
            let holds_a_target = targets
                .iter()
                .any(|a| req.history_accounts.iter().any(|h| h.as_str() == a.name()));
            if !on_admissible && !holds_a_target {
                // The clamp and the disable are NAMED here rather than folded
                // into "blocked": they are the two causes on this list the
                // operator clears himself, and by opposite acts. One derivation,
                // shared with the backstop below — see [`withdrawn_note`].
                let withdrawn = withdrawn_note(&clamped, &disabled);
                return Ok(Admission::Park {
                    cause: ParkCause::Continuity,
                    reason: format!(
                        "this thread may not run where it is and holds no conversation on any \
                         account it could be placed on, so moving it is failover's to do — it \
                         carries the transcript or it does not move (history: {:?}; admissible \
                         now: {:?}; blocked: {}{withdrawn})",
                        req.history_accounts,
                        clear,
                        detail_or(&blocked_detail, "out of this budget's account scope")
                    ),
                    budget_key: req.budget_key.map(str::to_string),
                });
            }

            // ── WHAT IT MAY CHOOSE FROM: EVERY TARGET ──
            //
            // This used to be `targets` INTERSECTED WITH HISTORY, on the
            // reading that "an account switch restarts the conversation unless
            // the thread already holds a session on the target". That reading
            // is false and the intersection is REMOVED.
            //
            // It is false because `ThreadManager::move_thread` ports the
            // transcript onto whatever target it is handed — DEC-56 makes
            // carrying the conversation the rule on every path, not a property
            // of lucky destinations — and then derives `resumable` from the
            // port's own outcome. A destination already named in history buys
            // nothing the port does not also buy. INTENT 83 struck the cost
            // argument in the operator's own words: *"the justification you
            // used that it's going to be more expensive to move it… that's not
            // true because you're gonna move it beforehand anyway."*
            //
            // And the intersection cost exactly what he reported on
            // 2026-09-18: history is written when a turn ENDS on an account, so
            // an account with no rows could never be ranked, and it could only
            // get rows by being placed. *"If an account is added to the pool,
            // there's a condition where it can never be used."* For a thread
            // that already exists the loop never opened — `bob-example-com`
            // sat at 1% weekly, measured, ranked first on pressure, and was not
            // a candidate for a single one of his 1,292 threads.
            //
            // Continuity is not lost, it is MOVED to where it can be weighed:
            // `PlacementKey::has_session`, the first tie-break inside `place`,
            // strictly below every measured criterion and strictly above
            // spread. An account this thread has never run on now wins only by
            // beating every session-bearing candidate on deaths, the cap,
            // pressure or cash.
            // ── THE ONE STATE THE FIELD IS *NOT* WIDENED IN ──
            //
            // A thread whose current account is admissible but is NOT a target
            // is sitting on a login that is dead, DISABLED (a true 0% share) or
            // past the share it was lent. All three share one ruling, and it is
            // the operator's: *"Overage we've determined is okay while the
            // current workload is finishing… then it needs to be hard clamped —
            // not available for placement as well."* Work already running there
            // FINISHES; what stops is new placement onto it.
            //
            // Widening the field here would convert that into an EVICTION — the
            // thread would be ranked over a field its own account had been
            // subtracted from, find any other target, and be ported off the
            // moment he set a share to zero.
            //
            // H179 (2026-10-16): for a DISABLED or DEAD account this no longer
            // arises from the runtime — `ThreadManager::account_unavailability`
            // hands both to this gate in `unavailable`, so they are never in
            // `clear` and the thread is failed over before its next turn
            // (`account_disable.rs::a_thread_with_history_only_on_the_disabled_
            // account_moves_before_its_next_turn`). What remains here is the
            // share clamp, which the operator ruled finishes out.
            //
            // So the widening applies where the thread is genuinely placeable —
            // which is the state the operator's report is about (a healthy
            // account merely running low) — and this state keeps exactly the
            // field it had. Such a thread still reaches a new account by the
            // route it always had: when it can no longer stay, it parks and
            // failover re-ranks it over every target.
            let finishing_out = on_admissible
                && !targets.iter().any(|a| a.name() == req.current_account);
            let mut field: Vec<Placeable> = if finishing_out {
                targets
                    .iter()
                    .filter(|a| req.history_accounts.iter().any(|h| h.as_str() == a.name()))
                    .cloned()
                    .collect()
            } else {
                targets.clone()
            };
            // WHERE IT IS NOW, when staying is legal — and it is NOT redundant
            // with `targets`. History is written when a turn ENDS, so a thread
            // that was switched last turn sits on an account its history does
            // not yet name; and the `finishing_out` account above is in `clear`
            // and in no target list at all. Drawn from `clear` rather than
            // `targets` for exactly that reason, and `place` re-applies the
            // disable and the share clamp to this list anyway — so a clamped
            // current account is dropped there, the field is judged without it,
            // and if nothing else can take the thread the `on_admissible`
            // fallback below leaves it exactly where it is.
            if on_admissible && !field.iter().any(|a| a.name() == req.current_account) {
                if let Some(cur) = clear.iter().find(|a| a.name() == req.current_account) {
                    field.push(cur.clone());
                }
            }
            match self.place(&PlacementRequest {
                thresholds: req.thresholds,
                candidates: &field,
                // Not a new placement: it must not turn the cursor. `prefer`
                // also carries the two things that keep re-ranking from
                // becoming churn — the spread discount (a thread never counts
                // against the account it is already on) and the anti-churn
                // tie-break — and it is what makes momentum reachable at all,
                // since momentum requires `is_current`.
                prefer: Some(req.current_account),
                // THE CONVERSATION, as a ranking input rather than a filter.
                sessions: req.history_accounts,
                demand: req.demand,
            })? {
                // `None` here reads as "stay", exactly as on the fresh path.
                Some(a) => Some(a).filter(|b| b.name() != req.current_account),
                // THE FALLBACK, and the reason the removed gate is not simply
                // reinstated one line lower: the ranker found nowhere this
                // thread may go, and where it already sits is still legal. It
                // stays and RUNS. Only a thread with nowhere legal at all parks
                // for continuity — which is the same condition, and the same
                // park, as before this branch was merged.
                None if on_admissible => None,
                // THE BACKSTOP. The continuity floor above has already parked
                // every thread that may not stay and has no session on a
                // target, and `on_admissible` catches every thread that may
                // stay — so reaching here means the ranker emptied a field the
                // floor had just proved non-empty (`place` re-applies the
                // disable and the share clamp to the current account it was
                // handed). It is kept, and it parks rather than admitting,
                // because the alternative to a park that cannot happen is an
                // admission that cannot be justified.
                None => {
                    // The same suffix the floor above carries, from the same
                    // derivation. This branch is the one no test reaches, which
                    // is exactly why it must not own a second copy of the text:
                    // a copy here could never be forced red. See
                    // [`withdrawn_note`].
                    let withdrawn = withdrawn_note(&clamped, &disabled);
                    return Ok(Admission::Park {
                        cause: ParkCause::Continuity,
                        reason: format!(
                            "every account this thread could be placed on was withdrawn by the \
                             ranker itself (history: {:?}; admissible now: {:?}; blocked: \
                             {}{withdrawn})",
                            req.history_accounts,
                            clear,
                            detail_or(&blocked_detail, "out of this budget's account scope")
                        ),
                        budget_key: req.budget_key.map(str::to_string),
                    })
                }
            }
        };

        // ── budget ──
        if let Some(key) = req.budget_key {
            // Metering reads the pool by NAME. Safe to hand it the placeable
            // pool: `check_trip` answers yes/no, never an account, so widening
            // its parameter cannot leak a destination.
            if let Some(trip) = self.check_trip(key, req.accounts)? {
                return Ok(Admission::Park {
                    cause: ParkCause::Budget,
                    reason: trip.reason(),
                    budget_key: Some(key.to_string()),
                });
            }
        }

        Ok(Admission::Admit { switch_to })
    }
}

fn detail_or(details: &[String], fallback: &str) -> String {
    if details.is_empty() {
        fallback.to_string()
    } else {
        details.join("; ")
    }
}

/// **THE LOGIN this account label answers to** — the key every per-login table
/// in this crate is written under.
///
/// `accounts.identity` when the fingerprint is readable, else the literal
/// `label:<name>`, which is honest about being a label and cannot collide with a
/// real fingerprint (a fingerprint has no colon).
///
/// # Why this function exists at all
///
/// Its docstring was already in this file, on 2026-10-06, sitting above
/// [`Budgets::pool_share`] and fused onto that function's own doc — an orphan
/// left behind when the accessor it described was deleted and its body was
/// hand-inlined into [`Budgets::record_period_estimate`]. The orphan asserted
/// the factoring-out as a live guarantee:
///
/// > "…factored out here so the two tables can never drift into keying on
/// > different things — which is the specific failure `limit_estimate_periods`
/// > was built to document, with the live store still holding a `default` row
/// > and a `owner-gmail` row that are one login disagreeing with itself by
/// > 42%."
///
/// Nothing prevented that drift. There was ONE copy of the rule, so nothing had
/// drifted yet either — which is the dangerous state, not the safe one: a reader
/// was told the invariant was structural, and the next per-login table would have
/// been written against a promise the code had stopped keeping. The function is
/// restored so the sentence is true again. It changes no behaviour and has one
/// caller, which is the point.
fn login_key(account: &str, identity: Option<&str>) -> String {
    identity
        .map(str::to_string)
        .unwrap_or_else(|| format!("label:{account}"))
}

/// **THE TWO WITHDRAWALS, NAMED AND NAMED APART** — the suffix every continuity
/// park carries when the target set came back empty.
///
/// Both halves say an account was taken out of `targets` while staying in
/// `clear`, and they are kept apart because their REMEDIES are opposite: a clamp
/// clears on the weekly reset with no action from the operator, and a disable
/// clears only when he reverses it. Reporting a disable as a clamp would send him
/// to wait for a reset that will never change anything.
///
/// # Why this is a function and not two more copies
///
/// It was written out by hand at both continuity parks — the floor
/// (`!on_admissible && !holds_a_target`) and the ranker backstop — sixteen lines
/// byte for byte identical, comments included. The backstop is reachable only
/// when `place` withdraws a candidate the floor had just proved non-empty, which
/// no test in this crate reaches, so that copy could never be forced red and
/// could never learn a correction made to the other one. That is the same shape
/// as the four hand-inlined predicates `d94eaa0` deleted on 2026-10-06, and it is
/// here for the same reason: one derivation, one place to fix.
///
/// Empty in, empty out — a park with neither withdrawal in play says nothing
/// extra, which is what both copies did and what the callers still rely on.
fn withdrawn_note(clamped: &[&str], disabled: &[&str]) -> String {
    let mut note = String::new();
    if !clamped.is_empty() {
        note.push_str(&format!("; past its lent share: {}", clamped.join(", ")));
    }
    if !disabled.is_empty() {
        note.push_str(&format!(
            "; DISABLED by you (share 0%): {}",
            disabled.join(", ")
        ));
    }
    note
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **THE FIXTURE PUTS THIS WHOLE MODULE ON THE CLOCK SEAM.**
    ///
    /// Seven tests in here measure an INTERVAL between two clock reads — the
    /// overlap-window method's whole premise is that an execution has width —
    /// and under an optimized build the two reads landed in the same microsecond
    /// and the window collapsed to `start == end`. At `290113e` the release
    /// `--lib` suite was red on **every one of ten runs**, with a different
    /// subset of those seven each time; `cargo test` was green only because debug
    /// codegen is slow enough for the clock to tick between two back-to-back
    /// calls. Ten debug runs of the same suite at the same commit: 0 red.
    ///
    /// The seven, so the record is one grep rather than an archaeology:
    ///
    /// * `calibration_is_learned_from_a_real_observation_pair`
    /// * `spend_too_small_to_move_the_gauge_yields_no_sample`
    /// * `a_poll_during_an_in_flight_execution_never_closes_a_bracket`
    /// * `a_window_rollover_never_produces_a_calibration_sample`
    /// * `jittered_polls_still_calibrate`
    /// * `idle_movement_between_two_of_our_own_executions_blocks_the_next_window`
    /// * `overlapping_executions_merge_into_one_window_and_calibrate_once`
    ///
    /// Only the first two were named in the original report, because a SINGLE
    /// release run shows only the subset that lost its race that time. The class
    /// is the seven, and that is why the fix belongs in the fixture.
    ///
    /// The clock is installed HERE, by the fixture, and not line by line in each
    /// test, because a requirement every one of eighty call sites has to remember
    /// is not a requirement — the next calibration test anybody writes is on the
    /// seam without being told. `clock::testing::ticking()` needs no guard to hold:
    /// it is the real clock with one property added, that it may not repeat. A
    /// test that wants the COLLISION back asks for it explicitly with
    /// `clock::testing::freeze_at`, which nests and restores — see
    /// `a_zero_width_execution_window_is_refused_and_writes_no_estimate`.
    fn svc() -> Budgets {
        crate::clock::testing::ticking();
        Budgets::new(Store::open_in_memory().unwrap())
    }

    /// A config scoped to `accounts` (round-7 item 6: never implicit).
    fn cfg(key: &str, accounts: &[&str]) -> BudgetConfig {
        let mut c = BudgetConfig::new(key);
        c.accounts = accounts.iter().map(|s| s.to_string()).collect();
        c
    }

    /// A test account that is IN THE POOL — i.e. one that names the directory
    /// its login is stored in.
    ///
    /// There is no shortcut here on purpose. A `Placeable` can only be minted
    /// from a [`crate::pool::PoolEntry`], and an entry cannot exist without a
    /// login directory, so even a test has to say what makes this account a
    /// pool member. That is the guarantee being tested: if a `From<&str>`
    /// existed for convenience, CON-27 would be advisory.
    fn placeable(name: &str) -> Placeable {
        crate::pool::PoolEntry::claude_account(
            name,
            &format!("/tmp/threads-test-accounts/{name}"),
        )
        .expect("an entry that names its login directory is a pool member")
        .placeable()
    }

    /// The PLACEMENT pool: what the gate may choose from.
    fn pool(names: &[&str]) -> Vec<Placeable> {
        names.iter().map(|s| placeable(s)).collect()
    }

    /// Bare account NAMES — history, exclusion sets, METERING, and anything
    /// else that observes accounts rather than choosing one.
    ///
    /// These used to be spelled `"default"`, on the reasoning that *"spend on
    /// the machine default is still real spend and still has to be counted"*.
    /// That reasoning is overruled (2026-08-27): the machine default is not a
    /// pool member, the daemon does not meter it, and a test that spells it
    /// that way is the module rehearsing a concept it is not allowed to hold.
    /// The name is now `aa-metered`, which is what these tests always actually
    /// needed — an arbitrary account name on the OBSERVING side of the line.
    ///
    /// The line itself is unchanged and is the point: `snapshot` / `check_trip`
    /// are generic over the element type precisely so that counting an account
    /// never yields a place to run. Reading a meter cannot return an account;
    /// only `admit` and `place` can, and those take `Placeable`.
    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// The live defect this tolerance was written for (observed 2026-07-29):
    /// consecutive `/usage` polls render the SAME reset with different
    /// sub-second fractions, and claude's own prose crosses minute boundaries.
    /// String equality read both as a rollover and calibrated nothing.
    #[test]
    fn reset_jitter_is_the_same_window_but_a_rollover_is_not() {
        // Sub-second jitter — the exact pair captured from two live polls.
        assert!(gauge::same_window(
            Some("2026-07-29T23:50:00.000039+00:00"),
            Some("2026-07-29T23:50:00.000430+00:00"),
        ));
        // A whole-minute swing (`1pm` vs `12:59pm` from the same account).
        assert!(gauge::same_window(
            Some("2026-08-01T18:00:00+00:00"),
            Some("2026-08-01T17:59:00+00:00"),
        ));
        // Offsets are normalized, not compared as text.
        assert!(gauge::same_window(
            Some("2026-08-01T18:00:00+00:00"),
            Some("2026-08-01T13:00:00-05:00"),
        ));
        // A REAL rollover must still refuse: the session pool is 5 hours and
        // the weekly pools 7 days — both far outside the tolerance.
        assert!(!gauge::same_window(
            Some("2026-07-29T23:50:00+00:00"),
            Some("2026-07-30T04:50:00+00:00"),
        ));
        assert!(!gauge::same_window(
            Some("2026-08-01T18:00:00+00:00"),
            Some("2026-08-08T18:00:00+00:00"),
        ));
        // Even a modest 10-minute move is a different window, not jitter.
        assert!(!gauge::same_window(
            Some("2026-08-01T18:00:00+00:00"),
            Some("2026-08-01T18:10:00+00:00"),
        ));
        // Absence: both unknown pairs; one-sided never does.
        assert!(gauge::same_window(None, None));
        assert!(!gauge::same_window(Some("2026-08-01T18:00:00+00:00"), None));
        assert!(!gauge::same_window(None, Some("2026-08-01T18:00:00+00:00")));
        // Unparseable falls back to string equality rather than pairing blind.
        assert!(gauge::same_window(Some("not-a-date"), Some("not-a-date")));
        assert!(!gauge::same_window(Some("not-a-date"), Some("other")));
    }

    /// End to end through `observe`: a percentage that RISES across two polls
    /// whose reset instants differ only by jitter must now yield a real
    /// `tokens_per_percent` sample. Before the tolerance this produced none.
    #[test]
    fn jittered_polls_still_calibrate() {
        let b = svc();
        // The SAME session window, reported 45 seconds apart — inside the
        // rollover tolerance, and (msg #690) inside the 5-hour session the
        // ledger rows below fall in.
        let base = crate::clock::now() + chrono::Duration::minutes(30);
        let (r1, r2) = (
            base.to_rfc3339(),
            (base + chrono::Duration::seconds(45)).to_rfc3339(),
        );
        b.observe("aa-metered", Axis::Session, 4.0, Some(r1), "usage_poll")
            .unwrap();
        // The overlap-window method needs an EXECUTION interval, not just
        // tokens on the ledger: `started_at` brackets it between the two
        // polls, exactly what `runtime.rs` stamps from `running_turns` live.
        b.store
            .record_usage(&crate::store::UsageRow {
                thread_id: "t1".into(),
                account: Some("aa-metered".into()),
                model: "claude-haiku-4-5".into(),
                input_tokens: 500_000,
                started_at: Some(crate::store::now_rfc3339()),
                ..Default::default()
            })
            .unwrap();
        b.observe("aa-metered", Axis::Session, 9.0, Some(r2), "usage_poll")
            .unwrap();
        let est = b
            .store
            .get_estimate("aa-metered", Axis::Session.as_str())
            .unwrap()
            .expect("a rising percent with tokens on the ledger must calibrate");
        assert_eq!(est.sample_count, 1);
        // 500_000 tokens across a 5-point rise = 100_000 tokens per percent.
        assert!((est.tokens_per_percent - 100_000.0).abs() < 1.0);
    }

    /// The reset instant a real poll would report on `axis`: inside the axis's
    /// own window, so the msg-#690 containment rule can place the window's
    /// start behind it. Relative to NOW, because the ledger rows these tests
    /// write are stamped now — a fixed literal would make the assertion
    /// depend on what time of day the suite runs.
    fn reset_for(axis: Axis) -> String {
        (crate::clock::now() + chrono::Duration::seconds(axis.window_secs() / 2)).to_rfc3339()
    }

    fn calibrate(b: &Budgets, account: &str, axis: Axis, tokens_per_pct: f64) {
        // Inject a calibration directly (the pairing path is covered by the
        // integration tests). `PRIOR_SAMPLES` samples, so the prior is fully
        // displaced and these tests divide by exactly the number they name —
        // the blend itself is exercised by the calibration tests below.
        b.store
            .upsert_estimate(&crate::store::EstimateRow {
                account: account.into(),
                axis: axis.as_str().into(),
                tokens_per_percent: tokens_per_pct,
                sample_count: gauge::PRIOR_SAMPLES as u64,
                last_obs_id: 0,
                updated_at: crate::store::now_rfc3339(),
            })
            .unwrap();
    }

    /// The common request shape: fresh thread, unpinned, no thresholds.
    fn req<'a>(
        key: Option<&'a str>,
        th: &'a Thresholds,
        accounts: &'a [Placeable],
        current: &'a str,
        history: &'a [String],
    ) -> AdmissionRequest<'a> {
        AdmissionRequest {
            budget_key: key,
            thresholds: th,
            accounts,
            current_account: current,
            pinned: false,
            history_accounts: history,
            unavailable: &[],
            unhealthy: &[],
            demand: Demand::default(),
        }
    }

    /// **The 2026-08-06 starvation, reproduced.**
    ///
    /// Three accounts in scope, every one of them inside a usage-limit window;
    /// a fourth registered and perfectly usable, but absent from the scope. The
    /// gate must still refuse — a scope is a thing the operator is allowed to
    /// mean — but it must name the account that would fix it, because the bare
    /// "no in-scope account is usable" sends him to wait for a reset instead.
    #[test]
    fn a_park_names_the_usable_account_the_scope_is_hiding() {
        let b = svc();
        let c = cfg("aui:default", &["old-a", "old-b", "old-c"]);
        b.set_config(&c).unwrap();
        let th = Thresholds::ungated();
        let accounts = pool(&["old-a", "old-b", "old-c", "fresh-login"]);
        let limited: Vec<String> = ["old-a", "old-b", "old-c"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut r = req(Some("aui:default"), &th, &accounts, "old-a", &[]);
        r.unavailable = &limited;

        match b.admit(&r).unwrap() {
            Admission::Park { cause, reason, .. } => {
                assert_eq!(cause, ParkCause::Unavailable);
                assert!(
                    reason.contains("fresh-login"),
                    "the park must name the account that would clear it: {reason}"
                );
                assert!(
                    reason.contains("budgets/<key>/set"),
                    "and say how to add it: {reason}"
                );
            }
            other => panic!("expected a park naming the out-of-scope account, got {other:?}"),
        }
    }

    /// The converse: when nothing usable is being hidden, the park says only
    /// what is true and does not invent an account to go add.
    #[test]
    fn a_park_stays_quiet_when_the_scope_hides_nothing_usable() {
        let b = svc();
        let c = cfg("aui:default", &["old-a", "old-b"]);
        b.set_config(&c).unwrap();
        let th = Thresholds::ungated();
        let accounts = pool(&["old-a", "old-b"]);
        let limited: Vec<String> = ["old-a", "old-b"].iter().map(|s| s.to_string()).collect();
        let mut r = req(Some("aui:default"), &th, &accounts, "old-a", &[]);
        r.unavailable = &limited;

        match b.admit(&r).unwrap() {
            Admission::Park { reason, .. } => assert!(
                !reason.contains("budgets/<key>/set"),
                "nothing is being hidden, so nothing to add: {reason}"
            ),
            other => panic!("expected a park, got {other:?}"),
        }
    }

    /// An out-of-scope account that is ALSO rate-limited or dead is not a fix,
    /// and naming it would send the operator to add something that cannot help.
    #[test]
    fn a_park_never_names_an_out_of_scope_account_that_is_itself_unusable() {
        let b = svc();
        let c = cfg("aui:default", &["old-a"]);
        b.set_config(&c).unwrap();
        let th = Thresholds::ungated();
        let accounts = pool(&["old-a", "also-limited", "dead-one"]);
        let limited: Vec<String> = ["old-a", "also-limited"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let dead: Vec<String> = vec!["dead-one".to_string()];
        let mut r = req(Some("aui:default"), &th, &accounts, "old-a", &[]);
        r.unavailable = &limited;
        r.unhealthy = &dead;

        match b.admit(&r).unwrap() {
            Admission::Park { reason, .. } => {
                assert!(!reason.contains("also-limited"), "{reason}");
                assert!(!reason.contains("dead-one"), "{reason}");
                assert!(!reason.contains("budgets/<key>/set"), "{reason}");
            }
            other => panic!("expected a park, got {other:?}"),
        }
    }

    // ── the shape of a budget ────────────────────────────────────────────

    #[test]
    fn no_key_means_no_budget() {
        let b = svc();
        let th = Thresholds::ungated();
        // CON-27: an ADMIT is a placement, so the pool names an account threads
        // owns. `default` cannot be built into a `Placeable` at all.
        let accounts = pool(&["acct-a"]);
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "acct-a", &[])).unwrap(),
            Admission::Admit { switch_to: None }
        );
    }

    // ── the prior, the blend and the adapters (msgs #876 / #1047 / #1055) ─

    /// SUPERSEDES `uncalibrated_axis_reports_calibrating_and_never_gates`.
    ///
    /// That test asserted the old refusal: an axis with no accepted sample
    /// reported `calibrating`, priced its spend at zero and never tripped. Msg
    /// #1055 replaced the refusal with a conservative prior, so the same
    /// scenario now GATES — and gates early, which is the direction the prior
    /// is chosen for.
    #[test]
    fn the_prior_gates_from_first_boot_and_errs_early() {
        let b = svc();
        let mut c = cfg("k", &["aa-metered"]);
        c.session_pct = Some(10.0);
        b.set_config(&c).unwrap();
        b.charge("k", "t1", "aa-metered", "claude-sonnet-5", 5_000_000, 1.0)
            .unwrap();

        let accounts = names(&["aa-metered"]);
        let snap = b.snapshot("k", &accounts).unwrap().unwrap();
        let m = snap.meter(Axis::Session);

        // Not a fabricated zero any more: 5M tokens at the 20k prior is 250%.
        assert_eq!(m.consumed_tokens, 5_000_000);
        assert!((m.consumed - 250.0).abs() < 1e-6, "{}", m.consumed);
        assert_eq!(m.status, MeterStatus::Exhausted);
        assert_eq!(snap.tripped_axis(), Some(Axis::Session));
        assert!(b.check_trip("k", &accounts).unwrap().is_some());
        // Nothing was spent to get here.
        assert!(b
            .store
            .get_estimate("aa-metered", Axis::Session.as_str())
            .unwrap()
            .is_none());

        // EARLY, never late: the prior is a lower bound on tokens-per-percent,
        // so whatever the truth turns out to be, the prior's reading of the
        // same spend is the larger one — it can only refuse sooner.
        let priced_by_prior = m.consumed;
        calibrate(&b, "aa-metered", Axis::Session, 1_000_000.0); // the real pool is bigger
        let measured = b
            .snapshot("k", &accounts)
            .unwrap()
            .unwrap()
            .meter(Axis::Session)
            .consumed;
        assert!(
            priced_by_prior > measured,
            "prior {priced_by_prior} must over-state measured {measured}"
        );
    }

    #[test]
    fn samples_shrink_the_prior_toward_measured_truth() {
        // The blend, at the three points that matter. The measured mean is 10x
        // the prior, so an un-shrunk first sample would move the estimate the
        // whole way — audit finding IS-50 — and the prior alone would never
        // move at all.
        let prior = gauge::PRIOR_TOKENS_PER_PERCENT;
        let mean = prior * 10.0;

        // No samples: exactly the prior.
        assert_eq!(gauge::blend_rate(prior, None), prior);
        assert_eq!(gauge::blend_rate(prior, Some((mean, 0))), prior);

        // Fully measured: exactly the mean, with no residual prior bias left
        // for a well-calibrated account to carry forever.
        let n = gauge::PRIOR_SAMPLES as u64;
        assert_eq!(gauge::blend_rate(prior, Some((mean, n))), mean);
        assert_eq!(gauge::blend_rate(prior, Some((mean, n + 50))), mean);

        // In between: strictly between, and monotone towards the measurement.
        let mut last = prior;
        for k in 1..n {
            let v = gauge::blend_rate(prior, Some((mean, k)));
            assert!(v > last && v < mean, "sample {k} gave {v}");
            last = v;
        }

        // IS-50 specifically: ONE sample no longer arms the meter by itself.
        // On the rate the meter multiplies by, it carries a quarter weight.
        let one = gauge::blend_rate(prior, Some((mean, 1)));
        let expected = 1.0 / ((3.0 / prior + 1.0 / mean) / 4.0);
        assert!((one - expected).abs() < 1e-9);
        assert!(one < prior * 1.4, "one sample moved the estimate to {one}");
    }

    #[test]
    fn the_blend_never_lands_on_the_less_conservative_side() {
        // Taking the weighted mean on the RECIPROCAL is a harmonic mean, which
        // never exceeds the arithmetic one — so the blend always errs towards
        // the smaller denominator, which is the earlier refusal.
        let prior = gauge::PRIOR_TOKENS_PER_PERCENT;
        for mean in [prior / 7.0, prior / 2.0, prior * 2.0, prior * 40.0] {
            for n in 1..gauge::PRIOR_SAMPLES as u64 {
                let w = gauge::prior_weight(n);
                let arithmetic = (w * prior + n as f64 * mean) / (w + n as f64);
                let blended = gauge::blend_rate(prior, Some((mean, n)));
                assert!(blended <= arithmetic + 1e-9, "{blended} > {arithmetic}");
                assert!(blended >= prior.min(mean) - 1e-9);
                assert!(blended <= prior.max(mean) + 1e-9);
            }
        }
    }

    #[test]
    fn measurement_memory_survives_a_restart() {
        // "Long-term memory of measurements so the code self-optimizes" (msg
        // #876): the accepted samples ride the durable store, so the estimate
        // a restart wakes up with is the one the last session earned — not the
        // prior all over again.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("threads.db");

        let (before, samples) = {
            let b = Budgets::new(Store::open(&path).unwrap());
            // Two accepted samples, as the overlap-window sampler would land
            // them: partly measured, so the value is the blend and not either
            // endpoint — a value the prior alone could never produce.
            for n in 1..=2u64 {
                b.store
                    .upsert_estimate(&crate::store::EstimateRow {
                        account: "aa-metered".into(),
                        axis: Axis::Session.as_str().into(),
                        tokens_per_percent: 250_000.0,
                        sample_count: n,
                        last_obs_id: n as i64,
                        updated_at: crate::store::now_rfc3339(),
                    })
                    .unwrap();
            }
            b.blended_tokens_per_percent("aa-metered", Axis::Session).unwrap()
        };
        assert_eq!(samples, 2);
        assert!(before > gauge::PRIOR_TOKENS_PER_PERCENT && before < 250_000.0);

        // A whole new Budgets over a reopened database — a daemon restart.
        let b2 = Budgets::new(Store::open(&path).unwrap());
        let (after, samples2) = b2
            .blended_tokens_per_percent("aa-metered", Axis::Session)
            .unwrap();
        assert_eq!(samples2, 2);
        assert!((after - before).abs() < 1e-9, "{after} != {before}");

        // And it only improves from there: a third sample moves it further
        // towards the measurement, never back towards the prior.
        b2.store
            .upsert_estimate(&crate::store::EstimateRow {
                account: "aa-metered".into(),
                axis: Axis::Session.as_str().into(),
                tokens_per_percent: 250_000.0,
                sample_count: 3,
                last_obs_id: 3,
                updated_at: crate::store::now_rfc3339(),
            })
            .unwrap();
        let (third, _) = b2
            .blended_tokens_per_percent("aa-metered", Axis::Session)
            .unwrap();
        assert!(third > after && third < 250_000.0);
    }

    #[test]
    fn a_fresh_account_inherits_the_fleets_tightest_measurement_as_its_prior() {
        // Ladder rung 2. Accounts usually share a plan, so a sibling's
        // hard-won calibration is better evidence than the raw constant — and
        // taking the TIGHTEST of them keeps the conservative direction.
        let b = svc();
        calibrate(&b, "a", Axis::Session, 400_000.0);
        calibrate(&b, "b", Axis::Session, 150_000.0);
        // A different AXIS is deliberately not consulted: the three pools are
        // not interchangeable, and 1 token per percent on fable must not make
        // every session meter on the fleet trip instantly.
        calibrate(&b, "a", Axis::WeeklyFable, 1.0);

        let (tpp, samples) = b.blended_tokens_per_percent("fresh", Axis::Session).unwrap();
        assert_eq!(samples, 0);
        assert_eq!(tpp, 150_000.0);
        assert_eq!(b.axis_prior("fresh", Axis::Session).unwrap(), 150_000.0);
        // Nothing has measured weekly-all anywhere, so that one falls to the
        // constant — rung 3.
        assert_eq!(
            b.axis_prior("fresh", Axis::WeeklyAll).unwrap(),
            gauge::PRIOR_TOKENS_PER_PERCENT
        );
        // An account's OWN measurement always outranks its siblings'.
        calibrate(&b, "fresh", Axis::Session, 900_000.0);
        assert_eq!(
            b.blended_tokens_per_percent("fresh", Axis::Session).unwrap().0,
            900_000.0
        );
    }

    #[test]
    fn the_adapter_prices_a_token_from_the_accounts_own_ledger() {
        // The dollars half of the measurement memory: the realised rate is
        // what this account actually paid, mixed models and cache hits and all,
        // and the conservative top-tier rate only until it has paid anything.
        let b = svc();
        assert_eq!(
            b.denominator("a", Axis::Session).unwrap().unwrap().usd_per_token,
            gauge::PRIOR_USD_PER_TOKEN
        );
        b.store
            .record_usage(&crate::store::UsageRow {
                thread_id: "t1".into(),
                account: Some("a".into()),
                model: "claude-haiku-4-5".into(),
                input_tokens: 1_000_000,
                cost_usd: 2.0,
                ..Default::default()
            })
            .unwrap();
        let d = b.denominator("a", Axis::Session).unwrap().unwrap();
        assert!((d.usd_per_token - 2e-6).abs() < 1e-12);
        assert!(d.usd_per_token < gauge::PRIOR_USD_PER_TOKEN);
        assert!(d.prior_only() && d.confidence() == 0.0);
    }

    #[test]
    fn a_dollars_threshold_gates_a_percent_reading_through_the_adapter() {
        // Msg #876's adapters doing gate work. No claude build emits a cash
        // `/usage` row, so a cash THRESHOLD had nothing to read and was inert
        // for ever. The percent gauge is readable, and one estimator converts
        // it: percent → tokens → dollars, same blend, same confidence.
        let b = svc();
        calibrate(&b, "a", Axis::Session, 100_000.0); // 1% = 100k tokens
        b.store
            .record_usage(&crate::store::UsageRow {
                thread_id: "t1".into(),
                account: Some("a".into()),
                model: "claude-fable-5".into(),
                input_tokens: 1_000_000,
                cost_usd: 10.0, // $1e-5 per token, realised
                ..Default::default()
            })
            .unwrap();
        // 40% of the session pool = 4M tokens = $40 equivalent.
        b.observe("a", Axis::Session, 40.0, None, "usage_poll").unwrap();
        assert_eq!(b.observed_value("a", Axis::Cash).unwrap(), None);

        let accounts = pool(&["a"]);
        let over = Thresholds {
            cash: Some(25.0),
            ..Default::default()
        };
        match b.admit(&req(None, &over, &accounts, "a", &[])).unwrap() {
            Admission::Park { cause, reason, .. } => {
                assert_eq!(cause, ParkCause::Threshold);
                assert!(reason.contains("$40"), "{reason}");
                // The reading must never read as if a cash gauge reported it.
                assert!(reason.contains("converted from session"), "{reason}");
            }
            other => panic!("expected a park, got {other:?}"),
        }

        // Under the line it admits, so this is a gate and not a blanket refusal.
        let under = Thresholds {
            cash: Some(60.0),
            ..Default::default()
        };
        assert_eq!(
            b.admit(&req(None, &under, &accounts, "a", &[])).unwrap(),
            Admission::Admit { switch_to: None }
        );

        // CON-5 still binds: nothing observed in EITHER denomination, no gate.
        let fresh = svc();
        assert_eq!(
            fresh
                .admit(&req(None, &over, &pool(&["z"]), "z", &[]))
                .unwrap(),
            Admission::Admit { switch_to: None }
        );
    }

    #[test]
    fn the_adapter_converts_all_three_denominations_both_ways() {
        let d = gauge::Denominator {
            tokens_per_percent: 100_000.0,
            usd_per_token: 1e-5,
            samples: 2,
        };
        use gauge::Denom::{Dollars, Percent, Tokens};
        let c = |v, f, t| d.convert(v, f, t).unwrap();
        let near = |a: f64, b: f64| assert!((a - b).abs() < 1e-9, "{a} != {b}");

        near(c(3.0, Percent, Tokens), 300_000.0);
        near(c(300_000.0, Tokens, Percent), 3.0);
        near(c(300_000.0, Tokens, Dollars), 3.0);
        near(c(3.0, Dollars, Tokens), 300_000.0);
        near(c(3.0, Percent, Dollars), 3.0);
        near(c(3.0, Dollars, Percent), 3.0);
        near(c(7.5, Percent, Percent), 7.5);
        assert_eq!(d.limit_tokens(), 10_000_000.0);
        assert_eq!(d.confidence(), 0.5);

        // Round-trips, both directions, through the hub.
        for (from, to) in [(Percent, Dollars), (Dollars, Percent), (Tokens, Dollars)] {
            let there = c(17.0, from, to);
            assert!((c(there, to, from) - 17.0).abs() < 1e-9);
        }

        // A degenerate estimator returns nothing rather than an infinity.
        let bad = gauge::Denominator {
            tokens_per_percent: 0.0,
            ..gauge::Denominator::prior()
        };
        assert_eq!(bad.convert(1.0, Percent, Tokens), None);
    }

    #[test]
    fn budget_trips_when_the_session_meter_exhausts() {
        let b = svc();
        let mut c = cfg("k", &["aa-metered"]);
        c.session_pct = Some(20.0);
        b.set_config(&c).unwrap();
        // 1% of the session limit = 100k tokens ⇒ 20% = 2M tokens.
        calibrate(&b, "aa-metered", Axis::Session, 100_000.0);
        calibrate(&b, "aa-metered", Axis::WeeklyAll, 100_000.0);

        let accounts = names(&["aa-metered"]);
        b.charge("k", "t1", "aa-metered", "claude-sonnet-5", 1_000_000, 1.0)
            .unwrap();
        let snap = b.snapshot("k", &accounts).unwrap().unwrap();
        assert_eq!(snap.meter(Axis::Session).status, MeterStatus::Ok);
        assert!((snap.meter(Axis::Session).consumed - 10.0).abs() < 1e-6);

        b.charge("k", "t1", "aa-metered", "claude-sonnet-5", 1_100_000, 1.0)
            .unwrap();
        let snap = b.snapshot("k", &accounts).unwrap().unwrap();
        assert_eq!(snap.meter(Axis::Session).status, MeterStatus::Exhausted);
        assert_eq!(snap.tripped_axis(), Some(Axis::Session));
        // ROUND-7 item 3: the default action is now FINISH, not kill.
        assert_eq!(
            b.check_trip("k", &accounts).unwrap().unwrap().action,
            TripAction::Finish
        );
    }

    #[test]
    fn kill_is_a_per_budget_toggle_not_the_default() {
        // Round-7 item 3: default finish; kill stays available per budget for
        // applications that accept a mid-turn teardown.
        let b = svc();
        assert_eq!(TripAction::default(), TripAction::Finish);
        let mut c = cfg("loop", &["aa-metered"]);
        c.session_pct = Some(1.0);
        c.on_trip = TripAction::Kill;
        b.set_config(&c).unwrap();
        calibrate(&b, "aa-metered", Axis::Session, 1_000.0);
        b.charge("loop", "t1", "aa-metered", "claude-sonnet-5", 1_000_000, 0.0)
            .unwrap();
        let accounts = names(&["aa-metered"]);
        assert_eq!(
            b.check_trip("loop", &accounts).unwrap().unwrap().action,
            TripAction::Kill
        );
        // …and the same key flipped back finishes.
        c.on_trip = TripAction::Finish;
        b.set_config(&c).unwrap();
        assert_eq!(
            b.check_trip("loop", &accounts).unwrap().unwrap().action,
            TripAction::Finish
        );
    }

    #[test]
    fn meters_are_independent_any_one_trips() {
        // Session-fine / weekly-exhausted still trips the budget.
        let b = svc();
        let mut c = cfg("k", &["aa-metered"]);
        c.session_pct = Some(90.0);
        c.weekly_pct = Some(5.0);
        b.set_config(&c).unwrap();
        calibrate(&b, "aa-metered", Axis::Session, 100_000.0);
        calibrate(&b, "aa-metered", Axis::WeeklyAll, 100_000.0);
        let accounts = names(&["aa-metered"]);
        b.charge("k", "t1", "aa-metered", "claude-sonnet-5", 600_000, 1.0)
            .unwrap();
        let snap = b.snapshot("k", &accounts).unwrap().unwrap();
        assert_eq!(snap.meter(Axis::Session).status, MeterStatus::Ok); // 6% of 90%
        assert_eq!(snap.meter(Axis::WeeklyAll).status, MeterStatus::Exhausted); // 6% of 5%
        assert_eq!(snap.tripped_axis(), Some(Axis::WeeklyAll));
    }

    #[test]
    fn a_weekly_fable_budget_counts_only_fable_tokens() {
        // ROUND-7 item 4: budgets exist on all THREE percentage axes, and the
        // fable meter must not be fed by a Sonnet turn.
        let b = svc();
        let mut c = cfg("k", &["aa-metered"]);
        c.fable_pct = Some(10.0);
        b.set_config(&c).unwrap();
        calibrate(&b, "aa-metered", Axis::WeeklyFable, 100_000.0);
        let accounts = names(&["aa-metered"]);

        b.charge("k", "t1", "aa-metered", "claude-sonnet-5", 5_000_000, 0.0)
            .unwrap();
        let snap = b.snapshot("k", &accounts).unwrap().unwrap();
        assert_eq!(snap.meter(Axis::WeeklyFable).consumed_tokens, 0);
        assert_eq!(snap.meter(Axis::WeeklyFable).status, MeterStatus::Ok);

        b.charge("k", "t1", "aa-metered", "claude-fable-5", 1_500_000, 0.0)
            .unwrap();
        let snap = b.snapshot("k", &accounts).unwrap().unwrap();
        assert_eq!(snap.meter(Axis::WeeklyFable).consumed_tokens, 1_500_000);
        assert_eq!(snap.meter(Axis::WeeklyFable).status, MeterStatus::Exhausted);
        assert_eq!(snap.tripped_axis(), Some(Axis::WeeklyFable));
    }

    #[test]
    fn the_cash_meter_is_dollars_pooled_across_accounts_and_never_calibrates() {
        // ROUND-7 item 5: "$20 is $20 across all the accounts", and dollars
        // are measured directly — this axis is never `calibrating`.
        let b = svc();
        let mut c = cfg("k", &["a", "b"]);
        c.cash_usd = Some(20.0);
        b.set_config(&c).unwrap();
        let accounts = pool(&["a", "b"]);

        b.charge("k", "t1", "a", "claude-sonnet-5", 10, 12.0).unwrap();
        let snap = b.snapshot("k", &accounts).unwrap().unwrap();
        let cash = snap.meter(Axis::Cash);
        assert_eq!(cash.status, MeterStatus::Ok);
        assert!((cash.consumed - 12.0).abs() < 1e-9);
        assert!(cash.accounts.iter().all(|a| a.limit_tokens.is_none()));

        // $12 on one account plus $9 on another is $21 TOTAL — over $20.
        b.charge("k", "t1", "b", "claude-sonnet-5", 10, 9.0).unwrap();
        let snap = b.snapshot("k", &accounts).unwrap().unwrap();
        assert!((snap.meter(Axis::Cash).consumed - 21.0).abs() < 1e-9);
        assert_eq!(snap.meter(Axis::Cash).status, MeterStatus::Exhausted);
        assert_eq!(snap.tripped_axis(), Some(Axis::Cash));
        let trip = b.check_trip("k", &accounts).unwrap().unwrap();
        // Round values still render round — the precision fix must not turn
        // `$21` into `$21.000000`.
        assert!(trip.reason().contains("$21 of its $20 limit"), "{}", trip.reason());
    }

    /// Every human-readable budget line reports the value it was actually
    /// configured with. Fixed-width rounding was misleading on both sides:
    /// a 3.5% threshold printed as `4%` (so the gate read as off by one) and a
    /// $0.005 cash limit printed as `$0.00` (so a live trip read as firing
    /// against a zero limit) — both observed on the 2026-07-29 run.
    #[test]
    fn budget_lines_report_the_configured_value_not_a_rounded_one() {
        assert_eq!(exact(3.5), "3.5");
        assert_eq!(exact(4.0), "4");
        assert_eq!(exact(0.005), "0.005");
        assert_eq!(exact(0.0), "0");
        assert_eq!(exact(-0.0), "0");
        assert_eq!(exact(21.0), "21");
        assert_eq!(exact(0.1 + 0.2), "0.3"); // no float noise leaks out

        // A fractional cash limit reads honestly in a trip line.
        let trip = Trip {
            budget_key: "stress-cash".into(),
            axis: Axis::Cash,
            consumed: 0.0075,
            limit: 0.005,
            action: TripAction::Kill,
        };
        assert_eq!(
            trip.reason(),
            "budget stress-cash exhausted: cash meter at $0.0075 of its $0.005 limit"
        );

        // And a fractional THRESHOLD reads honestly in a park reason. 3.5 must
        // not present as 4 — at 4% observed the gate really is above the line.
        let b = svc();
        b.observe("a", Axis::Session, 4.0, None, "usage_poll").unwrap();
        let th = Thresholds {
            session: Some(3.5),
            ..Default::default()
        };
        let pool = pool(&["a"]);
        let admission = b.admit(&req(None, &th, &pool, "a", &[])).unwrap();
        let Admission::Park { reason, .. } = admission else {
            panic!("4% observed against a 3.5% threshold must park: {admission:?}");
        };
        assert!(
            reason.contains("session at 4% ≥ threshold 3.5%"),
            "the threshold must print as configured: {reason}"
        );
    }

    #[test]
    fn a_cash_budget_bites_with_no_calibration_at_all() {
        // The cash axis needs no denominator: dollars are measured directly,
        // so it enforces with no estimate and no prior anywhere.
        //
        // The old version of this test also asserted `session_pct = 1.0` was
        // INERT while uncalibrated. That premise is gone (msg #1055): a
        // percentage budget now gates on the prior. The session axis is left
        // unset here so the cash half is what this test is about; the prior's
        // own gating is `the_prior_gates_from_first_boot_and_errs_early` below.
        let b = svc();
        let mut c = cfg("k", &["a"]);
        c.cash_usd = Some(5.0);
        b.set_config(&c).unwrap();
        let accounts = pool(&["a"]);
        b.charge("k", "t1", "a", "claude-sonnet-5", 9_999_999, 6.0)
            .unwrap();
        let snap = b.snapshot("k", &accounts).unwrap().unwrap();
        assert_eq!(snap.tripped_axis(), Some(Axis::Cash));
        // No estimator underneath it, before or after the prior landed.
        assert!(b.denominator("a", Axis::Cash).unwrap().is_none());
        assert_eq!(snap.meter(Axis::Cash).accounts[0].limit_tokens, None);
    }

    #[test]
    fn denomination_is_single_account_units_pooled_across_accounts() {
        // DEC-5: 10% on one account + 10% on another = 20% of ONE account,
        // not 10% of installed capacity.
        let b = svc();
        let mut c = cfg("k", &["a", "b"]);
        c.session_pct = Some(20.0);
        b.set_config(&c).unwrap();
        calibrate(&b, "a", Axis::Session, 100_000.0);
        calibrate(&b, "b", Axis::Session, 100_000.0);
        let accounts = pool(&["a", "b"]);

        b.charge("k", "t1", "a", "claude-sonnet-5", 1_000_000, 0.0)
            .unwrap(); // 10% of a
        b.charge("k", "t1", "b", "claude-sonnet-5", 1_000_000, 0.0)
            .unwrap(); // 10% of b
        let snap = b.snapshot("k", &accounts).unwrap().unwrap();
        assert!((snap.meter(Axis::Session).consumed - 20.0).abs() < 1e-6);
        assert_eq!(snap.meter(Axis::Session).status, MeterStatus::Exhausted);
        assert_eq!(snap.meter(Axis::Session).accounts.len(), 2);
    }

    #[test]
    fn per_account_limits_may_differ_and_the_meter_rebases() {
        // DEC-12: the denominator is per-account and moves; the meter divides
        // at read time, so re-basing never double-charges.
        let b = svc();
        let mut c = cfg("k", &["a"]);
        c.session_pct = Some(50.0);
        b.set_config(&c).unwrap();
        calibrate(&b, "a", Axis::Session, 100_000.0); // 1% = 100k
        let accounts = pool(&["a"]);
        b.charge("k", "t1", "a", "claude-sonnet-5", 2_000_000, 0.0)
            .unwrap();
        let before = b.snapshot("k", &accounts).unwrap().unwrap();
        assert!((before.meter(Axis::Session).consumed - 20.0).abs() < 1e-6);

        // The estimate doubles (Anthropic gave this account more room).
        calibrate(&b, "a", Axis::Session, 200_000.0);
        let after = b.snapshot("k", &accounts).unwrap().unwrap();
        assert_eq!(after.meter(Axis::Session).consumed_tokens, 2_000_000);
        assert!((after.meter(Axis::Session).consumed - 10.0).abs() < 1e-6);
    }

    // ── account scope (round-7 item 6) ───────────────────────────────────

    #[test]
    fn a_budget_key_must_name_its_accounts() {
        // The crate refuses to guess "all accounts".
        let b = svc();
        let mut c = BudgetConfig::new("unscoped");
        c.session_pct = Some(10.0);
        let err = b.set_config(&c).unwrap_err().to_string();
        assert!(err.contains("must name the accounts"), "{err}");
        assert!(b.get_config("unscoped").unwrap().is_none());
    }

    #[test]
    fn scope_confines_metering_and_admission() {
        let b = svc();
        let mut c = cfg("biz-a", &["a"]);
        c.session_pct = Some(50.0);
        b.set_config(&c).unwrap();
        calibrate(&b, "a", Axis::Session, 100_000.0);
        calibrate(&b, "b", Axis::Session, 100_000.0);
        let accounts = pool(&["a", "b"]);

        // A charge on the OUT-OF-SCOPE account is still recorded (history is
        // never rewritten) but the metered set is driven by the scope.
        b.charge("biz-a", "t1", "a", "claude-sonnet-5", 1_000_000, 0.0)
            .unwrap();
        let snap = b.snapshot("biz-a", &accounts).unwrap().unwrap();
        assert_eq!(snap.metered_accounts, vec!["a".to_string()]);

        // A thread on the out-of-scope account is moved into scope…
        let th = Thresholds::ungated();
        assert_eq!(
            b.admit(&req(Some("biz-a"), &th, &accounts, "b", &[]))
                .unwrap(),
            Admission::Admit {
                switch_to: Some(placeable("a"))
            }
        );
        // …and when NO registered account is in scope, it parks.
        let mut c2 = cfg("biz-c", &["nowhere"]);
        c2.session_pct = Some(50.0);
        b.set_config(&c2).unwrap();
        match b
            .admit(&req(Some("biz-c"), &th, &accounts, "a", &[]))
            .unwrap()
        {
            Admission::Park { cause, reason, .. } => {
                assert_eq!(cause, ParkCause::Scope);
                assert!(reason.contains("nowhere"), "{reason}");
            }
            other => panic!("expected a scope park, got {other:?}"),
        }
    }

    // ── thresholds ───────────────────────────────────────────────────────

    #[test]
    fn thresholds_gate_on_the_pool_and_move_a_fresh_thread() {
        // DEC-10: over on one account, under on another ⇒ run over there —
        // but only because this thread has no conversation to lose.
        let b = svc();
        b.observe("a", Axis::WeeklyFable, 80.0, None, "usage_poll")
            .unwrap();
        b.observe("b", Axis::WeeklyFable, 20.0, None, "usage_poll")
            .unwrap();
        let th = Thresholds {
            fable: Some(60.0),
            ..Default::default()
        };
        let accounts = pool(&["a", "b"]);
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "a", &[])).unwrap(),
            Admission::Admit {
                switch_to: Some(placeable("b"))
            }
        );
    }

    #[test]
    fn thresholds_park_when_every_account_is_over() {
        let b = svc();
        b.observe("a", Axis::WeeklyFable, 80.0, None, "usage_poll")
            .unwrap();
        b.observe("b", Axis::WeeklyFable, 95.0, None, "usage_poll")
            .unwrap();
        let th = Thresholds {
            fable: Some(60.0),
            ..Default::default()
        };
        let accounts = pool(&["a", "b"]);
        match b.admit(&req(None, &th, &accounts, "a", &[])).unwrap() {
            Admission::Park { cause, reason, .. } => {
                assert_eq!(cause, ParkCause::Threshold);
                assert!(reason.contains("weekly_fable"));
            }
            other => panic!("expected a park, got {other:?}"),
        }
    }

    #[test]
    fn an_unobserved_gauge_never_blocks() {
        // CON-5 again, from the threshold side — including the cash gate,
        // which has no live gauge at all yet.
        let b = svc();
        let th = Thresholds {
            session: Some(1.0),
            cash: Some(0.01),
            ..Default::default()
        };
        let accounts = pool(&["a"]);
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "a", &[])).unwrap(),
            Admission::Admit { switch_to: None }
        );
    }

    #[test]
    fn a_cash_threshold_gates_in_dollars_once_observed() {
        // ROUND-7 item 5, threshold half: the gate reads DOLLARS.
        let b = svc();
        b.observe("a", Axis::Cash, 25.0, None, "usage_poll").unwrap();
        b.observe("b", Axis::Cash, 3.0, None, "usage_poll").unwrap();
        let th = Thresholds {
            cash: Some(20.0),
            ..Default::default()
        };
        let accounts = pool(&["a", "b"]);
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "a", &[])).unwrap(),
            Admission::Admit {
                switch_to: Some(placeable("b"))
            }
        );
        let th_all = Thresholds {
            cash: Some(1.0),
            ..Default::default()
        };
        match b.admit(&req(None, &th_all, &accounts, "a", &[])).unwrap() {
            Admission::Park { cause, reason, .. } => {
                assert_eq!(cause, ParkCause::Threshold);
                assert!(reason.contains('$'), "{reason}");
            }
            other => panic!("expected a park, got {other:?}"),
        }
    }

    #[test]
    fn a_pinned_thread_waits_instead_of_moving() {
        let b = svc();
        b.observe("a", Axis::Session, 90.0, None, "usage_poll")
            .unwrap();
        b.observe("b", Axis::Session, 10.0, None, "usage_poll")
            .unwrap();
        let th = Thresholds {
            session: Some(50.0),
            ..Default::default()
        };
        let accounts = pool(&["a", "b"]);
        let mut r = req(None, &th, &accounts, "a", &[]);
        r.pinned = true;
        assert!(matches!(b.admit(&r).unwrap(), Admission::Park { .. }));
    }

    // ── placement (rank 7): which permissible account, and why ───────────

    /// Put a thread row on an account — the census the "fewest threads placed
    /// here" criterion reads.
    fn place_thread(b: &Budgets, id: &str, account: &str) {
        let now = crate::store::now_rfc3339();
        b.store
            .upsert_thread(&crate::store::ThreadRow {
                id: id.into(),
                account: Some(account.into()),
                cwd: None,
                model: None,
                settings_json: "{}".into(),
                last_seq: 0,
                next_message_id: 1,
                last_compaction_message_id: 0,
                needs_restate: false,
                created_at: now.clone(),
                updated_at: now,
            })
            .unwrap();
    }

    fn place<'a>(b: &Budgets, th: &'a Thresholds, candidates: &'a [Placeable]) -> Option<Placeable> {
        b.place(&PlacementRequest {
            thresholds: th,
            candidates,
            prefer: None,
            sessions: &[],
            demand: Demand::default(),
        })
        .unwrap()
    }

    // ── wave two: pressure, the pool share, the cap, and momentum ────────
    //
    // Everything below was mutation-tested: each rule was BROKEN in the source
    // and the named test was watched to go red. A green nobody has forced to
    // fail is not evidence, so where a mutation SURVIVED it is written down in
    // the test that failed to catch it rather than left for someone to find.

    /// Give an ACCOUNT a pool share, by name — which is now the key.
    ///
    /// This helper used to resolve the account's login fingerprint first, so
    /// that *"a test cannot accidentally prove the label-keyed behaviour the
    /// table exists to prevent"*. The operator overruled the premise on
    /// 2026-08-27: *"the login slot is subject to change"*, so the name is the
    /// key and the fingerprint is not consulted at all.
    fn set_share(b: &Budgets, account: &str, share: f64) {
        b.store
            .set_pool_share(account, share, Some("test"))
            .unwrap();
    }

    /// TAKE AN ACCOUNT OUT OF THE POOL, OR PUT IT BACK. Its own helper because
    /// it is its own act: before 2026-09-20 the tests below spelled a disable
    /// `set_share(.., 0.0)`, which is precisely the conflation the operator
    /// struck.
    fn set_disabled(b: &Budgets, account: &str, disabled: bool) {
        b.store
            .set_account_disabled(account, disabled, Some("test"))
            .unwrap();
    }

    /// **PUT A LIVE EXECUTION ON AN ACCOUNT** — the second condition behind
    /// [`UnrankedReason::OverShareSettled`], as a test fixture.
    ///
    /// Two rows, because [`Budgets::nothing_running_on`] needs both and a test
    /// that wrote only one would be testing the wrong thing:
    ///
    /// * a `running_turns` row whose `pid` is THIS TEST PROCESS, because the
    ///   predicate refuses to count a turn whose process is gone (a leftover
    ///   `recover()` would clear), and
    /// * a `thread_accounts` row, because `running_turns` carries no account
    ///   column and attribution goes through the thread's most recently served
    ///   account. Without it the turn is UNATTRIBUTABLE, which the predicate
    ///   counts against every account at once — true to its contract, and
    ///   useless for a test that needs exactly one account to be busy.
    fn set_running_on(b: &Budgets, thread_id: &str, account: &str) {
        b.store
            .upsert_running_turn(&crate::store::RunningTurnRow {
                thread_id: thread_id.into(),
                pid: std::process::id() as i64,
                spawn_cookie: "cookie".into(),
                output_path: "/dev/null".into(),
                byte_offset: 0,
                started_at: crate::store::now_rfc3339(),
                prompt: None,
                message_ids: vec![],
                attempt: 1,
            })
            .unwrap();
        b.store.record_thread_account(thread_id, account).unwrap();
    }

    /// Production-scale denominators, so the expected-deaths objective TIES and
    /// the criterion under test is the one actually deciding. With the 20K
    /// prior, a point of weekly slack is a fifth of a nominal turn and the
    /// survival term decides everything before pressure is ever consulted.
    fn flatten_deaths(b: &Budgets, accounts: &[&str]) {
        for name in accounts {
            for axis in [Axis::Session, Axis::WeeklyAll] {
                b.store
                    .upsert_estimate(&crate::store::EstimateRow {
                        account: (*name).into(),
                        axis: axis.as_str().into(),
                        tokens_per_percent: 20_500_000.0,
                        sample_count: 4,
                        last_obs_id: 0,
                        updated_at: crate::store::now_rfc3339(),
                    })
                    .unwrap();
            }
        }
    }

    #[test]
    fn pressure_is_a_RATIO_and_the_subtraction_would_pick_the_other_account() {
        // **THE DISCRIMINATING CASE.** Two accounts constructed so that the
        // shipped subtraction and the required division DISAGREE ON THE WINNER
        // — not merely on the numbers. Without that, "we changed it to a
        // division" is a claim no test is actually checking.
        //
        //   sprinting: 20% of its share unused, 2% of the window left
        //              subtraction 0.20 − 0.02 = +0.18
        //              division    0.20 ÷ 0.02 = 10.0
        //   cruising:  60% of its share unused, 40% of the window left
        //              subtraction 0.60 − 0.40 = +0.20   ← bigger, so it WINS
        //              division    0.60 ÷ 0.40 = 1.5     ← smaller, so it LOSES
        //
        // The subtraction says `cruising`. It is wrong, and the operator said
        // why: *"I don't see why it's a subtraction and not a division."* An
        // additive gap cannot tell an account that must burn at TEN TIMES its
        // nominal rate to avoid losing its remainder from one that is merely
        // ahead of schedule. A rate can.
        let b = svc();
        let now = crate::clock::now();
        let window = Axis::WeeklyAll.window_secs() as f64;
        let resets_in = |frac: f64| {
            Some((now + ChronoDuration::seconds((window * frac) as i64)).to_rfc3339())
        };
        flatten_deaths(&b, &["sprinting", "cruising"]);
        b.observe("sprinting", Axis::WeeklyAll, 80.0, resets_in(0.02), "usage_poll")
            .unwrap();
        b.observe("cruising", Axis::WeeklyAll, 40.0, resets_in(0.40), "usage_poll")
            .unwrap();

        // The arithmetic itself, pinned before the ranking that consumes it.
        // R9.PR1: measured against `cap = min(1.0, 0.95)`, so `sprinting` has
        // 0.15/0.95 of its cap unused (was 0.20) and `cruising` 0.55/0.95 (was
        // 0.60). The division — and the order — are what this test is about.
        let p = |a: &str| b.pool_pressure(a, now).value().unwrap();
        assert!(
            (p("sprinting") - (0.15 / 0.95) / 0.02).abs() < 0.2,
            "0.15/0.95 unused over 0.02 of a window is a burn rate of 7.9, got {}",
            p("sprinting")
        );
        assert!(
            (p("cruising") - (0.55 / 0.95) / 0.40).abs() < 0.05,
            "0.55/0.95 unused over 0.40 of a window is a burn rate of 1.45, got {}",
            p("cruising")
        );

        // The ORDER is the point: the subtraction ranks these the other way.
        let sub = |unused: f64, left: f64| unused - left;
        assert!(
            sub(0.20, 0.02) < sub(0.60, 0.40),
            "the case is only discriminating if the subtraction really does \
             prefer `cruising` — if this assert fails the test has gone blunt"
        );
        assert!(p("sprinting") > p("cruising"));

        let accounts = pool(&["sprinting", "cruising"]);
        assert_eq!(
            place(&b, &Thresholds::default(), &accounts),
            Some(placeable("sprinting")),
            "drain the LARGEST pressure first: an account with 20% of its share \
             left and 2% of its window left must outrank one that is merely \
             ahead of schedule, which is exactly the call the subtraction got \
             backwards"
        );
    }

    #[test]
    fn a_ten_percent_account_does_not_attract_ten_times_the_pressure() {
        // **THE FAILURE THE OPERATOR NAMED, BUILT EXACTLY AS HE STATED IT:**
        //
        //   "if I only gave 10% of an account, well, then there's going to be
        //    disproportionate pressure of 10x on that 10% account when really,
        //    the amount available to it is only the 10% to begin with."
        //
        // Two untouched accounts, identical windows, identical readings. The
        // only difference is that one login offered 10% of itself and the other
        // offered all of it. Both are on pace, because neither has spent
        // anything of what it gave. Both must read 1.0.
        //
        // The bug this pins is the share applied as a NUMERATOR — `unused ÷
        // share`, which is 1.0 ÷ 0.1 = 10.0 and is precisely the 10x he
        // described. The share is a DENOMINATOR: it normalises the unused
        // budget against what was actually offered.
        let b = svc();
        let now = crate::clock::now();
        let resets_in = Some((now + ChronoDuration::hours(84)).to_rfc3339());
        flatten_deaths(&b, &["small", "whole"]);
        for a in ["small", "whole"] {
            b.observe(a, Axis::WeeklyAll, 0.0, resets_in.clone(), "usage_poll")
                .unwrap();
        }
        set_share(&b, "small", 0.10);
        // `whole` gets NO row at all — the absent-row default is 1.0, which is
        // the entire migration story and is worth pinning as behaviour.
        assert_eq!(
            b.pool_share("whole"),
            PoolShare::pooled(1.0),
            "an absent row means the whole account"
        );
        assert_eq!(b.pool_share("small"), PoolShare::pooled(0.10));

        let small = b.pool_pressure("small", now).value().unwrap();
        let whole = b.pool_pressure("whole", now).value().unwrap();
        assert!(
            (small - whole).abs() < 1e-9,
            "two untouched accounts are equally on pace whatever share was \
             offered — got small={small}, whole={whole}"
        );
        assert!(
            small < 10.0 * whole * 0.5,
            "the 10% account is showing the 10x the operator called out: \
             small={small} against whole={whole}"
        );

        // And the share DOES divide — it is not simply ignored, which is the
        // other way to make the assertions above pass. Spending 5% of the whole
        // account is spending HALF of a 10% share, and must read the same as
        // spending half of a whole account.
        let c = svc();
        flatten_deaths(&c, &["tenth", "full"]);
        c.observe("tenth", Axis::WeeklyAll, 5.0, resets_in.clone(), "usage_poll")
            .unwrap();
        // R9.PR1: a whole account's cap is 0.95, so HALF of it spent is 47.5%.
        c.observe("full", Axis::WeeklyAll, 47.5, resets_in.clone(), "usage_poll")
            .unwrap();
        set_share(&c, "tenth", 0.10);
        let tenth = c.pool_pressure("tenth", now).value().unwrap();
        let full = c.pool_pressure("full", now).value().unwrap();
        assert!(
            (tenth - full).abs() < 1e-9,
            "half a 10% share spent must rank exactly like half a whole account \
             spent — got tenth={tenth}, full={full}. If these differ the share \
             is not in the denominator."
        );
    }

    #[test]
    fn overage_past_the_share_stays_visible_AND_is_hard_clamped() {
        // ── THIS TEST WAS CALLED `..._and_is_never_blocked`, AND THAT HALF IS
        //    OVERRULED (2026-08-27) ──
        //
        // It used to assert that an over-share account is STILL PLACEABLE, on
        // the reasoning that "overage moves a ranking; it never refuses a turn"
        // and that refusing it would be the per-account veto the operator
        // struck. He drew the line differently, and explicitly:
        //
        //   "if all the other accounts are blocked, we don't want that account
        //    to even be available for placement, right? So overage we've
        //    determined is okay while the current workload is finishing. But
        //    then it needs to be like hard clamped — not available for
        //    placement as well. So it needs to have an effect on pressure, but
        //    it also needs to have an effect on whether placement is possible."
        //
        // The veto he struck was a per-account THRESHOLD. This is the per-account
        // CAPACITY he asked for in the same breath ("we just need a per account
        // capacity, like percent offered to the pool"), now carrying both of the
        // effects he has given it. Nothing here reads a threshold.
        //
        // The FIRST half of the old test is unchanged and still load-bearing:
        // pressure goes NEGATIVE rather than clamping at zero, because he keeps
        // the remainder to spend by hand and asked to see how far past his share
        // the pool went.
        let b = svc();
        let now = crate::clock::now();
        let resets_in = Some((now + ChronoDuration::hours(84)).to_rfc3339());
        let resets_in2 = resets_in.clone();
        flatten_deaths(&b, &["gmail"]);
        // His Gmail login: 95% offered, 100% of the account consumed.
        set_share(&b, "gmail", 0.95);
        b.observe("gmail", Axis::WeeklyAll, 100.0, resets_in, "usage_poll")
            .unwrap();

        // ── AND THE FIRST HALF NOW NEEDS ITS SECOND CONDITION SPELLED
        //    (2026-09-22) ──
        //
        // "Overage stays visible" acquired a precondition: the reading is kept
        // WHILE A THREAD IS STILL RUNNING THERE, and removed from view once the
        // account is quiet. That is the operator's own ruling, and this is the
        // test that states the first half of it — so the running turn belongs
        // here rather than being worked around. See
        // `UnrankedReason::OverShareSettled`, and
        // `an_over_share_account_with_a_thread_still_running_keeps_its_negative_pressure`
        // for the same property proved on purpose rather than in passing.
        //
        // The thread is exactly the one the operator described: the last one to
        // run on the account, not evicted early, which is WHY the account went
        // past its share in the first place.
        set_running_on(&b, "t-overrun", "gmail");

        let p = b.pool_pressure("gmail", now).value().unwrap();
        assert!(
            p < 0.0,
            "spending past the offered share must read NEGATIVE so the overage \
             is visible, got {p}"
        );

        // …and it is NOT a placement target, even as the only candidate. The
        // single-candidate case is the whole point: it is "if all the other
        // accounts are blocked", and it is the case a ranking term can never
        // cover, because a term with nothing to lose to still wins.
        let accounts = pool(&["gmail"]);
        assert_eq!(
            place(&b, &Thresholds::ungated(), &accounts),
            None,
            "past the share, the account is not available for placement — the boundary \
             is a veto as well as a denominator"
        );

        // INSIDE the share it is placeable again, so the refusal above is the
        // CLAMP and not something else about this fixture. Note the boundary is
        // `consumed >= share`, not `>`: having spent exactly what he lent is
        // having spent it, and an account at 100% of a whole share is
        // exhausted, so both ends of that read the same way.
        b.observe("gmail", Axis::WeeklyAll, 94.0, resets_in2, "usage_poll")
            .unwrap();
        assert_eq!(
            place(&b, &Thresholds::ungated(), &accounts),
            Some(placeable("gmail")),
            "94% of a 95% share is inside it, and the account is a target again"
        );
    }

    #[test]
    fn no_pressure_however_large_can_outweigh_a_veto() {
        // **A VETO IS ABSOLUTE, and this is the property that makes expressing
        // it as a weight illegal.** A weight would have to be infinite; any
        // finite number loses to enough pressure, and the operator's example of
        // what that costs is a background loop dragged into his working session
        // past the 40% gate on his trading loop.
        //
        // So: the vetoed account is given the largest pressure the criterion
        // can produce — an untouched weekly window seconds from its reset,
        // which is the floor of the denominator and therefore the ceiling of
        // the whole measurement — and the sweep walks it up to that ceiling.
        // At no magnitude may it be placed.
        let b = svc();
        let now = crate::clock::now();
        let window = Axis::WeeklyAll.window_secs() as f64;
        flatten_deaths(&b, &["hot", "cool"]);
        // `cool` is the lawful account and is deliberately UNATTRACTIVE: ahead
        // of its clock, so nothing but the veto explains it winning.
        b.observe("cool", Axis::Session, 5.0, None, "usage_poll").unwrap();
        b.observe(
            "cool",
            Axis::WeeklyAll,
            90.0,
            Some((now + ChronoDuration::hours(160)).to_rfc3339()),
            "usage_poll",
        )
        .unwrap();

        // THE GATE: his trading loop's 40% session threshold.
        let th = Thresholds {
            session: Some(40.0),
            ..Default::default()
        };
        let accounts = pool(&["hot", "cool"]);

        for frac_left in [0.5, 0.1, 0.01, 0.001, 0.0001, 0.000_001] {
            // `hot` is over the session gate — vetoed, absolutely.
            b.observe("hot", Axis::Session, 99.0, None, "usage_poll").unwrap();
            // …and simultaneously the most attractive account imaginable on the
            // axis pressure measures.
            b.observe(
                "hot",
                Axis::WeeklyAll,
                0.0,
                Some((now + ChronoDuration::seconds((window * frac_left).max(1.0) as i64)).to_rfc3339()),
                "usage_poll",
            )
            .unwrap();
            let p = b.pool_pressure("hot", now).value().unwrap();
            assert!(p > 1.0, "the sweep must actually be building pressure, got {p}");

            let got = b.admit(&req(None, &th, &accounts, "cool", &[])).unwrap();
            assert_eq!(
                got,
                Admission::Admit { switch_to: None },
                "pressure of {p} on an account over its 40% session gate must \
                 not place a turn there. A veto is not a weight and cannot be \
                 outbid."
            );
        }
    }

    #[test]
    fn the_landing_cap_grades_rather_than_switching() {
        // Thresholds in this crate are a BINARY veto: under the line you are
        // fine, at it you are refused, and placement is blind in between. So
        // two accounts at 5% and 45% against a 50% ceiling are indistinguishable
        // to every gate that exists today — and the second one is one turn from
        // falling off a cliff.
        //
        // The cap grades the same line. And the grading has to be REAL, so the
        // test is built so the cap and the rank point at DIFFERENT accounts:
        // `tight` is given far more weekly pressure than `roomy`. If the cap
        // were a bit, or ran after the rank, `tight` would win.
        let b = svc();
        let now = crate::clock::now();
        let window = Axis::WeeklyAll.window_secs() as f64;
        flatten_deaths(&b, &["roomy", "tight"]);
        let th = Thresholds {
            session: Some(50.0),
            ..Default::default()
        };

        b.observe("roomy", Axis::Session, 5.0, None, "usage_poll").unwrap();
        b.observe("tight", Axis::Session, 45.0, None, "usage_poll").unwrap();
        // `tight` is starved and about to reset — maximum pull from the rank.
        b.observe(
            "tight",
            Axis::WeeklyAll,
            5.0,
            Some((now + ChronoDuration::seconds((window * 0.02) as i64)).to_rfc3339()),
            "usage_poll",
        )
        .unwrap();
        // `roomy` is ahead of its clock — the rank actively repels it.
        b.observe(
            "roomy",
            Axis::WeeklyAll,
            80.0,
            Some((now + ChronoDuration::hours(160)).to_rfc3339()),
            "usage_poll",
        )
        .unwrap();
        assert!(
            b.pool_pressure("tight", now).value().unwrap()
                > b.pool_pressure("roomy", now).value().unwrap(),
            "the case is only discriminating if the RANK prefers `tight`"
        );

        // GRADED: neither account is vetoed, and yet they are distinguishable.
        let cap = |a: &str| b.landing_capacity(a, &th).value().unwrap();
        assert!((cap("roomy") - 0.9).abs() < 1e-9, "got {}", cap("roomy"));
        assert!((cap("tight") - 0.1).abs() < 1e-9, "got {}", cap("tight"));

        let accounts = pool(&["roomy", "tight"]);
        assert_eq!(
            place(&b, &th, &accounts),
            Some(placeable("roomy")),
            "the CAP runs before the RANK: an account with 10% of its landing \
             room left must not take the turn just because its week is emptier"
        );

        // A bit has two values. A capacity has as many as the reading does —
        // pinned as strict monotonicity, so a later `if over_half {}` cannot
        // pass this test.
        let c = svc();
        let mut last = f64::INFINITY;
        for pct in [0.0, 10.0, 20.0, 30.0, 40.0, 49.0] {
            c.observe("a", Axis::Session, pct, None, "usage_poll").unwrap();
            let v = c.landing_capacity("a", &th).value().unwrap();
            assert!(
                v < last,
                "capacity must fall with every point consumed, not switch: \
                 {pct}% read {v}, previous read {last}"
            );
            last = v;
        }
    }

    #[test]
    fn the_cap_measures_against_the_share_not_the_whole_account() {
        // The same denominator rule as pressure, on the other axis. A login
        // that offered 10% of itself has 10% to land in, so 5% consumed is HALF
        // its room — not the 95%-clear it would read against a whole account.
        let b = svc();
        let th = Thresholds::ungated();
        b.observe("tenth", Axis::Session, 5.0, None, "usage_poll").unwrap();
        set_share(&b, "tenth", 0.10);
        let v = b.landing_capacity("tenth", &th).value().unwrap();
        assert!(
            (v - 0.5).abs() < 1e-9,
            "5% of an account whose login offered 10% is half the landing room, \
             got {v}"
        );
    }

    #[test]
    fn momentum_never_places_where_the_veto_or_the_cap_refused() {
        // Momentum decides WHICH account, never WHETHER. The cheapest way for
        // that to be false is for a warm cache to hold a thread on an account a
        // gate has since closed, so both gates are tested against a cache that
        // is genuinely warm — a turn booked on the ledger this instant.
        let warm = |b: &Budgets, account: &str| {
            b.store
                .record_usage(&crate::store::UsageRow {
                    thread_id: "me".into(),
                    account: Some(account.into()),
                    model: "fable".into(),
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                    cost_usd: 0.0,
                    started_at: None,
                })
                .unwrap();
        };

        // ── the VETO ──
        let b = svc();
        flatten_deaths(&b, &["home", "away"]);
        warm(&b, "home");
        assert!(
            b.momentum_is_warm("home", crate::clock::now()),
            "the test is only meaningful if the cache really is warm"
        );
        b.observe("home", Axis::Session, 99.0, None, "usage_poll").unwrap();
        b.observe("away", Axis::Session, 1.0, None, "usage_poll").unwrap();
        let th = Thresholds {
            session: Some(40.0),
            ..Default::default()
        };
        let accounts = pool(&["home", "away"]);
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "home", &[])).unwrap(),
            Admission::Admit {
                switch_to: Some(placeable("away"))
            },
            "a warm cache on a vetoed account must not keep the turn there"
        );

        // ── the CAP ──
        //
        // `prefer` is SET here, and that is load-bearing. Momentum requires the
        // thread to already be on the account, so a test that leaves `prefer`
        // as `None` leaves momentum permanently false and proves nothing about
        // the ordering at all. (Found by mutation: promoting momentum above the
        // cap survived until this line said `Some("home")`.)
        let c = svc();
        flatten_deaths(&c, &["home", "away"]);
        warm(&c, "home");
        // Both clear the veto; `home` has almost no landing room left.
        c.observe("home", Axis::Session, 39.0, None, "usage_poll").unwrap();
        c.observe("away", Axis::Session, 1.0, None, "usage_poll").unwrap();
        let accounts = pool(&["home", "away"]);
        let got = c
            .place(&PlacementRequest {
                thresholds: &th,
                candidates: &accounts,
                prefer: Some("home"),
                sessions: &[],
                demand: Demand::default(),
            })
            .unwrap();
        assert!(
            c.momentum_is_warm("home", crate::clock::now()),
            "the ordering is only under test while the cache is genuinely warm"
        );
        assert_eq!(
            got,
            Some(placeable("away")),
            "the cap runs BEFORE momentum: a warm cache must not hold a turn on \
             an account with 2.5% of its landing room left"
        );
    }

    #[test]
    fn pressure_stays_finite_at_the_very_edge_of_a_window() {
        // The denominator floor. A required burn rate divides by the time left,
        // and the time left goes to zero at every single reset — so without a
        // floor this produces an infinity, and an infinity in a ranking is
        // unorderable in exactly the way a veto-expressed-as-a-weight is.
        //
        // Sub-second on purpose: the reset is in the future (so the staleness
        // guard does not fire) but rounds to zero whole seconds, which is the
        // only input that actually divides by zero. (Found by mutation: removing
        // the floor survived a sweep that only asserted `pressure > 1.0`, which
        // an infinity satisfies perfectly well.)
        let b = svc();
        let now = crate::clock::now();
        b.observe(
            "edge",
            Axis::WeeklyAll,
            10.0,
            Some((now + ChronoDuration::milliseconds(500)).to_rfc3339()),
            "usage_poll",
        )
        .unwrap();
        let p = b.pool_pressure("edge", now).value().unwrap();
        assert!(
            p.is_finite(),
            "a window less than a second from its reset must yield a large \
             FINITE burn rate, got {p}"
        );
        assert!(p > 1.0, "…and it must still be a very large one, got {p}");
    }

    #[test]
    fn a_cold_cache_gives_no_momentum_but_still_does_not_churn() {
        // The demotion, and the invariant it must not take down with it.
        //
        // `stay` used to do two jobs under one flag. Momentum is one of them
        // (cost: a warm cache is cheaper) and it EXPIRES. Anti-churn is the
        // other (moving a settled thread for no measurable reason is worse than
        // leaving it) and it does NOT. Gating the single flag on warmth would
        // have silently deleted the second, so they are separate criteria and
        // this pins both halves.
        let b = svc();
        let th = Thresholds::ungated();
        let accounts = pool(&["a", "b"]);
        place_thread(&b, "me", "b");

        // Cold: no ledger row at all.
        assert!(!b.momentum_is_warm("b", crate::clock::now()));
        // …and the thread still does not move, on anti-churn alone.
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "b", &[])).unwrap(),
            Admission::Admit { switch_to: None },
            "a cold cache removes the momentum PREFERENCE, never the anti-churn \
             floor — otherwise a settled thread ping-pongs on the rotation cursor"
        );

        // A row too old to matter is still cold — the window is real, not a
        // dressed-up `is_some()`.
        b.store
            .execute_sql(
                "INSERT INTO usage_ledger
                    (thread_id, account, model, input_tokens, output_tokens,
                     cache_read_tokens, cache_creation_tokens, cost_usd, ts, accounting_rev)
                 VALUES ('me','b','fable',1,1,0,0,0,'2020-01-01T00:00:00Z',1)",
            )
            .unwrap();
        assert!(
            !b.momentum_is_warm("b", crate::clock::now()),
            "a turn from 2020 is not a warm prompt cache"
        );
    }

    #[test]
    fn the_pressure_rank_has_an_off_switch_and_it_is_exactly_silent() {
        // The reversibility requirement, in the same terms the criterion this
        // replaces stated it: ONE constant, set to infinity, and the criterion
        // is gone. No mode, no flag, no migration.
        //
        // It is a SKIP rather than a very wide band, and that distinction is
        // what this test exists to hold. `narrow` does not only filter on the
        // deadband — it also drops every candidate whose reading is `Unknown`.
        // So a criterion "disabled" by widening its band would go on silently
        // filtering the field on absence of evidence. That is not off.
        let key = |name: &str, pressure: Slack| PlacementKey {
            deaths: 0.0,
            cap: Slack::Unknown,
            pressure,
            cash: Slack::Unknown,
            placed: 0,
            momentum: false,
            is_current: false,
            has_session: false,
            rotation: 0,
            name: placeable(name),
        };
        let field = || {
            vec![
                key("measured", Slack::Known(9.0)),
                key("also-measured", Slack::Known(0.1)),
                key("never-polled", Slack::Unknown),
            ]
        };

        // ON: the criterion decides, and the unmeasured account drops.
        let mut on = field();
        narrow_unless_disabled(&mut on, PRESSURE_DEADBAND, |k| k.pressure);
        assert_eq!(on.len(), 1);
        assert_eq!(on[0].name, placeable("measured"));

        // OFF: the field is handed on EXACTLY as it arrived.
        let mut off = field();
        narrow_unless_disabled(&mut off, f64::INFINITY, |k| k.pressure);
        assert_eq!(
            off.len(),
            3,
            "an infinite deadband must skip the criterion outright, including \
             for the account with no reading at all"
        );

        // And this is why a wide band is NOT the off switch: it still eats the
        // unmeasured account. Pinned so nobody later "simplifies" the skip into
        // a big number.
        let mut wide = field();
        narrow(&mut wide, 1.0e30, |k| k.pressure);
        assert_eq!(
            wide.len(),
            2,
            "a merely-huge band still drops the unmeasured account — which is \
             the whole reason the off switch is a skip"
        );

        // The cap carries the identical switch.
        let mut cap_off = vec![
            key("x", Slack::Unknown),
            key("y", Slack::Unknown),
        ];
        cap_off[0].cap = Slack::Known(1.0);
        cap_off[1].cap = Slack::Known(0.0);
        narrow_unless_disabled(&mut cap_off, f64::INFINITY, |k| k.cap);
        assert_eq!(cap_off.len(), 2);
    }

    #[test]
    fn a_soon_resetting_window_with_unused_capacity_attracts_the_turn() {
        // 2026-08-21, reproduced from the live daemon at 16:50Z. Three
        // accounts: `owner-account` at 17% weekly with the window resetting in ~25
        // hours (83% of a week's budget about to evaporate), `cc1` at 94-95%
        // with ~53 hours still to run (its last few percent are the fleet's
        // burst reserve), `owner-gmail` at 78% with ~56 hours. Placement was
        // ranking them with NO notion of time remaining: at production
        // denominators (~20.5M tokens per weekly percent) every account with a
        // few percent of slack scores ~zero expected deaths, the whole field
        // ties inside DEATHS_DEADBAND, and the clock-blind tie-breaks decide.
        //
        // Operator, same day: "there's still two days left on the CC1... it
        // shouldn't even be using that one, because it's at 94% weekly
        // consumption. It should be saving that and keeping that session open
        // in case we have a burst... So the placement algorithm failed there.
        // It needs to take into account how much time is left in the window."
        let b = svc();
        let sam = "owner-account";
        let cc1 = "alice-example-com";
        let gmail = "owner-gmail";
        let now = crate::clock::now();
        let resets_in = |hours: i64| Some((now + ChronoDuration::hours(hours)).to_rfc3339());

        // Production-scale denominators, fully displacing the 20K-tokens/pct
        // prior (sample_count ≥ PRIOR_SAMPLES): with the prior, a point of
        // weekly slack is a fifth of a nominal turn and the survival term
        // would repel cc1 on its own — which is NOT what the live daemon saw.
        for name in [sam, cc1, gmail] {
            b.store
                .upsert_estimate(&crate::store::EstimateRow {
                    account: name.into(),
                    axis: Axis::WeeklyAll.as_str().into(),
                    tokens_per_percent: 20_500_000.0,
                    sample_count: 4,
                    last_obs_id: 0,
                    updated_at: crate::store::now_rfc3339(),
                })
                .unwrap();
        }
        b.observe(sam, Axis::WeeklyAll, 17.0, resets_in(25), "usage_poll")
            .unwrap();
        b.observe(cc1, Axis::WeeklyAll, 94.0, resets_in(53), "usage_poll")
            .unwrap();
        b.observe(gmail, Axis::WeeklyAll, 78.0, resets_in(56), "usage_poll")
            .unwrap();

        // The census is pointed AT the wrong answers on purpose: fewest
        // threads sit on cc1, then gmail, then sam — so if the clock does not
        // outrank spread, spread hands the turn to the account whose remaining
        // percent has two days of reserve duty left. (Live it was 483 on cc1
        // against sam's 4; the count here is adversarial, not documentary,
        // because settled threads never reach `place` at all — DEC-26.)
        for i in 0..4 {
            place_thread(&b, &format!("sam-{i}"), sam);
        }
        for i in 0..2 {
            place_thread(&b, &format!("gm-{i}"), gmail);
        }

        let th = Thresholds::default();
        let accounts = pool(&[cc1, sam, gmail]);

        // Pace: sam = 0.83 unused − 25/168 left = +0.68 (use it or lose it);
        // cc1 = 0.06 − 53/168 = −0.26 (reserve); gmail = 0.22 − 56/168 =
        // −0.11. The deaths band is a three-way tie (~0 each), so before the
        // pace criterion existed this fell to spread and picked cc1.
        assert_eq!(
            place(&b, &th, &accounts),
            Some(placeable(sam)),
            "83% of a weekly budget expiring in 25 hours must attract the turn \
             over accounts whose windows have days left"
        );

        // The repulsion, isolated: with sam out of the field, the 94%/53h
        // account must still lose to the 78%/56h one — its last percents are
        // two days of burst reserve — even though spread prefers it.
        let without_sam = pool(&[cc1, gmail]);
        assert_eq!(
            place(&b, &th, &without_sam),
            Some(placeable(gmail)),
            "an account at 94% with two days left in its window is the fleet's \
             reserve and must repel placement"
        );

        // And at the daemon table's exact 95%: zero slack against the default
        // weekly threshold means zero reach, the survival term itself repels
        // cc1, and the pace term still hands the turn to the expiring window.
        // (Recorded directly — `observe` would try to calibrate on the pair.)
        b.store
            .record_observation(&ObservationRow {
                id: 0,
                account: cc1.into(),
                axis: Axis::WeeklyAll.as_str().into(),
                pct: 95.0,
                resets_at: resets_in(53),
                source: "usage_poll".into(),
                observed_at: crate::store::now_rfc3339(),
            })
            .unwrap();
        assert_eq!(
            place(&b, &th, &accounts),
            Some(placeable(sam)),
            "at its threshold the reserve account is repelled by the objective \
             itself, and the expiring window still wins the tie"
        );
    }

    #[test]
    fn an_empty_account_with_no_reset_time_is_not_deprioritised_for_being_empty() {
        // THE PLACEMENT INVERSION, reproduced. The upstream gauge reports no
        // reset time for a pool the account has never consumed against — the
        // window does not exist until the first token is spent on it. Verified
        // in the live store: every one of the 6,843 null-`resets_at`
        // observations sits at exactly 0.0%, on all three axes, no exceptions.
        //
        // `weekly_pace` used to answer `Unknown` to that, and `narrow` drops an
        // Unknown the moment any other candidate has a reading — so the
        // EMPTIEST account in the fleet was struck off the placement front
        // precisely BECAUSE it was empty, and the work piled onto whichever
        // account was furthest through its week.
        let b = svc();
        let full = "nearly-spent";
        let empty = "freshly-rolled";
        let now = crate::clock::now();
        for name in [full, empty] {
            calibrate(&b, name, Axis::WeeklyAll, 20_500_000.0);
        }
        b.observe(
            full,
            Axis::WeeklyAll,
            94.0,
            Some((now + ChronoDuration::hours(53)).to_rfc3339()),
            "usage_poll",
        )
        .unwrap();
        b.observe(empty, Axis::WeeklyAll, 0.0, None, "usage_poll")
            .unwrap();

        // Spread is pointed at the wrong answer on purpose: the threads sit on
        // the EMPTY account, so if pace stays silent the clock-blind tie-breaks
        // hand the turn straight back to the 94% one.
        for i in 0..3 {
            place_thread(&b, &format!("e-{i}"), empty);
        }

        let th = Thresholds::default();
        let accounts = pool(&[full, empty]);
        // Pace: `full` = 0.06 unused − 53/168 of its window left = −0.26, an
        // account ahead of its clock and therefore the fleet's burst reserve;
        // `empty` = 1.00 unused − one whole ASSUMED week left = 0.00, exactly
        // on schedule. A quarter-point of pace apart, far outside
        // PACING_DEADBAND, so the empty account takes the front on its own and
        // never reaches the spread tie-break that would sink it.
        assert_eq!(
            place(&b, &th, &accounts),
            Some(placeable(empty)),
            "an account at 0% whose gauge reports no reset time must still win \
             placement over one at 94% — an absent window is the mark of a pool \
             nobody has touched yet, not a reason to avoid it"
        );
    }

    #[test]
    fn a_missing_reset_time_and_an_expired_one_are_different_windows() {
        // These two used to share one `_` arm in `window_start` and both
        // metered against a rolling window ending now. They mean opposite
        // things: MISSING means the pool has never been spent against, so its
        // window opened at the observation and up to a whole window of older
        // charges must NOT be counted against it; EXPIRED means the data is
        // old, and there the rolling window is still the honest fallback.
        let b = svc();
        let now = crate::clock::now();
        b.observe("fresh", Axis::Session, 0.0, None, "usage_poll")
            .unwrap();
        b.observe(
            "old",
            Axis::Session,
            99.0,
            Some("2000-01-01T00:00:00+00:00".into()),
            "usage_poll",
        )
        .unwrap();

        let fresh = b.window_start("fresh", Axis::Session, now);
        let old = b.window_start("old", Axis::Session, now);
        assert!(
            (now - fresh).num_seconds().abs() < 60,
            "a pool with no reset time has only just opened: its window starts \
             at the observation, not five hours before it (got {fresh})"
        );
        assert_eq!(
            (now - old).num_seconds(),
            gauge::SESSION_WINDOW_SECS,
            "an expired reset instant is stale data, and stale data still \
             meters against the rolling window"
        );
        assert_ne!(
            fresh, old,
            "NULL and expired must no longer collapse into one case"
        );
        assert!(b
            .effective_reset("fresh", Axis::Session, now)
            .unwrap()
            .is_assumed());
        assert_eq!(
            b.effective_reset("old", Axis::Session, now).unwrap().basis,
            ResetBasis::Observed
        );
    }

    #[test]
    fn the_fable_axis_borrows_the_weekly_reset_instant_when_its_own_is_missing() {
        // The two weekly axes are the SAME week: across 12,352 matched pairs in
        // the live store they always share a reset instant, and the only
        // differences are exactly 60 seconds — an upstream rendering artifact
        // of "12:59pm" against "1pm", already inside
        // `gauge::WINDOW_MATCH_TOLERANCE_SECS`. A borrowed instant is REAL, so
        // it is preferred over an assumed one.
        let b = svc();
        let now = crate::clock::now();
        let weekly_reset = now + ChronoDuration::hours(53);
        b.observe(
            "a",
            Axis::WeeklyAll,
            40.0,
            Some(weekly_reset.to_rfc3339()),
            "usage_poll",
        )
        .unwrap();
        b.observe("a", Axis::WeeklyFable, 0.0, None, "usage_poll")
            .unwrap();
        let borrowed = b.effective_reset("a", Axis::WeeklyFable, now).unwrap();
        assert_eq!(borrowed.basis, ResetBasis::Borrowed);
        assert_eq!(borrowed.at.timestamp(), weekly_reset.timestamp());

        // Nothing LIVE to borrow — a weekly reset already in the past cannot
        // place a window that is currently open — so fable assumes instead.
        b.observe(
            "b",
            Axis::WeeklyAll,
            90.0,
            Some("2000-01-01T00:00:00+00:00".into()),
            "usage_poll",
        )
        .unwrap();
        b.observe("b", Axis::WeeklyFable, 0.0, None, "usage_poll")
            .unwrap();
        assert!(b
            .effective_reset("b", Axis::WeeklyFable, now)
            .unwrap()
            .is_assumed());

        // And the session axis has no second five-hour pool to borrow from, so
        // it can only ever assume.
        b.observe("a", Axis::Session, 0.0, None, "usage_poll")
            .unwrap();
        assert!(b
            .effective_reset("a", Axis::Session, now)
            .unwrap()
            .is_assumed());
    }

    #[test]
    fn a_fleet_with_no_reset_instants_at_all_still_falls_through_to_spread() {
        // The reversibility property, restated for the assumed-window world.
        // Every observation here carries `resets_at: None` — exactly the state
        // of the whole pre-existing fleet — so every account is ranked on the
        // SAME assumption (a full window, opening now) and their paces differ
        // only by their percentages. Three points apart is inside
        // PACING_DEADBAND, so the criterion leaves the field untouched and the
        // spread tie-break decides, precisely as before.
        let b = svc();
        b.observe("a", Axis::WeeklyAll, 17.0, None, "usage_poll").unwrap();
        b.observe("b", Axis::WeeklyAll, 20.0, None, "usage_poll").unwrap();
        place_thread(&b, "t0", "a");
        let th = Thresholds::default();
        let accounts = pool(&["a", "b"]);
        assert_eq!(
            place(&b, &th, &accounts),
            Some(placeable("b")),
            "with no reset instant on record the clock says nothing and the \
             spread tie-break decides, exactly as it did before"
        );
    }

    #[test]
    fn dollars_do_not_outvote_percentages_in_placement() {
        // FIX 1. The tie-break used to ADD every axis's headroom into one
        // scalar, so a cash threshold measured in hundreds of dollars decided
        // the account by arithmetic magnitude alone: percentages max out at
        // 100 points and a $500 limit contributes up to 500.
        let b = svc();
        // `a`: session almost entirely free, cash all but spent.
        b.observe("a", Axis::Session, 10.0, None, "usage_poll").unwrap();
        b.observe("a", Axis::Cash, 499.0, None, "usage_poll").unwrap();
        // `b`: the session nearly gone, the cash untouched.
        b.observe("b", Axis::Session, 85.0, None, "usage_poll").unwrap();
        b.observe("b", Axis::Cash, 0.0, None, "usage_poll").unwrap();
        // `ungated()`, not `default()`: this test is about SESSION against
        // CASH, and `Thresholds::default()` now carries a weekly gate
        // (`DEFAULT_WEEKLY_THRESHOLD_PCT`). Leaving it in would not weaken the
        // test, it would change its subject — the dual-currency adapter would
        // convert `a`'s $499 into a weekly percent and block `a` on a third
        // axis, so the assertion below would be measuring the adapter rather
        // than the tie-break.
        let th = Thresholds {
            session: Some(90.0),
            cash: Some(500.0),
            ..Thresholds::ungated()
        };
        let accounts = pool(&["a", "b"]);
        // Summed: b scored 5 + 500 = 505 against a's 80 + 1 = 81, so the turn
        // went to the account with five points of session left. Compared axis
        // by axis, percentages against percentages, `a` wins on the session
        // and the dollars never enter that comparison at all.
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "gone", &[])).unwrap(),
            Admission::Admit {
                switch_to: Some(placeable("a"))
            }
        );
    }

    #[test]
    fn the_weekly_axes_rank_by_the_tightest_one_never_by_their_sum() {
        // FIX 1, the same defect between two axes of the SAME unit: a thread
        // gated on both weekly pools runs out at whichever empties first, so
        // the minimum is the quantity that means something and the sum is not.
        let b = svc();
        // `a` looks rich in total (80 + 2) but is two points from its fable
        // gate; `b` is comfortable on both (40 + 40).
        b.observe("a", Axis::WeeklyAll, 0.0, None, "usage_poll").unwrap();
        b.observe("a", Axis::WeeklyFable, 58.0, None, "usage_poll").unwrap();
        b.observe("b", Axis::WeeklyAll, 40.0, None, "usage_poll").unwrap();
        b.observe("b", Axis::WeeklyFable, 20.0, None, "usage_poll").unwrap();
        let th = Thresholds {
            weekly: Some(80.0),
            fable: Some(60.0),
            ..Default::default()
        };
        let accounts = pool(&["a", "b"]);
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "gone", &[])).unwrap(),
            Admission::Admit {
                switch_to: Some(placeable("b"))
            }
        );
    }

    #[test]
    fn a_never_polled_account_no_longer_wins_every_placement() {
        // FIX 2. An unread gauge used to contribute its FULL threshold to the
        // headroom sum, which is arithmetically identical to reading 0%: an
        // account we know nothing about scored as maximally empty and beat
        // every account we had actually measured. A freshly registered
        // account is in exactly that state.
        let b = svc();
        b.observe("measured", Axis::Session, 40.0, None, "usage_poll")
            .unwrap();
        let th = Thresholds {
            session: Some(50.0),
            ..Default::default()
        };
        let accounts = pool(&["fresh", "measured"]);
        // Old: fresh scored 50 against measured's 10 and took the turn.
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "gone", &[])).unwrap(),
            Admission::Admit {
                switch_to: Some(placeable("measured"))
            },
            "an unknown reading is not an empty account"
        );
        // Evidence only settles the axes it exists on: once `fresh` is read
        // and is genuinely emptier, it wins on the merits.
        b.observe("fresh", Axis::Session, 5.0, None, "usage_poll")
            .unwrap();
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "gone", &[])).unwrap(),
            Admission::Admit {
                switch_to: Some(placeable("fresh"))
            }
        );
    }

    #[test]
    fn an_unobserved_axis_never_decides_a_placement_for_either_account() {
        // FIX 2, the other half: when NOBODY has a reading on an axis, that
        // axis is silent rather than scoring every candidate as empty, and
        // the next criterion decides. Here the session is unread everywhere
        // and the week is what is actually known.
        let b = svc();
        b.observe("a", Axis::WeeklyAll, 70.0, None, "usage_poll").unwrap();
        b.observe("b", Axis::WeeklyAll, 5.0, None, "usage_poll").unwrap();
        let th = Thresholds {
            session: Some(90.0),
            weekly: Some(80.0),
            ..Default::default()
        };
        let accounts = pool(&["a", "b"]);
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "gone", &[])).unwrap(),
            Admission::Admit {
                switch_to: Some(placeable("b"))
            }
        );
    }

    #[test]
    fn an_exact_tie_spreads_by_fewest_threads_then_the_cursor_then_the_name() {
        // FIX 3. With nothing to choose between two accounts the old rule
        // broke the tie by NAME — so every thread landed on the alphabetically
        // first account, forever. That is the herding that jitter was proposed
        // for; this is the deterministic answer instead.
        let b = svc();
        let th = Thresholds::ungated();
        let accounts = pool(&["a", "b"]);

        // The cursor alternates rather than repeating.
        assert_eq!(place(&b, &th, &accounts).as_deref(), Some("a"));
        assert_eq!(
            place(&b, &th, &accounts).as_deref(),
            Some("b"),
            "the second placement into an exact tie must not repeat the first"
        );

        // A real placement outranks the cursor: two threads on `a` send the
        // next four somewhere else no matter where the cursor stands.
        place_thread(&b, "t1", "a");
        place_thread(&b, "t2", "a");
        for _ in 0..4 {
            assert_eq!(place(&b, &th, &accounts).as_deref(), Some("b"));
        }
        // …and it is a SPREAD, not a preference: once `b` carries more, the
        // next placement comes back.
        for i in 0..3 {
            place_thread(&b, &format!("u{i}"), "b");
        }
        assert_eq!(place(&b, &th, &accounts).as_deref(), Some("a"));

        // Nothing here is random: the same state gives the same answer.
        let again = place(&b, &th, &accounts);
        assert_eq!(again.as_deref(), Some("a"));
    }

    #[test]
    fn the_round_robin_cursor_survives_a_restart() {
        // FIX 3: "persistent" is the load-bearing word — a cursor that reset
        // on every daemon start would send the first thread of every boot to
        // the same account.
        let store = Store::open_in_memory().unwrap();
        let b = Budgets::new(store.clone());
        let th = Thresholds::ungated();
        let accounts = pool(&["a", "b"]);
        assert_eq!(place(&b, &th, &accounts).as_deref(), Some("a"));
        // A brand-new service over the SAME store is what a restart looks like.
        let after_restart = Budgets::new(store);
        assert_eq!(place(&after_restart, &th, &accounts).as_deref(), Some("b"));
    }

    #[test]
    fn an_ungated_thread_is_placed_too_because_selection_is_not_gating() {
        // FIX 4, the half that lives in this file. A thread with no budget key
        // and no thresholds — every AUI thread — has nothing to ENFORCE, and
        // the old gate concluded from that that it had nothing to DECIDE
        // either: `a` is admissible, stay on `a`. So the account was whatever
        // the create-time alphabetical fallback picked, forever.
        let b = svc();
        let th = Thresholds::ungated();
        let accounts = pool(&["a", "b"]);
        place_thread(&b, "t1", "a");
        place_thread(&b, "t2", "a");
        place_thread(&b, "me", "a");
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "a", &[])).unwrap(),
            Admission::Admit {
                switch_to: Some(placeable("b"))
            }
        );
    }

    #[test]
    fn placement_leaves_a_settled_thread_exactly_where_it_is() {
        // The other side of fix 4: placing on every admission must not become
        // churn. An exact tie stays put — including on the account that does
        // NOT sort first — and a thread's own placement never counts against
        // the account it is already on, or spread would push it off and
        // straight back every turn.
        let b = svc();
        let th = Thresholds::ungated();
        let accounts = pool(&["a", "b"]);
        place_thread(&b, "me", "b");
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "b", &[])).unwrap(),
            Admission::Admit { switch_to: None }
        );
        // A thread WITH a conversation is ALSO left alone by an exact tie —
        // and for the same reason, the anti-churn tie-break, not because it is
        // exempt from placement. `a` and `b` are indistinguishable here and it
        // has history only on `b`, so there is nothing to move it for.
        let history = names(&["b"]);
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "b", &history)).unwrap(),
            Admission::Admit { switch_to: None }
        );
    }

    #[test]
    fn a_thread_with_a_conversation_is_re_ranked_and_not_pinned_by_mere_legality() {
        // ── THE STICKINESS DEFECT, PINNED FROM THE FAILING SIDE ──
        //
        // This is the inverse of the assertion that used to sit at the bottom
        // of `placement_leaves_a_settled_thread_exactly_where_it_is`, which
        // read "a thread WITH a conversation is never re-placed at all, however
        // much emptier the other account is (DEC-26)". That assertion described
        // the `} else if on_admissible { None }` gate, and the gate is the bug:
        // *"if it always sends to the same account, there's a bug."*
        //
        // The setup is deliberately the one the old gate SURVIVED: the thread's
        // current account is perfectly legal — no threshold crossed, no bar, no
        // dead login, not over its share — and merely WORSE than an account the
        // thread already has a session on. Legality is exactly what the gate
        // short-circuited on, so a test whose current account is illegal proves
        // nothing about it, and neither does one with no conversation at all.
        let b = svc();
        let th = Thresholds::ungated();
        let accounts = pool(&["a", "b"]);
        // Both accounts are far from any ceiling — this is not a gate test.
        b.observe("a", Axis::Session, 2.0, None, "usage_poll").unwrap();
        b.observe("b", Axis::Session, 60.0, None, "usage_poll").unwrap();
        // The thread has talked on both, so continuity costs nothing either way
        // and the ranking is the only thing that can decide.
        let history = names(&["a", "b"]);
        // It is sitting on `b`, the worse account, with a conversation behind
        // it. Under the gate this returned `switch_to: None` forever.
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "b", &history)).unwrap(),
            Admission::Admit {
                switch_to: Some(placeable("a"))
            },
            "a thread with a conversation must be re-ranked every turn; \
             `where it sits is still legal` is not a reason to keep it there"
        );
        // And the reverse, so this is a RANKING and not a new pin: put it on
        // the good account and it stays, because nothing better exists.
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "a", &history)).unwrap(),
            Admission::Admit { switch_to: None },
            "re-ranking is not churn: the best account keeps the thread"
        );
    }

    #[test]
    fn a_live_thread_on_a_merely_legal_account_moves_to_the_most_pressured_one() {
        // ── THE CASE HE SAW, msg 342 ──
        //
        // *"I saw that it placed this running thread onto ALICE at Example COM,
        // which was the one with just medium pressure… You rebuild the
        // placement and it is failing."*
        //
        // Three accounts at three DIFFERENT pressures, and a LIVE thread — one
        // with a conversation already on it — parked on the middle one, which
        // is entirely legal. The rebuilt cascade says the highest pressure
        // takes the turn: the account that must burn fastest to spend the share
        // it was lent before its window resets and destroys the remainder. The
        // gate meant that cascade was computed and then thrown away for every
        // thread old enough to have said anything.
        let b = svc();
        let th = Thresholds::ungated();
        let accounts = pool(&["cc1", "gmail", "sam"]);
        flatten_deaths(&b, &["cc1", "gmail", "sam"]);
        // Equal shares, so pressure is decided by the numerator and the clock
        // and not by a denominator that differs per account.
        for a in ["cc1", "gmail", "sam"] {
            set_share(&b, a, 1.0);
        }
        // One clock for all three: half the weekly window still to run, so
        // `frac_left` is the same for every account and pressure is ordered by
        // unused share alone. HIGH pressure = lots left to burn and not long to
        // burn it in = the account that should take the turn.
        let half_window =
            (crate::clock::now() + chrono::Duration::seconds(Axis::WeeklyAll.window_secs() / 2)).to_rfc3339();
        // `sam` has burned almost everything: least to spend, LOWEST pressure.
        b.observe("sam", Axis::WeeklyAll, 92.0, Some(half_window.clone()), "usage_poll")
            .unwrap();
        // `cc1` in the middle — the one he watched the thread land on.
        b.observe("cc1", Axis::WeeklyAll, 50.0, Some(half_window.clone()), "usage_poll")
            .unwrap();
        // `gmail` has barely been touched: HIGHEST pressure, and it is the one
        // whose unused share the reset is about to destroy.
        b.observe("gmail", Axis::WeeklyAll, 6.0, Some(half_window), "usage_poll")
            .unwrap();
        // Sanity: the fixture is discriminating. If these three collapse into
        // one deadband the assertion below proves nothing.
        let now = crate::clock::now();
        let p = |a: &str| b.pressure_reading(a, now).pressure.expect("ranked");
        assert!(
            p("gmail") > p("cc1") && p("cc1") > p("sam"),
            "fixture must produce three distinct pressures, got gmail={} cc1={} sam={}",
            p("gmail"),
            p("cc1"),
            p("sam")
        );
        assert!(
            p("gmail") - p("cc1") > PRESSURE_DEADBAND,
            "the gap must exceed the deadband or pressure abstains and this \
             test is measuring the rotation cursor"
        );
        // THE LIVE THREAD. It has a conversation on all three, it is sitting on
        // `cc1`, and `cc1` is perfectly legal. This is his screen.
        let history = names(&["cc1", "gmail", "sam"]);
        place_thread(&b, "live", "cc1");
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "cc1", &history)).unwrap(),
            Admission::Admit {
                switch_to: Some(placeable("gmail"))
            },
            "the running thread must move off the merely-legal middle account \
             onto the most pressured one — this is msg 342"
        );
    }

    #[test]
    fn re_ranking_never_moves_a_thread_off_the_only_account_it_can_use() {
        // The floor under the fix. Removing the gate must not turn "stay where
        // you legally are" into a continuity park: a thread whose only usable
        // account is the one it is on keeps running there.
        let b = svc();
        b.observe("a", Axis::Session, 10.0, None, "usage_poll").unwrap();
        b.observe("b", Axis::Session, 90.0, None, "usage_poll").unwrap();
        let th = Thresholds {
            session: Some(50.0),
            ..Default::default()
        };
        let accounts = pool(&["a", "b"]);
        // History on `a` only, sitting on `a`, and `b` is over its threshold.
        let history = names(&["a"]);
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "a", &history)).unwrap(),
            Admission::Admit { switch_to: None }
        );
    }

    #[test]
    fn work_already_on_an_over_share_account_finishes_rather_than_parking() {
        // ── THE ASYMMETRY, AND THE ARM THAT CARRIES IT ──
        //
        // *"Overage we've determined is okay while the current workload is
        // finishing. But then it needs to be like hard clamped — not available
        // for placement as well."*
        //
        // The clamp removes an account from TARGETS and leaves it in CLEARED,
        // so once the gate stopped short-circuiting on `on_admissible` the
        // question became live: a thread whose current account is clamped is
        // now ranked over a field that account has been subtracted from, and if
        // nothing else can take it the ranker returns nothing at all. Without
        // the `None if on_admissible => None` arm that becomes a CONTINUITY
        // PARK — the fix converting "he has spent past his share" into "this
        // thread stops running", which is precisely the reading he ruled out.
        //
        // Written because the guard test beside it did NOT catch this: removing
        // the arm left `re_ranking_never_moves_a_thread_off_the_only_account_it
        // _can_use` green, because there the single candidate survives the clamp
        // and `place` returns it. Only a CLAMPED current account empties the
        // field, so only this fixture reaches the arm. (Found by mutation; the
        // surviving mutant is written down here rather than left for someone.)
        let b = svc();
        let now = crate::clock::now();
        let resets_in = Some((now + ChronoDuration::hours(84)).to_rfc3339());
        flatten_deaths(&b, &["gmail"]);
        set_share(&b, "gmail", 0.95);
        b.observe("gmail", Axis::WeeklyAll, 100.0, resets_in, "usage_poll")
            .unwrap();
        let th = Thresholds::ungated();
        let accounts = pool(&["gmail"]);
        let history = names(&["gmail"]);
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "gmail", &history)).unwrap(),
            Admission::Admit { switch_to: None },
            "a thread already on an over-share account keeps running there when \
             there is nowhere else it may go — the clamp stops NEW placement, \
             it does not stop the work that is already there"
        );
    }

    #[test]
    fn a_thread_switched_last_turn_does_not_bounce_back_before_history_catches_up() {
        // History is written when a turn ENDS, so for exactly one turn after a
        // switch the thread sits on an account its history does not name. If
        // the ranking field were history-only it would be judged over a set
        // that excludes where it is and would be sent straight back — churn
        // manufactured by the fix. `clear`'s current account closes that hole.
        let b = svc();
        let th = Thresholds::ungated();
        let accounts = pool(&["a", "b"]);
        // `b` is the better account and the thread was just moved there; its
        // history still says `a` only.
        b.observe("a", Axis::Session, 70.0, None, "usage_poll").unwrap();
        b.observe("b", Axis::Session, 2.0, None, "usage_poll").unwrap();
        let history = names(&["a"]);
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "b", &history)).unwrap(),
            Admission::Admit { switch_to: None },
            "the account it was just switched onto must be a candidate even \
             though no turn has ended there yet"
        );
    }

    #[test]
    fn a_nearly_closed_session_stops_attracting_work_however_free_the_week_is() {
        // **INTENT 83 SUPERSEDES THIS TEST'S OLD ASSERTION, deliberately.**
        //
        // It used to be called `a_near_tie_on_the_week_falls_through_to_the
        // _session` and it pinned a LEXICOGRAPHIC rule: the week decided first
        // and the session only ever broke a weekly near-tie, so an account with
        // a wide-open week and five points of session left still won every
        // placement outright.
        //
        // The operator's correction (idx `2951`): *"You'll want to keep it away
        // from an account nearing a session limit, but that should just be a
        // natural falling out of… the measurement that it will hit the second
        // limit."* The reachable budget is therefore the MIN across the axes,
        // and this test now pins that — with no `if` anywhere naming a session
        // limit or a nearly-closed account.
        let b = svc();
        b.observe("a", Axis::WeeklyAll, 20.0, None, "usage_poll").unwrap();
        b.observe("b", Axis::WeeklyAll, 21.0, None, "usage_poll").unwrap();
        b.observe("a", Axis::Session, 90.0, None, "usage_poll").unwrap();
        b.observe("b", Axis::Session, 3.0, None, "usage_poll").unwrap();
        let th = Thresholds {
            weekly: Some(80.0),
            session: Some(95.0),
            ..Default::default()
        };
        let accounts = pool(&["a", "b"]);
        // `a` has the freer week by a point and five points of session; `b` has
        // ninety-two points of session. `b` can actually carry the turn.
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "gone", &[])).unwrap(),
            Admission::Admit {
                switch_to: Some(placeable("b"))
            }
        );
        // AND THIS IS THE HALF THAT CHANGED. Opening `a`'s week all the way up
        // does NOT win it the placement back: its session is still nearly shut,
        // so the amount it can actually spend before the nearest window closes
        // has not moved at all. Under the old rule a decisive weekly gap
        // overrode the session and `a` took the turn.
        b.observe("a", Axis::WeeklyAll, 2.0, None, "usage_poll").unwrap();
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "gone", &[])).unwrap(),
            Admission::Admit {
                switch_to: Some(placeable("b"))
            },
            "reachable budget is the MIN across the axes, so weekly headroom \
             cannot buy back a session that is about to close"
        );
        // And the converse, so the min is pinned in both directions: give `a`
        // its session back and it wins, because now BOTH of its axes are free.
        b.observe("a", Axis::Session, 1.0, None, "usage_poll").unwrap();
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "gone", &[])).unwrap(),
            Admission::Admit {
                switch_to: Some(placeable("a"))
            }
        );
    }

    // ── park for continuity (round-7 item 7) ─────────────────────────────

    #[test]
    fn a_thread_with_history_parks_rather_than_losing_its_conversation() {
        let b = svc();
        b.observe("a", Axis::Session, 90.0, None, "usage_poll")
            .unwrap();
        b.observe("b", Axis::Session, 10.0, None, "usage_poll")
            .unwrap();
        let th = Thresholds {
            session: Some(50.0),
            ..Default::default()
        };
        let accounts = pool(&["a", "b"]);
        let history = names(&["a"]); // it has only ever talked on `a`

        match b
            .admit(&req(None, &th, &accounts, "a", &history))
            .unwrap()
        {
            Admission::Park { cause, reason, .. } => {
                assert_eq!(cause, ParkCause::Continuity);
                assert!(reason.contains("history"), "{reason}");
            }
            other => panic!("expected a continuity park, got {other:?}"),
        }
    }

    #[test]
    fn a_thread_moves_to_an_admissible_account_it_already_has_history_on() {
        // The park is for continuity, not for immobility: if the conversation
        // already exists on an admissible account, go there.
        let b = svc();
        b.observe("a", Axis::Session, 90.0, None, "usage_poll")
            .unwrap();
        b.observe("b", Axis::Session, 10.0, None, "usage_poll")
            .unwrap();
        b.observe("c", Axis::Session, 5.0, None, "usage_poll").unwrap();
        let th = Thresholds {
            session: Some(50.0),
            ..Default::default()
        };
        let accounts = pool(&["a", "b", "c"]);
        let history = names(&["a", "b"]);
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "a", &history)).unwrap(),
            Admission::Admit {
                switch_to: Some(placeable("b"))
            }
        );
    }

    // ── calibration ──────────────────────────────────────────────────────

    /// **THE OVERLAP-WINDOW METHOD, ON A WINDOW THE CLOCK CANNOT COLLAPSE.**
    ///
    /// This read `Utc::now()` four times — twice inside `record_usage` alone,
    /// once for `started_at` and once for the row's `ts` — and under ANY
    /// optimized build both landed in the same microsecond. The execution window
    /// became `start == end`, `evaluate_window` rejected it `Unbracketed`, and
    /// `get_estimate` answered `None`: the `unwrap()` below panicked. It passed
    /// in debug only because debug codegen is slow enough for the clock to tick
    /// between two back-to-back calls, which is not a property a suite may rest
    /// on. The fix is at the cause and it is in `svc()`, which puts this whole
    /// module on the clock seam: real time, never repeating. See `src/clock.rs`.
    #[test]
    fn calibration_is_learned_from_a_real_observation_pair() {
        let b = svc();
        let reset = reset_for(Axis::WeeklyAll);
        b.observe(
            "a",
            Axis::WeeklyAll,
            50.0,
            Some(reset.clone()),
            "usage_poll",
        )
        .unwrap();
        b.store
            .record_usage(&crate::store::UsageRow {
                thread_id: "t1".into(),
                account: Some("a".into()),
                model: "claude-sonnet-5".into(),
                input_tokens: 1_000_000,
                started_at: Some(crate::store::now_rfc3339()),
                ..Default::default()
            })
            .unwrap();
        b.observe("a", Axis::WeeklyAll, 60.0, Some(reset), "usage_poll")
            .unwrap();

        let est = b.store.get_estimate("a", "weekly_all").unwrap().unwrap();
        assert!((est.tokens_per_percent - 100_000.0).abs() < 1.0);
        assert_eq!(est.sample_count, 1);
    }

    #[test]
    fn a_window_rollover_never_produces_a_calibration_sample() {
        let b = svc();
        // The session rolls over BETWEEN the two polls: the reset instant moves
        // by the window length, four orders of magnitude past the jitter
        // tolerance, and the percent falls rather than rises.
        let before = (crate::clock::now() + chrono::Duration::minutes(30)).to_rfc3339();
        let after = (crate::clock::now() + chrono::Duration::minutes(30) + chrono::Duration::hours(5))
            .to_rfc3339();
        b.observe("a", Axis::Session, 90.0, Some(before), "usage_poll")
            .unwrap();
        b.store
            .record_usage(&crate::store::UsageRow {
                thread_id: "t1".into(),
                account: Some("a".into()),
                model: "claude-sonnet-5".into(),
                input_tokens: 1_000_000,
                started_at: Some(crate::store::now_rfc3339()),
                ..Default::default()
            })
            .unwrap();
        b.observe("a", Axis::Session, 5.0, Some(after), "usage_poll")
            .unwrap();
        assert!(b.store.get_estimate("a", "session").unwrap().is_none());
        // …and for the RIGHT reason: the pair spans a rollover, not merely a
        // delta that failed to clear granularity.
        let verdict = b
            .calibrate_overlap_window("a", Axis::Session, crate::clock::now())
            .unwrap();
        assert!(
            matches!(
                verdict,
                WindowVerdict::Rejected {
                    reason: gauge::WindowRejection::Rollover,
                    ..
                }
            ),
            "{verdict:?}"
        );
    }

    #[test]
    fn a_429_observes_but_never_calibrates() {
        let b = svc();
        let reset = reset_for(Axis::Session);
        b.observe("a", Axis::Session, 40.0, Some(reset.clone()), "usage_poll")
            .unwrap();
        b.store
            .record_usage(&crate::store::UsageRow {
                thread_id: "t1".into(),
                account: Some("a".into()),
                model: "claude-sonnet-5".into(),
                input_tokens: 1_000_000,
                started_at: Some(crate::store::now_rfc3339()),
                ..Default::default()
            })
            .unwrap();
        b.observe(
            "a",
            Axis::Session,
            100.0,
            Some(reset.clone()),
            "limit_hit",
        )
        .unwrap();
        assert_eq!(b.observed_value("a", Axis::Session).unwrap(), Some(100.0));
        assert!(b.store.get_estimate("a", "session").unwrap().is_none());

        // And it cannot calibrate through the back door either: a REAL poll
        // after the refusal must not pair the earlier reading against the
        // cliff's fabricated 100%, nor across the reset the cliff proves
        // happened. (The 429 row was the `after` bracket before this was
        // enforced — 1M tokens against a 60-point "rise".)
        b.observe("a", Axis::Session, 45.0, Some(reset), "usage_poll")
            .unwrap();
        assert!(
            b.store.get_estimate("a", "session").unwrap().is_none(),
            "a 429 in the middle of a bracket may never produce a sample"
        );
    }

    // ── the overlap-window method end to end (operator msg #620) ─────────

    /// Two of our own executions that overlap merge into ONE window and
    /// calibrate from their SUMMED tokens — not two separately-too-small
    /// samples, and not two separate estimates.
    #[test]
    fn overlapping_executions_merge_into_one_window_and_calibrate_once() {
        let b = svc();
        let reset = reset_for(Axis::Session);
        b.observe("a", Axis::Session, 4.0, Some(reset.clone()), "usage_poll")
            .unwrap();
        let start = crate::store::now_rfc3339();
        b.store
            .record_usage(&crate::store::UsageRow {
                thread_id: "t1".into(),
                account: Some("a".into()),
                model: "claude-sonnet-5".into(),
                input_tokens: 300_000,
                started_at: Some(start.clone()),
                ..Default::default()
            })
            .unwrap();
        // A second execution that STARTS AT THE SAME INSTANT — the two
        // overlap outright, so they must merge into one window.
        b.store
            .record_usage(&crate::store::UsageRow {
                thread_id: "t2".into(),
                account: Some("a".into()),
                model: "claude-sonnet-5".into(),
                input_tokens: 200_000,
                started_at: Some(start),
                ..Default::default()
            })
            .unwrap();
        b.observe("a", Axis::Session, 9.0, Some(reset), "usage_poll")
            .unwrap();

        let est = b.store.get_estimate("a", "session").unwrap().unwrap();
        assert_eq!(est.sample_count, 1);
        // The SUM of both executions' tokens over the 5-point rise.
        assert!((est.tokens_per_percent - 100_000.0).abs() < 1.0);
    }

    /// THE self-diagnosis rule, end to end: two of our own executions on one
    /// account, with the gauge seen moving BETWEEN them while neither of ours
    /// was running — exactly the signature of a third party (the operator's
    /// own AUI, in the live case) sharing the account. The second
    /// execution's window must be REFUSED rather than folded in as if the
    /// whole delta were ours (which is precisely how the 2026-07-29 samples
    /// came out 2-50x wrong).
    #[test]
    fn idle_movement_between_two_of_our_own_executions_blocks_the_next_window() {
        let b = svc();
        let reset = reset_for(Axis::Session);
        let poll = |pct: f64| {
            b.observe("a", Axis::Session, pct, Some(reset.clone()), "usage_poll")
                .unwrap()
        };
        poll(4.0);
        b.store
            .record_usage(&crate::store::UsageRow {
                thread_id: "t1".into(),
                account: Some("a".into()),
                model: "claude-sonnet-5".into(),
                input_tokens: 200_000,
                started_at: Some(crate::store::now_rfc3339()),
                ..Default::default()
            })
            .unwrap();
        // This first pair calibrates cleanly.
        poll(6.0);
        let first = b.store.get_estimate("a", "session").unwrap().unwrap();
        assert_eq!(first.sample_count, 1);

        // The gauge keeps moving with NOTHING of ours running.
        poll(9.0);

        // A second execution follows; its own bracket polls look clean in
        // isolation, but the idle gap immediately before it was not quiet.
        b.store
            .record_usage(&crate::store::UsageRow {
                thread_id: "t2".into(),
                account: Some("a".into()),
                model: "claude-sonnet-5".into(),
                input_tokens: 100_000,
                started_at: Some(crate::store::now_rfc3339()),
                ..Default::default()
            })
            .unwrap();
        poll(11.0);

        // Re-evaluate explicitly (idempotent) to see WHY, not just THAT.
        let verdict = b
            .calibrate_overlap_window("a", Axis::Session, crate::clock::now())
            .unwrap();
        assert!(
            matches!(
                verdict,
                WindowVerdict::Rejected {
                    reason: gauge::WindowRejection::IdleMovement,
                    ..
                }
            ),
            "{verdict:?}"
        );

        // The estimate is UNCHANGED — the second window never folded in.
        let after = b.store.get_estimate("a", "session").unwrap().unwrap();
        assert_eq!(after.sample_count, 1);
        assert!((after.tokens_per_percent - first.tokens_per_percent).abs() < 1e-9);
    }

    /// TWO executions, ONE poll on either side and nothing in between — the
    /// commonest shape there is once the background sweep polls on an interval
    /// wider than a turn. The newest window's tokens are its own, but the
    /// delta those two polls measure covers BOTH executions, so pairing them
    /// records a sample that is wrong by the ratio between them (here 5x).
    /// No sample is the only honest answer until a poll lands in the gap.
    #[test]
    fn two_executions_with_one_poll_around_them_never_pool_into_one_sample() {
        let b = svc();
        let reset = reset_for(Axis::Session);
        b.observe("a", Axis::Session, 4.0, Some(reset.clone()), "usage_poll")
            .unwrap();
        for (thread, tokens) in [("t1", 400_000u64), ("t2", 100_000)] {
            b.store
                .record_usage(&crate::store::UsageRow {
                    thread_id: thread.into(),
                    account: Some("a".into()),
                    model: "claude-sonnet-5".into(),
                    input_tokens: tokens,
                    started_at: Some(crate::store::now_rfc3339()),
                    ..Default::default()
                })
                .unwrap();
        }
        b.observe("a", Axis::Session, 9.0, Some(reset), "usage_poll")
            .unwrap();

        let verdict = b
            .calibrate_overlap_window("a", Axis::Session, crate::clock::now())
            .unwrap();
        assert!(
            matches!(
                verdict,
                WindowVerdict::Rejected {
                    reason: gauge::WindowRejection::Unbracketed,
                    ..
                }
            ),
            "{verdict:?}"
        );
        assert!(
            b.store.get_estimate("a", "session").unwrap().is_none(),
            "100_000 tokens against a 5-point rise 500_000 tokens caused would \
             record 20_000/percent — five times too small"
        );
    }

    /// A poll taken while a SECOND execution of ours is still in flight cannot
    /// close the first one's bracket: the in-flight turn's tokens are not on
    /// the ledger yet (its row is written from its `result_turn`), so the
    /// reading includes burn the token side does not. This is the shape the
    /// background sweep produces every time it polls through a running turn.
    #[test]
    fn a_poll_during_an_in_flight_execution_never_closes_a_bracket() {
        let b = svc();
        let reset = reset_for(Axis::Session);
        b.observe("a", Axis::Session, 4.0, Some(reset.clone()), "usage_poll")
            .unwrap();
        b.store
            .record_usage(&crate::store::UsageRow {
                thread_id: "t1".into(),
                account: Some("a".into()),
                model: "claude-sonnet-5".into(),
                input_tokens: 100_000,
                started_at: Some(crate::store::now_rfc3339()),
                ..Default::default()
            })
            .unwrap();
        // t2 is spawned and RUNNING — this test's own pid stands in for its
        // live child, since the guard only counts a turn whose process exists.
        b.store
            .upsert_running_turn(&crate::store::RunningTurnRow {
                thread_id: "t2".into(),
                pid: std::process::id() as i64,
                spawn_cookie: "cookie".into(),
                output_path: "/dev/null".into(),
                byte_offset: 0,
                started_at: crate::store::now_rfc3339(),
                prompt: None,
                message_ids: vec![],
                attempt: 1,
            })
            .unwrap();
        b.store.record_thread_account("t2", "a").unwrap();

        b.observe("a", Axis::Session, 9.0, Some(reset.clone()), "usage_poll")
            .unwrap();
        assert!(
            b.store.get_estimate("a", "session").unwrap().is_none(),
            "t2's unrecorded burn is inside this reading — 100_000 tokens \
             against the whole 5-point rise would be a wrong sample"
        );

        // Once t2 lands, the ledger is complete again and the NEXT window can
        // calibrate on its own merits.
        b.store.clear_running_turn("t2").unwrap();
        b.store
            .record_usage(&crate::store::UsageRow {
                thread_id: "t2".into(),
                account: Some("a".into()),
                model: "claude-sonnet-5".into(),
                input_tokens: 300_000,
                started_at: Some(crate::store::now_rfc3339()),
                ..Default::default()
            })
            .unwrap();
        b.observe("a", Axis::Session, 12.0, Some(reset), "usage_poll")
            .unwrap();
        let est = b.store.get_estimate("a", "session").unwrap().unwrap();
        // Bracketed by the 9% and 12% polls: 300_000 tokens over 3 points.
        assert_eq!(est.sample_count, 1);
        assert!(
            (est.tokens_per_percent - 100_000.0).abs() < 1.0,
            "{est:?}"
        );
    }

    /// Cent-scale spend that never clears the gauge's own 1% granularity
    /// produces NO sample — the exact obstacle the live spare-account
    /// calibration attempt has to reckon with, reproduced deterministically.
    ///
    /// **AND THE REJECTION MUST BE THE RIGHT ONE.** The whole point of this test
    /// is `BelowGranularity` — the gauge cannot represent cent-scale spend as a
    /// rise. Under an optimized build it got `Unbracketed` instead, from a
    /// zero-width window, so it was failing while *looking* like it was still
    /// testing granularity. A widened tolerance would have hidden exactly that.
    /// The ticking clock keeps the window real, which is what leaves
    /// `BelowGranularity` as the only reason left. The ticking clock comes from
    /// `svc()`, which puts every test in this module on the seam.
    #[test]
    fn spend_too_small_to_move_the_gauge_yields_no_sample() {
        let b = svc();
        let reset = reset_for(Axis::Session);
        b.observe("a", Axis::Session, 4.0, Some(reset.clone()), "usage_poll")
            .unwrap();
        b.store
            .record_usage(&crate::store::UsageRow {
                thread_id: "t1".into(),
                account: Some("a".into()),
                model: "claude-haiku-4-5".into(),
                input_tokens: 500,
                started_at: Some(crate::store::now_rfc3339()),
                ..Default::default()
            })
            .unwrap();
        // Same reading: cent-scale spend that the whole-percent gauge cannot
        // represent as a rise.
        b.observe("a", Axis::Session, 4.0, Some(reset), "usage_poll")
            .unwrap();
        let verdict = b
            .calibrate_overlap_window("a", Axis::Session, crate::clock::now())
            .unwrap();
        assert!(
            matches!(
                verdict,
                WindowVerdict::Rejected {
                    reason: gauge::WindowRejection::BelowGranularity,
                    ..
                }
            ),
            "{verdict:?}"
        );
        assert!(b.store.get_estimate("a", "session").unwrap().is_none());
    }

    /// **WHAT A ZERO-WIDTH EXECUTION WINDOW ACTUALLY DOES — the case nothing
    /// could assert before the clock seam existed.**
    ///
    /// Production cannot reach this: the real usage poll runs on an interval
    /// floored in whole SECONDS, so `started_at` and the ledger row's `ts` can
    /// never be the same instant in the field. An optimized TEST binary reaches
    /// it every time, which is how it was found — and before the seam there was
    /// no way to reach it deliberately, so the one thing nobody could state was
    /// what the calibrator does when it arrives. Not "should do": does.
    ///
    /// `clock::testing::freeze_at` makes the collision a CHOICE. The execution's
    /// two clock reads are pinned to one instant while the two polls that should
    /// bracket it sit a second either side, which is the exact shape the
    /// optimized build produced by accident.
    ///
    /// **The answer: it is refused, it writes nothing, and it is refused as
    /// `Unbracketed`.** Refused because `evaluate_window` searches for the first
    /// poll at or after `window.end` and bounds that search by the next window's
    /// start — and a window whose `start == end` satisfies `w.start >= window.end`
    /// FOR ITSELF, so the search collapses to "a poll at exactly this instant",
    /// which no poll is. Writing nothing is the half that matters: a degenerate
    /// window cannot poison `tokens_per_percent`, so the daemon loses one sample
    /// and keeps its estimate. **It is also SILENT** — `Unbracketed` is the same
    /// verdict a genuinely unbracketed pair gets, so nothing downstream can tell
    /// "the polls do not bracket this execution" from "this execution has no
    /// width". That is reported, not changed: whether a zero-width window
    /// deserves a verdict of its own is a question about the calibration logic,
    /// and this test exists to make the current answer visible rather than to
    /// decide it.
    #[test]
    fn a_zero_width_execution_window_is_refused_and_writes_no_estimate() {
        let b = svc();
        // One timeline, written explicitly: poll, execution, poll. Only the
        // EXECUTION is degenerate.
        let t_exec = crate::clock::now();
        let t_before = t_exec - chrono::Duration::seconds(1);
        let t_after = t_exec + chrono::Duration::seconds(1);
        let reset = (t_exec + chrono::Duration::seconds(Axis::Session.window_secs() / 2))
            .to_rfc3339();

        let poll = |pct: f64, at: DateTime<Utc>| crate::store::ObservationRow {
            id: 0,
            account: "a".to_string(),
            axis: Axis::Session.as_str().to_string(),
            pct,
            resets_at: Some(reset.clone()),
            source: "usage_poll".to_string(),
            observed_at: at.to_rfc3339(),
        };
        b.store.record_observation(&poll(9.0, t_before)).unwrap();

        // **THE COLLAPSE, ON PURPOSE.** `started_at` and the row's own `ts` are
        // two reads of the seam; frozen, they are one instant, so the execution
        // has no width.
        {
            let _clock = crate::clock::testing::freeze_at(t_exec);
            b.store
                .record_usage(&crate::store::UsageRow {
                    thread_id: "t1".into(),
                    account: Some("a".into()),
                    model: "claude-sonnet-5".into(),
                    input_tokens: 1_000_000,
                    started_at: Some(crate::clock::now().to_rfc3339()),
                    ..Default::default()
                })
                .unwrap();
        }
        b.store.record_observation(&poll(19.0, t_after)).unwrap();

        // The fixture is the thing being claimed, so prove it: the window this
        // execution forms really is zero-width, read back off the store.
        let intervals = b
            .store
            .execution_intervals("a", &t_before.to_rfc3339(), &t_after.to_rfc3339())
            .unwrap();
        assert_eq!(intervals.len(), 1, "one execution: {intervals:?}");
        assert_eq!(
            intervals[0].0, intervals[0].1,
            "the frozen clock did not collapse the execution, so this test is \
             not about a zero-width window at all: {intervals:?}"
        );

        let verdict = b
            .calibrate_overlap_window("a", Axis::Session, t_after + chrono::Duration::seconds(1))
            .unwrap();
        match verdict {
            WindowVerdict::Rejected { reason, window, .. } => {
                assert_eq!(
                    reason,
                    gauge::WindowRejection::Unbracketed,
                    "a degenerate window is refused as Unbracketed: {verdict:?}"
                );
                assert_eq!(
                    window.start, window.end,
                    "the window it refused is the degenerate one: {window:?}"
                );
            }
            other => panic!("a zero-width window must be REFUSED, not {other:?}"),
        }
        assert!(
            b.store.get_estimate("a", "session").unwrap().is_none(),
            "a degenerate window must never reach `tokens_per_percent`"
        );

        // **THE ANTI-TEST.** The same three facts with the execution given one
        // microsecond of width DO calibrate — so the refusal above is
        // attributable to the zero width and to nothing else in this fixture.
        let b2 = svc();
        b2.store.record_observation(&poll(9.0, t_before)).unwrap();
        {
            let _clock = crate::clock::testing::freeze_at(t_exec);
            b2.store
                .record_usage(&crate::store::UsageRow {
                    thread_id: "t1".into(),
                    account: Some("a".into()),
                    model: "claude-sonnet-5".into(),
                    input_tokens: 1_000_000,
                    started_at: Some(
                        (t_exec - chrono::Duration::microseconds(1)).to_rfc3339(),
                    ),
                    ..Default::default()
                })
                .unwrap();
        }
        b2.store.record_observation(&poll(19.0, t_after)).unwrap();
        let ok = b2
            .calibrate_overlap_window("a", Axis::Session, t_after + chrono::Duration::seconds(1))
            .unwrap();
        assert!(
            matches!(ok, WindowVerdict::Sampled { .. }),
            "one microsecond of width is the whole difference: {ok:?}"
        );
    }

    #[test]
    fn config_is_mutable_and_keys_outlive_their_threads() {
        let b = svc();
        let mut c = cfg("loop-project", &["a"]);
        c.session_pct = Some(20.0);
        b.set_config(&c).unwrap();
        // DEC-11: a "+10% increment" is just a config edit on the key.
        c.session_pct = Some(30.0);
        c.on_trip = TripAction::Kill;
        b.set_config(&c).unwrap();
        let got = b.get_config("loop-project").unwrap().unwrap();
        assert_eq!(got.session_pct, Some(30.0));
        assert_eq!(got.on_trip, TripAction::Kill);
        assert_eq!(got.accounts, vec!["a".to_string()]);
        // Q-34: a key is retired by DISABLING it, and everything about it —
        // config included — is still there afterwards. There is no delete.
        b.charge("loop-project", "t1", "a", "claude-sonnet-5", 100, 0.0)
            .unwrap();
        let off = b.set_disabled("loop-project", true).unwrap();
        assert!(off.disabled);
        assert_eq!(off.session_pct, Some(30.0));
        assert!(b.get_config("loop-project").unwrap().is_some());
        assert_eq!(b.store.charge_accounts("loop-project").unwrap(), vec!["a"]);
        // …and disabling something that was never minted is an error, not a
        // cheerful no-op that enforces nothing.
        assert!(b.set_disabled("never-existed", true).is_err());
    }

    // ── HARDENING: the edge cases around the admission path (item 2) ─────

    #[test]
    fn hardening_admission_errors_are_real_errors_not_silent_admits() {
        // ROUND-7 item 1, crate half: when the ledger is unreadable, `admit`
        // returns Err. The runtime's fail-CLOSED policy depends on this never
        // being softened into an `Admit`.
        let b = svc();
        let mut c = cfg("k", &["a"]);
        c.session_pct = Some(10.0);
        b.set_config(&c).unwrap();
        b.store.execute_sql("DROP TABLE budget_charges").unwrap();
        let th = Thresholds::ungated();
        let accounts = pool(&["a"]);
        assert!(b.admit(&req(Some("k"), &th, &accounts, "a", &[])).is_err());
        assert!(b.check_trip("k", &accounts).is_err());
    }

    #[test]
    fn hardening_a_ledger_write_failure_surfaces_instead_of_vanishing() {
        let b = svc();
        let mut c = cfg("k", &["a"]);
        c.session_pct = Some(10.0);
        b.set_config(&c).unwrap();
        b.store.execute_sql("DROP TABLE budget_charges").unwrap();
        assert!(b
            .charge("k", "t1", "a", "claude-sonnet-5", 1_000, 0.1)
            .is_err());
    }

    #[test]
    fn hardening_an_empty_account_list_parks_rather_than_admitting() {
        // Nothing can be enforced against a pool of nothing, so nothing runs.
        let b = svc();
        let th = Thresholds {
            session: Some(50.0),
            ..Default::default()
        };
        let accounts: Vec<Placeable> = Vec::new();
        match b.admit(&req(None, &th, &accounts, "a", &[])).unwrap() {
            Admission::Park { cause, .. } => assert_eq!(cause, ParkCause::Scope),
            other => panic!("expected a park, got {other:?}"),
        }
    }

    #[test]
    fn hardening_a_stale_reset_instant_falls_back_to_a_rolling_window() {
        // The gauge went quiet and its reset instant is in the past: the meter
        // must not place a window in the past (which would zero the meter) —
        // it falls back to a rolling window ending now, which can only
        // UNDER-report, and the threshold still reads the last observation.
        let b = svc();
        let mut c = cfg("k", &["a"]);
        c.session_pct = Some(10.0);
        b.set_config(&c).unwrap();
        calibrate(&b, "a", Axis::Session, 1_000.0);
        b.observe(
            "a",
            Axis::Session,
            99.0,
            Some("2000-01-01T00:00:00+00:00".into()),
            "usage_poll",
        )
        .unwrap();
        b.charge("k", "t1", "a", "claude-sonnet-5", 500_000, 0.0)
            .unwrap();

        let accounts = pool(&["a"]);
        let now = crate::clock::now();
        let start = b.window_start("a", Axis::Session, now);
        assert!(start < now && (now - start).num_seconds() <= gauge::SESSION_WINDOW_SECS);
        let snap = b.snapshot("k", &accounts).unwrap().unwrap();
        assert_eq!(snap.meter(Axis::Session).status, MeterStatus::Exhausted);
        // A stale observation is still the last thing we know.
        assert_eq!(b.observed_value("a", Axis::Session).unwrap(), Some(99.0));
    }

    #[test]
    fn hardening_estimation_absent_on_one_account_of_several() {
        // One account measured, one not. The meter used to report only the
        // priced half (10%) and sit in `calibrating`, which is exactly the
        // "pretend the unpriced half is zero" it was written to avoid — the
        // status said so honestly, but the number under-reported by 9x and the
        // budget did not bite.
        //
        // Now the unmeasured account is priced by the PRIOR, and rung 2 of the
        // ladder makes that prior `a`'s own measurement: accounts share a plan,
        // so `b` starts from 100k tokens per percent rather than from the raw
        // constant. 1M/100k = 10% on `a`, 9M/100k = 90% on `b`, pooled 100%.
        let b = svc();
        let mut c = cfg("k", &["a", "b"]);
        c.session_pct = Some(50.0);
        b.set_config(&c).unwrap();
        calibrate(&b, "a", Axis::Session, 100_000.0);
        let accounts = pool(&["a", "b"]);
        b.charge("k", "t1", "a", "claude-sonnet-5", 1_000_000, 0.0)
            .unwrap();
        b.charge("k", "t1", "b", "claude-sonnet-5", 9_000_000, 0.0)
            .unwrap();
        let snap = b.snapshot("k", &accounts).unwrap().unwrap();
        let m = snap.meter(Axis::Session);
        assert_eq!(m.status, MeterStatus::Exhausted);
        assert!((m.consumed - 100.0).abs() < 1e-6, "{}", m.consumed);
        assert_eq!(m.consumed_tokens, 10_000_000);
        // `b` is priced, and priced by inheritance rather than by its own
        // samples — which is what the `calibrating` label means now.
        let bm = m.accounts.iter().find(|a| a.account == "b").unwrap();
        assert_eq!(bm.calibration_samples, 0);
        assert_eq!(bm.limit_tokens, Some(10_000_000.0));
        assert!(snap.tripped_axis().is_some());
    }

    #[test]
    fn hardening_config_mutated_between_charges_takes_effect_immediately() {
        // The config is re-read on every evaluation, so an edit lands on the
        // very next check — including mid-turn.
        let b = svc();
        let mut c = cfg("k", &["a"]);
        c.session_pct = Some(100.0);
        b.set_config(&c).unwrap();
        calibrate(&b, "a", Axis::Session, 100_000.0);
        let accounts = pool(&["a"]);
        b.charge("k", "t1", "a", "claude-sonnet-5", 1_000_000, 0.0)
            .unwrap();
        assert!(b.check_trip("k", &accounts).unwrap().is_none());

        c.session_pct = Some(5.0); // the operator tightened it mid-turn
        b.set_config(&c).unwrap();
        assert_eq!(
            b.check_trip("k", &accounts).unwrap().unwrap().axis,
            Axis::Session
        );

        // Q-34: DISABLING the key keeps both the meter and the history — the
        // trip is still visible, the charges are still there, and what changes
        // is only that no further turn is admitted (see the disable tests).
        b.set_disabled("k", true).unwrap();
        assert_eq!(
            b.check_trip("k", &accounts).unwrap().unwrap().axis,
            Axis::Session
        );
        assert!(!b.store.charge_accounts("k").unwrap().is_empty());
    }

    #[test]
    fn hardening_nonsense_limits_are_refused_at_write_time() {
        let b = svc();
        let mut c = cfg("k", &["a"]);
        c.session_pct = Some(-1.0);
        assert!(b.set_config(&c).is_err());
        c.session_pct = Some(f64::NAN);
        assert!(b.set_config(&c).is_err());
        c.session_pct = Some(10.0);
        c.cash_usd = Some(f64::INFINITY);
        assert!(b.set_config(&c).is_err());
        c.cash_usd = Some(20.0);
        b.set_config(&c).unwrap();
        // A duplicated account in the scope is a config bug, not a no-op.
        c.accounts = names(&["a", "a"]);
        assert!(b.set_config(&c).is_err());
        // An empty key is refused too.
        let mut empty = cfg("", &["a"]);
        empty.session_pct = Some(1.0);
        assert!(b.set_config(&empty).is_err());
    }

    #[test]
    fn hardening_a_zero_charge_is_a_no_op() {
        let b = svc();
        let c = cfg("k", &["a"]);
        b.set_config(&c).unwrap();
        b.charge("k", "t1", "a", "claude-sonnet-5", 0, 0.0).unwrap();
        assert!(b.store.charge_accounts("k").unwrap().is_empty());
        // …but a dollars-only charge (no tokens) still lands on the cash axis.
        b.charge("k", "t1", "a", "claude-sonnet-5", 0, 0.5).unwrap();
        let accounts = pool(&["a"]);
        let snap = b.snapshot("k", &accounts).unwrap().unwrap();
        assert!((snap.meter(Axis::Cash).consumed - 0.5).abs() < 1e-9);
    }

    #[test]
    fn hardening_an_unknown_budget_key_parks_instead_of_running_unmetered() {
        // Q-34's other half. A thread pointed at a key with NO config (a typo,
        // or a client that never called `budgets.set`) used to be admitted
        // unmetered — the same state as having no key at all. That was the hole
        // `budgets.delete` drove through the fail-closed posture, and with
        // deletion gone there is no reason left to keep it: an unmeterable key
        // parks. Nothing is fabricated and nothing is widened to a default
        // scope — there is simply no meter, so there is no turn.
        let b = svc();
        let th = Thresholds::ungated();
        let accounts = pool(&["a", "b"]);
        match b
            .admit(&req(Some("ghost"), &th, &accounts, "a", &[]))
            .unwrap()
        {
            Admission::Park {
                cause,
                reason,
                budget_key,
            } => {
                assert_eq!(cause, ParkCause::UnknownKey);
                assert_eq!(budget_key.as_deref(), Some("ghost"));
                assert!(reason.contains("ghost"), "{reason}");
                assert!(reason.contains("budgets/<key>/set"), "{reason}");
            }
            other => panic!("expected a park, got {other:?}"),
        }
        assert!(b.snapshot("ghost", &accounts).unwrap().is_none());
        assert!(b.check_trip("ghost", &accounts).unwrap().is_none());
        // Minting the key is the remedy, and it admits immediately after.
        let mut c = cfg("ghost", &["a", "b"]);
        c.session_pct = Some(50.0);
        b.set_config(&c).unwrap();
        assert_eq!(
            b.admit(&req(Some("ghost"), &th, &accounts, "a", &[]))
                .unwrap(),
            Admission::Admit { switch_to: None }
        );
        // A thread with NO key is still ungated — "no key ⇒ no budget" is
        // unchanged; this is only about a key that names nothing.
        assert_eq!(
            b.admit(&req(None, &th, &accounts, "a", &[])).unwrap(),
            Admission::Admit { switch_to: None }
        );
    }

    // ── Q-34 (msg #751): disable, never delete ────────────────────────────

    #[test]
    fn a_disabled_key_denies_every_admission_and_enabling_lets_it_through() {
        // "whenever the budget key is used, we just deny." Unconditional: this
        // key has a huge budget, no threshold and a clear gauge, so the ONLY
        // thing that can stop it is the disable switch.
        let b = svc();
        let mut c = cfg("retired", &["a"]);
        c.session_pct = Some(90.0);
        b.set_config(&c).unwrap();
        calibrate(&b, "a", Axis::Session, 1_000.0);
        let th = Thresholds::ungated();
        let accounts = pool(&["a"]);
        assert_eq!(
            b.admit(&req(Some("retired"), &th, &accounts, "a", &[]))
                .unwrap(),
            Admission::Admit { switch_to: None }
        );

        let off = b.set_disabled("retired", true).unwrap();
        assert!(off.disabled);
        let stamp = off.disabled_at.clone().expect("the instant is recorded");

        match b
            .admit(&req(Some("retired"), &th, &accounts, "a", &[]))
            .unwrap()
        {
            Admission::Park {
                cause,
                reason,
                budget_key,
            } => {
                assert_eq!(cause, ParkCause::Disabled);
                assert_eq!(budget_key.as_deref(), Some("retired"));
                // The park must NAME the disabled key and say what happened.
                assert!(reason.contains("retired"), "{reason}");
                assert!(reason.contains("disabled"), "{reason}");
                assert!(reason.contains(&stamp), "{reason}");
            }
            other => panic!("expected a disabled park, got {other:?}"),
        }
        // It is a GATE, not an error, and not a fabricated meter reading: the
        // budget itself is nowhere near tripped.
        assert!(b.check_trip("retired", &accounts).unwrap().is_none());

        // Disabling twice keeps the instant it was first disabled at…
        let again = b.set_disabled("retired", true).unwrap();
        assert_eq!(again.disabled_at, Some(stamp));
        // …and enabling is the exact inverse.
        let on = b.set_disabled("retired", false).unwrap();
        assert!(!on.disabled);
        assert_eq!(on.disabled_at, None);
        assert_eq!(
            b.admit(&req(Some("retired"), &th, &accounts, "a", &[]))
                .unwrap(),
            Admission::Admit { switch_to: None }
        );
    }

    #[test]
    fn a_disabled_key_denies_before_scope_thresholds_or_meters_are_consulted() {
        // Every OTHER gate is either satisfiable or absent here, and the key is
        // scoped to an account that is not even registered — which would
        // normally park with cause `scope`. The disable switch outranks it, so
        // the operator is told the actual reason rather than a downstream one.
        let b = svc();
        let mut c = cfg("retired", &["not-registered"]);
        c.session_pct = Some(1.0);
        b.set_config(&c).unwrap();
        b.set_disabled("retired", true).unwrap();
        let th = Thresholds {
            session: Some(1.0),
            ..Default::default()
        };
        b.observe("a", Axis::Session, 99.0, None, "usage_poll").unwrap();
        let accounts = pool(&["a"]);
        match b
            .admit(&req(Some("retired"), &th, &accounts, "a", &[]))
            .unwrap()
        {
            Admission::Park { cause, .. } => assert_eq!(cause, ParkCause::Disabled),
            other => panic!("expected a disabled park, got {other:?}"),
        }
    }

    #[test]
    fn disabling_keeps_every_number_about_the_key_queryable() {
        // The whole point of the operator's answer: "so we can still query and
        // see information about the budget, like how much of the budget did it
        // spend". Spend on every axis, the per-account breakdown, the window,
        // calibration state and the config all survive being disabled.
        let b = svc();
        let mut c = cfg("k", &["a", "bb"]);
        c.session_pct = Some(50.0);
        c.cash_usd = Some(10.0);
        b.set_config(&c).unwrap();
        calibrate(&b, "a", Axis::Session, 100_000.0);
        calibrate(&b, "bb", Axis::Session, 100_000.0);
        b.charge("k", "t1", "a", "claude-sonnet-5", 1_000_000, 2.50)
            .unwrap();
        b.charge("k", "t2", "bb", "claude-sonnet-5", 2_000_000, 1.25)
            .unwrap();
        let accounts = pool(&["a", "bb"]);
        let before = b.snapshot("k", &accounts).unwrap().unwrap();

        b.set_disabled("k", true).unwrap();
        let after = b.snapshot("k", &accounts).unwrap().unwrap();

        // Byte-for-byte the same accounting, plus the disabled marker.
        assert_eq!(after.metered_accounts, before.metered_accounts);
        for axis in Axis::ALL {
            let (x, y) = (before.meter(axis), after.meter(axis));
            assert_eq!(y.consumed_tokens, x.consumed_tokens, "{axis:?}");
            assert_eq!(y.status, x.status, "{axis:?}");
            assert_eq!(y.limit, x.limit, "{axis:?}");
            assert!((y.consumed - x.consumed).abs() < 1e-9, "{axis:?}");
            assert!((y.consumed_usd - x.consumed_usd).abs() < 1e-9, "{axis:?}");
            assert_eq!(y.accounts.len(), x.accounts.len(), "{axis:?}");
        }
        let session = after.meter(Axis::Session);
        assert_eq!(session.consumed_tokens, 3_000_000);
        assert_eq!(session.accounts.len(), 2);
        assert_eq!(
            session.accounts[0].calibration_samples,
            gauge::PRIOR_SAMPLES as u64
        );
        assert!(session.accounts[0].window_start <= crate::store::now_rfc3339());
        assert!((after.meter(Axis::Cash).consumed - 3.75).abs() < 1e-9);
        assert!(after.config.disabled);
        assert_eq!(after.config.session_pct, Some(50.0));
        assert_eq!(after.config.accounts, vec!["a".to_string(), "bb".into()]);
        // …and it is visible in the wire payload a client actually reads.
        let j = after.to_json();
        assert_eq!(j["disabled"], true);
        assert_eq!(j["config"]["disabled"], true);
        assert!(j["config"]["disabled_at"].is_string());
        assert_eq!(j["session"]["consumed_tokens"], 3_000_000);
        assert_eq!(j["cash"]["consumed"], 3.75);
        // The key is listed like any other, disabled or not.
        assert_eq!(b.list_configs().unwrap().len(), 1);
    }

    #[test]
    fn a_disabled_key_is_still_charged_so_no_spend_goes_unaccounted() {
        // Disabling stops turns from STARTING; it cannot un-spend a turn that
        // was already in flight. That tail still has to land on the ledger, or
        // the history the operator wants to query would be missing spend.
        let b = svc();
        let mut c = cfg("k", &["a"]);
        c.cash_usd = Some(10.0);
        b.set_config(&c).unwrap();
        b.set_disabled("k", true).unwrap();
        b.charge("k", "t1", "a", "claude-sonnet-5", 500, 0.75)
            .unwrap();
        let accounts = pool(&["a"]);
        let snap = b.snapshot("k", &accounts).unwrap().unwrap();
        assert!((snap.meter(Axis::Cash).consumed - 0.75).abs() < 1e-9);
        assert_eq!(b.store.charge_accounts("k").unwrap(), vec!["a"]);
        // Nothing NEW can start on it, so that tail is all it can ever gain.
        let th = Thresholds::ungated();
        assert!(matches!(
            b.admit(&req(Some("k"), &th, &accounts, "a", &[])).unwrap(),
            Admission::Park {
                cause: ParkCause::Disabled,
                ..
            }
        ));
    }

    #[test]
    fn a_key_whose_config_no_longer_validates_can_still_be_disabled() {
        // The one thing you must always be able to do to a broken key is turn
        // it off. A pre-round-7 key has an EMPTY account scope, which
        // `set_config` refuses outright — so disabling must not go through
        // validation, or such a key could never be retired at all.
        let b = svc();
        b.store
            .upsert_budget(&BudgetRow {
                key: "legacy".into(),
                parent_key: None,
                session_pct: Some(20.0),
                weekly_pct: None,
                fable_pct: None,
                cash_usd: None,
                accounts: Vec::new(),
                on_trip: "kill".into(),
                disabled_at: None,
                created_at: crate::store::now_rfc3339(),
                updated_at: crate::store::now_rfc3339(),
            })
            .unwrap();
        let broken = b.get_config("legacy").unwrap().unwrap();
        assert!(Budgets::validate_config(&broken).is_err());
        assert!(b.set_disabled("legacy", true).unwrap().disabled);
        assert!(b.get_config("legacy").unwrap().unwrap().disabled);
    }

    #[test]
    fn set_config_never_flips_the_disable_switch_by_itself() {
        // Editing a limit on a disabled key must not quietly re-enable it (the
        // client that edits a number is usually not the one that retired the
        // key), and the `disabled` flag on a write is honored when it IS sent.
        let b = svc();
        let mut c = cfg("k", &["a"]);
        c.session_pct = Some(10.0);
        b.set_config(&c).unwrap();
        b.set_disabled("k", true).unwrap();
        let stamp = b.get_config("k").unwrap().unwrap().disabled_at.unwrap();

        // A client round-tripping the config it read back keeps `disabled`.
        let mut edited = b.get_config("k").unwrap().unwrap();
        edited.session_pct = Some(11.0);
        b.set_config(&edited).unwrap();
        let got = b.get_config("k").unwrap().unwrap();
        assert!(got.disabled);
        assert_eq!(got.session_pct, Some(11.0));
        // The instant it was disabled at does not move on an unrelated edit.
        assert_eq!(got.disabled_at, Some(stamp));

        // Explicitly clearing the flag is how a write re-enables.
        edited.disabled = false;
        b.set_config(&edited).unwrap();
        let got = b.get_config("k").unwrap().unwrap();
        assert!(!got.disabled);
        assert_eq!(got.disabled_at, None);
    }

    // ── wave three: THE DISABLE — a true 0% share is an EXCLUSION ───────────
    //
    // 2026-09-08. The operator diagnosed this himself and the diagnosis is the
    // spec: *"I tried changing the share to like 0.001%, but I think that just
    // means that there's still a non-zero pressure on it… the simplest is just
    // to be able to set a true 0% share on an account, just to disable usage on
    // it, and then we should just not consider it for placement. It should be
    // out of the placement selection. That's the cleanest."*
    //
    // Everything below was mutation-tested: each rule was BROKEN in the source
    // and the named test watched to go red before it was allowed to be green.

    /// **A TINY SHARE IS NOT A SMALL PRESSURE — his diagnosis, pinned as
    /// arithmetic.**
    ///
    /// This test proves the PREMISE of the whole feature, and it has to exist
    /// because the obvious implementation ("just let him set a very small
    /// number") is the thing that does not work. It is not a near-miss that
    /// could be tuned: the share is the DENOMINATOR, so shrinking it does not
    /// shrink pressure. On an untouched account it cancels out entirely and the
    /// pressure does not move AT ALL; on a partly-used one it grows without
    /// bound.
    ///
    /// If this test ever goes green in the direction of "a small share gives a
    /// small pressure", the exclusion below is unnecessary and this file is
    /// wrong about why it exists.
    #[test]
    fn a_point_oh_oh_one_percent_share_does_not_reduce_pressure_it_explodes_it() {
        let b = svc();
        let now = crate::clock::now();
        let window = Axis::WeeklyAll.window_secs() as f64;
        let resets_in =
            |frac: f64| Some((now + ChronoDuration::seconds((window * frac) as i64)).to_rfc3339());
        flatten_deaths(&b, &["untouched", "used"]);
        // Half a window left, in both cases.
        b.observe("untouched", Axis::WeeklyAll, 0.0, resets_in(0.5), "usage_poll")
            .unwrap();
        b.observe("used", Axis::WeeklyAll, 5.0, resets_in(0.5), "usage_poll")
            .unwrap();
        // `used` is about to be driven far past its share, and since 2026-09-22
        // an over-share account with NOTHING RUNNING ON IT reports no pressure
        // at all (`UnrankedReason::OverShareSettled`). This test is about the
        // ARITHMETIC — that there is no small share that means "off" — so it
        // keeps a thread executing there, which is the state in which that
        // arithmetic is reported. Without this line the assertions below would
        // be unwrapping a `None`, and the test would be measuring the new
        // visibility rule instead of the explosion it was written for.
        set_running_on(&b, "t-used", "used");

        let p = |a: &str| b.pool_pressure(a, now).value().unwrap();

        // THE WHOLE ACCOUNT: on pace at 2.0 (all of the share unused, half the
        // window gone).
        set_share(&b, "untouched", 1.0);
        let whole = p("untouched");

        // THE 0.001% HE ACTUALLY TRIED. Identical. The share cancels.
        set_share(&b, "untouched", 0.00001);
        assert!(
            (p("untouched") - whole).abs() < 1e-9,
            "shrinking the share of an UNTOUCHED account changes its pressure not \
             one bit — it is the denominator of a fraction whose numerator it also \
             scales. Got {} against {whole}",
            p("untouched")
        );

        // AND ON AN ACCOUNT WITH ANY USAGE AT ALL IT GOES THE WRONG WAY. 5%
        // consumed against a 0.001% share is ~5000x overspent, which over half a
        // window reads as a pressure near −10,000 — not "leave it alone", but
        // "this account is catastrophically off its lane". The magnitude grows
        // without bound as the share shrinks, which is the point: there is no
        // small number that converges on "off".
        set_share(&b, "used", 0.00001);
        let at_1e5 = p("used");
        assert!(
            at_1e5 < -5_000.0,
            "a 0.001% share on a used account is not 'almost off'; got {at_1e5}"
        );
        set_share(&b, "used", 0.0000001);
        assert!(
            p("used") < at_1e5 * 10.0,
            "and shrinking it further makes it MORE extreme, never less: {} vs {at_1e5}",
            p("used")
        );

        // The point, stated as the assertion the operator's own attempt fails:
        // there is NO share he could have typed that means 'do not use this'.
        for tiny in [0.001, 0.0001, 0.00001, 1e-12] {
            set_share(&b, "untouched", tiny);
            assert!(
                (p("untouched") - whole).abs() < 1e-6,
                "share {tiny} still ranks exactly like the whole account"
            );
        }
    }

    /// **THE EXCLUSION, PROVEN AT N ≥ 2 — the disabled account would have WON in
    /// one pool and would have LOST in the other, and the selection is the same
    /// in both.**
    ///
    /// The N ≥ 2 requirement is the whole design of this test. A pool where the
    /// disabled account was never going to be chosen proves nothing: it passes
    /// with the exclusion, without the exclusion, and with the exclusion spelled
    /// backwards.
    ///
    /// **AND THE WINNING CASE HAD TO BE RE-BUILT, which is worth recording.** It
    /// was first written so the disabled account won on POOL PRESSURE — the
    /// operator's own mechanism — and that version SURVIVED the mutation that
    /// deletes the exclusion from `place`. The reason is an accident: pressure
    /// for a disabled account is `Slack::Unknown` (there is no share to divide),
    /// and an unknown criterion is dropped whenever any rival has a real score.
    /// So the disabled account lost anyway, for a reason that has nothing to do
    /// with being excluded, and the test was reporting the right answer while
    /// proving nothing about it.
    ///
    /// It now wins on EXPECTED DEATHS instead — the objective at the very top of
    /// the cascade, which `narrow_min` resolves before the cap or pressure is
    /// consulted at all. A disabled account that wins there cannot be rescued from
    /// the field by any accident further down, so its absence from the answer
    /// can only be the exclusion.
    ///
    /// **THE MUTATION.** Delete the `is_disabled` skip in `Budgets::place` and the
    /// FIRST case goes red (it selects `aa-disabled`). The second case stays
    /// green, which is exactly why it cannot be the only case.
    #[test]
    fn a_disabled_account_is_out_of_selection_whether_it_would_have_won_or_lost() {
        // Small denominators, so `expected_deaths` actually discriminates: with
        // the production-scale 20M/percent that `flatten_deaths` installs, every
        // account reaches `exp(-200) ≈ 0` and the objective ties for everyone.
        let calibrate_small = |b: &Budgets, account: &str| {
            b.store
                .upsert_estimate(&crate::store::EstimateRow {
                    account: account.into(),
                    axis: Axis::WeeklyAll.as_str().into(),
                    tokens_per_percent: 1_000.0,
                    sample_count: 8,
                    last_obs_id: 0,
                    updated_at: crate::store::now_rfc3339(),
                })
                .unwrap();
        };
        let th = Thresholds::default();
        let field = pool(&["aa-disabled", "zz-rival"]);

        // ── CASE ONE: the disabled account WINS THE OBJECTIVE OUTRIGHT.
        // Untouched (100% of the axis reachable) against a rival at 99%, which
        // at 1,000 tokens per percent is 100,000 reachable tokens against 1,000
        // — a whole nominal turn against a hundredth of one.
        let b = svc();
        for a in ["aa-disabled", "zz-rival"] {
            calibrate_small(&b, a);
        }
        b.observe("aa-disabled", Axis::WeeklyAll, 0.0, None, "usage_poll")
            .unwrap();
        b.observe("zz-rival", Axis::WeeklyAll, 99.0, None, "usage_poll")
            .unwrap();

        // FIRST, prove the case is discriminating, and prove it on the criterion
        // that decides — not on the outcome, which is what let the earlier
        // version of this test fool itself.
        let d = |a: &str| Demand::default().expected_deaths(b.reachable_tokens(&placeable(a), &th));
        assert!(
            d("zz-rival") - d("aa-disabled") > DEATHS_DEADBAND,
            "fixture: `aa-disabled` must win the DEATHS objective outright, beyond \
             the deadband — deaths {} vs {}",
            d("aa-disabled"),
            d("zz-rival")
        );
        assert_eq!(
            place(&b, &th, &field),
            Some(placeable("aa-disabled")),
            "fixture: re-enabled, it wins the top criterion, so nothing below can \
             take it out of the front"
        );

        // NOW DISABLE IT.
        set_disabled(&b, "aa-disabled", true);
        assert_eq!(
            place(&b, &th, &field),
            Some(placeable("zz-rival")),
            "a disabled account is ABSENT from the candidate set — not ranked last, \
             not present with a clamped pressure. It won the objective and it is \
             simply not there"
        );
        // And the objective it won is UNCHANGED by the disable: the exclusion is
        // not a demotion dressed up, and nothing about the account's own
        // measurements moved.
        assert!(
            d("zz-rival") - d("aa-disabled") > DEATHS_DEADBAND,
            "disabling must not alter what the account measures — only whether it \
             is looked at"
        );

        // ── CASE TWO: the disabled account LOSES anyway. Same exclusion, same
        // answer — which is what "out of the selection" means as opposed to
        // "penalised in it".
        let c = svc();
        for a in ["aa-disabled", "zz-rival"] {
            calibrate_small(&c, a);
        }
        c.observe("aa-disabled", Axis::WeeklyAll, 99.0, None, "usage_poll")
            .unwrap();
        c.observe("zz-rival", Axis::WeeklyAll, 0.0, None, "usage_poll")
            .unwrap();
        let d2 =
            |a: &str| Demand::default().expected_deaths(c.reachable_tokens(&placeable(a), &th));
        assert!(
            d2("aa-disabled") - d2("zz-rival") > DEATHS_DEADBAND,
            "fixture: in this pool the account to be disabled is the clear LOSER"
        );
        assert_eq!(place(&c, &th, &field), Some(placeable("zz-rival")));
        set_disabled(&c, "aa-disabled", true);
        assert_eq!(
            place(&c, &th, &field),
            Some(placeable("zz-rival")),
            "and disabling a loser changes nothing — the same selection in both \
             pools is the property being claimed"
        );
    }

    /// **THE SHORT CIRCUIT — "if all the other accounts are blocked" must not be
    /// how work reaches the account he switched off.**
    ///
    /// `place` returns the sole candidate WITHOUT consulting anything when the
    /// field is length one. That short circuit is exactly the shape of the case
    /// this feature exists for, so the exclusion is applied ABOVE it.
    ///
    /// **THE MUTATION.** Move the `is_disabled` skip below the `len() == 1` match
    /// and this goes red while every two-account test above stays green — the
    /// precise blind spot the share clamp was already written to avoid.
    #[test]
    fn a_disabled_account_is_not_selected_even_as_the_only_candidate() {
        let b = svc();
        let now = crate::clock::now();
        let window = Axis::WeeklyAll.window_secs() as f64;
        flatten_deaths(&b, &["only-one"]);
        b.observe(
            "only-one",
            Axis::WeeklyAll,
            0.0,
            Some((now + ChronoDuration::seconds((window * 0.5) as i64)).to_rfc3339()),
            "usage_poll",
        )
        .unwrap();
        let field = pool(&["only-one"]);
        assert_eq!(place(&b, &Thresholds::default(), &field), Some(placeable("only-one")));

        set_disabled(&b, "only-one", true);
        assert_eq!(
            place(&b, &Thresholds::default(), &field),
            None,
            "an all-disabled field has nowhere to place, and says so — it does not \
             fall through to the only name it can see"
        );
    }

    /// **THE ARITHMETIC NEVER SEES THE ZERO, and this test can tell the
    /// difference between that and getting away with it.**
    ///
    /// The discriminating assertion is `unranked_reason`, not `pressure`. Both
    /// the guarded and the unguarded implementation report `pressure: null`:
    /// `(0.0 − 0.5) / 0.0` is −∞, `−∞ / frac_left` is −∞, and `Slack::value`
    /// rejects non-finite readings. So a test that checked only the OUTCOME
    /// would pass with a divide-by-zero sitting one branch away — which is the
    /// exact failure mode this task was told to prove against.
    ///
    /// The two implementations disagree on exactly one observable: the guarded
    /// one says `disabled`, and the unguarded one says `not_finite`.
    ///
    /// **THE MUTATION.** Replace the `share.lent()` guard in `pressure_reading`
    /// with `share.for_display()` and this goes red on the reason while the
    /// `pressure` assertion below it stays green.
    #[test]
    fn the_pressure_arithmetic_is_never_reached_for_a_disabled_account() {
        let b = svc();
        let now = crate::clock::now();
        let window = Axis::WeeklyAll.window_secs() as f64;
        b.observe(
            "disabled",
            Axis::WeeklyAll,
            50.0,
            Some((now + ChronoDuration::seconds((window * 0.5) as i64)).to_rfc3339()),
            "usage_poll",
        )
        .unwrap();
        // A REAL SHARE ALONGSIDE THE DISABLE, which is the 2026-09-20 shape and
        // is also the harder case: with a 0.4 share the arithmetic below would
        // succeed and produce a plausible number, so the guard has to be a
        // STRUCTURAL exclusion and cannot be an accident of dividing by zero.
        set_share(&b, "disabled", 0.4);
        set_disabled(&b, "disabled", true);

        let r = b.pressure_reading("disabled", now);
        assert!(r.disabled);
        assert_eq!(r.pressure, None, "a disabled account is UNRANKED");
        assert_eq!(
            r.unranked_reason,
            Some(UnrankedReason::Disabled),
            "THE DISCRIMINATING ASSERTION. `not_finite` here would mean the \
             division ran and produced an infinity that the finiteness check \
             happened to swallow — the right answer for the wrong reason, and a \
             divide-by-zero one refactor away from reaching the wire"
        );

        // And it is NOT the absence of evidence, on either surface: the reading
        // it does have is still there, and `share` reports the zero he typed.
        assert_eq!(r.observed_pct, Some(50.0));
        assert!(r.observed_at.is_some());
        assert!(r.resets_at.is_some());
        assert_eq!(
            r.share, 0.4,
            "`share` is the quantity he lent and is INDEPENDENT of `disabled` \
             beside it — it read 0.0 here only while the zero WAS the disable"
        );

        // DISABLED DOMINATES the five measurement reasons. An account with no
        // reading at all reports `disabled`, not `no_reading` — the reason a
        // human can act on, not the one that sends him to poll a gauge on an
        // account he deliberately switched off.
        set_disabled(&b, "never-polled", true);
        let n = b.pressure_reading("never-polled", now);
        assert_eq!(n.unranked_reason, Some(UnrankedReason::Disabled));
        assert!(n.disabled);

        // The ranker's own two-state collapse agrees, through the same path.
        assert_eq!(b.pool_pressure("disabled", now).value(), None);
    }

    /// **DISABLED IS NOT OVER-SHARE, and the accident that made it look like one
    /// is named here so nobody re-introduces it as a simplification.**
    ///
    /// `over_share` is `consumed >= share`. While the disable WAS a zero share
    /// that was true of every reading there had ever been, so an unguarded
    /// implementation excluded a disabled account correctly — by calling it a
    /// clamp. Since 2026-09-20 a disabled account carries a REAL share, so the
    /// accident is gone and the hazard is the opposite one: the arithmetic now
    /// gives a real answer, and on the fixture below that answer is `true`. The
    /// `lent()` guard is the only thing that stops the operator being told to
    /// wait for a weekly reset, or to raise a share, about an account he simply
    /// took out of the pool.
    ///
    /// **THE MUTATION.** Delete the `lent()` guard in `over_share` and this goes
    /// red, while `a_disabled_account_is_out_of_selection…` above stays green.
    #[test]
    fn a_disabled_account_is_never_reported_as_having_spent_past_its_share() {
        let b = svc();
        let now = crate::clock::now();
        b.observe("disabled", Axis::WeeklyAll, 99.0, None, "usage_poll")
            .unwrap();
        // 0.5 with 99% consumed is genuinely over share, so the ONLY thing that
        // can make this assertion pass is the `lent()` guard: the arithmetic
        // says `true` and the exclusion has to answer before it.
        set_share(&b, "disabled", 0.5);
        set_disabled(&b, "disabled", true);
        assert!(
            !b.over_share("disabled").unwrap(),
            "a disabled account has no share to have spent past; `consumed >= 0.0` \
             is true of everything and is not a finding"
        );
        // The landing cap declines to grade it too, rather than dividing by the
        // ceiling the share bounds.
        assert_eq!(
            b.landing_capacity("disabled", &Thresholds::default()).value(),
            None
        );
        let _ = now;
    }

    /// **THE PARK NAMES THE DISABLE, and it is a different cause from the clamp.**
    ///
    /// Two states that both empty the target set and mean opposite things to the
    /// person reading the reason. A clamp clears on a weekly reset with no
    /// action from him; a disable clears only when he reverses it, and telling him
    /// to wait would be telling him to wait forever.
    ///
    /// **THE MUTATION.** Return `ParkCause::Share` for the disabled case and this
    /// goes red on the cause; return the clamp's reason string and it goes red
    /// on the text.
    #[test]
    fn parking_on_an_all_disabled_field_says_disabled_and_not_share() {
        let b = svc();
        let th = Thresholds::default();
        let accounts = pool(&["aa-disabled", "zz-disabled"]);
        flatten_deaths(&b, &["aa-disabled", "zz-disabled"]);
        for a in ["aa-disabled", "zz-disabled"] {
            b.observe(a, Axis::WeeklyAll, 10.0, None, "usage_poll").unwrap();
            set_disabled(&b, a, true);
        }
        match b
            .admit(&req(None, &th, &accounts, "aa-disabled", &[]))
            .unwrap()
        {
            Admission::Park { cause, reason, .. } => {
                assert_eq!(cause, ParkCause::AccountDisabled);
                assert_ne!(cause, ParkCause::Share, "a disabling is not a clamp");
                assert_ne!(
                    cause,
                    ParkCause::Unavailable,
                    "and it is not a bar either — nothing observed a failure here"
                );
                assert!(reason.contains("DISABLED"), "{reason}");
                assert!(
                    !reason.contains("wait for the weekly window to reset"),
                    "the clamp's remedy is the WRONG advice for a disabling and must \
                     not appear: {reason}"
                );
                assert!(
                    reason.contains("accounts/<account>/enable"),
                    "the reason has to name the one action that clears it: {reason}"
                );
                assert!(
                    !reason.contains("share to 0%"),
                    "and it must NOT send him to the share field — that is the door \
                     the 2026-09-20 separation closed: {reason}"
                );
            }
            other => panic!("expected a disabled park, got {other:?}"),
        }
    }

    /// **THE CONTINUITY PARK NAMES BOTH WITHDRAWALS, AND NAMES THEM APART.**
    ///
    /// A thread that may not stay where it is, holding no conversation on any
    /// account it could be moved to, parks for continuity — and the two reasons
    /// its target set came back empty have OPPOSITE remedies. A clamp clears on
    /// the weekly reset with no action from him; a disable clears only when he
    /// reverses it. A reason that merged them, or dropped either, would send him
    /// to wait for a reset that will never change anything, or to re-enable an
    /// account that was never disabled.
    ///
    /// **This is the test that was missing**, and its absence is why the note
    /// survived as two verbatim copies: no test in this crate asserted the text
    /// on EITHER continuity branch, so a copy could drift from the other without
    /// anything going red. One derivation now, witnessed here.
    ///
    /// **THE MUTATION.** Drop either arm of [`withdrawn_note`] and this goes red
    /// on the half that vanished.
    #[test]
    fn the_continuity_park_names_the_clamp_and_the_disable_apart() {
        let b = svc();
        let th = Thresholds::default();
        let accounts = pool(&["cur", "clamped-one", "disabled-one"]);
        flatten_deaths(&b, &["cur", "clamped-one", "disabled-one"]);

        // `cur` is over the default weekly ceiling, so the thread may not stay.
        b.observe("cur", Axis::WeeklyAll, 99.0, None, "usage_poll").unwrap();
        // Lent half the account, spent 60% of it: past its share, so it stays in
        // `clear` (work there finishes) and is not a placement TARGET.
        set_share(&b, "clamped-one", 0.5);
        b.observe("clamped-one", Axis::WeeklyAll, 60.0, None, "usage_poll")
            .unwrap();
        // Taken out of the pool by hand. Also in `clear`, also not a target, and
        // for a reason no reset will ever lift.
        //
        // It is ALSO past its lent share (5% lent, 10% of the account spent),
        // and that is deliberate: it is what makes the third assertion below
        // non-vacuous. With a share this account was under, `is_disabled` and
        // `over_share` could be evaluated in either order and the reason would
        // read the same, so the assertion that the ORDER holds would be
        // unfalsifiable. Over on both counts, the order decides which list it
        // lands in, and swapping the two tests in `admit` reddens this test.
        set_share(&b, "disabled-one", 0.05);
        b.observe("disabled-one", Axis::WeeklyAll, 10.0, None, "usage_poll")
            .unwrap();
        set_disabled(&b, "disabled-one", true);

        let history = vec!["cur".to_string()];
        match b
            .admit(&req(None, &th, &accounts, "cur", &history))
            .unwrap()
        {
            Admission::Park { cause, reason, .. } => {
                assert_eq!(cause, ParkCause::Continuity, "{reason}");
                assert!(
                    reason.contains("; past its lent share: clamped-one"),
                    "the clamp must be named, and must name the clamped account: {reason}"
                );
                assert!(
                    reason.contains("; DISABLED by you (share 0%): disabled-one"),
                    "the disable must be named APART from the clamp: {reason}"
                );
                assert!(
                    !reason.contains("past its lent share: disabled-one"),
                    "a disabled account must never be described as over-share — it never \
                     reached the gauge read that would decide it: {reason}"
                );
            }
            other => panic!("expected a continuity park, got {other:?}"),
        }
    }

    /// **REVERSIBLE, WITH NOTHING TO REMEMBER — BECAUSE NOTHING WAS DESTROYED.**
    ///
    /// One row, in and out, and the share it was lent is not part of the
    /// transaction in either direction. This USED to be "one number, one row":
    /// the disable overwrote the share with zero and the reversal was setting a
    /// positive number back. The operator separated them, so the reversal is
    /// now a DELETE and the account returns to exactly the standing it had
    /// without anybody supplying a number.
    ///
    /// **THE MUTATION.** Make `Store::set_account_disabled(.., false, ..)` also
    /// write a share (any share) and the final assertion goes red at the share
    /// it lands on rather than at placement — which is the point: an enable
    /// that touches the share is an enable that can silently re-weight his pool.
    #[test]
    fn a_disable_is_reversed_by_an_enable_and_leaves_the_share_alone() {
        let b = svc();
        let now = crate::clock::now();
        let window = Axis::WeeklyAll.window_secs() as f64;
        let resets_in =
            |frac: f64| Some((now + ChronoDuration::seconds((window * frac) as i64)).to_rfc3339());
        flatten_deaths(&b, &["aa-disabled", "zz-rival"]);
        b.observe("aa-disabled", Axis::WeeklyAll, 0.0, resets_in(0.01), "usage_poll")
            .unwrap();
        b.observe("zz-rival", Axis::WeeklyAll, 50.0, resets_in(0.5), "usage_poll")
            .unwrap();
        let field = pool(&["aa-disabled", "zz-rival"]);
        let th = Thresholds::default();

        set_share(&b, "aa-disabled", 0.5);
        set_disabled(&b, "aa-disabled", true);
        assert!(b.is_disabled("aa-disabled"));
        assert_eq!(
            b.pool_share("aa-disabled").share(),
            0.5,
            "the share is what he lent, and taking the account out of the pool \
             is not a statement about it"
        );
        assert_eq!(place(&b, &th, &field), Some(placeable("zz-rival")));

        // Back in — and it wins again, at the share it never stopped having.
        // NOTHING supplies a number here, which is the assertion.
        set_disabled(&b, "aa-disabled", false);
        assert!(!b.is_disabled("aa-disabled"));
        assert_eq!(b.pool_share("aa-disabled"), PoolShare::pooled(0.5));
        assert_eq!(
            place(&b, &th, &field),
            Some(placeable("aa-disabled")),
            "re-enabling restores the account to exactly the standing it had"
        );
    }

    /// **A DISABLE HAS NO TIMER, and this test asserts the reason why rather than
    /// the symptom.**
    ///
    /// `is_disabled` reads the row the operator's own act wrote and consults no
    /// clock, no gauge, no health flag and no share — so there is nothing in it
    /// that could expire.
    /// A bar is the opposite by construction: `limit_until` is compared against
    /// the wall clock and lifts itself.
    #[test]
    fn a_disable_does_not_expire_however_far_the_clock_moves() {
        let b = svc();
        set_disabled(&b, "disabled", true);
        assert!(b.is_disabled("disabled"));
        for days in [1_i64, 8, 90, 3650] {
            let later = crate::clock::now() + ChronoDuration::days(days);
            assert!(
                b.is_disabled("disabled"),
                "still disabled {days} days on — nothing in this crate writes the \
                 `account_disabled` table on a timer"
            );
            assert_eq!(
                b.pressure_reading("disabled", later).unranked_reason,
                Some(UnrankedReason::Disabled)
            );
        }
    }

    /// **NO SHARE DISABLES ANYTHING ANY MORE — not a zero, not a negative, not
    /// a NaN — and a store fault does not disable either.**
    ///
    /// This test used to be called `only_an_exact_zero_disables` and it asserted
    /// the opposite of what it asserts now. That inversion IS the change: the
    /// operator ruled that *"whether to include it in the pool supersedes how
    /// much should be made available to the pool"*, so the number field cannot
    /// carry the state, and the most valuable thing to pin is that a zero
    /// arriving from a route the setter does not guard — a hand-edited row, a
    /// `pool.json` DECLARATION, a pre-migration database — does NOT revive the
    /// retired meaning. A retired convention that still works by accident is how
    /// it comes back.
    ///
    /// Exercised through the DECLARED share, which shares the whole classifier
    /// with the store row and is the only branch a test can write a refused
    /// value into without reaching around the setter.
    ///
    /// **THE MUTATION.** Put the old `if s == 0.0 { PoolShare::Disabled }` arm
    /// back in `Budgets::pool_share` and the declared-zero cases go red.
    #[test]
    fn no_share_value_disables_an_account_and_nonsense_still_means_the_whole_account() {
        use crate::pool::{Pool, PoolEntry};
        let mk = |share: Option<f64>| {
            let mut e = PoolEntry::claude_account("declared", "/tmp/declared").unwrap();
            e.share = share;
            Budgets::new(Store::open_in_memory().unwrap()).with_pool(std::sync::Arc::new(Pool {
                entries: vec![e],
            }))
        };
        for bad in [-0.5, -1.0, 2.0, f64::NAN] {
            let b = mk(Some(bad));
            assert!(!b.is_disabled("declared"), "share {bad} is nonsense, and does not disable");
            assert_eq!(b.pool_share("declared"), PoolShare::pooled(DEFAULT_POOL_SHARE));
        }
        // THE ZERO, in both spellings. A negative zero is still zero in IEEE 754
        // and both are now simply out of the legal band: the whole account, and
        // IN the pool.
        for zero in [0.0, -0.0] {
            let b = mk(Some(zero));
            assert_eq!(b.pool_share("declared"), PoolShare::pooled(DEFAULT_POOL_SHARE));
            assert!(
                !b.is_disabled("declared"),
                "a {zero} share is nonsense now, not a disable — the state moved to \
                 `account_disabled` and must not be reachable from this field"
            );
        }
        // A LIVE share is untouched by any of this.
        assert_eq!(mk(Some(0.25)).pool_share("declared"), PoolShare::pooled(0.25));
        // And an account nobody has said anything about is the whole account,
        // never DISABLED: absence is not an instruction.
        assert_eq!(
            mk(None).pool_share("declared"),
            PoolShare::pooled(DEFAULT_POOL_SHARE)
        );
        assert!(!mk(None).is_disabled("never-mentioned"));
    }

    /// **THE TWO QUESTIONS ARE INDEPENDENT IN BOTH DIRECTIONS, which is more
    /// than "a disable keeps the share".**
    ///
    /// Independence is a two-way claim and only one half is obvious. The other
    /// half — that editing the SHARE of a disabled account leaves it disabled —
    /// is the one a re-conflation would break silently: under the old model any
    /// positive share was an enable, so a board offering "set share" on a
    /// disabled card would have put the account back into placement as a side
    /// effect of a number the operator was only adjusting for later.
    ///
    /// **THE MUTATION.** Have `Store::set_pool_share` delete the
    /// `account_disabled` row (the old model's "a positive share re-enables")
    /// and the second half goes red.
    #[test]
    fn the_share_and_the_inclusion_decision_move_independently_of_each_other() {
        let b = svc();
        set_share(&b, "acct", 0.6);

        // Direction 1: taking it out does not touch the number.
        set_disabled(&b, "acct", true);
        assert_eq!(b.pool_share("acct"), PoolShare::withheld(0.6));
        assert_eq!(b.pool_share("acct").share(), 0.6);
        assert_eq!(
            b.pool_share("acct").lent(),
            None,
            "`lent` is the one door to the arithmetic and it stays shut"
        );

        // Direction 2: changing the number does not put it back.
        set_share(&b, "acct", 0.3);
        assert!(
            b.is_disabled("acct"),
            "a share edit is not an enable — that equivalence is exactly what was struck"
        );
        assert_eq!(b.pool_share("acct"), PoolShare::withheld(0.3));

        // And the enable is the only thing that puts it back, at whatever the
        // number happens to be by then.
        set_disabled(&b, "acct", false);
        assert_eq!(b.pool_share("acct"), PoolShare::pooled(0.3));
        assert_eq!(b.pool_share("acct").lent(), Some(0.3));
    }

    /// **AN ACCOUNT WITH NO SHARE ROW AT ALL CAN BE DISABLED, and disabling it
    /// does not MINT one.**
    ///
    /// The reason this is its own test is the wire. `pool_share_row.source`
    /// distinguishes *"the operator set this"* (`override`) from *"the pool
    /// declares it"* / *"nobody said anything"* (`declared` / `unset`). If the
    /// inclusion decision lived in a column on `pool_shares`, disabling an
    /// account that had never carried a share would have had to invent a row,
    /// and every surface would then have shown him an override he never typed.
    ///
    /// **THE MUTATION.** Have `set_account_disabled` upsert a `pool_shares` row
    /// alongside its own and the `source` assertion goes red.
    #[test]
    fn disabling_an_account_that_has_no_share_row_invents_no_override() {
        let b = svc();
        set_disabled(&b, "untouched", true);
        assert!(b.is_disabled("untouched"));
        assert_eq!(b.pool_share("untouched"), PoolShare::withheld(DEFAULT_POOL_SHARE));
        let row = &b.pool_share_rows(&names(&["untouched"]))["shares"][0];
        assert_eq!(row["disabled"], serde_json::json!(true));
        assert_eq!(row["share"], serde_json::json!(DEFAULT_POOL_SHARE));
        assert!(row["override"].is_null(), "no share row was minted: {row}");
        assert_eq!(
            row["source"], "unset",
            "he has said nothing about this account's SHARE, and taking it out \
             of the pool is not saying something about its share: {row}"
        );
        assert!(row["disabled_at"].is_string(), "{row}");
    }

    /// **THE WIRE KEEPS A DISABLED ACCOUNT VISIBLE, and keeps it apart from a bar.**
    ///
    /// He must be able to see that the account exists and that it is disabled,
    /// with its readings intact. Silently absent is how he loses track of an
    /// account, and this board already carries the case of him being told two
    /// accounts were dead when they were not.
    #[test]
    fn a_disabled_account_is_still_on_both_wire_payloads_with_its_readings() {
        let b = svc();
        let now = crate::clock::now();
        let window = Axis::WeeklyAll.window_secs() as f64;
        b.observe(
            "disabled",
            Axis::WeeklyAll,
            42.0,
            Some((now + ChronoDuration::seconds((window * 0.5) as i64)).to_rfc3339()),
            "usage_poll",
        )
        .unwrap();
        b.observe(
            "live",
            Axis::WeeklyAll,
            10.0,
            Some((now + ChronoDuration::seconds((window * 0.5) as i64)).to_rfc3339()),
            "usage_poll",
        )
        .unwrap();
        set_share(&b, "disabled", 0.45);
        set_disabled(&b, "disabled", true);
        set_share(&b, "live", 0.8);

        let rows = b.pressure_rows(&names(&["disabled", "live"]), now);
        let accounts = rows["accounts"].as_array().unwrap();
        assert_eq!(accounts.len(), 2, "a disabled account is NOT dropped from the list");
        let p = &accounts[0];
        assert_eq!(p["account"], "disabled");
        assert_eq!(p["disabled"], serde_json::json!(true));
        assert_eq!(p["ranked"], serde_json::json!(false));
        assert!(p["pressure"].is_null(), "null, never 0 — see `pressure_reading`");
        assert_eq!(p["unranked_reason"], "disabled");
        assert_eq!(
            p["share"],
            serde_json::json!(0.45),
            "the SHARE is what he lent, on a disabled account as much as on a live \
             one — it used to read 0.0 here because the zero WAS the disable"
        );
        assert_eq!(p["observed_pct"], serde_json::json!(42.0), "readings intact");
        assert!(p["rank"].is_null(), "unranked means no rank, not last place");
        assert_eq!(accounts[1]["disabled"], serde_json::json!(false));

        // And the share payload says the operator SAID it, rather than
        // reporting `unset` — "he has said nothing about this account" about
        // the one account he said the most deliberate thing about.
        let shares = b.pool_share_rows(&names(&["disabled"]));
        let row = &shares["shares"][0];
        assert_eq!(row["disabled"], serde_json::json!(true));
        assert_eq!(row["share"], serde_json::json!(0.45));
        assert_eq!(row["override"], serde_json::json!(0.45));
        assert_eq!(
            row["source"], "override",
            "he set 0.45; taking the account out of the pool does not unsay it"
        );
        // AND THE TWO FIELDS ARE NOT THE SAME FIELD. The strongest single
        // assertion in this file about the separation: a state and a quantity
        // that disagree, both true, both on the wire at once.
        assert_ne!(row["share"], serde_json::json!(0.0));
        assert!(row["restore_to"].is_null(), "retired with the memo: {row}");
        assert!(row["prior"].is_null(), "retired with the memo: {row}");
    }

    /// **THE MEMO IS GONE, AND SO IS EVERY TEST THAT GUARDED IT.**
    ///
    /// Two tests lived here: `re_enabling_restores_what_the_account_had_then_
    /// what_the_pool_declares` (the four rungs of `Budgets::restore_share`) and
    /// `disabling_an_already_disabled_account_keeps_the_share_it_first_displaced`
    /// (the `prior_share` CASE that a second disable would otherwise overwrite
    /// with the zero it was standing in for).
    ///
    /// Both were correct and both are deleted, because the problem they guarded
    /// no longer exists: a disable destroys nothing, so there is no displaced
    /// number to remember, no ladder to climb back up, and no second press that
    /// can eat anything. What replaced them is
    /// `the_share_and_the_inclusion_decision_move_independently_of_each_other`
    /// above, which asserts the stronger property directly.
    ///
    /// The one thing worth keeping from the pair is the IDEMPOTENCE half — a
    /// second disable must not rewrite history — which now applies to
    /// `disabled_at` instead of to a share.
    ///
    /// **THE MUTATION.** Change `set_account_disabled`'s `ON CONFLICT DO UPDATE`
    /// to also set `disabled_at=excluded.disabled_at` and this goes red.
    #[test]
    fn a_second_disable_does_not_re_date_the_first_one_or_drop_his_words() {
        let b = svc();
        b.store
            .set_account_disabled("acct", true, Some("not a paid account any more"))
            .unwrap();
        let first = b.store.account_disabled_at("acct").unwrap().unwrap();

        // A bare second press changes nothing at all — not the instant, and not
        // the note, which a naive upsert would null out.
        b.store.set_account_disabled("acct", true, None).unwrap();
        assert_eq!(b.store.account_disabled_at("acct").unwrap().unwrap(), first);
        assert_eq!(
            b.store.account_disabled_note("acct").unwrap().as_deref(),
            Some("not a paid account any more"),
            "a second disable with nothing to say must not erase what he said the \
             first time"
        );

        // But a second press that CARRIES words is him saying something new.
        b.store
            .set_account_disabled("acct", true, Some("cancelled the subscription"))
            .unwrap();
        assert_eq!(
            b.store.account_disabled_note("acct").unwrap().as_deref(),
            Some("cancelled the subscription")
        );
        assert_eq!(
            b.store.account_disabled_at("acct").unwrap().unwrap(),
            first,
            "still when he took it out, not when he last explained why"
        );

        // And the enable really deletes, so the next disable is a fresh record
        // rather than the old one wearing a new note.
        b.store.set_account_disabled("acct", false, None).unwrap();
        assert_eq!(b.store.account_disabled_at("acct").unwrap(), None);
        assert_eq!(b.store.account_disabled_note("acct").unwrap(), None);
    }

    /// **A SHARE OF ZERO IS REFUSED BY THE SETTER, AND THE REFUSAL NAMES THE
    /// OTHER DOOR.**
    ///
    /// The defect this whole change is about is a capability that was live for
    /// four days and never found, because reaching it meant knowing that a magic
    /// value in a number field was a different act. The single cheapest place to
    /// catch somebody still spelling it the old way is the moment they type the
    /// old spelling, so the error message is asserted and not just the `Err`.
    ///
    /// **THE MUTATION.** Relax the bound back to `share < 0.0` and this goes red
    /// on the very first line.
    #[test]
    fn a_zero_share_is_refused_and_the_error_points_at_the_disable_verb() {
        let b = svc();
        let err = b
            .store
            .set_pool_share("acct", 0.0, None)
            .expect_err("0 is not a share");
        let msg = format!("{err}");
        assert!(msg.contains("(0.0, 1.0]"), "{msg}");
        assert!(
            msg.contains("accounts/acct/disable"),
            "the refusal has to name the door that DOES take an account out: {msg}"
        );
        assert!(!b.is_disabled("acct"), "and it must not have half-happened");
        // The legal band is unchanged at both ends.
        assert!(b.store.set_pool_share("acct", 0.01, None).is_ok());
        assert!(b.store.set_pool_share("acct", 1.0, None).is_ok());
        for bad in [-0.1, 1.5, f64::NAN, f64::INFINITY] {
            assert!(b.store.set_pool_share("acct", bad, None).is_err(), "{bad}");
        }
    }

    /// **THE TWO `disabled` CAUSES ARE DIFFERENT FACTS AND HAVE DIFFERENT WIRE
    /// STRINGS.**
    ///
    /// [`ParkCause::Disabled`] means the thread's BUDGET KEY is disabled (Q-34,
    /// msg #751). [`ParkCause::AccountDisabled`] means every account that could
    /// have taken the thread is out of the pool because the operator took it
    /// out. Same adjective, different subject, different remedy — re-enable a
    /// key versus re-enable an account — so a reader that could not tell them
    /// apart would send him to fix the wrong thing.
    ///
    /// **THE MUTATION.** Give `AccountDisabled` the wire string `"disabled"` and
    /// this goes red. It is the collision the name exists to avoid, and it would
    /// otherwise be invisible until an interface rendered the wrong sentence.
    #[test]
    fn the_budget_key_disable_and_the_account_disable_never_collide_on_the_wire() {
        assert_eq!(ParkCause::Disabled.as_str(), "disabled");
        assert_eq!(ParkCause::AccountDisabled.as_str(), "account_disabled");
        assert_ne!(
            ParkCause::Disabled.as_str(),
            ParkCause::AccountDisabled.as_str(),
            "a disabled BUDGET KEY and a disabled ACCOUNT are different facts"
        );
        // And the account one is distinct from every other cause too — a park
        // reason is the only thing the operator is shown when nothing ran.
        let all = [
            ParkCause::Threshold,
            ParkCause::Budget,
            ParkCause::Scope,
            ParkCause::Continuity,
            ParkCause::Disabled,
            ParkCause::UnknownKey,
            ParkCause::Internal,
            ParkCause::Unavailable,
            ParkCause::Share,
            ParkCause::AccountDisabled,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for c in all {
            assert!(seen.insert(c.as_str()), "duplicate park cause string: {}", c.as_str());
        }
        assert_eq!(seen.len(), 10, "a cause was added without a distinct string");
    }

    // ── R9.PR1 — ONE PRESSURE FUNCTION, READ BY PLACEMENT AND THE SHAPE ─────
    //
    // Forcings and their red runs: substrate an internal report §5.

    const R9PR1_WEEK: i64 = 7 * 24 * 3600;

    fn r9pr1_obs(b: &Budgets, a: &str, pct: f64, secs_left: i64, now: DateTime<Utc>) {
        b.observe(
            a,
            Axis::WeeklyAll,
            pct,
            Some((now + ChronoDuration::seconds(secs_left)).to_rfc3339()),
            "usage_poll",
        )
        .unwrap();
    }

    /// The pre-R9.PR1 inline formula, `((s − u) / s) / t`, written out so the
    /// guards below can say where the new number AGREES with the old one.
    fn r9pr1_old(share: f64, used: f64, t: f64) -> f64 {
        ((share - used) / share) / t
    }

    /// **(a) PLACEMENT RANKS EXACTLY THE VALUE THE READING PUBLISHES**, for
    /// three accounts on three different shares (0.5, 0.9, 1.0) and three
    /// different values — so the number is per-account, not a pool constant.
    ///
    /// THE MUTATION: `pool_pressure` computing its own `((s−u)/s)/t` instead of
    /// reading `pressure_reading().value` — the 1.0-share account then ranks
    /// 1.0 while the reading publishes 0.947, and the identity assert fires.
    #[test]
    fn r9pr1_placement_and_the_published_value_are_one_number_per_account() {
        let b = svc();
        let now = crate::clock::now();
        set_share(&b, "half", 0.5);
        set_share(&b, "ninety", 0.9);
        // `whole` has no row: share 1.0, so the 0.95 cap binds.
        r9pr1_obs(&b, "half", 20.0, R9PR1_WEEK / 2, now);
        r9pr1_obs(&b, "ninety", 85.0, R9PR1_WEEK / 5, now);
        r9pr1_obs(&b, "whole", 50.0, R9PR1_WEEK / 2, now);
        let table = b.pressure_table(&names(&["half", "ninety", "whole"]), now);
        let expected = [
            ("half", ((0.5 - 0.2) / 0.5) / 0.5),
            ("ninety", ((0.9 - 0.85) / 0.9) / 0.2),
            ("whole", ((0.95 - 0.5) / 0.95) / 0.5),
        ];
        let mut values = Vec::new();
        for (a, want) in expected {
            let published = b.pressure_reading(a, now).value.expect("ranked");
            let ranked_by_placement = b.pool_pressure(a, now).value().expect("ranked");
            assert_eq!(
                ranked_by_placement,
                published.rank_key(),
                "{a}: placement ranks {ranked_by_placement}, the reading publishes {published:?}"
            );
            let row = table.accounts.iter().find(|r| r.account == a).unwrap();
            assert_eq!(row.pressure, published.finite(), "{a}: the wire row is the same value");
            assert!(
                (ranked_by_placement - want).abs() < 1e-9,
                "{a}: want {want}, got {ranked_by_placement}"
            );
            values.push(ranked_by_placement);
        }
        values.sort_by(|x, y| x.partial_cmp(y).unwrap());
        values.dedup();
        assert_eq!(values.len(), 3, "three accounts, three different values: {values:?}");
    }

    /// **WHERE THE OLD AND NEW NUMBERS AGREE, THE DECISION IS UNCHANGED.**
    /// Shares 0.5 and 0.9 sit under the 0.95 cap, so `cap = share` and the new
    /// value is the old one exactly; both reads put `b` 0.36 ahead (> the 0.15
    /// band), so placement picks `b` before and after R9.PR1.
    ///
    /// THE MUTATION: `pressure_cap` = `share.min(0.5)` — `b`'s value drops to
    /// 1.2, the agreement assert fires, and `a`/`b` tie inside the band.
    #[test]
    fn r9pr1_where_old_and_new_agree_placement_decides_the_same() {
        let b = svc();
        let now = crate::clock::now();
        flatten_deaths(&b, &["a", "b"]);
        set_share(&b, "a", 0.5);
        set_share(&b, "b", 0.9);
        r9pr1_obs(&b, "a", 20.0, R9PR1_WEEK / 2, now);
        r9pr1_obs(&b, "b", 20.0, R9PR1_WEEK / 2, now);
        for (acct, share) in [("a", 0.5), ("b", 0.9)] {
            let v = b.pool_pressure(acct, now).value().unwrap();
            assert!(
                (v - r9pr1_old(share, 0.2, 0.5)).abs() < 1e-9,
                "{acct}: share {share} is under the cap, so the value must equal the \
                 pre-R9.PR1 formula — got {v}"
            );
        }
        assert_eq!(
            place(&b, &Thresholds::ungated(), &pool(&["a", "b"])),
            Some(placeable("b")),
            "b is 0.36 ahead on both formulas, so it wins on both"
        );
    }

    /// **WHERE THE CAP BINDS, HOW THE DECISION MOVES.** Both accounts lent
    /// 1.0. `late` (70% used, 0.2 of the week left) read 1.50 on the old
    /// formula against `fresh`'s 1.25 — a 0.25 lead, outside the 0.15 band, so
    /// the old placement took `late`. Under `cap = 0.95` `late` reads 1.316
    /// (`fresh` is unchanged at 1.25): the lead shrinks to 0.066, INSIDE the
    /// band, the pressure criterion no longer decides, and the tie-breaks do —
    /// here the candidate order, which puts `fresh` first.
    ///
    /// THE MUTATION: `pressure_cap` = `share` (no 0.95) — `late` is back to
    /// 1.50 and placement takes it again.
    #[test]
    fn r9pr1_where_the_cap_binds_a_lead_can_fall_inside_the_band() {
        let b = svc();
        let now = crate::clock::now();
        flatten_deaths(&b, &["fresh", "late"]);
        r9pr1_obs(&b, "fresh", 0.0, R9PR1_WEEK * 4 / 5, now);
        r9pr1_obs(&b, "late", 70.0, R9PR1_WEEK / 5, now);
        let p = |a: &str| b.pool_pressure(a, now).value().unwrap();
        assert!((p("fresh") - 1.25).abs() < 1e-9, "fresh {}", p("fresh"));
        assert!((p("late") - (0.25 / 0.95) / 0.2).abs() < 1e-9, "late {}", p("late"));
        assert!(r9pr1_old(1.0, 0.7, 0.2) - r9pr1_old(1.0, 0.0, 0.8) > PRESSURE_DEADBAND);
        assert!(p("late") - p("fresh") < PRESSURE_DEADBAND);
        assert_eq!(
            place(&b, &Thresholds::ungated(), &pool(&["fresh", "late"])),
            Some(placeable("fresh")),
            "inside the band the tie-breaks decide, and they take `fresh`"
        );
    }

    /// **∞ RANKS ABOVE EVERY FINITE VALUE, AND THE WIRE SAYS ∞.** `edge` has
    /// 30 s left and half its cap unused: ∞. `steady` reads 1.9. Placement
    /// takes `edge`; the table row carries `infinite: true`, `pressure: null`,
    /// `ranked: true`, rank 1.
    ///
    /// THE MUTATION: `PressureValue::rank_key` mapping `Infinite` to `0.0` —
    /// `steady` wins and the rank assert fires.
    #[test]
    fn r9pr1_infinite_ranks_first_and_is_flagged_on_the_wire() {
        let b = svc();
        let now = crate::clock::now();
        flatten_deaths(&b, &["edge", "steady"]);
        r9pr1_obs(&b, "edge", 50.0, 30, now);
        r9pr1_obs(&b, "steady", 5.0, R9PR1_WEEK / 2, now);
        assert_eq!(b.pressure_reading("edge", now).value, Some(PressureValue::Infinite));
        let table = b.pressure_table(&names(&["steady", "edge"]), now);
        let row = table.accounts.iter().find(|r| r.account == "edge").unwrap();
        assert!(row.infinite && row.ranked && row.pressure.is_none(), "{row:?}");
        assert_eq!(row.rank, Some(1));
        let wire = serde_json::to_value(row).unwrap();
        assert_eq!(wire["infinite"], json!(true));
        assert!(wire["pressure"].is_null());
        assert_eq!(
            place(&b, &Thresholds::ungated(), &pool(&["steady", "edge"])),
            Some(placeable("edge"))
        );
    }
}
