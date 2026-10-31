"""**THE DAEMON'S OWN WARNING COUNT, ON THE BOARD HE ACTUALLY READS.**

The operator's ask: *"For TUI and the threads tui can you display the number of
warnings coming from threads so it annoys me? 27k is incredible."*

The 27,705 (29,644 by the time this was built) were correct every time and
invisible every time, because their only output was `~/.aui/threads-daemon.log`
— 47 MB, 165,281 `INFO` lines, and nothing that distinguished a warning from
anything else. So the fix is not a better log line. It is the number arriving
where he is already looking.

**EVERY ASSERTION HERE IS ON THE RENDERED STRING**, never on `Board` fields or a
projected dict. The rendered string is what he reads; a dict that holds 47 and a
header that draws nothing are not the same product, and only one of them is the
feature.

**WHAT EACH TEST WOULD CATCH, and every one was forced red by the mutation named
in its own docstring.**

Run:  .venv/bin/python -m pytest -q tests/test_warnings.py
"""

from __future__ import annotations

import pathlib
import re
import sys
from datetime import datetime, timezone

sys.path.insert(0, __file__.rsplit("/tests/", 1)[0])
sys.path.insert(0, __file__.rsplit("/test_warnings.py", 1)[0])

from fake_daemon import warnings_export, warnings_live  # noqa: E402

from threads_tui import model, render, shapes  # noqa: E402
from threads_tui.link import LinkState  # noqa: E402

NOW = datetime(2026, 10, 6, 12, 0, tzinfo=timezone.utc)

#: Wide enough that `header_line1`'s narrow fallback is NOT taken, so a test that
#: asserts on a tail segment is asserting on the real layout rather than on the
#: truncation. `test_refresh.py` has a long comment about this exact trap.
WIDE = 200

#: Narrow enough to force the fallback. 100 columns is a real terminal.
NARROW = 100


def header(board: model.Board, width: int = WIDE) -> str:
    """The painted first header line, as a plain string."""
    state = render.HeaderState(
        link=LinkState.LIVE,
        subscribed=True,
        refresh_age=5.0,
        running=4,
        running_live=True,
    )
    return render.header_line1(board, state, width, NOW).plain


def mirror(*frames: dict) -> shapes.ShapeMirror:
    m = shapes.ShapeMirror(name=shapes.WARNINGS)
    for f in frames:
        m.on_frame(f)
    return m


# ═══════════════════════════════════════════════════════════════════════════
# 1. The two silences, and they are different facts
# ═══════════════════════════════════════════════════════════════════════════


def test_a_daemon_that_has_not_warned_draws_nothing() -> None:
    """**ZERO DRAWS NOTHING.**

    The operator's own requirement: *"Zero warnings should be visibly zero or
    absent — not a decoration that is always there and therefore ignored. A
    number that is always on screen becomes wallpaper, which is how the log
    failed."*

    So a clean daemon costs zero columns. Note what this does NOT mean: the
    producer still exports `warn: 0` in state `known`
    (`tests/warnings_shape.rs::zero_exports_as_a_known_zero_rather_than_an_absent_cell`
    pins that). The producer is honest and the face is quiet; those are
    different jobs.

    **THE MUTATION.** In `render.warnings_segment`, delete the
    `if total == 0: return Text()` guard.

    Forced red:
    ```text
    AssertionError: a clean daemon costs no columns:
    ' claude-accounts   ▶ 4 running ... · ⚠ 0 warnings · R = poll accounts'
    ```
    """
    line = header(model.Board(daemon_warnings=0, daemon_errors=0))
    assert "warning" not in line, f"a clean daemon costs no columns: {line!r}"
    assert "⚠" not in line, line

    # THE POSITIVE CONTROL. Without it this passes against a segment that can
    # never draw anything at all, which is the vacuous green this suite is
    # written against.
    loud = header(model.Board(daemon_warnings=1, daemon_errors=0))
    assert "⚠ 1 warning" in loud, f"the same header DOES draw a real count: {loud!r}"


def test_a_daemon_that_has_not_said_is_not_a_daemon_that_said_zero() -> None:
    """**`None` IS NOT `0`, AND THE BOARD MUST NOT TURN ONE INTO THE OTHER.**

    A daemon too old to carry the `warnings` shape has not told us it is healthy.
    It has told us nothing. Both render as absence — which is right, because the
    operator asked for silence when there is nothing to say — but the MODEL keeps
    them apart, because they are different facts and only one of them is
    reassuring.

    This is the three-valued-unknown rule applied one level up, and it is the one
    place this feature could quietly lie.

    **THE MUTATION.** In `model.Board`, default `daemon_warnings: int = 0`
    instead of `int | None = None`.

    Forced red:
    ```text
    AssertionError: an unreported count is None, never 0 -- a daemon that has
    not spoken has not said it is healthy
    ```
    """
    unspoken = model.Board()
    assert unspoken.daemon_warnings is None, (
        "an unreported count is None, never 0 -- a daemon that has not spoken "
        "has not said it is healthy"
    )
    assert unspoken.daemon_errors is None

    # Both silences look the same on screen, deliberately.
    assert "⚠" not in header(unspoken)
    assert "⚠" not in header(model.Board(daemon_warnings=0, daemon_errors=0))

    # And an un-hydrated mirror does not invent one either.
    cold = mirror()
    board = model.project(
        shapes.ShapeMirror(), now=NOW, absent_note=None, warnings=cold
    )
    assert board.daemon_warnings is None, (
        "a mirror that has never exported leaves the count unspoken: "
        f"{board.daemon_warnings!r}"
    )


# ═══════════════════════════════════════════════════════════════════════════
# 2. A real count reaches the screen
# ═══════════════════════════════════════════════════════════════════════════


def test_the_header_draws_the_count_when_it_is_real() -> None:
    """**THE STRING HE WILL ACTUALLY READ.**

    Thousands separators because `27705` and `2770` are the same shape at a
    glance and the magnitude is the whole point — *"27k is incredible"* is a
    sentence about a number's size.

    **THE MUTATION.** In `render.warnings_segment`, drop the `,` from the
    format: `f"{total} "`.

    Forced red:
    ```text
    AssertionError: the count is grouped so its MAGNITUDE reads at a glance:
    '... · ⚠ 29644 warnings · ...'
    ```
    """
    line = header(model.Board(daemon_warnings=29_644, daemon_errors=0))
    assert "⚠ 29,644 warnings" in line, (
        f"the count is grouped so its MAGNITUDE reads at a glance: {line!r}"
    )
    # Singular is singular. A header that says "1 warnings" is a header nobody
    # proofread, and he reads this line every day.
    assert "⚠ 1 warning" in header(model.Board(daemon_warnings=1))
    assert "1 warnings" not in header(model.Board(daemon_warnings=1))


def test_errors_are_spelled_apart_from_warnings() -> None:
    """**AN ERROR IS NOT A WARNING, AND THE HEADER SAYS SO.**

    The producer keeps `warn` and `error` in two cells on purpose
    (`warnings::Tally::error`), because a single number cannot tell 47 warnings
    from 47 errors. The header sums them for the headline — that is the one
    number a one-line header has room for — and then names the errors, so the
    distinction survives to the screen.

    199 of the live log's 33,188 records are `ERROR`, so this is not a
    hypothetical column.

    **THE MUTATION.** In `render.warnings_segment`, delete the
    `if errors:` block that appends the parenthesis.

    Forced red:
    ```text
    AssertionError: errors are named, not folded into the total:
    '... · ⚠ 246 warnings · ...'
    ```
    """
    line = header(model.Board(daemon_warnings=47, daemon_errors=199))
    assert "⚠ 246 warnings (199 errors)" in line, (
        f"errors are named, not folded into the total: {line!r}"
    )
    assert "(1 error)" in header(model.Board(daemon_warnings=0, daemon_errors=1)), (
        "and one error is one error, not '1 errors'"
    )


def test_the_count_survives_a_narrow_terminal() -> None:
    """**IT IS NOT THE FIRST THING DROPPED WHEN THE TERMINAL IS SMALL.**

    `header_line1` truncates from the right by rebuilding the tail, and
    everything else in that tail is a REMINDER (`R = poll accounts`) or an age.
    A warning count that vanishes at 100 columns is a warning count the operator
    does not have — so it is in BOTH the full chain and the fallback, the only
    segment in this function that is.

    **THE MUTATION.** In `render.header_line1`, delete the
    `if warnings.cell_len:` block inside the narrow-terminal fallback.

    Forced red:
    ```text
    AssertionError: the count survives truncation:
    ' claude-accounts   ▶ 4 running      threads ● live · read 5s ago'
    """
    board = model.Board(daemon_warnings=47, daemon_errors=0)
    narrow = header(board, NARROW)
    assert "⚠ 47 warnings" in narrow, f"the count survives truncation: {narrow!r}"

    # THE POSITIVE CONTROL for the premise: at this width a tail segment IS
    # dropped, so the fallback really was taken and this is not a test of the
    # wide layout wearing a narrow number.
    #
    # **IT USED TO READ `"R = poll accounts" not in narrow` AND THAT CONTROL
    # DIED IN THE MERGE, SILENTLY.** This module was written against a header
    # whose tail still carried that reminder; the board's three corrections
    # DELETED the string from the renderer outright — on the operator's own
    # instruction that it be "the forcing function to prevent ever needing
    # this". Nothing conflicted, nothing went red, and the assertion became
    # vacuously true at EVERY width, including the wide one. A control that
    # cannot fail does not establish the premise it is there to establish.
    #
    # The surviving droppable segment is the AGE, so that is what is measured.
    # Asserted against the wide layout in the same breath, so this control
    # cannot quietly die the same way: if the age ever leaves the wide tail too,
    # the first assertion fails rather than the second passing for free.
    wide = header(board, WIDE)
    assert "last update:" in wide, (
        f"the age is in the WIDE tail, so its absence below means something: {wide!r}"
    )
    assert "last update:" not in narrow, (
        f"the fallback was taken, so the survival above is real: {narrow!r}"
    )
    # And it still costs nothing when there is nothing to say.
    assert "⚠" not in header(model.Board(daemon_warnings=0, daemon_errors=0), NARROW)


# ═══════════════════════════════════════════════════════════════════════════
# 3. End to end on this side: a daemon frame -> the painted header
# ═══════════════════════════════════════════════════════════════════════════


def test_a_daemon_frame_moves_the_number_on_the_painted_header() -> None:
    """**THE WHOLE BOARD-SIDE PATH, FROM WIRE BYTES TO THE RENDERED STRING.**

    An export then a live `row`, both built by `fake_daemon` out of the
    producer's own field names, driven through the real `ShapeMirror`, projected
    by the real `model.project`, and painted by the real `header_line1`. Nothing
    is stubbed between the frame and the string.

    Both directions in one test, which is the point: the header is silent after
    the zero export and loud after the live frame, so neither state is
    satisfiable by a stub.

    **THE MUTATION.** In `model.project`, delete the
    `board.daemon_warnings = wrow["warn"]` assignment.

    Forced red:
    ```text
    AssertionError: a live row frame from the daemon reaches the header:
    ' claude-accounts   ▶ 4 running ... · R = poll accounts'
    ```
    """
    accounts = shapes.ShapeMirror()
    warn_mirror = mirror(warnings_export(0, 0))

    quiet = header(
        model.project(accounts, now=NOW, absent_note=None, warnings=warn_mirror)
    )
    assert "⚠" not in quiet, f"the daemon's honest zero draws nothing: {quiet!r}"

    # The daemon warns three times and samples. One live row frame.
    warn_mirror.on_frame(warnings_live(3, 0))
    loud = header(
        model.project(accounts, now=NOW, absent_note=None, warnings=warn_mirror)
    )
    assert "⚠ 3 warnings" in loud, (
        f"a live row frame from the daemon reaches the header: {loud!r}"
    )

    # And it keeps climbing, because the producer's counter does.
    warn_mirror.on_frame(warnings_live(29_644, 199))
    grown = header(
        model.project(accounts, now=NOW, absent_note=None, warnings=warn_mirror)
    )
    assert "⚠ 29,843 warnings (199 errors)" in grown, (
        f"and a later frame replaces it rather than accumulating: {grown!r}"
    )


def test_a_dead_fold_stops_the_board_claiming_a_count() -> None:
    """**A RESET MIRROR DRAWS NOTHING, RATHER THAN THE LAST NUMBER IT HELD.**

    `app.py` resets this mirror when the link drops and when the daemon's
    `boot_id` moves. Painting a remembered 47 beside a dead link would be *"a
    frozen screen that looks calm"* — the board's own words for the failure it
    exists to prevent — and after a restart the number would describe a process
    that is gone, because the counters are since-boot.

    **THIS TEST IS NOT SINGLE-MUTATION FALSIFIABLE, AND THAT IS RECORDED RATHER
    THAN PAPERED OVER.** The forced-red pass refuted two successive guesses about
    what carries it:

    1. *`model.project`'s `warnings.exported` condition* — deleted, still green.
    2. *`ShapeMirror.reset` clearing `held`* — deleted, still green.

    Both are true protections and either one alone is sufficient: `reset()` clears
    `held` AND sets `exported = False`, and `project` has to get past both. So the
    only mutation that flips THIS test is the compound one, which is weaker
    evidence than a single edit.

    It is kept, because it is the claim the operator cares about — the painted
    header after a dead link — and the two mechanisms are pinned SEPARATELY by
    `test_an_unexported_mirror_is_not_read_even_if_it_holds_a_row` below, which has
    a real single-mutation red. A docstring naming a mutation that does not flip
    its test is how a decoration gets mistaken for a guard, so neither of the two
    wrong guesses is left standing in this file.

    **THE MUTATION (compound).** In `model.project`, drop `and warnings.exported`
    AND in `shapes.ShapeMirror.reset`, delete `self.held.clear()`.

    Forced red:
    ```text
    AssertionError: a reset mirror claims nothing: ' claude-accounts   ▶ 4 running
    ... · ⚠ 47 warnings · R = poll accounts'
    ```
    """
    accounts = shapes.ShapeMirror()
    m = mirror(warnings_export(47, 0))
    assert "⚠ 47 warnings" in header(
        model.project(accounts, now=NOW, absent_note=None, warnings=m)
    ), "the control: it was on screen before the reset"

    m.reset()
    after = header(model.project(accounts, now=NOW, absent_note=None, warnings=m))
    assert "⚠" not in after, f"a reset mirror claims nothing: {after!r}"


def test_an_unexported_mirror_is_not_read_even_if_it_holds_a_row() -> None:
    """**ONE OF THE TWO PROTECTIONS ABOVE, PINNED ON ITS OWN.**

    `project` reads the warning count only from a mirror that has actually
    EXPORTED. The state this isolates is real and not contrived: a mirror can hold
    a row and not be exported — a `shared`-only frame, or a reset that cleared the
    flag while something refilled a row — and in that state the number is not this
    connection's to report.

    Constructed by clearing `exported` while leaving `held` in place, which is the
    only way to exercise this guard in isolation, because `reset()` does both.

    **THE MUTATION.** In `model.project`, drop the condition:
    `if warnings is not None:`.

    Forced red:
    ```text
    AssertionError: a mirror that has not exported is not read, even holding a
    row: ' claude-accounts   ▶ 4 running ... · ⚠ 47 warnings · R = poll accounts'
    ```
    """
    accounts = shapes.ShapeMirror()
    m = mirror(warnings_export(47, 0))
    assert m.row(model.KEY_DAEMON).get("warn") == 47, "the control: the row is held"

    # The flag alone, with the row left exactly where it was.
    m.exported = False
    line = header(model.project(accounts, now=NOW, absent_note=None, warnings=m))
    assert "⚠" not in line, (
        f"a mirror that has not exported is not read, even holding a row: {line!r}"
    )


def test_the_app_drops_the_warning_count_wherever_it_drops_the_account_fold() -> None:
    """**THE WIRING THE TEST ABOVE CANNOT REACH.**

    `test_a_dead_fold_stops_the_board_claiming_a_count` proves that a reset mirror
    draws nothing. It cannot prove that anything ever RESETS it — that lives in
    `app.py`'s connection handling, behind a Textual app and a socket, and the
    failure mode is silent: the count would simply go on being painted after the
    daemon it describes was gone.

    So this reads `app.py` and checks the pairing directly: **every place the
    account fold is dropped, the warning count is dropped too.** Two sites today —
    the link dying, and a `boot_id` that moved — and a third added later without
    its partner is exactly the regression this catches.

    A SOURCE read, in the style of `test_wire_inventory.py`, and it does not
    witness itself: it scans `threads_tui/app.py`, which contains no part of this
    file.

    **IT IS NOT VACUOUS**: it asserts `self.shapes.reset()` is FOUND first, so a
    rename that made the scan match nothing fails here rather than passing.

    **THE MUTATION.** In `app.py`, delete one of the two `self.warnings.reset()`
    calls.

    Forced red:
    ```text
    AssertionError: app.py drops the account fold 2 times and the warning count
    1 -- a fold dropped without its partner leaves a stale count on screen
    ```
    """
    app_py = pathlib.Path(__file__).resolve().parents[1] / "threads_tui" / "app.py"
    src = app_py.read_text()
    accounts_resets = src.count("self.shapes.reset()")
    warning_resets = src.count("self.warnings.reset()")

    assert accounts_resets > 0, (
        "NON-VACUITY FAILED: no `self.shapes.reset()` found in app.py, so the "
        "comparison below proves nothing"
    )
    assert warning_resets == accounts_resets, (
        f"app.py drops the account fold {accounts_resets} times and the warning "
        f"count {warning_resets} -- a fold dropped without its partner leaves a "
        f"stale count on screen"
    )


# ═══════════════════════════════════════════════════════════════════════════
# 4. The cross-language guard — the one that catches a rename
# ═══════════════════════════════════════════════════════════════════════════


def test_the_board_reads_the_field_names_the_daemon_writes() -> None:
    """**THE TEST THAT CATCHES A RENAME ON THE OTHER SIDE OF THE REPO.**

    `test_wire_inventory.py`'s lesson, applied to this shape: every socket test
    in this suite drives a FAKE daemon, so the fake and the board get renamed
    together and agree with each other perfectly. The one thing neither of them
    is, is the producer.

    So this reads `lib/threads/src/shapes/warnings.rs` — the producer's own
    `Root::row` — and checks the names this board actually indexes against it. A
    SOURCE read and not a socket read, deliberately: a test that needs the
    operator's daemon running is a test that gets skipped.

    **IT DOES NOT WITNESS ITSELF.** It scans the DAEMON's file, in another
    language, in another directory. There is no path by which this file's own
    text could satisfy it.

    **IT IS NOT VACUOUS**: it asserts the parse found a non-empty key set before
    comparing, so a regex that silently matched nothing fails here rather than
    passing everything.

    **THE MUTATION.** In `shapes/warnings.rs`'s `row()`, rename `"warn"` to
    `"warnings"`.

    Forced red:
    ```text
    AssertionError: the board indexes `warn`, which the producer's row() does not
    write. Producer writes: ['error', 'kinds', 'producer', 'sink_errors',
    'warnings']
    ```
    """
    here = pathlib.Path(__file__).resolve()
    # test_warnings.py -> tests -> threads-app -> scripts -> threads
    shape_rs = here.parents[3] / "src" / "shapes" / "warnings.rs"
    assert shape_rs.is_file(), f"the producer's shape must be readable: {shape_rs}"
    src = shape_rs.read_text()

    # The body of `fn row` only, so `cells()` and `shared()` cannot supply a name
    # `row` does not carry.
    start = src.index("fn row(&self, key: &str) -> Option<Value> {")
    end = src.index("fn cells(&self, key: &str)", start)
    row_body = src[start:end]
    produced = set(re.findall(r'"(\w+)":', row_body))

    assert produced, (
        "NON-VACUITY FAILED: the parse found no field names in the producer's "
        "row(), so the comparison below proves nothing"
    )

    # What this board actually reads, by name, in `model.project`.
    for name in ("warn", "error"):
        assert name in produced, (
            f"the board indexes `{name}`, which the producer's row() does not "
            f"write. Producer writes: {sorted(produced)}"
        )

    # And the KEY is spelled the same on both sides, or the board subscribes to
    # a key the producer never mints and sits quiet forever.
    m = re.search(r'pub const KEY_DAEMON: &str = "(\w+)";', src)
    assert m, "the producer declares its one key as a const"
    assert m.group(1) == model.KEY_DAEMON, (
        f"the key is spelled identically on both sides: producer "
        f"{m.group(1)!r} vs board {model.KEY_DAEMON!r}"
    )


def test_the_board_subscribes_to_the_shape_the_daemon_registers() -> None:
    """**THE NAME THE BOARD ASKS FOR IS THE NAME THE DAEMON REGISTERS.**

    The other half of the rename risk, and the one that bit this board before:
    on 2026-09-28 every method became a path, the board kept asking for the old
    names, and it drew `NO FOLD ON THIS DAEMON` at a daemon holding six folds.

    `Shape::NAME` is the producer's declaration. If it and `shapes.WARNINGS`
    drift, this board subscribes to nothing, gets a refusal it deliberately
    swallows, and draws no count forever — the exact silent failure this whole
    feature exists to end.

    **THE MUTATION.** In `shapes/warnings.rs`, change
    `const NAME: &'static str = "warnings"` to `"daemon_warnings"`.

    Forced red:
    ```text
    AssertionError: the board subscribes to 'warnings'; the producer registers
    'daemon_warnings'
    ```
    """
    here = pathlib.Path(__file__).resolve()
    shape_rs = here.parents[3] / "src" / "shapes" / "warnings.rs"
    src = shape_rs.read_text()
    m = re.search(r"const NAME: &'static str = \"([\w/-]+)\";", src)
    assert m, "the producer declares its shape name"
    assert m.group(1) == shapes.WARNINGS, (
        f"the board subscribes to {shapes.WARNINGS!r}; the producer registers "
        f"{m.group(1)!r}"
    )

    # And it is actually REGISTERED, not merely declared. A shape nothing
    # registers is a shape no subscribe can reach.
    runtime_rs = here.parents[3] / "src" / "runtime.rs"
    assert "shapes::warnings::Warnings" in runtime_rs.read_text(), (
        "the daemon registers the shape on its hub; a declared-but-unregistered "
        "shape refuses every subscribe"
    )
