#!/usr/bin/env bash
# BUILD AND DEPLOY the threads daemon. Build, sign, verify — in that order, and
# the signature is what tells the running daemon to absorb the build.
#
# ────────────────────────────────────────────────────────────────────────────
# THERE IS A GENERAL FORM OF THIS NOW, AND IT IS THE ONE TO USE.
#
#     core-plugins/core/tools/promote publish threads
#     core-plugins/core/tools/promote status  threads
#
# in the harness repo (`~/code/harness`). One promotion cycle for EVERY local
# service rather than one script per binary, and it does three things this
# script does not: it refuses to build a tree whose COMPILED INPUTS are dirty
# (schema.sql is compiled in with include_str!), it says whether the daemon
# actually QUEUED the absorb — exit 10 if it did not, which is the silent
# failure this whole mechanism exists around — and it CLEANS UP the build
# scratch, with a ten-minute TTL sweep that survives hundreds of parallel
# worktrees.
#
# Read the skill `core:signed-service-deploy` for the whole story.
#
# THIS SCRIPT STILL WORKS and is still correct; it is the threads-only form of
# the same cycle, and the service confs and docs that name it are not wrong.
# Nothing here changed.
#
# ────────────────────────────────────────────────────────────────────────────
# WHY THIS SCRIPT EXISTS
#
# Operator, verbatim:
#
#   "In terms of who builds the binary, whoever affected the change needs to
#    build the binary. It's that simple. … someone needs to indicate that this
#    build needs to get absorbed. … Whoever did the change does the build.
#    That's separate from making a compile, though. CHECKING THAT IT COMPILES
#    NEEDS TO BE RESERVED AS A NO-OP IN TERMS OF RESTARTING THE DAEMON."
#
#   "instead of moving it to a new binary, I actually think you need to do the
#    same thing that AUI is doing by signing the binary, ideally with the same
#    thing that's doing the signing for AUI."
#
# So: `cargo build` and `cargo check` are compile checks and NOTHING ELSE. Many
# agents share this tree and run them constantly; before this, every one of
# those was a deploy, because the live daemon watched target/debug/threads for
# a content change and restarted into whatever landed there.
#
# THIS script is the deploy. It signs with the "harness-codesign" identity, and
# the daemon's watcher (lib/threads/src/update.rs — `absorbable_identity`) will
# only act on an image whose designated requirement names com.harness.threads.
# Only the holder of that certificate's private key can produce one, so cargo
# cannot forge the signal even by accident: cargo's linker leaves an AD-HOC
# signature whose DR is a bare `cdhash H"…"` naming no identifier at all.
#
# Modelled on harness/scripts/build-ui-server.sh, and it calls that tree's
# sign-ui-server.sh rather than re-implementing codesign — same signer, same
# certificate, per the operator's "ideally with the same thing".
#
# ONE DIFFERENCE FROM AUI, DELIBERATE: the AUI daemon ALSO signs any build it
# observes, defensively, because an unsigned AUI silently loses the operator's
# microphone grant. threads must NOT do that. Here the signature is not a safety
# property (this binary holds no TCC grants) — it is the human's statement of
# intent to deploy, and a daemon that signed its own binary would be inventing
# that intent for someone who only wanted to know whether it compiled.
#
# ────────────────────────────────────────────────────────────────────────────
# USAGE
#
#   lib/threads/scripts/build-threads.sh [extra cargo build args, e.g. --release]
#
# Honours CARGO_TARGET_DIR (so a scratch build can be proven without touching
# the path the live daemon watches) and HARNESS_SIGN_SCRIPT (so the coupling to
# the harness checkout is one overridable variable rather than a buried path).
#
# EXIT CODES
#   0 — built, signed, signature verified, `--self-check` answered. In
#       identity mode that is a deploy: the running daemon absorbs it at its
#       next quiet moment. In ad-hoc mode it is a runnable build, NOT a deploy.
#   3 — ad-hoc build landed on the path a live daemon watches: NOT deployed.
#   non-zero — anything else. NOTHING was deployed, loudly.

set -uo pipefail

# lib/threads/scripts → lib/threads → lib → <repo>
THREADS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# REPO_ROOT follows the PHYSICAL scripts dir (`cd -P`/`pwd -P`): under cauldron
# this script is reached as <data-home>/scripts/<name>, where `scripts` is a
# SYMLINKED DIR into services/threads/scripts and <data-home> is outside any repo.
# The logical path put REPO_ROOT above the data home (H179: "not a git
# repository" on every board sign-in). THREADS_DIR stays the invoking home on
# purpose: it is the data dir (threads.db, accounts) the caller meant.
REPO_ROOT="$(cd -P "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd -P)"

# The identifier the daemon matches on. Keep in sync BY HAND with
# lib/threads/src/update.rs `STABLE_BUNDLE_ID`; the verify step below is what
# catches it if they ever drift, which is why the verify is not optional.
BUNDLE_ID="com.harness.threads"

# THE SIGNER (REL-4.S3). Two modes, chosen by ONE variable:
#
#   HARNESS_SIGN_SCRIPT set    -> IDENTITY mode: that script signs (the AUI's
#                                 signer, reused whole, on the operator's
#                                 machine: harness/scripts/sign-ui-server.sh),
#                                 and the verify below requires the designated
#                                 requirement to NAME ${BUNDLE_ID}. This is the
#                                 only mode that is a DEPLOY to a running daemon.
#   HARNESS_SIGN_SCRIPT unset  -> AD-HOC mode (the default, for everyone else):
#                                 `codesign --force -s - --identifier ...`. The
#                                 image runs and carries the identifier, but its
#                                 DR is a bare cdhash, so a running daemon's
#                                 absorb path IGNORES it — said loudly below.
#
# The default used to be a literal path into the operator's home directory, so
# on any other machine this script died at "no such file". The operator keeps
# his signer by exporting HARNESS_SIGN_SCRIPT (see the REL-4.S3 report).
SIGN_SCRIPT="${HARNESS_SIGN_SCRIPT:-}"
if [[ -n "${SIGN_SCRIPT}" ]]; then
    SIGN_MODE="identity"
    SIGNER_DESC="${SIGN_SCRIPT}"
else
    SIGN_MODE="adhoc"
    SIGNER_DESC="ad-hoc (codesign -s -; set HARNESS_SIGN_SCRIPT to sign with an identity)"
fi

# `--sign-only <binary>`: skip the build, sign and verify an existing image,
# and stop (no --self-check, no live-path check). Used by the conformance test
# and by anyone signing a binary built elsewhere.
SIGN_ONLY=""
if [[ "${1:-}" == "--sign-only" ]]; then
    [[ -n "${2:-}" ]] || { echo "[build-threads] usage: --sign-only <binary>" >&2; exit 2; }
    SIGN_ONLY="$2"
    shift 2
fi

PROFILE="debug"
for a in "$@"; do
    if [[ "$a" == "--release" ]]; then
        PROFILE="release"
    fi
done

TARGET_DIR="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}"
BINARY="${SIGN_ONLY:-${TARGET_DIR}/${PROFILE}/threads}"

echo "[build-threads] repo      ${REPO_ROOT}"
echo "[build-threads] binary    ${BINARY}"
echo "[build-threads] signer    ${SIGNER_DESC}"
echo "[build-threads] mode      ${SIGN_MODE}"
echo "[build-threads] bundle id ${BUNDLE_ID}"
echo

# ── 1. compile ──────────────────────────────────────────────────────────────
# In place, straight to the watched path, on purpose. Staging the build
# somewhere else and moving it in after signing would need a second full target
# tree (this workspace already turned off incremental debug info over disk
# pressure — see .cargo/config.toml), and it buys nothing: the window between
# "cargo wrote an unsigned binary here" and "we signed it" is exactly the state
# the watcher now ignores by construction. That is the point of gating on the
# signature instead of on the bytes.
if [[ -z "${SIGN_ONLY}" ]]; then
    echo "[build-threads] cargo build -p threads $*"
    ( cd "${REPO_ROOT}" && cargo build -p threads "$@" ) || exit $?
fi

[[ -f "${BINARY}" ]] || {
    echo "[build-threads] ERROR: cargo reported success but there is no binary at ${BINARY}" >&2
    exit 1
}

echo
echo "[build-threads] identity BEFORE signing:"
/usr/bin/codesign -dvvv "${BINARY}" 2>&1 | grep -E '^(Identifier|CDHash|Signature)=' | sed 's/^/    /'

# ── 2. sign ─────────────────────────────────────────────────────────────────
# Do NOT let a signing failure pass through. A binary that built but did not
# sign is not deployed — it will sit at the watched path being ignored forever,
# and the operator would be left wondering why his change never went live. That
# silent outcome is the entire failure mode this script exists to prevent, so it
# has to be loud.
echo
echo "[build-threads] signing (${SIGN_MODE})…"
if [[ "${SIGN_MODE}" == "identity" ]]; then
    "${SIGN_SCRIPT}" "${BINARY}" "${BUNDLE_ID}"
    sign_status=$?
else
    /usr/bin/codesign --force --sign - --identifier "${BUNDLE_ID}" "${BINARY}"
    sign_status=$?
fi
if [[ ${sign_status} -ne 0 ]]; then
    echo "" >&2
    echo "[build-threads] ERROR: build succeeded but signing did NOT verify" \
         "(${SIGN_SCRIPT##*/} exit ${sign_status}). The binary at ${BINARY} is NOT" \
         "signed for deploy — the running daemon will IGNORE it and keep serving" \
         "the code it is already running. Nothing was deployed." >&2
    if [[ "${SIGN_MODE}" == "identity" && ${sign_status} -eq 2 ]]; then
        echo "[build-threads] exit 2 means the 'harness-codesign' identity is not in" \
             "the keychain. Run harness/scripts/setup-codesign.sh once." >&2
    fi
    exit "${sign_status}"
fi

# ── 3. verify the END STATE ─────────────────────────────────────────────────
# Independently of what the sign script just claimed, and using the SAME
# question the daemon asks (update.rs::is_stably_signed): does the designated
# requirement actually name our identifier? This is the check that catches
# BUNDLE_ID drifting away from STABLE_BUNDLE_ID, a signer that no-op'd, or a
# certificate that changed underneath us.
echo
echo "[build-threads] verifying:"
/usr/bin/codesign --verify --verbose=2 "${BINARY}" 2>&1 | sed 's/^/    /'
verify_status=${PIPESTATUS[0]}

dr="$(/usr/bin/codesign -d -r- "${BINARY}" 2>&1)"
# Match `designated` loosely on purpose: codesign prints an ad-hoc requirement
# as `# designated => cdhash H"…"` but a certificate-backed one as
# `designated => identifier "…" and certificate leaf = H"…"`, with no `# `.
# Anchoring on `^# designated` silently printed nothing for exactly the case
# the operator most needs to see.
echo "${dr}" | grep 'designated =>' | sed 's/^/    /'

if [[ "${SIGN_MODE}" == "identity" ]]; then
    if [[ ${verify_status} -ne 0 ]] || ! grep -q "${BUNDLE_ID}" <<<"${dr}"; then
        echo "" >&2
        echo "[build-threads] ERROR: ${BINARY} does not verify as ${BUNDLE_ID}" \
             "(codesign --verify exit ${verify_status}). NOT deployed." >&2
        exit 1
    fi
else
    # Ad-hoc: the DR is a bare cdhash by construction, so the end state to
    # verify is "valid signature, ad-hoc, carrying our identifier".
    info="$(/usr/bin/codesign -dvvv "${BINARY}" 2>&1)"
    if [[ ${verify_status} -ne 0 ]] || ! grep -q '^Signature=adhoc' <<<"${info}" \
        || ! grep -q "^Identifier=${BUNDLE_ID}\$" <<<"${info}"; then
        echo "" >&2
        echo "[build-threads] ERROR: ${BINARY} is not a valid ad-hoc signature with" \
             "identifier ${BUNDLE_ID} (codesign --verify exit ${verify_status})." >&2
        exit 1
    fi
    echo "[build-threads] AD-HOC signed. This image RUNS, but it is NOT a deploy to a"
    echo "[build-threads] running daemon: the absorb path requires a DR naming"
    echo "[build-threads] ${BUNDLE_ID}, which only an identity signature has."
    echo "[build-threads] Set HARNESS_SIGN_SCRIPT to deploy-sign."
fi

if [[ -n "${SIGN_ONLY}" ]]; then
    echo "[build-threads] --sign-only: signed and verified ${BINARY}; stopping here."
    exit 0
fi

echo
echo "[build-threads] identity AFTER signing (this is what the daemon absorbs):"
/usr/bin/codesign -dvvv "${BINARY}" 2>&1 | grep -E '^(Identifier|CDHash|Authority)=' | sed 's/^/    /'

# ── 4. prove it runs ────────────────────────────────────────────────────────
# The same gate the daemon's own preflight applies before it will exec a
# candidate (update.rs `self_check`). Running it here means the operator finds
# out at build time, in front of the terminal he is already looking at, rather
# than as a `refused` line in restarts.jsonl an hour later.
echo
banner="$("${BINARY}" --self-check 2>&1)"
self_status=$?
if [[ ${self_status} -ne 0 ]] || [[ "${banner}" != *"threads self-check ok"* ]]; then
    echo "[build-threads] ERROR: the signed binary failed --self-check (exit ${self_status}):" >&2
    echo "    ${banner}" >&2
    echo "[build-threads] It is signed but it does not run. The daemon's preflight would" \
         "refuse it and restore the last-good copy. Fix it before walking away." >&2
    exit 1
fi
echo "[build-threads] --self-check: ${banner}"

# ── 4. did this land where the live daemon is actually watching? ────────────
#
# This used to be `[[ "${BINARY}" == "${REPO_ROOT}/target/debug/threads" ]]`, and
# that literal `debug` was a latent lie. The daemon watches `current_exe()` —
# whatever path the SUPERVISOR launched it from — and that path is a variable
# (`BINARY` in `services/threads.conf`, built out of `paths.env`), not a
# constant. The moment the operator points the service at an optimised build,
# the old test would have called a correct `--release` deploy "NOT the path the
# live daemon watches" and blamed `CARGO_TARGET_DIR` for it, which would have
# been wrong twice in one sentence.
#
# So ASK THE SUPERVISOR instead of guessing. It owns the answer; this script
# only needs to read it. Sourced in a subshell with `BINARY` unset first, so a
# conf that fails to resolve leaves the variable empty and we say we do not know
# rather than inheriting our OWN path and reporting a false success.
SUP_DIR="${THREADS_SUPERVISOR_DIR:-${HOME}/.threads-supervisor}"
LIVE_BINARY=""
if [[ -r "${SUP_DIR}/paths.env" && -r "${SUP_DIR}/services/threads.conf" ]]; then
    LIVE_BINARY="$(
        unset BINARY
        # shellcheck disable=SC1090,SC1091
        . "${SUP_DIR}/paths.env"   >/dev/null 2>&1
        # shellcheck disable=SC1090,SC1091
        . "${SUP_DIR}/services/threads.conf" >/dev/null 2>&1
        printf '%s' "${BINARY:-}"
    )"
fi

echo
if [[ -z "${LIVE_BINARY}" ]]; then
    echo "[build-threads] Signed OK at ${BINARY}."
    echo "[build-threads] CANNOT TELL whether that is the path the live daemon watches:"
    echo "[build-threads] ${SUP_DIR} did not yield a BINARY. Check by hand with:"
    echo "[build-threads]     promote status threads"
elif [[ "${BINARY}" == "${LIVE_BINARY}" && "${SIGN_MODE}" == "adhoc" ]]; then
    echo "[build-threads] NOT DEPLOYED. The binary is at the path the live daemon watches"
    echo "[build-threads] (${LIVE_BINARY}), but it is AD-HOC signed, so the daemon will"
    echo "[build-threads] IGNORE it and keep serving the code it runs. Re-run with"
    echo "[build-threads] HARNESS_SIGN_SCRIPT=<your signer> to deploy."
    exit 3
elif [[ "${BINARY}" == "${LIVE_BINARY}" ]]; then
    echo "[build-threads] DEPLOYED. The signed binary is at the path the live daemon"
    echo "[build-threads] watches (${LIVE_BINARY}); it will absorb it at its next quiet"
    echo "[build-threads] moment (no turn executing, no request in flight). Watch it"
    echo "[build-threads] land with:"
    echo "[build-threads]     tail -f ${THREADS_DIR}/restarts.jsonl"
else
    echo "[build-threads] Signed OK, but this is NOT the path the live daemon watches."
    echo "[build-threads]   built:    ${BINARY}"
    echo "[build-threads]   watched:  ${LIVE_BINARY}"
    if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
        echo "[build-threads] CARGO_TARGET_DIR is set, which is why. Nothing was deployed."
    else
        echo "[build-threads] The PROFILES differ — this build is '${PROFILE}' and the"
        echo "[build-threads] service launches from a different one. Nothing was deployed."
    fi
fi
