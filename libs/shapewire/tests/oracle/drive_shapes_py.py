"""DRIVE THE REFERENCE MIRROR — `threads_tui/shapes.py::ShapeMirror`.

The Rust `Mirror` is a SECOND implementation of a contract whose only prior
implementation is 427 lines of Python in one consumer. Two implementations of
one contract drift, and the whole reason the mirror was written into the chassis
is to end that drift — so the only real evidence the twin has not already
drifted is to drive the SAME frame sequences through BOTH and compare the
resulting state.

This script is the Python end. It reads a scenario file on `argv[2]`:

    {"shape": "accounts", "frames": [ {...}, {...} ]}

applies every frame through `ShapeMirror.on_frame`, and prints one canonical
state dump on stdout. `argv[1]` is the directory holding the `threads_tui`
package. `tests/mirror_oracle.rs` produces the same dump from the Rust mirror
and differs the two.

It imports nothing but the standard library and `threads_tui.shapes` (which
itself imports only `threads_tui.fmt`), so it runs on a bare system python with
no venv — which is what lets the comparison be a `cargo test` rather than a
ritual somebody has to remember to perform.
"""

import json
import sys


def stamp(dt):
    """One spelling for every timestamp, on both sides of the comparison.

    `parse_ts` has already normalised to UTC; six fractional digits always, so a
    whole second and a sub-second stamp are formatted the same way and a
    difference in the dump is a difference in the STATE.
    """
    if dt is None:
        return None
    return dt.strftime("%Y-%m-%dT%H:%M:%S.%f+00:00")


def cell(state):
    return {
        "kind": state.kind,
        "complete": state.complete,
        "folded_at": stamp(state.folded_at),
        "since_init": state.since_init,
        "covers_since": stamp(state.covers_since),
        "reason": state.reason,
    }


def dump(mirror):
    return {
        "name": mirror.name,
        "state": mirror.state,
        "dead_reason": mirror.dead_reason,
        "init_at": stamp(mirror.init_at),
        "reinit": mirror.reinit,
        "lost": mirror.lost,
        "exported": mirror.exported,
        "exports": mirror.exports,
        "last_frame_at": stamp(mirror.last_frame_at),
        "shared": mirror.shared,
        "shared_cells": {n: cell(c) for n, c in sorted(mirror.shared_cells.items())},
        "keys": mirror.keys(),
        "held": {
            k: {
                "row": h.row,
                "as_of": stamp(h.as_of),
                "at": stamp(h.at),
                "dead_reason": h.dead_reason,
                "cells": {n: cell(c) for n, c in sorted(h.cells.items())},
            }
            for k, h in sorted(mirror.held.items())
        },
    }


def main():
    sys.path.insert(0, sys.argv[1])
    from threads_tui.shapes import ShapeMirror

    scenario = json.load(open(sys.argv[2]))
    mirror = ShapeMirror(name=scenario["shape"])
    moved = []
    for frame in scenario["frames"]:
        moved.append(bool(mirror.on_frame(frame)))
    out = dump(mirror)
    # `on_frame`'s `bool` is the whole of the reference mirror's receipt
    # vocabulary — rule 2.3's received / applied / rejected, spent at the door.
    # It is carried here so the comparison can say exactly what the Rust
    # `Receipt` adds rather than only asserting that it differs.
    out["moved"] = moved
    json.dump(out, sys.stdout, sort_keys=True, indent=1)


if __name__ == "__main__":
    main()
