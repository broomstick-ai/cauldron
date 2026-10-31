//! # The `agents` façade — `docs/managed-subagents.md` §2 on the MCP door
//!
//! **One act has one method path. The MCP tool is a façade that binds the
//! subject into that path.** That is §2.1's rule, and this module is the
//! façade. Say it out loud, because the design asks for exactly that:
//!
//! > This cannot reintroduce the two-spellings hazard `bind_subject` exists to
//! > refuse, because the agent only ever sends params; the façade is the sole
//! > writer of the path. Say this out loud in the façade's own docs, or someone
//! > will re-derive the params form as a second entry point.
//!
//! So:
//!
//! * **The method path is the act's one home.** `agents/{agent}/message` is a
//!   template in [`crate::server::WS_METHODS`] with an arm in
//!   [`crate::server::handle`], exactly like `threads/{thread_id}/send`. It is
//!   reachable over the chassis's `/ws` door by its path, and over the `/mcp`
//!   door by its tool name. Two carriers, one producer, one arm.
//! * **The tool name is the path with `/` → `_` and the subject segment
//!   dropped**, mechanically: `agents/{agent}/message` is `agents_message`.
//!   [`Tool::mcp_name_of`] is that derivation and `the_surface_is_exactly_section_2`
//!   pins every name to it, so a tool cannot be named by hand.
//! * **The agent sends the subject as a PARAMETER** (`agent`, `network`), and
//!   this façade — [`bind_path`] — takes it OUT of the arguments and writes
//!   the concrete path. What then reaches `route` + `bind_subject` has the
//!   subject in the path and NOT in the params, which is the only spelling
//!   `bind_subject` accepts. The façade is the sole writer of the path; the
//!   agent never writes one. A façade that left the parameter in place would
//!   be refused by `bind_subject` as two spellings — and that is the forced
//!   red for this rule, not a hazard it introduces.
//! * **There is no params-form entry point for an agent.** Do not add one. An
//!   agent that wants `agents/{agent}/message` calls `agents_message` with
//!   `agent`; an operator-side consumer that wants it calls the path on `/ws`.
//!   Neither sends both.
//!
//! ## The caller is the token's, and it is the SECOND thing an agent may not spell
//!
//! Every act here is about *the caller's* children, log, networks. Over `/mcp`
//! the caller is the verified run token's `thread_id`
//! ([`shapewire::token::Claims`]), and this façade binds it into the params
//! under [`CALLER`] — the handler reads it there, as it reads `thread_id`
//! after `bind_subject`. An argument named `caller` in a tool call is refused
//! (`malformed`), for `bind_subject`'s own reason: a precedence rule between
//! *who the token says you are* and *who you say you are* is a place for the
//! two to disagree, and deleting that place is the whole point of the token.
//! Over `/ws` there is no token — that door is the operator's, and it already
//! serves every thread's history — so a `/ws` caller states `caller` itself.
//!
//! ## LIVE versus typed-refused, on this tree
//!
//! R4.S1 builds the surface and the carrier. **R4.S2 makes four tools live** —
//! `agents_spawn`, `agents_list`, `agents_result`, and `agents_log` beyond the
//! caller's own thread — over the managed runtime in [`crate::managed`]
//! (durable ledgers; lineage is `ThreadMeta.parent_id` in them, not a
//! `thread_lineage` table). Still NOT built: the `agents` shape and its facts,
//! networks (R4.S3), holds, suspensions, `gather`, `message`. The table below
//! is R4.S1's, as it was written; the live column is now `SURFACE`'s `live`. The brief's rule: *"Where a tool's
//! resolving fact or method does not exist on trunk yet, the façade exposes the
//! tool with its full schema and REFUSES it typed (`not_available_yet` naming
//! the method path), so the surface is whole and honest."*
//!
//! | tool | kind | on this tree | resolved by (R4.S2) |
//! |---|---|---|---|
//! | `agents_spawn` | command | **LIVE** (R4.S2; `agent`, `effort`, `networks`, `network` R4.S8) | the child's `threads/create` promise |
//! | `agents_spawn_batch` | command | refused | one `RunStarted` per member |
//! | `agents_message` | command | **LIVE** (R4.S3; `notify` R10.M3) | `mail_queued` on the caller's ledger |
//! | `agents_policy` | command | **LIVE** (R10.M3) | `notify_policy` on the caller's ledger (`crate::networks::policy`) |
//! | `agents_broadcast` | command | **LIVE** (R4.S3) | one `mail_queued` on the caller's ledger |
//! | `agents_interrupt` | command | refused | `Fact::Held` |
//! | `agents_watch` | command | refused | `Fact::SuspensionArmed` |
//! | `agents_network_create` | command | **LIVE** (R4.S3) | `network_created` on the caller's ledger |
//! | `agents_network_admit` / `_join` / `_leave` / `_remove` | command | **LIVE** (R4.S3) | `network_membership` on the caller's ledger |
//! | `agents_network_configure` | command | **LIVE** (R4.S3) | `network_configured` on the caller's ledger |
//! | `agents_reach` | read | **LIVE** (R4.S3) | `crate::networks::reach` |
//! | `agents_list` | read | refused | rows of the `agents` shape |
//! | `agents_result` | read | refused | `agent_runs` |
//! | `agents_gather` | read | **LIVE** (R4.S8) | `crate::managed::gather`, the bounded block (§2.4, spelling (c)) |
//! | `agents_log` | read | **LIVE**, gated by READS (R4.S3) | rows of `threads/log` through `log::row_of` |
//!
//! `agents_log` is live because its resolving method exists: `threads/log`'s
//! rows, built by the same [`crate::shapes::log::row_of`] the shape's hydrate
//! and live fold use (§6: *"it must not become a second source: it reads the
//! same rows through the same `row_of` builder"*). What does NOT exist is the
//! relation that would authorise reading another thread's log — §6 makes that
//! a network question, and there are no networks and no lineage here — so until
//! R4.S2 a token reads only its own thread's log, and any other `agent` is
//! refused `not_available_yet` naming `thread_lineage`. That narrowing is an
//! ASSUMPTION recorded in the R4.S1 report; its reversal is one predicate in
//! [`log`].
//!
//! ## Commands acknowledge — and the façade checks it
//!
//! §2.2: *"No command returns a result, a status, a count, or a snapshot."*
//! Every command here will answer `Accepted{promise_id, visible, changed?}`
//! through `shapewire::command` once its fact exists; `agents_spawn` may add
//! `thread_id` and `run_id`, which are *addresses the act minted, not facts
//! the shape carries* — the `threads/create` precedent. [`acknowledgement_only`]
//! is the guard: a command arm whose reply carries any other key is refused at
//! the façade, so the rule holds at runtime and not only in review. Its forced
//! red is a command handler returning a body.
//!
//! ## Waiting and intent — the two operator questions, taken at their recommendations
//!
//! `agents_gather` is spelled (c): a bounded block that returns after `poll_s`
//! (default 60) with `{satisfied: false, pending: […]}` when the predicate
//! does not yet hold, so no request outlives one interval and the daemon's
//! restart gate (`spawning == 0 && requests == 0`) is never held for a wait.
//! The surface — `until`, `timeout_s` — does not change under (a) or (b); the
//! difference is one function's body. `intent: immediate` means latency of
//! routing (§2.5), buildable today; `interrupt` is network-gated (the
//! `interruptible` flag) and, since R4.S8, cuts the recipient's running turn
//! (`ThreadManager::cancel`) and queues the message as the turn that follows.

use serde_json::{json, Map, Value};
use shapewire::envelope::{codes, Refusal};
use shapewire::mcp::ToolDef;
use shapewire::server::MethodPath;
use shapewire::token::Claims;

use crate::error::{Error, Result};
use crate::runtime::ThreadManager;
use crate::server::WS_METHODS;

/// **THE PARAMETER THE FAÇADE BINDS THE CALLER INTO.** Read by every `agents/*`
/// arm; written by this façade from the verified token, or by an operator-side
/// `/ws` caller stating it. Refused as a tool-call argument.
pub const CALLER: &str = "caller";

/// How many `threads/log` rows `agents_log` answers when the caller does not
/// say. Small, because the rows land in a model's context.
pub const LOG_DEFAULT_LIMIT: u64 = 100;

/// Whether a tool is a command (acknowledges) or a read (answers) — §2.2 / §2.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Command,
    Read,
}

/// One parameter of a tool's input schema.
#[derive(Debug, Clone, Copy)]
pub struct Param {
    pub name: &'static str,
    pub required: bool,
    /// The JSON Schema for this parameter, as JSON text. Parsed at
    /// [`Tool::input_schema`]; `every schema parses` is pinned by test.
    pub schema: &'static str,
}

/// One tool on the surface.
#[derive(Debug, Clone, Copy)]
pub struct Tool {
    /// The MCP tool name — derivable from `template` by [`Tool::mcp_name_of`].
    pub name: &'static str,
    /// The method path template, verbatim from [`WS_METHODS`].
    pub template: &'static str,
    pub kind: Kind,
    pub description: &'static str,
    pub params: &'static [Param],
    /// Does this tool's resolving fact or method exist on this tree?
    pub live: bool,
}

const S: &str = r#"{"type":"string"}"#;
/// **`notify`** (R10.M3, `docs/MESSAGING-ORCHESTRATION.md` §8) — one literal,
/// spliced into `agents_spawn`, `SPAWN_ITEMS`, `agents_message` and
/// `agents_policy`, so the four cannot drift.
macro_rules! notify_schema {
    () => {
        r#"{"description":"what to do with this agent's answers and mail to you over this link (R10.M3): \"direct\" (the default when no act set one — it wakes you) | \"hold\" (it rides your next turn and never starts one). {\"gather\": …} is R10.M5's and refused not_available_yet","oneOf":[{"type":"string","enum":["direct","hold"]},{"type":"object","required":["gather"],"properties":{"gather":{}}}]}"#
    };
}
const NOTIFY: &str = notify_schema!();
/// **`wake`, DEPRECATED (R10.M3b, Q-23 option B).**
/// `DEPRECATED(wake, remove after DEPLOY-9n)`: for ONE release `agents_spawn`
/// (and the batch's items) take `wake` again with its pre-R10.M3 meaning —
/// `never`: no spawn-report / run-end wake of the parent, the child's mail
/// still `direct` (never `hold`); `end`: the default. With `notify` beside it
/// the act is refused. `managed::spawn` honours it, answers [`WAKE_WARNING`]
/// and records `deprecated_param` on the caller's ledger (the census). To
/// remove: delete this param and its `SPAWN_ITEMS` property, and the
/// [`REMOVED`] entry refuses it again on spawn.
macro_rules! wake_schema {
    () => {
        r#"{"type":"string","enum":["end","never"],"description":"DEPRECATED (R10.M3b), refused next release: omit it to be woken by receipts (`notify: direct`, the default), or use `agents_policy`. `never`: no run-end wake of you, the child's mail still wakes you; `end`: the default. Refused beside `notify`"}"#
    };
}
const WAKE: &str = wake_schema!();
/// The `warning` a spawn carrying `wake` is answered with (R10.M3b).
pub const WAKE_WARNING: &str = "`wake` is deprecated and will be refused next release; omit it to be woken by receipts, or use `agents_policy`.";
/// **REMOVED PARAMETERS, AND WHAT REPLACED EACH.** A call carrying one is
/// refused `unknown_param` NAMING the replacement — never ignored, never
/// silently mapped (R10.M3: `wake` → `notify`). A tool that still lists the
/// name among its params (R10.M3b: `agents_spawn` takes the deprecated
/// `wake`) is not refused by this table.
const REMOVED: &[(&str, &str, &str)] = &[(
    "wake",
    "notify",
    "`wake` was removed by R10.M3: use `notify` — \"direct\" (the default; the child's answer wakes you; was `wake: end`) or \"hold\" (it rides your next turn; was `wake: never`)",
)];
const INT: &str = r#"{"type":"integer","minimum":0}"#;
const STRINGS: &str = r#"{"type":"array","items":{"type":"string"}}"#;
const INTENT: &str = r#"{"type":"string","enum":["turn_end","immediate","interrupt"],"description":"turn_end: deliver when the owner finishes its current work. immediate (default): deliver at the owner's next turn boundary, which the daemon starts the moment it can — latency of routing, not mid-thought arrival. interrupt: cut the owner's running turn and restart it carrying this message; network-gated, never the default."}"#;
const RUNS: &str = r#"{"description":"run ids, or the literal \"children\" for every direct child of the caller","oneOf":[{"type":"array","items":{"type":"string"}},{"type":"string","enum":["children"]}]}"#;
/// The eight per-member flags of `docs/agent-networks.md` §1.2. Absent is false.
const FLAGS: &str = r#"{"type":"object","additionalProperties":false,"properties":{"view":{"type":"boolean","description":"you see every member"},"visible":{"type":"boolean","description":"every member sees you"},"wakeable":{"type":"boolean","description":"mail over this network may wake you if dormant"},"interruptible":{"type":"boolean","description":"mail over this network may carry intent interrupt"},"inspectable":{"type":"boolean","description":"members that see you may read your ledger"},"imposable":{"type":"boolean","description":"members that see you hold authority over you"},"members":{"type":"boolean","description":"you may add and remove members"},"configure":{"type":"boolean","description":"you may change the rules and the flags of members you hold authority over"}}}"#;
const MEMBER_FLAGS: &str = r#"{"type":"object","additionalProperties":{"type":"object","additionalProperties":false,"properties":{"view":{"type":"boolean","description":"you see every member"},"visible":{"type":"boolean","description":"every member sees you"},"wakeable":{"type":"boolean","description":"mail over this network may wake you if dormant"},"interruptible":{"type":"boolean","description":"mail over this network may carry intent interrupt"},"inspectable":{"type":"boolean","description":"members that see you may read your ledger"},"imposable":{"type":"boolean","description":"members that see you hold authority over you"},"members":{"type":"boolean","description":"you may add and remove members"},"configure":{"type":"boolean","description":"you may change the rules and the flags of members you hold authority over"}}},"description":"thread id -> flags (view, visible, wakeable, interruptible, inspectable, imposable, members, configure; absent = false). A member you hold authority over is admitted at once; any other is OFFERED and must accept with agents_network_join. You may only add a thread you already SEE by flag in some network (introduction is non-transitive)."}"#;
const RULES: &str = r#"{"type":"object","additionalProperties":false,"properties":{"max_chars":{"type":"integer","minimum":1,"maximum":65536,"description":"per-message ceiling, default 8192; over it is refused too_large, never truncated"},"history":{"type":"boolean","description":"a joiner receives the network's prior broadcasts as mail; default false"}}}"#;
const UNTIL: &str = r#"{"type":"string","description":"the predicate: all | any | count:<n> | any_state:[<state>,…]"}"#;

/// **THE SURFACE — §2.2's commands and §2.3's reads, with the network acts of
/// `docs/agent-networks.md` §4.2 (R4.S3): admit, join, leave, remove,
/// configure, and the `agents_reach` read.**
pub const SURFACE: &[Tool] = &[
    Tool {
        name: "agents_spawn",
        template: "agents/spawn",
        kind: Kind::Command,
        description: "Spawn one managed subagent: a thread the daemon owns, as a child of the caller. Acknowledges with {promise_id, visible, changed?, thread_id, run_id, network?} (`network` when the spawn minted one); the child appears on the `threads` shape (its row resolves the promise). By default (`notify: direct`) you may end your turn: when your last outstanding child's run ends, the daemon starts your next turn carrying every child's report; `notify: hold` sets the link so its answers ride your next turn instead of starting one. Idempotent on `request_id`. A child always inherits its parent's budget key — `budget_key` is refused, not ignored. `wake` is DEPRECATED (R10.M3b): accepted this release with its old meaning and answered with a `warning`; refused next release.",
        params: &[
            Param { name: "task", required: true, schema: r#"{"type":"string","description":"the dispatch prompt"}"# },
            Param { name: "agent", required: false, schema: r#"{"type":"string","description":"an opaque agent type, passed through to --agent, never resolved by threads"}"# },
            Param { name: "model", required: false, schema: S },
            Param { name: "effort", required: false, schema: S },
            Param { name: "max_thinking_tokens", required: false, schema: INT },
            Param { name: "cwd", required: false, schema: S },
            Param { name: "append_system_prompt", required: false, schema: S },
            Param { name: "name", required: false, schema: S },
            Param { name: "json_schema", required: false, schema: r#"{"type":"object","description":"a JSON Schema (2020-12, at most 16 KiB serialized) for the child's answer to this task: passed as --json-schema on the turn that delivers the task, and on no later turn. Refused bad_schema, schema_too_large, before anything is spawned"}"# },
            Param { name: "schema_retries", required: false, schema: r#"{"type":"integer","minimum":0,"maximum":3,"description":"with json_schema only: how many corrective turns (each showing the validation errors) an answer that does not match the schema gets before it is reported schema_unmet; default 1"}"# },
            Param { name: "networks", required: false, schema: r#"{"description":"networks to admit the child to, each one you hold `members` in: {network id: flags} (absent flags false), or [network id] for view+visible+wakeable. Refused no_route, home, no_right, above_ceiling","oneOf":[{"type":"object","additionalProperties":{"type":"object"}},{"type":"array","items":{"type":"string"}}]}"# },
            Param { name: "network", required: false, schema: r#"{"type":"object","additionalProperties":false,"properties":{"name":{"type":"string"},"self":{"type":"object","description":"your flags in it; default view, visible, wakeable, members, configure"},"child":{"type":"object","description":"the child's flags; default view, visible, wakeable"},"rules":{"type":"object","description":"max_chars, history"}},"description":"mint a two-member network, you and the child, in the same act; its id is acknowledged as `network`"}"# },
            Param { name: "notify", required: false, schema: NOTIFY },
            // DEPRECATED(wake, remove after DEPLOY-9n) — R10.M3b.
            Param { name: "wake", required: false, schema: WAKE },
            Param { name: "request_id", required: false, schema: r#"{"type":"string","description":"defaults to this call's tool-use id; spawn is idempotent on (owner, request_id)"}"# },
        ],
        live: true,
    },
    Tool {
        name: "agents_spawn_batch",
        template: "agents/spawn_batch",
        kind: Kind::Command,
        description: "Spawn several subagents at once and mint one network over the whole batch, so the members know each other at birth. Acknowledges; one RunStarted per member arrives on the `agents` shape.",
        params: &[
            Param { name: "agents", required: true, schema: SPAWN_ITEMS },
            Param { name: "json_schema", required: false, schema: r#"{"type":"object","description":"the batch's default JSON Schema: every item that names no json_schema of its own spawns with this one"}"# },
            Param { name: "network", required: false, schema: r#"{"type":"object","properties":{"name":{"type":"string"},"rules":{"type":"object"}},"description":"the batch network's name and rules (max_chars, history); minted only when passed"}"# },
        ],
        live: false,
    },
    Tool {
        name: "agents_message",
        template: "agents/{agent}/message",
        kind: Kind::Command,
        description: "Send a message to one agent you SEE in some network (or that has messaged you there — delivery grants a route back). It lands durably on the recipient's ledger and queue, and wakes it only if it is `wakeable` to you in that network; otherwise it is held until something else opens its next turn. With no `network`, it rides one in which the recipient is wakeable to you whenever one exists. `notify` sets what is done with the recipient's ANSWERS over that link (direct: they wake you; hold: they ride your next turn); absent, the link keeps its policy (direct if never set). A recipient holding you (`hold`) gets your mail as `held` when dormant. Acknowledges with `delivery`: `delivered` (woken, or queued behind its running turn) or `held` — never silently; the fact is `mail_queued` on your own ledger, with the same word. Refused `no_route` (no network gives you a route — the same answer whether or not the address exists), `too_large` (names the network's max_chars), `not_interruptible`.",
        params: &[
            Param { name: "agent", required: true, schema: r#"{"type":"string","description":"the recipient's thread id — the SUBJECT; the façade binds it into agents/{agent}/message"}"# },
            Param { name: "body", required: true, schema: S },
            Param { name: "intent", required: false, schema: INTENT },
            Param { name: "network", required: false, schema: r#"{"type":"string","description":"which network carries this; defaults to the first in which you reach the recipient AND it is wakeable to you, else the first in which you reach it at all (your home networks nearest first, then networks you joined, oldest first)"}"# },
            Param { name: "notify", required: false, schema: NOTIFY },
        ],
        live: true,
    },
    Tool {
        name: "agents_policy",
        template: "agents/{agent}/policy",
        kind: Kind::Command,
        description: "Change what is done with one agent's answers and mail to you, without sending anything (R10.M3): `notify: direct` (they wake you — the default when nothing set it) or `hold` (they ride your next turn and never start one). With `network`, that one link; without, every network in which you reach the agent. The latest write wins; `notify` on agents_spawn / agents_message sets the same link. Acknowledges; the fact is `notify_policy` on your own ledger. Refused `no_route` (you reach that agent nowhere — the same answer whether or not it exists); `{\"gather\": …}` is refused not_available_yet (R10.M5).",
        params: &[
            Param { name: "agent", required: true, schema: r#"{"type":"string","description":"the peer whose answers this governs — the SUBJECT; the façade binds it into agents/{agent}/policy"}"# },
            Param { name: "notify", required: true, schema: NOTIFY },
            Param { name: "network", required: false, schema: r#"{"type":"string","description":"the one link to set; absent = every network in which you reach the agent"}"# },
        ],
        live: true,
    },
    Tool {
        name: "agents_broadcast",
        template: "agents/{network}/broadcast",
        kind: Kind::Command,
        description: "Fan one message out, at send time, to every member of a network that you SEE there (never yourself). A member who joins later does not get it unless the network's `history` rule is true. Acknowledges; the fact is one `mail_queued` on your own ledger. Refused `no_route` when you are not a member or see nobody in it; `too_large` is checked once against the network's max_chars.",
        params: &[
            Param { name: "network", required: true, schema: r#"{"type":"string","description":"the network id — the SUBJECT"}"# },
            Param { name: "body", required: true, schema: S },
            Param { name: "intent", required: false, schema: INTENT },
        ],
        live: true,
    },
    Tool {
        name: "agents_interrupt",
        template: "agents/{agent}/interrupt",
        kind: Kind::Command,
        description: "Place a hold on an agent (scope one) or on its whole subtree top-down (scope all). A hold is not a cancel: the run stays open as `held` and the next message to it continues the same run. Acknowledges; Held arrives on the `agents` shape.",
        params: &[
            Param { name: "agent", required: true, schema: r#"{"type":"string","description":"the thread id — the SUBJECT"}"# },
            Param { name: "scope", required: true, schema: r#"{"type":"string","enum":["one","all"]}"# },
        ],
        live: false,
    },
    Tool {
        name: "agents_watch",
        template: "agents/watch",
        kind: Kind::Command,
        description: "Arm a suspension and return immediately: when the predicate over the named runs holds, the daemon wakes the caller with an ordinary turn (action wake) or emits a fact (action emit). The caller may end its turn. Acknowledges; SuspensionArmed then SuspensionSatisfied arrive on the `agents` shape.",
        params: &[
            Param { name: "runs", required: true, schema: RUNS },
            Param { name: "until", required: true, schema: UNTIL },
            Param { name: "action", required: true, schema: r#"{"type":"string","enum":["wake","emit"]}"# },
            Param { name: "label", required: false, schema: S },
        ],
        live: false,
    },
    Tool {
        name: "agents_network_create",
        template: "agents/network_create",
        kind: Kind::Command,
        description: "Create a network: a named membership set with two rules (max_chars, history) and eight per-member boolean flags. You are a member with `self` flags (default view, visible, wakeable, members, configure). Each entry of `members` you hold authority over is admitted at once; any other is offered. Acknowledges with the minted `network` id; the fact is `network_created` on your own ledger. Refused `no_route` (a member you do not SEE by flag), `too_large` (max_chars over 65536), `above_ceiling` (64 networks per thread, 256 members).",
        params: &[
            Param { name: "name", required: true, schema: S },
            Param { name: "rules", required: false, schema: RULES },
            Param { name: "self", required: false, schema: FLAGS },
            Param { name: "members", required: false, schema: MEMBER_FLAGS },
        ],
        live: true,
    },
    Tool {
        name: "agents_network_admit",
        template: "agents/{network}/network_admit",
        kind: Kind::Command,
        description: "Add members to a network you hold `members` in: admitted at once where you hold authority, offered otherwise. Acknowledges; the fact is `network_membership` on your own ledger. Refused `no_right`, `no_route`, `home` (a home network's membership is your subtree, derived), `above_ceiling`.",
        params: &[
            Param { name: "network", required: true, schema: r#"{"type":"string","description":"the network id — the SUBJECT"}"# },
            Param { name: "members", required: true, schema: MEMBER_FLAGS },
        ],
        live: true,
    },
    Tool {
        name: "agents_network_join",
        template: "agents/{network}/network_join",
        kind: Kind::Command,
        description: "Accept an offer of membership (it arrived as mail, and shows under `pending` in agents_reach), with the flags it offered. Acknowledges; the fact is `network_membership` on your own ledger. Refused `no_offer` — also the answer for a network that does not exist.",
        params: &[
            Param { name: "network", required: true, schema: r#"{"type":"string","description":"the network id — the SUBJECT"}"# },
        ],
        live: true,
    },
    Tool {
        name: "agents_network_leave",
        template: "agents/{network}/network_leave",
        kind: Kind::Command,
        description: "Leave a network (or decline a pending offer). Acknowledges. Refused `home`: a home network is your lineage and cannot be left.",
        params: &[
            Param { name: "network", required: true, schema: r#"{"type":"string","description":"the network id — the SUBJECT"}"# },
        ],
        live: true,
    },
    Tool {
        name: "agents_network_remove",
        template: "agents/{network}/network_remove",
        kind: Kind::Command,
        description: "Remove members you see from a network you hold `members` in. Removal takes sight away and needs no authority. Acknowledges. Refused `no_right`, `no_route`, `home`.",
        params: &[
            Param { name: "network", required: true, schema: r#"{"type":"string","description":"the network id — the SUBJECT"}"# },
            Param { name: "members", required: true, schema: STRINGS },
        ],
        live: true,
    },
    Tool {
        name: "agents_network_configure",
        template: "agents/{network}/network_configure",
        kind: Kind::Command,
        description: "Change a network's rules (needs `configure`) and members' flags (needs `configure` and authority over that member; you may always LOWER your own). Works on your home network too. Acknowledges. Refused `no_right`, `no_authority`, `no_route`, `too_large`.",
        params: &[
            Param { name: "network", required: true, schema: r#"{"type":"string","description":"the network id — the SUBJECT; your home network is home:<your thread id>"}"# },
            Param { name: "rules", required: false, schema: RULES },
            Param { name: "flags", required: false, schema: MEMBER_FLAGS },
        ],
        live: true,
    },
    Tool {
        name: "agents_list",
        template: "agents/list",
        kind: Kind::Read,
        description: "Rows of the `agents` shape for the caller's own descendants: run state (running | held | parked | ended), outcome (completed | crashed), depth.",
        params: &[
            Param { name: "filter", required: false, schema: r#"{"type":"object","properties":{"state":{"type":"string","enum":["running","held","parked","ended"]},"outcome":{"type":"string","enum":["completed","crashed"]},"depth":{"type":"integer","minimum":0}}}"# },
        ],
        live: true,
    },
    Tool {
        name: "agents_reach",
        template: "agents/reach",
        kind: Kind::Read,
        description: "Your picture of who you can reach, read first thing every turn: each network you are in and aware of (home networks flagged `derived`), its rules, your own flags, each peer you SEE there with its run state and the exposure flags you may act on, your correspondents (routes back to whoever mailed you), and pending offers. Descendants appear under your home network.",
        params: &[],
        live: true,
    },
    Tool {
        name: "agents_result",
        template: "agents/{agent}/result",
        kind: Kind::Read,
        description: "One ended run's result: {outcome, text, structured_output?, schema_unmet?}. Refused `not_ended` while the run is open.",
        params: &[
            Param { name: "agent", required: true, schema: r#"{"type":"string","description":"the thread id — the SUBJECT"}"# },
            Param { name: "run", required: false, schema: r#"{"type":"integer","minimum":0,"description":"the run seq; defaults to the latest"}"# },
        ],
        live: true,
    },
    Tool {
        name: "agents_gather",
        template: "agents/gather",
        kind: Kind::Read,
        description: "Wait in place, boundedly: returns when the predicate over the named runs holds — the same rows as agents_list plus each ended run's result — or after poll_s (default 60) with {satisfied: false, pending: [...]}, in which case call it again. No single call outlives one interval, so a daemon restart costs a retried call, never a lost wait.",
        params: &[
            Param { name: "runs", required: true, schema: RUNS },
            Param { name: "until", required: true, schema: UNTIL },
            Param { name: "timeout_s", required: false, schema: r#"{"type":"integer","minimum":0,"description":"give up after this many seconds in total, across polls"}"# },
            Param { name: "poll_s", required: false, schema: r#"{"type":"integer","minimum":1,"default":60,"description":"the bound on one call, at most 300"}"# },
        ],
        live: true,
    },
    Tool {
        name: "agents_log",
        template: "agents/{agent}/log",
        kind: Kind::Read,
        description: "A page of a thread's log — rows of the `threads/log` shape, one per ledger event, every kind, assembled into nothing; bodies over the inline bound ride behind the resource door by summary. You may read your own log and the log of any thread you SEE in a network where it is `inspectable` (every descendant, by default) — never an ancestor's. Anything else is refused `no_route`.",
        params: &[
            Param { name: "agent", required: true, schema: r#"{"type":"string","description":"the thread id whose log to read — the SUBJECT"}"# },
            Param { name: "from_seq", required: false, schema: r#"{"type":"integer","minimum":0,"description":"the first seq to answer; defaults to 0"}"# },
            Param { name: "limit", required: false, schema: r#"{"type":"integer","minimum":1,"maximum":4000,"description":"rows per page; default 100, ceiling 4000 (the shape's WINDOW), refused above it"}"# },
        ],
        live: true,
    },
];

/// `agents_spawn_batch.agents`: an array of spawn inputs — the same properties
/// as `agents_spawn` and the same required `task`. Spelled once here;
/// `the_surface_is_exactly_section_2` asserts the item properties equal
/// `agents_spawn`'s, so the two cannot drift.
const SPAWN_ITEMS: &str = concat!(r#"{"type":"array","minItems":1,"items":{"type":"object","additionalProperties":false,"required":["task"],"properties":{"task":{"type":"string","description":"the dispatch prompt"},"agent":{"type":"string","description":"an opaque agent type, passed through to --agent, never resolved by threads"},"model":{"type":"string"},"effort":{"type":"string"},"max_thinking_tokens":{"type":"integer","minimum":0},"cwd":{"type":"string"},"append_system_prompt":{"type":"string"},"name":{"type":"string"},"json_schema":{"type":"object","description":"a JSON Schema (2020-12, at most 16 KiB serialized) for the child's answer to this task: passed as --json-schema on the turn that delivers the task, and on no later turn. Refused bad_schema, schema_too_large, before anything is spawned"},"schema_retries":{"type":"integer","minimum":0,"maximum":3,"description":"with json_schema only: how many corrective turns (each showing the validation errors) an answer that does not match the schema gets before it is reported schema_unmet; default 1"},"networks":{"description":"networks to admit the child to, each one you hold `members` in: {network id: flags} (absent flags false), or [network id] for view+visible+wakeable. Refused no_route, home, no_right, above_ceiling","oneOf":[{"type":"object","additionalProperties":{"type":"object"}},{"type":"array","items":{"type":"string"}}]},"network":{"type":"object","additionalProperties":false,"properties":{"name":{"type":"string"},"self":{"type":"object","description":"your flags in it; default view, visible, wakeable, members, configure"},"child":{"type":"object","description":"the child's flags; default view, visible, wakeable"},"rules":{"type":"object","description":"max_chars, history"}},"description":"mint a two-member network, you and the child, in the same act; its id is acknowledged as `network`"},"notify":"#, notify_schema!(), r#","wake":"#, wake_schema!(), r#","request_id":{"type":"string","description":"defaults to this call's tool-use id; spawn is idempotent on (owner, request_id)"}}}}"#);

impl Tool {
    /// **THE NAMING RULE, AS A FUNCTION**: the template with `/` → `_` and
    /// the `{subject}` segment dropped. `agents/{agent}/message` →
    /// `agents_message`.
    pub fn mcp_name_of(template: &str) -> String {
        template
            .split('/')
            .filter(|seg| !(seg.starts_with('{') && seg.ends_with('}')))
            .collect::<Vec<_>>()
            .join("_")
    }

    /// The subject parameter's name, from the template — `None` for a
    /// subjectless act.
    pub fn subject(&self) -> Option<&'static str> {
        MethodPath::parse(self.template).subject
    }

    /// The JSON Schema `tools/list` publishes for this tool.
    ///
    /// `additionalProperties: false`, which is the schema-level statement of
    /// [`check_params`]'s `unknown_param` refusal: a client that validates
    /// before sending learns it for free; one that does not is told on the
    /// reply.
    pub fn input_schema(&self) -> Value {
        let mut props = Map::new();
        let mut required = Vec::new();
        for p in self.params {
            let schema: Value = serde_json::from_str(p.schema)
                .unwrap_or_else(|e| panic!("{}.{}: schema does not parse: {e}", self.name, p.name));
            props.insert(p.name.to_string(), schema);
            if p.required {
                required.push(Value::String(p.name.to_string()));
            }
        }
        json!({
            "type": "object",
            "properties": Value::Object(props),
            "required": required,
            "additionalProperties": false,
        })
    }

    pub fn def(&self) -> ToolDef {
        ToolDef {
            name: self.name.to_string(),
            description: self.description.to_string(),
            input_schema: self.input_schema(),
        }
    }
}

/// Look a tool up by its MCP name.
pub fn tool(name: &str) -> Option<&'static Tool> {
    SURFACE.iter().find(|t| t.name == name)
}

/// The whole surface as `tools/list` answers it, in [`SURFACE`] order.
pub fn defs() -> Vec<ToolDef> {
    SURFACE.iter().map(Tool::def).collect()
}

/// **THE PARAMETER CHECK: unknown refused, required present, `caller` not
/// yours to say.**
///
/// Order: `caller` first (it is the identity hazard), then unknown names
/// (§1.4's *refused, never ignored*), then required names. A spawn carrying
/// `budget_key` is answered `unknown_param` even though spawn itself is
/// refused `not_available_yet` on this tree — the schema is enforced now so
/// the surface is honest about what it takes before the runtime exists.
pub fn check_params(tool: &Tool, args: &Map<String, Value>) -> std::result::Result<(), Refusal> {
    if args.contains_key(CALLER) {
        return Err(Refusal::malformed(format!(
            "`{CALLER}` is not an argument of `{}`: the caller is the run token's thread, \
             bound by the façade from the verified claims. Drop it — an identity you state \
             beside one the daemon verified is two spellings of one subject",
            tool.name
        )));
    }
    for k in args.keys() {
        if let Some((_, by, why)) = REMOVED
            .iter()
            .find(|(gone, by, _)| gone == k && !tool.params.iter().any(|p| p.name == *gone) && tool.params.iter().any(|p| p.name == *by))
        {
            return Err(Refusal::new(
                codes::UNKNOWN_PARAM,
                format!("`{}` does not take `{k}`; {why}. Refused rather than ignored (no `{by}` was inferred from it)", tool.name),
            ));
        }
        if !tool.params.iter().any(|p| p.name == k) {
            return Err(Refusal::new(
                codes::UNKNOWN_PARAM,
                format!(
                    "`{}` does not take `{k}`; it takes {}. Refused rather than ignored: a \
                     parameter you meant and the daemon dropped is the silent divergence \
                     this surface exists to delete",
                    tool.name,
                    tool.params
                        .iter()
                        .map(|p| if p.required { format!("`{}`*", p.name) } else { format!("`{}`", p.name) })
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
    }
    for p in tool.params.iter().filter(|p| p.required) {
        if !args.contains_key(p.name) {
            return Err(Refusal::malformed(format!(
                "missing required param: {} (`{}`)",
                p.name, tool.name
            )));
        }
    }
    Ok(())
}

/// **THE FAÇADE'S ONE WRITE: THE PATH.** Take the subject OUT of the
/// arguments and spell the concrete path.
///
/// After this the arguments carry no subject, which is the only spelling
/// `bind_subject` accepts — and `bind_subject` is still run on the result,
/// unchanged, so that if this function ever stops removing the parameter the
/// chassis's own refusal fires rather than a precedence rule deciding. That is
/// the forced red for §2.1's rule.
pub fn bind_path(tool: &Tool, args: &mut Map<String, Value>) -> std::result::Result<String, Refusal> {
    let Some(subject) = tool.subject() else {
        return Ok(tool.template.to_string());
    };
    let Some(value) = args.remove(subject) else {
        return Err(Refusal::malformed(format!(
            "missing required param: {subject} (`{}`'s subject)",
            tool.name
        )));
    };
    let Some(value) = value.as_str() else {
        return Err(Refusal::malformed(format!(
            "`{subject}` must be a string (a thread or network id)"
        )));
    };
    if value.is_empty() || value.contains('/') {
        return Err(Refusal::malformed(format!(
            "`{subject}` must be a non-empty id without `/`"
        )));
    }
    Ok(tool.template.replace(&format!("{{{subject}}}"), value))
}

/// The keys a command's reply may carry: rule 2.4's acknowledgement, plus the
/// two addresses `agents_spawn` mints.
const ACK_KEYS: &[&str] = &["promise_id", "visible", "changed"];
const SPAWN_ADDRESSES: &[&str] = &["thread_id", "run_id"];
/// The address `agents_network_create` mints (R4.S3) — the `thread_id`-of-spawn
/// precedent: an address the act minted, not a fact the shape carries.
const NETWORK_ADDRESS: &str = "network";
/// **`agents_message`'s one word of outcome** (R10.W1, operator-approved P1:
/// "agents_message never silently holds mail"): `delivered | held`. The ONE
/// non-address key a command may carry, and only this command, because the
/// sender is the only party who can act on "your mail did not wake them" and
/// a model does not read its own shape — the same word is on its
/// `mail_queued` fact, so the shape still carries the fact.
const MESSAGE_DELIVERY: &str = "delivery";
/// **`agents_spawn`'s deprecation `warning`** (R10.M3b, Q-23 option B) — the
/// [`MESSAGE_DELIVERY`] precedent: only the caller can act on "this word
/// goes away next release", and the same use is the `deprecated_param` fact
/// on its own ledger. Only [`WAKE_WARNING`] is ever sent.
/// DEPRECATED(wake, remove after DEPLOY-9n).
const SPAWN_WARNING: &str = "warning";

/// **THE GUARD: A COMMAND ACKNOWLEDGES, IT NEVER ANSWERS.**
///
/// A [`Kind::Command`] reply must be exactly `Accepted{promise_id, visible,
/// changed?}` — plus `thread_id` / `run_id` on `agents_spawn`, which are
/// addresses the act minted and not facts the shape carries. Anything else is
/// refused HERE, after the handler answered, so a command arm that returns a
/// result, a count or a snapshot cannot reach an agent through this door. A
/// [`Kind::Read`] reply passes through untouched.
///
/// **Forced red** by a command arm returning `{"rows": [...]}`: this refuses
/// with the key list; delete this function's check and the body reaches the
/// caller.
pub fn acknowledgement_only(tool: &Tool, reply: Value) -> std::result::Result<Value, Refusal> {
    if tool.kind == Kind::Read {
        return Ok(reply);
    }
    let Some(obj) = reply.as_object() else {
        return Err(guard_refusal(tool, "a non-object"));
    };
    let allowed = |k: &str| {
        ACK_KEYS.contains(&k)
            || (tool.name == "agents_spawn" && (SPAWN_ADDRESSES.contains(&k) || k == NETWORK_ADDRESS || k == SPAWN_WARNING))
            || (tool.name == "agents_network_create" && k == NETWORK_ADDRESS)
            || (tool.name == "agents_message" && k == MESSAGE_DELIVERY)
    };
    let stray: Vec<&str> = obj.keys().map(String::as_str).filter(|k| !allowed(k)).collect();
    if !stray.is_empty() {
        return Err(guard_refusal(tool, &format!("keys {stray:?}")));
    }
    if obj.get("promise_id").and_then(Value::as_u64).is_none() || obj.get("visible").and_then(Value::as_bool).is_none() {
        return Err(guard_refusal(tool, "no `promise_id`/`visible`"));
    }
    Ok(reply)
}

fn guard_refusal(tool: &Tool, what: &str) -> Refusal {
    Refusal::new(
        codes::ERROR,
        format!(
            "façade guard: `{}` is a COMMAND and its handler answered {what}; a command \
             acknowledges — Accepted{{promise_id, visible, changed?}} — and the fact arrives \
             on a shape (docs/shapes.md 2.4). The reply was withheld; this is a producer \
             defect, not something to retry",
            tool.name
        ),
    )
}

/// **THE WHOLE FAÇADE, FOR ONE CALL.** Tool → params checked → subject into
/// the path → the chassis's own `route` + `bind_subject` → caller bound →
/// the producer's one arm → the acknowledgement guard.
pub async fn call(
    manager: &ThreadManager,
    caller: &Claims,
    name: &str,
    arguments: Value,
) -> std::result::Result<Value, Refusal> {
    let Some(tool) = tool(name) else {
        return Err(Refusal::new(
            codes::UNKNOWN_METHOD,
            format!("{name}; the surface is tools/list"),
        ));
    };
    let mut args = match arguments {
        Value::Null => Map::new(),
        Value::Object(m) => m,
        other => {
            return Err(Refusal::malformed(format!(
                "`arguments` must be an object, got {other}"
            )))
        }
    };
    check_params(tool, &args)?;
    let path = bind_path(tool, &mut args)?;
    // The chassis's own grammar, on the path the façade wrote. `route` cannot
    // fail for a template the surface pins (`the_surface_is_exactly_section_2`
    // asserts every template is in `WS_METHODS`), and `bind_subject` is what
    // refuses a subject written twice — which, after `bind_path`, can only
    // happen if the façade stopped removing it.
    let routed = shapewire::server::route(WS_METHODS, &path).ok_or_else(|| {
        Refusal::new(
            codes::ERROR,
            format!("façade defect: `{path}` routes to no template in WS_METHODS"),
        )
    })?;
    let mut params = Value::Object(args);
    shapewire::server::bind_subject(&mut params, &routed)?;
    params
        .as_object_mut()
        .expect("bind_subject made it an object")
        .insert(CALLER.to_string(), Value::String(caller.thread_id.clone()));
    let reply = crate::server::handle(manager, routed.template, params)
        .await
        .map_err(shapewire::server::refuse)?;
    acknowledgement_only(tool, reply)
}

/// **THE TYPED REFUSAL FOR AN ACT WHOSE FACT IS NOT ON THIS TREE.** Names the
/// method path, so the caller — and the next reader — knows which act and
/// which release.
pub fn not_available_yet(template: &str) -> Error {
    Error::Refused {
        code: codes::NOT_AVAILABLE_YET,
        message: format!(
            "`{template}` is on the surface and refused: the fact or method that resolves \
             it does not exist on this daemon yet (the `agents` shape and its facts, \
             networks — R4.S3 — holds, suspensions). Nothing was done; do not retry until \
             the daemon says otherwise"
        ),
    }
}

/// `agents/{agent}/log`.
///
/// Reads `events` from `from_seq`, at most `limit` rows, through the same
/// [`crate::shapes::log::row_of`] the shape uses, so a page here and a window
/// on the shape are the same rows. Self-scoped until lineage exists: see the
/// module doc.
pub fn log(manager: &ThreadManager, params: &Value) -> Result<Value> {
    use crate::shapes::log::{row_of, WINDOW};
    let agent = req_str(params, "agent")?;
    let caller = req_str(params, CALLER)?;
    // READS (`docs/agent-networks.md` §6, N9): self, or a thread the caller
    // SEES where it is `inspectable` — never an ancestor. A refusal is
    // `no_route` whether or not the thread exists (N2), so it comes BEFORE the
    // existence check below.
    if !crate::networks::reads(manager, &caller, &agent)? {
        return Err(Error::Refused {
            code: crate::networks::refusal::NO_ROUTE,
            message: format!(
                "no_route: you (`{caller}`) may not read `{agent}`'s log. A thread reads its own \
                 log and the log of a thread it SEES where that thread is `inspectable` (every \
                 descendant, by default), never an ancestor's"
            ),
        });
    }
    let from_seq = params.get("from_seq").and_then(Value::as_u64).unwrap_or(0);
    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(LOG_DEFAULT_LIMIT);
    if limit == 0 || limit > WINDOW as u64 {
        return Err(Error::Refused {
            code: codes::MALFORMED,
            message: format!(
                "`limit` must be 1..={WINDOW} (the `threads/log` shape's WINDOW); got {limit}. \
                 Refused rather than clamped: a page shorter than asked would be a bound you \
                 could not see"
            ),
        });
    }
    if manager.store.get_thread(&agent)?.is_none() {
        return Err(Error::Refused {
            code: codes::UNKNOWN,
            message: format!("no such thread: {agent}"),
        });
    }
    let events = manager.store.events_from(&agent, from_seq, limit as usize)?;
    let rows: Vec<_> = events.iter().map(|e| row_of(&agent, e)).collect();
    let next_seq = rows.last().map(|r| r.seq + 1);
    Ok(json!({
        "agent": agent,
        "from_seq": from_seq,
        "rows": rows,
        "next_seq": next_seq,
        "window": WINDOW,
    }))
}

fn req_str(params: &Value, key: &str) -> Result<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| Error::Config(format!("missing required param: {key}")))
}
