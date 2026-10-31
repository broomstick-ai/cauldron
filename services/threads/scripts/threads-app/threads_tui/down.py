"""The daemon-down page. One page, one button.

    "if the daemon is down, I actually just want to have one page that says
     the daemon is down with a button to restart the daemon. That's it."

So there is no degraded board here, no stale meters, no "last known" numbers,
and no fallback to the database. When the daemon is not answering, the app has
no facts, and a screen full of numbers it cannot stand behind is worse than a
screen that says so. This page and the board are mutually exclusive.

THE ONE PLACE THE API RULE CANNOT HOLD
--------------------------------------
The rule is that every operation goes through the API. Restarting a daemon that
is not answering cannot, by construction: there is nothing on the other end to
ask. This is not a shortcut, it is a consequence — a dead process cannot be
asked to revive itself.

So the button is a THIRD sanctioned local operation, and it is kept as narrow
as the other two:

  * It is only reachable from this page, which is only shown when liveness has
    actually failed.
  * It performs exactly one action and reads no facts.
  * When the daemon IS reachable, this page is not shown, and a restart goes
    through `daemon/restart` over the API like anything else. The local path is
    for the case where the API does not exist to be used.

WHY launchctl AND NOT A BARE SPAWN
----------------------------------
The daemon is supervised by launchd (`com.harness.threads`, which runs a
supervisor script that picks the binary and handles the last-good fallback).
Spawning `target/debug/threads` directly would create a SECOND, unsupervised
daemon which then races the supervised one for port 3651 — the port bind is the
only mutual exclusion these processes have, so the loser dies having done
nothing, and which one loses is a coin flip. Asking launchd to kickstart the
job it already owns restarts the daemon that is actually meant to be running.
The direct spawn is kept only for the case where the launchd job is absent.
"""

from __future__ import annotations

import asyncio
import os
from dataclasses import dataclass

from textual.app import ComposeResult
from textual.containers import Center, Middle, Vertical
from textual.screen import Screen
from textual.widgets import Button, Label, Static

from .endpoint import PATHS, daemon_endpoint

LAUNCHD_LABEL = "com.harness.threads"

# The seam that lets this page be exercised without actually restarting the
# operator's daemon. Set to 1 and the button reports the command it would run.
DRY_RUN_ENV = "THREADS_TUI_DRY_RUN"


@dataclass
class RestartResult:
    ok: bool
    detail: str
    dry_run: bool = False


def restart_argv() -> list[str]:
    """The supervised restart. `kickstart -k` starts a stopped job and restarts
    a running one, so one command covers both states."""
    uid = os.getuid()
    return ["launchctl", "kickstart", "-k", f"gui/{uid}/{LAUNCHD_LABEL}"]


async def restart_daemon() -> RestartResult:
    """SANCTIONED SUBPROCESS (3 of 3): start a daemon that cannot be asked."""
    argv = restart_argv()
    if os.environ.get(DRY_RUN_ENV) == "1":
        return RestartResult(True, " ".join(argv), dry_run=True)

    try:
        proc = await asyncio.create_subprocess_exec(
            *argv,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        out, err = await asyncio.wait_for(proc.communicate(), timeout=30)
    except FileNotFoundError:
        return await _spawn_directly()
    except asyncio.TimeoutError:
        return RestartResult(False, "launchctl did not return within 30s")

    if proc.returncode == 0:
        return RestartResult(True, "asked launchd to restart the daemon")

    text = (err or out or b"").decode("utf-8", "replace").strip()
    # No such job: this machine is not running the supervised daemon, so fall
    # back to starting the binary directly. Nothing to race in that case.
    if "no such process" in text.lower() or "could not find" in text.lower():
        return await _spawn_directly()
    return RestartResult(False, text or f"launchctl exited {proc.returncode}")


async def _spawn_directly() -> RestartResult:
    binary = PATHS.threads_bin
    if binary is None:
        return RestartResult(
            False,
            "no launchd job and no built threads binary — build it with "
            "`cargo build -p threads` and try again",
        )
    if os.environ.get(DRY_RUN_ENV) == "1":
        return RestartResult(True, f"{binary} (detached)", dry_run=True)
    try:
        await asyncio.create_subprocess_exec(
            str(binary),
            stdout=asyncio.subprocess.DEVNULL,
            stderr=asyncio.subprocess.DEVNULL,
            stdin=asyncio.subprocess.DEVNULL,
            env=PATHS.child_env(),
            cwd=str(PATHS.threads_home),
            start_new_session=True,
        )
    except Exception as exc:
        return RestartResult(False, f"could not start the daemon: {exc}")
    return RestartResult(True, f"started {binary.name} directly (no launchd job found)")


class DaemonDownScreen(Screen):
    """The whole app while the daemon is not answering."""

    DEFAULT_CSS = """
    DaemonDownScreen { background: #1a1a19; }
    DaemonDownScreen Vertical { width: 72; height: auto; }
    DaemonDownScreen .headline { text-style: bold; color: #d03b3b; }
    DaemonDownScreen .body { color: #c3c2b7; }
    DaemonDownScreen .where { color: #898781; }
    DaemonDownScreen #status { height: auto; color: #fab219; }
    DaemonDownScreen Button { margin: 1 0; }
    """

    BINDINGS = [("q", "quit_app", "quit"), ("r", "restart", "restart")]

    def compose(self) -> ComposeResult:
        host, port, _ = daemon_endpoint()
        with Middle():
            with Center():
                with Vertical():
                    yield Label("The threads daemon is not running.", classes="headline")
                    yield Static("", classes="body")
                    yield Static(
                        "Nothing on this screen is a stale reading, because there "
                        "are no readings. Every number this board shows comes from "
                        "the daemon, so with the daemon down there is nothing "
                        "honest to draw.",
                        classes="body",
                    )
                    yield Static("", classes="body")
                    yield Static(f"tried  ws://{host}:{port}/ws", classes="where")
                    yield Button("restart the daemon", id="restart", variant="error")
                    yield Static("", id="status")
                    yield Static("r restart · q quit", classes="where")

    async def action_restart(self) -> None:
        await self._restart()

    async def on_button_pressed(self, event: Button.Pressed) -> None:
        if event.button.id == "restart":
            await self._restart()

    async def _restart(self) -> None:
        status = self.query_one("#status", Static)
        button = self.query_one("#restart", Button)
        button.disabled = True
        status.update("restarting…")
        result = await restart_daemon()
        if result.dry_run:
            status.update(f"dry run — would run: {result.detail}")
        elif result.ok:
            status.update(f"{result.detail} — waiting for it to come back…")
        else:
            status.update(f"could not restart: {result.detail}")
        button.disabled = False

    def action_quit_app(self) -> None:
        self.app.exit()
