"""**A POOL-SHARE OVERRIDE CAN BE CLEARED, NOT ONLY SET (R2.S3g).**

> *"The end state should clear aa-owned from my pool because its not real."*

The daemon's `accounts/{account}/pool_share_set` now reads `share: null`
(present) as DELETE the override row, and still refuses an ABSENT `share` as a
malformed call. This file holds the board's half: `Api.pool_share_set(account,
None)` sends a PRESENT null, never an omitted key, and the fake daemon answers
the method the way the daemon does, so no test here can pass against a fake
that is more lenient than the wire.

The fake binds `127.0.0.1` on a port the kernel hands `free_port`; nothing here
dials a fixed port.
"""

from __future__ import annotations

import asyncio
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(ROOT / "tests"))

import fake_daemon  # noqa: E402

from threads_tui import caps  # noqa: E402
from threads_tui.api import Api  # noqa: E402


def test_none_is_sent_as_a_present_null_never_an_absent_share():
    """Forced red by dropping the key when `share is None` (`params = {}` for
    None): the captured params are `{}` and this fails `assert {} == {'share':
    None}` — which the daemon would refuse as `missing required param`."""
    sent: list[tuple[str, dict]] = []

    async def capture(method, params, **_kw):
        sent.append((method, params))
        return {"promise_id": 1, "visible": True, "changed": True}

    api = Api()
    api.request = capture  # type: ignore[method-assign]
    asyncio.run(api.pool_share_set("aa-owned", None))
    method, params = sent[0]
    assert method == caps.fill(caps.ACCOUNTS_POOL_SHARE_SET, account="aa-owned")
    assert params == {"share": None}
    assert json.dumps(params) == '{"share": null}'


async def _ask(url: str, method: str, params: dict) -> dict:
    from websockets.asyncio.client import connect

    async with connect(url) as ws:
        await ws.recv()  # welcome
        await ws.send(json.dumps({"type": "request", "id": "1", "method": method, "params": params}))
        while True:
            frame = json.loads(await ws.recv())
            if frame.get("type") == "response":
                return frame["outcome"]


def test_the_fake_clears_on_null_and_refuses_an_absent_share():
    """Forced red by making the fake read an absent `share` as null
    (`p.get("share")` with no presence check): the absent call SUCCEEDS and
    this fails on `assert 'success' == 'error'`."""

    async def run() -> None:
        daemon = fake_daemon.FakeDaemon()
        port = fake_daemon.free_port()
        await daemon.start(port)
        url = f"ws://127.0.0.1:{port}/ws"
        method = "accounts/aa-owned/pool_share_set"
        try:
            out = await _ask(url, method, {"share": 0.5})
            assert out["type"] == "success" and out["payload"]["changed"] is True, out
            assert daemon.share_overrides == {"aa-owned": 0.5}

            out = await _ask(url, method, {"share": None})
            assert out["type"] == "success", out
            assert out["payload"]["changed"] is True, out
            assert daemon.share_overrides == {}

            out = await _ask(url, method, {"share": None})
            assert out["type"] == "success", "clearing nothing is not an error"
            assert out["payload"]["changed"] is False, out

            await _ask(url, method, {"share": 0.5})
            out = await _ask(url, method, {"note": "forgot the number"})
            assert out["type"] == "error", out
            assert "missing required param: share" in out["error"], out
            assert daemon.share_overrides == {"aa-owned": 0.5}, "absent must not clear"
        finally:
            await daemon.stop()

    asyncio.run(run())
