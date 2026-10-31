//! # The supervisor owns every MCP tool — R5.S2 (DEC-34)
//!
//! The operator's ruling (`cc-session:<session>#H135`): the cauldron supervisor
//! owns ALL MCP tools. They are version-controlled, so the supervisor hosts
//! them rather than letting each one run unmanaged; never more than one is
//! awake at a time; one is woken on demand when a tool is given to an active
//! thread; and the tools route to libs or crates rather than staying resident
//! as a held binary — *"I'm open to being wrong on the last point."*
//!
//! Three modules, one each for the three halves of that sentence:
//!
//! | module | what it owns |
//! |---|---|
//! | [`decl`] | THE DECLARATION: one `mcp/<name>.toml` per tool server, in the repository, so a tool is a version-controlled thing and not whatever happens to be running |
//! | [`host`] | THE HOST: wake on demand, and the at-most-one-awake invariant, held STRUCTURALLY as one `Option` slot |
//! | [`wire`] | THE SEAM WORDS: `MCP_TOOLS`, `MCP_CALL`, `MCP_STATUS`, `MCP_SLEEP` on the supervisor's one-word line protocol, which is how the threads `/mcp` façade routes a call through the supervisor |
//!
//! ## Residency — the point he holds loosely, and what was chosen
//!
//! A declaration says `residency = "in-process"` or `residency = "process"`.
//!
//! * **in-process** — the tool is OURS and is a library call inside a service
//!   that already runs (`provider` names it). The `agents_*` façade is the one
//!   such server today: it is code in `services/threads`, so there is no
//!   process to wake, nothing for the at-most-one slot to hold, and routing it
//!   through a second process would add a hop and a failure mode for no
//!   isolation. The supervisor still HOLDS its declaration (`mcp/agents.toml`)
//!   and reports it, and refuses to serve it itself.
//! * **process** — an EXTERNAL tool server, a stdio MCP binary we do not own.
//!   The supervisor spawns it on the first call that needs it and puts the
//!   previous one to sleep (kills it) — so at most one external tool process is
//!   awake at any instant.
//!
//! That is the recommendation the brief carried: route to in-process libs and
//! crates where a tool is ours, wake a process only for an external one.

pub mod decl;
pub mod host;
pub mod wire;

pub use decl::{Residency, ToolDecl, ToolSpec};
pub use host::{CallError, ToolHost, Wake};
