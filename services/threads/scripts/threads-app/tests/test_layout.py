"""The board must draw to the width it actually has, not the width of the terminal.

THE DEFECT THIS PINS. `#body` lives inside a `VerticalScroll`. When the content
is taller than the viewport a vertical scrollbar appears and eats two columns,
so the widget is two cells narrower than the terminal. The render layer pads
every line to EXACTLY the width it is given (`render._pad`), so being handed the
terminal width instead of the content width does not crop — it WRAPS. Every row
spills its last two cells onto a line of its own, the board doubles in height,
and the layout reads as broken. Taller content also keeps the scrollbar up, so
the fault sustains itself.

Two independent halves, and the second survives fixing the first:

  1. THE SCROLLBAR. Width was read from `App.size` (the terminal) rather than
     from the scroll container's content region.
  2. INITIAL LOAD. At first paint the content region is 0 — layout has not run —
     and there was no resize handler at all, so a resize was only picked up by
     the 1s tick. Reading the container naively would render a 0-wide board on
     the first frame; that is why the fix reserves the gutter with
     `scrollbar-gutter: stable`, which makes the content width CONSTANT and the
     pre-layout fallback agree with the post-layout measurement.

The load-bearing assertion is `painted height == produced line count`. That is
the wrap, which is what is actually visible, and it fails loudly on the old code.

Run: .venv/bin/python tests/test_layout.py   (from threads-app/)
"""

from __future__ import annotations

import asyncio
import sys
from datetime import datetime, timedelta, timezone

sys.path.insert(0, __file__.rsplit("/tests/", 1)[0])
sys.path.insert(0, __file__.rsplit("/test_layout.py", 1)[0])

from textual.containers import VerticalScroll  # noqa: E402
from textual.widgets import Static  # noqa: E402

import fake_daemon  # noqa: E402

from threads_tui.app import BoardApp, BoardScreen  # noqa: E402

FAILURES: list[str] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    if ok:
        print(f"  ok   {name}")
    else:
        print(f"  FAIL {name}  {detail}")
        FAILURES.append(name)


# ── fixtures ─────────────────────────────────────────────────────────────────


def _frames(n: int) -> list[dict]:
    """`n` accounts as SHAPE EXPORTS — the frames, not the assembled board.

    The frames are what the fixture hands the app now. `redraw()` projects from
    the mirror on every paint (S3: `model.project` is the only builder), so a
    test that assigned `app.board = …` directly would have its fixture wiped by
    the very next repaint. Feeding the mirror puts the fixture through the same
    door a real export goes through, which is also the only way this file can
    still fail when the app would fail.

    It used to be `build_board(gauge=…, health=…)` — two of the five query
    payloads, joined by label. S3 deleted that function, so a geometry fixture
    that still used it would be measuring a board the app can no longer
    produce. This goes through `model.project`, which is the one builder left.

    Every account carries two axes and a login expiry, because the CARD HEIGHT
    is what this file measures and a card with nothing on it is shorter than a
    real one. `days_left` is derived from an instant at projection time (there
    is no `days_left` on the wire), so the expiry is set as one.
    """
    return [
            fake_daemon.export(
                f"acct{i}",
                r=fake_daemon.row(
                    f"acct{i}",
                    gauge={
                        "session": fake_daemon.gauge_axis(40.0 + i),
                        "weekly-all": fake_daemon.gauge_axis(20.0 + i),
                    },
                    credential=fake_daemon.credential(
                        identity={
                            "acct": f"a{i}@example.com",
                            "email": f"a{i}@example.com",
                        },
                        is_live_login=(i == 0),
                        refresh_expires_at_ms=int(
                            (fake_daemon.NOW + timedelta(days=6 + i)).timestamp() * 1000
                        ),
                    ),
                ),
            )
            for i in range(n)
    ]


def _board(n: int):
    """The same `n` accounts, projected — for the pure line-count arithmetic."""
    return fake_daemon.board_of(*_frames(n))


class Harness(BoardApp):
    """The real app with the network removed. Everything else is the real path.

    **OVERRIDING `on_mount` DOES NOT REMOVE THE NETWORK, AND USED NOT TO.**
    Textual does not dispatch a message to one handler — it walks the MRO and
    calls EVERY `on_mount` it finds, base class first. So `BoardApp.on_mount`
    ran on every one of these harnesses, `api.start()` with it, and this test
    has been opening a real WebSocket to the live daemon and receiving real
    frames from it since it was written.

    That went unnoticed for as long as no pushed frame could change the board's
    HEIGHT: the frames were fed to the running-thread count, which lives in the
    header, and the header is two lines whatever it says. The live information
    feed broke that — a `lifecycle` frame the daemon replays to every new
    subscriber puts a narration line on the board — and the failure it produced
    was a layout test that passed or failed depending on whether the daemon had
    restarted recently. Non-deterministic, and nothing to do with the widths
    this file exists to pin.

    So the network is now removed for real, by shutting the client down after
    the base class has started it, which is the only order the MRO allows.
    """

    async def on_mount(self) -> None:
        # `BoardApp.on_mount` has already run and already started the socket.
        # This is the earliest point at which it can be stopped.
        self.api.begin_shutdown()
        # And drop anything a frame that beat us here may have left behind, so
        # the board this test measures is a pure function of its fixture.
        self.feed.reset()
        self.feed.note_at = None
        # `self.live.reset()` stood here. `threads_tui/live.py` is deleted
        # (S3) and the mirror it was replaced by is reset by `_on_link` when
        # the socket dies — which `begin_shutdown` above brings about. The
        # board this test measures stays a pure function of its fixture,
        # because `redraw()` re-projects from an empty mirror and the fixture
        # is assigned straight onto `self.board` afterwards.
        self.push_screen(BoardScreen())


# ── the measurements ─────────────────────────────────────────────────────────


def _measure(app) -> dict:
    screen = app.screen
    body = screen.query_one("#body", Static)
    scroll = screen.query_one(VerticalScroll)
    strips = [body.render_line(y) for y in range(body.size.height)]
    return {
        "terminal": app.size.width,
        "widget": body.size.width,
        "scrollbar": scroll.show_vertical_scrollbar,
        "painted": body.size.height,
        "widths": {s.cell_length for s in strips},
    }


async def _frame(width: int, height: int, n: int, *, settle: bool = True) -> dict:
    app = Harness()
    async with app.run_test(size=(width, height)) as pilot:
        for frame in _frames(n):
            app.shapes.on_frame(frame)
        app.redraw()
        if settle:
            await pilot.pause()
            await pilot.pause()
            app.redraw()
            await pilot.pause()
        else:
            # ONE pause: the first frame the operator sees, nothing more.
            await pilot.pause()
        m = _measure(app)
        m["produced"] = len(app.last_render_lines) if hasattr(app, "last_render_lines") else None
        app.api.begin_shutdown()
    return m


def _expected_lines(app_board, width: int, height: int) -> int:
    """Count the lines the render layer produces, independently of the widget."""
    from threads_tui import render

    mode = render.layout_mode(width, max(10, height))
    total = 2  # two header lines
    total += len(list(app_board.warnings()))
    # ── THE HEADROOM ROW IS ONE CARD TALLER THAN THE OTHERS ──────────────────
    #
    # `BoardApp.redraw` resolves `Board.room()` once per paint and hands
    # `room_leader=True` to that one card, which adds a line to it (the reading
    # the operator moved off the header). This helper deliberately MIRRORS the
    # app's loop rather than calling it, so the mirror has to carry the flag or
    # it under-counts by exactly one line on every board with a gauged,
    # enabled account — which is what it did, on nine of this file's cases, the
    # first time the card grew.
    room = app_board.room()
    leader = room[0].label if room is not None else None
    for index, account in enumerate(app_board.accounts):
        total += len(
            render.card_lines(
                account,
                width,
                selected=(index == 0),
                mode=mode,
                warn_below=app_board.warn_below_days,
                extras=None,
                flashing=None,
                now=datetime(2025, 8, 27, 12, 0, tzinfo=timezone.utc),
                room_leader=(leader is not None and account.label == leader),
            )
        )
    total += len(app_board.notes)
    return total


# ── cases ────────────────────────────────────────────────────────────────────

# Several terminal widths, and at each one an account count below and above the
# point where the content outgrows the viewport. The pairs straddle the
# transition deliberately: the no-scrollbar case is the control that proves the
# test is capable of passing, and the with-scrollbar case is the defect.
TERMINALS = [(80, 24), (100, 30), (120, 40), (140, 50), (160, 24), (92, 26)]


def _transition(width: int, height: int) -> int:
    """The smallest account count whose board is taller than the viewport.

    DERIVED, not hardcoded. The transition is a property of how tall a card is,
    and a card gets taller whenever a row is added to it — pinning the number
    would mean the suite quietly stopped testing the boundary the first time
    the card grew, which is exactly when it matters most.
    """
    viewport = height - 1  # the Footer takes a row
    body_w = width - 2  # the reserved scrollbar gutter
    for n in range(1, 60):
        if _expected_lines(_board(n), body_w, height) > viewport:
            return n
    raise AssertionError(f"no transition found for {width}x{height}")


def _cases() -> list[tuple[int, int, int, bool]]:
    """Straddle the transition at every terminal size, plus a large case.

    The below-the-line case is the CONTROL: it proves the assertions are capable
    of passing, so a green above the line means something.
    """
    out: list[tuple[int, int, int, bool]] = []
    for width, height in TERMINALS:
        n = _transition(width, height)
        if n > 1:
            out.append((width, height, n - 1, False))
        out.append((width, height, n, True))
        out.append((width, height, n + 6, True))
    return out


async def run() -> None:
    print("\nthe painted board is exactly as tall as the lines it produced")
    print("(a wrap shows up here as painted > produced)")
    for width, height, n, want_bar in _cases():
        m = await _frame(width, height, n)
        board = _board(n)
        produced = _expected_lines(board, m["widget"], height)
        tag = f"{width}x{height} n={n}"
        check(
            f"{tag}: scrollbar {'appears' if want_bar else 'stays away'}",
            m["scrollbar"] == want_bar,
            f"got scrollbar={m['scrollbar']}",
        )
        check(
            f"{tag}: no line wraps",
            m["painted"] == produced,
            f"painted {m['painted']} rows for {produced} lines "
            f"(widget {m['widget']}, terminal {m['terminal']})",
        )
        check(
            f"{tag}: every line is exactly the widget width",
            m["widths"] == {m["widget"]},
            f"line widths {sorted(m['widths'])} vs widget {m['widget']}",
        )

    print("\nthe content width is stable across the scrollbar transition")
    below = await _frame(100, 30, 2)
    above = await _frame(100, 30, 10)
    check(
        "crossing the transition does not change the drawing width",
        below["widget"] == above["widget"],
        f"{below['widget']} without a scrollbar, {above['widget']} with one",
    )

    print("\nthe FIRST frame is already right (no settling, no tick)")
    first = await _frame(100, 30, 10, settle=False)
    produced = _expected_lines(_board(10), first["widget"], 30)
    check(
        "initial load: no line wraps on the first painted frame",
        first["painted"] == produced,
        f"painted {first['painted']} for {produced} lines, widget {first['widget']}",
    )
    check(
        "initial load: the width is not zero",
        first["widget"] > 0,
        f"widget {first['widget']}",
    )

    print("\na resize is reflected in the next frame, not one second later")
    app = Harness()
    async with app.run_test(size=(120, 40)) as pilot:
        for frame in _frames(14):
            app.shapes.on_frame(frame)
        await pilot.pause()
        await pilot.pause()
        wide = _measure(app)
        await pilot.resize_terminal(92, 26)
        # SETTLE UNTIL STABLE, not a fixed number of pauses. A resize is handled
        # in two passes on purpose (`BoardScreen.on_resize` redraws, then
        # redraws again after layout), so a fixed pause count is a race with
        # the machine's load rather than a statement about the code. Under load
        # this measured mid-resize and reported a part-painted board — a
        # failure that says nothing about the width logic it is here to test.
        # The assertion afterwards is unchanged and just as strict.
        narrow = _measure(app)
        for _ in range(40):
            await pilot.pause()
            settled = _measure(app)
            if settled == narrow and settled["widget"] > 0:
                break
            narrow = settled
        app.api.begin_shutdown()
    check(
        "resize: the board narrows with the terminal",
        narrow["widget"] < wide["widget"],
        f"{wide['widget']} -> {narrow['widget']}",
    )
    check(
        "resize: no line wraps after the resize",
        narrow["widths"] == {narrow["widget"]},
        f"line widths {sorted(narrow['widths'])} vs widget {narrow['widget']}",
    )
    check(
        "resize: painted height matches the produced line count",
        narrow["painted"] == _expected_lines(_board(14), narrow["widget"], 26),
        f"painted {narrow['painted']}",
    )


def main() -> int:
    asyncio.run(run())
    print()
    if FAILURES:
        print(f"{len(FAILURES)} FAILED: {', '.join(FAILURES)}")
        return 1
    print("all layout checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
