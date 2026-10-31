"""Formatting primitives: the countdown grammar, ages, meter bars, colors.

The grammar is fixed so that columns of these align and read at a glance:
`42m` under an hour, `15h 42m` under a day, `3d 07h` beyond.

PURE. Copied from the old app's `fmt.py`, with one change: `now_utc()` has
moved out to `threads_tui.clock`. It was the single impure function in an
otherwise pure module, and leaving it here meant every importer of a formatting
helper also imported a clock — which is exactly how a `now or now_utc()`
default gets written without anyone noticing. `fmt_countdown` now requires
`now` for the same reason.
"""

from __future__ import annotations

from datetime import datetime, timezone

UTC = timezone.utc

# ── the palette (DESIGN §7) ──────────────────────────────────────────────────

SURFACE = "#1a1a19"
INK = "#ffffff"
INK2 = "#c3c2b7"
MUTED = "#898781"

BLUE = "#3987e5"  # magnitude, 0-69%
AMBER = "#fab219"  # warning: 70-89%, staleness, reconnecting, hazard frames
RED = "#d03b3b"  # critical: 90-99%, dead/expired/wiped/stranded, refusals
ATLIMIT = "#6e2f2f"  # the 100% bar only
GREEN = "#0ca30c"  # momentary confirmations only

EIGHTHS = " ▏▎▍▌▋▊▉█"


def parse_ts(value) -> datetime | None:
    """RFC3339 out of the daemon or SQLite. Returns None on anything unusable —
    a timestamp we cannot read is not a timestamp we may guess at."""
    if not isinstance(value, str) or not value:
        return None
    text = value.strip()
    if text.endswith("Z"):
        text = text[:-1] + "+00:00"
    try:
        dt = datetime.fromisoformat(text)
    except ValueError:
        return None
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=UTC)
    return dt.astimezone(timezone.utc)


def fmt_delta(seconds: float) -> str:
    """The fixed countdown grammar. Negative clamps to `0m`."""
    seconds = max(0.0, float(seconds))
    minutes = int(seconds // 60)
    if minutes < 60:
        return f"{minutes}m"
    hours, minutes = divmod(minutes, 60)
    if hours < 24:
        return f"{hours}h {minutes:02d}m"
    days, hours = divmod(hours, 24)
    return f"{days}d {hours:02d}h"


def fmt_age(seconds: float) -> str:
    """Age of an observation. Same grammar; seconds resolution under a minute
    so a just-completed probe reads as fresh rather than as `0m`."""
    seconds = max(0.0, float(seconds))
    if seconds < 60:
        return f"{int(seconds)}s"
    return fmt_delta(seconds)


def fmt_clock(dt: datetime | None) -> str:
    """The absolute reset time, in the operator's local zone: `Fri 07:40`."""
    if dt is None:
        return ""
    return dt.astimezone().strftime("%a %H:%M")


def fmt_countdown(resets_at: datetime | None, now: datetime) -> str:
    """`now` is required. A defaulted clock is a hidden input."""
    if resets_at is None:
        return "reset time unknown"
    return f"resets in {fmt_delta((resets_at - now).total_seconds())}"


def fmt_pct(pct: float | None, low_confidence: bool = False) -> str:
    """Right-aligned in 4 columns: `  5%` / `~44%` / `100%`."""
    if pct is None:
        return "   ?"
    marker = "~" if low_confidence else ""
    value = int(round(pct))
    return f"{marker}{value}%".rjust(4)


def fmt_days(days: float | None, state: str) -> str:
    """`login Nd` in the card title. Mirrors the bash renderer's `days_str`."""
    if state in ("clone-of", "stranded"):
        return "n/a"  # the countdown belongs to someone else's login
    if days is None:
        return "?"
    if days <= 0:
        return "EXPIRED"
    return f"{days:.0f}d"


def bar(pct: float | None, width: int) -> str:
    """A `width`-cell meter. `None` renders as an empty track — the caller is
    responsible for saying WHY it is empty; this never invents a zero."""
    if width <= 0:
        return ""
    if pct is None:
        return "░" * width
    filled = max(0.0, min(100.0, float(pct))) / 100.0 * width
    whole = int(filled)
    remainder = filled - whole
    cells = "█" * whole
    if whole < width:
        eighth = int(remainder * 8)
        cells += EIGHTHS[eighth] if eighth else "░"
        cells += "░" * (width - whole - 1)
    return cells[:width]


def meter_color(pct: float | None) -> str:
    """Exactly three meter states plus at-limit (DESIGN §7)."""
    if pct is None:
        return MUTED
    if pct >= 100.0:
        return ATLIMIT
    if pct >= 90.0:
        return RED
    if pct >= 70.0:
        return AMBER
    return BLUE


def age_style(seconds: float | None) -> tuple[str, bool]:
    """(color, is_stale) for an observation age. Staleness is never red."""
    if seconds is None:
        return MUTED, False
    if seconds > 3600:
        return AMBER, True
    if seconds > 1200:
        return AMBER, False
    return MUTED, False


def ellipsize(text: str, width: int) -> str:
    if width <= 0:
        return ""
    if len(text) <= width:
        return text
    if width == 1:
        return "…"
    return text[: width - 1] + "…"


SPINNER = "⣾⣽⣻⢿⡿⣟⣯⣷"


def spinner_frame(tick: int) -> str:
    return SPINNER[tick % len(SPINNER)]
