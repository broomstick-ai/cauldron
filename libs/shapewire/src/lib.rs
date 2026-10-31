//! # shapewire — the subscription-shape chassis, in the cauldron tree
//!
//! A producer holds state its consumers mirror. This crate does that once:
//! cells that converge (`fold`), a hub that stamps every input exactly once and
//! folds it in arrival order (`hub`), a subscriber ledger (`ledger`), typed
//! commands whose result arrives as a fact on a shape (`command`), the one
//! network door every producer serves (`server`, `resource`), and the cauldron
//! seam a producer registers through (`seam`, `warnings`).
//!
//! The contract is `docs/shapes.md` in substrate-interface-layer (rules
//! 2.1–2.9); this crate is its second implementation, rewritten for this tree
//! from `lib/shapewire` at `bc4ba24`. The WIRE is unchanged — every frame a
//! face sends or receives is byte-identical for the same inputs, and the seam
//! reference kit under `seam/` is the instrument that says so. What changed is
//! what sits under it: the data root, the environment name, the bind address,
//! the boot token and the warnings attestation all come from cauldron's `env`
//! crate, and nothing in here reads `$HOME` or `$CAULDRON_ENV` itself.
//!
//! ## Who links what
//!
//! | Module | Who needs it | Flag |
//! |---|---|---|
//! | [`wire`], [`mirror`], [`envelope`], [`operation`], [`cell`], [`surface`], [`token`], [`mcp`] | a CONSUMER | none — the floor |
//! | [`fold`] | a producer authoring a shape | `fold` |
//! | [`hub`] | a producer's runtime | `hub` (needs tokio) |
//! | [`ledger`] | a producer; a reader tool | `ledger` |
//! | [`command`] | a producer | `command` |
//! | [`server`], [`resource`] | a producer's door | `server` |
//! | [`seam`], [`warnings`] | a producer under CAULDRON | `seam` (needs `env`) |
//!
//! The floor is what a face vendors, and it must build from `src/` with a
//! manifest the face writes itself; so no floor module may name `env`, tokio
//! or axum — and `--features server` may not name `env` either, which is why
//! the cauldron pair sits behind a flag of its own. `check-features.sh` keeps
//! both true.

#![forbid(unsafe_code)]

pub mod wire;
pub mod mirror;
pub mod envelope;
pub mod operation;
pub mod cell;
pub mod surface;

/// **THE TWO SUBTREE RULES** every face of `threads` applies — recents hide
/// subagents, and a parent's status rolls up from its subtree (R9.UI1). In the
/// floor so the producer and every face link one implementation.
pub mod subtree;

/// **A THREAD'S STATUS ROLLED UP OVER ITS WHOLE SUBTREE** — `active`, `idle`
/// or an error state, the count of running descendants, and the ONE display
/// string every face renders (R9.TS1). In the floor beside [`subtree`].
pub mod rolled;

/// **THE RUN TOKEN — signed claims saying which thread the caller IS.** The
/// claims and the hex helpers are in the floor (a consumer may READ a token);
/// the key, the mint and the verify are behind `server` with the `sha2` they
/// need. Moved from substrate `lib/shapewire/src/token.rs` (R3.S2.0.finish).
pub mod token;

/// **THE MCP DOOR'S PROTOCOL CORE** — JSON-RPC framing, version negotiation,
/// the five methods, and the [`mcp::McpTools`] trait a producer implements.
/// In the floor because it takes no axum; the HTTP handlers are `mcp::http`,
/// behind `server`. The operator's one carve-out from the no-MCP rule: the
/// built-in tools threads and the soup expose TO AGENTS. Moved from substrate
/// (R3.S2.0.finish).
pub mod mcp;

#[cfg(feature = "fold")]
pub mod fold;

#[cfg(feature = "hub")]
pub mod hub;

#[cfg(feature = "ledger")]
pub mod ledger;

#[cfg(feature = "command")]
pub mod command;

#[cfg(feature = "server")]
pub mod server;

#[cfg(feature = "server")]
pub mod resource;

/// The cauldron seam: `$CAULDRON_BIND`, the line protocol, the fixed-port
/// rule, and the registration that carries the warnings attestation.
///
/// Behind `seam`, not `server`: the door is wire-identical to substrate's and
/// does not know what supervises it, and `env` is reachable from here and
/// nowhere else. See `Cargo.toml`.
#[cfg(feature = "seam")]
pub mod seam;

/// The warnings a producer owes cauldron, counted by call site.
#[cfg(feature = "seam")]
pub mod warnings;

// ── The crate root: every name a producer reaches for, at one path ──────────

pub use wire::{
    CellInfo, CellState, ConnId, ConsumerEntry, Derived, Detail, Epoch, FieldInfo, Link,
    MembersPage, Op, Resolving, ShapeContract, ShapeError, ShapeFrame, ShapeInfo, ShapeMax,
    ShapeState, ShapeWire, Stamped, Superseded, Timestamp,
};

pub use envelope::{
    Bearer, ChassisMsg, ChassisServerMsg, Coded, HealthWire, Identity, Outcome, Refusal,
};

pub use operation::{DType, Declared, Operation, Param, Presence, Scalar, TimeEncoding, TimeUnit};

pub use cell::{Cell, Cells};

pub use surface::{Carried, Dialable, Dialed, Method};

pub use token::Claims;

pub use mcp::{McpTools, ToolDef};

pub use mirror::{
    Ask, Declaration, Disposition, Enumeration, Era, Held, KeyState, Mirror, OverCeiling,
    Receipt, Roster, SubscribeStatus, VersionMismatch,
};

#[cfg(feature = "server")]
pub use server::{Bearers, Chassis, ExtraFrame, Producer, ProducerControl, ProducerStream};

#[cfg(feature = "server")]
pub use resource::{ByteRange, Digest, Found, ResourceId, Resources, Retention, Stored, Uploads};

#[cfg(feature = "fold")]
pub use fold::{
    Counter, Edge, Join, Map, Membership, Min, Outbox, Register, Root, Semilattice, Sequence,
    Transition,
};

#[cfg(feature = "hub")]
pub use hub::{
    Fifo, Handles, Hub, Hydrate, HydrateRequest, Hydration, Item, Kind, Shape, SubscribeItem,
    Tracking, Tx, DEFAULT_FIFO_CAPACITY, MAX_ARMED_TIMERS_PER_CONN, MAX_FUTURE_AT,
    REINIT_FLOOR_SECS,
};

#[cfg(feature = "command")]
pub use command::{
    barrier, Command, Durable, DurableContract, NeverVisible, Promise, PromiseBook, PromiseId,
    Resolves, BARRIER_METHOD, MAX_PROMISES_PER_BARRIER,
};

#[cfg(feature = "seam")]
pub use seam::{Registration, SeamError, FACES_ENV};
