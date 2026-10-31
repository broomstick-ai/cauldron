//! Account registry + tracked usage state (multi-account scaffolding, §10).
//!
//! The registry (`accounts.toml`, name → `CLAUDE_CONFIG_DIR`) is ported
//! verbatim from CCW — `CLAUDE_CONFIG_DIR` is Claude Code's real isolation
//! mechanism (config, transcripts, AND the macOS Keychain entry), so switching
//! accounts is just spawning with a different env value; no new credential
//! mechanics are needed.
//!
//! What is NEW here is the tracked per-account limit state ([`AccountState`],
//! persisted in the `accounts` table): every `limit_hit` event stamps the
//! account's `limit_until` from the parsed reset prose, and
//! [`unavailable_accounts`] / [`unhealthy_accounts`] turn that state into the
//! EXCLUSION SETS placement is handed. The split of responsibility is the point:
//! this module answers "which accounts are barred right now", and
//! `budgets::admit` alone answers "so where does this thread run".
//! Spend attribution per account lives in the usage ledger (Q11, `store.rs`).
//!
//! Also here: the per-account ACTIVITY roll-up ([`AccountActivity`],
//! [`collect_activity`]) — how many threads are executing / parked (and why) /
//! queued on each account, so an agent can tell whether the operator is live on
//! an account before starting work on it. Read-only over already-durable state;
//! it never polls.
//!
//! SEAM — live `/usage` probing: CCW shelled `claude /usage` per account for a
//! zero-cost usage snapshot (`ccw::claude::usage_snapshot`). That probe is NOT
//! ported yet; availability here is derived from observed `limit_hit` events
//! only. Port the probe behind [`UsageProbe`] when real-claude burn-in starts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The name reserved for the machine default (env unset) account.
pub const DEFAULT_ACCOUNT: &str = "default";

/// Warn a full week ahead of the refresh-token cliff: the operator has to be at
/// a browser to fix it, and a warning that arrives the day of is not a warning.
///
/// Lifted out of `main.rs::accounts_health` so the CLI and the `accounts.health`
/// WS arm cannot be given different thresholds — a difference that would make
/// the two surfaces disagree about which accounts are in trouble while both
/// looked correct.
pub const HEALTH_WARN_BELOW_DAYS: f64 = 7.0;

/// **THE CREDENTIAL-FLEET HEALTH PAYLOAD — one builder, both surfaces.**
///
/// `threads accounts health --json` printed this inline in `main.rs`, and it
/// was the ONLY way to get it: the accounts interface shelled out to the
/// binary and paid a process spawn plus up to thirty seconds for every board
/// build, which is why it could show a stale board straight after a login. The
/// WS arm and the CLI now both call this and are byte-identical over the same
/// data dir, asserted by
/// `tests/ws_protocol.rs::accounts_health_is_on_the_wire_and_byte_identical_to_the_cli`
/// — the same proof `accounts.status` already carries.
///
/// # This payload still contains the machine-default slot, deliberately
///
/// `ensure_default` runs here and the `default` row stays in `slots`, tagged
/// `folds_into`. That is NOT the placement default the operator struck: this is
/// the CREDENTIAL fleet, rotation reasons about the machine store and must keep
/// seeing it, and the operator's one stated exception — *"setting my local
/// account"* — is exactly this concept. It is reported as a residue in the
/// sweep rather than removed here, because removing it would break rotation
/// and is a ruling to take rather than a cleanup to perform.
///
/// Returns `Err` for the two conditions the CLI exits non-zero on, so the WS
/// arm reports them as a protocol error instead of killing the daemon.
pub fn health_json(data_dir: &Path, home: &Path, now_ms: i64) -> Result<serde_json::Value> {
    let mut registry = Registry::load(&data_dir.join("accounts.toml"))?;
    registry.ensure_default();

    // R9.A2c (CON-28): a disabled slot is NOT READ. It still has a row — a
    // disabled account is still one of his accounts, and disappearing from
    // this list is a failure (`account_disable.rs`) — but the row is built
    // from config alone: state `disabled`, no dates, no identity, and a
    // rotation verdict of `skip` that gives no sign-in advice. `rotate`
    // executes that verdict, so a disabled slot is never refreshed either.
    let (mut reports, disabled) =
        collect_slot_inventory(&registry, data_dir, home, now_ms, HEALTH_WARN_BELOW_DAYS);
    // Which real account IS the machine login.
    let pinned = read_live_login_pointer(data_dir);
    mark_live_login(&mut reports, pinned.as_deref());
    let live_login = reports.iter().find(|r| r.is_live_login).map(|r| r.label.clone());

    let mut slots: Vec<serde_json::Value> = reports
        .iter()
        .map(|r| serde_json::to_value(r).expect("a SlotReport serializes"))
        .collect();
    slots.extend(disabled.iter().map(|(label, config_dir)| disabled_slot_row(label, config_dir)));
    slots.sort_by(|a, b| a["label"].as_str().cmp(&b["label"].as_str()));

    Ok(serde_json::json!({
        "now_ms": now_ms,
        "keychain_account": keychain_account(),
        "accounts_dir": data_dir.join("accounts").to_string_lossy(),
        "warn_below_days": HEALTH_WARN_BELOW_DAYS,
        "live_login": live_login,
        "slots": slots,
    }))
}

/// **A disabled slot's `health --json` row, from config alone (R9.A2c,
/// CON-28).** Nothing about its login is known, because nothing was read:
/// `health.state` is `disabled`, the dates and identity are absent, and
/// `rotation` is `skip` with a reason that names the disable and gives no
/// sign-in advice.
fn disabled_slot_row(label: &str, config_dir: &Option<String>) -> serde_json::Value {
    serde_json::json!({
        "label": label,
        "config_dir": config_dir,
        "keychain_service": keychain_service_for(config_dir.as_deref()),
        "health": {"state": "disabled"},
        "days_left": null,
        "access_expired": false,
        "secret_sha8": null,
        "identity": null,
        "credential_mode": null,
        "subscription_type": null,
        "rotation": {
            "action": "skip",
            "reason": "the account is disabled — its slot is not checked and not rotated",
        },
        "warning": null,
        "is_machine_default": false,
        "is_live_login": false,
        "disabled": true,
    })
}

/// `$HOME` as a path, or the error every caller of [`health_json`] reports the
/// same way. Slot reports are resolved relative to the operator's home, so an
/// unset `HOME` is not a thing to guess at.
pub fn home_dir() -> Result<PathBuf> {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => Ok(PathBuf::from(h)),
        _ => Err(Error::Config("HOME is unset".into())),
    }
}

/// One registered account.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Account {
    /// `CLAUDE_CONFIG_DIR` for this account. Absent = the machine default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<String>,
}

impl Account {
    pub fn default_machine() -> Self {
        Self { config_dir: None }
    }
}

/// The full registry, serialized to/from `accounts.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub accounts: BTreeMap<String, Account>,
}

impl Registry {
    pub fn from_toml(s: &str) -> Result<Self> {
        toml::from_str(s).map_err(|e| Error::Config(format!("parsing accounts.toml: {e}")))
    }

    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self)
            .map_err(|e| Error::Config(format!("rendering accounts.toml: {e}")))
    }

    /// Load from `path`; a missing file yields an empty registry.
    pub fn load(path: &std::path::Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => Self::from_toml(&s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(Error::Config(format!("reading {}: {e}", path.display()))),
        }
    }

    /// Save to `path` (creating parent dirs).
    ///
    /// **THE MACHINE DEFAULT IS NEVER WRITTEN TO DISK, from any caller.**
    ///
    /// This is the choke point, and it is placed here rather than at the call
    /// sites because the call sites were the problem. The sibling migrating
    /// persisted state removed `[accounts.default]` from `accounts.toml`,
    /// deleted the `default` row from the `accounts` table and cleaned the
    /// budget scope — and the very next `threads accounts adopt`, which
    /// `claude-accounts login` runs, would have put the stanza straight back,
    /// because `adopt` calls `ensure_default()` and then `save()`.
    ///
    /// `build_manager` had the same shape by a different route: it LOADS a
    /// registry that may already carry the row, merges `config.toml`'s
    /// `[accounts]` seeds — which is where `default = ""` lives (see
    /// `config.rs`) — and saves the whole thing back the moment anything
    /// differs. So the row could return without a single caller intending it.
    ///
    /// A fix applied to the six known call sites would have been a fix that
    /// holds until somebody adds a seventh. Filtering at the WRITE makes the
    /// reappearance impossible rather than unlikely, which is the standard the
    /// operator set for this:
    ///
    /// > *"We do not want the threads module to have any concept of the
    /// > default."*
    ///
    /// **In-memory it may still exist, and that is deliberate.** The credential
    /// fleet is the one stated exception — `collect_slot_reports` reports the
    /// machine store's health and rotation must keep seeing it, and
    /// `delete_refusal` needs it to refuse deleting the live login. Those are
    /// READS, they never save, and they are the *"set my local account"* half
    /// the operator carved out. What is forbidden is persisting it as a
    /// registered account, because that is the thing the daemon then treats as
    /// a place work can go.
    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Config(format!("creating {}: {e}", parent.display())))?;
        }
        let mut on_disk = self.clone();
        if on_disk.accounts.remove(DEFAULT_ACCOUNT).is_some() {
            tracing::debug!(
                path = %path.display(),
                "the machine default is not written to accounts.toml — it is not a \
                 registered account and never becomes one by being saved"
            );
        }
        std::fs::write(path, on_disk.to_toml()?)
            .map_err(|e| Error::Config(format!("writing {}: {e}", path.display())))
    }

    /// Ensure the machine default is registered (idempotent). Returns true if
    /// it was just added (first-run behavior).
    pub fn ensure_default(&mut self) -> bool {
        if self.accounts.contains_key(DEFAULT_ACCOUNT) {
            return false;
        }
        self.accounts
            .insert(DEFAULT_ACCOUNT.to_string(), Account::default_machine());
        true
    }

    /// Add (or replace) an account. `config_dir = None` = machine default.
    pub fn add(&mut self, name: &str, config_dir: Option<String>) {
        self.accounts
            .insert(name.to_string(), Account { config_dir });
    }

    pub fn get(&self, name: &str) -> Option<&Account> {
        self.accounts.get(name)
    }

    /// All registered LOGIN SLOTS, sorted.
    ///
    /// **This is the slot-management view, and it is NOT the pool.** It belongs
    /// to the `claude-accounts` CLI — the separate utility that decides which
    /// login sits in which slot on this machine, including the machine-default
    /// slot. The daemon's account list is [`crate::pool::Pool::names`], which is
    /// a different list from a different file and is not this one filtered.
    ///
    /// Nothing on the placement, metering, gauge or status path may call this.
    /// `tests/default_out_of_placement.rs` guards that.
    pub fn names(&self) -> Vec<String> {
        self.accounts.keys().cloned().collect()
    }
}

/// **A member of the pool — the only thing placement can name (CON-27).**
///
/// The operator's words (intent item 70, msg #1949): *"I didn't even want the
/// default account to be in scope of what the threads module has access to. It
/// shouldn't even be on the table… Threads has two logins that are independent
/// of the one that I use on my computer… And threads, by the way, should never
/// access my default account."*
///
/// # Why this is a TYPE
///
/// A check at one placement call site is a check the NEXT placement call site
/// will not have. So the placement domain does not take account names at all.
/// [`crate::budgets::PlacementRequest::candidates`] and
/// [`crate::budgets::AdmissionRequest::accounts`] take `[Placeable]`, and
/// [`crate::budgets::Budgets::place`] RETURNS one, so an account that cannot be
/// made into a `Placeable` cannot be ranked, cannot be failed over to, and
/// cannot be recovered onto — there is no expressible program that does it.
///
/// # What CHANGED on 2026-08-27, and why the constructor is now infallible
///
/// This type used to be built from a registry row and REFUSED the machine
/// default twice over: by reserved name (`name == "default"`) and by the
/// absence of a config dir. The operator struck the first of those and, with
/// it, the framing underneath:
///
/// > *"I do not want default even in the purview of the system. Why does it
/// > even see that there is a default? … It's a completely separate system, not
/// > even visible to placement."*
///
/// A module that refuses `default` by name is a module that knows the concept,
/// which is the thing he has ruled against repeatedly. So the refusal moved
/// UPSTREAM and stopped being a refusal: a `Placeable` is now minted only from
/// a [`crate::pool::PoolEntry`], and the machine default is not a pool entry —
/// not because it is excluded, but because a pool entry must name the directory
/// its login is stored in and the machine default is exactly the account that
/// names none. **Membership in the pool IS placeability**, so there is nothing
/// left to check and nothing left to forget.
///
/// # Why a future author cannot get around it
///
/// The inner field is PRIVATE TO THIS MODULE and [`Placeable::of`] is
/// `pub(crate)`, callable only from [`crate::pool`]. `Placeable(name)` does not
/// compile in `runtime.rs`, `budgets.rs`, `server.rs`, or any test. There is
/// deliberately NO `From<String>`, NO `FromStr`, and NO `Default`: every one of
/// those would be a way to say "trust me, this name is fine", which is
/// precisely the sentence CON-27 exists to make unsayable.
/// `default_out_of_placement.rs` guards those absences.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Placeable(String);

impl Placeable {
    /// Mint a `Placeable` from a pool entry's name.
    ///
    /// `pub(crate)` and deliberately not `pub`: the ONLY caller is
    /// [`crate::pool::PoolEntry::placeable`], so the sentence "this name is
    /// placeable" can only be uttered by something that already IS a pool
    /// entry. It is infallible because by the time an entry exists the question
    /// has been answered — see the type banner.
    pub(crate) fn of(name: &str) -> Self {
        Self(name.to_string())
    }

    /// The account name. Placement results cross back into `String` here, at
    /// one named place.
    pub fn name(&self) -> &str {
        &self.0
    }

    pub fn into_name(self) -> String {
        self.0
    }
}

impl std::ops::Deref for Placeable {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for Placeable {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for Placeable {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Placeable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl PartialEq<str> for Placeable {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for Placeable {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for Placeable {
    fn eq(&self, other: &String) -> bool {
        &self.0 == other
    }
}

impl PartialEq<Placeable> for String {
    fn eq(&self, other: &Placeable) -> bool {
        self == &other.0
    }
}

/// **The selectable pool: one entry per distinct login, healthy only.**
///
/// The collapse runs over the POOL and nothing else, which is what finally
/// settles a bug that two previous fixes only moved. The pool used to be built
/// by collapsing ALL REGISTRY LABELS into logins and taking each login's
/// alphabetically-first healthy label as its canonical name. On the operator's
/// own machine `default` and `owner-account` carry the SAME identity
/// fingerprint — the machine login happens to be signed in as the same person —
/// so they collapsed into one login and `default`, which sorts first, became
/// that login's canonical label. Placement was not choosing the machine default
/// by accident; it was choosing it AS THE NAME OF an account threads owns. That
/// is how 119 threads came to sit on it.
///
/// The first fix dropped the machine default BEFORE the collapse rather than
/// after. This one removes the step that had to be ordered at all: the machine
/// default is not in the input, because the input is the pool
/// ([`crate::pool::Pool`]) rather than the registry, and it is not a pool
/// member. There is no ordering left to get wrong.
pub fn selectable_pool(
    pool: &crate::pool::Pool,
    states: &BTreeMap<String, AccountState>,
) -> Vec<Placeable> {
    let placeable = pool.placeable();
    let names: Vec<String> = placeable.iter().map(|p| p.name().to_string()).collect();
    selection_units(&names, states)
        .into_iter()
        .filter(|u| u.healthy)
        .filter_map(|u| placeable.iter().find(|p| p.name() == u.canonical).cloned())
        .collect()
}

/// The `CLAUDE_CONFIG_DIR` value for an account, if any (None = env unset).
pub fn config_dir_of(acct: &Account) -> Option<PathBuf> {
    acct.config_dir.as_ref().map(PathBuf::from)
}

/// Tracked per-account limit state (the `accounts` table row).
#[derive(Debug, Clone, Default)]
pub struct AccountState {
    pub name: String,
    pub config_dir: Option<String>,
    /// RFC3339 UTC instant this account's last observed usage limit resets.
    /// `None` / past = presumed available.
    pub limit_until: Option<String>,
    /// Verbatim reset prose from the last `limit_hit` (display).
    pub last_limit_prose: Option<String>,
    /// The login IDENTITY this account's config dir points at, as an opaque
    /// hash ([`identity_fingerprint`]). Two registry entries carrying the SAME
    /// fingerprint are two LABELS for one login, not two accounts — see
    /// [`selection_units`]. `None` = never computed, or the config dir holds no
    /// readable `oauthAccount` block; absence is never treated as a match.
    pub identity: Option<String>,
    /// RFC3339 instant this login was proven DEAD (an authentication failure
    /// that no retry can fix — [`is_dead_login_error`]), or `None` while it is
    /// presumed healthy. A dead account is excluded from selection and from
    /// failover, and stays fully visible in `accounts status`.
    pub dead_since: Option<String>,
    /// The verbatim failure text that proved it dead (display).
    pub dead_reason: Option<String>,
    /// The login [`Self::limit_until`] was observed on, stamped from
    /// [`Self::identity`] when the 429 was recorded. `None` = the limit could
    /// not be attributed (a row written before this column existed, or a 429
    /// taken while the config dir held no readable identity).
    pub limit_identity: Option<String>,
    /// Which quota refused (`gauge::Axis`: session | weekly_all | weekly_fable),
    /// or `None` when the prose named none. Display and diagnosis only — the
    /// bar it annotates is still account-wide.
    pub limit_axis: Option<String>,
    pub updated_at: Option<String>,
}

impl AccountState {
    /// **Does this row's limit still belong to the login this label points at?**
    ///
    /// A limit is a fact about a LOGIN. `name` is a label, and a label can be
    /// re-pointed at a different login without the daemon being told: the
    /// machine-default label has no config dir, so it is whatever
    /// `~/.claude.json` currently holds, and a human `/login` swaps that out
    /// underneath it. `identity` is only recomputed at daemon boot
    /// (`runtime::refresh_identities`), and it is written blind — nothing
    /// compares the new fingerprint against the old one.
    ///
    /// So the stale case is real and it is not rare: the label keeps a
    /// `limit_until` belonging to a login it no longer names, and because
    /// [`with_aliases`] widens a bar across every label of the login it *now*
    /// names, the foreign limit is promoted into a bar on a different, healthy
    /// account. On 2026-08-02 that stranded a thread for five and a half hours
    /// on a login that was at 29% and answering turns throughout.
    ///
    /// Only a POSITIVE disagreement falsifies a limit. Two known, differing
    /// fingerprints mean the limit was observed somewhere else. A `None` on
    /// either side means we cannot tell, and a limit we cannot attribute keeps
    /// barring — un-barring on absence of evidence would hammer an account that
    /// really is exhausted, which is the worse of the two errors.
    pub fn limit_is_stale(&self) -> bool {
        match (&self.limit_identity, &self.identity) {
            (Some(observed_on), Some(points_at)) => observed_on != points_at,
            _ => false,
        }
    }

    /// The limit window this row actually imposes: [`Self::limit_until`] unless
    /// it belongs to a login this label no longer points at, **or its window has
    /// already ended**.
    ///
    /// Every reader — the gate, `accounts status`, the `accounts.list` wire
    /// payload — goes through this, so what the operator is shown and what
    /// placement believes cannot disagree.
    ///
    /// # Expiry is the second retraction, and it was missing
    ///
    /// This checked STALENESS only. A row whose window had simply ended still
    /// reported that window, to every display reader there is, forever — the
    /// column is a watermark and nothing rewrites it when the clock passes it.
    /// On 2026-10-06 `alice-example-com` was still publishing
    /// `Some("2026-10-02T15:45:29Z")`, four days in the past, on every
    /// `accounts.list` hydrate (`shapes::accounts`) and every live fold, so the
    /// board logged `barred=true` for a window that ended on 2 October and the
    /// `limit.prose` cell carried the OAuth failure sentence that bar was
    /// written from.
    ///
    /// It was never a PLACEMENT bug, which is exactly why it survived:
    /// [`Self::available`] compared against `now` and therefore read correctly,
    /// so the account was never actually held out of the pool. It was a
    /// wire-and-display inconsistency between two readings of one column — and
    /// the whole point of this accessor is that there is only one reading.
    ///
    /// The rule is the one `804cffd` established at the producer and it is
    /// quoted here verbatim, because the same sentence has to govern both ends:
    /// **a reset at or before now describes a window that has ended.** There it
    /// meant `resolve` answers `None` rather than rolling forward a day; here it
    /// means this answers `None` rather than republishing a watermark.
    ///
    /// `now_rfc3339` is a PARAMETER rather than a `Utc::now()` call for the same
    /// reason it is one on [`Self::available`]: this module is pure, the gate's
    /// sweep already holds one `now` for a whole pass, and a test has to be able
    /// to name the instant it is asking about.
    ///
    /// The comparison is the string compare [`Self::available`] has always used,
    /// moved here rather than duplicated. That is deliberate: both instants are
    /// written by `to_rfc3339()` on a `Utc` value, so the form is fixed, and the
    /// one thing that must never happen is the gate and the display disagreeing
    /// about expiry because one parses and the other compares bytes. There is
    /// now exactly one predicate and `available` is defined in terms of it.
    pub fn effective_limit_until(&self, now_rfc3339: &str) -> Option<&str> {
        if self.limit_is_stale() {
            return None;
        }
        self.limit_until.as_deref().filter(|t| *t > now_rfc3339)
    }

    /// Is this account presumed to have usage available at `now_rfc3339`?
    ///
    /// Now literally "no effective window": the expiry comparison this used to
    /// own lives in [`Self::effective_limit_until`], so the gate and every
    /// display reader answer from the same predicate instead of from two.
    pub fn available(&self, now_rfc3339: &str) -> bool {
        self.effective_limit_until(now_rfc3339).is_none()
    }

    /// **Does the bar this row carries make the account's PRESSURE noise?**
    ///
    /// A SECOND question about the same column, and it is not
    /// [`Self::available`]. Availability decides whether work may be PLACED
    /// here; this decides whether the account's rank and share are still worth
    /// showing. They had been the same boolean, and that is the defect:
    ///
    /// > *"note that the threads tui should display pressure on accounts even if
    /// > they are at the session limit. They should only not display pressure if
    /// > they are at the weekly limit."* — the operator, 2026-10-06
    ///
    /// `budgets::pressure_reading` read `effective_limit_until` — which carries
    /// no axis — and suppressed the number for ANY bar, so the board was handed
    /// `pressure: null, unranked_reason: "unavailable"` whatever had said no. The
    /// axis was sitting on this struct the whole time, one field along from the
    /// instant.
    ///
    /// # The rule, and every axis it is applied to
    ///
    /// **Pressure survives a bar that lifts on its own within hours. It does not
    /// survive one measured in days, or one that waits on a human.** The operator
    /// named the two ends; this is that principle carried across all five values
    /// the column actually holds, because an axis handled by omission is the next
    /// defect.
    ///
    /// * `session` — **KEEPS its pressure.** The session pool is five hours, so
    ///   the rank and share answer a live question: where work will land when
    ///   this clears. There is also no arithmetic objection — pressure is
    ///   computed from the WEEKLY axis, which a session bar says nothing about,
    ///   so a session-barred account's number is an ordinary one.
    /// * `weekly_all` — **loses it.** Days, and worse: this is the very axis
    ///   pressure is built from, so a bar on it means the numerator is at or past
    ///   the share with the window closing. That is the −49.63 the operator
    ///   reported, and the reason the suppression exists at all.
    /// * `weekly_fable` — **loses it.** The same cadence. Named explicitly rather
    ///   than caught by a `starts_with("weekly")` test, so a future axis has to
    ///   be classified rather than inheriting a spelling.
    /// * `login` — **loses it.** An expired credential is lifted by `claude
    ///   /login` and by nothing else, so there is no instant to wait for, and a
    ///   quota rank on an account that cannot authenticate describes capacity it
    ///   has no way to spend.
    /// * `credit` — **loses it.** A human has to top the account up. The bar's own
    ///   clock is a re-probe interval, not a reset.
    /// * **`None` — loses it.** A bar we cannot attribute keeps barring, which is
    ///   the direction [`Self::limit_is_stale`] and
    ///   `runtime::note_account_quota_measured_open` already take and for the same
    ///   reason. It is also the safe default for rows written before
    ///   `limit_axis` existed: treating an unknown bar as a session bar would
    ///   quietly hand the −49.63 back for every legacy row.
    ///
    /// So exactly one value keeps its pressure, and it is the one he named.
    ///
    /// # It composes with expiry rather than repeating it
    ///
    /// Asked through [`Self::effective_limit_until`], so a bar that has ENDED
    /// hides nothing — there is no bar. One predicate for "is there a live bar",
    /// one for "what kind", and no third reading of the column.
    pub fn limit_hides_pressure(&self, now_rfc3339: &str) -> bool {
        if self.effective_limit_until(now_rfc3339).is_none() {
            return false;
        }
        self.limit_axis.as_deref() != Some(crate::gauge::Axis::Session.as_str())
    }

    /// Is this login usable at all? False once an auth failure proved it dead.
    pub fn healthy(&self) -> bool {
        self.dead_since.is_none()
    }

    /// Usable as a selection or failover candidate right now: healthy AND not
    /// inside an observed limit window.
    pub fn selectable(&self, now_rfc3339: &str) -> bool {
        self.healthy() && self.available(now_rfc3339)
    }
}

/// Is `name` a candidate for selection / failover at all? An account with no
/// tracked row has never hit a limit and has never failed to authenticate, so
/// it is presumed healthy and available — absence of evidence is not a fault.
pub fn selectable(
    states: &BTreeMap<String, AccountState>,
    name: &str,
    now_rfc3339: &str,
) -> bool {
    states
        .get(name)
        .map(|s| s.selectable(now_rfc3339))
        .unwrap_or(true)
}

/// Widen a set of barred labels to **every registry label for the same login**,
/// sorted.
///
/// This is the clause that makes an exclusion honest rather than arithmetic on
/// a fiction. Two labels can point at one underlying login (the captured
/// `secondary-live` clone and the machine `default` did exactly this: same
/// `accountUuid`, `emailAddress`, `organizationUuid`), and a limit window or a
/// dead session is a fact about the LOGIN, not about the label — "failing over"
/// between two names for one login re-hits the very limit it is running from,
/// at the cost of the thread's conversation.
///
/// Fingerprints are opaque hashes and an UNKNOWN fingerprint never matches
/// anything, so a missing fingerprint can only ever leave an extra account IN
/// the field, never take a real one out of it.
fn with_aliases(
    names: &[String],
    states: &BTreeMap<String, AccountState>,
    mut barred: std::collections::BTreeSet<String>,
) -> Vec<String> {
    let identities: std::collections::BTreeSet<String> = barred
        .iter()
        .filter_map(|n| states.get(n).and_then(|s| s.identity.clone()))
        .collect();
    if !identities.is_empty() {
        for name in names {
            if states
                .get(name)
                .and_then(|s| s.identity.as_ref())
                .is_some_and(|id| identities.contains(id))
            {
                barred.insert(name.clone());
            }
        }
    }
    barred.into_iter().collect()
}

/// **The accounts that cannot serve a turn at all right now**, sorted: inside an
/// observed usage-limit window ([`AccountState::available`]), plus every other
/// label for those logins.
///
/// This is half of what replaces the bespoke 429 selector (`select_failover`),
/// which ranked the registry itself and was therefore a second placement
/// implementation sitting beside `budgets::place`. What that selector decided
/// privately is now an INPUT — `budgets::AdmissionRequest::unavailable` — and
/// the deciding is left to the one ranking that already existed.
///
/// An account inside a limit window bars a thread from STAYING on it as well as
/// from being placed there: a 429 is not a preference, the account genuinely
/// cannot answer. An account with no tracked row is absent from the result — it
/// has never hit a limit, and absence of evidence is not a fault.
pub fn unavailable_accounts(
    names: &[String],
    states: &BTreeMap<String, AccountState>,
    now_rfc3339: &str,
) -> Vec<String> {
    let limited = names
        .iter()
        .filter(|n| {
            states
                .get(n.as_str())
                .map(|s| !s.available(now_rfc3339))
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    with_aliases(names, states, limited)
}

/// **The accounts nothing may be PLACED on**, sorted: logins proven dead
/// ([`AccountState::healthy`]), plus every other label for them.
///
/// The other half of the retired selector. The asymmetry with
/// [`unavailable_accounts`] is deliberate and matches what the dead flag has
/// always meant (`runtime::note_login_health`): a dead login is *not a selection
/// candidate and not a failover candidate*, but the thread already sitting on
/// one keeps taking its turns there — the flag is inferred from a single error
/// string, and a turn that SUCCEEDS on that account is the only thing that ever
/// clears it. Bar the thread that is already there and the flag becomes
/// permanent.
pub fn unhealthy_accounts(
    names: &[String],
    states: &BTreeMap<String, AccountState>,
) -> Vec<String> {
    let dead = names
        .iter()
        .filter(|n| {
            states
                .get(n.as_str())
                .map(|s| !s.healthy())
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    with_aliases(names, states, dead)
}

/// One LOGIN, and every registry label that points at it.
///
/// This is the unit every pool count, exclusivity claim and "N accounts means
/// N×100% capacity" statement has to be made in. Before it existed the registry
/// had no notion of identity at all, so two labels for one login counted as two
/// accounts.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectionUnit {
    /// The label selection uses for this login: the alphabetically first
    /// HEALTHY member, else the alphabetically first member.
    pub canonical: String,
    /// Every registry label pointing at this login, sorted.
    pub members: Vec<String>,
    /// The shared identity fingerprint, or `None` for a unit of one label whose
    /// identity could not be read (never collapsed with anything).
    pub identity: Option<String>,
    /// True when at least one member's login still authenticates.
    pub healthy: bool,
}

impl SelectionUnit {
    /// Is this login a duplicate label situation (more than one name)?
    pub fn is_aliased(&self) -> bool {
        self.members.len() > 1
    }
}

/// Collapse registry labels into LOGINS by identity fingerprint.
///
/// Labels with a known, shared fingerprint form one unit. A label whose
/// fingerprint is unknown forms a unit of its own — absence of evidence never
/// merges two accounts, because merging wrongly would hide a real account from
/// selection, which is the worse error of the two.
pub fn selection_units(
    names: &[String],
    states: &BTreeMap<String, AccountState>,
) -> Vec<SelectionUnit> {
    let mut by_identity: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut solo: Vec<String> = Vec::new();
    for name in names {
        match states.get(name).and_then(|s| s.identity.clone()) {
            Some(fp) => by_identity.entry(fp).or_default().push(name.clone()),
            None => solo.push(name.clone()),
        }
    }
    let healthy_of = |members: &[String]| -> bool {
        members
            .iter()
            .any(|m| states.get(m).map(|s| s.healthy()).unwrap_or(true))
    };
    let canonical_of = |members: &[String]| -> String {
        members
            .iter()
            .find(|m| states.get(*m).map(|s| s.healthy()).unwrap_or(true))
            .cloned()
            .unwrap_or_else(|| members[0].clone())
    };
    let mut out: Vec<SelectionUnit> = Vec::new();
    for (fp, mut members) in by_identity {
        members.sort();
        out.push(SelectionUnit {
            canonical: canonical_of(&members),
            healthy: healthy_of(&members),
            members,
            identity: Some(fp),
        });
    }
    for name in solo {
        let members = vec![name];
        out.push(SelectionUnit {
            canonical: members[0].clone(),
            healthy: healthy_of(&members),
            members,
            identity: None,
        });
    }
    out.sort_by(|a, b| a.canonical.cmp(&b.canonical));
    out
}

/// The HONEST account count: how many distinct logins we hold that still
/// authenticate. This — not `registry.names().len()` — is the number any
/// capacity or exclusivity claim may be made over.
pub fn distinct_healthy_logins(
    names: &[String],
    states: &BTreeMap<String, AccountState>,
) -> usize {
    selection_units(names, states)
        .into_iter()
        .filter(|u| u.healthy)
        .count()
}

/// Does this failure text PROVE the login is dead — i.e. no retry, no reset and
/// no other thread can make it work again, only a human `/login`?
///
/// Deliberately narrow. A dead flag that fires on a transient blip would take a
/// working account out of the pool, so only failures that name the credential
/// itself qualify. The live case this was written against is the `primary`
/// snapshot: *"Failed to authenticate: OAuth session expired and could not be
/// refreshed"* — Claude Code rotates the refresh token on refresh, so a cloned
/// session dies permanently the moment the original refreshes.
///
/// A rate limit, an overload, a connection drop and an exhausted credit balance
/// are all explicitly NOT this: the first three heal on their own and the
/// fourth is a billing state, not a broken login.
pub fn is_dead_login_error(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    if t.contains("credit balance") || t.contains("insufficient credit") {
        return false;
    }
    let auth_shaped = t.contains("failed to authenticate")
        || t.contains("authentication_error")
        || t.contains("authentication failed")
        || t.contains("invalid api key")
        || t.contains("oauth token has expired")
        || t.contains("oauth session expired")
        || t.contains("please run /login")
        || t.contains("run `/login`")
        || t.contains("not logged in");
    if !auth_shaped {
        return false;
    }
    // "expired and could not be refreshed" is the proof; a bare 401 that
    // Claude Code retried through is not, so require the terminal phrasing OR
    // an explicit instruction to log in again.
    t.contains("could not be refreshed")
        || t.contains("expired")
        || t.contains("/login")
        || t.contains("invalid api key")
        || t.contains("not logged in")
        || t.contains("authentication_error")
        || t.contains("authentication failed")
}

/// The login IDENTITY behind a `.claude.json`, as an opaque comparison key.
///
/// Built from [`IdentityFingerprint`] — the one fingerprinting implementation in
/// this crate — as `<sha8(accountUuid)>.<sha8(emailAddress)>`, i.e. exactly the
/// pair [`IdentityFingerprint::same_account_as`] compares. **Only hashes exist
/// after this function returns**: no uuid, email, org name, token or any other
/// credential value is returned, stored, logged or compared.
///
/// The organization is deliberately NOT part of the key. Two labels differing
/// only by active org are the same subscription, so they share one set of usage
/// limits — which is the only thing selection cares about. (The registry's
/// `secondary` label was exactly this case: same account and email as the live
/// login, different `organizationUuid`.)
///
/// `None` when the file is missing, unparseable, or carries no `oauthAccount`
/// block. An unknown identity is never treated as equal to anything else.
pub fn identity_fingerprint(config_json: &std::path::Path) -> Option<String> {
    let fp = IdentityFingerprint::from_config_json(config_json).ok()??;
    Some(format!("{}.{}", fp.acct, fp.email))
}

/// The `.claude.json` an account's spawns will read: `<config_dir>/.claude.json`
/// when the account sets `CLAUDE_CONFIG_DIR`, else `$HOME/.claude.json`.
pub fn config_json_for(config_dir: Option<&str>, home: &std::path::Path) -> PathBuf {
    match config_dir.filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d).join(".claude.json"),
        None => home.join(".claude.json"),
    }
}

/// The identity fingerprint of a registered account, resolved through its
/// config dir. Read-only: opens one JSON file and hashes three fields. It never
/// invokes `claude`, never touches the keychain, and costs nothing.
pub fn identity_of(config_dir: Option<&str>, home: &std::path::Path) -> Option<String> {
    identity_fingerprint(&config_json_for(config_dir, home))
}

// ─────────────────────────────────────────────────────────────────────────
// TRANSCRIPT PORTING — a thread's conversation is a FILE (msg #1028)
//
// The old rule "a thread with history never moves" was SUPERSEDED by the
// operator: "if we're running a thread on one account and then it hits the
// budget for that account, we should continue running the thread on the other
// account… I just care about continuity and it staying within its threshold
// and budget."
//
// What the old rule protected is the conversation, and the conversation is one
// file: Claude Code writes each session's transcript to
// `<state_dir>/projects/<cwd-slug>/<session>.jsonl`, and the daemon's thread id
// IS the claude session id. Port the file and the thread ports — his own
// machine proves the principle: one transcript store, switchable credentials.
//
// THE CWD SLUG IS NEVER RE-DERIVED. Claude Code's slug rule is its own, and a
// re-implementation that drifted would answer "the conversation is not here",
// which is the one answer that must never be wrong. So the source is found by
// SCANNING `<projects>/*/<session>.jsonl`, and the destination reuses the
// source directory's own basename — the slug is a function of the cwd, which is
// the same on both sides, so copying the name across is exact by construction.
//
// CRASH SAFETY is copy-verify-then-activate:
//
//   1. count the source's events,
//   2. stream the bytes into a dot-prefixed `.port-<pid>-<nanos>` temp file in
//      the DESTINATION directory (same filesystem ⇒ the rename below is
//      atomic), and `fsync` it,
//   3. VERIFY by counting the temp's events and re-counting the source's; the
//      copy must be a consistent snapshot,
//   4. `rename` the temp over the destination — the single atomic instant at
//      which the destination becomes readable as a transcript at all,
//   5. `fsync` the destination directory.
//
// The SOURCE IS NEVER DELETED. Which copy is authoritative is therefore decided
// by the thread's daemon record, not by the filesystem: the account column
// moves only after step 4 returns, so every crash point leaves a usable state —
// before 4 the thread is still on the source and the destination holds at most
// an inert `.port-*` temp (swept by the next port); after 4 both stores hold a
// complete transcript and the daemon record names the winner.
//
// NEWEST WINS on collision: a destination left by an EARLIER port that already
// holds at least as many events is KEPT, never clobbered. That is the same rule
// the external-transcript reconcile uses, in the same function.
// ─────────────────────────────────────────────────────────────────────────

/// The state dir a `CLAUDE_CONFIG_DIR` names — `<home>/.claude` when unset
/// (which is exactly what Claude Code itself does).
pub fn state_dir_for(config_dir: Option<&str>, home: &Path) -> PathBuf {
    match config_dir.filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => home.join(".claude"),
    }
}

/// `<state_dir>/projects` — one directory per cwd slug, each holding one
/// `<session>.jsonl` per session started there.
pub fn projects_dir_for(config_dir: Option<&str>, home: &Path) -> PathBuf {
    state_dir_for(config_dir, home).join("projects")
}

/// The HOME whose `.claude` is the machine-default account's transcript store.
///
/// `THREADS_CLAUDE_HOME` overrides `HOME`. That override exists for exactly one
/// reason: the machine-default account is the operator's OWN login, so every
/// test that exercises porting would otherwise read — and, porting *to*
/// `default`, WRITE — inside `~/.claude`. Tests point this at a tempdir.
pub fn transcript_home() -> Option<PathBuf> {
    std::env::var_os("THREADS_CLAUDE_HOME")
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var_os("HOME"))
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// Find this session's transcript inside one account's store, by SCANNING
/// `<projects>/*/<session>.jsonl`. Returns `None` when the store holds no
/// transcript for the session (including when the store does not exist).
///
/// Scanning rather than re-deriving the cwd slug is deliberate — see the
/// section banner.
///
/// **PL-2d (PL-2c-1): this is the LOSSY form.** A store that exists but cannot
/// be read answers `None` here, exactly as an absent one does. It is kept, with
/// its signature, for the callers that predate the census; anything that must
/// tell "no transcript" from "could not look" calls
/// [`locate_transcript_checked`], which this delegates to.
pub fn locate_transcript(config_dir: Option<&str>, home: &Path, session: &str) -> Option<PathBuf> {
    locate_transcript_checked(config_dir, home, session).unwrap_or(None)
}

/// **PL-2d (PL-2c-1) — [`locate_transcript`] that reports an unreadable store
/// as an ERROR, not as absent.** `Ok(None)` means the store was read and holds
/// no transcript for the session: `<projects>` does not exist, or no slug dir
/// under it holds `<session>.jsonl`. Every other failure to look is `Err`:
/// `<projects>` exists but cannot be listed, an entry cannot be read, or a slug
/// dir cannot be searched (mode 000 makes `is_file()` answer `false`, which is
/// the error-reads-as-absent this replaces). A non-directory entry directly
/// under `<projects>` is not a slug dir and is skipped.
///
/// **PL-2d inspect F-1:** an unreadable entry or slug dir is SKIPPED and its
/// error remembered; a readable hit anywhere else still wins. `Err` is returned
/// only when nothing readable was found AND something could not be read — so
/// one locked sibling never hides a readable transcript (from this, or from
/// the lossy [`locate_transcript`] the pre-census callers use).
pub fn locate_transcript_checked(
    config_dir: Option<&str>,
    home: &Path,
    session: &str,
) -> Result<Option<PathBuf>> {
    let file = format!("{session}.jsonl");
    let projects = projects_dir_for(config_dir, home);
    let unreadable =
        |p: &Path, e: std::io::Error| Error::Config(format!("transcript store unreadable: {}: {e}", p.display()));
    let entries = match std::fs::read_dir(&projects) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(unreadable(&projects, e)),
    };
    let mut found: Option<PathBuf> = None;
    let mut skipped: Option<Error> = None;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                skipped.get_or_insert(unreadable(&projects, e));
                continue;
            }
        };
        let candidate = entry.path().join(&file);
        match std::fs::metadata(&candidate) {
            Ok(md) if md.is_file() => {}
            Ok(_) => continue,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                continue
            }
            Err(e) => {
                skipped.get_or_insert(unreadable(&candidate, e));
                continue;
            }
        }
        // More than one slug dir holding the same session is possible (the
        // thread's cwd changed mid-life). The one with more events is the live
        // conversation; the rest are earlier stubs.
        let better = match &found {
            None => true,
            Some(prev) => {
                transcript_events(&candidate).unwrap_or(0) > transcript_events(prev).unwrap_or(0)
            }
        };
        if better {
            found = Some(candidate);
        }
    }
    match (found, skipped) {
        (Some(hit), _) => Ok(Some(hit)),
        (None, Some(e)) => Err(e),
        (None, None) => Ok(None),
    }
}

/// How many EVENTS a transcript holds — non-blank lines, which is the unit
/// Claude Code appends in. This is the "newest wins" comparator: a transcript
/// that has been advanced further has strictly more lines than the copy it was
/// forked from.
pub fn transcript_events(path: &Path) -> Result<usize> {
    use std::io::BufRead;
    let file = std::fs::File::open(path)
        .map_err(|e| Error::Config(format!("reading {}: {e}", path.display())))?;
    let mut n = 0usize;
    for line in std::io::BufReader::new(file).lines() {
        let line = line.map_err(|e| Error::Config(format!("reading {}: {e}", path.display())))?;
        if !line.trim().is_empty() {
            n += 1;
        }
    }
    Ok(n)
}

/// **PL-2b — [`locate_transcript`] as a census answer.** A thin wrapper and
/// nothing else: the search is `locate_transcript_checked`'s, unchanged, and
/// this only says what it found as `(path, exists)`. `exists` is
/// `path.is_some()`, which is honest because the search already requires
/// `.is_file()` of every candidate it returns. Reads only.
///
/// PL-2d: an unreadable store is `Err`, never `(None, false)` — the census
/// reports it as `transcript.store_error`.
pub fn transcript_presence(
    config_dir: Option<&str>,
    home: &Path,
    session: &str,
) -> Result<(Option<PathBuf>, bool)> {
    let path = locate_transcript_checked(config_dir, home, session)?;
    let exists = path.is_some();
    Ok((path, exists))
}

/// What a port did.
#[derive(Debug, Clone, PartialEq)]
pub enum PortOutcome {
    /// The source store holds no transcript for this session — there is no
    /// conversation to carry. Moving the thread costs nothing.
    NoSource,
    /// The destination already held an equal-or-longer transcript, so it was
    /// KEPT untouched (newest wins). The conversation is already there.
    Kept {
        dest: PathBuf,
        dest_events: usize,
        source_events: usize,
    },
    /// The source was copied, verified and activated. `replaced` names the
    /// event count of the shorter copy the rename superseded, if any.
    Ported {
        dest: PathBuf,
        events: usize,
        replaced: Option<usize>,
    },
}

impl PortOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            PortOutcome::NoSource => "no_source",
            PortOutcome::Kept { .. } => "kept",
            PortOutcome::Ported { .. } => "ported",
        }
    }

    /// The transcript path in the DESTINATION store, when there is one.
    pub fn destination(&self) -> Option<&Path> {
        match self {
            PortOutcome::NoSource => None,
            PortOutcome::Kept { dest, .. } | PortOutcome::Ported { dest, .. } => Some(dest),
        }
    }

    /// Does the destination store now hold this thread's conversation? This is
    /// the question a caller actually has: a thread may RESUME (rather than
    /// restart) on the destination exactly when this is true.
    pub fn holds_conversation(&self) -> bool {
        !matches!(self, PortOutcome::NoSource)
    }

    /// The wire shape. `destination_events` is what the destination holds NOW;
    /// `source_events` is what the copy that lost (or was carried) held;
    /// `replaced` names the shorter copy a port superseded, when there was one.
    pub fn to_json(&self) -> serde_json::Value {
        let (dest_events, source_events, replaced) = match self {
            PortOutcome::NoSource => (None, None, None),
            PortOutcome::Kept {
                dest_events,
                source_events,
                ..
            } => (Some(*dest_events), Some(*source_events), None),
            PortOutcome::Ported {
                events, replaced, ..
            } => (Some(*events), Some(*events), *replaced),
        };
        serde_json::json!({
            "outcome": self.as_str(),
            "destination": self.destination().map(|p| p.to_string_lossy().to_string()),
            "destination_events": dest_events,
            "source_events": source_events,
            "replaced_events": replaced,
        })
    }
}

/// Port a session transcript from one account's store into another's.
///
/// `from` / `to` are `CLAUDE_CONFIG_DIR` values (`None` = the machine default).
/// Never deletes the source; never clobbers a longer destination.
pub fn port_transcript(
    from: Option<&str>,
    to: Option<&str>,
    home: &Path,
    session: &str,
) -> Result<PortOutcome> {
    let Some(source) = locate_transcript(from, home, session) else {
        return Ok(PortOutcome::NoSource);
    };
    port_transcript_file(&source, to, home, session)
}

/// Port an EXPLICITLY NAMED transcript file into an account's store.
///
/// This is the external-transcript reconcile entry point: the AUI fallback runs
/// turns on the machine login while the daemon is down, advancing a copy of the
/// transcript somewhere the daemon did not choose, and hands the path back on
/// recovery.
///
/// The destination slug directory is, in order of preference:
///   1. the directory the destination store ALREADY keeps this session in (the
///      definitive answer — the thread has run there),
///   2. the source file's own parent basename (the slug is a function of the
///      cwd, which is the same on both sides).
pub fn port_transcript_file(
    source: &Path,
    to: Option<&str>,
    home: &Path,
    session: &str,
) -> Result<PortOutcome> {
    off_the_reactor(|| port_transcript_file_blocking(source, to, home, session))
}

/// **RUN A LONG SYNCHRONOUS FILE COPY WITHOUT PARKING A TOKIO WORKER.**
///
/// The port is `std::fs` from end to end and the volumes are not small: this
/// daemon's log records 61 sidecar carries totalling 2.10 GB, the largest a
/// single 207,942,818-byte carry across 457 files, and a 159 MB one landed
/// inside the 2026-09-11 stall window. Every one of those ran directly on one
/// of ten runtime workers — 10% of the daemon's capacity to answer anything,
/// for as long as the copy took (`reports/0093-stability-threads.md`, F4).
///
/// **Why `block_in_place` and not `spawn_blocking`.** `spawn_blocking` is the
/// right tool at an ASYNC call site and is what the `/usage` shell-out uses
/// (`runtime.rs`, twice). It is not reachable here: the account-switch path
/// arrives through a fully SYNCHRONOUS stack — `admit_or_park` →
/// `move_thread` → `port_conversation` → here — whose five callers include
/// sync ones, so reaching an `.await` would mean making the hottest function
/// in the admission path async and cascading that through all of them. That is
/// a far larger blast radius than the defect. `block_in_place` is tokio's
/// primitive for precisely this case: it hands the current worker's queue to a
/// replacement thread for the duration, so the runtime keeps its full worker
/// count while this thread blocks.
///
/// **It lives at the CHOKE POINT, not at the call sites**, so that the two
/// known paths (the `threads.reconcile_transcript` verb and the account
/// switch) and any third one added later are all covered by construction —
/// a copy that forgot to offload itself is exactly the defect being fixed.
///
/// Degrades to a plain call when there is no multi-thread runtime to protect:
/// a CLI verb, a unit test, or a `#[tokio::test]` on the default
/// current-thread flavour, where `block_in_place` would panic.
fn off_the_reactor<R>(f: impl FnOnce() -> R) -> R {
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

fn port_transcript_file_blocking(
    source: &Path,
    to: Option<&str>,
    home: &Path,
    session: &str,
) -> Result<PortOutcome> {
    if !source.is_file() {
        return Ok(PortOutcome::NoSource);
    }
    let dest = match locate_transcript(to, home, session) {
        Some(resident) => resident,
        None => {
            let slug = source
                .parent()
                .and_then(Path::file_name)
                .ok_or_else(|| {
                    Error::Config(format!(
                        "{} is not inside a projects/<cwd-slug>/ directory, so its \
                         destination slug cannot be determined",
                        source.display()
                    ))
                })?
                .to_owned();
            projects_dir_for(to, home)
                .join(slug)
                .join(format!("{session}.jsonl"))
        }
    };
    let outcome = port_into(source, &dest)?;
    // THE SIDECAR MOVES WITH THE CONVERSATION. Deliberately after `port_into`
    // and outside its result: a transcript that arrived is worth more than a
    // sidecar that did not, so a sidecar failure is logged and swallowed, never
    // propagated. Run on the `Kept` path too — a destination that already holds
    // the current transcript can still be missing every subagent file, which is
    // exactly the state a re-port would otherwise leave in place.
    if !matches!(outcome, PortOutcome::NoSource) {
        match port_sidecar(source, &dest) {
            Ok((files, bytes)) if files > 0 => tracing::info!(
                session, files, bytes,
                "carried the session sidecar (subagents/, tool-results/) to the target account"
            ),
            Ok(_) => {}
            Err(e) => tracing::warn!(
                session, error = %e,
                "the transcript ported but its sidecar did not — subagent transcripts and \
                 externalised tool results are missing on the target account, so that content \
                 has nowhere to live but the main context"
            ),
        }
    }
    Ok(outcome)
}

/// **Carry `<session>/` — the sidecar beside `<session>.jsonl`.**
///
/// Claude Code keeps a session's SUBAGENT transcripts (`subagents/`) and its
/// externalised oversized tool outputs (`tool-results/`) in a directory named
/// for the session, as a sibling of the transcript file. The main transcript
/// only REFERENCES them.
///
/// [`port_into`] moves one file, so for a long time this directory did not move
/// at all. On 2026-08-06 that emptied four threads' sidecars on failover — 148
/// subagent transcripts and 24 tool-results on the busiest one — and the
/// content they held had nowhere to go but the main context, which pushed three
/// threads over the auto-compaction line within twenty seconds of each other.
/// A coordinator-pattern thread keeps its main context small precisely BY
/// holding that material out of line, so it is the threads that are most
/// carefully built that this hurt most.
///
/// ADDITIVE AND NON-DESTRUCTIVE. Files are copied when the destination lacks
/// them, or when the source copy is strictly longer (these logs are append-only,
/// so longer is newer). Nothing at the destination is ever deleted, and a
/// destination file that is already equal or longer is left alone — the same
/// newest-wins rule [`port_into`] applies to the transcript itself.
///
/// Returns `(files_copied, bytes_copied)`.
fn port_sidecar(source: &Path, dest: &Path) -> Result<(usize, u64)> {
    let (Some(src_parent), Some(dst_parent)) = (source.parent(), dest.parent()) else {
        return Ok((0, 0));
    };
    let src_dir = src_parent.join(stem_of(source));
    let dst_dir = dst_parent.join(stem_of(dest));
    if !src_dir.is_dir() || same_file(&src_dir, &dst_dir) {
        return Ok((0, 0));
    }
    copy_tree_additive(&src_dir, &dst_dir)
}

/// Recursive, additive file copy: create missing directories, copy a file when
/// the destination is absent or shorter. Never removes anything.
fn copy_tree_additive(src: &Path, dst: &Path) -> Result<(usize, u64)> {
    std::fs::create_dir_all(dst)
        .map_err(|e| Error::Config(format!("creating {}: {e}", dst.display())))?;
    let mut files = 0usize;
    let mut bytes = 0u64;
    let entries = std::fs::read_dir(src)
        .map_err(|e| Error::Config(format!("reading {}: {e}", src.display())))?;
    for entry in entries.flatten() {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            let (f, b) = copy_tree_additive(&from, &to)?;
            files += f;
            bytes += b;
        } else if meta.is_file() {
            let resident = std::fs::metadata(&to).ok().map(|m| m.len());
            if resident.is_some_and(|len| len >= meta.len()) {
                continue;
            }
            if std::fs::copy(&from, &to).is_ok() {
                files += 1;
                bytes += meta.len();
            }
        }
    }
    Ok((files, bytes))
}

/// The port itself: copy, verify, activate. See the section banner for the
/// crash-safety argument.
fn port_into(source: &Path, dest: &Path) -> Result<PortOutcome> {
    let source_events = transcript_events(source)?;

    // The same file on both sides (the destination store already IS the source
    // store) — nothing to do, and copying a file over itself would truncate it.
    if same_file(source, dest) {
        return Ok(PortOutcome::Kept {
            dest: dest.to_path_buf(),
            dest_events: source_events,
            source_events,
        });
    }

    // NEWEST WINS. A stale copy left by an earlier port loses; an equal-or-
    // longer one is authoritative and is never clobbered.
    let resident = dest.is_file().then(|| transcript_events(dest).unwrap_or(0));
    if let Some(dest_events) = resident {
        if dest_events >= source_events {
            return Ok(PortOutcome::Kept {
                dest: dest.to_path_buf(),
                dest_events,
                source_events,
            });
        }
    }

    let dir = dest
        .parent()
        .ok_or_else(|| Error::Config(format!("{} has no parent directory", dest.display())))?;
    std::fs::create_dir_all(dir)
        .map_err(|e| Error::Config(format!("creating {}: {e}", dir.display())))?;

    // Sweep temps a crashed earlier port left behind. They are inert (not
    // `.jsonl`, dot-prefixed) but they are litter, and sweeping here rather
    // than on a timer means the cleanup happens exactly when a port is proven
    // to be running again.
    let temp_prefix = format!(".{}.jsonl.port-", stem_of(dest));
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with(&temp_prefix) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    let temp = dir.join(format!(
        "{temp_prefix}{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));

    // 2. copy + fsync.
    let copy = (|| -> std::io::Result<()> {
        let mut r = std::io::BufReader::new(std::fs::File::open(source)?);
        let mut w = std::fs::File::create(&temp)?;
        std::io::copy(&mut r, &mut w)?;
        w.sync_all()
    })();
    if let Err(e) = copy {
        let _ = std::fs::remove_file(&temp);
        return Err(Error::Config(format!(
            "copying {} to {}: {e}",
            source.display(),
            temp.display()
        )));
    }

    // 3. VERIFY. The copy must be a consistent snapshot of the source: at least
    // what the source held before it started, at most what it holds now. In the
    // quiescent case (nothing writes to a transcript between turns) both bounds
    // are the same number and this is plain equality.
    let verify = (|| -> Result<usize> {
        let copied = transcript_events(&temp)?;
        let after = transcript_events(source)?;
        if copied < source_events || copied > after {
            return Err(Error::Config(format!(
                "verify failed porting {}: copied {copied} events, source held {source_events} \
                 before and {after} after — the destination was NOT activated and the source \
                 is untouched",
                source.display()
            )));
        }
        Ok(copied)
    })();
    let copied = match verify {
        Ok(n) => n,
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            return Err(e);
        }
    };

    // 4. ACTIVATE — one atomic rename within one directory.
    std::fs::rename(&temp, dest).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        Error::Config(format!(
            "activating {} (the source is untouched): {e}",
            dest.display()
        ))
    })?;

    // 5. fsync the directory so the rename itself is durable.
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }

    Ok(PortOutcome::Ported {
        dest: dest.to_path_buf(),
        events: copied,
        replaced: resident,
    })
}

/// `<session>` out of `<session>.jsonl` (the temp-file prefix).
fn stem_of(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "transcript".to_string())
}

/// Are these two paths the same file? Compared by canonical path — enough for
/// the one case that matters (an account registered twice under two labels
/// pointing at one config dir).
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Account capture — snapshot the CURRENTLY authenticated claude config into
// `<data_dir>/accounts/<name>/` so it can be spawned as an isolated account.
//
// How Claude Code stores auth on macOS (verified against binary v2.1.220):
//
// - Config/identity: `.claude.json` — at `~/.claude.json` by default, or at
//   `$CLAUDE_CONFIG_DIR/.claude.json` when the env var is set. Holds the
//   `oauthAccount` identity block (email, org, tier) but NO tokens in
//   keychain mode.
// - Tokens (keychain mode, the default on macOS): a login-keychain generic
//   password. Service name is `Claude Code-credentials` for the default
//   config dir, and `Claude Code-credentials-<first 8 hex of sha256(dir)>`
//   when `CLAUDE_CONFIG_DIR` is set (dir NFC-normalized, hashed verbatim —
//   same string, same separators, no trailing slash). Account = `$USER`.
// - Tokens (plaintext fallback, non-mac / no keychain): `.credentials.json`
//   inside the config dir.
//
// Because the keychain service name is derived from the config dir, two
// config dirs are FULLY isolated — including credentials. Capture therefore:
//   1. copies `.claude.json` (+ `settings.json`) into the snapshot dir,
//   2. clones the keychain secret into a new entry keyed to the snapshot dir
//      (via `security`; the secret never touches argv, disk, or logs), or
//      copies `.credentials.json` if the install is in plaintext mode,
//   3. writes a non-secret `manifest.toml`, and registers the account so a
//      spawn with `CLAUDE_CONFIG_DIR=<snapshot dir>` runs as that login.
// ─────────────────────────────────────────────────────────────────────────

/// Base keychain service name used by Claude Code on macOS.
pub const KEYCHAIN_SERVICE_BASE: &str = "Claude Code-credentials";

/// The keychain service name Claude Code uses for a given `CLAUDE_CONFIG_DIR`
/// (`None` = env unset = machine default).
///
/// NOTE: Claude Code NFC-normalizes the dir string before hashing; we hash the
/// bytes verbatim, which is identical for ASCII paths (all paths we generate).
/// Keep snapshot dirs ASCII.
pub fn keychain_service_for(config_dir: Option<&str>) -> String {
    match config_dir {
        None => KEYCHAIN_SERVICE_BASE.to_string(),
        Some(d) => {
            use sha2::{Digest, Sha256};
            let hash = Sha256::digest(d.as_bytes());
            let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
            format!("{KEYCHAIN_SERVICE_BASE}-{}", &hex[..8])
        }
    }
}

/// The keychain account name Claude Code uses (`$USER`, with its fallback).
pub fn keychain_account() -> String {
    std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "claude-code-user".to_string())
}

/// Where the currently authenticated claude config lives (resolved from the
/// capturing process's env, mirroring Claude Code's own resolution).
#[derive(Debug, Clone)]
pub struct CaptureSource {
    /// `CLAUDE_CONFIG_DIR` if set and non-empty.
    pub config_dir: Option<PathBuf>,
    /// `$HOME`.
    pub home: PathBuf,
}

impl CaptureSource {
    pub fn from_env() -> Result<Self> {
        let home = std::env::var("HOME")
            .map_err(|_| Error::Config("HOME is not set".into()))?;
        let config_dir = std::env::var("CLAUDE_CONFIG_DIR")
            .ok()
            .filter(|d| !d.is_empty())
            .map(PathBuf::from);
        Ok(Self {
            config_dir,
            home: PathBuf::from(home),
        })
    }

    /// The state dir (`~/.claude` or `$CLAUDE_CONFIG_DIR`).
    pub fn state_dir(&self) -> PathBuf {
        self.config_dir
            .clone()
            .unwrap_or_else(|| self.home.join(".claude"))
    }

    /// The config file (`~/.claude.json` or `$CLAUDE_CONFIG_DIR/.claude.json`).
    pub fn config_json(&self) -> PathBuf {
        match &self.config_dir {
            Some(d) => d.join(".claude.json"),
            None => self.home.join(".claude.json"),
        }
    }

    /// The keychain service name for this source.
    pub fn keychain_service(&self) -> String {
        keychain_service_for(self.config_dir.as_deref().and_then(|p| p.to_str()))
    }
}

/// Whether capture may touch the macOS keychain.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum KeychainMode {
    /// Clone the keychain entry when the install is keychain-based (macOS).
    Auto,
    /// Never invoke `security` (tests, non-interactive contexts).
    Skip,
}

/// Whether the caller has EXPLICITLY asked to clone the live login's stored
/// session into a new slot.
///
/// THE GUARD ON THE CLONE VERB (F-1). [`capture_snapshot`] copies a stored
/// credential — the keychain secret on macOS, `.credentials.json` in plaintext
/// mode — into a second store. That is not a second login: it is ONE OAuth
/// session in two places. Claude Code rotates the refresh token on use and
/// reconciles only WITHIN one credential store, so the copy that refreshes
/// second is handed a retired token and self-wipes. Measured in
/// `account-visibility-report`; it is what killed the `primary` slot.
///
/// The mechanism is kept because SNAPSHOTTING is a real, wanted capability. It
/// is simply no longer reachable by accident: every credential-copying path
/// requires [`CloneConsent::CloneLiveSession`], which only the explicit
/// `--clone-session` flag produces. Everything else gets a refusal that names
/// the durable alternative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneConsent {
    /// No consent. A capture that would copy a stored session is REFUSED
    /// before anything is read, written, or created.
    Refuse,
    /// `--clone-session`: the caller accepts a shared-session snapshot.
    CloneLiveSession,
}

/// The verbatim refusal every un-consented capture returns. Names what a clone
/// actually is, and both durable alternatives.
pub const CLONE_REFUSAL: &str = "REFUSED: this would CLONE your live login's stored session, not create a second account.

  A cloned session is ONE OAuth session in two stores, sharing one refresh
  token. Claude rotates that token on use and reconciles only WITHIN a single
  store, so whichever side refreshes second is handed a token the server has
  already retired — and that side is logged out for good. This is what killed
  the `primary` slot.

  For a durable second account, log in for real:
      scripts/claude-accounts              (interactive; `n` = new login)
      scripts/add-account --login <name>

  To register a slot that already exists on disk:
      threads accounts adopt <name>

  If you really do want a shared-session SNAPSHOT, and you accept that a token
  rotation on either side strands the other, pass --clone-session.";

/// How the snapshot's credentials ended up.
#[derive(Debug, Clone, PartialEq)]
pub enum CredentialOutcome {
    /// `.credentials.json` (plaintext mode) was copied into the snapshot dir.
    PlaintextCopied,
    /// The keychain secret was cloned into a new entry with `service`.
    KeychainCloned { service: String },
    /// Credentials could NOT be copied; a one-time login under the snapshot
    /// dir is required (`CLAUDE_CONFIG_DIR=<dir> claude` then `/login`).
    ManualLoginRequired { service: String, reason: String },
}

impl CredentialOutcome {
    pub fn mode_str(&self) -> &'static str {
        match self {
            Self::PlaintextCopied => "plaintext-file",
            Self::KeychainCloned { .. } => "keychain-cloned",
            Self::ManualLoginRequired { .. } => "manual-login-required",
        }
    }
}

/// Result of a capture.
#[derive(Debug)]
pub struct SnapshotOutcome {
    /// The snapshot dir — the account's future `CLAUDE_CONFIG_DIR`.
    pub dir: PathBuf,
    /// Files copied into the dir (relative names).
    pub copied: Vec<String>,
    pub credentials: CredentialOutcome,
}

/// Validate an account/label name (`[A-Za-z0-9_-]`, max 64 chars). `pub` so
/// callers adopting a pre-existing snapshot (not going through
/// [`capture_snapshot`]) can validate before touching the registry.
/// Directory names that live under `accounts/` but are NOT accounts.
///
/// `backups/` holds the live-login undo snapshots that `claude-accounts
/// set-live-login` takes before it touches the machine store
/// (`accounts/backups/live-login/<stamp>/`). It has never been an account, it is
/// not in the pool, and it has no credential — but it is a plain directory with
/// a perfectly legal name, so every enumerator that filtered on "is it a
/// directory with a valid name?" listed it as a slot with no login and told the
/// operator to sign it in.
///
/// Naming it here makes it STRUCTURALLY not-a-slot rather than something each
/// listing has to remember to skip: [`validate_account_name`] rejects it, so it
/// cannot be created, adopted, renamed to, logged into, or enumerated as an
/// account anywhere that validates — which is every path that matters.
pub const RESERVED_SLOT_DIRS: &[&str] = &["backups"];

/// Is this directory name reserved for something that is not an account?
pub fn is_reserved_slot_dir(name: &str) -> bool {
    RESERVED_SLOT_DIRS.contains(&name)
}

pub fn validate_account_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        return Err(Error::Config(format!(
            "invalid account name {name:?}: use [A-Za-z0-9_-], max 64 chars"
        )));
    }
    if is_reserved_slot_dir(name) {
        return Err(Error::Config(format!(
            "{name:?} is not an account — `accounts/{name}/` holds the live-login undo \
             snapshots, and the name is reserved for them"
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}
#[cfg(not(unix))]
fn set_mode(_path: &std::path::Path, _mode: u32) {}

fn copy_private(src: &std::path::Path, dest: &std::path::Path) -> Result<()> {
    std::fs::copy(src, dest).map_err(|e| {
        Error::Config(format!("copying {} → {}: {e}", src.display(), dest.display()))
    })?;
    set_mode(dest, 0o600);
    Ok(())
}

/// Clone the keychain secret from `src_service` to `dest_service` for
/// `account`. The secret stays in process memory and the `security` stdin
/// pipe — it is never printed, logged, or written to disk.
#[cfg(target_os = "macos")]
fn clone_keychain_entry(
    src_service: &str,
    dest_service: &str,
    account: &str,
) -> std::result::Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    if keychain_override_installed() {
        return Ok(()); // R9.F2h: the fixture holds no real item.
    }
    real_keychain_tripwire(src_service);

    // Read the current secret. May pop a GUI keychain prompt on some setups;
    // Claude Code writes items via /usr/bin/security, which normally grants
    // /usr/bin/security silent read access.
    let out = Command::new("/usr/bin/security")
        .args([
            "find-generic-password",
            "-a",
            account,
            "-s",
            src_service,
            "-w",
        ])
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("running security: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "keychain item {src_service:?} not readable (status {})",
            out.status
        ));
    }
    let mut secret = out.stdout;
    // `security -w` appends exactly one newline.
    if secret.last() == Some(&b'\n') {
        secret.pop();
    }
    if secret.is_empty() {
        return Err(format!("keychain item {src_service:?} is empty"));
    }

    // Write via `security -i` (stdin command stream) with hex data (-X):
    // no secret bytes on argv, no quoting pitfalls.
    let hex: String = secret.iter().map(|b| format!("{b:02X}")).collect();
    let cmd = format!(
        "add-generic-password -U -a \"{account}\" -s \"{dest_service}\" -X {hex}\n"
    );
    let mut child = Command::new("/usr/bin/security")
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("running security -i: {e}"))?;
    child
        .stdin
        .as_mut()
        .expect("piped stdin")
        .write_all(cmd.as_bytes())
        .map_err(|e| format!("writing to security -i: {e}"))?;
    drop(child.stdin.take());
    let status = child
        .wait()
        .map_err(|e| format!("waiting for security -i: {e}"))?;
    if !status.success() {
        return Err(format!("security add-generic-password failed ({status})"));
    }

    // Verify (metadata only — no secret retrieval).
    let verify = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-a", account, "-s", dest_service])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("verifying: {e}"))?;
    if !verify.success() {
        return Err("cloned keychain item not found on verify".into());
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn clone_keychain_entry(
    _src_service: &str,
    _dest_service: &str,
    _account: &str,
) -> std::result::Result<(), String> {
    Err("keychain cloning is macOS-only".into())
}

/// Snapshot the currently authenticated claude config into
/// `<data_dir>/accounts/<name>/`.
///
/// The snapshot dir doubles as the account's `CLAUDE_CONFIG_DIR`; everything
/// under `<data_dir>/accounts/` is force-gitignored via a `*` `.gitignore`
/// written alongside the snapshots (credentials must never reach a repo).
///
/// GUARDED. If this call would copy a stored credential (the macOS keychain
/// secret, or a plaintext `.credentials.json`) it REFUSES unless `consent` is
/// [`CloneConsent::CloneLiveSession`] — see that type for why. The refusal
/// happens before any directory is created, so a refused capture leaves
/// nothing behind, and before `security` is invoked, so a refused capture
/// never reads the operator's keychain.
pub fn capture_snapshot(
    source: &CaptureSource,
    data_dir: &std::path::Path,
    name: &str,
    keychain: KeychainMode,
    consent: CloneConsent,
) -> Result<SnapshotOutcome> {
    validate_account_name(name)?;

    let src_config_json = source.config_json();
    if !src_config_json.exists() {
        return Err(Error::Config(format!(
            "no authenticated claude config found at {} — log in with `claude` first",
            src_config_json.display()
        )));
    }

    // THE GUARD (F-1). Decided BEFORE anything is created or read: would this
    // capture copy a stored session? Both credential paths count — a copied
    // `.credentials.json` shares one refresh token exactly as a cloned
    // keychain item does.
    let would_copy_credential = source.state_dir().join(".credentials.json").exists()
        || (keychain == KeychainMode::Auto && cfg!(target_os = "macos"));
    if would_copy_credential && consent != CloneConsent::CloneLiveSession {
        return Err(Error::Config(CLONE_REFUSAL.to_string()));
    }

    let accounts_root = data_dir.join("accounts");
    let dest = accounts_root.join(name);
    // Refuse to clobber an existing capture (folded in from the bash
    // prototype's same guard, IS-18 reconciliation): re-running `add` over an
    // already-captured account would silently overwrite its `.claude.json`
    // and re-clone the keychain entry with whatever is CURRENTLY logged in,
    // which is almost never what's wanted. Use `adopt` to register an
    // existing snapshot, or remove the directory first to recapture.
    if dest.join("manifest.toml").exists() {
        return Err(Error::Config(format!(
            "account {name:?} is already captured at {} — remove it first, \
             or use `threads accounts adopt {name}` to just (re-)register it",
            dest.display()
        )));
    }
    std::fs::create_dir_all(&dest)
        .map_err(|e| Error::Config(format!("creating {}: {e}", dest.display())))?;
    set_mode(&accounts_root, 0o700);
    set_mode(&dest, 0o700);
    // Belt-and-braces: whatever repo this state dir lands in, snapshots
    // (which can contain plaintext credentials) are ignored.
    let gitignore = accounts_root.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(&gitignore, "*\n")
            .map_err(|e| Error::Config(format!("writing {}: {e}", gitignore.display())))?;
    }

    let mut copied = Vec::new();
    copy_private(&src_config_json, &dest.join(".claude.json"))?;
    copied.push(".claude.json".to_string());

    let src_state = source.state_dir();
    let src_settings = src_state.join("settings.json");
    if src_settings.exists() {
        copy_private(&src_settings, &dest.join("settings.json"))?;
        copied.push("settings.json".to_string());
    }

    let dest_str = dest.to_string_lossy().to_string();
    let dest_service = keychain_service_for(Some(&dest_str));
    let src_creds = src_state.join(".credentials.json");
    let credentials = if src_creds.exists() {
        // Plaintext mode: the token file rides along in the config dir.
        copy_private(&src_creds, &dest.join(".credentials.json"))?;
        copied.push(".credentials.json".to_string());
        CredentialOutcome::PlaintextCopied
    } else if keychain == KeychainMode::Auto && cfg!(target_os = "macos") {
        let src_service = source.keychain_service();
        match clone_keychain_entry(&src_service, &dest_service, &keychain_account()) {
            Ok(()) => CredentialOutcome::KeychainCloned {
                service: dest_service.clone(),
            },
            Err(reason) => CredentialOutcome::ManualLoginRequired {
                service: dest_service.clone(),
                reason,
            },
        }
    } else {
        CredentialOutcome::ManualLoginRequired {
            service: dest_service.clone(),
            reason: "keychain access skipped".into(),
        }
    };

    write_manifest(&dest, name, source, &dest_str, &credentials)?;

    Ok(SnapshotOutcome {
        dir: dest,
        copied,
        credentials,
    })
}

/// Non-secret snapshot manifest, `<dir>/manifest.toml`.
///
/// Field name `label` (not `name`) is deliberate: this is the wire format the
/// bash `add-account` prototype used before the IS-18 reconciliation, and two
/// already-captured accounts (`primary`, `secondary`) have `manifest.toml`
/// files on disk with `label = "..."` — matching here means both existing
/// snapshots parse with zero rewriting.
#[derive(Debug, Serialize, Deserialize)]
pub struct SnapshotManifest {
    pub label: String,
    pub captured_at: String,
    /// Where the config was captured FROM (paths only, no secrets).
    pub source_config_dir: String,
    pub source_config_json: String,
    /// The env this account's spawns require.
    pub spawn_env: String,
    /// `keychain-cloned` | `plaintext-file` | `manual-login-required`.
    pub credential_mode: String,
    /// The keychain service the spawned claude will look up (macOS).
    pub keychain_service: String,
    pub keychain_account: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn write_manifest(
    dest: &std::path::Path,
    name: &str,
    source: &CaptureSource,
    dest_str: &str,
    credentials: &CredentialOutcome,
) -> Result<()> {
    let note = match credentials {
        CredentialOutcome::ManualLoginRequired { reason, .. } => Some(format!(
            "credentials NOT copied ({reason}); run `CLAUDE_CONFIG_DIR={dest_str} claude` \
             and complete /login once — tokens then persist under this dir's keychain \
             service (or its .credentials.json) independently of every other account"
        )),
        CredentialOutcome::KeychainCloned { .. } => Some(
            "SHARED SESSION, not an independent login: this slot's keychain item is a COPY \
             of the live login's, so both hold the SAME refresh token. Claude rotates that \
             token on use and reconciles only within one store — whichever side refreshes \
             second is stranded and must log in again. Tokens live in the login keychain \
             under `keychain_service`, not in this dir, so deleting this dir does not \
             delete them (security delete-generic-password)"
                .into(),
        ),
        CredentialOutcome::PlaintextCopied => Some(
            "SHARED SESSION, not an independent login: this slot's .credentials.json is a \
             COPY of the live login's, so both hold the SAME refresh token. Whichever side \
             refreshes it second is stranded and must log in again"
                .into(),
        ),
    };
    let manifest = SnapshotManifest {
        label: name.to_string(),
        captured_at: chrono::Utc::now().to_rfc3339(),
        source_config_dir: source.state_dir().to_string_lossy().to_string(),
        source_config_json: source.config_json().to_string_lossy().to_string(),
        spawn_env: format!("CLAUDE_CONFIG_DIR={dest_str}"),
        credential_mode: credentials.mode_str().to_string(),
        keychain_service: keychain_service_for(Some(dest_str)),
        keychain_account: keychain_account(),
        note,
    };
    let toml = toml::to_string_pretty(&manifest)
        .map_err(|e| Error::Config(format!("rendering manifest.toml: {e}")))?;
    let path = dest.join("manifest.toml");
    std::fs::write(&path, toml)
        .map_err(|e| Error::Config(format!("writing {}: {e}", path.display())))?;
    set_mode(&path, 0o600);
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// Per-account ACTIVITY (operator ask, msg #690: "we should have some
// information in the threads thing to see like how many threads are active on
// each account ... that makes it easier for you to test without getting in the
// way of me actually using the different accounts").
//
// The purpose is COORDINATION, not accounting: before an agent starts work on
// an account it must be able to see whether the operator (or another agent) is
// live on it. So this is a cheap, read-only roll-up of state that is ALREADY
// durable — it never polls `/usage`, never invokes `claude`, and never touches
// the keychain. Gauge readings are passed IN by the caller when they happen to
// be known (`Budgets::gauge_state`, itself a pure table read); a status query
// must never cost a poll.
// ─────────────────────────────────────────────────────────────────────────

/// The bucket a thread lands in when its account is not in the registry (a
/// stale `threads.account`, or a snapshot that was un-registered under it).
pub const UNATTRIBUTED: &str = "(unattributed)";

/// What ONE thread contributes to the per-account roll-up.
///
/// Deliberately a plain struct of already-resolved facts: the aggregation
/// ([`activity_by_account`]) is then a pure function that fixtures can drive
/// without a database.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadActivity {
    pub thread_id: String,
    /// The account this thread's work is attributed to — see
    /// [`attribute_account`] for the resolution order and its caveat.
    pub account: String,
    /// A `running_turns` row exists: a claude child is live for this thread.
    ///
    /// This is the ONE activity fact here that is not read through
    /// [`crate::runtime::ThreadManager::thread_states`], and it is deliberate:
    /// `collect_thread_activity` is also the CLI's path (`threads accounts
    /// status`), which has no daemon and therefore no manager to ask, and the
    /// `running_turns` row is written and cleared in the same turn epilogue as
    /// the `thread_locks` row the derivation reads. If the two are ever seen to
    /// disagree, THAT is the defect to fix — by threading a manager in here, not
    /// by adding a third opinion.
    pub executing: bool,
    /// A retry is armed for this thread (`retries` row present).
    ///
    /// Derived from the `retries` table, which is where `thread_states` reads it
    /// too. It used to be `status == "retrying"` off the cached `threads.status`
    /// column, and that column was the reason `accounts.status`'s `idle` count
    /// was silently wrong: a thread stuck at a stale `busy` counted as neither
    /// retrying nor idle, so the pool looked emptier than it was.
    pub retrying: bool,
    /// The park reason when the thread is sitting on a parked turn (a budget
    /// trip or an admission gate), else `None`.
    pub park_reason: Option<String>,
    /// Waiting (undelivered, uncancelled) queue rows for this thread.
    pub queued_messages: usize,
}

impl ThreadActivity {
    /// Neither executing, nor parked, nor holding queued work.
    pub fn quiet(&self) -> bool {
        !self.executing && self.park_reason.is_none() && self.queued_messages == 0
    }
}

/// The per-account roll-up. One per registered account (so an idle account
/// still reports, which is the answer an agent actually needs), plus a
/// [`UNATTRIBUTED`] bucket only when something lands in it.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountActivity {
    pub account: String,
    pub config_dir: Option<String>,
    /// False only for the [`UNATTRIBUTED`] bucket.
    pub registered: bool,
    /// Threads with a live claude child right now. THIS is the number that
    /// answers "is someone on this account".
    pub executing: usize,
    pub parked: usize,
    /// Park reason → count, so "how many are parked" comes with "and why".
    pub park_reasons: BTreeMap<String, usize>,
    /// Threads holding at least one waiting queue row.
    pub queued_threads: usize,
    /// Total waiting queue rows across those threads.
    pub queued_messages: usize,
    /// Threads with a retry armed (a `retries` row), derived — not read off a
    /// cached status word.
    pub retrying: usize,
    /// Threads that are none of the above.
    pub idle: usize,
    /// Every thread attributed to this account, whatever its state.
    pub threads: usize,
    pub executing_thread_ids: Vec<String>,
    pub parked_thread_ids: Vec<String>,
    /// From the tracked `accounts` table: is this account presumed to have
    /// usage available right now?
    pub available: bool,
    pub limit_until: Option<String>,
    pub last_limit_prose: Option<String>,
    /// WHICH quota or condition put [`Self::limit_until`] there — a `gauge::Axis`
    /// (`session` | `weekly_all` | `weekly_fable`), or one of the two non-quota
    /// axes: `credit` (wave one) and `login` (an expired credential).
    ///
    /// It is on this row rather than only in the database because a bar is
    /// rendered to the operator as a time window, and for the two non-quota axes
    /// a bare window is actively misleading — it says "wait" when the truth is
    /// "act". `login` in particular means the window will keep being re-armed
    /// until he re-authenticates, and nothing else about the row can say so.
    pub limit_axis: Option<String>,
    /// Does this login still authenticate? An unhealthy account is excluded
    /// from selection and failover but stays fully visible HERE — the operator
    /// has to be able to see that an account is dead and why.
    pub healthy: bool,
    pub dead_since: Option<String>,
    pub dead_reason: Option<String>,
    /// The opaque identity key ([`identity_fingerprint`]) of the login this
    /// label points at, or `None` when it could not be read.
    pub identity: Option<String>,
    /// When two or more registry labels point at the SAME login, the label
    /// selection uses for it. Equal to `account` for the canonical label, and
    /// `None` when this label is the only one for its login. A non-`None` value
    /// that differs from `account` means this row is an ALIAS and must not be
    /// counted as a second account.
    pub selection_unit: Option<String>,
    /// The account's ALREADY-KNOWN gauge axes (the `axes` object of one
    /// `gauge.status` row), or `None` when the caller had no readings to hand.
    /// Never triggers a poll.
    pub gauge_axes: Option<serde_json::Value>,
}

impl AccountActivity {
    fn empty(account: &str, config_dir: Option<String>, registered: bool) -> Self {
        Self {
            account: account.to_string(),
            config_dir,
            registered,
            executing: 0,
            parked: 0,
            park_reasons: BTreeMap::new(),
            queued_threads: 0,
            queued_messages: 0,
            retrying: 0,
            idle: 0,
            threads: 0,
            executing_thread_ids: Vec::new(),
            parked_thread_ids: Vec::new(),
            available: true,
            limit_until: None,
            last_limit_prose: None,
            limit_axis: None,
            healthy: true,
            dead_since: None,
            dead_reason: None,
            identity: None,
            selection_unit: None,
            gauge_axes: None,
        }
    }

    /// Is anything at all happening on this account?
    pub fn busy(&self) -> bool {
        self.executing > 0 || self.parked > 0 || self.queued_messages > 0 || self.retrying > 0
    }

    /// The wire shape — used verbatim by both surfaces (the `accounts.status`
    /// WS method and `threads accounts status --json`), so the two can never
    /// drift.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "account": self.account,
            "config_dir": self.config_dir,
            "registered": self.registered,
            "busy": self.busy(),
            "executing": self.executing,
            "parked": self.parked,
            "park_reasons": self.park_reasons,
            "queued_threads": self.queued_threads,
            "queued_messages": self.queued_messages,
            "retrying": self.retrying,
            "idle": self.idle,
            "threads": self.threads,
            "executing_thread_ids": self.executing_thread_ids,
            "parked_thread_ids": self.parked_thread_ids,
            "available": self.available,
            "limit_until": self.limit_until,
            "last_limit_prose": self.last_limit_prose,
            "limit_axis": self.limit_axis,
            "healthy": self.healthy,
            "dead_since": self.dead_since,
            "dead_reason": self.dead_reason,
            "identity": self.identity,
            "selection_unit": self.selection_unit,
            "gauge": self.gauge_axes,
        })
    }
}

/// Which account is a thread's work attributed to?
///
/// Resolution order, most to least authoritative:
///
/// 1. `live` — the daemon's in-memory serving account for the thread
///    (`ThreadState.account`).
/// 2. `row_account` — the `threads.account` column, the DURABLE mirror of (1).
/// 3. The freshest `thread_accounts` row (`history` is most-recent-first) —
///    where the thread last COMPLETED a turn.
///
/// History used to outrank the column, because `switch_account` wrote the
/// column at CREATE time and never again (IS-A), which made it wrong for every
/// thread that had failed over. `ThreadManager::persist_thread_account` now
/// writes it on every switch and on every rehydration that re-resolves the
/// account, so the column tracks `ThreadState.account` and is strictly better
/// than history — which is *not* the same question: history is only appended
/// when a turn ENDS, so it still names the previous account for exactly the
/// case that matters (a thread that has just failed over and is executing on
/// its new one).
///
/// History stays as the tier-3 fallback rather than being deleted: the column
/// write is best-effort (a store error must never abort a switch), and a
/// database written before the IS-A fix can still hold a stale column with
/// usable history behind it.
pub fn attribute_account(
    history: &[String],
    row_account: Option<&str>,
    live: Option<&str>,
) -> String {
    if let Some(a) = live.filter(|a| !a.is_empty()) {
        return a.to_string();
    }
    if let Some(a) = row_account.filter(|a| !a.is_empty()) {
        return a.to_string();
    }
    match history.first().filter(|a| !a.is_empty()) {
        Some(a) => a.clone(),
        None => UNATTRIBUTED.to_string(),
    }
}

/// Pull one account's `axes` object out of a `gauge.status`-shaped array
/// (`[{"account": name, "axes": {...}}, ...]`). Pure lookup — no polling.
fn gauge_axes_for(gauge: Option<&serde_json::Value>, account: &str) -> Option<serde_json::Value> {
    gauge?
        .as_array()?
        .iter()
        .find(|row| row.get("account").and_then(|v| v.as_str()) == Some(account))
        .and_then(|row| row.get("axes").cloned())
}

/// Roll per-thread activity up per account. Pure — every input is already
/// resolved, which is what makes this fixture-testable without a daemon.
///
/// Every POOL account appears in the output even when nothing is happening on
/// it (an agent's question is "is this account free?", and an absent row is not
/// an answer). The [`UNATTRIBUTED`] bucket appears only when non-empty.
///
/// **Seeded from the POOL, not the registry** (2026-08-27). This used to take a
/// `&Registry` and seed a row per registered slot, which is what put the machine
/// default into `accounts status`, into `accounts.status` on the wire, and from
/// there into the bar rows the `claude-accounts` TUI draws — where it survived
/// the renderer's `folds_into` filter on the two branches where the machine
/// login is nobody's alias. It takes the pool's entries now, so that row is not
/// produced at all rather than being filtered downstream.
pub fn activity_by_account(
    pool: &crate::pool::Pool,
    threads: &[ThreadActivity],
    states: &BTreeMap<String, AccountState>,
    now_rfc3339: &str,
    gauge: Option<&serde_json::Value>,
) -> Vec<AccountActivity> {
    let mut out: BTreeMap<String, AccountActivity> = BTreeMap::new();
    for entry in &pool.entries {
        let cd = entry.login_dir().map(str::to_string);
        out.insert(
            entry.name.clone(),
            AccountActivity::empty(&entry.name, cd, true),
        );
    }

    for t in threads {
        let entry = out
            .entry(t.account.clone())
            .or_insert_with(|| AccountActivity::empty(&t.account, None, false));
        entry.threads += 1;
        if t.executing {
            entry.executing += 1;
            entry.executing_thread_ids.push(t.thread_id.clone());
        }
        if let Some(reason) = &t.park_reason {
            entry.parked += 1;
            entry.parked_thread_ids.push(t.thread_id.clone());
            *entry.park_reasons.entry(reason.clone()).or_insert(0) += 1;
        }
        if t.queued_messages > 0 {
            entry.queued_threads += 1;
            entry.queued_messages += t.queued_messages;
        }
        if t.retrying {
            entry.retrying += 1;
        }
        if t.quiet() && !t.retrying {
            entry.idle += 1;
        }
    }

    // Which names are ALIASES for one login? Computed over the POOL only: an
    // `(unattributed)` bucket is not an account and has no identity.
    let units = selection_units(&pool.names(), states);
    let alias_of: BTreeMap<String, String> = units
        .iter()
        .filter(|u| u.is_aliased())
        .flat_map(|u| {
            u.members
                .iter()
                .map(|m| (m.clone(), u.canonical.clone()))
                .collect::<Vec<_>>()
        })
        .collect();

    for (name, entry) in out.iter_mut() {
        if let Some(st) = states.get(name) {
            entry.available = st.available(now_rfc3339);
            // The EFFECTIVE window, not the raw column: a limit belonging to a
            // login this label no longer points at bars nothing, and showing it
            // anyway is how "`default` is limited but its sibling is not" read
            // as a live fact for a whole afternoon.
            entry.limit_until = st.effective_limit_until(now_rfc3339).map(str::to_string);
            entry.last_limit_prose = st
                .effective_limit_until(now_rfc3339)
                .and(st.last_limit_prose.as_deref())
                .map(str::to_string);
            // Gated on the EFFECTIVE window for the same reason the prose is: an
            // axis without a live bar describes a limit that is not being
            // applied, and "login" in particular would read as "re-authenticate
            // this account" long after the account was fine.
            entry.limit_axis = st
                .effective_limit_until(now_rfc3339)
                .and(st.limit_axis.as_deref())
                .map(str::to_string);
            entry.healthy = st.healthy();
            entry.dead_since = st.dead_since.clone();
            entry.dead_reason = st.dead_reason.clone();
            entry.identity = st.identity.clone();
        }
        entry.selection_unit = alias_of.get(name).cloned();
        entry.gauge_axes = gauge_axes_for(gauge, name);
    }

    out.into_values().collect()
}

/// The full payload both surfaces return: the per-account rows plus a
/// pool-wide total (the one number that answers "is ANYTHING running?").
pub fn activity_json(rows: &[AccountActivity]) -> serde_json::Value {
    let executing: usize = rows.iter().map(|r| r.executing).sum();
    let parked: usize = rows.iter().map(|r| r.parked).sum();
    let queued: usize = rows.iter().map(|r| r.queued_messages).sum();
    let threads: usize = rows.iter().map(|r| r.threads).sum();
    // ── PARKED IS A POOL STATE, NOT AN ACCOUNT ONE (H179) ──
    //
    // The operator: *"you never park TO an account. Parked threads wait in the
    // pool; any placement event re-places ALL of them, each onto whichever
    // account admission picks."* The daemon already works that way —
    // `parked_turns` has no account column and every placement pass sweeps every
    // row — but this payload only said `parked` UNDER an account, which a reader
    // took for "3 parked on cc1". So the pool's own list is here, at top level,
    // and the per-account `parked` / `parked_thread_ids` / `park_reasons` stay
    // only as LAST-ACCOUNT attribution (where the thread last sat): the
    // `accounts status` table and `tests/ws_protocol.rs` read them, and dropping
    // them would be a wire break for no gain.
    let mut pool_reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut pool_threads: Vec<serde_json::Value> = Vec::new();
    for r in rows {
        for (reason, n) in &r.park_reasons {
            *pool_reasons.entry(reason.clone()).or_insert(0) += n;
        }
        for id in &r.parked_thread_ids {
            pool_threads.push(serde_json::json!({
                "thread_id": id,
                "last_account": r.account,
            }));
        }
    }
    serde_json::json!({
        "accounts": rows.iter().map(AccountActivity::to_json).collect::<Vec<_>>(),
        "pool_parked": {
            "count": parked,
            "threads": pool_threads,
            "reasons": pool_reasons,
        },
        "totals": {
            "executing": executing,
            "parked": parked,
            "queued_messages": queued,
            "threads": threads,
            "busy": executing > 0 || parked > 0 || queued > 0,
        },
    })
}

/// Build [`ThreadActivity`] rows for every thread in the store.
///
/// `live_accounts` is the daemon's in-memory thread-id → serving-account map;
/// it wins over the durable attribution per [`attribute_account`]. Both
/// surfaces pass an EMPTY map today — the CLI has no daemon to ask, and since
/// the IS-A fix the `threads.account` column already mirrors that map, so the
/// daemon has nothing better to offer.
///
/// Read-only: five fleet-wide indexed table reads, no per-thread fan-out.
/// Nothing here can poll, spawn, or write.
pub fn collect_thread_activity(
    store: &crate::store::Store,
    live_accounts: &BTreeMap<String, String>,
    limit: usize,
) -> Result<Vec<ThreadActivity>> {
    let executing: std::collections::HashSet<String> = store
        .list_running_turns()?
        .into_iter()
        .map(|r| r.thread_id)
        .collect();
    let parked: BTreeMap<String, String> = store
        .list_parked_turns()?
        .into_iter()
        .map(|p| (p.thread_id, p.reason))
        .collect();
    // The `retries` table, which is where `thread_states` reads `retrying` from
    // too — one fleet-wide read replacing a column echo.
    let retrying: std::collections::HashSet<String> = store
        .list_retries()?
        .into_iter()
        .map(|r| r.thread_id)
        .collect();
    // One grouped read replacing the per-queued-thread `waiting_queue` fan-out.
    let queued = store.waiting_queue_counts()?;

    let mut out = Vec::new();
    for row in store.list_threads(limit)? {
        let history = store.thread_accounts(&row.id).unwrap_or_default();
        let account = attribute_account(
            &history,
            row.account.as_deref(),
            live_accounts.get(&row.id).map(String::as_str),
        );
        out.push(ThreadActivity {
            executing: executing.contains(&row.id),
            retrying: retrying.contains(&row.id),
            park_reason: parked.get(&row.id).cloned(),
            queued_messages: queued.get(&row.id).copied().unwrap_or(0) as usize,
            thread_id: row.id,
            account,
        });
    }
    Ok(out)
}

/// The whole per-account activity answer, straight from a store + registry.
///
/// This is the one function both surfaces call: the `accounts.status` WS method
/// (passing the daemon's live account map and `Budgets::gauge_state`) and
/// `threads accounts status` (passing an empty live map). `gauge` is optional
/// and is only ever READ — a status query never costs a `/usage` poll.
pub fn collect_activity(
    store: &crate::store::Store,
    pool: &crate::pool::Pool,
    live_accounts: &BTreeMap<String, String>,
    gauge: Option<&serde_json::Value>,
    limit: usize,
) -> Result<Vec<AccountActivity>> {
    let threads = collect_thread_activity(store, live_accounts, limit)?;
    let states: BTreeMap<String, AccountState> = store
        .list_account_states()?
        .into_iter()
        .map(|s| (s.name.clone(), s))
        .collect();
    Ok(activity_by_account(
        pool,
        &threads,
        &states,
        &crate::store::now_rfc3339(),
        gauge,
    ))
}

/// SEAM: a live per-account usage probe (CCW shelled `claude /usage`). Not yet
/// implemented — implement against a real claude during phase-0 burn-in and
/// feed the results into `AccountState` / `accounts.status`.
pub trait UsageProbe: Send + Sync {
    /// Return `(pool, pct_used, reset_prose)` rows for the account, or `None`
    /// when probing is unavailable.
    fn probe(&self, config_dir: Option<&str>) -> Option<Vec<(String, f64, Option<String>)>>;
}

/// The default probe: unavailable (see [`UsageProbe`] seam note).
pub struct NoProbe;

impl UsageProbe for NoProbe {
    fn probe(&self, _config_dir: Option<&str>) -> Option<Vec<(String, f64, Option<String>)>> {
        None
    }
}

// ─────────────────────────────────────────────────────────────────────────
// OWN-LOGIN slots (task-multiaccount-signin).
//
// WHY THIS EXISTS. `capture_snapshot` CLONES the live keychain secret into a
// new service name. That was measured (account-visibility-report) NOT to
// produce an independent login: the clone is the same OAuth *session*, and
// Claude Code rotates the refresh token, so whichever copy refreshes second is
// rejected and self-wipes. Confirmed against the shipped binary (v2.1.220):
//
//   * the refresh exchange destructures `{access_token, refresh_token = old,
//     expires_in}` from the token response — the server MAY hand back a NEW
//     refresh token, and the client adopts it;
//   * on `invalid_grant` the client marks the refresh token dead and ZEROES
//     `accessToken`/`refreshToken`/`expiresAt` in its own store;
//   * cross-process reconciliation (a refresh lock + a compare-and-swap that
//     "adopts the sibling's" token) exists ONLY WITHIN ONE storage backend.
//     Two config dirs are two backends, so two clones cannot reconcile.
//
// The durable alternative is an OWN LOGIN per config dir: a genuinely separate
// OAuth grant with its own refresh token, in its own keychain service. This
// section is the machinery for preparing such a slot and — the part that
// matters — REFUSING to register the result unless it is provably a different
// account AND a different session from the live login.
//
// Everything here compares by HASH. No credential value, email, uuid or org
// name is ever returned, printed, or stored by these functions.
// ─────────────────────────────────────────────────────────────────────────

/// `credential_mode` written for a slot the operator logged into himself.
pub const LOGIN_CREDENTIAL_MODE: &str = "own-login";

/// 8-hex-char SHA-256 prefix of `s`. The one hashing primitive these helpers
/// use, so bash mirrors only have to match `printf %s <s> | shasum -a 256`.
pub fn sha8(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(s.as_bytes());
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    hex[..8].to_string()
}

/// A non-reversible identity fingerprint of a `.claude.json`'s `oauthAccount`
/// block: which account/email/org a config dir is logged in as, expressed only
/// as hashes so it is safe to print, log, and commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityFingerprint {
    /// `sha8(accountUuid)`.
    pub acct: String,
    /// `sha8(emailAddress)`.
    pub email: String,
    /// `sha8(organizationUuid)`.
    pub org: String,
    /// Billing tier verbatim — not identifying (e.g. `stripe_subscription`).
    pub tier: String,
}

impl IdentityFingerprint {
    /// Fingerprint the `oauthAccount` block of a `.claude.json`. `Ok(None)`
    /// means the file exists but carries no `oauthAccount` (never logged in).
    pub fn from_config_json(path: &std::path::Path) -> Result<Option<Self>> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("reading {}: {e}", path.display())))?;
        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| Error::Config(format!("parsing {}: {e}", path.display())))?;
        let Some(oa) = v.get("oauthAccount") else {
            return Ok(None);
        };
        let field = |k: &str| -> String {
            oa.get(k)
                .and_then(|x| x.as_str())
                .map(sha8)
                .unwrap_or_else(|| "-".to_string())
        };
        Ok(Some(Self {
            acct: field("accountUuid"),
            email: field("emailAddress"),
            org: field("organizationUuid"),
            tier: oa
                .get("billingType")
                .or_else(|| oa.get("subscriptionType"))
                .and_then(|x| x.as_str())
                .unwrap_or("unknown")
                .to_string(),
        }))
    }

    /// Same human/account, regardless of which org/workspace is active.
    /// (`secondary` in this repo's registry was exactly this case: same
    /// account and email as the live login, different `organizationUuid` —
    /// which is why it never had a credential of its own.)
    pub fn same_account_as(&self, other: &Self) -> bool {
        self.acct == other.acct && self.email == other.email
    }
}

/// A prepared, empty own-login slot: where the login will land and which
/// keychain service the spawned `claude` will read and write.
#[derive(Debug, Clone)]
pub struct LoginSlot {
    pub name: String,
    /// The slot dir — the account's `CLAUDE_CONFIG_DIR`.
    pub dir: PathBuf,
    /// `<dir>/.claude.json`, written by the login itself.
    pub config_json: PathBuf,
    /// The macOS keychain service the login will write. Derived from `dir`, so
    /// it is DIFFERENT from the live login's bare `Claude Code-credentials`.
    pub keychain_service: String,
    /// True when the slot already held a manifest (i.e. this is a re-login).
    pub preexisting: bool,
}

/// Prepare `<data_dir>/accounts/<name>/` to receive its own `/login`.
///
/// Creates the dir 0700 (and the force-`*` `.gitignore` alongside it), and
/// derives the keychain service. It does NOT read, copy, or clone any
/// credential, does not invoke `claude`, and cannot touch the machine-default
/// keychain service: the service name it reports is a function of `dir`, and
/// `dir` is never the default config dir.
///
/// **A PRE-EXISTING SNAPSHOT DOES NOT REFUSE.** This function used to return
/// `"account {name} already has a snapshot at {dir} — pass --force"` whenever a
/// `manifest.toml` was present and `force` was not set. That refusal is GONE,
/// by operator ruling: re-logging a slot in is always permitted and always
/// replaces what was there, and a refusal that only relaxes when you pass a flag
/// is still a refusal — he should not have to know the flag exists.
///
/// The old justification was that a re-login "would strand the keychain entry
/// under a manifest describing something else". It does not: the keychain
/// service name is a pure function of the slot DIRECTORY, so a fresh login into
/// the same slot writes the same service name, replacing the credential in
/// place, and the manifest is rewritten immediately afterwards by
/// [`write_login_manifest`]. There is nothing to strand.
///
/// `force` is retained in the signature and ignored. Every caller still passes
/// it; making it a compile error would be churn, and silently changing its
/// meaning is exactly what the doc comment above is for. `preexisting` is still
/// REPORTED on the returned [`LoginSlot`] — callers may say "this is a
/// re-login", they may not refuse one.
pub fn prepare_login_slot(
    data_dir: &std::path::Path,
    name: &str,
    _force: bool,
) -> Result<LoginSlot> {
    validate_account_name(name)?;
    let accounts_root = data_dir.join("accounts");
    let dir = accounts_root.join(name);
    let preexisting = dir.join("manifest.toml").exists();
    // The whole point of this slot is that it is NOT the default config dir.
    // Assert it rather than trust it: an own-login into the default dir would
    // overwrite the operator's live credential in place (`add-generic-password
    // -U`), which is the one outcome this task must never produce.
    let dir_str = dir.to_string_lossy().to_string();
    let service = keychain_service_for(Some(&dir_str));
    if service == KEYCHAIN_SERVICE_BASE {
        return Err(Error::Config(
            "refusing: the slot resolved to the machine-default keychain service".into(),
        ));
    }
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Config(format!("creating {}: {e}", dir.display())))?;
    set_mode(&accounts_root, 0o700);
    set_mode(&dir, 0o700);
    let gitignore = accounts_root.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(&gitignore, "*\n")
            .map_err(|e| Error::Config(format!("writing {}: {e}", gitignore.display())))?;
    }
    Ok(LoginSlot {
        name: name.to_string(),
        config_json: dir.join(".claude.json"),
        keychain_service: service,
        dir,
        preexisting,
    })
}

/// Write the non-secret `manifest.toml` for a slot that has its OWN login, so
/// `threads accounts adopt <name>` registers it like any other snapshot.
pub fn write_login_manifest(slot: &LoginSlot) -> Result<()> {
    let dir_str = slot.dir.to_string_lossy().to_string();
    let manifest = SnapshotManifest {
        label: slot.name.clone(),
        captured_at: chrono::Utc::now().to_rfc3339(),
        // An own-login was not captured FROM anywhere; the slot is the source.
        source_config_dir: dir_str.clone(),
        source_config_json: slot.config_json.to_string_lossy().to_string(),
        spawn_env: format!("CLAUDE_CONFIG_DIR={dir_str}"),
        credential_mode: LOGIN_CREDENTIAL_MODE.to_string(),
        keychain_service: slot.keychain_service.clone(),
        keychain_account: keychain_account(),
        note: Some(
            "own login (not a clone): this slot completed its own `claude auth login`, \
             so it holds an independent OAuth grant with its own refresh token and \
             cannot be invalidated by any other account's refresh"
                .into(),
        ),
    };
    let toml = toml::to_string_pretty(&manifest)
        .map_err(|e| Error::Config(format!("rendering manifest.toml: {e}")))?;
    let path = slot.dir.join("manifest.toml");
    std::fs::write(&path, toml)
        .map_err(|e| Error::Config(format!("writing {}: {e}", path.display())))?;
    set_mode(&path, 0o600);
    Ok(())
}

/// One side of a distinctness comparison: who a config dir is logged in as,
/// and a hash of the keychain secret it reads. Hashes only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionWitness {
    /// `None` when the config dir has no `oauthAccount` yet.
    pub identity: Option<IdentityFingerprint>,
    /// `sha8` of the raw keychain secret blob. `None` = no keychain entry.
    pub secret: Option<String>,
}

/// Why a freshly-logged-in slot may NOT be registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DistinctnessVerdict {
    /// A genuinely independent login: different account AND different secret.
    Distinct,
    /// No credential landed — the login did not complete.
    NoCredential,
    /// The secret blob is byte-identical to the live login's: this is the SAME
    /// session (a clone), not a second login. It will die on the next refresh.
    SameSession,
    /// A real, separate grant — but for the SAME account. Two grants for one
    /// account share one rate-limit pool, so this buys nothing.
    SameAccount,
}

/// What a [`DistinctnessVerdict`] MEANS FOR THE RUN. Mirrors bash
/// `login_verdict_action` in `scripts/add-account`; the conformance test pins
/// the two together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerdictAction {
    /// Register, silently. The happy path.
    Register,
    /// Register, after saying loudly what he got. NOT a stop.
    Warn,
    /// Nothing to register: the login did not land. A statement of fact rather
    /// than a policy refusal — the remedy is to run the same command again, and
    /// running it again is never blocked.
    Report,
    /// The ONE hard stop, and it is not about prior state: a clone.
    Refuse,
}

impl DistinctnessVerdict {
    pub fn ok(&self) -> bool {
        matches!(self, Self::Distinct)
    }

    /// **THE POLICY, IN ONE PLACE.**
    ///
    /// Exactly one verdict stops a registration, and it is [`Self::SameSession`]
    /// — the clone. Two stores sharing one refresh token means the second
    /// refresher is handed a retired token and self-wipes; that is what
    /// destroyed `primary` and `secondary-live`.
    ///
    /// [`Self::SameAccount`] USED TO STOP IT TOO, unless `--allow-same-account`
    /// was passed. It no longer does, by operator ruling: a flag-gated refusal
    /// is still a refusal. It warns and registers, and the manifest records the
    /// verdict honestly so the registry never claims a same-account slot is
    /// distinct.
    ///
    /// Note what CANNOT reach `Refuse`: a fresh browser login into a slot that
    /// already holds a credential — live, dead, or issued ten minutes ago. A
    /// fresh grant is a new token, so it can never be byte-identical to another
    /// store's. Refusing to DUPLICATE a credential and refusing to RE-LOGIN are
    /// different things, and only the first one survives.
    pub fn action(&self) -> VerdictAction {
        match self {
            Self::Distinct => VerdictAction::Register,
            Self::SameAccount => VerdictAction::Warn,
            Self::NoCredential => VerdictAction::Report,
            Self::SameSession => VerdictAction::Refuse,
        }
    }

    /// Does this verdict STOP the registration? Exactly one does.
    pub fn blocks_registration(&self) -> bool {
        matches!(self.action(), VerdictAction::Refuse)
    }

    /// One line, safe to speak aloud. No identifying value appears.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Distinct => {
                "independent login: different account, different session secret"
            }
            Self::NoCredential => {
                "no credential landed in the slot — the login did not complete"
            }
            Self::SameSession => {
                "the slot's secret is byte-identical to the live login's — this is \
                 the same session, not a second login; it dies on the next refresh"
            }
            Self::SameAccount => {
                "logged in as the SAME account as the live login — a separate grant, \
                 but it shares one rate-limit pool, so it buys nothing"
            }
        }
    }
}

/// Decide whether a newly-logged-in slot is genuinely independent of the live
/// login. Pure; ordering matters — `SameSession` outranks `SameAccount`
/// because a byte-identical secret is the failure that actively kills a
/// working login, while a same-account grant is merely useless.
pub fn verify_distinct(live: &SessionWitness, slot: &SessionWitness) -> DistinctnessVerdict {
    let Some(slot_secret) = slot.secret.as_ref() else {
        return DistinctnessVerdict::NoCredential;
    };
    if slot.identity.is_none() {
        return DistinctnessVerdict::NoCredential;
    }
    if live.secret.as_ref() == Some(slot_secret) {
        return DistinctnessVerdict::SameSession;
    }
    match (&live.identity, &slot.identity) {
        (Some(l), Some(s)) if s.same_account_as(l) => DistinctnessVerdict::SameAccount,
        _ => DistinctnessVerdict::Distinct,
    }
}

// ─────────────────────────────────────────────────────────────────────────
// SLOT HEALTH AND KEY ROTATION (task-account-login-cli)
//
// The operator-facing `scripts/claude-accounts` command renders what is
// computed here. Everything in this section is either PURE (and unit-tested)
// or a thin, read-only `security(1)` call. Nothing here writes a keychain
// item, invokes `claude`, or can reach the machine-default credential.
//
// THE RULE THAT GOVERNS THE WHOLE SECTION. A cloned credential and the live
// login share ONE refresh token. Refreshing a clone therefore ROTATES THE
// OPERATOR'S OWN REFRESH TOKEN, and because two config dirs are two storage
// backends that cannot reconcile (multiaccount-signin-report §1), whichever
// copy refreshes second is rejected and self-wipes. So rotation refuses any
// slot that is not a proven own-login, and refuses the machine default
// outright. See `rotation_plan`, which is where that refusal is decided and
// where it is tested.
// ─────────────────────────────────────────────────────────────────────────

/// Claude Code's default refresh-token lifetime when the token response does
/// not state one: 30 days.
///
/// CONFIRMED-CODE, v2.1.220. `ano(e, t)` = `typeof e === "number" ? Date.now()
/// + e*1000 : (t ? Date.now() + sxg : undefined)`, with `sxg = 2592000000`.
/// The LOGIN path calls it with `t = true` (so a missing server value falls
/// back to 30 days); the REFRESH path calls it with `t = false`, and the store
/// merge keeps the previous value when it comes back `undefined`
/// (`refreshTokenExpiresAt: t.refreshTokenExpiresAt ?? e?.refreshTokenExpiresAt`).
/// So a refresh EXTENDS the window only if the server returns
/// `refresh_token_expires_in`. `rotate` measures the before/after rather than
/// assuming it — see `RotationOutcome`.
pub const DEFAULT_REFRESH_TOKEN_TTL_MS: i64 = 2_592_000_000;

/// Non-secret facts read out of one credential blob.
///
/// Deliberately holds NO token material: only a hash, two absolute instants,
/// and two plan labels. This is the only type that ever sees a credential
/// blob, and it is constructed by a single pure function so that what it can
/// and cannot carry is checkable by test.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialFacts {
    /// `sha8` of the raw blob. `None` = no keychain entry at all.
    ///
    /// Mirrors bash `kc_secret_sha8`: newlines stripped, then sha256. An entry
    /// that exists but is EMPTY reads as `None`, never as `sha8("")` —
    /// `e3b0c442` is a plausible-looking hash that would otherwise make "no
    /// credential" pass as a real, distinct secret.
    pub secret_sha8: Option<String>,
    /// `claudeAiOauth.expiresAt` — when the ACCESS token dies (~8h).
    pub access_expires_at_ms: Option<i64>,
    /// `claudeAiOauth.refreshTokenExpiresAt` — when the LOGIN dies (~30d).
    pub refresh_expires_at_ms: Option<i64>,
    /// False when the blob exists but `refreshToken` is `""` — the shape
    /// Claude Code leaves behind when it zeroes a store after an
    /// `invalid_grant` it could not recover. That is a DEAD login, and it is
    /// what killed `primary`.
    pub refresh_token_present: bool,
    /// `claudeAiOauth.subscriptionType` (e.g. `max`). Not identifying.
    pub subscription_type: Option<String>,
}

/// Parse the non-secret facts out of a credential blob. Pure.
///
/// Returns a fully-empty `CredentialFacts` for anything unparseable, which
/// classifies as `NoLogin` — the safe direction, since every unsafe action is
/// gated on a POSITIVE proof of health.
pub fn credential_facts_from_blob(blob: &str) -> CredentialFacts {
    // Match bash `kc_secret_sha8`: `tr -d '\n'` then `shasum -a 256`.
    let stripped: String = blob.chars().filter(|c| *c != '\n').collect();
    let secret_sha8 = if stripped.is_empty() {
        None
    } else {
        Some(sha8(&stripped))
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&stripped) else {
        return CredentialFacts {
            secret_sha8,
            ..Default::default()
        };
    };
    let Some(oauth) = v.get("claudeAiOauth") else {
        return CredentialFacts {
            secret_sha8,
            ..Default::default()
        };
    };
    let ms = |k: &str| -> Option<i64> {
        oauth
            .get(k)
            .and_then(|x| x.as_i64())
            .filter(|n| *n > 0)
    };
    CredentialFacts {
        secret_sha8,
        access_expires_at_ms: ms("expiresAt"),
        refresh_expires_at_ms: ms("refreshTokenExpiresAt"),
        refresh_token_present: oauth
            .get("refreshToken")
            .and_then(|x| x.as_str())
            .is_some_and(|s| !s.is_empty()),
        subscription_type: oauth
            .get("subscriptionType")
            .and_then(|x| x.as_str())
            .map(str::to_string),
    }
}

/// What state one account slot's login is in. Ordered by severity of the
/// action it implies, not by how bad it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum SlotHealth {
    /// No keychain entry, or one with no `claudeAiOauth` block. Never signed
    /// in (or the entry was deleted).
    NoLogin,
    /// **R9.T7 (gap T-7): the slot's `manifest.toml` DECLARES a keychain login
    /// — `own-login` or `keychain-cloned`, `declared` names which — and its
    /// store holds no item (or its dir has no `.claude.json`).** Plain
    /// [`Self::NoLogin`] stays placeable because a plaintext install and every
    /// test fixture look like that too; a slot that SAYS its login lives in
    /// the keychain and has none there is not that ambiguity — its next turn
    /// is a 401. So this reading is UNAVAILABLE ([`Self::needs_sign_in`]).
    ///
    /// Same word as `NoLogin` (`no-login`, so placement's reason is
    /// `login_no-login`) and the same serialized `state`, so every face that
    /// already draws `no-login` draws this one; the board's availability
    /// verdict is what tells the two apart. Never deserialized: a `no-login`
    /// state reads back as [`Self::NoLogin`].
    #[serde(rename = "no-login", skip_deserializing)]
    NoLoginDeclared { declared: String },
    /// The entry exists but its refresh token is `""`: Claude Code zeroed the
    /// store after a refresh it could not recover. Nothing can revive this but
    /// a fresh sign-in.
    Wiped,
    /// The secret blob is byte-identical to another store's. This is ONE
    /// session with two homes, not two logins — and the copy that refreshes
    /// second dies. `of` names the twin (`"the live login"` for the machine
    /// default).
    CloneOf { of: String },
    /// A capture (`credential_mode = "keychain-cloned"`) whose secret no
    /// longer matches any live store. The two copies have DIVERGED, which
    /// means one of them has refreshed and rotated the shared token away from
    /// the other. This copy still has dates in the future and still reports
    /// "logged in" locally, so it reads healthy and is not.
    ///
    /// Observed on this machine 2026-07-30: `secondary-live` was byte-identical
    /// to the live login at 18:59Z on the 29th; by 04:44Z the live login had
    /// refreshed and the two no longer matched. A hash comparison ALONE would
    /// have promoted it from "clone" to "live" at exactly the moment it died —
    /// which is why clone-ness is read from the manifest as well as measured.
    Stranded,
    /// The refresh token's own expiry has passed. Only a sign-in fixes it.
    Expired,
    /// A healthy, independent login.
    Live,
}

impl SlotHealth {
    /// Is this slot usable as an account right now?
    pub fn usable(&self) -> bool {
        matches!(self, Self::Live)
    }

    /// **Does this slot need a fresh sign-in before it can run a turn?** True
    /// for every reading that is a POSITIVE fact about the store: [`Self::Wiped`]
    /// (no refresh token), [`Self::Expired`] (the refresh token's own expiry has
    /// passed) and [`Self::Stranded`] (a clone whose twin rotated the token
    /// away). No timed recheck changes any of them; only a sign-in does.
    ///
    /// H179: this was `Wiped` only, so an EXPIRED slot — which the board drew
    /// as `EXPIRED` and `FROZEN` — stayed a placement candidate. The operator
    /// read those words as "account not available", and placement now does too
    /// (`ThreadManager::account_unavailability`).
    ///
    /// `NoLogin` is deliberately NOT here. A missing keychain item is also what
    /// a plaintext-mode install and every test fixture look like, and absence of
    /// evidence is never treated as a fault in this crate (`selectable`).
    ///
    /// R9.A2 (gap A-2): [`Self::CloneOf`] is here too. It is not dead yet, but
    /// it is the one state whose USE does the damage: a turn on a clone
    /// refreshes the shared token and logs its twin out — possibly the
    /// operator's own login. Its only fix is a sign-in of its own
    /// (`claude-accounts login <label>`), and unlike the other three it CAN
    /// change on a recheck (the twin refreshes, and it becomes `Stranded`).
    pub fn needs_sign_in(&self) -> bool {
        matches!(
            self,
            Self::Wiped
                | Self::Expired
                | Self::Stranded
                | Self::CloneOf { .. }
                | Self::NoLoginDeclared { .. }
        )
    }

    /// The machine word for this reading — the same word the `accounts`
    /// shape's `credential.slot_state` carries, so a reason placement gives and
    /// a badge the board draws are spelled identically.
    pub fn word(&self) -> &'static str {
        match self {
            Self::NoLogin | Self::NoLoginDeclared { .. } => "no-login",
            Self::Wiped => "wiped",
            Self::CloneOf { .. } => "clone-of",
            Self::Stranded => "stranded",
            Self::Expired => "expired",
            Self::Live => "live",
        }
    }

    /// One line, safe to speak aloud and safe to commit. No identifying value.
    pub fn summary(&self) -> String {
        match self {
            Self::NoLogin => "not signed in".to_string(),
            Self::NoLoginDeclared { declared } => format!(
                "not signed in — the slot declares a {declared} login and its keychain \
                 store holds none, so it is unavailable"
            ),
            Self::Wiped => {
                "DEAD — claude zeroed this store after a refresh it could not recover"
                    .to_string()
            }
            Self::CloneOf { of } => format!(
                "CLONE of {of} — the same session in two places, not a second \
                 login; it dies the next time either copy refreshes"
            ),
            Self::Stranded => "STRANDED CLONE — it was captured from another store, \
                 which has since rotated the shared token away from it; it still \
                 reads 'logged in' locally but its refresh will be rejected"
                .to_string(),
            Self::Expired => "EXPIRED — the refresh token is past its own expiry".to_string(),
            Self::Live => "live".to_string(),
        }
    }
}

/// Which slot (if any) shares `secret` with this one. Pure.
///
/// `peers` is `(label, secret_sha8)` for every OTHER store, the machine
/// default included. A `None` secret never matches anything: absence of a
/// credential is not evidence of sameness.
pub fn find_clone_of<'a>(
    secret: Option<&str>,
    peers: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
) -> Option<String> {
    let secret = secret?;
    peers.into_iter().find_map(|(label, peer)| {
        (peer == Some(secret)).then(|| label.to_string())
    })
}

/// `credential_mode` written by capture (keychain cloning). Never durable.
pub const CLONED_CREDENTIAL_MODE: &str = "keychain-cloned";

/// Classify one slot from its facts. Pure.
///
/// Order is load-bearing, and clone-ness comes before every reading of the
/// dates. A clone that still looks healthy is the single most dangerous state
/// in this system: it is the one that, if refreshed, takes the operator's live
/// login down with it.
///
/// Clone-ness is established TWO ways, and both are needed:
///
/// * **measured** — the secret is byte-identical to another store's right now
///   (`clone_of`). True only while neither copy has refreshed.
/// * **declared** — `manifest.toml` says the slot was captured
///   (`credential_mode = "keychain-cloned"`). True forever.
///
/// Using only the measurement is a live bug: the instant the source refreshes,
/// the hashes stop matching and the clone would be promoted to `Live` at
/// precisely the moment its token became worthless. See `SlotHealth::Stranded`.
pub fn classify_slot(
    facts: &CredentialFacts,
    credential_mode: Option<&str>,
    clone_of: Option<String>,
    now_ms: i64,
) -> SlotHealth {
    if facts.secret_sha8.is_none() {
        return no_login_for(credential_mode);
    }
    if !facts.refresh_token_present {
        // An entry with a blank refresh token, and an entry whose blob we
        // could not parse at all, are the same actionable state: no usable
        // grant is in there.
        return SlotHealth::Wiped;
    }
    if let Some(of) = clone_of {
        return SlotHealth::CloneOf { of };
    }
    if credential_mode == Some(CLONED_CREDENTIAL_MODE) {
        return SlotHealth::Stranded;
    }
    match facts.refresh_expires_at_ms {
        Some(exp) if exp <= now_ms => SlotHealth::Expired,
        _ => SlotHealth::Live,
    }
}

/// **Does this `credential_mode` declare that the slot's login lives in the
/// KEYCHAIN? (R9.T7)** `own-login` and `keychain-cloned` do; `plaintext-file`,
/// `manual-login-required`, any other string and no manifest at all do not.
pub fn declares_keychain_login(credential_mode: Option<&str>) -> bool {
    matches!(credential_mode, Some(m) if m == LOGIN_CREDENTIAL_MODE || m == CLONED_CREDENTIAL_MODE)
}

/// The reading for a slot with no store to read (R9.T7): [`SlotHealth::NoLoginDeclared`]
/// when its manifest declares a keychain login, else today's placeable
/// [`SlotHealth::NoLogin`]. The ONE place that choice is made — [`classify_slot`]
/// (board and probe) and the probe's no-`.claude.json` return both call it.
pub fn no_login_for(credential_mode: Option<&str>) -> SlotHealth {
    match credential_mode {
        Some(m) if declares_keychain_login(Some(m)) => {
            SlotHealth::NoLoginDeclared { declared: m.to_string() }
        }
        _ => SlotHealth::NoLogin,
    }
}

/// The `credential_mode` a slot's `manifest.toml` declares, if it has one
/// that parses. The ONE reader of it: the board ([`collect_slot_reports`]) and
/// the runtime probe ([`probe_slot_health_among`]) both come through here, so
/// the two can never disagree about whether a slot was captured.
pub fn manifest_credential_mode(config_dir: &str) -> Option<String> {
    read_slot_manifest(config_dir).map(|m| m.credential_mode)
}

/// A slot's `manifest.toml`, if it has one that parses. File read only — never
/// the keychain.
pub fn read_slot_manifest(config_dir: &str) -> Option<SnapshotManifest> {
    let text =
        std::fs::read_to_string(std::path::Path::new(config_dir).join("manifest.toml")).ok()?;
    toml::from_str::<SnapshotManifest>(&text).ok()
}

/// **Of two stores holding ONE session, is THIS one the copy? (R9.A2b, A-2
/// inspect F1.)** Pure. `me` / `peer` are the two slots' manifests (`None` =
/// no parseable manifest); `peer_is_machine_default` is true for
/// `"the live login"`.
///
/// Only the COPY is barred as `CloneOf`. Barring both halves cost capacity and
/// bought no safety: when the original refreshes, the copy is stranded, and the
/// copy is already out of placement. The rule, in order:
///
/// 1. **The machine default is always the original.** It is the operator's own
///    login; nothing is ever captured INTO it, so a pool slot byte-identical
///    to it is the copy.
/// 2. **The declaration decides, as A2 already decides Stranded.** A slot whose
///    manifest says `credential_mode = "keychain-cloned"` was captured FROM
///    somewhere; one that does not, was not. If exactly one of the two
///    declares a capture, that one is the copy.
/// 3. **Otherwise the earlier-established is the original** — the manifest's
///    `captured_at` (written when the slot was set up, own-login or capture),
///    strictly earlier.
/// 4. **Undecidable → the copy.** No manifest, an unparseable `captured_at`, or
///    two equal stamps: this slot's provenance cannot be told apart from its
///    twin's, so both stay barred, which is A2's behaviour and the safe side.
pub fn is_the_copy(
    me: Option<&SnapshotManifest>,
    peer: Option<&SnapshotManifest>,
    peer_is_machine_default: bool,
) -> bool {
    if peer_is_machine_default {
        return true;
    }
    let captured = |m: Option<&SnapshotManifest>| {
        m.is_some_and(|m| m.credential_mode == CLONED_CREDENTIAL_MODE)
    };
    match (captured(me), captured(peer)) {
        (true, false) => return true,
        (false, true) => return false,
        _ => {}
    }
    let at = |m: Option<&SnapshotManifest>| {
        m.and_then(|m| chrono::DateTime::parse_from_rfc3339(&m.captured_at).ok())
    };
    match (at(me), at(peer)) {
        (Some(mine), Some(theirs)) if mine != theirs => mine > theirs,
        _ => true,
    }
}

/// One store's facts from the macOS keychain, addressed by config dir
/// (`None` = the machine default). The production reader the probe is handed.
///
/// It reads through the same process-wide override as the inventory
/// ([`set_keychain_reader_for_test`]), so a test's one fixture covers the
/// probe's default store too (R9.F2d, CON-28). In production no override is
/// set and this is [`read_credential_facts`], exactly as before.
pub fn read_store_facts(config_dir: Option<&str>) -> CredentialFacts {
    inventory_read(&keychain_service_for(config_dir), &keychain_account())
}

/// **One slot's health, classified exactly as the board classifies it** —
/// with the manifest's declared `credential_mode` and the peer hashes, so the
/// two clone states are reachable (R9.A2, gap A-2). Before this the runtime
/// probe called `classify_slot(&facts, None, None, …)`, so `Stranded` and
/// `CloneOf` could never come back from it: `login_stranded` never fired in
/// production, and a cloned slot was placed on, refreshed its token and logged
/// out its twin.
///
/// `peers` is `(display label, config dir)` for every OTHER store — the
/// machine default as `("the live login", None)`, the same spelling
/// [`build_slot_reports`] uses. `read_facts` reads one store by config dir
/// ([`read_store_facts`] in production; a stand-in under test, which must
/// never touch the keychain).
///
/// Peers are read only when the slot holds a usable grant: [`classify_slot`]
/// decides `NoLogin` and `Wiped` before it looks at any peer, so those cost
/// one read, as before.
///
/// The runtime calls this at boot, after a turn failed to authenticate, and
/// on a slow recheck while a slot is known unavailable — never per placement
/// decision.
pub fn probe_slot_health_among(
    login_dir: Option<&str>,
    peers: &[(String, Option<String>)],
    read_facts: &dyn Fn(Option<&str>) -> CredentialFacts,
) -> SlotHealth {
    // R9.T7: the manifest is read FIRST (a file read, never the keychain), so
    // both no-login returns below know whether the slot DECLARES a keychain
    // login — one that does and has none is unavailable, not placeable.
    let manifest = login_dir.and_then(read_slot_manifest);
    let mode = manifest.as_ref().map(|m| m.credential_mode.clone());
    // A directory with no `.claude.json` was never signed in — the same "looks
    // like a slot" predicate `collect_slot_reports` uses — so there is no store
    // worth asking `security` about.
    if !has_config_json(login_dir) {
        return no_login_for(mode.as_deref());
    }
    let now_ms = chrono::Utc::now().timestamp_millis();
    let facts = read_facts(login_dir);
    if facts.secret_sha8.is_none() || !facts.refresh_token_present {
        return classify_slot(&facts, mode.as_deref(), None, now_ms);
    }
    // R9.A2b: a twin only makes THIS slot `CloneOf` when this slot is the copy
    // ([`is_the_copy`]); the original of a pool pair stays placeable. A peer's
    // manifest is read only when its secret matches (a file read, no keychain).
    let mine = facts.secret_sha8.as_deref();
    let clone_of = peers.iter().find_map(|(label, dir)| {
        let theirs = read_facts(dir.as_deref()).secret_sha8;
        if find_clone_of(mine, [(label.as_str(), theirs.as_deref())]).is_none() {
            return None;
        }
        let peer_manifest = dir.as_deref().and_then(read_slot_manifest);
        is_the_copy(manifest.as_ref(), peer_manifest.as_ref(), dir.is_none())
            .then(|| label.clone())
    });
    classify_slot(&facts, mode.as_deref(), clone_of, now_ms)
}

/// Does this slot dir hold a `.claude.json`? The machine default (`None`)
/// always counts. The probe asks no store about a dir without one, and the
/// board (R9.T7) draws such a dir that declares a keychain login from the same
/// empty facts, so the two verdicts agree.
fn has_config_json(login_dir: Option<&str>) -> bool {
    login_dir.map_or(true, |d| std::path::Path::new(d).join(".claude.json").exists())
}

/// [`probe_slot_health_among`] against the real keychain.
pub fn probe_slot_health(login_dir: Option<&str>, peers: &[(String, Option<String>)]) -> SlotHealth {
    probe_slot_health_among(login_dir, peers, &read_store_facts)
}

/// Days until the LOGIN (not the access token) expires. `None` when the blob
/// carries no `refreshTokenExpiresAt` — an older login, or one the server
/// never dated.
pub fn days_left(facts: &CredentialFacts, now_ms: i64) -> Option<f64> {
    facts
        .refresh_expires_at_ms
        .map(|exp| (exp - now_ms) as f64 / 86_400_000.0)
}

/// Has the ACCESS token expired? This is the only lever rotation has: Claude
/// Code refreshes lazily, so a probe only rotates the refresh token when the
/// access token it would otherwise reuse is already dead.
pub fn access_expired(facts: &CredentialFacts, now_ms: i64) -> bool {
    match facts.access_expires_at_ms {
        Some(exp) => exp <= now_ms,
        // No recorded access expiry: treat as expired so the slot gets its
        // chance rather than being silently skipped forever.
        None => true,
    }
}

/// What the daily rotation pass should do with one slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum RotationAction {
    /// Run the free probe under this slot's own config dir so Claude Code
    /// refreshes it from its own store.
    Refresh,
    /// Nothing to do, and nothing wrong.
    Skip { reason: String },
    /// Automation cannot fix this one; the operator has to sign it in.
    ManualLoginRequired { reason: String },
}

impl RotationAction {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Refresh => "refresh",
            Self::Skip { .. } => "skip",
            Self::ManualLoginRequired { .. } => "manual-login-required",
        }
    }
}

/// Decide what rotation may do with one slot. Pure, and the safety boundary
/// of the whole rotation feature.
///
/// The organising idea: **the danger only exists where a live, rotatable
/// credential exists.** So the order is
///
/// 1. the machine default — never, under any reading;
/// 2. slots with nothing to rotate (no login / wiped / expired) — these are
///    harmless and the honest answer is "a human must sign this in";
/// 3. slots holding a credential that is or might be SHARED (a measured clone,
///    or one whose manifest does not prove it owns its login) — refused,
///    because refreshing them consumes a refresh token the live session also
///    depends on;
/// 4. only what is left — a proven, healthy own-login — may be refreshed.
pub fn rotation_plan(
    is_machine_default: bool,
    credential_mode: Option<&str>,
    health: &SlotHealth,
    facts: &CredentialFacts,
    now_ms: i64,
) -> RotationAction {
    let skip = |r: &str| RotationAction::Skip { reason: r.to_string() };
    let manual = |r: &str| RotationAction::ManualLoginRequired {
        reason: r.to_string(),
    };

    // 1. The machine default IS the operator's live login. Automation never
    //    touches it — his own sessions keep it fresh, and a background refresh
    //    racing an interactive one is the documented way to lose it
    //    (upstream #24317 / #43392).
    if is_machine_default {
        return skip(
            "the machine default is your live login — this tool never refreshes it; \
             your own sessions keep it fresh",
        );
    }

    // 2. Nothing rotatable in there. Harmless, and a human is the only fix —
    //    said before the provenance checks so that an empty or dead slot reads
    //    as "sign this in" rather than as a safety refusal about a credential
    //    that is not there.
    match health {
        SlotHealth::NoLogin => {
            return manual("no credential in this slot — sign it in")
        }
        SlotHealth::NoLoginDeclared { declared } => {
            return manual(&format!(
                "the slot declares a {declared} login and its keychain store holds none — \
                 sign it in"
            ))
        }
        SlotHealth::Wiped => {
            return manual(
                "claude zeroed this store after a refresh it could not recover; \
                 there is no automatic path back — sign it in again",
            )
        }
        SlotHealth::Expired => {
            return manual("the refresh token is past its own expiry — sign it in again")
        }
        SlotHealth::CloneOf { .. } | SlotHealth::Stranded | SlotHealth::Live => {}
    }

    // 3. A clone shares one refresh token with whatever it was copied from.
    //    Refreshing it rotates THAT token, and the other copy dies. Refused
    //    rather than reported as a human action, because the ONE thing that
    //    must not happen here is a refresh — the sign-in nudge rides along in
    //    the reason.
    if let SlotHealth::CloneOf { of } = health {
        return skip(&format!(
            "clone of {of} — refreshing it would rotate the shared refresh token \
             and kill the other copy; sign this slot in with its own login instead",
        ));
    }
    // 3b. A capture that has already diverged from its source. It may still
    //     hold the CURRENT token (if it was the last copy to refresh), in
    //     which case refreshing it would rotate the live login's token out
    //     from under it. Same refusal, same fix.
    if matches!(health, SlotHealth::Stranded) {
        return skip(
            "a capture that has diverged from the store it was copied from — one of the \
             two is already holding a rotated-away token, and refreshing the wrong one \
             kills the other; sign this slot in with its own login instead",
        );
    }
    // 4. The same refusal, sourced from the manifest rather than from the
    //    measurement — so a slot whose twin is not in this inventory (another
    //    machine, a deleted label) is still refused.
    match credential_mode {
        Some(m) if m == LOGIN_CREDENTIAL_MODE => {}
        Some(m) => {
            return skip(&format!(
                "credential_mode is {m:?}, not {LOGIN_CREDENTIAL_MODE:?} — only a slot \
                 that completed its own login owns a refresh token that is safe to rotate"
            ))
        }
        None => {
            return skip(
                "no manifest.toml, so this slot has never been proven to own its login — \
                 refusing to refresh a credential whose provenance is unknown",
            )
        }
    }

    // 5. A proven, healthy own-login: the only thing that may be refreshed.
    if access_expired(facts, now_ms) {
        RotationAction::Refresh
    } else {
        // A probe would reuse the cached access token and rotate nothing.
        // Saying so beats a probe that pretends to have worked.
        skip(
            "access token is still valid, so a probe would refresh nothing — \
             it will roll on the next run after it lapses (~8h)",
        )
    }
}

/// Should the operator be warned that this login is running out? Pure.
pub fn expiry_warning(days: Option<f64>, warn_below_days: f64) -> Option<String> {
    match days {
        Some(d) if d <= 0.0 => Some("EXPIRED — sign this slot in".to_string()),
        Some(d) if d < warn_below_days => Some(format!(
            "{d:.1} days left — sign this slot in if the daily rotation cannot extend it"
        )),
        Some(_) => None,
        // No recorded expiry is itself worth surfacing: we cannot tell how long
        // it has, so it cannot be trusted to be fine.
        None => Some(
            "no recorded login expiry — cannot tell how long this login has".to_string(),
        ),
    }
}

/// Everything the operator-facing CLI needs about one slot. Hashes and dates
/// only — this struct is rendered to a terminal and serialized to JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlotReport {
    pub label: String,
    /// `None` = the machine default (env unset).
    pub config_dir: Option<String>,
    /// Derived, never guessed.
    pub keychain_service: String,
    /// From `manifest.toml`; `None` for the machine default and for any slot
    /// that has never been registered.
    pub credential_mode: Option<String>,
    /// Who this slot is logged in as — hashes only.
    pub identity: Option<IdentityFingerprint>,
    pub secret_sha8: Option<String>,
    pub health: SlotHealth,
    pub days_left: Option<f64>,
    pub access_expired: bool,
    pub subscription_type: Option<String>,
    pub rotation: RotationAction,
    pub warning: Option<String>,
    /// True for the machine default. The CLI paints this one differently and
    /// every mutating path refuses it.
    pub is_machine_default: bool,
    /// True on the ONE row that the machine-default store is currently signed
    /// in as — the operator's "live login". Set by [`mark_live_login`], never
    /// by [`build_slot_reports`], and never true on more than one row.
    #[serde(default)]
    pub is_live_login: bool,
    /// Set ONLY on the machine-default row, and only when a named account
    /// carries the same identity: the label that row folds into. The row stays
    /// in the JSON — rotation still has to reason about the default store, and
    /// hiding it from the machinery would hide a credential — but the CLI does
    /// not print it, because it is not a separate account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folds_into: Option<String>,
    /// Carried onto the live-login row from the machine-default row: the
    /// default store's OWN days-left. The two are separate OAuth grants for one
    /// account and they expire on separate clocks, so both numbers are real.
    /// **THE ABSOLUTE EXPIRY, BECAUSE `age` IS KILLED.**
    ///
    /// `days_left` is a function of `now_ms` and is therefore a CLOCK READ
    /// baked into a payload; a consumer holding it has a number that silently
    /// rots. These carry the instant instead and let the consumer subtract at
    /// render — the same rule that empties `age_seconds`, `days_left` and
    /// `refresh_age` out of the `accounts` shape. `days_left` stays for the
    /// existing readers; these are additive and omitted when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_expires_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_expires_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_store_access_expires_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_store_days_left: Option<f64>,
    /// Likewise the default store's own health, so a collapsed row can still
    /// say "your machine login itself is expired".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_store_health: Option<SlotHealth>,
}

/// The pure half of building a slot inventory: given each slot's already-read
/// facts and identity, decide health, clone-twins, rotation and warnings.
///
/// `raw` is `(label, config_dir, credential_mode, identity, facts)`. Split out
/// from the I/O so the classification of a whole fleet — including the
/// cross-slot clone detection, which is the part with real logic in it — is
/// testable without a keychain.
pub fn build_slot_reports(
    raw: Vec<(
        String,
        Option<String>,
        Option<String>,
        Option<IdentityFingerprint>,
        CredentialFacts,
    )>,
    now_ms: i64,
    warn_below_days: f64,
) -> Vec<SlotReport> {
    let manifests = (0..raw.len()).map(|_| None).collect();
    build_slot_reports_among(raw, manifests, now_ms, warn_below_days)
}

/// [`build_slot_reports`] with each slot's parsed `manifest.toml` (same order
/// as `raw`; `None` = none that parses), so a clone PAIR is told apart.
///
/// **R9.A2c: the board does not show the ORIGINAL of a clone pair as a
/// clone.** Two stores holding one session used to BOTH read `CloneOf` here,
/// while the runtime probe (R9.A2b) already barred only the copy — so the
/// board drew the original as a clone that placement was happily using. The
/// rule is the probe's own, [`is_the_copy`]: the machine default is always the
/// original; else the slot whose manifest declares a capture is the copy; else
/// the later-established one; undecidable → both stay `CloneOf` (the safe
/// side). The original is classified as if it had no twin.
pub fn build_slot_reports_among(
    raw: Vec<(
        String,
        Option<String>,
        Option<String>,
        Option<IdentityFingerprint>,
        CredentialFacts,
    )>,
    manifests: Vec<Option<SnapshotManifest>>,
    now_ms: i64,
    warn_below_days: f64,
) -> Vec<SlotReport> {
    assert_eq!(manifests.len(), raw.len(), "one manifest slot per raw row");
    // Peer table first: clone detection is a property of the whole fleet, so
    // every slot needs every other slot's secret hash before any of them can
    // be classified.
    let secrets: Vec<(String, Option<String>)> = raw
        .iter()
        .map(|(label, config_dir, _, _, facts)| {
            let display = if config_dir.is_none() {
                "the live login".to_string()
            } else {
                label.clone()
            };
            (display, facts.secret_sha8.clone())
        })
        .collect();

    raw.iter()
        .enumerate()
        .map(|(i, (label, config_dir, mode, identity, facts))| {
            let is_machine_default = config_dir.is_none();
            // R9.A2c: a twin makes THIS row `CloneOf` only when this row is the
            // copy ([`is_the_copy`]). The machine default is never the copy.
            let clone_of = secrets
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .filter(|(j, (l, s))| {
                    find_clone_of(facts.secret_sha8.as_deref(), [(l.as_str(), s.as_deref())])
                        .is_some()
                        && !is_machine_default
                        && is_the_copy(
                            manifests[i].as_ref(),
                            manifests[*j].as_ref(),
                            raw[*j].1.is_none(),
                        )
                })
                .map(|(_, (l, _))| l.clone())
                .next();
            let health = classify_slot(facts, mode.as_deref(), clone_of, now_ms);
            let days = days_left(facts, now_ms);
            let rotation =
                rotation_plan(is_machine_default, mode.as_deref(), &health, facts, now_ms);
            // Only a healthy login has a countdown worth warning about; every
            // other state's health line already says the whole story. The two
            // clone states are the exception — their dates look FINE, which is
            // exactly the trap, so they get an explicit warning of their own.
            let warning = match &health {
                SlotHealth::Live => expiry_warning(days, warn_below_days),
                SlotHealth::CloneOf { .. } | SlotHealth::Stranded => Some(
                    "the days-left figure for this slot is meaningless — it is a copy of \
                     another login, not one of its own. Replace it with `claude-accounts \
                     login <label>`."
                        .to_string(),
                ),
                _ => None,
            };
            SlotReport {
                label: label.clone(),
                config_dir: config_dir.clone(),
                keychain_service: keychain_service_for(config_dir.as_deref()),
                credential_mode: mode.clone(),
                identity: identity.clone(),
                secret_sha8: facts.secret_sha8.clone(),
                health,
                days_left: days,
                access_expired: access_expired(facts, now_ms),
                subscription_type: facts.subscription_type.clone(),
                access_expires_at_ms: facts.access_expires_at_ms,
                refresh_expires_at_ms: facts.refresh_expires_at_ms,
                live_store_access_expires_at_ms: None,
                rotation,
                warning,
                is_machine_default,
                // Filled by `mark_live_login`, which needs the whole fleet.
                is_live_login: false,
                folds_into: None,
                live_store_days_left: None,
                live_store_health: None,
            }
        })
        .collect()
}

/// Collapse the machine-default row into the named account it actually IS.
///
/// The operator's observation, verbatim (msg #928): *"I don't know why we have
/// numbered slots where one is default and then there's the other ones. It
/// doesn't make sense because the default is always going to be one of the
/// other two."* He is right. `default` is not an account — it is the store that
/// `claude` reads when `CLAUDE_CONFIG_DIR` is unset, and whatever is in it is
/// always one of the real accounts. So this marks WHICH real account it is and
/// tells the CLI to stop printing a second row for it.
///
/// Identity is the join key, not the secret hash. Two independent `claude auth
/// login`s for one account produce two different secrets and two different
/// expiries — that is the normal, healthy shape here (the machine default and
/// its slot are exactly that today) — but one `oauthAccount`. Joining on the
/// secret would fail on precisely the healthy case, and joining on it would
/// *also* be the clone-detector's job, which is a different question.
///
/// `pinned` is the label recorded by `set-live-login`. It is a TIE-BREAK ONLY,
/// consulted when two named slots hold the same account, and it is ignored
/// unless that slot's identity still matches the default store's. A stale
/// pointer can therefore never make this lie.
///
/// Nothing is hidden that has a credential of its own: when no named account
/// carries the default's identity, the default row stays visible and is itself
/// marked as the live login, so an unregistered live account can never vanish
/// from the listing.
pub fn mark_live_login(reports: &mut [SlotReport], pinned: Option<&str>) {
    let Some(default_idx) = reports.iter().position(|r| r.is_machine_default) else {
        return;
    };
    for r in reports.iter_mut() {
        r.is_live_login = false;
        r.folds_into = None;
        r.live_store_days_left = None;
        r.live_store_access_expires_at_ms = None;
        r.live_store_health = None;
    }
    let Some(default_identity) = reports[default_idx].identity.clone() else {
        // No readable `oauthAccount` in `~/.claude.json`: we cannot prove which
        // account the machine login is, so we claim nothing and leave its row
        // standing on its own.
        reports[default_idx].is_live_login = true;
        return;
    };

    let candidates: Vec<usize> = reports
        .iter()
        .enumerate()
        .filter(|(i, r)| {
            *i != default_idx
                && r.identity
                    .as_ref()
                    .is_some_and(|id| id.same_account_as(&default_identity))
        })
        .map(|(i, _)| i)
        .collect();

    if candidates.is_empty() {
        reports[default_idx].is_live_login = true;
        return;
    }

    let chosen = pinned
        .and_then(|p| candidates.iter().copied().find(|i| reports[*i].label == p))
        .unwrap_or(candidates[0]);

    let days = reports[default_idx].days_left;
    let expires = reports[default_idx].access_expires_at_ms;
    let health = reports[default_idx].health.clone();
    let label = reports[chosen].label.clone();
    reports[chosen].is_live_login = true;
    reports[chosen].live_store_days_left = days;
    reports[chosen].live_store_access_expires_at_ms = reports[default_idx].access_expires_at_ms;
    reports[chosen].live_store_access_expires_at_ms = expires;
    reports[chosen].live_store_health = Some(health);
    reports[default_idx].folds_into = Some(label);
}

/// The label recorded by `set-live-login`, if any. Non-fatal in every failure
/// mode: this is a hint, and [`mark_live_login`] re-derives the truth anyway.
pub fn read_live_login_pointer(data_dir: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(data_dir.join("accounts").join("live-login")).ok()?;
    let label = text.lines().next()?.trim().to_string();
    (!label.is_empty() && validate_account_name(&label).is_ok()).then_some(label)
}

/// Record which account the machine-default store was just signed in as.
pub fn write_live_login_pointer(data_dir: &std::path::Path, label: &str) -> Result<()> {
    validate_account_name(label)?;
    let dir = data_dir.join("accounts");
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Config(format!("creating {}: {e}", dir.display())))?;
    set_mode(&dir, 0o700);
    let path = dir.join("live-login");
    std::fs::write(&path, format!("{label}\n"))
        .map_err(|e| Error::Config(format!("writing {}: {e}", path.display())))?;
    set_mode(&path, 0o600);
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// RENAME and DELETE.
//
// Both are ordinary registry edits with one sharp edge: the macOS keychain
// service name is derived from the slot's DIRECTORY PATH
// ([`keychain_service_for`]), so moving the directory moves which keychain item
// the account's spawns will look for. A rename that only touched the registry
// would leave the credential behind under a service name nothing reads — the
// account would silently become "not signed in" with a perfectly good grant
// orphaned in the keychain.
//
// Neither operation ever runs `claude auth logout`, and neither can reach the
// machine-default keychain item: every service name here is derived from a path
// under `<data_dir>/accounts/`, and both paths are asserted against
// [`KEYCHAIN_SERVICE_BASE`] before anything is written or removed.
// ─────────────────────────────────────────────────────────────────────────

/// Why a rename may not proceed. Pure, so the refusals are tested without a
/// filesystem. `existing` is every label currently registered or on disk.
pub fn rename_refusal(old: &str, new: &str, existing: &[String]) -> Option<String> {
    if old == DEFAULT_ACCOUNT {
        return Some(format!(
            "{DEFAULT_ACCOUNT:?} is not an account you can rename — it is the machine \
             login itself (the store `claude` reads when CLAUDE_CONFIG_DIR is unset). \
             Rename the account it is signed in as instead."
        ));
    }
    if new == DEFAULT_ACCOUNT {
        return Some(format!(
            "{DEFAULT_ACCOUNT:?} is reserved for the machine login and cannot be used \
             as an account name."
        ));
    }
    if let Err(e) = validate_account_name(new) {
        return Some(format!("{e}"));
    }
    if old == new {
        return Some("that is already its name".to_string());
    }
    if !existing.iter().any(|l| l == old) {
        return Some(format!("there is no account {old:?}"));
    }
    if existing.iter().any(|l| l == new) {
        return Some(format!("{new:?} is already taken"));
    }
    None
}

/// **Delete's lookup, shared by the CLI (`threads accounts delete`) and the WS
/// arm (`accounts/{account}/delete`): why this delete may not proceed, or
/// `None`.** R9.A2c-r2 (inspect F1): since A2c the inventory has no
/// `SlotReport` for a DISABLED slot, and both arms used to compute `exists`
/// from those rows alone — so a disabled account could not be deleted at all.
/// CON-28 bars READING a disabled slot, not removing it. Here a disabled slot
/// the inventory skipped (unread) still EXISTS, and whether it is the live
/// login comes from the `set-live-login` pointer — a file, never the keychain.
/// `registry` must already carry `ensure_default` (the live-login guard).
pub fn delete_lookup(
    registry: &Registry,
    data_dir: &std::path::Path,
    home: &std::path::Path,
    label: &str,
    force: bool,
    now_ms: i64,
) -> Option<String> {
    let (mut reports, skipped) =
        collect_slot_inventory(registry, data_dir, home, now_ms, HEALTH_WARN_BELOW_DAYS);
    let pinned = read_live_login_pointer(data_dir);
    mark_live_login(&mut reports, pinned.as_deref());
    let row = reports.iter().find(|r| r.label == label);
    let exists = row.is_some() || skipped.iter().any(|(l, _)| l == label);
    let is_live = match row {
        Some(r) => r.is_live_login,
        None => pinned.as_deref() == Some(label),
    };
    delete_refusal(label, exists, is_live, force)
}

/// Why a delete may not proceed. Pure. `is_live_login` comes from
/// [`mark_live_login`]; `force` is the operator's explicit override.
pub fn delete_refusal(label: &str, exists: bool, is_live_login: bool, force: bool) -> Option<String> {
    if label == DEFAULT_ACCOUNT {
        return Some(format!(
            "{DEFAULT_ACCOUNT:?} is the machine login itself, not an account slot — there \
             is nothing here that could delete it, and nothing that should. Use \
             `set-live-login` to point it at a different account."
        ));
    }
    if let Err(e) = validate_account_name(label) {
        return Some(format!("{e}"));
    }
    if !exists {
        return Some(format!("there is no account {label:?}"));
    }
    if is_live_login && !force {
        return Some(format!(
            "{label:?} is the account your machine login is currently signed in as. \
             Deleting its slot would leave that account with only the machine store — \
             and no way for the daemon to spawn as it. Point the machine login at \
             another account first (`claude-accounts set-live-login <other>`), or pass \
             --force if you really mean to keep only the machine copy."
        ));
    }
    None
}

/// What a rename actually moved.
#[derive(Debug, Clone, PartialEq)]
pub struct RenameOutcome {
    pub old_label: String,
    pub new_label: String,
    pub old_dir: PathBuf,
    pub new_dir: PathBuf,
    pub old_service: String,
    pub new_service: String,
    /// True when a keychain item was copied to the new service name and the old
    /// one removed. False when there was nothing there to move, or when
    /// [`KeychainMode::Skip`] was asked for.
    pub credential_moved: bool,
}

/// Everything a partially-completed rename needs to put itself back, plus the
/// record of how far it got. Built from PRE-FLIGHT reads, so a rollback never
/// has to re-read state the failure may already have damaged.
struct RenameUnwind {
    data_dir: PathBuf,
    old_dir: PathBuf,
    new_dir: PathBuf,
    new_service: String,
    kc_account: String,
    registry_path: PathBuf,
    registry_before: Registry,
    registry_file_existed: bool,
    manifest_before: Option<String>,
    pointer_before: Option<String>,
    // How far the rename got.
    credential_copied: bool,
    dir_moved: bool,
    manifest_rewritten: bool,
    registry_saved: bool,
    pointer_rewritten: bool,
}

impl RenameUnwind {
    /// Undo every completed step, most recent first, so a failed rename leaves
    /// the slot exactly as it was found: ONE directory, ONE keychain item, ONE
    /// registry entry, all under the OLD name.
    ///
    /// Best-effort by construction. An unwind step that itself fails must not
    /// mask the original error, and every step here is idempotent, so the
    /// operator can simply re-run the rename.
    fn rollback(&self, keychain: KeychainMode) {
        if self.pointer_rewritten {
            if let Some(before) = &self.pointer_before {
                let _ = write_live_login_pointer(&self.data_dir, before);
            }
        }
        if self.registry_saved {
            if self.registry_file_existed {
                let _ = self.registry_before.save(&self.registry_path);
            } else {
                let _ = std::fs::remove_file(&self.registry_path);
            }
        }
        // The manifest is restored while the directory is still at its NEW
        // path; the directory move is undone directly after it.
        if self.manifest_rewritten {
            if let Some(text) = &self.manifest_before {
                let path = self.new_dir.join("manifest.toml");
                let _ = std::fs::write(&path, text);
                set_mode(&path, 0o600);
            }
        }
        if self.dir_moved {
            let _ = std::fs::rename(&self.new_dir, &self.old_dir);
        }
        // Last, because until the directory is back the OLD service name is
        // not the one a spawn would look for.
        if self.credential_copied && keychain != KeychainMode::Skip {
            let _ = delete_keychain_entry(&self.new_service, &self.kc_account);
        }
    }
}

/// Rename an account: its slot directory, its keychain item, its manifest and
/// its registry entry, in an order that can be unwound at every step.
///
/// ORDER IS THE SAFETY ARGUMENT.
///
/// 0. Every READ the rename depends on happens up front (registry, manifest,
///    live-login pointer). A read that fails has changed nothing, so the
///    failure modes below are all writes.
/// 1. The new keychain service name is *derived* from the target path, which
///    needs no filesystem change — so the credential is copied FIRST, while the
///    old one is still in place. A failure here has changed nothing.
/// 2. Then the directory moves.
/// 3. Then the manifest and the registry are rewritten to match, then the
///    live-login pointer if it named the old label.
/// 4. The OLD keychain item is removed LAST. Between (1) and (4) two items hold
///    one refresh token — a clone, briefly — but nothing refreshes in that
///    window because no `claude` is spawned anywhere in this function.
///
/// EVERY step from (1) on is undone by [`RenameUnwind::rollback`] if any later
/// step fails (F-2). The earlier version unwound only the directory move, so a
/// failure at the registry or pointer step returned `Err` having left a moved
/// directory, a rewritten manifest and BOTH keychain items in place — a
/// transient clone nothing would ever clean up.
///
/// The old item is deleted, not logged out: `security delete-generic-password`
/// is local and revokes nothing server-side.
pub fn rename_account(
    data_dir: &std::path::Path,
    old: &str,
    new: &str,
    keychain: KeychainMode,
) -> Result<RenameOutcome> {
    validate_account_name(old)?;
    validate_account_name(new)?;
    if new == DEFAULT_ACCOUNT || old == DEFAULT_ACCOUNT {
        return Err(Error::Config(format!(
            "{DEFAULT_ACCOUNT:?} is the machine login, not a renameable slot"
        )));
    }
    let accounts_root = data_dir.join("accounts");
    let old_dir = accounts_root.join(old);
    let new_dir = accounts_root.join(new);
    if !old_dir.is_dir() {
        return Err(Error::Config(format!(
            "no slot directory at {} — nothing to rename",
            old_dir.display()
        )));
    }
    if new_dir.exists() {
        return Err(Error::Config(format!(
            "{} already exists — pick another name",
            new_dir.display()
        )));
    }

    let old_str = old_dir.to_string_lossy().to_string();
    let new_str = new_dir.to_string_lossy().to_string();
    let old_service = keychain_service_for(Some(&old_str));
    let new_service = keychain_service_for(Some(&new_str));
    // Cannot happen (both are paths under `accounts/`), asserted anyway: this
    // is the one mistake that would put a slot's credential on top of the
    // operator's live login.
    if old_service == KEYCHAIN_SERVICE_BASE || new_service == KEYCHAIN_SERVICE_BASE {
        return Err(Error::Config(
            "refusing: a slot resolved to the machine-default keychain service".into(),
        ));
    }

    // 0. PRE-FLIGHT: every read the rename depends on, before the first
    //    mutation. A failure here has changed nothing, which is why the
    //    unwind below only ever has writes to undo.
    let kc_account = keychain_account();
    let registry_path = data_dir.join("accounts.toml");
    let registry_file_existed = registry_path.exists();
    let registry_before = Registry::load(&registry_path)?;
    let mut done = RenameUnwind {
        data_dir: data_dir.to_path_buf(),
        old_dir: old_dir.clone(),
        new_dir: new_dir.clone(),
        new_service: new_service.clone(),
        kc_account: kc_account.clone(),
        registry_path: registry_path.clone(),
        registry_before: registry_before.clone(),
        registry_file_existed,
        manifest_before: std::fs::read_to_string(old_dir.join("manifest.toml")).ok(),
        pointer_before: read_live_login_pointer(data_dir),
        credential_copied: false,
        dir_moved: false,
        manifest_rewritten: false,
        registry_saved: false,
        pointer_rewritten: false,
    };

    // 1. Move the credential, while the old name is still intact.
    if keychain == KeychainMode::Auto && cfg!(target_os = "macos") {
        if keychain_entry_exists(&old_service, &kc_account) {
            clone_keychain_entry(&old_service, &new_service, &kc_account)
                .map_err(|e| Error::Config(format!("copying the keychain item: {e}")))?;
            done.credential_copied = true;
        }
    }

    // 2. Move the directory.
    if let Err(e) = std::fs::rename(&old_dir, &new_dir) {
        done.rollback(keychain);
        return Err(Error::Config(format!(
            "moving {} → {}: {e}",
            old_dir.display(),
            new_dir.display()
        )));
    }
    done.dir_moved = true;

    // 3. The manifest describes the slot by path; rewrite it or it lies.
    let manifest_path = new_dir.join("manifest.toml");
    if let Some(text) = done.manifest_before.clone() {
        if let Ok(mut m) = toml::from_str::<SnapshotManifest>(&text) {
            m.label = new.to_string();
            // `source_*` describe where this slot's login LIVES for an
            // own-login (the slot is its own source); for a capture they name
            // the store it was copied from, which the rename does not change.
            if m.source_config_dir == old_str {
                m.source_config_dir = new_str.clone();
            }
            if m.source_config_json == format!("{old_str}/.claude.json") {
                m.source_config_json = format!("{new_str}/.claude.json");
            }
            m.spawn_env = format!("CLAUDE_CONFIG_DIR={new_str}");
            m.keychain_service = new_service.clone();
            let rendered = match toml::to_string_pretty(&m) {
                Ok(t) => t,
                Err(e) => {
                    done.rollback(keychain);
                    return Err(Error::Config(format!("rendering the renamed manifest: {e}")));
                }
            };
            if let Err(e) = std::fs::write(&manifest_path, rendered) {
                done.rollback(keychain);
                return Err(Error::Config(format!(
                    "writing {}: {e}",
                    manifest_path.display()
                )));
            }
            done.manifest_rewritten = true;
            set_mode(&manifest_path, 0o600);
        }
    }

    // 4. The registry.
    let mut registry = registry_before;
    registry.accounts.remove(old);
    registry.add(new, Some(new_str.clone()));
    if let Err(e) = registry.save(&registry_path) {
        done.rollback(keychain);
        return Err(e);
    }
    done.registry_saved = true;

    // 5. The pointer, if it named the old label.
    if done.pointer_before.as_deref() == Some(old) {
        if let Err(e) = write_live_login_pointer(data_dir, new) {
            done.rollback(keychain);
            return Err(e);
        }
        done.pointer_rewritten = true;
    }

    // 6. Only now drop the old keychain item.
    if done.credential_copied {
        if let Err(e) = delete_keychain_entry(&old_service, &kc_account) {
            done.rollback(keychain);
            return Err(Error::Config(format!(
                "removing the old keychain item: {e}"
            )));
        }
    }

    Ok(RenameOutcome {
        old_label: old.to_string(),
        new_label: new.to_string(),
        old_dir,
        new_dir,
        old_service,
        new_service,
        credential_moved: done.credential_copied,
    })
}

/// What a delete removed.
#[derive(Debug, Clone, PartialEq)]
pub struct DeleteOutcome {
    pub label: String,
    pub dir: PathBuf,
    pub service: String,
    pub dir_removed: bool,
    pub keychain_removed: bool,
    pub registry_removed: bool,
}

/// Delete an account slot: its keychain item, its directory, its registry
/// entry. Local only — nothing is revoked server-side, and no `logout` verb is
/// ever invoked, so every other copy of that account keeps working.
///
/// The caller owns the "are you sure?" and the live-login refusal
/// ([`delete_refusal`]); this function is the mechanism.
pub fn delete_account(
    data_dir: &std::path::Path,
    label: &str,
    keychain: KeychainMode,
) -> Result<DeleteOutcome> {
    validate_account_name(label)?;
    if label == DEFAULT_ACCOUNT {
        return Err(Error::Config(format!(
            "{DEFAULT_ACCOUNT:?} is the machine login, not a deletable slot"
        )));
    }
    let accounts_root = data_dir.join("accounts");
    let dir = accounts_root.join(label);
    let dir_str = dir.to_string_lossy().to_string();
    let service = keychain_service_for(Some(&dir_str));
    if service == KEYCHAIN_SERVICE_BASE {
        return Err(Error::Config(
            "refusing: the slot resolved to the machine-default keychain service".into(),
        ));
    }
    // The path must be exactly one level under the accounts root. A label that
    // survived `validate_account_name` cannot contain a separator, so this is
    // belt-and-braces against a future loosening of that rule.
    if dir.parent() != Some(accounts_root.as_path()) {
        return Err(Error::Config(format!(
            "refusing: {} is not directly under {}",
            dir.display(),
            accounts_root.display()
        )));
    }

    let kc_account = keychain_account();
    let mut keychain_removed = false;
    if keychain == KeychainMode::Auto
        && cfg!(target_os = "macos")
        && keychain_entry_exists(&service, &kc_account)
    {
        delete_keychain_entry(&service, &kc_account)
            .map_err(|e| Error::Config(format!("removing the keychain item: {e}")))?;
        keychain_removed = true;
    }

    let dir_removed = if dir.is_dir() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| Error::Config(format!("removing {}: {e}", dir.display())))?;
        true
    } else {
        false
    };

    let registry_path = data_dir.join("accounts.toml");
    let mut registry = Registry::load(&registry_path)?;
    let registry_removed = registry.accounts.remove(label).is_some();
    if registry_removed {
        registry.save(&registry_path)?;
    }

    if read_live_login_pointer(data_dir).as_deref() == Some(label) {
        let _ = std::fs::remove_file(accounts_root.join("live-login"));
    }

    Ok(DeleteOutcome {
        label: label.to_string(),
        dir,
        service,
        dir_removed,
        keychain_removed,
        registry_removed,
    })
}

/// Does a keychain item exist? Metadata only — the secret is never retrieved.
///
/// **R9.F2h (CON-28):** under the process-wide test seam
/// ([`set_keychain_entry_exists_for_test`]) this asks the FIXTURE and never
/// spawns `security`. With no seam installed (production) it is the real
/// metadata query, behind the CON-28 tripwire. The seam is separate from the
/// credential reader ([`set_keychain_reader_for_test`]) on purpose: an
/// existence check is not a read of the store's facts, and tests that count
/// those reads (CON-28's "a disabled slot is not read") must not see it.
pub fn keychain_entry_exists(service: &str, account: &str) -> bool {
    let over = keychain_exists_override().read().unwrap().clone();
    keychain_entry_exists_with(over.as_ref(), service, account, real_keychain_entry_exists)
}

/// How a test answers "does this keychain item exist?": `(service, account)`.
pub type KeychainExists = std::sync::Arc<dyn Fn(&str, &str) -> bool + Send + Sync>;

fn keychain_exists_override() -> &'static std::sync::RwLock<Option<KeychainExists>> {
    static R: std::sync::OnceLock<std::sync::RwLock<Option<KeychainExists>>> =
        std::sync::OnceLock::new();
    R.get_or_init(|| std::sync::RwLock::new(None))
}

/// Replace the keychain ITEM operations — the existence check, and so the
/// delete and rename doors' delete/clone of an item, which then do nothing —
/// for this whole PROCESS. For TESTS only (R9.F2h); `None` restores
/// production. `tests/support/keychain.rs` installs it with the reader.
#[doc(hidden)]
pub fn set_keychain_entry_exists_for_test(exists: Option<KeychainExists>) {
    *keychain_exists_override().write().unwrap() = exists;
}

/// [`keychain_entry_exists`]'s decision, with the real query injected so the
/// production arm is testable without a keychain.
fn keychain_entry_exists_with(
    over: Option<&KeychainExists>,
    service: &str,
    account: &str,
    real: impl FnOnce(&str, &str) -> bool,
) -> bool {
    match over {
        Some(exists) => exists(service, account),
        None => real(service, account),
    }
}

/// Is the item seam installed? (R9.F2h: the item WRITES — delete, clone — act
/// on the fixture, i.e. do nothing, while it is.)
fn keychain_override_installed() -> bool {
    keychain_exists_override().read().unwrap().is_some()
}

#[cfg(target_os = "macos")]
fn real_keychain_entry_exists(service: &str, account: &str) -> bool {
    real_keychain_tripwire(service);
    use std::process::{Command, Stdio};
    Command::new("/usr/bin/security")
        .args(["find-generic-password", "-a", account, "-s", service])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
fn real_keychain_entry_exists(_service: &str, _account: &str) -> bool {
    false
}

/// Remove a keychain item. LOCAL ONLY: this deletes a copy of a token from this
/// machine's keychain. It is not a logout — nothing is revoked server-side and
/// no other copy of that grant is affected.
///
/// Refuses the machine-default service outright. That item is the operator's
/// live login and nothing in this crate has a reason to delete it.
#[cfg(target_os = "macos")]
pub fn delete_keychain_entry(service: &str, account: &str) -> std::result::Result<(), String> {
    use std::process::{Command, Stdio};
    if service == KEYCHAIN_SERVICE_BASE {
        return Err(format!(
            "refusing to delete {KEYCHAIN_SERVICE_BASE:?} — that is the machine login"
        ));
    }
    if keychain_override_installed() {
        return Ok(()); // R9.F2h: the fixture holds no real item.
    }
    real_keychain_tripwire(service);
    let status = Command::new("/usr/bin/security")
        .args(["delete-generic-password", "-a", account, "-s", service])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("running security: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("security delete-generic-password failed ({status})"))
    }
}

#[cfg(not(target_os = "macos"))]
pub fn delete_keychain_entry(service: &str, _account: &str) -> std::result::Result<(), String> {
    if service == KEYCHAIN_SERVICE_BASE {
        return Err("refusing to delete the machine login's keychain item".to_string());
    }
    Err("keychain deletion is macOS-only".into())
}

/// Read one store's non-secret facts from the macOS keychain. Read-only:
/// `find-generic-password` does not alter the item (its `mdat` is unchanged),
/// and the secret is parsed in-process and dropped — never printed, logged,
/// written, or placed on an argv.
#[cfg(target_os = "macos")]
pub fn read_credential_facts(service: &str, account: &str) -> CredentialFacts {
    real_keychain_tripwire(service);
    use std::process::{Command, Stdio};
    let out = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-a", account, "-s", service, "-w"])
        .stderr(Stdio::null())
        .output();
    let Ok(out) = out else {
        return CredentialFacts::default();
    };
    if !out.status.success() {
        // No such item. Distinct from "an item holding nothing".
        return CredentialFacts::default();
    }
    let blob = String::from_utf8_lossy(&out.stdout).to_string();
    credential_facts_from_blob(&blob)
}

#[cfg(not(target_os = "macos"))]
pub fn read_credential_facts(service: &str, _account: &str) -> CredentialFacts {
    real_keychain_tripwire(service);
    // Plaintext-mode installs keep the blob in `<config_dir>/.credentials.json`;
    // this tool is macOS-only and says so at its entry point.
    CredentialFacts::default()
}

/// **CON-28 tripwire (R9.F2d): no threads TEST reads the real keychain.**
/// Is this process a cargo test harness — a unit-test or integration-test
/// binary, which cargo writes as `<target>/<profile>/deps/<name>-<16 hex>`?
/// The daemon never runs from there (`target/<profile>/threads`, or a copy in
/// the build store), so in production this is `false` and the real reader is
/// untouched. Decided once per process.
#[doc(hidden)]
pub fn real_keychain_tripwire_armed() -> bool {
    static ARMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ARMED.get_or_init(|| {
        std::env::current_exe()
            .map(|p| is_cargo_test_harness_exe(&p))
            .unwrap_or(false)
    })
}

/// The path predicate under [`real_keychain_tripwire_armed`].
#[doc(hidden)]
pub fn is_cargo_test_harness_exe(exe: &std::path::Path) -> bool {
    let in_deps = exe
        .parent()
        .and_then(|d| d.file_name())
        .map_or(false, |n| n == "deps");
    let hashed = exe
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.rsplit_once('-'))
        .map_or(false, |(_, h)| h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit()));
    in_deps && hashed
}

/// The real reader was reached. In a test harness that is a CON-28 breach —
/// the test forgot `tests/support/keychain.rs` — so stop the whole process
/// BEFORE any `security` spawn: an abort fails the test binary however deep
/// in a spawned task the read happened, where a panic could be swallowed.
fn real_keychain_tripwire(service: &str) {
    if real_keychain_tripwire_armed() {
        // `writeln!` on the raw handle, not `eprintln!`: the test harness
        // captures the macro's output per test and an abort discards it.
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr(),
            "CON-28 tripwire: the REAL keychain reader was reached in a test process \
             (service {service:?}). Install the fixture: tests/support/keychain.rs \
             `keychain::install()` (R9.F2d). Aborting before any keychain read."
        );
        std::process::abort();
    }
}

/// Build the full slot inventory: every registered account plus every slot
/// directory on disk that is not registered yet, with the machine default
/// always included so clone detection has the live login to compare against.
///
/// Read-only end to end. It opens `.claude.json` files, reads `manifest.toml`
/// files, and asks `security` for credential blobs. It never invokes `claude`.
pub fn collect_slot_reports(
    registry: &Registry,
    data_dir: &std::path::Path,
    home: &std::path::Path,
    now_ms: i64,
    warn_below_days: f64,
) -> Vec<SlotReport> {
    collect_slot_inventory(registry, data_dir, home, now_ms, warn_below_days).0
}

/// **The labels whose account is DISABLED (CON-28), read from the threads
/// store in `data_dir`** — the `account_disabled` rows the operator's own act
/// wrote, which is what `Budgets::is_disabled` reads in the daemon. Read-only
/// (`Store::open_read_only`): it never creates or migrates a store. No store,
/// or one that cannot be read, is an empty set — the same "absence is not a
/// fault" rule `Budgets::pool_share` applies (CON-5).
pub fn disabled_labels(data_dir: &std::path::Path) -> std::collections::BTreeSet<String> {
    let db = data_dir.join("threads.db");
    if !db.is_file() {
        return Default::default();
    }
    match crate::store::Store::open_read_only(&db).and_then(|s| s.list_disabled_accounts()) {
        Ok(rows) => rows.into_iter().map(|(account, _, _)| account).collect(),
        Err(e) => {
            tracing::debug!(error = %e, "disabled accounts unreadable; reading every slot");
            Default::default()
        }
    }
}

/// How the slot inventory reads one keychain store: `(service, account)` →
/// facts. [`read_credential_facts`] in production.
pub type KeychainReader = std::sync::Arc<dyn Fn(&str, &str) -> CredentialFacts + Send + Sync>;

fn keychain_reader_override() -> &'static std::sync::RwLock<Option<KeychainReader>> {
    static R: std::sync::OnceLock<std::sync::RwLock<Option<KeychainReader>>> =
        std::sync::OnceLock::new();
    R.get_or_init(|| std::sync::RwLock::new(None))
}

/// Replace the keychain read under [`collect_slot_reports`] — and so under
/// `health_json`, rotation's plan, delete's existence check and the board's
/// `LoginSlots` hydrate — for this whole PROCESS. For TESTS (R9.A2c), which
/// must never touch the keychain: once set, nothing in this process reads the
/// real one through the inventory. `None` restores production.
#[doc(hidden)]
pub fn set_keychain_reader_for_test(read: Option<KeychainReader>) {
    *keychain_reader_override().write().unwrap() = read;
}

fn inventory_read(service: &str, account: &str) -> CredentialFacts {
    let r = keychain_reader_override().read().unwrap().clone();
    match r {
        Some(r) => r(service, account),
        None => read_credential_facts(service, account),
    }
}

/// [`collect_slot_reports`], plus the slots it did NOT read because their
/// account is disabled — `(label, config_dir)`, sorted by label.
///
/// **R9.A2c (CON-28): A DISABLED SLOT IS NOT READ.** No keychain read, no
/// `.claude.json`, no manifest — and no `SlotReport`: an unread slot has no
/// health to classify, and any reading this inventory could draw for it
/// (`no-login` above all) would be a sign-in prompt for an account the
/// operator retired. It is not anyone's clone peer either. The board,
/// `health`, rotation's plan (the script reads `health --json`) and delete's
/// lookup all come through here; `health_json` gives each skipped slot a row
/// built from config alone ([`disabled_slot_row`]).
pub fn collect_slot_inventory(
    registry: &Registry,
    data_dir: &std::path::Path,
    home: &std::path::Path,
    now_ms: i64,
    warn_below_days: f64,
) -> (Vec<SlotReport>, Vec<(String, Option<String>)>) {
    let disabled = disabled_labels(data_dir);
    let kc_account = keychain_account();
    let accounts_root = data_dir.join("accounts");

    // Registered accounts, plus any slot dir that exists but was never
    // registered — an interrupted `--login` leaves exactly that, and a slot
    // invisible to this list is a slot the operator cannot see is a clone.
    let mut entries: BTreeMap<String, Option<String>> = registry
        .accounts
        .iter()
        .map(|(name, a)| (name.clone(), a.config_dir.clone()))
        .collect();
    entries
        .entry(DEFAULT_ACCOUNT.to_string())
        .or_insert(None);
    if let Ok(rd) = std::fs::read_dir(&accounts_root) {
        for e in rd.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            let Some(name) = e.file_name().to_str().map(str::to_string) else {
                continue;
            };
            // Only things that could BE an account. The accounts root also
            // holds working state — `.rotate.lock` is a directory, and
            // listing it as a slot with no credential is both noise and a
            // false "sign this in" instruction.
            //
            // `validate_account_name` now rejects the RESERVED names too, which
            // is what finally keeps `backups/` off the board. That directory
            // holds the live-login undo snapshots; it is not an account, it is
            // not in the pool, and the operator has never had a login there.
            if name.starts_with('.') || validate_account_name(&name).is_err() {
                continue;
            }
            // AND IT MUST LOOK LIKE A CONFIG DIR. This is the predicate
            // `scripts/claude-accounts` has documented at its own pool listing
            // for as long as it has had one — *"an account directory is one
            // holding a `.claude.json` — that is what makes it a config dir,
            // and it is what keeps `backups/` out of the listing without
            // hardcoding a name to skip"* — and this enumerator was the one
            // place that never implemented it. It is belt-and-braces alongside
            // the reserved-name rule above, and it is what generalises: any
            // future working directory under `accounts/` is excluded on its
            // shape, with nobody having to add it to a list.
            //
            // `manifest.toml` counts as well as `.claude.json`. An INTERRUPTED
            // `--login` leaves a manifest and no config json, and that slot must
            // stay visible — it is precisely the half-finished state the
            // operator needs to see in order to finish it.
            let looks_like_a_slot = e.path().join(".claude.json").exists()
                || e.path().join("manifest.toml").exists();
            if !looks_like_a_slot {
                continue;
            }
            entries
                .entry(name)
                .or_insert_with(|| Some(e.path().to_string_lossy().to_string()));
        }
    }

    let skipped: Vec<(String, Option<String>)> = entries
        .iter()
        .filter(|(l, _)| disabled.contains(*l))
        .map(|(l, d)| (l.clone(), d.clone()))
        .collect();
    entries.retain(|label, _| !disabled.contains(label));

    let (raw, manifests): (Vec<_>, Vec<_>) = entries
        .into_iter()
        .map(|(label, config_dir)| {
            let service = keychain_service_for(config_dir.as_deref());
            let manifest = config_dir.as_deref().and_then(read_slot_manifest);
            let mode = manifest.as_ref().map(|m| m.credential_mode.clone());
            // R9.T7: a dir with no `.claude.json` that declares a keychain
            // login is drawn from EMPTY facts — exactly what the runtime probe
            // classifies it from (it asks no store) — so board and placement
            // both read `NoLoginDeclared`. Every other slot is read as before.
            let facts = if declares_keychain_login(mode.as_deref())
                && !has_config_json(config_dir.as_deref())
            {
                CredentialFacts::default()
            } else {
                inventory_read(&service, &kc_account)
            };
            let identity = IdentityFingerprint::from_config_json(&config_json_for(
                config_dir.as_deref(),
                home,
            ))
            .ok()
            .flatten();
            ((label, config_dir, mode, identity, facts), manifest)
        })
        .unzip();

    (build_slot_reports_among(raw, manifests, now_ms, warn_below_days), skipped)
}

// ─────────────────────────────────────────────────────────────────────────
// R12.1 — AN ACCOUNT IS ITS EMAIL, AND THE IDENTITY GUARD
// (cauldron `docs/ACCOUNT-EMAIL-ID.md` §2.1, §3; substrate
// an internal report)
//
// The operator's point: "if we do a login (or set local) then we don't have to
// worry about making the mistake of setting, say, owner@example.com's login
// for the account bob-example-com." Credentials that authenticate as a
// different email than the account claims are refused.
//
// WHICH FIELD IS TRUSTED. Only the email the Anthropic SERVER returns for the
// credential stored in the slot's keychain item (`GET /api/oauth/profile`,
// `account.email`). `<slot>/.claude.json` `oauthAccount.emailAddress` is a
// plain file — a TTL cache of that same endpoint — and `claude auth status`
// prints exactly that cached field, so neither is trusted. The guard never
// reads the email out of the file at all; the file contributes only a hash
// to the BINDING, a cheap tripwire that voids a verified result when either
// the keychain item or the cached profile changes.
//
// The trusted read sits behind [`IdentitySource`] so tests inject a FAKE
// source; no test reaches a real slot, keychain item or server.
// ─────────────────────────────────────────────────────────────────────────

/// **A pool account's canonical id: its normalised email address** (doc §2.1).
///
/// Constructed only through [`AccountId::parse`] (or serde, which routes
/// through it). Normalisation is: trim; exactly one `@`; non-empty local part;
/// a domain containing an interior `.`; no whitespace; the WHOLE address
/// lower-cased. Plus-addressing is kept (`a+1@d` is a different login from
/// `a@d`) and dots are kept (stripping them is a Gmail-only rule). The display
/// form is the normalised form; there is no second spelling.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AccountId(String);

impl AccountId {
    /// Parse and normalise an email address. `Err` names what is wrong.
    pub fn parse(raw: &str) -> std::result::Result<Self, String> {
        let t = raw.trim();
        if t.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(format!("not an email address (contains whitespace): {t:?}"));
        }
        let mut parts = t.split('@');
        let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(format!("not an email address (need exactly one '@'): {t:?}"));
        };
        if local.is_empty() {
            return Err(format!("not an email address (empty local part): {t:?}"));
        }
        if domain.is_empty()
            || !domain.contains('.')
            || domain.starts_with('.')
            || domain.ends_with('.')
        {
            return Err(format!("not an email address (domain needs a '.'): {t:?}"));
        }
        Ok(Self(t.to_lowercase()))
    }

    /// The normalised address.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AccountId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for AccountId {
    type Error = String;
    fn try_from(s: String) -> std::result::Result<Self, String> {
        Self::parse(&s)
    }
}

impl From<AccountId> for String {
    fn from(id: AccountId) -> String {
        id.0
    }
}

/// `identity_state` (doc §3.2). Only `Verified` is placeable.
///
/// * `verified` — a trusted read returned this account's own email, and the
///   binding recorded with it still holds.
/// * `mismatch` — a trusted read returned a DIFFERENT email: the crossed login.
/// * `unreadable` — the trusted read could not answer (offline, token
///   rejected or expired, no credential, malformed answer). Never treated as
///   a pass, and never answered from the cached file.
/// * `unverified` — not yet read, or a verified result whose binding changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdentityState {
    Verified,
    Mismatch,
    Unreadable,
    Unverified,
}

impl IdentityState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Mismatch => "mismatch",
            Self::Unreadable => "unreadable",
            Self::Unverified => "unverified",
        }
    }
}

/// What the server says a slot's stored credential authenticates as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerIdentity {
    /// `account.email`, verbatim as the server returned it.
    pub email: String,
    /// `account.uuid`, when present.
    pub account_uuid: Option<String>,
}

/// **The trusted read.** Ask the SERVER which email the credential stored for
/// the slot at `config_dir` (`None` = the machine default) authenticates as.
/// `Err` carries a reason and becomes `unreadable`; an implementation must
/// never answer from `.claude.json` when the server cannot be asked.
pub trait IdentitySource: Send + Sync {
    fn authenticated_identity(
        &self,
        config_dir: Option<&str>,
    ) -> std::result::Result<ServerIdentity, String>;
}

/// The keychain half of the binding: an item's modification date, read as
/// METADATA ONLY (`security find-generic-password -a -s`, never `-w`/`-g`).
/// `None` = no such item.
pub trait KeychainMetadata: Send + Sync {
    fn mdat(&self, service: &str, account: &str) -> Option<String>;
}

/// The production [`KeychainMetadata`]: `/usr/bin/security
/// find-generic-password -a <account> -s <service>` with no secret flag. The
/// secret is never requested. Trips the CON-28 test tripwire before spawning.
pub struct SecurityKeychainMetadata;

impl KeychainMetadata for SecurityKeychainMetadata {
    fn mdat(&self, service: &str, account: &str) -> Option<String> {
        real_keychain_tripwire(service);
        #[cfg(target_os = "macos")]
        {
            use std::process::{Command, Stdio};
            let out = Command::new("/usr/bin/security")
                .args(["find-generic-password", "-a", account, "-s", service])
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            parse_keychain_mdat(&String::from_utf8_lossy(&out.stdout))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = account;
            None
        }
    }
}

/// Pull `mdat` out of `security find-generic-password`'s attribute listing,
/// whose line reads `    "mdat"<timedate>=0x…  "20261025004103Z\000"`. Pure.
/// `None` when the attribute is absent or `<NULL>`.
pub fn parse_keychain_mdat(attrs: &str) -> Option<String> {
    const KEY: &str = "\"mdat\"<timedate>=";
    let line = attrs.lines().map(str::trim_start).find(|l| l.starts_with(KEY))?;
    let value = &line[KEY.len()..];
    let start = value.find('"')? + 1;
    let rest = &value[start..];
    let end = rest.find('"')?;
    let v = rest[..end].trim_end_matches("\\000");
    (!v.is_empty()).then(|| v.to_string())
}

/// `sha8` of the `oauthAccount` value of a `.claude.json`, re-serialised
/// compactly. Any rewrite of the block (a fresh profile fetch included) moves
/// it, which only costs a re-verify. `None` when the file is missing,
/// unparseable or has no `oauthAccount`. The EMAIL inside is never read out.
pub fn oauth_account_sha8(config_json: &Path) -> Option<String> {
    let text = std::fs::read_to_string(config_json).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let oa = v.get("oauthAccount")?;
    Some(sha8(&serde_json::to_string(oa).ok()?))
}

/// **The cheap tripwire stored with every verified result** (doc §3.1): the
/// keychain item's mdat plus the sha8 of `.claude.json` `oauthAccount`. If
/// either changes, the verified result is void.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityBinding {
    pub keychain_mdat: Option<String>,
    pub oauth_account_sha8: Option<String>,
}

/// One account's identity verdict: `identity_state` + `identity_reason`, and
/// the binding a `verified` result was taken under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityVerdict {
    pub account: AccountId,
    pub identity_state: IdentityState,
    pub identity_reason: Option<String>,
    /// Set only on `verified`.
    pub binding: Option<IdentityBinding>,
}

impl IdentityVerdict {
    /// The state of an account nobody has asked the server about yet.
    pub fn unverified(account: AccountId) -> Self {
        Self {
            account,
            identity_state: IdentityState::Unverified,
            identity_reason: Some("identity not yet verified with the server".into()),
            binding: None,
        }
    }

    /// May a turn be placed on this account, as far as identity goes?
    /// Only on `verified`. (CON-28 is separate: a verified unpaid account is
    /// still not placeable.)
    pub fn is_placeable(&self) -> bool {
        self.identity_state == IdentityState::Verified
    }
}

/// **`accounts::IdentityGuard` — the one guard** (doc §3). Compares an
/// account's id with the email its stored credential authenticates as,
/// according to the server, and keeps the binding that voids the answer.
///
/// No call site yet beyond tests: R12.4 wires placement, boot and
/// attach/detach; R12.2 uses it for the rekey.
pub struct IdentityGuard {
    source: Box<dyn IdentitySource>,
    keychain: Box<dyn KeychainMetadata>,
    home: PathBuf,
}

impl IdentityGuard {
    /// `home` resolves the machine default's `.claude.json` (config dir `None`).
    pub fn new(
        source: Box<dyn IdentitySource>,
        keychain: Box<dyn KeychainMetadata>,
        home: PathBuf,
    ) -> Self {
        Self {
            source,
            keychain,
            home,
        }
    }

    /// The production guard: the server profile read and the metadata-only
    /// keychain reader.
    pub fn production(home: PathBuf) -> Self {
        Self::new(
            Box::new(ServerProfileSource::production()),
            Box::new(SecurityKeychainMetadata),
            home,
        )
    }

    /// The binding as it stands now for the slot at `config_dir`.
    pub fn binding_of(&self, config_dir: Option<&str>) -> IdentityBinding {
        IdentityBinding {
            keychain_mdat: self
                .keychain
                .mdat(&keychain_service_for(config_dir), &keychain_account()),
            oauth_account_sha8: oauth_account_sha8(&config_json_for(config_dir, &self.home)),
        }
    }

    /// **The full trusted read.** Asks the source, compares the normalised
    /// answer with `account`, and records the binding. The slot's
    /// `.claude.json` email is never consulted, so a stale file can neither
    /// pass a crossed login nor fail a good one.
    pub fn verify(&self, account: &AccountId, config_dir: Option<&str>) -> IdentityVerdict {
        let before = self.binding_of(config_dir);
        let verdict = |state: IdentityState, reason: String, binding: Option<IdentityBinding>| IdentityVerdict {
            account: account.clone(),
            identity_state: state,
            identity_reason: Some(reason),
            binding,
        };
        let got = match self.source.authenticated_identity(config_dir) {
            Ok(id) => id,
            Err(e) => {
                return verdict(
                    IdentityState::Unreadable,
                    format!("identity of {account} unreadable: {e}"),
                    None,
                )
            }
        };
        let got = match AccountId::parse(&got.email) {
            Ok(g) => g,
            Err(e) => {
                return verdict(
                    IdentityState::Unreadable,
                    format!("the server's answer for {account} is not an email: {e}"),
                    None,
                )
            }
        };
        if &got != account {
            return verdict(
                IdentityState::Mismatch,
                format!("credentials authenticate as {got}, not {account}"),
                None,
            );
        }
        // A login landing DURING the read would leave a verdict bound to
        // credentials it never saw. Re-read the binding; a change is not a pass.
        let after = self.binding_of(config_dir);
        if after != before {
            return verdict(
                IdentityState::Unverified,
                format!("the credential of {account} changed during verification; re-verify"),
                None,
            );
        }
        IdentityVerdict {
            account: account.clone(),
            identity_state: IdentityState::Verified,
            identity_reason: None,
            binding: Some(after),
        }
    }

    /// **The server's answer alone** (R12.5b): which account the credential
    /// stored for `config_dir` authenticates as, normalised. `Err` = the read
    /// could not answer (it never falls back to `.claude.json`). For a caller
    /// that must report "wanted X, got Y", or that has no wanted address and
    /// needs only a READABLE identity; [`Self::verify`] is the comparison.
    pub fn authenticated_as(&self, config_dir: Option<&str>) -> std::result::Result<AccountId, String> {
        let got = self.source.authenticated_identity(config_dir)?;
        AccountId::parse(&got.email).map_err(|e| format!("the server's answer is not an email: {e}"))
    }

    /// **The cheap check** (per placement): no server call. A `verified`
    /// verdict whose binding still holds stays `verified`; one whose binding
    /// changed becomes `unverified` and needs [`Self::verify`] before it is
    /// placeable again. Any other verdict is returned unchanged: only a full
    /// trusted read can clear it.
    pub fn check_binding(&self, prior: &IdentityVerdict, config_dir: Option<&str>) -> IdentityVerdict {
        if prior.identity_state != IdentityState::Verified {
            return prior.clone();
        }
        let now = self.binding_of(config_dir);
        if prior.binding.as_ref() == Some(&now) {
            return prior.clone();
        }
        let mut what = Vec::new();
        if let Some(b) = &prior.binding {
            if b.keychain_mdat != now.keychain_mdat {
                what.push("keychain item modified");
            }
            if b.oauth_account_sha8 != now.oauth_account_sha8 {
                what.push(".claude.json oauthAccount changed");
            }
        }
        IdentityVerdict {
            account: prior.account.clone(),
            identity_state: IdentityState::Unverified,
            identity_reason: Some(format!(
                "binding changed since verification ({}); needs a trusted read",
                if what.is_empty() { "no binding recorded".into() } else { what.join(", ") }
            )),
            binding: None,
        }
    }
}

/// An OAuth access token, held only in memory. `Debug` never prints it.
pub struct AccessToken {
    secret: String,
    /// `claudeAiOauth.expiresAt`, epoch ms.
    pub expires_at_ms: Option<i64>,
}

impl AccessToken {
    pub fn new(secret: String, expires_at_ms: Option<i64>) -> Self {
        Self {
            secret,
            expires_at_ms,
        }
    }
}

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccessToken")
            .field("secret", &"<redacted>")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

/// Take the access token out of a keychain credential blob. Pure. The error
/// never quotes the blob.
pub fn access_token_from_blob(blob: &str) -> std::result::Result<AccessToken, String> {
    let v: serde_json::Value =
        serde_json::from_str(blob.trim()).map_err(|_| "credential blob is not JSON".to_string())?;
    let oauth = v
        .get("claudeAiOauth")
        .ok_or_else(|| "credential has no claudeAiOauth block".to_string())?;
    let secret = oauth
        .get("accessToken")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "credential holds no access token".to_string())?;
    Ok(AccessToken::new(
        secret.to_string(),
        oauth.get("expiresAt").and_then(|x| x.as_i64()).filter(|n| *n > 0),
    ))
}

/// Parse the body of `GET /api/oauth/profile`: `account.email` (required) and
/// `account.uuid`. Pure.
pub fn parse_profile_response(body: &str) -> std::result::Result<ServerIdentity, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("profile answer is not JSON: {e}"))?;
    let account = v
        .get("account")
        .ok_or_else(|| "profile answer has no `account`".to_string())?;
    let email = account
        .get("email")
        .and_then(|x| x.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "profile answer has no `account.email`".to_string())?;
    Ok(ServerIdentity {
        email: email.to_string(),
        account_uuid: account.get("uuid").and_then(|x| x.as_str()).map(str::to_string),
    })
}

/// Reads one slot's access token. The production reader is the keychain.
pub type AccessTokenReader =
    Box<dyn Fn(Option<&str>) -> std::result::Result<AccessToken, String> + Send + Sync>;

/// **The production [`IdentitySource`]** (an internal report):
/// `GET <base_url>/api/oauth/profile` with `Authorization: Bearer <the slot's
/// access token>`, the same request Claude Code makes to fill `oauthAccount`.
///
/// The request is made by `/usr/bin/curl` (the crate has no HTTP client),
/// with the Authorization header fed on STDIN (`-H @-`): the token is never
/// on an argv, never logged, never written. Every failure — no token, an
/// expired token (the daemon never refreshes one: refresh rotates a token
/// claude owns), no network, a non-2xx answer, a malformed body — is an
/// `Err`, which the guard records as `unreadable`. There is no cache fallback.
pub struct ServerProfileSource {
    pub base_url: String,
    pub timeout_secs: u64,
    token: AccessTokenReader,
}

impl ServerProfileSource {
    pub const PROFILE_PATH: &'static str = "/api/oauth/profile";
    pub const PRODUCTION_BASE_URL: &'static str = "https://api.anthropic.com";

    /// The real thing: the slot's keychain token, the real server.
    pub fn production() -> Self {
        Self::with_token_reader(
            Self::PRODUCTION_BASE_URL.to_string(),
            Box::new(read_keychain_access_token),
        )
    }

    /// Any base URL, any token reader — how a test drives the real request
    /// path against a local listener without touching the keychain.
    pub fn with_token_reader(base_url: String, token: AccessTokenReader) -> Self {
        Self {
            base_url,
            timeout_secs: 10,
            token,
        }
    }
}

impl IdentitySource for ServerProfileSource {
    fn authenticated_identity(
        &self,
        config_dir: Option<&str>,
    ) -> std::result::Result<ServerIdentity, String> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let token = (self.token)(config_dir)?;
        if let Some(exp) = token.expires_at_ms {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            if exp <= now {
                return Err(format!(
                    "access token expired at {exp} ms; the daemon does not refresh tokens"
                ));
            }
        }
        let url = format!("{}{}", self.base_url.trim_end_matches('/'), Self::PROFILE_PATH);
        let timeout = self.timeout_secs.to_string();
        let mut child = Command::new("/usr/bin/curl")
            .args([
                "-q",
                "-sS",
                "--max-time",
                &timeout,
                "-H",
                "@-",
                "-H",
                "Cache-Control: no-cache",
                "-w",
                "\n%{http_code}",
                &url,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("running curl: {e}"))?;
        {
            let mut stdin = child.stdin.take().ok_or("curl stdin unavailable")?;
            stdin
                .write_all(format!("Authorization: Bearer {}\n", token.secret).as_bytes())
                .map_err(|e| format!("writing to curl: {e}"))?;
        }
        drop(token);
        let out = child
            .wait_with_output()
            .map_err(|e| format!("waiting for curl: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!(
                "profile request failed ({}): {}",
                out.status,
                err.lines().next().unwrap_or("").trim()
            ));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let (body, code) = text.rsplit_once('\n').unwrap_or(("", text.as_ref()));
        let code = code.trim();
        if !code.starts_with('2') {
            return Err(format!("profile request answered HTTP {code}"));
        }
        parse_profile_response(body)
    }
}

/// The production token reader: the slot's keychain item, parsed in-process
/// and dropped. Trips the CON-28 test tripwire before any `security` spawn.
fn read_keychain_access_token(config_dir: Option<&str>) -> std::result::Result<AccessToken, String> {
    let service = keychain_service_for(config_dir);
    real_keychain_tripwire(&service);
    #[cfg(target_os = "macos")]
    {
        use std::process::{Command, Stdio};
        let out = Command::new("/usr/bin/security")
            .args(["find-generic-password", "-a", &keychain_account(), "-s", &service, "-w"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|e| format!("running security: {e}"))?;
        if !out.status.success() {
            return Err(format!("no keychain item {service:?}"));
        }
        access_token_from_blob(&String::from_utf8_lossy(&out.stdout))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("the keychain token reader is macOS-only".into())
    }
}

// ── R12.10: A STALE FLAG NEVER SURVIVES A FRESH LOGIN OR A RE-ENABLE ─────────
//
// Operator ruling r41-2 (after he re-enabled cc2@example.com and found the
// board still calling it unavailable): every `wiped` / `expired` / dead / auth
// flag on an account is RE-CHECKED IMMEDIATELY on his enable and on every
// login that passed the identity guard. A pass clears the flag and says so,
// naming the evidence; a fail keeps it with a fresh reason. A crossed or
// unreadable login clears nothing, and nothing here ever re-disables an
// account: his enable stands, and a failed re-check leaves the account
// enabled but unavailable with its reason.
//
// `frozen` and `unpaid` have no code representation of their own (R12.10
// report §0): FROZEN is the board's word for the expired/wiped reading, and
// unpaid is his disable (which only his enable clears) or the `billing` bar
// (which a login cannot test — a login proves the credential, not payment).

/// What set off a [`recheck_flags`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecheckTrigger {
    /// The operator's `accounts/{account}/enable`.
    Enable,
    /// A login (`claude-accounts login` / `set-live-login`) that PASSED the
    /// identity guard, reported through `accounts/{account}/recheck`.
    Login,
    /// A turn that COMPLETED on an account carrying a login flag (R12.5c):
    /// the success proves only that the slot's credential authenticates as
    /// SOMEONE, so the flags it would lift are lifted on this re-check alone.
    Turn,
}

impl RecheckTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enable => "enable",
            Self::Login => "login",
            Self::Turn => "turn",
        }
    }
}

/// The outcome of one [`recheck_flags`]. Every word is an
/// `account_unavailability` word (`login_wiped`, `login_dead`, `auth_failed`,
/// `billing`, …); `disabled` is never listed, because no re-check reads or
/// moves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FlagRecheck {
    pub account: String,
    pub trigger: RecheckTrigger,
    /// The trusted read's verdict; `None` when the slot was not read at all
    /// (the account is disabled).
    pub identity_state: Option<IdentityState>,
    pub identity_reason: Option<String>,
    /// The words the account carried before the re-check.
    pub before: Vec<String>,
    /// Words the re-check lifted, on the evidence in [`Self::evidence`].
    pub cleared: Vec<String>,
    /// Words the account still carries afterwards (kept or freshly found).
    pub kept: Vec<String>,
    /// What lifted the cleared words.
    pub evidence: Option<String>,
    /// Why the kept words stayed.
    pub reason: Option<String>,
    /// Set when the re-check did not run (a disabled account is never read).
    pub skipped: Option<String>,
}

/// How a test answers the trusted read: R12.1's real [`IdentityGuard`] over a
/// fake source and fake keychain metadata.
pub type IdentityGuardSeam = std::sync::Arc<IdentityGuard>;

fn identity_guard_override() -> &'static std::sync::RwLock<Option<IdentityGuardSeam>> {
    static R: std::sync::OnceLock<std::sync::RwLock<Option<IdentityGuardSeam>>> =
        std::sync::OnceLock::new();
    R.get_or_init(|| std::sync::RwLock::new(None))
}

/// Replace the guard [`recheck_flags`] asks, for this whole PROCESS. For TESTS
/// only (R12.10); `None` restores production. Without it a test that reaches
/// the trusted read trips the CON-28 tripwire and aborts.
#[doc(hidden)]
pub fn set_identity_guard_for_test(guard: Option<IdentityGuardSeam>) {
    *identity_guard_override().write().unwrap() = guard;
}

/// The guard every trusted read in this process uses (R12.5b): the test seam
/// when one is installed, else R12.1's production guard (`$HOME` for the
/// machine default). The daemon's own login path asks THIS, so it and
/// [`recheck_flags`] can never consult two different guards.
pub fn trusted_identity_guard() -> IdentityGuardSeam {
    if let Some(g) = identity_guard_override().read().unwrap().clone() {
        return g;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    std::sync::Arc::new(IdentityGuard::production(home))
}

/// The address a slot was signed in FOR: the first line of
/// `<config dir>/login-hint`, which `claude-accounts` and `add-account` write.
/// It is the WANTED identity, never the answer — the answer comes only from
/// the server. `None` = no slot dir or no hint.
pub fn recorded_login_address(config_dir: Option<&str>) -> Option<String> {
    let dir = config_dir?;
    let text = std::fs::read_to_string(Path::new(dir).join("login-hint")).ok()?;
    let line = text.lines().next()?.trim().to_string();
    (!line.is_empty()).then_some(line)
}

/// **The trusted read for one slot** (R12.1): the server's answer for the
/// slot's stored credential, compared with the address it was signed in for.
/// No recorded address, or one that is not an email, is `unreadable` — the
/// check cannot pass without knowing whom it is checking for, and the slot's
/// `.claude.json` is never consulted in its place.
fn trusted_identity(account: &str, config_dir: Option<&str>) -> (IdentityState, Option<String>) {
    let Some(addr) = recorded_login_address(config_dir) else {
        return (
            IdentityState::Unreadable,
            Some(format!(
                "no recorded address (login-hint) for {account}, so the server's answer \
                 cannot be checked against anything"
            )),
        );
    };
    let id = match AccountId::parse(&addr) {
        Ok(id) => id,
        Err(e) => {
            return (
                IdentityState::Unreadable,
                Some(format!("the recorded address for {account} is not an email: {e}")),
            )
        }
    };
    let seam = identity_guard_override().read().unwrap().clone();
    let verdict = match seam {
        Some(guard) => guard.verify(&id, config_dir),
        None => {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            IdentityGuard::production(home).verify(&id, config_dir)
        }
    };
    (verdict.identity_state, verdict.identity_reason)
}

fn unavailability_words(m: &crate::runtime::ThreadManager, account: &str) -> Vec<String> {
    m.account_unavailable_reasons(account)
        .unwrap_or_default()
        .into_iter()
        .filter(|w| w != "disabled")
        .collect()
}

fn account_row(m: &crate::runtime::ThreadManager, account: &str) -> Option<AccountState> {
    m.store
        .list_account_states()
        .unwrap_or_default()
        .into_iter()
        .find(|s| s.name == account)
}

/// **`recheck_flags(account)` — THE ONE RE-CHECK (R12.10).** Run as the last
/// act of the operator's enable and of every login that passed the identity
/// guard. One call, so a later wrapper (PS4's pending promise) wraps it whole.
///
/// 1. A DISABLED account is not read (CON-28) and nothing moves: a login never
///    clears `disabled`; only his enable does, and it runs this after.
/// 2. The trusted read (R12.1's [`IdentityGuard`], against the slot's recorded
///    address). Never the cached `.claude.json`.
/// 3. **Verified ⇒ the credential probe that sets each sign-in flag runs with
///    authority to lift** (`reconcile_sign_in_needed`: a Live reading lifts
///    `login_wiped` / `login_expired` / … and, R9.A3, `login_dead`). A login
///    bar (`auth_failed`) is lifted, and a `login_dead` the probe could not
///    reach is lifted on the server's word. `billing` stays: a login proves
///    the credential, not payment.
/// 4. **Anything else ⇒ nothing is lifted.** The probe runs ADD-ONLY
///    (`seed_sign_in_needed`, boot semantics), so a slot wiped while the
///    account was disabled is marked now rather than on the next sample. A
///    MISMATCH (the slot authenticates as someone else) is a positive fault and
///    is recorded as `login_dead` with that reason; an UNREADABLE answer adds
///    nothing, but a kept `login_dead` gets the fresh reason.
/// 5. The result is announced: `flags_cleared` (with the evidence) and/or
///    `flags_kept` (with the reason), and the pressure rows are re-stamped.
///
/// It never writes `account_disabled`: his re-enable is never overridden.
pub fn recheck_flags(
    m: &crate::runtime::ThreadManager,
    account: &str,
    trigger: RecheckTrigger,
) -> FlagRecheck {
    let before = unavailability_words(m, account);
    let mut out = FlagRecheck {
        account: account.to_string(),
        trigger,
        identity_state: None,
        identity_reason: None,
        before: before.clone(),
        cleared: Vec::new(),
        kept: before.clone(),
        evidence: None,
        reason: None,
        skipped: None,
    };
    if m.budgets.is_disabled(account) {
        out.skipped = Some(
            "the account is DISABLED: its slot is not read (CON-28), nothing was cleared, and \
             only the operator's enable lifts a disable"
                .into(),
        );
        tracing::info!(%account, trigger = trigger.as_str(), "flag re-check skipped: disabled");
        return out;
    }
    let dir = m.pool().login_dir(account).map(str::to_string);
    let (state, why) = trusted_identity(account, dir.as_deref());
    out.identity_state = Some(state);
    out.identity_reason = why.clone();

    if state == IdentityState::Verified {
        m.reconcile_sign_in_needed();
        if let Some(row) = account_row(m, account) {
            if row.limit_until.is_some() && row.limit_axis.as_deref() == Some("login") {
                if let Err(e) = m.store.set_account_limit(account, None) {
                    tracing::warn!(%account, error = %e, "re-check: lifting the login bar failed");
                }
            }
        }
        let now = unavailability_words(m, account);
        let sign_in = now.iter().any(|w| w.starts_with("login_") && w != "login_dead");
        if !sign_in && now.iter().any(|w| w == "login_dead") {
            if let Err(e) = m.store.set_account_health(account, None) {
                tracing::warn!(%account, error = %e, "re-check: lifting login_dead failed");
            }
        }
        out.evidence = Some(format!(
            "identity verified by the server as the slot's recorded address, and the slot's \
             credential probe re-read on {}",
            trigger.as_str()
        ));
    } else {
        m.seed_sign_in_needed();
        let reason = why.unwrap_or_else(|| format!("identity {}", state.as_str()));
        let row = account_row(m, account);
        let since = row.as_ref().and_then(|r| r.dead_since.clone());
        let fresh = format!("kept on {} re-check: {reason}", trigger.as_str());
        let write = match (state, since) {
            (IdentityState::Mismatch, s) => Some(s.unwrap_or_else(crate::store::now_rfc3339)),
            (_, Some(s)) => Some(s),
            _ => None,
        };
        if let Some(s) = write {
            if let Err(e) = m.store.set_account_health(account, Some((&s, &fresh))) {
                tracing::warn!(%account, error = %e, "re-check: recording the reason failed");
            }
        }
        out.reason = Some(reason);
    }

    let after = unavailability_words(m, account);
    out.cleared = before.iter().filter(|w| !after.contains(w)).cloned().collect();
    out.kept = after.clone();
    if out.reason.is_none() && !after.is_empty() {
        out.reason = Some(if after.iter().any(|w| w == "billing") {
            "a login proves the credential, not payment: a billing bar runs out on its own timer"
                .to_string()
        } else {
            format!("the slot still reads {}", after.join(", "))
        });
    }
    if !out.cleared.is_empty() {
        let detail = format!(
            "{account}: cleared {} on {} — evidence: {}",
            out.cleared.join(", "),
            trigger.as_str(),
            out.evidence.as_deref().unwrap_or("")
        );
        tracing::info!(%account, cleared = ?out.cleared, "{detail}");
        m.emit_info("accounts", "flags_cleared", Some(account), Some(detail));
        m.account_picture_changed_for("flags_cleared", Some(account));
    }
    if !out.kept.is_empty() {
        let detail = format!(
            "{account}: kept {} on {} — {}",
            out.kept.join(", "),
            trigger.as_str(),
            out.reason.as_deref().unwrap_or("")
        );
        tracing::warn!(%account, kept = ?out.kept, "{detail}");
        m.emit_info("accounts", "flags_kept", Some(account), Some(detail));
    }
    m.emit_health_fact(account);
    m.produce_pressure();
    out
}

/// **R12.10 guards** — a stale flag never survives a fresh login or a
/// re-enable; a crossed or unreadable login never clears one; his re-enable is
/// never overridden. Every slot is read through the injectable probe and every
/// identity through R12.1's real [`IdentityGuard`] over a fake source: no
/// keychain, no network. Forcings and their red runs: an internal report §5.
#[cfg(test)]
mod flag_recheck_tests {
    use super::*;
    use crate::runtime::ThreadManager;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    /// The identity seam is process-wide: these tests take turns.
    static SERIAL: Mutex<()> = Mutex::new(());

    const DIED: &str = "2026-01-01T00:00:00Z";
    const LATER: &str = "2099-01-01T00:00:00Z";

    /// The server, faked: what each slot dir's credential authenticates as.
    #[derive(Default)]
    struct Server {
        answers: Mutex<BTreeMap<String, std::result::Result<String, String>>>,
        hits: Mutex<usize>,
    }
    struct Src(Arc<Server>);
    impl IdentitySource for Src {
        fn authenticated_identity(
            &self,
            dir: Option<&str>,
        ) -> std::result::Result<ServerIdentity, String> {
            *self.0.hits.lock().unwrap() += 1;
            match self.0.answers.lock().unwrap().get(dir.unwrap_or("")) {
                Some(Ok(email)) => Ok(ServerIdentity { email: email.clone(), account_uuid: None }),
                Some(Err(e)) => Err(e.clone()),
                None => Err("no credential in this slot".into()),
            }
        }
    }
    struct Kc;
    impl KeychainMetadata for Kc {
        fn mdat(&self, _s: &str, _a: &str) -> Option<String> {
            Some("20261025000000Z".into())
        }
    }

    type Slots = Arc<Mutex<BTreeMap<String, SlotHealth>>>;
    type Reads = Arc<Mutex<BTreeMap<String, usize>>>;

    struct World {
        _tmp: tempfile::TempDir,
        m: ThreadManager,
        server: Arc<Server>,
        slots: Slots,
        reads: Reads,
    }

    impl World {
        /// `accounts`: `(label, Some(recorded address))`.
        fn new(accounts: &[(&str, Option<&str>)]) -> World {
            let tmp = tempfile::tempdir().unwrap();
            let mut map = BTreeMap::new();
            for (name, hint) in accounts {
                let d = tmp.path().join("accounts").join(name);
                std::fs::create_dir_all(&d).unwrap();
                if let Some(h) = hint {
                    std::fs::write(d.join("login-hint"), format!("{h}\n")).unwrap();
                }
                map.insert(name.to_string(), d.to_string_lossy().to_string());
            }
            let m = crate::build_manager(crate::Config {
                data_dir: Some(tmp.path().to_string_lossy().to_string()),
                accounts: map,
                calibrate: false,
                edge_poll: false,
                probe_interval_secs: 0,
                ..Default::default()
            })
            .unwrap();
            let slots: Slots = Arc::default();
            let reads: Reads = Arc::default();
            let (s, r) = (slots.clone(), reads.clone());
            m.set_slot_probe_for_test(Arc::new(move |label: &str, _dir: Option<&str>| {
                *r.lock().unwrap().entry(label.to_string()).or_default() += 1;
                s.lock().unwrap().get(label).cloned().unwrap_or(SlotHealth::Live)
            }));
            let server = Arc::new(Server::default());
            set_identity_guard_for_test(Some(Arc::new(IdentityGuard::new(
                Box::new(Src(server.clone())),
                Box::new(Kc),
                tmp.path().to_path_buf(),
            ))));
            World { _tmp: tmp, m, server, slots, reads }
        }
        fn dir(&self, label: &str) -> String {
            self.m.pool().login_dir(label).unwrap().to_string()
        }
        /// The server says `label`'s stored credential authenticates as `email`.
        fn serves(&self, label: &str, answer: std::result::Result<&str, &str>) {
            self.server
                .answers
                .lock()
                .unwrap()
                .insert(self.dir(label), answer.map(str::to_string).map_err(str::to_string));
        }
        fn slot_reads(&self, label: &str, h: SlotHealth) {
            self.slots.lock().unwrap().insert(label.to_string(), h);
        }
        fn reads_of(&self, label: &str) -> usize {
            self.reads.lock().unwrap().get(label).copied().unwrap_or(0)
        }
        fn hits(&self) -> usize {
            *self.server.hits.lock().unwrap()
        }
        fn words(&self, label: &str) -> Vec<String> {
            self.m.account_unavailable_reasons(label).unwrap_or_default()
        }
        fn row(&self, label: &str) -> Option<AccountState> {
            account_row(&self.m, label)
        }
        /// A dead login and a live login bar, both written long ago.
        fn stale_dead_and_bar(&self, label: &str) {
            self.m.store.set_account_health(label, Some((DIED, "OAuth token has expired"))).unwrap();
            self.m
                .store
                .set_account_limit(label, Some((LATER, Some("401"), Some("login"), None)))
                .unwrap();
            let w = self.words(label);
            assert!(
                w.contains(&"login_dead".to_string()) && w.contains(&"auth_failed".to_string()),
                "fixture: {label} is dead and login-barred: {w:?}"
            );
        }
        /// The slot is WIPED and the daemon knows it (as at boot).
        fn stale_wiped(&self, label: &str) {
            self.slot_reads(label, SlotHealth::Wiped);
            self.m.seed_sign_in_needed();
            assert!(
                self.words(label).contains(&"login_wiped".to_string()),
                "fixture: {label} is login_wiped: {:?}",
                self.words(label)
            );
        }
        fn disabled(&self, label: &str) -> bool {
            self.m.store.account_disabled_at(label).unwrap().is_some()
        }
    }

    fn infos(rx: &mut tokio::sync::broadcast::Receiver<crate::event::Push>, label: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        while let Ok(p) = rx.try_recv() {
            if let crate::event::Push::Info { domain, reason, account, detail, .. } = p {
                if domain == "accounts" && account.as_deref() == Some(label) {
                    out.push((reason, detail.unwrap_or_default()));
                }
            }
        }
        out
    }

    // ── (a) his re-enable re-checks: a good login clears every stale flag ───

    #[tokio::test]
    async fn a_reenable_with_a_good_login_clears_the_stale_flags() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let w = World::new(&[("cc2", Some("cc2@x.test")), ("cc1", Some("cc1@x.test"))]);
        w.stale_dead_and_bar("cc2");
        crate::server::route_request(&w.m, "accounts/cc2/disable", json!({})).await.unwrap();
        w.serves("cc2", Ok("CC2@x.test"));
        let mut rx = w.m.subscribe();

        crate::server::route_request(&w.m, "accounts/cc2/enable", json!({})).await.unwrap();

        assert!(w.hits() >= 1, "the enable asked the server");
        assert!(w.reads_of("cc2") >= 1, "the enable re-read the slot");
        assert_eq!(w.words("cc2"), Vec::<String>::new(), "no stale flag survives the re-enable");
        assert_eq!(w.row("cc2").and_then(|r| r.dead_since), None, "login_dead lifted");
        assert_eq!(w.row("cc2").and_then(|r| r.limit_until), None, "login bar lifted");
        assert!(!w.disabled("cc2"), "enabled");
        let said = infos(&mut rx, "cc2");
        let cleared = said.iter().find(|(r, _)| r == "flags_cleared");
        assert!(
            cleared.is_some_and(|(_, d)| d.contains("login_dead")
                && d.contains("auth_failed")
                && d.contains("evidence: identity verified by the server")),
            "the clear is announced with its evidence: {said:?}"
        );
    }

    /// Opposing arm of (a): a slot WIPED while disabled. The enable stands —
    /// the account stays enabled — and it is unavailable with its reason NOW,
    /// not on the next sample.
    #[tokio::test]
    async fn a_reenable_over_a_wiped_slot_stays_enabled_but_unavailable() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let w = World::new(&[("cc2", Some("cc2@x.test"))]);
        crate::server::route_request(&w.m, "accounts/cc2/disable", json!({})).await.unwrap();
        w.slot_reads("cc2", SlotHealth::Wiped);
        w.serves("cc2", Err("credential holds no access token"));
        let mut rx = w.m.subscribe();

        crate::server::route_request(&w.m, "accounts/cc2/enable", json!({})).await.unwrap();

        assert!(!w.disabled("cc2"), "his re-enable is never overridden");
        assert_eq!(w.words("cc2"), vec!["login_wiped".to_string()], "unavailable, with its reason");
        let said = infos(&mut rx, "cc2");
        assert!(
            said.iter().any(|(r, d)| r == "flags_kept" && d.contains("login_wiped")),
            "{said:?}"
        );
        assert!(!said.iter().any(|(r, _)| r == "flags_cleared"), "{said:?}");
    }

    /// A crossed slot on re-enable: kept unavailable as `login_dead` with the
    /// crossing as its reason — and still enabled.
    #[tokio::test]
    async fn a_reenable_over_a_crossed_slot_stays_enabled_and_names_the_crossing() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let w = World::new(&[("cc2", Some("cc2@x.test"))]);
        w.stale_dead_and_bar("cc2");
        crate::server::route_request(&w.m, "accounts/cc2/disable", json!({})).await.unwrap();
        w.serves("cc2", Ok("someone-else@x.test"));

        crate::server::route_request(&w.m, "accounts/cc2/enable", json!({})).await.unwrap();

        assert!(!w.disabled("cc2"), "his re-enable is never overridden");
        let words = w.words("cc2");
        assert!(words.contains(&"login_dead".to_string()) && words.contains(&"auth_failed".to_string()), "{words:?}");
        let row = w.row("cc2").unwrap();
        assert_eq!(row.dead_since.as_deref(), Some(DIED), "kept, not re-stamped");
        assert!(
            row.dead_reason.as_deref().is_some_and(|r| r.contains("kept on enable re-check")
                && r.contains("authenticate as someone-else@x.test")),
            "fresh reason: {:?}",
            row.dead_reason
        );
    }

    // ── (b) a fresh login over `wiped` clears it ────────────────────────────

    #[tokio::test]
    async fn b_a_verified_login_over_a_wiped_flag_clears_it() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let w = World::new(&[("cc1", Some("cc1@x.test"))]);
        w.stale_wiped("cc1");
        // The login landed: the slot now holds a live grant for cc1.
        w.slot_reads("cc1", SlotHealth::Live);
        w.serves("cc1", Ok("cc1@x.test"));
        let mut rx = w.m.subscribe();

        let out = crate::server::route_request(&w.m, "accounts/cc1/recheck", json!({})).await.unwrap();

        assert_eq!(out["identity_state"], "verified", "{out}");
        assert_eq!(out["cleared"], json!(["login_wiped"]), "{out}");
        assert_eq!(out["kept"], json!([]), "{out}");
        assert_eq!(w.words("cc1"), Vec::<String>::new());
        assert!(
            infos(&mut rx, "cc1")
                .iter()
                .any(|(r, d)| r == "flags_cleared" && d.contains("login_wiped") && d.contains("on login")),
            "announced"
        );
    }

    /// And `login_dead` + the login bar, on a login, the same way.
    #[tokio::test]
    async fn b_a_verified_login_clears_dead_and_the_login_bar_but_not_billing() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let w = World::new(&[("cc1", Some("cc1@x.test")), ("cc3", Some("cc3@x.test"))]);
        w.stale_dead_and_bar("cc1");
        w.m.store
            .set_account_limit("cc3", Some((LATER, Some("credit balance too low"), Some("credit"), None)))
            .unwrap();
        w.serves("cc1", Ok("cc1@x.test"));
        w.serves("cc3", Ok("cc3@x.test"));

        let one = recheck_flags(&w.m, "cc1", RecheckTrigger::Login);
        let three = recheck_flags(&w.m, "cc3", RecheckTrigger::Login);

        assert_eq!(w.words("cc1"), Vec::<String>::new(), "{one:?}");
        assert_eq!(three.kept, vec!["billing".to_string()], "a login proves the credential, not payment: {three:?}");
        assert!(three.reason.as_deref().is_some_and(|r| r.contains("not payment")), "{three:?}");
    }

    // ── (c) a crossed or invalid login never clears ─────────────────────────

    #[tokio::test]
    async fn c_a_crossed_login_keeps_the_wiped_flag_and_names_the_crossing() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let w = World::new(&[("cc1", Some("cc1@x.test"))]);
        w.stale_wiped("cc1");
        // The crossed grant is a perfectly live token — for someone else.
        w.slot_reads("cc1", SlotHealth::Live);
        w.serves("cc1", Ok("b@x.test"));
        let mut rx = w.m.subscribe();

        let out = recheck_flags(&w.m, "cc1", RecheckTrigger::Login);

        assert_eq!(out.identity_state, Some(IdentityState::Mismatch), "{out:?}");
        assert_eq!(out.cleared, Vec::<String>::new(), "a crossed login clears nothing: {out:?}");
        assert!(w.words("cc1").contains(&"login_wiped".to_string()), "{:?}", w.words("cc1"));
        assert!(w.words("cc1").contains(&"login_dead".to_string()), "the crossing is a fault");
        assert!(out.reason.as_deref().is_some_and(|r| r.contains("authenticate as b@x.test")), "{out:?}");
        assert!(!infos(&mut rx, "cc1").iter().any(|(r, _)| r == "flags_cleared"));
    }

    #[tokio::test]
    async fn c_an_unreadable_login_keeps_every_flag_and_adds_none() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let w = World::new(&[("cc1", Some("cc1@x.test")), ("cc4", Some("cc4@x.test"))]);
        w.stale_wiped("cc1");
        w.stale_dead_and_bar("cc1");
        w.slot_reads("cc1", SlotHealth::Live);
        w.serves("cc1", Err("profile request answered HTTP 401"));
        w.serves("cc4", Err("the profile server could not be reached"));

        let out = recheck_flags(&w.m, "cc1", RecheckTrigger::Login);
        let clean = recheck_flags(&w.m, "cc4", RecheckTrigger::Login);

        assert_eq!(out.identity_state, Some(IdentityState::Unreadable), "{out:?}");
        assert_eq!(out.cleared, Vec::<String>::new(), "{out:?}");
        for f in ["login_wiped", "login_dead", "auth_failed"] {
            assert!(w.words("cc1").contains(&f.to_string()), "{f} kept: {:?}", w.words("cc1"));
        }
        assert!(
            w.row("cc1").and_then(|r| r.dead_reason).is_some_and(|r| r.contains("HTTP 401")),
            "the kept dead flag carries the fresh reason"
        );
        // Absence of evidence is not a fault: an unreadable answer adds nothing.
        assert_eq!(w.words("cc4"), Vec::<String>::new(), "{clean:?}");
    }

    /// No recorded address: the check cannot pass, and the cached file is not
    /// consulted in its place — the server is not even asked.
    #[tokio::test]
    async fn c_no_recorded_address_is_unreadable_and_clears_nothing() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let w = World::new(&[("cc1", None)]);
        std::fs::write(
            Path::new(&w.dir("cc1")).join(".claude.json"),
            r#"{"oauthAccount":{"emailAddress":"cc1@x.test"}}"#,
        )
        .unwrap();
        w.stale_wiped("cc1");
        w.slot_reads("cc1", SlotHealth::Live);
        w.serves("cc1", Ok("cc1@x.test"));

        let out = recheck_flags(&w.m, "cc1", RecheckTrigger::Login);

        assert_eq!(out.identity_state, Some(IdentityState::Unreadable), "{out:?}");
        assert_eq!(w.hits(), 0, "nothing to compare the answer with: not asked");
        assert!(w.words("cc1").contains(&"login_wiped".to_string()));
    }

    // ── a login never touches a disabled account ────────────────────────────

    #[tokio::test]
    async fn a_login_recheck_on_a_disabled_account_reads_nothing_and_clears_nothing() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let w = World::new(&[("cc2", Some("cc2@x.test"))]);
        w.stale_dead_and_bar("cc2");
        w.m.store.set_account_disabled("cc2", true, Some("unpaid (CON-28)")).unwrap();
        w.serves("cc2", Ok("cc2@x.test"));
        let reads = w.reads_of("cc2");

        let out = crate::server::route_request(&w.m, "accounts/cc2/recheck", json!({})).await.unwrap();

        assert!(out["skipped"].as_str().is_some_and(|s| s.contains("DISABLED")), "{out}");
        assert!(w.disabled("cc2"), "a login never clears his disable");
        assert_eq!(w.reads_of("cc2"), reads, "a disabled slot is not read (CON-28)");
        assert_eq!(w.hits(), 0, "and not asked about");
        assert!(w.row("cc2").and_then(|r| r.dead_since).is_some(), "nothing cleared");
    }
}

#[cfg(test)]
mod tests {
    /// R9.F2d: the tripwire's predicate fires for a cargo test binary and for
    /// nothing the daemon runs as — both arms, so neither constant passes.
    #[test]
    fn tripwire_predicate_is_test_harness_only() {
        use std::path::Path;
        let yes = [
            "/t/r9-lane-b/debug/deps/keychain_tripwire-0123456789abcdef",
            "/t/target/release/deps/threads-fedcba9876543210",
        ];
        let no = [
            "/t/target/release/threads",
            "/t/target/debug/threads",
            "/Users/x/.cauldron/builds/66e9ac13428b-release/threads",
            "/t/target/release/deps/threads-notahexhash00000",
            "/t/target/release/deps/threads-0123456789abcde",
            "/t/target/release/xdeps/threads-0123456789abcdef",
        ];
        for p in yes {
            assert!(super::is_cargo_test_harness_exe(Path::new(p)), "{p}");
        }
        for p in no {
            assert!(!super::is_cargo_test_harness_exe(Path::new(p)), "{p}");
        }
        assert!(super::real_keychain_tripwire_armed(), "this unit-test binary is a harness");
    }

    use super::*;
    use serde_json::json;

    #[test]
    fn keychain_service_names_match_claude_code() {
        // Default config dir → bare service name.
        assert_eq!(keychain_service_for(None), "Claude Code-credentials");
        // CLAUDE_CONFIG_DIR set → `-<first 8 hex of sha256(dir)>` suffix.
        // Vectors precomputed with `printf %s <dir> | shasum -a 256`.
        assert_eq!(
            keychain_service_for(Some("/tmp/x")),
            "Claude Code-credentials-2e56aa36"
        );
        assert_eq!(
            keychain_service_for(Some("/Users/testuser/.threads/accounts/work")),
            "Claude Code-credentials-18316f4f"
        );
    }

    /// The bash side of `add-account --login` derives the same service name.
    /// Both read `scripts/login-golden-vectors.txt`; neither owns the truth.
    #[test]
    fn golden_vectors_match_the_shared_file() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("scripts")
            .join("login-golden-vectors.txt");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        let mut checked = 0;
        for line in text.lines() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (dir, want) = line
                .split_once('\t')
                .unwrap_or_else(|| panic!("golden vector is not TAB-separated: {line:?}"));
            assert_eq!(keychain_service_for(Some(dir)), want, "vector {dir:?}");
            checked += 1;
        }
        assert!(checked >= 4, "expected at least 4 golden vectors, saw {checked}");
    }

    #[test]
    fn sha8_is_the_first_eight_hex_of_sha256() {
        // `printf %s /tmp/x | shasum -a 256` → 2e56aa36...
        assert_eq!(sha8("/tmp/x"), "2e56aa36");
        assert_eq!(sha8("").len(), 8);
    }

    #[test]
    fn identity_fingerprint_hashes_and_never_leaks() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".claude.json");
        std::fs::write(
            &p,
            json!({
                "oauthAccount": {
                    "accountUuid": "acct-uuid-1",
                    "emailAddress": "someone@example.com",
                    "organizationUuid": "org-uuid-1",
                    "billingType": "stripe_subscription",
                },
                "unrelated": 1,
            })
            .to_string(),
        )
        .unwrap();
        let fp = IdentityFingerprint::from_config_json(&p).unwrap().unwrap();
        assert_eq!(fp.acct, sha8("acct-uuid-1"));
        assert_eq!(fp.email, sha8("someone@example.com"));
        assert_eq!(fp.org, sha8("org-uuid-1"));
        assert_eq!(fp.tier, "stripe_subscription");
        // The whole point: no raw value survives into the fingerprint.
        let rendered = format!("{fp:?}");
        for secret in ["acct-uuid-1", "someone@example.com", "org-uuid-1"] {
            assert!(!rendered.contains(secret), "{secret} leaked into {rendered}");
        }

        // No oauthAccount → Ok(None), not an error: a fresh slot looks like this.
        let empty = dir.path().join("empty.json");
        std::fs::write(&empty, "{}").unwrap();
        assert!(IdentityFingerprint::from_config_json(&empty).unwrap().is_none());
    }

    #[test]
    fn same_account_ignores_the_org() {
        // The real `secondary` case: same account + email, different org.
        let a = IdentityFingerprint {
            acct: "aaaa".into(),
            email: "bbbb".into(),
            org: "cccc".into(),
            tier: "stripe_subscription".into(),
        };
        let same_human_other_org = IdentityFingerprint {
            org: "dddd".into(),
            ..a.clone()
        };
        assert!(a.same_account_as(&same_human_other_org));
        let other = IdentityFingerprint {
            acct: "zzzz".into(),
            email: "yyyy".into(),
            ..a.clone()
        };
        assert!(!a.same_account_as(&other));
    }

    fn witness(acct: &str, secret: Option<&str>) -> SessionWitness {
        SessionWitness {
            identity: Some(IdentityFingerprint {
                acct: acct.into(),
                email: format!("{acct}-mail"),
                org: "org".into(),
                tier: "stripe_subscription".into(),
            }),
            secret: secret.map(|s| s.to_string()),
        }
    }

    #[test]
    fn distinctness_refuses_clones_and_same_account_grants() {
        let live = witness("live", Some("secretA"));

        // The good case.
        assert_eq!(
            verify_distinct(&live, &witness("other", Some("secretB"))),
            DistinctnessVerdict::Distinct
        );

        // A byte-identical secret is a CLONE — the `primary` failure mode.
        // Outranks same-account: it is the one that kills a working login.
        assert_eq!(
            verify_distinct(&live, &witness("live", Some("secretA"))),
            DistinctnessVerdict::SameSession
        );
        assert_eq!(
            verify_distinct(&live, &witness("other", Some("secretA"))),
            DistinctnessVerdict::SameSession
        );

        // A real second grant for the same human buys nothing: one pool.
        assert_eq!(
            verify_distinct(&live, &witness("live", Some("secretB"))),
            DistinctnessVerdict::SameAccount
        );

        // Nothing landed.
        assert_eq!(
            verify_distinct(&live, &witness("other", None)),
            DistinctnessVerdict::NoCredential
        );
        assert_eq!(
            verify_distinct(
                &live,
                &SessionWitness { identity: None, secret: Some("secretB".into()) }
            ),
            DistinctnessVerdict::NoCredential
        );

        // Only Distinct is registrable, and every verdict has a spoken reason.
        for v in [
            DistinctnessVerdict::Distinct,
            DistinctnessVerdict::NoCredential,
            DistinctnessVerdict::SameSession,
            DistinctnessVerdict::SameAccount,
        ] {
            assert_eq!(v.ok(), v == DistinctnessVerdict::Distinct);
            assert!(!v.reason().is_empty());
        }
    }

    #[test]
    fn login_slot_is_private_gitignored_and_never_the_default_service() {
        let data = tempfile::tempdir().unwrap();
        let slot = prepare_login_slot(data.path(), "work", false).unwrap();
        assert!(slot.dir.is_dir());
        assert!(!slot.preexisting);
        assert_eq!(slot.config_json, slot.dir.join(".claude.json"));
        // Derived, and never the machine default.
        assert_ne!(slot.keychain_service, KEYCHAIN_SERVICE_BASE);
        assert_eq!(
            slot.keychain_service,
            keychain_service_for(Some(&slot.dir.to_string_lossy()))
        );
        // Snapshots can hold plaintext credentials: force-ignored.
        assert_eq!(
            std::fs::read_to_string(data.path().join("accounts").join(".gitignore")).unwrap(),
            "*\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&slot.dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }

        // Bad labels cannot escape the accounts root.
        assert!(prepare_login_slot(data.path(), "../evil", false).is_err());

        // Re-preparing an EMPTY slot is fine (no manifest yet) — that is how a
        // retry after an abandoned login works.
        assert!(prepare_login_slot(data.path(), "work", false).is_ok());

        // ── THE REFUSAL THAT USED TO LIVE HERE ───────────────────────────────
        // This assertion was `assert!(prepare_login_slot(.., false).is_err())`:
        // a slot with a manifest refused a login unless `--force` was passed.
        // That is the *"already has a snapshot at …"* stop that cost the
        // operator half an hour, and it is gone. Re-logging in is always
        // permitted and always replaces what was there — WITHOUT a flag, which
        // is the point: a refusal that relaxes when you pass a flag is still a
        // refusal.
        write_login_manifest(&slot).unwrap();
        let again = prepare_login_slot(data.path(), "work", false)
            .expect("a pre-existing manifest must NEVER refuse a login");
        assert!(
            again.preexisting,
            "the re-login is still REPORTED as a re-login — callers may say so, \
             they may not refuse it"
        );
        // And the flag changes nothing, because there is nothing left to unlock.
        let forced = prepare_login_slot(data.path(), "work", true).unwrap();
        assert!(forced.preexisting);
        assert_eq!(forced.dir, again.dir);
        assert_eq!(forced.keychain_service, again.keychain_service);
    }

    /// **THE LAW: PRIOR STATE NEVER REFUSES A LOGIN.**
    ///
    /// Every state below used to be, or could plausibly be made into, a reason
    /// to stop him. Each is built explicitly and each must be logged into
    /// without a flag. Forced to fail first: against the previous code the
    /// manifest cases returned `Err`.
    #[test]
    fn no_prior_state_whatsoever_can_refuse_a_login() {
        let data = tempfile::tempdir().unwrap();

        // 1. A slot that has never existed.
        prepare_login_slot(data.path(), "fresh", false).expect("a brand-new slot");

        // 2. A slot with a MANIFEST — the literal "already has a snapshot" case.
        let s = prepare_login_slot(data.path(), "snap", false).unwrap();
        write_login_manifest(&s).unwrap();
        prepare_login_slot(data.path(), "snap", false)
            .expect("an existing snapshot must not refuse a login");

        // 3. A slot with a manifest AND a config json — a fully set-up account.
        std::fs::write(
            s.dir.join(".claude.json"),
            r#"{"oauthAccount":{"accountUuid":"a","emailAddress":"e@x.com",
                "organizationUuid":"o","billingType":"max"}}"#,
        )
        .unwrap();
        prepare_login_slot(data.path(), "snap", false)
            .expect("a fully set-up account must not refuse a re-login");

        // 4. A slot with a LOGIN HINT and settings — the state a real, working
        //    account is in. Nothing here is a reason to stop.
        std::fs::write(s.dir.join("login-hint"), "e@x.com\n").unwrap();
        std::fs::write(s.dir.join("settings.json"), "{}").unwrap();
        prepare_login_slot(data.path(), "snap", false)
            .expect("a hint and settings must not refuse a re-login");

        // 5. And the slot dir, manifest, hint and settings all SURVIVE the
        //    preparation. Preparing to log in is not a reason to delete his
        //    account's belongings.
        assert!(s.dir.join("manifest.toml").exists(), "manifest survives");
        assert!(s.dir.join("login-hint").exists(), "login hint survives");
        assert!(s.dir.join("settings.json").exists(), "settings survive");
        assert!(s.dir.join(".claude.json").exists(), "config json survives");
    }

    /// The verdict policy, stated as a table. Exactly ONE verdict stops a
    /// registration, and it is the clone.
    ///
    /// Forced to fail first: against the previous code `SameAccount` also
    /// stopped it unless `--allow-same-account` was passed.
    #[test]
    fn only_the_clone_verdict_stops_a_registration() {
        use DistinctnessVerdict::*;

        assert_eq!(Distinct.action(), VerdictAction::Register);
        assert_eq!(SameAccount.action(), VerdictAction::Warn);
        assert_eq!(NoCredential.action(), VerdictAction::Report);
        assert_eq!(SameSession.action(), VerdictAction::Refuse);

        // The same-account grant REGISTERS now. It is his rate-limit pool to
        // share, and he was never given a way to say so without a flag.
        assert!(
            !SameAccount.blocks_registration(),
            "a same-account login must not be refused — it warns and registers"
        );
        assert!(!Distinct.blocks_registration());
        assert!(!NoCredential.blocks_registration());

        // THE GUARD THAT STAYS. A cloned credential in a second store means one
        // refresh token with two homes; the second refresher self-wipes.
        assert!(
            SameSession.blocks_registration(),
            "cloning a credential into a second store must still be refused"
        );

        // Exactly one, so nobody can quietly add a second.
        let all = [Distinct, SameAccount, NoCredential, SameSession];
        assert_eq!(
            all.iter().filter(|v| v.blocks_registration()).count(),
            1,
            "exactly one verdict may ever stop a registration"
        );
    }

    /// The clone guard is about DUPLICATION, not about re-logging in. A fresh
    /// grant into a slot that already holds a credential — live, dead, or
    /// minted ten minutes ago — is `Distinct`, never `SameSession`.
    #[test]
    fn a_fresh_grant_into_an_occupied_slot_is_never_read_as_a_clone() {
        let ident = |a: &str| IdentityFingerprint {
            acct: a.into(),
            email: format!("{a}@x"),
            org: "o".into(),
            tier: "max".into(),
        };
        let live = SessionWitness {
            identity: Some(ident("live")),
            secret: Some("live-secret".into()),
        };

        // The slot HAD a credential; the browser just replaced it with a new
        // one. Different secret => not a clone, whatever was there before.
        let after_fresh_login = SessionWitness {
            identity: Some(ident("other")),
            secret: Some("brand-new-secret".into()),
        };
        assert_eq!(verify_distinct(&live, &after_fresh_login), DistinctnessVerdict::Distinct);
        assert!(!verify_distinct(&live, &after_fresh_login).blocks_registration());

        // Same ACCOUNT, fresh grant: warns, registers, is not a clone.
        let same_account_fresh = SessionWitness {
            identity: Some(ident("live")),
            secret: Some("brand-new-secret".into()),
        };
        assert_eq!(
            verify_distinct(&live, &same_account_fresh),
            DistinctnessVerdict::SameAccount
        );
        assert!(!verify_distinct(&live, &same_account_fresh).blocks_registration());

        // Only an actual COPY trips it.
        let copied = SessionWitness {
            identity: Some(ident("live")),
            secret: Some("live-secret".into()),
        };
        assert!(verify_distinct(&live, &copied).blocks_registration());
    }

    /// `accounts/backups/` holds live-login undo snapshots. It is not an
    /// account and must not be listed, created, adopted or renamed to.
    ///
    /// Forced to fail first: against the previous code `validate_account_name`
    /// accepted it and `collect_slot_reports` listed it as a no-login slot.
    #[test]
    fn the_backups_directory_is_structurally_not_a_slot() {
        assert!(
            validate_account_name("backups").is_err(),
            "`backups` must not be a legal account name"
        );
        assert!(is_reserved_slot_dir("backups"));
        // It cannot be created as a slot either.
        let data = tempfile::tempdir().unwrap();
        assert!(prepare_login_slot(data.path(), "backups", true).is_err());
        // Ordinary names are untouched.
        assert!(validate_account_name("backups-2").is_ok());
        assert!(validate_account_name("my-backups").is_ok());
        assert!(validate_account_name("work").is_ok());
    }

    /// The enumerator itself: build the real directory shape the operator has
    /// on disk and prove `backups` is not in the report while his real slots
    /// are.
    #[test]
    fn the_slot_listing_excludes_backups_and_other_non_config_dirs() {
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let root = data.path().join("accounts");

        // Exactly what is on his machine: backups/live-login/<stamp>/, with no
        // .claude.json and no manifest.toml anywhere in it.
        std::fs::create_dir_all(root.join("backups/live-login/20260828T000000Z")).unwrap();
        // A working directory that is not an account either.
        std::fs::create_dir_all(root.join(".rotate.lock")).unwrap();
        // Some scratch dir nobody thought to exclude by name.
        std::fs::create_dir_all(root.join("scratch")).unwrap();
        // A REAL slot.
        std::fs::create_dir_all(root.join("real-account")).unwrap();
        std::fs::write(root.join("real-account/.claude.json"), "{}").unwrap();
        // A half-finished login: manifest, no config json. Must STAY visible.
        std::fs::create_dir_all(root.join("interrupted")).unwrap();
        std::fs::write(root.join("interrupted/manifest.toml"), "label = \"interrupted\"\n").unwrap();

        let registry = Registry::default();
        // CON-28 (R9.F2d): this listing reads every slot's store and the
        // machine default; the fixture answers, never the real keychain.
        set_keychain_reader_for_test(Some(std::sync::Arc::new(|_: &str, _: &str| {
            CredentialFacts::default()
        })));
        let reports = collect_slot_reports(&registry, data.path(), home.path(), 0, 7.0);
        let labels: Vec<&str> = reports.iter().map(|r| r.label.as_str()).collect();

        assert!(
            !labels.contains(&"backups"),
            "`backups` is not an account and must never be listed: got {labels:?}"
        );
        assert!(!labels.contains(&"scratch"), "got {labels:?}");
        assert!(!labels.contains(&".rotate.lock"), "got {labels:?}");
        assert!(labels.contains(&"real-account"), "got {labels:?}");
        assert!(
            labels.contains(&"interrupted"),
            "a half-finished login must stay visible so it can be finished: {labels:?}"
        );
    }

    #[test]
    fn login_manifest_declares_own_login_and_parses_back() {
        let data = tempfile::tempdir().unwrap();
        let slot = prepare_login_slot(data.path(), "work", false).unwrap();
        write_login_manifest(&slot).unwrap();
        let text = std::fs::read_to_string(slot.dir.join("manifest.toml")).unwrap();
        let m: SnapshotManifest = toml::from_str(&text).unwrap();
        assert_eq!(m.label, "work");
        assert_eq!(m.credential_mode, LOGIN_CREDENTIAL_MODE);
        assert_eq!(m.keychain_service, slot.keychain_service);
        assert_eq!(m.spawn_env, format!("CLAUDE_CONFIG_DIR={}", slot.dir.display()));
        // Distinguishable from a clone at a glance — that is the whole point.
        assert_ne!(m.credential_mode, "keychain-cloned");
    }

    #[test]
    fn account_name_validation() {
        assert!(validate_account_name("work-2").is_ok());
        assert!(validate_account_name("").is_err());
        assert!(validate_account_name("../evil").is_err());
        assert!(validate_account_name("a/b").is_err());
    }

    fn fake_source(root: &std::path::Path, explicit_dir: bool) -> CaptureSource {
        // Dummy non-secret fixture content.
        if explicit_dir {
            let dir = root.join("cfg");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(".claude.json"), "{\"oauthAccount\":{}}").unwrap();
            CaptureSource {
                config_dir: Some(dir),
                home: root.to_path_buf(),
            }
        } else {
            std::fs::create_dir_all(root.join(".claude")).unwrap();
            std::fs::write(root.join(".claude.json"), "{\"oauthAccount\":{}}").unwrap();
            CaptureSource {
                config_dir: None,
                home: root.to_path_buf(),
            }
        }
    }

    #[test]
    fn capture_plaintext_mode_copies_credentials_file() {
        let tmp = tempfile::tempdir().unwrap();
        let source = fake_source(tmp.path(), true);
        let state = source.state_dir();
        std::fs::write(state.join("settings.json"), "{}").unwrap();
        std::fs::write(state.join(".credentials.json"), "{}").unwrap();

        let data = tmp.path().join("threads-state");
        let out =
            capture_snapshot(
                &source,
                &data,
                "work",
                KeychainMode::Skip,
                // The plaintext copy IS a session clone, so it needs consent too.
                CloneConsent::CloneLiveSession,
            )
            .unwrap();
        assert_eq!(out.credentials, CredentialOutcome::PlaintextCopied);
        assert!(out.dir.join(".claude.json").exists());
        assert!(out.dir.join("settings.json").exists());
        assert!(out.dir.join(".credentials.json").exists());
        assert!(out.dir.join("manifest.toml").exists());
        // Snapshots are force-gitignored.
        assert_eq!(
            std::fs::read_to_string(data.join("accounts/.gitignore")).unwrap(),
            "*\n"
        );
        let manifest: SnapshotManifest = toml::from_str(
            &std::fs::read_to_string(out.dir.join("manifest.toml")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.credential_mode, "plaintext-file");
        assert!(manifest.spawn_env.starts_with("CLAUDE_CONFIG_DIR="));
    }

    #[test]
    fn capture_keychain_mode_without_keychain_requires_login() {
        let tmp = tempfile::tempdir().unwrap();
        let source = fake_source(tmp.path(), false); // default ~/.claude layout
        let data = tmp.path().join("threads-state");
        let out =
            capture_snapshot(&source, &data, "personal", KeychainMode::Skip, CloneConsent::Refuse)
                .unwrap();
        match &out.credentials {
            CredentialOutcome::ManualLoginRequired { service, .. } => {
                // Service is keyed to the SNAPSHOT dir, not the source.
                let dir = out.dir.to_string_lossy().to_string();
                assert_eq!(service, &keychain_service_for(Some(&dir)));
            }
            other => panic!("expected ManualLoginRequired, got {other:?}"),
        }
        assert!(out.dir.join(".claude.json").exists());
        let manifest: SnapshotManifest = toml::from_str(
            &std::fs::read_to_string(out.dir.join("manifest.toml")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.credential_mode, "manual-login-required");
        assert!(manifest.note.is_some());
    }

    #[test]
    fn capture_refuses_to_clobber_an_existing_snapshot() {
        // Folded in from the bash `add-account` prototype's guard (IS-18
        // reconciliation): re-running capture over an already-captured
        // account must fail loudly, not silently overwrite the config and
        // re-clone the keychain entry with whatever is currently logged in.
        let tmp = tempfile::tempdir().unwrap();
        let source = fake_source(tmp.path(), false);
        let data = tmp.path().join("threads-state");
        capture_snapshot(&source, &data, "dup", KeychainMode::Skip, CloneConsent::Refuse).unwrap();
        let err = capture_snapshot(&source, &data, "dup", KeychainMode::Skip, CloneConsent::Refuse)
            .unwrap_err();
        assert!(err.to_string().contains("already captured"), "{err}");
    }

    /// F-5 / F-1: NO un-flagged path clones a stored session.
    ///
    /// The old coverage ran the capture with [`KeychainMode::Skip`], which
    /// never reaches the cloning branch at all — so nothing forbade it. This
    /// exercises the REAL keychain branch (`KeychainMode::Auto`) and asserts it
    /// refuses without consent, and asserts the plaintext branch, which copies
    /// a stored session just as surely, refuses too.
    #[test]
    fn no_unflagged_capture_clones_a_session() {
        let tmp = tempfile::tempdir().unwrap();
        let source = fake_source(tmp.path(), true);
        let data = tmp.path().join("threads-state");

        // The real keychain branch. The guard fires BEFORE `security` is
        // invoked and before anything is created, so this test never reads a
        // keychain and never leaves a directory behind.
        let err = capture_snapshot(&source, &data, "clone", KeychainMode::Auto, CloneConsent::Refuse)
            .unwrap_err()
            .to_string();
        assert!(err.contains("REFUSED"), "{err}");
        assert!(err.contains("--clone-session"), "{err}");
        // (the prose wraps, so match the words, not the phrase)
        assert!(err.contains("refresh") && err.contains("token"), "{err}");
        assert!(
            !data.join("accounts").exists(),
            "a refused capture must leave nothing behind"
        );

        // The plaintext branch is the same defect wearing different clothes.
        std::fs::write(source.state_dir().join(".credentials.json"), "{}").unwrap();
        let err = capture_snapshot(&source, &data, "clone", KeychainMode::Skip, CloneConsent::Refuse)
            .unwrap_err()
            .to_string();
        assert!(err.contains("REFUSED"), "{err}");
        assert!(!data.join("accounts").exists());

        // With consent it proceeds — the mechanism is guarded, not removed.
        let out = capture_snapshot(
            &source,
            &data,
            "clone",
            KeychainMode::Skip,
            CloneConsent::CloneLiveSession,
        )
        .unwrap();
        assert_eq!(out.credentials, CredentialOutcome::PlaintextCopied);
        // …and it says what it is, in the manifest a later reader will trust.
        let m: SnapshotManifest =
            toml::from_str(&std::fs::read_to_string(out.dir.join("manifest.toml")).unwrap())
                .unwrap();
        assert!(
            m.note.as_deref().unwrap_or_default().contains("SHARED SESSION"),
            "{:?}",
            m.note
        );

        // And the CLI cannot produce consent by any route but the flag: one
        // capture call, one consent value, one place it is constructed.
        let cli = include_str!("main.rs");
        assert_eq!(
            cli.matches("accounts::CloneConsent::CloneLiveSession").count(),
            1,
            "main.rs constructs clone consent in more than one place"
        );
        assert!(cli.contains("take_flag(&mut args, \"--clone-session\")"));
        assert_eq!(
            cli.matches("accounts::capture_snapshot(").count(),
            1,
            "a second CLI verb reached capture_snapshot"
        );
    }

    // ── per-account activity (msg #690) ──────────────────────────────────

    /// `retrying` is a BOOLEAN now, not a status word: the third argument used
    /// to be the cached `threads.status` column (`busy` | `idle` | `retrying`)
    /// and the roll-up counted `retrying` and `idle` off it. It is derived from
    /// the `retries` table now, so the fixture states the fact directly.
    fn ta(id: &str, account: &str, retrying: bool) -> ThreadActivity {
        ThreadActivity {
            thread_id: id.to_string(),
            account: account.to_string(),
            executing: false,
            retrying,
            park_reason: None,
            queued_messages: 0,
        }
    }

    fn two_account_registry() -> Registry {
        let mut r = Registry::default();
        r.add("primary", Some("/tmp/acct/primary".into()));
        r.add("secondary-live", Some("/tmp/acct/secondary-live".into()));
        r
    }

    /// The POOL those two accounts form. Derived from the registry, which is
    /// the "no `pool.json`" path and therefore the one worth exercising here.
    fn two_account_pool() -> crate::pool::Pool {
        crate::pool::Pool::from_registry(&two_account_registry())
    }

    #[test]
    fn attribution_prefers_live_then_the_row_column_then_history() {
        // The live in-memory account wins outright.
        assert_eq!(
            attribute_account(&["hist".into()], Some("row"), Some("live")),
            "live"
        );
        // The `threads.account` column beats history: since the IS-A fix it is
        // the durable mirror of the SERVING account, while history only names
        // where a turn last ENDED — so on a fresh failover the column is right
        // and the freshest history row is a turn old.
        assert_eq!(
            attribute_account(&["older".into()], Some("switched-to"), None),
            "switched-to"
        );
        // History is the fallback when the column has nothing (most-recent
        // first) — a best-effort column write that failed, or a pre-fix row.
        assert_eq!(
            attribute_account(&["fresh".into(), "older".into()], None, None),
            "fresh"
        );
        assert_eq!(attribute_account(&[], Some("row"), None), "row");
        assert_eq!(attribute_account(&[], None, None), UNATTRIBUTED);
        // Empty strings are not answers.
        assert_eq!(attribute_account(&["hist".into()], Some(""), Some("")), "hist");
        assert_eq!(attribute_account(&[], Some(""), Some("")), UNATTRIBUTED);
    }

    #[test]
    fn every_registered_account_reports_even_when_idle() {
        let rows = activity_by_account(
            &two_account_pool(),
            &[],
            &BTreeMap::new(),
            "2026-07-29T00:00:00Z",
            None,
        );
        assert_eq!(
            rows.iter().map(|r| r.account.as_str()).collect::<Vec<_>>(),
            vec!["primary", "secondary-live"]
        );
        assert!(rows.iter().all(|r| r.registered && !r.busy()));
        assert_eq!(activity_json(&rows)["totals"]["busy"], json!(false));
    }

    #[test]
    fn activity_counts_executing_parked_and_queued_per_account() {
        let mut threads = vec![
            ta("t-exec", "primary", false),
            ta("t-park", "primary", false),
            ta("t-queued", "primary", false),
            ta("t-quiet", "primary", false),
            ta("t-retry", "secondary-live", true),
            ta("t-other", "not-registered", false),
        ];
        threads[0].executing = true;
        threads[1].park_reason = Some("budget:stress".into());
        threads[2].queued_messages = 3;
        threads[4].executing = true;

        let mut states = BTreeMap::new();
        states.insert(
            "secondary-live".to_string(),
            AccountState {
                name: "secondary-live".into(),
                config_dir: None,
                limit_until: Some("2026-07-29T12:00:00Z".into()),
                last_limit_prose: Some("resets at noon".into()),
                ..Default::default()
            },
        );

        let gauge = json!([
            {"account": "primary", "axes": {"session": {"observed_pct": 41.0}}},
            {"account": "nobody", "axes": {"session": {"observed_pct": 99.0}}},
        ]);

        let rows = activity_by_account(
            &two_account_pool(),
            &threads,
            &states,
            "2026-07-29T00:00:00Z",
            Some(&gauge),
        );
        let by = |n: &str| -> AccountActivity {
            rows.iter().find(|r| r.account == n).cloned().unwrap()
        };

        let p = by("primary");
        assert_eq!(p.threads, 4);
        assert_eq!(p.executing, 1);
        assert_eq!(p.executing_thread_ids, vec!["t-exec".to_string()]);
        assert_eq!(p.parked, 1);
        assert_eq!(p.park_reasons.get("budget:stress"), Some(&1));
        assert_eq!((p.queued_threads, p.queued_messages), (1, 3));
        assert_eq!(p.idle, 1);
        assert!(p.busy());
        // Gauge readings ride along when already known — matched by name.
        assert_eq!(p.gauge_axes.unwrap()["session"]["observed_pct"], json!(41.0));

        let s = by("secondary-live");
        assert_eq!((s.executing, s.retrying, s.idle), (1, 1, 0));
        // Inside an observed limit window ⇒ not available, with the prose.
        assert!(!s.available);
        assert_eq!(s.last_limit_prose.as_deref(), Some("resets at noon"));
        assert!(s.gauge_axes.is_none());

        // An unregistered account still surfaces, flagged as such — never
        // silently dropped.
        let u = by("not-registered");
        assert!(!u.registered && u.threads == 1);

        let payload = activity_json(&rows);
        assert_eq!(payload["totals"]["executing"], json!(2));
        assert_eq!(payload["totals"]["parked"], json!(1));
        assert_eq!(payload["totals"]["queued_messages"], json!(3));
        assert_eq!(payload["totals"]["busy"], json!(true));
        assert_eq!(payload["accounts"].as_array().unwrap().len(), 3);
    }

    /// **PARKED IS LISTED AT POOL LEVEL (H179)**, across accounts, with the
    /// account each thread last sat on as secondary attribution only.
    #[test]
    fn parked_threads_are_listed_at_pool_level() {
        let mut threads = vec![
            ta("t-park-a", "primary", false),
            ta("t-park-b", "secondary-live", false),
            ta("t-run", "primary", false),
        ];
        threads[0].park_reason = Some("continuity".into());
        threads[1].park_reason = Some("continuity".into());
        threads[2].executing = true;
        let rows = activity_by_account(
            &two_account_pool(),
            &threads,
            &BTreeMap::new(),
            "2026-07-29T00:00:00Z",
            None,
        );
        let payload = activity_json(&rows);
        let pool = &payload["pool_parked"];
        assert_eq!(pool["count"], json!(2), "{payload}");
        assert_eq!(pool["reasons"]["continuity"], json!(2));
        let mut ids: Vec<(String, String)> = pool["threads"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                (
                    t["thread_id"].as_str().unwrap().to_string(),
                    t["last_account"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        ids.sort();
        assert_eq!(
            ids,
            vec![
                ("t-park-a".to_string(), "primary".to_string()),
                ("t-park-b".to_string(), "secondary-live".to_string()),
            ]
        );
    }

    /// End-to-end over a real (in-memory) store: the collector must read
    /// `running_turns`, `parked_turns`, `queue` and `thread_accounts` and land
    /// each thread on the right account.
    #[test]
    fn collect_activity_reads_the_store() {
        use crate::store::{now_rfc3339, ParkedTurnRow, RunningTurnRow, Store, ThreadRow};

        let store = Store::open_in_memory().unwrap();
        let now = now_rfc3339();
        let mk = |id: &str, account: Option<&str>| ThreadRow {
            id: id.to_string(),
            account: account.map(str::to_string),
            cwd: Some("/tmp".into()),
            model: Some("haiku".into()),
            settings_json: "{\"cwd\":\"/tmp\",\"model\":\"haiku\"}".to_string(),
            last_seq: 0,
            next_message_id: 1,
            last_compaction_message_id: 0,
            needs_restate: false,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        for row in [
            mk("t-exec", Some("primary")),
            mk("t-park", Some("primary")),
            mk("t-queued", Some("secondary-live")),
            // A thread that has FAILED OVER: `switch_account` wrote the new
            // account into the column (IS-A), while the history table still
            // ends on the account its last completed turn ran on. The column
            // is the current answer and must win.
            mk("t-moved", Some("secondary-live")),
            // No column at all (an imported row) — history is the fallback.
            mk("t-historic", None),
        ] {
            store.upsert_thread(&row).unwrap();
        }

        store
            .upsert_running_turn(&RunningTurnRow {
                thread_id: "t-exec".into(),
                pid: 424242,
                spawn_cookie: "c".into(),
                output_path: "/tmp/out".into(),
                byte_offset: 0,
                started_at: now.clone(),
                prompt: None,
                message_ids: vec![],
                attempt: 1,
            })
            .unwrap();
        store
            .upsert_parked_turn(&ParkedTurnRow {
                thread_id: "t-park".into(),
                prompt: "held".into(),
                message_ids: vec![1],
                attempt: 1,
                reason: "budget:stress session 100%".into(),
                budget_key: Some("stress".into()),
                parked_at: now.clone(),
            })
            .unwrap();
        store
            .enqueue("t-queued", 1, "hello", "turn_end", &serde_json::Value::Null, &[])
            .unwrap();
        store
            .enqueue("t-queued", 2, "again", "turn_end", &serde_json::Value::Null, &[])
            .unwrap();
        // t-moved last COMPLETED a turn on primary, then failed over.
        store.record_thread_account("t-moved", "primary").unwrap();
        // t-historic has only history to go on.
        store
            .record_thread_account("t-historic", "secondary-live")
            .unwrap();

        let pool = two_account_pool();
        let rows = collect_activity(&store, &pool, &BTreeMap::new(), None, 100).unwrap();
        let by = |n: &str| -> AccountActivity {
            rows.iter().find(|r| r.account == n).cloned().unwrap()
        };

        let p = by("primary");
        assert_eq!((p.threads, p.executing, p.parked), (2, 1, 1));
        assert!(p.park_reasons.keys().any(|k| k.starts_with("budget:stress")));

        let s = by("secondary-live");
        // t-queued (2 waiting) + t-moved (attributed by its now-current
        // `threads.account` column, NOT by the older history row that still
        // names primary) + t-historic (no column, attributed by history).
        assert_eq!((s.threads, s.queued_threads, s.queued_messages), (3, 1, 2));
        assert_eq!(s.executing, 0);

        // The live in-memory map overrides everything.
        let mut live = BTreeMap::new();
        live.insert("t-exec".to_string(), "secondary-live".to_string());
        let rows = collect_activity(&store, &pool, &live, None, 100).unwrap();
        assert_eq!(
            rows.iter().find(|r| r.account == "secondary-live").unwrap().executing,
            1
        );
        assert_eq!(
            rows.iter().find(|r| r.account == "primary").unwrap().executing,
            0
        );

        // Unused rows in the queue must not be counted: cancel one.
        assert!(store.cancel_queued("t-queued", 2).unwrap());
        let rows = collect_activity(&store, &pool, &BTreeMap::new(), None, 100).unwrap();
        assert_eq!(
            rows.iter().find(|r| r.account == "secondary-live").unwrap().queued_messages,
            1
        );
    }

    #[test]
    fn capture_without_login_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let source = CaptureSource {
            config_dir: None,
            home: tmp.path().to_path_buf(),
        };
        let err = capture_snapshot(
            &source,
            &tmp.path().join("state"),
            "x",
            KeychainMode::Skip,
            CloneConsent::Refuse,
        )
        .unwrap_err();
        assert!(err.to_string().contains("no authenticated claude config"));
    }

    // ── identity + health (account-selection-design §3.1) ────────────────

    fn st(name: &str, identity: Option<&str>, dead: bool) -> AccountState {
        AccountState {
            name: name.to_string(),
            identity: identity.map(str::to_string),
            dead_since: dead.then(|| "2026-07-29T00:00:00Z".to_string()),
            dead_reason: dead.then(|| "OAuth session expired".to_string()),
            ..Default::default()
        }
    }

    fn states(rows: Vec<AccountState>) -> BTreeMap<String, AccountState> {
        rows.into_iter().map(|s| (s.name.clone(), s)).collect()
    }

    #[test]
    fn one_login_under_two_labels_is_one_account() {
        // The live case: `capture-secondary-live` and `default` are the same
        // login (same accountUuid + emailAddress), and every selector treated
        // them as two accounts. Two labels, one selection unit, one account.
        let names = vec![
            "capture-secondary-live".to_string(),
            "default".to_string(),
            "primary".to_string(),
        ];
        let s = states(vec![
            st("capture-secondary-live", Some("aaaaaaaa.bbbbbbbb"), false),
            st("default", Some("aaaaaaaa.bbbbbbbb"), false),
            st("primary", Some("cccccccc.dddddddd"), true),
        ]);

        let units = selection_units(&names, &s);
        assert_eq!(units.len(), 2, "three labels, two logins: {units:#?}");
        let live = units.iter().find(|u| u.is_aliased()).unwrap();
        assert_eq!(
            live.members,
            vec!["capture-secondary-live".to_string(), "default".to_string()]
        );
        assert_eq!(live.canonical, "capture-secondary-live");
        assert!(live.healthy);

        // …and the honest account count is ONE, not three: the second login is
        // dead. Every "N accounts means N x 100% capacity" claim has to be made
        // over this number.
        assert_eq!(distinct_healthy_logins(&names, &s), 1);

        // A label whose identity could not be read is never merged with
        // anything — absence of evidence must not hide a real account.
        let names2 = vec!["a".to_string(), "b".to_string()];
        let s2 = states(vec![st("a", None, false), st("b", None, false)]);
        assert_eq!(selection_units(&names2, &s2).len(), 2);
        assert_eq!(distinct_healthy_logins(&names2, &s2), 2);
    }

    /// The two exclusion sets placement is handed carry every refusal the
    /// deleted `select_failover` used to encode privately — and carry them as
    /// SETS, which is the whole difference: they say what is barred, never
    /// where to go.
    #[test]
    fn the_exclusion_sets_bar_a_limit_window_a_dead_login_and_every_alias() {
        let now = "2026-07-29T06:00:00Z";
        let names = vec![
            "alias".to_string(),
            "dead".to_string(),
            "live".to_string(),
            "other".to_string(),
        ];
        let mut s = states(vec![
            // Same login as `live`, under a second label.
            st("alias", Some("same.same"), false),
            // A different login that cannot authenticate at all.
            st("dead", Some("dead.dead"), true),
            st("live", Some("same.same"), false),
            // A genuinely separate, healthy login.
            st("other", Some("other.other"), false),
        ]);

        // Nothing is limited yet; the dead login is barred as a TARGET only.
        assert!(unavailable_accounts(&names, &s, now).is_empty());
        assert_eq!(unhealthy_accounts(&names, &s), vec!["dead".to_string()]);

        // `live` hits a usage limit. The limit belongs to the LOGIN, so `alias`
        // — the second label for it — is barred with it. A hop between the two
        // would re-hit the very limit at the cost of a conversation.
        s.get_mut("live").unwrap().limit_until = Some("2026-07-29T12:00:00Z".into());
        assert_eq!(
            unavailable_accounts(&names, &s, now),
            vec!["alias".to_string(), "live".to_string()],
        );
        // …which leaves exactly one usable account, and placement — not this
        // function — is what picks it.
        assert!(!unavailable_accounts(&names, &s, now).contains(&"other".to_string()));

        // A limit window that has PASSED bars nothing (the original rule).
        assert!(unavailable_accounts(&names, &s, "2026-07-29T18:00:00Z").is_empty());

        // An UNKNOWN fingerprint never matches, so it can only ever leave an
        // extra account in the field, never take a real one out of it.
        let two = vec!["live".to_string(), "unknown".to_string()];
        let mut s3 = s.clone();
        s3.insert("unknown".into(), st("unknown", None, false));
        assert_eq!(
            unavailable_accounts(&two, &s3, now),
            vec!["live".to_string()]
        );

        // An account with no tracked row at all is presumed fine on both axes.
        assert!(unavailable_accounts(&["fresh".to_string()], &s, now).is_empty());
        assert!(unhealthy_accounts(&["fresh".to_string()], &s).is_empty());

        // A dead login's aliases are barred as targets with it.
        s.get_mut("alias").unwrap().dead_since = Some(now.into());
        assert_eq!(
            unhealthy_accounts(&names, &s),
            vec!["alias".to_string(), "dead".to_string(), "live".to_string()],
        );
    }

    #[test]
    fn only_a_failure_that_proves_a_dead_login_sets_the_flag() {
        // The live text, verbatim (account-visibility-report).
        assert!(is_dead_login_error(
            "Failed to authenticate: OAuth session expired and could not be refreshed"
        ));
        assert!(is_dead_login_error("unclassified error: Invalid API key"));
        assert!(is_dead_login_error(
            "Not logged in. Please run /login to authenticate"
        ));
        // Everything that heals on its own, or that is a billing state rather
        // than a broken login, must NOT take an account out of the pool.
        assert!(!is_dead_login_error(
            "You have hit your session limit resets 2:40am"
        ));
        assert!(!is_dead_login_error("API Error: 529 Overloaded"));
        assert!(!is_dead_login_error("connection reset by peer"));
        assert!(!is_dead_login_error(
            "Your credit balance is too low to run this request"
        ));
        assert!(!is_dead_login_error("result subtype error_max_turns"));
    }

    #[test]
    fn an_identity_is_a_hash_of_the_account_not_the_org() {
        let tmp = tempfile::tempdir().unwrap();
        let write = |name: &str, acct: &str, email: &str, org: &str| -> PathBuf {
            let dir = tmp.path().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            let p = dir.join(".claude.json");
            std::fs::write(
                &p,
                serde_json::json!({
                    "oauthAccount": {
                        "accountUuid": acct,
                        "emailAddress": email,
                        "organizationUuid": org,
                    }
                })
                .to_string(),
            )
            .unwrap();
            p
        };

        let a = identity_fingerprint(&write("a", "acct-1", "me@example.com", "org-1")).unwrap();
        // Same human, different active workspace — one subscription, so one set
        // of usage limits, so ONE account for selection.
        let b = identity_fingerprint(&write("b", "acct-1", "me@example.com", "org-2")).unwrap();
        assert_eq!(a, b);
        // A different login is a different identity.
        let c = identity_fingerprint(&write("c", "acct-2", "you@example.com", "org-1")).unwrap();
        assert_ne!(a, c);
        // Only hashes come back — no uuid, email or org value survives.
        assert!(!a.contains("acct-1") && !a.contains("me@example.com") && !a.contains("org"));

        // Nothing to fingerprint is `None`, never a guess.
        assert_eq!(identity_fingerprint(&tmp.path().join("missing.json")), None);
        let empty = tmp.path().join("empty.json");
        std::fs::write(&empty, "{}").unwrap();
        assert_eq!(identity_fingerprint(&empty), None);

        // Resolution through the registry: an account with a config dir reads
        // that dir's `.claude.json`; the machine default reads `$HOME`'s.
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(".claude.json"),
            serde_json::json!({"oauthAccount": {"accountUuid": "acct-1", "emailAddress": "me@example.com"}})
                .to_string(),
        )
        .unwrap();
        assert_eq!(
            identity_of(Some(tmp.path().join("a").to_str().unwrap()), &home),
            identity_of(None, &home),
            "the machine default and a capture of it are ONE login"
        );
    }

    #[test]
    fn account_status_keeps_a_dead_account_visible_and_names_its_alias() {
        let mut r = Registry::default();
        r.add("alias", Some("/tmp/acct/alias".into()));
        r.add("dead", Some("/tmp/acct/dead".into()));
        r.add("live", Some("/tmp/acct/live".into()));
        let r = crate::pool::Pool::from_registry(&r);
        let s = states(vec![
            st("alias", Some("same.same"), false),
            st("dead", Some("dead.dead"), true),
            st("live", Some("same.same"), false),
        ]);

        let rows = activity_by_account(&r, &[], &s, "2026-07-29T06:00:00Z", None);
        let by = |n: &str| rows.iter().find(|x| x.account == n).cloned().unwrap();

        // Excluded from selection, still fully visible with the reason.
        let d = by("dead");
        assert!(!d.healthy);
        assert!(d.dead_since.is_some());
        assert_eq!(d.dead_reason.as_deref(), Some("OAuth session expired"));
        assert!(d.registered, "a dead account is still a registered account");
        assert_eq!(d.selection_unit, None, "it is the only label for its login");

        // Both labels for one login name the same selection unit, so a reader
        // can see that counting them as two accounts would be wrong.
        assert_eq!(by("alias").selection_unit.as_deref(), Some("alias"));
        assert_eq!(by("live").selection_unit.as_deref(), Some("alias"));
        assert_eq!(by("live").identity, by("alias").identity);

        // The wire shape carries all of it.
        let payload = activity_json(&rows);
        let dead = payload["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["account"] == "dead")
            .unwrap()
            .clone();
        assert_eq!(dead["healthy"], json!(false));
        assert!(dead["dead_reason"].is_string());
    }

    // ─────────────────────────────────────────────────────────────────
    // SLOT HEALTH AND ROTATION (task-account-login-cli)
    // ─────────────────────────────────────────────────────────────────

    /// The shape of a real credential blob, as read out of the live keychain
    /// item on 2026-07-29 (field names and types only — no values from it).
    fn blob(refresh: &str, access_ms: i64, refresh_ms: i64) -> String {
        json!({"claudeAiOauth": {
            "accessToken": "at-xxxx",
            "refreshToken": refresh,
            "expiresAt": access_ms,
            "refreshTokenExpiresAt": refresh_ms,
            "scopes": ["user:inference"],
            "subscriptionType": "max",
            "rateLimitTier": "default_claude_max_20x",
        }})
        .to_string()
    }

    const NOW: i64 = 1_785_000_000_000; // 2026-07-25T18:40:00Z
    const DAY: i64 = 86_400_000;

    #[test]
    fn credential_facts_carry_no_token_material() {
        let raw = blob("rt-SECRET-VALUE", NOW + 8 * 3_600_000, NOW + 30 * DAY);
        let f = credential_facts_from_blob(&raw);
        assert!(f.refresh_token_present);
        assert_eq!(f.access_expires_at_ms, Some(NOW + 8 * 3_600_000));
        assert_eq!(f.refresh_expires_at_ms, Some(NOW + 30 * DAY));
        assert_eq!(f.subscription_type.as_deref(), Some("max"));
        // The whole point: nothing rendered from this can leak a token.
        let rendered = serde_json::to_string(&f).unwrap();
        for leak in ["rt-SECRET-VALUE", "at-xxxx"] {
            assert!(!rendered.contains(leak), "leaked {leak} in {rendered}");
        }
    }

    #[test]
    fn secret_hash_matches_the_bash_mirror_and_never_reads_empty_as_a_hash() {
        // bash: `security … -w | tr -d '\n' | shasum -a 256 | cut -c1-8`.
        let raw = "{\"claudeAiOauth\":{}}\n";
        assert_eq!(
            credential_facts_from_blob(raw).secret_sha8,
            Some(sha8("{\"claudeAiOauth\":{}}")),
            "trailing newline must be stripped before hashing, as bash does"
        );
        // An entry holding nothing must read as NO credential, never as
        // sha8("") = e3b0c442 — the bug that would make "no credential" pass
        // as a real distinct secret.
        assert_eq!(credential_facts_from_blob("").secret_sha8, None);
        assert_eq!(credential_facts_from_blob("\n").secret_sha8, None);
        assert_eq!(sha8(""), "e3b0c442");
    }

    #[test]
    fn unparseable_blob_is_no_login_not_a_healthy_one() {
        // Fail-closed: a blob we cannot read must never classify as usable.
        let f = credential_facts_from_blob("not json at all");
        assert!(f.secret_sha8.is_some(), "there IS an entry");
        assert!(!f.refresh_token_present);
        assert_eq!(classify_slot(&f, None, None, NOW), SlotHealth::Wiped);
        assert!(!classify_slot(&f, None, None, NOW).usable());
    }

    #[test]
    fn a_wiped_store_is_the_shape_that_killed_primary() {
        // What Claude Code leaves behind after an invalid_grant it could not
        // recover: refreshToken "", accessToken "", expiresAt 0.
        let raw = json!({"claudeAiOauth":
            {"accessToken":"","refreshToken":"","expiresAt":0,"scopes":[]}})
        .to_string();
        let f = credential_facts_from_blob(&raw);
        assert!(!f.refresh_token_present);
        assert_eq!(f.access_expires_at_ms, None, "0 is not an instant");
        assert_eq!(classify_slot(&f, None, None, NOW), SlotHealth::Wiped);
        // …and it stays Wiped even though its manifest says it was cloned:
        // "there is nothing in here" outranks "it came from somewhere else".
        assert_eq!(
            classify_slot(&f, Some(CLONED_CREDENTIAL_MODE), None, NOW),
            SlotHealth::Wiped
        );
    }

    #[test]
    fn classify_orders_clone_above_expiry() {
        let live = credential_facts_from_blob(&blob("rt", NOW + DAY, NOW + 20 * DAY));
        assert_eq!(
            classify_slot(&live, Some(LOGIN_CREDENTIAL_MODE), None, NOW),
            SlotHealth::Live
        );

        // A clone that still looks perfectly healthy is the dangerous case, so
        // clone-ness must win over every other reading.
        assert_eq!(
            classify_slot(&live, None, Some("the live login".into()), NOW),
            SlotHealth::CloneOf {
                of: "the live login".into()
            }
        );

        let stale = credential_facts_from_blob(&blob("rt", NOW - DAY, NOW - 1));
        assert_eq!(
            classify_slot(&stale, Some(LOGIN_CREDENTIAL_MODE), None, NOW),
            SlotHealth::Expired
        );
        // …and even an EXPIRED clone reports as a clone: the fix is a sign-in,
        // not a refresh, and only the clone wording says so.
        assert_eq!(
            classify_slot(&stale, None, Some("primary".into()), NOW),
            SlotHealth::CloneOf { of: "primary".into() }
        );
    }

    #[test]
    fn a_capture_stays_a_clone_after_its_source_rotates_away_from_it() {
        // THE REGRESSION THIS PINS. Measured on this machine: `secondary-live`
        // was byte-identical to the live login on 2026-07-29T18:59Z; by
        // 2026-07-30T04:44Z the live login had refreshed, the hashes no longer
        // matched, and the capture was left holding a token the server had
        // already rotated away. Its dates still say 28 days.
        //
        // With clone-ness read ONLY from the hash, this slot reports `Live` —
        // and it reports `Live` starting at the exact moment it stopped
        // working. The manifest is what keeps it honest.
        let looks_fine = credential_facts_from_blob(&blob("rt", NOW - DAY, NOW + 28 * DAY));
        assert_eq!(
            classify_slot(&looks_fine, Some(CLONED_CREDENTIAL_MODE), None, NOW),
            SlotHealth::Stranded
        );
        assert!(!SlotHealth::Stranded.usable());
        assert!(SlotHealth::Stranded.summary().contains("STRANDED"));

        // While the hashes still match it is the plainer, more urgent CloneOf.
        assert_eq!(
            classify_slot(
                &looks_fine,
                Some(CLONED_CREDENTIAL_MODE),
                Some("the live login".into()),
                NOW
            ),
            SlotHealth::CloneOf { of: "the live login".into() }
        );

        // And rotation refuses it either way — a stranded capture MIGHT be the
        // copy that refreshed last, in which case refreshing it is what kills
        // the live login.
        let plan = rotation_plan(
            false,
            Some(CLONED_CREDENTIAL_MODE),
            &SlotHealth::Stranded,
            &looks_fine,
            NOW,
        );
        match plan {
            RotationAction::Skip { reason } => {
                assert!(reason.contains("diverged"), "{reason}");
                assert!(reason.contains("own login"), "{reason}");
            }
            other => panic!("a stranded capture must never be refreshed, got {other:?}"),
        }
    }

    /// R9.A2b: which half of a shared session is the copy. Each rule paired
    /// with its mirror, so neither `true` nor `false` passes.
    #[test]
    fn is_the_copy_follows_default_then_declaration_then_age() {
        let m = |mode: &str, at: &str| SnapshotManifest {
            label: "x".into(),
            captured_at: at.into(),
            source_config_dir: "/x".into(),
            source_config_json: "/x/.claude.json".into(),
            spawn_env: String::new(),
            credential_mode: mode.into(),
            keychain_service: String::new(),
            keychain_account: String::new(),
            note: None,
        };
        let own_early = m(LOGIN_CREDENTIAL_MODE, "2026-08-01T00:00:00Z");
        let own_late = m(LOGIN_CREDENTIAL_MODE, "2026-09-01T00:00:00Z");
        let cap_early = m(CLONED_CREDENTIAL_MODE, "2026-07-01T00:00:00Z");
        // 1. the machine default is always the original
        assert!(is_the_copy(Some(&own_early), None, true));
        // 2. the declaration decides, even against age
        assert!(is_the_copy(Some(&cap_early), Some(&own_late), false));
        assert!(!is_the_copy(Some(&own_late), Some(&cap_early), false));
        // 3. no capture declared: the earlier-established is the original
        assert!(is_the_copy(Some(&own_late), Some(&own_early), false));
        assert!(!is_the_copy(Some(&own_early), Some(&own_late), false));
        // 4. undecidable: both are the copy (A2's behaviour)
        assert!(is_the_copy(None, None, false));
        assert!(is_the_copy(Some(&own_early), None, false));
        assert!(is_the_copy(None, Some(&own_early), false));
        assert!(is_the_copy(Some(&own_early), Some(&own_early), false));
    }

    #[test]
    fn find_clone_of_never_matches_on_absence() {
        let peers = [("a", Some("deadbeef")), ("b", None)];
        assert_eq!(
            find_clone_of(Some("deadbeef"), peers),
            Some("a".to_string())
        );
        // Two slots that both have NO credential are not clones of each other.
        assert_eq!(find_clone_of(None, peers), None);
        assert_eq!(find_clone_of(Some("cafef00d"), peers), None);
    }

    #[test]
    fn days_left_and_access_expired_read_the_right_clock() {
        let f = credential_facts_from_blob(&blob("rt", NOW - 1, NOW + 12 * DAY));
        assert!((days_left(&f, NOW).unwrap() - 12.0).abs() < 1e-6);
        assert!(access_expired(&f, NOW), "the ACCESS token, not the login");
        assert_eq!(
            classify_slot(&f, Some(LOGIN_CREDENTIAL_MODE), None, NOW),
            SlotHealth::Live
        );
        // No recorded access expiry ⇒ treated as expired, so the slot still
        // gets its rotation attempt instead of being skipped forever.
        assert!(access_expired(&CredentialFacts::default(), NOW));
        assert_eq!(days_left(&CredentialFacts::default(), NOW), None);
    }

    // ── the safety boundary ──────────────────────────────────────────────

    #[test]
    fn rotation_refuses_the_machine_default_whatever_else_is_true() {
        let f = credential_facts_from_blob(&blob("rt", NOW - DAY, NOW + DAY));
        let plan = rotation_plan(true, Some(LOGIN_CREDENTIAL_MODE), &SlotHealth::Live, &f, NOW);
        assert_eq!(plan.kind(), "skip");
        match plan {
            RotationAction::Skip { reason } => assert!(
                reason.contains("live login"),
                "the refusal must say WHY: {reason}"
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn rotation_refuses_a_clone_even_when_it_looks_healthy() {
        // THE hazard: `secondary-live` is byte-identical to the live login.
        // Refreshing it rotates the operator's own refresh token.
        let f = credential_facts_from_blob(&blob("rt", NOW - DAY, NOW + 25 * DAY));
        let health = SlotHealth::CloneOf {
            of: "the live login".into(),
        };
        let plan = rotation_plan(false, Some(LOGIN_CREDENTIAL_MODE), &health, &f, NOW);
        match plan {
            RotationAction::Skip { reason } => {
                assert!(reason.contains("clone"), "{reason}");
                assert!(reason.contains("shared refresh token"), "{reason}");
            }
            other => panic!("a clone must never be refreshed, got {other:?}"),
        }
    }

    #[test]
    fn rotation_refuses_anything_not_proven_to_own_its_login() {
        let f = credential_facts_from_blob(&blob("rt", NOW - DAY, NOW + 25 * DAY));
        // A captured (cloned) snapshot whose twin is not in this inventory:
        // the measurement cannot catch it, so the manifest must.
        for mode in [Some("keychain-cloned"), Some("manual-login-required"), None] {
            let plan = rotation_plan(false, mode, &SlotHealth::Live, &f, NOW);
            assert_eq!(plan.kind(), "skip", "mode {mode:?} must not be refreshed");
        }
        // Only an own-login is eligible.
        assert_eq!(
            rotation_plan(false, Some(LOGIN_CREDENTIAL_MODE), &SlotHealth::Live, &f, NOW),
            RotationAction::Refresh
        );
    }

    #[test]
    fn rotation_names_the_slots_that_need_a_human() {
        let f = credential_facts_from_blob(&blob("rt", NOW - DAY, NOW - DAY));
        for health in [SlotHealth::NoLogin, SlotHealth::Wiped, SlotHealth::Expired] {
            let plan = rotation_plan(false, Some(LOGIN_CREDENTIAL_MODE), &health, &f, NOW);
            assert_eq!(
                plan.kind(),
                "manual-login-required",
                "{health:?} must surface as a human action, not a silent skip"
            );
        }
    }

    #[test]
    fn rotation_does_not_pretend_to_refresh_a_live_access_token() {
        // Claude Code refreshes lazily; probing with a valid access token
        // rotates nothing, so claiming a rotation there would be a lie.
        let fresh = credential_facts_from_blob(&blob("rt", NOW + 3_600_000, NOW + 20 * DAY));
        match rotation_plan(false, Some(LOGIN_CREDENTIAL_MODE), &SlotHealth::Live, &fresh, NOW) {
            RotationAction::Skip { reason } => assert!(reason.contains("still valid"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn expiry_warning_fires_before_the_thirty_day_cliff() {
        assert_eq!(expiry_warning(Some(29.0), 7.0), None);
        assert!(expiry_warning(Some(3.0), 7.0).unwrap().contains("3.0 days"));
        assert!(expiry_warning(Some(-1.0), 7.0).unwrap().contains("EXPIRED"));
        // Unknown is not the same as fine.
        assert!(expiry_warning(None, 7.0).unwrap().contains("no recorded"));
    }

    #[test]
    fn default_refresh_ttl_matches_the_shipped_binary() {
        // `sxg = 2592000000` in claude v2.1.220 — the fallback lifetime when
        // the token response does not state one. If a CLI upgrade changes it,
        // every "days left" number this tool prints changes with it.
        assert_eq!(DEFAULT_REFRESH_TOKEN_TTL_MS, 30 * DAY);
    }

    // ── the fleet view ───────────────────────────────────────────────────

    #[test]
    fn build_slot_reports_finds_the_clone_across_the_fleet() {
        let shared = blob("rt-shared", NOW - DAY, NOW + 25 * DAY);
        let own = blob("rt-own", NOW - DAY, NOW + 28 * DAY);
        let ident = |a: &str| {
            Some(IdentityFingerprint {
                acct: sha8(a),
                email: sha8(a),
                org: sha8("org"),
                tier: "max".into(),
            })
        };
        let raw = vec![
            // the machine default — the live login
            ("default".to_string(), None, None, ident("me"), credential_facts_from_blob(&shared)),
            // a captured clone of it, still inside its access-token window
            (
                "secondary-live".to_string(),
                Some("/slots/secondary-live".to_string()),
                Some("keychain-cloned".to_string()),
                ident("me"),
                credential_facts_from_blob(&shared),
            ),
            // a real second account
            (
                "work-2".to_string(),
                Some("/slots/work-2".to_string()),
                Some(LOGIN_CREDENTIAL_MODE.to_string()),
                ident("other"),
                credential_facts_from_blob(&own),
            ),
            // an interrupted login: dir exists, nothing landed
            (
                "half".to_string(),
                Some("/slots/half".to_string()),
                None,
                None,
                CredentialFacts::default(),
            ),
            // a capture whose source has since rotated: no twin left to match,
            // dates still comfortable, and completely dead.
            (
                "primary".to_string(),
                Some("/slots/primary".to_string()),
                Some(CLONED_CREDENTIAL_MODE.to_string()),
                ident("me"),
                credential_facts_from_blob(&blob("rt-orphan", NOW - DAY, NOW + 26 * DAY)),
            ),
        ];
        let reports = build_slot_reports(raw, NOW, 7.0);
        let get = |n: &str| reports.iter().find(|r| r.label == n).unwrap();

        // The clone is named as a clone OF the live login, by that wording.
        assert_eq!(
            get("secondary-live").health,
            SlotHealth::CloneOf { of: "the live login".into() }
        );
        // R9.A2c: the default is the ORIGINAL of the pair (`is_the_copy`
        // rule 1), so it is not drawn as a clone — only the copy is. (This
        // used to assert the symmetric `CloneOf { of: "secondary-live" }`; the
        // copy's row still names "the live login", which is what tells the
        // operator his own login has a twin.)
        assert_eq!(get("default").health, SlotHealth::Live);
        assert_eq!(get("secondary-live").rotation.kind(), "skip");
        assert_eq!(get("default").rotation.kind(), "skip");

        // The genuine own-login is the only thing rotation will touch.
        assert_eq!(get("work-2").health, SlotHealth::Live);
        assert_eq!(get("work-2").rotation, RotationAction::Refresh);
        assert!((get("work-2").days_left.unwrap() - 28.0).abs() < 1e-6);
        assert_eq!(
            get("work-2").keychain_service,
            keychain_service_for(Some("/slots/work-2"))
        );

        // An unregistered, empty slot is visible rather than silently absent.
        assert_eq!(get("half").health, SlotHealth::NoLogin);
        assert_eq!(get("half").rotation.kind(), "manual-login-required");

        // The stranded capture is NOT promoted to live just because its twin
        // no longer matches, and its comfortable-looking countdown is called
        // out as meaningless.
        assert_eq!(get("primary").health, SlotHealth::Stranded);
        assert_eq!(get("primary").rotation.kind(), "skip");
        assert!(get("primary").days_left.unwrap() > 25.0);
        assert!(get("primary").warning.as_ref().unwrap().contains("meaningless"));

        // Nothing rendered anywhere carries a token or an email.
        let wire = serde_json::to_string(&reports).unwrap();
        for leak in ["rt-shared", "rt-own", "rt-orphan", "at-xxxx"] {
            assert!(!wire.contains(leak), "leaked {leak}");
        }
    }

    #[test]
    fn only_the_machine_default_is_flagged_as_the_live_login() {
        let raw = vec![
            ("default".to_string(), None, None, None, CredentialFacts::default()),
            (
                "work".to_string(),
                Some("/slots/work".to_string()),
                Some(LOGIN_CREDENTIAL_MODE.to_string()),
                None,
                CredentialFacts::default(),
            ),
        ];
        let reports = build_slot_reports(raw, NOW, 7.0);
        assert!(reports[0].is_machine_default);
        assert_eq!(reports[0].keychain_service, KEYCHAIN_SERVICE_BASE);
        assert!(!reports[1].is_machine_default);
        assert_ne!(reports[1].keychain_service, KEYCHAIN_SERVICE_BASE);
    }

    // ── collapsing the machine-default row (task-accounts-cli-upgrade §1) ──

    /// `(label, config_dir, account, refresh_token, days)` → one raw row.
    fn slot(
        label: &str,
        dir: Option<&str>,
        account: &str,
        refresh: &str,
        days: i64,
    ) -> (
        String,
        Option<String>,
        Option<String>,
        Option<IdentityFingerprint>,
        CredentialFacts,
    ) {
        (
            label.to_string(),
            dir.map(str::to_string),
            dir.map(|_| LOGIN_CREDENTIAL_MODE.to_string()),
            Some(IdentityFingerprint {
                acct: sha8(account),
                email: sha8(account),
                org: sha8("org"),
                tier: "max".into(),
            }),
            credential_facts_from_blob(&blob(refresh, NOW - DAY, NOW + days * DAY)),
        )
    }

    #[test]
    fn the_machine_default_folds_into_the_account_it_is_signed_in_as() {
        // The operator's real fleet on 2026-07-30: the machine login and one
        // named slot are TWO INDEPENDENT GRANTS FOR ONE ACCOUNT (different
        // secrets, different expiries, same `oauthAccount`). That is the shape
        // a secret-hash join would get wrong, so the join is on identity.
        let raw = vec![
            slot("default", None, "example", "rt-machine", 28),
            slot("example", Some("/slots/example"), "example", "rt-slot", 27),
            slot("gmail", Some("/slots/gmail"), "gmail", "rt-gmail", 26),
        ];
        let mut reports = build_slot_reports(raw, NOW, 7.0);
        mark_live_login(&mut reports, None);
        let get = |n: &str| reports.iter().find(|r| r.label == n).unwrap();

        // Exactly one row is the live login, and it is a REAL account.
        assert_eq!(reports.iter().filter(|r| r.is_live_login).count(), 1);
        assert!(get("example").is_live_login);
        assert!(!get("default").is_live_login);
        assert!(!get("gmail").is_live_login);

        // The default row is folded away rather than deleted: rotation still
        // has to see that store and refuse it.
        assert_eq!(get("default").folds_into.as_deref(), Some("example"));
        assert_eq!(get("default").rotation.kind(), "skip");
        assert!(get("example").folds_into.is_none());
        assert!(get("gmail").folds_into.is_none());

        // Both clocks survive the collapse — they are different grants.
        assert!((get("example").days_left.unwrap() - 27.0).abs() < 1e-6);
        assert!((get("example").live_store_days_left.unwrap() - 28.0).abs() < 1e-6);
        assert_eq!(get("example").live_store_health, Some(SlotHealth::Live));
        assert!(get("gmail").live_store_days_left.is_none());

        // And the two grants are NOT mistaken for clones of each other.
        assert_eq!(get("example").health, SlotHealth::Live);
        assert_eq!(get("default").health, SlotHealth::Live);
    }

    #[test]
    fn an_unregistered_live_login_is_never_hidden() {
        // Nothing may drop a store that holds a credential. When the machine
        // login is an account no named slot carries, its row STAYS and it is
        // the one marked live.
        let raw = vec![
            slot("default", None, "nobody-else-has-this", "rt-machine", 28),
            slot("gmail", Some("/slots/gmail"), "gmail", "rt-gmail", 26),
        ];
        let mut reports = build_slot_reports(raw, NOW, 7.0);
        mark_live_login(&mut reports, None);
        let get = |n: &str| reports.iter().find(|r| r.label == n).unwrap();
        assert!(get("default").is_live_login);
        assert!(get("default").folds_into.is_none());
        assert!(!get("gmail").is_live_login);
    }

    #[test]
    fn an_unreadable_machine_identity_claims_nothing() {
        // No `oauthAccount` in `~/.claude.json` = we cannot prove which account
        // the machine login is. Guessing would put "LIVE LOGIN" on the wrong
        // row, so the default row simply stands on its own.
        let raw = vec![
            ("default".to_string(), None, None, None, credential_facts_from_blob(&blob("rt-m", NOW - DAY, NOW + 28 * DAY))),
            slot("gmail", Some("/slots/gmail"), "gmail", "rt-gmail", 26),
        ];
        let mut reports = build_slot_reports(raw, NOW, 7.0);
        mark_live_login(&mut reports, None);
        let get = |n: &str| reports.iter().find(|r| r.label == n).unwrap();
        assert!(get("default").is_live_login);
        assert!(get("default").folds_into.is_none());
        assert!(!get("gmail").is_live_login);
    }

    #[test]
    fn the_pointer_only_breaks_ties_and_a_stale_one_is_ignored() {
        // Two slots, one account. The pointer picks between them…
        let raw = vec![
            slot("default", None, "example", "rt-machine", 28),
            slot("bs-a", Some("/slots/bs-a"), "example", "rt-a", 27),
            slot("bs-b", Some("/slots/bs-b"), "example", "rt-b", 27),
        ];
        let mut reports = build_slot_reports(raw.clone(), NOW, 7.0);
        mark_live_login(&mut reports, Some("bs-b"));
        assert!(reports.iter().find(|r| r.label == "bs-b").unwrap().is_live_login);
        assert_eq!(reports.iter().filter(|r| r.is_live_login).count(), 1);

        // …and a pointer naming a slot that is NOT that account is ignored
        // outright, so a stale pointer can never make the listing lie.
        let raw2 = vec![
            slot("default", None, "example", "rt-machine", 28),
            slot("example", Some("/slots/example"), "example", "rt-slot", 27),
            slot("gmail", Some("/slots/gmail"), "gmail", "rt-gmail", 26),
        ];
        let mut reports2 = build_slot_reports(raw2, NOW, 7.0);
        mark_live_login(&mut reports2, Some("gmail"));
        assert!(reports2.iter().find(|r| r.label == "example").unwrap().is_live_login);
        assert!(!reports2.iter().find(|r| r.label == "gmail").unwrap().is_live_login);

        // Re-marking is idempotent — no row keeps a stale flag.
        let mut reports3 = build_slot_reports(raw, NOW, 7.0);
        mark_live_login(&mut reports3, Some("bs-b"));
        mark_live_login(&mut reports3, Some("bs-a"));
        assert!(reports3.iter().find(|r| r.label == "bs-a").unwrap().is_live_login);
        assert!(!reports3.iter().find(|r| r.label == "bs-b").unwrap().is_live_login);
        assert_eq!(reports3.iter().filter(|r| r.is_live_login).count(), 1);
    }

    // ── rename / delete refusals (task-accounts-cli-upgrade §3) ────────────

    #[test]
    fn rename_refuses_the_machine_login_and_every_bad_name() {
        let have = vec!["default".to_string(), "work".to_string(), "home".to_string()];
        let why = |o: &str, n: &str| rename_refusal(o, n, &have).unwrap_or_default();
        assert!(why("default", "anything").contains("machine login"));
        assert!(why("work", "default").contains("reserved"));
        assert!(why("work", "home").contains("already taken"));
        assert!(why("nope", "fresh").contains("no account"));
        assert!(why("work", "work").contains("already its name"));
        assert!(why("work", "../evil").contains("invalid"));
        assert!(why("work", "has space").contains("invalid"));
        assert!(why("work", "").contains("invalid"));
        // The one legal move.
        assert_eq!(rename_refusal("work", "work-2", &have), None);
    }

    #[test]
    fn delete_refuses_the_machine_login_and_the_live_account_without_force() {
        let why = |l: &str, exists, live, force| {
            delete_refusal(l, exists, live, force).unwrap_or_default()
        };
        assert!(why("default", true, false, false).contains("machine login"));
        // Even --force cannot reach the machine login: it is not a slot.
        assert!(why("default", true, true, true).contains("machine login"));
        assert!(why("ghost", false, false, false).contains("no account"));
        assert!(why("work", true, true, false).contains("currently signed in as"));
        assert!(why("work", true, true, false).contains("--force"));
        assert!(why("../evil", true, false, false).contains("invalid"));
        // Not live: no refusal. Live but forced: no refusal.
        assert_eq!(delete_refusal("work", true, false, false), None);
        assert_eq!(delete_refusal("work", true, true, true), None);
    }

    #[test]
    fn delete_never_names_the_machine_default_keychain_service() {
        // The one mistake that would cost the operator his login is deleting
        // the bare `Claude Code-credentials` item. Two independent guards.
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();
        let dir = data.join("accounts").join("work");
        std::fs::create_dir_all(&dir).unwrap();
        let o = delete_account(data, "work", KeychainMode::Skip).unwrap();
        assert_ne!(o.service, KEYCHAIN_SERVICE_BASE);
        assert!(o.dir_removed);
        assert!(!o.keychain_removed);
        assert!(!dir.exists());
        // And the primitive itself refuses the default service by name.
        assert!(delete_keychain_entry(KEYCHAIN_SERVICE_BASE, "someone").is_err());
        assert!(delete_account(data, DEFAULT_ACCOUNT, KeychainMode::Skip).is_err());
    }

    #[test]
    fn rename_moves_the_directory_the_manifest_and_the_registry_together() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();
        let old_dir = data.join("accounts").join("work");
        std::fs::create_dir_all(&old_dir).unwrap();
        let old_str = old_dir.to_string_lossy().to_string();
        std::fs::write(old_dir.join(".claude.json"), "{}").unwrap();
        std::fs::write(
            old_dir.join("manifest.toml"),
            toml::to_string_pretty(&SnapshotManifest {
                label: "work".into(),
                captured_at: "2026-07-30T00:00:00Z".into(),
                source_config_dir: old_str.clone(),
                source_config_json: format!("{old_str}/.claude.json"),
                spawn_env: format!("CLAUDE_CONFIG_DIR={old_str}"),
                credential_mode: LOGIN_CREDENTIAL_MODE.into(),
                keychain_service: keychain_service_for(Some(&old_str)),
                keychain_account: "tester".into(),
                note: None,
            })
            .unwrap(),
        )
        .unwrap();
        let mut reg = Registry::default();
        reg.add("work", Some(old_str.clone()));
        reg.save(&data.join("accounts.toml")).unwrap();
        write_live_login_pointer(data, "work").unwrap();

        let o = rename_account(data, "work", "work-2", KeychainMode::Skip).unwrap();
        let new_str = o.new_dir.to_string_lossy().to_string();

        assert!(!o.old_dir.exists() && o.new_dir.is_dir());
        assert!(o.new_dir.join(".claude.json").exists());
        // The keychain service is DERIVED FROM THE PATH, so it must change —
        // this is the whole reason a rename cannot be a registry-only edit.
        assert_ne!(o.old_service, o.new_service);
        assert_eq!(o.new_service, keychain_service_for(Some(&new_str)));

        let m: SnapshotManifest =
            toml::from_str(&std::fs::read_to_string(o.new_dir.join("manifest.toml")).unwrap())
                .unwrap();
        assert_eq!(m.label, "work-2");
        assert_eq!(m.source_config_dir, new_str);
        assert_eq!(m.source_config_json, format!("{new_str}/.claude.json"));
        assert_eq!(m.spawn_env, format!("CLAUDE_CONFIG_DIR={new_str}"));
        assert_eq!(m.keychain_service, o.new_service);

        let reg2 = Registry::load(&data.join("accounts.toml")).unwrap();
        assert!(reg2.get("work").is_none());
        assert_eq!(reg2.get("work-2").unwrap().config_dir.as_deref(), Some(new_str.as_str()));
        // The live-login pointer follows the rename rather than dangling.
        assert_eq!(read_live_login_pointer(data).as_deref(), Some("work-2"));
    }

    /// A `work` slot with a manifest, a registry entry and a live-login
    /// pointer — everything a rename touches. Returns the slot dir.
    fn rename_fixture(data: &std::path::Path) -> PathBuf {
        let old_dir = data.join("accounts").join("work");
        std::fs::create_dir_all(&old_dir).unwrap();
        let old_str = old_dir.to_string_lossy().to_string();
        std::fs::write(old_dir.join(".claude.json"), "{}").unwrap();
        std::fs::write(
            old_dir.join("manifest.toml"),
            toml::to_string_pretty(&SnapshotManifest {
                label: "work".into(),
                captured_at: "2026-07-30T00:00:00Z".into(),
                source_config_dir: old_str.clone(),
                source_config_json: format!("{old_str}/.claude.json"),
                spawn_env: format!("CLAUDE_CONFIG_DIR={old_str}"),
                credential_mode: LOGIN_CREDENTIAL_MODE.into(),
                keychain_service: keychain_service_for(Some(&old_str)),
                keychain_account: "tester".into(),
                note: None,
            })
            .unwrap(),
        )
        .unwrap();
        old_dir
    }

    /// Make `path` a symlink to a file that READS fine and cannot be WRITTEN —
    /// the failure injection the late rename steps need. Returns false when the
    /// environment can write it anyway (running as root), in which case the
    /// caller has nothing to prove.
    #[cfg(unix)]
    fn readonly_stand_in(path: &std::path::Path, contents: &str) -> bool {
        use std::os::unix::fs::PermissionsExt;
        let target = path.with_file_name(format!(
            "ro-{}",
            path.file_name().unwrap().to_string_lossy()
        ));
        std::fs::write(&target, contents).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o400)).unwrap();
        if std::fs::write(&target, contents).is_ok() {
            return false; // root, or a filesystem that ignores the mode bits
        }
        let _ = std::fs::remove_file(path);
        std::os::unix::fs::symlink(&target, path).unwrap();
        true
    }

    /// F-2: a rename that fails LATE puts everything back.
    ///
    /// The failure is forced at step 5, the live-login pointer — after the
    /// credential step, the directory move, the manifest rewrite AND the
    /// registry save. The old code unwound only the directory move, so this
    /// returned `Err` leaving a moved directory, a rewritten manifest, a
    /// rewritten registry and (on a real keychain) BOTH items present.
    #[cfg(unix)]
    #[test]
    fn rename_unwinds_every_completed_step_when_a_late_step_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();
        let old_dir = rename_fixture(data);
        let mut reg = Registry::default();
        reg.add("work", Some(old_dir.to_string_lossy().to_string()));
        let registry_path = data.join("accounts.toml");
        reg.save(&registry_path).unwrap();

        // The pointer names `work`, so step 5 will try to rewrite it — and
        // cannot.
        if !readonly_stand_in(&data.join("accounts").join("live-login"), "work\n") {
            return;
        }
        assert_eq!(read_live_login_pointer(data).as_deref(), Some("work"));

        let manifest_before = std::fs::read_to_string(old_dir.join("manifest.toml")).unwrap();
        let registry_before = std::fs::read_to_string(&registry_path).unwrap();

        // KeychainMode::Skip: this test never touches a keychain, real or
        // otherwise. The unwind's credential step is unreachable here by
        // construction, which is exactly what makes the test safe to run.
        let err = rename_account(data, "work", "work-2", KeychainMode::Skip).unwrap_err();
        assert!(err.to_string().contains("live-login"), "{err}");

        // Everything is back, under the OLD name.
        assert!(old_dir.is_dir(), "the directory was not moved back");
        assert!(old_dir.join(".claude.json").exists());
        assert!(
            !data.join("accounts").join("work-2").exists(),
            "the new slot dir survived a failed rename"
        );
        assert_eq!(
            std::fs::read_to_string(old_dir.join("manifest.toml")).unwrap(),
            manifest_before,
            "the manifest was left rewritten"
        );
        assert_eq!(
            std::fs::read_to_string(&registry_path).unwrap(),
            registry_before,
            "the registry was left rewritten"
        );
        let reg_after = Registry::load(&registry_path).unwrap();
        assert!(reg_after.get("work").is_some() && reg_after.get("work-2").is_none());
        assert_eq!(read_live_login_pointer(data).as_deref(), Some("work"));

        // And it is still renameable: the unwind left a consistent slot, not a
        // wedged one.
        let _ = std::fs::remove_file(data.join("accounts").join("live-login"));
        let o = rename_account(data, "work", "work-2", KeychainMode::Skip).unwrap();
        assert!(o.new_dir.is_dir() && !o.old_dir.exists());
    }

    /// The same claim one step earlier: a failure at the REGISTRY save unwinds
    /// the manifest rewrite and the directory move.
    #[cfg(unix)]
    #[test]
    fn rename_unwinds_when_the_registry_save_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();
        let old_dir = rename_fixture(data);
        let registry_path = data.join("accounts.toml");
        let mut reg = Registry::default();
        reg.add("work", Some(old_dir.to_string_lossy().to_string()));
        let registry_text = reg.to_toml().unwrap();
        // Readable (the pre-flight load succeeds), unwritable (step 4 fails).
        if !readonly_stand_in(&registry_path, &registry_text) {
            return;
        }
        let manifest_before = std::fs::read_to_string(old_dir.join("manifest.toml")).unwrap();

        let err = rename_account(data, "work", "work-2", KeychainMode::Skip).unwrap_err();
        assert!(err.to_string().contains("accounts.toml"), "{err}");

        assert!(old_dir.is_dir());
        assert!(!data.join("accounts").join("work-2").exists());
        assert_eq!(
            std::fs::read_to_string(old_dir.join("manifest.toml")).unwrap(),
            manifest_before
        );
        assert_eq!(std::fs::read_to_string(&registry_path).unwrap(), registry_text);
    }

    #[test]
    fn rename_refuses_to_clobber_an_existing_slot() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();
        std::fs::create_dir_all(data.join("accounts").join("work")).unwrap();
        std::fs::create_dir_all(data.join("accounts").join("home")).unwrap();
        assert!(rename_account(data, "work", "home", KeychainMode::Skip).is_err());
        assert!(data.join("accounts").join("work").is_dir());
        assert!(rename_account(data, "ghost", "fresh", KeychainMode::Skip).is_err());
    }

    #[test]
    fn the_live_login_pointer_is_a_hint_and_never_a_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();
        assert_eq!(read_live_login_pointer(data), None);
        std::fs::create_dir_all(data.join("accounts")).unwrap();
        std::fs::write(data.join("accounts").join("live-login"), "../../etc\n").unwrap();
        assert_eq!(read_live_login_pointer(data), None);
        std::fs::write(data.join("accounts").join("live-login"), "  work \n junk\n").unwrap();
        assert_eq!(read_live_login_pointer(data).as_deref(), Some("work"));
        assert!(write_live_login_pointer(data, "../evil").is_err());
    }

    /// Build one account's store holding a transcript plus a populated sidecar.
    fn seed_session(home: &Path, config_dir: &str, slug: &str, session: &str, events: usize) {
        let dir = projects_dir_for(Some(config_dir), home).join(slug);
        std::fs::create_dir_all(dir.join(session).join("subagents")).unwrap();
        std::fs::create_dir_all(dir.join(session).join("tool-results")).unwrap();
        let body: String = (0..events).map(|i| format!("{{\"n\":{i}}}\n")).collect();
        std::fs::write(dir.join(format!("{session}.jsonl")), body).unwrap();
        std::fs::write(
            dir.join(session).join("subagents").join("agent-a1.jsonl"),
            "{\"sub\":true}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join(session).join("tool-results").join("big-output.txt"),
            "OVERSIZED TOOL OUTPUT",
        )
        .unwrap();
    }

    #[test]
    fn porting_a_transcript_carries_its_subagent_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let session = "11111111-2222-3333-4444-555555555555";
        let slug = "-Users-someone-proj";
        let from = home.join("acct-a").to_string_lossy().to_string();
        let to = home.join("acct-b").to_string_lossy().to_string();
        seed_session(home, &from, slug, session, 3);

        let outcome = port_transcript(Some(&from), Some(&to), home, session).unwrap();
        assert!(outcome.holds_conversation(), "{outcome:?}");

        let dst = projects_dir_for(Some(&to), home).join(slug);
        assert!(dst.join(format!("{session}.jsonl")).is_file());
        // THE REGRESSION (2026-08-06): the transcript used to arrive alone, and
        // the subagent transcripts and externalised tool results it REFERENCES
        // were left behind on the source account.
        assert_eq!(
            std::fs::read_to_string(dst.join(session).join("subagents").join("agent-a1.jsonl"))
                .unwrap(),
            "{\"sub\":true}\n"
        );
        assert_eq!(
            std::fs::read_to_string(
                dst.join(session).join("tool-results").join("big-output.txt")
            )
            .unwrap(),
            "OVERSIZED TOOL OUTPUT"
        );
    }

    #[test]
    fn a_kept_transcript_still_gets_its_missing_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let session = "22222222-3333-4444-5555-666666666666";
        let slug = "-Users-someone-proj";
        let from = home.join("acct-a").to_string_lossy().to_string();
        let to = home.join("acct-b").to_string_lossy().to_string();
        seed_session(home, &from, slug, session, 2);
        // The destination already holds an EQUAL-OR-LONGER transcript, so
        // `port_into` keeps it and copies nothing — but its sidecar is empty.
        let dst = projects_dir_for(Some(&to), home).join(slug);
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(
            dst.join(format!("{session}.jsonl")),
            "{\"n\":0}\n{\"n\":1}\n{\"n\":2}\n",
        )
        .unwrap();

        let outcome = port_transcript(Some(&from), Some(&to), home, session).unwrap();
        assert!(matches!(outcome, PortOutcome::Kept { .. }), "{outcome:?}");
        // Keeping the transcript must not mean keeping an empty sidecar.
        assert!(dst
            .join(session)
            .join("subagents")
            .join("agent-a1.jsonl")
            .is_file());
    }

    #[test]
    fn the_sidecar_port_never_shortens_or_deletes_the_destination() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let session = "33333333-4444-5555-6666-777777777777";
        let slug = "-Users-someone-proj";
        let from = home.join("acct-a").to_string_lossy().to_string();
        let to = home.join("acct-b").to_string_lossy().to_string();
        seed_session(home, &from, slug, session, 2);
        let dst = projects_dir_for(Some(&to), home).join(slug);
        std::fs::create_dir_all(dst.join(session).join("subagents")).unwrap();
        // A LONGER destination copy (the thread ran there and appended) and a
        // file the source never had.
        std::fs::write(
            dst.join(session).join("subagents").join("agent-a1.jsonl"),
            "{\"sub\":true}\n{\"sub\":\"newer\"}\n",
        )
        .unwrap();
        std::fs::write(
            dst.join(session).join("subagents").join("agent-local.jsonl"),
            "{\"only\":\"here\"}\n",
        )
        .unwrap();

        port_transcript(Some(&from), Some(&to), home, session).unwrap();

        assert_eq!(
            std::fs::read_to_string(dst.join(session).join("subagents").join("agent-a1.jsonl"))
                .unwrap(),
            "{\"sub\":true}\n{\"sub\":\"newer\"}\n",
            "a longer destination copy is authoritative and must not be clobbered"
        );
        assert!(
            dst.join(session)
                .join("subagents")
                .join("agent-local.jsonl")
                .is_file(),
            "a destination-only file must never be deleted"
        );
    }

    /// **R9.F2h opposed arm: production still asks the REAL keychain.** With
    /// no override the existence check calls the real query (injected here as
    /// a counter, so no keychain is touched) and returns its answer; with an
    /// override it asks the fixture and never the real query.
    #[test]
    fn entry_exists_asks_the_real_query_only_without_an_override() {
        use std::cell::Cell;
        for answer in [true, false] {
            let calls = Cell::new(0);
            let got = keychain_entry_exists_with(None, "svc-f2h", "acct", |s, a| {
                assert_eq!((s, a), ("svc-f2h", "acct"));
                calls.set(calls.get() + 1);
                answer
            });
            assert_eq!(calls.get(), 1, "production arm did not run the real query");
            assert_eq!(got, answer, "production arm did not return the real answer");
        }
        let absent: KeychainExists = std::sync::Arc::new(|_: &str, _: &str| false);
        let present: KeychainExists = std::sync::Arc::new(|_: &str, _: &str| true);
        let never = |_: &str, _: &str| -> bool { panic!("the real query ran under an override") };
        assert!(!keychain_entry_exists_with(Some(&absent), "svc-f2h", "acct", never));
        assert!(keychain_entry_exists_with(Some(&present), "svc-f2h", "acct", never));
    }
}
