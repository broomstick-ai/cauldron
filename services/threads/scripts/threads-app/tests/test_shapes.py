"""**THE SHAPE MIRROR** — the board drawn from the daemon's own fold.

WHAT THIS FILE PINS, in the order the risks actually rank.

  1. **S2a: AN OLD DAEMON IS NOT DEGRADED.** `shapes/list` answers
     `unknown method:` and everything about this board stays exactly as it was
     — the five queries, `live.py`'s lease, the placement worker. This is not a
     hypothetical: it is the daemon running on this machine right now, and it
     is what the board will be talking to until the new binary is promoted.
     The whole shape path must be invisible until it is not.

  2. **THE THREE-VALUED UNKNOWN RENDERS AS THREE THINGS.** `Known`,
     `NotHydrated` and `Dead`. The operator's ruling, and the reason it is a
     ruling: a dead fold must never look like an empty account or a quiet one.
     Two of these tests would have passed against a two-valued client, so they
     assert all three pairwise-distinct rather than asserting "shows a `?`".

  3. **KEY ENUMERATION.** One keyless `shapes/{shape}/subscribe` is what hydrates the
     shape, enumerates its keys AND registers this connection for their live
     frames. `shapes/list` cannot do it — a lazy shape holds no keys until
     something subscribes — and a board that believed otherwise would subscribe
     to nothing and sit quiet forever.

  4. **THE FOLD IS NOT RE-IMPLEMENTED HERE.** A row arrives and is held. A
     `gone` removes. A count is a `count` over held rows. There is no pressure
     formula, no placement lookup, no lease and no age on this side.

The pure half runs under pytest with no daemon, no socket and no sleeping. The
socket half stands up a real WebSocket server in the style of `test_place.py`
and drives the real `BoardApp` through Textual's pilot. **`pilot.press` is
called exactly once in this file** — in the dead-fold scenario, where the whole
claim is that the operator has to ASK for the fallback and does not get it
silently. Everywhere else a keystroke would disprove the thing being tested.

Run:  .venv/bin/python tests/test_shapes.py   (from threads-app/)
      .venv/bin/python -m pytest -q tests/test_shapes.py
"""

from __future__ import annotations

import asyncio
import json
import os
import sys
from datetime import datetime, timedelta, timezone

sys.path.insert(0, __file__.rsplit("/tests/", 1)[0])
sys.path.insert(0, __file__.rsplit("/test_shapes.py", 1)[0])

import fake_daemon  # noqa: E402

from threads_tui import model, render, shapes  # noqa: E402


# ═══════════════════════════════════════════════════════════════════════════
# THE FRAME BUILDERS LIVE IN `fake_daemon.py` NOW.
#
# They were written here and three other files came to need them — the fixture
# for every board in the suite is a shape export since S3 deleted
# `build_board`. Four copies of the wire would be four contracts to drift
# apart, so there is one, it is `tests/fake_daemon.py`, and it is not named
# `test_*` so no runner collects it.
# ═══════════════════════════════════════════════════════════════════════════

from fake_daemon import (  # noqa: E402
    NOW,
    SHARED,
    T0,
    cells,
    credential,
    death,
    export,
    gauge_axis,
    known,
    live,
    nonidle_with,
    not_hydrated,
    roster_death,
    roster_export,
    roster_live,
    row,
    thread_row,
)

UTC = timezone.utc


def mirror_with(*frames: dict) -> shapes.ShapeMirror:
    m = shapes.ShapeMirror()
    for frame in frames:
        m.on_frame(frame)
    return m


def board_of(*frames: dict) -> model.Board:
    return model.project(mirror_with(*frames), now=NOW, non_idle=nonidle_with(*frames))


# ═══════════════════════════════════════════════════════════════════════════
# THE PURE HALF
# ═══════════════════════════════════════════════════════════════════════════


def test_an_export_is_held_and_its_keys_are_the_account_count() -> None:
    """The count is `len(keys)`, and every key has a row BY CONSTRUCTION.

    The legacy path joins `accounts/health`'s slots to `accounts/status`'s rows
    by label and drops what does not match, which is how four accounts can
    render as zero. Under the shape the daemon exports the row WITH the key, so
    a key without a row is not a state that exists.
    """
    m = mirror_with(export("acct0"), export("acct1"))
    assert m.keys() == ["acct0", "acct1"]
    assert m.exported is True
    assert m.live is True
    assert all(m.row(k).get("account") == k for k in m.keys())


def test_a_row_upserts_and_a_gone_removes() -> None:
    m = mirror_with(export("acct0"))
    changed = m.on_frame(
        live("acct0", {"kind": "row", "row": row("acct0", login_dir="/moved")})
    )
    assert changed is True
    assert m.row("acct0")["login_dir"] == "/moved"
    assert m.on_frame(live("acct0", {"kind": "gone"})) is True
    assert m.keys() == []
    # A second `gone` for a key already removed is not a change. The board
    # repaints on True, and a repaint per redundant frame is how a 50/s stream
    # becomes a 50/s redraw.
    assert m.on_frame(live("acct0", {"kind": "gone"})) is False


def test_a_transition_carries_the_frames_stamp_onto_the_item() -> None:
    """`Sequence::take_pending` serialises the item ALONE — no `at`.

    The export's `to_json` puts `at` on every retained item, so without this
    graft an appended move and an exported move would have different shapes and
    every consumer counting `moves` would have to know which arrived how.
    """
    m = mirror_with(export("acct0"))
    m.on_frame(
        live(
            "acct0",
            {
                "kind": "transition",
                "cell": "moves",
                "item": {
                    "thread_id": "t9",
                    "direction": "off",
                    "other": "acct1",
                    "reason": "failover",
                },
            },
            at="2026-09-19T11:59:30+00:00",
        )
    )
    moves = m.row("acct0")["moves"]
    assert len(moves) == 1
    assert moves[0]["at"] == "2026-09-19T11:59:30+00:00"
    assert moves[0]["direction"] == "off"


def test_a_shared_move_replaces_the_shape_level_scalars() -> None:
    """Ruling 6's home. `warn_below_days` is read, never hardcoded."""
    m = mirror_with(export("acct0"))
    assert model.project(m, now=NOW).warn_below_days == 4.0
    m.on_frame(
        live(None, {"kind": "shared", "shared": {"warn_below_days": 9.0, "folds_into": {}}})
    )
    assert model.project(m, now=NOW).warn_below_days == 9.0


def test_a_frame_for_another_shape_is_ignored_and_does_not_raise() -> None:
    m = mirror_with(export("acct0"))
    other = dict(export("acct0"), shape="budgets")
    assert m.on_frame(other) is False
    assert m.keys() == ["acct0"]


def test_reset_drops_everything_including_the_export_flag() -> None:
    """A mirror is valid only for the connection it was exported to."""
    m = mirror_with(export("acct0"))
    m.reset()
    assert m.keys() == []
    assert m.exported is False
    assert m.live is False


# ── the three-valued unknown, at the state level ─────────────────────────────


def test_the_three_states_are_three_states() -> None:
    c = cells(credential=not_hydrated("no slot read"))
    m = mirror_with(export("acct0", c=c))
    assert m.cell("acct0", "registered").known
    assert m.cell("acct0", "credential").not_hydrated
    assert m.cell("acct0", "credential").reason == "no slot read"
    m.on_frame(death("the shape task panicked"))
    assert m.cell("acct0", "registered").dead
    # AND THE ROWS SURVIVE. A board that empties itself on a dead fold looks
    # exactly like a board whose accounts all went away.
    assert m.row("acct0")["account"] == "acct0"


def test_a_cell_the_export_never_carried_is_not_hydrated_not_known() -> None:
    """Absence of provenance is never read as `Known`."""
    m = mirror_with(export("acct0", c={"registered": known()}))
    assert m.cell("acct0", "gauge").not_hydrated
    assert m.cell("acct0", "gauge").known is False


def test_an_unrecognised_state_word_is_not_treated_as_known() -> None:
    """The daemon may grow a fourth word. Guessing it means `fine` is the bug."""
    m = mirror_with(export("acct0", c=cells(gauge={"state": "quantum"})))
    assert m.cell("acct0", "gauge").not_hydrated


def test_the_death_export_reaches_a_mirror_holding_any_key() -> None:
    """One keyless send, every mirror. `epoch.state` is the whole message."""
    m = mirror_with(export("acct0"), export("acct1"))
    assert m.on_frame(death()) is True
    assert m.dead is True
    assert m.live is False
    assert all(m.cell(k, "credential").dead for k in m.keys())


# ── the projection ───────────────────────────────────────────────────────────


def test_the_placement_is_the_key_the_row_lives_under() -> None:
    """No lookup, no lease, no residual. One group-by over the roster IS the answer.

    Since R2.S8 the placement is each `threads/non-idle` entry's `account`
    rather than the `accounts` key a row lived under; the card's numbers are
    the same derived ops over the same facts, and `queued_messages` is the
    roster's restored `queued` summed per card.
    """
    board = board_of(
        export("acct0"),
        export("acct1"),
        roster_export({
            "acct0": {"t0": thread_row(), "t1": thread_row()},
            "acct1": {"t2": thread_row(state="queued", executing=False, queued=3)},
        }),
    )
    a0 = board.by_label("acct0")
    a1 = board.by_label("acct1")
    assert a0.activity.executing == 2
    assert sorted(a0.activity.executing_thread_ids) == ["t0", "t1"]
    assert a1.activity.executing == 0
    assert a1.activity.queued_threads == 1
    assert a1.activity.queued_messages == 3


def test_a_thread_placed_on_the_machine_default_is_drawn_on_the_card_it_folds_into() -> None:
    """**THE ROSTER CARRIES THE PLACEMENT THE DAEMON REPORTED; THE CARD IS THE FOLD'S.**

    `accounts` used to file a thread placed on `default` under the account its
    shared `folds_into` names, because the placement WAS the key. The roster
    carries `account: "default"`, so the board applies the same map off the same
    `shared` block or the thread vanishes from every card.

    THE MUTATION. `roster_by_account` with `fold.get(...)` replaced by the raw
    `entry["account"]`: acct0's count drops to 0 and the first assert fires.
    """
    folded = dict(SHARED, folds_into={"default": "acct0"})
    frame = export("acct0")
    frame["shared"] = folded
    board = board_of(
        frame,
        roster_export({"default": {"t0": thread_row()}, "acct0": {"t1": thread_row()}}),
    )
    assert board.by_label("acct0").activity.executing == 2
    assert sorted(board.by_label("acct0").activity.executing_thread_ids) == ["t0", "t1"]


def test_days_left_is_subtracted_from_an_instant_at_projection_time() -> None:
    """There is no `days_left` on the wire and there must never be one again.

    A day earlier and the same payload yields a day more. That is the whole
    point of killing `age`: the number is the caller's subtraction, not a
    figure the daemon baked in when it happened to serialise.
    """
    frames = [export("acct0")]
    today = board_of(*frames).by_label("acct0")
    tomorrow = model.project(
        mirror_with(*frames), now=NOW + timedelta(days=1), non_idle=nonidle_with(*frames)
    ).by_label("acct0")
    assert round(today.days_left) == 12
    assert round(tomorrow.days_left) == 11


def test_pressure_is_rendered_not_computed() -> None:
    """The board holds the daemon's number and has no formula to disagree with."""
    board = board_of(export("acct0"))
    a = board.by_label("acct0")
    assert a.pressure == 1.4
    assert a.pressure_ranked is True
    assert a.in_pressure_pool is True
    assert a.pool_share == 0.5
    # An UNRANKED account is not a zero-pressure one.
    unranked = board_of(
        export(
            "acct1",
            r=row(
                "acct1",
                pressure={
                    "account": "acct1",
                    "ranked": False,
                    "pressure": None,
                    "unranked_reason": "no observation",
                    "disabled": False,
                    "share": 0.5,
                    "axis": "weekly-all",
                },
            ),
        )
    ).by_label("acct1")
    assert unranked.pressure is None
    assert unranked.pressure_ranked is False
    assert unranked.pressure_reason == "no observation"


def test_an_unregistered_key_is_still_drawn() -> None:
    """A login that left the roster with work on it is a fact, not a hole."""
    board = board_of(
        export(
            "acct9",
            r=row("acct9", registered=False),
        ),
        roster_export({"acct9": {"t0": thread_row()}}),
    )
    a = board.by_label("acct9")
    assert a.registered is False
    assert a.activity.executing == 1
    assert "NOT REGISTERED" in render._badges(a, NOW).plain


# ── THE HEADLINE: three states, three renderings ─────────────────────────────


def _card(account: model.Account) -> str:
    return "\n".join(
        line.plain
        for line in render.card_lines(
            account,
            width=100,
            selected=False,
            mode=render.FULL,
            warn_below=4.0,
            extras=None,
            flashing=None,
            now=NOW,
        )
    )


def test_the_three_unknowns_render_as_three_different_things() -> None:
    """**THE RULING, MECHANISED.**

    One fact — "this card has no usage meters and no login expiry" — for three
    different reasons, and the operator's remedy differs for each:

      KNOWN AND EMPTY  the daemon looked and there is nothing recorded.
      NOT HYDRATED     nothing has ever populated the cell. It may fill in at
                       any moment and there is nothing to do.
      DEAD             the fold is gone, every number is a corpse, and it will
                       not come back until the daemon restarts.

    Asserted PAIRWISE-DISTINCT rather than by looking for a `?`, because a
    client that collapsed two of the three would still show a `?`.
    """
    # NO LOGIN EXPIRY. `refresh_expires_at_ms` is the one the card counts down
    # to (see `tests/test_expiry.py`), so its absence is what makes this card
    # "known, and empty" rather than "we were not told".
    empty_row = row("acct0", gauge={}, credential=credential(refresh_expires_at_ms=None))

    known_empty = board_of(export("acct0", r=empty_row)).by_label("acct0")
    dry = model.project(
        mirror_with(
            export(
                "acct0",
                r=empty_row,
                c=cells(
                    gauge=not_hydrated("the gauge read failed"),
                    credential=not_hydrated("no slot read"),
                ),
            )
        ),
        now=NOW,
        non_idle=nonidle_with(
            roster_export(c={"threads": not_hydrated("the thread read failed")})
        ),
    ).by_label("acct0")
    corpse = board_of(
        export("acct0", r=empty_row),
        death("the shape task panicked"),
        roster_export(),
        roster_death("the shape task panicked"),
    ).by_label("acct0")

    a, b, c = _card(known_empty), _card(dry), _card(corpse)
    assert a != b, "known-and-empty must not render like not-hydrated"
    assert b != c, "not-hydrated must not render like dead"
    assert a != c, "dead must not render like known-and-empty"

    # And each says WHICH, in words, rather than being merely different.
    assert "no usage readings for this account" in a
    assert "login —" in a
    assert "? usage not hydrated" in b
    assert "the gauge read failed" in b
    assert "login ?" in b
    assert "? running" in b
    assert "✕ FOLD DIED" in c
    assert "LAST KNOWN" in c
    assert "login ✕ last known" in c

    # A DEAD CARD MUST NOT LOOK QUIET. The empty and not-hydrated cards carry
    # no death marks at all; the dead one carries them on every line that
    # matters.
    assert "✕ FOLD DIED" not in a and "✕ FOLD DIED" not in b


def test_a_clone_keeps_its_n_a_rather_than_becoming_the_empty_dash() -> None:
    """`—` replaces only the AMBIGUOUS `?`, never an answer that already exists.

    A clone's countdown belongs to someone else's login, which is why
    `fmt_days` says `n/a` and not `?`. Substituting the empty-dash there would
    turn a specific answer into a vaguer one, which is the opposite of the
    point of the three-valued state.
    """
    a = board_of(
        export(
            "acct0",
            r=row(
                "acct0",
                credential=credential(
                    slot_state="clone-of", clone_of="acct1", refresh_expires_at_ms=None
                ),
            ),
        )
    ).by_label("acct0")
    card = _card(a)
    assert "login n/a" in card
    assert "CLONE of acct1" in card


def test_the_unknown_mark_is_three_marks() -> None:
    """The one function where the three become three glyphs."""
    assert render.unknown_mark(shapes.CellState(kind=shapes.KNOWN)) is None
    assert render.unknown_mark(shapes.CellState(kind=shapes.NOT_HYDRATED)).plain == "?"
    assert render.unknown_mark(shapes.CellState(kind=shapes.DEAD, reason="x")).plain == "✕"
    # None is NOT a fourth state: it is the legacy path, and it renders as
    # whatever it always did.
    assert render.unknown_mark(None) is None


def test_a_dead_fold_is_announced_once_for_the_whole_board() -> None:
    board = board_of(export("acct0"), export("acct1"), death("the shape task panicked"))
    warnings = board.warnings()
    dead = [text for severity, text in warnings if "FOLD DIED" in text]
    assert len(dead) == 1
    assert "the shape task panicked" in dead[0]
    assert "NOT current" in dead[0]
    assert "restarts" in dead[0]


def test_a_running_count_is_never_drawn_as_current_on_a_dead_fold() -> None:
    board = board_of(
        export("acct0"),
        roster_export({"acct0": {"t0": thread_row()}}),
        roster_death(),
    )
    badges = render._badges(board.by_label("acct0"), NOW).plain
    assert "last known" in badges
    assert "▶ running 1" not in badges


def test_a_cell_less_account_takes_none_of_the_three_valued_branches() -> None:
    """**THE GATE THAT LET S2 SHIP BESIDE THE LEGACY PATH, KEPT AND REPURPOSED.**

    It used to read a `build_board` account: the five-query path produced
    `Account`s with no `cells` at all, `account.cell(name)` answered None, and
    every three-valued branch in `render.py` fell through to exactly what it
    drew before. That was the whole S2a safety argument, and `build_board` is
    deleted (S3), so the *source* of a cell-less account is gone.

    **THE PROPERTY IS NOT.** None is still not a fourth state, and `render.py`
    still gates on it — which matters because a bare `Account()` is what the
    render layer sees from any fixture, any future second shape whose
    projection does not populate cells, and any partial row. A `?` that means
    "this cell was never populated" and a `?` that means "nobody asked" must
    not be produced by the same branch, so the None path stays pinned.
    """
    bare = model.Account(number=1, label="acct0")
    assert bare.cells == {}
    assert bare.cell("credential") is None
    card = _card(bare)
    assert "no usage readings for this account" in card
    assert "login ?" in card  # fmt_days(None) — unchanged, not the shape's `?`
    assert "FOLD DIED" not in card
    assert "not hydrated" not in card


def test_moves_are_counted_and_qualified_by_covers_since() -> None:
    """`2 moved off since 14:02`, never `2 failovers`."""
    m = mirror_with(
        export(
            "acct0",
            r=row(
                "acct0",
                moves=[
                    {"thread_id": "t1", "direction": "off", "other": "acct1",
                     "reason": "failover", "at": T0},
                    {"thread_id": "t2", "direction": "on", "other": "acct1",
                     "reason": "dec10", "at": T0},
                ],
            ),
            c=cells(moves=known(since_init=True, covers_since=T0)),
        )
    )
    account = model.project(m, now=NOW).by_label("acct0")
    lines = "\n".join(line.plain for line in render._shape_lines(account))
    assert "1 moved off · 1 moved on" in lines
    assert "since" in lines


# ═══════════════════════════════════════════════════════════════════════════
# THE SOCKET HALF — a real server, the real BoardApp, no mocks of either.
# ═══════════════════════════════════════════════════════════════════════════

FAILURES: list[str] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    if ok:
        print(f"  ok   {name}")
    else:
        print(f"  FAIL {name}  {detail}")
        FAILURES.append(name)


def _daemon(keys=("acct0", "acct1")):
    """`test_place`'s `PoolDaemon`, narrowed to the keys this file wants.

    A second fake daemon would be a second wire contract and the two would
    drift. That one already answers `GET /health`, the shape surface, and —
    deliberately — all five of the queries S3 deleted from the board, which is
    what makes "the board did not ask" a provable claim rather than an
    arrangement of the fixture.
    """
    import test_place

    daemon = test_place.PoolDaemon()
    daemon.shape.keys = list(keys)
    daemon.shape.rows = {k: row(k) for k in keys}
    return daemon


async def _boot(daemon):
    import test_place

    return await test_place._boot(daemon)


async def _settle(pilot, seconds: float) -> None:
    end = asyncio.get_running_loop().time() + seconds
    while asyncio.get_running_loop().time() < end:
        await pilot.pause()
        await asyncio.sleep(0.05)


def _body(app) -> str:
    from textual.widgets import Static

    try:
        body = app.screen.query_one("#body", Static)
    except Exception:
        return ""
    return "\n".join(body.render_line(y).text for y in range(body.size.height))


# ── the fold IS the board ────────────────────────────────────────────────────


async def scenario_the_board_is_the_fold() -> None:
    """One keyless subscribe enumerates; a pushed row moves the screen.

    **`pilot.press` IS NEVER CALLED.** The claim is that the card's number
    moves because the daemon folded something, with no keystroke and no query.

    The S2a scenario that used to stand beside this one — *"an old daemon is
    untouched"* — is DELETED, because its claim is no longer true and pretending
    otherwise would be the worst kind of green. An old daemon is not untouched
    after S3: it gets no board at all. That state is proved in
    `test_place.py::scenario_a_shapes_less_daemon_says_so_out_loud`, where it
    belongs with the rest of the one-source consequences.
    """
    print("\nthe keyless subscribe enumerates, and a row moves the screen")

    daemon = _daemon()
    app = await _boot(daemon)
    async with app.run_test(size=(140, 45)) as pilot:
        await _settle(pilot, 3.0)

        check("exactly one subscribe was sent, and it carried NO key",
              len(fake_daemon.subscribes_for(daemon.shape)) == 1
              and "key" not in fake_daemon.subscribes_for(daemon.shape)[0],
              f"{fake_daemon.subscribes_for(daemon.shape)}")

        # ── AND IT SAID WHO IT WAS. Rule 2.1, compulsory since the operator's
        #    ruling of 2026-10-08. Asserted HERE, positively and by name,
        #    because the fake daemon's refusal alone reddens this suite only
        #    through downstream UI assertions — four tests failed with
        #    `'BoardScreen' object has no attribute 'account'`, which is a red
        #    that does not name its own cause. This check does.
        # Indexed defensively: when the identity is MISSING the fake daemon
        # refuses the subscribe, so it is never recorded and this list is empty.
        # A bare `[0]` raised IndexError there and took the whole scenario phase
        # down with it — every scenario after this one stopped running, which is
        # a worse failure than the one it was reporting.
        sent = fake_daemon.subscribes_for(daemon.shape)
        sub = sent[0] if sent else {}
        consumer = sub.get("consumer")
        check("the subscribe STATED AN IDENTITY — a keyless subscribe is no "
              "longer an empty params object",
              isinstance(consumer, dict),
              f"consumer={consumer!r} in {sub}")
        check("and it named the PROGRAM and what it was built against, both "
              "non-empty, which is what the producer refuses without",
              isinstance(consumer, dict)
              and isinstance(consumer.get("name"), str)
              and consumer["name"].strip() != ""
              and isinstance(consumer.get("shape_version"), str)
              and consumer["shape_version"].strip() != "",
              f"consumer={consumer!r}")
        check("both keys were enumerated by that one request",
              app.shapes.keys() == ["acct0", "acct1"],
              f"{app.shapes.keys()}")
        check("the board has two accounts, counted from held keys",
              [a.label for a in app.board.accounts] == ["acct0", "acct1"],
              f"{[a.label for a in app.board.accounts]}")
        check("warn_below_days came off the shared block, not a hardcode",
              app.board.warn_below_days == 4.0,
              str(app.board.warn_below_days))

        # ── THE CLAIM: a row lands and the card moves ──
        daemon.shape.placed["acct0"] = {"t0": thread_row()}
        await daemon.push(roster_live(daemon.shape.placed))
        await _settle(pilot, 1.5)

        check("THE CARD SHOWS A RUNNING THREAD WITH NO KEYSTROKE",
              "▶ running 1" in _body(app), _body(app)[:400])

        # ── AND NOTHING ELSE WAS ASKED ────────────────────────────────────
        #
        # This daemon answers all five deleted queries and `threads/{thread_id}/get`. The
        # board asking none of them is the whole of S3, and it is asserted
        # against a daemon that would have answered.
        check("none of the five deleted board queries was asked",
              fake_daemon.queries_asked(daemon.methods) == [],
              f"asked: {fake_daemon.queries_asked(daemon.methods)}")
        check("and no threads/{thread_id}/get was issued — the placement is the key",
              daemon.thread_get_calls == [],
              f"{daemon.thread_get_calls}")

        # ── a gone removes the card ──
        await daemon.push(live("acct1", {"kind": "gone"}))
        await _settle(pilot, 1.0)
        check("a `gone` removes the account from the board",
              [a.label for a in app.board.accounts] == ["acct0"],
              f"{[a.label for a in app.board.accounts]}")

    await daemon.stop()


async def scenario_a_new_key_is_re_enumerated() -> None:
    """A key that appears later needs a new export, or its rows are DROPPED.

    `drive` gates a live `row` on this connection already holding that key, so
    a brand-new key's rows go nowhere until it is subscribed. The trigger is
    the same `info` announcement — a line on the `narration` shape now, a
    firehose frame before R2.S7 — that used to schedule a five-query reload — and the
    point of this scenario after S3 is that it schedules a re-enumeration
    INSTEAD, not as well.
    """
    print("\nan announcement re-enumerates the keys")

    daemon = _daemon()
    app = await _boot(daemon)
    async with app.run_test(size=(140, 45)) as pilot:
        await _settle(pilot, 3.0)
        check("two keys to start with", app.shapes.keys() == ["acct0", "acct1"])

        daemon.shape.keys.append("acct2")
        daemon.shape.rows["acct2"] = row("acct2")
        await daemon.narrate("info", domain="accounts",
                             reason="pool_reloaded", detail="acct2 added")
        await _settle(pilot, 2.0)

        check("THE NEW KEY IS ON THE BOARD, WITH NO KEYSTROKE",
              app.shapes.keys() == ["acct0", "acct1", "acct2"],
              f"{app.shapes.keys()}")
        check("it was picked up by a re-subscribe, not by a board reload",
              len(fake_daemon.subscribes_for(daemon.shape)) == 2
              and "key" not in fake_daemon.subscribes_for(daemon.shape)[1],
              f"{fake_daemon.subscribes_for(daemon.shape)}")
        check("and the coalesce window made it ONE re-subscribe",
              len(fake_daemon.subscribes_for(daemon.shape)) == 2,
              f"{len(fake_daemon.subscribes_for(daemon.shape))} subscribes")
        check("the announcement read no account query — there is none to read",
              fake_daemon.queries_asked(daemon.methods) == [],
              f"asked: {fake_daemon.queries_asked(daemon.methods)}")

    await daemon.stop()


async def scenario_a_dead_fold_is_loud_and_terminal() -> None:
    """The mirror flips to dead, says so, and CANNOT quietly heal itself.

    At S2b this scenario ended by pressing `r` and falling back to the five
    queries — an explicit escape hatch, chosen because a frozen screen with no
    door is its own dishonesty. S3 deletes the room behind that door, and the
    result is stronger rather than weaker: there is nothing left that could
    repaint a dead fold into looking healthy. `r` now asks the daemon to
    re-subscribe and the daemon refuses in its own words. That half is proved
    in `test_place.py::scenario_a_dead_fold_has_no_fallback_to_hide_behind`;
    what is proved HERE is the rendering, which is where the ruling lives.
    """
    print("\na dead fold, which must not look like a quiet one")

    daemon = _daemon()
    app = await _boot(daemon)
    async with app.run_test(size=(140, 45)) as pilot:
        await _settle(pilot, 3.0)
        check("the board is drawn from the fold", app.shapes.exported)

        daemon.shape.dead = "the shape task panicked"
        await daemon.push(death("the shape task panicked"))
        await _settle(pilot, 1.5)

        body = _body(app)
        check("the board says the fold died", "FOLD DIED" in body, body[:400])
        check("and says it is not current", "NOT current" in body)
        check("and names the reason the daemon gave",
              "the shape task panicked" in body)
        # THE CARDS SURVIVE, MARKED. A board that empties itself on a dead fold
        # looks exactly like a board whose accounts all went away, and the
        # operator must be able to see WHAT went stale.
        check("the cards are still drawn, marked dead rather than removed",
              "acct0" in body and "acct1" in body and body.count("FOLD DIED") >= 2,
              f"FOLD DIED x{body.count('FOLD DIED')}")
        check("it did NOT silently fall back — there is nothing to fall back to",
              fake_daemon.queries_asked(daemon.methods) == [],
              "a five-query reload repainted the screen and the death vanished")
        # AND IT IS NOT THE SAME SCREEN AS A DAEMON WITH NO FOLD AT ALL. One is
        # terminal until the daemon restarts; the other is a version mismatch
        # that fixes itself on promotion. Drawing them alike would flatten the
        # two remedies into one shrug.
        check("a dead fold and an absent one do not read the same",
              "NO FOLD ON THIS DAEMON" not in body,
              body[:400])

    await daemon.stop()


SCENARIOS = [
    scenario_the_board_is_the_fold,
    scenario_a_new_key_is_re_enumerated,
    scenario_a_dead_fold_is_loud_and_terminal,
]


def main() -> int:
    print(__doc__.strip().splitlines()[0])

    print("\nTHE PURE HALF — no daemon, no socket, no sleeping\n")
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            try:
                fn()
                print(f"  ok   {name}")
            except AssertionError as exc:
                print(f"  FAIL {name}  {exc}")
                FAILURES.append(name)

    asyncio.run(_run_all())
    print()
    if FAILURES:
        print(f"{len(FAILURES)} FAILED: {', '.join(FAILURES)}")
        return 1
    print("all shape checks passed")
    return 0


async def _run_all() -> None:
    for scenario in SCENARIOS:
        await scenario()


if __name__ == "__main__":
    sys.exit(main())
