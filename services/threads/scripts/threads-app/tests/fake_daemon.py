"""THE FAKE DAEMON'S SHAPE SURFACE — one wire contract, shared by every test.

**WHY THIS FILE EXISTS, AND WHY IT IS NOT A TEST.** After stage S3 the board
has exactly one way to know anything: the `accounts` shape. That means every
test that drives the real `BoardApp` against a fake daemon needs that daemon to
HOLD A FOLD — not as an extra, but as the only thing that puts a card on the
screen. Three test files stand up a socket (`test_place`, `test_refresh`,
`test_shapes`) and a fourth builds boards directly, and if each grew its own
shape surface there would be four wire contracts to drift apart. There is one,
it is here, and it is deliberately NOT named `test_*` so no runner collects it.

Everything below is the wire EXACTLY as `shape.rs` serialises it. `ShapeWire` is
`#[serde(skip_serializing_if = "Option::is_none")]` on every optional field, so
an absent `key` is ABSENT and not null, a live frame has no `as_of` and an
export has no `at`. Building the frames that way rather than filling every
field with a plausible value is what makes these tests evidence about the real
wire instead of about a convenient one.
"""

from __future__ import annotations

import asyncio
import json
from datetime import datetime, timedelta, timezone

UTC = timezone.utc
NOW = datetime(2026, 9, 19, 12, 0, 0, tzinfo=UTC)
T0 = "2026-09-19T11:59:00+00:00"


# ═══════════════════════════════════════════════════════════════════════════
# THE ROUTER — because a method name is a PATH now
# ═══════════════════════════════════════════════════════════════════════════
#
# Every daemon method is `family[/{subject}]/leaf` since 2026-09-28, and the
# subject rides the path. A fake that matched `method == "accounts.disable"`
# and read `params["account"]` was testing the board against a daemon that no
# longer exists — and worse, it would have gone on passing a board that sent
# the subject in BOTH places, which the real daemon refuses outright.
#
# So the fakes route exactly as `shapewire::server::{route, bind_subject}`
# does: match the concrete path to a TEMPLATE, lift the subject out of the
# path, and REFUSE if the caller also put it in the params. Each fake then
# switches on the template and reads the subject from `params` as it always
# did, which is why this change did not have to reach into their bodies.

# `lib/threads/src/server.rs::WS_METHODS`, verbatim. The whole inventory and
# not just the board's own calls, deliberately: a fake that only knows the
# methods the board sends cannot prove that the board sends no others, which
# is the claim `queries_asked` exists to make.
WIRE_METHODS = (
    "accounts/login/begin",
    "accounts/login/{session_id}/cancel",
    "accounts/login/{session_id}/complete",
    "accounts/rotate",
    "accounts/{account}/delete",
    "accounts/{account}/disable",
    "accounts/{account}/enable",
    "accounts/{account}/login_restored",
    "accounts/{account}/pool_share_set",
    "accounts/{account}/rename",
    "agents/gather",
    "agents/list",
    "agents/network_create",
    "agents/reach",
    "agents/spawn",
    "agents/spawn_batch",
    "agents/watch",
    "agents/{agent}/interrupt",
    "agents/{agent}/log",
    "agents/{agent}/message",
    "agents/{agent}/result",
    "agents/{network}/broadcast",
    "agents/{network}/network_admit",
    "agents/{network}/network_configure",
    "agents/{network}/network_join",
    "agents/{network}/network_leave",
    "agents/{network}/network_remove",
    "budgets/{key}/delete",
    "budgets/{key}/disable",
    "budgets/{key}/enable",
    "budgets/{key}/get",
    "budgets/{key}/set",
    "daemon/image",
    "daemon/restart",
    "gauge/probe",
    "gauge/status",
    "shapes/list",
    "shapes/{shape}/subscribe",
    "shapes/{shape}/unsubscribe",
    "threads/create",
    "threads/list",
    "threads/{thread_id}/budget_assign",
    "threads/{thread_id}/budget_get",
    "threads/{thread_id}/cancel",
    "threads/{thread_id}/event",
    "threads/{thread_id}/history",
    "threads/{thread_id}/queue_cancel",
    "threads/{thread_id}/queue_reselect",
    "threads/{thread_id}/reconcile_transcript",
    "threads/{thread_id}/seen",
    "threads/{thread_id}/send",
    "threads/{thread_id}/update",
)

#: R2.S3c (R4.S9, HOLDS R9): the ten reads the daemon RETIRED once every
#: consumer had moved onto shapes. They are NOT on the wire — the fakes refuse
#: them exactly as the daemon does — but they are named here so a test can
#: still CLASSIFY a frame the board sent as one of them (`classify`). Without
#: that, every "the board never asked X" check would route X to `None` and
#: compare `None` against X: a probe that cannot fail.
RETIRED_READS = (
    "accounts/health",
    "accounts/list",
    "accounts/pool_share_get",
    "accounts/pressure",
    "accounts/status",
    "daemon/status",
    "gauge/history",
    "threads/status",
    "threads/{thread_id}/get",
    "threads/{thread_id}/query",
)

_LITERALS = frozenset(m for m in WIRE_METHODS if "{" not in m)
# Longest head first, which is the daemon's "longest known family off the
# front" rule. Without it `accounts/login/s1/complete` could be read as an
# account literally named `login/s1`.
_TEMPLATED = tuple(
    sorted(
        (m for m in WIRE_METHODS if "{" in m),
        key=lambda m: len(m.partition("{")[0]),
        reverse=True,
    )
)


# The three the fakes compare against by name, spelled once.
SHAPES_LIST = "shapes/list"
SUBSCRIBE = "shapes/{shape}/subscribe"
UNSUBSCRIBE = "shapes/{shape}/unsubscribe"


class SubjectInParams(ValueError):
    """The caller sent the subject in the path AND in the params.

    The real daemon refuses this rather than resolving it by precedence, and
    the refusal is the same whether the two agree or disagree — a precedence
    rule is a place for them to disagree, and deleting that place is the whole
    point of the move. A half-migrated call therefore fails LOUDLY here, which
    is the only reason these tests can tell a migrated board from a board that
    kept its old params.
    """


def _match(method: str, literals, templated) -> tuple[str | None, dict]:
    if method in literals:
        return method, {}
    for template in templated:
        head, _, rest = template.partition("{")
        name, _, tail = rest.partition("}")
        if not method.startswith(head) or not method.endswith(tail):
            continue
        subject = method[len(head) : len(method) - len(tail)]
        if subject:
            return template, {name: subject}
    return None, {}


_ALL = WIRE_METHODS + RETIRED_READS
_ALL_LITERALS = frozenset(m for m in _ALL if "{" not in m)
_ALL_TEMPLATED = tuple(
    sorted(
        (m for m in _ALL if "{" in m),
        key=lambda m: len(m.partition("{")[0]),
        reverse=True,
    )
)


def classify(method: str) -> str | None:
    """The TEMPLATE a sent path names, retired reads included.

    `route` answers what the daemon SERVES; this answers what the board ASKED
    for, which is the question every "never asked" check is about.
    """
    return _match(method, _ALL_LITERALS, _ALL_TEMPLATED)[0]


def route(method: str) -> tuple[str | None, dict]:
    """A concrete path in; `(template, {subject_name: value})` out.

    `(None, {})` for a path that matches no template — which the fakes render
    as `unknown method: <what was sent>`, exactly as the daemon does. Note that
    this is also what an OMITTED subject now produces: `shapes/subscribe` is
    not a malformed request, it is a path that is not a method.
    """
    if method in _LITERALS:
        return method, {}
    for template in _TEMPLATED:
        head, _, rest = template.partition("{")
        name, _, tail = rest.partition("}")
        if not method.startswith(head) or not method.endswith(tail):
            continue
        subject = method[len(head) : len(method) - len(tail)]
        # Non-empty, and never swallowing the leaf: `accounts//disable` and a
        # bare `accounts/disable` both name no subject.
        if subject:
            return template, {name: subject}
    return None, {}


def bind_subject(method: str, params: dict) -> tuple[str, dict]:
    """Route, then fold the path's subject into the params for the handler.

    Raises `SubjectInParams` when the caller also sent it, and `KeyError` when
    the path names no method — the two failures the fakes already render.
    """
    template, subject = route(method)
    if template is None:
        raise KeyError(method)
    out = dict(params or {})
    for name, value in subject.items():
        if name in out:
            raise SubjectInParams(
                f"`{name}` is the SUBJECT of `{template}` and rides the path, "
                f"not the params — send `{method}` and drop `{name}`"
            )
        out[name] = value
    return template, out


# ═══════════════════════════════════════════════════════════════════════════
# Frame builders
# ═══════════════════════════════════════════════════════════════════════════


def known(folded_at: str = T0, **kw) -> dict:
    out = {"state": "known", "complete": True, "folded_at": folded_at}
    out.update(kw)
    return out


def not_hydrated(reason: str) -> dict:
    return {"state": "not_hydrated", "reason": reason}


def cells(**overrides) -> dict:
    from threads_tui import model

    out = {name: known() for name in model.SHAPE_CELLS}
    out.update(overrides)
    return out


def credential(**kw) -> dict:
    out = {
        "mode": "oauth",
        "subscription_type": "max",
        # NO `days_left`. The wire carries the INSTANT and the consumer
        # subtracts — see `model._days_until`.
        #
        # THE TWO EXPIRIES ARE DELIBERATELY FAR APART, and the short one is the
        # ACCESS token. A fixture that gave them the same instant would let the
        # board read either field and stay green, which is exactly how `login
        # 0d` reached every card on the live board: the access token is refreshed
        # lazily every couple of hours, the REFRESH token is the login's real
        # end, and only the second one is worth a countdown. Two hours and twelve
        # days is what a healthy account actually looks like.
        "access_expires_at_ms": int((NOW + timedelta(hours=2)).timestamp() * 1000),
        "refresh_expires_at_ms": int((NOW + timedelta(days=12)).timestamp() * 1000),
        "access_expired": False,
        "identity": {"acct": "acct0@example.com", "email": "acct0@example.com"},
        "secret_sha8": "deadbeef",
        "keychain_service": "Claude Code-credentials",
        "is_live_login": False,
        "slot_state": "live",
        "clone_of": None,
        "folds_into": None,
        "live_store_expires_at_ms": None,
        "live_store_state": None,
    }
    out.update(kw)
    return out


def machine_default_note(**kw) -> dict:
    """`MachineDefaultNote` — the machine-default store as a NOTE on its host.

    Ruling 5: the machine login is never its own key. It rides on the row of the
    pool account it folds into, which is the same row `is_live_login` is true on,
    and it carries its OWN pair of expiries — which is where the machine store's
    login countdown comes from.
    """
    out = {
        "label": "default",
        "state": "live",
        "access_expires_at_ms": int((NOW + timedelta(hours=2)).timestamp() * 1000),
        "refresh_expires_at_ms": int((NOW + timedelta(days=12)).timestamp() * 1000),
        "access_expired": False,
        "subscription_type": "max",
        "identity": {"acct": "acct0@example.com", "email": "acct0@example.com"},
        "secret_sha8": "deadbeef",
        "keychain_service": "Claude Code-credentials",
    }
    out.update(kw)
    return out


def gauge_axis(pct: float = 41.0, observed_at: str = T0) -> dict:
    return {
        "observed_pct": pct,
        "resets_at": "2026-09-22T00:00:00+00:00",
        "source": "usage",
        "observed_at": observed_at,
        "limit_tokens": 1000,
        "usd_per_token": None,
        "confidence": 1.0,
        "calibration_samples": 15,
        "calibrated": True,
    }


def row(account: str, **kw) -> dict:
    out = {
        "account": account,
        "registered": True,
        "login_dir": f"/tmp/{account}",
        # `Budgets::pool_share_row`, field for field. `prior` and `restore_to`
        # are GONE from it: a disable no longer moves the share, so there is
        # nothing to remember and nothing to restore. `disabled_at` and
        # `disabled_note` took their place, and they are `null` here because
        # this account is in the pool — the daemon maintains all three together
        # and there is no state in which one is set and another is not.
        "share": {
            "account": account,
            "share": 0.5,
            "disabled": False,
            "disabled_at": None,
            "disabled_note": None,
            "override": 0.5,
            "declared": None,
            "note": None,
            "source": "override",
        },
        "gauge": {"weekly-all": gauge_axis()},
        "limit": None,
        "health": {"healthy": True, "dead_since": None, "dead_reason": None},
        "credential": credential(),
        "pressure": {
            "account": account,
            "ranked": True,
            "pressure": 1.4,
            "rank": 1,
            "unranked_reason": None,
            "disabled": False,
            "share": 0.5,
            "axis": "weekly-all",
            "observed_pct": 41.0,
            "observed_at": T0,
            "resets_at": "2026-09-22T00:00:00+00:00",
            "reset_basis": "observed",
        },
        "moves": [],
        "machine_default": None,
    }
    out.update(kw)
    return out


def thread_row(state: str = "active", executing: bool = True, queued: int = 0) -> dict:
    """One thread's placement fields, minus where it is placed.

    The account is the KEY of the `placed` dict every roster helper below
    takes, so a fixture still reads as "these threads are on this card" — and
    the helpers turn it into what the wire actually carries since R2.S8: one
    `threads/non-idle` roster whose entries each NAME their account.
    """
    return {
        "state": state,
        "executing": executing,
        "queued": queued,
        "since": T0,
    }


#: `threads/non-idle`'s one key and its name, as the board spells them.
NON_IDLE = "threads/non-idle"
FLEET = "fleet"


def roster_row(placed: dict[str, dict[str, dict]] | None = None) -> dict:
    """`{account: {thread_id: thread_row()}}` -> the roster ROW the daemon sends.

    Sorted by thread id, because the producer's `BTreeMap` emits it that way and
    a fixture that shuffled would be testing an order the wire never has.
    """
    entries = []
    for account, threads in (placed or {}).items():
        for tid, t in threads.items():
            entries.append({"id": tid, "account": account, **t})
    entries.sort(key=lambda e: e["id"])
    return {"pool": FLEET, "threads": entries}


def roster_export(
    placed: dict[str, dict[str, dict]] | None = None,
    *,
    c: dict | None = None,
    as_of: str = T0,
) -> dict:
    """The roster's export frame: one key, `fleet`, one cell, `threads`."""
    return {
        "type": "shape",
        "shape": NON_IDLE,
        "key": FLEET,
        "op": "read",
        "as_of": as_of,
        "epoch": {"init_at": T0, "reinit": 0, "lost": 0, "state": "known"},
        "cells": c if c is not None else {"threads": known()},
        "row": roster_row(placed),
        "shared": {"shape_version": 1, "pool": FLEET},
        "shared_cells": {"shape_version": known()},
    }


def roster_live(placed: dict[str, dict[str, dict]] | None, at: str = T0) -> dict:
    """A live `row` for the roster — the whole roster, as the producer sends it."""
    return {
        "type": "shape",
        "shape": NON_IDLE,
        "key": FLEET,
        "op": "live",
        "at": at,
        "epoch": {"init_at": T0, "reinit": 0, "lost": 0, "state": "known"},
        "event": {"kind": "row", "row": roster_row(placed)},
    }


def roster_death(reason: str = "the shape task panicked") -> dict:
    """The roster task's ONE final frame — `death()`, for `threads/non-idle`."""
    frame = death(reason)
    frame["shape"] = NON_IDLE
    return frame


SHARED = {"warn_below_days": 4.0, "folds_into": {}}


def export(
    key: str,
    *,
    r: dict | None = None,
    c: dict | None = None,
    as_of: str = T0,
    subscribes: list[int] | None = None,
) -> dict:
    """One export frame.

    `subscribes` is the id list `ShapeWire` grew when a subscribe's ack started
    carrying a `subscribe_id` — the answer naming the ask it answers. OMITTED
    unless asked for, because `skip_serializing_if = "Option::is_none"` means
    an absent one is absent and not null, and a fixture that filled every new
    field with a plausible value would stop being evidence about the real wire.
    """
    frame = {
        "type": "shape",
        "shape": "accounts",
        "key": key,
        "op": "read",
        "as_of": as_of,
        "epoch": {"init_at": T0, "reinit": 0, "lost": 0, "state": "known"},
        "cells": c if c is not None else cells(),
        "row": r if r is not None else row(key),
        "shared": SHARED,
        "shared_cells": {"warn_below_days": known(), "folds_into": known()},
    }
    if subscribes is not None:
        frame["subscribes"] = subscribes
    return frame


def live(key: str | None, event: dict, at: str = T0) -> dict:
    frame = {
        "type": "shape",
        "shape": "accounts",
        "op": "live",
        "at": at,
        "epoch": {"init_at": T0, "reinit": 0, "lost": 0, "state": "known"},
        "event": event,
    }
    if key is not None:
        frame["key"] = key
    return frame


def death(reason: str = "the shape task panicked") -> dict:
    """The supervisor's ONE final frame: keyless, `epoch.state: dead`, no row.

    `server.rs::drive` delivers a keyless frame to any connection holding any
    key of that shape, which is how one send reaches every mirror with a `Map`
    root.
    """
    return {
        "type": "shape",
        "shape": "accounts",
        "op": "read",
        "as_of": "2026-09-19T12:00:05+00:00",
        "epoch": {
            "init_at": T0,
            "reinit": 0,
            "lost": 0,
            "state": "dead",
            "dead_reason": reason,
        },
    }


# ═══════════════════════════════════════════════════════════════════════════
# The pure side: a mirror and a board, with no socket at all
# ═══════════════════════════════════════════════════════════════════════════


def mirror_with(*frames: dict):
    from threads_tui import shapes

    m = shapes.ShapeMirror()
    for frame in frames:
        m.on_frame(frame)
    return m


def nonidle_with(*frames: dict):
    """The ROSTER mirror over the same frames — it takes only its own shape's.

    **A FRAME LIST WITH NO ROSTER FRAME IN IT GETS AN EMPTY, KNOWN ROSTER.**
    That is what every board fixture meant before R2.S8, when `row()` carried
    `threads: {}` under a known `threads` cell: "nothing is running here, and
    the daemon said so". A test about the roster's own unknown or dead states
    passes its roster frames explicitly.
    """
    from threads_tui import shapes

    m = shapes.ShapeMirror(name=shapes.NON_IDLE)
    if not any(isinstance(f, dict) and f.get("shape") == NON_IDLE for f in frames):
        frames = (roster_export(), *frames)
    for frame in frames:
        m.on_frame(frame)
    return m


def board_of(*frames: dict, now: datetime = NOW):
    """Frames in, the `Board` the render layer draws out.

    **THIS IS HOW EVERY BOARD FIXTURE IS BUILT NOW.** `build_board(status=…,
    gauge=…, health=…, pressure=…, shares=…)` was the other way and it is
    deleted (S3), so a test that assembled a board out of five payloads was
    assembling one the app can no longer produce. Fixtures that go through the
    same door the app goes through are the only ones that can fail when the app
    would fail.
    """
    from threads_tui import model

    return model.project(mirror_with(*frames), now=now, non_idle=nonidle_with(*frames))


# ═══════════════════════════════════════════════════════════════════════════
# The socket side: the shape surface a fake daemon mixes in
# ═══════════════════════════════════════════════════════════════════════════


class ShapeSurface:
    """`shapes/list` / `shapes/{shape}/subscribe` / `…/unsubscribe`, and exports.

    A fake daemon holds one of these and routes the `shapes` family into
    `payload()` — by TEMPLATE, after `bind_subject` has lifted the shape name
    out of the path — then calls `export_all()` after a SUCCESSFUL subscribe,
    in that order, because that is the real order: `{"queued": true}` answers
    on the request id and the exports follow as their own frames.

    `has_shapes=False` is the absent case: a daemon holding no fold at all. It
    refuses every `shapes` method as an unknown method, which is what
    `caps.is_absence` matches, and that means the board draws no cards and says
    why. Proving that path is not optional: it is the honest consequence of
    having one source.

    **THAT CASE IS ALSO WHAT THE BOARD HIT FOR REAL, AND IT WAS A LIE.** The
    daemon grew six folds and renamed `shapes.list` to `shapes/list`; the board
    kept asking for the old name and got the same `unknown method:` a
    fold-less daemon gives. The screen was correct about what it was told and
    wrong about the world. Nothing in this file could catch that, because the
    fake was renamed in step with the board — which is why the live-daemon
    check in `SHAPES-TUI.md` is not optional either.
    """

    def __init__(self, keys: list[str] | None = None, *, has_shapes: bool = True) -> None:
        self.has_shapes = has_shapes
        self.keys: list[str] = list(keys or ["acct0"])
        self.rows: dict[str, dict] = {k: row(k) for k in self.keys}
        # `{account: {thread_id: thread_row()}}` — what `threads/non-idle`
        # exports. Since R2.S8 the running threads are NOT in `rows`.
        self.placed: dict[str, dict[str, dict]] = {}
        self.dead: str | None = None
        self.subscribes: list[dict] = []
        # THE `narration` FOLD — three channel windows, oldest first, as
        # `shapes/narration.rs` holds them. The `lifecycle` channel starts with
        # this boot's own note, because the real daemon's does: the boot id on
        # it is the `welcome`'s (`BOOT_ID`), so a board that reads it is not
        # looking at a restart.
        self.narration: dict[str, list[dict]] = {
            "info": [],
            "lifecycle": [narration_line("lifecycle", boot_id=BOOT_ID)],
            "progress": [],
        }
        # THE `accounts/gauge-history` FOLD (R2.S3d) — `{account: [reading]}`,
        # oldest first. `PerKey`: a keyed subscribe exports that one account's
        # week. An account NOT in this map is one `gauge_history.rs::
        # hydrates_key` refuses (`false` — outside the pool), and the real hub
        # STILL EXPORTS for it: a keyed frame with no row and no cells
        # (`shapewire::hub`, "exporting a row-less frame so the consumer is
        # told, rather than nothing"). This fake sent no frame at all there,
        # which let a board test pass against a case the producer never
        # produces (R2.S3d inspect, finding 2). It now sends that frame.
        self.history: dict[str, list[dict]] = {}

    def subscribed(self, shape: str) -> bool:
        """Has this connection subscribed `shape`? The real daemon's gate.

        `server.rs::drive` delivers a shape's live frames only to a connection
        holding that `(shape, key)` — so a fake that delivered them to anyone
        would let the board's subscribe be deleted with the board still green.
        """
        return any(s.get("shape", "accounts") == shape for s in self.subscribes)

    def payload(self, method: str, params: dict):
        """`method` is a TEMPLATE and `params` carries the bound subject."""
        if not self.has_shapes:
            raise KeyError(method)
        if method == "shapes/list":
            return {
                "shapes": [
                    {
                        "name": "accounts",
                        # `keyed` is GONE from the daemon's wire (2026-09-28):
                        # `Shape::KEYED` was a const read by no code, so every
                        # consumer that read it read `True`. A fixture that
                        # still emitted it would be testing the board against a
                        # daemon that no longer exists.
                        #
                        # EMPTY until something subscribes, exactly as a LAZY
                        # shape reports itself. This is why `shapes/list`
                        # cannot be the enumerator — and now that a
                        # `detail: keys` SUBSCRIPTION is, it never has to be.
                        "keys": self.keys if self.subscribes else [],
                        "tracking": "lazy",
                        "tracked": bool(self.subscribes),
                        "subscribers": len(self.subscribes),
                        # `consumers[].key` became `keys` (a list) + `keyless`
                        # (a bool) when a keyless subscribe stopped meaning one
                        # thing. Empty here, as the live daemon reports it for
                        # a shape nothing has declared an identity against.
                        "consumers": [],
                        "epoch": {"reinit": 0, "lost": 0, "state": "known"},
                        "fifo_high_water": 0,
                        "bursts": 0,
                        "unplaced_status": 0,
                        # Rule 2.8: every producer ceiling rides `shapes/list`,
                        # so a consumer reads the contract before it uses it.
                        #
                        # **ALL SIX, AND THE LAST TWO ARE THE ONES THIS FIXTURE
                        # WAS MISSING.** `retained_items` and
                        # `beat_interval_ms` are both REQUIRED with no serde
                        # default, so a strict parser breaks on their absence —
                        # and a hand-written `max` is exactly the kind of thing
                        # that agrees with a wrong consumer. These six are the
                        # live daemon's own answer for `accounts`, read off
                        # the live daemon rather than guessed: `0` for both of the new
                        # ones, which are positive claims — this shape holds no
                        # windowed cell, and this shape does not beat.
                        "max": {
                            "rows_per_subscribe": 500,
                            "keys_per_frame": 500,
                            "future_at_secs": 86400,
                            "armed_timers_per_conn": 4,
                            "retained_items": 0,
                            "beat_interval_ms": 0,
                        },
                    }
                ]
            }
        if method == "shapes/{shape}/subscribe":
            if self.dead is not None:
                raise ValueError(
                    f"shape `accounts` is dead ({self.dead}); it cannot be "
                    "subscribed to until the daemon restarts"
                )
            # ── IDENTITY IS COMPULSORY, AND THIS FAKE ENFORCES IT ────────────
            #
            # The real daemon refuses a subscribe that states no `consumer`
            # (operator ruling 2026-10-08; `shapewire::ledger`'s
            # `IDENTITY_REQUIRED_REFUSAL`). This fake accepted anything, which
            # made it USELESS AS A GUARD for exactly the regression that matters
            # here: the board sent no identity at all until that ruling, and had
            # it regressed to sending none again, every test in this suite would
            # have stayed green while the real daemon refused every subscribe
            # the board made. A fake that is more permissive than the thing it
            # stands in for converts a production refusal into a silent pass.
            #
            # The message is the real daemon's, in substance, so a reader who
            # hits this red recognises what the live refusal would say.
            consumer = (params or {}).get("consumer")
            if not isinstance(consumer, dict):
                raise ValueError(
                    "`consumer` is required and must be an object: "
                    "{name, shape_version, pid?, build?} — say who you are. "
                    f"this subscribe sent {consumer!r}"
                )
            for required in ("name", "shape_version"):
                value = consumer.get(required)
                if not isinstance(value, str) or not value.strip():
                    raise ValueError(
                        f"`consumer.{required}` is required and must be a "
                        f"non-empty string; got {value!r}"
                    )
            self.subscribes.append(dict(params or {}))
            # `subscribe_id` rides the ack now and the export that answers it
            # carries `subscribes: [id]`. The board reads neither — it waits on
            # `ShapeMirror.exports` moving — so this is here to BE ignored, and
            # a board that started reading it would find it present.
            return {"queued": True, "subscribe_id": len(self.subscribes)}
        if method == "shapes/{shape}/unsubscribe":
            return {"ok": True}
        raise KeyError(method)

    async def export_all(self, ws, params) -> None:
        """A keyless subscribe exports EVERY key, each addressed to this conn.

        **ONLY FOR THIS SURFACE'S OWN SHAPE, AND THAT GUARD IS LOAD-BEARING.**
        `export()` stamps `shape: "accounts"` unconditionally, and this one
        surface answers `shapes/{shape}/subscribe` for whatever shape is asked.
        So before this check, a `shapes/warnings/subscribe` was answered with a
        SECOND FULL ACCOUNTS EXPORT — the board's accounts mirror applied it,
        because the frames really did say `accounts`, and `ShapeMirror.exports`
        counted it.

        The board only started subscribing a second shape when the `warnings`
        shape landed, so nothing had ever asked this surface for another shape
        and the bug could not exist until then. What it broke was the one
        scenario check that reads the export COUNT as its evidence —
        `scenario_*`'s "A DIFFERENT BOOT ID DROPS THE MIRROR AND
        RE-ENUMERATES", which asserts a fresh enumeration is at most one export
        per account key and was seeing two. Counted: `exports_mid=16` for 4 keys
        and 2 subscribe rounds, i.e. (2 accounts + 2 warnings) x 4.

        A shape this surface does not serve gets the ack it already got and no
        frames. `test_warnings.py` drives the real `warnings` shape with
        `warnings_export()` / `warnings_live()`, which carry their own
        `shape: "warnings"`, so nothing needs this surface to fake it.
        """
        params = params or {}
        if params.get("shape") == NARRATION:
            # THE `narration` SHAPE, served as a shape — one export per channel
            # key, `lines` its one cell. This is the ONLY door narration has on
            # this fake: it serves no firehose (`FakeDaemon._handler`).
            key = params.get("key")
            sub_id = len(self.subscribes) or None
            for channel in ([key] if key else list(self.narration)):
                if channel not in self.narration:
                    continue
                await ws.send(json.dumps(narration_export(
                    channel, self.narration[channel],
                    subscribes=[sub_id] if sub_id else None,
                )))
        if params.get("shape") == GAUGE_HISTORY:
            key = params.get("key")
            sub_id = len(self.subscribes) or None
            if key in self.history:
                await ws.send(json.dumps(history_export(
                    key, self.history[key], subscribes=[sub_id] if sub_id else None,
                )))
            elif key is not None:
                await ws.send(json.dumps(history_refused(
                    key, subscribes=[sub_id] if sub_id else None,
                )))
            return
        if params.get("shape") == NON_IDLE:
            sub_id = len(self.subscribes) or None
            frame = roster_export(self.placed)
            if sub_id:
                frame["subscribes"] = [sub_id]
            await ws.send(json.dumps(frame))
            return
        if params.get("shape", "accounts") != "accounts":
            return
        key = params.get("key")
        wanted = [key] if key else list(self.keys)
        sub_id = len(self.subscribes) or None
        for k in wanted:
            if k not in self.rows:
                continue
            await ws.send(
                json.dumps(
                    export(k, r=self.rows[k], subscribes=[sub_id] if sub_id else None)
                )
            )


# ── driving the real app ─────────────────────────────────────────────────────


def free_port() -> int:
    import socket

    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


async def boot(daemon):
    """Start `daemon` on a free port and hand back an unstarted real `BoardApp`."""
    import os

    port = free_port()
    os.environ["THREADS_TUI_WS_PORT"] = str(port)
    await daemon.start(port)
    from threads_tui.app import BoardApp

    return BoardApp()


async def settle(pilot, seconds: float) -> None:
    import asyncio

    end = asyncio.get_running_loop().time() + seconds
    while asyncio.get_running_loop().time() < end:
        await pilot.pause()
        await asyncio.sleep(0.05)


def body(app) -> str:
    from textual.widgets import Static

    try:
        widget = app.screen.query_one("#body", Static)
    except Exception:
        return ""
    return "\n".join(widget.render_line(y).text for y in range(widget.size.height))


# ── THE S3 ASSERTION EVERY SOCKET TEST CAN MAKE ──────────────────────────────
#
# The five queries the board used to join. Four of them are RETIRED from the
# daemon since R2.S3c (R4.S9) and the fakes refuse them as it does; the claim
# is still that the board DOES NOT ASK, and `queries_asked` proves it off the
# frames the fake RECEIVED (recorded before routing), classified with
# `classify` so a refused method is still recognised.
FIVE_QUERIES = (
    "accounts/status",
    "gauge/status",
    "accounts/health",
    "accounts/pressure",
    "accounts/pool_share_get",
)


def subscribes_for(surface: "ShapeSurface", shape: str = "accounts") -> list[dict]:
    """The subscribes this surface received FOR ONE SHAPE, newest last.

    **`ShapeSurface.subscribes` IS EVERY SUBSCRIBE ON THE CONNECTION, ACROSS
    EVERY SHAPE THE BOARD HOLDS — AND THE BOARD NOW HOLDS MORE THAN ONE.**

    Every scenario check that says "exactly one keyless subscribe enumerated the
    board", or counts a re-subscribe after an announcement, is a claim about ONE
    shape's enumeration. They were written against the global list because
    `accounts` was the only shape there was, so the two readings were the same
    number and nothing distinguished them.

    The `warnings` shape made them different. Nine such checks across
    `test_place.py` and `test_shapes.py` went red at once with
    `[{'shape': 'warnings'}, {'shape': 'accounts'}]` — not because the board had
    started enumerating wrongly, but because it had a second shape to enumerate.
    **Nothing in `pytest tests/` could see it**: those checks live only in the
    modules' `__main__` scenario runners, which drive a real daemon through
    Textual's pilot, and pytest collects the `def test_*` functions beside them
    and not those. The standalone tier is the only place they run.

    Filtering here rather than loosening the counts keeps every claim exactly as
    strong as it was: a per-key fan-out, a second enumeration of this shape, or a
    subscribe that carries a key all still redden.
    """
    return [s for s in surface.subscribes if s.get("shape") == shape]


def queries_asked(methods: list[str]) -> list[str]:
    """Which of the five deleted board queries were asked. Must always be [].

    Routed rather than compared, so the answer does not depend on the spelling
    the board happened to send. All five are subject-less — each answers for
    one account or for the pool, so none of them took a path segment — but
    routing is what makes this honest if one ever does.
    """
    return [m for m in methods if classify(m) in FIVE_QUERIES]


def asked(methods: list[str], *templates: str) -> list[str]:
    """The TEMPLATE of every call in `methods` that is one of `templates`.

    What a test wants to know is *was the account disabled, and then enabled* —
    not whether `accounts/acct0/disable` was the exact string. Templates come
    back rather than the raw paths so the assertion does not have to name the
    fixture's account, which is the kind of coupling that makes a test fail
    when a fixture is renamed. The raw paths are still on `daemon.methods` for
    a failure message to print.
    """
    wanted = frozenset(templates)
    return [t for t in (route(m)[0] for m in methods) if t in wanted]


# ═══════════════════════════════════════════════════════════════════════════
# THE REAL WEBSOCKET FAKE DAEMON — moved here from `test_refresh.py` on
# 2026-10-06, when that file was deleted.
#
# WHY IT MOVED RATHER THAN DIED WITH ITS FILE. `test_refresh.py` existed to prove
# the `R` key's round trip: press the key, the daemon runs a real `claude /usage`,
# the fresh number reaches the painted board. `R` has been REMOVED (the operator's
# forcing function — see `tests/test_keys.py`), so every scenario in that file was
# testing a key that no longer exists and the file went with them.
#
# ITS FAKE DAEMON IS NOT OBSOLETE. `test_place.py` imported it rather than writing
# a second one, with the stated reason that "a second fake daemon would be a
# second wire contract, and the two would drift" — and this one is the one that
# has been checked against the live daemon, including its `GET /health` route. A
# shared fixture living in a test file was always the wrong shape; one test
# importing another is how a deletion in one breaks the other, which is exactly
# what happened. It belongs in this module, which is already the single home for
# the wire contract and is deliberately not named `test_*` so no runner collects
# it.
#
# IT STILL ANSWERS `gauge/probe`, ON PURPOSE. The board can no longer call it, and
# a fake that REFUSED the method would prove only that a missing method cannot be
# called. Keeping it answerable is what makes `test_place.py`'s claim real: the
# board does not poll, measured against a daemon that would have let it.
# ═══════════════════════════════════════════════════════════════════════════

STALE_PCT = 97.0
FRESH_PCT = 12.0

STALE_AT = "2026-08-27T06:12:04+00:00"
FRESH_AT = "2026-08-28T23:43:04+00:00"

def gauge_axes_payload(pct: float, observed_at: str) -> dict:
    return {
        "session": {
            "observed_pct": 5.0,
            "calibrated": True,
            "calibration_samples": 20,
            "confidence": 1.0,
            "observed_at": observed_at,
            "resets_at": "2026-08-29T03:50:00+00:00",
            "source": "usage_poll",
        },
        "weekly_all": {
            "observed_pct": pct,
            "calibrated": True,
            "calibration_samples": 30,
            "confidence": 1.0,
            "observed_at": observed_at,
            "resets_at": "2026-08-30T22:00:00+00:00",
            "source": "usage_poll",
        },
        # The cash axis exactly as the real daemon sends it on an account with
        # no extra-usage billing: present, and entirely null.
        "cash": {
            "observed_pct": None,
            "calibrated": False,
            "calibration_samples": 0,
            "confidence": None,
            "observed_at": None,
            "resets_at": None,
            "source": None,
        },
    }



class FakeDaemon:
    def __init__(self) -> None:
        self.methods: list[str] = []
        self.probe_delay = 0.0
        self.probe_calls = 0
        # Which thread ids the app asked to PLACE, and how many times each.
        # The count is the assertion: once per execution, never per frame.
        self.thread_get_calls: list[str] = []
        # EVERY FRAME THE BOARD SENT, decoded, in arrival order — requests,
        # auth, and anything else. `tests/test_narration.py` asserts over this
        # list that no frame's `type` is `subscribe`; recording only requests
        # would make that assertion unable to fail.
        self.received: list[dict] = []
        # `GET /health` — the route the app's daemon-down detection now uses.
        # `health_ok` is the knob that makes a live daemon look dead without
        # touching the socket, which is the exact failure the health check
        # exists to catch quickly.
        self.health_ok = True
        self.health_calls = 0
        # `accounts/{account}/pool_share_set`'s override rows, by account, and
        # the promise ids its acknowledgements hand out.
        self.share_overrides: dict[str, float] = {}
        self.promises = 0
        self._server = None
        self._conns: set = set()
        # ── THE FOLD, WHICH IS NOW THE ONLY WAY THIS BOARD SEES ANYTHING ────
        #
        # After S3 the board draws itself from the `accounts` shape and from
        # nothing else, so a fake daemon without one puts NO CARDS on the
        # screen — which is a legitimate state (and `test_shapes` proves it is
        # drawn honestly), but it is not the state this file is about. The
        # five query methods above are KEPT and still answered, because the
        # claim being tested is that the board does not ASK them, and that is
        # only provable against a daemon that would have answered.
        self.shape = ShapeSurface(["acct0"])
        self.shape.rows["acct0"] = row(
            "acct0", gauge={"weekly-all": gauge_axis(STALE_PCT, STALE_AT)}
        )

    # The gauge payload the two methods disagree about. This IS the fixture:
    # `gauge/status` keeps handing back the reading the daemon recorded a day
    # and a half ago, and only `gauge/probe` produces a new one.
    def _gauge(self, fresh: bool) -> dict:
        pct = FRESH_PCT if fresh else STALE_PCT
        at = FRESH_AT if fresh else STALE_AT
        return {"accounts": [{"account": "acct0", "axes": gauge_axes_payload(pct, at)}]}

    def _payload(self, method: str, params: dict):
        if method == "gauge/status":
            return self._gauge(fresh=False)
        if method == "gauge/probe":
            self.probe_calls += 1
            # **THE ANSWER COMES BACK THROUGH THE FOLD, NOT THROUGH THIS
            # RETURN VALUE.** The real daemon records an observation per login
            # and each recorded observation emits a `GaugeObserved` fact, which
            # the `accounts` shape applies and publishes as a row. The response
            # payload below still exists on the wire and the board no longer
            # reads it — so the fresh number reaches the screen only if the
            # push path works, which is exactly what this file measures.
            self.shape.rows["acct0"] = row(
                "acct0",
                gauge={"weekly-all": gauge_axis(FRESH_PCT, FRESH_AT)},
            )
            self._probed = True
            return {"observations": 4, "accounts": self._gauge(fresh=True)["accounts"]}
        if method == "accounts/{account}/pool_share_set":
            # **AS THE DAEMON ANSWERS IT** (R2.S3g): `share: null`, PRESENT,
            # clears the override and is not an error; an ABSENT `share` is the
            # daemon's malformed-call refusal; a number outside (0, 1] is
            # refused, never clamped. The reply is the acknowledgement
            # (`{promise_id, visible, changed}`), never the row.
            p = params or {}
            account = p["account"]
            if "share" not in p:
                raise ValueError("missing required param: share (a number)")
            share = p["share"]
            if share is None:
                changed = self.share_overrides.pop(account, None) is not None
            else:
                if isinstance(share, bool) or not isinstance(share, (int, float)):
                    raise ValueError("missing required param: share (a number)")
                if not (0.0 < float(share) <= 1.0):
                    raise ValueError(f"pool share must be a fraction in (0.0, 1.0] — got {share}")
                changed = self.share_overrides.get(account) != float(share)
                self.share_overrides[account] = float(share)
            self.promises += 1
            return {"promise_id": self.promises, "visible": True, "changed": changed}
        if method == "daemon/image":
            return {"ok": True}
        raise KeyError(method)

    async def _handler(self, ws) -> None:
        self._conns.add(ws)
        try:
            await ws.send(
                json.dumps(
                    {
                        "type": "welcome",
                        "proto": 1,
                        "daemon": "threads",
                        "daemon_version": "test",
                        "boot_id": BOOT_ID,
                        "auth_required": False,
                    }
                )
            )
            async for raw in ws:
                frame = json.loads(raw)
                self.received.append(frame)
                kind = frame.get("type")
                # NO FIREHOSE IS SERVED. A `{"type": "subscribe"}` frame is
                # recorded above and refused exactly as the producer refuses it
                # since R2.S4 retired the firehose: one response with no
                # correlate and code `malformed` — no `sub_ack`, and no push is
                # ever gated on it. Narration is the `narration` shape
                # (`ShapeSurface.export_all`), as on the chassis door.
                if kind == "auth":
                    continue
                if kind != "request":
                    await ws.send(
                        json.dumps(
                            {
                                "type": "response",
                                "correlate": "",
                                "outcome": {
                                    "type": "error",
                                    "error": f"invalid frame: unknown type `{kind}`",
                                    "code": "malformed",
                                },
                            }
                        )
                    )
                    continue
                method = frame.get("method")
                self.methods.append(method)
                # Recorded at RECEIPT, not in a payload branch: the daemon
                # refuses `threads/{thread_id}/get` since R2.S3c, so a record
                # kept in its handler could never be written and
                # `thread_get_calls == []` would be a check that cannot fail.
                if classify(method or "") == "threads/{thread_id}/get":
                    self.thread_get_calls.append(method.split("/")[1])
                if method == "gauge/probe" and self.probe_delay:
                    await asyncio.sleep(self.probe_delay)
                params = frame.get("params") or {}
                # ROUTE AS THE DAEMON ROUTES. A method name is a path:
                # `bind_subject` matches it to a template, lifts the subject out
                # of the path into the params for the handler, and REFUSES a
                # caller that also sent it in the params. An unroutable path is
                # a `KeyError`, rendered below as `unknown method:` — which is
                # also what an OMITTED subject now produces, because a path with
                # a hole in it is not a half-formed request, it is not a method.
                template = None
                try:
                    template, bound = bind_subject(method, params)
                    if template.startswith("shapes/"):
                        payload = self.shape.payload(template, bound)
                    else:
                        payload = self._payload(template, bound)
                    outcome = {"type": "success", "payload": payload}
                except KeyError:
                    outcome = {
                        "type": "error",
                        "error": f"unknown method: {method}",
                    }
                except ValueError as exc:
                    outcome = {"type": "error", "error": str(exc)}
                await ws.send(
                    json.dumps(
                        {
                            "type": "response",
                            "correlate": frame.get("id"),
                            "outcome": outcome,
                        }
                    )
                )
                # THE EXPORTS FOLLOW THE RESPONSE, never ride in it. `{"queued":
                # true}` means the subscribe item is in the FIFO; the snapshot
                # is taken when the task reaches it.
                if template == SUBSCRIBE and outcome["type"] == "success":
                    await self.shape.export_all(ws, bound)
                # AND THE PROBE'S ANSWER ARRIVES AS A ROW. The real daemon
                # records observations and its fold publishes them; the board
                # reads the row and never the probe's return value.
                if method == "gauge/probe" and outcome["type"] == "success":
                    await self.push(
                        live(
                            "acct0",
                            {"kind": "row", "row": self.shape.rows["acct0"]},
                            at=FRESH_AT,
                        )
                    )
        except Exception:
            pass
        finally:
            self._conns.discard(ws)

    async def push(self, frame: dict) -> None:
        """Send a live SHAPE frame — delivered only if that shape is subscribed.

        GATED ON THE SHAPE SUBSCRIPTION, exactly as the real daemon gates it
        (`server.rs::drive` delivers a shape's frames only to a connection that
        holds the shape). A fake that pushed unconditionally would let the
        app's subscribe be deleted with the assertion still green — which is
        the whole failure mode this file is here to prevent. A frame that is
        not a shape frame is a firehose frame and is REFUSED here: this fake
        serves no firehose. `push_raw` is the one way to put such a frame on
        the socket, for the test that proves the board ignores it.
        """
        assert frame.get("type") == "shape", f"not a shape frame: {frame.get('type')!r}"
        if not self.shape.subscribed(str(frame.get("shape") or "")):
            return
        await self.push_raw(frame)

    async def narrate(self, kind: str, *, at: str = T0, **fields) -> dict:
        """The daemon says something: append a line to a `narration` channel
        and publish it as the fold does — one `transition` on `lines`."""
        line = narration_line(kind, **fields)
        self.shape.narration[kind].append(line)
        await self.push(narration_live(kind, line, at=at))
        return line

    async def push_raw(self, frame: dict) -> None:
        """Put ANY frame on every open socket, asked for or not. Test use only."""
        for ws in list(self._conns):
            try:
                await ws.send(json.dumps(frame))
            except Exception:
                pass

    def _process_request(self, connection, request):
        """**ANSWER `GET /health`, BECAUSE THE REAL DAEMON DOES.**

        The app now asks `GET /health` every three seconds and shows the
        daemon-down page after two consecutive failures — that is how a daemon
        that dies without closing its socket is noticed in six seconds instead
        of the fifty-five the link state machine needs (see
        `app.HEALTH_SECONDS`).

        A WebSocket-only fake refuses a plain HTTP GET, so without this every
        test in this file would be thrown onto the daemon-down page six seconds
        in and would fail for a reason that has nothing to do with what it is
        testing. That is not a reason to make the app's check optional; it is
        the fixture being thinner than the wire. The payload below is the real
        one, copied from the live daemon's own response.
        """
        if request.path == "/health":
            from websockets.datastructures import Headers
            from websockets.http11 import Response

            body = json.dumps(
                {
                    "boot_id": "boot-test",
                    "daemon": "threads",
                    "graceful_shutdown": True,
                    "proto": 1,
                    "status": "ok",
                    "version": "test",
                }
            ).encode()
            self.health_calls += 1
            if not self.health_ok:
                return Response(503, "Service Unavailable", Headers(
                    {"Content-Length": "0", "Connection": "close"}), b"")
            return Response(200, "OK", Headers({
                "Content-Type": "application/json",
                "Content-Length": str(len(body)),
                "Connection": "close",
            }), body)
        return None

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


# ── the run ──────────────────────────────────────────────────────────────────



# ═══════════════════════════════════════════════════════════════════════════
# THE `warnings` SHAPE — the daemon's count of its OWN warnings
#
# A second shape on the same producer, so a second pair of builders. They are
# HERE rather than in the test file for the reason the header of this section's
# neighbour gives: four copies of the wire would be four contracts to drift
# apart. `tests/test_warnings.py::test_the_board_reads_the_field_names_the_daemon_writes`
# reads `shapes/warnings.rs` and fails if these names stop matching the producer.
# ═══════════════════════════════════════════════════════════════════════════

#: The shape's ONE key — the producer itself. Spelled as
#: `shapes/warnings.rs::KEY_DAEMON` spells it.
WARN_KEY = "daemon"

#: What the producer puts on `shared`. `counts_since` is the DAEMON's start and
#: not the shape's epoch, which is the field that stops a `Lazy` re-init reading
#: as a reset.
WARN_SHARED = {
    "shape_version": 1,
    "key": WARN_KEY,
    "counts_since": T0,
    "sample_interval_ms": 1000,
    "top_kinds": 5,
    "sink_attached": True,
    "sink_dir": "threads.warnings",
    "sink_file": "warnings.jsonl",
}


def warn_row(warn: int = 0, error: int = 0, sink_errors: int = 0, kinds=None) -> dict:
    """One `warnings` row, with the producer's own field names."""
    return {
        "producer": WARN_KEY,
        "warn": warn,
        "error": error,
        "kinds": kinds if kinds is not None else [],
        "sink_errors": sink_errors,
    }


def warn_cells(**overrides) -> dict:
    out = {n: known() for n in ("warn", "error", "kinds", "sink_errors")}
    out.update(overrides)
    return out


def warnings_export(
    warn: int = 0,
    error: int = 0,
    *,
    sink_errors: int = 0,
    kinds=None,
    as_of: str = T0,
) -> dict:
    """The export that answers a `shapes/warnings/subscribe`."""
    return {
        "type": "shape",
        "shape": "warnings",
        "key": WARN_KEY,
        "op": "read",
        "as_of": as_of,
        "epoch": {"init_at": T0, "reinit": 0, "lost": 0, "state": "known"},
        "cells": warn_cells(),
        "row": warn_row(warn, error, sink_errors, kinds),
        "shared": WARN_SHARED,
        "shared_cells": {
            "shape_version": known(),
            "counts_since": known(),
            "sink_attached": known(),
        },
    }


def warnings_live(
    warn: int, error: int = 0, *, sink_errors: int = 0, kinds=None, at: str = T0
) -> dict:
    """A live `row` frame — what a sample that MOVED produces."""
    return {
        "type": "shape",
        "shape": "warnings",
        "key": WARN_KEY,
        "op": "live",
        "at": at,
        "epoch": {"init_at": T0, "reinit": 0, "lost": 0, "state": "known"},
        "event": {"kind": "row", "row": warn_row(warn, error, sink_errors, kinds)},
    }


# ═══════════════════════════════════════════════════════════════════════════
# THE `narration` SHAPE — the daemon talking while it works
#
# `lib/threads/src/shapes/narration.rs`. Three channel keys, one `lines` cell
# each, every line internally tagged on `kind` with the corresponding `Push`
# variant's fields verbatim. These builders are the ONLY narration this fake
# serves: there is no firehose here, so a board that still read `info` /
# `progress` / `lifecycle` off the socket would see nothing.
# ═══════════════════════════════════════════════════════════════════════════

NARRATION = "narration"

#: The `welcome`'s boot id, which is also the boot note's on `lifecycle`.
BOOT_ID = "boot-test"

_LINE_DEFAULTS = {
    "progress": {"op": "rotate", "id": "run-1", "account": None, "phase": "slot",
                 "detail": None, "done": False, "ok": None},
    "lifecycle": {"service": "threads", "phase": "back_up", "reason": "unknown",
                  "detail": "threads came back", "boot_id": BOOT_ID, "at": T0},
    "info": {"domain": "accounts", "reason": "changed", "account": None,
             "detail": None, "at": T0},
}


def narration_line(kind: str, **fields) -> dict:
    """One `Line`, as `#[serde(tag = "kind", rename_all = "snake_case")]`."""
    line = {"kind": kind, **_LINE_DEFAULTS[kind]}
    line.update(fields)
    return line


def narration_export(
    channel: str, lines: list[dict], *, as_of: str = T0,
    subscribes: list[int] | None = None,
) -> dict:
    """The export of one channel: `row` is `{channel, lines}`, `lines` stamped."""
    frame = {
        "type": "shape",
        "shape": NARRATION,
        "key": channel,
        "op": "read",
        "as_of": as_of,
        "epoch": {"init_at": T0, "reinit": 0, "lost": 0, "state": "known"},
        "cells": {"lines": known(since_init=True, covers_since=T0)},
        "row": {"channel": channel, "lines": [dict(ln) for ln in lines]},
        "shared": {"shape_version": 1, "channels": ["info", "lifecycle", "progress"],
                   "line_capacity": 64, "window_hours": 1},
        "shared_cells": {"shape_version": known()},
    }
    if subscribes is not None:
        frame["subscribes"] = subscribes
    return frame


def narration_live(channel: str, line: dict, *, at: str = T0) -> dict:
    """One appended line: a `transition` on `lines`, the item alone."""
    return {
        "type": "shape",
        "shape": NARRATION,
        "key": channel,
        "op": "live",
        "at": at,
        "epoch": {"init_at": T0, "reinit": 0, "lost": 0, "state": "known"},
        "event": {"kind": "transition", "cell": "lines", "item": dict(line)},
    }


# ═══════════════════════════════════════════════════════════════════════════
# `accounts/gauge-history` — an account's week of readings (R2.S3d)
# ═══════════════════════════════════════════════════════════════════════════

GAUGE_HISTORY = "accounts/gauge-history"


def history_reading(at: str, pct: float, axis: str = "weekly_all") -> dict:
    """One retained reading, exactly as `Sequence::to_json_retained` stamps it."""
    return {"axis": axis, "pct": pct, "source": "usage_poll", "at": at}


def history_export(
    account: str, readings: list[dict], *, as_of: str = T0, complete: bool = True,
    subscribes: list[int] | None = None,
) -> dict:
    """The export of one account's week: `row` is `{account, readings}`."""
    frame = {
        "type": "shape",
        "shape": GAUGE_HISTORY,
        "key": account,
        "op": "read",
        "as_of": as_of,
        "epoch": {"init_at": T0, "reinit": 0, "lost": 0, "state": "known"},
        "cells": {"readings": known(complete=complete)},
        "row": {"account": account, "readings": [dict(r) for r in readings]},
        "shared": {"shape_version": 1,
                   "window": {"days": 7, "per_axis": 5040,
                              "axes": ["session", "weekly_all", "weekly_fable"]}},
        "shared_cells": {"shape_version": known(), "window": known()},
    }
    if subscribes is not None:
        frame["subscribes"] = subscribes
    return frame


def history_refused(
    account: str, *, as_of: str = T0, subscribes: list[int] | None = None,
) -> dict:
    """What the real hub exports for a key `hydrates_key` refused: keyed, and
    NO `row` and NO `cells` — both `None` on `ShapeWire`, so both absent."""
    frame = {
        "type": "shape",
        "shape": GAUGE_HISTORY,
        "key": account,
        "op": "read",
        "as_of": as_of,
        "epoch": {"init_at": T0, "reinit": 0, "lost": 0, "state": "known"},
        "shared": {"shape_version": 1,
                   "window": {"days": 7, "per_axis": 5040,
                              "axes": ["session", "weekly_all", "weekly_fable"]}},
        "shared_cells": {"shape_version": known(), "window": known()},
    }
    if subscribes is not None:
        frame["subscribes"] = subscribes
    return frame


def history_live(account: str, reading: dict, *, at: str = T0) -> dict:
    """One recorded reading: a `transition` on `readings`, carrying its own `at`."""
    return {
        "type": "shape",
        "shape": GAUGE_HISTORY,
        "key": account,
        "op": "live",
        "at": at,
        "epoch": {"init_at": T0, "reinit": 0, "lost": 0, "state": "known"},
        "event": {"kind": "transition", "cell": "readings", "item": dict(reading)},
    }
