//! `seamdiff` — the reference diff, as a library so its own tests and the CLI
//! beside it drive the same code.
//!
//! The engine is a SEPARATE FILE with no `mod` statements and no crate
//! attributes in it, so each face's `tests/seam_reference.rs` in the consumer
//! repo can `include!` it verbatim:
//!
//! ```ignore
//! include!("../../../libs/shapewire/seam/tools/seamdiff/src/engine.rs");
//! ```
//!
//! That is how one engine guards two copies without either copy taking a
//! dependency on the other or on this crate — the same rule the reference
//! itself obeys (`docs/consumer-seam.md` §7.1: *"the reference copy is data for
//! a test, never a `use`"*), applied one level up.

#[path = "engine.rs"]
mod engine;

pub use engine::*;
