"""**THE TEST THAT WOULD HAVE CAUGHT THE RENAME, AND DID NOT EXIST.**

On 2026-09-28 every one of the daemon's fifty methods became a path:
`shapes.list` became `shapes/list`, `accounts.disable` became
`accounts/{account}/disable`. The board kept asking for the old names. The
daemon answered `unknown method:` — which is the ONE signal `caps.is_absence`
treats as "this daemon does not carry this method" — so the board recorded
`shapes.list` ABSENT, drew `NO FOLD ON THIS DAEMON`, and explained at length
that the operator should promote a daemon carrying the fold.

The daemon was holding six folds at the time.

**NOTHING IN THE SUITE COULD TELL.** Every socket test drives a FAKE daemon
that answers whatever the board asks, so the fake and the board were renamed
together and agreed with each other perfectly. The one thing neither of them
is, is the producer. So this file does not ask the fake anything: it reads
`lib/threads/src/server.rs::WS_METHODS` — the daemon's own pinned inventory,
the list its router routes against — and checks the board's names against it.

It is a SOURCE read and not a socket read, deliberately. A test that needs the
operator's daemon running is a test that gets skipped, and this one has to run
on every `pytest tests/` or it is not a guard. The inventory is a `const` in a
file in this repo; reading it costs nothing and cannot be unavailable.

**WHAT IT CANNOT CATCH**, so nobody trusts it too far: it proves a name is one
the daemon ROUTES, not that the call's params are right or that the reply is
the shape the board expects. Those are the live daemon's to answer, and
`smoke.py` is where they are asked.
"""

from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(ROOT / "tests"))

import fake_daemon  # noqa: E402

from threads_tui import caps  # noqa: E402

# threads-app -> scripts -> threads
SERVER_RS = ROOT.parent.parent / "src" / "server.rs"


def daemon_inventory() -> list[str]:
    """`WS_METHODS` out of the daemon's own source.

    Parsed rather than copied. A copy in this file would be a second source
    for the one thing this test exists to compare against, and it would go
    stale in exactly the way the board did.
    """
    text = SERVER_RS.read_text()
    match = re.search(r"pub const WS_METHODS: &\[&str\] = &\[(.*?)\];", text, re.S)
    assert match, f"WS_METHODS not found in {SERVER_RS}"
    return re.findall(r'"([^"]+)"', match.group(1))


def test_the_daemons_inventory_is_readable_and_is_all_paths() -> None:
    """The parse works, and nothing in it is still a dotted name.

    This is the precondition for every other check here. If the regex stops
    matching, the checks below would pass vacuously against an empty list —
    which is the failure mode this whole file exists to object to.
    """
    inventory = daemon_inventory()
    assert len(inventory) > 40, f"only parsed {len(inventory)} methods: {inventory[:5]}"
    dotted = [m for m in inventory if "." in m]
    assert dotted == [], f"the daemon still carries dotted names: {dotted}"
    assert "shapes/list" in inventory


def test_every_method_the_board_names_is_one_the_daemon_routes() -> None:
    """**THE GUARD.** Every `caps` constant is in the daemon's inventory.

    THE MUTATION: set `caps.SHAPES_LIST = "shapes.list"` and this goes red,
    naming the constant — which is the whole of the bug this file was written
    after, caught before a socket is opened.

    Note that the constants are TEMPLATES and the inventory is templates too,
    so this is an exact comparison and not a routing one. That is the stronger
    check: it pins the board's idea of where the SUBJECT goes, not just the
    family and the leaf. A board that thought `accounts/rotate` took an
    account would name `accounts/{account}/rotate` here and be caught, even
    though both route to something on a real daemon.
    """
    inventory = set(daemon_inventory())
    # EVERY upper-case string in the module except the one that is not a
    # method name. The first draft of this selected on `"/" in value`, and
    # that was the same bug in miniature: a constant reverted to
    # `"shapes.list"` has no `/`, so it was SKIPPED by the check written to
    # catch it. A filter that excludes the thing it is looking for is worse
    # than no filter, so the selection is by name and the count is pinned.
    named = {
        name: value
        for name, value in vars(caps).items()
        if name.isupper() and isinstance(value, str) and name != "UNKNOWN_METHOD_PREFIX"
    }
    # 18 BECAME 17 ON 2026-10-06, when `GAUGE_PROBE` was deleted with the `R`
    # key. The pinned count is the point of this assertion — it is what makes a
    # NEW constant impossible to add without being checked here — so it is
    # updated deliberately rather than loosened to `>=`.
    # 17 BECAME 14 IN R2.S3b: `ACCOUNTS_LIST`, `DAEMON_STATUS` and `DAEMON_IMAGE`
    # named reads the board no longer sends (see `test_request_surface.py`).
    # 14 BECAME 13 IN R2.S3d: `GAUGE_HISTORY`, the last read, has a carrier —
    # the `accounts/gauge-history` shape — and the constant is deleted.
    assert len(named) == 13, (
        f"expected 13 method constants, found {len(named)} — a new one must be "
        f"checked here too: {sorted(named)}"
    )
    unknown = {n: v for n, v in named.items() if v not in inventory}
    assert unknown == {}, (
        "these names are not methods this daemon carries, so every call to "
        f"them answers `unknown method:` and is recorded as ABSENT: {unknown}"
    )


def test_the_capability_registry_is_keyed_on_names_the_daemon_carries() -> None:
    """The same check for `CAPABILITIES`, which is keyed separately.

    It is a dict built from `Capability.method`, so a constant could be
    re-pathed while the entry that carries its absent-note was not. The note
    is the only thing the operator ever reads about a missing method; a note
    filed under a name nothing looks up is a note that never appears.
    """
    inventory = set(daemon_inventory())
    orphans = [m for m in caps.CAPABILITIES if m not in inventory]
    assert orphans == [], f"capability notes filed under non-methods: {orphans}"


def test_the_probe_order_only_probes_methods_that_exist() -> None:
    """A probe is a round trip whose whole purpose is to answer *is this
    there*. One that names a method the daemon does not carry answers "no"
    correctly and means nothing — which is precisely how the board came to
    believe a six-fold daemon had none."""
    inventory = set(daemon_inventory())
    assert [m for m in caps.PROBE_ORDER if m not in inventory] == []


def test_the_fakes_inventory_is_the_daemons_inventory() -> None:
    """`fake_daemon.WIRE_METHODS` must not drift from the real list.

    The fakes route against their own copy, so a method the daemon carries and
    the fake does not would be refused in tests and served in production — a
    fake that is WRONGER than the thing it stands in for. Copied rather than
    parsed there on purpose (the fake must not need the Rust tree to import);
    this is the check that makes the copy safe.
    """
    assert list(fake_daemon.WIRE_METHODS) == daemon_inventory()


def test_route_puts_the_subject_where_the_daemon_puts_it() -> None:
    """The fake's router, against the cases that are easy to get wrong."""
    assert fake_daemon.route("shapes/list") == ("shapes/list", {})
    assert fake_daemon.route("shapes/accounts/subscribe") == (
        "shapes/{shape}/subscribe",
        {"shape": "accounts"},
    )
    # A shape name containing a `/` needs no escaping: the leaf comes off the
    # end and the longest known family off the front.
    assert fake_daemon.route("shapes/threads/summary/subscribe") == (
        "shapes/{shape}/subscribe",
        {"shape": "threads/summary"},
    )
    # A sub-family must win over an account literally named `login`.
    assert fake_daemon.route("accounts/login/begin") == ("accounts/login/begin", {})
    assert fake_daemon.route("accounts/login/s-1/complete") == (
        "accounts/login/{session_id}/complete",
        {"session_id": "s-1"},
    )
    # An OMITTED subject is not a half-formed request, it is not a method.
    assert fake_daemon.route("shapes/subscribe") == (None, {})
    assert fake_daemon.route("shapes.list") == (None, {})


def test_sending_the_subject_twice_is_refused_not_reconciled() -> None:
    """The refusal the path move exists to create, in the fake that stands in
    for the daemon. Same refusal whether the two agree or disagree — a
    precedence rule would be a place for them to disagree."""
    import pytest

    template, params = fake_daemon.bind_subject("accounts/aa/disable", {"note": "x"})
    assert template == "accounts/{account}/disable"
    assert params == {"note": "x", "account": "aa"}

    for also in ({"account": "aa"}, {"account": "bb"}):
        with pytest.raises(fake_daemon.SubjectInParams):
            fake_daemon.bind_subject("accounts/aa/disable", also)


def test_fill_refuses_a_hole_it_cannot_fill() -> None:
    """`caps.fill` is strict because the failure it prevents is silent: an
    unfilled hole goes out as a literal `{account}`, the daemon answers
    `unknown method:`, and the board records a WORKING capability as ABSENT."""
    import pytest

    assert caps.fill(caps.ACCOUNTS_DISABLE, account="aa") == "accounts/aa/disable"
    with pytest.raises(KeyError):
        caps.fill(caps.ACCOUNTS_DISABLE, acct="aa")  # wrong hole name
    with pytest.raises(KeyError):
        caps.fill(caps.ACCOUNTS_DISABLE)  # not filled at all
    with pytest.raises(ValueError):
        caps.fill(caps.ACCOUNTS_DISABLE, account="")  # a subject must be a subject
