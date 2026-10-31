"""**THE LOGIN COUNTDOWN COUNTS DOWN TO THE LOGIN.**

Every card on this board carries `login Nd` in its title, and on 2026-09-22 every
card on the operator's live board read `login 0d`. Measured against the running
daemon at that moment:

    NOW                      2026-09-22T20:50:18Z
    access_expires_at_ms  -> 2026-09-22T20:52:22Z   two minutes away
    refresh_expires_at_ms -> 2026-10-19T10:08:29Z   27 days away

The board was subtracting `access_expires_at_ms`. That is the OAuth **access**
token: it lives a couple of hours, Claude Code refreshes it lazily and
automatically, and it is dead-and-replaced several times a day in the normal
working of an account that is perfectly healthy. Counting down to it is
arithmetically flawless and semantically empty — `0d` on a card said nothing
except "some time in the next 24 hours a refresh will happen", which is always
true.

The field that means what the card claims is `refresh_expires_at_ms`. When the
REFRESH token expires the login is actually gone and the operator has to open a
browser. That is the deadline worth a countdown, it is the one the retired
`accounts/health` path showed him (~29d), and the daemon's own
`accounts::days_left` reads exactly that field — with a docstring that says so:
*"Days until the LOGIN (not the access token) expires."*

So this file pins the FIELD, not just the number. Three of these tests would
stay green against a board that had simply been handed a different fixture; the
fourth reads the source and fails if `_days_until` is ever wired back onto an
access expiry, which is the regression that cannot be caught by a value.

`access_expired` is checked here too, and the answer is that it is NOT a card
state: it is the daemon's rotation lever (`accounts::access_expired` — "the only
lever rotation has"), true on healthy accounts many times a day. A card that
went red on it would be crying wolf on schedule.

Run: .venv/bin/python -m pytest tests/test_expiry.py
"""

from __future__ import annotations

import ast
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import fake_daemon  # noqa: E402

from threads_tui import render  # noqa: E402

NOW = fake_daemon.NOW
UTC = timezone.utc

# The live measurement, to the second, as the header of this file records it.
LIVE_NOW = datetime(2026, 9, 22, 20, 50, 18, tzinfo=UTC)
LIVE_ACCESS = datetime(2026, 9, 22, 20, 52, 22, tzinfo=UTC)
LIVE_REFRESH = datetime(2026, 10, 19, 10, 8, 29, tzinfo=UTC)


def _ms(dt: datetime) -> int:
    return int(dt.timestamp() * 1000)


def _account(**cred):
    board = fake_daemon.board_of(
        fake_daemon.export(
            "acct0", r=fake_daemon.row("acct0", credential=fake_daemon.credential(**cred))
        ),
        now=cred.pop("_now", None) or NOW,
    )
    return board.by_label("acct0")


def _account_at(now: datetime, **cred):
    board = fake_daemon.board_of(
        fake_daemon.export(
            "acct0", r=fake_daemon.row("acct0", credential=fake_daemon.credential(**cred))
        ),
        now=now,
    )
    return board.by_label("acct0")


# ────────────────────────────────────────────────────────────────────────────
# 1. THE MEASUREMENT THAT CAUGHT IT
# ────────────────────────────────────────────────────────────────────────────


def test_the_live_reading_that_was_wrong_now_reads_twenty_seven_days():
    """**The exact numbers off the running daemon, replayed.**

    THE MUTATION. Point `model.project` back at `access_expires_at_ms` and this
    goes red with `days_left` of 0.0014 — which rendered as `login 0d`, which is
    what was on every card.
    """
    a = _account_at(
        LIVE_NOW,
        access_expires_at_ms=_ms(LIVE_ACCESS),
        refresh_expires_at_ms=_ms(LIVE_REFRESH),
    )
    assert a.days_left is not None
    assert round(a.days_left) == 27, (
        f"the card counts down to the wrong token: {a.days_left} days — the "
        "access token is two minutes away and the LOGIN is 27 days away"
    )
    title = render._title_right(a, warn_below=4.0).plain
    assert title == "login 27d", title
    assert "0d" not in title
    assert "EXPIRED" not in title


def test_an_access_token_already_dead_is_not_an_expired_login():
    """**The refresh is what expires a login, and it has not.**

    An access token that is already in the past is the NORMAL state of an
    account that has not been spoken to for a few hours — Claude Code refreshes
    lazily, on use, so the stale access token is the trigger for the refresh
    rather than a failure. The board rendering `EXPIRED` here would send the
    operator to sign in an account that is completely fine.

    THE MUTATION. Read the access field and this goes red twice: `days_left`
    goes negative and `fmt_days` turns a negative into the word `EXPIRED`.
    """
    a = _account_at(
        LIVE_NOW,
        access_expires_at_ms=_ms(LIVE_NOW - timedelta(hours=3)),
        refresh_expires_at_ms=_ms(LIVE_NOW + timedelta(days=27)),
        access_expired=True,
        slot_state="live",
    )
    assert a.days_left is not None and a.days_left > 26
    assert render._title_right(a, warn_below=4.0).plain == "login 27d"


def test_access_expired_is_not_rendered_as_a_card_state():
    """**`access_expired: true` changes nothing on the card.**

    It is the daemon's rotation lever and it is true of healthy accounts
    routinely. The card's health words come from `slot_state`, which the daemon
    derives from the REFRESH expiry (`accounts::classify_slot`).

    THE MUTATION. Fold `access_expired` into the badge or the countdown and this
    goes red — the two cards below stop rendering identically.
    """
    fine = _account_at(LIVE_NOW, refresh_expires_at_ms=_ms(LIVE_NOW + timedelta(days=27)),
                       access_expires_at_ms=_ms(LIVE_NOW + timedelta(hours=1)),
                       access_expired=False)
    stale = _account_at(LIVE_NOW, refresh_expires_at_ms=_ms(LIVE_NOW + timedelta(days=27)),
                        access_expires_at_ms=_ms(LIVE_NOW - timedelta(hours=1)),
                        access_expired=True)
    assert fine.days_left == stale.days_left
    assert render._title_right(fine, 4.0).plain == render._title_right(stale, 4.0).plain
    assert render._badges(fine, LIVE_NOW).plain == render._badges(stale, LIVE_NOW).plain


def test_a_login_with_no_refresh_expiry_says_so_and_never_falls_back():
    """**No refresh expiry is `—`, not the access token's number.**

    An older login, or one the server never dated, carries no
    `refreshTokenExpiresAt` — the daemon's own `days_left` answers `None` there.
    A fallback to the access expiry would put the defect back for exactly the
    accounts where nobody would notice: it would print a plausible small number
    where the honest answer is "this login has no stated end".

    THE MUTATION. Add `or cred.get("access_expires_at_ms")` to the projection and
    this goes red — the card reads `login 12d` for a login with no expiry at all.
    """
    a = _account_at(
        NOW,
        refresh_expires_at_ms=None,
        access_expires_at_ms=_ms(NOW + timedelta(days=12)),
    )
    assert a.days_left is None, f"a fallback was taken: {a.days_left}"
    assert render._title_right(a, 4.0).plain == "login —"


def test_the_warning_threshold_now_measures_the_thing_worth_warning_about():
    """**`warn_below_days` against the LOGIN, which is what it was for.**

    Ruling 6 put the threshold on the shared block so no consumer hardcodes it.
    Against the access token it was meaningless in the other direction: EVERY
    account was permanently inside a 4-day warning band, so the amber that
    exists to say "sign this in soon" was on all the time and said nothing.

    THE MUTATION. Read the access field and every account below goes amber.
    """
    soon = _account_at(NOW, refresh_expires_at_ms=_ms(NOW + timedelta(days=2)))
    later = _account_at(NOW, refresh_expires_at_ms=_ms(NOW + timedelta(days=20)))
    soon_style = str(render._title_right(soon, 4.0).style)
    later_style = str(render._title_right(later, 4.0).style)
    assert soon_style != later_style, "a login two days from the end is not styled apart"
    assert render._title_right(soon, 4.0).plain == "login 2d"
    assert render._title_right(later, 4.0).plain == "login 20d"


# ────────────────────────────────────────────────────────────────────────────
# 2. THE MACHINE STORE'S OWN COUNTDOWN — the same confusion, repeated
# ────────────────────────────────────────────────────────────────────────────


def test_the_machine_store_countdown_is_the_machine_stores_login_not_its_access_token():
    """**`machine store: Nd left` is a login countdown too.**

    `credential.live_store_expires_at_ms` is `SlotReport::live_store_access_
    expires_at_ms` — the ACCESS expiry of the machine-default store, named
    honestly on the wire and read dishonestly here. The machine store's LOGIN
    expiry is on the wire as well: the `machine_default` note that lands on this
    same row (ruling 5) carries its own `refresh_expires_at_ms`.

    THE MUTATION. Point `live_store_days_left` back at
    `credential.live_store_expires_at_ms` and this goes red at `0d` — the same
    `0d` the login line was showing.
    """
    a = _machine_login_account(now=LIVE_NOW)
    assert a.is_live_login is True
    assert a.live_store_days_left is not None
    assert round(a.live_store_days_left) == 28, a.live_store_days_left
    card = "\n".join(
        line.plain
        for line in render.card_lines(
            a, 100, selected=True, mode="full", warn_below=4.0,
            extras=None, flashing=None, now=LIVE_NOW,
        )
    )
    assert "machine store: 28d left" in card, card


def _machine_login_account(*, now: datetime):
    """The live-login row exactly as the daemon builds it.

    `mark_live_login` puts `is_live_login` and `live_store_*` on the pool account
    the machine store folds into, and `hydrates` puts the `machine_default` NOTE
    on that same row. Both halves are here because the fix reads one and the
    defect read the other.
    """
    board = fake_daemon.board_of(
        fake_daemon.export(
            "acct0",
            r=fake_daemon.row(
                "acct0",
                credential=fake_daemon.credential(
                    is_live_login=True,
                    access_expires_at_ms=_ms(now + timedelta(minutes=2)),
                    refresh_expires_at_ms=_ms(now + timedelta(days=27)),
                    live_store_expires_at_ms=_ms(now + timedelta(minutes=2)),
                    live_store_state="live",
                ),
                machine_default=fake_daemon.machine_default_note(
                    access_expires_at_ms=_ms(now + timedelta(minutes=2)),
                    refresh_expires_at_ms=_ms(now + timedelta(days=28)),
                ),
            ),
        ),
        now=now,
    )
    return board.by_label("acct0")


# ────────────────────────────────────────────────────────────────────────────
# 3. THE PIN THAT SURVIVES A CHANGE OF FIXTURE
# ────────────────────────────────────────────────────────────────────────────


def test_no_countdown_anywhere_on_this_board_is_derived_from_an_access_expiry():
    """**Read off the SOURCE, because a value test can be fixtured green.**

    Every test above asserts a number, and every one of them would pass against
    a board that read the access field if the fixture happened to carry the same
    instant in both. This one walks `model.py`'s AST, finds every call to
    `_days_until` — the one place a countdown is formed — and fails if any of
    them is handed an access expiry.

    THE MUTATION. Put `cred.get("access_expires_at_ms")` back into any
    `_days_until` call and this goes red naming the line.
    """
    import threads_tui.model as model_mod

    source = Path(model_mod.__file__).read_text()
    tree = ast.parse(source)
    calls = [
        node
        for node in ast.walk(tree)
        if isinstance(node, ast.Call)
        and isinstance(node.func, ast.Name)
        and node.func.id == "_days_until"
    ]
    assert calls, "`_days_until` is the one place a countdown is formed; it vanished"
    offenders = []
    for call in calls:
        arg = ast.unparse(call.args[0]) if call.args else ""
        if "access_expires_at_ms" in arg:
            offenders.append(f"model.py:{call.lineno}: _days_until({arg})")
    assert not offenders, (
        "a countdown is being derived from an ACCESS token expiry:\n  "
        + "\n  ".join(offenders)
        + "\nThe access token refreshes itself every couple of hours; the field "
        "that means 'how long until this login is gone' is refresh_expires_at_ms."
    )

    # And the field IS read, so the pin cannot be satisfied by deleting the
    # countdown altogether.
    read = [ast.unparse(c.args[0]) for c in calls if c.args]
    assert any("refresh_expires_at_ms" in r for r in read), (
        f"nothing reads the refresh expiry: {read}"
    )
