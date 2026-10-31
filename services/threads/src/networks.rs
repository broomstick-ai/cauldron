//! # Messaging networks — `docs/agent-networks.md` (second pass), first working version
//!
//! The design is `docs/agent-networks.md`, which revises
//! `docs/managed-subagents.md` §3 and §6 and carries the operator's rulings in
//! its §0. This module is every RULE of it; [`crate::store`]'s `networks`
//! child module is the rows and decides nothing; [`crate::agents`] is the MCP
//! façade that reaches these functions through `crate::server::handle`.
//!
//! ## The model, in the design's own vocabulary
//!
//! * A **network** is a named membership set with two rules (`max_chars`,
//!   `history`) and, per member, eight independent boolean **flags** (§1.2):
//!   sight (`view`, `visible`), exposure (`wakeable`, `interruptible`,
//!   `inspectable`, `imposable`) and rights over the network (`members`,
//!   `configure`). There are no roles, no owner, no topology enum (N6).
//! * `SEES_N(a, b) ⟺ a ≠ b ∧ (a.view ∨ b.visible)` — [`sees_in`].
//! * `authority(a, b) ⟺ ∃M: SEES_M(a, b) ∧ b.imposable_M` — [`authority`].
//!   Adding a member you hold authority over admits it at once; otherwise it is
//!   an **offer** (`pending`), delivered as mail, accepted with `join` (§1.3).
//! * **`home(t)`** is derived, never created: its members are `subtree(t)`,
//!   computed from `parent_id` (§1.4, N1). The owner has `view`,
//!   `configure` and (R10.W1: a reply wakes its parent by default)
//!   `wakeable`; every descendant is `wakeable`, `interruptible`,
//!   `inspectable` and `imposable`. Overrides set through `configure` are rows
//!   in `network_members` under the id `home:<t>`; the membership itself is
//!   never a row and no membership command touches it (refused `home`).
//! * **Delivery grants a route back** (§1.5, §5): once b has delivered mail to
//!   a over N, a may address b over N. A route is not a roster row, confers no
//!   add (N4) and no read (N9).
//! * **Introduction is non-transitive** (N4): a may add b only if a SEES b by
//!   flag at the moment of the add.
//! * **Membership is not mail; only mail wakes** (N7). Membership facts land on
//!   the ACTING thread's ledger only. An offer is mail to its one addressee.
//! * **Refuse, never clamp** (N8): `no_route`, `too_large`, `no_right`,
//!   `no_authority`, `not_interruptible`, `no_offer`, `home`, `above_ceiling`.
//!   A refusal never distinguishes "does not exist" from "you may not see it".
//! * **No upward ledger read** (N9) — [`reads`].
//!
//! ## Where the mail goes, and why it survives a restart
//!
//! A message is the recipient's ordinary `UserMessage` + `queue` row, through
//! [`ThreadManager::send`] — the same durable path the operator's words take —
//! with the sender, the network and `kind` in the message metadata and a
//! one-line header in the text so a model can tell mail from its operator
//! (`managed-subagents.md` §8 item 8). The act itself is a `Custom` ledger
//! event on the SENDER (`mail_queued`), which is what resolves the command's
//! promise. Networks, members, routes and held mail are rows in `threads.db`.
//! Nothing here is in memory, so a daemon restart loses none of it.
//!
//! ## Choices this first version made (each recorded in the R4.S3 report)
//!
//! * Lineage has ONE owner: `ThreadMeta.parent_id` in R4.S2's managed ledgers
//!   (`crate::managed`), private and write-once, set by `agents/spawn` at its
//!   commit point (the parent's `child{started}`). This module only READS it —
//!   [`ancestors`] walks the child's meta, [`subtree`] walks the parents'
//!   committed `child{started}` events. There is no networks-side writer and
//!   no `set_parent` seam any more: a spawn's commit IS the parent edge, so a
//!   second writer would be a second owner (R4.S3-land; the earlier
//!   `thread_parents` table is gone).
//! * `intent: interrupt` to an `interruptible` member CUTS its running turn
//!   (R4.S8): [`ThreadManager::cancel`] — the operator's own cancel, the same
//!   level-triggered flag the tail loop polls — and then the mail is queued as
//!   the turn that follows, which is "cut the owner's running turn and restart
//!   it carrying this message" (`managed-subagents.md` §2.5). The cut comes
//!   FIRST so that a turn which ends on its own in between is never the one
//!   cancelled: `run_once` clears the flag on entry to the next turn. To a
//!   DORMANT member there is nothing to cut, and it is delivered under the
//!   same `wakeable` rule as `immediate` — interrupt grants no wake. To a
//!   member that is not `interruptible` it is `not_interruptible`, never
//!   downgraded (N8). A broadcast with `interrupt` is refused whole,
//!   `not_interruptible`, when any recipient is not `interruptible`.
//! * Mail to a DORMANT member that is not `wakeable` is held
//!   (`network_held_mail`) and marked on the recipient's ledger as a
//!   ledger-only `mail_held` event; it is released ahead of the next mail
//!   that does wake it, and [`release_held`] is the seam R4.S2's turn start
//!   calls so that any turn opens with it. Mail to a RUNNING member is queued
//!   as usual whatever `wakeable` says — the member is not dormant.
//! * A creator's default flags on itself are `view, visible, wakeable,
//!   members, configure` (§3.3: "visible ... the default for a creator",
//!   "wakeable ... also the creator default"); every other member defaults to
//!   all-false (§6's "default false in a created network").
//! * `agents_network_create` acknowledges with the minted `network` id beside
//!   `promise_id`/`visible`, by the `thread_id`-of-`agents_spawn` precedent: an
//!   address the act minted, not a fact the shape carries.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use shapewire::envelope::codes;

use crate::error::{Error, Result};
use crate::event::SendIntent;
use crate::runtime::ThreadManager;
use crate::store::networks::{HeldMail, MemberRow, NetworkRow};
use crate::store::now_rfc3339;

/// **THE NOTIFY POLICY** (R10.M3): one row per (owner, peer, network), `direct`
/// when none is set; read by [`post`], [`message`], [`broadcast`] and
/// [`route`]. See the module for the rules.
pub mod policy;
use policy::Notify;

/// A home network's id is this prefix and the owner's thread id.
pub const HOME_PREFIX: &str = "home:";
/// `max_chars` when a network does not say (§1.2).
pub const DEFAULT_MAX_CHARS: usize = 8_192;
/// The producer ceiling on any network's `max_chars` (§7). A `configure` or
/// `create` above it is refused `too_large` naming this number.
pub const MAX_MESSAGE_CHARS: usize = 65_536;
/// How many networks one thread may create (§4.2 `above_ceiling`).
pub const MAX_NETWORKS_PER_THREAD: usize = 64;
/// How many members one created network may hold (§4.2 `above_ceiling`).
pub const MAX_MEMBERS: usize = 256;

/// The refusal codes of §2 N8 / §4.2. `no_route` is the one answer for "does
/// not exist" and "exists and you may not see it" (N2).
pub mod refusal {
    pub const NO_ROUTE: &str = "no_route";
    pub const TOO_LARGE: &str = "too_large";
    pub const NO_RIGHT: &str = "no_right";
    pub const NO_AUTHORITY: &str = "no_authority";
    pub const NOT_INTERRUPTIBLE: &str = "not_interruptible";
    pub const NO_OFFER: &str = "no_offer";
    pub const HOME: &str = "home";
    pub const ABOVE_CEILING: &str = "above_ceiling";
}

/// Serialises the network WRITE acts. Each act reads several rows and writes
/// several; one daemon-wide lock keeps a check and its write together. The
/// acts are short (a handful of indexed rows) so contention is not a concern
/// at swarm scale; a per-network lock is the rework if it ever is.
static ACTS: Mutex<()> = Mutex::new(());

// ═════════════════════════════════════════════════════════════════════════════
// flags and rules
// ═════════════════════════════════════════════════════════════════════════════

/// The eight per-member flags of §1.2. Absent keys are `false`; an unknown key
/// is refused (`malformed`), never ignored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Flags {
    pub view: bool,
    pub visible: bool,
    pub wakeable: bool,
    pub interruptible: bool,
    pub inspectable: bool,
    pub imposable: bool,
    pub members: bool,
    pub configure: bool,
}

impl Flags {
    /// A creator's default flags on itself (§3.3).
    pub fn creator() -> Self {
        Flags { view: true, visible: true, wakeable: true, members: true, configure: true, ..Default::default() }
    }
    /// `home(t)`'s owner row (§1.4).
    ///
    /// **`wakeable` BY DEFAULT** (R10.W1, `MESSAGING-ORCHESTRATION.md` §2.2:
    /// a link with no policy set is `direct`). A descendant can only address
    /// its owner over `home(t)` through a route back (the owner is not
    /// `visible`), i.e. ANSWERING mail the owner sent — and that answer must
    /// wake the owner. Before R10.W1 the owner row had no `wakeable`, so every
    /// child's reply over `home:<parent>` to a dormant parent was HELD until
    /// something else opened the parent's next turn (an internal thread sat 55
    /// min). An owner that wants replies held lowers it with `configure`.
    pub fn home_owner() -> Self {
        Flags { view: true, wakeable: true, configure: true, ..Default::default() }
    }
    /// `home(t)`'s row for every descendant (§1.4).
    pub fn home_descendant() -> Self {
        Flags { wakeable: true, interruptible: true, inspectable: true, imposable: true, ..Default::default() }
    }
    fn bits(&self) -> [bool; 8] {
        [self.view, self.visible, self.wakeable, self.interruptible, self.inspectable, self.imposable, self.members, self.configure]
    }
    /// Every flag set in `self` is also set in `other`: `self` only LOWERS.
    pub fn is_subset_of(&self, other: &Flags) -> bool {
        self.bits().iter().zip(other.bits().iter()).all(|(a, b)| !*a || *b)
    }
}

/// A network's two rules (§1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Rules {
    pub max_chars: usize,
    pub history: bool,
}

impl Default for Rules {
    fn default() -> Self {
        Rules { max_chars: DEFAULT_MAX_CHARS, history: false }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// refusals
// ═════════════════════════════════════════════════════════════════════════════

fn refused(code: &'static str, message: impl Into<String>) -> Error {
    Error::Refused { code, message: message.into() }
}

fn no_route(what: impl std::fmt::Display) -> Error {
    refused(
        refusal::NO_ROUTE,
        format!(
            "no_route: {what}. No network you are in gives you sight of (or a route to) that \
             address. This answer is the same whether or not it exists"
        ),
    )
}

fn malformed(message: impl Into<String>) -> Error {
    refused(codes::MALFORMED, message)
}

fn parse<T: for<'de> Deserialize<'de>>(what: &str, v: &Value) -> Result<T> {
    serde_json::from_value(v.clone()).map_err(|e| malformed(format!("`{what}`: {e}")))
}

// ═════════════════════════════════════════════════════════════════════════════
// lineage — derived from `parent_id`, which is write-once
// ═════════════════════════════════════════════════════════════════════════════

/// `anc(t)`, nearest first — a walk up `ThreadMeta.parent_id` (R4.S2's, the
/// one owner). A thread that took part in no spawn has none.
pub fn ancestors(manager: &ThreadManager, t: &str) -> Result<Vec<String>> {
    let mg = manager.managed();
    let mut out = Vec::new();
    let mut cur = mg.parent_of(t);
    while let Some(p) = cur {
        if out.contains(&p) || out.len() > 10_000 {
            break;
        }
        cur = mg.parent_of(&p);
        out.push(p);
    }
    Ok(out)
}

/// `subtree(t)`, including `t` — the committed `child{started}` events down
/// from `t`, through the managed runtime's own walk. A cycle (only reachable
/// by a hand-edited ledger) is refused `integrity`, never looped on.
pub fn subtree(manager: &ThreadManager, t: &str) -> Result<BTreeSet<String>> {
    let mg = manager.managed();
    let tree = crate::ledger::spawn::subtree(t, |id| mg.children_of(id))
        .map_err(|c| refused(codes::INTEGRITY, c.to_string()))?;
    Ok(tree.into_keys().collect())
}

// ═════════════════════════════════════════════════════════════════════════════
// networks as the rules see them
// ═════════════════════════════════════════════════════════════════════════════

/// One network, home or created, with its rules resolved.
#[derive(Debug, Clone)]
pub struct Net {
    pub id: String,
    pub name: String,
    /// `Some(owner)` for `home(owner)`.
    pub home_of: Option<String>,
    pub created_by: String,
    pub rules: Rules,
}

impl Net {
    pub fn is_home(&self) -> bool {
        self.home_of.is_some()
    }
}

fn rules_of(row: &NetworkRow) -> Result<Rules> {
    serde_json::from_str(&row.rules_json).map_err(|e| Error::Internal(format!("network {} rules: {e}", row.id)))
}

/// Load a network by id. `home:<t>` exists exactly when thread `t` does.
pub fn load(manager: &ThreadManager, id: &str) -> Result<Option<Net>> {
    let row = manager.store.get_network(id)?;
    if let Some(owner) = id.strip_prefix(HOME_PREFIX) {
        if manager.store.get_thread(owner)?.is_none() {
            return Ok(None);
        }
        let rules = match &row {
            Some(r) => rules_of(r)?,
            None => Rules::default(),
        };
        return Ok(Some(Net {
            id: id.to_string(),
            name: "home".to_string(),
            home_of: Some(owner.to_string()),
            created_by: owner.to_string(),
            rules,
        }));
    }
    let Some(row) = row else { return Ok(None) };
    Ok(Some(Net {
        rules: rules_of(&row)?,
        id: row.id,
        name: row.name,
        home_of: None,
        created_by: row.created_by,
    }))
}

/// Current members of `net` with their flags (pending offers excluded).
pub fn members(manager: &ThreadManager, net: &Net) -> Result<BTreeMap<String, Flags>> {
    let rows = manager.store.network_members(&net.id)?;
    let decode = |r: &MemberRow| -> Result<Flags> {
        serde_json::from_str(&r.flags_json).map_err(|e| Error::Internal(format!("member flags: {e}")))
    };
    let mut out = BTreeMap::new();
    match &net.home_of {
        Some(owner) => {
            let overrides: BTreeMap<String, Flags> = rows
                .iter()
                .filter(|r| r.state == "member")
                .map(|r| Ok((r.thread_id.clone(), decode(r)?)))
                .collect::<Result<_>>()?;
            for t in subtree(manager, owner)? {
                let default = if &t == owner { Flags::home_owner() } else { Flags::home_descendant() };
                let f = overrides.get(&t).copied().unwrap_or(default);
                out.insert(t, f);
            }
        }
        None => {
            for r in rows.iter().filter(|r| r.state == "member") {
                out.insert(r.thread_id.clone(), decode(r)?);
            }
        }
    }
    Ok(out)
}

fn pending_offer(manager: &ThreadManager, net: &Net, t: &str) -> Result<Option<Flags>> {
    if net.is_home() {
        return Ok(None);
    }
    for r in manager.store.network_members(&net.id)? {
        if r.thread_id == t && r.state == "pending" {
            return Ok(Some(serde_json::from_str(&r.flags_json).map_err(|e| Error::Internal(format!("offer flags: {e}")))?));
        }
    }
    Ok(None)
}

/// `SEES_N(a, b)` over a resolved roster.
pub fn sees_in(roster: &BTreeMap<String, Flags>, a: &str, b: &str) -> bool {
    match (roster.get(a), roster.get(b)) {
        (Some(fa), Some(fb)) => a != b && (fa.view || fb.visible),
        _ => false,
    }
}

/// Every network `t` is a member of: the homes of `t` and of each ancestor
/// (nearest first), then created networks in creation order.
pub fn networks_of(manager: &ThreadManager, t: &str) -> Result<Vec<Net>> {
    let mut out = Vec::new();
    let mut homes = vec![t.to_string()];
    homes.extend(ancestors(manager, t)?);
    for h in homes {
        if let Some(n) = load(manager, &format!("{HOME_PREFIX}{h}"))? {
            out.push(n);
        }
    }
    for row in manager.store.networks_with_member(t, "member")? {
        if row.id.starts_with(HOME_PREFIX) {
            continue;
        }
        if let Some(n) = load(manager, &row.id)? {
            out.push(n);
        }
    }
    Ok(out)
}

/// `SEES(a, b)` by flag — the first network that gives it, or `None`.
pub fn sees(manager: &ThreadManager, a: &str, b: &str) -> Result<Option<Net>> {
    for n in networks_of(manager, a)? {
        if sees_in(&members(manager, &n)?, a, b) {
            return Ok(Some(n));
        }
    }
    Ok(None)
}

/// `authority(a, b) ⟺ ∃M: SEES_M(a, b) ∧ b.imposable_M` (§1.3).
pub fn authority(manager: &ThreadManager, a: &str, b: &str) -> Result<bool> {
    for n in networks_of(manager, a)? {
        let roster = members(manager, &n)?;
        if sees_in(&roster, a, b) && roster.get(b).map(|f| f.imposable).unwrap_or(false) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `READS(a, b) ⟺ a = b ∨ (∃N: SEES_N(a, b) ∧ b.inspectable_N ∧ b ∉ anc(a))`.
/// N9 stands above the flags: no flag lets a thread read an ancestor.
pub fn reads(manager: &ThreadManager, a: &str, b: &str) -> Result<bool> {
    if a == b {
        return Ok(true);
    }
    if ancestors(manager, a)?.iter().any(|x| x == b) {
        return Ok(false);
    }
    for n in networks_of(manager, a)? {
        let roster = members(manager, &n)?;
        if sees_in(&roster, a, b) && roster.get(b).map(|f| f.inspectable).unwrap_or(false) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The network a message from `a` to `b` rides: the named one, or — unnamed —
/// the first in which `a` SEES `b` or holds a route to it **and `b` is
/// `wakeable`**, else the first that gives a route at all. `no_route` when
/// none does.
///
/// **WHY THE WAKEABLE PREFERENCE** (R10.W1, the 8e4fca10 wake bug). The
/// candidates come in [`networks_of`] order — home networks nearest first,
/// then joined networks — so before R10.W1 a child answering its parent took
/// `home:<parent>` (where the parent messaged it, granting the route back)
/// even though the parent was `wakeable` to it in the spawn's own network,
/// and the mail was held. An unnamed send now rides a network in which it
/// wakes the recipient whenever one exists; only when none does is the
/// first route kept, and then [`message`] tells the sender it was `held`.
///
/// "Wakes" is [`wakes`] since R10.M3: the recipient's `wakeable` flag (the
/// network's cap) AND its notify policy on the link from `a` (`direct` unless
/// it set `hold`).
fn route(manager: &ThreadManager, a: &str, b: &str, named: Option<&str>) -> Result<(Net, BTreeMap<String, Flags>)> {
    let candidates: Vec<Net> = match named {
        Some(id) => match load(manager, id)? {
            Some(n) => vec![n],
            None => return Err(no_route(format!("`{a}` to `{b}` over `{id}`"))),
        },
        None => networks_of(manager, a)?,
    };
    let mut first: Option<(Net, BTreeMap<String, Flags>)> = None;
    for n in candidates {
        let roster = members(manager, &n)?;
        if !roster.contains_key(a) || !roster.contains_key(b) || a == b {
            continue;
        }
        if sees_in(&roster, a, b) || manager.store.has_route(&n.id, a, b)? {
            if wakes(manager, &roster, a, b, &n.id)? {
                return Ok((n, roster));
            }
            if first.is_none() {
                first = Some((n, roster));
            }
        }
    }
    first.ok_or_else(|| no_route(format!("`{a}` to `{b}`")))
}

// ═════════════════════════════════════════════════════════════════════════════
// delivery
// ═════════════════════════════════════════════════════════════════════════════

/// **DOES MAIL FROM `from` OVER `network` WAKE A DORMANT `to`?** (R10.M3) The
/// network's cap — `to`'s own `wakeable` flag there — AND `to`'s notify policy
/// on that link: a `hold` link holds what the flag would have let wake.
fn wakes(manager: &ThreadManager, roster: &BTreeMap<String, Flags>, from: &str, to: &str, network: &str) -> Result<bool> {
    if !roster.get(to).map(|f| f.wakeable).unwrap_or(false) {
        return Ok(false);
    }
    Ok(policy::get(manager, to, from, network)? != Notify::Hold)
}

fn parse_intent(params: &Value) -> Result<&'static str> {
    match params.get("intent").and_then(Value::as_str) {
        None | Some("immediate") => Ok("immediate"),
        Some("turn_end") => Ok("turn_end"),
        Some("interrupt") => Ok("interrupt"),
        Some(other) => Err(malformed(format!("`intent` must be turn_end | immediate | interrupt, got `{other}`"))),
    }
}

fn send_intent(intent: &str) -> SendIntent {
    if intent == "turn_end" {
        SendIntent::TurnEnd
    } else {
        SendIntent::Immediate
    }
}

/// Put one piece of mail on `to`'s ledger, or hold it (see the module doc).
/// Returns whether it was held.
fn deliver(
    manager: &ThreadManager,
    net: &Net,
    from: &str,
    to: &str,
    kind: &str,
    text: String,
    intent: &str,
    wakeable: bool,
) -> Result<bool> {
    let metadata = json!({
        "mail": {"kind": kind, "from": from, "network": net.id, "network_name": net.name}
    });
    let running = manager.store.get_running_turn(to)?.is_some();
    if !running && !wakeable {
        manager.store.hold_mail(
            &HeldMail {
                id: 0,
                recipient: to.to_string(),
                network_id: net.id.clone(),
                sender: from.to_string(),
                text,
                metadata_json: metadata.to_string(),
            },
            &now_rfc3339(),
        )?;
        manager.append_custom(
            to,
            "mail_held".to_string(),
            json!({"from": from, "network": net.id, "kind": kind}),
            false,
        )?;
        return Ok(true);
    }
    release_held(manager, to)?;
    if intent == "interrupt" && running {
        // The cut, then the mail (see the module doc for the order).
        manager.cancel_turn(to)?;
    }
    post(manager, to, text, send_intent(intent), metadata)?;
    Ok(false)
}

/// **THE ONE DOOR MAIL TAKES ONTO A RECIPIENT** — [`deliver`]'s, and a managed
/// child's completion report's (`crate::managed`, H179,
/// `docs/SUBAGENT-WAKE.md`). Releases anything held for `to` ahead of it, then
/// [`ThreadManager::send`]: the recipient's durable `UserMessage` + `queue`
/// row, dispatched in the same call when `to` is dormant and the intent is
/// `Immediate`. Event-driven end to end — the sender's act IS the wake; nothing
/// polls. Returns the message id.
///
/// **THE NOTIFY POLICY IS READ HERE** (R10.M3, §7.1, §10.1 H22): when the
/// message's link is `hold`, or it is a reply into an owner whose home cap is
/// lowered, it is queued WITHOUT a dispatch and set `ride` — recorded, read
/// with the recipient's next turn, never the cause of one
/// ([`policy::ride_reason`]). Otherwise `Immediate`, as before.
pub fn post(manager: &ThreadManager, to: &str, text: String, intent: SendIntent, metadata: Value) -> Result<u64> {
    post_routed(manager, to, text, intent, metadata).map(|(mid, _)| mid)
}

/// [`post`], also answering WHY the message rides (`None`: it was dispatched
/// eagerly). A kill between the enqueue and the `ride` leaves an eager row —
/// one extra turn, never a lost message (the R9.N1 trade, unchanged).
pub fn post_routed(manager: &ThreadManager, to: &str, text: String, intent: SendIntent, metadata: Value) -> Result<(u64, Option<&'static str>)> {
    release_held(manager, to)?;
    match policy::ride_reason(manager, to, &metadata)? {
        None => Ok((manager.send(to, text, intent, metadata, false)?, None)),
        Some(reason) => {
            let mid = manager.enqueue_undispatched(to, text, intent, metadata)?;
            manager.defer_to_ride(to, &[mid], false, reason);
            Ok((mid, Some(reason)))
        }
    }
}

/// **THE SEAM A TURN START CALLS** — wired (R4.S3-land): `maybe_dispatch`
/// calls it when a turn is about to start on `thread` for any reason, and
/// [`deliver`] calls it ahead of a waking mail. Puts every held message for
/// `thread` into its queue, oldest first, WITHOUT dispatching (the caller is
/// the dispatch). Returns how many were released.
///
/// **A ROW IS DELETED ONLY AFTER ITS ENQUEUE IS DURABLE** (R4.S3b, the
/// R4.S3-land inspector's F3). The enqueue is several writes (the message id,
/// the `UserMessage` event, the queue row), so it cannot share one transaction
/// with the delete without re-plumbing the send path; instead each row is
/// enqueued and then deleted, one at a time. A kill, or an enqueue that
/// errors, therefore leaves every not-yet-enqueued message HELD, and the next
/// turn start releases it. The one remaining window — killed after a row's
/// enqueue and before its delete — delivers that one message twice after the
/// restart; lost is the failure this replaced, and duplicated-once on a crash
/// is the trade (recorded in the R4.S3b report).
///
/// `RELEASE` serializes releases in this process, so the two callers
/// (`deliver` and `maybe_dispatch`) racing on one thread cannot both enqueue
/// the same row: the at-most-once that `take_held_mail`'s single transaction
/// gave, kept without its loss.
pub fn release_held(manager: &ThreadManager, thread: &str) -> Result<usize> {
    static RELEASE: Mutex<()> = Mutex::new(());
    let _one = RELEASE.lock().unwrap_or_else(|e| e.into_inner());
    let held = manager.store.held_mail(thread)?;
    let n = held.len();
    for (i, h) in held.into_iter().enumerate() {
        release_halted(thread, i)?;
        let metadata: Value = serde_json::from_str(&h.metadata_json).unwrap_or(Value::Null);
        manager.enqueue_undispatched(thread, h.text, SendIntent::TurnEnd, metadata)?;
        manager.store.delete_held_mail(h.id)?;
    }
    Ok(n)
}

/// **WHERE A TEST MAY STOP [`release_held`], AS A KILL WOULD** (R4.S3b, the
/// R4.S3-land inspector's F3). `(thread, n)`: the next release for `thread`
/// stops just before it enqueues its `n`th held message (0-based) — the
/// instant a `SIGKILL`, or an enqueue that errors, would leave. Keyed by
/// thread so tests running in parallel in one process cannot trip each other.
/// Never set outside a test.
static RELEASE_HALT: Mutex<Option<(String, usize)>> = Mutex::new(None);

/// **TEST ONLY.** The next [`release_held`] for `thread` stops before its
/// `nth` enqueue, as a kill would.
#[doc(hidden)]
pub fn halt_release_before(thread: &str, nth: usize) {
    *RELEASE_HALT.lock().unwrap_or_else(|e| e.into_inner()) = Some((thread.to_string(), nth));
}

fn release_halted(thread: &str, i: usize) -> Result<()> {
    let mut h = RELEASE_HALT.lock().unwrap_or_else(|e| e.into_inner());
    if matches!(&*h, Some((t, n)) if t == thread && *n == i) {
        *h = None;
        return Err(Error::Config(format!("release_held halted before enqueue {i} (test kill point)")));
    }
    Ok(())
}

fn header(kind: &str, from: &str, net: &Net) -> String {
    format!("[{kind} from agent {from} over network \"{}\" ({})]", net.name, net.id)
}

/// The command's acknowledgement: the fact is a `Custom` event on the
/// caller's own ledger, which is what resolves the promise.
fn ack(manager: &ThreadManager, caller: &str, event_type: &str, data: Value, extra: Option<(&str, Value)>) -> Result<Value> {
    let promise = manager
        .promises
        .open::<crate::commands::ThreadsEvent>(Some(caller.to_string()));
    let (_, visible) = manager.append_custom(caller, event_type.to_string(), data, false)?;
    let mut v = promise.accepted(visible).changed(true).json();
    if let (Some((k, val)), Some(obj)) = (extra, v.as_object_mut()) {
        obj.insert(k.to_string(), val);
    }
    Ok(v)
}

fn req<'a>(params: &'a Value, key: &str) -> Result<&'a Value> {
    params.get(key).ok_or_else(|| malformed(format!("missing required param: {key}")))
}

fn req_str(params: &Value, key: &str) -> Result<String> {
    req(params, key)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| malformed(format!("`{key}` must be a string")))
}

fn check_size(net: &Net, body: &str) -> Result<()> {
    let n = body.chars().count();
    if n > net.rules.max_chars {
        return Err(refused(
            refusal::TOO_LARGE,
            format!(
                "too_large: {n} chars over network `{}`'s max_chars of {}. Refused, not truncated",
                net.id, net.rules.max_chars
            ),
        ));
    }
    Ok(())
}

fn check_rules(rules: &Rules) -> Result<()> {
    if rules.max_chars > MAX_MESSAGE_CHARS || rules.max_chars == 0 {
        return Err(refused(
            refusal::TOO_LARGE,
            format!(
                "too_large: max_chars {} is outside 1..={MAX_MESSAGE_CHARS} (MAX_MESSAGE_CHARS). Refused, not clamped",
                rules.max_chars
            ),
        ));
    }
    Ok(())
}

// ═════════════════════════════════════════════════════════════════════════════
// commands — each acknowledges; the fact is on the caller's ledger
// ═════════════════════════════════════════════════════════════════════════════

/// `agents/{agent}/message`.
pub fn message(manager: &ThreadManager, params: &Value) -> Result<Value> {
    let caller = req_str(params, crate::agents::CALLER)?;
    let to = req_str(params, "agent")?;
    let body = req_str(params, "body")?;
    let intent = parse_intent(params)?;
    // R10.M3: what the caller wants done with `to`'s answers over this link —
    // judged before anything is written; absent inherits the link (§2.1).
    let notify = policy::parse(params, "agents_message")?;
    let named = params.get("network").and_then(Value::as_str).map(str::to_string);
    let _g = ACTS.lock().map_err(|_| Error::Internal("networks lock poisoned".into()))?;
    let (net, roster) = route(manager, &caller, &to, named.as_deref())?;
    check_size(&net, &body)?;
    let target = roster.get(&to).copied().unwrap_or_default();
    if intent == "interrupt" {
        if !target.interruptible {
            return Err(refused(
                refusal::NOT_INTERRUPTIBLE,
                format!("not_interruptible: `{to}` is not interruptible over `{}`", net.id),
            ));
        }
    }
    // The message carries the policy for its answer (§2.1): set the link
    // BEFORE delivery, so an answer can never race ahead of it.
    if let Some(n) = notify {
        policy::set(manager, &caller, &to, &net.id, n, json!({"act": "agents_message"}))?;
    }
    let text = format!("{}\n{body}", header("mail", &caller, &net));
    let woken = wakes(manager, &roster, &caller, &to, &net.id)?;
    let held = deliver(manager, &net, &caller, &to, "message", text, intent, woken)?;
    // Delivery grants a route back (§1.5).
    manager.store.add_route(&net.id, &to, &caller, &now_rfc3339())?;
    // **NEVER SILENTLY HELD** (R10.W1): the sender is told, in the tool result
    // AND in its own ledger fact, whether the mail woke (or queued behind a
    // running turn of) the recipient, or was held until the recipient's next
    // turn opens for some other reason.
    let delivery = if held { "held" } else { "delivered" };
    let mut fact = json!({"to": to, "network": net.id, "intent": intent, "chars": body.chars().count(), "held": held, "delivery": delivery});
    if held {
        fact["reason"] = json!(if target.wakeable {
            format!(
                "`{to}` is dormant and holds mail from you over `{}` (its notify policy on you is `hold`); the mail rides its next turn",
                net.id
            )
        } else {
            format!(
                "`{to}` is dormant and not wakeable to you over `{}`, and no network you share makes it so; the mail rides its next turn",
                net.id
            )
        });
    }
    ack(manager, &caller, "mail_queued", fact, Some(("delivery", json!(delivery))))
}

/// `agents/{network}/broadcast` — fan-out at send time to every member the
/// caller SEES in the network (§3.4).
pub fn broadcast(manager: &ThreadManager, params: &Value) -> Result<Value> {
    let caller = req_str(params, crate::agents::CALLER)?;
    let id = req_str(params, "network")?;
    let body = req_str(params, "body")?;
    let intent = parse_intent(params)?;
    let _g = ACTS.lock().map_err(|_| Error::Internal("networks lock poisoned".into()))?;
    let net = load(manager, &id)?.ok_or_else(|| no_route(format!("broadcast over `{id}`")))?;
    let roster = members(manager, &net)?;
    if !roster.contains_key(&caller) {
        return Err(no_route(format!("broadcast over `{id}`")));
    }
    let to: Vec<(String, Flags)> = roster
        .iter()
        .filter(|(t, _)| sees_in(&roster, &caller, t))
        .map(|(t, f)| (t.clone(), *f))
        .collect();
    if to.is_empty() {
        return Err(no_route(format!("broadcast over `{id}`: you see nobody in it")));
    }
    check_size(&net, &body)?;
    if intent == "interrupt" {
        if let Some((t, _)) = to.iter().find(|(_, f)| !f.interruptible) {
            return Err(refused(
                refusal::NOT_INTERRUPTIBLE,
                format!("not_interruptible: `{t}` is not interruptible over `{}`; a broadcast with intent `interrupt` is all or nothing, and nothing was sent", net.id),
            ));
        }
    }
    let text = format!("{}\n{body}", header("broadcast", &caller, &net));
    let mut held = 0usize;
    for (t, _) in &to {
        let woken = wakes(manager, &roster, &caller, t, &net.id)?;
        if deliver(manager, &net, &caller, t, "broadcast", text.clone(), intent, woken)? {
            held += 1;
        }
        manager.store.add_route(&net.id, t, &caller, &now_rfc3339())?;
    }
    manager.store.record_broadcast(&net.id, &caller, &body, &now_rfc3339())?;
    ack(
        manager,
        &caller,
        "mail_queued",
        json!({"broadcast": true, "network": net.id, "recipients": to.iter().map(|(t, _)| t).collect::<Vec<_>>(), "held": held, "intent": intent}),
        None,
    )
}

/// One checked add: the member, its flags, whether the adder holds authority
/// over it, and the network the adder SEES it through (where an offer rides).
struct Admit {
    thread: String,
    flags: Flags,
    authority: bool,
    via: Net,
}

/// **N4, CHECKED FOR EVERY ADD BEFORE ANYTHING IS WRITTEN** — so a refusal
/// leaves no partial network and no partial membership.
fn plan_admits(manager: &ThreadManager, caller: &str, adds: &BTreeMap<String, Flags>) -> Result<Vec<Admit>> {
    let mut plan = Vec::new();
    for (t, f) in adds {
        if t == caller {
            return Err(malformed("you are already a member; set your own flags with `configure`"));
        }
        // N4: sight BY FLAG, at the moment of the add. Not a route, not an id
        // read somewhere.
        let Some(via) = sees(manager, caller, t)? else {
            return Err(no_route(format!("`{caller}` adding `{t}`")));
        };
        plan.push(Admit { thread: t.clone(), flags: *f, authority: authority(manager, caller, t)?, via });
    }
    Ok(plan)
}

/// Write a checked plan into created network `net`: under authority at once,
/// otherwise as an offer delivered as mail.
fn apply_admits(manager: &ThreadManager, caller: &str, net: &Net, plan: Vec<Admit>) -> Result<(Vec<String>, Vec<String>)> {
    let now = now_rfc3339();
    let (mut admitted, mut offered) = (Vec::new(), Vec::new());
    for Admit { thread: t, flags: f, authority: auth, via } in plan {
        manager.store.upsert_member(&MemberRow {
            network_id: net.id.clone(),
            thread_id: t.clone(),
            flags_json: serde_json::to_string(&f)?,
            state: if auth { "member" } else { "pending" }.to_string(),
            joined_by: caller.to_string(),
            joined_at: now.clone(),
        })?;
        if auth {
            replay_history(manager, net, &t)?;
            admitted.push(t);
        } else {
            // An offer is mail to its one addressee, over the network the
            // adder saw it through (N7's one exception).
            let wake = members(manager, &via)?.get(&t).map(|x| x.wakeable).unwrap_or(false);
            let text = format!(
                "{}\nYou are offered membership of network \"{}\" ({}) with flags {}. Accept with \
                 agents_network_join {{network: \"{}\"}}, or ignore it.",
                header("network offer", caller, &via),
                net.name,
                net.id,
                serde_json::to_string(&f)?,
                net.id
            );
            deliver(manager, &via, caller, &t, "offer", text, "immediate", wake)?;
            offered.push(t);
        }
    }
    Ok((admitted, offered))
}

fn replay_history(manager: &ThreadManager, net: &Net, joiner: &str) -> Result<()> {
    if !net.rules.history {
        return Ok(());
    }
    let wake = members(manager, net)?.get(joiner).map(|f| f.wakeable).unwrap_or(false);
    for (sender, body) in manager.store.network_broadcasts(&net.id)? {
        if sender == joiner {
            continue;
        }
        let text = format!("{} (history)\n{body}", header("broadcast", &sender, net));
        deliver(manager, net, &sender, joiner, "broadcast", text, "turn_end", wake)?;
    }
    Ok(())
}

fn parse_member_map(v: Option<&Value>) -> Result<BTreeMap<String, Flags>> {
    match v {
        None | Some(Value::Null) => Ok(BTreeMap::new()),
        Some(Value::Object(m)) => m
            .iter()
            .map(|(k, f)| Ok((k.clone(), parse::<Flags>(&format!("members.{k}"), f)?)))
            .collect(),
        Some(_) => Err(malformed("`members` must be an object of thread id -> flags")),
    }
}

/// `agents/network_create`.
pub fn create(manager: &ThreadManager, params: &Value) -> Result<Value> {
    let caller = req_str(params, crate::agents::CALLER)?;
    let name = req_str(params, "name")?;
    if name.trim().is_empty() {
        return Err(malformed("`name` must be non-empty"));
    }
    let rules: Rules = match params.get("rules") {
        None | Some(Value::Null) => Rules::default(),
        Some(v) => parse("rules", v)?,
    };
    check_rules(&rules)?;
    let me: Flags = match params.get("self") {
        None | Some(Value::Null) => Flags::creator(),
        Some(v) => parse("self", v)?,
    };
    let adds = parse_member_map(params.get("members"))?;
    let _g = ACTS.lock().map_err(|_| Error::Internal("networks lock poisoned".into()))?;
    if manager.store.count_networks_created_by(&caller)? >= MAX_NETWORKS_PER_THREAD {
        return Err(refused(
            refusal::ABOVE_CEILING,
            format!("above_ceiling: a thread may create at most {MAX_NETWORKS_PER_THREAD} networks (MAX_NETWORKS_PER_THREAD)"),
        ));
    }
    if adds.len() + 1 > MAX_MEMBERS {
        return Err(refused(refusal::ABOVE_CEILING, format!("above_ceiling: at most {MAX_MEMBERS} members (MAX_MEMBERS)")));
    }
    let plan = plan_admits(manager, &caller, &adds)?;
    let now = now_rfc3339();
    let id = uuid::Uuid::new_v4().to_string();
    manager.store.upsert_network(&NetworkRow {
        id: id.clone(),
        name: name.clone(),
        created_by: caller.clone(),
        rules_json: serde_json::to_string(&rules)?,
        created_at: now.clone(),
    })?;
    manager.store.upsert_member(&MemberRow {
        network_id: id.clone(),
        thread_id: caller.clone(),
        flags_json: serde_json::to_string(&me)?,
        state: "member".into(),
        joined_by: caller.clone(),
        joined_at: now,
    })?;
    let net = load(manager, &id)?.ok_or_else(|| Error::Internal("network vanished".into()))?;
    let (admitted, offered) = apply_admits(manager, &caller, &net, plan)?;
    ack(
        manager,
        &caller,
        "network_created",
        json!({"network": id, "name": name, "rules": rules, "self": me, "admitted": admitted, "offered": offered}),
        Some(("network", Value::String(id.clone()))),
    )
}

/// `agents/{network}/network_admit`.
pub fn admit(manager: &ThreadManager, params: &Value) -> Result<Value> {
    let caller = req_str(params, crate::agents::CALLER)?;
    let id = req_str(params, "network")?;
    let adds = parse_member_map(Some(req(params, "members")?))?;
    let _g = ACTS.lock().map_err(|_| Error::Internal("networks lock poisoned".into()))?;
    let net = load(manager, &id)?.ok_or_else(|| no_route(format!("network `{id}`")))?;
    let roster = members(manager, &net)?;
    let Some(mine) = roster.get(&caller) else {
        return Err(no_route(format!("network `{id}`")));
    };
    if net.is_home() {
        return Err(refused(refusal::HOME, "home: a home network's membership is the subtree, derived; no command alters it"));
    }
    if !mine.members {
        return Err(refused(refusal::NO_RIGHT, format!("no_right: you lack `members` in `{id}`")));
    }
    if roster.len() + adds.len() > MAX_MEMBERS {
        return Err(refused(refusal::ABOVE_CEILING, format!("above_ceiling: at most {MAX_MEMBERS} members (MAX_MEMBERS)")));
    }
    let plan = plan_admits(manager, &caller, &adds)?;
    let (admitted, offered) = apply_admits(manager, &caller, &net, plan)?;
    ack(
        manager,
        &caller,
        "network_membership",
        json!({"act": "admit", "network": id, "admitted": admitted, "offered": offered}),
        None,
    )
}

/// `agents/{network}/network_join` — accept a pending offer.
pub fn join(manager: &ThreadManager, params: &Value) -> Result<Value> {
    let caller = req_str(params, crate::agents::CALLER)?;
    let id = req_str(params, "network")?;
    let _g = ACTS.lock().map_err(|_| Error::Internal("networks lock poisoned".into()))?;
    let no_offer = || refused(refusal::NO_OFFER, format!("no_offer: you hold no offer for `{id}`"));
    let net = load(manager, &id)?.ok_or_else(no_offer)?;
    let Some(flags) = pending_offer(manager, &net, &caller)? else {
        return Err(no_offer());
    };
    manager.store.upsert_member(&MemberRow {
        network_id: id.clone(),
        thread_id: caller.clone(),
        flags_json: serde_json::to_string(&flags)?,
        state: "member".into(),
        joined_by: caller.clone(),
        joined_at: now_rfc3339(),
    })?;
    replay_history(manager, &net, &caller)?;
    ack(manager, &caller, "network_membership", json!({"act": "join", "network": id, "flags": flags}), None)
}

/// `agents/{network}/network_leave`.
pub fn leave(manager: &ThreadManager, params: &Value) -> Result<Value> {
    let caller = req_str(params, crate::agents::CALLER)?;
    let id = req_str(params, "network")?;
    let _g = ACTS.lock().map_err(|_| Error::Internal("networks lock poisoned".into()))?;
    let net = load(manager, &id)?.ok_or_else(|| no_route(format!("network `{id}`")))?;
    if !members(manager, &net)?.contains_key(&caller) && pending_offer(manager, &net, &caller)?.is_none() {
        return Err(no_route(format!("network `{id}`")));
    }
    if net.is_home() {
        return Err(refused(refusal::HOME, "home: you cannot leave a home network; it is your lineage"));
    }
    manager.store.delete_member(&id, &caller)?;
    ack(manager, &caller, "network_membership", json!({"act": "leave", "network": id}), None)
}

/// `agents/{network}/network_remove`.
pub fn remove(manager: &ThreadManager, params: &Value) -> Result<Value> {
    let caller = req_str(params, crate::agents::CALLER)?;
    let id = req_str(params, "network")?;
    let gone: Vec<String> = parse("members", req(params, "members")?)?;
    let _g = ACTS.lock().map_err(|_| Error::Internal("networks lock poisoned".into()))?;
    let net = load(manager, &id)?.ok_or_else(|| no_route(format!("network `{id}`")))?;
    let roster = members(manager, &net)?;
    let Some(mine) = roster.get(&caller) else {
        return Err(no_route(format!("network `{id}`")));
    };
    if net.is_home() {
        return Err(refused(refusal::HOME, "home: a home network's membership is the subtree, derived; no command alters it"));
    }
    if !mine.members {
        return Err(refused(refusal::NO_RIGHT, format!("no_right: you lack `members` in `{id}`")));
    }
    for t in &gone {
        if !sees_in(&roster, &caller, t) {
            return Err(no_route(format!("`{t}` in `{id}`")));
        }
    }
    for t in &gone {
        manager.store.delete_member(&id, t)?;
    }
    ack(manager, &caller, "network_membership", json!({"act": "remove", "network": id, "removed": gone}), None)
}

/// `agents/{network}/network_configure`.
pub fn configure(manager: &ThreadManager, params: &Value) -> Result<Value> {
    let caller = req_str(params, crate::agents::CALLER)?;
    let id = req_str(params, "network")?;
    let rules: Option<Rules> = match params.get("rules") {
        None | Some(Value::Null) => None,
        Some(v) => Some(parse("rules", v)?),
    };
    let flags = parse_member_map(params.get("flags"))?;
    let _g = ACTS.lock().map_err(|_| Error::Internal("networks lock poisoned".into()))?;
    let net = load(manager, &id)?.ok_or_else(|| no_route(format!("network `{id}`")))?;
    let roster = members(manager, &net)?;
    let Some(mine) = roster.get(&caller).copied() else {
        return Err(no_route(format!("network `{id}`")));
    };
    if let Some(r) = &rules {
        if !mine.configure {
            return Err(refused(refusal::NO_RIGHT, format!("no_right: you lack `configure` in `{id}`")));
        }
        check_rules(r)?;
    }
    for (t, f) in &flags {
        if t == &caller {
            if f.is_subset_of(&mine) {
                continue; // a member may always lower its own flags
            }
            if !mine.configure {
                return Err(refused(refusal::NO_RIGHT, format!("no_right: raising your own flags needs `configure` in `{id}`")));
            }
            continue;
        }
        if !sees_in(&roster, &caller, t) {
            return Err(no_route(format!("`{t}` in `{id}`")));
        }
        if !mine.configure {
            return Err(refused(refusal::NO_RIGHT, format!("no_right: you lack `configure` in `{id}`")));
        }
        if !authority(manager, &caller, t)? {
            return Err(refused(refusal::NO_AUTHORITY, format!("no_authority: you hold no authority over `{t}`")));
        }
    }
    let now = now_rfc3339();
    if let Some(r) = &rules {
        manager.store.upsert_network(&NetworkRow {
            id: net.id.clone(),
            name: net.name.clone(),
            created_by: net.created_by.clone(),
            rules_json: serde_json::to_string(r)?,
            created_at: now.clone(),
        })?;
    }
    for (t, f) in &flags {
        if net.is_home() && manager.store.get_network(&net.id)?.is_none() {
            // The override rows of a home hang off a `networks` row so the
            // join in `networks_with_member` sees them; it carries the rules.
            manager.store.upsert_network(&NetworkRow {
                id: net.id.clone(),
                name: net.name.clone(),
                created_by: net.created_by.clone(),
                rules_json: serde_json::to_string(&net.rules)?,
                created_at: now.clone(),
            })?;
        }
        manager.store.upsert_member(&MemberRow {
            network_id: net.id.clone(),
            thread_id: t.clone(),
            flags_json: serde_json::to_string(f)?,
            state: "member".into(),
            joined_by: caller.clone(),
            joined_at: now.clone(),
        })?;
    }
    ack(
        manager,
        &caller,
        "network_configured",
        json!({"network": id, "rules": rules, "flags": flags}),
        None,
    )
}

// ═════════════════════════════════════════════════════════════════════════════
// the read — `agents_reach`
// ═════════════════════════════════════════════════════════════════════════════

fn run_state(manager: &ThreadManager, t: &str) -> Result<&'static str> {
    Ok(if manager.store.get_running_turn(t)?.is_some() { "running" } else { "dormant" })
}

/// `agents/reach` — the caller's egocentric row (§4.1): every network it is in
/// and aware of, its own flags and rights, each peer it SEES by flag with the
/// exposure flags it may act on, its correspondents as routes, and pending
/// offers. A network in which it sees nobody, holds no right and has no route
/// is silent and absent (§5).
pub fn reach(manager: &ThreadManager, params: &Value) -> Result<Value> {
    let caller = req_str(params, crate::agents::CALLER)?;
    let routes = manager.store.routes_of(&caller)?;
    let mut nets = Vec::new();
    for n in networks_of(manager, &caller)? {
        let roster = members(manager, &n)?;
        let mine = roster.get(&caller).copied().unwrap_or_default();
        let mut peers = Vec::new();
        for (t, f) in &roster {
            if sees_in(&roster, &caller, t) {
                peers.push(json!({
                    "id": t,
                    "run_state": run_state(manager, t)?,
                    "wakeable": f.wakeable,
                    "interruptible": f.interruptible,
                    "inspectable": f.inspectable,
                    "imposable": f.imposable,
                }));
            }
        }
        let correspondents: Vec<&String> = routes
            .iter()
            .filter(|(nid, t)| nid == &n.id && !peers.iter().any(|p| p["id"] == json!(t)))
            .map(|(_, t)| t)
            .collect();
        if peers.is_empty() && correspondents.is_empty() && !mine.members && !mine.configure {
            continue;
        }
        nets.push(json!({
            "id": n.id,
            "name": n.name,
            "derived": n.is_home(),
            "rules": n.rules,
            "self": mine,
            "peers": peers,
            "correspondents": correspondents,
        }));
    }
    let mut pending = Vec::new();
    for row in manager.store.networks_with_member(&caller, "pending")? {
        if let Some(n) = load(manager, &row.id)? {
            pending.push(json!({
                "id": n.id,
                "name": n.name,
                "offered": pending_offer(manager, &n, &caller)?,
            }));
        }
    }
    Ok(json!({"thread_id": caller, "networks": nets, "pending": pending}))
}
