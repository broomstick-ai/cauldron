"""**THE 7-DAY SPARKLINE IS DRAWN OFF THE `accounts/gauge-history` FOLD (R2.S3d).**

It was the board's ONE remaining read: `gauge/history`, sent for the selected
card and re-sent whenever a `gauge` announcement cleared the cache. The series
is a shape now — `accounts/gauge-history`, keyed per account, subscribed once
per card (`app._arm_history`) — and the card draws what the fold holds
(`app.history_extras`). Driven through the real `BoardApp` against
`tests/fake_daemon.py` on an ephemeral port, never the live daemon.

THE PREDICATE is the sparkline's PIXEL COUNT: the number of bar glyphs
(`render.SPARK_CHARS`) on the selected card's `weekly 7d` line. It is the
positive sentinel the guards below lean on — it must MOVE when the fold moves.

Run:  .venv/bin/python -m pytest -q tests/test_gauge_history.py   (from threads-app/)
"""

from __future__ import annotations

import asyncio
import sys
from datetime import datetime, timedelta, timezone

sys.path.insert(0, __file__.rsplit("/tests/", 1)[0])
sys.path.insert(0, __file__.rsplit("/test_gauge_history.py", 1)[0])

import fake_daemon  # noqa: E402

from threads_tui import api, caps, render, shapes  # noqa: E402

NOW = datetime.now(timezone.utc)


def _ago(delta: timedelta) -> str:
    return (NOW - delta).isoformat()


def _pixels(body: str) -> int:
    """The bar glyphs on the `weekly 7d` line — THE predicate."""
    for line in body.splitlines():
        if "weekly 7d" in line:
            return sum(1 for ch in line if ch in render.SPARK_CHARS)
    return -1


async def _drive(scenario, history: dict[str, list[dict]] | None = None):
    daemon = fake_daemon.FakeDaemon()
    if history is not None:
        daemon.shape.history = history
    app = await fake_daemon.boot(daemon)
    try:
        async with app.run_test(size=(140, 45)) as pilot:
            await fake_daemon.settle(pilot, 3.0)
            return await scenario(daemon, app, pilot)
    finally:
        await daemon.stop()


WEEK = {
    "acct0": [
        fake_daemon.history_reading(_ago(timedelta(days=6)), 10.0),
        fake_daemon.history_reading(_ago(timedelta(days=5)), 30.0),
        fake_daemon.history_reading(_ago(timedelta(days=4)), 50.0),
    ]
}


def test_a_reading_appended_on_the_fold_draws() -> None:
    """**THE POSITIVE SENTINEL.** Three readings exported, so three pixels; one
    reading appended as a `transition` an hour ago, in a slice that held none,
    so four — with no request between the two paints.

    FORCED RED (R2.S3d) by deleting the `accounts/gauge-history` arm from
    `app._on_shape`: the export never reaches the mirror, the card says
    "reading history…", and the first paint draws 0 pixels where the fold
    holds 3 (recorded: `the exported week draws three pixels, drew 0`). And,
    narrower, by dropping only the `op: live` frames of that shape: the export
    draws and the appended reading does not (recorded: `a reading appended on
    the fold must draw: 3 -> 3`)."""

    async def scenario(daemon, app, pilot):
        before = _pixels(fake_daemon.body(app))
        asked = list(daemon.methods)
        await daemon.push(fake_daemon.history_live(
            "acct0",
            fake_daemon.history_reading(_ago(timedelta(hours=1)), 70.0),
            at=_ago(timedelta(hours=1)),
        ))
        await fake_daemon.settle(pilot, 1.0)
        return before, _pixels(fake_daemon.body(app)), daemon.methods[len(asked):]

    before, after, sent = asyncio.run(_drive(scenario, WEEK))
    assert before == 3, f"the exported week draws three pixels, drew {before}"
    assert after == before + 1, f"a reading appended on the fold must draw: {before} -> {after}"
    assert sent == [], f"and nothing was ASKED to draw it: {sent}"


def test_the_week_is_subscribed_keyed_and_gauge_history_is_never_asked() -> None:
    """**THE REQUEST PATH IS GONE AND THE SPARKLINE STILL DRAWS.** The fake
    still answers `gauge/history` (a fake that refused would only prove a
    missing method cannot be called); the board must not send it, must
    subscribe the shape keyed per account, and must still draw the week.

    FORCED RED (R2.S3d) by `history_extras` returning `CardExtras(loaded=True)`
    without reading the fold: zero pixels, "no observations in the last 7
    days" — the request is gone AND nothing replaced it."""

    async def scenario(daemon, app, pilot):
        keyed = [
            s.get("key")
            for s in fake_daemon.subscribes_for(daemon.shape, shapes.GAUGE_HISTORY)
        ]
        return keyed, list(daemon.methods), _pixels(fake_daemon.body(app))

    keyed, methods, pixels = asyncio.run(_drive(scenario, WEEK))
    assert keyed == ["acct0"], f"one KEYED subscribe per account on screen: {keyed}"
    assert "gauge/history" not in methods, f"the board asked gauge/history: {methods}"
    assert any(m.startswith("shapes/") for m in methods), f"sentinel: {methods}"
    assert pixels == 3, f"the week is drawn off the fold: {pixels}"


def test_the_ability_to_ask_is_gone() -> None:
    """The constant, the probe, and the wrapper — the three things that made
    `gauge/history` askable — are deleted, not left dormant."""
    assert not hasattr(caps, "GAUGE_HISTORY")
    assert "gauge/history" not in caps.PROBE_ORDER
    assert "gauge/history" not in caps.CAPABILITIES
    assert not hasattr(api.Api, "gauge_history")


def _weekly_line(body: str) -> str:
    return next((ln for ln in body.splitlines() if "weekly 7d" in ln), "")


def test_a_refused_series_is_unknown_not_an_empty_week() -> None:
    """**THE PRODUCER'S ROW-LESS EXPORT IS `?`, NEVER "no observations".**

    `acct0` is outside the fake's history map, so the fake sends what the real
    hub sends for a key `hydrates_key` refused: keyed, no row, no cells. The
    selected card must say `?` / `no history` — an unknown week — and not the
    known-empty sentence. SENTINEL, same run: `acct1` IS in the map and the fake
    sends it a FULL frame, which the mirror holds as a known series.

    FORCED RED (R2.S3d inspect) by restoring `history_extras`' old branch
    (`return render.CardExtras(loaded=True)` for a not-known cell): the line
    reads "no observations in the last 7 days"."""

    async def scenario(daemon, app, pilot):
        return (
            _weekly_line(fake_daemon.body(app)),
            ("acct0" in app.gauge_history.keys(), app.gauge_history.row("acct0")),
            app.gauge_history.cell("acct1", shapes.READINGS).known,
            len(app.gauge_history.row("acct1").get(shapes.READINGS) or []),
        )

    async def drive():
        daemon = fake_daemon.FakeDaemon()
        daemon.shape.keys = ["acct0", "acct1"]
        daemon.shape.rows["acct1"] = fake_daemon.row("acct1")
        daemon.shape.history = {"acct1": WEEK["acct0"]}
        app = await fake_daemon.boot(daemon)
        try:
            async with app.run_test(size=(140, 60)) as pilot:
                await fake_daemon.settle(pilot, 3.0)
                return await scenario(daemon, app, pilot)
        finally:
            await daemon.stop()

    line, refused_row, acct1_known, acct1_len = asyncio.run(drive())
    assert acct1_known and acct1_len == 3, "sentinel: the pool account got a FULL frame"
    assert refused_row == (True, {}), f"the refused key is HELD, row-less: {refused_row}"
    assert "no observations" not in line, f"an unknown week drawn as empty: {line!r}"
    assert "?" in line and "no history" in line, f"the unknown mark is missing: {line!r}"


def test_a_known_empty_week_says_no_observations() -> None:
    """OPPOSED to the test above: a frame whose `readings` IS `[]`, `known`, is
    a statement — the card says "no observations in the last 7 days", not `?`.

    FORCED RED (R2.S3d inspect) by `history_extras` setting `history_state` on
    every key: the known-empty week is drawn `?`."""

    async def scenario(daemon, app, pilot):
        return _weekly_line(fake_daemon.body(app))

    line = asyncio.run(_drive(scenario, {"acct0": []}))
    assert "no observations in the last 7 days" in line, f"{line!r}"
    assert "?" not in line, f"a known empty week drawn unknown: {line!r}"


def test_no_export_yet_is_reading_history() -> None:
    """The first state: no export held for the key (a subscribe in flight) is
    `loaded=False` — the card's "reading history…". Driven on the real app; the
    held export is removed to stand for the instant before it arrives."""

    async def scenario(daemon, app, pilot):
        before = app.history_extras(app.selected_account(), NOW)
        app.gauge_history.held.pop("acct0", None)
        after = app.history_extras(app.selected_account(), NOW)
        return before.loaded, after.loaded, after.history_state

    held_loaded, empty_loaded, state = asyncio.run(_drive(scenario, WEEK))
    assert held_loaded is True, "sentinel: with the export held the series is loaded"
    assert empty_loaded is False and state is None, "no export is in flight, not unknown"
