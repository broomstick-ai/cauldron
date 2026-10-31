# Changelog

Every release of cauldron is listed here, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). The plan for upcoming releases is in
[ROADMAP.md](ROADMAP.md).

## [Unreleased] — V1

Planned for the release after 0.1.1:

- **Two tab bars.** A global bar on top (Config: the UI, devices, providers and
  accounts) and an environment bar below it (Cauldron, Threads, Status,
  Permissions).
- **First run.** A "create your first environment" screen, then a "create your
  first cluster" screen for a new, empty environment.
- **Status → Ledger.** The Ledger becomes the first Status sub-tab, shown as a
  time-ordered list of dumps. Each dump expands into its phases and the chain
  of work it set off.
- **Status → Tokens.** The real account pool and its usage pressure.
- **Config → Accounts.** Accounts are global: you log in once, then switch each
  one on or off per cauldron, with a configurable default.
- **Inference.** Claude Code is the only provider. Every other endpoint is
  shown greyed out.
- **Secret human actions.** A masked input, plus a routing adapter that sends
  the value to a graph node, the macOS keychain or an env file. A secret action
  with no destination is rejected when it is written.
- **Ripples live.** Dumps set off ripples, and ripples write to the soup. The
  soup engine is hardened, and the Ledger shows real ripple events. Neither is
  in 0.1.0 or 0.1.1.
- **Cleanup** of leftover names and states in the UI.
- **Fix: threads daemon boot with an oversized write-ahead log.** The daemon
  will fold the log once at boot, before recovery, and its periodic fold will
  first run at 30 seconds instead of 5 minutes. In 0.1.0 and 0.1.1, a large log
  left from earlier runs can stall startup until the supervisor kills the daemon.
- **Fix: re-login re-enables a disabled account.** A verified login will lift
  the disable and trigger a fresh usage reading. In 0.1.0 and 0.1.1, only an
  explicit enable clears it.

## [0.1.1] — 2026-10-31

### Added

- **The Threads tab.** The environment's tab bar now reads Cauldron, Threads,
  Status, Config; Threads replaces the Agents tab, which drew sample data.
  It shows the environment's threads pinned or recent, plus closed threads
  under Archived, read live from that environment's threads daemon and built
  on the threads face's client (the same connection and store as
  `cauldron tui --chat`). Open a thread to read its transcript, send it a
  message, open a tool call to see its full arguments and result, and see a
  Task subagent's own transcript. When the daemon is down, the last rows stay
  drawn and sending is refused with "can't send — threads is down". The tab
  connects to the daemon the first time it is shown. Switching cauldron keeps
  the Threads tab connected to the new environment's own threads endpoint.
  Starting a new thread from this tab is not in this release (the threads
  face has it).

### Changed

- **Isolation hardening for tests.** A test, or any face or CLI run with
  `CAULDRON_ISOLATED=1`, can no longer reach a real installation. Every dial
  and every daemon-control spawn goes through one egress check
  (`libs/env/src/egress.rs`): a dial is admitted only to a loopback listener
  that the isolated process tree itself bound, paid or third-party APIs are
  refused, starting or stopping a daemon is refused unless it targets a
  scratch environment, and a root inside the real `~/.cauldron` or the login
  home is refused. A non-interactive face needs an explicit endpoint, and
  `cauldron-launcher` forwards only from a scratch root to a scratch binary.
  The test binaries set the marker themselves (`testkit/test-isolation`).
  Not isolated, nothing changes.

## [0.1.0] — 2026-10-31 — MVP (Halloween)

The first public release.

### Added

- **The new terminal UI.** It shows environments, the cluster sidebar with
  clusters seeded from a boundary you write, cluster tabs (Questions, Actions,
  Tasks, Nodes, Status, Details), the bucket and dumps, and the Ledger.
  `cauldron tui --chat` opens the threads face.
- **The release branch:** the main line merged with the new UI.
- **`cauldron-build` demo environment.** Curated data from cauldron's own
  development, scrubbed of names, paths and secrets, shipped in the soup crate.
  It shows clusters and the aggregate cluster, directives, constraints,
  objectives and milestones, questions and multi-phase actions (including a
  secret action), dumps, the Ledger, and opening a file.
- **A fresh environment starts empty;** the sample data lives in the
  `cauldron-build` demo environment.
- **Install path.** `scripts/install` builds and installs cauldron for the
  current user under their own home directory. It refuses anything but macOS,
  uses ad-hoc code signing by default (no personal signing identity), uses
  neutral launchd labels, and offers `--dry-run`. Both `scripts/install` and
  `cauldron install` refuse to restart a cauldron job that is already running
  unless you pass `--replace-running`.
- **License and docs.** Apache-2.0 `LICENSE` and `NOTICE`, the README with its
  image, this changelog and `ROADMAP.md`.

### Removed

- **Internal development notes and personal data** are kept out of the public
  tree: step reports, runbooks, design notes and crate-provenance notes are not
  shipped, nor are the proof scripts that run against one machine's live
  services. Shipped files name no home directory of a real user and no private
  network address; the only person named is the developer, in `NOTICE` and the
  README credit. Source comments still carry provenance tags (step ids,
  `msg N`, `INTENT N`), short quoted product direction from development
  sessions, and the names of the private repositories and workspace paths
  cauldron was built alongside; the live-store guard in `libs/env` names, by
  design, the directories it refuses to write. Two checks fail if a listed
  path or a personal value comes back: `scripts/check-public-tree.sh` (which
  paths ship) and `scripts/check-no-personal-data.sh` (what the shipped files
  say).

[Unreleased]: https://github.com/broomstick-ai/cauldron/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/broomstick-ai/cauldron/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/broomstick-ai/cauldron/releases/tag/v0.1.0
