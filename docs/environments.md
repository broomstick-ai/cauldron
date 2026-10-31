# Environments — running several cauldrons side by side

An **environment** is a named cauldron: its own data root, its own seam table,
its own pins and supervision policy, its own logs, its own supervisor (one
daemon per environment, held by a kernel lock) and its own launchd label.
`prod` is the default and is exactly the layout production already occupies.

Everything here is a `cauldron` command. Nothing below asks you to edit a file
or copy a directory by hand.

## What is shared and what is not

| | where | per environment? |
|---|---|---|
| data root (service stores, seam table, pins, logs, supervision ledger) | `~/.cauldron/data/<env>/` | **yes** |
| a service's data | `~/.cauldron/data/<env>/<service>/<kind>/` | **yes** |
| threads' warnings record | `data/<env>/threads/warnings/` (prod keeps `~/.cauldron/threads.warnings/`) | **yes** |
| builds (installed versions) | `~/.cauldron/data/shared/builds/<service>/<label>/` | no — build once, pin per environment |
| build cache, model weights | `~/.cauldron/data/shared/{cache,weights}/` | no |
| declarations | `~/.cauldron/services/` over the shipped `services/` | no |
| ports | registering services (tts): kernel-assigned; hosted services (threads): **per environment, set with `pin --addr`** | yes |
| the holder's launch path (the file its supervisor runs from) | `~/.cauldron/bin/cauldron` (prod), `~/.cauldron/bin/cauldron-<env>` (every other env) | **yes** |

So a second environment shares prod's builds — pinning prod's threads build
in `dev` costs nothing — and nothing else.

## The whole lifecycle

Pick a name (`[a-z0-9][a-z0-9_-]{0,31}`; `shared` is reserved). Below it is
`dev`.

### 1. Create it

```sh
cauldron env create dev                  # empty: nothing pinned
cauldron env create dev --from staging   # SEEDED from another environment
cauldron env create dev --from prod --pins-only   # prod's versions, no data
```

`--from <env>` copies the source's **pins** (versions only — never its
`good` history, which is per environment, and never its addresses) and a copy
of every pinned service's **data directory**. Every hosted service gets a
fresh kernel-free port, printed under `hosted ports`. It is **refused while
anything in the source is running**: a copy of a store a live process is
writing is a torn copy. To seed from `prod` without stopping it, use
`--pins-only` (pins, fresh ports, no data).

`cauldron env show dev` prints where everything lives and whether a
supervisor holds it. `cauldron envs` lists every environment side by side.

### 2. Pin what it runs

```sh
cauldron versions threads                                  # what is installed
cauldron --env dev pin threads <label> --addr 127.0.0.1:4653
cauldron --env dev pin tts <label>
```

A service runs in an environment **if and only if it is pinned there.**

`--addr` is for a **hosted** service (one that cannot publish its own port —
`threads`). Its declaration names prod's port (3653), so a second environment
must name its own. `pin` **refuses** a port any other environment in this root
claims — prod included — whether or not that environment is running right
now, and `--addr` on a registering service (tts) is refused, because it binds a
kernel-assigned port and cannot collide.

To try a NEW build in `dev` first: `cauldron build threads` (from a clean
checkout), then `cauldron --env dev pin threads <new label>`. Prod keeps its
own pin; nothing moves there until you pin it.

### 3. Run it

```sh
cauldron --env dev daemon        # the supervisor, in the foreground
```

**A non-prod holder runs from its own file** (R6.S6). `cauldron --env dev
daemon`, typed at any cauldron binary, EXECS `~/.cauldron/bin/cauldron-dev`
before it takes the hold, and the first time it places that file there (a
copy of the binary you typed). The holder's self-update watches the file it
runs from, so it exits only when THAT file changes: a deploy of prod's
`~/.cauldron/bin/cauldron` no longer stops it. To give `dev` a new cauldron:

```sh
/path/to/new/cauldron --env dev promote   # copy THIS binary onto bin/cauldron-dev
cauldron --env dev daemon                 # a hand-run holder exited on the promote; start it again
```

`promote` refuses `prod` (prod's file is placed by the signed deploy, and its
holder behaves exactly as before). The copy is byte-for-byte, so a code
signature on the source is on the copy too: `codesign -d -r-
~/.cauldron/bin/cauldron-dev` reads the same designated requirement. `install`
places the same file (it always has), so a launchd job and a hand-run holder
of one environment run the same file.

**What counts as "that file changed"** (R6.S6b; every holder, prod included).
A different file at the path (new inode: every rename-into-place, so every
`promote`, `install` or `mv`) or a new size or mtime is a change. `promote`'s
copy keeps the SOURCE's mtime, so mtime alone used to miss a promote from a
source with the same mtime; the inode now catches it. When only the ctime
moved (an in-place rewrite that put the mtime back, or a `chmod`), the holder
reads the file and compares its sha256 with the one it took at start: new
bytes are a change, a metadata-only change is not. The exit's log line and its
`supervisions` row say which of these it was.

The daemon holds `dev` and launches everything `dev` pins, each with its own
data directory and port, beside prod's daemon. To keep it alive across logins
instead: `cauldron --env dev install` (label `com.broomstick.cauldron-dev`).
A new environment's policy is `auto_restart = false`: its daemon launches
each pinned service ONCE when it starts, and after that reports a death
rather than relaunching it. Create it with `--auto-restart` for prod-style
supervision (relaunch, backoff, and launchd revival).

```sh
cauldron --env dev status        # its services, probed
cauldron --env dev supervisions  # what its supervisor did, and why
cauldron --env dev tui           # the face, pointed at dev's threads
cauldron --env dev tui --resolve # which address the face would use, and why
```

The TUI refuses to open in a non-prod environment whose threads resolves to
prod's address.

The face a router runs is found beside its own binary: `tui-<env>` first for a
non-prod environment, then plain `tui`. `cauldron --env <e> install` puts the
TUI at `<root>/bin/tui-<e>` (prod's is `<root>/bin/tui`, and only a prod
install replaces it), so installing a non-prod environment never changes prod's
face (R6.S8). `promote` does not touch any TUI.

### 4. Stop it (and keep it)

```sh
cauldron --env dev down          # stop dev's holder, then every service it runs
cauldron --env dev daemon        # later: bring it back, on the same data
```

`down` stops the holder first (so it cannot relaunch what is being stopped),
takes the environment's hold so no new holder starts meanwhile, stops every
service through the normal stop path, and re-probes that nothing is live. The
data root, `env.toml` (pins, ports), the declarations and the builds are left
exactly where they were; `envs` still lists `dev`. A second `down` on a
stopped environment says so and exits 0. It is the same sequence
`env remove --stop` runs, minus the delete.

Do not stop a holder with `kill -TERM`: **a holder does not stop its services
on TERM** (deliberately — hosted services outlive a holder restart, which is
how a self-updating holder hands over to its successor), so the services are
orphaned and keep serving (R6.S4 F2). Use `down`.

`down` **refuses `prod`**, and any environment installed under launchd, unless
`--supervised` is given: launchd's KeepAlive revives the holder at once, so on
an installed environment `down` is a restart of every service rather than a
stop. For a lasting stop of an installed environment run
`cauldron --env dev uninstall` first.

### 5. Tear it down

```sh
cauldron --env dev uninstall              # only if you installed it
cauldron env remove dev                   # DRY RUN: what would go
cauldron env remove dev --stop --yes      # stop holder + services, delete data root
```

`env remove` refuses `prod` by name, refuses an environment still installed
under launchd (its KeepAlive would revive what it stops), and without
`--stop` refuses an environment with anything running. With `--stop` it
signals the holder first (so it cannot relaunch what is being stopped), takes
the environment's hold itself, stops every service through the normal stop
path, re-probes that nothing is live, and only then deletes
`~/.cauldron/data/dev/`. Builds and every other environment are untouched.

## The guards, and what they refuse

* **A data root that resolves anywhere but `data/<env>/`** — e.g. a symlink
  `data/dev -> data/prod` — is refused by every verb at resolution, before
  anything is read or written through it. The refusal names production when
  the link lands there.
* **A hosted port another environment claims** — refused at `pin`, and again
  at launch (before the fork) if `env.toml` was edited by hand.
* **Something already listening** on a hosted service's address — refused at
  launch, before the fork (R6.S1).
* **A second supervisor for one environment** — refused by the kernel lock.
* **`--env shared`** — reserved; refused.

## What is not done yet

* Seeding from a RUNNING environment (prod) with data needs an online copy of
  threads' store (sqlite backup + a quiesced turns directory). Today the
  answer is `--pins-only`.
* The launchd label is per environment NAME, not per root: two roots each
  with an environment called `dev` would share a label if both were installed.
