"""**THE PER-ACCOUNT RUNNING COUNT: THE PLACEMENT IS THE KEY.**

His report, verbatim:

    "I have to click refresh to get my counts of active running on each account
     to update in the UI. that seems to contradict and needs to be resolved."

He was right that it contradicted. The header count had been live since
`live.py` was written — it moved on a pushed frame with no timer anywhere near
it — and the number beside each ACCOUNT did not move at all between reloads.
Two numbers on one screen, derived from the same frames, behaving differently.

**WHAT THIS FILE USED TO PIN, AND WHY NONE OF IT EXISTS ANY MORE.** It measured
two compounding defects in machinery that stage S3 has now deleted outright:

  1. THE PLACEMENT LOOKUP LIVELOCKED. `place_running` was a
     `@work(exclusive=True)` worker called on every frame that left anything
     unplaced; `exclusive=True` CANCELS the worker in flight, mid-`await`, and
     the replacement restarts from the TOP of the work list. Four busy threads
     beat at ~1 Hz each, so a `threads/{thread_id}/get` sharing the daemon's store mutex
     with `accounts/status` (219 ms, measured) never survived to its own
     `place()` call. Twelve frames produced fourteen requests, twelve of them
     for the same thread, and the tail was never asked at all.

  2. THE GATE WAS ALL-OR-NOTHING. One unresolved thread blanked the live number
     on EVERY card, and defect 1 guaranteed there was always one.

**S2 replaced that mechanism; S3 DELETED IT.** There is no placement worker, no
`threads/{thread_id}/get`, no client-side lease and no residual, because a thread's row
lives under the account it was placed on — so being counted and being placed
are ONE act, and the header total is the sum of the cards by construction. A
disagreement between the two is no longer a race to be reconciled; it is
arithmetically impossible.

WHAT THIS FILE PINS NOW:

  * the count is drawn from a shape export and from nothing else, with the five
    deleted queries still answered by the daemon and never asked;
  * a `liveness` beat — the frame that used to arm the whole machinery — moves
    nothing and arms nothing;
  * a shapes-less daemon draws NO BOARD and says why, which is the honest
    consequence of having one source;
  * the narration line still works, because shapes do not carry narration;
  * every surviving key binding still resolves to a handler.

The pure half runs under pytest at a frozen clock, with no daemon and no
sleeping. The socket half stands up a real WebSocket daemon, drives the real
`BoardApp` through Textual's pilot, and reads the characters painted into
`#body`. **`pilot.press` is never called in the placement scenario.** That is
the claim: the cards must go live off frames alone. If a future edit makes that
scenario need a keystroke, the feature is gone.

Run the socket half:  .venv/bin/python tests/test_place.py   (from threads-app/)
"""

from __future__ import annotations

import asyncio
import json
import os
import sys
import time

sys.path.insert(0, __file__.rsplit("/tests/", 1)[0])
sys.path.insert(0, __file__.rsplit("/test_place.py", 1)[0])

import fake_daemon  # noqa: E402

from threads_tui import feed as feed_mod  # noqa: E402
from threads_tui import render  # noqa: E402
from threads_tui.link import LinkState  # noqa: E402


# ═══════════════════════════════════════════════════════════════════════════
# THE PURE HALF — a frozen clock, no daemon, no sleeping.
#
# The seven `LiveThreads` tests that stood here are DELETED with the module
# they tested (`threads_tui/live.py`): the work list, the blocked set, the
# residual, the per-execution placement cache and the lease TTL were all one
# hand-written client-side fold, and the daemon's own fold answers every
# question they answered. A test kept alive for a deleted module is a test
# that pins nothing.
# ═══════════════════════════════════════════════════════════════════════════


# ── the subscription claim, which the firehose `sub_ack` used to make ────────
#
# Four tests stood here for `sub_ack`: that it scheduled a reseed, that a
# repeated one did not, that `firehose: false` did not, and that a reconnect
# reseeded again. They are DELETED WITH THE FIREHOSE: this board sends no
# `{"type": "subscribe"}`, so no `sub_ack` is ever sent to it, and the reseed
# they pinned was always the keyless subscribe `_on_caps` sends on every
# connection anyway. What survives is the CLAIM — `feed.subscribed`, now set by
# the narration export — and the one property of it that matters across a
# reconnect is pinned here.


def test_a_reconnect_must_be_re_proven() -> None:
    """`reset()` drops the claim, and only a NEW narration export restores it —
    so a reconnection that fails to re-subscribe stays visible as such."""
    f = feed_mod.Feed()
    assert f.narrating() is True and f.subscribed is True
    f.reset()
    assert f.subscribed is False
    assert f.narrating() is True, "the export after a reconnect is a transition again"
    assert f.pending() is False, "and, unlike `sub_ack`, it schedules nothing"


# ── the header ───────────────────────────────────────────────────────────────


def test_the_header_says_live_once_when_both_facts_agree() -> None:
    """**THE TWO "LIVE" WORDS COLLAPSE TO ONE.**

    `daemon_segment` (the socket) and `feed_segment` (the subscription) are
    genuinely different facts, and in the healthy case they are the same word
    twice. A duplicated signal does not make a failure more visible; it teaches
    the reader to skip both.
    """
    state = render.HeaderState(link=LinkState.LIVE, subscribed=True)
    text = render.link_segment(state).plain
    assert text.count("live") == 1, text
    assert "threads" in text


def test_the_header_splits_when_the_subscription_is_missing() -> None:
    """**AND SPLITS APART FOR THE FAILURE IT EXISTS TO CATCH.**

    A connection that is open, healthy, answering requests and never subscribed
    receives nothing forever while looking perfectly live. No link state can see
    it. Collapsing the words must not cost that, so the second word appears
    exactly when it carries information.
    """
    state = render.HeaderState(link=LinkState.LIVE, subscribed=False)
    text = render.link_segment(state).plain
    assert "NOT subscribed" in text, text
    assert "threads" in text


def test_a_dead_socket_does_not_say_it_twice() -> None:
    """With the socket down, `daemon_segment` has already said so in red.
    Appending "live updates stopped" would be the same duplication in a louder
    colour."""
    state = render.HeaderState(link=LinkState.OFFLINE, subscribed=False)
    text = render.link_segment(state).plain
    assert "offline" in text
    assert "live updates stopped" not in text, text


def test_quiet_time_is_not_staleness() -> None:
    """**THE SAME NUMBER MEANS OPPOSITE THINGS DEPENDING ON THE FEED.**

    With the board re-read on announcement rather than on a timer, four minutes
    since the last read means "the daemon has had nothing to tell us" when the
    feed is healthy and "this screen has been frozen for four minutes" when it
    is not. Escalating the colour on the first would turn a working idle system
    amber for doing nothing wrong.
    """
    healthy = render.last_update_segment(
        render.HeaderState(link=LinkState.LIVE, subscribed=True, refresh_age=240.0)
    )
    assert "STALE" not in healthy.plain, healthy.plain
    # NON-VACUOUS: the same render carries the reading, so a `STALE` prefix had a
    # line to sit on and is absent because the feed is healthy.
    assert "last update: 4m ago" in healthy.plain, healthy.plain

    unsubscribed = render.last_update_segment(
        render.HeaderState(link=LinkState.LIVE, subscribed=False, refresh_age=240.0)
    )
    assert "STALE" in unsubscribed.plain, unsubscribed.plain
    assert "last update: 4m ago" in unsubscribed.plain, unsubscribed.plain

    dropped = render.last_update_segment(
        render.HeaderState(link=LinkState.RECONNECTING, subscribed=False, refresh_age=240.0)
    )
    assert "STALE" in dropped.plain, dropped.plain

    # ── QUIET WHEN FRESH, LOUD WHEN STALE, AND THE WEIGHT IS THE LOUD PART ───
    #
    # The two renders above carry the SAME age and differ only in the feed. The
    # old segment escalated its colour and kept its weight, on a header where
    # every other word is muted — a difference he was not seeing. The healthy
    # reading now carries no `bold` at all and the stale one does, which is the
    # anti-wallpaper half of this change and is asserted on the STYLE because that
    # is where it lives.
    assert "bold" not in str(healthy.spans[0].style), healthy.spans
    assert "bold" in str(unsubscribed.spans[0].style), unsubscribed.spans


def test_the_countdown_is_gone() -> None:
    """A countdown to a refresh that no longer drives the board is a number the
    operator learns to watch instead of the ones that matter — and it is the
    interface still telling him refreshing is the mechanism."""
    board_state = render.HeaderState(link=LinkState.LIVE, subscribed=True, refresh_age=5.0)
    from threads_tui.model import Board
    from datetime import datetime, timezone

    line = render.header_line1(
        Board(), board_state, 200, datetime(2026, 9, 15, 12, 0, tzinfo=timezone.utc)
    ).plain
    assert "next refresh" not in line, line
    # ── AND THE KEY HINT IS GONE WITH IT (2026-10-06) ────────────────────────
    #
    # This line USED TO ASSERT `"R = poll accounts" in line`, which was correct
    # while `R` existed: the countdown went and the one real affordance stayed
    # advertised. `R` has since been removed outright, along with the hidden `r`
    # and `Api.gauge_probe` — the operator's forcing function, argued at length in
    # `tests/test_keys.py`. The header now names NO key.
    #
    # THESE TWO ABSENCES ARE NOT VACUOUS, and the assertion below them is what
    # makes that true: the same rendered line still carries its freshness report,
    # so this is a populated header being checked for two specific strings, not an
    # empty one that trivially lacks everything. `test_keys.py` additionally
    # proves the exact-set binding table, which is where a re-added key is caught.
    assert "R = poll accounts" not in line, line
    assert "poll accounts" not in line, line
    assert "r = re-read" not in line, line
    # The non-vacuity witness is now the ONE freshness reading the operator asked
    # for by name; `read … ago` was the string this used to look for and he struck
    # it (see `render.last_update_segment`).
    assert "last update:" in line, line


def test_the_residual_cannot_exist() -> None:
    """**WHAT REPLACED THE ALL-OR-NOTHING GATE, AND THEN REPLACED ITSELF.**

    The gate existed to stop a card reading `running 0` while the header read
    `1 running`. S2 solved that by RECONCILING rather than hiding: every card
    drew its live number and the header named the leftovers, `4 running ·
    1 placing…` beside cards summing to 3.

    S3 removes the leftovers themselves. The header total is
    `sum(card.activity.executing)` over rows the daemon placed under an account
    key, so there is no second number to reconcile and no field to carry one.
    This asserts the SEGMENT cannot draw a residual — not that the residual is
    currently zero, which is a much weaker claim and the one a leftover field
    would still satisfy.
    """
    clean = render.running_segment(
        render.HeaderState(running=4, running_live=True)
    ).plain
    # THIS USED TO BE `endswith("4 running")`, which was a PROXY for "nothing
    # trails the count". The proxy stopped working when the pool's queue depth
    # moved into this segment from the account cards, so it is replaced by the
    # thing it was standing in for: the segment is pinned WHOLE. That is strictly
    # stronger than the `endswith` was — an exact match cannot be satisfied by a
    # residual appended anywhere, including in the middle, which `endswith` could
    # not have said. Both pool facts are named here and nothing else may appear.
    assert clean.strip() == "▶ 4 running · ● 0 msgs queued", clean
    assert "placing" not in clean, clean
    assert "unplaced" not in clean, clean

    fields = render.HeaderState().__dataclass_fields__
    assert "unplaced" not in fields, (
        "a field that can only ever be 0 is a field the next person wires back "
        "up: the residual is removed, not pinned at zero"
    )
    assert "placement_blocked" not in fields, fields.keys()


def test_running_is_unknown_until_the_first_export() -> None:
    """**A CONFIDENT ZERO IS THE LIE THIS HEADER REFUSES.**

    Before an export has landed, 0 running means "nobody has told us" and not
    "nothing is running". It is the same three-valued honesty the cells carry,
    applied to the header's one number — and it matters more after S3, because
    there is no longer a polled number underneath it to fall back on.
    """
    unknown = render.running_segment(
        render.HeaderState(running=0, running_live=False)
    ).plain
    assert "running ?" in unknown, unknown

    known = render.running_segment(
        render.HeaderState(running=0, running_live=True)
    ).plain
    assert "0 running" in known, known



# The frozen clock for the pure-half renders below. The render layer takes `now`
# as a parameter everywhere (`tests/test_purity.py` enforces it), so a fixed
# instant here makes these assertions byte-stable.
from datetime import datetime as _dt, timezone as _tz  # noqa: E402

NOW_DT = _dt(2026, 10, 6, 12, 0, tzinfo=_tz.utc)


# ── QUEUE DEPTH IS A POOL FACT, AND `retrying` IS NOT `queued` ───────────────
#
# THREE DEFECTS, ONE CARD. The operator was looking at an account card that read
# `● queued 5` in its badge row while its own detail line, three rows below, read
# `0 queued`. Both numbers came from the same `Activity`:
#
#   * the badge drew `max(act.queued_threads, act.queued_messages)` — the LARGER
#     of a count of THREADS and a sum of MESSAGES, printed under the smaller
#     one's label. With three threads `retrying`, `queued_threads` was 0 and
#     `queued_messages` was 5, so `max(0, 5)` printed `queued 5` over work that
#     was not queued at all;
#   * `_badges` had NO `retrying` branch, though `Activity.retrying` has always
#     existed, so the one true word for those threads was unavailable;
#   * and the whole figure was attributed to an ACCOUNT. There is no per-account
#     queue and no pool-level one either: the only queue is a table keyed by
#     `thread_id`, and accounts are placement targets. The board derived the
#     per-card number itself by bucketing per-thread counts under each thread's
#     LAST account — which answers "where did this last run", not "where will it
#     run next", because placement is re-decided at admission.
#
# The operator: *"why would a specific account say queued at all? threads should
# have a queue for the pool not an individual account."* Asked whether a per-card
# count could be worded honestly — retrospectively, as `(last here)` — his answer
# was a flat no. So a card carries NO queue count in any wording, and the pool's
# depth is drawn once, in the header, in threads.
#
# EVERY ASSERTION BELOW IS ON THE RENDERED STRING, never on an intermediate dict:
# the rendered string is the thing he reads, and a dict can be right while the
# label over it is wrong — which is precisely the defect.


def _retrying_activity():
    """Three threads RETRYING, five messages queued behind them. The live shape.

    `queued_threads` is 0 because no thread is in state `queued`; `queued_messages`
    is 5. That pair is what made `max()` print `queued 5`, so the fixture has to
    carry both numbers or it cannot witness the bug.
    """
    from threads_tui.model import Activity

    return Activity(
        known=True,
        executing=0,
        queued_threads=0,
        queued_messages=5,
        retrying=3,
    )


def test_the_badge_calls_retrying_work_retrying() -> None:
    """**THE WORD.** `⟳ retrying 3`, and nothing about a queue.

    THE MUTATION. Put back `elif act.queued_threads > 0 or act.queued_messages > 0:
    parts.append(Text(f"● queued {max(act.queued_threads, act.queued_messages)}"))`
    and this goes red with the string he reported: `● queued 5`.
    """
    from threads_tui.model import Account

    account = Account(number=1, label="bob-example-com", state="live")
    account.activity = _retrying_activity()

    badges = render._badges(account, NOW_DT, None).plain
    assert "retrying 3" in badges, badges
    # THE NUMBER THAT MUST NOT APPEAR, and it is not vacuous: 5 is live in this
    # fixture as `queued_messages`, and `3` above proves this badge row does draw
    # activity counts. So a `queued 5` coming back is caught here.
    assert "queued" not in badges, badges
    assert "5" not in badges, badges


def test_a_card_carries_no_queue_count_in_any_wording() -> None:
    """**THE SCOPE.** Not the badge, not the detail line, not retrospectively.

    The card used to contradict itself — `queued 5` in the badge, `0 queued` in
    the detail line — and the fix is not to make the two agree on a number. The
    question the badge was answering (how deep is the queue?) is a POOL question
    and has moved to the header; the question the detail line was answering (how
    many threads last placed here are queued?) is one a card may not ask, because
    no wording makes a per-account queue figure true when there is no per-account
    queue. A reader seeing a number beside an account's name reads a commitment
    that the account will run that work.

    THE MUTATION. Restore `detail.append(f" · {act.queued_threads} queued")` — in
    any wording, including `(last here)` — and this goes red.
    """
    from threads_tui.model import Account

    account = Account(number=1, label="bob-example-com", state="live")
    account.activity = _retrying_activity()

    card = "\n".join(
        line.plain
        for line in render.card_lines(
            account, 120, selected=True, mode=render.FULL, warn_below=4.0,
            extras=None, flashing=None, now=NOW_DT,
        )
    )
    # NON-VACUOUS FIRST: the card genuinely drew its activity detail, so this is a
    # populated card being checked and not an empty render that lacks everything.
    assert "daemon:" in card, card
    assert "3 retrying" in card, card
    # AND THEN THE ABSENCE. The word `queued` appears nowhere on a card, which is
    # a stronger claim than "the two numbers agree" and is the one he asked for.
    assert "queued" not in card, [ln for ln in card.splitlines() if "queued" in ln]


def test_the_pool_queue_depth_is_in_the_header() -> None:
    """**WHERE THE NUMBER WENT.** Beside the pool-wide `running`, in THREADS.

    THE MUTATION. Drop the `queued` append from `running_segment` and this goes
    red; the number would then exist nowhere on the board, which is the other way
    to get this wrong.
    """
    segment = render.running_segment(
        render.HeaderState(running=2, queued=7, running_live=True)
    ).plain
    assert "2 running" in segment, segment
    # ── AND IT SAYS *MESSAGES*, WHICH IS WHAT `queued` MEANS ─────────────────
    #
    # He asked outright: *"what does queued mean if it's not parked. is it threads
    # with new messages queued to land in them?"* It is. The `queue` table is
    # `(id, thread_id, message_id, text, intent, state, ...)` with
    # `state='waiting'` — messages awaiting delivery into a thread. A bare
    # `7 queued` is ambiguous between seven threads and seven messages, and the
    # true reading is messages, so the unit is on the screen rather than in a
    # comment.
    assert "7 msgs queued" in segment, segment
    assert segment.strip() == "▶ 2 running · ● 7 msgs queued", segment


def test_the_header_queue_depth_is_unknown_until_the_first_export() -> None:
    """**A CONFIDENT ZERO IS THE LIE THIS HEADER ALREADY REFUSES FOR `running`.**

    Both numbers come off the same export, so there is no state in which one is
    trustworthy and the other is a guess. `queued 0` before anything has landed
    would assert an empty queue nobody has told us about.

    THE MUTATION. Gate `queued` on anything other than `running_live` — or draw it
    unconditionally — and this goes red.
    """
    unknown = render.running_segment(
        render.HeaderState(running=0, queued=0, running_live=False)
    ).plain
    assert "running ?" in unknown, unknown
    assert "msgs queued ?" in unknown, unknown
    assert "0 msgs queued" not in unknown, unknown


def test_the_pool_depth_is_the_sum_of_the_accounts_not_one_account_s_number() -> None:
    """**THE ARITHMETIC THAT MAKES IT A POOL FACT.**

    The header's number is the sum over every account's `queued_threads`. Two
    accounts holding 2 and 3 queued threads is a pool depth of 5 — and that 5 is
    not drawn against either card, which is the whole correction. A fixture with
    one account could not tell a sum from a copy, so there are two.
    """
    from threads_tui.model import Account, Activity, Board

    board = Board()
    for number, (label, queued) in enumerate(
        (("acct-a", 2), ("acct-b", 3)), start=1
    ):
        a = Account(number=number, label=label, state="live")
        # MESSAGES AS WELL AS THREADS, and deliberately a DIFFERENT total, so the
        # assertions below can tell which field was summed.
        a.activity = Activity(
            known=True, executing=0, queued_threads=queued, queued_messages=queued + 3
        )  # threads 2,3 (=5) and messages 5,6 (=11): two different totals
        board.accounts.append(a)

    # THE BOARD'S OWN METHOD, not a `sum()` written again here. A test that
    # re-implements the computation it is checking agrees with any mistake the
    # original makes — and the original mistake in this area was exactly a wrong
    # choice of input (`queued_messages` where `queued_threads` was meant), which
    # a duplicated correct `sum()` in the test would never have caught.
    depth = board.queue_depth()
    # 11 MESSAGES, NOT 5 THREADS. The two accounts hold 2 and 3 threads in state
    # `queued` and 5 and 6 waiting MESSAGES. The queue is a message table, so the
    # depth is 11 — and the fixture's two totals are deliberately different so
    # this assertion can tell which field was summed. Both numbers are true; only
    # one of them is what the word `queued` names.
    assert depth == 11, depth
    assert sum(a.activity.queued_threads for a in board.accounts) == 5
    assert depth != 5, "queue_depth is summing threads, not messages"
    segment = render.running_segment(
        render.HeaderState(running=0, queued=depth, running_live=True)
    ).plain
    assert "11 msgs queued" in segment, segment

    # AND NEITHER CARD CLAIMS ANY PART OF IT.
    for account in board.accounts:
        badges = render._badges(account, NOW_DT, None).plain
        assert "queued" not in badges, (account.label, badges)


def test_nothing_running_is_distinguished_from_nothing_can_run() -> None:
    """**`0 running` MEANT TWO OPPOSITE THINGS AND LOOKED THE SAME.**

    *"threads is still not displaying running threads so its impossible for me to
    know."* Once the producer's `executing`/`state` thrash was fixed, `0 running`
    became honest — nothing was running because the whole pool was barred — and
    was still useless, because a QUIET pool and a SHUT pool render identically and
    call for opposite responses.

    THE MUTATION. Delete the `state.placeable == 0` branch from `running_segment`
    and the shut pool goes back to reading exactly like the quiet one, which this
    test's final assertion catches.
    """
    shut = render.running_segment(
        render.HeaderState(running=0, queued=4, placeable=0, running_live=True)
    ).plain
    assert "no account can accept work" in shut, shut

    # THE QUIET POOL, which must NOT say it. Same `running=0`.
    quiet = render.running_segment(
        render.HeaderState(running=0, queued=4, placeable=3, running_live=True)
    ).plain
    assert "no account can accept work" not in quiet, quiet

    # AND THE TWO MUST NOT RENDER THE SAME, which is the whole complaint. This is
    # the assertion that cannot be satisfied by drawing the sentence never or
    # always — either of those makes these two strings equal.
    assert shut != quiet, (shut, quiet)

    # UNKNOWN IS NOT ZERO. `placeable=None` means nobody worked it out; claiming
    # the pool is shut on no information is the confident-zero mistake.
    unknown = render.running_segment(
        render.HeaderState(running=0, queued=4, placeable=None, running_live=True)
    ).plain
    assert "no account can accept work" not in unknown, unknown

    # AND IT IS NOT SAID WHILE A TURN IS IN FLIGHT: a pool running something is
    # demonstrably able to run something, whatever `placeable` says.
    busy = render.running_segment(
        render.HeaderState(running=2, queued=4, placeable=0, running_live=True)
    ).plain
    assert "no account can accept work" not in busy, busy


def test_placeable_counts_only_accounts_that_could_take_a_turn() -> None:
    """**THE THREE WAYS AN ACCOUNT IS NOT A CANDIDATE**, one account each.

    A disabled account, an account whose login store is wiped, and an account
    inside an observed limit window. Each is excluded for a different reason, and
    a fixture with only one of them could not tell a correct filter from one that
    happens to catch that case.

    THE MUTATION. Drop any one of the three conditions from `Board.placeable()`
    and the count goes to 2 instead of 1.
    """
    from threads_tui.model import Account, Activity, Board

    board = Board()

    ok = Account(number=1, label="can-run", state="live")
    ok.activity = Activity(known=True, available=True)

    off = Account(number=2, label="disabled", state="live", disabled=True)
    off.activity = Activity(known=True, available=True)

    broken = Account(number=3, label="wiped-login", state="wiped")
    broken.activity = Activity(known=True, available=True)

    barred = Account(number=4, label="rate-limited", state="live")
    barred.activity = Activity(known=True, available=False)

    board.accounts = [ok, off, broken, barred]
    assert board.placeable() == 1, [
        (a.label, a.disabled, a.state, a.activity.available) for a in board.accounts
    ]

    # AND A POOL IN WHICH EVERY ACCOUNT IS BARRED OR DISABLED COUNTS ZERO — the
    # state the operator was actually looking at.
    board.accounts = [off, broken, barred]
    assert board.placeable() == 0, board.placeable()


# ═══════════════════════════════════════════════════════════════════════════
# THE SOCKET HALF — a real daemon, the real app, the real painted pixels.
# ═══════════════════════════════════════════════════════════════════════════

FAILURES: list[str] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    if ok:
        print(f"  ok   {name}")
    else:
        print(f"  FAIL {name}  {detail}")
        FAILURES.append(name)


ACCOUNTS = ["acct0", "acct1", "acct2", "acct3"]
PLACEMENT = {"t0": "acct0", "t1": "acct1", "t2": "acct2", "t3": "acct3"}


def _daemon_module():
    """The shared `FakeDaemon`, reused rather than re-implemented.

    A second fake daemon would be a second wire contract, and the two would
    drift. This one is the one that has been checked against the live daemon —
    including its `GET /health` route.

    IT USED TO LIVE IN `test_refresh.py` and was imported from there. That file
    was deleted on 2026-10-06 with the `R` key it existed to test (the operator's
    forcing function — see `tests/test_keys.py`), and the fake moved into
    `fake_daemon.py`, which is where the wire contract already lives and which no
    runner collects. One test file importing another is how a deletion in one
    breaks the other; this indirection is kept only so the call sites below read
    unchanged.
    """
    return fake_daemon


class PoolDaemon:
    """FOUR accounts, a fold over them, and every deleted query still answered.

    **SINCE R2.S3c (R4.S9) IT REFUSES `threads/{thread_id}/get` AND THE FOUR
    RETIRED BOARD QUERIES, AS THE DAEMON DOES.** The claim is still that the
    board DOES NOT ASK. `self.methods` (every request, recorded at receipt,
    before routing) and `self.thread_get_calls` (likewise at receipt, via
    `fake_daemon.classify`) are the record; `fake_daemon.queries_asked` reads
    it — so an ask is caught whether or not it would have been answered.
    """

    def __init__(self, *, has_shapes: bool = True) -> None:
        tr = _daemon_module()
        self.base = tr.FakeDaemon()
        # ── THE FOLD. THE ONLY THING THAT PUTS A CARD ON THE SCREEN. ────────
        #
        # `has_shapes=False` is a daemon that answers `unknown method:
        # shapes/list` — the one running on this machine until the new binary
        # is promoted. After S3 that board draws NO CARDS, and
        # `scenario_a_shapes_less_daemon_says_so_out_loud` is what makes sure
        # it says why rather than looking like an empty pool.
        self.shape = fake_daemon.ShapeSurface(ACCOUNTS, has_shapes=has_shapes)
        self.thread_get_delay = 0.0
        self.thread_get_error: set[str] = set()
        self.thread_get_absent = False
        self.thread_get_calls: list[str] = []
        self.methods = self.base.methods
        # MUTABLE, because the daemon is growing a reloadable account pool and a
        # board that is open while an account is added or removed is now the
        # normal case rather than a restart-only one.
        self.roster = list(ACCOUNTS)
        self.health_ok = True
        self.health_calls = 0
        # Every frame the board sent — see `fake_daemon.FakeDaemon.received`.
        self.received: list[dict] = []
        self._server = None
        self._conns: set = set()

    # ── payloads ────────────────────────────────────────────────────────────
    def _payload(self, method, params):
        base = self.base
        if method in ("gauge/status", "gauge/probe"):
            out = base._payload(method, params)
            rows = out["accounts"]
            out["accounts"] = [dict(rows[0], account=a) for a in self.roster]
            return out
        return base._payload(method, params)

    # ── the socket ──────────────────────────────────────────────────────────
    def _process_request(self, connection, request):
        if request.path == "/health":
            from websockets.datastructures import Headers
            from websockets.http11 import Response

            self.health_calls += 1
            if not self.health_ok:
                return Response(503, "Service Unavailable",
                                Headers({"Content-Length": "0", "Connection": "close"}), b"")
            body = json.dumps({"status": "ok", "daemon": "threads",
                               "boot_id": "boot-test", "proto": 1}).encode()
            return Response(200, "OK", Headers({
                "Content-Type": "application/json",
                "Content-Length": str(len(body)),
                "Connection": "close"}), body)
        return None

    async def _handler(self, ws) -> None:
        self._conns.add(ws)
        try:
            await ws.send(json.dumps({
                "type": "welcome", "proto": 1, "daemon": "threads",
                "daemon_version": "test", "boot_id": "boot-test",
                "auth_required": False}))
            async for raw in ws:
                frame = json.loads(raw)
                self.received.append(frame)
                # No firehose is served: a `subscribe` frame gets no `sub_ack`.
                if frame.get("type") != "request":
                    continue
                method = frame.get("method")
                self.methods.append(method)
                # Classified, not compared: the thread id is in the path, so
                # there is no one string this could equal. Recorded at RECEIPT
                # (R4.S9): the method is refused now, so a record kept in its
                # payload branch could never be written.
                if fake_daemon.classify(method or "") == "threads/{thread_id}/get":
                    self.thread_get_calls.append(method.split("/")[1])
                    if self.thread_get_delay:
                        await asyncio.sleep(self.thread_get_delay)
                params = frame.get("params") or {}
                # ROUTE AS THE DAEMON ROUTES. A method name is a path:
                # `bind_subject` matches it to a template, lifts the subject out
                # of the path into the params for the handler, and REFUSES a
                # caller that also sent it in the params. An unroutable path is
                # a `KeyError`, rendered below as `unknown method:` — which is
                # also what an OMITTED subject now produces, because a path with
                # a hole in it is not a half-formed request, it is not a method.
                template = None
                try:
                    template, bound = fake_daemon.bind_subject(method, params)
                    if template.startswith("shapes/"):
                        payload = self.shape.payload(template, bound)
                    else:
                        payload = self._payload(template, bound)
                    outcome = {"type": "success", "payload": payload}
                except KeyError:
                    outcome = {"type": "error", "error": f"unknown method: {method}"}
                except ValueError as exc:
                    outcome = {"type": "error", "error": str(exc)}
                await ws.send(json.dumps({"type": "response",
                                          "correlate": frame.get("id"),
                                          "outcome": outcome}))
                # The exports FOLLOW the response — `{"queued": true}` means
                # the item is in the FIFO, not that the snapshot was taken.
                if template == fake_daemon.SUBSCRIBE and outcome["type"] == "success":
                    await self.shape.export_all(ws, bound)
        except Exception:
            pass
        finally:
            self._conns.discard(ws)

    # The shape-gated push, the narration helper and the raw push are
    # `fake_daemon.FakeDaemon`'s, bound here so both fakes gate identically.
    push = fake_daemon.FakeDaemon.push
    narrate = fake_daemon.FakeDaemon.narrate

    async def push_raw(self, frame: dict) -> None:
        for ws in list(self._conns):
            try:
                await ws.send(json.dumps(frame))
            except Exception:
                pass

    async def start(self, port: int):
        from websockets.asyncio.server import serve

        self._server = await serve(self._handler, "127.0.0.1", port,
                                   process_request=self._process_request)
        return self._server

    async def stop(self) -> None:
        if self._server is not None:
            self._server.close()
            try:
                await self._server.wait_closed()
            except Exception:
                pass


def _body(app) -> str:
    from textual.widgets import Static

    try:
        body = app.screen.query_one("#body", Static)
    except Exception:
        return ""
    return "\n".join(body.render_line(y).text for y in range(body.size.height))


def _card(app, label: str) -> str:
    return next((ln for ln in _body(app).splitlines() if f"· {label}" in ln), "")


def _header(app) -> str:
    text = _body(app)
    return text.splitlines()[0] if text else ""


async def _settle(pilot, seconds: float) -> None:
    end = asyncio.get_running_loop().time() + seconds
    while asyncio.get_running_loop().time() < end:
        await pilot.pause()
        await asyncio.sleep(0.05)


async def _boot(daemon: PoolDaemon):
    tr = _daemon_module()
    port = tr.free_port()
    os.environ["THREADS_TUI_WS_PORT"] = str(port)
    await daemon.start(port)
    from threads_tui.app import BoardApp

    return BoardApp()


# ── scenario 1: THE PLACEMENT IS THE KEY ─────────────────────────────────────


async def scenario_the_placement_is_the_key() -> None:
    """**THE CARD'S NUMBER, OFF A SHAPE EXPORT, WITH NOTHING ELSE INVOLVED.**

    This is the scenario the whole file is named for, rewritten for the thing
    that replaced the machinery it used to measure. Two threads are running on
    `acct1` and one on `acct3`, stated by the daemon's fold as rows under those
    keys. The board must draw `running 2` and `running 1` on those two cards,
    `2 + 1 = 3` on the header, and nothing on the other two — and it must do it
    with:

      * NO `threads/{thread_id}/get` — there is no question left to ask,
      * NONE of the five deleted board queries, all of which this daemon
        would have answered,
      * and NO KEYSTROKE.

    `pilot.press` is never called here. The cards go live off frames alone or
    the feature is gone.
    """
    print("\nthe placement is the key a thread's row lives under\n")
    daemon = PoolDaemon()
    # Since R2.S8 the placement rides `threads/non-idle` — one roster, each
    # entry naming its account — and not the account rows themselves.
    daemon.shape.placed["acct1"] = {
        "t-a": fake_daemon.thread_row(),
        "t-b": fake_daemon.thread_row(),
        "t-idle": fake_daemon.thread_row(state="queued", executing=False, queued=2),
    }
    daemon.shape.placed["acct3"] = {"t-c": fake_daemon.thread_row()}
    app = await _boot(daemon)

    async with app.run_test(size=(140, 60)) as pilot:
        await _settle(pilot, 4.0)

        check("the board was enumerated by ONE keyless subscribe",
              len(fake_daemon.subscribes_for(daemon.shape)) == 1
              and "key" not in fake_daemon.subscribes_for(daemon.shape)[0],
              f"{fake_daemon.subscribes_for(daemon.shape)}")
        check("all four accounts are drawn, counted from held keys",
              [a.label for a in app.board.accounts] == ACCOUNTS,
              f"{[a.label for a in app.board.accounts]}")

        # ── THE NUMBERS, PER ACCOUNT, OFF THE EXPORT ──
        by_label = {a.label: a.activity.executing for a in app.board.accounts}
        check("THE PER-ACCOUNT RUNNING COUNT IS CORRECT, FROM THE EXPORT ALONE",
              by_label == {"acct0": 0, "acct1": 2, "acct2": 0, "acct3": 1},
              f"{by_label}")
        check("and it is PAINTED on the right card",
              "running 2" in _card(app, "acct1"),
              f"acct1 card: {_card(app, 'acct1').strip()!r}")
        check("…and on the other one",
              "running 1" in _card(app, "acct3"),
              f"acct3 card: {_card(app, 'acct3').strip()!r}")
        check("an account with nothing on it wears no running badge",
              "running" not in _card(app, "acct0"),
              f"acct0 card: {_card(app, 'acct0').strip()!r}")
        check("THE HEADER IS THE SUM OF THE CARDS, not a second count",
              "3 running" in _header(app),
              f"header: {_header(app).strip()!r}")
        check("the queued messages are counted too, from the same rows",
              app.board.by_label("acct1").activity.queued_messages == 2,
              f"{app.board.by_label('acct1').activity.queued_messages}")

        # ── AND WHAT IT COST ──
        check("NO threads/{thread_id}/get WAS EVER ISSUED",
              daemon.thread_get_calls == [],
              f"thread_get_calls={daemon.thread_get_calls}")
        check("NONE of the five deleted board queries was asked",
              fake_daemon.queries_asked(daemon.methods) == [],
              f"asked: {fake_daemon.queries_asked(daemon.methods)}")

        # ── A ROW MOVES THE NUMBER, WITH NO KEYSTROKE ──
        daemon.shape.placed["acct1"] = {"t-a": fake_daemon.thread_row()}
        await daemon.push(fake_daemon.roster_live(daemon.shape.placed))
        await _settle(pilot, 1.5)
        check("A ROW LANDING MOVES THE CARD, WITH NO KEYSTROKE",
              "running 1" in _card(app, "acct1"),
              f"acct1 card: {_card(app, 'acct1').strip()!r}")
        check("and the header follows it down, because it is the same number",
              "2 running" in _header(app),
              f"header: {_header(app).strip()!r}")

        # ── THE FRAME THAT USED TO ARM EVERYTHING IS INERT ──
        #
        # A `liveness` beat says a thread is executing and says nothing about
        # where. It is exactly what `request_placement` listened for. No
        # firehose is subscribed any more, so it is forced onto the socket raw
        # — the frame really does arrive; asserting SILENCE is the only way to
        # tell "deleted" from "never provoked".
        # Compare the RUNNING SEGMENT, not the whole header: the reading-age
        # segment moves with the wall clock and would make this a test of the
        # passage of time.
        before = [a.activity.executing for a in app.board.accounts]
        await daemon.push_raw({"type": "liveness", "thread_id": "t-ghost",
                           "busy": True, "parent_running": True,
                           "running_subagents": []})
        await _settle(pilot, 1.5)
        after = [a.activity.executing for a in app.board.accounts]
        check("a liveness beat moves NO number",
              after == before and "2 running" in _header(app),
              f"{before} -> {after}; header {_header(app).strip()!r}")
        check("and arms no lookup",
              daemon.thread_get_calls == [],
              f"thread_get_calls={daemon.thread_get_calls}")
        check("and re-reads nothing",
              fake_daemon.queries_asked(daemon.methods) == [],
              f"asked: {fake_daemon.queries_asked(daemon.methods)}")

    await daemon.stop()


# ── scenario 2: THE HONEST CONSEQUENCE OF ONE SOURCE ─────────────────────────


async def scenario_a_shapes_less_daemon_says_so_out_loud() -> None:
    """**NO FOLD, NO BOARD — AND THE SCREEN SAYS WHICH NOTHING THAT IS.**

    This is the price of deleting the five queries, and it is paid in the open.
    A daemon that answers `unknown method: shapes/list` holds no fold, and this
    board no longer has a second way to draw itself. The failure mode being
    guarded against is not a crash: it is a board that is simply EMPTY, which
    is indistinguishable from a healthy board over an empty pool, and which
    would send the operator looking for a missing account instead of a missing
    daemon.

    So: an amber heading, the capability registry's own sentence, what it
    costs, and that the remedy is on the daemon side. Not red — nothing has
    failed and this is a version mismatch with a known end. Red is reserved for
    a fold that existed and DIED, which is terminal until the daemon restarts.
    """
    print("\na daemon with no folds: the board says so rather than going blank\n")
    from threads_tui import caps

    daemon = PoolDaemon(has_shapes=False)
    app = await _boot(daemon)

    async with app.run_test(size=(140, 60)) as pilot:
        await _settle(pilot, 4.0)
        body = _body(app)

        check("shapes/list was probed and recorded ABSENT",
              app.api.caps.state(caps.SHAPES_LIST) is caps.State.ABSENT,
              str(app.api.caps.state(caps.SHAPES_LIST)))
        # ROUTED, AND THIS LINE WAS A TRAP. A raw method is
        # `shapes/accounts/subscribe`; comparing it to the TEMPLATE string
        # `shapes/{shape}/subscribe` can never be true, so the mechanical
        # rename turned this into an assertion that passed no matter what the
        # board did. `fake_daemon.asked` routes, so it is a claim again.
        check("no shapes subscribe was ever sent to a daemon that has none",
              fake_daemon.asked(daemon.methods, fake_daemon.SUBSCRIBE) == [],
              f"{[m for m in daemon.methods if m.startswith('shapes')]}")
        check("THERE IS NO BOARD — and no fallback was attempted",
              app.board.accounts == []
              and fake_daemon.queries_asked(daemon.methods) == [],
              f"accounts={[a.label for a in app.board.accounts]} "
              f"asked={fake_daemon.queries_asked(daemon.methods)}")

        # THE SENTENCES. This is the deliverable: not a crash, not an empty
        # pool, and not a bare `?`.
        check("the screen says there is NO FOLD ON THIS DAEMON",
              "NO FOLD ON THIS DAEMON" in body,
              body[:400])
        check("it quotes the registry's own reason",
              "shapes/list is not on the wire" in body,
              body[:400])
        # The paragraph is WRAPPED to the terminal (see `render._wrapped`), so
        # a phrase is matched against the flattened text rather than against
        # one painted line.
        flat = " ".join(body.split())
        check("it says the board has no other way to draw itself",
              "no other way to draw itself" in flat,
              flat[:500])
        check("it names the five queries that were deleted, so the cost is legible",
              "accounts/status" in body and "accounts/health" in body,
              body[:600])
        check("it says the remedy is on the DAEMON side and needs no keystroke",
              "Promote a daemon" in body and "fills in by itself" in body,
              body[:600])
        check("and it is NOT dressed as a dead fold — nothing has failed",
              "FOLD DIED" not in body,
              "an absent fold and a dead one must not read the same")

        # AND IT IS DISTINGUISHABLE FROM AN EMPTY POOL, which is the whole
        # point: the same zero cards, a different sentence.
        from threads_tui import model, render
        from threads_tui.clock import now_utc

        empty = fake_daemon.board_of(now=now_utc())
        empty.source = model.SOURCE_HELD
        empty_text = "\n".join(t.plain for t in render.no_board_lines(empty, 100))
        check("an EMPTY POOL says something completely different",
              "holds no accounts" in empty_text
              and "NO FOLD" not in empty_text,
              empty_text)

        waiting = fake_daemon.board_of(now=now_utc())
        waiting_text = "\n".join(t.plain for t in render.no_board_lines(waiting, 100))
        check("and an UN-HYDRATED board says a third thing again",
              "waiting for the daemon's first export" in waiting_text
              and "holds no accounts" not in waiting_text,
              waiting_text)

    await daemon.stop()


# ── scenario 3: narration, which shapes do not carry ─────────────────────────


async def scenario_the_narration_line_survives() -> None:
    """**`feed.py` DID TWO JOBS AND ONLY ONE OF THEM WAS DELETED.**

    S3 removed its reload-scheduling role: an announcement no longer triggers a
    five-query re-read. It did NOT remove the narration — and since R2.S7 the
    narration arrives as LINES on the `narration` shape rather than as firehose
    frames, so this scenario drives it through `daemon.narrate`, which appends
    to the fold and publishes a transition. The operator asked for it by name:
    *"If I have it open, that means I want to see live stuff happening in there
    as it's happening."*

    A `progress` frame that is NOT done is the sharpest case: it narrates an
    operation still in flight and must schedule nothing at all.
    """
    print("\nthe daemon narrates, and the board still draws it\n")
    daemon = PoolDaemon()
    app = await _boot(daemon)

    async with app.run_test(size=(140, 60)) as pilot:
        await _settle(pilot, 4.0)
        subs_before = len(fake_daemon.subscribes_for(daemon.shape))

        await daemon.narrate("progress", op="rotate", id="r1",
                             account="acct2", phase="slot",
                             detail="rotating slot 3 of 4", done=False)
        await _settle(pilot, 1.5)
        body = _body(app)
        check("THE DAEMON'S OWN WORDS ARE ON THE SCREEN",
              "rotating slot 3 of 4" in body,
              [ln for ln in body.splitlines() if "rotat" in ln][:2])
        check("drawn as narration, not as a warning",
              any("⚡" in ln and "rotating slot 3 of 4" in ln
                  for ln in body.splitlines()),
              [ln for ln in body.splitlines() if "rotating" in ln][:2])
        check("an in-flight progress frame schedules NOTHING",
              len(fake_daemon.subscribes_for(daemon.shape)) == subs_before
              and fake_daemon.queries_asked(daemon.methods) == [],
              f"subscribes {subs_before} -> {len(fake_daemon.subscribes_for(daemon.shape))}")

        # A `lifecycle` frame narrates too, and this one is the daemon we are
        # already talking to — a replayed boot note, not a restart.
        await daemon.narrate("lifecycle", service="threads",
                             phase="back_up", reason="unknown",
                             detail="threads came back", boot_id="boot-test",
                             at="2026-09-15T03:57:19+00:00")
        await _settle(pilot, 1.5)
        check("a lifecycle note is narrated as well",
              "threads came back" in _body(app),
              [ln for ln in _body(app).splitlines() if "came back" in ln][:2])
        check("and the mirror survived it — a replayed boot note is not a restart",
              [a.label for a in app.board.accounts] == ACCOUNTS,
              f"{[a.label for a in app.board.accounts]}")

    await daemon.stop()


# ── scenario 4: `R` still probes, `r` re-exports, every binding resolves ─────


async def scenario_the_affordances_all_still_resolve() -> None:
    """**EVERY KEY THE BOARD OFFERS STILL LANDS ON A HANDLER.**

    A binding whose action was deleted with the mechanism under it does not
    error — Textual drops it silently, which is the worst failure this
    interface has (a dead account's row tells him to press a key, and nothing
    happens). S3 deleted `action_refresh`'s implementation and renamed it, so
    this walks the binding table and resolves every one of them.

    **NO KEY IS PRESSED HERE except `r`.** `n` and `l` start a login, `d`
    disables an account, `S` writes a share: pressing them to prove they are
    bound would be performing the operation. Resolution is what is asserted;
    the operations have their own files.
    """
    print("\nevery surviving affordance resolves, and r re-exports\n")
    daemon = PoolDaemon()
    app = await _boot(daemon)

    async with app.run_test(size=(140, 60)) as pilot:
        await _settle(pilot, 4.0)
        screen = app.screen

        # `"r": "resync"` AND `"R": "probe"` STOOD IN THIS MAP and are gone with
        # the keys themselves — the operator's forcing function, argued in full in
        # `tests/test_keys.py`, which pins the binding table as an EXACT set. This
        # map is deliberately NOT that exact set: its job is that every surviving
        # affordance resolves to a handler, and duplicating the exact-set
        # assertion here would mean two places to edit and one of them forgotten.
        wanted = {
            "q": "quit_app", "p": "pressure",
            "l": "login", "L": "login", "n": "new_account", "N": "new_account",
            "s": "set_live", "S": "share", "d": "disable", "D": "disable",
            "j": "down", "k": "up", "exclamation_mark": "drag_note",
        }
        bound = {b.key: b.action for b in type(screen).BINDINGS}
        missing = [k for k in wanted if k not in bound]
        wrong = {k: bound.get(k) for k, a in wanted.items()
                 if k in bound and bound[k] != a}
        check("every affordance is still BOUND",
              not missing and not wrong, f"missing={missing} wrong={wrong}")

        unresolved = [
            b.action for b in type(screen).BINDINGS
            if not hasattr(screen, f"action_{b.action.split('(')[0]}")
        ]
        check("and every binding RESOLVES to a handler on the screen",
              not unresolved, f"unresolved: {unresolved}")

        digits = [b.key for b in type(screen).BINDINGS
                  if b.action.startswith("select(")]
        check("the digits 1-9 still select a row by its printed number",
              digits == [str(n) for n in range(1, 10)], f"{digits}")
        # ── AND NEITHER REFRESH KEY EXISTS, AGAINST A REAL DAEMON ────────────
        #
        # The two blocks that stood here pressed `R` (asserting it ran
        # `gauge/probe`) and `r` (asserting it forced a re-subscribe). Both keys
        # are removed. What replaces them is the stronger claim, and it is made
        # against a LIVE socket rather than against the binding table: with EVERY
        # key on this board pressed, the daemon is never asked to poll an account
        # and never asked to re-subscribe. `test_keys.py` pins the table; this
        # pins the WIRE, which is the thing the table is only a proxy for.
        # ── WHAT THE SWEEP SKIPS, AND WHY IT IS NOT A HOLE ───────────────────
        #
        # `q` quits the app, which would end the scenario. `l`/`L` are the
        # terminal LOGIN HANDOFF: the handler calls `App.suspend` to drop out of
        # the TUI and hand the terminal to `claude login`, and Textual's pilot
        # raises `SuspendNotSupported` because there is no terminal to suspend.
        # Skipping them is an environment limit, not an exemption — both resolve
        # to the `login` action, which is asserted by NAME below, and the
        # exact-set binding table in `tests/test_keys.py` is what proves neither
        # is secretly a poll.
        SKIP = {"q", "l", "L"}
        for key in SKIP - {"q"}:
            check(f"the skipped key {key!r} is the login handoff, not a poll",
                  {b.action for b in type(screen).BINDINGS if b.key == key} == {"login"},
                  f"{key} -> "
                  f"{ {b.action for b in type(screen).BINDINGS if b.key == key} }")

        probes_before = daemon.base.probe_calls
        subs_before = len(fake_daemon.subscribes_for(daemon.shape))
        pressed = 0
        for key in sorted({b.key for b in type(screen).BINDINGS} - SKIP):
            await pilot.press(key)
            await _settle(pilot, 0.05)
            # Several of these open a modal; escape puts the board back so the
            # next key lands on the board and not on whatever the last one opened.
            await pilot.press("escape")
            await _settle(pilot, 0.05)
            pressed += 1
        await _settle(pilot, 1.0)
        check("NO KEY ON THIS BOARD POLLS AN ACCOUNT — gauge/probe is unreachable",
              daemon.base.probe_calls == probes_before,
              f"probe_calls {probes_before} -> {daemon.base.probe_calls}")
        check("and no key forces a re-subscribe either",
              len(fake_daemon.subscribes_for(daemon.shape)) == subs_before,
              f"subscribes {subs_before} -> {len(fake_daemon.subscribes_for(daemon.shape))}")
        check("and no key re-ran a retired account query",
              fake_daemon.queries_asked(daemon.methods) == [],
              f"asked: {fake_daemon.queries_asked(daemon.methods)}")
        # NON-VACUOUS: the sweep must actually have pressed something. A loop over
        # an empty binding set satisfies all three checks above in silence.
        check("and the sweep pressed a real number of keys",
              pressed >= 23, f"only pressed {pressed} keys")

    await daemon.stop()


# ── scenario 5: a dead fold is terminal, and `r` does not hide it ────────────


async def scenario_a_dead_fold_has_no_fallback_to_hide_behind() -> None:
    """**AT S2b, `r` ESCAPED A DEAD FOLD BY RE-READING THE FIVE QUERIES.**

    That fallback repainted the screen with fresh numbers and HID the death,
    which is the one outcome the three-valued unknown exists to prevent. It was
    accepted then because the five queries still existed and a frozen screen
    with no door is its own dishonesty.

    S3 removes the door by removing the room behind it, and that is an
    improvement rather than a loss. `r` now asks the daemon to re-subscribe;
    the daemon REFUSES a dead shape in its own words; the board shows that
    refusal verbatim and keeps the marked corpse on screen. Nothing can repaint
    it into looking healthy, because there is nothing left that could.
    """
    print("\na dead fold: terminal, and nothing can quietly repaint it\n")
    daemon = PoolDaemon()
    app = await _boot(daemon)

    async with app.run_test(size=(140, 60)) as pilot:
        await _settle(pilot, 4.0)
        check("the board is drawn from the fold", app.shapes.exported)

        daemon.shape.dead = "the shape task panicked"
        await daemon.push(fake_daemon.death("the shape task panicked"))
        await _settle(pilot, 1.5)

        body = _body(app)
        check("the board says the fold died", "FOLD DIED" in body, body[:400])
        check("and says what is on screen is not current", "NOT current" in body)
        check("the cards SURVIVE, marked — an emptied board looks like no accounts",
              all(a in body for a in ACCOUNTS) and body.count("FOLD DIED") >= 2,
              f"FOLD DIED x{body.count('FOLD DIED')}")

        # ── AND THERE IS NO LONGER A KEY TO TRY ──────────────────────────────
        #
        # This block used to press `r` and assert that the re-subscribe came back
        # with the daemon's own refusal rather than with a repainted healthy
        # board. With `r` removed the claim gets simpler and stronger: there is no
        # key to press at all, so nothing can even ATTEMPT to repaint a dead fold.
        # Every key is pressed and the corpse stays marked.
        asked_before = len(fake_daemon.queries_asked(daemon.methods))
        pressed = 0
        # Same exclusions as the affordance sweep, for the same reasons: `q` ends
        # the scenario and `l`/`L` suspend the terminal.
        for key in sorted({b.key for b in app.screen.BINDINGS} - {"q", "l", "L"}):
            await pilot.press(key)
            await _settle(pilot, 0.05)
            await pilot.press("escape")
            await _settle(pilot, 0.05)
            pressed += 1
        await _settle(pilot, 1.5)
        after = _body(app)
        check("NO KEY REPAINTS A DEAD FOLD — the corpse is still marked",
              "FOLD DIED" in after, after[:400])
        check("and nothing fell back to a retired query to do it",
              len(fake_daemon.queries_asked(daemon.methods)) == asked_before,
              f"asked: {fake_daemon.queries_asked(daemon.methods)}")
        check("and the board still says what is on screen is not current",
              "NOT current" in after)
        check("and the key sweep was real",
              pressed >= 23, f"only pressed {pressed} keys")

    await daemon.stop()


async def scenario_the_sparkline_is_drawn_off_the_fold() -> None:
    """**THE 7-DAY SPARKLINE IS A SHAPE NOW, AND A RECORDED READING DRAWS (R2.S3d).**

    It was the board's ONE remaining read: `gauge/history` for the selected
    card, re-asked whenever a `gauge` announcement cleared the cache. The series
    is `accounts/gauge-history` — keyed per account, subscribed once per card —
    so this scenario proves four things against a fake that would still answer
    the old request:

      1. the board subscribes the shape KEYED, once per account on screen;
      2. it NEVER sends `gauge/history`;
      3. the selected card's sparkline is drawn off the export;
      4. **the positive sentinel**: a reading pushed as a `transition` on the
         fold CHANGES THE SPARKLINE'S PIXEL COUNT — the number of drawn cells —
         with no request in between.

    **FORCED RED** (R2.S3d): (a) `_on_shape` stops routing
    `accounts/gauge-history` frames — the transition is dropped and check 4
    reads the same count twice; (b) `history_extras` returns
    `CardExtras(loaded=True)` without reading the fold — check 3 draws nothing.
    """
    print("\nthe sparkline is drawn off the accounts/gauge-history fold\n")
    from datetime import datetime, timedelta, timezone

    now = datetime.now(timezone.utc)

    def at(delta: timedelta) -> str:
        return (now - delta).isoformat()

    daemon = PoolDaemon()
    tr = _daemon_module()
    daemon.shape.history = {
        "acct0": [
            tr.history_reading(at(timedelta(days=6)), 10.0),
            tr.history_reading(at(timedelta(days=5)), 20.0),
            tr.history_reading(at(timedelta(days=4)), 30.0),
        ],
    }
    app = await _boot(daemon)

    def drawn() -> int:
        account = app.selected_account()
        extras = app.history_extras(account, datetime.now(timezone.utc))
        return sum(1 for cell in (extras.sparkline or []) if cell is not None)

    async with app.run_test(size=(140, 60)) as pilot:
        await _settle(pilot, 4.0)
        keyed = [s.get("key") for s in tr.subscribes_for(daemon.shape, tr.GAUGE_HISTORY)]
        check("THE WEEK IS SUBSCRIBED KEYED, ONCE PER ACCOUNT ON SCREEN",
              sorted(keyed) == sorted(a.label for a in app.board.accounts),
              f"keyed history subscribes: {keyed}")
        check("AND gauge/history IS NEVER ASKED",
              daemon.methods.count("gauge/history") == 0,
              f"methods: {daemon.methods}")
        check("the selected card is acct0, whose week the fake holds",
              app.selected_label() == "acct0", f"selected={app.selected_label()}")
        before = drawn()
        check("THE SPARKLINE IS DRAWN OFF THE EXPORT",
              before == 3, f"drawn cells: {before}")

        await daemon.push(tr.history_live(
            "acct0", tr.history_reading(at(timedelta(hours=1)), 55.0),
            at=at(timedelta(hours=1)),
        ))
        await _settle(pilot, 1.5)
        after = drawn()
        check("A READING APPENDED ON THE FOLD DRAWS — the pixel count moved",
              after == before + 1, f"drawn cells {before} -> {after}")

        # An announcement asks for nothing: the reading arrived on the fold.
        asked = len(daemon.methods)
        await daemon.narrate("info", domain="gauge", reason="usage_poll",
                             account="acct0", detail="recorded a fresh reading",
                             at="2026-09-15T12:00:00+00:00")
        await _settle(pilot, 2.5)
        check("a GAUGE announcement sends no request at all",
              daemon.methods[asked:] == [],
              f"sent after the announcement: {daemon.methods[asked:]}")
        check("and does not re-export every account key",
              len(tr.subscribes_for(daemon.shape)) == 1,
              f"{len(tr.subscribes_for(daemon.shape))} subscribes")

    await daemon.stop()


# ── scenario 6: the daemon dies, and the health route says so first ──────────


async def scenario_health_detects_a_dead_daemon() -> None:
    """**THE SOCKET IS STILL PERFECTLY OPEN.**

    That is the point of this scenario, and it is the failure the health check
    was added for: a daemon that stops serving without closing its socket is
    invisible to the link state machine for up to 40 seconds of ping timeout,
    plus a four-step reconnect ladder, before the down page can appear. Here the
    socket is never touched at all — only `GET /health` stops answering — and
    the page must still come up in seconds.
    """
    print("\nthe daemon stops answering /health with the socket still open\n")
    daemon = PoolDaemon()
    app = await _boot(daemon)

    async with app.run_test(size=(140, 60)) as pilot:
        await _settle(pilot, 4.0)
        check("the board is up and the link is live",
              not app._down_shown and app.api.state is LinkState.LIVE,
              f"down={app._down_shown} link={app.api.state}")
        check("the health route is actually being polled",
              daemon.health_calls >= 1, f"health_calls={daemon.health_calls}")

        daemon.health_ok = False
        start = time.monotonic()
        for _ in range(120):
            if app._down_shown:
                break
            await _settle(pilot, 0.25)
        elapsed = time.monotonic() - start

        check("THE DAEMON-DOWN PAGE CAME UP OFF THE HEALTH CHECK",
              app._down_shown, f"still on the board after {elapsed:.1f}s")
        check("and it did so in seconds, not in the socket's ~55",
              app._down_shown and elapsed < 20.0, f"took {elapsed:.1f}s")
        check("the socket never dropped — this was NOT the link state machine",
              app.api.state is LinkState.LIVE, f"link={app.api.state}")
        # THE COUNT IS NOT SEPARATELY DROPPED, and that is the simplification:
        # it lives in the mirror with everything else, and the down page is
        # mutually exclusive with the board, so nothing stale can be on screen.
        # Clearing the mirror HERE would be wrong — a failed `GET /health` is
        # not evidence about the socket, which may still be folding.
        check("the board's one source was not thrown away on an HTTP failure",
              app.shapes.exported,
              "the socket is still open; only the health route stopped")

        daemon.health_ok = True
        for _ in range(120):
            if not app._down_shown:
                break
            await _settle(pilot, 0.25)
        check("AND THE BOARD COMES BACK WHEN HEALTH DOES",
              not app._down_shown, "still on the down page")

    await daemon.stop()


# ── scenario: a restarted daemon ─────────────────────────────────────────────


async def scenario_a_new_boot_id_re_enumerates() -> None:
    """**A DIFFERENT DAEMON PROCESS IS A DIFFERENT FOLD.**

    Every row this board holds was computed by one fold on one connection. If
    the boot id changes, those rows describe a process that is gone, and
    carrying them across would show the dead daemon's answers wearing the new
    one's provenance — which is precisely the lie the three-valued state exists
    to prevent, arriving by the back door.

    The first `lifecycle` frame is NOT a restart. The daemon replays one to
    every new subscriber, carrying a boot note that can be hours old (measured
    against the live daemon), so only a CHANGE may reset anything.
    """
    print("\na lifecycle frame from a different daemon\n")
    daemon = PoolDaemon()
    daemon.shape.placed["acct1"] = {"t1": fake_daemon.thread_row()}
    app = await _boot(daemon)

    async with app.run_test(size=(140, 60)) as pilot:
        await _settle(pilot, 4.0)
        check("a thread is running on acct1", "running 1" in _card(app, "acct1"),
              f"{_card(app, 'acct1').strip()!r}")

        # **THE TELL IS `ShapeMirror.exports`**, which is monotonic and is
        # reset by `reset()` and by nothing else. A lifecycle frame is an
        # announcement like any other, so BOTH of these re-enumerate — counting
        # subscribes cannot tell them apart. Whether the MIRROR WAS THROWN AWAY
        # first can, and that is the actual claim.
        await _settle(pilot, 1.0)
        exports_before = app.shapes.exports
        await daemon.narrate("lifecycle", service="threads",
                             phase="back_up", reason="unknown",
                             detail="came back", boot_id="boot-test",
                             at="2026-09-15T03:57:19+00:00")
        await _settle(pilot, 2.0)
        check("THE REPLAYED BOOT NOTE IS NOT TREATED AS A RESTART",
              "running 1" in _card(app, "acct1")
              and app.shapes.exports > exports_before,
              f"exports {exports_before} -> {app.shapes.exports}: a counter that "
              "went backwards means the mirror was dropped on a frame that says "
              "nothing new")

        exports_mid = app.shapes.exports
        await daemon.narrate("lifecycle", service="threads",
                             phase="back_up", reason="restart",
                             detail="came back", boot_id="boot-DIFFERENT",
                             at="2026-09-15T12:00:00+00:00")
        await _settle(pilot, 2.5)
        check("BUT A DIFFERENT BOOT ID DROPS THE MIRROR AND RE-ENUMERATES",
              app.shapes.exports <= len(ACCOUNTS) < exports_mid
              and app._boot_id == "boot-DIFFERENT",
              f"exports {exports_mid} -> {app.shapes.exports} "
              f"(a reset restarts the count), boot={app._boot_id}")
        check("and the board came back from the new fold rather than going blank",
              [a.label for a in app.board.accounts] == ACCOUNTS,
              f"{[a.label for a in app.board.accounts]}")
        check("and it did NOT fall back to a five-query re-read to recover",
              fake_daemon.queries_asked(daemon.methods) == [],
              f"asked: {fake_daemon.queries_asked(daemon.methods)}")

    await daemon.stop()


# ── scenario: the subscription, and the enumeration that must not double ────


async def scenario_the_subscription_enumerates_exactly_once() -> None:
    """**ONE KEYLESS SUBSCRIBE PER CONNECTION, AND NO ANNOUNCEMENT IS A SECOND.**

    The firehose's `sub_ack` used to schedule an announcement before this
    board had subscribed to anything. The firehose is gone (R2.S7), but the
    race it exposed is not specific to it: any announcement that predates the
    first enumeration is covered by the keyless subscribe `_on_caps` arms, and
    acting on it as well asks the daemon to re-export every key, once per
    connection, for nothing — measured, which is why the `announced_at` check
    exists at all.
    """
    print("\nthe subscription enumerates once, not twice\n")
    daemon = PoolDaemon()
    app = await _boot(daemon)

    async with app.run_test(size=(140, 60)) as pilot:
        await _settle(pilot, 4.0)
        sent = [f.get("type") for f in daemon.received]
        check("the app sent NO firehose subscribe",
              "request" in sent and "subscribe" not in sent, f"{sent}")
        check("the header reports the feed as live",
              app.feed.subscribed and "live" in _header(app),
              _header(app).strip())
        check("EXACTLY ONE keyless subscribe went out on this connection",
              len(fake_daemon.subscribes_for(daemon.shape)) == 1
              and "key" not in fake_daemon.subscribes_for(daemon.shape)[0],
              f"{fake_daemon.subscribes_for(daemon.shape)}")
        check("and the board is drawn",
              [a.label for a in app.board.accounts] == ACCOUNTS,
              f"{[a.label for a in app.board.accounts]}")

        # ── THE RACE THAT WAS FOUND ON THE LIVE DAEMON, FORCED HERE ────────
        #
        # `sub_ack` arrived within milliseconds of the socket opening, while
        # the capability probe is still in flight, so `_enumerated_at` is still
        # None — and the old condition read that as "no evidence, so act". It
        # sent a `shapes/{shape}/subscribe` that raced ahead of the very probe deciding
        # whether to send one at all, and `_on_caps` then sent a second. The
        # live daemon reported `[gauge/history, shapes/{shape}/subscribe, shapes/list,
        # shapes/{shape}/subscribe]`.
        #
        # A fake daemon answers its probe too fast to lose that race by
        # accident, so the pre-probe state is restored by hand. Without this
        # the fix would be untested and the check above would stay green on
        # timing rather than on behaviour.
        app._enumerated_at = None
        subs_before = len(fake_daemon.subscribes_for(daemon.shape))
        await daemon.narrate("info", domain="accounts",
                             reason="pool_reloaded", detail="a burst")
        await _settle(pilot, 2.0)
        check("AN ANNOUNCEMENT BEFORE THE FIRST ENUMERATION SENDS NOTHING",
              len(fake_daemon.subscribes_for(daemon.shape)) == subs_before,
              f"subscribes {subs_before} -> {len(fake_daemon.subscribes_for(daemon.shape))}: "
              "`_on_caps` performs the first enumeration unconditionally, so "
              "there is nothing here to RE-enumerate")
        app._enumerated_at = time.monotonic()

        # A firehose `lagged` used to re-enumerate. The arm is deleted with the
        # firehose: this board subscribes none, so one arriving was not asked
        # for and moves nothing. Forced onto the socket raw to prove it.
        await daemon.push_raw({"type": "lagged", "dropped": 12})
        await _settle(pilot, 1.5)
        check("a firehose `lagged` is not a reason to re-enumerate any more",
              len(fake_daemon.subscribes_for(daemon.shape)) == subs_before
              and fake_daemon.queries_asked(daemon.methods) == [],
              f"subscribes={len(fake_daemon.subscribes_for(daemon.shape))} "
              f"asked={fake_daemon.queries_asked(daemon.methods)}")

    await daemon.stop()


async def scenario_the_pool_reloads_under_the_cursor() -> None:
    """**AN ACCOUNT GOES AWAY AND THE CURSOR DOES NOT MOVE.**

    The daemon is growing a reloadable account pool, so the roster can change
    without a daemon restart — and the board now re-reads itself on the
    announcement rather than on a five-minute timer, which means the operator is
    far more likely to be looking at it when it happens.

    `self.selected` is an INDEX. Held across a shrinking roster it silently
    lands on a DIFFERENT account, and every key on this board acts on the
    selection: `d` disables it, `S` sets its share, `L` signs it in. The
    selection is re-anchored by label instead.
    """
    print("\nan account disappears while the board is open\n")
    daemon = PoolDaemon()
    app = await _boot(daemon)

    async with app.run_test(size=(140, 60)) as pilot:
        await _settle(pilot, 4.0)
        app.select_number(3)  # the row printed as 3, which is acct2
        await _settle(pilot, 0.5)
        check("the cursor is on acct2", app.selected_label() == "acct2",
              f"selected={app.selected_label()}")

        # The pool loses an account ABOVE the cursor. The daemon's fold emits
        # a `gone` for that key — which is how a key leaves, and it is a
        # statement rather than an absence from a re-read — and announces the
        # domain move, which is what re-enumerates the rest.
        daemon.roster = ["acct0", "acct2", "acct3"]
        daemon.shape.keys = ["acct0", "acct2", "acct3"]
        await daemon.push(fake_daemon.live("acct1", {"kind": "gone"}))
        await daemon.narrate("info", domain="accounts",
                             reason="pool_changed", detail="acct1 removed",
                             at="2026-09-15T12:00:00+00:00")
        await _settle(pilot, 2.5)

        check("THE NEW ROSTER IS ON SCREEN WITH NO KEYSTROKE",
              [a.label for a in app.board.accounts] == ["acct0", "acct2", "acct3"],
              f"{[a.label for a in app.board.accounts]}")
        check("AND THE CURSOR IS STILL ON THE ACCOUNT HE PUT IT ON",
              app.selected_label() == "acct2",
              f"selected={app.selected_label()} — a held index would have moved "
              f"it to acct3, and d/S/L all act on the selection")

    await daemon.stop()


SCENARIOS = [
    scenario_the_placement_is_the_key,
    scenario_a_shapes_less_daemon_says_so_out_loud,
    scenario_the_narration_line_survives,
    scenario_the_affordances_all_still_resolve,
    scenario_a_dead_fold_has_no_fallback_to_hide_behind,
    scenario_the_sparkline_is_drawn_off_the_fold,
    scenario_health_detects_a_dead_daemon,
    scenario_a_new_boot_id_re_enumerates,
    scenario_the_subscription_enumerates_exactly_once,
    scenario_the_pool_reloads_under_the_cursor,
]


async def run_all() -> None:
    for scenario in SCENARIOS:
        await scenario()


def main() -> int:
    print(__doc__.strip().splitlines()[0])
    asyncio.run(run_all())
    print()
    if FAILURES:
        print(f"{len(FAILURES)} FAILED: {', '.join(FAILURES)}")
        return 1
    print("all placement checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
