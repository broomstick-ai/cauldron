"""**THE THREE METHODS THAT HAD NO DOOR: rename, delete, rotate.**

`accounts/{account}/rename`, `accounts/{account}/delete` and `accounts/rotate` were on the wire,
declared in `caps.py`, given `Api` methods — and called by nothing. This file
covers the doors that were added, and it is organised around the four failures
that are actually available here, all of which are quiet:

  1. **A VALIDATOR THAT DRIFTS.** `slots.rename_refusal` mirrors
     `accounts::rename_refusal` (src/accounts.rs), which in turn calls
     `validate_account_name`. If they disagree, the operator types a name this
     modal accepts and the daemon then refuses — after the round trip the
     validation exists to prevent. Every case below is one the Rust also
     refuses, in the order the Rust refuses it.

  2. **A CANCEL THAT IS NOT A CANCEL.** Escape on any of these three modals must
     leave the daemon untouched. This is asserted against a real socket by
     reading the METHOD LIST the daemon recorded, not by trusting a return
     value: "nothing was sent" is a claim about the wire.

  3. **A REFUSAL THAT GETS PARAPHRASED.** The daemon's refusals name their own
     remedy — `delete_refusal`'s live-login sentence ends with the exact command
     to run first — so the board must show them verbatim. A summary is the one
     rewrite that reliably deletes the useful half.

  4. **A VERDICT THAT REPORTS THE REQUEST.** Every one of these reads its
     outcome off the daemon's echo. `rename_verdict` says `history_keeps_old_name`
     ONLY when the payload carries it; `delete_verdict` reports each removal
     from its own boolean; `rotate_verdict` reads the `--json` blob and never a
     log line.

**NOTHING IN THIS FILE SUBMITS AN OPERATION AGAINST A REAL DAEMON.** The socket
half runs against a fake that records what it was asked. The refusal path is
driven by making that fake refuse.

Run: .venv/bin/python tests/test_slots.py   (from threads-app/)
     or: pytest -q tests/test_slots.py
"""

from __future__ import annotations

import asyncio
import json
import sys

sys.path.insert(0, __file__.rsplit("/tests/", 1)[0])
sys.path.insert(0, __file__.rsplit("/test_slots.py", 1)[0])

import fake_daemon  # noqa: E402

from threads_tui import slots  # noqa: E402
from threads_tui.login import MAX_LABEL  # noqa: E402


# ────────────────────────────────────────────────────────────────────────────
# 1. RENAME — the validator, mirrored from accounts::rename_refusal
# ────────────────────────────────────────────────────────────────────────────

EXISTING = ["work", "home", "spare"]


def test_the_machine_login_cannot_be_renamed_and_the_message_names_the_key():
    """**`default` is not a slot, in either direction.**

    THE MUTATION. Delete the `old == DEFAULT_ACCOUNT` branch and this goes red.
    Without it the board would send a rename for the machine login, and the
    daemon's own refusal would arrive after the round trip — which is exactly
    what every local check in this module exists to avoid.
    """
    why = slots.rename_refusal("default", "work-2", EXISTING)
    assert why and "machine login" in why, why
    # The remedy, not just the rule. `s` is the key that actually changes which
    # account plain `claude` signs in as.
    assert "Press s" in why or "press s" in why, why

    onto = slots.rename_refusal("work", "default", EXISTING)
    assert onto and "reserved" in onto, onto


def test_the_name_shape_rules_are_the_shared_ones_and_not_a_second_copy():
    """**The charset/length/reserved rules come from `login.label_refusal`.**

    THE MUTATION. Re-implement the charset check inside `rename_refusal` with
    `str.isalnum()` — which accepts `wörk`, because it is Unicode-aware — and
    the non-ascii case goes red. There are already three implementations of
    this rule in the repo; the whole point of delegating is that there is not a
    fourth.
    """
    assert slots.rename_refusal("work", "work 2", EXISTING)
    assert "use letters" in slots.rename_refusal("work", "work!", EXISTING)
    assert "use letters" in slots.rename_refusal("work", "wörk", EXISTING)
    assert "use letters" in slots.rename_refusal("work", "a/b", EXISTING)
    assert "too long" in slots.rename_refusal("work", "a" * (MAX_LABEL + 1), EXISTING)
    assert slots.rename_refusal("work", "a" * MAX_LABEL, EXISTING) is None
    # `accounts/backups/` holds the live-login undo snapshots.
    assert "reserved" in slots.rename_refusal("work", "backups", EXISTING)


def test_an_empty_field_asks_for_the_new_name_rather_than_a_new_account():
    """**The shared validator, with the one sentence that had to differ.**

    THE MUTATION. Drop the `empty=` argument and this reads "type a name for
    the NEW ACCOUNT" in a rename modal — a sentence about a different
    operation. That is the reason `label_refusal` grew a parameter instead of
    being copied.
    """
    why = slots.rename_refusal("work", "   ", EXISTING)
    assert why == "type the new name", why


def test_renaming_to_its_own_name_and_onto_a_taken_one_are_both_refused_here():
    """**The two collisions, and neither becomes a round trip.**

    THE MUTATION. Remove the `old == new` branch and the first goes red; the
    daemon would then answer "that is already its name" after a request that
    moved nothing. Remove the `new in existing` branch and the second goes red,
    and that one is worse: the daemon catches it, but only after this board has
    told him the name was fine.
    """
    assert slots.rename_refusal("work", "work", EXISTING) == "that is already its name"
    taken = slots.rename_refusal("work", "home", EXISTING)
    assert taken and "already taken" in taken, taken
    # And the message must NOT be the new-account one, which names the wrong key.
    assert "press l" not in (taken or "")


def test_a_rename_of_an_account_the_board_does_not_hold_is_refused():
    """THE MUTATION. Remove the `old not in existing` branch — the board would
    then send a rename for a row that has left the roster under it."""
    why = slots.rename_refusal("ghost", "work-2", EXISTING)
    assert why and "no account" in why, why


def test_a_good_rename_is_accepted():
    assert slots.rename_refusal("work", "work-2", EXISTING) is None
    assert slots.rename_refusal("work", "  work-2  ", EXISTING) is None


# ────────────────────────────────────────────────────────────────────────────
# 2. RENAME — the verdict is read off the daemon's echo
# ────────────────────────────────────────────────────────────────────────────


def test_the_rename_verdict_says_history_keeps_the_old_name_only_when_told():
    """**The surprising fact, spoken from the payload and never from memory.**

    The daemon puts `history_keeps_old_name: true` in the answer precisely
    because it is surprising — threads and budget rows already written under
    the old label keep it. It has to reach the operator, and it has to reach
    him as a READ FACT.

    THE MUTATION. Hard-code the sentence instead of reading the flag: the
    second half of this test goes red, and the board would then promise a
    property of a daemon that never claimed it.
    """
    line, severity = slots.rename_verdict(
        {
            "old_label": "work",
            "new_label": "work-2",
            "credential_moved": True,
            "history_keeps_old_name": True,
        },
        "work",
        "work-2",
    )
    assert severity == "ok"
    assert "work is now work-2" in line
    assert "keychain item moved" in line
    assert "KEEP that name" in line and "'work'" in line

    quiet, _ = slots.rename_verdict(
        {"old_label": "work", "new_label": "work-2"}, "work", "work-2"
    )
    assert "KEEP" not in quiet, quiet


def test_a_rename_that_left_the_credential_behind_is_reported_as_such():
    """**`credential_moved: false` is the silent failure this names.**

    The keychain service name is derived from the slot's directory path, so a
    directory that moved without its secret leaves a perfectly good grant
    orphaned under a service nothing reads — the account silently becomes "not
    signed in".

    THE MUTATION. Report only the `True` case and this goes red; the board would
    then say "renamed" and nothing else while the login was gone.
    """
    line, _ = slots.rename_verdict(
        {"old_label": "a", "new_label": "b", "credential_moved": False}, "a", "b"
    )
    assert "NO keychain item was moved" in line
    assert "press l" in line


def test_a_rename_answer_the_board_cannot_read_is_not_reported_as_success():
    line, severity = slots.rename_verdict(None, "work", "work-2")
    assert severity == "warn"
    assert "not confirmed" in line


# ────────────────────────────────────────────────────────────────────────────
# 3. DELETE — the one local refusal, the typed confirmation, the verdict
# ────────────────────────────────────────────────────────────────────────────


def test_the_machine_default_row_is_the_only_delete_the_board_refuses_itself():
    """**One local refusal, and every other one is the daemon's.**

    The machine-default row is not a slot — there is no request that could
    delete it — so asking is asking a question we already have the answer to.
    Everything else (a missing slot, the live login) is refused BY THE DAEMON,
    deliberately, because its sentences name their remedy.

    THE MUTATION. Add a local `is_live_login` refusal and the third assertion
    goes red — and the board would be paraphrasing, in its own words, the one
    refusal whose exact wording carries the command to run first.
    """
    why = slots.delete_refusal("default", True)
    assert why and "machine login" in why and "Press s" in why, why
    assert slots.delete_refusal("default", False) is not None
    assert slots.delete_refusal("work", False) is None


def test_the_delete_confirmation_must_be_the_name_typed_exactly():
    """**The arming rule. A reflex must not be able to satisfy it.**

    THE MUTATION. Make the comparison case-insensitive, or accept a prefix, and
    the corresponding case goes red. Either one would let the confirmation be
    satisfied by something that is not a second, deliberate statement of WHICH
    account — which is the whole reason it is asked for.
    """
    assert slots.confirmation_matches("work", "work")
    assert slots.confirmation_matches("work", "  work  ")  # paste artefact
    assert not slots.confirmation_matches("work", "Work")
    assert not slots.confirmation_matches("work", "wor")
    assert not slots.confirmation_matches("work", "work-2")
    assert not slots.confirmation_matches("work", "")
    assert not slots.confirmation_matches("work", "   ")
    # An empty label can never be confirmed by an empty box.
    assert not slots.confirmation_matches("", "")


def test_the_delete_verdict_reports_each_removal_from_its_own_boolean():
    """**Three independent removals, three booleans, no inference.**

    THE MUTATION. Report "deleted" off the call having succeeded rather than off
    the flags: the partial case goes red, and an operator told "deleted" would
    never look for the keychain item that stayed.
    """
    line, severity = slots.delete_verdict(
        {
            "label": "work",
            "dir_removed": True,
            "keychain_removed": True,
            "registry_removed": True,
            "revoked": False,
        },
        "work",
    )
    assert severity == "ok"
    assert "its directory" in line and "its keychain item" in line

    partial, severity = slots.delete_verdict(
        {
            "label": "work",
            "dir_removed": True,
            "keychain_removed": False,
            "registry_removed": True,
            "revoked": False,
        },
        "work",
    )
    assert severity == "warn"
    assert "did NOT remove its keychain item" in partial


def test_the_delete_verdict_always_says_nothing_was_revoked():
    """**`revoked: false` is not a caveat to tidy away.**

    Nothing is revoked server-side: the login stays valid wherever else it was
    copied, and an operator who reads "deleted" as "signed out" has a wrong
    model of what he just did.

    THE MUTATION. Drop the `revoked` branch and this goes red.
    """
    line, _ = slots.delete_verdict(
        {"label": "work", "dir_removed": True, "revoked": False}, "work"
    )
    assert "Nothing was revoked" in line and "LOCAL" in line


def test_a_delete_answer_the_board_cannot_read_is_not_reported_as_success():
    line, severity = slots.delete_verdict("nope", "work")
    assert severity == "warn" and "not confirmed" in line


# ────────────────────────────────────────────────────────────────────────────
# 4. ROTATE — the verdict comes from --json, never from a log
# ────────────────────────────────────────────────────────────────────────────


def _pass(slots_rows, *, ok=True, dry=False, live="untouched (secret, mdat and identity all unchanged)"):
    return {
        "run_id": "r1",
        "ok": ok,
        "dry_run": dry,
        "result": {"at": "2026-09-20T00:00Z", "live_login": live, "slots": slots_rows},
    }


def test_a_rotation_names_the_slots_that_need_a_sign_in_rather_than_counting_them():
    """**A number says something is wrong; a label says which key to press.**

    `manual` is the outcome the pass cannot fix — the slot needs a browser — and
    it is the only reason a human is reading this line at all.

    THE MUTATION. Replace the naming line with `f"{len(needs)} need a sign-in"`
    and this goes red: the verdict would read "1 need a sign-in" and he would
    have to go and find out which one. (Merely ADDING `manual` to the counted
    buckets does NOT turn this red — both lines would then appear — which is
    why the mutation named here is the removal and not the addition.)
    """
    line, severity = slots.rotate_verdict(
        _pass(
            [
                {"label": "work", "outcome": "rotated", "days_left": 30.0, "note": "n"},
                {"label": "home", "outcome": "manual", "days_left": 2.0, "note": "n"},
            ]
        )
    )
    assert severity == "warn"
    assert "NEEDS A SIGN-IN: home" in line
    assert "press l" in line
    assert "1 rolled forward" in line


def test_a_failed_slot_is_named_too_and_the_pass_failing_is_a_warning():
    line, severity = slots.rotate_verdict(
        _pass(
            [{"label": "work", "outcome": "error", "days_left": None, "note": "n"}],
            ok=False,
        )
    )
    assert severity == "warn"
    assert "FAILED: work" in line
    assert "the pass itself reported failure" in line


def test_a_rehearsal_says_it_changed_nothing():
    """**A dry run must be distinguishable from a real pass, in words.**

    The daemon goes out of its way to make the two indistinguishable in EFFECT
    — a dry run deliberately does not ring `account_picture_changed` — so the
    only thing that can tell them apart afterwards is what the board says.

    THE MUTATION. Ignore `dry_run` and this goes red.
    """
    line, severity = slots.rotate_verdict(
        _pass(
            [{"label": "work", "outcome": "would-refresh", "days_left": 9.0, "note": "n"}],
            dry=True,
        )
    )
    assert severity == "ok"
    assert "REHEARSAL" in line and "nothing was changed" in line
    assert "1 would refresh" in line


def test_an_outcome_this_client_has_never_heard_of_is_still_counted():
    """**A client that drops an unrecognised outcome reports a smaller pass.**

    The script owns this vocabulary and may grow it. THE MUTATION: filter the
    buckets to the known words and this goes red — the line would say "1 slot(s)
    reported" for a pass that did something with a name.
    """
    line, _ = slots.rotate_verdict(
        _pass([{"label": "work", "outcome": "quarantined", "days_left": None, "note": ""}])
    )
    assert "1 quarantined" in line


def test_the_identity_changed_alarm_comes_first_and_is_a_warning():
    """**The one outcome the script says must never happen.**

    THE MUTATION. Append the live-login note like any other and this goes red —
    the sentence would arrive at the end of a line that starts by saying the
    rotation went fine.
    """
    line, severity = slots.rotate_verdict(
        _pass(
            [{"label": "work", "outcome": "rotated", "days_left": 30.0, "note": "n"}],
            live="IDENTITY CHANGED — this must never happen; do not re-run",
        )
    )
    assert severity == "warn"
    assert line.startswith("live login: IDENTITY CHANGED")


def test_the_boring_live_login_note_is_not_repeated_and_a_moved_one_is():
    quiet, _ = slots.rotate_verdict(
        _pass([{"label": "w", "outcome": "rotated", "days_left": 1.0, "note": ""}])
    )
    assert "live login" not in quiet, quiet
    moved, _ = slots.rotate_verdict(
        _pass(
            [{"label": "w", "outcome": "rotated", "days_left": 1.0, "note": ""}],
            live="secret/mdat moved, identity unchanged (one of your own sessions refreshed — normal)",
        )
    )
    assert "live login: secret/mdat moved" in moved


def test_a_pass_with_no_parseable_result_is_a_failure_and_not_nothing_to_do():
    """**The daemon's own rule, mirrored.** `login.rs`: a pass that printed
    nothing parseable did not rotate anything, and reporting `{}` as a result
    would read as "nothing needed doing"."""
    line, severity = slots.rotate_verdict({"run_id": "r", "ok": True})
    assert severity == "warn"
    assert "no result blob" in line

    line, severity = slots.rotate_verdict(None)
    assert severity == "warn" and "not known" in line


# ────────────────────────────────────────────────────────────────────────────
# 5. THE MODALS, DRIVEN FOR REAL — cancel leaves nothing behind
# ────────────────────────────────────────────────────────────────────────────


async def _host():
    from textual.app import App, ComposeResult
    from textual.widgets import Static

    class Host(App):
        def compose(self) -> ComposeResult:
            yield Static("host")

    return Host()


async def _rename_modal_cancel() -> tuple[object, str]:
    from textual.widgets import Input, Static

    app = await _host()
    async with app.run_test() as pilot:
        modal = slots.RenameModal("work", ["work", "home"])
        result: list = []
        app.push_screen(modal, callback=result.append)
        await pilot.pause()
        # A refusal must NOT dismiss: it has to be readable in the modal that
        # produced it, beside the field it is about.
        modal.query_one("#name", Input).value = "home"
        modal._submit()
        await pilot.pause()
        why = str(modal.query_one("#why", Static).content)
        await pilot.press("escape")
        await pilot.pause()
        return (result[0] if result else "NO CALLBACK"), why


async def _delete_modal_arming() -> tuple[bool, bool, object]:
    from textual.widgets import Button, Input

    app = await _host()
    async with app.run_test() as pilot:
        modal = slots.DeleteConfirm("work", is_live_login=True)
        result: list = []
        app.push_screen(modal, callback=result.append)
        await pilot.pause()
        armed_at_open = not modal.query_one("#go", Button).disabled
        modal.query_one("#confirm", Input).value = "work"
        await pilot.pause()
        armed_after_typing = not modal.query_one("#go", Button).disabled
        await pilot.press("escape")
        await pilot.pause()
        return armed_at_open, armed_after_typing, (result[0] if result else "NO CALLBACK")


async def _rotate_modal_cancel() -> object:
    app = await _host()
    async with app.run_test() as pilot:
        modal = slots.RotateConfirm(4)
        result: list = []
        app.push_screen(modal, callback=result.append)
        await pilot.pause()
        await pilot.press("escape")
        await pilot.pause()
        return result[0] if result else "NO CALLBACK"


def test_the_rename_modal_refuses_in_place_and_escape_returns_nothing():
    """**A refusal is shown IN the modal; escape returns None.**

    THE MUTATION. Dismiss on a refusal and the first assertion goes red — he
    would lose the field he was editing and the sentence explaining why.
    """
    got, why = asyncio.run(_rename_modal_cancel())
    assert "already taken" in why, why
    assert got is None, f"escape returned {got!r}, expected None"


def test_the_delete_button_does_not_arm_until_the_name_is_typed():
    """THE MUTATION. Ship the button with `disabled=False` and the first
    assertion goes red — a press-and-Enter reflex would then delete a slot."""
    at_open, after_typing, got = asyncio.run(_delete_modal_arming())
    assert at_open is False, "the delete button was armed the moment the modal opened"
    assert after_typing is True, "typing the exact name did not arm the button"
    assert got is False, f"escape returned {got!r}, expected False"


def test_the_rotate_modal_escapes_to_nothing_and_never_names_an_account():
    """**Rotate is pool-wide and the copy must not imply otherwise.**

    THE MUTATION. Put the selected label in the title and the second assertion
    goes red. `accounts/rotate` takes no account; a modal that named one would
    be claiming a scope the wire does not have.
    """
    got = asyncio.run(_rotate_modal_cancel())
    assert got is None, f"escape returned {got!r}, expected None"
    body = " ".join(text for text, _ in slots.RotateConfirm(4).body_lines())
    assert "every slot" in slots.RotateConfirm(4).title_line()
    assert "no per-account rotation" in body
    assert "all 4 accounts" in body


def test_the_delete_modal_says_local_and_names_the_reversible_door():
    """He asked for `d` precisely because he did not want deletion. The
    irreversible modal owes him the reminder that the other one exists.

    THE MUTATION. Drop the disable line and this goes red."""
    body = " ".join(text for text, _ in slots.DeleteConfirm("work").body_lines())
    assert "LOCAL" in body and "Nothing is revoked" in body
    assert "press escape and then d" in body
    assert "Type work below" in body
    # Already disabled: the alternative has already been taken, so offering it
    # again would be noise on the one screen that must be read.
    off = " ".join(text for text, _ in slots.DeleteConfirm("work", disabled=True).body_lines())
    assert "press escape and then d" not in off


def test_the_rename_modal_warns_about_history_before_the_operation():
    body = " ".join(text for text, _ in slots.RenameModal("work").body_lines())
    assert "KEEP the old name" in body


# ────────────────────────────────────────────────────────────────────────────
# 6. THE SOCKET — the keys open modals, cancelling sends NOTHING,
#    and a refusal reaches the screen verbatim
# ────────────────────────────────────────────────────────────────────────────


class SlotDaemon:
    """A daemon that records what it was asked and can be told to refuse.

    Deliberately ANSWERS `accounts/{account}/rename`, `accounts/{account}/delete` and
    `accounts/rotate`. A fake that refused them would prove only that a missing
    method cannot be called; the claim under test is that a CANCELLED modal does
    not ask — and that is provable only against a daemon that would have
    answered. Same argument as `fake_daemon.queries_asked`.
    """

    MUTATIONS = ("accounts/{account}/rename", "accounts/{account}/delete", "accounts/rotate")

    def __init__(self, keys=("work", "home"), *, refuse: str | None = None) -> None:
        self.shape = fake_daemon.ShapeSurface(list(keys))
        self.methods: list[str] = []
        self.params: list[dict] = []
        self.refuse = refuse
        self._server = None
        self._conns: set = set()

    def asked(self) -> list[str]:
        """Which ACCOUNT-MUTATING methods this daemon was asked for.

        Routed, because two of the three carry their account in the PATH, so
        there is no single string a raw method could be compared against.
        """
        return fake_daemon.asked(self.methods, *self.MUTATIONS)

    def _payload(self, method: str, params: dict):
        if self.refuse is not None and method in self.MUTATIONS:
            raise ValueError(self.refuse)
        if method == "accounts/{account}/rename":
            return {
                "old_label": params["account"],
                "new_label": params["new_name"],
                "credential_moved": True,
                "history_keeps_old_name": True,
            }
        if method == "accounts/{account}/delete":
            return {
                "label": params["account"],
                "dir_removed": True,
                "keychain_removed": True,
                "registry_removed": True,
                "revoked": False,
            }
        if method == "accounts/rotate":
            return {
                "run_id": "run-1",
                "ok": True,
                "dry_run": bool(params.get("dry_run")),
                "result": {"at": "now", "live_login": "untouched (x)", "slots": []},
            }
        raise KeyError(method)

    def _process_request(self, connection, request):
        if request.path == "/health":
            from websockets.datastructures import Headers
            from websockets.http11 import Response

            body = json.dumps(
                {"status": "ok", "daemon": "threads", "boot_id": "boot-slots", "proto": 1}
            ).encode()
            return Response(
                200,
                "OK",
                Headers(
                    {
                        "Content-Type": "application/json",
                        "Content-Length": str(len(body)),
                        "Connection": "close",
                    }
                ),
                b"" + body,
            )
        return None

    async def _handler(self, ws) -> None:
        self._conns.add(ws)
        try:
            await ws.send(
                json.dumps(
                    {
                        "type": "welcome",
                        "proto": 1,
                        "daemon": "threads",
                        "daemon_version": "test",
                        "boot_id": "boot-slots",
                        "auth_required": False,
                    }
                )
            )
            async for raw in ws:
                frame = json.loads(raw)
                # No firehose is served: the board sends no `subscribe` (R2.S7).
                if frame.get("type") != "request":
                    continue
                method = frame.get("method")
                params = frame.get("params") or {}
                self.methods.append(method)
                self.params.append(dict(params))
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
                await ws.send(
                    json.dumps(
                        {
                            "type": "response",
                            "correlate": frame.get("id"),
                            "outcome": outcome,
                        }
                    )
                )
                if template == fake_daemon.SUBSCRIBE and outcome["type"] == "success":
                    await self.shape.export_all(ws, bound)
        except Exception:
            pass
        finally:
            self._conns.discard(ws)

    async def start(self, port: int):
        from websockets.asyncio.server import serve

        self._server = await serve(
            self._handler, "127.0.0.1", port, process_request=self._process_request
        )
        return self._server

    async def stop(self) -> None:
        if self._server is not None:
            self._server.close()
            try:
                await self._server.wait_closed()
            except Exception:
                pass


async def _open_and_escape(key: str, modal_type) -> tuple[bool, list[str], str]:
    """Press `key`, assert the modal is up, press escape, read the wire."""
    daemon = SlotDaemon()
    app = await fake_daemon.boot(daemon)
    try:
        async with app.run_test(size=(140, 60)) as pilot:
            await fake_daemon.settle(pilot, 3.0)
            await pilot.press(key)
            opened = False
            for _ in range(60):
                if isinstance(app.screen, modal_type):
                    opened = True
                    break
                await fake_daemon.settle(pilot, 0.1)
            await pilot.press("escape")
            await fake_daemon.settle(pilot, 0.8)
            still_up = isinstance(app.screen, modal_type)
            return opened and not still_up, daemon.asked(), fake_daemon.body(app)
    finally:
        await daemon.stop()


def test_m_opens_the_rename_modal_and_escape_sends_nothing():
    """**THE CANCEL PATH, READ OFF THE WIRE.**

    THE MUTATION. Move the `if new is None: return` guard below the
    `accounts_rename` call and `asked` becomes `['accounts/{account}/rename']` — the
    board would rename on a cancelled modal.
    """
    ok, asked, _ = asyncio.run(_open_and_escape("m", slots.RenameModal))
    assert ok, "m did not open the rename modal, or escape did not dismiss it"
    assert asked == [], f"a cancelled rename still asked the daemon: {asked}"


def test_X_opens_the_delete_modal_and_escape_sends_nothing():
    ok, asked, _ = asyncio.run(_open_and_escape("X", slots.DeleteConfirm))
    assert ok, "X did not open the delete modal, or escape did not dismiss it"
    assert asked == [], f"a cancelled delete still asked the daemon: {asked}"


def test_O_opens_the_rotate_modal_and_escape_sends_nothing():
    ok, asked, _ = asyncio.run(_open_and_escape("O", slots.RotateConfirm))
    assert ok, "O did not open the rotate modal, or escape did not dismiss it"
    assert asked == [], f"a cancelled rotation still asked the daemon: {asked}"


async def _unshifted(key: str) -> tuple[str, list[str], object]:
    daemon = SlotDaemon()
    app = await fake_daemon.boot(daemon)
    try:
        async with app.run_test(size=(140, 60)) as pilot:
            await fake_daemon.settle(pilot, 3.0)
            before = type(app.screen)
            await pilot.press(key)
            await fake_daemon.settle(pilot, 0.6)
            return app.status_note, daemon.asked(), type(app.screen) is before
    finally:
        await daemon.stop()


def test_the_unshifted_guard_keys_explain_themselves_instead_of_being_silent():
    """**A guarded key must not be indistinguishable from an unbound one.**

    The worst failure this interface has ever had was a key that looked bound,
    did nothing, and said nothing. Putting `X` and `O` behind shift would have
    manufactured two more of exactly that, so `x` and `o` say where the act is.

    THE MUTATION. Drop the two `*_hint` bindings and both go red: the guard
    would be silent, which is the failure it was written to avoid.
    """
    note, asked, same_screen = asyncio.run(_unshifted("x"))
    assert "X (shift+x)" in note, note
    assert "Press d" in note, "the hint must name the reversible alternative"
    assert asked == [] and same_screen

    note, asked, same_screen = asyncio.run(_unshifted("o"))
    assert "O (shift+o)" in note, note
    assert asked == [] and same_screen


LIVE_LOGIN_REFUSAL = (
    "\"work\" is the account your machine login is currently signed in as. "
    "Deleting its slot would leave that account with only the machine store — "
    "and no way for the daemon to spawn as it. Point the machine login at "
    "another account first (`claude-accounts set-live-login <other>`), or pass "
    "--force if you really mean to keep only the machine copy."
)


async def _refusal_reaches_the_screen() -> tuple[str, list[str], list[dict], list[str]]:
    """Drive a real delete against a daemon that refuses it, and read the note.

    The modal is ARMED here rather than cancelled — this is the one case where
    the request must actually leave, because the claim is about what comes back.
    The daemon is a fake and deletes nothing.
    """
    from textual.widgets import Input

    daemon = SlotDaemon(refuse=LIVE_LOGIN_REFUSAL)
    app = await fake_daemon.boot(daemon)
    try:
        async with app.run_test(size=(140, 60)) as pilot:
            await fake_daemon.settle(pilot, 3.0)
            await pilot.press("X")
            for _ in range(60):
                if isinstance(app.screen, slots.DeleteConfirm):
                    break
                await fake_daemon.settle(pilot, 0.1)
            modal = app.screen
            label = modal.account
            modal.query_one("#confirm", Input).value = label
            await pilot.pause()
            modal._submit()
            for _ in range(80):
                if "refused" in app.status_note:
                    break
                await fake_daemon.settle(pilot, 0.1)
            return app.status_note, daemon.asked(), daemon.params, daemon.methods
    finally:
        await daemon.stop()


def test_the_daemons_refusal_reaches_the_screen_verbatim_and_no_force_is_sent():
    """**THE REFUSAL, WORD FOR WORD — including the command it names.**

    `delete_refusal`'s live-login sentence ends with the remedy: point the
    machine login somewhere else first. That is the only part of it worth
    reading, and it is the part a summary deletes.

    AND THE SECOND HALF OF THIS TEST IS THE STRONGER ONE. The refusal mentions
    `--force`, and this board must have no way to send it. THE MUTATION: add
    `force` to `Api.accounts_delete` and pass it here, and the params assertion
    goes red.

    **THE ACCOUNT IS NOW READ OFF THE PATH AND THE PARAMS MUST BE EMPTY**, and
    that pair of assertions is stronger than the one it replaces. This read
    `{"account": "work"} in params` while the subject was a parameter. It is
    the path segment now, and sending it in BOTH places is refused by the
    daemon — so proving the params are empty proves the call is fully migrated,
    where before an un-migrated extra `account` would have been invisible.
    """
    note, asked, params, methods = asyncio.run(_refusal_reaches_the_screen())
    assert asked == ["accounts/{account}/delete"], asked
    assert LIVE_LOGIN_REFUSAL in note, note
    # Not a summary of it. The whole sentence, including the command.
    assert "claude-accounts set-live-login <other>" in note
    assert note.startswith("refused: ")
    assert all("force" not in p for p in params), (
        f"this board must have no way to send force: {params}"
    )
    # The subject, on the path, spelled out — not a template.
    assert "accounts/work/delete" in methods, methods
    # And NOTHING in the params. Not the account (it would be refused), not
    # `force` (it does not exist here).
    delete_params = [
        p
        for m, p in zip(methods, params)
        if fake_daemon.route(m)[0] == "accounts/{account}/delete"
    ]
    assert delete_params == [{}], delete_params


def test_rotate_sends_no_account_because_the_wire_takes_none():
    """**A parameter the wire ignores is worse than a missing one.**

    `server.rs` reads `dry_run` off `accounts/rotate` and nothing else. The old
    `Api.accounts_rotate(account=...)` let a caller believe it had scoped a
    pool-wide operation.

    THE MUTATION. Put `account` back in the params and this goes red.
    """
    import inspect

    from threads_tui.api import Api

    sig = inspect.signature(Api.accounts_rotate)
    assert "account" not in sig.parameters, sig
    assert "dry_run" in sig.parameters, sig


# ────────────────────────────────────────────────────────────────────────────


def _main() -> int:
    import traceback

    tests = [
        (name, fn)
        for name, fn in sorted(globals().items())
        if name.startswith("test_") and callable(fn)
    ]
    failures: list[str] = []
    for name, fn in tests:
        try:
            fn()
        except Exception:
            print(f"  FAIL {name}")
            traceback.print_exc()
            failures.append(name)
        else:
            print(f"  ok   {name}")
    print()
    if failures:
        print(f"test_slots: {len(failures)} FAILED of {len(tests)}: {', '.join(failures)}")
        return 1
    print(f"test_slots: all good ({len(tests)} checks)")
    return 0


if __name__ == "__main__":
    sys.exit(_main())
