//! **THE IN-MEMORY SESSION MODEL, and the projection of it into blocks.**
//!
//! # TWO CONCERNS, ONE FILE — deliberately, and here is the line between them
//!
//! `lib.rs` used to describe this module as "the in-memory session model,
//! which is deliberately almost empty". That was true when build step 2 wrote
//! it and false by the end of build step 3, which added the whole presentation
//! projection here — roughly half this file. `0037` corrected the description
//! rather than the code, and this header is where the boundary the split would
//! have drawn is written down instead:
//!
//! * **THE MODEL** — [`Store`], [`ThreadFacts`], [`Open`], [`Store::apply`]
//!   and the caches. It ingests [`crate::transport::TransportEvent`] and knows
//!   nothing about rows, glyphs, lanes or widths.
//! * **THE PROJECTION** — everything from `short` downwards, ending in the
//!   five public functions [`thread_blocks`], [`activity_blocks`],
//!   [`thread_list`], [`header`] and [`banner`]. It reads the model and
//!   [`crate::proto::ThreadEvent`] and produces the pure crate's vocabulary.
//!   **It is a PURE function of the model** — no clock it did not receive, no
//!   I/O, no `self`-mutation — which is what makes it testable in one process
//!   and what would make it movable later.
//!
//! **Why it is not two files, let alone in `tui-render`.** The projection
//! reads `proto::ThreadEvent`, and `tui-render` may not know the wire exists —
//! that is the gate its short `Cargo.toml` enforces, and taking `proto` in
//! there would end it. So the projection has to sit on this side of the crate
//! boundary; a second FILE would rename the problem without changing which
//! crate the knowledge lives in, and would move a large, well-tested body of
//! code for a boundary this comment already draws.
//!
//! **THE TWO RULES THE LINE IMPLIES, both machine-checked by
//! `the_two_halves_of_this_file_stay_on_their_own_side`:**
//!
//! 1. The MODEL half never builds a `Block` or a `Line` — presentation is
//!    produced below the line or not at all.
//! 2. The PROJECTION half never takes `&mut Store` — it reads the model, it
//!    does not move it, which is what makes "the projection is a pure function
//!    of the model" a fact rather than a habit.
//!
//! **And the honest exception, named rather than tidied away.** The model half
//! DOES build two pure-crate values during ingest: [`QueueItem`] (in
//! [`Store::queue_loaded`]) and [`Severity`] (in `severity_of`). Both are
//! VOCABULARY rather than presentation — a shape and an honesty level, with no
//! geometry in either — and the queue arrives as opaque JSON whose honest
//! landing place is where it lands. It is recorded here because a boundary
//! with a silent exception is a boundary nobody trusts twice.
//!
//! `0023` `what_the_tui_stores` is a binding list, and its review rule is the
//! one to hold this file to: *if a datum can be asked of the daemon in ONE RPC,
//! holding it beyond cache lifetime is a defect.* The daemon's own schema goes
//! further — `schema.sql` records columns it deliberately DROPPED so that
//! clients could not cache derivable state, and there is a comment banning a
//! `status` column outright because status is derived fleet-wide from locks,
//! parked turns, retries and the queue. A client that reintroduces those
//! columns in RAM has done the thing the daemon refused to do in SQL.
//!
//! # What this file holds
//!
//! * **The link state.** Client-authoritative by construction: only this
//!   process knows whether this process has a socket.
//! * **A droppable FLEET cache** — one [`ThreadFacts`] per thread in the
//!   painted WINDOW, merged from `threads/list` (identity: cwd, model, account)
//!   and `threads/status` (derived: state, queued, unread, last_seq). Held only
//!   because `Status` is EDGE-TRIGGERED: it does not repeat, so between edges
//!   the last value is the only copy there is. Dropped wholesale by
//!   [`Store::invalidate`], which every reconnect and every `Lagged` triggers,
//!   and re-fetched with one call. That is a cache, not a store.
//! * **The OPEN thread's ledger tail**, bounded at [`OPEN_CAP`] and keyed by
//!   `seq`. Not a transcript, not a ledger, not a source of truth —
//!   `threads/{id}/history` is all three and it is one call away.
//! * **A bounded fleet ACTIVITY tail** ([`TAIL_CAP`]) — the firehose, for the
//!   view that shows every thread at once.
//!
//! **The settings blob is DROPPED ON INGEST.** `threads/list` rows carry the
//! thread's whole settings object, and for these threads that includes the
//! entire appended system prompt — measured, kilobytes per row, a thousand rows
//! a paint. Nothing on screen reads it. Keeping it would be the same defect as
//! caching a derivable count, just larger.
//!
//! # What this file REFUSES to hold, and why each one is the daemon's
//!
//! messages (the ledger, `threads/{id}/history`), the queue (the `queue` table,
//! `threads/{id}/queue`), unread counts (`thread_seen`, `threads/{id}/seen`), thread
//! status as a durable fact (derived fleet-wide), retries and failures
//! (`retries`, `threads/{id}/queue_failed`), account health (`accounts.*`), other
//! instances' dirty flags (thread metadata), and **any byte on disk beyond the
//! log**. The per-thread delivered-seq cursors ARE client state and are the one
//! exception the protocol itself mandates — which is why they live in
//! [`crate::transport`], next to the socket that gives them meaning, and not
//! here.

use std::collections::{BTreeMap, HashMap, VecDeque};

use chrono::{DateTime, TimeDelta, Utc};
use serde_json::Value;
use tui_render::model::{
    ArchivedRow, ArchivedView, Avail, Facts, ProjectFacts, SheetAct, SheetFacts, SheetField,
    SheetRow, SheetView,
};
use tui_render::model::{
    Block, BlockId, Fetch, Known, ProjectList, ProjectRow, QueueItem, QueueView,
    Sender as Who, ThreadList, ThreadRow, BAND_ROWS,
};
use tui_render::{AgentTree, Banner, LinkState, Severity, SubagentState, TreeId, TreeRow};

use crate::agents::SubagentRow;
use crate::index::{self, Project};
use crate::proto::{ServerMsg, ThreadEvent};
use crate::transport::{NoteLevel, TransportEvent};

/// How many ledger rows the FLEET activity tail keeps.
///
/// A bound rather than a policy argument: this is the firehose across every
/// thread the daemon knows, and `stream_event` alone was measured at most of a
/// busy thread's ledger. An unbounded tail would be a memory leak that only
/// shows up on the machine of the person who leaves the face open longest —
/// which is him.
pub const TAIL_CAP: usize = 2_000;

/// How many ledger rows one OPEN thread keeps, and therefore how many
/// `threads/{id}/history` asks for.
///
/// Larger than the fleet tail because it is ONE thread and it is the thing he
/// is actually reading. When it overflows the OLDEST rows go, the count of what
/// went is rendered, and `threads/{id}/history` still has all of it.
pub const OPEN_CAP: usize = 4_000;

/// **THE FLEET WINDOW — the paging decision, in one constant.**
///
/// `threads/status` (no `thread_id`) and `threads/list` are both
/// `list_threads(limit)`, which is `ORDER BY updated_at DESC LIMIT n`
/// (`store.rs::list_threads`). So a full page is not a truncated fleet: it is
/// **the n most recently touched threads**, which is a set with a name.
///
/// Both calls carry this same number, which is `0031`'s fix kept intact — the
/// two used to default to 200 and 100 and the header read whichever landed
/// last. What CHANGED is what the number is allowed to mean: see
/// [`Store::window`] and `header`.
pub const FLEET_WINDOW: u64 = 1000;

/// **HOW MANY STATUSES TO ASK FOR, and it is a DIFFERENT number now.**
///
/// `threads/status` and `threads/list` are both `ORDER BY updated_at DESC LIMIT
/// n`, and his daemon holds 1,849 threads — so at 1,000 the page silently omits
/// the 849 least-recently-updated, which is most of his own history. `0035`
/// filed that as a pre-existing AUI defect (`resync_status` passes the same
/// 1,000) and warned against copying the constant. The first live run of this
/// build showed exactly why: 36 of 39 rows in `harness` reported no daemon row
/// at all, because their ids were outside the window rather than outside the
/// daemon.
///
/// It is raised for STATUS ONLY, deliberately. A `threads/status` row is eight
/// small scalars; a `threads/list` row carries the thread's whole settings
/// object, which for these threads is the entire appended system prompt —
/// kilobytes apiece, measured, and dropped on ingest because nothing on screen
/// reads it. Asking for five thousand of THOSE to decorate a cwd fallback would
/// trade a real payload for a field the summary has already replaced.
///
/// So: statuses wide enough to cover the fleet, identity left at the window it
/// always had. [`FLEET_WINDOW`] still governs `threads/list` and still means
/// what it meant.
pub const STATUS_WINDOW: u64 = 10_000;

/// One ledger row, as held.
#[derive(Debug, Clone)]
pub struct LedgerRow {
    pub seq: u64,
    pub message_id: Option<u64>,
    /// The daemon's own timestamp, when it gave us one we could read. `None`
    /// renders `?` — never [`LedgerRow::received`], because an arrival time
    /// passed off as an event time is honesty rule 1 wearing a plausible
    /// number.
    pub ts: Option<DateTime<Utc>>,
    pub received: DateTime<Utc>,
    pub event: ThreadEvent,
}

/// Everything the fleet list needs about one thread, merged from the two calls
/// that know different halves of it.
#[derive(Debug, Clone, Default)]
pub struct ThreadFacts {
    /// From `threads/list`. The settings blob is NOT kept — see the header.
    pub cwd: String,
    pub model: String,
    pub account: String,
    pub updated: Option<DateTime<Utc>>,
    /// From either call; the daemon derives it once so the two cannot disagree.
    pub state: String,
    pub executing: bool,
    /// From `threads/status` ONLY. The `status` PUSH does not carry these two —
    /// measured at the source (`runtime.rs::emit_status` builds the frame from
    /// five of `status_json`'s eight keys) — so a thread whose only news came
    /// from a push keeps [`Known::Unknown`] here rather than a zero.
    pub queued: Known<u32>,
    pub unread: Known<u32>,
    /// The ledger's high-water mark, which is how the open-thread fetch finds
    /// the TAIL of a long thread. `threads/status` carries it; the push does
    /// not.
    pub last_seq: Option<u64>,
    /// **WHEN HE LAST SPOKE HERE, FROM THE DAEMON** — `threads/recent-updates`'
    /// `at`, which moves only on a `user_message`. Since the threads move the
    /// per-project indexes are no longer written on his turns, so this is the
    /// live half of the recents band's key (`row_of` takes the newer of it and
    /// the index's `last_user_input_at`). Sticky: a thread that falls out of the
    /// daemon's 32-entry window keeps the last stamp it was seen with.
    pub last_user_input: Option<DateTime<Utc>>,
    /// The daemon's title (`threads/summary`), for a recent thread that no
    /// project index carries a header for.
    pub title: Option<String>,
    /// `status.detail` — WHY a parked or retrying thread is where it is, in
    /// the daemon's words. Moves with `state`.
    pub detail: Option<String>,
    /// **THIS THREAD'S PARENT**, when it is a managed subagent — the
    /// `threads.parent_id` cell. Immutable, so sticky.
    pub parent_id: Option<String>,
    /// **THE MANAGED CHILDREN THIS THREAD SPAWNED** — the `threads.children`
    /// cell. `None` until a frame carried it.
    pub children: Option<Children>,
    /// **THE DAEMON'S ROLLED-UP STATUS, READY TO DRAW** — the `threads.rolled`
    /// cell (R9.TS1). `None` until a frame carried it. The tree draws its
    /// `display` and `children_summary` VERBATIM (TUI-P1b): nothing about a
    /// status is computed in this face.
    pub rolled: Option<RolledFacts>,
    /// **THE THREAD'S NAME, LIVE** (TUI-P1a) — the newest `summary_updated`
    /// payload's `summary`, read off the `threads` row's `app` cell for every
    /// key the window holds, else `threads/summary`'s title. The cell first,
    /// because a keyless summary subscriber does not hear about a thread named
    /// for the FIRST time (`services/threads/src/shapes/summary.rs`, "one known
    /// gap") — and a rename while he is looking at the thread has to land
    /// without a re-subscribe. `None` until a frame carried one.
    pub name: Option<String>,
    /// **WHEN A MANUAL NAME WAS GIVEN THROUGH THE DAEMON** — set only when the
    /// newest `summary_updated` payload says `manual: true` (a rename from this
    /// face, or a name given at creation). Compared against the AUI's own
    /// manual-summary lock in [`Store::label_of`].
    pub name_manual_at: Option<DateTime<Utc>>,
    /// **WHICH APPLICATION STARTED THIS THREAD** — `settings.app_id`, the one
    /// field of the settings blob this store keeps (TUI-P1a). `None` is "not
    /// attributed to any app", which is NOT one of his faces. See
    /// [`is_his_app`].
    pub app_id: Option<String>,
    /// **AN AGENT'S TAG** (TUI-P1a) — `settings.metadata.agent`, set at
    /// creation by whatever made the thread for an agent (the daemon's spawn
    /// path, a proof script). A tagged thread is never his, whatever `app_id`
    /// it copied.
    pub agent: Option<String>,
    /// **THE FIRST LINE OF THE THREAD'S FIRST MESSAGE** (TUI-P1a), trimmed and
    /// bounded — the name of a thread nobody named. Read once off the thread's
    /// first message page and never re-derived.
    pub first_line: Option<String>,
}

/// **ONE THREAD'S `rolled` CELL** (R9.TS1) — the fields the tree draws.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RolledFacts {
    /// `active` | `idle` | `error`.
    pub status: String,
    /// `active (+2)`, `active`, `idle`, `error: parked` — the ONE display
    /// string, from `shapewire::rolled::display` on the producer.
    pub display: String,
    /// Descendants at every depth that are running themselves.
    pub active_descendants: u64,
    /// `N active, M idle` over DIRECT children; `None` with no children.
    pub children_summary: Option<String>,
}

impl RolledFacts {
    /// Parse the cell. `None` for anything without its required `status` and
    /// `display`, so a malformed cell reads as no answer.
    pub fn parse(v: &Value) -> Option<RolledFacts> {
        Some(RolledFacts {
            status: v.get("status")?.as_str()?.to_string(),
            display: v.get("display")?.as_str()?.to_string(),
            active_descendants: v.get("active_descendants").and_then(Value::as_u64).unwrap_or(0),
            children_summary: v
                .get("children_summary")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }
}

/// **THE `app_id`s OF HIS FACES** — the apps a thread HE started carries:
/// `harness-tui` is what this face stamps (`acts::route_new_thread`, ledger
/// R4.S7 / IS-27), `tui` the spelling the daemon also accepts, and `aui` the
/// audio interface's — the same three the daemon gives the context id to by
/// default (`services/threads/src/settings.rs`, `CONTEXT_ID_BY_DEFAULT_FOR`,
/// on his words *"I mean threads started from AUI and TUI"*).
///
/// **THE RULING** (an internal session, 2026-10-20; an internal report
/// TC-05): *"there are a bunch of temp and var threads that have made it into
/// my recent threads. They shouldn't be there. I shouldn't be able to see any
/// of those unless they were actually created by the Tui. So in the metadata of
/// the thread, we need to have an indicator that it was created by the Tui."*
/// The checklist's target reads it as *"the TUI (or AUI)"*, and the census
/// settles it: the AUI's own threads since the threads move carry `aui` and
/// NO index header (144 `aui` threads in the prod store, in `harness`, `globs2`,
/// `miscellaneous`), so leaving `aui` out would empty his recents of the
/// threads he is working in. The temp and var threads are un-attributed
/// (`app_id` NULL: 45 under `/var`, `/private`, `/tmp`), and that is what the
/// filter removes.
pub const HIS_APPS: &[&str] = &["aui", "tui", "harness-tui"];

/// **DID HE START THIS THREAD, FROM ONE OF HIS FACES?** One of his faces'
/// `app_id`s, and no agent tag. A thread no app claimed (`None`) is not his:
/// it is how a script or an agent calling the daemon directly looks (4,108
/// un-attributed rows in the prod store, TUI-P1a census), and an agent's tag
/// wins over an `app_id` it copied from its parent.
pub fn is_his_app(app_id: Option<&str>, agent: Option<&str>) -> bool {
    agent.map_or(true, |a| a.is_empty()) && app_id.is_some_and(|a| HIS_APPS.contains(&a))
}

/// **THE SCRATCH ROOTS** — where a test or a proof makes a throwaway working
/// directory: the system temp dirs. He never works in one, and he named them:
/// *"there are a bunch of temp and var threads that have made it into my
/// recent threads. They shouldn't be there."* (523f67d3 L145).
pub const SCRATCH_ROOTS: &[&str] = &["/tmp/", "/private/tmp/", "/var/folders/", "/private/var/folders/"];

/// **IS THIS CWD A SCRATCH DIRECTORY?** (TUI-P1a) The rule that hides the
/// proof threads made BEFORE agents tagged their threads — they copied one of
/// his apps' `app_id` and carry no tag, so `is_his_app` alone lets them
/// through. Read-only census, 2026-10-26: of his apps' spoken-in threads, the
/// five in a scratch root are all proofs (`d19dee63` aui-threads-proof,
/// `c55920a4`/`225353dd` r24-proof, `66556cd8`/`06ab8e19` r26-mcp-proof) and
/// none of the other 147 is in one.
pub fn in_scratch_dir(cwd: &str) -> bool {
    SCRATCH_ROOTS.iter().any(|r| cwd.starts_with(r))
}

/// **IS THIS ONE OF HIS THREADS?** His app, no agent tag, and not run from a
/// scratch directory.
pub fn is_his(app_id: Option<&str>, agent: Option<&str>, cwd: &str) -> bool {
    is_his_app(app_id, agent) && !in_scratch_dir(cwd)
}

/// **A THREAD'S NAME FROM ITS FIRST MESSAGE** — the first non-blank line,
/// trimmed, at most [`NAME_MAX`] characters with an ellipsis. `None` for
/// nothing to say.
pub fn first_line_of(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(clip(line, NAME_MAX))
}

/// How long a derived name may be on a row.
pub const NAME_MAX: usize = 60;

/// What a thread with no name and no first message is called.
pub const UNTITLED: &str = "untitled";

/// **A PARENT'S MANAGED CHILDREN, AS THE DAEMON FOLDS THEM** — lineage only.
/// Each child's live state is on the child's own row, which has one owner.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Children {
    /// The children the parent is still owed a report from — the daemon's own
    /// fold, `wake: never` children left out. Never re-derived here.
    pub outstanding: Vec<String>,
    /// Every committed spawn, in spawn order.
    pub runs: Vec<ChildRun>,
}

/// One committed spawn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChildRun {
    pub id: String,
    /// The agent type it was spawned as (`core:curator-v5`).
    pub agent: Option<String>,
    /// The spawn's `name`, when one was given.
    pub name: Option<String>,
    /// `wake: never` — the parent is not waiting on this one.
    pub no_wake: bool,
    pub started_at: Option<DateTime<Utc>>,
    /// `ok | failed | interrupted`, with when — `None` while unreported.
    pub finished: Option<(String, Option<DateTime<Utc>>)>,
}

impl Children {
    /// Parse the cell's value. `None` for anything that is not the declared
    /// object, so a malformed cell reads as no answer rather than no children.
    pub fn parse(v: &Value) -> Option<Children> {
        let outstanding = v
            .get("outstanding")?
            .as_array()?
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect();
        let runs = v
            .get("runs")?
            .as_array()?
            .iter()
            .filter_map(|r| {
                Some(ChildRun {
                    id: r.get("id")?.as_str()?.to_string(),
                    agent: r.get("agent").and_then(Value::as_str).map(str::to_string),
                    name: r.get("name").and_then(Value::as_str).map(str::to_string),
                    no_wake: r.get("wake").and_then(Value::as_str) == Some("never"),
                    started_at: r.get("started_at").and_then(Value::as_str).and_then(parse_ts),
                    finished: r.get("finished").filter(|f| !f.is_null()).map(|f| {
                        (
                            f.get("outcome").and_then(Value::as_str).unwrap_or("").to_string(),
                            f.get("at").and_then(Value::as_str).and_then(parse_ts),
                        )
                    }),
                })
            })
            .collect();
        Some(Children { outstanding, runs })
    }

    /// **THE LAST BATCH** — the runs that finished after the newest spawn
    /// started, which is what one wake reports. A count for `resuming · N
    /// reports in`, not a history.
    pub fn last_batch_reported(&self) -> usize {
        let newest = self.runs.iter().filter_map(|r| r.started_at).max();
        self.runs
            .iter()
            .filter(|r| !r.no_wake)
            .filter(|r| match (&r.finished, newest) {
                (Some((_, Some(at))), Some(n)) => *at >= n,
                (Some(_), None) => true,
                _ => false,
            })
            .count()
    }
}

/// **ONE PHRASE ABOUT WHAT A THREAD IS DOING** — the long form for the
/// header and the tree, the short form for a menu row's state column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    pub long: String,
    pub short: String,
    pub severity: Severity,
}

/// The open thread's loaded tail.
#[derive(Debug)]
pub struct Open {
    pub id: String,
    /// By `seq`, which is the ledger's own order AND its identity — so a
    /// replayed row and the live push of the same event collapse into one
    /// entry with no dedupe pass to get wrong.
    pub rows: BTreeMap<u64, LedgerRow>,
    pub fetch: Fetch,
    /// Rows dropped off the bottom of [`OPEN_CAP`].
    pub forgotten: u64,
    /// The lowest seq the load reached. `Some(1)` or lower means the whole
    /// thread is here.
    pub from_seq: Option<u64>,
}

#[derive(Debug)]
pub struct Store {
    pub link: LinkState,
    /// **WHEN THE LINK WENT DOWN — `None` while it is up.**
    ///
    /// The shell's half of the one fact `0040` §9 draws and nothing tracked:
    /// how long the daemon has actually been gone. It is set on the FIRST
    /// `Disconnected` of an outage and survives every `Connecting` hop of the
    /// reconnect ladder, so it measures the OUTAGE and not the last attempt;
    /// only a connection that comes all the way up clears it. See
    /// [`tui_render::Link::down_since`], which is where it is read.
    pub down_since: Option<DateTime<Utc>>,
    /// **HE HAS NEVER RUN THIS PROGRAM BEFORE** (`0040` §10).
    ///
    /// Resolved ONCE, in `main`, from whether `~/.tui` existed before the log
    /// opened it — see [`crate::first_run`], which owns the reasoning and the
    /// reason there is no marker file. Ambient state a pure layer may not read
    /// for itself, so it arrives here as a value, exactly as `$HOME` and the
    /// timezone do.
    pub first_run: bool,
    /// **THE DAEMON'S COUNT OF ITS OWN WARNINGS SINCE ITS BOOT**, and how many of
    /// them were errors.
    ///
    /// `Known::Unknown` until a `threads/status` reply carries them, which covers
    /// two states that must never be painted as zero: we have not read the fleet
    /// yet, and this daemon is too old to report. A daemon that has not told us it
    /// is healthy has not told us it is healthy.
    ///
    /// A SNAPSHOT and not a live number — this face is on the request/response
    /// door and consumes no shapes, so the count is as fresh as its last fleet
    /// read. `DIR-7` forbids adding a poll for it and none is added.
    pub daemon_warnings: Known<u32>,
    /// The ERROR half of [`Store::daemon_warnings`]. Apart, because the producer
    /// keeps it apart and an error is a different statement from a warning.
    pub daemon_warning_errors: Known<u32>,
    pub endpoint: String,
    /// The daemon we are talking to, when we have been told.
    pub boot_id: Option<String>,
    /// `$HOME`, resolved once by the shell, for shortening a cwd. The render
    /// crate cannot do this: it has no filesystem and no environment.
    home: String,
    /// **DROPPABLE.** `None` means "we have not been told", never "empty".
    ///
    /// **This is now a DECORATION cache and nothing else.** It used to be the
    /// thread list; it is now the set of daemon facts that get joined, by id,
    /// onto rows the INDEXES produced. A thread in here that is not in an index
    /// never reaches the screen — that is `IS-20`'s fix, and it is a change of
    /// QUESTION, not a filter (see [`crate::index`]).
    threads: Option<BTreeMap<String, ThreadFacts>>,
    /// The page filled the window we asked for, so the window is a WINDOW.
    window_full: bool,
    /// **THE STATUS page filled ITS window**, which is a different and much
    /// larger one ([`STATUS_WINDOW`]). Tracked apart from `window_full`
    /// because it is the one that decides whether `absent` is a fact: with the
    /// status page truncated, "the daemon has no row for this id" and "the id
    /// fell outside the page" are indistinguishable, and only one of them is
    /// true of a thread of his.
    status_full: bool,
    /// **THE LIST'S REAL SOURCE.** Every root the human faces own, with its
    /// `.mind/threads.json` already read. Loaded from the filesystem at
    /// startup and on `r`; never written.
    projects: Vec<Project>,
    /// False until the filesystem has been read once, so the root menu can say
    /// "reading" rather than "none".
    projects_loaded: bool,
    /// **HIS ARCHIVE SET** — the projects `~/.aui/archived-projects.json`
    /// names, with when he archived each.
    ///
    /// It was a COUNT while the root's chrome row was a tally. It is the set
    /// itself now that the row is a DOOR: the label and the room behind it are
    /// read off one value, so they cannot say different things. Drawn even at
    /// zero. Fail-open: an unreadable archive file is an EMPTY set, which hides
    /// nothing and counts nothing.
    archived: Vec<index::Archived>,
    /// **WHERE THE PROJECT LIST CAME FROM ON THE LAST READ.**
    ///
    /// Held rather than re-derived because the only surface that needs it is
    /// the banner, which runs once per PROJECTION and must not stat a file to
    /// draw a line. `Read` until the first `load_projects`, which is the
    /// honest default for a fixture: an adopted list is not a claim about a
    /// file nobody opened.
    registry: index::RegistryState,
    /// Which project's threads [`Mode::List`] is showing. `None` at the root.
    open_project: Option<usize>,
    /// Every id every index answers to. Derived from [`Store::projects`] on
    /// load and held because the event lane tests it per arriving event.
    owned: std::collections::HashSet<String>,
    open: Option<Open>,
    queue: QueueView,
    tail: VecDeque<(String, LedgerRow)>,
    forgotten: u64,
}

/// What one applied event changed, per VIEW — because the "N new" chip is a
/// promise about the view he is looking at, and an event on some other thread
/// is not new to the thread he has open.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Applied {
    pub activity_grew: bool,
    pub open_grew: bool,
}

impl Store {
    pub fn new(endpoint: String, home: String) -> Self {
        Self {
            link: LinkState::Connecting { attempt: 1 },
            down_since: None,
            first_run: false,
            // UNKNOWN, not zero. A store that has not read the fleet has not been
            // told the daemon is healthy.
            daemon_warnings: Known::Unknown,
            daemon_warning_errors: Known::Unknown,
            endpoint,
            boot_id: None,
            home,
            threads: None,
            window_full: false,
            status_full: false,
            projects: Vec::new(),
            projects_loaded: false,
            archived: Vec::new(),
            registry: index::RegistryState::default(),
            open_project: None,
            owned: std::collections::HashSet::new(),
            open: None,
            queue: QueueView::default(),
            tail: VecDeque::new(),
            forgotten: 0,
        }
    }

    // ── the projects ─────────────────────────────────────────────────────

    /// **READ EVERY ROOT'S INDEX.** The one place the filesystem is touched,
    /// and it is a READ: `.mind/threads.json` is the AUI's private write
    /// surface (see [`crate::index`]) and this face never writes it.
    ///
    /// `home` empty means there is nowhere to look, which degrades to no
    /// projects rather than to a panic — the same honest degradation the
    /// cwd-shortening takes.
    pub fn load_projects(&mut self) {
        if self.home.is_empty() {
            self.projects_loaded = true;
            return;
        }
        let (projects, archived, registry) = index::load_projects(&self.home);
        let archived_n = archived.len();
        self.archived = archived;
        self.registry = registry;
        self.adopt_projects(projects);
        tracing::info!(
            roots = self.projects.len(),
            archived = archived_n,
            registry = ?self.registry,
            threads = self.owned_count(),
            ids = self.owned.len(),
            "read the project indexes"
        );
    }

    /// Take a set of roots as read. Split out of [`Store::load_projects`] so
    /// the membership set is derived in ONE place whether the roots came off
    /// the filesystem or out of a fixture — a test that built `owned` its own
    /// way would be proving its own arithmetic.
    pub fn adopt_projects(&mut self, projects: Vec<Project>) {
        self.owned = index::owned_ids(&projects);
        self.projects = projects;
        self.projects_loaded = true;
    }

    /// Take an ARCHIVE SET as read — [`Store::adopt_projects`]'s twin, and it
    /// exists for the same reason: the set reaches the store from the
    /// filesystem or from a fixture, and a test that reached around this door
    /// would be proving its own arithmetic rather than the one the room draws.
    pub fn adopt_archived(&mut self, archived: Vec<index::Archived>) {
        self.archived = archived;
    }

    /// **THE MEMBERSHIP TEST.** Whether this id appears in any root's index.
    ///
    /// Note the fallback: with NO indexes read at all, everything is owned.
    /// That is deliberate and it is the honest degradation — a face that
    /// silently showed nothing because `$HOME` was unreadable would look
    /// identical to a daemon with no threads.
    pub fn owns(&self, thread_id: &str) -> bool {
        self.owned.is_empty() || self.owned.contains(thread_id)
    }

    pub fn projects(&self) -> &[Project] {
        &self.projects
    }

    pub fn open_project(&self) -> Option<&Project> {
        self.open_project.and_then(|i| self.projects.get(i))
    }

    pub fn enter_project(&mut self, i: usize) {
        self.open_project = (i < self.projects.len()).then_some(i);
    }

    pub fn leave_project(&mut self) {
        self.open_project = None;
    }

    /// Which project a thread id belongs to, by index. Retained because the
    /// AUI's pin mutations are keyed by `{project, thread_id}` and `project` is
    /// on no header — so a row has to remember where it was read from.
    pub fn project_of(&self, thread_id: &str) -> Option<&str> {
        self.projects
            .iter()
            .find(|p| p.threads.iter().any(|t| t.id == thread_id))
            .map(|p| p.name.as_str())
    }

    /// **DROP EVERY CURRENT-STATE FACT.**
    ///
    /// The transcript is NOT touched: it is history, and history stays true
    /// across a disconnect. Only the things that claim to describe *now* are
    /// dropped, and they go back to [`Known::Unknown`] rather than keeping
    /// their last value — because the honesty rule is "never render stale as
    /// current", and a queue depth from before a daemon restart is stale in the
    /// exact way that reads as current.
    pub fn invalidate(&mut self, reason: &str) {
        tracing::info!(%reason, "current state invalidated");
        self.threads = None;
        self.window_full = false;
        self.status_full = false;
    }

    // ── the fleet ────────────────────────────────────────────────────────

    fn facts(&mut self, id: &str) -> &mut ThreadFacts {
        self.threads
            .get_or_insert_with(BTreeMap::new)
            .entry(id.to_string())
            .or_default()
    }

    pub fn thread_count(&self) -> Known<u32> {
        match &self.threads {
            None => Known::Unknown,
            Some(m) => Known::Known(m.len() as u32),
        }
    }

    /// Whether the painted page filled its window — which means the page is a
    /// RECENCY WINDOW and not the fleet.
    pub fn window_full(&self) -> bool {
        self.window_full
    }

    /// Whether a missing status row can be BELIEVED. False while the status
    /// page is truncated, in which case a row the daemon did not mention says
    /// `…` (not told yet) rather than drawing blank (V1.9: never `absent`).
    pub fn absence_is_a_fact(&self) -> bool {
        self.threads.is_some() && !self.status_full
    }

    /// Queued messages, scoped. `None` asks across the whole painted window.
    pub fn queued(&self, thread_id: Option<&str>) -> Known<u32> {
        let Some(m) = &self.threads else {
            return Known::Unknown;
        };
        match thread_id {
            Some(id) => m.get(id).map(|f| f.queued).unwrap_or(Known::Unknown),
            None => sum(m.values().map(|f| f.queued)),
        }
    }

    /// **QUEUED ACROSS HIS THREADS, not across the daemon's fleet.**
    ///
    /// `DIR-6` says the queue badge is visible at all times, so it has to be a
    /// number he can act on. Summed over the whole painted window it was the
    /// arena's queue plus his — 1,849 threads' worth of somebody else's work in
    /// a badge with his name on it. The membership set is the same one the list
    /// is built from, so the badge and the rows count the same threads.
    pub fn queued_owned(&self) -> Known<u32> {
        let Some(m) = &self.threads else {
            return Known::Unknown;
        };
        sum(m
            .iter()
            .filter(|(id, _)| self.owns(id))
            .map(|(_, f)| f.queued))
    }

    /// How many threads the indexes claim, across every root. A real count, not
    /// a window: an index is not a page.
    pub fn owned_count(&self) -> usize {
        self.owned_thread_count()
    }

    fn owned_thread_count(&self) -> usize {
        self.projects.iter().map(|p| p.threads.len()).sum()
    }

    /// How many of one project's threads the daemon says are executing.
    pub fn active_in(&self, p: &Project) -> Known<u32> {
        match &self.threads {
            None => Known::Unknown,
            Some(m) => Known::Known(
                p.threads
                    .iter()
                    .filter(|t| m.get(&t.id).is_some_and(|f| f.executing))
                    .count() as u32,
            ),
        }
    }

    /// Unread on every thread EXCEPT the one named. "Elsewhere" means
    /// elsewhere; counting the open thread into it is how a badge stays lit
    /// after he has read the thing it is about. **Scoped to his threads**, for
    /// the same reason [`Store::queued_owned`] is.
    pub fn unread_elsewhere(&self, except: Option<&str>) -> Known<u32> {
        let Some(m) = &self.threads else {
            return Known::Unknown;
        };
        sum(m
            .iter()
            .filter(|(id, _)| Some(id.as_str()) != except && self.owns(id))
            .map(|(_, f)| f.unread))
    }

    /// Executing, **among his threads**.
    pub fn active_threads(&self) -> Known<u32> {
        match &self.threads {
            None => Known::Unknown,
            Some(m) => Known::Known(
                m.iter()
                    .filter(|(id, f)| f.executing && self.owns(id))
                    .count() as u32,
            ),
        }
    }

    pub fn last_seq_of(&self, id: &str) -> Option<u64> {
        self.threads.as_ref()?.get(id)?.last_seq
    }

    /// **HOW A THREAD IS NAMED ON A ROW AND IN ITS HEADER**, and it is never
    /// the hash and never a path. A note row (`thread_id` empty) is ours and
    /// names nothing by design. Otherwise [`Store::name_of`], else `untitled`.
    pub fn label_of(&self, id: &str) -> String {
        if id.is_empty() {
            return String::new();
        }
        clip(&flat(&self.name_of(id).unwrap_or_else(|| UNTITLED.to_string())), 32)
    }

    /// **WHAT A THREAD IS CALLED, IF ANYTHING CALLS IT** (TUI-P1a, checklist
    /// TC-02/TC-03), in this order:
    ///
    /// 1. **The daemon's name** — the newest `summary_updated` ([`ThreadFacts::name`],
    ///    live off the `app` cell, else `threads/summary`'s title). This is
    ///    where a rename lands, from this face or any other, so it is first.
    ///    **One exception:** a name he locked in the AUI (`summary_locked`)
    ///    outranks a daemon name that was not given by a person after it —
    ///    an agent's drifting summary must not displace a name he chose.
    /// 2. **The index's summary** — the AUI's own name for a thread it holds.
    /// 3. **The first line of the first message** — for a thread nobody named.
    ///
    /// `None` when none of the three has anything to say. **The cwd is not on
    /// this list any more**: it was rung two, and *"if a thread wasn't named,
    /// then it just shows the project path, which is the worst possible thing
    /// it could show."*
    pub fn name_of(&self, id: &str) -> Option<String> {
        // The SURVIVING copy of a header that lives in several roots — the
        // newest `last_turn_at`, `all_headers`' own rule — and not whichever
        // root happens to be read first.
        let header = self
            .projects
            .iter()
            .flat_map(|p| p.threads.iter())
            .filter(|t| t.id == id)
            .fold(None::<&index::IndexThread>, |best, t| match best {
                Some(b) if b.last_turn_at >= t.last_turn_at => Some(b),
                _ => Some(t),
            })
            .filter(|t| !t.summary.trim().is_empty());
        let f = self.fact(id);
        let daemon = f
            .and_then(|f| f.name.clone().or_else(|| f.title.clone()))
            .filter(|n| !n.trim().is_empty());
        if let Some(h) = header.filter(|h| h.summary_locked) {
            let manual_after = f
                .and_then(|f| f.name_manual_at)
                .is_some_and(|at| h.summary_set_at.map_or(true, |locked| at > locked));
            return Some(match daemon {
                Some(n) if manual_after => n,
                _ => h.summary.clone(),
            });
        }
        daemon
            .or_else(|| header.map(|h| h.summary.clone()))
            .or_else(|| f.and_then(|f| f.first_line.clone()))
    }

    /// **A PARENT'S NAME FOR A CHILD'S ROW** — the index summary, else the
    /// daemon's title, else `a thread`. Never the hash.
    pub fn parent_label(&self, id: &str) -> String {
        let l = self.label_of(id);
        if !l.is_empty() {
            return l;
        }
        self.fact(id)
            .and_then(|f| f.title.as_deref())
            .map(|t| clip(&flat(t), 32))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "a thread".to_string())
    }

    /// The daemon facts held for one thread, if any.
    pub fn fact(&self, id: &str) -> Option<&ThreadFacts> {
        self.threads.as_ref().and_then(|m| m.get(id))
    }

    /// **UI-b FOR ONE THREAD, AS THE DAEMON ANSWERED IT** (R9.TS1, TUI-P1c) —
    /// the `threads.rolled` cell is the ONLY source of a subtree's status. A
    /// thread is `SelfActive` when it is itself active, `SubtreeActive` when
    /// the daemon's `rolled.status` is `active`, else its own word (an error
    /// does not roll up).
    ///
    /// **NO FACE-SIDE ROLLUP.** Until TUI-P1c a thread with no `rolled` cell
    /// was answered by walking the `children` cells this store held and
    /// re-running `shapewire::subtree::rollup` over them (and over the
    /// mirror's old `rollup` word, which the mirror stopped sending at
    /// TUI-P1b2). That was a second copy of the rule over a partial window —
    /// the store holds at most the mirror's window of descendants — and it is
    /// deleted: with no `rolled` cell yet, the thread shows its own word.
    /// `None` when this store holds no facts for `id`.
    pub fn subtree_rollup(&self, id: &str) -> Option<shapewire::subtree::Rollup> {
        use shapewire::subtree::Rollup;
        let f = self.fact(id)?;
        Some(if shapewire::subtree::is_active(&f.state, f.executing) {
            Rollup::SelfActive
        } else if f.rolled.as_ref().is_some_and(|r| r.status == "active") {
            Rollup::SubtreeActive
        } else {
            Rollup::Own
        })
    }

    /// **THE ONE STATE WORD EVERY FACE OF THIS STORE DRAWS FOR A THREAD**
    /// (R9.UI1b) — the recents and pinned rows, the project list, the thread
    /// header, a child's tree row. [`Store::subtree_rollup`]'s word: the
    /// subtree's answer (UI-b: a thread whose turn ended shows `active` while
    /// any descendant runs, else `parked` while any is parked), the thread's
    /// own word otherwise. `None` when no facts are held for `id`; an empty
    /// string when the daemon has not said a state.
    pub fn shown_word(&self, id: &str) -> Option<String> {
        let f = self.fact(id)?;
        let r = self.subtree_rollup(id)?;
        Some(r.word(&f.state).to_string())
    }

    /// **A MANAGED CHILD'S NAME** — `agent · name`, the agent alone, or the
    /// daemon's title for it. Never its hash (`DIR-16`).
    pub fn child_label(&self, run: &ChildRun) -> String {
        let agent = run.agent.as_deref().map(|a| a.rsplit(':').next().unwrap_or(a));
        match (agent, run.name.as_deref()) {
            (Some(a), Some(n)) => format!("{a} · {}", flat(n)),
            (Some(a), None) => a.to_string(),
            (None, Some(n)) => flat(n),
            (None, None) => self
                .fact(&run.id)
                .and_then(|f| f.title.clone())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| "subagent".to_string()),
        }
    }

    /// **ONE CHILD'S STATE** (TUI-SUBAGENTS §3.3) — the parent's `finished`
    /// first, because a report is final; otherwise the child's own `status`.
    /// Returns the word and, for a parked or retrying child, its reason.
    pub fn child_state(&self, kids: &Children, run: &ChildRun) -> (SubagentState, Option<String>) {
        if let Some((outcome, _)) = &run.finished {
            return (
                match outcome.as_str() {
                    "ok" => SubagentState::Done,
                    "failed" => SubagentState::Failed,
                    "interrupted" => SubagentState::Interrupted,
                    _ => SubagentState::Unknown,
                },
                None,
            );
        }
        let owed = kids.outstanding.iter().any(|o| o == &run.id);
        // UI-b: a child is itself a parent of its own subagents, so its row
        // reads the rolled-up word, the same one its header does.
        let shown = self.shown_word(&run.id).unwrap_or_default();
        match self.fact(&run.id) {
            Some(f) if f.executing || shown == "active" || shown == "queued" => {
                (SubagentState::Running, None)
            }
            Some(f) if shown == "parked" => (SubagentState::Parked, f.detail.clone()),
            Some(f) if shown == "retrying" => (SubagentState::Retrying, f.detail.clone()),
            // **STOPPED, AND ITS PARENT NEVER HEARD.** Idle, nothing queued,
            // no report — and the parent still lists it as owed.
            Some(f) if f.state == "idle" && owed && matches!(f.queued, Known::Known(0) | Known::Unknown) => {
                (SubagentState::Unreported, None)
            }
            // A `wake: never` child that went idle has finished its run; the
            // daemon records no report for it, by design.
            Some(f) if f.state == "idle" && run.no_wake => (SubagentState::Done, None),
            // Not held yet, and the daemon says the parent is owed it: the
            // daemon's own fold is the best word we have.
            _ if owed => (SubagentState::Running, None),
            _ => (SubagentState::Unknown, None),
        }
    }

    /// **WHAT A THREAD IS DOING, IN ONE PHRASE** (TUI-SUBAGENTS §3.2) — or
    /// `None` when its own state word already tells the truth.
    ///
    /// It exists for one case: a parent whose turn ENDED while the subagents
    /// it spawned still run. Its `status` says `idle` and `executing: false`,
    /// so every face used to draw it as finished. The face derives the phrase
    /// from the parent's `children` cell and each child's own row; the daemon
    /// carries no `waiting` state word, deliberately.
    pub fn activity(&self, id: &str) -> Option<Activity> {
        let f = self.fact(id)?;
        let kids = f.children.as_ref();
        let waiting: Vec<&ChildRun> = kids
            .map(|k| {
                k.runs
                    .iter()
                    .filter(|r| k.outstanding.iter().any(|o| o == &r.id))
                    .collect()
            })
            .unwrap_or_default();
        // An id the daemon lists as owed that no run carries still counts.
        let owed = kids.map(|k| k.outstanding.len()).unwrap_or(0).max(waiting.len());
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        if f.executing || f.state == "active" {
            if owed == 0 {
                return None;
            }
            return Some(Activity {
                long: format!("working · {owed} subagent{} out", plural(owed)),
                short: format!("▶ {owed} out"),
                severity: Severity::Calm,
            });
        }
        if f.state == "queued" && owed == 0 {
            let n = kids.map(Children::last_batch_reported).unwrap_or(0);
            if n > 0 {
                return Some(Activity {
                    long: format!("resuming · {n} report{} in", plural(n)),
                    short: "resuming".to_string(),
                    severity: Severity::Calm,
                });
            }
        }
        if f.state == "parked" || f.state == "retrying" || owed == 0 {
            return None;
        }
        let k = kids?;
        let (mut parked, mut unreported) = (0, 0);
        for r in &waiting {
            match self.child_state(k, r).0 {
                SubagentState::Parked | SubagentState::Retrying => parked += 1,
                SubagentState::Unreported => unreported += 1,
                _ => {}
            }
        }
        // **THE BATCH** — every wake-end spawn since the oldest one still
        // owed, so `waiting on 3 of 5` counts down as reports land.
        let oldest_owed = waiting.iter().filter_map(|r| r.started_at).min();
        let batch = match oldest_owed {
            Some(t0) => k
                .runs
                .iter()
                .filter(|r| !r.no_wake && r.started_at.is_some_and(|t| t >= t0))
                .count(),
            None => owed,
        };
        let mut long = if batch > owed {
            format!("waiting on {owed} of {batch} subagents")
        } else {
            format!("waiting on {owed} subagent{}", plural(owed))
        };
        let mut short = format!("⋯ waiting {owed}");
        // `held`, not `parked` (R9.UI1c, ledger R9.UI1b-1): this counts DIRECT
        // owed children that are parked OR retrying, while the colour and every
        // state word follow the subtree rollup (where retrying is active). The
        // word must not claim the rollup's `parked` beside a calm colour.
        if parked > 0 {
            long.push_str(&format!(" · {parked} held"));
            short.push_str(&format!(" · ◆{parked}"));
        }
        // UI-b decides the colour, on EVERY path (R9.UI1b): anything active
        // anywhere below is calm, parked with nothing active is a warning —
        // including a parked GRANDCHILD, which the direct-children count above
        // cannot see. The count above is text only; it no longer sets the
        // colour, because it put parked before active whenever the mirror's
        // rollup was absent or uninformed (R9.UI1-I1).
        let mut severity = match self.subtree_rollup(id) {
            Some(r) if r.parked(&f.state) => Severity::Warn,
            _ => Severity::Calm,
        };
        if unreported > 0 {
            long.push_str(&format!(" · {unreported} ended, unreported"));
            short = format!("! {unreported} unreported");
            severity = Severity::Critical;
        }
        Some(Activity {
            long,
            short,
            severity,
        })
    }

    /// **THE MANAGED CHILDREN OF THE OPEN THREAD, AS TREE ROWS** — depth 1, in
    /// spawn order. Each child's state from §3.3, its reason when parked.
    pub fn managed_rows(&self, id: &str) -> Vec<TreeRow> {
        let Some(k) = self.fact(id).and_then(|f| f.children.as_ref()) else {
            return Vec::new();
        };
        let n = k.runs.len();
        k.runs
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let (state, detail) = self.child_state(k, r);
                // TUI-P1b: the daemon's display string and children summary,
                // verbatim; without the `rolled` cell, the state's word in his
                // vocabulary (see `status_word`).
                let rolled = self.fact(&r.id).and_then(|f| f.rolled.as_ref());
                TreeRow {
                    id: TreeId::Thread(r.id.clone()),
                    depth: 1,
                    last_sibling: i + 1 == n,
                    label: self.child_label(r),
                    state,
                    started: r.started_at,
                    ended: r.finished.as_ref().and_then(|f| f.1),
                    class: tui_render::SubagentClass::Managed,
                    detail,
                    no_wake: r.no_wake,
                    status: rolled
                        .map(|x| x.display.clone())
                        .or_else(|| status_word(state).map(str::to_string)),
                    children_summary: rolled.and_then(|x| x.children_summary.clone()),
                    ..TreeRow::default()
                }
            })
            .collect()
    }

    /// **THE MODEL THE DAEMON REPORTS FOR THIS THREAD**, empty when it has not
    /// told us.
    ///
    /// It is what the LAST TURN ran on. The AUI's per-thread override — what
    /// the NEXT turn will run on — lives in `~/.aui/settings.json`, which this
    /// face never opens, so the two can disagree and this is the half that can
    /// be observed honestly (`0060`'s act table says so in as many words).
    pub fn model_of(&self, id: &str) -> Option<&str> {
        self.threads
            .as_ref()
            .and_then(|m| m.get(id))
            .map(|f| f.model.as_str())
            .filter(|m| !m.is_empty())
    }

    pub fn cwd_of(&self, id: &str) -> String {
        self.threads
            .as_ref()
            .and_then(|m| m.get(id))
            .map(|f| shorten(&f.cwd, &self.home))
            .unwrap_or_default()
    }

    pub fn account_of(&self, id: &str) -> Known<String> {
        match self.threads.as_ref().and_then(|m| m.get(id)) {
            Some(f) if !f.account.is_empty() => Known::Known(match f.model.is_empty() {
                true => f.account.clone(),
                false => format!("{} · {}", f.account, f.model),
            }),
            _ => Known::Unknown,
        }
    }

    // ── the open thread ──────────────────────────────────────────────────

    /// **THE LEDGER'S HIGH-WATER MARK IN THE OPEN THREAD'S RETAINED TAIL** —
    /// what the scroll freeze anchors on. See `App::Frozen::seq`.
    pub fn open_head_seq(&self) -> Option<u64> {
        self.open
            .as_ref()
            .and_then(|o| o.rows.keys().next_back().copied())
    }

    /// Whether the open thread still retains the row a `view` key names
    /// (`seq0000000042`). For the flight recorder's `GONE` lines only.
    pub fn open_has_seq(&self, key: &str) -> bool {
        let Some(seq) = key.strip_prefix("seq").and_then(|n| n.parse::<u64>().ok()) else {
            return false;
        };
        self.open.as_ref().is_some_and(|o| o.rows.contains_key(&seq))
    }

    pub fn open_id(&self) -> Option<&str> {
        self.open.as_ref().map(|o| o.id.as_str())
    }

    pub fn open(&mut self, id: &str) {
        if let Some(o) = self.open.as_ref() {
            crate::diag!(
                "store",
                "REMOVE-ALL thread={} rows={} reason=open({id}) replaces the open thread",
                o.id,
                o.rows.len()
            );
        }
        crate::diag!("store", "OPEN thread={id} rows=0 fetch=loading");
        self.open = Some(Open {
            id: id.to_string(),
            rows: BTreeMap::new(),
            fetch: Fetch::Loading,
            forgotten: 0,
            from_seq: None,
        });
    }

    pub fn close(&mut self) {
        if let Some(o) = self.open.as_ref() {
            crate::diag!(
                "store",
                "REMOVE-ALL thread={} rows={} reason=close()",
                o.id,
                o.rows.len()
            );
        }
        self.open = None;
    }

    /// A page of `threads/{id}/history` for the open thread.
    ///
    /// Ignored when it is for a thread he has since left: a response that
    /// arrives after the view moved on is not news, and applying it would put
    /// another thread's ledger under this thread's name.
    pub fn load_transcript(&mut self, thread_id: &str, rows: Vec<LedgerRow>, from_seq: u64) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        if open.id != thread_id {
            tracing::debug!(%thread_id, open = %open.id, "transcript for a thread we left");
            return;
        }
        let (mut inserted, mut same, mut replaced) = (0usize, 0usize, 0usize);
        for r in rows {
            let line = crate::diag::on().then(|| describe(&r));
            match open.rows.insert(r.seq, r) {
                None => inserted += 1,
                Some(old) => {
                    let now = line.unwrap_or_default();
                    let was = if crate::diag::on() { describe(&old) } else { String::new() };
                    if was == now {
                        same += 1;
                    } else {
                        replaced += 1;
                        crate::diag!(
                            "store",
                            "REPLACE thread={thread_id} reason=transcript load · was [{was}] now [{now}]"
                        );
                    }
                }
            }
        }
        crate::diag!(
            "store",
            "TRANSCRIPT thread={thread_id} from_seq={from_seq} inserted={inserted} \
             unchanged={same} replaced={replaced} total={}",
            open.rows.len()
        );
        open.from_seq = Some(match open.from_seq {
            Some(prev) => prev.min(from_seq),
            None => from_seq,
        });
        open.fetch = Fetch::Loaded;
        trim_open(open);
    }

    /// The seq the open thread's transcript covers from, once loaded.
    pub fn open_from_seq(&self) -> Option<u64> {
        self.open.as_ref().and_then(|o| o.from_seq)
    }

    pub fn transcript_failed(&mut self, thread_id: &str, why: String) {
        if let Some(open) = self.open.as_mut() {
            if open.id == thread_id {
                open.fetch = Fetch::Failed(why);
            }
        }
    }

    // ── the queue overlay ────────────────────────────────────────────────

    pub fn queue(&self) -> &QueueView {
        &self.queue
    }

    /// **THE QUEUE OVERLAY NAMES ITS THREAD THE WAY HE NAMES IT.** A FOURTH
    /// live `DIR-16` surface, and it is the one neither `0051`'s sweep nor
    /// `0073`'s survey named — `0076`'s sweep MACHINE found it by enumerating
    /// the row builders instead of listing them, which is the whole reason the
    /// machine exists. It said `queue · 57db2ec9`.
    ///
    /// `label_of` returning the empty string is already the renderer's
    /// "nothing to name it by" case: the overlay then reads `queue`, which is
    /// what it drew for a thread out of focus before this change.
    pub fn queue_loading(&mut self, thread_id: &str) {
        let thread = self.label_of(thread_id);
        self.queue = QueueView {
            thread,
            waiting: Vec::new(),
            failed: Vec::new(),
            state: Fetch::Loading,
        };
    }

    pub fn queue_loaded(&mut self, thread_id: &str, waiting: &[Value], failed: &[Value]) {
        let thread = self.label_of(thread_id);
        self.queue = QueueView {
            thread,
            waiting: waiting.iter().map(queue_item).collect(),
            failed: failed.iter().map(queue_item).collect(),
            state: Fetch::Loaded,
        };
    }

    pub fn queue_failed(&mut self, thread_id: &str, why: String) {
        let thread = self.label_of(thread_id);
        self.queue = QueueView {
            thread,
            waiting: Vec::new(),
            failed: Vec::new(),
            state: Fetch::Failed(why),
        };
    }

    pub fn queue_idle(&mut self) {
        self.queue = QueueView::default();
    }

    // ── the activity tail ────────────────────────────────────────────────

    pub fn tail_len(&self) -> usize {
        self.tail.len()
    }

    pub fn forgotten(&self) -> u64 {
        self.forgotten
    }

    fn push_tail(&mut self, thread_id: String, row: LedgerRow) {
        if self.tail.len() == TAIL_CAP {
            self.tail.pop_front();
            self.forgotten += 1;
        }
        self.tail.push_back((thread_id, row));
    }

    /// A transport NOTE, which is not a ledger event and does not pretend to
    /// be: it has no seq, it is ours, and it lands in the fleet lane only.
    fn note(&mut self, now: DateTime<Utc>, level: NoteLevel, text: &str) {
        self.push_tail(
            String::new(),
            LedgerRow {
                seq: 0,
                message_id: None,
                ts: Some(now),
                received: now,
                event: ThreadEvent::Custom {
                    event_type: "tui.note".into(),
                    data: serde_json::json!({ "text": text, "level": level_word(level) }),
                },
            },
        );
    }

    // ── applying one transport event ─────────────────────────────────────

    pub fn apply(&mut self, ev: TransportEvent, now: DateTime<Utc>) -> Applied {
        let grew_activity = |b: bool| Applied {
            activity_grew: b,
            open_grew: false,
        };
        match ev {
            TransportEvent::Connecting { attempt } => {
                self.link = LinkState::Connecting { attempt };
                grew_activity(false)
            }
            TransportEvent::Connected { boot_id, resync } => {
                let changed = self.boot_id.as_deref() != Some(boot_id.as_str());
                if changed && self.boot_id.is_some() {
                    tracing::warn!(old = ?self.boot_id, new = %boot_id, "the daemon restarted");
                }
                self.boot_id = Some(boot_id);
                self.link = LinkState::Connected;
                // The outage is OVER, and only getting all the way up ends it.
                // Not a `Connecting` — the ladder passes through that on every
                // attempt, and an outage that reset there would never be more
                // than one backoff long on screen.
                self.down_since = None;
                self.note(
                    now,
                    NoteLevel::Calm,
                    &format!("link up · {}", resync.label()),
                );
                grew_activity(true)
            }
            TransportEvent::Disconnected {
                reason,
                next_retry_in_ms,
            } => {
                // **FIRST DROP WINS.** `get_or_insert` and not an assignment:
                // the transport re-emits `Disconnected` once per failed
                // attempt, so assigning here would restart the clock every few
                // seconds and the face would say "down 4s" through an
                // hour-long outage — the stale-number defect wearing a fresh
                // number's clothes.
                self.down_since.get_or_insert(now);
                // A DEADLINE, not a duration. `now + wait` stays true as the
                // clock moves; the duration it was computed from does not.
                self.link = LinkState::Disconnected {
                    reason: reason.clone(),
                    retry_at: TimeDelta::try_milliseconds(next_retry_in_ms as i64).map(|d| now + d),
                };
                // The transcript STAYS — it is history and it remains true.
                // Only the meters go dark.
                self.invalidate("the link went down");
                self.note(now, NoteLevel::Critical, &format!("link down · {reason}"));
                grew_activity(true)
            }
            TransportEvent::Invalidate { reason } => {
                self.invalidate(&reason);
                grew_activity(false)
            }
            TransportEvent::Fleet { statuses, capped } => {
                for s in &statuses {
                    let Some(id) = s.get("thread_id").and_then(Value::as_str) else {
                        continue;
                    };
                    let id = id.to_string();
                    let f = self.facts(&id);
                    if let Some(v) = s.get("state").and_then(Value::as_str) {
                        f.state = v.to_string();
                    }
                    if let Some(v) = s.get("executing").and_then(Value::as_bool) {
                        f.executing = v;
                    }
                    // `detail` moves WITH the state: a status frame that
                    // carried a state and no detail is a state with no reason.
                    if s.get("state").is_some() {
                        f.detail = s.get("detail").and_then(Value::as_str).map(str::to_string);
                    }
                    if let Some(v) = s.get("parent_id").and_then(Value::as_str) {
                        f.parent_id = Some(v.to_string());
                    }
                    if let Some(c) = s.get("children").and_then(Children::parse) {
                        f.children = Some(c);
                    }
                    if let Some(r) = s.get("rolled").and_then(RolledFacts::parse) {
                        f.rolled = Some(r);
                    }
                    f.queued = known_u32(s.get("queued"));
                    f.unread = known_u32(s.get("unread"));
                    if let Some(v) = s.get("last_seq").and_then(Value::as_u64) {
                        f.last_seq = Some(v);
                    }
                }
                self.status_full = capped;
                self.window_full = self.window_full || capped;
                tracing::info!(threads = statuses.len(), capped, "fleet status applied");
                grew_activity(true)
            }
            TransportEvent::Threads { rows, capped } => {
                for r in &rows {
                    let Some(id) = r.get("thread_id").and_then(Value::as_str) else {
                        continue;
                    };
                    let id = id.to_string();
                    let f = self.facts(&id);
                    // The SETTINGS BLOB IS NOT READ. See this file's header.
                    f.cwd = str_of(r, "cwd");
                    f.model = str_of(r, "model");
                    f.account = str_of(r, "account");
                    f.updated = parse_ts(&str_of(r, "updated_at"));
                    if let Some(v) = r.get("state").and_then(Value::as_str) {
                        f.state = v.to_string();
                    }
                    if let Some(v) = r.get("executing").and_then(Value::as_bool) {
                        f.executing = v;
                    }
                    if let Some(v) = r.get("last_seq").and_then(Value::as_u64) {
                        f.last_seq = Some(v);
                    }
                    if let Some(at) = parse_ts(&str_of(r, "last_user_input_at")) {
                        f.last_user_input = f.last_user_input.max(Some(at));
                    }
                    if let Some(t) = r.get("title").and_then(Value::as_str) {
                        f.title = Some(t.to_string());
                    }
                    // **A RENAME LANDS HERE, WHILE HE IS LOOKING** (TUI-P1a):
                    // the next projection names the thread by it. Nothing is
                    // re-read and nothing is re-subscribed.
                    if let Some(n) = r.get("name").and_then(Value::as_str).filter(|n| !n.trim().is_empty()) {
                        f.name = Some(n.to_string());
                        f.name_manual_at = if r.get("name_manual").and_then(Value::as_bool) == Some(true) {
                            // A manual name with no stamp still outranks an
                            // unstamped lock: it is the newest thing said.
                            Some(parse_ts(&str_of(r, "name_at")).unwrap_or(DateTime::<Utc>::MAX_UTC))
                        } else {
                            None
                        };
                    }
                    // **WHO STARTED IT** — only once the producer has said, so
                    // "not told yet" is never read as "nobody".
                    if r.get("identity").and_then(Value::as_bool) == Some(true) {
                        f.app_id = r.get("app_id").and_then(Value::as_str).map(str::to_string);
                        f.agent = r.get("agent").and_then(Value::as_str).map(str::to_string);
                    }
                }
                self.window_full = self.window_full || capped;
                grew_activity(true)
            }
            TransportEvent::Note { text, level } => {
                self.note(now, level, &text);
                grew_activity(true)
            }
            TransportEvent::FirstLine { thread_id, line } => {
                if let Some(l) = first_line_of(&line) {
                    self.facts(&thread_id).first_line = Some(l);
                }
                grew_activity(false)
            }
            TransportEvent::DaemonWarnings { warn, error } => {
                // **NOT `grew_activity(true)`.** A warning count moving is not
                // activity on a thread and must not scroll anything, mark anything
                // unread, or wake a quiet screen. It changes one segment of the
                // header on the next paint, which the fleet event that follows it
                // triggers anyway.
                self.daemon_warnings = Known::Known(warn);
                self.daemon_warning_errors = Known::Known(error);
                grew_activity(false)
            }
            TransportEvent::Frame { msg, replayed } => self.apply_frame(msg, replayed, now),
        }
    }

    fn apply_frame(&mut self, msg: ServerMsg, _replayed: bool, now: DateTime<Utc>) -> Applied {
        match msg {
            ServerMsg::Event {
                thread_id,
                seq,
                message_id,
                ts,
                event,
            } => {
                let row = LedgerRow {
                    seq,
                    message_id,
                    ts: parse_ts(&ts),
                    received: now,
                    event,
                };
                // The high-water mark moves with the ledger, so the next open
                // of this thread fetches the right tail even if no status has
                // landed since.
                let f = self.facts(&thread_id);
                f.last_seq = Some(f.last_seq.map_or(seq, |p| p.max(seq)));

                let open_grew = match self.open.as_mut() {
                    Some(o) if o.id == thread_id => {
                        let now_d = crate::diag::on().then(|| describe(&row));
                        match o.rows.insert(seq, row.clone()) {
                            None => crate::diag!(
                                "store",
                                "INSERT thread={thread_id} {} reason=frame",
                                now_d.unwrap_or_default()
                            ),
                            Some(old) => crate::diag!(
                                "store",
                                "REPLACE thread={thread_id} reason=frame · was [{}] now [{}]",
                                describe(&old),
                                now_d.unwrap_or_default()
                            ),
                        }
                        trim_open(o);
                        true
                    }
                    _ => false,
                };
                self.push_tail(thread_id, row);
                Applied {
                    activity_grew: true,
                    open_grew,
                }
            }
            // EDGE-TRIGGERED, and DELIBERATELY PARTIAL. The push carries five
            // of `status_json`'s eight keys — no `last_seq`, no `last_seen_seq`
            // — so it updates what it knows and touches nothing else. A push
            // that overwrote `last_seq` with a default would be the daemon's
            // silence rendered as a number.
            ServerMsg::Status {
                thread_id,
                state,
                executing,
                queued,
                unread,
                ..
            } => {
                let f = self.facts(&thread_id);
                f.state = state;
                f.executing = executing;
                f.queued = Known::Known(queued as u32);
                f.unread = Known::Known(unread as u32);
                Applied::default()
            }
            // The 1 Hz beat. Not a row and not history — the next beat repairs
            // any one that is missed, which is why lag recovery ignores it too.
            ServerMsg::Liveness { .. } => Applied::default(),
            ServerMsg::Progress {
                op,
                phase,
                detail,
                done,
                ok,
                ..
            } => {
                let sev = if done && ok == Some(false) {
                    NoteLevel::Critical
                } else {
                    NoteLevel::Calm
                };
                let d = detail.unwrap_or_default();
                self.note(now, sev, &format!("{op} · {phase}{}{d}", sep(&d)));
                Applied {
                    activity_grew: true,
                    open_grew: false,
                }
            }
            ServerMsg::Lifecycle {
                service,
                phase,
                reason,
                ..
            } => {
                let level = if phase == "going_down" {
                    NoteLevel::Warn
                } else {
                    NoteLevel::Calm
                };
                self.note(now, level, &format!("{service} {phase} ({reason})"));
                Applied {
                    activity_grew: true,
                    open_grew: false,
                }
            }
            ServerMsg::Info { domain, reason, .. } => {
                self.note(
                    now,
                    NoteLevel::Calm,
                    &format!("{domain} changed ({reason})"),
                );
                Applied {
                    activity_grew: true,
                    open_grew: false,
                }
            }
            // **NEVER DROPPED.** A frame this build has not heard of still
            // reaches the screen, because `DIR-4` makes silence the defect and
            // the daemon's docs promise new frames without warning.
            ServerMsg::Unrecognized { frame_type, .. } => {
                self.note(
                    now,
                    NoteLevel::Warn,
                    &format!("unknown frame type {frame_type} — shown, not dropped"),
                );
                Applied {
                    activity_grew: true,
                    open_grew: false,
                }
            }
            // Handshake and RPC frames never reach the store: the transport
            // consumes them. Reaching here would be a transport defect.
            other => {
                tracing::error!(
                    label = other.label(),
                    "a transport-only frame reached the store"
                );
                Applied::default()
            }
        }
    }
}

/// One row, as the flight recorder names it: seq, kind, message id, and the
/// length of the words it carries (`-` for a kind that has none).
pub fn describe(r: &LedgerRow) -> String {
    let text = match r.event.body_text() {
        Some(t) => format!("text_len={} text={}", t.len(), crate::diag::preview(t, 40)),
        None => "text=-".to_string(),
    };
    format!(
        "seq={} kind={} message_id={} {text}",
        r.seq,
        r.event.kind(),
        r.message_id.map_or("-".to_string(), |m| m.to_string()),
    )
}

fn trim_open(o: &mut Open) {
    while o.rows.len() > OPEN_CAP {
        let Some(first) = o.rows.keys().next().copied() else {
            break;
        };
        if let Some(gone) = o.rows.get(&first) {
            crate::diag!(
                "store",
                "REMOVE thread={} {} reason=over OPEN_CAP={OPEN_CAP}, oldest row trimmed",
                o.id,
                describe(gone)
            );
        }
        o.rows.remove(&first);
        o.forgotten += 1;
        o.from_seq = Some(first + 1);
    }
}

fn sum(it: impl Iterator<Item = Known<u32>>) -> Known<u32> {
    let mut total = 0u32;
    let mut any = false;
    for k in it {
        if let Known::Known(v) = k {
            total = total.saturating_add(v);
            any = true;
        }
    }
    // Nothing known is not zero. One thing known among many is a known number
    // over a partly-unknown set — which the SCOPE label beside the badge is
    // there to qualify, rather than this function pretending otherwise.
    if any {
        Known::Known(total)
    } else {
        Known::Unknown
    }
}

fn known_u32(v: Option<&Value>) -> Known<u32> {
    match v.and_then(Value::as_u64) {
        Some(n) => Known::Known(n as u32),
        None => Known::Unknown,
    }
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn queue_item(v: &Value) -> QueueItem {
    QueueItem {
        message_id: v.get("message_id").and_then(Value::as_u64).unwrap_or(0),
        text: clip(&str_of(v, "text"), 2_000),
        intent: str_of(v, "intent"),
        created_at: str_of(v, "created_at"),
    }
}

fn level_word(l: NoteLevel) -> &'static str {
    match l {
        NoteLevel::Calm => "calm",
        NoteLevel::Warn => "warn",
        NoteLevel::Critical => "critical",
    }
}

fn severity_of(level: &str) -> Severity {
    match level {
        "warn" => Severity::Warn,
        "critical" => Severity::Critical,
        _ => Severity::Calm,
    }
}

fn sep(d: &str) -> &'static str {
    if d.is_empty() {
        ""
    } else {
        " · "
    }
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

// ════════════════════════════════════════════════════════════════════════════
//  ↑ THE MODEL  │  THE PROJECTION ↓
//
//  The line this file's header draws, as a landmark a test can find. Above:
//  what the face KNOWS. Below: what the face SHOWS. See the header for the two
//  rules that cross it and the one named exception.
// ════════════════════════════════════════════════════════════════════════════

// **THERE IS NO `short()` HELPER ANY MORE**, and that is `0076` finishing the
// job rather than tidying. It took the first eight characters of a uuid, and
// its doc comment said they were *"what fits and what he reads"* — which was
// the face's belief and not his. `DIR-16` is his: *"don't show me thread or
// task hash names. not useful to me."*
//
// By the end of this round its four call sites were the list row, the list
// header, the thread header and the queue overlay, and every one of them is a
// face he looks at. There is nothing left for it to be right about, and a
// helper that abbreviates an id is a helper the next row builder will reach
// for. `Store::label_of` is the one producer of how a thread is NAMED.

/// `$HOME` -> `~`. Done HERE because the render crate has no environment; done
/// by PREFIX only, so nothing else about the path is touched. Two of his
/// directories differ by one syllable and a face that tidied a path is the
/// thing that would make them look alike.
fn shorten(cwd: &str, home: &str) -> String {
    match (home.is_empty(), cwd.strip_prefix(home)) {
        (false, Some(rest)) => format!("~{rest}"),
        _ => cwd.to_string(),
    }
}

/// A generous clip so a pathological payload cannot make one row cost a
/// megabyte of wrapping work. The renderer ellipsizes to the real width.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}

fn one_line(v: &Value) -> String {
    v.to_string().replace(['\n', '\r'], " ")
}

pub(crate) fn flat(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
}

// ────────────────────────────────────────────────────────────────────────────
//  IS THIS ROW STDOUT? — the one predicate the deleted filter left behind
// ────────────────────────────────────────────────────────────────────────────

/// **WHETHER THIS EVENT IS SOMETHING THAT WAS SAID.** Read only while
/// `ViewState::stdout_only` is on, and read nowhere else.
///
/// # What this replaced, and why the replacement is a predicate
///
/// Until round six there were nine `Lane` groups, a `LANES` table naming each
/// one and the digit that toggled it, a `lane_of(kind)` total function over
/// forty-four wire kinds, and a `lane_of_event` refinement for the nine hats
/// `unknown` wears. All of it existed to serve the filter overlay, and the
/// filter is gone — *"filter is too much. Just drop that feature… I had that
/// request because I wanted a simple toggle."*
///
/// **THE `Lane` ENUM WAS MEASURED BEFORE IT WAS DELETED, and the claim
/// defending it was false.** Its doc comment said the groups were "also the
/// vocabulary the event lane is sorted into". They were not: `lane_of_event`
/// had exactly ONE non-test caller in the shipped tree — the
/// `filter.allows(lane)` guard in [`project`] — and the `Lane` the event lane
/// is sorted into is `Block::Lane`, an unrelated ROW SHAPE that shares the
/// word and nothing else. Nothing but filtering read a group, so nothing but
/// filtering had to survive.
///
/// **AND WHAT SURVIVED IS INVISIBLE, which is the condition it survived on.**
/// One `bool`-returning function, private to this module, with no key, no
/// overlay, no indicator, no label and no user-facing word. There is no group
/// called `speech` any more; there is a question about a row.
///
/// # The two things it has to get right
///
/// `context_injected` is IN. A skill body loaded into the turn is CONTENT — it
/// is why `0047` D5/D7 pulled the `unknown`/`user` subtype out of the firehose
/// in the first place, and that decision is preserved here verbatim rather than
/// rediscovered.
///
/// Everything else is OUT, including every kind this build has never seen. That
/// is the safe direction and it is the opposite of the old `Lane::Other`
/// default: a kind from the future is not speech, so `stdout only` shows him
/// what was said rather than what was said plus whatever arrived last week.
/// With the toggle OFF — the default, and `DEC-19`'s reachability promise —
/// this function is not consulted at all and no row is dropped for any reason.
fn is_stdout(ev: &ThreadEvent) -> bool {
    match ev {
        // A sidechain reply is a subagent talking. It is speech either way.
        ThreadEvent::Unknown { raw } => subtype_of(raw) == "user",
        other => matches!(
            other.kind(),
            "user_message" | "assistant_text" | "context_injected"
        ),
    }
}

/// `raw.type/raw.subtype`, which is the key the `unknown` census is built on.
/// Falls back to `raw.type` alone when there is no subtype, because two of the
/// nine live forms (`rate_limit_event`, `tool_progress`, `user`) carry only a
/// `type`.
fn subtype_of(raw: &Value) -> String {
    let t = raw.get("type").and_then(Value::as_str).unwrap_or("");
    match raw.get("subtype").and_then(Value::as_str) {
        Some(s) => format!("{t}/{s}"),
        None => t.to_string(),
    }
}

/// How a kind FOLDS when many of it arrive in a row.
///
/// **Nothing is ever dropped** — a folded row carries `×n` and `x` expands it
/// to every event it stands for. This is display coalescing, which is what
/// `DEC-12` decided and what makes `DIR-4`'s breadth readable instead of
/// theoretical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fold {
    /// One row per event. The default, and what most kinds do.
    Never,
    /// One LIVE row per `task_id`, sitting where that task's newest event is.
    /// `DEC-12`, verbatim: a task's row should be where the task currently is,
    /// which on a bottom-recency transcript means it rides down with it.
    PerTask,
    /// A contiguous RUN of the same fold key becomes one row: the same fact,
    /// repeated. The rich `rate_limit_event` is the live example — it restates
    /// the same two utilization windows every few seconds.
    PerRun,
}

/// **THE FOLD TABLE, KEYED ON THE EVENT** rather than on its kind string.
///
/// `stream_event` and `message_usage` LEFT this table in `0051` and did not
/// move to another entry in it: they are not folded any more, they are not
/// rendered any more. Deltas become the growing message (`DEC-31` + the `0048`
/// design) and the per-message usage row was deleted outright because its
/// numbers were the API's `message_start` placeholder (`0047` D3). Folding is
/// for rows that exist.
fn fold_of(ev: &ThreadEvent) -> Fold {
    match ev {
        ThreadEvent::TaskProgress { .. } => Fold::PerTask,
        // Both of these RESTATE THEMSELVES. A rate-limit report repeats the
        // same two utilization windows every few seconds, and an API retry
        // storm is ten rows that differ only in a number the newest one already
        // carries — `api retry · attempt 8/10 · in 39s ×8` is the whole of what
        // eight rows were saying.
        ThreadEvent::Unknown { raw }
            if matches!(
                subtype_of(raw).as_str(),
                "rate_limit_event" | "system/api_retry"
            ) =>
        {
            Fold::PerRun
        }
        // **N+1 RESULTS PER TURN, AND THE LEDGER IS RIGHT TO HOLD THEM**
        // (`0132`). The daemon's own words: *"`modelUsage` is the CUMULATIVE
        // per-model total for the whole claude process — parent and every
        // subagent […] A turn with N subagents therefore emits N+1 results all
        // quoting the same running total."* Measured on a clone of his live
        // ledger: 12,940 `result_turn` against 11,092 `turn_started`, and one
        // fan-out round drew TWENTY-SIX of them.
        //
        // So *"getting multiple of these after each turn"* is not a replay and
        // not one event rendered twice — it is several true events, and ONE
        // honest row that says `×n` is the same answer the usage row got in
        // `0047` D4. Nothing is dropped: the fold is disarmed the moment the
        // row is open, so `x` still draws every one of them.
        ThreadEvent::ResultTurn { .. } => Fold::PerRun,
        _ => Fold::Never,
    }
}

/// What a [`Fold::PerRun`] run is a run OF. The kind alone is too coarse now
/// that one kind (`unknown`) carries nine different facts: a run of rate-limit
/// reports interrupted by an api_retry is two runs, not one.
fn fold_key(ev: &ThreadEvent) -> String {
    match ev {
        ThreadEvent::Unknown { raw } => format!("unknown/{}", subtype_of(raw)),
        // **THE OUTCOME IS PART OF THE FACT.** Measured on his ledger
        // (`6dda9d84`, seqs 43349–43353): a real burst carries four successes
        // and an `is_error` result stamped to the same millisecond, and the
        // amber one is LAST. Keyed on the kind alone that burst would have
        // drawn one calm `×5` row showing the failure's numbers under the word
        // `success` — a collapse that HIDES a failure, which is the one thing
        // this fold must not do. `subtype` rides along because the wire can
        // disagree with itself: `subtype: "success"` with `is_error: true` is a
        // real pairing in that same thread.
        ThreadEvent::ResultTurn {
            subtype, is_error, ..
        } => format!("result_turn/{subtype}/{is_error}"),
        other => other.kind().to_string(),
    }
}

/// One rendered row, before folding.
enum Shape {
    /// A speech act: a box, sided.
    Message(Who, String),
    /// A lane annotation.
    Lane(char, Severity, String),
    /// A full-width rule.
    Rule(String),
    /// **NOT RENDERED, AND THE STRING IS WHAT `x` SHOWS INSTEAD.**
    ///
    /// His rule, verbatim: *"no rows that say unknown"* — and behind it the
    /// broader one, that a row which has nothing to tell him is worse than no
    /// row, because he has to read it to find that out. `0047` measured the
    /// live `unknown` vocabulary and two thirds of it is machinery: a
    /// thinking-token estimate, a heartbeat, "an HTTP request is in flight".
    ///
    /// **This is a DROP FROM THE DEFAULT FACE AND NOT A DROP FROM THE BUILD.**
    /// The row is retained, the ledger has it, and expanded mode (`x`) renders
    /// the string carried here — which is how `DIR-4`'s breadth survives a
    /// retraction that was only ever about the default view. A kind whose
    /// `Silent` text were empty would be a real drop; there is none, and the
    /// test `every_silent_kind_still_says_something_under_x` is what keeps it
    /// that way.
    Silent(String),
}

impl Shape {
    /// What this shape would put on screen. Used to ask whether OPENING a row
    /// would change anything — rendering it both ways and comparing is the only
    /// answer that cannot disagree with what is actually drawn.
    fn text(&self) -> &str {
        match self {
            Shape::Message(_, t) | Shape::Lane(_, _, t) | Shape::Rule(t) | Shape::Silent(t) => t,
        }
    }
}

/// What the projection knows that ONE event does not know about itself.
///
/// It exists because two of `0051`'s rulings are about relationships between
/// rows rather than about a row: the rich rate-limit report supersedes the thin
/// one, and a task's human description arrives on one event and is wanted on
/// the next four. Both were solvable by threading two more arguments through
/// `shape`; one struct is the version that does not grow a third argument
/// silently.
struct Ctx<'a> {
    /// A RICH `rate_limit_event` (the untyped form, carrying the five-hour and
    /// seven-day utilization windows) appears somewhere in this tail.
    ///
    /// **The thin typed `rate_limit_event` then renders NOTHING**, because the
    /// two describe the same moment and he asked not to see two rows for it.
    /// The flag is a PRE-SCAN over the same tail rather than a constant, so a
    /// daemon that stops emitting the rich form does not silently take rate
    /// limits off his screen with it — the thin row comes back on its own.
    rich_rate_limits: bool,
    /// `task_id` -> the human description, learned from `task_started` /
    /// `task_progress` earlier in the walk.
    ///
    /// *"don't show me thread or task hash names. not useful to me."* A
    /// `task_updated` carries a status and a hash and nothing else; this is
    /// where the name it deserves comes from.
    task_names: &'a HashMap<String, String>,
}

/// One ledger event as what it should LOOK like. The rendering vocabulary of
/// `0023` `event_stream`, kind by kind.
///
/// **`expanded` governs BOTH how much of a payload comes through AND whether a
/// [`Shape::Silent`] row appears at all** — and the docstring that stood here
/// claimed the opposite ("never WHETHER a row exists"). It was false when it
/// was written: `project` has always read `expanded` to disable folding for the
/// whole transcript, which changes the row COUNT (`0047` D1). It is false twice
/// over now that `Silent` exists. The honest statement is the one above: a
/// hidden LANE is the filter's business and the header says when the filter is
/// on; a folded row is this function's and it says `×n`; a silent row is this
/// function's too and `x` is what reaches it.
/// **THE METADATA THAT ATTACHES TO A USER INPUT**, flattened to ordered
/// key/value rows for the white box's expander.
///
/// > *There's also some metadata we attach to user inputs.*
/// > *- message id*
/// > *- threads system metadata*
/// > *- app specific metadata.*
/// > *we need a way to toggle viewing this.*
///
/// The MESSAGE ID is not here — it is on [`BlockId`], it is drawn at the top
/// right of every white box by default, and it is the one piece of this that is
/// never behind the toggle. What is here is the other two: `intent`, which is
/// the threads system's own, and whatever the application stamped into the
/// opaque `metadata` object, which for this face is
/// `{"tui": {"client": ..., "queued_parts": ...}}` and for the AUI is
/// something else entirely.
///
/// **IT FLATTENS AND IT NEVER INTERPRETS.** Nested objects become dotted paths
/// and every leaf is printed as the JSON it is. The daemon's own contract for
/// this field is that its shape belongs to the application; a face that
/// re-shaped it would be asserting an opinion nobody gave it, and the first
/// application to disagree would have its metadata silently mangled.
///
/// **NO HASHES.** A key whose value is a long opaque string is still printed —
/// this is the EXPANDED view of one message he asked to see, not the thread
/// header the no-hash-names directive governs.
fn user_meta(ev: &ThreadEvent) -> Vec<(String, String)> {
    let ThreadEvent::UserMessage { metadata, intent, .. } = ev else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = Vec::new();
    if let Some(i) = intent {
        out.push(("intent".to_string(), i.clone()));
    }
    flatten(String::new(), metadata, &mut out);
    out
}

/// One leaf per row, keyed by its dotted path. An empty object contributes
/// nothing rather than an empty row.
fn flatten(path: String, v: &serde_json::Value, out: &mut Vec<(String, String)>) {
    match v {
        serde_json::Value::Null => {}
        serde_json::Value::Object(m) => {
            for (k, v) in m {
                let next = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                flatten(next, v, out);
            }
        }
        serde_json::Value::String(t) => out.push((path, t.clone())),
        other => out.push((path, other.to_string())),
    }
}

/// `(outcome, words)` per `<agent_report … outcome="…">words</agent_report>`
/// in a wake's text, in order. The daemon writes that envelope
/// (`services/threads/src/managed.rs` `deliver_due_wakes`); a text that is not
/// one yields nothing, never a guess.
fn agent_reports(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("<agent_report ") {
        rest = &rest[i..];
        let Some(close) = rest.find('>') else { break };
        let head = &rest[..close];
        let outcome = head
            .split("outcome=\"")
            .nth(1)
            .and_then(|t| t.split('"').next())
            .unwrap_or("")
            .to_string();
        let body = &rest[close + 1..];
        let end = body.find("</agent_report>").unwrap_or(body.len());
        out.push((outcome, body[..end].trim().to_string()));
        rest = &body[end..];
    }
    out
}

fn shape(ev: &ThreadEvent, expanded: bool, ctx: &Ctx) -> Shape {
    let cap = if expanded { 8_000 } else { 400 };
    match ev {
        // **THE WAKE IS NOT HIS WORDS** (TUI-SUBAGENTS §3.7). The daemon's
        // child→parent report arrives as a `user_message` tagged
        // `origin: agents, kind: child_reported`; drawn as a "you said" box it
        // put words in his mouth. It is a lane block: who reported, how it
        // went, and — expanded — each child's last words.
        ThreadEvent::UserMessage { text, metadata, .. }
            if metadata.get("origin").and_then(Value::as_str) == Some("agents")
                && metadata.get("kind").and_then(Value::as_str) == Some("child_reported") =>
        {
            let reports = agent_reports(text);
            let words: Vec<&str> = reports
                .iter()
                .map(|(o, _)| match o.as_str() {
                    "ok" => "done",
                    "failed" => "failed",
                    "interrupted" => "interrupted",
                    _ => "reported",
                })
                .collect();
            let sev = if reports.iter().any(|(o, _)| o == "failed") {
                Severity::Warn
            } else {
                Severity::Calm
            };
            let mut line = format!(
                "agent reports · {} in · {}",
                reports.len(),
                words.join(" · ")
            );
            if expanded {
                for (o, w) in &reports {
                    line.push_str(&format!(" ▸ {o} — {}", clip(&flat(w), cap)));
                }
            } else if let Some((_, w)) = reports.first() {
                line.push_str(&format!(" · \"{}\"", clip(&flat(w), HEADLINE)));
            }
            Shape::Lane('◇', sev, line)
        }
        ThreadEvent::UserMessage { text, .. } => Shape::Message(Who::User, text.clone()),
        // A sidechain reply is a SUBAGENT talking, not the thread. The daemon
        // draws that line with `parent_tool_use_id` and this face keeps it:
        // boxing a subagent's words as the thread's own reply is the kind of
        // wrong that reads perfectly.
        ThreadEvent::AssistantText {
            text,
            parent_tool_use_id: None,
        } => Shape::Message(Who::Assistant, text.clone()),
        // *"seeing the 'subagent said' notice is cool but I should be able to
        // expand events like that."* (`DEC-32`) — the notice stays exactly as
        // it was and the collapsed clip is what makes the row announce itself
        // as openable.
        ThreadEvent::AssistantText { text, .. } => Shape::Lane(
            '◇',
            Severity::Calm,
            format!(
                "subagent said · {}",
                clip(&flat(text), if expanded { cap } else { HEADLINE })
            ),
        ),
        ThreadEvent::ContextInjected { text } => Shape::Lane(
            '·',
            Severity::Calm,
            format!(
                "context injected · {}",
                clip(&flat(text), if expanded { cap } else { HEADLINE })
            ),
        ),
        // **AN EMPTY THINKING ROW SAYS NOTHING AND TAKES A LINE TO SAY IT.**
        // The daemon emits a great many with `text: ""` — the seq window in
        // `0047`'s prose has one — and `thinking · 1 line(s) · ` is a row whose
        // entire content is the word "thinking". It goes Silent; the live
        // assembly is what shows reasoning while it is happening, and a
        // non-empty row still shows it afterwards.
        ThreadEvent::Thinking { text } if text.is_empty() => {
            Shape::Silent("thinking · (the daemon sent no text for this block)".into())
        }
        ThreadEvent::Thinking { text } => {
            let lines = text.lines().count().max(1);
            if expanded {
                Shape::Lane('·', Severity::Unknown, format!("thinking · {}", flat(text)))
            } else {
                Shape::Lane(
                    '·',
                    Severity::Unknown,
                    format!(
                        "thinking · {lines} line(s) · {}",
                        clip(&flat(text.lines().next().unwrap_or("")), 160)
                    ),
                )
            }
        }
        ThreadEvent::ThreadStarted { model, cwd, .. } => {
            Shape::Rule(format!("thread started · {model} · {cwd}"))
        }
        // Collapsed it is the three facts he would want at a glance; expanded
        // it is the tool roster, which is the reason `0047` marked this kind
        // expandable rather than long.
        ThreadEvent::SystemInit {
            model,
            cwd,
            tools,
            permission_mode,
        } => Shape::Lane(
            '·',
            Severity::Calm,
            format!(
                "session init · {} · {}{}{}",
                model.as_deref().unwrap_or("?"),
                cwd.as_deref().unwrap_or("?"),
                permission_mode
                    .as_deref()
                    .map(|p| format!(" · {p}"))
                    .unwrap_or_default(),
                match (tools.is_empty(), expanded) {
                    (true, _) => String::new(),
                    (false, false) => format!(" · {} tools", tools.len()),
                    (false, true) => format!(" · {}", tools.join(" · ")),
                }
            ),
        ),
        // **ONE ROW FOR THE CALL AND ITS RESULT** (`DEC-33`), which `project`
        // assembles by rewriting this row in place when the result lands. This
        // arm is what an UNMATCHED call looks like: the tool is still running,
        // and it says so rather than sitting there looking finished.
        ThreadEvent::ToolUse { name, input, .. } => {
            let (sev, text) = tool_row(name, input, Outcome::Running, None, expanded, cap);
            Shape::Lane('⚙', sev, text)
        }
        // **A RESULT THAT REACHED THIS ARM IS AN ORPHAN.** `project` merges
        // every result whose `tool_use_id` matched a call in the tail; what is
        // left is a result whose call fell off the bottom of `OPEN_CAP`, or one
        // the daemon sent no id for. It renders alone, because the alternative
        // is dropping a tool failure nobody would ever see.
        ThreadEvent::ToolResult {
            is_error, content, ..
        } => {
            let (sev, text) = tool_row(
                "(call not in view)",
                &Value::Null,
                if *is_error { Outcome::Failed } else { Outcome::Ok },
                Some(content),
                expanded,
                cap,
            );
            Shape::Lane('⚙', sev, text)
        }
        // **DELETED AS A ROW** (`0047` D3/D4, and his words: *"in 2 · out 4 ·
        // cache 168606 … is not true if it's talking about tokens"*). It was
        // not a partial and not a stream value: it is the API's `message_start`
        // PLACEHOLDER, captured once and never revised — measured output
        // {5,3,8,3} where the true finals were {1133,2034,394,850}. The honest
        // usage line is `result_turn` and it is rendered instead.
        //
        // The row is not corrected, it is retired, because there is no
        // correction available: a per-MESSAGE final does not exist as a
        // persisted event at all.
        ThreadEvent::MessageUsage {
            model,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
        } => Shape::Silent(format!(
            "message_usage (the API's message_start placeholder, NOT a count of \
             what was written) · {model} · in {input_tokens} · out {output_tokens} · \
             cache read {cache_read_tokens} · cache write {cache_creation_tokens}"
        )),
        // **HIS RETRACTION**: *"We don't need stream chunks rendered."*
        // (`DEC-31`). The deltas do not vanish — they become the message, live,
        // in `project`. This is what `x` still reaches.
        ThreadEvent::StreamEvent { raw } => {
            Shape::Silent(format!("stream · {}", clip(&one_line(raw), cap)))
        }
        // ── agents going out and coming back — what he actually named ──
        ThreadEvent::AgentSpawn {
            subagent_type,
            description,
            run_in_background,
        } => Shape::Lane(
            '◇',
            Severity::Calm,
            format!(
                "agent → {}{}{}",
                subagent_type.as_deref().unwrap_or("?"),
                description
                    .as_deref()
                    .map(|d| format!(" · {}", flat(d)))
                    .unwrap_or_default(),
                if *run_in_background { " · background" } else { "" }
            ),
        ),
        // **NO HASH ON THE ONE-LINE FACE** (his words: *"don't show me thread
        // or task hash names. not useful to me."*). `agent_id` told him
        // nothing; the status does.
        ThreadEvent::AgentLaunched {
            is_async, status, ..
        } => Shape::Lane(
            '◇',
            Severity::Calm,
            format!(
                "agent launched · {status}{}",
                if *is_async { " · async" } else { "" }
            ),
        ),
        // A DOCUMENTED HEURISTIC with ~9 spurious firings per real completion
        // (`event.rs`). It renders, and it says on its own row that it is not
        // the signal — `task_notification` is.
        ThreadEvent::AgentCompleted { .. } => Shape::Lane(
            '◇',
            Severity::Unknown,
            "agent completed (heuristic — task_notification is the real one)".into(),
        ),
        ThreadEvent::TaskStarted {
            description,
            subagent_type,
            ..
        } => Shape::Lane(
            '◇',
            Severity::Calm,
            format!(
                "agent out{}{}",
                subagent_type
                    .as_deref()
                    .map(|s| format!(" · {s}"))
                    .unwrap_or_default(),
                description
                    .as_deref()
                    .map(|d| format!(" · {}", flat(d)))
                    .unwrap_or_default()
            ),
        ),
        // A `task_updated` carries a status, a hash, and nothing else. The
        // description it deserves came in on an earlier row of the same walk —
        // see [`Ctx::task_names`].
        ThreadEvent::TaskUpdated {
            task_id,
            status,
            error,
        } => {
            let sev = match error {
                Some(_) => Severity::Critical,
                None => Severity::Calm,
            };
            Shape::Lane(
                '◇',
                sev,
                format!(
                    "agent{} · {}{}",
                    named(ctx, task_id),
                    status.as_deref().unwrap_or("updated"),
                    error
                        .as_deref()
                        .map(|e| format!(" · {}", flat(e)))
                        .unwrap_or_default()
                ),
            )
        }
        ThreadEvent::TaskProgress {
            task_id,
            description,
            last_tool_name,
            total_tokens,
            tool_uses,
            duration_ms,
            ..
        } => Shape::Lane(
            '◇',
            Severity::Calm,
            format!(
                "agent · {}{}{}",
                description
                    .as_deref()
                    .map(flat)
                    .or_else(|| ctx.task_names.get(task_id).cloned())
                    .unwrap_or_else(|| "working".into()),
                last_tool_name
                    .as_deref()
                    .map(|t| format!(" · {t}"))
                    .unwrap_or_default(),
                meter(*total_tokens, *tool_uses, *duration_ms)
            ),
        ),
        // THE TRUE TERMINAL SIGNAL — and *"seeing the 'subagent said' notice is
        // cool but I should be able to expand events like that."* The collapsed
        // row is the headline; expanded is the agent's whole report body, which
        // is why the collapsed clip is deliberately short enough for the row to
        // announce itself as expandable.
        ThreadEvent::TaskNotification {
            task_id,
            status,
            summary,
            total_tokens,
            duration_ms,
        } => {
            let (word, sev) = match status.as_deref() {
                Some("completed") => ("done", Severity::Calm),
                Some("failed") => ("failed", Severity::Critical),
                Some(s) => (s, Severity::Warn),
                None => ("status unknown", Severity::Unknown),
            };
            Shape::Lane(
                '◇',
                sev,
                format!(
                    "agent back{} · {word} · {}{}",
                    named(ctx, task_id),
                    clip(
                        &flat(summary.as_deref().unwrap_or("")),
                        if expanded { cap } else { HEADLINE }
                    ),
                    meter(*total_tokens, None, *duration_ms)
                ),
            )
        }
        // ── turn boundaries ──
        ThreadEvent::TurnStarted { attempt } => Shape::Lane(
            '·',
            if *attempt > 1 {
                Severity::Warn
            } else {
                Severity::Calm
            },
            format!("turn started (attempt {attempt})"),
        ),
        ThreadEvent::TurnEnded { exit_code } => Shape::Lane(
            '·',
            Severity::Calm,
            match exit_code {
                Some(c) => format!("turn ended ({c})"),
                None => "turn ended (exit unknown)".into(),
            },
        ),
        // **A CAUSED CANCEL IS NOT A WARNING** (`0005`): an interruption the
        // operator asked for is calm, and only one nobody asked for is amber.
        ThreadEvent::TurnInterrupted { reason, trigger } => Shape::Lane(
            '·',
            if trigger == "operator" || trigger == "user" {
                Severity::Calm
            } else {
                Severity::Warn
            },
            format!("turn interrupted · {trigger} · {}", flat(reason)),
        ),
        // **THE HONEST USAGE ROW, AND THE ONLY ONE** (`0047` D4). Final,
        // per-model, with cost, and NOT a stream frame — so it satisfies the
        // no-stream-values retraction and the wish for real numbers at once.
        // Words, no glyphs (`0040`). One lane row; expandable when more than
        // one model ran, because the per-model breakdown is what he would open
        // it for.
        ThreadEvent::ResultTurn {
            subtype,
            is_error,
            total_cost_usd,
            num_turns,
            duration_ms,
            model_usage,
            ..
        } => Shape::Lane(
            '·',
            if *is_error {
                Severity::Critical
            } else {
                Severity::Calm
            },
            format!(
                "turn done · {subtype}{}{}{}{}",
                num_turns
                    .map(|n| format!(" · {n} turn(s)"))
                    .unwrap_or_default(),
                duration_ms.map(|d| format!(" · {}", ms(d))).unwrap_or_default(),
                total_cost_usd
                    .map(|c| format!(" · ${c:.2}"))
                    .unwrap_or_default(),
                usage_words(model_usage, expanded)
            ),
        ),
        ThreadEvent::BackgroundTasksChanged { tasks } => Shape::Lane(
            '◇',
            Severity::Calm,
            format!(
                "background tasks · {} running{}",
                tasks.len(),
                if expanded {
                    let names: Vec<String> = tasks
                        .iter()
                        .filter_map(|t| t.get("description").and_then(Value::as_str))
                        .map(flat)
                        .collect();
                    match names.is_empty() {
                        true => String::new(),
                        false => format!(" · {}", names.join(" · ")),
                    }
                } else {
                    String::new()
                }
            ),
        ),
        // **THE `unknown · {…}` ROW, SPLIT AND RETIRED.** Nine live subtypes
        // behind one wire kind (`0047` D5/D7): two thirds of the volume is
        // machinery that says nothing, and the rest is content that was being
        // thrown on screen as raw JSON. Each one is now either a typed
        // one-liner or [`Shape::Silent`]; the catch-all at the bottom is Silent
        // too, which is his rule — *no rows that say unknown* — with `x` as the
        // escape `DIR-4` needs.
        ThreadEvent::Unknown { raw } => unknown_shape(raw, expanded, cap),
        ThreadEvent::Compacted {
            trigger,
            pre_tokens,
            message_id_range,
        } => Shape::Rule(format!(
            "compacted ({trigger}){}{}",
            pre_tokens
                .map(|t| format!(" · {t} tokens before"))
                .unwrap_or_default(),
            message_id_range
                .map(|(a, b)| format!(" · swallowed #{a}–#{b}"))
                .unwrap_or_default()
        )),
        // ── trouble ──
        ThreadEvent::TurnFailed {
            error_kind,
            message,
        } => Shape::Lane(
            '⚠',
            Severity::Critical,
            format!("turn failed · {error_kind} · {}", clip(&flat(message), cap)),
        ),
        ThreadEvent::TurnParked { cause, reason } => Shape::Lane(
            '⚠',
            Severity::Warn,
            format!("turn parked · {cause} · {}", flat(reason)),
        ),
        ThreadEvent::TurnResumed { reason } => {
            Shape::Lane('·', Severity::Calm, format!("turn resumed · {}", flat(reason)))
        }
        ThreadEvent::TurnKilled { reason, strategy } => Shape::Lane(
            '⚠',
            Severity::Critical,
            format!("turn killed · {strategy} · {}", flat(reason)),
        ),
        // **STALENESS IS NEVER RED** (`0005`): a limit with a reset time is a
        // wait, and amber is where data age tops out.
        ThreadEvent::LimitHit { pool, reset, text } => Shape::Lane(
            '⚠',
            Severity::Warn,
            format!(
                "limit hit · {}{} · {}",
                pool.as_deref().unwrap_or("?"),
                reset
                    .as_deref()
                    .map(|r| format!(" · resets {r}"))
                    .unwrap_or_default(),
                clip(&flat(text), cap)
            ),
        ),
        ThreadEvent::ApiError {
            error,
            status,
            text,
        } => Shape::Lane(
            '⚠',
            Severity::Critical,
            format!(
                "api error{}{} · {}",
                status.map(|s| format!(" {s}")).unwrap_or_default(),
                error
                    .as_deref()
                    .map(|e| format!(" · {}", flat(e)))
                    .unwrap_or_default(),
                clip(&flat(text), cap)
            ),
        ),
        // **THE THIN ONE, SUPERSEDED BY THE RICH ONE WHEN BOTH ARE HERE.**
        //
        // `rate_limit_event` arrives twice for the same moment: as this typed
        // kind, carrying one window, and as an `unknown` carrying the whole
        // `rate_limit_info` with both the five-hour and seven-day utilizations
        // (`0047`'s third open question, measured at 2,261 against 7,546). He
        // asked not to see two rows for one fact, so the richer row wins and
        // this one goes Silent — but only when the richer row is actually in
        // the tail, so a daemon that stops emitting it does not take rate
        // limits off his screen without anyone noticing.
        ThreadEvent::RateLimitEvent {
            rate_limit_type,
            utilization,
            status,
        } => {
            let text = format!(
                "limits · {rate_limit_type} · {:.0}% used{}",
                utilization * 100.0,
                status
                    .as_deref()
                    .map(|s| format!(" · {s}"))
                    .unwrap_or_default()
            );
            match ctx.rich_rate_limits {
                true => Shape::Silent(text),
                false => Shape::Lane('⚠', Severity::Warn, text),
            }
        }
        ThreadEvent::BudgetTripped {
            budget_key,
            axis,
            consumed_pct,
            limit_pct,
            action,
        } => Shape::Lane(
            '⚠',
            Severity::Warn,
            format!(
                "budget tripped · {budget_key} · {axis} {consumed_pct:.0}%/{limit_pct:.0}% · {action}"
            ),
        ),
        ThreadEvent::BudgetsFailed {
            phase,
            error,
            outcome,
        } => Shape::Lane(
            '⚠',
            Severity::Critical,
            format!("budgets failed · {phase} · {outcome} · {}", flat(error)),
        ),
        ThreadEvent::RetryScheduled {
            error_kind,
            reason,
            attempt,
            retry_at,
        } => Shape::Lane(
            '⚠',
            Severity::Warn,
            format!(
                "retry #{attempt} at {retry_at} · {error_kind} · {}",
                flat(reason)
            ),
        ),
        ThreadEvent::RetryStarving {
            error_kind,
            attempt,
            give_up_at,
            account,
        } => Shape::Lane(
            '⚠',
            Severity::Critical,
            format!("retry starving · {account} · {error_kind} · {attempt}/{give_up_at}"),
        ),
        ThreadEvent::AccountLoginExpiredNotice { account, bar_until } => Shape::Lane(
            '⚠',
            Severity::Critical,
            format!("login expired · {account} · barred until {bar_until}"),
        ),
        // ── system ──
        ThreadEvent::AccountSwitched { from, to, reason } => Shape::Lane(
            '·',
            Severity::Warn,
            format!("account switched · {from} → {to} · {}", flat(reason)),
        ),
        ThreadEvent::SettingsChanged { patch } => Shape::Lane(
            '·',
            Severity::Unknown,
            format!("settings · {}", clip(&one_line(patch), cap)),
        ),
        ThreadEvent::PluginsChanged {
            added,
            removed,
            modified,
        } => Shape::Lane(
            '·',
            Severity::Calm,
            format!(
                "plugins · +{} -{} ~{}",
                added.len(),
                removed.len(),
                modified.len()
            ),
        ),
        ThreadEvent::ThreadSystemInterrupted { trigger, detail } => Shape::Lane(
            '·',
            Severity::Warn,
            format!("system interrupted · {trigger} · {}", flat(detail)),
        ),
        ThreadEvent::ThreadSystemResumed { trigger, elapsed } => Shape::Lane(
            '·',
            Severity::Calm,
            format!("system resumed · {trigger} · after {elapsed}"),
        ),
        ThreadEvent::ThreadSystemQueued { queued_for } => Shape::Lane(
            '·',
            Severity::Calm,
            format!("queued for {queued_for} before it ran"),
        ),
        // OUR OWN note, which is not a ledger event and is styled apart from
        // one. It rides `custom` because that is what it is on the wire: a
        // client-authored row with an `event_type`.
        ThreadEvent::Custom { event_type, data } if event_type == "tui.note" => Shape::Lane(
            '·',
            severity_of(data.get("level").and_then(Value::as_str).unwrap_or("calm")),
            data.get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ),
        ThreadEvent::Custom { event_type, data } => Shape::Lane(
            '·',
            Severity::Calm,
            format!("{event_type} · {}", clip(&one_line(data), cap)),
        ),
        // **THE CATCH-ALL. ONE CASE STILL RENDERS; TWO GO SILENT.**
        //
        // The three cases are still three different facts and they are still
        // told apart. What changed in `0051` is which of them earns a row in
        // the default face:
        //
        // * A SHAPE THAT DRIFTED (`why` is set) still renders, loudly. This is
        //   a kind we DO model whose body did not fit — a daemon and a client
        //   that disagree about a payload, which is a defect in one of them and
        //   the single most important thing this face can say.
        // * A kind the daemon documents that this build has not specialized,
        //   and a kind nothing has ever heard of, both go Silent. They are the
        //   literal `<kind> · {json}` rows he ruled out, and neither one has
        //   ever told him anything a row could act on. `x` reaches both.
        ThreadEvent::Untyped {
            kind,
            in_daemon_vocabulary,
            why,
            raw,
        } => {
            if let Some(why) = why {
                Shape::Lane(
                    '⚠',
                    Severity::Warn,
                    format!(
                        "{kind} — could not read this daemon's shape ({}) · {}",
                        clip(&flat(why), 200),
                        clip(&one_line(raw), cap)
                    ),
                )
            } else if *in_daemon_vocabulary {
                Shape::Silent(format!("{kind} · {}", clip(&one_line(raw), cap)))
            } else {
                Shape::Silent(format!(
                    "unknown event kind {kind} · {}",
                    clip(&one_line(raw), cap)
                ))
            }
        }
    }
}

/// How long a headline gets before it is an essay. Short enough that a
/// `task_notification` row RENDERS as expandable — which is the affordance his
/// *"I should be able to expand events like that"* asks for — and long enough
/// that most of them need no expanding at all.
const HEADLINE: usize = 120;

/// The task's human description, as ` · <description>`, or nothing at all.
/// Never the hash: a row with no name available says less, rather than saying
/// something useless.
fn named(ctx: &Ctx, task_id: &str) -> String {
    match ctx.task_names.get(task_id) {
        Some(d) if !d.is_empty() => format!(" · {}", clip(d, HEADLINE)),
        _ => String::new(),
    }
}

/// Whether a tool call has come back yet, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Running,
    Ok,
    Failed,
}

/// **ONE ROW FOR A TOOL CALL AND ITS RESULT**, his words: *"tool result should
/// just become merged to tool · type / args / results etc."* (`DEC-33`).
///
/// Collapsed it is the shape of the act — what tool, on what, and whether it
/// worked. Expanded it is the whole call and the whole result, which is the
/// only place those bytes were ever going to be readable.
fn tool_row(
    name: &str,
    input: &Value,
    outcome: Outcome,
    content: Option<&Value>,
    expanded: bool,
    cap: usize,
) -> (Severity, String) {
    let sev = match outcome {
        Outcome::Failed => Severity::Critical,
        Outcome::Running => Severity::Unknown,
        Outcome::Ok => Severity::Calm,
    };
    let word = match outcome {
        Outcome::Running => "running",
        Outcome::Ok => "ok",
        Outcome::Failed => "failed",
    };
    let args = match expanded {
        true => clip(&one_line(input), cap),
        false => args_gist(input),
    };
    let mut text = format!("tool · {name}");
    if !args.is_empty() {
        text.push_str(&format!(" · {args}"));
    }
    text.push_str(&format!(" · {word}"));
    if let Some(c) = content {
        // The result body is the expensive half and the reason to expand. A
        // collapsed row says the outcome and stops; `ok` plus four kilobytes of
        // stdout is the row this merge exists to stop being.
        if expanded {
            text.push_str(&format!(" · {}", clip(&one_line(c), cap)));
        }
    }
    (sev, text)
}

/// **THE TELLING FIELD OF A TOOL'S ARGUMENTS**, not the whole JSON.
///
/// `{"command":"git status","description":"…","timeout":120}` is one useful
/// string and two he does not need at a glance. The priority list is the set of
/// keys the tools he actually runs put their subject in; the fallback is the
/// first string field, and the fallback's fallback is the object itself, so a
/// tool nobody anticipated still shows something.
fn args_gist(input: &Value) -> String {
    const TELLING: &[&str] = &[
        "command",
        "file_path",
        "pattern",
        "path",
        "query",
        "url",
        "prompt",
        "description",
        "skill",
        "old_string",
    ];
    let Some(obj) = input.as_object() else {
        return match input.is_null() {
            true => String::new(),
            false => clip(&flat(&one_line(input)), HEADLINE),
        };
    };
    for k in TELLING {
        if let Some(s) = obj.get(*k).and_then(Value::as_str) {
            if !s.is_empty() {
                return clip(&flat(s), HEADLINE);
            }
        }
    }
    match obj.values().find_map(Value::as_str) {
        Some(s) => clip(&flat(s), HEADLINE),
        None => clip(&one_line(input), HEADLINE),
    }
}

/// **`result_turn.model_usage` AS WORDS — AND IT IS THE SESSION'S TOTAL, NOT
/// THE TURN'S, WHICH IS WHY IT IS NOT ON THE COLLAPSED ROW.**
///
/// # The measurement that decided this, and why it overrides the brief
///
/// `0047` D4 established that `result_turn` is the honest usage frame, and it
/// is — but it did not ask whether `model_usage` is per TURN. It is not.
/// Measured on a scratch copy of the live ledger: **6,715 of 7,668 adjacent
/// `result_turn` pairs carry a BYTE-IDENTICAL `model_usage`** (87.6%), and a
/// burst of three turns stamped to the same millisecond reads
///
/// ```text
/// seq 175425  total_cost_usd 11.63  num_turns 5   model_usage sum $54.0886
/// seq 175426  total_cost_usd 44.12  num_turns 2   model_usage sum $54.0886
/// seq 175427  total_cost_usd 54.09  num_turns 2   model_usage sum $54.0886
/// ```
///
/// The per-model costs sum to the LARGEST `total_cost_usd` of the burst, not to
/// each row's own. So `model_usage` is a **cumulative session decomposition,
/// repeated once per turn** — structurally the same pathology `0047` D3 found
/// one level down in `message_usage`, where the same snapshot was repeated once
/// per content block.
///
/// **Putting `out 350631` on three consecutive turn rows would therefore
/// recreate, exactly, the row this task was written to delete.** His objection
/// was never to the format — it was to a number that is not true of the thing
/// it is printed beside. So:
///
/// * the COLLAPSED row carries only what is genuinely per-turn —
///   `total_cost_usd`, `num_turns`, `duration_ms`, `subtype` — every one of
///   which does differ row to row in the data above;
/// * the decomposition moves into the EXPANSION and is **labelled `session so
///   far`**, which is what it is.
///
/// Recorded as a deviation from the brief's letter in `0051`'s report, with
/// this measurement as its argument.
fn usage_words(model_usage: &Value, expanded: bool) -> String {
    let Some(obj) = model_usage.as_object().filter(|o| !o.is_empty()) else {
        return String::new();
    };
    if !expanded {
        // Collapsed, the breakdown is only ANNOUNCED — so the row says there is
        // something behind it without asserting anything untrue on its face.
        return format!(" · {} model(s) this session", obj.len());
    }
    let one = |model: &str, u: &Value| -> String {
        let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        format!(
            "{model} out {} · in {} · cache read {} · cache write {}{}{}",
            n("outputTokens"),
            n("inputTokens"),
            n("cacheReadInputTokens"),
            n("cacheCreationInputTokens"),
            match n("thinkingTokens") {
                0 => String::new(),
                t => format!(" · thinking {t}"),
            },
            u.get("costUSD")
                .and_then(Value::as_f64)
                .map(|c| format!(" · ${c:.2}"))
                .unwrap_or_default()
        )
    };
    let all: Vec<String> = obj.iter().map(|(m, u)| one(m, u)).collect();
    format!(" · session so far · {}", all.join(" · "))
}

/// **THE NINE LIVE `unknown` SUBTYPES, EACH GIVEN ITS VERDICT** — the
/// `unknown_subtype_inventory` of `0047`, implemented.
///
/// The verdicts are not taste: they come from a census of 236,384 ledger rows
/// restricted to what still ARRIVES. Two thirds of the live volume is
/// telemetry (`thinking_tokens` alone is 43%), and the historical `task_*`
/// forms that dominate the lifetime count have ZERO live rows because the
/// daemon has since typed them — which is why this function is small and the
/// number behind it is large.
fn unknown_shape(raw: &Value, expanded: bool, cap: usize) -> Shape {
    let sub = subtype_of(raw);
    let raw_line = || format!("{sub} · {}", clip(&one_line(raw), cap));
    match sub.as_str() {
        // ── machinery: it says nothing a turn boundary does not ──
        // thinking_tokens duplicates an estimate already inside the thinking
        // deltas; tool_progress is a per-N-second heartbeat; status is "an HTTP
        // request is in flight"; the five task_* forms are pre-typing history
        // with no live volume at all.
        "system/thinking_tokens"
        | "tool_progress"
        | "system/status"
        | "system/task_progress"
        | "system/task_started"
        | "system/task_notification"
        | "system/task_updated"
        | "system/background_tasks_changed"
        | "system/code_change_published"
        | "system/commands_changed" => Shape::Silent(raw_line()),

        // ── content that was being thrown away as JSON ──
        // The whole `rate_limit_info`, which the typed kind does not carry.
        "rate_limit_event" => {
            let info = raw.get("rate_limit_info").unwrap_or(&Value::Null);
            let pct = |window: &str| -> Option<String> {
                let u = info
                    .get("unifiedWindows")?
                    .get(window)?
                    .get("utilization")?
                    .as_f64()?;
                Some(format!("{:.0}%", u * 100.0))
            };
            let five = pct("five_hour");
            let seven = pct("seven_day");
            if five.is_none() && seven.is_none() {
                return Shape::Silent(raw_line());
            }
            let overage = info
                .get("isUsingOverage")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Shape::Lane(
                '⚠',
                Severity::Warn,
                format!(
                    "limits{}{}{}",
                    five.map(|p| format!(" · 5h {p}")).unwrap_or_default(),
                    seven.map(|p| format!(" · 7d {p}")).unwrap_or_default(),
                    if overage { " · on overage" } else { "" }
                ),
            )
        }
        "system/api_retry" => {
            let n = |k: &str| raw.get(k).and_then(Value::as_u64);
            Shape::Lane(
                '⚠',
                Severity::Warn,
                format!(
                    "api retry{}{}{}",
                    match (n("attempt"), n("max_retries")) {
                        (Some(a), Some(m)) => format!(" · attempt {a}/{m}"),
                        (Some(a), None) => format!(" · attempt {a}"),
                        _ => String::new(),
                    },
                    n("retry_delay_ms")
                        .map(|d| format!(" · in {}", ms(d)))
                        .unwrap_or_default(),
                    raw.get("error")
                        .and_then(Value::as_str)
                        .filter(|e| !e.is_empty() && *e != "unknown")
                        .map(|e| format!(" · {}", clip(&flat(e), HEADLINE)))
                        .unwrap_or_default()
                ),
            )
        }
        // **HE MUST NEVER LEARN A MODEL SWAP FROM A JSON BLOB.** The model was
        // refused and silently replaced; 162 of these happened and every one of
        // them was invisible.
        "system/model_refusal_fallback" => Shape::Lane(
            '⚠',
            Severity::Critical,
            format!(
                "model refused{} · swapped · {}",
                raw.get("api_refusal_category")
                    .and_then(Value::as_str)
                    .map(|c| format!(" · {c}"))
                    .unwrap_or_default(),
                clip(
                    &flat(raw.get("content").and_then(Value::as_str).unwrap_or("")),
                    if expanded { cap } else { HEADLINE }
                )
            ),
        ),
        "system/permission_denied" => Shape::Lane(
            '⚠',
            Severity::Critical,
            format!(
                "permission denied{} · {}",
                raw.get("decision_reason_type")
                    .and_then(Value::as_str)
                    .map(|t| format!(" · {t}"))
                    .unwrap_or_default(),
                clip(
                    &flat(
                        raw.get("decision_reason")
                            .or_else(|| raw.get("message"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                    ),
                    if expanded { cap } else { HEADLINE }
                )
            ),
        ),
        "system/vcs_state_changed" => Shape::Lane(
            '·',
            Severity::Calm,
            format!(
                "{}{}",
                raw.get("kind").and_then(Value::as_str).unwrap_or("vcs"),
                raw.get("cwd")
                    .and_then(Value::as_str)
                    .map(|c| format!(" · {c}"))
                    .unwrap_or_default()
            ),
        ),
        // **REAL INJECTED CONTEXT, not machinery** — a skill body being loaded
        // into the turn. It reads as `isSynthetic` on the wire and as a wall of
        // JSON on screen; one line and an expansion is what it always wanted.
        "user" => {
            let text = synthetic_user_text(raw);
            match text.is_empty() {
                true => Shape::Silent(raw_line()),
                false => Shape::Lane(
                    '·',
                    Severity::Calm,
                    format!(
                        "context injected · {}",
                        clip(&flat(&text), if expanded { cap } else { HEADLINE })
                    ),
                ),
            }
        }
        // **NO ROW SAYS `unknown`.** A kind that reached here is one nothing in
        // this build recognises, and a row that can only report its own
        // ignorance is the row he ruled out. `x` still reaches it, which is the
        // whole of `DIR-4`'s breadth promise under the retraction.
        _ => Shape::Silent(raw_line()),
    }
}

/// The text of a synthetic `user` line, which is nested two levels down inside
/// a content array. Returns empty when there is nothing readable there, which
/// sends the row back to [`Shape::Silent`] rather than rendering an empty one.
fn synthetic_user_text(raw: &Value) -> String {
    let Some(parts) = raw
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
    else {
        return String::new();
    };
    parts
        .iter()
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

fn meter(tokens: Option<u64>, tool_uses: Option<u64>, duration_ms: Option<u64>) -> String {
    let mut out = String::new();
    if let Some(d) = duration_ms {
        out.push_str(&format!(" · {}", ms(d)));
    }
    if let Some(t) = tool_uses {
        out.push_str(&format!(" · {t} tools"));
    }
    if let Some(t) = tokens {
        out.push_str(&format!(" · {}", tok(t)));
    }
    out
}

fn tok(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M tok", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}k tok", n / 1_000)
    } else {
        format!("{n} tok")
    }
}

fn ms(d: u64) -> String {
    let s = d / 1_000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3_600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h {:02}m", s / 3_600, (s % 3_600) / 60)
    }
}

// ────────────────────────────────────────────────────────────────────────────
//  the projection
// ────────────────────────────────────────────────────────────────────────────

// ────────────────────────────────────────────────────────────────────────────
//  LIVE ASSEMBLY — the message that builds itself out of its own deltas
//
//  His invitation: *"you're getting stream events, so I think building the
//  messages live is on the table anyway."* The design of record is `0048`; this
//  is its `state_machine` key as code, and the shapes below are named for the
//  things in it so the two can be read side by side.
//
//  THE ONE FACT THAT MAKES THIS CHEAP: the daemon already passes
//  `--include-partial-messages` unconditionally, so the deltas are not a new
//  ask — they are PERSISTED, seq-ordered ledger rows that were already
//  arriving and already being rendered, badly, one row per chunk. Nothing here
//  needs a daemon change and nothing here holds state between paints.
// ────────────────────────────────────────────────────────────────────────────

/// Which half of a message a block is. Tool-use blocks are deliberately absent:
/// the `tool_use` event renders those, and assembling their arguments from
/// `input_json_delta` is explicitly out of v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Text,
    Thought,
}

/// One content block of the message currently being written.
#[derive(Debug)]
struct Blk {
    /// The SSE `index`, which is how two blocks of one message are told apart.
    index: i64,
    part: Part,
    buf: String,
    /// The FIRST delta's event time — when the writing began. Not the arrival
    /// time, and not the time it finishes: honesty rule 1 both ways.
    ts: Option<DateTime<Utc>>,
    seq: u64,
    thread: String,
    /// This block was opened by a delta with no `content_block_start` in front
    /// of it — a replay that began mid-block, or a prefix evicted by
    /// [`OPEN_CAP`]. It renders with a leading `…` so a SUFFIX is never
    /// presented as a whole message.
    implicit: bool,
    /// Where its live row currently sits in the output.
    slot: Option<usize>,
}

/// The message being assembled, for ONE thread.
#[derive(Debug, Default)]
struct Asm {
    /// The API's own `message.id` off `message_start`. Used as the block
    /// identity so a drag selection does not tear as the text grows.
    api_id: Option<String>,
    blocks: Vec<Blk>,
}

/// How an open block stopped being open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// `message_stop`: generation completed. The words stand as a normal box.
    Final,
    /// A turn-terminal event arrived with the message still open. The words
    /// stand, and the run header SAYS SO — in words, per `0040`.
    Interrupted,
}

/// **THE STATE MACHINE'S INPUT SIDE.** One `stream_event`, folded in.
fn feed(a: &mut Asm, raw: &Value, thread: &str, r: &LedgerRow, out: &mut Vec<Option<Block>>) {
    // **SIDECHAIN DELTAS ARE NOT THIS THREAD'S SPEECH** and are excluded in v1
    // — the same guard the AUI's streaming translator uses, for the same
    // reason: a subagent's words folded into the main chain's blocks would
    // corrupt them, and a subagent's reply already has a row of its own.
    if raw.get("parent_tool_use_id").is_some_and(|v| !v.is_null()) {
        return;
    }
    let Some(ev) = raw.get("event") else { return };
    let ty = ev.get("type").and_then(Value::as_str).unwrap_or("");
    let index = || ev.get("index").and_then(Value::as_i64).unwrap_or(0);
    match ty {
        // Defensive: a message opening while one is still open means we missed
        // the close. The old one is retired honestly rather than merged into.
        "message_start" => {
            retire_all(a, out, Ending::Interrupted);
            a.api_id = ev
                .get("message")
                .and_then(|m| m.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        "content_block_start" => {
            let part = match ev
                .get("content_block")
                .and_then(|c| c.get("type"))
                .and_then(Value::as_str)
            {
                Some("text") => Part::Text,
                Some("thinking") => Part::Thought,
                // `tool_use` blocks are ignored here on purpose: the `tool_use`
                // ledger event is what renders them, merged with its result.
                _ => return,
            };
            open_blk(a, index(), part, thread, r, false);
            place_all(a, out);
        }
        "content_block_delta" => {
            let Some(d) = ev.get("delta") else { return };
            let (part, text) = match d.get("type").and_then(Value::as_str) {
                Some("text_delta") => (Part::Text, d.get("text")),
                Some("thinking_delta") => (Part::Thought, d.get("thinking")),
                // `signature_delta` and `input_json_delta` carry nothing he
                // reads; they are state, consumed silently.
                _ => return,
            };
            let Some(text) = text.and_then(Value::as_str) else {
                return;
            };
            let i = index();
            if !a.blocks.iter().any(|b| b.index == i && b.part == part) {
                open_blk(a, i, part, thread, r, true);
            }
            if let Some(b) = a.blocks.iter_mut().find(|b| b.index == i && b.part == part) {
                b.buf.push_str(text);
            }
            place_all(a, out);
        }
        // A block's own ledger line is what retires it; this frame arrives
        // AFTER that line and would retire it twice.
        "content_block_stop" => {}
        // `stop_reason` and the turn's real usage. Usage is never rendered near
        // the box — his ruling — and the honest totals are `result_turn`'s.
        "message_delta" => {}
        "message_stop" => retire_all(a, out, Ending::Final),
        _ => {}
    }
}

fn open_blk(a: &mut Asm, index: i64, part: Part, thread: &str, r: &LedgerRow, implicit: bool) {
    a.blocks.push(Blk {
        index,
        part,
        buf: String::new(),
        ts: r.ts,
        seq: r.seq,
        thread: thread.to_string(),
        implicit,
        slot: None,
    });
}

/// **RIDE AT THE NEWEST DELTA** (`DEC-12`'s mechanism, applied to speech).
///
/// The live row is re-slotted to the walk's current end, so the growing box is
/// always the newest thing on a bottom-recency transcript (`DIR-3`) and the
/// authoritative row that retires it one seq later lands in the same place —
/// which is why completion causes NO VISUAL JUMP.
///
/// Holing the old slot rather than leaving it is what keeps the BLOCK COUNT
/// steady as the message grows — which is what stops the `↓ N new` chip
/// counting keystrokes, since it is derived from that count.
///
/// The `i + 1 == out.len()` arm is a cost saving on top and nothing more:
/// delta after delta with nothing in between updates the row where it stands,
/// so the holes left behind are proportional to how often something
/// INTERLEAVES rather than to how many deltas arrived. Removing it changes
/// no output, only the size of the vector the walk builds — it is named here
/// as an optimization so nobody later reads it as a correctness mechanism.
fn place_all(a: &mut Asm, out: &mut Vec<Option<Block>>) {
    let api_id = a.api_id.clone();
    for b in a.blocks.iter_mut() {
        let block = live_block(b, api_id.as_deref());
        match b.slot {
            Some(i) if i + 1 == out.len() => out[i] = Some(block),
            Some(i) => {
                out[i] = None;
                b.slot = Some(out.len());
                out.push(Some(block));
            }
            None => {
                b.slot = Some(out.len());
                out.push(Some(block));
            }
        }
    }
}

/// The in-progress row. A normal assistant box with a word on its header — the
/// pure renderer re-wraps it at the paint width every frame, so a lengthening
/// text needs no design of its own.
fn live_block(b: &Blk, api_id: Option<&str>) -> Block {
    let text = match b.implicit {
        true => format!("…{}", b.buf),
        false => b.buf.clone(),
    };
    match b.part {
        Part::Text => Block::Message {
            id: BlockId {
                thread_id: b.thread.clone(),
                // Stable while it grows, which is what stops a drag selection
                // tearing mid-growth.
                message_id: api_id
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("seq{}", b.seq)),
            },
            sender: Who::Assistant,
            ts: b.ts,
            text,
            note: Some("writing".into()),
            // The wire attaches metadata to USER inputs. An assistant box has
            // none, and inventing a row to fill the space would be honesty
            // rule 1.
            meta: Vec::new(),
            key: Some((b.thread.clone(), b.seq)),
            // A live block shows every word it has — there is nothing behind it
            // to open, so it is neither expandable nor open and draws no mark.
            expandable: false,
            open: false,
        },
        // **THE FACE'S LONGEST SILENCE, ENDED.** The `thinking` ledger event
        // only lands when the block COMPLETES, so without this row the
        // transcript shows nothing at all for the minutes he most wants it to
        // look alive.
        Part::Thought => Block::Lane {
            ts: b.ts,
            glyph: '·',
            severity: Severity::Unknown,
            text: format!(
                "thinking · writing · {} line(s) so far",
                b.buf.lines().count().max(1)
            ),
            count: None,
            key: Some((b.thread.clone(), b.seq)),
            expandable: false,
            open: false,
            affordance: None,
        },
    }
}

/// Retire every open block, leaving its words on screen as a finished thing.
fn retire_all(a: &mut Asm, out: &mut [Option<Block>], how: Ending) {
    for b in a.blocks.iter() {
        seal(b, out, how, a.api_id.as_deref());
    }
    a.blocks.clear();
    a.api_id = None;
}

/// Turn one live row into a finished one, in place. An EMPTY block leaves
/// nothing behind: a box with no words in it is not a message.
fn seal(b: &Blk, out: &mut [Option<Block>], how: Ending, api_id: Option<&str>) {
    let Some(i) = b.slot else { return };
    if b.buf.is_empty() {
        out[i] = None;
        return;
    }
    let mut block = live_block(b, api_id);
    let note = match how {
        Ending::Final => None,
        Ending::Interrupted => Some("interrupted mid-reply".to_string()),
    };
    match &mut block {
        Block::Message { note: n, .. } => *n = note,
        Block::Lane { text, .. } => {
            *text = format!(
                "thinking · {}{}",
                match how {
                    Ending::Final => String::new(),
                    Ending::Interrupted => "interrupted mid-reply · ".into(),
                },
                clip(&flat(&b.buf), HEADLINE)
            )
        }
        Block::Rule { .. } => {}
    }
    out[i] = Some(block);
}

/// **THE STATE MACHINE'S LEDGER SIDE.** Every non-delta event is a candidate to
/// retire an open block, and the rule is the same one throughout: THE LEDGER
/// REPLACES THE ASSEMBLY, unconditionally.
///
/// No prefix-suppression, no reconciliation, no comparison. The complete
/// assistant line is byte-identical to the concatenation of its own deltas, so
/// replacing is visually a no-op — and on the day identity ever breaks, the
/// ledger is the one that is right.
fn retire_for(a: &mut Asm, ev: &ThreadEvent, out: &mut [Option<Block>]) {
    match ev {
        // Main chain only. A sidechain reply retires nothing — it was never
        // assembled (see the guard in `feed`).
        ThreadEvent::AssistantText {
            parent_tool_use_id: None,
            ..
        } => drop_oldest(a, out, Part::Text),
        // **AN EMPTY `thinking` ROW IS NOT TRUTH ABOUT THE WORDS.** The daemon
        // emits many with `text: ""`, and letting one of those replace a fully
        // assembled reasoning block would delete what he was reading in favour
        // of a row that says nothing. Both are ledger rows; the one with the
        // words in it is the one that describes what happened. A NON-empty
        // thinking row replaces as normal.
        ThreadEvent::Thinking { text } if text.is_empty() => {
            let api = a.api_id.clone();
            if let Some(pos) = a.blocks.iter().position(|b| b.part == Part::Thought) {
                seal(&a.blocks[pos], out, Ending::Final, api.as_deref());
                a.blocks.remove(pos);
            }
        }
        ThreadEvent::Thinking { .. } => drop_oldest(a, out, Part::Thought),
        // **EVERY PATH OFF THE HAPPY PATH HAS AN EVENT THAT ALWAYS ARRIVES** —
        // the daemon guarantees `turn_failed` on exhaustion and
        // `turn_interrupted` on a restart orphaning a turn — so a stream that
        // never finalises is a state that resolves on the next event or the
        // next replay, never a permanent lie.
        ThreadEvent::TurnEnded { .. }
        | ThreadEvent::TurnFailed { .. }
        | ThreadEvent::TurnInterrupted { .. }
        | ThreadEvent::TurnKilled { .. }
        | ThreadEvent::ThreadSystemInterrupted { .. } => retire_all(a, out, Ending::Interrupted),
        _ => {}
    }
}

/// The ledger row is about to render this block's words itself, so the live row
/// makes way entirely. Holing rather than sealing is what keeps the block COUNT
/// unchanged across completion — which is what keeps the `↓ N new` chip from
/// counting a message twice.
fn drop_oldest(a: &mut Asm, out: &mut [Option<Block>], part: Part) {
    let Some(pos) = a.blocks.iter().position(|b| b.part == part) else {
        return;
    };
    if let Some(i) = a.blocks[pos].slot {
        out[i] = None;
    }
    a.blocks.remove(pos);
    if a.blocks.is_empty() {
        a.api_id = None;
    }
}

/// Which rows are open, per row key. See [`Block::key`] — a SET of ledger
/// identities, so it survives the re-projection that happens on every arrival.
pub type Opened = std::collections::HashSet<tui_render::model::RowKey>;

/// **THE ONE PLACE A LEDGER BECOMES BLOCKS**, for both the open thread and the
/// fleet lane. `label` names a thread the way a human would, which the fleet
/// lane needs and the open thread must not have.
///
/// # What this walk does beyond folding, since `0051`
///
/// It ASSEMBLES MESSAGES LIVE from `stream_event` deltas (the `0048` design of
/// record), it MERGES each tool call with its result into one row (`DEC-33`),
/// and it decides per row whether that row is OPEN — which is what makes
/// expansion a property of a row rather than of the whole transcript (`IS-31`).
///
/// **All three are FOLDS and none of them is state.** This function is pure
/// over the retained tail and is re-run per paint, which is what makes a
/// partially-assembled message survive a reconnect, a `Lagged` replay and a
/// cold open mid-turn with no recovery code at all: the deltas are persisted
/// ledger rows, so re-walking them rebuilds the same half-written box. An
/// in-memory accumulator would have needed all three paths written by hand and
/// would have lost the buffer on every one of them.
/// **THE LENS THE OPEN THREAD IS PROJECTED THROUGH** — which chat we are
/// looking at, and where the subagents' own rows went.
///
/// > *I don't want to see them in the chat area, it takes up 90% of the chat
/// > area. I only want things having to do with the viewed thread in view.*
///
/// Every row a subagent produced is a HOLE in the parent's transcript, and the
/// `Agent` call that launched it is ONE non-expanding TILE at its LAUNCH seq —
/// *"you can have a single info tile in that view per subagent… That's an
/// appropriate amount of redundancy."* Drill into a subagent and the same
/// mechanism runs with `subject` set to its id: its rows are the ones kept and
/// the parent's are the holes.
///
/// The fleet lane passes `None` and is untouched: it is a lane over every
/// thread's events and a subagent's rows are events.
pub struct Lens<'a> {
    /// Ledger seq → the subagent that produced that row.
    pub owner: &'a HashMap<u64, String>,
    /// Ledger seq → the tile that stands in for a launch at that seq.
    pub tiles: &'a HashMap<u64, String>,
    /// Whose chat this is. `None` is the thread itself.
    pub subject: Option<&'a str>,
}

fn project<'a>(
    rows: impl Iterator<Item = (&'a str, &'a LedgerRow)>,
    label: &dyn Fn(&str) -> String,
    stdout_only: bool,
    expanded: bool,
    opened: &Opened,
    lens: Option<&Lens>,
    frozen_at: Option<u64>,
) -> Vec<Block> {
    // COLLECTED, not streamed, and for one reason: the thin `rate_limit_event`
    // is only superseded when a RICH one is present, and that is a fact about
    // the whole tail rather than about a row. A bounded tail (`OPEN_CAP`) makes
    // one extra pass over borrowed pointers the cheapest way to know it.
    let rows: Vec<(&str, &LedgerRow)> = rows.collect();
    let ctx_rich = rows.iter().any(|(_, r)| {
        matches!(&r.event, ThreadEvent::Unknown { raw }
            if subtype_of(raw) == "rate_limit_event" && raw.get("rate_limit_info").is_some())
    });

    // `None` is a row a later event folded into itself — kept as a hole rather
    // than removed, so folding stays O(n) on a four-thousand-row transcript.
    let mut out: Vec<Option<Block>> = Vec::new();
    let mut per_task: HashMap<String, usize> = HashMap::new();
    let mut counts: HashMap<usize, u64> = HashMap::new();
    let mut run: Option<(String, usize)> = None;
    // `tool_use_id` -> (the row it occupies, the index of its ledger row).
    // The ROW INDEX rather than a clone of the call: a tool input can be
    // kilobytes and there can be hundreds of them in one tail.
    let mut tools: HashMap<String, (usize, usize)> = HashMap::new();
    let mut task_names: HashMap<String, String> = HashMap::new();
    // One assembly per thread. The fleet lane interleaves threads and their
    // streams are independent; the open thread has exactly one entry here.
    let mut asm: HashMap<String, Asm> = HashMap::new();

    for (row_ix, (thread_id, r)) in rows.iter().enumerate() {
        let thread_id = *thread_id;
        let kind = r.event.kind();
        let key: tui_render::model::RowKey = (thread_id.to_string(), r.seq);
        // **PER-ROW EXPANSION** — `x` opens everything, and a row he opened
        // himself is open on its own. This is the parameter `shape` has always
        // taken; what changed is that it is no longer one bool for the lot.
        let open = expanded || opened.contains(&key);

        // ── THE LENS: whose chat is this, and is this row in it? ───────────
        if let Some(lens) = lens {
            // **A ROW A SUBAGENT PRODUCED IS A HOLE IN THE PARENT'S CHAT**,
            // and the parent's rows are holes in the subagent's. One
            // comparison, both directions.
            if lens.owner.get(&r.seq).map(String::as_str) != lens.subject {
                continue;
            }
            // **THE ONE TILE.** It is drawn at the LAUNCH seq and never
            // relocates, it is not expandable, and `→` on it DRILLS — the
            // trail it used to expand into is now the subagent's own chat,
            // reached from the tree.
            if let Some(text) = lens.tiles.get(&r.seq) {
                run = None;
                let prefix = match label(thread_id).as_str() {
                    "" => String::new(),
                    l => format!("{l} "),
                };
                out.push(Some(Block::Lane {
                    ts: r.ts,
                    glyph: '◇',
                    severity: Severity::Calm,
                    text: format!("{prefix}{text}"),
                    count: None,
                    key: Some(key.clone()),
                    expandable: false,
                    // **THE ONE ROW IN THE TRANSCRIPT THAT DECLARES A
                    // GESTURE.** It is not expandable, so there is no second
                    // state a cropped tail could be recovered from: without
                    // this the firm blob margins eat `→ opens its chat` at his
                    // own pane size and the drill becomes invisible from the
                    // chat, which is exactly the redundancy he authorised.
                    affordance: Some(TILE_AFFORDANCE.to_string()),
                    open: false,
                }));
                continue;
            }
        }

        // Learn every task's human name as it goes past, so a later row that
        // carries only a hash has something to say. (`0051` item 5.)
        remember_task_name(&mut task_names, &r.event);

        // ── THE ASSEMBLY EATS THE DELTAS, COLLAPSED OR NOT ─────────────────
        //
        // A `stream_event` is not a row any more; it is an INPUT. Whether its
        // RAW form is visible (which only happens under expansion at all) must
        // not govern whether the message he is reading gets BUILT — that would
        // make the stdout-only toggle delete his replies, which is the trap
        // `0048` names. So the feed happens FIRST, unconditionally, and the
        // stdout question is asked below like every other row's.
        //
        // **THERE USED TO BE A SECOND, STREAM-SPECIFIC GUARD RIGHT HERE** and
        // it is gone rather than translated: it dropped the row when the Stream
        // lane was off, and the general guard below already drops it when the
        // toggle is on, because a delta is not speech. Two guards that can only
        // ever agree are one guard and a place to disagree.
        if let ThreadEvent::StreamEvent { raw } = &r.event {
            let a = asm.entry(thread_id.to_string()).or_default();
            feed(a, raw, thread_id, r, &mut out);
        } else if let Some(a) = asm.get_mut(thread_id) {
            // Every other kind is a candidate to RETIRE an open block.
            retire_for(a, &r.event, &mut out);
        }

        // **THE TOGGLE IS APPLIED HERE AND ONLY HERE.** A row that is not
        // stdout never becomes a block while it is on, and the header says
        // `stdout only` — so what is on screen is either everything or
        // explicitly announced as less. With the toggle off this asks nothing
        // and drops nothing, which is `DEC-19`'s reachability promise held for
        // free: the default face applies no predicate at all.
        if stdout_only && !is_stdout(&r.event) {
            continue;
        }

        let ctx = Ctx {
            rich_rate_limits: ctx_rich,
            task_names: &task_names,
        };
        let shaped = shape(&r.event, open, &ctx);

        // **WHETHER OPENING THIS ROW WOULD SHOW HIM ANYTHING**, asked by
        // rendering it both ways rather than guessed from a trailing ellipsis.
        // Only asked when the row is closed: an open row's affordance is
        // "close", which every open row has.
        let meta = user_meta(&r.event);
        let expandable = match open {
            true => true,
            false => {
                let wide = shape(&r.event, true, &ctx);
                // **A MESSAGE WITH METADATA IS EXPANDABLE EVEN WHEN ITS WORDS
                // ARE WHOLE**, because the metadata IS the thing behind it.
                // *"we need a way to toggle viewing this"* — and the toggle he
                // gets is the one every other foldable row in this transcript
                // already has, rather than a second gesture with its own key.
                shaped.text() != wide.text() || !meta.is_empty()
            }
        };

        let prefix = match label(thread_id).as_str() {
            "" => String::new(),
            l => format!("{l} "),
        };

        let block = match shaped {
            // **A SILENT ROW IS NOT A ROW.** Under expansion it becomes its raw
            // self so `x` still reaches every retained event; closed, it does
            // not exist, which is his *"no rows that say unknown"* rule and the
            // stream-chunk retraction, implemented in the one place both apply.
            Shape::Silent(raw_text) => {
                if !open {
                    continue;
                }
                run = None;
                Block::Lane {
                    ts: r.ts,
                    glyph: '·',
                    severity: Severity::Unknown,
                    text: format!("{prefix}{raw_text}"),
                    count: None,
                    key: Some(key.clone()),
                    expandable: true,
                    // A silent row exists ONLY when it is open — the `continue`
                    // directly above is the closed case — so this is not a
                    // reading of `open` so much as a restatement of the arm.
                    open: true,
                    affordance: None,
                }
            }
            Shape::Message(who, text) => {
                run = None;
                Block::Message {
                    id: BlockId {
                        thread_id: thread_id.to_string(),
                        message_id: r
                            .message_id
                            .map(|m| m.to_string())
                            .unwrap_or_else(|| format!("seq{}", r.seq)),
                    },
                    sender: who,
                    ts: r.ts,
                    text,
                    note: None,
                    meta,
                    key: Some(key.clone()),
                    expandable,
                    open,
                }
            }
            Shape::Lane(glyph, severity, text) => Block::Lane {
                ts: r.ts,
                glyph,
                severity,
                text: format!("{prefix}{text}"),
                count: None,
                key: Some(key.clone()),
                expandable,
                open,
                affordance: None,
            },
            Shape::Rule(text) => {
                run = None;
                Block::Rule {
                    text: format!("{prefix}{text}"),
                }
            }
        };

        // ── THE MERGE (`DEC-33`) ──────────────────────────────────────────
        // A result rewrites its call's row in place and pushes nothing of its
        // own: *"tool result should just become merged to tool · type / args /
        // results etc."* The call's position is kept because that is where in
        // the story the tool was reached.
        if let ThreadEvent::ToolResult {
            tool_use_id,
            is_error,
            content,
        } = &r.event
        {
            if let Some((slot, use_ix)) = tools.remove(tool_use_id.as_str()) {
                if let ThreadEvent::ToolUse { name, input, .. } = &rows[use_ix].1.event {
                    // The MERGED row's openness follows the CALL's row, which
                    // is the one he clicked and the one still on screen.
                    let use_key: tui_render::model::RowKey =
                        (rows[use_ix].0.to_string(), rows[use_ix].1.seq);
                    let wide = expanded || opened.contains(&use_key);
                    let cap = if wide { 8_000 } else { 400 };
                    let outcome = if *is_error {
                        Outcome::Failed
                    } else {
                        Outcome::Ok
                    };
                    let (sev, text) = tool_row(name, input, outcome, Some(content), wide, cap);
                    if let Some(Some(Block::Lane {
                        severity,
                        text: t,
                        expandable,
                        open,
                        ..
                    })) = out.get_mut(slot)
                    {
                        *severity = sev;
                        *t = format!("{prefix}{text}");
                        // A merged row always has more behind it: the full
                        // arguments and the full result.
                        //
                        // **THIS SAID `!wide`, AND THAT WAS A DEFECT `0056`
                        // FOUND BY DRAWING IT.** Every other row in this
                        // projection is expandable WHEN OPEN — the affordance
                        // of an open row is "close", which is the rule stated
                        // at the `match open` above and the one `app.rs`'s
                        // `keyboard_target` relies on. A merged row alone said
                        // the opposite, so opening one made its `expandable`
                        // false, and once the expander became a DRAWN mark that
                        // meant the marker VANISHED at the moment he opened the
                        // row — no `▾`, and no way back but `x`. Invisible
                        // while one glyph stood for both states; not invisible
                        // now.
                        *expandable = true;
                        // Openness follows the CALL's row, which is the one he
                        // clicked and the one still on screen. It is already
                        // `wide` by construction — the call was projected under
                        // this same key — and it is written here anyway so the
                        // rewrite sets every field it is responsible for.
                        *open = wide;
                    }
                    run = None;
                    continue;
                }
            }
            // No call in view. The orphan arm of `shape` already said so.
        }
        if let ThreadEvent::ToolUse { id, .. } = &r.event {
            tools.insert(id.clone(), (out.len(), row_ix));
        }

        let fold = if open { Fold::Never } else { fold_of(&r.event) };
        match fold {
            Fold::Never => {
                run = None;
                out.push(Some(block));
            }
            // THE LIVE ROW (`DEC-12`). A task's row moves to where the task
            // currently IS: the old row becomes a hole and a new one is pushed
            // at the end with the running count. On a bottom-recency transcript
            // that means a working task rides down with the newest thing, which
            // is what "live" has to mean when the newest row is at the floor.
            Fold::PerTask => {
                run = None;
                let k = task_key(&r.event);
                match per_task.get(&k).copied() {
                    // **THE FREEZE IS A LEDGER SEQ, AND THIS IS WHY IT HAD TO
                    // BE.**
                    //
                    // A live row does not APPEND — it RELOCATES: the old slot
                    // becomes a hole and a new one is pushed at the end, so
                    // `blocks.len()` never changes. A freeze that counts
                    // BLOCKS therefore reports ZERO arrivals while every row
                    // below the relocation slides up by one, and his text
                    // jumps under his eyes with `↓ 0 new` on the screen. It is
                    // not hypothetical and it is not only about a future
                    // aggregate: `Fold::PerTask` does this today, on every
                    // `task_progress` beat.
                    //
                    // While he is away from the bottom, the row updates IN
                    // PLACE instead: nothing at or above `frozen_at` moves,
                    // the row still shows its newest state, and it takes its
                    // live position back on the next thaw — the one
                    // deliberate jump he asked for.
                    Some(prev) if held_by(&out, prev, frozen_at) => {
                        let n = counts.get(&prev).copied().unwrap_or(1) + 1;
                        out[prev] = Some(with_count(block, n));
                        counts.insert(prev, n);
                    }
                    prev => {
                        let n = match prev {
                            Some(prev) => {
                                out[prev] = None;
                                counts.remove(&prev).unwrap_or(1) + 1
                            }
                            None => 1,
                        };
                        let idx = out.len();
                        out.push(Some(with_count(block, n)));
                        counts.insert(idx, n);
                        per_task.insert(k, idx);
                    }
                }
            }
            Fold::PerRun => {
                // **PER THREAD, BECAUSE THE FLEET LANE INTERLEAVES THEM.**
                // `activity_blocks` projects every thread's events into one
                // lane and `label_of` is EMPTY for a thread the index has not
                // named, so two threads' results are textually identical
                // there. A run keyed on the event alone would have merged them
                // and reported another thread's turn as this one's.
                let fk = format!("{thread_id}\u{0}{}", fold_key(&r.event));
                match run.as_ref() {
                    // Same fact, still contiguous: replace the row in place so
                    // the fold shows the NEWEST of the run.
                    Some((k, idx)) if *k == fk => {
                        let idx = *idx;
                        let n = counts.get(&idx).copied().unwrap_or(1) + 1;
                        out[idx] = Some(with_count(block, n));
                        counts.insert(idx, n);
                    }
                    _ => {
                        let idx = out.len();
                        out.push(Some(block));
                        counts.insert(idx, 1);
                        run = Some((fk, idx));
                    }
                }
            }
        }
        let _ = kind;
    }
    out.into_iter().flatten().collect()
}

/// **IS THE BLOCK AT `slot` INSIDE THE FROZEN PREFIX?** — the one question the
/// seq freeze asks. Not frozen, or a block with no ledger key, are both "no":
/// a row nothing can address cannot be held still by address.
fn held_by(out: &[Option<Block>], slot: usize, frozen_at: Option<u64>) -> bool {
    let Some(frozen_at) = frozen_at else {
        return false;
    };
    matches!(out.get(slot), Some(Some(b)) if b.key().is_some_and(|(_, q)| *q <= frozen_at))
}

/// A `task_progress` row folds per `task_id`; anything else that ever gains
/// [`Fold::PerTask`] must say what its key is here rather than silently sharing
/// one.
fn task_key(ev: &ThreadEvent) -> String {
    match ev {
        ThreadEvent::TaskProgress { task_id, .. } => task_id.clone(),
        other => other.kind().to_string(),
    }
}

/// Learn a task's human description from whichever row happens to carry it, so
/// the rows that carry only a hash have a name to print. See [`Ctx::task_names`].
fn remember_task_name(names: &mut HashMap<String, String>, ev: &ThreadEvent) {
    let pair = match ev {
        ThreadEvent::TaskStarted {
            task_id,
            description: Some(d),
            ..
        }
        | ThreadEvent::TaskProgress {
            task_id,
            description: Some(d),
            ..
        } => Some((task_id, d)),
        _ => None,
    };
    if let Some((id, d)) = pair {
        if !d.is_empty() {
            names.insert(id.clone(), flat(d));
        }
    }
}

/// `×n`, but only once there is more than one — a `×1` on every row would be
/// noise standing for nothing.
fn with_count(block: Block, n: u64) -> Block {
    match block {
        Block::Lane {
            ts,
            glyph,
            severity,
            text,
            key,
            expandable,
            open,
            affordance,
            ..
        } => Block::Lane {
            ts,
            glyph,
            severity,
            text,
            count: (n > 1).then_some(n),
            key,
            // **A ROW STANDING FOR SEVERAL EVENTS ALWAYS HAS SOMETHING BEHIND
            // IT**, so it always offers the marker (`0132`). `expandable` is
            // otherwise derived by rendering the row open and closed and
            // comparing — and a burst of results on ONE model renders
            // identically both ways, so the collapsed row would have carried
            // `×5` and no affordance at all. Opening it disarms the fold for
            // that row, which is exactly the drill the marker promises.
            expandable: expandable || n > 1,
            affordance,
            // CARRIED, not recomputed: stamping a `×n` on a row says how many
            // events it stands for and says nothing about whether it is open.
            open,
        },
        other => other,
    }
}

/// **THE OPEN THREAD'S ROWS**, in ledger order, as both the aggregation and
/// the projection want them. One borrow, read twice — the aggregation and the
/// transcript must not disagree about which rows exist.
pub fn open_rows(store: &Store) -> Vec<(&str, &LedgerRow)> {
    match store.open.as_ref() {
        None => Vec::new(),
        Some(open) => open
            .rows
            .values()
            .map(|r| (open.id.as_str(), r))
            .collect(),
    }
}

/// **WHICH SUBAGENT PRODUCED EACH ROW** — ledger seq → the opaque id of the
/// `Agent` call that owns it, for every row that one does.
///
/// This is the whole of *"I only want things having to do with the viewed
/// thread in view"*: a row with an owner is a hole in the parent's chat and a
/// row of that owner's own chat. The linkage is a KEY and not a heuristic —
/// `0080` measured 17,644 of 17,644 resolved `parent_tool_use_id`s resolving
/// to an `Agent` call, so nothing else can be a parent.
pub fn attribution(rows: &[(&str, &LedgerRow)]) -> HashMap<u64, String> {
    // tool_use id -> the subagent that owns that call. A LAUNCH owns itself:
    // its result is the subagent's own summary, which belongs in the
    // subagent's chat and not in the parent's.
    let mut owner_of_tool: HashMap<String, String> = HashMap::new();
    // task_id -> owner, resolved through `task_started.tool_use_id`. A later
    // start on a seen id is a RESUME and supersedes: `task_id` is not unique.
    let mut owner_of_task: HashMap<String, String> = HashMap::new();
    let mut out: HashMap<u64, String> = HashMap::new();

    for (_, r) in rows {
        if let ThreadEvent::ToolUse {
            id,
            name,
            parent_tool_use_id,
            ..
        } = &r.event
        {
            if name == "Agent" || name == "SendMessage" {
                owner_of_tool.insert(id.clone(), id.clone());
            } else if let Some(p) = parent_tool_use_id {
                owner_of_tool.insert(id.clone(), p.clone());
            }
        }
    }
    for (_, r) in rows {
        if let ThreadEvent::TaskStarted {
            task_id,
            tool_use_id: Some(tu),
            ..
        } = &r.event
        {
            match owner_of_tool.get(tu) {
                Some(o) => owner_of_task.insert(task_id.clone(), o.clone()),
                None => owner_of_task.remove(task_id),
            };
        }
    }
    for (_, r) in rows {
        let owner = match &r.event {
            // A LAUNCH is not a hole — it is the TILE. Its own row stays in
            // whichever chat it was made from.
            ThreadEvent::ToolUse {
                name,
                parent_tool_use_id,
                ..
            } if name == "Agent" || name == "SendMessage" => parent_tool_use_id.clone(),

            ThreadEvent::ToolUse {
                parent_tool_use_id, ..
            } => parent_tool_use_id.clone(),
            ThreadEvent::AssistantText {
                parent_tool_use_id, ..
            } => parent_tool_use_id.clone(),
            ThreadEvent::ToolResult { tool_use_id, .. } => owner_of_tool.get(tool_use_id).cloned(),
            ThreadEvent::TaskStarted { task_id, .. }
            | ThreadEvent::TaskUpdated { task_id, .. }
            | ThreadEvent::TaskProgress { task_id, .. }
            | ThreadEvent::TaskNotification { task_id, .. } => owner_of_task.get(task_id).cloned(),
            _ => None,
        };
        // **AN ATTRIBUTED ROW WHOSE LAUNCH IS NOT IN THE TAIL STAYS WHERE IT
        // IS.** The tail is bounded, so a subagent's reply can outlive the
        // `Agent` call that started it — and a row holed out of the parent's
        // chat with no tree row to reach it would be a row DROPPED, which is
        // the one thing this face promises it never does. It is still a lane
        // row saying a subagent said it; it simply has no home to be moved to.
        if let Some(o) = owner.filter(|o| owner_of_tool.get(o).is_some_and(|t| t == o)) {
            out.insert(r.seq, o);
        }
    }
    out
}

/// **THE GESTURE THE TILE CARRIES**, and the reason the tile exists at all:
/// it is what makes the subagent tree discoverable from the chat instead of
/// only from the band. It is the SUFFIX of every tile's text and it is handed
/// to the renderer separately, so the firm blob margins ellipsize the label
/// rather than the gesture. See [`tiles_of`].
pub const TILE_AFFORDANCE: &str = " — → opens its chat";

/// **THE ONE INFO TILE PER SUBAGENT**, keyed by the seq it is drawn at.
///
/// > *you can have a single info tile in that view per subagent, so you can
/// > see them in the chat. That's an appropriate amount of redundancy.*
///
/// It carries NO DURATION, deliberately. A duration formatted here would be
/// frozen at whatever the clock said when the projection ran — the defect this
/// file's own clock rules name — and the live figures are read in the tree,
/// which subtracts them from the `now` the frame is handed. What the tile
/// carries is what does not move: who, what state, how many tokens.
///
/// **AND ITS LAST CLAUSE IS AN AFFORDANCE, NOT PROSE.** [`TILE_AFFORDANCE`] is
/// what makes the tree drillable FROM THE CHAT rather than only from the band,
/// and the tile is not expandable — so a crop that ate it would delete the
/// gesture with nothing on the screen to say so. It is declared to the renderer
/// as the row's affordance and reserved there before anything else is measured.
fn tiles_of(agents: &[SubagentRow]) -> HashMap<u64, String> {
    agents
        .iter()
        .map(|a| {
            // THE SAME FORMATTER THE TREE ROW USES. Two spellings of one
            // number on one screen is two numbers that can disagree.
            let tokens = a
                .tokens
                .map(|t| format!(" · {}", tui_render::fmt_tokens(t)))
                .unwrap_or_default();
            (
                a.launched_at.1,
                format!(
                    "agent · {} · {}{tokens}{TILE_AFFORDANCE}",
                    a.label,
                    a.state.word()
                ),
            )
        })
        .collect()
}

/// **THE SUBAGENT TREE** — the parent thread as row 0, then every subagent in
/// LAUNCH order, depth-first under its own parent, with rails.
///
/// > *I just want to see the subagents, and their subagents. It looks like a
/// > file tree… Basically the text area above should be whichever one we've
/// > selected, so the parent needs to be an item in that list.*
///
/// The parent row is STRUCTURAL, not decoration: it is how he gets the thread
/// itself back as the subject, by the same `→`/`enter` that opened a subagent.
pub fn agent_tree(
    store: &Store,
    agents: &[SubagentRow],
    cursor: TreeId,
    subject: TreeId,
    ctx: &mut TreeCtx,
) -> AgentTree {
    let Some(open) = store.open.as_ref().map(|o| o.id.clone()) else {
        return AgentTree::default();
    };
    // **THE TREE IS KEPT TO THE ROOT** (TUI-P1b). It is drawn from the
    // outermost thread he came down through, not from the open one: entering
    // a child keeps its ancestors, its siblings and every fold he opened.
    let root = ctx.path.first().cloned().unwrap_or_else(|| open.clone());
    let mut rows = Vec::new();
    tree_node(store, &root, 0, true, None, &open, agents, ctx, &mut rows);
    // **The parent alone is not a tree.**
    if rows.len() < 2 {
        return AgentTree::default();
    }
    AgentTree {
        rows,
        cursor,
        subject,
        summary: store
            .fact(&open)
            .and_then(|f| f.rolled.as_ref())
            .and_then(|r| r.children_summary.clone()),
    }
}

/// **WHAT THE TREE REMEMBERS BETWEEN PROJECTIONS** (TUI-P1b) — held by the
/// app, so entering a child never rebuilds the tree from scratch.
#[derive(Debug, Default)]
pub struct TreeCtx {
    /// The threads he came down through, ROOT FIRST, the open one excluded.
    pub path: Vec<String>,
    /// Thread rows he unfolded, by thread id. Survives entering and leaving.
    pub expanded: std::collections::HashSet<String>,
    /// The direct (in-process) subagents each thread last showed while it was
    /// open, so an ancestor keeps its rows while a child is open.
    pub agents_of: HashMap<String, Vec<SubagentRow>>,
    /// Each thread's managed children as last read, so a thread whose
    /// `children` cell left the mirror's window keeps its rows.
    pub kids_of: HashMap<String, Vec<TreeRow>>,
}

/// **ONE THREAD'S ROW, THEN — WHEN IT IS UNFOLDED — EVERYTHING UNDER IT.**
///
/// `base` is the row its parent's `children` cell described (label, spawn
/// figures); the root has none. A thread is unfolded when it is the open one,
/// on the path to it, or one he expanded.
#[allow(clippy::too_many_arguments)]
fn tree_node(
    store: &Store,
    id: &str,
    depth: usize,
    last_sibling: bool,
    base: Option<TreeRow>,
    open: &str,
    agents: &[SubagentRow],
    ctx: &mut TreeCtx,
    out: &mut Vec<TreeRow>,
) {
    let is_open = id == open;
    let rolled = store.fact(id).and_then(|f| f.rolled.clone());
    let mut row = base.unwrap_or_else(|| TreeRow {
        label: Some(store.label_of(id))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "this thread".to_string()),
        ..TreeRow::default()
    });
    row.id = if is_open {
        TreeId::Parent
    } else {
        TreeId::Thread(id.to_string())
    };
    row.depth = depth;
    row.last_sibling = last_sibling;
    if depth == 0 {
        row.detail = store.activity(id).map(|a| a.long);
    }
    if let Some(r) = &rolled {
        row.status = Some(r.display.clone());
        row.children_summary = r.children_summary.clone();
    }
    // The children: direct ones from the transcript (the open thread's live,
    // an ancestor's as last seen), managed ones from the `children` cell.
    let direct: Vec<SubagentRow> = if is_open {
        if !agents.is_empty() {
            ctx.agents_of.insert(id.to_string(), agents.to_vec());
        }
        agents.to_vec()
    } else {
        ctx.agents_of.get(id).cloned().unwrap_or_default()
    };
    let fresh = store.managed_rows(id);
    let managed = if fresh.is_empty() {
        ctx.kids_of.get(id).cloned().unwrap_or_default()
    } else {
        ctx.kids_of.insert(id.to_string(), fresh.clone());
        fresh
    };
    let has_kids = !direct.is_empty() || !managed.is_empty();
    let unfolded = is_open || ctx.path.iter().any(|p| p == id) || ctx.expanded.contains(id);
    row.expanded = has_kids.then_some(unfolded);
    out.push(row);
    if !unfolded {
        return;
    }
    // Direct children by parent.
    let mut kids: HashMap<Option<String>, Vec<usize>> = HashMap::new();
    for (i, a) in direct.iter().enumerate() {
        kids.entry(a.parent.clone()).or_default().push(i);
    }
    // **MOST RECENTLY USED FIRST, at every level** (TC-10: "the most recently
    // used agents should be at the top of the list of subagents, not the
    // bottom"). Each sibling run is sorted by [`recency`].
    for v in kids.values_mut() {
        v.sort_by(|&x, &y| recency_order(direct_recency(&direct[x]), x, direct_recency(&direct[y]), y));
    }
    // A subagent whose own parent is not in the tail is drawn at the first
    // level rather than dropped: an orphan is a row we have.
    let known: std::collections::HashSet<&str> = direct.iter().map(|a| a.id.as_str()).collect();
    // **ONE SIBLING LIST FOR BOTH KINDS** (TC-10: "display both of them
    // [direct-invoke and managed] the same way as sub-agents"): the direct
    // (Task) roots and the managed children are merged and sorted together.
    enum Kid {
        Direct(usize),
        Managed(TreeRow, String),
    }
    let mut first: Vec<(Recency, usize, Kid)> = Vec::new();
    for (i, a) in direct.iter().enumerate() {
        match a.parent.as_deref() {
            Some(p) if known.contains(p) => {}
            _ => first.push((direct_recency(a), i, Kid::Direct(i))),
        }
    }
    for (k, m) in managed.into_iter().enumerate() {
        let TreeId::Thread(child) = m.id.clone() else {
            continue;
        };
        let r = managed_recency(store, &child, &m);
        // Managed rows rank after every direct row on a full tie, in spawn
        // order reversed — the newest spawn first.
        first.push((r, direct.len() + k, Kid::Managed(m, child)));
    }
    first.sort_by(|(rx, x, _), (ry, y, _)| recency_order(*rx, *x, *ry, *y));
    let n = first.len();
    for (k, (_, _, kid)) in first.into_iter().enumerate() {
        let last = k + 1 == n;
        match kid {
            Kid::Direct(i) => walk_tree(&direct, &kids, &[i], depth + 1, last, out),
            Kid::Managed(m, child) => {
                tree_node(store, &child, depth + 1, last, Some(m), open, agents, ctx, out)
            }
        }
    }
}

/// **HOW RECENTLY A SUBAGENT WAS USED** — `(active now, last time seen)`. An
/// active row is in use NOW, so it outranks any time; among the rest the
/// latest of start, end and (for a managed child) the daemon's `updated_at`.
type Recency = (bool, Option<DateTime<Utc>>);

fn is_active_status(status: Option<&str>) -> bool {
    status.is_some_and(|s| s == "active" || s.starts_with("active "))
}

fn direct_recency(a: &SubagentRow) -> Recency {
    (
        is_active_status(status_word(a.state)),
        a.started.max(a.ended),
    )
}

fn managed_recency(store: &Store, id: &str, m: &TreeRow) -> Recency {
    let updated = store.fact(id).and_then(|f| f.updated);
    (is_active_status(m.status.as_deref()), m.started.max(m.ended).max(updated))
}

/// Most recent first; on a full tie, the later launch first.
fn recency_order(rx: Recency, x: usize, ry: Recency, y: usize) -> std::cmp::Ordering {
    ry.cmp(&rx).then(y.cmp(&x))
}

/// **A SUBAGENT'S STATE IN HIS VOCABULARY** — `active`, `idle`, or an error —
/// for a row the daemon publishes no `rolled.display` for: an in-process Task
/// subagent has no thread of its own, so there is no daemon string to draw.
/// A RENAMING, not a rollup: it reads one row's own state and nothing under
/// it. `None` (no end seen) keeps the state's own word.
pub fn status_word(s: SubagentState) -> Option<&'static str> {
    match s {
        SubagentState::Running | SubagentState::Retrying => Some("active"),
        SubagentState::Done | SubagentState::Interrupted => Some("idle"),
        SubagentState::Failed => Some("error: failed"),
        SubagentState::Parked => Some("error: parked"),
        SubagentState::Unreported => Some("error: unreported"),
        SubagentState::Unknown => None,
    }
}

/// Depth-first, most recently used first under each parent (`kids` is
/// already sorted), marking the last of each sibling run so the rail can draw
/// `└` rather than `├`. Draws ONE row `i` (whose `last_sibling` is `last`) and
/// everything under it.
fn walk_tree(
    agents: &[SubagentRow],
    kids: &HashMap<Option<String>, Vec<usize>>,
    here: &[usize],
    depth: usize,
    last: bool,
    out: &mut Vec<TreeRow>,
) {
    for (n, &i) in here.iter().enumerate() {
        let a = &agents[i];
        let mine = kids.get(&Some(a.id.clone()));
        out.push(TreeRow {
            id: TreeId::Agent(a.id.clone()),
            depth,
            last_sibling: last && n + 1 == here.len(),
            label: a.label.clone(),
            state: a.state,
            started: a.started,
            ended: a.ended,
            tokens: a.tokens,
            calls: a.calls,
            bash_in_flight: a.bash_in_flight,
            agents_running: a.agents_running,
            class: a.class,
            status: status_word(a.state).map(str::to_string),
            expanded: mine.map(|_| true),
            ..TreeRow::default()
        });
        if let Some(mine) = mine {
            for (k, &c) in mine.iter().enumerate() {
                walk_tree(agents, kids, &[c], depth + 1, k + 1 == mine.len(), out);
            }
        }
    }
}

/// **WHAT THE CHAT AREA GAINS WHEN THE SUBAGENTS LEAVE IT** — `(before,
/// after)`, in transcript rows, for the open thread's retained tail.
///
/// > *I don't want to see them in the chat area, it takes up 90% of the chat
/// > area.*
///
/// That number is his and it deserved a measurement rather than agreement.
/// `before` is the SAME projection with the attribution map emptied — every
/// row a subagent produced back where it was, which is exactly the face he was
/// complaining about — and `after` is what he gets now. It exists as a
/// function rather than as a one-off script so the claim is re-runnable
/// against his live ledger and testable against a fixture; see
/// `examples/subagent_reclaim.rs`.
pub fn chat_area_reclaimed(
    store: &Store,
    stdout_only: bool,
    opened: &Opened,
    agents: &[SubagentRow],
) -> (usize, usize) {
    let rows = open_rows(store);
    let owner = attribution(&rows);
    let tiles = tiles_of(agents);
    let none: HashMap<u64, String> = HashMap::new();
    let count = |o: &HashMap<u64, String>| {
        project(
            rows.iter().copied(),
            &|_| String::new(),
            stdout_only,
            false,
            opened,
            Some(&Lens {
                owner: o,
                tiles: &tiles,
                subject: None,
            }),
            None,
        )
        .len()
    };
    (count(&none), count(&owner))
}

/// The OPEN thread's transcript.
pub fn thread_blocks(
    store: &Store,
    stdout_only: bool,
    expanded: bool,
    opened: &Opened,
    agents: &[SubagentRow],
    subject: &TreeId,
    frozen_at: Option<u64>,
) -> Vec<Block> {
    let Some(open) = store.open.as_ref() else {
        return Vec::new();
    };
    let mut out: Vec<Block> = Vec::new();
    match &open.fetch {
        Fetch::Loading => out.push(Block::Rule {
            text: "loading this thread's tail from the daemon…".into(),
        }),
        // Not empty. FAILED — and the daemon's own words, because a transcript
        // that cannot say why it is short reads as a short transcript.
        Fetch::Failed(why) => out.push(Block::Rule {
            text: format!("could not load this thread: {why}"),
        }),
        Fetch::Idle | Fetch::Loaded => {
            if open.forgotten > 0 {
                out.push(Block::Rule {
                    text: format!(
                        "{} older row(s) dropped from this render cache — the ledger still has them",
                        open.forgotten
                    ),
                });
            } else if let Some(from) = open.from_seq {
                if from > 1 {
                    out.push(Block::Rule {
                        text: format!(
                            "showing from seq {from} — everything before it is still in the ledger"
                        ),
                    });
                }
            }
        }
    }
    let rows = open_rows(store);
    let owner = attribution(&rows);
    let tiles = tiles_of(agents);
    let subject_id = match subject {
        // A managed child is never projected into its parent's transcript:
        // making it the subject OPENS it (`App::drill`).
        TreeId::Parent | TreeId::Thread(_) => None,
        TreeId::Agent(id) => Some(id.as_str()),
    };
    // **THE DIRECT INVOCATION IS THE SUBAGENT'S FIRST "USER" TURN.** It is the
    // verbatim `Agent.input.prompt` — the instruction the parent gave it — and
    // it is the one thing in a subagent's chat that did not arrive as one of
    // its own rows. Without it a drilled chat starts mid-sentence.
    if let Some(id) = subject_id {
        if let Some(a) = agents.iter().find(|a| a.id == id) {
            if let Some(text) = a.invocation.as_ref() {
                out.push(Block::Message {
                    id: BlockId {
                        thread_id: open.id.clone(),
                        // **NO ID ON THIS ONE, AND THAT IS THE HASH DIRECTIVE
                        // DOING ITS JOB.** Every other white box prints its
                        // MESSAGE id at the top right, bare, because he asked
                        // for it by name. This box is not a message on the
                        // ledger — it is a tool call's `input.prompt` — so the
                        // only id it could print is the `tool_use` HASH, and
                        // *"don't show me thread or task hash names"* stands.
                        // The label the box needs is `DIRECT INVOCATION`, and
                        // it has it.
                        message_id: String::new(),
                    },
                    sender: Who::User,
                    ts: a.started,
                    text: text.clone(),
                    // **HIS OWN LABEL, IN HIS OWN CASE.** *"If it's possible
                    // to detect that it's a message from a parent agent then
                    // let's mark that input block with something like 'DIRECT
                    // INVOCATION'"* — so it is shouted, because the whole point
                    // of the mark is that this white box is not his.
                    //
                    // **THE SEAM IS THE TOOL NAME AND NOT A HEURISTIC**:
                    // `agents.rs::launches` sets `invocation_is_direct` from
                    // `name == "Agent"`, with `SendMessage` the messaging path.
                    // There is nothing being inferred here.
                    note: Some(
                        if a.invocation_is_direct {
                            "DIRECT INVOCATION"
                        } else {
                            "message to this agent"
                        }
                        .to_string(),
                    ),
                    meta: Vec::new(),
                    key: None,
                    expandable: false,
                    open: true,
                });
            }
        }
    }
    out.extend(project(
        rows.iter().copied(),
        // ONE thread, so there is nothing to disambiguate and nothing to name.
        &|_| String::new(),
        stdout_only,
        expanded,
        opened,
        Some(&Lens {
            owner: &owner,
            tiles: &tiles,
            subject: subject_id,
        }),
        frozen_at,
    ));
    out
}

/// **HIS threads' event lane**: every thread he owns at once, in arrival order.
///
/// It is SCOPED BY THE SAME PREDICATE the list is, and that is not scope creep
/// — it is the same defect seen from the other side. The daemon pushes events
/// from all 1,849 threads it holds, 1,617 of them the arena's, and a lane that
/// showed them all would be exactly the bug `IS-20` names, one view over. Two
/// faces of one question deserve one answer.
///
/// The STDOUT-ONLY TOGGLE is a different mechanism and stays a different
/// mechanism: it holds back rows he can bring straight back and the header says
/// so. This is MEMBERSHIP, and a thread that is not his is not his with the
/// toggle off.
pub fn activity_blocks(
    store: &Store,
    stdout_only: bool,
    expanded: bool,
    opened: &Opened,
) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    if store.forgotten > 0 {
        out.push(Block::Rule {
            text: format!(
                "{} older row(s) dropped from this render cache — the ledger still has them",
                store.forgotten
            ),
        });
    }
    out.extend(project(
        store
            .tail
            .iter()
            .filter(|(t, _)| store.owns(t))
            .map(|(t, r)| (t.as_str(), r)),
        // **THE THREAD, NAMED THE WAY HE NAMES IT** — *"don't show me thread
        // or task hash names. not useful to me."* The fleet lane needs to say
        // which thread a row came from; it used to say `bcccfa79`, which is
        // eight characters he cannot read anything off. The index's own summary
        // is the line he recognises a thread by, and the index is already
        // loaded because it is what the thread list is built from.
        &|id| store.label_of(id),
        stdout_only,
        expanded,
        opened,
        // **THE FLEET LANE IS A LANE OVER EVERY THREAD'S EVENTS**, and a
        // subagent's rows are events. No lens, and no seq freeze: its seqs are
        // per-thread and nothing in it relocates.
        None,
        None,
    ));
    out
}

/// **ONE INDEX HEADER, DECORATED BY THE DAEMON** — the one derivation, used by
/// the project's own list and by both of the root's cross-project bands.
///
/// It is a function rather than the same block written three times for the
/// reason `drop_tail_to_floor` is one: the root menu copied a list once already
/// and the copy went straight into `0023`'s defect class 3, where a proof
/// mutated the wrong one of two identical bodies and reported the mutation as
/// unnoticed.
fn row_of(store: &Store, project: &str, t: &index::IndexThread) -> ThreadRow {
    let f = store.threads.as_ref().and_then(|m| m.get(&t.id));
    ThreadRow {
        id: t.id.clone(),
        // TUI-P1a: the thread's NAME (`Store::name_of`), so a rename shows on
        // every list row the moment it lands, and an unnamed thread is called
        // by its first message — never by its path.
        summary: clip(&store.name_of(&t.id).unwrap_or_default(), 400),
        // WHICH INDEX THIS CAME FROM. Constant inside a project and never
        // drawn there; the only address a root band row has.
        project: project.to_string(),
        cwd: f.map(|f| shorten(&f.cwd, &store.home)).unwrap_or_default(),
        model: f.map(|f| f.model.clone()).unwrap_or_default(),
        // THREE STATES, AND THEY ARE DIFFERENT THINGS — but only one of them
        // is a WORD, and it is the daemon's (consumer §8, TC-48; V1.9). `…` is
        // "the daemon has not told us yet" (loading); BLANK is "we asked and
        // it has no row for this id at all", which is what an unadopted
        // AUI-era thread looks like and is not a defect. Neither is drawn as
        // a status the daemon never said — never `absent`.
        state: match f {
            // Blank is only sayable when the status page was NOT truncated.
            // Otherwise the id may simply have fallen off the end of the page,
            // and "not told yet" is the true thing.
            None if store.absence_is_a_fact() => String::new(),
            None => "…".to_string(),
            // UI-b: the subtree's word, not only the thread's own.
            Some(_) => match store.shown_word(&t.id) {
                Some(w) if !w.is_empty() => w,
                _ => "?".to_string(),
            },
        },
        executing: f.is_some_and(|f| f.executing),
        queued: f.map(|f| f.queued).unwrap_or(Known::Unknown),
        unread: f.map(|f| f.unread).unwrap_or(Known::Unknown),
        // **LAST USER INPUT — when HE last put words into this thread.**
        //
        // His own sentence: *"the recents need to be ordered on last user
        // input, not on when they were created or any other events, including
        // renames."* This field is what the recents band SORTS on
        // (`project_list`) and what the age column RENDERS
        // (`tui_render::render`, which reads `ThreadRow::updated` and needed no
        // change) — one value feeding both, so the sort and the age cannot
        // disagree on screen. That property was already deliberate here and is
        // preserved by pointing both at the new field rather than by splitting
        // them.
        //
        // **This used to be the index's `last_turn_at`, and that was the
        // defect.** `last_turn_at` is stamped by six things that are not user
        // input: thread creation (so a thread with zero turns sorted to the
        // TOP), a manual summary set, an agent-emitted summary on three
        // separate paths, and both ends of a compaction. Measured on his live
        // index: 67 of 210 threads carry a `last_turn_at` equal to a summary
        // event's timestamp, and three of them — last spoken to in July and
        // August — had been re-summarised within the hour and were sitting at
        // the top of his recents band because of it.
        //
        // The daemon's `updated_at` is a THIRD clock on a third fact ("when did
        // this thread last do anything") and is still not mixed in here, for
        // the reason it never was.
        //
        // `None` means he has never spoken here; it sorts to the bottom of the
        // band and renders as an unknown age, never as a date this face
        // invented.
        //
        // **AND THE NEWER OF THE INDEX AND THE DAEMON.** Since the threads move
        // nothing writes `last_user_input_at` into `.mind/threads.json` on his
        // turns any more — every index froze, and the band read `idle > 8d` on
        // every row. The daemon's `threads/recent-updates` stamp is the same
        // fact (user messages only) and is live, so it wins when it is newer.
        updated: t.last_user_input_at.max(f.and_then(|f| f.last_user_input)),
        pinned: t.pinned,
        closed: t.closed,
        known: f.is_some(),
        activity: store.activity(&t.id).map(|a| (a.short, a.severity)),
        parent: f
            .and_then(|f| f.parent_id.as_deref())
            .map(|p| store.parent_label(p)),
    }
}

/// **EVERY HEADER IN EVERY ROOT, ONCE.**
///
/// `index::dedupe` folds duplicates WITHIN one index; this folds them ACROSS
/// roots, which is the case that actually bites. `.mind/threads.json` is tracked
/// in git, so a repo seeded from another carries its headers: 46 ids live in
/// three or four indexes and account for 101 of the fleet's 305 headers
/// (`0035` `ambiguous_cases`). A naive union would put "Broomstick platform
/// braindump" on the pinned band four times, which would read as a new bug
/// introduced by giving him bands.
///
/// The survivor is the header with the newest `last_turn_at`, and it keeps the
/// project it was read from — which is the project a pin write would be
/// addressed to. The AUI's own lookup takes the first match in arbitrary
/// `HashMap` order (`apps/aui/src/v2/store.rs:78`); that is a latent bug to
/// avoid, not a rule to copy.
/// **The comparison is HEADER-TO-HEADER, and it has to be.** It used to read
/// `prev.updated >= t.last_turn_at` — a projected row's sort key against a raw
/// header's `last_turn_at`. That happened to be the same number while
/// `ThreadRow::updated` WAS `last_turn_at`; it stopped being one the moment the
/// band moved to last-user-input, and left that way it would have compared two
/// different clocks and picked the survivor by neither. So the winner is chosen
/// among the HEADERS first, on `last_turn_at` explicitly, and only the winner
/// is projected.
///
/// Survivorship stays on `last_turn_at` deliberately and is NOT moved to
/// last-user-input: the question here is "which copy of this duplicated id was
/// written to most recently", not "when did he last speak in it". Keeping it
/// also keeps the AUI's `ledger_fold::resolve_project` — the same rule, on the
/// same field, with a documented STICKINESS argument that depends on a folded
/// summary bumping the chosen copy — in agreement with this face.
fn all_headers(store: &Store) -> Vec<ThreadRow> {
    let mut winners: Vec<(&str, &index::IndexThread)> = Vec::new();
    for p in &store.projects {
        for t in &p.threads {
            match winners.iter_mut().find(|(_, h)| h.id == t.id) {
                Some((_, h)) if h.last_turn_at >= t.last_turn_at => {}
                Some(slot) => *slot = (p.name.as_str(), t),
                None => winners.push((p.name.as_str(), t)),
            }
        }
    }
    winners
        .into_iter()
        .map(|(proj, t)| row_of(store, proj, t))
        .collect()
}

/// **IS AN INDEXED HEADER IN THE RECENT BAND?** Not closed, and — UI-a, the
/// operator's ruling that recent threads hide subagents — not a thread the
/// daemon says has a parent. The daemon's own `threads/recent-updates` already
/// leaves subagents out; this is the same shared rule applied to the headers
/// this face merges in from `.mind/threads.json`.
fn in_recents(store: &Store, r: &ThreadRow) -> bool {
    let f = store.fact(&r.id);
    !r.closed
        && !shapewire::subtree::hidden_from_recents(f.and_then(|f| f.parent_id.as_deref()))
        // TUI-P1a: an agent's tag hides an indexed thread too, the moment its
        // identity row is read — even before any `parent_id` fact arrives.
        && !f.and_then(|f| f.agent.as_deref()).is_some_and(|a| !a.is_empty())
        // …and a proof's scratch directory hides an indexed thread too.
        && !f.is_some_and(|f| in_scratch_dir(&f.cwd))
}

/// **THE DAEMON'S RECENT THREADS THAT NO INDEX HAS A HEADER FOR.**
///
/// Only threads with a known last-user-input stamp: the band orders on that and
/// nothing else, so a daemon row he never spoke in has no place here (the same
/// rule `threads/recent-updates` applies at its source). The project is the
/// registered root the thread's cwd lives under, or empty.
fn daemon_only_recent(store: &Store) -> Vec<ThreadRow> {
    let Some(m) = &store.threads else {
        return Vec::new();
    };
    let indexed: std::collections::HashSet<&str> = store
        .projects
        .iter()
        .flat_map(|p| p.threads.iter())
        .flat_map(|t| std::iter::once(t.id.as_str()).chain(std::iter::once(t.session_id.as_str())))
        .collect();
    m.iter()
        // UI-a: a subagent is never in the recents, whatever stamp it holds.
        .filter(|(id, f)| {
            f.last_user_input.is_some()
                && !indexed.contains(id.as_str())
                && !shapewire::subtree::hidden_from_recents(f.parent_id.as_deref())
                // **ONLY THREADS HIS FACES STARTED** (TUI-P1a, TC-05) — read
                // off the producer's `settings` cell. "Not told yet" is
                // `app_id: None`, which is not his, so an agent's thread never
                // flashes into the band while its identity is in flight. (A
                // separate "identity read" gate stood here and was removed: it
                // was forced out and nothing went red, because `None` already
                // says it — doctrine 5.)
                && is_his(f.app_id.as_deref(), f.agent.as_deref(), &f.cwd)
        })
        .map(|(id, f)| {
            let project = store
                .projects
                .iter()
                .filter(|p| !f.cwd.is_empty() && std::path::Path::new(&f.cwd).starts_with(&p.root))
                .max_by_key(|p| p.root.as_os_str().len())
                .map(|p| p.name.clone())
                .unwrap_or_default();
            ThreadRow {
                id: id.clone(),
                summary: clip(&store.name_of(id).unwrap_or_default(), 400),
                project,
                cwd: shorten(&f.cwd, &store.home),
                model: f.model.clone(),
                // UI-b: the subtree's word, not only the thread's own.
                state: store.shown_word(id).filter(|w| !w.is_empty()).unwrap_or_else(|| "?".to_string()),
                executing: f.executing,
                queued: f.queued,
                unread: f.unread,
                updated: f.last_user_input,
                pinned: false,
                closed: false,
                known: true,
                activity: store.activity(id).map(|a| (a.short, a.severity)),
                parent: f.parent_id.as_deref().map(|p| store.parent_label(p)),
            }
        })
        .collect()
}

/// **THE PINNED BAND'S ORDER — ONE DERIVATION.**
///
/// `pin_order` ascending, tie-broken by most-recent-activity first, mirroring
/// `build_pinned_items` (`apps/aui/src/dispatch/unread.rs:89-123`). The
/// tie-break is load-bearing rather than cosmetic: legacy headers all default
/// to `pin_order: 0`, so without it the band's order is arbitrary.
///
/// It is a FUNCTION and no longer a closure inside the root's projection
/// because a second reader arrived — `pinned_order`, which is what a `MovePin`
/// computes its `before` from. Two copies of this comparator would be two
/// answers to "what is the order of the pinned band", and the second one is the
/// one that would renumber his pins.
fn sort_pinned(store: &Store, pinned: &mut [ThreadRow]) {
    let header = |id: &str| {
        store
            .projects
            .iter()
            .flat_map(|p| p.threads.iter())
            .find(|t| t.id == id)
    };
    let order = |id: &str| header(id).map(|t| t.pin_order).unwrap_or(0);
    // **The tie-break is read off the HEADER and stays on `last_turn_at`.**
    // It used to be `b.updated.cmp(&a.updated)`, which was `last_turn_at` only
    // because `ThreadRow::updated` was. Now that `updated` means last user
    // input, taking it from the row would have silently moved the PINNED band's
    // order too — a change the operator did not ask for, and one that would put
    // this face out of step with `build_pinned_items`
    // (`apps/aui/src/dispatch/unread.rs`), which still ties on `last_turn_at`
    // and RENUMBERS his pins from the order it computes. Two faces disagreeing
    // about pin order is the one disagreement that can write itself down.
    let activity = |id: &str| header(id).and_then(|t| t.last_turn_at);
    pinned.sort_by(|a, b| {
        order(&a.id)
            .cmp(&order(&b.id))
            .then(activity(&b.id).cmp(&activity(&a.id)))
    });
}

/// **THE ROOT MENU.** One row per root the human faces own, under the
/// cross-project bands.
/// **ONE CAP, APPLIED THE SAME WAY TO EVERY BAND — EXCEPT THE ONE IN FOCUS.**
///
/// On the root every band is [`BAND_ROWS`]. On a band's own `see all` page that
/// band is UNCAPPED, and `all` means all:
///
/// > it takes us to a dedicated page with just that band and all results
///
/// `BAND_EXPANDED_MAX` is RETIRED with the expansion it bounded. It was a
/// DRAWING bound by its own doc comment, and the drawing is now bounded where
/// it belongs: `render::project_list` crops the surface to the viewport BEFORE
/// painting a row, so a five-thousand-row page costs a screenful of paints and
/// not five thousand. Nothing about LOADING changed — `Store::load_projects`
/// already holds every header of every project, so this was never a fetch.
///
/// Returns what the band would have held, taken BEFORE the truncate, because
/// after it the number is gone and the `see all` row would have to guess.
fn cap<T>(rows: &mut Vec<T>, uncapped: bool) -> usize {
    let total = rows.len();
    if !uncapped {
        rows.truncate(BAND_ROWS);
    }
    total
}

/// **THE ROOT MENU, AND — WITH `focus` SET — ONE BAND'S OWN `see all` PAGE.**
///
/// `focus` is `Some(b)` exactly when the shell is projecting `Mode::SeeAll(b)`.
/// The only two things it changes here are that band's CAP (gone) and the
/// `ProjectList::focus` flag that `model::root_lines` reads to emit one band
/// and no chrome. Everything else — the pinned order, recent's newest-first,
/// the totals, the clamp — is the root's, unchanged, because the page IS the
/// root's projection and not a second one.
pub fn project_list(
    store: &Store,
    cursor: usize,
    from_bottom: usize,
    focus: Option<tui_render::BandId>,
) -> ProjectList {
    let rows: Vec<ProjectRow> = store
        .projects
        .iter()
        .map(|p| ProjectRow {
            name: p.name.clone(),
            path: shorten(&p.root.to_string_lossy(), &store.home),
            threads: match p.index {
                crate::index::IndexState::Corrupt => Known::Unknown,
                _ => Known::Known(p.threads.len()),
            },
            pinned: p.pinned(),
            active: running_in(store, p),
            unread: unread_in(store, p),
            last: p.last(),
            unreadable_index: p.index == crate::index::IndexState::Corrupt,
        })
        .collect();

    let headers = all_headers(store);

    // **THE PINNED BAND — every pin, cross-project, in pin order.** Primary key
    // `pin_order` ascending; tie-broken by most-recent-activity FIRST, which is
    // `build_pinned_items` (`apps/aui/src/dispatch/unread.rs:118-123`) exactly
    // — legacy headers all default to `pin_order: 0`, so without the tie-break
    // the band's order would be arbitrary.
    //
    // The AUI RENUMBERS the headers when it finds duplicate orders. This face
    // does not and must not: that is a WRITE to `.mind/threads.json`, which is
    // the AUI's private surface, and the same write the clobber argument
    // forbids (`Q-11`). We read the order and live with the tie-break.
    let mut pinned: Vec<ThreadRow> = headers.iter().filter(|r| r.pinned).cloned().collect();
    sort_pinned(store, &mut pinned);

    // **THE RECENT BAND — the jump-back-in shortcut.** Newest FIRST (the doc's
    // own drawing, and `build_recent_items`' `b.cmp(&a)`), closed threads
    // excluded because they stay visible only inside their own project
    // (`libs/aui-nav/src/threads.rs:77-82`), capped at `BAND_ROWS`.
    //
    // A pinned thread is NOT excluded here. The AUI does not exclude it either,
    // and recency is a fact about the thread rather than about the band above
    // it: dropping the thread he touched two minutes ago out of "recent"
    // because it also happens to be pinned would make the band lie about what
    // it is.
    let mut rows = rows;
    let mut recent: Vec<ThreadRow> = headers.into_iter().filter(|r| in_recents(store, r)).collect();
    // **THE KEY IS LAST USER INPUT.** `ThreadRow::updated` is the index's
    // `last_user_input_at` (`row_of`, which carries the whole argument) — not
    // `last_turn_at`, not `created_at`, not the daemon's `updated_at`. The sort
    // line itself never changed and never was the defect; what it reads did.
    //
    // `Reverse` and not a flipped comparator, so the lint that reads this can
    // still see it as a key sort. **A row with no last user input sorts LAST**,
    // which is right twice over: `Reverse(None)` is the largest key, so a
    // thread he has never spoken a word in lands at the BOTTOM of the band
    // instead of the top where `last_turn_at: now` in `ThreadHeader::with_id`
    // used to put it. It stays IN the band rather than being filtered out of
    // it, because a thread he created is a thread he meant to use and the band
    // is the only cross-project place he can reach it from — absence would make
    // a new thread harder to find than an old one, which is the opposite of
    // what the band is for.
    // **PLUS EVERY THREAD THE DAEMON SAYS HE SPOKE IN THAT NO INDEX CARRIES.**
    // Threads started since the threads move have a daemon row and no header in
    // any `.mind/threads.json`, so an index-only band could never show the
    // thread he was working in five minutes ago.
    recent.extend(daemon_only_recent(store));
    recent.sort_by_key(|r| std::cmp::Reverse(r.updated));
    // **EVERY BAND IS CAPPED AT TEN ON THE ROOT**, which is the operator's
    // second ruling on `Q-14`: *"Let's make the default for pinned and projects
    // just 10 like the recents."* **AND THE BAND IN FOCUS IS NOT CAPPED AT
    // ALL** — *"all results"* — because its page is only that band and the
    // drawing bound now lives in the renderer's crop. See `cap`.
    //
    // The order of the three is the surface's own, so `BandId::slot` and this
    // array cannot disagree about which band a total belongs to.
    let un = |b: tui_render::BandId| focus == Some(b);
    let totals = [
        cap(&mut pinned, un(tui_render::BandId::Pinned)),
        cap(&mut recent, un(tui_render::BandId::Recent)),
        cap(&mut rows, un(tui_render::BandId::Projects)),
    ];

    let mut list = ProjectList {
        cursor: 0,
        from_bottom,
        loaded: store.projects_loaded,
        archived: store.archived.len(),
        pinned,
        recent,
        focus,
        totals,
        rows,
        // Both are the SHELL'S — the typed buffer must outlive a projection,
        // and the link state is a socket fact this module has no access to.
        // `project_list` builds the surface; `App::project` dresses it.
        new_project: None,
        aui_down: None,
    };
    // **CLAMPED AGAINST THE SURFACE'S OWN STOP COUNT**, not against a second
    // sum of the three bands: `+ new project` and `archived` are stops now, and
    // a clamp that did not know that would refuse to let the cursor reach the
    // last two rows of the root.
    list.cursor = cursor.min(list.selectable().saturating_sub(1));
    list
}

/// **ONE PROJECT'S THREADS — FROM ITS INDEX, decorated by the daemon.**
///
/// The inversion `0035` proved and `IS-20`'s whole fix: every row here exists
/// because a header exists in `<root>/.mind/threads.json`. The daemon is joined
/// on afterwards, BY ID, and its absence is a fact about the row rather than a
/// reason to drop it — one index thread today has no daemon row and it still
/// belongs on the list.
///
/// OLDEST FIRST so the newest sits at the bottom (`DIR-3`); [`crate::index`]
/// already deduped and sorted.
pub fn thread_list(store: &Store, cursor: usize, from_bottom: usize) -> ThreadList {
    let Some(p) = store.open_project() else {
        return ThreadList::default();
    };
    let rows: Vec<ThreadRow> = p
        .threads
        .iter()
        .map(|t| row_of(store, &p.name, t))
        .collect();
    let n = rows.len();
    ThreadList {
        cursor: cursor.min(n.saturating_sub(1)),
        from_bottom,
        // No longer a daemon page. `Some` means "this project has an index",
        // which is what the empty-state wording needs to tell apart.
        window: (p.index != crate::index::IndexState::Corrupt).then_some(n as u32),
        complete: true,
        rows,
    }
}

// ────────────────────────────────────────────────────────────────────────────
//  THE SHEET (§5) — facts on top, acts below
// ────────────────────────────────────────────────────────────────────────────

/// **WHO IS REACHABLE**, handed in by the shell because neither this module nor
/// the render crate owns a socket.
///
/// Two owners, two answers, and the sheet needs both: `0060`'s whole frame is
/// that each act is bound to the process that owns its state, so "can this row
/// act" has a different answer per row and the two answers arrive separately.
#[derive(Debug, Clone, Copy)]
pub struct Owners<'a> {
    /// `None` when the AUI's 9010 link is up; otherwise the SENTENCE its rows
    /// carry — `crate::aui::Reach::words`.
    pub aui_down: Option<&'a str>,
    /// `None` when the threads daemon is connected; otherwise the sentence.
    pub daemon_down: Option<&'a str>,
}

/// **`interrupt — waits for the connection`** (`DEC-13`'s posture: a
/// current-state act refuses loudly and nothing typed is lost).
pub const DAEMON_DOWN: &str = "waits for the connection";

/// **THE SHEET FOR ONE THREAD.**
///
/// `field` is the shell's — it holds the buffer so that what he typed survives
/// the sheet being re-projected, the link going down, and the field being
/// closed. This function never invents one.
///
/// **EVERY ROW IS BUILT IN EVERY STATE.** There is no branch here that omits a
/// row, and there must never be one: `0060`'s temporal rule is that
/// unavailability is a SENTENCE on the row, never a missing row and never a
/// bare grey row. The row set is therefore a constant shape and only the
/// `Avail` inside it moves.
pub fn sheet(
    store: &Store,
    project: &str,
    thread_id: &str,
    cursor: usize,
    field: Option<SheetField>,
    owners: Owners<'_>,
) -> Option<SheetView> {
    let (p, t) = store.projects.iter().find_map(|p| {
        (p.name == project)
            .then(|| p.threads.iter().find(|t| t.id == thread_id).map(|t| (p, t)))
            .flatten()
    })?;
    let f = store.threads.as_ref().and_then(|m| m.get(&t.id));
    let label = if t.summary.is_empty() {
        "(no summary)".to_string()
    } else {
        clip(&t.summary, 400)
    };

    // The model the SHEET states is the daemon's — it is what the last turn
    // actually ran on. The AUI's override is what the NEXT turn will run on,
    // and this face cannot read the AUI's settings file to know it. `0060`
    // rules that where they would disagree the honest thing is to show what we
    // have and name whose it is, so the row states the daemon's word and the
    // act is the AUI's.
    let model = match f.map(|f| f.model.as_str()) {
        Some(m) if !m.is_empty() => m.to_string(),
        _ => "?".to_string(),
    };

    // The pinned band's DISPLAYED order, which is the only input a move needs
    // and the reason this face never renumbers a pin itself.
    let order = pinned_order(store);
    let at = order.iter().position(|id| id == thread_id);

    let aui = |extra: Option<String>| match (owners.aui_down, extra) {
        // **THE UNREACHABLE OWNER WINS.** When the AUI is down every AUI-owned
        // row is blocked for one reason, and that reason is the specific one
        // the socket gave ("needs the audio interface, which is not running",
        // or "the audio interface's door is not open" — `DownCause::words`).
        // A structural reason underneath it — "pin it first" — is still true
        // and still shown the moment the link returns.
        (Some(why), _) => Avail::Unavailable(why.to_string()),
        (None, Some(why)) => Avail::Unavailable(why),
        (None, None) => Avail::Ready,
    };
    let daemon = |extra: Option<String>| match (owners.daemon_down, extra) {
        (Some(why), _) => Avail::Unavailable(why.to_string()),
        (None, Some(why)) => Avail::Unavailable(why),
        (None, None) => Avail::Ready,
    };

    let unpinned = || Some("pin it to the root first".to_string());
    let rows = vec![
        SheetRow {
            act: SheetAct::Model,
            stem: format!("model {model}"),
            hint: Some("click to change".into()),
            avail: aui(None),
        },
        SheetRow {
            act: SheetAct::Interrupt,
            stem: "interrupt the running turn".into(),
            hint: None,
            avail: daemon(None),
        },
        SheetRow {
            act: SheetAct::Compact,
            stem: "compact the thread".into(),
            hint: Some("instructions optional, typed below".into()),
            avail: daemon((t.turn_count == 0).then(|| "nothing to compact yet".to_string())),
        },
        SheetRow {
            act: SheetAct::Rename,
            stem: "rename".into(),
            hint: Some("edit the name in place".into()),
            // TUI-P1a: a rename is the DAEMON'S now (TC-03/TC-28), so it is
            // blocked by the daemon being down and never by the AUI.
            avail: daemon(None),
        },
        SheetRow {
            act: SheetAct::Pin,
            stem: if t.pinned {
                "unpin from the root".into()
            } else {
                "pin to the root".to_string()
            },
            hint: None,
            avail: aui(None),
        },
        SheetRow {
            act: SheetAct::MoveUp,
            stem: "move up".into(),
            hint: None,
            avail: aui(match at {
                None => unpinned(),
                Some(0) => Some("it is already at the top of the pinned band".into()),
                Some(_) => None,
            }),
        },
        SheetRow {
            act: SheetAct::MoveDown,
            stem: "move down".into(),
            hint: None,
            avail: aui(match at {
                None => unpinned(),
                Some(i) if i + 1 >= order.len() => {
                    Some("it is already at the bottom of the pinned band".into())
                }
                Some(_) => None,
            }),
        },
        SheetRow {
            act: SheetAct::Closed,
            stem: if t.closed {
                "reopen".into()
            } else {
                "mark closed".to_string()
            },
            hint: None,
            avail: aui(None),
        },
        // **THE ID'S ONE DELIBERATE HOME.** Owned by nobody outside this
        // process — it is a clipboard write and cannot be refused by an owner
        // that is down — so it is `Avail::Ready` unconditionally, and it is the
        // only row on this sheet that is.
        SheetRow {
            act: SheetAct::CopyId,
            stem: "copy the thread id".into(),
            hint: None,
            avail: Avail::Ready,
        },
    ];

    Some(SheetView {
        title: label,
        facts: Facts::Thread(SheetFacts {
            project: p.name.clone(),
            started: t.created_at,
            turns: t.turn_count,
            last_turn: t.last_turn_at,
        }),
        cursor: cursor.min(rows.len() - 1),
        rows,
        field,
        // A thread sheet arms nothing: `remove` is the one row that asks first
        // and it is on the PROJECT sheet.
        armed: None,
    })
}

/// **WHICH ROW OF AN OPEN SHEET CARRIES THIS ACT.**
///
/// The one translation from "the act he named with a key" to "the row index
/// the sheet's own door takes", and it exists so that a key which means a VERB
/// (`r` = rename) reaches that verb through exactly the path a click reaches
/// it by, instead of growing a second implementation of the act beside the
/// sheet's.
///
/// A row index is not a stable name for an act — the thread sheet's rows are
/// built per thread and two of them change their words with the thread's state
/// — so a caller that hard-coded `3` would silently rename via whatever row
/// landed there the day the sheet grew a row. Asking the projection is the
/// only reading that cannot go stale.
///
/// `None` when the sheet is not showing that act at all, which is the PROJECT
/// sheet's ordinary state: it has no rename row, and a key aimed at one there
/// must do nothing rather than fire the row that happens to be in its place.
pub fn sheet_row_for(view: &SheetView, act: SheetAct) -> Option<usize> {
    view.rows.iter().position(|r| r.act == act)
}

// ────────────────────────────────────────────────────────────────────────────
//  THE PROJECT SHEET (§5's second frame) and THE ARCHIVED ROOM (§1)
// ────────────────────────────────────────────────────────────────────────────

/// **THE SHEET FOR ONE PROJECT.** `0060`: it *"SURVIVES WHOLE"*.
///
/// Three act rows — model, archive, remove — and **two for `miscellaneous`**.
///
/// # The distinction this function exists to get right
///
/// Everywhere else on this face an act that cannot be performed KEEPS ITS ROW
/// and wears its reason in words. `miscellaneous` is the one deliberate
/// exception, and it is an exception because it is a different KIND of thing
/// rather than the same menu at a different time: it is the un-projected
/// bucket, it is not under `~/minds`, it can never be archived and there is no
/// symlink to remove. A GREYED `archive` here would promise a temporary outage
/// that will never lift — it would say "not now" about a thing that is "not
/// ever" — so the rows are ABSENT, and the facts line says why in words.
///
/// Getting this wrong in EITHER direction is the interesting failure: drawing
/// them greyed lies about time, and dropping them from an ordinary project's
/// sheet when the AUI is down lies about the menu.
pub fn project_sheet(
    store: &Store,
    project: &str,
    cursor: usize,
    chosen: Option<Option<aui_wire::Model>>,
    armed: Option<SheetAct>,
    owners: Owners<'_>,
) -> Option<SheetView> {
    let p = store.projects.iter().find(|p| p.name == project)?;
    let bucket = p.name == crate::acts::BUCKET;

    let aui = |extra: Option<String>| match (owners.aui_down, extra) {
        (Some(why), _) => Avail::Unavailable(why.to_string()),
        (None, Some(why)) => Avail::Unavailable(why),
        (None, None) => Avail::Ready,
    };
    // **THE ARMED ROW SAYS IT IS ARMED**, so the second press is something he
    // reads before he makes it. This is §5's `asks first`, and it is drawn
    // rather than modal: the grammar does not change while a row is armed, so
    // the `?` overlay stays true.
    let arm = |act: SheetAct, hint: &'static str| -> Option<String> {
        (armed == Some(act))
            .then(|| "press again to remove it · anything else cancels".to_string())
            .or_else(|| Some(hint.to_string()))
    };

    // **THE MODEL ROW STATES WHAT IT KNOWS, WHICH IS WHAT THIS FACE SET.**
    //
    // §5 draws `model default — click to change`, and that word is a CLAIM this
    // face cannot back: the project's override lives in `~/.aui/settings.json`
    // behind `effective_model`'s three-level fallback, and this face never
    // opens that file (`0060`'s "never write a file" has a read twin — there is
    // no honest read of it either). Unlike a thread, a project has no daemon
    // value to fall back on: the daemon has no project concept at all.
    //
    // So before he touches it the row says `model ?` — honesty rule 1, the same
    // `?` every other unknown on this face wears — and after he cycles it, it
    // says what he just set, including the doc's own word `default` for the
    // press that CLEARS the override. The word appears exactly when it is true.
    let model = match chosen {
        None => "?".to_string(),
        Some(None) => "default".to_string(),
        Some(Some(m)) => m.label().to_ascii_lowercase(),
    };

    let mut rows = vec![SheetRow {
        act: SheetAct::ProjectModel,
        stem: format!("model {model}"),
        hint: Some("click to change".into()),
        avail: aui(None),
    }];
    if !bucket {
        rows.push(SheetRow {
            act: SheetAct::Archive,
            stem: format!("archive {}", p.name),
            hint: Some("leaves the root, threads kept, one click brings it back".into()),
            avail: aui(None),
        });
        rows.push(SheetRow {
            act: SheetAct::Remove,
            stem: format!("remove {}", p.name),
            // The armed words REPLACE the hint, which is `SheetRow::words`'
            // existing substitution rule and not a second one.
            hint: arm(SheetAct::Remove, "asks first"),
            avail: aui(None),
        });
    }

    let oldest = p.threads.iter().filter_map(|t| t.created_at).min();
    Some(SheetView {
        title: p.name.clone(),
        facts: Facts::Project(ProjectFacts {
            name: p.name.clone(),
            threads: p.threads.len(),
            running: running_in(store, p),
            oldest,
            bucket,
        }),
        cursor: cursor.min(rows.len().saturating_sub(1)),
        rows,
        field: None,
        armed,
    })
}

/// How many of a project's threads the daemon says are executing.
///
/// `Unknown` until the daemon has answered once — never `0`, which is honesty
/// rule 1's exact violation. Derived the same way the root's project row
/// derives its own, because two answers to "how many are running here" is one
/// answer too many.
fn running_in(store: &Store, p: &Project) -> Known<u32> {
    let Some(f) = store.threads.as_ref() else {
        return Known::Unknown;
    };
    Known::Known(
        p.threads
            .iter()
            .filter(|t| f.get(&t.id).is_some_and(|d| d.executing))
            .count() as u32,
    )
}

/// **HOW MANY UNREAD THIS PROJECT'S THREADS CARRY**, summed off the daemon's
/// own per-thread counts.
///
/// `Unknown` until the daemon has answered at all — never a zero we invented,
/// which is the same rule [`running_in`] is written under and the same rule the
/// row's rendering enforces one layer up.
fn unread_in(store: &Store, p: &Project) -> Known<u32> {
    let Some(f) = store.threads.as_ref() else {
        return Known::Unknown;
    };
    Known::Known(
        p.threads
            .iter()
            .filter_map(|t| f.get(&t.id))
            .map(|d| match d.unread {
                Known::Known(n) => n,
                Known::Unknown => 0,
            })
            .sum(),
    )
}

/// **THE ARCHIVED ROOM.** One row per archived project, newest first.
///
/// Read off the FILESYSTEM — `~/.aui/archived-projects.json` — and never off
/// 9010, so the door opens and the room draws whether or not the audio
/// interface is running (`0060`'s second binding condition: nothing rendered
/// is sourced from the AUI's socket). What needs the AUI is the unarchive, and
/// that is a sentence on the row.
pub fn archived_view(store: &Store, cursor: usize, owners: Owners<'_>) -> ArchivedView {
    let mut rows: Vec<ArchivedRow> = store
        .archived
        .iter()
        .map(|a| ArchivedRow {
            name: a.name.clone(),
            since: a.at,
            avail: match owners.aui_down {
                Some(why) => Avail::Unavailable(why.to_string()),
                None => Avail::Ready,
            },
        })
        .collect();
    // Newest first: the one he archived last is the one he is most likely to
    // have changed his mind about. A row with no date sorts last rather than
    // first, because `None` is "we do not know", not "a long time ago".
    rows.sort_by_key(|r| std::cmp::Reverse(r.since));
    ArchivedView {
        cursor: cursor.min(rows.len().saturating_sub(1)),
        rows,
    }
}

/// **THE PINNED BAND'S DISPLAYED ORDER**, top to bottom, as thread ids.
///
/// It is derived from the SAME sort `project_list` draws the band with, and it
/// is derived here rather than read off the drawn frame so that a sheet raised
/// from a project's thread list — where the pinned band is not on screen at all
/// — computes the same `before` as one raised from the root.
///
/// This is `0053`'s recorded divergence still standing: the AUI RENUMBERS
/// duplicate `pin_order` values by writing the index; this face reads the order
/// and lives with the tie-break, because that write is the AUI's private
/// surface. What a move sends is a NEIGHBOUR's id, and the server renumbers.
pub fn pinned_order(store: &Store) -> Vec<String> {
    let mut pinned: Vec<ThreadRow> = all_headers(store)
        .into_iter()
        .filter(|r| r.pinned)
        .collect();
    sort_pinned(store, &mut pinned);
    pinned.into_iter().map(|r| r.id).collect()
}

/// The header's daemon-owned scalars, or [`Known::Unknown`] when the daemon has
/// not told us. There is no third option and no way to spell one as a zero.
///
/// **THERE IS NO FLEET COUNT HERE, and its absence is the paging decision.**
/// `0031` rendered `1000+` because both fleet calls are `LIMIT`-ed and his
/// daemon has more threads than the page. That was honest about the number and
/// wrong about the question: a page ordered `updated_at DESC` cannot say how
/// many threads exist, and now that there is a real thread list it does not
/// need to. So the header names the WINDOW — *the 1000 most recent* — which is
/// what the call actually answers.
pub fn header(store: &Store, mode: tui_render::Mode) -> tui_render::Header {
    let open = store.open_id().map(str::to_string);
    let (title, cwd, account, queued, scope) = match (&open, mode) {
        // **THE OPEN THREAD IS NAMED THE WAY HE NAMES IT.** This line read
        // `5f4413b1 · idle` until `0076` — `DIR-16`'s third live surface, found
        // by `0073` on frame A2. `label_of` is the one producer of "how a
        // thread is named on a row, and it is never the hash"; it has existed
        // since `0051` and this header never called it.
        //
        // **A THREAD WITH NO NAME SHOWS ITS STATE AND NOTHING ELSE**, rather
        // than falling back to the hash the directive just removed. `label_of`
        // returns the empty string when neither an index summary nor a cwd can
        // name the thread, and `· idle` with nothing in front of it is an
        // honest "we cannot name this yet" — the same shape every other
        // unknown on this face takes.
        // **MIC MODE IS A THREAD SURFACE AND ITS HEADER SAYS SO.** It fell
        // through to the `_` arm until `0076` and drew the ACTIVITY header —
        // `activity · his 305 threads` over a mic band belonging to one open
        // thread. Found by the `DIR-16` sweep machine, which asks every mode
        // to name the thread it is about before it will believe an absence;
        // mic mode could not, and the reason was this arm.
        (Some(id), tui_render::Mode::Thread(_) | tui_render::Mode::Mic) => (
            // **NO STATE WORD WHEN THE ACTIVITY PHRASE SPEAKS.** A parent whose
            // turn ended while its subagents run is `idle` to the daemon; the
            // header's activity part says `waiting on N subagents`, and an
            // `idle` beside it would be the lie the phrase exists to replace.
            //
            // **TUI-P1a (TC-04): THE TITLE IS THE NAME AND THE STATUS, ALWAYS.**
            // The activity phrase no longer replaces the status word — it is
            // not drawn on the thread page at all (`render::header_line`), and
            // the status is the subtree's (UI-b), so a parent whose children
            // run already reads `active` without it. `label_of` never answers
            // empty and never answers a path.
            format!("{} · {}", store.label_of(id), header_status(store, id)),
            store.cwd_of(id),
            store.account_of(id),
            store.queued(Some(id)),
            "this thread".to_string(),
        ),
        // **THE ROOT HEADER, CUT TO WHAT HE ASKED TO SEE.** His words, quoting
        // his own screen back:
        //
        // > projects · 13 with an index · 305 threads · ▲1 across his threads ·
        // > 1656 unread elsewhere · connected · …
        // > that's what I'm seeing in the header. I don't need to see projects,
        // > 13 with an index, 305 threads or 1656 unread. All I want to see is
        // > the # unread (as counted from the last 100 recent threads, but not
        // > explicitly stated as such), and connected. I don't know what ▲1
        // > means so let's drop it. users won't get it. Also count active
        // > threads.
        //
        // So: the unread count, an ACTIVE count, and `connected` (which is the
        // link part, added by `render::header_line` and not by this title).
        // Gone: the project count, `with an index`, the thread total, and
        // `unread elsewhere`'s framing.
        //
        // **THE PROJECT COUNT DID NOT VANISH, IT MOVED** — onto the `projects`
        // band header, which is where he put it in the same breath: "You can
        // say the number of projects available in the band header."
        //
        // **THE UNREAD COUNT'S BASIS IS DELIBERATELY UNSTATED.** It is summed
        // over the daemon's fleet page, which is the last hundred threads by
        // recency, and he ruled explicitly that the UI must not say so.
        // `unread_elsewhere(None)` is that sum with nothing excluded, which at
        // the root is every thread he owns that the page reached.
        //
        // **AND THE QUEUE COUNT STAYED — ONLY THE GLYPH WENT.** This is the one
        // ruling in this block that is NOT a literal reading of his words, and
        // it is deliberate. He said *"I don't know what ▲1 means so let's drop
        // it. users won't get it."* — which is a man failing to RECOGNISE a
        // rendering, not a man retracting `DIR-6`, his own standing directive
        // that the queue count be visible AT ALL TIMES and one that round two
        // restored as a defect fix. "Users won't get it" is a LEGIBILITY
        // complaint, and correctness at the source says the defect is the
        // glyph, not the information. So `▲1` became `1 queued` — and it kept
        // its own never-dropped PART in `render::header_line` rather than being
        // folded into this title, because `DIR-6`'s guarantee is expressed as a
        // RANK the fit loop may not drop, and a fact inside the title is a fact
        // the title's crop can eat.
        (_, tui_render::Mode::Projects) => (
            {
                if store.projects_loaded {
                    let unread = store.unread_elsewhere(None);
                    let active = store.active_threads();
                    format!("{} unread · {} active", unread.label(), active.label())
                } else {
                    "reading ~/minds".to_string()
                }
            },
            String::new(),
            Known::Unknown,
            store.queued_owned(),
            // **THE SCOPE IS STILL COMPUTED AND IS NOT DRAWN AT THE ROOT.**
            // Dropping it HERE would have been a second place that decides
            // what the root header shows; `render::header_line` already gates
            // it, explicitly, alongside the other three facts his cut removed.
            // A fact the model stops carrying is a fact no other view can get
            // back.
            "across his threads".to_string(),
        ),
        // **A BAND'S OWN PAGE IS TITLED BY ITS BAND AND ITS DEPTH** —
        // `recent · 182 threads`, `pinned · 26 threads`, `projects · 33`.
        //
        // The NUMBER is `ProjectList::totals`' number, read off the same
        // `all_headers` walk the root's band note reads (`band_note`), so the
        // header of the page and the note on the band header he pressed `→` on
        // cannot disagree about how deep the band is. It is the count of what
        // EXISTS and not of what is drawn, which is what he asked the band
        // header for in the first place: *"You can say the number of projects
        // available in the band header."*
        //
        // **IT IS COMPUTED HERE AND NOT COPIED FROM THE PROJECTION**, because
        // this function runs BEFORE `project_list` in `App::project` and a
        // title read off last frame's projection would be one keystroke stale.
        (_, tui_render::Mode::SeeAll(b)) => (
            {
                let n = band_total(store, b);
                match b {
                    tui_render::BandId::Projects => format!("projects · {n}"),
                    _ => format!(
                        "{} · {} thread{}",
                        b.label(),
                        n,
                        if n == 1 { "" } else { "s" }
                    ),
                }
            },
            String::new(),
            Known::Unknown,
            store.queued_owned(),
            "across his threads".to_string(),
        ),
        // **AND THE WORKING DIRECTORY LEFT THIS HEADER.** `0040` §2 names hex
        // ids and working directories among what is *absent, deliberately* on
        // the project screen, and its header is the single word `harness`. The
        // cwd was a FLEET-era fact: across projects a path is what tells two
        // rows apart, and inside one project it is constant — it said the same
        // thing on every frame he could reach it from, which is the definition
        // of a fact that is not worth a column.
        //
        // The project's NAME says the same thing in the word he uses for it,
        // and it is already the first thing on the line.
        (_, tui_render::Mode::List) => (
            match store.open_project() {
                Some(p) => format!(
                    "{} · {} thread{} · {} active",
                    p.name,
                    p.threads.len(),
                    if p.threads.len() == 1 { "" } else { "s" },
                    store.active_in(p).label()
                ),
                None => "threads · no project open".to_string(),
            },
            String::new(),
            Known::Unknown,
            store.queued_owned(),
            "this project".to_string(),
        ),
        _ => (
            match store.thread_count() {
                Known::Unknown => "activity · waiting for the daemon".to_string(),
                Known::Known(_) => format!(
                    "activity · his {} threads · {} active",
                    store.owned_count(),
                    store.active_threads().label()
                ),
            },
            String::new(),
            Known::Unknown,
            store.queued_owned(),
            "across his threads".to_string(),
        ),
    };
    // **WHAT THE OPEN THREAD IS DOING** — `waiting on 2 subagents`, never
    // `idle`, while its children run; and on a child, whose subagent it is.
    let activity = match mode {
        tui_render::Mode::Thread(_) | tui_render::Mode::Mic => store.open_id().and_then(|id| {
            store.activity(id).map(|a| (a.long, a.severity)).or_else(|| {
                store
                    .fact(id)
                    .and_then(|f| f.parent_id.as_deref())
                    .map(|p| (format!("↑ subagent of {}", store.parent_label(p)), Severity::Unknown))
            })
        }),
        _ => None,
    };
    tui_render::Header {
        thread_page: matches!(mode, tui_render::Mode::Thread(_) | tui_render::Mode::Mic),
        activity,
        // **THE TRAIL IS THE SHELL'S, NOT THE STORE'S.** It is a function of
        // `App::origin` — where he came FROM — and this module knows only what
        // is open. `App::project` fills it in immediately after this returns;
        // see `App::trail`.
        trail: Vec::new(),
        title,
        cwd,
        link: tui_render::Link {
            url: store.endpoint.clone(),
            state: store.link.clone(),
            down_since: store.down_since,
        },
        account,
        queued,
        queue_scope: scope,
        unread_elsewhere: store.unread_elsewhere(open.as_deref()),
        // Passed STRAIGHT THROUGH, not summed here: the renderer decides what a
        // one-row header can afford to say about two numbers, and this module
        // deciding it first would be the collapse happening one layer early.
        warnings: store.daemon_warnings,
        warning_errors: store.daemon_warning_errors,
    }
}

/// **HOW DEEP A BAND IS — the one number its page's header and the root's band
/// note both show.**
///
/// Derived from the same two walks `project_list` derives `totals` from, and
/// deliberately NOT from a `ProjectList` handed in: `store::header` runs before
/// `store::project_list` on every projection, so a number taken off the live
/// projection would be last frame's.
fn band_total(store: &Store, b: tui_render::BandId) -> usize {
    match b {
        tui_render::BandId::Projects => store.projects.len(),
        tui_render::BandId::Pinned => all_headers(store).iter().filter(|r| r.pinned).count(),
        // The band lists indexed headers AND the daemon-only threads this face
        // started (`project_list`), so its depth counts both — it used to count
        // the headers alone and undercount the band (HANDOFF §2.2, TC-06).
        tui_render::BandId::Recent => {
            all_headers(store).iter().filter(|r| in_recents(store, r)).count()
                + daemon_only_recent(store).len()
        }
    }
}

fn state_word(store: &Store, id: &str) -> String {
    // UI-b: the header says what the subtree says.
    store.shown_word(id).filter(|w| !w.is_empty()).unwrap_or_else(|| "?".into())
}

/// **A THREAD'S STATUS IN HIS WORDS** — `active`, `idle`, or an error, and
/// nothing else (operator ruling, r42: *"a thread's status is active, idle, or
/// an error"*). Mapped from [`Store::shown_word`], the subtree's word (UI-b),
/// until R9.TS1's `status_display` reaches this face:
///
/// | daemon word (rolled up) | his word |
/// |---|---|
/// | `active`, `retrying` | `active` |
/// | `parked` | `error: parked` — the one error state the producer derives (R9.TS1's `ERROR_PARKED`) |
/// | `idle`, `queued`, `done`, anything else | `idle` |
/// | not told yet | `?` — never a guess (honesty rule 1) |
pub fn header_status(store: &Store, id: &str) -> String {
    status_in_his_words(&state_word(store, id)).to_string()
}

/// The mapping [`header_status`] applies, on its own so it can be tested whole.
pub fn status_in_his_words(word: &str) -> &'static str {
    match word {
        "?" | "" => "?",
        "active" | "retrying" => "active",
        "parked" => "error: parked",
        _ => "idle",
    }
}

/// The standing row above the content.
///
/// **The disconnected posture** (`DEC-13`): the transcript STAYS and the banner
/// says the link is down. The rule is "never render stale as current", not
/// "render nothing" — a chat is mostly history and history remains true, which
/// is the deliberate deviation from `$ threads`' full-screen down page.
pub fn banner(store: &Store, mode: tui_render::Mode) -> Option<Banner> {
    // **AN UNREADABLE REGISTRY OUTRANKS EVERYTHING, including the link.**
    //
    // The list of projects is the WHOLE UNIVERSE of this face
    // (`index::load_projects`), and when `~/.cauldron/projects.json` is
    // present and will not parse that universe is the bucket alone. An empty
    // root with no sentence on it reads as "you have no projects" — a lie he
    // would act on, and the one failure this face can produce that looks
    // exactly like success. A daemon that is not answering is louder but it is
    // not worse: history is still on screen and still true.
    //
    // `ui-server` is the only writer of that file; this face states the
    // crate's own sentence and repairs nothing.
    if let index::RegistryState::Unreadable(why) = &store.registry {
        return Some(Banner {
            text: format!(
                "the project registry will not parse ({why}) — this list is the bucket alone \
                 until ~/.cauldron/projects.json is repaired; this face never writes it"
            ),
            severity: Severity::Critical,
        });
    }
    match &store.link {
        // **THE DOWN-DURATION AND THE NEXT RETRY ARE PROMISED HERE, not only
        // in the header.** The header ellipsizes from the right and the link
        // line is deliberately late on it (`render::header_line` says why), so
        // at 80 columns the promise would be off the end. The banner has the
        // whole row.
        //
        // **Neither NUMBER is in this string, and that is the fix.** This
        // function runs once per PROJECTION — on an event — so anything it
        // formats from the clock is frozen at that event. `render::banner_rows`
        // prefixes both readings from `Link`, against the frame's own `now`.
        LinkState::Disconnected { reason, .. } => Some(Banner {
            text: format!(
                "the daemon is not answering ({reason}) — everything below is HISTORY and still \
                 true; every current-state meter above reads ?"
            ),
            severity: Severity::Critical,
        }),
        LinkState::Connecting { attempt } if *attempt > 1 => Some(Banner {
            text: format!(
                "reconnecting (attempt {attempt}) — what is below is history and still true"
            ),
            severity: Severity::Warn,
        }),
        // **THE FIRST-RUN BAND** (`0040` §10): "the home surface with one
        // extra band — a single hint line that teaches the whole grammar and
        // never appears again". The doc draws it in the banner's own slot,
        // between the header rule and the body, which is why it is a `Banner`
        // and not a row inside the root: it IS that band.
        //
        // **The link outranks it**, by being matched first — a face that
        // cannot reach the daemon has something more urgent to say than how
        // its keys work, and there is one slot.
        //
        // **Root only.** The line teaches `→`, `←` and the sheet, which is the
        // ROOT's grammar; carried into a thread it would be teaching keys that
        // are not the ones in front of him.
        _ if store.first_run && mode == tui_render::Mode::Projects => Some(Banner {
            text: FIRST_RUN_HINT.to_string(),
            severity: Severity::Unknown,
        }),
        _ => None,
    }
}

/// **The one line the face ever teaches itself with.**
///
/// Verbatim from `0040` §10's frame. Every key it names is a key the root
/// actually has, which is the bar a teaching line has to clear: `?` opens the
/// help that lists the rest.
pub const FIRST_RUN_HINT: &str =
    "first run · → or click opens · ← goes back · click a title for its sheet · ? all ten keys";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tui_render::Mode;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-14T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// **THE TILE'S DECLARED GESTURE IS REALLY THE SUFFIX OF ITS TEXT.**
    ///
    /// The renderer reserves [`TILE_AFFORDANCE`]'s columns and then paints the
    /// text with that suffix STRIPPED — so if the two ever drift apart the row
    /// says `opens its chat` twice, or says it once with the label's last
    /// characters missing, and nothing else in the stack would notice. This is
    /// the seam, asserted at the seam, with the states and the token spellings
    /// the real tile takes.
    #[test]
    fn every_tile_ends_in_the_gesture_it_declares_to_the_renderer() {
        use tui_render::{SubagentClass, SubagentState};
        let row = |state: SubagentState, tokens: Option<u64>| SubagentRow {
            id: "tu_1".into(),
            class: SubagentClass::Direct,
            parent: None,
            label: "Explore · scout the raw list".into(),
            state,
            launched_at: ("t1".to_string(), 7),
            started: Some(now()),
            ended: None,
            tokens,
            calls: None,
            bash_in_flight: None,
            agents_running: None,
            invocation: None,
            invocation_is_direct: true,
        };
        for state in [
            SubagentState::Running,
            SubagentState::Done,
            SubagentState::Unknown,
        ] {
            for tokens in [None, Some(0), Some(31_400), Some(210_000)] {
                let tiles = tiles_of(&[row(state, tokens)]);
                let text = tiles.get(&7).expect("one tile at the launch seq");
                assert!(
                    text.ends_with(TILE_AFFORDANCE),
                    "a tile that does not end in its own gesture: {text:?}"
                );
                assert!(
                    text.starts_with("agent · Explore"),
                    "a tile that does not name its subagent: {text:?}"
                );
                assert_eq!(
                    text.matches("opens its chat").count(),
                    1,
                    "the gesture is spelled twice: {text:?}"
                );
            }
        }
    }

    /// **AN UNREADABLE REGISTRY SAYS SO, LOUDLY, AND OUTRANKS THE LINK.**
    ///
    /// `~/.cauldron/projects.json` is the whole universe of this face now. A
    /// file that will not parse leaves the root holding the bucket alone —
    /// which looks EXACTLY like "he has no projects", and that is the one
    /// failure mode of this cutover that would read as success. So it gets the
    /// banner, and it gets it even while the daemon is also down: a dead
    /// daemon still leaves history on screen and still true, and a registry
    /// that will not parse has taken his list away.
    ///
    /// FORCED both ways: delete the early return and the first assertion goes
    /// red with the link's sentence instead; move the early return BELOW the
    /// `match` and the ordering assertion goes red while the plain case still
    /// passes — two different halves, each with its own mutation.
    #[test]
    fn an_unreadable_registry_banner_outranks_even_a_dead_daemon() {
        let mut st = store();
        // WITNESS: with a good registry and a dead link, the banner is the
        // LINK's. Without this an early return that fired unconditionally
        // would score a pass below.
        st.apply(
            TransportEvent::Disconnected {
                reason: "connection refused".into(),
                next_retry_in_ms: 2000,
            },
            Utc::now(),
        );
        let b = banner(&st, tui_render::Mode::Projects).expect("a dead link says so");
        assert!(b.text.contains("the daemon is not answering"));

        st.registry = index::RegistryState::Unreadable("expected value at line 1".into());
        let b = banner(&st, tui_render::Mode::Projects)
            .expect("an unreadable registry said nothing at all");
        assert!(
            b.text.contains("the project registry will not parse"),
            "the registry banner lost to the link: {}",
            b.text
        );
        assert!(
            b.text.contains("expected value at line 1"),
            "the crate's own sentence never reached him: {}",
            b.text
        );
        assert_eq!(b.severity, Severity::Critical);
    }

    fn store() -> Store {
        Store::new("ws://127.0.0.1:3651/ws".into(), "/Users/x".into())
    }

    /// One spelling of a fixture timestamp, so a fixture cannot set a thread's
    /// two clocks from two different parses and then be believed about which
    /// one a band read.
    fn fixture_stamp(rfc3339: &str) -> Option<DateTime<Utc>> {
        Some(
            DateTime::parse_from_rfc3339(rfc3339)
                .expect("fixture stamp")
                .with_timezone(&Utc),
        )
    }

    /// **A THREAD WITH TWO DIFFERENT CLOCKS** — the only fixture that can tell
    /// the recents band's key apart from `last_turn_at`, which is why every
    /// existing band test was blind to the defect this round fixed.
    ///
    /// `spoke` is when he last put words in; `touched` is when something else
    /// last wrote to the header (a rename, a summary, a compaction, the
    /// creation stamp). `None` for `spoke` is a thread he has never spoken in.
    fn drifted(name: &str, threads: &[(&str, Option<&str>, &str)]) -> Project {
        Project {
            name: name.to_string(),
            root: std::path::PathBuf::from(format!("/Users/x/minds/{name}")),
            threads: threads
                .iter()
                .map(|(id, spoke, touched)| crate::index::IndexThread {
                    id: (*id).to_string(),
                    session_id: (*id).to_string(),
                    legacy_ids: Vec::new(),
                    summary_locked: false,
                    summary_set_at: None,
                    created_at: fixture_stamp(touched),
                    summary: format!("thread {id}"),
                    last_user_input_at: spoke.and_then(fixture_stamp),
                    last_turn_at: fixture_stamp(touched),
                    turn_count: if spoke.is_some() { 1 } else { 0 },
                    closed: false,
                    pinned: false,
                    pin_order: 0,
                    queued_count: 0,
                })
                .collect(),
            index: crate::index::IndexState::Read,
        }
    }

    /// Open a thread AND declare its load finished. Without the second half the
    /// projection legitimately prints a "loading…" rule, which is the right
    /// behaviour and the wrong fixture.
    fn opened(s: &mut Store, id: &str) {
        s.open(id);
        s.load_transcript(id, Vec::new(), 1);
    }

    fn ev(thread: &str, seq: u64, e: Value) -> TransportEvent {
        TransportEvent::Frame {
            msg: ServerMsg::Event {
                thread_id: thread.into(),
                seq,
                message_id: None,
                ts: "2026-09-14T11:59:00Z".into(),
                event: ThreadEvent::parse(e),
            },
            replayed: false,
        }
    }

    fn fleet(queued: u64, unread: u64) -> TransportEvent {
        TransportEvent::Fleet {
            statuses: vec![json!({
                "thread_id": "t1", "state": "idle", "executing": false,
                "queued": queued, "unread": unread, "last_seq": 42
            })],
            capped: false,
        }
    }

    fn texts(bs: &[Block]) -> Vec<String> {
        bs.iter().map(|b| b.text().to_string()).collect()
    }

    /// **PROJECT WITH NOTHING OPENED.** Per-row expansion is a set, and the
    /// overwhelmingly common fixture is the empty one; these two shadow the
    /// real functions so a test says what it is about rather than passing an
    /// empty set a hundred times. A test that IS about per-row expansion calls
    /// `super::thread_blocks` with a real set.
    fn thread_blocks(store: &Store, stdout_only: bool, expanded: bool) -> Vec<Block> {
        super::thread_blocks(
            store,
            stdout_only,
            expanded,
            &Opened::new(),
            &[],
            &TreeId::Parent,
            None,
        )
    }

    fn activity_blocks(store: &Store, stdout_only: bool, expanded: bool) -> Vec<Block> {
        super::activity_blocks(store, stdout_only, expanded, &Opened::new())
    }

    /// Shape one event with no projection context around it — the fixture for
    /// a test that is about ONE arm and nothing else.
    fn shape(ev: &ThreadEvent, expanded: bool) -> Shape {
        let names = HashMap::new();
        super::shape(
            ev,
            expanded,
            &Ctx {
                rich_rate_limits: false,
                task_names: &names,
            },
        )
    }

    /// A project the face would have read off disk, built from literals. Goes
    /// in through `adopt_projects`, which is the same door `load_projects`
    /// uses, so the membership set is derived by the shipped code and not by
    /// the fixture.
    fn project(name: &str, threads: &[(&str, &str, bool)]) -> Project {
        Project {
            name: name.to_string(),
            root: std::path::PathBuf::from(format!("/Users/x/minds/{name}")),
            threads: threads
                .iter()
                .enumerate()
                .map(|(i, (id, summary, pinned))| crate::index::IndexThread {
                    id: (*id).to_string(),
                    session_id: (*id).to_string(),
                    legacy_ids: Vec::new(),
                    summary_locked: false,
                    summary_set_at: None,
                    created_at: None,
                    summary: (*summary).to_string(),
                    // The ordinary fixture thread is one he SPOKE in, so both
                    // clocks read the same instant. A fixture that left
                    // `last_user_input_at` at `None` would put every row in the
                    // never-spoken class and silently stop testing the recents
                    // order at all.
                    last_user_input_at: fixture_stamp(&format!("2026-09-0{}T00:00:00Z", i + 1)),
                    last_turn_at: fixture_stamp(&format!("2026-09-0{}T00:00:00Z", i + 1)),
                    turn_count: 1,
                    closed: false,
                    pinned: *pinned,
                    pin_order: 0,
                    queued_count: 0,
                })
                .collect(),
            index: crate::index::IndexState::Read,
        }
    }

    /// Like [`project`], but with real control over the fields the ROOT's
    /// bands sort and filter on. The three-tuple fixture above cannot express
    /// a pin order, a close, or a distinct clock, and all three decide a band.
    /// A root of `n` dated threads, newest LAST in the fixture, so that a band
    /// that comes back newest-first has visibly reversed them.
    fn many(name: &str, n: usize) -> Project {
        Project {
            name: name.to_string(),
            root: std::path::PathBuf::from(format!("/Users/x/minds/{name}")),
            threads: (0..n)
                .map(|i| crate::index::IndexThread {
                    id: format!("{name}-{i:02}"),
                    session_id: format!("{name}-{i:02}"),
                    legacy_ids: Vec::new(),
                    summary_locked: false,
                    summary_set_at: None,
                    created_at: None,
                    summary: format!("thread {i}"),
                    // He spoke in it at that instant; both clocks agree.
                    last_user_input_at: fixture_stamp(&format!(
                        "2026-01-01T00:{:02}:00Z",
                        i.min(59)
                    )),
                    last_turn_at: fixture_stamp(&format!(
                        "2026-01-01T00:{:02}:00Z",
                        i.min(59)
                    )),
                    turn_count: 1,
                    closed: false,
                    pinned: false,
                    pin_order: 0,
                    queued_count: 0,
                })
                .collect(),
            index: crate::index::IndexState::Read,
        }
    }

    /// **TEN, COLLAPSED, AND EVERYTHING WHEN HE ASKS — `Q-14`'s answer, built.**
    ///
    /// The operator's complaint was that recent showed "like active right now"
    /// rather than the last twenty or thirty threads, and his own number for
    /// the collapsed band was ten. `0072` measured the cause at the line before
    /// touching it: there is no activity filter anywhere — `all_headers` walks
    /// every project and every thread, `Store::load_projects` reads every index
    /// eagerly — and the cap was the whole of it.
    ///
    /// FORCED RED three ways, each one separately:
    ///   * put `BAND_ROWS` back to 3 and the first assertion goes red at 3;
    ///   * drop the `focus` branch from `cap` and the page stays at ten;
    ///   * drop the `recent_total` capture (compute it after the truncate) and
    ///     the header stops being able to say what it is hiding.
    #[test]
    fn the_recent_band_holds_ten_on_the_root_and_all_of_them_on_its_page() {
        let mut s = store();
        s.adopt_projects(vec![many("harness", 40)]);

        let c = project_list(&s, 0, 0, None);
        assert_eq!(c.recent.len(), 10, "the collapsed band is not ten rows");
        assert_eq!(
            c.totals[tui_render::BandId::Recent.slot()],
            40,
            "the band does not know what it is hiding"
        );
        // NEWEST FIRST, and the fixture is written oldest-first so a band that
        // merely copied it would fail here.
        assert_eq!(c.recent[0].id, "harness-39");
        assert_eq!(c.recent[9].id, "harness-30");
        // DISTINCT — ten rows, ten threads.
        let mut ids: Vec<&str> = c.recent.iter().map(|r| r.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 10, "the band repeated a thread");

        let e = project_list(&s, 0, 0, Some(tui_render::BandId::Recent));
        assert_eq!(e.recent.len(), 40, "the page did not give him the rest");
        assert_eq!(e.recent[0].id, "harness-39", "the page lost the band's order");
        assert_eq!(e.recent[39].id, "harness-00");
        // **AND THE OTHER TWO BANDS ARE STILL CAPPED.** The page is focused on
        // ONE band; `focus` is not a global "show everything".
        assert_eq!(e.focus, Some(tui_render::BandId::Recent));
    }

    /// **ALL MEANS ALL: THE PAGE HAS NO CAP, AT ANY SIZE.** `I-2`.
    ///
    /// *"a dedicated page with just that band and all results"*, and `0133` is
    /// explicit that a silent truncate is the one thing this must not be. So the
    /// old `BAND_EXPANDED_MAX = 500` bound is GONE rather than raised, and this
    /// asserts the property at the sizes where a surviving off-by-one bound
    /// would show: 0, 10, 11, 500, 501 and 640.
    ///
    /// The bound that constant existed for was about DRAWING, by its own doc
    /// comment, and it has moved to where it belongs —
    /// `render::project_list` crops the surface to the viewport BEFORE painting
    /// a row (`I-12`).
    ///
    /// **AND THE PAGE IS NOTHING BUT THE BAND.** No header row (the header LINE
    /// names it), no `see all` foot (he is already there), no spacer, no `+ new
    /// project`, no `archived` — so `selectable()` is exactly the band's total,
    /// which is the one number a cap anywhere would change.
    ///
    /// FORCED RED by restoring `rows.truncate(500)` in `cap` for the focused
    /// band (the 640 case reads 500), and by leaving `root_lines` pushing
    /// `RootLine::Band(b)` under focus (every case is one too many). Both
    /// watched.
    #[test]
    fn the_page_is_the_whole_band_and_nothing_else_at_any_size() {
        use tui_render::BandId;
        for n in [0usize, 10, 11, 500, 501, 640] {
            let mut s = store();
            // ONE root of exactly `n`. Split across roots it was one arithmetic
            // slip away from a fixture that did not hold what the assertion
            // named, which is how a size test comes to test a different size.
            s.adopt_projects(vec![many("harness", n)]);
            let page = project_list(&s, 0, 0, Some(BandId::Recent));
            assert_eq!(
                page.totals[BandId::Recent.slot()], n,
                "the fixture did not produce {n} threads"
            );
            assert_eq!(page.recent.len(), n, "the page capped a band of {n}");
            assert_eq!(
                page.band_hidden(BandId::Recent),
                0,
                "the page is hiding rows of a band it was asked to show whole ({n})"
            );
            // The page's stop count IS the band's size — no chrome, in either
            // direction. An empty band is the one exception and it is a
            // SENTENCE, not a stop: see `BandId::empty_sentence`.
            let want = if n == 0 { 0 } else { n };
            assert_eq!(
                page.selectable(), want,
                "the page grew or lost a stop at {n}: {:?}",
                page.lines()
            );
            let lines = page.lines();
            if n == 0 {
                assert_eq!(
                    lines,
                    vec![tui_render::RootLine::Hint(tui_render::RECENT_EMPTY)],
                    "an empty page is a blank screen"
                );
            } else {
                for l in &lines {
                    assert!(
                        matches!(l, tui_render::RootLine::Recent(_)),
                        "the page drew root chrome at {n}: {l:?}"
                    );
                }
            }
        }
    }

    // ──────────────────────────────────────────────────────────────────────
    //  THE RECENTS BAND ORDERS ON LAST USER INPUT — his three exclusions
    //
    //  *"the recents need to be ordered on last user input, not on when they
    //  were created or any other events, including renames."*
    //
    //  **Every one of these needed the `drifted` fixture, and that is the
    //  finding.** With `last_user_input_at` and `last_turn_at` set to the same
    //  instant — which is what every other fixture in this module does, because
    //  it is what an ordinary thread looks like — the two are indistinguishable
    //  and the band cannot be tested at all. MEASURED: reverting `row_of` to
    //  `updated: t.last_turn_at`, i.e. putting the whole defect back, left all
    //  518 inherited tests GREEN. The behaviour was completely unprotected.
    // ──────────────────────────────────────────────────────────────────────

    /// **EXCLUSION (3) — "including renames."** THE DEFECT HE REPORTED, in the
    /// shape his own disk is in.
    ///
    /// `chess` was last spoken in on 2026-07-25 and was re-summarised on
    /// 2026-09-28. `yesterday` was spoken in on 2026-09-27 and not touched
    /// since. He expects `yesterday` first, and before this round he got
    /// `chess`, because the band read the field the rename moved.
    ///
    /// Measured on his live index at the time of the fix: 67 of 210 threads
    /// carry a `last_turn_at` sitting exactly on a summary event's timestamp,
    /// and THREE of them — silent since July and August — had been re-summarised
    /// within the hour and were leading his band.
    ///
    /// FORCED RED: point `row_of`'s `updated` back at `t.last_turn_at` and this
    /// fails with `chess` first — which is the operator's bug report, restated
    /// as an assertion.
    #[test]
    fn a_rename_does_not_move_a_thread_up_the_recent_band() {
        let mut s = store();
        s.adopt_projects(vec![drifted(
            "harness",
            &[
                // (id, when he last spoke, when the header was last touched)
                ("chess", Some("2026-07-25T04:43:53Z"), "2026-09-28T21:14:47Z"),
                (
                    "yesterday",
                    Some("2026-09-27T09:00:00Z"),
                    "2026-09-27T09:00:00Z",
                ),
            ],
        )]);
        let l = project_list(&s, 0, 0, None);
        // POSITIVE WITNESS: the band is populated and holds both rows, so the
        // order assertion below is about ORDER and not about an empty band.
        assert_eq!(l.recent.len(), 2, "the band did not come back");
        assert_eq!(
            l.recent.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["yesterday", "chess"],
            "a RENAME pushed a thread he has not spoken in since July above one \
             he spoke in yesterday — this is the defect verbatim"
        );
    }

    /// **EXCLUSION (2) — "not any other events."**
    ///
    /// Same fixture shape, with the late header write coming from a COMPACTION
    /// rather than a rename — the `9f3e215d` case on his disk, where a
    /// compaction boundary moved `last_turn_at` four days past his last turn.
    /// The band must not care what kind of non-input event it was.
    ///
    /// FORCED RED: point `row_of`'s `updated` back at `t.last_turn_at` and
    /// `compacted` leads the band.
    #[test]
    fn a_non_user_event_does_not_move_a_thread_up_the_recent_band() {
        let mut s = store();
        s.adopt_projects(vec![drifted(
            "harness",
            &[
                (
                    "compacted",
                    Some("2026-08-29T00:09:14Z"),
                    "2026-09-02T21:42:57Z",
                ),
                (
                    "spoken",
                    Some("2026-08-30T00:00:00Z"),
                    "2026-08-30T00:00:00Z",
                ),
            ],
        )]);
        let l = project_list(&s, 0, 0, None);
        assert_eq!(l.recent.len(), 2, "the band did not come back");
        assert_eq!(
            l.recent.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["spoken", "compacted"],
            "a compaction moved a thread up the recents band"
        );
    }

    /// **EXCLUSION (1) — "not on when they were created."**
    ///
    /// A thread created two minutes ago with zero user input must not lead the
    /// band, and must not be missing from it either: it sits at the BOTTOM.
    /// `ThreadHeader::with_id` stamps `last_turn_at: now` beside
    /// `created_at: now`, so before this round a brand-new empty thread was
    /// automatically the most recent thing he owned.
    ///
    /// Three of his 210 live headers are in exactly this state.
    ///
    /// FORCED RED: point `row_of`'s `updated` back at `t.last_turn_at` and
    /// `fresh` leads the band instead of trailing it.
    #[test]
    fn a_thread_he_has_never_spoken_in_does_not_lead_the_recent_band() {
        let mut s = store();
        s.adopt_projects(vec![drifted(
            "harness",
            &[
                // Never spoken in, created moments ago — the newest `created_at`
                // and the newest `last_turn_at` in the fixture.
                ("fresh", None, "2026-09-28T23:59:00Z"),
                ("older", Some("2026-09-01T00:00:00Z"), "2026-09-01T00:00:00Z"),
                ("oldest", Some("2026-08-01T00:00:00Z"), "2026-08-01T00:00:00Z"),
            ],
        )]);
        let l = project_list(&s, 0, 0, None);
        assert_eq!(l.recent.len(), 3, "the band did not come back");
        assert_eq!(
            l.recent.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["older", "oldest", "fresh"],
            "a thread with zero user input sorted as the most recent thing he \
             has — or fell out of the band, which is the other way to get this \
             wrong"
        );
    }

    /// **THE SORT AND THE AGE COLUMN READ THE SAME FIELD.** The property
    /// `row_of` carried a comment defending before this round — *"mixing them
    /// would make the sort and the age disagree on screen"* — and the one that
    /// breaking silently would be worse than either shape of the fix.
    ///
    /// `render` reads `ThreadRow::updated` for the age column and `project_list`
    /// sorts on the same field, so the check is that `updated` IS the index's
    /// `last_user_input_at` for every row — never `last_turn_at`, which on this
    /// fixture is two months out.
    ///
    /// FORCED RED: point `row_of`'s `updated` at `t.last_turn_at` and every row
    /// fails; point it at `t.created_at` and every row fails.
    #[test]
    fn the_recent_sort_and_the_age_column_read_one_field() {
        let mut s = store();
        s.adopt_projects(vec![drifted(
            "harness",
            &[
                ("chess", Some("2026-07-25T04:43:53Z"), "2026-09-28T21:14:47Z"),
                ("fresh", None, "2026-09-28T23:59:00Z"),
            ],
        )]);
        let l = project_list(&s, 0, 0, None);
        assert_eq!(l.recent.len(), 2);
        for row in &l.recent {
            let hdr = s
                .projects
                .iter()
                .flat_map(|p| p.threads.iter())
                .find(|t| t.id == row.id)
                .expect("the header the row came from");
            assert_eq!(
                row.updated, hdr.last_user_input_at,
                "the age column and the sort key disagree for `{}`",
                row.id
            );
            assert_ne!(
                row.updated, hdr.last_turn_at,
                "the fixture stopped distinguishing the two clocks, so this \
                 test proves nothing"
            );
        }
    }

    /// **A THREAD WITH NO LAST USER INPUT SORTS LAST.** Named
    /// `a_thread_with_no_last_turn_at_sorts_to_the_bottom_of_the_recent_band`
    /// until the band moved off `last_turn_at`; the property is unchanged and
    /// the field it is about is now `last_user_input_at`.
    ///
    /// **A THREAD THE DAEMON NEVER DATED SORTS LAST**, which is the one thing
    /// the real index could not prove: `0072`'s probe over his actual `~/minds`
    /// found 182 recent rows and NOT ONE of them undated, so this property is
    /// unexercised by live data and would rot silently.
    ///
    /// An age we were never given is not a claim of recency. `Reverse(None)` is
    /// the largest key, so this holds by construction rather than by luck —
    /// FORCED RED by flipping the comparator to `sort_by_key(|r| r.updated)`,
    /// which puts the undated row FIRST and calls it the newest thing he owns.
    #[test]
    fn a_thread_with_no_last_user_input_sorts_to_the_bottom_of_the_recent_band() {
        let mut s = store();
        let mut p = many("harness", 3);
        p.threads.push(crate::index::IndexThread {
            id: "undated".into(),
            session_id: "undated".into(),
            legacy_ids: Vec::new(),
            summary_locked: false,
            summary_set_at: None,
            created_at: None,
            summary: "never dated".into(),
            // BOTH absent. `last_turn_at: None` is what the old spelling of
            // this test was about; the band now orders on `last_user_input_at`,
            // and a row with neither is the honest "he has never spoken here".
            last_user_input_at: None,
            last_turn_at: None,
            turn_count: 1,
            closed: false,
            pinned: false,
            pin_order: 0,
            queued_count: 0,
        });
        s.adopt_projects(vec![p]);
        let l = project_list(&s, 0, 0, None);
        assert_eq!(l.recent.len(), 4);
        // A POSITIVE WITNESS FIRST: the dated rows are in the order they
        // should be, so the assertion below is about WHERE the undated row
        // went and not about a band that came back empty or scrambled.
        assert_eq!(
            l.recent
                .iter()
                .take(3)
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            vec!["harness-02", "harness-01", "harness-00"],
        );
        assert_eq!(l.recent[3].id, "undated", "an undated row claimed recency");
    }

    /// **THE RECENTS BAND IS ORDERED AND AGED ON THE DAEMON'S LAST USER INPUT.**
    ///
    /// The regression (2026-10-16): since the threads move nothing writes
    /// `last_user_input_at` into `.mind/threads.json` on his turns, so every
    /// index froze and the band read `idle > 8d` on every row, in the frozen
    /// order — and a thread started since the move, which has NO header in any
    /// index, was not on the band at all. The daemon's `threads/recent-updates`
    /// stamp (carried on the `Threads` row as `last_user_input_at`) is the live
    /// clock: it must (1) move an indexed thread to the top, with ITS stamp as
    /// the row's age, and (2) bring a daemon-only thread onto the band, named by
    /// the daemon's title and placed in the project its cwd lives under.
    #[test]
    fn the_recent_band_orders_and_ages_on_the_daemons_last_user_input() {
        let mut s = store();
        s.adopt_projects(vec![many("harness", 3)]);
        // POSITIVE WITNESS: before the daemon speaks, the frozen index order.
        let before = project_list(&s, 0, 0, None);
        assert_eq!(before.recent[0].id, "harness-02");
        s.apply(
            TransportEvent::Threads {
                rows: vec![
                    // An indexed thread he spoke in today: the index says Jan 1.
                    json!({"thread_id":"harness-00","cwd":"/Users/x/minds/harness",
                           "last_user_input_at":"2026-10-16T15:00:00+00:00"}),
                    // A thread no index carries, spoken in just after.
                    // (TUI-P1a: started by one of his faces — the band now
                    // shows only those.)
                    json!({"thread_id":"fresh","cwd":"/Users/x/minds/harness/sub",
                           "title":"Audit the thread ledger",
                           "identity": true, "app_id": "harness-tui",
                           "last_user_input_at":"2026-10-16T16:00:00+00:00"}),
                    // A daemon row he never spoke in has no place on the band.
                    json!({"thread_id":"silent","cwd":"/Users/x/elsewhere"}),
                ],
                capped: false,
            },
            now(),
        );
        let l = project_list(&s, 0, 0, None);
        let ids: Vec<&str> = l.recent.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["fresh", "harness-00", "harness-02", "harness-01"]);
        // The AGE column renders `updated`: it must be the daemon's stamp, not
        // the frozen index's.
        assert_eq!(l.recent[0].updated, fixture_stamp("2026-10-16T16:00:00Z"));
        assert_eq!(l.recent[1].updated, fixture_stamp("2026-10-16T15:00:00Z"));
        assert_eq!(l.recent[0].summary, "Audit the thread ledger");
        assert_eq!(l.recent[0].project, "harness");
        // An OLDER daemon stamp never drags a newer index stamp backwards.
        s.apply(
            TransportEvent::Threads {
                rows: vec![json!({"thread_id":"harness-01",
                                  "last_user_input_at":"2025-01-01T00:00:00+00:00"})],
                capped: false,
            },
            now(),
        );
        let l = project_list(&s, 0, 0, None);
        let h1 = l.recent.iter().find(|r| r.id == "harness-01").expect("harness-01");
        assert_eq!(h1.updated, fixture_stamp("2026-01-01T00:01:00Z"));
    }

    // ── TUI-P1a: naming, the thread header, and recents that are only his ──

    /// An identity row for a daemon-only thread he spoke in.
    fn daemon_row(id: &str, cwd: &str, extra: Value) -> Value {
        let mut r = json!({"thread_id": id, "cwd": cwd,
                           "last_user_input_at": "2026-10-20T10:00:00+00:00"});
        for (k, v) in extra.as_object().expect("an object") {
            r[k] = v.clone();
        }
        r
    }

    fn threads_ev(rows: Vec<Value>) -> TransportEvent {
        TransportEvent::Threads { rows, capped: false }
    }

    /// **AN UNNAMED THREAD IS CALLED BY ITS FIRST MESSAGE, ELSE `untitled`, AND
    /// NEVER BY ITS PATH; A NAMED ONE BY ITS NAME** (TUI-P1a, TC-02). N = 2 of
    /// each kind, every one with a cwd whose last segment a path-fallback would
    /// have shown — so the "never the path" assertions can fail.
    ///
    /// Forcings (report): put the cwd's last segment back as `name_of`'s last
    /// rung → the untitled rows go red; drop the `first_line` rung → the
    /// first-line rows go red; rank `first_line` above the daemon name → the
    /// named rows go red.
    #[test]
    fn an_unnamed_thread_is_its_first_line_else_untitled_and_never_its_path() {
        let mut s = store();
        s.apply(
            threads_ev(vec![
                daemon_row("u1", "/Users/x/code/pathone", json!({})),
                daemon_row("u2", "/Users/x/.aui/miscellaneous", json!({})),
                daemon_row("f1", "/Users/x/code/pathtwo", json!({})),
                daemon_row("f2", "/Users/x/code/paththree", json!({})),
                daemon_row("n1", "/Users/x/code/pathfour", json!({"name": "Answer the tax letter"})),
                daemon_row("n2", "/Users/x/code/pathfive", json!({"name": "Plan the trip"})),
            ]),
            now(),
        );
        for (id, line) in [("f1", "  \n  fix the login page\nand more"), ("f2", "why is the build red?"), ("n1", "ignored, it has a name"), ("n2", "ignored too")] {
            s.apply(TransportEvent::FirstLine { thread_id: id.into(), line: line.into() }, now());
        }
        assert_eq!(s.label_of("u1"), UNTITLED);
        assert_eq!(s.label_of("u2"), UNTITLED);
        assert_eq!(s.label_of("f1"), "fix the login page", "the first NON-BLANK line, trimmed");
        assert_eq!(s.label_of("f2"), "why is the build red?");
        assert_eq!(s.label_of("n1"), "Answer the tax letter");
        assert_eq!(s.label_of("n2"), "Plan the trip");
        for (id, seg) in [("u1", "pathone"), ("u2", "miscellaneous"), ("f1", "pathtwo"), ("f2", "paththree")] {
            assert!(!s.label_of(id).contains(seg), "{id} is named by its path: {}", s.label_of(id));
        }
        // …and the HEADER says the same, because it reads the same name.
        s.open("u2");
        let h = header(&s, tui_render::Mode::Thread(tui_render::Region::Composer));
        assert!(h.title.starts_with("untitled · "), "{}", h.title);
        assert!(!h.title.contains("miscellaneous"), "the header named the thread by its path: {}", h.title);
    }

    /// **A RENAME THAT ARRIVES WHILE HE IS LOOKING AT THE THREAD CHANGES ITS
    /// HEADER AND ITS ROW IN PLACE** — no reload, no re-subscribe, no index
    /// re-read: one identity row carrying the new name (TUI-P1a, TC-03). Two
    /// renames in a row, so a store that kept the FIRST name fails as surely as
    /// one that never took any.
    ///
    /// Forcings (report): drop the `name` arm from `TransportEvent::Threads`
    /// → red; make `name_of` prefer the index summary over the daemon's name →
    /// red on the indexed thread.
    #[test]
    fn a_rename_arriving_while_the_thread_is_open_changes_its_header_in_place() {
        let mut s = store();
        s.adopt_projects(vec![project("harness", &[("t1", "what the index calls it", false)])]);
        s.open("t1");
        s.apply(
            TransportEvent::Fleet {
                statuses: vec![json!({"thread_id": "t1", "state": "idle", "executing": false})],
                capped: false,
            },
            now(),
        );
        let mode = tui_render::Mode::Thread(tui_render::Region::Composer);
        assert_eq!(header(&s, mode).title, "what the index calls it · idle");
        for name in ["Tax letter", "Tax letter, final"] {
            s.apply(threads_ev(vec![json!({"thread_id": "t1", "name": name, "name_manual": true,
                                           "name_at": "2026-10-25T10:00:00Z"})]), now());
            assert_eq!(header(&s, mode).title, format!("{name} · idle"), "the open header did not follow the rename");
            let row = project_list(&s, 0, 0, None);
            assert!(row.recent.iter().any(|r| r.id == "t1" && r.summary == name), "the row did not follow the rename");
        }
    }

    /// **A NAME HE LOCKED IN THE AUI OUTRANKS AN AGENT'S DRIFTING DAEMON
    /// SUMMARY, AND A LATER MANUAL RENAME OUTRANKS THE LOCK.**
    #[test]
    fn a_locked_aui_name_beats_a_drifting_summary_and_loses_to_a_later_manual_one() {
        let mut s = store();
        let mut p = project("harness", &[("t1", "my name for it", false)]);
        p.threads[0].summary_locked = true;
        p.threads[0].summary_set_at = fixture_stamp("2026-10-01T00:00:00Z");
        s.adopt_projects(vec![p]);
        s.apply(threads_ev(vec![json!({"thread_id": "t1", "name": "agent drift"})]), now());
        assert_eq!(s.label_of("t1"), "my name for it", "an agent's summary displaced his locked name");
        s.apply(threads_ev(vec![json!({"thread_id": "t1", "name": "renamed in the tui", "name_manual": true,
                                       "name_at": "2026-10-02T00:00:00Z"})]), now());
        assert_eq!(s.label_of("t1"), "renamed in the tui", "a later manual rename lost to an older lock");
        s.apply(threads_ev(vec![json!({"thread_id": "t1", "name": "older manual", "name_manual": true,
                                       "name_at": "2026-09-01T00:00:00Z"})]), now());
        assert_eq!(s.label_of("t1"), "my name for it", "an OLDER manual name beat the lock");
    }

    /// **RECENTS SHOW ONLY THREADS HIS FACES STARTED** (TUI-P1a, TC-05): N = 2
    /// of his (`harness-tui`, `aui`) are on the band; N = 2 agent threads are
    /// not — one un-attributed (`app_id` absent, the DEPLOY proof's shape) and
    /// one that copied `harness-tui` but carries an agent's tag (a spawned
    /// child's shape) — and neither is a thread whose identity has not been
    /// read yet.
    ///
    /// Forcings (report): drop the `is_his_app` clause from
    /// `daemon_only_recent` → both agent threads appear; make `is_his_app`
    /// ignore the tag → the tagged one appears; `is_his_app` returning
    /// `false` → his two vanish. (Dropping a separate "identity read" gate
    /// changed nothing, F08, so the gate was deleted — `None` already hides the
    /// un-read one.)
    #[test]
    fn recents_show_only_threads_his_faces_started() {
        let mut s = store();
        s.apply(
            threads_ev(vec![
                daemon_row("his-tui", "/Users/x/.aui/miscellaneous",
                           json!({"identity": true, "app_id": "harness-tui", "name": "mine, from the tui"})),
                daemon_row("his-aui", "/Users/x/code/harness",
                           json!({"identity": true, "app_id": "aui", "name": "mine, from the aui"})),
                daemon_row("proof", "/Users/x/code/project/notes/deploy-proof",
                           json!({"identity": true})),
                daemon_row("child", "/private/tmp/r24-proof/work",
                           json!({"identity": true, "app_id": "harness-tui", "agent": "agents_spawn:his-tui"})),
                daemon_row("unread", "/tmp/w12/scratch", json!({})),
            ]),
            now(),
        );
        let l = project_list(&s, 0, 0, None);
        let mut ids: Vec<&str> = l.recent.iter().map(|r| r.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["his-aui", "his-tui"], "recents are not exactly his: {ids:?}");
        assert_eq!(
            band_total(&s, tui_render::BandId::Recent),
            2,
            "the band's depth does not count what the band lists"
        );
    }

    /// **PROOF THREADS MADE BEFORE TAGGING — HIS APP ID, NO TAG, A SCRATCH
    /// CWD — ARE NOT HIS** (TUI-P1a). The four that survived the app filter in
    /// the prod census, in their real shapes: N = 3 proof-shaped rows (one
    /// `aui` under `/var/folders`, two `harness-tui` under `/private/tmp`) are
    /// hidden; opposed by N = 2 of his with the SAME app ids in real
    /// directories, which stay — so neither "hide every aui/harness-tui" nor
    /// "hide nothing" passes.
    ///
    /// Forcings (report): `in_scratch_dir` returning `false` → the three
    /// proofs appear; returning `true` → his two vanish.
    #[test]
    fn proof_threads_in_scratch_dirs_are_not_his_even_with_his_app_id() {
        let mut s = store();
        s.apply(
            threads_ev(vec![
                daemon_row("d19dee63", "/var/folders/rk/v7pd/T/aui-threads-proof-96412-52542",
                           json!({"identity": true, "app_id": "aui"})),
                daemon_row("c55920a4", "/private/tmp/claude-501/r24-proof/work",
                           json!({"identity": true, "app_id": "harness-tui"})),
                daemon_row("06ab8e19", "/private/tmp/claude-501/r26-mcp-proof/work",
                           json!({"identity": true, "app_id": "harness-tui"})),
                daemon_row("his-aui", "/Users/x/code/harness", json!({"identity": true, "app_id": "aui"})),
                daemon_row("his-tui", "/Users/x/.aui/miscellaneous", json!({"identity": true, "app_id": "harness-tui"})),
            ]),
            now(),
        );
        let l = project_list(&s, 0, 0, None);
        let mut ids: Vec<&str> = l.recent.iter().map(|r| r.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["his-aui", "his-tui"], "proof threads reached his recents: {ids:?}");
    }

    /// **HIS THREE WORDS** — the header's status is `active`, `idle` or an
    /// error and nothing else (operator ruling r42).
    #[test]
    fn the_status_is_active_idle_or_an_error_and_nothing_else() {
        for (w, his) in [
            ("active", "active"),
            ("retrying", "active"),
            ("parked", "error: parked"),
            ("idle", "idle"),
            ("queued", "idle"),
            ("done", "idle"),
            ("?", "?"),
        ] {
            assert_eq!(status_in_his_words(w), his, "{w}");
        }
    }

    /// **WHAT A BAND HEADER SAYS, AND WHERE THE AFFORDANCE ACTUALLY LIVES.**
    ///
    /// The header carries the TOTAL and nothing else. It carried
    /// `10 of 41 · → for all` for exactly one build, until:
    ///
    /// > Don't show `· → for all` in the header. It can be discovered.
    ///
    /// "Discovered" is not left to chance: the `see all` row at the foot of the
    /// band is the affordance, in his own word, in the place he is already
    /// looking when he runs out of rows. `→` on the header is the second door
    /// and is deliberately unadvertised.
    ///
    /// **A COMPLETE BAND GETS NO FOOT ROW AT ALL** — a `see all — 0 more` is
    /// the invented-zero defect wearing an affordance. The HEADER still opens
    /// its page there, which is the one law `→` now has; that half is asserted
    /// in `app.rs`, where the key lives.
    ///
    /// FORCED RED by putting the hint back in `band_note`, by making
    /// `band_hidden` return non-zero unconditionally (a complete band grows a
    /// foot row), and by dropping the `SeeAll` push from `root_lines` (the only
    /// taught door disappears).
    #[test]
    fn the_header_says_the_total_and_the_see_all_row_says_the_rest() {
        use tui_render::{BandId, RootLine};
        let mut s = store();
        s.adopt_projects(vec![
            banded(
                "harness",
                &[("p", "a pin", true, 0, false, "2026-09-01T00:00:00Z")],
            ),
            many("other", 40),
        ]);

        let c = project_list(&s, 0, 0, None);
        // THE HEADER: a total, for every band, with nothing that looks like a
        // key anywhere in it.
        assert_eq!(c.band_note(BandId::Pinned), "1");
        assert_eq!(c.band_note(BandId::Projects), "2");
        assert_eq!(c.band_note(BandId::Recent), "41");
        for b in BandId::ALL {
            let note = c.band_note(b);
            assert!(!note.contains('\u{2192}'), "{b:?} advertises a key: {note}");
            assert!(!note.contains('\u{2190}'), "{b:?} advertises a key: {note}");
        }

        // THE FOOT: only the capped band has one, and it says the depth in
        // HIS WORD. The wording is `0133`'s and is his to overturn — the bare
        // `see all`, with no count, is one format string.
        assert!(c.band_has_more(BandId::Recent));
        assert_eq!(c.band_hidden(BandId::Recent), 31);
        assert_eq!(c.see_all_words(BandId::Recent), "see all — 31 more");
        let lines = c.lines();
        assert!(lines.contains(&RootLine::SeeAll(BandId::Recent)));
        // **COMPLETE BANDS GET NOTHING.** Positive witness first: their headers
        // ARE on the surface, so the absence below is about the foot row.
        for b in [BandId::Pinned, BandId::Projects] {
            assert!(lines.contains(&RootLine::Band(b)), "{b:?} lost its header");
            assert!(!c.band_has_more(b));
            assert_eq!(c.band_hidden(b), 0);
            assert!(
                !lines.contains(&RootLine::SeeAll(b)),
                "{b:?} grew a dead foot"
            );
        }

        // **ON ITS OWN PAGE THERE IS NO FOOT EITHER**, and for the opposite
        // reason: he is already there. Nothing is hidden, so nothing offers to
        // show more — which is `show less`'s epitaph. In-place expansion is
        // gone, so there is nothing to collapse and no row that could say so.
        let e = project_list(&s, 0, 0, Some(BandId::Recent));
        assert_eq!(e.band_note(BandId::Recent), "41");
        assert_eq!(e.band_hidden(BandId::Recent), 0);
        let page = e.lines();
        assert_eq!(page.len(), 41, "the page is not exactly the band: {page:?}");
        for b in BandId::ALL {
            assert!(
                !page.contains(&RootLine::SeeAll(b)),
                "{b:?}'s page grew a foot"
            );
            assert!(
                !page.contains(&RootLine::Band(b)),
                "{b:?}'s page grew a caption over its only band"
            );
        }
        assert!(!page.contains(&RootLine::NewProject), "the page grew root chrome");
        assert!(!page.contains(&RootLine::Archived), "the page grew root chrome");
        assert!(!page.contains(&RootLine::Spacer), "the page grew a spacer");

        // **A `recent` BAND THAT HAPPENS TO BE COMPLETE REPORTS ITSELF SO**,
        // derived from the rows rather than from which band it is.
        let mut small = store();
        small.adopt_projects(vec![many("tiny", 4)]);
        let t = project_list(&small, 0, 0, None);
        assert_eq!(t.band_note(BandId::Recent), "4");
        assert!(!t.band_has_more(BandId::Recent));
        assert!(!t.lines().contains(&RootLine::SeeAll(BandId::Recent)));
    }

    /// **TEN IS THE DEFAULT FOR ALL THREE BANDS**, which is his second ruling:
    ///
    /// > Let's make the default for pinned and projects just 10 like the
    /// > recents.
    ///
    /// Sized to his real shape — 26 pins, 33 projects — so the two bands this
    /// generalised to are both genuinely capped rather than accidentally
    /// complete.
    ///
    /// FORCED RED by capping only `recent` (pinned comes back at 26 and
    /// projects at 33, which is the pre-amendment build), and by making `cap`
    /// ignore `focus` and uncap every band (one page then shows all three
    /// bands whole, which is the root with the cap removed rather than a page).
    #[test]
    fn every_band_defaults_to_ten_and_each_one_has_its_own_page() {
        use tui_render::BandId;
        let ids: Vec<String> = (0..26).map(|i| format!("pin{i:02}")).collect();
        let pins: Vec<(&str, &str, bool, i64, bool, &str)> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| {
                (
                    id.as_str(),
                    "a pin",
                    true,
                    i as i64,
                    false,
                    "2026-09-01T00:00:00Z",
                )
            })
            .collect();
        let mut roots = vec![banded("harness", &pins)];
        roots.extend((0..32).map(|i| many(&format!("r{i:02}"), 1)));
        let mut s = store();
        s.adopt_projects(roots);

        let c = project_list(&s, 0, 0, None);
        assert_eq!(c.pinned.len(), 10, "the pinned band is not capped at ten");
        assert_eq!(c.rows.len(), 10, "the projects band is not capped at ten");
        assert_eq!(c.recent.len(), 10);
        assert_eq!(c.totals, [26, 58, 33]);
        for b in BandId::ALL {
            assert!(c.band_has_more(b), "{b:?} is not hiding anything to see");
        }

        // **ONE BAND AT A TIME, AND THAT IS WHAT A PAGE IS.** The pinned page
        // shows every pin and leaves the other two bands capped — `focus` is a
        // FOCUS and not a global "show everything", which is what makes the
        // page "just that band" rather than the root with the cap taken off.
        let e = project_list(&s, 0, 0, Some(BandId::Pinned));
        assert_eq!(e.pinned.len(), 26, "the pinned page did not show every pin");
        assert_eq!(e.rows.len(), 10, "the pinned page uncapped projects too");
        assert_eq!(e.recent.len(), 10, "the pinned page uncapped recent too");
        // …and the surface it lays out is the pinned band alone.
        assert_eq!(e.selectable(), 26);
    }

    fn banded(name: &str, threads: &[(&str, &str, bool, i64, bool, &str)]) -> Project {
        Project {
            name: name.to_string(),
            root: std::path::PathBuf::from(format!("/Users/x/minds/{name}")),
            threads: threads
                .iter()
                .map(
                    |(id, summary, pinned, pin_order, closed, at)| crate::index::IndexThread {
                        id: (*id).to_string(),
                        session_id: (*id).to_string(),
                        legacy_ids: Vec::new(),
                        summary_locked: false,
                        summary_set_at: None,
                        created_at: None,
                        summary: (*summary).to_string(),
                        // The `at` column of this fixture is "when he last
                        // spoke", which is what both bands order on.
                        last_user_input_at: fixture_stamp(at),
                        last_turn_at: fixture_stamp(at),
                        turn_count: 1,
                        closed: *closed,
                        pinned: *pinned,
                        pin_order: *pin_order,
                        queued_count: 0,
                    },
                )
                .collect(),
            index: crate::index::IndexState::Read,
        }
    }

    /// **THE ROOT'S BANDS, WHICH IS WHAT HE NOTICED WAS MISSING.**
    ///
    /// Pinned in pin order; recent newest-first, closed excluded, capped; and
    /// `~/.aui/miscellaneous` is a peer of the projects in both — it is
    /// mechanically just another root and the doc draws it as one.
    ///
    /// FORCED: flip the pinned sort's `pin_order` key to `updated` and the pin
    /// order goes to `["b", "a"]`; drop the `!r.closed` filter and the closed
    /// thread walks into the recent band.
    #[test]
    fn the_root_bands_are_pins_in_pin_order_and_recents_newest_first() {
        let mut s = store();
        s.adopt_projects(vec![
            banded(
                "harness",
                &[
                    // Pinned SECOND but touched most recently: pin order wins.
                    ("a", "the older pin", true, 1, false, "2026-09-09T00:00:00Z"),
                    ("b", "the newer pin", true, 0, false, "2026-09-01T00:00:00Z"),
                    ("c", "a closed one", false, 0, true, "2026-09-10T00:00:00Z"),
                    ("d", "plain", false, 0, false, "2026-09-05T00:00:00Z"),
                ],
            ),
            banded(
                "miscellaneous",
                &[(
                    "m",
                    "Answer the tax letter",
                    false,
                    0,
                    false,
                    "2026-09-08T00:00:00Z",
                )],
            ),
        ]);
        let ps = project_list(&s, 0, 0, None);

        // PIN ORDER, not recency — `b` has the lower `pin_order`.
        assert_eq!(
            ps.pinned.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["b", "a"],
            "the pinned band is not in pin order"
        );
        // RECENT: newest first, and the CLOSED thread is not in it even though
        // it is the newest thing in the fixture. It stays inside its project.
        assert_eq!(
            ps.recent.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            // FOUR, not three: `BAND_ROWS` is ten now, so nothing in this
            // fixture is capped out and `b` — the newer pin, 2026-09-01 — is
            // in the band on its own recency. A pinned thread is NOT excluded
            // from recent, deliberately.
            vec!["a", "m", "d", "b"],
            "the recent band is not newest-first-minus-closed"
        );
        assert!(
            !ps.recent.iter().any(|r| r.id == "c"),
            "a closed thread walked into the cross-project recent feed"
        );
        // MISC IS A PEER. Its thread is in the band, named by its own root —
        // which is the only thing on a band row that says where it lives.
        let tax = ps
            .recent
            .iter()
            .find(|r| r.id == "m")
            .expect("the misc row");
        assert_eq!(tax.project, "miscellaneous");
        assert!(
            ps.rows.iter().any(|p| p.name == "miscellaneous"),
            "the bucket lost its project row"
        );
        // The cap is ONE constant, and `Q-14` is the thing that moves it.
        assert!(ps.recent.len() <= BAND_ROWS);
    }

    /// **THE CAP, FORCED.** Four eligible threads, three rows.
    #[test]
    fn the_recent_band_is_capped_at_one_obvious_constant() {
        let mut s = store();
        s.adopt_projects(vec![banded(
            "harness",
            &[
                ("a", "one", false, 0, false, "2026-09-01T00:00:00Z"),
                ("b", "two", false, 0, false, "2026-09-02T00:00:00Z"),
                ("c", "three", false, 0, false, "2026-09-03T00:00:00Z"),
                ("d", "four", false, 0, false, "2026-09-04T00:00:00Z"),
            ],
        )]);
        let ps = project_list(&s, 0, 0, None);
        assert_eq!(ps.recent.len(), BAND_ROWS.min(4));
        // **TEN, AND `Q-14` IS THE REASON IT IS NOT THREE.** The operator
        // answered it in his own words and with his own number — "if we just
        // show like the last 10 we can do that" — so this asserts the ANSWER
        // and not the old frame. It is pinned to a literal deliberately: the
        // constant is the whole of the fix `0072` shipped, and a test written
        // as `BAND_ROWS == BAND_ROWS` would let it drift back to three in
        // silence.
        assert_eq!(
            BAND_ROWS, 10,
            "Q-14 is answered: ten, collapsed, expandable"
        );
        // The cap takes from the OLD end, never the new.
        assert_eq!(ps.recent[0].id, "d");
    }

    /// **THE GIT-SHARED HEADER, ACROSS ROOTS — and it is a third of his fleet.**
    ///
    /// `.mind/threads.json` is tracked in git, so a repo seeded from another
    /// carries its headers: 46 ids live in three or four indexes. `index::dedupe`
    /// folds duplicates WITHIN one index and cannot see this; without the
    /// cross-root fold the pinned band shows "Broomstick platform braindump"
    /// four times, which reads as a new bug introduced by giving him bands.
    ///
    /// FORCED: remove the `find(|r| r.id == t.id)` arm in `all_headers` and
    /// both bands double.
    #[test]
    fn a_thread_in_three_roots_is_one_band_row_and_the_newest_header_wins() {
        let mut s = store();
        s.adopt_projects(vec![
            banded(
                "broomstick",
                &[(
                    "dup",
                    "the stale copy",
                    true,
                    0,
                    false,
                    "2026-01-01T00:00:00Z",
                )],
            ),
            banded(
                "ewing",
                &[(
                    "dup",
                    "the newest copy",
                    true,
                    0,
                    false,
                    "2026-03-01T00:00:00Z",
                )],
            ),
            banded(
                "splash",
                &[(
                    "dup",
                    "a middling copy",
                    true,
                    0,
                    false,
                    "2026-02-01T00:00:00Z",
                )],
            ),
        ]);
        let ps = project_list(&s, 0, 0, None);
        assert_eq!(
            ps.pinned.len(),
            1,
            "the pinned band showed the same thread three times"
        );
        assert_eq!(
            ps.recent.len(),
            1,
            "the recent band showed the same thread three times"
        );
        // The SURVIVOR is the newest header, and it keeps the root it was read
        // from — which is the project a pin write would be addressed to.
        assert_eq!(ps.pinned[0].summary, "the newest copy");
        assert_eq!(
            ps.pinned[0].project, "ewing",
            "the row forgot which index it came from, so a pin has no address"
        );
    }

    #[test]
    fn a_fresh_store_knows_nothing_and_says_so_rather_than_saying_zero() {
        let s = store();
        assert!(matches!(s.queued(None), Known::Unknown));
        assert!(matches!(s.unread_elsewhere(None), Known::Unknown));
        assert!(matches!(s.thread_count(), Known::Unknown));
        let h = header(&s, Mode::Projects);
        assert!(h.title.contains("reading ~/minds"), "{}", h.title);
        assert!(matches!(h.queued, Known::Unknown), "a fresh badge is ?");
    }

    /// **THE INVERSION, AS A TEST.** Every row on the list exists because an
    /// INDEX header exists. A daemon thread that is in no index does not
    /// appear; an index thread the daemon has never heard of does.
    #[test]
    fn the_index_drives_the_list_and_the_daemon_only_decorates_it() {
        let mut s = store();
        s.adopt_projects(vec![project(
            "harness",
            &[
                ("mine", "the first thread", false),
                ("unadopted", "never sent", false),
            ],
        )]);
        s.enter_project(0);
        // The daemon knows one of his and one that is nobody's — the arena
        // case, which is 1,617 threads on his real fleet.
        s.apply(
            TransportEvent::Threads {
                rows: vec![
                    json!({"thread_id":"mine","cwd":"/Users/x/minds/harness","updated_at":"2026-09-01T00:00:00Z"}),
                    json!({"thread_id":"arena","cwd":"/Users/x/code/broomstick/workshops/arena/runs/hostile/llm/era000","updated_at":"2026-09-14T00:00:00Z"}),
                ],
                capped: false,
            },
            now(),
        );
        let l = thread_list(&s, 0, 0);
        let ids: Vec<&str> = l.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["mine", "unadopted"],
            "the list is the INDEX, decorated — not the daemon's answer filtered"
        );
        // FORCED, the way that matters most: a daemon row with no header is
        // not on the list at ANY position, however recently it was touched.
        assert!(
            !l.rows.iter().any(|r| r.id == "arena"),
            "a process-generated thread reached the list"
        );
        // The decoration landed on the one the daemon knows…
        let mine = &l.rows[0];
        assert!(
            mine.known,
            "the daemon knows `mine` and the row must say so"
        );
        // …and the one it does not know is KEPT, its state BLANK (V1.9: never
        // `absent`). Daemon absence means unadopted, not un-owned; dropping it
        // would silently delete his own history.
        let unadopted = &l.rows[1];
        assert!(!unadopted.known);
        assert_eq!(unadopted.state, "");
        // V1.9's opposition: the row the daemon DOES know carries a word, so
        // the blank above is the absence and not a column that never fills.
        assert!(!mine.state.is_empty(), "the known row's state: {:?}", mine.state);
        assert!(
            matches!(unadopted.queued, Known::Unknown),
            "a row the daemon does not know must not render a zero"
        );
    }

    /// **V1.9 G3 — NO THREAD ROW SAYS `absent`** (consumer §8, TC-48). A
    /// listed thread the daemon has not answered for yet draws `…`; one it
    /// answered WITHOUT (an unadopted index thread, `absence_is_a_fact()`)
    /// draws a BLANK state column; one it reports `idle` draws `idle` — the
    /// opposition, so the blank is the absence and not a column that never
    /// fills. Asserted on the model AND on the drawn frame at 120 columns.
    #[test]
    fn v19_g3_a_thread_row_never_says_absent_blank_or_loading_instead() {
        fn drawn(s: &Store) -> Vec<String> {
            let vs = tui_render::ViewState { mode: Mode::List, list: thread_list(s, 0, 0), ..Default::default() };
            tui_render::frame(&vs, now(), 120, 24).iter().map(|r| r.spans.iter().map(|sp| sp.content.as_ref()).collect()).collect()
        }
        fn line<'a>(text: &'a [String], needle: &str) -> &'a String {
            text.iter().find(|t| t.contains(needle)).unwrap_or_else(|| panic!("no row says {needle}: {text:#?}"))
        }
        const KNOWN: &str = "the daemon knows this one";
        const UNADOPTED: &str = "an unadopted one";
        let mut s = store();
        s.adopt_projects(vec![project("harness", &[("mine", KNOWN, false), ("unadopted", UNADOPTED, false)])]);
        s.enter_project(0);

        // Never subscribed: no status page has arrived, so nothing is a fact.
        assert!(!s.absence_is_a_fact());
        let l = thread_list(&s, 0, 0);
        assert_eq!(l.rows.iter().map(|r| r.state.as_str()).collect::<Vec<_>>(), ["…", "…"], "not told yet is `…`");
        let text = drawn(&s);
        // The summary is drawn WHOLE, so the `…` on its row is the state
        // word and not a crop's ellipsis.
        let row = line(&text, UNADOPTED);
        assert!(row.contains(&format!("{UNADOPTED} ")) && row.contains('…'), "{row:?}");
        assert!(!text.iter().any(|t| t.contains("absent")), "{text:#?}");

        // The daemon's status page arrives, whole: it reports `mine` idle
        // and has no row at all for `unadopted`.
        s.apply(
            TransportEvent::Fleet { statuses: vec![json!({"thread_id": "mine", "state": "idle", "executing": false})], capped: false },
            now(),
        );
        assert!(s.absence_is_a_fact(), "the fixture must reach the absence branch");
        let l = thread_list(&s, 0, 0);
        let st = |id: &str| l.rows.iter().find(|r| r.id == id).unwrap().state.clone();
        assert_eq!(st("unadopted"), "", "a thread the daemon has no row for draws blank, never `absent`");
        assert_eq!(st("mine"), "idle", "opposition: the column CAN show a word");
        let text = drawn(&s);
        assert!(line(&text, KNOWN).contains("idle"), "{text:#?}");
        let row = line(&text, UNADOPTED);
        assert!(!row.contains("absent") && !row.contains('…') && !row.contains("idle"), "the unadopted row's state is blank: {row:?}");
        assert!(!text.iter().any(|t| t.contains("absent")), "{text:#?}");
    }

    /// The event lane is the same question one view over: his threads' events,
    /// not the daemon's firehose.
    #[test]
    fn the_event_lane_is_scoped_by_the_same_predicate_the_list_is() {
        let mut s = store();
        s.adopt_projects(vec![project("harness", &[("mine", "his", false)])]);
        s.apply(
            ev(
                "mine",
                1,
                json!({"kind":"assistant_text","text":"his own work"}),
            ),
            now(),
        );
        s.apply(
            ev(
                "arena",
                1,
                json!({"kind":"assistant_text","text":"somebody else's"}),
            ),
            now(),
        );
        let t = texts(&activity_blocks(&s, false, false));
        assert!(t.iter().any(|x| x.contains("his own work")), "{t:?}");
        assert!(
            !t.iter().any(|x| x.contains("somebody else's")),
            "the lane is showing threads he does not own: {t:?}"
        );
        // …and the counters agree with the rows, rather than summing the fleet.
        s.apply(
            TransportEvent::Fleet {
                statuses: vec![
                    json!({"thread_id":"mine","queued":2,"unread":1,"executing":true}),
                    json!({"thread_id":"arena","queued":40,"unread":900,"executing":true}),
                ],
                capped: false,
            },
            now(),
        );
        assert!(
            matches!(s.queued_owned(), Known::Known(2)),
            "the badge counted somebody else's queue: {:?}",
            s.queued_owned()
        );
        assert!(matches!(s.active_threads(), Known::Known(1)));
        assert!(matches!(s.unread_elsewhere(None), Known::Known(1)));
    }

    /// Pins come out of the index he already reads, and the AUI's file is the
    /// only registry — the daemon has no concept of a pin at all.
    #[test]
    fn pins_are_read_out_of_the_index_and_marked_where_the_row_stands() {
        let mut s = store();
        s.adopt_projects(vec![project(
            "harness",
            &[("a", "unpinned", false), ("b", "pinned", true)],
        )]);
        s.enter_project(0);
        let l = thread_list(&s, 0, 0);
        assert!(!l.rows[0].pinned);
        assert!(l.rows[1].pinned, "the pin did not survive the projection");
        // A pin is NOT a re-order: `DIR-3`'s one direction survives it, and the
        // row is where he remembers leaving it.
        assert_eq!(l.rows[1].id, "b", "the pinned row was hoisted");
        let ps = project_list(&s, 0, 0, None);
        assert_eq!(ps.rows[0].pinned, 1, "the root menu must total the pins");
    }

    /// **THE ROOT MENU, AND THE ONE DISTINCTION THAT HAD TO SURVIVE HIS ASK.**
    ///
    /// The old name of this test was `…_an_indexless_one_says_so_rather_than_
    /// reading_empty`, and its assertion message is the contract in one line:
    /// *"an unreadable index must not read as a project with no threads."* It
    /// was, until `0072`, **the only automated defence of that distinction
    /// anywhere in the system.**
    ///
    /// The operator asked for the SENTENCE to go — *"Don't tell me no index"* —
    /// and a scout measured that every one of his twenty indexless roots is a
    /// directory he has never held a thread in, so `0 thr` is the true answer
    /// for all twenty. But the CORRUPT case is not one of those twenty and must
    /// not be absorbed into them: `aui-nav`'s `ThreadIndex::save` writes an
    /// empty index back over an unparseable one as a whole-file write, so a
    /// corrupt `threads.json` plus one turn permanently replaces his header
    /// list, and this row is the only surface that would show it.
    ///
    /// So the guard is REWRITTEN, not deleted: the distinction moved from a
    /// sentence into `Known<usize>`, and this asserts the new shape.
    ///
    /// FORCED RED by making the corrupt case `Known::Known(0)` in
    /// `project_list` — the unreadable project then renders `0 thr` and is
    /// indistinguishable from an empty one, which is the defect.
    #[test]
    fn an_unreadable_index_is_unknown_and_an_absent_one_is_an_honest_zero() {
        let mut s = store();
        let mut corrupt = project("chess", &[]);
        corrupt.index = crate::index::IndexState::Corrupt;
        let mut absent = project("plan", &[]);
        absent.index = crate::index::IndexState::Absent;
        s.adopt_projects(vec![
            project("harness", &[("a", "x", false)]),
            corrupt,
            absent,
        ]);
        let l = project_list(&s, 0, 0, None);
        assert_eq!(l.rows.len(), 3);
        assert!(l.loaded);

        // A project that was READ says the number it read.
        assert_eq!(l.rows[0].threads, Known::Known(1));
        assert!(!l.rows[0].unreadable_index);

        // **THE CORRUPT ONE IS UNKNOWN, NOT ZERO** — the contract, in the type.
        assert_eq!(
            l.rows[1].threads,
            Known::Unknown,
            "an unreadable index must not read as a project with no threads"
        );
        assert!(l.rows[1].unreadable_index);

        // **AND THE ABSENT ONE IS AN HONEST ZERO**, which is the half his ask
        // added: twenty of his roots are exactly this, and `0 thr` is true.
        assert_eq!(l.rows[2].threads, Known::Known(0));
        assert!(!l.rows[2].unreadable_index);
        assert_ne!(
            l.rows[1].threads, l.rows[2].threads,
            "corrupt and absent collapsed back into one rendering"
        );

        // **AND THE BANNED SENTENCE IS NOWHERE ON THE FRAME.** A POSITIVE
        // WITNESS FIRST — the corrupt row is drawn, and says `?` — so the
        // absence below means "the words went" and not "the row went".
        let vs = tui_render::ViewState {
            mode: Mode::Projects,
            projects: l.clone(),
            ..Default::default()
        };
        let drawn = tui_render::frame(&vs, now(), 100, 24);
        let text: Vec<String> = drawn
            .iter()
            .map(|r| r.spans.iter().map(|sp| sp.content.as_ref()).collect())
            .collect();
        assert!(
            text.iter()
                .any(|t: &String| t.contains("chess") && t.contains("? thr")),
            "the unreadable project is not drawn as `? thr`: {text:#?}"
        );
        assert!(
            text.iter()
                .any(|t: &String| t.contains("plan") && t.contains("0 thr")),
            "the absent-index project is not drawn as `0 thr`: {text:#?}"
        );
        assert!(
            !text.iter().any(|t: &String| t.contains("no index")),
            "the words he asked to be rid of are still on the frame: {text:#?}"
        );

        // Never told, so never a zero.
        assert!(matches!(l.rows[0].active, Known::Unknown));
    }

    /// **THE ROOT HEADER, CUT TO WHAT HE ASKED TO SEE — and the queue count
    /// that did NOT go with it.**
    ///
    /// FORCED RED by putting `{n} under ~/minds` back into the title, by
    /// dropping the `queued` part from `render::header_line`, and by letting
    /// `0 queued` be drawn.
    #[test]
    fn the_root_header_is_three_things_and_the_queue_count_is_still_one_of_them() {
        let mut s = store();
        s.adopt_projects(vec![project("harness", &[("a", "x", false)])]);
        s.apply(
            TransportEvent::Fleet {
                statuses: vec![json!({"thread_id":"a","queued":3,"unread":12,"executing":true})],
                capped: false,
            },
            now(),
        );
        let h = header(&s, Mode::Projects);
        // **WHAT HE ASKED FOR**: the unread count and an active count.
        assert_eq!(h.title, "12 unread · 1 active", "{}", h.title);
        // **WHAT HE ASKED TO LOSE.**
        for gone in ["under ~/minds", "with an index", "thread", "elsewhere"] {
            assert!(!h.title.contains(gone), "`{gone}` survived: {}", h.title);
        }

        let vs = tui_render::ViewState {
            mode: Mode::Projects,
            header: h,
            projects: project_list(&s, 0, 0, None),
            ..Default::default()
        };
        let row: String = tui_render::frame(&vs, now(), 100, 24)[0]
            .spans
            .iter()
            .map(|sp| sp.content.as_ref())
            .collect();

        // **THE QUEUE COUNT IS STILL THERE, IN WORDS.** `DIR-6` says it is
        // visible AT ALL TIMES; he said "I don't know what ▲1 means so let's
        // drop it. users won't get it." That is a LEGIBILITY complaint about a
        // glyph, not a retraction of his own directive — so the glyph went and
        // the count stayed. **A decision he may overturn in one beat.**
        assert!(
            row.contains("3 queued"),
            "the queue count left the header: {row:?}"
        );
        assert!(
            !row.contains('▲'),
            "the glyph he could not read is still drawn: {row:?}"
        );
        assert!(row.contains("12 unread"), "{row:?}");
        assert!(row.contains("1 active"), "{row:?}");
        for gone in ["unread elsewhere", "acct", "across his threads"] {
            assert!(!row.contains(gone), "`{gone}` survived the cut: {row:?}");
        }
    }

    /// **`0 queued` IS NEVER DRAWN**, and `? queued` still is.
    ///
    /// The word costs more room than the glyph did, so a quiet queue spends
    /// nothing — but an UNKNOWN queue still speaks, because a silent absence
    /// and a known zero are different facts and honesty rule 1 is about exactly
    /// that difference.
    #[test]
    fn a_quiet_queue_says_nothing_and_an_unknown_one_still_speaks() {
        let quiet = {
            let mut s = store();
            s.adopt_projects(vec![project("harness", &[("a", "x", false)])]);
            s.apply(
                TransportEvent::Fleet {
                    statuses: vec![json!({"thread_id":"a","queued":0,"unread":0})],
                    capped: false,
                },
                now(),
            );
            s
        };
        let row = |s: &Store| -> String {
            let vs = tui_render::ViewState {
                mode: Mode::Projects,
                header: header(s, Mode::Projects),
                projects: project_list(s, 0, 0, None),
                ..Default::default()
            };
            tui_render::frame(&vs, now(), 100, 24)[0]
                .spans
                .iter()
                .map(|sp| sp.content.as_ref())
                .collect()
        };
        let r = row(&quiet);
        assert!(!r.contains("queued"), "a quiet queue spent a word: {r:?}");
        // THE POSITIVE WITNESS: the row is a real header, so the absence above
        // means "no queue part" and not "no header".
        assert!(
            r.contains("unread"),
            "the header did not draw at all: {r:?}"
        );

        // The daemon has said nothing at all: `? queued`, never silence.
        let mut untold = store();
        untold.adopt_projects(vec![project("harness", &[("a", "x", false)])]);
        let r = row(&untold);
        assert!(
            r.contains("? queued"),
            "an unknown queue went silent: {r:?}"
        );
    }

    #[test]
    fn a_disconnect_greys_the_meters_and_keeps_the_transcript() {
        let mut s = store();
        s.apply(fleet(2, 5), now());
        opened(&mut s, "t1");
        s.apply(
            ev(
                "t1",
                1,
                json!({"kind":"assistant_text","text":"history stays true"}),
            ),
            now(),
        );
        assert!(matches!(s.queued(Some("t1")), Known::Known(2)));

        s.apply(
            TransportEvent::Disconnected {
                reason: "the daemon closed the socket".into(),
                next_retry_in_ms: 2000,
            },
            now(),
        );

        // THE HONESTY RULE, both halves in one assertion pair.
        assert!(
            matches!(s.queued(Some("t1")), Known::Unknown),
            "a current-state meter must not survive the link that justified it"
        );
        let bs = thread_blocks(&s, false, false);
        assert!(
            texts(&bs).iter().any(|t| t.contains("history stays true")),
            "the transcript is history and stays: {:?}",
            texts(&bs)
        );
        // …and the banner says which it is. **The two NUMBERS are not in this
        // string on purpose** — see `banner`: a duration formatted here would
        // be frozen at the projection that made it. What the store owes is the
        // two INSTANTS the renderer measures against, and they are asserted
        // below in `the_face_knows_when_the_link_went_down_and_when_it_retries`.
        let b = banner(&s, tui_render::Mode::Thread(tui_render::Region::Composer)).expect("a down link must be announced");
        assert!(b.text.contains("HISTORY"), "{}", b.text);
        assert!(
            !b.text.contains("retry in"),
            "a countdown frozen into a projected string is the defect, not the feature: {}",
            b.text
        );
    }

    /// **THE VERY FIRST LAUNCH TEACHES, AND THE SECOND ONE DOES NOT** (`0040`
    /// §10).
    ///
    /// Three claims, because the band is defined as much by where it is ABSENT
    /// as by what it says: it is the ROOT's band (the keys it teaches are the
    /// root's keys), it appears exactly once in the life of a `~/.tui`
    /// directory (`crate::first_run` owns that half), and **the link outranks
    /// it** — there is one banner slot, and a face that cannot reach the daemon
    /// has something more urgent to say than how its arrow keys work.
    #[test]
    fn the_first_launch_teaches_the_grammar_and_nothing_else_does() {
        let mut s = store();
        assert!(
            banner(&s, tui_render::Mode::Projects).is_none(),
            "a face he has used before must not welcome him"
        );

        s.first_run = true;
        let b = banner(&s, tui_render::Mode::Projects).expect("the first run says so");
        assert_eq!(b.text, FIRST_RUN_HINT);
        // Every key it names is a key the root has. A teaching line that
        // teaches a key that is not there is worse than no teaching line.
        for k in ["→", "←", "?"] {
            assert!(b.text.contains(k), "the hint drops {k}: {}", b.text);
        }
        assert!(
            banner(&s, tui_render::Mode::Thread(tui_render::Region::Composer)).is_none(),
            "the root's grammar taught over a thread teaches the wrong keys"
        );

        // …and the link outranks it, in the one slot they share.
        s.apply(
            TransportEvent::Disconnected {
                reason: "the daemon closed the socket".into(),
                next_retry_in_ms: 2000,
            },
            now(),
        );
        let b = banner(&s, tui_render::Mode::Projects).expect("a down link must be announced");
        assert!(
            b.text.contains("not answering"),
            "a welcome banner buried the fact that the daemon is gone: {}",
            b.text
        );
    }

    /// **THE TWO INSTANTS THE FACE OWES THE RENDERER, AND THE LADDER THAT USED
    /// TO DESTROY ONE OF THEM.**
    ///
    /// `down_since` is the fix for the defect `0059` names: nothing anywhere
    /// tracked how long the link had been down, so the only thing the banner
    /// could say about time was a duration snapshotted at the drop. The shell's
    /// whole job now is to hold the two INSTANTS — when it went down, and when
    /// the next attempt is due — and let the renderer measure them against the
    /// frame's clock.
    ///
    /// **The subtle half is the ladder.** `transport::supervise` emits
    /// `Disconnected` once per failed attempt, not once per outage, with a
    /// `Connecting` between each pair. An outage clock that took the LATEST of
    /// those would read a few seconds forever — a fresh number that is just as
    /// false as a frozen one, and harder to spot. Every clock here is this
    /// test's own; nothing sleeps.
    #[test]
    fn the_face_knows_when_the_link_went_down_and_when_it_retries() {
        let mut s = store();
        let t0 = now();
        assert!(
            s.down_since.is_none(),
            "a face that has not dropped is not down"
        );

        s.apply(
            TransportEvent::Disconnected {
                reason: "the daemon closed the socket".into(),
                next_retry_in_ms: 2000,
            },
            t0,
        );
        assert_eq!(s.down_since, Some(t0), "the drop was not recorded");
        assert_eq!(
            s.link,
            LinkState::Disconnected {
                reason: "the daemon closed the socket".into(),
                retry_at: Some(t0 + TimeDelta::seconds(2)),
            },
            "the retry must be a DEADLINE; a duration is what went stale"
        );

        // …the ladder: attempt 2 goes out, and fails four minutes later with a
        // longer backoff. The outage is FOUR MINUTES OLD, not four seconds.
        let t1 = t0 + TimeDelta::minutes(4);
        s.apply(TransportEvent::Connecting { attempt: 2 }, t1);
        assert_eq!(
            s.down_since,
            Some(t0),
            "an attempt in flight restarted the outage clock"
        );
        s.apply(
            TransportEvent::Disconnected {
                reason: "connection refused".into(),
                next_retry_in_ms: 8000,
            },
            t1,
        );
        assert_eq!(
            s.down_since,
            Some(t0),
            "the SECOND failure restarted the clock — the face would say 'down 0s' forever"
        );
        assert_eq!(
            s.link,
            LinkState::Disconnected {
                reason: "connection refused".into(),
                retry_at: Some(t1 + TimeDelta::seconds(8)),
            },
            "the new backoff must move the deadline"
        );

        // The renderer's reading, at a clock this test owns: the projection
        // hands over the instant, and the age is derived from it per frame.
        let link = header(&s, tui_render::Mode::Thread(tui_render::Region::Composer)).link;
        assert_eq!(
            link.down_since,
            Some(t0),
            "the projection dropped the instant"
        );
        assert_eq!(
            link.down_for(t0 + TimeDelta::minutes(9))
                .map(|d| d.num_seconds()),
            Some(540),
            "nine minutes down must read as nine minutes down"
        );

        // …and only a connection that comes ALL THE WAY UP ends the outage.
        s.apply(
            TransportEvent::Connected {
                boot_id: "b1".into(),
                resync: crate::transport::ResyncKind::First,
            },
            t1,
        );
        assert!(
            s.down_since.is_none(),
            "the outage outlived the link coming back"
        );
    }

    /// **HIS RULE: NO ROWS THAT SAY UNKNOWN** — and `DIR-4`'s breadth survives
    /// it, through `x` rather than through a row.
    ///
    /// This test replaces `an_unknown_event_kind_renders_rather_than_vanishing`
    /// and it is the same promise read the other way round. That test proved
    /// the payload reached the SCREEN; this one proves it reaches the screen
    /// WHEN ASKED, and that it does not squat on a line in the meantime. The
    /// thing neither version tolerates is the payload being lost.
    #[test]
    fn a_kind_nothing_recognises_renders_nothing_by_default_and_its_raw_self_under_x() {
        let mut s = store();
        opened(&mut s, "t1");
        s.apply(
            ev("t1", 1, json!({"kind": "neutrino_detected", "energy": 42})),
            now(),
        );
        let closed = texts(&thread_blocks(&s, false, false)).join("\n");
        assert!(
            !closed.contains("neutrino_detected"),
            "a row he cannot act on took a line: {closed}"
        );
        let open = texts(&thread_blocks(&s, false, true)).join("\n");
        assert!(
            open.contains("unknown event kind neutrino_detected"),
            "{open}"
        );
        assert!(
            open.contains("42"),
            "the payload must still be reachable: {open}"
        );
    }

    /// The same, for a kind the DAEMON documents and this build has not
    /// specialized. It is a different fact — a gap in this client rather than a
    /// daemon that moved — and `x` still tells the two apart by their wording.
    #[test]
    fn a_daemon_kind_we_have_not_specialized_is_silent_and_says_which_it_is_under_x() {
        let mut s = store();
        opened(&mut s, "t1");
        s.apply(
            ev(
                "t1",
                1,
                json!({"kind": "agent_listing_delta", "raw": {"a": 1}}),
            ),
            now(),
        );
        assert!(
            thread_blocks(&s, false, false).is_empty(),
            "{:?}",
            texts(&thread_blocks(&s, false, false))
        );
        let open = texts(&thread_blocks(&s, false, true)).join("\n");
        assert!(open.contains("agent_listing_delta"), "{open}");
        assert!(!open.contains("unknown event kind"), "{open}");
    }

    /// **A SHAPE THAT DRIFTED STILL SHOUTS.** The one `Untyped` case that keeps
    /// its row, and it must: a daemon and a client that disagree about a
    /// payload is a defect in one of them, and silence is how it would ship.
    ///
    /// FORCED: with the `why.is_some()` arm made Silent like its two
    /// neighbours, this goes red — which is the whole point of the three cases
    /// being three.
    #[test]
    fn a_kind_we_do_type_whose_body_did_not_fit_is_never_silent() {
        let mut s = store();
        opened(&mut s, "t1");
        // `turn_started` is typed and wants `attempt: u32`. A string is a shape
        // this build cannot read, which is exactly the case in question.
        s.apply(
            ev("t1", 1, json!({"kind": "turn_started", "attempt": "lots"})),
            now(),
        );
        let t = texts(&thread_blocks(&s, false, false)).join("\n");
        assert!(
            t.contains("could not read this daemon's shape"),
            "a payload mismatch must never be quiet: {t}"
        );
    }

    /// `DEC-12`, and the promise the `×n` stamp makes.
    #[test]
    fn task_progress_folds_to_one_live_row_and_expands_to_every_one_of_them() {
        let mut s = store();
        opened(&mut s, "t1");
        for i in 1..=5 {
            s.apply(
                ev(
                    "t1",
                    i,
                    json!({"kind":"task_progress","task_id":"agent-a","description":format!("step {i}")}),
                ),
                now(),
            );
        }
        s.apply(
            ev(
                "t1",
                6,
                json!({"kind":"task_progress","task_id":"agent-b","description":"other"}),
            ),
            now(),
        );

        let folded = thread_blocks(&s, false, false);
        assert_eq!(
            folded.len(),
            2,
            "one live row per task_id: {:?}",
            texts(&folded)
        );
        let a = texts(&folded).join("\n");
        assert!(a.contains("step 5"), "the live row shows the NEWEST: {a}");
        assert!(
            !a.contains("step 4"),
            "a superseded step must not also be a row: {a}"
        );
        assert!(
            matches!(&folded[0], Block::Lane { count: Some(5), .. }),
            "the count stamp is the promise nothing was dropped: {:?}",
            texts(&folded)
        );

        // NOTHING WAS DROPPED — `x` proves it.
        let all = thread_blocks(&s, false, true);
        assert_eq!(all.len(), 6, "expanding must show every retained event");
        assert!(texts(&all).iter().any(|t| t.contains("step 1")));
    }

    /// **THE RETRACTION, AS THE ONE ASSERTION IT IS ABOUT.** *"We don't need
    /// stream chunks rendered."*
    ///
    /// This replaces `a_run_of_stream_events_folds_and_a_different_kind_breaks_
    /// the_run`, whose subject — how a run of chunk ROWS coalesces — stopped
    /// existing when the rows did. What it proves instead is the pair of facts
    /// that had to stay true through that change: no chunk row by default, and
    /// every chunk still reachable.
    ///
    /// FORCED: remove the `Shape::Silent` arm for `StreamEvent` and the first
    /// assertion goes red with three `stream · {…}` rows; remove the
    /// `if !open { continue }` in `project`'s Silent arm and the second goes
    /// red instead, because the raw rows would never have been retained-only.
    #[test]
    fn stream_chunks_render_no_row_by_default_and_every_one_of_them_under_x() {
        let mut s = store();
        opened(&mut s, "t1");
        for i in 1..=3 {
            s.apply(
                ev("t1", i, json!({"kind":"stream_event","raw":{"i":i}})),
                now(),
            );
        }
        s.apply(
            ev("t1", 4, json!({"kind":"turn_ended","exit_code":0})),
            now(),
        );

        let closed = texts(&thread_blocks(&s, false, false));
        assert!(
            !closed.iter().any(|t| t.starts_with("stream")),
            "a stream chunk reached the default face: {closed:?}"
        );
        let open = texts(&thread_blocks(&s, false, true));
        assert_eq!(
            open.iter().filter(|t| t.starts_with("stream")).count(),
            3,
            "`x` must still reach every retained chunk: {open:?}"
        );
    }

    /// **THE PER-MESSAGE USAGE ROW IS GONE** (`0047` D3), and the number it
    /// showed is named for what it was wherever it can still be seen.
    ///
    /// FORCED: restore the old `Shape::Lane` for `MessageUsage` and the first
    /// assertion goes red immediately.
    #[test]
    fn the_per_message_usage_row_is_deleted_and_x_says_why() {
        let mut s = store();
        opened(&mut s, "t1");
        s.apply(
            ev(
                "t1",
                1,
                json!({"kind":"message_usage","model":"opus","input_tokens":2,
                       "output_tokens":4,"cache_read_tokens":168606}),
            ),
            now(),
        );
        let closed = texts(&thread_blocks(&s, false, false)).join("\n");
        assert!(
            !closed.contains("out 4"),
            "his exact complaint is back on screen: {closed}"
        );
        let open = texts(&thread_blocks(&s, false, true)).join("\n");
        assert!(
            open.contains("message_start placeholder"),
            "the retained row must say what the number actually is: {open}"
        );
    }

    /// **TWO ADJACENT FOLDABLE FACTS ARE TWO RUNS**, and since `0051` the fold
    /// key is finer than the wire kind — because the one kind that still folds
    /// by run (`unknown`) carries nine different facts, and a run of rate-limit
    /// reports interrupted by an api_retry is two runs and not one.
    ///
    /// FORCED: make `fold_key` return `ev.kind()` and this goes red with one
    /// `×3` row standing for two unrelated things.
    #[test]
    fn a_run_is_keyed_on_the_fact_and_not_on_the_wire_kind() {
        let mut s = store();
        opened(&mut s, "t1");
        let limits = |u: f64| {
            json!({"kind":"unknown","raw":{"type":"rate_limit_event","rate_limit_info":{
                "unifiedWindows":{"five_hour":{"utilization":u},"seven_day":{"utilization":0.16}}}}})
        };
        s.apply(ev("t1", 1, limits(0.1)), now());
        s.apply(ev("t1", 2, limits(0.11)), now());
        s.apply(
            ev(
                "t1",
                3,
                json!({"kind":"unknown","raw":{"type":"system","subtype":"api_retry",
                       "attempt":7,"max_retries":10,"retry_delay_ms":38882}}),
            ),
            now(),
        );
        s.apply(ev("t1", 4, limits(0.12)), now());

        let bs = thread_blocks(&s, false, false);
        let t = texts(&bs);
        assert_eq!(bs.len(), 3, "run · break · run: {t:?}");
        assert!(
            matches!(&bs[0], Block::Lane { count: Some(2), .. }),
            "{t:?}"
        );
        assert!(t[1].contains("api retry"), "{t:?}");
        assert!(
            matches!(&bs[2], Block::Lane { count: None, .. }),
            "a run of one is not ×1"
        );
    }

    /// **THE STDOUT-ONLY TOGGLE REMOVES WHAT WAS NOT SAID AND NOTHING ELSE.**
    ///
    /// This is the same test slot that held
    /// `a_hidden_lane_removes_its_rows_and_nothing_else` when there were nine
    /// lanes and a digit each. There is one boolean now, so the property is
    /// narrower and sharper: the boxes stay, everything else goes, and the
    /// default (`false`) touches nothing at all.
    ///
    /// FORCED: make `project`'s guard `if stdout_only` without the
    /// `!is_stdout(...)` half and the second assertion goes red — every row
    /// vanishes including his words. Make it `if false && …` (the toggle
    /// disarmed) and the third goes red. Make `is_stdout` return `true` for
    /// `thinking` and the fourth does.
    #[test]
    fn the_stdout_toggle_keeps_the_boxes_and_drops_the_rest() {
        let mut s = store();
        opened(&mut s, "t1");
        s.apply(
            ev("t1", 1, json!({"kind":"assistant_text","text":"kept"})),
            now(),
        );
        s.apply(
            ev("t1", 2, json!({"kind":"thinking","text":"reasoning"})),
            now(),
        );
        s.apply(
            ev(
                "t1",
                3,
                json!({"kind":"tool_use","id":"tu1","name":"Read","input":{}}),
            ),
            now(),
        );

        // THE DEFAULT: the toggle off drops nothing.
        let all = thread_blocks(&s, false, false);
        assert_eq!(all.len(), 3, "{:?}", texts(&all));

        // THE TOGGLE: only what was said survives.
        let said = thread_blocks(&s, true, false);
        assert_eq!(said.len(), 1, "{:?}", texts(&said));
        assert!(
            texts(&said).iter().any(|t| t.contains("kept")),
            "his words did not survive the collapse: {:?}",
            texts(&said)
        );
        assert!(
            !texts(&said).iter().any(|t| t.contains("reasoning")),
            "thinking survived a stdout-only view: {:?}",
            texts(&said)
        );
        // **AND A USER MESSAGE AND AN INJECTED SKILL BODY ARE BOTH STDOUT.**
        // `context_injected` was `Lane::Speech` and stays in, on `0047`
        // D5/D7's reasoning: a skill body loaded into the turn is CONTENT.
        let mut u = store();
        opened(&mut u, "t1");
        u.apply(
            ev("t1", 1, json!({"kind":"user_message","text":"mine"})),
            now(),
        );
        u.apply(
            ev("t1", 2, json!({"kind":"context_injected","text":"a skill body"})),
            now(),
        );
        u.apply(
            ev("t1", 3, json!({"kind":"settings_changed"})),
            now(),
        );
        let t = texts(&thread_blocks(&u, true, false)).join("\n");
        assert!(t.contains("mine"), "{t}");
        assert!(!t.contains("settings"), "a system row survived: {t}");
    }

    // ────────────────────────────────────────────────────────────────────
    //  LIVE ASSEMBLY — the `0048` design, proved against the real anatomy
    //
    //  The fixtures below are the seq window 18963–18981 of his own thread,
    //  transcribed out of `0047`'s prose: message_start, a thinking block,
    //  a message_usage BEFORE the text, the text deltas, the authoritative
    //  `assistant_text`, a SECOND identical message_usage, and the stops.
    //  Using the real order matters — the `message_usage` sitting between the
    //  deltas and the complete line is exactly the kind of interleaving a fold
    //  written against a tidy fixture would get wrong.
    // ────────────────────────────────────────────────────────────────────

    fn sse(kind: &str, body: Value) -> Value {
        let mut ev = body;
        ev["type"] = json!(kind);
        json!({"kind": "stream_event", "raw": {"event": ev}})
    }

    fn delta(index: i64, ty: &str, field: &str, text: &str) -> Value {
        sse(
            "content_block_delta",
            json!({"index": index, "delta": {"type": ty, field: text}}),
        )
    }

    /// The assembled box GROWS, row count steady, and it sits at the bottom.
    ///
    /// FORCED THREE WAYS: drop the `content_block_delta` arm of `feed` and the
    /// box never appears; drop `place_all`'s push and it appears with no text;
    /// make `place_all` always push instead of updating in place and the block
    /// count climbs with the deltas instead of holding at one.
    #[test]
    fn a_message_assembles_live_from_its_deltas_and_stays_one_row() {
        let mut s = store();
        opened(&mut s, "t1");
        s.apply(
            ev(
                "t1",
                1,
                sse("message_start", json!({"message": {"id": "msg_01"}})),
            ),
            now(),
        );
        s.apply(
            ev(
                "t1",
                2,
                sse(
                    "content_block_start",
                    json!({"index": 0, "content_block": {"type": "text"}}),
                ),
            ),
            now(),
        );
        s.apply(
            ev(
                "t1",
                3,
                delta(0, "text_delta", "text", "The memory is repaired"),
            ),
            now(),
        );
        let one = thread_blocks(&s, false, false);
        assert_eq!(one.len(), 1, "{:?}", texts(&one));
        assert_eq!(one[0].text(), "The memory is repaired");
        assert!(
            one[0].is_message(),
            "assembled speech must be a BOX, not a lane row"
        );

        s.apply(
            ev("t1", 4, delta(0, "text_delta", "text", " and clean.")),
            now(),
        );
        let two = thread_blocks(&s, false, false);
        assert_eq!(
            two.len(),
            1,
            "a second delta made a second row: {:?}",
            texts(&two)
        );
        assert_eq!(two[0].text(), "The memory is repaired and clean.");
    }

    /// **THE HEADER SAYS `writing`, IN WORDS**, and it stops saying it the
    /// moment the message is whole.
    ///
    /// FORCED: remove the `note` from `live_block` and the first assertion goes
    /// red; leave the note on after retirement and the second does.
    #[test]
    fn the_growing_box_says_writing_and_the_finished_one_says_nothing() {
        let mut s = store();
        opened(&mut s, "t1");
        s.apply(
            ev(
                "t1",
                1,
                sse(
                    "content_block_start",
                    json!({"index":0,"content_block":{"type":"text"}}),
                ),
            ),
            now(),
        );
        s.apply(
            ev("t1", 2, delta(0, "text_delta", "text", "half a th")),
            now(),
        );
        let live = thread_blocks(&s, false, false);
        assert!(
            matches!(&live[0], Block::Message { note: Some(n), .. } if n == "writing"),
            "{:?}",
            live[0]
        );
        s.apply(
            ev(
                "t1",
                3,
                json!({"kind":"assistant_text","text":"half a thought, whole"}),
            ),
            now(),
        );
        let done = thread_blocks(&s, false, false);
        assert!(
            matches!(&done[0], Block::Message { note: None, .. }),
            "{:?}",
            done[0]
        );
    }

    /// **THE LEDGER REPLACES THE ASSEMBLY, AND THE BOX DOES NOT JUMP.**
    ///
    /// The whole anatomy, in its real order. One box before, one box after, at
    /// the same position, carrying the ledger's text — and the `message_usage`
    /// that sits in the middle of it contributes no row at all.
    ///
    /// FORCED: delete the `AssistantText` arm of `retire_for` and the same
    /// words appear TWICE, which is the defect this arm exists to prevent and
    /// the one `0048` named first.
    #[test]
    fn the_authoritative_line_replaces_the_assembly_in_place_without_a_jump() {
        let mut s = store();
        opened(&mut s, "t1");
        let words = "The memory is repaired and clean — waiting on you.";
        let anatomy = vec![
            (
                18963,
                sse("message_start", json!({"message":{"id":"msg_01"}})),
            ),
            (
                18964,
                sse(
                    "content_block_start",
                    json!({"index":0,"content_block":{"type":"thinking"}}),
                ),
            ),
            (18966, delta(0, "thinking_delta", "thinking", "weighing it")),
            (18970, json!({"kind":"thinking","text":""})),
            (
                18971,
                json!({"kind":"message_usage","model":"opus","input_tokens":2,
                           "output_tokens":3,"cache_read_tokens":160020}),
            ),
            (18972, sse("content_block_stop", json!({"index":0}))),
            (
                18973,
                sse(
                    "content_block_start",
                    json!({"index":1,"content_block":{"type":"text"}}),
                ),
            ),
            (
                18974,
                delta(1, "text_delta", "text", "The memory is repaired and clean"),
            ),
            (18975, delta(1, "text_delta", "text", " — waiting on you.")),
        ];
        for (seq, e) in anatomy {
            s.apply(ev("t1", seq, e), now());
        }
        let before = thread_blocks(&s, false, false);
        let box_at = before
            .iter()
            .position(|b| b.is_message())
            .expect("no growing box");
        assert_eq!(before[box_at].text(), words, "{:?}", texts(&before));
        assert_eq!(
            box_at,
            before.len() - 1,
            "the growing box must ride at the FLOOR: {:?}",
            texts(&before)
        );

        // The authoritative line, then the SECOND identical usage row and the
        // stops — all three of which must change nothing he can see.
        s.apply(
            ev("t1", 18976, json!({"kind":"assistant_text","text":words})),
            now(),
        );
        s.apply(
            ev(
                "t1",
                18977,
                json!({"kind":"message_usage","model":"opus","input_tokens":2,
                                   "output_tokens":3,"cache_read_tokens":160020}),
            ),
            now(),
        );
        s.apply(
            ev("t1", 18978, sse("content_block_stop", json!({"index":1}))),
            now(),
        );
        s.apply(ev("t1", 18980, sse("message_stop", json!({}))), now());

        let after = thread_blocks(&s, false, false);
        assert_eq!(
            after.iter().filter(|b| b.is_message()).count(),
            1,
            "the message was rendered twice: {:?}",
            texts(&after)
        );
        let now_at = after.iter().position(|b| b.is_message()).expect("box gone");
        assert_eq!(
            now_at,
            box_at,
            "the box MOVED when it completed: {:?}",
            texts(&after)
        );
        assert_eq!(after.len(), before.len(), "{:?}", texts(&after));
        assert_eq!(after[now_at].text(), words);
        assert!(
            !texts(&after).iter().any(|t| t.contains("out 3")),
            "a usage row survived: {:?}",
            texts(&after)
        );
    }

    /// **AN INTERRUPTED REPLY KEEPS ITS WORDS AND SAYS SO, IN WORDS.**
    ///
    /// FORCED: delete the turn-terminal arm of `retire_for` and the box sits
    /// there saying `writing` forever — which is the second failure `0048` told
    /// the builder to force, and a face that lies about a dead turn.
    #[test]
    fn a_turn_that_dies_mid_reply_leaves_the_words_and_the_word_interrupted() {
        let mut s = store();
        opened(&mut s, "t1");
        s.apply(
            ev(
                "t1",
                1,
                sse(
                    "content_block_start",
                    json!({"index":0,"content_block":{"type":"text"}}),
                ),
            ),
            now(),
        );
        s.apply(
            ev("t1", 2, delta(0, "text_delta", "text", "I was saying that")),
            now(),
        );
        s.apply(
            ev(
                "t1",
                3,
                json!({"kind":"turn_failed","error_kind":"exhausted","message":"gave up"}),
            ),
            now(),
        );
        let bs = thread_blocks(&s, false, false);
        let b = bs
            .iter()
            .find(|b| b.is_message())
            .expect("the words were lost");
        assert_eq!(b.text(), "I was saying that");
        assert!(
            matches!(b, Block::Message { note: Some(n), .. } if n == "interrupted mid-reply"),
            "{b:?}"
        );
    }

    /// **A REPLAY THAT BEGAN MID-BLOCK IS NEVER PRESENTED AS A WHOLE MESSAGE.**
    ///
    /// The `OPEN_CAP`-eviction and cold-open-mid-turn case: a delta with no
    /// `content_block_start` in front of it. It opens implicitly and renders
    /// with a leading `…`, and the authoritative line then replaces it whole.
    ///
    /// FORCED: set `implicit: false` at the implicit-open site and a SUFFIX is
    /// silently presented as a complete reply.
    #[test]
    fn a_partial_seen_from_the_middle_renders_as_a_suffix_and_is_healed_by_the_ledger() {
        let mut s = store();
        opened(&mut s, "t1");
        // The delta's OWN text deliberately does not begin with an ellipsis:
        // the mark has to come from the projection knowing this is a suffix,
        // not from the payload happening to look like one.
        s.apply(
            ev("t1", 90, delta(0, "text_delta", "text", "the rest of it")),
            now(),
        );
        let partial = thread_blocks(&s, false, false);
        assert_eq!(
            partial[0].text(),
            "…the rest of it",
            "a suffix was presented as a whole message"
        );
        s.apply(
            ev(
                "t1",
                91,
                json!({"kind":"assistant_text","text":"all of it, from the start"}),
            ),
            now(),
        );
        let healed = thread_blocks(&s, false, false);
        assert_eq!(healed.len(), 1, "{:?}", texts(&healed));
        assert_eq!(healed[0].text(), "all of it, from the start");
    }

    /// **A SUBAGENT'S WORDS NEVER LAND IN THE MAIN BOX.**
    ///
    /// FORCED: drop the `parent_tool_use_id` guard at the top of `feed` and a
    /// subagent's deltas are folded into the thread's own reply — the fourth
    /// failure `0048` told the builder to force, and the one that reads
    /// perfectly while being completely wrong.
    #[test]
    fn a_sidechain_delta_is_not_folded_into_the_threads_own_reply() {
        let mut s = store();
        opened(&mut s, "t1");
        s.apply(
            ev(
                "t1",
                1,
                sse(
                    "content_block_start",
                    json!({"index":0,"content_block":{"type":"text"}}),
                ),
            ),
            now(),
        );
        s.apply(ev("t1", 2, delta(0, "text_delta", "text", "mine.")), now());
        let mut side = delta(0, "text_delta", "text", " NOT MINE.");
        side["raw"]["parent_tool_use_id"] = json!("toolu_01");
        s.apply(ev("t1", 3, side), now());

        let bs = thread_blocks(&s, false, false);
        let b = bs.iter().find(|b| b.is_message()).expect("no box");
        assert_eq!(
            b.text(),
            "mine.",
            "a subagent's words reached the thread's box"
        );
    }

    /// **THE LIVE THINKING ROW** — the face's longest silence, ended. The
    /// `thinking` ledger event lands only when the block COMPLETES, so without
    /// this the transcript shows nothing for the minutes he most wants it to
    /// look alive.
    ///
    /// FORCED: remove `Part::Thought` from `feed`'s delta arm and the first
    /// assertion goes red; make the empty-`thinking` arm of `retire_for` hole
    /// the row instead of sealing it and the second does — losing the reasoning
    /// to a ledger row that carries no text at all.
    #[test]
    fn thinking_shows_while_it_is_happening_and_an_empty_ledger_row_does_not_erase_it() {
        let mut s = store();
        opened(&mut s, "t1");
        s.apply(
            ev(
                "t1",
                1,
                sse(
                    "content_block_start",
                    json!({"index":0,"content_block":{"type":"thinking"}}),
                ),
            ),
            now(),
        );
        s.apply(
            ev(
                "t1",
                2,
                delta(0, "thinking_delta", "thinking", "a line\nand another"),
            ),
            now(),
        );
        let live = texts(&thread_blocks(&s, false, false)).join("\n");
        assert!(
            live.contains("thinking · writing · 2 line(s) so far"),
            "{live}"
        );

        // The daemon's `thinking` for this block carries NO TEXT — the real
        // case, straight out of `0047`'s seq window at 18970.
        s.apply(ev("t1", 3, json!({"kind":"thinking","text":""})), now());
        let after = texts(&thread_blocks(&s, false, false)).join("\n");
        assert!(
            after.contains("a line"),
            "an empty ledger row erased the reasoning it was supposed to describe: {after}"
        );
        assert!(
            !after.contains("writing"),
            "it is not still being written: {after}"
        );
    }

    /// **EVERY KIND `0047` MARKED EXPANDABLE ACTUALLY ANNOUNCES ITSELF AS
    /// EXPANDABLE** — and that is a claim about the AFFORDANCE, not about the
    /// text. `0047` D6 measured what happens when the two come apart: 92% of
    /// `unknown` payloads were under the clip cap, so the row said nothing
    /// about having more behind it, he clicked, and nothing happened.
    ///
    /// FORCED: compute `expandable` from `is_folded`'s trailing-ellipsis
    /// heuristic instead of from the two renderings, and `system_init` — whose
    /// collapsed row ends in a tool COUNT rather than an ellipsis — goes red.
    #[test]
    fn every_kind_marked_expandable_says_so_on_its_row() {
        let cases = vec![
            (
                "task_notification",
                json!({"kind":"task_notification","task_id":"h",
                "status":"completed","summary":"the agent's whole report body, ".repeat(20)}),
            ),
            (
                "merged tool row",
                json!({"kind":"tool_use","id":"tu","name":"Bash",
                "input":{"command":"git status","extra":"more than the gist shows"}}),
            ),
            (
                "system_init",
                json!({"kind":"system_init","model":"opus","cwd":"/x",
                "tools":["Read","Write","Bash"]}),
            ),
            (
                "context_injected",
                json!({"kind":"context_injected","text":"y".repeat(400)}),
            ),
            (
                "subagent said",
                json!({"kind":"assistant_text","text":"z".repeat(400),
                "parent_tool_use_id":"tu"}),
            ),
            (
                "injected user",
                json!({"kind":"unknown","raw":{"isSynthetic":true,"type":"user",
                "message":{"content":[{"text":"w".repeat(400)}]}}}),
            ),
        ];
        for (name, e) in cases {
            let mut s = store();
            opened(&mut s, "t1");
            s.apply(ev("t1", 1, e), now());
            let bs = thread_blocks(&s, false, false);
            assert_eq!(
                bs.len(),
                1,
                "`{name}` did not render one row: {:?}",
                texts(&bs)
            );
            assert!(
                bs[0].is_expandable(),
                "`{name}` has more behind it and does not say so: {:?}",
                bs[0].text()
            );
            // …and opening it actually changes what is on the row, which is the
            // half `0047` D6 found missing.
            let open = super::thread_blocks(
                &s,
                false,
                false,
                &Opened::from([("t1".to_string(), 1u64)]),
                &[],
                &TreeId::Parent,
                None,
            );
            assert_ne!(
                texts(&open)[0],
                texts(&bs)[0],
                "`{name}` announced an expansion that does nothing"
            );
        }
    }

    /// **THE HONEST USAGE ROW, AGAINST A REAL PAYLOAD** — the `result_turn`
    /// body transcribed out of `0047`'s prose, numbers and all.
    ///
    /// FORCED: return `String::new()` from `usage_words` and every number
    /// assertion goes red at once.
    #[test]
    fn the_turn_usage_row_reads_the_frame_that_carries_final_numbers() {
        let mut s = store();
        opened(&mut s, "t1");
        s.apply(
            ev(
                "t1",
                1,
                json!({"kind":"result_turn","subtype":"success","is_error":false,
                       "total_cost_usd":20.189937,"num_turns":7,"duration_ms":53650,
                       "stop_reason":"end_turn",
                       "model_usage":{
                         "claude-fable-5-1":{"inputTokens":228,"outputTokens":48189,
                           "cacheCreationInputTokens":422653,"cacheReadInputTokens":3060127,
                           "thinkingTokens":8372,"costUSD":8.45992425,"contextWindow":1000000},
                         "claude-opus-5":{"inputTokens":20,"outputTokens":6842,
                           "cacheCreationInputTokens":795588,"cacheReadInputTokens":7205966,
                           "thinkingTokens":1506,"costUSD":11.730013,"contextWindow":1000000}}}),
            ),
            now(),
        );
        let t = texts(&thread_blocks(&s, false, false)).join("\n");
        // **THE COLLAPSED ROW CARRIES ONLY WHAT IS TRUE OF THIS TURN.**
        assert_eq!(
            t, "turn done · success · 7 turn(s) · 53s · $20.19 · 2 model(s) this session",
            "{t}"
        );
        // **AND NOT ONE TOKEN COUNT**, because `model_usage` is the SESSION's
        // and putting it here would restate the same figure on every turn —
        // the defect this task deleted, one level up. See `usage_words`.
        assert!(
            !t.contains("48189") && !t.contains("6842"),
            "a session total is on a TURN row: {t}"
        );
        // **NO GLYPHS, WORDS** (`0040`), and no invented units.
        assert!(!t.contains('⚙') && !t.contains('◇'), "{t}");

        let open = texts(&thread_blocks(&s, false, true)).join("\n");
        assert!(
            open.contains("session so far"),
            "the breakdown must say what it is OF: {open}"
        );
        assert!(open.contains("claude-opus-5 out 6842"), "{open}");
        assert!(open.contains("claude-fable-5-1 out 48189"), "{open}");
        assert!(open.contains("cache read 7205966"), "{open}");
        assert!(
            open.contains("cache write 795588"),
            "the cache WRITE was dropped: {open}"
        );
        assert!(open.contains("$11.73"), "{open}");
    }

    /// **EVERY MACHINERY SUBTYPE IS ABSENT, AND EVERY CONTENT SUBTYPE READS AS
    /// ITSELF.** The `unknown_subtype_inventory` of `0047`, asserted row by row
    /// — the drops by absence and the renders by their exact string.
    ///
    /// FORCED: move any subtype from the drop arm to the catch-all (or the
    /// reverse) and exactly one line of this goes red, naming it.
    #[test]
    fn the_unknown_kind_is_split_by_subtype_exactly_as_it_was_measured() {
        let raw = |v: Value| json!({"kind":"unknown","raw":v});
        let render = |v: Value| -> String {
            let mut s = store();
            opened(&mut s, "t1");
            s.apply(ev("t1", 1, raw(v)), now());
            texts(&thread_blocks(&s, false, false)).join("\n")
        };

        // ── DROPPED: two thirds of the live volume, and it says nothing ──
        for (name, v) in [
            (
                "thinking_tokens",
                json!({"type":"system","subtype":"thinking_tokens",
                                       "estimated_tokens":650,"estimated_tokens_delta":300}),
            ),
            (
                "status",
                json!({"type":"system","subtype":"status","status":"requesting"}),
            ),
            (
                "tool_progress",
                json!({"type":"tool_progress","heartbeat":true,
                                     "tool_name":"Agent","elapsed_time_seconds":540}),
            ),
            (
                "task_progress (historical)",
                json!({"type":"system","subtype":"task_progress"}),
            ),
            (
                "background_tasks_changed (historical)",
                json!({"type":"system","subtype":"background_tasks_changed"}),
            ),
            (
                "code_change_published",
                json!({"type":"system","subtype":"code_change_published"}),
            ),
            (
                "commands_changed",
                json!({"type":"system","subtype":"commands_changed"}),
            ),
        ] {
            assert_eq!(render(v), "", "`{name}` still takes a line");
        }

        // ── RENDERED: the content that was being thrown away as JSON ──
        let limits = render(json!({"type":"rate_limit_event","rate_limit_info":{
            "isUsingOverage":false,"rateLimitType":"five_hour","status":"allowed",
            "unifiedWindows":{"five_hour":{"utilization":0.1},"seven_day":{"utilization":0.16}}}}));
        assert_eq!(limits, "limits · 5h 10% · 7d 16%", "{limits}");

        let retry = render(json!({"type":"system","subtype":"api_retry","attempt":7,
                                  "max_retries":10,"retry_delay_ms":38882,"error":"unknown"}));
        assert_eq!(retry, "api retry · attempt 7/10 · in 38s", "{retry}");

        let refusal = render(json!({"type":"system","subtype":"model_refusal_fallback",
            "api_refusal_category":"cyber",
            "content":"Fable 5's safeguards flagged this message. Switched to Opus 4.8."}));
        assert!(
            refusal.starts_with("model refused · cyber · swapped · Fable 5's safeguards"),
            "he must never learn a model swap from JSON: {refusal}"
        );

        let denied = render(json!({"type":"system","subtype":"permission_denied",
            "decision_reason_type":"safetyCheck",
            "decision_reason":"Dangerous rm operation on possibly-empty variable path"}));
        assert!(
            denied.starts_with("permission denied · safetyCheck · Dangerous rm"),
            "{denied}"
        );

        let vcs = render(json!({"type":"system","subtype":"vcs_state_changed",
                                "kind":"commit","cwd":"/Users/testuser/.aui/x"}));
        assert_eq!(vcs, "commit · /Users/testuser/.aui/x", "{vcs}");

        // Injected context: real content, one line, and the whole of it on `x`.
        let injected = render(
            json!({"isSynthetic":true,"type":"user","message":{"content":[
            {"text":"Base directory for this skill: core/skills/task-runner"}]}}),
        );
        assert_eq!(
            injected, "context injected · Base directory for this skill: core/skills/task-runner",
            "{injected}"
        );

        // ── AND A SUBTYPE NOBODY ANTICIPATED IS SILENT, NOT `unknown · {…}` ──
        assert_eq!(
            render(json!({"type":"system","subtype":"a_subtype_from_next_year","x":1})),
            "",
            "his rule is that no row says unknown"
        );
    }

    /// **`background_tasks_changed` — THE SECOND OFFENDER HE DID NOT NAME.**
    /// 7,681 rows averaging 996 bytes, rendered as a clipped JSON blob because
    /// the kind was in `KNOWN_KINDS` and not in `TYPED_KINDS`.
    ///
    /// FORCED: take `background_tasks_changed` back out of `TYPED_KINDS` and
    /// the row is a JSON blob again.
    #[test]
    fn background_tasks_changed_is_a_count_and_not_a_kilobyte_of_json() {
        let mut s = store();
        opened(&mut s, "t1");
        s.apply(
            ev(
                "t1",
                1,
                json!({"kind":"background_tasks_changed","tasks":[
                    {"task_id":"t-aaa","task_type":"local_agent","description":"sweep the plugins"},
                    {"task_id":"t-bbb","task_type":"local_bash","description":"build"}]}),
            ),
            now(),
        );
        let t = texts(&thread_blocks(&s, false, false)).join("\n");
        assert_eq!(t, "background tasks · 2 running", "{t}");
        assert!(
            !t.contains("t-aaa"),
            "a task hash reached the one-line face: {t}"
        );
        let open = texts(&thread_blocks(&s, false, true)).join("\n");
        assert!(open.contains("sweep the plugins"), "{open}");
    }

    /// **ONE ROW FOR A CALL AND ITS RESULT** (`DEC-33`), including the case the
    /// merge has to survive: a call whose result has not arrived.
    ///
    /// FORCED: remove the `tools.remove(...)` merge branch and the count goes
    /// to three rows — a call row and a result row for the same act, which is
    /// what he asked to stop seeing.
    #[test]
    fn a_tool_call_and_its_result_are_one_row_and_an_unanswered_call_says_running() {
        let mut s = store();
        opened(&mut s, "t1");
        s.apply(
            ev(
                "t1",
                1,
                json!({"kind":"tool_use","id":"toolu_A","name":"Bash",
                               "input":{"command":"git status","description":"check"}}),
            ),
            now(),
        );
        // Still out: one row, and it says so rather than looking finished.
        let running = texts(&thread_blocks(&s, false, false));
        assert_eq!(
            running,
            vec!["tool · Bash · git status · running"],
            "{running:?}"
        );

        s.apply(
            ev(
                "t1",
                2,
                json!({"kind":"tool_result","tool_use_id":"toolu_A",
                               "is_error":false,"content":"nothing to commit"}),
            ),
            now(),
        );
        let done = texts(&thread_blocks(&s, false, false));
        assert_eq!(done, vec!["tool · Bash · git status · ok"], "{done:?}");
        assert!(
            !done[0].contains("toolu_A"),
            "a tool_use hash reached the one-line face: {done:?}"
        );

        // A FAILURE says failed and is red; a second, unrelated call is its own
        // row, so the merge is keyed and not positional.
        s.apply(
            ev(
                "t1",
                3,
                json!({"kind":"tool_use","id":"toolu_B","name":"Read",
                               "input":{"file_path":"/x/y.rs"}}),
            ),
            now(),
        );
        s.apply(
            ev(
                "t1",
                4,
                json!({"kind":"tool_result","tool_use_id":"toolu_B",
                               "is_error":true,"content":"no such file"}),
            ),
            now(),
        );
        let bs = thread_blocks(&s, false, false);
        assert_eq!(bs.len(), 2, "{:?}", texts(&bs));
        assert_eq!(texts(&bs)[1], "tool · Read · /x/y.rs · failed");
        assert!(matches!(
            &bs[1],
            Block::Lane {
                severity: Severity::Critical,
                ..
            }
        ));

        // **AND THE RESULT IS BEHIND IT.** The merged row is expandable and
        // opening it shows the full arguments and the full result — which is
        // the only place those bytes were ever going to be readable.
        assert!(
            bs[0].is_expandable(),
            "a merged row must announce what it holds"
        );
        let open = super::thread_blocks(
            &s,
            false,
            false,
            &Opened::from([("t1".to_string(), 1u64)]),
            &[],
            &TreeId::Parent,
            None,
        );
        assert!(
            texts(&open)[0].contains("nothing to commit"),
            "the result is not reachable: {:?}",
            texts(&open)
        );
        assert!(
            !texts(&open)[1].contains("no such file"),
            "opening one merged row opened another: {:?}",
            texts(&open)
        );

        // **AN OPEN MERGED ROW STILL CARRIES ITS AFFORDANCE, AND SAYS IT IS
        // OPEN.** The rewrite used to set `expandable = !wide`, which made a
        // merged row the ONE row in this projection that stopped advertising
        // itself at the moment he opened it — every other arm treats an open
        // row as expandable, because an open row's affordance is "close".
        // Invisible while the expander was a flag nobody drew; once `0056`
        // painted it, it meant the mark VANISHED on open, leaving no way back
        // but `x`.
        //
        // FORCED: restore `*expandable = !wide` and the first of these goes
        // red; drop `*open = wide` and the second does.
        assert!(
            open[0].is_expandable(),
            "an open merged row dropped its affordance: {:?}",
            texts(&open)
        );
        assert!(open[0].is_open(), "an open merged row reads as closed");
        assert!(!bs[0].is_open(), "a closed merged row reads as open");
    }

    /// **A RESULT WHOSE CALL FELL OFF THE BOTTOM STILL RENDERS.** The orphan
    /// case: dropping it would hide a tool failure nobody would ever see.
    #[test]
    fn a_result_with_no_call_in_view_is_not_swallowed() {
        let mut s = store();
        opened(&mut s, "t1");
        s.apply(
            ev(
                "t1",
                1,
                json!({"kind":"tool_result","tool_use_id":"toolu_gone",
                               "is_error":true,"content":"it blew up"}),
            ),
            now(),
        );
        let t = texts(&thread_blocks(&s, false, false)).join("\n");
        assert!(t.contains("failed"), "an orphan failure vanished: {t}");
        assert!(t.contains("call not in view"), "{t}");
        assert!(!t.contains("toolu_gone"), "a hash reached the face: {t}");
    }

    /// **NO THREAD, TASK, AGENT OR TOOL HASH IN THE TRANSCRIPT OR THE EVENT
    /// LANE.** His words: *"don't show me thread or task hash names. not
    /// useful to me."*
    ///
    /// # This test called itself a SWEEP and was not one — read that first
    ///
    /// It said *"A SWEEP rather than a list of cases: it drives every row
    /// builder that used to print one"*, and it greps exactly TWO projections:
    /// `thread_blocks` and `activity_blocks`. `0073` measured what the gap
    /// cost. In the shipped tree, with this test green, the face was drawing a
    /// hex id on **38 of 38 rows** of his real project list, a working
    /// directory in that list's header, `5f4413b1` in the thread header, and
    /// `queue · 57db2ec9` in the queue overlay. Four live violations of the
    /// directive this test names, under a guard that names it.
    ///
    /// The word "sweep" was the defect, not the coverage: a list of two that
    /// calls itself exhaustive stops anyone from asking what else there is.
    ///
    /// **THE SWEEP IS NOW A MACHINE, and it is
    /// `app::tests::no_surface_of_this_face_prints_a_hash_on_the_glass_or_the_clipboard`.**
    /// It enumerates MODES under an exhaustive `match` — so a new surface will
    /// not compile until the sweep knows how to reach it — renders each one as
    /// a frame, and greps what was drawn plus what a drag across it would
    /// copy. Whatever builder puts a hash anywhere, it lands there.
    ///
    /// THIS test is kept, narrowed to what it actually covers, because it is a
    /// UNIT test of the two block builders and it fails with a sharper message
    /// than a frame diff does.
    ///
    /// FORCED: put `short(task_id)` back into any one of those arms and this
    /// names it.
    #[test]
    fn no_row_prints_a_hash_it_was_handed() {
        let mut s = store();
        // **EIGHT CHARACTERS EACH, DELIBERATELY.** `short()` truncates an id to
        // its first eight, so a longer fixture id would let a leaked hash pass
        // this test by being cut one character short of what it greps for —
        // which is exactly how a mutation of `named` slipped through the first
        // forced-failure sweep.
        s.adopt_projects(vec![project("harness", &[("thrdhash", "the arc", false)])]);
        opened(&mut s, "thrdhash");
        let rows = vec![
            json!({"kind":"task_started","task_id":"taskhash","description":"diagnose the lane",
                   "subagent_type":"claude"}),
            json!({"kind":"task_progress","task_id":"taskhash","last_tool_name":"Grep"}),
            json!({"kind":"task_updated","task_id":"taskhash","status":"running"}),
            json!({"kind":"task_notification","task_id":"taskhash","status":"completed",
                   "summary":"found it"}),
            json!({"kind":"agent_launched","agent_id":"agnthash","status":"ok","is_async":true}),
            json!({"kind":"agent_completed","agent_id":"agnthash"}),
            json!({"kind":"tool_use","id":"toolhash","name":"Grep","input":{"pattern":"fold_of"}}),
        ];
        for (i, r) in rows.into_iter().enumerate() {
            s.apply(ev("thrdhash", i as u64 + 1, r), now());
        }
        for view in [
            texts(&thread_blocks(&s, false, false)).join("\n"),
            texts(&activity_blocks(&s, false, false)).join("\n"),
        ] {
            for hash in ["taskhash", "agnthash", "toolhash", "thrdhash"] {
                assert!(
                    !view.contains(hash),
                    "`{hash}` reached a one-line face:\n{view}"
                );
            }
            // …and the HUMAN thing is there instead, on the rows that have one.
            assert!(view.contains("diagnose the lane"), "{view}");
        }
        // The fleet lane names the thread by its INDEX SUMMARY, not its id.
        let fleet = texts(&activity_blocks(&s, false, false)).join("\n");
        assert!(
            fleet.contains("the arc"),
            "the fleet lane lost its thread label:\n{fleet}"
        );
    }

    /// **A LATER ROW BORROWS THE NAME AN EARLIER ONE CARRIED.** A
    /// `task_updated` is a status and a hash and nothing else; the description
    /// came in on the `task_started` above it.
    ///
    /// FORCED: make `remember_task_name` a no-op and the status rows go back to
    /// being anonymous.
    #[test]
    fn a_row_that_carries_only_a_hash_borrows_the_name_from_earlier_in_the_walk() {
        let mut s = store();
        opened(&mut s, "t1");
        s.apply(
            ev(
                "t1",
                1,
                json!({"kind":"task_started","task_id":"h","description":"port the migrations"}),
            ),
            now(),
        );
        s.apply(
            ev(
                "t1",
                2,
                json!({"kind":"task_updated","task_id":"h","status":"failed"}),
            ),
            now(),
        );
        let t = texts(&thread_blocks(&s, false, false));
        assert_eq!(t[1], "agent · port the migrations · failed", "{t:?}");
    }

    /// **`x` ALONE, FROM THE FACE HE STARTS ON, REACHES A STREAM CHUNK.**
    ///
    /// This asserts the PROPERTY and not the constant, which is the whole
    /// reason it replaces `only_one_lane_ships_off_and_it_is_the_stream`. That
    /// test could only ever say which lane was in a list; it could not have
    /// noticed that the list was costing `DEC-19` a keystroke, because the cost
    /// is not in the filter at all — it is in `project`, which skips a hidden
    /// lane's raw row BEFORE shaping it, so expansion never gets the chance.
    ///
    /// The two assertions are deliberately opposed, and both have to hold.
    /// `DEC-31` says stream chunks are omitted from the RENDERED face, so the
    /// default face must show none. `DEC-19` says nothing is dropped and `x`
    /// reaches every event, so ONE keystroke from that same face must show one.
    /// Off-by-default satisfied the first and quietly broke the second.
    ///
    /// # AND THE FILTER'S DELETION KEPT THE PROMISE FOR FREE
    ///
    /// `DEC-19`'s reachability promise was defended by a long note arguing
    /// that the stream lane must ship ON so that `x` alone still reaches a
    /// stream chunk. **With no lanes there is no lane to be off**, so the
    /// narrow question — *after the deletion, does `x` still reach every
    /// event?* — has a clean answer: YES, and the promise is now structural
    /// rather than defended.
    ///
    /// The argument, in one line: `Filter::DEFAULT_OFF` was EMPTY, so the
    /// default face already applied no predicate; the only guard `project`
    /// runs now is `stdout_only && !is_stdout(…)`, and `stdout_only` is
    /// `false` on the face he starts on. Nothing can be pre-hidden because
    /// there is no state to pre-hide it in. The third assertion below is what
    /// says so mechanically: with the toggle off, the projection's row count
    /// is the ledger's.
    ///
    /// FORCED: change `project`'s guard to `if !is_stdout(&r.event)` (the
    /// toggle ignored) and the SECOND and THIRD assertions go red — `x` alone
    /// reaches nothing and the default face is short. The first stays GREEN
    /// under that same mutation, and that is the point worth reading twice:
    /// `DEC-31`'s omission is implemented somewhere else entirely.
    #[test]
    fn nothing_ships_pre_hidden_and_x_alone_still_reaches_a_stream_chunk() {
        let mut s = store();
        opened(&mut s, "t1");
        s.apply(
            ev("t1", 1, json!({"kind":"stream_event","raw":{"i":1}})),
            now(),
        );
        s.apply(
            ev("t1", 2, json!({"kind":"turn_ended","exit_code":0})),
            now(),
        );

        // THE DEFAULT FACE — what is on screen before he presses anything.
        let closed = texts(&thread_blocks(&s, false, false));
        assert!(
            !closed.iter().any(|t| t.starts_with("stream")),
            "DEC-31: a stream chunk reached the default face: {closed:?}"
        );

        // ONE KEYSTROKE LATER. `x` toggles expansion and nothing else — and
        // `stdout_only: false` is the state he starts the session in, which is
        // why it is the only honest fixture for this property.
        let after_x = texts(&thread_blocks(&s, false, true));
        assert!(
            after_x.iter().any(|t| t.starts_with("stream")),
            "DEC-19: `x` alone did not reach a stream chunk: {after_x:?}"
        );

        // **AND NOTHING IS PRE-HIDDEN, asserted over every kind rather than
        // over a constant.** This is what `Filter::DEFAULT_OFF.is_empty()`
        // used to stand for; the constant is gone and the property it stood
        // for is stronger without it. One event of every documented kind goes
        // in, and the default expanded face must produce a row for each — if
        // any kind were silently dropped on the way in, it would be
        // unreachable by `x` and `DEC-19` would be broken for it.
        for kind in crate::proto::KNOWN_KINDS {
            let mut one = store();
            opened(&mut one, "t1");
            one.apply(ev("t1", 1, json!({"kind": kind})), now());
            assert!(
                !thread_blocks(&one, false, true).is_empty(),
                "`{kind}` is unreachable even with `x`: nothing projects it"
            );
        }
    }

    /// A subagent's words are not the thread's reply. The daemon draws the
    /// line; the face keeps it.
    #[test]
    fn a_sidechain_reply_is_not_boxed_as_the_thread_talking() {
        let mut s = store();
        opened(&mut s, "t1");
        s.apply(
            ev("t1", 1, json!({"kind":"assistant_text","text":"top level"})),
            now(),
        );
        s.apply(
            ev(
                "t1",
                2,
                json!({"kind":"assistant_text","text":"inner","parent_tool_use_id":"tu_1"}),
            ),
            now(),
        );
        let bs = thread_blocks(&s, false, false);
        assert!(bs[0].is_message(), "a top-level reply is a box");
        assert!(!bs[1].is_message(), "a sidechain reply is a lane row");
        assert!(bs[1].text().contains("subagent said"), "{}", bs[1].text());
    }

    #[test]
    fn the_open_thread_shows_only_its_own_events() {
        let mut s = store();
        opened(&mut s, "t1");
        s.apply(
            ev("t1", 1, json!({"kind":"assistant_text","text":"mine"})),
            now(),
        );
        s.apply(
            ev("t2", 1, json!({"kind":"assistant_text","text":"theirs"})),
            now(),
        );
        let t = texts(&thread_blocks(&s, false, false)).join("\n");
        assert!(t.contains("mine") && !t.contains("theirs"), "{t}");
        // …and the fleet lane shows both, prefixed.
        let a = texts(&activity_blocks(&s, false, false)).join("\n");
        assert!(a.contains("mine") && a.contains("theirs"), "{a}");
    }

    /// The same event arriving live and again as a replay is ONE row: `seq` is
    /// the key, so there is no dedupe pass to get wrong.
    #[test]
    fn a_replayed_event_collapses_onto_the_live_one() {
        let mut s = store();
        opened(&mut s, "t1");
        s.apply(
            ev("t1", 7, json!({"kind":"turn_ended","exit_code":0})),
            now(),
        );
        s.load_transcript(
            "t1",
            vec![LedgerRow {
                seq: 7,
                message_id: None,
                ts: Some(now()),
                received: now(),
                event: ThreadEvent::parse(json!({"kind":"turn_ended","exit_code":0})),
            }],
            1,
        );
        assert_eq!(thread_blocks(&s, false, false).len(), 1);
    }

    // ── THE DUPLICATE `turn done` (`0132`) ────────────────────────────────
    //
    // > *> turn done · success* — *getting multiple of these after each turn.*
    //
    // **THE LEDGER REALLY HOLDS SEVERAL, AND THE DAEMON IS RIGHT TO RECORD
    // THEM.** Measured on an APFS clone of the live `threads.db` (2,504,326
    // events): 12,940 `result_turn` against 11,092 `turn_started`, and on his
    // six most-recently-active threads 2,022 turns carry one result while 115
    // carry between two and twenty-six. The producer says why in its own
    // words (`lib/threads/src/runtime.rs`, the `ResultTurn` arm):
    //
    // > `modelUsage` is the CUMULATIVE per-model total for the whole claude
    // > process — parent and every subagent — as of the moment that `result`
    // > line was written […] **A turn with N subagents therefore emits N+1
    // > results all quoting the same running total.**
    //
    // So this is not one event rendered twice and it is not a replay: it is
    // N+1 genuine events, and the FACE is the producer of the duplicate
    // DISPLAY. The rows below are lifted VERBATIM from his ledger.

    /// **THE DEFECT, FROM REAL DATA.** Thread `6dda9d84`, seqs 79286/79287,
    /// 1.15 ms apart — byte-identical `model_usage`, identical cost, identical
    /// `num_turns`, differing only in `duration_ms`. Two rows that say the
    /// same thing twice is exactly what he is looking at.
    fn real_pair() -> (Value, Value) {
        let usage = json!({"claude-opus-5-5":{
            "cacheCreationInputTokens":564057,"cacheReadInputTokens":13415249,
            "costUSD":8.809590799999999,"inputTokens":210,"outputTokens":133004,
            "thinkingTokens":57509}});
        (
            json!({"kind":"result_turn","subtype":"success","is_error":false,
                   "total_cost_usd":8.809590799999999,"num_turns":5,
                   "duration_ms":60759,"stop_reason":"end_turn","model_usage":usage}),
            json!({"kind":"result_turn","subtype":"success","is_error":false,
                   "total_cost_usd":8.809590799999999,"num_turns":5,
                   "duration_ms":38389,"stop_reason":"end_turn","model_usage":usage}),
        )
    }

    fn lanes(bs: &[Block]) -> Vec<&Block> {
        bs.iter()
            .filter(|b| b.text().contains("turn done"))
            .collect()
    }

    #[test]
    fn a_real_burst_of_result_turns_is_one_row() {
        let (a, b) = real_pair();
        let mut s = store();
        opened(&mut s, "t1");
        s.apply(ev("t1", 79_286, a), now());
        s.apply(ev("t1", 79_287, b), now());
        let bs = thread_blocks(&s, false, false);
        let rows = lanes(&bs);
        assert_eq!(
            rows.len(),
            1,
            "his two real results must draw ONE row: {:?}",
            texts(&bs)
        );
        // And the row SAYS there were two, rather than hiding one.
        assert!(
            matches!(rows[0], Block::Lane { count: Some(2), .. }),
            "the collapsed row must carry ×2: {:?}",
            texts(&bs)
        );
    }

    /// **ONE RESULT IS STILL ONE ROW, AND IT CARRIES NO `×1`.** The other
    /// direction of the same rule — a fold that turned a single honest result
    /// into a counted row would be noise standing for nothing.
    #[test]
    fn one_result_turn_is_still_one_plain_row() {
        let (a, _) = real_pair();
        let mut s = store();
        opened(&mut s, "t1");
        s.apply(ev("t1", 79_286, a), now());
        let bs = thread_blocks(&s, false, false);
        let rows = lanes(&bs);
        assert_eq!(rows.len(), 1, "{:?}", texts(&bs));
        assert!(
            matches!(rows[0], Block::Lane { count: None, .. }),
            "a lone result must not be stamped: {:?}",
            texts(&bs)
        );
    }

    /// **A FAILURE INSIDE A BURST IS NEVER COLLAPSED INTO A SUCCESS.** The
    /// case that decides the KEY, and it is real: thread `6dda9d84`, seqs
    /// 43349–43353, four successes and an `is_error` result stamped to the
    /// same millisecond. Keying the run on the kind alone would have drawn one
    /// calm `×5` row and his failure would have been GONE from the default
    /// face. The key carries `subtype` and `is_error`, so the failure breaks
    /// the run and stands on its own, amber.
    #[test]
    fn a_real_mixed_burst_keeps_the_failure_visible() {
        let mut s = store();
        opened(&mut s, "t1");
        let cost = 65.88203775000001;
        for (seq, nt, dur, err) in [
            (43_349u64, 3u64, 26_062u64, false),
            (43_350, 8, 80_430, false),
            (43_351, 1, 3_969, false),
            (43_352, 3, 35_845, false),
            (43_353, 1, 1_438, true),
        ] {
            s.apply(
                ev(
                    "t1",
                    seq,
                    json!({"kind":"result_turn","subtype":"success","is_error":err,
                           "total_cost_usd":cost,"num_turns":nt,"duration_ms":dur,
                           "model_usage":{}}),
                ),
                now(),
            );
        }
        let bs = thread_blocks(&s, false, false);
        let rows = lanes(&bs);
        assert_eq!(
            rows.len(),
            2,
            "four successes collapse, the failure does not: {:?}",
            texts(&bs)
        );
        assert!(
            matches!(rows[0], Block::Lane { count: Some(4), .. }),
            "the four successes are one ×4 row: {:?}",
            texts(&bs)
        );
        assert!(
            matches!(
                rows[1],
                Block::Lane {
                    severity: Severity::Critical,
                    count: None,
                    ..
                }
            ),
            "the failure keeps its own amber row: {:?}",
            texts(&bs)
        );
    }

    /// **REACHABILITY, HALF ONE — `x`** (`DEC-19`). Expanded, every one of the
    /// collapsed events is on screen as its own row. Nothing was dropped from
    /// the store and nothing is unreachable; the fold is a DEFAULT-FACE
    /// decision, which is the same line `Shape::Silent` takes.
    #[test]
    fn every_result_in_a_collapsed_burst_is_reachable_under_x() {
        let (a, b) = real_pair();
        let mut s = store();
        opened(&mut s, "t1");
        s.apply(ev("t1", 79_286, a), now());
        s.apply(ev("t1", 79_287, b), now());
        let bs = thread_blocks(&s, false, true);
        assert_eq!(
            lanes(&bs).len(),
            2,
            "`x` must reach both real results: {:?}",
            texts(&bs)
        );
        let t = texts(&bs).join("\n");
        // The two differ only in their duration — both must be readable.
        assert!(
            t.contains("5 turn(s) · 1m ·") && t.contains("5 turn(s) · 38s ·"),
            "{t}"
        );
    }

    /// **REACHABILITY, HALF TWO — THE EXPANDER.** A row standing for more than
    /// one event always has something behind it, so it always offers the
    /// marker. Without this a burst of results on a SINGLE model would collapse
    /// into a row with no affordance at all — the breakdown is the only thing
    /// that made the ordinary result row expandable, and one model makes the
    /// open and closed texts identical.
    #[test]
    fn the_collapsed_result_row_offers_its_expander() {
        let mut s = store();
        opened(&mut s, "t1");
        for (seq, dur) in [(1u64, 60_759u64), (2, 38_389)] {
            s.apply(
                ev(
                    "t1",
                    seq,
                    json!({"kind":"result_turn","subtype":"success","is_error":false,
                           "num_turns":5,"duration_ms":dur,"model_usage":{}}),
                ),
                now(),
            );
        }
        let bs = thread_blocks(&s, false, false);
        let rows = lanes(&bs);
        assert_eq!(rows.len(), 1, "{:?}", texts(&bs));
        assert!(
            matches!(
                rows[0],
                Block::Lane {
                    count: Some(2),
                    expandable: true,
                    ..
                }
            ),
            "a ×n row must be openable: {:?}",
            texts(&bs)
        );
        // And OPENING it peels the newest off the run, which is the drill the
        // affordance promises.
        let key: tui_render::model::RowKey = ("t1".to_string(), 2);
        let mut open = Opened::new();
        open.insert(key);
        let bs = super::thread_blocks(
            &s,
            false,
            false,
            &open,
            &[],
            &TreeId::Parent,
            None,
        );
        assert_eq!(
            lanes(&bs).len(),
            2,
            "opening the collapsed row reveals the event behind it: {:?}",
            texts(&bs)
        );
    }

    /// **THE TURN WHOSE START HAS BEEN FORGOTTEN** — the case his 37,000-event
    /// threads generate and a synthetic fixture never would. `Open.forgotten`
    /// drops rows off the bottom of [`OPEN_CAP`], so a thread can hold a burst
    /// of results with NO `turn_started` in memory to key on.
    ///
    /// The rule survives it because the rule never mentions a turn: a run is a
    /// contiguous stretch of results in the projection, and contiguity is a
    /// property of the rows that are here. "The last result per `turn_started`"
    /// would have had nothing to key on and would have drawn all five.
    #[test]
    fn a_burst_whose_turn_start_was_forgotten_still_collapses() {
        let mut s = store();
        opened(&mut s, "t1");
        for seq in 900_001u64..=900_005 {
            s.apply(
                ev(
                    "t1",
                    seq,
                    json!({"kind":"result_turn","subtype":"success","is_error":false,
                           "num_turns":1,"duration_ms":seq,"model_usage":{}}),
                ),
                now(),
            );
        }
        let bs = thread_blocks(&s, false, false);
        assert!(
            !texts(&bs).iter().any(|t| t.contains("turn started")),
            "the fixture must have no turn boundary at all: {:?}",
            texts(&bs)
        );
        let rows = lanes(&bs);
        assert_eq!(rows.len(), 1, "{:?}", texts(&bs));
        assert!(
            matches!(rows[0], Block::Lane { count: Some(5), .. }),
            "{:?}",
            texts(&bs)
        );
    }

    /// **A RUN NEVER CROSSES A THREAD.** The fleet lane interleaves every
    /// thread's events, and `label_of` is empty for a thread the index has not
    /// named — so two threads' results are textually identical there. A run
    /// keyed on the event alone would have merged them under one thread's
    /// label and reported another thread's turn as this one's.
    #[test]
    fn a_run_never_collapses_across_two_threads() {
        let mut s = store();
        let row = |dur: u64| {
            json!({"kind":"result_turn","subtype":"success","is_error":false,
                   "num_turns":1,"duration_ms":dur,"model_usage":{}})
        };
        s.apply(ev("t1", 1, row(1_000)), now());
        s.apply(ev("t2", 1, row(2_000)), now());
        let bs = activity_blocks(&s, false, false);
        assert_eq!(
            lanes(&bs).len(),
            2,
            "two threads, two rows: {:?}",
            texts(&bs)
        );
    }

    /// **THE PAGING DECISION IS ANSWERED RATHER THAN NAMED NOW.**
    ///
    /// `0031` rendered `1000+` because both fleet calls are `LIMIT`-ed, and
    /// `0032` replaced it with the WINDOW's name because a page ordered
    /// `updated_at DESC` cannot say how many threads exist. It still cannot —
    /// but the face no longer has to ask it, because the count it renders is an
    /// INDEX count, which is a whole number of a real set. This asserts both
    /// halves: a real count, and no `+` anywhere.
    #[test]
    fn the_root_counts_a_real_set_and_never_renders_a_windowed_plus() {
        let mut s = store();
        s.adopt_projects(vec![
            project("harness", &[("a", "x", false), ("b", "y", false)]),
            project("broomstick", &[("c", "z", false)]),
        ]);
        // A CAPPED daemon page, which is what used to produce `1000+`.
        s.apply(
            TransportEvent::Fleet {
                statuses: vec![json!({"thread_id":"a","queued":0,"unread":0})],
                capped: true,
            },
            now(),
        );
        let h = header(&s, Mode::Projects);
        // **THE THREAD TOTAL LEFT THIS LINE** — "I don't need to see … 305
        // threads". What `1000+` was retired FOR still holds and is what this
        // test is really about: no number on this face may be a `LIMIT`-ed
        // page's length wearing a `+`. Asserted against the whole title now
        // that the count it used to guard is on the `projects` band header.
        assert!(
            !h.title.contains('+'),
            "`1000+` is retired and cannot come back through a capped page: {}",
            h.title
        );
        assert!(
            !h.title.contains("threads"),
            "the thread total came back to the header: {}",
            h.title
        );
        // POSITIVE WITNESS: the title is a real title, so the two absences
        // above mean what they say rather than meaning "the header is empty".
        assert!(h.title.contains("unread"), "{}", h.title);
        // And the count that is NOT a page length — the project total — is on
        // the band header, where a capped page cannot reach it: it is read off
        // the filesystem.
        let l = project_list(&s, 0, 0, None);
        assert_eq!(l.band_note(tui_render::BandId::Projects), "2");
        assert!(
            h.queue_scope.contains("his threads"),
            "the badge's scope must travel with it: {}",
            h.queue_scope
        );
        // The LIST's own header names the project, not a window.
        s.enter_project(0);
        let h = header(&s, Mode::List);
        assert!(h.title.starts_with("harness · 2 threads"), "{}", h.title);
        assert!(!h.title.contains("most recent"), "{}", h.title);
    }

    /// The `status` PUSH carries five of the query's eight keys. A push must
    /// therefore not be able to erase `last_seq`, which is what finds the tail.
    #[test]
    fn a_status_push_updates_what_it_knows_and_erases_nothing_else() {
        let mut s = store();
        s.apply(fleet(0, 0), now());
        assert_eq!(s.last_seq_of("t1"), Some(42));
        s.apply(
            TransportEvent::Frame {
                msg: ServerMsg::Status {
                    thread_id: "t1".into(),
                    state: "queued".into(),
                    executing: false,
                    queued: 3,
                    unread: 1,
                    detail: None,
                },
                replayed: false,
            },
            now(),
        );
        assert!(matches!(s.queued(Some("t1")), Known::Known(3)));
        assert_eq!(
            s.last_seq_of("t1"),
            Some(42),
            "the push does not carry last_seq and must not be read as saying zero"
        );
        // And an invalidation takes the lot away rather than leaving it current.
        s.apply(
            TransportEvent::Invalidate {
                reason: "lagged".into(),
            },
            now(),
        );
        assert!(matches!(s.queued(Some("t1")), Known::Unknown));
    }

    #[test]
    fn unread_elsewhere_excludes_the_thread_he_is_reading() {
        let mut s = store();
        s.apply(
            TransportEvent::Fleet {
                statuses: vec![
                    json!({"thread_id":"t1","queued":0,"unread":5}),
                    json!({"thread_id":"t2","queued":0,"unread":2}),
                ],
                capped: false,
            },
            now(),
        );
        assert!(matches!(s.unread_elsewhere(None), Known::Known(7)));
        assert!(matches!(s.unread_elsewhere(Some("t1")), Known::Known(2)));
    }

    #[test]
    fn the_list_puts_the_newest_thread_at_the_bottom() {
        let mut s = store();
        s.adopt_projects(vec![project(
            "harness",
            &[
                ("old", "the older one", false),
                ("new", "the newer one", false),
            ],
        )]);
        s.enter_project(0);
        s.apply(
            TransportEvent::Threads {
                rows: vec![
                    json!({"thread_id":"old","cwd":"/Users/x/a","updated_at":"2026-09-01T00:00:00Z"}),
                    json!({"thread_id":"new","cwd":"/Users/x/b","updated_at":"2026-09-14T00:00:00Z"}),
                ],
                capped: false,
            },
            now(),
        );
        let l = thread_list(&s, 0, 0);
        assert_eq!(l.rows.first().expect("a row").id, "old");
        assert_eq!(
            l.rows.last().expect("a row").id,
            "new",
            "newest at the bottom (DIR-3)"
        );
        assert_eq!(l.rows[0].cwd, "~/a", "$HOME shortens by PREFIX only");
        // The SUMMARY is what he reads; inside a project the cwd cannot tell
        // two threads apart.
        assert_eq!(l.rows[0].summary, "the older one");
    }

    #[test]
    fn the_render_tail_is_bounded_and_says_what_it_forgot() {
        let mut s = store();
        for i in 1..=(TAIL_CAP as u64 + 10) {
            s.apply(
                ev("t1", i, json!({"kind":"turn_ended","exit_code":0})),
                now(),
            );
        }
        assert_eq!(s.tail_len(), TAIL_CAP);
        assert_eq!(s.forgotten(), 10);
        let bs = activity_blocks(&s, false, false);
        assert!(
            bs.iter()
                .any(|b| matches!(b, Block::Rule { text } if text.contains("10 older row"))),
            "a cache that silently forgets is a transcript that silently lies"
        );
    }

    /// A caused cancel is not a warning (`0005`).
    #[test]
    fn an_operator_interrupt_is_calm_and_an_unexplained_one_is_not() {
        let calm = shape(
            &ThreadEvent::TurnInterrupted {
                reason: "he asked".into(),
                trigger: "operator".into(),
            },
            false,
        );
        let warn = shape(
            &ThreadEvent::TurnInterrupted {
                reason: "socket died".into(),
                trigger: "supervisor".into(),
            },
            false,
        );
        assert!(matches!(calm, Shape::Lane(_, Severity::Calm, _)));
        assert!(matches!(warn, Shape::Lane(_, Severity::Warn, _)));
    }

    /// Staleness never red (`0005`): a limit that names its reset is a WAIT.
    #[test]
    fn a_rate_limit_is_amber_and_an_api_error_is_red() {
        let limit = shape(
            &ThreadEvent::LimitHit {
                pool: Some("weekly".into()),
                reset: Some("2026-09-15T00:00:00Z".into()),
                text: "hit".into(),
            },
            false,
        );
        let err = shape(
            &ThreadEvent::ApiError {
                error: Some("overloaded".into()),
                status: Some(529),
                text: "".into(),
            },
            false,
        );
        assert!(matches!(limit, Shape::Lane(_, Severity::Warn, _)));
        assert!(matches!(err, Shape::Lane(_, Severity::Critical, _)));
    }

    /// **THE STDOUT PREDICATE IS NARROW, AND A KIND FROM THE FUTURE IS NOT
    /// STDOUT.**
    ///
    /// This is the same test slot that held `every_documented_kind_has_a_lane`,
    /// which proved `lane_of` total over all forty-four wire kinds so that none
    /// fell through to `Lane::Other`. With nine groups gone there is no
    /// totality left to prove — there is one question, asked of a row.
    ///
    /// **AND THE DEFAULT FLIPPED, deliberately.** The old table defaulted an
    /// unknown kind to `Lane::Other`, which was VISIBLE, because a lane that
    /// hid a kind nobody had classified would hide it silently. The new
    /// predicate defaults an unknown kind to NOT stdout, which is the safe
    /// direction for THIS question: `stdout only` must show him what was said,
    /// not what was said plus whatever the daemon shipped last week. Nothing is
    /// hidden by it that `x` and the toggle-off face do not reach.
    ///
    /// FORCED: make `is_stdout`'s fallback arm `true` and the last assertion
    /// goes red; drop `context_injected` from the list and the third does.
    #[test]
    fn only_what_was_said_is_stdout_and_an_unknown_kind_is_not() {
        let said = |kind: &str| is_stdout(&ThreadEvent::parse(json!({"kind": kind})));
        assert!(said("user_message"));
        assert!(said("assistant_text"));
        // A skill body loaded into the turn is CONTENT (`0047` D5/D7).
        assert!(said("context_injected"));
        for k in crate::proto::KNOWN_KINDS {
            if matches!(*k, "user_message" | "assistant_text" | "context_injected") {
                continue;
            }
            assert!(!said(k), "`{k}` counts as stdout and should not");
        }
        assert!(!said("a_kind_from_the_future"));
        // **AND THE `unknown`/`user` SUBTYPE IS STDOUT**, which is the one
        // refinement `lane_of_event` existed for and the only part of it that
        // had to survive: a sidechain reply is a subagent talking, and it is
        // speech either way.
        assert!(is_stdout(&ThreadEvent::Unknown {
            raw: json!({"type": "user"})
        }));
        assert!(!is_stdout(&ThreadEvent::Unknown {
            raw: json!({"type": "system", "subtype": "vcs_state_changed"})
        }));
    }

    /// **THE TWO-CONCERN BOUNDARY, MACHINE-CHECKED.** This file holds the
    /// session model AND the projection, deliberately — the header says why a
    /// second file would rename the problem rather than solve it. What keeps
    /// that from being an excuse is that the line is checkable, and this is
    /// the check.
    ///
    /// Both rules are about DIRECTION. Presentation may read the model; the
    /// model may not reach down into presentation, and the projection may not
    /// reach back and move what it is describing.
    #[test]
    fn the_two_halves_of_this_file_stay_on_their_own_side() {
        let src = include_str!("store.rs");
        let (model, projection) = src
            .split_once("//  ↑ THE MODEL  │  THE PROJECTION ↓")
            .expect("the boundary marker is gone — the line it draws went with it");
        // The projection half ends where the tests begin; a test is allowed to
        // do either thing, because proving a rule means naming it.
        let projection = projection
            .split_once("#[cfg(test)]")
            .map(|(p, _)| p)
            .unwrap_or(projection);

        // RULE 1: the model never builds presentation.
        for forbidden in ["-> Block", "Block::", "Line<", "-> Vec<Block>"] {
            assert!(
                !model.contains(forbidden),
                "`{forbidden}` appears ABOVE the line: the model half has started \
                 rendering, which is the drift the boundary exists to catch"
            );
        }

        // RULE 2: the projection never moves the model. Every public entry
        // point below the line takes it by shared reference.
        assert!(
            !projection.contains("&mut Store"),
            "something below the line takes `&mut Store`: the projection is \
             mutating the thing it is supposed to be describing"
        );
        // …and the five entry points are all still there to have been checked,
        // so a rename cannot make this test vacuous.
        for f in [
            // Wrapped across lines since the subject and the seq freeze became
            // parameters; the shared reference is still what is checked for.
            "pub fn thread_blocks(\n    store: &Store,",
            // Wrapped across lines since `0051` gave it a fourth argument; the
            // shared reference is still what this test is checking for.
            "pub fn activity_blocks(\n    store: &Store,",
            "pub fn thread_list(store: &Store",
            "pub fn header(store: &Store",
            "pub fn banner(store: &Store",
        ] {
            assert!(
                projection.contains(f),
                "`{f}…` is not below the line — either it moved or its signature \
                 changed, and this test stopped checking what it claims to"
            );
        }
    }

    // ── TUI-P1c: the daemon's `rolled` is the ONLY source of a subtree's word ──

    /// A fleet frame: `(id, state, executing, rolled, children)` — `rolled` is
    /// the daemon's `rolled.status` (its `display` the same word), `None` for
    /// no cell; `children` are all owed (`outstanding`).
    fn ui1b_fleet(rows: &[(&str, &str, bool, Option<&str>, &[&str])]) -> TransportEvent {
        let statuses = rows
            .iter()
            .map(|(id, state, exec, rolled, kids)| {
                let mut v = json!({"thread_id": id, "state": state, "executing": exec,
                                   "queued": 0, "unread": 0});
                if let Some(r) = rolled {
                    v["rolled"] = json!({"status": r, "display": r, "active_descendants": 0});
                }
                if !kids.is_empty() {
                    v["children"] = json!({
                        "outstanding": kids,
                        "runs": kids.iter().map(|k| json!({"id": k, "wake": "end",
                            "started_at": "2026-10-19T08:00:00Z", "finished": null}))
                            .collect::<Vec<_>>(),
                    });
                }
                v
            })
            .collect();
        TransportEvent::Fleet { statuses, capped: false }
    }

    fn ui1b(rows: &[(&str, &str, bool, Option<&str>, &[&str])]) -> Store {
        let mut s = store();
        s.apply(ui1b_fleet(rows), now());
        s
    }

    /// **THE WORD A THREAD SHOWS IS THE DAEMON'S `rolled`, NEVER A RECOUNT OF
    /// THE DESCENDANTS THIS STORE HOLDS** (TUI-P1c item 4, TC-10).
    ///
    /// A: `p` is idle and the daemon says `idle`, while the store HOLDS an
    /// executing grandchild `g` under `m` (a stale frame, or one the daemon has
    /// already folded as ended). The daemon's answer wins: `idle`. A face-side
    /// walk over the held `children` cells says `active`.
    /// B (opposition): the daemon says `active` for `p` while every descendant
    /// this store holds is idle (the active one is outside the mirror's
    /// window). `active`. A constant own-word fails B; a recount fails A and B.
    /// C: NO `rolled` cell yet, and an executing grandchild held — the thread
    /// shows its OWN word. The deleted fallback walked the held children here
    /// and said `active`.
    ///
    /// FORCED RED: `subtree_rollup` reinstated as the held-children walk
    /// (`shapewire::subtree::rollup` over the `children` cells) — A and C go
    /// red; with the walk only when no `rolled` cell is held (the P1b2 shape),
    /// C goes red.
    #[test]
    fn tui_p1c_the_status_word_is_the_daemons_rolled_never_a_face_recount() {
        use shapewire::subtree::Rollup;
        let s = ui1b(&[
            ("p", "idle", false, Some("idle"), &["m"]),
            ("m", "queued", false, Some("idle"), &["g"]),
            ("g", "active", true, Some("active"), &[]),
        ]);
        assert_eq!(s.subtree_rollup("p"), Some(Rollup::Own), "A: a recount beat the daemon");
        assert_eq!(state_word(&s, "p"), "idle", "A: the header recounted");
        assert_eq!(s.shown_word("g").as_deref(), Some("active"), "sentinel: g itself is active");
        let s = ui1b(&[
            ("p", "idle", false, Some("active"), &["c"]),
            ("c", "idle", false, Some("idle"), &[]),
        ]);
        assert_eq!(s.subtree_rollup("p"), Some(Rollup::SubtreeActive), "B: the daemon's active");
        assert_eq!(state_word(&s, "p"), "active", "B: the header");
        let s = ui1b(&[
            ("p", "idle", false, None, &["m"]),
            ("m", "queued", false, None, &["g"]),
            ("g", "active", true, None, &[]),
        ]);
        assert_eq!(s.subtree_rollup("p"), Some(Rollup::Own), "C: a face-side fallback rolled up");
        assert_eq!(state_word(&s, "p"), "idle", "C: no `rolled` cell is the thread's own word");
        assert_eq!(state_word(&s, "nobody"), "?");
    }
}
