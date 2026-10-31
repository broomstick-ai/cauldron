//! Wave 1 of the Cauldron supervisor: **environments and seams**.
//!
//! An **environment** is a named triple — a *data root*, a set of *service
//! versions*, and a *seam table* — and production is just the environment
//! named `prod`. Nothing in this crate knows production as a special case;
//! the only thing that singles production out is [`guard`], and what it
//! guards is a *path on disk*, not an environment name.
//!
//! A **seam** is a line in a file. A service binds whatever port the kernel
//! gives it and writes its real bound address into the environment's
//! `seams.json`; a consumer reads that file. There is no resolver daemon, no
//! fixed port, and no discovery protocol.
//!
//! Two properties the layout buys, and they are the reason for it:
//!
//! 1. **A consumer cannot be pointed at the wrong environment by accident**,
//!    because the seam table it reads *is* the environment. The table lives
//!    inside the data root, every entry restates the `env` and `data_root` it
//!    was written under, and [`seams::Table::load`] refuses a table whose
//!    entries disagree with the directory it was loaded from.
//! 2. **A stale seam is detectable.** See [`seams::probe`]: the verdict is a
//!    handshake with the socket, never the recorded pid.
//! 3. **A service that does not report its own warnings is not a well-formed
//!    cauldron service — and it DOES NOT BUILD.** See [`warnings`]: serving a
//!    warnings shape is part of the contract, and the contract is enforced in
//!    two places that fail at two different times.
//!
//!    The MECHANISM is a compile-time check on the producer build.
//!    [`seams::ServiceEnv::register`] — the one call that writes a seam, and so
//!    the one call that makes a process visible to cauldron at all — takes an
//!    [`warnings::Attestation`], which can only be minted by
//!    [`attest_warnings!`], which does not type-check without an
//!    [`warnings::ServesWarnings`] impl. A producer with no warnings shape
//!    therefore does not link, and the compile error names the items it is
//!    missing. See `scripts/prove-compile-gate`.
//!
//!    The BACKSTOP is the runtime refusal: `cauldron start` asks a running
//!    service for the shape its declaration named and refuses to call it
//!    started if the answer does not check out, and `cauldron status` exits 4
//!    when it finds one running. That is what catches the two things a build
//!    cannot see — a binary that compiled fine and then lies, and a process
//!    started by something other than `cauldron start`.
//!
//!    Either way the supervisor aggregates by *subscribing like any other
//!    consumer* — never by sitting in the data path.

pub mod alias;
// R9.CFG1 (E18-19): `[cli] interface` — which binary PATH's `cauldron` runs.
pub mod cli_config;
pub mod decl;
pub mod egress;
pub mod fingerprint;
pub mod guard;
pub mod hold;
pub mod hosting;
pub mod identity;
pub mod lock;
pub mod paths;
pub mod seams;
pub mod supervisions;
pub mod tccd;
pub mod warnings;

pub use paths::Environment;
pub use seams::{Seam, SeamState, Table};
