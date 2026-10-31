"""**A DISABLED ACCOUNT ON SCREEN: visible, and never wearing a bar's clothes.**

The daemon half of this feature is proved in Rust (`tests/account_disable.rs`
and `budgets.rs`'s own module). This file covers the half the operator actually
looks at, and it exists because the two failures available here are both silent.

THE FIRST FAILURE IS DISAPPEARANCE. An account that drops off the board is how
he loses track of one — this board already carries the case of him being told
two accounts were dead when they were not. A disabled account must still be a row,
still carry its readings, still be selectable.

THE SECOND FAILURE IS CONFLATION. `DISABLED` and `dead` and "inside a limit
window" must not look alike, because they call for opposite responses. A bar is
the system reporting a failure it observed and will retry out of; disabling is his
own choice, has no timer, and nothing lifts it. Rendering a disabled account in the health
badge's red would tell him an account had broken when he had switched it off.

AND THE ENTRY POINT. `parse_share` used to refuse 0% with "a share must be
greater than 0%" — the message that sent him to try 0.001% instead, which does
not disable anything and ranks the account as desperate to be drained. That
refusal going away is the whole feature from where he sits.

Every test names the MUTATION that makes it red.

Run: uv run --with pytest --with textual python -m pytest tests/test_disable.py
"""

from __future__ import annotations

import asyncio
import json
import pathlib
import sys
from datetime import datetime, timedelta, timezone

sys.path.insert(0, __file__.rsplit("/tests/", 1)[0])
sys.path.insert(0, __file__.rsplit("/test_disable.py", 1)[0])

import fake_daemon  # noqa: E402

from threads_tui import model, render  # noqa: E402
from threads_tui.model import Account  # noqa: E402
from threads_tui.pressure import UNRANKED_REASON, unranked_note  # noqa: E402
from threads_tui.share import (  # noqa: E402
    fmt_share_pct,
    parse_share,
    share_state,
)

NOW = datetime(2026, 9, 8, 12, 0, tzinfo=timezone.utc)


# ────────────────────────────────────────────────────────────────────────────
# 1. THE ENTRY POINT — he has to be able to type it
# ────────────────────────────────────────────────────────────────────────────


def test_zero_percent_is_refused_and_the_refusal_names_the_key():
    """**0% IS NOT A SHARE ANY MORE, and the refusal says where membership is.**

    THIS TEST REPLACES `test_zero_percent_is_accepted_and_is_the_disable`, which
    pinned the doctrine the operator overturned: *"I want disabled to be
    independent of share."* Membership lives in `account_disabled` now,
    `Store::set_pool_share` is bound back to `(0.0, 1.0]`, and the daemon's own
    refusal names `accounts/{account}/disable`. This editor mirrors that one round trip
    earlier, and names the KEY rather than the method, because a key is what he
    has.

    THE MUTATION — and it is the one that matters, because the SILENT version of
    this failure is what cost four days. Accept the zero and send it: the daemon
    refuses, the board prints the refusal, and he is no worse off than before the
    feature existed. Accept the zero and treat it locally as the disable, and the
    board tells him an account is out of the pool while placement is still
    ranking it. Both are caught here.
    """
    for text in ["0", "0%", " 0 ", "0.0", "00", "-0"]:
        value, error = parse_share(text)
        assert value is None, f"{text!r} was accepted as a share: {value}"
        assert "0%" in error or "not a share" in error, error
        # AND IT NAMES THE OTHER DOOR. "not allowed" on its own is the message
        # that sent him to 0.001%.
        assert "press d" in error.lower(), f"{text!r}: the refusal names no key: {error}"


def test_a_tiny_share_is_still_accepted_and_is_still_not_a_disable():
    """**0.001% is a legal share, and there is no longer any share that is not.**

    He is allowed to set a very small share. What must never happen is the
    interface treating one AS membership, because the daemon will not: a 0.001%
    share ranks the account as needing to burn its remainder immediately. Under
    the old model this test had to sweep down to 1e-300 to kill any tolerance
    someone might reintroduce into an `is_disabled` predicate. That predicate is
    GONE — there is no share value that means "out of the pool" — so the sweep
    now asserts something stronger and simpler: every one of them is accepted,
    and every one of them renders as its own number.

    THE MUTATION. Reintroduce `is_disabled` anywhere and have `share_state`
    consult it; the 0.0 case cannot be reached (it is refused above), so any
    tolerance wide enough to catch anything at all catches one of these.
    """
    value, error = parse_share("0.001")
    assert error == ""
    assert value is not None and value > 0.0

    for tiny in [1e-3, 1e-5, 1e-7, 1e-9, 1e-12, 1e-100, 1e-300, 5e-324]:
        assert share_state(tiny) != "DISABLED", tiny
        assert share_state(tiny).endswith("%"), tiny


def test_no_share_value_renders_as_a_membership_state():
    """**`share_state` answers a number or a dash, and never a state.**

    It used to answer `DISABLED` for an exact zero, and had to: the zero WAS the
    state. Membership is its own fact now, so a client that still read a state
    out of the number would be re-deriving something the daemon stopped
    encoding there — and would disagree with the thing making the placement
    decision the moment a pre-migration `0.0` row survived on disk. The daemon
    narrowed its own display bound back to `s > 0.0` for exactly this reason.

    THE MUTATION. Put the `DISABLED` branch back and this goes red.
    """
    assert share_state(0.0) == "0%", "a zero is a number, and a relic; not a state"
    assert share_state(0.95) == "95%"
    assert share_state(0.00001) == "0.001%", "a tiny share keeps its number"
    assert share_state(None) == "—", "unknown is not disabled"
    assert "DISABLED" not in share_state(0.0)

    # `fmt_share_pct` is the EDITOR's field value and is deliberately still a
    # bare number — it is what he types into, not what he reads.
    assert fmt_share_pct(0.0) == "0"


def test_a_negative_share_is_still_refused():
    """A negative share is not a "more disabled" disable. It is nonsense."""
    for text in ["-1", "-0.5", "-100"]:
        value, error = parse_share(text)
        assert value is None, text
        assert "negative" in error, error
    # And the refusal POINTS AT THE KEY, so the next thing he does is right.
    _, error = parse_share("-1")
    assert "press d" in error.lower(), error


def test_over_a_hundred_percent_is_still_refused():
    value, error = parse_share("150")
    assert value is None
    assert "cannot exceed" in error


# ────────────────────────────────────────────────────────────────────────────
# 2. THE WORD, NOT THE NUMBER
# ────────────────────────────────────────────────────────────────────────────


def test_the_unranked_reason_for_a_disable_is_not_a_fault():
    """**`disabled` is in the vocabulary, and it does not read like a failure.**

    Every other entry in `UNRANKED_REASON` describes a measurement problem —
    "no usage reading yet", "the usage read failed". If a disabled account were rendered
    with an unrecognised-reason passthrough it would show the raw wire token
    `disabled`, which is not wrong but says nothing about whose decision it was.

    THE MUTATION. Delete the `disabled` key and this goes red on the second
    assertion — `unranked_note` falls back to passing the token through verbatim.
    """
    assert "disabled" in UNRANKED_REASON
    note = unranked_note("disabled")
    assert note != "disabled", "the raw wire token is not an explanation"
    assert "DISABLED" in note
    assert "you" in note, "it has to say whose decision this was"
    # AND HOW TO UNDO IT. This used to assert `0%` — the mechanism — which was
    # the only remedy there was while the share editor was the only door. It now
    # names the KEY, because the act has one: telling him to go set a number to
    # undo a thing he did with one keypress is the same gap that left his
    # `pool_shares` row on 0.001% for four days.
    assert "press d" in note, "and what to press to undo it"

    # The measurement reasons still read as measurement problems, so the
    # distinction survives on the same screen.
    assert "reading" in unranked_note("no_reading")
    assert "failed" in unranked_note("read_failed")


# ────────────────────────────────────────────────────────────────────────────
# 3. VISIBLE, AND DISTINGUISHABLE FROM A BAR
# ────────────────────────────────────────────────────────────────────────────


LABELS = ("alice-example-com", "zz-second")


def _board_with(pressure=None, shares=None):
    """A two-account board, built the way the app builds one: from exports.

    **THE PAYLOAD SHAPES ARE UNCHANGED AND THAT IS THE POINT.** The call sites
    below still pass the daemon's own `accounts/pressure` and
    `accounts/pool_share_get` rows, because those are the rows the shape's
    `pressure` and `share` cells carry — the fold holds the same structs the
    queries returned, which is why `disabled`, `share`, `disabled_at` and
    `disabled_note` all read the same on this side. What changed is the DOOR: S3
    deleted `build_board`, so the rows arrive as cells on an export instead of
    as two of five joined payloads.

    A cell the daemon has nothing for is `not_hydrated` WITH A REASON, never an
    absent key silently defaulting to something. That is the difference the
    three-valued unknown bought: on the old path an account missing from
    `accounts/pressure` was indistinguishable from a payload that failed to
    arrive.
    """
    press_rows = {
        r["account"]: r for r in ((pressure or {}).get("accounts") or [])
    }
    share_rows = {r["account"]: r for r in ((shares or {}).get("shares") or [])}
    frames = []
    for label in LABELS:
        overrides = {}
        cell_overrides = {}
        if label in press_rows:
            overrides["pressure"] = press_rows[label]
        else:
            overrides["pressure"] = None
            cell_overrides["pressure"] = fake_daemon.not_hydrated(
                "not ranked in the pressure pool"
            )
        if label in share_rows:
            overrides["share"] = share_rows[label]
        else:
            overrides["share"] = None
            cell_overrides["share"] = fake_daemon.not_hydrated(
                "no share row for this account"
            )
        frames.append(
            fake_daemon.export(
                label,
                r=fake_daemon.row(label, **overrides),
                c=fake_daemon.cells(**cell_overrides),
            )
        )
    return fake_daemon.board_of(*frames, now=NOW)


def test_the_disabled_flag_is_read_from_the_daemon_not_inferred_from_the_number():
    """**`disabled` comes off the wire as a boolean.**

    The daemon is the authority on what a zero means. A client that re-derives
    the state from `share == 0` can disagree with the thing actually making the
    placement decision — and the whole failure this feature addresses is a
    number being interpreted as a state.

    THE MUTATION. Drop the `disabled` reads in `model.decorate` and this goes red.
    """
    pressure = {
        "accounts": [
            {
                "account": "alice-example-com",
                "ranked": False,
                "pressure": None,
                "unranked_reason": "disabled",
                "disabled": True,
                "share": 0.0,
                "observed_pct": 42.0,
            },
            {
                "account": "zz-second",
                "ranked": True,
                "pressure": 1.4,
                "disabled": False,
                "share": 0.95,
            },
        ]
    }
    board = _board_with(pressure=pressure)
    by_label = {a.label: a for a in board.accounts}

    disabled = by_label["alice-example-com"]
    assert disabled.disabled is True
    assert disabled.pressure is None, "unranked, and never rendered as 0"
    assert disabled.pressure_ranked is False
    assert disabled.pressure_reason == "disabled"
    assert disabled.pool_share == 0.0

    live = by_label["zz-second"]
    assert live.disabled is False, "not a board-wide default"
    assert live.pressure == 1.4


def test_a_disable_set_on_a_non_pool_member_still_reaches_the_screen():
    """**Either payload may carry it.**

    `accounts/pressure` lists pool members. A share set on a label that is not
    currently a pool member appears only in `accounts/pool_share_get`. A disabled account
    the interface cannot see is precisely the failure this feature was written
    to avoid, so both are read.

    THE MUTATION. Drop the `disabled_by_account` fallback and this goes red.
    """
    shares = {
        "shares": [
            {
                "account": "alice-example-com",
                "share": 0.0,
                "disabled": True,
                "override": 0.0,
                "source": "override",
                "note": "no longer a paid account",
            }
        ]
    }
    board = _board_with(pressure=None, shares=shares)
    disabled = {a.label: a for a in board.accounts}["alice-example-com"]
    assert disabled.disabled is True
    assert disabled.pool_share == 0.0


def test_a_disabled_account_is_still_a_row_on_the_board():
    """**STILL VISIBLE.** Silently absent is how he loses track of an account."""
    pressure = {
        "accounts": [
            {
                "account": "alice-example-com",
                "ranked": False,
                "pressure": None,
                "unranked_reason": "disabled",
                "disabled": True,
                "share": 0.0,
            }
        ]
    }
    board = _board_with(pressure=pressure)
    labels = [a.label for a in board.accounts]
    assert "alice-example-com" in labels, "a disabled account is never dropped"
    assert len(board.accounts) == 2


def test_disabled_badges_apart_from_dead_and_from_a_limit():
    """**DISABLED IS NOT BARRED, on screen.**

    The three states must be visually distinct, because they call for opposite
    responses: `dead` means go and log in, a limit window means wait, and a
    disable means nothing at all unless he changes his mind.

    THE MUTATION. Render the disabled state through `health_badge` — the "we already have
    a badge mechanism" simplification — and this goes red twice: the disabled account
    arrives in the health badge's RED, and a dead account and a disabled one stop
    being separable by style.
    """
    disabled = Account(number=1, label="alice-example-com", state="live", disabled=True)
    dead = Account(number=2, label="zz-dead", state="wiped")
    healthy = Account(number=3, label="mm-fine", state="live")

    disabled_badges = render._badges(disabled, NOW).plain
    dead_badges = render._badges(dead, NOW).plain
    healthy_badges = render._badges(healthy, NOW).plain

    assert "DISABLED" in disabled_badges
    assert "DISABLED" not in dead_badges, "a dead account is not disabled"
    assert "DISABLED" not in healthy_badges, "and neither is a healthy one"

    # A disabled account is HEALTHY. It must not pick up a health badge merely by
    # being disabled — the row says "out of the pool", never "broken".
    assert disabled.health_badge is None
    assert disabled.healthy_login is True

    # And the two are different STYLES, not merely different words: the disabled state is
    # amber, and the health badge for a wiped login is red.
    disabled_styles = {str(span.style) for span in render._badges(disabled, NOW).spans}
    dead_styles = {str(span.style) for span in render._badges(dead, NOW).spans}
    assert disabled_styles, "the disabled badge must carry a style of its own"
    assert disabled_styles.isdisjoint(dead_styles), (
        f"a disabled account must not render in a bar's colour: disabled={disabled_styles} "
        f"dead={dead_styles}"
    )


# ────────────────────────────────────────────────────────────────────────────
# 5. THE ACT HAS A KEY, AND THE KEY SAYS WHAT IT WILL DO
#
# The capability shipped on 2026-09-08 behind "type 0 into the share editor".
# It was live in the running daemon and the operator never reached it: his
# `pool_shares` row for `alice-example-com` still held `1.0e-05` on 2026-09-11
# — the 0.001% he tried BEFORE the feature existed. These tests exist because
# that gap was an INTERFACE gap, not a capability gap.
# ────────────────────────────────────────────────────────────────────────────


def test_d_is_bound_on_the_board_in_both_cases():
    """**`d` disables the selected account, and `D` does the same thing.**

    He drives this board by keyboard. `l`/`L` are already both bound here for
    exactly this reason — Textual matches binding keys by EXACT string with no
    case folding, so a shift he did not mean to press finds no binding, finds no
    handler, and is dropped SILENTLY. A silent no-op on the key the board tells
    him to press is this interface's worst failure mode.

    THE MUTATION. Drop either binding and this goes red. Drop the `action_disable`
    method and the third assertion goes red — a binding whose action does not
    exist is also a silent no-op.
    """
    from threads_tui.app import BoardScreen

    keys = {b.key for b in BoardScreen.BINDINGS}
    assert "d" in keys, "the disable key is not bound"
    assert "D" in keys, "shift+d must not be a silent no-op"
    actions = {b.action for b in BoardScreen.BINDINGS if b.key in ("d", "D")}
    assert actions == {"disable"}
    assert hasattr(BoardScreen, "action_disable"), "the binding points at nothing"

    # And it did not collide with a key that already meant something. A rebind
    # that silently steals `s` or `S` would break the share editor and the
    # machine-login carve-out, neither of which this change is allowed to touch.
    # `"r"` STOOD IN THIS LIST and is removed because the key is: the board has
    # no refresh key and no poll key (see `tests/test_keys.py`, which pins the
    # binding table as an exact set and carries the argument). Leaving `"r"` here
    # would have made this test demand the key's return.
    for taken in ("q", "p", "l", "s", "S"):
        assert taken in keys, f"{taken} was displaced by the disable binding"


def test_the_confirmation_says_what_disabling_does_and_does_not_do():
    """**The modal answers the question he actually asked.**

    *"I don't want it to be deleted, though. I still want to see it there as one
    of my accounts, because if I turn it back on, I'm going to keep using the
    same login."* That sentence has to be ON SCREEN at the moment he presses the
    key, not in a commit message.

    THE MUTATION. Remove the not-deleted/login-untouched line from
    `DisableConfirm.compose` and this goes red.
    """
    from threads_tui.share import ReactivateConfirm

    m = ReactivateConfirm("alice-example-com", currently_disabled=False, share=0.45)
    text = m.title_line() + " " + " ".join(t for t, _ in m.body_lines())
    assert "alice-example-com" in text
    assert "Disable" in m.title_line()
    assert "NOT deleted" in text, "the deletion fear must be answered here"
    assert "login is NOT touched" in text
    assert "placement" in text, "it must say what actually changes"
    assert "timer" in text, "a disable is not a limit window and must not read as one"

    # AND THE SHARE IS KEPT — the new fact, said in the direction where it
    # matters most. Under the old model this act destroyed the share, and the
    # operator had to trust a memo to get it back. It does not any more, and the
    # sentence that used to be needed is the sentence that is now false.
    assert "45%" in text, "say the number it keeps, at the moment he decides"
    assert "keeps" in text.lower() or "kept" in text.lower(), text


def test_the_reactivate_confirmation_says_the_share_was_never_taken_away():
    """**REACTIVATE, and it promises the share the account still has.**

    THIS TEST REPLACES THE RESTORE ONES, which tested a concept the daemon
    deleted. Under the old model a disable WAS a share of zero, so the share it
    had was destroyed by the act and `restore_to` / `prior` existed to remember
    it — two fields answering "what would an enable put this back at". Under the
    new model `disabled` is its own row in its own table, the share is never
    touched, and the answer to that question is the `share` field sitting right
    beside it.

    So the modal does not promise a REMEMBERED number, it states a KEPT one, and
    the difference is not cosmetic: "it comes back at 60%, the share it had"
    describes a restoration, and there was no destruction to restore from.

    THE MUTATION — the one this rewrite exists to catch. Pass `account.share`
    into a parameter still named `restore_to` and have the sentence say "comes
    back at" / "the share it had", and the wording assertions go red. The concept
    has to be gone from the words as well as from the fields, because the words
    are what the operator reads.
    """
    from threads_tui.share import ReactivateConfirm

    m = ReactivateConfirm("acct", currently_disabled=True, share=0.6)
    text = m.title_line() + " " + " ".join(t for t, _ in m.body_lines())
    assert "Reactivate" in m.title_line(), m.title_line()
    assert "Enable" not in m.title_line() and "Restore" not in m.title_line()
    assert "60%" in text, "it still names the share the account will be on"
    assert "still" in text.lower() or "keeps" in text.lower(), text
    for gone in ("restore", "Restore", "the share it had", "comes back at"):
        assert gone not in text, f"the restore concept survives in the copy: {gone!r}"

    # AND IT NEVER POINTS AT `S`. The old fallback said "Press S to set one by
    # hand", which was wrong twice over: it implied the daemon had failed to send
    # something, and `S` is the share editor, which no longer enables anything.
    assert "Press S" not in text and "press S" not in text


def test_the_reactivate_confirmation_is_honest_when_no_share_was_reported():
    """**No number is ever invented, and the missing case no longer blames the daemon.**

    A share of `None` means the daemon reported no share row for this account —
    which under the new model is a real and ordinary state, since a disable no
    longer MINTS an override. The sentence must not read as a fault report, and
    it must not guess 100%, which is the one direction in which a wrong guess
    costs real capacity.

    THE MUTATION. Default a missing share to 1.0 and this goes red.
    """
    from threads_tui.share import ReactivateConfirm

    m = ReactivateConfirm("acct", currently_disabled=True, share=None)
    text = " ".join(t for t, _ in m.body_lines())
    assert "%" not in text, f"a share was guessed: {text!r}"
    assert "did not report a restore share" not in text
    assert "Press S" not in text


def test_the_board_carries_no_restore_concept_at_all():
    """**`restore_to` and `prior` are GONE from this side, not repurposed.**

    The daemon deleted `Budgets::restore_share`, `Store::pool_share_prior` and
    both wire fields. The tempting one-liner was to keep the board's field and
    feed it `account.share` — the account-model note even suggested it — and that
    is precisely what must not happen: a field named `restore_to` carrying a
    share that was never taken away is the two-fields-one-question defect
    reintroduced on the client, and the next reader has to discover that the name
    is a lie.

    THE MUTATION. Re-add `restore_to` to `Account` (with any value, from any
    source) and this goes red.
    """
    from threads_tui import model, share

    a = Account(number=1, label="acct")
    assert not hasattr(a, "restore_to"), "the restore concept is back on the model"
    assert not hasattr(a, "restore_prior")
    assert not hasattr(share, "DisableConfirm"), (
        "the old modal name is retired with the old semantics"
    )
    # Read the CODE, with comments and prose stripped, so that a module free to
    # explain why the concept is gone is not accused of keeping it.
    def code_of(module) -> str:
        import ast

        tree = ast.parse(pathlib.Path(module.__file__).read_text())
        for node in ast.walk(tree):
            if isinstance(node, (ast.Module, ast.ClassDef, ast.FunctionDef,
                                 ast.AsyncFunctionDef)) and ast.get_docstring(node):
                node.body = node.body[1:] or [ast.Pass()]
        return ast.unparse(ast.fix_missing_locations(tree))

    for dead in ("restore_to", "prior", "restore_prior", "restore_share"):
        assert dead not in code_of(model), f"model.py still reads {dead}"
    for dead in ("restore_to", "restore_prior", "restore_sentence"):
        assert dead not in code_of(share), f"share.py still carries {dead}"


def test_a_disabled_account_keeps_its_share_on_the_wire_and_on_the_model():
    """**The share rides across a disable, because the daemon no longer moves it.**

    `PoolShare` is `{share, disabled}` now and `share()` answers the real number
    on a disabled account, where `for_display()` used to answer `0.0` because the
    zero WAS the state. A board that still expected a zero would draw a disabled
    account as having no share at all, and the reactivate copy would have nothing
    to name.

    THE MUTATION. Gate the `share` read on `not disabled` and this goes red.
    """
    shares = {
        "shares": [
            {
                "account": "alice-example-com",
                "share": 0.45,
                "disabled": True,
                "disabled_at": "2026-09-20T09:15:00+00:00",
                "disabled_note": "no longer a paid account",
                "source": "override",
            },
            {"account": "zz-second", "share": 0.8, "disabled": False},
        ]
    }
    board = _board_with(shares=shares)
    off = {a.label: a for a in board.accounts}["alice-example-com"]
    assert off.disabled is True
    assert off.pool_share == 0.45, "the share was kept across the disable"
    assert off.disabled_note == "no longer a paid account"
    assert off.disabled_at is not None
    assert off.disabled_at.year == 2026 and off.disabled_at.month == 9


def test_the_disabled_badge_does_not_wear_a_pause_glyph():
    """**The word is `DISABLED` and there is no `⏸` beside it.**

    `⏸` is the pause glyph and it means "temporarily, and it will resume". This
    state does not resume — nothing lifts it but him — so the glyph contradicts
    the word it is decorating. His word is `disabled`; it stands on its own.

    THE MUTATION. Put the glyph back and this goes red.
    """
    off = Account(number=1, label="alice-example-com", state="live", disabled=True)
    badges = render._badges(off, NOW).plain
    assert "DISABLED" in badges
    assert "⏸" not in badges, "a pause glyph says the opposite of what this state is"
    for banned in ("PAUSED", "paused", "inactive", "0%"):
        assert banned not in badges, f"his word is `disabled`, never {banned!r}"


def test_the_legacy_paused_key_is_retired_with_the_path_that_needed_it():
    """**A TRANSITION SHIM WITH A KNOWN END, AND THIS IS THE END.**

    `_wire_disabled` read either `disabled` or `paused`, because the old daemon
    spelled the state one way and the new one spells it the other, and this
    interface runs FROM SOURCE while the daemon ships as a signed build on its
    own schedule. For that window, reading only the new key would have made the
    badge VANISH on a daemon reporting the state perfectly well — an account
    out of placement with the board saying nothing, which is the exact failure
    the whole feature exists to prevent.

    The window is closed, and it is closed by construction rather than by
    judgement: the board reads this state off the shape's `share` and
    `pressure` cells, which carry the daemon's own `PoolShareRow` /
    `PressureRow` structs, and those have one field spelled `disabled`. A
    daemon old enough to say `paused` has no folds at all, and a board against
    that daemon draws no cards — so there is no screen on which the leniency
    could have applied.

    So the shim is deleted and this test is what it became: the client must NOT
    grow a second vocabulary back. `unranked_note` still translates the reason
    token, which is a different thing — that is the daemon's own word for WHY,
    and it is rendered rather than re-derived.
    """
    from threads_tui import model

    assert not hasattr(model, "_wire_disabled"), (
        "the legacy `paused` reader is retired: the shape carries one spelling"
    )
    assert not hasattr(model, "_DISABLED_WIRE_KEYS")

    # A row that says `paused` and not `disabled` is NOT read as disabled. This
    # is the assertion that stops the shim being reintroduced quietly.
    board = _board_with(
        pressure={
            "accounts": [
                {
                    "account": "alice-example-com",
                    "ranked": False,
                    "pressure": None,
                    "paused": True,
                    "unranked_reason": "paused",
                    "share": 0.0,
                }
            ]
        }
    )
    off = {a.label: a for a in board.accounts}["alice-example-com"]
    assert off.disabled is False, (
        "`paused` is not a key the fold emits; reading it would be the client "
        "holding a vocabulary the daemon does not have"
    )

    # The REASON token is still translated, because that one really does come
    # off the wire as free text the daemon chose, and a raw token is not an
    # explanation.
    assert unranked_note("paused") == unranked_note("disabled")
    assert "DISABLED" in unranked_note("paused")



# ────────────────────────────────────────────────────────────────────────────
# 6. THE SIMPLE CARD
#
# *"Disabled just means we don't want to see any details. Just make the card
# simple. We don't need limit data at all. Just its name and that it's
# disabled."*
#
# The previous card drew the full thing — three meters, a pressure row with a
# diverging bar, a limit window, an observation stamp. Every one of those is a
# measurement of an account that is not being placed on, sitting in the vertical
# space of a board he scans to decide where work should go. The readings are not
# WRONG; they are irrelevant, and irrelevant numbers on a dense board cost more
# than they say.
# ────────────────────────────────────────────────────────────────────────────


def _loaded_account(**kw) -> Account:
    """An account carrying EVERYTHING a card can draw.

    Deliberately maximal: if the simple card were produced by an account that
    happened to have nothing to show, the test would prove nothing. This one has
    three calibrated axes, a live limit window, a pressure reading and a share,
    so every suppressed element is an element that was genuinely there.
    """
    from threads_tui.model import Activity, AxisState

    axes = {
        name: AxisState(
            name=name, pct=pct, present=True, calibrated=True, samples=15,
            confidence=1.0, source="usage",
            observed_at=NOW - __import__("datetime").timedelta(minutes=3),
            resets_at=NOW + __import__("datetime").timedelta(days=2),
        )
        for name, pct in (("session", 12.0), ("weekly_all", 97.0), ("weekly_fable", 40.0))
    }
    a = Account(
        number=1, label="alice-example-com", state="live", axes=axes,
        pressure=None, pressure_ranked=False, pressure_reason="disabled",
        pool_share=0.45, in_pressure_pool=True,
        activity=Activity(
            known=True,
            limit_until=NOW + __import__("datetime").timedelta(hours=5),
            last_limit_prose="weekly limit reached",
            limit_axis="weekly_all",
        ),
        identity_acct="cc1@example.com", subscription_type="max",
    )
    for k, v in kw.items():
        setattr(a, k, v)
    return a


def _card_text(account: Account, *, selected: bool, mode: str = "full") -> str:
    return "\n".join(
        line.plain
        for line in render.card_lines(
            account, 100, selected=selected, mode=mode, warn_below=4.0,
            extras=None, flashing=None, now=NOW,
        )
    )


def test_a_disabled_card_is_its_name_and_the_word_and_nothing_measured():
    """**The whole card, in every mode, carries no measurement at all.**

    THE MUTATION. Delete the disabled branch from `card_lines` and this goes red
    on the first assertion — the meters come straight back, on both the selected
    and the unselected card.
    """
    off = _loaded_account(disabled=True)
    on = _loaded_account(disabled=False)

    for mode in ("full", "narrow", "compact"):
        for selected in (True, False):
            text = _card_text(off, selected=selected, mode=mode)
            where = f"{mode}/{'selected' if selected else 'unselected'}"

            # It is still a card, and it still says which account it is.
            assert "alice-example-com" in text, where
            assert "DISABLED" in text, where

            # NO METERS. `█`/`░` are the bar glyphs and the axis labels are the
            # row headings; neither has any business on a card he is not
            # choosing between.
            assert "█" not in text and "░" not in text, f"{where}: a meter survived\n{text}"
            for axis_word in ("session", "weekly", "fable"):
                assert axis_word not in text, f"{where}: axis {axis_word!r} survived\n{text}"

            # NO LIMIT DATA — his words, exactly.
            assert "AT LIMIT" not in text, where
            assert "limit" not in text.lower(), f"{where}: limit data survived\n{text}"

            # NO PRESSURE. The row, its bar, its share column and its `unranked`
            # word all go: pressure is the ranking of where the next turn goes,
            # and this account is not a candidate for one.
            assert "pressure" not in text.lower(), f"{where}: the pressure row survived\n{text}"
            assert "unranked" not in text, where
            assert "×" not in text, where

            # And the enabled card in the same fixture DOES draw all of it, so
            # the assertions above are not passing on an empty account.
            lit = _card_text(on, selected=selected, mode=mode)
            assert "█" in lit or "░" in lit, f"{where}: the fixture has no meters to hide"


def test_the_simple_card_is_short_and_the_saving_is_the_point():
    """**Fewer lines, not the same lines with the numbers blanked.**

    Blanking would keep the vertical cost, which is the cost that matters: this
    board is a stack and a disabled account is one he has decided not to think
    about.

    THE MUTATION. Render the disabled card through the normal path with muted
    styles and this goes red — the height does not move.
    """
    off = _loaded_account(disabled=True)
    on = _loaded_account(disabled=False)
    for selected in (True, False):
        short = len(render.card_lines(off, 100, selected=selected, mode="full",
                                      warn_below=4.0, extras=None, flashing=None, now=NOW))
        tall = len(render.card_lines(on, 100, selected=selected, mode="full",
                                     warn_below=4.0, extras=None, flashing=None, now=NOW))
        assert short < tall, f"selected={selected}: {short} lines vs {tall}"
        # Three lines: the two borders and one sentence.
        assert short == 3, f"selected={selected}: {short} lines\n{_card_text(off, selected=selected)}"

    # `card_height` must agree with what `card_lines` actually produces, or a
    # future caller lays the board out against a number that is not true.
    for selected in (True, False):
        assert render.card_height(off, selected=selected, mode="full") == len(
            render.card_lines(off, 100, selected=selected, mode="full",
                              warn_below=4.0, extras=None, flashing=None, now=NOW)
        ), f"card_height disagrees with card_lines for selected={selected}"


def test_the_simple_card_still_says_the_two_things_he_can_act_on():
    """**His own note and the key that undoes it.**

    `disabled_note` is what HE typed when he disabled it, and it is the only
    thing on this card that is not derivable from anywhere else. Dropping it in
    the name of simplicity would throw away the one fact the simple card exists
    to carry.

    THE MUTATION. Drop `disabled_note` from the line and this goes red.
    """
    from datetime import timedelta

    off = _loaded_account(
        disabled=True,
        disabled_note="no longer a paid account",
        disabled_at=NOW - timedelta(days=2),
    )
    text = _card_text(off, selected=True)
    assert "no longer a paid account" in text, text
    assert "press d" in text.lower(), "the card names the key that undoes it"

    # With nothing recorded it stays a clean sentence rather than growing an
    # empty quote or a dangling `since`.
    bare = _card_text(_loaded_account(disabled=True), selected=True)
    assert "DISABLED" in bare
    assert "''" not in bare and '""' not in bare and "since None" not in bare


def test_the_live_login_mark_survives_the_simple_card():
    """**A disabled account can still be the machine login, and must still say so.**

    The `s` key sets this machine's login and the board marks it on the
    corresponding card. That marking is a fact about the MACHINE, not about
    placement, so simplifying the card must not take it with the meters — the
    operator would lose the only place the board says which account plain
    `claude` is signed in as.

    THE MUTATION. Move the `LIVE LOGIN` badge below the disabled short-circuit
    and this goes red.
    """
    off = _loaded_account(disabled=True, is_live_login=True)
    text = _card_text(off, selected=True)
    assert "LIVE LOGIN" in text, text
    assert "DISABLED" in text, text


# ────────────────────────────────────────────────────────────────────────────
# 7. DELETED IS NOT DISABLED
#
# *"deleted is removed from the pool, which should be emitted as a key being
# removed from the shape and the board should drop it."*
#
# These are the two halves of one distinction and they are tested together on
# purpose. A disabled account STAYS — silently dropping one is how he loses
# track of an account, and section 3 above pins that. A deleted account GOES —
# a board that kept it would be showing a card for a login that no longer
# exists, and every key on this board acts on the selected card.
# ────────────────────────────────────────────────────────────────────────────


def test_a_removed_key_drops_its_card_and_a_disabled_one_does_not():
    """**`gone` removes the key; `disabled: true` does not.**

    `reload_pool`'s diff emits `Fact::AccountRemoved`, the `accounts` fold calls
    `tx.rows.remove(name)`, and the chassis turns that into one `op: live`
    frame with `kind: "gone"`. The board's half is `ShapeMirror._live`.

    THE MUTATION. Make the `gone` arm `return False` without popping — the
    "hold the export, apply everything after it" rule broken by keeping — and
    the removal assertions go red while the disabled ones stay green, which is
    exactly the shape of the bug: one of the two states disappears correctly
    and the other does not.
    """
    disabled_row = fake_daemon.row(
        "zz-second",
        share={"account": "zz-second", "share": 0.45, "disabled": True,
               "disabled_at": "2026-09-20T09:15:00+00:00", "disabled_note": "off for now",
               "override": 0.45, "declared": None, "note": None, "source": "override"},
    )
    mirror = fake_daemon.mirror_with(
        fake_daemon.export("acct0"),
        fake_daemon.export("zz-second", r=disabled_row),
        fake_daemon.export("doomed"),
    )
    from threads_tui import model

    before = model.project(mirror, now=fake_daemon.NOW)
    assert [a.label for a in before.accounts] == ["acct0", "doomed", "zz-second"]

    moved = mirror.on_frame(fake_daemon.live("doomed", {"kind": "gone"}))
    assert moved is True, "a key removal must report that something a reader sees moved"

    after = model.project(mirror, now=fake_daemon.NOW)
    labels = [a.label for a in after.accounts]
    assert "doomed" not in labels, f"the removed key kept its card: {labels}"
    assert labels == ["acct0", "zz-second"]
    assert "doomed" not in mirror.keys()
    assert mirror.row("doomed") == {}

    # AND THE DISABLED ONE IS STILL THERE, with its readings, in the same pass.
    off = after.by_label("zz-second")
    assert off is not None and off.disabled is True
    assert off.pool_share == 0.45

    # A `gone` for a key that was never held is not an error and changes nothing.
    assert mirror.on_frame(fake_daemon.live("never-existed", {"kind": "gone"})) is False
    assert [a.label for a in model.project(mirror, now=fake_daemon.NOW).accounts] == [
        "acct0", "zz-second"
    ]


def test_a_removed_key_leaves_no_trace_on_the_rendered_board():
    """**Not merely absent from the model — absent from the screen.**

    `model.project` enumerates `mirror.keys()`, so the card count follows the
    held keys by construction. This asserts the consequence rather than the
    mechanism, because the mechanism is the thing that could be replaced.
    """
    from threads_tui import model

    mirror = fake_daemon.mirror_with(
        fake_daemon.export("acct0"), fake_daemon.export("doomed")
    )
    mirror.on_frame(fake_daemon.live("doomed", {"kind": "gone"}))
    board = model.project(mirror, now=fake_daemon.NOW)
    screen = "\n".join(
        line.plain
        for a in board.accounts
        for line in render.card_lines(
            a, 100, selected=False, mode="full", warn_below=4.0,
            extras=None, flashing=None, now=fake_daemon.NOW,
        )
    )
    assert "doomed" not in screen, screen
    assert "acct0" in screen


# ────────────────────────────────────────────────────────────────────────────
# 8. `d` IS A TOGGLE, AND IT IS PROVED IN BOTH DIRECTIONS ON A SOCKET
#
# *"if it's disabled we want to be able to un-disable it, e.g. restore it. Maybe
# we call it reactivate. It can use the same keybind as disable used, like a
# toggle."*
#
# The direction is chosen from the account's CURRENT state, which is the mirror's
# state, which arrives from the daemon. So a toggle that works is a claim about
# three things at once — the key, the method it picks, and the board having
# re-read the result — and only a socket can test all three. A pure test of
# `do_disable`'s branch would pass against a board that never learned the
# account had been disabled and so offered `accounts/{account}/disable` forever.
# ────────────────────────────────────────────────────────────────────────────


class ToggleDaemon:
    """Answers `accounts/{account}/disable` / `accounts/{account}/enable` and MOVES ITS OWN STATE.

    The share is deliberately left alone by both, exactly as the daemon now
    leaves it: `accounts/{account}/disable` writes `account_disabled` and never
    `pool_shares`. A fake that zeroed the share would let a board that still
    equated a zero share with the disabled state pass.
    """

    MUTATIONS = ("accounts/{account}/disable", "accounts/{account}/enable")

    def __init__(self, keys=("acct0",)) -> None:
        self.shape = fake_daemon.ShapeSurface(list(keys))
        self.methods: list[str] = []
        self.disabled: dict[str, bool] = {k: False for k in keys}
        self._server = None

    def asked(self) -> list[str]:
        # Routed, because the account is in the path: there is no single string
        # `accounts/{account}/disable` that a raw method could equal.
        return fake_daemon.asked(self.methods, *self.MUTATIONS)

    def _share_row(self, account: str) -> dict:
        return {
            "account": account,
            "share": 0.45,  # NEVER moves. That is the whole model change.
            "disabled": self.disabled[account],
            "disabled_at": "2026-09-19T11:00:00+00:00" if self.disabled[account] else None,
            "disabled_note": "off for now" if self.disabled[account] else None,
            "override": 0.45,
            "declared": None,
            "note": None,
            "source": "override",
        }

    def _apply(self, account: str, disabled: bool) -> dict:
        self.disabled[account] = disabled
        self.shape.rows[account] = fake_daemon.row(
            account,
            share=self._share_row(account),
            pressure={
                "account": account,
                "ranked": not disabled,
                "pressure": None if disabled else 1.4,
                "rank": None if disabled else 1,
                "unranked_reason": "disabled" if disabled else None,
                "disabled": disabled,
                "share": 0.45,
                "axis": "weekly-all",
                "observed_pct": 41.0,
                "observed_at": fake_daemon.T0,
                "resets_at": "2026-09-22T00:00:00+00:00",
                "reset_basis": "observed",
            },
        )
        return {"shares": [self._share_row(account)]}

    def _payload(self, method: str, params: dict):
        if method == "accounts/{account}/disable":
            return self._apply(params["account"], True)
        if method == "accounts/{account}/enable":
            return self._apply(params["account"], False)
        raise KeyError(method)

    def _process_request(self, connection, request):
        if request.path == "/health":
            from websockets.datastructures import Headers
            from websockets.http11 import Response

            body = json.dumps(
                {"status": "ok", "daemon": "threads", "boot_id": "boot-toggle", "proto": 1}
            ).encode()
            return Response(200, "OK", Headers({
                "Content-Type": "application/json",
                "Content-Length": str(len(body)),
                "Connection": "close",
            }), body)
        return None

    async def _handler(self, ws) -> None:
        try:
            await ws.send(json.dumps({
                "type": "welcome", "proto": 1, "daemon": "threads",
                "daemon_version": "test", "boot_id": "boot-toggle",
                "auth_required": False,
            }))
            async for raw in ws:
                frame = json.loads(raw)
                # No firehose is served: the board sends no `subscribe` (R2.S7).
                if frame.get("type") != "request":
                    continue
                method = frame.get("method")
                params = frame.get("params") or {}
                self.methods.append(method)
                # ROUTE AS THE DAEMON ROUTES. A method name is a path:
                # `bind_subject` matches it to a template, lifts the subject out
                # of the path into the params for the handler, and REFUSES a
                # caller that also sent it in the params. An unroutable path is
                # a `KeyError`, rendered below as `unknown method:` — which is
                # also what an OMITTED subject now produces, because a path with
                # a hole in it is not a half-formed request, it is not a method.
                template = None
                try:
                    template, bound = fake_daemon.bind_subject(method, params)
                    if template.startswith("shapes/"):
                        payload = self.shape.payload(template, bound)
                    else:
                        payload = self._payload(template, bound)
                    outcome = {"type": "success", "payload": payload}
                except KeyError:
                    outcome = {"type": "error", "error": f"unknown method: {method}"}
                except ValueError as exc:
                    outcome = {"type": "error", "error": str(exc)}
                await ws.send(json.dumps({
                    "type": "response", "correlate": frame.get("id"), "outcome": outcome,
                }))
                if template == fake_daemon.SUBSCRIBE and outcome["type"] == "success":
                    await self.shape.export_all(ws, bound)
        except Exception:
            pass

    async def start(self, port: int):
        from websockets.asyncio.server import serve

        self._server = await serve(
            self._handler, "127.0.0.1", port, process_request=self._process_request
        )
        return self._server

    async def stop(self) -> None:
        if self._server is not None:
            self._server.close()
            try:
                await self._server.wait_closed()
            except Exception:
                pass


async def _press_d_and_confirm(app, pilot) -> str:
    """Press `d`, wait for the modal, read its title, confirm it."""
    from threads_tui.share import ReactivateConfirm

    await pilot.press("d")
    title = ""
    for _ in range(80):
        if isinstance(app.screen, ReactivateConfirm):
            title = app.screen.title_line()
            break
        await fake_daemon.settle(pilot, 0.1)
    assert title, "`d` did not open the confirmation"
    await pilot.press("enter")
    await fake_daemon.settle(pilot, 1.5)
    return title


async def _toggle_run() -> dict:
    daemon = ToggleDaemon()
    app = await fake_daemon.boot(daemon)
    out: dict = {}
    try:
        async with app.run_test(size=(140, 60)) as pilot:
            await fake_daemon.settle(pilot, 3.0)
            out["before"] = app.board.by_label("acct0").disabled

            out["title_off"] = await _press_d_and_confirm(app, pilot)
            out["asked_off"] = list(daemon.asked())
            out["state_off"] = app.board.by_label("acct0").disabled
            out["share_off"] = app.board.by_label("acct0").pool_share
            out["screen_off"] = fake_daemon.body(app)

            out["title_on"] = await _press_d_and_confirm(app, pilot)
            out["asked_on"] = list(daemon.asked())
            out["state_on"] = app.board.by_label("acct0").disabled
            out["share_on"] = app.board.by_label("acct0").pool_share
            out["note"] = app.status_note
    finally:
        await daemon.stop()
    return out


def test_d_toggles_both_ways_and_the_share_never_moves():
    """**One key, two directions, proved off the wire.**

    THE MUTATION — two of them, and each catches a different half.
      1. Hardcode `caps.ACCOUNTS_DISABLE` in `do_disable` and the second half
         goes red: `asked_on` reads `[disable, disable]`, so `d` on a disabled
         account disables it again and the operator has no way back.
      2. Delete the `await self.resync()` and the board never learns the state
         moved, so `state_off` stays False and the SECOND press picks the wrong
         direction — the failure looks like a broken key rather than a stale
         mirror, which is why it is asserted here and not on `do_disable`.
    """
    out = asyncio.run(_toggle_run())

    assert out["before"] is False

    # ── out ──
    assert out["asked_off"] == ["accounts/{account}/disable"], out["asked_off"]
    assert "Disable" in out["title_off"], out["title_off"]
    assert out["state_off"] is True, "the board did not learn the account was disabled"
    assert "DISABLED" in out["screen_off"]

    # ── and back, on the SAME key ──
    assert out["asked_on"] == ["accounts/{account}/disable", "accounts/{account}/enable"], out["asked_on"]
    assert "Reactivate" in out["title_on"], out["title_on"]
    assert out["state_on"] is False, "`d` on a disabled account did not reactivate it"

    # THE SHARE NEVER MOVED, in either direction. Under the old model the round
    # trip was 0.45 -> 0.0 -> 0.45 and depended on a memo; now nothing touches it.
    assert out["share_off"] == 0.45, out["share_off"]
    assert out["share_on"] == 0.45, out["share_on"]

    # And the verdict speaks the new word.
    assert "REACTIVATED" in out["note"] or "reactivat" in out["note"].lower(), out["note"]


def test_the_absent_notes_for_this_act_do_not_point_at_a_door_that_is_shut():
    """**An absent-capability note must name a remedy that still exists.**

    Both notes told him to reach the act through the share editor — *"press S
    and enter 0, which is the same act through the number"*. It is not the same
    act any more and it is not an act at all: the daemon refuses `0.0` and so
    does `parse_share`, so following that instruction now produces a refusal on
    a board that just told him to do it. An absent-note that names a shut door
    is worse than one that names none, because he will spend the keystrokes.

    THE MUTATION. Put the old sentence back in either note and this goes red.
    """
    from threads_tui import caps as caps_mod

    for name in (caps_mod.ACCOUNTS_DISABLE, caps_mod.ACCOUNTS_ENABLE):
        note = caps_mod.CAPABILITIES[name].absent_note
        assert note, name
        assert "enter 0" not in note, f"{name}: names a value the board refuses\n{note}"
        assert "above 0" not in note, f"{name}: the share editor no longer enables\n{note}"
        assert "press S" not in note and "Press S" not in note, (
            f"{name}: points at the share editor, which cannot do this\n{note}"
        )
        # AND IT SAYS WHAT WOULD ACTUALLY FIX IT. There is no CLI verb for
        # either of these — `threads accounts` has add/adopt/list/show/status/
        # health/pool-share/pressure/rename/delete and nothing else — so a note
        # naming one would be the same defect wearing a different door. The only
        # true remedy is a newer daemon, and the note says so.
        assert "Promote a daemon" in note, f"{name}: names no remedy at all\n{note}"
        assert "threads accounts disable" not in note, (
            f"{name}: names a CLI verb that does not exist\n{note}"
        )
        assert "threads accounts enable" not in note, (
            f"{name}: names a CLI verb that does not exist\n{note}"
        )


def test_a_long_note_shortens_and_never_evicts_the_key():
    """**`disabled_note` is free text he typed, and the card is a fixed width.**

    Something has to give on a narrow card. It must not be the hint: `press d to
    reactivate` is the only actionable thing on a simple card, and a wordy note
    that pushed it off the end would leave a card that states a problem and
    names no remedy. The note shortens with an `…`; the key does not move.

    THE MUTATION. Put the hint back at the end of the left-hand run — which is
    where it was first written — and this goes red at width 60: the note runs to
    the edge and the hint is cropped away entirely.
    """
    from datetime import timedelta

    off = _loaded_account(
        disabled=True,
        disabled_at=NOW - timedelta(days=2),
        disabled_note=(
            "cancelled the subscription on it after the billing change and it "
            "should not be placed on again until I decide whether to renew"
        ),
    )
    for width in (60, 80, 100, 140):
        lines = render.card_lines(
            off, width, selected=False, mode="full", warn_below=4.0,
            extras=None, flashing=None, now=NOW,
        )
        text = "\n".join(l.plain for l in lines)
        assert {l.cell_len for l in lines} == {width}, (
            f"w={width}: lines are {sorted({l.cell_len for l in lines})}"
        )
        assert "press d to reactivate" in text, f"w={width}: the key was cropped away\n{text}"
        assert "DISABLED" in text, width
        # Below ~80 cells there is no room for free text beside the hint, and
        # the card drops the note rather than crop the key. From 80 up the note
        # is there in some form.
        if width >= 80:
            assert "cancelled" in text, f"w={width}\n{text}"
            assert "…" in text, f"w={width}: a 120-char note fitted whole?\n{text}"
        # And a cropped AGE is never shown: it is all of it or none of it,
        # because "2d 00h" with the " ago" shaved off still reads as a duration.
        if "2d 00h" in text:
            assert "2d 00h ago" in text, f"w={width}: a cropped age\n{text}"

    # At the widest, it is not truncated at all.
    wide = "\n".join(
        l.plain for l in render.card_lines(
            off, 200, selected=False, mode="full", warn_below=4.0,
            extras=None, flashing=None, now=NOW,
        )
    )
    assert "whether to renew" in wide, wide
    assert "…" not in wide, "nothing needed shortening at 200 cells"


# ────────────────────────────────────────────────────────────────────────────
# 7. THE BOARD-LEVEL WARNING — the third reader of the flag, and the one that
#    was not reading it
# ────────────────────────────────────────────────────────────────────────────
#
# His words: *"threads shows me two errors for the two DEAD accounts even though
# I've marked them disabled. Maybe we can remove those error messages since we
# want them to be disabled."*
#
# THE MESSAGES ARE NOT REMOVED. `Board.warnings()` looped every account and
# emitted a `dead`-severity line for any account whose `state` was not `live` or
# `?`, with no `disabled` check anywhere in it — the one reader of this flag that
# never asked. `_badges` honours it (it draws `DISABLED` in amber rather than the
# health badge's red) and `card_lines` honours it (it collapses the card so the
# health note is never drawn); this function did not.
#
# AND NOTHING WAS DEAD. The word he read as DEAD is `STATE_NOTE["wiped"]` —
# login-health prose for a store `claude` cleared — while `accounts.dead_since`
# was NULL on all four accounts. The board has told him that once before; the
# note on `render._badges` names that incident, and this loop is how it happened
# again.
#
# THESE TESTS COME IN PAIRS ON PURPOSE. A test that only proves the disabled
# account went quiet is satisfied by deleting the whole loop — which would also
# silence the enabled account that genuinely cannot run. Each direction is the
# other's control, and neither alone is a fix.


def _warn_board(*accounts: Account):
    from threads_tui.model import Board

    board = Board()
    board.accounts = list(accounts)
    return board


def _wiped(number: int, label: str, **kw) -> Account:
    """An account whose LOGIN STORE is wiped.

    NOT a dead account — there is no dead account on this machine, which is the
    whole of the confusion. `state` comes from the credential's `slot_state`, and
    `health_note` is a PROPERTY derived from it, so this cannot be faked by
    stuffing the note: the fixture has to produce the real state to produce the
    real sentence.
    """
    a = Account(number=number, label=label, state="wiped")
    for k, v in kw.items():
        setattr(a, k, v)
    return a


def test_a_disabled_wiped_account_raises_no_red_line_at_all():
    """**DIRECTION ONE: the two red lines he should never have seen.**

    THE MUTATION. Delete `if account.disabled: continue` from `Board.warnings()`
    and this goes red immediately, with the exact text he reported — except for
    its first word, which is the subject of Correction 3:
    `✕ owner-account WIPED — claude cleared this store after a refresh it
    could not recover — press 1, then L, to sign it in`. It said `DEAD` when he
    reported it, which is why he believed two accounts were dead.
    """
    board = _warn_board(
        _wiped(1, "owner-account", disabled=True),
        _wiped(2, "alice-example-com", disabled=True),
    )
    assert board.warnings() == [], board.warnings()

    # AND THE RENDERED LINE, because a severity tuple is not what he reads.
    # `warning_line` is what turns a `dead` severity into the red `✕`.
    rendered = [
        render.warning_line(sev, text, 120).plain for sev, text in board.warnings()
    ]
    assert rendered == [], rendered
    assert not any("✕" in line for line in rendered), rendered


def test_an_enabled_wiped_account_still_shouts_in_red():
    """**DIRECTION TWO, AND IT IS HALF THE FIX.**

    An account that is IN THE POOL and cannot run must still shout: work will be
    placed on it and the placement will fail. The message was always correct —
    only its unconditional application was wrong.

    THE MUTATION. Widen the skip from `account.disabled` to every account, or
    return early from `warnings()`, and this goes red: the enabled wiped store
    stops being reported and he loses the one warning that was doing its job.
    """
    board = _warn_board(_wiped(4, "spare-example-com"))
    warnings = board.warnings()
    assert len(warnings) == 1, warnings
    severity, text = warnings[0]
    assert severity == model.CRITICAL, severity

    line = render.warning_line(severity, text, 200).plain
    assert "✕" in line, line
    assert "spare-example-com" in line, line
    # CORRECTION 3: the word is `WIPED`, which is what `STATE_BADGE` has always
    # called this state and what the badge on the card beside it says. It used to
    # be `DEAD`, colliding with `not_answering_since`'s `✕ not answering 9m`.
    assert "WIPED" in line, line
    assert "DEAD" not in line, line
    assert "press 4, then L, to sign it in" in line, line


def test_the_same_board_holds_both_directions_at_once():
    """**THE DISCRIMINATION, not two separate behaviours.**

    This is the operator's actual board: two disabled wiped accounts beside an
    enabled wiped one. The fix must tell them apart in ONE render, which is
    strictly more than either direction alone proves — a flag read in the wrong
    sense passes exactly one of the two tests above and fails this one.
    """
    board = _warn_board(
        _wiped(1, "owner-account", disabled=True),
        _wiped(2, "alice-example-com", disabled=True),
        _wiped(4, "spare-example-com"),
    )
    rendered = [
        render.warning_line(sev, text, 200).plain for sev, text in board.warnings()
    ]
    assert len(rendered) == 1, rendered
    assert "spare-example-com" in rendered[0], rendered
    # THE ABSENCE ASSERTIONS BELOW ARE THE ONES THAT COULD BE VACUOUS, so each is
    # paired with proof that the needle is producible HERE: `rendered[0]` is
    # exactly this sentence about a different account, so a `WIPED — ...` line
    # plainly CAN appear in this list. These two therefore fail the moment either
    # disabled account raises, rather than passing because the string went extinct.
    assert "WIPED" in rendered[0], rendered
    assert not any("owner-account" in line for line in rendered), rendered
    assert not any("alice-example-com" in line for line in rendered), rendered


def test_a_disabled_account_that_is_not_answering_is_also_silent():
    """**THE SECOND LOOP, which was silent only by luck.**

    `Board.warnings()` had the identical omission twice: the
    `act.not_answering_since` branch (`act.dead_since` when this was written)
    never checked `disabled` either. It raised nothing today only because the
    daemon's `dead_since` column is NULL on all four accounts — a latent copy of
    the bug above, waiting for the first daemon that reports one. A latent copy of
    a bug you just fixed is the next bug.

    THE MUTATION. Move the `continue` to AFTER the health-note block so it guards
    only the first loop, and this goes red: `✕ alice-example-com is NOT ANSWERING
    — socket closed`. That mutation leaves every other test in this section
    GREEN, which is exactly why this one has to exist separately.
    """
    from threads_tui.model import Activity

    silent = Activity(
        known=True,
        not_answering_since=NOW - timedelta(minutes=9),
        not_answering_reason="socket closed",
    )
    off = Account(number=2, label="alice-example-com", state="live", disabled=True)
    off.activity = silent
    assert _warn_board(off).warnings() == [], _warn_board(off).warnings()

    # THE CONTROL, on the SAME `Activity` object: enabled, it must still shout.
    # Sharing the object is deliberate — it rules out the pair passing because the
    # two fixtures differed in some way other than the flag.
    on = Account(number=3, label="bob-example-com", state="live")
    on.activity = silent
    warnings = _warn_board(on).warnings()
    assert len(warnings) == 1, warnings
    line = render.warning_line(warnings[0][0], warnings[0][1], 200).plain
    assert "✕" in line and "NOT ANSWERING" in line, line
    assert "socket closed" in line, line


def test_the_disabled_card_still_says_its_store_is_wiped():
    """**WHAT THE SKIP MUST NOT COST HIM.**

    Silencing the board-level alarm must not silence the FACT. The card he selects
    still wears `WIPED` in its title border, so a disabled account with a cleared
    store is still visibly broken where he looks at that account. What is gone is
    only the imperative at the top of the board telling him to undo a decision he
    made — and `press 1, then L` is an imperative, which is why it is noise on a
    row he switched off and why noise there teaches him to skip the real ones.

    THE MUTATION. Fold the `disabled` skip into `_badges` as well, or drop
    `health_badge` from it, and this goes red.
    """
    badges = render._badges(
        _wiped(1, "owner-account", disabled=True), NOW, None
    ).plain
    assert "DISABLED" in badges, badges
    assert "WIPED" in badges, badges


# ────────────────────────────────────────────────────────────────────────────
# 8. THE HEADER WAS DRAWING A CARD'S GAUGE — the FOURTH, FIFTH and SIXTH
#    readers of the flag, found by going looking after the third was fixed
# ────────────────────────────────────────────────────────────────────────────
#
# His words: *"Why am i seeing ROOM alice-example-com - 93% of weekly left?
# That's card specific information and it's not in a card. Also that account is
# disabled so it's likely inaccurate."*
#
# TWO DEFECTS IN ONE SENTENCE, and they are fixed in two different files.
#
#   THE FLAG. `Board.room()` ranked every account the daemon reported and never
#   asked whether work could go there. `alice-example-com` has been disabled
#   since 2026-10-02 and was named as the pool's headroom for five days. Going
#   looking for a fourth copy — which section 7 above asks for in so many words,
#   because a latent copy of a just-fixed bug is the next bug — turned up TWO
#   more in the same family: `any_gauged()`, whose disagreement with `room()`
#   made the header's three-way branch print `none — all accounts at limit` when
#   nothing was at limit, and `next_back()`, which named the same disabled
#   account off a login window that CLOSED on 2 October.
#
#   THE PLACE. A per-account gauge reading in the header sits far from the
#   `DISABLED` badge, the `WIPED` badge and the observation stamp that qualify
#   it. A number separated from its qualifiers cannot look wrong, which is how
#   this one survived. It is now on the card, and because `card_lines` returns
#   the simple card for a disabled account before it reaches any body line, a
#   disabled account cannot carry it however the flag is read.
#
# EVERY TEST HERE ASSERTS ON RENDERED STRINGS. A severity tuple and a
# `(Account, float)` pair are not what he reads.


def _gauged(number: int, label: str, weekly_pct: float, **kw) -> Account:
    """An enabled, healthy, gauged account. `weekly_pct` is USAGE, not headroom."""
    from threads_tui.model import AxisState

    a = Account(number=number, label=label, state="live")
    a.axes = {
        "weekly_all": AxisState(
            name="weekly_all",
            pct=weekly_pct,
            observed_at=NOW - timedelta(minutes=1),
            resets_at=NOW + timedelta(days=2),
            calibrated=True,
            confidence=1.0,
            present=True,
        )
    }
    for k, v in kw.items():
        setattr(a, k, v)
    return a


def _room_board(*accounts: Account):
    from threads_tui.model import Board

    board = Board()
    board.accounts = list(accounts)
    return board


def test_the_header_never_names_a_disabled_account_as_the_pools_room():
    """**HIS EXACT LINE, REPRODUCED AND THEN GONE.**

    The fixture is the board he was looking at: `alice-example-com` disabled with
    the most weekly headroom in the pool, beside one enabled account with less.

    THE MUTATION. Delete `if account.disabled: continue` from `Board.room()` and
    this goes red with the string he reported, verbatim:
    `ROOM  alice-example-com — 93% of weekly left`.
    """
    board = _room_board(
        _gauged(1, "alice-example-com", 7.0, disabled=True),
        _gauged(2, "bob-example-com", 40.0),
    )
    line = render.header_line2(board, 120, NOW).plain

    assert "alice-example-com" not in line, line
    assert "93% of weekly left" not in line, line
    # NON-VACUOUS, and this is the pairing that matters: the disabled account IS
    # the one with the most headroom, so with the flag ignored this very render
    # produces the string above. Both absences are findings about this board, not
    # about a string that exists nowhere.
    assert board.room() is not None, "the fixture must HAVE room to prove anything"
    assert board.room()[0].label == "bob-example-com", board.room()
    assert round(100.0 - 7.0) == 93, "the disabled account's headroom is the 93% he saw"


def test_the_header_draws_no_accounts_gauge_even_when_the_leader_is_enabled():
    """**THE OTHER HALF: it is not about `disabled`, it is about the HEADER.**

    A test that only proved the disabled account went unnamed is satisfied by a
    board where every account is disabled. The reading is card detail whoever
    holds it, so an ENABLED leader must not be named in the header either.

    THE MUTATION. Put the `account.label` / `% of weekly left` branch back in
    `header_line2` and this goes red.
    """
    board = _room_board(_gauged(1, "spare-example-com", 12.0))
    line = render.header_line2(board, 120, NOW).plain

    assert "spare-example-com" not in line, line
    assert "of weekly left" not in line, line
    assert "ROOM" not in line, line
    # NON-VACUOUS: there IS room on this board and the card below proves the
    # string is producible from this very account in this very render.
    assert board.room() is not None, board.room()
    card = "\n".join(
        l.plain
        for l in render.card_lines(
            board.accounts[0], 90, selected=False, mode="full", warn_below=7.0,
            extras=None, flashing=None, now=NOW, room_leader=True,
        )
    )
    assert "88% of weekly left" in card, card


def test_the_two_pool_wide_room_sentences_are_kept():
    """**WHAT WAS NOT DELETED, because deleting it would be the easy over-fix.**

    Two of the three ROOM branches never were per-account: "there are no readings
    at all" and "nothing can take work" are facts about the pool and there is no
    card that could carry either. They stay, and the `ROOM` label with them.

    THE MUTATION. Delete the ROOM segment from `header_line2` outright — the
    tempting reading of his complaint — and this goes red twice.
    """
    from threads_tui.model import Board

    empty = Board()
    empty.accounts = [Account(number=1, label="spare-example-com", state="live")]
    line = render.header_line2(empty, 120, NOW).plain
    assert "ROOM" in line, line
    assert "no usage readings available" in line, line

    maxed = _room_board(_gauged(1, "spare-example-com", 100.0))
    line = render.header_line2(maxed, 120, NOW).plain
    assert "ROOM" in line, line
    assert "all accounts at limit" in line, line


def test_a_disabled_account_shows_no_headroom_line_and_an_enabled_one_does():
    """**BOTH DIRECTIONS, IN ONE RENDER. Neither alone is a fix.**

    DIRECTION ONE: the disabled card carries no headroom line. A test that proved
    only this is satisfied by deleting `_room_line` altogether.
    DIRECTION TWO: the enabled leader's card still carries it. A test that proved
    only this is satisfied by never consulting the flag.

    THE MUTATIONS, one per direction. Remove the `account.disabled` early return
    from `card_lines` (or make `Board.room()` ignore the flag AND pass
    `room_leader=True` for the disabled card) and direction one goes red with
    `most room in the pool — 93% of weekly left` inside a disabled card. Delete
    the `_room_line` append and direction two goes red.
    """
    off = _gauged(1, "alice-example-com", 7.0, disabled=True)
    on = _gauged(2, "bob-example-com", 40.0)
    board = _room_board(off, on)

    # The app resolves this ONCE per paint; mirrored here so the fixture goes
    # through the same decision the screen does.
    room = board.room()
    leader = room[0].label if room is not None else None
    assert leader == "bob-example-com", leader

    def render_card(account: Account) -> str:
        return "\n".join(
            l.plain
            for l in render.card_lines(
                account, 90, selected=False, mode="full", warn_below=7.0,
                extras=None, flashing=None, now=NOW,
                room_leader=(leader is not None and account.label == leader),
            )
        )

    disabled_card = render_card(off)
    enabled_card = render_card(on)

    # DIRECTION TWO FIRST, because it is this pair's producibility proof: the
    # needle is a real string that this very code renders, from this very board,
    # at this very width.
    assert "most room in the pool" in enabled_card, enabled_card
    assert "60% of weekly left" in enabled_card, enabled_card

    # DIRECTION ONE. The same needle, the same render pass, absent.
    assert "most room in the pool" not in disabled_card, disabled_card
    assert "of weekly left" not in disabled_card, disabled_card
    assert "93%" not in disabled_card, disabled_card
    # And the disabled card still says what it IS — the skip costs him nothing.
    assert "DISABLED" in disabled_card, disabled_card


def test_the_headroom_line_is_a_comparison_and_not_a_second_copy_of_the_meter():
    """**WHY THE LINE IS NOT THE DUPLICATED SIGNAL THIS FILE KEEPS DELETING.**

    The weekly meter two rows up already says `40%`, and `100 − 40` is the same
    number. What this line adds is the part NO single card can know: that this is
    the most room in the pool. So the comparison must be in the rendered words,
    not merely implied by which card got the line.

    THE MUTATION. Shorten the line to `60% of weekly left` and this goes red.
    """
    on = _gauged(2, "bob-example-com", 40.0)
    card = "\n".join(
        l.plain
        for l in render.card_lines(
            on, 90, selected=False, mode="full", warn_below=7.0, extras=None,
            flashing=None, now=NOW, room_leader=True,
        )
    )
    assert "most room in the pool" in card, card
    # Both halves on one line: the comparison AND the magnitude. "most room" with
    # no number is unactionable when the most is 4%.
    line = next(l for l in card.splitlines() if "most room" in l)
    assert "60% of weekly left" in line, line
    # And the meter it is derived from is still on the card, unchanged — this is
    # an addition, not a re-spelling of the gauge.
    assert "40%" in card, card


def test_next_back_does_not_name_a_disabled_account():
    """**THE FIFTH SITE, and it was live on his screen beside the one he named.**

    `alice-example-com` is disabled and carries a limit whose window ENDED on
    2 October. `next_back()` returned it — earliest `resets_at` of any at-limit
    axis anywhere — and the header drew `NEXT BACK  alice-example-com in 0m`,
    because `fmt_delta` clamps a negative delta to `0m`. A disabled account coming
    back from a limit is not the pool coming back from a limit.

    THE MUTATION. Delete `if account.disabled: continue` from `next_back()` and
    this goes red with `NEXT BACK  alice-example-com in 0m`.
    """
    from threads_tui.model import AxisState, Board

    off = Account(number=1, label="alice-example-com", state="live", disabled=True)
    off.axes = {
        "login": AxisState(
            name="login",
            pct=100.0,
            # 2 October 2026 — five days before the day he reported this.
            resets_at=datetime(2026, 10, 2, 0, 0, tzinfo=timezone.utc),
            observed_at=NOW,
            calibrated=True,
            present=True,
        )
    }
    on = _gauged(2, "bob-example-com", 40.0)

    board = Board()
    board.accounts = [off, on]
    then = datetime(2026, 10, 7, 12, 0, tzinfo=timezone.utc)
    line = render.header_line2(board, 120, then).plain
    assert "alice-example-com" not in line, line
    assert "in 0m" not in line, line
    # NON-VACUOUS: `NEXT BACK` is still drawn on this line (as `—`), so the label
    # had somewhere to appear, and the axis IS at limit with a resets_at, so the
    # needle is producible the moment the flag is ignored.
    assert "NEXT BACK" in line, line
    assert off.axes["login"].at_limit, "the fixture must be at limit to prove anything"
    assert off.axes["login"].resets_at is not None


def test_any_gauged_agrees_with_room_about_which_accounts_count():
    """**THE SIXTH SITE: a disagreement that produced a confident lie.**

    `header_line2` branches three ways and the first two questions are asked of
    DIFFERENT populations unless both honour the flag. With a gauged DISABLED
    account and no gauged enabled one, `any_gauged()` said yes, `room()` said
    None, and the header drew `ROOM none — all accounts at limit`. Nothing was at
    limit; there were no readings for any account work could go to.

    THE MUTATION. Drop the `if not a.disabled` from `any_gauged()` and this goes
    red with `all accounts at limit` on a board where nothing is at limit.
    """
    from threads_tui.model import Board

    board = Board()
    board.accounts = [
        _gauged(1, "alice-example-com", 7.0, disabled=True),
        # Enabled, and NOT gauged: no axes at all.
        Account(number=2, label="spare-example-com", state="live"),
    ]
    line = render.header_line2(board, 120, NOW).plain
    assert "no usage readings available" in line, line
    assert "all accounts at limit" not in line, line
    # NON-VACUOUS: the `all accounts at limit` sentence is reachable from this
    # same function — `test_the_two_pool_wide_room_sentences_are_kept` renders it —
    # and the disabled account here really does hold a reading, which is the input
    # that produced the wrong sentence.
    assert board.accounts[0].has_gauge, "the disabled account must be gauged"
    assert not board.accounts[0].any_at_limit, "and must NOT be at limit"


def test_card_height_agrees_with_card_lines_including_the_headroom_row():
    """**THE PREDICTION MUST MATCH THE THING IT PREDICTS, IN ALL FOUR STATES.**

    `card_height` was one short for every enabled account in both selection
    states: it never counted the pressure row. Nothing in `app.py` calls it (the
    board is laid out from `len(card_lines(...))`), and the only assertion on it
    exercised the DISABLED card, which returns early — so the drift was invisible.
    Adding the headroom term to a wrong total would have preserved the wrongness.

    THE MUTATION. Drop either `+ 1` for pressure, or the `room` term, and one of
    these four goes red.
    """
    on = _gauged(2, "bob-example-com", 40.0)
    for selected in (False, True):
        for leader in (False, True):
            produced = len(
                render.card_lines(
                    on, 100, selected=selected, mode="full", warn_below=7.0,
                    extras=None, flashing=None, now=NOW, room_leader=leader,
                )
            )
            predicted = render.card_height(
                on, selected=selected, mode="full", room_leader=leader
            )
            assert predicted == produced, (
                f"selected={selected} room_leader={leader}: "
                f"card_height {predicted} != card_lines {produced}"
            )
    # AND THE HEADROOM ROW IS WHAT MAKES THE DIFFERENCE A DIFFERENCE: the leader
    # card is exactly one line taller, so the four checks above are not four
    # copies of one case.
    assert (
        render.card_height(on, selected=False, mode="full", room_leader=True)
        == render.card_height(on, selected=False, mode="full", room_leader=False) + 1
    )


def test_compact_mode_carries_no_headroom_row():
    """COMPACT is the density mode and the room line is not worth a row in it —
    but `card_lines` and `card_height` have to agree about that.

    THE MUTATION. Append `_room_line` in the COMPACT branch of `card_lines`
    without touching `card_height` and this goes red.
    """
    on = _gauged(2, "bob-example-com", 40.0)
    lines = render.card_lines(
        on, 90, selected=False, mode=render.COMPACT, warn_below=7.0, extras=None,
        flashing=None, now=NOW, room_leader=True,
    )
    assert len(lines) == render.card_height(
        on, selected=False, mode=render.COMPACT, room_leader=True
    )
    assert not any("most room" in l.plain for l in lines), [l.plain for l in lines]
    # NON-VACUOUS: the same account, same flag, in FULL mode, DOES carry it.
    full = render.card_lines(
        on, 90, selected=False, mode="full", warn_below=7.0, extras=None,
        flashing=None, now=NOW, room_leader=True,
    )
    assert any("most room" in l.plain for l in full), [l.plain for l in full]


# ────────────────────────────────────────────────────────────────────────────
# 9. `dead` MEANT TWO UNRELATED THINGS — the rename, in the strings he reads
# ────────────────────────────────────────────────────────────────────────────
#
# His words: *"if I understand, sounds like fields need to be named more
# appropriately."* He is answering the diagnosis in section 7, where he had been
# told two accounts were dead and nothing was.
#
# THE COLLISION, EXACTLY.
#
#   `STATE_NOTE["wiped"]` began `DEAD — ` and described a CREDENTIAL STORE that
#   `claude` cleared. Login health. Nothing is dead; one sign-in fixes it; and
#   the badge beside it on the same card already said `WIPED`.
#
#   `Activity.dead_since` is the DAEMON saying an account stopped answering, and
#   rendered `✕ dead 9m`.
#
# He read the first and understood the second. `dead_since` was NULL on all four
# accounts, so what he acted on was a word rather than a fact.
#
# WHAT WAS CHOSEN, AND WHY NOT TWO SPELLINGS OF ONE WORD. `WIPED` and
# `NOT ANSWERING` share no stem, no length and no shape, so there is no reading
# speed at which one becomes the other — which `dead`/`died` would not have given.
# Each names what was OBSERVED rather than diagnosing it, and neither claims
# anything is unrecoverable, which is exactly what `DEAD` claimed about the one
# state on that table a single sign-in fixes.
#
# AND A THIRD `dead` WENT WITH THEM: the warning SEVERITY, which is a rendering
# instruction and not a fact about an account, and which sat in the tuple right
# beside the prose he misread. It is `model.CRITICAL` now.
#
# THE PRODUCER HAS ITS OWN COPY AND IT IS NOT TOUCHED. See the report and the
# note above `STATE_NOTE`: `lib/threads/src/accounts.rs::SlotHealth::Wiped
# .summary()` returns a string beginning `"DEAD — "`, read by the
# `threads accounts health` CLI. It does not reach this board.


def test_the_wiped_store_is_called_wiped_everywhere_he_reads_it():
    """**THE PROSE AND THE BADGE NOW USE THE SAME WORD, which is the fix.**

    Asserted on the RENDERED card and the RENDERED board line, not on
    `STATE_NOTE`, because a dict entry is not what he reads.

    THE MUTATION. Put `DEAD — ` back at the front of `STATE_NOTE["wiped"]` and
    both halves of this go red.
    """
    account = _wiped(4, "spare-example-com")

    card = "\n".join(
        l.plain
        for l in render.card_lines(
            account, 90, selected=True, mode="full", warn_below=7.0, extras=None,
            flashing=None, now=NOW,
        )
    )
    assert "WIPED" in card, card
    assert "this slot's own login was cleared" in card, card
    # NON-VACUOUS: the sentence IS on this card (the clause above proves it), so
    # `DEAD` had a line to appear on and does not.
    assert "DEAD" not in card, card

    board = _warn_board(account)
    line = render.warning_line(*board.warnings()[0], 200).plain
    assert "WIPED" in line, line
    assert "DEAD" not in line, line
    # The badge and the prose agree, which they did not before.
    assert "WIPED" in render._badges(account, NOW, None).plain


def test_an_account_that_is_not_answering_says_so_on_the_badge_and_the_board():
    """**`✕ dead 9m` IS `✕ not answering 9m`, and the board line matches it.**

    One fact, one wording, in both places it is drawn. Before this the badge said
    `dead` and the board line said `is not answering` about the same field — which
    is how a reader concludes they are two different problems.

    THE MUTATION. Restore `f"✕ dead {age}"` in `render._badges` and this goes red.
    """
    from threads_tui.model import Activity

    silent = Activity(
        known=True,
        not_answering_since=NOW - timedelta(minutes=9),
        not_answering_reason="socket closed",
    )
    account = Account(number=3, label="bob-example-com", state="live")
    account.activity = silent

    badges = render._badges(account, NOW, None).plain
    assert "not answering" in badges, badges
    assert "9m" in badges, badges
    # NON-VACUOUS: the badge is populated and carries the age, so `dead` had its
    # slot and is absent.
    assert "dead" not in badges.lower(), badges

    line = render.warning_line(*_warn_board(account).warnings()[0], 200).plain
    assert "NOT ANSWERING" in line, line
    assert "socket closed" in line, line
    assert "dead" not in line.lower(), line

    # THE FIELD IS RENAMED TOO, and the old name is gone rather than aliased — an
    # alias is how both spellings survive and how the next reader picks the wrong
    # one.
    assert not hasattr(silent, "dead_since") or "dead_since" not in type(
        silent
    ).__dataclass_fields__, "the old field name is still part of Activity"
    assert "not_answering_since" in type(silent).__dataclass_fields__


def test_the_warning_severity_is_not_a_third_meaning_of_dead():
    """**A SEVERITY SAYS HOW LOUD, NEVER WHAT HAPPENED.**

    THE MUTATION. Change `model.CRITICAL` back to `"dead"` and the sweep below
    goes red; change `render.warning_line`'s test to `== "dead"` without changing
    `model.CRITICAL` and every `✕` line on the board silently becomes an amber
    `⚠` — which is why the constant is shared rather than spelled at each site.
    """
    assert model.CRITICAL == "critical"
    # The glyph and colour are UNCHANGED — this was a rename, not a re-styling.
    line = render.warning_line(model.CRITICAL, "something broke", 80).plain
    assert line.startswith(" ✕ "), line
    # And an unrecognised severity still falls through to the warning triangle,
    # so a stale `"dead"` string somewhere would be visible rather than silent.
    stale = render.warning_line("dead", "something broke", 80).plain
    assert stale.startswith(" ⚠ "), stale


# ── THE SOURCE SWEEP: no rendered literal in `threads_tui/` says `dead` ──────
#
# `ast`, not grep: a grep cannot tell a string literal from the word in a comment,
# and this file's subject matter is PROSE, of which there is a great deal in the
# comments here and all of it legitimately about the old name.
#
# WHAT IS SWEPT: every string constant in `threads_tui/*.py` that is NOT a
# docstring. Docstrings are excluded because they are documentation — several of
# them discuss `dead-fold` and the history of this rename, and must.
#
# THE CARVE-OUTS, NAMED AND REASONED, because an unexplained allowlist is how a
# sweep stops sweeping:
#
#   `shapes.py`  `DEAD = "dead"` and the kind comparisons against it. This is the
#                THREE-VALUED CELL STATE — a fact about a SHAPE, not an account —
#                and it is a machine token that is never rendered: every string
#                the operator sees for it says `FOLD DIED` or `✕`, which names the
#                fold and cannot be read as an account's health.
#
# `dead_since` / `dead_reason` are NOT carve-outs and need none: the pattern is
# the whole word, and `_` is a word character, so a wire key does not match. That
# is deliberate — those are the producer's column names and this client does not
# get to rename them.

WORD_DEAD = __import__("re").compile(r"\bdead\b", __import__("re").IGNORECASE)


def _literals(include_docstrings: bool = False):
    """Every string constant in `threads_tui/*.py`, with its file and line."""
    import ast

    pkg = pathlib.Path(__file__).resolve().parent.parent / "threads_tui"
    for path in sorted(pkg.glob("*.py")):
        tree = ast.parse(path.read_text())
        docstrings = set()
        if not include_docstrings:
            for node in ast.walk(tree):
                if isinstance(
                    node,
                    (ast.Module, ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef),
                ):
                    body = getattr(node, "body", None)
                    if (
                        body
                        and isinstance(body[0], ast.Expr)
                        and isinstance(body[0].value, ast.Constant)
                        and isinstance(body[0].value.value, str)
                    ):
                        docstrings.add(id(body[0].value))
        for node in ast.walk(tree):
            if (
                isinstance(node, ast.Constant)
                and isinstance(node.value, str)
                and id(node) not in docstrings
            ):
                yield path.name, node.lineno, node.value


def test_no_rendered_literal_in_the_package_says_dead():
    """**THE SWEEP, and it is the thing that keeps the rename from coming back.**

    THE MUTATION. Restore `f"✕ dead {age}"` or `"DEAD — claude cleared…"` and this
    goes red naming the file and line.
    """
    offenders = [
        (name, lineno, value)
        for name, lineno, value in _literals()
        if WORD_DEAD.search(value) and not (name == "shapes.py" and value == "dead")
    ]
    assert offenders == [], offenders

    # ── POSITIVE SENTINEL ONE: the scanner sees literals that ARE there ───────
    #
    # Without this the assertion above passes just as well on a scanner that
    # yields nothing at all — which is the exact defect this suite has found
    # nineteen times across five languages.
    found_wiped = [
        (name, lineno)
        for name, lineno, value in _literals()
        if "WIPED" in value
    ]
    assert found_wiped, "the scanner found no `WIPED` literal; it is not looking"
    assert any(name == "model.py" for name, _ in found_wiped), found_wiped

    # ── POSITIVE SENTINEL TWO: it sees the needle itself when it is present ───
    #
    # The carve-out is the proof: `shapes.py`'s `"dead"` token MATCHES the
    # pattern and is excluded by name, so the pattern demonstrably fires on a real
    # literal in this very package. A pattern that matched nothing anywhere would
    # fail here.
    matched = [
        (name, lineno, value)
        for name, lineno, value in _literals()
        if WORD_DEAD.search(value)
    ]
    assert matched, "the pattern matches no literal at all; it cannot be finding anything"
    assert all(name == "shapes.py" for name, _, _ in matched), matched

    # ── AND THE DOCSTRING EXCLUSION IS DOING WORK, not hiding an empty scan ───
    #
    # Including docstrings finds STRICTLY MORE hits, every one of them prose about
    # this rename or about a dead FOLD. If this were equal, the exclusion would be
    # a no-op and the first assertion would be weaker than it reads.
    with_docs = [
        (name, lineno)
        for name, lineno, value in _literals(include_docstrings=True)
        if WORD_DEAD.search(value)
    ]
    assert len(with_docs) > len(matched), (
        "excluding docstrings changed nothing, so the exclusion is untested",
        with_docs,
    )


# ── H179: WIPED HAS TO SAY WHAT IT MEANS, AND WHY THE MACHINE LOGIN DOESN'T FIX IT ─
#
# He saw `alice-example-com` wearing `WIPED` beside a healthy `machine store:` line
# for the SAME account and read it as a stale board. The two are separate stores:
# the slot's own login (what work placed on the slot runs under) is wiped; his
# machine login is fine. The note now says that, and says what to press.


def _h179_card(account: Account) -> str:
    return "\n".join(
        l.plain
        for l in render.card_lines(
            account, 120, selected=True, mode="full", warn_below=7.0, extras=None,
            flashing=None, now=NOW,
        )
    )


def test_the_wiped_note_says_what_happened_what_it_costs_and_what_to_press():
    """THE MUTATION. Restore the old `STATE_NOTE["wiped"]` and this goes red."""
    account = _wiped(2, "alice-example-com")
    card = _h179_card(account)
    assert "no work can run on it" in card, card
    assert "press L" in card, card
    assert "claude-accounts login alice-example-com" in card, card

    line = render.warning_line(*_warn_board(account).warnings()[0], 300).plain
    assert "no work can run on it" in line, line
    assert "press 2, then L" in line, line
    assert "claude-accounts login alice-example-com" in line, line


def test_a_wiped_slot_whose_machine_login_is_live_says_the_two_stores_differ():
    """THE OPERATOR'S EXACT CASE: is_live_login, slot wiped, machine store live.

    THE MUTATION. Drop the machine-login branch from `Account.health_note` and
    both the card and the board line lose the sentence that connects the
    `WIPED` badge to the healthy `machine store:` line beside it.
    """
    account = _wiped(
        2, "alice-example-com",
        is_live_login=True, live_store_state="live", live_store_days_left=200.0,
    )
    card = _h179_card(account)
    assert "WIPED" in card, card
    assert "machine store:" in card, card
    assert "your machine login (same account) is fine" in card, card
    assert "this slot's own login is wiped" in card, card
    assert "work placed here can't run" in card, card
    assert "claude-accounts login alice-example-com" in card, card

    line = render.warning_line(*_warn_board(account).warnings()[0], 300).plain
    assert "your machine login (same account) is fine" in line, line
    assert "this slot's own login is wiped" in line, line
    assert "press 2, then L" in line, line

    # NON-VACUOUS: the specific sentence is only for the live-machine-store case.
    plain = _wiped(3, "other", is_live_login=False)
    assert "your machine login" not in _h179_card(plain)
    dead_store = _wiped(4, "x", is_live_login=True, live_store_state="wiped")
    assert "your machine login" not in _h179_card(dead_store)
