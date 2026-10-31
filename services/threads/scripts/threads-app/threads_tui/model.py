"""The board's data model — one shape, assembled ONLY from API payloads.

THERE IS EXACTLY ONE SOURCE (STAGE S3). Every field on every dataclass below
comes from ONE place — the `accounts` shape, mirrored in `shapes.py` and turned
into a `Board` by `project` — except the activity block, which since R2.S8 is
counted off `threads/non-idle`, the roster the `accounts.threads` cell became:

  meters, freshness       ← the `gauge` cell
  activity, running count ← `threads/non-idle`'s roster (a count over its
                            entries, grouped by each entry's `account`)
  login expiry, identity  ← the `credential` cell
  pressure                ← the `pressure` cell (PRODUCED by the daemon)
  pool share              ← the `share` cell

The old app assembled this from four sources: the daemon's WebSocket, a
`threads accounts health --json` subprocess, and a direct read-only SQLite
connection used as a degraded fallback. The version before this one had two: a
five-payload join (`build_board`) and this projection, with the join dormant.

Both counts are now one, and the second reduction is the one that mattered —
see the deletion note where `build_board` used to be. Two sources for one fact
means a disagreement can never be attributed, and a stale mirror cannot tell it
is stale.

THERE IS NO DEGRADED SQLITE PATH. That is the point. The old app, when the
daemon went quiet, quietly switched to reading the database and rendered the
numbers with a small "from the recorded log" stamp. It looked like the board
still worked. It was a second, staler implementation of the daemon's answer,
and the stamp was easy to miss. Here, if the daemon is not answering, the app
says the daemon is not answering — see the daemon-down page.

This module is PURE: dataclasses and a builder that takes already-decoded
payloads. No sockets, no clock, no environment. `now` is a parameter wherever
time matters. See `threads_tui.drag`, obligation 1.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timedelta
from typing import Any

from . import shapes
from .fmt import parse_ts

AXES = ("session", "weekly_all", "weekly_fable")
AXIS_LABEL = {"session": "session", "weekly_all": "weekly", "weekly_fable": "fable"}

# The daemon spells its axes with hyphens on the wire; the board has always used
# underscores. One place to translate, so the difference never leaks further.
WIRE_AXIS = {
    "session": "session",
    "weekly-all": "weekly_all",
    "weekly_all": "weekly_all",
    "weekly-fable": "weekly_fable",
    "weekly_fable": "weekly_fable",
    "cash": "cash",
}

# ── `DEAD` IS GONE FROM THIS TABLE, AND THAT IS THE WHOLE OF CORRECTION 3 ────
#
# *"if I understand, sounds like fields need to be named more appropriately."*
#
# `dead` meant TWO UNRELATED THINGS on this board and he read one and understood
# the other:
#
#   LOGIN HEALTH   `STATE_NOTE["wiped"]` began `DEAD — ` and described a
#                  CREDENTIAL STORE that `claude` cleared. Nothing is dead: the
#                  account is fine, the machine is fine, and signing in restores
#                  it. It is the state `STATE_BADGE` has always correctly called
#                  `WIPED`, with the prose contradicting the badge beside it.
#   A PRODUCER FACT  `Activity.not_answering_since` (named `dead_since` until
#                  this change) is the daemon saying an account stopped
#                  responding. It rendered `✕ dead 9m`.
#
# He read the first and concluded the second, and so believed two accounts were
# dead when `dead_since` was NULL on all four. `Board.warnings()` is where the
# two collided on one line, and the fix there was to honour `disabled`; this is
# the other half, because the words themselves were the trap.
#
# THE WORDS CHOSEN, AND WHY NOT A SECOND SPELLING OF "DEAD". `WIPED` and
# `NOT ANSWERING` share no stem, no length and no glyph, so there is no reading
# speed at which one can be mistaken for the other — which two spellings of one
# word ("dead"/"died") would not have given. Each also names what was OBSERVED
# rather than diagnosing it: a store was wiped, a process is not answering.
# Neither claims anything is unrecoverable, and "dead" claimed exactly that
# about the one state on this table a single sign-in fixes.
#
# `WIPED` MATCHES `STATE_BADGE["wiped"]` ON PURPOSE. The card wears the badge and
# the note explains it; before this they used two different words for one state,
# which is how the note could be read as being about something else entirely.
#
# THE PRODUCER HAS ITS OWN COPY OF THIS COLLISION AND IT IS NOT TOUCHED HERE.
# `lib/threads/src/accounts.rs::SlotHealth::Wiped.summary()` returns a string
# beginning `"DEAD — "`. It does NOT reach this board — the shape carries the
# machine word `slot_state` and the prose above is this client's own — but it is
# the same mistake in the same system, read by the `threads accounts health` CLI
# (`main.rs`). Reported, deliberately unedited.
STATE_NOTE = {
    "no-login": "not signed in — nothing has ever logged in here",
    # H179: plain words — what happened, what it costs, and (on the card and the
    # board line, which know the label and the key) what to press.
    "wiped": (
        "WIPED — this slot's own login was cleared after a refresh failed, "
        "so no work can run on it until it is signed in again"
    ),
    "stranded": (
        "STRANDED CLONE — copied from another login, which has since rotated "
        "away from it. It still reads 'logged in' locally, and is not."
    ),
    "expired": "EXPIRED — the login itself has run out",
}

STATE_BADGE = {
    "no-login": "NO LOGIN",
    "wiped": "WIPED",
    "stranded": "STRANDED",
    "expired": "EXPIRED",
}

STALE_SECONDS = 3600.0

# Slot states in which the daemon's /usage probe cannot run, and the short cause
# the card stamp names instead of a bare `STALE` (H179).
UNPROBEABLE_CAUSE = {
    "no-login": "no login",
    "wiped": "login wiped",
    "expired": "login expired",
    "stranded": "login stranded",
}

# ── THE WARNING SEVERITIES, AND THE LOUDEST ONE IS NO LONGER CALLED `dead` ───
#
# `Board.warnings()` returns `(severity, text)` and `render.warning_line` turns
# the severity into a glyph and a colour. The loudest one was spelled `"dead"`,
# which made THREE things on this board called dead at once: a wiped credential
# store (prose), an account not answering (`dead_since`), and HOW LOUD TO DRAW A
# LINE. The third is not a state of the world at all — it is a rendering
# instruction — and it was the one sitting in the tuple right next to the prose
# the operator misread.
#
# `CRITICAL` says what it governs and could not be mistaken for a fact about an
# account. It is a named constant rather than a bare literal so the severity
# vocabulary has one definition instead of five spellings at five call sites.
CRITICAL = "critical"
WARN = "warn"
OK = "ok"

# Where this board's one source stands. See `Board.source`.
SOURCE_HELD = "held"
SOURCE_WAITING = "waiting"
SOURCE_ABSENT = "absent"


@dataclass
class AxisState:
    name: str
    pct: float | None = None
    resets_at: datetime | None = None
    observed_at: datetime | None = None
    calibrated: bool = False
    samples: int = 0
    confidence: float | None = None
    source: str = ""
    # True when the API actually reported this axis. A placeholder axis is not
    # the same thing as an axis reported uncalibrated, and the board must not
    # render the first as if it were the second.
    present: bool = False
    # Retained so render.py stays a verbatim lift. Always False now: there is
    # no database path, so nothing can come from one.
    from_db: bool = False

    @property
    def known(self) -> bool:
        return self.pct is not None

    @property
    def at_limit(self) -> bool:
        return self.pct is not None and self.pct >= 100.0

    @property
    def low_confidence(self) -> bool:
        return self.confidence is not None and self.confidence < 0.9

    def age_seconds(self, now: datetime) -> float | None:
        if self.observed_at is None:
            return None
        return (now - self.observed_at).total_seconds()


@dataclass
class Activity:
    executing: int = 0
    executing_thread_ids: list[str] = field(default_factory=list)
    queued_threads: int = 0
    queued_messages: int = 0
    parked: int = 0
    retrying: int = 0
    idle: int = 0
    threads: int = 0
    busy: bool = False
    available: bool = True
    healthy: bool = True
    # ── `dead_since` / `dead_reason` RENAMED (Correction 3) ──────────────────
    #
    # The daemon's own columns are still spelled `dead_since` / `dead_reason` and
    # the wire keys are read under those names in `_activity_from_roster`, which
    # is the ONE place the producer's spelling appears on this side. Everything
    # downstream — the badge, the board warning, the tests — now says what the
    # fact is: this account is not answering. See the note above `STATE_NOTE`.
    not_answering_since: datetime | None = None
    not_answering_reason: str | None = None
    limit_until: datetime | None = None
    last_limit_prose: str | None = None
    limit_axis: str | None = None
    # False when the API has not told us. Distinct from "told us zero".
    known: bool = False


@dataclass
class Account:
    number: int
    label: str
    state: str = "?"
    clone_of: str | None = None
    days_left: float | None = None
    identity_acct: str | None = None
    identity_email: str | None = None
    subscription_type: str | None = None
    credential_mode: str | None = None
    config_dir: str | None = None
    keychain_service: str | None = None
    secret_sha8: str | None = None
    is_live_login: bool = False
    is_machine_default: bool = False
    live_store_days_left: float | None = None
    live_store_state: str | None = None
    warning: str | None = None
    rotation_action: str = "skip"
    rotation_reason: str = ""
    hint: str | None = None
    axes: dict[str, AxisState] = field(default_factory=dict)
    activity: Activity = field(default_factory=Activity)

    # From accounts/pressure / accounts/pool_share_get. None means the API did not
    # say — never defaulted to a plausible-looking number.
    pressure: float | None = None
    pressure_ranked: bool = False
    pressure_reason: str = ""
    pool_share: float | None = None
    # **DISABLED** — the operator set a true 0% share, so this account is out of
    # placement selection entirely. Read from the daemon's OWN `disabled` boolean
    # rather than inferred from `pool_share == 0`: the daemon is the authority
    # on what a zero means, and a client that re-derives the state can disagree
    # with the thing making the decision.
    #
    # It is NOT a health state and must never be rendered as one. A bar
    # (`dead`, a limit window) is the system reporting a failure it observed and
    # will retry out of; this is his own deliberate choice, it has no timer, and
    # nothing lifts it.
    disabled: bool = False
    # WHEN, AND IN HIS OWN WORDS. `disabled_at` and `disabled_note` are the two
    # other columns of the daemon's `account_disabled` table, and they are the
    # only facts on a disabled card that are not derivable from somewhere else —
    # which is why the simple card keeps them when it drops everything else.
    # Both are None on an account that is not disabled, and `disabled_note` is
    # None when he disabled it without saying why.
    disabled_at: datetime | None = None
    disabled_note: str | None = None
    #
    # ── THERE IS NO `restore_to` AND THERE MUST NOT BE ONE ───────────────────
    #
    # `restore_to` and `prior` answered "what would an enable put this account
    # back at", and they existed because a disable WAS a share of zero: the act
    # destroyed the share, so something had to remember it. It does not any
    # more. `disabled` is its own fact in its own table, `pool_shares` is never
    # touched by either direction, and the answer to that question is
    # `pool_share` — the field directly above.
    #
    # The obvious migration was to keep the field and assign `share` to it. That
    # is refused deliberately: a field named `restore_to` holding a share that
    # was never taken away is two fields answering one question with a name that
    # lies about which, which is the defect this codebase keeps naming. The
    # concept is gone from the model, from the modal, and from the copy.
    # Did accounts/pressure list this account AT ALL? An account outside the
    # pool is simply absent from the payload, which is a third state distinct
    # from "listed and unranked" and must not be shown as the same thing.
    in_pressure_pool: bool = False

    # ── SHAPE PROVENANCE — see `threads_tui.shapes` ──────────────────────────
    #
    # Per-cell state off the export's `cells` block, so the render layer can
    # draw the THREE unknowns apart: a value the daemon read and found empty, a
    # cell nothing has ever populated, and a cell whose fold is dead. EMPTY on
    # the legacy five-query path, which is how `cell()` answers None there and
    # every existing rendering stays exactly as it was.
    cells: dict[str, Any] = field(default_factory=dict)
    # Is this account in the pool roster? False means the key exists only
    # because something non-idle is running on it — a real state, and one the
    # card must not draw as ordinary membership.
    registered: bool = True
    # The machine-default store, as a NOTE on the account it folds into
    # (ruling 5). Never its own card: `accounts.rs::mark_live_login` exists to
    # stop the machine default and the account it is signed in as being drawn
    # as two cards with one identity, and a `default` row would reintroduce
    # exactly that from the other side.
    machine_default: dict | None = None
    # The recent placement moves under this key — a windowed `Sequence`, not a
    # ledger. Named `moves` and not `failovers` because `AccountSwitched.reason`
    # distinguishes a deliberate placement from a failover and the count
    # includes both. Every count off it is qualified by the cell's
    # `covers_since`, which is what makes "3 moves" honest rather than a claim
    # about all time.
    moves: list = field(default_factory=list)

    def cell(self, name: str):
        """This cell's `shapes.CellState`, or None on the legacy path.

        None is not a fourth state. It means "this board was not built from a
        shape", and every caller reads it as "render exactly as before".
        """
        return self.cells.get(name) if self.cells else None

    @property
    def healthy_login(self) -> bool:
        return self.state == "live"

    @property
    def health_badge(self) -> str | None:
        if self.state == "clone-of":
            return f"CLONE of {self.clone_of or '?'}"
        return STATE_BADGE.get(self.state)

    @property
    def health_note(self) -> str | None:
        if self.state == "clone-of":
            return (
                f"CLONE of {self.clone_of or '?'} — one session in two places; "
                "whichever refreshes second dies"
            )
        if self.wiped_but_machine_live:
            return (
                "WIPED — your machine login (same account) is fine, but this slot's "
                "own login is wiped, so work placed here can't run"
            )
        return STATE_NOTE.get(self.state)

    @property
    def wiped_but_machine_live(self) -> bool:
        """H179: the slot's own store is wiped while the MACHINE login — a
        separate store, signed in as this same account — is live. Work placed
        on the slot runs under the slot's own config dir, so it still fails;
        the card shows both facts and must say how they fit together."""
        return (
            self.state == "wiped"
            and self.is_live_login
            and self.live_store_state == "live"
        )

    @property
    def sign_in_command(self) -> str:
        return f"`claude-accounts login {self.label}`"

    @property
    def has_gauge(self) -> bool:
        return any(a.pct is not None for a in self.axes.values())

    @property
    def any_at_limit(self) -> bool:
        return any(a.at_limit for a in self.axes.values())

    def newest_observed(self) -> datetime | None:
        """The NEWEST axis reading, as the daemon's `last_observed_at` does
        (budgets.rs). This was `min()`, and one dead or unmapped axis then
        pinned the whole card STALE forever (H179). Per-axis stamps still
        show each axis's own age."""
        stamps = [a.observed_at for a in self.axes.values() if a.observed_at is not None]
        return max(stamps) if stamps else None

    def reading_age(self, now: datetime) -> float | None:
        newest = self.newest_observed()
        if newest is None:
            return None
        return (now - newest).total_seconds()

    @property
    def unprobeable_cause(self) -> str | None:
        """Why the daemon CANNOT take a new reading for this slot, or None.

        The usage probe runs as the slot's own login; a wiped, expired,
        absent or stranded login fails it every hour and the daemon keeps
        serving the last reading. That is not ordinary staleness and the
        card must not present it as such."""
        return UNPROBEABLE_CAUSE.get(self.state)

    def is_stale_at(self, now: datetime) -> bool:
        """Staleness needs a clock, so the clock is an argument.

        The old app exposed this as a bare `is_stale` PROPERTY that called
        `now_utc()` internally, which is precisely the kind of hidden input
        that makes a render layer untestable.
        """
        age = self.reading_age(now)
        return age is not None and age > STALE_SECONDS


@dataclass
class Board:
    accounts: list[Account] = field(default_factory=list)
    live_login: str | None = None
    warn_below_days: float = 7.0
    fetched_at: datetime | None = None

    # What the API could not tell us, and why. Rendered as an explanation.
    notes: list[str] = field(default_factory=list)
    health_error: str | None = None

    # ── WHERE THIS BOARD CAME FROM ───────────────────────────────────────────
    #
    # "" — the five-query legacy path (S0/S2a). Otherwise the shape's own
    # state word: `known`, `not_hydrated` or `dead`. It is on the BOARD as well
    # as on every cell because a dead fold is one fact about the whole screen,
    # and the header has to be able to say so once rather than the operator
    # having to infer it from every card wearing the same badge.
    shape_state: str = ""
    shape_note: str = ""

    # ── THE DAEMON'S OWN WARNING COUNT ───────────────────────────────────────
    #
    # `None` means THE DAEMON DID NOT SAY, which is a different fact from zero
    # and must never be painted as one: a daemon too old to carry the `warnings`
    # shape, or one this board failed to subscribe to, has not told us it is
    # healthy. Zero means it said zero.
    #
    # Two fields and not a sum, because the producer keeps `warn` and `error`
    # apart on purpose (see `warnings::Tally::error`) and a board that added them
    # up here could not tell 47 warnings from 47 errors either.
    daemon_warnings: int | None = None
    daemon_errors: int | None = None

    # ── WHY THERE IS NO BOARD, WHEN THERE IS NONE ────────────────────────────
    #
    # An empty `accounts` list is THREE different facts and a board that drew
    # them the same way would be telling the operator a confident lie in two of
    # the three cases. This is the same rule as un-hydrated being distinct from
    # empty, one level up: it is about the whole screen rather than one cell.
    #
    #   SOURCE_HELD     an export landed. An empty list is EMPTY — the daemon
    #                   holds no accounts, and that is a fact about the pool.
    #   SOURCE_WAITING  no export has landed yet. The fold may be hydrating (a
    #                   lazy shape is ~0.8s cold), the probe may still be in
    #                   flight, or the socket may not be up. Nothing is known.
    #   SOURCE_ABSENT   this daemon answered `unknown method: shapes/list`. It
    #                   holds no folds, and since S3 THERE IS NO FALLBACK: the
    #                   five queries that used to draw this board are gone. The
    #                   board says so and says it will turn on by itself.
    #
    # `source_note` is the sentence to show, and for the absent case it is read
    # out of the capability registry rather than written here, so it is the
    # same sentence every other absent capability produces.
    source: str = SOURCE_WAITING
    source_note: str = ""

    def by_label(self, label: str) -> Account | None:
        return next((a for a in self.accounts if a.label == label), None)

    def by_number(self, number: int) -> Account | None:
        return next((a for a in self.accounts if a.number == number), None)

    def resolve(self, want: str) -> Account | None:
        want = (want or "").strip()
        if not want:
            return None
        if want.isdigit():
            return self.by_number(int(want))
        return self.by_label(want)

    def room(self) -> tuple[Account, float] | None:
        """**WHICH ACCOUNT HAS THE MOST WEEKLY HEADROOM. A COMPARISON, not a
        gauge — and the FOURTH reader of `disabled` that was not reading it.**

        His words: *"Why am i seeing ROOM alice-example-com - 93% of weekly left?
        That's card specific information and it's not in a card. Also that
        account is disabled so it's likely inaccurate."*

        Both halves were real and this is the second one. `alice-example-com` has
        been disabled since 2026-10-02 and this loop never asked: it ranked every
        account the daemon reported, picked the one with the most weekly left, and
        the header named it as where the work should go. The gauge reading itself
        may well have been true at the moment it was taken — what was false was
        the recommendation built on it, because nothing will ever be placed there.
        An account out of placement selection has no headroom in any sense the
        word is used on this board: its room is not room.

        THIS IS THE SAME DISEASE `warnings()` HAD, in its THIRD site. See the two
        `disabled` notes in that function; `placeable()` had it right all along.
        `any_gauged()` and `next_back()` are the other two header-feeding loops
        and BOTH had the identical omission; they are fixed in the same change
        and each carries its own note, because each was wrong in its own way.

        WHERE THE NUMBER WENT. Out of the header and onto the card, which is the
        first half of his complaint. See `render._room_line`.
        """
        best: tuple[Account, float] | None = None
        for account in self.accounts:
            # OUT OF PLACEMENT SELECTION, SO OUT OF THIS RANKING. Not a relaxed
            # entry, a skip: there is no honest way to say "this account has the
            # most room" about an account that will never be given any.
            if account.disabled:
                continue
            if not account.has_gauge or account.any_at_limit:
                continue
            weekly = account.axes.get("weekly_all")
            if weekly is None or not weekly.known:
                continue
            left = 100.0 - weekly.pct
            if best is None or left > best[1]:
                best = (account, left)
        return best

    def any_gauged(self) -> bool:
        """**IS THERE A GAUGE READING BEHIND THE HEADER'S ROOM VERDICT.**

        FOURTH SITE. It exists for exactly one caller — `render.header_line2`,
        which asks it first and says `ROOM no usage readings available` when the
        answer is no — and it counted DISABLED accounts, which made it disagree
        with `room()` in a way that produced a confident lie rather than a wrong
        number: with a gauged disabled account and no gauged enabled one, this
        returned True, `room()` returned None, and the header drew
        `ROOM none — all accounts at limit`. Nothing was at limit. There were no
        readings for any account work could go to.

        The two must range over the SAME population or the three-way branch in
        the header is not a partition. So it ranges over the placement-eligible
        accounts, exactly as `room()` does.
        """
        return any(a.has_gauge for a in self.accounts if not a.disabled)

    def next_back(self) -> tuple[Account, AxisState] | None:
        """**WHEN THE POOL GETS CAPACITY BACK, and from which account.**

        FIFTH SITE, and it was live on his screen beside the one he reported.
        `alice-example-com` is disabled AND carries a `login`-axis limit whose
        window ENDED on 2 October, so this returned that axis — the earliest
        `resets_at` of any at-limit axis anywhere — and the header drew
        `NEXT BACK  alice-example-com in 0m` (`fmt_delta` clamps a negative delta
        to `0m`). Two falsehoods in six words: the account is out of the pool, so
        its window reopening gives the pool nothing, and the window reopened five
        days ago.

        A disabled account coming back from a limit is not the pool coming back
        from a limit. Same skip, same reason as `room()`.
        """
        best: tuple[Account, AxisState] | None = None
        for account in self.accounts:
            if account.disabled:
                continue
            for axis in account.axes.values():
                if not axis.at_limit or axis.resets_at is None:
                    continue
                if best is None or axis.resets_at < best[1].resets_at:
                    best = (account, axis)
        return best

    def queue_depth(self) -> int:
        """**THE POOL'S QUEUE DEPTH, IN THREADS. ONE NUMBER, ONE OWNER.**

        The operator: *"why would a specific account say queued at all? threads
        should have a queue for the pool not an individual account."*

        He is right, and there is no per-account queue to draw: the only queue is
        a table keyed by `thread_id`, and accounts are placement TARGETS. The
        board derives its per-account counts by bucketing per-thread rows under
        each thread's LAST account, which answers "where did this last run" and
        not "where will it run next" — placement is re-decided at admission. So a
        count under one account's name asserted a commitment that does not exist,
        while the SUM of those same counts asserts only what is true.

        **IT IS A METHOD RATHER THAN A `sum()` AT THE CALL SITE, DELIBERATELY.**
        The number used to be computed inline in `app.py`, which meant a test
        could only check it by writing the same `sum()` a second time — and a
        test that re-implements the thing it is testing agrees with any mistake
        the original makes. Owning it here gives the guard something real to call.

        **IT COUNTS MESSAGES, AND THE WORD `queued` MEANS MESSAGES.** He asked
        outright — *"what does queued mean if it's not parked. is it threads with
        new messages queued to land in them?"* — and that is exactly what it is.
        The `queue` table is
        `(id, thread_id, message_id, text, intent, state, created_at, delivered_at)`
        with `state='waiting'`: MESSAGES AWAITING DELIVERY INTO A THREAD, keyed by
        thread, with no account column anywhere. So the queue's own unit is the
        message, and `queued_messages` is the field that matches the table.

        **THIS IS WHY `max(queued_threads, queued_messages)` WAS SO WRONG.** The
        card badge took the larger of a count of THREADS in state `queued` and a
        sum of waiting MESSAGES, then printed it under one label. The two are not
        rival measurements of one quantity — they answer different questions, both
        can be true at once, and neither is a substitute for the other. `max()`
        picked whichever was bigger and renamed it.

        The thread-state count is NOT drawn anywhere on this board now. It is a
        real fact and it is still on `Activity.queued_threads` for anyone who
        needs it; it simply is not the number the word `queued` names, and showing
        both under one word is the defect this method exists to end.
        """
        return sum(a.activity.queued_messages for a in self.accounts)

    def parked_count(self) -> int:
        """**THE POOL'S PARKED COUNT, IN THREADS (H179).**

        *"you never park TO an account. Parked threads wait in the pool."* Each
        card's `Activity.parked` buckets parked threads under the account they
        LAST sat on, which answers "where did it last run", never "where will it
        run" — placement is re-decided on every placement event, over the whole
        pool. The sum is the true fact, and it is the only one drawn.
        """
        return sum(a.activity.parked for a in self.accounts)

    def placeable(self) -> int:
        """**HOW MANY ACCOUNTS COULD TAKE A TURN RIGHT NOW.**

        Three conditions, and each one is a different reason the answer can be
        zero while the board looks fine:

          * NOT DISABLED — his own decision, nothing lifts it but him;
          * THE LOGIN IS USABLE — `live` or the honest `?`, so a wiped or expired
            store does not count as a candidate;
          * NOT INSIDE AN OBSERVED LIMIT WINDOW — `Activity.available`, which is
            derived from `limit_until` against a `now` passed in at projection.

        IT EXISTS FOR ONE SENTENCE IN THE HEADER. *"threads is still not
        displaying running threads so its impossible for me to know."* `0 running`
        was honest — the whole pool was barred — and still told him nothing,
        because a quiet pool and a shut pool render identically. This is the fact
        that separates them.

        AN UNKNOWN ACTIVITY CELL COUNTS AS PLACEABLE, deliberately. `available` is
        True when no limit window is known, so an account the daemon has said
        nothing about is treated as able to run rather than as barred. Guessing
        the other way would put "no account can accept work" on screen whenever
        the threads cell had not hydrated yet, which is the confident-zero mistake
        the rest of this board is built to avoid.
        """
        return sum(
            1
            for a in self.accounts
            if not a.disabled
            and a.state in ("live", "?")
            and a.activity.available
        )

    def warnings(self) -> list[tuple[str, str]]:
        out: list[tuple[str, str]] = []
        # ── THE DEAD FOLD, ONCE, ABOVE EVERYTHING ───────────────────────────
        #
        # Every card also wears `✕ FOLD DIED`, because a card read in isolation
        # must not look healthy. This line is the other half: the fact is about
        # the WHOLE SCREEN, it does not clear on its own, and nothing this board
        # can do will revive it — the daemon refuses to re-subscribe a dead
        # shape until it restarts. Saying that outright is the difference
        # between a board that has stopped and a board that says it has
        # stopped.
        if self.shape_state == shapes.DEAD:
            out.append(
                (
                    CRITICAL,
                    f"THE DAEMON'S ACCOUNTS FOLD DIED — {self.shape_note or 'no reason given'}. "
                    "Everything below is the last state it managed to compute and "
                    "is NOT current. It cannot be re-subscribed until the daemon "
                    "restarts, and this board has no second way to read these facts — "
                    "the five account queries it used to fall back to are gone",
                )
            )
        for account in self.accounts:
            # ── A DISABLED ACCOUNT RAISES NOTHING HERE ───────────────────────
            #
            # He was being shown two red `✕` lines demanding he sign in two
            # accounts he had switched OFF HIMSELF — and the word they carried
            # was `DEAD`, which was `STATE_NOTE["wiped"]`, login-health prose for
            # a cleared store. Nothing was dead: `dead_since` was NULL on all
            # four accounts. This board has told him that once before (see the
            # note on `render._badges`) and this loop is how it did it again.
            # (That prose now says `WIPED` — the word the badge beside it always
            # used. See the note above `STATE_NOTE`.)
            #
            # `_badges` AND `card_lines` ALREADY HONOURED THIS FLAG. `_badges`
            # draws `DISABLED` in amber rather than the health badge's red, and
            # `card_lines` collapses the card so the health note is not drawn at
            # all. This function was the one place that looped the accounts and
            # never asked — and it was NOT the last: `room()`, `any_gauged()` and
            # `next_back()` had the same omission and are fixed above.
            #
            # WHAT IS SKIPPED IS THE WARNING, NOT THE FACT. The card still wears
            # its `WIPED` badge in the title border, so a disabled account with a
            # cleared store is still visibly broken on the row he selects — it
            # just does not raise a board-level alarm that asks him to undo a
            # decision he made. These lines are imperatives ("press 2, then L"),
            # and an imperative aimed at a state he chose is noise that teaches
            # him to stop reading the lines that are real.
            #
            # IT IS A SKIP, NOT A RELAXED MESSAGE, FOR AN ENABLED ACCOUNT. Drop
            # the `disabled` test and an enabled wiped store must STILL shout:
            # that account is in the pool, work will be placed on it, and it
            # cannot run. The message is right; only its unconditional
            # application was wrong.
            if account.disabled:
                continue
            if account.state not in ("live", "?"):
                note = account.health_note or account.state
                how = f"press {account.number}, then L, to sign it in"
                if account.state == "wiped":
                    # H179: the slot itself needs signing in; say the CLI too.
                    how += f" (or run {account.sign_in_command})"
                out.append(
                    (
                        CRITICAL
                        if account.state in ("wiped", "stranded", "expired")
                        else "warn",
                        f"{account.label} {note} — {how}",
                    )
                )
            act = account.activity
            # THE SECOND LOOP HAD THE IDENTICAL OMISSION and was silent only
            # because `dead_since` is NULL on every account on this machine. A
            # latent copy of the bug above is the next bug; the `continue` over
            # both of them is what makes that impossible rather than unlikely.
            #
            # `NOT ANSWERING` IS SHOUTED, in caps, for the same reason the line
            # above shouts `WIPED`: these two lines were the place the operator
            # read one fact and understood the other, and the two facts now share
            # no word. The badge on the card says the same three syllables
            # (`✕ not answering 9m`), so the board-level line and the card agree.
            if act.known and act.not_answering_since is not None:
                out.append(
                    (
                        CRITICAL,
                        f"{account.label} is NOT ANSWERING — "
                        f"{act.not_answering_reason or 'no reason given'}",
                    )
                )
        return out


# ── assembly ─────────────────────────────────────────────────────────────────


def _axes_from_gauge(axes_obj: Any) -> dict[str, AxisState]:
    """Translate one account's `axes` object from gauge/status or accounts/status."""
    out: dict[str, AxisState] = {}
    if not isinstance(axes_obj, dict):
        return out
    for wire_name, raw in axes_obj.items():
        name = WIRE_AXIS.get(wire_name, wire_name.replace("-", "_"))
        if not isinstance(raw, dict):
            continue
        pct = raw.get("observed_pct")
        out[name] = AxisState(
            name=name,
            pct=float(pct) if isinstance(pct, (int, float)) else None,
            resets_at=parse_ts(raw.get("resets_at")),
            observed_at=parse_ts(raw.get("observed_at")),
            calibrated=bool(raw.get("calibrated")),
            samples=int(raw.get("calibration_samples") or 0),
            confidence=(
                float(raw["confidence"]) if isinstance(raw.get("confidence"), (int, float)) else None
            ),
            source=str(raw.get("source") or ""),
            present=True,
        )
    return out


# ── `build_board` AND THE FIVE-PAYLOAD JOIN ARE DELETED (S3) ────────────────
#
# What stood here: `build_board(status, gauge, health, pressure, shares)`,
# `_activity_from_status`, and `_wire_disabled` — the join that took five
# separate query payloads, picked one of them as the SPINE (`accounts/health`
# when it was on the wire, `accounts/status` otherwise), matched the other four
# to it BY LABEL, and dropped whatever did not match.
#
# It is deleted rather than kept for an old daemon, and the reason is the join
# itself as much as the queries. Every one of those five answers was a separate
# read of a separate table at a separate instant, so the board's picture was
# never a state the daemon was ever actually in; and a label that failed to
# match was silently absent, which is how four accounts could render as zero.
# `project` below replaces the whole of it with a read of held state: the
# daemon has already done the fold, every key it exports comes WITH its row, so
# the account count is `len(keys)` and there is nothing left to drop.
#
# `_wire_disabled` went with it, and that one is worth a sentence because it
# was a deliberate leniency: the old daemon spelled this state `paused` and the
# new one spells it `disabled`, and the client read either so the badge would
# not vanish mid-deploy. The shape's `share` cell carries `disabled` and only
# `disabled`, so the second spelling has no reader and the shim has no window
# left to cover.

# ── THE SHAPE PROJECTION ─────────────────────────────────────────────────────
#
# `build_board` joined five payloads by label and dropped what did not match —
# see the deletion note above, which is where it stood. `project` replaces that
# join with a read of held state: the daemon has already done the fold, every
# key it exports comes WITH its row, and the count of accounts is `len(keys)`
# rather than whatever survived the join. That is the difference between "four
# accounts rendered as zero" being unlikely and it being impossible.
#
# SINCE S3 IT IS ALSO THE ONLY BUILDER. `BoardApp.redraw` calls it once per
# paint and nothing else assembles a `Board`.
#
# IT TAKES `now` AND IT IS STILL PURE. Every stamp on the wire is an INSTANT —
# `access_expires_at_ms`, `observed_at`, `until` — and never an age, because an
# age is a subtraction with a clock in it and the daemon's clock is not the
# board's. So the subtraction happens exactly once, here, against a `now` the
# caller passes in. That is the same rule that keeps `render.py` pure, applied
# one layer earlier.

SHAPE_CELLS = (
    "registered",
    "login_dir",
    "share",
    "gauge",
    "limit",
    "health",
    "credential",
    "pressure",
    # `threads` WAS HERE. It is gone from `accounts` (R2.S8, version 2); the
    # running counts come off `threads/non-idle` — see `roster_cell`.
    "moves",
    "machine_default",
)

MS_PER_DAY = 86_400_000.0


def _days_until(expires_at_ms: Any, now: datetime) -> float | None:
    """An expiry INSTANT minus `now`, in days. None when there is no expiry.

    THE ONE PLACE `days_left` IS COMPUTED, and it is computed rather than read
    because the daemon deliberately stopped sending it. `SlotReport.days_left`
    was a clock read baked into a payload: it aged the moment it was serialised,
    and two consumers reading the same payload a minute apart got the same
    number. The wire now carries the instant and every consumer subtracts its
    own `now`.

    **WHICH INSTANT IS THE CALLER'S DECISION AND IT IS NOT A FREE ONE.** The
    credential cell carries TWO expiries and they answer different questions:

      `access_expires_at_ms`   the OAuth ACCESS token. Lives a couple of hours,
                               is refreshed lazily and automatically on use, and
                               is routinely already in the past on a completely
                               healthy account. Counting down to it is arithmetic
                               without meaning.
      `refresh_expires_at_ms`  the LOGIN. When this one goes, the operator has to
                               open a browser. This is the deadline a countdown
                               is for, and it is what the daemon's own
                               `accounts::days_left` reads — *"Days until the
                               LOGIN (not the access token) expires."*

    The board read the first one and every card on the live board said
    `login 0d`. `tests/test_expiry.py` pins the second, at the source level as
    well as by value, because a value test can be fixtured green.
    """
    if not isinstance(expires_at_ms, (int, float)):
        return None
    return (float(expires_at_ms) - now.timestamp() * 1000.0) / MS_PER_DAY


def roster_by_account(non_idle: "shapes.ShapeMirror | None", folds_into: Any) -> dict[str, list[dict]]:
    """`threads/non-idle`'s one roster, GROUPED by the card each thread is drawn on.

    **THE PLACEMENT IS A FIELD NOW, NOT THE KEY.** Until R2.S8 a thread's row
    lived under the account it was placed on (`accounts.<key>.threads`), so the
    question "which account did this thread land on" was answered by which map
    the row was in. That map was the nesting-law violation and it is gone; the
    roster carries the placement as each entry's `account`, and this is the one
    group-by that turns it back into cards. Still no per-thread lookup, no
    lease, no TTL and no residual — it is one keyless subscribe, as it was.

    **`folds_into` IS APPLIED HERE, BECAUSE THE ACCOUNT FOLD USED TO APPLY IT.**
    `accounts` filed a thread placed on the machine default under the account
    its shared `folds_into` names; the roster carries the placement the daemon
    reported. So the same map, off the same `shared` block the board already
    reads, puts that thread on the same card it was drawn on before.
    """
    if non_idle is None:
        return {}
    roster = non_idle.row(shapes.FLEET).get("threads")
    if not isinstance(roster, list):
        return {}
    fold = folds_into if isinstance(folds_into, dict) else {}
    out: dict[str, list[dict]] = {}
    for entry in roster:
        if not isinstance(entry, dict) or not isinstance(entry.get("account"), str):
            continue
        card = fold.get(entry["account"], entry["account"])
        out.setdefault(str(card), []).append(entry)
    return out


def roster_cell(non_idle: "shapes.ShapeMirror | None") -> "shapes.CellState":
    """The ROSTER's own state — which is what `known` on every card now means.

    `None` is a board built without the third mirror; it has not been told, and
    says so in words rather than as a bare `?`.
    """
    if non_idle is None:
        return shapes.CellState(
            kind=shapes.NOT_HYDRATED, reason="this board holds no `threads/non-idle` mirror"
        )
    return non_idle.cell(shapes.FLEET, "threads")


def _activity_from_roster(
    entries: list[dict], roster: "shapes.CellState", row: dict, now: datetime
) -> Activity:
    """The card's activity block, COUNTED over the roster entries placed on it.

    Every number here is a derived op — `count`, `sum` — over entries the
    daemon put in `threads/non-idle` with this card's account. None of them is
    stored by the shape and none of them reads a clock: `available` is the one
    that needs `now`, and it gets it as an argument.
    """
    rows = [t for t in entries if isinstance(t, dict)]
    executing_ids = [str(t.get("id")) for t in rows if t.get("executing") is True]
    health = row.get("health") if isinstance(row.get("health"), dict) else {}
    limit = row.get("limit") if isinstance(row.get("limit"), dict) else None
    limit_until = parse_ts((limit or {}).get("until"))

    return Activity(
        executing=len(executing_ids),
        executing_thread_ids=executing_ids,
        queued_threads=sum(1 for t in rows if t.get("state") == "queued"),
        # `queued` rides the roster for exactly this sum (R2.S8 step 1): it was
        # dropped from `threads/non-idle` on the false claim that nothing read it
        # off `accounts.threads`, and this line was the thing that read it.
        queued_messages=sum(
            int(t.get("queued") or 0) for t in rows if isinstance(t.get("queued"), (int, float))
        ),
        parked=sum(1 for t in rows if t.get("state") == "parked"),
        retrying=sum(1 for t in rows if t.get("state") == "retrying"),
        # `idle` and the all-time `threads` total are NOT in the shape: the
        # roster holds non-idle threads only, and neither number is drawn.
        # Reporting a zero the daemon never said would be a fabricated fact on a
        # card.
        idle=0,
        threads=len(rows),
        busy=bool(executing_ids),
        # DERIVED FROM THE LIMIT, which is where the fact actually lives.
        available=limit_until is None or limit_until <= now,
        healthy=bool(health.get("healthy", True)),
        # THE ONE PLACE THE PRODUCER'S SPELLING APPEARS ON THIS SIDE. `dead_since`
        # and `dead_reason` are the daemon's column names (`accounts.rs`,
        # `store.rs`) and the wire is not this client's to rename; the FIELD is,
        # and it is named for what the fact is. Keeping the translation to one
        # expression is the same rule `WIRE_AXIS` follows for the hyphenated axis
        # names: one place to translate, so the difference never leaks further.
        not_answering_since=parse_ts(health.get("dead_since")),
        not_answering_reason=health.get("dead_reason"),
        limit_until=limit_until,
        last_limit_prose=(limit or {}).get("prose"),
        limit_axis=(limit or {}).get("axis"),
        # `known` is the ROSTER cell's own state, not "did we get a payload". A
        # `?` on a card means that one cell has never been populated.
        known=roster.known or roster.dead,
    )


#: The `warnings` shape's ONE key — the producer itself. Spelled
#: identically to `shapes/warnings.rs::KEY_DAEMON`, as
#: `docs/shape-design-guide.md` step 1 requires of any key two sides must
#: agree on.
KEY_DAEMON = "daemon"


def project(
    mirror: "shapes.ShapeMirror",
    *,
    now: datetime,
    notes: list[str] | None = None,
    absent_note: str | None = None,
    warnings: "shapes.ShapeMirror | None" = None,
    non_idle: "shapes.ShapeMirror | None" = None,
) -> Board:
    """Held shape state -> the board the render layer draws. **THE ONE BUILDER.**

    `render.py` is unchanged in what it consumes: the same `Account` and the
    same `Board`. What changed is where the fields come from and, for the
    unknown ones, that the card can now say WHICH kind of unknown it is.

    `absent_note` is the caller's answer to "does this daemon have folds at
    all", which is a fact about the CONNECTION and not about the mirror, so it
    cannot be derived here. Passing it in is what lets a board with no accounts
    say which of the three empties it is (see `Board.source`) while keeping
    this function pure — dicts and a mirror in, a Board out, no socket and no
    capability registry.
    """
    board = Board(
        fetched_at=mirror.last_frame_at,
        notes=list(notes or []),
        shape_state=mirror.state,
        shape_note=mirror.dead_reason or "",
    )
    if absent_note is not None:
        board.source = SOURCE_ABSENT
        board.source_note = absent_note
    elif mirror.exported:
        board.source = SOURCE_HELD
    else:
        board.source = SOURCE_WAITING
        board.source_note = (
            "waiting for the daemon's first export of the accounts fold"
        )
    # ── THE DAEMON'S WARNING COUNT, OFF ITS OWN MIRROR ───────────────────
    #
    # A SECOND MIRROR rather than a second source: the `warnings` shape is the
    # only place this board learns the number, and nothing else on screen is
    # derived from it. Left as `None` unless the mirror has actually exported,
    # because an un-hydrated mirror's empty row is "we have not been told", and
    # the header must be able to say nothing rather than say zero.
    if warnings is not None and warnings.exported:
        wrow = warnings.row(KEY_DAEMON)
        if isinstance(wrow.get("warn"), int):
            board.daemon_warnings = wrow["warn"]
        if isinstance(wrow.get("error"), int):
            board.daemon_errors = wrow["error"]

    shared = mirror.shared if isinstance(mirror.shared, dict) else {}
    warn = shared.get("warn_below_days")
    if isinstance(warn, (int, float)):
        # RULING 6's home. The threshold rides on the export's `shared` block so
        # no consumer hardcodes it — `tests/ws_protocol.rs` asserts the same
        # thing about `accounts/health`, and this honours that rather than
        # working around it.
        board.warn_below_days = float(warn)

    placed = roster_by_account(non_idle, shared.get("folds_into"))
    roster = roster_cell(non_idle)
    for index, key in enumerate(mirror.keys(), start=1):
        row = mirror.row(key)
        cells = {name: mirror.cell(key, name) for name in SHAPE_CELLS}
        # **`threads` IS THE ROSTER'S CELL, NOT AN ACCOUNTS CELL.** `render.py`
        # asks `account.cell("threads")` whether the running badge may be drawn
        # as current, and the honest answer to that is now the state of the one
        # `threads/non-idle` cell every card's activity is counted from.
        cells["threads"] = roster
        cred = row.get("credential") if isinstance(row.get("credential"), dict) else {}
        identity = cred.get("identity") if isinstance(cred.get("identity"), dict) else {}
        share = row.get("share") if isinstance(row.get("share"), dict) else {}
        press = row.get("pressure") if isinstance(row.get("pressure"), dict) else {}
        md = row.get("machine_default") if isinstance(row.get("machine_default"), dict) else None

        account = Account(
            number=index,
            label=key,
            # `?` is what the render layer draws for an unknown login state,
            # and it is now qualified by `cells["credential"]`: the SAME `?`
            # means three different things and the card says which.
            state=str(cred.get("slot_state") or "?") if cells["credential"].known else "?",
            clone_of=cred.get("clone_of"),
            # THE LOGIN'S END, never the access token's. See `_days_until`.
            # There is deliberately NO fallback to `access_expires_at_ms` when
            # this is absent: a login the server never dated has no stated end,
            # and `login —` says that, where a fallback would print a plausible
            # small number instead.
            days_left=_days_until(cred.get("refresh_expires_at_ms"), now),
            identity_acct=identity.get("acct"),
            identity_email=identity.get("email"),
            subscription_type=cred.get("subscription_type"),
            credential_mode=cred.get("mode"),
            config_dir=row.get("login_dir"),
            keychain_service=cred.get("keychain_service"),
            secret_sha8=cred.get("secret_sha8"),
            is_live_login=bool(cred.get("is_live_login")),
            # THE MACHINE STORE'S LOGIN, and it comes off the machine-default
            # NOTE rather than off `credential`. `credential.live_store_expires_
            # at_ms` is `SlotReport::live_store_access_expires_at_ms` — the
            # machine store's ACCESS token, honestly named on the wire and the
            # same mistake as the line above. The note (ruling 5) lands on this
            # very row — `mark_live_login` picks the host, `hydrates` puts the
            # note on that same host — and carries the store's own
            # `refresh_expires_at_ms`, which is the countdown the line claims.
            live_store_days_left=_days_until(
                (md or {}).get("refresh_expires_at_ms"), now
            ),
            live_store_state=cred.get("live_store_state"),
            axes=_axes_from_gauge(row.get("gauge")),
            activity=_activity_from_roster(placed.get(key, []), roster, row, now),
            cells=cells,
            registered=bool(row.get("registered")),
            machine_default=md,
            moves=[m for m in (row.get("moves") or []) if isinstance(m, dict)],
        )

        # ── pressure: PRODUCED by the daemon, never computed here ───────────
        #
        # The board renders the number it is given and stamps it. There is no
        # formula on this side and there must never be one: `caps.py` says a
        # second implementation is a second answer, and pressure is the fact
        # that rule was written about.
        if cells["pressure"].known and press:
            account.in_pressure_pool = True
            value = press.get("pressure")
            if press.get("ranked") and isinstance(value, (int, float)):
                account.pressure = float(value)
                account.pressure_ranked = True
            account.pressure_reason = str(press.get("unranked_reason") or "")
            if isinstance(press.get("share"), (int, float)):
                account.pool_share = float(press["share"])
            if press.get("disabled") is True:
                account.disabled = True

        if isinstance(share.get("share"), (int, float)):
            account.pool_share = float(share["share"])
        if share.get("disabled") is True:
            account.disabled = True
            # The two facts that ride with it. Read only under `disabled`, so a
            # stale `disabled_at` on a re-enabled row cannot put a date on a card
            # that is back in the pool.
            account.disabled_at = parse_ts(share.get("disabled_at"))
            note = share.get("disabled_note")
            account.disabled_note = str(note) if isinstance(note, str) and note else None

        if account.is_live_login:
            board.live_login = key

        board.accounts.append(account)

    return board



# ── the 7-day history (the `accounts/gauge-history` fold, R2.S3d) ────────────
#
# The payload these readers take was `gauge/history`'s answer; it is now built
# off the held fold by `history_payload`, in the same form, so the binning, the
# projection and the clip note did not fork.
#
# These two are PURE: a decoded `gauge/history` payload in, a sparkline or a
# sentence out. No clock — the binning is done against the span of the points
# THEMSELVES, so there is no hidden input and a fixed payload always renders the
# same figure. The daemon already scopes the window (`days`, default 7).
#
# The old app produced both of these by opening `threads.db` read-only and
# running its own SQL, duplicating the axis vocabulary and the schema on the
# client. `gauge/history` exists specifically to retire that, and this is the
# code that finally consumes it. See `threads_tui.drag`, obligation 2.


def history_payload(
    readings: Any,
    account: str,
    *,
    until: datetime,
    complete: bool = True,
    days: int = 7,
) -> dict:
    """The held `accounts/gauge-history` series, in the `{since, until, series}`
    form the three readers below take.

    **THE SERIES IS THE FOLD'S NOW, NOT A REQUEST'S** (R2.S3d). The readings are
    the row this board holds for `account`; nothing was asked for them. Shaping
    them into the payload the old `gauge/history` answer had keeps
    `history_sparkline`, `burn_projection` and `history_truncated` unchanged —
    one binning, one projection, one clip note — rather than growing a second
    copy of each over a second input.

    `until` is the CALLER's clock (the paint's `now`), because a series held
    across a quiet hour still ends at the present: the x-axis is the last
    `days` days, exactly the window the request used to be asked for. Still
    pure: the clock is an argument. `complete` is the cell's own qualifier —
    `False` when the producer's window could not hold the whole week — and is
    what the clip note reads.
    """
    by_axis: dict[str, list[dict]] = {}
    for r in readings if isinstance(readings, list) else []:
        if not isinstance(r, dict):
            continue
        by_axis.setdefault(str(r.get("axis")), []).append(
            {"at": r.get("at"), "pct": r.get("pct")}
        )
    since = until - timedelta(days=days)
    return {
        "since": since.isoformat(),
        "until": until.isoformat(),
        "series": [
            {"account": account, "axis": axis, "points": points, "truncated": not complete}
            for axis, points in by_axis.items()
        ],
    }


def history_points(payload: Any, account: str, axis: str = "weekly_all") -> list[dict]:
    """The `{at, pct}` points for one account+axis out of a gauge/history payload."""
    if not isinstance(payload, dict):
        return []
    for series in payload.get("series") or []:
        if not isinstance(series, dict):
            continue
        if str(series.get("account")) != account:
            continue
        if axis and str(series.get("axis")) != axis:
            continue
        return [p for p in (series.get("points") or []) if isinstance(p, dict)]
    return []


def history_window(payload: Any) -> tuple[datetime, datetime] | None:
    """The window `gauge/history` was ASKED for, off the payload it answered with.

    `gauge/history` states its own bounds — `{"since": ..., "until": ..., "limit":
    ..., "series": [...]}` (`server.rs`, the `gauge/history` arm) — and those two
    instants are the x-axis the sparkline is drawn on. Reading them is what makes
    the figure's horizontal position mean a TIME rather than an ordinal.

    None when either bound is missing or inverted, which is the pre-`since`/`until`
    daemon. The caller falls back to the span of the observations themselves; that
    is the old behaviour and it is wrong in a knowable way, so it is a fallback
    rather than the rule.
    """
    if not isinstance(payload, dict):
        return None
    since, until = parse_ts(payload.get("since")), parse_ts(payload.get("until"))
    if since is None or until is None or (until - since).total_seconds() <= 0:
        return None
    return since, until


def history_truncated(payload: Any, account: str, axis: str = "weekly_all") -> bool:
    """Did the daemon CLIP this series, so the figure is not the whole window?

    `gauge/history` caps a series at `limit` (default 1000) and says so per
    series — and its own comment is the reason this is read rather than ignored:
    *"a sparkline drawn from a silently-truncated series is a lie about the shape
    of the week."* It was ignored. This is the read that stops it being a lie.

    **WHICH END IS LOST IS THE PART THAT MATTERS.** The query is
    `ORDER BY id DESC LIMIT ?` (`store.rs::observations_between`), reversed back
    to ascending before it returns, so a clipped series keeps the NEWEST rows and
    drops the OLDEST — the end he is NOT looking at. IT USED TO BE THE OTHER WAY
    ROUND, and the note this flag renders used to say so; the reorder and every
    string that describes it were flipped in one commit. Nothing MADE that
    happen, which is worth knowing: the two asserts in `tests/test_sparkline.py`
    pin the note's wording, but they render a Python object and cannot see this
    query, so the reorder landed green against a note that contradicted it.
    `test_the_clip_note_names_the_END_THE_QUERY_ACTUALLY_KEEPS` is the check that
    actually reads `store.rs` and ties the two together. At the busy cadence
    (`probe_interval_busy_secs = 120`) a week is ~5,040 observations per axis
    against a default cap of 1,000 — and against the hard cap of 5,000 as well,
    though only by 40 observations — so this is reachable on an account that has
    been busy for more than about a day and a half, not a theoretical edge.
    """
    if not isinstance(payload, dict):
        return False
    for series in payload.get("series") or []:
        if not isinstance(series, dict):
            continue
        if str(series.get("account")) != account:
            continue
        if axis and str(series.get("axis")) != axis:
            continue
        return bool(series.get("truncated"))
    return False


def history_sparkline(
    payload: Any, account: str, *, axis: str = "weekly_all", buckets: int = 40
) -> list[float | None]:
    """Bin one account's history onto the WINDOW'S grid: `buckets` cells, one maximum
    or `None` each.

    MAXIMUM per bucket, not mean. This is a usage meter against a ceiling, and
    the question it answers is "how close to the limit did this account get",
    which a mean smooths away — a spike to 100% inside a bucket that averages
    40% is the single most important thing the row can show.

    **AN EMPTY BUCKET IS `None`, AND IT HOLDS ITS PLACE.** It is not drawn as
    zero — a gap in polling is not a period of no usage, and drawing it at the
    floor would invent a dip that never happened. But it used to be DROPPED, and
    dropping it was the worse of the two errors, because it destroyed the
    horizontal axis without saying so:

      * cell `i` was the `i`-th bucket that HAPPENED TO HAVE DATA, not the `i`-th
        slice of the week. Two observations six days apart rendered as two
        adjacent glyphs — side by side, reading as consecutive;
      * the figure's WIDTH became a function of how densely that account was
        polled, so no two cards' sparklines lined up with each other down the
        stack, and the burn-rate sentence `_expansion` puts to the right of it
        moved per card.

    The operator on this figure: *"the gauge.history sparkline looks unaligned."*
    It was, in both of those senses, and in a third that is `history_window`'s:
    the grid was the span of the OBSERVATIONS and the label says `weekly 7d`, so
    an account last polled six hours ago drew six hours of history under a
    seven-day heading, stretched across the full width.

    So the grid is the window the daemon was asked for (`history_window`), the
    cells are fixed at `buckets`, and an unobserved slice says so. The result is
    still CLOCK-FREE: both bounds come off the payload, so a fixed payload always
    renders the same figure and there is no hidden input. Only when the payload
    carries no bounds does it fall back to the span of the points.

    Returns `[]` when there is nothing to draw at all — the caller says why.
    """
    points = [
        (parse_ts(p.get("at")), p.get("pct"))
        for p in history_points(payload, account, axis)
    ]
    points = [
        (at, float(pct))
        for at, pct in points
        if at is not None and isinstance(pct, (int, float))
    ]
    if not points:
        return []
    points.sort(key=lambda pair: pair[0])

    window = history_window(payload)
    if window is None:
        # THE FALLBACK, and it keeps the grid even though it cannot keep the
        # axis. A one-point series has no span to bin against either way.
        if len(points) == 1:
            return [points[0][1]]
        first, last = points[0][0], points[-1][0]
        if (last - first).total_seconds() <= 0:
            return [max(pct for _, pct in points)]
    else:
        first, last = window

    span = (last - first).total_seconds()
    if span <= 0:
        return [max(pct for _, pct in points)]

    binned: list[list[float]] = [[] for _ in range(buckets)]
    for at, pct in points:
        index = int((at - first).total_seconds() / span * (buckets - 1))
        binned[max(0, min(buckets - 1, index))].append(pct)
    # EVERY bucket, in order, with `None` where nothing was observed. The list
    # length is the grid and not the sample count, which is the whole fix.
    return [max(values) if values else None for values in binned]


# The refusal thresholds for a projection. Each one exists to stop a confident
# sentence being produced from evidence that cannot support it.
_MIN_SAMPLES = 3
_MIN_SPAN_HOURS = 1.0
_MIN_SLOPE_PCT_PER_HOUR = 0.05


def burn_projection(
    payload: Any,
    account: str,
    axis_state: AxisState | None,
    *,
    axis: str = "weekly_all",
    window_hours: float = 6.0,
) -> str | None:
    """"reaches 100% in ~2d 04h" — or None, which is the common answer.

    DELIBERATELY EASY TO REFUSE. Extrapolating a limit from a handful of noisy
    observations is how a board starts telling the operator things that are not
    true, and a wrong ETA on a quota is worse than no ETA: he plans around it.
    So this returns None unless the reading is calibrated and confident, there
    are at least three samples spanning at least an hour, and the slope is
    actually positive. It also never projects an axis already at the limit —
    there is nothing left to reach.

    The word "pace" does not appear in the output. See `pressure.py`.
    """
    if axis_state is None or not axis_state.calibrated or axis_state.pct is None:
        return None
    if axis_state.low_confidence or axis_state.at_limit:
        return None

    points = [
        (parse_ts(p.get("at")), p.get("pct"))
        for p in history_points(payload, account, axis)
    ]
    points = [
        (at, float(pct))
        for at, pct in points
        if at is not None and isinstance(pct, (int, float))
    ]
    if len(points) < _MIN_SAMPLES:
        return None
    points.sort(key=lambda pair: pair[0])

    last_at = points[-1][0]
    recent = [
        (at, pct)
        for at, pct in points
        if (last_at - at).total_seconds() <= window_hours * 3600.0
    ]
    if len(recent) < _MIN_SAMPLES:
        return None

    span_hours = (recent[-1][0] - recent[0][0]).total_seconds() / 3600.0
    if span_hours < _MIN_SPAN_HOURS:
        return None

    # A WINDOW ROLL READS AS A CRASH. The weekly meter resets to near zero when
    # its window rolls, so a naive endpoint slope across a reset is a large
    # NEGATIVE number, and the sign test below is what rejects it rather than
    # projecting from nonsense.
    slope = (recent[-1][1] - recent[0][1]) / span_hours
    if slope < _MIN_SLOPE_PCT_PER_HOUR:
        return None

    remaining = 100.0 - axis_state.pct
    if remaining <= 0:
        return None
    hours = remaining / slope
    return f"{AXIS_LABEL.get(axis, axis)} reaches 100% in ~{_short_delta(hours * 3600.0)}"


def _short_delta(seconds: float) -> str:
    """`2d 04h` / `4h 20m` / `35m`. Local so this module imports nothing new."""
    seconds = max(0.0, seconds)
    days, rem = divmod(int(seconds), 86400)
    hours, rem = divmod(rem, 3600)
    minutes = rem // 60
    if days:
        return f"{days}d {hours:02d}h"
    if hours:
        return f"{hours}h {minutes:02d}m"
    return f"{minutes}m"


def pressure_rows(board: Board, selected_label: str | None = None):
    """Project the board onto the pressure view's row type. Pure."""
    from .pressure import PressureRow

    return [
        PressureRow(
            number=a.number,
            label=a.label,
            pressure=a.pressure,
            share=a.pool_share,
            reason=a.pressure_reason,
            selected=(a.label == selected_label),
            in_pool=a.in_pressure_pool,
        )
        for a in board.accounts
    ]
