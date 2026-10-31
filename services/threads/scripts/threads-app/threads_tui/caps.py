"""The method names this app is built against, and what to do when one is absent.

WHY THIS MODULE EXISTS
----------------------
This app was written against a set of daemon methods that a sibling agent was
building at the same time. Some of them landed; some had not landed yet; some
may land under a different shape. The rule the app follows is:

    Code against the agreed name. If it is not there, say so on the surface
    and disable exactly the affordance that needed it. Never substitute a
    local computation, a SQLite read, or a shell-out for a missing method.

That rule is obligation 2 from `threads_tui.drag`, and it is the reason this
file is a REGISTRY rather than a pile of try/except. A missing method degrades
one panel to an honest "not available yet" message; it never degrades the app
into a second, quieter implementation of the daemon.

HOW ABSENCE IS DETECTED
-----------------------
The daemon answers an unrecognised method with an error whose text begins
`unknown method:` (lib/threads/src/server.rs). That is the ONLY signal treated
as absence. Any other error is a real failure of a real method and is shown as
one — conflating the two would let a transient daemon fault silently look like
a missing feature and permanently grey out a working button.

HOW PROBING IS DONE — AND THE ONE THING WE REFUSE TO DO
------------------------------------------------------
READ methods are probed by calling them at startup. They have no side effects,
so a probe costs one round trip and nothing else.

WRITE methods are NEVER probed. There is a tempting trick — call the method
with deliberately invalid params and read the error to tell "unknown method"
apart from "bad params", proving existence without performing the operation.
We do not do it. A method whose params are all optional would EXECUTE instead
of rejecting, and `accounts/rotate` with no arguments is exactly the shape that
could rotate the whole pool. Discovering a write capability by firing it is not
a probe; it is the operation. Write methods are therefore discovered ON USE,
and the failure surfaces as a clear message at the moment the operator asked
for it.

A METHOD NAME IS A PATH, AND SOME PATHS HAVE A HOLE IN THEM
-----------------------------------------------------------
Since 2026-09-28 every daemon method is `family[/{subject}]/leaf` — the key a
call is ABOUT rides the PATH, and sending it in the params as well is REFUSED
rather than reconciled, because a precedence rule is a place for the two to
disagree. So the names below are of two sorts:

    a LITERAL    `shapes/list`, `gauge/probe` — the whole name, sent as is.
    a TEMPLATE   `accounts/{account}/disable` — a name with a hole, filled by
                 `fill()` at the call site.

**THE TEMPLATE IS THE CAPABILITY'S IDENTITY AND THE FILLED PATH IS ONLY THE
WIRE'S.** `accounts/aa/disable` and `accounts/bb/disable` are one capability:
whether this daemon can take an account out of the pool is not a question about
which account. So the registry is keyed on the TEMPLATE throughout, and every
call site that fills one passes the template alongside it (`Api.request`'s
`cap` argument). Keying on the filled path instead would give every account its
own capability entry, and `note()` would then miss the carefully-worded absent
note for the generic "not available" fallback — in the one module whose entire
job is to say what is missing and what it costs.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum

UNKNOWN_METHOD_PREFIX = "unknown method:"


def is_absence(error_text: str) -> bool:
    """True only for the daemon's own unknown-method refusal.

    The TEXT is the signal and not the `code`, deliberately. The chassis door
    (`:3653`) types this refusal as `code: "unknown_method"`, but the legacy
    door this board talks to answers the same sentence UNTYPED — verified
    against the live daemon, where every `outcome.code` is absent on `:3651`.
    A client that branched on the code would read every absence as a present
    method answering strangely.
    """
    return error_text.strip().lower().startswith(UNKNOWN_METHOD_PREFIX)


def fill(template: str, **subject: str) -> str:
    """A template plus its subject, giving the path to send.

    Strict on purpose. An unfilled hole would be sent literally — the daemon
    would answer `unknown method: accounts/{account}/disable`, `is_absence`
    would match it, and the board would record a WORKING capability as ABSENT
    and grey the key out until the next reconnect. A typo in the hole's name is
    the same failure. So both are an error here, one call before the wire.
    """
    out = template
    for name, value in subject.items():
        hole = "{" + name + "}"
        if hole not in out:
            raise KeyError(f"{template!r} has no {hole} to fill")
        if not value:
            raise ValueError(f"{template!r}: {name} is the subject and cannot be empty")
        out = out.replace(hole, value)
    if "{" in out:
        raise KeyError(f"{template!r} still has an unfilled subject: {out!r}")
    return out


class Kind(Enum):
    READ = "read"  # safe to probe: no side effects
    WRITE = "write"  # never probed: calling it IS the operation


class State(Enum):
    UNKNOWN = "unknown"  # not yet probed / not yet used
    PRESENT = "present"  # answered something other than unknown-method
    ABSENT = "absent"  # answered unknown-method


# ── the method names ─────────────────────────────────────────────────────────
#
# Confirmed present in the daemon's pinned inventory (`server.rs::WS_METHODS`)
# at the time of writing:
# `ACCOUNTS_LIST = "accounts/list"` STOOD HERE (R2.S3b). No `Api` method sent
# it since S3; the roster is the `accounts` shape.
ACCOUNTS_LOGIN_RESTORED = "accounts/{account}/login_restored"
# `GAUGE_PROBE = "gauge/probe"` STOOD HERE. The board has no poll key and no
# `Api.gauge_probe`, so it names no method that asks an account for a reading.
# The daemon still answers `gauge/probe`; this client simply never calls it.

# ── SIX NAMES ARE DELETED HERE (S3), AND THE DAEMON STILL ANSWERS ALL OF THEM
#
#   accounts/status   accounts/health   accounts/pressure
#   accounts/pool_share_get   gauge/status   threads/{thread_id}/get
#
# They are deleted from THE BOARD, not from the wire. The daemon keeps every
# one of them: AUI calls `accounts/list` and `accounts/status`, the CLI uses
# them, and removing them from the daemon would be a different change with a
# different blast radius. What is gone is this client's ability to ask — the
# five-query board reload and the per-thread placement lookup — because while
# a second way to learn a fact existed, a disagreement between the board and
# the daemon could not be attributed to either side.
#
# THEY WERE ALSO THE EXPENSIVE HALF OF STARTUP. `PROBE_ORDER` walks its methods
# SERIALLY on every connection, and five of the nine entries were board queries
# whose payloads the probe throws away — `accounts/health` alone is ~520 ms
# measured, `accounts/status` ~265 ms with an N+1 over `thread_accounts`. A
# probe is a round trip that exists to learn whether a method is THERE, and a
# method this client never calls does not need to be there.

# Agreed with the sibling agent building the threads side. Coded against by
# name; each is discovered at runtime.
# NOTE: the getter was agreed as `accounts.pool_share`, landed as
# `accounts.pool_share.get`, and is now `accounts/pool_share_get` — three
# spellings for one method, which is why the board codes against the name it
# actually sees rather than the name it was promised. It no longer
# reads it (S3 — shares arrive on the shape's `share` cell); the SETTER landed
# exactly as agreed and stays, because writing a share is an operation and not
# a fact.
#
# ITS `account` MOVED INTO THE PATH AND THE GETTER'S DID NOT, and the rule
# behind that split is worth keeping: REQUIRED goes in the path, OPTIONAL stays
# a parameter. Setting a share is about exactly one account, so the account is
# the subject; the getter answers for one account or for all of them, so its
# `account` is a FILTER and stays where it was.
ACCOUNTS_POOL_SHARE_SET = "accounts/{account}/pool_share_set"
# DISABLE AN ACCOUNT, AND PUT IT BACK. Named for the ACT, not for the field it
# writes. They are not a second mechanism — the daemon still has exactly one
# thing that takes an account out of placement, a share of zero — but the act
# had no name, and an interface cannot offer "disable this account" over a
# method called "set the pool share" without deciding, client-side, what a zero
# means to the daemon.
ACCOUNTS_DISABLE = "accounts/{account}/disable"
ACCOUNTS_ENABLE = "accounts/{account}/enable"
# `GAUGE_HISTORY` ("gauge/history") STOOD HERE (R2.S3d). It was the board's
# last READ: the 7-day sparkline's series, which no shape carried. One does now
# — `accounts/gauge-history` (`shapes.GAUGE_HISTORY`), subscribed per account —
# so the constant is gone, and with it the probe and the ability to ask.
DAEMON_RESTART = "daemon/restart"
# ── THE LOGIN PAIR, AND THE ONE PLACE THIS MIGRATION IS NOT A RENAME ─────────
#
# `begin` takes a LABEL and is not keyed on one: it is handed an account that
# may not exist yet (`n` creates a slot by signing it in), so there is nothing
# for the path to name and `account` stays a parameter. Verified on the live
# daemon, which answers `missing required param: account` to a bare
# `accounts/login/begin`.
#
# `complete` IS keyed, on the SESSION — and the `account` fallback it used to
# accept was DELETED, not renamed. Two spellings of one subject, resolved by a
# lookup against the in-flight logins, is exactly the disagreement the path
# move exists to remove. So the board must now carry `session_id` from
# `begin`'s reply to `complete`'s path; passing the label is no longer a thing
# the wire can resolve. See `Api.login_complete` and `app.do_login`.
LOGIN_BEGIN = "accounts/login/begin"
LOGIN_COMPLETE = "accounts/login/{session_id}/complete"
ACCOUNTS_RENAME = "accounts/{account}/rename"
ACCOUNTS_DELETE = "accounts/{account}/delete"
# NOT keyed: there is no per-account rotation and never was — the daemon reads
# `dry_run` and walks the whole registry. A pool-wide act has no subject.
ACCOUNTS_ROTATE = "accounts/rotate"

# ── SUBSCRIPTION WEBSOCKET SHAPES ────────────────────────────────────────────
#
# `shapes/list` is a READ and is probed: it reads each shape task's published
# `ShapeInfo` watch and touches no cell and no connection state, which is
# exactly why it can live in the daemon's `dispatch` while the other two
# cannot. Its ABSENCE is the decisive one — a daemon that answers
# `unknown method: shapes/list` holds no fold, `is_absence` matches it, and
# since S3 the board has no second way to draw itself (`render.no_board_lines`).
#
# **THIS IS THE ONE THAT WAS BROKEN, AND IT WAS BROKEN BY BEING RIGHT ONCE.**
# `shapes/list` was a real method and became `shapes/list` on 2026-09-28. The
# board kept asking for the old name, the daemon answered `unknown method:` as
# it does for any name it does not carry, `is_absence` did its job correctly,
# and the whole board drew `NO FOLD ON THIS DAEMON` against a daemon holding
# six folds. The failure was indistinguishable from the one honest case this
# constant exists to detect, which is why nothing louder happened.
SHAPES_LIST = "shapes/list"
# WRITE, and NEVER probed — the side effect IS the subscription. A probe would
# hydrate a lazy shape (four `security` invocations and ~0.8s of store reads on
# a machine nobody is watching) and would register this connection for frames
# nothing had asked for. That is the module docstring's rule applied to a
# method whose params are all optional: calling it is the operation.
#
# THE SHAPE IS THE SUBJECT, so it rides the path and leaves the params: a
# `shapes/accounts/subscribe` carrying `{"shape": "accounts"}` is REFUSED, and
# refused identically whether the two agree or disagree. A shape name holding a
# `/` needs no escaping — `shapes/threads/summary/subscribe` names
# `threads/summary`, because the parse takes the leaf off the end and the
# longest known family off the front.
SHAPES_SUBSCRIBE = "shapes/{shape}/subscribe"
SHAPES_UNSUBSCRIBE = "shapes/{shape}/unsubscribe"


@dataclass(frozen=True)
class Capability:
    method: str
    kind: Kind
    surface: str  # what the operator loses without it
    absent_note: str  # shown verbatim on that surface


CAPABILITIES: dict[str, Capability] = {
    c.method: c
    for c in [
        Capability(
            ACCOUNTS_POOL_SHARE_SET,
            Kind.WRITE,
            "the share editor",
            "The daemon does not expose a pool-share setter yet, so the share "
            "cannot be changed from here.",
        ),
        # THE FALLBACK THESE TWO NOTES USED TO NAME NO LONGER EXISTS. Both said
        # "press S and enter 0, which is the same act through the number" — true
        # while a share of zero WAS the disable. Membership and allocation are
        # separate now: the daemon refuses a zero share and names this verb in
        # its refusal, and `share.parse_share` refuses it one round trip earlier.
        # An absent-note naming a shut door is worse than one naming none,
        # because he spends the keystrokes before he finds out. So they name the
        # CLI, which is a door that is actually open on such a daemon.
        Capability(
            ACCOUNTS_DISABLE,
            Kind.WRITE,
            "take an account out of the pool",
            "This daemon does not expose accounts/{account}/disable yet, so an "
            "account "
            "cannot be taken out of the pool at all — not from here and not "
            "from the CLI, which has no verb for it either. The share editor is "
            "NOT a way round it: membership and share are separate questions "
            "now, and a share of 0 is refused by both ends. Promote a daemon "
            "that carries it and this key works.",
        ),
        Capability(
            ACCOUNTS_ENABLE,
            Kind.WRITE,
            "put an account back in the pool",
            "This daemon does not expose accounts/{account}/enable yet, so an "
            "account it "
            "has already taken out of the pool cannot be put back — there is no "
            "CLI verb for this one either. Its share is untouched meanwhile. "
            "Promote a daemon that carries it and this key works.",
        ),
        Capability(
            DAEMON_RESTART,
            Kind.WRITE,
            "restart from the board",
            "The daemon does not expose a restart operation.",
        ),
        Capability(
            LOGIN_BEGIN,
            Kind.WRITE,
            "the in-app login",
            "accounts/login/begin is not on the wire yet. Login falls back to the "
            "terminal handoff, which is fully supported.",
        ),
        Capability(
            LOGIN_COMPLETE,
            Kind.WRITE,
            "the in-app login",
            "accounts/login/{session_id}/complete is not on the wire yet.",
        ),
        Capability(
            ACCOUNTS_RENAME,
            Kind.WRITE,
            "rename",
            "accounts/{account}/rename is not on the wire yet.",
        ),
        Capability(
            ACCOUNTS_DELETE,
            Kind.WRITE,
            "delete",
            "accounts/{account}/delete is not on the wire yet.",
        ),
        Capability(
            ACCOUNTS_ROTATE,
            Kind.WRITE,
            "rotate",
            "accounts/rotate is not on the wire yet.",
        ),
        Capability(
            SHAPES_LIST,
            Kind.READ,
            "THE BOARD",
            # **THE SURFACE IS THE WHOLE BOARD NOW, AND THE NOTE SAYS SO.**
            # Before S3 this read "nothing is degraded" and that was true: the
            # five queries drew the board and the shape path was an upgrade.
            # The queries are gone, so the same sentence would now be a lie of
            # exactly the kind this registry exists to forbid — a capability
            # note that understates what its absence costs is worse than none,
            # because it is read at the moment the operator is trying to work
            # out why his screen is empty.
            "shapes/list is not on the wire. This daemon holds no fold for the "
            "board to mirror, and since stage S3 the board has no other way to "
            "draw itself — the five account queries it used to join "
            "(accounts/status, gauge/status, accounts/health, "
            "accounts/pressure, accounts/pool_share_get) were deleted on "
            "purpose, so that a wrong number is a bug in one named place "
            "rather than a disagreement between two paths that cannot be "
            "attributed to either. The daemon still answers all five for AUI "
            "and the CLI; this board simply no longer asks. Promote a daemon "
            "that carries the fold and the board fills in by itself.",
        ),
        Capability(
            SHAPES_SUBSCRIBE,
            Kind.WRITE,
            "THE BOARD",
            "shapes/{shape}/subscribe is not on the wire, so no fold can be "
            "mirrored "
            "and there is nothing for this board to draw. It is the only way "
            "the board learns anything; see the note on shapes/list.",
        ),
        Capability(
            SHAPES_UNSUBSCRIBE,
            Kind.WRITE,
            "releasing a shape subscription",
            "shapes/{shape}/unsubscribe is not on the wire. A subscription is "
            "released "
            "when the connection closes, which the daemon does for itself.",
        ),
    ]
}

# ── PROBED AT STARTUP, IN THIS ORDER. TWO ENTRIES, DOWN FROM NINE (S3). ─────
#
# READ only — see the module docstring. The probe is SERIAL, on every
# connection, in front of the first paint, so what is in this list is a cost
# the operator pays before he sees anything.
#
# WHAT LEFT, AND THE RULE THAT REMOVED IT: **if the board does not call a
# method, it does not probe it.** A probe exists to answer "can I call this",
# and asking that about a method with no call site is a round trip that can
# change nothing on screen.
#
#   accounts/status, gauge/status, accounts/health, accounts/pressure,
#   accounts/pool_share_get  — the five board queries. S3 deleted the reload
#     that called them, so their capability state had no reader left. They were
#     also the slow ones: ~520 ms for health and ~265 ms for status, measured,
#     every connection, with the payloads discarded.
#
#   daemon/status, daemon/image  — these had NO call site even before S3. They
#     were probed and nothing ever asked the registry about them, so the result
#     was recorded and never read. Removed on the same rule rather than left
#     because they are cheap; a probe nobody reads is the kind of thing that
#     survives by being small.
#
# `threads/{thread_id}/get` was never here, for a reason worth keeping: it
# needs a
# `thread_id`, and with the subject in the PATH a probe could not even be
# SPELLED without inventing one — it would send `threads/x/get` about a thread
# that does not exist, on every connect. It is now gone from the file entirely, along with the placement
# lookup that was its only caller.
PROBE_ORDER = [
    # `GAUGE_HISTORY` was probed FIRST here, as "the board's ONE remaining
    # read". It is not a read any more (R2.S3d): the series is the
    # `accounts/gauge-history` shape, and a shape is not probed — subscribing
    # it IS the operation. The board now probes nothing it would read from.
    #
    # LAST, deliberately, and now it is also the decisive one. A daemon that
    # refuses this holds no fold, and since S3 that means there is no board to
    # draw — `render.no_board_lines` says so outright. Probing it last still
    # matters for the other direction: on a daemon that HAS folds, the keyless
    # subscribe that follows this answer is what hydrates them, and putting a
    # slower probe in front of it would delay the first real paint.
    SHAPES_LIST,
]


@dataclass
class Registry:
    """What the daemon on the other end of this socket actually supports.

    Reset whenever the connection is replaced: a reconnect may land on a daemon
    of a different version, and a capability map cached across a restart is a
    map of a daemon that is no longer there.
    """

    states: dict[str, State] = field(default_factory=dict)

    def reset(self) -> None:
        self.states.clear()

    def state(self, method: str) -> State:
        return self.states.get(method, State.UNKNOWN)

    def has(self, method: str) -> bool:
        """True only when the method is known-present. UNKNOWN is not True.

        Read affordances gate on this, so an unprobed read stays dark rather
        than rendering a panel that is about to fail.
        """
        return self.state(method) is State.PRESENT

    def usable(self, method: str) -> bool:
        """True unless the method is known-absent. UNKNOWN is not False.

        Write affordances gate on this. A write is never probed, so its state
        is UNKNOWN until first use; gating writes on `has()` would grey out
        every button forever.
        """
        return self.state(method) is not State.ABSENT

    def record(self, method: str, present: bool) -> None:
        self.states[method] = State.PRESENT if present else State.ABSENT

    def record_from_error(self, method: str, error_text: str) -> bool:
        """Fold a failed call into the registry. Returns True if it was absence."""
        absent = is_absence(error_text)
        if absent:
            self.states[method] = State.ABSENT
        return absent

    def note(self, method: str) -> str:
        cap = CAPABILITIES.get(method)
        return cap.absent_note if cap else f"{method} is not available."

    def missing(self) -> list[str]:
        return sorted(m for m, s in self.states.items() if s is State.ABSENT)
