"""END TO END: the BOARD changes because the DAEMON changed. No keystroke.

This is the assertion the whole change is for, and it is the one that cannot be
faked by a unit test. `test_feed.py` proves the state machine schedules a
re-read on a frame at a frozen clock; this proves the wire, the daemon, the
subscription, the coalesce window, the reload and the repaint are all actually
connected to each other.

    "If I have it open, that means I want to see live stuff happening in there
    as it's happening. All information updates should get relayed out as a
    broadcast and picked up by my text user interface."

HOW IT IS RIGGED, AND WHY THE RIG IS THE POINT.

  * A REAL `threads` daemon, on its own port, with its own `THREADS_HOME`. Not
    a mock. A mock of the daemon would be a mock of the exact thing in doubt —
    whether the daemon broadcasts at all — and it would have passed just as
    happily before this change as after it.
  * The REAL `BoardApp`, through Textual's own test pilot.
  * The change is made over a SECOND, SEPARATE WebSocket connection, so nothing
    the app itself did could account for what it then displays.
  * **`pilot.press` IS NEVER CALLED.** Not once, anywhere in this file. That is
    the whole claim: `r` is what used to be required and is what he was
    complaining about having to press. If a future edit makes this test need a
    keypress to go green, the feature is gone and the test must fail, not be
    helped along.

WHAT IT ASSERTS, IN ORDER: the app subscribes (the daemon exports `narration`);
the board is showing a known value; a third party changes that value in the
daemon; and the SCREEN shows the new value, with no input, within a couple of
seconds.

Run: .venv/bin/python tests/test_live_board.py   (from threads-app/)

It needs a daemon it is allowed to write to, and it will not go looking for one:
point it at a sandbox with THREADS_TUI_WS_PORT (and THREADS_E2E=1 to confirm the
daemon on that port is disposable). It SKIPS otherwise rather than touching
whatever is on the default port, because the default port is the operator's own
pool and this test sets pool shares.
"""

from __future__ import annotations

import asyncio
import json
import os
import sys

sys.path.insert(0, __file__.rsplit("/tests/", 1)[0])

FAILURES: list[str] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    if ok:
        print(f"  ok   {name}")
    else:
        print(f"  FAIL {name}  {detail}")
        FAILURES.append(name)


ACCOUNT = os.environ.get("THREADS_E2E_ACCOUNT", "demo-acct")


async def set_share(url: str, account: str, share: float) -> dict:
    """Change a fact in the daemon over a connection the app does not own."""
    from websockets.asyncio.client import connect

    async with connect(url) as ws:
        await ws.recv()  # welcome
        # THE ACCOUNT RIDES THE PATH AND MUST NOT ALSO BE IN THE PARAMS. This
        # sent `accounts.pool_share.set` with `{account, share}`; the daemon
        # would now refuse the method name, and refuse the params even once the
        # name was right. Spelled by hand rather than through `caps.fill`
        # because the point of this file is to drive the wire from outside the
        # app, so it must not borrow the app's idea of what the wire is.
        await ws.send(
            json.dumps(
                {
                    "type": "request",
                    "id": "e2e",
                    "method": f"accounts/{account}/pool_share_set",
                    "params": {"share": share},
                }
            )
        )
        while True:
            frame = json.loads(await ws.recv())
            if frame.get("type") == "response":
                return frame


def _screen_text(app) -> str:
    from textual.widgets import Static

    body = app.screen.query_one("#body", Static)
    strips = [body.render_line(y) for y in range(body.size.height)]
    return "\n".join("".join(seg.text for seg in s) for s in strips)


async def run() -> None:
    from threads_tui import endpoint
    from threads_tui.app import BoardApp

    url = endpoint.ws_url()
    print(f"\ndriving the REAL board against {url}")

    app = BoardApp()
    async with app.run_test(size=(140, 45)) as pilot:
        # ── the app connects and subscribes on its own ───────────────────────
        for _ in range(120):
            await pilot.pause()
            await asyncio.sleep(0.05)
            if app.feed.subscribed and app.board.accounts:
                break
        check(
            "the app subscribed (the daemon exported the narration fold to it)",
            app.feed.subscribed,
            "no narration export arrived — nothing will ever be pushed to this connection",
        )
        check(
            f"the board loaded and shows {ACCOUNT}",
            any(a.label == ACCOUNT for a in app.board.accounts),
            f"accounts: {[a.label for a in app.board.accounts]}",
        )
        if not app.board.accounts:
            return

        # ── a known starting value ───────────────────────────────────────────
        await set_share(url, ACCOUNT, 0.25)
        for _ in range(60):
            await pilot.pause()
            await asyncio.sleep(0.05)
            row = app.board.by_label(ACCOUNT)
            if row is not None and row.pool_share is not None and abs(row.pool_share - 0.25) < 1e-6:
                break
        before_share = app.board.by_label(ACCOUNT).pool_share
        before_screen = _screen_text(app)
        before_refresh = app.feed.last_refresh_at
        check(
            "the board settled on the starting share (25%)",
            before_share is not None and abs(before_share - 0.25) < 1e-6,
            f"share={before_share}",
        )

        # ── THE MOMENT. A third party changes the daemon. Nothing is typed. ──
        print("\n  … changing the share in the daemon over a separate connection")
        print("  … NOT pressing anything")
        await set_share(url, ACCOUNT, 0.77)

        moved = False
        for _ in range(80):  # ~4s ceiling; the coalesce window is 0.4s
            await pilot.pause()
            await asyncio.sleep(0.05)
            row = app.board.by_label(ACCOUNT)
            if row is not None and row.pool_share is not None and abs(row.pool_share - 0.77) < 1e-6:
                moved = True
                break

        after_screen = _screen_text(app)
        check(
            "THE BOARD PICKED UP THE CHANGE WITH NO KEYSTROKE",
            moved,
            f"share is still {app.board.by_label(ACCOUNT).pool_share} after ~4s "
            "— the announcement did not reach the board",
        )
        check(
            "and the PAINTED SCREEN changed, not just the model",
            before_screen != after_screen,
            "the model moved but the widget did not repaint",
        )
        check(
            "the new value is actually on screen",
            "77%" in after_screen,
            "77% is not in the painted text",
        )
        check(
            "the refresh clock advanced, so the header can say how current this is",
            app.feed.last_refresh_at is not None
            and (before_refresh is None or app.feed.last_refresh_at > before_refresh),
        )
        check(
            "the header reports a live subscription",
            "⚡" in after_screen and "NOT subscribed" not in after_screen,
        )

        # ── N >= 2: THE DEAD SOCKET, and it must be VISIBLE ──────────────────
        #
        # A subscription that dies silently is the failure he is reporting. So
        # the second case is not "does it recover" but "does it SAY SO".
        print("\n  … killing the app's socket underneath it")
        await app.api._ws.close()
        for _ in range(80):
            await pilot.pause()
            await asyncio.sleep(0.05)
            if not app.feed.subscribed:
                break
        check(
            "the feed stops claiming to be subscribed once the socket is gone",
            not app.feed.subscribed,
            "a dead feed still advertising itself as live is the frozen-screen bug",
        )
        down_screen = _screen_text(app)
        check(
            "and the screen SAYS the live updates stopped, rather than looking calm",
            ("live updates stopped" in down_screen) or ("NOT subscribed" in down_screen),
            f"screen says nothing about the dead feed",
        )
        check(
            "the refresh age is still shown while the link is down (it is what matters most then)",
            "updated" in down_screen or "not refreshed" in down_screen,
        )

    await app.api.stop()


def main() -> int:
    if os.environ.get("THREADS_E2E") != "1" or not os.environ.get("THREADS_TUI_WS_PORT"):
        print(
            "SKIP: end-to-end needs a disposable daemon.\n"
            "  THREADS_E2E=1 THREADS_TUI_WS_PORT=<port> .venv/bin/python tests/test_live_board.py\n"
            "  Refusing to run against the default port: this test SETS POOL SHARES."
        )
        return 0
    print("END TO END — the daemon changes, the board follows, nothing is typed")
    asyncio.run(run())
    print()
    if FAILURES:
        print(f"{len(FAILURES)} FAILED: {', '.join(FAILURES)}")
        return 1
    print("all end-to-end checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
