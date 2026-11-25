# cauldron

![cauldron](docs/images/cauldron.png)

![cauldron teaser](docs/images/teaser.gif)

cauldron is a workspace for building software with coding agents. You don't
manage the agents one at a time. You dump what you know, what you want and what
you think of the result into one place. The questions and actions that come back
are sorted into the areas of the project they belong to.

Home: [github.com/broomstick-ai/cauldron](https://github.com/broomstick-ai/cauldron).
Developer: [samhaaf](https://github.com/samhaaf).

This is an early release. Some parts are working today and some are still
on the roadmap, and this README labels each one. [What is in this
release](#what-is-in-this-release) has the details.

## The picture

- **A cauldron is a project's knowledge store.** You keep one for each part
  of your mind: one for a big project, another for work, another for your
  family. They don't mix with each other: each has its own soup, its own
  dumps and its own history, private to you.
- **Cauldrons live in an environment, and you use `prod`.** An environment
  is an installed copy of the services (its own supervisor, data and
  versions); the installer makes `prod`, and your cauldrons go there. Other
  environments (`dev`, for example) are for developing cauldron itself. Name
  your cauldrons after what they hold; `prod` names the environment.
- **The soup is a knowledge graph.** Everything a cauldron knows goes into
  one typed graph, with structure, regions and discoverability, so agents
  can find context without having it pasted in again. You can write into it
  and read it today, from a shell and from Claude Code (see [From install to
  a working session](#from-install-to-a-working-session)). From 0.1.10 a
  ripple turns one dump into records when you ask (`cauldron ripples run`),
  and from 0.1.12 on its own, for a cauldron whose owner turned that on
  (`cauldron ripples on`; off by default; see [Who may act for the
  owner](#who-may-act-for-the-owner)).
- **The surface is your view into the cauldron.** You dump ideas, feedback
  and half-formed thoughts into it. You collect text, pastes and references in the
  **bucket** and **dump** it onto the surface. Every dump is recorded in the
  **Ledger**, the cauldron's history of what came in and what ran.
- **Clusters are user-defined regions of the soup.** You name a cluster and
  describe in your own words what belongs in it; that description is its
  boundary. Each cluster gathers its own questions, actions, directives,
  constraints, objectives and milestones. Clusters can be nested: a cluster
  can sit inside another.
- **Ripples are short-lived agents that ripple out from your brain dumps,
  curate the soup and complete your tasks.** (0.1.10 has the first one, run
  by hand on one dump at a time; see [Ripples, by hand](#ripples-by-hand).
  From 0.1.12 it can also run on its own, for a cauldron whose owner turned
  that on; see [Ripples on their own](#ripples-on-their-own). The other
  ripples come later; see [ROADMAP.md](ROADMAP.md).)

## Why

Most of the time we no longer read the code our agents write. What we need
from the tooling has changed:

- **long-term consistency**, so that decisions made weeks ago still hold;
- **efficient context retrieval**, so that an agent starts with what it needs
  and nothing else;
- **knowledge organisation**, so that what you know is findable later;
- **task execution**, so that work moves without you supervising each step;
- **one interface**, where every question an agent has and every action a
  human must take (such as "set up this API key") rolls up into its cluster,
  instead of stalling in a chat window.

The goal is the simplest, strongest user experience for vibe coding. It runs
on brain dumps. You can always look underneath to see what is going on, but
the focus is design and intent, not the agents' output.

- **Questions wait at the cluster.** When an agent has a question for you, it
  waits in the cluster it belongs to. When you answer it, the tasks downstream
  of it resume.
- **Human actions wait there too.** When an agent needs a person for
  something ("go set up this API key"), an action waits at that cluster. Do
  it, and the tasks downstream of it resume.
- **The Ledger shows what happened beneath the surface:** every dump, and the
  chain of actions that resulted from it.

In this release, questions and actions already appear at their cluster, and
the Ledger is a time-ordered list of dumps, newest first, each expanding into
its phases and the work under it. Agents raising questions and actions, and
tasks resuming when they are answered, arrive with ripples, coming in 0.2.0
(see [ROADMAP.md](ROADMAP.md)).

## What holds it up

cauldron is built on **threads**, an engine (a daemon) that owns every agent
conversation and load-balances those conversations across a pool of Claude
Code accounts, so you make full use of your weekly limits:

- **Placement on every turn.** Each turn runs on an account that is under its
  limits. The limits are re-estimated continuously from observed usage, so
  the pool's weekly limits get used across all accounts instead of one
  account running dry while another sits idle. A turn that cannot be placed
  waits. It is never dropped.
- **Budgets.** A budget is a named meter that any set of threads can share. It
  covers the session and weekly limits as percentages, plus a dollar amount,
  and it applies to the accounts you name for it. One budget can cover one
  project, one account or the whole pool. That lets some work run continuously
  on a small slice while higher-priority work gets first call.

In this release, budgets are set through the threads daemon's API. Setting a
budget per cauldron from the UI is on the roadmap.

## Separate cauldrons for separate parts of your life

Keep one big soup per project, separate from the one for your work and the one
for your family. They don't mix: each cauldron's data is its own, under
`~/.cauldron/data/prod/cauldrons/<name>/`, and only you can read it (its
directories are mode 0700 and its files 0600; a cauldron made by 0.1.7 or
earlier is set so by its next write).

## What is in this release

The release is the workspace version (`version` in the top-level
`Cargo.toml`, the same in the plugin's `plugin.json`), and
[CHANGELOG.md](CHANGELOG.md) lists every change, newest first. On top of the V1 interface (two tab bars,
a first-run screen, the Ledger, the account pool, secret human actions and
`cauldron ctl`, from 0.1.2 and 0.1.3), the releases since made a cauldron's
soup a knowledge graph you use from Claude Code, and then let a ripple turn
your dumps into records:

- 0.1.4 and 0.1.5: the published soup schema, `cauldron create` and `list`,
  the `cauldron soup` verbs, `cauldron link`, and the Claude Code plugin with
  its curator.
- 0.1.6: the MCP door, a service through which Claude Code sessions read,
  search and write the cauldrons you grant to them.
- 0.1.7: dumps, raw text or a whole session transcript kept in a cauldron.
- 0.1.8: curating a dump into records (`/cauldron:curate-dump`), `cauldron
  rename` and `remove`, the plugin installed from GitHub, an upgrade that
  brings up the MCP service or moves it to the new build, a cauldron's data
  private to you, and fixes filed as issues.
- 0.1.9: removing one dump for good (`cauldron soup dump-remove`), and the
  MCP service hardened.
- 0.1.10: the first ripple, run by hand: `cauldron ripples run` turns one
  dump into unreviewed records ([Ripples, by hand](#ripples-by-hand)).
- 0.1.11: local inference, a model on this machine for the dump ripple,
  with nothing leaving it.
- 0.1.12: automatic ripples, turned on per cauldron by its owner and off by
  default ([Ripples on their own](#ripples-on-their-own)).
- 0.1.13: the review queue: see what a ripple wrote, grouped by dump, and
  approve or reject it in bulk at a terminal ([Reviewing a ripple's
  records](#reviewing-a-ripples-records)).
- 0.1.14: region locks: a ripple holds the few regions its dump clearly
  points at while it thinks, and the regions its records go into while it
  writes them, so you and your sessions write everywhere else meanwhile
  ([Region locks](#region-locks)).
- 0.1.15: prebuilt binaries for Apple silicon, installed with no Rust
  toolchain ([Install](#install)); a plugin MCP launcher that finds
  `cauldron` without the `PATH` Claude Code was started with; and
  `cauldron soup import-corpus`, which brings records in the older corpus
  format into a cauldron's soup ([Records in the older corpus
  format](#records-in-the-older-corpus-format)).
- 0.1.16: clusters claim regions: a cluster, an area of the surface with a
  name and a boundary, claims the regions of a cauldron's soup it covers
  (`soup_claim`, `cauldron soup claim`); `import-corpus` brings an older
  corpus's clusters in with their claims; and `soup_changes` can wait for
  the next change ([Clusters and claims](#clusters-and-claims)).
- 0.1.17: every tool answers when called as the consumer's interface
  table writes it (`soup_link {from, kind, to}`); an MCP server
  for one cauldron, pinned with `cauldron mcp stdio --cauldron` and written
  out by `cauldron mcp config`; a wrong edge or citation fixed on the record
  itself (`soup_unlink`, `soup_cite`, and `cauldron soup link | unlink |
  cite`); who may act for the owner: granting, approving and turning
  automatic ripples on stay the owner's acts at a terminal by default, and
  the owner can let agents do them (`cauldron owner-acts`), each act
  recorded with who did it ([Who may act for the
  owner](#who-may-act-for-the-owner)); local inference on a prebuilt
  install: the bundle carries the `inference` and `ripples` services,
  `cauldron inference models pull` fetches one of the two measured models
  when you ask, and `cauldron inference status` says what is still missing
  ([Local inference](#local-inference)); and two fixes, to reads of a quiet
  threads store and to the plugin launcher in a subdirectory.
- 0.1.18: installing and upgrading from a prebuilt bundle (issues #30 to
  #35): `install --replace-running` moves the threads daemon to the new
  build as it does the MCP service; account login runs on macOS's own bash
  and says where it stalled; an account is named by its email address; and
  text can be selected and copied on every surface of the terminal
  interface ([Known limitations](#known-limitations)).

cauldron has three parts:

- **`cauldron`**, a command-line supervisor. It keeps each environment's
  services running under launchd (one supervisor per environment) and opens
  the terminal UI (`cauldron`, or `cauldron tui`).
- **`threads`**, the daemon described above.
- **`tui`**, the terminal UI. `cauldron` opens it.

All three are written in Rust. The repository also carries shell and Python
helper scripts, and an older Python client for the threads daemon. cauldron
runs on **macOS only** for now, and it is licensed under **Apache-2.0**.

**Working now:** environments; two tab bars, a global Config bar (TUI,
Devices, Providers, Accounts) over the environment bar (Cauldron, Threads,
Status, Permissions, Settings); a first-run screen that makes your first
cauldron; the cluster sidebar, with cluster creation from a boundary you
write; cluster tabs for questions, actions, tasks, nodes, status and details;
the bucket, and dumps, each of which is saved; the Threads tab, which shows
each environment's threads pinned or recent, read live from its threads
daemon; the Ledger, a time-ordered event flow of the environment's own dumps;
Status → Tokens, the real account pool and its usage pressure; Config →
Accounts, the threads daemon's accounts, each named by its email address (from 0.1.18 shown in full), with login from
the face and each account switched on or off per cauldron; secret human
actions, typed into a masked input and routed to an env file or the macOS
keychain; the threads face, with persistent Claude Code conversations on the
account pool; `cauldron ctl`, which sends a dump, prints one frame of the
face or reads one soup node from the command line; the `cauldron-build`
demo environment (see below); and a cauldron's soup and dumps from the shell
and from Claude Code (see [From install to a working
session](#from-install-to-a-working-session)).

**Not yet:** the rest of the ripples. In the design, a dump sets off
**ripples**: agents that read it, update the soup, and turn it into tasks,
questions and actions. Today there is one, the dump ripple, which turns a
dump into records: you run it by hand (`cauldron ripples run`, from
0.1.10), or, from 0.1.12, it runs on its own for a cauldron whose owner
turned that on (`cauldron ripples on`; off by default, see [Ripples on
their own](#ripples-on-their-own)); or you curate a dump into the soup with
`/cauldron:curate-dump`. Questions and actions appear in the UI, but they
come from the environment's data or the demo data. See
[ROADMAP.md](ROADMAP.md).

## Requirements

- **macOS only.** The supervisor runs on launchd and its binaries are signed
  with `codesign`. Linux and Windows are not supported.
- **For the prebuilt install:** an Apple silicon (arm64) Mac. Nothing else:
  no Rust toolchain, no Xcode tools.
- **To build from source:** a Rust toolchain, installed with
  [rustup](https://rustup.rs) (stable, edition 2021), plus the Xcode Command
  Line Tools (`xcode-select --install`).
- **Claude Code**, installed and logged in (`claude` on your `PATH`). The
  threads daemon runs your agents through it.

## Install

There are two ways in. **Use the prebuilt binaries** if you want to use
cauldron on an Apple silicon Mac; **build from source** if you work on
cauldron itself, or your Mac is an Intel one. Both give you the same
environment, service pins and supervisor, and run the same later steps. The
prebuilt install also keeps the bundle in `share/cauldron/releases/` and its
service and MCP tool declarations in `share/cauldron/` under your cauldron
root, and its `threads` keeps the bundle's ad-hoc signature.

### Prebuilt binaries (Apple silicon, no Rust toolchain)

From 0.1.15, each release on GitHub carries two files: the bundle
`cauldron-<version>-aarch64-apple-darwin.tar.gz` (the `cauldron` command, the
terminal UI, the `threads` daemon, the `mcp` service, from 0.1.17 the
`inference` and `ripples` services, their declarations and the installer)
and `SHA256SUMS`, its checksum. Download both with `curl`,
check the checksum, unpack, and install (the commands are for 0.1.18;
change the version for another release):

```sh
mkdir cauldron-download && cd cauldron-download
curl -fLO https://github.com/broomstick-ai/cauldron/releases/download/v0.1.18/cauldron-0.1.18-aarch64-apple-darwin.tar.gz
curl -fLO https://github.com/broomstick-ai/cauldron/releases/download/v0.1.18/SHA256SUMS
shasum -a 256 -c SHA256SUMS
tar -xzf cauldron-0.1.18-aarch64-apple-darwin.tar.gz
cauldron-0.1.18-aarch64-apple-darwin/install --prebuilt cauldron-0.1.18-aarch64-apple-darwin.tar.gz
```

`shasum -a 256 -c SHA256SUMS` must print `OK` for the tarball. The installer
checks it again itself: `--prebuilt <tarball>` reads the `SHA256SUMS` beside
the tarball, or takes the sum with `--sha256 <sum>`. From a clone of the
repository, `./scripts/install --prebuilt <url> --sha256 <sum>` downloads
the tarball itself (https only; a URL always needs `--sha256`). It refuses a
Mac that is not arm64 first. Before it unpacks anything, it refuses a tarball
whose sum does not match, and one with an entry outside its own directory (an
absolute path, `..`, or a link). It then unpacks the tarball into a scratch
directory, where every file must match the bundle's own checksum list and
every binary must be an arm64 executable carrying a valid signature; a
bundle that fails any of these is not installed. A download is cut off past
256 MiB. The bundle's own `install`, run without `--prebuilt`, says to pass
it the tarball. It needs no cargo, git or compiler. It places the
bundle in `~/.cauldron/share/cauldron/releases/<version>-<commit>/`, installs
`threads` and `mcp` from it as versions of their own (`cauldron build
<service> --artifact <image>`), keeps the `inference` and `ripples` images
there unpinned and unstarted (see [Local inference](#local-inference)), and
then does exactly what the from-source
install does below: makes `prod`, pins the services, puts the supervisor under
launchd, and prints the same closing lines. `--dry-run` checks a local tarball
and prints the plan without installing anything. To upgrade, run the new
release's install the same way with `--replace-running`: it moves both
`threads` and `mcp` to the new release's build (see **Upgrading** below).
`cauldron --version` names the release and the commit it was built from
(`cauldron 0.1.18 (012345678901)`), the commit shortened as every build
label in `cauldron status` shortens it.

**What "ad-hoc signed" means here.** Every binary in the bundle is signed
with `codesign --sign -`, an ad-hoc signature: it carries no developer
identity, and it is not notarised by Apple. An Apple silicon Mac runs no
unsigned code, so the signature is what lets the binaries run at all, and
`codesign --verify` (which the installer runs) shows that their bytes are
the ones that were signed. It does not say who built them; the checksum
you compare with the release's `SHA256SUMS` is what ties the tarball to the
release. That `SHA256SUMS` comes from the same release page as the tarball,
so it protects against a download that was damaged or changed on the way to
you, and is only as trustworthy as the GitHub release itself and HTTPS:
someone who could replace the tarball on the release page could replace
`SHA256SUMS` too, and the installer would accept both. The bundle is built
reproducibly from the public tree (`scripts/build-release-bundle`): two
builds of one commit on one machine give the same tarball byte for byte.

**Gatekeeper and the quarantine attribute.** A file downloaded by a browser
(or another app that marks its downloads) carries the extended attribute
`com.apple.quarantine`, and `tar` copies it onto every file it unpacks.
Gatekeeper checks a quarantined program when it is launched, and it refuses
an ad-hoc-signed, un-notarised one: macOS says it cannot verify the program,
and the command does not run. `curl` does not set the attribute, which is why
the commands above use it. If you downloaded the tarball with a browser,
remove the attribute before unpacking it:

```sh
xattr -d com.apple.quarantine cauldron-0.1.15-aarch64-apple-darwin.tar.gz
```

(If you unpacked it already, `xattr -dr com.apple.quarantine
cauldron-0.1.15-aarch64-apple-darwin` clears the unpacked directory too.) The
installer refuses a quarantined tarball and prints that command. `xattr -l
<file>` lists a file's attributes.

### From source

```sh
git clone https://github.com/broomstick-ai/cauldron
cd cauldron
./scripts/install
```

`./scripts/install` builds `cauldron`, `tui`, the `threads` daemon and the
`mcp` service in release mode, makes the environment `prod`, pins `threads`
and `mcp` in it, and puts the environment's supervisor under launchd for the
current user. It always installs the whole stack, built from the checkout.
It installs
everything under your own home directory (`~/.cauldron`, or `$CAULDRON_HOME` if
you set it). It checks that it is running on macOS before it does anything, and
it prints each step it takes. It does not sign anything with a personal
identity.

Run it from a clean checkout. A first install builds `threads` and names that
build by its git commit, so it stops before building anything if the checkout
has uncommitted or untracked files, and lists them. Commit, move or delete them,
then run it again. If an install stops partway, fix what it reports and run
`./scripts/install` again: it finishes the steps the first run did not.

To see the plan without changing anything, run `./scripts/install --dry-run`.
If a cauldron supervisor is already running for that environment, the install
stops and prints the command to replace it
(`./scripts/install --replace-running`).

**Upgrading.** After a `git pull`, run `./scripts/install --replace-running`
from a clean checkout (the upgrade builds the `threads` and `mcp` services,
and a build needs one). An environment installed before the MCP service
existed (0.1.5 or earlier) has no `mcp` pin; the upgrade builds the service
and pins it. A `threads` or `mcp` pin at an older build has it rebuilt from
this checkout (or, with `--prebuilt`, installed from the bundle) and
re-pinned; `threads` keeps its address, the supervisor moves the running
service to the new build, and your grants are kept. Through 0.1.17 the
installer moved only `mcp` and left `threads` on the old build, so the
account helper scripts a 0.1.17 upgrade laid down were not the ones the
running daemon looked for (issue #31); running the 0.1.18 install with
`--replace-running` moves it. A pin that is already this build is left as
it is, so a second run changes nothing. Without `--replace-running` an
older pin is left as it is, and the install says plainly that the service
is still the old build; for `mcp` it prints the two commands that upgrade
it:

```sh
cauldron --env prod build mcp --env-copy
cauldron --env prod pin mcp <the label that build printed>
```

In `cauldron status`, a hosted `threads` shows as `live:unverified`: its
health check answered, but it does not speak cauldron's handshake, so the
supervisor cannot also confirm its identity with a boot token. That is the
expected state for `threads`, and the status line says so; nothing needs
doing. Its health answer (the URL `status` prints, for example `curl -s
http://127.0.0.1:<port>/health`) carries `helpers`: where the running daemon
finds `add-account`, `claude-accounts` and `claude-login-browser`, and
whether each passes the check it makes before running one.

`cauldron mcp status` shows the running service's version beside the
`cauldron` command's, and says when they differ; so does a Claude Code
session (the service's instructions, and any refusal of a tool or an
argument the older service does not know). `--dry-run` shows the plan.

The install puts `cauldron` in `~/.cauldron/bin` and does not change your
`PATH`; its last lines say so. Add that directory to your `PATH` once. For zsh:

```sh
echo 'export PATH="$HOME/.cauldron/bin:$PATH"' >> ~/.zshrc
```

Then open a new terminal. (For bash, use `~/.bash_profile`. If you set
`$CAULDRON_HOME`, the directory is `$CAULDRON_HOME/bin`.) The commands below
assume `cauldron` is on your `PATH`.

Claude Code does not need `cauldron` on its own `PATH`. The plugin starts
its MCP server through a small launcher it ships, `bin/cauldron-mcp`, which
runs `cauldron mcp stdio` from the first of: the path in `$CAULDRON_BIN`; the
path the installer records for `prod` in `~/Library/Application
Support/cauldron/cauldron-bin`; `~/.cauldron/bin/cauldron` (or
`$CAULDRON_HOME/bin/cauldron`); and `cauldron` on `PATH`. So a Claude Code
started from the Dock, Finder or an IDE, whose `PATH` is the system's, finds
it too. Whichever source names it, the launcher runs only a regular file
(not a symlink) owned by you or root that group and others cannot write,
in directories owned by you or root that group and others cannot write
either. It does not take one from the project Claude Code starts it in
(the directory it starts in, `$CLAUDE_PROJECT_DIR`, and every directory
above them, short of your home directory, that holds a `.git` or a
`.mcp.json`): a file there is refused whichever source names it (`$CAULDRON_BIN`,
the install marker, `$CAULDRON_HOME`), and the `PATH` step skips empty
and relative entries (`.`, or a leading, trailing or doubled `:`), entries
inside the project, and entries others can write to. When it finds none,
the session still lists a `cauldron` server, with no tools, whose
instructions say where it looked and how to install.

To check that the services are up:

```sh
cauldron status
```

## First run

```sh
cauldron
```

This opens the terminal UI. Press `?` to see every key binding. On an empty
install it opens on one action, **+ create your first cauldron**, and makes
nothing until you take it. After that, `cauldron` opens the cauldron you used
last, unless you name one. To see every part of the UI with something in it,
open the [`cauldron-build` demo
environment](#the-cauldron-build-demo-environment).

**More cauldrons.** Keep one cauldron per project, all in `prod`:

```sh
cauldron create myproject --env prod
cauldron list --env prod
```

Environments other than `prod` are for developing cauldron itself (`cauldron
env create dev`); `cauldron envs` lists them, and
[`docs/environments.md`](docs/environments.md) covers their lifecycle.

**Your first cluster.** A cauldron with no cluster of its own opens with the
Clusters sidebar's `+` selected: press Enter to make a cluster at the root, or
`+` beside an existing cluster to make one under it. Give it a
name, then describe in your own words what belongs in it. That description is
the cluster's boundary, and cauldron uses it to seed the cluster from what the
environment already holds.

**Talking to an agent directly.** `cauldron tui --chat` opens the threads face
on the threads daemon. Start a thread, give it a task, close the terminal, and
it is still there, in the same place, when you come back. The Threads tab in
`cauldron` shows the same threads, pinned or recent, for the environment you
are in: open one to read it, send it a message, or look inside a tool call or
a subagent. Starting a new thread is done from the threads face for now.

## The `cauldron-build` demo environment

`cauldron-build` is a curated environment. Its data is cauldron's own
development, scrubbed of private names, paths and credentials. It shows every
part of the UI with something in it:

- clusters, and the aggregate cluster;
- directives, constraints, objectives and milestones;
- questions, and multi-step actions, including a secret action;
- the Ledger;
- opening a file from a node.

The data ships in the soup crate, not in the UI code, and no other environment
shows it. Open it with:

```sh
cauldron env create cauldron-build
cauldron --env cauldron-build
```


**The terminal interface's Cauldron tab and a cauldron's soup are two
stores.** The tab (its clusters, regions and nodes) reads a seed corpus,
`~/.cauldron/data/<env>/tui/soup/seed/clusters.json`, a file of regions and
nodes in an older workspace format (corpus schema version 5). Only
`cauldron-build` carries one, written when it is created, so the tab of
every other environment shows only what was made in it there. The soup that
`cauldron soup`, the MCP tools and the ripples read and write lives in each
cauldron's directory, `~/.cauldron/data/<env>/cauldrons/<name>/soup`, in the
published `workspace@6` grammar; the terminal interface does not read it
yet. That soup is the store to write knowledge into; records in the older
format are brought into it with `cauldron soup import-corpus` (below). The
terminal interface keeps reading its seed file in this release, and an
import does not change that file. When the tab will read a cauldron's soup
instead is not settled in this release. The tab's clusters are the seed's
too: a cauldron's soup has clusters of its own from 0.1.16 ([Clusters and
claims](#clusters-and-claims)), which the tab does not show yet.

### Records in the older corpus format

`cauldron soup import-corpus <file-or-dir> --env prod --cauldron <name>`
lands records written in the older corpus format (corpus schema version 5,
the seed file's `corpus`) in a cauldron's soup. It reads a seed file, a
bare corpus (`regions` and `nodes`), one record, or a directory of
one-record files, searched recursively. Run it with `--dry-run` first:

```sh
cauldron soup import-corpus ~/old-store --env prod --cauldron kb --dry-run
cauldron soup import-corpus ~/old-store --env prod --cauldron kb
```

Each region becomes a `region` record (its `narrows` become edges) and each
node a record of its type (its `in_region` become edges, and its `cite`
`<thread>#<msg>` becomes `operator`). Two keys the old format could not hold
are read too: `refs` on a node (ids, or `{to, kind, note}`) become edges,
and `cites` becomes `meta.cites`. Each cluster, and each of its children,
becomes a `cluster` record of the `cauldron@1` schema ([Clusters and
claims](#clusters-and-claims)): its name the handle, its description the
boundary, and a `claims` edge to each region of the input it names, so the
region lists it among the clusters that claim it. Every record keeps its old id in
`meta.imported_from`, and every record gets a new id: the printed JSON's
`corpus.id_map` maps old ids to new ones (a cluster's as `cluster:<id>`),
and references and `[[links]]` between the imported records are rewritten.
The batch then goes through `cauldron soup import --remint` unchanged, the
clusters in the same write as the regions they claim (a cauldron made
before 0.1.16 gets its cluster pool with that write): every record is judged by the
soup's door, nothing lands unless all of it does, the same batch twice is
refused, a write to a project marker tracked by git is refused, and while
the `mcp` service holds the cauldron the import is sent to it.

What the published schemas cannot hold is listed under `corpus.not_carried`,
each item with its reason, and is not imported: what a cluster holds besides
its name, description and regions (its tasks, questions and the rest, by key
and count; a `cluster` record has a handle, a boundary and its claims only);
a cluster's claim of a region that is not in the input; a child cluster's
place under its parent (it lands as a cluster of its own, since `cauldron@1`
has no edge between clusters); a cluster whose name or description is
outside the bounds a region's handle and boundary have, or that has no id,
name or description, which is not padded or cut but left out whole; a node
whose type is not one of the schema's ten content types; a reference to an
id that is not in the input; a symbolic link inside a directory input,
which is not followed; and the seed's messages and dashboards. A
`regions`, `nodes` or `clusters` that is not a list refuses the import. A key
the format does not have is listed and carried verbatim in
`meta.imported_from.unmapped`, and a boundary that names old ids is listed,
since its text keeps them. A state outside its type's states, a boundary
over its cap, or a node left with no region is refused by the door, and the
whole batch with it.

## From install to a working session

This is the whole path, from a fresh Mac to a Claude Code session writing
into a cauldron. Every command names `--env prod`: see the `$ENV` note
below for why.

1. **Install** (see [Install](#install)): `git clone
   https://github.com/broomstick-ai/cauldron`, then `./scripts/install` in
   the checkout. It makes the environment `prod` and runs its `threads` and
   `mcp` services.
2. **Put `cauldron` on your `PATH`** (`~/.cauldron/bin`), open a new
   terminal, and check with `command -v cauldron` and `cauldron status`.
3. **Make a cauldron** for your project:

   ```sh
   cauldron create kb --env prod
   ```

   Its soup starts empty, with no regions, and the curator files nothing it
   cannot place in a region.
4. **Give it regions.** The plugin ships an example,
   [`claude-code/cauldron/regions.example.json`](claude-code/cauldron/regions.example.json):
   four regions (the code, its tests, the product, the process). Copy it,
   edit the handles and boundaries to fit your project, then judge it and
   land it:

   ```sh
   cauldron soup import regions.json --env prod --cauldron kb --remint --dry-run
   cauldron soup import regions.json --env prod --cauldron kb --remint
   ```

   (Or skip this step: on the first run the curator proposes regions for you
   to approve.)
5. **Open the cauldron to Claude Code**, yourself, at a terminal (an agent
   cannot do this unless you let it: [Who may act for the
   owner](#who-may-act-for-the-owner)):

   ```sh
   cauldron mcp grant kb --env prod --project ~/code/proj
   ```

   Every cauldron is closed to Claude Code until you grant it. `--project`
   makes `kb` the default for sessions working in `~/code/proj`; it does not
   limit access (any of your sessions can still reach `kb` by naming it).
6. **Install the plugin** inside Claude Code:

   ```text
   /plugin marketplace add broomstick-ai/cauldron
   /plugin install cauldron@cauldron
   ```

   Then restart Claude Code from a terminal where `cauldron` is on the
   `PATH`. In a session, `/mcp` lists `cauldron` as connected.
7. **Write into the cauldron.** In a session in `~/code/proj`:

   ```text
   /cauldron:curate Always read region ids from the soup before writing records: a guessed id made an import fail.
   ```

   The command asks the MCP door which cauldron this directory means (`kb`,
   from the grant's project), tells you, and asks you to confirm before it
   writes to `prod`. The curator then judges its records (a dry run), lands
   them, and reports their new ids.

> **Two ways to say "this directory means cauldron `kb`".** They are
> separate, and setting one does not set the other.
>
> | | `.cauldron` marker | grant `--project` |
> |---|---|---|
> | made by | `cauldron link kb --env prod` | `cauldron mcp grant kb --env prod --project <dir>` |
> | read by | the `cauldron soup` commands (and the curator's shell fallback) | Claude Code sessions, through the MCP door |
> | opens the cauldron to Claude Code? | no, never | the grant does; `--project` only sets the directory's default |
> | kept in | a file in the project (keep it out of git) | `~/.cauldron/data/prod/mcp/grants.json` |
>
> A session uses the grant's project; a shell uses the marker. For both,
> run both commands.

> **`$ENV`.** `cauldron create`, `cauldron mcp grant` and the other
> commands take their environment from `--env`, else `$CAULDRON_ENV`, else
> `$ENV`, else `prod` (the `cauldron soup` commands never fall back to
> `prod`: they need one of the first three, or a marker). Some shells export `ENV`
> for their own use (`ENV=development`, or a startup file's path); then
> `cauldron create kb` without `--env` aims at that environment (and is
> refused when no such environment exists), while Claude Code sessions,
> whose bridge never reads `$ENV`, use `prod`. Pass `--env prod`, as every
> command here does, or unset `ENV`.

**What the door gives a session.** The plugin's MCP server, `cauldron mcp
stdio`, is a bridge to the environment's `mcp` service. A session gets tools
to read, search and write a granted cauldron's soup (`soup_status`,
`soup_regions`, `soup_region`, `soup_nodes`, `soup_search`, `soup_get`,
`soup_put`, `soup_link`, `soup_changes`) and its dumps (below). The grants:

```sh
cauldron mcp grant kb --env prod --project ~/code/proj   # open kb; sessions in ~/code/proj use it by default
cauldron mcp grants --env prod                          # what is open
cauldron mcp revoke kb --env prod                       # close it again, from the next call on
cauldron mcp status --env prod                          # the service and what it holds
```

A grant opens the cauldron to the Claude Code sessions you run on this
machine, whichever directory they work in: any of them can name it.
`--project` only tells a session which cauldron a directory means when the
session does not name one; it does not limit which sessions can reach the
cauldron. `grant` refuses to run without an interactive terminal unless you
opened `grant` to agents ([Who may act for the
owner](#who-may-act-for-the-owner)), so by default an agent cannot open a
cauldron for itself; no MCP tool can grant or revoke. `--scopes read` opens a cauldron read-only. While the service holds
a granted cauldron it is the soup's one writer: `cauldron soup import` is
sent to it, and `cauldron soup recover` is refused.

**A server for one cauldron.** `cauldron mcp config --cauldron kb --env
prod` prints a `.mcp.json` entry for a project: a server `cauldron-kb` that
runs `cauldron mcp stdio --env prod --cauldron kb`. That bridge is pinned to
`kb`. Every call reaches `kb`, and a call naming another cauldron is refused
as `cauldron_pinned`. Add one entry per cauldron to reach several at once.
The plugin README has the details
([One server per cauldron](claude-code/cauldron/README.md#one-server-per-cauldron)).

**From a shell.** `cauldron soup import | regions | region | nodes | search
| get` write records into a soup and read them back (`cauldron soup help`);
`cauldron soup link | unlink | cite` add or remove one edge, or one
citation, of records it holds.
The records follow the published schema in
[`schema/soup/workspace-v6/`](schema/soup/workspace-v6/README.md), whose
README also says how to change a node (supersede it), what the import checks
and what it leaves to you. `cauldron link kb --env prod` writes a
`.cauldron` marker into a project directory, so `cauldron soup` run inside it
needs neither `--env` nor `--cauldron`.

**Dumps, and curating them.** A dump is raw text put into a cauldron for
later: a note, a file, or a whole Claude Code session's transcript. It is
kept in the cauldron's dumps store beside its soup (not in the soup), as
`DM-<n>` in state `received`. From Claude Code, `/cauldron:dump` keeps text
or a file and `/cauldron:dump-session` keeps the session's transcript (the
grant's `dump` scope allows both); from a shell, `cauldron soup dump <file|->
--env prod --cauldron kb`. `/cauldron:curate-dump DM-3` has the curator read
the dump in pages (for a transcript, only what you typed and what the
assistant wrote as text: tool calls and their output, thinking, `!` shell
commands and their output, local commands' output, slash-command wrappers,
system reminders and attachments are left out and counted) and land the
records worth keeping, each citing the dump; the dump itself stays
`received`. `cauldron soup
dump-get DM-3 --env prod --cauldron kb --view text` shows the same pages in
a shell. A session transcript holds tool output, shell output and anything
pasted into the session, and it is stored verbatim (the raw view, `dump-get`
without `--view text`, shows all of it), in your own cauldron, readable only
by you. The grant's scopes govern Claude Code sessions; the `cauldron soup
dumps` and `dump-get` commands run as you and read dumps without a grant.

**Renaming and removing a cauldron.** `cauldron rename kb notes --env prod`
renames it; its soup, dumps and grant follow. `cauldron remove kb --env
prod` is a dry run; with `--yes` it moves the cauldron to
`cauldrons/.removed/` (it never deletes) and drops its grant. Both are
refused while the `mcp` service holds the cauldron: stop the service
(`cauldron --env prod stop mcp`, then `start mcp` afterwards) or revoke the
grant first (a revoked cauldron has to be granted again). A project's `.cauldron` marker naming the old name is not
updated; run `cauldron link notes --env prod --replace` there.

The plugin's [README](claude-code/cauldron/README.md) has the details.

### Who may act for the owner

New in 0.1.17 ([#25](https://github.com/broomstick-ai/cauldron/issues/25)).
Three acts belong to a cauldron's owner:

| act | commands (and tools) |
|---|---|
| `grant` | `cauldron mcp grant`: open a cauldron to the Claude Code sessions you run |
| `approve` | `cauldron soup approve`, `cauldron ripples approve \| reject` (and taking either back); the MCP tools `soup_approve` and `soup_reject` |
| `ripples` | `cauldron ripples on`: automatic processing, and the model spend it starts |

**What the gate is for.** By default each runs only with its input and
output on an interactive terminal: a command run at a terminal is taken to
be the owner's, and an agent, a script or a ripple is refused (the refusal
names the command for you to run, and how you would let agents run it). That keeps three things
from happening without you deciding: an agent approving the records a
ripple wrote, so that model output reads as reviewed; an agent opening a
cauldron to every Claude Code session, or widening its scopes; and
automatic model spend and unattended writes starting.

**What the gate is not.** It stops an agent that follows the rules from
acting for you by accident. It is not a security boundary against a process
that means to get around it: anything running as your user can give a
command a terminal (`script(1)` does, in one line) or read your files (the
`mcp` service's approval key among them, which its `DECIDE` verb takes as a
decision made at a terminal). The boundary is your user account, as it
always was. So `by: "tty"` means "run with a terminal attached", not "a
person". What the `mcp` service can check, it does: a decision claimed as
an agent's is taken only where your persisted setting opens `approve`, and
how a choice was set (`how`) is recorded by the command that set it, never
taken from an argument.

**What it costs, and the choice.** An agent you told to set up or run
Cauldron has to stop and hand you a command; a review queue only a person
can drain piles up; and a session with no terminal (CI, a server, a
scheduled job) cannot do these steps at all. So the gate is a setting,
`owner_acts`, that you choose. `terminal` is the default and changes
nothing: with nothing chosen (an upgraded root included) every act behaves
as in 0.1.16. `open` lets all three run without a terminal; a list
(`grant`, `approve`, `ripples`) opens only those.

**What `open` gives up.** With `approve` open, an agent can approve what a
ripple wrote, so a record can read as reviewed without a person having read
it. With `grant` open, an agent can open a cauldron to every Claude Code
session of yours. With `ripples` open, an agent can start automatic model
spend and writes. Choose it when you want your agents to do these for you.

**Three ways to choose.**

```sh
# at a terminal: the root's choice, for every cauldron of every environment
cauldron owner-acts open
cauldron owner-acts open --only approve
# one cauldron's own choice wins over the root's, both ways
cauldron owner-acts terminal --cauldron kb --env prod
cauldron owner-acts unset --cauldron kb --env prod     # back to the root's
cauldron owner-acts show --env prod                    # what is in effect, per act, and from where
```

- **The command**, `cauldron owner-acts open | terminal [--only
  grant,approve,ripples] [--cauldron C] [--env E]`. At a terminal it just
  applies. Without one, opening is refused unless the variable below is set
  in its environment (and opens each act it would open); closing runs from
  anywhere.
- **The environment variable**, `CAULDRON_OWNER_ACTS=open` (or a comma
  list of acts) in the environment of the `cauldron` command that acts: put
  it in your shell profile, in your user settings for Claude Code (`env` in
  `~/.claude/settings.json`, never a project's `.claude/settings.json`,
  which a cloned repository could carry), in CI or on a server. It opens
  `grant` and `ripples` for that command, whatever the setting says, and it
  lets `cauldron owner-acts open` record a choice without a terminal.
  Approving and rejecting are carried out by the `mcp` service, which takes
  an agent's decision only where the persisted setting opens `approve`: the
  variable alone does not, and the command says so before it sends
  anything (record it once: `CAULDRON_OWNER_ACTS=approve cauldron owner-acts
  open --only approve`). Anything
  that can set its own environment, an agent told to included, can turn it
  on. That is the point: you decide whether your agents may act for you.
  It means "the command I ran may act", and nothing else reads it: `cauldron
  start` and the supervisor remove it from every service they start, and
  the `mcp` service decides `soup_approve` / `soup_reject` from the
  persisted choice alone, so the variable in your shell never opens the
  door to sessions. To let sessions approve through the door, record the
  choice (`cauldron owner-acts open --only approve`).
- **The installer**, `scripts/install --owner-acts open|terminal`, for a
  headless install (it runs `cauldron install --owner-acts …`, the only
  command that records `how: install`).

Each choice is kept in `~/.cauldron/owner-acts.json` (owner-only) with when
it was set and how (`tty`: run with a terminal attached, `env`: under the
variable, or `install`), recorded by the command itself; a cauldron's own choice is
bound to the cauldron's id, so a cauldron removed and made again under the
name does not carry it. A development or check root is not special: its
commands carry the variable, and its sessions follow its persisted choice.

**What is not an owner act.** `soup_link`, `soup_unlink` and `soup_cite`
(and `cauldron soup link | unlink | cite`) are ordinary writes, open to any
grant with `write`, whatever `owner_acts` says, and none of them can set or
clear a review flag, its `by`, or a ripple's provenance. `soup_cite` also
leaves alone the `dump` citation of a record a ripple wrote, the one the
review queue groups it by (`dump-cite-is-provenance`). A server pinned with
`cauldron mcp stdio --cauldron` refuses `soup_approve` and `soup_reject`
naming another cauldron, as it refuses every tool, and `cauldron mcp
config` never writes `CAULDRON_OWNER_ACTS` into a project's `.mcp.json`
(there is no flag for it). `cauldron inference models pull` asks you to
type `yes` at a terminal, or takes `--yes`: that is your consent to one
download, not an owner act, so `owner_acts` neither opens nor closes it.

**Seeing who did what.** Every act records who did it: `by: "tty"` (run
with a terminal attached) or `by: "agent"` (run without one, allowed by your
choice) in the
command's answer, in the grant row (`cauldron mcp grants`), in the `mcp`
service's audit log (`calls.jsonl`, with the session for a tool call), in
the automatic journal's `turned_on` line, and on each approved or rejected
record (`meta.approved.by`, `meta.rejected.by`). A decision recorded before
0.1.17 (`by: "owner"`) was made at a terminal and reads as `tty`.
`cauldron soup nodes --review by-agent` (or `by-tty`; `review` on the MCP
reads) lists only the records an agent (or a person) decided, each with
`decided_by`, and the review queue counts both. `cauldron status`,
`cauldron mcp status`, `cauldron ripples status` and `cauldron owner-acts
show` show the setting in effect, twice over: per act for a command run
from that shell without a terminal (`acts`, where `env` marks an act only
the variable opens), and for MCP sessions (`sessions`, the persisted choice
alone); the `soup_status` tool says what the door lets a session do.

**What no setting changes.** A ripple never approves or rejects: the soup
refuses the review flags to every ripple, and the `mcp` service refuses the
ripples service's identity on `soup_approve` / `soup_reject`. No record
write sets the review flags or a ripple's provenance. `review
--interactive` always needs a terminal, because it asks a person.

### Ripples, by hand

A **ripple** turns one dump into new records in the soup: ideas, issues,
questions, decisions, constraints, tasks and dangers, each one claim, each
citing the dump it came from. A ripple runs when you run it, on the one dump
you name, or, for a cauldron where you turned that on, on each new dump by
itself ([Ripples on their own](#ripples-on-their-own)):

```sh
cauldron ripples run DM-3 --env prod --cauldron kb        # one dump; prints the records and the tokens
cauldron ripples status --env prod --cauldron kb          # the last runs, spend today and per dump, the caps
cauldron ripples review --env prod --cauldron kb --text   # what a ripple wrote and nobody reviewed yet, by dump
cauldron ripples approve --dump DM-3 --env prod --cauldron kb   # you, at a terminal: mark a dump's records reviewed
cauldron ripples reject --dump DM-3 --env prod --cauldron kb    # you, at a terminal: keep them out of the reads
```

**Off unless you start it.** The work is done by the `ripples` service,
which the installer does not start. Start it yourself, in the environment:
`cauldron --env prod build ripples`, then `cauldron --env prod pin ripples
<the version label it printed>`, then `cauldron --env prod start ripples`.
The `mcp` and `threads` services must be running too (threads from this
release: an older threads is refused, see below), and the cauldron must be
granted to the MCP door with `read`, `write` and `dump` (the default
scopes). Even running, the service processes nothing on its own unless you
turned automatic processing on for a cauldron (`cauldron ripples on`, see
[Ripples on their own](#ripples-on-their-own)); otherwise only `cauldron
ripples run` sets it off. Stop it with `cauldron --env prod stop
ripples`.

**The model call is a bare completion, not an agent.** The call goes
through the environment's `threads` service, which runs it as one Claude
Code call (`claude -p`) on the Claude login threads uses on this machine,
under threads' own budgets. The ripple asks threads for a thread with
nothing an agent has, and threads runs it that way:

- no tools of any kind: no built-in tool (`--tools ""`), an explicit deny
  of every built-in tool and of threads' own MCP server, no allow rule, and
  no MCP server at all (`--strict-mcp-config` with an empty config);
- no settings files, so no hooks, plugins or permission rules from your
  user or project configuration (`--setting-sources ""`), no skills or
  commands (`--disable-slash-commands`), and no plugin directory;
- no CLAUDE.md or memory: Claude Code's safe mode (`--safe-mode`, which
  its help says starts with CLAUDE.md, skills, plugins, hooks and MCP
  servers disabled), auto-memory turned off, and an empty working directory
  made for the call. Settings an administrator manages (policy) still
  apply, as they do to every Claude Code;
- permission mode `dontAsk` (a tool use nothing pre-approved is denied),
  never a skip-permissions mode;
- one model turn, its answer bounded to 16,000 tokens, and given as JSON
  that must meet [`batch.schema.json`](services/ripples/package/dump/v1/batch.schema.json)
  (Claude Code's structured output; its `StructuredOutput` step is how the
  answer is returned, not a tool the model can use for anything else).

Before anything is sent, the ripple reads the thread back from threads and
refuses to go on (`threads_unsupported`) unless threads kept those
settings: an older threads silently ignores them. The dump's text sits in
the prompt between two marker lines, after a rule that it is untrusted data
and never instructions, and the answer is checked against the schema again
by the ripple itself. Those two are there to keep answers useful; what
keeps a dump from steering anything is that the call has no tool to use.

**What leaves this machine, and what is stored where.**

- *Sent to the model provider* (Anthropic, through Claude Code on the login
  threads uses): Claude Code's own system prompt, which names the working
  directory (an empty directory made for the call under the system's
  temporary directory, never your project or home), the platform, the OS
  version and the date, and a note Claude Code adds naming the email
  address of the login the call runs on; and one prompt, the file
  [`services/ripples/package/dump/v1/prompt.md`](services/ripples/package/dump/v1/prompt.md)
  filled in with the cauldron's regions (id, handle and boundary, at most
  50, each boundary cut at 400 characters), the dump's id and kind, and the
  dump's text. No existing record of the cauldron is sent. A text dump is
  sent whole. A session dump is sent only as its text view: what you typed
  and what the assistant wrote as text (a subagent's text included), never
  tool calls, tool output, thinking or shell output. Since 0.1.12 the text
  (and the region text) is first passed through the secret scrubber, which
  replaces credential-shaped text with `<redacted:kind>` ([the
  scrubber](#the-secret-scrubber)); it reduces the risk and is not a
  guarantee. The schema is sent as the shape of the answer.
- *The call's environment*: `claude` runs with every variable of the threads
  service's environment removed except `PATH`, `HOME`, `USER`, `LOGNAME`,
  `TMPDIR`, the locale (`LANG`, `LC_ALL`, `LC_CTYPE`) and proxy settings
  (`HTTPS_PROXY`, `HTTP_PROXY`, `NO_PROXY`, `NODE_EXTRA_CA_CERTS`), plus
  the login's `CLAUDE_CONFIG_DIR` and settings that turn off Claude Code's
  telemetry, error reports and other non-essential traffic. These stay on
  this machine; Claude Code needs them to run and to find its login.
- *Kept by threads* (in the environment's threads store,
  `~/.cauldron/data/<env>/threads/store/`): one thread per call, named
  `ripple <first characters of the run id>`, with its settings; the prompt,
  dump text included, as the thread's message, and the answer and its token
  counts as events, in `threads.db`; and the call's output stream (the
  answer, never the prompt) in `turns/<thread>/`. threads keeps them until
  you delete them; nothing in this release removes them, and removing the
  dump afterwards (`cauldron soup dump-remove`) does not either.
- *Not kept by Claude Code*: the call runs with `--no-session-persistence`,
  so no session transcript is written under the login's `projects/`
  directory.
- *Kept by the ripple*: the run journal,
  `~/.cauldron/data/<env>/ripples/runs.jsonl`, with each step's ids,
  counts, models, tokens and outcome: codes and fixed reasons, never dump
  text, the prompt, the model's answer or its notes, or a backend's own
  words. `run` shows you those words on the terminal (`detail`, `notes`)
  and writes them nowhere; `cauldron ripples run … --debug-keep-answer`
  keeps one run's answers and refusals, unscrubbed, in one owner-only file
  under `~/.cauldron/data/<env>/ripples/debug/`, whose path it prints (by
  hand only; delete it when done). The call's working directory is removed
  when the call ends.
- *Written to the soup*: the records, through the `mcp` service, audited in
  its `calls.jsonl` (counts and ids, never text).
- *With the local backend* (`cauldron ripples run ... --backend local`, see
  [Local inference](#local-inference)): **nothing leaves this machine.** The
  same prompt and schema go over loopback to the environment's `inference`
  service and from it to its `llama-server` child on 127.0.0.1; threads,
  Claude Code, a login and the network are not involved, so no provider
  receives anything and threads keeps nothing. Every address the ripple
  dials must be a loopback address, or nothing is sent. The `inference`
  service writes no prompt or answer itself; its `llama-server.log` holds
  llama-server's own output at its default log level, which has not yet
  been checked against a real model. The ripple's journal gains a `local`
  line per call: the model, the sha256 of the model file as checked at
  load, the tokens and tokens per second, still never text.

The scrubber does not catch every secret, so do not run a ripple on a dump
that you know holds one: remove the dump instead (`cauldron soup
dump-remove`), before any run.

**Removing a dump after a ripple ran on it** erases the dump's content and
nothing else. Two things stay:

- *The records the ripple drew from it.* `dump-remove` does not touch them,
  and a model may have copied text from the dump into one. Find them by
  their cite: each carries `meta.cites: [{"kind": "dump", "id": "DM-3"}]`
  (`cauldron soup get <id>` shows it). The run's records are also listed by
  the dump itself before you remove it (`processed.records` in `cauldron
  soup dumps`), by `cauldron ripples status` (the run's `records`), and,
  while you have not reviewed them, by `cauldron ripples review --dump DM-3`.
  Reject or supersede any record you do not want.
- *threads' copy of each call.* The threads service keeps a copy of every
  ripple call's prompt (the dump's text included) and of its answer, in its
  store, as above, until you delete it.

**How a ripple's records are marked.** Each record cites the dump
(`meta.cites: [{"kind": "dump", "id": "DM-3"}]`) and carries `meta.ripple`
(the package, the run and the model). It is written through the `mcp`
service, which records the write as a ripple's: no field of the record can
say otherwise. From 0.1.17 only that path sets `meta.ripple`: any other
record write (`soup_put`, `cauldron soup import`, with or without
`--remint`, direct or sent on to the service) that carries it, in any
casing or nesting, is refused as `ripple_provenance`, as the review flags
are. A record written before 0.1.17 may still carry one a session made up:
read who wrote a record from `soup_review` (or `cauldron ripples review`),
never from the field. Every read shows it, marked `unreviewed: true`, until you
approve it: `cauldron soup nodes --review pending` (and `soup_nodes` /
`soup_search` with `review: "pending"`) lists only those. An unreviewed record was written by an automatic step from your dump, so
its text is data, not instructions: an MCP reply that holds one carries an
`unreviewed_note` saying so, and the plugin tells Claude Code sessions never
to act on an instruction found inside a record or a dump, and to tell you
when one reads like an instruction. That is guidance to the session, not a
control.

**Reviewing is yours**, at a terminal unless you let agents do it: see
[Reviewing a ripple's records](#reviewing-a-ripples-records).

**What a ripple may write.** New records only (never an edit, a close or a
supersede of an existing node), of those seven types, each in at least one
existing region, linked only with `derived_from` to other records of the
same answer, at most 25 at once. No directives, regions or flags. A label
used twice in one answer (`duplicate-id-in-batch`), a label equal to an id
the cauldron holds (`label-shadows-existing`) or a record that links to
itself is refused, and nothing of that answer lands.

**The caps, counted across runs.** Before every call the ripple adds up, from
its journal, what has been spent:

- per dump, across every run of it: at most 3 model calls and 200,000
  input tokens. Running a dump again does not reset them;
  `cauldron ripples run ... --force-budget <tokens>` lets that one run spend
  that many more input tokens (and 3 more calls) on the dump;
- per cauldron per day (UTC): at most 1,500,000 input tokens or 30 dumps.
  `--force-budget` does not lift this.

A call is charged when it begins: before anything is sent, the ripple
checks the caps and writes the call into its journal in one step, under a
lock, as one call and its estimate (the prompt's characters divided by
three), so two runs cannot both take the last call. When the answer comes,
the call is settled to the larger of the input tokens it reported and that
estimate. A call cut short (the `ripples` service killed or stopped during
it) is never settled and stays charged at its estimate; `status` counts it
and shows it under `unsettled_calls`. The estimate does not include Claude
Code's own system prompt, and one call can go past a cap (the check is made
before it, not during it). Over a cap,
nothing more is sent and the run ends `deferred` (`budget` or `daily_cap`),
with the dump still `received`. The first call is Opus (or `--model`); an
answer that misses the schema, or that the soup refuses, gets one retry on
Opus with the refusal. A backend that answers busy (another request holds
the local `inference` service, or it is not ready yet; threads refusing the
call as busy or overloaded) refused the call before it ran: it is charged
nothing and is no call against a cap, and a run by hand tries it at most 3
times, then ends `deferred` (reason `busy`) with the dump still `received`.
`run` prints each run's tokens (input, output, cached, the calls and the
models), and `status` shows the spend today per cauldron and per dump. Today's
spend is grouped by the cauldron itself (its id), not by the name it was
spent under, and shown under its current name, with `cauldron_id` and the
`names` its calls were journaled under: a renamed cauldron's day is one
entry, a cauldron made again under an old name is another, and spend of a
cauldron no longer in the environment shows as `<name> (gone)`. The
evaluations' ledger (`evaluations_today`) is grouped the same way.

**A call given up is stopped.** When threads parks the call under its
budget, the turn ends without an answer, or no answer comes within 15
minutes, the ripple asks threads to stop the turn and to withdraw the
message (so no budget reset or retry runs it later), then reads the thread
until it is idle. A call whose `claude` ends without an answer is not run
again: threads ends it at once with the last lines `claude` wrote to its
error output (the home directory shown as `~`, the call's working directory
as `<cwd>`), and the run ends `inference_failed` with that text. The
outcome says whether threads confirmed it; if not, it
says the call may still be running and how to stop it (cancel that thread
from a threads face, or stop the threads service), and `status` lists it
under `possibly_running`. A call left running by a killed or stopped
`ripples` service is cancelled the same way when the service next starts
or runs.

**The dump.** A dump a ripple processed moves from `received` to
`processed`, and records which run and which records (`soup_dumps` and
`dump-get` show it). Curating by hand (`/cauldron:curate-dump`) still leaves a
dump `received`. A removed dump, or one whose removal is unfinished, is
refused. Running a ripple on a dump twice gives one set of records: the
second run answers the first one's. A run cut short (the service stopped or
killed) shows as `interrupted` in `cauldron ripples status`; run the same
command again, and it lands once.

### Reviewing a ripple's records

New in 0.1.13. Every record a ripple writes is **unreviewed** until you
decide it: you approve it (it reads as reviewed from then on) or you reject
it (it is kept, and left out of the normal reads). Only you decide: at an
interactive terminal, or, where you opened `approve` to agents, an agent
you told to (the commands without a terminal, or a Claude Code session
through `soup_approve` / `soup_reject`), each decision recorded with who
made it. A ripple never decides.

**The `mcp` service must be this release's.** A decision is written by the
running `mcp` service, and this command line refuses to send one to an
older service (it would not read the decision as this release means it),
writing nothing. If you upgraded without `--replace-running`, move the
service to the new build first, with `./scripts/install --replace-running`
or the two commands under "Upgrading" above.

```sh
cauldron ripples review --env prod --cauldron kb --text          # the queue, grouped by dump and run
cauldron ripples review --env prod --cauldron kb --dump DM-3     # one dump's records, as JSON
cauldron ripples approve --dump DM-3 --env prod --cauldron kb    # every record of that dump
cauldron ripples reject --run <run> --env prod --cauldron kb --reason "duplicates"
cauldron ripples approve C-12 IS-4 --env prod --cauldron kb      # by id
cauldron ripples review --interactive --env prod --cauldron kb   # walk the queue, a key per dump
```

**1. See what waits.** `cauldron ripples review` lists the records a ripple
wrote that carry neither decision, grouped by the dump they cite and the run
that wrote them, newest group first. Each group says the model and backend
that wrote it, when, and the exact commands that approve or reject it; each
record its id, type, state, handle, the first 240 characters of its body,
its regions and its other edges. `counts` gives the records waiting, the
groups, per dump and per type, and how many of the matching records you
already approved or rejected. Filters: `--dump DM-n`, `--run <run>`, `--type
<type>`, `--since <YYYY-MM-DD or RFC 3339 time>`. The answer is at most
`--max-chars` characters (default 12,000, at most 50,000; under 2,000 is
refused), as JSON and as `--text`: whole records while they fit, and a
record longer than the bound cut (its edges, then its longest fields),
listed in `truncated`. One that stopped early says `next_after`, the last
record it showed; `--after <that id>` lists the records after it, so you can
decide a page and then ask for the next without skipping any. `--text`
prints it for reading, each record on one line, with its control and
invisible characters (escape sequences, bidirectional overrides, zero-width
characters) shown escaped, as `\u{202E}`, so a record cannot draw on your
terminal or reorder what a line shows; a group cut by the page says how
many of its records it shows, and that its commands decide them all. It
reads only, so it works with the services running or not. `cauldron ripples status`, `cauldron mcp status` and `cauldron status`
show how many records wait per cauldron, with this command; a dump receipt
that waited for its records (`wait_secs`) names it too, and the approve and
reject commands for that dump.

**2. Decide.** `cauldron ripples approve` and `cauldron ripples reject` take
the records one way: `--dump DM-n` (every record a ripple wrote citing that
dump), `--run <run>` (every record that run wrote), or record ids. Each is
one write through the running `mcp` service, as the owner, audited in its
`calls.jsonl` (`soup_approve`, `soup_unapprove`, `soup_reject` or
`soup_unreject`, with the selector, the counts and, for `--run`, the run;
never a record's text or your reason).

- **Approve** sets `meta.approved: {at, by: "owner"}`. `--unapprove` takes
  it back.
- **Reject** sets `meta.rejected: {at, by: "owner", reason}` (`--reason`:
  one line, at most 500 characters). Nothing is deleted. `soup get` (and
  `soup_get`) still answers a rejected record, marked `rejected: true`,
  with its cite to the dump and your decision. `soup nodes`, `soup search`
  and a region's members (and `soup_nodes`, `soup_search`, `soup_region`)
  leave it out and say how many in `rejected_hidden`; `--include-rejected`
  (MCP: `include_rejected`) lists them, marked. The queue no longer lists
  it. `--unreject` takes a rejection back: the record is unreviewed again.
- **The two exclude each other.** A rejected record is not approved, by
  `ripples approve` or `soup approve`, until you take the rejection back;
  an approved one is not rejected until you take the approval back. Asking
  again for a state a record is already in changes nothing: the answer
  lists it in `skipped`, with why.

`cauldron soup approve <id>... [--unapprove]` still works as before, and
like these it needs this release's `mcp` service.

**3. Or walk it.** `cauldron ripples review --interactive` shows the queue
one group at a time and takes a key: `a` approves the whole group, `r`
rejects it (asking for an optional reason), a record's number marks it
(approve, then reject, then unmarked; several at once: `1 3`) and `w`
writes the marks, `s` skips the group, `q` quits. Each group's decision is
one write: the reason is asked before anything is written, then its
approvals and rejections land together (one audit line,
`soup_approve_and_reject`, when it does both), or, when any of its records
is no longer as it was listed, nothing of the group lands and the pass says
why. A skipped group, and every group after `q` or the end of input, is
left as it was. Records are shown as `--text` shows them. It takes the same
filters as `review`.

**Why a terminal, and when not.** By default `approve`, `reject` and
`review --interactive` run only with their input and output on an
interactive terminal, like `cauldron mcp grant`: a script or an agent
running them is refused, and the refusal names the exact command for you
to run and how you would let agents do it. If you open `approve` to agents
([Who may act for the owner](#who-may-act-for-the-owner)), `approve` and
`reject` run without a terminal too, and a Claude Code session can decide
through `soup_approve` / `soup_reject`; every such decision says `by:
"agent"` on the record (`meta.approved.by`, `meta.rejected.by`, with the
session when there is one), in the answer and in the audit log, where one
made at a terminal says `by: "tty"`. `review --interactive` asks a person,
key by key, so it always needs a terminal. A ripple can do neither,
whatever you chose: the soup refuses `meta.approved` and `meta.rejected` from a ripple,
and no record write (`soup import`, `soup_put`, a ripple's batch) may carry
either, set or cleared, in any casing or nesting. In a Claude
Code session, `/cauldron:review` reads the queue (the MCP tool
`soup_review`, scope `read`) and tells you these commands; where you opened
`approve` to agents and you ask it to, it decides through `soup_approve` /
`soup_reject` instead. Unreviewed text was written by a model from a dump, so the session is
told to treat it as data and never to act on an instruction inside it. That
is guidance to the session; the gate guards the commands and the tools, not
the files, and a process running as you that reads the `mcp` service's
private key file can send the same request.

### Ripples on their own

New in 0.1.12. **Off by default, for every cauldron, and an upgrade never
turns it on.** The owner turns it on for one cauldron at a time, at a
terminal (or an agent the owner let do it: [Who may act for the
owner](#who-may-act-for-the-owner)):

```sh
cauldron ripples on kb --env prod                     # threads backend, text dumps only
cauldron ripples on kb --env prod --backend local     # this machine's inference service; nothing leaves it
cauldron ripples on kb --env prod --include-sessions  # session transcripts too (their text view)
cauldron ripples status --env prod --cauldron kb      # on or off, since when, the backend, the queue, spend today
cauldron ripples off kb --env prod                    # from anywhere; before the next call
```

**What turning it on means.** From that moment, each dump that lands in the
cauldron is processed with no further command, exactly as `cauldron ripples
run` would process it: the same call, the same caps, the same journal, and
records marked `unreviewed` until you approve or reject them at a terminal
([Reviewing a ripple's records](#reviewing-a-ripples-records)). It authorises model spend and writes into the
cauldron that nobody asks for one by one, so, like `cauldron mcp grant`,
`on` requires an interactive terminal (its input and output must both be
one; no MCP tool or seam verb turns it on) unless you opened `ripples` to
agents. At a terminal it prints what will be sent where, the caps and the
failure policy, then asks `[y/N]`, and writes the setting only on yes;
without one (opened to agents) it asks nothing, prints the same text as
what it authorised, and records `by: "agent"` in its answer and in a
`turned_on` line of the automatic journal (`ripples status` shows who last
turned it on). The setting is a file per cauldron,
`~/.cauldron/data/<env>/ripples/auto/<cauldron>.json` (owner-only, written
whole and renamed into place); no file means off.

- **Bound to one cauldron, never to its name.** The setting records the
  cauldron it was turned on for: the environment and the cauldron's id, the
  random id in its `cauldron.toml` that its MCP grant is bound to too (see
  [The cauldron's id](#the-cauldrons-id)). It is checked before every
  automatic run and every model call of one. If the cauldron is gone, a
  cauldron made again under its name is there (even one whose
  `cauldron.toml` names the same creation time), or the file was copied
  from another cauldron or environment, the setting is void: nothing runs,
  a dump's receipt says it is not taken, and `status` shows `automatic:
  void`, why, and how to clear it (`cauldron ripples off`). The cauldron's
  directory plays no part once it has an id, so a data root moved whole,
  to another place or another volume, keeps its settings on, and so does a
  copy of a whole data root restored elsewhere. `cauldron remove` turns automatic processing off for the cauldron
  and says so; `cauldron rename` turns it off too and prints the `on`
  command for the new name (an authorisation of automatic spend never
  follows a rename). A cauldron made again under an old name starts with a
  clean queue.

- **Only new dumps.** `on` records the newest dump the cauldron holds, and
  only dumps after it are processed automatically. Older dumps are
  processed only by hand. Turned off and on again, the mark moves to that
  moment.
- **Text dumps only, unless `--include-sessions`.** A session dump is then
  sent as its text view, as by hand.
- **The service.** The `ripples` service must be running; the installer
  does not start it (see [Ripples, by hand](#ripples-by-hand) for how).
  The `mcp` service tells it when a dump lands through it; it also looks for
  dumps still `received` when it starts and every minute, which finds dumps
  that landed while it was down and dumps `cauldron soup dump` wrote
  straight to the store. One dump at a time, oldest first, one dump per
  cauldron per pass, the cauldron served least recently first. One service
  per environment: a second one started on the same data refuses to start
  and names the first's pid. While an automatic run holds the service's
  one run slot, `cauldron ripples run` is refused `busy` with `held_by:
  automatic`; the slot is let go between dumps, so trying again in a moment
  gets it. While a run by hand or an evaluation holds it, a dump that lands
  waits, `status` says so (`waits_for.run_slot`), and it starts when that
  run ends.

**What is sent where, per backend.**

- *threads* (the default): the same bare completion as by hand, on this
  machine's Claude login (see [What leaves this machine](#ripples-by-hand)
  above): the dump's text, scrubbed, with the cauldron's regions, to
  Anthropic; threads keeps a copy of each call's prompt and answer in its
  own store. Opus first, one Opus retry when the answer is refused, or
  `--model`. A cauldron turned on before 0.1.17 without `--model` was
  turned on when the default was Sonnet; it runs on Opus now, which costs
  more per call (about twice the per-token price at Anthropic's API rates,
  and more of a subscription's usage limits). It is not moved silently:
  `cauldron ripples status` shows `default_changed` for it (what it was,
  what it is, the cost, and the command that keeps Sonnet, `cauldron
  ripples on <cauldron> --model sonnet`), and its first automatic run
  after the upgrade writes one `default_model_changed` line to the
  automatic journal. A cauldron turned on with `--model` keeps that model.
- *local* (`--backend local`): the `inference` service on this machine,
  over loopback; nothing leaves the machine. The text is scrubbed too,
  unless `on` was given `--no-scrub-local`.

**Caps.** As by hand, charged before each call: per dump, 3 calls and
200,000 input tokens across every run of it; per cauldron per UTC day,
1,500,000 input tokens or 30 dumps. Over a cap the dump stays `received`,
its listing shows `auto: {state: deferred, reason: budget, retry_after}`,
and it is tried again after midnight UTC, when the daily window resets (a
dump over its own cap is then deferred again with nothing sent, until you
run it by hand with `--force-budget`). The daily cap is counted per
cauldron, not per name: a cauldron made again under an old name starts its
day clean, and a renamed one keeps what it spent that day.

**When threads parks the call.** threads holds a call when every account it
may use is over its usage threshold, an account reached its limit, the
credit balance is spent, the login expired or one of its budgets tripped.
The ripple withdraws the call; the dump stays `received` with `auto:
{state: deferred, reason: threads_parked, park: <cause>, retry_after}`,
and the attempt is not a failed one. A call threads parked before it ran
(no tokens reported) is charged nothing and does not count against any
cap. After a park, no automatic run on threads, of any cauldron in the
environment, starts until the wait is over: 2 minutes, then 8, then 32,
then an hour at most, for as long as the parks go on in a row. A parked
account costs one withdrawn call per wait, not one per dump. `status`
shows the hold (`automatic.threads_hold`: since when, the cause, the dump
that set it, the next attempt) and marks each cauldron it holds (`held`).
When the wait is over, the cauldron served least recently is tried first,
so one whose calls keep being parked does not keep the others waiting.

**When the backend is busy.** A call the backend refused as busy (another
request holds the local `inference` service, as a run by hand or another
client does, or it is not ready yet; threads refusing it as busy or
overloaded) ends the automatic attempt at once: the dump stays `received`
with `auto: {state: deferred, reason: busy, retry_after}`, the attempt is
not a failed one and is charged nothing, and the next scan tries it
again.

**Failure.** A failed attempt (the model could not be reached, the call
failed or ended at once, as a `claude` that refuses its command line and
exits does, the service was stopped during it) leaves the dump `received` with
`auto: {state: retrying, failed_attempts, last_code, retry_after}`, and it
is tried again after 2 minutes, then 8. After 3 failed attempts, or at
once when trying again cannot help (the answer was refused after its retry,
the cauldron has no region, threads is too old, the scrubber ran out of
time), the dump is parked in state `failed` with `failure: {code, message,
attempts, what_to_do}`, whose message is a fixed sentence for the code. It
is never tried automatically again; `cauldron ripples run` still runs it by
hand, and a dump processed that way no longer lists the failure. Each
attempt is held to the dump's caps, so a failing dump costs at most what
any dump may. Every attempt and outcome is a line in
`~/.cauldron/data/<env>/ripples/auto.jsonl`: ids, codes, counts, times and
fixed reasons, never the dump's text, the model's answer or a backend's own
words (nor the end of a failed `claude`'s stderr that threads reports; a
run by hand shows it on the terminal). If that file cannot be written,
nothing is run until it can.

**Seeing it.** `cauldron ripples status` shows, per cauldron with a
setting: on, off or void, since when, the backend, and the queue (waiting,
running, deferred with the reason, retrying, failed with the reason,
session dumps not taken, the last ten processed), what holds its waiting
dumps (`held` by the threads hold, `waits_for` another run), and the
threads hold itself (`threads_hold`), with the spend today; it reads the
same from the files when the service is stopped (all but `waits_for`,
which only the running service knows). A dump's own listing
(`soup_dumps`, `soup_dump_get`, `cauldron soup dumps`) shows its state, and
once processed the run and the record ids. The dump receipt (`soup_dump`,
`soup_dump_session`, `cauldron soup dump`) says whether automatic
processing is on and takes that dump; `soup_dump` with `wait_secs` (and
`cauldron soup dump --wait-secs`), at most 120 seconds, waits for it and
answers the record ids.

**Turning it off.** `cauldron ripples off <cauldron> --env <env>` works
from anywhere, needs no terminal, and takes effect before the next model
call: a run in progress stops before its next call and before it would
write its records, and writes nothing more (an answer already received is
discarded and the dump stays `received`). Stopping the `ripples` service
stops it for every cauldron.

**Who can turn it on.** The owner acts' gate guards the command, not the
file: anyone who can write the data root can write a setting file (and a
binding that matches the cauldron, which is computable from its
`cauldron.toml`), just as they can write `grants.json`. The setting keeps a
session or a script from turning it on through Cauldron; it is not a
defence against someone who can already write your files.

#### The cauldron's id

New in 0.1.12. `cauldron create` writes a random 128-bit id into the
cauldron's `cauldron.toml` (`id`). A rename keeps it, and it moves with the
cauldron's directory, so a data root moved whole keeps it too; no command
copies it to another cauldron. MCP grants and the
automatic ripples setting are bound to it: a cauldron made again under an
old name has another id, even when its `cauldron.toml` names the old
creation time, and is opened by neither.

A cauldron made before ids is given its id by the first writer that opens
it: the `mcp` service before a call reaches a granted cauldron, `cauldron
mcp grant`, `cauldron ripples on` or `cauldron rename`. At that moment,
and only then, its grant is carried onto the id if the grant records the
cauldron's creation time, and its automatic setting if the setting records
its creation time and its directory. Anything that does not match then is
refused, naming the command that makes it again (`cauldron mcp grant` or
`cauldron ripples on`); `cauldron mcp grants` and `cauldron mcp status`
name such a grant before its first refusal.

#### The secret scrubber

What a ripple sends to a model (always through threads; to a local model
unless `--no-scrub-local`) and every string of the model's answer before it
is written are passed through a scrubber that replaces each match with a
typed placeholder, `<redacted:kind>`, and counts it by kind in the run
journal (a `redacted` line; never the text). An answer's records are
written with the placeholders in them; a match does not refuse the batch.
It matches:

- private-key blocks (`-----BEGIN … PRIVATE KEY-----`, PGP private key
  blocks, four or five dashes, any case), whole or cut short;
- JSON Web Tokens;
- keys and tokens with well-known prefixes: Anthropic, OpenAI, AWS access
  key ids, Google API keys and OAuth tokens, GitHub, GitLab, Slack tokens
  and webhooks, Stripe secret keys, npm, PyPI, Hugging Face, SendGrid,
  DigitalOcean, Shopify;
- the password in a URL (`scheme://user:password@host`), a URL's userinfo
  that is one long token (`https://TOKEN@host`), and a secret in a URL's
  query (`?api_key=…`, `&token=…`);
- the value of an `Authorization:` or `Proxy-Authorization:` header, and of
  a `Cookie:` or `Set-Cookie:` header (the whole value, a token inside it
  included);
- a secret on a command line: the value of a long flag whose name holds a
  credential word as one of its hyphen-separated parts (`password`,
  `passwd`, `pwd`, `passphrase`, `secret`, `token`, `credential`, `apikey`,
  `api-key`, `private-key`, `access-key`, `auth-key`, `account-key`), as
  `--password x`, `--admin-password=x`, `--db-password "x"`, `--client-secret
  x`, `--access-token x` or `--storage-account-key x`, but not a flag that
  names something else after the word (`--api-key-id`, `--password-file`,
  `--password-stdin`, `--token-url`, `--secret-name`, `--token-ttl`, and the
  others the scrubber lists), nor a value that is a path; `-pVALUE` after
  `mysql` and its kin, a quoted `-p'…'` after any tool, `sshpass -p`,
  `redis-cli -a`;
- the value assigned to a name holding `SECRET`, `TOKEN`, `PASSWORD`,
  `PASSWD`, `PASSPHRASE`, `PWD`, `PASS` (as a whole part of the name),
  `CREDENTIAL`, `API_KEY`, `ACCESS_KEY`, `PRIVATE_KEY`, `AUTH_KEY` or
  `ACCOUNT_KEY`, in any case: `.env` style (`NAME=value`), lower case and
  dotted (`password=…`, `db.password=…`), with spaces (`api_key = …`, a
  value with a digit or a sign in it), quoted (`"password": "…"`, a
  passphrase of any length, a key inside it included), or YAML-ish
  (`password: …`, words to the end of the line, or one capitalised word);
- any other run of 32 or more base64 or URL-safe characters that mixes
  upper case, lower case and digits and whose character entropy is high,
  standing alone or as one segment of a path; except a digest in its
  obvious context, which is left: base64 of exactly a digest's length after
  a digest label starting a word, `sha1`, `sha224`, `sha256`, `sha384`,
  `sha512` (with or without a hyphen after `sha`) or `md5`, followed by `-`
  (`sha256-…`: Subresource Integrity, lockfiles, Nix), `=` or `=:`
  (`sha-256=…`: the HTTP `Digest` headers; also `sha256=…`, `md5=…`, and
  after a dot, `api.sha256=…`), a go.sum `h1:` hash, and the body of an
  image, font, audio or video `data:` URI. Git commit ids, UUIDs and hex digests are lower-case
  hex and never matched by this rule.

Matches that overlap or touch are replaced as one: their union, under the
outermost match's kind, with each kind inside it still counted. A match is
never dropped for overlapping another, so a token inside a `Cookie:` value
or a key inside a quoted passphrase never lets the rest of the value
through.

A value that reads as code or a stand-in is left alone: a variable or a
template (`$X`, `<…>`), a call or a type (`read_password()`, `u32`), a
dotted path (`self.token`), a number given to a name one of whose parts
counts (`TOKEN_LIMIT=4096`, `MAX_TOKENS=1000`, `tokenTimeout: 30000`; a
number of three or more digits given to `ADMIN_PASSWORD` or `PASSWORD` is
replaced).

**It reduces the risk; it is not a guarantee.** A secret in a format it does
not know, shorter than 32 characters and not given to a name or a flag it
knows, written in words outside an assignment, split across lines, made
only of hex digits, glued to the word before it, or written with look-alike
characters (a Unicode hyphen, a full-width letter) passes through, and so
does one written after a digest label with exactly that digest's length.
Its test corpus (`services/ripples/tests/it/scrub.rs`) generates samples
of every format above and misses none, and leaves a list of look-alikes
untouched (among them the flags that name something else), and touches
none of its generated digests, commit ids and UUIDs in their usual places
(it replaced 100 of those 180 samples before the digest rule); planted
beside one another and inside one another in every pairing, about 34,000
texts, it leaves no run of 8 characters of any of them (before matches
were merged, 359 of 26,593 such texts left part of a secret);
on this repository's own docs and code, which hold no secret, it replaced
nothing in about a million characters of prose and about one string per
million characters of code when measured (mostly string values given to
names like `token` in tests). It never changes the stored dump: removing a
dump is still the way to get a secret out of a cauldron.

**Its time is bounded.** Every pattern runs in time linear in the text (1
MiB of adversarial input takes well under a second), and a run gives it a
time in proportion to the text (2 seconds plus 20 per MiB). If it runs out,
nothing is sent (or, for the answer, written): the run fails
`scrub_timeout`, and automatic processing parks the dump.

### Region locks

New in 0.1.14. A ripple reads the cauldron's regions, thinks for a while
(its model call), then writes records into some of them. It locks by
region, never the whole soup: **while it thinks it holds only the regions
its dump clearly points at, often none, and while it writes it holds the
regions its records go into, for that one write**. You, your Claude Code
sessions and the curator keep writing everywhere else in the same soup.

**What a ripple holds, and when.**
- **Before its model call**, a run declares the regions its dump's text
  clearly points at. No model is asked: each region is scored against the
  dump's words (its handle and the inside of its boundary, the text before
  `Outside:`; stop words and plural, `-ing` and `-ed` endings left out).
  A shared word counts by how rare it is among the regions: 1 for a word
  only that region uses, little for one every region uses. A region is
  declared at a score of 2 or more (two words that region alone uses, or
  the like) and at least half the best region's score; at most four, and
  never more than half the regions. On the plugin's starter regions file
  (four regions) an ordinary note declares one region or none; text made
  of every region's words declares two. It holds `write` on each declared
  region and `link` on each region above it, from then until its records
  land (let go in the same act) or the run ends any other way (a park, a
  cap, a failure, `off`: let go before the outcome is written; when the
  `mcp` service cannot be reached then, the ripples service lets it go
  when it next starts or runs, and it expires meanwhile).
- **When it writes**, in the one act on the `mcp` service's writer that
  writes the records, it takes every region its records go into (and
  `link` on the regions above them), declared or not: when another holder
  holds one of them, the put is refused `region_locked` and nothing
  lands. No record lands in a region the run did not hold at that moment.
- **Once ripples have held a region for 900 seconds within the last
  hour** (`CAULDRON_RIPPLES_REGION_HOLD_S`, counted from the ripples
  journal), a run that declares it is `deferred` (`hold_budget`) before it
  takes any lock or makes any call, and tried again once the budget frees
  up, so no dump's text can keep a busy region held hour after hour. (The
  run that crosses the budget finishes: a region can be held for up to
  one run's length past it.)

A hold is a row in the soup, `region-locks.json`, with the holder (the run
and its dump), what it is for, since when and until when. A row lasts 120
seconds and the run renews it while its model call is open (and while a
run by hand waits a few seconds for a busy backend); a row past its time
holds nothing, whatever happened to its holder, and no row is held more
than 900 seconds ahead of now, whatever it says (one that says more, from
a hand edit or a clock set back, is held 900 seconds from when it was
first read so, and written back that way).

**What a session sees when it writes into a held region.** `soup_put`,
`soup_link`, `cauldron soup import`, and the owner's approve and reject are
refused at once, with code `region_locked` and nothing written. A batch
(a `soup_put` with `partial: true` too, and one approve or reject over
many records) is refused whole when any of its records touches a held
region. The message reads like this:

```
region `R-2` is locked by a ripple (run <run>, drawing records from dump DM-7)
for placing the records drawn from dump DM-7, since 2026-11-18T09:12:04Z until
2026-11-18T09:14:04Z. Nothing was written. Try again in about 95 s (the lock is
let go as soon as that write lands or gives up, and at 2026-11-18T09:14:04Z at
the latest), or, if the records belong in another region, write them there.
```

and `detail` holds the rows and `retry_after_secs`. **What to do:** wait
about `retry_after_secs` and send the same call again, or write the
records into another region if they belong there; never re-place records
only to get around a lock. There is nothing to clean up: the lock goes by
itself. Writes into any region that is not held land as before.

A held region also holds the regions below it (`narrows`): a write into a
child of a held region is refused, naming the held one. A region above a
held one still takes new records, but not a change of its own header or a
move under another parent.

**Where to see it.** `cauldron ripples status` and `cauldron mcp status`
(and `soup_status` in a session) list the held regions per cauldron;
`cauldron soup regions` (and `soup_regions`) marks a held region
`locked`, and a region above a held one `locked_below`, each with its
holder, purpose, since and until.

**When a region changes under a ripple anyway.** Every region has a head
revision, read from the store each time: it moves when the region's
handle or boundary is rewritten, the region is retired or superseded, or
it is re-parented or given a child region. A member added, moved,
superseded or closed never moves it: the model is shown each region's
id, handle and boundary, never a member. A ripple's put carries the head
revisions it read, and is refused `region_changed`, nothing written, when
one of the regions its records go into, or one it holds, moved since.
The run then reads the regions again. When nothing the model was shown
changed, it sends the same records again with no new call; otherwise it
calls the model once more, its prompt saying which regions changed and
how. A second change leaves the dump `received`, tried again at the next
scan. A call whose answer was refused so (or refused `region_locked` at
the put) counts against the cauldron's daily caps but not against the
dump's own three calls, so other writers alone can never use them up. A
region the model was shown but placed nothing in is not checked: holding
every region to its revision would make any edit anywhere stop every
ripple, which is the whole soup again.

**A damaged lock table.** A `region-locks.json` that cannot be read
(truncated, not JSON, the wrong shape, another release's format, empty)
refuses every write, `rename` and `remove`, naming the fix: stop the
`mcp` service if it runs, then `cauldron soup recover --env <env>
--cauldron <name>`. Holding the soup's writer lock, `recover` moves the
file aside to `region-locks.json.damaged-<UTC time>` (the soup then holds
no lock) only when no ripple run of the cauldron is live in the ripples
journal; with one live it refuses, naming it, and changes nothing. When
it sets the file aside its output says so (`region_locks`).

**Crashes.** A ripples service killed while a run holds regions lets them
go when it starts again, from its journal; a hold of a run whose records
landed holds nothing; every hold expires. While a region is held, `cauldron
rename` and `cauldron remove` are refused, naming the holder and until when.

**Upgrade the `mcp` service with the command line.** A 0.1.13 `mcp`
service knows nothing of region locks: a 0.1.14 ripples service will not
write through one (its runs fail `mcp_too_old`, nothing sent or written,
naming the upgrade), and a 0.1.13 command line or `mcp` service writing
the store ignores the rows (see [Known limitations](#known-limitations)).

The ripples service still runs one dump at a time, so two ripples never
contend for a region today; the locks keep a ripple and your sessions
apart.

### Clusters and claims

A **cluster** is an area of the surface of a cauldron's soup: a name and a
boundary description saying what it covers. It **claims** the regions it
covers: any cluster may claim any region, and a region may be claimed by
many clusters. Regions are the soup's own unit: a region holds nodes (each
node sits in its regions) and regions nest. A cluster holds no node and does
not nest; it groups regions, the way an area of your work spans several
topics. A claim is a statement about the surface. It is not a lease on work:
it holds nothing, assigns nothing to anyone, and never expires. What does
hold a region is a ripple's region lock, taken and let go by the ripple
itself ([Region locks](#region-locks)); no session takes one.

A cluster's `boundary` is for whoever reads the surface: you, a session
choosing where a piece of work belongs, the curator proposing a claim. No
machine reads its wording (a region's boundary, by contrast, is what each
`in_region` note is meant to quote, and what a ripple's region scoring
reads), so it is not checked beyond its length. Make a **parent region**
(`narrows`) when the nodes themselves belong together under a wider topic;
make a **cluster** when you want a named view across regions that stay
separate, or that already sit under different parents.

Clusters live in the soup's second pool, `cauldron`, on their own published
schema, `cauldron@1` ([`schema/soup/cauldron-v1/`](schema/soup/cauldron-v1/README.md)):
one node type, `cluster` (`CL-<n>`: a `handle`, a `boundary` of 40 to 1,000
characters as a region's, an open `meta`, no state), and one edge kind,
`claims`, from a cluster to a region. The workspace schema, `workspace@6`,
is unchanged.

**Write a cluster** as a record of type `cluster`, beside workspace records
if you like, with `cauldron soup import` or `soup_put`:

```json
[
  {"id": "r1", "type": "region", "handle": "Packaging",
   "boundary": "Inside: how a release is packaged and signed. Outside: how it is built (R-2)."},
  {"id": "k1", "type": "cluster", "handle": "Shipping",
   "boundary": "Inside: everything that gets a release to the people who use it. Outside: what it contains.",
   "edges": [{"kind": "claims", "to": "r1"}, {"kind": "claims", "to": "R-2"}]}
]
```

```sh
cauldron soup import shipping.json --env prod --cauldron kb --remint --dry-run
cauldron soup import shipping.json --env prod --cauldron kb --remint
```

The batch is one write, all or nothing: the region, the cluster and both
claims land together, or nothing does. A `claims` edge names a region of
the batch by its label, or a region the soup holds by its id (`R-2`, or
`R-2@ws`). A claim of a node that is not a region is refused.

**Claim, or release a claim**, one at a time:

```sh
cauldron soup claim CL-1 R-3 --env prod --cauldron kb [--note "only its install half"]
cauldron soup claim CL-1 R-3 --env prod --cauldron kb --release
```

From a session, `soup_claim {cluster, region, release?, note?}` (it needs a
grant with `write`, and is audited like every write). Both are idempotent:
claiming what is claimed prints `already-claimed`, releasing what is not
prints `not-claimed`, and neither writes. A note is at most 1,000
characters, as every edge's note now is at the door (a longer one is
refused with nothing written; one stored before this release reads back
cut to 1,000, named in `truncated`). A claim touches the region it
claims, so while a ripple holds that region it is refused `region_locked`
at once, as any write into it is ([Region locks](#region-locks)).

**Read them.** `cauldron soup region R-3` (and `soup_region`) lists the
clusters that claim the region under `clusters`. `cauldron soup nodes --type
cluster` lists the clusters, each with the regions it claims. `cauldron soup
get CL-1` answers the cluster with its `claims`. `cauldron soup search`
matches cluster handles and boundaries too. `soup_status` reports them
separately, under `clusters` with `cluster_schema: "cauldron@1"`; its
`total` and `counts` are the workspace records only, as through 0.1.15.
`cauldron soup changes` (and `soup_changes`) reports a new cluster as
`create`, a claim as `link` and a release as `unlink`, on the same cursor as
every other write. `unlink` is new in this release, and the set of `op`
values can grow again: a program reading the change feed must ignore an op
it does not know (or read that change's ids again), never fail on it.

**Waiting for a change.** `soup_changes` takes `wait_secs` (0 to 60, with
`since`): the call answers as soon as anything lands after the cursor, or
when the wait ends with nothing (the same cursor back). A write through the
`mcp` service wakes it at once, a write by another process within a second;
the grant is checked again each time it wakes, and the wait holds no lock.
A wait ends as soon as its caller hangs up. At most 16 calls wait at once,
and at most 4 of them one session's; one more answers at once with
`waited.ended: "not-waited"` and `retry_after_secs`, and the stdio bridge
then waits that long and asks again, backing off, within the call's
`wait_secs`. `cauldron soup changes --since <cursor> --wait <secs>` does the
same from a shell (it reads the store itself, so it is never refused a
slot).

**Older cauldrons and older releases.** A cauldron made by 0.1.4 to 0.1.15
has no cluster pool. It gets one on its first cluster write, as one additive
act under the writer lock: nothing in the store is migrated or rewritten. A
store with clusters still opens, reads and writes under 0.1.14 and 0.1.15,
whose ordinary reads do not see clusters (its `soup_region` answers `clusters:
[]`, its `soup_get CL-1` answers it missing, and its `soup_claim` answers
`unsupported`); its `soup_changes` lists the cluster writes by id, and its
command line reads and writes the cluster pool when asked for it by name
(`--pool cauldron`; see [Known limitations](#known-limitations)). A claim
writes no workspace node or edge; the spine records the write's bookkeeping
(which parts of it have landed) in each pool it touched, `ws.db` among
them. A ripple writes no cluster and no claim in this release: its batch is
refused if it holds one.

### Local inference

New in 0.1.11; from 0.1.17 the prebuilt bundle carries it, and `cauldron
inference models pull` fetches a model for you when you ask. It has run
whole dump ripples and the fixture evaluation on two real models
(Qwen3.6-35B-A3B at Q8_0 and gpt-oss-120b at MXFP4, llama-server build
12110).

The `inference` service runs a language model **on this machine**, with
llama.cpp's `llama-server`, for work where a smaller model is good enough
and nothing should leave the machine: summaries, curation, and turning a
dump into records (the dump ripple's `--backend local`). It is **not** for
writing code, settling a design question, or anything an agent acts on with tools:
in this release it offers a model no tools, and nothing in cauldron gives it
any. (Threads on a local model, where the model reads and edits files, are
planned for a later release.)

**What it needs, and what it never does.** llama.cpp is not bundled:
install it yourself (on macOS, `brew install llama.cpp`), and the service
uses the `llama-server` on `PATH` (looked up again at every load), or the
one named by `llama_server = "/absolute/path"` in
`~/.cauldron/data/<env>/inference/config.toml`. Each time it loads a model
it first checks that binary with `--version` and `--help` (a build number
of at least 6000, and every flag it passes), and refuses a missing or older
one with what to do. That check proves the version and the flags, not where
the binary came from. Nothing is downloaded unless you run `cauldron
inference models pull` and say yes (below): otherwise you place a `.gguf`
model file on this machine yourself. Either way the model runs with
`--offline`.

**From install to a local ripple.** Every step is yours; the installer
installs the `inference` and `ripples` services but pins and starts
neither. On a prebuilt install `cauldron --env prod build <service>`
installs the image the release shipped (no cargo); from source it compiles
it.

```sh
brew install llama.cpp                                              # llama-server, build 6000 or later (tested: 12110)
cauldron inference status --env prod                                # says what is still missing, and how to get it
cauldron --env prod build inference                                 # the service: off unless you start it
cauldron --env prod pin inference <the version label it printed>
cauldron --env prod start inference
cauldron inference models pull qwen --env prod                      # 34.37 GiB, Apache-2.0: shows where it goes, asks first
#   or a file you already have:
#   cauldron inference models add qwen ~/models/Qwen3.6-35B-A3B-Q8_0.gguf --env prod
cauldron --env prod build ripples                                   # then pin and start it the same way
cauldron --env prod pin ripples <the version label it printed>
cauldron --env prod start ripples
cauldron mcp grant kb --env prod --project <dir>                    # yourself, at a terminal (mcp runs from install)
cauldron ripples run DM-3 --env prod --cauldron kb --backend local  # a dump ripple, on this machine
cauldron inference status --env prod                                # loaded or not, memory, idle time, pid, crashes
```

`cauldron inference status` lists, under `missing`, each thing still in
the way, with what to do: the service not installed or not started, no
usable `llama-server` (none on `PATH`, or a build older than 6000: install
or upgrade llama.cpp with Homebrew), no registered model (`models pull` or
`models add`), a model whose memory estimate is over the budget, and a
budget larger than the Mac's memory.

**Getting a model: `models pull`.** `cauldron inference models pull <name>
[--yes] --env <env>` fetches one model from a catalogue built into this
release, and nothing else. The catalogue (`cauldron inference models pull
--list`) holds exactly the two models this release was measured with, each
from llama.cpp's own publisher repository on Hugging Face, pinned at a fixed
revision, with its size and sha256:

| name | file | size | licence | registered with |
|---|---|---|---|---|
| `qwen` (`qwen3.6-35b-a3b-q8`) | `ggml-org/Qwen3.6-35B-A3B-GGUF`, `Qwen3.6-35B-A3B-Q8_0.gguf` | 34.37 GiB | Apache-2.0 | context 65,536; about 36 GiB resident; a 64 GB Mac or larger |
| `gpt-oss` (`gpt-oss-120b-mxfp4`) | `ggml-org/gpt-oss-120b-GGUF`, `gpt-oss-120b-MXFP4.gguf` | 59.03 GiB | Apache-2.0 | context 32,768; about 60 GiB resident; a 96 GB Mac or larger |

Before it fetches anything it prints the size, the licence, where the file
goes (`~/.cauldron/models/<id>/`, shared by the root's environments and
never inside a project), the free disk there and the memory estimate
against the budget, and asks you to type `yes` (or takes `--yes`); away
from a terminal and without `--yes` it refuses. That yes is consent to one
download, not an owner act: `owner_acts` and `CAULDRON_OWNER_ACTS` do not
open or close it. It downloads over HTTPS
only, with macOS's own `curl`, from `huggingface.co`, which redirects the
file to its file store `us.aws.cdn.hf.co`; that redirect is the only one
followed, and it says so. Any other host is refused. The bytes go to a
private partial file beside the destination; if the transfer stops (a lost
connection, Ctrl-C), running the same command again resumes it where it
stopped. Nothing is registered until the whole file is there and its size
and sha256 are the catalogue's; then it is renamed into place and
registered as `models add` would. A file whose sha256 is wrong is not
registered and not deleted: it is set aside as `.<file>.mismatch` and the
command says where. The file is never run. At the 3.7 MB/s measured here,
Qwen takes under three hours.

**Registering a model copies nothing.** `cauldron inference models add
<name> <path> [--context <tokens>]` reads the file once for its sha256 (a
line every 4 GiB says how far it has got) and records, in
`data/<env>/inference/models.json` (owner-only), its path, its identity
(the real path with symlinks resolved, device, inode, size, and
modification and change times), its sha256 and a few facts from its
header. The header is read with every size it declares bounded, and a
header outside those bounds is refused, naming the field. Before every
load the file's identity is compared with the one recorded; if any of it
differs, the whole file is read again, and it is loaded only if its sha256
is the one registered, otherwise refused with how to register it again.
Reading a 60 GB file takes about two minutes, so that is not done at every
load unless `verify_every_load = true` is in the settings; `cauldron
inference models verify <name>` does it now. `models remove <name>` forgets
a model and leaves the file where it is.

**On demand, one model at a time.** The installer does not start the
service. Started, it loads nothing until a request names a model; then it
starts one `llama-server` child for it, on 127.0.0.1, on a port chosen at
each load. Its completions answer only a key made for that load
(`llama.key`, owner-only, removed when the child goes; any process of
yours can read it, as it can the service's door key); llama-server's
only `/health` answers without it, and says only that the child is up
(`/v1/models` needs the key too, in build 12110). A model nobody used for
15 minutes is unloaded and its memory is free again (`idle_minutes` in the
settings); `cauldron inference unload` does it now. Asking for another
model unloads the first. A child that crashes is recorded (`status` shows
its exit and the end of its log), and the next load waits 1 s, then 2, 4,
up to 60 s after repeated crashes. Stopping the service stops its child
first. If the service is killed, the guard process that holds the child
stops it within seconds; if the service and the guard are both killed, the
child runs until the service next starts, which stops it.

**The memory budget.** Before loading, the service estimates the memory a
model needs: the file's size, plus the KV cache for its context (layers ×
KV heads × (key + value width) × 2 bytes × context tokens, from the file's
header), plus 2 GiB for llama-server's buffers. Over the budget, 64 GiB
unless `memory_budget_gib` says otherwise, the model is refused, and the
refusal shows that arithmetic. The context is 65,536 tokens unless the
settings' `context` or the model's `--context` say otherwise; a smaller
context needs less memory, and a dump whose prompt does not fit is refused
by name. For example, gpt-oss-120b (a 59 GiB file) is refused at 65,536
tokens (65.53 GiB) and fits registered with `--context 32768` (63.28 GiB).
The estimate errs high: it counts every layer's cache in full, and
Qwen3.6-35B-A3B measured about 36 GiB resident against 41.37 GiB
estimated.

**A ripple, locally.** `cauldron ripples run <DM-n> --backend local [--model
<name>]` (the model defaults to the only one registered) sends the same
prompt and schema to the local model, and its answer is held to the same
schema, caps, journal and `unreviewed` marking as any ripple. Both of its
calls use the local model: there is no Opus retry, so nothing leaves the
machine. Local tokens cost no money, and the caps count them all the same:
3 calls per dump is what stops a runaway loop. The journal adds, per call,
the model, its file's sha256, the tokens and tokens per second. The
`ripples`, `mcp` and `inference` services must be running (not `threads`).

**Comparing backends.** `cauldron ripples eval --dumps <dir> --backends
threads,local --out <dir> --env <env> --cauldron <name> [--local-model
<name>] [--repeat <n>]` runs each fixture dump in `<dir>` (each `.txt` or
`.md` file is one dump) on each backend, and **writes nothing to any soup**:
every answer is judged by the `mcp` service's dry run and never landed. Into
the new `--out` directory go, per dump, each run's records and measures
side by side (`side-by-side.md`) and, for all of them, `summary.md`: schema
validity on the first try and after one retry, record count, records that
quote the dump, regions used, duplicate handles, tokens, time and tokens per
second. Which records are better is for you to judge from those files.
The threads runs are your Claude spend. With `threads` in `--backends`,
`--max-calls <n>` (at most 100) is required; the command prints the most
calls and the estimated input tokens first, and sends nothing unless you
add `--yes`. Each threads call is charged before it is sent, in the
evaluations' own ledger (`data/<env>/ripples/eval.jsonl`), with a daily
input-token cap of its own, and never against the cauldron's caps for runs.
A local-only evaluation needs neither flag. The evaluation runs in the
`ripples` service: Ctrl-C stops the command following it, and stopping the
service stops the evaluation. The `--out` directory is made owner-only.

## Known limitations

This is an early release. These are known:

- **macOS only.**
- **Nothing acts on a dump by itself unless you turn it on.** Curate a dump
  into records with `/cauldron:curate-dump`, run the dump ripple on it by
  hand (`cauldron ripples run`, see [Ripples, by hand](#ripples-by-hand)),
  or turn automatic processing on for a cauldron (`cauldron ripples on`,
  see [Ripples on their own](#ripples-on-their-own)). It is turned on for
  one cauldron, never for a name: removing, renaming or making a cauldron
  again under the name never carries it over.
- **The secret scrubber is not a guarantee.** It replaces known credential
  formats and long random strings in what a ripple sends and writes, and
  misses secrets in other shapes: do not run a ripple on a dump you know
  holds one (remove the dump instead). threads keeps the prompt of every
  ripple call (scrubbed) in its own store until you delete it. A dump
  longer than one call's input cap is not processed.
- **Automatic processing is tested with stand-ins only.** Its runs, kills,
  retries and caps are tested against a scripted backend, and its parks,
  fast failures and token counts against a fake threads service, with the
  real `mcp` door; no automatic run has been made against a real model or
  a real threads.
- **After a downgrade to 0.1.9, ripple records are not marked.** 0.1.9 reads
  a 0.1.10 store, but shows records a ripple wrote without `unreviewed`.
- **An older release ignores region locks.** A 0.1.13 command line or
  `mcp` service reads and writes a store with region locks as before, and
  its writes are not held back by them; a ripple's head-revision check
  still catches a boundary such a write changed, but not a record it added
  to a held region. Upgrade the `mcp` service with the command line: a
  0.1.14 ripples service refuses to write through a 0.1.13 one.
- **Which regions a ripple holds while it thinks is a scored word match**
  between the dump and the regions' handles and boundaries, not a reading
  of what the dump means; it has not been measured on real dumps. A dump
  it matches to no region holds nothing during its call, and its records
  are held only for the write itself.
- **A ripple writes no cluster and no claim.** Clusters and claims are
  written by you or a session (`cauldron soup import | claim`, `soup_put`,
  `soup_claim`); a ripple's batch holding one is refused, by name.
- **The terminal interface does not show a soup's clusters.** Its clusters
  are the seed corpus's (see "two stores" above).
- **An older release does not see clusters.** A 0.1.14 or 0.1.15 command
  line or `mcp` service reads and writes a store that has them, and leaves
  them out of its ordinary reads; its `soup_changes` lists their writes by id (a claim
  or a release as `update`). Asked for the pool by name, that command
  line reads clusters raw (`--pool cauldron`) and lands a cluster record
  with its claims (`soup import --pool cauldron`), judged by the pool's own
  pinned grammar, so that write is checked as this release checks it. A
  cluster record sent to a 0.1.14 or 0.1.15 `mcp` service (a `soup_put`, or
  an import it lands) is refused as an unknown type: upgrade the service with the
  command line.
- **A claim stays on the region it names.** Superseding a region does not
  move its claims: the claim stays on the retired region, the region that
  replaces it lists no clusters until they claim it, and a cluster may still
  claim the retired one. Release and claim again (`soup_claim`) to move a
  claim. Moving claims on a supersede would make every supersede a write to
  the cluster pool as well; this release leaves the two apart.
- **A cluster cannot be changed or retired.** `cauldron@1` has no
  `supersedes`, no state and no removal: a wrong handle or boundary stays.
  Write a new cluster and move the claims to it.
- **Selecting text in the terminal UI** (from 0.1.18): drag over any text
  in the cauldron face (a modal, an error, a list, a page, a setting) and
  release to send it to the terminal's clipboard (OSC 52). A drag never
  activates or changes what it starts on; a press acts only as a click
  (released where it was pressed). OSC 52 is not acknowledged, so the face
  cannot know whether the copy landed, and says so: Terminal.app does not
  accept OSC 52 (turn off View > Allow Mouse Reporting, Cmd-R, or hold Fn
  while dragging to use its own selection); iTerm2 accepts it only with
  Settings > General > Selection > "Applications in terminal may access
  clipboard" on (else hold Option while dragging); most other terminals
  accept it, and Shift held while dragging gives their own selection.
- **Going back from 0.1.18.** `cauldron --env <env> rollback threads` and
  `rollback mcp` re-pin the builds that ran before the upgrade (the
  installer's closing lines print both). Running an older release's
  installer instead moves only `mcp` back. Rename any account named by an
  address to an `[A-Za-z0-9_-]` name before going back to 0.1.17, which
  refuses address names.
- **Tests that touch the real keychain are opt-in.** The account-script
  suites `claude-accounts-prune`, `claude-accounts` and
  `add-account-login` (conformance), `prove-accounts.sh` (which also runs
  a real Claude turn per account), and the codesigning-identity tests read
  or write your login keychain; each refuses to run unless
  `CAULDRON_TEST_ALLOW_REAL_KEYCHAIN=1` is set. A threads daemon a test
  starts never runs the real `security`.
- **The prebuilt binaries are for Apple silicon only, and are not
  notarised.** They are ad-hoc signed (see [Install](#install)). On an Intel
  Mac, build from source. The prebuilt `threads` keeps the bundle's ad-hoc
  signature: `HARNESS_SIGN_SCRIPT` is not used on that path.
- **The account helper scripts are installed with cauldron (from 0.1.17).**
  `add-account`, `claude-accounts` and `claude-login-browser` ship in the
  bundle and are laid down by `scripts/install` (prebuilt or from source;
  an upgrade with `--replace-running` replaces them) at
  `<root>/share/cauldron/scripts/threads/`, every directory and script
  0755 and yours. Logging an account in from the terminal UI (the threads
  daemon runs `add-account`), rotation (`claude-accounts rotate`) and the
  account board's "make this the machine's login" action (`tui pool`, key
  `s`, which runs `claude-accounts set-live-login`) use that copy, which runs
  the installed `threads`, never `cargo`, against the environment's threads
  store. The daemon and the terminal UI run a helper only from an absolute
  path to a regular file you own that group and others cannot write, in
  directories owned by you or root that nobody else can write; anything else
  is refused, and a missing one is refused naming the install that lays it
  down. `TUI_POOL_ACCOUNTS_TOOL` still names another copy, checked the same
  way. Run by hand, an installed copy needs the store named
  (`CLAUDE_ACCOUNTS_HOME=<root>/data/<env>/threads/store`). From 0.1.18 the
  scripts run on macOS's own `/bin/bash` (3.2); no second bash is needed
  (issue #33). A new account is named by its email address (the terminal
  UI asks for it) and shown by it in full; an older account keeps its
  name, and can take its address with `threads accounts rename <old>
  <address>`, which is refused unless the slot's login records that very
  address (its login hint or the account in its `.claude.json`), and
  refused when it records none: log the slot in again under its current
  name first (issue #35). An address-named account logs in only as that
  address. A
  login from the terminal UI waits for the daemon's answer, and a login
  that stalls says what the script printed last (issue #30). The login
  still runs as a child of the threads daemon, not in the terminal UI's
  own session; moving it there is planned and not in 0.1.18. **A login
  from a prebuilt install has still not been confirmed with a real
  account by the project**: the paths are tested against stand-in
  scripts, a stand-in `claude` and a fake keychain only.
- **A dump is removed from a shell only.** `cauldron soup dump-remove
  <DM-n> --env <env> --cauldron <cauldron> --yes` erases one dump's content
  from the cauldron's directory and leaves a tombstone; it is refused while
  the `mcp` service holds the dumps store (revoke or stop it first). The
  plugin's README has the details.
- **Claude Code is the only remote inference provider that runs.** Config
  shows the other token sources, endpoints and templates greyed out, and
  refuses to act on them. The one local provider is llama.cpp's
  `llama-server`, through the `inference` service.
- **Local inference runs the dump ripple only.** Whole ripples and the
  fixture evaluation have run on two real models; threads on a local model
  are planned for a later release. A local run never falls back to Claude,
  and the evaluation reads fixture dumps from text files and does not
  measure recall or memory use. `models pull` offers only the two measured
  models, which need a Mac with 64 GB of memory or more. See [Local
  inference](#local-inference).
- **Parts of the UI read sample files, not the daemon.** These are the Status
  Mesh, Soup and Bus views and some Config views. The Threads tab, Status →
  Tokens and the threads face (`cauldron tui --chat`) are wired to the live
  threads daemon, and the Ledger reads the environment's own data. The
  Threads tab cannot start a new thread yet.
- **Cluster seeding runs a local stand-in.** The model-backed seeding call needs
  a `type-one` tool that this release does not include. Without it, cauldron
  seeds a new cluster by matching words between your boundary and the
  environment's contents, and labels the result `stand-in`.
- **The Ledger is drawn as a tree** rather than as fork-and-join rails.
- **Re-logging into a disabled Claude account does not re-enable it**, and the
  account pool can show a usage reading that is out of date. (A fix is
  planned.)
- **A large threads write-ahead log can slow the daemon's start.** A log left
  from earlier runs is folded only every five minutes, so a big one can stall
  startup. (A fix is planned.)

## Roadmap and changes

[ROADMAP.md](ROADMAP.md) lists what comes next, and [CHANGELOG.md](CHANGELOG.md)
lists what each release contains. Not baked yet, and on the roadmap: Codex
and other inference adapters, local models for more than the dump ripple
and the evaluation, encrypted cauldrons (fully
private, never leaving your machine), remote cauldrons, and meshes of them.

## License

Licensed under the [Apache License, Version 2.0](LICENSE). See [NOTICE](NOTICE).
