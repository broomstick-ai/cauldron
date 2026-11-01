# Changelog

Every release of cauldron is listed here, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). The plan for upcoming releases is in
[ROADMAP.md](ROADMAP.md).

## [Unreleased]

Planned, and not released yet. Ripples and the soup engine arrive in 0.2.0.

### 0.2.0 (planned)

<!-- RIPPLES: filled when the ripple executor's RV1.S16 lands -->

- **Ripples live.** Dumps set off ripples, and ripples write to the soup. The
  soup engine is hardened, and the Ledger shows real ripple events. Neither is
  in 0.1.0, 0.1.1, 0.1.2 or 0.1.3.

### Not yet scheduled to a release

- **Fix: threads daemon boot with an oversized write-ahead log.** The daemon
  will fold the log once at boot, before recovery, and its periodic fold will
  first run at 30 seconds instead of 5 minutes. Through 0.1.3, a large log
  left from earlier runs can stall startup until the supervisor kills the daemon.
- **Fix: re-login re-enables a disabled account.** A verified login will lift
  the disable and trigger a fresh usage reading. Through 0.1.3, only an
  explicit enable clears it.

## [0.1.3] — 2026-10-31

Account polish. Ripples and the soup engine arrive in 0.2.0: nothing in this
release acts on a dump yet.

### Changed

- **Account settings fail safe when the install's key is wrong.** Account ids
  are salted with a key kept per install (0.1.2). If that key belongs to
  another install, cannot be read, is not a key, or has gone missing after one
  was made, the face no longer mints a new key or adopts another install's:
  it opens account settings read-only and says why in a notice. Changes made
  in that face are not saved. Where it is true, the notice also says that
  settings saved for an email-address account are not applied in that
  session; a named account's settings still apply.
- **Clearer refusals.** When account settings are read-only because of the
  install's key, adding or changing an account or an endpoint is refused with
  a message that names the key problem, instead of blaming `--dump`.
- **A face no longer waits forever on a service that does not answer.** The
  face's connection to an environment's services gives up after 5 seconds
  when the socket accepts but never completes its handshake, marks the link
  down with the reason, and retries.

### Fixed

- **Scheduled jobs fire at the right local time across daylight-saving
  changes.** A job's daily slot is worked out as a local date and time, not
  as "24 hours ago", so a change of clocks no longer moves it by an hour.
  When the time happens twice (clocks go back), the job runs at the first
  occurrence; when it does not exist (clocks go forward), it runs that much
  later (02:30 runs at 03:30).
- **Uptime is correct across daylight-saving changes.** A service's uptime is
  measured between two instants instead of two local clock readings, so it no
  longer reads an hour short or long after the clocks change.
- **`cauldron pin` says the right thing when a park lands at the same
  moment.** Pin reads the service's state and saves the pin as one step, so
  its message no longer says a service "is not running" when the supervisor
  revived it while the pin was being saved.

## [0.1.2] — 2026-10-31

The V1 interface. Ripples and the soup engine arrive in 0.2.0: nothing in
this release acts on a dump yet.

### Added

- **Two tab bars.** A global bar on top, Config, holds TUI, Devices, Providers
  and Accounts. Under it, the environment bar holds Cauldron, Threads, Status,
  Permissions and Settings (this environment's own settings, which Config
  called `<name> cauldron` in 0.1.x), with the environment dropdown at its
  left. The two bars are drawn in different colors, and one tab is shown at a
  time. The single Config tab of 0.1.x is gone.
- **First run.** An empty install opens on one action, "+ create your first
  cauldron", and makes nothing until you take it. A cauldron with no cluster
  of its own opens with the sidebar's `+` selected, ready to name its first
  cluster. When no cauldron is named, the face and `cauldron tui` open the
  last-used one.
- **Secret human actions.** A secret field of a human action is typed into a
  masked input, and its value is routed to the environment's env file or the
  macOS keychain, never into the soup. A non-secret field can also be routed
  to the action's graph node. A secret field with no destination, two fields
  with one name, or two fields routed to one env variable is rejected when
  the action is written, and the face runs the same check before it draws one.
- **`cauldron ctl`: the face, headless.** `ctl dump --env <name>` sends one
  dump (its text on stdin) through the TUI's own send — the same line in
  `dumps.jsonl` the send key writes; `ctl frame` prints one frame of the face
  (`ctl frame --status-tab ledger`); `ctl node <id>` reads one node from the
  environment's soup door at the address its seam table declares. JSON out,
  non-zero exit on any refusal; `dump` is the only write, and the
  `cauldron-build` demo refuses it. The face gains `--dump --send`, which
  `ctl dump` runs. A `ctl dump` sends only its own text: the draft open in
  your face is never read into it.
- **Config → Accounts.** Accounts are global and come from the threads
  daemon: the roster is the daemon's own account list, and when it is not
  there yet the page says why (not connected, connecting, threads is down,
  loading). Log in once, from the face: "+ add account", or an account's
  "log in", opens a login window where you paste the code (drawn as dots).
  Each account can be taken out of the pool or put back, and switched on or
  off for new cauldrons; each cauldron's Settings → Accounts can inherit that
  default or turn the account on or off for itself. Addresses are masked
  everywhere as `a•••@e•••`, and two addresses that mask alike get a short
  tag to tell them apart (`c•••@e••• ·d64`). Account ids are salted per
  install, and settings saved under the old ids carry over. A login code is
  never printed in a debug line, and the face's log keeps raw daemon frames
  out at every log level.

### Changed

- **Status → Ledger, first version.** The Ledger shows dumps as a time-ordered
  event flow, newest first; each dump expands into its phases and the tree of
  work under it. It reads the environment's own data: a real environment,
  `prod` included, shows its own ledger or an empty one, never sample data.
  The `cauldron-build` demo environment carries a demo ledger.
- **Status → Tokens reads the real account pool.** Claude Code's band shows
  the threads daemon's accounts, their usage meters and their placement
  pressure, over the face's one connection to the daemon, instead of sample
  data. Values not read yet show as loading, never as missing.
- **Inference: Claude Code is the only live inference source.** Codex, the
  other token sources, every endpoint and every template are shown greyed out,
  never hidden, and acting on one is refused with a notice. What you already
  wrote for an endpoint is kept.
- **Cleanup.** The ripple kind once called "splash" is now "type 3" (stored as
  `type_three`; policies that say `shell` still read). Threads no longer show
  an "absent" status: a value that is not there is left blank, and one not
  read yet shows `…`.
- **Threads tab, honest when the daemon never answered.** A Threads tab whose
  daemon was dialled but never reached now reads "threads is down · it has not
  answered yet" instead of claiming to show what it last said.
- **Isolation: every never-dial port is refused.** An isolated face (see
  0.1.1) refuses an environment that declares threads on any port the egress
  check never dials, and says so in its own words.

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

[Unreleased]: https://github.com/broomstick-ai/cauldron/compare/v0.1.3...HEAD
[0.1.3]: https://github.com/broomstick-ai/cauldron/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/broomstick-ai/cauldron/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/broomstick-ai/cauldron/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/broomstick-ai/cauldron/releases/tag/v0.1.0
