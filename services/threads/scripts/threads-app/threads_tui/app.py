"""The board.

Structure: one API client, one shape mirror, one pure render pass. The app
holds a `Board`, rebuilds it from the mirror on every paint, and re-renders it;
it does not compute anything the daemon already knows.

Two things here are worth reading before changing them.

THE VERDICT WAIT. After an operation that changes an account's state, the board
is brought up to date and then a verdict is shown. The old app waited 8 seconds
for that — while the health call inside it was allowed 30. So a health call
taking 9 to 30 seconds let the verdict panel read the PREVIOUS board and report
the pre-operation state: a successful re-login shown as failed, and a failed one
shown as fine. Wrong in both directions, which is the worst kind of wrong,
because neither answer can be trusted enough to act on.

The fix is not a bigger number. It is that the wait is DERIVED from the timeout
of the call it waits on, so the two cannot drift apart again, plus the verdict
states plainly when it timed out rather than silently reading whatever it has.
A verdict that says "I don't know yet" is useful; a confident wrong one is not.

**AND THE SAME DEFECT HAD A SECOND DOOR, WHICH S3 CLOSED.** `resync()` asks the
daemon to re-export and the daemon answers `{"queued": true}` — the subscribe
item is in its FIFO, the snapshot has NOT been taken. A verdict that read the
board on the strength of that response would be reading pre-operation state
again, by a new route. So the ask and the wait are ONE function: `resync()`
waits for the export counter to move and returns whether it did, and every
caller says which. Splitting them is how they drifted apart the first time.

THE DAEMON-DOWN SWITCH. There is no degraded board. When liveness fails the app
shows `DaemonDownScreen` and nothing else; when the link is restored it returns.
The board and that page are mutually exclusive by construction, so there is no
code path that can render a meter from a stale value.

It is driven by TWO independent signals now, and `_sync_down_screen` is the only
place that decides between them. The link state machine is one; `GET /health`,
polled every three seconds, is the other, and it is much faster — a daemon that
dies without closing its socket is invisible to the link state for up to 40
seconds of ping timeout plus a four-step reconnect ladder. See `HEALTH_SECONDS`.

THERE IS EXACTLY ONE WAY THIS BOARD KNOWS ANYTHING (STAGE S3). Every fact it
draws — the roster, the login state, the usage meters, the pool shares, the
pressure, and WHICH ACCOUNT EACH RUNNING THREAD IS ON — arrives as a `shape`
frame and is held in `self.shapes`. `model.project` turns that held state into
the `Board` the render layer draws, and nothing else does.

WHAT WAS DELETED HERE, AND WHY IT HAD TO BE DELETED RATHER THAN LEFT DORMANT.
S2 built the shape path beside the old one and left the old one reachable:
`reload()` and its five queries (`accounts/status`, `gauge/status`,
`accounts/health`, `accounts/pressure`, `accounts/pool_share_get`), the
placement worker (`request_placement` / `place_running` and one `threads/{thread_id}/get`
per running thread), `live.py`'s 5-second lease, and a 900-second backstop
timer that called `reload()` from nowhere near a frame. All of it is gone.

The reason is not tidiness. A count disagreed: the audio interface and the
daemon both said four running threads and this board said three, and the
shape's export was CORRECT — the wrong number came from a client still holding
state the dormant path had put there. A stale mirror cannot tell it is stale,
and while a second way to learn a fact exists every disagreement is ambiguous,
because either side might be the liar. With one source a wrong number is a bug
in a named place rather than a maybe.

THE HONEST CONSEQUENCE, WHICH IS ON SCREEN. A board against a daemon with no
shapes has NO FALLBACK. It does not crash and it does not draw an empty pool
that looks like a pool with nothing in it: `render.no_board_lines` states that
this daemon holds no fold, quotes the capability registry's own note, and says
the board will turn on by itself the moment the daemon grows one. That is the
same rule as un-hydrated being distinct from empty, applied one level up.

`r` AND `R` ARE STILL TWO KEYS AND STILL MEAN TWO THINGS. `R` is untouched:
`gauge/probe` is how a READING IS MADE and no amount of pushing produces one.
`r` kept its meaning — the cheap "show me the current state" key — and changed
its mechanism, because the five queries it used to run no longer exist: it now
forces a re-subscribe, which makes the daemon flush its debounce and re-export
every key it holds. See `do_resync`.

THE DRAWING WIDTH. The board is drawn to `content_width()`, never to
`self.size.width`. The two are not the same number and the difference is the
scrollbar. `#body` sits inside a `VerticalScroll`; when the board is taller than
the viewport a vertical scrollbar takes two columns, and the widget is two cells
narrower than the terminal. Because `render._pad` sizes every line to EXACTLY
the width it is handed, over-reporting the width does not crop — it WRAPS. Each
row spilled its last two cells onto a line of its own, the board painted at
twice its true height, and every right-hand column landed in the wrong place.
The extra height also kept the scrollbar up, so the fault held itself open.

The scrollbar is KEPT, not disabled. With a realistic pool the board runs three
to four times the height of the viewport, so removing the scrollbar would make
every account below the fold unreachable — a worse defect than a misaligned one,
and a silent one.

Two mechanisms hold the width correct, and the second is the one that fixes
first paint. `scrollbar-gutter: stable` reserves the two columns whether or not
the scrollbar is currently shown, so the content width is CONSTANT — the
appearing scrollbar no longer changes the geometry, and the circularity (width
depends on the scrollbar, which depends on height, which depends on width) is
gone. That is also what makes the pre-layout fallback in `content_width()`
sound: before the first layout the measured region is 0, and the reserved gutter
lets the computed fallback equal the number layout will later produce, so the
FIRST frame is already right rather than right one tick later.
"""

from __future__ import annotations

import asyncio
import time

from rich.text import Text
from textual import work
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import VerticalScroll
from textual.screen import Screen
from textual.widgets import Footer, Static

from . import caps, drag, feed, local, login, render, shapes, slots
from .api import (
    Api,
    DaemonError,
    DaemonUnavailable,
    MethodAbsent,
    ShuttingDown,
    daemon_alive,
)
from .clock import now_utc
from .down import DaemonDownScreen
from .fmt import AMBER, GREEN, INK, INK2, MUTED, RED
from .link import LinkState
from .model import (
    burn_projection,
    history_payload,
    history_sparkline,
    history_truncated,
    pressure_rows,
    project,
    Board,
    SOURCE_HELD,
)
from .pressure import pressure_lines
from .share import ReactivateConfirm, ShareEditor

# ── THE 900-SECOND BACKSTOP IS DELETED (S3), AND HERE IS WHAT IT COVERED ─────
#
# `BACKSTOP_SECONDS = 900` armed a slow, silent `reload()` from a timer. Its
# stated job was the ONE failure no pushed frame can announce: a connection
# that stays up, stays subscribed, and is simply never told about something.
# That is a real failure and it has not stopped being real — what changed is
# that the repair it performed no longer exists. A backstop whose remedy is
# "re-read the five queries" cannot survive the deletion of the five queries,
# and re-introducing them on a timer would re-introduce the exact second source
# S3 exists to remove: a timer-driven answer and a fold-driven answer, silently
# disagreeing, with nothing on screen to say which one is on the card.
#
# WHAT COVERS IT NOW, AND IT IS SMALLER AND HONEST ABOUT BEING SMALLER:
#
#   * `r` — the operator's own key, which forces a re-subscribe and therefore a
#     fresh export of every key the daemon holds. Same key, same meaning, new
#     mechanism (`do_resync`). This is the manual backstop, and unlike the
#     timer it says on screen that it ran.
#   * an `info` line appended to the `narration` fold — re-enumerates the key
#     set, coalesced by `feed.py`. (A firehose `lagged` used to re-enumerate
#     too; this board subscribes no firehose, so it is never sent one, and a
#     shape channel's own lag is repaired by the daemon re-exporting.)
#   * the socket dropping — the mirror is reset and re-hydrated from scratch.
#
# WHAT IS NO LONGER COVERED AT ALL, SAID OUT LOUD: a daemon whose fold silently
# stops emitting a row it should have emitted, on a connection that never lags
# and never drops. Nothing here will notice. That is not a regression this
# change chose to accept quietly — it is the price of one source, and the
# price is payable because the fold is now the SAME code path the daemon's own
# `shapes/list` reports on (`lost`, `reinit`, `bursts`), and because a fold
# that dies says so on the wire and the board draws it in red.

# ── DAEMON-DOWN DETECTION, MOVED OFF THE SOCKET AND ONTO THE HEALTH ROUTE ────
#
#     "Daemon going down - detect based on health check"
#
# WHAT IT COST BEFORE. The down page was driven entirely by the link state
# machine, and that machine is slow by design: `websockets` is configured with
# `ping_interval=20, ping_timeout=20`, so a daemon that dies without closing
# its socket (a kill -9, a wedge, a crash mid-write) is not noticed for up to 40
# seconds — and only THEN does the reconnect ladder start, which needs
# GIVE_UP_AFTER=4 attempts with 1+2+4+8s of backoff before the state reaches
# OFFLINE. Worst case is around 55 seconds of a board full of numbers that are
# no longer coming from anywhere, with a green "live" in the header.
#
# `GET /health` answers that question directly and costs one loopback
# connection. Two consecutive failures, six seconds apart at worst, and the down
# page is up.
#
# TWO STRIKES, NOT ONE. A single refused connection is not proof a daemon is
# gone — it is also what a daemon that is briefly busy accepting looks like, and
# throwing the operator onto a full-screen "the daemon is not running" page for
# that would make the page the thing he learns to ignore. Two is the smallest
# number that distinguishes a blip from a state.
HEALTH_SECONDS = 3.0
HEALTH_TIMEOUT_FAST = 2.0
HEALTH_STRIKES = 2

# Derived, never a literal. See the module docstring: an independently chosen
# verdict wait is how the two numbers drifted apart in the first place.
from .api import HEALTH_TIMEOUT  # noqa: E402

# ── HOW LONG A FORCED RE-EXPORT IS GIVEN ─────────────────────────────────────
#
# `resync()` answers `{"queued": true}` the moment the subscribe item is in the
# shape's FIFO, and the EXPORTS follow as frames. So the wait is not on a
# response, it is on the mirror taking a new export, and it needs a number.
#
# IT IS DERIVED FROM THE CALL IT WAITS ON, and that is the same rule the
# verdict wait was rewritten under (see the module docstring): the old defect
# was an independently-chosen 8 seconds waiting on a call allowed 30, so a slow
# answer let the verdict read the PREVIOUS board. `REQUEST_TIMEOUT` is what the
# subscribe itself is allowed; the slack on top is for the daemon's own work
# between accepting the item and exporting — a LAZY shape that is cold hydrates
# first, which is four `security` invocations and ~0.8s measured, and the item
# is queued behind whatever producer events are already in the FIFO.
from .api import REQUEST_TIMEOUT  # noqa: E402

RESYNC_DEADLINE = REQUEST_TIMEOUT + 5.0

MIN_WIDTH = 40


def content_width(screen: Screen) -> int:
    """The width the board must draw to — the scroll region's CONTENT width.

    This is the one place the drawing width is decided, and it is deliberately
    not `App.size.width`: that is the terminal, and the terminal includes the
    scrollbar gutter that the body does not get to use.

    Measured when layout has run. Before it has — the first frame, where the
    region is still 0 — the value is COMPUTED from the same reserved gutter that
    layout will use. With `scrollbar-gutter: stable` those two agree exactly,
    so first paint is not a guess that a later frame corrects.
    """
    try:
        scroll = screen.query_one(VerticalScroll)
    except Exception:
        # No scroll container on this screen: the whole width is drawable.
        return max(MIN_WIDTH, screen.app.size.width)

    # `scrollbar_size_vertical` reports the RESERVED gutter under
    # `scrollbar-gutter: stable`, not the currently-visible one, so this does
    # not depend on knowing whether the scrollbar is up yet.
    computed = screen.app.size.width - scroll.scrollbar_size_vertical
    measured = scroll.scrollable_content_region.width
    if measured <= 0:
        return max(MIN_WIDTH, computed)  # pre-layout, first frame

    # THE SMALLER OF THE TWO, deliberately. Neither source is trustworthy on its
    # own at the instant a resize is being processed: `measured` is last frame's
    # region and `computed` assumes the body spans the screen. They disagree only
    # mid-resize, and the two ways of being wrong are not equally bad — drawing
    # NARROWER than the widget leaves a blank column, while drawing WIDER wraps
    # every row and doubles the board's height. So take the low one and let the
    # post-refresh redraw settle it on the next frame.
    return max(MIN_WIDTH, min(measured, computed))


class BoardScreen(Screen):
    # ── THE KEYS THE BOARD OFFERS, AND THE TWO THAT WERE MISSING ─────────────
    #
    # A dead account's row says *"press <n> then L to sign it in"*. The operator
    # pressed exactly that and NOTHING HAPPENED, because of two independent
    # gaps, both fixed here:
    #
    #   1. THERE WERE NO DIGIT BINDINGS ON THIS BOARD AT ALL. The hint told him
    #      to press a number that no handler listened for. Selection could only
    #      be moved with j/k/arrows. (The sibling `claude-accounts` board did
    #      bind 1-9, which is why the same hint half-worked over there and made
    #      the failure look inconsistent.)
    #
    #   2. `L` WAS NOT BOUND — only lowercase `l`. Textual matches binding keys
    #      by EXACT string with no case folding, so shift+L found no binding,
    #      found no `key_upper_l` handler either, and was dropped silently. And
    #      this codebase already knows uppercase is a distinct key: it binds "S"
    #      for share next to "s" for set-local, two lines down. A lowercase `l`
    #      in a terminal font is near-indistinguishable from `I` or `1`, so
    #      reading it as "L" and pressing shift is the correct human response to
    #      what is on screen.
    #
    # BOTH CASES OF `l` NOW SIGN IN. That is deliberate over picking one and
    # rewriting the hint to match: the failure mode of a wrong guess here is
    # silent, and a silent no-op on the key a DEAD account tells you to press is
    # the worst case in this whole interface — it is the one he reaches for when
    # he is already locked out.
    BINDINGS = [
        Binding("q", "quit_app", "quit"),
        # ── THERE IS NO REFRESH KEY AND NO POLL KEY ON THIS BOARD ───────────
        #
        # `Binding("r", "resync", "re-read", show=False)` and
        # `Binding("R", "probe", "poll accounts")` STOOD HERE. Both are gone, and
        # their absence is not a tidy-up. It is a CLAIM ABOUT THE PRODUCER, and
        # it is the operator's own instruction:
        #
        #     "also 'R = poll accounts' should not just be retired and removed
        #      from view, it should be the forcing function to prevent ever
        #      needing this."
        #
        # WHAT A REFRESH KEY MEANS, WHICH IS THE WHOLE ARGUMENT. A key that
        # re-reads the board is the interface admitting it does not trust what it
        # is showing. Worse, it makes that distrust COMFORTABLE: when the screen
        # goes stale the operator's recourse is a keystroke, he gets his number,
        # and the defect that made the screen stale is never reported — so it is
        # never fixed. The escape hatch does not mitigate the staleness bug, it
        # PRESERVES it, indefinitely, by making it survivable.
        #
        # Remove the hatch and the arithmetic inverts. Any staleness becomes an
        # unacceptable, visible defect that someone has to fix AT THE PRODUCER,
        # because there is no longer anything the operator can press to paper
        # over it. That is what "forcing function" means here: the missing key is
        # the pressure that keeps the push path honest.
        #
        # SO IF THIS BOARD EVER LOOKS STALE, THE FIX IS IN THE PRODUCER. It is
        # the daemon's job to push every change to every fold it holds, and to
        # make its own usage readings on its own schedule. It is not this board's
        # job to ask. Adding a key back here would not fix a stale board; it
        # would only stop anyone noticing that the board is stale — which is the
        # exact failure this deletion exists to prevent. The next person to feel
        # the itch is meeting this paragraph on purpose.
        #
        # ENFORCED, NOT ASKED FOR. `tests/test_keys.py` pins the binding table
        # as an EXACT SET. It is a positive assertion rather than a ban on the
        # word "poll", because a ban goes quiet the moment someone spells the
        # same key `refresh` or `reload`; an exact set fails on ANY new binding,
        # including one nobody thought to forbid. That test is the forcing
        # function's teeth, and it cannot be satisfied by renaming.
        #
        # THE MECHANISM WENT WITH THE KEYS, in both directions. `action_resync`,
        # `action_probe`, `do_resync` and `do_refresh` are deleted; so are
        # `Api.gauge_probe` and `caps.GAUGE_PROBE`, this client's only way to ASK
        # an account for a reading. Leaving an unbound poll method behind would
        # have left a one-line rebind, and this file's own rule — the one that
        # removed `seconds_to_refresh` and the `unplaced` residual rather than
        # pinning them at zero — is that a mechanism nothing reads is a mechanism
        # the next person wires back up.
        #
        # `resync()` ITSELF IS NOT A REFRESH PATH AND STAYS. It is how a MUTATION
        # SETTLES: disable, share, login, rename and rotate each await it so the
        # board shows the state the daemon actually reached rather than the state
        # the keystroke intended. It is caused by an operator ACT with a known
        # expected effect, never by doubt about the screen, and no key invokes it.
        Binding("p", "pressure", "pressure"),
        Binding("l", "login", "login"),
        Binding("L", "login", "login", show=False),
        # ── ADD AN ACCOUNT ───────────────────────────────────────────────────
        #
        # THE ONLY ACT ON THIS BOARD WITH NO ROW TO SELECT, which is exactly why
        # it was the only act with no key. Every other operation here reads its
        # subject off the selection; creating an account has no subject yet, so
        # it needs somewhere to type one, and without that the operator had to
        # leave the board and run `claude-accounts login <name>` in a terminal.
        #
        # Nothing else was missing. `accounts/login/begin` on a label with no
        # slot already creates the account end to end — `add-account --login`
        # makes the slot directory and, once the grant lands, runs `threads
        # accounts adopt`. So the whole feature is a text field in front of the
        # login flow that already exists.
        Binding("n", "new_account", "new account"),
        Binding("N", "new_account", "new account", show=False),
        Binding("s", "set_live", "set local"),
        Binding("S", "share", "share"),
        # ── DISABLE / ENABLE THE SELECTED ACCOUNT ────────────────────────────
        #
        # *"Create the machinery on both sides of the contract to disable an
        # account so that it doesn't get considered a placement, and then the
        # text user interface just says, like, disabled."* (msg #531)
        #
        # ONE KEY, BOTH DIRECTIONS, and the direction is read off the account
        # rather than typed. A separate `e` for enable would be a key that does
        # nothing on most rows most of the time, and he drives this board by
        # keyboard — a key whose effect depends on where the cursor is, stated on
        # the row itself, is the shape the rest of this board already uses.
        #
        # WHY THIS KEY HAD TO EXIST AT ALL. The capability shipped on 2026-09-08
        # behind the share editor: set the share to 0. It was live in the running
        # daemon for four days and he never reached it — his `pool_shares` row
        # still held the 0.001% he had tried BEFORE the feature existed. A
        # capability whose only door is a magic value in a number field is a
        # capability nobody finds, and that is not a guess, it is what the row
        # said.
        Binding("d", "disable", "disable"),
        Binding("D", "disable", "disable", show=False),
        # ── THE THREE METHODS THAT WERE ON THE WIRE WITH NO DOOR ─────────────
        #
        # `accounts/{account}/rename`, `accounts/{account}/delete` and `accounts/rotate` were all
        # declared in `caps.py`, all had `Api` methods, and all had ZERO
        # CALLERS. The capability was never missing; the door was. That is the
        # same gap `n` closed, and the same cost: the operator who wanted any
        # of the three had to leave the board and find a terminal.
        #
        # ── THE RULE THAT CHOSE THESE KEYS ──────────────────────────────────
        #
        # This board already had half a rule — `r` is the cheap re-read and `R`
        # is the expensive poll, because "the expensive act is the one that
        # should require a deliberate shift". These three finish it:
        #
        #   REVERSIBLE and PER-ACCOUNT  →  BOTH CASES, like n/N, d/D, l/L.
        #   COSTLY or IRREVERSIBLE      →  THE SHIFTED KEY ONLY, and the
        #                                  unshifted key EXPLAINS ITSELF.
        #
        # The second half of that second line is the part that matters. The
        # worst failure this interface has is a key that silently does nothing
        # (see the note at the top of this table), so an unshifted `x` or `o`
        # is bound to a handler that says which key to press instead. A guard
        # that is also silent would be trading one known failure for another.
        #
        # AND EVERY ONE OF THE THREE OPENS A MODAL FIRST. No keypress here
        # performs an operation; the worst a fat-finger costs is an escape. It
        # is why `m` may sit beside `n`, and why `O` may sit beside `p` and `l`.
        #
        # ── m / M — RENAME ──────────────────────────────────────────────────
        #
        # Reversible (rename it back), per-account, and cheap. `r` is taken
        # twice over, so the key is `m` for MOVE — which is literally what the
        # daemon does: `rename_account` moves the login directory, and with it
        # the keychain service name that is derived from the directory path.
        # Its neighbours are `n` (a modal) and `j`/`k` (navigation); nothing
        # destructive is within a finger of it.
        Binding("m", "rename", "rename"),
        Binding("M", "rename", "rename", show=False),
        # ── X — DELETE ──────────────────────────────────────────────────────
        #
        # THE ONLY IRREVERSIBLE ACT ON THIS BOARD. Everything else undoes in
        # one more keypress: a disable with `d`, a share with `S`, a rename
        # with `m`. This one removes a directory, a keychain item and a
        # registry entry, and no key puts them back.
        #
        # So it is SHIFTED, and `x` — which is a plausible slip from `d` and
        # `s`, both of which sit next to it — does nothing but explain itself.
        # The modal then does not arm until the account's name has been typed,
        # because a confirmation satisfied by the same reflex that opened it
        # protects against nothing.
        Binding("X", "delete", "delete", show=True),
        Binding("x", "delete_hint", "delete", show=False),
        # ── O — ROTATE KEYS ─────────────────────────────────────────────────
        #
        # COSTLY, and pool-wide. A pass invokes `claude` once per registered
        # slot, serially, each probe capped at two minutes — this is the `R`
        # argument again, and by that argument it belongs on the shifted key.
        # `o` is the next letter of "rotate" with `r` and `R` both spent, and
        # bare `o` explains where the act lives.
        #
        # IT IS NOT READ OFF THE SELECTED ROW, and the modal says so.
        # `accounts/rotate` takes no account — the daemon reads `dry_run` and
        # nothing else — so a key that appeared to scope itself to the cursor
        # would be claiming a scope the wire does not have.
        Binding("O", "rotate", "rotate keys", show=True),
        Binding("o", "rotate_hint", "rotate keys", show=False),
        Binding("j", "down", "down", show=False),
        Binding("k", "up", "up", show=False),
        Binding("down", "down", "down", show=False),
        Binding("up", "up", "up", show=False),
        Binding("exclamation_mark", "drag_note", "note", show=False),
        # Select a row by the number PRINTED ON IT. Routed through
        # `action_select`, which looks the account up with `Board.by_number`
        # rather than indexing the list — see the note there; the printed number
        # is 1-based and `self.selected` is a 0-based index, and conflating the
        # two would silently select the wrong account.
        *[Binding(str(n), f"select({n})", "", show=False) for n in range(1, 10)],
    ]

    def compose(self) -> ComposeResult:
        with VerticalScroll(id="scroll"):
            yield Static(id="body")
        yield Footer()

    def on_mount(self) -> None:
        self.set_interval(1.0, self._tick)
        # Draw as soon as the body exists. `App.on_mount` fires a redraw before
        # this screen's widgets are mounted, and that one is dropped; without
        # this the first real frame waited on the 1s tick.
        self.app.redraw()

    def on_resize(self) -> None:
        # Redraw on the resize EVENT, not on the next tick. There was no resize
        # handler at all, so a resize left the previous width on screen for up
        # to a second — the "it's not consistent when I reduce the size" half.
        #
        # TWICE, and the second one is the one that is correct. While this event
        # is being handled the container's region is still last frame's, so the
        # first redraw draws to the conservative width; `call_after_refresh`
        # runs once layout has been applied and redraws to the settled one.
        self.app.redraw()
        self.app.call_after_refresh(self.app.redraw)

    def _tick(self) -> None:
        self.app.on_tick()

    def action_quit_app(self) -> None:
        self.app.action_quit_app()

    # `action_resync` (`r`) and `action_probe` (`R`) STOOD HERE and are deleted
    # with their keys. The reasoning is in the binding table above, and it is not
    # "these were unused": the absence of a refresh path is a claim that the
    # daemon pushes every change, and a handler left behind is a one-line rebind
    # away from cancelling that claim.

    def action_pressure(self) -> None:
        self.app.push_screen(PressureScreen())

    def action_drag_note(self) -> None:
        self.app.push_screen(DragScreen())

    def action_down(self) -> None:
        self.app.move_selection(1)

    def action_up(self) -> None:
        self.app.move_selection(-1)

    def action_select(self, number: int) -> None:
        """Select the account whose PRINTED number is `number`.

        Delegates to `App.select_number`. A digit that names no row is ignored
        — pressing 7 on a four-account board should do nothing, not move the
        selection somewhere arbitrary.
        """
        self.app.select_number(number)

    # These three FIRE a worker rather than awaiting one. `@work` returns a
    # Worker object, which is not awaitable; awaiting it raises. Firing is also
    # the behaviour we want — the keypress handler returns immediately and the
    # board keeps redrawing while the modal or terminal handoff is up.
    def action_login(self) -> None:
        self.app.do_login()

    def action_new_account(self) -> None:
        self.app.do_new_account()

    def action_set_live(self) -> None:
        self.app.do_set_live()

    def action_share(self) -> None:
        self.app.do_share()

    def action_disable(self) -> None:
        self.app.do_disable()

    def action_rename(self) -> None:
        self.app.do_rename()

    def action_delete(self) -> None:
        self.app.do_delete()

    def action_rotate(self) -> None:
        self.app.do_rotate()

    # ── THE TWO UNSHIFTED KEYS THAT ARE NOT SILENT ───────────────────────────
    #
    # They perform nothing and they send nothing. Their entire job is to stop a
    # guarded key from being indistinguishable from an unbound one: the worst
    # failure this interface has ever had was a key that looked bound, did
    # nothing, and said nothing, and putting a guard on `X` and `O` would have
    # manufactured two more of exactly that.
    def action_delete_hint(self) -> None:
        self.app.note(
            "deleting an account is X (shift+x) — it removes the slot, its keychain "
            "item and its registry entry, and nothing on this board undoes it. "
            "Press d to take an account out of placement instead; that one comes back",
            "warn",
        )
        self.app.redraw()

    def action_rotate_hint(self) -> None:
        self.app.note(
            "rotating keys is O (shift+o) — it refreshes EVERY slot's credential, "
            "one `claude` invocation at a time, and can take minutes. It is on the "
            "shifted key for the same reason R is",
            "warn",
        )
        self.app.redraw()


class PressureScreen(Screen):
    BINDINGS = [Binding("escape", "back", "back"), Binding("q", "back", "back")]

    def compose(self) -> ComposeResult:
        with VerticalScroll():
            yield Static(id="pbody")
        yield Footer()

    def on_mount(self) -> None:
        self.render_now()

    def on_resize(self) -> None:
        self.render_now()

    def render_now(self) -> None:
        app = self.app
        # ── THE GATE IS THE FOLD, NOT A SEPARATE `accounts/pressure` PROBE ──
        #
        # It used to ask the capability registry whether `accounts/pressure`
        # was on the wire. That method is no longer called by this board at all
        # (S3), so its capability state would be a claim about a query nothing
        # issues — and a view drawn behind a gate that measures the wrong thing
        # is exactly the class of quiet wrongness S3 exists to end. Pressure
        # rides on the shape's own `pressure` cell now, so the honest gate is
        # whether this board has a fold to read at all, and the sentence shown
        # is the board's own explanation of why it has not.
        unavailable = None
        if app.board.source != SOURCE_HELD:
            unavailable = app.board.source_note
        rows = pressure_rows(app.board, app.selected_label())
        # `self.size.width - 2` used to stand in for the gutter here. It was
        # right by coincidence; `content_width` is right by construction.
        lines = pressure_lines(rows, content_width(self), unavailable=unavailable)
        self.query_one("#pbody", Static).update(render.join(lines))

    def action_back(self) -> None:
        self.app.pop_screen()


class DragScreen(Screen):
    """The drag note, in the tool."""

    BINDINGS = [Binding("escape", "back", "back"), Binding("q", "back", "back")]

    def compose(self) -> ComposeResult:
        with VerticalScroll():
            yield Static(id="dbody")
        yield Footer()

    def on_mount(self) -> None:
        out = Text(no_wrap=False)
        for line in drag.note_lines():
            style = f"bold {INK}" if line and not line.startswith(" ") else INK2
            out.append(line + "\n", style=style)
        self.query_one("#dbody", Static).update(out)

    def action_back(self) -> None:
        self.app.pop_screen()


class BoardApp(App):
    CSS = """
    Screen { background: #1a1a19; }

    /* The gutter is RESERVED, always. The scrollbar appearing must not change
       the drawable width, or the width the last frame was drawn to becomes
       wrong the moment the board outgrows the viewport. */
    VerticalScroll { scrollbar-gutter: stable; overflow-x: hidden; }

    /* Belt and braces. Every line is already sized to the widget, so wrapping
       could only ever be a bug — and it is a bug that CASCADES, because a
       wrapped line makes the board taller, which is what summons the scrollbar
       in the first place. `nowrap` turns any residual off-by-one into a
       one-cell crop: still wrong, but bounded and obvious rather than a board
       drawn at twice its height. */
    #body, #pbody, #dbody { width: 100%; height: auto; text-wrap: nowrap; }
    """

    def __init__(self) -> None:
        super().__init__()
        self.api = Api(
            on_state_change=self._on_link,
            on_capabilities=self._on_caps,
            on_shape=self._on_shape,
        )
        self.board = Board()
        self.selected = 0
        self.tick = 0
        self.refreshing = False
        self.status_note: str = ""
        # A status note is not always a warning. See `render.warning_line`.
        self.status_severity: str = "warn"
        self._down_shown = False

        # THE FORCED-PROBE BANNER STATE (`probing`, `probe_started`,
        # `probe_elapsed`) STOOD HERE. It existed to acknowledge the `R`
        # keypress while a pool-wide `claude /usage` ran. There is no such
        # keypress, so there is no such banner and no such state.

        # A rotation pass is pool-wide, serial, and can run for minutes. The
        # daemon holds its own lock and refuses a second pass with a sentence of
        # its own, so this flag is not the guard — it is what stops the board
        # firing a second 900-second request in order to be told that.
        self.rotating = False

        # ── PLACEMENT BOOKKEEPING IS DELETED (S3) ────────────────────────
        #
        # `_placing`, `placement_absent`, `request_placement` and
        # `place_running` are gone, and so is the `threads/{thread_id}/get`-per-thread
        # resolution they drove. THE PLACEMENT IS THE KEY: a thread row lives
        # under the account it was placed on, so "which account did this thread
        # land on" is answered by which map the row is in, and there is no
        # question left to ask, no work list to drain, no residual to report
        # and no `unplaced` segment on the header.

        # The daemon process we believe we are talking to, learned from the
        # `boot_id` on the newest `lifecycle` line of the `narration` fold. A
        # changed boot id means the running set we hold describes a daemon that
        # no longer exists. See `_on_narration`.
        self._boot_id: str | None = None

        # ── DAEMON LIVENESS OFF `GET /health` ────────────────────────────────
        self._health_strikes = 0
        self._health_down = False
        self._health_inflight = False

        # ── `live.LiveThreads` IS DELETED (S3) ───────────────────────────────
        #
        # `threads_tui/live.py` is gone with it. It was a hand-written
        # client-side fold: a 5-second lease off a 1 Hz heartbeat, a per-thread
        # placement cache, a work list, a blocked set and a residual. Every one
        # of those existed because the wire carried no held state, and all of
        # them were a SECOND answer to a question the daemon's own fold now
        # answers — which is precisely how the board came to disagree with the
        # daemon about how many threads were running.

        # THE LIVE NARRATION AND ANNOUNCEMENT FEED — see `feed.py`. It renders
        # the live narration line from `info` / `progress` / `lifecycle` LINES,
        # read off the `narration` mirror below. What it no longer does is
        # schedule a board re-read; an announcement now means "the KEY SET may
        # have moved", and the response to it is a re-enumeration.
        self.feed = feed.Feed()

        # ── THE SHAPE MIRROR — see `shapes.py`. THE BOARD'S ONE SOURCE. ──────
        #
        # The daemon's own fold, held here and applied frame by frame. There is
        # no longer anything for it to be "active" instead of: `model.project`
        # over this mirror is the only thing that builds `self.board`.
        self.shapes = shapes.ShapeMirror()
        # ── A SECOND MIRROR, FOR THE DAEMON'S OWN WARNING COUNT ───────────
        #
        # `ShapeMirror` is single-shape by construction — `on_frame` returns
        # False for any shape but its own — so a second shape needs a second
        # instance. That is the cost of `warnings` being its own shape rather
        # than a cell on `accounts`, and it is paid here deliberately: a count
        # of the PRODUCER's warnings has nothing to do with accounts, and
        # hanging it off the account board would mean the AUI could not learn
        # it without subscribing to a board it does not care about.
        self.warnings = shapes.ShapeMirror(name=shapes.WARNINGS)
        # ── A THIRD MIRROR: THE DAEMON TALKING WHILE IT WORKS ─────────────
        #
        # `narration` — three channel keys (`info`, `lifecycle`, `progress`),
        # one `lines` cell each. It is why this board sends no firehose
        # subscribe: those three kinds of frame carried an empty thread id and
        # had no other door until this shape. `feed.py` reads lines off THIS
        # fold (`_on_narration`), never frames off the socket.
        self.narration = shapes.ShapeMirror(name=shapes.NARRATION)
        # Set when the narration subscribe has been SENT on this connection,
        # cleared when the socket drops. ONCE PER CONNECTION, unlike the
        # accounts enumeration: its keys are a fixed three, so re-sending it on
        # every `info` announcement would re-export up to three hours-long
        # windows for nothing.
        self._narration_armed = False
        # ── A THIRD MIRROR: THE NON-IDLE ROSTER (R2.S8) ───────────────────
        #
        # Every card's running / queued / parked numbers are a group-by over
        # this one roster's `account` field. They used to ride each account row
        # as its `threads` cell, which was six fields of another shape's row
        # nested under an account key; that cell is gone from the producer.
        self.non_idle = shapes.ShapeMirror(name=shapes.NON_IDLE)
        # Set while a keyless subscribe is in flight, so a burst of `info`
        # frames arms ONE re-enumeration rather than one per frame. The same
        # lesson as `request_placement`: the flag is set SYNCHRONOUSLY, on the
        # same turn of the loop as the decision to arm.
        self._subscribing = False
        # When the last keyless subscribe was SENT. Taken before the request
        # rather than after the answer, deliberately: an announcement arriving
        # while a subscribe is in flight may describe a change the daemon made
        # after it snapshotted, so the conservative reading is that it is not
        # covered. Over-enumerating costs one round trip; under-enumerating
        # drops a key's frames on the floor until the next announcement.
        self._enumerated_at: float | None = None
        # ── `shape_fallback` IS DELETED (S3) ────────────────────────────────
        #
        # At S2b a dead fold could be escaped by pressing `r`, which fell back
        # to the five queries. There is nothing to fall back TO any more, and
        # that is the correct end state rather than a loss: the fallback was
        # the second source, and a second source is what made a disagreement
        # unattributable. A dead fold is now terminal and says so — `r` asks
        # the daemon to re-subscribe, the daemon refuses in its own words
        # ("shape `accounts` is dead (...)"), and that refusal is what the
        # operator reads. The way out is a daemon restart, which this board
        # does not do and does not pretend to do.

        # ── A FIFTH MIRROR: EACH ACCOUNT'S WEEK OF READINGS (R2.S3d) ─────────
        #
        # The 7-day sparkline used to be the board's ONE remaining read — a
        # `gauge/history` request for the selected card, re-asked on every
        # `gauge` announcement. It is a shape now, `accounts/gauge-history`,
        # keyed per account and `PerKey`, so it is subscribed ONCE per account
        # on the screen (`_arm_history`) and every reading the daemon records
        # arrives as a `transition` on `readings`. The sparkline is drawn off
        # this fold at paint time (`history_extras`); nothing is asked, so
        # nothing has to be invalidated.
        self.gauge_history = shapes.ShapeMirror(name=shapes.GAUGE_HISTORY)
        # The account keys a history subscribe has been SENT for on this
        # connection. Set synchronously, before the worker runs, for
        # `subscribe_shapes`' reason: a burst of account rows must arm each key
        # once, not once per row.
        self._history_keys: set[str] = set()

    # ── lifecycle ────────────────────────────────────────────────────────────

    async def on_mount(self) -> None:
        self.push_screen(BoardScreen())
        self.api.start()
        # DAEMON LIVENESS. Independent of the socket on purpose: the socket is
        # what takes up to 40 seconds to notice a daemon that died without
        # closing it.
        self.set_interval(HEALTH_SECONDS, self._health_check)
        # NOTHING IS FETCHED HERE. There is no board to build until the daemon
        # exports one, and the whole sequence that gets us there is driven by
        # the connection: `api.start()` connects, probes, and calls `_on_caps`,
        # which is where the keyless subscribe goes out. Issuing anything at
        # mount would be issuing it before the socket exists — which is what
        # the deleted `await self.reload()` did, losing every time.
        self.redraw()

    async def _health_check(self) -> None:
        """**IS THE DAEMON THERE.** Asked over `GET /health`, not over the socket.

        The socket cannot answer this quickly — see `HEALTH_SECONDS`. This can,
        and it is the fact the daemon-down page is now driven by.

        `_health_inflight` guards against overlap: the interval is 3 seconds and
        the call is allowed 2, but a machine under load can miss that, and a
        second check starting while the first is still connecting would double
        the strike rate and halve the threshold it was chosen for.

        A SUCCESS CLEARS THE STRIKES OUTRIGHT rather than decrementing them.
        Two failures mean "the daemon is gone"; one failure, a success, and
        another failure mean "something is flaky", and flaky is not down.
        """
        if self.api.shutting_down or self._health_inflight:
            return
        self._health_inflight = True
        try:
            ok, _ = await daemon_alive(timeout=HEALTH_TIMEOUT_FAST)
        except Exception:  # noqa: BLE001 — a failed check is a failed check
            ok = False
        finally:
            self._health_inflight = False

        if ok:
            self._health_strikes = 0
            if self._health_down:
                self._health_down = False
                self._sync_down_screen()
            return

        self._health_strikes += 1
        if self._health_strikes >= HEALTH_STRIKES and not self._health_down:
            self._health_down = True
            # THE MIRROR IS DELIBERATELY NOT CLEARED HERE, and neither is the
            # FEED. Both are claims about THIS SOCKET, and a failed `GET
            # /health` is not evidence about the socket — the connection may
            # genuinely still be open and folding. What this signal does is
            # raise the down page, which is mutually exclusive with the board,
            # so nothing stale can be on screen either way. If the socket
            # really is gone, `_on_link` resets both, which is the one place
            # that is entitled to.
            self._sync_down_screen()

    def _sync_down_screen(self) -> None:
        """Show or hide the daemon-down page. **THE ONLY PLACE THAT DECIDES.**

        There are now two independent reasons to believe the daemon is gone —
        the link state machine reaching OFFLINE, and the health check failing
        twice — and they do not move together. The health check is much faster,
        so it is usually the one that raises the page; the link state is the one
        that survives a health route that answers while the daemon is otherwise
        unusable. Either is sufficient; BOTH must be clear before the board
        comes back, or the faster signal would be undone by the slower one still
        catching up.
        """
        should_be_down = self._health_down or self.api.state is LinkState.OFFLINE
        if should_be_down and not self._down_shown:
            self._down_shown = True
            self.push_screen(DaemonDownScreen())
        elif not should_be_down and self._down_shown:
            self._down_shown = False
            try:
                self.pop_screen()
            except Exception:
                pass

    def _on_link(self, state: LinkState) -> None:
        if state is not LinkState.LIVE:
            # THE BOARD DIES WITH THE SOCKET, rows and all. A subscription is
            # per CONNECTION: the daemon's shape is lazy, it stops holding
            # state when the last subscriber goes, and a reconnect re-hydrates.
            # Rows carried across would be the last fold's answers wearing the
            # new fold's provenance — which is precisely the lie the
            # three-valued state exists to prevent, arriving by the back door.
            #
            # The count goes back to `running ?` with them, because it IS them:
            # since S3 the running count is a `count` over held rows, so there
            # is no separate lease to expire and no second number that could
            # survive the connection that vouched for it.
            self.shapes.reset()
            # **AND SO DOES THE WARNING COUNT**, for the same reason and with a
            # sharper consequence: a stale count is a claim about a daemon this
            # board is no longer talking to. `reset()` puts it back to
            # un-hydrated, `project` then leaves `daemon_warnings` as `None`, and
            # the header draws NOTHING rather than the last number it happened to
            # hold. Painting a remembered 47 beside a dead link would be the
            # frozen screen that looks calm.
            self.warnings.reset()
            # The running counts ARE this roster, so it goes with the link.
            self.non_idle.reset()
            # AND THE FEED DIES WITH IT — louder than the count does. The count
            # going to `running ?` is an admission that one number is unknown;
            # this is the admission that NOTHING on the screen will move on its
            # own until the link is back, which is the difference between a
            # quiet system and a frozen one. `subscribed` is cleared here and
            # only a fresh narration export sets it again, so a reconnection that fails
            # to re-subscribe stays visible instead of passing for healthy.
            self.feed.reset()
            # And the narration mirror with it: its windows were exported to
            # THIS connection, and the next one is exported its own.
            self.narration.reset()
            self._narration_armed = False
            # And each account's week: exported to THIS connection, so the next
            # one subscribes its own.
            self.gauge_history.reset()
            self._history_keys.clear()

        # The daemon-down page is the ONLY thing shown while the daemon is
        # down. No degraded board. The DECISION is not made here any more —
        # `_sync_down_screen` owns it, because there are now two independent
        # sources for it and the faster one is the health check.
        self._sync_down_screen()

        # A reconnection does not need a "reload" arming here any more, and
        # this is where one used to be. Becoming LIVE re-runs the capability
        # probe, which calls `_on_caps`, which is the one place a subscription
        # is armed — so the reseed is attached to the event that makes it
        # necessary and is spelled once rather than twice.
        self.redraw()

    def _on_caps(self) -> None:
        """Probing finished. **THE ONE PLACE A SUBSCRIPTION IS ARMED.**

        There is no version negotiation anywhere in this app beyond the
        capability registry. `shapes/list` answering `unknown method:` means a
        daemon with no folds, `caps.is_absence` records it ABSENT, and since
        S3 THAT IS THE WHOLE STORY: there is no second path to fall back to,
        so the board draws the registry's own note instead of a board (see
        `render.no_board_lines`) and turns on by itself if the daemon is later
        promoted. It answered anything else and the board asks for an export.

        It is also the RE-switch point across a daemon promotion: `caps.Registry`
        is reset on a changed boot id inside `Api._handshake`, so a board that
        reconnects onto a daemon that has grown shapes probes again and arrives
        here again, with `_on_link` having already cleared the mirror.
        """
        if self.api.caps.has(caps.SHAPES_LIST):
            self.subscribe_shapes()
        self.redraw()

    # ── the tick, and the live count ─────────────────────────────────────────

    def on_tick(self) -> None:
        """One second. Animates the spinner and repaints the subtractions.

        NOTE WHAT IS *NOT* HERE, AND IT IS NOW THE WHOLE LIST: no count, no
        lease expiry, no poll, no re-read, no countdown. The tick asks the
        daemon nothing. Every number on this board moves when a `shape` frame
        lands (`_on_shape`); the repaint on this tick exists for the two
        things that genuinely change with the clock and nothing else — the
        spinner, and the subtractions-against-now that `model.project` does
        (`days_left`, axis age, `available`), which is the price of the wire
        carrying instants rather than pre-aged numbers.
        """
        self.tick += 1
        now = time.monotonic()
        # THE BACKSTOP FOR THE COALESCE TIMER, not a mechanism of its own.
        # `_on_narration` arms a one-shot timer the instant an announcement lands.
        # This catches the one case that timer cannot: a scheduled
        # re-enumeration whose timer was lost across a screen push or a
        # cancelled worker. It fires at most a second late and never fires at
        # all in the normal path.
        if self.feed.due(at=now):
            self._feed_announced()
        self.redraw()

    # ── `_on_push` IS DELETED, NOT GUARDED ───────────────────────────────
    #
    # It was the FIREHOSE's handler: every frame the daemon pushed to a
    # connection that had sent `{"type": "subscribe", "threads": null}`. Its
    # docstring said "shapes do not carry narration", which stopped being true
    # when the `narration` shape landed, and its two arms are gone with it:
    #
    #   * the `lifecycle` arm, which read `boot_id` off a firehose frame. The
    #     boot id is on the `lifecycle` LINE now (`_on_narration`).
    #   * the `lagged` arm, which re-enumerated on a firehose lag. There is no
    #     firehose on this connection to lag.
    #
    # `Api` no longer has an `on_push` to call, so a frame that is neither a
    # response nor a shape frame is dropped at the socket (`Api._dispatch`).

    def _on_narration(self, frame: dict) -> None:
        """A `narration` shape frame. **THE ONE DOOR NARRATION HAS.**

        The mirror applies it first; then the feed reads what the FOLD now
        holds. Three outcomes, and they are separate on purpose:

          * an EXPORT (`op: read`) is the daemon's word that it is narrating
            to this connection — `feed.narrating()`, which is what the header's
            "live" reads. It narrates NOTHING: an export is the window as it
            already was, up to an hour of it, and replaying that as the `⚡`
            line would announce old news as though it had just happened.
          * a TRANSITION appends one line, and THAT is the `⚡` line
            (`shapes.appended_line` → `feed.on_line`). An `info` line schedules
            the coalesced re-enumeration exactly as the firehose frame did.
          * either way, a `lifecycle` channel whose newest line names a boot id
            different from the one this board has been mirroring means a
            different daemon, so the other folds are dropped and re-enumerated.
        """
        if not self.narration.on_frame(frame):
            return
        changed = False
        key = frame.get("key")
        if frame.get("op") == "read" and key is not None:
            changed = self.feed.narrating() or changed
        line = shapes.appended_line(self.narration, frame)
        if key is not None:
            shapes.bound_lines(self.narration, str(key))
        if line is not None:
            was_pending = self.feed.pending()
            changed = self.feed.on_line(line, at=time.monotonic()) or changed
            if self.feed.pending() and not was_pending:
                # First announcement of a burst: arm the coalesce window. Later
                # lines of the same burst fold into this one timer — see
                # `feed.take`, which is what makes a twelve-line probe ONE
                # re-enumeration rather than twelve.
                self.set_timer(self.feed.coalesce, self._feed_announced)
        if key == shapes.LIFECYCLE:
            self._track_boot(shapes.newest_boot_id(self.narration))
        if changed:
            self.redraw()

    def _track_boot(self, boot: str | None) -> None:
        """**A DIFFERENT DAEMON IS A DIFFERENT FOLD.**

        The boot id on a lifecycle line identifies the process. If it is not
        the one we have been mirroring, every row we hold was computed by a
        fold that no longer exists, and carrying them across would show the
        dead daemon's answers wearing the new one's provenance. The mirror is
        dropped and re-enumerated from the daemon that is actually there.

        The FIRST sighting is not a restart: the export of the `lifecycle`
        window carries this boot's own note, which can be hours old, and only
        the client knows whether it has seen a different boot id before. So
        the reset fires on a CHANGE, never on the first sighting.

        **AND NOT ON A CHANGE THE SOCKET ALREADY REPAIRED.** A boot id equal to
        the one this connection's `welcome` carried (`api.boot_id`) describes
        the daemon every mirror was exported from on THIS connection — a new
        daemon is a new socket, and `_on_link` already dropped every mirror
        when the old one died. Resetting again there would be a race, not a
        repair: the narration export lands WHILE the accounts subscribe is in
        flight, so the reset could wipe keys that had just been exported, and
        `subscribe_shapes` would decline to re-arm because `_subscribing` is
        still set. What is left is the case the arm exists for: this fold
        naming a process other than the one the socket said it was.
        """
        if not boot:
            return
        if (
            self._boot_id is not None
            and boot != self._boot_id
            and boot != self.api.boot_id
        ):
            self.shapes.reset()
            # **A NEW BOOT IS A NEW COUNTING WINDOW.** The daemon's counters
            # are since-boot, so a boot id that moved means the number this
            # board holds describes a process that is gone.
            self.warnings.reset()
            self.non_idle.reset()
            self.gauge_history.reset()
            self._history_keys.clear()
            self.subscribe_shapes()
        self._boot_id = str(boot)

    def subscribe_shapes(self) -> None:
        """**ARM THE KEYLESS SUBSCRIBE. SYNCHRONOUSLY, AND THAT IS THE POINT.**

        The flag is set here, in a plain synchronous function, and not on the
        first line of the worker. `@work` SCHEDULES a coroutine; it does not
        run it. A guard on the worker's first line is reached only after the
        event loop gets round to it — by which time a second `info` frame has
        already armed it again and `exclusive=True` has cancelled the first
        mid-`await`, so the work is never finished and never stops being
        started. A `pool_reloaded` burst is a dozen frames inside a second.
        This is the same lesson the deleted placement worker was written down
        for, and it outlives it: the flag has to be set on the same turn of the
        loop as the decision to arm, or it is not a guard at all.
        """
        if self._subscribing or self.api.shutting_down:
            return
        # ── TWO GATES, AND THE FIRST ONE IS THE S3 ONE ──────────────────────
        #
        # `SHAPES_LIST` is the READ that gets PROBED, so its state is the one
        # fact this client has about whether the daemon holds folds at all.
        # Once it is known-ABSENT, asking for a subscription is asking a daemon
        # that has already said no — and this function is now reached from the
        # announcement path as well as from `_on_caps`, so without this gate a
        # shapes-less daemon would be sent a refused `shapes/{shape}/subscribe` on
        # every `info` frame it emitted, forever. (Found by a test, not by
        # reading: `scenario_a_shapes_less_daemon_says_so_out_loud`.)
        #
        # `usable`, not `has`: UNKNOWN must pass. The probe runs on connect and
        # `_on_caps` fires after it, but this function is also reached from
        # the announcement path and from `_track_boot`, and refusing there on a
        # registry not yet repopulated would be refusing on no evidence.
        if not self.api.caps.usable(caps.SHAPES_LIST):
            return
        if not self.api.caps.usable(caps.SHAPES_SUBSCRIBE):
            return
        self._subscribing = True
        self._enumerated_at = time.monotonic()
        self.subscribe_shapes_worker()

    @work(exclusive=True, group="shapes")
    async def subscribe_shapes_worker(self) -> None:
        """**HOW THE BOARD ENUMERATES KEYS.** One request, no key.

        `shapes/accounts/subscribe` with the key OMITTED means "every
        key you hold" — the daemon's own rule (`shape.rs::do_subscribe`). The
        task hydrates if it is lazy and cold, then emits ONE EXPORT PER KEY
        addressed to this connection, and `server.rs::drive` puts each
        `(shape, key)` into this connection's `shape_subs` as that export goes
        past. So one round trip does three things that cannot be done
        separately without a race: it triggers hydration, it enumerates the
        keys, and it registers this connection for every one of their live
        frames.

        **WHY NOT `shapes/list` FIRST, WHICH IS WHAT THE DESIGN SAID.** Because
        a LAZY shape with no subscriber holds no state, and `ShapeInfo.keys` is
        therefore EMPTY until something subscribes. Listing first and
        subscribing to what it lists would subscribe to nothing at all, once,
        and then sit quiet forever. The list is still useful — it says whether
        a fold exists and reports `lost`, `reinit` and `bursts` — but it cannot
        be the enumerator. This is the largest single place the design did not
        survive contact with the built daemon; see `SHAPES-TUI.md`.

        **AND WHY IT IS RE-ISSUED.** A key that appears later — an account
        added to the pool, or a thread going non-idle on a login that had
        nothing on it — produces a `row` frame for a key this connection does
        not hold, and `drive`'s gate DROPS it. The enumeration is therefore
        re-run whenever the daemon announces that the accounts domain moved,
        which is the same `info` frame that used to schedule a five-query
        reload. `feed.py` already coalesces those into one.
        """
        # **THE WARNING SHAPE FIRST, AND ITS ABSENCE IS NOT AN ERROR.**
        #
        # Subscribed in its own `try`, before the accounts fold, so that a
        # daemon too old to carry `warnings` cannot cost this board its
        # ACCOUNT board — which is what a shared `try` would have done, since
        # the first refusal would skip the second subscribe.
        #
        # A refusal is swallowed WITHOUT a note, deliberately. "This daemon
        # does not report its warnings" is true of every daemon built before
        # today and is not something the operator can act on; a warn-level
        # note on every connection to an older daemon is exactly the noise
        # this whole feature exists to reduce. The consequence is visible in
        # the right place instead: the header draws nothing, because
        # `daemon_warnings` stays `None` rather than becoming a false zero.
        #
        # NOT re-subscribed by `resync()` (the manual `R`), and that is not an
        # omission: `resync` exists to re-ENUMERATE keys that may have
        # appeared, and this shape has one fixed key that is minted at init
        # and never joined by a second.
        try:
            await self.api.shapes_subscribe(shapes.WARNINGS)
        except (MethodAbsent, DaemonError, DaemonUnavailable, ShuttingDown):
            pass
        # **NARRATION, ONCE PER CONNECTION, IN ITS OWN `try`.** Keyless: the
        # three channels are a fixed set, so "every key" is a real inventory
        # (`narration.rs`, `KeyScope::Fleet`). Its own `try` for the warnings
        # reason — a daemon without the shape must not cost the board its
        # accounts — and swallowed without a note for the same reason: the
        # consequence is visible where it belongs, as the header's
        # `NOT subscribed` (`feed.subscribed` stays False with no export).
        if not self._narration_armed:
            self._narration_armed = True
            try:
                await self.api.shapes_subscribe(shapes.NARRATION)
            except (MethodAbsent, DaemonError, DaemonUnavailable, ShuttingDown):
                pass
        # **THE ROSTER, IN ITS OWN `try`, BEFORE THE ACCOUNT BOARD.** A daemon
        # that predates `threads/non-idle` refuses it; the cards then say
        # `? running` — the roster cell was never populated, which is the truth —
        # and the account board itself is not lost to a shared `try`. Unlike
        # `warnings` the refusal is NOTED: every running number on screen comes
        # off this shape, and "this daemon does not serve the roster" is the one
        # sentence that explains why none of them is drawn. Not re-subscribed by
        # `resync()`, for `warnings`' reason: one fixed key, minted at init.
        try:
            await self.api.shapes_subscribe(shapes.NON_IDLE)
        except DaemonError as exc:
            self.note(f"no running counts: {exc}", "warn")
        except (MethodAbsent, DaemonUnavailable, ShuttingDown):
            pass
        try:
            await self.api.shapes_subscribe(shapes.ACCOUNTS)
        except MethodAbsent:
            # The daemon has `shapes/list` and not `shapes/{shape}/subscribe`. Not a
            # state this daemon can be in, but a client that assumes the shape
            # of a daemon it is talking to is how S2a breaks.
            self.note("this daemon lists shapes but cannot be subscribed to", "warn")
        except (DaemonUnavailable, ShuttingDown):
            pass
        except DaemonError as exc:
            # **A DEAD FOLD ANSWERS HERE, NOT WITH SILENCE.** `Hub::subscribe`
            # refuses with `shape ... is dead (...)` rather than `{queued:
            # true}`, precisely so a consumer is never left waiting on a fold
            # that is gone. Shown verbatim: the daemon wrote a better sentence
            # about its own failure than this app could.
            self.note(str(exc), "warn")
        finally:
            # In `finally`, because the worker CAN be cancelled: `@work` with
            # `exclusive=True` kills the one in flight, and a flag cleared only
            # on the normal exit would stick True forever and no enumeration
            # would ever be armed again.
            self._subscribing = False

    def _on_shape(self, frame: dict) -> None:
        """A `type: shape` frame. **THE WHOLE LIVE PATH, ONCE THE MIRROR IS UP.**

        There is no reload here and no query, and since S3 there is no other
        path either. The export carries the row and its provenance; every
        change after it arrives as a row, a gone, a transition or a shared
        move, and the board redraws from held state. The five queries this
        replaced do not exist in this file any more.
        """
        # **TWO MIRRORS, ROUTED BY THE FRAME'S OWN SHAPE NAME.** Each mirror
        # ignores the other's frames and says so by returning False, so this is
        # a dispatch and not a guess. A frame for neither shape falls through
        # and is dropped exactly as it was before this one existed.
        if frame.get("shape") == shapes.NARRATION:
            self._on_narration(frame)
            return
        if self.warnings.on_frame(frame):
            self.redraw()
            return
        if self.non_idle.on_frame(frame):
            self.feed.refreshed(at=time.monotonic())
            self.redraw()
            return
        if self.gauge_history.on_frame(frame):
            key = frame.get("key")
            if key is not None:
                shapes.bound_readings(self.gauge_history, str(key))
            self.redraw()
            return
        if not self.shapes.on_frame(frame):
            return
        # The header's "read Xs ago" now measures the FOLD, not a poll: it is
        # the age of the newest frame the daemon sent us. On a quiet system it
        # grows, and that is quiet time rather than staleness — which is the
        # distinction `refresh_segment` already draws off `feed.subscribed`.
        self.feed.refreshed(at=time.monotonic())
        self.redraw()

    # ── THE PLACEMENT WORKER IS DELETED (S3) ─────────────────────────────
    #
    # `request_placement` and `place_running` lived here: a drain that issued
    # one `threads/{thread_id}/get` per running thread to learn WHICH ACCOUNT it had landed
    # on, plus the `_placing` guard that stopped four busy threads beating at
    # 1 Hz from cancelling the worker mid-`await` forever. They were the whole
    # answer to a question that no longer has to be asked, because THE
    # PLACEMENT IS THE KEY: the daemon puts a thread's row under the account it
    # was placed on, so being counted and being placed are the same act. With
    # them go `live.py`'s work list, its blocked set, `unplaced`,
    # `placement_blocked`, and the `▶ 4 running · 1 placing…` residual on the
    # header — not because the residual was wrong, but because it cannot exist:
    # a row under a key IS a placement. (A placement the daemon itself cannot
    # name is counted on `shapes/list` as `unplaced_status` and is never
    # guessed at here.)

    def _feed_announced(self) -> None:
        """The coalesce window closed. **AN ANNOUNCEMENT MEANS TWO THINGS NOW.**

        It used to mean "re-read the board" and it does not any more, because
        every fact on the board already arrived as a row. What is left is
        narrower and both halves are real:

          1. **THE KEY SET MAY HAVE MOVED.** A pool reload can add a login, and
             a thread going non-idle on a login that had nothing on it creates
             a key. `server.rs::drive` gates a live `row` on this connection
             already holding that key, so a brand-new key's rows are DROPPED
             until it is subscribed. The repair is the keyless subscribe, and
             `feed.py`'s coalesce window is what makes a twelve-frame
             `pool_reloaded` burst one of them.

          2. **A `gauge`-DOMAIN ANNOUNCEMENT USED TO INVALIDATE THE
             SPARKLINE**, because the 7-day series came from `gauge/history`,
             the board's last read. Since R2.S3d the series is the
             `accounts/gauge-history` shape and the reading arrives on its
             fold, so a `gauge` announcement means nothing to the board beyond
             the key-set rule below — which it is exempt from.

        AN ANNOUNCEMENT THAT PREDATES AN ENUMERATION IS ALREADY COVERED BY IT,
        and there are TWO cases of that. Both were measured rather than
        reasoned about, and the second one was measured against the LIVE
        daemon while proving this change.

          1. An announcement older than the LAST enumeration. Skipped by the
             stamp comparison at the bottom.

          2. **An announcement that arrives before the FIRST enumeration.**
             The firehose's `sub_ack` was exactly this (it is gone: this board
             subscribes no firehose), and a narration line can still be: the
             capability probe may be in flight, so `_enumerated_at` is None. `_on_caps`
             arms the keyless subscribe the moment that probe finishes, and
             THAT subscribe is the reseed the ack was asking for. Acting on the
             ack as well sends a second `shapes/{shape}/subscribe` per connection and
             makes the daemon re-export every key for nothing — observed on the
             live daemon as `[gauge/history, shapes/accounts/subscribe, shapes/list,
             shapes/accounts/subscribe]`, where the first subscribe raced
             ahead of the
             very probe that decides whether to send one at all.

             `_enumerated_at is None` therefore means SKIP, not "no evidence,
             so act". There is nothing to RE-enumerate before the first
             enumeration, and `_on_caps` performs that one unconditionally on
             every connection, so nothing is lost by declining here.
        """
        if not self.feed.pending():
            return
        if self.api.shutting_down:
            return
        announced = self.feed.announced_at()
        domains = self.feed.take(at=time.monotonic())
        # **NO SPARKLINE INVALIDATION ANY MORE (R2.S3d).** A `gauge` domain
        # used to clear the cached history and re-ask `gauge/history`. The
        # series is a shape now: the reading that caused this announcement
        # arrives as a `transition` on `accounts/gauge-history`, and the next
        # paint draws it. There is nothing cached to be stale.
        if self._enumerated_at is None:
            return
        # ── A `gauge` ANNOUNCEMENT CANNOT MOVE THE KEY SET ──────────────────
        #
        # The general rule is "an announcement means the KEY SET may have
        # moved", and it is deliberately conservative: a domain this client has
        # never heard of still re-enumerates, because the honest response to
        # "something you display changed" is to make sure we hold every key,
        # not to drop the frame for having an unfamiliar label.
        #
        # `gauge` is the one domain where that is demonstrably false. The keys
        # are the pool's names plus the accounts any non-idle thread row names
        # (`shapes/accounts.rs`); recording a usage observation changes
        # neither. And it is the FREQUENT one — the daemon polls usage while
        # threads are running, so on a busy pool this fires every couple of
        # minutes per account, and each re-enumeration asks the daemon to
        # re-export EVERY key at ~1-2 KB a row. Measured against the live
        # daemon: a 30-second window with one thread running produced a second
        # full re-export for a reading that moved one number, which arrives as
        # a row on its own anyway.
        #
        # The sparkline invalidation above is NOT skipped with it: that one is
        # the whole reason the domain is still carried.
        if domains and domains <= {"gauge"}:
            return
        if announced is None or announced > self._enumerated_at:
            self.subscribe_shapes()

    def action_quit_app(self) -> None:
        self.api.begin_shutdown()
        self.exit()

    # ── data ─────────────────────────────────────────────────────────────────

    async def _maybe(self, coro, method: str):
        """Call an API method, tolerating exactly one thing: its absence.

        A method that is not on the wire yields None and a note. Every other
        failure propagates, because a transient daemon fault must not be
        mistaken for a missing feature — that mistake would permanently grey
        out a working affordance.
        """
        try:
            return await coro, None
        except MethodAbsent as exc:
            return None, exc.note
        except (DaemonUnavailable, ShuttingDown):
            raise
        except DaemonError as exc:
            return None, str(exc)

    async def resync(self) -> bool:
        """**FORCE A FRESH EXPORT OF EVERY KEY, AND WAIT FOR IT TO LAND.**

        This is what `reload()` used to be, and it is deliberately the same
        SHAPE of act from the operator's side — "go and get the current state,
        cheaply" — with none of the same mechanism. `reload()` issued five
        queries against the daemon's own tables and joined them here. This
        issues ONE keyless `shapes/{shape}/subscribe`, which is the daemon's own rule
        for "every key you hold": the task flushes the per-key debounce and
        emits one export per key addressed to this connection. The board is
        then repainted from those exports, by the same code path that paints it
        when nobody pressed anything.

        **IT RETURNS WHETHER THE EXPORTS ACTUALLY ARRIVED**, and every caller
        must say which. `{"queued": true}` on the request id means the item is
        in the FIFO, NOT that the snapshot has been taken — so a verdict that
        read the board on the strength of the response would be reading the
        state from before the operation it is reporting on. That is the
        verdict-wait defect in the module docstring, arriving by a new door,
        and it is closed the same way: the wait is on the EXPORT COUNTER moving
        (`ShapeMirror.exports`), with a deadline, and a caller that times out
        says "I don't know yet" rather than reporting stale state confidently.

        **A DEAD FOLD ANSWERS HERE, AND THE ANSWER IS THE POINT.**
        `Hub::subscribe` refuses a dead shape with a sentence of its own —
        *"shape `accounts` is dead (...); it cannot be subscribed to until the
        daemon restarts"* — and that sentence is shown verbatim rather than
        paraphrased. At S2b this key fell back to the five queries instead,
        which repainted the screen with fresh numbers and HID the death. There
        is no fallback now, which means there is nothing left that can hide it.
        """
        if self.refreshing or self.api.shutting_down:
            return False
        if not self.api.connected:
            self.note("the daemon is not connected — cannot re-read the board", "warn")
            self.redraw()
            return False
        if not self.api.caps.usable(caps.SHAPES_SUBSCRIBE):
            self.note(self.api.caps.note(caps.SHAPES_SUBSCRIBE), "warn")
            self.redraw()
            return False

        before = self.shapes.exports
        self.refreshing = True
        # The same synchronous arming as `subscribe_shapes`, and for the same
        # reason: an `info` burst landing while this is in flight must not fire
        # a second enumeration that cancels this one.
        self._subscribing = True
        self._enumerated_at = time.monotonic()
        self.redraw()
        try:
            await self.api.shapes_subscribe(shapes.ACCOUNTS)
        except MethodAbsent:
            self.note(self.api.caps.note(caps.SHAPES_SUBSCRIBE), "warn")
            return False
        except (DaemonUnavailable, ShuttingDown):
            return False
        except DaemonError as exc:
            # The daemon's refusal, verbatim. It is the authority on its own
            # folds, and it wrote a better sentence about this failure than
            # this app could.
            self.note(str(exc), "warn")
            return False
        finally:
            self._subscribing = False
            self.refreshing = False
            self.redraw()

        # THE EXPORTS ARE NOT IN THE RESPONSE. They arrive as frames after it,
        # so this waits for the mirror to say it took one.
        loop = asyncio.get_running_loop()
        deadline = loop.time() + RESYNC_DEADLINE
        while self.shapes.exports == before:
            if loop.time() >= deadline or self.api.shutting_down:
                return False
            await asyncio.sleep(0.05)
        return True

    def _project(self, now) -> None:
        """Rebuild `self.board` from held shape state. **THE ONLY BUILDER.**

        `build_board` and its five payloads are gone; this runs once per paint.
        It runs per PAINT rather than per frame because two of its outputs are
        SUBTRACTIONS AGAINST NOW — `days_left` off `refresh_expires_at_ms`,
        `available` off the limit window — and the wire deliberately carries
        instants rather than pre-computed numbers. A `days_left` baked into a
        payload ages the moment it is serialised; an instant does not, and the
        price of that honesty is that somebody has to subtract.

        **THE SELECTION IS HELD BY NAME, NOT BY INDEX.** The roster can change
        under a board that is sitting open — more easily now, since a key
        appears the moment a thread goes non-idle on a login — and every key on
        this board acts on the selection. An index held across a projection
        silently moves the cursor to a different account, and `d` disables the
        one it is on.

        **AND IT CARRIES WHY THERE IS NO BOARD, WHEN THERE IS NONE.** The
        absent note is read out of the capability registry rather than written
        here, so the sentence the operator reads about a shapes-less daemon is
        the same one every other absent capability produces.
        """
        keep = self.selected_label()
        absent = (
            self.api.caps.note(caps.SHAPES_LIST)
            if self.api.caps.state(caps.SHAPES_LIST) is caps.State.ABSENT
            else None
        )
        self.board = project(
            self.shapes,
            now=now,
            absent_note=absent,
            warnings=self.warnings,
            non_idle=self.non_idle,
        )
        if keep is not None:
            for index, account in enumerate(self.board.accounts):
                if account.label == keep:
                    self.selected = index
                    break
        if self.selected >= len(self.board.accounts):
            self.selected = max(0, len(self.board.accounts) - 1)
        # The 7-day history is KEYED by label on the producer, so a label that
        # left the roster is a key the daemon sends `gone` for, and a renamed
        # account is a new key `_arm_history` subscribes. No client cache to
        # prune — and this is where a new card's week is subscribed, because
        # this is where the card comes into existence.
        self._arm_history()

    def note(self, text: str, severity: str = "warn") -> None:
        """Set the one-line status note, WITH its severity.

        The severity is the point. Everything used to land on this line as a
        warning, including confirmations of things he had just successfully
        done — see `render.warning_line`.
        """
        self.status_note = text
        self.status_severity = severity

    # ── `do_resync` AND `do_refresh` STOOD HERE (the `r` and `R` keys) ──
    #
    # Both are deleted with their bindings. `do_resync` forced a
    # re-subscribe so the daemon would re-export every key; `do_refresh`
    # called `gauge/probe` so the daemon would run a real `claude /usage`
    # against every login, and carried the banner that said so. The argument
    # for removing them rather than hiding them is in the binding table.
    #
    # WHAT THEIR ABSENCE ASSERTS: that every change to this board arrives
    # because the daemon pushed it, and that readings are made on the
    # daemon's own schedule. Both halves are now the producer's obligation,
    # and this board has kept no way to cover for it failing either one.

    # ── the 7-day history, for the selected card ─────────────────────────────

    def history_extras(self, account, now) -> render.CardExtras:
        """The selected card's 7-day history, READ OFF THE FOLD at paint time.

        R2.S3d: this was `load_extras`, a worker that sent `gauge/history` for
        the selected account and cached the answer until a `gauge` announcement
        cleared it. The series is the `accounts/gauge-history` shape now, so
        there is nothing to fetch and nothing to cache: the card draws what the
        mirror holds for its key, and a recorded reading moves it because it is
        appended to that key's `readings` on the fold.

        Three answers, kept apart as the three-valued unknown requires:

          | the fold holds                        | the card says                       |
          |---------------------------------------|-------------------------------------|
          | no export for this key                | `reading history…` (in flight)      |
          | an export, `readings` NOT known       | `?` / `✕` `no history`              |
          | a known series, empty                 | `no observations in the last 7 days`|
          | a known series, with readings         | the sparkline                       |

        The second row is the one the producer really sends for a key it
        refused (an account outside the pool) or could not read: a keyed,
        row-less, cell-less export (`shapewire::hub`, "exporting a row-less
        frame so the consumer is told"). It used to fall into the third row —
        an unknown week drawn as an empty one (R2.S3d inspect, finding 1).
        """
        label = account.label
        if label not in self.gauge_history.keys():
            return render.CardExtras(loaded=False)
        state = self.gauge_history.cell(label, shapes.READINGS)
        if not state.known:
            return render.CardExtras(loaded=True, history_state=state)
        payload = history_payload(
            self.gauge_history.row(label).get(shapes.READINGS),
            label,
            until=now,
            complete=state.complete,
        )
        return render.CardExtras(
            sparkline=history_sparkline(payload, label),
            projection=burn_projection(payload, label, account.axes.get("weekly_all")),
            loaded=True,
            truncated=history_truncated(payload, label),
        )

    def _arm_history(self) -> None:
        """Subscribe each account's week ONCE per connection — keyed, per card.

        Synchronous, for `subscribe_shapes`' reason: the key set is updated on
        the same turn as the decision, so a burst of account rows arms each key
        once. A key whose subscribe is refused stays in the set — the card then
        says "reading history…" rather than the board retrying a refusal.
        """
        if self.api.shutting_down:
            return
        if not self.api.caps.usable(caps.SHAPES_SUBSCRIBE):
            return
        fresh = [
            a.label
            for a in self.board.accounts
            if a.label not in self._history_keys
        ]
        if not fresh:
            return
        self._history_keys.update(fresh)
        self.subscribe_history_worker(fresh)

    @work(group="history")
    async def subscribe_history_worker(self, labels: list[str]) -> None:
        for label in labels:
            try:
                await self.api.shapes_subscribe(shapes.GAUGE_HISTORY, key=label)
            except (MethodAbsent, DaemonError, DaemonUnavailable, ShuttingDown):
                # A daemon without the shape: the card says "reading history…"
                # and nothing else is lost. Not noted — every daemon built
                # before R2.S3d is this daemon, and the operator cannot act on it.
                pass

    # ── `settle()` IS FOLDED INTO `resync()` (S3) ────────────────────────
    #
    # It waited for an in-flight `reload()` to finish so a verdict read fresh
    # state, and the pair `await reload(); settled = await settle()` was how
    # every verdict in this file was written. With one source there is one act:
    # `resync()` asks for the export AND waits for it, and returns whether it
    # landed. Splitting the ask from the wait is what let the two drift apart
    # in the first place — see the verdict-wait note in the module docstring —
    # so they are deliberately not two functions any more.

    # ── selection ────────────────────────────────────────────────────────────

    def selected_account(self):
        if not self.board.accounts:
            return None
        self.selected = max(0, min(self.selected, len(self.board.accounts) - 1))
        return self.board.accounts[self.selected]

    def selected_label(self) -> str | None:
        account = self.selected_account()
        return account.label if account else None

    def move_selection(self, delta: int) -> None:
        if not self.board.accounts:
            return
        self.selected = (self.selected + delta) % len(self.board.accounts)
        self.redraw()

    def select_number(self, number: int) -> bool:
        """Select the account whose PRINTED number is `number`. 1-based.

        **THE OFF-BY-ONE THIS AVOIDS.** `self.selected` is a 0-BASED index into
        `board.accounts`, but the number drawn on each row (`account.number`) is
        1-BASED. The obvious implementation — `self.selected = number` — quietly
        selects the row BELOW the one he pointed at, and on the last row selects
        nothing at all. So the number is resolved through `Board.by_number`,
        which is the model's own bridge between the two, and the index is then
        derived from the account that lookup returned.

        Returns whether anything was selected, so a digit naming no row can be a
        no-op rather than a wrong move.
        """
        account = self.board.by_number(number)
        if account is None:
            return False
        # IDENTITY, not equality. `Account` is a dataclass, so `list.index()`
        # compares field-by-field and would return the FIRST row that happens to
        # match — and two rows genuinely can, since a slot and the machine
        # default can carry identical values. `by_number` handed back an object
        # out of this exact list, so `is` is both correct and exact.
        for index, candidate in enumerate(self.board.accounts):
            if candidate is account:
                self.selected = index
                self.redraw()
                return True
        return False  # pragma: no cover — by_number came from this list

    # ── operations ───────────────────────────────────────────────────────────

    # `@work` is required, not stylistic: `push_screen_wait` refuses to run
    # outside a worker, and these three are the methods that open a modal and
    # wait for an answer. Running them as workers also keeps the operation off
    # the message-handling path, so the board keeps redrawing while a modal or
    # a terminal handoff is up.
    @work
    async def do_login(self) -> None:
        account = self.selected_account()
        if account is None:
            return
        if account.is_machine_default:
            self.note("that is the machine login; use s to change it", "warn")
            self.redraw()
            return

        choice = login.LoginChoice(label=account.label)

        # The modal is used only when BOTH halves of the API flow exist. With
        # only one of them, the terminal handoff is used — a login that can be
        # started but not completed is worse than one that never started.
        can_modal = self.api.caps.usable(caps.LOGIN_BEGIN) and self.api.caps.usable(
            caps.LOGIN_COMPLETE
        )
        if can_modal:
            try:
                begun = await self.api.login_begin(account.label)
                url = (begun or {}).get("url") or (begun or {}).get("login_url")
                # THE SESSION ID IS CARRIED, NOT RE-DERIVED. `complete` is keyed
                # on the session and the daemon deleted its `account` fallback,
                # so this reply is the only place the id exists. No url and no
                # session id both mean the same thing here — fall through to the
                # terminal handoff, which needs neither.
                session_id = (begun or {}).get("session_id")
                if url and session_id:
                    code = await self.push_screen_wait(login.LoginModal(account.label, url))
                    if code is None:
                        self.note("login cancelled", "warn")
                        self.redraw()
                        return
                    await self.api.login_complete(session_id, code)
                    await self._verdict_after_login(account.label)
                    return
            except MethodAbsent:
                pass  # fall through to the terminal handoff
            except (DaemonUnavailable, ShuttingDown):
                return
            except DaemonError as exc:
                self.note(f"login refused: {exc}", "warn")
                self.redraw()
                return

        argv = login.terminal_login_argv(choice)
        await login.run_terminal_login(
            self, argv, f"claude auth login for {account.label}"
        )
        await self._verdict_after_login(account.label)

    async def _verdict_after_login(self, label: str) -> None:
        # ONE ACT, NOT TWO. `resync()` asks the daemon for a fresh export of
        # every key and waits for it to land; the board below is then the fold
        # as of AFTER the login, which is the only state a verdict may be read
        # from. It answers False rather than throwing when nothing arrived, and
        # this says so out loud — a verdict that says "I do not know yet" is
        # useful and a confident wrong one is not.
        settled = await self.resync()
        account = self.board.by_label(label)
        if not settled:
            self.note(
                f"{label}: the board was not re-exported within "
                f"{RESYNC_DEADLINE:.0f}s, so this verdict is not known. There is no refresh\n"
                f"key: the card corrects itself on the daemon's next push",
                "warn",
            )
        elif account is None:
            self.note(f"{label}: no longer listed", "warn")
        elif account.state == "live":
            # A success he caused. Not a warning. See `render.warning_line`.
            self.note(f"{label} is signed in", "ok")
        elif account.state == "?":
            # accounts/health is what reports login state. Without it, claiming
            # success or failure would be a guess.
            self.note(
                f"{label}: the fold has not hydrated this account's credential "
                "cell, so the result cannot be confirmed here",
                "warn",
            )
        else:
            self.note(f"{label}: {account.health_note or account.state}", "warn")
        self.redraw()

    @work
    async def do_new_account(self) -> None:
        """`n` — CREATE AN ACCOUNT AND SIGN IT IN, without leaving the board.

        **THIS IS THE SAME FLOW AS `l`, GIVEN A NAME TO WORK ON.** It is
        deliberately not a second mechanism: `accounts/login/begin` on a label
        that has no slot yet already does the whole job, because the script it
        drives (`scripts/add-account --login`) creates the slot directory before
        it invokes `claude`, and runs `threads accounts adopt` once a genuinely
        distinct grant has landed. Registration, the clone refusal, the keychain
        baseline and the identity verification all stay exactly where they are.

        So the only thing this adds is somewhere to type the name — and the
        validation that makes a refusal arrive BEFORE a browser round trip
        rather than after one.
        """
        existing = [a.label for a in self.board.accounts]
        label = await self.push_screen_wait(login.NewAccountModal(existing))
        if label is None:
            return

        can_modal = self.api.caps.usable(caps.LOGIN_BEGIN) and self.api.caps.usable(
            caps.LOGIN_COMPLETE
        )
        if not self.api.connected:
            self.note("the daemon is not connected — cannot create an account", "warn")
            self.redraw()
            return

        if can_modal:
            try:
                begun = await self.api.login_begin(label)
                url = (begun or {}).get("url") or (begun or {}).get("login_url")
                # See `do_login`: the session id comes from this reply and
                # nowhere else, so it is carried to `complete` rather than the
                # label being sent again.
                session_id = (begun or {}).get("session_id")
                if url and session_id:
                    code = await self.push_screen_wait(login.LoginModal(label, url))
                    if code is None:
                        # The slot directory may exist by now; the account is
                        # NOT registered, because `adopt` only runs after a
                        # grant lands. Say that rather than leaving him to
                        # wonder what half-happened.
                        self.note(
                            f"cancelled — {label} was not registered. Press n to try again",
                            "warn",
                        )
                        self.redraw()
                        return
                    result = await self.api.login_complete(session_id, code)
                    await self._verdict_after_new_account(label, result)
                    return
            except MethodAbsent:
                pass  # fall through to the terminal handoff
            except (DaemonUnavailable, ShuttingDown):
                return
            except DaemonError as exc:
                # `add-account`'s refusals name their own remedy — the
                # CLAUDE_CODE_OAUTH_TOKEN guard names the variable to unset.
                # Paraphrasing would lose it, so this is verbatim.
                self.note(f"refused: {exc}", "warn")
                self.redraw()
                return

        # `new=True`: no `--force --allow-same-account`. Those two exist to get
        # past refusals about PRIOR STATE, and a slot that does not exist yet
        # has none.
        argv = login.terminal_login_argv(login.LoginChoice(label=label), new=True)
        await login.run_terminal_login(self, argv, f"create {label} and sign it in")
        await self._verdict_after_new_account(label, None)

    async def _verdict_after_new_account(self, label: str, result) -> None:
        """Did the account actually land — and can it be PLACED on yet?

        Two questions that used to need two reads, and now need none. The
        board was spined on `accounts/health`, which re-reads the registry from
        disk, so a freshly adopted account appeared as a card immediately —
        while PLACEMENT came from a pool this board could only see through a
        separate `accounts/status`. Reporting "signed in" off the first without
        the second was true and useless, so the verdict asked twice.

        Under the shape there is one key space and it is the daemon's own
        account list, so "is there a card" and "is it in the pool" are the same
        question asked once. See the note at the check itself.
        """
        if isinstance(result, dict) and result.get("ok") is False:
            out = (result.get("output") or "").strip().splitlines()
            why = out[-1] if out else f"exit {result.get('exit_code')}"
            self.note(f"{label} was not registered: {why}", "warn")
            self.redraw()
            return

        settled = await self.resync()
        if not settled:
            self.note(
                f"{label}: the board was not re-exported within "
                f"{RESYNC_DEADLINE:.0f}s, so the card below is not confirmed current. "
                "There is no refresh key: it corrects itself on the daemon's next push",
                "warn",
            )
            self.redraw()
            return

        # ── ONE QUESTION, ONE SOURCE: IS IT A KEY? ──────────────────────────
        #
        # This used to be TWO reads with two different answers — the board said
        # whether the account existed and a separate `accounts/status` said
        # whether it was in the placement pool — and the whole reason for the
        # second read was that those could disagree.
        #
        # They cannot disagree now, because they are the same fact. The shape's
        # key space is the daemon's own account list (`pool().names()`, union
        # the accounts any non-idle thread row names), so a card EXISTING is a
        # statement that the pool holds that account. There is no second query
        # to issue and no "cannot tell" branch to guess in.
        if self.board.by_label(label) is not None:
            self.note(f"{label} is signed in and in the pool", "ok")
        else:
            self.note(
                f"{label} is signed in, but the daemon's account list does not "
                "carry it yet, so it is not in placement. The daemon announces "
                "a pool reload on its own and the card appears when it does; "
                "and see the login output if it does not. There is no refresh key "
                "on this board",
                "warn",
            )
        self.redraw()

    @work
    async def do_set_live(self) -> None:
        """THE CARVE-OUT. See threads_tui.local."""
        account = self.selected_account()
        if account is None:
            return
        pre = await local.preflight(account.label)
        if not pre.ok:
            self.note(f"refused: {pre.message}", "warn")
            self.redraw()
            return
        if pre.already_live:
            # No browser trip: the machine login already IS this account. Run
            # the real command once so the tool records the name (H179).
            rec = await local.record_already_live(account.label)
            if rec.ok:
                self.note(f"{account.label} is already your machine login", "ok")
            else:
                self.note(
                    f"{account.label} is already your machine login, but its "
                    f"name was not recorded: {rec.message}",
                    "warn",
                )
            await self.resync()
            self.redraw()
            return
        argv = local.set_live_argv(account.label)
        await login.run_terminal_login(
            self, argv, f"set the machine login to {account.label}"
        )
        settled = await self.resync()
        if settled:
            self.note(f"machine login: see the board for {account.label}", "ok")
        else:
            self.note(
                "the board was not re-exported in time, so the card below is not "
                "confirmed current. There is no refresh key: it corrects itself "
                "on the daemon's next push",
                "warn",
            )
        self.redraw()

    @work
    async def do_share(self) -> None:
        account = self.selected_account()
        if account is None:
            return
        if not self.api.caps.usable(caps.ACCOUNTS_POOL_SHARE_SET):
            self.note(self.api.caps.note(caps.ACCOUNTS_POOL_SHARE_SET), "warn")
            self.redraw()
            return
        value = await self.push_screen_wait(
            ShareEditor(account.label, account.pool_share, disabled=account.disabled)
        )
        if value is None:
            return
        try:
            await self.api.pool_share_set(account.label, value)
            # ── THE ECHO IS GONE, AND THE SENTENCE CHANGED TO MATCH ─────────
            #
            # This read `result["shares"]` — the effective row off the command's
            # own reply — on the reasoning that the daemon might return a number
            # other than the one we sent, so reporting our own variable would be
            # "reporting our intent as though it were the outcome". The reasoning
            # was right. The mechanism was a second source (`docs/shapes.md`
            # 2.4), and the daemon now answers `{promise_id}`.
            #
            # WE DO NOT RE-ADD A GETTER. S3 deleted `pool_share_get` from this
            # client precisely so it has no second way to know, and asking again
            # here would reverse that for one sentence in a toast.
            #
            # So the toast no longer CLAIMS an effective number it cannot see.
            # It states what was asked and points at the card, which is fed by
            # the shape and is the one thing allowed to know. `resync()` below
            # forces that row to arrive; the card is correct within the beat,
            # and it is correct from the only source.
            #
            # The proper close is the promise this call now returns plus the
            # consumer-side `Mirror` (6.5): issue the command, hold the id, and
            # report when the frame carrying it lands. The mirror does not exist
            # yet — writing a half of it here would be the second source again,
            # wearing a different hat.
            # `ok`, NOT a warning. His words: "why is there a warning there? It
            # doesn't make sense." He set a share and it worked.
            #
            # THERE IS NO LONGER A DISABLING BRANCH HERE. This editor once had
            # two outcomes because a zero was a state; the daemon refuses a zero
            # now and the state has its own key, so a share write is a share
            # write and the sentence says the number. A disabled account can be
            # re-shared from here and stays disabled — which the modal says
            # before he types, so the verdict does not have to.
            self.note(
                f"{account.label}: share set to {value * 100:g}% "
                "— saved; the card shows what placement will use",
                "ok",
            )
            # The daemon's own fold re-exports on its own when the share write
            # rings `info{accounts, pool_share}`; this forces it so the number
            # on the card has moved by the time he looks back at it.
            await self.resync()
        except MethodAbsent as exc:
            self.note(exc.note, "warn")
        except (DaemonUnavailable, ShuttingDown):
            return
        except DaemonError as exc:
            # The daemon's refusal, verbatim. It is the authority on validity.
            self.note(f"refused: {exc}", "warn")
        self.redraw()

    @work
    async def do_disable(self) -> None:
        """**`d` — TAKE THE SELECTED ACCOUNT OUT OF THE POOL, OR PUT IT BACK.**

        **ONE KEY, BOTH DIRECTIONS.** *"It can use the same keybind as disable
        used, like a toggle."* The direction is read off the account's CURRENT
        state, which is the mirror's state, which came from the daemon — so the
        toggle is only as correct as the board's last re-read, which is why
        `resync()` below is not optional and why the toggle is proved on a
        socket rather than on this branch (`tests/test_disable.py` §8).

        THROUGH THE API, ALWAYS. This interface is a CLIENT of the daemon and
        must not reach around it: it writes no config file, touches no database,
        and holds no opinion about what a share of zero means. It calls
        `accounts/{account}/disable` / `accounts/{account}/enable` and reports what comes back.

        **THE SHAPE, NOT THE REPLY** — the same rule `do_share` follows, and
        this paragraph used to say the opposite. It read *"the daemon answers
        with the EFFECTIVE row"*, which stopped being true on 2026-09-23 when
        these two became acknowledgements (rule 2.4); the block at the call
        below was rewritten then and this was not, so the docstring was telling
        the next reader to do the one thing that change exists to prevent.

        The reply is `{promise_id, visible, changed}` and carries no row. The
        effective share — which for an enable is a number this interface never
        sent, because the daemon chose it — is read off the mirror, and
        `resync()` settles it from the shape. There is no second source to
        disagree with, which is the whole point: do NOT add a getter back to
        service the toast.
        """
        account = self.selected_account()
        if account is None:
            return
        method = caps.ACCOUNTS_ENABLE if account.disabled else caps.ACCOUNTS_DISABLE
        if not self.api.caps.usable(method):
            # The note names the share editor as the fallback, because the act IS
            # still reachable through it on an older daemon — it is the name that
            # is missing there, not the capability.
            self.note(self.api.caps.note(method), "warn")
            self.redraw()
            return
        go = await self.push_screen_wait(
            ReactivateConfirm(
                account.label,
                account.disabled,
                # THE ACCOUNT'S OWN SHARE, and it is passed as `share` because
                # that is what it is. It used to be `restore_to` — a number the
                # daemon computed for "what would an enable put this back at" —
                # and both the field and the question are deleted: neither
                # direction of this act touches `pool_shares` any more.
                share=account.pool_share,
            )
        )
        if not go:
            return
        try:
            if account.disabled:
                result = await self.api.account_enable(account.label)
            else:
                result = await self.api.account_disable(account.label)
            # ── THE REPLY IS AN ACKNOWLEDGEMENT, AND THAT IS ALL IT IS ──
            #
            # This block used to read `result["shares"]` — the new row, off the
            # command's own reply — and set both the share and the disabled
            # state from it. That was a SECOND SOURCE for two values this board
            # already mirrors, which is the thing S3 deleted from the read path
            # and left standing on the write path (`docs/shapes.md` 2.4). While
            # two ways to learn a fact exist, every disagreement is ambiguous.
            #
            # The daemon now answers `{promise_id}`. The share comes from the
            # mirror, where it already was; the state flips optimistically and
            # `resync()` below settles it from the shape. Nothing is lost: the
            # number in the tail is the same number, read from the one place
            # that is allowed to know it.
            # The id itself is NOT read here, deliberately. Matching it to the
            # frame that resolves it is the consumer half — a `Mirror` that
            # holds promises and closes them on receipt (`docs/shapes.md` 6.5) —
            # and that does not exist yet. Binding it to a variable nothing
            # waits on would be a stub pretending to work; `resync()` is the
            # honest mechanism until the mirror lands.
            effective: float | None = account.pool_share
            now_disabled = not account.disabled
            # `ok`, NEVER a warning, in both directions. His words on the share
            # editor: "why is there a warning there? It doesn't make sense." He
            # disabled an account and it worked. Disabling is a thing he meant to
            # do, not an event to be warned about.
            if now_disabled:
                tail = f", still on {effective * 100:g}%" if effective is not None else ""
                self.note(
                    f"{account.label}: DISABLED — out of placement selection from "
                    f"the next turn{tail}. Not deleted, login untouched; press d "
                    "again to reactivate",
                    "ok",
                )
            else:
                # NEVER "share is now 60%" here. He pressed the key to put the
                # account back; the sentence has to answer the question he
                # asked, with the number as the detail rather than as the
                # headline. The number is now a statement that nothing moved,
                # which is why it is worded "still on" rather than "at".
                tail = f", still on {effective * 100:g}%" if effective is not None else ""
                self.note(
                    f"{account.label}: REACTIVATED — back in placement "
                    f"selection from the next turn, on the same login{tail}",
                    "ok",
                )
            await self.resync()
        except MethodAbsent as exc:
            self.note(exc.note, "warn")
        except (DaemonUnavailable, ShuttingDown):
            return
        except DaemonError as exc:
            # The daemon's refusal, verbatim. It is the authority.
            self.note(f"refused: {exc}", "warn")
        self.redraw()

    # ── THE THREE THAT HAD NO DOOR ───────────────────────────────────────────
    #
    # `accounts/{account}/rename`, `accounts/{account}/delete`, `accounts/rotate`. All three were on
    # the wire, all three were in `caps.py`, all three had `Api` methods, and
    # none of them had a caller. See `threads_tui.slots` for the modals and the
    # pure verdict functions, and the binding table for why these keys.
    #
    # ALL THREE FOLLOW THE POSTURE THE REST OF THIS SECTION ALREADY FOLLOWS:
    # report the OUTCOME the daemon echoed rather than the request that was
    # sent; quote a refusal verbatim because the daemon's sentences name their
    # remedy; and `resync()` before reading a verdict off the board, because a
    # board read before the export lands is a report on the state BEFORE the
    # operation.

    @work
    async def do_rename(self) -> None:
        """`m` — RENAME THE SELECTED SLOT.

        The validation runs IN THE MODAL, against `slots.rename_refusal`, which
        mirrors `accounts::rename_refusal` in its own order. That is the `n`
        pattern and it is here for the same reason: this operation moves a
        directory and a keychain item, and a refusal that arrives after the move
        has started is the failure the validation exists to prevent.

        **THE `existing` SET IS THIS BOARD'S ROSTER, WHICH IS NARROWER THAN THE
        DAEMON'S.** The daemon checks the registry PLUS any unregistered
        directory under `accounts/`, because a rename onto one of those would
        silently merge two slots. This board cannot see a directory that is not
        in the fold, so a collision with one gets past the local check and is
        refused by the daemon — which is the correct division: the local check
        is an accelerator for the cases it can know, never the authority.
        """
        account = self.selected_account()
        if account is None:
            return
        if not self.api.caps.usable(caps.ACCOUNTS_RENAME):
            self.note(self.api.caps.note(caps.ACCOUNTS_RENAME), "warn")
            self.redraw()
            return
        existing = [a.label for a in self.board.accounts]
        new = await self.push_screen_wait(slots.RenameModal(account.label, existing))
        if new is None:
            # Cancelled. NOTHING was sent, so there is nothing to report and no
            # note to leave: a status line that said "rename cancelled" would be
            # this board narrating its own inaction over whatever the daemon
            # last said, which is the more useful line.
            return
        if not self.api.connected:
            self.note("the daemon is not connected — cannot rename an account", "warn")
            self.redraw()
            return
        old = account.label
        try:
            result = await self.api.accounts_rename(old, new)
        except MethodAbsent as exc:
            self.note(exc.note, "warn")
            self.redraw()
            return
        except (DaemonUnavailable, ShuttingDown):
            return
        except DaemonError as exc:
            # VERBATIM. `rename_refusal` on the daemon's side writes the sentence
            # that names the remedy — "rename the account it is signed in as
            # instead" — and a paraphrase loses exactly that half.
            self.note(f"refused: {exc}", "warn")
            self.redraw()
            return

        line, severity = slots.rename_verdict(result, old, new)
        settled = await self.resync()
        if not settled:
            self.note(
                f"{line} — but the board was not re-exported within "
                f"{RESYNC_DEADLINE:.0f}s, so the row below is not confirmed current. "
                "There is no refresh key: it corrects itself on the daemon's next push",
                "warn",
            )
            self.redraw()
            return
        # READ IT BACK. The daemon said it renamed; the fold is what says the
        # pool now holds that name, and those are two different claims.
        if self.board.by_label(new) is None:
            self.note(
                f"{line} — but the daemon's account list does not carry {new!r} yet, "
                "so it is not in placement. Press r if the row does not appear",
                "warn",
            )
        else:
            self.note(line, severity)
        self.redraw()

    @work
    async def do_delete(self) -> None:
        """`X` — DELETE THE SELECTED SLOT. The only irreversible key here.

        **NO `force`, EVER.** `Api.accounts_delete` has no such parameter, so
        there is nothing here that could send one. The daemon refuses to delete
        the slot the machine login is signed in as and its refusal names the
        remedy; that refusal is shown and the board stops. See `slots`.
        """
        account = self.selected_account()
        if account is None:
            return
        # THE ONE LOCAL REFUSAL: the machine-default row is not a slot. Every
        # other refusal is the daemon's, deliberately.
        why = slots.delete_refusal(account.label, account.is_machine_default)
        if why:
            self.note(why, "warn")
            self.redraw()
            return
        if not self.api.caps.usable(caps.ACCOUNTS_DELETE):
            self.note(self.api.caps.note(caps.ACCOUNTS_DELETE), "warn")
            self.redraw()
            return
        go = await self.push_screen_wait(
            slots.DeleteConfirm(
                account.label,
                is_live_login=account.is_live_login,
                disabled=account.disabled,
            )
        )
        if not go:
            return  # Nothing was sent. See `do_rename`.
        if not self.api.connected:
            self.note("the daemon is not connected — cannot delete an account", "warn")
            self.redraw()
            return
        label = account.label
        try:
            result = await self.api.accounts_delete(label)
        except MethodAbsent as exc:
            self.note(exc.note, "warn")
            self.redraw()
            return
        except (DaemonUnavailable, ShuttingDown):
            return
        except DaemonError as exc:
            # VERBATIM, and this is the one where it matters most. The live-login
            # refusal is four sentences long and the last one is the instruction:
            # point the machine login somewhere else first. Summarising it to
            # "refused: cannot delete the live login" would throw away the only
            # part that tells him what to do.
            self.note(f"refused: {exc}", "warn")
            self.redraw()
            return

        line, severity = slots.delete_verdict(result, label)
        settled = await self.resync()
        if not settled:
            self.note(
                f"{line} — the board was not re-exported within "
                f"{RESYNC_DEADLINE:.0f}s, so the roster below is not confirmed "
                "current. There is no refresh key: it corrects itself on the "
                "daemon's next push",
                "warn",
            )
            self.redraw()
            return
        # READ IT BACK. A row that is still there after a reported delete is the
        # failure worth catching: the daemon reloads its pool before it answers,
        # so a surviving card means placement can still rank an account whose
        # directory is gone.
        if self.board.by_label(label) is not None:
            self.note(
                f"{line} — but {label!r} is STILL on the daemon's account list, so it "
                "may still be rankable by placement. Press r, and check "
                "`threads accounts list`",
                "warn",
            )
        else:
            self.note(line, severity)
        self.redraw()

    @work
    async def do_rotate(self) -> None:
        """`O` — REFRESH EVERY SLOT'S CREDENTIAL.

        **POOL-WIDE, NOT PER-ACCOUNT.** The selection is not the subject and
        this never sends one: `accounts/rotate` reads `dry_run` and nothing else.

        **THE PROGRESS IS NOT THE VERDICT.** The pass emits `Push::Progress`
        frames as it reaches each slot, and `feed.py` already renders them on the
        narration line — that is what it survived S3 for, and it is why nothing
        here subscribes to anything or reads a log. The verdict comes from the
        `--json` result this call returns, through `slots.rotate_verdict`, which
        never looks at a log line.
        """
        if not self.api.caps.usable(caps.ACCOUNTS_ROTATE):
            self.note(self.api.caps.note(caps.ACCOUNTS_ROTATE), "warn")
            self.redraw()
            return
        if self.rotating:
            self.note("a rotation pass is already running on this board", "warn")
            self.redraw()
            return
        choice = await self.push_screen_wait(
            slots.RotateConfirm(len(self.board.accounts))
        )
        if choice is None:
            return  # Nothing was sent. See `do_rename`.
        if not self.api.connected:
            self.note("the daemon is not connected — cannot rotate keys", "warn")
            self.redraw()
            return
        dry = choice == "dry"
        self.rotating = True
        # SAID BEFORE THE AWAIT, so the first frame after the button already
        # carries it. A pass can run for minutes and the alternative is a board
        # that looks idle while something is invoking `claude` across the pool.
        self.note(
            "rehearsing a rotation (dry run) — the ⚡ line narrates each slot"
            if dry
            else "rotating keys across every slot — the ⚡ line narrates each slot as "
            "the daemon reaches it",
            "ok",
        )
        self.redraw()
        try:
            result = await self.api.accounts_rotate(dry_run=dry)
        except MethodAbsent as exc:
            self.note(exc.note, "warn")
            self.redraw()
            return
        except (DaemonUnavailable, ShuttingDown):
            return
        except DaemonError as exc:
            # VERBATIM. The pass's own refusals are the useful ones: "another
            # rotation pass is running (lock: …). It is safe to wait." and the
            # daemon's "the rotation pass produced no parseable JSON result. It
            # said: …", which carries the child's own output.
            self.note(f"refused: {exc}", "warn")
            self.redraw()
            return
        finally:
            self.rotating = False

        line, severity = slots.rotate_verdict(result)
        if dry:
            # A REHEARSAL CHANGED NOTHING AND MUST NOT ASK THE BOARD TO MOVE.
            # The daemon deliberately does not announce a dry run — telling
            # clients to re-read after a rehearsal is what would make the two
            # modes indistinguishable from outside — so this does not resync
            # either. Reporting the result is the whole of it.
            self.note(line, severity)
            self.redraw()
            return
        settled = await self.resync()
        if not settled:
            self.note(
                f"{line}. The board was not re-exported within "
                f"{RESYNC_DEADLINE:.0f}s, so the days-left column below is not "
                "confirmed current. There is no refresh key: it corrects "
                "itself on the daemon's next push",
                "warn",
            )
        else:
            self.note(line, severity)
        self.redraw()

    # ── rendering ────────────────────────────────────────────────────────────

    def redraw(self) -> None:
        try:
            screen = self.screen
        except Exception:
            return
        # ── THE PROJECTION, ONCE PER PAINT. THE ONLY WAY A BOARD IS BUILT. ──
        #
        # See `_project` for why it is per paint rather than per frame. It is
        # ABOVE the screen dispatch because the pressure view reads
        # `self.app.board` too and takes the early return below; projecting
        # after that return would leave that whole view frozen at whatever the
        # board was when it was opened, which is the class of failure the shape
        # exists to end.
        self._project(now_utc())
        if isinstance(screen, PressureScreen):
            screen.render_now()
            return
        if not isinstance(screen, BoardScreen):
            return
        try:
            body = screen.query_one("#body", Static)
        except Exception:
            return

        now = now_utc()
        at = time.monotonic()
        # The CONTENT width, not the terminal width. See the module docstring.
        width = content_width(screen)
        mode = render.layout_mode(width, max(10, self.size.height))

        # ── THE RUNNING COUNT: THE CARDS ARE THE SOURCE, THE HEADER IS THE SUM
        #
        # There is no `placed` map and no `unplaced` residual here any more,
        # and both deletions are the same fact. A thread row lives under the
        # account it was placed on, so being counted and being placed are ONE
        # act: the header total is `sum(card)` by construction, and two numbers
        # on one screen cannot disagree about one fact because there is only
        # one number, added up twice.
        #
        # What this replaces is worth keeping written down, because it is the
        # defect the operator reported — *"I have to click refresh to get my
        # counts of active running on each account to update in the UI"*. The
        # board held a client-side lease map (`live.py`), a per-thread
        # placement lookup that livelocked, and an all-or-nothing gate that
        # made ONE unresolved thread blank the live number on EVERY card and
        # fall back to a polled one that only a reload could move. S2 replaced
        # the mechanism; S3 removes the fallback, so there is nothing left that
        # can quietly answer this question a second way.
        #
        # `running_live` is `exported`, not `True`: before the first export has
        # landed, 0 means "nobody has told us" and must not be drawn as a
        # confident zero. That is the same three-valued honesty the cells have,
        # applied to the header's one number.
        running = sum(a.activity.executing for a in self.board.accounts)
        # THE POOL'S QUEUE DEPTH, summed exactly as `running` is and for the same
        # reason: it is one fact about the whole pool. It is summed from the same
        # per-account rows the old per-card `● queued N` badge drew FROM, which is
        # the point — the numbers were never wrong, the ATTRIBUTION was. Bucketed
        # under a card they claimed an account had committed to run that work;
        # added up here they claim only what is true, that the pool has this many
        # threads waiting. `queued_threads`, never `queued_messages`: the queue is
        # keyed by `thread_id` and threads are the unit it is measured in.
        queued = self.board.queue_depth()
        running_live = self.shapes.exported

        header = render.HeaderState(
            link=self.api.state,
            retries=self.api.retries,
            refreshing=self.refreshing,
            tick=self.tick,
            running=running,
            queued=queued,
            # Parked is a POOL fact (H179), summed like `queued`, never per card.
            parked=self.board.parked_count(),
            placeable=self.board.placeable(),
            running_live=running_live,
            subscribed=self.feed.subscribed,
            refresh_age=self.feed.refresh_age(at=at),
        )

        lines: list[Text] = [
            render.header_line1(self.board, header, width, now),
            render.header_line2(self.board, width, now),
        ]
        for severity, text in self.board.warnings():
            lines.append(render.warning_line(severity, text, width))

        # ── THERE IS NO BOARD, AND THE SCREEN SAYS WHICH KIND OF NO ─────────
        #
        # An empty `accounts` list is three different facts and they must not
        # look alike: this daemon has no folds to mirror (S3's honest
        # consequence — there is no fallback and the board says so), the fold
        # has not exported yet (un-hydrated, which is not empty), or the daemon
        # genuinely holds no accounts (empty, which is a fact). See
        # `render.no_board_lines`.
        if not self.board.accounts:
            lines.extend(render.no_board_lines(self.board, width))

        # WHICH CARD CARRIES THE HEADROOM READING. Computed ONCE for the whole
        # paint, not per card: `Board.room()` is a ranking over every account, so
        # asking it inside the loop would be the same comparison four times and
        # would let two cards claim it if the list moved mid-paint. The header no
        # longer draws this — see `render.header_line2` — and the account's own
        # card does, which is where the operator said it belongs.
        room = self.board.room()
        room_leader = room[0].label if room is not None else None
        for index, account in enumerate(self.board.accounts):
            selected = index == self.selected
            lines.extend(
                render.card_lines(
                    account,
                    width,
                    selected=selected,
                    mode=mode,
                    warn_below=self.board.warn_below_days,
                    # The history is drawn on the selected card only, off the
                    # `accounts/gauge-history` fold (`history_extras`).
                    extras=self.history_extras(account, now) if selected else None,
                    flashing=None,
                    now=now,
                    # `live_running` WAS the client-side lease map's per-account
                    # number, handed in beside the polled one so the card could
                    # prefer it. There is no second number to prefer any more:
                    # `account.activity.executing` IS the live count, counted
                    # over the rows the daemon put under this key. Passing None
                    # is what tells the card to draw its own.
                    live_running=None,
                    room_leader=(
                        room_leader is not None and account.label == room_leader
                    ),
                )
            )

        for note in self.board.notes:
            lines.append(render.warning_line("warn", note, width))

        # THE LIVE NARRATION LINE — what the daemon last told us, as it told us.
        # Kept SEPARATE from `status_note`, which carries the verdict of an
        # operation the operator himself started (a login result, a share
        # confirmation). Folding the two would let a background gauge reading
        # overwrite "cc1 is signed in" a second after he read it, which is the
        # one thing on this screen he is waiting for.
        live_note = self.feed.note(at=at)
        if live_note:
            lines.append(render.live_line(live_note, width))

        if self.status_note:
            lines.append(
                render.warning_line(self.status_severity, self.status_note, width)
            )

        body.update(render.join(lines))

    async def on_unmount(self) -> None:
        await self.api.stop()


def main() -> int:
    BoardApp().run()
    return 0
