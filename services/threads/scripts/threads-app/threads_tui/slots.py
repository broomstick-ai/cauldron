"""**THE THREE SLOT OPERATIONS: rename, delete, rotate.**

WHY THIS MODULE EXISTS
----------------------
Three daemon methods were on the wire, declared in `threads_tui.caps`, given
`Api` methods — and called by nothing. `accounts/{account}/rename`, `accounts/{account}/delete` and
`accounts/rotate` each had a complete path from this process to the daemon and
no door on this board, so the operator who wanted any of them had to leave the
board and find a terminal. That is the same class of gap `n` closed: the
capability was not missing, the DOOR was.

They are grouped here rather than scattered because the daemon groups them —
`server.rs` calls two of them "account slot operations" and drives them through
the same `accounts::*_refusal` / `*_account` pairs the CLI verbs drive. A module
that mirrors that grouping means a change on either side has one place to land.

WHAT THIS MODULE IS, AND IS NOT
-------------------------------
It is modal screens plus the PURE functions they and the app call to decide
what to say. It holds no policy the daemon holds: it does not decide whether a
name is ultimately acceptable, whether a delete may proceed, or what a rotation
achieved. Every one of those is read back off the daemon's own answer, and
where the daemon refuses, its sentence is shown verbatim because its sentences
name their remedy and a paraphrase loses that.

The local validation that IS here exists for exactly one reason, the same one
`login.NewAccountModal` states: a refusal that arrives after a round trip —
after a directory has moved, after a browser, after fifteen minutes of
rotation — is the failure the validation exists to prevent. So the checks here
MIRROR `accounts::rename_refusal` and are kept in step with it by name.

THE ONE THING THIS BOARD REFUSES TO OFFER: `--force`
----------------------------------------------------
`accounts/{account}/delete` takes a `force` flag, and the daemon uses it to get past one
refusal in particular — deleting the slot of the account the machine login is
currently signed in as. That refusal's own sentence names the remedy
(`claude-accounts set-live-login <other>`), and the consequence of overriding it
is that the account survives only as the machine copy, with no way for the
daemon to spawn as it.

This board never sends `force`. Not as a checkbox, not as a second confirmation,
not on a shifted key. Putting it on the surface would make the fastest path
through the interface the one that leaves the pool a login short, and the
operator would reach it at the exact moment he is already being told not to.
The board shows the refusal and stops; the override lives where it costs a
deliberate trip to a terminal.
"""

from __future__ import annotations

from typing import Any

from textual.app import ComposeResult
from textual.containers import Horizontal, Vertical
from textual.screen import ModalScreen
from textual.widgets import Button, Input, Label, Static

from .login import label_refusal

# `accounts::DEFAULT_ACCOUNT`. It is the MACHINE LOGIN — the store plain
# `claude` reads when CLAUDE_CONFIG_DIR is unset — and it is not a slot. The
# daemon refuses to rename it, to rename onto it, and to delete it.
DEFAULT_ACCOUNT = "default"


# ── RENAME ───────────────────────────────────────────────────────────────────


def rename_refusal(old: str, new: str, existing=()) -> str | None:
    """Why this rename cannot proceed, or None if it can. PURE.

    **MIRRORS `accounts::rename_refusal` (src/accounts.rs), IN ITS ORDER.**
    The order is not decoration: `old == new` and "that name is invalid" can
    both be true at once, and the two implementations must pick the same one to
    say or the local message and the daemon's disagree about the same input.

    The NAME-SHAPE half is not re-implemented — it is `login.label_refusal`,
    which already mirrors `accounts::validate_account_name` and is already
    tested against it. A second validator for the same rule is the drift this
    whole arrangement exists to avoid; there are three implementations of that
    rule in this repo already and a fourth would be one more thing to keep in
    step.

    `existing` is every label registered OR present on disk, which is what the
    daemon computes before it asks: a rename onto an unregistered directory
    would silently merge two slots.
    """
    old = (old or "").strip()
    new = (new or "").strip()

    # 1. The machine login is not a slot, in either direction.
    if old == DEFAULT_ACCOUNT:
        return (
            f"{DEFAULT_ACCOUNT!r} is not an account you can rename — it is the machine "
            "login itself. Press s to point it at a different account, or rename the "
            "account it is signed in as."
        )
    if new == DEFAULT_ACCOUNT:
        return (
            f"{DEFAULT_ACCOUNT!r} is reserved for the machine login and cannot be used "
            "as an account name."
        )

    # 2. The name's SHAPE — charset, length, the reserved `backups` directory.
    #    `existing` is deliberately NOT passed: `label_refusal`'s collision
    #    message is the one for a NEW account ("press l to sign it in again"),
    #    which is the wrong remedy for a rename. The collision is checked below
    #    with a message that fits.
    why = label_refusal(new, empty="type the new name")
    if why:
        return why

    # 3. The rename-specific refusals, in the daemon's order.
    if old == new:
        return "that is already its name"
    names = list(existing)
    if names and not any(n == old for n in names):
        return f"there is no account {old!r}"
    if any(n == new for n in names):
        return f"{new!r} is already taken — pick a name no slot and no directory has"
    return None


def rename_verdict(payload: Any, asked_old: str, asked_new: str) -> tuple[str, str]:
    """What the rename actually did, read off the daemon's answer. PURE.

    **NOTHING HERE IS ASSUMED.** The labels reported are the daemon's
    `old_label` / `new_label`, not the two strings we sent; the credential move
    is its `credential_moved`; and the history sentence is spoken ONLY when the
    payload carries `history_keeps_old_name: true`. A verdict that stated any of
    those from its own variables would be reporting the request as though it
    were the outcome, which is the thing this codebase most consistently refuses
    to do.

    Returns `(line, severity)`.
    """
    if not isinstance(payload, dict):
        return (
            f"{asked_old}: the daemon accepted the rename and answered with nothing "
            f"this board could read, so the outcome is not confirmed; the row "
            f"below corrects itself on the daemon's next push",
            "warn",
        )
    old = str(payload.get("old_label") or asked_old)
    new = str(payload.get("new_label") or asked_new)
    parts = [f"{old} is now {new}"]

    # THE KEYCHAIN. The macOS service name is derived from the slot's directory
    # path, so a rename that moved the directory and NOT the secret leaves a
    # perfectly good grant orphaned under a service name nothing reads — the
    # account silently becomes "not signed in". The daemon reports whether it
    # moved; say so either way rather than assuming the happy one.
    moved = payload.get("credential_moved")
    if moved is True:
        parts.append("its keychain item moved with it")
    elif moved is False:
        parts.append(
            "the daemon reports NO keychain item was moved — if it stops being "
            "able to sign in, press l to log it in again"
        )

    # THE SURPRISING PART, and it is surprising enough that the daemon puts it
    # in the payload rather than leaving clients to work it out.
    if payload.get("history_keeps_old_name") is True:
        parts.append(
            f"threads and budget history already recorded against {old!r} KEEP that "
            "name — the history does not move with the slot"
        )
    return ("; ".join(parts), "ok")


class RenameModal(ModalScreen[str | None]):
    """Rename the selected slot. Returns the new name, or None if dismissed.

    Pre-filled with the current name so a long label can be EDITED rather than
    retyped. The consequence is that a stray Enter submits the name it already
    has — which `rename_refusal` catches in this modal with "that is already its
    name", never as a round trip.
    """

    DEFAULT_CSS = """
    RenameModal { align: center middle; }
    RenameModal > Vertical {
        width: 72; height: auto; max-height: 90%;
        border: round #898781; background: #1a1a19; padding: 1 2;
    }
    RenameModal .title { text-style: bold; }
    RenameModal .hint { color: #898781; }
    RenameModal .keep { color: #d8a531; text-style: bold; }
    RenameModal #why { height: auto; color: #d64040; }
    RenameModal Input { margin: 1 0; }
    RenameModal .row { height: auto; align: right middle; }
    """

    BINDINGS = [("escape", "dismiss_none", "cancel")]

    def __init__(self, account: str, existing=()) -> None:
        super().__init__()
        self.account = account
        self.existing = tuple(existing)

    def body_lines(self) -> list[tuple[str, str]]:
        """(text, css class) for every line of the body. PURE.

        Split out for the same reason `DisableConfirm.body_lines` is: the COPY
        is most of what this modal is for, and copy that can only be read by
        standing up a Textual app is copy nobody asserts.
        """
        return [
            (
                "The name moves with the slot: its directory under accounts/, its "
                "keychain item, the manifest and the registry all move together. "
                "Letters, digits, - and _.",
                "hint",
            ),
            # SAID BEFORE, NOT ONLY AFTER. The verdict repeats it off the
            # daemon's payload, but a surprise is cheaper to absorb while he is
            # still deciding than once the directory has moved.
            (
                "Threads and budget history already recorded against this account "
                "KEEP the old name. Renaming does not rewrite history.",
                "keep",
            ),
        ]

    def compose(self) -> ComposeResult:
        with Vertical():
            yield Label(f"Rename — {self.account}", classes="title")
            for text, css in self.body_lines():
                yield Static(text, classes=css)
            yield Input(value=self.account, id="name")
            yield Static("", id="why")
            with Horizontal(classes="row"):
                yield Button("rename", id="ok", variant="success")
                yield Button("cancel", id="cancel")

    def on_mount(self) -> None:
        self.query_one("#name", Input).focus()

    def _submit(self) -> None:
        new = self.query_one("#name", Input).value.strip()
        why = rename_refusal(self.account, new, self.existing)
        if why:
            self.query_one("#why", Static).update(why)
            return
        self.dismiss(new)

    def on_button_pressed(self, event: Button.Pressed) -> None:
        if event.button.id == "ok":
            self._submit()
        else:
            self.dismiss(None)

    def on_input_submitted(self, event: Input.Submitted) -> None:
        self._submit()

    def action_dismiss_none(self) -> None:
        self.dismiss(None)


# ── DELETE ───────────────────────────────────────────────────────────────────


def delete_refusal(label: str, is_machine_default: bool) -> str | None:
    """The ONE delete the board refuses on its own. PURE.

    Everything else is the daemon's call, deliberately. It refuses a slot that
    does not exist and it refuses the live login without `force`, and both of
    its sentences name their remedy — so pre-empting them here would replace a
    precise instruction with a paraphrase of it.

    The machine-default ROW is different: it is not a slot at all, it is this
    laptop's own login folded onto the board, and there is no request that could
    delete it. Sending one would be asking the daemon a question whose answer we
    already have, and the board already refuses `l` on this row for the same
    reason (`do_login`: "that is the machine login; use s to change it").
    """
    if is_machine_default or (label or "").strip() == DEFAULT_ACCOUNT:
        return (
            "that row is the machine login, not an account slot — there is nothing "
            "here that could delete it. Press s to point it at a different account."
        )
    return None


def confirmation_matches(label: str, typed: str) -> bool:
    """Has he typed the account's name, exactly? PURE.

    EXACT, and case-sensitively so. Surrounding whitespace is forgiven because
    it is invisible and is a paste artefact rather than an intent; nothing else
    is. A fuzzy match here would defeat the whole point of asking — the typing
    is not a formality, it is the second, deliberate statement of WHICH account,
    made after the first one was a cursor position.
    """
    return (typed or "").strip() == (label or "").strip() != ""


def delete_verdict(payload: Any, asked: str) -> tuple[str, str]:
    """What the delete actually removed, read off the daemon's answer. PURE.

    Each of the three removals is reported from its own boolean rather than
    inferred from the call having succeeded, because they are genuinely
    independent — a slot can lose its registry entry and keep a keychain item,
    and the operator who is told "deleted" would never look.

    `revoked` is the load-bearing one. The daemon sends `revoked: false` on
    every delete, and that is not a caveat to be tidied away: nothing was
    revoked server-side, the credential still exists wherever else it was
    copied, and an operator who reads "deleted" as "signed out" has a wrong
    model of what he just did.
    """
    if not isinstance(payload, dict):
        return (
            f"{asked}: the daemon accepted the delete and answered with nothing this "
            "board could read, so what was removed is not confirmed; the roster "
            "below corrects itself on the daemon's next push",
            "warn",
        )
    label = str(payload.get("label") or asked)
    removed = [
        name
        for name, key in (
            ("its directory", "dir_removed"),
            ("its keychain item", "keychain_removed"),
            ("its registry entry", "registry_removed"),
        )
        if payload.get(key) is True
    ]
    left = [
        name
        for name, key in (
            ("its directory", "dir_removed"),
            ("its keychain item", "keychain_removed"),
            ("its registry entry", "registry_removed"),
        )
        if payload.get(key) is False
    ]
    line = f"{label}: deleted — removed {', '.join(removed)}" if removed else (
        f"{label}: the daemon reported the delete but removed nothing it names"
    )
    if left:
        line += f". It did NOT remove {', '.join(left)}"
    if payload.get("revoked") is False:
        line += (
            ". Nothing was revoked — this was a LOCAL delete, so the login itself is "
            "still valid anywhere else it was copied"
        )
    severity = "ok" if removed and not left else "warn"
    return line, severity


class DeleteConfirm(ModalScreen[bool]):
    """**Delete the selected slot.** Returns True only after a typed name.

    WHY THE TYPING, AND WHY NOT A SECOND KEYPRESS. Every other confirmation on
    this board is reversible in one more keypress — a disable is undone with
    `d`, a share with `S`. This one is not undoable at all: the directory is
    removed, the keychain item is removed, and the registry entry is gone. A
    confirmation that can be satisfied by the same reflex that opened it
    (press the key, press Enter) protects against nothing, because the reflex
    is what went wrong.

    So the button does not arm until the account's name has been typed. That is
    not friction for its own sake: it is the operator stating WHICH account a
    second time, in a way a cursor position cannot, which is exactly the mistake
    available here — the roster moves under a board that is sitting open.

    WHAT IT SAYS, AND WHAT IT DOES NOT PROMISE. It says the delete is LOCAL and
    revokes nothing, because that is what the daemon's payload says and it is
    the thing most likely to be misread. It does NOT offer `force` — see the
    module docstring.
    """

    DEFAULT_CSS = """
    DeleteConfirm { align: center middle; }
    DeleteConfirm > Vertical {
        width: 72; height: auto; max-height: 90%;
        border: round #d64040; background: #1a1a19; padding: 1 2;
    }
    DeleteConfirm .title { text-style: bold; color: #d64040; }
    DeleteConfirm .hint { color: #898781; }
    DeleteConfirm .keep { color: #d8a531; text-style: bold; }
    DeleteConfirm #why { height: auto; color: #d64040; }
    DeleteConfirm Input { margin: 1 0; }
    DeleteConfirm .row { height: auto; align: right middle; }
    """

    BINDINGS = [("escape", "cancel", "cancel")]

    def __init__(
        self,
        account: str,
        *,
        is_live_login: bool = False,
        disabled: bool = False,
    ) -> None:
        super().__init__()
        self.account = account
        self.is_live_login = is_live_login
        self.disabled = disabled

    def title_line(self) -> str:
        return f"Delete — {self.account}"

    def body_lines(self) -> list[tuple[str, str]]:
        """(text, css class) for every line of the body. PURE."""
        out: list[tuple[str, str]] = [
            (
                "This removes the slot: its directory under accounts/, its keychain "
                "item and its registry entry. It is not undoable from this board and "
                "there is no disabled state to come back from.",
                "hint",
            ),
            (
                "It is LOCAL. Nothing is revoked — the login itself stays valid, and "
                "this board simply stops being able to spawn as it.",
                "keep",
            ),
        ]
        if not self.disabled:
            # The reversible alternative, named at the moment the irreversible
            # one is on screen. He asked for disable precisely because he did not
            # want deletion — "I don't want it to be deleted, though" — so the
            # interface owes him the reminder that the other door exists.
            out.append(
                (
                    "If you only want it out of placement, press escape and then d: "
                    "disabling keeps the row, the readings and the login.",
                    "hint",
                )
            )
        if self.is_live_login:
            # STATED, NOT RULED ON. The daemon decides whether this may proceed
            # and its refusal names the remedy; repeating that remedy here in
            # different words would be this board holding a policy it does not
            # own. All this line does is tell him which row he is on.
            out.append(
                (
                    "This is the account your machine login is currently signed in "
                    "as. The daemon refuses that unless you point the machine login "
                    "elsewhere first; if it does, its refusal will say so here.",
                    "keep",
                )
            )
        out.append(
            (f"Type {self.account} below to confirm.", "hint"),
        )
        return out

    def compose(self) -> ComposeResult:
        with Vertical():
            yield Label(self.title_line(), classes="title")
            for text, css in self.body_lines():
                yield Static(text, classes=css)
            yield Input(placeholder=self.account, id="confirm")
            yield Static("", id="why")
            with Horizontal(classes="row"):
                yield Button("delete", id="go", variant="error", disabled=True)
                yield Button("cancel", id="cancel")

    def on_mount(self) -> None:
        self.query_one("#confirm", Input).focus()

    def on_input_changed(self, event: Input.Changed) -> None:
        # The button ARMS on the match rather than validating on the press, so
        # the interface answers "have I typed it right" while he is typing it.
        self.query_one("#go", Button).disabled = not confirmation_matches(
            self.account, event.value
        )

    def _submit(self) -> None:
        typed = self.query_one("#confirm", Input).value
        if not confirmation_matches(self.account, typed):
            self.query_one("#why", Static).update(
                f"type {self.account} exactly to confirm"
            )
            return
        self.dismiss(True)

    def on_input_submitted(self, event: Input.Submitted) -> None:
        self._submit()

    def on_button_pressed(self, event: Button.Pressed) -> None:
        if event.button.id == "go":
            self._submit()
        else:
            self.dismiss(False)

    def action_cancel(self) -> None:
        self.dismiss(False)


# ── ROTATE ───────────────────────────────────────────────────────────────────

# The outcome words `claude-accounts rotate --json` puts on each slot. They are
# the SCRIPT'S vocabulary and this module does not invent a second one: it
# groups them and names the groups, so an outcome nobody here has heard of is
# still counted and still shown rather than silently dropped.
ROTATE_ROLLED = "rotated"
ROTATE_UNCHANGED = "unchanged"
ROTATE_SKIPPED = "skip"
ROTATE_NEEDS_YOU = "manual"
ROTATE_FAILED = "error"
ROTATE_WOULD = "would-refresh"


def rotate_verdict(payload: Any) -> tuple[str, str]:
    """What the rotation pass achieved, read off `--json`. PURE.

    **THE VERDICT IS THE JSON, NOT THE LOG.** The daemon is emphatic about this
    and so is this function: the `Progress` frames that scrolled past on the
    narration line while the pass ran are NARRATION, and an earlier version of
    this feature elsewhere read outcomes by regex-parsing a shared append-only
    log that a concurrent CLI run could interleave into. Nothing here looks at a
    log line.

    Returns `(line, severity)`.
    """
    if not isinstance(payload, dict):
        return (
            "the rotation pass answered with nothing this board could read, so its "
            "outcome is not known — check `claude-accounts rotate` in a terminal",
            "warn",
        )
    ok = payload.get("ok")
    dry = bool(payload.get("dry_run"))
    result = payload.get("result")
    if not isinstance(result, dict):
        return (
            "the rotation pass returned no result blob, so what it did is not known. "
            "The daemon treats a pass with no parseable JSON as a failure, not as "
            "nothing-to-do",
            "warn",
        )
    slots = result.get("slots")
    if not isinstance(slots, list):
        slots = []

    buckets: dict[str, list[str]] = {}
    for row in slots:
        if not isinstance(row, dict):
            continue
        outcome = str(row.get("outcome") or "?")
        buckets.setdefault(outcome, []).append(str(row.get("label") or "?"))

    head = "REHEARSAL (dry run) — nothing was changed" if dry else "key rotation"
    counted: list[str] = []
    for outcome, word in (
        (ROTATE_ROLLED, "rolled forward"),
        (ROTATE_WOULD, "would refresh"),
        (ROTATE_UNCHANGED, "unchanged"),
        (ROTATE_SKIPPED, "skipped"),
    ):
        if buckets.get(outcome):
            counted.append(f"{len(buckets[outcome])} {word}")
    # Anything the script grew that this module has never heard of is COUNTED
    # under its own word rather than dropped. A client that silently discards an
    # outcome it does not recognise reports a smaller pass than the one that ran.
    for outcome, labels in sorted(buckets.items()):
        if outcome in (
            ROTATE_ROLLED,
            ROTATE_WOULD,
            ROTATE_UNCHANGED,
            ROTATE_SKIPPED,
            ROTATE_NEEDS_YOU,
            ROTATE_FAILED,
        ):
            continue
        counted.append(f"{len(labels)} {outcome}")

    parts = [head]
    parts.append(", ".join(counted) if counted else f"{len(slots)} slot(s) reported")

    # THE TWO THAT NEED HIM ARE NAMED, never counted. A number tells him
    # something is wrong and a label tells him which key to press next.
    severity = "ok"
    needs = buckets.get(ROTATE_NEEDS_YOU) or []
    failed = buckets.get(ROTATE_FAILED) or []
    if needs:
        parts.append(
            f"NEEDS A SIGN-IN: {', '.join(needs)} — select each row and press l"
        )
        severity = "warn"
    if failed:
        parts.append(f"FAILED: {', '.join(failed)}")
        severity = "warn"
    if ok is False:
        severity = "warn"
        parts.append("the pass itself reported failure")

    # THE LIVE LOGIN. The script watches its own identity across the pass and
    # says so in one sentence. `untouched` is the boring case; anything else is
    # the operator's own sessions moving underneath it, and IDENTITY CHANGED is
    # the one outcome the script says must never happen.
    live = result.get("live_login")
    if isinstance(live, str) and live.strip():
        if live.startswith("IDENTITY CHANGED"):
            return (
                f"live login: {live}. Rotation reported: {'; '.join(parts)}",
                "warn",
            )
        if not live.startswith("untouched"):
            parts.append(f"live login: {live}")
    return "; ".join(parts), severity


class RotateConfirm(ModalScreen[str | None]):
    """**Refresh every slot's credential.** Returns "go", "dry", or None.

    IT IS NOT A PER-ACCOUNT OPERATION AND THIS MODAL SAYS SO. `accounts/rotate`
    takes no account — `server.rs` reads only `dry_run` from its params — and it
    runs `claude-accounts rotate`, which walks every registered slot. A modal
    opened from a selected row that then acted on the whole pool would be the
    interface implying a scope the wire does not have, so the selection is not
    mentioned and the sentence says "every".

    THE REHEARSAL IS OFFERED FIRST. `--dry-run` invokes `claude` for nothing and
    changes no credential, which makes it the only way to find out what a pass
    WOULD do without doing it. It is a real distinction on the daemon's side
    too: a dry run deliberately does not ring `account_picture_changed`, so it
    is indistinguishable from outside except by what it says.
    """

    DEFAULT_CSS = """
    RotateConfirm { align: center middle; }
    RotateConfirm > Vertical {
        width: 74; height: auto; max-height: 90%;
        border: round #898781; background: #1a1a19; padding: 1 2;
    }
    RotateConfirm .title { text-style: bold; }
    RotateConfirm .hint { color: #898781; }
    RotateConfirm .keep { color: #d8a531; text-style: bold; }
    RotateConfirm .row { height: auto; align: right middle; }
    """

    BINDINGS = [("escape", "cancel", "cancel")]

    def __init__(self, slots: int | None = None) -> None:
        super().__init__()
        self.slots = slots

    def title_line(self) -> str:
        return "Rotate keys — every slot"

    def body_lines(self) -> list[tuple[str, str]]:
        """(text, css class) for every line of the body. PURE."""
        scope = (
            f"all {self.slots} accounts on this board"
            if isinstance(self.slots, int) and self.slots > 0
            else "every registered account"
        )
        return [
            (
                f"A rotation pass runs a free `claude /usage` probe under {scope}, "
                "one at a time, so each slot's own store refreshes its own token. "
                "There is no per-account rotation: the daemon's method takes no "
                "account and this runs the whole pool.",
                "hint",
            ),
            (
                "It can take minutes. The board stays live and the ⚡ line narrates "
                "each slot as the daemon reaches it; the verdict is read from the "
                "pass's own result when it finishes, not from the log.",
                "hint",
            ),
            (
                "A rehearsal invokes nothing and changes no credential. It is the "
                "only way to see what a pass would do without doing it.",
                "keep",
            ),
        ]

    def compose(self) -> ComposeResult:
        with Vertical():
            yield Label(self.title_line(), classes="title")
            for text, css in self.body_lines():
                yield Static(text, classes=css)
            with Horizontal(classes="row"):
                yield Button("rehearse (dry run)", id="dry")
                yield Button("rotate now", id="go", variant="primary")
                yield Button("cancel", id="cancel")

    def on_button_pressed(self, event: Button.Pressed) -> None:
        if event.button.id in ("go", "dry"):
            self.dismiss(event.button.id)
        else:
            self.dismiss(None)

    def action_cancel(self) -> None:
        self.dismiss(None)
