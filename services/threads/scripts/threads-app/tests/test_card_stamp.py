"""**THE CARD STAMP SAYS WHY A READING IS OLD, NOT ONLY THAT IT IS.** (H179)

On 2026-10-15 the board's card for `alice-example-com` read `STALE 16d 14h`
and never moved. Every gauge axis carried observed_at 2026-09-29T04:04Z: the
daemon's hourly /usage probe for that account failed every hour because the
slot's own login was WIPED, so the daemon kept serving the last reading it had.

Two defects, one test each:

  (a) `Account.oldest_observed` took the MIN over axes. The daemon's own
      `last_observed_at` (budgets.rs) takes the newest, deliberately: one dead
      or unmapped axis must not pin a card STALE forever. The card now uses the
      newest axis; per-axis stamps are unchanged.
  (b) A slot that cannot be probed (wiped / expired / no-login / stranded) will
      never get a new reading, and a bare `STALE <age>` presents that as
      ordinary lateness. The stamp now names the login as the cause and keeps
      the age.
"""

from __future__ import annotations

from datetime import datetime, timedelta, timezone

from threads_tui.model import Account, AxisState
from threads_tui.render import _card_stamp

NOW = datetime(2026, 10, 15, 18, 4, tzinfo=timezone.utc)
OLD = NOW - timedelta(days=16, hours=14)


def _axis(name: str, at: datetime) -> AxisState:
    return AxisState(name=name, pct=40.0, observed_at=at, present=True)


def test_mixed_axis_ages_use_the_newest_reading():
    a = Account(number=1, label="bob-example-com", state="live")
    a.axes = {
        "weekly-all": _axis("weekly-all", NOW - timedelta(minutes=1)),
        "weekly-opus": _axis("weekly-opus", OLD),  # dead / unmapped axis
    }
    assert not a.is_stale_at(NOW)
    stamp = _card_stamp(a, NOW).plain
    assert "STALE" not in stamp, stamp
    assert "obs" in stamp and "16d" not in stamp, stamp


def test_wiped_slot_with_old_readings_names_the_login():
    a = Account(number=1, label="alice-example-com", state="wiped")
    a.axes = {
        "weekly-all": _axis("weekly-all", OLD),
        "session": _axis("session", OLD),
    }
    assert a.is_stale_at(NOW)
    stamp = _card_stamp(a, NOW).plain
    assert "login wiped" in stamp, stamp
    assert stamp.startswith("FROZEN"), stamp
    assert "16d 14h" in stamp, stamp  # the age stays visible
    assert not stamp.startswith("STALE"), stamp


def test_live_slot_with_old_readings_is_still_plain_stale():
    a = Account(number=1, label="carol-example-com", state="live")
    a.axes = {"weekly-all": _axis("weekly-all", OLD)}
    stamp = _card_stamp(a, NOW).plain
    assert stamp.startswith("STALE"), stamp
    assert "login" not in stamp, stamp
