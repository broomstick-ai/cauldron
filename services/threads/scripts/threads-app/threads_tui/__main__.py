"""Entry point for `python -m threads_tui`.

Argument dispatch (board vs. the Rust CLI) happens in the `threads` launcher
shim, not here, so that `threads accounts health` never pays for a Python
interpreter start it does not need.

`--check` is the board's equivalent of the daemon's `--self-check`: it boots the
whole app headlessly, connects, renders one frame and exits. It exists because
"the command is installed" and "the command works from here" are different
claims, and only the second one matters. A TUI cannot be verified by piping it,
so this is how the launcher is proven from an arbitrary working directory —
including one containing a `json.py` that would shadow the standard library if
the shim ever lost its `-P` flag.
"""

from __future__ import annotations

import asyncio
import sys

from .app import main


async def _check() -> int:
    from .app import BoardApp
    from .link import LinkState

    app = BoardApp()
    async with app.run_test(size=(100, 30)) as pilot:
        for _ in range(60):
            await pilot.pause()
            await asyncio.sleep(0.1)
            if app.api.state is LinkState.LIVE and app.board.accounts:
                break
        live = app.api.state is LinkState.LIVE
        accounts = len(app.board.accounts)
        caps_present = sorted(m for m, s in app.api.caps.states.items() if s.value == "present")
        app.api.begin_shutdown()

    print(
        f"threads board self-check ok — link {'live' if live else app.api.state.value}, "
        f"{accounts} accounts, {len(caps_present)} API methods present"
    )
    # The board is usable when it is connected and has drawn real accounts.
    return 0 if (live and accounts) else 1


if __name__ == "__main__":
    if "--check" in sys.argv:
        sys.exit(asyncio.run(_check()))
    sys.exit(main())
