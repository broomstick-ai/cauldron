"""The pressure view's math and its honesty properties.

Run: .venv/bin/python -m tests.test_pressure   (from threads-app/)

These are the properties that make the view mean what it claims. Each one was
made to FAIL before it was allowed to pass — a green nobody has forced to fail
is not evidence that anything is being checked.
"""

from __future__ import annotations

import math
import sys

from threads_tui.pressure import (
    LOG2_SATURATION,
    PressureRow,
    diverging_bar,
    log_offset,
    pressure_lines,
    relative_shares,
)

FAILURES: list[str] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    if ok:
        print(f"  ok   {name}")
    else:
        print(f"  FAIL {name}  {detail}")
        FAILURES.append(name)


def plain(text) -> str:
    return text.plain if hasattr(text, "plain") else str(text)


def main() -> int:
    print("pressure scale — anchoring")

    H = 20  # half-width

    # The anchor. This is the whole premise of band 1: on-pace is the centre,
    # not the bottom of a range.
    check("on-pace sits exactly at centre", log_offset(1.0, H) == 0, f"got {log_offset(1.0, H)}")

    # "half-pace a quarter left, double a quarter right" — a quarter of the FULL
    # width is half of the half-width.
    check(
        "half-pace lands a quarter of the full width left",
        log_offset(0.5, H) == -H // 2,
        f"got {log_offset(0.5, H)}, want {-H // 2}",
    )
    check(
        "double-pace lands a quarter of the full width right",
        log_offset(2.0, H) == H // 2,
        f"got {log_offset(2.0, H)}, want {H // 2}",
    )

    # Symmetry is the property that makes a log scale worth having: a factor of
    # k in either direction must deflect equally.
    for k in (1.25, 1.5, 2.0, 3.0):
        check(
            f"a factor of {k} deflects symmetrically",
            log_offset(k, H) == -log_offset(1 / k, H),
            f"{log_offset(k, H)} vs {log_offset(1 / k, H)}",
        )

    # End stops.
    sat = 2**LOG2_SATURATION
    check("saturates right at the end stop", log_offset(sat, H) == H)
    check("saturates left at the end stop", log_offset(1 / sat, H) == -H)
    check("beyond the end stop stays clamped", log_offset(sat * 100, H) == H)

    # Over-share. The daemon leaves pressure unclamped so that "burned more than
    # its share" reads as negative; that must pin left, not crash on log(<=0).
    check("zero pressure pins to the left end stop", log_offset(0.0, H) == -H)
    check("negative pressure pins to the left end stop", log_offset(-0.4, H) == -H)

    print("\nunranked is not a low value")

    W = 21
    unranked = plain(diverging_bar(None, W))
    tiny = plain(diverging_bar(0.30, W))
    on_pace = plain(diverging_bar(1.0, W))

    # THE defect this view exists to avoid. If these two render alike, a
    # missing measurement reads as a measured-low value.
    check("an unranked bar differs from a low-pressure bar", unranked != tiny)
    check("an unranked bar has no fill at all", "█" not in unranked, repr(unranked))
    check("a low-pressure bar does have fill", "█" in tiny, repr(tiny))
    check("every bar is exactly the requested width", len(unranked) == W == len(tiny))
    check("the centre line is drawn even when unranked", "│" in unranked)

    # On-pace is a centre line with no deflection either way.
    check("on-pace draws no fill", "█" not in on_pace, repr(on_pace))

    # Direction must be readable without reading a number: fill sits on the
    # correct side of centre.
    centre = W // 2
    ahead = plain(diverging_bar(2.0, W))
    behind = plain(diverging_bar(0.5, W))
    check("room deflects right", "█" in ahead[centre + 1 :] and "█" not in ahead[:centre])
    check("over-pace deflects left", "█" in behind[:centre] and "█" not in behind[centre + 1 :])

    print("\nrelative strip — the denominator")

    rows = [
        PressureRow(1, "alpha", 2.0, share=0.5),
        PressureRow(2, "beta", 1.0, share=0.25),
        PressureRow(3, "gamma", None, share=0.25),  # unranked
    ]
    shares = relative_shares(rows)
    check("unranked contributes nothing and gets None", shares["gamma"] is None)
    check(
        "ranked shares are normalised over ranked accounts only",
        abs(shares["alpha"] - 66.666) < 0.01 and abs(shares["beta"] - 33.333) < 0.01,
        f"{shares}",
    )
    check(
        "ranked shares sum to 100",
        abs(sum(v for v in shares.values() if v is not None) - 100.0) < 0.001,
    )

    # An over-share account does not compete for the next turn.
    neg = relative_shares([PressureRow(1, "a", 2.0), PressureRow(2, "b", -1.0)])
    check("an over-share account gets 0% of the next turn", neg["b"] == 0.0, f"{neg}")
    check("the remaining account gets all of it", abs(neg["a"] - 100.0) < 0.001)

    # The degenerate fleet: nobody has room. Must not divide by zero, and must
    # not silently promote anyone.
    dead = relative_shares([PressureRow(1, "a", 0.0), PressureRow(2, "b", -2.0)])
    check("a fleet with no positive pressure yields zeroes, not a crash", dead == {"a": 0.0, "b": 0.0}, f"{dead}")

    print("\nthe balanced-fleet problem band 1 exists to solve")

    # Ten equal accounts. The relative strip says 10% each in BOTH cases —
    # that is exactly why it cannot be the only band. Band 1 must tell them
    # apart.
    healthy = [PressureRow(i, f"a{i}", 1.0) for i in range(10)]
    crisis = [PressureRow(i, f"a{i}", 0.1) for i in range(10)]
    rel_healthy = relative_shares(healthy)
    rel_crisis = relative_shares(crisis)
    check(
        "the relative strip really cannot tell the two apart",
        [round(v, 6) for v in rel_healthy.values()] == [round(v, 6) for v in rel_crisis.values()],
        "if this fails the premise of the design changed",
    )
    check(
        "band 1 tells them apart",
        plain(diverging_bar(1.0, W)) != plain(diverging_bar(0.1, W)),
    )

    print("\nshare is shown, never assumed")

    lines = pressure_lines([PressureRow(1, "alpha", 1.0, share=None)], 80)
    body = "\n".join(plain(x) for x in lines)
    check("an unknown pool share renders as ?, not as 100%", "?" in body, body)
    check("unranked renders as the word", "unranked" in "\n".join(
        plain(x) for x in pressure_lines([PressureRow(1, "a", None)], 80)
    ))

    print("\nthe unavailable state is an explanation, not a board of zeroes")
    out = "\n".join(plain(x) for x in pressure_lines([], 80, unavailable="not on the wire yet"))
    check("the note is shown", "not on the wire yet" in out)
    check("no bars are drawn", "█" not in out and "▬" not in out)

    print()
    if FAILURES:
        print(f"{len(FAILURES)} FAILED: {', '.join(FAILURES)}")
        return 1
    print("all pressure checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
