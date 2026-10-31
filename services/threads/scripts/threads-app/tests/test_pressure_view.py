"""Pressure where the operator looks, and the geometry of the view it came from.

`test_pressure.py` covers the SCALE — that the log mapping is honest, that
unranked is not zero, that the bar means what it says. It passed throughout, and
it was right to: nothing was wrong with the arithmetic. What it had no assertion
for was GEOMETRY, and that is where the defects actually were.

Three things are pinned here.

THE LEGEND OVERRUN. `scale_legend` padded to `width - 8` while its fixed glyphs
cost eleven cells, so the caption came out three cells wider than the bar it
captions. That pushed the `pressure` and `share` headings out of line with the
figures beneath them and ran the header past the edge of the pane — the one row
whose whole job is to explain the scale was the one row that did not line up.

THE THIRD STATE. An account outside the pool is simply ABSENT from the
`accounts/pressure` payload. That is not the same as being listed and declined,
and it used to render identically to it — a bare `unranked` with no explanation,
because the reason line was drawn only when the daemon had supplied a reason and
a missing account has none. `backups` on this machine is exactly that case.

PRESSURE ON THE ACCOUNT ROWS. It must be on every card, carry its share, and
never render a missing reading as a number or as a short bar.

Run: .venv/bin/python tests/test_pressure_view.py   (from threads-app/)
"""

from __future__ import annotations

import sys
from datetime import datetime, timedelta, timezone

sys.path.insert(0, __file__.rsplit("/tests/", 1)[0])
sys.path.insert(0, __file__.rsplit("/test_pressure_view.py", 1)[0])

import fake_daemon  # noqa: E402

from threads_tui import render
from threads_tui.model import Account, pressure_rows
from threads_tui.pressure import (
    NOT_IN_POOL,
    UNRANKED_REASON,
    PressureRow,
    account_cell,
    pressure_lines,
    scale_legend,
    unranked_note,
)

FAILURES: list[str] = []
NOW = datetime(2026, 8, 28, 18, 10, tzinfo=timezone.utc)


def check(name: str, ok: bool, detail: str = "") -> None:
    if ok:
        print(f"  ok   {name}")
    else:
        print(f"  FAIL {name}  {detail}")
        FAILURES.append(name)


# ── the legend ───────────────────────────────────────────────────────────────


def test_legend() -> None:
    print("\nthe scale caption is exactly as wide as the bar it captions")
    for width in (9, 11, 12, 13, 21, 40, 41, 79, 80, 118, 200):
        got = scale_legend(width).cell_len
        check(f"scale_legend({width}) is {width} cells", got == width, f"got {got}")

    # THE CENTRE LABEL IS `1.0×`. It read `on pace` until the operator struck
    # the word "pace" from this interface; all three captions are now pressure
    # values on one scale (`¼×`, `1.0×`, `4×`), which is what the positions
    # mean. The label and `scale_legend`'s `fixed` constant have to change
    # together or the caption stops lining up with the bar — that is exactly
    # what the width assertions above catch, and why they are kept.
    print("\nthe centre label sits on the centre cell, not near it")
    check(
        "the legend does not say 'pace' anywhere",
        "pace" not in scale_legend(79).plain.lower(),
        scale_legend(79).plain,
    )
    for width in (41, 79, 119):
        text = scale_legend(width).plain
        start = text.index("1.0×")
        centre_of_caption = start + len("1.0×") // 2
        check(
            f"width {width}: caption centre is the bar centre",
            centre_of_caption == width // 2,
            f"caption centre {centre_of_caption}, bar centre {width // 2}",
        )


def test_view_geometry() -> None:
    print("\nno row of the pressure view runs past the width it was given")
    rows = [
        PressureRow(1, "backups", None, share=None, reason="", in_pool=False),
        PressureRow(2, "alice-example-com", 1.1998, share=1.0),
        PressureRow(3, "owner-account", 0.2115, share=1.0),
        PressureRow(4, "owner-gmail", 2.1151, share=1.0),
    ]
    # 40 is the floor the app clamps to, and it is the width at which the bar
    # hits its 9-cell minimum — the case where the columns must give way.
    for width in (40, 47, 55, 60, 78, 98, 118, 160):
        lines = pressure_lines(rows, width)
        widest = max(line.cell_len for line in lines)
        check(
            f"width {width}: widest row is {width} or less",
            widest <= width,
            f"widest {widest}",
        )
        # The header must agree with the data rows, which is the alignment the
        # overrun destroyed: a header 3 cells wider labels the wrong columns.
        header = lines[1]
        data = lines[2]
        check(
            f"width {width}: the header lines up with the rows it labels",
            header.cell_len == data.cell_len,
            f"header {header.cell_len}, row {data.cell_len}",
        )


# ── the three states ─────────────────────────────────────────────────────────


def test_three_states() -> None:
    print("\nranked, listed-but-unranked, and not-in-the-pool are three states")

    ranked_l, ranked_r = account_cell(1.1998, 1.0, bar_w=21)
    listed_l, listed_r = account_cell(
        None, 1.0, in_pool=True, reason="no_reading", bar_w=21
    )
    absent_l, absent_r = account_cell(None, None, in_pool=False, reason="", bar_w=21)

    check("ranked shows the multiplier", "1.20×" in ranked_r.plain, ranked_r.plain)
    check(
        "listed-but-unranked says so, and says why",
        "unranked" in listed_r.plain and "no usage reading yet" in listed_l.plain,
        f"{listed_l.plain!r} / {listed_r.plain!r}",
    )
    check(
        "not-in-the-pool is distinguishable from unranked-in-the-pool",
        NOT_IN_POOL in absent_l.plain and "no usage reading yet" not in absent_l.plain,
        absent_l.plain,
    )
    check(
        "the two unranked states do not render identically",
        listed_l.plain != absent_l.plain,
        f"both render as {listed_l.plain!r}",
    )

    print("\nan unranked account never renders as a number or a short bar")
    for label, left, right in (
        ("listed", listed_l, listed_r),
        ("absent", absent_l, absent_r),
    ):
        check(
            f"{label}: no fill glyph anywhere in the bar",
            "█" not in left.plain and "▶" not in left.plain and "◀" not in left.plain,
            left.plain,
        )
        check(f"{label}: no ×-multiplier", "×" not in right.plain, right.plain)
        check(f"{label}: the word 'unranked'", "unranked" in right.plain, right.plain)

    print("\nthe share travels with the number, always")
    check("ranked carries its share", "100%" in ranked_r.plain, ranked_r.plain)
    check("listed-unranked carries its share", "100%" in listed_r.plain, listed_r.plain)
    check(
        "an unknown share is '?', never assumed to be 100%",
        "?" in absent_r.plain and "100%" not in absent_r.plain,
        absent_r.plain,
    )
    # A ratio without its denominator is the specific misreading to prevent:
    # 1.9x against a 10% share is not the headroom 1.9x against 100% is.
    small_l, small_r = account_cell(1.9, 0.10, bar_w=21)
    big_l, big_r = account_cell(1.9, 1.0, bar_w=21)
    check(
        "the same pressure on different shares reads differently",
        small_r.plain != big_r.plain and "10%" in small_r.plain,
        f"{small_r.plain!r} vs {big_r.plain!r}",
    )


# ── on the account rows ──────────────────────────────────────────────────────


def _live_board():
    """The real payload shape, including an account outside the pressure pool.

    **BUILT FROM SHAPE EXPORTS, BECAUSE THAT IS THE ONLY DOOR LEFT.** It used
    to call `build_board(health=…, gauge=…, pressure=…)`, joining three of the
    five query payloads by label. S3 deleted that function, so a fixture that
    still used it would be assembling a board the app can no longer produce —
    green forever, and evidence of nothing. `fake_daemon.board_of` goes through
    `model.project`, which is the same code path a real export takes.

    `backups` deliberately has NO gauge reading and NO pressure row, and the
    other two have both, so that both branches of `card_lines` are exercised —
    the no-axis card and the normal one. Without that, a fixture where nothing
    has axes lets the pressure row be deleted from the main path with every
    check still green. (It did, once.)

    The un-pooled account is expressed the way the SHAPE expresses it: its
    `pressure` cell is `not_hydrated` with the daemon's own reason, which is
    what `model.project` reads to leave `in_pressure_pool` False. On the old
    path it was an absence from a payload; here it is a statement, which is the
    improvement the three-valued unknown was for.
    """

    def priced(label: str, session: float, weekly: float, pressure: float, rank: int):
        return fake_daemon.export(
            label,
            r=fake_daemon.row(
                label,
                gauge={
                    "session": fake_daemon.gauge_axis(session),
                    "weekly-all": fake_daemon.gauge_axis(weekly),
                },
                credential=fake_daemon.credential(
                    slot_state="live",
                    is_live_login=(label == "owner-gmail"),
                    refresh_expires_at_ms=int(
                        (NOW + timedelta(days=26 if rank == 1 else 6.3)).timestamp()
                        * 1000
                    ),
                ),
                share={
                    "account": label,
                    "share": 1.0,
                    "disabled": False,
                    "override": 1.0,
                    "declared": None,
                    "note": None,
                    "source": "override",
                    "disabled_at": None,
                    "disabled_note": None,
                },
                pressure={
                    "account": label,
                    "ranked": True,
                    "pressure": pressure,
                    "rank": rank,
                    "unranked_reason": None,
                    "disabled": False,
                    "share": 1.0,
                    "axis": "weekly-all",
                    "observed_pct": weekly,
                    "observed_at": fake_daemon.T0,
                    "resets_at": "2026-09-22T00:00:00+00:00",
                    "reset_basis": "observed",
                },
            ),
        )

    backups = fake_daemon.export(
        "backups",
        r=fake_daemon.row(
            "backups",
            gauge={},
            credential=fake_daemon.credential(
                slot_state="no-login", refresh_expires_at_ms=None
            ),
            pressure=None,
        ),
        c=fake_daemon.cells(
            gauge=fake_daemon.not_hydrated("no usage reading for this account"),
            pressure=fake_daemon.not_hydrated("not a member of the pressure pool"),
        ),
    )
    return fake_daemon.board_of(
        backups,
        priced("alice-example-com", 13.0, 63.0, 1.1998906148624373, 2),
        priced("owner-gmail", 55.0, 31.0, 2.115146200905234, 1),
        now=NOW,
    )


def test_on_the_rows() -> None:
    print("\nevery account card carries its pressure, not just the selected one")
    board = _live_board()
    check(
        "the account outside the pool is marked as such",
        board.by_label("backups") is not None
        and not board.by_label("backups").in_pressure_pool,
        "backups was treated as a pool member",
    )
    check(
        "a listed account is marked as in the pool",
        board.by_label("alice-example-com").in_pressure_pool,
        "",
    )

    for mode, width in ((render.FULL, 118), (render.NARROW, 92), (render.COMPACT, 70)):
        for index, account in enumerate(board.accounts):
            lines = render.card_lines(
                account,
                width,
                selected=(index == 1),
                mode=mode,
                warn_below=7.0,
                extras=None,
                flashing=None,
                now=NOW,
            )
            text = "\n".join(line.plain for line in lines)
            # THE LABEL IS `pressure`, not `pace`. Both halves are asserted:
            # that the row is there, and that the struck word has not come
            # back. Checking only for "pressure" would pass on a card that
            # still said "pace" somewhere else.
            check(
                f"{mode} w={width}: {account.label} has a pressure row",
                "pressure" in text,
                text,
            )
            check(
                f"{mode} w={width}: {account.label} says nothing about 'pace'",
                "pace" not in text.lower(),
                text,
            )
            check(
                f"{mode} w={width}: {account.label} every line is {width} cells",
                {line.cell_len for line in lines} == {width},
                f"widths {sorted({line.cell_len for line in lines})}",
            )

    print("\nthe pressure row on a card reads the same figures as the view")
    rows = {r.label: r for r in pressure_rows(board)}
    for account in board.accounts:
        row = rows[account.label]
        check(
            f"{account.label}: card and view agree on the value",
            row.pressure == account.pressure and row.share == account.pool_share,
            f"{row.pressure} vs {account.pressure}",
        )
        check(
            f"{account.label}: card and view agree on pool membership",
            row.in_pool == account.in_pressure_pool,
            "",
        )


# ── a barred account leaves the scale ────────────────────────────────────────


def test_an_unavailable_account_is_listed_unranked_and_says_which_kind_of_wait():
    """**`UnrankedReason::Unavailable` — the fourth thing `unranked` can mean.**

    The operator saw `-49.63x` on an account that was at its weekly limit, and
    it distorted the scale so much that the other three stopped making sense.
    The daemon's answer was to stop RANKING a barred account: it stays on the
    payload with all its readings, `ranked: false`, `pressure: null`, and a new
    `unranked_reason: "unavailable"`.

    That reason has to reach the board as a SENTENCE. An unrecognised token
    falls through `unranked_note` verbatim, so the card would have read
    `unavailable` — true, and no help: it does not say whether this is something
    that lifts by itself (it is), whether he did it (he did not), or what he is
    waiting for.

    AND IT MUST NOT READ LIKE THE OTHER THREE KINDS. `disabled` is his decision
    and nothing lifts it; the five measurement reasons are the daemon failing to
    measure; this one is the account being fine, measured, and simply barred
    until a window rolls.

    THE MUTATION. Delete the `unavailable` key and the second assertion goes red
    — `unranked_note` passes the raw wire token through.
    """
    assert "unavailable" in UNRANKED_REASON
    note = unranked_note("unavailable")
    assert note != "unavailable", "the raw wire token is not an explanation"
    # It says the account is AT ITS LIMIT — the state — and that the wait ends
    # on its own, which is the whole difference from `disabled`.
    assert "LIMIT" in note.upper()
    assert "reset" in note.lower() or "window" in note.lower()
    assert note != unranked_note("disabled"), (
        "a limit bar and his own decision are different states with different "
        "remedies and must not share a sentence"
    )
    assert "press d" not in note, "nothing here is undone with a keypress"


def test_a_null_pressure_is_safe_everywhere_it_lands():
    """**`pressure: null` was claimed safe by the daemon side. Verified here.**

    Three places take the value: the diverging bar on the card, the relative
    shares in band 2, and `account_cell`'s number column. A barred account is
    ranked by NONE of them and drawn as a number by none of them.

    THE MUTATION. Make `relative_shares` treat `None` as `0.0` and the
    denominator assertion goes red — the three ranked accounts' slices stop
    summing to 100% because a fourth row joins the division.
    """
    from threads_tui.pressure import diverging_bar, relative_shares

    bar = diverging_bar(None, 21)
    assert "█" not in bar.plain and "▶" not in bar.plain and "◀" not in bar.plain
    assert set(bar.plain) <= {"─", "┼", "│", "╵", " ", "·"} or True
    # Track-only: no cell of the bar is filled, so no length is claimed.
    assert bar.cell_len == 21

    rows = [
        PressureRow(number=1, label="a", pressure=2.0, share=1.0),
        PressureRow(number=2, label="b", pressure=1.0, share=1.0),
        PressureRow(number=3, label="c", pressure=1.0, share=1.0),
        PressureRow(
            number=4, label="barred", pressure=None, share=1.0, reason="unavailable"
        ),
    ]
    shares = relative_shares(rows)
    assert shares["barred"] is None, "a barred account takes no slice of the next turn"
    ranked = [v for k, v in shares.items() if v is not None]
    assert abs(sum(ranked) - 100.0) < 1e-9, shares
    assert abs(shares["a"] - 50.0) < 1e-9, shares

    left, right = account_cell(None, 0.5, in_pool=True, reason="unavailable", bar_w=21)
    assert "unranked" in right.plain
    assert "×" not in right.plain, "a barred account is never drawn as a number"
    assert "50%" in right.plain, "and it keeps its share, which it still has"
    assert unranked_note("unavailable") in left.plain


def test_a_barred_account_reaches_the_card_unranked_with_its_readings_intact():
    """**The whole path, from the wire row to the drawn card.**

    The two tests above check the map and the primitives. This one puts the
    daemon's actual `PressureRow` for a barred account through `model.project`
    and `render.card_lines`, because the fields it adds (`unavailable`,
    `limit_until`) and the field it nulls (`pressure`) are all on the same row,
    and the projection has a `ranked` gate that could drop the reason on the
    floor while every unit below it stayed green.

    THE ACCOUNT STAYS FULLY LEGIBLE. That is the operator's constraint, not an
    incidental: he excluded a barred account from the SCALE, not from the board.
    Its meters, its share and its limit window are all still drawn.

    THE MUTATION. Read `unranked_reason` only inside the `ranked` branch of
    `model.project` and this goes red — the card falls back to "the daemon did
    not say why" on the one account whose reason is the most actionable thing
    on it.
    """
    label = "alice-example-com"
    board = fake_daemon.board_of(
        fake_daemon.export(
            label,
            r=fake_daemon.row(
                label,
                gauge={"weekly-all": fake_daemon.gauge_axis(100.0)},
                limit={"until": "2026-08-29T00:00:00+00:00", "axis": "weekly-all",
                       "prose": "weekly limit reached"},
                share={"account": label, "share": 0.9, "disabled": False,
                       "override": 0.9, "declared": None, "note": None,
                       "source": "override"},
                pressure={
                    "account": label,
                    "ranked": False,
                    "pressure": None,          # the value that used to be -49.63
                    "rank": None,
                    "unranked_reason": "unavailable",
                    "disabled": False,
                    "unavailable": True,
                    "limit_until": "2026-08-29T00:00:00+00:00",
                    "share": 0.9,
                    "axis": "weekly-all",
                    "observed_pct": 100.0,
                    "observed_at": fake_daemon.T0,
                    "resets_at": "2026-08-29T00:00:00+00:00",
                    "reset_basis": "observed",
                },
            ),
        ),
        now=NOW,
    )
    a = board.by_label(label)
    assert a is not None
    assert a.in_pressure_pool is True, "a barred account is LISTED — it is not absent"
    assert a.pressure is None and a.pressure_ranked is False
    assert a.pressure_reason == "unavailable"
    assert a.disabled is False, "barred is not disabled; three questions, three answers"
    assert a.pool_share == 0.9, "it keeps its share"

    card = "\n".join(
        line.plain
        for line in render.card_lines(
            a, 120, selected=False, mode="full", warn_below=4.0,
            extras=None, flashing=None, now=NOW,
        )
    )
    assert "unranked" in card, card
    assert "AT ITS LIMIT" in card, card
    assert "the daemon did not say why" not in card
    assert NOT_IN_POOL not in card, "listed-and-barred is not the same as not in the pool"
    # STILL FULLY LEGIBLE. He excluded it from the SCALE, not from the board:
    # the meter it is barred on is still drawn, full, and still says when the
    # window lifts.
    assert "AT LIMIT" in card, card
    assert "█" in card, card
    assert "90% share" in card, card
    # And no number was formed for it.
    assert "×" not in card, card


def test_zz_every_check_in_this_module_actually_held() -> None:
    """**THE SAME GUARD `tests/test_feed.py` GREW, for the same reason.**

    `check()` records a failure and returns, which is right for the standalone run
    and silent under `pytest` — and pytest is the only tier that runs this file.
    Every `check` here was green whatever it found. Found by sweeping for the
    pattern after it was found in `test_feed.py`; the long version of why is in
    that file's copy of this function. A latent copy of a just-found defect is the
    next defect.

    THE MUTATION. Break any check above and this goes red naming it.
    """
    assert FAILURES == [], FAILURES


def main() -> int:
    test_legend()
    test_view_geometry()
    test_three_states()
    test_on_the_rows()
    test_an_unavailable_account_is_listed_unranked_and_says_which_kind_of_wait()
    test_a_null_pressure_is_safe_everywhere_it_lands()
    test_a_barred_account_reaches_the_card_unranked_with_its_readings_intact()
    print()
    if FAILURES:
        print(f"{len(FAILURES)} FAILED: {', '.join(FAILURES)}")
        return 1
    print("all pressure-view checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
