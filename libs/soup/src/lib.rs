//! **The soup crate, minimal, for the first release** (REL-3.S1).
//!
//! The demo environment's data belongs in a real crate, not in a fixture: the
//! curated data that the teaser shows lives HERE, never in UI code, and
//! appears in exactly one environment, [`demo::ENV`] (`cauldron-build`). No other environment is ever
//! seeded from it ([`demo::seed_env`] refuses every other name).
//!
//! ## Why a minimal crate, and what replaces it
//!
//! Neither `main` nor `r9/globs-iteration-2` carries a soup. The two real soups
//! (`services/soup` on `ripple/v2`, and the `soup/phase-*` line reconciled on
//! `soup/phase-d`) are not on the release branch and cannot write yet (no
//! tranche lock, no lander). CONSOLIDATION Q-2 recommended "a minimal `soup`
//! data crate on the release branch ... replaced by whichever soup wins". This
//! is that crate. It deliberately carries the package name `soup`: when the
//! real soup merges, cargo refuses two packages of one name, so the merge
//! cannot happen without moving [`demo`] into the real crate. That refusal is
//! the reminder, by design.
//!
//! ## The read path
//!
//! The face reads a cauldron's data from the environment's own data root:
//! `<root>/data/<env>/tui/soup/seed/{clusters,artifacts,ledger}.json` (the
//! face's `store::own_seed`). [`demo::install`] writes the dataset THERE, in
//! the face's own seed formats, so the face reads `cauldron-build` through the
//! same path any hand-curated cauldron's data takes. There is no second read
//! path in the face, and nothing here is compiled into it.
//!
//! [`scrub`] is the guard every byte of the dataset passes: no home paths, no
//! e-mail addresses, no personal names, no secrets.

pub mod actions;
pub mod demo;
pub mod scrub;
