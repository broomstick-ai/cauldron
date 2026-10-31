"""THE KEY SEQUENCE A DEAD ACCOUNT OFFERS MUST ACTUALLY WORK.

A dead row on this board says *"<label> ... — press 3, then L, to sign it in"*.
The operator pressed exactly that and NOTHING HAPPENED. That is the worst
failure this interface has, because it is the path he reaches for when an
account is ALREADY dead and he is already locked out.

There were two independent causes, and this module pins both so neither can
come back:

  1. THERE WERE NO DIGIT BINDINGS ON THIS BOARD AT ALL. The hint named a key
     nothing listened for. (The sibling `claude-accounts` board did bind 1-9,
     which is why the same hint half-worked there and the failure looked
     inconsistent between the two.)

  2. `L` WAS NOT BOUND — only lowercase `l`. Textual matches binding keys by
     exact string with no case folding, so shift+L matched nothing, found no
     `key_upper_l` handler either, and was dropped in silence.

And one latent trap that the fix had to avoid rather than introduce: the number
PRINTED on a row is 1-based, while `App.selected` is a 0-based list index.
Binding the digit straight to the index would have selected the row below the
one he pointed at — a wrong action instead of no action, which is worse.

WHY THIS TESTS THE BINDING TABLE AND `select_number` RATHER THAN DRIVING A LIVE
APP. The failure was never in the action bodies — `do_login` was correct all
along and has no dead-account gate. It was that the keypress never REACHED
them. So the thing worth asserting is the wiring: that the key exists, and that
the number resolves to the row it is drawn on.

AND SINCE 2026-10-06 THIS FILE CARRIES A SECOND SUBJECT: that the board has NO
refresh key and NO poll key, pinned as an EXACT binding table. That is not a
cleanup assertion — the absence of a refresh key is a claim about the producer,
and the long argument for it sits above that section rather than here, because
the next person to want the key back needs to meet the argument where the test
stops them.

Run: .venv/bin/python tests/test_keys.py   (from threads-app/)
"""

from __future__ import annotations

import sys

from threads_tui.app import BoardScreen
from threads_tui.model import Account, Board

FAILURES: list[str] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    if ok:
        print(f"  ok   {name}")
    else:
        print(f"  FAIL {name}  {detail}")
        FAILURES.append(name)


def binding_keys() -> dict[str, str]:
    """{key: action} for every binding the board declares."""
    out: dict[str, str] = {}
    for b in BoardScreen.BINDINGS:
        if isinstance(b, tuple):
            out[b[0]] = b[1]
        else:
            out[b.key] = b.action
    return out


print("== the keys the dead-row hint names actually exist ==")
KEYS = binding_keys()

# The letter. BOTH cases sign in: a lowercase `l` is near-indistinguishable
# from `I`/`1` in a terminal font, so reading it as "L" is the correct human
# response to what is on screen.
check("lowercase l signs in", KEYS.get("l") == "login", f"got {KEYS.get('l')!r}")
check(
    "UPPERCASE L signs in — this is the one that silently did nothing",
    KEYS.get("L") == "login",
    f"got {KEYS.get('L')!r}",
)

# The digits. Every number a row can be drawn with must be bound.
for n in range(1, 10):
    check(
        f"digit {n} selects a row",
        KEYS.get(str(n)) == f"select({n})",
        f"got {KEYS.get(str(n))!r}",
    )

# The handler the digits route to has to exist, or the binding is a no-op that
# looks bound.
check(
    "BoardScreen.action_select exists",
    callable(getattr(BoardScreen, "action_select", None)),
)


print("== the digit selects the row it is PRINTED on (the off-by-one) ==")


class FakeApp:
    """Just enough App to exercise `select_number` without a running terminal."""

    def __init__(self, board: Board) -> None:
        self.board = board
        self.selected = 0
        self.redrawn = 0

    def redraw(self) -> None:
        self.redrawn += 1

    # `load_extras` STOOD HERE: selecting a card used to FETCH its 7-day history.
    # Since R2.S3d the history is the `accounts/gauge-history` fold, subscribed
    # per card and drawn at paint time, so selection triggers no fetch and the
    # double needs no method for one.

    # The method under test, taken from the real class so this cannot drift.
    select_number = None  # replaced below


def account(number: int, label: str, state: str) -> Account:
    """A real `Account`, built through the dataclass constructor.

    Deliberately not `__new__` + attribute pokes: `health_note` is a PROPERTY
    derived from `state`, so a hand-stuffed instance cannot produce the hint
    string this test exists to read.
    """
    return Account(number=number, label=label, state=state)


from threads_tui.app import BoardApp  # noqa: E402

FakeApp.select_number = BoardApp.select_number

# Four rows, numbered 1..4 as they are drawn. Row 3 is the DEAD one — exactly
# the shape of the board he was looking at.
rows = [
    account(1, "alice-example-com", "live"),
    account(2, "owner-gmail", "live"),
    account(3, "owner-account", "wiped"),
    account(4, "spare", "live"),
]
board = Board.__new__(Board)
board.accounts = rows
board.warn_below_days = 7.0

app = FakeApp(board)

# THE OPERATOR'S ACTUAL KEYSTROKE. "Press 3" must land on the third row —
# index 2 — not on index 3, which is the next account down.
ok = app.select_number(3)
check("pressing 3 returns True (something was selected)", ok is True)
check(
    "pressing 3 selects the row PRINTED as 3, not the one after it",
    app.selected == 2,
    f"selected index {app.selected}, expected 2",
)
check(
    "and that row is the DEAD account the hint was about",
    board.accounts[app.selected].label == "owner-account",
    f"got {board.accounts[app.selected].label!r}",
)

# The last row is the one a naive `selected = number` gets most wrong.
app.select_number(4)
check(
    "pressing 4 selects the last row rather than falling off the end",
    app.selected == 3,
    f"selected index {app.selected}",
)

app.select_number(1)
check("pressing 1 selects the first row", app.selected == 0, f"got {app.selected}")

# A digit naming no row is a NO-OP, not an arbitrary move. Pressing 7 on a
# four-account board must leave the selection alone.
before = app.selected
moved = app.select_number(7)
check("a digit naming no row returns False", moved is False)
check(
    "a digit naming no row does not move the selection",
    app.selected == before,
    f"selection moved to {app.selected}",
)


print("== the hint on a dead row names keys that exist ==")
# Close the loop: take the string the model actually renders and check every
# key it tells him to press is bound. This is what failed in the field — the
# prose and the binding table disagreed and nothing noticed.
board.warn_below_days = 7.0

hint = None
for severity, text in Board.warnings(board):
    if "owner-account" in text:
        hint = text
        break

check("a dead row produces a sign-in hint", hint is not None, f"got {hint!r}")
if hint:
    print(f"       hint reads: {hint!r}")
    check("the hint names the row's number", " 3," in hint or "press 3" in hint, hint)
    check(
        "the hint names a key that is actually bound",
        ("L" in hint and "L" in KEYS) or ("then l" in hint and "l" in KEYS),
        hint,
    )
    # The specific regression: the hint must not name a key with no binding.
    for token in ("then L", "then l"):
        if token in hint:
            key = token.split()[-1]
            check(
                f"the hint's {key!r} is bound",
                key in KEYS,
                f"hint says {token!r} but {key!r} is not in the binding table",
            )



print("== THE FORCING FUNCTION: there is no refresh key and no poll key ==")
# ─────────────────────────────────────────────────────────────────────────────
# WHY THIS SECTION EXISTS, AND WHY IT IS NOT A CLEANUP TEST.
#
# The board used to carry two keys: `R` ("poll accounts"), which made the daemon
# run a real `claude /usage` against every login, and a hidden `r` ("re-read"),
# which forced a re-subscribe so the daemon would re-export every key. The
# operator's instruction was not "delete a key":
#
#     "also 'R = poll accounts' should not just be retired and removed from
#      view, it should be the forcing function to prevent ever needing this."
#
# THE ABSENCE OF A REFRESH KEY IS A CLAIM ABOUT THE PRODUCER. A refresh key is
# the interface conceding that it does not trust what it is showing — and, worse,
# making that distrust comfortable. With the key there, a stale screen costs the
# operator one keystroke; he gets his number and moves on, and the defect that
# made the screen stale is never reported and therefore never fixed. The escape
# hatch does not reduce staleness bugs. It PRESERVES them, by making them
# survivable. Take the hatch away and any staleness becomes an unacceptable,
# visible defect that has to be fixed AT THE PRODUCER, because nothing the
# operator can press will paper over it any more.
#
# SO: IF THIS BOARD EVER LOOKS STALE, THE FIX IS IN THE PRODUCER — the daemon
# must push every change to every fold it holds, and must make its own usage
# readings on its own schedule. Adding a key back here would not fix a stale
# board. It would only stop anyone noticing that the board is stale, which is
# precisely the failure this guard exists to prevent. If you are reading this
# because you wanted the key back, that itch IS the signal: file the producer
# bug instead.
#
# WHY A POSITIVE ASSERTION RATHER THAN A BAN. A ban ("no binding whose action is
# `probe`") goes silent the moment someone spells the same key `refresh`,
# `reload`, `poll` or `sync`. The exact-set assertion below fails on ANY change
# to the binding table, including a key nobody thought to forbid. It is
# deliberately annoying to a legitimate new binding: adding one means editing
# this set, which is a prompt to read the paragraphs above.
#
# WHY THIS CANNOT WITNESS ITSELF. The witness is the imported `BINDINGS` table
# and real module attributes — never the text of a source file — so there is no
# needle this file's own prose could satisfy. The one text-derived check below
# reads STRING LITERALS out of `threads_tui/*.py` via `ast`, which is immune to
# comments by construction and never reads `tests/`, where this guard lives.
# ─────────────────────────────────────────────────────────────────────────────

EXPECTED_BINDINGS = {
    "q": "quit_app",
    "p": "pressure",
    "l": "login",
    "L": "login",
    "n": "new_account",
    "N": "new_account",
    "s": "set_live",
    "S": "share",
    "d": "disable",
    "D": "disable",
    "m": "rename",
    "M": "rename",
    "X": "delete",
    "x": "delete_hint",
    "O": "rotate",
    "o": "rotate_hint",
    "j": "down",
    "k": "up",
    "down": "down",
    "up": "up",
    "exclamation_mark": "drag_note",
    **{str(n): f"select({n})" for n in range(1, 10)},
}

check(
    "the board's binding table is EXACTLY the expected set — no refresh key, no "
    "poll key, and no new key that slipped in unread",
    KEYS == EXPECTED_BINDINGS,
    f"unexpected={ {k: v for k, v in KEYS.items() if EXPECTED_BINDINGS.get(k) != v} } "
    f"missing={ {k: v for k, v in EXPECTED_BINDINGS.items() if KEYS.get(k) != v} }",
)

# THE TWO KEYS BY NAME. Redundant against the exact set above on purpose: when
# someone re-adds `R`, the exact-set failure reports a diff, while these two say
# WHICH key came back and point at the argument. A diff is evidence; a sentence
# is the reason.
check("`R` is not bound to anything", "R" not in KEYS, f"R -> {KEYS.get('R')!r}")
check("`r` is not bound to anything", "r" not in KEYS, f"r -> {KEYS.get('r')!r}")

# AND NO HANDLER TO REBIND TO. A key is one line; a key plus a handler is two.
# Removing the handlers is what makes the deletion a decision rather than a
# cosmetic unbinding, so the handlers are asserted gone independently.
for handler in ("action_probe", "action_resync"):
    check(
        f"BoardScreen.{handler} does not exist",
        getattr(BoardScreen, handler, None) is None,
        f"{handler} is still defined and is one Binding away from being live",
    )

for method in ("do_refresh", "do_resync"):
    check(
        f"BoardApp.{method} does not exist",
        getattr(BoardApp, method, None) is None,
        f"{method} is still defined",
    )

# `resync()` IS THE CONTROL, AND IT MUST STILL EXIST. It is how a MUTATION
# settles — disable, share, login, rename and rotate each await it so the board
# shows the state the daemon actually reached. It is caused by an operator act
# with a known expected effect, not by doubt about the screen, and no key invokes
# it. Without this assertion the four checks above are all satisfied by deleting
# the settle path too, which would break every mutation on the board.
check(
    "but `BoardApp.resync` SURVIVES — a mutation must still settle",
    callable(getattr(BoardApp, "resync", None)),
    "resync is the post-mutation settle, not a refresh path; it must not be "
    "collateral damage of removing the refresh keys",
)

# THE CLIENT HAS NO WAY TO ASK AN ACCOUNT FOR A READING.
from threads_tui import api as api_mod  # noqa: E402
from threads_tui import caps as caps_mod  # noqa: E402

check(
    "Api.gauge_probe does not exist — this client cannot initiate a poll",
    getattr(api_mod.Api, "gauge_probe", None) is None,
)
check(
    "caps.GAUGE_PROBE does not exist — the board names no polling method",
    getattr(caps_mod, "GAUGE_PROBE", None) is None,
)

# THE STRING-LITERAL SWEEP. The attribute checks above are satisfied by a method
# under a different name, so this reads every string literal in the package and
# looks for the wire method itself. `ast` rather than a grep, deliberately: the
# removal notes in `api.py` and `caps.py` mention `gauge/probe` in PROSE, and a
# text search would match those and be red forever. Literals only.
#
# IT READS `threads_tui/` AND NEVER `tests/`, so this file's own paragraphs —
# which say "gauge/probe" repeatedly — cannot satisfy it.
import ast  # noqa: E402
import pathlib  # noqa: E402

PKG = pathlib.Path(__file__).resolve().parents[1] / "threads_tui"


def _docstring_ids(tree: ast.AST) -> set[int]:
    """The `id()` of every DOCSTRING constant in a tree.

    Docstrings are `ast.Constant` strings, so the sweep below would otherwise
    match the removal notes written in `app.py`, `caps.py` and `feed.py`'s module
    docstrings — prose explaining that the call is gone, flagged as the call
    being present. Documentation about a deletion must not read as the deletion
    being undone, so docstrings are excluded exactly as comments already are.
    A string in any OTHER position — an argument, an assignment, a dict value —
    is code, and that is what this is looking for.
    """
    out: set[int] = set()
    for node in ast.walk(tree):
        if isinstance(
            node, (ast.Module, ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)
        ):
            body = getattr(node, "body", None)
            if (
                body
                and isinstance(body[0], ast.Expr)
                and isinstance(body[0].value, ast.Constant)
                and isinstance(body[0].value.value, str)
            ):
                out.add(id(body[0].value))
    return out


offenders: list[str] = []
scanned = 0
for path in sorted(PKG.glob("*.py")):
    scanned += 1
    tree = ast.parse(path.read_text(), filename=str(path))
    skip = _docstring_ids(tree)
    for node in ast.walk(tree):
        if isinstance(node, ast.Constant) and isinstance(node.value, str):
            if id(node) in skip:
                continue
            if "gauge/probe" in node.value:
                offenders.append(f"{path.name}:{node.lineno}")

check(
    "the package scan actually read some files (a sweep over nothing is vacuous)",
    scanned >= 10,
    f"only scanned {scanned} modules",
)
check(
    "no string literal in threads_tui/ names `gauge/probe`",
    not offenders,
    f"named at: {', '.join(offenders)}",
)

# THE SWEEP'S OWN PROOF OF LIFE. An absence check over source is worthless unless
# the needle is one this scanner can actually see, so the same scanner is pointed
# at a literal known to be present. If this goes red the sweep above is broken
# and its green means nothing.
sentinel = []
for path in sorted(PKG.glob("*.py")):
    tree = ast.parse(path.read_text(), filename=str(path))
    skip = _docstring_ids(tree)
    for node in ast.walk(tree):
        if isinstance(node, ast.Constant) and isinstance(node.value, str):
            if id(node) in skip:
                continue
            if node.value == "shapes/list":
                sentinel.append(path.name)
check(
    "and the scanner CAN see a wire method that is still there (`shapes/list`) "
    "— so the absence above is a finding, not a blind spot",
    bool(sentinel),
    "the literal sweep found nothing at all; it is not looking where it thinks",
)

# THE HEADER NO LONGER NAMES A KEY EITHER. The hint read `R = poll accounts`, and
# removing the binding while leaving the hint would tell him to press a key that
# does nothing — the exact silent failure the top of this file is about.
from datetime import datetime, timezone  # noqa: E402

from threads_tui import render  # noqa: E402
from threads_tui.link import LinkState  # noqa: E402

for link_state in (LinkState.LIVE, LinkState.OFFLINE, LinkState.CONNECTING):
    header = render.header_line1(
        Board(),
        render.HeaderState(link=link_state, refresh_age=240.0),
        200,
        datetime(2026, 10, 6, 12, 0, tzinfo=timezone.utc),
    ).plain
    check(
        f"the header names no poll key ({link_state.name})",
        "poll accounts" not in header and "R =" not in header,
        header,
    )
    # NON-VACUOUS: the same rendered line must still carry the freshness report,
    # so this is a header with content being checked, not an empty string that
    # trivially contains nothing.
    check(
        f"and it still reports freshness, so the line is not simply empty "
        f"({link_state.name})",
        "last update:" in header,
        header,
    )

print()
if FAILURES:
    print(f"test_keys: {len(FAILURES)} FAILED: {', '.join(FAILURES)}")
    sys.exit(1)
print("test_keys: all good")
