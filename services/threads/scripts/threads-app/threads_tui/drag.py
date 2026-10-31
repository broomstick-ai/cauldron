"""The recorded deviation.

This module exists so that a future blueprint pass IDENTIFIES this app as drag
rather than DISCOVERING it. Discovering it costs an investigation; identifying
it costs a read. The difference is the whole point of writing this down.

It is `drag` in the blueprint's sense: a tolerated deviation from the design of
record, recorded deliberately, with a stated criterion for when it stops being
tolerated. It is NOT a stub — the app does not pretend to work, it works — and
it is NOT dead code. It is the right thing in the wrong language.

The note is surfaced IN THE TOOL (press `!` on the board), not only in a file,
because a note nobody reads is a note nobody acts on.
"""

from __future__ import annotations

TITLE = "This app is recorded drag"

DEVIATION = """\
A Python Textual application lives inside a Rust workspace. Every other \
executable in lib/threads is Rust; this one is not. That is a deliberate, \
recorded deviation from the plan of record, not an oversight and not a \
decision anyone needs to re-litigate in a code review.\
"""

WHY = """\
The board is a dense, redraw-heavy terminal UI with modal flows. Textual gives \
that for free. Building the equivalent in Rust was scoped and judged to cost \
more than the interface was worth AT THE TIME THE INTERFACE WAS STILL CHANGING \
SHAPE. Writing a UI twice is cheap only if you write the throwaway one fast.\
"""

PLAN_OF_RECORD = """\
A Rust equivalent, in-tree with the rest of lib/threads, sharing the daemon's \
own types instead of re-deriving them across a JSON boundary.\
"""

# The two obligations. These are what make the eventual port a PORT rather than
# a rewrite, and they are the reason this drag is cheap to carry. Each one is
# mechanically checkable — see `tests/` — so neither decays quietly.
OBLIGATIONS = [
    (
        "The render layer stays pure.",
        "threads_tui/render.py and threads_tui/pressure.py take state in and "
        "return Text lines out. No I/O, no clock reads, no environment reads, "
        "no imports that perform I/O at import time. Every time-dependent value "
        "arrives as an explicit `now` parameter. A pure render layer is a "
        "TRANSLATION problem in the port; an impure one is a redesign, because "
        "the hidden inputs have to be found before anything can move.",
    ),
    (
        "Every fact comes from the API.",
        "No direct SQLite reads, no shelling out to the threads binary for "
        "data, no reading account files behind the daemon's back. The one "
        "carve-out is setting this machine's local account (threads_tui/local.py), "
        "which is machine-local state the daemon deliberately has no concept of. "
        "This obligation means the port inherits a DATA CONTRACT that is already "
        "proven over the wire, rather than a pile of incidental reads that each "
        "have to be re-discovered and re-justified.",
    ),
]

# The resolution criterion. Stated as a checkable condition, not an aspiration,
# so that a blueprint pass can answer "is this still tolerated?" by testing it
# rather than by forming an opinion.
RESOLUTION_CRITERION = """\
This drag is RESOLVED when a Rust binary in lib/threads renders the board and \
is what `threads` on PATH executes — at which point this directory is deleted \
outright, not kept alongside.

It becomes DUE — that is, it stops being tolerated and becomes work that must \
be scheduled — when ANY ONE of these becomes true:

  1. The board's data contract is stable for a full release cycle with no
     field added or removed. Churn was the reason not to write it twice;
     when the churn stops, the reason stops with it.

  2. Either obligation above is breached and not repaired in the same change.
     The obligations are what keep the port cheap. A breach means the cost of
     porting is now rising, so the cheapest remaining moment to port is now.

  3. This app acquires a second consumer — anything that imports threads_tui,
     or any script that parses its output. Drag with one consumer is a local
     cost; drag with two is an interface, and an interface in the wrong
     language is a different and much worse problem.

Until one of those is true, this app is the right thing to be running and no \
one should feel obliged to rewrite it.\
"""


def note_lines() -> list[str]:
    """The note as plain text lines. Pure — this is what the screen renders."""
    out: list[str] = [TITLE, ""]
    out += ["THE DEVIATION", "  " + DEVIATION, ""]
    out += ["WHY IT WAS TAKEN", "  " + WHY, ""]
    out += ["PLAN OF RECORD", "  " + PLAN_OF_RECORD, ""]
    out += ["THE TWO OBLIGATIONS (these keep the port a port)", ""]
    for i, (headline, body) in enumerate(OBLIGATIONS, 1):
        out += [f"  {i}. {headline}", "     " + body, ""]
    out += ["RESOLUTION CRITERION", ""]
    for line in RESOLUTION_CRITERION.split("\n"):
        out.append("  " + line if line else "")
    return out
