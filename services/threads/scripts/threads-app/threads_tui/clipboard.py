"""clipboard.py — put a block of text where the operator can actually get at it.

PORTED, ON PURPOSE, FROM THE ONE THAT WORKS
-------------------------------------------
This is `human_actions/clipboard.py` from ~/code/human-actions, reused rather
than reinvented, because the operator named it: "Look at the human actions
harness and the text user interface there. They solved copying. Please do it
the same way here." The contract, the three mechanisms, the `CopyResult` shape
and the honesty rule are all carried over unchanged.

ONE THING IS ADDED, AND IT IS THE BUG THAT BROUGHT US HERE
----------------------------------------------------------
The previous version of this screen called `App.copy_to_clipboard(url)` and
then told the operator "copied to the clipboard". It had not copied anything.

Textual 8.2.8 emits a BARE OSC 52:

    app.py:1786    self._driver.write(f"\\x1b]52;c;{base64_text}\\a")

The operator runs this app in **Textual → tmux → iTerm2**, and his tmux is
configured `set-clipboard external` (~/.tmux.conf sets `allow-passthrough on`;
`set-clipboard` is left at tmux's default, which is `external`). tmux only
honours an application's OSC 52 when `set-clipboard` is `on`; under `external`
it drops it on the floor and forwards nothing. So the sequence never reached
iTerm2 at all. Measured, not assumed — see `tests/test_clipboard.py`, which
runs a real tmux under a real pty and reads the outer side:

    set-clipboard=external  bare       -> OSC 52 reached outer terminal: False
    set-clipboard=external  DCS-wrapped-> OSC 52 reached outer terminal: True

Hence `emit_osc52` here does NOT go through `App.copy_to_clipboard`. It writes
the sequence itself and, when `$TMUX` is set, wraps it in tmux's DCS
passthrough — `ESC P tmux; <sequence, with every ESC doubled> ESC \\` — which
tmux forwards verbatim regardless of `set-clipboard`.

WHICH MECHANISM ACTUALLY CARRIES THE CLAIM
-------------------------------------------
`pbcopy`, verified by reading it straight back with `pbpaste`. That is the one
the human-actions tool is really resting on, and it is the one that works on
this machine today no matter what tmux and iTerm2 do, because the copying is
done by a subprocess talking to the macOS pasteboard — nothing traverses the
terminal at all.

OSC 52 is still sent, because it is the only mechanism that survives SSH (the
terminal emulator on his desk does the copying) and because it gives ⌘C parity.
But **OSC 52 can never be read back**: we can observe that we wrote the bytes,
never that iTerm2 accepted them (it needs its own preference enabled). So a
copy resting only on OSC 52 is reported as *sent, unverified* — never as
*copied*. That is the rule the old screen broke.

Nothing in here ever reports a success it has not observed.
"""

from __future__ import annotations

import base64
import os
import re
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional

# Test seam, as in the original: write the spill file but launch nothing.
NO_OPEN_ENV = "THREADS_TUI_NO_OPEN"


def default_spill_dir() -> Path:
    """Where a copy goes when there is no working clipboard.

    Deliberately NOT under `threads_home` — that is the repo, and an operator's
    sign-in URL has no business being written into a working tree.
    """
    base = os.environ.get("XDG_CACHE_HOME") or str(Path.home() / ".cache")
    return Path(base) / "threads-tui" / "copy"


@dataclass
class CopyResult:
    """What actually happened. ``ok`` is only True for something observed.

    ``mechanism`` names the one the message's claim rests on:

    ``clipboard``  pbcopy, verified by a pbpaste readback — the only one we call
                   "copied".
    ``terminal``   OSC 52 went out and nothing else worked. Whether the terminal
                   honoured it is unknowable from here, and the message says so.
    ``file``       spilled to a file (and usually opened for him).
    ``none``       nothing happened.

    ``osc52_emitted`` is orthogonal: it records that the escape sequence was
    written, whichever mechanism ended up carrying the claim.
    """

    ok: bool
    mechanism: str               # "clipboard" | "terminal" | "file" | "none"
    message: str                 # operator-facing, one line
    path: Optional[Path] = None  # set when a spill file was written
    osc52_emitted: bool = False

    @property
    def verified(self) -> bool:
        """True only when the text was read back off the pasteboard.

        The UI is allowed to say the word "copied" on this and on nothing else.
        """
        return self.ok and self.mechanism == "clipboard"


# ── mechanism 1: pbcopy, verified ──────────────────────────────────────────

def clipboard_available() -> bool:
    """True iff both halves of the macOS clipboard pair are on PATH. ``pbpaste``
    is required as well as ``pbcopy`` — without it a copy could not be verified,
    and an unverifiable copy is not one this module is willing to claim."""
    return bool(shutil.which("pbcopy") and shutil.which("pbpaste"))


def write_clipboard(text: str) -> bool:
    """pbcopy the text, then pbpaste it back and compare. Returns True only on a
    matching readback."""
    if not clipboard_available():
        return False
    try:
        subprocess.run(["pbcopy"], input=text.encode("utf-8"), check=True, timeout=10)
        back = subprocess.run(["pbpaste"], capture_output=True, check=True, timeout=10)
    except (OSError, subprocess.SubprocessError):
        return False
    got = back.stdout.decode("utf-8", "replace")
    # Exact match, or match once a trailing newline difference is discounted —
    # some clipboard managers normalise the final newline, which is not data loss.
    return got == text or got.rstrip("\n") == text.rstrip("\n")


# ── mechanism 2: OSC 52, with the tmux wrap Textual does not do ────────────

def osc52_sequence(text: str, *, in_tmux: bool) -> str:
    """The bytes to write. Split out so the transport test can assert on them.

    Bare:      ESC ] 52 ; c ; <base64> BEL
    In tmux:   ESC P tmux ; <the above, every ESC doubled> ESC \\

    The doubling is tmux's escaping rule for passthrough payloads; without it
    tmux terminates the DCS at the first inner ESC and the OSC never lands.
    """
    payload = base64.b64encode(text.encode("utf-8")).decode("ascii")
    seq = f"\x1b]52;c;{payload}\a"
    if in_tmux:
        return "\x1bPtmux;" + seq.replace("\x1b", "\x1b\x1b") + "\x1b\\"
    return seq


def emit_osc52(text: str, *, write=None, stream=None) -> bool:
    """Write the OSC 52 sequence to the real terminal. True iff the bytes went out.

    Note what this does NOT do: call `App.copy_to_clipboard`. That method emits
    the BARE form, which his tmux discards (see the module docstring). We build
    the sequence ourselves so the tmux wrap is actually applied.

    `write` MATTERS, and it is not a stylistic hook. Textual's Unix driver owns
    the terminal through a WriterThread on `sys.__stderr__`. A second writer
    pushing bytes at the same tty from another thread can land INSIDE one of
    Textual's own escape sequences, which corrupts the screen rather than the
    clipboard. So the caller passes `App._driver.write` — the same serialised
    path Textual itself uses — and only a caller with no driver at all falls
    back to writing the stream directly.

    Returns False when there is nowhere real for the bytes to go, because
    claiming otherwise would be exactly the unobserved success this module
    refuses.
    """
    seq = osc52_sequence(text, in_tmux=bool(os.environ.get("TMUX")))
    if write is not None:
        try:
            write(seq)
        except Exception:  # noqa: BLE001 — a clipboard must never take the TUI down
            return False
        return True

    out = stream if stream is not None else sys.__stdout__
    if out is None:
        return False
    try:
        if not out.isatty():
            return False
        out.write(seq)
        out.flush()
    except Exception:  # noqa: BLE001
        return False
    return True


# ── mechanism 3: the spill file ────────────────────────────────────────────

def _slug(label: str) -> str:
    s = re.sub(r"[^a-z0-9]+", "-", (label or "copy").lower()).strip("-")
    return (s or "copy")[:60]


def _spill(text: str, label: str, spill_dir: Path) -> Path:
    spill_dir = Path(spill_dir)
    spill_dir.mkdir(parents=True, exist_ok=True)
    path = spill_dir / f"{_slug(label)}.txt"
    path.write_text(text)
    return path


def spill(text: str, label: str, spill_dir: Path | None = None) -> Path | None:
    """Write the text to a file and hand back the path, or None if that failed.

    Called on its own by the login screen, up front, so the URL is reachable
    from a second window BEFORE any clipboard has been attempted. The login must
    not be reachable only through a feature that has already failed twice.
    """
    try:
        return _spill(text, label, spill_dir or default_spill_dir())
    except OSError:
        return None


# ── the whole ladder ───────────────────────────────────────────────────────

def copy_text(
    text: str,
    label: str,
    spill_dir: Path | None = None,
    osc52: Optional[Callable[[str], bool]] = None,
) -> CopyResult:
    """Deliver ``text`` to the operator so he can paste it elsewhere.

    Both real mechanisms are attempted: ``pbcopy`` (verified by a ``pbpaste``
    readback) and OSC 52. The message names the one the claim rests on and never
    overstates the unverifiable one:

    * pbcopy verified → "copied", and OSC 52 is mentioned only as also-sent;
    * pbcopy not verified but OSC 52 sent → "sent … unverified", plus the spill
      file as the escape hatch;
    * neither → spill the text to a file and open it, as the original does.
    """
    if not text.strip():
        return CopyResult(False, "none", "nothing to copy — this block is empty")

    spill_dir = spill_dir or default_spill_dir()
    emitter = osc52 if osc52 is not None else emit_osc52

    pb_ok = write_clipboard(text)
    try:
        sent = bool(emitter(text))
    except Exception:  # noqa: BLE001
        sent = False

    if pb_ok:
        also = " · also sent to the terminal (OSC 52)" if sent else ""
        return CopyResult(
            True, "clipboard",
            f"copied: {label} — verified by reading it back off the pasteboard; "
            f"press ⌘V to paste{also}",
            osc52_emitted=sent,
        )

    # pbcopy is out. Write the spill file either way — it costs nothing and is
    # the one artefact that is definitely there.
    path: Optional[Path] = None
    spill_error: Optional[str] = None
    try:
        path = _spill(text, label, spill_dir)
    except OSError as exc:
        spill_error = str(exc)

    if sent:
        where = f"; a copy is also saved at {path}" if path else ""
        return CopyResult(
            True, "terminal",
            f"SENT to your terminal (OSC 52) but NOT VERIFIED — try ⌘V. This one "
            f"cannot be read back, so we cannot tell you it worked (the macOS "
            f"pasteboard was unavailable){where}",
            path=path, osc52_emitted=True,
        )

    if path is None:
        return CopyResult(
            False, "none",
            f"could not copy, and the fallback file could not be written: {spill_error}",
        )

    opened = False
    if not os.environ.get(NO_OPEN_ENV):
        try:
            subprocess.run(["open", "-e", str(path)], check=True, timeout=10)
            opened = True
        except (OSError, subprocess.SubprocessError):
            opened = False

    if opened:
        return CopyResult(
            True, "file",
            f"could not copy — opened it in TextEdit instead, select it there: {path}",
            path=path,
        )
    return CopyResult(
        False, "file",
        f"could not copy and TextEdit would not open — the text is at {path}",
        path=path,
    )
