"""**PARKED IS A POOL STATE, NOT AN ACCOUNT ONE (H179).**

The operator, on the board reading "3 parked on alice-example-com": *"you never
park TO an account. Parked threads wait in the pool; any placement event triggers
re-placing ALL parked threads, each onto whichever account admission picks."*

The daemon already holds parked turns pool-wide (`parked_turns` has no account
column; every placement pass sweeps every row). The board was the thing that
said otherwise: it drew `· N parked` on the card of the account each thread
LAST sat on. The same correction the queue depth already had: the number is a
pool fact, drawn once in the header, and a card carries no parked count.

Every assertion is on the RENDERED string, which is the thing he reads.
"""

from __future__ import annotations

import sys
from datetime import datetime, timezone

sys.path.insert(0, __file__.rsplit("/tests/", 1)[0])

from threads_tui import render  # noqa: E402
from threads_tui.model import Account, Activity, Board  # noqa: E402

NOW_DT = datetime(2026, 10, 15, 12, 0, tzinfo=timezone.utc)


def _parked_board() -> Board:
    """Two accounts, parked threads last seen on BOTH — 3 on cc1, 2 on cc2.

    Two accounts, because a fixture with one could not tell a pool SUM from one
    account's number.
    """
    board = Board()
    for number, (label, parked) in enumerate(
        (("alice-example-com", 3), ("bob-example-com", 2)), start=1
    ):
        a = Account(number=number, label=label, state="live")
        a.activity = Activity(known=True, executing=0, parked=parked, retrying=0)
        board.accounts.append(a)
    return board


def test_no_card_draws_a_parked_count() -> None:
    """THE MUTATION. Restore `detail.append(f" · {act.parked} parked")` on the
    card's detail line and this goes red with the string he reported."""
    board = _parked_board()
    for account in board.accounts:
        card = "\n".join(
            line.plain
            for line in render.card_lines(
                account, 120, selected=True, mode=render.FULL, warn_below=4.0,
                extras=None, flashing=None, now=NOW_DT,
            )
        )
        # NON-VACUOUS: the card really drew its activity detail line.
        assert "daemon:" in card, card
        assert "parked" not in card, [ln for ln in card.splitlines() if "parked" in ln]


def test_the_pool_parked_count_is_in_the_header() -> None:
    """The board's own pool sum, drawn beside `running` and `msgs queued`."""
    board = _parked_board()
    assert board.parked_count() == 5
    segment = render.running_segment(
        render.HeaderState(
            running=1, queued=0, parked=board.parked_count(), running_live=True
        )
    ).plain
    assert "5 parked in pool" in segment, segment


def test_no_parked_segment_when_nothing_is_parked_or_before_the_first_export() -> None:
    """Zero parked draws nothing (the header stays as it was), and before the first
    export there is no confident number to draw at all."""
    quiet = render.running_segment(
        render.HeaderState(running=2, queued=7, parked=0, running_live=True)
    ).plain
    assert "parked" not in quiet, quiet
    unknown = render.running_segment(
        render.HeaderState(running=0, queued=0, parked=4, running_live=False)
    ).plain
    assert "4 parked" not in unknown, unknown
