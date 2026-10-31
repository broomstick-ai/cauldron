"""THE ONE CARVE-OUT, `threads_tui.local`: how the board reads the bash tool.

H179: pressing `s` on an account that ALREADY IS the machine login was refused.
`claude-accounts set-live-login <label> --dry-run` stops early on that account
("already signed in") and exits 0 without the dry-run marker, and `preflight`
read "exit 0, no marker" as "never checked anything". The tool now prints a
stable `already-live:` marker there, and these tests pin how the board treats
it: success, no browser trip, one real (non-dry) run so the tool records the
name — with no `--yes` and stdin closed, so a machine login that moved in the
meantime cannot start a sign-in from a child with no terminal.

NOTHING HERE RUNS THE REAL TOOL. `local.PATHS` is pointed at a temp dir holding
a fake `claude-accounts` that prints what the test tells it to and records the
argv and stdin it was given.

Run: uv run --with pytest --with textual --with websockets pytest -q tests/test_local.py
"""

from __future__ import annotations

import asyncio
import os
import stat
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from threads_tui import local  # noqa: E402
from threads_tui.endpoint import Paths  # noqa: E402

ALREADY = (
    "\n  Your machine login is already signed in as that account.\n"
    "  already-live: nothing to sign in, no browser needed.\n\n"
)
DRY = "  ════ about to change your MACHINE login ════\n  --dry-run: stopping here. Nothing was backed up, written or invoked.\n"


@pytest.fixture
def fake_tool(tmp_path, monkeypatch):
    """A fake bash tool. Write its stdout to `out`, its exit code to `rc`."""
    tool = tmp_path / "claude-accounts"
    tool.write_text(
        "#!/bin/sh\n"
        f'here="{tmp_path}"\n'
        'printf \'%s\\n\' "$*" >> "$here/argv"\n'
        'if [ -t 0 ]; then echo tty > "$here/stdin"; else cat > "$here/stdin"; echo eof >> "$here/stdin"; fi\n'
        'cat "$here/out"\n'
        'exit "$(cat "$here/rc")"\n'
    )
    tool.chmod(tool.stat().st_mode | stat.S_IEXEC)
    (tmp_path / "out").write_text("")
    (tmp_path / "rc").write_text("0")
    monkeypatch.setattr(
        local, "PATHS", Paths(scripts_dir=tmp_path, threads_home=tmp_path, repo_root=tmp_path)
    )
    monkeypatch.delenv("CLAUDE_CONFIG_DIR", raising=False)

    def set_(out: str, rc: int = 0) -> Path:
        (tmp_path / "out").write_text(out)
        (tmp_path / "rc").write_text(str(rc))
        return tmp_path

    return set_


def argv_lines(d: Path) -> list[str]:
    return (d / "argv").read_text().splitlines()


def test_preflight_treats_already_live_marker_as_success(fake_tool):
    d = fake_tool(ALREADY)
    out = asyncio.run(local.preflight("alice-example-com"))
    assert out.ok, out.message
    assert out.refusal is None
    assert out.already_live is True
    assert argv_lines(d) == ["set-live-login alice-example-com --dry-run"]


def test_preflight_dry_run_marker_is_not_already_live(fake_tool):
    fake_tool(DRY)
    out = asyncio.run(local.preflight("work-2"))
    assert out.ok and out.refusal is None
    assert out.already_live is False


def test_preflight_still_refuses_clean_exit_with_no_marker(fake_tool):
    fake_tool("something else entirely\n")
    out = asyncio.run(local.preflight("work-2"))
    assert not out.ok
    assert "never reached its dry-run stop" in out.message
    assert out.already_live is False


def test_record_already_live_runs_real_command_without_yes_and_with_no_stdin(fake_tool):
    d = fake_tool(ALREADY)
    out = asyncio.run(local.record_already_live("alice-example-com"))
    assert out.ok, out.message
    assert out.already_live is True
    (line,) = argv_lines(d)
    assert line == "set-live-login alice-example-com"  # non-dry, no --yes
    assert (d / "stdin").read_text() == "eof\n"  # /dev/null, not the terminal


def test_record_already_live_refuses_if_the_tool_did_not_say_already_live(fake_tool):
    # e.g. the machine login moved between preflight and here: the tool asks
    # "type yes", reads EOF, prints "nothing done." and exits 1. Or exits 0
    # without the marker. Either way the board must not call it success.
    fake_tool("  nothing done.\n", rc=1)
    assert not asyncio.run(local.record_already_live("work-2")).ok
    fake_tool("odd\n", rc=0)
    out = asyncio.run(local.record_already_live("work-2"))
    assert not out.ok and out.refusal


def test_do_set_live_on_already_live_skips_the_browser_login(fake_tool, monkeypatch):
    """The board's `s`: already-live means a note, no terminal login."""
    from threads_tui import app as app_mod

    fn = getattr(app_mod.BoardApp.do_set_live, "__wrapped__", None)
    if fn is None:
        pytest.skip("textual's @work does not expose __wrapped__ here")
    d = fake_tool(ALREADY)
    logins: list = []

    async def no_login(*a, **k):
        logins.append(a)

    monkeypatch.setattr(app_mod.login, "run_terminal_login", no_login)
    notes: list = []

    async def resync():
        return True

    me = SimpleNamespace(
        selected_account=lambda: SimpleNamespace(label="alice-example-com"),
        note=lambda msg, kind: notes.append((msg, kind)),
        redraw=lambda: None,
        resync=resync,
    )
    asyncio.run(fn(me))
    assert logins == []
    assert notes and notes[-1][1] == "ok", notes
    assert "already your machine login" in notes[-1][0]
    assert argv_lines(d) == [
        "set-live-login alice-example-com --dry-run",
        "set-live-login alice-example-com",
    ]
