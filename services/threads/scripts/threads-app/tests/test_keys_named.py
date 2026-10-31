"""EVERY KEY THIS BOARD TELLS HIM TO PRESS IS A KEY IT ACTUALLY BINDS.

`r` was deleted — the binding, `action_resync`, `action_probe` and `do_refresh`
all went, and `app.BINDINGS` carries a long comment saying why: *"also 'R = poll
accounts' should not just be retired and removed from view, it should be the
forcing function to prevent ever needing this."*

TEN USER-FACING STRINGS WENT ON TELLING HIM TO PRESS IT. Not comments — strings
he reads on screen, at the worst moment in each flow: every mutation whose
`resync` timed out ("the row below is not confirmed current; press r"), the
login verdict, the delete and rename verdicts, and the DEAD-FOLD warning, which
is the single most important line this board can draw and whose advice was
*"press r to fall back to the account queries"* — a deleted key pointing at a
deleted fallback. Pressing `r` on this board does nothing at all, silently.

That is the failure mode the `l`/`L` comment in `app.BINDINGS` calls the worst
case in the whole interface — *"a silent no-op on the key a DEAD account tells
you to press"* — and it had escaped into ten places by being prose rather than
code.

THE SCANNER AND ITS POSITIVE SENTINEL
-------------------------------------
This reads SOURCE, so it is a probe that could trivially pass by seeing nothing:
a broken regex, a renamed package directory or a changed quoting style would all
produce zero findings and a green test forever. The ~25 probes-that-could-not-fail
found in this arc were mostly this shape.

So `test_the_scanner_can_see_a_key_that_is_there` is not decoration and must not
be deleted: it asserts the scanner finds the keys that ARE legitimately named in
strings (`d`, `l`, `s`, `S`, `n`) and that it finds a MINIMUM number of them. If
the scanner stops seeing those, it has stopped scanning, and the absence
assertion below means nothing.
"""

from __future__ import annotations

import ast
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PKG = ROOT / "threads_tui"
sys.path.insert(0, str(ROOT))

# `press d`, `press S`, `press !`. Word-bounded so `press Enter`, `press escape`
# and English like "press instead" are not read as key names.
PRESS = re.compile(r"\bpress(?:ing)? (?:the )?([A-Za-z0-9!])\b")

# Words that follow "press" in ordinary prose and are not keys. Single letters
# only reach the regex, so this is a short list by construction.
NOT_A_KEY = {"a"}


def bound_keys() -> set[str]:
    """The keys `app.py` actually binds, read out of the `Binding(...)` calls.

    Parsed from the AST rather than imported, so this does not need Textual and
    does not care what `BINDINGS` is composed of at runtime. The digit bindings
    are a comprehension and are handled by the `select(n)` arm.
    """
    tree = ast.parse((PKG / "app.py").read_text())
    keys: set[str] = set()
    for node in ast.walk(tree):
        if not (isinstance(node, ast.Call) and isinstance(node.func, ast.Name)):
            continue
        if node.func.id != "Binding" or not node.args:
            continue
        first = node.args[0]
        if isinstance(first, ast.Constant) and isinstance(first.value, str):
            keys.add(first.value)
        else:
            # `Binding(str(n), f"select({n})", ...)` — the 1-9 card selectors.
            keys.update("123456789")
    # Named keys Textual resolves itself; a string may legitimately say these.
    keys.update({"escape", "enter", "up", "down"})
    return keys


def named_keys() -> list[tuple[str, str, int]]:
    """Every `press X` found in a STRING LITERAL. Returns (key, file, line).

    String literals only — a comment that discusses the deleted `r` binding is
    documentation and is the opposite of a defect.
    """
    found: list[tuple[str, str, int]] = []
    for path in sorted(PKG.glob("*.py")):
        tree = ast.parse(path.read_text())
        for node in ast.walk(tree):
            if not (isinstance(node, ast.Constant) and isinstance(node.value, str)):
                continue
            # An expression statement holding only a string is a docstring.
            for key in PRESS.findall(node.value):
                if key.lower() in NOT_A_KEY:
                    continue
                found.append((key, path.name, node.lineno))
    return found


def _is_docstring_line(path: str, line: int) -> bool:
    """Is this literal a docstring? Those are documentation, not screen text."""
    tree = ast.parse((PKG / path).read_text())
    for node in ast.walk(tree):
        if isinstance(node, (ast.Module, ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)):
            doc = ast.get_docstring(node, clean=False)
            if doc is None or not node.body:
                continue
            first = node.body[0]
            if isinstance(first, ast.Expr) and getattr(first, "lineno", None) is not None:
                if first.lineno <= line <= getattr(first, "end_lineno", first.lineno):
                    return True
    return False


def screen_keys() -> list[tuple[str, str, int]]:
    return [
        hit for hit in named_keys() if not _is_docstring_line(hit[1], hit[2])
    ]


# ── the sentinel comes FIRST, so a broken scanner is read before its verdict ──


def test_the_scanner_can_see_a_key_that_is_there() -> None:
    """THE POSITIVE SENTINEL. Without this the test below is vacuous.

    These five keys are named in screen strings on purpose and are all bound:
    `d` (reactivate a disabled account — `UNRANKED_REASON["disabled"]` and
    `_disabled_line` promise it in the same words), `l` (sign in, the key a dead
    account's own line points at), `s` (set the machine login), `S` (pool share)
    and `n` (give the machine login a slot).
    """
    hits = screen_keys()
    assert hits, "the scanner found NO `press X` in any screen string — it is broken"
    seen = {key for key, _, _ in hits}
    for key in ("d", "l", "s", "n"):
        assert key in seen, (
            f"the scanner cannot see `press {key}`, which IS in a screen string — "
            "it has stopped scanning and the absence assertion below is vacuous",
            sorted(seen),
        )
    # A floor on the count, so a regex that degrades to one lucky match is caught.
    assert len(hits) >= 8, (len(hits), hits)


def test_the_scanner_can_see_the_bindings() -> None:
    """The other half of the sentinel: the key list must not be empty either."""
    keys = bound_keys()
    assert {"q", "d", "l", "s", "S", "n", "p", "m", "X", "O"} <= keys, sorted(keys)
    # And it must not be a wildcard that accepts anything.
    assert "r" not in keys, (
        "`r` is bound again — if that is intended the ten strings this test "
        "guards may say `press r` once more, and this assertion should go with it"
    )
    assert "R" not in keys, sorted(keys)


# ── the verdict ──────────────────────────────────────────────────────────────


def test_no_screen_string_names_an_unbound_key() -> None:
    """THE MUTATION. Put `press r` back into any screen string in `threads_tui/`
    and this goes red naming the file, the line and the key.
    """
    keys = bound_keys()
    offenders = [
        f"{path}:{line} says `press {key}`, which is not bound"
        for key, path, line in screen_keys()
        if key not in keys and key.lower() not in keys
    ]
    assert not offenders, offenders
