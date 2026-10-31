"""Pure rendering: board state in, Rich `Text` lines out.

Copied from the old app's `render.py`, which was the part worth preserving —
the layout and geometry are unchanged and this is deliberately a near-verbatim
lift. What changed is the purity, which the old version claimed and did not
quite have:

  * `now_utc` is no longer imported. It cannot be called if it is not in scope,
    which is a stronger guarantee than a convention. Every time-dependent value
    now arrives as an explicit `now` parameter.

  * `card_lines(now=...)` is REQUIRED, not defaulted. The old signature accepted
    `now=None` and filled it from the clock — but then did not thread it into
    `_axis_line`, which read the clock again on its own. So passing a fixed
    `now` did not actually fix the output: the axis rows still moved. A
    defaulted clock is how that leak survived, so the default is gone.

  * `LinkState` comes from `link.py`, not from the module that owns the socket.
    The old import chain reached `paths.py`, which read the environment at
    module scope, so importing the "pure" render layer had a side effect.

This matters beyond tidiness. A pure render layer makes the eventual Rust port
(see `threads_tui.drag`) a TRANSLATION; an impure one makes it a redesign,
because every hidden input has to be found before anything can move. It is also
what makes golden-testing the board possible at all — with a real clock inside,
no two runs agree.

Nothing here touches the network, the filesystem, a subprocess or the clock.
`tests/test_purity.py` enforces that mechanically.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime

from rich.text import Text

from .link import LinkState
from .fmt import (
    AMBER,
    ATLIMIT,
    BLUE,
    GREEN,
    INK,
    INK2,
    MUTED,
    RED,
    age_style,
    bar,
    ellipsize,
    fmt_age,
    fmt_clock,
    fmt_delta,
    fmt_days,
    fmt_pct,
    meter_color,
    spinner_frame,
)
from .model import (
    AXIS_LABEL,
    Account,
    Board,
    CRITICAL,
    SOURCE_ABSENT,
    SOURCE_WAITING,
)
from . import pressure

# ── layout modes (DESIGN §3.6) ───────────────────────────────────────────────

FULL = "full"
NARROW = "narrow"
COMPACT = "compact"


def layout_mode(width: int, height: int) -> str:
    if width < 80 or height < 20:
        return COMPACT
    if width < 100:
        return NARROW
    return FULL


BAR_WIDTH = {FULL: 25, NARROW: 15, COMPACT: 12}


# ── small helpers ────────────────────────────────────────────────────────────


def join(lines: list[Text]) -> Text:
    """Stack pre-measured lines into one non-wrapping, non-reflowing block.

    Every line this module produces is already exactly the widget's width, so
    wrapping would only ever be a bug made visible; `no_wrap` makes it a crop
    instead of a broken layout."""
    out = Text(no_wrap=True, overflow="crop")
    for index, line in enumerate(lines):
        if index:
            out.append("\n")
        out.append_text(line)
    return out


def _pad(text: Text, width: int) -> Text:
    """Crop or pad a single line to exactly `width` cells."""
    length = text.cell_len
    if length > width:
        text = text[:width] if width > 0 else Text("")
        # cell_len may still exceed width with wide glyphs; trim defensively.
        while text.cell_len > width:
            text = text[: len(text.plain) - 1]
        length = text.cell_len
    if length < width:
        text.append(" " * (width - length))
    return text


def _row(inner: int, left: Text, right: Text | None = None) -> Text:
    """A card body line: `left`, padding, `right`, exactly `inner` cells."""
    if right is None or right.cell_len == 0:
        return _pad(left.copy(), inner)
    room = inner - right.cell_len - 1
    line = _pad(left.copy(), max(0, room))
    line.append(" ")
    line.append_text(right)
    return _pad(line, inner)


# ── the header (DESIGN §3.2) ─────────────────────────────────────────────────


@dataclass
class HeaderState:
    link: LinkState = LinkState.CONNECTING
    retries: int = 0
    # `seconds_to_refresh` USED TO LIVE HERE and is deliberately gone rather
    # than kept and ignored. It fed a `next refresh M:SS` countdown to a timer
    # that no longer drives the board (see `header_line1`), and a field nothing
    # reads is a field the next person wires back up. The app stopped computing
    # it in the same change.
    refreshing: bool = False
    # `probing` AND `probe_elapsed` STOOD HERE. They drove the `probing all
    # accounts 3s` banner for the `R` key's pool-wide `claude /usage`. The key is
    # gone and so is the act, so there is nothing left to narrate — see the
    # binding table in `app.py` for why the key's absence is a guarantee rather
    # than a removal.
    tick: int = 0
    degraded_from_db: bool = False
    # How many threads are running right now, off the daemon's pushed
    # `liveness`/`status` frames. `running_live` is False until the first push
    # arrives, so a count of 0 that means "nobody has told us" is never drawn
    # as a confident zero.
    running: int = 0
    # THE POOL'S QUEUE DEPTH, in THREADS, summed across every account's rows —
    # the board-level fact that used to be drawn on individual cards as though
    # each account owned a backlog. There is no per-account queue; the queue is
    # one table keyed by `thread_id`. Gated by `running_live` with `running`,
    # because both are read off the same export: `queued ?` until one lands.
    queued: int = 0
    # THE POOL'S PARKED COUNT, in threads (H179). Parked is a POOL state: a parked
    # turn waits in the pool and is re-placed onto whichever account admission
    # picks on the next placement event, so it is drawn here, once, and never on
    # the card of the account the thread last sat on.
    parked: int = 0
    # HOW MANY ACCOUNTS COULD TAKE A TURN RIGHT NOW — not disabled, login usable,
    # and not inside an observed limit window. `None` means nobody has worked it
    # out, which is NOT the same as zero and must never be drawn as "nothing can
    # run": see `running_segment`. It exists to separate `0 running` because the
    # pool is quiet from `0 running` because the pool is shut.
    placeable: int | None = None
    running_live: bool = False
    # Whether the daemon has CONFIRMED this connection is subscribed to the
    # firehose (`sub_ack`). See `feed.py`: a connection can be open, healthy and
    # unsubscribed, which delivers nothing forever while looking perfectly live.
    subscribed: bool = False
    # Seconds since the board's data was last actually re-read. None = never.
    # THE NUMBER HE ASKED FOR BY NAME, and not the same number as
    # `freshness_segment`'s — see `refresh_segment`.
    refresh_age: float | None = None
    # ── `unplaced` AND `placement_blocked` ARE DELETED (S3) ──────────────────
    #
    # They carried the RESIDUAL: running threads this client counted but could
    # not attribute to a card, drawn as `▶ 4 running · 1 placing…` beside cards
    # summing to 3. That residual existed because the count and the placement
    # were TWO facts arriving by two routes — a pushed `liveness` beat said a
    # thread was running, and a `threads/{thread_id}/get` round trip said where.
    #
    # It is not that the residual is now always zero. It CANNOT EXIST: a thread
    # row lives under the account it was placed on, so the header total is the
    # sum of the cards by construction and there is nothing left over to name.
    # A field that can only ever be 0 is a field the next person wires back up,
    # so it is removed rather than pinned — the same reasoning that removed
    # `seconds_to_refresh` above. (A placement the DAEMON itself cannot name is
    # counted on `shapes/list` as `unplaced_status`; it is a fact about the
    # fold, it is not guessed at here, and it is not this field.)


def feed_segment(state: HeaderState) -> Text:
    """Is the LIVE FEED actually delivering?

    **A FROZEN SCREEN THAT LOOKS CALM IS THE FAILURE THIS EXISTS TO PREVENT.**
    The board is driven by pushed announcements now, which means the interesting
    question is no longer only "is the socket up" but "are updates arriving".
    Those come apart in one specific way, and it is silent: the daemon gates
    every push behind a per-connection subscribe flag that defaults to OFF, so a
    connection whose `subscribe` was lost stays open, answers requests, passes
    pings, and pushes nothing — indistinguishable from a system where nothing is
    happening.

    So the subscription is drawn as its own fact, in amber, rather than being
    folded into the link state that cannot see it.
    """
    text = Text()
    if state.link is not LinkState.LIVE:
        # The socket itself is down; `daemon_segment` has already said so in
        # red, and the operator needs to know the consequence, which is that
        # nothing on this screen will move on its own until it is back.
        text.append("⚡ live updates stopped", style=RED)
        return text
    if not state.subscribed:
        text.append("⚡ NOT subscribed — no live updates", style=AMBER)
        return text
    text.append("⚡ ", style=GREEN)
    text.append("live", style=INK2)
    return text


def link_segment(state: HeaderState) -> Text:
    """**ONE WORD WHEN THE TWO FACTS AGREE, TWO FACTS WHEN THEY DO NOT.**

    The header used to draw `threads ● live · ⚡ live` — two segments, both
    saying "live", both true, and about DIFFERENT THINGS:

      * `daemon_segment` — the SOCKET. Is there a connection to the daemon.
      * `feed_segment` — the SUBSCRIPTION. Is this connection actually being
        pushed to.

    They are genuinely two facts and they genuinely come apart, which is the
    whole reason `feed_segment` was written: the daemon gates every push behind
    a per-connection flag that defaults to OFF, so a connection whose
    `subscribe` was lost stays open, answers requests, passes pings, and pushes
    nothing — a frozen screen that looks perfectly calm. No link state can see
    that, so it needs its own word.

    But in the healthy case — which is nearly all the time — they collapse to
    the same word twice, and a header that says "live · live" teaches the
    operator to read past both of them. That is the cost of a duplicated signal:
    it does not make the failure more visible, it makes it less.

    So: when the socket is up AND the subscription is confirmed, this draws ONE
    "live". The instant they disagree it SPLITS, and the split is the loud case
    rather than the quiet one. Nothing is lost; the second word is spent only
    when it carries information.
    """
    if state.link is LinkState.LIVE and state.subscribed:
        text = Text()
        text.append("threads ", style=MUTED)
        text.append("●", style=GREEN)
        text.append(" live", style=INK2)
        return text

    text = daemon_segment(state)
    if state.link is LinkState.LIVE:
        # THE SILENT FAILURE, AND THE ONLY CASE THAT NEEDS BOTH WORDS. The
        # socket is fine and the daemon is answering; what is broken is that
        # nothing will arrive unasked. `daemon_segment` cannot say this — it
        # only knows about the connection — so the second fact is spelled out
        # beside it.
        text.append(" · ", style=MUTED)
        text.append_text(feed_segment(state))
    # Every other state is a socket state, and `daemon_segment` has already
    # said it in red or amber. Appending "⚡ live updates stopped" to
    # "threads ● offline" would be the same duplication in a louder colour.
    return text


def last_update_segment(state: HeaderState) -> Text:
    """**`last update: <age>` — THE HEADER'S ONE AND ONLY FRESHNESS READING.**

    It was `refresh_segment`, it drew `read 4m ago`, and it stood next to a
    second reading, `freshness_segment`'s `usage readings: oldest 8m old`. The
    operator struck both:

        *"let's remove the read <time> ago and the usage readings: <time ago>.
        They don't help me. You could have **last update: <time ago>** so we can
        get a sense of updated shapes but that's it."*

    **WHAT IT MEASURES, AND IT IS THE CLIENT'S OWN KNOWLEDGE.** `refresh_age` is
    `feed.Feed.refresh_age` — seconds since THIS BOARD last applied a shape
    frame. It is the only number on this screen that can say *my view is live*,
    because it is a fact about arrival rather than about production. His own
    words name it: "a sense of updated SHAPES" is the shape frame, and nothing
    else on the wire is called a shape.

    **WHAT WAS REJECTED: the newest `updated_at` across the account rows.** That
    is the obvious alternative and it is producer-side. It answers "when did the
    daemon last change something", which is a fact about the DAEMON and says
    nothing about whether frames are reaching this process — a daemon writing
    rows into a fold this client stopped receiving would hold that number at a
    few seconds forever while the screen froze. It is the exact shape of the
    failure `feed_segment` exists to catch, and putting a producer number under
    the words "last update" would quietly undo it. The board already separates
    client-authoritative facts from producer facts (see `model.py`'s header, and
    `_card_stamp`, which is where producer-side observation ages still live, on
    the card, about the account they belong to). This stays on the client side of
    that line.

    **QUIET WHEN FRESH, LOUD WHEN STALE — and that is a choice about wallpaper,
    not about colour.** A number that is always on screen and always small stops
    being read; 27,705 unread warnings on this machine are what that looks like.
    So the weight is spent only where it buys something:

      * feed healthy  →  `last update: 4m ago`, MUTED, no prefix, no weight,
        never escalating. On a feed driven by announcements a large age here is
        QUIET TIME — the daemon has had nothing to tell us — and escalating on
        it would turn a working idle system amber for doing nothing wrong, which
        is the fastest way to teach someone to ignore a colour.
      * feed NOT healthy  →  `STALE — last update: 4m ago`, **bold**, escalating
        through `age_style`. This is the one case where the number is a warning,
        and it is now the only case that looks like one. The bold is the new
        part: the old line escalated its colour and kept its weight, which on a
        header full of muted text is a difference he was not seeing.
    """
    text = Text()
    if state.refreshing:
        # A settle is in flight (`resync` after a mutation), so there is no age
        # to report yet — two answers to one question is the thing this whole
        # segment was collapsed to avoid.
        text.append(f"refreshing {spinner_frame(state.tick)}", style=AMBER)
        return text
    if state.refresh_age is None:
        # NOT "0s ago". Nothing has ever arrived, and the honest word for that is
        # not a small number.
        text.append("last update: none yet", style=MUTED)
        return text
    # Under two seconds reads as "now". A counter flickering 0s/1s/0s draws the
    # eye to the one part of the header that is least worth watching.
    if state.refresh_age < 2:
        text.append("last update: just now", style=GREEN)
        return text
    healthy = state.link is LinkState.LIVE and state.subscribed
    if healthy:
        text.append(f"last update: {fmt_age(state.refresh_age)} ago", style=MUTED)
        return text
    color, _ = age_style(state.refresh_age)
    text.append(
        f"STALE — last update: {fmt_age(state.refresh_age)} ago", style=f"bold {color}"
    )
    return text


def running_segment(state: HeaderState) -> Text:
    """The live running-thread count, pushed rather than polled — AND the part
    of it the cards cannot show yet.

    Drawn next to the title because that is where he is looking when he
    submits a thread and waits for it to land.

    **IT IS THE SUM OF THE CARDS, AND SINCE S3 IT CANNOT BE ANYTHING ELSE.**
    There used to be a residual segment here — `▶ 4 running · 1 placing…` — for
    the threads this client had counted but could not attribute to an account,
    because the count arrived on a `liveness` beat and the placement arrived on
    a separate `threads/{thread_id}/get`. A thread's row now lives under the account it was
    placed on, so the two are one fact and the arithmetic closes by
    construction. See the deletion note on `HeaderState`.

    `running ?` is still drawn, and it is the honest half that survives: before
    the first export has landed, 0 means "nobody has told us" and must never be
    painted as a confident zero.

    **QUEUE DEPTH IS THE SECOND POOL FACT AND IT LIVES HERE, NOT ON A CARD.**
    It used to be drawn per account as `● queued N` (see the long note where that
    branch was deleted in `_badges`). There is no per-account queue to draw: the
    only queue is one table keyed by `thread_id`, and accounts are placement
    targets. The operator said it in one line — "threads should have a queue for
    the pool not an individual account" — and this is where a pool fact belongs,
    beside the other one, counted once.

    IT IS IN THREADS, which is the unit the queue is actually keyed in. The old
    per-card figure could be either threads or messages depending on which was
    larger; this one has a single unit and no second input to be confused with.

    IT FOLLOWS `running`'s THREE-VALUED HONESTY, on the same `running_live` flag
    and deliberately in the same early return: both numbers come from the same
    export, so there is no state in which one is trustworthy and the other is a
    guess. Before the first export `queued ?` is the truth, and `queued 0` would
    be a confident zero about a backlog nobody has told us about yet.
    """
    text = Text()
    if not state.running_live:
        text.append("▶ ", style=MUTED)
        text.append("running ?", style=MUTED)
        text.append(" · ", style=MUTED)
        text.append("● ", style=MUTED)
        text.append("msgs queued ?", style=MUTED)
        return text
    text.append("▶ ", style=GREEN if state.running else MUTED)
    style = f"bold {INK}" if state.running else MUTED
    text.append(f"{state.running} running", style=style)
    text.append(" · ", style=MUTED)
    text.append("● ", style=AMBER if state.queued else MUTED)
    text.append(
        f"{state.queued} msgs queued", style=f"bold {INK}" if state.queued else MUTED
    )
    # ── PARKED, A POOL FACT (H179) ───────────────────────────────────────────
    #
    # *"you never park TO an account."* The card used to say `· 3 parked` under
    # the account each thread last sat on, which read as the daemon holding those
    # turns for that account. It does not: every placement event re-places every
    # parked turn over the whole pool. Drawn only when non-zero, so a quiet pool's
    # header is unchanged, and under the same `running_live` gate as its
    # neighbours (the early return above).
    if state.parked:
        text.append(" · ", style=MUTED)
        text.append("◆ ", style=AMBER)
        text.append(f"{state.parked} parked in pool", style=f"bold {INK}")
    # ── NOTHING RUNNING vs NOTHING CAN RUN ───────────────────────────────────
    #
    # *"threads is still not displaying running threads so its impossible for me
    # to know."* With the producer's `executing`/`state` thrash fixed, `0 running`
    # became HONEST — nothing was running, because every account in the pool was
    # barred or disabled. But honest and useless is still useless: `0 running` on
    # a quiet pool and `0 running` on a pool that CANNOT accept work are the same
    # two characters, and they call for opposite responses. The first is nothing
    # to do; the second is why his work is not starting.
    #
    # It is drawn only when it is true AND only when `running` is 0 — with a turn
    # in flight the pool is demonstrably able to run something, and the sentence
    # would be a contradiction next to the count beside it. The WHY is not
    # repeated here: the bars and the disables each have their own warning line
    # already, and this says only "look at them".
    if state.running == 0 and state.placeable == 0:
        text.append(" · ", style=MUTED)
        text.append("no account can accept work", style=AMBER)
    return text


def daemon_segment(state: HeaderState) -> Text:
    text = Text()
    if state.link is LinkState.LIVE:
        text.append("threads ", style=MUTED)
        text.append("●", style=GREEN)
        text.append(" live", style=INK2)
    elif state.link is LinkState.CONNECTING:
        text.append("threads ", style=MUTED)
        text.append(f"◌ connecting… {spinner_frame(state.tick)}", style=MUTED)
    elif state.link is LinkState.RECONNECTING:
        text.append("threads ", style=MUTED)
        text.append(
            f"◌ reconnecting {spinner_frame(state.tick)} (retry {state.retries})",
            style=AMBER,
        )
    else:
        text.append("threads ", style=MUTED)
        text.append("●", style=RED)
        text.append(" offline — last recorded readings; refresh disabled", style=AMBER)
    return text


# ── `freshness_segment` IS DELETED, AND IT IS THE OPERATOR'S OWN CUT ─────────
#
# It drew `usage readings all ≤4m old` / `usage readings: oldest 1d 17h old` —
# the worst observation age across every account's every axis, pooled into one
# header number. His words: *"let's remove the read <time> ago and the usage
# readings: <time ago>. They don't help me. You could have last update: <time
# ago> so we can get a sense of updated shapes but that's it."*
#
# It is deleted rather than kept and unused, for the same reason
# `seconds_to_refresh` and `unplaced` were: a function nothing calls is a
# function the next person wires back up, and the point of the cut is that this
# header carries ONE freshness reading.
#
# WHAT IS NOT LOST. The producer-side observation age still exists and is still
# drawn — on the CARD, about the account it belongs to, by `_card_stamp`
# (`obs 4m ago` / `STALE 2h`) and per axis by `_axis_stamp`. That is the right
# home for it twice over: it is per-account detail, and pooling four accounts'
# worst axis into one header number was an aggregate nobody could act on. It had
# the same defect Correction 2 names — one account's gauge quoted in the header —
# with the account's name left off.
#
# AND IT WAS A PRODUCER NUMBER WEARING A CLIENT NUMBER'S CLOTHES. Side by side
# with `read 4m ago` the two read as rival measurements of one quantity. See
# `last_update_segment` for which one survived and why.


def warnings_segment(board: Board) -> Text:
    """**THE DAEMON'S OWN WARNING COUNT, AND IT IS ABSENT UNLESS IT IS REAL.**

    The operator's ask: *"For TUI and the threads tui can you display the number
    of warnings coming from threads so it annoys me? 27k is incredible."*

    The 27k went unseen for one reason: its only output was a log file, and a log
    file is a thing you have to decide to read. So the number comes here — but
    **a number that is always on screen becomes wallpaper, which is how the log
    failed in the first place.** Hence the three states, and nothing else:

    | the daemon says | this draws |
    |---|---|
    | nothing (`None`) | **nothing** — see below, this is not zero |
    | zero | **nothing** |
    | anything else | `⚠ 47 warnings`, in amber; red once any ERROR is counted |

    **WHY "APPEARS ONLY WHEN NON-ZERO" AND NOT ONE OF THE OTHER LEVERS.** It is
    the only one that cannot decay into wallpaper, needs no state that does not
    already exist, and is honest in both directions. The alternatives, and why
    each is worse here:

    * **A RATE.** It needs a previous reading kept across renders, i.e. new
      mutable state on the connection — and worse, it is a number that is ZERO
      almost all the time. A daemon warning fifty times a day would show
      `3,000 warnings · +0/min`, and the `+0` is the half he would read. A rate
      that is usually zero beside a total that is large is actively reassuring
      about a situation that is not fine.
    * **CLEAR / ACKNOWLEDGE.** Refused on principle: *"a count that silently
      resets is a lie about 27,705."* A button that hides the number is the log
      file with a nicer interface, and the producer deliberately exposes no way
      to zero the counter at all.
    * **ALWAYS ON, EVEN AT ZERO.** A permanent `⚠ 0 warnings` is a decoration,
      and the operator said so in the brief: it is the thing he would stop
      seeing by the second day.

    **`None` DRAWS NOTHING AND THAT IS NOT THE SAME AS ZERO — it is the one
    place this segment could lie and does not.** A daemon too old to carry the
    `warnings` shape has not told us it is healthy; it has told us nothing. Both
    render as absence here because the operator asked for silence when there is
    nothing to say, and the distinction is kept where it can be acted on:
    `Board.daemon_warnings` holds `None` rather than `0`, and it is the SHAPE
    that is obliged to export a `Known` zero
    (`tests/warnings_shape.rs::zero_exports_as_a_known_zero_rather_than_an_absent_cell`).
    The producer is honest; the face is quiet.

    **RED ON AN ERROR, AMBER OTHERWISE, AND NO INVENTED THRESHOLD.** The colour
    follows the severity of what was actually counted rather than a magnitude
    somebody picked: 199 of the live log's records are `ERROR`, and an error is a
    different statement from a warning. A `warnings > 500 → red` rule would be
    this module deciding how bad a number is, which is the producer's business
    and not the renderer's.
    """
    warn = board.daemon_warnings
    errors = board.daemon_errors or 0
    if warn is None and board.daemon_errors is None:
        return Text()
    total = (warn or 0) + errors
    if total == 0:
        return Text()
    style = RED if errors else AMBER
    text = Text()
    text.append("⚠ ", style=style)
    # Thousands separators, because `27705` and `2770` are the same shape at a
    # glance and the whole point is the magnitude.
    text.append(f"{total:,} ", style=f"bold {style}")
    text.append("warning" if total == 1 else "warnings", style=style)
    if errors:
        # Spelled out rather than folded into the total, because the producer
        # keeps them apart and a reader must be able to tell 47 warnings from
        # 47 errors.
        text.append(f" ({errors:,} error{'' if errors == 1 else 's'})", style=style)
    return text


def header_line1(board: Board, state: HeaderState, width: int, now: datetime) -> Text:
    """**`board` AND `now` ARE NOW UNUSED, AND KEPT DELIBERATELY RATHER THAN BY
    ACCIDENT.** Both existed for `freshness_segment`, which is deleted; everything
    this line still draws comes off `HeaderState`, which is the honest shape of it —
    line 1 is the CLIENT's report on itself.

    They stay because the two header functions are called side by side from one
    place and `header_line2` genuinely needs both, and because a `now` passed in
    rather than read is the purity contract this whole module is built on (see
    `threads_tui.drag`): a signature that still takes the clock cannot quietly grow
    a `now_utc()` call inside it. If a later change gives line 1 nothing
    board-shaped to say either, delete the pair then — do not re-derive a reason
    for them.
    """
    line = Text(" ")
    line.append("claude-accounts", style=f"bold {INK}")
    line.append("   ")
    line.append_text(running_segment(state))
    line.append("   ")

    # THE ORDER IS THE PRIORITY ORDER, because the narrow-terminal fallback
    # below truncates from the right. Reading left to right: is the daemon
    # there, are updates arriving, and when did this view last update.
    #
    # THERE IS NO THIRD SEGMENT ANY MORE. `freshness_segment` was here and is
    # deleted — see the note above it — so the tail the narrow fallback used to
    # drop no longer exists, and the fallback below is now unreachable in
    # practice and kept only because a terminal can be narrower than the link
    # state plus one age.
    right = Text()
    right.append_text(link_segment(state))
    right.append(" · ", style=MUTED)
    right.append_text(last_update_segment(state))

    # ── THE DAEMON'S WARNING COUNT ───────────────────────────────────────────
    #
    # Appended at the TAIL of the chain and also added to the narrow-terminal
    # fallback below, which is the one place in this function where a segment is
    # in both: everything else in the tail is a REMINDER that a small terminal
    # can afford to lose, and this is the opposite — the thing he must not miss
    # is not the thing to drop first.
    #
    # It is `Text()` and therefore invisible whenever there is nothing to say, so
    # on a healthy daemon this costs no columns at all and the separator is only
    # drawn when the segment is.
    warnings = warnings_segment(board)
    if warnings.cell_len:
        right.append(" · ", style=MUTED)
        right.append_text(warnings)

    # ── THE COUNTDOWN IS GONE, AND NOT BECAUSE IT WAS UGLY ───────────────────
    #
    # `next refresh 4:12` was an accurate description of a mechanism that no
    # longer exists. The board is re-read when the daemon ANNOUNCES a change,
    # not when a clock comes round (see `feed.py`), so a countdown to the next
    # re-read would be counting down to something that is not how the board
    # updates — and on a busy system it would be ticking while the board was
    # already being re-read several times a minute. A number that is both true
    # and irrelevant is worse than no number: it is the one the operator learns
    # to watch instead of the ones that matter.
    #
    # Leaving it would also have contradicted the point of the change on screen:
    # the operator asked to stop having to refresh, and a visible refresh clock
    # is the interface still telling him that refreshing is the mechanism.
    #
    # ── AND `R = poll accounts` IS GONE FROM THIS HEADER TOO ─────────────────
    #
    # The hint read `R = poll accounts`, or `R unavailable while offline`. Both
    # are deleted with the key, and the operator asked for exactly that
    # distinction: *"'R = poll accounts' should not just be retired and removed
    # from view, it should be the forcing function to prevent ever needing this."*
    #
    # So this is not a hint being tidied away while the key survives unadvertised.
    # The key is gone, the handler is gone, and `gauge/probe` is gone from the
    # client. What this header says about freshness — `read 4m ago`, or
    # `STALE — read 4m ago` — is now a report with NO ACCOMPANYING INSTRUCTION,
    # and that is the point: a stale board is a producer defect to be fixed at the
    # producer, not a prompt for the operator to press something. There is no key
    # to name here, and naming one again would be re-opening the hole.

    room = width - line.cell_len - 1
    if right.cell_len > room:
        # A terminal too narrow for both keeps the LINK STATE and drops the age,
        # and nothing load-bearing goes with it: `link_segment` already carries
        # the whole trust verdict on its own — `● offline` in red, or the split
        # `● live · ⚡ live updates stopped` — which is the same `healthy` test
        # `last_update_segment` splits on. The age is the magnitude of a problem
        # the surviving segment has already named.
        #
        # ── THE WARNING COUNT SURVIVES THE TRUNCATION, AND IT IS THE ONLY ─────
        #    SEGMENT THAT DOES
        #
        # Merge note, and both intents are kept. The `warnings` branch wrote this
        # fallback as link, then refresh age, then warnings, and gave the reason
        # the count is in both places: *"A warning count that disappears at 100
        # columns is a warning count the operator does not have."* That reason
        # stands and is honoured here.
        #
        # What could NOT come across with it is the middle segment. That branch
        # was written against a tail that still had the refresh clock in it, and
        # the board's corrections DELETED that function; `tests/test_feed.py`
        # asserts the NAME itself is gone — *"two names for one segment is how a
        # second one comes back"*. Taking that side verbatim would have called a
        # function that no longer exists, which is a `NameError` on every terminal
        # narrow enough to reach this branch.
        #
        # So the fallback is the board's shape — link state, no age — carrying the
        # warnings branch's invariant on top of it.
        right = Text()
        right.append_text(link_segment(state))
        if warnings.cell_len:
            right.append(" · ", style=MUTED)
            right.append_text(warnings)
    return _row(width, line, right)


def header_line2(board: Board, width: int, now: datetime) -> Text:
    """**THE POOL-WIDE ROOM VERDICT — and NO ACCOUNT'S GAUGE READING.**

    This line used to draw `ROOM  alice-example-com — 93% of weekly left`, and the
    operator asked why: *"Why am i seeing ROOM alice-example-com - 93% of weekly
    left? That's card specific information and it's not in a card. Also that
    account is disabled so it's likely inaccurate."*

    He is right on both counts and they are two separate defects. The second —
    that `Board.room()` never consulted `disabled` — is fixed in the model, with
    `any_gauged()` and `next_back()`, which had the identical omission. The first
    is fixed here.

    **WHAT THE ROOM SEGMENT MAY SAY NOW: only facts about the POOL.** Two of the
    three branches always were pool facts and are kept verbatim — there are no
    readings at all, or nothing can take work. The third was not: `<label> — 93%
    of weekly left` is one account's gauge, in a header, far from the card that
    account owns and far from the `WIPED`/`DISABLED` badges and the observation
    stamp that qualify it. A number separated from its qualifiers is a number
    that cannot be wrong in the reader's eyes, which is how it survived five days
    of being stale on a retired account.

    So in the good case this segment draws NOTHING — no label, no account, no
    percentage. `ROOM` now appears only when there is a pool-level problem, which
    makes its presence informative instead of constant. The reading itself is not
    lost: it moved to the card of the account it is about (`_room_line`), which is
    the half of his sentence that says where it belongs.

    `NEXT BACK` on the right is kept and still names an account, deliberately.
    It is not a gauge reading: it is a CLOCK on the pool's recovery, the account
    name is which door it comes through, and there is no card that could carry
    "soonest in the pool". It now excludes disabled accounts, which is what made
    it name `alice-example-com in 0m` off a login window that closed on 2 October.
    """
    line = Text(" ")
    room = board.room()
    if not board.any_gauged():
        line.append("ROOM  ", style=f"bold {MUTED}")
        line.append("no usage readings available", style=MUTED)
    elif room is None:
        line.append("ROOM  ", style=f"bold {AMBER}")
        line.append("none — all accounts at limit", style=AMBER)
    # else: there IS room, and which account holds it is card detail. Silence
    # here is the fix, not an omission — see `_room_line`.

    right = Text()
    back = board.next_back()
    if back is not None:
        account, axis = back
        right.append("NEXT BACK  ", style=f"bold {MUTED}")
        right.append(account.label, style=INK2)
        if axis.resets_at is not None:
            right.append(
                f" in {fmt_delta((axis.resets_at - now).total_seconds())}", style=f"bold {INK}"
            )
    else:
        right.append("NEXT BACK  ", style=f"bold {MUTED}")
        right.append("—", style=MUTED)
    if right.cell_len + line.cell_len + 4 > width:
        return _pad(line, width)
    return _row(width, line, right)


def _wrapped(sentence: str, width: int, *, indent: str = "  ", style: str = "") -> list[Text]:
    """Word-wrap one sentence to `width`, as a list of exactly-`width` lines.

    The board is drawn with `text-wrap: nowrap` and every line is sized to
    EXACTLY the width it is handed (see `_pad`), so a long line does not wrap —
    it is CROPPED, silently, mid-word. That is tolerable for a status note the
    operator can guess the rest of; it is not tolerable for the paragraph that
    explains why there is no board at all, which is the one thing on screen at
    that moment and the one he has to be able to read in full.
    """
    room = max(1, width - len(indent))
    words = sentence.split()
    lines: list[Text] = []
    current = ""
    for word in words:
        candidate = f"{current} {word}".strip()
        if len(candidate) > room and current:
            lines.append(_pad(Text(indent + current, style=style), width))
            current = word
        else:
            current = candidate
    if current:
        lines.append(_pad(Text(indent + current, style=style), width))
    return lines


def no_board_lines(board: Board, width: int) -> list[Text]:
    """**THERE ARE NO CARDS, AND THIS SAYS WHICH KIND OF NOTHING THAT IS.**

    THE RULE THIS OBEYS is the one the cells obey: un-hydrated is not empty,
    and neither of them is "there is no source". A blank board is the easiest
    lie this interface can tell, because it is indistinguishable from a healthy
    board with nothing on it — and after S3 there is one case where a blank
    board is exactly what a WRONG deployment produces, so it is the one case
    that must never pass for a quiet one.

    THE ABSENT CASE IS THE HONEST CONSEQUENCE OF S3, drawn rather than hidden.
    A daemon that answers `unknown method: shapes/list` holds no folds, and
    this board no longer has a second way to draw itself: the five account
    queries, the client-side running-thread lease and the per-thread placement
    lookup are gone. That is the price of one source and it is a price paid on
    purpose — while a second path existed, a disagreement between the board and
    the daemon could not be attributed to either. So the screen says the daemon
    has no fold, says what that costs, says the remedy is on the DAEMON side,
    and says the board turns on by itself when it lands. It is not a crash and
    it is not an empty pool.

    It is drawn in AMBER rather than RED: nothing has failed, and this is a
    version mismatch with a known end. The dead-fold banner — a fold that
    existed and died — is the red one (`Board.warnings`), because that one is
    terminal until the daemon restarts.
    """
    out: list[Text] = []
    if board.source == SOURCE_ABSENT:
        head = Text("  ")
        head.append("NO FOLD ON THIS DAEMON", style=f"bold {AMBER}")
        out.append(_pad(head, width))
        for sentence in (
            board.source_note
            or "This daemon does not expose shapes/list, so it holds no fold "
            "for the board to mirror.",
            "The board draws itself from the daemon's `accounts` fold and from "
            "nothing else. There is no fallback: the five account queries it "
            "used to join were deleted so that a wrong number is a bug in one "
            "named place rather than an unattributable disagreement.",
            "Nothing here is broken and nothing needs pressing. Promote a "
            "daemon that carries the fold and this board fills in by itself, "
            "on its next connection.",
        ):
            out.extend(_wrapped(sentence, width, indent="  ", style=INK2))
            out.append(_pad(Text(""), width))
        return out

    if board.source == SOURCE_WAITING:
        # NOT an empty pool, and the word "waiting" is what says so. A lazy
        # shape holds nothing until something subscribes and takes ~0.8s to
        # hydrate when it is cold, so this is the normal first second of every
        # connection — which is exactly why it must not be silent.
        line = Text("  ")
        line.append("◌ ", style=MUTED)
        line.append(
            board.source_note
            or "waiting for the daemon's first export of the accounts fold",
            style=MUTED,
        )
        return [_pad(line, width)]

    # SOURCE_HELD with no keys: the daemon exported and there is nothing in the
    # pool. An EMPTY Known is a fact and renders as one — no `?`, no spinner.
    line = Text("  ")
    line.append("the daemon's accounts fold holds no accounts", style=MUTED)
    return [_pad(line, width)]


def live_line(text: str, width: int) -> Text:
    """WHAT THE DAEMON JUST SAID — the live narration line.

    A fourth line style rather than a fourth `warning_line` severity, because it
    is not a verdict on anything: nothing is wrong, nothing succeeded, and the
    operator did not ask for it. It is the system talking while it works, which
    is the thing he said he wanted — *"If I have it open, that means I want to
    see live stuff happening in there as it's happening."*

    Drawn in muted ink with a `⚡` rather than a triangle or a tick, so it reads
    as ambient at a glance and cannot be confused with the `⚠`/`✓` lines that
    DO require a reaction. It ages out on its own (see `feed.Feed.note`).
    """
    line = Text(" ")
    line.append("⚡ ", style=INK2)
    line.append(text, style=MUTED)
    return _pad(line, width)


def warning_line(severity: str, text: str, width: int) -> Text:
    """One status line. THREE severities, and the third one is the fix.

    `ok` exists because a successful action he took was being drawn with a
    warning triangle in warning amber: setting a share reported
    `⚠ owner-gmail share set to 95%`. His words — "why is there a
    warning there? It doesn't make sense." He was right: nothing was wrong,
    and dressing a confirmation as a warning teaches him to stop reading the
    warnings that ARE warnings.

    **THE LOUDEST SEVERITY IS `critical`, AND IT WAS SPELLED `dead`.** Renamed
    with the two rendered strings in Correction 3 — see the severity block in
    `model.py`. A severity is how loudly to draw a line, never a claim about an
    account, and `dead` was a third meaning of a word already overloaded twice on
    this board. The glyph and the colour are unchanged.
    """
    line = Text(" ")
    if severity == CRITICAL:
        line.append("✕ ", style=f"bold {RED}")
        line.append(text, style=RED)
    elif severity == "ok":
        line.append("✓ ", style=f"bold {GREEN}")
        line.append(text, style=INK2)
    else:
        line.append("⚠ ", style=f"bold {AMBER}")
        line.append(text, style=AMBER)
    return _pad(line, width)


# ── the account card (DESIGN §3.3 / §3.4) ────────────────────────────────────


@dataclass
class CardExtras:
    """Per-account detail computed lazily for the selected card only.

    `projection` was called `pace`. The operator struck the word from this
    interface — see `_pressure_line` — so the field carrying the burn-rate
    sentence is named for what it is instead.
    """

    sparkline: list[float | None] | None = None
    projection: str | None = None
    loaded: bool = False
    # Did `gauge/history` CLIP the series it answered with? The daemon says so
    # per series and the clip takes the NEWEST rows (`ORDER BY id DESC LIMIT`,
    # reversed back to ascending), so an unreported truncation draws the LAST
    # part of the week and calls it the week. See `model.history_truncated`.
    truncated: bool = False
    # **THE SERIES' OWN CELL STATE WHEN IT IS NOT KNOWN** (R2.S3d inspect). The
    # producer EXPORTS a row-less, cell-less frame for a key it refused or could
    # not read, so "the key arrived" is not "the week is known". Set only for
    # that case; `None` for a known series. Rendered `?` / `✕` by `unknown_mark`,
    # never as "no observations", which is the KNOWN-and-empty answer.
    history_state: object | None = None


BADGE_SEPARATOR = " · "


# ── THE THREE-VALUED UNKNOWN (operator ruling) ───────────────────────────────
#
# A cell is `Known`, `NotHydrated` or `Dead`, and the three MUST NOT render the
# same. The distinction is not decoration:
#
#   Known, and empty   the daemon read it and there is nothing there. `no usage
#                      readings`, `login —`, no running badge at all. An empty
#                      answer is an ANSWER and it renders as empty.
#   NotHydrated        nothing has ever populated this cell — the hydrate
#                      handler errored, or answered no row for this key. `?`,
#                      muted. It may become a value at any moment.
#   Dead               the fold that maintained it is gone. Everything under it
#                      is the last thing the daemon managed to compute. `✕`,
#                      RED, and the card wears `✕ FOLD DIED` so a dead account
#                      can never be mistaken for a quiet one or an empty one.
#
# `account.cell(name)` answers None on the legacy five-query path, and None is
# NOT a fourth state: it means this board was not built from a shape, so every
# rendering below falls through to exactly what it did before.


def unknown_mark(state) -> Text | None:
    """The glyph for a cell that is not `Known`, or None when it is.

    Pure, and deliberately tiny: it is the one place the three states become
    three different marks, so a test can pin all three by calling it three
    times and a future edit cannot quietly collapse two of them.
    """
    if state is None or state.known:
        return None
    if state.dead:
        return Text("✕", style=f"bold {RED}")
    return Text("?", style=MUTED)


def _badges(account: Account, now: datetime, live_running: int | None = None) -> Text:
    """Fixed order: LIVE LOGIN, DISABLED, daemon activity, health badge.

    `live_running` is the PUSH-DRIVEN count of threads executing on this
    account (`live.LiveThreads.by_account`), or None when the live stream
    cannot answer — the link is down, no frame has arrived yet, or at least one
    running thread's placement is still unresolved.

    **IT WINS OVER `account.activity.executing` WHENEVER IT IS NOT NONE, AND
    THAT IS THE FIX HE ASKED FOR.** The polled number is read out of
    `accounts/status`, which is only re-read when the board reloads, so it is
    right at the instant of a reload and arbitrarily stale afterwards. It went
    UP within a second or two of a thread starting only by accident — a running
    turn streams token-usage lines, each of which broadcasts a `gauge` info
    frame, which schedules a reload — and it did not come DOWN at all, because
    the traffic that was incidentally refreshing it is the same traffic that
    stops when the thread stops. The board was live exactly while it did not
    matter and frozen at the transition he was watching for. This number moves
    on the frame.
    """
    parts: list[Text] = []

    # ── THE DEAD FOLD, FIRST AND LOUDEST ──
    #
    # Ahead of everything, because it is the first thing true about the card and
    # it changes the meaning of every number below it. The operator's rule: a
    # dead task must never look like an empty one or a quiet one. A card whose
    # fold has died is showing the last state the daemon managed to compute,
    # and the daemon REFUSES to re-subscribe until it restarts, so this does
    # not clear on its own and must not look like it might.
    if account.cells and any(state.dead for state in account.cells.values()):
        parts.append(Text("✕ FOLD DIED", style=f"bold {RED}"))

    if account.is_live_login:
        parts.append(Text("LIVE LOGIN", style=f"bold {INK}"))

    # NOT A POOL MEMBER. The key exists only because something non-idle is
    # running on it — a real state, and one the card must not draw as ordinary
    # membership. `registered` is True on the legacy path, where the question
    # cannot be asked.
    if not account.registered:
        parts.append(Text("NOT REGISTERED", style=MUTED))

    # ── DISABLED, AND IT IS NOT A HEALTH BADGE ──
    #
    # Placed here — ahead of activity and ahead of the health badge — because it
    # is the first thing true about the account: it is not in the pool. It is
    # deliberately NOT folded into `health_badge`, which renders in RED and
    # AMBER and means "the system found something wrong". Nothing is wrong with a
    # disabled account. It is healthy, it authenticates, it reports its gauges, and
    # it is out of placement because HE SAID SO.
    #
    # So it gets its own colour and its own word. A disabled account wearing a bar's badge
    # would tell him an account had failed when he had switched it off, and this
    # board already carries the case of him being told two accounts were dead
    # when they were not.
    #
    # THE GLYPH IS GONE WITH THE WORD (2026-09-11). `⏸` is the PAUSE glyph and it
    # says "temporarily, and it will resume" — which is the one thing this state
    # does not do. Nothing lifts it but him. The word carries the meaning and a
    # glyph that contradicts it is worse than no glyph.
    if account.disabled:
        parts.append(Text("DISABLED", style=f"bold {AMBER}"))

    act = account.activity
    threads = account.cell("threads")
    if threads is not None and threads.dead:
        # LAST KNOWN, and said so. Drawing the held number here without the
        # mark would be the one thing ruling 1 exists to stop: a mirror showing
        # a stale count as a current one.
        parts.append(Text(f"✕ running {act.executing} (last known)", style=f"bold {RED}"))
    elif threads is not None and not threads.known:
        # NOT the same as `running 0`. Nothing has populated this cell, so the
        # honest answer is that the board does not know — which is what the
        # header has always drawn for the total and what the card never could.
        parts.append(Text("? running", style=MUTED))
    elif act.known:
        if act.not_answering_since is not None:
            # ── `✕ dead 9m` WAS HERE, AND IT IS HALF OF CORRECTION 3 ─────────
            #
            # Four characters that told the operator an account was dead while
            # the card's health note told him a credential store was `DEAD`. Two
            # unrelated facts, one word, and he reasonably concluded the worse of
            # the two about accounts where neither was true — `dead_since` was
            # NULL on all four. The badge now says what the daemon actually
            # reported, in the SAME words as the board-level line
            # (`model.Board.warnings`), so the two can be read as one fact
            # instead of two.
            age = fmt_delta((now - act.not_answering_since).total_seconds())
            parts.append(Text(f"✕ not answering {age}", style=f"bold {RED}"))
        elif (live_running if live_running is not None else act.executing) > 0:
            running = live_running if live_running is not None else act.executing
            parts.append(Text(f"▶ running {running}", style=GREEN))
        # ── `● queued N` STOOD HERE AND IS GONE. TWO FAULTS, ONE LINE ─────────
        #
        # FAULT ONE, THE WORD. There was no `retrying` branch in this chain at
        # all, though `Activity.retrying` has always existed. Three threads were
        # in state `retrying`, so `queued_threads` was 0 for them — and the badge
        # still said `queued`, over work that was not queued.
        #
        # FAULT TWO, THE NUMBER: `max(act.queued_threads, act.queued_messages)`.
        # Those two counts are not the same KIND of thing. `queued_threads` is a
        # count of THREADS whose state is `queued`; `queued_messages` is a sum of
        # MESSAGES waiting inside rows regardless of their state. `max()` over
        # two differently-scoped counts does not reconcile them, it lets the
        # larger one silently win and then prints it under the smaller one's
        # label: `max(0, 5)` drew `queued 5` where no thread was queued at all.
        # A number whose unit depends on which input happened to be bigger is not
        # a number. There is no `max()` here now and there is no place to put one
        # back: the pool's queue depth is counted ONCE, in threads, in the header.
        #
        # FAULT THREE IS THE SCOPE, AND IT IS WHY NOTHING REPLACES IT IN KIND.
        # The operator: "why would a specific account say queued at all? threads
        # should have a queue for the pool not an individual account." He is
        # right. There is no per-account queue — the only queue is a table keyed
        # by `thread_id`, and accounts are placement targets, not owners of a
        # backlog. This board invented the per-account figure by bucketing
        # per-thread counts under each thread's LAST account, which answers
        # "where did this last run", not "where will it run next" — placement is
        # re-decided at admission. `queued 5` under one card therefore asserted a
        # commitment that does not exist. Queue depth is a POOL fact and it is
        # drawn once, pool-wide, in `running_segment`.
        #
        # WHAT A CARD MAY STILL SAY IS RETROSPECTIVE, and `retrying` is exactly
        # that: these threads HAVE run here, the attempt that failed was made on
        # THIS account, and the retry is of that attempt. It makes no claim about
        # where the next turn lands, so the retrospective bucketing is the right
        # reading of it rather than a misreading.
        elif act.retrying > 0:
            parts.append(Text(f"⟳ retrying {act.retrying}", style=AMBER))

    badge = account.health_badge
    if badge:
        color = AMBER if account.state == "no-login" else RED
        parts.append(Text(badge, style=f"bold {color}"))

    out = Text()
    for index, part in enumerate(parts):
        if index:
            out.append(BADGE_SEPARATOR, style=MUTED)
        out.append_text(part)
    return out


def _title_right(account: Account, warn_below: float) -> Text:
    """`login 12d` in the card title — or which kind of `?` it is.

    `fmt_days(None, ...)` renders `?` and has always meant two things at once:
    "the daemon read this slot and it has no expiry" and "nobody told us". With
    a cell state in hand those separate, and a third appears.
    """
    cred = account.cell("credential")
    if cred is not None and cred.dead:
        return Text("login ✕ last known", style=f"bold {RED}")
    if cred is not None and not cred.known:
        return Text("login ?", style=MUTED)
    days = fmt_days(account.days_left, account.state)
    if cred is not None and days == "?":
        # KNOWN AND EMPTY. The daemon answered for this slot and there is no
        # access expiry on it — a `no-login` slot, say. `—` rather than `?`,
        # because `?` is now reserved for the cell nobody has populated. Only
        # the `?` case is substituted: `n/a` (a clone's countdown belongs to
        # someone else's login) is already a specific answer and stays.
        return Text("login —", style=MUTED)
    if days == "EXPIRED":
        style = f"bold {RED}"
    elif account.days_left is not None and account.days_left <= warn_below:
        style = AMBER
    else:
        style = MUTED
    return Text(f"login {days}", style=style)


def _top_border(
    account: Account,
    width: int,
    selected: bool,
    warn_below: float,
    now: datetime,
    live_running: int | None = None,
) -> Text:
    fill_char = "━" if selected else "─"
    corner_l, corner_r = ("┏", "┓") if selected else ("┌", "┐")
    frame = INK if selected else MUTED

    title = Text()
    title.append(f"{account.number} · ", style=f"bold {INK}" if selected else INK2)
    title.append(account.label, style=f"bold {INK}" if selected else INK)

    right = _title_right(account, warn_below)
    badges = _badges(account, now, live_running)

    line = Text()
    line.append(corner_l, style=frame)
    line.append(" ")
    line.append_text(title)
    line.append(" ")

    # fill + " " + right + " " + corner
    used = line.cell_len + right.cell_len + 3
    remaining = width - used
    if remaining < 2:
        line = Text()
        line.append(corner_l, style=frame)
        line.append(" ")
        line.append(ellipsize(f"{account.number} · {account.label}", max(0, width - 4)),
                    style=f"bold {INK}")
        return _pad(line, width - 1).append(corner_r, style=frame)

    if badges.cell_len and remaining >= badges.cell_len + 4:
        pad = remaining - badges.cell_len - 2
        left_fill = max(1, pad // 2)
        right_fill = max(1, pad - left_fill)
        line.append(fill_char * left_fill, style=frame)
        line.append(" ")
        line.append_text(badges)
        line.append(" ")
        line.append(fill_char * right_fill, style=frame)
    else:
        line.append(fill_char * remaining, style=frame)

    line.append(" ")
    line.append_text(right)
    line.append(" ", style=frame)
    line = _pad(line, width - 1)
    line.append(corner_r, style=frame)
    return line


def _bottom_border(width: int, selected: bool) -> Text:
    if selected:
        return Text("┗" + "━" * (width - 2) + "┛", style=INK)
    return Text("└" + "─" * (width - 2) + "┘", style=MUTED)


def _wrap(inner_lines: list[Text], width: int, selected: bool) -> list[Text]:
    edge = "┃" if selected else "│"
    style = INK if selected else MUTED
    out = []
    for line in inner_lines:
        row = Text()
        row.append(edge, style=style)
        row.append_text(_pad(line, width - 2))
        row.append(edge, style=style)
        out.append(row)
    return out


def _axis_line(
    account: Account,
    axis_name: str,
    inner: int,
    mode: str,
    stamp: Text | None,
    dim: bool,
    flash: bool,
    now: datetime,
    back: bool = False,
) -> Text:
    axis = account.axes.get(axis_name)
    label = AXIS_LABEL.get(axis_name, axis_name)
    line = Text("  ")
    line.append(label.ljust(7), style=MUTED)
    line.append("  ")

    if axis is None or not axis.present:
        line.append("—", style=MUTED)
        return _row(inner, line, stamp)

    if not axis.calibrated:
        # THE RULE: never a fabricated percent before one real sample. An
        # uncalibrated axis is `calibrating`, over an empty track — never 0%.
        line.append("calibrating — no real sample yet", style=MUTED)
        return _row(inner, line, stamp)

    if axis.pct is None:
        line.append("no reading for this axis", style=MUTED)
        return _row(inner, line, stamp)

    width = BAR_WIDTH.get(mode, 25)
    line.append("▕", style=MUTED)
    line.append(bar(axis.pct, width), style=meter_color(axis.pct))
    line.append("▏", style=MUTED)

    if axis.at_limit:
        line.append(" ")
        if axis.resets_at is not None:
            delta = fmt_delta((axis.resets_at - now).total_seconds())
            line.append(f"AT LIMIT — back in {delta}", style=f"bold {INK}")
            if mode == FULL:
                line.append(f"  ({fmt_clock(axis.resets_at)})", style=MUTED)
        else:
            line.append("AT LIMIT — reset time unknown", style=f"bold {INK}")
    else:
        pct_style = MUTED if dim else INK
        if flash:
            pct_style = f"bold {INK} on {ATLIMIT}"
        line.append(fmt_pct(axis.pct, axis.low_confidence), style=pct_style)
        line.append("    ")
        if axis.resets_at is not None:
            delta = fmt_delta((axis.resets_at - now).total_seconds())
            line.append(f"resets in {delta}", style=INK2 if not dim else MUTED)
        else:
            line.append("reset time unknown", style=MUTED)

    if back:
        line.append("  BACK", style=f"bold {GREEN}")

    return _row(inner, line, stamp)


def _axis_stamp(axis, now: datetime) -> Text:
    """`obs 2m · ✓ 15 samples` for the expanded card."""
    text = Text()
    age = axis.age_seconds(now)
    if age is None:
        text.append("never observed", style=MUTED)
        return text
    color, _ = age_style(age)
    text.append(f"obs {fmt_age(age)}", style=color)
    text.append(" · ", style=MUTED)
    if not axis.calibrated:
        text.append("calibrating", style=MUTED)
    elif axis.low_confidence:
        text.append(f"~ conf {axis.confidence:.1f} · {axis.samples} samples", style=AMBER)
    elif axis.from_db:
        text.append("from the recorded log", style=MUTED)
    else:
        text.append(f"✓ {axis.samples} samples", style=MUTED)
    return text


SPARK_CHARS = "▁▂▃▄▅▆▇█"
# The UNOBSERVED slice. Off the bar scale on purpose — see `_sparkline`.
SPARK_GAP = "·"


def _sparkline(values: list[float | None]) -> Text:
    """One cell per bucket of the window, in order. `None` is an UNOBSERVED slice.

    `SPARK_GAP` is deliberately not `SPARK_CHARS[0]`. `▁` is the lowest BAR and
    scans as "almost nothing was used here", which is a reading the daemon did
    not take; `·` sits off the bar scale entirely and in `MUTED`, so a stretch of
    unpolled time reads as absence rather than as a trough. The alternative —
    dropping the cell — is what made the figure unaligned, because it moved every
    later cell leftward in time. See `model.history_sparkline`.
    """
    out = Text()
    for value in values:
        if value is None:
            out.append(SPARK_GAP, style=MUTED)
            continue
        index = min(7, max(0, int(round(value / 100.0 * 7))))
        style = ATLIMIT if value >= 100.0 else BLUE
        out.append(SPARK_CHARS[index], style=style)
    return out


def _axis_names(account: Account) -> list[str]:
    """The axes that have something real to say, in fixed order. An account
    with no readings at all yields none — the caller says so in one line.

    THE CASH AXIS IS OMITTED UNLESS IT HAS A REAL SAMPLE. The daemon reports
    `cash` for every account whether or not that account has extra-usage
    billing, and on an account without it every field comes back null — so the
    row rendered as `cash  calibrating — no real sample yet` forever, on every
    account, and could never become anything else.

    The operator's instruction: "if this is an account that's not based on
    cash, then we're not doing cash, so just say NA or don't even include it."
    Omitting is the stronger of the two options he offered, and it is the right
    one: a field that CANNOT populate is not missing data, it is an
    inapplicable field, and printing `NA` on every card forever is the same
    noise one word shorter.

    The percentage axes keep their `calibrating` line, because there it is
    true and temporary — that axis is warming up and will show a number.
    """
    names = [
        name
        for name in ("session", "weekly_all", "weekly_fable")
        if name in account.axes and account.axes[name].present
    ]
    cash = account.axes.get("cash")
    if cash is not None and cash.present and (cash.calibrated or cash.pct is not None):
        names.append("cash")
    return names


def card_lines(
    account: Account,
    width: int,
    *,
    selected: bool,
    mode: str,
    warn_below: float,
    extras: CardExtras | None,
    flashing: set[str] | None,
    now: datetime,
    back_axes: set[str] | None = None,
    live_running: int | None = None,
    room_leader: bool = False,
) -> list[Text]:
    """`room_leader` is True on the ONE account `Board.room()` picked.

    It is a BOARD fact handed to a card, which is the only way round this can
    work: the card can read its own weekly axis but it cannot see the other
    three, and "the most room in this pool" is a comparison. The percentage is
    NOT passed with it — `_room_line` derives it from this account's own axis, so
    there is exactly one subtraction and the card cannot be handed a number that
    disagrees with the meter drawn two rows above it.

    Defaulted False so every existing caller and test renders as before, and so
    a card drawn with no board in hand simply omits the line rather than guessing.
    """
    inner = width - 2
    flashing = flashing or set()
    back_axes = back_axes or set()

    # ── THE SIMPLE CARD, AHEAD OF EVERY OTHER BRANCH ────────────────────────
    #
    # *"Disabled just means we don't want to see any details. Just make the card
    # simple. We don't need limit data at all. Just its name and that it's
    # disabled."*
    #
    # Placed FIRST so there is exactly one way to draw a disabled account and no
    # mode, selection or axis state can route around it. The suppressed rows are
    # not blanked, they are not drawn: blanking would keep the vertical cost,
    # and the vertical cost is the whole complaint. This board is a stack he
    # scans to decide where work should go, and a disabled account is one he has
    # decided not to think about.
    #
    # WHAT SURVIVES, AND WHY EACH ONE. The title border keeps the number and the
    # label (it is still a card he selects with a digit and acts on with a key)
    # and it keeps `_badges`, which carries `LIVE LOGIN` — a fact about the
    # MACHINE, not about placement, and the only place this board says which
    # account plain `claude` is signed in as. `FOLD DIED` survives there too: a
    # card drawn from a corpse must say so whatever else is true of it.
    #
    # WHAT DOES NOT. Meters, the limit window, the observation stamp and the
    # pressure row. Every one of those measures an account nothing will be
    # placed on. They are not wrong, they are irrelevant, and irrelevant numbers
    # on a dense board cost more than they say.
    if account.disabled:
        return [
            _top_border(account, width, selected, warn_below, now, live_running),
            *_wrap([_disabled_line(account, inner, now)], width, selected),
            _bottom_border(width, selected),
        ]

    if mode == COMPACT:
        body = [_compact_line(account, inner, now), _pressure_line(account, inner, mode)]
        return [
            _top_border(account, width, selected, warn_below, now, live_running),
            *_wrap(body, width, selected),
            _bottom_border(width, selected),
        ]

    dim = account.is_stale_at(now)
    axis_names = _axis_names(account)

    body: list[Text] = []
    if not axis_names:
        # Three identical "no reading" rows train the eye to skip the card.
        # One honest line does the same job without pretending to be a gauge.
        #
        # AND IT SAYS WHY THERE ARE NONE. "the daemon has nothing recorded for
        # this account" and "nothing has ever populated this cell" and "the
        # fold that kept it is dead" are three different facts about an empty
        # meter, and the operator's remedy differs for each.
        body.append(_row(inner, _no_readings_line(account)))
        body.append(_pressure_line(account, inner, mode))
        if not selected:
            return [
                _top_border(account, width, selected, warn_below, now, live_running),
                *_wrap(body, width, selected),
                _bottom_border(width, selected),
            ]

    if selected:
        for name in axis_names:
            body.append(
                _axis_line(
                    account,
                    name,
                    inner,
                    mode,
                    _axis_stamp(account.axes[name], now) if mode == FULL else None,
                    dim,
                    name in flashing,
                    now,
                    name in back_axes,
                )
            )
    else:
        stamp = _card_stamp(account, now) if mode == FULL else None
        last = axis_names[-1] if axis_names else None
        for name in axis_names:
            body.append(
                _axis_line(
                    account,
                    name,
                    inner,
                    mode,
                    stamp if name == last else None,
                    dim,
                    name in flashing,
                    now,
                    name in back_axes,
                )
            )

    # Pressure sits ON the card, under the meters it is derived from, on EVERY
    # account and not only the selected one. It was previously reachable only
    # from the `p` tab; the operator asked to see it beside each login, which is
    # where he is already looking. The dedicated view survives — it ranks and
    # distributes, which a single row cannot — but the list now carries the
    # number, so the tab is a place to go deeper rather than the only place the
    # figure exists. (The no-axis branch above already added its own.)
    if axis_names:
        body.append(_pressure_line(account, inner, mode))

    # ── THE HEADROOM READING, ON THE CARD IT IS ABOUT ────────────────────────
    #
    # This is where `ROOM  alice-example-com — 93% of weekly left` went. Under the
    # weekly meter it is derived from, beside the badges and the observation stamp
    # that qualify it, on a card that is only drawn at all because this account is
    # not disabled. See `_room_line` and `header_line2`.
    #
    # LAST IN THE BODY, under pressure rather than above it, because it is the
    # weakest claim of the three: the meters are measurements, pressure is the
    # daemon's own ranking, and this is a comparison this client made.
    room_line = _room_line(account, inner) if room_leader else None
    if room_line is not None:
        body.append(room_line)

    if selected:
        body.append(Text("  " + "─" * max(0, inner - 4), style=MUTED))
        body.extend(_expansion(account, inner, extras, now))

    return [
        _top_border(account, width, selected, warn_below, now, live_running),
        *_wrap(body, width, selected),
        _bottom_border(width, selected),
    ]


def _room_line(account: Account, inner: int) -> Text | None:
    """**`most room in the pool — 93% of weekly left`, ON THE CARD. PURE.**

    The reading the operator found in the header: *"That's card specific
    information and it's not in a card."* So it is in the card.

    **WHY IT IS NOT REDUNDANT WITH THE WEEKLY METER TWO ROWS UP.** That row says
    `weekly  ███░░░  7%` and `100 − 7` is the same number, so a line that only
    said `93% of weekly left` would be a second grammar for a figure already on
    screen — the duplicated-signal defect this file keeps deleting. What this
    line adds is the part no single card can know: that this is the MOST room in
    the pool. The percentage travels with the comparison because a comparison
    with no magnitude ("most room") is unactionable when the most is 4%.

    **WHY IT CANNOT APPEAR ON A DISABLED CARD, STRUCTURALLY RATHER THAN BY A
    SECOND CHECK.** Two independent reasons, and either alone is enough:
    `Board.room()` skips disabled accounts, so `room_leader` is never True for
    one; and `card_lines` returns the simple card before it reaches this, so no
    body line of any kind is drawn. There is deliberately no `account.disabled`
    test in here — a third check would be a third thing to forget.

    Returns None when the account has no usable weekly reading. `room()` only
    picks a gauged account with a known `weekly_all`, so that is unreachable
    through the normal path; it is here so the line can never be invented out of
    a missing number if a future caller passes the flag on its own authority.
    """
    weekly = account.axes.get("weekly_all")
    if weekly is None or not weekly.known:
        return None
    left = 100.0 - weekly.pct
    line = Text("  ")
    line.append("most room in the pool", style=f"bold {GREEN}")
    line.append(f" — {left:.0f}% of weekly left", style=INK2)
    return _row(inner, line)


def _disabled_line(account: Account, inner: int, now: datetime) -> Text:
    """The one line a disabled card has. PURE, and `now` is an argument.

    Three things, in the order he needs them: the state, HIS OWN WORDS for why,
    and the key that undoes it.

    The note is the only fact on this card that is not derivable from anywhere
    else — the daemon stores it in `account_disabled.note` because he typed it —
    so dropping it in the name of simplicity would throw away the one thing the
    simple card exists to carry. When there is no note and no date the sentence
    simply gets shorter; it never grows an empty quote or a dangling `since`.

    `press d to reactivate` is here rather than on the pressure row because the
    pressure row is gone. It is the same promise `UNRANKED_REASON["disabled"]`
    makes in the pressure VIEW, worded identically on purpose.
    """
    # THE HINT GOES ON THE RIGHT, and that is a decision about what gets cropped.
    # Every line on this board is sized to exactly the width it is handed, so
    # something has to give on a narrow card — and `account_disabled.note` is
    # free text the operator typed, of no bounded length. With the hint at the
    # end of one long line, a wordy note evicts the only actionable thing on the
    # card. In the right-hand column it is the NOTE that shortens, with an `…`
    # saying so, and the key he needs is the one part that cannot be pushed off.
    hint = Text("press d to reactivate", style=MUTED)

    line = Text("  ")
    line.append("DISABLED", style=f"bold {AMBER}")
    line.append(" — out of the pool", style=MUTED)
    # What is left for everything after the state, once the hint has its column.
    def room() -> int:
        return inner - hint.cell_len - 1 - line.cell_len

    if account.disabled_at is not None:
        # AN AGE, NOT A WALL CLOCK. `Fri 09:15` is unambiguous for a few hours
        # and then silently wrong — an account switched off three weeks ago
        # reads exactly like one switched off yesterday. The subtraction is
        # against the `now` this function is handed, which is the same rule the
        # observation stamps follow.
        age = fmt_delta((now - account.disabled_at).total_seconds())
        # ALL OF IT OR NONE OF IT. `_row` crops, and a cropped age is a WRONG
        # age rather than a short one — "2d 00h" with the " ago" shaved off
        # still reads as a duration, and "2d 0" reads as a different one.
        if room() >= len(age) + 6:
            line.append(", ", style=MUTED)
            line.append(age, style=INK2)
            line.append(" ago", style=MUTED)
    if account.disabled_note:
        space = room() - 3
        if space >= 4:
            line.append(" · ", style=MUTED)
            line.append(ellipsize(account.disabled_note, space), style=INK2)
    return _row(inner, line, hint)


def _no_readings_line(account: Account) -> Text:
    """Why this card has no meters — one of three answers, never one of two."""
    gauge = account.cell("gauge")
    if gauge is not None and gauge.dead:
        return Text(
            "  ✕ usage is LAST KNOWN — the fold that maintained it died",
            style=f"bold {RED}",
        )
    if gauge is not None and not gauge.known:
        reason = gauge.reason or "nothing has populated this cell"
        return Text(f"  ? usage not hydrated — {reason}", style=MUTED)
    return Text("  no usage readings for this account", style=MUTED)


def _pressure_line(account: Account, inner: int, mode: str) -> Text:
    """The account's pressure, on the account's own row.

    Pressure is `unused_of_share / fraction_of_window_remaining`: 1.0 means
    level with its share, above 1 means it has room it must burn to spend that
    share, below 1 is held in reserve, and negative means it is already past
    its share. THE HIGHEST PRESSURE TAKES THE NEXT TURN — that is what the
    number is for, and it is why the operator struck the word "pace" from
    this interface in favour of the word the ranker actually uses.

    THE SHARE TRAVELS WITH IT, always. That is not decoration. Pressure is a
    ratio against a denominator the operator sets per account, so "1.9x" on an
    account lent 10% of itself and "1.9x" on one lent 100% describe completely
    different amounts of actual headroom. Shown alone, a high number reads as
    "lots of room" on exactly the account that has almost none.

    An account with no reading renders as the WORD `unranked` with an empty
    track and a reason — never as a number, never as a short bar. A short bar
    would sort and scan as "nearly out", which is a claim the daemon did not
    make.
    """
    bar_w = pressure.CARD_BAR.get(mode, 0)
    # Never let the bar crowd out the number and the share, which are the parts
    # that carry meaning without needing to be measured against anything.
    # 50, not 46: the row label grew from `pace  ` to `pressure  `, four cells.
    # The reserve and the label have to move together or the bar reclaims the
    # difference and the right-hand columns stop lining up down the stack.
    bar_w = min(bar_w, max(0, inner - 50))
    if bar_w and bar_w % 2 == 0:
        bar_w -= 1  # odd width gives a true centre cell
    left, right = pressure.account_cell(
        account.pressure,
        account.pool_share,
        in_pool=account.in_pressure_pool,
        reason=account.pressure_reason,
        bar_w=bar_w,
    )
    return _row(inner, left, right)


def _card_stamp(account: Account, now: datetime) -> Text:
    age = account.reading_age(now)
    if age is None:
        return Text("no readings", style=MUTED)
    color, stale = age_style(age)
    cause = account.unprobeable_cause
    if stale and cause:
        # The probe cannot run, so this reading will never refresh by itself.
        # Name the cause; keep the age visible. Short: it shares the last axis
        # line with the meter.
        return Text(
            f"FROZEN — {cause} · {fmt_age(age)}",
            style=f"bold {color}",
        )
    if stale:
        return Text(f"STALE {fmt_age(age)}", style=f"bold {color}")
    return Text(f"obs {fmt_age(age)} ago", style=color)


def _expansion(
    account: Account, inner: int, extras: CardExtras | None, now: datetime
) -> list[Text]:
    lines: list[Text] = []

    # 2. sparkline + burn-rate projection
    #
    # `reading history…` is a LOADING state, and it is only honest while
    # something is actually loading. It used to be permanent: nothing ever
    # called `gauge/history`, `extras` was hard-coded to None at the call site,
    # so every account on the board said "reading history…" forever. It read as
    # a fact about the account rather than a fact about the fetch. It now
    # appears only between selecting a card and its history arriving.
    spark = Text("  ")
    if extras is None or not extras.loaded:
        spark.append("weekly 7d  ", style=MUTED)
        spark.append("reading history…", style=MUTED)
    elif extras.history_state is not None and unknown_mark(extras.history_state) is not None:
        # The key was exported and its series is NOT KNOWN — refused, or a
        # failed read. The three-valued unknown's mark, never an empty week.
        spark.append("weekly 7d  ", style=MUTED)
        spark.append_text(unknown_mark(extras.history_state))
        spark.append(" no history", style=MUTED)
    elif not extras.sparkline:
        spark.append("weekly 7d  ", style=MUTED)
        spark.append("no observations in the last 7 days", style=MUTED)
    else:
        # THE HEADING IS A CLAIM ABOUT THE X-AXIS, so it changes when the axis
        # does. `weekly 7d` beside a clipped series said the figure covered the
        # week; it covers whatever the newest 1,000 observations reached, which
        # on a busy account is its last ~33 hours. The clipped figure is still
        # shown — it is real data — and it is now labelled as partial instead of
        # whole, which is the difference between a short answer and a wrong one.
        if extras.truncated:
            spark.append("weekly 7d  ", style=AMBER)
        else:
            spark.append("weekly 7d  ", style=MUTED)
        spark.append_text(_sparkline(extras.sparkline))
        if extras.truncated:
            spark.append(" ▸ newest only; the older end was clipped", style=AMBER)
    right = (
        Text(extras.projection, style=INK2)
        if (extras and extras.projection)
        else None
    )
    lines.append(_row(inner, spark, right))

    # 3. identity
    identity = Text("  ")
    identity.append("identity ", style=MUTED)
    identity.append(account.identity_acct or "—", style=INK2)
    if account.subscription_type:
        identity.append(" · tier ", style=MUTED)
        identity.append(account.subscription_type, style=INK2)
    if account.hint:
        identity.append(" · signs in as ", style=MUTED)
        identity.append(account.hint, style=INK2)
    lines.append(_row(inner, identity))

    # THE KEYCHAIN LINE IS GONE, on the operator's instruction: "take out the
    # keychain, claude code, credentials, secret, number line — don't want to
    # see it." It printed the macOS keychain service name and eight bytes of a
    # credential hash. Neither is a decision he makes from this board, and the
    # row cost a line on every expanded card. `Account.keychain_service` and
    # `Account.secret_sha8` are still carried on the model — they come free in
    # the `accounts/health` payload — but nothing renders them.

    # 4. the live account's second, independent grant
    if account.is_live_login and not account.is_machine_default:
        store_line = Text("  ")
        if account.live_store_state == "live":
            days = fmt_days(account.live_store_days_left, account.live_store_state or "")
            store_line.append(f"machine store: {days} left", style=INK2)
            store_line.append(" · this is what plain `claude` signs in as", style=MUTED)
        else:
            store_line.append(
                "this is what plain `claude` signs in as — but the machine store itself "
                f"reads {account.live_store_state!r}; press s to sign it in again",
                style=AMBER,
            )
        lines.append(_row(inner, store_line))
    elif account.is_live_login and account.is_machine_default:
        lines.append(
            _row(
                inner,
                Text(
                    "  this is your machine login, and no named account carries it yet — "
                    "press n to give it a slot",
                    style=AMBER,
                ),
            )
        )

    # 5 / 6. daemon detail, or the verbatim state explanation when unhealthy
    note = account.health_note
    if note:
        color = AMBER if account.state == "no-login" else RED
        lines.append(_row(inner, Text("  " + note, style=color)))
        if account.state == "wiped":
            # H179: what to do, on its own row so a narrow card cannot clip it
            # off the end of the note. The card is selected, so the key is L.
            lines.append(
                _row(
                    inner,
                    Text(
                        f"  sign this slot in: press L, or run {account.sign_in_command}",
                        style=color,
                    ),
                )
            )
    else:
        act = account.activity
        detail = Text("  ")
        if not act.known:
            detail.append("daemon: offline — activity unknown", style=MUTED)
        else:
            detail.append("daemon: ", style=MUTED)
            if act.executing:
                shown = ", ".join(t[:8] for t in act.executing_thread_ids[:3])
                more = "" if len(act.executing_thread_ids) <= 3 else f" +{len(act.executing_thread_ids) - 3}"
                detail.append(f"executing {shown}{more}", style=INK2)
            else:
                detail.append("idle", style=MUTED)
            # ── `· 0 queued` STOOD HERE AND A CARD CARRIES NO QUEUE COUNT ────
            #
            # THE CARD USED TO CONTRADICT ITSELF. The badge three rows above said
            # `queued 5` and this line said `0 queued` — two numbers for one word
            # on one card, because the badge was summing MESSAGES and this line
            # was counting THREADS. Making them agree on a number would have been
            # the wrong fix; they were answering different questions.
            #
            # BOTH QUESTIONS ARE ANSWERED ELSEWHERE OR NOT AT ALL. "How deep is
            # the queue" is a POOL question and is drawn once, in the header. "How
            # many threads last placed here are queued" is a question a card may
            # not ask: the operator was asked directly whether a per-card count
            # could be worded honestly — retrospectively, as `(last here)` — and
            # the answer was a flat no. There is no per-account queue to describe,
            # so no wording makes a per-card queue figure true; a reader seeing a
            # number beside an account's name reads a commitment that the account
            # will run that work, and placement is re-decided at admission.
            #
            # WHAT A CARD MAY STILL SAY is `retrying`, below, and it is kept for
            # the reason the queue counts are gone: it is a fact about the STATE of
            # those threads, not a claim about where the next turn lands.
            # ── `· N parked` STOOD HERE AND A CARD CARRIES NO PARKED COUNT ────
            #
            # H179: the board read "3 parked on alice-example-com". Parking is a
            # POOL state — a parked turn is re-placed onto whichever account
            # admission picks — so the count is drawn once, in the header, for
            # the same reason the queue depth is. The thread's last account is
            # still on the daemon's `pool_parked` list as attribution.
            if act.retrying:
                detail.append(f" · {act.retrying} retrying", style=AMBER)
            detail.append(
                " · healthy" if act.healthy else " · UNHEALTHY",
                style=MUTED if act.healthy else RED,
            )
            if act.limit_until is not None:
                detail.append(
                    f" · rate-limited until {fmt_clock(act.limit_until)}", style=AMBER
                )
        lines.append(_row(inner, detail))

    lines.extend(_shape_lines(account))
    return lines


def _shape_lines(account: Account) -> list[Text]:
    """The two facts only the shape carries. Empty on the legacy path.

    MOVES, AND WHY THEY ARE QUALIFIED. `moves` is a windowed `Sequence`, not a
    ledger: it holds what the fold has seen since it hydrated, bounded by 32
    items and 24 hours, and the cell carries `covers_since` — the oldest stamp
    it still retains. So the line says "2 moved off since 14:02" and never
    "2 failovers", because an unqualified count off a windowed cell is a claim
    about all time that the cell cannot support. The word is `moved`, not
    `failover`, for the same reason the daemon's cell is: `AccountSwitched`
    carries a reason and the count includes deliberate placements too.

    THE MACHINE DEFAULT IS A NOTE, NOT A CARD (ruling 5). It lands on the
    account it folds into, which is what stops the machine store and the pool
    login it is signed in as being drawn as two cards with one identity.
    """
    out: list[Text] = []

    cell = account.cell("moves")
    if cell is not None and account.moves:
        off = sum(1 for m in account.moves if m.get("direction") == "off")
        on = sum(1 for m in account.moves if m.get("direction") == "on")
        line = Text("  ")
        line.append("placement  ", style=MUTED)
        line.append(f"{off} moved off · {on} moved on", style=INK2)
        since = cell.covers_since
        if since is not None:
            line.append(f" · since {fmt_clock(since)}", style=MUTED)
        out.append(line)

    md = account.machine_default
    if md:
        line = Text("  ")
        line.append("machine default  ", style=MUTED)
        line.append(str(md.get("label") or "default"), style=INK2)
        line.append(" folds into this account · ", style=MUTED)
        line.append(str(md.get("state") or "?"), style=INK2)
        out.append(line)

    return out


def _compact_line(account: Account, inner: int, now: datetime) -> Text:
    """Two-line card: title border plus the single worst axis."""
    worst_name, worst = None, None
    for name, axis in account.axes.items():
        if axis.pct is None or not axis.calibrated:
            continue
        if worst is None or axis.pct > worst.pct:
            worst_name, worst = name, axis
    line = Text("  ")
    if worst is None:
        line.append("no calibrated reading", style=MUTED)
        return line
    line.append(AXIS_LABEL.get(worst_name, worst_name).ljust(7), style=MUTED)
    line.append(" ")
    line.append(bar(worst.pct, BAR_WIDTH[COMPACT]), style=meter_color(worst.pct))
    line.append(" ")
    if worst.at_limit and worst.resets_at is not None:
        line.append(
            f"AT LIMIT {fmt_delta((worst.resets_at - now).total_seconds())}",
            style=f"bold {INK}",
        )
    else:
        line.append(fmt_pct(worst.pct, worst.low_confidence), style=INK)
    return line


def card_height(account: Account, *, selected: bool, mode: str, room_leader: bool = False) -> int:
    # The simple card is two borders and one sentence, in every mode and whether
    # or not it is selected. Answered ahead of the COMPACT check because it is
    # not a mode, it is a state.
    if account.disabled:
        return 3
    if mode == COMPACT:
        # COMPACT DOES NOT CARRY THE ROOM LINE, so `room_leader` changes nothing
        # here. Compact is the density mode — one worst-axis line — and
        # `card_lines` does not append the room row in this branch either.
        #
        # FOUR, NOT THREE. Two borders, the worst-axis line AND the pressure row,
        # which `card_lines` appends in this branch too. This said 3 for as long
        # as the pressure row has existed, for the same reason the full-card
        # arithmetic below was one short: nothing calls this function and the one
        # test on it exercised the disabled card. See the note below.
        return 4
    axes = max(len(_axis_names(account)), 1)
    # ── TWO BODY ROWS THIS FUNCTION USED TO FORGET ───────────────────────────
    #
    # `pressure` was never counted. `card_lines` has appended a `_pressure_line`
    # to EVERY non-compact card since the pressure row moved onto the cards, and
    # this arithmetic was never updated, so it has been one short for every
    # enabled account in both selection states. It went unnoticed because the
    # only assertion on this function (`tests/test_disable.py`) exercises the
    # DISABLED card, which returns above, and because nothing in `app.py` calls
    # it — the board is laid out from `len(card_lines(...))` itself.
    #
    # It is fixed rather than left because the `room` row below is counted here,
    # and adding a correct term to a wrong total is how a wrong total survives.
    # `tests/test_disable.py` now pins the agreement across every combination.
    room = 1 if (room_leader and _room_line(account, 80) is not None) else 0
    if not selected:
        return axes + 1 + room + 2  # + pressure + borders
    expansion = 2  # sparkline, identity  (the keychain row was removed)
    if account.is_live_login:
        expansion += 1
    expansion += 1  # daemon detail / state explanation
    return axes + 1 + room + 1 + expansion + 2  # + pressure + separator + borders
