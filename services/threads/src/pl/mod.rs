//! # `pl` — the thread ledger's adapter onto protoledger (PL-1)
//!
//! `docs/PROTOLEDGER-INTEGRATION.md` rev 3, §0.4.5 and the PL-1 row of §4.1.
//! **Nothing in the daemon calls this yet**: PL-3 (shadow writes, behind
//! `THREADS_PL_SHADOW=1`) is the first caller. Until then it is exercised only
//! by its own tests, in temp directories, and no call site's truth moves.
//!
//! **THE ONLY IMPORTER.** No file outside `src/pl/` may `use protoledger::…`
//! (§3 item 3). A pin bump then touches this module and nothing else.
//!
//! - [`store`] — [`PlStore`]: a `protoledger::store::Store` at
//!   `<data dir>/protoledger/` (`ledgers/` + `index.db`, both protoledger's),
//!   with the data-root refusal kept from the first PL-1 draft.
//! - [`write`] — the WRITERS for what cauldron itself records, the facts that
//!   are lost today (§0.4.5): the composed prompt as `text` with its CT meta,
//!   FULL `settings` snapshots, `peer_message`, a network join as its own
//!   event (including the join at spawn), namespaced `app` (summary) and the
//!   neutral `title`, and the `spawned-by` link at spawn.
//!
//! Every cauldron fact rides in [`CT`], cauldron's OWN namespace. protoledger's
//! worked example (`fixtures/protoledger/orchestrated-peers/`) now spells its
//! namespace `example.orchestrator` (rev 3 §0R3.2); that is upstream's example,
//! not ours, and a test fails if it ever appears in what we write.
//!
//! - [`shadow`] — PL-3: the shadow behind `THREADS_PL_SHADOW=1` (project,
//!   carry, follow, compare); the runtime's only door into this module.
//!
//! **Meanwhile workarounds** (features not in the pinned release, each tagged
//! `// PL-MEANWHILE(<feature>)` where it lives): none. Every API used here is
//! on protoledger `6b794ae8`. PL-3 adds one, `PL-MEANWHILE(PL-5 backfill)` in
//! [`shadow`]: the native transcript's new lines are CARRIED onto the
//! projection, because Claude does not yet resume the projection itself.

pub mod shadow;
pub mod store;
pub mod write;

#[cfg(test)]
mod tests;

pub use store::PlStore;
pub use write::{AppFact, NetworkJoin, PeerDirection, PeerMessage, Prompt, SettingsSnapshot, Spawn};

/// The metadata namespace every cauldron fact rides in (`CT` in §2), and the
/// prefix of every `app` type cauldron writes. NOT upstream's
/// `example.orchestrator` (§0R3.2).
pub const CT: &str = "cauldron.threads";

/// Re-exported read types, so callers of [`PlStore`] never name protoledger.
pub use protoledger::{Event, Kind, Link};
/// Where one append landed (`seq`, byte `offset`, `len`).
pub use protoledger::Appended;
