"""`threads_tui.clipboard` against the REAL transports.

THE POINT OF THIS FILE
----------------------
A unit test asserting "we wrote the right bytes" is exactly the test that would
have passed while the operator could not paste. The failure was in the
TRANSPORT, not the string. So the two checks that matter here are the two that
exercise a real transport:

  * pbcopy is written and then READ BACK with pbpaste. That is the mechanism the
    fix rests on, and it is provable end to end on this machine.
  * a REAL tmux is run under a REAL pty, OSC 52 is emitted from inside a pane,
    and the OUTER side is read to see what came through. That reproduces the
    bug: under `set-clipboard external` — the tmux default, and what the
    operator is running — the bare sequence Textual writes never leaves tmux,
    and the DCS-wrapped one does.

The tmux check leads with a FORCED FAILURE: it first asserts that the OLD
behaviour is genuinely broken. A green on the fix means something only because
the same harness shows red on what shipped.

WHAT CANNOT BE PROVEN HERE, AND WHY
-----------------------------------
Whether iTerm2 ACCEPTS the OSC 52 once it arrives. That depends on iTerm2's own
"Applications in terminal may access clipboard" preference; it is a decision
made by a GUI process on the far side of the pty; and OSC 52 has no reply, so
the protocol gives the sender nothing to observe. No test on this machine can
close that gap — only pressing Cmd-V can. That is exactly why OSC 52 is not
allowed to carry the word "copied" and why pbcopy, which CAN be read back, is
the mechanism the claim rests on.

Run: .venv/bin/python tests/test_clipboard.py   (from threads-app/)
"""

from __future__ import annotations

import base64
import io
import os
import pty
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from threads_tui import clipboard  # noqa: E402

FAILURES: list[str] = []
PROBE = "https://claude.ai/oauth/authorize?code=1&probe=TRANSPORT"


def check(name: str, ok: bool, detail: str = "") -> None:
    print(f"  {'ok  ' if ok else 'FAIL'}  {name}" + (f"   [{detail}]" if detail and not ok else ""))
    if not ok:
        FAILURES.append(name)


def skip(name: str, why: str) -> None:
    print(f"  skip  {name}   [{why}]")


# ── the sequence itself ────────────────────────────────────────────────────

def test_sequences() -> None:
    print("\nthe bytes\n")

    bare = clipboard.osc52_sequence("hi", in_tmux=False)
    check("the bare form matches what Textual emits (or we test a different bug)",
          bare == "\x1b]52;c;" + base64.b64encode(b"hi").decode() + "\a",
          repr(bare))

    wrapped = clipboard.osc52_sequence("hi", in_tmux=True)
    check("the tmux form is a DCS passthrough",
          wrapped.startswith("\x1bPtmux;") and wrapped.endswith("\x1b\\"), repr(wrapped))
    inner = wrapped[len("\x1bPtmux;"):-2]
    # tmux terminates the DCS at the first un-doubled inner ESC. If this
    # regresses the passthrough truncates silently and nothing lands.
    check("every inner ESC is doubled", inner.replace("\x1b\x1b", "").count("\x1b") == 0)
    check("the doubled payload still carries the OSC 52", "\x1b\x1b]52;c;" in inner)


# ── mechanism 1: the one that carries the claim ────────────────────────────

def test_pbcopy() -> None:
    print("\npbcopy, verified by reading it back\n")
    if not clipboard.clipboard_available():
        skip("pbcopy round trip", "no pbcopy/pbpaste on PATH")
        return

    saved = subprocess.run(["pbpaste"], capture_output=True).stdout
    try:
        check("write_clipboard reports success", clipboard.write_clipboard(PROBE) is True)
        back = subprocess.run(["pbpaste"], capture_output=True, check=True).stdout
        check("the text is really on the pasteboard — read back, not assumed",
              back.decode() == PROBE, repr(back[:80]))

        result = clipboard.copy_text(PROBE, "probe", osc52=lambda _t: False)
        check("a verified copy is the one thing allowed to say 'copied'",
              result.verified and result.mechanism == "clipboard"
              and "copied" in result.message, result.message)
    finally:
        subprocess.run(["pbcopy"], input=saved)


# ── the honesty contract, under FORCED failure of the verified path ────────

def test_honesty() -> None:
    print("\nwhat the screen is allowed to claim when the verified path fails\n")

    real = clipboard.write_clipboard
    tmp = Path(tempfile.mkdtemp(prefix="threads-copy-"))
    try:
        clipboard.write_clipboard = lambda _t: False  # type: ignore[assignment]

        # This is the operator's exact case: no pasteboard, OSC 52 went out.
        # If this ever says "copied", the original bug is back.
        r = clipboard.copy_text(PROBE, "probe", tmp, osc52=lambda _t: True)
        check("OSC 52 alone is 'terminal', never 'clipboard'", r.mechanism == "terminal")
        check("OSC 52 alone is NOT verified — the UI keys off this", r.verified is False)
        check("the message says NOT VERIFIED out loud", "NOT VERIFIED" in r.message, r.message)
        check("the spill file is written anyway",
              r.path is not None and r.path.read_text() == PROBE)

        os.environ[clipboard.NO_OPEN_ENV] = "1"
        r2 = clipboard.copy_text(PROBE, "probe", tmp, osc52=lambda _t: False)
        check("nothing worked -> ok is False", r2.ok is False and r2.verified is False)
        check("the text is still on disk", r2.path is not None and r2.path.read_text() == PROBE)

        r3 = clipboard.copy_text("   ", "probe", tmp, osc52=lambda _t: True)
        check("an empty payload is refused, not 'copied'",
              r3.ok is False and r3.mechanism == "none")
    finally:
        clipboard.write_clipboard = real  # type: ignore[assignment]
        os.environ.pop(clipboard.NO_OPEN_ENV, None)
        shutil.rmtree(tmp, ignore_errors=True)

    check("a non-tty stream reports False rather than an unobserved success",
          clipboard.emit_osc52(PROBE, stream=io.StringIO()) is False)


# ── mechanism 2: the actual transport, through a real tmux ────────────────

def _osc52_through_tmux(set_clipboard: str, wrapped: bool) -> bool:
    """Run tmux under a pty we own — standing in for iTerm2 — emit OSC 52 from
    inside a pane, and report whether it reached the OUTER side."""
    conf = Path(tempfile.gettempdir()) / f"threads-osc52-{os.getpid()}.tmux.conf"
    conf.write_text(f"set -g set-clipboard {set_clipboard}\nset -g allow-passthrough on\n")
    sock = f"tosc{int(time.time() * 1000) % 100000}{'w' if wrapped else 'b'}"
    seq = clipboard.osc52_sequence(PROBE, in_tmux=wrapped)
    prog = f"import sys,time;sys.stdout.write({seq!r});sys.stdout.flush();time.sleep(1)"

    pid, fd = pty.fork()
    if pid == 0:  # child: becomes tmux, talking to the pty we hold
        os.environ["TERM"] = "xterm-256color"
        os.execvp("tmux", ["tmux", "-f", str(conf), "-L", sock,
                           "new-session", "-A", "-s", sock,
                           sys.executable, "-c", prog])
    out = b""
    deadline = time.time() + 8
    try:
        while time.time() < deadline:
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                break
            if not chunk:
                break
            out += chunk
    finally:
        subprocess.run(["tmux", "-L", sock, "kill-server"], capture_output=True)
        os.close(fd)
        conf.unlink(missing_ok=True)
    return b"]52;" in out


def test_tmux_transport() -> None:
    print("\ndoes the sequence actually leave the tmux pane?\n")
    if shutil.which("tmux") is None:
        skip("tmux transport", "tmux not installed")
        return

    # THE FORCED FAILURE. This is the shipped behaviour, and it must be red.
    check("BARE OSC 52 (what Textual writes) is EATEN by tmux at set-clipboard=external "
          "— this is the reported bug, reproduced",
          _osc52_through_tmux("external", wrapped=False) is False)

    check("DCS-WRAPPED OSC 52 reaches the outer terminal at set-clipboard=external "
          "— this is the fix",
          _osc52_through_tmux("external", wrapped=True) is True)

    # The wrap must not break the configuration that already worked.
    check("DCS-WRAPPED OSC 52 also survives set-clipboard=on",
          _osc52_through_tmux("on", wrapped=True) is True)


# ── the SCREEN, driven for real ───────────────────────────────────────────

async def _drive_modal() -> tuple[str, bool, bool]:
    """Push the real LoginModal, press the real copy button through a real
    Button.Pressed message, and read the real pasteboard back."""
    from textual.app import App, ComposeResult
    from textual.widgets import Button, Static
    from threads_tui.login import LoginModal

    class Host(App):
        def compose(self) -> ComposeResult:
            yield Static("host")

    app = Host()
    async with app.run_test() as pilot:
        modal = LoginModal("probe-account", PROBE)
        app.push_screen(modal)
        await pilot.pause()
        spilled = modal._spill_path is not None and modal._spill_path.read_text() == PROBE
        modal.query_one("#copy", Button).press()
        await pilot.pause()
        status = str(modal.query_one("#copied", Static).content)
        back = subprocess.run(["pbpaste"], capture_output=True).stdout.decode()
        return status, back == PROBE, spilled


def test_modal() -> None:
    print("\nthe login screen itself, end to end\n")
    if not clipboard.clipboard_available():
        skip("modal copy", "no pbcopy/pbpaste on PATH")
        return
    import asyncio

    saved = subprocess.run(["pbpaste"], capture_output=True).stdout
    try:
        status, on_board, spilled = asyncio.run(_drive_modal())
    finally:
        subprocess.run(["pbcopy"], input=saved)

    check("the URL is spilled to a file ON MOUNT — the clipboard-free path exists "
          "before any copy is attempted", spilled)
    check("pressing the copy button puts the URL on the REAL pasteboard", on_board)
    check("and only then does the screen say 'copied'",
          "copied" in status and "NOT VERIFIED" not in status, status)


def test_zz_every_check_in_this_module_actually_held() -> None:
    """**THE GUARD THAT MAKES EVERY `check()` ABOVE MEAN SOMETHING UNDER PYTEST.**

    `check()` RECORDS a failure and returns. That is right for the standalone run
    below, where `main()` prints every result and then exits non-zero on
    `FAILURES` — but this module is ALSO collected by pytest, and under pytest a
    test function that records and returns is a test function that PASSES. All
    five `test_*` functions in this file call `check()` and nothing else, so
    twenty-one probes were green under pytest no matter what they found.

    `test_feed.py` and `test_pressure_view.py` already carry this guard; this
    file was the third module with the same shape and the only one without it,
    which is why the class was reported closed while it was still open here.

    pytest runs the functions in a module in DEFINITION ORDER, so this one —
    named to sort and written last — sees everything the others recorded.

    THE MUTATION, AND IT WAS RUN. Corrupting the OSC 52 selector in
    `clipboard.osc52_sequence` (`]52;c;` -> `]52;X;`) gives:

        with this guard      1 failed, 5 passed   (this function, naming the checks)
        without this guard   6 passed             <- the silent green it closes
        standalone tier      exit 1               (always worked)

    THE LIMIT, stated so it is not mistaken for more: running a single test by
    node id does not run this guard. A full-module or full-suite run does, which
    is both tiers.
    """
    assert FAILURES == [], FAILURES


def main() -> int:
    test_sequences()
    test_pbcopy()
    test_honesty()
    test_tmux_transport()
    test_modal()

    print()
    if FAILURES:
        print(f"{len(FAILURES)} FAILED: {', '.join(FAILURES)}")
        return 1
    print("all clipboard checks passed")
    print("NOT covered, and uncoverable from here: whether iTerm2 accepts the OSC 52 "
          "once it arrives. Only Cmd-V can say. pbcopy is why that does not block him.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
