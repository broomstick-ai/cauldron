"""THE SPARKLINE'S X-AXIS IS A TIME, AND IT HAD STOPPED BEING ONE.

    "the gauge.history sparkline looks unaligned"

It was, three ways at once, and each one is a separate assertion below.

  1. EMPTY BUCKETS WERE DROPPED. `[max(v) for v in binned if v]` returned a list
     whose length was the number of buckets that happened to hold a sample. Cell
     `i` was the `i`-th POPULATED slice, not the `i`-th slice of the week, so two
     observations six days apart rendered as two ADJACENT glyphs — and the
     figure's width became a function of polling density, so no two cards' lines
     lined up down the stack.

  2. THE GRID WAS THE OBSERVED SPAN, NOT THE WINDOW. The heading says `weekly 7d`
     and the bins were cut between the first and last OBSERVATION, so an account
     last polled six hours ago drew six hours stretched across the full width,
     under a seven-day label.

  3. TRUNCATION WAS IGNORED. `gauge/history` answers `truncated: true` per series
     and the board never read it. The clip TOOK the OLDEST rows
     (`store.rs::observations_between` was `ORDER BY id ASC LIMIT ?` then; it is
     `DESC` now, reversed back to ascending, so the clip takes the NEWEST), so the
     figure silently showed the FIRST part of the week and called it the week.
     The daemon's own comment predicted exactly this: *"a sparkline drawn from a
     silently-truncated series is a lie about the shape of the week."*

NO `check()` HELPER IN THIS FILE, DELIBERATELY. `check()` records a failure and
returns, which under pytest is a test that passes; see
`test_feed.test_zz_every_check_in_this_module_actually_held`. These are bare
`assert`s, so there is no tier in which they can be green while failing.

THE MUTATIONS, each forced red before this file was trusted:
  * `return [max(values) if values else None ...]` -> `... for values in binned if values`
    (the old line) reddens `test_a_gap_holds_its_place_*` and `test_the_grid_is_the_window`.
  * `history_window` -> `return None` reddens `test_the_grid_is_the_window`.
  * `history_truncated` -> `return False` reddens `test_a_clipped_series_says_so*`.
  * `SPARK_GAP = SPARK_CHARS[0]` reddens `test_a_gap_is_not_drawn_as_a_low_bar`.
  * the clip note rewritten to the OPPOSITE end -- `"▸ oldest only; the recent
    end was clipped"` -- reddens `test_a_clipped_series_is_labelled_partial_on_the_card`.
    IT DID NOT USED TO. The test asserted only the substring `"clipped"`, which
    every rewording survives, so the one mutation this file exists to catch -- a
    note that names the wrong end -- was silently green. The two `assert`s on the
    note's actual words are what closed that -- but NOT to the query: see the
    comment on those asserts. The query coupling is section 4's, which reads the
    SQL out of `store.rs`.
  * `store.rs::observations_between` reordered between `ASC` and `DESC` without
    rewording the note reddens `test_the_clip_note_names_the_END_THE_QUERY_ACTUALLY_KEEPS`,
    in either direction. Forced both ways.
  * `pub fn observations_between` renamed reddens
    `test_the_query_is_parseable_at_all`, loudly, rather than letting section 4
    pass vacuously against a regex that no longer matches.
  * the note emitted unconditionally (`if extras.truncated:` -> `if True:`)
    reddens the same test's counter-case, so the absence half is not vacuous.
"""

from __future__ import annotations

import sys
from datetime import datetime, timedelta, timezone
import re
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

# threads-app -> scripts -> threads. Same hop `test_wire_inventory.py` makes to
# read `server.rs`; the board and the daemon are one repo and this path is how
# the suite already reaches across the seam.
STORE_RS = Path(__file__).resolve().parents[3] / "src" / "store.rs"

from threads_tui.model import (  # noqa: E402
    history_sparkline,
    history_truncated,
    history_window,
)
from threads_tui import render  # noqa: E402

SINCE = datetime(2026, 10, 1, 0, 0, tzinfo=timezone.utc)
UNTIL = SINCE + timedelta(days=7)
BUCKETS = 40


def _at(offset: timedelta) -> str:
    return (SINCE + offset).isoformat()


def _payload(
    points: list[tuple[timedelta, float]],
    *,
    truncated: bool = False,
    window: bool = True,
    account: str = "work",
    axis: str = "weekly_all",
) -> dict:
    series = {
        "account": account,
        "axis": axis,
        "points": [{"at": _at(off), "pct": pct} for off, pct in points],
        "truncated": truncated,
    }
    out: dict = {"series": [series], "limit": 1000}
    if window:
        out["since"] = SINCE.isoformat()
        out["until"] = UNTIL.isoformat()
    return out


# ── 1. the grid is kept, so a cell is a time ─────────────────────────────────


def test_the_grid_is_the_window_and_every_bucket_is_present() -> None:
    """Two readings SIX DAYS APART are six days apart on the figure.

    This is the assertion the whole change is for. The two glyphs must sit at
    opposite ends of a `BUCKETS`-wide line with unobserved cells between them —
    not side by side, which is what dropping empty buckets produced and which
    reads as "two consecutive readings".
    """
    spark = history_sparkline(
        _payload([(timedelta(hours=1), 20.0), (timedelta(days=6, hours=1), 90.0)]),
        "work",
    )
    assert len(spark) == BUCKETS, (
        f"the figure must be one cell per bucket of the window, got {len(spark)}",
        spark,
    )
    observed = [i for i, v in enumerate(spark) if v is not None]
    assert len(observed) == 2, (observed, spark)
    # NON-VACUOUS: the two values really are in there, so this is a figure with
    # content being positioned, not an all-None list that trivially spreads.
    assert spark[observed[0]] == 20.0 and spark[observed[1]] == 90.0, spark
    # Six of seven days apart on a 40-cell grid is ~34 cells. The old code put
    # them at indices 0 and 1.
    assert observed[1] - observed[0] > BUCKETS // 2, (
        "six days of separation collapsed to adjacent cells — the axis is gone",
        observed,
    )


def test_the_grid_is_the_window_not_the_observed_span() -> None:
    """Six hours of observations do not fill a seven-day figure.

    The heading is `weekly 7d`. Binning between the first and last observation
    made that heading false: a six-hour cluster was stretched across the full
    width, so a nearly-unpolled account and a fully-polled one drew the same
    shape.
    """
    cluster = [(timedelta(hours=h), 50.0 + h) for h in range(6)]
    spark = history_sparkline(_payload(cluster), "work")
    observed = [i for i, v in enumerate(spark) if v is not None]
    assert len(spark) == BUCKETS, spark
    # Six hours is 3.6% of a week: every sample lands in the first two cells.
    assert max(observed) <= 2, (
        "a six-hour cluster is being spread across the week",
        observed,
    )
    assert observed[0] == 0, observed
    # And the tail is explicitly unobserved rather than absent.
    assert spark[-1] is None, spark


def test_without_a_window_it_falls_back_but_still_keeps_the_grid() -> None:
    """A pre-`since`/`until` daemon loses the AXIS but not the ALIGNMENT.

    The fallback cannot know the window, so the figure's left and right edges are
    the first and last observation — wrong, knowably. What it must not also do is
    drop the gaps, because that is the error that is fixable without the daemon.
    """
    spark = history_sparkline(
        _payload(
            [(timedelta(hours=1), 20.0), (timedelta(days=6), 90.0)],
            window=False,
        ),
        "work",
    )
    assert len(spark) == BUCKETS, spark
    assert spark[0] == 20.0 and spark[-1] == 90.0, spark
    assert any(v is None for v in spark), "the gaps were dropped in the fallback"


def test_history_window_refuses_what_it_cannot_use() -> None:
    assert history_window({"since": SINCE.isoformat(), "until": UNTIL.isoformat()}) == (
        SINCE,
        UNTIL,
    )
    assert history_window({"since": SINCE.isoformat()}) is None
    assert history_window({"until": UNTIL.isoformat()}) is None
    # Inverted, and equal: neither is a span to bin against.
    assert history_window({"since": UNTIL.isoformat(), "until": SINCE.isoformat()}) is None
    assert history_window({"since": SINCE.isoformat(), "until": SINCE.isoformat()}) is None
    assert history_window(None) is None


def test_a_bucket_still_reports_its_MAXIMUM() -> None:
    """The change must not have turned the fold into a mean or a last-wins.

    A spike to 100% inside an otherwise quiet bucket is the single most important
    thing the row can show.
    """
    same_bucket = [(timedelta(minutes=m), pct) for m, pct in ((0, 10.0), (5, 100.0), (10, 12.0))]
    spark = history_sparkline(_payload(same_bucket), "work")
    assert spark[0] == 100.0, spark


def test_nothing_to_draw_is_an_empty_figure_not_a_grid_of_gaps() -> None:
    """`[]` is the caller's signal to write the sentence instead.

    `_expansion` branches on `not extras.sparkline` to say "no observations in
    the last 7 days". A 40-cell all-gap line is truthy and would route past it,
    drawing an empty grid with no explanation.
    """
    assert history_sparkline(_payload([]), "work") == []
    assert history_sparkline({"series": []}, "work") == []
    assert history_sparkline(None, "work") == []
    # And an account that is not in the payload is the same answer.
    assert history_sparkline(_payload([(timedelta(hours=1), 5.0)]), "other") == []


# ── 2. the gap is not a low reading ──────────────────────────────────────────


def test_a_gap_is_not_drawn_as_a_low_bar() -> None:
    """`·` and not `▁`.

    `▁` is the bottom of the bar scale and scans as "almost nothing was used
    here" — a reading the daemon never took. The two must not look alike, which
    is the same three-valued rule the rest of this board is built on.
    """
    assert render.SPARK_GAP not in render.SPARK_CHARS, (
        "the gap glyph is on the bar scale, so absence renders as a low value",
        render.SPARK_GAP,
    )
    drawn = render._sparkline([0.0, None, 100.0]).plain
    assert drawn == f"{render.SPARK_CHARS[0]}{render.SPARK_GAP}{render.SPARK_CHARS[7]}", drawn
    # NON-VACUOUS: a real ZERO reading is still drawn on the scale, so this is
    # not "anything low becomes a dot".
    assert render._sparkline([0.0]).plain == render.SPARK_CHARS[0]


def test_the_rendered_figure_is_as_wide_as_the_grid() -> None:
    """The width is the alignment, down the stack and against the line to its right.

    Every card's figure must be the same number of cells whatever each account's
    polling density was, or the burn-rate sentence `_expansion` puts on the right
    sits in a different column on every card.
    """
    dense = history_sparkline(
        _payload([(timedelta(hours=h), 40.0) for h in range(0, 168, 2)]), "work"
    )
    sparse = history_sparkline(
        _payload([(timedelta(hours=1), 40.0), (timedelta(days=6), 40.0)]), "work"
    )
    assert len(render._sparkline(dense).plain) == len(render._sparkline(sparse).plain), (
        len(dense),
        len(sparse),
    )
    # NON-VACUOUS: the two series really do differ in sample count, so equal
    # widths is a property of the grid and not of identical inputs.
    assert sum(v is not None for v in dense) > sum(v is not None for v in sparse)


# ── 3. a clipped series says so ──────────────────────────────────────────────


def test_a_clipped_series_says_so() -> None:
    assert history_truncated(_payload([(timedelta(hours=1), 5.0)], truncated=True), "work")
    assert not history_truncated(_payload([(timedelta(hours=1), 5.0)]), "work")
    # Another account's truncation is not this account's.
    assert not history_truncated(
        _payload([(timedelta(hours=1), 5.0)], truncated=True, account="other"), "work"
    )
    # Another axis's truncation is not this axis's.
    assert not history_truncated(
        _payload([(timedelta(hours=1), 5.0)], truncated=True, axis="session"), "work"
    )
    assert not history_truncated(None, "work")


def test_a_clipped_series_is_labelled_partial_on_the_card() -> None:
    """The figure is still shown — it is real data — and it stops claiming to be
    the whole week.

    Rendered through the real card so the assertion is about what he SEES, not
    about a field.
    """
    extras = render.CardExtras(sparkline=[10.0, None, 90.0], loaded=True, truncated=True)
    whole = render.CardExtras(sparkline=[10.0, None, 90.0], loaded=True, truncated=False)
    clipped_line = render._expansion(_account(), 100, extras, _now())[0].plain
    whole_line = render._expansion(_account(), 100, whole, _now())[0].plain

    assert "clipped" in clipped_line, clipped_line
    # NON-VACUOUS, and this is the pairing a vacuous absence assertion needs: the
    # same render with the same figure and `truncated=False` must NOT carry the
    # note, so the needle is demonstrably producible in this exact render.
    assert "clipped" not in whole_line, whole_line
    # WHICH END SURVIVED IS PINNED, NOT JUST THE WORD "clipped". Asserting the
    # substring alone passes against a note claiming the OPPOSITE end, which is
    # how the note and the query came to disagree in the first place: the clip
    # keeps the NEWEST rows (`store.rs::observations_between` is `ORDER BY id
    # DESC LIMIT ?`, reversed back to ascending), so the note says the OLDER end
    # is the one that went.
    #
    # WHAT THESE TWO PIN, EXACTLY — and it is less than the comment here used to
    # claim. They pin the note's WORDING: reword the note and this reds, which is
    # real coverage the older `assert "clipped" in ...` did not have (that one
    # survived the note being rewritten to its exact opposite). They do NOT pin
    # the note against the QUERY. They render a Python `CardExtras` and assert on
    # the Python string it produces; no change to `store.rs` can reach them.
    #
    # THAT WAS MEASURED, NOT ASSUMED. The comment that stood here said "if
    # `observations_between` is ever reordered so the cap keeps the NEWEST rows,
    # this line must red". The reorder was merged — git reported the merge CLEAN,
    # since the query and the note live in different files and no branch touched
    # both — and `pytest tests/test_sparkline.py` answered 10 passed with the
    # note reading "oldest only" against an `ORDER BY id DESC` query.
    #
    # The coupling now exists for real, in section 4 below, which reads the SQL
    # out of `store.rs` and compares it with this note. These assertions were
    # re-pointed rather than deleted, so the wording stays pinned here too.
    assert "newest only" in clipped_line, clipped_line
    assert "the older end was clipped" in clipped_line, clipped_line
    # Both still draw the figure itself.
    for line in (clipped_line, whole_line):
        assert render.SPARK_GAP in line, line
        assert "weekly 7d" in line, line


# ── 4. the note and the QUERY, actually tied together ────────────────────────
#
# THE COMMENT ABOVE THOSE TWO ASSERTS USED TO CLAIM THIS COUPLING AND DID NOT
# HAVE IT. It said "if `observations_between` is ever reordered so the cap keeps
# the NEWEST rows, this line must red". It could not: that test renders a Python
# `CardExtras` and asserts on the Python string it produces, and no Rust change
# can reach it. The reorder WAS merged, the note said "oldest only" against an
# `ORDER BY id DESC` query, and `pytest tests/test_sparkline.py` answered
# 10 passed. Measured, not reasoned about.
#
# So the mechanism is here instead, and it is the one the rest of this suite
# already uses for a cross-seam claim: read the daemon's raw source
# (`test_wire_inventory.py` does exactly this to `server.rs::WS_METHODS`) and
# compare it against what the board SAYS. A comment asserting a coupling is a
# wish; a test that reads both ends is the coupling.


def order_direction() -> str:
    """`ASC` or `DESC`, read out of `observations_between`'s own SQL.

    Parsed rather than copied, for the reason `test_wire_inventory.py` gives
    about `WS_METHODS`: a copy here would be a second source for the one thing
    this check exists to compare against, and it would go stale exactly the way
    the clip note did.
    """
    text = STORE_RS.read_text()
    fn = re.search(
        r"pub fn observations_between\b.*?\n    \}", text, re.S
    )
    assert fn, f"`observations_between` not found in {STORE_RS}"
    body = fn.group(0)
    order = re.search(r"ORDER BY id (ASC|DESC) LIMIT", body)
    assert order, f"no `ORDER BY id <dir> LIMIT` in observations_between:\n{body[:400]}"
    return order.group(1)


def test_the_query_is_parseable_at_all() -> None:
    """THE PRECONDITION, and it is not ceremony.

    Every assertion below is of the form "the source says X, so the board must
    say Y". If the parse stops matching -- the function renamed, the SQL moved
    into a helper, the file relocated -- those assertions would pass vacuously
    against nothing, which is the failure mode this section exists to object to.
    So the parse is asserted on its own first, and it fails loudly rather than
    skipping.
    """
    assert STORE_RS.is_file(), f"expected the daemon's store at {STORE_RS}"
    assert order_direction() in ("ASC", "DESC")
    # `out.reverse()` is what keeps the DECLARED order ascending while the cap
    # chooses the other end. If it ever disappears under `DESC`, every caller's
    # x-axis inverts and the note is the least of the problems -- so it is
    # pinned here too, where the direction is already in hand.
    body = STORE_RS.read_text()
    assert "out.reverse();" in body, "the DESC answer is no longer reversed back to ascending"


def test_the_clip_note_names_the_END_THE_QUERY_ACTUALLY_KEEPS() -> None:
    """The forcing function, for real this time.

    `ORDER BY id DESC LIMIT ?` keeps the NEWEST rows, so the note must say the
    OLDER end went. `ASC` keeps the OLDEST, so it must say the RECENT end went.
    Reorder the query without rewording the note -- in either direction -- and
    this reds, naming both halves.
    """
    direction = order_direction()
    extras = render.CardExtras(sparkline=[10.0, None, 90.0], loaded=True, truncated=True)
    note = render._expansion(_account(), 100, extras, _now())[0].plain

    expected, wrong = {
        "DESC": (("newest only", "the older end was clipped"),
                 ("oldest only", "the recent end was clipped")),
        "ASC": (("oldest only", "the recent end was clipped"),
                ("newest only", "the older end was clipped")),
    }[direction]

    for needle in expected:
        assert needle in note, (
            f"`observations_between` is ORDER BY id {direction}, so the clip note "
            f"must contain {needle!r}. It is: {note.strip()!r}"
        )
    for needle in wrong:
        assert needle not in note, (
            f"`observations_between` is ORDER BY id {direction}, but the clip note "
            f"still claims {needle!r} -- it names the end the cap KEEPS, not the "
            f"end it drops. Note: {note.strip()!r}"
        )


def _now() -> datetime:
    return UNTIL


def _account():
    from threads_tui.model import Account

    return Account(number=1, label="work")
