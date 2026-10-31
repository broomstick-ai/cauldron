//! The per-thread settings blob (Q9) — changeable mid-thread, persisted whole.
//!
//! Everything CCW froze in `SessionSpec` at session start lives here as
//! mutable thread state: model (Q10), working path, plugin attachments,
//! retry/account policy (§10), context-injection toggle. `threads.update`
//! applies a [`SettingsPatch`] (only the provided fields change) and takes
//! effect on the NEXT spawn — which, with per-turn `-p` spawns, means the next
//! turn (oq-plugin-dirs-per-turn: "the plugin-manager workflow assumes edits
//! land immediately").

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How a thread picks its account (ported from CCW).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccountChoice {
    /// **The daemon picks once, at CREATE time, and the choice then STICKS.**
    ///
    /// The pick is `ThreadManager::resolve_account`: the first account of the
    /// selectable pool — one label per distinct login, dead logins excluded,
    /// ties broken by name. It does not consult the retry policy, the gauge, or
    /// any limit window; the pool-aware selector is `budgets::admit`, and the
    /// 429 failover goes through THAT — the retry path stamps the limit window
    /// and hands the gate an `unavailable` set, it does not choose an account.
    ///
    /// `Auto` does NOT mean "re-pick whenever you look at me". The claude session
    /// for a thread id lives inside one account's config dir, so re-picking
    /// restarts the conversation — see `ThreadManager::reresolve_account` for the
    /// stickiness rule and DEC-26 for why it is not negotiable.
    ///
    /// (This comment used to read "the daemon picks per the retry policy at spawn
    /// time". Every clause of that was wrong: the retry policy was never
    /// consulted, the pick is at create time, and it was re-run on every restart
    /// and every settings patch.)
    #[default]
    Auto,
    /// A specific registered account name. A pin is absolute: it is re-resolved
    /// to its name on every rehydration — that IS the self-healing case — and it
    /// parks rather than moving when the account is not admissible.
    Named(String),
}

/// Usage-limit failover policy (§10: "behavior is a PER-THREAD policy flag,
/// because other consumers differ").
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Failover {
    /// AUI's default: on a usage limit, retry on whichever registered account
    /// has usage available.
    #[default]
    AnyAvailable,
    /// Loop-style consumers: pinned to one account, never fail over.
    PinnedAccount { account: String },
}

/// Per-thread retry/account policy blob (§10).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RetryPolicy {
    #[serde(default)]
    pub failover: Failover,
    // REMOVED (DEC-19, Q-29): `usage_cap_fraction` — the never-wired
    // "fraction of one account" seam. Budget KEYS supersede it so there is
    // exactly ONE enforcement point (`budgets.rs`); the loop cap it described
    // is now a shared budget key with `session_pct`/`weekly_pct`. Nothing
    // outside this workspace consumes the threads service yet, so retiring it
    // is safe. Old settings blobs carrying the field still deserialize —
    // serde ignores it — and it is simply no longer read by anything.
    /// Attempt ceiling for TRANSIENT failures (overload / connection drop /
    /// error_during_execution). Usage-limit retries have their OWN, far higher
    /// ceiling — see `max_usage_limit_attempts` — because the two quantities
    /// are not comparable: three attempts on the transient ladder is about
    /// seven seconds, where a usage-limit attempt is one refused reset window.
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    /// Attempt ceiling for USAGE-LIMIT retries.
    ///
    /// This field used to not exist, and the doc comment above used to end
    /// *"Usage-limit retries are NOT capped — they wait for the reset and try
    /// again."* That sentence described a quantity `attempt` does not measure.
    /// It counts SPAWNED TURNS, not elapsed patience, so "not capped" bought an
    /// unbounded number of real `claude` processes rather than an unbounded
    /// willingness to wait — and bought 924 of them on one delivery on
    /// 2026-08-06. Waiting is still unbounded; only SPENDING is bounded.
    ///
    /// Per-thread like every other ceiling here, and old settings blobs written
    /// before the field existed deserialize straight onto the default.
    #[serde(default = "default_max_usage_limit_attempts")]
    pub max_usage_limit_attempts: u32,
    /// Attempt ceiling for DETERMINISTIC failures — a bad model name, a
    /// permission refusal, an error string the classifier recognized as nothing.
    ///
    /// This class had no ceiling because until 2026-08-25 it had no ladder: it
    /// was `RetryDecision::Fatal` and it ended the delivery. Under the rule that
    /// only a user interruption may do that, it needs a bound, and it needs a
    /// TIGHT one — an unbounded retry on a genuinely deterministic failure is a
    /// token furnace, spawning a real process every few seconds forever to be
    /// refused identically. See `retry::DEFAULT_MAX_DETERMINISTIC_ATTEMPTS`.
    #[serde(default = "default_max_deterministic_attempts")]
    pub max_deterministic_attempts: u32,
    /// Attempt ceiling for CREDIT-BALANCE retries.
    ///
    /// Bounds SPAWNS, not patience: the waiting is owned by the account bar, and
    /// a thread with every account barred sits in the admission gate rather than
    /// climbing this ladder. Each rung is a real re-attempt on an account
    /// placement believed had money. See `retry::DEFAULT_MAX_CREDIT_ATTEMPTS`.
    #[serde(default = "default_max_credit_attempts")]
    pub max_credit_attempts: u32,
    /// Attempt ceiling for LOGIN-EXPIRED retries.
    ///
    /// Bounds SPAWNS, not patience, exactly as `max_credit_attempts` does: the
    /// waiting is owned by the account bar (`retry::LOGIN_BAR_SECS`), and a
    /// thread whose every login has expired sits in the admission gate rather
    /// than climbing this ladder. Each rung is a real re-attempt on an account
    /// the gate believed was authenticated. See
    /// `retry::DEFAULT_MAX_LOGIN_ATTEMPTS`.
    #[serde(default = "default_max_login_attempts")]
    pub max_login_attempts: u32,
}

fn default_max_attempts() -> u32 {
    3
}

fn default_max_usage_limit_attempts() -> u32 {
    crate::retry::DEFAULT_MAX_USAGE_LIMIT_ATTEMPTS
}

fn default_max_deterministic_attempts() -> u32 {
    crate::retry::DEFAULT_MAX_DETERMINISTIC_ATTEMPTS
}

fn default_max_credit_attempts() -> u32 {
    crate::retry::DEFAULT_MAX_CREDIT_ATTEMPTS
}

fn default_max_login_attempts() -> u32 {
    crate::retry::DEFAULT_MAX_LOGIN_ATTEMPTS
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            failover: Failover::default(),
            max_attempts: default_max_attempts(),
            max_usage_limit_attempts: default_max_usage_limit_attempts(),
            max_deterministic_attempts: default_max_deterministic_attempts(),
            max_credit_attempts: default_max_credit_attempts(),
            max_login_attempts: default_max_login_attempts(),
        }
    }
}

/// Per-thread tool policy (ported verbatim from CCW's `ToolConfig`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ToolConfig {
    /// When true, start with ALL built-in tools deactivated (`--tools ""`).
    #[serde(default)]
    pub clean_slate: bool,
    /// Explicit built-in tool set (e.g. `"Bash,Edit,Read"`). Ignored when
    /// `clean_slate` is true. `None` = claude's default.
    #[serde(default)]
    pub tools: Option<String>,
    /// Injected custom agents as a JSON object (`--agents <json>`).
    #[serde(default)]
    pub agents_json: Option<String>,
    /// Allow-list fence (supports scoping like `Bash(git *)`).
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Deny-list fence.
    #[serde(default)]
    pub disallowed_tools: Vec<String>,
}

/// The full per-thread settings blob. Stored as `settings_json` on the thread
/// row; every field is live-editable via `threads.update`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThreadSettings {
    /// Working directory — AUTHORITATIVE (Q7: "threads are initialized in a
    /// directory... the agent already knows its cwd"). Set as the child's
    /// `current_dir`.
    pub cwd: String,
    /// Model alias (`sonnet`/`opus`/`fable`) or full id — passed verbatim so
    /// tiers keep floating with Claude Code updates. Changeable mid-thread
    /// (Q10).
    pub model: String,
    #[serde(default)]
    pub account: AccountChoice,
    #[serde(default)]
    pub append_system_prompt: Option<String>,
    #[serde(default)]
    pub add_dirs: Vec<String>,
    /// `None` ⇒ `--dangerously-skip-permissions` (AUI's default posture).
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub tools: ToolConfig,
    #[serde(default)]
    pub fallback_model: Option<String>,
    #[serde(default)]
    pub max_budget_usd: Option<f64>,
    /// **The agent type this thread runs as** — `--agent <name>` on every turn
    /// (R4.S8, `docs/managed-subagents.md` §1.5). An OPAQUE string: threads
    /// never resolves, types or validates it beyond refusing an empty value or
    /// one that would read as a flag; the name resolves in the child's own
    /// process through its inherited `plugin_roots`. `None` adds nothing.
    #[serde(default)]
    pub agent: Option<String>,
    /// **The effort level** — `--effort <level>` on every turn (R4.S8). One of
    /// [`EFFORT_LEVELS`], checked where it enters (`agents/spawn`), never
    /// clamped. `None` adds nothing, which is the CLI's own default.
    #[serde(default)]
    pub effort: Option<String>,
    /// Roots scanned for plugin dirs EVERY turn (hot reload, Q9): each child
    /// dir holding a `.claude-plugin/plugin.{json,yaml}` manifest is attached.
    /// AUI's two-tier rollup maps to `[<project>/.mind/plugins,
    /// <repo>/core-plugins]` in that order (project overrides global).
    #[serde(default)]
    pub plugin_roots: Vec<String>,
    /// Explicitly attached plugin dirs. A listed dir MISSING its manifest gets
    /// one auto-built (§12: "given a plugin dir without a marketplace, threads
    /// builds the marketplace").
    #[serde(default)]
    pub plugin_dirs: Vec<String>,
    #[serde(default)]
    pub retry_policy: RetryPolicy,
    /// The thread's BUDGET KEY (DEC-4). A key means this thread uses and
    /// CONSUMES that budget, and several threads may share one key (that is the
    /// point of keying); it is re-pointable mid-life (DEC-14). `None` still
    /// means no budget at all — unmetered and ungated — but a thread now has to
    /// be unkeyed DELIBERATELY to get there, because of the default below.
    ///
    /// **MANDATORY DEFAULT INHERITANCE (INTENT 72b).** A thread that names no
    /// key is GIVEN one — [`crate::budgets::DEFAULT_BUDGET_KEY`] — it is never
    /// refused. The serde default is what makes this an invariant rather than a
    /// convention: every consumer of the crate gets it, including ones that do
    /// not know budgets exist, so it cannot be bypassed by forgetting.
    ///
    /// It is deliberately NOT a refusal at creation. A thread is created when
    /// the operator says "new thread" into an audio interface with no hands; if
    /// creation could fail for a configuration reason, that failure would have
    /// to be spoken, understood and repaired by voice before he could say the
    /// thing he actually meant to say. There is no acceptable spoken form of
    /// "I could not start your thread because it has no budget." Refusal also
    /// fails open in practice — every caller grows a fallback, and the
    /// fallbacks get written as "if no budget, use none".
    ///
    /// An explicit `"budget_key": null` over `threads.update` still unkeys the
    /// thread. That is a deliberate operator act, not an omission, and it is
    /// the documented remedy path; the default covers everything that arrives
    /// by forgetting.
    /// The field is written even when `None` — no `skip_serializing_if` — and
    /// that omission is load-bearing rather than untidy. With the serde default
    /// above, ABSENT means "inherit the default". If a deliberate unkey were
    /// serialized by being left out, it would read back as the default on the
    /// next load and the operator's explicit act would silently undo itself. So
    /// absent means inherit, and an explicit `null` means unkeyed, and the two
    /// stay distinguishable on disk.
    ///
    /// This is also the whole backfill. Every thread stored before this change
    /// has no `budget_key` field in its blob, so all of them inherit the default
    /// the moment their settings are read — no migration script, no daemon
    /// restart, nothing to run against the live database.
    #[serde(default = "default_budget_key")]
    pub budget_key: Option<String>,
    /// Per-thread threshold GATES on the four gauges (DEC-6). Distinct from
    /// the budget: not metered, not consumed, and they compose with it (AND).
    #[serde(default)]
    pub thresholds: crate::budgets::Thresholds,
    /// **Turn the budgets module OFF for this thread** (round-7 item 1).
    ///
    /// Enforcement fails CLOSED: if the budgets module errors internally,
    /// NOTHING runs — except threads with this flag set. That is the operator's
    /// remedy path, stated verbatim: "I would turn off budgeting on a
    /// particular thread, bypass that module, fix the module, and then resume
    /// behavior." A bypassed thread is neither metered nor gated: no charges,
    /// no thresholds, no trips. Settable live over `threads.update`, exactly
    /// like every other setting, so the bypass needs no daemon restart.
    #[serde(default)]
    pub budgeting_off: bool,
    /// **INTERRUPT THE RUNNING TURN when this thread's account is barred.**
    /// Defaults to TRUE.
    ///
    /// The daemon learns an account is exhausted from three places — the
    /// `/usage` poller, a subagent's own usage-limit death, and the parent
    /// turn's 429. Only the third could ever act, and only when it arrived.
    /// On 2026-08-14 the first fired at 19:26:13, the second six times between
    /// 19:33 and 19:36, and the third at 19:39:37: thirty-three minutes in
    /// which a turn kept running on an account the daemon already knew was at
    /// 100%, and the operator heard nothing. With this set, the first two
    /// signals tear the execution down where the third one would have, and the
    /// turn goes back through the SAME admission gate (`admit_or_park`) — no
    /// placement logic on the interrupt path (DEC-56 as amended, msg #1412:
    /// "you don't directly attempt to put it in another account. You put it
    /// back to the balancer"), and the prompt is parked or laddered, never
    /// dropped (CON-4).
    ///
    /// **DEFAULT TRUE, on the operator's explicit instruction, 2026-08-15:**
    /// *"Yeah, I want interrupt mid turn as an opt-in flag for all of the
    /// threads in AUI. Go ahead and default it to true."* It stays a FLAG —
    /// settable to `false` per thread over `threads.update` — because it cuts
    /// against a decided position that still stands elsewhere:
    /// [`crate::budgets::TripAction`] defaults to `Finish` precisely because
    /// "killing mid-turn can leave half-applied edits behind". That reasoning is
    /// unchanged FOR BUDGET TRIPS, and `TripAction` keeps its finish-by-default.
    /// The trade differs here: a budget trip means WE decided to stop, and
    /// finishing costs only money, whereas a usage limit means the ACCOUNT has
    /// already stopped — the turn is not going to complete, so letting it run is
    /// not continuity, it is silence.
    ///
    /// Known residual, not papered over: the teardown is a process-group SIGTERM
    /// (then SIGKILL), so every running subagent dies with the parent, and
    /// `--resume` replays the transcript but not the model's in-progress
    /// reasoning. The transcript itself survives —
    /// `ThreadManager::trimmable_tail` refuses to roll back any tail containing a
    /// real assistant record — so the conversation is intact; the half-finished
    /// thought is not.
    #[serde(default = "default_true")]
    pub limit_kill_mid_turn: bool,
    /// **SCRUB THE DETAILS OF EVERY API ERROR OFF THIS THREAD'S TRANSCRIPT
    /// BEFORE EVERY SPAWN.** Defaults to TRUE.
    ///
    /// The operator's words, 2026-08-17: *"we can have a flag that's set on a
    /// per thread basis. And when this flag is on, we scrub usage limits. So
    /// what we do is before ever starting an agent backup, regardless of if we
    /// just switched accounts or not, we go into the data structure and where
    /// it says API error hit usage limit, we take that whole sentence out and
    /// just call it API error. We don't tell it what the API error was at all.
    /// And we can just do that for all API errors just so it's general, so it's
    /// not trying to infer because it'll do it, it'll infer."*
    ///
    /// **ALL API errors, not only usage limits, and that generality is the
    /// point.** If only limits were blanked, a blank would MEAN "limit" and the
    /// scrub would be a louder signal than the prose it replaced.
    ///
    /// The mechanism and the exact shapes covered are [`crate::scrub`]; the
    /// spawn-path call is in `ThreadManager::run_once`, deliberately on EVERY
    /// spawn rather than only after an account switch — a limit hit six turns
    /// ago is still sitting in the transcript on turn seven.
    ///
    /// **DEFAULT TRUE, and the default is what the AUI actually gets.** AUI's
    /// `build_thread_settings` (`~/code/harness/apps/aui/src/threads_client.rs`)
    /// sends ten keys and omits every other field, so the serde default below
    /// IS the AUI default — there is no second place to set it and no way to
    /// forget. `tests/scrub_settings.rs` deserializes AUI's exact blob and
    /// asserts this, so a future AUI that starts sending the key cannot change
    /// the default silently.
    #[serde(default = "default_true")]
    pub scrub_api_error_details: bool,
    /// **ONE FLAG PER INJECTED THING** — see [`Injection`]. Flattened, so every
    /// flag is a top-level key of the settings blob exactly as the two switches
    /// it replaced were.
    #[serde(flatten)]
    pub injection: Injection,
    /// **WHO the application-declared headers on this thread belong to**
    /// (INTENT 66b). Stamped as `app="…"` on every `<application_header>` the
    /// thread renders.
    ///
    /// It lives in SETTINGS rather than on the message deliberately. A change
    /// here is written to the ledger as a `SettingsChanged` event, so the
    /// identity a header is attributed to can never move without a record of
    /// it moving; a per-message field would let one message claim to be the
    /// AUI and the next claim to be something else, silently. `None` renders
    /// `app="unidentified"`, which is the honest answer and looks wrong on
    /// purpose. Must be a lowercase identifier; anything else is refused when
    /// headers are rendered, not when the setting is stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    /// **MAY THIS THREAD KEEP GOING AFTER AN AUTOMATIC COMPACTION?**
    /// Defaults to FALSE, and the default is the whole point.
    ///
    /// The operator's words, 2026-08-28: *"if we can run cloud code with a flag
    /// indicating like we do not want to permit it to do an automatic
    /// compaction, I want that. I no longer want automatic compactions. Like an
    /// automatic compaction on its own is actually fine. What I really don't
    /// want is resume after compaction. I want to have a flag when we're running
    /// a thread, when we're submitting a thread job into the pool. I want to
    /// have a flag called resume after compaction, which defaults to false…
    /// because it makes such catastrophic errors after every compaction, every
    /// single time. I really need to give it a preamble to let it know, okay,
    /// here's what you're doing, here's the mistakes you're going to make, don't
    /// make those mistakes. I just want it to stop. I want it to end. I want the
    /// turn to end. It should not keep going on its own."*
    ///
    /// **A COMPACTION IS STILL FINE AND STILL HAPPENS.** This flag does not
    /// prevent one and must never be read as doing so — he said so in the same
    /// breath. What it governs is the thread's own MOMENTUM afterwards: the
    /// model resuming, from a summary it has just been handed, work it no longer
    /// accurately remembers. That is where the catastrophic errors come from,
    /// and the remedy he asked for is not a better summary, it is an ending — he
    /// comes back and starts the next turn himself, with whatever framing he
    /// wants to give it.
    ///
    /// **FALSE BY DEFAULT, so the new behaviour is what a caller gets by
    /// forgetting and CONTINUING is the deliberate opt-in.** With
    /// `#[serde(default)]` on a `bool` that is simply `false`, which also makes
    /// it the whole backfill: every thread stored before this change has no
    /// `resume_after_compaction` key in its blob, so all of them read as
    /// stopping the moment their settings are next loaded — no migration, no
    /// daemon restart, nothing to run against the live database.
    ///
    /// **The ending is not a kill and not a failure.** It lands as
    /// [`crate::event::ThreadEvent::TurnInterrupted`] with trigger
    /// [`crate::event::InterruptTrigger::Compaction`] — the daemon's one
    /// interrupt vocabulary — and, like the operator's own cancel, it ENDS the
    /// delivery rather than laddering it (`retry::ends_the_delivery`). Work
    /// already on the ledger stays there, the compaction bookkeeping still runs,
    /// and anything queued behind the turn is untouched and goes out on the next
    /// one, which receives the ordinary post-compaction restatement.
    ///
    /// **Why not simply tell the CLI not to auto-compact.** It can be told —
    /// `DISABLE_COMPACT`, `DISABLE_AUTO_COMPACT`, or `autoCompactEnabled: false`
    /// — but what it then does when it WOULD have compacted is answer the turn
    /// with `"Prompt is too long"` and `error: "invalid_request"`. That is worse
    /// than compacting and it is unrecoverable: with auto-compact off nothing
    /// ever shrinks the context, so every later turn on that thread hits the
    /// same wall. Hence the decision lives here, in our own machinery, where a
    /// compaction is allowed to happen and only the resumption is refused.
    #[serde(default)]
    pub resume_after_compaction: bool,
    /// **IS THIS THREAD'S NATIVE FILE A PROJECTION OF THE LEDGER?** Defaults
    /// to [`Projection::NativeOwned`], which is today's path, byte for byte.
    ///
    /// The per-thread switch `docs/thread-adapters.md` §5.8 phase 3 named
    /// (`projection: native_owned | projected`), carried on the ONE settings
    /// mechanism this crate has rather than a second one. Under `projected`
    /// the turn renders the native file from `lib/threads/src/ledger/` before
    /// the spawn, reconciles the runtime's tail into it after the exit, and
    /// refuses placement while the file holds what the ledger does not — the
    /// mechanism `docs/proto-ledger.md` §3–§4 specifies, wired in
    /// `ledger::turn`. Under `native_owned` none of that code runs: the daemon
    /// keeps writing `events` exactly as it does today, which is the whole
    /// reason this is a per-thread flag with the OFF default rather than a
    /// build flag — a thread is flipped deliberately and can be flipped back
    /// with one `threads.update`.
    ///
    /// **Flipping a thread that already holds a native conversation is
    /// REFUSED at turn start** (`ledger::turn::ProjectionRefused::NativeWithoutLedger`)
    /// until the cutover (R4.S4b) has built its ledger from the raw copies: a
    /// render from an empty ledger would overwrite the only conversation that
    /// thread has, and nothing here is allowed to destroy one.
    #[serde(default)]
    pub projection: Projection,
    /// **WHICH NAMED RENDER LOSSES THIS THREAD ACCEPTS**, beyond the three the
    /// operator already ruled accepted (`ledger::turn::ACCEPTED_LOSSES`). A
    /// render that meets any other loss — today that is
    /// `prompt_text_unrecoverable`, a prompt of his whose text the ledger does
    /// not hold — is REFUSED by name unless the name is listed here, and the
    /// turn parks with cause `projection` rather than running on a history
    /// with a hole in it. `docs/thread-adapters.md` §3.2's
    /// `allow_inexpressible`, in the proto-ledger's vocabulary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projection_allow_loss: Vec<String>,
    /// Opaque client blob for the thread as a whole (AUI stores its own
    /// thread uuid, summary, flags here). Stored and returned, never read.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

fn default_true() -> bool {
    true
}

/// Who owns a thread's native session file — see
/// [`ThreadSettings::projection`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Projection {
    /// Today: the runtime writes the file and the daemon never rebuilds it.
    /// The default, and the path every thread not deliberately flipped runs.
    #[default]
    NativeOwned,
    /// The file is rendered from the ledger at every turn start and the
    /// runtime's tail is reconciled back after every exit.
    Projected,
}

impl Projection {
    pub fn as_str(&self) -> &'static str {
        match self {
            Projection::NativeOwned => "native_owned",
            Projection::Projected => "projected",
        }
    }
    pub fn is_projected(&self) -> bool {
        matches!(self, Projection::Projected)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// INJECTION — ONE FLAG PER INJECTED THING (DIR-56, as actually asked for)
// ═══════════════════════════════════════════════════════════════════════════

/// **ONE FLAG PER THING THREADS INJECTS. No levels, no master switch, no
/// nesting, and therefore nothing to keep coherent.**
///
/// DIR-56 asked for "a series of flags that you could turn on to allow
/// inserting different levels of metadata into the thread." It was built as a
/// LEVEL (`metadata_level: standard | none`) sitting under a master switch
/// (`inject_context`), with the context id nested under BOTH — and a validator,
/// `ensure_context_id_coherent`, whose entire reason for existing was that the
/// nesting made an incoherent combination representable. The operator's ruling
/// retiring all three:
///
/// > *"`inject_context: true` — this now seems inappropriately named. and I
/// > don't know what it means. and I don't know why the id depends on it. If we
/// > remove the general settings and just have individual flags then I don't
/// > think we need this."*
///
/// > *"`metadata_level: standard` — this seems generic and not easy to
/// > understand. We should probably just have flags on certain metadata being
/// > injected."*
///
/// And on the validator: *"ensuring it's coherent sounds like a code thing, not
/// a setting thing. It should always be coherent."*
///
/// # Why there is no `inject_message_metadata` flag, and that is the point
///
/// The per-message `<thread_system_notice kind="message_metadata">` block is
/// **not a thing you flag — it is DERIVED.** It is rendered if and only if at
/// least one of its lines is switched on, and
/// [`crate::context::message_metadata_notice`] returns `None` when none is. A
/// flag for the envelope ALONGSIDE flags for its lines would recreate exactly
/// the nesting this type removes, and a validator would be wanted again within
/// the week.
///
/// So every combination of these twelve booleans means something:
///
/// * the context id and nothing else → a block with one line;
/// * every line off → no block at all, which is what `metadata_level: none`
///   used to spell, and the renderer emits no envelope, no blank line, nothing
///   (checked against the old code path and pinned in
///   `tests/fixtures/injection-parity.golden`);
/// * every flag off → the model is told nothing extra, which is what
///   `inject_context: false` used to spell. [`Self::NOTHING`].
///
/// # What is deliberately NOT here
///
/// The `body_scrutiny` notice has no flag and must not acquire one. It
/// announces that threads EDITED the operator's own words to neutralise forged
/// control structure; announcing an edit is part of making it, not context.
/// `crate::context::build_prompt_body` emits it regardless of every flag on
/// this struct, and `header_protocol.rs` holds that to be true.
///
/// # Parity
///
/// [`Default`] reproduces `inject_context: true` + `metadata_level: standard`
/// + `inject_context_id: false` byte for byte. That is not an argument, it is
/// `tests/injection_parity.rs`, diffed against a golden the OLD code generated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "InjectionWire", into = "InjectionWire")]
pub struct Injection {
    // ── THE PREAMBLES AND NOTICES (what `inject_context` used to gate) ────
    /// The opening `[thread <id>]` line plus the plugin paths, on the first
    /// turn of a thread — `crate::context::first_turn_preamble`.
    #[serde(rename = "inject_first_turn_preamble")]
    pub first_turn_preamble: bool,
    /// The post-compaction restatement (§12): thread id, working path, now, the
    /// swallowed message-id range, plugin paths —
    /// `crate::context::compaction_restatement`.
    ///
    /// Switching this off also leaves `threads.needs_restate` UNCONSUMED, so a
    /// thread whose restatement is suppressed will deliver one if the flag is
    /// later turned on. That is what `inject_context: false` already did; it is
    /// recorded here rather than changed.
    #[serde(rename = "inject_compaction_restatement")]
    pub compaction_restatement: bool,
    /// The hot-reload notice naming the thread's current plugin set —
    /// `crate::context::plugins_changed_preamble`.
    #[serde(rename = "inject_plugin_changes")]
    pub plugin_changes: bool,
    /// Threads' own category-(b) system notices: `resumed_after_interrupt`,
    /// `queued_turn`, `interrupted` —
    /// `crate::context::assemble_model_notices`. Off, the notices are not
    /// minted at all, so the interrupted flag stays up rather than being
    /// cleared by a notice nobody was shown.
    #[serde(rename = "inject_system_notices")]
    pub system_notices: bool,
    /// Every `<application_header app="…">` an application declared for this
    /// message — `crate::context::application_header`.
    #[serde(rename = "inject_application_headers")]
    pub application_headers: bool,

    // ── THE PER-MESSAGE METADATA BLOCK'S LINES ────────────────────────────
    // The block itself is derived: it exists iff at least one of these is on.
    /// `thread: <id>` — on the boundary posts (thread start, first after a
    /// compaction), as INTENT 65 specifies.
    #[serde(rename = "inject_thread_id")]
    pub thread_id: bool,
    /// `context: <thread_id>.<boundary>` — the retrieval-dedup cache key. Read
    /// [`crate::context::context_id`] for what it is and why it exists.
    ///
    /// **The only flag here that defaults to FALSE**, because it is a
    /// capability a specific consumer opts into and every other thread would
    /// pay a line of context for a key nobody reads. Threads STARTED FROM the
    /// AUI or the TUI get it true — see
    /// [`ThreadSettings::apply_creator_defaults`], which is where the
    /// operator's *"I mean threads started from AUI and TUI"* lives.
    #[serde(rename = "inject_context_id")]
    pub context_id: bool,
    /// `message: #<n>` — the threads ledger id.
    #[serde(rename = "inject_message_id")]
    pub message_id: bool,
    /// `idx: #<n>` — the APPLICATION's own coordinate, the one a citation
    /// resolves (`crate::context::APP_IDX_KEYS`). Absent anyway when the
    /// caller sent none; this flag suppresses it when they did.
    #[serde(rename = "inject_app_idx")]
    pub app_idx: bool,
    /// `sent: <stamp>` — local wall clock, with the date elided while the
    /// calendar day holds. Off, the elision state (`threads.last_meta_date`)
    /// stops advancing too, because nothing is being stated to elide against.
    #[serde(rename = "inject_timestamp")]
    pub timestamp: bool,
    /// `duration: <formatted>` — how long the operator's audio ran. Absent
    /// anyway when the caller sent no audio start.
    #[serde(rename = "inject_duration")]
    pub duration: bool,
    /// The `Queued messages:` lead-in above a multi-message drain. A thing
    /// threads injects, so a flag of its own; a single-section delivery never
    /// carries it whatever this says.
    #[serde(rename = "inject_queue_lead_in")]
    pub queue_lead_in: bool,
}

impl Default for Injection {
    /// **TODAY'S BEHAVIOUR, EXACTLY** — `inject_context: true` plus
    /// `metadata_level: standard` plus `inject_context_id: false`.
    fn default() -> Self {
        Self {
            first_turn_preamble: true,
            compaction_restatement: true,
            plugin_changes: true,
            system_notices: true,
            application_headers: true,
            thread_id: true,
            context_id: false,
            message_id: true,
            app_idx: true,
            timestamp: true,
            duration: true,
            queue_lead_in: true,
        }
    }
}

impl Injection {
    /// **TELL THE MODEL NOTHING EXTRA** — what `inject_context: false` meant,
    /// spelled as what it actually did. Every flag off.
    pub const NOTHING: Self = Self {
        first_turn_preamble: false,
        compaction_restatement: false,
        plugin_changes: false,
        system_notices: false,
        application_headers: false,
        thread_id: false,
        context_id: false,
        message_id: false,
        app_idx: false,
        timestamp: false,
        duration: false,
        queue_lead_in: false,
    };

    /// **THE APPLICATIONS WHOSE THREADS GET THE CONTEXT ID BY DEFAULT.** The
    /// operator: *"I want it to be set to true for AUI and TUI consumers. It
    /// can default to false for the rest"*, narrowed by his own clarification
    /// to *"I mean threads started from AUI and TUI"* — so this is matched
    /// against `ThreadSettings::app_id`, per thread, at creation.
    ///
    /// **`harness-tui` IS THE TUI** (R4.S7, IS-27 ask 1): it is the `app_id`
    /// cauldron's TUI actually sends (`apps/tui/src/acts.rs`, `route_new_thread`
    /// — deliberately not `aui`). With only `tui` here, no thread the TUI
    /// started ever got the default. `tui` stays for a face that spells it so.
    /// `tests/injection_flags.rs` reads the TUI's source for the literal, so the
    /// two cannot drift apart silently.
    pub const CONTEXT_ID_BY_DEFAULT_FOR: &'static [&'static str] = &["aui", "tui", "harness-tui"];

    /// Switch off every line of the per-message metadata block, and the queue
    /// lead-in that was part of the same presentation. This is precisely what
    /// `metadata_level: none` meant.
    fn silence_message_metadata(&mut self) {
        self.thread_id = false;
        self.context_id = false;
        self.message_id = false;
        self.app_idx = false;
        self.timestamp = false;
        self.duration = false;
        self.queue_lead_in = false;
    }

    /// Does this thread inject ANY line of the per-message metadata block? The
    /// block is derived from this and never flagged directly.
    pub fn any_message_metadata(&self) -> bool {
        self.thread_id
            || self.context_id
            || self.message_id
            || self.app_idx
            || self.timestamp
            || self.duration
    }

    /// Apply a partial update, returning the wire keys actually set so the
    /// `SettingsChanged` audit event names them.
    fn apply(&mut self, patch: InjectionPatch) -> Vec<(String, Value)> {
        let mut applied: Vec<(String, Value)> = Vec::new();
        // `$wire` is BOTH the patch field and the audit key, so the two cannot
        // drift: a flag whose wire name and patch name disagree does not
        // compile.
        macro_rules! set {
            ($wire:ident, $field:ident) => {
                if let Some(v) = patch.$wire {
                    self.$field = v;
                    applied.push((stringify!($wire).to_string(), Value::Bool(v)));
                }
            };
        }
        set!(inject_first_turn_preamble, first_turn_preamble);
        set!(inject_compaction_restatement, compaction_restatement);
        set!(inject_plugin_changes, plugin_changes);
        set!(inject_system_notices, system_notices);
        set!(inject_application_headers, application_headers);
        set!(inject_thread_id, thread_id);
        set!(inject_context_id, context_id);
        set!(inject_message_id, message_id);
        set!(inject_app_idx, app_idx);
        set!(inject_timestamp, timestamp);
        set!(inject_duration, duration);
        set!(inject_queue_lead_in, queue_lead_in);
        // ── THE RETIRED KEYS, still accepted in a patch (see `InjectionWire`).
        // They are recorded under the NEW keys they moved, because that is what
        // actually changed and the audit event must not name a field this
        // struct no longer has.
        if patch.metadata_level == Some(LegacyMetadataLevel::None) {
            self.silence_message_metadata();
            for k in LEGACY_METADATA_KEYS {
                applied.push((k.to_string(), Value::Bool(false)));
            }
        }
        if patch.inject_context == Some(false) {
            *self = Self::NOTHING;
            applied.clear();
            for k in ALL_INJECTION_KEYS {
                applied.push((k.to_string(), Value::Bool(false)));
            }
        }
        applied
    }
}

/// Every wire key on [`Injection`], for the audit record of a retired-key patch.
const ALL_INJECTION_KEYS: &[&str] = &[
    "inject_first_turn_preamble",
    "inject_compaction_restatement",
    "inject_plugin_changes",
    "inject_system_notices",
    "inject_application_headers",
    "inject_thread_id",
    "inject_context_id",
    "inject_message_id",
    "inject_app_idx",
    "inject_timestamp",
    "inject_duration",
    "inject_queue_lead_in",
];

/// The subset `metadata_level: none` switched off.
const LEGACY_METADATA_KEYS: &[&str] = &[
    "inject_thread_id",
    "inject_context_id",
    "inject_message_id",
    "inject_app_idx",
    "inject_timestamp",
    "inject_duration",
    "inject_queue_lead_in",
];

/// `metadata_level`, as it still arrives from a settings blob written before
/// the flags existed. Never constructed by this crate and never serialized.
/// `Serialize` is derived only because [`InjectionWire`] is both directions;
/// the field is `skip_serializing_if = "Option::is_none"` and is ALWAYS `None`
/// on the way out, so no code path can write this enum to a blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyMetadataLevel {
    Standard,
    None,
}

/// **THE ONE DOOR THE RETIRED KEYS COME THROUGH.**
///
/// Threads on disk — and the AUI, which sends `inject_context: true` among its
/// ten keys and lives in another repository — still speak `inject_context` and
/// `metadata_level`. The migration chosen is **accept both, translate once,
/// store only the new keys**:
///
/// * it is the only option under which an existing thread cannot silently
///   change behaviour, which was the binding constraint;
/// * refusing would break the AUI, whose blob this crate cannot edit;
/// * and because this translation lives inside `Deserialize` rather than in a
///   helper somebody has to remember to call, there is no second door. Every
///   path that turns JSON into [`ThreadSettings`] — `threads/create`,
///   `threads.update`'s probe, and all five places a stored
///   `settings_json` is read back — goes through here.
///
/// **A thread whose stored settings say `metadata_level: none` keeps injecting
/// no metadata, forever.** The level translates to the seven line flags off;
/// the next time that thread's settings are written, the blob is rewritten with
/// those seven keys explicitly false and the retired key is gone. Nothing about
/// what the model sees moves at any point.
///
/// `inject_context: false` translates to [`Injection::NOTHING`] and is applied
/// AFTER the level, because it was the broader switch and that is the order
/// that reproduces the old composition.
/// # `inject_context` IS STILL WRITTEN, and that is not an oversight
///
/// It is one of the SIX keys in AUI's `AUI_OWNED_SETTINGS`, and AUI's settings
/// sync is a literal `serde_json::Value` comparison per key against the blob
/// `threads.get` hands back (`docs/aui-data-contract.md` §5.2, which spells out
/// the consequence: *"A daemon that normalised, reordered, defaulted-in, or
/// re-typed any of those six fields on read-back would put AUI into a patch
/// loop, sending the same `threads.update` on every single turn."*). AUI lives
/// in another repository and this crate cannot edit it.
///
/// So `inject_context` survives on the WIRE as a **derived mirror**: `false`
/// exactly when every flag is off, `true` otherwise. It is computed at
/// serialization, it is stored nowhere, nothing reads it to make a decision, and
/// it cannot drift from the flags because it has no independent existence. The
/// round trip is stable in both directions — `true` translates to "no change"
/// and `false` to [`Injection::NOTHING`], which is what it already meant.
///
/// **It is a compatibility shim with an owner.** It comes off the wire when
/// AUI's `AUI_OWNED_SETTINGS` drops it; that is an AUI-side change and it is
/// listed in `docs/aui-data-contract.md` §5.9.
///
/// `metadata_level` gets no such mirror and is simply GONE from the wire: it was
/// never one of AUI's six and no consumer diffs it.
#[derive(Debug, Serialize, Deserialize)]
struct InjectionWire {
    #[serde(default = "default_true")]
    inject_first_turn_preamble: bool,
    #[serde(default = "default_true")]
    inject_compaction_restatement: bool,
    #[serde(default = "default_true")]
    inject_plugin_changes: bool,
    #[serde(default = "default_true")]
    inject_system_notices: bool,
    #[serde(default = "default_true")]
    inject_application_headers: bool,
    #[serde(default = "default_true")]
    inject_thread_id: bool,
    #[serde(default)]
    inject_context_id: bool,
    #[serde(default = "default_true")]
    inject_message_id: bool,
    #[serde(default = "default_true")]
    inject_app_idx: bool,
    #[serde(default = "default_true")]
    inject_timestamp: bool,
    #[serde(default = "default_true")]
    inject_duration: bool,
    #[serde(default = "default_true")]
    inject_queue_lead_in: bool,
    // ── RETIRED AS A CONTROL ─────────────────────────────────────────────
    /// Accepted on the way in; written back as a DERIVED mirror on the way out,
    /// for AUI's diff. See this struct's doc comment.
    #[serde(default)]
    inject_context: Option<bool>,
    /// Accepted on the way in, NEVER written back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metadata_level: Option<LegacyMetadataLevel>,
}

impl From<Injection> for InjectionWire {
    fn from(i: Injection) -> Self {
        // EXHAUSTIVE, no `..`, for the same reason the other direction is.
        let Injection {
            first_turn_preamble,
            compaction_restatement,
            plugin_changes,
            system_notices,
            application_headers,
            thread_id,
            context_id,
            message_id,
            app_idx,
            timestamp,
            duration,
            queue_lead_in,
        } = i;
        Self {
            inject_first_turn_preamble: first_turn_preamble,
            inject_compaction_restatement: compaction_restatement,
            inject_plugin_changes: plugin_changes,
            inject_system_notices: system_notices,
            inject_application_headers: application_headers,
            inject_thread_id: thread_id,
            inject_context_id: context_id,
            inject_message_id: message_id,
            inject_app_idx: app_idx,
            inject_timestamp: timestamp,
            inject_duration: duration,
            inject_queue_lead_in: queue_lead_in,
            // THE DERIVED MIRROR. `false` exactly when nothing is injected, so
            // reading it back is a no-op in both directions.
            inject_context: Some(i != Injection::NOTHING),
            metadata_level: None,
        }
    }
}

impl From<InjectionWire> for Injection {
    fn from(w: InjectionWire) -> Self {
        // EXHAUSTIVE on purpose, with no `..`: adding a flag to `Injection`
        // without adding it here is a COMPILE error, not a silently defaulted
        // field. This is the whole drift guard and it needs no test.
        let InjectionWire {
            inject_first_turn_preamble,
            inject_compaction_restatement,
            inject_plugin_changes,
            inject_system_notices,
            inject_application_headers,
            inject_thread_id,
            inject_context_id,
            inject_message_id,
            inject_app_idx,
            inject_timestamp,
            inject_duration,
            inject_queue_lead_in,
            inject_context,
            metadata_level,
        } = w;
        let mut out = Injection {
            first_turn_preamble: inject_first_turn_preamble,
            compaction_restatement: inject_compaction_restatement,
            plugin_changes: inject_plugin_changes,
            system_notices: inject_system_notices,
            application_headers: inject_application_headers,
            thread_id: inject_thread_id,
            context_id: inject_context_id,
            message_id: inject_message_id,
            app_idx: inject_app_idx,
            timestamp: inject_timestamp,
            duration: inject_duration,
            queue_lead_in: inject_queue_lead_in,
        };
        if metadata_level == Some(LegacyMetadataLevel::None) {
            out.silence_message_metadata();
        }
        if inject_context == Some(false) {
            out = Injection::NOTHING;
        }
        out
    }
}

/// A partial update to [`Injection`]. Flattened into [`SettingsPatch`], so the
/// patch keys are the same top-level `inject_*` names the settings blob uses.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct InjectionPatch {
    pub inject_first_turn_preamble: Option<bool>,
    pub inject_compaction_restatement: Option<bool>,
    pub inject_plugin_changes: Option<bool>,
    pub inject_system_notices: Option<bool>,
    pub inject_application_headers: Option<bool>,
    pub inject_thread_id: Option<bool>,
    pub inject_context_id: Option<bool>,
    pub inject_message_id: Option<bool>,
    pub inject_app_idx: Option<bool>,
    pub inject_timestamp: Option<bool>,
    pub inject_duration: Option<bool>,
    pub inject_queue_lead_in: Option<bool>,
    /// Retired; still accepted so a consumer patching the old key is not
    /// broken. Translated exactly as [`InjectionWire`] translates it.
    pub inject_context: Option<bool>,
    /// Retired; still accepted. See [`InjectionWire`].
    pub metadata_level: Option<LegacyMetadataLevel>,
}

/// The `--effort` levels the CLI accepts (`claude --help`, 2026-10-15:
/// "low, medium, high, xhigh, max"). A spawn naming anything else is refused
/// `malformed` before its commit point (R4.S8), so a typo is the caller's
/// answer and never a child that crashes on its first turn.
pub const EFFORT_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// Every thread is associated with a budget; one that names none inherits the
/// fleet-wide default (INTENT 72b). See [`ThreadSettings::budget_key`].
fn default_budget_key() -> Option<String> {
    Some(crate::budgets::DEFAULT_BUDGET_KEY.to_string())
}

impl ThreadSettings {
    /// A minimal valid settings blob (tests, imports).
    pub fn new(cwd: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            cwd: cwd.into(),
            model: model.into(),
            account: AccountChoice::default(),
            append_system_prompt: None,
            add_dirs: Vec::new(),
            permission_mode: None,
            tools: ToolConfig::default(),
            fallback_model: None,
            max_budget_usd: None,
            agent: None,
            effort: None,
            plugin_roots: Vec::new(),
            plugin_dirs: Vec::new(),
            retry_policy: RetryPolicy::default(),
            budget_key: default_budget_key(),
            thresholds: crate::budgets::Thresholds::default(),
            budgeting_off: false,
            limit_kill_mid_turn: true,
            scrub_api_error_details: true,
            injection: Injection::default(),
            app_id: None,
            resume_after_compaction: false,
            projection: Projection::default(),
            projection_allow_loss: Vec::new(),
            metadata: Value::Null,
        }
    }

    /// **THE CREATOR DEFAULT for the context id** — applied once, at thread
    /// creation, from the raw settings blob the creator sent.
    ///
    /// The operator asked for the context id *"set to true for AUI and TUI
    /// consumers"*, and then narrowed it himself: *"I mean threads started from
    /// AUI and TUI."* So this is PER-THREAD, decided by the CREATOR, and not a
    /// per-consumer override. What it writes is ordinary thread state from that
    /// moment on — a `threads.update` can turn it off and the thread stays off,
    /// because injection changes what the MODEL sees and that is thread state,
    /// not a property of whoever happens to be looking at the thread.
    ///
    /// `raw` is the blob as SENT, because the question this asks cannot be asked
    /// of a deserialized [`Injection`]: a flag that arrived `false` explicitly
    /// and a flag that never arrived are the same `false` by then. Reading key
    /// PRESENCE off the JSON is what keeps *"the creator said nothing"*
    /// distinguishable from *"the creator said no"* — so an explicit
    /// `inject_context_id: false` from the AUI is honoured, not overridden.
    ///
    /// # It is also the BACKFILL, and that is deliberate
    ///
    /// Called from [`Self::from_stored`] as well as from the create door, it
    /// fires on exactly those threads whose stored blob predates the flag —
    /// which is every thread the AUI has ever made. The operator will see the
    /// context id appear on his EXISTING AUI threads, not only on new ones, and
    /// it appears without waiting for a compaction, because of the catch-up in
    /// `ThreadManager::build_prompt`.
    ///
    /// It is SELF-RETIRING: the first time a thread's settings are written back,
    /// the blob carries `inject_context_id` explicitly, the key is present, and
    /// this never fires for that thread again. No migration script, no daemon
    /// restart, and no second writer of settings.
    pub fn apply_creator_defaults(&mut self, raw: &Value) {
        let said_so = raw.get("inject_context_id").is_some_and(|v| !v.is_null());
        if said_so {
            return;
        }
        if self
            .app_id
            .as_deref()
            .is_some_and(|a| Injection::CONTEXT_ID_BY_DEFAULT_FOR.contains(&a))
        {
            self.injection.context_id = true;
        }
    }

    /// **THE ONE WAY A STORED SETTINGS BLOB BECOMES A `ThreadSettings`.**
    ///
    /// Deserializes — which is where the retired `inject_context` /
    /// `metadata_level` keys are translated, see [`InjectionWire`] — and then
    /// applies [`Self::apply_creator_defaults`], so a thread written before the
    /// context-id flag existed is backfilled on read rather than by a migration
    /// that would be a second writer of settings.
    ///
    /// Every `settings_json` read in this crate goes through here. A bare
    /// `serde_json::from_str::<ThreadSettings>` would still translate the retired
    /// keys — that part is inside `Deserialize` and cannot be skipped — but
    /// would miss the backfill.
    pub fn from_stored(settings_json: &str) -> serde_json::Result<Self> {
        let raw: Value = serde_json::from_str(settings_json)?;
        let mut out: Self = serde_json::from_value(raw.clone())?;
        out.apply_creator_defaults(&raw);
        Ok(out)
    }

    /// Apply a partial update; returns the JSON patch actually applied (for
    /// the `SettingsChanged` audit event).
    pub fn apply(&mut self, patch: SettingsPatch) -> Value {
        let mut applied = serde_json::Map::new();
        macro_rules! set {
            ($field:ident) => {
                if let Some(v) = patch.$field {
                    applied.insert(
                        stringify!($field).to_string(),
                        serde_json::to_value(&v).unwrap_or(Value::Null),
                    );
                    self.$field = v;
                }
            };
        }
        set!(cwd);
        set!(model);
        set!(account);
        set!(append_system_prompt);
        set!(add_dirs);
        set!(permission_mode);
        set!(tools);
        set!(fallback_model);
        set!(max_budget_usd);
        set!(agent);
        set!(effort);
        set!(plugin_roots);
        set!(plugin_dirs);
        set!(retry_policy);
        set!(budget_key);
        set!(thresholds);
        set!(budgeting_off);
        // Both of these were declared "settable per thread over
        // `threads.update`" in their own doc comments while being absent from
        // this macro list, which made the doc comment a lie: the field
        // deserialized, defaulted, and could never afterwards be changed.
        // `limit_kill_mid_turn` shipped that way in `df4ae3a`; wiring it here
        // is a one-line correction, not a behaviour change — its default is
        // untouched.
        set!(limit_kill_mid_turn);
        set!(scrub_api_error_details);
        // ── THE INJECTION FLAGS, whose `set!` list lives on `Injection` ──
        // One call, twelve flags, and the applied map is extended with the wire
        // keys that actually moved. A flag missing from `Injection::apply` is
        // the same old bug this macro list has shipped twice; `context_id.rs`
        // keeps the forcing function.
        for (k, v) in self.injection.apply(patch.injection) {
            applied.insert(k, v);
        }
        set!(app_id);
        set!(resume_after_compaction);
        // The projection switch and its loss allowance — both on this list
        // from the day they shipped, because the list has twice shipped a
        // field short and `tests/projected_turn.rs` keeps the forcing function.
        set!(projection);
        set!(projection_allow_loss);
        set!(metadata);
        Value::Object(applied)
    }
}

/// A partial settings update: every field optional; absent = unchanged.
/// Option-typed settings fields use an inner Option-in-Value: passing
/// `"append_system_prompt": null` clears the value.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SettingsPatch {
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub account: Option<AccountChoice>,
    #[serde(default, deserialize_with = "some_nullable")]
    pub append_system_prompt: Option<Option<String>>,
    pub add_dirs: Option<Vec<String>>,
    #[serde(default, deserialize_with = "some_nullable")]
    pub permission_mode: Option<Option<String>>,
    pub tools: Option<ToolConfig>,
    #[serde(default, deserialize_with = "some_nullable")]
    pub fallback_model: Option<Option<String>>,
    #[serde(default, deserialize_with = "some_nullable")]
    pub max_budget_usd: Option<Option<f64>>,
    /// `"agent": null` clears it (R4.S8).
    #[serde(default, deserialize_with = "some_nullable")]
    pub agent: Option<Option<String>>,
    /// `"effort": null` clears it (R4.S8).
    #[serde(default, deserialize_with = "some_nullable")]
    pub effort: Option<Option<String>>,
    pub plugin_roots: Option<Vec<String>>,
    pub plugin_dirs: Option<Vec<String>>,
    pub retry_policy: Option<RetryPolicy>,
    /// `"budget_key": null` UNKEYS the thread (no key ⇒ no budget, DEC-4);
    /// an absent key leaves the assignment alone.
    #[serde(default, deserialize_with = "some_nullable")]
    pub budget_key: Option<Option<String>>,
    pub thresholds: Option<crate::budgets::Thresholds>,
    /// Round-7 item 1: the per-thread budgets bypass. `true` runs this thread
    /// with the budgets module out of the loop entirely.
    pub budgeting_off: Option<bool>,
    /// The mid-turn usage-limit interrupt (`df4ae3a`). Present here because its
    /// own doc comment promises it is patchable.
    pub limit_kill_mid_turn: Option<bool>,
    /// The per-thread API-error scrub. `false` leaves vendor error prose on the
    /// transcript, which is the state that produced the 2026-08-14 incident —
    /// see [`ThreadSettings::scrub_api_error_details`] before turning it off.
    pub scrub_api_error_details: Option<bool>,
    /// **ONE FLAG PER INJECTED THING**, every one live-patchable, plus the two
    /// retired keys still accepted for translation. Flattened, so a patch names
    /// the same top-level `inject_*` keys the settings blob does. See
    /// [`Injection`]; there is nothing to keep coherent, so there is no
    /// validator at either door any more.
    #[serde(flatten)]
    pub injection: InjectionPatch,
    #[serde(default, deserialize_with = "some_nullable")]
    pub app_id: Option<Option<String>>,
    /// **The post-compaction ending**, live-patchable like every other flag —
    /// `true` lets this thread resume after an automatic compaction, `false`
    /// (the default) ends the turn at the boundary. See
    /// [`ThreadSettings::resume_after_compaction`] before turning it on.
    pub resume_after_compaction: Option<bool>,
    /// **The per-thread projection switch**, live-patchable. See
    /// [`ThreadSettings::projection`] — `"projected"` turns the ledger-rendered
    /// native file on for this thread, `"native_owned"` turns it back off.
    pub projection: Option<Projection>,
    /// The render losses this thread accepts by name. See
    /// [`ThreadSettings::projection_allow_loss`].
    pub projection_allow_loss: Option<Vec<String>>,
    pub metadata: Option<Value>,
}

/// Deserialize a present-but-possibly-null field as `Some(Option<T>)` so a
/// JSON `null` clears the setting while an absent key leaves it unchanged.
fn some_nullable<'de, T, D>(de: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<T>::deserialize(de)?))
}
