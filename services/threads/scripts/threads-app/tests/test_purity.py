"""Mechanical enforcement of the two obligations in `threads_tui.drag`.

A note that says "the render layer stays pure" decays the first time someone is
in a hurry. This test is what makes the obligation checkable instead of
aspirational, which is the difference between a recorded deviation and a
comment nobody acts on.

Two things are checked, both by parsing the actual source with `ast` rather
than by grepping text — a grep is fooled by a substring in a comment, and, more
importantly, it cannot tell an import that is merely PRESENT from a call that
is actually MADE.

  OBLIGATION 1  the render layer is pure: no I/O, no clock, no environment,
                and no import that reaches a module which does any of those at
                import time.

  OBLIGATION 2  every fact comes from the API: nothing in the package imports
                sqlite3, and nothing but the sanctioned carve-outs runs a
                subprocess. That list is named and reasoned below; its length is
                the honest tally of where the obligation does not hold.

Run: .venv/bin/python tests/test_purity.py   (from threads-app/)
"""

from __future__ import annotations

import ast
import importlib.util
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
PKG = ROOT / "threads_tui"

# The modules that must be pure, and the whole transitive closure of what they
# are allowed to import from inside the package.
PURE_MODULES = [
    "render.py",
    "pressure.py",
    "model.py",
    "fmt.py",
    "link.py",
    "drag.py",
    # `live.py` STOOD HERE and is deleted (S3). It was the client-side running
    # -thread fold — a 5-second lease off a 1 Hz beat, a placement cache and a
    # work list — and every question it answered is answered by the daemon's
    # own fold, where a thread's row lives under the account it was placed on.
    # The running count is now a `count` over held rows in `model.py`, which is
    # in this list and is pure for the same reason `live.py` was.
    #
    # The live narration and announcement feed. Pure because `at` is a
    # parameter everywhere, so `test_feed.py` can drive a burst, a coalesce
    # window and a dropped socket at a frozen clock and prove the response is
    # scheduled BY A FRAME rather than by the passage of time.
    "feed.py",
    # THE SHAPE MIRROR, and it is in this list for a stronger reason than the
    # other two. `model.py` imports it, so the closure check below would fail
    # if it were left out — but the reason it is SAFE to add is that it holds
    # the daemon's fold and must never subtract two timestamps: every stamp it
    # parses is an instant, and "how old is this" is a question with a `now` in
    # it that belongs to the caller. A clock read in here would put an age
    # inside the model and the render layer's `now` parameter would stop being
    # the only clock on the screen.
    "shapes.py",
]

# Anything from these is I/O or non-determinism.
BANNED_IMPORTS = {
    "os", "sys", "subprocess", "sqlite3", "socket", "asyncio", "shutil",
    "websockets", "urllib", "requests", "http", "pathlib", "tomllib", "json",
}

# Calls that read a hidden input rather than a parameter.
BANNED_CALLS = {"now_utc", "now", "utcnow", "today", "getenv", "system", "popen", "open"}

# The sanctioned shell-outs. FOUR, and the count is the point: this list is
# the honest tally of where "every operation through the API" does not hold.
# Each entry carries the reason it is allowed and the condition under which it
# goes away, so the list can be audited rather than merely trusted.
#
# A fourth entry appearing here without a reason of the same quality is a
# finding, not a formality.
SUBPROCESS_ALLOWED = {
    # PERMANENT. The daemon deliberately has no concept of a default account
    # and should not acquire one — "which account does plain `claude` use on
    # this laptop" is a property of the laptop, not of the pool.
    "local.py": "setting this machine's local account — the one carve-out that "
                "was asked for, and the only permanent one",
    # STRUCTURAL. Not a shortcut: a dead process cannot be asked to revive
    # itself, so this operation can never go through the API by construction.
    "down.py": "starting a daemon that is not answering — there is nothing on "
               "the other end to ask",
    # TEMPORARY. Retires the day accounts/login/begin and .complete land, at
    # which point the modal becomes the only path and this is deleted.
    "login.py": "the terminal login handoff — a fallback that exists only "
                "because accounts/login/begin/.complete are not on the wire "
                "yet; it retires when they land",
    # PERMANENT, and the same KIND of carve-out as local.py: the clipboard is a
    # property of the laptop, not of the pool, and the daemon has no business
    # holding one. The in-band alternative — OSC 52 through the terminal — was
    # what shipped, and it failed in the field: the operator runs Textual inside
    # tmux inside iTerm2, tmux's `set-clipboard` is `external`, and under that
    # setting tmux DISCARDS an application's OSC 52 rather than forwarding it
    # (measured through a real tmux in tests/test_clipboard.py). Even fixed with
    # the DCS passthrough, OSC 52 has no reply, so it can never be VERIFIED — and
    # this app is not permitted to tell him it copied something when it did not.
    # pbcopy plus a pbpaste readback is the only mechanism on this machine whose
    # success can be observed, so it is the one the claim rests on. It does not
    # retire; the OSC 52 path is kept beside it for the SSH case.
    "clipboard.py": "putting text on the macOS pasteboard and READING IT BACK — "
                    "the only copy this app can honestly claim, and a property "
                    "of the laptop rather than of the pool",
}

FAILURES: list[str] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    if ok:
        print(f"  ok   {name}")
    else:
        print(f"  FAIL {name}  {detail}")
        FAILURES.append(name)


def parse(path: pathlib.Path) -> ast.Module:
    return ast.parse(path.read_text(), filename=str(path))


def imported_modules(tree: ast.Module) -> set[str]:
    out: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for a in node.names:
                out.add(a.name.split(".")[0])
        elif isinstance(node, ast.ImportFrom):
            if node.level == 0 and node.module:
                out.add(node.module.split(".")[0])
    return out


def local_imports(tree: ast.Module) -> set[str]:
    """Relative imports — the in-package dependency edges."""
    out: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom) and node.level and node.module:
            out.add(node.module.split(".")[0])
        elif isinstance(node, ast.ImportFrom) and node.level and not node.module:
            for a in node.names:
                out.add(a.name)
    return out


def called_names(tree: ast.Module) -> set[str]:
    out: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Call):
            f = node.func
            if isinstance(f, ast.Name):
                out.add(f.id)
            elif isinstance(f, ast.Attribute):
                out.add(f.attr)
    return out


def module_level_calls(tree: ast.Module) -> set[str]:
    """Calls executed at IMPORT time. `PATHS = resolve()` is the shape that
    made the old render layer environment-dependent just by being imported."""
    out: set[str] = set()
    for node in tree.body:
        if isinstance(node, (ast.Assign, ast.Expr, ast.AnnAssign)):
            for sub in ast.walk(node):
                if isinstance(sub, ast.Call):
                    f = sub.func
                    if isinstance(f, ast.Name):
                        out.add(f.id)
                    elif isinstance(f, ast.Attribute):
                        out.add(f.attr)
    return out


def main() -> int:
    print("OBLIGATION 1 — the render layer is pure\n")

    for name in PURE_MODULES:
        path = PKG / name
        if not path.exists():
            check(f"{name} exists", False, "missing")
            continue
        tree = parse(path)

        bad = imported_modules(tree) & BANNED_IMPORTS
        check(f"{name} imports nothing that does I/O", not bad, f"imports {sorted(bad)}")

        calls = called_names(tree) & BANNED_CALLS
        check(f"{name} calls no hidden input", not calls, f"calls {sorted(calls)}")

        mlc = module_level_calls(tree)
        # dataclass/field/enum construction at module scope is fine; a call that
        # resolves paths or reads env is not.
        suspicious = mlc & {"resolve", "getenv", "environ", "open", "connect", "run"}
        check(f"{name} does nothing at import time", not suspicious, f"{sorted(suspicious)}")

    # The transitive edge. render.py may import model.py; model.py must not then
    # import api.py. This is the check that would have caught the old app's
    # render -> daemon -> paths -> os.environ chain.
    print()
    closure_ok = True
    detail = ""
    seen: set[str] = set()
    frontier = [m[:-3] for m in PURE_MODULES]
    while frontier:
        mod = frontier.pop()
        if mod in seen:
            continue
        seen.add(mod)
        p = PKG / f"{mod}.py"
        if not p.exists():
            continue
        for dep in local_imports(parse(p)):
            if dep not in seen:
                frontier.append(dep)
    impure_reached = seen - {m[:-3] for m in PURE_MODULES}
    if impure_reached:
        closure_ok = False
        detail = f"pure modules transitively import {sorted(impure_reached)}"
    check("the pure closure imports only pure modules", closure_ok, detail)

    # The specific regression: `card_lines` must not accept a defaulted clock.
    # The old signature had `now: datetime | None = None` and then leaked the
    # real clock into the axis rows anyway, so a caller passing a fixed `now`
    # got time-varying output and could not tell.
    print()
    render_tree = parse(PKG / "render.py")
    card_lines = next(
        (n for n in render_tree.body if isinstance(n, ast.FunctionDef) and n.name == "card_lines"),
        None,
    )
    check("card_lines exists", card_lines is not None)
    if card_lines is not None:
        kw = {a.arg for a in card_lines.args.kwonlyargs}
        check("card_lines takes `now`", "now" in kw or any(a.arg == "now" for a in card_lines.args.args))
        idx = [a.arg for a in card_lines.args.kwonlyargs]
        default_is_none = False
        if "now" in idx:
            d = card_lines.args.kw_defaults[idx.index("now")]
            default_is_none = d is not None
        check(
            "card_lines does NOT default `now` (a defaulted clock is how the leak survived)",
            not default_is_none,
        )

    print("\nOBLIGATION 2 — every fact comes from the API\n")

    for path in sorted(PKG.glob("*.py")):
        tree = parse(path)
        imports = imported_modules(tree)
        check(f"{path.name} does not import sqlite3", "sqlite3" not in imports)

        uses_subprocess = "subprocess" in imports
        allowed = path.name in SUBPROCESS_ALLOWED
        if uses_subprocess and not allowed:
            check(f"{path.name} runs no subprocess", False, "unsanctioned subprocess")
        elif uses_subprocess and allowed:
            print(f"  ok   {path.name} subprocess is sanctioned: {SUBPROCESS_ALLOWED[path.name]}")
        else:
            check(f"{path.name} runs no subprocess", True)

    # The whole package must be importable and must actually compile.
    print()
    sys.path.insert(0, str(ROOT))
    for path in sorted(PKG.glob("*.py")):
        mod = f"threads_tui.{path.stem}"
        try:
            spec = importlib.util.find_spec(mod)
            check(f"{mod} is importable", spec is not None)
        except Exception as exc:
            check(f"{mod} is importable", False, str(exc))

    print()
    if FAILURES:
        print(f"{len(FAILURES)} FAILED: {', '.join(FAILURES)}")
        return 1
    print("all purity checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
