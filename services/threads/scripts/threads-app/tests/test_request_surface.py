"""**EVERY METHOD THIS BOARD CAN ASK THE DAEMON, PINNED (R2.S3b).**

> *"We should not have new paths to request data... We should remove the
> ability to request data."*

The board learns facts off the shapes. What it may still SEND is a command and
the three `shapes/*` (the chassis's own door). **NO READ IS LEFT.**

* `gauge/history` was the last one — the 7-day sparkline's series, which no
  shape carried. R2.S3d gave it a carrier, `accounts/gauge-history`, and the
  board draws the sparkline off that fold, so the read left this set and the
  pinned call count fell 15 -> 14 (the `Api.gauge_history` call site).

A SOURCE scan, so it runs on every `pytest`. Four trip-wires, each with a
positive sentinel over its own predicate:

1. every `.request(` call is in `api.py`, its first argument resolves to a
   `caps` constant (the probe loop's variable resolves only as the target of
   `for <name> in caps.PROBE_ORDER`), and the resolved set is pinned;
2. no `getattr`/`__getattribute__`/`attrgetter` names `"request"`;
3. exactly ONE `{"type": "request"}` frame literal exists, and it is
   `api.py`'s sender;
4. the `.request(` call count is pinned EXACTLY — a lost or added call is red.
"""

from __future__ import annotations

import ast
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from threads_tui import caps  # noqa: E402

PKG = ROOT / "threads_tui"

#: Reads no shape carries. EMPTY since R2.S3d — `gauge/history` became the
#: `accounts/gauge-history` shape. A name added here is a request path widened.
UNSHAPED_READS: set[str] = set()
#: The chassis's own door. Not a request for data.
SHAPE_DOOR = {"shapes/list", "shapes/{shape}/subscribe", "shapes/{shape}/unsubscribe"}
#: Commands: calling them IS the operation.
COMMANDS = {
    "accounts/{account}/delete",
    "accounts/{account}/disable",
    "accounts/{account}/enable",
    "accounts/{account}/login_restored",
    "accounts/{account}/pool_share_set",
    "accounts/{account}/rename",
    "accounts/rotate",
    "accounts/login/begin",
    "accounts/login/{session_id}/complete",
    "daemon/restart",
}
PINNED = UNSHAPED_READS | SHAPE_DOOR | COMMANDS

#: MEASURED, not a floor. Updated deliberately when a call is removed or added.
REQUEST_CALLS = 14


def _modules() -> list[tuple[pathlib.Path, ast.Module]]:
    return [(p, ast.parse(p.read_text(), filename=str(p))) for p in sorted(PKG.glob("*.py"))]


def _caps_name(node: ast.AST) -> str | None:
    """`caps.X` -> "X"; `caps.fill(caps.X, ...)` -> "X"; anything else None."""
    if isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name) and node.value.id == "caps":
        return node.attr
    if (
        isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and node.func.attr == "fill"
        and node.args
    ):
        return _caps_name(node.args[0])
    return None


def _probe_loop_targets(fn: ast.AST) -> set[str]:
    """Names bound by `for <name> in caps.PROBE_ORDER` inside `fn`, and nothing else."""
    out: set[str] = set()
    for node in ast.walk(fn):
        if (
            isinstance(node, (ast.For, ast.AsyncFor))
            and isinstance(node.target, ast.Name)
            and isinstance(node.iter, ast.Attribute)
            and isinstance(node.iter.value, ast.Name)
            and node.iter.value.id == "caps"
            and node.iter.attr == "PROBE_ORDER"
        ):
            out.add(node.target.id)
    return out


def request_surface() -> tuple[set[str], list[str], int, dict[str, set[str]]]:
    """(methods resolved, offenders, calls seen, {method: api.py functions sending it})."""
    methods: set[str] = set()
    offenders: list[str] = []
    senders: dict[str, set[str]] = {}
    calls = 0
    for path, tree in _modules():
        for fn in ast.walk(tree):
            if not isinstance(fn, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            loop_names = _probe_loop_targets(fn)
            for node in ast.walk(fn):
                if not (
                    isinstance(node, ast.Call)
                    and isinstance(node.func, ast.Attribute)
                    and node.func.attr == "request"
                ):
                    continue
                calls += 1
                where = f"{path.name}:{node.lineno}"
                if path.name != "api.py":
                    offenders.append(f"{where} asks the daemon from outside api.py")
                    continue
                first = node.args[0] if node.args else None
                if isinstance(first, ast.Name) and first.id in loop_names:
                    resolved = list(caps.PROBE_ORDER)
                else:
                    name = _caps_name(first) if first is not None else None
                    if name is None or not isinstance(getattr(caps, name, None), str):
                        offenders.append(f"{where} requests something that is not a caps constant")
                        continue
                    resolved = [getattr(caps, name)]
                for m in resolved:
                    methods.add(m)
                    if fn.name != "_probe_capabilities":
                        senders.setdefault(m, set()).add(fn.name)
    return methods, offenders, calls, senders


def getattr_request_sites(tree: ast.Module, label: str) -> list[str]:
    """`getattr(x, "request")`, `x.__getattribute__("request")`, `attrgetter("request")`."""
    hits = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        f = node.func
        name = f.id if isinstance(f, ast.Name) else f.attr if isinstance(f, ast.Attribute) else None
        if name not in ("getattr", "__getattribute__", "attrgetter"):
            continue
        if any(isinstance(a, ast.Constant) and a.value == "request" for a in node.args):
            hits.append(f"{label}:{node.lineno}")
    return hits


def request_frame_sites(tree: ast.Module, label: str) -> list[str]:
    """A dict literal `{"type": "request", ...}`, or a string holding one."""
    hits = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Dict):
            for k, v in zip(node.keys, node.values):
                if (
                    isinstance(k, ast.Constant) and k.value == "type"
                    and isinstance(v, ast.Constant) and v.value == "request"
                ):
                    hits.append(f"{label}:{node.lineno}")
        elif isinstance(node, ast.Constant) and isinstance(node.value, str):
            compact = node.value.replace(" ", "")
            if '"type":"request"' in compact or "'type':'request'" in compact:
                hits.append(f"{label}:{node.lineno}")
    return hits


# THE SENTINEL FIXTURE for `getattr_request_sites`: never called, only parsed.
def _sentinel_fixture(o):  # pragma: no cover
    return getattr(o, "request")


def test_the_request_call_count_is_exactly_the_measured_one() -> None:
    methods, _, calls, _ = request_surface()
    assert calls == REQUEST_CALLS, (
        f"{calls} `.request(` calls, pinned at {REQUEST_CALLS}: a call was lost or "
        "added — update the pin deliberately"
    )
    # POSITIVE SENTINEL: the scanner resolves the door and a command.
    assert "shapes/list" in methods and "daemon/restart" in methods, sorted(methods)


def test_every_request_the_board_can_make_is_pinned() -> None:
    methods, offenders, _, _ = request_surface()
    assert offenders == [], offenders
    unpinned = sorted(methods - PINNED)
    assert unpinned == [], (
        f"the board can ask the daemon {unpinned}, which no list here admits. A "
        "fact goes on a shape; a read with no carrier is a FINDING, not a method "
        "added to this set"
    )


def test_the_pinned_reads_are_called_by_something_outside_api_py() -> None:
    """A pinned read whose `Api` wrapper nothing calls is a stale exemption.

    Vacuous since R2.S3d, and said so rather than left to pass quietly: there
    is no pinned read left. The emptiness itself is pinned in
    `test_no_read_is_left_to_ask`."""
    _, _, _, senders = request_surface()
    callers: set[str] = set()
    for path, tree in _modules():
        if path.name == "api.py":
            continue
        for node in ast.walk(tree):
            if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute):
                callers.add(node.func.attr)
    for read in sorted(UNSHAPED_READS):
        wrappers = senders.get(read, set())
        assert wrappers, f"`{read}` is pinned and no Api method sends it"
        assert wrappers & callers, f"`{read}` is sent by {sorted(wrappers)}, which nothing calls"


def test_no_read_is_left_to_ask() -> None:
    """R2.S3d: `gauge/history`, the last read, has a carrier. Every method the
    board can send is a command or the shape door, and none is a read."""
    methods, _, _, _ = request_surface()
    assert UNSHAPED_READS == set(), f"a read was re-admitted: {UNSHAPED_READS}"
    reads = sorted(methods - SHAPE_DOOR - COMMANDS)
    assert reads == [], f"the board can still READ {reads} off the daemon"


def test_no_getattr_reaches_request() -> None:
    hits = [h for path, tree in _modules() for h in getattr_request_sites(tree, path.name)]
    assert hits == [], f"`request` reached by name, which the call scan cannot see: {hits}"
    own = ast.parse(pathlib.Path(__file__).read_text())
    assert getattr_request_sites(own, "self"), "the getattr sweep cannot see its own fixture"


def test_exactly_one_site_builds_a_request_frame_and_it_is_api_py() -> None:
    hits = [h for path, tree in _modules() for h in request_frame_sites(tree, path.name)]
    assert len(hits) >= 1, "the frame sweep sees nothing — not even api.py's sender"
    assert len(hits) == 1 and hits[0].startswith("api.py:"), (
        f"request frames are built at {hits}; only api.py's sender may build one"
    )
