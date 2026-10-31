//! Retry classification — the union of CCW's and AUI's classifiers (Q5:
//! "retries owned by threads ENTIRELY").
//!
//! # THE RULE (2026-08-25): a user interruption ends the delivery. Nothing else
//! does.
//!
//! Everything in this module is downstream of one operator ruling:
//!
//! > *"I want to retry after all events that are not user interruptions."*
//!
//! So there is no longer any such thing as a "non-retryable class" here. Every
//! verdict this module can produce lands on a bounded ladder — see
//! [`ladder_for`], which is TOTAL, and whose totality IS the rule. The only
//! things that end a delivery are the interrupt triggers that name an
//! INTENTION — [`crate::event::InterruptTrigger::Operator`] and
//! [`crate::event::InterruptTrigger::Compaction`] — and that is asserted in
//! exactly one place, [`ends_the_delivery`]. No GATE ever ends one.
//!
//! Ported from CCW `retry.rs` with the gaps closed:
//! - **`CreditBalance`** (G9): AUI speaks a specific message for an exhausted
//!   credit balance and correctly never retries it; CCW classified it as an
//!   anonymous `Fatal`. It is a first-class class here.
//!
//!   **The "never retries it" half of that is SUPERSEDED as of 2026-08-25 and
//!   must not be restored from this comment.** See [`RetryDecision::CreditBalance`].
//! - **AUI's substring patterns merged in** (`502`, `timed out`, `timeout`,
//!   `connection reset`, `temporarily`, `too many requests`, `session limit`,
//!   `usage limit`) so nothing AUI caught regresses.
//! - **`backoff_ms(attempt)` is actually driven by the attempt** — CCW
//!   computed an exponential ladder and then called it with a literal `1`
//!   (§9.4: "not a backoff at all"). The runtime here passes the real attempt.

use serde::{Deserialize, Serialize};

/// What kind of transient failure was seen (drives the wait strategy).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransientKind {
    /// 429 rate/session/weekly limit — wait for reset OR fail over accounts
    /// per the thread's retry policy.
    RateLimit,
    /// 5xx / overload — bounded exponential backoff.
    Overload,
    /// **The API itself answered with a server error** — the CLI's named
    /// `server_error`, or a bare 5xx status on an API-error record.
    ///
    /// Its OWN class, on a SMALL budget ([`MAX_SERVER_ERROR_ATTEMPTS`] attempts
    /// or [`SERVER_ERROR_BUDGET_SECS`] of wall-clock, whichever ends first),
    /// and above all NOT [`Self::Connectivity`]. Until 2026-10-14 a
    /// `server_error` whose prose mentioned the network (`Can't reach the API
    /// server … (ENOTFOUND)`) was read as a lost network and given the
    /// connectivity ladder — forty attempts. Thread 60f0380d took all forty,
    /// and every one of them re-ran about three minutes of real subagent work
    /// before the API refused it again: $1,212.87 over 2½ days. A server error
    /// arrives AFTER a turn has done work, so each retry re-buys that work; a
    /// lost network fails before anything runs. Those two cost profiles cannot
    /// share a budget.
    ServerError,
    /// **The network itself failed** — no route to Anthropic at all.
    ///
    /// DNS failure, TLS failure, connection refused, connection reset, no
    /// route to host, a timeout, a request that never got out. Intent item 73
    /// (msg #2166): *"I can't let these threads just experience a network error
    /// and go down."*
    ///
    /// This is a DIFFERENT ANIMAL from a usage limit and is treated as one:
    ///
    /// - It has **no reset time**. Nothing can be parsed out of it to wait for.
    /// - It is **not the account's fault**, so failing over is pointless — the
    ///   network is not account-specific, and moving would spend a placement to
    ///   arrive at the same broken socket. The runtime therefore schedules a
    ///   connectivity retry with no failover hint, on the SAME account.
    /// - It must **not consume the usage-limit ladder**, and a usage limit must
    ///   not consume this one; they are separate classes with separate ceilings.
    ///
    /// Its `kind_str` is its own tag — `"connectivity"`, not the generic
    /// `"transient"` — so the AUI can tell "this thread is waiting on the
    /// network" apart from "this thread is working" without guessing. That tag
    /// is application metadata and never reaches the model (DEC-62).
    Connectivity,
    /// Connection drop / timeout — bounded exponential backoff.
    ///
    /// SUPERSEDED as a classification target by [`Self::Connectivity`], which
    /// is what `classify_message` now returns for every wire-level failure.
    /// The variant is retained because it is persisted in `retries.kind` on
    /// existing rows and must still deserialize; it takes the same path as
    /// `Connectivity` everywhere it is matched.
    ConnectionDrop,
    /// `result` subtype `error_during_execution` — retry, bounded.
    ErrorDuringExecution,
    /// **A failure that LOOKS deterministic — the old `Fatal`, now on a short
    /// ladder instead of on the floor.**
    ///
    /// A bad model name, a permission refusal, an unrecognized error string.
    /// Retrying one of these is not expected to help, and an UNBOUNDED retry on
    /// it is a token furnace: it would spawn a real `claude` process every few
    /// seconds forever to be refused in the identical way.
    ///
    /// It is retried anyway, a handful of times, for one reason: **this class is
    /// a guess, not a fact.** [`classify_message`]'s `Fatal` arm is literally
    /// `unclassified error: {text}` — it is what the classifier says when it
    /// recognizes nothing, which is precisely when it is least entitled to be
    /// certain. Every genuinely-transient failure that this crate has ever
    /// mis-shelved arrived here first (the connectivity block's comment is a
    /// list of six of them). So the ladder is sized to be cheap enough that
    /// being wrong about a truly deterministic failure costs six spawns over
    /// about eighteen minutes, and generous enough that being wrong in the OTHER
    /// direction — a transient failure we failed to name — is survivable.
    ///
    /// Its `kind_str` is still `"claude_error"`, deliberately: the wire
    /// vocabulary clients already parse does not change because the internal
    /// disposition did.
    Deterministic,
    /// **The account's credit balance is exhausted** — the account is barred,
    /// the TURN is retried.
    ///
    /// See [`RetryDecision::CreditBalance`] for the ruling. The distinction that
    /// makes this a retry rather than an ending: an exhausted balance is a fact
    /// about ONE account, and the thread can be somewhere else. Its `kind_str`
    /// stays `"credit_balance"` so the AUI still speaks its specific message.
    CreditBalance,
    /// **This login's authentication has expired** — the account is barred, the
    /// TURN is retried elsewhere, and the operator is asked to re-authenticate.
    ///
    /// The sibling of [`Self::CreditBalance`] and deliberately shaped like it:
    /// both are "THIS account cannot serve, another might", and both end in a
    /// state only a human can leave. See [`RetryDecision::LoginExpired`].
    ///
    /// It is its OWN class rather than a flavour of [`Self::Deterministic`]
    /// because of what that misfiling cost. Until now an expired login had no
    /// class at all: through the stderr door it landed on `Deterministic`
    /// (`unclassified error: …`), and through the errored-result door it landed
    /// on [`Self::ErrorDuringExecution`] — a three-attempt, ten-second ladder
    /// that named the failure `"transient"`, barred nothing, moved nothing, and
    /// then quoted the raw CLI text at the operator. That is the 2026-08-26
    /// incident, and both doors are closed by giving the failure a name.
    LoginExpired,
}

impl TransientKind {
    /// The stable `error_kind` tag AUI persists and speaks
    /// (`ClaudeErrorClass::kind_str` compatibility).
    pub fn kind_str(&self) -> &'static str {
        match self {
            TransientKind::RateLimit => "usage_limit",
            // Its OWN tag, and this is what the AUI reads to say "waiting on
            // the network" rather than "working" (intent item 73). A thread
            // that has lost the network must be distinguishable from a thread
            // that is thinking; before this they were both "transient".
            TransientKind::Connectivity | TransientKind::ConnectionDrop => "connectivity",
            TransientKind::Overload | TransientKind::ErrorDuringExecution => "transient",
            // Its own tag, so a server-side refusal is distinguishable from both
            // "the network is gone" and a generic transient.
            TransientKind::ServerError => "server_error",
            // The two classes that used to be terminal keep the EXACT wire tags
            // their terminal `TurnFailed` events carried, so no client learns a
            // new word because the daemon changed its mind about retrying.
            TransientKind::Deterministic => "claude_error",
            TransientKind::CreditBalance => "credit_balance",
            // A NEW wire tag, and the only one this wave adds. Wave one kept
            // `credit_balance` and `claude_error` unchanged because the AUI
            // already spoke both; there is nothing to preserve here, because an
            // expired login has never had a tag of its own — it borrowed
            // `transient` and `claude_error`, which are the two words that made
            // it unspeakable. This tag is the surface that carries "he must
            // re-authenticate": one named class the AUI can say a sentence
            // about, instead of quoting a credential error at him.
            TransientKind::LoginExpired => "login_expired",
        }
    }

    /// Is this the network being down, as opposed to the service refusing us?
    ///
    /// The one predicate the runtime branches on to choose the connectivity
    /// ladder and to suppress failover. Kept here rather than spelled out at
    /// the call site so the two persisted variants can never drift apart.
    pub fn is_connectivity(&self) -> bool {
        matches!(
            self,
            TransientKind::Connectivity | TransientKind::ConnectionDrop
        )
    }
}

/// The retry decision for an observed failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetryDecision {
    /// Transient — retry (usage limits indefinitely, others bounded).
    Retry { kind: TransientKind, reason: String },
    /// The resume id expired — the CLI found no conversation to resume.
    ///
    /// **NOT a same-resume retry, and NOT an unconditional fresh start either.**
    /// See `ThreadManager::restart_fresh_is_safe`: a fresh start re-mints the
    /// SAME session id (`spawn.rs` passes `--session-id <thread_id>`), so doing
    /// it while a real transcript still exists would strand that history under
    /// an id now pointing at a new conversation. The runtime checks first.
    RestartFresh { reason: String },
    /// **Account is out of credit.** The ACCOUNT is barred from placement; the
    /// TURN is retried elsewhere.
    ///
    /// # This reverses a deliberate decision, on the operator's ruling
    ///
    /// This variant's doc used to read *"needs a human top-up; never retried
    /// (G9)"*, and the module header called it *"a first-class non-retryable
    /// class"*. **That is SUPERSEDED as of 2026-08-25 and must not be restored
    /// from this comment.** Asked about credit-balance exhaustion directly, the
    /// operator ruled:
    ///
    /// > *"credit limit exhaustion means that that account is no longer open for
    /// > placement, but that means that another account might be open for
    /// > placement. So it does deserve a retry."*
    ///
    /// The original decision was not wrong about the FACT — an exhausted balance
    /// really does need a human top-up, and no amount of retrying on that
    /// account will produce money. It was wrong about the SCOPE. It treated a
    /// fact about one account as a fact about the delivery, and so it threw away
    /// the operator's words to punish an account. G9's real content survives
    /// intact and is now expressed where it belongs: the account is barred (see
    /// `ThreadManager::note_account_credit_exhausted`), and the bar is what stops
    /// the pointless retrying — not the destruction of the turn.
    CreditBalance { reason: String },
    /// **This login's authentication has expired.** The ACCOUNT is barred; the
    /// TURN is retried elsewhere; the operator is asked — once, on the status
    /// surface — to re-authenticate.
    ///
    /// # Why this is the credit shape and not a new one
    ///
    /// The operator's ruling about credit is about SCOPE, and it transfers
    /// verbatim:
    ///
    /// > *"credit limit exhaustion means that that account is no longer open for
    /// > placement, but that means that another account might be open for
    /// > placement. So it does deserve a retry."*
    ///
    /// An expired login is the same category of fact — true of ONE login, false
    /// of the fleet — and the same error was being made about it, only worse.
    /// Credit at least had a class. This had none.
    ///
    /// # Where it differs from credit, and it is the one thing worth thinking
    /// about
    ///
    /// A credit bar ends when a human tops the account up. A login bar ends when
    /// a human re-authenticates. Both are invisible to the daemon, so both bars
    /// are RE-PROBE INTERVALS rather than predictions — see [`LOGIN_BAR_SECS`],
    /// which is deliberately SHORTER than [`CREDIT_BAR_SECS`] rather than
    /// longer, for a reason that is easy to get backwards.
    ///
    /// The tempting move is to make this bar permanent, because unlike a credit
    /// balance an expired login provably will not heal on its own. That flag
    /// already exists — `AccountState::dead_since` — and it is NOT chosen here.
    /// The reason is not that it is broken. It is that it answers a different
    /// question, and the two answers cannot both be given.
    ///
    /// A dead-flagged login is barred as a placement TARGET while the thread
    /// already on it STAYS (DEC-26: moving a thread with history costs its
    /// conversation) and keeps taking its turns there. That thread's eventual
    /// success is the only thing that clears the flag, which is why
    /// `note_login_health` has a success branch at all and why `unavailable` is
    /// deliberately left empty for it. Coherent — and the exact opposite of what
    /// was asked for:
    ///
    /// > *"bars an account for a while … retry the turn elsewhere"*
    ///
    /// Once the turn MOVES, the dead flag loses its only clearer: no thread is
    /// left on the account to succeed there, and `accounts::placeable_pool`
    /// refuses to send a new one. It would then latch for good. So the two
    /// mechanisms are mutually exclusive for this class, and the ruling picks the
    /// one that moves the turn — `limit_until`, which expires on a clock AND is
    /// cleared by any completed turn on the login.
    ///
    /// See [`crate::accounts::is_dead_login_error`], which this reuses as its
    /// detection predicate rather than inventing a second one.
    LoginExpired { reason: String },
    /// Deterministic — surface the error.
    ///
    /// **No longer terminal.** It lands on [`TransientKind::Deterministic`]'s
    /// short ladder; see that variant for why a class whose own reason string is
    /// `unclassified error: …` has not earned the right to end a delivery.
    Fatal { reason: String },
}

impl RetryDecision {
    /// The stable `error_kind` tag for terminal `TurnFailed` events.
    pub fn kind_str(&self) -> &'static str {
        match self {
            RetryDecision::Retry { kind, .. } => kind.kind_str(),
            RetryDecision::RestartFresh { .. } => "transient",
            RetryDecision::CreditBalance { .. } => "credit_balance",
            RetryDecision::LoginExpired { .. } => "login_expired",
            RetryDecision::Fatal { .. } => "claude_error",
        }
    }
}

/// **THE RULE. An INTENTION ends the delivery. A gate never does.**
///
/// > *"I want to retry after all events that are not user interruptions."*
/// > — the operator, 2026-08-25
///
/// > *"I just want it to stop. I want it to end. I want the turn to end. It
/// > should not keep going on its own."*
/// > — the operator, 2026-08-28, on resuming after a compaction
///
/// Two triggers answer `true`, and the second one is why this doc no longer
/// reads "a user interruption… nothing else does". A standing instruction given
/// once and applied when it becomes relevant is still his intention; what the
/// rule has always excluded is a GATE — the system refusing — because the retry
/// ladder exists precisely to catch a refusal.
///
/// This function is the whole of that rule, and it is deliberately a function
/// rather than a match arm somewhere in `run_turn`. Before this, "what ends a
/// delivery" was an EMERGENT PROPERTY of which `RetryDecision` variants happened
/// to fall through to a terminal arm — three of them did (`Fatal`,
/// `CreditBalance`, `RestartFresh`-exhausted), not because anyone had decided
/// those three should end a turn but because that arm was where unhandled
/// variants landed. A rule nobody can point at is a rule that drifts every time
/// someone adds a variant.
///
/// The discrimination it needs already existed and is not re-invented here:
/// [`InterruptTrigger`] already separates the triggers that name an INTENTION
/// from the ones that name a GATE, and says so in its own doc comments. This
/// reads that distinction; it does not add a second classifier beside it. That
/// is also why adding `Compaction` cost one arm and no new concept.
///
/// The match is EXHAUSTIVE with no wildcard arm, on purpose. A new interrupt
/// trigger must not be able to acquire a policy by default — adding one breaks
/// this build and forces whoever added it to say which side it is on.
pub fn ends_the_delivery(trigger: crate::event::InterruptTrigger) -> bool {
    use crate::event::InterruptTrigger as T;
    match trigger {
        // The operator asked for it.
        T::Operator => true,
        // He also asked for THIS, once, in advance: a thread that has just been
        // through an automatic compaction and is not permitted to resume through
        // one stops there. It sits on this side of the line for the same reason
        // `Operator` does — it names an INTENTION, not a gate. Nothing refused
        // the turn, so there is nothing for a ladder to catch, and re-spawning
        // would recreate precisely the post-compaction momentum the setting
        // exists to prevent.
        //
        // Ending the DELIVERY is not abandoning his words: the messages this
        // turn claimed are already on the ledger with their work, and anything
        // still queued was never claimed and goes out on the next turn.
        T::Compaction => true,
        // Every one of these names a GATE, not an intention. A gate closing is
        // the system refusing, and the whole point of the retry ladder is to
        // catch a refusal — so a refusal must never be able to end the delivery
        // that it refused.
        T::DaemonRestart
        | T::SessionLimit
        | T::WeeklyLimit
        | T::FableLimit
        | T::Threshold
        | T::HardBudget => false,
    }
}

/// **Every verdict lands on a ladder. This function is TOTAL, and its totality
/// IS the rule.**
///
/// There is no `Option` here and no `None` arm to add one to. If you are holding
/// a [`RetryDecision`], you are holding something that gets retried; the only
/// question this answers is *on which ladder*, and the answer is never "none".
///
/// [`RetryDecision::RestartFresh`] is the one verdict that is not PURELY a
/// ladder — it also carries a recovery ACTION (start a new conversation) that
/// the runtime must decide about first. It still has a ladder, because that
/// recovery can be refused, and when it is refused the turn must still be
/// retried rather than dropped.
pub fn ladder_for(decision: &RetryDecision) -> (TransientKind, String) {
    match decision {
        RetryDecision::Retry { kind, reason } => (kind.clone(), reason.clone()),
        RetryDecision::CreditBalance { reason } => (TransientKind::CreditBalance, reason.clone()),
        RetryDecision::LoginExpired { reason } => (TransientKind::LoginExpired, reason.clone()),
        RetryDecision::Fatal { reason } => (TransientKind::Deterministic, reason.clone()),
        // A refused / already-spent fresh start is not a session problem any
        // more, it is an unexplained failure — which is exactly what the
        // deterministic ladder is for.
        RetryDecision::RestartFresh { reason } => (TransientKind::Deterministic, reason.clone()),
    }
}

/// **The ceiling for a DETERMINISTIC failure — the ladder that did not exist.**
///
/// Six. Sized against the token-furnace hazard rather than against any hope that
/// the sixth attempt succeeds: a bad model name will be a bad model name six
/// times. What six buys is the ability to be WRONG about the classification —
/// and [`classify_message`]'s `Fatal` arm is the classifier admitting it
/// recognized nothing, which is the least reliable verdict it produces.
///
/// Six attempts on [`deterministic_backoff_secs`] is six `claude` spawns spread
/// over about eighteen minutes. Compare the alternatives it sits between: three
/// (`max_attempts`) over seven seconds is not enough time for anything external
/// to recover, and unbounded is the 924-spawn pathology
/// [`DEFAULT_MAX_USAGE_LIMIT_ATTEMPTS`] was written to stop.
pub const DEFAULT_MAX_DETERMINISTIC_ATTEMPTS: u32 = 6;

/// Backoff (seconds) for a DETERMINISTIC retry — 5, 15, 45, 120, 300, then 600.
///
/// Climbs much faster than the transient ladder and much further, because the
/// prior here is the opposite one. A transient failure is EXPECTED to clear in
/// seconds, so its ladder is dense and short. This one is expected NOT to clear
/// at all, so every rung has to justify a whole spawn: the gaps widen quickly so
/// that six attempts cover a useful span of wall-clock time instead of six
/// pointless spawns in the same half-minute.
pub fn deterministic_backoff_secs(attempt: u32) -> u64 {
    match attempt {
        0 | 1 => 5,
        2 => 15,
        3 => 45,
        4 => 120,
        5 => 300,
        _ => 600,
    }
}

/// **The ceiling for a CREDIT-BALANCE retry.**
///
/// Six, and it is a bound on SPAWNS, not on patience — the same distinction
/// [`DEFAULT_MAX_USAGE_LIMIT_ATTEMPTS`] draws. The wait is owned by the account
/// bar (see `ThreadManager::note_account_credit_exhausted`), and while every
/// account is barred the thread sits in the admission gate rather than climbing
/// this ladder at all.
///
/// So each rung here represents a genuine re-attempt on some account the gate
/// believed was open, that then reported no credit. Six of those is six
/// different chances for placement to find money. Beyond that the honest
/// diagnosis is not "try again", it is "the operator's accounts are out of
/// credit", and that is a thing he must be TOLD, loudly, with his prompt intact.
pub const DEFAULT_MAX_CREDIT_ATTEMPTS: u32 = 6;

/// Backoff (seconds) for a credit-balance retry — 30, 60, 120, 300, then 600.
///
/// The account bar does the real waiting; this only stops a thread with several
/// registered logins from burning all of them in one tight loop.
pub fn credit_backoff_secs(attempt: u32) -> u64 {
    match attempt {
        0 | 1 => 30,
        2 => 60,
        3 => 120,
        4 => 300,
        _ => 600,
    }
}

/// **How long an account stays barred after reporting an exhausted credit
/// balance — one hour.**
///
/// This is a RE-PROBE INTERVAL, not a prediction. Unlike every other bar written
/// to `accounts.limit_until`, a credit exhaustion has no reset prose and no
/// natural reset time at all: it ends when a human tops the account up, and
/// nothing in the daemon can observe that happening.
///
/// So the bar cannot be "until it is fixed" — it has to expire and let one turn
/// through to find out, and `note_account_served` clears it the moment one
/// succeeds. That makes the account's own recovery the thing that proves the
/// recovery, which is the same argument [`connectivity_backoff_secs`] makes for
/// preferring a short ceiling over a reachability probe.
///
/// One hour, because the asymmetry runs the same way `MAX_PARSED_RESET`'s does
/// and the note there applies verbatim: *a window that is too SHORT costs one
/// extra refusal; a window that is too LONG costs the account until the clock
/// runs out.* Too short here costs one refused spawn per account per hour. Too
/// long means the operator tops up his account and the daemon keeps refusing to
/// use it for hours afterwards, with no way to say so — which is the 2026-08-15
/// year-long-bar incident in miniature.
pub const CREDIT_BAR_SECS: i64 = 3600;

/// **The ceiling for a LOGIN-EXPIRED retry.** Six, exactly as credit, and for
/// the same reason: it bounds SPAWNS, not patience.
///
/// The account bar does the waiting. Each rung here is a genuine re-attempt on
/// some account the admission gate believed was authenticated, that then
/// reported it was not. Six of those is six chances to find a live login. Past
/// that the honest diagnosis is not "try again", it is "every login this turn
/// could reach needs re-authenticating", and that is the sentence the give-up
/// carries — with his prompt intact and re-selectable.
pub const DEFAULT_MAX_LOGIN_ATTEMPTS: u32 = 6;

/// Backoff (seconds) for a login-expired retry — 30, 60, 120, 300, then 600.
///
/// Identical to [`credit_backoff_secs`] and for the identical reason: the
/// account bar owns the real waiting, and this only stops a thread with several
/// registered logins from burning all of them in one tight loop. Kept as its own
/// function rather than aliased so the two can be tuned apart without one
/// silently moving the other.
pub fn login_backoff_secs(attempt: u32) -> u64 {
    match attempt {
        0 | 1 => 30,
        2 => 60,
        3 => 120,
        4 => 300,
        _ => 600,
    }
}

/// **How long a login stays barred after failing to authenticate — thirty
/// minutes. Half the credit bar, and the direction is the point.**
///
/// # Why it expires at all, when the failure provably will not heal
///
/// This is the objection to answer, because an expired OAuth session is not
/// like an exhausted balance in one important way: nothing about the passage of
/// time fixes it. It needs a human at a browser. So a bar that silently expires
/// and lets the turn try again looks like a machine pretending a human acted.
///
/// It expires anyway, and the argument is a consequence of MOVING THE TURN.
/// `AccountState::dead_since` is the permanent version of this bar, and it works
/// only because it does not move anything: the thread stays put (DEC-26), keeps
/// taking turns on the flagged login, and its eventual success is what clears
/// the flag. The operator asked for the opposite — the turn goes elsewhere — and
/// once it does, that clearer is gone: no thread remains on the account, and
/// `placeable_pool` will not send a new one. A permanent bar downstream of a
/// failover therefore does not express "this needs a human"; it expresses "this
/// account is gone until someone edits the database", and it would take his
/// fleet down one login at a time with no way back. Boot's `refresh_identities`
/// re-reads every config dir and would not clear it either.
///
/// So the bar expires, one turn probes, and the account's own success is what
/// proves the account is back — the same argument [`CREDIT_BAR_SECS`] and
/// [`connectivity_backoff_secs`] both make, and it needs no new mechanism:
/// `note_account_served` already clears the window on every label of a login the
/// moment a turn completes on it.
///
/// # Why THIRTY minutes, when credit gets sixty
///
/// The instinct is that a bar which needs a human should be LONGER than one
/// which needs a human. It should be shorter, and the asymmetry is about what a
/// re-probe COSTS.
///
/// A credit re-probe is expensive and its cost is unbounded on the wrong side: a
/// topped-up account answers the turn, so the probe may spend a full turn's
/// tokens to discover the answer. That is a reason to probe rarely. An auth
/// re-probe is the cheapest failure in this crate — the CLI rejects the
/// credential before it ever reaches a model, so the spawn costs a process and
/// zero tokens, and it returns in well under a second. Thirty minutes of that is
/// two refused spawns an hour per barred login; it is not a token furnace and
/// cannot become one.
///
/// The cost that dominates instead is on the other side, and it is HIS time. The
/// slow step in this loop is the human one: he has to be told, get to a browser,
/// and run `/login`. Once he has done it, the bar is the only thing still
/// standing between a working account and a waiting thread — and a bar that is
/// too long means he re-authenticates and the daemon keeps refusing the account
/// for the rest of the hour with no way to say so. That is the 2026-08-15
/// year-long-bar incident in miniature, which `CREDIT_BAR_SECS` also cites, and
/// it is strictly worse here than there, because here he has just done work
/// specifically to fix it and is watching for the result.
///
/// The same asymmetry `MAX_PARSED_RESET` states therefore lands differently:
/// *too SHORT costs one extra refusal; too LONG costs the account until the
/// clock runs out.* Too short here costs one free spawn. Too long costs him the
/// half hour after he has already fixed the problem. Thirty minutes is where
/// those meet; it is not tuned to a measurement, and if it moves it should move
/// DOWN.
///
/// # IT MOVED DOWN (2026-08-27), exactly as the paragraph above predicted
///
/// SUPERSEDED by [`login_bar_secs`]. Retained only as the documented history of
/// why the number was what it was; nothing reads it on the bar path any more.
/// The reasoning above survives intact — the operator supplied the missing
/// premise, and it is the one this comment had already identified as decisive.
pub const LOGIN_BAR_SECS: i64 = 1800;

/// **How long a login stays barred after failing to authenticate — 60, 120,
/// 300, then 600 seconds, indexed by how many times in a row it has failed.**
///
/// # The ruling, and its CONDITION
///
/// > *"The auth recheck, if it's just a process and it's not an actual auth
/// > check against their API, then you can do it more frequently than that. You
/// > can do it like back off, slow back off only up to like five minutes even
/// > or 10 minutes."*
///
/// The condition is his, so it was answered before the number was touched.
///
/// **It is a process. It is not an auth check against Anthropic's API.** The
/// evidence, in the order it settles the question:
///
/// 1. **This crate cannot make an API call.** `lib/threads/Cargo.toml` has no
///    HTTP client. `reqwest` is in the *workspace* manifest but is not a
///    dependency of `threads`; `axum` and `tokio-tungstenite` are the daemon's
///    own server and its stress-test client. `api.anthropic.com` appears
///    exactly once in `src/`, inside a comment.
/// 2. **There is no auth-recheck function to begin with.** The bar expires on
///    the clock, `retry_sweep` wakes, and one ORDINARY turn is admitted. The
///    account's own success is the recheck (`note_account_served` clears it).
/// 3. **The only periodic spawn is local.** `gauge::ClaudeUsageProbe::probe`
///    runs `claude -p /usage --output-format json --no-session-persistence` as
///    a `std::process::Command`. It measures quota and never touches auth.
/// 4. **A bad credential fails before a model is reached.** The paragraph on
///    [`LOGIN_BAR_SECS`] above already recorded this from the captures: *"the
///    CLI rejects the credential before it ever reaches a model, so the spawn
///    costs a process and zero tokens, and it returns in well under a second."*
///    The four real `authentication_failed` records in the turn logs confirm
///    it — `"Failed to authenticate: OAuth session expired and could not be
///    refreshed"`, with `usage` all zeroes.
///
/// So the cost of a recheck is one local process and no tokens, and the cost of
/// being slow is his time after he has already fixed the problem. That is a
/// ratio that argues for minutes, not half-hours.
///
/// # Why a ladder rather than a smaller constant
///
/// Because the two ends of the loop want different things. The FIRST recheck
/// should be prompt: he has just been told to run `claude /login`, he may be at
/// the browser already, and one refused local spawn is the entire downside of
/// being early. The TENTH recheck is evidence he is not at the browser — asleep,
/// out, or the account is genuinely retired — and continuing to spawn a doomed
/// process every minute for hours is noise in his logs for no benefit.
///
/// 60 → 120 → 300 → 600 answers both: under two minutes for the case where he
/// is watching, settling to his stated ceiling for the case where he is not.
/// The ceiling is the ten-minute end of the five-to-ten range he gave, chosen
/// over five because the streak only reaches it after roughly seventeen minutes
/// of a login failing repeatedly, by which point promptness has stopped being
/// the thing that matters.
///
/// # The streak resets on any good news
///
/// `note_account_served` (a turn completed) and `note_login_restored` (the
/// login flow observed a success) both clear it, so a login that recovers and
/// later fails again starts at 60 seconds rather than resuming at 600.
///
/// It is held in memory, not in the schema. A daemon restart resets every
/// streak to zero, which fails in the SAFE direction — the next recheck happens
/// sooner, costing at most one extra local spawn per barred login.
pub fn login_bar_secs(consecutive_failures: u32) -> i64 {
    match consecutive_failures {
        0 => 60,
        1 => 120,
        2 => 300,
        _ => LOGIN_BAR_MAX_SECS,
    }
}

/// The ceiling of [`login_bar_secs`] — ten minutes, the far end of the range
/// the operator gave. Down from the thirty minutes of [`LOGIN_BAR_SECS`].
pub const LOGIN_BAR_MAX_SECS: i64 = 600;

/// **What a `result` event flagged as an error actually means, given that its
/// subtype may be `success`.**
///
/// The entry point the runtime uses for `result` lines carrying
/// `is_error: true`. [`classify_result`] below classifies the SUBTYPE, and for
/// every subtype but one that is the right thing to do.
///
/// # `subtype: "success"` with `is_error: true` is not a success and not a
/// subtype we can classify
///
/// It reaches the classifier — the doc comment on [`classify_result`] used to
/// claim it could not — and it did so 46 times between 2026-08-03 and
/// 2026-08-18 in the operator's own ledger, thirteen of them on the 18th, over
/// a dozen threads. Every one of them was thrown away as a fatal
/// `claude_error` whose entire message read `result subtype success`: a turn
/// destroyed under a sentence that says nothing and contradicts itself.
///
/// The ledger says exactly what those runs were. The last thing on the stream
/// before the flagged result, in all 46:
///
/// * `API Error: 529 Overloaded` — after the CLI burned all ten of its own
///   internal `api_retry` attempts (23 occurrences)
/// * `API Error: Unable to connect to API (ENOTFOUND)` (8)
/// * `API Error: Connection closed mid-response` (9)
/// * `API Error: 500 Internal server error` (5)
/// * `Prompt is too long` (4)
///
/// So the meaning is settled by evidence rather than by guess: the CLI's
/// session loop ENDED in an orderly way — that is all `subtype: "success"`
/// asserts — while the run itself failed, and the failure is described in the
/// stream's final text, never in the subtype. Classifying from the subtype
/// here is a category error, and it is the bug.
///
/// Three of those five causes are transient and one is the network being gone;
/// each already has a ladder built for it. So this hands the CLI's last words
/// to [`classify_message`], the same door a non-zero exit with stderr goes
/// through, and the operator's prompt rides the retry row that a `Retry`
/// creates — it is not destroyed.
///
/// When the text names nothing we recognize (or there is no text at all) we
/// have a self-contradictory report and no diagnosis. That is precisely the
/// case that must NOT be fatal: an unexplained contradiction may cost a few
/// bounded retries, never the operator's turn. It is treated as what it
/// literally is — an error that happened during execution — which is the CLI's
/// own `error_during_execution` class, bounded by `max_attempts` and ending in
/// a VISIBLE `TurnFailed` that quotes the real last words rather than a
/// tautology.
pub fn classify_errored_result(subtype: &str, final_text: Option<&str>) -> RetryDecision {
    if subtype != "success" {
        return classify_result(subtype);
    }
    let text = final_text.map(str::trim).filter(|t| !t.is_empty());
    match text.map(classify_message) {
        // A recognized failure mode: it gets its own ladder (5xx → overload,
        // ENOTFOUND / closed connection → connectivity, credit → G9).
        // `LoginExpired` joins this list, and its absence is the 2026-08-26
        // incident. A verdict that is NOT passed through here does not merely
        // lose its name — it is silently downgraded by the arm below into
        // `ErrorDuringExecution`, which means the `"transient"` wire tag and the
        // three-attempt `max_attempts` ladder. That is how an expired login came
        // to be "retried" three times in ten seconds, on the same dead account,
        // and then given up with the raw credential error quoted at the operator
        // (an internal thread, 2026-08-26T23:41:37→23:41:47 in his own ledger).
        //
        // Note what did NOT go wrong there: `classify_message` got it right and
        // returned a verdict. This match threw the verdict away. Any future
        // class added to `classify_message` and forgotten here inherits exactly
        // that failure, silently — which is why the list is spelled out rather
        // than written as "anything but Fatal".
        Some(decision @ (RetryDecision::Retry { .. }
        | RetryDecision::CreditBalance { .. }
        | RetryDecision::LoginExpired { .. }
        | RetryDecision::RestartFresh { .. })) => decision,
        // Unrecognized text, or none. Bounded retry, and the reason carries
        // the words the CLI actually said.
        _ => RetryDecision::Retry {
            kind: TransientKind::ErrorDuringExecution,
            reason: match text {
                Some(t) => format!("the result was flagged as an error: {}", brief(t)),
                None => "the result was flagged as an error but named no cause".into(),
            },
        },
    }
}

/// A retry reason and a `TurnFailed` message are read aloud by AUI; the last
/// thing on a stream can be a whole assistant turn. Quote enough to name the
/// cause and no more.
fn brief(text: &str) -> String {
    const MAX: usize = 240;
    if text.chars().count() <= MAX {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX).collect();
    format!("{head}…")
}

/// The `result` subtype the CLI writes when the model kept failing the
/// `--json-schema` contract (R11.J1: five refused `StructuredOutput` calls,
/// `is_error: true`, exit 1).
pub const SCHEMA_RETRIES_EXHAUSTED: &str = "error_max_structured_output_retries";

/// **R11.J2 — a missing shape is not a failed turn** (`docs/STRUCTURED-OUTPUT.md`
/// §3.3; D3's "process level machined status").
///
/// `error_max_structured_output_retries` used to be classified here as a fatal
/// configured ceiling, beside `error_max_turns`. It is not one: the turn's work
/// RAN and only its JSON is missing, and the schema is the asker's contract to
/// handle. So a `result` with this subtype is read as an OK turn
/// (`stream::parse_result` clears `is_error`, the ledger writes
/// `outcome: ok`, and the runtime does not ladder its exit code), and the
/// daemon's own record says `unmet` (`event::StructuredOutputRecord`).
pub fn is_schema_unmet_result(subtype: &str) -> bool {
    subtype == SCHEMA_RETRIES_EXHAUSTED
}

/// Classify a `result` event's subtype.
///
/// `success` does reach here on a flagged result — see
/// [`classify_errored_result`], which is the runtime's entry point and handles
/// that case before this function is consulted.
pub fn classify_result(subtype: &str) -> RetryDecision {
    match subtype {
        "error_during_execution" => RetryDecision::Retry {
            kind: TransientKind::ErrorDuringExecution,
            reason: "result error_during_execution".into(),
        },
        "error_max_turns" | "error_max_budget_usd" => {
            RetryDecision::Fatal {
                reason: format!("hit configured ceiling: {subtype}"),
            }
        }
        other => RetryDecision::Fatal {
            reason: format!("result subtype {other}"),
        },
    }
}

/// Classify a free-text error message (in-stream synthetic text or stderr).
/// Ordering matters: credit balance is checked BEFORE the limit patterns,
/// since both can mention "limit" (ported from AUI `classify_claude_error`).
pub fn classify_message(text: &str) -> RetryDecision {
    let t = text.to_ascii_lowercase();
    if t.contains("credit balance") || t.contains("insufficient credit") {
        return RetryDecision::CreditBalance {
            reason: text.to_string(),
        };
    }
    // ── AN EXPIRED LOGIN, AND THE PREDICATE IS BORROWED ON PURPOSE ──────────
    //
    // `is_dead_login_error` is the crate's existing answer to "does this text
    // prove the credential itself is broken", it is already narrow (it requires
    // an auth-shaped phrase AND a terminal one, and explicitly returns false for
    // a credit balance, a rate limit, an overload and a connection drop), and it
    // was already written against the real captures. Calling it here rather than
    // adding a second list of substrings is the whole of the reuse: there is now
    // ONE definition of "this login is expired" in the crate, and the classifier
    // and `runtime::note_login_health` cannot drift apart about what it means.
    //
    // Placed directly after the credit check and ahead of every generic bucket,
    // because those buckets are what swallowed it. This block is ordered above
    // the rate-limit patterns for the same reason credit is: an auth failure
    // that happens to contain a generic substring must not be shelved as one.
    if crate::accounts::is_dead_login_error(text) {
        return RetryDecision::LoginExpired {
            reason: text.to_string(),
        };
    }
    if t.contains("no conversation found") {
        return RetryDecision::RestartFresh {
            reason: "expired resume session".into(),
        };
    }
    if t.contains("rate_limit")
        || t.contains("rate limit")
        || t.contains("session limit")
        || t.contains("usage limit")
        || t.contains("too many requests")
        || t.contains("hit your")
        || t.contains("429")
        || t.contains("resets")
    {
        return RetryDecision::Retry {
            kind: TransientKind::RateLimit,
            reason: "rate limit (429)".into(),
        };
    }
    if t.contains("overloaded")
        || t.contains("529")
        || t.contains("503")
        || t.contains("502")
        || t.contains("500")
        || t.contains("temporarily")
    {
        return RetryDecision::Retry {
            kind: TransientKind::Overload,
            reason: "server overload (5xx)".into(),
        };
    }
    // ── CONNECTIVITY ────────────────────────────────────────────────────────
    //
    // **This block is the fix for intent item 73, and the reason it cost him
    // hours.** It used to match six substrings — `econnreset`,
    // `connection reset`, `unable to connect`, `timed out`, `timeout`,
    // `connection` — and every one of them assumes the word "connection" or
    // "timeout" appears in the text.
    //
    // The errors a REAL outage produces mostly do not contain either word.
    // `getaddrinfo ENOTFOUND api.anthropic.com` (DNS down) does not.
    // `fetch failed` — which is what undici says when the request never left —
    // does not. `ECONNREFUSED` does not contain "connection". Neither does
    // `socket hang up`, `EAI_AGAIN`, `ENETUNREACH`, `EHOSTUNREACH`, or a TLS
    // handshake failure. Every one of those fell through to `Fatal` below:
    // terminal, never retried, thread down. That is precisely what he
    // described — *"I just gave some input and then it got disconnected from
    // the internet and wasn't able to resume, and I have to now say this a few
    // hours later."*
    //
    // So the list is now the error VOCABULARY of the network stack rather than
    // a guess at its prose. Ordered after the 5xx block on purpose: a 500 from
    // the API is the service answering, which is an overload with its own
    // ladder, not the network being gone.
    if t.contains("econnreset")
        || t.contains("econnrefused")
        || t.contains("econnaborted")
        || t.contains("enotfound")
        || t.contains("eai_again")
        || t.contains("enetunreach")
        || t.contains("enetdown")
        || t.contains("ehostunreach")
        || t.contains("epipe")
        || t.contains("etimedout")
        || t.contains("getaddrinfo")
        || t.contains("fetch failed")
        || t.contains("socket hang up")
        || t.contains("network")
        || t.contains("offline")
        || t.contains("dns")
        || t.contains("tls")
        || t.contains("ssl")
        || t.contains("certificate")
        || t.contains("handshake")
        || t.contains("connection reset")
        || t.contains("connection refused")
        || t.contains("connection closed")
        || t.contains("unable to connect")
        || t.contains("could not connect")
        || t.contains("cannot connect")
        || t.contains("no route to host")
        || t.contains("timed out")
        || t.contains("timeout")
        || t.contains("connection")
    {
        return RetryDecision::Retry {
            kind: TransientKind::Connectivity,
            reason: "cannot reach the service (network)".into(),
        };
    }
    RetryDecision::Fatal {
        reason: format!("unclassified error: {text}"),
    }
}

/// **What a SYNTHETIC API ERROR actually is — read from the fields the CLI
/// sets, not guessed from its prose.**
///
/// # The defect this exists to close
///
/// `stream::is_api_error` detects the CLI's synthetic API-error record. Until
/// 2026-08-27 everything it detected became `ThreadEvent::LimitHit`, and
/// `runtime::interpret` converts a `LimitHit` into
/// `TransientKind::RateLimit` **unconditionally** — no guard, no inspection of
/// the status the parser had already captured. That was survivable only because
/// the detector was broken in a way that made it fire for rate limits and
/// nothing else (it tested `isApiErrorMessage`/`apiErrorStatus`, which the
/// producer has never emitted; `error == "rate_limit"` was the only live
/// clause).
///
/// So the two defects were holding each other up. Fixing the spelling alone
/// would have routed an expired OAuth session, a 529 and a rejected prompt all
/// onto the usage-limit ladder — waiting for a reset that does not exist, with
/// no login bar raised and nothing said to the operator. The cause would have
/// been lost in exactly the way `classify_errored_result`'s `_` arm lost it.
///
/// This function is the axis. `stream::parse_assistant` now mints `LimitHit`
/// only for a genuine limit and `ThreadEvent::ApiError` for everything else,
/// carrying `error` and `status` through intact so this can read them.
///
/// # The real classes, measured
///
/// Every named `error` value in the operator's turn logs (7,018 files,
/// 2026-07-30 → 2026-08-26), with the count of assistant-path records:
///
/// | `error`                 | n     | status seen | verdict            |
/// |-------------------------|-------|-------------|--------------------|
/// | `rate_limit`            | 4,845 | —           | `RateLimit`        |
/// | `server_error`          | 158   | —, 529, 500 | prose, else `Overload` |
/// | `invalid_request`       | 6     | —, 400      | `Fatal`            |
/// | `authentication_failed` | 4     | —           | `LoginExpired`     |
///
/// # The order, and why it is this order
///
/// 1. **The named cause first**, because it is the producer's own diagnosis and
///    it is structured. Prose is a translation of it and translations drift.
/// 2. **Then the status**, for the shape that names no cause at all — which is
///    what a `result` line does (4,742 of them carry a bare
///    `api_error_status: 429`).
/// 3. **Then the prose**, via [`classify_message`].
///
/// The fallback is the point of the third step. A class this crate has never
/// seen must NOT land on a default verdict — that is the failure mode of both
/// audited defects. It falls back to the classifier that reads the words, and
/// only if that also recognizes nothing does it become `Fatal`, which is itself
/// a bounded ladder rather than a floor.
pub fn classify_api_error(error: Option<&str>, status: Option<u64>, text: &str) -> RetryDecision {
    // ── 1. THE NAMED CAUSE ───────────────────────────────────────────────
    match error {
        Some("rate_limit") => {
            return RetryDecision::Retry {
                kind: TransientKind::RateLimit,
                reason: "rate limit (429)".into(),
            };
        }
        // H179: the API's own names for "this ACCOUNT cannot be used" — an
        // unpaid or revoked account, a refused credential. Account-scoped, so
        // the account is barred and the turn is re-placed, never retried there.
        Some("billing_error") | Some("payment_required") => {
            return RetryDecision::CreditBalance {
                reason: if text.trim().is_empty() {
                    "the API refused this account for billing".into()
                } else {
                    text.to_string()
                },
            };
        }
        Some("authentication_error") | Some("permission_error") => {
            return RetryDecision::LoginExpired {
                reason: if text.trim().is_empty() {
                    format!("the API refused this account's credential ({})", error.unwrap_or(""))
                } else {
                    text.to_string()
                },
            };
        }
        Some("authentication_failed") => {
            // The reason is the CLI's verbatim sentence — "Failed to
            // authenticate: OAuth session expired and could not be refreshed"
            // — because it is what `note_account_login_expired` puts in front
            // of the operator, and it names the action only he can take.
            return RetryDecision::LoginExpired {
                reason: if text.trim().is_empty() {
                    "the CLI reported authentication_failed".into()
                } else {
                    text.to_string()
                },
            };
        }
        // The REQUEST is bad, not the service and not the credential: a
        // safeguards refusal, or a prompt over the context window. Retrying
        // the identical bytes cannot change the answer, so this is the
        // deterministic ladder — short, widely spaced, and bounded — rather
        // than a transient one.
        Some("invalid_request") => {
            return RetryDecision::Fatal {
                reason: if text.trim().is_empty() {
                    "the CLI rejected this request as invalid".into()
                } else {
                    text.to_string()
                },
            };
        }
        // A 5xx from the service: `ServerError`, on its own SMALL budget.
        //
        // **SUPERSEDED (2026-10-14): this arm used to let the prose decide**,
        // on the theory that "Connection closed mid-response" and "Unable to
        // connect to API (ENOTFOUND)" under `server_error` were the network
        // being gone and deserved the 26-minute connectivity ladder. Thread
        // 60f0380d is the refutation: forty `server_error … (ENOTFOUND)`
        // refusals, each arriving after ~3 minutes of real subagent work, i.e.
        // a turn that plainly HAD the network. The producer's structured name
        // outranks a translation of it (rule 1 of this function), so the name
        // wins. Only the account-scoped prose verdicts survive — a credit, a
        // login or a rate limit is a fact about the ACCOUNT that a different
        // ladder exists to act on.
        Some("server_error") => return server_error_unless_account_scoped(text, "5xx"),
        _ => {}
    }

    // ── 2. THE STATUS, for the shape that names no cause ──────────────────
    if let Some(code) = status {
        if code == 429 {
            return RetryDecision::Retry {
                kind: TransientKind::RateLimit,
                reason: "rate limit (429)".into(),
            };
        }
        if (500..600).contains(&code) {
            return server_error_unless_account_scoped(text, &code.to_string());
        }
        if (400..500).contains(&code) {
            // 400/401/403/413 — the request or the credential. `classify_message`
            // is consulted first precisely so a 401 whose prose names an expired
            // session still becomes `LoginExpired` rather than a flat refusal.
            let by_prose = classify_message(text);
            return match by_prose {
                // H179: 401 / 403 / 402 are facts about the ACCOUNT, not the
                // request — the operator: "unpaid / 401 / 403 / billing errors"
                // make an account unavailable. Barred, and the turn re-placed.
                RetryDecision::Fatal { .. } if code == 402 => RetryDecision::CreditBalance {
                    reason: if text.trim().is_empty() {
                        "the API refused this account for billing (402)".into()
                    } else {
                        text.to_string()
                    },
                },
                RetryDecision::Fatal { .. } if code == 401 || code == 403 => {
                    RetryDecision::LoginExpired {
                        reason: if text.trim().is_empty() {
                            format!("the API refused this account's credential ({code})")
                        } else {
                            text.to_string()
                        },
                    }
                }
                RetryDecision::Fatal { .. } => RetryDecision::Fatal {
                    reason: if text.trim().is_empty() {
                        format!("the API refused this request ({code})")
                    } else {
                        text.to_string()
                    },
                },
                other => other,
            };
        }
    }

    // ── 3. THE PROSE ─────────────────────────────────────────────────────
    //
    // An `error` this crate has never seen lands here rather than on a default
    // verdict. This is the arm the audit exists to make sure keeps existing.
    classify_message(text)
}

/// The verdict for an API-reported server error: [`TransientKind::ServerError`],
/// unless the prose names something about the ACCOUNT (credit, login, rate
/// limit), which keeps its own ladder because that ladder acts on the account.
/// Connectivity prose does NOT win here — see the `server_error` arm of
/// [`classify_api_error`] for why.
fn server_error_unless_account_scoped(text: &str, code: &str) -> RetryDecision {
    match classify_message(text) {
        d @ (RetryDecision::CreditBalance { .. } | RetryDecision::LoginExpired { .. }) => d,
        d @ RetryDecision::Retry {
            kind: TransientKind::RateLimit,
            ..
        } => d,
        _ => RetryDecision::Retry {
            kind: TransientKind::ServerError,
            reason: if text.trim().is_empty() {
                format!("the API answered with a server error ({code})")
            } else {
                format!("the API answered with a server error ({code}): {}", brief(text.trim()))
            },
        },
    }
}

/// **The ceiling for a SERVER-ERROR retry, in attempts — three.**
///
/// Small on purpose. A server error arrives after the turn has already run, so
/// every retry re-buys whatever work the turn did before the refusal (in thread
/// 60f0380d, ~3 minutes of subagents per attempt). Three is enough to ride out
/// a genuine blip; past it, the operator is told and his words are kept.
pub const MAX_SERVER_ERROR_ATTEMPTS: u32 = 3;

/// **The ceiling for a SERVER-ERROR retry, in wall-clock — ten minutes**,
/// measured from the first server error of the current run
/// (`Store::note_delivery_failure`). Whichever of this and
/// [`MAX_SERVER_ERROR_ATTEMPTS`] is reached first ends the ladder, so a turn
/// whose every attempt runs for an hour cannot spend three hours on it.
pub const SERVER_ERROR_BUDGET_SECS: u64 = 10 * 60;

/// Backoff (seconds) for a server-error retry — 10, 30, then 60.
pub fn server_error_backoff_secs(attempt: u32) -> u64 {
    match attempt {
        0 | 1 => 10,
        2 => 30,
        _ => 60,
    }
}

/// Has a server-error run spent its budget? `attempt` is the attempt that just
/// failed; `elapsed_secs` is wall-clock since the run's first server error.
pub fn server_error_exhausted(attempt: u32, elapsed_secs: u64) -> bool {
    attempt >= MAX_SERVER_ERROR_ATTEMPTS || elapsed_secs >= SERVER_ERROR_BUDGET_SECS
}

/// **The connectivity budget, as WALL-CLOCK time — twenty-six minutes.**
///
/// This is the figure [`MAX_CONNECTIVITY_ATTEMPTS`]'s own comment always
/// justified ("a little over 26 minutes of patience"), now enforced as the
/// thing it described. The count only approximated it under the assumption
/// that a failed attempt costs nothing but its backoff; an attempt that runs
/// for minutes before failing breaks that assumption, and forty of them were
/// 2½ days in an internal thread. Measured from the first connectivity failure of
/// the current run (`Store::note_delivery_failure`), which survives retries,
/// parks and daemon restarts.
pub const CONNECTIVITY_BUDGET_SECS: u64 = 26 * 60;

/// Has a connectivity run spent its budget? Time only — no attempt count.
pub fn connectivity_exhausted(elapsed_secs: u64) -> bool {
    elapsed_secs >= CONNECTIVITY_BUDGET_SECS
}

/// Backoff (seconds) for a CONNECTIVITY retry — 2, 5, 10, 20, 30, then 45
/// forever.
///
/// Two constraints pull against each other here and the cap is where they
/// meet. An outage can last minutes, so the ladder has to keep going; but
/// *"cap the backoff so a long outage does not become an hours-long silence"*
/// (task 0088), so it must never grow without bound.
///
/// **The 45-second ceiling is also the recovery mechanism, and that is
/// deliberate.** The task offered a choice: probe reachability, or retry on a
/// short ceiling. This takes the ceiling, because a probe is a second network
/// dependency that can itself fail, hang, or disagree with the real request —
/// a reachability check that says "up" while the API is unreachable is worse
/// than no check at all, and it needs its own timeout, its own failure mode
/// and its own tests. A bounded ceiling needs none of that: when the network
/// comes back, the thread resumes within at most 45 seconds, and the thing
/// that proves the network is back is the actual turn succeeding rather than a
/// proxy for it. Simple, and it cannot be wrong in a way the real request is
/// not already wrong.
pub fn connectivity_backoff_secs(attempt: u32) -> u64 {
    match attempt {
        0 | 1 => 2,
        2 => 5,
        3 => 10,
        4 => 20,
        5 => 30,
        _ => 45,
    }
}

/// How many times a connectivity failure is retried before the thread is
/// failed VISIBLY.
///
/// **SUPERSEDED AS THE ENFORCEMENT (2026-10-14) by [`CONNECTIVITY_BUDGET_SECS`].**
/// Nothing on the retry path compares an attempt count against this any more;
/// it survives as the sizing argument for the wall-clock budget, which is what
/// the paragraphs below were always describing.
///
/// Sized against the ladder above: attempts 1-5 cost 67 seconds, and every
/// attempt after that costs 45, so 40 attempts is a little over 26 minutes of
/// patience. That is long enough to sit out a router reboot, a tunnel flap or
/// an ISP blip — the failures that actually stranded him — without becoming an
/// unbounded silence.
///
/// It is DISTINCT from `RetryPolicy::max_attempts` (3) on purpose. Three
/// attempts on the transient ladder is about seven seconds, which is not a
/// network outage policy at all; it is what made a brief disconnection
/// terminal. A connectivity failure must not consume the transient ceiling and
/// a transient failure must not consume this one.
pub const MAX_CONNECTIVITY_ATTEMPTS: u32 = 40;

/// Bounded exponential backoff (ms) for attempt `n` (1-based), capped at 30s:
/// 1s, 2s, 4s, 8s, 16s, 30s, 30s…
///
/// **SUPERSEDED as the transient ladder (2026-08-27) — see
/// [`transient_backoff_secs`].** It survives because `RetryPolicy` and several
/// tests still name it, and because it is the honest description of what the
/// `_` arm in `runtime::compute_retry_at` used to do. Nothing on the retry path
/// selects it any more.
pub fn backoff_ms(attempt: u32) -> u64 {
    let base = 500u64;
    let cap = 30_000u64;
    base.saturating_mul(1u64 << attempt.min(6)).min(cap)
}

/// **The TRANSIENT ladder — 0, 5, 15, 45, 120, 300, then 300 forever.**
///
/// # The ruling
///
/// > *"we need to have rate limiting on both types. A transient issue, even
/// > though it might justify an immediate retry, definitely needs a back-off
/// > strategy. And then the fatal issues, like doing once an hour, makes sense
/// > there."*
///
/// Both halves are load-bearing and this function is the first half. The first
/// retry is FREE — attempt 1 waits zero seconds, because a hiccup in a request
/// that reached the service genuinely does often clear on the very next try and
/// making him wait for that is pure latency. Every attempt after it backs off,
/// because a second identical failure is evidence the first one was not a
/// hiccup.
///
/// # What it replaces, and why that was a defect of the audited shape
///
/// `Overload` and `ErrorDuringExecution` had **no arm at all** in
/// `runtime::compute_retry_at`. They fell through its `_ =>` default onto
/// [`backoff_ms`] — 1s, 2s, 4s — and were ceilinged by `RetryPolicy::max_attempts`,
/// which defaults to **3**. So the complete retry policy for a 529 was *three
/// spawns inside seven seconds, then give up*.
///
/// That is not a backoff strategy; it is a burst. And the two classes it
/// governed are not marginal:
///
/// - `Overload` is what [`classify_message`] assigns to every `529` / `503` /
///   `502` / `500` / `"temporarily"`, the most common non-limit refusal in the
///   ledger.
/// - `ErrorDuringExecution` is the destination of [`classify_errored_result`]'s
///   own catch-all arm — every errored result whose cause this crate could not
///   name. The least-understood failures got the least patience.
///
/// It is also the same *mechanical* defect as the two this audit was called
/// for: a class reaching a `match` that has no arm for it and being absorbed by
/// a default that was written for something else. `compute_retry_at` no longer
/// has a `_` arm, so a new [`TransientKind`] cannot inherit this by accident —
/// it will not compile.
///
/// # Why these rungs
///
/// Sized between the two ladders it sits amongst. Denser at the bottom than
/// [`deterministic_backoff_secs`] (5, 15, 45…) because a transient failure is
/// expected to clear and should be re-tried promptly; wider at the top than
/// [`connectivity_backoff_secs`] (ceiling 45s) because an overloaded service
/// is not a router reboot and hammering it every 45 seconds for half an hour is
/// exactly the behaviour a 529 is asking you to stop.
///
/// Eight attempts on this ladder span about fourteen minutes, against the seven
/// seconds it replaces.
pub fn transient_backoff_secs(attempt: u32) -> u64 {
    match attempt {
        // "may justify an immediate retry" — his words, and only ONCE.
        0 | 1 => 0,
        2 => 5,
        3 => 15,
        4 => 45,
        5 => 120,
        _ => 300,
    }
}

/// **The ceiling for a TRANSIENT retry — eight.**
///
/// Distinct from `RetryPolicy::max_attempts` (3) for the same reason
/// [`MAX_CONNECTIVITY_ATTEMPTS`] is: no class may spend another's budget. Three
/// was never chosen for `Overload`; `Overload` simply inherited it by falling
/// off the end of an `if` chain.
///
/// Eight rungs of [`transient_backoff_secs`] is roughly fourteen minutes, which
/// is long enough to sit out a real capacity event without becoming the
/// unbounded silence [`DEFAULT_MAX_USAGE_LIMIT_ATTEMPTS`] exists to prevent.
pub const DEFAULT_MAX_TRANSIENT_ATTEMPTS: u32 = 8;

/// **The ladder for a kind, as a pure function — one definition, one place, and
/// since 2026-10-06 THE ONLY definition.**
///
/// It used to be the extracted, testable HALF of `runtime::compute_retry_at`,
/// which wrote a per-thread `retries.retry_at` instant from it. That function is
/// gone with the timestamp (operator: *"There should not be a per-thread retry
/// timestamp"*), so this is now the whole of the policy, and the POOL reads it:
/// [`crate::runtime::PoolCadence`] takes the MINIMUM rung across everything
/// parked and sleeps that long. **The numbers below are untouched by that move —
/// they were re-homed, not retuned.**
///
/// The `match` is exhaustive on purpose. That is the whole lesson of this
/// audit: a wildcard here would let the next `TransientKind` be born silently
/// on somebody else's ladder.
pub fn ladder_secs(kind: &TransientKind, attempt: u32) -> u64 {
    match kind {
        TransientKind::RateLimit => usage_limit_backoff_secs(attempt),
        TransientKind::Connectivity | TransientKind::ConnectionDrop => {
            connectivity_backoff_secs(attempt)
        }
        TransientKind::Overload | TransientKind::ErrorDuringExecution => {
            transient_backoff_secs(attempt)
        }
        TransientKind::ServerError => server_error_backoff_secs(attempt),
        TransientKind::Deterministic => deterministic_backoff_secs(attempt),
        TransientKind::CreditBalance => credit_backoff_secs(attempt),
        TransientKind::LoginExpired => login_backoff_secs(attempt),
    }
}

/// **The persisted `retries.kind` string, back to the class that wrote it.**
///
/// The inverse of [`TransientKind::kind_str`], and it exists because the pool's
/// cadence reads [`ladder_secs`] off a ROW rather than off a live failure. The
/// `match` carries every tag `kind_str` can emit; anything else answers `None`
/// and the caller treats that as the most PATIENT class it knows, never as
/// attempt zero of a fast ladder, because an unreadable tag must not buy a
/// tighter loop.
///
/// `ConnectionDrop` is unreachable here on purpose: it shares `"connectivity"`
/// with [`TransientKind::Connectivity`] and takes the identical ladder, so one
/// arm is honest and two would imply a difference that does not exist.
pub fn kind_from_str(kind: &str) -> Option<TransientKind> {
    Some(match kind {
        "usage_limit" => TransientKind::RateLimit,
        "connectivity" => TransientKind::Connectivity,
        "transient" => TransientKind::Overload,
        "server_error" => TransientKind::ServerError,
        "claude_error" => TransientKind::Deterministic,
        "credit_balance" => TransientKind::CreditBalance,
        "login_expired" => TransientKind::LoginExpired,
        _ => return None,
    })
}

/// Backoff (seconds) for usage-limit retries with NO parseable reset time —
/// AUI's ladder (`schedule_auto_retry`): 30 → 60 → 120 → 300, then 300
/// forever (capped, retrying indefinitely).
pub fn usage_limit_backoff_secs(attempt: u32) -> u64 {
    match attempt {
        0 | 1 => 30,
        2 => 60,
        3 => 120,
        _ => 300,
    }
}

/// **The attempt ceiling for a USAGE-LIMIT retry — the one ladder that had
/// none.**
///
/// Every other failure class in this crate is bounded: transient failures at
/// `RetryPolicy::max_attempts` (3), connectivity at
/// [`MAX_CONNECTIVITY_ATTEMPTS`] (40). The usage-limit path was deliberately
/// exempt — *"Usage-limit retries are NOT capped — they wait for the reset and
/// try again"* — and the exemption was reasonable for the quantity it was
/// written about. Waiting out a reset should not be rationed by a counter.
///
/// **But `attempt` never counted waiting. It counts SPAWNS.** Every increment
/// is a real `claude` child process that ran and was refused (`runtime.rs`
/// writes `attempt + 1` only after `run_once` returned). So an uncapped
/// usage-limit ladder is not "wait as long as it takes" — it is "spend as many
/// processes as it takes", with nothing that ever decides it has taken too
/// many. Measured live on 2026-08-06: one thread reached `attempt = 924` on a
/// single delivery, `kind = usage_limit`, `reason = rate limit (429)`.
///
/// Forty-eight is sized against what a HEALTHY usage-limit retry costs, which
/// is roughly one attempt per genuine reset window: one a week on the weekly
/// pool, one every few hours on the session pool. Forty-eight refusals is
/// therefore more than a week of the fastest-cycling window and several months
/// of the slowest — far outside anything a reset can explain, and one
/// nineteenth of what was actually observed. It bounds the pathology without
/// coming anywhere near the legitimate case.
///
/// It is a DEFAULT, not a constant: it lives on `RetryPolicy` next to
/// `max_attempts`, so it is per-thread like every other ceiling here.
pub const DEFAULT_MAX_USAGE_LIMIT_ATTEMPTS: u32 = 48;

/// What a usage-limit retry should do at this attempt count.
///
/// A pure function of the counter and the ceiling, deliberately: the decision
/// is the thing worth testing, and testing it must not require a daemon, a
/// database, or a spawned process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageLimitVerdict {
    /// Still a backoff. Arm the next attempt quietly.
    Wait,
    /// Still retrying, but this has stopped looking like waiting for a reset.
    /// Arm the next attempt AND say so loudly, on the operator's channel.
    Starving,
    /// This is not a backoff any more. Stop, visibly.
    GiveUp,
}

/// How many attempts in a retry may go before it is STARVING rather than
/// waiting — a quarter of its ceiling.
///
/// The shape is lifted from the deferred-restart starvation bound in
/// `update.rs` ("a restart pending longer than … is no longer waiting, it is
/// starving, and it says so at ERROR … repeating every 5 minutes"). Same three
/// parts: a threshold past which the wait is reclassified, a REPEAT so the
/// escalation cannot be deduplicated into a single forgotten line, and a hard
/// stop behind both.
///
/// Derived from the ceiling rather than fixed so that lowering the ceiling —
/// in a test, or for a thread the operator wants on a short leash — moves the
/// escalation with it instead of silently disabling it.
pub fn usage_limit_starving_from(ceiling: u32) -> u32 {
    (ceiling / 4).max(1)
}

/// How often the starving escalation repeats once it has begun — an eighth of
/// the ceiling. At the default that is a loud line at attempts 12, 18, 24, 30,
/// 36 and 42, and the give-up at 48.
///
/// `update.rs` records why a repeat is not optional: its own deferral log was
/// deduplicated forever, so *"an hour of starvation left ONE line in the ledger
/// and looked like seven quiet deferrals."* One line at attempt 12 that is
/// never repeated would misreport 924 attempts exactly the same way.
pub fn usage_limit_escalate_every(ceiling: u32) -> u32 {
    (ceiling / 8).max(1)
}

/// The bound and the escalation, in one place.
pub fn usage_limit_verdict(attempt: u32, ceiling: u32) -> UsageLimitVerdict {
    if attempt >= ceiling {
        return UsageLimitVerdict::GiveUp;
    }
    let from = usage_limit_starving_from(ceiling);
    if attempt >= from && (attempt - from) % usage_limit_escalate_every(ceiling) == 0 {
        return UsageLimitVerdict::Starving;
    }
    UsageLimitVerdict::Wait
}
