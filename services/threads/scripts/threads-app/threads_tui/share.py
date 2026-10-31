"""The share editor.

The pool share is the per-account limit: the fraction of the weekly pool an
account is expected to consume. It is the DENOMINATOR of pressure — pressure is
"how much of your share is unused, relative to how much of the window is left" —
so it is the single number that most changes how the pressure view reads, and
until now it had no surface at all. The setter existed in the store and was
reachable from nothing.

**A SHARE OF 0% IS NO LONGER THE DISABLE, AND IS NO LONGER A LEGAL SHARE.**
That doctrine shipped on 2026-09-08 and was overturned by the operator: *"I want
disabled to be independent of share. Whether to include it in the pool supersedes
how much should be made available to the pool."* Membership now lives in its own
table (`account_disabled`), a disable never touches `pool_shares`, and
`Store::set_pool_share` is bound back to `(0.0, 1.0]` — it REFUSES a zero and its
refusal names the other door.

So this editor refuses it too, in the same words and one round trip earlier. The
cost of the old design is the reason the refusal is worth writing out rather than
just returning: the capability shipped behind a magic value in a number field,
and the operator's own row for `alice-example-com` still read `1.0e-05` four days
later — the 0.001% he tried before there was any other door. A number field with
a state hidden in one of its values is how that happens. The state has a key now,
and the refusal names it.

VALIDATION HAPPENS TWICE, ON PURPOSE. The daemon validates and will refuse an
out-of-range value; this checks locally as well. That is not redundancy for its
own sake — a local check turns a typo into an instant, specific message instead
of a round trip that returns a refusal the operator then has to interpret. The
daemon stays the authority: if it refuses a value this accepted, its refusal is
what gets shown, verbatim.

Shares are ENTERED as percentages, because that is how the board displays them
and how the operator talks about them, and SENT as fractions, because that is
what the API takes. The conversion happens in exactly one place: `parse_share`.
"""

from __future__ import annotations

from textual.app import ComposeResult
from textual.containers import Horizontal, Vertical
from textual.screen import ModalScreen
from textual.widgets import Button, Input, Label, Static

# The daemon admits `(0.0, 1.0]` — zero EXCLUDED. Mirrored here so a typo is
# caught before it becomes a round trip.
MIN_SHARE = 0.0
MAX_SHARE = 1.0


def parse_share(text: str) -> tuple[float | None, str]:
    """Percent string in, fraction out. Pure. Returns (value, error).

    Accepts `25`, `25%`, `0.25` — but note that `0.25` is read as a QUARTER OF
    A PERCENT, not as a quarter, because the field is documented as percent and
    silently switching units based on magnitude is how someone sets a share to
    a hundredth of what they meant. The one exception is refused explicitly
    rather than guessed at.
    """
    raw = (text or "").strip().rstrip("%").strip()
    if not raw:
        return None, "enter a percentage"
    try:
        pct = float(raw)
    except ValueError:
        return None, f"{text!r} is not a number"
    if pct != pct or pct in (float("inf"), float("-inf")):
        return None, "not a finite number"
    share = pct / 100.0
    if share < MIN_SHARE:
        return None, "a share cannot be negative — press d to take it out of the pool"
    # ZERO IS REFUSED, AND THE REFUSAL NAMES THE OTHER DOOR. It is refused
    # because the daemon refuses it — `set_pool_share` is bound to `(0.0, 1.0]`
    # — and the sentence names `d` because the thing he is reaching for when he
    # types a zero is membership, which is a different act with its own key.
    # Saying only "not allowed" is what sent him to 0.001% the first time.
    if share == MIN_SHARE:
        return None, (
            "0% is not a share — press d to take this account out of the pool "
            "instead; its share is kept while it is out"
        )
    if share > MAX_SHARE:
        return None, "a share cannot exceed 100% of the pool"
    return share, ""


def fmt_share_pct(share: float | None) -> str:
    if share is None:
        return ""
    return f"{share * 100:g}"


def share_state(share: float | None) -> str:
    """The share as text, for anywhere it is shown beside other figures.

    IT NO LONGER ANSWERS `DISABLED` FOR A ZERO. It used to, and had to: a zero
    WAS the state, so rendering it as a number near other numbers was rendering
    a state as a magnitude. Membership is now its own fact on the wire and no
    share value carries it — so a share that somehow reads zero is a
    pre-migration relic or out-of-band nonsense, and calling it "DISABLED" would
    be this client re-deriving a state the daemon stopped encoding there. The
    daemon made the same narrowing in `pool_share_row`'s display bound.
    """
    if share is None:
        return "—"
    return f"{share * 100:g}%"


class ShareEditor(ModalScreen[float | None]):
    """Set one account's pool share. Returns the new fraction, or None."""

    DEFAULT_CSS = """
    ShareEditor { align: center middle; }
    ShareEditor > Vertical {
        width: 64; height: auto;
        border: round #898781; background: #1a1a19; padding: 1 2;
    }
    ShareEditor .title { text-style: bold; }
    ShareEditor .hint { color: #898781; }
    ShareEditor .disabled { color: #d8a531; text-style: bold; }
    ShareEditor #err { height: auto; color: #d03b3b; }
    ShareEditor .row { height: auto; align: right middle; }
    """

    BINDINGS = [("escape", "cancel", "cancel")]

    def __init__(
        self,
        account: str,
        current: float | None,
        total_known: float | None = None,
        *,
        disabled: bool = False,
    ) -> None:
        super().__init__()
        self.account = account
        self.current = current
        self.total_known = total_known
        # Membership is READ here and never SET here. The editor says what the
        # number it is editing will and will not do while the account is out of
        # the pool; the act itself belongs to `d`.
        self.disabled = disabled

    def compose(self) -> ComposeResult:
        with Vertical():
            yield Label(f"Pool share — {self.account}", classes="title")
            yield Static(
                "The fraction of the weekly pool this account is expected to "
                "use. It is the denominator of pressure: raising it lowers this "
                "account's pressure at the same usage, and vice versa.",
                classes="hint",
            )
            # SAY WHERE MEMBERSHIP LIVES, at the moment he is deciding what to
            # type. This sentence used to say "enter 0 to disable"; the two acts
            # have been separated and it would now walk him into a refusal.
            yield Static(
                "This is ALLOCATION, not membership. Taking the account out of "
                "the pool is a different act with its own key — press d — and "
                "the share set here is kept while it is out, so there is nothing "
                "to restore when you put it back. 0% is not a share and is "
                "refused.",
                classes="hint",
            )
            if self.disabled:
                yield Static(
                    "This account is DISABLED right now. Changing its share does "
                    "NOT put it back — press d for that. The number set here is "
                    "what it will be on when you do.",
                    classes="disabled",
                )
            if self.current is None:
                yield Static(
                    "The API did not report a current share for this account, so "
                    "it is not shown. It is not assumed to be 100%.",
                    classes="hint",
                )
            yield Input(
                value=fmt_share_pct(self.current),
                placeholder="percent of the pool, e.g. 25",
                id="share",
            )
            yield Static("", id="err")
            with Horizontal(classes="row"):
                yield Button("set", id="set", variant="success")
                yield Button("cancel", id="cancel")

    def on_mount(self) -> None:
        self.query_one("#share", Input).focus()

    def on_button_pressed(self, event: Button.Pressed) -> None:
        if event.button.id == "set":
            self._submit()
        else:
            self.dismiss(None)

    def on_input_submitted(self, event: Input.Submitted) -> None:
        self._submit()

    def _submit(self) -> None:
        value, error = parse_share(self.query_one("#share", Input).value)
        if value is None:
            self.query_one("#err", Static).update(error)
            return
        self.dismiss(value)

    def action_cancel(self) -> None:
        self.dismiss(None)


class ReactivateConfirm(ModalScreen[bool]):
    """**Take this account out of the pool, or put it back.** Returns True to act.

    RENAMED FROM `DisableConfirm` (2026-09-22), and the rename is the point
    rather than a tidy-up. The other direction used to be called ENABLE and was
    a RESTORATION: a disable was a share of zero, so the act destroyed the
    account's allocation and the daemon kept a memo (`prior_share`) so that an
    enable could put it back. The operator overturned that model — membership is
    now its own fact and nothing is destroyed — so there is nothing to restore,
    and a modal still promising to restore something would be describing
    machinery that no longer exists.

    *"if it's disabled we want to be able to un-disable it, e.g. restore it.
    Maybe we call it reactivate. It can use the same keybind as disable used,
    like a toggle."*

    THE WORD FOR THE STATE IS STILL HIS. *"The text user interface just says,
    like, disabled."* Not `paused`, not `inactive`, not `0%` — one word, on every
    surface that mentions it. `Reactivate` names the ACT in the other direction;
    it does not rename the state.

    WHY A CONFIRMATION AND NOT A BARE TOGGLE. `d` sits one key from `j`, `k` and
    `l` on the row he navigates with, and the two directions are not equally
    harmless: disabling an account he is mid-conversation on moves live work. The
    modal also has a job beyond confirming — it is where the interface can say
    what the act actually DOES, which a keypress cannot, and "it is not deleted
    and the login is untouched" is the sentence he most needs in front of him at
    the moment he presses it.

    WHY IT SHOWS THE SHARE. Not as a promise about what will be handed back — as
    a statement that nothing was taken. The number is the account's CURRENT
    share, read off the same `share` cell the card draws, and it is true in both
    directions and at every moment: before the disable, during it, and after the
    reactivation. That is exactly why the two fields that used to answer this
    question are gone.
    """

    DEFAULT_CSS = """
    ReactivateConfirm { align: center middle; }
    ReactivateConfirm > Vertical {
        width: 66; height: auto;
        border: round #898781; background: #1a1a19; padding: 1 2;
    }
    ReactivateConfirm .title { text-style: bold; }
    ReactivateConfirm .hint { color: #898781; }
    ReactivateConfirm .keep { color: #d8a531; text-style: bold; }
    ReactivateConfirm .row { height: auto; align: right middle; }
    """

    BINDINGS = [("escape", "cancel", "cancel")]

    def __init__(
        self,
        account: str,
        currently_disabled: bool,
        share: float | None = None,
    ) -> None:
        super().__init__()
        self.account = account
        self.currently_disabled = currently_disabled
        # THE ACCOUNT'S SHARE, not a restore target. One field, one meaning; see
        # the class docstring and `model.Account`'s note where `restore_to` was.
        self.share = share

    def share_sentence(self) -> str:
        """What the account's allocation is, and that the act does not move it.

        NEVER guesses a number. A share of None means the daemon reported no
        share row — an ordinary state now that a disable no longer mints an
        override — and the sentence says what is true without one rather than
        defaulting to 100%, which is the one direction in which a wrong guess
        costs real capacity.
        """
        if self.share is None:
            return (
                "Its share is whatever the pool declares for it; no override is "
                "set, and this does not change that either way."
            )
        pct = f"{self.share * 100:g}%"
        if self.currently_disabled:
            return f"It is still on {pct}, and that is what it will be placed at."
        return f"It keeps its {pct} share the whole time it is out."

    def title_line(self) -> str:
        verb = "Reactivate" if self.currently_disabled else "Disable"
        return f"{verb} — {self.account}"

    def body_lines(self) -> list[tuple[str, str]]:
        """(text, css class) for every line of the body. PURE.

        Split out of `compose` deliberately. The copy in this modal is the whole
        point of the modal — it is where the interface answers *"I don't want it
        to be deleted"* — so it has to be assertable without standing up a
        Textual app, which is the thing that made the previous version of this
        untestable. `compose` renders these and adds nothing of its own.
        """
        if self.currently_disabled:
            return [
                (
                    "This account is DISABLED. Reactivating puts it straight back "
                    "into placement selection, on the login it already has — no "
                    "sign-in, nothing to re-authorise, and none of its history "
                    "is touched.",
                    "hint",
                ),
                (self.share_sentence(), "hint"),
            ]
        return [
            (
                "It leaves placement selection entirely: no new turn is put on "
                "it, and work already running there finishes where it is. "
                "Nothing lifts this on a timer — it stays disabled until you "
                "press d again.",
                "hint",
            ),
            # THE SENTENCE HE ASKED FOR, said at the moment he is deciding.
            # *"I don't want it to be deleted, though. I still want to see it
            # there as one of my accounts, because if I turn it back on, I'm
            # going to keep using the same login."*
            (
                "The account is NOT deleted and the login is NOT touched. It "
                "stays on this board with all its readings, and reactivating it "
                "later uses the same login.",
                "keep",
            ),
            (self.share_sentence(), "hint"),
        ]

    def compose(self) -> ComposeResult:
        with Vertical():
            yield Label(self.title_line(), classes="title")
            for text, css in self.body_lines():
                yield Static(text, classes=css)
            with Horizontal(classes="row"):
                yield Button(
                    "reactivate" if self.currently_disabled else "disable",
                    id="go",
                    variant="primary",
                )
                yield Button("cancel", id="cancel")

    def on_button_pressed(self, event: Button.Pressed) -> None:
        self.dismiss(event.button.id == "go")

    def action_cancel(self) -> None:
        self.dismiss(False)
