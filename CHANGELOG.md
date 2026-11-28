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
  soup engine is hardened, and the Ledger shows real ripple events. Through
  0.1.9 neither is in a release; 0.1.10 adds the first ripple, run by hand on
  one dump at a time.

### Not yet scheduled to a release

- **Fix: threads daemon boot with an oversized write-ahead log.** The daemon
  will fold the log once at boot, before recovery, and its periodic fold will
  first run at 30 seconds instead of 5 minutes. Through 0.1.19, a large log
  left from earlier runs can stall startup until the supervisor kills the daemon.
- **Fix: re-login re-enables a disabled account.** A verified login will lift
  the disable and trigger a fresh usage reading. Through 0.1.19, only an
  explicit enable clears it.

## [0.1.19] — 2026-11-28

Merging cauldrons (issue #9): `cauldron merge <source> --into <target>`
copies one cauldron's soup and dumps into another, all or nothing, with a
plan printed first, nothing carried twice, every review decision carried
only as far as the source's own history records it, a size ceiling, and
an undo that is resumable and never removes a write the merge did not make.

### Added

- **`cauldron merge <source> --into <target> --env <env> [--dry-run]
  [--allow-large]`.** Command line only; no MCP tool. It prints the plan
  first, always: the source's size with the estimated time and memory,
  counts by type, `id_collisions` and the `id_map` (every source id is
  re-minted; edges, `[[links]]`, dump cites and the ids boundaries and edge
  notes name are rewritten through it, and the map saved is read back from
  what landed), the regions with their boundaries, look-alike regions and
  clusters (same handle or boundary: both kept and flagged, with how to
  supersede one), what is already in the target (`already_here`), edges
  across the two pools, the dumps (carried in their state, tombstones as
  tombstones, blobs the target holds shared), each record's review state
  and how many decisions will and will not be carried, what is not carried
  and why, and what blocks the merge. `--dry-run` stops there, judged on a
  private copy of the target.
- **A size ceiling that counts what costs.** The source's size is counted
  from its files before anything is held: records, the ones a ripple
  wrote, edges (the main write's and the ripple part's), dumps and their
  bytes, with the estimated time and memory. A source of more than 20,000
  records (both pools), or one estimated to take longer than that many
  ordinary records would (edges and dump bytes count), is refused before
  anything is read in full, held or written (`too_large`), dry run
  included, because a merge is one soup write whose cost grows faster than
  the source and the target refuses every other writer meanwhile;
  `--allow-large` merges it after printing the estimate. A source whose
  write would pass the soup's per-statement fuel (about 3 a record and 5 an
  edge, against 1,000,000) cannot land as one statement and is refused
  whatever the flag. Measured on one machine, a release build: 6.7 s and
  620 MB at 11,000 records, 14 s and 1.2 GB at 22,000, 55 s and 3.1 GB at
  55,000; 5,000 records with 30 edges each 9.4 s and 1.1 GB.
- **Nothing twice.** A record or dump whose origin (the cauldron it was
  first written in and its id there) the target already holds, whether
  merged before by this source or carried in by another route, is not
  carried again, and what points at it points at the target's copy. Merging
  a source again carries only what it gained since; a merge with nothing
  new is refused, and so is a source with no record and no dump.
- **Review state travels with each record, as far as the source's history
  records it.** `meta.merged_from` records the merge, the source cauldron's
  name and id, the record's id there, who wrote it there and its review
  state. A ripple's record nobody decided stays unreviewed in the target,
  and lands in a write of the ripple class, so a 0.1.17 reader lists it as
  unreviewed too. An approval or rejection is carried only when the
  source's last write of that flag was a decision: the owner's command at a
  terminal (`by: tty`), or, where the owner opened `approve`, the same
  command without one or a session's `soup_approve` / `soup_reject` (`by:
  agent`), carried alike; or a merge that carried one. Who decided, how and
  when go into `meta.merged_from.decision`, with `carried_by: {merge, by}`,
  marked `carried_by_merge`; any other flag is not carried and the record
  arrives without it, saying why.
- **Carrying decisions is the owner act `merge`**, a fourth act in
  `owner_acts` (default `terminal`): at a terminal, or without one where
  the owner opened it (`cauldron owner-acts open --only merge`, or
  `CAULDRON_OWNER_ACTS=merge` for the command). A choice recorded as `open`
  before this release lists the three acts it opened, so `merge` stays at a
  terminal until it is opened. The merge record says who ran it (`by: tty |
  agent`) and on what authority. Undoing the owner's merge (one run at a
  terminal, or one that carried decisions) is the same act.
- **A complete snapshot, and `cauldron merge --undo <MG-n> --into
  <target>`.** Before anything lands the merge snapshots everything under
  the target's directory that a merge can change (both pools, the dumps
  store with every blob, the region lock table, the merges), each file
  checked by size and SHA-256 into a manifest, beside the cauldron's
  directory under a dated name, private (on APFS each file is a clone).
  `--undo` puts it back exactly only when the target holds nothing but what
  it held when the merge began and the merge's own writes, compared by
  content with exactly the merge's writes left out; otherwise it refuses,
  changing nothing, lists what else changed by count and id, and says how
  to see those writes. It is resumable: a marker first, both stores held,
  every file recorded, then put back store by store; cut short, the same
  undo, `soup recover` or the next writer of this release (a merge
  included) finishes it, as long as its snapshot is there and whole. What
  it replaces goes beside the cauldron, outside the snapshot. A renamed
  cauldron still undoes, keeping its new name.
- **`cauldron soup recover --abandon-undo`**: the way out of an undo that
  cannot be finished (its snapshot moved, deleted or changed, its marker
  unreadable), which every writer, recover, the undo and a merge name. It
  keeps the merge: the cauldron ends exactly as the merge left it (soup,
  dumps and the merge's record), or it refuses and changes nothing; never
  one store from each side. It does not count what SQLite or a reader
  leaves beside a database as a write; it refuses over a write made after
  the undo put a store back unless told `--set-aside-later-writes`; it
  refuses an undo that can still be finished unless told `--lose-undo`;
  and, like `--abandon-merge` (which refuses a merge `recover` can still
  settle unless told `--keep-partial-merge`), it is the owner act `merge`
  when the merge or the undo was the owner's. Where two things were removed
  by hand so that neither side can be had, it says so (`torn`), and no dump
  ever takes an id a record cites. An undo cut before it moved anything
  whose snapshot or record is gone is dropped, saying so.
- **`cauldron merge --status <MG-n> --into <target>`**: planned, landing,
  abandoned, restored, merged, undoing (how far), undone, torn or partial,
  read from the stores rather than from a marker, and judged against what
  that merge carried (records only, dumps only, or both), so a merge of
  dumps alone is `merged` when its dumps and record are there. `cauldron
  soup recover --settle-merge <MG-n>` completes a torn cauldron: a record
  left over is moved aside; where part of the merge is gone, it lists the
  records citing a missing dump and how to re-point them (merging again does
  not repair them), and once none that can be changed is left it settles
  the merge as it stands, its record naming what was lost. `cauldron
  status` lists every merge or undo cut short, with the command that
  finishes it.
- **All or nothing, across the soup and the dumps store, with a way out of
  every cut.**
  The merge holds the target (the spine's halted state, which every release
  honours, so an older `cauldron` cannot write while it is held), lands
  each dump, then its records (one write, a second of the ripple class for
  the records a ripple wrote that nobody decided, and a third for the edges
  between the two), the last lifting the hold. Cut short, `cauldron soup
  recover` abandons it (nothing landed), finishes it exactly once, or, when
  it cannot finish it, puts the target back from the snapshot and says so;
  `--abandon-merge` lifts the hold and keeps what landed, for a merge it
  can do neither with, and sets aside a merge journal that cannot be read
  or lifts a merge's hold nothing names any more. Until then every writer
  of this release refuses, naming it, and every read carries
  `merge_pending`. Every file the merge and the undo depend on was taken
  away or damaged at every phase in a test: each time, commands alone made
  the target writable again.
- **Forwarded to the `mcp` service** when it holds the target: `MERGE` from
  a terminal, `AMERGE` without one where the persisted setting opens
  `merge` (whatever the merge carries). While it lands, a session's write
  to the target is refused at once with `busy` and `retry_after_secs`,
  not held in the queue for the length of the merge; reads go on.
- **`soup get` shows `merged`** (the merge, when, and the cauldron and id a
  record came from).
- **`spine::Cauldron::hold`**: one writer's work over several commits holds
  the cauldron, refusing every other writer by the halted state, with the
  hold lifted in the same transaction as the work's last commit.

### Changed

- **`soup_changes`**: each of a merge's soup writes is a change of an
  existing op (`create`, or `link`) carrying `merge: {id, part:
  main|ripple|links, from: {cauldron_id}, counts, edges}` (and `resync`
  when `ids` cannot list every record). The set of `op` values is
  unchanged.
- **Every write but a merge refuses `merged_from`**, in any casing, dotted
  or nested, anywhere in what it carries, naming it (`refused:
  merged_from`): `soup import`, `soup import-corpus`, `soup_put` (records
  and `cite`), `soup_link` (listed or flat), `soup_unlink`, `soup_cite`,
  `soup_claim`, a dump's `cite`, and a ripple's batch.
  `schema/soup/write-back.json` lists `meta.merged_from` as refused and a
  read's `merged` as ignored.
- **`soup_cite`** treats a ripple's record a merge carried as a ripple's
  record, whichever of the merge's writes it landed in: its `dump`
  citation stays its review trail.
- **Importing a large batch is faster**: it is judged whole first, and item
  by item only when that refuses (to say which record and why), and three
  lookups that grew with the square of the batch are gone.
- A merge's dry run judges its batch in a private read directory, removed
  when it ends and swept after a crash.
- A refusal by the soup's writer lock names `cauldron merge` for a merge
  (and a merge that is landing), carries `retry_after_secs`, and `soup
  import` prints it as JSON too; a `soup recover` refused while the `mcp`
  service holds the soup says to close the cauldron to it or stop it first.
- The published schemas (`workspace@6`, `cauldron@1`) keep their validation
  keywords.

### Not carried, and not done

- The source's MCP grant and automatic ripples setting are never carried,
  and the source is never changed or removed.
- A merge does not change a record it does not carry: an edge a record
  already in the target gained in the source since is listed, not added.
- An undo does not undo a merge while keeping later writes: a later write
  can name the merge's records, and writing it again would make it another
  write.
- A release of 0.1.17 or earlier reads a merged target and sees its
  unreviewed records as unreviewed, but counts a carried decision as an
  ordinary record, and cannot finish a merge or an undo that was cut short
  (it is refused while either holds the target).
- A merge's batch is one soup statement, so its memory grows with it
  (about 56 KB a record).

## [0.1.18] — 2026-11-24

Installing and upgrading from a prebuilt bundle, from the issues filed
after a test of 0.1.17 on a second machine (issues #30 to #35). An upgrade
with `--replace-running` now moves the threads daemon to the new build, as
it already moved the MCP service. The account helper scripts run on
macOS's own bash. An account is named by its email address. Text can be
selected and copied on every surface of the terminal interface.

### Fixed

- **`install --replace-running` left `threads` on the old build** (issue
  #31). Upgrading a 0.1.16 install with 0.1.17's installer re-pinned `mcp`
  and left `threads` pinned to 0.1.16, so the account helpers the new
  installer laid down were not where the running daemon looked, and the
  fix for #30 never reached it. A `threads` pin at another build is now
  built from the bundle (or the checkout) and re-pinned like `mcp`'s,
  keeping its address; the supervisor restarts the daemon on the new
  build. Without `--replace-running` the pin is left, and the install says,
  in step 4b and in its closing lines, that threads is still the old build.
  Running this release's install with `--replace-running` over a 0.1.16 or
  0.1.17 install moves both services; the manual `build threads` and `pin
  threads` steps of the issue's workaround are no longer needed.
- **Account login asked for a newer bash** (issue #33). `add-account` and
  `claude-accounts` required bash 4.4 and, under the daemon (whose PATH
  finds macOS's `/bin/bash` 3.2) on a Mac without Homebrew's bash, refused
  with "needs bash 4.4 or newer ... brew install bash". Both now run on bash
  3.2, and the requirement is gone.
- **The terminal UI gave up on a login before the daemon answered** (issue
  #30, comment of 2026-11-23; this release does NOT move the login out of
  the daemon, see Known limitations). The face bounded every request at 30
  seconds, and the daemon gives the login script 60 seconds to print its
  URL (180 for the pasted code), so the only thing shown was "the daemon did
  not answer within 30s". The face now waits 120 seconds for `begin` and 240
  for `complete`, longer than the daemon's own bounds. When the script
  prints no URL in time, the daemon's refusal carries the last lines it
  wrote, so the step it stalled on shows. The daemon's own keychain read
  before the login, which had no bound, is bounded at 20 seconds and
  refused naming a keychain dialog or a locked keychain as the likely cause.
- **Typing the account's email address was refused as a label** (issue
  #35). An account may now be named by its email address, its one name:
  the terminal UI asks for the address when adding a Claude Code account,
  and the daemon, `add-account` and `claude-accounts` accept it (lowercase).
  Older names stay valid, so an existing install keeps its accounts.
  `claude-accounts login <address>` names a new account by the address;
  an account already filed under the older derived name (`a-b-example-com`)
  keeps it. The face shows an account by its address, in full, everywhere
  it names it (until now every address was drawn masked, `a•••@e•••`); an
  account's id, a settings key, still never carries the address, and an
  address OTHER than the account's own in a daemon's or tool's message is
  still masked. An address name has to be the account's own login: an
  address-named account logs in only as that address (a different
  `--email` is refused), and renaming a slot to an address is refused
  unless the slot's login records that address (its login hint, or the
  account in its `.claude.json`), or when it records none.

### Added

- **Select and copy on every surface** (issue #34). A drag selects the text
  on screen in the cauldron face, in modals (the login modal's error and
  URL among them), notices, lists, pages and settings, and the release
  sends it to the terminal's clipboard (OSC 52). A press acts only as a
  click: released on the cell it was pressed on, it does what it always
  did; dragged, it only selects, and the button, toggle, option, row or tab
  it started on is never activated or changed. The divider drags are
  unchanged. OSC 52 has no acknowledgement, so the notice says the text was
  SENT and how to copy otherwise in the terminal in use: Terminal.app does
  not accept OSC 52 (turn off View > Allow Mouse Reporting, or hold Fn
  while dragging, then Cmd-C); iTerm2 accepts it only with "Applications in
  terminal may access clipboard" on (else hold Option while dragging);
  other terminals: hold Shift while dragging for their own selection. The
  chat face's copy notice says the same.
- **`cauldron --version` names the release** (issue #32):
  `cauldron 0.1.18 (012345678901)`, the commit shortened as build labels
  shorten it. `--build-stamp` still carries the full commit.
- **Where the daemon finds the account helpers** (issue #32): the threads
  daemon's `/health` answer carries `helpers`, the resolved path of
  `add-account`, `claude-accounts` and `claude-login-browser` and whether
  each passes its trust check, so an upgrade can be checked without starting
  a login.

### Changed

- **`live:unverified` says it is expected** for a hosted service such as
  `threads` (issue #32): the status line says nothing needs doing, and the
  README explains the state.
- The installer's closing `status:` line names the binary an environment
  other than `prod` actually has (`cauldron-<env>`).
- **A re-pin replaces any other build, and says how to go back.**
  `--replace-running` replaces a `threads` or `mcp` pin at any build other
  than the bundle's (older, newer or hand-built), and now says so and
  prints `cauldron --env <env> rollback <service>`, the way back.
- **Tests never reach the real keychain.** Every `security` the threads
  daemon runs is `/usr/bin/false` in a process a cargo test started, and
  the suites that read or write the real login keychain by design
  (`claude-accounts-prune`, `claude-accounts` and `add-account-login`
  conformance, `prove-accounts.sh`, and three codesigning-identity tests)
  run only with `CAULDRON_TEST_ALLOW_REAL_KEYCHAIN=1`.
- `q()`, the account scripts' quoting for messages, escapes every byte
  outside printable ASCII (an 8-bit control byte could reach the terminal
  raw in the C locale); the release check parses under macOS's bash 3.2.

### Known limitations

- **A real account login from a prebuilt install is still not confirmed by
  the project** (issue #30 stays open). Login, rotation and set-live are
  tested against stand-in scripts, a stand-in `claude`, fake keychains and
  a fake daemon; the issue's report is what a real one does. What stalled
  the reported login is not known; 0.1.18 makes the daemon say where it
  stalls and waits for it.
- **0.1.18 does not do what the #30 comment asks**: the login still runs
  `add-account` and `claude auth login` as a child of the threads daemon,
  with no terminal. What changed is what is shown when it stalls (above).
  Moving the login into the terminal UI's own session (the UI runs `claude
  auth login`; the daemon keeps the account checks before and after it) is
  the planned next step.
- Existing accounts are not renamed to their addresses automatically.
  `threads accounts rename <old> <address>` does it, slot and keychain item
  together, only when the slot's login records that address; a slot whose
  login records none must be logged in again under its current name first.
- **Going back to 0.1.17**: `cauldron --env <env> rollback threads` and
  `rollback mcp` re-pin the builds that ran before; running 0.1.17's
  installer instead moves `mcp` back but leaves `threads` on 0.1.18. Rename
  any account named by an address to an `[A-Za-z0-9_-]` name first: 0.1.17
  refuses an address name (it cannot rename or delete such an account).
- Selection covers what is on screen and does not scroll. Whether a copy
  landed is the terminal's to say; the face cannot know.

## [0.1.17] — 2026-11-20

The consumer interface of issue #1 holds as written. Every tool in the
interface table of issue #10 can be called exactly as the table writes it.
A Claude Code project can have an MCP server for one cauldron. A wrong edge
or citation can be fixed on the record itself (issue #24). Three fixes left
over from 0.1.16: reads of a quiet threads store, the plugin launcher
started from a subdirectory, and the directory a private read left behind.
Who may act for the owner becomes the owner's setting (issue #25). Local
inference on a prebuilt install: the bundle now carries the `inference` and
`ripples` services, and `cauldron inference models pull` fetches one of the
two measured models when you ask for it. Account login, rotation and the
account board's set-live work on a prebuilt install (issue #30). Ripples
use Opus by default (issue #27). A record read with `soup_get` can be
written back, and the confusions reported at 0.1.16 are fixed or documented
(issue #23).

### Added

- **`soup_unlink {from, kind, to}`** (scope `write`, audited; also `{links:
  [...]}`, as `soup_link`) and **`cauldron soup unlink <from> <kind> <to>`**
  remove an edge between two records the soup holds, in one write, through
  the store's existing edge removal. `soup_get` stops showing the edge, and
  `soup_changes` lists the write as `unlink`. Removing an edge that is not
  there answers `not-linked` and writes nothing. Each of these is refused
  by name, and then nothing of the call is written:
  - `required-edge`: a record's last `in_region` (or, for a record placed
    by `rejected_by`, the edges and field that stand for it), or a
    milestone's last `serves`;
  - `supersedes-is-history`: a `supersedes` edge, except one from a record
    to itself or one on a cycle of `supersedes` edges, which may be removed
    (neither records a replacement, and each hides the records on it);
  - `claims-is-soup-claim`: a cluster's claim, which `soup_claim` releases;
  - `unknown-node` and `unknown-kind`;
  - `region_locked`: a removal touching a region a ripple holds.
- **`soup_cite {id, add | remove}`** (scope `write`, audited) and **`cauldron
  soup cite <id> --add <json> | --remove <index-or-json>`** add or remove
  one entry of a record's `meta.cites`, on a workspace record or a cluster.
  An added citation is judged by the import's citation table (`bad-cite`).
  A removal names the entry by index or by an object it matches; a match
  of more than one entry is refused. The call writes `meta.cites` and no
  other key, so the review flags and a ripple's provenance stay out of
  reach. Adding a citation the record already has answers `already-cited`,
  removing one that is not there answers `not-cited`, and neither writes.
  On a record a ripple wrote, the `dump` citation that ties it to its dump
  (what the review queue groups by, and what approving or rejecting by
  `--dump` selects on) is neither added nor removed: refused
  `dump-cite-is-provenance`, nothing written.
- **`cauldron soup link <from> <kind> <to> [--note]`**: `soup_link` from a
  shell (issue #23, item 2.2). `cauldron link help` now says that
  `cauldron link` names a project's cauldron and does not connect records.
  While the `mcp` service holds the soup, `soup link`, `unlink` and `cite`
  are sent to it, as `soup claim` is.
- **`soup_link {from, kind, to, note?}`**: one link given flat, as the
  interface table writes it. `{links: [...]}` still lands several links in
  one write. A call that gives both forms is refused as `usage`, and the
  message names the keys that clash. A link refused for an unknown edge kind
  now lists the kinds the grammar has (`refused.kinds`). It refuses a
  `supersedes` edge (`supersedes-at-creation`: one is written only with the
  new record that supersedes, through `soup_put` or an import) and a
  `narrows` edge from a region to itself or closing a cycle (`edge-loop`).
- **`soup_status` takes `max_chars`**, as every other read does, with the
  same default, floor and ceiling.
- **A refused record says what to do in two common cases** (`soup_put` and
  `cauldron soup import`). A record whose id the soup already holds carries
  a `hint`: the soup never writes over a record, so write the new version
  under a new id with a `supersedes` edge to the old one. A batch-local
  label sent without `remint` carries a hint that names `remint`.
- **`cauldron mcp config --cauldron <name> [--env <env>]`** prints the
  `.mcp.json` entry of an MCP server for that one cauldron, and nothing else
  on stdout. The server is `cauldron-<name>`. It runs this `cauldron` by its
  absolute path as `mcp stdio --env <env> --cauldron <name>`. The verb
  refuses a cauldron the environment does not have. For a cauldron with no
  grant it prints the grant command on stderr. It writes nothing and grants
  nothing. The entry has no `env` block: it never carries
  `CAULDRON_OWNER_ACTS` into a project's `.mcp.json`, and there is no flag
  that would.
- **`soup_approve` and `soup_reject`**, MCP tools (scope `write`): a Claude
  Code session approves or rejects a ripple's records (by `ids`, `dump` or
  `run`; `reason`; `undo`) where the owner opened `approve` to agents for
  that cauldron, recorded `by: "agent"` with the session. Refused
  `owner_act_closed`, nothing written, otherwise. The `mcp` service follows
  the persisted choice alone: `CAULDRON_OWNER_ACTS` never reaches it.
  `/cauldron:review` and the curator use them when the user asks and the
  owner allowed it, and give the terminal commands when not. Granting and
  turning ripples on stay command-line acts.
- **`by` everywhere an act is recorded**: the command's answer, the grant
  row, the `mcp` audit log (`calls.jsonl`), a `turned_on` line in the
  automatic journal, and each decided record. Listed records carry
  `decided_by`; the review filter takes `by-tty` and `by-agent` (`cauldron
  soup nodes | search --review`, `review` on `soup_nodes` / `soup_search`);
  the review queue counts decisions by who made them.
- **The status views** (`cauldron status`, `cauldron mcp status`, `cauldron
  ripples status`, `cauldron owner-acts show`) show the effective
  owner_acts setting twice over: per act for a command run from that shell
  without a terminal (`acts`, the variable marked `env`), and what MCP
  sessions may do (`sessions`, the persisted setting alone). `soup_status`
  carries the door's own answer (`owner_acts`). `ripples status` shows who
  last turned automatic processing on.
- **`ADECIDE`**, a new `mcp` seam verb: a decision made without a terminal
  goes on it, so a service older than this release answers `ERR unknown
  request` and writes nothing rather than recording an agent's decision as
  one made at a terminal. The service takes it only where the persisted
  owner_acts setting opens `approve` for the cauldron (`owner_act_closed`
  otherwise). `DECIDE` is unchanged: an older command line's decisions land
  as `tty`, which records that the command had a terminal attached and
  cannot be checked by the service.
- **The bundle carries `cauldron-inference` and `cauldron-ripples`**
  (`libexec/`), with their declarations, built with the same flags, ad-hoc
  signed (identifiers `cauldron-inference` and `cauldron-ripples`) and
  listed in the bundle's checksums. The release gate checks them as it
  checks every other binary (arm64, signature, identifier, the byte scan
  for compiled-in paths, libraries, the runtime probe), and checks that
  each states the bundle's commit with `dirty=0`.
- **`cauldron --env <env> build <service>` from a release install**
  installs the image the release shipped (`inference`, `ripples`, `mcp`,
  ...), as `--artifact` would, with no cargo. It looks beside the binary
  (`../libexec/`) and in the installed release
  (`share/cauldron/releases/<version>-<commit>/libexec/`). A checkout build
  still compiles.
- **`cauldron inference models pull <name> [--yes]`** and `models pull
  --list`. The catalogue built into this release holds exactly two models,
  the ones this release was measured with: `qwen` (Qwen3.6-35B-A3B, Q8_0,
  from `ggml-org/Qwen3.6-35B-A3B-GGUF`, 36,903,139,616 bytes) and `gpt-oss`
  (gpt-oss-120b, MXFP4, from `ggml-org/gpt-oss-120b-GGUF`, 63,387,346,208
  bytes). Both are Apache-2.0 and pinned at a fixed revision with their
  sha256. Before anything is fetched it shows the size, licence, destination
  (`<root>/models/<id>/`, never inside a project), free disk and memory
  estimate, and asks for a typed `yes` (or `--yes`); away from a terminal
  without `--yes` it refuses. HTTPS only, through macOS's `/usr/bin/curl`
  (no `.curlrc`, no redirect followed by curl). The one redirect followed
  is from `huggingface.co` to its file store `us.aws.cdn.hf.co`, and it is
  named when it happens; any other host is refused. The transfer resumes
  with range requests into a private partial file. The size and sha256 are
  checked before the file is renamed into place and registered. A wrong
  sha256 sets the file aside as `.<file>.mismatch`, deletes nothing and
  registers nothing. The file is never run. The typed yes is consent to
  the download, not an owner act: `owner_acts` neither opens nor closes it.
- **`cauldron inference status` says what is missing**, under `missing`,
  each item with what to do: the service not installed or not started, no
  usable `llama-server` (none on `PATH`, or older than build 6000: install
  or upgrade llama.cpp with Homebrew; tested with 12110), no registered
  model, a model over the memory budget, and a budget larger than the Mac's
  memory.

### Changed

- **A repeated `soup_link` writes nothing (issue #23, item 2.12).** Through
  0.1.16 linking an edge the soup already held answered `linked` and took a
  new change entry that pointed at nothing new (the spine kept one edge).
  Now such a link is answered `already-linked`, its note stays as it was,
  and when every link of the call is, nothing is written (`outcome:
  "already-linked"`, `written: false`), as `soup_claim` answers
  `already-claimed`. In a list, the new edges land in one write and the
  held ones are named. `cauldron soup link` answers the same.
- **`cauldron mcp stdio --cauldron <name>` pins the bridge to that
  cauldron.** Through 0.1.16 the flag only chose the cauldron for a call
  that named none, and a call naming another cauldron reached that one.
  Now such a call is refused by the bridge as `cauldron_pinned` and never
  reaches the service. This holds while the service is down too. The
  bridge's `initialize` instructions say which cauldron it is pinned to.
  The owner-act tools are no exception: a pinned bridge refuses
  `soup_approve` and `soup_reject` naming another cauldron the same way.
  `$CAULDRON_CAULDRON` is unchanged: it only chooses a default.
- **The plugin launcher finds the project root from a subdirectory.**
  Through 0.1.16 the launcher treated only its current directory and
  `$CLAUDE_PROJECT_DIR` as the project. A session started in a subdirectory
  with no `$CLAUDE_PROJECT_DIR` therefore still ran a `cauldron` from an
  absolute `PATH` entry elsewhere in the repository. Now every directory
  above either of them that holds a `.git` or a `.mcp.json` is part of the
  project too, up to but not including `$HOME`. A candidate inside the
  project is refused from every source. A home directory kept under git is
  not a project.
- **Who may act for the owner is the owner's setting** (issue #25).
  `cauldron mcp grant`, `cauldron soup approve` (`--unapprove`), `cauldron
  ripples approve | reject` (`--unapprove`, `--unreject`) and `cauldron
  ripples on` were refused unless their input and output were an
  interactive terminal, as a fixed rule. They now ask one shared decision:
  a command run with a terminal attached may always do them (`by: "tty"`);
  without a terminal, an act runs only when the owner opened it (`by:
  "agent"`), and
  is refused otherwise with the same words as before plus one sentence on
  how the owner allows it. The default, `terminal`, changes nothing: a root
  from 0.1.16, with no setting, behaves exactly as before. The terminal
  check stops an agent that follows the rules; it is not a security
  boundary against a process of the same user that fakes a terminal
  (`script(1)` does) or reads the user's files, as the README's "Who may
  act for the owner" now says: `by: "tty"` means "run with a terminal
  attached", not "a person".
- **The setting, `owner_acts`**, in `<root>/owner-acts.json` (owner-only):
  one choice for the root and, optionally, one per cauldron that wins over
  it both ways (bound to the cauldron's id, as its grant is). `terminal`,
  `open`, or a list of the acts that are open (`grant`, `approve`,
  `ripples`), each recorded with when and how it was set (`tty`, `env`,
  `install`), by the command that set it: `cauldron owner-acts` takes no
  `--how`, and only `cauldron install --owner-acts` records `install`.
  Three ways to choose it: `cauldron owner-acts open | terminal
  [--only grant,approve,ripples] [--cauldron C] [--env E]` (opening needs a
  terminal or the variable; closing runs anywhere; `unset --cauldron C`
  hands a cauldron back to the root's choice; `show` prints what is in
  effect per cauldron and where it came from; a cauldron with no choice of
  its own that is given one starts from the root's, and the answer names
  what it took over as `copied_from_root`); the environment variable
  `CAULDRON_OWNER_ACTS=open` (or a comma list) in the environment of the
  `cauldron` command that acts, which opens `grant` and `ripples` for that
  command and lets `cauldron owner-acts open` record a choice without a
  terminal (anything that can set its own environment, an agent told to
  included, can turn it on: the owner decides whether their agents may act
  for them; the README names a shell profile or your user settings for
  Claude Code, never a project's `.claude/settings.json`); and
  `scripts/install --owner-acts open|terminal` for a headless install,
  which passes it to `cauldron install`. The variable is read only by the
  command that carries it: `cauldron start` and the supervisor remove it
  from every service they start, and approving and rejecting, which the
  `mcp` service carries out, need the persisted setting (the command says
  so before it sends anything when only the variable opened `approve`).
- **`cauldron ripples on`, allowed without a terminal**, asks nothing and
  prints what it authorised.
- **Every refusal of an owner act** names how the owner allows it; the
  ones that said "no option skips this" no longer do (`review --interactive`
  still needs a terminal, because it asks a person).
- **A decided record says who decided**: `meta.approved` and
  `meta.rejected` carry `by: "tty"` or `by: "agent"` (and the session, for
  a decision made through the MCP door), where 0.1.16 wrote `by: "owner"`.
  A decision recorded as `owner` was made at a terminal and reads as `tty`.
- **`scripts/install --prebuilt`** verifies the two new images and their
  declarations, as it verifies the others, and lays them down with the
  bundle, but pins and starts neither. Both install paths end with the
  commands that turn local inference on.
- **The docs no longer say that local inference never downloads
  anything.** Nothing is downloaded unless you run `models pull` and say
  yes. The model still gets no tools in this release; threads on a local
  model are planned for a later release. The README walks a prebuilt
  install to its first local ripple.
- **Ripples use Opus by default (issue #27).** The first call of a ripple on
  the threads backend is now Opus (it was Sonnet); the one retry after a
  refused answer stays Opus, `--model` (on `ripples run` and `ripples on`)
  still chooses the first call's model, and a local run is unchanged (both
  calls on its local model). `ripples eval`'s threads model follows the same
  default (`--threads-model` still overrides). The command-line help, the
  "what leaves this machine" text `ripples on` prints, and the README say
  so. The caps are unchanged: a call's input-token estimate is the prompt's
  characters divided by three whatever the model, so the per-dump and
  per-day caps count the same; an Opus call costs more of your Claude
  usage than a Sonnet one. A cauldron turned on under 0.1.16 or earlier
  without `--model` consented to Sonnet: it runs on Opus now, and not
  silently. `ripples status` shows `default_changed` for it (Sonnet to
  Opus, the cost, and `cauldron ripples on <c> --model sonnet` to keep
  Sonnet), and its first automatic run after the upgrade writes one
  `default_model_changed` line to the automatic journal. A setting that
  recorded a model keeps it, and `ripples on` now records the default it
  was turned on under. This holds until ripples split graph writing
  (a cheaper model) from reasoning over issues, contradictions and work
  items (a stronger one).
- **A record read with `soup_get` can be written back (issue #23, 2.4 and
  2.6).** `soup_put` and `cauldron soup import` (direct, dry run, or sent on
  to the service) drop the keys a read adds and never stores
  (`superseded_by`, `unreviewed`, a top-level `rejected: true`, a cluster's
  `claims` list) and name each one in the answer's new `ignored` list.
  Through 0.1.16 each was refused (`undeclared_field`, or as a review
  flag). The keys a write still refuses that a read can show (the review
  flags `meta.approved` and `meta.rejected`, `meta.ripple`, and a held
  `id`) are published, with the dropped ones and what a read may cut, as a
  machine-readable list, `schema/soup/write-back.json`, so a client strips
  them from the list instead of by hand. The list says how the review
  flags are written instead: by the owner's decision at a terminal or,
  where the owner opened approving to agents, through `soup_approve` /
  `soup_reject`; never by a record write. `soup_link`'s tool schema now
  carries the 1,000-character `maxLength` on `note` that `soup_claim`'s
  already did.

### Fixed

- **A read of a threads store that no daemon holds open no longer reads
  the file in place.** `threads cutover`, and the account inventory's read
  of which accounts are disabled, opened such a store `immutable=1`, with
  no lock. If a writer opened the store meanwhile and checkpointed its log,
  pages changed under the read. The read could then fail with `database
  disk image is malformed`, or return a wrong answer. A failed read of the
  disabled accounts was taken as "none disabled". The store is now read the
  way 0.1.16 reads a soup: through a private copy that is checked not to
  have changed while it was taken. With a `-wal` present, it is read
  through SQLite's locking, as before. The copy writes nothing beside the
  store. If the store changes during 20 copies in a row, the read falls back
  to `mode=ro`, which is correct but may leave `-wal` and `-shm` beside it.
- **A read through a private copy leaves no directory behind.** Through
  0.1.16, a read of a soup no daemon holds made a directory named
  `spine-read-<uid>` in the temporary directory, kept it, and shared it
  between every read of that user. Where the temporary directory is shared
  (`/tmp`), another user could make that name first, which turned the
  private copy off for the owner (the read then went through SQLite's
  locking). Now each read makes its own directory under a new name that
  cannot be guessed, mode 0700, holding a lock while the read lives, and
  removes it before the read returns. What a process killed in the middle
  of a read leaves is removed by the next process's first read. A threads
  store's read (above) works the same way. A `spine-read-<uid>` directory
  left by 0.1.16 is not removed automatically; it can be deleted by hand.
- **No session can write `meta.ripple` (issue #23, 2.10).** Ripple
  provenance is stamped by the door on a ripple's records only. Every other
  record write (`soup_put`, `cauldron soup import` with or without
  `--remint`, direct, dry run or forwarded to the service) now refuses a
  record that carries it, in any casing, dotted, nested or as a top-level
  `ripple`, as `usage` with `detail.refused: "ripple_provenance"`, the way
  the owner's review flags are refused. Through 0.1.16 a session could land
  a record carrying a made-up `meta.ripple`; it gave no power (authorship
  is read from the writing transaction, and `soup_review` never listed
  it), but a reader of the record could be misled. Records already stored
  are not changed.
- **Logging an account in from the terminal UI works on an install (issue
  #30).** The threads daemon ran `add-account` (and, to rotate,
  `claude-accounts`) from `<data dir>/scripts/`, which nothing installs, and
  the scripts built `threads` with `cargo run` from a source tree they
  assumed was three directories above them; on a prebuilt install the login
  was refused before it started. Now the bundle ships `add-account`,
  `claude-accounts` and `claude-login-browser`
  (`share/cauldron/scripts/threads/`, covered by its `SHA256SUMS`), and
  `scripts/install` lays them down at `<root>/share/cauldron/scripts/threads/`
  on the prebuilt and the from-source path alike (every directory and
  script 0755 and the user's; an upgrade with `--replace-running` replaces
  them, the previous copy kept as `threads.prev`). The daemon finds them
  there through the `CAULDRON_HOME` its supervisor sets (never a path
  compiled in), falling back to `<data dir>/scripts/` for a development
  home, and runs one only when it is yours and nothing on its path is
  writable by group or others; a missing one is refused naming the command
  that installs it. It tells the scripts which `threads` to run
  (`CAULDRON_THREADS_BIN`, its own binary) and, for rotation as for login,
  which store (`CLAUDE_ACCOUNTS_HOME`). An installed copy of a script never
  runs `cargo`: it runs that `threads`, or the one the cauldron installed
  beside it pins, and refuses to run without a named store; a copy in a
  source checkout behaves as before.
- **The account board's "make this the machine's login" works on a prebuilt
  install (issue #30).** `tui pool`, key `s`, runs the installed
  `claude-accounts` (`<root>/share/cauldron/scripts/threads/`, found beside
  the running `tui` at run time; a release-bundle `tui` still compiles in no
  helper path) with the environment's threads store, after the same
  ownership and mode check as before. Through 0.1.16 a prebuilt terminal UI
  had no helper and the action refused.
- **The account helpers never load code from the directory they are
  started in.** They piped the keychain secret into `python3 -c` with no
  isolation, and the terminal UI started them from its own working
  directory, so a `json.py` in a project the board was opened in was
  imported with the secret on its stdin (the code was older; this release
  ships it to every prebuilt install). Now every python a shipped script
  runs is one interpreter at a fixed absolute path (`/opt/homebrew/bin`,
  `/usr/local/bin`, or `/usr/bin` when the Command Line Tools are
  installed), not writable by group or others, started with `-I` (no
  current or script directory on its path, no user site-packages, no
  `PYTHON*` variables). The helpers and `install` keep only the absolute
  entries of `PATH` and drop `CDPATH`, `GLOBIGNORE`, `BASH_ENV` and `ENV`;
  `claude-login-browser` runs `/usr/bin/open`. The threads daemon and the
  terminal UI start a helper from a fixed directory (the threads store, or
  `/`), never the caller's, with no `BASH_ENV`, `ENV`, `IFS`, `CDPATH`,
  `PYTHON*`, exported bash function or dynamic-loader variable, and a
  `PATH` of absolute entries.
- **A service never inherits `CAULDRON_OWNER_ACTS`.** `cauldron start` and
  the supervisor passed the caller's environment on, so a shell carrying it
  opened the MCP door's `soup_approve` and `soup_reject` to every session
  while `cauldron mcp status` said `terminal`. Now the variable is removed
  from every service's environment, the door decides from the persisted
  setting alone, and the views say what sessions may do apart from what the
  shell may.
- **No record can be hidden for good by a `supersedes` edge.** `soup_link`
  accepted `supersedes`, a self-edge and a cycle included, and `soup_unlink`
  refused to remove one, so a session with write scope could mark any
  record superseded and no tool could undo it. Now `supersedes` is written
  only with its new record, self-edges and cycles of `supersedes` and
  `narrows` are refused on every write (`edge-loop`), and `soup_unlink`
  removes a self-edge or an edge on a cycle that an earlier build let in.
- **A change-feed `update` entry keeps its ids** when the record is written
  again later (a second citation, an approval then a citation): the ids are
  read from the write's own recorded changes.
- `scripts/check-release-bundle` checks the shipped helper scripts: present,
  mode 0755 in the tarball, bash, and, run from the unpacked bundle, refusing
  to run without a named store and reaching no build, keychain, Claude or
  git. It also fails a shipped script that runs python without `-I` or keeps
  a relative `PATH` entry, and, on the source of the bundle's commit when the
  clone has it, a helper spawn in the daemon or the terminal UI with no
  working directory. `scripts/test-check-release-bundle` plants a helper
  without its mode, helpers altered to behave as checkout copies, python run
  without `-I` (each spelling) and a relative `PATH` entry, and the gate
  fails each.

### Documentation

- `schema/soup/workspace-v6/README.md`, prose only (issue #23, item 2.1):
  "A node is never edited in place" is now accurate. Imports and `soup_put`
  only create nodes. Link, unlink, cite and the owner's review flags add to
  or take from a node that exists. The paragraph on adding an edge now also
  covers removing one. The plugin's copy is synced.
- The published schema's descriptions of `meta.approved` and
  `meta.rejected` say who writes them now (`by: tty` with a terminal
  attached, `by: agent` without one or through `soup_approve` /
  `soup_reject` where owner_acts opens it). Nothing a validator checks
  changed: a spine test now holds every validation keyword of
  `schema/soup/workspace-v6/` and `schema/soup/cauldron-v1/` (everything
  but `description`, `title` and `$comment`) to the files published with
  0.1.16, in place of holding the files byte for byte.
- The workspace-v6 README says where the door is stricter than the
  schemas: the review flags are declared (stored records carry them) and
  refused on every write, an edge `note` of `null` is taken as no note, and
  the `supersedes` and `edge-loop` rules above.
- The plugin README has a new section, "Correcting a record".
- The plugin README has a new section, "One server per cauldron". It covers
  the project default, a pinned server and its `.mcp.json` entry, and
  several cauldrons at once. The README has a short paragraph on the same.
- The tool descriptions of `soup_link`, `soup_put` and `soup_status`, the
  plugin README and the curator describe the forms above. The launcher's
  header and both READMEs describe the project root.
- Issue #23, the rest. 2.3: the quick start says a grant's `--project`
  only sets a default and does not limit access, as the grant help already
  did. 2.5: the workspace-v6 README opens with what changed around it since
  0.1.10 (rejection, the note bound, region locks, clusters, the write-back
  list, `meta.ripple`). 2.6: it says the door bounds a new edge note at
  1,000 characters though the schemas do not. 2.7: a claim holds nothing,
  and what holds a region is a ripple's region lock (README). 2.8: who reads a cluster's boundary (people and the
  sessions choosing where work belongs; nothing in the soup), and when to
  make a cluster rather than a parent region. 2.9: `soup_status` reports
  clusters separately, and its `total` and `counts` are workspace records
  only. 2.11: `--remint` rewrites an edge's `to` and `[[label]]` links in a
  record's `body` only; a label in a handle, a boundary or a note is kept as
  written. Of the workspace@6 and cauldron@1 schema files, only the two
  descriptions above and the workspace@6 README changed. Items 2.1 and 2.2 (in-place edits by
  links and claims, and a command-line `soup link`) are the workspace-v6
  README change and `cauldron soup link` above; 2.12 (a repeated
  `soup_link`) is under Changed.
- README "Known limitations": the account helpers' install location, the
  checks made before one runs, and how to run an installed copy by hand.

### Unchanged, whatever the setting

- A ripple never approves or rejects: the soup refuses the review flags to
  every ripple, and the door refuses the ripples service's identity on the
  review tools. No record write sets the review flags or a ripple's
  provenance.
- No root is special: there is no separate test root, and a development or
  check root's commands carry `CAULDRON_OWNER_ACTS=open` like anyone's (its
  services never see it, and its sessions follow its persisted choice). Test
  binaries remove the variable at load, so `cargo test` always starts from
  `terminal`.

### Known limitations

- **The pull catalogue fits large Macs only.** Qwen needs about 36 GiB
  resident and gpt-oss about 60 GiB. A smaller model has not been measured,
  so none is offered.
- **The redirect allow-list is the one host observed when the catalogue was
  made** (`us.aws.cdn.hf.co`). If the publisher serves the file from another
  host, `models pull` refuses and says which host. `models add` still
  registers a file fetched any other way.
- A real account login, rotation and set-live action from a prebuilt
  install have been exercised against fake scripts, a fake `threads` and a
  stubbed installer only, never against a keychain or a Claude account.
- The account helpers find `bash` 4.4 or newer through `PATH` (their first
  line is `#!/usr/bin/env bash`; when that one is older they run again
  under one from `/opt/homebrew/bin` or `/usr/local/bin`); the daemon and the terminal UI give them a `PATH` of
  absolute entries only. They need a python 3 at one of the three fixed
  places above.
- The owner-act gate is a guard against an agent that follows the rules,
  not a security boundary against a process running as the same user
  (see "Who may act for the owner" in the README).

## [0.1.16] — 2026-11-18

Clusters claim regions (issue #1). A cluster is an area of the surface of a
cauldron's soup: a name and a boundary description. It claims the regions
it covers, many-to-many. A claim is a statement about the surface, not a
lease on work. `cauldron soup import-corpus` brings an older corpus's
clusters in with their claims. `soup_changes` can also wait for the next
change.

### Added

- **A second published schema, `cauldron@1`** (`schema/soup/cauldron-v1/`,
  generated by the same code and checked by the same drift test as
  `workspace-v6`): one node type, `cluster` (`CL-<n>`; a required `handle`;
  a required `boundary` of 40 to 1,000 characters, the region's rule; an
  open `meta`; no state), and one edge kind, `claims`, from a cluster to a
  region in a pool of kind `workspace`, with the optional `note` every edge
  has (bounded at the door: see Changed). It lives in the soup's second pool, `cauldron`. `workspace@6` is
  unchanged, byte for byte. The plugin carries a copy in
  `schema/cauldron-v1/`.
- **The cluster pool.** `cauldron create` makes it. A cauldron made by
  0.1.4 to 0.1.15 gets it on its first write that lands a cluster, as one
  additive act under the writer lock: the pool file is built whole beside
  the store and linked into place in one step (never over a file already
  there); nothing is migrated or rewritten and no spine row is written for
  it. A batch the grammar refuses (judged first on a snapshot copy given
  the pool) adds nothing, not even the pool; a claim never adds it. An add
  cut short leaves only a staging directory no open reads, which the next
  add (or the next cluster write, once the pool is in place) removes. The
  spine adds no pool but this one to an existing cauldron.
- **Cluster records in `soup_put` and `cauldron soup import`.** A record of
  type `cluster` goes to the cluster pool in the same all-or-nothing write
  as the workspace records beside it, so one batch can make a region, a
  cluster and the cluster's claim of that region. A claim names a region of
  the batch by its label, or a held one as `R-3` or `R-3@ws`. Every guard on
  a record holds for a cluster: the review flags are refused, `meta.cites`
  is checked, the batch-already-landed guard and labels under `--remint`
  apply. A dry run of a cluster write on an older store is judged on a
  snapshot copy and adds no pool.
- **`cauldron soup import-corpus` carries clusters.** In 0.1.15 it listed
  an older corpus's clusters under `corpus.not_carried` and imported none.
  Now each cluster, and each child cluster, lands as a `cluster` record in
  the same all-or-nothing batch as the corpus's regions and nodes, through
  the same door: its name the handle, its description the boundary, and a
  `claims` edge to each region of the input it names, so the region lists
  it. `corpus.carried.clusters` counts them, and `corpus.id_map` names each
  as `cluster:<id>`. Still listed under `not_carried`, each with its
  reason: what a cluster holds besides its name, description and regions
  (by key and count), a claim of a region that is not in the input, a
  child's place under its parent (`cauldron@1` has no edge between
  clusters), and a cluster whose name or description is outside the
  grammar's bounds or that is incomplete (left out whole, never padded or
  cut).
- **`soup_claim {cluster, region, release?, note?}`** (scope `write`,
  audited) and **`cauldron soup claim <cluster> <region> [--release]
  [--note]`**: add one claim, or release it. Idempotent: `already-claimed`
  and `not-claimed` write nothing. Refused as `not_found` for a cluster or
  region the soup does not hold, `usage` for a node that is not a region,
  and `region_locked` while a ripple holds the region, as `soup_link` is.
  While the `mcp` service holds the soup, the command line sends the claim
  to it.
- **Reads.** `soup_region` (and `cauldron soup region`) fills `clusters`,
  the clusters that claim the region, from the claims' reverse edges (it was
  always empty). `soup_nodes {type: "cluster"}` (`cauldron soup nodes --type
  cluster`) lists clusters with the regions they claim. `soup_get CL-n`
  answers a cluster with its `claims`. `soup_search` matches cluster handles
  and boundaries; an answer of `soup_get` or `soup_search` holding a
  cluster says `cluster_pool: "cauldron"` beside `pool` (which still names
  the workspace pool). `soup_status` adds `clusters` and `cluster_schema:
  "cauldron@1"`; `schema` stays `workspace@6`, and `total` and `counts`
  still count the workspace nodes only, as in 0.1.14 and 0.1.15. All are
  bounded by `max_chars` and `limit` as before.
- **`soup_changes` reports cluster and claim writes** on the one cursor
  (every pool's writes share the spine's counter): a cluster is a `create`,
  a claim a `link` (its id the cluster's), a release the new op `unlink`.
  A write whose edges were removed since is still reported by what it did.
- **`soup_changes.wait_secs`** (0 to 60, with `since`; the polling half of
  issue #7): the call answers as soon as something lands after the cursor,
  or when the wait ends with nothing (`waited.ended` is `landed` or
  `timeout`, the cursor unchanged). A write through the service wakes it at
  once; a write by another process is seen within a second. The grant is
  re-read on every wake, a stopping service ends it, and it holds no writer
  or lock. It ends as soon as its caller hangs up (the connection
  closes: a cancelled tool call, a killed bridge), giving its slot back. At
  most 16 calls wait at once, and at most 4 of them one session's, so one
  session cannot hold them all; one more answers at once with `ended:
  "not-waited"` and `retry_after_secs` (at least 1: the time until the
  soonest slot frees). The stdio bridge honours it: it sleeps that long,
  asks again, and doubles the sleep each time it is told the same, within
  the call's own `wait_secs`.
- **`cauldron soup changes [--since <cursor>] [--wait <secs>]`**: the
  change feed from a shell, the same JSON as `soup_changes`.

### Changed

- **The change feed's `op` is an open set, and it has a new value,
  `unlink`.** This is a change to `soup_changes`'s reply contract.
  Releases 0.1.6 to 0.1.15 described `op` (in the `soup_changes` tool
  description, the only place it was documented) as exactly `create`,
  `link` or `update`, and said nothing about an op outside that list, so a
  consumer could fairly read the list as closed. 0.1.16 reports a write
  that only removed edges (a released claim) as `unlink`; a 0.1.14 or 0.1.15
  service reports the same write as `update` with no ids. From this release the
  tool description, the `cauldron@1` schema README, the README and the
  plugin README say that the set can grow and that a consumer must ignore
  an op it does not know (or read that change's ids again) and never fail
  on it.
- **A ripple writes no cluster and no claim in this release**: a ripple's
  batch holding a `cluster` record or a `claims` edge is refused, naming
  why.
- The change feed and its cursor moved from the `mcp` service into the
  `soup-door` library, unchanged, so the command line answers it too.
- **A claim is never read before its region.** A batch holding a region and
  a cluster's claim of it is one spine row with one part per pool, and the
  parts land one after the other. The spine now lands the far end of a
  cross-pool edge first (the region's pool, then the claim's), where it
  used to land parts by pool name. Every read that follows a claim
  (`soup_region.clusters`, `soup_get`, `soup_nodes {type: "cluster"}`,
  `soup_search`) reads the cluster pool first and the workspace pool second,
  and leaves out a claim whose region it cannot read. That covers a write
  in flight, a store left mid-write by a crash until `cauldron soup recover`
  (or the next open) finishes it, and a store left that way by any build
  that landed the claim first. `soup_changes` lists a write only once all of it
  has landed. Recovery still finishes or rolls back the whole row, both
  pools together.
- **An edge's note is bounded at the door.** The grammars let every edge
  carry a free-text `note` and put no bound on it, and until this release
  neither did the door. A new note, on a claim
  (`soup_claim`, `cauldron soup claim`), a record's edge (`soup_put`,
  `cauldron soup import`, a ripple's batch) or a link (`soup_link`), is
  now at most 1,000 characters, the bound the grammar puts on an edge's
  other text (`admits`); a longer one is refused with nothing written. The
  published schema files are unchanged, and nothing stored is refused or
  rewritten: a longer note written before reads back cut to 1,000 in every
  read, named in `truncated`, and `already-claimed` echoes it cut.
- **A service killed mid-call is a typed refusal.** When the `mcp`
  service dies while a call is in flight (during a `soup_changes` wait, a
  `soup_put`, any call), the stdio bridge answers the typed `service_down`
  refusal with the command that starts it, as it does in every other down
  case; it used to answer a JSON-RPC `-32000` error. For a write the
  refusal says plainly that whether it landed is not known, and to look for
  it with `soup_changes` since the session's last cursor (a `soup_put` sent
  again unchanged is refused as `batch_already_landed` if it did land).
  `cauldron soup import`, sent to a service that dies with it, says the
  same.
- **A read while a write lands is never refused or torn.** A read that
  does not hold the writer (every `soup` read from a shell, and the `mcp`
  service's reads) opened a soup file with no write-ahead log beside it as
  unchanging, without a lock. A write that began meanwhile checkpointed its
  log into that file as it closed, rewriting pages under the read: SQLite
  answered `database disk image is malformed` (refused as `store`), or the
  read returned a mix of the two states. Such a read now takes a private
  copy of the file (a clone on APFS) in a directory of the temporary
  directory only you can enter, keeps it only if nothing wrote to the file
  during the copy, and reads the copy; a file a writer has open is read
  through SQLite's own locking, as before. Nothing is written beside the
  soup's files. Earlier releases, 0.1.5 at least, have the same fault.
- **The plugin launcher refuses a project's file from every source.** In
  0.1.15 the rule that the project directory does not provide `cauldron`
  covered `$CAULDRON_BIN` and `PATH` only: `$CAULDRON_HOME` naming the
  project, or an install marker naming a file in it, ran that file. Now a
  candidate from any source whose resolved path is inside the current
  directory or `$CLAUDE_PROJECT_DIR` is skipped and named on stderr, and
  the next source is tried. A `$CLAUDE_PROJECT_DIR` that is your home
  directory itself is not a project, as the current directory already
  was not.

### Upgrading

- A store with clusters opens, reads and writes under 0.1.14 and 0.1.15
  (whose soup code is 0.1.14's),
  ordinary reads do not see clusters (`clusters: []`, `CL-n` missing,
  `soup_claim` `unsupported`); its `soup_changes` lists their writes by id
  (a claim or a release as `update`, with no ids). Its command line reads
  the cluster pool raw when asked for it by name (`--pool cauldron`), and
  lands a cluster record there (`soup import --pool cauldron`), judged by
  the pool's pinned grammar. A cluster record sent to a 0.1.14 or 0.1.15 `mcp`
  service is refused as an unknown type: upgrade the service with the
  command line (`scripts/install --replace-running`).
- A claim writes no workspace node or edge, but the spine records which
  parts of the write landed in each pool it touched, `ws.db` among them.
- Each waiting `soup_changes` call looks at the store again after every
  write the service lands, so many waiters slow writes somewhat (with 16
  waiting, a `soup_put`'s median was measured at about 47 ms against about
  20 ms with none). Nothing waits on a waiter, and every waiter sees every
  change once, in order.

### Documentation

- README: "Clusters and claims", and "Records in the older corpus format"
  on the clusters `import-corpus` now carries. The plugin README, the
  curator (which proposes clusters and claims, judged with a dry run, and lands one only
  when you approved it), `/cauldron:curate` and the `soup-schema` skill
  cover them. ROADMAP: clusters claiming regions, in place of "claiming
  work".

## [0.1.15] — 2026-11-18

Cauldron on an Apple silicon Mac without a Rust toolchain, a plugin that
finds it without the `PATH` Claude Code was started with, and the older
corpus format brought into a cauldron's soup.

### Added

- **Prebuilt binaries for Apple silicon.** `scripts/build-release-bundle
  <commit> <out-dir>` builds the public archive of a commit in a scratch
  directory and compiles `cauldron`, `tui`, `threads` and `cauldron-mcp`
  there, never in a working tree, with the build machine's paths remapped,
  no debug information and no local symbols. Each binary is ad-hoc signed.
  It writes `cauldron-<version>-aarch64-apple-darwin.tar.gz` and
  `SHA256SUMS`. The tarball's metadata does not depend on the builder:
  every member's mtime is the commit's own time in UTC (`BUNDLE` records it
  as `commit_time`), owners are 0:0 root:wheel, modes are 0755 or 0644
  whatever the builder's umask, entries are in byte order, the format is
  ustar, and gzip stores no name or time. The build runs under `TZ=UTC` and
  `LC_ALL=C` with `SOURCE_DATE_EPOCH` set to the commit's time.
  `scripts/check-release-bundle` fails a bundle whose checksum, entries,
  contents, signatures, architecture or tarball metadata are wrong; whose
  files hold the home directory of whoever runs it, `/Users/` or the
  repository's path, this machine's host names or the git `user.email` of
  whoever runs it; whose binaries hold that user's name, a personal
  pattern, the build's own scratch directory, or a path in a location
  another user could create (under `/tmp`, `/private/tmp`, `/var/tmp`,
  `/var/folders` or `/Users/Shared`, or a relative `./` or `../` path);
  or whose binaries, run with `--help`, `--build-stamp` or `--self-check`
  under macOS's unprivileged sandbox, touch such a location
  (`scripts/probe-creatable-paths`), or which link a library outside
  `/usr/lib` and `/System/Library`. The build refuses a scratch root that
  another user owns or can write to, or whose path holds the builder's
  user name.
- **`scripts/install --prebuilt <tarball-or-url> [--sha256 <sum>]`.** It
  installs from the bundle with no cargo, git or compiler. It checks the
  tarball's sha256 (from `--sha256`, or from a `SHA256SUMS` beside a local
  tarball; a URL needs `--sha256` and must be https). It refuses a tarball
  with an entry outside its directory or a link, one that carries macOS's
  quarantine attribute, and a machine that is not arm64. Every binary must
  be a thin arm64 executable whose ad-hoc signature verifies. It places the
  bundle under `<root>/share/cauldron/`, installs `threads` and `mcp` as
  versions from it, and runs the same environment, pin, supervisor and
  closing steps as the from-source install, which stays the default. The
  bundle's declarations go to `<root>/share/cauldron/{services,mcp}` as the
  last write before the supervisor step, so a run that stops earlier
  leaves an installed cauldron on the declarations it came with. A
  download is cut off past 256 MiB. The bundle's own `install`, run
  without `--prebuilt`, refuses and says to pass it the tarball.
- **`cauldron build <service> --artifact <image>`** installs a prebuilt
  image as a version of a service, with no compiler; the version is named
  by the commit the image states (`--build-stamp`).
- **The plugin's MCP launcher.** `.mcp.json` now runs
  `${CLAUDE_PLUGIN_ROOT}/bin/cauldron-mcp`. It finds `cauldron` at
  `$CAULDRON_BIN`, then the install marker the installer writes for `prod`
  (`~/Library/Application Support/cauldron/cauldron-bin`), then
  `<root>/bin/cauldron`, then `PATH`. Every candidate must be a regular
  file, not a symlink, owned by the user or root, that group and others
  cannot write, in directories owned by the user or root that group and
  others cannot write. `$CAULDRON_BIN` inside the project directory is
  refused. The `PATH` step skips empty and relative entries, entries
  inside the project directory, and entries in directories others can
  write. With none found, it answers the
  MCP handshake itself as a server with no tools whose instructions say how
  to install. A Claude Code started from the Dock or an IDE gets the tools
  without `cauldron` on its `PATH` (issue #22).
- **`cauldron soup import-corpus <file-or-dir>`** lands records in the older
  corpus format (corpus schema version 5: a seed file, a bare corpus, one
  record or a directory of one-record files) in a cauldron's soup, as
  workspace-v6 records with new ids and the old id in `meta.imported_from`.
  It goes through `cauldron soup import --remint`: the same door, all or
  nothing, the batch-already-landed guard, the refusal of a write aimed by
  a tracked marker, and forwarding to the `mcp` service. Its JSON lists
  what was not carried and why, under `corpus.not_carried`: clusters,
  node types outside the ten, references to ids not in the input, a
  symbolic link in a directory input (not followed), the seed's messages
  and dashboards. A `regions`, `nodes` or `clusters` that is not an array
  refuses the import. It gives the old-to-new `corpus.id_map`
  (issue #4).

### Changed

- `scripts/install`'s closing lines say where the plugin's launcher looks
  for `cauldron`, instead of saying that `cauldron` must be on Claude Code's
  `PATH`.
- A binary built with `--cfg cauldron_release_bundle` (the bundle's) reads
  its shipped service and MCP tool declarations from `../share/cauldron/`
  beside its own path with symlinks resolved, so a symlink to it elsewhere
  reads the same ones. A binary built from a checkout reads them from the checkout,
  as before.
- The terminal UI runs the `claude-accounts` helper only from an absolute
  path to a regular file you own that group and others cannot write, in
  directories owned by you or root that nobody else can write, whether the
  path is its default or `TUI_POOL_ACCOUNTS_TOOL`. A release-bundle build
  compiles in no default for it.
- `threads` compiles in `SOURCE_DATE_EPOCH` as its build time when that is
  set, and the clock otherwise, as before.
- `scripts/install` writes the install marker the plugin's launcher reads,
  for `prod`, after the supervisor step, on the from-source path as well as
  the prebuilt one.

### Known limitations

- The prebuilt binaries are for arm64 only and are ad-hoc signed, not
  notarised: a tarball downloaded by a browser must have its quarantine
  attribute removed (`xattr -d com.apple.quarantine <tarball>`) before it
  is unpacked.
- A prebuilt `threads` keeps the bundle's ad-hoc signature; the installer
  does not re-sign it with `HARNESS_SIGN_SCRIPT`.
- In a prebuilt install, the account board's "make this the machine's
  login" action (`tui pool`, key `s`) runs no helper by default: the
  `claude-accounts` script it needs ships only in the source tree. The
  action refuses, runs nothing, and says how to name a clone's copy with
  `TUI_POOL_ACCOUNTS_TOOL`.
- `import-corpus` does not carry clusters: workspace-v6 has no cluster type.
- Two builds of one commit on one machine, in different build directories
  and under different time zones, give the same tarball byte for byte. Builds
  on two machines, or with another Rust toolchain or another macOS `tar` or
  `gzip`, have not been compared.

## [0.1.14] — 2026-11-18

Region locks: a ripple locks by region, never the whole soup. While it
thinks it holds the few regions its dump clearly points at (often none),
and while it writes it holds the regions its records go into, so you or a
session can write anywhere else in the same soup at the same time.

### Added

- **A ripple holds the regions its dump clearly points at.** Before its
  model call, a run scores each region against the dump's text: the words
  of its handle and of the inside of its boundary (the text before
  `Outside:`), less stop words and plural, `-ing` and `-ed` endings, each
  shared word counting by how rare it is among the regions (1 for a word
  only that region uses, little for one every region uses). It declares a
  region at a score of 2 or more and at least half the best region's
  score, at most four and never more than half the regions. No model is
  asked. On the plugin's starter regions file an ordinary note declares
  one region or none, and text made of every region's words declares two
  of the four. It holds each declared region (`write`) and each of their
  ancestors (`link`) from then until its records land, when the `mcp`
  service lets them go in the same act, or until the run ends any other
  way, when the run lets them go before it writes its outcome (or, when
  the `mcp` service cannot be reached then, at the service's next start or
  run, expiring meanwhile). A park, a cap, a failure or `off` ends the run,
  and a run already at a cap takes no lock. A run by hand whose backend is
  busy waits up to twice 5 seconds, holding its regions and renewing them.
  The model is still shown every region.
- **The put takes the regions it writes into.** In the one act on the
  `mcp` service's writer that writes a ripple's records, the regions they
  go into (and `link` on their ancestors) are taken for the run first,
  declared or not; another holder's row on one refuses the put
  `region_locked`, nothing written. No record lands in a region the run
  did not hold at that moment.
- **An hourly hold budget per region.** Once ripples have held a region
  for 900 seconds within the last hour (`CAULDRON_RIPPLES_REGION_HOLD_S`,
  1 to 3600, counted from the ripples journal), a run that declares it is
  deferred `hold_budget` before it takes any lock or makes any call, with
  when to try again, so a dump's text cannot keep busy regions held hour
  after hour.
- **A lock is a row in the soup.** `<soup>/region-locks.json` (mode 0600)
  holds one row per held region: the region, `write` or `link`, the holder
  (`{kind: "ripple", run, dump}`), what it is for, since when and until
  when. It is written only by the holder of the soup's writer lock, as a
  whole file replaced atomically, and read by anyone who may read the soup.
  A row has a time to live of 120 seconds (`CAULDRON_RIPPLES_LOCK_TTL_S`,
  1 to 900), renewed every third of that while the model call is open; a
  row past its `until` holds nothing, and renewing never revives one.
- **No lock is held more than 900 seconds ahead of now.** A row whose
  `until` says more (a hand edit, or a lock taken while the clock was set
  ahead) is held 900 seconds from when it was first read so, logged once,
  and written back that way by the writer; writing such a row is refused.
  A clock set back stretches a lock by at most 900 seconds; one set
  forward expires locks early, which the revision check at the put
  catches.
- **Every other writer into a held region is refused at once, by name.**
  `soup_put`, `soup_link`, the owner's approve and reject, `cauldron soup
  import` (direct or forwarded) and another ripple are refused
  `region_locked`, with nothing written: the message names the region, the
  run and the dump holding it, since when and until when, and says to try
  again in about N seconds or, if the records belong in another region, to
  write them there; `detail` holds the rows and `retry_after_secs`.
  Nothing waits. A call is refused whole when any of its records touches a
  held region (`soup_put` with `partial: true`, and one approve or reject
  over many records, too). A held region covers the regions below it
  (`narrows`), and a region above a held one takes new records but not a
  change of its own header or parent. Writes into every other region land
  as before.
- **A region head revision.** Every region has a head revision, derived
  from the store each time it is read and never stored: its boundary and
  identity (the region node's own revision and state, its parents, its
  children and what supersedes it). No member enters it: the model is
  shown each region's id, handle and boundary and never a member, so a
  member added, moved, superseded or closed is not staleness. The ripple
  seam's `RREAD` and the new `RLOCK` answer every region's head revision.
- **The revision check, and one re-plan that is never a no-op.** A
  ripple's put carries the head revisions it read; when one of the regions
  its records go into, or one it holds, moved since, the put is refused
  `region_changed` and nothing is written. The run then reads the regions
  again (and holds them again). When nothing the model was shown changed,
  it sends the same records again with the fresh revisions and makes no
  call; otherwise it calls the model once more, within the same caps, its
  prompt saying which regions changed and how. A second change leaves the
  dump `received`, deferred `region_changed`, tried again at the next scan.
  A region the model was shown but placed nothing in is not checked.
- **A call another writer voided is not the dump's.** A call whose answer
  was refused `region_changed` or `region_locked` is journaled `voided`:
  it counts against the cauldron's daily caps, and never against the
  dump's own three calls and 200,000 tokens, so other writers' activity
  alone can never use up a dump's lifetime cap.
- **New ripple seam verbs** on the `mcp` service: `RLOCK` (hold regions
  for a run, answered with the regions and their head revisions read at
  that moment), `RRENEW` and `RUNLOCK`.
- **Visible everywhere.** `cauldron ripples status` (running or stopped),
  `cauldron mcp status` and `soup_status` list the held regions;
  `cauldron soup regions` and `soup_regions` mark a held region `locked`,
  and a region above a held one `locked_below`, with the holder, purpose,
  since and until.
- **A damaged lock table names its fix, and `cauldron soup recover` mends
  it.** A `region-locks.json` that cannot be read (truncated, not JSON,
  the wrong shape, another release's format, empty) still refuses every
  write, `rename` and `remove`, never read as holding nothing, and the
  refusal names the command. `cauldron soup recover`, holding the soup's
  writer lock, moves it aside to `region-locks.json.damaged-<UTC time>`
  (the soup then holds no lock) only when no ripple run of the cauldron is
  live in the ripples journal; with one live it refuses, naming it, and
  changes nothing. When it sets the file aside its output says so
  (`region_locks`, with `moved_to`); otherwise its output is unchanged.

### Changed

- **Automatic processing defers instead of failing on a held or changed
  region.** `meta.auto` reads `{state: deferred, reason: region_locked |
  region_changed | hold_budget, ...}`; it is not a failed attempt, and a
  later scan tries again (a `hold_budget` deferral once the budget frees
  up).
- **`cauldron rename` and `cauldron remove` are refused while a region is
  held**, naming the holder and until when, and change nothing.
- **After a crash, a hold goes by itself.** The ripples service lets go of
  every lock its journal shows a run took and never let go, first thing
  when it starts and before each run; the `mcp` service treats a row of a
  run that has already landed as let go, and sweeps it; and every row
  expires.
- **The `mcp` service must be upgraded with the command line.** A 0.1.14
  ripples service will not write through a 0.1.13 `mcp` service: a run
  against one fails `mcp_too_old`, with nothing sent or written, naming
  the upgrade (`scripts/install --replace-running`).
- **One run at a time, still.** The ripples service runs one dump at a time,
  as before, and runs by hand and automatic runs share that one slot, so
  two ripples never contend for a region today; the locks keep a ripple
  and your sessions apart. Two ripples over one `mcp` door holding
  disjoint regions at once are exercised in the tests only.

### Documentation

- **ROADMAP.md** lists 0.1.13 as shipped and 0.1.14's region locks.
- **The README says which store is which** (issue #4): the terminal
  interface's Cauldron tab reads its seed corpus
  (`~/.cauldron/data/<env>/tui/soup/seed/clusters.json`, corpus schema version 5,
  carried by `cauldron-build` only), not a cauldron's soup, which is the
  store `cauldron soup`, the MCP tools and the ripples read and write; and
  there is no importer for the older workspace format in this release.

### Known limitations

- **An older release ignores region locks.** A 0.1.13 command line or
  `mcp` service reads and writes a store that has lock rows exactly as
  before (the rows are a file it never opens, and no revision is stored),
  so its writes are not held back by a lock; a ripple's head-revision
  check still catches a boundary such a write changed, but not a record
  it added to a held region.
- **Which regions a ripple declares is a scored word match**, not a
  reading of what the dump means, and it has not been measured on real
  dumps. A dump that clears the threshold for no region holds nothing
  during its call, and the regions its records go into are held only for
  the write itself; one that matches broadly holds at most half the
  regions, and at most four, for the length of one run.

## [0.1.13] — 2026-11-17

The review queue: see what a ripple wrote, grouped by dump, and approve or
reject it in bulk, at a terminal.

**Upgrade the `mcp` service with the command line.** This command line
decides records only through a running `mcp` service of this release or
later, and refuses, writing nothing, when the running one is older (the
default `scripts/install` leaves the running service on its old build). Run
`scripts/install --replace-running` from your checkout, or `cauldron --env
<env> build mcp --env-copy` and then `cauldron --env <env> pin mcp <the
label that build printed>` (the supervisor moves the running service to the
new build). An older command line still approves by id through this
release's service.

### Added

- **`cauldron ripples review --env <env> --cauldron <name>`: the review
  queue.** The records a ripple wrote that carry neither decision, grouped
  by the dump they cite and the run that wrote them (read from the writing
  transaction, not from the record), newest group first. Each group names
  the model and backend that wrote it, when, and the exact approve and
  reject commands; each record its id, type, state, handle, a 240-character
  excerpt of its body, its regions and its other edges. `counts` per dump
  and per type, and how many matching records are approved or rejected.
  Filters `--dump`, `--run`, `--type`, `--since`. The answer is at most
  `--max-chars` characters (default 12000, at most 50000; under 2000 is
  refused), as JSON and as `--text`: whole records while they fit, and a
  record longer than the bound cut, listed in `truncated`. It pages by a
  cursor: an answer that stops early says `next_after`, the last record it
  showed, and `--after <id>` lists the records after it, so deciding a page
  before asking for the next skips nothing. `--text` prints it for reading,
  each record on one line with its control and invisible characters
  (escape sequences, bidirectional overrides, zero-width characters) shown
  escaped, as `\u{202E}`; a group cut by the page says how many of its
  records it shows, and that its commands decide them all. It reads only,
  so it works with or without the services. A superseded record that is
  still unreviewed stays in the queue, marked.
- **`cauldron ripples approve | reject`, by dump, by run or by id.**
  `--dump DM-n`, `--run <run>` or record ids; `--unapprove` and
  `--unreject` take a decision back; `reject` takes `--reason` (one line,
  at most 500 characters). Each is one write through the `mcp` service, as
  the owner, on its new `DECIDE` seam verb, audited as `soup_approve`,
  `soup_unapprove`, `soup_reject` or `soup_unreject` with the selector, the
  counts and, for `--run`, the run, never a record's text or the reason. Like `cauldron soup approve`, they run only with their
  input and output on an interactive terminal, and no option skips that.
- **Reject keeps the record.** A rejected record gets `meta.rejected: {at,
  by: "owner", reason}` and nothing is deleted: `soup get` / `soup_get`
  answer it, marked `rejected: true`, with its cite to the dump and the
  decision. `soup nodes`, `soup search` and a region's members (and
  `soup_nodes`, `soup_search`, `soup_region`) leave it out, counted in
  `rejected_hidden`, unless `--include-rejected` / `include_rejected`; it is
  no longer unreviewed and leaves the queue. The two decisions exclude each
  other: approve (`ripples approve` and `soup approve`) skips a rejected
  record until it is unrejected, and reject skips an approved one until it
  is unapproved. Asking again for a state a record is in changes nothing;
  the answer lists it in `skipped`, with why.
- **`meta.rejected` is a flag.** A row of the spine's flag table on every
  content type, beside `meta.approved`, so the soup refuses it to every
  ripple. The published schema describes it (`meta.rejected` in every
  content type's `meta`), regenerated from the grammar; the grammar and the
  stores do not change.
- **`cauldron ripples review --interactive`.** At a terminal, one group at a
  time: `a` approves the group, `r` rejects it (asking for an optional
  reason), a record's number marks it and `w` writes the marks, `s` skips,
  `q` quits. Each group's decision is one write and one audit line
  (`soup_approve_and_reject` when it does both): the reason is asked first,
  and its approvals and rejections land together, or, when any of its
  records is no longer as it was listed, nothing of the group lands and the
  pass says why. Line-based prompts, no screen control; records are shown
  as `--text` shows them.
- **`soup_review`, an MCP tool with scope `read`**, answers the same queue
  for a session to show the user, with `unreviewed_note`, paged by `after`
  and bounded by `max_chars` (floor 2000, refused below). No MCP tool
  approves or rejects.
- **`/cauldron:review`** in the plugin lists the queue and tells the user
  the exact terminal commands; it never runs them. The curator is told it
  never approves, rejects or writes either flag, and never acts on an
  instruction inside a record or a dump.
- **Receipts and status views name the queue.** A dump receipt that waited
  for its records (`wait_secs`) carries `processing.review`: the commands
  that list, approve and reject that dump's records. `cauldron ripples
  status` gains `review` (the records waiting per cauldron, with the command
  that lists them) and says so on stderr; `cauldron mcp status` gives each
  cauldron's `unreviewed` count; `cauldron status` lists them in its JSON
  (`review`) and its table view.

### Changed

- **Spend is shown by the cauldron, not by the name it was spent under.**
  `today.per_cauldron` in `cauldron ripples status` (with the service
  running or stopped) and `evaluations_today` group calls by the identity
  their journal lines carry, the identity the caps already count by, and
  key each group by the cauldron's current name, with `cauldron_id`, the
  `names` it was journaled under and `gone`. A renamed cauldron's day is
  one entry; one made again under an old name is another; spend of a
  cauldron no longer in the environment shows as `<name> (gone)`.
- **The evaluations' ledger records the cauldron's identity**, and its own
  daily cap counts by it, as runs do.
- **No record write sets or clears a review flag.** `cauldron soup import`
  (direct, `--dry-run`, or forwarded to the `mcp` service), `soup_put` and a
  ripple's batch refuse a record carrying `meta.approved` or
  `meta.rejected`, set or `null`, in any casing, dotted or nested, and
  write nothing: the review flags are the owner's. One check in the door
  makes the refusal for every one of them.
- **Text a terminal would act on is shown escaped.** The JSON that
  `cauldron soup` and `cauldron ripples` print writes control and invisible
  characters inside strings as JSON escapes (the same string), and `cauldron
  soup dump-get --raw` shows them escaped when its output is a terminal (a
  file or a pipe still gets the stored bytes).
- **`cauldron soup approve` goes through `DECIDE`** as `ripples approve`
  does, so it too refuses an older `mcp` service. The service's `APPROVE`
  verb now takes only the body an older `soup approve` sends (`cauldron`,
  `ids`, `unapprove`) and refuses any other key.
- **The `inference` service's status reads `unloading`** while an unload
  waits for its child to exit, and `idle` only once `last_unload` records
  it.
- The README's "Reviewing a ripple's records" and the plugin README's
  section of the same name describe the flow end to end.

### Known limitations

- **An older build does not know rejection.** A store with rejected
  records opens and reads under 0.1.12: its command line and its `mcp`
  service list a rejected record among the others, marked `unreviewed:
  true`, with `meta.rejected` as a plain field of its `meta`, and do not
  leave it out of `nodes`, `search` or a region's members. A 0.1.12 `mcp`
  service would approve a rejected record if asked (`soup approve` of
  0.1.12); under this release a record carrying both flags reads as
  rejected.
- **An older `mcp` service still running beside this command line** answers
  this release's decisions with a refusal, and nothing is decided until it
  is upgraded (above). It also does not refuse a review flag in a record a
  session or a ripple sends it directly, and this release's reads hide a
  record carrying `meta.rejected` whoever wrote it.
- **The terminal check guards the commands, not the files**, as for `mcp
  grant` and `ripples on`: a process running as the owner that reads the
  `mcp` service's `approve.secret` can send the same request.
- **Spend lines written before 0.1.12 carry no identity**, so they count
  for the cauldron that has their name now.

## [0.1.12] — 2026-11-17

Automatic processing of dumps, per cauldron, behind a setting that is off
by default, and a secret scrubber on what a ripple sends and writes.
Nothing changes for a cauldron until its owner turns it on, and an upgrade
turns nothing on.

### Added

- **`cauldron ripples on <cauldron> --env <env>`: a dump is processed when
  it lands.** For that cauldron, each dump received after that moment is
  processed by the `ripples` service with no further command, as `cauldron
  ripples run` would process it: the same call, caps, journal and
  `unreviewed` marking. Dumps received before it are processed only by
  hand. Text dumps only, unless `--include-sessions`. `--backend threads`
  (the default) or `--backend local` (the `inference` service on this
  machine; nothing leaves it), and `--model`. It authorises model spend and
  writes, so it requires an interactive terminal, like `cauldron mcp
  grant`: it prints what will be sent where, the caps and the failure
  policy, asks `[y/N]`, and writes the setting only on yes. The setting is
  one owner-only file per cauldron, written whole and renamed into place.
- **The setting belongs to one cauldron, never to its name.** It records
  the cauldron it was turned on for (the environment and the cauldron's
  id, which its MCP grant is bound to too) and is checked before every
  automatic run and every model call of one. A cauldron that is gone, one
  made again under the name (even with the same creation time in its
  `cauldron.toml`), or a setting copied from another cauldron or
  environment makes it void: nothing runs,
  the dump receipt says the dump is not taken, and `status` says why and
  how to clear it. `cauldron remove` turns automatic processing off for
  the cauldron and says so; `cauldron rename` turns it off and prints the
  `on` command for the new name. A cauldron made again under an old name
  starts with a clean queue. A data root moved whole keeps it on, once the
  cauldron has its id.
- **Every cauldron has an id**, 128 random bits that `cauldron create`
  writes into its `cauldron.toml`. A rename keeps it and it moves with the
  cauldron's directory; no command copies it to another cauldron. MCP
  grants are now bound to it, as the automatic setting is, so a cauldron
  made again under an old name, even with the old creation time in its
  `cauldron.toml`, is opened by neither. A cauldron made before ids is
  given its id by its first writer (the `mcp` service before a call
  reaches a granted cauldron, `cauldron mcp grant`, `cauldron ripples on`,
  `cauldron rename`); at that moment only, its grant is carried onto the
  id if it records the cauldron's creation time, and its automatic setting
  if it records its creation time and directory. Anything that does not
  match is refused with the `cauldron mcp grant` or `cauldron ripples on`
  command that makes it again, and `cauldron mcp grants` and `mcp status`
  name such a grant beforehand.
- **`cauldron ripples off <cauldron> --env <env>`**, from anywhere. It
  takes effect before the next model call: a run in progress stops before
  its next call and before it would write its records.
- **How the service finds dumps.** The `mcp` service tells the `ripples`
  service when a dump lands through it; the `ripples` service also looks
  for dumps still `received` when it starts and every minute, which finds
  dumps that landed while it was down and dumps `cauldron soup dump` wrote
  straight to the store. One dump at a time, oldest first, the cauldron
  served least recently first. A dump that lands while a run by hand or an
  evaluation holds the service's one run slot waits, `status` says so, and
  it starts when that run ends. A dump lands at
  most one set of records, whatever is killed when: kill tests stop the
  service during the scan, before a run, during the model call, after the
  records landed, and after the run ended, and land the records with the
  dump's state not yet flipped; each ends with one record set. One
  `ripples` service per environment: a second one refuses to start and
  names the first's pid.
- **Caps and failures, without a retry storm.** The caps are those of a
  run by hand, charged before each call. Over a cap a dump stays
  `received`, its listing shows `auto: {state: deferred, reason: budget}`,
  and it is tried again after midnight UTC. A failed attempt is retried
  after 2 minutes, then 8; after 3 failed attempts, or at once when trying
  again cannot help, the dump is parked `failed` with the reason and is run
  again only by hand. A call that fails at once (a `claude` that exits
  before it answers, as one refusing its command line does) is a failed
  attempt like any other. A call the backend refuses as busy (another
  request holds the local `inference` service, or it is not ready yet;
  threads refusing it as busy or overloaded) is charged nothing and is no
  call against a cap: an automatic attempt ends at once as `auto: {state:
  deferred, reason: busy}`, not a failed attempt, and the next scan tries
  the dump again; a run by hand tries at most 3 times, then ends `deferred`
  `busy`. If the automatic journal cannot be written,
  nothing is run until it can. The daily cap is counted per cauldron, not
  per name: a cauldron made again under an old name starts its day clean,
  and a renamed one keeps what it spent.
- **A call threads parks is a deferral, not a failure.** When threads holds
  a call (every account over its usage threshold, a usage limit, a spent
  credit balance, an expired login, a budget), the dump stays `received`,
  its listing shows `auto: {state: deferred, reason: threads_parked, park:
  <cause>}`, and the attempt is not counted as a failed one. A call threads
  parked before it ran, with no tokens reported, is charged nothing and
  does not count against any cap. After a park, no automatic run on
  threads, of any cauldron in the environment, starts until the wait is
  over: 2 minutes, growing fourfold with each park in a row, at most an
  hour. A parked account costs one withdrawn call per wait, never one per
  dump. When the wait is over, the cauldron served least recently is tried
  first, so one whose calls keep being parked keeps no other waiting.
- **`cauldron ripples status`** shows, per cauldron with a setting: on,
  off or void, since when, the backend, and the queue (waiting, running,
  deferred with the reason, retrying, failed with the reason, session dumps
  not taken, the last ten processed), with the service running or stopped;
  the threads hold (`threads_hold`: since when, the cause, the dump that
  set it, the next attempt) and, on each cauldron it holds, `held`; and,
  from the running service, what a waiting dump waits for (`waits_for`).
- **Journals keep codes, never words.** The run journal, the automatic
  journal and a dump's own record of what automatic processing did with it
  (`auto`, `failure`) hold ids, codes, counts, times and fixed reasons;
  never the dump's text, the model's answer or its notes, or a backend's
  own words, including the end of a failed `claude`'s stderr that threads
  hands back (a test plants a word in a fake `claude`'s stderr and finds it
  in no file under the root). A run by hand shows those words on the terminal (`detail`,
  `notes`), and `cauldron ripples run --debug-keep-answer` keeps one run's
  answers and refusals in one owner-only file whose path it prints.
- **A secret scrubber.** What a ripple sends to a model (always through
  threads; to a local model unless `on` was given `--no-scrub-local`), by
  hand or automatically, and every string of the model's answer before it
  is written, have credential-shaped text replaced by `<redacted:kind>`:
  private-key blocks, JWTs, keys and tokens with well-known prefixes
  (Anthropic, OpenAI, AWS, Google, GitHub, GitLab, Slack, Stripe, npm, PyPI,
  Hugging Face, SendGrid, DigitalOcean, Shopify), passwords and tokens in
  URLs, `Authorization:` and `Cookie:` header values (whole, a token inside
  included), secrets on command lines (the value of a long flag whose name
  holds a credential word as one of its parts, as `--password x`,
  `--admin-password=x` or `--client-secret "x"`, but not `--api-key-id`,
  `--password-file` and the other named look-alikes; `mysql -p…`; a quoted
  `-p'…'` after any tool), values assigned to `SECRET`, `TOKEN`,
  `PASSWORD`, `PWD` and similar names in upper or lower case, quoted (a
  passphrase of any length, a key inside it included), dotted or YAML-ish,
  and long random strings (except a digest in its obvious context: a hash
  of exactly that digest's length after `sha1`, `sha224`, `sha256`,
  `sha384`, `sha512` or `md5`, with or without a hyphen after `sha`,
  followed by `-`, `=` or `=:`, as `sha512-…` and `sha-256=…`, a go.sum
  `h1:` hash, and the body of an image, font, audio or video `data:` URI).
  Matches that overlap are replaced as their union, so no match is dropped
  for overlapping another. The answer's
  records are written with the placeholders in them. The run journal
  counts each kind, never the text. The stored dump is never changed. Its
  time is linear in the text and bounded in proportion to it; if it runs
  out, nothing is sent. Its test corpus misses none of its generated
  samples of each shape and leaves a list of look-alikes untouched, and
  touches none of its generated digests, git commit ids and UUIDs in their
  usual places (100 of 180 such samples were replaced before the digest
  rule); planted beside and inside one another in every pairing (about
  34,000 texts), it leaves no run of 8 characters of any of them; on
  this repository's own docs and code it replaced nothing in about a
  million characters of prose and about one string per million of code. It
  reduces the risk and is not a guarantee.
- **The dump receipt says whether automatic processing takes the dump.**
  `soup_dump`, `soup_dump_session` and `cauldron soup dump` answer
  `automatic` (on or off, and whether it takes this dump). `soup_dump`
  with `wait_secs` and `cauldron soup dump --wait-secs`, at most 120
  seconds, wait for it and answer the dump's state, the run and the record
  ids. The plugin's `/cauldron:dump` and `/cauldron:dump-session` say so.

### Known limitations

- **Tested with stand-ins only.** Automatic runs, kills, retries and caps
  are tested against a scripted backend, and parks, fast failures and token
  counts against a fake threads service, with the real `mcp` door; no
  automatic run has been made against a real model or a real threads.
- **The installer does not start the `ripples` service.** Start it
  yourself; until it runs, nothing is processed, turned on or not.
- **The scrubber misses secrets in other shapes**: a format it does not
  know, a short one not given to a name or a flag it knows, one written in
  words outside an assignment, split across lines, made only of hex
  digits, glued to the word before it, or written with look-alike
  characters. A secret written after a digest label with exactly a
  digest's length is taken for that digest and left.
- **One run at a time per environment.** An automatic run and a run by
  hand share the service's one run slot, so `cauldron ripples run` is
  refused as `busy` (saying automatic processing holds it) while an
  automatic run is in progress; the slot is let go between dumps.
- **An answer received after `off` is discarded**, and its call stays
  charged; the dump stays `received`.
- **Spend lines written before this release carry no cauldron identity**,
  so the daily cap counts them by name; `status` shows a day's spend under
  the name it was spent under.
- **The threads hold is per environment**: a park on one account holds
  automatic runs of every cauldron on threads there, even one whose calls
  another account could take.
- **The terminal check guards the commands, not the files.** Anyone who
  can write the data root can write an automatic setting, as they can
  write `grants.json`.
- **A copy of a whole data root keeps its settings on**: the id travels in
  each `cauldron.toml`, and the directory no longer binds a setting that
  has one.
- **A dump's run is named after the cauldron's name**, so after a rename a
  dump gets a new run, and its own caps count again from nothing.

## [0.1.11] — 2026-11-17

Local inference: a language model on this machine, for a dump ripple and
for comparing it with Claude, with nothing leaving the machine. Off unless
you start its service; nothing is downloaded, and llama.cpp is yours to
install.

### Added

- **An `inference` service: a local model, on demand.** It runs llama.cpp's
  `llama-server` (installed by you, found on `PATH` or named in its
  settings; never bundled) with one registered `.gguf` model file at a
  time, on 127.0.0.1 only. It loads the model when a request first names
  it, unloads it after 15 idle minutes, and refuses a model whose estimated
  memory (file size + KV cache for its context + 2 GiB) is over its budget,
  64 GiB by default, with the arithmetic. Before each load it checks the
  `llama-server` it runs and checks that the model file is the one
  registered. It records crashes and backs off before reloading, never
  offers the model tools, and stops its child before it exits. Off unless
  you start it; the installer does not.
- **`cauldron inference models add <name> <path>`, `list`, `verify`,
  `remove`, `status`, `unload`.** Registering copies nothing and downloads
  nothing. It records the model file's path, its identity (the real path
  with symlinks resolved, device, inode, size, and modification and change
  times) and its sha256. Before every load the identity is compared; when
  any of it differs, the whole file is read again, and it is refused unless
  its sha256 is the one registered. `verify` reads it again on demand, and
  `verify_every_load = true` in the settings does so at every load (about
  two minutes for a 60 GB file). A model file's header is read with every
  declared size bounded, and a header outside those bounds is refused,
  naming the field.
- **`cauldron ripples run <DM-n> --backend local [--model <name>]`.** The
  dump ripple on the local model: the same schema, caps, journal and
  unreviewed marking, its retry on the same model, and nothing leaves the
  machine. Every address it dials (the `inference` service, and the `mcp`
  and threads addresses of any ripple) must be a loopback address, or
  nothing is sent. The journal records the model, the sha256 of its file as
  verified at load, tokens and tokens per second.
- **`cauldron ripples eval --dumps <dir> --backends threads,local --out
  <dir>`.** Backends side by side on fixture dumps (each `.txt` or `.md`
  file is one dump), with nothing written to any soup (a new dry run on the
  `mcp` service's ripple seam judges each answer): per dump and backend,
  schema validity, record count, records quoting the dump, region coverage,
  duplicate handles, tokens and timing, as files a person can read and
  judge. With `threads` it needs `--max-calls <n>`, prints the estimated
  spend, and sends nothing without `--yes`; each threads call is charged
  before it is sent, in the evaluations' own ledger, never against a
  cauldron's caps for runs.

### Known limitations

- **Run against two real models, not yet through a whole ripple.**
  Qwen3.6-35B-A3B (Q8_0) and gpt-oss-120b (MXFP4) ran through the service
  on llama-server build 12110: answers met the batch schema (36 of 36 each
  on invented fixtures), memory stayed under the estimate, a cancel stopped
  generation at once, the idle unload and a stop left no child, and the log
  held no prompt text. A ripple run and the evaluation through the mcp
  service have not run on a real model yet.
- **gpt-oss-120b needs a smaller context under the default budget.** At
  65,536 tokens its estimate is 65.53 GiB and it is refused; registered with
  `--context 32768` it is 63.28 GiB and loads (about 60.5 GiB resident).
- **gpt-oss writes a reasoning section whatever `--reasoning` says.** It is
  not part of the answer, but it counts against the output limit.
- **If the service and the guard process that holds its child are both
  killed,** the `llama-server` child keeps running until the service next
  starts, which stops it. Killing only the service stops the child within
  seconds.
- **llama-server answers `/health` without its key.** It says only
  `{"status":"ok"}`; `/v1/models` and completions need the key (checked
  against build 12110). The key file (owner-only) is readable by any process of the same
  user, as the service's door key is.
- **`llama-server` is found on `PATH` again at every load**, unless
  `llama_server` names it. The check before a load proves its version and
  its flags, not where it came from.
- **The evaluation reads fixture dumps from text files**, not dumps of a
  cauldron by id, and does not measure recall and precision against
  another model's answers, or memory use.
- **A local run never falls back to Claude.** Both of its calls use the
  local model, so a dump the local model cannot answer to the schema fails.
- **A 0.1.10 `cauldron status` does not list a running `inference`
  service**; `cauldron envs` does.

## [0.1.10] — 2026-11-17

The first ripple, run by hand: `cauldron ripples run <DM-n>` turns one dump
into new records in the soup, each citing the dump and marked unreviewed
until its owner approves it at a terminal. Off unless you start its service
and run it; nothing is processed on its own.

### Added

- **`cauldron ripples run <DM-n> --env E --cauldron C [--model M]
  [--force-budget N]`.** It sends one dump to the new `ripples` service,
  which reads the dump and the cauldron's regions through the `mcp` service,
  makes a structured-output call through the `threads` service, and hands
  the answer back to the `mcp` service, which lands it as new records and
  moves the dump from `received` to `processed`. The prompt and the
  answer's JSON schema are two files, `services/ripples/package/dump/v1/`
  `prompt.md` and `batch.schema.json`. What leaves the machine is the dump's
  id, kind and text (a session dump only as its text view: what was typed
  and what the assistant wrote as text), the cauldron's regions (at most
  50), the answer's schema, and Claude Code's own system prompt; no
  existing record. The README's "Ripples, by
  hand" lists what is sent and what is stored where. Sonnet first, one Opus
  retry when the answer misses the schema or the soup refuses it. The
  outcome prints the records and the tokens (input, output, cached, calls,
  models), and failures say what to do. A dump that is removed, or whose
  removal is unfinished, is refused; a dump run twice gets one set of
  records; a run cut short reads as `interrupted` and is safe to run again.
- **The model call is a bare completion.** The call runs in Claude Code
  with no tools of any kind (no built-in tool, each denied by name, no MCP
  server), no settings files, plugins, hooks, skills or CLAUDE.md
  (`--setting-sources ""` and Claude Code's `--safe-mode`),
  permission mode `dontAsk`, one turn, an answer bounded to 16,000 tokens,
  no session transcript, an environment stripped to what Claude Code needs,
  and an empty working directory made for the call and removed after it.
  The dump's text is marked in the prompt as untrusted data, and the answer
  is checked against the schema by the ripple as well as by threads.
- **`threads`: a thread can be a bare completion.** A new thread setting,
  `completion: {max_output_tokens}`, absent unless a caller names it, makes
  every turn of that thread run as above; threads mints no run token and
  writes no MCP config for it, and sends the prompt as it was given. It is
  set only at creation and cannot be loosened by a later update. A ripple
  reads the thread back before sending anything and refuses a threads that
  did not keep it (`threads_unsupported`), so the `ripples` service needs
  the `threads` service of this release. A completion whose `claude` ends
  without an answer fails at once (`turn_failed`, with the last lines of its
  error output, the home and working directories shown as `~` and `<cwd>`)
  and is not delivered again; a usage limit, a spent credit balance or an
  expired login still parks it. A ripple ends its call the same way if
  threads schedules a re-delivery.
- **Caps that hold across runs.** Counted from the run journal before every
  call: per dump, across every run of it, at most 3 model calls and 200,000
  input tokens (`--force-budget N` lets one run spend N more); per cauldron
  per UTC day, at most 1,500,000 input tokens or 30 dumps. A call is
  charged when it begins, before anything is sent, as one call and its
  estimate, with the check and the charge made in one step under the
  journal's lock (two runs cannot both take the last call), and settled to
  the larger of the input tokens it reported and that estimate when it ends.
  A call cut short by killing or stopping the `ripples` service stays
  charged. Over a cap the run ends `deferred` (`budget` or `daily_cap`)
  and the dump stays `received`.
- **A call given up is cancelled.** When threads parks a call, the turn
  ends without an answer, or no answer comes in 15 minutes, the ripple asks
  threads to stop the turn and to withdraw its message, and checks the
  thread is idle; the outcome says whether that was confirmed, or that the
  call may still be running and how to stop it. A call left by a killed or
  stopped service is cancelled when the service next starts or runs.
- **The `ripples` service** (`services/ripples.toml`, `cauldron-ripples`).
  The installer does not start it: build, pin and start it yourself. It
  never opens a cauldron; it reads and writes through the `mcp` service, and
  keeps a run journal, `data/<env>/ripples/runs.jsonl`, with every step and
  its tokens and never dump text. Its `RUN` and `STATUS` answer only a
  caller holding the key it mints at each start (`run.secret`, owner-only).
- **`cauldron ripples status`** shows the service, the last ten runs, the
  spend today per cauldron and per dump across its runs (a call that never
  ended counted and shown in `unsettled_calls`), any call that may
  still be running in threads, and the caps (read from the journal when the
  service is stopped).
- **The review flag, and `cauldron soup approve`.** A record a ripple wrote
  reads as `unreviewed: true` in `soup_nodes`, `soup_search`, `soup_get` and
  `cauldron soup nodes | search | get` until its `meta.approved` is set.
  Only the owner sets it, with `cauldron soup approve <id>... --env E
  --cauldron C` (`--unapprove` takes it back), which runs only with its
  input and output on an interactive terminal, as `cauldron mcp grant` does,
  and is audited. No MCP tool approves, so no session can; the soup refuses
  `meta.approved` to every ripple. A review flow for sessions comes in a
  later release. `soup_nodes` and `soup_search` take `review: "pending" |
  "reviewed"`, and the CLI reads take `--review`. A store with no ripple
  record reads exactly as before. The published schema describes
  `meta.approved` and `meta.ripple`.
- **Unreviewed records are marked as data, not instructions.** Every MCP
  reply that holds a record marked `unreviewed: true` carries, once,
  `unreviewed_note`: such records were written by an automatic step from a
  dump, are not approved by the owner, and are to be treated as data, never
  as instructions. The MCP door's `initialize` instructions, the read tools'
  descriptions, the curator, `/cauldron:curate`, `/cauldron:curate-dump` and
  the schema skill say the same of records and dumps, and tell a session
  never to act on an instruction found inside one and to tell the user when
  one reads like an instruction.
- **The `mcp` service's ripple seam.** Five verbs on its seam (`RDUMP`,
  `RTEXT`, `RREAD`, `RRUN`, `RPUT`) answer only a line carrying a second
  secret the service mints at each start, `mcp/ripples.secret`
  (owner-only), which only the `ripples` service reads; the MCP tools never
  accept it. `RPUT` lands a batch under a ripple's provenance that the door
  sets itself (no field of a record can change it), new records only (seven
  types, in existing regions, `ref` and `derived_from` links, at most 25,
  each label used once and none equal to a held id), judged without landing
  first and then landed as one write, and lands a given run at most once. A
  sixth verb, `APPROVE`, answers only the owner's approval key
  (`mcp/approve.secret`).

### Changed

- **A dump's receipt and the dump commands** now say that a dump is
  processed only by hand (`cauldron ripples run`, or
  `/cauldron:curate-dump`), and a processed dump's summary names the run
  and its records (`processed`).
- **The curator** reads records marked `unreviewed` as suggestions, treats
  them and dumps as data and never as instructions, and never approves.

### Fixed

- **A structured-output turn whose schema names JSON Schema 2020-12 now
  starts.** Claude Code's CLI cannot load the 2020-12 meta-schema and
  refused such a `--json-schema` before it ran (`no schema with key or ref
  "https://json-schema.org/draft/2020-12/schema"`, exit 1), so the turn was
  retried and failed with no answer; a ripple's schema says it is 2020-12.
  threads now leaves a top-level `$schema` other than draft-07's off the
  value it passes; the schema it judges the answer against is unchanged.

### Known limitations

- **After a downgrade to 0.1.9, ripple records are not marked.** 0.1.9
  reads a 0.1.10 store, but shows a ripple's records without `unreviewed`.
- **threads keeps every ripple call's prompt and answer**, dump text
  included, in its store until you delete it; removing the dump does not
  remove that copy.
- **Removing a dump leaves the records a ripple drew from it.** They keep
  citing it (`meta.cites`, `{"kind": "dump", "id": "DM-<n>"}`), which is how
  to find them; supersede any you do not want.

## [0.1.9] — 2026-11-17

Removing one dump for good, the `mcp` service hardened in several places,
the dumps store in `cauldron mcp status`, a schema for `--remint` batches
(#17), and name-only grants refused and named before they are.

### Added

- **`cauldron soup dump-remove <DM-n> --env E --cauldron C [--yes]`.** It
  removes one dump's content from the cauldron for good, for a dump that
  holds something it must not (a secret in a transcript). Without `--yes`
  it is a dry run that removes nothing, says what it would do and exits 2
  (it takes the store's writer lock like the removal, so it too is refused
  while the `mcp` service holds the store). With it,
  the removal runs in two phases. First one commit makes the dump a
  tombstone in state `removing`: its id, kind, size and when it was
  received and removed are kept, its title, source, cites, refs and sha256
  are dropped, and from then on no read answers its content. Then its blob
  is deleted unless another dump holds the same bytes (named in
  `same_bytes_in`), its text, title, source note and sha256 are erased from
  the dumps store's record of its writes, the store's databases are
  checkpointed and vacuumed, so none of it is left in the cauldron's
  directory, and only then a second commit records `removed`. The tests
  plant text and scan every file for it afterwards, including after a
  removal stopped after each of its steps.
  - **A removal cut short** (a kill, a crash, a reader holding the
    database) leaves the dump `removing`, never `removed`. `soup dumps`,
    `soup dump-get` and `soup_dump_get` then say `erasure: unfinished` and
    give the command that finishes it (`finish_with`), and `--raw` is
    refused with that command. Running the removal again finishes it, and so
    does the next open of the dumps store for writing, by the CLI or the
    `mcp` service.
  - Reading a removed dump answers its tombstone (`soup dump-get`,
    `soup_dump_get`), `--raw` is refused, and a soup record that cites it
    still finds it. The first removal re-pins the store's grammar from
    `dump@1` to `dump@2`, which adds only the states `removing` and
    `removed`.
  - It is a CLI verb only, never an MCP tool. It is refused while the `mcp`
    service holds the cauldron's dumps store and is not sent to it, because
    it needs the store's databases to itself: revoke the grant or stop the
    service first. `--env` and `--cauldron` must be given as flags. Copies
    made elsewhere (a backup, a cauldron put away with `cauldron remove`)
    are not touched. This replaces the 0.1.7 advice to remove a cauldron's
    whole dumps store by hand.
  - **With 0.1.8.** A store with a removed dump still opens under 0.1.8,
    which lists the tombstone with its state (`removed` or `removing`) and
    no size, shows it as a text dump whose text is `removed` (`--raw`
    included), and refuses `soup dumps --state removed`. 0.1.8 still takes
    new dumps into that store, but it does not finish a removal left
    `removing`: that waits for 0.1.9 to open the store for writing, or for
    the removal to be run again.
- **`cauldron mcp status` shows the dumps store.** Each cauldron has a
  `dumps` entry: whether the service holds its dumps store, and its dumps
  counted by state. `open_dumps` lists the stores the service holds, with
  when it opened each. A service older than this release cannot say which
  stores it holds, and `held_by_service` is then null.
- **A schema for `--remint` batches (#17).** `remint.<type>.schema.json`
  beside each `node.<type>.schema.json`: the same schema, except that `id`
  is a batch label (a non-blank string without `@` or `#`) and an edge's
  `to` may name a label of the batch. The import still judges what no
  schema can see (`label-shadows-existing`, `unknown-target`). The plugin
  ships the new files with its copy of the schema.
- **`cauldron-mcp --build-stamp`** prints the commit the binary was built
  from, as the other services do, so an `artifact` declaration of the `mcp`
  service can be started.

### Changed

- **A grant that names its cauldron only by name is refused.** Such a grant
  was written only by pre-release builds of 0.1.6, before a grant recorded
  the cauldron it was made for. It is now refused as `not_granted`, naming
  the `cauldron mcp grant` command that makes it again with its scopes and
  projects. It is not bound on first use, because it cannot tell the
  cauldron it was made for from one made since under the same name. A
  cauldron that has no identity of its own (no `created_at`) keeps its
  grant honoured by name, since there is nothing to bind to. Such a grant
  is also named before its first call is refused: `cauldron mcp status`
  (`grant_note`, `regrant`), `cauldron mcp grants` (`warnings`) and the
  closing lines of `scripts/install` on an upgrade each say so, with the
  command that makes it again.
- **`soup_dump_get` is audited.** Each call adds a line to `calls.jsonl`
  with which dump, the offset and how many bytes were read; never the text.
- **`calls.jsonl` is rotated.** At 8 MiB it is moved to `calls.jsonl.1`,
  replacing an older one.

### Fixed

- **A client that stops reading no longer holds a connection.** The door
  closes a connection whose answer has made no progress for 10 seconds, and
  says so in its log. A call is applied before its answer is written, so a
  write on a connection closed this way has landed whole or not at all.
- **A JSON-RPC batch gets an answer.** The bridge answers a line that is an
  array (or anything but an object) itself, with `-32600`, instead of
  relaying it and writing nothing.
- **A slow open no longer stalls other calls.** Opening a cauldron for
  writing, or its dumps store, no longer holds the list of open ones, so
  calls to a cauldron already open, the grant poll and `mcp status` do not
  wait on it.
- **`/cauldron:dump-session` works if `${CLAUDE_SESSION_ID}` is not
  substituted.** A `session_id` that is still a `${...}` placeholder is
  taken as absent, so the bridge uses the session id it was started with
  (`$CLAUDE_CODE_SESSION_ID`).
- **Docs.** The README's line that only you can read a cauldron's data now
  says that a cauldron made by 0.1.7 or earlier is set so by its next
  write. The plugin README says what `calls.jsonl` records and when it is
  rotated.

## [0.1.8] — 2026-11-16

The path from install to a working session (including an upgrade that
moves the `mcp` service to the new build), curating a dump into records,
`cauldron rename` and `cauldron remove`, a cauldron's data private to its
owner, and small import, read, dump and documentation fixes, most of them
filed as issues.

### Added

- **Curating a dump, without the ripple engine.** `soup_dump_get` takes
  `view: "text"` for a session-transcript dump: it answers only what the
  person typed and what the assistant wrote as text, as turns each with
  `at` (the byte where its line starts in the transcript), `role` and
  `text`. Everything else is left out and counted in `elided` by kind: tool
  calls and their results, thinking, `!` shell commands and their output
  (`shell`), local commands' output (`local_command`), slash-command
  wrappers (`commands`), system reminders (the typed words around one are
  kept), other text Claude Code writes itself such as a compaction summary
  (`generated`), `isMeta` lines, attachments, images and other lines. Its
  pages are bounded as a whole by `max_chars` (at least 1,000), end between
  turns, and read the same from the same cursor; a turn longer than a page
  is cut, with `chars` its whole length, and `next_turn_offset` (passed
  back as `turn_offset`) goes on in it. The raw view still answers the
  whole transcript, verbatim, as the stored blob holds it; the text view
  needs `read` and `dump`, like it. From a shell: `cauldron soup
  dump-get <id> --view text [--turn-offset N]`. The curator subagent has
  `soup_dumps` and `soup_dump_get`, and a new command, `/cauldron:curate-dump
  <DM-n> [cauldron]`, has it read the dump in pages, judge its records with
  a dry run, land them each citing the dump (`meta.cites: [{"kind": "dump",
  "id": "DM-n"}]`) and report their ids. The dump stays `received`.
- **A version skew is visible.** `cauldron mcp status` gives the running
  `mcp` service's `version` beside the `cauldron` command's, and when they
  differ says so (`version_skew`, and on stderr) with the two commands that
  upgrade it (`upgrade`). The stdio bridge does the same for a session: the
  service's instructions carry the note, and when an older service refuses
  a tool or an argument it does not know (`takes no argument`, `unknown
  tool`), the refusal names both versions and how to upgrade. A forwarded
  `cauldron soup import` or `soup dump` refused that way says it too.
- **An example regions file.** A new cauldron's soup starts empty, and the
  curator places nothing without a region. The plugin ships
  `regions.example.json`, four regions to edit and import with `cauldron
  soup import --remint`; a test lands it in a fresh cauldron.
- **`cauldron rename` and `cauldron remove`.** Both verbs were written but
  never dispatched; they now are, and they are listed in the usage text.
  Both are refused while the environment's `mcp` service holds the cauldron
  (the soup's writer lock, or its dumps store's, names the service): the
  refusal gives the service's pid and `cauldron mcp revoke <name> --env
  <env>`, or stopping the service. Any other holder is refused as another
  writer, and the earlier refusals stand (a face holding it open, a live
  tranche, a live soup daemon, the legacy cauldron, a name taken). A rename
  carries the cauldron's MCP grant to the new name with its scopes and
  projects, still bound to the same cauldron; a remove drops the grant. The
  dumps store moves with the directory. Both say that a project's
  `.cauldron` marker naming the old name is not updated. `remove` never
  deletes: it moves the directory to `cauldrons/.removed/<name>-<stamp>/`,
  and without `--yes` it is a dry run.
- **Reads hide superseded nodes, and `get` marks them** (#16). A node that
  another node's `supersedes` edge points at is superseded. `soup get` and
  `soup_get` still answer it, adding `superseded_by: [ids]`. `soup nodes`,
  `soup search` and a region's members (`soup region`), and the
  `soup_nodes`, `soup_search` and `soup_region` tools, leave it out by
  default, and each answer says how many it left out in
  `superseded_hidden`. `--include-superseded` (`include_superseded` on the
  tools) lists them too, each with its `superseded_by`. `--limit`,
  `--max-chars` and the tools' `max_chars` budget work as before.

### Changed

- **An upgrade brings up mcp, and moves it to the new build.**
  `scripts/install` on an existing environment with no `mcp` pin (one made
  before mcp shipped) now builds and pins the service, as it does for a new
  environment (`cauldron build mcp --json --env-copy`, then `cauldron pin
  mcp`). Under `--replace-running`, an existing `mcp` pin at an older build
  than the checkout's (every 0.1.6 and 0.1.7 install) is rebuilt and
  re-pinned the same way, and the supervisor moves the running service to
  it (the install itself stops nothing); before, the pin was left on its old
  build, so sessions kept reaching the old service. A pin at the checkout's
  build is left as it is, so a re-run changes nothing. Without
  `--replace-running` an older pin is left as it is, and the install says
  plainly that the mcp service is still the old build and prints the two
  commands that upgrade it. An install stopped after its threads pin and
  before clearing its marker is now resumed as an upgrade, where it was
  refused.
- **The plugin installs from GitHub.** The marketplace moved to
  `.claude-plugin/marketplace.json` at the repository root (source
  `./claude-code/cauldron`), so `/plugin marketplace add
  broomstick-ai/cauldron` finds it; a checkout is added by its root
  directory. There is one marketplace; the copy under `claude-code/` is gone.
- **The installer says where `cauldron` must be found.** Its closing lines
  (and a `--dry-run`'s) say that the plugin starts its server as the bare
  command `cauldron mcp stdio`, so `cauldron` must be on the PATH Claude
  Code itself sees, which a Dock, Finder or IDE launch may not take from the
  shell startup file, and how to check.
- **`meta.cites` is checked** (#15). Each citation must be an object whose
  `kind` is one of `session`, `file`, `commit`, `url`, `thread` and `dump`
  and which carries that kind's keys (present, not null, and not empty when
  a string). A record with a bad one is refused as `bad-cite`, naming the
  record and the citation, in a dry run and a real import, through
  `cauldron soup import` and `soup_put` alike. Other keys are still allowed.
  The published per-type schemas describe `meta.cites` the same way.
- **A cauldron's data is private to its owner.** The soup's directories
  and files were readable by other local users (0755 and 0644) while the
  dumps store was not. A cauldron's whole directory (for the legacy
  cauldron, its soup directory) is now 0700 for every directory and 0600
  for every file, whatever the umask: `cauldron create` makes it so, and a
  cauldron made before this is tightened by its next writer (`cauldron soup
  import`, `recover` or `dump`, or the `mcp` service when it opens the
  cauldron) or by `cauldron rename`, which also writes its manifest and the
  locks it opens 0600. Reads and dry runs change nothing. Everything that reads a
  cauldron runs as its owner and reads it as before.
- **First-run documentation, for `prod`.** The README and the plugin's
  README now give one numbered path from install to a Claude Code session
  writing into a granted cauldron, in `prod`: the plugin installed from
  GitHub, regions from the example file, the grant, and the upgrade with
  `scripts/install --replace-running`. They say that Claude Code must find
  `cauldron` on its own PATH, how a `.cauldron` marker differs from a
  grant's `--project`, that the `cauldron` command reads `$ENV` while the
  plugin's bridge does not, and how to rename or remove a cauldron. The
  README no longer says the soup is not in this build, or that a cauldron
  is an environment. ROADMAP.md is refreshed.
- **A refused batch prints one shape** (#20). `batch-already-landed` now
  carries `accepted: 0`, `refused` (the batch's size) and `records: []`,
  like every other import. Every refused batch names its cause in
  `refusal`: `records-refused`, `landing-refused` or
  `batch-already-landed`. `soup_put`'s refusal detail is the same document.

### Fixed

- **A refused `--remint` import printed an `id_map`** (#19) for ids it never
  took. The map, and each record's `new_id`, are now printed only when the
  batch landed or a dry run validated.
- **The wrong reason for an unknown edge key** (#18). An edge with a `note`
  and an undeclared key (`weight` on `in_region`) was refused as carrying
  "both `note` and declared fields". The reason now names the key and the
  edge kind. The note-or-fields reason is kept for an edge whose other keys
  are all declared.
- **The schema README misled a first import** (#14). It now says how to
  change a node (a new node of the same type that `supersedes` it, with an
  example), how to add an edge to a node that exists (supersede it, or
  `soup_link` from a session), that regions overlap and a node may sit in
  several (the rule "No dual home along one chain" is now "Not in a region
  and its ancestor"), that an import needs nothing stopped (the face does
  not matter and no soup daemon ships), that the import does not check the
  rules the grammar leaves out, that a record pointing at a refused record
  is refused as `target-refused`, that `get` answers edges sorted by kind
  and target, that a dry run reports `batch-already-landed` too, and two
  knowledge-capture conventions (a decision drawn from a case carries
  `derived_from`; a case recorded as an `idea` is `done`).
- **`--remint` labels fail the published schemas** (#17). The schema
  README now says which checks to skip for a label (the `id` pattern, and
  the `to` pattern of an edge naming a label), or to validate a copy with
  placeholder ids. A schema variant for labels is not in this release.
- **An audit line could be read half written.** The `mcp` service wrote
  each `calls.jsonl` line in several writes, so a reader could see part of
  a line (a test once failed on it). Each line is now appended in one
  write.
- **A dump's page could be longer than `max_chars`.** `soup_dump_get`
  (and `cauldron soup dump-get`) answered a page whose fixed part (the
  cauldron, the dump's summary, the cursor) did not fit a small budget: a
  page over `max_chars` that carried none of the content and did not move
  the cursor on. A page of a dump's content now takes `max_chars` of at
  least 1,000 (its schema says so), refuses a smaller one as `usage`, and is
  never longer than asked; a page whose fixed part alone does not fit (a
  very long title) is refused the same way. Every other read keeps its
  floor of 256.
- **Concurrent dumps could be refused** (since 0.1.7). Two `soup_dump`
  calls started in the same microsecond in the `mcp` service (which also
  takes every `cauldron soup dump` sent to it while it holds the store)
  could mint the same temporary file name for their bytes, and the second
  was refused as `store` ("File exists"). Nothing was written for it and
  nothing else was lost; a retry landed. The name now carries a serial
  unique in the process, and so does the dump's write, so many dumps at
  once, of the same text or not, all land.
- **`state: null` for regions** (#21). `soup nodes`, `soup search` and a
  region's members (and their tools) now list a region or viewer without a
  `state` key, as `get` already did.
- **The test suite under `-p cauldron`.** tui's egress guard admits the mcp
  service's loopback listener (its `hyper` server dependency and its one
  `hyper::` use) by narrow entries instead of failing on it. The four `ctl`
  tests build the face themselves, so they no longer need `-p tui` in the
  same command.

## [0.1.7] — 2026-11-16

Dumps: raw text put into a cauldron for later processing, from Claude Code
or a shell, including a whole Claude Code session's transcript. A dump is
kept in the cauldron's dumps store beside its soup, never in the soup, and
waits there in state `received`. Nothing processes dumps in this release;
processing of dumps comes in a later release. Stores made by 0.1.6 open
unchanged.

### Added

- **The dumps store.** Each cauldron's dumps live in `<cauldron>/dumps/`
  (for the implicit legacy cauldron, `data/<env>/soup/dumps/`), beside its
  soup: a spine of its own with one pool, `dumps`, whose grammar `dump@1`
  is grammar v7's `dump` node type copied (prefix `DM`, states `received`,
  `processed`, `failed`, the same fields) plus an optional `meta` object, and
  a content-addressed blob store, `blobs/<sha256>.<txt|jsonl>`. It is made on
  the first dump; a cauldron nobody has dumped into has none. Each dump is
  one transaction: its bytes are written to a temporary file in the blob
  store; then, holding the store, the grant is read again (by the service),
  the file is synced and named by its sha256, and one write creates
  `DM-<n>` in state `received`. A dump refused or failed at any step leaves
  no bytes: its temporary file is removed, and so is any blob no dump
  names. A writer killed mid-dump can leave a temporary file, or a blob and
  no node; both are removed the next time the store is opened for writing
  (the service's `open-dumps` audit line counts them, under
  `recovered_at_open.swept`). The
  same text dumped twice is two dumps and one blob. The store is private to
  its owner: its directories are made 0700 and its files 0600, whatever the
  umask. A soup record that
  came from a dump can cite it as `meta.cites: [{"kind": "dump", "id":
  "DM-3"}]` (documented in the schema's README; `meta` is open, so nothing
  in the grammar changed).
- **`soup_dump`** (MCP): keep up to 1 MiB of text as a dump, with an optional
  `title`, `source`, `cite` and `refs`. Larger text is refused as
  `too_large`. It answers a receipt: `dump_id`, `state: received`, `kind`,
  `bytes`, `sha256`, `blob_reused`, `tx`, the change `cursor`, and `next`
  (that processing comes later); never the text.
- **`soup_dump_session`** (MCP): keep a Claude Code session's transcript as
  a dump, verbatim, up to 64 MiB. The stdio bridge, which runs as the user
  beside the session, finds the transcript: the session is the call's
  `session_id`, else `$CLAUDE_CODE_SESSION_ID`; it must be a plain id (no
  path separator); and it is looked for only as
  `<config>/projects/*/<id>.jsonl`, `<config>` being `$CLAUDE_CONFIG_DIR`,
  else `~/.claude`. It follows no link there: a project directory or
  transcript that is a link, a transcript with a second name, or one whose
  real path is outside `<config>/projects` is `transcript_link`, and a FIFO,
  device or directory in the transcript's place is `transcript_unreadable`;
  nothing of it is read. No transcript of that id is `session_unknown`; a
  store or file that cannot be read is `transcript_unreadable`, not absent;
  over 64 MiB is `too_large`; each is answered before anything is sent. The
  bridge streams the transcript to the service's new route,
  `POST /mcp/dump-session` (the call on the first line, then the bytes); the
  service judges the call, grant included, before it reads any of the
  transcript, streams it to the blob store without holding it in memory,
  and never reads a path a call names. The receipt adds `session_id`,
  `lines` and `cwd`, and its `sha256` is the transcript file's. A
  transcript still being written is sent as it stood when the bridge looked
  at it, and when it has grown since, up to its last whole line. Called through
  `POST /mcp` instead, the tool is refused as `usage`.
- **`soup_dumps`** (MCP, `read`): the dumps, newest first, as summaries (id,
  state, kind, title, bytes, sha256, when received, source), never their
  text, filtered by `state` and `kind`. **`soup_dump_get`** (MCP, `read` and
  `dump`): one dump's content from a byte `offset`, as much as fits
  `max_chars`, with `next_offset` while there is more. Reading content needs
  both scopes because a dump is raw material: a transcript holds tool output
  and anything pasted into the session, and a grant made to read the
  knowledge graph does not hand it out to a Claude Code session. The scopes
  govern sessions through the MCP service only: `cauldron soup dumps` and
  `cauldron soup dump-get` run as the user and read dumps without a grant,
  like every other CLI read.
- **`cauldron soup dump <file|-> --env <env> --cauldron <name> [--title <t>]
  [--source <s>]`**, `cauldron soup dumps` and `cauldron soup dump-get <id>
  [--offset N] [--max-chars N] [--raw]` (`--raw` prints the content
  verbatim). `soup dump` writes directly when nothing holds the dumps store;
  while the `mcp` service holds it, the dump is sent to the service as one
  `soup_dump` and printed the same way, plus `forwarded_to`, as `soup
  import` does. The reads open the store so that they cannot write.
- **Plugin commands** `/cauldron:dump` (text, or a file's text) and
  `/cauldron:dump-session [title]` (this session's transcript, by
  `${CLAUDE_SESSION_ID}`), and README sections on dumps. A session
  transcript holds every message, every tool call and its output, and
  anything pasted into the session; it is stored verbatim, in the user's own
  cauldron on this machine.
- **A side-effect hook after each dump.** Once a dump's transaction has
  committed and the store has been let go, the service calls its
  `SideEffects` hook; in this release that only writes a log line. Later
  processing attaches there, after the write and never inside it.

### Changed

- **The `dump` scope is used.** A grant's `dump` scope (in the default
  `read,write,dump`) is what `soup_dump` and `soup_dump_session` need. The
  service opens a cauldron's dumps store on its first dump, under the
  store's own writer lock (`mcp <pid>`), and holds it until the grant loses
  `dump` or is revoked, or the service stops; it reads the grant again
  immediately before each dump is written, and every 4 MiB while a
  transcript arrives (a revoke stops the reading). A dump refused that way
  is answered "Nothing was written", and nothing is.
- **The change cursor has a fourth part**, the dumps store's counter:
  `v1.<instance>.<soup counter>.<dumps counter>`. A cursor 0.1.6 answered
  (three parts) is still accepted, read as dumps counter 0. A four-part
  cursor given to a 0.1.6 service is refused there as `cursor_unknown`.
- **`soup_changes` lists dumps** beside the soup's changes:
  `dumps: [{seq, at, op, id, kind, bytes, state}]`, ids and sizes only, cut
  by the same `limit` and `max_chars`, with the cursor moved back to the
  last item kept.
- **`soup_status` counts dumps:** `dumps: {store, total, by_state, held}`.
- **The audit log** gets one line per dump call (and per open and close of a
  dumps store), with the dump's id, its size and a transcript's line count,
  never its text.
- **`cauldron mcp status`** shows, per cauldron, what its dumps store's lock
  says (`dumps_lock_says`).

### Known limitations

- **A dump cannot be deleted.** No verb or tool removes one. To remove a
  cauldron's dumps (all of them, for example after a transcript holding a
  secret was dumped), stop the `mcp` service or revoke the cauldron, then
  remove `<root>/data/<env>/cauldrons/<cauldron>/dumps/` by hand
  (`<root>/data/<env>/soup/dumps/` for the implicit legacy cauldron). A
  blob alone is not enough: a text dump's text is also in the store's
  databases. The plugin's README has the steps.

### Compatibility

- A store made by 0.1.6 opens under 0.1.7 unchanged: nothing in its soup
  changes, and a cauldron has no dumps store until its first dump.
- A cauldron holding dumps works under the 0.1.6 binary: it reads and writes
  only `soup/` and does not look in `dumps/`. Checked with a 0.1.6 build
  (its `soup nodes`, `regions`, `search`, `get`, `import`, `recover`,
  `list` and `mcp status` on a cauldron with a dump), and by reading the
  0.1.6 service, which opens, reads and scans only the soup's directory.

## [0.1.6] — 2026-11-16

The MCP door: a Claude Code session can read, search and write the soups
their owner has opened to it, through a new supervised service. Every
cauldron is closed to it until the owner grants it at a terminal. Stores made
by 0.1.5 open unchanged.

### Added

- **The `mcp` service** (`services/mcp.toml`), one per environment, started
  like any other: `cauldron --env <env> start mcp`. It listens on
  `127.0.0.1` only (port 3654 in `prod`, a port the kernel chooses in every
  other environment, published through its seam) and answers MCP's JSON-RPC
  on `POST /mcp`. A request must carry the door secret, which the service
  mints at every start into `<root>/data/<env>/mcp/door.secret`
  (owner-only); a request with an `Origin` header, or with a `Host` that does
  not name the loopback door, is refused. These three are checked from the
  request's headers before any of its body is read. The door serves at most
  64 connections at once (the next waits to be accepted), reads only a body
  with a `Content-Length`, at most 16 MiB in one request (a larger one is
  refused as `too_large` from its `Content-Length`, unread) and 64 MiB of
  request bodies at once (a body's share is held until its answer is ready,
  and a request that cannot get its share within 10 seconds is answered
  `busy`), closes a connection that does not send a request's
  head within 5 seconds, and answers 408 to a body that does not arrive
  within 10.
- **Grants: every cauldron is closed until its owner opens it.** `cauldron
  mcp grant <cauldron> [--env <env>] [--scopes read,write,dump] [--project
  <dir>]...` runs only with its input and output on an interactive terminal,
  and no option skips that, so an agent cannot open a cauldron for itself.
  `cauldron mcp revoke <cauldron> [--project <dir>]` closes it again (or
  unbinds one project), and `cauldron mcp grants` lists them. The grants are
  kept in `<root>/data/<env>/mcp/grants.json` and checked on every call; no
  MCP tool can grant, revoke or list them. A grant opens the cauldron to the
  Claude Code sessions run by this user on this machine against that
  environment, whichever directory they work in. `--project` only tells a
  session which cauldron a directory means when it does not name one: a
  call's cauldron is its `cauldron` argument, else `$CAULDRON_CAULDRON`, else
  the grant whose project holds the session's working directory. The
  `.cauldron` marker never opens anything to the door. A grant is bound to
  the cauldron it was made for (its `created_at`, to the second), not only
  to its name: a cauldron removed and made again under the same name is
  refused as `cauldron_gone` until the owner grants it again. (A grant
  written by a pre-release build of 0.1.6 has no such binding and opens by
  name until it is granted again.)
- **The tools.** `soup_status` (which cauldron, how it was chosen, its
  grant's scopes, counts, the change cursor, and whether the service holds
  it open); `soup_regions`, `soup_region`, `soup_nodes`, `soup_search` and
  `soup_get`, which answer the same items as the `cauldron soup` reads, each
  reply's payload bounded as a whole by `max_chars` (default 12,000, floor
  256, ceiling 50,000; a value outside them is raised or lowered to the
  nearer) and saying what it left out; `soup_put`, records in the published
  shape judged and landed as one all-or-nothing write, with `dry_run`, `remint`,
  `partial`, `cite` and `allow_duplicate`, and the session cited on every
  record that has `meta` when the bridge knows the session's id; `soup_link`,
  edges between nodes the soup holds, all or nothing; `soup_changes`, what
  landed since a cursor (with no cursor, only the current one; a cursor of
  another cauldron, or of one removed and made again under the same name, is
  refused as `cursor_unknown`, and one ahead of the soup as `cursor_stale`).
  `soup_claim` is listed and always refused as `unsupported` (this schema
  has no clusters); `soup_dump` and `soup_dump_session` are listed and
  refused as `not_available_yet`. A refusal is a tool result with a typed
  code and a message that says what to do.
- **One writer per soup.** The service opens a granted cauldron for writing
  on its first write call, never at start, under the same proofs as an
  import, holds its soup's writer lock until the grant is revoked or loses
  `write` or the service stops, and runs every write on one thread per
  cauldron: concurrent sessions wait their turn and are never refused for
  it, except by the door's own limits above. Opening a soup an interrupted write left unsettled recovers it, and
  `soup_status` and `cauldron mcp status` say what that recovered
  (`recovered_at_open`). A revoke closes the cauldron and frees the lock
  even when no call follows it. The writer re-reads the grant immediately
  before it applies each write, so once a revoke (or the loss of `write`) is
  seen, every write still queued is refused as `not_granted` and nothing
  more lands; a write already being applied when the revoke comes finishes,
  all of it or none of it.
- **An audit line per write call** in `<root>/data/<env>/mcp/calls.jsonl`:
  when, which client and session, which cauldron and tool, the outcome and
  counts. Never record text.
- **`cauldron mcp stdio`**, the bridge Claude Code launches: JSON-RPC, one
  message per line, relayed to the service with the door secret, which it
  never prints. It adds the session's context to every tool call (its
  working directory, `$CLAUDE_CODE_SESSION_ID` when set, and `--cauldron` or
  `$CAULDRON_CAULDRON`) and replaces any context a caller put in the
  arguments. While the service is not running it still answers
  `initialize` and `tools/list`, and refuses every call as `service_down`
  with the command that starts it. Its environment is `--env`, else
  `$CAULDRON_ENV`, else `prod`; unlike the other verbs it does not read
  `$ENV`. When that environment cannot be used it still answers
  `initialize` and `tools/list`, and refuses every call as `no_environment`,
  saying why.
- **`cauldron mcp status`**: the service, the cauldrons it holds open and
  what opening each recovered, and per cauldron its grant and what its
  writer lock says.
- **The plugin's MCP server.** `claude-code/cauldron/.mcp.json` names
  `cauldron mcp stdio`. The curator and `/cauldron:curate` use the MCP
  tools first and the `cauldron` CLI as the fallback when the session has no
  MCP tools or the service is down; they never use the CLI to get round a
  refusal. The curator still states its target before its first write, and
  asks before writing to `prod` when the user did not name it.
- **`scripts/install` pins `mcp`** in an environment it creates, beside
  `threads`: a release build installed as the environment's own new version,
  then the pin. An existing `env.toml` is never edited; one without an `mcp`
  pin is told the two commands that add it. An install stopped between the
  two pins is finished by running it again.

### Changed

- **`cauldron soup import` while the service holds the soup.** A real import
  into a cauldron the `mcp` service holds is sent to the service and lands
  through it: the same judgement, the same JSON plus `forwarded_to`, and the
  same batch-already-landed check across both ways of landing, with
  `--allow-duplicate` and `--remint` carried. The service records the writer
  as `mcp:cli`, not `--agent`, and writes only the `ws` pool. A dry run is
  never sent. A batch larger than the 16 MiB the service reads in one request
  is refused before it is sent, naming the limit and the two ways on (smaller
  files, or revoking the grant or stopping the service and importing
  directly); it is never sent in parts, so an import still lands all of it or
  none of it. An ungranted cauldron, and every cauldron once the service has
  stopped, imports directly as before.
- **`cauldron soup recover` is refused while the service holds the soup,**
  naming it and its pid: the service recovers the soup itself when it opens
  it.
- **The soup logic is a library** (`libs/soup-door`), shared by the CLI and
  the service. A recorded session of every `cauldron soup` verb (imports,
  reads, refusals and help) prints the same bytes as 0.1.5 did, apart from
  the help text, which now describes the two changes above.

## [0.1.5] — 2026-11-16

Small fixes to the soup commands, and a way for a project directory to name
its cauldron. Nothing new runs in the background, and stores made by 0.1.4
open unchanged.

### Added

- **`cauldron link <cauldron> --env <env>`.** Writes a `.cauldron` marker
  file into a project directory, after checking that the environment and the
  cauldron exist. `cauldron soup` run in that directory or below it then
  needs neither `--env` nor `--cauldron`. An explicit flag or variable always
  wins over the marker (a variable that names no usable environment is an
  error, not a gap the marker fills), and an explicit environment that
  differs from the marker's, with no cauldron named, is refused rather than
  guessed. Given both `--env` and `--cauldron`, a verb reads no marker and
  not even the current directory. `cauldron link --show` prints what a
  directory resolves to and where each value came from. Without a marker the
  verbs behave as before, and nothing defaults to `prod`. The plugin's
  curator and `/cauldron:curate` use it when no cauldron is named.
- **The marker is per-machine, and a shared one cannot aim a write.** It
  names an environment and a cauldron on this machine, never a path or a
  pool, so keep it out of version control: add `.cauldron` to `.gitignore`.
  `cauldron link` prints a hint when it writes a marker in a git work tree
  that does not ignore it, and never edits `.gitignore` itself. A marker in
  any parent directory is obeyed, including one that came with a cloned
  repository, so every soup verb that took its target from a marker says so
  on stderr and as `"resolved_from": "marker:<path>"` in its JSON. A write
  (`cauldron soup import` without `--dry-run`, and `recover`) refuses to take
  its target from a marker that git tracks or that another user owns;
  `--env` and `--cauldron` given explicitly still write, and reads are not
  affected. `cauldron link --show` reports that judgement as
  `writes_refused`. A `.cauldron` that is a symbolic link is never followed
  when read, and `cauldron link` never writes through one, even with
  `--replace`. The plugin's curator states a target it took from a marker
  before its first write, and asks before writing to `prod` from one.
- **`cauldron soup import` refuses a batch that already landed.** Re-importing
  the same records (as read, before `--remint`) into the same pool is refused
  as `batch-already-landed`, with when that batch landed and how many records
  it held. This includes an import that `cauldron soup recover` finished.
  Before, with `--remint`, the second import landed every record again under
  new ids. `--allow-duplicate` lands a second copy on purpose. Each import
  records its batch's digest in its own transaction row, so batches landed by
  0.1.4 or earlier are not recognised.

### Fixed

- **`--pool` must be a pool name.** It was joined into a file path unchecked,
  so a path or `..` could open a file outside the soup (read-only). It is
  now refused unless it is `[a-z][a-z0-9-]*` and not `spine`.
- **`cauldron soup help` lists the refusals this release makes.** It said
  `import` and `recover` were refused while a soup daemon runs, but this
  release has no soup daemon. It now names the writer lock, a soup not at
  rest, and the environment's seam table.
- **Five error messages no longer carry long runs of spaces** in the middle of
  a sentence: the tranche timeout check, the newer-spine refusal, two
  `cauldron install` messages and the `--addr` refusal.

## [0.1.4] — 2026-11-16

A cauldron's soup as a knowledge graph you can use from Claude Code. Nothing
new runs in the background: there is still no soup engine and no ripples, no
new service or daemon, and nothing changes for an existing install until you
run one of the new commands. The soup here is a store you write and read
with the `cauldron` command.

### Added

- **The soup's schema is published: `schema/soup/workspace-v6/`.** The soup's
  grammar as JSON (`grammar.json`), one JSON Schema per node type, the edge
  table (`edges.json`), and a README for anyone writing records from outside.
  It is generated from the soup's grammar, and a test fails if the two drift.
- **`cauldron create <cauldron> --env <env>`** makes a cauldron, with its own
  empty soup, under `data/<env>/cauldrons/<cauldron>/`, and `cauldron list
  --env <env>` lists an environment's cauldrons. (`--seed` is not in this
  release and is refused.)
- **`cauldron soup import | regions | get | nodes | search | region`.** Land
  records written in the published shape into a cauldron's soup, as one
  all-or-nothing write after each record is checked against the soup's
  grammar (`--dry-run` checks without writing; `--remint` gives the records
  fresh ids). Then read them back: every region, one region with its
  neighbours and members, nodes newest first, a word search over handles and
  bodies, and whole records by id. Every command names its environment, prints
  JSON, and exits non-zero on a refusal; the reads never write. An import that
  is interrupted lands nothing until `cauldron soup recover` settles the soup:
  recover either finishes that import (it says `landed`, with the ids; do not
  import that batch again) or rolls it back (it says `rolled-back`; importing
  it again is safe), and the next import can then run.
- **A Claude Code plugin: `claude-code/cauldron/`.** A curator subagent and a
  `/cauldron:curate` command that turn notes and findings into soup records
  and land them with `cauldron soup import`, plus a copy of the schema.
  `claude-code/cauldron/README.md` says how to install it and walks through a
  session.

### Changed

- **`cauldron env create` accepts `--no-engine`.** This release has no engine,
  so it changes nothing, and the command says so.

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

[Unreleased]: https://github.com/broomstick-ai/cauldron/compare/v0.1.19...HEAD
[0.1.19]: https://github.com/broomstick-ai/cauldron/compare/v0.1.18...v0.1.19
[0.1.18]: https://github.com/broomstick-ai/cauldron/compare/v0.1.17...v0.1.18
[0.1.17]: https://github.com/broomstick-ai/cauldron/compare/v0.1.16...v0.1.17
[0.1.16]: https://github.com/broomstick-ai/cauldron/compare/v0.1.15...v0.1.16
[0.1.15]: https://github.com/broomstick-ai/cauldron/compare/v0.1.14...v0.1.15
[0.1.14]: https://github.com/broomstick-ai/cauldron/compare/v0.1.13...v0.1.14
[0.1.13]: https://github.com/broomstick-ai/cauldron/compare/v0.1.12...v0.1.13
[0.1.12]: https://github.com/broomstick-ai/cauldron/compare/v0.1.11...v0.1.12
[0.1.11]: https://github.com/broomstick-ai/cauldron/compare/v0.1.10...v0.1.11
[0.1.10]: https://github.com/broomstick-ai/cauldron/compare/v0.1.9...v0.1.10
[0.1.9]: https://github.com/broomstick-ai/cauldron/compare/v0.1.8...v0.1.9
[0.1.8]: https://github.com/broomstick-ai/cauldron/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/broomstick-ai/cauldron/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/broomstick-ai/cauldron/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/broomstick-ai/cauldron/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/broomstick-ai/cauldron/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/broomstick-ai/cauldron/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/broomstick-ai/cauldron/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/broomstick-ai/cauldron/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/broomstick-ai/cauldron/releases/tag/v0.1.0
