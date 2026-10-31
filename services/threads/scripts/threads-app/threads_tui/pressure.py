"""The pressure view. PURE: rows in, Text lines out.

NOTHING IN THIS MODULE COMPUTES PRESSURE. The number arrives from
`accounts/pressure` and is rendered. The daemon's scheduler is the only thing
entitled to an opinion about what pressure is; a second implementation here
would be a second answer, and the two would diverge silently on the day it
mattered. See `threads_tui.drag`, obligation 2.

WHY THE VIEW LOOKS LIKE THIS
----------------------------
The ask was for "a relative distribution", with the honest caveat that the
normalisation was an open question. Taken literally, share-of-total has two
failure modes that make it useless as the ONLY view:

  * Ten balanced accounts each read 10%. A perfectly healthy fleet renders as
    a flat row of identical bars — indistinguishable from a fleet where
    nothing is happening at all.
  * A fleet-wide crisis renders IDENTICALLY to a calm day. If every account is
    equally starved, each is still 10% of the total. The one condition you
    most need to see is the one a purely relative view cannot show.

So the view is two bands, and the order matters:

  BAND 1 — a DIVERGING BAR ON A LOG SCALE, ANCHORED AT 1.0×.
    Absolute, not relative. Pressure 1.0× sits at the centre line. 0.5×
    sits a quarter of the width to the left, 2× a quarter to the
    right; the bar saturates at 0.25× and 4×. Log, because
    pressure is a RATIO — 2x and 0.5x are the same size of deviation in
    opposite directions, and only a log scale draws them that way. Anchored,
    because the question "is this account ahead or behind?" has an absolute
    answer and direction should be readable without reading a number.

    THE WORD "PACE" IS NOT USED IN THIS INTERFACE. The operator struck it:
    the number decides placement, and it is called pressure. Everything the
    scale used to describe in units of pace is described in units of
    pressure — `¼×`, `1.0×`, `4×` — which is the same scale said plainly.

  BAND 2 — the RELATIVE STRIP that was asked for.
    Share of total pressure across ranked accounts. This answers exactly one
    question, and it is a good question: WHERE WOULD THE NEXT TURN GO. It sits
    underneath band 1 rather than replacing it, so it is read in the context of
    whether the fleet as a whole is healthy.

POOL SHARE IS SHOWN BESIDE EVERY ACCOUNT, because pressure is a fraction OF A
SHARE. Without the denominator, "high pressure" on an account with almost no
share reads as "lots of room" when it means "lots of room out of very little".

UNRANKED ACCOUNTS RENDER AS THE WORD `unranked`, never as a short bar. A short
bar is a claim that the value is low. Absence of a value is not a low value,
and the two must not look alike.
"""

from __future__ import annotations

import math
from dataclasses import dataclass

from rich.text import Text

from .fmt import AMBER, ATLIMIT, BLUE, INK, INK2, MUTED, RED, ellipsize

# The scale. `LOG2_SATURATION` is where the bar hits its end stop: 2 means a
# factor of four either way (2^2). With 0.5× at log2 = -1 landing at half
# the half-width, that is "a quarter of the total width left of centre" exactly
# as specified.
LOG2_SATURATION = 2.0

# `ON_PACE = 1.0` stood here, unreferenced by anything. It went out with the
# word: the anchor is 1.0 by definition of the ratio, and a named constant for
# it bought nothing but a second place to say "pace".

TRACK = "·"
FILL = "█"
CENTRE = "│"
OVERFLOW_LEFT = "◀"
OVERFLOW_RIGHT = "▶"


@dataclass(frozen=True)
class PressureRow:
    """One account's pressure, exactly as the API reported it.

    `pressure is None` means the daemon declined to rank this account — the
    `Slack::Unknown` case: no recent observation, or a reset window it cannot
    place. That is a DIFFERENT state from a low pressure and is rendered
    differently.
    """

    number: int
    label: str
    pressure: float | None
    share: float | None = None  # pool share; None = the API did not say
    reason: str = ""  # why unranked, when the API explains
    selected: bool = False
    # Whether the daemon listed this account in the pressure payload at all.
    # THREE states, not two: ranked; listed but unranked (a reason is given);
    # and not listed, which means the account is not in the pool and there is
    # no pressure for it to have. The third used to render identically to the
    # second, with no explanation — "not measured" wearing the clothes of
    # "measured, and declined to rank".
    in_pool: bool = True


# The daemon's `unranked_reason` vocabulary, in English. Anything unrecognised
# is passed through verbatim rather than swallowed: a reason we do not have a
# translation for is still more useful than no reason.
UNRANKED_REASON = {
    "no_reading": "no usage reading yet",
    "no_reset_window": "no reset window to measure against",
    "window_rolled": "the weekly window rolled; waiting on a fresh reading",
    "read_failed": "the usage read failed",
    "not_finite": "the reading was not a finite number",
    # THE ONE REASON ON THIS LIST THAT IS NOT A MEASUREMENT PROBLEM. Every other
    # entry says "we tried and could not"; this says "we did not try, because
    # you told us not to". It must never read like a fault — a disabled account is
    # healthy, and the only thing that changes it is him.
    "disabled": "DISABLED by you — out of placement selection; press d to reactivate",
    # The pre-rename token, for the window in which this board is newer than the
    # daemon it is talking to. Same sentence, because it is the same state — the
    # alternative is showing him the raw wire word, which explains nothing.
    "paused": "DISABLED by you — out of placement selection; press d to reactivate",
    # THE THIRD KIND OF UNRANKED, and the newest. Not a measurement problem and
    # not his decision: the account is at its limit, so the daemon measured it
    # perfectly well and then threw the number away. It had to — an account at
    # 100% with minutes left in its window produces a pressure like -157x, and
    # one number that size flattens every other account on a shared scale. The
    # operator saw -49.63x and said so.
    #
    # THE SENTENCE SAYS THE WAIT ENDS BY ITSELF. That is the entire difference
    # from `disabled`, which sits two lines up and ends only when he acts, and
    # it is why the two must never share wording: one is "press d", the other is
    # "do nothing".
    "unavailable": "AT ITS LIMIT — off the scale until its window resets",
}

NOT_IN_POOL = "not in the pool"


def unranked_note(reason: str, *, in_pool: bool = True) -> str:
    """Why there is no number. Never blank when we can say anything at all."""
    if not in_pool:
        return NOT_IN_POOL
    if not reason:
        return "the daemon did not say why"
    return UNRANKED_REASON.get(reason, reason)


def log_offset(pressure: float, half_width: int) -> int:
    """Cells left (negative) or right (positive) of centre, on the log scale.

    Anchored so that log2(pressure) == -1 (0.5x) lands at half the
    half-width — a quarter of the full bar — and saturates at LOG2_SATURATION.

    `pressure <= 0` has no logarithm. It is also a real, meaningful state: the
    daemon leaves pressure unclamped precisely so that an account which has
    burned MORE than its share reads as negative. It pins to the end stop.
    """
    if half_width <= 0:
        return 0
    if pressure <= 0:
        return -half_width
    ratio = math.log2(pressure) / LOG2_SATURATION
    ratio = max(-1.0, min(1.0, ratio))
    return int(round(ratio * half_width))


def diverging_bar(pressure: float | None, width: int) -> Text:
    """Band 1. An absolute, log-scaled, centre-anchored bar."""
    if width < 5:
        return Text(" " * max(0, width))
    centre = width // 2
    half = min(centre, width - centre - 1)

    if pressure is None:
        # Unranked: the track and the centre line, and nothing else. No fill of
        # any length, because every length is a claim.
        cells = [Text(TRACK, style=MUTED) for _ in range(width)]
        cells[centre] = Text(CENTRE, style=MUTED)
        out = Text()
        for c in cells:
            out.append_text(c)
        return out

    offset = log_offset(pressure, half)
    saturated = pressure > 0 and abs(math.log2(pressure)) > LOG2_SATURATION

    if pressure <= 0:
        style = ATLIMIT  # over its share outright
    elif pressure >= 1.0:
        style = BLUE  # room relative to its share
    elif pressure >= 0.75:
        style = AMBER  # slightly ahead of its share
    else:
        style = RED  # burning well ahead of its share

    out = Text()
    for i in range(width):
        if i == centre:
            out.append(CENTRE, style=INK2)
            continue
        inside = (offset > 0 and centre < i <= centre + offset) or (
            offset < 0 and centre + offset <= i < centre
        )
        if inside:
            at_end = (offset > 0 and i == centre + offset) or (
                offset < 0 and i == centre + offset
            )
            if saturated and at_end:
                out.append(OVERFLOW_RIGHT if offset > 0 else OVERFLOW_LEFT, style=style)
            else:
                out.append(FILL, style=style)
        else:
            out.append(TRACK, style=MUTED)
    return out


def relative_shares(rows: list[PressureRow]) -> dict[str, float | None]:
    """Band 2's numbers: share of total pressure among RANKED accounts.

    This is a normalisation of values the API produced — it is not a
    computation of pressure. Unranked accounts get None and are excluded from
    the denominator, because including them would require inventing a value
    for them.

    Only POSITIVE pressure contributes. An account that is over its share does
    not compete for the next turn, so giving it a slice of "where the next turn
    goes" would be false. It is shown as 0% of the next turn, which is what it
    is.
    """
    total = sum(r.pressure for r in rows if r.pressure is not None and r.pressure > 0)
    out: dict[str, float | None] = {}
    for r in rows:
        if r.pressure is None:
            out[r.label] = None
        elif total <= 0:
            out[r.label] = 0.0
        else:
            out[r.label] = max(0.0, r.pressure) / total * 100.0
    return out


def strip_bar(pct: float | None, width: int) -> Text:
    """Band 2's bar: a plain proportional meter, 0..100% of the next turn."""
    if width <= 0:
        return Text("")
    if pct is None:
        return Text("─" * width, style=MUTED)
    filled = int(round(max(0.0, min(100.0, pct)) / 100.0 * width))
    out = Text()
    out.append("▬" * filled, style=INK2)
    out.append("─" * (width - filled), style=MUTED)
    return out


def fmt_pressure(pressure: float | None) -> str:
    if pressure is None:
        return "unranked"
    return f"{pressure:.2f}×"


def fmt_share(share: float | None) -> str:
    """The denominator. `?` when the API did not say — never assumed equal."""
    if share is None:
        return "  ?"
    return f"{share * 100:>3.0f}%"


def scale_legend(width: int) -> Text:
    """The axis caption. Without it the centre line is just a character.

    It is exactly `width` cells, matching the bar it captions. It used to be
    `width + 3`: the padding was sized as `width - 8` while the fixed glyphs
    cost eleven cells (`¼×` 2 + the centre word 7 + `4×` 2). Three cells of
    overrun pushed the `pressure` and `share` headings out of line with the
    numbers underneath them and shoved the whole header past the edge of the
    pane — so the one row that explains the scale was the one row that did not
    line up.

    THE CENTRE LABEL IS `1.0×`, NOT `on pace`. All three captions are now
    pressure VALUES on one scale — `¼×`, `1.0×`, `4×` — which is what the
    positions actually mean, and it removes the last place the interface said
    "pace". `fixed` moved from 11 to 8 with it; getting that wrong is what
    broke the alignment last time, so the two are changed together.
    """
    out = Text()
    if width < 7:
        return Text("─" * max(0, width), style=MUTED)
    if width < 10:
        # No room for the centre label. The end stops still carry the scale,
        # and a caption that does not fit is worse than no caption: it overruns.
        out.append("¼×", style=MUTED)
        out.append("─" * (width - 4), style=MUTED)
        out.append("4×", style=MUTED)
        return out
    fixed = 8  # "¼×" 2 + "1.0×" 4 + "4×" 2
    pad = width - fixed
    left = pad // 2
    out.append("¼×", style=MUTED)
    out.append("─" * left, style=MUTED)
    out.append("1.0×", style=INK2)
    out.append("─" * (pad - left), style=MUTED)
    out.append("4×", style=MUTED)
    return out


# ── the cell that goes ON an account row ─────────────────────────────────────
#
# The operator asked for pressure beside each login, where he already looks,
# rather than only on a tab he has to go to. This is that cell, and it is the
# SAME renderer the dedicated view uses for its bar and its numbers, so the two
# places can never drift into disagreeing about what an account's pressure is.

CARD_BAR = {"full": 21, "narrow": 13, "compact": 0}


def account_cell(
    pressure: float | None,
    share: float | None,
    *,
    in_pool: bool = True,
    reason: str = "",
    bar_w: int = 21,
) -> tuple[Text, Text]:
    """One account's pressure as a (left, right) pair for a card row.

    Left is the label and the bar; right is the number and the DENOMINATOR.
    The share travels with the number and is never dropped to save room:
    "1.9x" against a 10% share and "1.9x" against a 100% share are different
    situations, and without the denominator the first reads as headroom it
    does not have.
    """
    left = Text("  ")
    # THE WORD IS PRESSURE. This row label read `pace`, and the operator struck
    # it: "I don't understand why you're using the word pace, because whichever
    # one has the highest PRESSURE is going to get the placement. That's all
    # that matters. It's just pressure."
    #
    # He is describing what the number is FOR. The daemon ranks the pool by
    # pressure and the top-ranked account takes the next turn; "pace" named an
    # intuition about the number's shape (a burn rate relative to a share)
    # rather than its function, and it made the one figure that decides
    # placement sound like a secondary read-out.
    left.append("pressure  ", style=MUTED)
    if bar_w >= 5:
        left.append_text(diverging_bar(pressure, bar_w))
    if pressure is None:
        # The reason goes on the LEFT, in the slack after the bar, so that the
        # right-hand columns stay a fixed width and the figures line up down a
        # stack of cards. A reason of variable length on the right would push
        # every account's number to a different column.
        left.append("  ")
        left.append(unranked_note(reason, in_pool=in_pool), style=MUTED)

    # FIXED WIDTHS. Nine cells for the value and four for the share, so the
    # decimal points and the percentages form columns the eye can run down.
    right = Text()
    if pressure is None:
        # NEVER a number here, and never a short bar. `diverging_bar` already
        # draws an empty track for None; the word carries the rest.
        right.append("unranked".rjust(9), style=MUTED)
    else:
        right.append(fmt_pressure(pressure).rjust(9), style=_pressure_style(pressure))
    right.append("  ")
    right.append(fmt_share(share).rjust(4), style=INK2 if share is not None else MUTED)
    right.append(" share", style=MUTED)
    return left, right


def _pressure_style(pressure: float) -> str:
    """Colour by what the number MEANS, not by size.

    Below zero is past its share; near 1.0 is level with its share and quiet;
    high is headroom, low is a reserve that must not be spent yet.
    """
    if pressure < 0:
        return f"bold {RED}"
    if pressure < 1.0 - DEADBAND:
        return AMBER
    if pressure > 1.0 + DEADBAND:
        return BLUE
    return INK2


# The daemon ranks within a 15% band as a tie (`Budgets` deadband). Colouring
# inside that band would imply a distinction the ranker does not make.
DEADBAND = 0.15


def pressure_lines(
    rows: list[PressureRow],
    width: int,
    *,
    unavailable: str | None = None,
) -> list[Text]:
    """The whole view. Pure: no clock, no I/O, no globals.

    `unavailable` is the honest empty state — passed in when
    `accounts/pressure` is not on the wire. It renders as an explanation, not
    as a board of zeroes.
    """
    lines: list[Text] = []
    if unavailable:
        lines.append(Text("PRESSURE", style=f"bold {INK}"))
        lines.append(Text(""))
        for chunk in _wrap_words(unavailable, max(20, width - 2)):
            lines.append(Text("  " + chunk, style=AMBER))
        return lines

    press_w = 9
    share_w = 5
    gutter = 2
    # The name column YIELDS FIRST when the pane is tight. The bar has a floor
    # of 9 cells (below that it cannot show a direction), and that floor used to
    # win unconditionally — so at a narrow width the row was assembled from a
    # full-width name plus an irreducible bar and simply ran off the edge. The
    # name is the one column that can be ellipsized without losing a fact.
    fixed = press_w + share_w + gutter * 3
    name_w = max(8, min(18, width - fixed - 9))
    bar_w = max(9, width - name_w - fixed)
    if bar_w % 2 == 0:
        bar_w -= 1  # odd width gives a true centre cell

    # THE HEADING DEGRADES RATHER THAN OVERRUNS. It says what the number is FOR
    # — the highest pressure takes the next turn — because that is the sentence
    # the operator used to explain why "pace" was the wrong word. But this view
    # is drawn at widths down to about 40 cells, and a heading longer than the
    # pane is not a heading, it is a broken row: `pressure_lines` promises every
    # line fits the width it was given, and the title is a line like any other.
    # So the explanation is dropped in one step and then the qualifier, and
    # `PRESSURE` alone always fits.
    for candidate in (
        "PRESSURE — highest takes the next turn · log scale",
        "PRESSURE — highest takes the next turn",
        "PRESSURE",
    ):
        if len(candidate) <= width:
            break
    lines.append(Text(candidate, style=f"bold {INK}"))

    head = Text()
    head.append(" " * (name_w + gutter), style=MUTED)
    head.append_text(scale_legend(bar_w))
    head.append(" " * gutter)
    head.append("pressure".rjust(press_w), style=MUTED)
    head.append(" " * gutter)
    head.append("share".rjust(share_w), style=MUTED)
    lines.append(head)

    for r in rows:
        line = Text()
        name = f"{r.number}. {r.label}"
        line.append(
            ellipsize(name, name_w).ljust(name_w),
            style=f"bold {INK}" if r.selected else INK2,
        )
        line.append(" " * gutter)
        line.append_text(diverging_bar(r.pressure, bar_w))
        line.append(" " * gutter)
        line.append(
            fmt_pressure(r.pressure).rjust(press_w),
            style=MUTED if r.pressure is None else INK2,
        )
        line.append(" " * gutter)
        line.append(fmt_share(r.share).rjust(share_w), style=MUTED)
        lines.append(line)
        if r.pressure is None:
            # ALWAYS a reason, now. This used to be conditional on the daemon
            # having sent one, so an account that was simply not in the pool got
            # a bare `unranked` with nothing under it — indistinguishable from a
            # pool member the daemon had declined to rank.
            note = Text()
            note.append(" " * (name_w + gutter))
            note.append(
                ellipsize(unranked_note(r.reason, in_pool=r.in_pool), max(10, bar_w)),
                style=MUTED,
            )
            lines.append(note)

    # ── band 2 ───────────────────────────────────────────────────────────────
    lines.append(Text(""))
    lines.append(Text("WHERE THE NEXT TURN WOULD GO", style=f"bold {INK}"))
    shares = relative_shares(rows)
    ranked = [r for r in rows if r.pressure is not None]
    if not ranked:
        lines.append(
            Text("  no account is ranked, so there is nothing to distribute", style=MUTED)
        )
        return lines

    strip_w = max(9, bar_w)
    for r in rows:
        pct = shares.get(r.label)
        line = Text()
        line.append(
            ellipsize(f"{r.number}. {r.label}", name_w).ljust(name_w),
            style=f"bold {INK}" if r.selected else INK2,
        )
        line.append(" " * gutter)
        line.append_text(strip_bar(pct, strip_w))
        line.append(" " * gutter)
        line.append(
            ("unranked" if pct is None else f"{pct:.0f}%").rjust(press_w),
            style=MUTED if pct is None else INK2,
        )
        lines.append(line)
    return lines


def _wrap_words(text: str, width: int) -> list[str]:
    """Pure word wrap. Local so this module imports nothing that does I/O."""
    words = text.split()
    out: list[str] = []
    cur = ""
    for w in words:
        if not cur:
            cur = w
        elif len(cur) + 1 + len(w) <= width:
            cur += " " + w
        else:
            out.append(cur)
            cur = w
    if cur:
        out.append(cur)
    return out or [""]
