"""Headless smoke test for the board.

Run:  .venv/bin/python -P smoke.py            (against the live daemon)
      .venv/bin/python -P smoke.py --down     (force the daemon-down page)

It drives the real app through Textual's test pilot, so it needs no terminal.

WHAT IT WILL NOT DO. It never completes a login, never sets the machine
account, and never restarts the daemon. `THREADS_TUI_DRY_RUN=1` is exported
before the app starts, so the one destructive button reports the command it
would run instead of running it. The point of a smoke test is to prove the
surfaces are reachable, not to operate the machine.
"""

from __future__ import annotations

import asyncio
import os
import sys

os.environ.setdefault("THREADS_TUI_DRY_RUN", "1")
os.environ.setdefault(
    "THREADS_TUI_SCRIPTS_DIR",
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
)

FAILURES: list[str] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    if ok:
        print(f"  ok   {name}")
    else:
        print(f"  FAIL {name}  {detail}")
        FAILURES.append(name)


async def settle(pilot, seconds: float) -> None:
    for _ in range(int(seconds * 10)):
        await pilot.pause()
        await asyncio.sleep(0.1)


async def run_live() -> None:
    from threads_tui import caps
    from threads_tui.app import BoardApp
    from threads_tui.down import DaemonDownScreen
    from threads_tui.link import LinkState

    app = BoardApp()
    async with app.run_test(size=(120, 42)) as pilot:
        await settle(pilot, 6.0)

        check("the link reached the daemon", app.api.state is LinkState.LIVE, str(app.api.state))
        check("the board is not showing the daemon-down page",
              not isinstance(app.screen, DaemonDownScreen))
        check("accounts were loaded from the API", len(app.board.accounts) > 0,
              f"{len(app.board.accounts)} accounts")

        # Capability probing must have produced a verdict for every read method,
        # not left them dangling as UNKNOWN.
        undecided = [m for m in caps.PROBE_ORDER if app.api.caps.state(m) is caps.State.UNKNOWN]
        check("every read capability was probed", not undecided, f"undecided: {undecided}")
        present = [m for m in caps.PROBE_ORDER if app.api.caps.has(m)]
        missing = app.api.caps.missing()
        print(f"       present: {present}")
        print(f"       absent : {missing}")

        # ── THE ONE SOURCE, CHECKED AS THE ONE SOURCE ───────────────────────
        #
        # Four checks stood here and all four named a capability S3 deleted
        # (`ACCOUNTS_STATUS`, `GAUGE_STATUS`, `ACCOUNTS_PRESSURE`,
        # `ACCOUNTS_POOL_SHARE`), so this file raised `AttributeError` on the
        # first of them and the live smoke test could not be run at all. They
        # were asking "did the five queries answer" of a board that no longer
        # asks them.
        #
        # What replaces them is the same question put to the one source: the
        # fold exists, this connection holds it, and every number on screen
        # came off it.
        check("shapes/list is PRESENT — the daemon holds a fold",
              app.api.caps.has(caps.SHAPES_LIST),
              f"state={app.api.caps.state(caps.SHAPES_LIST)}")
        check("the mirror took at least one export",
              app.shapes.exported, f"exports={app.shapes.exports}")
        check("the fold is not dead", not app.shapes.dead, str(app.shapes.dead_reason))
        check("the board's cards are the mirror's keys",
              len(app.board.accounts) >= len(app.shapes.keys()),
              f"{len(app.board.accounts)} cards vs {len(app.shapes.keys())} keys")

        # A value in an UNKNOWN cell must stay unknown rather than being
        # invented — the three-valued state asserted end to end, on whatever
        # the live daemon happens to be holding.
        for account in app.board.accounts:
            if not account.cells["pressure"].known:
                check(f"{account.label}: an unknown pressure cell is not a zero",
                      account.pressure is None, repr(account.pressure))
            if not account.cells["share"].known:
                check(f"{account.label}: an unknown share cell is not a 100%",
                      account.pool_share is None, repr(account.pool_share))

        # Navigation.
        await pilot.press("j"); await pilot.pause()
        await pilot.press("k"); await pilot.pause()
        check("selection survives navigation",
              0 <= app.selected < max(1, len(app.board.accounts)))

        # The pressure view renders, whether or not the method exists.
        await pilot.press("p")
        await settle(pilot, 1.0)
        from threads_tui.app import PressureScreen
        check("the pressure view opens", isinstance(app.screen, PressureScreen))
        await pilot.press("escape"); await settle(pilot, 0.5)

        # The drag note is reachable IN THE TOOL, which is the requirement —
        # a note only in a file is a note nobody meets.
        await pilot.press("exclamation_mark")
        await settle(pilot, 1.0)
        from threads_tui.app import DragScreen
        check("the drag note is reachable from the board", isinstance(app.screen, DragScreen))
        await pilot.press("escape"); await settle(pilot, 0.5)

        # The share editor: either it opens, or it explains why it cannot.
        await pilot.press("S")
        await settle(pilot, 1.0)
        from threads_tui.share import ShareEditor
        opened = isinstance(app.screen, ShareEditor)
        check("the share editor either opens or says why not",
              opened or bool(app.status_note), app.status_note)
        if opened:
            await pilot.press("escape"); await settle(pilot, 0.5)

        # The verdict wait must not be shorter than the call it waits on. This
        # is the exact defect being fixed, asserted as a property.
        #
        # **THE CALL IT WAITS ON CHANGED, AND SO DID THIS CHECK.** It read
        # `SETTLE_DEADLINE >= HEALTH_TIMEOUT` and `SETTLE_DEADLINE` has not
        # existed since S3 — this file raised `ImportError` here. The verdict
        # no longer waits on `accounts/health`; it waits on the mirror taking a
        # fresh export, which is a `shapes/{shape}/subscribe` plus the
        # daemon's own work after it accepts the item. So the number it must
        # not be shorter than is `REQUEST_TIMEOUT`, and `RESYNC_DEADLINE` is
        # DERIVED from it rather than chosen beside it — which is the property
        # worth asserting, because an independently-chosen number is exactly
        # how the original defect happened.
        from threads_tui.api import REQUEST_TIMEOUT
        from threads_tui.app import RESYNC_DEADLINE
        check("the verdict wait is at least the call it waits on",
              RESYNC_DEADLINE >= REQUEST_TIMEOUT,
              f"resync {RESYNC_DEADLINE} vs request {REQUEST_TIMEOUT}")

        app.api.begin_shutdown()


async def run_down() -> None:
    """Point the app at a dead port and require the one-page treatment."""
    os.environ["THREADS_TUI_WS_PORT"] = "3999"
    from threads_tui.app import BoardApp
    from threads_tui.down import DaemonDownScreen

    app = BoardApp()
    async with app.run_test(size=(120, 42)) as pilot:
        # Backoff has to run out before the link declares itself offline.
        await settle(pilot, 22.0)

        check("the daemon-down page is shown", isinstance(app.screen, DaemonDownScreen),
              type(app.screen).__name__)
        check("no accounts were invented while the daemon was unreachable",
              len(app.board.accounts) == 0, f"{len(app.board.accounts)}")

        if isinstance(app.screen, DaemonDownScreen):
            from textual.widgets import Static
            # The restart button, exercised in dry-run: it must report the
            # command rather than run it.
            await pilot.press("r")
            await settle(pilot, 2.0)
            text = str(app.screen.query_one("#status", Static).render())
            check("the restart button is wired and reports a command",
                  "dry run" in text and "launchctl" in text, text)

        app.api.begin_shutdown()


def main() -> int:
    down = "--down" in sys.argv
    print(f"threads board smoke — {'daemon-down path' if down else 'live daemon'}")
    asyncio.run(run_down() if down else run_live())
    print()
    if FAILURES:
        print(f"{len(FAILURES)} FAILED: {', '.join(FAILURES)}")
        return 1
    print("smoke passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
