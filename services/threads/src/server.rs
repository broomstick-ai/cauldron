//! The method table — what is left of the old axum WebSocket daemon.
//!
//! Ported from CCW `server.rs` with the locked contract upgrades: timestamped
//! history (G5), kind-filtered history (G6), message-id retrieval (G20),
//! settings + custom-event + ledger methods.
//!
//! **THERE IS NO LISTENER IN THIS FILE ANY MORE (R2.S4, 2026-10-10).** It used
//! to own the legacy door on `:3651` — an axum router with `/ws` (the RPC
//! methods, the `{"type":"subscribe"}` firehose with its once-per-connection
//! boot-note replay and its `Lagged` cursors) and `/health`. Every consumer
//! moved to the chassis door (`crate::chassis`, `:3653`), which serves the
//! same [`handle`] behind `shapewire::server::Producer`, the shapes the
//! firehose's frames became (`narration`, `boot`, `threads/log`), and the same
//! `/health` body. So the router, its `drive` loop and the firehose went, and
//! the daemon binds one port. What stays is the producer's half of the API:
//! [`WS_METHODS`], [`handle`] and [`bind`], the port-take that is still the
//! only mutual exclusion between two daemon processes.
//!
//! The queue READS are no longer here. `threads/{id}/queue` and
//! `threads/{id}/queue_failed` were removed once `shapes::threads`' `queue`
//! cell carried their contents: they projected the same four fields out of
//! `store::QueueRow` and differed in one `WHERE state=`, so a consumer filters
//! on a declared `state` word instead of choosing a method name. The queue
//! WRITES (`queue_cancel`, `queue_reselect`) stay, because an act with an
//! effect is not a projection.

use std::net::SocketAddr;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::event::SendIntent;
use crate::runtime::{DerivedState, ThreadManager};
use crate::settings::{SettingsPatch, ThreadSettings};

/// **THE CHASSIS NAME.** Also the consumer ledger's filename stem — one
/// producer, one file, so appends from two chassis never interleave. See
/// [`crate::consumers::producer_ledger_path`].
pub const DAEMON_NAME: &str = "threads";

/// THE PINNED WS METHOD INVENTORY — every request method [`handle`] answers.
///
/// This exists because the surface was recorded as "22 methods" and had
/// quietly become 24 (F-3): the `budgets.disable` / `budgets.enable` pair
/// arrived with DEC-33, and `budgets.delete` survived as an explicit refusal
/// rather than being removed. Nothing asserted the inventory, so the number
/// drifted silently.
///
/// `tests/ws_protocol.rs::the_ws_method_inventory_is_pinned` reads the match
/// arms out of THIS FILE and asserts they are exactly this list, so the two
/// cannot diverge: adding, renaming or deleting a method fails that test until
/// this list is updated deliberately.
///
/// `budgets.delete` is listed and is deliberately a refusal, not a gap — see
/// its arm.
///
/// `threads.reconcile_transcript` is the newest arrival (msg #1028): the client
/// hands back a transcript that advanced outside the daemon.
///
/// # 2026-09-28 — THE NAMES ARE PATHS NOW, AND THE SUBJECT IS A SEGMENT
///
/// Every name below was `noun.verb` and is now
/// `family[/{subject}]/leaf` — [`shapewire::server::MethodPath`] holds the
/// grammar and the operator's ruling behind it. The three things that changed
/// and the one that did not:
///
/// * **A required key moved out of the params and into the path.** That is the
///   whole point: `accounts.disable {account: "default"}` could open its promise
///   under the alias while the fold re-keyed to the host account, and the id was
///   stranded forever. `accounts/default/disable` has ONE place to write the
///   subject down. A request that also sends the param is REFUSED
///   ([`shapewire::server::bind_subject`]), not resolved by precedence.
/// * **An OPTIONAL key stayed a parameter**, because it is a filter over an
///   answer and not the subject of an act. `threads/status`, `gauge/history` and
///   `accounts/pool_share_get` all still take theirs in the params.
/// * **Two methods changed family.** `budgets.assign` and `budgets.for_thread`
///   are keyed on `thread_id`, not on a budget key, so under "a path names its
///   subject" they are leaves of `threads`:
///   `threads/{thread_id}/budget_assign` and `threads/{thread_id}/budget_get`.
/// * **The handlers are UNCHANGED.** What reaches [`handle`] is the TEMPLATE,
///   with the subject bound back into the params under the name the handler
///   already reads. The arms below are the template strings verbatim.
///
/// **One capability was deleted with the move**: `accounts.login.complete`
/// accepted `account` as a fallback for `session_id`, resolving it against the
/// in-flight logins. Two spellings of one subject is the disagreement this
/// change exists to delete, so the fallback is gone and `session_id` is the
/// path. See `docs/consumer-migration-ledger.md`.
pub const WS_METHODS: &[&str] = &[
    "accounts/login/begin",
    "accounts/login/{session_id}/cancel",
    "accounts/login/{session_id}/complete",
    "accounts/rotate",
    "accounts/{account}/delete",
    "accounts/{account}/disable",
    "accounts/{account}/enable",
    "accounts/{account}/login_restored",
    "accounts/{account}/recheck",
    "accounts/{account}/pool_share_set",
    "accounts/{account}/rename",
    // ── `agents` — R4.S1, `docs/managed-subagents.md` §2 ────────────────
    //
    // The family is `agents`, not `threads`, because the subject of these acts
    // is an agent RUN even though the rows underneath are a thread's (§2.1).
    // Each is one act with one path; the MCP door's tool of the same name
    // (`/` → `_`, subject dropped) is a FAÇADE over it — `crate::agents` —
    // and never a second entry point. Eleven are refused `not_available_yet`
    // on this tree because their resolving fact is R4.S2's; `agents/{agent}/log`
    // is live, self-scoped. The arms are below, under the same heading.
    //
    // R4.S3 (`docs/agent-networks.md`): the network acts and `agents/reach`
    // are LIVE — `crate::networks` — and `agents/{agent}/log` is gated by
    // READS rather than self-scoped.
    "agents/gather",
    "agents/list",
    "agents/network_create",
    "agents/reach",
    "agents/spawn",
    "agents/spawn_batch",
    "agents/watch",
    "agents/{agent}/interrupt",
    "agents/{agent}/log",
    "agents/{agent}/message",
    "agents/{agent}/policy",
    "agents/{agent}/result",
    "agents/{network}/broadcast",
    "agents/{network}/network_admit",
    "agents/{network}/network_configure",
    "agents/{network}/network_join",
    "agents/{network}/network_leave",
    "agents/{network}/network_remove",
    "budgets/{key}/delete",
    "budgets/{key}/disable",
    "budgets/{key}/enable",
    "budgets/{key}/get",
    "budgets/{key}/set",
    "daemon/image",
    "daemon/restart",
    "gauge/probe",
    "gauge/status",
    "shapes/list",
    "shapes/{shape}/subscribe",
    "shapes/{shape}/unsubscribe",
    "threads/create",
    "threads/list",
    // SH-A2: lease the thread to an interactive claude TUI (`docs/SHIMS.md` §A).
    "threads/{thread_id}/attach",
    "threads/{thread_id}/budget_assign",
    "threads/{thread_id}/budget_get",
    "threads/{thread_id}/cancel",
    // SH-A4: end the interactive lease; the row is deleted (`docs/SHIMS.md` §A).
    "threads/{thread_id}/detach",
    "threads/{thread_id}/event",
    "threads/{thread_id}/history",
    "threads/{thread_id}/queue_cancel",
    "threads/{thread_id}/queue_reselect",
    "threads/{thread_id}/reconcile_transcript",
    "threads/{thread_id}/seen",
    "threads/{thread_id}/send",
    // PL-2b: read-only — transcript + `turns/` capture presence per thread.
    "threads/{thread_id}/source_census",
    "threads/{thread_id}/update",
];

/// Take the port, and nothing else.
///
/// The bind is the ONLY mutual exclusion between two daemon processes — there
/// is no pidfile, no advisory lock on `threads.db`, no `daemons` table. So the
/// bind has to happen before a daemon does anything a second daemon would also
/// do, which is why [`crate::run_daemon`] calls this as its FIRST act — ahead of
/// `build_manager`, ahead of `update.on_boot()`, ahead of `recover()` — and
/// hands the listener to the chassis (`crate::chassis::chassis(..)
/// .serve_on_with_shutdown`) afterwards. Since R2.S4 the port it takes is the
/// chassis port, because that is the only port the daemon serves.
///
/// **A daemon that loses this race dies having touched nothing.** That sentence
/// was here before it was true: until 2026-08-20 the loser had already opened
/// the live `threads.db` read-write, run `apply_schema()`, possibly rewritten
/// `accounts.toml`, cleared the durable queued-restart record, overwritten
/// `threads.last-good` and appended to `restarts.jsonl` by the time it got here.
/// The order in `run_daemon` is what makes the claim true, and
/// `tests/daemon_race.rs::a_daemon_that_loses_the_port_race_writes_nothing` is
/// what keeps it true — it asserts the loser never even creates its data dir.
pub async fn bind(addr: SocketAddr) -> Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| Error::Config(format!("binding {addr}: {e}")))
}

/// **THE WHOLE DOOR, FOR A CALLER THAT HOLDS A PATH.** Route it against
/// [`WS_METHODS`], bind the subject the path carried, then dispatch.
///
/// Every in-crate test goes through THIS rather than calling [`handle`] with a
/// template it built by hand — otherwise a test would be addressing a handler
/// by a name no consumer can send, and the path grammar would be untested by
/// exactly the tests that claim to exercise the commands.
#[cfg(test)]
pub(crate) async fn route_request(
    manager: &ThreadManager,
    path: &str,
    params: Value,
) -> Result<Value> {
    let routed = shapewire::server::route(WS_METHODS, path)
        .ok_or_else(|| Error::Config(format!("unknown method: {path}")))?;
    let mut params = params;
    shapewire::server::bind_subject(&mut params, &routed)
        .map_err(|e| Error::Config(e.message))?;
    handle(manager, routed.template, params).await
}

/// `pub(crate)` since the API moved: [`crate::chassis::ThreadsProducer::handle`]
/// is this function behind `shapewire::server::Producer`. The chassis owns
/// dispatch; these 50 arms are what stayed with the producer.
///
/// **`method` is a TEMPLATE, never a concrete path** — the chassis routed it
/// and bound the subject back into `params` under the name these arms already
/// read. See [`shapewire::server::MethodPath`].
pub(crate) async fn handle(
    manager: &ThreadManager,
    method: &str,
    params: Value,
) -> Result<Value> {
    match method {
        // ── threads ──────────────────────────────────────────────────────
        "threads/create" => {
            let raw_settings = params.get("settings").cloned().unwrap_or(Value::Null);
            let mut settings: ThreadSettings = serde_json::from_value(raw_settings.clone())
                .map_err(|e| Error::Config(format!("bad settings: {e}")))?;
            // **THE CREATOR DEFAULT.** The context id is on for threads STARTED
            // FROM the AUI or the TUI and off elsewhere, decided here, once, from
            // the blob as the creator sent it — key PRESENCE is the only way to
            // tell "said nothing" from "said no", and it is gone the moment this
            // deserializes. See `ThreadSettings::apply_creator_defaults`.
            settings.apply_creator_defaults(&raw_settings);
            let thread_id = params
                .get("thread_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            // **AN ACKNOWLEDGEMENT, AND THE ADDRESS IT MINTED** (rule 2.4).
            //
            // `message_id` is GONE from this reply. It is the allocated
            // addressable position — the same fact `threads/{id}/event` used to
            // answer with, and the same one `ThreadCells::last_message_id`
            // carries. A caller that needs the id of a first message sends it
            // with `threads/{id}/send`, which still answers its own.
            //
            // **`thread_id` STAYS, and the distinction is the whole of 2.4.** It
            // is the ADDRESS this act minted, not a fact about state: the shape
            // carries cells UNDER that key and never carries the key as a value,
            // so there is no second source for the two to disagree about. Without
            // it a caller that let the daemon name the thread could not address
            // what it just made. The act that mints a key is subjectless under
            // its family — `accounts/login/begin` is the same shape of act — so
            // there is no path segment for it to ride instead.
            //
            // The promise is opened inside `create_thread_acked`, bound to the id
            // at the moment that id is minted, and resolved by the pool's
            // membership statement. See there.
            let (id, accepted) = manager.create_thread_acked(thread_id, settings)?;
            // **A THREAD CAN BE BORN WITH A NAME** (TUI-P1a, checklist TC-01).
            // The operator: *"I can't create a new thread and name it"*. A
            // thread's name is the newest `summary_updated` custom event (the
            // `threads/summary` shape, and the `threads` row's `app` cell), so
            // the name is written as exactly that, in the same call, before
            // any first message — the face that asked never shows the thread
            // unnamed. `manual: true` marks a name a person gave, which a face
            // ranks above an agent's drifting summary. Blank names are ignored,
            // not refused: an unnamed thread is legal and named later.
            if let Some(name) = params
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|n| !n.is_empty())
            {
                // The thread EXISTS at this point, so a failed name must not
                // fail the create: a caller that retried would make a second,
                // duplicate thread (TUI-P1a, inspect F-4). The thread is
                // created unnamed and the failure is logged; the reply carries
                // no extra field (rule 2.4), and the name is given again by a
                // rename.
                if let Err(e) = manager.append_custom(
                    &id,
                    crate::shapes::summary::SUMMARY_EVENT_TYPE.to_string(),
                    json!({
                        "summary": name,
                        "by": "create",
                        "manual": true,
                        "ts": chrono::Utc::now().to_rfc3339(),
                    }),
                    false,
                ) {
                    tracing::warn!(thread = %id, error = %e, "threads/create: the thread was created but its name was not written");
                }
            }
            // Optional inline first message. Its own position is not reported
            // here for the reason above.
            if let Some(text) = params.get("text").and_then(Value::as_str) {
                if !text.is_empty() {
                    manager.send(
                        &id,
                        text.to_string(),
                        SendIntent::Immediate,
                        params.get("metadata").cloned().unwrap_or(Value::Null),
                        false,
                    )?;
                }
            }
            let mut out = accepted.json();
            out["thread_id"] = json!(id);
            Ok(out)
        }
        "threads/{thread_id}/send" => {
            let id = req_str(&params, "thread_id")?;
            let text = req_str(&params, "text")?;
            let intent = match params.get("intent").and_then(Value::as_str) {
                None => SendIntent::Immediate,
                Some(s) => SendIntent::parse(s)
                    .ok_or_else(|| Error::Config(format!("unknown intent: {s}")))?,
            };
            let metadata = params.get("metadata").cloned().unwrap_or(Value::Null);
            let bare = params.get("bare").and_then(Value::as_bool).unwrap_or(false);
            // THE HEADER DECLARATION (INTENT 66b). A separate top-level param
            // from `metadata`, not a key inside it: `metadata` is the blob
            // threads stores and never reads, and `headers` is the structure
            // threads reads and renders. One channel each, so neither promise
            // is weakened by the other.
            let headers = params.get("headers").cloned().unwrap_or(Value::Null);
            // **`update_thread_settings` — THE TURN-START SETTINGS WRITE**
            // (R4.S7, IS-27 ask 1; `docs/thread-adapters.md` §7). Applied
            // through the one settings writer BEFORE the message is queued, so
            // the turn this send starts runs under the settings as they now
            // are, and the `SettingsChanged` row precedes the turn it explains.
            // A patch that does not parse refuses the WHOLE send: nothing is
            // queued under settings the caller did not get.
            let settings_changed = match params.get("update_thread_settings") {
                None | Some(Value::Null) => false,
                Some(p) => {
                    let patch: SettingsPatch = serde_json::from_value(p.clone())
                        .map_err(|e| Error::Config(format!("bad update_thread_settings: {e}")))?;
                    let (applied, _) = manager.update_settings(&id, patch)?;
                    applied.as_object().is_some_and(|o| !o.is_empty())
                }
            };
            let (message_id, header_warnings) =
                manager.send_declaring(&id, text, intent, metadata, bare, &headers)?;
            // The send SUCCEEDS even when a declaration was refused — the
            // operator's message is never the price of an application's bad
            // header. The warnings ride the success payload so the application
            // still finds out.
            Ok(json!({
                "accepted": true,
                "thread_id": id,
                "message_id": message_id,
                "header_warnings": header_warnings,
                "settings_changed": settings_changed,
            }))
        }
        "threads/{thread_id}/update" => {
            let id = req_str(&params, "thread_id")?;
            let patch: SettingsPatch = serde_json::from_value(
                params.get("patch").cloned().unwrap_or(Value::Null),
            )
            .map_err(|e| Error::Config(format!("bad patch: {e}")))?;
            // **AN ACKNOWLEDGEMENT, NOT THE SETTINGS** (rule 2.4). This answered
            // `{thread_id, applied}` — the applied settings, which is the blob
            // `Fact::ThreadSettings` carries and `ThreadCells::settings` holds,
            // byte-identically by construction (`thread_shape_writes.rs` pins
            // that identity). Two sources for one blob, and the reply was the one
            // that could be stale before it was read.
            //
            // `changed` is what `SettingsPatch::apply` actually moved: an empty
            // patch, or one that re-asserts what is already set, is a real
            // outcome and the reply says so rather than reporting a write.
            let promise = manager
                .promises
                .open::<crate::commands::ThreadsUpdate>(Some(id.clone()));
            let (applied, visible) = manager.update_settings(&id, patch)?;
            let changed = applied.as_object().is_some_and(|o| !o.is_empty());
            Ok(promise.accepted(visible).changed(changed).json())
        }
        "threads/list" => {
            let limit = opt_usize(&params, "limit").unwrap_or(100);
            let rows = manager.store.list_threads(limit)?;
            // ONE derivation for the whole page — five reads total, whatever
            // `limit` is. (A comment here used to say AUI asks for 1000 of
            // these on every reconnect; that was stale since the faces moved
            // onto shapes. The one remaining sender is `threads-stress`, which
            // is why this read survived R2.S3c — R4.S9, HOLDS R9.)
            let ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
            let derived = manager.thread_states(&ids)?;
            let out: Vec<Value> = rows
                .iter()
                .map(|r| thread_json(r, derived.get(&r.id)))
                .collect();
            Ok(json!({ "threads": out }))
        }
        "threads/{thread_id}/history" => {
            let id = req_str(&params, "thread_id")?;
            let after = opt_u64(&params, "after_seq").unwrap_or(0);
            let limit = opt_usize(&params, "limit").unwrap_or(200);
            let kinds = str_list(&params, "kinds");
            let events = manager
                .store
                .list_events(&id, after, limit, kinds.as_deref())?;
            let next_after = events.last().map(|e| e.seq).unwrap_or(after);
            let out: Vec<Value> = events.into_iter().map(event_json).collect();
            Ok(json!({ "thread_id": id, "events": out, "next_after": next_after }))
        }
        "threads/{thread_id}/cancel" => {
            let id = req_str(&params, "thread_id")?;
            manager.cancel(&id)?;
            Ok(json!({ "cancelled": true, "thread_id": id }))
        }
        "threads/{thread_id}/event" => {
            // §11: application-defined ledger events.
            let id = req_str(&params, "thread_id")?;
            let event_type = req_str(&params, "event_type")?;
            let data = params.get("data").cloned().unwrap_or(Value::Null);
            let addressable = params
                .get("addressable")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            // **AN ACKNOWLEDGEMENT, NOT THE POSITION** (rule 2.4). This answered
            // `{thread_id, message_id}`, and `message_id` is the allocated
            // addressable position — a fact the `threads` shape holds as
            // `ThreadCells::last_message_id`, folded out of the very ledger
            // event this call appends. The position arrives where every other
            // position arrives.
            //
            // `changed` is whether a position was ALLOCATED: a non-addressable
            // event moves `app` and nothing else, and saying `changed: true` for
            // it would claim a position that does not exist. It is never absent
            // here — the producer read `addressable` and knows the answer.
            let promise = manager
                .promises
                .open::<crate::commands::ThreadsEvent>(Some(id.clone()));
            let (message_id, visible) =
                manager.append_custom(&id, event_type, data, addressable)?;
            Ok(promise
                .accepted(visible)
                .changed(message_id.is_some())
                .json())
        }
        "threads/{thread_id}/seen" => {
            // Advance the unread cursor: "I have read this thread through seq
            // N". Monotonic in the store, so a stale client cannot un-read
            // what the operator has already heard. Returns the fresh status so
            // a client never needs a second call to learn the new unread count.
            let id = req_str(&params, "thread_id")?;
            let seq = opt_u64(&params, "seq")
                .unwrap_or_else(|| manager.store.max_seq(&id).unwrap_or(0));
            // Through the MANAGER, not the store: the cursor write and the fact
            // that announces it live in one place, because `Status` carries
            // `unread` and not `seen_seq`, so this position used to move in
            // silence. See `ThreadManager::mark_seen`.
            //
            // **AN ACKNOWLEDGEMENT, NOT THE STATUS** (rule 2.4). This answered
            // `manager.thread_status(&id)` — the whole fresh status — and the
            // comment justifying it said *"so a client never needs a second
            // call"*. That second call is the one the shape exists to remove:
            // `unread` and `seen_seq` are `ThreadCells`, folded from the
            // `Fact::ThreadSeen` this very call emits, and a reply carrying them
            // is a second source for a number whose whole difficulty is that a
            // cursor JUMP makes it hard to agree on.
            //
            // **`changed` is read BEFORE the write** and is about the CURSOR:
            // the store merges monotonically, so a stale client asking for a
            // position it has already passed is a re-assertion, and saying
            // `changed: true` for it would report a move that did not happen.
            let before = manager.store.last_seen_seq(&id).unwrap_or(0);
            let promise = manager
                .promises
                .open::<crate::commands::ThreadsSeen>(Some(id.clone()));
            let (seen, visible) = manager.mark_seen(&id, seq)?;
            manager.emit_status(&id);
            Ok(promise.accepted(visible).changed(seen > before).json())
        }
        "threads/{thread_id}/queue_cancel" => {
            let id = req_str(&params, "thread_id")?;
            let message_id = opt_u64(&params, "message_id")
                .ok_or_else(|| Error::Config("missing required param: message_id".into()))?;
            // **AN ACKNOWLEDGEMENT, NOT `cancelled`** (rule 2.4, R2.S5). This
            // answered `{thread_id, message_id, cancelled}`: the first two were
            // the caller's own params read back, and `cancelled` is whether the
            // call moved anything — which is `changed`, on every command. The
            // queue itself is `ThreadCells::queue`, re-read after the write.
            //
            // A refusal after the open closes the promise it had opened: a
            // promise never outlives the reply that refused it.
            let promise = manager
                .promises
                .open::<crate::commands::ThreadsQueueCancel>(Some(id.clone()));
            let (cancelled, visible) = match manager.cancel_queued(&id, message_id) {
                Ok(v) => v,
                Err(e) => {
                    manager.promises.abandon(promise.id());
                    return Err(e);
                }
            };
            Ok(promise.accepted(visible).changed(cancelled).json())
        }
        // ── THE RE-SELECT SURFACE (2026-08-25) ───────────────────────────
        //
        // What a client shows when it offers "that didn't get through — send it
        // again?". A turn whose retry ladder ran out leaves its messages in the
        // queue on `state='failed'`: preserved verbatim, and picked up by no
        // dispatcher until something explicitly asks for them back.
        //
        // Without a call like this the preservation would be real but
        // unreachable, which for the operator is indistinguishable from the
        // silent drop this whole wave exists to remove.
        //
        // **THE READING HALF IS GONE AND THIS IS THE WRITING HALF.** It was a
        // PAIR until the `queue` cell landed: `threads.queue_failed` answered
        // "what is parked on `failed`" and this answers "send it again". The
        // reading half — and `threads.queue`, its `state='waiting'` twin — were
        // REMOVED, because they projected the same four fields out of
        // `store::QueueRow` and differed in one `WHERE state=`, so the
        // discriminator their NAMES encoded is now a field with a closed
        // vocabulary (`shapes::threads::QueueEntry`, `QUEUE_STATE_WORDS`). A
        // face reads the cell it is already subscribed to and FILTERS; it does
        // not pick a method name per panel, and it cannot show the two panels
        // disagreeing about which rows exist.
        //
        // The WRITE stays a method, and that is rule 2.4 working rather than an
        // oversight: a cell publishes a fact, and "move these rows back to
        // `waiting`" is an act with an effect. Nothing about the removal of the
        // read makes the write a projection.
        //
        //   threads.queue_reselect { thread_id, message_ids?: [u64] }
        //     -> { promise_id, visible, changed }
        //
        // (It answered `{thread_id, reselected: <count>}` until R2.S5. The rows
        // are on `ThreadCells::queue`; whether any moved is `changed`.)
        //
        // `message_ids` omitted means every failed message on the thread, which
        // is the common case: a delivery is usually a batch and he wants all of
        // it back. Idempotent — a second call moves nothing.
        //
        // It ends by asking the ordinary dispatcher to look, so a re-selected
        // message goes out on the same path a fresh one would, through the same
        // admission gate. Nothing here re-runs a turn directly.
        "threads/{thread_id}/queue_reselect" => {
            let id = req_str(&params, "thread_id")?;
            let message_ids: Vec<u64> = params
                .get("message_ids")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_u64()).collect())
                .unwrap_or_default();
            // **`changed` is the WRITE's own count, not a read before it.**
            // `seen` reads before because its store write answers the merged
            // cursor, which cannot say whether it moved. This write answers the
            // number of rows its `UPDATE ... WHERE state='failed'` moved, inside
            // one transaction — the exact answer; a read before it would be a
            // second, racing reading of the same question.
            let promise = manager
                .promises
                .open::<crate::commands::ThreadsQueueReselect>(Some(id.clone()));
            let (reselected, visible) = match manager.reselect_failed(&id, &message_ids) {
                Ok(v) => v,
                Err(e) => {
                    manager.promises.abandon(promise.id());
                    return Err(e);
                }
            };
            Ok(promise.accepted(visible).changed(reselected > 0).json())
        }
        // The external-transcript reconcile (msg #1028). A client that ran turns
        // for this thread OUTSIDE the daemon — the AUI in-process fallback,
        // which runs on whichever login holds the conversation while the daemon
        // is down — hands the advanced transcript back on recovery, and the
        // daemon merges it into the thread's CURRENT account store (newest
        // wins) before the next daemon turn.
        //
        //   params: { "thread_id": <string>, "path": <absolute .jsonl path> }
        //   result: { "promise_id", "visible", "changed" }
        //
        // **AN ACKNOWLEDGEMENT, NOT THE OUTCOME** (rule 2.4, R2.S5). The port
        // outcome — `thread_id`, `account`, `source`, `outcome`, `reconciled`,
        // `destination`, the three event counts — is the payload of the
        // `transcript_reconciled` ledger event this call appends, which the
        // `threads` shape holds as `app["transcript_reconciled"]`, and which
        // `threads/{thread_id}/history` still answers. `changed` is whether the
        // transcript was actually ported (`reconciled` as was).
        //
        // Idempotent: handing the same path back twice answers "kept" the second
        // time. An unknown thread id, or a path that is not a readable file, is
        // an error — the daemon never guesses which conversation it was handed.
        "threads/{thread_id}/reconcile_transcript" => {
            let id = req_str(&params, "thread_id")?;
            let path = req_str(&params, "path")?;
            let promise = manager
                .promises
                .open::<crate::commands::ThreadsReconcileTranscript>(Some(id.clone()));
            let (data, visible) = match manager.reconcile_transcript(&id, &path) {
                Ok(v) => v,
                Err(e) => {
                    manager.promises.abandon(promise.id());
                    return Err(e);
                }
            };
            let ported = data["reconciled"].as_bool().unwrap_or(false);
            Ok(promise.accepted(visible).changed(ported).json())
        }

        // ── accounts (§10) ───────────────────────────────────────────────
        // **THE LOGIN-RESTORED HOOK.** The wire surface of
        // `ThreadManager::note_login_restored`.
        //
        // > *"if we're asking for the login codes from threads by using that
        // > [force] function I just mentioned, then we'll know when the login
        // > has succeeded and you can drop that flag and immediately add that
        // > account to the pool as available."*
        //
        // The login flow is issued THROUGH threads, so the thing that watched
        // the login succeed calls this and the account is placeable on the next
        // decision — no timer, no confirmation step, no self-attestation.
        //
        // BUILDING THAT FLOW IS A SEPARATE PIECE OF WORK AND IS NOT DONE HERE.
        // This is the hook it calls, and it is complete and callable today.
        //
        // It lifts the LOGIN bar only. A quota bar on the same account stays
        // exactly where it is — a successful `/login` is evidence about the
        // credential and says nothing at all about usage.
        //
        // Idempotent: reopening an account that was not barred is a quiet
        // no-op with an empty `reopened`, so the caller may fire it whenever it
        // sees a success without first checking whether it was needed.
        //
        // R12.5b: the report is a TRIGGER, not evidence. Any WS client can
        // call this, so it clears nothing on the caller's word: R12.10's
        // re-check (the server's identity read + the slot probe) runs first,
        // and only a verified login lifts anything. `recheck` is that verdict.
        "accounts/{account}/login_restored" => {
            let account = req_str(&params, "account")?;
            let (recheck, reopened) = manager.note_login_restored_checked(&account);
            Ok(json!({ "account": account, "reopened": reopened, "recheck": recheck }))
        }

        // **R12.10: THE RE-CHECK A VERIFIED LOGIN ASKS FOR.** `claude-accounts`
        // calls this (through `threads accounts recheck <label>`) as the last
        // act of a `login` / `set-live-login` that PASSED the identity guard.
        // It is NOT a "this login worked" report: the daemon trusts nothing the
        // caller says and re-runs the trusted read and the credential probe
        // itself, so a call for a crossed or dead slot clears nothing. A
        // disabled account is not read and stays disabled.
        "accounts/{account}/recheck" => {
            let account = req_str(&params, "account")?;
            refuse_the_machine_alias(&account, "accounts/{account}/recheck")?;
            let out = crate::accounts::recheck_flags(
                manager,
                &account,
                crate::accounts::RecheckTrigger::Login,
            );
            Ok(serde_json::to_value(out).map_err(|e| Error::Config(e.to_string()))?)
        }

        // **THE PER-ACCOUNT SHARE — the write half, and the only door into the
        // table besides sqlite3 by hand.**
        //
        // The bound [0.0, 1.0] is enforced by `Store::set_pool_share` and is
        // REFUSED rather than clamped, so this arm returns an error for 1.5
        // instead of quietly storing 1.0 and reporting success. The response
        // echoes the row back through the same builder `.get` uses, so the
        // caller sees the EFFECTIVE share — which is the number placement will
        // actually use — rather than the number it just sent.
        //
        // **`share: 0` IS REFUSED** (2026-09-20). It used to disable the account.
        // The operator separated the two facts — *"I want disabled to be
        // independent of share. Whether to include it in the pool supersedes how
        // much should be made available to the pool."* — so this arm sets a
        // QUANTITY and nothing else, and `accounts.disable` is the door to the
        // other question. The refusal from `Store::set_pool_share` names that
        // door, which is the cheapest place to catch a caller still spelling the
        // act the old way.
        //
        // The response still carries `disabled` beside `share`, unchanged: a
        // client reading this payload always learns both facts, it just no
        // longer sets one of them here.
        //
        // It takes effect on the next placement with no restart and no reload:
        // `Budgets::pool_share` reads the row per decision.
        // `tests/accounts_pool_share_api.rs` forces that end to end.
        "accounts/{account}/pool_share_set" => {
            let account = req_str(&params, "account")?;
            refuse_the_machine_alias(&account, "accounts/{account}/pool_share_set")?;
            // **`share: null` CLEARS; an ABSENT `share` IS STILL MALFORMED**
            // (R2.S3g, 2026-10-10). The two are different statements and are
            // kept apart on purpose: absent is a call that forgot its argument,
            // null is the operator saying "no override for this account" — the
            // only way to remove a key minted by an override alone (`aa-owned`:
            // no registration, no slot, no threads), which until this arm had a
            // door in and none out. A non-number that is not null is refused
            // with the same message as absent, as before.
            let share = match params.get("share") {
                None => {
                    return Err(Error::Config("missing required param: share (a number)".into()))
                }
                Some(Value::Null) => None,
                Some(v) => Some(v.as_f64().ok_or_else(|| {
                    Error::Config("missing required param: share (a number)".into())
                })?),
            };
            let note = params.get("note").and_then(Value::as_str);
            // **READ BEFORE WRITE, so the reply can say `changed`** — rule 2.7's
            // *did this call move anything*. One indexed read of the row this write
            // is about to replace; the alternative is deriving it from two stamps
            // on the frame, which is exactly the consumer-side arithmetic the rule
            // exists to stop.
            // The OVERRIDE row is what this write replaces, so it is the thing
            // `changed` is about — not the effective share, which also folds in what
            // the pool blob declares and would report "changed" for a write that
            // merely pinned a declared value.
            let changed = match share {
                Some(share) => {
                    let before = manager.store.pool_share(&account).unwrap_or(None);
                    manager.store.set_pool_share(&account, share, note)?;
                    before.is_none_or(|b| (b - share).abs() > f64::EPSILON)
                }
                // The DELETE answers `changed` itself: a row existed or it did
                // not. Same promise, same fact — "this account's share moved" —
                // and the fact's row now reads `source: unset|declared`, which is
                // what lets the `accounts` shape drop a key nothing else holds.
                None => manager.store.clear_pool_share(&account)?,
            };
            // An input to placement just moved — and we know exactly whose, so
            // the frame says so and the board can narrate the account by name.
            manager.account_picture_changed_for("pool_share", Some(&account));
            // **AN ACKNOWLEDGEMENT** (docs/shapes.md 2.4). The fourth violation,
            // found by sweeping every mutation-shaped handler rather than by
            // trusting the doc's list of two.
            //
            // **AND IT IS ABOUT THIS ACCOUNT.** The subject is what makes
            // `emit_share_fact`'s own fact the answer to THIS command rather than
            // to a command about some other account — and what puts the id on the
            // frame this caller's key actually receives.
            let promise = manager
                .promises
                .open::<crate::commands::PoolShareSet>(Some(account.clone()));
            // **AND THE REPLY CARRIES THE STAGE THE EMIT REACHED** (rule 2.7): the
            // emit itself answers whether a frame will ever carry this id, so the
            // handler never reads `any_tracking()` a second time.
            let visible = manager.emit_share_fact(&account); // P3
            Ok(promise.accepted(visible).changed(changed).json())
        }

        // ── DISABLE AN ACCOUNT, AND PUT IT BACK ──────────────────────────────
        //
        // The operator, msg #531, about an account that is no longer a paid
        // subscription: *"create the machinery on both sides of the contract to
        // disable an account so that it doesn't get considered a placement, and
        // then the text user interface just says, like, disabled. I don't want
        // it to be deleted, though. I still want to see it there as one of my
        // accounts, because if I turn it back on, I'm going to keep using the
        // same login."*
        //
        // **THESE ARE NOW THE ONLY DOOR, and that is the 2026-09-20 change.**
        //
        // They shipped 2026-09-11 as a VERB over a state that was really
        // `pool_shares.share == 0`, so `accounts.pool_share.set` with a literal
        // zero was an equally valid spelling of the same act. That equivalence
        // was the defect, not a convenience: the capability was live for four
        // days behind a magic value in a number field and the operator never
        // found it — his own `pool_shares` row for `alice-example-com` still
        // read `1.0e-05` on 2026-09-11, the 0.001% he tried BEFORE the feature
        // existed. He ruled:
        //
        //   *"I want disabled to be independent of share. Whether to include it
        //    in the pool supersedes how much should be made available to the
        //    pool."*
        //
        // So inclusion has its own state (`account_disabled`), its own writer
        // (`Store::set_account_disabled`), its own fact (`Fact::AccountDisabled`)
        // and its own cell on the shape. These two arms are the only way to move
        // it; `accounts.pool_share.set` now REFUSES a zero and says so.
        //
        // **AND THE SHARE IS NO LONGER DESTROYED.** Neither arm touches
        // `pool_shares`. An account disabled on a 0.6 share is still lent 0.6 on
        // the way out, on the wire while it sits there, and on the way back —
        // which is what made `restore_to`, `prior` and `Budgets::restore_share`
        // unnecessary and removed all three.
        //
        // Named after `budgets.disable`/`budgets.enable`, which are the same
        // shape over the same kind of state and set the precedent.
        //
        // NOT DESTRUCTIVE, in either direction. Neither arm touches the account
        // registry, the login, the credential store, the gauge history or the
        // thread history. A disabled account is still listed by `accounts.list`,
        // still carries its readings on `accounts.pressure`, and still shows up
        // on `accounts.health`. Disappearing from those is the failure mode, not
        // the feature.
        "accounts/{account}/disable" => {
            let account = req_str(&params, "account")?;
            refuse_the_machine_alias(&account, "accounts/{account}/disable")?;
            // The note is the operator's own words about WHY, kept beside the
            // row so a disable set weeks ago is still explicable. Optional, and
            // absent means absent — never a fabricated string.
            let note = params.get("note").and_then(Value::as_str);
            // Rule 2.7's `changed`: presence of the row IS the state, so one read
            // answers *was this already true*.
            let was_disabled = manager.store.account_disabled_at(&account)?.is_some();
            // ONE ROW, IN ONE TABLE, AND NO NUMBER ANYWHERE. There is nothing
            // here for the arithmetic to reach and nothing for the share column
            // to forget.
            manager.store.set_account_disabled(&account, true, note)?;
            manager.account_picture_changed_for("account_disabled", Some(&account));
            // **AN ACKNOWLEDGEMENT, NOT A FACT** (docs/shapes.md 2.4). This
            // used to answer `pool_share_rows` — the new row, on the reply,
            // which is a second source for a value the shape already carries.
            // The row arrives where every other row arrives; this says only
            // "heard, and here is what to watch for".
            let promise = manager
                .promises
                .open::<crate::commands::AccountsDisable>(Some(account.clone()));
            // **`visible` COMES BACK FROM THE EMIT** (rule 2.7). This promise is
            // about the DISABLED fact, so it is `emit_disabled_fact`'s answer and
            // not the share emit's — two facts, two emits, one of which resolves
            // this id.
            let visible = manager.emit_disabled_fact(&account); // P16
            // The share did not move, but the PRESSURE did: a disabled account
            // is unranked, and the row carrying that has to be re-produced or
            // every mirror keeps showing the pressure it had a second ago.
            manager.emit_share_fact(&account); // P3
            Ok(promise
                .accepted(visible)
                .changed(!was_disabled)
                .json())
        }

        // ENABLE. It deletes the row and that is all: the account comes back at
        // the share it never stopped having. There is no number to restore and
        // therefore no number to get wrong, which is the whole benefit of
        // independence and is why `Budgets::restore_share` and its four rungs
        // are gone.
        //
        // **`share` IS STILL ACCEPTED, and now it means exactly what it says**:
        // put this account back AND set its share to this, for the case where he
        // wants it back at a different weight. Two acts in one call, both
        // explicit. It goes through the same bound as every other share, so a
        // zero is refused here rather than performing a disable under the name
        // `enable` — and the refusal is now `Store::set_pool_share`'s own,
        // pointing at `accounts.disable`, so there is one sentence about what a
        // zero share is not.
        "accounts/{account}/enable" => {
            let account = req_str(&params, "account")?;
            refuse_the_machine_alias(&account, "accounts/{account}/enable")?;
            let note = params.get("note").and_then(Value::as_str);
            let was_disabled = manager.store.account_disabled_at(&account)?.is_some();
            // ORDERED: the share first, so a rejected share leaves the account
            // disabled rather than half-enabled at an unknown weight. `?` here
            // returns before anything has moved.
            if let Some(s) = params.get("share").and_then(Value::as_f64) {
                manager.store.set_pool_share(&account, s, note)?;
            }
            manager.store.set_account_disabled(&account, false, None)?;
            manager.account_picture_changed_for("account_enabled", Some(&account));
            // Same rule as its sibling, and the same fix. The doc named two
            // violations; this was the third, in the same file.
            let promise = manager
                .promises
                .open::<crate::commands::AccountsEnable>(Some(account.clone()));
            let visible = manager.emit_disabled_fact(&account); // P16
            manager.emit_share_fact(&account); // P3
            // R12.10 (operator ruling r41-2): a stale wiped / expired / dead /
            // auth flag never survives his re-enable. The LAST act of the
            // enable re-checks every flag now — a pass clears it, a fail keeps
            // it with a fresh reason — and NEVER re-disables: a failed re-check
            // leaves the account enabled but unavailable, with its reason. The
            // outcome is announced (`flags_cleared` / `flags_kept`) and lands on
            // the pressure rows; the reply stays an acknowledgement.
            crate::accounts::recheck_flags(
                manager,
                &account,
                crate::accounts::RecheckTrigger::Enable,
            );
            // `changed` is about THIS command's own question — did the account come
            // back INTO the pool. A call that also moved the share on an already-
            // enabled account is `changed: false` for inclusion, which is true, and
            // the share's own move is the share fact's to report.
            Ok(promise.accepted(visible).changed(was_disabled).json())
        }

        // ── budgets (§15; Q-25/DEC-17: config is POLLED here, state
        //    transitions are PUSHED as thread events on the existing stream) ─
        "budgets/{key}/get" => {
            // Config + live usage: BOTH meters, the per-account breakdown,
            // and the estimation state (DEC-7 — AUI reads, it never meters).
            let key = req_str(&params, "key")?;
            let accounts = manager.account_pool();
            let snap = manager
                .budgets
                .snapshot(&key, &accounts)?
                .ok_or_else(|| Error::Config(format!("no such budget key: {key}")))?;
            let mut payload = snap.to_json();
            payload["estimation"] = manager.budgets.estimation_state()?;
            payload["threads"] = json!(threads_using_key(manager, &key)?);
            Ok(payload)
        }
        "budgets/{key}/set" => {
            // Create or update a key's config — the ONE way a budget changes
            // (DEC-11: every key carries its own config; a "+10% increment"
            // is just this call with a bigger number).
            //
            // Round-7: limits exist on ALL FOUR axes (three percentages plus
            // dollars), and `accounts` is REQUIRED on creation — this crate
            // never defaults a budget to every account. A client that wants
            // all of them sends all of them.
            let key = req_str(&params, "key")?;
            // READ BEFORE WRITE, so the reply can answer rule 2.7's `changed`.
            let before = manager.budgets.get_config(&key)?;
            let mut cfg = before
                .clone()
                .unwrap_or_else(|| crate::budgets::BudgetConfig::new(&key));
            for (name, field) in [
                ("session_pct", 0usize),
                ("weekly_pct", 1),
                ("fable_pct", 2),
                ("cash_usd", 3),
            ] {
                if params.get(name).is_some() {
                    let v = params.get(name).and_then(Value::as_f64);
                    match field {
                        0 => cfg.session_pct = v,
                        1 => cfg.weekly_pct = v,
                        2 => cfg.fable_pct = v,
                        _ => cfg.cash_usd = v,
                    }
                }
            }
            if let Some(list) = params.get("accounts") {
                cfg.accounts = serde_json::from_value(list.clone())
                    .map_err(|e| Error::Config(format!("bad accounts list: {e}")))?;
            }
            if let Some(t) = params.get("on_trip").and_then(Value::as_str) {
                cfg.on_trip = crate::budgets::TripAction::parse(t)
                    .ok_or_else(|| Error::Config(format!("unknown on_trip: {t} (kill|finish)")))?;
            }
            // Q-34: `disabled` is settable here too, so a client can mint or
            // edit a key and take it out of service in one call. Omitting it
            // NEVER changes the current state — editing a limit on a disabled
            // key must not quietly re-enable it. `budgets.disable` /
            // `budgets.enable` are the direct surface, and the only one that
            // works on a key whose config no longer validates.
            if let Some(d) = params.get("disabled") {
                cfg.disabled = d.as_bool().ok_or_else(|| {
                    Error::Config(format!("disabled must be a boolean, got {d}"))
                })?;
            }
            if params.get("parent_key").is_some() {
                // RESERVED (DEC-14): stored, never read by v1.
                cfg.parent_key = params
                    .get("parent_key")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            // Rule 2.7's `changed`, read BEFORE the write: the config this call
            // replaces, compared with the one it writes. A `budgets/{key}/set`
            // that re-sends what is already stored is a re-assertion, and the
            // reply says so rather than reporting a move.
            let unchanged = before.as_ref().is_some_and(|b| *b == cfg);
            manager.budgets.set_config(&cfg)?;
            // ── A BUDGET EDIT IS A PLACEMENT EVENT ───────────────────────
            //
            // It used to call `resume_sweep()` straight, which reached the PARKED
            // threads and left every RETRYING one waiting for the tick — and a
            // thread on the usage-limit ladder is exactly the thread a raised
            // limit frees. Ringing the bell instead wakes both sweeps through one
            // coalescing window (`ThreadManager::placement_pass`), so one `set`
            // that changes four axes costs one pass.
            //
            // `placement_wake`, not the full bell: as the acknowledgement below
            // explains at length, there is no `budgets` shape to announce on and
            // this reply is `Visible::Never`. Announcing on the `accounts` domain
            // for a budget edit would tell clients to re-read something that did
            // not move.
            manager.placement_wake("budget_set");
            // ── AN ACKNOWLEDGEMENT, AND IT WILL NEVER BE `visible` ───────────
            //
            // This answered the budget SNAPSHOT — the config, the meters, the
            // per-account breakdown — which is state, and rule 2.4 says a
            // command never answers with the fact it caused.
            //
            // **But there is no fact for it to arrive as, and that is a finding
            // rather than a gap in this conversion.** No shape on this producer
            // carries a budget: every `Fact` variant is about an account, a
            // login, the pool or a thread, and `shapes::accounts` has cells for
            // the share, the gauge, the limit, health, the credential and
            // pressure — none for a budget key. So this is `gauge.probe`'s
            // position exactly (`docs/shapes.md` 2.4, closing paragraph): it
            // reaches `accepted`, never `visible`, SAYS SO on the reply, and
            // closes its own promise on the way out rather than leaving an id
            // open forever for `open_ids` to find.
            //
            // The promise is opened at all — rather than skipped — because
            // `visible: false` is a real answer a writer can act on, and a reply
            // with no id at all would be indistinguishable from a method that
            // does not participate in the contract. What would make it
            // `visible: true` is a `budgets` shape; see the ledger.
            let promise = manager
                .promises
                .open::<crate::commands::BudgetsSet>(Some(key.clone()));
            manager.promises.abandon(promise.id());
            Ok(promise
                .accepted(shapewire::command::Visible::Never)
                .changed(!unchanged)
                .json())
        }
        "budgets/{key}/disable" | "budgets/{key}/enable" => {
            // Q-34 (msg #751): "I don't think we should delete a budget key. I
            // think we should disable a budget key so we can still query and
            // see information about the budget… So no deleting." A disabled key
            // DENIES every turn that names it (the turn parks — CON-4) while
            // everything about it stays queryable through `budgets/{key}/get`,
            // which keeps reporting spend, meters, the per-account breakdown
            // and calibration state exactly as before. (`budgets/list` had no
            // caller and was RETIRED at R2.S3a.)
            let key = req_str(&params, "key")?;
            let disable = method == "budgets/{key}/disable";
            manager.budgets.set_disabled(&key, disable)?;
            // Enabling can unpark waiting work immediately; disabling re-runs
            // the same gate so a thread that was parked for another reason is
            // re-parked under the reason that now actually holds it.
            //
            // Through the bell for the same reason `budgets/{key}/set` is: a
            // disabled key DENIES every turn that names it, and the turns it
            // denies include ones sitting on a retry floor, which a bare
            // `resume_sweep()` never reached.
            //
            // Two literal rings rather than one computed tag, so the guard that
            // reads the event set out of `src/` can enumerate it: a reason handed
            // through a binding is invisible to that scan.
            if disable {
                manager.placement_wake("budget_disabled");
            } else {
                manager.placement_wake("budget_enabled");
            }
            let accounts = manager.account_pool();
            let mut payload = manager
                .budgets
                .snapshot(&key, &accounts)?
                .map(|s| s.to_json())
                .unwrap_or(Value::Null);
            payload["threads"] = json!(threads_using_key(manager, &key)?);
            Ok(payload)
        }
        "budgets/{key}/delete" => {
            // REFUSED, always (Q-34). Deleting a key silently unmetered every
            // thread still pointed at it, which is the exact failure this
            // method's removal closes. It is kept as an explicit refusal rather
            // than falling through to `unknown method` so a client that has not
            // caught up is told what to call instead.
            let key = req_str(&params, "key")?;
            Err(Error::Config(format!(
                "budget keys are never deleted: call `budgets/{key}/disable` \
                 instead — a disabled key denies every turn that uses it while its spend \
                 and history stay queryable"
            )))
        }
        "threads/{thread_id}/budget_assign" => {
            // Point a thread at a key (or unkey it with `key: null`). Several
            // threads may share one key (DEC-4); re-pointing is legal at any
            // time (DEC-14).
            let thread_id = req_str(&params, "thread_id")?;
            let key = params.get("key").and_then(Value::as_str).map(str::to_string);
            // A key that does not exist would park the thread on its next turn
            // (cause `unknown_key` — nothing runs unmetered). Say so HERE, when
            // the caller can still fix the typo, rather than at the next send.
            if let Some(k) = &key {
                if manager.budgets.get_config(k)?.is_none() {
                    return Err(Error::Config(format!(
                        "no such budget key: {k} — mint it with `budgets/<key>/set` first (a thread \
                         pointed at a key with no config parks rather than running unmetered)"
                    )));
                }
            }
            let patch: SettingsPatch = serde_json::from_value(json!({ "budget_key": key }))
                .map_err(|e| Error::Config(format!("bad budget key: {e}")))?;
            // **AN ACKNOWLEDGEMENT, NOT `budget_key`** (rule 2.4, R2.S5). This
            // answered `{thread_id, budget_key}`, and `budget_key` is a field of
            // the settings blob `Fact::ThreadSettings` carries and
            // `ThreadCells::settings` holds — `update_settings` emits it, so this
            // is NOT `budgets/{key}/set`'s `NeverVisible` case (see
            // `crate::commands::ThreadsBudgetAssign`).
            //
            // `changed` is read BEFORE the write and is about the KEY: re-pointing
            // a thread at the key it already has is a re-assertion. The read is
            // also before the open, so its refusal leaks no promise.
            let before = manager.thread_budget_key(&thread_id)?;
            let promise = manager
                .promises
                .open::<crate::commands::ThreadsBudgetAssign>(Some(thread_id.clone()));
            let changed = before != key;
            let visible = match manager.update_settings(&thread_id, patch) {
                Ok((_, visible)) => visible,
                Err(e) => {
                    manager.promises.abandon(promise.id());
                    return Err(e);
                }
            };
            // N-2: the parent's live descendants on its OLD key follow it, so a
            // child parked on a spent key is re-admitted under the new one —
            // see `crate::managed::repoint_descendants`.
            //
            // R9.N2b: a descendant whose re-point FAILED stays on the old key,
            // and the answer says so — `left_on_old_key` names each one (and
            // why), so `changed: true` (the PARENT's key moved) is never read
            // as "the whole subtree moved". A clean re-point answers as before,
            // with no such field.
            let left = if changed {
                crate::managed::repoint_descendants(manager, &thread_id, before.as_deref(), key.as_deref()).left
            } else {
                Vec::new()
            };
            let mut answer = promise.accepted(visible).changed(changed).json();
            if !left.is_empty() {
                answer["left_on_old_key"] = json!(left
                    .iter()
                    .map(|(id, why)| json!({ "thread_id": id, "budget_key": before, "error": why }))
                    .collect::<Vec<_>>());
            }
            Ok(answer)
        }
        "threads/{thread_id}/budget_get" => {
            // The per-thread resolution: which key, which thresholds, and the
            // live state of that key.
            let thread_id = req_str(&params, "thread_id")?;
            let row = manager
                .store
                .get_thread(&thread_id)?
                .ok_or_else(|| Error::Config(format!("no such thread: {thread_id}")))?;
            let settings = ThreadSettings::from_stored(&row.settings_json)?;
            let accounts = manager.account_pool();
            let budget = match &settings.budget_key {
                Some(k) => manager
                    .budgets
                    .snapshot(k, &accounts)?
                    .map(|s| s.to_json())
                    .unwrap_or(Value::Null),
                None => Value::Null,
            };
            let parked = manager.park_state(&thread_id)?;
            Ok(json!({
                "thread_id": thread_id,
                "budget_key": settings.budget_key,
                "thresholds": settings.thresholds,
                // Round-7 item 1: the bypass is part of a thread's budget
                // resolution, so it is readable right here.
                "budgeting_off": settings.budgeting_off,
                // Round-7 item 7: where this thread's conversation lives.
                "history_accounts": manager.store.thread_accounts(&thread_id)?,
                "budget": budget,
                "parked": parked,
            }))
        }
        "threads/{thread_id}/source_census" => {
            // **PL-2b — A PLAIN READ, like `budget_get` above.** No promise is
            // opened and nothing is written: the transcript `{path, exists}`
            // under the thread's OWN account store, the raw `turns/<id>/`
            // capture count, account and cwd. PL-2's census finding, verbatim:
            // "`locate_transcript` searches only the thread's current account
            // store, so **a PL-5 backfill would report it as having no
            // source.** PL-5 should search every store." This verb does not fix
            // that: `exists: false` means "not in the current account's store".
            let thread_id = req_str(&params, "thread_id")?;
            manager.source_census(&thread_id)
        }

        // **SH-A2 — ATTACH** (`docs/SHIMS.md` §A.1): take the thread's lock,
        // render it, mint its door, record the attached row, and answer the
        // launch recipe. `crate::runtime::attach` holds the order and the
        // guarantees; this arm only translates. A refusal is typed: `busy`
        // when a turn or another attach holds the thread, `unsupported` for a
        // thread that is not `projection: projected`, `forbidden` for an
        // `account` that is not the thread's own, and `not_found` for a thread
        // that does not exist — never `busy` (SH-A2c; see `refused_not_found`).
        "threads/{thread_id}/attach" => {
            let thread_id = req_str(&params, "thread_id")?;
            let req = crate::runtime::attach::AttachRequest {
                wrapper_pid: params.get("wrapper_pid").and_then(Value::as_i64).unwrap_or(0),
                account: params.get("account").and_then(Value::as_str).map(str::to_string),
            };
            match manager.attach_thread(&thread_id, &req) {
                Ok(recipe) => Ok(recipe.to_json()),
                Err(e) => {
                    use crate::runtime::attach::AttachRefused as R;
                    let code = match &e {
                        R::NotFound { .. } => return Err(refused_not_found(&e, e.code())),
                        R::Locked(_) => shapewire::envelope::codes::BUSY,
                        R::NotProjected => shapewire::envelope::codes::UNSUPPORTED,
                        R::AccountMismatch { .. } => shapewire::envelope::codes::FORBIDDEN,
                        _ => shapewire::envelope::codes::ERROR,
                    };
                    Err(Error::Refused { code, message: format!("{} [{}]", e, e.code()) })
                }
            }
        }

        // **SH-A4 — DETACH** (`docs/SHIMS.md` §A.1 step 4): delete the
        // attached row, close the projected turn, release the lock, re-dispatch
        // the queue. `crate::runtime::attach` holds the order. An unknown
        // cookie (or another thread's), and the loser of a detach racing the
        // reaper, is refused `not_found` (SH-A2c; see `refused_not_found`).
        "threads/{thread_id}/detach" => {
            let thread_id = req_str(&params, "thread_id")?;
            let cookie = req_str(&params, "cookie")?;
            let exit = params.get("exit").and_then(Value::as_i64).map(|c| c as i32);
            match manager.detach_thread(&thread_id, &cookie, exit) {
                Ok(d) => Ok(d.to_json()),
                Err(e) => {
                    use crate::runtime::attach::DetachRefused as R;
                    let code = match &e {
                        R::NotFound { .. } => return Err(refused_not_found(&e, e.code())),
                        R::Io(_) => shapewire::envelope::codes::ERROR,
                    };
                    Err(Error::Refused { code, message: format!("{} [{}]", e, e.code()) })
                }
            }
        }

        // ── the gauge itself (DEC-15) ────────────────────────────────────
        "gauge/status" => {
            let accounts = manager.account_pool();
            Ok(json!({
                "accounts": manager.budgets.gauge_state(&accounts)?,
                "estimation": manager.budgets.estimation_state()?,
                // The per-period HISTORY beside the live estimate: one entry
                // per (login, axis, reset window). Additive — `estimation`
                // above is unchanged, and this is the only place the history
                // is readable at all.
                "estimation_periods": manager.budgets.estimate_period_history()?,
            }))
        }
        "gauge/probe" => {
            // Force a `/usage` poll now. Zero-cost per CCW's recon (verified
            // live 2026-07-29: `num_turns: 0`, every token counter 0, three
            // back-to-back probes left the gauge identical), but it IS a real
            // claude invocation, so it is explicit rather than free.
            //
            // `force` is what makes "poll now" mean it. The background sweep's
            // own preconditions — no budget configured, probing disabled by
            // interval — used to silently apply here too, so an explicit probe
            // could answer `observations: 0` having done nothing and said
            // nothing. Now it either polls or reports why it could not.
            // ── NO SUBJECT, AND NOTHING THIS PRODUCER EMITS CAN CARRY IT ─────
            //
            // **THIS ARM HELD TWO BUGS AND THEY WERE THE SAME BUG.** Every other
            // command names an account; this one says "poll the pool now" and names
            // none, so its promise is about the SHAPE. The book used to let any
            // KEYED fact of the right type close a subjectless promise, so:
            //
            // * the promise closed on the FIRST `GaugeObserved` of the sweep —
            //   collapsing *"my sweep completed"* with *"one observation of one
            //   account arrived"* — and not necessarily an observation of this
            //   sweep at all, because the background poller emits down the same
            //   path and races it; and
            // * on the `Err` arm the promise opened above was never resolved, so
            //   the book held it forever and `open_ids()` — the liveness test's own
            //   oracle — reported a leak the tests never triggered.
            //
            // `PromiseBook::resolve` is exact in both directions now, which means
            // NOTHING closes this promise: a shape-level command needs a shape-level
            // fact, and this producer has none. `Fact` carries ten variants and every
            // one of them is about an account.
            //
            // So this reply is rule 2.7's `accepted` WITHOUT `visible`, said out
            // loud: the sweep ran, `observations` and `failures` report what it did,
            // and `visible: false` tells the caller no frame will ever carry this
            // id, so it must not wait for one. The promise is closed on the way out
            // of BOTH arms — a promise never outlives the reply that answers it.
            //
            // **WHAT WOULD MAKE IT `visible: true`**, for whoever adds it: one
            // keyless fact meaning *the sweep completed* (`last_probe_at` on the
            // shape's `shared` block is the value the board already wants as "last
            // refresh"), emitted with `resolve::<..>(None)` as an ARGUMENT of the
            // emit. That is a new `Fact` variant and a new `shared` cell, which is
            // the shapes dispatch's to add, not this one's.
            let promise = manager.promises.open::<crate::commands::GaugeProbe>(None);
            let recorded = manager.probe_accounts_once(true).await;
            manager.promises.abandon(promise.id());
            match recorded {
                // **AN ACKNOWLEDGEMENT, NOT A FACT** (docs/shapes.md 2.4). This
                // used to answer `budgets.gauge_state(..)` — the gauge itself,
                // on the reply, a second source for what the shape carries.
                //
                // `observations` and `failures` STAY, and they are not facts
                // about the gauge: they are facts about THIS CALL — how many
                // polls it performed and which ones could not. A partial
                // failure still has to be able to say "two of three, and here
                // is why the third did not", and nothing else reports that. The
                // gauge VALUES are what left.
                Ok(n) => {
                    let mut payload = promise
                        .accepted(shapewire::command::Visible::Never)
                        .json();
                    payload["observations"] = json!(n);
                    payload["failures"] = json!(manager.last_probe_failures());
                    Ok(payload)
                }
                Err(reason) => Err(Error::Config(format!("gauge/probe did not poll: {reason}"))),
            }
        }

        // ── the daemon's own liveness and its restart ────────────────────

        // **WHICH BINARY IS ACTUALLY RUNNING.**
        //
        // > *"You need a way to be able to verify which binary is running. If
        // > you could just add an endpoint in the API surface with metadata,
        // > just anything we would want to know. Like whether it's signed
        // > even."*
        //
        // Answered about the IMAGE THIS PROCESS IS EXECUTING, from the kernel
        // — `csops(2)` for the code signature and `proc_pidinfo(2)` for the
        // text vnode — never from a path, because a path can be replaced
        // underneath a running process and on this machine it routinely is.
        //
        // The arm is thin on purpose: everything lives in `crate::image` so
        // the CLI verb renders the SAME payload and the two cannot drift into
        // two answers to one question, which is how the next investigation
        // would go wrong.
        "daemon/image" => {
            let exe = manager.update.exe();
            let data_dir = manager.data_dir().to_path_buf();
            Ok(crate::image::report(&crate::image::Context {
                launch_path: exe,
                data_dir: &data_dir,
                boot_identity: manager.update.boot_identity(),
                boot_id: Some(&manager.boot_id),
                image_booted_at: Some(&manager.started_at),
            }))
        }

        // **THE RESTART, AND IT GOES THROUGH THE SUPERVISOR.**
        //
        // It does NOT kill a process, and it is not `execv` on the spot. It
        // QUEUES a restart on the daemon's own update protocol, durably, and
        // that protocol fires at the first instant with no turn executing, no
        // spawn in flight and admission provably closed. So this cannot
        // interrupt the operator's work no matter when it is called — which is
        // the property that makes it safe to put on a button.
        //
        // # It is not reachable accidentally
        //
        // Three things, and the first is the one that matters:
        //
        // 1. **It cannot take effect at a bad moment.** A queued restart is not
        //    a kill. The worst outcome of an unintended call is that the daemon
        //    restarts sooner than it would have, between turns, which is the
        //    same thing the binary watcher already does unattended.
        // 2. It is classified WRITE by the client capability registry, so it is
        //    never probed — the registry probes READ methods to discover what a
        //    daemon supports, and probing this one would BE the operation.
        // 3. It REFUSES when this process is not a deployed daemon image, where
        //    the exec would relaunch a test harness or a CLI. That refusal is
        //    why the arm used to report `restart_supported` on `daemon.status` (retired by R2.S3c, R4.S9) rather
        //    than letting a client find out by trying.
        //
        // A `note` is recorded so `restarts.jsonl` says who asked and why,
        // rather than logging an anonymous operator restart.
        "daemon/restart" => {
            if manager.update.exe().is_none() {
                return Err(Error::Config(
                    "this process is not a deployed daemon binary, so a restart would not \
                     relaunch the daemon — it would re-exec whatever image is running (a test \
                     harness, or a CLI). REFUSED."
                        .into(),
                ));
            }
            let note = params
                .get("note")
                .and_then(Value::as_str)
                .unwrap_or("restart requested over the API")
                .to_string();
            manager
                .update
                .request_restart(crate::update::RestartReason::Operator { note: note.clone() });
            let pending = manager.update.pending();
            Ok(json!({
                "queued": true,
                "note": note,
                // NOT "restarting". The distinction is the whole design and the
                // client should be able to say it out loud: the restart is
                // ARMED and fires at the next quiet moment, which may be now or
                // may be after the turn that is running finishes.
                "fires": "at the first moment no turn is executing and nothing can start",
                "pending": pending.map(|p| json!({
                    "kind": p.kind, "note": p.note, "queued_at": p.queued_at,
                })),
            }))
        }

        // **THE LOGIN, IN TWO CALLS.**
        //
        // `claude auth login --claudeai` prints a URL and then blocks reading a
        // pasted `code#state` from STDIN. That is a conversation with a human
        // in the middle, which is why logging an account in was the one account
        // operation the interface could not perform: it had to suspend itself
        // and hand the operator a bare terminal.
        //
        // `begin` starts the child and returns the URL. `complete` hands back
        // what he pasted. In between the child is alive and parked on a read,
        // owned by `ThreadManager::logins`.
        //
        // PIPES, NOT A PTY — measured against claude v2.1.220 rather than
        // assumed. On a pipe the child prints the URL in plain UTF-8 with zero
        // ANSI escapes and accepts the pasted code normally; under a pty the
        // same output arrives wrapped in an OSC-8 hyperlink with the URL
        // repeated in a colour escape. A pty buys nothing and costs an
        // escape-stripping pass over the one string we must not corrupt. See
        // `crate::login` for the full finding, including the two behaviours
        // that would have been easy to get wrong by reasoning alone.
        "accounts/login/begin" => {
            let account = req_str(&params, "account")?;
            let opts = crate::login::LoginOptions {
                email: params
                    .get("email")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                force: params
                    .get("force")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            };
            manager.emit_progress(
                "login",
                &account,
                Some(&account),
                "starting",
                Some("running the account login script".into()),
                false,
                None,
            );
            let begun = manager
                .logins
                .begin(
                    &manager.add_account_script(),
                    manager.data_dir(),
                    &account,
                    opts,
                )
                .await
                .inspect_err(|e| {
                    manager.emit_progress(
                        "login",
                        &account,
                        Some(&account),
                        "failed",
                        Some(e.to_string()),
                        true,
                        Some(false),
                    );
                })?;
            manager.emit_progress(
                "login",
                &begun.session_id,
                Some(&begun.account),
                "awaiting_code",
                Some("open the URL, sign in, and paste the code back".into()),
                false,
                None,
            );
            Ok(json!({
                "session_id": begun.session_id,
                "account": begun.account,
                "url": begun.url,
                // What the caller must do next, named rather than implied, so
                // an interface does not have to encode the protocol twice.
                "next": "accounts/login/{session_id}/complete",
                "expires_in_secs": crate::login::SESSION_TTL.as_secs(),
            }))
        }
        "accounts/login/{session_id}/complete" => {
            // `session_id` is the correct key — two logins can be in flight for
            // two accounts — and since 2026-09-28 it is the only one.
            //
            // **THE `account` FALLBACK IS DELETED, and that is this change in
            // miniature.** It existed "because the interface's first cut sends
            // it", and it made one act reachable under two different subjects,
            // resolved by a precedence rule. That is the disagreement the path
            // move exists to delete: a session id and an account name are not
            // the same key, the resolution between them was a lookup that could
            // fail or find the wrong in-flight login, and no consumer can now
            // write the subject down twice.
            let session_id = req_str(&params, "session_id")?;
            let code = req_str(&params, "code")?;
            manager.emit_progress(
                "login",
                &session_id,
                None,
                "exchanging",
                Some("handing the code back to the login script".into()),
                false,
                None,
            );
            // R12.5b: ONE call — the guarded completion (crossed ⇒ refused and
            // reverted), the pool reload, and the flag re-check as its last act.
            // PS4's pending promise wraps exactly this.
            let crate::login::DaemonLogin { done, reopened, recheck } =
                crate::login::complete_daemon_login(manager, &session_id, &code).await?;

            // **THE HOOK.** On success the account's login bar is lifted on
            // every label of the login at once and it is placeable again on the
            // NEXT decision — no timer, no confirmation step, no waiting out
            // the recheck window.
            //
            // > *"then we'll know when the login has succeeded and you can drop
            // > that flag and immediately add that account to the pool as
            // > available."*
            //
            // It is called only on a zero exit, because `add-account` exits
            // non-zero for every refusal it makes: reopening an account on a
            // FAILED login would mark a dead credential placeable.
            // ── THE POOL, BEFORE THE HOOK ───────────────────────────────
            //
            // **THIS IS WHAT MAKES "ADD AN ACCOUNT FROM THE TUI" ACTUALLY
            // WORK.** `accounts.login.begin` on a label with no existing slot
            // creates the account end to end — `scripts/add-account` makes the
            // directory, logs in under it, and registers the slot with `threads
            // accounts adopt` — but every one of those writes happens in a CHILD
            // process, to `accounts.toml`, underneath a daemon whose pool was
            // fixed at boot. So the login succeeded, the credential was real,
            // and `accounts.list` / `.status` / `.pressure` all kept reporting
            // the roster from before it existed, until a restart.
            //
            // Unconditional rather than `if done.ok`: a login can fail AFTER
            // `add-account` has already adopted the slot, and a half-created
            // account the daemon cannot see is harder to diagnose than one it
            // can see and reports as unauthenticated. `reload_pool` is silent
            // and cheap when nothing moved. (Both now run inside
            // `complete_daemon_login`, in this order.)
            manager.emit_progress(
                "login",
                &session_id,
                Some(&done.account),
                if done.ok { "signed_in" } else { "failed" },
                Some(match &done.guard {
                    _ if done.ok => format!("{} is signed in and placeable again", done.account),
                    crate::login::GuardVerdict::Refused { reason, .. } => reason.clone(),
                    _ => "the login script refused or the exchange failed".to_string(),
                }),
                true,
                Some(done.ok),
            );
            let (refused, identity) = match &done.guard {
                crate::login::GuardVerdict::Refused { reason, .. } => (Some(reason.clone()), None),
                crate::login::GuardVerdict::Verified { email } => (None, Some(email.clone())),
                crate::login::GuardVerdict::NothingLanded => (None, None),
            };
            Ok(json!({
                "account": done.account,
                "ok": done.ok,
                "exit_code": done.exit_code,
                // R12.5b: the server-verified address, or why the login was
                // refused (and reverted), and the flag re-check that ran last.
                "identity": identity,
                "refused": refused,
                "recheck": recheck,
                // The labels whose login bar this lifted. Empty is a normal,
                // quiet outcome — the account may not have been barred.
                "reopened": reopened,
                // The script's OWN words. `add-account`'s refusals name the
                // remedy, and paraphrasing them would lose it.
                "output": done.output,
            }))
        }
        "accounts/login/{session_id}/cancel" => {
            let session_id = req_str(&params, "session_id")?;
            let cancelled = manager.logins.cancel(&session_id).await;
            if cancelled {
                manager.emit_progress(
                    "login",
                    &session_id,
                    None,
                    "cancelled",
                    None,
                    true,
                    Some(false),
                );
            }
            Ok(json!({ "cancelled": cancelled }))
        }

        // **THE ROTATION PASS, with its progress on the stream.**
        //
        // The interface used to get this by TAILING `accounts/rotation.log`
        // from a byte offset it captured beforehand, regex-parsing another
        // program's sentences into outcomes every 150ms, and classifying the
        // results by string prefix. Two things were wrong with that and only
        // one was the parsing: `rotation.log` is a SHARED append-only file, so
        // a pass started from the CLI at the same moment interleaved into the
        // same tail and was attributed to the client's own run.
        //
        // Now the log is pointed at a run-private file through the script's own
        // seam, each line becomes a `Progress` frame with a machine-readable
        // phase, and the VERDICT is the `--json` blob rather than the log. See
        // `crate::login::rotate`.
        "accounts/rotate" => {
            let dry_run = params
                .get("dry_run")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let run_id = uuid::Uuid::new_v4().to_string();
            let m = manager.clone();
            let id = run_id.clone();
            manager.emit_progress(
                "rotate",
                &run_id,
                None,
                if dry_run { "dry_run" } else { "started" },
                Some("refreshing every slot's credential".into()),
                false,
                None,
            );
            let script = manager.data_dir().join("scripts").join("claude-accounts");
            let outcome = crate::login::rotate(
                &script,
                manager.data_dir(),
                &run_id,
                dry_run,
                |phase, detail| m.emit_progress("rotate", &id, None, phase, Some(detail), false, None),
            )
            .await;
            match outcome {
                Ok((ok, result, _errs)) => {
                    manager.emit_progress(
                        "rotate",
                        &run_id,
                        None,
                        if ok { "finished" } else { "failed" },
                        None,
                        true,
                        Some(ok),
                    );
                    // A REAL rotation re-credentialled every slot, so login
                    // health across the whole board just moved. A DRY RUN
                    // changed nothing and must not claim otherwise — telling
                    // clients to re-read after a rehearsal would make the two
                    // modes indistinguishable from outside, which is the one
                    // thing a dry run exists to avoid.
                    if !dry_run {
                        manager.account_picture_changed("rotated");
                        // P6 FROM THE EVENT, not from the poll. A rotation
                        // re-credentials every slot and the daemon knows the
                        // instant it finished; waiting up to five minutes for
                        // the sampler to notice would be polling standing in
                        // for an event that exists.
                        let m2 = manager.clone();
                        tokio::task::spawn_blocking(move || m2.sample_credentials());
                    }
                    Ok(json!({ "run_id": run_id, "ok": ok, "dry_run": dry_run, "result": result }))
                }
                Err(e) => {
                    manager.emit_progress(
                        "rotate",
                        &run_id,
                        None,
                        "failed",
                        Some(e.to_string()),
                        true,
                        Some(false),
                    );
                    Err(e)
                }
            }
        }

        // ── account slot operations (were: shelling out to a bash tool) ───
        //
        // Both drive the SAME library functions the CLI verbs drive —
        // `accounts::rename_refusal` / `rename_account` and
        // `accounts::delete_refusal` / `delete_account` — so the credential
        // guards, the keychain ordering argument and the refusal vocabulary are
        // shared rather than reimplemented. The refusals arrive as STRUCTURED
        // errors instead of prose the client greps.
        "accounts/{account}/rename" => {
            let old = req_str(&params, "account")?;
            let new = req_str(&params, "new_name")?;
            let data_dir = manager.data_dir();
            let registry = crate::accounts::Registry::load(&data_dir.join("accounts.toml"))?;
            // The existing-name set is the registry PLUS any directory on disk
            // that is not registered, exactly as the CLI computes it: a rename
            // onto an unregistered directory would silently merge two slots.
            let mut existing = registry.names();
            if let Ok(rd) = std::fs::read_dir(data_dir.join("accounts")) {
                for e in rd.flatten() {
                    if let Some(n) = e.file_name().to_str() {
                        if e.path().is_dir() && !existing.iter().any(|x| x == n) {
                            existing.push(n.to_string());
                        }
                    }
                }
            }
            if let Some(why) = crate::accounts::rename_refusal(&old, &new, &existing) {
                return Err(Error::Config(why));
            }
            let o = crate::accounts::rename_account(
                data_dir,
                &old,
                &new,
                crate::accounts::KeychainMode::Auto,
            )?;
            // ── THE POOL ITSELF, FIRST ──────────────────────────────────
            //
            // `rename_account` moved the login DIRECTORY and rewrote
            // `accounts.toml`; the daemon's pool was built at boot and still
            // holds the old label pointing at the old path. Two things were
            // wrong until this line existed, and the second is the dangerous
            // one: `accounts.list` kept drawing a label that no longer exists,
            // and `config_dir_for` kept handing placement a `CLAUDE_CONFIG_DIR`
            // that had been moved out from under it — so a turn placed there
            // spawns against a directory that is gone.
            //
            // It runs BEFORE the announcement, always in this order: the frame
            // means "re-read this", and a client that re-reads faster than we
            // reload is told to re-read and handed the stale answer.
            manager.reload_pool();
            // The ROSTER changed — every client is showing a label that no
            // longer exists. Announced here rather than through
            // `account_picture_changed` because nothing about PLACEMENT moved:
            // the same logins are selectable, under one new name. Waking the
            // retry sweep to tell clients about a rename would be a side effect
            // on the wrong system.
            manager.emit_info(
                "accounts",
                "renamed",
                Some(&o.new_label),
                Some(format!("{} is now {}", o.old_label, o.new_label)),
            );
            Ok(json!({
                "old_label": o.old_label,
                "new_label": o.new_label,
                "old_dir": o.old_dir.to_string_lossy(),
                "new_dir": o.new_dir.to_string_lossy(),
                "credential_moved": o.credential_moved,
                // Threads already recorded against the old name KEEP it, in the
                // thread store and in budget history. Said here so the client
                // does not have to know it, and does not have to guess whether
                // history moved.
                "history_keeps_old_name": true,
            }))
        }
        "accounts/{account}/delete" => {
            let label = req_str(&params, "account")?;
            let force = params
                .get("force")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let data_dir = manager.data_dir();
            let home = crate::accounts::home_dir()?;
            let mut registry = crate::accounts::Registry::load(&data_dir.join("accounts.toml"))?;
            // The machine default is materialised IN MEMORY here for one
            // reason: `delete_refusal` needs it present to refuse deleting the
            // account the live login is signed in as. It is never saved —
            // `Registry::save` filters it — so this is a guard, not a
            // registration.
            registry.ensure_default();
            let now_ms = chrono::Utc::now().timestamp_millis();
            // R9.A2c-r2: the shared lookup — a DISABLED slot exists (unread)
            // and can be deleted (CON-28 bars reading it, not removing it).
            if let Some(why) =
                crate::accounts::delete_lookup(&registry, data_dir, &home, &label, force, now_ms)
            {
                return Err(Error::Config(why));
            }
            let o = crate::accounts::delete_account(
                data_dir,
                &label,
                crate::accounts::KeychainMode::Auto,
            )?;
            // The pool first, the announcement second — see `accounts.rename`
            // for why that order is not interchangeable. Without the reload a
            // deleted account stays in `accounts.list`, stays on
            // `accounts.pressure`, and stays RANKABLE by placement, which is the
            // worst of the three.
            manager.reload_pool();
            // A row LEFT the board. This one does move placement's inputs — a
            // login that was selectable is gone — so it rings the bell, which
            // both wakes the retry sweep and broadcasts.
            manager.account_picture_changed_for("deleted", Some(&label));
            Ok(json!({
                "label": o.label,
                "dir": o.dir.to_string_lossy(),
                "dir_removed": o.dir_removed,
                "keychain_removed": o.keychain_removed,
                "registry_removed": o.registry_removed,
                // LOCAL ONLY. Nothing was revoked server-side, and the payload
                // says so rather than leaving a reader to assume either way —
                // `accounts logout` is a REFUSED verb in this crate precisely
                // because revocation on a shared credential kills his live
                // session irreversibly.
                "revoked": false,
            }))
        }

        // `ledger/query` (Q11, spend grouped by thread or account) stood here.
        // RETIRED at R2.S3a: no face, board or tool called it, and the operator's
        // ruling is that the ability to request data goes. `Store::spend_summary`
        // stays — the budget calibration reads it.
        "shapes/list" => {
            // Reads each shape task's `watch::Sender<ShapeInfo>`. **No cell is
            // read from outside the task that owns it** — that is the whole
            // reason `shapes.list` can live in `dispatch` while `subscribe`
            // and `unsubscribe` cannot: this touches no connection state and
            // no held state.
            Ok(json!({ "shapes": manager.hub.list() }))
        }
        // ── agents — R4.S1 (`crate::agents`) ─────────────────────────────
        //
        // **ELEVEN ARMS, ONE REFUSAL, AND THE REFUSAL NAMES THE PATH.** The
        // resolving fact of each (`RunStarted`, `MailQueued`, `Held`,
        // `SuspensionArmed`, `NetworkCreated`, `NetworkMembership`, the `agents`
        // shape's rows, `agent_runs`) is the managed runtime's — R4.S2, another
        // branch — so each is `not_available_yet` naming its own template
        // (`method` IS the template here). Rule 2.7: refused where the caller
        // can see it, never as an absence. When the facts land, each arm opens
        // its promise and answers `Accepted` like `threads/{id}/update` above;
        // the façade's `acknowledgement_only` guard is what holds it to that.
        //
        // One line, deliberately: `ws_protocol::the_ws_method_inventory_is_pinned`
        // reads arms off this file a line at a time and splits on `|`.
        "agents/spawn_batch" | "agents/{agent}/interrupt" | "agents/watch" => {
            Err(crate::agents::not_available_yet(method))
        }
        // R4.S8: §2.4's bounded block, spelling (c) — a read that waits at most `poll_s`.
        "agents/gather" => {
            crate::managed::gather(manager, &params).await
        }
        // **THE NETWORK ACTS — R4.S3, LIVE.** `docs/agent-networks.md` §4.2:
        // every one acknowledges, and its fact is a ledger event on the
        // CALLER's own thread. The rules are all in `crate::networks`.
        "agents/{agent}/message" => {
            crate::networks::message(manager, &params)
        }
        "agents/{network}/broadcast" => {
            crate::networks::broadcast(manager, &params)
        }
        // R10.M3: change a link's notify policy without sending anything.
        "agents/{agent}/policy" => {
            crate::networks::policy::command(manager, &params)
        }
        "agents/network_create" => {
            crate::networks::create(manager, &params)
        }
        "agents/{network}/network_admit" => {
            crate::networks::admit(manager, &params)
        }
        "agents/{network}/network_join" => {
            crate::networks::join(manager, &params)
        }
        "agents/{network}/network_leave" => {
            crate::networks::leave(manager, &params)
        }
        "agents/{network}/network_remove" => {
            crate::networks::remove(manager, &params)
        }
        "agents/{network}/network_configure" => {
            crate::networks::configure(manager, &params)
        }
        "agents/reach" => {
            crate::networks::reach(manager, &params)
        }
        // ── R4.S2: THE MANAGED RUNTIME'S LIVE ARMS (`crate::managed`) ────
        //
        // `agents/spawn` is a COMMAND: it acknowledges with the `threads/create`
        // promise of the child it minted (the child's row on the `threads`
        // shape resolves it), plus the two addresses the act minted. The
        // `agents` shape and its `RunStarted` fact are not built; see the R4.S2
        // report. `list` and `result` are reads over the managed ledgers and the
        // child's own store rows.
        "agents/spawn" => {
            crate::managed::spawn(manager, &params)
        }
        "agents/list" => {
            crate::managed::list(manager, &params)
        }
        "agents/{agent}/result" => {
            crate::managed::result(manager, &params)
        }
        // **THE ONE LIVE ARM.** Rows of `threads/log` for the caller's own
        // thread, through the shape's own `row_of`; any other `agent` is
        // refused naming the lineage that would authorise it.
        "agents/{agent}/log" => {
            crate::agents::log(manager, &params)
        }
        other => Err(Error::Config(format!("unknown method: {other}"))),
    }
}

/// Which threads currently point at a budget key (several may share one —
/// DEC-4).
fn threads_using_key(manager: &ThreadManager, key: &str) -> Result<Vec<String>> {
    Ok(manager
        .store
        .list_threads(1000)?
        .into_iter()
        .filter(|r| {
            ThreadSettings::from_stored(&r.settings_json)
                .map(|s| s.budget_key.as_deref() == Some(key))
                .unwrap_or(false)
        })
        .map(|r| r.id)
        .collect())
}

/// Render one thread row for `threads.get` / `threads.list`.
///
/// **THE STATUS HERE IS DERIVED, NOT ECHOED.** `d` is this thread's entry from
/// [`ThreadManager::thread_states`] — the same computation `threads.status`
/// answers from — so the two calls cannot disagree. Before this, `status` was
/// the raw `threads.status` COLUMN, and it is what reported two completed arena
/// threads as `busy` for twenty-five hours.
///
/// **THREE KEYS, ON PURPOSE — all three are load-bearing on the wire:**
///
///   * `status` — the legacy `busy | retrying | idle` projection
///     ([`DerivedState::status_word`]). AUI's `status_str_is_running` accepts
///     ONLY `busy` and `retrying` here and is pinned by AUI's own tests; its
///     `running_from_list` fallback paints the whole fleet from this key when
///     `threads.status` is unavailable. Drop or rename it and every thread
///     silently reads as idle in the operator's ear.
///   * `state` — the full `active | parked | retrying | queued | idle`
///     vocabulary, verbatim from the derivation.
///   * `executing` — the boolean AUI's `status_frame_executing` prefers over
///     `state`. It is exactly "a live execution holds this thread's lock".
///
/// The COUNTS (`unread`, `last_seen_seq`) are deliberately absent: they are a
/// per-thread `events` fan-out measured at ~0.26s over this database, and
/// `threads.list` neither returned them before nor needs them. They live on
/// `threads.status`.
///
/// `d` is `Option` only so a row whose derivation is somehow missing degrades
/// to the honest `idle` reading rather than dropping the thread from the list.
fn thread_json(r: &crate::store::ThreadRow, d: Option<&DerivedState>) -> Value {
    let owned;
    let d = match d {
        Some(d) => d,
        None => {
            owned = DerivedState::idle(&r.id);
            &owned
        }
    };
    json!({
        "thread_id": r.id,
        "status": d.status_word(),
        "state": d.state,
        "executing": d.executing,
        "account": r.account,
        "cwd": r.cwd, "model": r.model,
        "settings": serde_json::from_str::<Value>(&r.settings_json).unwrap_or(Value::Null),
        "last_seq": r.last_seq, "next_message_id": r.next_message_id,
        // **THE COMPACTION MARKER, ON THE RPC ROW TOO.**
        //
        // The `threads` SHAPE has carried this since it was written — the
        // `compaction` cell's `CompactionRow` holds both durable fields. This
        // row did not, so a consumer on the RPC path could read `last_seq` and
        // `next_message_id` and have no way to ask where the last compaction
        // ended, even though `ThreadRow` has read the column all along.
        //
        // Both are rendered because `update_thread_compaction` writes them
        // TOGETHER: `needs_restate` without its boundary is a flag a consumer
        // cannot act on, and the boundary without the flag does not say whether
        // the producer still owes a restatement.
        "last_compaction_message_id": r.last_compaction_message_id,
        "needs_restate": r.needs_restate,
        "created_at": r.created_at, "updated_at": r.updated_at,
    })
}

/// `threads.query` page size when the caller does not say.
pub const QUERY_DEFAULT_LIMIT: usize = 50;
/// `threads.query` hard cap — a citation is a citation, not a transcript dump.
pub const QUERY_MAX_LIMIT: usize = 500;

/// What "a message" means when a caller cites one: the operator's words and
/// the agent's reply. Everything else is an EVENT and must be asked for by
/// name (`kinds`) or by `include:"all"`.
pub const MESSAGE_KINDS: &[&str] = &["user_message", "assistant_text"];

/// The role a citing caller reads off a row.
fn cited_role(ev: &crate::event::ThreadEvent) -> &'static str {
    use crate::event::ThreadEvent as E;
    match ev {
        E::UserMessage { .. } => "user",
        E::AssistantText { .. } => "assistant",
        E::Thinking { .. } => "thinking",
        E::ThreadSystemInterrupted { .. }
        | E::ThreadSystemResumed { .. }
        | E::ThreadSystemQueued { .. } => "system",
        E::Custom { .. } => "application",
        _ => "event",
    }
}

/// The citable BODY of an event. Kinds that carry prose give it verbatim;
/// everything else gives its compact payload, so a caller never gets an empty
/// row it cannot interpret.
fn cited_text(ev: &crate::event::ThreadEvent) -> String {
    use crate::event::ThreadEvent as E;
    match ev {
        E::UserMessage { text, .. }
        | E::AssistantText { text, .. }
        | E::Thinking { text, .. }
        | E::ContextInjected { text } => text.clone(),
        E::TurnFailed { message, .. } => message.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// `pub(crate)` since R2.S3d: a message PAGE (`crate::shapes::pages`) spells each
/// message with this one function, so the resource door and the query it
/// replaced serve the same object. `pub` since R2.S3c (R4.S9) retired the
/// `threads/{thread_id}/query` request: `tests/message_query.rs` pins the
/// spelling here, where it now lives, instead of through the retired arm.
pub fn cited_message_json(e: &crate::event::StoredEvent, want_text: bool, max_chars: usize) -> Value {
    let full = cited_text(&e.event);
    let chars = full.chars().count();
    let (text, truncated) = if !want_text {
        (None, false)
    } else if max_chars > 0 && chars > max_chars {
        (Some(full.chars().take(max_chars).collect::<String>()), true)
    } else {
        (Some(full), false)
    };
    let mut obj = json!({
        "message_id": e.message_id,
        "seq": e.seq,
        "ts": e.ts,
        "role": cited_role(&e.event),
        "kind": e.event.kind(),
        "chars": chars,
        "truncated": truncated,
        "audience": e.event.audience().as_str(),
    });
    if let Some(t) = text {
        obj["text"] = Value::String(t);
    }
    if let crate::event::ThreadEvent::UserMessage { metadata, .. } = &e.event {
        if !metadata.is_null() {
            obj["metadata"] = metadata.clone();
        }
    }
    obj
}

fn event_json(e: crate::event::StoredEvent) -> Value {
    json!({
        "seq": e.seq, "message_id": e.message_id, "ts": e.ts, "event": e.event,
    })
}

/// **THE ALIAS IS NOT A SUBJECT, SO A COMMAND ABOUT IT IS REFUSED.**
///
/// `wire::Resolving`'s own doc states the obligation: *"a producer whose fold
/// re-keys an input — the `accounts` shape resolves the `default` alias onto
/// whichever account hosts the machine login — must name the key the fact will fold
/// under, not the alias it was called with."* These three handlers passed the
/// request string straight through: `open(Some("default"))` →
/// `emit_*_fact("default")` → `resolve(Some("default"))`, while
/// `AccountsRoot::resolve` re-keys `default` onto its host (or to NOTHING, when no
/// pool login hosts the machine store). The row then flushed under the host key,
/// `take_key("default")` found nothing, and the ids stayed pending **forever** — on
/// a command the store had accepted, because `set_account_disabled` and
/// `set_pool_share` happily insert a row for `"default"` too.
///
/// **Refusing rather than resolving the alias**, for the operator's own reason:
/// *"I do not want default even in the purview of the system. Why does it even see
/// that there is a default? … It's a completely separate system, not even visible
/// to placement."* `default` is not a pool member (`pool.rs`: membership is *names a
/// config dir*, and the machine login is precisely the one that names none), so
/// there is no account here to disable, enable or lend — only the account that
/// HOSTS it, which the caller can name. That is the same shape as
/// `Store::set_pool_share`'s refusal of a zero share and the same shape as
/// `accounts rename`/`adopt`'s existing refusals of this label, and the refusal
/// names the door.
fn refuse_the_machine_alias(account: &str, method: &str) -> Result<()> {
    if account == crate::accounts::DEFAULT_ACCOUNT {
        return Err(Error::Config(format!(
            "{:?} is the machine login, not an account slot — `{method}` cannot move \
             it. The machine login folds into whichever pool account is signed in as \
             it (`shared.machine_login.hosted_by` on the `accounts` shape says which, \
             and `unhosted: true` says nothing does); name THAT account. Passing the \
             alias wrote a row nothing reads and left the command's promise pending \
             forever, because the fold keys the fact under the host and not under \
             the alias.",
            crate::accounts::DEFAULT_ACCOUNT
        )));
    }
    Ok(())
}

/// **THE ONE NOT-FOUND REFUSAL of the attach/detach verbs** (SH-A2c,
/// DEPLOY-9k finding 2): an attach on a thread that does not exist, a detach
/// of an unknown cookie (or another thread's), and the loser of a detach
/// racing the reaper all leave the door through here, so all three carry ONE
/// wire code, `not_found` — never `unknown`, never `busy`. `tag` is the
/// refusal's own `code()`, kept in the prose as before.
fn refused_not_found(e: &dyn std::fmt::Display, tag: &str) -> Error {
    Error::Refused { code: shapewire::envelope::codes::NOT_FOUND, message: format!("{e} [{tag}]") }
}

fn req_str(params: &Value, key: &str) -> Result<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| Error::Config(format!("missing required param: {key}")))
}
fn opt_u64(params: &Value, key: &str) -> Option<u64> {
    params.get(key).and_then(Value::as_u64)
}
fn opt_usize(params: &Value, key: &str) -> Option<usize> {
    params.get(key).and_then(Value::as_u64).map(|v| v as usize)
}
fn str_list(params: &Value, key: &str) -> Option<Vec<String>> {
    params.get(key).and_then(Value::as_array).map(|a| {
        a.iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    })
}

// ═════════════════════════════════════════════════════════════════════════════
#[cfg(test)]
mod command_promise_tests {
    //! **The liveness half of rule 4** (`docs/shapes.md` 2.4).
    //!
    //! Types prove WHICH event resolves a command. No type can prove that the
    //! emit HAPPENS — that is liveness. This is the doc's suite-wide property
    //! test: run a command through a real producer, and assert no promise is
    //! left open. A handler that forgot to emit shows up here and nowhere else.
    //!
    //! Written in-crate because `handle` is private: the alternative was making
    //! the dispatch surface public purely so a test could reach it, which trades
    //! a real encapsulation for a test convenience.
    //!
    //! **Liveness is only half of what a promise needs, and this is the half it
    //! covers.** "The book is empty" says the emit happened; it says nothing
    //! about whether the id reached the consumer that was waiting, because the
    //! book is emptied by a resolve and the frame is delivered by `drive`. The
    //! other half — the id rides the frame for the SUBJECT the command was about,
    //! which is the only frame a consumer holding that key receives — is asserted
    //! from the consumer's side in `tests/shapes.rs`
    //! (`a_promise_id_rides_the_frame_for_the_key_its_command_was_about`). Both
    //! were green while a command about `bb` was answered on `aa`'s frame; only
    //! the second one could see it.

    use super::*;
    use std::collections::BTreeMap;
    use std::path::Path;

    /// **Make the `accounts` shape actually TRACKED.**
    ///
    /// Without this every test below takes `emit_disabled_fact`'s
    /// not-tracking early return, where the promise closes without an emit —
    /// so a deleted emit would still leave the book empty and every assertion
    /// green. The subscribe is what puts the real emit path under test. This is
    /// precisely the hole the forced-red pass exposed.
    async fn tracked_manager(dir: &Path, accounts: &[&str]) -> crate::ThreadManager {
        tracked_manager_with(config_at(dir, accounts)).await
    }

    async fn tracked_manager_with(cfg: crate::Config) -> crate::ThreadManager {
        let m = crate::build_manager(cfg).unwrap();
        m.start_watchers();
        m.hub
            .subscribe("accounts", None, 1, None)
            .expect("subscribe to accounts");
        // The Lazy task flips `tracking` on the subscribe item as it drains.
        for _ in 0..200 {
            if m.hub.any_tracking() {
                return m;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the accounts shape never began tracking — the test below would be vacuous");
    }

    fn config_at(dir: &Path, accounts: &[&str]) -> crate::Config {
        let mut map = BTreeMap::new();
        for name in accounts {
            map.insert(name.to_string(), dir.join(name).to_string_lossy().to_string());
        }
        crate::Config {
            data_dir: Some(dir.to_string_lossy().to_string()),
            accounts: map,
            calibrate: false,
            edge_poll: false,
            probe_interval_secs: 0,
            ..Default::default()
        }
    }

    /// **`accounts.disable` answers an acknowledgement and NOT a fact.**
    ///
    /// The reply used to be `pool_share_rows` — the new row, on the reply. The
    /// assertion is in two halves because only together do they mean the rule
    /// holds: there IS a promise id, and there is NO share anywhere in it.
    #[tokio::test]
    async fn disable_answers_an_acknowledgement_and_leaves_no_promise_open() {
        let tmp = tempfile::tempdir().unwrap();
        let m = tracked_manager(tmp.path(), &["aa", "bb"]).await;

        let out = route_request(&m, "accounts/aa/disable", json!({}))
            .await
            .expect("disable");

        assert!(
            out.get("promise_id").is_some(),
            "a command replies with an acknowledgement: {out}"
        );
        assert!(
            out.get("shares").is_none() && out.get("share").is_none(),
            "and NEVER with the fact it caused — that is the second source \
             rule 4 exists to delete: {out}"
        );
        assert!(
            m.promises.is_empty(),
            "no promise left open after the producer ran: {:?}",
            m.promises.open_ids()
        );
    }

    /// The sibling the doc did not name. Same rule, same shape of proof.
    #[tokio::test]
    async fn enable_answers_an_acknowledgement_and_leaves_no_promise_open() {
        let tmp = tempfile::tempdir().unwrap();
        let m = tracked_manager(tmp.path(), &["aa", "bb"]).await;
        route_request(&m, "accounts/aa/disable", json!({}))
            .await
            .unwrap();

        let out = route_request(&m, "accounts/aa/enable", json!({}))
            .await
            .expect("enable");

        assert!(out.get("promise_id").is_some(), "acknowledgement: {out}");
        assert!(out.get("shares").is_none(), "no fact on the reply: {out}");
        assert!(
            m.promises.is_empty(),
            "no promise left open: {:?}",
            m.promises.open_ids()
        );
    }

    /// **A no-op still resolves.** Disabling an already-disabled account changes
    /// nothing, and the fact must be emitted anyway as a re-assertion — or the
    /// test cannot tell "resolved by re-assertion" from "never emitted at all".
    /// Levels absorb the re-assertion for free.
    #[tokio::test]
    async fn a_no_op_command_still_resolves_its_promise() {
        let tmp = tempfile::tempdir().unwrap();
        let m = tracked_manager(tmp.path(), &["aa"]).await;

        route_request(&m, "accounts/aa/disable", json!({}))
            .await
            .unwrap();
        assert!(m.promises.is_empty());

        // Again. Nothing moves in the store; the promise must still close.
        route_request(&m, "accounts/aa/disable", json!({}))
            .await
            .unwrap();
        assert!(
            m.promises.is_empty(),
            "a no-op that does not re-assert is indistinguishable from a \
             handler that forgot to emit: {:?}",
            m.promises.open_ids()
        );
    }

    /// **"IT TOOK" AND "NOBODY WILL EVER TELL YOU" ARE NOT THE SAME REPLY** —
    /// rule 2.7's `accepted` without `visible`, and the audit's recommended first
    /// amendment.
    ///
    /// The not-tracking early return closes the promise with no emit. The comment
    /// justifying it was right about the book and wrong about the wire, and it said
    /// why: *"a consumer that is not subscribed cannot be waiting."* That assumption
    /// is false for the very next consumer in line — the AUI issues commands from a
    /// process not yet subscribed to the shape they move — and the reply was
    /// BYTE-IDENTICAL on both paths, so a writer could hold an id that resolves
    /// nowhere and could not tell that from a slow fold.
    ///
    /// Asserted as a DIFFERENCE between two replies from the same handler, not as a
    /// field's presence: a test that only checked `visible` exists would stay green
    /// against a handler that hard-coded it.
    ///
    /// **Forced red** by returning `Visible::Expected` from the not-tracking arm of
    /// `emit_disabled_fact`.
    #[tokio::test]
    async fn the_reply_distinguishes_a_frame_is_coming_from_nothing_ever_will() {
        // ── NOTHING TRACKS: `accepted`, and no frame will follow ──────────────
        let quiet = tempfile::tempdir().unwrap();
        let m = crate::build_manager(config_at(quiet.path(), &["aa"])).unwrap();
        assert!(
            !m.hub.any_tracking(),
            "the fixture must NOT subscribe — this is the untracked path"
        );
        let untracked = route_request(&m, "accounts/aa/disable", json!({}))
            .await
            .expect("the store write happens either way — this is not a refusal");
        assert!(
            untracked.get("promise_id").is_some(),
            "still accepted, and still named: {untracked}"
        );
        assert_eq!(
            untracked["visible"],
            json!(false),
            "nothing is folding, so no frame will ever carry this id and the reply \
             has to say so: {untracked}"
        );
        assert!(
            m.store.account_disabled_at("aa").unwrap().is_some(),
            "and it TOOK — `visible: false` is not a refusal"
        );
        assert!(
            m.promises.is_empty(),
            "an abandoned promise is closed, not leaked: {:?}",
            m.promises.open_ids()
        );

        // ── SOMETHING TRACKS: a frame is coming ───────────────────────────────
        let busy = tempfile::tempdir().unwrap();
        let m = tracked_manager(busy.path(), &["aa"]).await;
        let tracked = route_request(&m, "accounts/aa/disable", json!({}))
            .await
            .unwrap();
        assert_eq!(
            tracked["visible"],
            json!(true),
            "a fold is tracking, so the frame carrying this id is on its way: {tracked}"
        );

        assert_ne!(
            untracked["visible"], tracked["visible"],
            "THE WHOLE BUG: these two replies used to be byte-identical, and the \
             second one is the only one a consumer may wait on"
        );
    }

    /// **`changed` SAYS WHETHER THIS CALL MOVED ANYTHING** — rule 2.7 applied to a
    /// no-op.
    ///
    /// `docs/shapes.md` 2.4 requires a no-op to RE-ASSERT its fact so the liveness
    /// test can tell *resolved by re-assertion* from *never emitted*. Nothing told
    /// the CONSUMER which happened: two operators, or a retry after a timeout, or a
    /// script — each got the same answer whether it was the call that disabled the
    /// account or the second one. The only tell available was that the upsert
    /// deliberately does not rewrite `disabled_at`, so `disabled_at < frame.at`
    /// implied "already true" — a derivation from two stamps, which is exactly what
    /// the rule exists to stop consumers doing.
    ///
    /// **Forced red** by passing a literal `true` to `.changed(..)` in the
    /// `accounts.disable` arm.
    #[tokio::test]
    async fn a_reassertion_says_it_is_one() {
        let tmp = tempfile::tempdir().unwrap();
        let m = tracked_manager(tmp.path(), &["aa"]).await;

        let first = route_request(&m, "accounts/aa/disable", json!({}))
            .await
            .unwrap();
        assert_eq!(first["changed"], json!(true), "this call did it: {first}");

        let again = route_request(&m, "accounts/aa/disable", json!({}))
            .await
            .unwrap();
        assert_eq!(
            again["changed"],
            json!(false),
            "it was already true, and the caller is told rather than left to \
             compare two stamps: {again}"
        );
        assert_ne!(first["promise_id"], again["promise_id"]);

        // And the other direction: enable moves it back, then does not.
        let back = route_request(&m, "accounts/aa/enable", json!({}))
            .await
            .unwrap();
        assert_eq!(back["changed"], json!(true), "{back}");
        let again = route_request(&m, "accounts/aa/enable", json!({}))
            .await
            .unwrap();
        assert_eq!(again["changed"], json!(false), "{again}");
    }

    /// **`gauge.probe` LEAVES NO PROMISE OPEN, ON EITHER ARM.**
    ///
    /// Two bugs, one root. `PromiseBook::resolve` let any KEYED fact close a
    /// SUBJECTLESS promise, so:
    ///
    /// * the sweep's promise closed on the FIRST `GaugeObserved` of the sweep —
    ///   collapsing *my sweep completed* with *one observation of one account
    ///   arrived* — and the background poller emits down the same path, so the
    ///   observation that closed it need not have been part of the sweep; and
    /// * on the `Err` arm nothing resolved it at all, so the book held it forever
    ///   and `open_ids()` — the liveness oracle — reported a leak no test triggered.
    ///
    /// Now nothing closes it, because a shape-level command needs a shape-level
    /// fact and this producer has none — so the reply says `visible: false` and the
    /// handler closes it on the way out of both arms.
    ///
    /// **Forced red** by deleting `manager.promises.abandon(promise.id())` from the
    /// `gauge.probe` arm: the `Err` half fails on the book, the `Ok` half too.
    #[tokio::test]
    async fn gauge_probe_leaks_no_promise_on_either_arm() {
        // ── THE `Err` ARM. No accounts registered → `poll_targets()` is empty →
        //    `probe_accounts_once` refuses before it spawns anything.
        let tmp = tempfile::tempdir().unwrap();
        let m = crate::build_manager(config_at(tmp.path(), &[])).unwrap();
        let err = route_request(&m, "gauge/probe", json!({})).await;
        assert!(err.is_err(), "nothing to poll is a refusal: {err:?}");
        assert!(
            m.promises.is_empty(),
            "a promise never outlives the reply that refused it: {:?}",
            m.promises.open_ids()
        );

        // ── THE `Ok` ARM, on a tracked shape — where the over-broad match used to
        //    close this promise on somebody else's observation.
        std::env::set_var(
            "THREADS_CLAUDE_BIN",
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-claude"),
        );
        let tmp2 = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp2.path().join("aa")).unwrap();
        let m = tracked_manager(tmp2.path(), &["aa"]).await;
        let out = route_request(&m, "gauge/probe", json!({})).await.expect("probe");
        assert!(out.get("promise_id").is_some(), "still accepted: {out}");
        assert_eq!(
            out["visible"],
            json!(false),
            "no fact this producer emits is `the sweep completed`, so no frame will \
             carry this id and the reply must not pretend one will: {out}"
        );
        assert!(
            out.get("observations").is_some(),
            "`observations` and `failures` are facts about the CALL and stay: {out}"
        );
        assert!(
            m.promises.is_empty(),
            "and nothing is left open: {:?}",
            m.promises.open_ids()
        );
    }

    /// **THE MACHINE ALIAS IS REFUSED ON ALL THREE WRITE DOORS.**
    ///
    /// `wire::Resolving` states the obligation — *"a producer whose fold re-keys an
    /// input … must name the key the fact will fold under, not the alias it was
    /// called with"* — and these three handlers passed the request string straight
    /// through. `AccountsRoot::resolve` re-keys `default` onto whichever account
    /// hosts the machine login (or onto NOTHING when none does), the row flushed
    /// under the host key, `take_key("default")` found nothing, and the ids stayed
    /// pending forever on a command the store had accepted.
    ///
    /// **WHERE THE PENDING ID ACTUALLY SITS, checked rather than assumed.** It is NOT
    /// in the book: `emit_disabled_fact` resolves at the emit, so the book empties
    /// whether or not any frame can carry the routing. The id is stranded one layer
    /// out, in `ShapeTask::pending_promises`, under a key the fold never flushes —
    /// so `promises.is_empty()` here proves only that this handler minted nothing,
    /// and the consumer-visible half is
    /// `tests/shapes.rs::the_machine_alias_is_refused_instead_of_stranding_a_promise`.
    /// The first draft of this test claimed the book would go red under the mutation;
    /// the scratch run that disproved it is why this paragraph exists.
    ///
    /// **Forced red** by deleting the `refuse_the_machine_alias` call from the
    /// `accounts.disable` arm: `must refuse the machine alias, got Ok(..)`.
    #[tokio::test]
    async fn the_machine_alias_is_refused_rather_than_left_pending() {
        let tmp = tempfile::tempdir().unwrap();
        let m = tracked_manager(tmp.path(), &["aa"]).await;
        let alias = crate::accounts::DEFAULT_ACCOUNT;

        for (method, params) in [
            (format!("accounts/{alias}/disable"), json!({})),
            (format!("accounts/{alias}/enable"), json!({})),
            (
                format!("accounts/{alias}/pool_share_set"),
                json!({ "share": 0.5 }),
            ),
        ] {
            let out = route_request(&m, &method, params).await;
            let Err(e) = out else {
                panic!("`{method}` must refuse the machine alias, got {out:?}");
            };
            let msg = e.to_string();
            assert!(
                msg.contains("machine login") && msg.contains("hosted_by"),
                "the refusal names the door — which account to name instead: {msg}"
            );
            assert!(
                m.promises.is_empty(),
                "and it refuses BEFORE minting a promise — see the docstring for why \
                 this is the weaker half of the claim: {:?}",
                m.promises.open_ids()
            );
            assert!(
                m.store.account_disabled_at(alias).unwrap().is_none(),
                "and it writes no row for a name that is not an account"
            );
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // THE PATH GRAMMAR — `shapewire::server::MethodPath`
    // ═════════════════════════════════════════════════════════════════════════

    /// **EVERY TEMPLATE IN THE INVENTORY IS WELL FORMED, AND NOTHING SITS AT THE
    /// ROOT.**
    ///
    /// `MethodPath::parse` panics on a malformed template, so this is the test
    /// that turns a typo in `WS_METHODS` into a red rather than into a daemon
    /// that panics on the first request for that method.
    ///
    /// **Forced red** by adding `"restart"` to `WS_METHODS`:
    /// `a method template has at least a family and a leaf: "restart"`.
    #[test]
    fn every_template_is_well_formed_and_nothing_is_at_the_root() {
        for t in WS_METHODS {
            let m = shapewire::server::MethodPath::parse(t);
            assert!(!m.family.is_empty(), "{t}");
            assert!(!m.leaf.is_empty(), "{t}");
            assert!(
                !m.leaf.contains('/'),
                "a leaf is ONE segment; multi-word leaves spell the space `_`: {t}"
            );
            assert!(
                t.split('/').count() >= 2,
                "nothing sits at the root that belongs under a noun: {t}"
            );
        }
    }

    /// **NO FAMILY SHADOWS A KEY.**
    ///
    /// The one ambiguity the right-to-left parse can have: if `a/b/leaf` is a
    /// template AND `a/{k}/leaf` is one, then a key whose value is `b` is
    /// unaddressable, because the longest family wins. No such pair exists; this
    /// is what says so, and it is why `accounts/login/begin` is safe to sit
    /// beside `accounts/{account}/disable`.
    ///
    /// **Forced red** by adding `"accounts/login/disable"` to `WS_METHODS`:
    /// `` `accounts/login/disable` shadows the key `login` of `accounts/{account}/disable` ``.
    #[test]
    fn no_family_shadows_a_key() {
        let parsed: Vec<_> = WS_METHODS
            .iter()
            .map(|t| (*t, shapewire::server::MethodPath::parse(t)))
            .collect();
        for (fixed_t, fixed) in &parsed {
            if fixed.subject.is_some() {
                continue;
            }
            let Some((outer, last)) = fixed.family.rsplit_once('/') else {
                continue;
            };
            for (keyed_t, keyed) in &parsed {
                if keyed.subject.is_none() || keyed.family != outer || keyed.leaf != fixed.leaf {
                    continue;
                }
                panic!("`{fixed_t}` shadows the key `{last}` of `{keyed_t}`");
            }
        }
    }

    /// **THE SUBJECT MAY NOT BE WRITTEN DOWN TWICE.**
    ///
    /// The operator's whole argument, as an assertion. A request that names its
    /// account in the path AND in the params is REFUSED rather than resolved by
    /// precedence, because a precedence rule is a place for the two to disagree
    /// — and `accounts.disable default` disagreeing with the fold's re-keyed
    /// host account is what stranded promise ids forever.
    ///
    /// **Forced red** by making `bind_subject` overwrite instead of refuse
    /// (`obj.insert(..)` with the `contains_key` guard removed):
    /// `the subject may not be written down twice: Ok(Object {"changed": Bool(true), ...})`.
    #[tokio::test]
    async fn the_subject_may_not_be_written_down_twice() {
        let tmp = tempfile::tempdir().unwrap();
        let m = tracked_manager(tmp.path(), &["aa", "bb"]).await;
        let out = route_request(&m, "accounts/aa/disable", json!({ "account": "bb" })).await;
        let Err(e) = out else {
            panic!("the subject may not be written down twice: {out:?}");
        };
        let msg = e.to_string();
        assert!(
            msg.contains("is the SUBJECT of") && msg.contains("accounts/aa/disable"),
            "the refusal names the one place the subject may be written: {msg}"
        );
        assert!(
            m.promises.is_empty(),
            "and it refuses before minting a promise: {:?}",
            m.promises.open_ids()
        );
    }

    // ═════════════════════════════════════════════════════════════════════════
    // RULE 2.4 — THE FIVE THAT REMAINED
    // ═════════════════════════════════════════════════════════════════════════

    /// The `threads` pool, TRACKING, with standing membership interest.
    ///
    /// The membership subscribe is not decoration: `threads` is `OnDemand` and
    /// its fold mints no row, so for a thread nobody holds the key of the ONLY
    /// frame that will ever exist is the `added` one — which is exactly what
    /// `threads/create` resolves on. Without this door the create test would be
    /// asserting against a frame nobody could receive.
    async fn tracked_threads(dir: &Path) -> (crate::ThreadManager, crate::shape::ConnFrames) {
        tracked_threads_with(config_at(dir, &["aa"])).await
    }

    /// [`tracked_threads`] whose `claude_bin` names nothing. For a test whose
    /// command NUDGES THE DISPATCHER (`queue_reselect`): a re-selected row goes
    /// out on the ordinary path, and a unit test must never spawn the real CLI.
    async fn tracked_threads_no_cli(
        dir: &Path,
    ) -> (crate::ThreadManager, crate::shape::ConnFrames) {
        let mut cfg = config_at(dir, &["aa"]);
        cfg.claude_bin = Some(dir.join("no-claude-here").to_string_lossy().to_string());
        tracked_threads_with(cfg).await
    }

    async fn tracked_threads_with(
        cfg: crate::Config,
    ) -> (crate::ThreadManager, crate::shape::ConnFrames) {
        let m = tracked_manager_with(cfg).await;
        let frames = m.hub.connect(2);
        m.hub
            .subscribe_as(
                "threads",
                None,
                2,
                None,
                None,
                crate::shape::Detail::Keys,
                crate::shape::Retain::Max,
                crate::shape::Beat::Max,
                // States no version — rule 2.6's served-anyway case, which is what
                // a test harness is: it is not a shipped consumer and has no
                // contract of its own to claim.
                None,
                shapewire::wire::Paging::Default,
                None,
            )
            .expect("standing membership interest in the pool");
        for _ in 0..200 {
            if m.hub.tracked("threads") {
                // **AND WAIT FOR INIT TO FINISH, WHICH IS A SEPARATE MOMENT.**
                // `tracked()` flips when the subscribe is QUEUED; the shape task
                // then hydrates, and it folds whatever arrived meanwhile into the
                // root under construction with `let _ = root.drain()` — a hydrate
                // publishes nothing, the export IS the publication. A `create`
                // that lands inside that window produces no `added` frame at all,
                // so without this the test would be asserting against a frame the
                // chassis deliberately did not send.
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                return (m, frames);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the threads shape never began tracking — the tests below would be vacuous");
    }

    /// **HOLD ONE THREAD'S KEY**, and wait for its export.
    ///
    /// `threads` is `OnDemand`: its fold drops every row-touching arm for a key
    /// the root does not hold, so without this there is no `row` frame for the
    /// promise id to ride and the "it rode a frame" assertions below would be
    /// asserting against a frame the chassis deliberately did not send.
    async fn hold_key(m: &crate::ThreadManager, id: &str) {
        m.hub
            .subscribe("threads", Some(id), 2, None)
            .expect("a keyed subscribe");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }

    /// **THE IDS THIS CONNECTION WAS ACTUALLY HANDED FOR `key`.**
    ///
    /// The half `PromiseBook::is_empty` cannot see. An emptied book says the
    /// resolve RAN; it says nothing about whether a frame carried the id to the
    /// consumer that was waiting — and a `resolve` sitting as a statement ABOVE
    /// its emit empties the book with no frame at all. That shape is the one the
    /// original forced-red pass shipped twice, so it is asserted directly.
    fn ids_for(frames: &[std::sync::Arc<crate::shape::ShapeFrame>], key: &str) -> Vec<u64> {
        frames
            .iter()
            .filter(|fr| fr.wire.key.as_deref() == Some(key))
            .flat_map(|fr| fr.wire.promises.clone())
            .collect()
    }

    fn a_thread(dir: &Path) -> crate::ThreadSettings {
        let mut s = crate::ThreadSettings::new(dir.to_string_lossy().to_string(), "fable");
        s.injection = crate::settings::Injection::NOTHING;
        s
    }

    /// Every frame this connection has buffered.
    fn drained(f: &mut crate::shape::ConnFrames) -> Vec<std::sync::Arc<crate::shape::ShapeFrame>> {
        let mut all = Vec::new();
        while let Ok(x) = f.try_recv() {
            all.push(x);
        }
        all
    }

    /// The `summary_updated` payloads on a thread's ledger, oldest first.
    fn names_of(m: &ThreadManager, id: &str) -> Vec<Value> {
        m.store
            .tail_events(id, 100)
            .unwrap()
            .into_iter()
            .filter_map(|e| match e.event {
                crate::event::ThreadEvent::Custom { event_type, data }
                    if event_type == crate::shapes::summary::SUMMARY_EVENT_TYPE =>
                {
                    Some(data)
                }
                _ => None,
            })
            .collect()
    }

    /// **A THREAD CREATED WITH A NAME IS NAMED FROM BIRTH; A BLANK OR ABSENT
    /// NAME WRITES NOTHING** (TUI-P1a, checklist TC-01). N = 2 named threads,
    /// each carrying its own name, trimmed and marked `manual`; opposed by a
    /// blank name and no name, which must leave the thread unnamed (so a
    /// constant "always write a name" fails, and so does "never").
    ///
    /// Forcings (report): delete the `name` block in the `threads/create` arm →
    /// the named threads' assertions are red; drop the blank filter → the blank
    /// thread's assertion is red.
    #[tokio::test]
    async fn a_thread_created_with_a_name_is_named_from_birth_and_a_blank_or_absent_one_is_not() {
        // CON-28: the manager under test reads slot credentials. In a FULL lib
        // run another test (`accounts.rs`, the slot-listing test) happens to
        // install this same fake first; run alone, nothing does and the
        // tripwire aborts the process. Installing the identical fake here makes
        // this test stand on its own (TUI-P1a report §7).
        crate::accounts::set_keychain_reader_for_test(Some(std::sync::Arc::new(|_: &str, _: &str| {
            crate::accounts::CredentialFacts::default()
        })));
        let tmp = tempfile::tempdir().unwrap();
        let (m, _f) = tracked_threads(tmp.path()).await;
        let settings = serde_json::to_value(a_thread(tmp.path())).unwrap();
        let mut named = Vec::new();
        for name in ["  Answer the tax letter  ", "Plan the trip"] {
            let out = route_request(
                &m,
                "threads/create",
                json!({"settings": settings.clone(), "name": name}),
            )
            .await
            .expect("create");
            named.push((out["thread_id"].as_str().unwrap().to_string(), name.trim().to_string()));
        }
        for (id, want) in &named {
            let got = names_of(&m, id);
            assert_eq!(got.len(), 1, "one name written for {id}: {got:?}");
            assert_eq!(got[0]["summary"], json!(want), "{got:?}");
            assert_eq!(got[0]["manual"], json!(true), "a name given at creation is a person's: {got:?}");
        }
        for params in [json!({"settings": settings.clone(), "name": "   "}), json!({"settings": settings.clone()})] {
            let out = route_request(&m, "threads/create", params.clone()).await.expect("create");
            let id = out["thread_id"].as_str().unwrap();
            assert!(names_of(&m, id).is_empty(), "{params} named the thread: {:?}", names_of(&m, id));
        }
    }

    /// **`threads/{thread_id}/event` ANSWERS AN ACKNOWLEDGEMENT, NOT THE
    /// POSITION.**
    ///
    /// It answered `{thread_id, message_id}`. `message_id` is the allocated
    /// addressable position — `ThreadCells::last_message_id`, folded out of the
    /// very ledger event this call appends. Two halves, and only together do they
    /// mean the rule holds: there IS a promise id, and there is NO position
    /// anywhere in the reply.
    ///
    /// **Forced red** by deleting the `self.emit_resolving(..)` call from
    /// `ThreadManager::append_custom`'s tracking arm:
    /// `the emit is what resolves it: [(PromiseId(2), "threads/{thread_id}/event", Some("<id>"))]`,
    /// and `the id has to reach the consumer that was waiting: []`.
    #[tokio::test]
    async fn threads_event_answers_an_acknowledgement_and_not_the_position() {
        let tmp = tempfile::tempdir().unwrap();
        let (m, mut f) = tracked_threads(tmp.path()).await;
        let id = m.create_thread(None, a_thread(tmp.path())).unwrap();
        hold_key(&m, &id).await;
        let _ = drained(&mut f);

        let out = route_request(
            &m,
            &format!("threads/{id}/event"),
            json!({"event_type": "summary_updated", "data": {"summary": "hi"},
                   "addressable": true}),
        )
        .await
        .expect("event");

        assert!(out["promise_id"].is_u64(), "{out}");
        assert_eq!(out["visible"], json!(true), "{out}");
        assert_eq!(out["changed"], json!(true), "an id WAS allocated: {out}");
        assert!(
            out.get("message_id").is_none() && out.get("thread_id").is_none(),
            "the position arrives through the shape, never on the reply: {out}"
        );
        assert!(
            m.promises.is_empty(),
            "the emit is what resolves it: {:?}",
            m.promises.open_ids()
        );
        // AND THE ID REACHED THE CONSUMER. See `ids_for`.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let carried = ids_for(&drained(&mut f), &id);
        assert!(
            carried.contains(&out["promise_id"].as_u64().unwrap()),
            "the id has to reach the consumer that was waiting: {carried:?}"
        );
    }

    /// **`threads/{thread_id}/seen` ANSWERS AN ACKNOWLEDGEMENT, NOT THE STATUS.**
    ///
    /// It answered the whole fresh status, justified as *"so a client never
    /// needs a second call"* — and `unread` and `seen_seq` are two cells the
    /// shape folds from the `Fact::ThreadSeen` this very call emits. The second
    /// call it saved is the one the shape exists to remove.
    ///
    /// **Forced red** by deleting the `self.hub.emit_fact_resolving(..)` call
    /// from `ThreadManager::mark_seen`:
    /// `the emit is what resolves it: [(PromiseId(2), "threads/{thread_id}/seen", Some("<id>"))]`,
    /// and `the id has to reach the consumer that was waiting: []`.
    #[tokio::test]
    async fn threads_seen_answers_an_acknowledgement_and_not_the_status() {
        let tmp = tempfile::tempdir().unwrap();
        let (m, mut f) = tracked_threads(tmp.path()).await;
        let id = m.create_thread(None, a_thread(tmp.path())).unwrap();
        m.append_custom(&id, "summary_updated".into(), json!({"summary": "x"}), true)
            .expect("an addressable event to have been read");
        hold_key(&m, &id).await;
        let _ = drained(&mut f);

        let out = route_request(&m, &format!("threads/{id}/seen"), json!({ "seq": 1 }))
            .await
            .expect("seen");

        assert!(out["promise_id"].is_u64(), "{out}");
        assert_eq!(out["visible"], json!(true), "{out}");
        assert_eq!(out["changed"], json!(true), "the cursor MOVED: {out}");
        for gone in ["unread", "seen_seq", "state", "executing", "queued"] {
            assert!(
                out.get(gone).is_none(),
                "`{gone}` is a cell the shape carries, not a reply field: {out}"
            );
        }
        assert!(
            m.promises.is_empty(),
            "the emit is what resolves it: {:?}",
            m.promises.open_ids()
        );
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let carried = ids_for(&drained(&mut f), &id);
        assert!(
            carried.contains(&out["promise_id"].as_u64().unwrap()),
            "the id has to reach the consumer that was waiting: {carried:?}"
        );

        // And a re-assertion says it is one, rather than reporting a move.
        let again = route_request(&m, &format!("threads/{id}/seen"), json!({ "seq": 1 }))
            .await
            .expect("seen again");
        assert_eq!(
            again["changed"],
            json!(false),
            "the cursor is monotonic: asking for a position already passed moved nothing: {again}"
        );
        assert_eq!(
            again["visible"],
            json!(true),
            "and it STILL emits — a no-op that emits nothing cannot be told from one that \
             never ran: {again}"
        );
    }

    /// **`threads/{thread_id}/update` ANSWERS AN ACKNOWLEDGEMENT, NOT THE
    /// SETTINGS.**
    ///
    /// It answered `{thread_id, applied}` — the same blob `Fact::ThreadSettings`
    /// carries and `ThreadCells::settings` holds, byte-identically by
    /// construction.
    ///
    /// **Forced red** by deleting the `self.hub.core().emit_resolving(..)` call
    /// carrying `Fact::ThreadSettings` from `ThreadManager::emit_thread_facts`:
    /// `the id has to reach the consumer that was waiting: []`.
    ///
    /// **THIS TEST WAS REWRITTEN BECAUSE A MUTATION CAME BACK GREEN.** Its first
    /// version asserted only that the book was empty, and a mutation that kept
    /// the `resolve` while removing the emit around it passed — which is the
    /// precise failure `docs/shapes.md` 2.4 records as the reason the resolve
    /// must be an ARGUMENT of the emit: *"separated, a deleted emit still empties
    /// the book and the liveness test proves nothing."* An empty book says the
    /// resolve RAN. Only a frame says the consumer was told. Both are asserted
    /// now, and the same repair was applied to the two tests either side of this
    /// one.
    #[tokio::test]
    async fn threads_update_answers_an_acknowledgement_and_not_the_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let (m, mut f) = tracked_threads(tmp.path()).await;
        let id = m.create_thread(None, a_thread(tmp.path())).unwrap();
        hold_key(&m, &id).await;
        let _ = drained(&mut f);

        let out = route_request(
            &m,
            &format!("threads/{id}/update"),
            json!({"patch": {"model": "opus"}}),
        )
        .await
        .expect("update");

        assert!(out["promise_id"].is_u64(), "{out}");
        assert_eq!(out["visible"], json!(true), "{out}");
        assert_eq!(out["changed"], json!(true), "{out}");
        assert!(
            out.get("applied").is_none() && out.get("settings").is_none(),
            "the blob arrives through the shape, never on the reply: {out}"
        );
        assert!(
            m.promises.is_empty(),
            "the emit is what resolves it: {:?}",
            m.promises.open_ids()
        );
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let carried = ids_for(&drained(&mut f), &id);
        assert!(
            carried.contains(&out["promise_id"].as_u64().unwrap()),
            "the id has to reach the consumer that was waiting: {carried:?}"
        );

        // An EMPTY patch is a real outcome and says so.
        let nothing = route_request(&m, &format!("threads/{id}/update"), json!({"patch": {}}))
            .await
            .expect("empty update");
        assert_eq!(
            nothing["changed"],
            json!(false),
            "a patch that says nothing moved nothing: {nothing}"
        );
    }

    /// **`threads/create` ANSWERS AN ACKNOWLEDGEMENT AND THE ADDRESS IT MINTED,
    /// AND ITS PROMISE RIDES THE POOL'S MEMBERSHIP FRAME.**
    ///
    /// The awkward one, and the two halves are separate claims:
    ///
    /// 1. `message_id` is gone from the reply — the same violation as
    ///    `threads/{id}/event`. `thread_id` STAYS, because it is the ADDRESS
    ///    this act minted and not a fact about state: the shape carries cells
    ///    UNDER that key and never carries the key as a value.
    /// 2. **A prior dispatch concluded this command could not be resolved at
    ///    all** — *"no consumer can hold this thread's key, so a fact emitted
    ///    here could never be folded by anything"*. Membership makes that false:
    ///    `Threads::folds` records `member(id)` before every `holds` guard, so
    ///    the `Push::Status` `create_thread` already emits yields one
    ///    `Derived::Added`, and the id rides it. This asserts the id is ON that
    ///    frame, from the consumer's side — which is the half the book being
    ///    empty cannot see.
    ///
    /// **Forced red** by deleting `let ids = self.pending_promises.take_key(&k);`
    /// from `hub.rs`'s `added` loop (passing `Resolving::none()` instead):
    /// `the added frame is the only frame this key will ever produce, and it must carry the id: []`.
    #[tokio::test]
    async fn threads_create_answers_an_acknowledgement_resolved_by_membership() {
        let tmp = tempfile::tempdir().unwrap();
        let (m, mut f) = tracked_threads(tmp.path()).await;
        let _ = drained(&mut f);

        let out = route_request(
            &m,
            "threads/create",
            json!({"settings": a_thread(tmp.path())}),
        )
        .await
        .expect("create");

        let id = out["thread_id"].as_str().expect("the minted address").to_string();
        let promise_id = out["promise_id"].as_u64().expect("an acknowledgement");
        assert_eq!(out["visible"], json!(true), "{out}");
        assert_eq!(out["changed"], json!(true), "{out}");
        assert!(
            out.get("message_id").is_none(),
            "the position of an inline first message is not a reply field: {out}"
        );
        assert!(
            m.promises.is_empty(),
            "the emit is what resolves it: {:?}",
            m.promises.open_ids()
        );

        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let carried: Vec<u64> = drained(&mut f)
            .iter()
            .filter(|fr| {
                fr.wire.key.as_deref() == Some(id.as_str())
                    && matches!(fr.wire.event, Some(crate::shape::Derived::Added))
            })
            .flat_map(|fr| fr.wire.promises.clone())
            .collect();
        assert!(
            carried.contains(&promise_id),
            "the added frame is the only frame this key will ever produce, and it must \
             carry the id: {carried:?}"
        );
    }

    /// **`budgets/{key}/set` ANSWERS AN ACKNOWLEDGEMENT THAT SAYS IT WILL NEVER
    /// BE `visible`.**
    ///
    /// It answered the budget SNAPSHOT. There is no fact for that to arrive as —
    /// **no shape on this producer carries a budget** — so this is `gauge.probe`'s
    /// position exactly: `accepted`, never `visible`, said so on the reply, and
    /// its own promise closed on the way out rather than left for `open_ids` to
    /// find. `visible: false` is the field that makes that difference legible;
    /// before it existed the reply here would have been byte-identical to one
    /// where a frame is coming.
    ///
    /// **Forced red** by deleting `manager.promises.abandon(promise.id());` from
    /// the arm:
    /// `a command with no resolver must close its own promise: [(PromiseId(1), "budgets/{key}/set", Some("tiny"))]`.
    #[tokio::test]
    async fn budgets_set_answers_an_acknowledgement_that_will_never_be_visible() {
        let tmp = tempfile::tempdir().unwrap();
        let (m, _f) = tracked_threads(tmp.path()).await;

        let out = route_request(
            &m,
            "budgets/tiny/set",
            json!({"session_pct": 10.0, "accounts": ["aa"]}),
        )
        .await
        .expect("set");

        assert!(out["promise_id"].is_u64(), "{out}");
        assert_eq!(
            out["visible"],
            json!(false),
            "no shape carries a budget, so no frame will ever carry this id: {out}"
        );
        assert_eq!(out["changed"], json!(true), "the key was minted: {out}");
        for gone in ["config", "spend", "meters", "key", "disabled"] {
            assert!(
                out.get(gone).is_none(),
                "`{gone}` is state, and a command never answers with state: {out}"
            );
        }
        assert!(
            m.promises.is_empty(),
            "a command with no resolver must close its own promise: {:?}",
            m.promises.open_ids()
        );

        // Re-sending the same config is a re-assertion and says so.
        let again = route_request(
            &m,
            "budgets/tiny/set",
            json!({"session_pct": 10.0, "accounts": ["aa"]}),
        )
        .await
        .expect("set again");
        assert_eq!(
            again["changed"],
            json!(false),
            "nothing moved the second time: {again}"
        );
    }

    /// **An external transcript a reconcile will PORT, and the guard that it
    /// ports into the test's own tempdir.** The destination store is the
    /// thread's account's config dir; on `aa` that is `<dir>/aa`. Were the thread
    /// ever placed on the machine login instead, the port would write into the
    /// operator's real `~/.claude`, so this refuses before writing anything.
    fn external_transcript(m: &crate::ThreadManager, dir: &Path, id: &str) -> std::path::PathBuf {
        let row = m.store.get_thread(id).unwrap().expect("the thread exists");
        assert_eq!(
            row.account.as_deref(),
            Some("aa"),
            "the port would land outside the tempdir — refusing before it writes"
        );
        let src_dir = dir.join("fallback-store/projects/-r2s5-reconcile");
        std::fs::create_dir_all(&src_dir).unwrap();
        let src = src_dir.join(format!("{id}.jsonl"));
        let body: String = (0..4)
            .map(|i| json!({"sessionId": id, "seq": i}).to_string() + "\n")
            .collect();
        std::fs::write(&src, body).unwrap();
        src
    }

    /// **`threads/{thread_id}/queue_cancel` ANSWERS AN ACKNOWLEDGEMENT, NOT
    /// `cancelled`** (R2.S5).
    ///
    /// It answered `{thread_id, message_id, cancelled}`. Two of those were the
    /// caller's own params read back; `cancelled` is whether the call moved
    /// anything, which is `changed`. The queue is `ThreadCells::queue`, re-read
    /// after the write — `Fact::ThreadQueue` — and the id rides that row frame.
    #[tokio::test]
    async fn queue_cancel_answers_an_acknowledgement_and_not_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let (m, mut f) = tracked_threads(tmp.path()).await;
        let id = m.create_thread(None, a_thread(tmp.path())).unwrap();
        // A WAITING row, put there by the store directly so nothing dispatches it.
        m.store
            .enqueue(&id, 5, "hold this", "turn_end", &Value::Null, &[])
            .unwrap();
        hold_key(&m, &id).await;
        let _ = drained(&mut f);

        let out = route_request(
            &m,
            &format!("threads/{id}/queue_cancel"),
            json!({ "message_id": 5 }),
        )
        .await
        .expect("cancel");

        assert!(out["promise_id"].is_u64(), "POSITIVE SENTINEL: a command reply: {out}");
        assert_eq!(out["visible"], json!(true), "{out}");
        assert_eq!(out["changed"], json!(true), "a waiting row WAS cancelled: {out}");
        for gone in ["thread_id", "message_id", "cancelled"] {
            assert!(
                out.get(gone).is_none(),
                "`{gone}` is not a reply field — the queue is a cell: {out}"
            );
        }
        assert!(
            m.promises.is_empty(),
            "the emit is what resolves it: {:?}",
            m.promises.open_ids()
        );
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let carried = ids_for(&drained(&mut f), &id);
        assert!(
            carried.contains(&out["promise_id"].as_u64().unwrap()),
            "the id has to reach the consumer that was waiting: {carried:?}"
        );

        // A cancel of a row that is no longer waiting moved nothing, says so,
        // and still emits — a no-op that emits nothing cannot be told from one
        // that never ran.
        let again = route_request(
            &m,
            &format!("threads/{id}/queue_cancel"),
            json!({ "message_id": 5 }),
        )
        .await
        .expect("cancel again");
        assert_eq!(again["changed"], json!(false), "nothing was waiting: {again}");
        assert_eq!(again["visible"], json!(true), "{again}");
        assert!(
            m.promises.is_empty(),
            "a no-op must still resolve: {:?}",
            m.promises.open_ids()
        );
    }

    /// **THE `answers` GATE, WITNESSED FROM BOTH SIDES.** `emit_status_answering`
    /// resolves a queue promise only when `answers` is true — the two queue
    /// commands' own emits — so an UNRELATED status edge on the same thread
    /// (a turn starting, a lock changing hands) cannot close a promise before
    /// the write it acknowledges is in the queue level. The inspector forced
    /// `answers || true` and nothing went red: the gate's one job had no test.
    /// Now it has both halves — an ordinary `emit_status` leaves the promise
    /// open, and the answering emit closes it — so neither "always resolve"
    /// nor "never resolve" can pass.
    #[tokio::test]
    async fn an_unrelated_status_edge_does_not_close_a_queue_promise() {
        let tmp = tempfile::tempdir().unwrap();
        let (m, mut f) = tracked_threads(tmp.path()).await;
        let id = m.create_thread(None, a_thread(tmp.path())).unwrap();
        hold_key(&m, &id).await;
        let _ = drained(&mut f);

        let p = m
            .promises
            .open::<crate::commands::ThreadsQueueCancel>(Some(id.clone()));
        let pid = p.id();
        assert!(!m.promises.is_empty(), "POSITIVE SENTINEL: a queue promise is open");

        // An unrelated status edge: the plain emit every turn transition uses.
        m.emit_status(&id);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let _ = drained(&mut f);
        assert!(
            m.promises.open_ids().iter().any(|(i, _, _)| *i == pid),
            "an unrelated status edge must NOT close a queue promise: {:?}",
            m.promises.open_ids()
        );

        // The answering emit — what the two queue commands do — closes it.
        let _ = m.emit_status_answering(&id, true);
        assert!(
            m.promises.is_empty(),
            "the answering emit is what resolves it: {:?}",
            m.promises.open_ids()
        );
    }

    /// **`threads/{thread_id}/queue_reselect` ANSWERS AN ACKNOWLEDGEMENT, NOT
    /// THE COUNT** (R2.S5).
    ///
    /// It answered `{thread_id, reselected: <count>}`. The rows it moved are on
    /// `ThreadCells::queue` with their `state` word; whether any moved is
    /// `changed`. The resolving queue fact rides the status edge the dispatcher
    /// nudge emits.
    #[tokio::test]
    async fn queue_reselect_answers_an_acknowledgement_and_not_the_count() {
        let tmp = tempfile::tempdir().unwrap();
        let (m, mut f) = tracked_threads_no_cli(tmp.path()).await;
        let id = m.create_thread(None, a_thread(tmp.path())).unwrap();
        // A FAILED row: delivered, then failed — the only way it happens.
        let row = m
            .store
            .enqueue(&id, 7, "that didn't get through", "turn_end", &Value::Null, &[])
            .unwrap();
        m.store.mark_delivered(&[row]).unwrap();
        assert_eq!(
            m.store.fail_deliveries(&id, &[7]).unwrap(),
            1,
            "POSITIVE SENTINEL: the fixture must actually fail a row"
        );
        hold_key(&m, &id).await;
        let _ = drained(&mut f);

        let out = route_request(&m, &format!("threads/{id}/queue_reselect"), json!({}))
            .await
            .expect("reselect");

        assert!(out["promise_id"].is_u64(), "POSITIVE SENTINEL: a command reply: {out}");
        assert_eq!(out["visible"], json!(true), "{out}");
        assert_eq!(out["changed"], json!(true), "a failed row WAS moved: {out}");
        for gone in ["thread_id", "reselected"] {
            assert!(
                out.get(gone).is_none(),
                "`{gone}` is not a reply field — the rows are on the cell: {out}"
            );
        }
        assert!(
            m.promises.is_empty(),
            "the emit is what resolves it: {:?}",
            m.promises.open_ids()
        );
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let carried = ids_for(&drained(&mut f), &id);
        assert!(
            carried.contains(&out["promise_id"].as_u64().unwrap()),
            "the id has to reach the consumer that was waiting: {carried:?}"
        );

        // Idempotent: nothing is `failed` for this id any more... unless the
        // dispatch failed it again, so ask for an id that never existed.
        let again = route_request(
            &m,
            &format!("threads/{id}/queue_reselect"),
            json!({ "message_ids": [999] }),
        )
        .await
        .expect("reselect again");
        assert_eq!(again["changed"], json!(false), "nothing moved: {again}");
        assert!(
            m.promises.is_empty(),
            "a no-op must still resolve: {:?}",
            m.promises.open_ids()
        );
    }

    /// **`threads/{thread_id}/reconcile_transcript` ANSWERS AN ACKNOWLEDGEMENT,
    /// NOT THE PORT OUTCOME** (R2.S5).
    ///
    /// It answered the whole outcome — account, source, destination, the three
    /// event counts. That is the payload of the `transcript_reconciled` ledger
    /// event it appends, which `ThreadCells::app` holds; the ledger append is the
    /// resolver, and the id rides the row frame it causes.
    #[tokio::test]
    async fn reconcile_transcript_answers_an_acknowledgement_and_not_the_outcome() {
        let tmp = tempfile::tempdir().unwrap();
        let (m, mut f) = tracked_threads(tmp.path()).await;
        let id = m.create_thread(None, a_thread(tmp.path())).unwrap();
        let src = external_transcript(&m, tmp.path(), &id);
        hold_key(&m, &id).await;
        let _ = drained(&mut f);

        let out = route_request(
            &m,
            &format!("threads/{id}/reconcile_transcript"),
            json!({ "path": src.to_string_lossy() }),
        )
        .await
        .expect("reconcile");

        assert!(out["promise_id"].is_u64(), "POSITIVE SENTINEL: a command reply: {out}");
        assert_eq!(out["visible"], json!(true), "{out}");
        assert_eq!(out["changed"], json!(true), "the transcript WAS ported: {out}");
        for gone in [
            "thread_id", "account", "source", "outcome", "reconciled", "destination",
            "destination_events", "source_events", "replaced_events",
        ] {
            assert!(
                out.get(gone).is_none(),
                "`{gone}` is the ledger event's payload, not a reply field: {out}"
            );
        }
        assert!(
            m.promises.is_empty(),
            "the emit is what resolves it: {:?}",
            m.promises.open_ids()
        );
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let carried = ids_for(&drained(&mut f), &id);
        assert!(
            carried.contains(&out["promise_id"].as_u64().unwrap()),
            "the id has to reach the consumer that was waiting: {carried:?}"
        );

        // Handing it back is `kept`: nothing moved, and it says so.
        let again = route_request(
            &m,
            &format!("threads/{id}/reconcile_transcript"),
            json!({ "path": src.to_string_lossy() }),
        )
        .await
        .expect("reconcile again");
        assert_eq!(again["changed"], json!(false), "{again}");

        // A refusal after the open closes the promise it opened.
        let refused = route_request(
            &m,
            &format!("threads/{id}/reconcile_transcript"),
            json!({ "path": tmp.path().join("nope.jsonl").to_string_lossy() }),
        )
        .await;
        assert!(refused.is_err(), "no file is a refusal");
        assert!(
            m.promises.is_empty(),
            "a promise never outlives the reply that refused it: {:?}",
            m.promises.open_ids()
        );
    }

    /// **`threads/{thread_id}/budget_assign` ANSWERS AN ACKNOWLEDGEMENT, NOT
    /// THE KEY — AND IT IS `visible`** (R2.S5).
    ///
    /// It answered `{thread_id, budget_key}`. The brief that converted it
    /// expected `budgets/{key}/set`'s `NeverVisible`; it is not that case. The
    /// key is a field of the thread's settings blob, `update_settings` emits
    /// `Fact::ThreadSettings`, and `ThreadCells::settings` holds it — so the id
    /// rides that row frame, which this asserts from the consumer's side.
    #[tokio::test]
    async fn budget_assign_answers_an_acknowledgement_resolved_by_the_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let (m, mut f) = tracked_threads(tmp.path()).await;
        let id = m.create_thread(None, a_thread(tmp.path())).unwrap();
        route_request(
            &m,
            "budgets/tiny/set",
            json!({"session_pct": 10.0, "accounts": ["aa"]}),
        )
        .await
        .expect("mint the key");
        hold_key(&m, &id).await;
        let _ = drained(&mut f);

        let out = route_request(
            &m,
            &format!("threads/{id}/budget_assign"),
            json!({ "key": "tiny" }),
        )
        .await
        .expect("assign");

        assert!(out["promise_id"].is_u64(), "POSITIVE SENTINEL: a command reply: {out}");
        assert_eq!(out["visible"], json!(true), "the settings fact carries it: {out}");
        assert_eq!(out["changed"], json!(true), "the key moved: {out}");
        for gone in ["thread_id", "budget_key"] {
            assert!(
                out.get(gone).is_none(),
                "`{gone}` is in the settings cell, not a reply field: {out}"
            );
        }
        assert!(
            m.promises.is_empty(),
            "the emit is what resolves it: {:?}",
            m.promises.open_ids()
        );
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let carried = ids_for(&drained(&mut f), &id);
        assert!(
            carried.contains(&out["promise_id"].as_u64().unwrap()),
            "the id has to reach the consumer that was waiting: {carried:?}"
        );

        let again = route_request(
            &m,
            &format!("threads/{id}/budget_assign"),
            json!({ "key": "tiny" }),
        )
        .await
        .expect("assign again");
        assert_eq!(
            again["changed"],
            json!(false),
            "re-pointing at the key it already has is a re-assertion: {again}"
        );
        assert!(m.promises.is_empty(), "{:?}", m.promises.open_ids());
    }

    /// **THE LIVENESS PROPERTY — EVERY COMMAND, THROUGH A REAL PRODUCER, LEAVES
    /// NO PROMISE OPEN** (`docs/shapes.md` 2.4).
    ///
    /// Types prove WHICH event resolves a command. No type can prove the emit
    /// HAPPENS. This is the suite-wide property the doc asks for: run every
    /// typed command through a producer whose shapes are really tracking, drain,
    /// and assert the book is empty. A handler that forgot to emit shows up here
    /// and nowhere else.
    ///
    /// **The harness PANICS rather than proceed if tracking never started.** That
    /// is the hole the original forced-red pass exposed: without a subscribe
    /// every command takes its not-tracking early return, which is the one path
    /// where a promise closes with no emit — so the book would be empty, every
    /// assertion green, and the test would be asserting against code that did
    /// not run.
    ///
    /// **Forced red** by deleting the `self.hub.emit_fact_resolving(..)` call
    /// from `ThreadManager::emit_disabled_fact`:
    /// `no command may leave a promise open: [(PromiseId(4), "accounts/{account}/disable", Some("aa"))]`.
    #[tokio::test]
    async fn every_command_through_a_real_producer_leaves_no_promise_open() {
        let tmp = tempfile::tempdir().unwrap();
        let (m, _f) = tracked_threads(tmp.path()).await;
        assert!(
            m.hub.any_tracking() && m.hub.tracked("threads"),
            "the harness must refuse to run against an untracked producer — every command \
             would take its not-tracking early return and the assertion would be vacuous"
        );

        let created = route_request(
            &m,
            "threads/create",
            json!({"settings": a_thread(tmp.path())}),
        )
        .await
        .expect("create");
        let id = created["thread_id"].as_str().unwrap().to_string();

        // EVERY typed command on this producer, in one pass. THIS LIST IS
        // HAND-MAINTAINED and nothing ties it to `crate::commands`: a command
        // added there and left off here passes silently (an inspector read the
        // old comment's "count is asserted below" and found no such assertion;
        // there is no runtime registry of `impl Command` to assert against).
        // When you add a command, add it here in the same commit.
        let calls: Vec<(String, Value)> = vec![
            (
                format!("threads/{id}/event"),
                json!({"event_type": "summary_updated", "data": {"summary": "s"},
                       "addressable": true}),
            ),
            (format!("threads/{id}/seen"), json!({ "seq": 1 })),
            (
                format!("threads/{id}/update"),
                json!({"patch": {"model": "opus"}}),
            ),
            (
                "budgets/tiny/set".into(),
                json!({"session_pct": 10.0, "accounts": ["aa"]}),
            ),
            ("accounts/aa/pool_share_set".into(), json!({"share": 0.4})),
            ("accounts/aa/disable".into(), json!({})),
            ("accounts/aa/enable".into(), json!({})),
            // The no-op case: `docs/shapes.md` 2.4 requires a command whose
            // effect is nothing to emit its resolving fact anyway, or the test
            // cannot tell "resolved by re-assertion" from "never emitted".
            ("accounts/aa/enable".into(), json!({})),
            ("gauge/probe".into(), json!({})),
            // R2.S5's four. The two queue writes in their no-op form (nothing
            // queued here, and a re-select would dispatch); the reconcile with a
            // real source so its resolving path is the one that runs.
            (format!("threads/{id}/queue_cancel"), json!({ "message_id": 999 })),
            (format!("threads/{id}/queue_reselect"), json!({})),
            (
                format!("threads/{id}/reconcile_transcript"),
                json!({ "path": external_transcript(&m, tmp.path(), &id).to_string_lossy() }),
            ),
            (format!("threads/{id}/budget_assign"), json!({ "key": "tiny" })),
        ];
        for (path, params) in &calls {
            // `gauge/probe` refuses on a machine with no claude binary, and a
            // refusal is a legitimate outcome the rule still binds: *a promise
            // never outlives the reply that refused it*.
            let _ = route_request(&m, path, params.clone()).await;
            assert!(
                m.promises.is_empty(),
                "no command may leave a promise open: `{path}` left {:?}",
                m.promises.open_ids()
            );
        }

        assert!(
            m.promises.is_empty(),
            "no command may leave a promise open: {:?}",
            m.promises.open_ids()
        );
    }
}
