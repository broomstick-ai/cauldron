#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  THE CONFORMANCE SOURCE MIRROR — sourced by every suite in this crate that
#  MUTATES A SOURCE FILE to prove a check can still detect its own defect.
#
#  **THE PROPERTY THIS FILE ENFORCES, AND IT IS THE ONE `conformance_target.sh`
#  DOES NOT:**
#
#      A suite that mutates source must leave the source UNMUTATED on EVERY
#      exit path, INCLUDING BEING KILLED.
#
#  **WHY. MEASURED, NOT SUSPECTED — this happened on 2026-09-22.**
#
#  A worker running `transport_conformance_test.sh`'s §3 was killed between its
#  `mutate` and its `restore`. The suite's `trap cleanup EXIT INT TERM` never
#  ran, because `SIGKILL` is not deliverable to a handler. What it left on disk
#  was `src/transport.rs:919` reading
#
#      .and_modify(|ours| *ours = *ours)
#
#  where the shipped code is `.and_modify(|ours| *ours = (*ours).min(c.last_seq))`
#  — the repeat-over-drop bias of the lag-recovery cursor plan, deleted. The
#  comment two lines above it says what that costs: *"a repeat is caught by the
#  gate below, a drop is caught by nobody."* The tree then sat there, poisoned,
#  as the baseline the NEXT worker would measure and report as truth.
#
#  **THAT SCRIPT IS RETIRED (R7.S3, HOLDS R22)** — its target, `src/transport.rs`'s
#  hand-rolled resync/repair machinery, was itself replaced by `seam::link`. The
#  incident above is kept because it is the reason THIS file's property exists,
#  and that reason does not depend on which script is doing the proving.
#
#  **AND THE TWO DEFECTS ARE ONE DEFECT WITH TWO FACES.** `0103` bounded
#  `probe.py`'s unbounded `waitpid` because a suite could hang forever. A suite
#  that can hang forever is a suite that WILL be killed, and being killed is
#  exactly how a mutation gets left behind. Bounding the hang removed the most
#  common reason to reach for `kill -9`; this file removes the damage that
#  reaching for it does.
#
#  # Why a trap is not the answer, and why this is
#
#  The obvious repair is a wider trap — `trap restore EXIT INT TERM HUP QUIT`.
#  It is strictly better than nothing and it is still not the property: no
#  handler catches `SIGKILL`, no handler runs when the kernel reaps a process
#  during a machine stall, and this machine locked up twice on the day the
#  incident happened. A guarantee that has a hole in exactly the failure mode
#  that produced the incident is not a guarantee.
#
#  So the mutation is applied to a COPY and the copy is what gets built. The
#  working tree is never written to AT ALL by a suite that sources this file,
#  which makes the property structural rather than procedural: there is no exit
#  path to get wrong, because there is nothing to undo. `git diff` over the
#  crate is the proof, and it is a proof that holds under `SIGKILL` because the
#  bytes the suite could damage were never the tree's bytes.
#
#  # What the mirror contains
#
#  The WORKSPACE, and only the workspace: the root `Cargo.toml` / `Cargo.lock`,
#  `.cargo/`, any `rust-toolchain*`, and every workspace member directory that
#  `cargo metadata` names — apps/tui and apps/tui/render among them. Not
#  `.git`, not `target`, and not the 276 MB of `apps/aui-ios` that is not a
#  cargo member. That is about 10 MB of source, and on APFS `cp -c` CLONES it:
#  the copy shares blocks with the original and costs ~0 bytes and ~0.3 s. A
#  machine at 98% disk was the other standing hazard of that day, so the cheap
#  copy is not an optimisation, it is a precondition.
#
#  # How a suite uses it
#
#  Source it AFTER `CRATE_DIR` / `REPO_ROOT` are computed and BEFORE any source
#  path is derived from them:
#
#      CRATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
#      REPO_ROOT="$(cd "$CRATE_DIR/../.." && pwd)"
#      . "$CRATE_DIR/conformance_target.sh"
#      . "$CRATE_DIR/conformance_mirror.sh"
#      TRANSPORT_RS="$CRATE_DIR/src/transport.rs"     # <- now in the mirror
#
#  It REWRITES `REPO_ROOT` and `CRATE_DIR` in place to their mirrored paths and
#  changes the working directory to the mirrored equivalent of wherever the
#  caller already was — so every `cd "$REPO_ROOT"`, every `"$CRATE_DIR/src/x.rs"`
#  and every bare `cargo test -p tui` below it lands in the mirror with no
#  further edit. `CONFORMANCE_MIRROR_REAL_ROOT` keeps the real path for the one
#  thing that still needs it (the live-binary guard's idea of `<repo>/target`),
#  and `CONFORMANCE_MIRROR` names the copy.
#
#  A suite's OWN backup-and-restore machinery is deliberately left in place. It
#  now protects the mirror instead of the tree, which is harmless, and it keeps
#  each suite's §"the sources are back as they were" check meaningful — that
#  check now proves the suite's own restore logic still works, on a copy where
#  a failure costs nothing.
#
#  Exit 6 is this file's refusal, kept distinct from the suites' own 1/2/3 and
#  from `conformance_target.sh`'s 4/5.
# ─────────────────────────────────────────────────────────────────────────────

# ── one mirror per RUNNING SUITE ─────────────────────────────────────────────
# The path carries the pid, for one reason: two suites running at once must not
# mutate each other's copy, and a single fixed path would let them. The cost of
# that choice is that a KILLED run leaks its directory — which is the whole
# situation this file exists for — so every setup first SWEEPS the mirrors
# whose pid is no longer alive. A leaked mirror is ~0 bytes of real disk (the
# clone shares blocks with a tree that still exists), so the sweep is tidiness
# rather than a safety measure. It is emphatically NOT a `target/` directory
# and the removal below can never be confused with cleaning one.
conformance_mirror_sweep() { # <parent>
    local d pid
    for d in "$1"/tui-conformance-mirror.*; do
        [ -d "$d" ] || continue
        pid="${d##*.}"
        case "$pid" in ''|*[!0-9]*) continue ;; esac
        [ "$pid" = "$$" ] && continue
        kill -0 "$pid" 2>/dev/null && continue   # still running: leave it alone
        rm -rf "$d"
    done
}

conformance_mirror_release() {
    [ -n "${CONFORMANCE_MIRROR:-}" ] || return 0
    case "$CONFORMANCE_MIRROR" in */tui-conformance-mirror.*) ;; *) return 0 ;; esac
    [ -d "$CONFORMANCE_MIRROR" ] || return 0
    if [ -n "${TUI_CONFORMANCE_KEEP_MIRROR:-}" ]; then
        printf '\n  [conformance source mirror kept at %s]\n' "$CONFORMANCE_MIRROR"
        return 0
    fi
    # Never pull the tree out from under something executing inside it, for the
    # same reason `conformance_target_release` does not.
    if command -v conformance_live_execs >/dev/null 2>&1 &&
       [ -n "$(conformance_live_execs "$CONFORMANCE_MIRROR")" ]; then
        printf '\n  [conformance mirror NOT removed — a process is executing inside it]\n'
        return 0
    fi
    # cwd may be inside it; leaving it removed but current is a trap for the
    # rest of the shutdown path.
    cd "${CONFORMANCE_MIRROR_REAL_ROOT:-/}" 2>/dev/null || cd / || true
    rm -rf "$CONFORMANCE_MIRROR"
}

# ── the member list is asked for, never guessed ──────────────────────────────
# `cargo metadata --no-deps` is the authority on what a build needs, exactly as
# `mic_conformance_test.sh` asks it where the target directory is rather than
# assuming. A hand-written list would rot the first time a crate is added, and
# it would rot SILENTLY — the mirror would still build, against a workspace
# missing a member, and the failure would look like a code defect.
#
# Nested members are dropped (apps/tui/render is inside apps/tui): copying both
# would put a second `render/` inside the first. Measured while building this.
conformance_mirror_members() { # <real-root>  -> relative dirs, one per line
    cargo metadata --no-deps --format-version 1 --offline \
        --manifest-path "$1/Cargo.toml" 2>/dev/null |
    python3 -c '
import json, os, sys
root = sys.argv[1]
try:
    meta = json.load(sys.stdin)
except Exception:
    sys.exit(7)
dirs = sorted({os.path.relpath(os.path.dirname(p["manifest_path"]), root)
               for p in meta.get("packages", [])})
keep = []
for d in dirs:
    if any(d == k or d.startswith(k + os.sep) for k in keep):
        continue          # already inside a directory we are copying whole
    keep.append(d)
print("\n".join(keep))
' "$1"
}

conformance_mirror_setup() { # <real-repo-root>
    local root="$1" parent mirror rel d
    parent="${TUI_CONFORMANCE_TARGET_DIR:-${TMPDIR:-/tmp}}"; parent="${parent%/}"
    conformance_mirror_sweep "$parent"
    mirror="$parent/tui-conformance-mirror.$$"
    rm -rf "$mirror"                  # a fresh copy EVERY run: a mirror left
    mkdir -p "$mirror" || {           # behind by a kill is never adopted, so a
        printf 'REFUSING TO RUN: could not create the source mirror at %s\n' "$mirror"
        exit 6                        # stale mutation cannot become a baseline.
    }

    local members
    members="$(conformance_mirror_members "$root")"
    if [ -z "$members" ]; then
        printf 'REFUSING TO RUN: `cargo metadata` named no workspace members for\n'
        printf '  %s\n\n' "$root"
        printf 'This suite MUTATES sources and mirrors the workspace so it never\n'
        printf 'writes to the working tree. Without a member list the mirror would\n'
        printf 'be incomplete, and building it would fail in a way that looks like\n'
        printf 'a code defect. Fix cargo, or run with the tree you meant.\n'
        rm -rf "$mirror"
        exit 6
    fi

    # `cp -c` is an APFS CLONE — copy-on-write, so ~10 MB of source costs ~0
    # bytes and ~0.3 s. `-c` is macOS-only and fails loudly on a volume that
    # cannot clone, so a plain recursive copy is the fallback rather than an
    # error: correctness first, cheapness when the filesystem allows it.
    while IFS= read -r d; do
        [ -n "$d" ] || continue
        mkdir -p "$mirror/$(dirname "$d")"
        cp -c -R "$root/$d" "$mirror/$d" 2>/dev/null ||
            cp -R "$root/$d" "$mirror/$d" || {
                printf 'REFUSING TO RUN: could not mirror %s\n' "$d"
                rm -rf "$mirror"; exit 6; }
    done <<EOF
$members
EOF
    for f in Cargo.toml Cargo.lock rust-toolchain rust-toolchain.toml clippy.toml rustfmt.toml; do
        [ -f "$root/$f" ] || continue
        cp -c "$root/$f" "$mirror/$f" 2>/dev/null || cp "$root/$f" "$mirror/$f"
    done
    # (R7.S1b copied the repo's `data/` here; REL-3.S2 deleted it — the tests'
    # seed is `apps/tui/tests/fixtures/seed/`, inside the member, mirrored above.)
    [ -d "$root/.cargo" ] && { cp -c -R "$root/.cargo" "$mirror/.cargo" 2>/dev/null ||
                               cp -R "$root/.cargo" "$mirror/.cargo"; }

    CONFORMANCE_MIRROR_REAL_ROOT="$root"
    CONFORMANCE_MIRROR="$mirror"
    export CONFORMANCE_MIRROR_REAL_ROOT CONFORMANCE_MIRROR

    # Re-point the caller. These are the two names every suite in this crate
    # derives its source paths and its `cd`s from; rewriting them here is what
    # makes the change two lines per suite instead of thirty.
    rel="${CRATE_DIR#"$root"/}"
    [ "$rel" = "${CRATE_DIR:-}" ] && rel="apps/tui"
    CRATE_DIR="$mirror/$rel"
    REPO_ROOT="$mirror"
    [ -n "${ROOT:-}" ] && ROOT="$CRATE_DIR"     # mic_conformance_test.sh's name

    # And follow the caller's cwd across, so a suite that was already sitting in
    # its crate directory (mic) keeps its relative `src/mic.rs` working.
    rel="${PWD#"$root"/}"
    if [ "$rel" != "$PWD" ] && [ -d "$mirror/$rel" ]; then
        cd "$mirror/$rel" || exit 6
    elif [ "$PWD" = "$root" ]; then
        cd "$mirror" || exit 6
    fi

    trap conformance_mirror_release EXIT INT TERM HUP QUIT
}

conformance_mirror_setup "${CONFORMANCE_MIRROR_REAL_ROOT:-${REPO_ROOT:?conformance_mirror.sh: REPO_ROOT must be set before sourcing}}"
