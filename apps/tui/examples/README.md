# The three probes that measured a door this face no longer has

`probe.rs`, `scope_proof.rs` and `subagent_reclaim.rs` were deleted when this
face stopped asking the daemon questions. All three were read-only instruments
pointed at the LIVE daemon, and all three were built out of `transport::Handle::call`
plus `methods::{LIST, STATUS, get, history}` — a door and a name table that no
longer exist. They are recoverable from git history at `2574070d`.

They are recorded here rather than silently dropped, because each one reached a
FINDING and the finding is the part worth keeping:

* **`probe.rs` — `threads/{id}/history`, not `threads/{id}/query`.** `0023`'s
  architecture spec named `query`; measured against the live daemon, `query`
  reaches about 1% of a busy thread's ledger, because
  `store.rs::query_messages` filters `message_id IS NOT NULL` and the daemon
  assigns a message id only to top-level `assistant_text`, `compacted` and
  accepted user messages. On the busiest thread in the fleet (`last_seq`
  175,428) it reported `total_matched` 1,938 and returned three kinds. Tool
  calls, thinking, agents and task progress were structurally unreachable
  through it.

  **The finding is now moot in the best way.** Neither call is made: the open
  thread's rows come off `threads/log`, which is the producer's own 4,000-row
  window of EVERY kind, unfiltered. The question *which read reaches all the
  events* has no reads left to choose between.

* **`scope_proof.rs` — the fleet predicate is a change of QUESTION, not a
  filter** (`IS-20`). It asked the daemon for every thread it held and counted
  what survived set membership against the project roots. The question moved
  with the window: the fleet this face can see is now
  `threads/recent-updates` ∪ `accounts.*.threads`, which is the producer's
  choice and not a `limit` this face picked — so there is no 1,000-row page to
  prove a predicate over.

* **`subagent_reclaim.rs` — the subagent lens reclaims most of the chat area.**
  The measurement put a stated number to a test, and the lens it
  measured is in `store.rs` and unchanged by this migration. What the probe
  needed and cannot have is `threads/list` plus a `history` page per thread.

`real_ledger_probe.rs` survives: it reads a scratch JSON dump off disk and
never opens a socket, so it was never built on the door that went away.

**If one of these is wanted back**, it is a short job and a different one: point
it at `seam::Seam::start(None)`, hold the keys it wants, and read the mirror —
which is less code than the paging loop it had, and which cannot reach a thread
the producer is not already exporting.
