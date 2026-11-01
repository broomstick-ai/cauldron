//! **`tui` — the terminal face of the harness, as a LIBRARY.**
//!
//! # What this crate is, and what it was
//!
//! This lib target was born in build step 2 for a narrow reason: the
//! transport's three hard parts — `boot_id`-discriminated reconnect, `Lagged`
//! recovery, the outbound drain — all fail SILENTLY when they are wrong, the
//! proof is "which RPCs did the client issue, in what order, on which
//! connection", and **an integration test cannot reach into a binary crate**.
//! So the parts that spoke the wire became a library and the parts that owned
//! a terminal stayed in `main.rs`.
//!
//! That line was drawn by a TEST need, and it left the library holding the
//! daemon client — `proto`, `store`, `transport` — while the things anybody
//! would call *the core behaviors of the TUI* sat in the binary where nothing
//! outside that one `main` could reach them: the terminal lifecycle, the event
//! loop, the repaint law, input dispatch, follow mode. **The test of a real lib
//! is whether a second face could be built on it, and the answer was no — for
//! a reason that was a `mod` keyword, not a design.** `0037` moved the line:
//! every core behavior is now a public module here, and `main.rs` is an entry
//! point that parses argv, opens a log, and hands over.
//!
//! # The module map, and what each module may not touch
//!
//! **The wire**
//! * [`proto`] — a local, serde-TOLERANT mirror of the daemon's wire
//!   (`DEC-10`), with no dependency on the `threads` crate. Knows nothing
//!   about screens.
//! * [`transport`] — one WebSocket, the reconnect ladder, the RPC correlation
//!   layer, the delivered-seq cursors and the gap repairs. Owns the only
//!   per-thread cursors in the crate, because they are meaningless away from
//!   the socket that gives them meaning.
//! * [`reads`] — every RPC this face issues that is a READ, and the module
//!   header is that claim; it used to be a grep over the event loop.
//! * [`aui`] — **THE SECOND SOCKET**, and the write side only: the AUI's
//!   proto-v2 mutation listener on port 9010. Four of the thread sheet's six
//!   acts change state the AUI owns and the daemon has no word for, so they go
//!   to the AUI directly (`0060`, the owner's console). Nothing the face
//!   RENDERS depends on this connection; unreachable is a drawn state, not an
//!   error path.
//! * [`acts`] — **the owner binding, made machine-checkable.** One pure
//!   function maps a sheet act to the bytes that leave the process, and says
//!   which of the two sockets they leave by. It is a separate module for the
//!   same reason `sends` is separate from `reads`: a claim is worth more when
//!   it is a file boundary than when it is a promise.
//! * [`sends`] — every RPC that is a MUTATION, and there is exactly ONE:
//!   `threads/{id}/send`. It is a separate module from `reads` for the same reason
//!   `reads` exists at all — a claim is worth more when it is a file boundary
//!   than when it is a promise, and splitting them keeps `reads`'s own claim
//!   true rather than putting a hole in it.
//!
//! **The model**
//! * [`store`] — TWO things, and it is honest about being two: the in-memory
//!   session model (a droppable fleet cache, the open thread's ledger tail, a
//!   bounded activity tail) and the PROJECTION of all of it into the pure
//!   crate's `Block`/`Header`/`ThreadList` vocabulary. The projection is pure
//!   and could live in `tui-render`; it does not, because it reads
//!   [`proto::ThreadEvent`] and `tui-render` may not know the wire exists.
//!   That is the boundary's cost, and it is recorded rather than hidden.
//!
//! **The face**
//! * [`event`] — the internal event vocabulary. Four modules speak it, so it
//!   belongs to none of them.
//! * [`vt`] — **the pre-parse layer: this program's own reading of the bytes
//!   the terminal sends.** It exists because crossterm read the descriptor and
//!   held the buffer, and that single fact was the root of both `IS-33`
//!   (`shift-enter` SENT, because crossterm cannot parse the form tmux emits by
//!   default) and `IS-38` (an unterminated paste left the keyboard deaf, because
//!   the bytes behind it sat in a buffer with no API to it). It emits
//!   crossterm's own `Event` type, so nothing above it learned a new word.
//! * [`input`] — the input pump and the ONE crossterm→`Key` translation. The
//!   key TABLE is in the pure crate; nothing here decides what a key means.
//! * [`term`] — terminal ownership: raw mode, the enumerable DECSET claim set
//!   (`?1002h` since `0041`, which is what makes a drag visible at all),
//!   restore on every exit path including panic, and the clipboard — `OSC 52`
//!   wrapped in tmux's DCS passthrough, because a raw one is dropped under his
//!   `set-clipboard external`.
//! * [`app`] — the coordinator. One event enum, one loop, one redraw per
//!   BATCH, the repaint law and follow mode.
//! * [`mic`] — **mic mode's three legs and the gate in front of the middle
//!   one.** Capture, transcription and playback are all external commands, and
//!   every one of them is injectable at that boundary so the whole pipeline is
//!   provable without opening the microphone. Its module header carries the
//!   finding the whole module exists for: whisper returns the word `you` at
//!   exit 0 on digital silence, so exit 0 is not a validity signal and the
//!   transcript is gated on audio PEAK before the transcriber is ever run.
//! * [`draft`] — the composer's crash journal, and a **deliberate, argued
//!   exception** to `0023`'s no-disk rule. Its module header carries the
//!   argument in full; it is named here so a reader meets it as a decision
//!   rather than finding it as a violation.
//!
//! **One redraw per BATCH, not per event, and the change is the paste
//! defence.** A paste whose content carries `ESC[201~` is truncated by
//! crossterm and its tail delivered as keystrokes ending in an `enter` that
//! would SEND. Those events all come out of ONE terminal read, so the read is
//! the unit the rule can act on: [`event::Ev::Input`] carries a whole batch and
//! [`input::defuse`] disarms it. Batching also means strictly FEWER redraws
//! than before and never more.
//!
//! **And not in this crate at all:** every line on screen, which comes from
//! `tui-render`'s `frame()`. It is a separate CRATE because its purity is
//! enforced by a `Cargo.toml` dependency list; a module here would have
//! inherited tokio, a socket and a filesystem and enforced nothing.
//!
//! # What a second face would get, and what it would still have to solve
//!
//! Everything above is reachable now, and `tests/it/lib_surface.rs` is the proof
//! — it names each core behavior from outside the binary, and it does not
//! compile against the crate as it stood before.
//!
//! What it does NOT get, honestly: [`app::run`] hard-wires the crossterm
//! `Terminal` it draws into and the pump it starts. A face on a different
//! backend reuses `proto`, `transport`, `reads`, `store`, `event`, the key
//! table and `frame()` — and writes its own loop. Making `run` generic over a
//! backend would be an abstraction with one implementation, which is a worse
//! trade than saying this sentence.
//!
//! **A library that can claim the alternate screen is a library somebody could
//! link into something that should not have one.** That risk was the stated
//! reason `term` stayed in the binary, and it is answered rather than ignored:
//! nothing is claimed at link time or import time, [`term::enter`] is an
//! explicit call, and [`term::restore`] is idempotent and safe to call having
//! never entered. The claim set is one `const` array a reader can enumerate.

pub mod acts;
/// **THE SUBAGENT AGGREGATION.** One record per subagent, derived from the
/// parent thread's own stream — see the module header for the five measured
/// facts it is built around.
pub mod agents;
pub mod app;
pub mod aui;
/// Which threads this face reads, asked of the cauldron environment it was
/// launched in (R7.S1c).
pub mod cauldron_env;
/// **THE TRANSCRIPT'S FLIGHT RECORDER** — every shape hold, row, send and
/// removal, to a file. See the module header.
pub mod diag;
pub mod draft;
pub mod event;
pub mod first_run;
pub mod cauldron;
pub mod index;
pub mod input;
/// The face's `TUI_LOG` filter, with the wire-frame targets capped at
/// `debug` (V1-ACCT-I N4).
pub mod log_filter;
/// **THE FACE'S HALF OF THE SEAM** — everything this crate supplies to
/// [`seam`], or does with what it hands back. The place every face-specific
/// decision lives, so the seam itself can stay a copy.
pub mod mirror;
pub mod mic;
/// `tui pool`: the threads account-pool board (R7.S2, DEC-37).
pub mod pool;
pub mod proto;
/// **THE TWO READS NO SHAPE ANSWERS**, above the seam, named and pinned at
/// two. The whole of what `reads` was, minus the four that found a mirror.
pub mod queue_reads;
pub mod sends;
/// **THE CONSUMER SEAM — A COPY OF `libs/shapewire/seam/*.rs`, NOT A PORT.**
///
/// Byte-identical to the vendored reference outside ONE fence (`D2`, this
/// face's own two words: `FACE = "tui"`, `ENV_PREFIX = "TUI_THREADS"`), which
/// `tests/it/seam_reference.rs` checks line by line and
/// `tests/it/seam_conformance.rs` re-checks by ANSWER. Nothing in here may be
/// edited here: a fix goes upstream to the chassis repo and arrives by a vendor
/// bump, which is the only direction of pressure that keeps two faces one
/// mirror. Everything this face SUPPLIES to it, or DOES with what it hands
/// back, lives in [`mirror`].
pub mod seam;
pub mod store;
pub mod term;
pub mod transport;
pub mod vt;
