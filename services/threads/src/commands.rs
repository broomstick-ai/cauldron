//! # The typed commands — `docs/shapes.md` 2.4
//!
//! **A command returns an acknowledgement, never a fact.** Until now three
//! handlers broke that: `accounts.disable` and `accounts.enable` answered
//! `pool_share_rows`, and `gauge.probe` answered the gauge state. Each reply was
//! a SECOND SOURCE for a value the shape already carries, and while two ways to
//! learn a fact exist, every disagreement between them is ambiguous — which is
//! exactly what S3 deleted from the read path and left standing on the write
//! path.
//!
//! The doc named two. There were three: `accounts.enable` is the same violation
//! in the same file, and fixing its sibling while leaving it would have left the
//! rule half-true.
//!
//! Each command names the FACT that resolves it. The marker types below stand in
//! for `Fact`'s variants because `Fact` is one enum and `Command::Resolves` must
//! be a distinct type per command — otherwise any fact would close any promise.

use shapewire::command::{Command, NeverVisible, Resolves};

use crate::shapes::accounts::Accounts;
use crate::shapes::threads::Threads;

// ─────────────────────────────────────────────────────────────────────────────
// WHO EMITS WHAT — the declaration `Command::ResolvedBy` demands
// ─────────────────────────────────────────────────────────────────────────────
//
// `shapewire::command::Resolves<E>` is implemented by the SHAPE whose fold emits
// `E` with a resolve. It is what makes "something on this producer can close this
// promise" a fact in the command's definition instead of a runtime discovery: a
// new command whose fact nothing emits does not compile, and rustc says so with
// `WHAT IS MISSING` naming both ways out.
//
// The impls live HERE rather than beside each shape, deliberately. A shape's file
// is about what it CARRIES; this file is about what a command PROMISES, and the
// pairing is a statement about the command. Keeping them together also keeps the
// whole declaration readable in one screen, which is the point of declaring it.
//
// **WHAT THESE IMPLS DO NOT CLAIM.** That the fold reaches the emit, on that
// path, with tracking on, about the right subject. That is a test — the chassis's
// own liveness property was written three times before it could fail — and
// `every_command_through_a_real_producer_leaves_no_promise_open` is where it
// lives. These say the PATH exists.

impl Resolves<AccountDisabledFact> for Accounts {}
impl Resolves<PoolShareFact> for Accounts {}
impl Resolves<ThreadMessageFact> for Threads {}
impl Resolves<ThreadSeenFact> for Threads {}
impl Resolves<ThreadSettingsFact> for Threads {}
impl Resolves<ThreadExistsFact> for Threads {}
impl Resolves<ThreadQueueFact> for Threads {}
impl Resolves<TranscriptReconciledFact> for Threads {}

/// Resolved by [`crate::event::Fact::AccountDisabled`].
#[derive(Debug)]
pub struct AccountDisabledFact;
/// Resolved by [`crate::event::Fact::PoolShare`].
#[derive(Debug)]
pub struct PoolShareFact;

/// **The marker `runtime.rs` spends on every keyed gauge observation.**
///
/// It is NO COMMAND'S `Resolves` any more: the only command that named it was
/// `gauge.probe`, whose promise is subjectless and which therefore declares
/// `NeverVisible` (see below). It stays because the emit site still names it —
/// `resolve::<GaugeObservedFact>(Some(account))` — and a future keyed command
/// about one account's gauge would name it again, with
/// `type ResolvedBy = Accounts`.
#[derive(Debug)]
pub struct GaugeObservedFact;

/// `accounts.disable` — the account leaves the pool.
pub struct AccountsDisable;
impl Command for AccountsDisable {
    const NAME: &'static str = "accounts/{account}/disable";
    type Resolves = AccountDisabledFact;
    type ResolvedBy = Accounts;
}

/// `accounts.enable` — it comes back, at the share it never stopped having.
///
/// Resolved by the same fact as its sibling: `AccountDisabled` carries the
/// STATE, not the transition, so an enable is the arrival of `disabled: false`.
pub struct AccountsEnable;
impl Command for AccountsEnable {
    const NAME: &'static str = "accounts/{account}/enable";
    type Resolves = AccountDisabledFact;
    type ResolvedBy = Accounts;
}

/// **`gauge.probe` — poll `/usage` now, AND NOTHING CLOSES ITS PROMISE.**
///
/// It declares `NeverVisible`, which is the honest statement of what it has always
/// done: the handler opens the promise, abandons it on the way out of both arms
/// and replies `visible: false`. Before this declaration that was a runtime
/// discovery a reader had to find in `server.rs`; it is now in the definition.
///
/// **READ THE REASON CAREFULLY, BECAUSE THE MARKER TRAIT DID NOT FIND THIS ONE.**
/// `GaugeObservedFact` IS emitted — `runtime.rs` resolves it per account, per axis
/// — so `impl Resolves<GaugeObservedFact> for Accounts` would type-check and would
/// be a lie. The mismatch is in the SUBJECT, not the type: this command's promise
/// is subjectless (*"poll the pool now"*), a keyed fact closes only keyed promises,
/// and no shape-level fact exists. `Command::ResolvedBy` encodes the type
/// dimension and not the subject dimension, so this case is caught by the author
/// writing down what is true rather than by the compiler refusing what is false.
///
/// **WHAT WOULD MAKE IT `visible: true`**, unchanged: one keyless fact meaning
/// *the sweep completed*, emitted with `resolve::<..>(None)` as an ARGUMENT of the
/// emit. That is a new `Fact` variant and a new `shared` cell, and when it exists
/// this declaration becomes `type Resolves = ThatFact; type ResolvedBy = Accounts;`.
pub struct GaugeProbe;
impl Command for GaugeProbe {
    const NAME: &'static str = "gauge/probe";
    type Resolves = NeverVisible;
    type ResolvedBy = NeverVisible;
}

/// `accounts.pool_share.set` — the share this account lends the pool moves.
///
/// A FOURTH violation, found by sweeping every mutation-shaped handler for one
/// that returns a computed value rather than an acknowledgement. The doc named
/// two; there were four. Same rule, same fix.
pub struct PoolShareSet;
impl Command for PoolShareSet {
    const NAME: &'static str = "accounts/{account}/pool_share_set";
    type Resolves = PoolShareFact;
    type ResolvedBy = Accounts;
}

// ─────────────────────────────────────────────────────────────────────────────
// THE FIVE THAT REMAINED — 2026-09-28
// ─────────────────────────────────────────────────────────────────────────────
//
// `docs/shape-surface.md` §5.2 named them and verified each: `threads.event`
// answered `message_id`, `threads.seen` answered the whole fresh status,
// `threads.create` answered an allocated position, `threads.update` answered the
// applied settings, `budgets.set` answered the budget snapshot. Every one of
// those values is state, and every one of them is a SECOND SOURCE for something
// a consumer can already learn — or, in `budgets.set`'s case, for something no
// shape carries yet, which is a different finding and is recorded as one.

/// Resolved by the ledger event that carries the addressable position —
/// `Push::Event` with `message_id: Some(_)`.
///
/// **NOT a `Fact`.** The `threads` shape folds the ledger envelope itself:
/// `ThreadCells::last_message_id` and `newest` are written out of
/// `Input::Push(Push::Event(_))`, and `ThreadCells::app` out of the same input's
/// `Custom` payload. Adding a `Fact` to carry it would give that cell a second
/// writer for the sole purpose of resolving a promise, which is the collapse the
/// rule is against wearing the rule's own clothes. So this marker is spent at the
/// PUSH emit, through `Hub::emit_push_resolving`.
#[derive(Debug)]
pub struct ThreadMessageFact;

/// Resolved by [`crate::event::Fact::ThreadSeen`].
#[derive(Debug)]
pub struct ThreadSeenFact;

/// Resolved by [`crate::event::Fact::ThreadSettings`].
#[derive(Debug)]
pub struct ThreadSettingsFact;

/// **Resolved by the pool's MEMBERSHIP statement — `Derived::Added`.**
///
/// A prior dispatch concluded that `create_thread` deliberately emits no fact
/// because "at this instant no consumer can hold this thread's key, no fold can
/// be holding a cell for it, and the first consumer to name it reads these three
/// values out of the store in `hydrates_key`". That was true and **it is false
/// now**: `Threads::folds` records `tx.rows.member(id)` BEFORE every `holds`
/// guard, so the `Push::Status` `create_thread` already emits yields one
/// `Derived::Added` — the pool telling a `detail: keys` subscriber that this
/// thread exists. That is precisely the fact `threads/create` causes, and it is
/// the ONLY frame a brand-new key produces while nobody holds it, which is why
/// `hub.rs`'s `added` loop now carries promise ids the way `gone` always has.
#[derive(Debug)]
pub struct ThreadExistsFact;

/// `threads/{thread_id}/event` — an application event lands in the ledger.
pub struct ThreadsEvent;
impl Command for ThreadsEvent {
    const NAME: &'static str = "threads/{thread_id}/event";
    type Resolves = ThreadMessageFact;
    type ResolvedBy = Threads;
}

/// `threads/{thread_id}/seen` — the unread cursor advances.
pub struct ThreadsSeen;
impl Command for ThreadsSeen {
    const NAME: &'static str = "threads/{thread_id}/seen";
    type Resolves = ThreadSeenFact;
    type ResolvedBy = Threads;
}

/// `threads/{thread_id}/update` — the settings blob moves.
pub struct ThreadsUpdate;
impl Command for ThreadsUpdate {
    const NAME: &'static str = "threads/{thread_id}/update";
    type Resolves = ThreadSettingsFact;
    type ResolvedBy = Threads;
}

/// `threads/create` — a thread that did not exist does.
pub struct ThreadsCreate;
impl Command for ThreadsCreate {
    const NAME: &'static str = "threads/create";
    type Resolves = ThreadExistsFact;
    type ResolvedBy = Threads;
}

/// **`budgets/{key}/set` — AND THIS PRODUCER HAS NOTHING THAT CAN RESOLVE IT.**
///
/// It declares `NeverVisible`, and that is the honest state of the surface rather
/// than a stub: **no shape on this producer carries a budget.** Every `Fact`
/// variant is about an account, a login, the pool or a thread; `shapes::accounts`
/// has cells for the share, the gauge, the limit, health, the credential and
/// pressure, and none for a budget key's config, its spend or its trip state.
///
/// The command reaches `accepted`, never `visible`, says so on the reply, and
/// CLOSES ITS OWN PROMISE on the way out rather than leaving an id open forever.
/// What would make it `visible: true` is a `budgets` shape — see
/// `docs/consumer-migration-ledger.md`, where it is filed as the finding it is
/// and not as a gap in this conversion.
///
/// **THIS IS THE CASE THE MARKER TRAIT ACTUALLY CATCHES.** It is a TYPE-level
/// hole: nothing anywhere emits a budget fact. `BudgetConfigFact` — a marker type
/// no emit site ever spent, which existed only to name the hole — is deleted,
/// because `NeverVisible` names it in the chassis's own vocabulary and an
/// uninhabited type says it better than an inhabited one nobody constructs. Had
/// somebody later written `type Resolves = SomeRealFact` here without a fold to
/// emit it, this is where the build would stop.
pub struct BudgetsSet;
impl Command for BudgetsSet {
    const NAME: &'static str = "budgets/{key}/set";
    type Resolves = NeverVisible;
    type ResolvedBy = NeverVisible;
}

// ─────────────────────────────────────────────────────────────────────────────
// THE FOUR THAT STILL ANSWERED WITH A FACT — 2026-10-10 (R2.S5)
// ─────────────────────────────────────────────────────────────────────────────
//
// `queue_cancel` answered `{thread_id, message_id, cancelled}`, `queue_reselect`
// answered `{thread_id, reselected: <count>}`, `reconcile_transcript` answered
// the whole port outcome, and `budget_assign` answered `{thread_id, budget_key}`.
// Every one of those is either a value a `threads` cell already carries or a
// fact about the CALL that `Accepted::changed` exists to carry. None of the four
// needed `NeverVisible`: each writes something the `threads` shape folds.

/// **Resolved by [`crate::event::Fact::ThreadQueue`]** — the whole undelivered
/// set for one thread, re-read after the write, which `ThreadCells::queue` holds
/// as a level.
///
/// Spent ONLY on the emits the two queue commands cause, never on the
/// `emit_status` every other queue write reaches: an unrelated status edge
/// landing between this promise's `open` and its write would otherwise close it
/// on a level that does not yet contain the write. See
/// `ThreadManager::emit_thread_queue`'s `answers` argument.
#[derive(Debug)]
pub struct ThreadQueueFact;

/// **Resolved by the ledger event `transcript_reconciled`** — `Push::Event`
/// carrying `ThreadEvent::Custom`, which `Threads::folds` writes into
/// `ThreadCells::app["transcript_reconciled"]` with the port outcome as its value.
///
/// **NOT a `Fact`, and NOT [`ThreadMessageFact`]**, for two different reasons.
/// Not a `Fact` for `ThreadMessageFact`'s own reason: the `app` cell has one
/// writer, the push arm, and a `Fact` twin would give it a second. Not
/// `ThreadMessageFact` because a promise matches on (type, subject): sharing the
/// marker would let a reconcile's ledger append close a `threads/{id}/event`
/// promise on the same thread before that event had landed, and vice versa.
#[derive(Debug)]
pub struct TranscriptReconciledFact;

/// `threads/{thread_id}/queue_cancel` — a still-waiting queued row leaves the
/// queue. Resolved by the queue level re-read after the write: the row is gone
/// from `ThreadCells::queue`. Whether a row was actually cancelled is a fact
/// about the CALL and rides `changed`.
pub struct ThreadsQueueCancel;
impl Command for ThreadsQueueCancel {
    const NAME: &'static str = "threads/{thread_id}/queue_cancel";
    type Resolves = ThreadQueueFact;
    type ResolvedBy = Threads;
}

/// `threads/{thread_id}/queue_reselect` — `failed` rows go back to `waiting`.
/// Resolved by the same queue level: the rows' `state` word moves in
/// `ThreadCells::queue` (and then they leave it, as the dispatcher delivers
/// them). How many moved was the reply's `reselected`; whether ANY moved is
/// `changed`, and the rows themselves are on the cell.
pub struct ThreadsQueueReselect;
impl Command for ThreadsQueueReselect {
    const NAME: &'static str = "threads/{thread_id}/queue_reselect";
    type Resolves = ThreadQueueFact;
    type ResolvedBy = Threads;
}

/// `threads/{thread_id}/reconcile_transcript` — an externally advanced
/// transcript is merged into the thread's current account store. Resolved by
/// the `transcript_reconciled` ledger event, whose payload — the outcome, the
/// account, the event counts — is exactly what the reply used to carry and is
/// what `ThreadCells::app` now holds under that event type.
pub struct ThreadsReconcileTranscript;
impl Command for ThreadsReconcileTranscript {
    const NAME: &'static str = "threads/{thread_id}/reconcile_transcript";
    type Resolves = TranscriptReconciledFact;
    type ResolvedBy = Threads;
}

/// **`threads/{thread_id}/budget_assign` — AND IT IS NOT `BudgetsSet`'s CASE.**
///
/// The dispatch that converted it expected `NeverVisible`, by analogy with
/// `budgets/{key}/set`. The analogy does not hold: that command writes a
/// budget KEY's config, which no shape carries; this one writes a THREAD's
/// `settings.budget_key`, through `ThreadManager::update_settings`, which emits
/// `Fact::ThreadSettings` with the whole blob — and `ThreadCells::settings`
/// holds it. So it is resolved by [`ThreadSettingsFact`], the same fact as
/// `threads/{thread_id}/update`, because it IS a settings write.
///
/// **Sharing the marker with `ThreadsUpdate` is correct, and the consequence is
/// stated:** both answer "the settings blob after my write", so a settings emit
/// on that thread closes both kinds of promise. That is the existing contract
/// of `emit_thread_facts`, not a new one.
pub struct ThreadsBudgetAssign;
impl Command for ThreadsBudgetAssign {
    const NAME: &'static str = "threads/{thread_id}/budget_assign";
    type Resolves = ThreadSettingsFact;
    type ResolvedBy = Threads;
}
