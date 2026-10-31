//! The transport proto version — all that is left of the legacy wire protocol.
//!
//! This file held the legacy door's whole frame vocabulary: `ClientMsg`
//! (`auth`, `request`, `subscribe`, `unsubscribe`), `ServerMsg` (`welcome`,
//! `response`, `event`, `liveness`, `status`, `progress`, `lifecycle`, `info`,
//! `sub_ack`, `lagged`, `shape`), the CCW `Outcome` switch and the `Lagged`
//! frame's per-thread replay cursors (`LagCursor`, `lag_frame`).
//!
//! **R2.S4 (2026-10-10) retired all of it with the `:3651` listener.** The
//! chassis owns the envelope now (`shapewire::envelope`: `ChassisMsg`,
//! `ChassisServerMsg`, the coded `Outcome`), and every fact the firehose pushed
//! is a shape: ledger events are `threads/log`, liveness and status are the
//! `threads` shape's cells, `progress`/`lifecycle`/`info` are `narration`, and
//! the once-per-connection boot note is `boot`. A shape consumer repairs a gap
//! by re-subscribing (`Hub::resubscribe_lagged`), so the `Lagged` cursors —
//! `docs/consumer-seam.md` §6 M7's producer half — have no reader left.
//!
//! [`PROTO`] stays because the chassis `Welcome` and `/health` still carry it
//! (`ThreadsProducer::new` builds the `Identity` from it).

/// Transport proto version (bumped on a breaking envelope change).
pub const PROTO: u16 = 1;
