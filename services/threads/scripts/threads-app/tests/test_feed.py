"""The BOARD re-reads because a frame arrived, not because a timer fired.

WHAT HE ASKED FOR. "The only updates I'm getting are when I trigger a refresh...
let's update the surface of the threads WebSocket broadcast system and just
subscribe to it from the... text user interface, so that we're just seeing the
live information as it's happening. If I have it open, that means I want to see
live stuff happening in there as it's happening. All information updates should
get relayed out as a broadcast and picked up by my text user interface."

THE TEST THAT WOULD NOT HAVE PROVEN IT — the same trap `test_live.py` names, and
it is worth restating because this board has already shipped one subscription
that was live in structure and polled in behaviour. A board that reloads every
300 seconds ends up showing the right numbers too. Any test that lets the clock
advance, or that calls `reload()` itself, cannot tell the live implementation
from the polled one, because both arrive at the same answer.

SO THIS FILE NEVER ADVANCES THE CLOCK EXCEPT TO PROVE THE COALESCE WINDOW, and
where it does, it says so. A re-read is scheduled at a frozen `at` because a
FRAME ARRIVED and for no other reason.

THE SECOND HALF, WHICH IS THE ONE HE ASKED FOR TWICE. A live subscription that
dies silently must be VISIBLE as dead. He is reporting a frozen screen right
now, and the reason it took a while to notice is that a frozen screen and a
quiet system look identical. So there are two failure cases here, not one:

  * the SOCKET drops     — the link state catches it, and the feed stops
                           claiming to be subscribed;
  * the socket LIVES but the subscription never took — the link state CANNOT
                           see this, the connection passes every ping, and
                           nothing is ever pushed. The `narration` EXPORT is
                           the only evidence (`Feed.narrating`), so its
                           absence is rendered.

THE INPUT IS A LINE OFF THE `narration` FOLD, NOT A FRAME OFF THE SOCKET. The
board used to read `info` / `progress` / `lifecycle` as firehose frames, tagged
on `type`; it now reads them as lines the `narration` shape appended, tagged on
`kind` (`lib/threads/src/shapes/narration.rs::Line`). The fixtures below are
lines, and `test_a_firehose_frame_is_not_a_line` pins that a frame handed in by
mistake announces nothing.

Run: .venv/bin/python tests/test_feed.py   (from threads-app/)
"""

from __future__ import annotations

import sys

sys.path.insert(0, __file__.rsplit("/tests/", 1)[0])

from datetime import timedelta  # noqa: E402

from threads_tui.feed import Feed  # noqa: E402
from threads_tui import render  # noqa: E402
from threads_tui.link import LinkState  # noqa: E402

FAILURES: list[str] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    if ok:
        print(f"  ok   {name}")
    else:
        print(f"  FAIL {name}  {detail}")
        FAILURES.append(name)


# ── the real line shapes, as the `narration` fold holds them ─────────────────
#
# Copied from `shapes/narration.rs`'s `Line`, which is `#[serde(tag = "kind",
# rename_all = "snake_case")]` — so the channel is the `kind` field and every
# field is the `Push` variant's own, verbatim. A fixture that drifts from that
# proves nothing.


def info(domain: str = "gauge", reason: str = "usage_poll", **kw) -> dict:
    frame = {
        "kind": "info",
        "domain": domain,
        "reason": reason,
        "at": "2026-09-01T22:30:00Z",
    }
    frame.update(kw)
    return frame


def progress(phase: str, done: bool = False, **kw) -> dict:
    frame = {
        "kind": "progress",
        "op": "rotate",
        "id": "run-1",
        "phase": phase,
        "done": done,
    }
    frame.update(kw)
    return frame


def lifecycle(phase: str = "back_up") -> dict:
    return {
        "kind": "lifecycle",
        "service": "threads",
        "phase": phase,
        "reason": "new_binary",
        "detail": "came back on a new binary",
        "boot_id": "b-1",
        "at": "2026-09-01T22:30:00Z",
    }


def as_firehose_frame(line: dict) -> dict:
    """The same fact as the LEGACY wire carried it: tagged on `type`, not `kind`."""
    frame = {k: v for k, v in line.items() if k != "kind"}
    frame["type"] = line["kind"]
    return frame


# ── the live path ────────────────────────────────────────────────────────────


def test_one_info_frame_schedules_a_reread() -> None:
    print("\nTHE CLOCK NEVER MOVES. One frame schedules the re-read.")
    T = 1000.0
    feed = Feed()

    check("nothing is scheduled before any frame", not feed.pending())
    check("and no re-read is due", not feed.due(at=T))

    repaint = feed.on_line(info(), at=T)
    check("ONE pushed `info` frame schedules a re-read, with no time passing", feed.pending())
    check("and it asks for an immediate repaint (the note line moves now)", repaint)
    check(
        "the re-read is NOT due yet — it coalesces first",
        not feed.due(at=T),
        "firing on the frame itself would issue one reload per reading",
    )
    check("it becomes due after the coalesce window", feed.due(at=T + feed.coalesce))


def test_a_burst_is_one_reload() -> None:
    print("\nA twelve-frame probe is ONE reload, not twelve")
    T = 2000.0
    feed = Feed()
    # Four logins x three axes, exactly what `gauge/probe` emits.
    for account in ("cc1", "gmail", "sam", "broomstick"):
        for axis in ("session", "weekly_all", "weekly_fable"):
            feed.on_line(info(reason="usage_poll", account=account, detail=axis), at=T)

    check("the whole burst schedules one re-read", feed.pending())
    check(
        "and it is due at ONE coalesce window after the FIRST frame, not the last",
        feed.due(at=T + feed.coalesce),
    )
    domains = feed.take(at=T + feed.coalesce)
    check("the claim yields the announced domain", domains == {"gauge"}, f"got {domains}")
    check("and nothing is left scheduled", not feed.pending())


def test_an_announcement_during_a_reload_is_not_swallowed() -> None:
    print("\nA frame that lands DURING a re-read schedules another one")
    T = 3000.0
    feed = Feed()
    feed.on_line(info(), at=T)
    feed.take(at=T)  # the reload starts here and will read the table now

    # The daemon writes a new observation a moment later. If this were folded
    # into the reload already in flight, the board would settle one update
    # behind while the header said it had just refreshed.
    feed.on_line(info(reason="rate_limit_event"), at=T + 0.1)
    check("the later frame schedules its own re-read", feed.pending())
    check("which comes due on its own window", feed.due(at=T + 0.1 + feed.coalesce))


def test_only_the_terminal_progress_frame_rereads() -> None:
    print("\nA rotation narrates while it runs and is re-read when it finishes")
    T = 4000.0
    feed = Feed()

    repaint = feed.on_line(progress("slot_2", detail="refreshing slot 2 of 4"), at=T)
    check("an in-flight phase repaints the narration line", repaint)
    check(
        "but does NOT schedule a re-read — the pool is half-rotated",
        not feed.pending(),
        "re-reading mid-rotation shows a partial state as though it were the outcome",
    )
    check("and the line says what is happening", feed.note(at=T) == "refreshing slot 2 of 4")

    feed.on_line(progress("finished", done=True), at=T)
    check("the TERMINAL frame schedules the re-read", feed.pending())


def test_lifecycle_and_unknown_domains() -> None:
    print("\nThe daemon restarting, and a domain this client has never heard of")
    T = 5000.0

    feed = Feed()
    feed.on_line(lifecycle("back_up"), at=T)
    check("a lifecycle frame schedules a re-read", feed.pending())
    check("and narrates it", feed.note(at=T) == "came back on a new binary")

    feed2 = Feed()
    feed2.on_line(info(domain="something_new_in_2027", reason="who_knows"), at=T)
    check(
        "an UNKNOWN domain still schedules a re-read rather than being dropped",
        feed2.pending(),
        "the honest answer to 'something you display changed' is to re-read",
    )
    check(
        "and the note falls back to the machine tag when there is no detail",
        feed2.note(at=T) == "who_knows",
        f"got {feed2.note(at=T)!r}",
    )


def test_thread_frames_do_not_reload_the_board() -> None:
    print("\nHeartbeats do NOT re-read the board — that would be a heavier poll")
    T = 6000.0
    feed = Feed()
    for junk in (
        {"kind": "liveness", "thread_id": "t-a", "busy": True},
        {"kind": "status", "thread_id": "t-a", "executing": True},
        {"kind": "event", "thread_id": "t-a", "seq": 1, "event": {}},
        {"kind": "welcome", "proto": 1},
        {"kind": "response", "correlate": "x", "outcome": {}},
        "not a dict",
    ):
        feed.on_line(junk, at=T)  # type: ignore[arg-type]
    check(
        "a 1 Hz liveness beat does not schedule a board reload",
        not feed.pending(),
        "reloading on a heartbeat is a poll far heavier than the 300s one",
    )


# ── `last update:` — THE ONE FRESHNESS READING, AND WHAT IT MEASURES ─────────
#
# The segment was `refresh_segment` and drew `read 42s ago`. The operator struck
# that string and the `usage readings:` string beside it and named the
# replacement: *"You could have last update: <time ago> so we can get a sense of
# updated shapes but that's it."*
#
# THESE CHECKS ARE ABOUT WHICH CLOCK IT IS, not about the words. `refresh_age`
# comes from `Feed.refresh_age` — the arrival of a SHAPE FRAME in this process —
# and the rejected alternative was the newest `updated_at` across the account
# rows, which is produced by the daemon and is true whether or not this client is
# still receiving anything. The checks below pin the client-side reading by
# driving `Feed` and nothing else: if the segment were ever re-sourced from a
# producer field, `feed.refreshed()` would stop moving the number and this goes
# red. See `render.last_update_segment`.


def test_last_update_measures_frame_arrival_not_reading_age() -> None:
    print("\n`last update:` measures when a frame ARRIVED HERE, not the readings")
    feed = Feed()
    check("never updated reports None, not zero", feed.refresh_age(at=100.0) is None)
    feed.refreshed(at=100.0)
    check("straight after a frame the age is 0", feed.refresh_age(at=100.0) == 0.0)
    check("and it counts up", feed.refresh_age(at=142.0) == 42.0)

    # THE CLIENT CLOCK IS THE ONLY INPUT. Nothing but `feed.refreshed()` has been
    # called, and the rendered string carries its number.
    state = render.HeaderState(
        link=LinkState.LIVE, subscribed=True, refresh_age=feed.refresh_age(at=142.0)
    )
    seg = render.last_update_segment(state).plain
    check("the header calls it `last update:`", "last update:" in seg, f"got {seg!r}")
    check("and reports the age the FEED gave it", "42s" in seg, f"got {seg!r}")
    # THE TWO STRINGS HE STRUCK, each absence paired with proof the needle is
    # producible in THIS render: `seg` is a populated freshness reading carrying
    # an age in the same grammar, so `read … ago` and `usage readings:` would have
    # fitted here exactly as they used to. They are gone because they were
    # removed, not because this segment has nothing to say.
    check("the struck `read <time> ago` is gone", "read " not in seg, f"got {seg!r}")
    check("and `usage readings:` with it", "usage readings" not in seg, f"got {seg!r}")
    check("it does not call it a reading at all", "reading" not in seg, f"got {seg!r}")

    fresh = render.last_update_segment(
        render.HeaderState(link=LinkState.LIVE, subscribed=True, refresh_age=0.4)
    ).plain
    check("a frame that just landed reads as 'just now'", "just now" in fresh, f"got {fresh!r}")

    never = render.last_update_segment(
        render.HeaderState(link=LinkState.LIVE, subscribed=True)
    ).plain
    check("and before the first frame it says so", "none yet" in never, f"got {never!r}")
    check(
        "'none yet' is a WORD, never a small number — nothing has arrived",
        "0s" not in never and "ago" not in never,
        f"got {never!r}",
    )


def test_the_header_carries_exactly_one_freshness_reading() -> None:
    """**TWO READINGS WENT, ONE CAME BACK, AND `freshness_segment` IS DELETED.**

    *"let's remove the read <time> ago and the usage readings: <time ago>. They
    don't help me."* — so the function that drew the second one no longer exists,
    and the whole header line is checked rather than one segment, because what he
    reads is the line.
    """
    print("\nOne freshness reading in the header, not two")
    from datetime import datetime, timezone

    from threads_tui.model import Account, AxisState, Board

    now = datetime(2026, 10, 7, 12, 0, tzinfo=timezone.utc)
    board = Board()
    # A GAUGED ACCOUNT WITH A DELIBERATELY ANCIENT OBSERVATION. This is exactly
    # the input `freshness_segment` existed to report on: it would have drawn
    # `usage readings: oldest 2d 00h old` off this axis. The header must not.
    old = Account(number=1, label="spare-example-com", state="live")
    old.axes = {
        "weekly_all": AxisState(
            name="weekly_all",
            pct=12.0,
            observed_at=now - timedelta(days=2),
            calibrated=True,
            present=True,
        )
    }
    board.accounts = [old]

    line = render.header_line1(
        board,
        render.HeaderState(link=LinkState.LIVE, subscribed=True, refresh_age=42.0),
        200,
        now,
    ).plain
    check("the line carries `last update:`", "last update:" in line, f"got {line!r}")
    check("exactly once", line.count("last update:") == 1, f"got {line!r}")
    # NON-VACUOUS BY CONSTRUCTION: the board handed in HAS a two-day-old usage
    # observation, so the struck string had its input available in this very
    # render and still does not appear.
    check(
        "the pooled usage-reading age is gone from the header",
        "usage readings" not in line,
        f"got {line!r}",
    )
    check(
        "and `2d` — the age it would have reported — is nowhere on the line",
        "2d" not in line,
        f"got {line!r}",
    )
    check(
        "`freshness_segment` is deleted, not merely unused",
        not hasattr(render, "freshness_segment"),
        "a function nothing calls is a function the next person wires back up",
    )
    check(
        "and so is the old `refresh_segment` name",
        not hasattr(render, "refresh_segment"),
        "two names for one segment is how a second one comes back",
    )


def test_a_running_refresh_says_so() -> None:
    print("\nA refresh in progress is announced — his second complaint")
    running = render.last_update_segment(
        render.HeaderState(link=LinkState.LIVE, subscribed=True, refreshing=True, refresh_age=5.0)
    ).plain
    check("a running reload says 'refreshing'", "refreshing" in running, f"got {running!r}")
    # THE FORCED-PROBE HALF OF THIS CHECK IS GONE. It asserted `probing all
    # accounts 42s` for the `R` key's pool-wide `claude /usage`. `R` is removed —
    # the operator's forcing function, argued in `tests/test_keys.py` — so there
    # is no such act to narrate and no `probing`/`probe_elapsed` on `HeaderState`.
    # `refreshing` SURVIVES and is still checked above: it is the post-mutation
    # settle (`resync`), which is caused by an operator ACT with a known expected
    # effect, not by a key pressed out of doubt about the screen.
    check(
        "and it does not show a stale age at the same time",
        "ago" not in running,
        "showing 'updated 5s ago' beside 'refreshing' is two answers to one question",
    )


# ── N >= 2: a live feed and a dead one ───────────────────────────────────────


def test_the_dead_feed_is_visible() -> None:
    print("\nN>=2 — a LIVE feed, and the two ways it dies, all three distinguishable")

    live = render.feed_segment(
        render.HeaderState(link=LinkState.LIVE, subscribed=True)
    ).plain
    check("a live, subscribed feed reads as live", "live" in live, f"got {live!r}")
    check("and does not shout", "NOT" not in live, f"got {live!r}")

    # FAILURE 1 — the socket dropped. The link state can see this.
    dropped = render.feed_segment(
        render.HeaderState(link=LinkState.OFFLINE, subscribed=False)
    ).plain
    check(
        "a dropped socket says live updates STOPPED",
        "stopped" in dropped,
        f"got {dropped!r}",
    )

    # FAILURE 2 — the socket is fine and the subscription never took. THIS is
    # the one that looks like a quiet system, and the link state cannot see it.
    unsub = render.feed_segment(
        render.HeaderState(link=LinkState.LIVE, subscribed=False)
    ).plain
    check(
        "a connected-but-unsubscribed feed is called out, not passed off as live",
        "NOT subscribed" in unsub,
        f"got {unsub!r}",
    )
    check(
        "the three states are all different text",
        len({live, dropped, unsub}) == 3,
        f"{live!r} / {dropped!r} / {unsub!r}",
    )


def test_the_narration_export_is_what_proves_the_subscription() -> None:
    print("\nOnly the narration EXPORT sets `subscribed` — not a line, not a frame")
    T = 7000.0
    feed = Feed()
    check("a fresh feed does not claim to be subscribed", not feed.subscribed)

    feed.on_line(info(), at=T)
    check(
        "and an `info` line does not retroactively prove it either",
        not feed.subscribed,
        "inferring the subscription from traffic would make silence unprovable",
    )

    changed = feed.narrating()
    check("the narration export proves it", feed.subscribed)
    check("and says the header must change", changed)
    check("a second export changes nothing on the header", not feed.narrating())

    quiet = Feed()
    quiet.narrating()
    check(
        "and the export schedules NO re-enumeration — it is an answer, not an ask",
        quiet.subscribed and not quiet.pending(),
        "the firehose `sub_ack` scheduled one; the export answers `_on_caps`'s own",
    )


def test_a_firehose_frame_is_not_a_line() -> None:
    print("\nA firehose FRAME handed to the feed announces nothing")
    T = 7500.0
    # N >= 2, and the positive half first: the SAME fact as a line DOES
    # announce, so the absence below is the frame's tag and not a dead feed.
    as_line = Feed()
    check("a `lifecycle` LINE narrates", as_line.on_line(lifecycle(), at=T))
    check("and is on the note", as_line.note(at=T) == "came back on a new binary")

    for line in (lifecycle(), info(), progress("finished", done=True)):
        feed = Feed()
        frame = as_firehose_frame(line)
        check(
            f"a firehose `{frame['type']}` frame does not repaint",
            not feed.on_line(frame, at=T),
        )
        check(
            f"nor narrate (`{frame['type']}`)",
            feed.note(at=T) == "",
            f"got {feed.note(at=T)!r}",
        )
        check(f"nor schedule (`{frame['type']}`)", not feed.pending())


def test_the_socket_dying_clears_the_claim_but_keeps_the_clock() -> None:
    print("\nWhen the socket dies: the claim goes, the refresh clock stays")
    T = 8000.0
    feed = Feed()
    feed.narrating()
    feed.refreshed(at=T)
    feed.on_line(info(), at=T)

    feed.reset()
    check("the subscription claim is dropped", not feed.subscribed)
    check("and nothing stays scheduled on a socket that is gone", not feed.pending())
    check(
        "but the refresh clock SURVIVES and keeps counting",
        feed.refresh_age(at=T + 300.0) == 300.0,
        "the board is still showing data fetched then, and its age is what matters most now",
    )


def test_the_narration_line_ages_out() -> None:
    print("\nThe narration line expires rather than becoming a claim about now")
    T = 9000.0
    feed = Feed()
    feed.on_line(lifecycle(), at=T)
    check("it is shown while fresh", feed.note(at=T + 5.0) != "")
    check("and gone once stale", feed.note(at=T + 31.0) == "")


def test_zz_every_check_in_this_module_actually_held() -> None:
    """**THE GUARD THAT MAKES EVERY `check()` ABOVE MEAN SOMETHING UNDER PYTEST.**

    `check()` RECORDS a failure and returns. That is right for the standalone run,
    where `main()` prints every result and then exits non-zero on `FAILURES` — but
    this module is collected by `pytest`, and under pytest a test function that
    records and returns is a test function that PASSES. Every check in this file
    was therefore green under pytest no matter what it found, and pytest is the
    only tier that runs this file at all.

    That was found by trying to force the new `last update:` checks red: three
    separate mutations to `render.last_update_segment` — restoring `read 4m ago`,
    restoring the pooled `usage readings:` age, and drawing `0s ago` where nothing
    has arrived — all left `pytest tests/test_feed.py` GREEN while the printed
    output said FAIL. A proof that cannot fail is not a proof, and this file had
    thirty of them.

    THE FIX IS A GUARD RATHER THAN A REWRITE of `check`, because the
    print-everything-then-summarise behaviour is what makes the standalone run
    readable, and raising on the first failure would lose it. pytest runs the
    functions in this module in DEFINITION ORDER, so this one — named to sort and
    to be written last — sees everything the others recorded.

    THE MUTATION. Break any check above (`"last update:" in seg`, say) and this
    goes red naming it.

    THE LIMIT, stated so it is not mistaken for more than it is: running a single
    test by node id does not run this guard. A full-module or full-suite run does,
    which is both tiers.
    """
    assert FAILURES == [], FAILURES


def main() -> int:
    print("the board is driven by pushed announcements, at a frozen clock")
    test_one_info_frame_schedules_a_reread()
    test_a_burst_is_one_reload()
    test_an_announcement_during_a_reload_is_not_swallowed()
    test_only_the_terminal_progress_frame_rereads()
    test_lifecycle_and_unknown_domains()
    test_thread_frames_do_not_reload_the_board()
    test_last_update_measures_frame_arrival_not_reading_age()
    test_the_header_carries_exactly_one_freshness_reading()
    test_a_running_refresh_says_so()
    test_the_dead_feed_is_visible()
    test_the_narration_export_is_what_proves_the_subscription()
    test_a_firehose_frame_is_not_a_line()
    test_the_socket_dying_clears_the_claim_but_keeps_the_clock()
    test_the_narration_line_ages_out()
    print()
    if FAILURES:
        print(f"{len(FAILURES)} FAILED: {', '.join(FAILURES)}")
        return 1
    print("all live-feed checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
