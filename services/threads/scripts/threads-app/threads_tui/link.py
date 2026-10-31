"""The connection state, as a value.

This lives in its own module for one reason: the render layer needs the type,
and the render layer must not import anything that performs I/O at import time.
In the old app `render.py` imported `LinkState` from `daemon.py`, which imported
`paths.py`, which executed `PATHS = resolve()` at module scope and read the
environment. Importing the pure render layer therefore had an environment-
dependent side effect — a small thing, but exactly the kind of edge that makes
a "pure" module untestable in isolation and turns the eventual Rust port into
an archaeology exercise.

One four-value enum, no imports. See `threads_tui.drag`, obligation 1.
"""

from __future__ import annotations

from enum import Enum


class LinkState(str, Enum):
    CONNECTING = "connecting"
    LIVE = "live"
    RECONNECTING = "reconnecting"
    OFFLINE = "offline"
