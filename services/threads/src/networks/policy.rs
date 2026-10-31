//! # Notify policy — R10.M3, `docs/MESSAGING-ORCHESTRATION.md` §2.2, §4, §7.1
//!
//! **A link has exactly one current policy, the latest written.** A link is
//! (owner, peer, network): what `owner` wants done with mail and replies that
//! `peer` sends it over `network`. Two words are live:
//!
//! | `notify` | mail / reply from the peer | wakes the owner? |
//! |---|---|---|
//! | `direct` | its own message on the owner's queue, eager | yes |
//! | `hold`   | its own message on the owner's queue, `ride` (or held, for dormant mail) | no — it rides whatever turn comes next |
//!
//! `{"gather": …}` is the third (§2.2) and is R10.M5's: it is on the schema
//! and refused `not_available_yet` naming R10.M5, never accepted and ignored.
//!
//! **NO ROW IS `direct`** ([`DEFAULT`]) — T-19's *immediate notification as
//! the default assumption*, the operator's ruling for R10.M3 ("`direct` as the
//! default"). This replaces R10.W1's approximation, which spelled the default
//! as the home owner's `wakeable` flag (`Flags::home_owner`): that flag is per
//! (network, member) and cannot say "wake me for THIS child, not that one".
//! The flag stays, as the network's CAP (§4 item 3): "the tool call asks; the
//! network caps". A `direct` link into an owner whose home `wakeable` is off is
//! accepted and the reply rides (§8, Q-7: `wake_capped` is not a refusal).
//!
//! **Who writes it.** Only the owner, by its own acts (§4: "written only by the
//! link's recipient side"): `notify` on `agents_spawn` (the spawn's links),
//! `notify` on `agents_message` (the link the message travels), and
//! `agents_policy` (changes links without sending anything). Each write is a
//! row in `notify_policy` AND a `notify_policy` event on the owner's ledger
//! (§6.3), so the history is in `events` and the table is its projection.
//!
//! **Who reads it.** [`super::post`] — the one door mail and replies take onto
//! a recipient — through [`ride_reason`], and [`super::message`] /
//! [`super::broadcast`] for mail to a DORMANT owner, which a `hold` link holds
//! (and the sender is told `held`, never silently, R10.W1).
//!
//! **A `notify` on a peer you do not reach is `no_route`** (§8), the networks'
//! one answer for "does not exist" and "you may not see it" (N2), and nothing
//! is written.

use serde_json::{json, Value};
use shapewire::envelope::codes;

use super::{ack, load, members, networks_of, no_route, refused, sees_in, Net, ACTS, HOME_PREFIX};
use crate::error::{Error, Result};
use crate::runtime::ThreadManager;
use crate::store::now_rfc3339;

/// The ledger event a policy write appends on the OWNER's ledger (§6.3).
pub const EVENT: &str = "notify_policy";

/// `dispatch_changed.reason` when a link's `hold` makes a message ride.
pub const RIDE_HOLD: &str = "notify_hold";
/// `dispatch_changed.reason` when the owner's home-network cap (its own
/// `wakeable` there, lowered) makes a `direct` reply ride (§4 item 3, Q-7).
pub const RIDE_CAPPED: &str = "wake_capped";

/// One link's policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notify {
    Direct,
    Hold,
}

/// **THE DEFAULT: A LINK NO ACT HAS SET IS `direct`.** The one place it is
/// spelled; [`get`] is its one reader.
pub const DEFAULT: Notify = Notify::Direct;

impl Notify {
    pub fn word(self) -> &'static str {
        match self {
            Notify::Direct => "direct",
            Notify::Hold => "hold",
        }
    }
    fn from_word(w: &str) -> Option<Self> {
        match w {
            "direct" => Some(Notify::Direct),
            "hold" => Some(Notify::Hold),
            _ => None,
        }
    }
}

/// **`notify` AS AN ACT STATES IT.** `None` when absent (`agents_message`:
/// inherit the link; `agents_spawn`: no row, so [`DEFAULT`]). `{"gather": …}`
/// is refused `not_available_yet` naming R10.M5; anything else `malformed`.
/// Refused before anything is written.
pub fn parse(params: &Value, act: &str) -> Result<Option<Notify>> {
    match params.get("notify") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(w)) => Notify::from_word(w).map(Some).ok_or_else(|| {
            refused(codes::MALFORMED, format!("`{act}` `notify` is \"direct\" | \"hold\" | {{\"gather\": …}}, got {w:?}. Refused rather than ignored; nothing was done"))
        }),
        Some(Value::Object(o)) if o.contains_key("gather") => Err(refused(
            codes::NOT_AVAILABLE_YET,
            format!(
                "`{act}` `notify: {{\"gather\": …}}` cannot be honoured on this daemon yet: gathers are R10.M5 \
                 (`docs/MESSAGING-ORCHESTRATION.md` §3). Use \"direct\" or \"hold\". Refused rather than ignored; nothing was done"
            ),
        )),
        Some(other) => Err(refused(
            codes::MALFORMED,
            format!("`{act}` `notify` is \"direct\" | \"hold\" | {{\"gather\": …}}, got {other}. Refused rather than ignored; nothing was done"),
        )),
    }
}

/// **THE LINK'S CURRENT POLICY** — its latest written row, or [`DEFAULT`].
pub fn get(m: &ThreadManager, owner: &str, peer: &str, network: &str) -> Result<Notify> {
    match m.store.notify_policy(owner, peer, network)? {
        None => Ok(DEFAULT),
        Some(w) => Notify::from_word(&w)
            .ok_or_else(|| Error::Internal(format!("notify_policy ({owner}, {peer}, {network}) holds {w:?}"))),
    }
}

/// Write one link: the row, then the owner's `notify_policy` event (§6.3).
pub fn set(m: &ThreadManager, owner: &str, peer: &str, network: &str, n: Notify, set_by: Value) -> Result<()> {
    m.store
        .set_notify_policy(owner, peer, network, n.word(), &set_by.to_string(), &now_rfc3339())?;
    m.append_custom(
        owner,
        EVENT.to_string(),
        json!({"peer": peer, "network": network, "notify": n.word(), "set_by": set_by}),
        false,
    )?;
    Ok(())
}

/// **WHY [`super::post`] PUTS A MESSAGE ON `ride` INSTEAD OF WAKING `to`** —
/// or `None` (eager). Read off the message's own metadata, which every mail
/// and reply carries: `mail.network` is the network it travelled, the peers
/// are `children` (a spawn report may batch several) or else `mail.from`.
///
/// 1. `hold` — every peer's link into `to` over that network is `hold`. A
///    batch with one `direct` member wakes (the member that asked to be told
///    must be told).
/// 2. the CAP — the network is `to`'s own home and `to` lowered its own
///    `wakeable` there (§4 item 3, §7.1: `direct -> post(P, R, dispatch =
///    eager if cap=allowed else ride)`).
///
/// A message with no `mail.network` (the operator's own words, an
/// application's send) has no link and is never ridden here.
pub fn ride_reason(m: &ThreadManager, to: &str, metadata: &Value) -> Result<Option<&'static str>> {
    let mail = &metadata["mail"];
    let Some(network) = mail["network"].as_str() else { return Ok(None) };
    let peers: Vec<&str> = match metadata["children"].as_array() {
        Some(a) => a.iter().filter_map(Value::as_str).collect(),
        None => mail["from"].as_str().into_iter().collect(),
    };
    if !peers.is_empty() {
        let mut all_hold = true;
        for p in &peers {
            if get(m, to, p, network)? != Notify::Hold {
                all_hold = false;
                break;
            }
        }
        if all_hold {
            return Ok(Some(RIDE_HOLD));
        }
    }
    if network.strip_prefix(HOME_PREFIX) == Some(to) {
        if let Some(net) = load(m, network)? {
            if !members(m, &net)?.get(to).map(|f| f.wakeable).unwrap_or(true) {
                return Ok(Some(RIDE_CAPPED));
            }
        }
    }
    Ok(None)
}

/// The networks over which `a` reaches `b` — sees it by flag, or holds a
/// route back to it — in [`networks_of`] order; only `named` when named.
/// Empty is `no_route` to the caller.
fn reached(m: &ThreadManager, a: &str, b: &str, named: Option<&str>) -> Result<Vec<Net>> {
    let candidates: Vec<Net> = match named {
        Some(id) => load(m, id)?.into_iter().collect(),
        None => networks_of(m, a)?,
    };
    let mut out = Vec::new();
    for n in candidates {
        let roster = members(m, &n)?;
        if a == b || !roster.contains_key(a) || !roster.contains_key(b) {
            continue;
        }
        if sees_in(&roster, a, b) || m.store.has_route(&n.id, a, b)? {
            out.push(n);
        }
    }
    Ok(out)
}

/// **`agents/{agent}/policy` — CHANGE A LINK WITHOUT SENDING ANYTHING** (§8).
/// `caller` is the owner, `agent` the peer. With `network`, that one link;
/// without, every network in which the caller reaches the peer ("stop waking
/// me for this agent", however it reaches me). Acknowledges; the fact is
/// `notify_policy` on the caller's own ledger. Refused `no_route` when the
/// caller reaches the peer nowhere (or not over the named network) — the
/// same answer whether or not the peer exists — and nothing is written.
pub fn command(m: &ThreadManager, params: &Value) -> Result<Value> {
    let caller = super::req_str(params, crate::agents::CALLER)?;
    let agent = super::req_str(params, "agent")?;
    let n = parse(params, "agents_policy")?
        .ok_or_else(|| refused(codes::MALFORMED, "missing required param: notify (`agents_policy`)"))?;
    let named = match params.get("network") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(other) => return Err(refused(codes::MALFORMED, format!("`network` must be a network id, got {other}"))),
    };
    let _g = ACTS.lock().map_err(|_| Error::Internal("networks lock poisoned".into()))?;
    let nets = reached(m, &caller, &agent, named.as_deref())?;
    if nets.is_empty() {
        return Err(no_route(match &named {
            Some(id) => format!("a notify policy from `{caller}` on `{agent}` over `{id}`"),
            None => format!("a notify policy from `{caller}` on `{agent}`"),
        }));
    }
    let now = now_rfc3339();
    let set_by = json!({"act": "agents_policy"});
    let ids: Vec<String> = nets.iter().map(|n| n.id.clone()).collect();
    for id in &ids {
        m.store.set_notify_policy(&caller, &agent, id, n.word(), &set_by.to_string(), &now)?;
    }
    ack(
        m,
        &caller,
        EVENT,
        json!({"peer": agent, "networks": ids, "notify": n.word(), "set_by": set_by}),
        None,
    )
}
