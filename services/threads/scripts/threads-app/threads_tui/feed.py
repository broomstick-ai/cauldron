"""THE LIVE NARRATION LINE, and the coalescer in front of the key re-enumeration.

**THIS MODULE DOES TWO JOBS AND IT USED TO DO THREE.** S3 deleted the third.

  1. **NARRATION, READ OFF THE `narration` FOLD.** `info`, `progress` and
     `lifecycle` are the daemon talking while it works — a rotation reaching a
     phase, a usage reading recorded, the service going down and coming back.
     They used to arrive as FIREHOSE frames, and reading them that way is what
     kept this board on the firehose. The daemon now folds each into a LINE on
     the `narration` shape (`shapes.NARRATION`, cell `lines`, tagged on
     `kind`), and `on_line()` takes a line as the fold holds it — never a frame
     off the socket. `note()` turns it into the one-line `⚡` the board draws,
     aged out on a TTL so a live narration does not silently become a claim
     about the present.

  2. **`subscribed`.** Set by `narrating()`, when the daemon has EXPORTED the
     narration fold to this connection — the producer's own word that the
     shape carrying these lines is flowing here. See below: a connection can be
     open, healthy and unsubscribed, which delivers nothing forever while
     looking perfectly live, and the link state cannot see it.

  3. **SCHEDULING A BOARD RE-READ (deleted, S3).** An announcement used to
     schedule five queries — `reload()` — and that is gone with them. What
     remains of the machinery is narrower and is not a re-read: an
     announcement now means **"the KEY SET may have moved"**, because
     `server.rs::drive` drops a live `row` for a key this connection has not
     been exported, so an account added to the pool would go unseen. The
     response is one keyless `shapes/{shape}/subscribe` (`app._feed_announced`), and
     the `domains` this still carries are used for ONE thing: a `gauge`-domain
     announcement is exempt from that re-enumeration (a reading cannot move the
     key set). It used to also invalidate the 7-day sparkline, which came from
     `gauge/history`; since R2.S3d the series is the `accounts/gauge-history`
     shape and needs no invalidation.

WHY THE MODULE EXISTED AT ALL. The app already held a WebSocket open and
already subscribed to the firehose, and it still only updated when the operator
pressed `r`. Both halves of that were true at once because the subscription was
used for exactly one thing: counting running threads. Every other frame was
dropped, while the FACTS ON THE BOARD were re-read only by `reload()`, which
had two callers: a timer and a key.

So the screen was a poll dressed in a socket. His words:

    "The only updates I'm getting are when I trigger a refresh... If I have it
    open, that means I want to see live stuff happening in there as it's
    happening. All information updates should get relayed out as a broadcast
    and picked up by my text user interface."

COALESCING IS NOT AN OPTIMIZATION, IT IS REQUIRED. A `gauge/probe` across four
logins emits a dozen `info` frames inside a second, and a `pool_reloaded` burst
is the same shape. Re-enumerating a dozen times would ask the daemon to
re-export every key it holds a dozen times, and each export is a ~1-2 KB row
per account. One re-enumeration, shortly after the burst starts, is both
cheaper and exactly as correct.

WHAT `subscribed` IS FOR, AND WHY THE LINK STATE IS NOT ENOUGH. A socket that
dies is caught by the link state: `api.py` runs a 20-second ping with a
20-second timeout, so a dead connection becomes RECONNECTING and the header says
so. But there is a second failure that the link state CANNOT see — a connection
that is open and healthy and was never subscribed, because the subscribe was
lost or refused. The daemon delivers a shape's frames only to a connection it
has exported that shape to, so that connection receives nothing, forever, while
looking perfectly live. It is indistinguishable from a quiet system, which is
the exact failure being fixed here. The narration EXPORT is the daemon's own
confirmation that it is narrating to this connection, so it is recorded
(`narrating()`) and rendered. (It used to be the firehose's `sub_ack`; the
firehose is gone from this board.)

PURITY. Every method takes `at` — a monotonic seconds float — as a PARAMETER.
This module reads no clock and does no I/O, which is what lets
`tests/test_feed.py` drive a whole burst-and-coalesce cycle with no sleeping and
no daemon, and prove the response is scheduled BY A FRAME rather than by a
timer.
"""

from __future__ import annotations

from dataclasses import dataclass, field

# How long to wait after the first announcement of a burst before acting.
# Long enough that a multi-login probe lands as ONE re-enumeration, short
# enough that it
# still reads as "immediately" to someone watching the screen. The operator's
# complaint was about minutes; this is under half a second.
COALESCE = 0.4

# The LINE kinds that announce an information update — a narration line's
# `kind`, which is the channel it was appended to. `info` is the general
# channel the daemon grew for exactly this; the other two are older, narrower
# frames that also imply the board moved and were already on the wire.
#
#   info       — the general announcement. `domain` says what moved; the board
#                uses it for the gauge/sparkline invalidation only.
#   progress   — a login or rotation reached a phase. Its TERMINAL frame
#                (`done`) means an account operation finished, so the roster and
#                the health column have changed.
#   lifecycle  — the daemon went down or came back. Everything is suspect.
#
# `liveness` and `status` are deliberately NOT here: they are thread facts and
# the `accounts` fold owns them now, so nothing on this board reads them at all
# (S3 deleted `live.py`). Acting on a ~1 Hz-per-busy-thread heartbeat would also
# put the daemon under a heavier load than the 300-second timer this replaced.
ANNOUNCING = ("info", "progress", "lifecycle")


@dataclass
class Feed:
    """What the daemon has told us, and when we last acted on it."""

    coalesce: float = COALESCE

    # True once the daemon EXPORTED the narration fold to this connection
    # (`narrating()`). Until then the socket may be perfectly healthy and still
    # deliver nothing.
    subscribed: bool = False

    # The monotonic `at` of the newest shape frame applied. This is the number
    # the operator asked for by name — "I'd like to see when the last refresh
    # ran" — and it is deliberately not the age of the oldest usage reading,
    # which answers a different question and is the number he was shown instead.
    #
    # SINCE S3 IT MEASURES THE FOLD RATHER THAN A POLL, and that changes what a
    # large number MEANS: on a healthy feed it is QUIET TIME (the daemon has had
    # nothing to tell us), not staleness. `render.refresh_segment` already
    # splits on exactly that, off `subscribed`.
    last_refresh_at: float | None = None

    # The most recent announcement, for the one-line "what just moved" note.
    reason: str = ""
    account: str | None = None
    detail: str = ""
    # When that note landed, so it can AGE OUT. A live narration line that stays
    # on screen forever stops being narration and becomes a claim about the
    # present — "rotating slot 3 of 4" an hour after the rotation finished is
    # worse than no line at all.
    note_at: float | None = None

    # Domains announced and not yet acted on.
    _pending: set[str] = field(default_factory=set)
    # When the coalesced re-enumeration comes due. None = nothing scheduled.
    _due_at: float | None = None

    # ── what the daemon says ─────────────────────────────────────────────────

    def narrating(self) -> bool:
        """The daemon exported the `narration` fold to this connection.

        Returns True if that changed what the header should say. It schedules
        NOTHING: the firehose's `sub_ack` used to schedule a reseed here, and
        that reseed was always the keyless subscribe `_on_caps` sends on every
        connection anyway — the export that lands here is an ANSWER to that
        subscribe, so there is nothing left for it to ask for.
        """
        was = self.subscribed
        self.subscribed = True
        return not was

    def on_line(self, line: dict, *, at: float) -> bool:
        """Absorb one narration LINE, as the fold holds it. True = repaint.

        The argument is a line off the `narration` mirror (`shapes.appended_line`)
        — tagged on `kind`, the channel it was appended to — and never a frame
        off the socket. A firehose frame is tagged on `type` instead and has no
        `kind`, so one handed here by mistake announces nothing.

        Scheduling work and repainting are different things: the note line
        changes the moment the line lands, and the DATA changes one coalesce
        window later. Returning True on the announcement is what makes the
        screen acknowledge the daemon instantly rather than after the
        re-enumeration.
        """
        if not isinstance(line, dict):
            return False
        kind = line.get("kind")

        if kind not in ANNOUNCING:
            return False

        # Every announcing frame stamps the note, including the intermediate
        # progress phases that schedule nothing: narrating a rotation
        # while it runs is the point, and it is exactly what he meant by wanting
        # to see things "as it's happening" rather than after.
        self.note_at = at

        if kind == "progress":
            # Only the TERMINAL line means a fact changed. The intermediate
            # phases are narration of an operation still in flight, and
            # acting on the board halfway through a rotation would show a
            # half-rotated pool as though it were the outcome.
            self.detail = str(line.get("detail") or "")
            self.reason = str(line.get("op") or "progress")
            self.account = line.get("account")
            if not line.get("done"):
                return True  # repaint the note, schedule nothing
            domain = "accounts"
        elif kind == "lifecycle":
            self.reason = f"{line.get('service') or 'daemon'} {line.get('phase') or ''}".strip()
            self.detail = str(line.get("detail") or "")
            self.account = None
            domain = "accounts"
        else:
            self.reason = str(line.get("reason") or "changed")
            self.detail = str(line.get("detail") or "")
            self.account = line.get("account")
            # An unrecognized domain still schedules. The daemon is entitled
            # to grow a domain this client has never heard of, and the honest
            # response to "something you display changed" is to make sure we
            # are subscribed to every key it holds, not to drop the frame
            # because the label is unfamiliar.
            domain = str(line.get("domain") or "accounts")

        self._pending.add(domain)
        if self._due_at is None:
            self._due_at = at + self.coalesce
        return True

    # ── the coalesced response ───────────────────────────────────────────────

    def due(self, *, at: float) -> bool:
        """Is a scheduled re-enumeration now due?"""
        return self._due_at is not None and at >= self._due_at

    def pending(self) -> bool:
        """Is a re-enumeration scheduled but not yet run?"""
        return self._due_at is not None

    def announced_at(self) -> float | None:
        """When the burst now scheduled FIRST announced itself. None = nothing.

        Read-only, and it exists for one consumer: the shape path re-enumerates
        keys on an announcement, and it must be able to tell an announcement
        that PREDATES its last enumeration from one that followed it. The
        first is already covered by the enumeration that came after it; acting
        on it again asks the daemon to re-export every key for nothing.
        """
        return None if self._due_at is None else self._due_at - self.coalesce

    def take(self, *, at: float) -> set[str]:
        """Claim the schedule. Returns the domains that announced.

        Clearing the schedule HERE, rather than when the work finishes, is
        deliberate: an announcement that lands DURING a re-enumeration must
        schedule a second one, because the subscribe it arrived behind was
        placed in the daemon's FIFO before the change being announced.
        Clearing on completion would swallow that frame and leave the board one
        update behind with nothing on screen saying so.
        """
        domains = self._pending
        self._pending = set()
        self._due_at = None
        return domains

    def refreshed(self, *, at: float) -> None:
        """A shape frame was applied. This is what the age on screen measures."""
        self.last_refresh_at = at

    def refresh_age(self, *, at: float) -> float | None:
        """Seconds since the last applied frame. None = nothing has arrived."""
        if self.last_refresh_at is None:
            return None
        return max(0.0, at - self.last_refresh_at)

    # ── the live narration line ──────────────────────────────────────────────

    def note(self, *, at: float, ttl: float = 30.0) -> str:
        """The 'what just moved' line, or '' once it has aged out.

        Returns prose assembled from the announcement's own fields. The daemon's
        `detail` is preferred when it sent one — it is the only party that knows
        what actually happened — and the machine tags are the fallback, so a
        reason this client has never heard of still says something true rather
        than nothing.
        """
        if self.note_at is None or at - self.note_at > ttl:
            return ""
        if self.detail:
            return self.detail
        if not self.reason:
            return ""
        if self.account:
            return f"{self.account}: {self.reason}"
        return self.reason

    # ── the socket died ──────────────────────────────────────────────────────

    def reset(self) -> None:
        """The connection dropped. Nothing scheduled survives it.

        `last_refresh_at` is KEPT on purpose: the board is still showing data
        fetched at that instant, and the age of it is exactly what the operator
        needs while the link is down. Zeroing it would replace a true and
        increasingly alarming number with no number at all, at the moment the
        number matters most.
        """
        self.subscribed = False
        self._pending = set()
        self._due_at = None
