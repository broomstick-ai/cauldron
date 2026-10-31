"""The API. Every fact this app shows comes through here.

Adapted from the old app's `daemon.py`, which was already careful about the
things that are hard to get right — request correlation, backoff, and a
shutdown that does not print asyncio noise onto the operator's bare terminal
after the alt-screen is gone. Those parts are kept close to verbatim.

What is NEW here:

  * a CAPABILITY REGISTRY (`threads_tui.caps`) folded into the request path, so
    a method that is not on the wire yet degrades one panel instead of
    crashing the board or, worse, tempting a local reimplementation;

  * BOOT-ID TRACKING. The welcome frame carries the daemon's boot id. A changed
    boot id means a different daemon process, which means the capability map we
    cached belongs to a daemon that no longer exists — so it is discarded and
    re-probed. A capability map that outlives its daemon is a lie that greys
    out working buttons;

  * a LIVENESS check over `GET /health`, spoken as raw async HTTP so that
    asking "is the daemon there?" never blocks the event loop.

WHAT IS DELIBERATELY ABSENT: there is no SQLite import, no subprocess, and no
filesystem read in this module. If a fact is not obtainable here, the surface
says so. See `threads_tui.drag`, obligation 2.
"""

from __future__ import annotations

import asyncio
import json
import os
import uuid
from dataclasses import dataclass, field
from typing import Any, Callable

from . import __version__, caps, endpoint

# Imported, NOT redefined. An earlier draft declared a second `LinkState` here
# as well as in link.py; the two compared unequal under `is`, so a connection
# that was live tested as not-live in anything that imported the other copy.
# The smoke test caught it. One definition, one module.
from .link import LinkState

#: **WHO THIS BOARD SAYS IT IS** — rule 2.1's consumer identity, in ONE place.
#:
#: COMPULSORY since the operator's ruling of 2026-10-08: *"what I want is
#: consumers to be forced to say who they are so we can track consumers."*
#: Before it, a subscribe with no `consumer` was served and recorded as
#: UNIDENTIFIED, and this board sent none — it was the single shipped consumer
#: that the ruling breaks, and `shapes_subscribe` below is where it is repaired.
#: A subscribe without this object is now refused `malformed`.
#:
#: `name` is the PROGRAM, not the library inside it (`docs/shapes.md` §2.1), and
#: it matches this package's distribution name so the ledger row and `pip` agree
#: about what is connected.
CONSUMER_NAME = "threads-tui"

#: What this board was BUILT AGAINST, as it claims it.
#:
#: FREE TEXT, and deliberately not a number: *"say what you were built against,
#: even if it is a date"*. Nothing on the producer compares it — that is rule
#: 2.6's job, over the separate top-level `version` param, and the ledger's own
#: ruling forbids gating on this field. It names the contract REVISION this
#: mirror was written to read, which is the honest answer for a consumer whose
#: version is a document rather than a commit.
CONSUMER_SHAPE_VERSION = "shapes.md@rev4"


class DaemonError(RuntimeError):
    """The daemon answered, and the answer was a refusal."""


class MethodAbsent(DaemonError):
    """The daemon does not know this method.

    A subclass of DaemonError so that a caller which does not care about the
    distinction still handles it, and one that does can catch it precisely.
    """

    def __init__(self, method: str, note: str) -> None:
        super().__init__(note)
        self.method = method
        self.note = note


class DaemonUnavailable(RuntimeError):
    """There is no daemon to answer."""


class ShuttingDown(RuntimeError):
    """The app is closing; this answer is no longer wanted."""


def _harvest(future: "asyncio.Future") -> None:
    if not future.cancelled():
        future.exception()


async def _swallow(task: "asyncio.Task") -> None:
    try:
        await task
    except (asyncio.CancelledError, Exception):
        pass


GIVE_UP_AFTER = 4
BACKOFF = (1.0, 2.0, 4.0, 8.0, 15.0, 30.0)
REQUEST_TIMEOUT = 20.0

# The verdict wait. The old app waited 8s for a call it allowed 30s, so a slow
# but successful login could be reported as a failure and a failed one as fine.
# A verdict wait must be >= the call it is waiting on or it is not a wait, it
# is a coin flip. See `threads_tui.model.settle_deadline`.
HEALTH_TIMEOUT = 30.0


async def daemon_alive(timeout: float = 2.0) -> tuple[bool, dict | None]:
    """Ask `GET /health`. A fact from the API, over the daemon's own HTTP route.

    Spoken as raw HTTP over asyncio rather than urllib, because urllib is
    synchronous and this is called from a running event loop. A liveness check
    that blocks the loop is how you turn "the daemon is slow" into "the UI is
    frozen".
    """
    host, port, _ = endpoint.daemon_endpoint()
    writer = None
    try:
        reader, writer = await asyncio.wait_for(
            asyncio.open_connection(host, port), timeout=timeout
        )
        writer.write(
            f"GET /health HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n".encode()
        )
        await writer.drain()
        raw = await asyncio.wait_for(reader.read(65536), timeout=timeout)
        head, _, body = raw.partition(b"\r\n\r\n")
        if b" 200 " not in head.split(b"\r\n")[0]:
            return False, None
        try:
            return True, json.loads(body.decode("utf-8", "replace"))
        except Exception:
            return True, None
    except Exception:
        return False, None
    finally:
        if writer is not None:
            try:
                writer.close()
            except Exception:
                pass


@dataclass
class Api:
    on_state_change: Callable[[LinkState], None] | None = None
    on_capabilities: Callable[[], None] | None = None
    # SHAPE FRAMES — the only frames this connection is sent that are not
    # answers. See `_dispatch`. There is no `on_push`: it carried the legacy
    # firehose, which this board no longer subscribes (see `_handshake`).
    on_shape: Callable[[dict], None] | None = None

    state: LinkState = LinkState.CONNECTING
    retries: int = 0
    boot_id: str | None = None
    daemon_version: str | None = None
    last_error: str | None = None
    caps: caps.Registry = field(default_factory=caps.Registry)

    _ws: Any = None
    _task: asyncio.Task | None = None
    _pending: dict[str, asyncio.Future] = field(default_factory=dict)
    _closing: bool = False

    # ── lifecycle ────────────────────────────────────────────────────────────

    def start(self) -> None:
        if self._task is None:
            self._task = asyncio.create_task(self._run(), name="threads-api")

    @property
    def shutting_down(self) -> bool:
        return self._closing

    @property
    def connected(self) -> bool:
        return self._ws is not None and self.state is LinkState.LIVE

    def begin_shutdown(self) -> None:
        """Flip the closing flag SYNCHRONOUSLY. Quitting is a keypress, and a
        keypress handler cannot await."""
        self._closing = True
        self._cancel_pending()
        if self._task is not None:
            self._task.cancel()

    async def stop(self) -> None:
        self.begin_shutdown()
        if self._ws is not None:
            try:
                await self._ws.close()
            except Exception:
                pass
        if self._task is not None:
            task, self._task = self._task, None
            task.cancel()
            try:
                await asyncio.wait_for(_swallow(task), timeout=3.0)
            except (asyncio.TimeoutError, asyncio.CancelledError, Exception):
                pass

    def _set_state(self, state: LinkState) -> None:
        if self._closing:
            return
        if state != self.state:
            self.state = state
            if self.on_state_change is not None:
                try:
                    self.on_state_change(state)
                except Exception:
                    pass

    async def _run(self) -> None:
        from websockets.asyncio.client import connect

        first = True
        while not self._closing:
            self._set_state(LinkState.CONNECTING if first else LinkState.RECONNECTING)
            try:
                async with connect(
                    endpoint.ws_url(),
                    open_timeout=5,
                    ping_interval=20,
                    ping_timeout=20,
                    max_size=32 * 1024 * 1024,
                ) as ws:
                    await self._handshake(ws)
                    self._ws = ws
                    self.retries = 0
                    self.last_error = None
                    self._set_state(LinkState.LIVE)
                    # Probing is fire-and-forget: the read loop must start
                    # immediately or the probe's own replies never arrive.
                    asyncio.create_task(self._probe_capabilities())
                    async for raw in ws:
                        self._dispatch(raw)
            except asyncio.CancelledError:
                raise
            except Exception as exc:  # noqa: BLE001 — every failure is a retry
                self.last_error = (
                    f"{type(exc).__name__}: {exc}" if str(exc) else type(exc).__name__
                )
            finally:
                self._ws = None
                self._fail_pending("the daemon connection dropped")

            if self._closing:
                break
            first = False
            self.retries += 1
            self._set_state(
                LinkState.OFFLINE if self.retries >= GIVE_UP_AFTER else LinkState.RECONNECTING
            )
            await asyncio.sleep(BACKOFF[min(self.retries - 1, len(BACKOFF) - 1)])

    async def _handshake(self, ws) -> None:
        welcome = json.loads(await asyncio.wait_for(ws.recv(), timeout=5))
        if welcome.get("type") != "welcome":
            raise DaemonUnavailable(f"unexpected first frame {welcome.get('type')!r}")

        boot_id = welcome.get("boot_id")
        # A different boot id is a different daemon process. Anything we learned
        # about the old one — above all its capability map — describes a daemon
        # that is not on the other end of this socket any more.
        if boot_id != self.boot_id:
            self.caps.reset()
        self.boot_id = boot_id
        self.daemon_version = welcome.get("daemon_version")

        if welcome.get("auth_required"):
            _, _, secret = endpoint.daemon_endpoint()
            if not secret:
                raise DaemonUnavailable(
                    "the daemon requires a shared secret; set THREADS_SECRET "
                    "(or `secret` in the threads config) and relaunch"
                )
            await ws.send(json.dumps({"type": "auth", "secret": secret}))
            ack = json.loads(await asyncio.wait_for(ws.recv(), timeout=5))
            if ack.get("type") != "auth_ack" or not ack.get("ok"):
                raise DaemonUnavailable("the daemon rejected the shared secret")

        # **NO `{"type": "subscribe"}` IS SENT, AND NONE MAY BE.** That frame
        # is the legacy FIREHOSE subscribe, and this board sent one here for one
        # reason: `info`, `progress` and `lifecycle` carry an empty thread id,
        # so no per-thread filter reaches them and the firehose was their only
        # door. The `narration` shape carries all three now (`shapes.NARRATION`,
        # subscribed beside the others in `BoardApp.subscribe_shapes_worker`),
        # so there is nothing left for a firehose to deliver to this board, and
        # the board was the last face holding it open by design
        # (`docs/consumer-seam.md` §6, M6). Every frame this connection receives
        # is now an answer to something it asked: a `response`, or a `shape`
        # frame for a `(shape, key)` it subscribed. `tests/test_narration.py`
        # records every frame the board sends and fails on a `subscribe`.

    def _dispatch(self, raw) -> None:
        try:
            frame = json.loads(raw)
        except Exception:
            return
        ftype = frame.get("type")
        if ftype == "shape":
            # The daemon sends one of these only to a connection that has
            # already received its own export for that `(shape, key)` —
            # `server.rs::drive`'s `shape_subs` is populated by nothing else —
            # so a board that never subscribes cannot receive one and a board
            # that does cannot receive another consumer's.
            if self.on_shape is not None and not self._closing:
                try:
                    self.on_shape(frame)
                except Exception:
                    pass
            return
        if ftype != "response":
            # ANYTHING ELSE IS DROPPED, and that is the firehose's arm deleted
            # rather than guarded. This board subscribes no firehose, so a
            # `lifecycle` / `info` / `progress` / `liveness` frame arriving here
            # was not asked for, and narration reaches the board only as
            # `narration` shape frames above.
            return
        future = self._pending.pop(frame.get("correlate", ""), None)
        if future is None or future.done():
            return
        outcome = frame.get("outcome") or {}
        kind = outcome.get("type")
        if kind == "success":
            future.set_result(outcome.get("payload"))
        elif kind == "error":
            future.set_exception(DaemonError(str(outcome.get("error") or "unknown error")))
        else:
            future.set_exception(DaemonError(f"unsupported outcome {kind!r}"))

    def _fail_pending(self, reason: str) -> None:
        if self._closing:
            self._cancel_pending()
            return
        for future in list(self._pending.values()):
            if not future.done():
                future.set_exception(DaemonUnavailable(reason))
        self._pending.clear()

    def _cancel_pending(self) -> None:
        for future in list(self._pending.values()):
            if not future.done():
                future.cancel()
        self._pending.clear()

    # ── capability discovery ─────────────────────────────────────────────────

    async def _probe_capabilities(self) -> None:
        """Probe the READ methods. Never the write ones — see caps.py."""
        for method in caps.PROBE_ORDER:
            if self._closing or self._ws is None:
                return
            if self.caps.state(method) is not caps.State.UNKNOWN:
                continue
            try:
                await self.request(method, timeout=10.0)
                self.caps.record(method, True)
            except MethodAbsent:
                pass  # already recorded by `request`
            except (DaemonUnavailable, ShuttingDown, asyncio.CancelledError):
                return
            except DaemonError:
                # It answered with a refusal that is not "unknown method", so
                # the method EXISTS and objected to something else (params,
                # state). Present, and the caller will see the real error.
                self.caps.record(method, True)
            except Exception:
                return
        if self.on_capabilities is not None and not self._closing:
            try:
                self.on_capabilities()
            except Exception:
                pass

    # ── requests ─────────────────────────────────────────────────────────────

    async def request(
        self,
        method: str,
        params: dict | None = None,
        timeout: float = REQUEST_TIMEOUT,
        cap: str | None = None,
    ) -> Any:
        """Send one request. `method` is the WIRE PATH; `cap` is its identity.

        **THE TWO ARE NOT THE SAME STRING ONCE A METHOD HAS A SUBJECT.** Since
        the 2026-09-28 path move, `accounts/aa/disable` and `accounts/bb/disable`
        are two paths and ONE capability, so the capability registry is keyed on
        the template (`caps.ACCOUNTS_DISABLE`) and the socket is given the filled
        path. `cap` defaults to `method`, which is correct for every literal
        name; a templated call site passes both.

        Keying the registry on the filled path instead would be quietly wrong
        rather than loudly: `missing()` would grow one entry per account, and
        `note()` — the one thing this module exists for — would fall through to
        "<path> is not available." instead of the sentence written for that
        capability, at exactly the moment the operator is asking why a key did
        nothing.
        """
        cap = cap or method
        if self._closing:
            raise ShuttingDown("the app is closing")
        ws = self._ws
        if ws is None:
            raise DaemonUnavailable("the threads daemon is not connected")
        request_id = uuid.uuid4().hex[:12]
        future: asyncio.Future = asyncio.get_running_loop().create_future()
        future.add_done_callback(_harvest)
        self._pending[request_id] = future
        try:
            await ws.send(
                json.dumps(
                    {
                        "type": "request",
                        "id": request_id,
                        "method": method,
                        "params": params or {},
                    }
                )
            )
            result = await asyncio.wait_for(future, timeout=timeout)
            # A method that answers is a method that exists. Recording success
            # here is what lets a WRITE capability become known without ever
            # being probed speculatively.
            if self.caps.state(cap) is caps.State.UNKNOWN:
                self.caps.record(cap, True)
            return result
        except asyncio.TimeoutError:
            self._pending.pop(request_id, None)
            raise DaemonUnavailable(f"{method} timed out after {timeout:.0f}s") from None
        except asyncio.CancelledError:
            self._pending.pop(request_id, None)
            if self._closing:
                raise ShuttingDown("the app is closing") from None
            raise
        except MethodAbsent:
            raise
        except DaemonError as exc:
            if self.caps.record_from_error(cap, str(exc)):
                raise MethodAbsent(cap, self.caps.note(cap)) from None
            raise
        except (DaemonUnavailable, ShuttingDown):
            raise
        except Exception as exc:
            self._pending.pop(request_id, None)
            if self._closing:
                raise ShuttingDown("the app is closing") from None
            raise DaemonUnavailable(f"{method}: {exc}") from exc

    # ── the facts ────────────────────────────────────────────────────────────
    #
    # One method per fact. Each names the wire method it consumes so that a
    # grep for a method name finds its only caller.

    # ── SIX QUERY METHODS ARE DELETED (S3) ──────────────────────────────────
    #
    # `accounts_status`, `accounts_list`, `thread_get`, `gauge_status`,
    # `accounts_health` and `pool_share_get` lived here, one per wire method,
    # and the board no longer calls any of them: the roster, the login state,
    # the meters, the shares, the pressure and the placement of every running
    # thread all arrive on the `accounts` shape.
    #
    # THE DAEMON STILL ANSWERS ALL SIX. They are deleted from this CLIENT, not
    # from the wire — AUI calls `accounts/list` and `accounts/status`, the CLI
    # uses them, and `threads/{thread_id}/get` is a perfectly good method for
    # anything that wants one thread's record. What is gone is this board's
    # ability to ask, which is the whole point: a client that can still ask has
    # a second way to know, and two ways to know one fact is what made today's
    # disagreement unattributable.
    #
    # **THE PATH MOVE IS WHERE THAT DELETION PAID FOR ITSELF.** Nine commands
    # now answer `{promise_id, accepted, visible, changed}` instead of the fact
    # they caused, and the rule is to read the value off the shape and never off
    # the reply. A board that had kept its six getters would have had a second
    # way to answer each of those, and the temptation at every one of the nine
    # would have been to re-read rather than to wait for the frame. There was
    # nothing to re-add here because there was nothing left to re-add.
    #
    # `HEALTH_TIMEOUT` stays defined (it is `accounts/health`'s 30 s, and
    # `app.py` derives the daemon-down settle from it) even though the call it
    # was written for is gone. Deleting the number and letting the derivation
    # pick a new literal is exactly how the verdict wait drifted apart the
    # first time.

    # ── `gauge_probe()` STOOD HERE AND THIS CLIENT NO LONGER POLLS ───────────
    #
    # It was the `R` key's call: make the daemon run a real `claude /usage`
    # against every login and record the observations. It was the one call this
    # board made that MADE a fact rather than reading one, and it is deleted with
    # the key — deliberately, rather than left unbound. An unreachable method
    # with a 180-second timeout and a cap entry is one line away from being a key
    # again, and the whole value of removing the key is that its absence promises
    # the daemon makes its own readings. A method that can still ask is a promise
    # with a back door.
    #
    # `PROBE_TIMEOUT` GOES WITH IT, unlike `HEALTH_TIMEOUT` above. The rule this
    # file already states is that a number another derivation depends on must
    # survive its call; nothing derives from this one, so keeping it would just
    # be a 180-second budget sitting next to a `request()` waiting to be reused.
    #
    # IF READINGS GO STALE, THAT IS A PRODUCER BUG. See the binding table in
    # `app.py`. The daemon is the only thing that can ask an account anything, and
    # it is now the only thing that decides when to.

    async def pool_share_set(self, account: str, share: float | None, note: str | None = None) -> Any:
        """Set one account's share of the pool.

        **THE ACCOUNT IS THE SUBJECT AND IT IS NOT A PARAMETER.** It rides the
        path, and sending it in the params as well is REFUSED — identically
        whether the two agree or disagree, because a precedence rule would be a
        place for them to disagree. Verified against the live daemon, which
        answers: *"`account` is the SUBJECT of
        `accounts/{account}/pool_share_set` and rides the path, not the params
        — send `accounts/<label>/pool_share_set` and drop `account`"*.

        Remaining params: `{share, note?}`. The bound (0, 1] is REFUSED rather
        than clamped on the daemon's side.

        **`share=None` CLEARS THE OVERRIDE** (R2.S3g): it is sent as JSON
        `null`, PRESENT, and the daemon deletes the account's override row —
        the only way to remove a key an override alone minted. `share` is never
        omitted: an ABSENT `share` is a malformed call to the daemon, and absent
        and null are different statements on purpose.

        **THE REPLY IS AN ACKNOWLEDGEMENT, NOT THE ROW** (rule 2.4) — it has
        been since 2026-09-23 and the caller was already written for it. It is
        `{promise_id, visible, changed}`; the EFFECTIVE share arrives on the
        shape's `share` cell, carrying `promises: [promise_id]` on the frame
        that resolves it. Confirmed live: a re-assertion of the share an
        account already had answered `{"changed": false, "promise_id": 1,
        "visible": true}` and the resolving `live` frame arrived carrying
        `promises: [1]`.
        """
        params: dict[str, Any] = {"share": share}
        if note:
            params["note"] = note
        return await self.request(
            caps.fill(caps.ACCOUNTS_POOL_SHARE_SET, account=account),
            params,
            cap=caps.ACCOUNTS_POOL_SHARE_SET,
        )

    async def account_disable(self, account: str, note: str | None = None) -> Any:
        """Take one account out of placement selection. Not destructive.

        The login, the credentials, the gauge history and the thread history are
        all untouched — the account stays registered and stays on every payload.

        The account is the SUBJECT and rides the path; `note` is the only
        parameter, and the params are EMPTY when he gave no reason. The reply is
        `{promise_id, visible, changed}` and carries no row: the disabled state
        arrives on the shape's `share` cell (`disabled`, `disabled_at`,
        `disabled_note`), which is where the caller already reads it.
        """
        params: dict[str, Any] = {}
        if note:
            params["note"] = note
        return await self.request(
            caps.fill(caps.ACCOUNTS_DISABLE, account=account),
            params,
            cap=caps.ACCOUNTS_DISABLE,
        )

    async def account_enable(self, account: str, share: float | None = None) -> Any:
        """Put one account back.

        `share` is OMITTED on purpose in the normal case: the daemon restores
        what the account had, and supplying a number from here would be the
        interface deciding a thing the daemon already knows the answer to. Pass
        one only when the operator asked for a different weight.
        """
        params: dict[str, Any] = {}
        if share is not None:
            params["share"] = share
        return await self.request(
            caps.fill(caps.ACCOUNTS_ENABLE, account=account),
            params,
            cap=caps.ACCOUNTS_ENABLE,
        )

    # `gauge_history` STOOD HERE (R2.S3d) — the board's last read, the 7-day
    # sparkline's series. The series is the `accounts/gauge-history` shape now,
    # subscribed per account through `shapes_subscribe`; the wrapper is gone so
    # the ability to ask for it is gone with it.

    # `daemon_status` STOOD HERE (R2.S3b). Nothing called it; liveness and the
    # boot identity are the `heartbeat` and `boot` shapes. A wrapper with no
    # caller is an ability to ask, and the ability is what was removed.

    # `daemon_image` STOOD HERE too (R2.S3b): no caller in the board, the tools
    # or the faces, and `boot` carries no image cell. `promote status threads`
    # answers which image is running.

    async def daemon_restart(self) -> Any:
        return await self.request(caps.DAEMON_RESTART)

    async def login_begin(self, account: str, **kw) -> Any:
        """Start a login. **`account` IS A PARAMETER HERE, NOT A SUBJECT.**

        It is not keyed on the account because the account need not exist: `n`
        creates a slot BY signing it in, so there is nothing for a path to name.
        Verified live — a bare `accounts/login/begin` answers `missing required
        param: account`, not `unknown method`.

        **ITS REPLY IS THE ONLY SOURCE OF THE `session_id` THAT `complete`
        NEEDS**, so a caller must keep it. The payload is `{session_id,
        account, url, next, expires_in_secs}`, where `next` names the method to
        call — the daemon spells its own protocol so an interface does not have
        to encode it twice.
        """
        return await self.request(caps.LOGIN_BEGIN, {"account": account, **kw})

    async def login_complete(self, session_id: str, code: str, **kw) -> Any:
        """Finish the login `session_id` names, with the code he pasted.

        **THIS SIGNATURE TOOK AN ACCOUNT LABEL AND NOW TAKES A SESSION ID, AND
        THAT IS A CAPABILITY THE DAEMON DELETED RATHER THAN RENAMED.** The arm
        used to accept `account` as a fallback and resolve it against the
        in-flight logins — one act reachable under two different subjects,
        settled by a precedence rule. Two logins can be in flight at once, so
        that lookup could find the wrong one; the fallback is gone and the
        session id is the only key.

        There is no second way to obtain one and that is deliberate: a caller of
        this method is by definition the process that called `login_begin`, and
        `begin` handed it the id. Passing the label would now be sent as the
        path segment, name no session, and be refused — which is better than
        the old resolution, where it could silently complete a different login.
        """
        return await self.request(
            caps.fill(caps.LOGIN_COMPLETE, session_id=session_id),
            {"code": code, **kw},
            cap=caps.LOGIN_COMPLETE,
        )

    async def accounts_rename(self, old: str, new: str) -> Any:
        """Move a slot: its directory, its keychain item, the manifest, the registry.

        The daemon answers with what it MOVED — `old_label`, `new_label`,
        `credential_moved`, and `history_keeps_old_name` — so the caller reports
        the echo rather than the two strings it sent. See `slots.rename_verdict`.

        The OLD name is the subject and rides the path; `new_name` stays a
        parameter, because it is not a key the daemon holds anything under yet.
        """
        return await self.request(
            caps.fill(caps.ACCOUNTS_RENAME, account=old),
            {"new_name": new},
            120.0,
            cap=caps.ACCOUNTS_RENAME,
        )

    async def accounts_delete(self, account: str) -> Any:
        """Remove a slot. LOCAL ONLY — the daemon answers `revoked: false`, always.

        **`force` IS DELIBERATELY NOT A PARAMETER HERE.** The daemon accepts it
        and uses it to get past one refusal: deleting the slot the machine login
        is signed in as. That refusal names its own remedy and the override
        leaves the pool a spawnable login short, so this board has no way to
        send it — not as an argument nobody passes, which is an argument
        somebody eventually passes, but by not existing. See `slots`.

        The account rides the path, so the params are EMPTY — and `force` not
        existing is now visible in the request itself rather than only in this
        docstring.
        """
        return await self.request(
            caps.fill(caps.ACCOUNTS_DELETE, account=account),
            {},
            120.0,
            cap=caps.ACCOUNTS_DELETE,
        )

    async def accounts_rotate(self, *, dry_run: bool = False) -> Any:
        """Refresh every slot's credential. **THERE IS NO PER-ACCOUNT ROTATION.**

        The signature said `account: str | None` and the daemon has never read
        one: `server.rs` takes `dry_run` off the params and nothing else, then
        runs `claude-accounts rotate`, which walks the whole registry. A
        parameter the wire ignores is worse than a missing one — it lets a
        caller believe it scoped an operation that is pool-wide — so it is gone
        rather than documented. This method had zero callers when it changed.

        Answers `{run_id, ok, dry_run, result}` where `result` is the pass's own
        `--json` blob. The `Push::Progress` frames that arrive while it runs are
        NARRATION; the verdict is this return value. 900 s, because a pass
        probes every slot serially and each probe is capped at two minutes.

        **AND IT IS NOT KEYED ON AN ACCOUNT, WHICH THE PATH NOW SAYS OUT LOUD.**
        `accounts/rotate` has no subject segment because the act has no subject:
        it is pool-wide. The old name could be read as a per-account call with
        an omitted argument; this one cannot be.
        """
        return await self.request(caps.ACCOUNTS_ROTATE, {"dry_run": dry_run}, 900.0)

    # ── shapes ───────────────────────────────────────────────────────────────

    async def shapes_list(self) -> Any:
        """Which folds this daemon holds, and what each one knows.

        THE KEYS IT REPORTS ARE THE KEYS THE TASK IS HOLDING, which for a LAZY
        shape with no subscriber is NONE — the task holds no state until the
        first subscribe item reaches it. So this answers "does this daemon have
        an `accounts` fold at all", and it is `shapes_subscribe` with no key
        that enumerates. See `BoardApp._subscribe_shapes`.

        **THE BOARD NEVER READS THIS PAYLOAD**, and that is why the shape of
        `ShapeInfo` moving under it cost nothing: `keyed` leaving, `max`
        arriving as a required object, `consumers[].key` becoming `keys` +
        `keyless`. The call exists as the one PROBE that decides whether there
        is a fold to mirror at all; its answer is discarded. A consumer that
        deserialised this strictly would have broken on `max` and on
        `max.retained_items` and on `max.beat_interval_ms` in turn — this one
        reads no key off it, so it broke only on the method NAME.
        """
        return await self.request(caps.SHAPES_LIST)

    async def shapes_subscribe(
        self, shape: str, key: str | None = None, at: str | None = None
    ) -> Any:
        """Ask for an export. Answers `{queued: true}` — the export is a FRAME.

        **KEY OMITTED MEANS EVERY KEY THE SHAPE HOLDS.** That is the daemon's
        own rule (`shape.rs::do_subscribe`: "a keyless subscribe on a keyed
        shape means every key I know about"), it hydrates the shape if it is
        lazy and cold, and each key gets its own export addressed to this
        connection. It is therefore both the hydrate trigger and the key
        enumerator, in one round trip that cannot race a key into existence
        between a list and a subscribe.

        `at` is RFC3339 and is a CADENCE for a consumer that wants one. The
        board is not one: every change arrives as a row, so it never asks for a
        future export. A past `at` is treated as absent by the daemon — cells
        hold no history, so a snapshot "as of" a past instant is not something
        it can produce and it refuses to pretend.

        The reply is an ERROR, not `{queued: true}`, when the fold is dead. That
        is deliberate on the daemon's side and it is the one answer this client
        must not swallow: a consumer told its request was accepted and then
        left waiting forever on a fold that is gone is the exact failure the
        supervision exists to close.

        **THE SHAPE IS THE SUBJECT AND RIDES THE PATH.** `key` and `at` stay
        parameters — they are optional, and the mechanical rule is that
        required goes in the path and optional stays where it was.

        **`consumer` IS NOT OPTIONAL, since 2026-10-08.** This docstring used to
        end *"so a keyless subscribe is `shapes/accounts/subscribe` with EMPTY
        params, which is exactly the call this board makes"* — and that call is
        now REFUSED. The operator ruled identity compulsory so that consumers can
        be tracked; a keyless subscribe is therefore `{consumer: {...}}` and
        never `{}`. `key` and `at` remain optional and are still omitted when
        absent, so the keyless/keyed distinction is unchanged — what changed is
        that the floor of the params object is no longer empty.

        Note what this means for the old spelling: `shapes/subscribe` with
        `{"shape": "accounts"}` is not a malformed request any more, it is
        `unknown method` — omitting a subject no longer produces a half-formed
        call, it produces a path that is not a method.
        """
        params: dict[str, Any] = {
            "consumer": {
                "name": CONSUMER_NAME,
                "shape_version": CONSUMER_SHAPE_VERSION,
                # The field that makes a finding ACTIONABLE: knowing a stale
                # board is connected and being able to end it are different
                # things, and this is the second one.
                "pid": os.getpid(),
                "build": __version__,
            }
        }
        if key is not None:
            params["key"] = key
        if at is not None:
            params["at"] = at
        return await self.request(
            caps.fill(caps.SHAPES_SUBSCRIBE, shape=shape),
            params,
            cap=caps.SHAPES_SUBSCRIBE,
        )

    async def shapes_unsubscribe(self, shape: str, key: str | None = None) -> Any:
        params: dict[str, Any] = {}
        if key is not None:
            params["key"] = key
        return await self.request(
            caps.fill(caps.SHAPES_UNSUBSCRIBE, shape=shape),
            params,
            cap=caps.SHAPES_UNSUBSCRIBE,
        )

    async def login_restored(self, account: str) -> Any:
        return await self.request(
            caps.fill(caps.ACCOUNTS_LOGIN_RESTORED, account=account),
            {},
            cap=caps.ACCOUNTS_LOGIN_RESTORED,
        )
