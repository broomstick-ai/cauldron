"""The clock. One function, one module, on purpose.

Every other module in this package that needs the time takes it as a parameter.
This is the only place the real clock is read, so "who reads the clock?" is
answered by finding the callers of this module rather than by auditing every
file.

That matters for two reasons. It keeps the render layer golden-testable — with
a clock inside the renderer, no two runs agree and there is nothing to compare
against. And it keeps the eventual Rust port honest: a single, named, injected
input is a parameter to translate, whereas a clock read scattered through the
render code is a set of hidden inputs that must all be found first.
"""

from __future__ import annotations

from datetime import datetime, timezone


def now_utc() -> datetime:
    return datetime.now(timezone.utc)
