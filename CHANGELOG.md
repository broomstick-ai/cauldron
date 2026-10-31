# Changelog

Every release of cauldron is listed here, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). The plan for upcoming releases is in
[ROADMAP.md](ROADMAP.md).

## [Unreleased] — V1

Planned for the release after 0.1.0:

- **Coming next: the Threads tab.** Threads pinned or recent per environment,
  built on the threads face's client.
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
  in 0.1.0.
- **Cleanup** of leftover names and states in the UI.
- **Fix: threads daemon boot with an oversized write-ahead log.** The daemon
  will fold the log once at boot, before recovery, and its periodic fold will
  first run at 30 seconds instead of 5 minutes. In 0.1.0, a large log left from
  earlier runs can stall startup until the supervisor kills the daemon.
- **Fix: re-login re-enables a disabled account.** A verified login will lift
  the disable and trigger a fresh usage reading. In 0.1.0, only an explicit
  enable clears it.

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

[Unreleased]: https://github.com/broomstick-ai/cauldron/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/broomstick-ai/cauldron/releases/tag/v0.1.0
