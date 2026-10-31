#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  THE CONFORMANCE TARGET DIRECTORY — sourced by every suite and probe in this
#  crate that asks cargo to build anything.
#
#  **WHY THIS FILE EXISTS. MEASURED, NOT SUSPECTED.**
#
#  The operator's LIVE terminal face runs the binary at
#  `<repo>/target/debug/tui`. Every suite in this crate built into exactly that
#  path, because `cargo build -p tui` with no `CARGO_TARGET_DIR` writes there.
#  So a conformance run — which MUTATES the sources, rebuilds, probes and
#  restores, several times per run — was replacing the executable of his
#  running session, repeatedly, with deliberately broken binaries.
#
#  What that costs is not obvious, so it is written down. A running process on
#  macOS keeps the inode it was launched from, so his face does NOT die when
#  the path is overwritten; the damage is what the path now POINTS AT. Between
#  a mutation and its restore, `target/debug/tui` is a binary with a claim
#  deleted or a panic hook removed, and anything that relaunches from that
#  path in that window — a supervisor, a crash restart, the operator himself —
#  starts a face built from a tree the suite was about to throw away.
#
#  Reproduced this round, with no mutation needed at all: a single plain
#  `cargo build -p tui` at the repo root moved that path from inode 185143983
#  (10499592 bytes) to inode 185158114 (9797672 bytes) while pid 46916 was
#  executing it.
#
#  **THE PROPERTY THIS FILE ENFORCES**
#
#  A conformance run must not be able to touch the binary a live face is
#  executing. Two mechanisms, and the second is the one that actually holds:
#
#    1. REDIRECT. Builds go to a SCRATCH tree under TMPDIR, never `<repo>/
#       target`. One directory shared by every suite, and removed on exit —
#       see "the scratch tree is EPHEMERAL" below for why it is not in-repo.
#
#    2. REFUSE. Redirecting is a convention, and a convention is not a
#       property — an outer `CARGO_TARGET_DIR`, a `build.target-dir` in a
#       cargo config, or a future suite that forgets to source this file can
#       all put the build back on top of a live binary. So before anything is
#       built, the resolved target directory is checked against the executable
#       path of every process on this machine, and a suite that would build
#       over a running binary REFUSES TO RUN and says whose pid it is. That
#       check does not care how the directory was chosen, which is what makes
#       it a property rather than a habit.
#
#  Sourced, not executed. The caller sets `REPO_ROOT` first:
#
#      REPO_ROOT="$(cd "$CRATE_DIR/../.." && pwd)"
#      . "$CRATE_DIR/conformance_target.sh"
#
#  and afterwards `CARGO_TARGET_DIR` is exported and safe to build into.
#
#  Exit 4 is this file's refusal, kept distinct from the suites' own 1/2/3.
# ─────────────────────────────────────────────────────────────────────────────

# ── the second target directory is not free, and this machine has no room ────
# **MEASURED THE HARD WAY.** The first cut of this file redirected the build and
# nothing else. Populating that directory filled the disk — `/System/Volumes/
# Data` went to 100%, 0 bytes free — and a machine at zero is a machine where
# the threads daemon cannot write its WAL. That is a far worse outcome than the
# defect being fixed, so the redirect ships with two mitigations and a refusal.
#
#   · NO INCREMENTAL CACHE. It is the single largest thing in a debug target
#     directory and it buys nothing here: every build in a conformance run is a
#     one-shot build of a source tree that is about to be thrown away.
#   · NO DEBUGINFO. On macOS the dev profile's unpacked debuginfo is most of the
#     remaining weight. Nothing this directory produces is ever debugged, and
#     the one check that reads a panic (`PANIC_AFTER_ALT`) reads the panic
#     MESSAGE, which the runtime prints with or without symbols.
#
# Together those take the conformance tree from gigabytes to a few hundred
# megabytes. They are set here rather than in a config file so they apply to
# every suite and cannot be forgotten by the next one.
export CARGO_INCREMENTAL=0
: "${CARGO_PROFILE_DEV_DEBUG:=0}"; export CARGO_PROFILE_DEV_DEBUG

# ── and REFUSE rather than fill his disk ─────────────────────────────────────
# A suite that cannot run is an inconvenience. A suite that takes the last
# gigabyte on the machine his daemon is writing to is an outage. So the floor
# is checked before the first build, and the refusal names the number.
# TWO FLOORS, because the two situations cost completely different amounts.
# Building the tree FROM SCRATCH pulls every dependency down again and is the
# one that filled the disk; running a suite against a tree that already exists
# relinks one binary and some test harnesses. One number for both would either
# refuse runs that were never a risk or permit the run that actually was.
# An explicitly exported CONFORMANCE_DISK_FLOOR_MB WINS over both defaults —
# the first cut of this let the fresh-tree default silently overwrite it, which
# made the guard untestable: asking for an impossible floor did nothing at all.
# A knob you cannot force to fire is not a knob, it is a decoration.
CONFORMANCE_DISK_FLOOR_EXPLICIT="${CONFORMANCE_DISK_FLOOR_MB:+yes}"
CONFORMANCE_DISK_FLOOR_MB="${CONFORMANCE_DISK_FLOOR_MB:-400}"
CONFORMANCE_DISK_FLOOR_FRESH_MB="${CONFORMANCE_DISK_FLOOR_FRESH_MB:-1200}"
conformance_disk_guard() { # <dir>
    local avail floor="$CONFORMANCE_DISK_FLOOR_MB" what="a suite run"
    if [ -z "$CONFORMANCE_DISK_FLOOR_EXPLICIT" ] && [ ! -d "$1/debug" ]; then
        floor="$CONFORMANCE_DISK_FLOOR_FRESH_MB"; what="building this tree from scratch"
    fi
    avail="$(df -k "$1" 2>/dev/null | tail -1 | awk '{print int($4/1024)}')"
    [ -n "$avail" ] || return 0            # cannot measure: do not invent a refusal
    [ "$avail" -lt "$floor" ] || return 0
    CONFORMANCE_DISK_FLOOR_MB="$floor"
    printf 'REFUSING TO RUN: not enough free disk for %s.\n' "$what"
    printf '\n'
    printf '  free on that volume : %s MB\n' "$avail"
    printf '  floor               : %s MB (CONFORMANCE_DISK_FLOOR_MB)\n' "$CONFORMANCE_DISK_FLOOR_MB"
    printf '  target directory    : %s\n\n' "$1"
    printf 'This suite builds a SECOND target tree so it never overwrites the\n'
    printf 'binary a live face is executing. Filling the disk to do that would\n'
    printf 'break the threads daemon, which is a worse failure than not running.\n'
    printf 'Free some space (`cargo clean` on a repo you are not using) and retry.\n'
    exit 5
}

# ── who is executing something inside <dir>? ─────────────────────────────────
# `ps -eo comm=` prints the FULL executable path on macOS, which is the fact we
# need; the pid is field one and the path is everything after it, so the path
# is reassembled rather than read as `$2` (a path with a space in it would
# otherwise split). A prefix compare, not a glob: the directory name is data
# and must not be read as a pattern.
conformance_live_execs() { # <dir>  ->  lines of "<pid> <path>"
    ps -eo pid=,comm= 2>/dev/null | awk -v d="$1/" '
        { pid = $1; $1 = ""; sub(/^ +/, "");
          if (substr($0, 1, length(d)) == d) print pid " " $0 }' \
        | conformance_only_ours "$1"
}

# ── …and of those, which ones could THIS suite overwrite? ────────────────────
# CAULDRON (R7.S1b). In harness the target directory was this face's own, so
# ANY live binary in it was a binary this suite might replace. In cauldron the
# release shares ONE target directory across every worktree (the arc's rule:
# disk is the binding constraint, no private target dirs), and sibling agents
# run their own test binaries in it continuously — measured: 40 refusals in a
# row over ~10 minutes, every one a sibling's `debug/deps/self_update-*`. A
# mutation here rebuilds only the face's own artifacts, so only those are a
# hazard: the `tui` binary, this crate's examples, and a `deps/` binary whose
# stem is one of the face's or `tui-render`'s targets (lib, bin, each
# `tests/*.rs`). Everything else is somebody else's file that no build here
# can touch, and it no longer blocks the suite.
#
# **PORTED from `apps/tui/conformance_target.sh` (R7.S3, HOLDS R21).** Ported
# verbatim rather than retyped — "paste, never retype" (STANDING-BLOCKS.md) —
# because it already names both crates' test directories and both stems
# (`tui`, `tui_render`), so this copy needed no edit to be correct here too.
conformance_our_stems() {
    local root="${REPO_ROOT:-}" f
    printf '%s\n' tui tui_render
    for f in "$root"/apps/tui/tests/*.rs "$root"/libs/tui-render/tests/*.rs; do
        [ -e "$f" ] || continue
        f="${f##*/}"; printf '%s\n' "${f%.rs}"
    done
}
conformance_only_ours() { # <dir>  stdin: "<pid> <path>" lines
    local stems
    stems="$(conformance_our_stems | tr '\n' ' ')"
    awk -v d="$1/" -v stems="$stems" '
        BEGIN { n = split(stems, a, " "); for (i = 1; i <= n; i++) ours[a[i]] = 1 }
        { pid = $1; path = $0; sub(/^[0-9]+ /, "", path);
          rel = substr(path, length(d) + 1);           # e.g. debug/deps/mic-0123…
          k = split(rel, seg, "/"); base = seg[k];
          if (k == 2 && base == "tui") { print; next }               # <profile>/tui
          if (k == 3 && seg[2] == "examples") { print; next }        # <profile>/examples/*
          if (k == 3 && seg[2] == "deps") {
              stem = base; sub(/-[0-9a-f]+$/, "", stem);
              if (stem in ours) { print; next }
          } }'
}

# ── the refusal ──────────────────────────────────────────────────────────────
conformance_target_guard() { # <dir>
    local dir="$1" hits
    hits="$(conformance_live_execs "$dir")"
    [ -n "$hits" ] || return 0
    printf 'REFUSING TO RUN: a LIVE process is executing a binary inside the\n'
    printf 'target directory this suite is about to build into.\n\n'
    printf '  target directory : %s\n' "$dir"
    printf '  CARGO_TARGET_DIR : %s\n' "${CARGO_TARGET_DIR:-<unset>}"
    printf '  live processes   :\n'
    printf '%s\n' "$hits" | sed 's/^/      /'
    printf '\nThis suite MUTATES sources, rebuilds, probes and restores. Building\n'
    printf 'here would repeatedly replace the executable above with binaries that\n'
    printf 'are deliberately broken. Point CARGO_TARGET_DIR somewhere else (or\n'
    printf 'leave it unset, which selects the ephemeral scratch tree) and re-run.\n'
    exit 4
}

# ── the scratch tree is EPHEMERAL ────────────────────────────────────────────
# **THE PROPERTY HAS A SECOND HALF** (operator's manager, mid-round): a
# conformance run must not be able to touch the binary a live face is executing,
# AND must not leave a large artifact behind. His disk runs near full and a
# permanent second target tree is half a gigabyte he did not agree to spend. So
# the tree is scratch and it is REMOVED on every exit path.
#
# It lives under TMPDIR rather than inside `<repo>/target`, deliberately and for
# one reason: every `target/` directory on this machine hosts a LIVE RUNNING
# BINARY — his face here, the threads daemon in substrate-interface-layer — and
# nothing in this crate may ever remove or clean one. A directory under TMPDIR
# that this file created is unambiguously not that, so the removal below can
# never be confused with cleaning a repo's target directory.
#
# The path is FIXED rather than `mktemp`-unique, so a run that was killed before
# its trap fired leaves ONE reusable tree instead of a new one every time; the
# next run adopts it (a warm cache, which is the only thing that makes this
# affordable) and removes it at its own exit.
#
# `TUI_CONFORMANCE_KEEP_TARGET=1` keeps it, for a worker running several suites
# back to back who does not want to pay a cold dependency build per suite. That
# is an explicit choice, made by a human who then owns the bytes.
conformance_target_release() {
    # **THE SOURCE MIRROR GOES WITH IT, AND IT IS CHAINED HERE ON PURPOSE.**
    # Bash traps are last-write-wins and every suite installs its own EXIT trap
    # AFTER sourcing these files — `trap 'cleanup; conformance_target_release'`.
    # A second `trap` line for the mirror would be a second thing for the next
    # suite's author to forget, so the mirror's release rides the one call that
    # is already in every trap in this crate. `conformance_mirror.sh` may not be
    # sourced at all (a suite that mutates nothing does not need it), hence the
    # existence check rather than a bare call.
    if declare -F conformance_mirror_release >/dev/null 2>&1; then
        conformance_mirror_release
    fi
    [ -n "${CONFORMANCE_TARGET_SCRATCH:-}" ] || return 0
    case "$CONFORMANCE_TARGET_SCRATCH" in */tui-conformance-target) ;; *) return 0 ;; esac
    [ -d "$CONFORMANCE_TARGET_SCRATCH" ] || return 0
    if [ -n "${TUI_CONFORMANCE_KEEP_TARGET:-}" ]; then
        printf '\n  [conformance target kept at %s — %s]\n' \
            "$CONFORMANCE_TARGET_SCRATCH" "$(du -sh "$CONFORMANCE_TARGET_SCRATCH" 2>/dev/null | cut -f1)"
        return 0
    fi
    # Never pull a tree out from under something that is executing inside it.
    if [ -n "$(conformance_live_execs "$CONFORMANCE_TARGET_SCRATCH")" ]; then
        printf '\n  [conformance target NOT removed — a process is executing inside it]\n'
        return 0
    fi
    rm -rf "$CONFORMANCE_TARGET_SCRATCH"
}

# ── resolve, export, guard ───────────────────────────────────────────────────
# An outer `CARGO_TARGET_DIR` is honoured — a caller may have a good reason —
# EXCEPT when it names the repo's default target directory, which is the live
# one and the whole point of this file. A directory a caller chose is THEIRS and
# is never removed; only the scratch path this file picks is. The guard below
# runs either way, so an honoured override that is ALSO live is refused rather
# than obeyed.
conformance_target_setup() { # <repo-root>
    local root="$1" default="$1/target"
    local scratch="${TUI_CONFORMANCE_TARGET_DIR:-${TMPDIR:-/tmp}}"
    scratch="${scratch%/}/tui-conformance-target"
    case "${CARGO_TARGET_DIR:-}" in
        ""|"$default"|"$default"/)
            CARGO_TARGET_DIR="$scratch"
            CONFORMANCE_TARGET_SCRATCH="$scratch" ;;
    esac
    export CARGO_TARGET_DIR
    mkdir -p "$CARGO_TARGET_DIR" 2>/dev/null
    conformance_disk_guard "$CARGO_TARGET_DIR"
    conformance_target_guard "$CARGO_TARGET_DIR"
    # Suites that install their own EXIT trap chain this in; this covers the
    # ones that do not, and bash's last-write-wins means the order matters.
    trap conformance_target_release EXIT INT TERM
}

conformance_target_setup "${REPO_ROOT:?conformance_target.sh: REPO_ROOT must be set before sourcing}"
