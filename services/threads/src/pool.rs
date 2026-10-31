//! **THE POOL — a flat list of typed resource entries the manager may draw
//! from.**
//!
//! The operator, 2026-08-27, describing the shape he wants:
//!
//! > *"for each pool of resources that you can draw from, you have a JSON blob
//! > indicating… what type it is. This is a cloud account. What percentage of
//! > the pool are we giving it access to? And then you just have all of the
//! > pools of resources of all different types, be it codecs, be it open router
//! > with credit limit keys, whatever. You just have them all flat beside each
//! > other."*
//!
//! So an entry carries three things and no hierarchy: a **name**, a **type**,
//! and the **share** of that resource given to the pool. One type is
//! implemented — [`Resource::ClaudeAccount`] — and the enum is open so that a
//! second one (OpenRouter with a credit-limit key, and whatever follows it) is
//! an ADDITION rather than a refactor. Nothing else is built here, deliberately.
//!
//! # `default` is not a member, and that is the whole mechanism
//!
//! The machine `default` account used to be kept out of placement by a RESERVED
//! NAME CHECK inside `Placeable::new` — the placement module holding a literal
//! `"default"` and refusing it. The operator struck that framing:
//!
//! > *"I do not want default even in the purview of the system. Why does it
//! > even see that there is a default? … It's a completely separate system, not
//! > even visible to placement."*
//!
//! And, standing since intent item 70: *"threads must not know the CONCEPT of a
//! default account, though it knows all three real ones."* A module that
//! refuses `default` by name is a module that knows the concept. So the check
//! is GONE, and nothing replaced it downstream.
//!
//! What replaces it is upstream and is not a check at all: **a
//! [`ClaudeAccount`] entry must name the directory its login is stored in**
//! ([`ClaudeAccount::login_dir`], non-empty, enforced in the only constructor).
//! The machine default is precisely the account that names none — it is what
//! you get when `CLAUDE_CONFIG_DIR` is left UNSET, and `~/.claude` is the
//! operator's own terminal login rather than a directory threads was given. So
//! there is no `PoolEntry` that describes it. It is not filtered out of the
//! pool; it was never expressible as a member of one.
//!
//! His framing of what that account actually is, which is why it belongs to
//! another system entirely: *"That's like a carve out specifically to allow me
//! to use cloud code on my machine… It's just like a separate utility that just
//! creates a duplicate login for me."* The `claude-accounts` CLI owns it. This
//! crate does not mention it.
//!
//! # Where the config lives
//!
//! `<data_dir>/pool.json` — the config blob, hand-editable:
//!
//! ```json
//! {
//!   "entries": [
//!     {
//!       "name": "alice-example-com",
//!       "type": "claude_account",
//!       "login_dir": "/Users/testuser/code/substrate-interface-layer/lib/threads/accounts/alice-example-com",
//!       "share": 0.95,
//!       "note": "the other 5% is mine to spend by hand"
//!     },
//!     {
//!       "name": "owner-gmail",
//!       "type": "claude_account",
//!       "login_dir": "/Users/testuser/code/substrate-interface-layer/lib/threads/accounts/owner-gmail"
//!     }
//!   ]
//! }
//! ```
//!
//! **An absent file means today's behaviour**, which is the standing
//! requirement on this wave: the pool is then DERIVED from the account registry
//! ([`Pool::from_registry`]) — one entry per registry row that names a login
//! directory, share absent, which reads as 1.0. No backfill, no shim, and no
//! first-boot write: materialising the file would freeze the pool at the moment
//! of the write, and an account added later with `claude-accounts` would then
//! silently never join it.
//!
//! # `share` here versus `share` in the store
//!
//! Two writers, one precedence rule, stated once. The store table
//! (`pool_shares`, keyed on the NAME) is the OPERATOR OVERRIDE — what he sets
//! at runtime and revises by hand — and it WINS. The `share` field here is the
//! DECLARED share, what the blob says when nothing has overridden it. Absent
//! both, 1.0. [`crate::budgets::Budgets::pool_share`] is the single place that
//! resolves this and nothing else may.

use std::path::Path;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::accounts::{Placeable, Registry};
use crate::error::{Error, Result};

/// **THE POOL, HELD SO IT CAN BE REPLACED** — one cell, shared by every holder.
///
/// The pool used to be an `Arc<Pool>` built once in `lib.rs::build_manager` and
/// never touched again, which made the roster a BOOT-TIME CONSTANT. That is
/// wrong for three operator acts that all change the roster while the daemon
/// runs — adding an account (a login on an unused label creates and registers
/// it), renaming one, and deleting one — and it is wrong in the direction that
/// is hardest to see: the daemon kept broadcasting "re-read the accounts", the
/// client faithfully re-read, and the answer did not move. See
/// [`crate::runtime::ThreadManager::pool`] for the full account, including the
/// rename case where a frozen entry hands placement a `CLAUDE_CONFIG_DIR` that
/// no longer exists on disk.
///
/// **A snapshot, never a borrow.** The accessor hands back the inner `Arc`, so a
/// caller walking `entries` walks ONE pool for the whole walk even if a reload
/// lands underneath it. Nothing holds the read guard across an await, and
/// nothing may: the writer is [`crate::runtime::ThreadManager::reload_pool`],
/// which runs on operator acts and must never queue behind a turn.
///
/// `RwLock` rather than a swap primitive because it needs no new dependency and
/// the read is one uncontended lock plus an `Arc` bump — immaterial beside the
/// `names()` call that follows it, which clones every string in the pool.
pub type PoolCell = Arc<RwLock<Arc<Pool>>>;

/// Wrap a pool in a fresh cell. For a holder that owns its pool outright — a
/// CLI verb, a unit test — and therefore has nobody to share a cell with.
pub fn cell(pool: Pool) -> PoolCell {
    Arc::new(RwLock::new(Arc::new(pool)))
}

/// Read the pool out of a cell as a SNAPSHOT.
///
/// Poison is not a reason to lose the roster: a panic while a reload held the
/// write lock would otherwise take placement, the gauge and every account query
/// down with it. `into_inner()` on the poisoned guard hands back the value that
/// is actually there, which is either the old pool or the new one and is a valid
/// pool either way — the write is a single `Arc` assignment and cannot leave a
/// half-built pool behind.
pub fn snapshot(cell: &PoolCell) -> Arc<Pool> {
    cell.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// **A Claude account offered to the pool, and WHERE ITS LOGIN IS STORED.**
///
/// The operator's description of these accounts, which is the whole content of
/// this type: the logins live in a dedicated directory for pool accounts,
/// written with a force flag so they do not land in the machine's default
/// location. *"That's the only difference. It's just not the default location
/// on the machine."*
///
/// `scripts/add-account` is what writes them: it refuses to run with
/// `CLAUDE_CONFIG_DIR` already set, points `CLAUDE_CONFIG_DIR` at
/// `lib/threads/accounts/<label>`, and takes `--force` to log in over a label
/// that already holds a snapshot. That directory is this field.
///
/// # Why it is REQUIRED, and not an `Option`
///
/// Because it is the property that makes an account a pool account. An account
/// with no login directory spawns with `CLAUDE_CONFIG_DIR` unset, which is
/// `~/.claude` — the operator's own terminal login, which threads was never
/// given. Making the field mandatory is what makes that account unrepresentable
/// here, and unrepresentable is the reason no code downstream needs to know it
/// exists. Compare the deleted reserved-name check, which needed the module to
/// know the word.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeAccount {
    /// The `CLAUDE_CONFIG_DIR` this account's spawns run under — Claude Code's
    /// real isolation mechanism (config, transcripts AND the macOS Keychain
    /// entry), so it is the login's storage location and not merely a settings
    /// path. Never empty; see [`ClaudeAccount::new`].
    login_dir: String,
}

impl ClaudeAccount {
    /// The ONLY constructor. `None` when no login directory is named.
    ///
    /// **This is the entire exclusion mechanism for the machine default**, and
    /// it is worth being explicit that it is not phrased as one: it does not
    /// mention `default`, does not know the word, and would refuse any other
    /// entry that named no directory for exactly the same reason. An empty
    /// string is treated as absent — that is how the registry has always
    /// spelled "machine default" in `config.toml`'s inline seeds
    /// (`default = ""`), so accepting it would reopen the hole through a
    /// different door.
    pub fn new(login_dir: &str) -> Option<Self> {
        if login_dir.is_empty() {
            return None;
        }
        Some(Self {
            login_dir: login_dir.to_string(),
        })
    }

    /// Where this account's login is stored.
    pub fn login_dir(&self) -> &str {
        &self.login_dir
    }
}

/// **What KIND of resource an entry is** — open by construction.
///
/// One variant is implemented. The operator named the others he expects
/// (*"be it codecs, be it open router with credit limit keys, whatever"*) and
/// they are deliberately NOT built here. `#[non_exhaustive]` plus internal
/// tagging is what makes the second one an addition: a new variant needs a new
/// `type` string and a new match arm, and every existing entry in the blob
/// keeps parsing untouched.
///
/// Serialised INTERNALLY TAGGED and flattened into the entry, so the blob reads
/// as one flat object per resource — `{"name": …, "type": "claude_account",
/// "login_dir": …}` — rather than nesting the type's fields under a key. Flat
/// beside each other is the shape he asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Resource {
    /// A Claude Code account with its own login directory.
    ClaudeAccount(ClaudeAccount),
}

impl Resource {
    /// The `type` string as it appears in the blob — for display and logs.
    pub fn type_name(&self) -> &'static str {
        match self {
            Resource::ClaudeAccount(_) => "claude_account",
        }
    }

    /// The Claude account this resource is, if it is one. `None` for every
    /// other type — which is how a caller that only understands Claude accounts
    /// stays correct when a second type is added, instead of mis-reading it.
    pub fn as_claude_account(&self) -> Option<&ClaudeAccount> {
        match self {
            Resource::ClaudeAccount(a) => Some(a),
        }
    }
}

/// **One entry in the pool: a name, a type, and a share.**
///
/// Nothing more. There is no grouping, no nesting and no per-type table —
/// *"you just have them all flat beside each other."*
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoolEntry {
    /// **THE KEY.** The name the operator manages this resource by, and what
    /// the share is keyed on everywhere else in the crate.
    ///
    /// Keyed on the NAME rather than on the login, and the operator gave the
    /// reason: *"I think it's actually better to key on the account name
    /// instead of on the login slot, because the login slot is subject to
    /// change."* Yesterday's table keyed the share on a login fingerprint;
    /// re-pointing a label at a different login then silently moved or lost its
    /// share. The name is the stable thing he manages.
    pub name: String,
    /// The resource, and what type of resource it is.
    #[serde(flatten)]
    pub resource: Resource,
    /// **The percentage of this resource given to the pool**, as a fraction in
    /// `(0.0, 1.0]`. Absent reads as 1.0 — the whole thing, which is what every
    /// account got before shares existed.
    ///
    /// A FRACTION, never a token count and never a percentage point, because
    /// *"percentages are the only thing that's comparable"* and a token count
    /// would need a denominator this entry does not have.
    ///
    /// Overridden by the `pool_shares` store row when one exists; see the
    /// module banner for the precedence, which is stated in exactly one place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share: Option<f64>,
    /// Free text for why this share is what it is, so a number set by hand a
    /// month ago is still explicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl PoolEntry {
    /// A Claude-account entry. `None` when no login directory is named — see
    /// [`ClaudeAccount::new`], which is where that refusal lives.
    pub fn claude_account(name: &str, login_dir: &str) -> Option<Self> {
        Some(Self {
            name: name.to_string(),
            resource: Resource::ClaudeAccount(ClaudeAccount::new(login_dir)?),
            share: None,
            note: None,
        })
    }

    /// The `CLAUDE_CONFIG_DIR` this entry's spawns run under, when the entry is
    /// a Claude account. `None` for any other resource type — which does not
    /// mean "the machine default", it means "this resource is not spawned that
    /// way at all".
    pub fn login_dir(&self) -> Option<&str> {
        self.resource.as_claude_account().map(ClaudeAccount::login_dir)
    }

    /// **This entry, as something placement can name.**
    ///
    /// Infallible, and that is the point of the whole re-shape: MEMBERSHIP IN
    /// THE POOL IS PLACEABILITY. There is no second question to ask and no
    /// second check that could be forgotten at a new call site.
    pub fn placeable(&self) -> Placeable {
        Placeable::of(&self.name)
    }
}

/// **The pool: a flat list of entries, and the config blob that describes it.**
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Pool {
    #[serde(default)]
    pub entries: Vec<PoolEntry>,
}

impl Pool {
    /// Parse the config blob.
    pub fn from_json(s: &str) -> Result<Self> {
        let pool: Self = serde_json::from_str(s)
            .map_err(|e| Error::Config(format!("parsing pool.json: {e}")))?;
        pool.validated()
    }

    /// Render the config blob.
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| Error::Config(format!("rendering pool.json: {e}")))
    }

    /// Refuse a blob that cannot mean what it says: a duplicate name (the key
    /// would be ambiguous) or a share outside `(0.0, 1.0]` (a zero share
    /// divides by zero in the pressure denominator, and above 1.0 claims more
    /// of a resource than exists). Refused rather than clamped — silently
    /// storing 1.0 for a hand-written 1.5 would tell him his edit landed when
    /// it did not.
    fn validated(self) -> Result<Self> {
        let mut seen: Vec<&str> = Vec::with_capacity(self.entries.len());
        for e in &self.entries {
            if e.name.is_empty() {
                return Err(Error::Config(
                    "a pool entry has no name; the name is the key its share is stored under"
                        .into(),
                ));
            }
            if seen.contains(&e.name.as_str()) {
                return Err(Error::Config(format!(
                    "pool.json names {:?} twice — the name is the KEY, so a duplicate makes \
                     its share ambiguous",
                    e.name
                )));
            }
            seen.push(&e.name);
            if let Some(s) = e.share {
                if !s.is_finite() || s <= 0.0 || s > 1.0 {
                    return Err(Error::Config(format!(
                        "pool entry {:?} has share {s} — it must be a fraction in (0.0, 1.0] \
                         (0.95 for 95%), never a percentage point and never a token count",
                        e.name
                    )));
                }
            }
        }
        Ok(self)
    }

    /// **The pool implied by the account registry** — the "no configuration"
    /// answer, and byte-for-byte the set of accounts placement had before
    /// `pool.json` existed.
    ///
    /// One entry per registry row THAT NAMES A LOGIN DIRECTORY. Rows that name
    /// none describe no resource: there is nothing to construct from them, so
    /// nothing is constructed. This is the derivation, not a filter — the
    /// difference being that a filter has a predicate someone could relax, and
    /// this has a constructor that returns `None` because it was handed no
    /// directory.
    pub fn from_registry(registry: &Registry) -> Self {
        Self {
            entries: registry
                .accounts
                .iter()
                .filter_map(|(name, acct)| {
                    PoolEntry::claude_account(name, acct.config_dir.as_deref().unwrap_or(""))
                })
                .collect(),
        }
    }

    /// Load `<data_dir>/pool.json`, falling back to the registry derivation
    /// when the file does not exist.
    ///
    /// A file that exists but does not parse is an ERROR, not a fallback. The
    /// fallback is for "he has not configured a pool"; silently ignoring a
    /// broken edit and running on a different pool than the one he wrote is how
    /// a config becomes untrustworthy.
    pub fn load(path: &Path, registry: &Registry) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => Self::from_json(&s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::from_registry(registry)),
            Err(e) => Err(Error::Config(format!("reading {}: {e}", path.display()))),
        }
    }

    pub fn get(&self, name: &str) -> Option<&PoolEntry> {
        self.entries.iter().find(|e| e.name == name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Every name in the pool, sorted.
    ///
    /// **This is the crate's account list.** Not "the registry minus the
    /// default" — the registry is another system's file and this list is not
    /// derived from it by subtraction. Metering, gauge polling, status output
    /// and the activity roll-up all read this, which is why the machine default
    /// stops APPEARING rather than being filtered out at each of them.
    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.entries.iter().map(|e| e.name.clone()).collect();
        v.sort();
        v
    }

    /// Every entry as a [`Placeable`], sorted by name.
    pub fn placeable(&self) -> Vec<Placeable> {
        let mut v: Vec<Placeable> = self.entries.iter().map(PoolEntry::placeable).collect();
        v.sort();
        v
    }

    /// The DECLARED share for a name, if the blob states one. The store
    /// override is applied by [`crate::budgets::Budgets::pool_share`], which is
    /// the only place the two are combined.
    pub fn declared_share(&self, name: &str) -> Option<f64> {
        self.get(name).and_then(|e| e.share)
    }

    /// The login directory for a name, when it is a Claude account.
    pub fn login_dir(&self, name: &str) -> Option<&str> {
        self.get(name).and_then(PoolEntry::login_dir)
    }
}
