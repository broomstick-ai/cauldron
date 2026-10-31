"""Where the daemon is, and the two local paths this app is allowed to know.

This is a deliberately SMALLER module than the old app's `paths.py`. That one
resolved the database, the accounts directory, the rotation log and the bash
tool because the app read all of them. This app reads NONE of them: every fact
comes from the API (see `threads_tui.drag`, obligation 2).

What survives here is:

  * the daemon endpoint — mirrors `threads::config` resolution exactly;
  * `bash_tool` — used by ONE caller, `threads_tui.local`, for the single
    carve-out (setting this machine's local account);
  * `threads_bin` — used by ONE caller, the daemon-down page, to start a daemon
    that is not answering. You cannot ask a dead daemon to revive itself, so
    that operation cannot go through the API by construction.

Nothing else in this package may import a path from here to read a fact. If you
find yourself adding `db_path` back, you are breaking obligation 2.
"""

from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path

DEFAULT_HOST = "127.0.0.1"
#: **THE CHASSIS DOOR, `:3653`** — `threads::config::DEFAULT_CHASSIS_PORT`.
#:
#: It was `3651`, the LEGACY door, and the board read its narration (`info`,
#: `progress`, `lifecycle`) off that door's firehose. Those three frames carry
#: an EMPTY thread id, so they are firehose-only by construction there, and a
#: board reading them that way is what kept the firehose alive
#: (`docs/consumer-seam.md` §6, M6). The `narration` shape carries all three,
#: so the board now reads them as a shape and has no reason to be on `:3651`.
#: The lever is unchanged: `chassis_port` in the threads config, then
#: `THREADS_CHASSIS_PORT`, then `THREADS_TUI_WS_PORT` — see `daemon_endpoint`.
DEFAULT_PORT = 3653


def _scripts_dir() -> Path:
    env = os.environ.get("THREADS_TUI_SCRIPTS_DIR", "")
    if env:
        return Path(env).resolve()
    # threads_tui/endpoint.py -> threads_tui -> threads-app -> scripts
    return Path(__file__).resolve().parents[2]


@dataclass(frozen=True)
class Paths:
    scripts_dir: Path
    threads_home: Path
    repo_root: Path

    @property
    def bash_tool(self) -> Path:
        """The legacy bash tool. ONE legal caller: threads_tui.local."""
        return self.scripts_dir / "claude-accounts"

    @property
    def threads_bin(self) -> Path | None:
        """The daemon binary. ONE legal caller: the daemon-down page.

        Release preferred over debug, matching the bash tool, so a release
        build appearing later just wins.
        """
        for candidate in (
            self.repo_root / "target" / "release" / "threads",
            self.repo_root / "target" / "debug" / "threads",
        ):
            if candidate.is_file() and os.access(candidate, os.X_OK):
                return candidate
        return None

    def child_env(self) -> dict[str, str]:
        env = dict(os.environ)
        env["THREADS_HOME"] = str(self.threads_home)
        return env


def resolve() -> Paths:
    scripts = _scripts_dir()
    threads_home = scripts.parent
    return Paths(
        scripts_dir=scripts,
        threads_home=threads_home,
        repo_root=threads_home.parent.parent,
    )


PATHS = resolve()


def daemon_endpoint() -> tuple[str, int, str | None]:
    """Mirrors `threads::config`: THREADS_CONFIG, else $THREADS_HOME/config.toml,
    else built-in defaults. THREADS_SECRET beats the file value.

    The port is the CHASSIS port — `chassis_port` in the file, then
    `THREADS_CHASSIS_PORT`, exactly as `Config::effective_chassis_port` resolves
    it. The file's `port` key is the LEGACY door and is deliberately NOT read:
    a config that moved the legacy port would otherwise move this board back
    onto it. `chassis_port = 0` (the chassis disabled) falls back to the
    default rather than to port 0, which would be no endpoint at all."""
    host, port = DEFAULT_HOST, DEFAULT_PORT
    secret: str | None = None

    cfg_path = os.environ.get("THREADS_CONFIG") or str(PATHS.threads_home / "config.toml")
    try:
        import tomllib

        with open(cfg_path, "rb") as fh:
            cfg = tomllib.load(fh)
        host = str(cfg.get("host") or host)
        port = int(cfg.get("chassis_port") or port)
        val = cfg.get("secret")
        if isinstance(val, str) and val:
            secret = val
    except Exception:
        pass

    env_secret = os.environ.get("THREADS_SECRET", "")
    if env_secret:
        secret = env_secret

    chassis_env = os.environ.get("THREADS_CHASSIS_PORT", "")
    if chassis_env:
        try:
            port = int(chassis_env) or port
        except ValueError:
            pass

    # The test seam for the daemon-down path: point the app at a dead port
    # without going anywhere near the operator's live daemon.
    override = os.environ.get("THREADS_TUI_WS_PORT", "")
    if override:
        try:
            port = int(override)
        except ValueError:
            pass
    return host, port, secret


def ws_url() -> str:
    host, port, _ = daemon_endpoint()
    return f"ws://{host}:{port}/ws"


def health_url() -> str:
    host, port, _ = daemon_endpoint()
    return f"http://{host}:{port}/health"
