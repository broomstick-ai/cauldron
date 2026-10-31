"""**THE BOARD READS NARRATION AS A SHAPE, AND SENDS NO FIREHOSE SUBSCRIBE.**

R2.S7. `info`, `progress` and `lifecycle` carry an EMPTY thread id on the legacy
wire, so no per-thread filter reaches them: a face that reads any of the three
off the firehose cannot retire the firehose (`docs/consumer-seam.md` §6, M6).
The `narration` shape carries all three (`lib/threads/src/shapes/narration.rs`),
and this board was the last face reading them off the firehose BY DESIGN. It
now subscribes `shapes/narration/subscribe` keyless, folds the `lines` cell into
a third mirror, and draws the `⚡` line off what that fold appended.

THREE GUARDS, each driven through the real `BoardApp` against
`tests/fake_daemon.py` on an ephemeral port — never the live daemon:

  1. a `lifecycle` LINE appended on the narration fold renders the `⚡` line —
     the POSITIVE sentinel the other two lean on;
  2. a `lifecycle` FRAME pushed raw onto the socket, with no thread id, is NOT
     rendered and NOT folded. The `_on_push` arm that drew it is DELETED, so
     absence is observed by a predicate that was TRUE before the deletion — the
     count of rendered `⚡` lines carrying its detail, which the old board drew
     as 1 — and the same predicate is then shown to be producible in this very
     render by appending the same fact as a line;
  3. the board sends NO frame whose `type` is `subscribe`, over the whole
     handshake and settle. Every frame the fake receives is recorded
     (`FakeDaemon.received`), and the list is asserted NON-EMPTY with the
     `shapes/*/subscribe` requests in it, so the absence cannot be an empty
     recording.

Run:  .venv/bin/python -m pytest -q tests/test_narration.py   (from threads-app/)
"""

from __future__ import annotations

import asyncio
import json
import pathlib
import sys

sys.path.insert(0, __file__.rsplit("/tests/", 1)[0])
sys.path.insert(0, __file__.rsplit("/test_narration.py", 1)[0])

import fake_daemon  # noqa: E402

from threads_tui import endpoint, shapes  # noqa: E402

#: A detail string no other part of the board could draw, so a count of the
#: rendered lines carrying it counts THIS narration and nothing else.
SAID = "threads came back on binary r2s7-sentinel"


def _narration_lines(body: str, needle: str) -> list[str]:
    """The rendered `⚡` lines carrying `needle`. THE predicate of guards 1 and 2."""
    return [ln for ln in body.splitlines() if "⚡" in ln and needle in ln]


async def _drive(scenario):
    daemon = fake_daemon.FakeDaemon()
    app = await fake_daemon.boot(daemon)
    try:
        async with app.run_test(size=(140, 45)) as pilot:
            await fake_daemon.settle(pilot, 3.0)
            return await scenario(daemon, app, pilot)
    finally:
        await daemon.stop()


# ── guard 1: the positive sentinel ───────────────────────────────────────────


def test_a_lifecycle_line_on_the_fold_draws_the_lightning_line() -> None:
    """FORCED RED by deleting `self.feed.on_line(...)` from `app._on_narration`
    (the fold still applies the line; nothing narrates it), and by reverting the
    five production files to `add0683` (no narration subscribe at all)."""

    async def scenario(daemon, app, pilot):
        held_before = len(app.narration.row(shapes.LIFECYCLE).get(shapes.LINES) or [])
        drawn_before = _narration_lines(fake_daemon.body(app), SAID)
        await daemon.narrate("lifecycle", detail=SAID, boot_id=fake_daemon.BOOT_ID)
        await fake_daemon.settle(pilot, 1.0)
        held_after = len(app.narration.row(shapes.LIFECYCLE).get(shapes.LINES) or [])
        return held_before, drawn_before, held_after, _narration_lines(fake_daemon.body(app), SAID)

    held_before, drawn_before, held_after, drawn = asyncio.run(_drive(scenario))
    assert held_before >= 1, "the lifecycle export (the boot note) never reached the mirror"
    assert drawn_before == [], f"drawn before anything was said: {drawn_before}"
    assert held_after == held_before + 1, f"the fold held {held_before} -> {held_after} lines"
    assert len(drawn) == 1, f"expected ONE ⚡ line saying {SAID!r}, got {drawn}"


# ── guard 2: the deleted arm, observed by a predicate that used to be true ───


def test_a_raw_lifecycle_frame_is_neither_drawn_nor_folded() -> None:
    """FORCED RED by restoring a firehose arm (`Api._dispatch` handing a
    non-shape frame to the feed as a line), and by reverting the five production
    files to `add0683`, where `_on_push` drew this frame as one `⚡` line."""

    async def scenario(daemon, app, pilot):
        held_before = len(app.narration.row(shapes.LIFECYCLE).get(shapes.LINES) or [])
        note_before = app.feed.note(at=0.0)
        # The legacy wire's `ServerMsg::Lifecycle`: tagged on `type`, no thread
        # id. Forced onto the socket raw — the fake serves no firehose, so this
        # is the only way the frame can arrive at all.
        await daemon.push_raw({
            "type": "lifecycle", "service": "threads", "phase": "back_up",
            "reason": "new_binary", "detail": SAID,
            "boot_id": fake_daemon.BOOT_ID, "at": fake_daemon.T0,
        })
        await fake_daemon.settle(pilot, 1.0)
        drawn_raw = _narration_lines(fake_daemon.body(app), SAID)
        held_raw = len(app.narration.row(shapes.LIFECYCLE).get(shapes.LINES) or [])
        pending_raw = app.feed.pending()
        # THE SAME PREDICATE, MADE TRUE IN THIS RENDER: the same fact as a
        # line on the fold. If this stays 0, the absence above proved nothing.
        await daemon.narrate("lifecycle", detail=SAID, boot_id=fake_daemon.BOOT_ID)
        await fake_daemon.settle(pilot, 1.0)
        drawn_line = _narration_lines(fake_daemon.body(app), SAID)
        return held_before, note_before, drawn_raw, held_raw, pending_raw, drawn_line

    held_before, note_before, drawn_raw, held_raw, pending_raw, drawn_line = asyncio.run(
        _drive(scenario)
    )
    assert drawn_raw == [], f"a raw firehose frame was DRAWN: {drawn_raw}"
    assert held_raw == held_before, f"a raw firehose frame was FOLDED: {held_before} -> {held_raw}"
    assert not pending_raw, "a raw firehose frame scheduled a re-enumeration"
    assert len(drawn_line) == 1, f"sentinel: the same fact as a LINE must draw once, got {drawn_line}"


# ── guard 3: no firehose subscribe, over a recording that is not empty ───────


def test_the_board_sends_no_firehose_subscribe() -> None:
    """FORCED RED by restoring `await ws.send(json.dumps({"type": "subscribe",
    "threads": None}))` in `Api._handshake`, and by reverting to `add0683`."""

    async def scenario(daemon, app, pilot):
        return list(daemon.received), app.feed.subscribed

    received, subscribed = asyncio.run(_drive(scenario))
    kinds = [f.get("type") for f in received]
    asked = [f.get("method") for f in received if f.get("type") == "request"]
    # THE SENTINELS: the recording is real and holds the shape subscribes.
    assert received, "the fake recorded no frames at all — the assertion below would be vacuous"
    assert "shapes/narration/subscribe" in asked, f"no narration subscribe in {asked}"
    assert "shapes/accounts/subscribe" in asked, f"no accounts subscribe in {asked}"
    narration_subs = [
        f for f in received if f.get("method") == "shapes/narration/subscribe"
    ]
    assert len(narration_subs) == 1, f"narration is subscribed ONCE per connection: {narration_subs}"
    assert "key" not in (narration_subs[0].get("params") or {}), "narration is subscribed keyless"
    # THE GUARD.
    assert "subscribe" not in kinds, f"a firehose subscribe was sent: {kinds}"
    # And the header's claim now rests on the narration export, not a `sub_ack`.
    assert subscribed, "the narration export never set `feed.subscribed`"


# ── the endpoint: `:3653`, the lever kept ────────────────────────────────────


def test_the_default_endpoint_is_the_chassis_door(tmp_path, monkeypatch) -> None:
    """`:3653` by default; the legacy `port` key is NOT read; `chassis_port`,
    `THREADS_CHASSIS_PORT` and `THREADS_TUI_WS_PORT` still move it."""
    for var in ("THREADS_TUI_WS_PORT", "THREADS_CHASSIS_PORT", "THREADS_SECRET"):
        monkeypatch.delenv(var, raising=False)
    cfg = tmp_path / "config.toml"
    monkeypatch.setenv("THREADS_CONFIG", str(cfg))

    assert endpoint.DEFAULT_PORT == 3653
    assert endpoint.daemon_endpoint()[1] == 3653, "no config: the chassis door"
    cfg.write_text("port = 3651\n")
    assert endpoint.daemon_endpoint()[1] == 3653, "the LEGACY port key must not move the board"
    cfg.write_text("port = 3651\nchassis_port = 4653\n")
    assert endpoint.daemon_endpoint()[1] == 4653, "chassis_port is the file's lever"
    monkeypatch.setenv("THREADS_CHASSIS_PORT", "5653")
    assert endpoint.daemon_endpoint()[1] == 5653, "THREADS_CHASSIS_PORT beats the file"
    monkeypatch.setenv("THREADS_TUI_WS_PORT", "6653")
    assert endpoint.daemon_endpoint()[1] == 6653, "THREADS_TUI_WS_PORT is the last word"
    assert endpoint.ws_url() == "ws://127.0.0.1:6653/ws"


# ── the fixture's lines are the producer's lines ─────────────────────────────


def test_the_fixture_speaks_the_inventory_s_narration() -> None:
    """The fake's narration lines use the field names `inventory.json` records
    for shape `narration`, cell `lines` — or every guard above is evidence about
    a convenient wire rather than the real one."""
    inv = pathlib.Path(__file__).resolve().parents[5] / "libs" / "shapewire" / "seam" / "inventory.json"
    shape = next(s for s in json.loads(inv.read_text())["shapes"] if s["name"] == "narration")
    cell = next(c for c in shape["cells"] if c["name"] == shapes.LINES)
    fields = {f["name"] for f in cell["fields"]}
    kinds = set(next(f for f in cell["fields"] if f["name"] == "kind")["words"])
    assert shape["name"] == shapes.NARRATION
    assert kinds == {"progress", "lifecycle", "info"}, kinds
    assert shapes.LIFECYCLE in kinds
    for kind in sorted(kinds):
        line = fake_daemon.narration_line(kind)
        assert set(line) <= fields, f"{kind}: fields not on the wire: {set(line) - fields}"
    assert "boot_id" in fake_daemon.narration_line("lifecycle")


# ── the two gaps the inspector found: the startup export, and the boot-id arm ─


def test_the_startup_export_does_not_replay_old_lines_as_new() -> None:
    """**THE INITIAL `lifecycle` EXPORT CARRIES THIS BOOT'S OWN NOTE, hours old
    on a real daemon, and the board must NOT draw it as news.** Only a line the
    fold APPENDS after the export draws the `⚡` line. The inspector forced the
    export to draw and the whole suite stayed green: nothing guarded it.

    FORCED RED by making `_on_narration` hand the export's newest line to
    `feed.on_line` (the boot note draws), and — the opposing constant — by
    making it hand NOTHING to the feed (the appended sentinel does not draw).
    """

    async def scenario(daemon, app, pilot):
        held = len(app.narration.row(shapes.LIFECYCLE).get(shapes.LINES) or [])
        replayed = _narration_lines(fake_daemon.body(app), "threads came back")
        await daemon.narrate("lifecycle", detail=SAID, boot_id=fake_daemon.BOOT_ID)
        await fake_daemon.settle(pilot, 1.0)
        return held, replayed, _narration_lines(fake_daemon.body(app), SAID)

    held, replayed, drawn = asyncio.run(_drive(scenario))
    assert held >= 1, "the export reached the mirror (the boot note is held) — sentinel"
    assert replayed == [], f"the startup export was REPLAYED as a new line: {replayed}"
    assert len(drawn) == 1, f"sentinel: a line appended AFTER the export draws once, got {drawn}"


def test_a_changed_boot_id_resets_the_mirror_and_an_unchanged_one_does_not() -> None:
    """**A DIFFERENT DAEMON IS A DIFFERENT FOLD; THE SAME ONE IS NOT.** Both
    directions, collected by pytest — the only guard before this was a
    `scenario_*` in `test_place.py` that pytest never runs.

    The witness is `app.shapes.reset` itself, counted: a lifecycle line carrying
    the boot id this socket's welcome already named must NOT reset (that
    daemon is the one every mirror was exported from), and a line naming a
    process the socket never said it was MUST reset and re-arm the keyless
    subscribe.

    FORCED RED by replacing `_track_boot`'s condition with `True` (the unchanged
    case resets) and with `False` (the changed case does not).
    """

    async def scenario(daemon, app, pilot):
        resets = []
        real_reset = app.shapes.reset

        def counting_reset(*a, **kw):
            resets.append(1)
            return real_reset(*a, **kw)

        app.shapes.reset = counting_reset  # instance attribute; the class is untouched
        exports_before = app.shapes.exports
        await daemon.narrate("lifecycle", detail="same daemon", boot_id=fake_daemon.BOOT_ID)
        await fake_daemon.settle(pilot, 1.5)
        same = (len(resets), app.shapes.exports)
        subs_before = sum(1 for f in daemon.received if f.get("method") == "shapes/accounts/subscribe")
        await daemon.narrate("lifecycle", detail="another daemon", boot_id="boot-DIFFERENT")
        await fake_daemon.settle(pilot, 2.0)
        subs_after = sum(1 for f in daemon.received if f.get("method") == "shapes/accounts/subscribe")
        return exports_before, same, len(resets), subs_before, subs_after

    exports_before, (resets_same, exports_same), resets_total, subs_before, subs_after = asyncio.run(
        _drive(scenario)
    )
    assert resets_same == 0, "the boot id the welcome already named is NOT a restart"
    assert exports_same >= exports_before, f"exports went backwards without a reset: {exports_before} -> {exports_same}"
    assert resets_total == 1, f"a DIFFERENT boot id resets the mirror exactly once, got {resets_total}"
    assert subs_after > subs_before, (
        f"sentinel: the reset re-arms the keyless subscribe ({subs_before} -> {subs_after})"
    )
