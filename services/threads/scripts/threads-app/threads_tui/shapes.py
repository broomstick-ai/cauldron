"""THE SHAPE MIRROR — held state applied from the daemon's own fold.

WHAT A SHAPE IS, from the consumer side. The daemon runs a fold (`accounts`)
over its own event stream and holds the result in cells. A consumer subscribes,
receives one EXPORT per key (`op: read`, a labelled read of current state with
per-cell provenance), and thereafter receives DERIVED EVENTS (`op: live`) that
it applies to what it holds. The consumer rule is one line and there is no
version number anywhere:

    hold the export, apply everything after it.

That works because ONE task emits both the exports and the derived events and
ONE receiver per connection delivers them in order, so every derived event
before an export precedes it and every one after follows it.

WHY THIS MODULE REPLACED `live.py` AND `feed.py`'s RELOAD, AND WHY BOTH ARE NOW
GONE. Those two were hand-written client-side folds: `live.py` kept a 5-second
lease off a 1 Hz heartbeat and chased placement one `threads/{thread_id}/get` at a time, and
`feed.py` turned an announcement into a five-query re-read. Both existed because
the wire carried no held state — the daemon said "something changed" and the
client had to go and look. A shape carries the state, so there is nothing to go
and look for: the row arrives.

S2 made them dormant. **S3 DELETED THEM**, and the deletion is the point rather
than the tidying: while a dormant second fold existed, a board that disagreed
with the daemon about how many threads were running could not be attributed to
either side, because either side might be holding the stale answer. `live.py`
no longer exists; `feed.py` keeps its OTHER job, the live narration line — which
a shape now carries too (`NARRATION`), so `feed.py` reads it off this mirror.

THE THREE-VALUED UNKNOWN, WHICH IS THE WHOLE REASON THIS FILE HAS A `CellState`
RATHER THAN A `dict`. The operator's ruling, and it is not a nicety:

    Known         a value reached this cell. An EMPTY Known is empty, and
                  renders as empty. Zero rows is a fact.
    NotHydrated   nothing has ever populated it — the handler errored, or
                  answered no row for this key. Renders `?`.
    Dead          the fold that maintained it is gone. Every value under it is
                  the last one the daemon managed to compute and MUST NOT be
                  shown as current. Renders `?` DIFFERENTLY.

A board that collapses the second and third into one `?` tells the operator
"nothing here yet" when the truth is "everything here is a corpse", and a board
that collapses the first and second tells him a login has no expiry when the
truth is that nobody asked. `render.py` draws all three apart; `tests/
test_shapes.py` is what stops that decaying.

PURITY. This module reads no clock and does no I/O. Timestamps arrive as
INSTANTS and are parsed into `datetime`s; nothing here subtracts one from
another, because "how old is this" is a question with a `now` in it and the
`now` belongs to the caller at render time. That is the same rule that keeps
`render.py` pure and it is why there is no `age` anywhere in the shape — the
cell carries the stamp and the consumer subtracts. See `threads_tui.drag`,
obligation 1, and `tests/test_purity.py`, which has this module in its pure
closure.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime
from typing import Any

from .fmt import parse_ts

# The one shape this board mirrors. Named once so a grep finds every use.
ACCOUNTS = "accounts"

#: **THE DAEMON'S OWN WARNING COUNT.** A second shape, mirrored by a second
#: `ShapeMirror` — see `app.py`'s `self.warnings`.
#:
#: It is a separate shape rather than a cell on `accounts` deliberately, and the
#: cost lands HERE: this mirror is single-shape by construction (`on_frame`
#: returns False for any other shape's frames), so a second fact about the
#: producer needs a second instance and a second subscribe. The daemon's own
#: `shapes/warnings.rs` argues why it is worth that: a warning count is a fact
#: about the PRODUCER, and a design where "is the daemon healthy" is only
#: answerable by subscribing to the account board is a shape encoding this app's
#: layout.
WARNINGS = "warnings"

#: **THE DAEMON TALKING WHILE IT WORKS** — `lib/threads/src/shapes/narration.rs`.
#:
#: A third shape, mirrored by a third `ShapeMirror` (`app.py`'s
#: `self.narration`), and the reason this board subscribes no firehose. `info`,
#: `progress` and `lifecycle` used to reach the board only as firehose frames,
#: because they carry an empty thread id; the producer now folds each into a
#: line on one of three CHANNEL keys — named after the line's `kind` — under
#: one cell, `lines`. A keyless subscribe exports all three channels.
#:
#: The fold is what `feed.py` reads: a line becomes the `⚡` note when it is
#: APPENDED to a channel (`appended_line`), never because a frame of some
#: `type` arrived on the socket.
NARRATION = "narration"

#: The one cell a narration channel carries: `list<struct:Line>`, oldest first,
#: each line internally tagged on `kind` (`progress` | `lifecycle` | `info`).
LINES = "lines"

#: The `lifecycle` channel's key — where the `boot_id` lives now.
LIFECYCLE = "lifecycle"

#: `narration.rs::LINE_CAPACITY`. The producer keeps the newest 64 lines a
#: channel; a mirror that appended every transition forever would grow without
#: bound on a board left open for a week of `usage_poll` lines, so the held
#: window is cut to the producer's own number (`bound_lines`).
NARRATION_CAPACITY = 64


def appended_line(mirror: "ShapeMirror", frame: dict) -> dict | None:
    """The line a narration TRANSITION just appended, read back off the fold.

    Call it AFTER `mirror.on_frame(frame)` returned True. It answers only for an
    `op: live` frame whose event is a `transition` on `lines`, and it reads the
    newest held line rather than the frame's item, so what the feed narrates is
    what the mirror holds — including the `at` the mirror grafted on. An export
    (`op: read`) appends nothing: it is the window as it already was, and
    narrating it would replay up to an hour of history as though it had just
    happened.
    """
    if not isinstance(frame, dict) or frame.get("op") != "live":
        return None
    event = frame.get("event")
    if not isinstance(event, dict) or event.get("kind") != "transition":
        return None
    if event.get("cell") != LINES or frame.get("key") is None:
        return None
    lines = mirror.row(str(frame["key"])).get(LINES)
    if not isinstance(lines, list) or not lines or not isinstance(lines[-1], dict):
        return None
    return lines[-1]


def bound_lines(mirror: "ShapeMirror", key: str, capacity: int = NARRATION_CAPACITY) -> None:
    """Cut one channel's held window to the producer's capacity, newest kept."""
    entry = mirror.held.get(key)
    if entry is None:
        return
    lines = entry.row.get(LINES)
    if isinstance(lines, list) and len(lines) > capacity:
        entry.row[LINES] = lines[-capacity:]


def newest_boot_id(mirror: "ShapeMirror") -> str | None:
    """The `boot_id` on the newest `lifecycle` line held, or None."""
    lines = mirror.row(LIFECYCLE).get(LINES)
    if not isinstance(lines, list):
        return None
    for line in reversed(lines):
        if isinstance(line, dict) and line.get("boot_id"):
            return str(line["boot_id"])
    return None


#: **THE NON-IDLE ROSTER** — every thread that is not idle, with its state,
#: whether it is executing, how many messages wait on it, and the account it is
#: placed on. A third mirror, for the same reason `warnings` is one.
#:
#: Until R2.S8 this roster rode `accounts` as each row's `threads` cell — a map
#: of thread id to six fields of another shape's row, keyed under the account it
#: was placed on. That was the one violation of the nesting law on the producer,
#: and the cell is gone; the card's activity block is now a group-by over this
#: roster's `account` field (`model._activity_from_roster`).
NON_IDLE = "threads/non-idle"

#: `threads/non-idle`'s ONE key. Spelled identically to `shapes/nonidle.rs::FLEET`.
FLEET = "fleet"

#: **AN ACCOUNT'S WEEK OF GAUGE READINGS** — `lib/threads/src/shapes/gauge_history.rs`
#: (R2.S3d). The 7-day sparkline's series, which only the `gauge/history`
#: REQUEST used to answer. Keyed by account and `PerKey`, so the board
#: subscribes it once per account on its screen (`app._arm_history`), and the
#: sparkline is drawn off this fold — a reading the daemon records arrives as
#: a `transition` on `readings` and the next paint draws it. Nothing is asked.
#:
#: Named `accounts/gauge-history` and not `gauge/history`: a `/` in a shape
#: name is a nesting edge, and the edge is to `accounts`.
GAUGE_HISTORY = "accounts/gauge-history"

#: Its one cell: `list<struct:Reading>`, oldest first — `{axis, pct, resets_at?,
#: source, at}`.
READINGS = "readings"

#: `gauge_history.rs::WINDOW` — 7 d at the 120 s busy cadence on three axes.
#: The held window is cut to it (`bound_readings`), as narration's is to 64.
GAUGE_HISTORY_CAPACITY = 15_120


def bound_readings(mirror: "ShapeMirror", key: str, capacity: int = GAUGE_HISTORY_CAPACITY) -> None:
    """Cut one account's held series to the producer's capacity, newest kept."""
    entry = mirror.held.get(key)
    if entry is None:
        return
    readings = entry.row.get(READINGS)
    if isinstance(readings, list) and len(readings) > capacity:
        entry.row[READINGS] = readings[-capacity:]

# The three states, spelled exactly as `shape.rs::CellState` serialises them
# (`#[serde(tag = "state", rename_all = "snake_case")]`). Spelled as constants
# so a typo is an import error rather than a silently-never-true comparison.
KNOWN = "known"
NOT_HYDRATED = "not_hydrated"
DEAD = "dead"


@dataclass(frozen=True)
class CellState:
    """One cell's provenance, exactly as the export's `cells` block carries it.

    `complete`, `since_init` and `covers_since` are QUALIFIERS on `Known`, not
    sibling states — the daemon collapsed the design's five provenance words
    into three states plus three qualifiers, and this mirrors that rather than
    re-inventing a vocabulary on the client.
    """

    kind: str = NOT_HYDRATED
    complete: bool = True
    folded_at: datetime | None = None
    since_init: bool = False
    covers_since: datetime | None = None
    reason: str | None = None

    @property
    def known(self) -> bool:
        return self.kind == KNOWN

    @property
    def unknown(self) -> bool:
        """Either kind of `?`. Deliberately NOT a substitute for asking which."""
        return self.kind != KNOWN

    @property
    def not_hydrated(self) -> bool:
        return self.kind == NOT_HYDRATED

    @property
    def dead(self) -> bool:
        return self.kind == DEAD


# What a cell with no entry in the export is. Not a guess: a key whose export
# never mentioned a cell has not had that cell populated, which is exactly
# `NotHydrated` with the reason stated.
UNSEEN = CellState(kind=NOT_HYDRATED, reason="no export has carried this cell")


def cell_state(raw: Any) -> CellState:
    """One `cells.<name>` block off the wire. Anything unreadable is `?`."""
    if not isinstance(raw, dict):
        return UNSEEN
    kind = str(raw.get("state") or "")
    if kind == KNOWN:
        return CellState(
            kind=KNOWN,
            complete=bool(raw.get("complete", True)),
            folded_at=parse_ts(raw.get("folded_at")),
            since_init=bool(raw.get("since_init")),
            covers_since=parse_ts(raw.get("covers_since")),
        )
    if kind == DEAD:
        return CellState(kind=DEAD, reason=raw.get("reason"))
    if kind == NOT_HYDRATED:
        return CellState(kind=NOT_HYDRATED, reason=raw.get("reason"))
    # A state word this client has never heard of is NOT treated as known. The
    # daemon is entitled to grow one; guessing that it means "fine" is how a
    # board shows a confident number it has no right to.
    return CellState(kind=NOT_HYDRATED, reason=f"unrecognised cell state {kind!r}")


def _cells(raw: Any) -> dict[str, CellState]:
    if not isinstance(raw, dict):
        return {}
    return {str(name): cell_state(block) for name, block in raw.items()}


@dataclass
class Held:
    """One key's mirror: the row, its provenance, and when it was last touched."""

    key: str
    row: dict = field(default_factory=dict)
    cells: dict[str, CellState] = field(default_factory=dict)
    # The export's REAL fold point (never the `at` a subscribe asked for — the
    # daemon refuses to lie about that; see `handle_connection_method`).
    as_of: datetime | None = None
    # The stamp of the newest live frame applied on top of the export.
    at: datetime | None = None
    # Set by the death export. Held at the ROW level rather than written into
    # every cell, so a cell the export never carried is dead too — a dead fold
    # does not leave some fields trustworthy.
    dead_reason: str | None = None

    def cell(self, name: str) -> CellState:
        if self.dead_reason is not None:
            return CellState(kind=DEAD, reason=self.dead_reason)
        return self.cells.get(name, UNSEEN)

    def mark_dead(self, reason: str) -> None:
        self.dead_reason = reason


@dataclass
class ShapeMirror:
    """What this connection holds of one shape.

    Fed by `BoardApp._on_shape`; read by `model.project`. It is a pure state
    machine over frames — no socket, no clock — which is what lets
    `tests/test_shapes.py` drive an export, a burst of rows, a key removal and
    a task death through it with no daemon and no sleeping.
    """

    name: str = ACCOUNTS
    held: dict[str, Held] = field(default_factory=dict)
    shared: dict = field(default_factory=dict)
    shared_cells: dict[str, CellState] = field(default_factory=dict)

    # The shape-level twin of `CellState`, off `epoch.state`.
    state: str = NOT_HYDRATED
    dead_reason: str | None = None
    init_at: datetime | None = None
    reinit: int = 0
    lost: int = 0

    # True once at least one export has landed. It is "am I actually holding
    # any" and never "does this daemon have shapes" — a lazy shape takes ~0.8s
    # to hydrate on the first subscribe, and the two questions have different
    # answers for that whole window. Since S3 it no longer switches a path
    # (there is one path); it is the three-way distinction the screen draws
    # between un-hydrated, empty, and no-fold-at-all. See `model.Board.source`.
    exported: bool = False
    # ── HOW MANY KEY EXPORTS HAVE LANDED ────────────────────────────────────
    #
    # A monotonic count, never reset except by `reset()`. It exists because
    # `shapes/{shape}/subscribe` answers `{"queued": true}` on the request id — the
    # item is in the FIFO, the snapshot has NOT been taken — so a caller that
    # needs the board to be current cannot wait on the response. `r` and every
    # post-operation verdict wait on THIS moving instead (`app.resync`), which
    # is the difference between "the daemon accepted my request" and "the
    # daemon has told me the answer".
    exports: int = 0
    # The newest stamp seen on any frame — what the header reports as the age
    # of the board. It is a FACT ABOUT THE FOLD, not a poll clock.
    last_frame_at: datetime | None = None

    # ── the frames ───────────────────────────────────────────────────────────

    def on_frame(self, frame: dict) -> bool:
        """Apply one `type: shape` frame. True if anything a reader can see moved.

        Returns False for a frame belonging to another shape rather than
        raising: a board that grows a second shape must not have this one throw
        on the other's frames.
        """
        if not isinstance(frame, dict):
            return False
        if frame.get("type") != "shape":
            return False
        if str(frame.get("shape") or "") != self.name:
            return False

        self._epoch(frame.get("epoch"))
        op = frame.get("op")
        if op == "read":
            return self._export(frame)
        if op == "live":
            return self._live(frame)
        return False

    def _epoch(self, raw: Any) -> None:
        if not isinstance(raw, dict):
            return
        self.init_at = parse_ts(raw.get("init_at")) or self.init_at
        if isinstance(raw.get("reinit"), int):
            self.reinit = raw["reinit"]
        if isinstance(raw.get("lost"), int):
            self.lost = raw["lost"]
        state = str(raw.get("state") or "")
        if state in (KNOWN, NOT_HYDRATED, DEAD):
            self.state = state
        if state == DEAD:
            self.dead_reason = str(raw.get("dead_reason") or "the shape task died")

    def _export(self, frame: dict) -> bool:
        """`op: read`. Replaces what is held for one key — or kills the lot.

        THE KEYLESS EXPORT IS THE DEATH NOTICE. `shape.rs`'s supervisor emits
        exactly ONE frame when the task dies: `key: null`, `epoch.state: dead`,
        no row and no cells, and `server.rs::drive` delivers a keyless frame to
        any connection holding ANY key of the shape. One send, every mirror.
        """
        as_of = parse_ts(frame.get("as_of"))
        if as_of is not None:
            self.last_frame_at = as_of
        self._absorb_shared(frame)

        key = frame.get("key")
        if key is None:
            if self.state == DEAD:
                self._die()
                return True
            # A keyless, live shape-level read. Nothing but `shared` to take,
            # and `_absorb_shared` has taken it.
            return True

        key = str(key)
        self.held[key] = Held(
            key=key,
            row=frame.get("row") if isinstance(frame.get("row"), dict) else {},
            cells=_cells(frame.get("cells")),
            as_of=as_of,
            at=as_of,
        )
        self.exported = True
        self.exports += 1
        return True

    def _live(self, frame: dict) -> bool:
        event = frame.get("event")
        if not isinstance(event, dict):
            return False
        at = parse_ts(frame.get("at"))
        if at is not None:
            self.last_frame_at = at
        kind = event.get("kind")
        key = frame.get("key")
        key = str(key) if key is not None else None

        if kind == "shared":
            shared = event.get("shared")
            if isinstance(shared, dict):
                self.shared = shared
            return True

        if key is None:
            return False

        if kind == "row":
            row = event.get("row")
            if not isinstance(row, dict):
                return False
            entry = self.held.get(key)
            if entry is None:
                # A row for a key this connection was never exported. It cannot
                # normally happen — `drive` gates live frames on having had the
                # export — but if it does, the row is real and its PROVENANCE is
                # not, so it is held with no cells at all and every cell reads
                # `NotHydrated`. Inventing `Known` here is exactly the lie the
                # three-valued state exists to prevent.
                entry = Held(key=key)
                self.held[key] = entry
            entry.row = row
            entry.at = at
            return True

        if kind == "gone":
            return self.held.pop(key, None) is not None

        if kind == "transition":
            entry = self.held.get(key)
            if entry is None:
                return False
            cell = str(event.get("cell") or "")
            item = event.get("item")
            if not cell or not isinstance(item, dict):
                return False
            # THE STAMP IS ON THE FRAME, NOT ON THE ITEM. `Sequence::take_pending`
            # serialises the item alone, while the export's `to_json` adds `at`
            # to every retained item. Grafting the frame's `at` on here is what
            # makes an appended item indistinguishable from an exported one, so
            # a consumer counting `moves` does not have to know which arrived
            # how.
            item = dict(item)
            if at is not None and "at" not in item:
                item["at"] = frame.get("at")
            items = entry.row.get(cell)
            if not isinstance(items, list):
                items = []
            entry.row[cell] = [*items, item]
            entry.at = at
            return True

        return False

    def _absorb_shared(self, frame: dict) -> None:
        shared = frame.get("shared")
        if isinstance(shared, dict):
            self.shared = shared
        cells = frame.get("shared_cells")
        if isinstance(cells, dict):
            self.shared_cells = _cells(cells)

    def _die(self) -> None:
        """Every held cell becomes `Dead`. The ROWS ARE KEPT.

        Keeping them is the point: the operator must be able to see WHAT went
        stale, and a board that empties itself on a dead fold looks exactly like
        a board whose accounts all went away. The values stay, and every one of
        them is marked as last-known so nothing renders as current.
        """
        reason = self.dead_reason or "the shape task died"
        for entry in self.held.values():
            entry.mark_dead(reason)
        state = CellState(kind=DEAD, reason=reason)
        self.shared_cells = {name: state for name in self.shared_cells}

    # ── what the app reads ───────────────────────────────────────────────────

    @property
    def dead(self) -> bool:
        return self.state == DEAD

    @property
    def live(self) -> bool:
        """Holding real state from a fold that is still folding."""
        return self.exported and not self.dead

    def keys(self) -> list[str]:
        """THE KEYS THIS CONNECTION HOLDS — and the pool's own count.

        Sorted, so the board's row order does not depend on frame arrival order.
        Every key has a row by construction (the daemon exports the row with the
        key), so `len(keys())` is the account count the board draws, replacing
        the join-and-drop that could render four accounts as zero.
        """
        return sorted(self.held)

    def row(self, key: str) -> dict:
        entry = self.held.get(key)
        return entry.row if entry is not None else {}

    def cell(self, key: str, name: str) -> CellState:
        entry = self.held.get(key)
        return entry.cell(name) if entry is not None else UNSEEN

    def reset(self) -> None:
        """The socket died, or the daemon did.

        EVERYTHING GOES, including `exported`. A mirror is valid only for the
        connection that was exported to it: the subscription is per connection,
        the shape is lazy, and a reconnect re-hydrates. Keeping rows across a
        reconnect would show the last fold's answers as the new fold's.
        """
        self.held.clear()
        self.shared = {}
        self.shared_cells = {}
        self.state = NOT_HYDRATED
        self.dead_reason = None
        self.init_at = None
        self.reinit = 0
        self.lost = 0
        self.exported = False
        # The counter goes too. It is "how many exports has THIS CONNECTION
        # been sent", and a waiter comparing across a reconnect would be
        # comparing two different subscriptions' answers.
        self.exports = 0
        self.last_frame_at = None
