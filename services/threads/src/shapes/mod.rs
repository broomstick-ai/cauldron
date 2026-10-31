//! Shapes registered on the `threads` producer.
//!
//! Twelve, and the split between them is the operator's ruling that a general
//! shape must not encode one application's meaning:
//!
//! | shape | key | scope | what it is |
//! |---|---|---|---|
//! | [`accounts`] | account name | `Fleet` | the account board's one source |
//! | [`threads`] | thread id | `OnDemand` | GENERAL: one thread in depth, app payloads held opaquely |
//! | [`summary`] | thread id | `Active` | APPLICATION-SPECIFIC: `summary_updated` is a thread's name. Nested under `threads` |
//! | [`narration`] | channel | `Fleet` | the daemon talking while it works — `Progress`, `Lifecycle`, `Info` |
//! | [`recent`] | the one pool, `fleet` | `Whole` | INDEX: the threads that moved most recently, in activity order. Nested under `threads` |
//! | [`recent_visits`] | the one pool, `fleet` | `Whole` | INDEX: the threads most recently VISITED (`mark_seen`), in visit order. Nested under `threads` |
//! | [`nonidle`] | the one pool, `fleet` | `Whole` | INDEX: the threads that are NOT idle, and where each is placed. Nested under `threads`. It exists so [`accounts`] need not hold six fields of [`threads`]' rows under an account key |
//! | [`gauge_history`] | account name | `PerKey` | an account's gauge READINGS over the last seven days — the sparkline's series. Nested under `accounts` |
//! | [`log`] | thread id | `PerKey` | GENERAL: one thread's LIVE EVENT LOG, a bounded window over the ledger, every kind, held opaquely. Nested under `threads` |
//! | [`warnings`] | the one producer, `daemon` | `Whole` | the PRODUCER reporting on itself: how many warnings it has emitted since boot, and the few call sites that dominate |
//! | [`heartbeat`] | the one link, `link` | `Whole` | the LINK is carrying frames — a beat that travels the FIFO, the fold and the connection's channel, which a websocket ping does not |
//! | [`boot`] | the one producer, `daemon` | `Whole` | the PRODUCER's account of ITSELF: why THIS process is the one you reached. The only shape here whose content predates every subscription, so the only one whose fold has no live arm |
//!
//! Together with `threads`' `liveness` cell, every `Push` variant has a home:
//! `Event` and `Status` and `Liveness` in [`threads`], the other three in
//! [`narration`]. That is what a consumer needs before it can stop subscribing
//! to the legacy firehose, and it is why `Liveness` got designed rather than
//! ignored.
//!
//! **[`heartbeat`] is not part of that accounting and must not be read as it.**
//! It homes no `Push` variant — it folds one input nothing else produces, and it
//! answers a question none of the five answer: not *is this thread busy*
//! (`threads`' `liveness` cell) and not *is the socket open* (the websocket
//! ping), but *are frames still reaching me*. Its module doc carries the table
//! that keeps the three apart.
//!
//! **[`boot`] is not part of that accounting either, and for a sharper reason:
//! it homes no `Push` variant because it folds NOTHING.** The accounting above
//! is about variants that ARRIVE and need somewhere to land. The boot note is
//! the one piece of the legacy wire that never arrived at all: `on_boot` computes
//! it before the server is listening, so the firehose REPLAYED it on subscribe
//! rather than broadcasting it. A variant home could not have held it, which is
//! why completing that accounting did not finish the job — see [`boot`]'s module
//! doc, and the two replay sites it exists to make removable
//! (`server.rs`'s `drive` and `chassis.rs`'s `ThreadsControl::client_frame`).
pub mod accounts;
/// **WHY THIS PROCESS IS THE ONE YOU REACHED** — the boot note, which was
/// firehose-only because the fact predates every subscription. The last thing on
/// the legacy wire with no shape home. See its module doc for why `narration`'s
/// one-hour window could not hold it and why `warnings`' row could not either.
pub mod boot;
/// **NOT A SHAPE.** What a message BODY is, how a shape mints an id for one,
/// and what the summary that names it carries — the producer half of
/// `shapewire::resource` for this daemon's first resource. It is here
/// rather than beside `store.rs` because deciding what counts as a body is a
/// shape's judgement about its own cells, and the store holds bytes.
pub mod body;
/// **AN ACCOUNT'S GAUGE READINGS OVER THE LAST WEEK** — the series
/// `accounts.gauge` (a level, `retained_items: 0`) cannot hold, and the carrier
/// that retires the `gauge/history` request (R2.S3d). Keyed per account, `PerKey`,
/// nested under [`accounts`].
pub mod gauge_history;
pub mod heartbeat;
/// **A THREAD'S LIVE EVENT LOG, AS A BOUNDED WINDOW** — the 58% of the ledger no
/// shape carried, and the gate on retiring the legacy firehose. Keyed per thread,
/// `PerKey`, nested under [`threads`].
pub mod log;
pub mod narration;
/// **NOT A SHAPE.** A thread's MESSAGE PAGES — the resource ids `threads`'
/// `pages` cell carries and the bytes the resource door serves for each, so a
/// historical message range is read through the door rather than requested
/// (R2.S3d).
pub mod pages;
/// **THE NON-IDLE ROSTER, KEYED BY THE POOL** — the index that exists so
/// [`accounts`] does not have to hold six fields of [`threads`]' rows under an
/// account key. See its module doc for the argument, including why
/// ids-plus-a-count on [`accounts`] could not answer the one question the cell
/// is read for.
pub mod nonidle;
pub mod recent;
/// The index of recent VISITS — `recent`'s sibling, ordered on visit time.
pub mod recent_visits;
pub mod summary;
pub mod threads;
/// The producer's own warning count — see its module doc for why it is a shape
/// of its own and not a cell on `heartbeat` or `narration`.
pub mod warnings;
