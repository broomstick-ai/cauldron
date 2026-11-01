//! **THE OWNER BINDING — one pure function, and the trap it exists to stop.**
//!
//! `0060` ruled the sheet is the OWNER'S CONSOLE: *"every act is bound,
//! statically and forever, to the one process that owns the state it changes,
//! and the face NEVER relays one owner's act through the other."* This module
//! is that binding written as code rather than as a paragraph, and the reason
//! it is a module of its own — rather than six arms inside `app.rs` — is that a
//! binding nobody can test is a binding that drifts.
//!
//! [`route`] is PURE. It takes an act and the thread it is about, and returns
//! **the bytes that would leave this process** plus which socket they leave by.
//! No `tokio`, no socket, no `Handle`. So every proof in this arc's task —
//! *"assert the exact bytes sent to the right socket"* — is a unit test with
//! nothing live anywhere near it, and the forced failure is one line: point an
//! act at the wrong owner and a NAMED test goes red.
//!
//! # ⚠ THE MODEL TRAP ⚠
//!
//! **`threads/{id}/update{patch:{model}}` EXISTS on the daemon and it is a TRAP.**
//! It is the one act that looks daemon-native and is not, and it is the one a
//! later worker would most plausibly "simplify" onto the daemon verb to drop a
//! dependency. Do not.
//!
//! The AUI declares `model` a field it is AUTHORITATIVE for
//! (`apps/aui/src/threads_client.rs:2159-2178` — *"the settings fields AUI is
//! AUTHORITATIVE for … the model"*), and its fingerprint sync patches any
//! drifted field onto the thread on its NEXT TURN. So a model set through 3651
//! survives exactly until his next utterance on that thread and then silently
//! reverts — **the same silent-revert class as writing the index file, one
//! layer down.** It would be a stub that pretends to work, which the stub
//! doctrine forbids by name.
//!
//! The model act therefore goes to 9010 as `Mutation::SetThreadModel`, and
//! [`tests::the_model_act_goes_to_the_aui_and_the_daemons_verb_is_never_spoken`]
//! plus [`tests::this_module_never_names_the_daemons_trap_verb`] are what keep
//! that true without anyone having to remember this paragraph.
//!
//! # The other asymmetry, and it runs the other way
//!
//! `Mutation::InterruptThread` and `Mutation::CompactThread` also exist on
//! 9010, and they are ALSO not used. `InterruptThread` is a five-line
//! pass-through to `threads/{id}/cancel` (`apps/aui/src/v2/mutate.rs:254-263`) and
//! `CompactThread` ends in the same `threads/{id}/send` this module builds
//! (`apps/aui/src/dispatch/thread.rs:3435-3438`). Relaying either through the
//! AUI would put a process between this face and the owner — the relay `DIR-7`'s
//! resolution removed — and would make an interrupt fail when the AUI is down,
//! which is the moment an interrupt is most wanted.
//!
//! **So both sockets carry acts the other socket also offers, and in both
//! directions the offer is declined.** That is the whole design in one
//! sentence, and it is why this file is a table rather than a fallback chain.

use std::path::Path;

use serde_json::{json, Value};
use uuid::Uuid;

use aui_wire::server::Mutation;

use aui_wire::Model;

/// The thread an act is about, in the terms the INDEX holds it.
///
/// `project` is the `~/minds/<name>` basename the row was read from — it is on
/// no header, so the row has to carry it (`ThreadRow::project`, 0053), and it
/// is half the key every AUI mutation is addressed by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadRef {
    pub project: String,
    /// The id as the index spells it. Parsed to a `Uuid` only where a mutation
    /// demands one, so a legacy non-uuid id fails LOUDLY on that act instead of
    /// failing the whole sheet.
    pub id: String,
    pub turn_count: u64,
    pub pinned: bool,
    pub closed: bool,
}

/// **The six acts the thread sheet draws**, in the design doc's order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadAct {
    /// `None` clears the override back to the project/global default.
    SetModel(Option<Model>),
    Interrupt,
    Compact {
        instructions: Option<String>,
    },
    Rename {
        summary: String,
    },
    SetPinned(bool),
    Move(Dir),
    SetClosed(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
}

/// **WHICH SOCKET.** There is no third value and no "either".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    /// The AUI's proto-v2 listener on 9010.
    Aui,
    /// The threads daemon on 3651.
    Daemon,
}

/// **THE BYTES THAT LEAVE THIS PROCESS**, before any socket exists.
#[derive(Debug, Clone, PartialEq)]
pub enum Wire {
    /// A `Mutation`, to be wrapped in `ClientMsg::Mutate` by [`crate::aui`].
    Aui(Mutation),
    /// An RPC, to be issued through [`crate::transport::Handle::call`].
    ///
    /// **`method` IS A `String`, NOT A `&'static str`, AND THAT IS FORCED.**
    /// Since the daemon re-pathed its surface (2026-09-28) a per-thread method
    /// name CONTAINS the thread id — `threads/{thread_id}/cancel` — so it is
    /// built per act and cannot be a literal with a `'static` lifetime. The
    /// field used to be `&'static str` and the type was the whole structural
    /// cost of that migration.
    Daemon(Verb),
}

/// **EVERY WORD THIS FACE CAN SAY TO THE THREADS DAEMON, AS A CLOSED ENUM.**
///
/// What stood here was `Daemon { method: String, params: Value }`, and the
/// comment above it explained — correctly — that `method` could not be a
/// `&'static str` because a per-thread path contains the thread id. Both the
/// field and that argument are gone, because the question changed: it is no
/// longer *how do I spell the method* but *which of the three things I am
/// allowed to say is this*.
///
/// A `String` method and a `Value` of params is a door wide enough for a READ,
/// and `src/methods.rs` existed to make the set of names that went through it
/// greppable. The names are not the surface any more:
/// `seam::commands::methods::TEMPLATES` is, `seamdiff --inventory` routes it
/// against the producer's own fifty-two, and a verb this face does not have a
/// variant for cannot be constructed at all. `methods.rs` is deleted.
///
/// **THREE VARIANTS, AND THE SEAM OFFERS SIX.** `update`, `seen` and `event`
/// are vocabulary this face does not use — divergence D4 in
/// `docs/consumer-seam.md` §5, dissolved on the ground that a verb a face never
/// calls is vocabulary and not policy. `seen` in particular is deliberately
/// never sent: advancing his unread cursor as a side effect of RENDERING a
/// thread would be a write he never asked for, and the badge it moves is one he
/// reads.
#[derive(Debug, Clone, PartialEq)]
pub enum Verb {
    /// `threads/{id}/send` — the one message-bearing command, and the carrier
    /// for `/compact`. Its reply is the one that still answers a real
    /// `accepted` rather than a bare acknowledgement.
    Send {
        thread: String,
        text: String,
        intent: Option<String>,
        bare: bool,
        headers: Vec<Value>,
        metadata: Value,
    },
    /// `threads/{id}/cancel` — interrupt the running turn. No parameters at
    /// all: the subject is the whole of the ask.
    Cancel { thread: String },
    /// `threads/create` — the only act whose subject does not exist yet, which
    /// is why the id is a PARAMETER and not a path segment. `name` is the name
    /// he gave it in the new-thread modal (TUI-P1a, TC-01), written by the
    /// daemon as the thread's first `summary_updated`; `None` leaves it
    /// unnamed.
    Create {
        thread: String,
        settings: Value,
        name: Option<String>,
    },
    /// **`threads/{id}/event` `summary_updated`** — NAME the thread (TUI-P1a,
    /// TC-03). The rename that used to go to the AUI's index over 9010, which
    /// never landed for a thread this face created (it has no index header),
    /// made the AUI SPEAK (TC-38), and only showed after an index re-read. The
    /// daemon's ledger is the one source of a thread's name: every face holding
    /// the thread sees the `app` cell move.
    Name { thread: String, name: String },
}

impl Verb {
    /// The verb's own name, for a log line and for the act receipt. Not a
    /// method path: nothing dispatches on this and nothing may start.
    pub fn name(&self) -> &'static str {
        match self {
            Verb::Send { .. } => "send",
            Verb::Cancel { .. } => "cancel",
            Verb::Create { .. } => "create",
            Verb::Name { .. } => "name",
        }
    }

    /// The thread this verb acts on.
    pub fn thread(&self) -> &str {
        match self {
            Verb::Send { thread, .. }
            | Verb::Cancel { thread }
            | Verb::Create { thread, .. }
            | Verb::Name { thread, .. } => thread,
        }
    }
}

/// **THE ACCOUNT VERBS** (V1.6, Config › Accounts): the machine-wide in-pool
/// switch and the daemon's piped login — five of the seam's methods, said
/// through the face's ONE connection ([`crate::mirror::Handle::account`]),
/// never through a second socket. A type of their own, apart from [`Verb`],
/// because none of them names a thread and every one of them is said only from
/// Config, never routed from a keypress on a thread.
///
/// `Complete` is the one variant that carries a secret — the code he pasted —
/// and its `Debug` says so instead of printing it.
#[derive(Clone, PartialEq)]
pub enum AccountVerb {
    /// `accounts/login/begin` for `account` (a new slot's name, or an
    /// existing account's key).
    LoginBegin { account: String },
    /// `accounts/login/{session_id}/complete` with the pasted code.
    LoginComplete { session_id: String, code: String },
    /// `accounts/login/{session_id}/cancel`.
    LoginCancel { session_id: String },
    /// `accounts/{account}/enable` (`on`) or `/disable`.
    InPool { account: String, on: bool },
}

impl std::fmt::Debug for AccountVerb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AccountVerb::LoginBegin { .. } => write!(f, "LoginBegin"),
            AccountVerb::LoginComplete { .. } => write!(f, "LoginComplete {{ code: <redacted> }}"),
            AccountVerb::LoginCancel { .. } => write!(f, "LoginCancel"),
            AccountVerb::InPool { on, .. } => write!(f, "InPool {{ on: {on} }}"),
        }
    }
}

impl Wire {
    pub fn owner(&self) -> Owner {
        match self {
            Wire::Aui(_) => Owner::Aui,
            Wire::Daemon { .. } => Owner::Daemon,
        }
    }
}

/// **The one refusal a route may make**, and it is never "the socket is down"
/// — that is [`crate::aui::Reach`]'s business and it is drawn on the row.
/// These are refusals about the ACT: it cannot be expressed.
pub type Refusal = String;

/// **THE TABLE.** One act in, one wire out, and the arms are the design's rows.
///
/// The `pinned_order` argument is the DISPLAYED pinned order — the ids of the
/// pinned band exactly as the face drew them, top to bottom. It is the only
/// input `Move` needs, and taking it here rather than recomputing it is what
/// keeps `0053`'s recorded divergence intact: **the face never renumbers pins
/// itself.** It reads an order and names a neighbour; the server renumbers.
pub fn route(act: &ThreadAct, t: &ThreadRef, pinned_order: &[String]) -> Result<Wire, Refusal> {
    match act {
        // ── 9010, and this is THE TRAP ROW ────────────────────────────────
        ThreadAct::SetModel(model) => Ok(Wire::Aui(Mutation::SetThreadModel {
            project: t.project.clone(),
            thread_id: uuid_of(t)?,
            model: *model,
        })),

        // ── 3651, direct, and it must work when the AUI is down ───────────
        //
        // The id rides the PATH and is NOT repeated in the params — the daemon
        // refuses a call that spells its subject twice. `params` is therefore
        // empty, which is the whole request.
        ThreadAct::Interrupt => Ok(Wire::Daemon(Verb::Cancel {
            thread: t.id.clone(),
        })),

        // ── 3651, direct, byte-for-byte what the AUI itself sends ─────────
        ThreadAct::Compact { instructions } => {
            // The AUI's own guard, at the AUI's own threshold: compaction
            // resumes a session, and a thread with no turns has none to resume
            // (`apps/aui/src/dispatch/mod.rs:2286-2300`). Its wording is kept
            // because the two faces should refuse the same act the same way.
            if t.turn_count == 0 {
                return Err("there's nothing to compact yet — this thread has no turns".into());
            }
            Ok(Wire::Daemon(compact_verb(
                &t.id,
                instructions.as_deref(),
            )))
        }

        // ── 9010: the one writer that applies the manual-summary lock ─────
        //
        // An EMPTY summary is deliberately NOT refused here. The server refuses
        // it with a sentence written for a human ("an empty summary would erase
        // the thread's name") and `0060` rules that sentence is relayed
        // verbatim. A second refusal here would be this face paraphrasing the
        // only party that knows the rule.
        //
        // **NOT 9010 ANY MORE** (TUI-P1a, TC-03/TC-28/TC-38): the daemon. The
        // empty name is refused HERE now, because the daemon has no rule about
        // it and an empty `summary_updated` would read as "no name".
        ThreadAct::Rename { summary } => {
            let name = summary.trim();
            if name.is_empty() {
                return Err("the name is empty — an empty name would erase the thread's name".into());
            }
            Ok(Wire::Daemon(Verb::Name {
                thread: t.id.clone(),
                name: name.to_string(),
            }))
        }

        // ── 9010: the server assigns pin_order; this face computes none ───
        ThreadAct::SetPinned(pinned) => Ok(Wire::Aui(Mutation::SetThreadPinned {
            project: t.project.clone(),
            thread_id: uuid_of(t)?,
            pinned: *pinned,
        })),

        ThreadAct::Move(dir) => {
            let before = move_before(pinned_order, &t.id, *dir)?;
            Ok(Wire::Aui(Mutation::MovePin {
                project: t.project.clone(),
                thread_id: uuid_of(t)?,
                before: before.as_deref().map(parse_uuid).transpose()?,
            }))
        }

        // ── 9010 only: the daemon REFUSED this concept on purpose ─────────
        ThreadAct::SetClosed(closed) => Ok(Wire::Aui(Mutation::SetThreadClosed {
            project: t.project.clone(),
            thread_id: uuid_of(t)?,
            closed: *closed,
        })),
    }
}

// ────────────────────────────────────────────────────────────────────────────
//  THE ACT WHOSE SUBJECT DOES NOT EXIST YET — `n` at the root
// ────────────────────────────────────────────────────────────────────────────

/// **A NEW THREAD, AND IT IS THE DAEMON'S ACT — there is no other owner to
/// offer it to.**
///
/// `0060`'s owner binding is *"every act is bound, statically and forever, to
/// the one process that owns the state it changes."* **Whether a thread EXISTS
/// is the daemon's state**, and this is the one row of the whole table where
/// the question has no second answer: `libs/aui-state/src/server/mod.rs` has
/// no `CreateThread` in its mutation vocabulary — `SubmitTurn`,
/// `SetThreadPinned`, `MovePin` and twenty-odd more, and nothing that mints a
/// thread — so there is nothing on 9010 to relay this through. It is not a
/// declined offer like [`ThreadAct::Interrupt`]'s; it is an absence.
///
/// # WHAT THIS DOES **NOT** DO, AND HE HAS TO BE TOLD
///
/// It does not put the thread on his LIST. This face's list is the union of
/// every root's own `.mind/threads.json` ([`crate::index`]'s predicate), and a
/// thread born here has a header in none of them — `apps/aui`'s ledger fold
/// *"never INSERTS a header (an unknown thread id is skipped and counted, not
/// created)"* (`apps/aui/src/dispatch/ledger_fold.rs`), and the only two
/// producers of a `ThreadHeader` in the tree are both inside the AUI's own desk
/// loop. So **membership in `miscellaneous` is a HEADER, not the absence of
/// one**, and this face does not write that file. [`crate::app::App::new_thread`]
/// says so in the notice line rather than letting the empty list say it for him.
///
/// # The id is MINTED HERE, which is the AUI's own convention
///
/// `thread_id` is an OPTIONAL param on `threads/create` and the daemon adopts
/// it verbatim; the AUI mints client-side too (`libs/aui-nav/src/threads.rs`),
/// which is why its headers and the daemon's rows share one key and need no
/// translation. Minting here means the face can open the thread off the ACK
/// without reading the reply — and it means that if a header is ever written
/// for this thread, by any path, it joins up exactly.
///
/// # The settings are the TWO REQUIRED FIELDS and nothing else
///
/// `cwd` and `model` are `ThreadSettings`'s only non-defaulted fields
/// (`substrate-interface-layer/lib/threads/src/settings.rs`). The AUI's
/// `build_thread_settings` sends nine more — its role prompt, every registered
/// project as an `--add-dir`, three tiers of plugin root, `inject_context`,
/// `app_id: "aui"` — and **those are the AUI'S IDENTITY, which it declares it
/// is authoritative for and re-patches onto a thread on every turn
/// (`AUI_OWNED_SETTINGS`)**. Copying that list here would be a second
/// derivation of another process's identity, drifting the moment either moves,
/// and it would make this face claim to BE the AUI. So it sends the minimum
/// that creates a thread, says `app_id` is the TUI, and nothing more.
pub fn route_new_thread(id: &Uuid, cwd: &Path, model: Model, name: Option<&str>) -> Wire {
    Wire::Daemon(Verb::Create {
        thread: id.to_string(),
        // **THE NAME HE TYPED IN THE MODAL** (TUI-P1a, TC-01), trimmed; blank
        // is no name, and the thread is called by its first message instead.
        name: name.map(str::trim).filter(|n| !n.is_empty()).map(str::to_string),
        // The id this face minted rides `Verb::Create`'s `thread` field, and
        // `seam::commands::create` is the ONE place it becomes a `thread_id`
        // parameter — because the act that mints a key is subjectless under its
        // family, so there is no path segment for it to be spelled twice in and
        // nothing to be refused for.
        settings: json!({
            "cwd": cwd.display().to_string(),
            "model": model_alias(model),
            // **WHICH LOGIN IT RUNS ON IS THE DAEMON'S, ALWAYS** (DIR-24),
            // and `auto` is the daemon's own default — sent explicitly
            // because the AUI sends it explicitly, and a face that relied
            // on a default here would go silently wrong the day the
            // default moved.
            "account": "auto",
            // **NOT `aui`.** `app_id` is what the daemon attributes a
            // declared header to, and this face is not that process.
            "app_id": "harness-tui",
        }),
    })
}

/// The daemon's model alias, DERIVED from the one label rather than tabled a
/// second time. `sonnet` / `opus` / `fable` — the three
/// `ThreadSettings::model` takes, which `apps/aui`'s private `model_alias`
/// also spells. [`tests::the_new_thread_act_is_the_daemons_and_carries_his_id`]
/// pins all three, so a label change that broke an alias goes red here rather
/// than reaching the daemon as an unknown model.
fn model_alias(m: Model) -> String {
    m.label().to_ascii_lowercase()
}

// ────────────────────────────────────────────────────────────────────────────
//  THE PROJECT HALF — §5's second frame, and the root's two affordances
// ────────────────────────────────────────────────────────────────────────────

/// **THE FIVE PROJECT ACTS**, in the design doc's order: the sheet's three,
/// then the root's two.
///
/// `0060`'s verdict is that the project sheet SURVIVES WHOLE, and the argument
/// is that a project is not a daemon thing at all — it is `~/minds/<name>` plus
/// the AUI's index, archive set and settings. So unlike [`ThreadAct`], whose
/// table has two owners, **every arm of this one is the AUI**. There is no
/// trap row here and no declined offer: the daemon has no project concept to
/// decline with (`lib/threads` has no `project` anywhere).
///
/// That makes [`route_project`] look trivial, and it is worth saying why it is
/// still a table rather than five inline constructions in `app.rs`: these are
/// **the acts that change what a project IS** — `Remove` takes one off his
/// root — so "which bytes leave for which act" has to be assertable as a
/// VALUE, with no socket in the test process, or it is not assertable at all.
///
/// # ⚠ WHAT `Remove` DOES CHANGED UNDER THESE COMMENTS ⚠
///
/// It used to unlink `~/minds/<name>` and every document in this arc says so.
/// **It does not any more.** The registry cutover replaced discovery-by-
/// directory with `~/.cauldron/projects.json`, and the v2 handler is now one
/// call to `projects::Registry::remove` (`apps/aui/src/v2/mutate.rs:599-610`,
/// under a comment reading *"the `is a real directory here, not a link`
/// refusal is GONE with the thing it described"*). That function **has no
/// filesystem in it** (`libs/cauldron/src/lib.rs:672`): it removes the ENTRY
/// and touches neither the directory, nor `.mind/`, nor `.mind/threads.json`,
/// nor the archive set, nor `~/minds/<name>`. The threads stay on disk and
/// come back if he re-registers the same path under the same name.
///
/// So the round's stated hazard — *"`RemoveProject` unlinks `~/minds/<name>`
/// on his real machine"* — is **no longer true of this tree**. The act is
/// still the one that changes his root most, and it still asks first; it is
/// no longer destructive of anything on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectAct {
    /// The project's DEFAULT model. `None` clears it back to the global.
    SetModel(Option<Model>),
    /// Leaves the root; threads kept; one click brings it back.
    Archive,
    /// **DE-REGISTERS.** Removes the entry from
    /// `~/.cauldron/projects.json` and NOTHING ELSE — not the directory, not
    /// `.mind/`, not `~/minds/<name>`. See the enum's own header: this used to
    /// unlink and no longer does. The v2 handler's refusals (an unknown name,
    /// the bucket) are written for a human and this face relays them verbatim.
    Remove,
    /// From the archived door — the inverse of [`ProjectAct::Archive`].
    Unarchive,
    /// From the root's `+ new project` row. The name is his raw typing; the
    /// SERVER normalizes it (`normalize_project_name`, the one normalizer) and
    /// refuses a collision rather than repairing it, so this face does neither.
    Create { name: String },
}

/// **`miscellaneous` — THE ONE NAME TWO OF THESE ACTS MAY NOT BE SPOKEN ABOUT.**
///
/// It is the un-projected bucket (`crate::index`): not under `~/minds`, so
/// there is no symlink to unlink, and never archivable, because there is
/// nowhere for its threads to go. The sheet draws neither row for it — ABSENT,
/// not unavailable — and this constant is the second, independent guard on the
/// same fact, for the reason the model trap has three: a later worker building
/// a project sheet from a different entry point would otherwise reach a live
/// `ArchiveProject{name: "miscellaneous"}`.
pub const BUCKET: &str = "miscellaneous";

/// **THE PROJECT TABLE.** One act in, one wire out, and every arm is the AUI.
///
/// `project` is the `~/minds/<name>` basename, which is the key every one of
/// these mutations is addressed by — `RemoveProject` and `SetProjectModel`
/// spell the field `project`, `Archive`/`Unarchive`/`Create` spell it `name`,
/// and that asymmetry is the SERVER'S and is reproduced here rather than
/// smoothed over.
pub fn route_project(act: &ProjectAct, project: &str) -> Result<Wire, Refusal> {
    match act {
        ProjectAct::SetModel(model) => Ok(Wire::Aui(Mutation::SetProjectModel {
            project: project.to_string(),
            model: *model,
        })),
        ProjectAct::Archive => {
            bucket_guard(project, "archived")?;
            Ok(Wire::Aui(Mutation::ArchiveProject {
                name: project.to_string(),
            }))
        }
        ProjectAct::Remove => {
            bucket_guard(project, "removed")?;
            Ok(Wire::Aui(Mutation::RemoveProject {
                project: project.to_string(),
            }))
        }
        // Unarchive is NOT guarded: `miscellaneous` can never be in the archive
        // set (the face filters it out of the set it reads, mirroring
        // `index::load_projects`), so a guard here would be a rule with no
        // reachable case, which is worse than no rule — it reads as though the
        // case exists.
        ProjectAct::Unarchive => Ok(Wire::Aui(Mutation::UnarchiveProject {
            name: project.to_string(),
        })),
        // **THE NAME IS SENT RAW.** `normalize_project_name` is the server's one
        // normalizer; normalizing here too would be a second answer to "what is
        // this project called", and the second one is the one his directory gets
        // named by. An EMPTY name is refused here because there is nothing to
        // send, and the server's own words for that case are kept.
        ProjectAct::Create { name } => {
            if name.trim().is_empty() {
                return Err("I couldn't make a project name out of that.".into());
            }
            Ok(Wire::Aui(Mutation::CreateProject {
                name: name.trim().to_string(),
            }))
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
//  THE THIRD OWNER — and it is a FILE, not a socket
// ────────────────────────────────────────────────────────────────────────────

/// **THE TWO ACTS THAT CHANGE THE REGISTRY WITH NO MUTATION BEHIND THEM.**
///
/// # Why these two are not [`ProjectAct`]s
///
/// Every arm of [`route_project`] ends in a `Mutation` the AUI's v2 handler
/// already parses. These two do not exist on 9010 at all: there is no
/// `register a directory that is already there` mutation (`CreateProject`
/// MAKES `~/code/<slug>` and refuses a path — `apps/aui/src/v2/mutate.rs:631`),
/// and there is no `MoveProject` (`libs/cauldron/tests/registry.rs:188` names
/// one; the vocabulary in `libs/aui-state/src/server/mod.rs:62-305` does not
/// have it). Routing them through `Wire` would mean minting a mutation kind
/// `Mutation::from_envelope` nacks — a stub that pretends to work.
///
/// # So where does the write go? `0071` §13, and it is a SANCTIONED writer
///
/// > *"a `[[bin]]` in `libs/cauldron` that calls `Registry::update` under the
/// > same lock and the same atomic write — **a third writer, but through the
/// > one implementation**, and visible to the AUI on its next `Snapshot::get`
/// > (a stat) without a restart."*
///
/// That binary is SHIPPED (`libs/cauldron/src/bin/cauldron.rs`), so
/// *"`ui-server` is the only writer"* is already false in this tree, and the
/// AUI is BUILT for it: `Snapshot::refresh` stats on every `get`
/// (`libs/cauldron/src/snapshot.rs:99`) and
/// `state::refresh_projects_if_changed` re-reads the list on root-menu entry
/// and on every v2 projection build (`apps/aui/src/state.rs:435`,
/// `dispatch/root.rs:47`, `v2/store.rs:149`) precisely so *"a registration
/// made by a future `cauldron register` CLI, **or by another face**, has to be
/// visible to the next call without a restart"* (`snapshot.rs:5-7`).
///
/// **This face is that other face.** It writes through
/// [`crate::index::apply_registry`], which is one call to
/// `projects::Registry::update` and nothing else — the same lock, the same
/// temp/fsync/rename, the same one implementation. It is emphatically NOT the
/// `.mind/threads.json` case, where a behind-the-AUI's-back write is *"simply
/// undone"* because `AppState` rewrites that index from memory on every turn
/// (`apps/aui/src/v2/mutate.rs:13-16`). The registry has no in-memory master;
/// the FILE is the master and every reader re-stats it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryAct {
    /// **`p` — ADD A PROJECT BY PATH.** The directory already exists; this
    /// registers it. The name is the caller's, or the basename when `None`,
    /// which is what `cauldron register <path>` does.
    Register {
        path: std::path::PathBuf,
        name: Option<String>,
    },
    /// **REORDER — `ctrl-↑` / `ctrl-↓` on a project row.** Same key pair, same
    /// neighbour-naming grammar and the same [`move_before`] as the pinned
    /// band; the cursor follows by NAME, as the pins' follows by id.
    Reorder { name: String, dir: Dir },
}

/// **THE WRITE THAT LEAVES FOR A REGISTRY ACT**, before any file is opened —
/// [`Wire`]'s counterpart for the owner that is not a socket, and separate
/// from it for the same reason `Wire` exists: the decision is asserted as a
/// VALUE, with no filesystem in the test process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryWrite {
    Register {
        path: std::path::PathBuf,
        name: Option<String>,
    },
    /// Place `name` immediately before `before`; `None` means last. **A
    /// NEIGHBOUR'S NAME, never an index** — `Registry::move_before`'s own
    /// contract, and the reason a list one second old names a project rather
    /// than a slot.
    MoveBefore {
        name: String,
        before: Option<String>,
    },
}

/// **THE REGISTRY TABLE.** Pure, and the reorder arm is literally the pinned
/// band's [`move_before`] — `0099`'s *"reorder by the existing grammar"* is a
/// shared function here, not a second implementation that resembles one.
///
/// `order` is the DISPLAYED project order, top to bottom, exactly as the face
/// drew it, **bucket included**. It has to include the bucket: the bucket is a
/// drawn row he can put his cursor on, so a neighbour computed from a list
/// that pretended it was not there would name the wrong project whenever he
/// moves the last real one down.
pub fn route_registry(act: &RegistryAct, order: &[String]) -> Result<RegistryWrite, Refusal> {
    match act {
        RegistryAct::Register { path, name } => {
            // The crate re-checks all of this under the lock — that is what
            // makes a genuine race safe. This pass is what makes a PREDICTABLE
            // refusal arrive before anything is opened, which is the shape
            // `CreateProject` was taught by a measured defect
            // (`mutate.rs:648-659`: a correctly refused name still left a
            // directory behind it).
            if path.as_os_str().is_empty() {
                return Err("I need the path of a directory to add.".into());
            }
            Ok(RegistryWrite::Register {
                path: path.clone(),
                name: name.clone(),
            })
        }
        RegistryAct::Reorder { name, dir } => {
            // **THE BUCKET IS NOT IN THE FILE**, so it can be neither moved nor
            // named as a destination (`0071` §4: synthesized, always last).
            // `Registry::move_before` would answer `no_such_project`, which is
            // true and useless — it reads as though the row were a stranger.
            if name == BUCKET {
                return Err(format!(
                    "{BUCKET} is where threads with no project live — it is always last"
                ));
            }
            let before = neighbour(order, name, *dir).map_err(|e| match e {
                Edge::NotInOrder => "that project is not in the list".to_string(),
                Edge::AtTop => "it is already first".to_string(),
                Edge::AtBottom => "it is already last".to_string(),
            })?;
            // The bucket is drawn LAST and is not in the file, so "put this one
            // before the bucket" and "put it last" are the same destination —
            // and only one of them is a name `Registry::move_before` will
            // accept. Collapsing it here rather than refusing keeps the last
            // real project reorderable, which refusing would have quietly
            // taken away.
            let before = before.filter(|b| b != BUCKET);
            // `Registry::move_before(x, None)` is a remove-and-reinsert-at-end,
            // so a project that is ALREADY last would be "moved" onto itself:
            // a locked write, a rewritten file and a byte-identical document.
            // `0071` §15's rule for the pinned band is that a move that cannot
            // happen says so rather than looking like it worked; the same rule
            // says a move that is a no-op is not a write.
            let last_real = order.iter().rev().find(|n| n.as_str() != BUCKET);
            if before.is_none() && last_real.map(String::as_str) == Some(name.as_str()) {
                return Err("it is already last".into());
            }
            Ok(RegistryWrite::MoveBefore {
                name: name.clone(),
                before,
            })
        }
    }
}

/// **A NAME OR A PATH — and the two cannot be confused.**
///
/// `p` and the `+ new project` row are ONE field with one act (*add a
/// project*) and two shapes of argument, rather than two fields: a project
/// NAME can never contain `/`, because `projects::name_objection` refuses one
/// that does (`libs/cauldron/src/lib.rs:357`) and
/// `aui_wire::server::normalize_project_name` strips it before the directory
/// is ever made. So the split is TOTAL, not a heuristic, and
/// [`tests::a_name_and_a_path_are_disjoint_and_the_crate_agrees`] asserts it
/// against the crate's own refusal rather than against this comment.
///
/// `~` is a path because nothing else starts with it and he types it; a bare
/// `.` or `..` is a path because `name_objection` refuses both as names.
pub fn looks_like_a_path(typed: &str) -> bool {
    let t = typed.trim();
    t.contains('/') || t.starts_with('~') || t == "." || t == ".."
}

fn bucket_guard(project: &str, verb: &str) -> Result<(), Refusal> {
    if project == BUCKET {
        return Err(format!(
            "{BUCKET} is where threads with no project live — it cannot be {verb}"
        ));
    }
    Ok(())
}

/// Which socket an act uses, without building its bytes. Static — it does not
/// depend on the thread, the link state, or anything else that can change.
pub fn owner_of(act: &ThreadAct) -> Owner {
    match act {
        ThreadAct::Interrupt | ThreadAct::Compact { .. } | ThreadAct::Rename { .. } => Owner::Daemon,
        ThreadAct::SetModel(_)
        | ThreadAct::SetPinned(_)
        | ThreadAct::Move(_)
        | ThreadAct::SetClosed(_) => Owner::Aui,
    }
}

/// **`/compact`, byte-for-byte as `submit_compaction_turn` sends it.**
///
/// Three details that are easy to get almost right — and one that was true
/// until the daemon re-pathed itself: **`thread_id` is no longer in here.** It
/// is the SUBJECT of `threads/{thread_id}/send`, it rides the path, and a call
/// that also puts it in the params is REFUSED rather than reconciled. That is
/// why this function no longer takes an id at all.
///
/// - The text is `"/compact"` or `"/compact <instructions>"`, and
///   `prompt_and_headers` (`threads_client.rs:2304-2315`) appends a trailing
///   NEWLINE to every prompt. So the bytes end in `\n`; a version without it is
///   a different message to the model.
/// - `bare: true` — the slash command must be the FIRST thing in the prompt or
///   the daemon injects its own header ahead of it and the command stops being
///   a command. `headers` is empty for the same reason.
/// - `metadata.tui.compaction.reason` is stamped **for evidence, not for
///   function**. The AUI's own face reads *"two `ThreadCompactionStarted`
///   events = he did it, one = the context overflowed"*
///   (`dispatch/mod.rs:3627-3653`), so a compaction started from here will read
///   as an OVERFLOW on the AUI's face. That is an AUI-side heuristic
///   conflating "not via me" with "automatic", it is filed as an AUI issue, and
///   `0060` rules explicitly that it is NOT to be dodged by routing compaction
///   through 9010. The stamp costs nothing and means the evidence exists on the
///   daemon's own ledger when somebody fixes it.
pub fn compact_verb(thread: &str, instructions: Option<&str>) -> Verb {
    let text = match instructions.map(str::trim).filter(|s| !s.is_empty()) {
        Some(i) => format!("/compact {i}\n"),
        None => "/compact\n".to_string(),
    };
    Verb::Send {
        thread: thread.to_string(),
        text,
        intent: Some("immediate".into()),
        bare: true,
        headers: Vec::new(),
        metadata: json!({
            "tui": {
                "client": "harness-tui",
                "compaction": {
                    "reason": instructions.map(str::trim).filter(|s| !s.is_empty()),
                }
            }
        }),
    }
}

/// **`before` FOR A MOVE, COMPUTED FROM THE DISPLAYED ORDER.**
///
/// `MovePin{before}` means *place this thread immediately before `before`*;
/// `None` means place it last. The server then renumbers ALL pins from zero
/// (`mutate.rs:683-743`), across projects, so a tie in `pin_order` cannot
/// swallow the move. This face supplies a neighbour and nothing else.
///
/// - **Up**: `before` is the row currently ABOVE, so the moved row lands where
///   that row was.
/// - **Down**: `before` is the row TWO BELOW — the one that must end up under
///   it after the swap — or `None` when the move is to last.
///
/// `Err` is *"this row cannot move that way"*, which is a different thing from
/// `Ok(None)` (*"move it to the end"*). Collapsing the two is the bug this
/// signature exists to make impossible.
pub fn move_before(order: &[String], id: &str, dir: Dir) -> Result<Option<String>, Refusal> {
    neighbour(order, id, dir).map_err(|e| {
        match e {
            Edge::NotInOrder => "this thread is not on the pinned band — pin it first",
            Edge::AtTop => "it is already at the top of the pinned band",
            Edge::AtBottom => "it is already at the bottom of the pinned band",
        }
        .to_string()
    })
}

/// **WHY A MOVE CANNOT HAPPEN**, with the noun left out.
///
/// The arithmetic of "which neighbour" is identical for a pinned thread and
/// for a project row — `0099`'s *"reorder by the existing grammar"* — but the
/// SENTENCES are not: a project that cannot move up is not *"at the top of the
/// pinned band"*. Splitting the reason from its wording is what lets the two
/// callers share the one implementation without either of them speaking the
/// other's words. See [`move_before`] and [`route_registry`], which are the
/// only two callers and are the two wordings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    NotInOrder,
    AtTop,
    AtBottom,
}

/// [`move_before`]'s arithmetic, with no noun in it. See [`Edge`].
pub fn neighbour(order: &[String], id: &str, dir: Dir) -> Result<Option<String>, Edge> {
    let Some(at) = order.iter().position(|o| o == id) else {
        return Err(Edge::NotInOrder);
    };
    match dir {
        Dir::Up if at == 0 => Err(Edge::AtTop),
        Dir::Up => Ok(Some(order[at - 1].clone())),
        Dir::Down if at + 1 >= order.len() => Err(Edge::AtBottom),
        // Two below, or off the end — which IS the "place it last" case.
        Dir::Down => Ok(order.get(at + 2).cloned()),
    }
}

fn uuid_of(t: &ThreadRef) -> Result<Uuid, Refusal> {
    parse_uuid(&t.id)
}

/// A pre-migration id that is not a uuid cannot address an AUI mutation, and
/// that is said out loud on the one act rather than swallowed. The daemon acts
/// (interrupt, compact) take the id as a STRING and keep working for such a
/// thread — which is the honest split, not a convenience.
fn parse_uuid(id: &str) -> Result<Uuid, Refusal> {
    Uuid::parse_str(id)
        .map_err(|_| format!("this thread's id ({id}) is not one the audio interface can address"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TID: &str = "11111111-2222-3333-4444-555555555555";

    fn thr() -> ThreadRef {
        ThreadRef {
            project: "harness".into(),
            id: TID.into(),
            turn_count: 41,
            pinned: true,
            closed: false,
        }
    }

    fn route1(act: ThreadAct) -> Wire {
        route(&act, &thr(), &[]).expect("routed")
    }

    // ────────────────────────────────────────────────────────────────────
    //  THE TRAP
    // ────────────────────────────────────────────────────────────────────

    /// **THE TEST THIS TASK EXISTS AROUND.**
    ///
    /// The daemon HAS a model verb — `threads/{id}/update{patch:{model}}`,
    /// `lib/threads/src/server.rs:558-566` — and using it would look like a
    /// simplification and be a silent revert on his next utterance.
    ///
    /// FORCED RED, and this is the forced failure the task names: change the
    /// `SetModel` arm of [`route`] to
    /// `Wire::Daemon { method: "threads/{id}/update", params: json!({"thread_id":
    /// t.id, "patch": {"model": model}}) }` and this test fails on its first
    /// assertion, naming the trap.
    #[test]
    fn the_model_act_goes_to_the_aui_and_the_daemons_verb_is_never_spoken() {
        let w = route1(ThreadAct::SetModel(Some(Model::Opus)));
        assert_eq!(
            w.owner(),
            Owner::Aui,
            "the model act was routed to the threads daemon — that verb exists, \
             and the AUI's fingerprint sync silently reverts it on the thread's \
             next turn (threads_client.rs:2159-2178). It is a stub that pretends \
             to work."
        );
        assert_eq!(
            w,
            Wire::Aui(Mutation::SetThreadModel {
                project: "harness".into(),
                thread_id: Uuid::parse_str(TID).unwrap(),
                model: Some(Model::Opus),
            })
        );
        // And the OTHER half of the act: clearing the override.
        assert_eq!(
            route1(ThreadAct::SetModel(None)),
            Wire::Aui(Mutation::SetThreadModel {
                project: "harness".into(),
                thread_id: Uuid::parse_str(TID).unwrap(),
                model: None,
            }),
            "None must clear the override, not be dropped"
        );
        assert_eq!(owner_of(&ThreadAct::SetModel(None)), Owner::Aui);
    }

    /// **THE TRAP, BANNED AT THE SOURCE LEVEL TOO**, in the shape
    /// `sends.rs`'s own surface test uses. A later worker who reaches for the
    /// daemon verb fails here even if they also "fixed" the test above.
    #[test]
    fn this_module_never_names_the_daemons_trap_verb() {
        let whole = include_str!("acts.rs");
        let body = whole
            .split_once("#[cfg(test)]")
            .expect("the test module is the end of this file")
            .0;
        // **CODE ONLY.** Whole-line comments are dropped, because this file's
        // header names the trap on purpose and must go on naming it — the
        // prohibition is worth nothing if the reason for it cannot be written
        // down beside it. A TRAILING comment on a code line is deliberately
        // still in scope, so the strip cannot be used as a hiding place.
        let src: String = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let src = src.as_str();
        // **THE DAEMON METHODS THIS FACE MAY SPEAK, AND THE COMPLETE SET.**
        //
        // This used to be a `match_indices("\"threads.")` over the dotted
        // spelling. When the daemon re-pathed itself that needle matched
        // nothing — and the two halves of this test then failed DIFFERENTLY,
        // which is the thing to carry away: the `assert_eq!` below went RED,
        // because an empty list is not a two-element list, while every
        // `!src.contains(…)` in the loop under it went SILENT, because a
        // string that no longer exists anywhere cannot be contained. **A
        // POSITIVE ASSERTION FAILS LOUDLY WHEN ITS SUBJECT IS RENAMED; A
        // NEGATIVE ONE GOES QUIET.** `crate::methods`'s header has the long
        // version of that and the three selectors it takes to close the gap.
        // **THE DAEMON SURFACE OF THIS ROUTER, AND IT IS THREE VERBS.**
        //
        // This used to assert over `methods::` references and then over two
        // spellings of a hand-written literal. `methods.rs` is deleted, so what
        // is left to assert over is the ENUM — and that is a stronger statement
        // than the list was, because a `Verb` this face has no variant for
        // cannot be constructed at all, whereas a name could always be typed.
        //
        // **THE SENTINEL FIRST.** A scan that matched nothing would make
        // everything below it vacuous at once, and this file has shipped
        // exactly that: when the daemon re-pathed itself the dotted needle
        // matched nothing, the `assert_eq!` went RED and every
        // `!src.contains(…)` under it went SILENT. A positive assertion fails
        // loudly when its subject is renamed; a negative one goes quiet.
        for verb in ["Verb::Cancel", "Verb::Send", "Verb::Create"] {
            assert!(
                src.contains(verb),
                "`{verb}` is not constructed in this router, so the claims \
                 below are about a file that routes nothing to the daemon"
            );
        }
        // And no FOURTH. `Verb` is three variants; this asserts the router uses
        // all three and names no other, which together say the set is closed.
        for (i, _) in src.match_indices("Verb::") {
            let rest = &src[i + "Verb::".len()..];
            let end = rest
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .unwrap_or(rest.len());
            let name = &rest[..end];
            assert!(
                ["Cancel", "Send", "Create", "Name"].contains(&name),
                "`Verb::{name}` is routed and is not one of the three this face \
                 speaks — widening the surface is a deliberate act, not an edit"
            );
        }
        // No method PATH written by hand, in EITHER grammar. The new spelling
        // is `"threads/…"`; the pre-2026-09-28 one is `"threads.…"`, and that
        // second selector is the one a migration's own guard forgets — it is
        // blind to exactly the regression it was written for, a line REVERTED.
        for needle in ["\"threads/", "\"threads."] {
            assert!(
                !src.contains(needle),
                "a method path is written by hand in the act router — every \
                 method this face speaks is a `seam::commands` verb"
            );
        }
        // The trap by name, plus the acts that are the AUI's relays of daemon
        // work, plus the service administration the design doc cut.
        //
        // **`update` IS THE LOAD-BEARING ONE.** `threads/{id}/update`
        // `{patch:{model}}` EXISTS on the daemon, and the AUI declares `model`
        // a field it is authoritative for — so a model set through 3651
        // survives until his next utterance and then SILENTLY REVERTS. It is a
        // stub that pretends to work, which is why it is banned here as well
        // as checked positively above.
        //
        // It is matched as `/update` rather than as a whole dotted name on
        // purpose: that catches `methods::update`, a hand-built
        // `format!("threads/{id}/update")`, and anything else that spells the
        // leaf, instead of only the one obsolete form.
        for never in [
            "/update",
            "Verb::Update",
            "Verb::Seen",
            "Verb::Event",
            "InterruptThread",
            "CompactThread",
            "AdminRestartServer",
            "AdminShutdownServer",
            "SetRingTarget",
        ] {
            assert!(
                !src.contains(never),
                "{never} is reachable from the act router — it is not in this face's surface"
            );
        }
    }

    // ────────────────────────────────────────────────────────────────────
    //  EVERY ACT TO ITS OWNER
    // ────────────────────────────────────────────────────────────────────

    /// **The whole table in one assertion.** FORCED RED by flipping any single
    /// arm of [`owner_of`] or [`route`] to the other socket.
    #[test]
    fn every_thread_act_goes_to_its_owner_and_the_two_sets_do_not_overlap() {
        let aui = [
            ThreadAct::SetModel(Some(Model::Fable)),
            ThreadAct::SetPinned(true),
            ThreadAct::SetClosed(true),
        ];
        for a in aui {
            assert_eq!(owner_of(&a), Owner::Aui, "{a:?} left by the wrong socket");
            assert_eq!(route(&a, &thr(), &[]).expect("routed").owner(), Owner::Aui);
        }
        let daemon = [
            ThreadAct::Interrupt,
            ThreadAct::Compact { instructions: None },
            // TUI-P1a: a rename is the daemon's (TC-03/TC-28).
            ThreadAct::Rename {
                summary: "x".into(),
            },
        ];
        for a in daemon {
            assert_eq!(
                owner_of(&a),
                Owner::Daemon,
                "{a:?} was relayed through the AUI — DIR-7's resolution forbids \
                 exactly that, and it would make the act fail when the AUI is down"
            );
            assert_eq!(
                route(&a, &thr(), &[]).expect("routed").owner(),
                Owner::Daemon
            );
        }
        // `owner_of` and `route` must never disagree — two sources of one fact
        // is how a binding drifts in the first place.
        let m = ThreadAct::Move(Dir::Up);
        assert_eq!(owner_of(&m), Owner::Aui);
    }

    /// The interrupt's exact bytes, and it takes the id as a STRING — so a
    /// pre-migration thread the AUI cannot address can still be stopped.
    #[test]
    fn interrupt_is_the_daemons_own_verb_with_its_one_parameter() {
        assert_eq!(
            route1(ThreadAct::Interrupt),
            Wire::Daemon(Verb::Cancel {
                thread: TID.to_string(),
            })
        );
        let legacy = ThreadRef {
            id: "sess-not-a-uuid".into(),
            ..thr()
        };
        assert_eq!(
            route(&ThreadAct::Interrupt, &legacy, &[]).expect("a legacy id can still be stopped"),
            Wire::Daemon(Verb::Cancel {
                thread: "sess-not-a-uuid".to_string(),
            })
        );
        // …but an AUI-owned act on that same thread refuses IN WORDS rather
        // than sending a malformed mutation or silently doing nothing.
        let err = route(&ThreadAct::SetPinned(true), &legacy, &[]).expect_err("refused");
        assert!(err.contains("sess-not-a-uuid"), "{err}");
    }

    /// **COMPACTION'S BYTES, AND THE TRAILING NEWLINE IS PART OF THEM.**
    ///
    /// FORCED RED by dropping the `\n` (the AUI's `prompt_and_headers` adds
    /// one to every prompt), by dropping `bare: true` (the daemon then injects
    /// its header ahead of the slash command and it stops being a command), or
    /// by dropping the metadata stamp.
    #[test]
    fn compaction_is_the_aui_s_own_send_byte_for_byte_with_and_without_instructions() {
        assert_eq!(
            route1(ThreadAct::Compact { instructions: None }),
            Wire::Daemon(Verb::Send {
                thread: TID.to_string(),
                text: "/compact\n".to_string(),
                intent: Some("immediate".into()),
                bare: true,
                headers: Vec::new(),
                metadata: json!({"tui": {"client": "harness-tui",
                                         "compaction": {"reason": Value::Null}}}),
            })
        );
        let with = route1(ThreadAct::Compact {
            instructions: Some("  keep the wire notes  ".into()),
        });
        let Wire::Daemon(Verb::Send { text, metadata, .. }) = &with else {
            panic!("compaction left by the AUI socket: {with:?}");
        };
        assert_eq!(text, "/compact keep the wire notes\n");
        assert_eq!(
            metadata["tui"]["compaction"]["reason"],
            json!("keep the wire notes"),
            "the reason stamp is the only evidence a TUI-started compaction was deliberate"
        );
        // Whitespace-only instructions are BARE, not a trailing space.
        let blank = route1(ThreadAct::Compact {
            instructions: Some("   ".into()),
        });
        let Wire::Daemon(Verb::Send { text, .. }) = &blank else {
            panic!("wrong socket")
        };
        assert_eq!(text, "/compact\n");
    }

    /// **A NEW THREAD'S EXACT BYTES, AND THE THREE THINGS THEY MUST NOT SAY.**
    ///
    /// FORCED RED four ways, each a mutation of one clause: send the id as a
    /// path segment (`threads/<id>/create`) and the method assertion goes red;
    /// drop `thread_id` from the params and the face can no longer open what it
    /// made; spell `app_id` as `aui` and this face starts claiming to be the
    /// other process; return a `Wire::Aui` and the owner assertion goes red
    /// with no socket in this process.
    #[test]
    fn the_new_thread_act_is_the_daemons_and_carries_his_id() {
        let id = Uuid::parse_str(TID).expect("TID is a uuid");
        let w = route_new_thread(&id, Path::new("/Users/x/.aui/miscellaneous"), Model::Sonnet, None);
        assert_eq!(w.owner(), Owner::Daemon, "a create left by the wrong socket");
        assert_eq!(
            w,
            Wire::Daemon(Verb::Create {
                // The id is `Verb::Create`'s `thread`, and
                // `seam::commands::create` is the ONE place it becomes a
                // `thread_id` PARAMETER — never a path segment, because the act
                // that mints a key is subjectless under its family.
                thread: TID.to_string(),
                settings: json!({
                    "cwd": "/Users/x/.aui/miscellaneous",
                    "model": "sonnet",
                    "account": "auto",
                    "app_id": "harness-tui",
                }),
                name: None,
            })
        );
        // **THE THREE ALIASES, PINNED.** `model_alias` derives them from
        // `Model::label`, so a label edit that broke one would otherwise reach
        // the daemon as an unknown model with nothing red in between.
        for (m, alias) in [
            (Model::Sonnet, "sonnet"),
            (Model::Opus, "opus"),
            (Model::Fable, "fable"),
        ] {
            assert_eq!(model_alias(m), alias, "{m:?} is not an alias the daemon takes");
        }
        // **AND THE AUI'S IDENTITY IS NOT COPIED HERE.** These are
        // `AUI_OWNED_SETTINGS` — the fields `apps/aui` declares it is
        // authoritative for and re-patches every turn. A worker who "completes"
        // the blob with them makes this face a second derivation of another
        // process's identity, and this is what says so.
        let Wire::Daemon(Verb::Create { settings, .. }) = &w else {
            panic!("wrong socket")
        };
        let settings = settings.as_object().expect("a settings object");
        for aui_owned in [
            "append_system_prompt",
            "add_dirs",
            "plugin_roots",
            "permission_mode",
            "inject_context",
        ] {
            assert!(
                !settings.contains_key(aui_owned),
                "{aui_owned} is the AUI's to declare, not this face's"
            );
        }
    }

    /// The AUI's own guard, at the AUI's own threshold and in its own words.
    #[test]
    fn a_thread_with_no_turns_cannot_be_compacted() {
        let fresh = ThreadRef {
            turn_count: 0,
            ..thr()
        };
        let err = route(&ThreadAct::Compact { instructions: None }, &fresh, &[])
            .expect_err("a turnless thread was compacted");
        assert_eq!(
            err,
            "there's nothing to compact yet — this thread has no turns"
        );
        // One turn is enough — the boundary, not just the empty case.
        let one = ThreadRef {
            turn_count: 1,
            ..thr()
        };
        assert!(route(&ThreadAct::Compact { instructions: None }, &one, &[]).is_ok());
    }

    // ────────────────────────────────────────────────────────────────────
    //  THE PIN MOVE — N >= 2, because one pin cannot be reordered
    // ────────────────────────────────────────────────────────────────────

    /// **`before` NAMES A NEIGHBOUR; THE SERVER RENUMBERS.**
    ///
    /// FORCED RED by using `at + 1` for Down (which is a no-op move: a row
    /// placed immediately before the row already under it does not move) or by
    /// returning `Ok(None)` at the top instead of `Err`.
    #[test]
    fn a_move_names_the_neighbour_it_must_land_before() {
        let order: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        // UP: land where the row above is now.
        assert_eq!(move_before(&order, "c", Dir::Up), Ok(Some("b".into())));
        assert_eq!(move_before(&order, "b", Dir::Up), Ok(Some("a".into())));
        // DOWN: land before the row TWO below, so exactly one row passes it.
        assert_eq!(move_before(&order, "a", Dir::Down), Ok(Some("c".into())));
        assert_eq!(move_before(&order, "b", Dir::Down), Ok(Some("d".into())));
        // DOWN from second-last is the "place it last" case — `Ok(None)`, which
        // is NOT the same value as a refusal.
        assert_eq!(move_before(&order, "c", Dir::Down), Ok(None));
    }

    /// The edges refuse IN WORDS, and a refusal is distinguishable from
    /// "place it last".
    #[test]
    fn the_ends_of_the_band_refuse_in_words_and_are_not_a_silent_no_op() {
        let order: Vec<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            move_before(&order, "a", Dir::Up),
            Err("it is already at the top of the pinned band".into())
        );
        assert_eq!(
            move_before(&order, "b", Dir::Down),
            Err("it is already at the bottom of the pinned band".into())
        );
        assert!(
            move_before(&order, "zz", Dir::Up).is_err(),
            "an unpinned thread cannot move"
        );
        // A single-pin band cannot move either way, and says so both times.
        let one = vec!["only".to_string()];
        assert!(move_before(&one, "only", Dir::Up).is_err());
        assert!(move_before(&one, "only", Dir::Down).is_err());
        assert!(move_before(&[], "only", Dir::Up).is_err(), "an empty band");
    }

    /// The move's full wire, end to end, with a real pinned order.
    #[test]
    fn a_move_becomes_move_pin_with_the_neighbours_uuid() {
        let other = "99999999-8888-7777-6666-555555555555";
        let order = vec![other.to_string(), TID.to_string()];
        assert_eq!(
            route(&ThreadAct::Move(Dir::Up), &thr(), &order).expect("routed"),
            Wire::Aui(Mutation::MovePin {
                project: "harness".into(),
                thread_id: Uuid::parse_str(TID).unwrap(),
                before: Some(Uuid::parse_str(other).unwrap()),
            })
        );
        // Moving the top one DOWN puts it last: `before: None`, which is the
        // server's "place it at the end" and not an absent field.
        let top = ThreadRef {
            id: other.into(),
            ..thr()
        };
        assert_eq!(
            route(&ThreadAct::Move(Dir::Down), &top, &order).expect("routed"),
            Wire::Aui(Mutation::MovePin {
                project: "harness".into(),
                thread_id: Uuid::parse_str(other).unwrap(),
                before: None,
            })
        );
    }

    /// Pin, unpin and close/reopen are the SAME mutation with a flipped
    /// boolean — there is no second verb for the inverse of any of them.
    #[test]
    fn the_inverse_of_each_flag_act_is_the_same_mutation_flipped() {
        for (act, want) in [
            (ThreadAct::SetPinned(false), false),
            (ThreadAct::SetPinned(true), true),
        ] {
            assert_eq!(
                route1(act),
                Wire::Aui(Mutation::SetThreadPinned {
                    project: "harness".into(),
                    thread_id: Uuid::parse_str(TID).unwrap(),
                    pinned: want,
                })
            );
        }
        assert_eq!(
            route1(ThreadAct::SetClosed(false)),
            Wire::Aui(Mutation::SetThreadClosed {
                project: "harness".into(),
                thread_id: Uuid::parse_str(TID).unwrap(),
                closed: false,
            }),
            "reopen is close with a false — the doc's 'closed threads stay on their list'"
        );
    }

    /// **A RENAME IS THE DAEMON'S, NAMES THE THREAD BY ITS ID, AND AN EMPTY
    /// ONE IS REFUSED BEFORE IT LEAVES** (TUI-P1a, TC-03/TC-28/TC-38). It used to
    /// be `Mutation::SetThreadSummary` on 9010: a thread this face created has no
    /// index header for that to land on, and the AUI SPOKE every rename. N = 2
    /// names, each trimmed and carried whole; opposed by an empty and a blank
    /// name, which must not leave at all (an empty `summary_updated` would read
    /// as "no name").
    ///
    /// Forcings (report): route `Rename` back to `Wire::Aui(SetThreadSummary)`
    /// → the owner and the bytes assertions are red; drop the empty check →
    /// the refusal assertions are red.
    #[test]
    fn a_rename_is_the_daemons_and_an_empty_one_never_leaves() {
        for (typed, want) in [("  Answer the tax letter ", "Answer the tax letter"), ("Plan", "Plan")] {
            let w = route1(ThreadAct::Rename { summary: typed.into() });
            assert_eq!(w.owner(), Owner::Daemon, "a rename left by the AUI's socket");
            assert_eq!(
                w,
                Wire::Daemon(Verb::Name {
                    thread: TID.into(),
                    name: want.into(),
                })
            );
        }
        for empty in ["", "   "] {
            assert!(
                route(&ThreadAct::Rename { summary: empty.into() }, &thr(), &[]).is_err(),
                "an empty name {empty:?} was sent — it would erase the thread's name"
            );
        }
    }

    /// **THE ENVELOPE ON THE WIRE**, which is what an assertion on BYTES
    /// ultimately means. The payload is the internally-tagged enum value, so
    /// the `kind` appears twice by construction and `from_envelope` — which is
    /// what `apps/aui/src/v2/ws.rs:159` calls — reads the payload.
    #[test]
    fn the_envelope_is_snake_case_and_round_trips_through_the_servers_own_parser() {
        let Wire::Aui(m) = route1(ThreadAct::SetModel(Some(Model::Opus))) else {
            panic!("wrong socket")
        };
        let env = m.clone().into_envelope();
        assert_eq!(env.kind, "set_thread_model");
        assert_eq!(
            env.payload,
            json!({
                "kind": "set_thread_model",
                "project": "harness",
                "thread_id": TID,
                "model": "Opus",
            }),
            "the Model enum carries NO rename_all — its variants serialize capitalised"
        );
        assert_eq!(
            Mutation::from_envelope(&env).expect("the server's own parser"),
            m,
            "the AUI would not have been able to read this"
        );
    }

    // ────────────────────────────────────────────────────────────────────
    //  THE PROJECT HALF — the destructive end of this face
    // ────────────────────────────────────────────────────────────────────

    /// **EVERY PROJECT ACT NAMES THE AUI AND THE MUTATION THE DESIGN BINDS IT
    /// TO, AS A VALUE, WITH NO SOCKET ANYWHERE.**
    ///
    /// This is the assertion that makes the rest of the project half safe to
    /// build: `RemoveProject` takes a project off his real root, so "which
    /// bytes would leave, for which act" must be answerable without anything
    /// leaving. (It de-registers rather than unlinking — see [`ProjectAct`]'s
    /// header — but a project vanishing from his list is still his list
    /// changing, and the wrong act reaching that socket is still the failure
    /// this test exists to make impossible.)
    ///
    /// FORCED RED five ways, one per arm: point `Archive` at
    /// `Mutation::RemoveProject`, `Remove` at `ArchiveProject`, `Unarchive` at
    /// `ArchiveProject`, `SetModel` at `SetThreadModel`, or `Create` at
    /// `ArchiveProject` — each turns this test red naming the arm.
    #[test]
    fn every_project_act_names_the_aui_and_its_own_mutation() {
        let cases: Vec<(ProjectAct, Mutation)> = vec![
            (
                ProjectAct::SetModel(Some(Model::Fable)),
                Mutation::SetProjectModel {
                    project: "harness".into(),
                    model: Some(Model::Fable),
                },
            ),
            (
                ProjectAct::SetModel(None),
                Mutation::SetProjectModel {
                    project: "harness".into(),
                    model: None,
                },
            ),
            (
                ProjectAct::Archive,
                Mutation::ArchiveProject {
                    name: "harness".into(),
                },
            ),
            (
                ProjectAct::Remove,
                Mutation::RemoveProject {
                    project: "harness".into(),
                },
            ),
            (
                ProjectAct::Unarchive,
                Mutation::UnarchiveProject {
                    name: "harness".into(),
                },
            ),
            (
                ProjectAct::Create {
                    name: "harness".into(),
                },
                Mutation::CreateProject {
                    name: "harness".into(),
                },
            ),
        ];
        for (act, want) in cases {
            let wire = route_project(&act, "harness")
                .unwrap_or_else(|e| panic!("{act:?} was refused: {e}"));
            assert_eq!(
                wire.owner(),
                Owner::Aui,
                "{act:?} left by the DAEMON socket — the daemon has no project concept at all"
            );
            let Wire::Aui(got) = wire else { unreachable!() };
            assert_eq!(
                got, want,
                "{act:?} did not build the mutation it is bound to"
            );
            // …and the SERVER'S OWN PARSER reads it. A payload it refuses is a
            // `MutResult::Err` on his machine and an act that silently did not
            // happen.
            let env = got.clone().into_envelope();
            assert_eq!(
                Mutation::from_envelope(&env).expect("the server's own parser"),
                got,
                "the AUI would not have been able to read {act:?}"
            );
        }
    }

    /// **THE FIELD NAMES ARE THE SERVER'S, AND THEY ARE NOT THE SAME WORD.**
    ///
    /// `RemoveProject` and `SetProjectModel` spell the key `project`;
    /// `Archive`, `Unarchive` and `Create` spell it `name`. That asymmetry is
    /// `libs/aui-state/src/server/mod.rs`'s and reproducing it is not a style
    /// choice — a frame with the wrong key is a `MutResult::Err` he would read
    /// as "the menu row is broken".
    ///
    /// FORCED RED by renaming either field in the route.
    #[test]
    fn the_wire_field_is_project_for_two_acts_and_name_for_three() {
        let env = |a: ProjectAct| {
            let Wire::Aui(m) = route_project(&a, "harness").expect("routed") else {
                unreachable!()
            };
            m.into_envelope().payload
        };
        for a in [ProjectAct::Remove, ProjectAct::SetModel(None)] {
            let p = env(a.clone());
            assert_eq!(p["project"], "harness", "{a:?} does not spell it `project`");
            assert!(p.get("name").is_none(), "{a:?} also carries a `name`");
        }
        for a in [
            ProjectAct::Archive,
            ProjectAct::Unarchive,
            ProjectAct::Create {
                name: "harness".into(),
            },
        ] {
            let p = env(a.clone());
            assert_eq!(p["name"], "harness", "{a:?} does not spell it `name`");
            assert!(p.get("project").is_none(), "{a:?} also carries a `project`");
        }
    }

    /// **`miscellaneous` CAN NEVER BE ARCHIVED OR REMOVED**, and the refusal is
    /// about the ACT rather than about a socket.
    ///
    /// The sheet does not DRAW those rows for the bucket at all, so this guard
    /// is the second, independent one — for the reason the model trap has
    /// three: a later worker reaching the router from a different entry point
    /// would otherwise send a live `ArchiveProject{name:"miscellaneous"}`.
    ///
    /// WITNESS, keyed on something neither guard can move: the SAME two acts
    /// route cleanly for an ordinary project in the same test. Without it a
    /// router that refused everything would score a triumphant pass.
    ///
    /// FORCED RED by deleting either `bucket_guard` call.
    #[test]
    fn the_un_projected_bucket_can_never_be_archived_or_removed() {
        for act in [ProjectAct::Archive, ProjectAct::Remove] {
            assert!(
                route_project(&act, "harness").is_ok(),
                "the WITNESS failed: {act:?} is refused for an ordinary project, \
                 so refusing it for the bucket proves nothing"
            );
            let why =
                route_project(&act, BUCKET).expect_err("the bucket was routed a destructive act");
            assert!(
                why.contains(BUCKET) && why.contains("cannot be"),
                "the refusal does not say what it is about: {why}"
            );
        }
        // The model DOES apply to the bucket — it is a real thread home with a
        // real default — and that is the whole distinction: two acts are absent
        // by KIND, the third is present.
        assert!(route_project(&ProjectAct::SetModel(None), BUCKET).is_ok());
    }

    /// An empty name is refused HERE, in the AUI's own words for that case, so
    /// that an accidental `enter` on an empty field is not a frame.
    ///
    /// FORCED RED by dropping the `trim().is_empty()` guard.
    #[test]
    fn an_empty_new_project_name_never_reaches_the_socket() {
        // WITNESS: a real name routes, so "refused" means something.
        assert!(route_project(&ProjectAct::Create { name: "ok".into() }, "ok").is_ok());
        for blank in ["", "   ", "\t\n"] {
            assert!(
                route_project(&ProjectAct::Create { name: blank.into() }, blank).is_err(),
                "a blank name ({blank:?}) built a frame"
            );
        }
    }

    // ────────────────────────────────────────────────────────────────────
    //  THE REGISTRY — the third owner
    // ────────────────────────────────────────────────────────────────────

    fn order(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    /// **A PROJECT REORDER NAMES A NEIGHBOUR AND NEVER AN INDEX** — and the
    /// arithmetic is the pinned band's, not a second copy of it.
    ///
    /// N >= 2 several times over: both directions, both ends, the middle, and
    /// a four-name list where "two below" and "off the end" are different
    /// answers. One case could not show that the shared function is being
    /// shared correctly rather than accidentally.
    ///
    /// FORCED RED three ways, each watched: return `Some(order[at + 1])` for
    /// `Down` (the `b` case then names `c` instead of `d`, and the move is a
    /// no-op); return the INDEX as a string (every assertion fails on the
    /// value); and delete the `AtTop` arm (`a` up becomes a write).
    #[test]
    fn a_project_reorder_names_a_neighbour_and_never_an_index() {
        let o = order(&["a", "b", "c", "d", BUCKET]);
        let up = |n: &str| {
            route_registry(
                &RegistryAct::Reorder {
                    name: n.into(),
                    dir: Dir::Up,
                },
                &o,
            )
        };
        let down = |n: &str| {
            route_registry(
                &RegistryAct::Reorder {
                    name: n.into(),
                    dir: Dir::Down,
                },
                &o,
            )
        };
        let before = |r: Result<RegistryWrite, Refusal>| match r {
            Ok(RegistryWrite::MoveBefore { before, .. }) => Ok(before),
            Ok(other) => panic!("a reorder built {other:?}"),
            Err(e) => Err(e),
        };

        // UP is the row ABOVE, so the moved row lands where that row was.
        assert_eq!(before(up("b")), Ok(Some("a".into())));
        assert_eq!(before(up("d")), Ok(Some("c".into())));
        // DOWN is the row TWO BELOW — the one that must end up under it.
        assert_eq!(before(down("a")), Ok(Some("c".into())));
        assert_eq!(before(down("b")), Ok(Some("d".into())));
        // …and `c` down is "place it last", which is `None` and NOT the bucket.
        // The bucket is drawn but is not in the file, so naming it as a
        // destination would be naming a project `Registry::move_before` has
        // never heard of.
        assert_eq!(
            before(down("c")),
            Ok(None),
            "the last real project's neighbour was the synthesized bucket"
        );
        // THE EDGES SAY WHICH EDGE, IN PROJECT WORDS.
        assert_eq!(before(up("a")), Err("it is already first".into()));
        assert_eq!(before(down("d")), Err("it is already last".into()));
        // A name that is not in the list is a different refusal from an edge.
        assert_eq!(
            before(up("nope")),
            Err("that project is not in the list".into())
        );
    }

    /// **THE BUCKET IS NEITHER A SUBJECT NOR A DESTINATION.**
    ///
    /// `0071` §4: it is SYNTHESIZED and always last, so the file has no entry
    /// to move. `Registry::move_before` would answer `no_such_project`, which
    /// is true and reads as though the row he is standing on were a stranger.
    ///
    /// FORCED RED by deleting the `name == BUCKET` guard — the bucket then
    /// builds a `MoveBefore` naming itself, which the crate refuses under the
    /// lock with the wrong sentence.
    #[test]
    fn the_bucket_cannot_be_reordered_and_is_never_named_as_a_destination() {
        let o = order(&["a", BUCKET]);
        // WITNESS: a real project in this same list DOES route, so a refusal
        // below means "refused" and not "this list is unusable".
        assert!(route_registry(
            &RegistryAct::Reorder {
                name: "a".into(),
                dir: Dir::Up,
            },
            &order(&["z", "a", BUCKET]),
        )
        .is_ok());
        for dir in [Dir::Up, Dir::Down] {
            let r = route_registry(
                &RegistryAct::Reorder {
                    name: BUCKET.into(),
                    dir,
                },
                &o,
            );
            assert!(r.is_err(), "the bucket moved: {r:?}");
            assert!(
                r.unwrap_err().contains("always last"),
                "the bucket's refusal did not say why"
            );
        }
    }

    /// **A NAME AND A PATH ARE DISJOINT, AND THE CRATE AGREES.**
    ///
    /// One field carries both, so the split must be a FACT rather than a
    /// heuristic — and this test is where the exact claim was MEASURED rather
    /// than assumed. The first draft asserted *"every path-shaped string is
    /// refused as a name by `projects::name_objection`"* and it is **false**:
    /// a bare `~` has no separator, no leading dot and no control character,
    /// so the crate would take it.
    ///
    /// The true, total claim is one step out: **nothing this function calls a
    /// path could ever have become a project of that name**, because either
    /// the crate refuses it outright OR
    /// `aui_wire::server::normalize_project_name` — the ONE normalizer, and
    /// the thing that actually decides what directory gets made — rewrites it
    /// into something he did not type. A bare `~` normalizes to the empty
    /// string and is refused by the AUI with *"I couldn't make a project name
    /// out of that"*; `./here` normalizes to `here`, which is a different
    /// word. Neither was ever reachable as his literal name, so neither is
    /// taken away by reading it as a path.
    ///
    /// FORCED RED by making `looks_like_a_path` return `true` for every
    /// non-empty string (`"harness"` then fails the second half), and forced
    /// red the other way by keeping the original over-strong assertion, which
    /// is how the `~` case was found.
    #[test]
    fn a_name_and_a_path_are_disjoint_and_the_crate_agrees() {
        for path in ["/Users/x/code/thing", "~/code/thing", "~", "./here", ".", ".."] {
            assert!(looks_like_a_path(path), "{path:?} was read as a name");
            let refused = projects::name_objection(path).is_some();
            let rewritten = aui_wire::server::normalize_project_name(path) != path;
            assert!(
                refused || rewritten,
                "{path:?} would have become a project of exactly that name — the \
                 one-field split is no longer total and `p` needs a field of its own"
            );
        }
        for name in ["harness", "type_3", "my project", "über"] {
            assert!(!looks_like_a_path(name), "{name:?} was read as a path");
            assert!(
                projects::name_objection(name).is_none(),
                "the crate refuses {name:?} as a name, so it is neither"
            );
        }
        // …and the two sets do not merely fail to overlap, they COVER: every
        // string above is one or the other, which is what makes the single
        // field answerable at all.
        assert!(looks_like_a_path("~") ^ looks_like_a_path("harness"));
    }

    /// **ONE ARITHMETIC, TWO WORDINGS.** The pinned band and the project band
    /// share [`neighbour`] and differ only in the sentence they put on an
    /// edge. This asserts the sharing is real: the same list, the same
    /// positions, the same answers — and two different sentences.
    ///
    /// FORCED RED by giving `route_registry` its own position search (the
    /// values still agree, so the test that catches it is this one's first
    /// half, which compares them).
    #[test]
    fn the_two_reorder_grammars_are_one_arithmetic_with_two_wordings() {
        let o = order(&["a", "b", "c"]);
        for (name, dir) in [("b", Dir::Up), ("a", Dir::Down), ("b", Dir::Down)] {
            let pinned = move_before(&o, name, dir);
            let project = route_registry(
                &RegistryAct::Reorder {
                    name: name.into(),
                    dir,
                },
                &o,
            );
            assert_eq!(
                pinned.clone().ok(),
                match project {
                    Ok(RegistryWrite::MoveBefore { before, .. }) => Some(before),
                    _ => None,
                },
                "{name} {dir:?}: the two bands computed different neighbours"
            );
            assert!(pinned.is_ok());
        }
        // The WORDINGS are not shared, and must not be.
        assert!(move_before(&o, "a", Dir::Up)
            .unwrap_err()
            .contains("pinned band"));
        assert!(!route_registry(
            &RegistryAct::Reorder {
                name: "a".into(),
                dir: Dir::Up,
            },
            &o,
        )
        .unwrap_err()
        .contains("pinned band"));
    }

    /// An empty path is refused before anything is opened — the shape
    /// `CreateProject` was taught by a measured defect (a correctly refused
    /// name still left a directory behind it, `mutate.rs:648-659`).
    #[test]
    fn an_empty_path_never_reaches_the_registry() {
        assert!(route_registry(
            &RegistryAct::Register {
                path: std::path::PathBuf::from("/Users/x/code/thing"),
                name: None,
            },
            &[],
        )
        .is_ok());
        assert!(route_registry(
            &RegistryAct::Register {
                path: std::path::PathBuf::new(),
                name: None,
            },
            &[],
        )
        .is_err());
    }
}
