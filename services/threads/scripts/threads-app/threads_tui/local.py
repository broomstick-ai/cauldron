"""THE ONE CARVE-OUT: setting this machine's local account.

Everything else in this app goes through the API. This does not, and the reason
is not convenience — it is that the daemon deliberately HAS NO CONCEPT OF A
DEFAULT ACCOUNT, and it should not acquire one. "Which account does plain
`claude` sign in as on this laptop?" is a property of the laptop, not of the
pool. Teaching the daemon about it would put machine-local state into a service
that is meant to be indifferent to which machine is asking.

So this is a bounded exception, and the boundary is worth stating precisely:

  * It is exactly ONE operation. This module has one public entry point.
  * It READS NOTHING. It does not look at the pointer file, the keychain, or
    `~/.claude.json` to decide what to display. The board's facts still all come
    from the API; this only ever WRITES, and the verdict afterwards is read back
    from the API like every other fact.
  * It does not reimplement the operation. The bash tool already does the
    backup, the identity verification and the pointer write, and those are
    hard-won. This shells out to it and reports what it said.

WHAT IT ACTUALLY CHANGES, via `claude-accounts set-live-login`: `~/.claude.json`
and the macOS keychain item for `Claude Code-credentials`, plus a pointer file
the Rust side reads as a TIE-BREAK only. A backup of both is taken first, by
the bash tool, before anything is touched.

WHY IT IS ASYNC: the old app ran this through a blocking `subprocess.run` on the
event loop, which froze the whole UI — and, because the WebSocket client cannot
answer a ping while the loop is blocked, tore down the daemon connection. Every
child process here is awaited, never waited on.
"""

from __future__ import annotations

import asyncio
import os
from dataclasses import dataclass

from .endpoint import PATHS

# The bash tool prints this and stops when asked to dry-run. Its presence is
# how we know the preflight actually ran rather than failing early for some
# unrelated reason and exiting zero.
DRY_RUN_MARKER = "--dry-run: stopping here"

# The bash tool prints this when the machine login is ALREADY the requested
# account — on a dry run and a real one alike — and stops before any browser
# trip. Without it the preflight on such an account exited 0 with no dry-run
# marker and was refused as "never checked anything" (H179). Keep in step with
# `cmd_set_live_login` in lib/threads/scripts/claude-accounts.
ALREADY_LIVE_MARKER = "already-live:"

PREFLIGHT_TIMEOUT = 120.0
SET_LIVE_TIMEOUT = 900.0


@dataclass
class Outcome:
    ok: bool
    code: int
    stdout: str = ""
    stderr: str = ""
    refusal: str | None = None
    # The tool said the machine login already IS this account: no sign-in owed.
    already_live: bool = False

    @property
    def message(self) -> str:
        if self.refusal:
            return self.refusal
        tail = (self.stderr or self.stdout or "").strip().splitlines()
        return tail[-1] if tail else f"exit {self.code}"


def config_dir_refusal() -> str | None:
    """Refuse while CLAUDE_CONFIG_DIR is set — at all, even to empty.

    Setting the machine login means writing the MACHINE store. If this process
    inherited a CLAUDE_CONFIG_DIR, the child would write a slot instead and
    silently do something other than what was asked, to a store the operator
    was not thinking about. Presence is the test, not truthiness: an empty
    value is still a value someone set on purpose.
    """
    if "CLAUDE_CONFIG_DIR" in os.environ:
        value = os.environ.get("CLAUDE_CONFIG_DIR") or "(empty)"
        return (
            "CLAUDE_CONFIG_DIR is set in this environment "
            f"({value}), so this shell is already pointed at a particular "
            "account store. Setting the machine login from here would write "
            "the wrong store. Start a shell without it and try again."
        )
    return None


async def _run(argv: list[str], timeout: float, *, stdin: int | None = None) -> Outcome:
    """Await a child. Never blocks the event loop."""
    try:
        proc = await asyncio.create_subprocess_exec(
            *argv,
            stdin=stdin,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            env=PATHS.child_env(),
            cwd=str(PATHS.threads_home),
        )
    except FileNotFoundError:
        return Outcome(False, 127, refusal=f"not found: {argv[0]}")
    try:
        out, err = await asyncio.wait_for(proc.communicate(), timeout=timeout)
    except asyncio.TimeoutError:
        try:
            proc.kill()
        except Exception:
            pass
        return Outcome(False, -1, refusal=f"timed out after {timeout:.0f}s")
    return Outcome(
        proc.returncode == 0,
        proc.returncode or 0,
        out.decode("utf-8", "replace"),
        err.decode("utf-8", "replace"),
    )


async def preflight(label: str) -> Outcome:
    """Dry-run first. The real thing rewrites the machine's login; asking the
    tool what it WOULD do costs one round trip and is the difference between a
    confirmation the operator can act on and one they have to guess at."""
    refusal = config_dir_refusal()
    if refusal:
        return Outcome(False, 1, refusal=refusal)
    result = await _run(
        [str(PATHS.bash_tool), "set-live-login", label, "--dry-run"], PREFLIGHT_TIMEOUT
    )
    if result.ok and ALREADY_LIVE_MARKER in result.stdout:
        result.already_live = True
        return result
    if result.ok and DRY_RUN_MARKER not in result.stdout:
        return Outcome(
            False,
            result.code,
            result.stdout,
            result.stderr,
            refusal=(
                "the preflight exited cleanly but never reached its dry-run stop "
                "point, so it did not actually check anything — refusing to go on"
            ),
        )
    return result


async def record_already_live(label: str) -> Outcome:
    """The preflight said the machine login already IS `label`. Run the real
    (non-dry) command once so the tool records `label` as the name for it (the
    live-login pointer) — it stops at the same already-live branch, so there is
    no browser trip and nothing is backed up or rewritten.

    stdin is /dev/null and `--yes` is NOT passed: if the machine login moved in
    the moment between preflight and here, the tool would reach its "type yes"
    confirmation, read EOF, and do nothing — rather than starting a sign-in
    nobody asked for from a child with no terminal.
    """
    refusal = config_dir_refusal()
    if refusal:
        return Outcome(False, 1, refusal=refusal)
    result = await _run(
        [str(PATHS.bash_tool), "set-live-login", label],
        PREFLIGHT_TIMEOUT,
        stdin=asyncio.subprocess.DEVNULL,
    )
    if result.ok and ALREADY_LIVE_MARKER in result.stdout:
        result.already_live = True
        return result
    if result.ok:
        result = Outcome(
            False,
            result.code,
            result.stdout,
            result.stderr,
            refusal=(
                "the machine login was no longer that account when its name was "
                "being recorded — nothing was changed; press s again"
            ),
        )
    return result


def set_live_argv(label: str, *, chrome_profile: str | None = None, no_browser: bool = False) -> list[str]:
    argv = [str(PATHS.bash_tool), "set-live-login", label, "--yes"]
    if no_browser:
        argv.append("--no-browser")
    elif chrome_profile:
        argv += ["--chrome-profile", chrome_profile]
    return argv
