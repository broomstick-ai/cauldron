"""Logging an account in. Two paths, and the fallback is the one built first.

THE TWO PATHS
-------------
1. THE TERMINAL HANDOFF (`run_terminal_login`). Suspends the UI, hands the
   terminal to `claude-accounts login`, and lets `claude` print its own URL and
   read its own code. This is what works TODAY and it is deliberately the path
   that was built and tested first, so there has never been a moment when the
   modal was the only way in. It also stays as the fallback for anything the
   modal does not handle.

2. THE MODAL (`LoginModal`). What was actually asked for, in these words:

       "I just need the text user interface to give me a copy button. If I
        click on the URL, just put the whole URL inside of the button. And when
        I click on it, it'll copy. And then I can go to the browser and then
        give me a field to paste in the return key."

   So: the whole URL rendered as a clickable button that copies, then a field
   to paste the returned code. That is the requirement, and this is that — not
   a state machine with retries, polling and reconciliation. It needs
   `accounts/login/begin` (to get the URL) and `accounts/login/{session_id}/complete` (to
   hand back the code); until both are on the wire, path 1 is used and the
   board says why.

THE DEFECT THIS FIXES
---------------------
The old flow ran the login through a blocking `subprocess.run` followed by a
blocking `input()`, from inside an asyncio task. `App.suspend()` restores the
terminal but does NOT stop the event loop — so the loop was frozen for the
entire browser round trip plus however long the operator took to press Enter.

The consequence was not just a frozen UI. The WebSocket client is configured
with `ping_interval=20, ping_timeout=20`; a blocked loop cannot answer a ping,
so any login taking ~40s or more had its connection torn down underneath it.
That is why logging in ended with the board reading "reconnecting" over stale
data, and it is the real reason the flow "didn't work that well".

Both blocking calls are gone. The child is awaited with
`asyncio.create_subprocess_exec`, and the final keypress is read through a
thread executor. The loop keeps turning, so the socket keeps answering pings,
so the board is still connected when the login returns.

THE SECOND DEFECT: THE BUTTON THAT SAID "COPIED" AND HAD NOT
------------------------------------------------------------
Reported twice. The modal called `App.copy_to_clipboard(url)` and then wrote
"copied to the clipboard" unconditionally. Nothing had been copied.

Textual 8.2.8 emits a BARE `ESC ] 52 ; c ; <base64> BEL`. He runs this in
Textual -> tmux -> iTerm2, and tmux only honours an application's OSC 52 when
`set-clipboard` is `on`; his is `external` (the default), under which tmux
DISCARDS it and forwards nothing. The sequence never reached iTerm2. That is
measured against a real tmux under a real pty in `tests/test_clipboard.py`, not
inferred.

`threads_tui.clipboard` is the fix, ported from `human_actions/clipboard.py` —
the interface he named. pbcopy carries the claim, verified by reading it back
with pbpaste; OSC 52 is still sent for the SSH case, now wrapped in tmux's DCS
passthrough so it actually leaves the pane; and the word "copied" is now spoken
only when the pasteboard was read back and matched.

There is also a THIRD path out of this screen that needs no clipboard at all —
"show in terminal", plus a spill file written on mount. Signing in must not
depend on a feature that has already failed twice.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass

from textual.app import ComposeResult
from textual.containers import Horizontal, Vertical
from textual.screen import ModalScreen
from textual.widgets import Button, Input, Label, Static

from . import clipboard
from .endpoint import PATHS
from .fmt import AMBER, GREEN, INK, INK2, MUTED


@dataclass
class LoginChoice:
    label: str
    email: str | None = None
    browser: str = "private"  # private | chrome | none
    chrome_profile: str | None = None


def terminal_login_argv(choice: LoginChoice, *, new: bool = False) -> list[str]:
    """The argv for the terminal handoff.

    SANCTIONED SUBPROCESS (2 of 3). This is a fallback that exists only because
    `accounts/login/begin` / `complete` are not on the wire yet. It retires the
    day they land — see `threads_tui.caps`.
    """
    argv = [str(PATHS.bash_tool), "login", choice.label]
    if choice.email:
        argv += ["--email", choice.email]
    if choice.browser == "none":
        argv.append("--no-browser")
    elif choice.browser == "chrome" and choice.chrome_profile:
        argv += ["--chrome-profile", choice.chrome_profile]
    if not new:
        # Re-logging in an existing slot: the tool asks for confirmation
        # otherwise, and the operator has already confirmed in the UI.
        argv += ["--force", "--allow-same-account"]
    return argv


async def run_terminal_login(app, argv: list[str], banner: str) -> int:
    """Hand the terminal over, and DO NOT BLOCK THE EVENT LOOP doing it.

    The `with app.suspend()` block is synchronous, but everything inside it is
    awaited. That is the whole fix: the loop keeps running, so the WebSocket
    keeps answering pings, so the connection survives a slow login.
    """
    loop = asyncio.get_running_loop()
    with app.suspend():
        print("\n" + banner)
        print("─" * 72)
        try:
            proc = await asyncio.create_subprocess_exec(
                *argv, env=PATHS.child_env(), cwd=str(PATHS.threads_home)
            )
        except FileNotFoundError:
            print(f"  could not run {argv[0]}")
            await loop.run_in_executor(None, input, "  press Enter to return to the board ")
            return 127
        code = await proc.wait()
        print("─" * 72)
        # `input()` is blocking, so it runs on a thread. Called directly it
        # would freeze the loop for as long as the operator took to react —
        # which is unbounded, and was the worse half of the original defect.
        await loop.run_in_executor(
            None, input, f"  done (exit {code}). Press Enter to return to the board. "
        )
    return code


class LoginModal(ModalScreen[str | None]):
    """The copy button and the paste field. Exactly what was asked for.

    Returns the pasted code, or None if dismissed.
    """

    DEFAULT_CSS = """
    LoginModal {
        align: center middle;
    }
    LoginModal > Vertical {
        width: 84;
        height: auto;
        max-height: 90%;
        border: round #898781;
        background: #1a1a19;
        padding: 1 2;
    }
    LoginModal .title { text-style: bold; }
    LoginModal #urlbutton {
        width: 100%;
        height: auto;
        border: round #3987e5;
        padding: 1 1;
        margin: 1 0;
        color: #ffffff;
    }
    LoginModal #urlbutton:hover { border: round #ffffff; background: #23231f; }
    LoginModal #copied { height: auto; color: #0ca30c; }
    LoginModal #copied.unverified { color: #d98c00; }
    LoginModal #copied.failed { color: #d64040; }
    LoginModal #spill { height: auto; color: #898781; }
    LoginModal Input { margin: 1 0; }
    LoginModal .row { height: auto; align: right middle; }
    LoginModal .hint { color: #898781; }
    """

    BINDINGS = [("escape", "dismiss_none", "cancel")]

    def __init__(self, account: str, url: str) -> None:
        super().__init__()
        self.account = account
        self.url = url
        self._spill_path = None

    def compose(self) -> ComposeResult:
        with Vertical():
            yield Label(f"Sign in — {self.account}", classes="title")
            yield Label(
                "Click the URL to copy it, open it in your browser, then paste "
                "the code it gives back. If the copy will not land, press "
                "\"show in terminal\" — that never needs a clipboard.",
                classes="hint",
            )
            # THE BUTTON, holding the WHOLE url, as asked. It is a Static rather
            # than a Button widget for one reason: a Button renders its label on
            # a single line and a sign-in URL is far too long for that, so the
            # middle of the URL — the part that makes it unmistakably the right
            # link — would be the part that got cut. This wraps and shows all of
            # it, and clicking it copies.
            yield Static(self.url, id="urlbutton")
            yield Static("", id="copied")
            # Written on mount, BEFORE any clipboard is attempted, so the URL is
            # reachable from a second window even if every copy path fails.
            yield Static("", id="spill")
            yield Input(placeholder="paste the code here", id="code")
            with Horizontal(classes="row"):
                yield Button("copy URL", id="copy", variant="primary")
                yield Button("show in terminal", id="show")
                yield Button("sign in", id="submit", variant="success")
                yield Button("cancel", id="cancel")

    def on_mount(self) -> None:
        self.query_one("#code", Input).focus()
        # The clipboard-free path, established up front rather than as a
        # consolation prize after a failure. The login must not be reachable
        # only through a feature that has now failed twice.
        self._spill_path = clipboard.spill(self.url, f"signin-{self.account}")
        if self._spill_path is not None:
            self.query_one("#spill", Static).update(
                f"also written to {self._spill_path}"
            )

    def _status(self, text: str, css_class: str = "") -> None:
        node = self.query_one("#copied", Static)
        node.set_classes(css_class)
        node.update(text)

    def _emit_osc52(self, text: str) -> bool:
        """Send OSC 52 through TEXTUAL'S OWN WRITER, not a second one.

        The driver serialises everything it puts on the terminal. Writing this
        sequence independently could land in the middle of one of Textual's,
        which corrupts the screen — so we hand the bytes to the same writer and
        let it order them. What we do NOT use is `App.copy_to_clipboard`: that
        emits the bare sequence his tmux throws away.

        False under a headless driver: there is no terminal, so there is nothing
        to claim.
        """
        app = self.app
        if app is None or app.is_headless:
            return False
        driver = getattr(app, "_driver", None)
        if driver is None:
            return False
        return clipboard.emit_osc52(text, write=driver.write)

    def _copy(self) -> None:
        """Copy the URL, and say ONLY what we actually observed.

        The mechanism and the honesty contract are ported from
        `human_actions/clipboard.py` — the interface the operator named as the
        one that solved this. pbcopy carries the claim because it is the only
        one that can be read back; OSC 52 is still sent (it is what survives
        SSH), now wrapped for tmux, but it may never say "copied".

        The old line said "copied to the clipboard" unconditionally, after an
        OSC 52 that his tmux was discarding. That sentence is why he believed
        it had worked.
        """
        result = clipboard.copy_text(
            self.url, f"signin-{self.account}", osc52=self._emit_osc52
        )
        if result.verified:
            self._status(result.message)
        elif result.ok:
            self._status(result.message, "unverified")
        else:
            self._status(result.message, "failed")
        if result.path is not None:
            self._spill_path = result.path
            self.query_one("#spill", Static).update(f"also written to {result.path}")

    async def _show_in_terminal(self) -> None:
        """The path that does not touch the clipboard at all.

        Drops out of Textual, prints the URL as ordinary terminal output, and
        waits. Ordinary output is selectable with the mouse in iTerm2 the normal
        way, and it is also visible to tmux's own copy-mode — neither of which
        depends on anything this app emits.
        """
        loop = asyncio.get_running_loop()
        with self.app.suspend():
            print("\n  Sign-in URL for " + self.account + ":\n")
            print("  " + self.url + "\n")
            if self._spill_path is not None:
                print(f"  (also at {self._spill_path})\n")
            print("  Select it with the mouse, or copy it out of the file above.")
            await loop.run_in_executor(
                None, input, "  Press Enter to return and paste the code. "
            )

    def on_click(self, event) -> None:
        widget = getattr(event, "widget", None)
        if widget is not None and widget.id == "urlbutton":
            self._copy()

    def on_button_pressed(self, event: Button.Pressed) -> None:
        if event.button.id == "copy":
            self._copy()
        elif event.button.id == "show":
            self.run_worker(self._show_in_terminal(), exclusive=True)
        elif event.button.id == "submit":
            self._submit()
        else:
            self.dismiss(None)

    def on_input_submitted(self, event: Input.Submitted) -> None:
        self._submit()

    def _submit(self) -> None:
        code = self.query_one("#code", Input).value.strip()
        if not code:
            self._status("paste the code first", "failed")
            return
        self.dismiss(code)

    def action_dismiss_none(self) -> None:
        self.dismiss(None)


# ── naming a NEW account ─────────────────────────────────────────────────────

# Mirrors `accounts::validate_account_name` (src/accounts.rs:1507) and
# `valid_label` in scripts/claude-accounts. Kept in step with BOTH, because the
# name is used three ways downstream and each one has its own opinion: it
# becomes a DIRECTORY under accounts/, a TOML table key in accounts.toml, and
# part of the keychain service name. A name this modal accepts and `add-account`
# then rejects is the worst outcome available here — the operator has already
# opened a browser and signed in by the time the script refuses.
MAX_LABEL = 64
RESERVED_LABELS = ("default", "backups")


def label_refusal(
    label: str, existing=(), *, empty: str = "type a name for the new account"
) -> str | None:
    """Why this name cannot be used for a NEW account, or None if it can.

    Pure, so the test can walk every branch without a screen. The refusals are
    phrased as the remedy rather than the rule — "use letters, digits…" tells
    him what to type, where "invalid label" tells him only that he is wrong.

    `empty` exists so that `slots.rename_refusal` can REUSE this rather than
    grow a second copy of `accounts::validate_account_name`. There are already
    three implementations of that rule in this repo (here, `accounts.rs`, and
    `valid_label` in scripts/claude-accounts) and the cost of them drifting is
    paid at the worst possible moment; a fourth, differing only in one sentence
    about an empty field, would be the cheapest possible way to make that worse.
    Everything except that sentence is shared.
    """
    label = label.strip()
    if not label:
        return empty
    if len(label) > MAX_LABEL:
        return f"too long — {MAX_LABEL} characters at most (this is {len(label)})"
    if not all(c.isascii() and (c.isalnum() or c in "-_") for c in label):
        bad = sorted({c for c in label if not (c.isascii() and (c.isalnum() or c in "-_"))})
        shown = " ".join(repr(c) for c in bad)
        return f"use letters, digits, - and _ only — remove {shown}"
    if label in RESERVED_LABELS:
        if label == "default":
            return (
                "'default' is the machine login, not a slot. Press s on the board "
                "to change which account plain `claude` signs in as."
            )
        return f"{label!r} is reserved — accounts/{label}/ holds the live-login undo snapshots"
    if label in set(existing):
        # NOT a silent re-login. `add-account` would happily sign this slot in
        # again — its own law is that prior state never refuses a login — but
        # doing that from a door labelled "new account" is a surprise, and the
        # board already has the right door for it on the row itself.
        return f"{label!r} already exists — select its row and press l to sign it in again"
    return None


class NewAccountModal(ModalScreen[str | None]):
    """Name a NEW account slot. Returns the label, or None if dismissed.

    **THE MISSING DOOR.** Every other account operation on this board acts on a
    row that already exists, so each one could take its subject from the
    selection. Creating an account is the one act with no row to select, and it
    was therefore the one act with no key — the operator had to leave the board
    and run `claude-accounts login <name>` in a terminal.

    Nothing else was missing. `accounts/login/begin` on a label with no slot
    already creates the account end to end: `add-account --login` makes the slot
    directory and, once the grant lands, runs `threads accounts adopt`. So this
    modal is the whole feature — a text field and the validation that keeps a
    refusal from arriving AFTER a browser round trip.
    """

    DEFAULT_CSS = """
    NewAccountModal { align: center middle; }
    NewAccountModal > Vertical {
        width: 72; height: auto; max-height: 90%;
        border: round #898781; background: #1a1a19; padding: 1 2;
    }
    NewAccountModal .title { text-style: bold; }
    NewAccountModal .hint { color: #898781; }
    NewAccountModal #why { height: auto; color: #d64040; }
    NewAccountModal Input { margin: 1 0; }
    NewAccountModal .row { height: auto; align: right middle; }
    """

    BINDINGS = [("escape", "dismiss_none", "cancel")]

    def __init__(self, existing=()) -> None:
        super().__init__()
        self.existing = tuple(existing)

    def compose(self) -> ComposeResult:
        with Vertical():
            yield Label("New account", classes="title")
            yield Label(
                "A name for the slot — it becomes the directory under accounts/ "
                "and the name this board shows. Letters, digits, - and _.\n"
                "Next you will sign in to it in a browser; it gets its own "
                "login and shares nothing with your other accounts.",
                classes="hint",
            )
            yield Input(placeholder="e.g. work-2", id="label")
            yield Static("", id="why")
            with Horizontal(classes="row"):
                yield Button("create and sign in", id="ok", variant="success")
                yield Button("cancel", id="cancel")

    def on_mount(self) -> None:
        self.query_one("#label", Input).focus()

    def _submit(self) -> None:
        label = self.query_one("#label", Input).value.strip()
        why = label_refusal(label, self.existing)
        if why:
            self.query_one("#why", Static).update(why)
            return
        self.dismiss(label)

    def on_button_pressed(self, event: Button.Pressed) -> None:
        if event.button.id == "ok":
            self._submit()
        else:
            self.dismiss(None)

    def on_input_submitted(self, event: Input.Submitted) -> None:
        self._submit()

    def action_dismiss_none(self) -> None:
        self.dismiss(None)
