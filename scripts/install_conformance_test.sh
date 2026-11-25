#!/usr/bin/env bash
# Conformance test for scripts/install's RUNNING-JOB GUARD (REL-4.S3b, S3c).
#
# The guard: when a cauldron job for the target environment is already LOADED
# in this user's launchd domain (under any label), install REFUSES — rc 2,
# before it builds, writes or loads anything — unless `--replace-running` is
# passed, and then its plan says it will restart that job. Paired arms: no
# job loaded / another environment's job loaded must NOT refuse, so a script
# that always refuses fails; a loaded job must refuse, so one that never
# refuses fails.
#
# SAFETY: `launchctl` is a STUB first on PATH that answers `print`/`list`
# from STUB_LOADED and exits 97 on anything else (logged, asserted absent).
# HOME is a scratch dir under /private/tmp; CAULDRON_HOME is unset. The dry
# runs keep the real PATH behind the stub. EVERY NON-DRY RUN (cases 7, 12, 14)
# is SEALED (REL-4.S8b): PATH is ONLY a scratch dir holding FAKES for every
# command that could reach launchd, the keychain or a build (launchctl,
# codesign, cargo, cauldron via --from, the threads signer via a scratch copy
# of the repo layout) and LOGGING SHIMS for the plain utilities the script
# uses (each logs its argv and refuses, rc 98, a mutating call naming an
# absolute path outside the scratch dir). Anything else the script tries is
# `command not found`, and the sealed checks FAIL on that, on any shim
# refusal, and on any cargo call. Absolute-path invocations and commands
# whose stderr is hidden cannot be seen that way, so EVERY sealed run is also
# AUDITED (REL-4.S8c, inspect REL-4.S8b-I N3): a TWIN of the run — the same
# script and arguments from a copy of the same starting HOME — runs again
# under `bash -x`, and every command in its trace must be a bash builtin or
# keyword, a function the script defines, a name in the sealed dir, or an
# absolute path inside the scratch dir. Anything else fails the case,
# whether or not it was found or what it printed. Case 15 is the audit's
# positive sentinel.
#
# THE THREADS STEP (REL-4.S8, S8b, S8c), cases 11-14: a fresh environment's
# plan builds `threads` with `cauldron build threads --json --env-copy` (a NEW
# version `<label>+<env>`), copies its image into the environment's OWN data
# dir, signs it there (ad-hoc unless HARNESS_SIGN_SCRIPT is set), renames it
# back into that new version and pins THAT with `--addr auto` (prod keeps the
# default port only if free; any other env gets its own), before step 5. The
# shared build directory is never written. The stub cauldron's PLAIN `build`
# writes fresh bytes and a fresh build.json into the shared label EVERY time,
# as the real one does, so case 14b's sha256 check fails for an install that
# runs one.
# Paired arms: an existing env.toml (threads pinned or not) must NOT build or
# pin it, and case 12 proves it for real (sealed) with a byte check that a
# pin WOULD break. Case 14 runs the step for real, sealed, in a scratch HOME.
#
# THE CLEAN-TREE PRE-FLIGHT AND THE RE-RUN (REL-4.S5b), cases 16-18: when
# step 4b will build threads, a checkout with any modified or UNTRACKED file
# is refused at 2c, before the build and before env.toml exists (16, sealed;
# 18 with the REAL git over a scratch repository, clean arm opposed). `git`
# is a stub (GIT_STUB below) everywhere else, answering clean unless
# STUB_GIT_DIRTY / STUB_GIT_FAIL say otherwise. Case 17: an install that
# created env.toml and then stopped before the threads pin (the stub
# cauldron's build refuses, as the real one does on a dirty tree) is
# COMPLETED by a re-run — built, pinned, marker cleared, no second env
# create — and a third run neither rebuilds nor re-pins. Case 13 (an env.toml
# with no marker is never pinned) is its opposing arm, as a plan AND as a
# SEALED real run (REL-4.S5c M3).
#
# REL-4.S5c, cases 19-20: the marker is written BEFORE `env create` (M2), so
# an install KILLED the instant `env create` returns (the stub cauldron
# SIGKILLs its parent) is still finished by a re-run, and a failed `env
# create` leaves no marker behind (19). 14a asserts the marker existed when
# `env create` ran and is gone after a first install (M4). 2c checks the
# tree `cauldron build threads` compiles (M1): the workspace the `--from`
# cauldron names (the stub answers `versions install-workspace-probe` with
# STUB_WS), or an override declaration's `[build] workspace` (20). The git
# stub is dirty only at STUB_GIT_DIRTY_AT when that is set, so a script that
# checks the wrong tree passes the wrong tree.
#
# REL-4.S5d, cases 19b, 21-25: the marker PROVES which env.toml the install
# created (state=created + device:inode:birth + sha256, recorded the moment
# `env create` returns) and step 4b runs for an existing env.toml only when
# it matches — so 19's kill-right-after-create re-run now REFUSES (the
# fingerprint was never recorded). 21: the inspector's stale-1 (killed inside
# env create, then HIS env.toml) refuses, byte-identical; 21b a marker with no
# env.toml is stale and the run is a first install. 22: `env remove` then a
# re-made env (identical bytes, other inode) refuses, as does the same file
# with other bytes; a proven marker resumes. 23: env.toml changed during the
# build -> no pin. 24: the probe's first-line check and an empty --from. 25:
# the override's build.workspace as an inline table / dotted key, or refused.
#
# REL-4.S5e, cases 19b/19d, 22e, 26-28: the refusal speaks to the person who
# stopped the earlier install and gives the exact commands, and the dev one
# (env remove, re-run) is FOLLOWED to a full install (M2). 22e: a `cp -p`
# restore (same device, birth and bytes; only the inode differs) refuses —
# the one case that holds the inode deterministically (M1). 26: a stop
# between a marker's temp write and its rename leaves no temp file — the EXIT
# trap removes it on a catchable signal, the next run removes one a SIGKILL
# left, and another live install's is left alone (M3). 27/28: an unset
# variable aborts install / this suite with rc != 0, not the rc 0 that bash
# 3.2 gives a `set -u` abort under an EXIT trap.
#
# THE MCP STEP (4c), cases 12, 13, 14a, 17b, 19b', 21b, 26b, 29 and 30: a
# fresh environment also gets `cauldron build mcp --json --env-copy` and
# `pin mcp <label>+<env>` after threads; an existing env.toml with an mcp pin
# is never touched (12); an existing env.toml WITHOUT one (the upgrade) gets
# the same build and `cauldron pin mcp`, said in a line of its own, once: a
# re-run changes nothing (13, 30); a run stopped between the two pins is
# finished by a re-run that pins only mcp, and a third run changes nothing
# (29a-c); an env.toml edited by hand after the threads pin keeps its edit,
# the stale marker is cleared and mcp is added as an upgrade (29d). Case 31:
# an EXISTING mcp pin at an older build than this checkout's is rebuilt and
# re-pinned once under --replace-running (the supervisor moves the running
# service; the script kills nothing), a re-run changes nothing, without the
# flag it is left exactly as it is and the run says plainly that the mcp
# service is still the old build with the two commands that upgrade it (12
# too), a re-pin made by another writer during the build stands, and a
# dirty checkout is refused at 2c.
#
#   INSTALL=<path> overrides the script under test (used to force it red).
#   SUITE_UNDER_TEST=<path> overrides the suite case 28 mutates (same reason).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL="${INSTALL:-${SCRIPT_DIR}/install}"
SUITE="${SUITE_UNDER_TEST:-${SCRIPT_DIR}/$(basename "${BASH_SOURCE[0]}")}"

PASS=0; FAIL=0
ok()  { PASS=$((PASS+1)); printf '  ok   %s\n' "$1"; }
bad() { FAIL=$((FAIL+1)); printf '  FAIL %s\n' "$1"; if [[ $# -gt 1 ]]; then printf '%s\n' "$2" | sed 's/^/       /'; fi; }

T="$(mktemp -d /private/tmp/install-conf.XXXXXX)"
# /bin/bash 3.2 exits 0 from a `set -u` abort when an EXIT trap is set, and
# `$?` in the trap is 0 as well (REL-4.S5e), so only the end of the suite sets
# SUITE_DONE, and an rc-0 exit without it is reported as an abort (rc 70).
SUITE_DONE=0
suite_exit() {
    local rc=$?
    rm -rf "${T}" || :
    if [[ ${rc} -eq 0 && ${SUITE_DONE} -ne 1 ]]; then
        printf 'install_conformance_test: ABORTED before the end (error above); this is not a pass. rc 70.\n' >&2
        rc=70
    fi
    exit "${rc}"
}
trap suite_exit EXIT
mkdir -p "${T}/stub" "${T}/gitstub" "${T}/home" "${T}/empty"
STUB_LOG="${T}/launchctl.log"
: >"${STUB_LOG}"

cat >"${T}/stub/launchctl" <<'STUB'
#!/usr/bin/env bash
# Read-only launchctl stub. STUB_LOADED: space-separated label:pid.
# STUB_LIST_HIDE=1: `list` shows nothing (so only `print` can find a job).
# STUB_LIST_FAIL=1: `list` exits non-zero.
echo "$*" >>"${STUB_LOG:?}"
case "${1:-}" in
    print)
        lab="${2##*/}"
        for j in ${STUB_LOADED:-}; do
            if [[ "${j%%:*}" == "${lab}" ]]; then
                printf '%s = {\n\tstate = running\n\tpid = %s\n}\n' "$2" "${j##*:}"; exit 0
            fi
        done
        echo "Could not find service \"${lab}\"" >&2; exit 113 ;;
    list)
        if [[ "${STUB_LIST_FAIL:-0}" == 1 ]]; then echo "stub: list failed" >&2; exit 5; fi
        printf 'PID\tStatus\tLabel\n'
        printf '%s\t0\t%s\n' 1 com.apple.something 2 org.example.notcauldron 3 com.x.cauldron-dev-not
        if [[ "${STUB_LIST_HIDE:-0}" != 1 ]]; then
            for j in ${STUB_LOADED:-}; do printf '%s\t0\t%s\n' "${j##*:}" "${j%%:*}"; done
        fi
        exit 0 ;;
    *)
        echo "MUTATING $*" >>"${STUB_LOG}"; exit 97 ;;
esac
STUB
chmod +x "${T}/stub/launchctl"
# GIT_STUB: answers the two read-only questions install asks of git. Clean
# unless STUB_GIT_DIRTY (its value is the porcelain output) or STUB_GIT_FAIL.
cat >"${T}/gitstub/git" <<'STUB'
#!/bin/bash
printf 'git %s\n' "$*" >>"${EXEC_LOG:-/dev/null}"
[[ "${1:-}" == -C && $# -ge 2 ]] || { echo "stub git: no -C: $*" >&2; exit 97; }
dir="$2"; shift 2
case "$*" in
    "status --porcelain --untracked-files=normal")
        printf 'gitstatus %s\n' "${dir}" >>"${GIT_DIRS_LOG:-/dev/null}"
        if [[ "${STUB_GIT_FAIL:-0}" == 1 ]]; then echo "fatal: not a git repository (stub)" >&2; exit 128; fi
        # STUB_GIT_DIRTY_AT: dirty ONLY in that tree (same path, symlinks resolved).
        if [[ -n "${STUB_GIT_DIRTY_AT:-}" ]]; then
            a="$(cd -P "${dir}" 2>/dev/null && pwd -P)"; b="$(cd -P "${STUB_GIT_DIRTY_AT}" 2>/dev/null && pwd -P)"
            [[ -n "${a}" && "${a}" == "${b}" ]] || exit 0
        fi
        if [[ -n "${STUB_GIT_DIRTY:-}" ]]; then printf '%s\n' "${STUB_GIT_DIRTY}"; fi
        exit 0 ;;
    "rev-parse --short=12 HEAD") echo feedc0de1234; exit 0 ;;
    *) echo "stub git: unexpected $*" >&2; exit 97 ;;
esac
STUB
chmod +x "${T}/gitstub/git"

# run <loaded> <list_hide> <args...>  -> sets OUT (stdout+stderr), ERR
# (stderr alone), RC. RUN_CWD / RUN_AS (a path relative to RUN_CWD) invoke
# the script from another directory by a relative path; STUB_LIST_FAIL is
# passed through.
run() {
    local loaded="$1" hide="$2"; shift 2
    set +e
    OUT="$(cd "${RUN_CWD:-${PWD}}" && env -u CAULDRON_HOME -u HARNESS_SIGN_SCRIPT \
        ${RUN_SIGNER:+HARNESS_SIGN_SCRIPT="${RUN_SIGNER}"} \
        HOME="${RUN_HOME:-${T}/home}" PATH="${RUN_PATH_HEAD:-}${T}/stub:${RUN_REAL_GIT:+${T}/empty}${RUN_REAL_GIT:-${T}/gitstub}:${PATH}" \
        STUB_LOG="${STUB_LOG}" STUB_LOADED="${loaded}" STUB_LIST_HIDE="${hide}" \
        STUB_LIST_FAIL="${STUB_LIST_FAIL:-0}" \
        STUB_GIT_DIRTY="${STUB_GIT_DIRTY:-}" STUB_GIT_FAIL="${STUB_GIT_FAIL:-0}" \
        STUB_GIT_DIRTY_AT="${STUB_GIT_DIRTY_AT:-}" STUB_WS="${STUB_WS:-${T}/repo}" STUB_PROBE="${STUB_PROBE:-}" \
        CLOG="${CLOG:-${T}/cauldron.log}" GIT_DIRS_LOG="${T}/gitdirs.log" \
        "${RUN_BASH:-bash}" "${RUN_AS:-${INSTALL}}" "$@" 2>"${T}/stderr")"
    RC=$?
    set -e
    ERR="$(cat "${T}/stderr")"
    OUT="${OUT}"$'\n'"${ERR}"
}
has()  { grep -qF -- "$1" <<<"${OUT}"; }
home_empty() { [[ -z "$(find "${T}/home" -mindepth 1 -print -quit)" ]]; }

# ── THE SEALED PATH (REL-4.S8b) ─────────────────────────────────────────────
SEAL="${T}/sealed"; EXEC_LOG="${T}/exec.log"; SIGN_LOG="${T}/sign.log"
mkdir -p "${SEAL}" "${T}/repo/scripts" "${T}/repo/services/threads/scripts"
: >"${EXEC_LOG}"; : >"${SIGN_LOG}"
# The account helper scripts the from-source path lays down (issue #30), as
# fakes in the scratch checkout, each naming where it came from.
for h in add-account claude-accounts claude-login-browser; do
    printf '#!/usr/bin/env bash\n# helper %s from the checkout\nexit 0\n' "${h}" >"${T}/repo/services/threads/scripts/${h}"
    /bin/chmod 0755 "${T}/repo/services/threads/scripts/${h}"
done
# helpers_at <home> <text>: the three helpers are installed at
# <root>/share/cauldron/scripts/threads/, each mode 755 and carrying <text>,
# and every directory from share/ down is 755.
helpers_at() {
    local d="$1/.cauldron/share/cauldron/scripts/threads" h x
    for h in add-account claude-accounts claude-login-browser; do
        [[ -f "${d}/${h}" && "$(/usr/bin/stat -f %Lp "${d}/${h}")" == 755 ]] && /usr/bin/grep -q "$2" "${d}/${h}" || return 1
    done
    for x in "$1/.cauldron/share" "$1/.cauldron/share/cauldron" "$1/.cauldron/share/cauldron/scripts" "${d}"; do
        [[ "$(/usr/bin/stat -f %Lp "${x}")" == 755 ]] || return 1
    done
}
real_of() { local d; for d in /usr/bin /bin /usr/sbin /sbin; do if [[ -x "${d}/$1" ]]; then echo "${d}/$1"; return 0; fi; done; echo "no real $1" >&2; return 1; }
# Logging shims for the plain utilities. Mutating ones refuse (98) any
# absolute path argument outside ${T} (cp: its destination only, since it may
# read a source elsewhere).
for c in dirname basename sed uname sw_vers id stat awk grep wc tr cat mkdir cp mv rm chmod bash shasum tar od cmp ls; do
    r="$(real_of "${c}")"
    cat >"${SEAL}/${c}" <<SHIM
#!/bin/bash
printf '%s\n' "${c} \$*" >>"\${EXEC_LOG:?}"
check() { case "\$1" in /*) case "\$1" in "${T}"/*) ;; *) printf 'ESCAPE ${c} %s\n' "\$1" >>"\${EXEC_LOG}"; exit 98 ;; esac ;; esac; }
case "${c}" in
    mv) # STUB_MV_KILL=<signal> (REL-4.S5e M3): the rename of a marker's temp
        # file signals the install instead, i.e. a stop between the write and the mv.
        if [[ -n "\${STUB_MV_KILL:-}" && "\${1:-}" == -f && "\${2:-}" == *.install-threads-pending.*.tmp.* ]]; then
            kill -"\${STUB_MV_KILL}" "\${PPID}"; exit 1
        fi
        for a in "\$@"; do check "\$a"; done ;;
    mkdir|rm|chmod) for a in "\$@"; do check "\$a"; done ;;
    # STUB_ARCH: what \`uname -m\` answers (the prebuilt path's arm64 check).
    uname) if [[ "\${1:-}" == -m && -n "\${STUB_ARCH:-}" ]]; then echo "\${STUB_ARCH}"; exit 0; fi ;;
    # A fake executable is marked FAKEMACHO:arm64; od reads it as a thin arm64 Mach-O header.
    od) for a in "\$@"; do f="\$a"; done
        if /usr/bin/grep -aq 'FAKEMACHO:arm64' "\$f" 2>/dev/null; then echo " cf fa ed fe 0c 00 00 01"; exit 0; fi ;;
    tar) prev=""; for a in "\$@"; do [[ "\$prev" == -C ]] && check "\$a"; prev="\$a"; done ;;
    cp) check "\${@: -1}" ;;
    sed) case " \$* " in *" -i "*) check "\${@: -1}" ;; esac ;;
    bash) [[ \$# -eq 0 ]] || check "\$1" ;;
esac
exec "${r}" "\$@"
SHIM
    /bin/chmod +x "${SEAL}/${c}"
done
/bin/cp "${T}/stub/launchctl" "${SEAL}/launchctl"
/bin/cp "${T}/gitstub/git" "${SEAL}/git"
# cargo: a build is never allowed in a sealed run.
cat >"${SEAL}/cargo" <<'FAKE'
#!/bin/bash
printf 'CARGO %s\n' "$*" >>"${EXEC_LOG:?}"
exit 97
FAKE
# codesign: a fake. `--force --sign - --identifier ID F` appends a marker to F
# (it rewrites the file, as the real one does); `-dvvv F` reports it.
cat >"${SEAL}/codesign" <<'FAKE'
#!/bin/bash
export LC_ALL=C
printf 'codesign %s\n' "$*" >>"${EXEC_LOG:?}"
f="${@: -1}"
case "$1" in
    --force) id=""; while [[ $# -gt 1 ]]; do [[ "$1" == --identifier ]] && id="$2"; shift; done
             printf '\nFAKESIGN:%s\n' "${id}" >>"${f}"
             # The sum of everything but itself: \`--verify\` fails once a byte changes.
             printf 'FAKESUM:%s\n' "$(/usr/bin/grep -av '^FAKESUM:' "${f}" | /usr/bin/shasum -a 256 | /usr/bin/awk '{print $1}')" >>"${f}" ;;
    -dvvv|-dv) id="$(/usr/bin/sed -n 's/^FAKESIGN://p' "${f}" 2>/dev/null | /usr/bin/tail -n 1)"
             if [[ -n "${id}" ]]; then printf 'Identifier=%s\nSignature=adhoc\n' "${id}" >&2; else echo "${f}: code object is not signed at all" >&2; exit 1; fi ;;
    --verify) want="$(/usr/bin/sed -n 's/^FAKESUM://p' "${f}" 2>/dev/null | /usr/bin/tail -n 1)"
             have="$(/usr/bin/grep -av '^FAKESUM:' "${f}" 2>/dev/null | /usr/bin/shasum -a 256 | /usr/bin/awk '{print $1}')"
             if [[ -n "${want}" && "${want}" == "${have}" ]]; then exit 0; fi
             echo "${f}: invalid signature (code or signature have been modified)" >&2; exit 1 ;;
    *) echo "fake codesign: unexpected $*" >&2; exit 64 ;;
esac
FAKE
# curl: a fake. \`-o OUT URL\` copies STUB_CURL_FILE to OUT (logged); nothing is fetched.
cat >"${SEAL}/curl" <<'FAKE'
#!/bin/bash
printf 'curl %s\n' "$*" >>"${EXEC_LOG:?}"
out=""; prev=""; for a in "$@"; do [[ "${prev}" == -o ]] && out="${a}"; prev="${a}"; done
[[ -n "${out}" && -f "${STUB_CURL_FILE:-}" ]] || { echo "fake curl: no -o or no STUB_CURL_FILE" >&2; exit 22; }
case "${out}" in "${HOME}"/*) ;; *) printf 'ESCAPE curl %s\n' "${out}" >>"${EXEC_LOG}"; exit 98 ;; esac
/bin/cp "${STUB_CURL_FILE}" "${out}"
FAKE
/bin/chmod +x "${SEAL}/launchctl" "${SEAL}/git" "${SEAL}/cargo" "${SEAL}/codesign" "${SEAL}/curl"
# The threads signer, at the path install derives from its own location, as
# a fake that signs through `codesign` ON PATH (the fake) and verifies.
cat >"${T}/repo/services/threads/scripts/build-threads.sh" <<'FAKE'
printf 'signer %s\n' "$*" >>"${SIGN_LOG:?}"
[[ "${1:-}" == --sign-only && -n "${2:-}" ]] || exit 2
if [[ -n "${HARNESS_SIGN_SCRIPT:-}" ]]; then "${HARNESS_SIGN_SCRIPT}" "$2" com.harness.threads || exit $?
else codesign --force --sign - --identifier com.harness.threads "$2" || exit $?; fi
codesign -dvvv "$2" 2>&1 | grep -q '^Identifier=com.harness.threads$' || { echo "fake signer: $2 does not verify" >&2; exit 1; }
FAKE

# srun <loaded> <args...>: a SEALED, non-dry run of a scratch copy of the
# script under test (so its signer is the fake above). Sets OUT, ERR, RC.
srun() {
    local loaded="$1"; shift
    local home="${RUN_HOME:-${T}/home}"
    /bin/cp "${SRUN_SRC:-${INSTALL}}" "${T}/repo/scripts/install"
    # The twin's starting state: a copy of this run's HOME, taken BEFORE it.
    /bin/rm -rf "${T}/twin"; /bin/mkdir -p "${T}/twin"
    if [[ -d "${home}" ]]; then /bin/cp -Rp "${home}/." "${T}/twin/"; fi
    # REL-4.S5c stub knobs, passed to the run AND its twin. Paths here hold no
    # spaces (all under ${T}), so word splitting is safe.
    local SRUN_STUBS="STUB_GIT_DIRTY_AT=${STUB_GIT_DIRTY_AT:-} STUB_WS=${STUB_WS:-${T}/repo} STUB_PROBE=${STUB_PROBE:-} STUB_CREATE_KILL=${STUB_CREATE_KILL:-0} STUB_CREATE_FAIL=${STUB_CREATE_FAIL:-0} STUB_BUILD_TOUCH=${STUB_BUILD_TOUCH:-0} STUB_MV_KILL=${STUB_MV_KILL:-} STUB_MCP_REFUSE=${STUB_MCP_REFUSE:-0} STUB_MCP_REPIN_DURING=${STUB_MCP_REPIN_DURING:-0} STUB_THREADS_REPIN_DURING=${STUB_THREADS_REPIN_DURING:-0} STUB_NAME_ONLY=${STUB_NAME_ONLY:-0} STUB_ARCH=${STUB_ARCH:-} STUB_CURL_FILE=${STUB_CURL_FILE:-}"
    set +e
    OUT="$(cd "${T}" && /usr/bin/env -i HOME="${RUN_HOME:-${T}/home}" PATH="${SEAL}${SRUN_PATH_EXTRA:+:${SRUN_PATH_EXTRA}}" \
        ${RUN_SIGNER:+HARNESS_SIGN_SCRIPT="${RUN_SIGNER}"} \
        STUB_LOG="${STUB_LOG}" STUB_LOADED="${loaded}" STUB_LIST_HIDE=0 STUB_LIST_FAIL=0 \
        STUB_GIT_DIRTY="${STUB_GIT_DIRTY:-}" STUB_GIT_FAIL="${STUB_GIT_FAIL:-0}" STUB_BUILD_REFUSE="${STUB_BUILD_REFUSE:-0}" \
        ${SRUN_STUBS} \
        EXEC_LOG="${EXEC_LOG}" SIGN_LOG="${SIGN_LOG}" CLOG="${CLOG:-${T}/cauldron.log}" GIT_DIRS_LOG="${T}/gitdirs.log" \
        /bin/bash "${T}/repo/scripts/install" "$@" 2>"${T}/stderr")"
    RC=$?
    # The TWIN, traced. Its own logs are throwaway; only its trace is read.
    : >"${T}/trace"
    (cd "${T}" && /usr/bin/env -i HOME="${T}/twin" PATH="${SEAL}" PS4='+XTRACE ' \
        ${RUN_SIGNER:+HARNESS_SIGN_SCRIPT="${RUN_SIGNER}"} \
        STUB_LOG="${T}/twin.log" STUB_LOADED="${loaded}" STUB_LIST_HIDE=0 STUB_LIST_FAIL=0 \
        STUB_GIT_DIRTY="${STUB_GIT_DIRTY:-}" STUB_GIT_FAIL="${STUB_GIT_FAIL:-0}" STUB_BUILD_REFUSE="${STUB_BUILD_REFUSE:-0}" \
        ${SRUN_STUBS} \
        EXEC_LOG="${T}/twin.log" SIGN_LOG="${T}/twin.log" CLOG="${T}/twin.clog" \
        /bin/bash -x "${T}/repo/scripts/install" "$@" >/dev/null 2>"${T}/trace")
    set -e
    ERR="$(cat "${T}/stderr")"
    OUT="${OUT}"$'\n'"${ERR}"
}
# exec_audit: every command the traced twin ran, which is not a builtin, a
# keyword, the script's own function, a sealed name or a scratch path. One per
# line; empty means clean. Trace records start `+XTRACE ` (`++…` when nested);
# a line without the prefix is a continuation of a multi-line value, not a
# command. Assignments are skipped.
ALLOWED="$( { /bin/bash -c 'compgen -b; compgen -k'; /bin/ls "${SEAL}"; } | /usr/bin/sort -u)"
exec_audit() {
    local funcs w
    funcs="$(/usr/bin/grep -oE '^[A-Za-z_][A-Za-z0-9_]*\(\)' "${T}/repo/scripts/install" | /usr/bin/tr -d '()')"
    { /usr/bin/grep -E '^\++XTRACE ' "${T}/trace" || :; } | /usr/bin/sed -E 's/^\++XTRACE //' | while IFS= read -r line; do
        w="${line%% *}"; w="${w#\'}"; w="${w%\'}"
        [[ -n "${w}" ]] || continue
        if [[ "${w}" =~ ^[A-Za-z_][A-Za-z0-9_]*(\[[^]]*\])?\+?= ]]; then continue; fi
        case "${w}" in
            "${T}"/*) continue ;;
            /*) printf '%s\n' "${line}"; continue ;;
        esac
        if /usr/bin/grep -qxF -- "${w}" <<<"${ALLOWED}"$'\n'"${funcs}"; then continue; fi
        printf '%s\n' "${line}"
    done
}
# sealed_ok <tag>: the run touched nothing but fakes and shims.
sealed_ok() {
    local bad_exec; bad_exec="$(exec_audit)"
    if [[ -s "${T}/trace" ]] && /usr/bin/grep -qE '^\++XTRACE ' "${T}/trace"; then ok "$1: the twin was traced"; else bad "$1: no trace from the twin" "$(cat "${T}/trace")"; fi
    if [[ -z "${bad_exec}" ]]; then ok "$1: every command in the trace is a builtin, a function, a sealed name or a scratch path"; else bad "$1: the trace ran a command outside the seal" "${bad_exec}"; fi
    if ! grep -q 'command not found' <<<"${ERR}"; then ok "$1: no command outside the sealed PATH was tried"; else bad "$1: a non-stub command was tried" "$(grep 'command not found' <<<"${ERR}")"; fi
    if ! grep -q '^ESCAPE' "${EXEC_LOG}"; then ok "$1: no shim refused a path outside the scratch dir"; else bad "$1: a path outside the scratch dir" "$(grep '^ESCAPE' "${EXEC_LOG}")"; fi
    if ! grep -q '^CARGO' "${EXEC_LOG}"; then ok "$1: cargo never ran"; else bad "$1: cargo ran" "$(grep '^CARGO' "${EXEC_LOG}")"; fi
    if ! grep -q '^MUTATING' "${STUB_LOG}"; then ok "$1: no mutating launchctl call"; else bad "$1: mutating launchctl call" "$(grep MUTATING "${STUB_LOG}")"; fi
    local p; p="$(grep -oE '^install: \$ [^ ]*/cauldron ' <<<"${OUT}" | sed -E 's/^install: \$ //; s/ $//' | sort -u || true)"
    if [[ -z "${p}" ]] || ! grep -qvF "${T}/" <<<"${p}"; then ok "$1: every cauldron it ran is under the scratch dir"; else bad "$1: a cauldron outside the scratch dir" "${p}"; fi
}

printf '\n== 0. the stub is the launchctl the script consults\n'
run "" 0 --dry-run
if grep -q '^print gui/' "${STUB_LOG}"; then ok "stub saw print calls"; else bad "stub never consulted" "$(cat "${STUB_LOG}")"; fi

printf '\n== 1. nothing loaded -> not refused\n'
run "" 0 --dry-run
if [[ ${RC} -eq 0 ]]; then ok "rc 0"; else bad "rc ${RC}, wanted 0" "${OUT}"; fi
if has "no cauldron job for 'prod' is loaded"; then ok "says none loaded"; else bad "no 'none loaded' line" "${OUT}"; fi
if ! grep -qF 'WARNING: launchctl list failed' <<<"${ERR}"; then ok "no list WARNING when list works"; else bad "list WARNING with a working list" "${ERR}"; fi
if grep -qE '^install: \$ .*/cauldron --env prod install$' <<<"${OUT}"; then ok "step 5 runs cauldron install without --replace-running"; else bad "step 5 line wrong" "${OUT}"; fi
# The closing lines on the bridge's PATH: the plan says them too, before its last line.
bridge_ok() {
    # <root> <a line that must come AFTER them> <tag> [<a line that must come BEFORE them>]
    local n c b=0
    n="$(grep -nF 'MCP:      the plugin'"'"'s MCP server is its launcher, bin/cauldron-mcp, which runs `cauldron mcp stdio`' <<<"${OUT}" | head -n 1 | cut -d: -f1)"
    c="$(grep -nF "${2}" <<<"${OUT}" | tail -n 1 | cut -d: -f1)"
    if [[ -n "${4:-}" ]]; then b="$(grep -nF "${4}" <<<"${OUT}" | head -n 1 | cut -d: -f1)"; fi
    if [[ -n "${n}" && -n "${c}" && -n "${b}" && ${c} -gt ${n} && ${n} -gt ${b} ]] && has "without needing \`cauldron\` on the PATH Claude Code was started with. It looks at" \
        && has "\$CAULDRON_BIN, then the install marker ${1%/.cauldron}/Library/Application Support/cauldron/cauldron-bin (written for prod)," \
        && has "then ${1}/bin/cauldron, then PATH." \
        && has "to check: \`/mcp\` inside a session lists cauldron as connected"; then
        ok "${3}: closing lines say where the plugin's launcher looks for cauldron, and how to check"
    else bad "${3}: no bridge PATH lines (or not at the end)" "${OUT}"; fi
}
bridge_ok "${T}/home/.cauldron" "DRY RUN complete" "1 (dry run)"

printf '\n== 2. legacy com.broomstick.cauldron loaded -> REFUSED\n'
run "com.broomstick.cauldron:4242" 0 --dry-run
if [[ ${RC} -eq 2 ]]; then ok "rc 2"; else bad "rc ${RC}, wanted 2" "${OUT}"; fi
if has "REFUSED: a cauldron job for environment 'prod' is already loaded"; then ok "REFUSED line"; else bad "no REFUSED line" "${OUT}"; fi
if has "LOADED     com.broomstick.cauldron (pid 4242)"; then ok "names the job and its pid"; else bad "job not named" "${OUT}"; fi
if has "--replace-running --dry-run"; then ok "prints the command to proceed"; else bad "no proceed command" "${OUT}"; fi
if ! has "== 3. binaries"; then ok "stopped before step 3"; else bad "went past the guard" "${OUT}"; fi
SAVED_REFUSAL="${OUT}"

printf '\n== 3. same job + --replace-running -> the plan shows the restart\n'
run "com.broomstick.cauldron:4242" 0 --dry-run --replace-running
if [[ ${RC} -eq 0 ]]; then ok "rc 0"; else bad "rc ${RC}, wanted 0" "${OUT}"; fi
if has "WILL RESTART com.broomstick.cauldron (pid 4242)"; then ok "WILL RESTART line"; else bad "no WILL RESTART line" "${OUT}"; fi
if has "== 5. supervisor"; then ok "plan reaches step 5"; else bad "plan stops early" "${OUT}"; fi
if grep -qE '^install: \$ .*/cauldron --env prod install --replace-running$' <<<"${OUT}"; then ok "--replace-running passed through to cauldron install"; else bad "flag not passed to cauldron install" "${OUT}"; fi

printf '\n== 3b. --owner-acts: open records the choice through step 5'"'"'s cauldron install, terminal closes it, anything else is refused\n'
run "" 0 --dry-run --owner-acts open
if [[ ${RC} -eq 0 ]] && grep -qE '^install: \$ env CAULDRON_OWNER_ACTS=open .*/cauldron --env prod install --owner-acts open$' <<<"${OUT}"; then ok "open: cauldron install records it, with the variable for that one command"; else bad "rc ${RC}, no install --owner-acts open line" "${OUT}"; fi
n5="$(grep -nF -- '== 5. supervisor' <<<"${OUT}" | head -n 1 | cut -d: -f1)"; noa="$(grep -nF -- 'install --owner-acts open' <<<"${OUT}" | head -n 1 | cut -d: -f1)"
if [[ -n "${n5}" && -n "${noa}" && "${n5}" -lt "${noa}" ]]; then ok "open: in step 5"; else bad "owner-acts planned before step 5" "${OUT}"; fi
if ! has "--how"; then ok "open: no --how argument (the code path records install)"; else bad "a --how argument" "${OUT}"; fi
run "" 0 --dry-run --owner-acts=terminal
if [[ ${RC} -eq 0 ]] && grep -qE '^install: \$ .*/cauldron --env prod install --owner-acts terminal$' <<<"${OUT}" && ! has "CAULDRON_OWNER_ACTS"; then ok "terminal: planned, no variable"; else bad "rc ${RC}, no install --owner-acts terminal line" "${OUT}"; fi
run "" 0 --dry-run
if [[ ${RC} -eq 0 ]] && ! has "owner-acts"; then ok "no flag: the setting is left alone"; else bad "rc ${RC}, owner-acts without the flag" "${OUT}"; fi
run "" 0 --dry-run --owner-acts yes
if [[ ${RC} -eq 2 ]] && has "--owner-acts is \`open\` or \`terminal\`, not \`yes\`" && ! has "== 3. binaries"; then ok "an unknown value: rc 2 before anything"; else bad "rc ${RC}" "${OUT}"; fi

printf '\n== 4. any other prefix (list only) -> REFUSED\n'
run "org.example.cauldron:77" 0 --dry-run
if [[ ${RC} -eq 2 ]] && has "LOADED     org.example.cauldron (pid 77)"; then ok "rc 2, org.example.cauldron named"; else bad "rc ${RC}" "${OUT}"; fi

printf '\n== 5. configured prefix found by print alone (list hidden) -> REFUSED\n'
mkdir -p "${T}/home/.cauldron/data/prod"
printf '[supervisor]\nauto_restart = true\nlabel_prefix = "net.example"\n' >"${T}/home/.cauldron/data/prod/env.toml"
run "net.example.cauldron:55" 1 --dry-run
if [[ ${RC} -eq 2 ]] && has "LOADED     net.example.cauldron (pid 55)"; then ok "rc 2, configured label named"; else bad "rc ${RC}" "${OUT}"; fi
rm -rf "${T}/home/.cauldron"

printf '\n== 6. another environment'"'"'s job loaded -> --env dev is NOT refused\n'
run "com.broomstick.cauldron:4242" 0 --dry-run --env dev
if [[ ${RC} -eq 0 ]] && has "no cauldron job for 'dev' is loaded"; then ok "rc 0, prod's job ignored for dev"; else bad "rc ${RC}" "${OUT}"; fi
run "ai.broomstick.cauldron-dev:9" 0 --dry-run --env dev
if [[ ${RC} -eq 2 ]] && has "LOADED     ai.broomstick.cauldron-dev (pid 9)"; then ok "rc 2 for dev's own job"; else bad "rc ${RC}" "${OUT}"; fi

printf '\n== 7. a REAL (non-dry, SEALED) run over a loaded job refuses before anything\n'
: >"${EXEC_LOG}"
srun "com.broomstick.cauldron:4242" --from "${T}/empty"
if [[ ${RC} -eq 2 ]] && has "REFUSED: a cauldron job"; then ok "rc 2, refused by the guard"; else bad "rc ${RC}, not refused by the guard" "${OUT}"; fi
sealed_ok "case 7"

printf '\n== 8. nothing was written and nothing mutating was called\n'
if home_empty; then ok "scratch HOME empty"; else bad "scratch HOME written" "$(find "${T}/home")"; fi
if ! grep -q '^MUTATING' "${STUB_LOG}"; then ok "no mutating launchctl call"; else bad "mutating call" "$(grep MUTATING "${STUB_LOG}")"; fi

printf '\n== 9. the command to proceed is absolute and names THIS script, from any cwd\n'
# Invoked by a RELATIVE path from the script's own dir, then checked from /.
RUN_CWD="$(dirname "${INSTALL}")" RUN_AS="./$(basename "${INSTALL}")" run "com.broomstick.cauldron:4242" 0 --dry-run
cmd_line="$(grep -F 'install:   ' <<<"${ERR}" | tail -n 1)"
first="$(sed -E 's/^install:   ([^ ]+) .*/\1/' <<<"${cmd_line}")"
if [[ ${RC} -eq 2 && "${first}" == /* ]]; then ok "proceed path is absolute (${first})"; else bad "proceed path not absolute: '${first}' rc ${RC}" "${OUT}"; fi
if [[ -n "${first}" && "${first}" -ef "${INSTALL}" ]]; then ok "proceed path is this script"; else bad "proceed path '${first}' is not ${INSTALL}"; fi
if [[ "${cmd_line}" == *" --replace-running --dry-run" ]]; then ok "proceed command carries the flags"; else bad "proceed flags wrong" "${cmd_line}"; fi

printf '\n== 10. launchctl list FAILS -> loud stderr WARNING, named prefixes still checked\n'
STUB_LIST_FAIL=1 run "com.broomstick.cauldron:4242" 0 --dry-run
if grep -qF 'WARNING: launchctl list failed' <<<"${ERR}"; then ok "WARNING on stderr"; else bad "no WARNING on stderr" "${OUT}"; fi
if [[ ${RC} -eq 2 ]] && has "LOADED     com.broomstick.cauldron (pid 4242)"; then ok "rc 2, legacy label still found by print"; else bad "rc ${RC}" "${OUT}"; fi
STUB_LIST_FAIL=1 run "" 0 --dry-run
if [[ ${RC} -eq 0 ]] && grep -qF 'WARNING: launchctl list failed' <<<"${ERR}"; then ok "rc 0 with nothing loaded, still warned"; else bad "rc ${RC}" "${OUT}"; fi
if ! grep -q '^MUTATING' "${STUB_LOG}"; then ok "still no mutating launchctl call"; else bad "mutating call" "$(grep MUTATING "${STUB_LOG}")"; fi

# ── THE THREADS STEP (REL-4.S8) ─────────────────────────────────────────────
# The resume marker (beside the env's data dir since REL-4.S5c M2) and env.toml.
MARK() { echo "$1/.cauldron/data/.install-threads-pending.prod"; }
ENVT() { echo "$1/.cauldron/data/prod/env.toml"; }
# line_no <ERE>: the first line of OUT matching it, or 0.
line_no() { local n; n="$(grep -nE -- "$1" <<<"${OUT}" | head -n 1 | cut -d: -f1)"; echo "${n:-0}"; }
# prefix_of <before> <after>: <after> starts with every byte of <before>.
prefix_of() { [[ -f "$2" ]] && /usr/bin/head -c "$(/usr/bin/wc -c <"$1" | /usr/bin/tr -d ' ')" "$2" | /usr/bin/cmp -s - "$1"; }
RE_BUILD='^install: \$ .*/cauldron --env prod build threads --json --env-copy$'
RE_SIGN='^install: \$ bash .*/services/threads/scripts/build-threads\.sh --sign-only .*/data/prod/\.threads-image\.[0-9]+/threads$'
RE_PIN='^install: \$ .*/cauldron --env prod pin threads [^ ]+ --addr auto$'
RE_MV='^install: \$ mv .*/data/prod/\.threads-image\.[0-9]+/threads .*/data/shared/builds/threads/\\<label\\>\+prod/threads$'
RE_ANYPIN='pin threads'
RE_STEP5='^install: \$ .*/cauldron --env prod install'

printf '\n== 11. fresh environment -> the plan builds, signs and pins threads before step 5\n'
mkdir -p "${T}/home11"
RUN_HOME="${T}/home11" run "" 0 --dry-run
b="$(line_no "${RE_BUILD}")"; sg="$(line_no "${RE_SIGN}")"; p="$(line_no "${RE_PIN}")"; s5="$(line_no "${RE_STEP5}")"
if [[ ${RC} -eq 0 ]]; then ok "rc 0"; else bad "rc ${RC}, wanted 0" "${OUT}"; fi
if [[ ${b} -gt 0 ]]; then ok "plan builds threads as its own version (cauldron build threads --json --env-copy)"; else bad "no threads build --env-copy in the plan" "${OUT}"; fi
if [[ ${sg} -gt 0 ]]; then ok "plan signs the installed image (build-threads.sh --sign-only on a copy)"; else bad "no threads sign in the plan" "${OUT}"; fi
if [[ ${p} -gt 0 ]]; then ok "plan pins threads for prod with --addr auto"; else bad "no threads pin --addr auto in the plan" "${OUT}"; fi
mv_="$(line_no "${RE_MV}")"
if [[ ${b} -gt 0 && ${sg} -gt ${b} && ${mv_} -gt ${sg} && ${p} -gt ${mv_} && ${s5} -gt ${p} ]]; then ok "order: build < sign (env's own dir) < rename in as <label>+prod < pin < step 5"; else bad "order wrong: build ${b} sign ${sg} mv ${mv_} pin ${p} step5 ${s5}" "${OUT}"; fi
# The WRITE target of every cp / mv / sed -i / signer line is its last word.
if ! grep -E '^install: \$ (cp|mv|sed|bash|rm|mkdir) ' <<<"${OUT}" | awk '{print $NF}' | grep -qE '/builds/threads/\\<label\\>(/|$)'; then ok "the plan writes nothing into the shared build directory"; else bad "the plan writes into the shared build" "${OUT}"; fi
if ! grep -E -- "${RE_ANYPIN}" <<<"${OUT}" | grep -qE ':(3651|3653|3753|9010)'; then ok "no pin names a literal prod port"; else bad "a pin names a prod port" "${OUT}"; fi
if has "signer     ad-hoc"; then ok "signer is ad-hoc with HARNESS_SIGN_SCRIPT unset"; else bad "signer not ad-hoc" "${OUT}"; fi
RUN_SIGNER=/nonexistent/sign.sh RUN_HOME="${T}/home11" run "" 0 --dry-run
if has "signer     HARNESS_SIGN_SCRIPT=/nonexistent/sign.sh (identity)"; then ok "signer is HARNESS_SIGN_SCRIPT when set"; else bad "HARNESS_SIGN_SCRIPT ignored" "${OUT}"; fi
RUN_HOME="${T}/home11" run "" 0 --dry-run --env dev
if [[ ${RC} -eq 0 ]] && grep -qE '^install: \$ .*/cauldron --env dev pin threads [^ ]+\+dev --addr auto$' <<<"${OUT}" && has "'dev' is not prod: it gets its OWN free loopback port"; then ok "--env dev pins its own <label>+dev with --addr auto, and says it gets its own port"; else bad "dev plan wrong (rc ${RC})" "${OUT}"; fi
if [[ -z "$(find "${T}/home11" -mindepth 1 -print -quit)" ]]; then ok "dry run wrote nothing"; else bad "dry run wrote" "$(find "${T}/home11")"; fi

printf '\n== 12. existing env.toml with threads AND mcp pinned -> NOT rebuilt, NOT re-pinned\n'
mkdir -p "${T}/home12/.cauldron/data/prod"
printf '[supervisor]\nauto_restart = true\n\n[services.threads]\nversion = "threadsold-release"\naddr = "127.0.0.1:3753"\n\n[services.mcp]\nversion = "mcpold-release+prod"\n' >"${T}/home12/.cauldron/data/prod/env.toml"
cp "${T}/home12/.cauldron/data/prod/env.toml" "${T}/env12.before"
RUN_HOME="${T}/home12" run "" 0 --dry-run
if [[ ${RC} -eq 0 ]] && has "threads is already pinned in" && has "mcp is already pinned in"; then ok "rc 0, says both existing pins are kept"; else bad "rc ${RC}" "${OUT}"; fi
if [[ $(line_no "${RE_BUILD}") -eq 0 ]] && ! grep -E '^install: \$ ' <<<"${OUT}" | grep -qE -- "${RE_ANYPIN}|build mcp|pin mcp"; then ok "no build or pin of threads or mcp in the plan"; else bad "existing env would be rebuilt/re-pinned" "${OUT}"; fi
# Its mcp pin is an older build (`mcpold-…`): without --replace-running it
# is left as it is, and the run says so plainly with the two commands.
old_mcp_said() {
    # <tag> <cauldron path the commands name>
    if has "mcp is pinned at mcpold-release+prod, which is NOT this checkout's build" \
        && has "THE mcp SERVICE IS STILL THE OLD BUILD: it is pinned at mcpold-release+prod" \
        && has "    ${2} --env prod build mcp --env-copy" \
        && has "    then: ${2} --env prod pin mcp <the label that build printed>" \
        && has "(or run this install again with --replace-running, which does both)" \
        && has "mcp:      the mcp service is still the old build (mcpold-release+prod); to upgrade it:" \
        && has "            ${2} --env prod pin mcp <the label that build printed>"; then
        ok "${1}: says the mcp service is still the old build, and the two commands that upgrade it (in 4c and at the end)"
    else bad "${1}: no plain old-build lines" "${OUT}"; fi
}
old_mcp_said "12 plan" "$(sed -nE 's/^install: \$ (.*\/cauldron) --env prod install$/\1/p' <<<"${OUT}")"
# F4: the byte check, on a REAL (sealed) run whose stub cauldron's `pin`
# really appends to env.toml — so a run that pinned WOULD change the bytes.
# Its positive sentinel is case 14, where the same stub pin does write.
mkdir -p "${T}/bin14"
CLOG="${T}/cauldron.log"
# The commit the stub git names, and so the stubs' build label for it,
# spelt in pieces (a long hex run reads as a hash).
TL="$(printf 'feed%s1234-release' c0de)"
cat >"${T}/bin14/cauldron" <<'STUBC'
#!/bin/bash
# Stub cauldron: logs argv. `env create` writes env.toml; `pin` APPENDS the
# pin to env.toml, as the real one writes it. `build threads --json` behaves
# like the real build (REL-4.S8c): EVERY call renames FRESH bytes over
# `<label>/threads` and rewrites `<label>/build.json` — there is no "only if
# absent" — so an install that runs it in a root where prod is pinned to
# `<label>` changes prod's image, and 14b sees it. With `--env-copy` it CLAIMS
# a new `<label>+<env>` (then `+<env>.N`) with a plain mkdir and writes only
# there. Either way it prints the record naming the label it wrote.
# REL-4.S5c: `versions install-workspace-probe` is refused the way the real
# one is, naming `<root>/services/…` then `${STUB_WS}/services/…` (the
# workspace this binary "was built from"); STUB_PROBE=garbled prints neither.
# `env create` records whether the install's marker existed when it ran
# (${CLOG}.mark); STUB_CREATE_FAIL=1 fails it without writing anything;
# STUB_CREATE_KILL=1 writes env.toml and then SIGKILLs the install that called
# it, i.e. a stop the instant env create returned.
printf '%s\n' "$*" >>"${CLOG:?}"
root="${HOME}/.cauldron"; e="$2"; shift 2
case "$*" in
    "versions install-workspace-probe")
        if [[ "${STUB_PROBE:-}" == garbled ]]; then echo "cauldron: something else went wrong" >&2; exit 1; fi
        if [[ "${STUB_PROBE:-}" == swapped ]]; then
            printf 'cauldron: no declaration for service `install-workspace-probe`. Looked in:\n  %s/services/install-workspace-probe.toml\n  %s/services/install-workspace-probe.toml\n' "${STUB_WS:?}" "${root}" >&2; exit 1
        fi
        printf 'cauldron: no declaration for service `install-workspace-probe`. Looked in:\n  %s/services/install-workspace-probe.toml\n  %s/services/install-workspace-probe.toml\n' "${root}" "${STUB_WS:?}" >&2
        exit 1 ;;
    "env create ${e} --auto-restart")
        if [[ -e "${root}/data/.install-threads-pending.${e}" ]]; then echo present; else echo absent; fi >"${CLOG}.mark"
        if [[ "${STUB_CREATE_FAIL:-0}" == 1 ]]; then echo "cauldron: REFUSED: environment \`${e}\` already exists (stub)" >&2; exit 1; fi
        /bin/mkdir -p "${root}/data/${e}"; printf '[supervisor]\nauto_restart = true\n' >"${root}/data/${e}/env.toml"
        if [[ "${STUB_CREATE_KILL:-0}" == 1 ]]; then kill -KILL "${PPID}"; exit 1; fi ;;
    "build threads --json"|"build threads --json --env-copy")
        if [[ "${STUB_BUILD_REFUSE:-0}" == 1 ]]; then
            echo "Error: REFUSED to install a version of \`threads\`: the working tree has uncommitted changes (stub)" >&2; exit 1
        fi
        # STUB_BUILD_TOUCH=1: env.toml is edited WHILE the build runs (another
        # writer, between install's 2c check and its pin).
        if [[ "${STUB_BUILD_TOUCH:-0}" == 1 ]]; then printf '# edited during the build\n' >>"${root}/data/${e}/env.toml"; fi
        # STUB_THREADS_REPIN_DURING=1: another writer re-pins threads WHILE the build runs.
        if [[ "${STUB_THREADS_REPIN_DURING:-0}" == 1 ]]; then
            f="${root}/data/${e}/env.toml"
            /usr/bin/sed 's/^version = "threadsold-release"$/version = "otherwriter-threads+prod"/' "${f}" >"${f}.stub" && /bin/mv -f "${f}.stub" "${f}"
        fi
        b="${root}/data/shared/builds/threads"; base="feedc0de1234-release"; /bin/mkdir -p "${b}"
        if [[ "$*" == *--env-copy ]]; then
            k=1; l="${base}+${e}"
            until /bin/mkdir "${b}/${l}" 2>/dev/null; do
                k=$((k+1)); l="${base}+${e}.${k}"; [[ ${k} -lt 100 ]] || exit 70
            done
        else
            l="${base}"; /bin/mkdir -p "${b}/${l}"
        fi
        d="${b}/${l}"; nonce="$$.${RANDOM}${RANDOM}"
        /bin/cp /bin/ls "${d}/.threads.installing"; /bin/chmod u+w "${d}/.threads.installing"
        printf '\nBUILT:%s\n' "${nonce}" >>"${d}/.threads.installing"
        /bin/mv -f "${d}/.threads.installing" "${d}/threads"
        printf '{\n  "service": "threads",\n  "label": "%s",\n  "bin": "threads",\n  "built_at": "%s"\n}\n' "${l}" "${nonce}" >"${d}/build.json"
        printf 'name = "threads"\n' >"${d}/declaration.toml"
        echo "cargo noise on stderr" >&2
        printf '[\n  {\n    "service": "threads",\n    "label": "%s",\n    "bin": "threads"\n  }\n]\n' "${l}" ;;
    "build mcp --json --env-copy")
        # As `build threads --env-copy`: a NEW `<label>+<env>` claimed with a
        # plain mkdir. STUB_MCP_REFUSE=1 refuses it, as a dirty tree would.
        if [[ "${STUB_MCP_REFUSE:-0}" == 1 ]]; then
            echo "Error: REFUSED to install a version of \`mcp\` (stub)" >&2; exit 1
        fi
        # STUB_MCP_REPIN_DURING=1: another writer re-pins mcp WHILE the build runs.
        if [[ "${STUB_MCP_REPIN_DURING:-0}" == 1 ]]; then
            f="${root}/data/${e}/env.toml"
            /usr/bin/sed 's/^version = "mcpold-release+prod"$/version = "otherwriter-release+prod"/' "${f}" >"${f}.stub" && /bin/mv -f "${f}.stub" "${f}"
        fi
        # Its label starts with the commit, as the real one's does (the stub
        # git names it): an install compares an existing pin with it.
        b="${root}/data/shared/builds/mcp"; base="$(git -C "${root}" rev-parse --short=12 HEAD)-release"; /bin/mkdir -p "${b}"
        k=1; l="${base}+${e}"
        until /bin/mkdir "${b}/${l}" 2>/dev/null; do
            k=$((k+1)); l="${base}+${e}.${k}"; [[ ${k} -lt 100 ]] || exit 70
        done
        /bin/cp /bin/ls "${b}/${l}/cauldron-mcp"
        printf '{\n  "service": "mcp",\n  "label": "%s",\n  "bin": "cauldron-mcp"\n}\n' "${l}" >"${b}/${l}/build.json"
        printf '[\n  {\n    "service": "mcp",\n    "label": "%s",\n    "bin": "cauldron-mcp"\n  }\n]\n' "${l}" ;;
    "pin threads "*" --addr auto")
        printf '\n[services.threads]\nversion = "%s"\n' "$3" >>"${root}/data/${e}/env.toml" ;;
    "pin threads "*)
        # A re-pin without --addr (#31): the version rewritten in place, the
        # address and every other line kept, as the real one does.
        f="${root}/data/${e}/env.toml"
        /usr/bin/awk -v v="$3" '/^\[/{sec=$0} sec=="[services.threads]" && /^version = /{print "version = \"" v "\""; next} {print}' "${f}" >"${f}.stub" && /bin/mv -f "${f}.stub" "${f}" ;;
    "pin mcp "*)
        # A first pin appends the section; a re-pin rewrites its version in
        # place (STUB_PIN_REWRITE logs it), as the real one leaves the rest.
        f="${root}/data/${e}/env.toml"
        if /usr/bin/grep -qx '\[services.mcp\]' "${f}"; then
            /usr/bin/awk -v v="$3" '/^\[/{sec=$0} sec=="[services.mcp]" && /^version = /{print "version = \"" v "\""; next} {print}' "${f}" >"${f}.stub" && /bin/mv -f "${f}.stub" "${f}"
        else
            printf '\n[services.mcp]\nversion = "%s"\n' "$3" >>"${f}"
        fi ;;
    "env remove ${e} --yes") /bin/rm -rf "${root}/data/${e}" ;;
    "install"|"install --replace-running") ;;
    "mcp grants")
        # Read-only, as the real one. STUB_NAME_ONLY=1: one grant names its
        # cauldron only by name, said on stderr as the real one says it.
        if [[ "${STUB_NAME_ONLY:-0}" == 1 ]]; then
            echo "cauldron: WARNING: the grant of cauldron \`kb\` in environment \`${e}\` names it only by name (it was written by a pre-release build), so every call to it through the MCP door is refused until it is granted again, in a terminal: cauldron mcp grant kb --env ${e} --scopes read,dump" >&2
        fi
        printf '{"env": "%s", "grants": []}\n' "${e}" ;;
    *) echo "stub cauldron: unexpected: $*" >&2; exit 64 ;;
esac
STUBC
/bin/chmod +x "${T}/bin14/cauldron"; : >"${T}/bin14/tui"; /bin/chmod +x "${T}/bin14/tui"
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${T}/home12" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 ]] && has "threads is already pinned in" && has "mcp is already pinned in"; then ok "sealed real run: rc 0, says both existing pins are kept"; else bad "sealed real run: rc ${RC}" "${OUT}"; fi
if [[ "$(cat "${CLOG}")" == "$(printf '%s\n' '--env prod versions install-workspace-probe' '--env prod install' '--env prod mcp grants')" ]]; then ok "sealed real run: cauldron asked which tree it builds (read-only) and called for install only (no build, no pin)"; else bad "existing env was rebuilt/re-pinned" "$(cat "${CLOG}")"; fi
old_mcp_said "12 sealed real run" "${T}/bin14/cauldron"
if cmp -s "${T}/env12.before" "${T}/home12/.cauldron/data/prod/env.toml"; then ok "env.toml byte-identical after a REAL run"; else bad "env.toml changed" "$(diff "${T}/env12.before" "${T}/home12/.cauldron/data/prod/env.toml")"; fi
sealed_ok "case 12"

printf '\n== 13. existing env.toml WITHOUT threads or mcp -> threads told how, NOT pinned; mcp added once (upgrade)\n'
mkdir -p "${T}/home13/.cauldron/data/prod"
printf '[supervisor]\nauto_restart = true\n' >"${T}/home13/.cauldron/data/prod/env.toml"
RUN_HOME="${T}/home13" run "" 0 --dry-run
if [[ ${RC} -eq 0 ]] && has "threads is NOT pinned in the existing"; then ok "rc 0, says threads is not pinned"; else bad "rc ${RC}" "${OUT}"; fi
if [[ $(line_no "${RE_BUILD}") -eq 0 && $(line_no "${RE_PIN}") -eq 0 ]]; then ok "no threads build or pin step"; else bad "existing env would be edited" "${OUT}"; fi
bm="$(line_no '^install: \$ .*/cauldron --env prod build mcp --json --env-copy$')"; pm="$(line_no '^install: \$ .*/cauldron --env prod pin mcp \\<label\\>\+prod$')"; s5="$(line_no "${RE_STEP5}")"
if has "mcp is NOT pinned in the existing" && has "adding it (upgrade)" && [[ ${bm} -gt 0 && ${pm} -gt ${bm} && ${s5} -gt ${pm} ]]; then ok "13 plan: says mcp is added (upgrade); build mcp --env-copy < pin mcp <label>+prod < step 5"; else bad "13: no mcp upgrade in the plan (build ${bm} pin ${pm} step5 ${s5})" "${OUT}"; fi
# REL-4.S5c M3: the same, as a REAL (sealed) run whose stub `pin` really
# appends to env.toml. Its positive sentinel is 17b: the same stub, with the
# marker present, does pin and does change the bytes.
cp "${T}/home13/.cauldron/data/prod/env.toml" "${T}/env13.before"
[[ ! -e "$(MARK "${T}/home13")" ]] || bad "13: fixture has a marker"
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${T}/home13" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 ]] && has "threads is NOT pinned in the existing"; then ok "13 sealed real run: rc 0, says threads is not pinned"; else bad "13 sealed real run: rc ${RC}" "${OUT}"; fi
if has "mcp is NOT pinned in the existing" && has "adding it (upgrade)"; then ok "13: says mcp is not pinned and is added (upgrade)"; else bad "13: no mcp upgrade line" "${OUT}"; fi
want13="$(printf '%s\n' '--env prod versions install-workspace-probe' '--env prod build mcp --json --env-copy' "--env prod pin mcp ${TL}+prod" '--env prod install' '--env prod mcp grants')"
if [[ "$(cat "${CLOG}")" == "${want13}" ]]; then ok "13 sealed real run: no threads build or pin; mcp built --env-copy and pinned by cauldron pin; install"; else bad "13: cauldron calls wrong" "$(cat "${CLOG}")"; fi
if prefix_of "${T}/env13.before" "$(ENVT "${T}/home13")" && [[ "$(grep -c '^\[services.mcp\]$' "$(ENVT "${T}/home13")")" == 1 ]] && ! grep -q 'services.threads' "$(ENVT "${T}/home13")"; then ok "13: env.toml keeps its bytes and gains exactly one mcp pin (and no threads pin)"; else bad "13: env.toml wrong" "$(cat "$(ENVT "${T}/home13")")"; fi
if has "services: mcp pinned at ${TL}+prod"; then ok "13: done line names the mcp pin"; else bad "13: no mcp done line" "${OUT}"; fi
sealed_ok "case 13"
cp "$(ENVT "${T}/home13")" "${T}/env13.after"; : >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${T}/home13" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 && "$(cat "${CLOG}")" == "$(printf '%s\n' '--env prod versions install-workspace-probe' '--env prod install' '--env prod mcp grants')" ]] && has "mcp is already pinned in" && cmp -s "${T}/env13.after" "$(ENVT "${T}/home13")"; then ok "13 re-run: idempotent (no build, no pin, env.toml byte-identical)"; else bad "13 re-run (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
sealed_ok "case 13 re-run"

printf '\n== 14. the step for real (SEALED): its own new version, signed in the env dir, shared build untouched\n'
B14() { echo "$1/.cauldron/data/shared/builds/threads/$2"; }
sha() { /usr/bin/shasum -a 256 "$1" 2>/dev/null | /usr/bin/awk '{print $1}'; }
# 14a. prod, fresh root: the shared label is not created at all.
: >"${CLOG}"; : >"${EXEC_LOG}"; : >"${SIGN_LOG}"; mkdir -p "${T}/home14"
RUN_HOME="${T}/home14" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 ]]; then ok "rc 0"; else bad "rc ${RC}, wanted 0" "${OUT}"; fi
# The stub's threads label, spelt in pieces (a long hex run reads as a hash).
TL="$(printf 'feed%s1234-release' c0de)"
want="$(printf '%s\n' '--env prod versions install-workspace-probe' '--env prod env create prod --auto-restart' '--env prod build threads --json --env-copy' "--env prod pin threads ${TL}+prod --addr auto" '--env prod build mcp --json --env-copy' "--env prod pin mcp ${TL}+prod" '--env prod install')"
if [[ "$(cat "${CLOG}")" == "${want}" ]]; then ok "cauldron called: workspace probe, env create, build --env-copy, pin threads <label>+prod --addr auto, build mcp --env-copy, pin mcp <label>+prod, install"; else bad "cauldron calls wrong" "$(cat "${CLOG}")"; fi
if grep -qx '\[services.mcp\]' "${T}/home14/.cauldron/data/prod/env.toml" && [[ -d "${T}/home14/.cauldron/data/shared/builds/mcp/${TL}+prod" && ! -e "${T}/home14/.cauldron/data/shared/builds/mcp/${TL}" ]]; then ok "14a: mcp pinned at its own new version; the shared label never created"; else bad "14a: mcp not pinned as its own version" "$(find "${T}/home14/.cauldron/data/shared/builds" 2>&1)"; fi
if has "services: mcp pinned at ${TL}+prod" && has "mcp grant <cauldron> --env prod"; then ok "14a: done line names the mcp pin and the grant command"; else bad "14a: no mcp done line" "${OUT}"; fi
# REL-4.S5c M2/M4: the marker existed when env create ran (sentinel: it WAS
# written, at the path MARK names), and a first install leaves none behind.
if [[ "$(cat "${CLOG}.mark" 2>/dev/null)" == present ]]; then ok "14a: the marker existed when env create ran (written before it)"; else bad "14a: no marker when env create ran" "$(cat "${CLOG}.mark" 2>&1)"; fi
if [[ ! -e "$(MARK "${T}/home14")" ]]; then ok "14a: marker cleared after a first install"; else bad "14a: marker left after a first install" "$(ls -la "$(MARK "${T}/home14")")"; fi
OWN14="$(B14 "${T}/home14" feedc0de1234-release+prod)"
info14="$(EXEC_LOG=/dev/null "${SEAL}/codesign" -dvvv "${OWN14}/threads" 2>&1 || true)"
if [[ "${info14}" == $'Identifier=com.harness.threads\nSignature=adhoc' ]]; then ok "the env's own version is signed as com.harness.threads"; else bad "own copy not signed" "$(ls -la "${OWN14}" 2>&1)"; fi
if [[ ! -e "$(B14 "${T}/home14" feedc0de1234-release)" ]]; then ok "the shared label was never created"; else bad "the shared label was written" "$(ls -la "$(B14 "${T}/home14" feedc0de1234-release)")"; fi
if grep -qF '"label": "feedc0de1234-release+prod"' "${OWN14}/build.json" && [[ -f "${OWN14}/declaration.toml" ]]; then ok "the version's record names its own label and carries the declaration"; else bad "record/declaration wrong" "$(cat "${OWN14}/build.json" 2>&1)"; fi
if grep -q "^signer --sign-only ${T}/home14/.cauldron/data/prod/.threads-image.[0-9]*/threads$" "${SIGN_LOG}"; then ok "signed in the environment's own data dir"; else bad "signed elsewhere" "$(cat "${SIGN_LOG}")"; fi
if [[ -z "$(find "${T}/home14/.cauldron" -name '.threads-image.*' -o -name '.threads.signing.*' | head -n 1)" ]]; then ok "no staging copy left behind"; else bad "staging left" "$(find "${T}/home14/.cauldron")"; fi
if has "services: threads pinned at feedc0de1234-release+prod"; then ok "done line names the pin"; else bad "done line" "${OUT}"; fi
if has "local:    local inference (the inference and ripples services) is installed OFF: nothing pinned, nothing started." \
   && has "To use it: ${T}/home14/.cauldron/bin/cauldron --env prod build inference   (and ripples; from this checkout (cargo))" \
   && ! grep -qE ' (build|pin|start) (inference|ripples)' "${CLOG}"; then ok "14a: inference and ripples neither built, pinned nor started; the closing lines say how"; else bad "14a: local inference lines wrong" "$(cat "${CLOG}")${OUT}"; fi
if helpers_at "${T}/home14" "from the checkout" && has "placed the account helpers (add-account, claude-accounts, claude-login-browser)"; then ok "14a: the account helpers installed from the checkout at share/cauldron/scripts/threads/, 755, directories 755"; else bad "14a: helpers not installed" "$(ls -laR "${T}/home14/.cauldron/share" 2>&1)"; fi
sealed_ok "case 14a"
# 14b. dev, in a root where the shared label ALREADY EXISTS (prod is pinned
# to it, signed) and so does a `+dev` copy from an earlier install. sha256 of
# the shared image AND its build.json, before and after, must be equal, and
# the image's inode too; dev gets `+dev.2`. The stub's plain `build` rewrites
# both every time, so an install that runs one fails here.
H="${T}/home14b"; SH="$(B14 "${H}" feedc0de1234-release)"; DV="$(B14 "${H}" feedc0de1234-release+dev)"
mkdir -p "${SH}" "${DV}"
/bin/cp /bin/ls "${SH}/threads"; printf '\nFAKESIGN:com.harness.threads\n' >>"${SH}/threads"
printf '{\n  "service": "threads",\n  "label": "feedc0de1234-release",\n  "built_at": "prod"\n}\n' >"${SH}/build.json"
/bin/cp /bin/ls "${DV}/threads"; printf '{\n  "label": "feedc0de1234-release+dev"\n}\n' >"${DV}/build.json"
sh_img="$(sha "${SH}/threads")"; sh_rec="$(sha "${SH}/build.json")"; ino_s="$(/usr/bin/stat -f %i "${SH}/threads")"
dv_img="$(sha "${DV}/threads")"; dv_rec="$(sha "${DV}/build.json")"; ino_d="$(/usr/bin/stat -f %i "${DV}/threads")"
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H}" srun "" --from "${T}/bin14" --env dev
if [[ ${RC} -eq 0 ]] && grep -qx -- '--env dev pin threads feedc0de1234-release+dev.2 --addr auto' "${CLOG}"; then ok "dev pins its own NEW version <label>+dev.2 with --addr auto"; else bad "dev pin wrong (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
if [[ -n "${sh_img}" && "$(sha "${SH}/threads")" == "${sh_img}" && "$(/usr/bin/stat -f %i "${SH}/threads")" == "${ino_s}" ]]; then ok "prod's pinned shared image: same sha256, same inode"; else bad "prod's pinned image was replaced or re-signed" "before ${sh_img} ino ${ino_s}; after $(sha "${SH}/threads") ino $(/usr/bin/stat -f %i "${SH}/threads")"; fi
if [[ -n "${sh_rec}" && "$(sha "${SH}/build.json")" == "${sh_rec}" ]]; then ok "prod's pinned shared build.json: same sha256"; else bad "prod's pinned build.json was rewritten" "$(cat "${SH}/build.json")"; fi
if [[ "$(sha "${DV}/threads")" == "${dv_img}" && "$(sha "${DV}/build.json")" == "${dv_rec}" && "$(/usr/bin/stat -f %i "${DV}/threads")" == "${ino_d}" ]]; then ok "an existing +dev copy: same sha256 (image, record), same inode"; else bad "an existing per-env copy was written into"; fi
if grep -q 'FAKESIGN:com.harness.threads' "$(B14 "${H}" feedc0de1234-release+dev.2)/threads" 2>/dev/null; then ok "the new +dev.2 version is the signed one"; else bad "+dev.2 not signed" "$(ls -la "$(B14 "${H}" feedc0de1234-release+dev.2)" 2>&1)"; fi
if [[ ! -e "${H}/Library/Application Support/cauldron/cauldron-bin" ]]; then ok "14b: no install marker for a non-prod environment"; else bad "14b: a dev install wrote the prod marker"; fi
sealed_ok "case 14b"
# 14c. Identity arm with a signer that signs nothing: the verify refuses, so
# install stops BEFORE the pin; the unsigned version it claimed is removed,
# the shared label is never created and the staging copy is removed.
rm -rf "${T}/home14"; mkdir -p "${T}/home14"; : >"${CLOG}"; : >"${EXEC_LOG}"
printf '#!/bin/bash\nexit 0\n' >"${T}/nosign.sh"; /bin/chmod +x "${T}/nosign.sh"
RUN_SIGNER="${T}/nosign.sh" RUN_HOME="${T}/home14" srun "" --from "${T}/bin14"
if [[ ${RC} -ne 0 ]] && has "threads was built but NOT pinned"; then ok "failed signer -> rc ${RC}, refused"; else bad "failed signer not refused (rc ${RC})" "${OUT}"; fi
if grep -qx -- '--env prod build threads --json --env-copy' "${CLOG}" && ! grep -q 'pin threads' "${CLOG}" && ! grep -q -- '--env prod install' "${CLOG}"; then ok "built, then no pin and no install after a failed sign"; else bad "pinned/installed after a failed sign, or never built" "$(cat "${CLOG}")"; fi
if [[ ! -e "$(B14 "${T}/home14" feedc0de1234-release)" && ! -e "$(B14 "${T}/home14" feedc0de1234-release+prod)" ]] && [[ -z "$(find "${T}/home14/.cauldron" -name '.threads-image.*' | head -n 1)" ]]; then ok "no shared label, the unsigned +prod version removed, staging removed"; else bad "a version left behind or staging left" "$(find "${T}/home14/.cauldron")"; fi
sealed_ok "case 14c"

printf '\n== 15. the exec audit catches an absolute-path call and a missing command with stderr hidden\n'
# Positive sentinel for sealed_ok's audit: a copy of the script under
# test with two such calls injected right after `set -euo pipefail`. Neither
# writes, fails or prints, so only the audit can see them.
/usr/bin/awk '{ print } $0 == "set -euo pipefail" { print "/usr/bin/true 2>/dev/null"; print "nosuchcmd15 >/dev/null 2>&1 || :" }' "${INSTALL}" >"${T}/mut15"
if [[ "$(/usr/bin/grep -c -e '^/usr/bin/true 2>/dev/null$' -e '^nosuchcmd15 ' "${T}/mut15")" == 2 ]]; then ok "sentinel script has both injected calls"; else bad "sentinel injection failed" "$(head -n 80 "${T}/mut15")"; fi
: >"${EXEC_LOG}"
SRUN_SRC="${T}/mut15" srun "com.broomstick.cauldron:4242" --from "${T}/empty"
a15="$(exec_audit)"
if [[ ${RC} -eq 2 ]]; then ok "the sentinel run still behaves (rc 2, refused by the guard)"; else bad "sentinel rc ${RC}" "${OUT}"; fi
if ! grep -q 'command not found' <<<"${ERR}"; then ok "its stderr shows nothing (the gap the audit closes)"; else bad "stderr showed it" "${ERR}"; fi
if grep -qx '/usr/bin/true' <<<"${a15}"; then ok "audit names /usr/bin/true (absolute path)"; else bad "audit missed /usr/bin/true" "${a15}"; fi
if grep -qx 'nosuchcmd15' <<<"${a15}"; then ok "audit names nosuchcmd15 (missing, stderr hidden)"; else bad "audit missed nosuchcmd15" "${a15}"; fi
if [[ "$(grep -c . <<<"${a15}")" == 2 ]]; then ok "audit names exactly those two"; else bad "audit named other commands" "${a15}"; fi


# ── THE CLEAN-TREE PRE-FLIGHT AND THE RE-RUN (REL-4.S5b) ────────────────────

printf '\n== 16. a dirty checkout is refused at 2c, before the build and before env.toml (SEALED)\n'
mkdir -p "${T}/home16"; : >"${CLOG}"; : >"${EXEC_LOG}"
STUB_GIT_DIRTY='?? scripts/install.s5-nostep5' RUN_HOME="${T}/home16" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 2 ]] && has "REFUSED: ${T}/repo has uncommitted or untracked files"; then ok "rc 2, refused naming the checkout"; else bad "dirty tree not refused at 2c (rc ${RC})" "${OUT}"; fi
if has "install:     ?? scripts/install.s5-nostep5"; then ok "names the untracked file"; else bad "untracked file not named" "${OUT}"; fi
if ! has "== 3. binaries" && [[ ! -s "${CLOG}" ]]; then ok "stopped before step 3: cauldron never ran"; else bad "went past 2c" "$(cat "${CLOG}")${OUT}"; fi
if [[ -z "$(find "${T}/home16" -mindepth 1 -print -quit)" ]]; then ok "HOME untouched (no env.toml, no marker)"; else bad "HOME written by a refused run" "$(find "${T}/home16")"; fi
sealed_ok "case 16"
STUB_GIT_DIRTY=' M README.md' RUN_HOME="${T}/home16" run "" 0 --dry-run
if [[ ${RC} -eq 2 ]] && has "install:      M README.md" && ! has "== 3. binaries"; then ok "--dry-run refuses a modified file too (read-only check)"; else bad "dry run not refused (rc ${RC})" "${OUT}"; fi
STUB_GIT_FAIL=1 RUN_HOME="${T}/home16" run "" 0 --dry-run
if [[ ${RC} -eq 2 ]] && has "\`git status\` failed in"; then ok "a git status that fails is refused, never read as clean"; else bad "failed git status not refused (rc ${RC})" "${OUT}"; fi
STUB_GIT_DIRTY='?? x' RUN_HOME="${T}/home12" run "" 0 --dry-run
if [[ ${RC} -eq 0 ]] && has "steps 4b and 4c will build nothing"; then ok "opposed: a dirty tree is NOT refused when 4b and 4c build nothing (threads and mcp already pinned)"; else bad "refused although 4b and 4c build nothing (rc ${RC})" "${OUT}"; fi

printf '\n== 17. a re-run COMPLETES an install that stopped before the threads pin (SEALED)\n'
H17="${T}/home17"; mkdir -p "${H17}"; : >"${CLOG}"; : >"${EXEC_LOG}"
STUB_BUILD_REFUSE=1 RUN_HOME="${H17}" srun "" --from "${T}/bin14"
if [[ ${RC} -ne 0 ]] && [[ -f "$(ENVT "${H17}")" ]] && ! grep -q 'services.threads' "$(ENVT "${H17}")" && [[ -e "$(MARK "${H17}")" ]]; then ok "17a partial: rc ${RC}, env.toml created, no threads pin, marker left"; else bad "17a: not the partial state (rc ${RC})" "$(find "${H17}" 2>&1)${OUT}"; fi
if [[ "$(cat "${CLOG}")" == $'--env prod versions install-workspace-probe\n--env prod env create prod --auto-restart\n--env prod build threads --json --env-copy' ]]; then ok "17a: env created, build refused, no pin, no install"; else bad "17a calls" "$(cat "${CLOG}")"; fi
sealed_ok "case 17a"
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H17}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 ]] && has "finishing step 4b now"; then ok "17b re-run: rc 0, says it finishes step 4b"; else bad "17b re-run did not finish 4b (rc ${RC})" "${OUT}"; fi
want17="$(printf '%s\n' '--env prod versions install-workspace-probe' '--env prod build threads --json --env-copy' "--env prod pin threads ${TL}+prod --addr auto" '--env prod build mcp --json --env-copy' "--env prod pin mcp ${TL}+prod" '--env prod install')"
if [[ "$(cat "${CLOG}")" == "${want17}" ]]; then ok "17b: build --env-copy, pin <label>+prod --addr auto, install; no second env create"; else bad "17b calls" "$(cat "${CLOG}")"; fi
if grep -qx '\[services.threads\]' "$(ENVT "${H17}")" && grep -qx 'version = "feedc0de1234-release+prod"' "$(ENVT "${H17}")"; then ok "17b: env.toml now pins threads"; else bad "17b: env.toml has no threads pin" "$(cat "$(ENVT "${H17}")")"; fi
if [[ ! -e "$(MARK "${H17}")" ]]; then ok "17b: marker cleared after the pin"; else bad "17b: marker left after the pin"; fi
if grep -q 'FAKESIGN:com.harness.threads' "$(B14 "${H17}" feedc0de1234-release+prod)/threads" 2>/dev/null; then ok "17b: the pinned version is the signed one"; else bad "17b: pinned version not signed"; fi
if has "PATH:     \`cauldron\` is not on your PATH yet (it is in ${H17}/.cauldron/bin)" && has "echo 'export PATH=\"${H17}/.cauldron/bin:\$PATH\"' >> ~/.zshrc"; then ok "17b: done says exactly how to put cauldron on PATH"; else bad "17b: no PATH instruction" "${OUT}"; fi
bridge_ok "${H17}/.cauldron" "whose instructions say how to install." "17b (real run)" "install: == done"
sealed_ok "case 17b"
cp "$(ENVT "${H17}")" "${T}/env17.before" 2>/dev/null || : >"${T}/env17.before"
mkdir -p "${H17}/.cauldron/data/prod" "${H17}/.cauldron/bin"; : >"$(MARK "${H17}")"
printf '#!/bin/bash\nexit 0\n' >"${H17}/.cauldron/bin/cauldron"; /bin/chmod +x "${H17}/.cauldron/bin/cauldron"
: >"${CLOG}"; : >"${EXEC_LOG}"
SRUN_PATH_EXTRA="${H17}/.cauldron/bin" RUN_HOME="${H17}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 && "$(cat "${CLOG}")" == "$(printf '%s\n' '--env prod versions install-workspace-probe' '--env prod install' '--env prod mcp grants')" ]]; then ok "17c third run (pinned, stale marker): no rebuild, no re-pin"; else bad "17c rebuilt or re-pinned (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
if cmp -s "${T}/env17.before" "$(ENVT "${H17}")" && [[ ! -e "$(MARK "${H17}")" ]]; then ok "17c: env.toml byte-identical, stale marker cleared"; else bad "17c: env.toml changed or marker kept"; fi
if ! has "is not on your PATH yet"; then ok "opposed: no PATH instruction when ${H17}/.cauldron/bin is on PATH"; else bad "PATH instruction printed although cauldron is on PATH" "${OUT}"; fi
sealed_ok "case 17c"

printf '\n== 18. the pre-flight with the REAL git: an untracked file refuses, a clean tree does not\n'
G="${T}/gitrepo"; mkdir -p "${G}/scripts" "${G}/services/threads/scripts" "${T}/home18"
cp "${INSTALL}" "${G}/scripts/install"; printf '# fake\n' >"${G}/services/threads/scripts/build-threads.sh"
(cd "${G}" && /usr/bin/git init -q && /usr/bin/git add scripts/install services/threads/scripts/build-threads.sh \
    && /usr/bin/git -c user.name=t -c user.email=t@example.invalid commit -qm fixture) >/dev/null 2>&1 || bad "fixture repo could not be made"
RUN_REAL_GIT=1 RUN_HOME="${T}/home18" RUN_AS="${G}/scripts/install" run "" 0 --dry-run
if [[ ${RC} -eq 0 ]] && has "clean: " && has "== 3. binaries"; then ok "clean committed tree: rc 0, passes 2c"; else bad "clean tree refused (rc ${RC})" "${OUT}"; fi
printf 'helper\n' >"${G}/scripts/install.s5-nostep5"
RUN_REAL_GIT=1 RUN_HOME="${T}/home18" RUN_AS="${G}/scripts/install" run "" 0 --dry-run
if [[ ${RC} -eq 2 ]] && has "install:     ?? scripts/install.s5-nostep5" && ! has "== 3. binaries"; then ok "one untracked file: rc 2, named, stopped before step 3"; else bad "untracked file not refused by real git (rc ${RC})" "${OUT}"; fi
if [[ -z "$(find "${T}/home18" -mindepth 1 -print -quit)" ]]; then ok "dry runs wrote nothing"; else bad "HOME written" "$(find "${T}/home18")"; fi

printf '\n== 19. the marker is written BEFORE env create (REL-4.S5c M2, SEALED)\n'
# 19a/b: killed the instant env create returned -> a re-run still finishes.
H19="${T}/home19"; mkdir -p "${H19}"; : >"${CLOG}"; : >"${EXEC_LOG}"
STUB_CREATE_KILL=1 RUN_HOME="${H19}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 137 ]] && [[ -f "$(ENVT "${H19}")" ]] && ! grep -q 'services.threads' "$(ENVT "${H19}")"; then ok "19a: killed (rc 137) right after env create: env.toml there, no pin"; else bad "19a: not the killed state (rc ${RC})" "$(find "${H19}" 2>&1)${OUT}"; fi
if [[ -e "$(MARK "${H19}")" ]]; then ok "19a: the marker is there although the install died before writing anything after env create"; else bad "19a: env.toml with NO marker (the unrecoverable state)" "$(find "${H19}" 2>&1)"; fi
sealed_ok "case 19a"
: >"${CLOG}"; : >"${EXEC_LOG}"
# REL-4.S5d: the install died before it could record WHICH env.toml its
# create wrote, so the marker proves nothing about the env.toml there now
# (it could as well be one the operator made since). The re-run REFUSES and
# leaves env.toml byte-identical; it was finished by 19b' below.
cp "$(ENVT "${H19}")" "${T}/env19.before"
RUN_HOME="${H19}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 2 ]] && has "REFUSED: environment 'prod' is half made. An earlier run of this install was" && has "stopped (Ctrl-C, a closed terminal, a crash) while it was creating it"; then ok "19b: an unproven marker beside env.toml -> rc 2, refused, and says it is the stopped install's half-made env"; else bad "19b: not refused, or not said to the one who stopped it (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
# REL-4.S5e M2: the exact commands, for the person who ran it: remove the
# marker, then THIS script with the same arguments. Prod is never removed by
# cauldron, so no `env remove` is offered for it (19d is the dev side).
if grep -qxF "install:   rm -f $(MARK "${H19}")" <<<"${ERR}" && grep -qxF "install:   ${T}/repo/scripts/install --from ${T}/bin14" <<<"${ERR}"; then ok "19b: prints the exact commands: rm -f <marker>, then this install with the same arguments"; else bad "19b: recovery commands missing or wrong" "${ERR}"; fi
if ! has "env remove"; then ok "19b opposed: no \`env remove\` offered for prod (cauldron refuses to remove prod)"; else bad "19b: env remove offered for prod" "${ERR}"; fi
if [[ ! -s "${CLOG}" ]] && cmp -s "${T}/env19.before" "$(ENVT "${H19}")" && [[ -e "$(MARK "${H19}")" ]]; then ok "19b: no cauldron call, env.toml byte-identical, marker left for the operator"; else bad "19b: called cauldron or changed env.toml" "$(cat "${CLOG}")"; fi
sealed_ok "case 19b"
# 19b': doing what the refusal says (remove the marker) -> an existing env
# without threads: told the two commands, never pinned; mcp is added
# (upgrade), and the bytes that were there are kept.
/bin/rm -f "$(MARK "${H19}")"; : >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H19}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 ]] && has "threads is NOT pinned in the existing" && [[ "$(cat "${CLOG}")" == "${want13}" ]] && prefix_of "${T}/env19.before" "$(ENVT "${H19}")" && ! grep -q 'services.threads' "$(ENVT "${H19}")"; then ok "19b': after rm of the marker: rc 0, threads told how, mcp added, the earlier bytes kept"; else bad "19b' (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
sealed_ok "case 19b'"
# 19c: env create FAILS -> its rc, and no marker left behind (opposes an
# install that writes the marker and never takes it back).
H19c="${T}/home19c"; mkdir -p "${H19c}"; : >"${CLOG}"; : >"${EXEC_LOG}"; : >"${CLOG}.mark"
STUB_CREATE_FAIL=1 RUN_HOME="${H19c}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 1 ]] && has "env create prod\` failed (rc 1"; then ok "19c: a failed env create stops the install with its rc"; else bad "19c: rc ${RC}" "${OUT}"; fi
if [[ "$(cat "${CLOG}.mark")" == present && ! -e "$(MARK "${H19c}")" && ! -e "$(ENVT "${H19c}")" ]]; then ok "19c: the marker was there for the create and is removed after it failed"; else bad "19c: marker left (or never written) after a failed create" "mark@create=$(cat "${CLOG}.mark") $(find "${H19c}" 2>&1)"; fi
if ! grep -q 'build threads' "${CLOG}"; then ok "19c: nothing built after the failed create"; else bad "19c: built after a failed create" "$(cat "${CLOG}")"; fi
sealed_ok "case 19c"
# 19d (REL-4.S5e M2): the same stop for --env dev; the refusal's FIRST way
# (env remove, then re-run) is FOLLOWED exactly as printed, and ends in a
# full install with threads.
H19d="${T}/home19d"; mkdir -p "${H19d}"; : >"${CLOG}"; : >"${EXEC_LOG}"
STUB_CREATE_KILL=1 RUN_HOME="${H19d}" srun "" --from "${T}/bin14" --env dev
if [[ ${RC} -eq 137 && -f "${H19d}/.cauldron/data/dev/env.toml" && -e "${H19d}/.cauldron/data/.install-threads-pending.dev" ]]; then ok "19d: dev killed right after env create: env.toml and marker"; else bad "19d: not the killed state (rc ${RC})" "$(find "${H19d}" 2>&1)${OUT}"; fi
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H19d}" srun "" --from "${T}/bin14" --env dev
rm19d="$(grep -E '^install:   [^ ]+/cauldron --env dev env remove dev --yes$' <<<"${ERR}" | sed -E 's/^install:   //' || true)"
if [[ ${RC} -eq 2 ]] && has "REFUSED: environment 'dev' is half made" && [[ "${rm19d}" == "${T}/bin14/cauldron --env dev env remove dev --yes" ]] && [[ "$(grep -cxF "install:   ${T}/repo/scripts/install --from ${T}/bin14 --env dev" <<<"${ERR}")" == 2 ]]; then ok "19d: refused; offers env remove dev --yes then the exact re-run, and the keep-it way"; else bad "19d: dev refusal wrong (rc ${RC})" "${ERR}"; fi
if [[ -n "${rm19d}" ]]; then HOME="${H19d}" CLOG="${CLOG}" /bin/bash -c "${rm19d}"; fi
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H19d}" srun "" --from "${T}/bin14" --env dev
if [[ ${RC} -eq 0 ]] && has "stale marker" && grep -qx -- '--env dev pin threads feedc0de1234-release+dev --addr auto' "${CLOG}" && [[ ! -e "${H19d}/.cauldron/data/.install-threads-pending.dev" ]]; then ok "19d: after the printed env remove, the printed re-run is a full install (created, pinned, marker cleared)"; else bad "19d: the printed recovery did not finish the install (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
sealed_ok "case 19d"

printf '\n== 20. 2c checks the tree `cauldron build threads` compiles, not only this checkout (REL-4.S5c M1)\n'
WS="${T}/otherws"; mkdir -p "${WS}" "${T}/home20"
# 20a: --from names a cauldron built from ANOTHER checkout, which is dirty;
# this one is clean. Refused at 2c, naming that tree, before anything.
: >"${CLOG}"; : >"${EXEC_LOG}"; : >"${T}/gitdirs.log"
STUB_WS="${WS}" STUB_GIT_DIRTY_AT="${WS}" STUB_GIT_DIRTY='?? scratch.txt' RUN_HOME="${T}/home20" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 2 ]] && has "REFUSED: ${WS} has uncommitted or untracked files" && has "the workspace ${T}/bin14/cauldron was built from"; then ok "20a: rc 2, refuses the --from binary's dirty workspace, and says why it is checked"; else bad "20a: the binary's dirty workspace was not refused (rc ${RC})" "${OUT}"; fi
if grep -qx "gitstatus ${T}/repo" "${T}/gitdirs.log" && grep -qx "gitstatus ${WS}" "${T}/gitdirs.log"; then ok "20a: both trees were asked (this checkout and the binary's)"; else bad "20a: trees asked" "$(cat "${T}/gitdirs.log")"; fi
if ! has "== 3. binaries" && [[ "$(cat "${CLOG}")" == "--env prod versions install-workspace-probe" ]] && [[ -z "$(find "${T}/home20" -mindepth 1 -print -quit)" ]]; then ok "20a: stopped before step 3; only the read-only probe ran; HOME untouched"; else bad "20a: went past 2c" "$(cat "${CLOG}")$(find "${T}/home20")"; fi
sealed_ok "case 20a"
# 20b-20d each get a FRESH HOME: one shared with 20a would be pinned by a
# script that let 20a through, and then skip 2c for a reason of its own.
# 20b (opposed): the same --from binary, its workspace CLEAN -> passes 2c,
# names that tree as checked, and the plan goes on.
mkdir -p "${T}/home20b"; STUB_WS="${WS}" RUN_HOME="${T}/home20b" run "" 0 --dry-run --from "${T}/bin14"
if [[ ${RC} -eq 0 ]] && has "step 4b compiles another tree: ${WS}" && has "clean: ${WS} at" && has "== 3. binaries"; then ok "20b: a clean other workspace passes 2c, named"; else bad "20b: clean other workspace refused (rc ${RC})" "${OUT}"; fi
# 20c (opposed the other way): the --from binary built from THIS checkout,
# dirty elsewhere -> not refused, and checked once.
: >"${T}/gitdirs.log"; mkdir -p "${T}/home20c"
STUB_GIT_DIRTY_AT="${WS}" STUB_GIT_DIRTY='?? scratch.txt' STUB_WS="$(cd "${INSTALL%/*}/.." && pwd -P)" RUN_HOME="${T}/home20c" run "" 0 --dry-run --from "${T}/bin14"
if [[ ${RC} -eq 0 ]] && ! has "compiles another tree" && [[ "$(grep -c . "${T}/gitdirs.log")" == 1 ]]; then ok "20c: a binary built from this checkout: one tree checked, a dirty unrelated tree ignored"; else bad "20c (rc ${RC})" "$(cat "${T}/gitdirs.log")${OUT}"; fi
# 20d: a binary that does not answer the probe -> refused, never guessed.
mkdir -p "${T}/home20d"; STUB_PROBE=garbled RUN_HOME="${T}/home20d" run "" 0 --dry-run --from "${T}/bin14"
if [[ ${RC} -eq 2 ]] && has "did not say which workspace it was built from" && ! has "== 3. binaries"; then ok "20d: a binary that does not name its workspace is refused at 2c"; else bad "20d: rc ${RC}" "${OUT}"; fi
# 20e: an override declaration names its own [build] workspace (~-relative):
# THAT tree is checked even with no --from at all.
mkdir -p "${T}/home20e/.cauldron/services" "${T}/home20e/ovrws"
printf 'name = "threads"\n\n[build]\npackage = "threads"\nworkspace = "~/ovrws"\n' >"${T}/home20e/.cauldron/services/threads.toml"
STUB_GIT_DIRTY_AT="${T}/home20e/ovrws" STUB_GIT_DIRTY=' M x.rs' RUN_HOME="${T}/home20e" run "" 0 --dry-run
if [[ ${RC} -eq 2 ]] && has "REFUSED: ${T}/home20e/ovrws has uncommitted or untracked files" && has "[build] workspace in ${T}/home20e/.cauldron/services/threads.toml"; then ok "20e: an override's dirty [build] workspace (~/…) is refused at 2c"; else bad "20e: override workspace not checked (rc ${RC})" "${OUT}"; fi
STUB_GIT_DIRTY_AT="${WS}" STUB_GIT_DIRTY=' M x.rs' RUN_HOME="${T}/home20e" run "" 0 --dry-run
if [[ ${RC} -eq 0 ]] && has "clean: ${T}/home20e/ovrws at"; then ok "20e opposed: the same override, clean, passes"; else bad "20e opposed (rc ${RC})" "${OUT}"; fi

# ── THE MARKER PROVES OWNERSHIP (REL-4.S5d) ─────────────────────────────────
printf '\n== 21. a STALE marker (install killed inside env create) beside HIS env.toml: never edited (SEALED; inspect REL-4.S5c-I B1)\n'
# bin21: `env create` SIGKILLs the install BEFORE it writes env.toml (a
# Ctrl-C or kill while the real create waits on the writers' lock).
mkdir -p "${T}/bin21"
cat >"${T}/bin21/cauldron" <<STUBK
#!/bin/bash
case "\$*" in
    *"env create "*) printf '%s\n' "\$*" >>"\${CLOG:?}"; kill -KILL "\${PPID}"; exit 1 ;;
esac
exec "${T}/bin14/cauldron" "\$@"
STUBK
/bin/chmod +x "${T}/bin21/cauldron"; : >"${T}/bin21/tui"; /bin/chmod +x "${T}/bin21/tui"
H21="${T}/home21"; mkdir -p "${H21}"; : >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H21}" srun "" --from "${T}/bin21"
if [[ ${RC} -eq 137 && -e "$(MARK "${H21}")" && ! -e "$(ENVT "${H21}")" ]]; then ok "21: run 1 killed inside env create: a marker, no env.toml"; else bad "21: not the stale state (rc ${RC})" "$(find "${H21}" 2>&1)${OUT}"; fi
sealed_ok "case 21 run 1"
# The operator then makes his OWN prod env, no threads (stand-in for
# `cauldron env create prod` or `cauldron pin …`'s ensure_manifest).
mkdir -p "${H21}/.cauldron/data/prod"; printf '[supervisor]\nauto_restart = false\n' >"$(ENVT "${H21}")"
cp "$(ENVT "${H21}")" "${T}/env21.before"
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H21}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 2 ]] && has "REFUSED: environment 'prod' is half made" && has "If you made or edited this env.toml yourself, that is the"; then ok "21: re-run over HIS env.toml: rc 2, refused, and tells its owner which way keeps it"; else bad "21: not refused (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
if cmp -s "${T}/env21.before" "$(ENVT "${H21}")"; then ok "21: HIS env.toml byte-identical"; else bad "21: HIS env.toml was EDITED by install" "$(diff "${T}/env21.before" "$(ENVT "${H21}")")"; fi
if ! grep -qE 'build threads|pin threads|env create' "${CLOG}"; then ok "21: no build, no pin, no env create"; else bad "21: built/pinned/created" "$(cat "${CLOG}")"; fi
sealed_ok "case 21 run 2"
# 21b (opposed): the same stale marker and NO env.toml -> the create never
# finished: the marker is removed and the run is a first install (it does
# create and pin), so a script that refuses every marker fails here.
H21b="${T}/home21b"; mkdir -p "${H21b}/.cauldron/data"
printf '# old\nstate=creating\ntoml=\n' >"$(MARK "${H21b}")"
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H21b}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 ]] && has "stale marker $(MARK "${H21b}")" && [[ "$(cat "${CLOG}")" == "${want}" ]]; then ok "21b: marker with no env.toml: called stale, then a full first install (create, build, pin, install)"; else bad "21b (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
if grep -qx 'version = "feedc0de1234-release+prod"' "$(ENVT "${H21b}")" && [[ ! -e "$(MARK "${H21b}")" ]]; then ok "21b: pinned, marker cleared"; else bad "21b: not pinned or marker kept"; fi
sealed_ok "case 21b"

printf '\n== 22. `cauldron env remove` leaves the marker: a later env is never the install'"'"'s (SEALED, --env dev)\n'
MARKD() { echo "$1/.cauldron/data/.install-threads-pending.dev"; }
ENVD() { echo "$1/.cauldron/data/dev/env.toml"; }
# 22-setup: an install of dev that stopped before the pin (build refused):
# env.toml created by it, the marker carries its fingerprint.
H22="${T}/home22"; mkdir -p "${H22}"; : >"${CLOG}"; : >"${EXEC_LOG}"
STUB_BUILD_REFUSE=1 RUN_HOME="${H22}" srun "" --from "${T}/bin14" --env dev
if [[ ${RC} -ne 0 && -f "$(ENVD "${H22}")" ]] && grep -qx 'state=created' "$(MARKD "${H22}")" && grep -qE '^toml=[0-9]+:[0-9]+:[0-9]+ sha256=[0-9a-f]{64}$' "$(MARKD "${H22}")"; then ok "22: a partial dev install: env.toml, marker state=created with dev:inode:birth and sha256"; else bad "22: not the partial state (rc ${RC})" "$(cat "$(MARKD "${H22}")" 2>&1)${OUT}"; fi
sealed_ok "case 22 setup"
cp -Rp "${H22}" "${T}/home22.partial"
# A COPY of that state is a new file (new inode), so each scenario below
# first re-records the marker for its copy, exactly in the install's format
# (prove_copy). 22d is the sentinel that a copy so proven IS resumed; 22b
# and 22c then change ONE component each (identity only, bytes only).
prove_copy() {
    local f; f="$(ENVD "$1")"
    printf '# copy\nstate=created\ntoml=%s sha256=%s\n' "$(/usr/bin/stat -f '%d:%i:%B' "${f}")" \
        "$(/usr/bin/shasum -a 256 <"${f}" | /usr/bin/awk '{print $1}')" >"$(MARKD "$1")"
}
# 22a: `cauldron env remove dev` (it deletes data/dev with everything in it,
# cli/src/envs.rs verb_env_remove) -> the marker is stale. A re-run whose
# checkout is dirty (refused at 2c) still removes it and edits nothing.
/bin/rm -rf "${H22}/.cauldron/data/dev"; : >"${CLOG}"; : >"${EXEC_LOG}"
STUB_GIT_DIRTY='?? x' RUN_HOME="${H22}" srun "" --from "${T}/bin14" --env dev
if [[ ${RC} -eq 2 ]] && has "stale marker $(MARKD "${H22}")" && [[ ! -e "$(MARKD "${H22}")" && ! -e "$(ENVD "${H22}")" ]] && [[ ! -s "${CLOG}" ]]; then ok "22a: marker without its env: removed as stale; no env.toml written, cauldron never ran"; else bad "22a (rc ${RC})" "$(find "${H22}/.cauldron" 2>&1)$(cat "${CLOG}")${OUT}"; fi
sealed_ok "case 22a"
# 22b: removed, then HE re-creates dev with the SAME BYTES the install's
# create wrote (`cauldron env create dev --auto-restart`): only the file's
# identity differs, and that alone must refuse.
H22b="${T}/home22b"; cp -Rp "${T}/home22.partial" "${H22b}"; prove_copy "${H22b}"
cp "$(ENVD "${H22b}")" "${T}/env22.bytes"
/bin/rm -rf "${H22b}/.cauldron/data/dev"; mkdir -p "${H22b}/.cauldron/data/dev"; cp "${T}/env22.bytes" "$(ENVD "${H22b}")"
cp "$(ENVD "${H22b}")" "${T}/env22b.before"
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H22b}" srun "" --from "${T}/bin14" --env dev
if [[ ${RC} -eq 2 ]] && has "its env.toml has been replaced or edited since" && cmp -s "${T}/env22b.before" "$(ENVD "${H22b}")" && ! grep -qE 'build|pin' "${CLOG}"; then ok "22b: env removed and re-made by him with identical bytes: rc 2, byte-identical, no build or pin"; else bad "22b: his re-made env was not protected (rc ${RC})" "$(cat "${CLOG}")$(diff "${T}/env22b.before" "$(ENVD "${H22b}")")${OUT}"; fi
sealed_ok "case 22b"
# 22c: the SAME file (same inode), edited by him since: other bytes refuse.
H22c="${T}/home22c"; cp -Rp "${T}/home22.partial" "${H22c}"; prove_copy "${H22c}"
printf '\n[supervisor.extra]\nnote = "mine"\n' >>"$(ENVD "${H22c}")"; cp "$(ENVD "${H22c}")" "${T}/env22c.before"
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H22c}" srun "" --from "${T}/bin14" --env dev
if [[ ${RC} -eq 2 ]] && cmp -s "${T}/env22c.before" "$(ENVD "${H22c}")" && ! grep -qE 'build|pin' "${CLOG}"; then ok "22c: the install's env.toml edited since (same inode, other bytes): rc 2, byte-identical"; else bad "22c (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
sealed_ok "case 22c"
# 22d (opposed): the untouched partial install, proven for its copy, IS
# resumed. (The install's OWN fingerprint resuming is 17b.)
H22d="${T}/home22d"; cp -Rp "${T}/home22.partial" "${H22d}"; prove_copy "${H22d}"
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H22d}" srun "" --from "${T}/bin14" --env dev
if [[ ${RC} -eq 0 ]] && has "finishing step 4b now" && grep -qx -- '--env dev pin threads feedc0de1234-release+dev --addr auto' "${CLOG}" && [[ ! -e "$(MARKD "${H22d}")" ]]; then ok "22d: a marker whose fingerprint matches: resumed, pinned, marker cleared"; else bad "22d: a proven marker was not resumed (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
sealed_ok "case 22d"
# 22e (REL-4.S5e, inspect REL-4.S5d-I M1): a `cp -p` RESTORE of the install's
# env.toml (a backup copied back after `env remove`): macOS cp -p keeps the
# birth time, the bytes are the same, so ONLY THE INODE tells it from the
# file install created. Deterministic, unlike 22b, which also differs in
# birth whenever the re-made file lands in another second.
H22e="${T}/home22e"; cp -Rp "${T}/home22.partial" "${H22e}"; prove_copy "${H22e}"
f22e="$(ENVD "${H22e}")"
id22e_before="$(/usr/bin/stat -f '%d %i %B' "${f22e}")"; sha22e_before="$(sha "${f22e}")"
/bin/cp -p "${f22e}" "${T}/env22e.bak"; /bin/rm -rf "${H22e}/.cauldron/data/dev"; mkdir -p "${H22e}/.cauldron/data/dev"; /bin/cp -p "${T}/env22e.bak" "${f22e}"
id22e_after="$(/usr/bin/stat -f '%d %i %B' "${f22e}")"
read -r d0 i0 b0 <<<"${id22e_before}"; read -r d1 i1 b1 <<<"${id22e_after}"
if [[ "${d0}" == "${d1}" && "${b0}" == "${b1}" && "$(sha "${f22e}")" == "${sha22e_before}" && "${i0}" != "${i1}" ]]; then ok "22e fixture: same device, birth and sha256; only the inode differs (${i0} -> ${i1})"; else bad "22e fixture: not an inode-only difference" "before ${id22e_before} ${sha22e_before}; after ${id22e_after} $(sha "${f22e}")"; fi
cp "${f22e}" "${T}/env22e.before"
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H22e}" srun "" --from "${T}/bin14" --env dev
if [[ ${RC} -eq 2 ]] && has "its env.toml has been replaced or edited since" && cmp -s "${T}/env22e.before" "${f22e}" && ! grep -qE 'build|pin' "${CLOG}"; then ok "22e: a restored copy (only the inode differs): rc 2, byte-identical, no build or pin"; else bad "22e: an inode-only change was resumed (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
sealed_ok "case 22e"

printf '\n== 23. env.toml changed WHILE the build ran: the pin is refused (re-check right before it, SEALED)\n'
H23="${T}/home23"; mkdir -p "${H23}"; : >"${CLOG}"; : >"${EXEC_LOG}"
STUB_BUILD_TOUCH=1 RUN_HOME="${H23}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 2 ]] && has "is no longer the env.toml this install created" && ! grep -q 'pin threads' "${CLOG}" && ! grep -q 'services.threads' "$(ENVT "${H23}")"; then ok "23: rc 2, no pin, env.toml carries only the other writer's edit"; else bad "23: pinned into a changed env.toml (rc ${RC})" "$(cat "${CLOG}")$(cat "$(ENVT "${H23}")")${OUT}"; fi
sealed_ok "case 23"

printf '\n== 24. 2c: the probe'"'"'s first line must be <root>/services (inspect REL-4.S5c-I M-a), and --from must hold a cauldron (M-c)\n'
# The two paths in SWAPPED order, with <root> existing and clean: dropping
# the first-line check would take <root> as the build tree and pass.
mkdir -p "${T}/home24/.cauldron"
STUB_PROBE=swapped RUN_HOME="${T}/home24" run "" 0 --dry-run --from "${T}/bin14"
if [[ ${RC} -eq 2 ]] && has "did not say which workspace it was built from" && ! has "== 3. binaries"; then ok "24a: probe paths in swapped order: refused at 2c"; else bad "24a: swapped probe accepted (rc ${RC})" "${OUT}"; fi
mkdir -p "${T}/home24b"; RUN_HOME="${T}/home24b" run "" 0 --dry-run --from "${T}/empty"
if [[ ${RC} -eq 2 ]] && has "holds no executable cauldron"; then ok "24b: --from with no cauldron: refused at 2c"; else bad "24b (rc ${RC})" "${OUT}"; fi

printf '\n== 25. 2c reads an override'"'"'s build.workspace in every TOML spelling, or refuses (inspect REL-4.S5c-I M-b)\n'
for form in inline dotted; do
  H25="${T}/home25${form}"; mkdir -p "${H25}/.cauldron/services" "${H25}/ovrws"
  if [[ ${form} == inline ]]; then printf 'name = "threads"\nbuild = { package = "threads", workspace = "~/ovrws" }\n' >"${H25}/.cauldron/services/threads.toml"
  else printf 'name = "threads"\nbuild.workspace = "~/ovrws"\n' >"${H25}/.cauldron/services/threads.toml"; fi
  STUB_GIT_DIRTY_AT="${H25}/ovrws" STUB_GIT_DIRTY=' M x.rs' RUN_HOME="${H25}" run "" 0 --dry-run
  if [[ ${RC} -eq 2 ]] && has "REFUSED: ${H25}/ovrws has uncommitted or untracked files" && ! has "== 3. binaries"; then ok "25 ${form}: the override's dirty workspace is refused at 2c"; else bad "25 ${form}: not read (rc ${RC})" "${OUT}"; fi
  STUB_GIT_DIRTY_AT="${WS}" STUB_GIT_DIRTY=' M x.rs' RUN_HOME="${H25}" run "" 0 --dry-run
  if [[ ${RC} -eq 0 ]] && has "clean: ${H25}/ovrws at"; then ok "25 ${form} opposed: clean, checked by name, passes"; else bad "25 ${form} opposed (rc ${RC})" "${OUT}"; fi
done
H25="${T}/home25sq"; mkdir -p "${H25}/.cauldron/services"
printf "[build]\nworkspace = '~/ovrws'\n" >"${H25}/.cauldron/services/threads.toml"
RUN_HOME="${H25}" run "" 0 --dry-run
if [[ ${RC} -eq 2 ]] && has "in a form this script cannot read"; then ok "25 unreadable (single-quoted): refused early, never skipped"; else bad "25 unreadable (rc ${RC})" "${OUT}"; fi
H25="${T}/home25other"; mkdir -p "${H25}/.cauldron/services"
printf 'name = "threads"\n[other]\nworkspace = "/nowhere"\n' >"${H25}/.cauldron/services/threads.toml"
RUN_HOME="${H25}" run "" 0 --dry-run
if [[ ${RC} -eq 0 ]] && ! has "compiles another tree"; then ok "25 opposed: a workspace key outside build is not a build workspace"; else bad "25 other-table (rc ${RC})" "${OUT}"; fi

printf '\n== 26. a marker write stopped between its temp file and the rename leaves no temp file (REL-4.S5e M3, SEALED)\n'
tmps() { find "$1/.cauldron/data" -maxdepth 1 -name '.install-threads-pending.*.tmp.*' 2>/dev/null || true; }
# 26a: SIGTERM at the rename -> the EXIT trap removes the temp file.
H26a="${T}/home26a"; mkdir -p "${H26a}"; : >"${CLOG}"; : >"${EXEC_LOG}"
STUB_MV_KILL=TERM RUN_HOME="${H26a}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 143 ]] && grep -qE '^mv -f .*/\.install-threads-pending\.prod\.tmp\.[0-9]+ ' "${EXEC_LOG}" && [[ "$(cat "${CLOG}")" == "--env prod versions install-workspace-probe" ]]; then ok "26a: stopped by SIGTERM at the marker's rename (rc 143, before env create)"; else bad "26a: not stopped at the rename (rc ${RC})" "$(cat "${CLOG}")$(cat "${EXEC_LOG}")${OUT}"; fi
if [[ -z "$(tmps "${H26a}")" && ! -e "$(MARK "${H26a}")" ]]; then ok "26a: no temp marker left, no marker"; else bad "26a: temp marker left after SIGTERM" "$(find "${H26a}" 2>&1)"; fi
sealed_ok "case 26a"
# 26b: SIGKILL there (no trap can run) -> the temp file IS left (sentinel that
# the stop landed between write and rename); the next run removes it and is
# a full first install.
H26b="${T}/home26b"; mkdir -p "${H26b}"; : >"${CLOG}"; : >"${EXEC_LOG}"
STUB_MV_KILL=KILL RUN_HOME="${H26b}" srun "" --from "${T}/bin14"
t26b="$(tmps "${H26b}")"
if [[ ${RC} -eq 137 && -n "${t26b}" && ! -e "$(MARK "${H26b}")" ]] && grep -qx 'state=creating' "${t26b}"; then ok "26b: SIGKILL at the rename leaves the temp file (state=creating), no marker"; else bad "26b: not the killed-mid-write state (rc ${RC})" "$(find "${H26b}" 2>&1)${OUT}"; fi
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H26b}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 ]] && has "removing ${t26b}: a temp marker" && [[ "$(cat "${CLOG}")" == "${want}" ]]; then ok "26b: the next run removes it and is a full first install"; else bad "26b: re-run (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
if [[ -z "$(tmps "${H26b}")" && ! -e "$(MARK "${H26b}")" ]]; then ok "26b: no temp marker and no marker left"; else bad "26b: temp marker left" "$(find "${H26b}/.cauldron/data" -maxdepth 1)"; fi
sealed_ok "case 26b"
# 26c (opposed): a temp marker whose pid is ALIVE (this suite's) is another
# install's write in flight: left alone, as is one with no pid in its name.
H26c="${T}/home26c"; mkdir -p "${H26c}/.cauldron/data"
printf 'state=creating\n' >"${H26c}/.cauldron/data/.install-threads-pending.prod.tmp.$$"
printf 'state=creating\n' >"${H26c}/.cauldron/data/.install-threads-pending.prod.tmp.x1"
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H26c}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 && -f "${H26c}/.cauldron/data/.install-threads-pending.prod.tmp.$$" && -f "${H26c}/.cauldron/data/.install-threads-pending.prod.tmp.x1" ]] && ! has "a temp marker an install"; then ok "26c: a live install's temp marker and a non-pid one are left alone; the run completes"; else bad "26c: removed a temp marker that was not stale (rc ${RC})" "$(find "${H26c}/.cauldron/data" -maxdepth 1)${OUT}"; fi
sealed_ok "case 26c"

printf '\n== 29. a re-run COMPLETES an install that stopped between the threads pin and the mcp pin (SEALED)\n'
# The stub's `build mcp` refuses (as on a dirty tree): env.toml then holds
# the threads pin (an edit AFTER the marker's first fingerprint), so the
# marker must have been bound to it again for the re-run to resume.
H29="${T}/home29"; mkdir -p "${H29}"; : >"${CLOG}"; : >"${EXEC_LOG}"
STUB_MCP_REFUSE=1 RUN_HOME="${H29}" srun "" --from "${T}/bin14"
if [[ ${RC} -ne 0 ]] && grep -qx '\[services.threads\]' "$(ENVT "${H29}")" && ! grep -q 'services.mcp' "$(ENVT "${H29}")" && [[ -e "$(MARK "${H29}")" ]]; then ok "29a partial: rc ${RC}, threads pinned, no mcp pin, marker left"; else bad "29a: not the partial state (rc ${RC})" "$(cat "$(ENVT "${H29}")" 2>&1)${OUT}"; fi
want29a="$(printf '%s\n' '--env prod versions install-workspace-probe' '--env prod env create prod --auto-restart' '--env prod build threads --json --env-copy' "--env prod pin threads ${TL}+prod --addr auto" '--env prod build mcp --json --env-copy')"
if [[ "$(cat "${CLOG}")" == "${want29a}" ]]; then ok "29a: threads built and pinned, mcp build refused, no install"; else bad "29a calls" "$(cat "${CLOG}")"; fi
if [[ -z "$(find "${H29}/.cauldron/data/shared/builds" -path '*/mcp/*' -print -quit 2>/dev/null)" ]]; then ok "29a: no mcp version left behind"; else bad "29a: an mcp version left" "$(find "${H29}/.cauldron/data/shared/builds")"; fi
sealed_ok "case 29a"
cp "$(ENVT "${H29}")" "${T}/env29.before"
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H29}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 ]] && has "pinned threads and stopped before pinning mcp"; then ok "29b re-run: rc 0, says it finishes step 4c"; else bad "29b re-run (rc ${RC})" "${OUT}"; fi
if [[ "$(cat "${CLOG}")" == "$(printf '%s\n' '--env prod versions install-workspace-probe' '--env prod build mcp --json --env-copy' "--env prod pin mcp ${TL}+prod" '--env prod install')" ]]; then ok "29b: no env create, no threads build or pin; mcp built and pinned; install"; else bad "29b calls" "$(cat "${CLOG}")"; fi
if grep -qx '\[services.mcp\]' "$(ENVT "${H29}")" && [[ "$(grep -c '^\[services.threads\]$' "$(ENVT "${H29}")")" == 1 ]] && [[ ! -e "$(MARK "${H29}")" ]]; then ok "29b: env.toml pins mcp, threads pinned once, marker cleared"; else bad "29b: wrong end state" "$(cat "$(ENVT "${H29}")")"; fi
sealed_ok "case 29b"
cp "$(ENVT "${H29}")" "${T}/env29.after"
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H29}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 && "$(cat "${CLOG}")" == "$(printf '%s\n' '--env prod versions install-workspace-probe' '--env prod install' '--env prod mcp grants')" ]] && has "mcp is already pinned in" && cmp -s "${T}/env29.after" "$(ENVT "${H29}")"; then ok "29c: a third run builds and pins nothing, env.toml byte-identical"; else bad "29c (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
sealed_ok "case 29c"
# 29d: an env.toml edited after the threads pin is not the one the marker
# records. The marker's own job (the threads pin) is done, so it protects
# nothing: the re-run clears it and adds mcp as an upgrade, keeping the edit.
# (A marker that does not match an env.toml WITHOUT threads still refuses:
# 19b, 21, 22b, 22c, 22e.)
H29d="${T}/home29d"; mkdir -p "${H29d}"; : >"${CLOG}"; : >"${EXEC_LOG}"
STUB_MCP_REFUSE=1 RUN_HOME="${H29d}" srun "" --from "${T}/bin14"
printf '# edited by hand\n' >>"$(ENVT "${H29d}")"; cp "$(ENVT "${H29d}")" "${T}/env29d.before"
: >"${CLOG}"
RUN_HOME="${H29d}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 ]] && has "an earlier install pinned threads and stopped before clearing its marker" && [[ "$(cat "${CLOG}")" == "${want13}" ]]; then ok "29d: threads left as it is, mcp built and pinned as an upgrade"; else bad "29d (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
if prefix_of "${T}/env29d.before" "$(ENVT "${H29d}")" && [[ "$(grep -c '^\[services.threads\]$' "$(ENVT "${H29d}")")" == 1 && "$(grep -c '^\[services.mcp\]$' "$(ENVT "${H29d}")")" == 1 && ! -e "$(MARK "${H29d}")" ]]; then ok "29d: the hand edit kept, one threads pin, one mcp pin, marker cleared"; else bad "29d: wrong end state" "$(cat "$(ENVT "${H29d}")")"; fi
sealed_ok "case 29d"

printf '\n== 30. the UPGRADE: an existing env with threads pinned and no mcp gets mcp, once (SEALED)\n'
# An environment made before mcp shipped: threads pinned, no mcp pin, a
# holder loaded, upgraded with --replace-running as the docs say.
H30="${T}/home30"; mkdir -p "${H30}/.cauldron/data/prod"
printf '[supervisor]\nauto_restart = true\n\n[services.threads]\nversion = "%s+prod"\naddr = "127.0.0.1:3653"\n' "${TL}" >"$(ENVT "${H30}")"
cp "$(ENVT "${H30}")" "${T}/env30.before"
# 30a: the plan.
RUN_HOME="${H30}" run "ai.broomstick.cauldron:4242" 0 --dry-run --replace-running
bm="$(line_no '^install: \$ .*/cauldron --env prod build mcp --json --env-copy$')"; pm="$(line_no '^install: \$ .*/cauldron --env prod pin mcp \\<label\\>\+prod$')"; s5="$(line_no "${RE_STEP5}")"
if [[ ${RC} -eq 0 ]] && has "mcp is NOT pinned in the existing" && has "adding it (upgrade)" && [[ ${bm} -gt 0 && ${pm} -gt ${bm} && ${s5} -gt ${pm} ]]; then ok "30a plan: says mcp is added (upgrade); build mcp < pin mcp < step 5"; else bad "30a plan (rc ${RC}, build ${bm} pin ${pm} step5 ${s5})" "${OUT}"; fi
if [[ $(line_no "${RE_BUILD}") -eq 0 ]] && ! grep -qE -- "${RE_ANYPIN}" <<<"${OUT}" && has "threads is already pinned in"; then ok "30a plan: threads neither built nor re-pinned"; else bad "30a: threads would be touched" "${OUT}"; fi
if cmp -s "${T}/env30.before" "$(ENVT "${H30}")"; then ok "30a: the dry run wrote nothing"; else bad "30a: the dry run changed env.toml"; fi
# 30b: for real.
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H30}" srun "ai.broomstick.cauldron:4242" --from "${T}/bin14" --replace-running
want30="$(printf '%s\n' '--env prod versions install-workspace-probe' '--env prod build mcp --json --env-copy' "--env prod pin mcp ${TL}+prod" '--env prod install --replace-running' '--env prod mcp grants')"
if [[ ${RC} -eq 0 && "$(cat "${CLOG}")" == "${want30}" ]]; then ok "30b: mcp built --env-copy and pinned by cauldron pin, then install --replace-running"; else bad "30b calls (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
if prefix_of "${T}/env30.before" "$(ENVT "${H30}")" && [[ "$(grep -c '^\[services.mcp\]$' "$(ENVT "${H30}")")" == 1 ]] && grep -qx "version = \"${TL}+prod\"" "$(ENVT "${H30}")"; then ok "30b: the threads pin is kept byte for byte; one mcp pin added"; else bad "30b: env.toml wrong" "$(cat "$(ENVT "${H30}")")"; fi
if has "services: mcp pinned at ${TL}+prod" && has "mcp grant <cauldron> --env prod"; then ok "30b: done names the mcp pin and the grant command"; else bad "30b: no mcp done line" "${OUT}"; fi
if [[ -d "${H30}/.cauldron/data/shared/builds/mcp/${TL}+prod" && ! -e "${H30}/.cauldron/data/shared/builds/mcp/${TL}" ]]; then ok "30b: its own version; the shared label never written"; else bad "30b: builds wrong" "$(find "${H30}/.cauldron/data/shared" 2>&1)"; fi
sealed_ok "case 30b"
# 30c: re-run: idempotent.
cp "$(ENVT "${H30}")" "${T}/env30.after"; : >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H30}" srun "ai.broomstick.cauldron:4242" --from "${T}/bin14" --replace-running
if [[ ${RC} -eq 0 && "$(cat "${CLOG}")" == "$(printf '%s\n' '--env prod versions install-workspace-probe' '--env prod install --replace-running' '--env prod mcp grants')" ]] && has "mcp is already pinned in" && has "this checkout's build" && cmp -s "${T}/env30.after" "$(ENVT "${H30}")"; then ok "30c: a re-run builds and pins nothing, env.toml byte-identical"; else bad "30c (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
sealed_ok "case 30c"
# 30d: the upgrade builds, so a dirty checkout is refused at 2c, before
# anything (the opposed arm, both pinned, is 16's last check).
H30d="${T}/home30d"; mkdir -p "${H30d}/.cauldron/data/prod"; cp "${T}/env30.before" "$(ENVT "${H30d}")"
: >"${CLOG}"; : >"${EXEC_LOG}"
STUB_GIT_DIRTY='?? notes.txt' RUN_HOME="${H30d}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 2 ]] && has "uncommitted or untracked files" && [[ ! -s "${CLOG}" || "$(cat "${CLOG}")" == '--env prod versions install-workspace-probe' ]] && cmp -s "${T}/env30.before" "$(ENVT "${H30d}")"; then ok "30d: dirty tree on an upgrade: rc 2 at 2c, cauldron ran at most its read-only workspace probe (comparing the threads pin), env.toml byte-identical"; else bad "30d (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
sealed_ok "case 30d"
# 30e: the mcp build fails on an upgrade: env.toml untouched and nothing
# left behind; the next run (the stop resumed) adds it.
: >"${CLOG}"; : >"${EXEC_LOG}"
STUB_MCP_REFUSE=1 RUN_HOME="${H30d}" srun "" --from "${T}/bin14"
if [[ ${RC} -ne 0 ]] && ! grep -q 'pin mcp' "${CLOG}" && cmp -s "${T}/env30.before" "$(ENVT "${H30d}")" && [[ -z "$(find "${H30d}/.cauldron/data/shared/builds" -path '*/mcp/*' -print -quit 2>/dev/null)" ]]; then ok "30e: a failed mcp build: rc ${RC}, no pin, env.toml byte-identical, no version left"; else bad "30e (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
sealed_ok "case 30e"
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H30d}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 && "$(cat "${CLOG}")" == "${want13}" ]] && [[ "$(grep -c '^\[services.mcp\]$' "$(ENVT "${H30d}")")" == 1 ]]; then ok "30e: the next run adds mcp once"; else bad "30e re-run (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
sealed_ok "case 30e re-run"

printf '\n== 31. the UPGRADE of an existing mcp pin: an older build is re-pinned under --replace-running, once (SEALED)\n'
# An environment from 0.1.6/0.1.7: threads and mcp both pinned, mcp at an
# older build than this checkout's, a holder loaded, upgraded with
# --replace-running as the docs say.
H31="${T}/home31"; mkdir -p "${H31}/.cauldron/data/prod"
printf '[supervisor]\nauto_restart = true\n\n[services.threads]\nversion = "%s+prod"\naddr = "127.0.0.1:3653"\n\n[services.mcp]\nversion = "mcpold-release+prod"\n' "${TL}" >"$(ENVT "${H31}")"
cp "$(ENVT "${H31}")" "${T}/env31.before"
# What env.toml must be after: the same bytes, the mcp version alone changed.
/usr/bin/sed "s/^version = \"mcpold-release+prod\"\$/version = \"${TL}+prod\"/" "${T}/env31.before" >"${T}/env31.want"
# 31a: the plan.
RUN_HOME="${H31}" run "ai.broomstick.cauldron:4242" 0 --dry-run --replace-running
bm="$(line_no '^install: \$ .*/cauldron --env prod build mcp --json --env-copy$')"; pm="$(line_no '^install: \$ .*/cauldron --env prod pin mcp \\<label\\>\+prod$')"; s5="$(line_no '^install: \$ .*/cauldron --env prod install --replace-running$')"
if [[ ${RC} -eq 0 ]] && has "mcp is pinned at mcpold-release+prod, which is NOT this checkout's build" && has "--replace-running: step 4c builds mcp from this checkout and re-pins it (upgrade)" && has "re-pinning it (upgrade," && [[ ${bm} -gt 0 && ${pm} -gt ${bm} && ${s5} -gt ${pm} ]]; then ok "31a plan: says the old mcp pin is re-pinned (upgrade); build mcp < pin mcp < step 5 --replace-running"; else bad "31a plan (rc ${RC}, build ${bm} pin ${pm} step5 ${s5})" "${OUT}"; fi
if [[ $(line_no "${RE_BUILD}") -eq 0 ]] && ! grep -qE -- "${RE_ANYPIN}" <<<"${OUT}" && has "threads is already pinned in" && ! has "STILL THE OLD BUILD"; then ok "31a plan: threads neither built nor re-pinned; no old-build warning"; else bad "31a: threads touched, or a warning" "${OUT}"; fi
if cmp -s "${T}/env31.before" "$(ENVT "${H31}")"; then ok "31a: the dry run wrote nothing"; else bad "31a: the dry run changed env.toml"; fi
# 31b: for real.
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H31}" srun "ai.broomstick.cauldron:4242" --from "${T}/bin14" --replace-running
want31="$(printf '%s\n' '--env prod versions install-workspace-probe' '--env prod build mcp --json --env-copy' "--env prod pin mcp ${TL}+prod" '--env prod install --replace-running' '--env prod mcp grants')"
if [[ ${RC} -eq 0 && "$(cat "${CLOG}")" == "${want31}" ]]; then ok "31b: mcp built --env-copy and re-pinned once by cauldron pin, then install --replace-running"; else bad "31b calls (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
if helpers_at "${H31}" "from the checkout"; then ok "31b: an install from before the helpers, upgraded with --replace-running, gets them"; else bad "31b: helpers not laid down on the upgrade" "$(ls -laR "${H31}/.cauldron/share" 2>&1)"; fi
if cmp -s "${T}/env31.want" "$(ENVT "${H31}")"; then ok "31b: env.toml changed in the mcp version alone (threads pin and every other byte kept)"; else bad "31b: env.toml wrong" "$(diff "${T}/env31.want" "$(ENVT "${H31}")")"; fi
if has "services: mcp re-pinned at ${TL}+prod (it was mcpold-release+prod); the supervisor runs that build, and every grant is kept." && ! has "STILL THE OLD BUILD"; then ok "31b: done names the re-pin, and no old-build warning"; else bad "31b: no re-pin done line" "${OUT}"; fi
if [[ -d "${H31}/.cauldron/data/shared/builds/mcp/${TL}+prod" && ! -e "${H31}/.cauldron/data/shared/builds/mcp/${TL}" ]]; then ok "31b: its own new version; the shared label never written"; else bad "31b: builds wrong" "$(find "${H31}/.cauldron/data/shared" 2>&1)"; fi
# The running service is moved by the supervisor (cauldron pin, then step
# 5's restart of the holder), never stopped or killed by the script.
if ! /usr/bin/grep -qE '^\++XTRACE (kill|pkill|killall|launchctl (bootout|kill|stop|remove))( |$)' "${T}/trace" && ! grep -q '^MUTATING' "${STUB_LOG}"; then ok "31b: the script itself stopped and killed nothing"; else bad "31b: the script signalled a process" "$(/usr/bin/grep -E 'XTRACE (kill|pkill|killall|launchctl)' "${T}/trace")"; fi
sealed_ok "case 31b"
# 31c: re-run: idempotent (the pin is this checkout's build now).
cp "$(ENVT "${H31}")" "${T}/env31.after"; : >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H31}" srun "ai.broomstick.cauldron:4242" --from "${T}/bin14" --replace-running
if [[ ${RC} -eq 0 && "$(cat "${CLOG}")" == "$(printf '%s\n' '--env prod versions install-workspace-probe' '--env prod install --replace-running' '--env prod mcp grants')" ]] && has "mcp is pinned at ${TL}+prod, this checkout's build" && cmp -s "${T}/env31.after" "$(ENVT "${H31}")" && ! has "STILL THE OLD BUILD"; then ok "31c: a re-run builds and pins nothing, env.toml byte-identical, no warning"; else bad "31c (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
sealed_ok "case 31c"
# 31d: without the flag (no job loaded), an upgraded pin says nothing old.
: >"${CLOG}"
RUN_HOME="${H31}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 ]] && ! has "STILL THE OLD BUILD" && ! has "still the old build" && cmp -s "${T}/env31.after" "$(ENVT "${H31}")"; then ok "31d: no flag, current pin: no warning, nothing changed"; else bad "31d (rc ${RC})" "${OUT}"; fi
# 31e: without the flag, an OLD pin is left exactly as it is, and said.
H31e="${T}/home31e"; mkdir -p "${H31e}/.cauldron/data/prod"; cp "${T}/env31.before" "$(ENVT "${H31e}")"
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H31e}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 ]] && cmp -s "${T}/env31.before" "$(ENVT "${H31e}")" && ! grep -qE 'build mcp|pin mcp' "${CLOG}"; then ok "31e: no flag: the old pin is untouched (no build, no pin, env.toml byte-identical)"; else bad "31e (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
old_mcp_said "31e" "${T}/bin14/cauldron"
sealed_ok "case 31e"
# 31f: another writer re-pins mcp while this run builds: that pin stands,
# and the version this run built is removed.
H31f="${T}/home31f"; mkdir -p "${H31f}/.cauldron/data/prod"; cp "${T}/env31.before" "$(ENVT "${H31f}")"
: >"${CLOG}"; : >"${EXEC_LOG}"
STUB_MCP_REPIN_DURING=1 RUN_HOME="${H31f}" srun "" --from "${T}/bin14" --replace-running
if [[ ${RC} -eq 0 ]] && has "mcp was pinned in" && has "while this run built it; that pin is left as it is" && ! grep -q 'pin mcp' "${CLOG}" && grep -qx 'version = "otherwriter-release+prod"' "$(ENVT "${H31f}")" && [[ -z "$(find "${H31f}/.cauldron/data/shared/builds" -path '*/mcp/*' -print -quit 2>/dev/null)" ]]; then ok "31f: a re-pin made during the build stands; no pin by this run; its build removed"; else bad "31f (rc ${RC})" "$(cat "${CLOG}")$(cat "$(ENVT "${H31f}")")${OUT}"; fi
sealed_ok "case 31f"
# 31g: the upgrade builds, so a dirty checkout is refused at 2c, before anything.
H31g="${T}/home31g"; mkdir -p "${H31g}/.cauldron/data/prod"; cp "${T}/env31.before" "$(ENVT "${H31g}")"
: >"${CLOG}"; : >"${EXEC_LOG}"
STUB_GIT_DIRTY='?? notes.txt' RUN_HOME="${H31g}" srun "" --from "${T}/bin14" --replace-running
if [[ ${RC} -eq 2 ]] && has "uncommitted or untracked files" && ! grep -q 'mcp' "${CLOG}" && cmp -s "${T}/env31.before" "$(ENVT "${H31g}")"; then ok "31g: dirty tree on an mcp upgrade: rc 2 at 2c, nothing built, env.toml byte-identical"; else bad "31g (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
sealed_ok "case 31g"

printf '\n== 31t. the UPGRADE of an existing threads pin (#31): an older build is re-pinned under --replace-running, its address kept, once (SEALED)\n'
# An environment from an older install: threads pinned at an older build
# (with its own address), mcp already current. --replace-running moves
# threads to this checkout's build the way it moves mcp; without the flag it
# is left, and said, as for mcp.
H31t="${T}/home31t"; mkdir -p "${H31t}/.cauldron/data/prod"
printf '[supervisor]\nauto_restart = true\n\n[services.threads]\nversion = "threadsold-release"\naddr = "127.0.0.1:3753"\n\n[services.mcp]\nversion = "%s+prod"\n' "${TL}" >"$(ENVT "${H31t}")"
cp "$(ENVT "${H31t}")" "${T}/env31t.before"
/usr/bin/sed "s/^version = \"threadsold-release\"\$/version = \"${TL}+prod\"/" "${T}/env31t.before" >"${T}/env31t.want"
# 31ta: the plan.
RUN_HOME="${H31t}" run "ai.broomstick.cauldron:4242" 0 --dry-run --replace-running
bt="$(line_no '^install: \$ .*/cauldron --env prod build threads --json --env-copy$')"; pt="$(line_no '^install: \$ .*/cauldron --env prod pin threads \\<label\\>\+prod$')"; s5="$(line_no '^install: \$ .*/cauldron --env prod install --replace-running$')"
if [[ ${RC} -eq 0 ]] && has "threads is pinned at threadsold-release, which is NOT this checkout's build" && has "--replace-running: threads is installed from this checkout and re-pinned, replacing that pin whatever" && has "rollback threads\` goes back to it" && has "ports      unchanged: the re-pin keeps the address env.toml already gives threads" && [[ ${bt} -gt 0 && ${pt} -gt ${bt} && ${s5} -gt ${pt} ]] && ! has "pin threads \<label\>+prod --addr"; then ok "31ta plan: the old threads pin is re-pinned (upgrade) without --addr; build threads < pin threads < step 5 --replace-running"; else bad "31ta plan (rc ${RC}, build ${bt} pin ${pt} step5 ${s5})" "${OUT}"; fi
if ! grep -E '^install: \$ ' <<<"${OUT}" | grep -qE 'build mcp|pin mcp'; then ok "31ta plan: mcp (current) neither built nor re-pinned"; else bad "31ta: mcp touched" "${OUT}"; fi
if cmp -s "${T}/env31t.before" "$(ENVT "${H31t}")"; then ok "31ta: the dry run wrote nothing"; else bad "31ta: the dry run changed env.toml"; fi
# 31tb: for real.
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H31t}" srun "ai.broomstick.cauldron:4242" --from "${T}/bin14" --replace-running
want31t="$(printf '%s\n' '--env prod versions install-workspace-probe' '--env prod build threads --json --env-copy' "--env prod pin threads ${TL}+prod" '--env prod install --replace-running' '--env prod mcp grants')"
if [[ ${RC} -eq 0 && "$(cat "${CLOG}")" == "${want31t}" ]]; then ok "31tb: threads built --env-copy and re-pinned once by cauldron pin (no --addr), then install --replace-running"; else bad "31tb calls (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
if cmp -s "${T}/env31t.want" "$(ENVT "${H31t}")"; then ok "31tb: env.toml changed in the threads version alone (its address 127.0.0.1:3753 and every other byte kept)"; else bad "31tb: env.toml wrong" "$(diff "${T}/env31t.want" "$(ENVT "${H31t}")")"; fi
if has "services: threads re-pinned at ${TL}+prod (it was threadsold-release); the supervisor restarts the daemon on that build, at the same address." && has "to go back: ${H31t}/.cauldron/bin/cauldron --env prod rollback threads" && ! has "STILL THE OLD BUILD" && ! has "still the old build"; then ok "31tb: done names the threads re-pin; no old-build warning"; else bad "31tb: no re-pin done line" "${OUT}"; fi
if [[ -f "${H31t}/.cauldron/data/shared/builds/threads/${TL}+prod/threads" && ! -e "${H31t}/.cauldron/data/shared/builds/threads/${TL}" && -z "$(find "${H31t}/.cauldron/data/prod" -name '.threads-image.*' -print -quit)" ]]; then ok "31tb: its own new version, signed in place; the shared label never written; no staging left"; else bad "31tb: builds wrong" "$(find "${H31t}/.cauldron/data" 2>&1)"; fi
if ! /usr/bin/grep -qE '^\++XTRACE (kill|pkill|killall|launchctl (bootout|kill|stop|remove))( |$)' "${T}/trace" && ! grep -q '^MUTATING' "${STUB_LOG}"; then ok "31tb: the script itself stopped and killed nothing"; else bad "31tb: the script signalled a process" "$(/usr/bin/grep -E 'XTRACE (kill|pkill|killall|launchctl)' "${T}/trace")"; fi
sealed_ok "case 31tb"
# 31tc: re-run: idempotent.
cp "$(ENVT "${H31t}")" "${T}/env31t.after"; : >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H31t}" srun "ai.broomstick.cauldron:4242" --from "${T}/bin14" --replace-running
if [[ ${RC} -eq 0 && "$(cat "${CLOG}")" == "$(printf '%s\n' '--env prod versions install-workspace-probe' '--env prod install --replace-running' '--env prod mcp grants')" ]] && has "threads is pinned at ${TL}+prod, this checkout's build" && cmp -s "${T}/env31t.after" "$(ENVT "${H31t}")"; then ok "31tc: a re-run builds and pins nothing, env.toml byte-identical"; else bad "31tc (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
sealed_ok "case 31tc"
# 31td: without the flag, an OLD threads pin is left exactly as it is, and said in 4b and at the end.
H31td="${T}/home31td"; mkdir -p "${H31td}/.cauldron/data/prod"; cp "${T}/env31t.before" "$(ENVT "${H31td}")"
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H31td}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 ]] && cmp -s "${T}/env31t.before" "$(ENVT "${H31td}")" && ! grep -qE 'build threads|pin threads' "${CLOG}"; then ok "31td: no flag: the old threads pin is untouched (no build, no pin, env.toml byte-identical)"; else bad "31td (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
if has "THE threads DAEMON IS STILL THE OLD BUILD: it is pinned at threadsold-release" && has "threads:  the threads daemon is still the old build (threadsold-release); the account helpers" && has "again with --replace-running"; then ok "31td: says in 4b and in the closing lines that threads is still the old build, and how to upgrade it"; else bad "31td: not said" "${OUT}"; fi
sealed_ok "case 31td"
# 31te: another writer re-pins threads while this run builds: that pin stands.
H31te="${T}/home31te"; mkdir -p "${H31te}/.cauldron/data/prod"; cp "${T}/env31t.before" "$(ENVT "${H31te}")"
: >"${CLOG}"; : >"${EXEC_LOG}"
STUB_THREADS_REPIN_DURING=1 RUN_HOME="${H31te}" srun "" --from "${T}/bin14" --replace-running
if [[ ${RC} -eq 0 ]] && has "threads was re-pinned in" && has "while this run built it; that pin is left as it is" && ! grep -q 'pin threads' "${CLOG}" && grep -qx 'version = "otherwriter-threads+prod"' "$(ENVT "${H31te}")"; then ok "31te: a re-pin made during the build stands; no pin by this run"; else bad "31te (rc ${RC})" "$(cat "${CLOG}")$(cat "$(ENVT "${H31te}")")${OUT}"; fi
sealed_ok "case 31te"

printf '\n== 32. an upgrade names a grant that names its cauldron only by name, in its closing lines (SEALED)\n'
# The env of case 31 (upgraded, current pin). The stub's `mcp grants` says on
# stderr, as the real one does, that `kb`'s grant names it only by name.
: >"${CLOG}"; : >"${EXEC_LOG}"
STUB_NAME_ONLY=1 RUN_HOME="${H31}" srun "" --from "${T}/bin14"
more="$(line_no '^install: more: ')"; gl="$(line_no '^install: grants:   an MCP grant names its cauldron only by name')"
if [[ ${RC} -eq 0 ]] && [[ ${gl} -gt ${more} && ${more} -gt 0 ]] && has "the grant of cauldron \`kb\` in environment \`prod\` names it only by name" && has "terminal: cauldron mcp grant kb --env prod --scopes read,dump"; then ok "32: the closing lines name the grant and the command that grants it again"; else bad "32 (rc ${RC}, more ${more}, grants ${gl})" "${OUT}"; fi
if [[ "$(cat "${CLOG}")" == "$(printf '%s\n' '--env prod versions install-workspace-probe' '--env prod install' '--env prod mcp grants')" ]] && cmp -s "${T}/env31.after" "$(ENVT "${H31}")"; then ok "32: only a read-only \`mcp grants\` was added, and env.toml is byte-identical"; else bad "32 calls" "$(cat "${CLOG}")"; fi
sealed_ok "case 32"
# Opposed: with no such grant, nothing is said (case 31d's run said nothing
# either), and a FIRST install never asks (case 14's call list has no
# `mcp grants`).
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H31}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 ]] && ! has "only by name" && ! has "install: grants:"; then ok "32: no name-only grant, no grants line"; else bad "32: a grants line with nothing to name" "${OUT}"; fi

# ── THE PREBUILT PATH (--prebuilt), cases 33-39 ─────────────────────────────
# A FAKE release bundle, made here: bin/cauldron is a stub (FAKEMACHO:arm64,
# so the od shim reads an arm64 header) that installs `--artifact` images the
# way the real `cauldron build <service> --artifact` does (a NEW
# `<commit12>-release+<env>` version, the commit read off the image) and
# hands every other verb to the stub cauldron above. Every binary is signed
# by the fake codesign, whose --verify fails once a byte changes. The
# commits are spelt in pieces (a long hex run reads as a hash).
PBC1="$(printf 'a1b2%.0s' 1 2 3 4 5 6 7 8 9 10)"
PBC2="$(printf 'c3d4%.0s' 1 2 3 4 5 6 7 8 9 10)"
PBV=0.1.99
PBN="cauldron-${PBV}-aarch64-apple-darwin"
mkdir -p "${T}/pb"
cat >"${T}/pb/cauldron.head" <<STUBPB
#!/bin/bash
# FAKEMACHO:arm64
case "\$*" in
    *" build threads --json --env-copy --artifact "*|*" build mcp --json --env-copy --artifact "*)
        printf '%s\n' "\$*" >>"\${CLOG:?}"
        root="\${HOME}/.cauldron"; e="\$2"; svc="\$4"; img="\${@: -1}"
        [[ "\${svc}" == threads ]] && bin=threads || bin=cauldron-mcp
        c="\$(/usr/bin/sed -n 's/^STAMP://p' "\${img}")"; [[ -n "\${c}" ]] || { echo "stub: image states no commit" >&2; exit 1; }
        b="\${root}/data/shared/builds/\${svc}"; base="\${c:0:12}-release"; /bin/mkdir -p "\${b}"
        k=1; l="\${base}+\${e}"
        until /bin/mkdir "\${b}/\${l}" 2>/dev/null; do k=\$((k+1)); l="\${base}+\${e}.\${k}"; [[ \${k} -lt 100 ]] || exit 70; done
        /bin/cp "\${img}" "\${b}/\${l}/\${bin}"
        printf '{\n  "service": "%s",\n  "label": "%s",\n  "bin": "%s"\n}\n' "\${svc}" "\${l}" "\${bin}" >"\${b}/\${l}/build.json"
        printf 'name = "%s"\n' "\${svc}" >"\${b}/\${l}/declaration.toml"
        printf '[\n  {\n    "service": "%s",\n    "label": "%s",\n    "bin": "%s"\n  }\n]\n' "\${svc}" "\${l}" "\${bin}"
        exit 0 ;;
esac
exec "${T}/bin14/cauldron" "\$@"
STUBPB
# make_bundle <out-dir> <commit> [<mutation>]: the tarball <out-dir>/${PBN}.tar.gz
# and <out-dir>/SHA256SUMS. Mutations: tamper-resummed (a byte of
# libexec/threads changed after signing, every sum made again), tamper (the
# same, sums left), escape (an entry <top>/../evil), link (a symlink entry).
make_bundle() {
    local out="$1" c="$2" mut="${3:-}" st b f
    st="$(mktemp -d "${T}/pbstage.XXXXXX")"; b="${st}/${PBN}"
    mkdir -p "${out}" "${b}/bin" "${b}/libexec" "${b}/share/cauldron/services" "${b}/share/cauldron/mcp"
    cp "${T}/pb/cauldron.head" "${b}/bin/cauldron"
    printf '#!/bin/bash\n# FAKEMACHO:arm64\nexit 0\n' >"${b}/bin/tui"
    printf '#!/bin/bash\n# FAKEMACHO:arm64\nexit 0\nSTAMP:%s\n' "${c}" >"${b}/libexec/threads"
    printf '#!/bin/bash\n# FAKEMACHO:arm64\nexit 0\nSTAMP:%s\n' "${c}" >"${b}/libexec/cauldron-mcp"
    chmod +x "${b}/bin/cauldron" "${b}/bin/tui" "${b}/libexec/threads" "${b}/libexec/cauldron-mcp"
    for f in bin/cauldron:cauldron bin/tui:tui libexec/threads:com.harness.threads libexec/cauldron-mcp:cauldron-mcp; do
        EXEC_LOG=/dev/null "${SEAL}/codesign" --force --sign - --identifier "${f#*:}" "${b}/${f%%:*}"
    done
    # The local-inference services and their declarations (mutation
    # no-inference leaves the inference image out).
    for f in cauldron-inference cauldron-ripples; do
        [[ "${mut}" == no-inference && "${f}" == cauldron-inference ]] && continue
        printf '#!/bin/bash\n# FAKEMACHO:arm64\nexit 0\nSTAMP:%s\n' "${c}" >"${b}/libexec/${f}"; chmod +x "${b}/libexec/${f}"
        EXEC_LOG=/dev/null "${SEAL}/codesign" --force --sign - --identifier "${f}" "${b}/libexec/${f}"
    done
    printf 'name = "inference"\n' >"${b}/share/cauldron/services/inference.toml"
    printf 'name = "ripples"\n' >"${b}/share/cauldron/services/ripples.toml"
    printf 'name = "threads"\n' >"${b}/share/cauldron/services/threads.toml"
    printf 'name = "mcp"\n# bundle %s\n' "${c:0:4}" >"${b}/share/cauldron/services/mcp.toml"
    printf '# no tools\n' >"${b}/share/cauldron/mcp/agents.toml"
    if [[ "${mut}" != no-helpers ]]; then
        mkdir -p "${b}/share/cauldron/scripts/threads"
        for f in add-account claude-accounts claude-login-browser; do
            printf '#!/usr/bin/env bash\n# helper %s of bundle %s\nexit 0\n' "${f}" "${c:0:4}" >"${b}/share/cauldron/scripts/threads/${f}"
            chmod 0755 "${b}/share/cauldron/scripts/threads/${f}"
        done
    fi
    printf '#!/bin/bash\nexit 0\n' >"${b}/install"; printf 'license\n' >"${b}/LICENSE"
    printf 'name=cauldron\nversion=%s\ncommit=%s\ntarget=aarch64-apple-darwin\nsigned=adhoc\n' "${PBV}" "${c}" >"${b}/BUNDLE"
    case "${mut}" in tamper|tamper-resummed) /usr/bin/sed -i '' 's/exit 0/exit 7/' "${b}/libexec/threads" ;; esac
    pb_sums() { (cd "${b}" && find . -type f ! -name SHA256SUMS | sed 's|^\./||' | LC_ALL=C sort | while IFS= read -r f; do shasum -a 256 "${f}"; done) >"${st}/sums" && mv "${st}/sums" "${b}/SHA256SUMS"; }
    if [[ "${mut}" == tamper ]]; then
        # Sums of the bundle BEFORE the change (recomputed on a pristine copy).
        /usr/bin/sed -i '' 's/exit 7/exit 0/' "${b}/libexec/threads"; pb_sums; /usr/bin/sed -i '' 's/exit 0/exit 7/' "${b}/libexec/threads"
    else
        pb_sums
    fi
    case "${mut}" in
        escape) printf 'evil\n' >"${st}/evil"
                (cd "${st}" && COPYFILE_DISABLE=1 /usr/bin/tar -czf "${out}/${PBN}.tar.gz" -s "|^evil\$|${PBN}/../evil|" "${PBN}" evil) ;;
        link)   ln -s /etc/hosts "${b}/share/cauldron/hosts"
                (cd "${st}" && COPYFILE_DISABLE=1 /usr/bin/tar -czf "${out}/${PBN}.tar.gz" "${PBN}") ;;
        *)      (cd "${st}" && COPYFILE_DISABLE=1 /usr/bin/tar -czf "${out}/${PBN}.tar.gz" "${PBN}") ;;
    esac
    (cd "${out}" && shasum -a 256 "${PBN}.tar.gz") >"${out}/SHA256SUMS"
    rm -rf "${st}"
}
PBSHA() { shasum -a 256 "$1/${PBN}.tar.gz" | awk '{print $1}'; }
REL() { echo "$1/.cauldron/share/cauldron/releases/${PBV}-$2"; }
make_bundle "${T}/pb/one" "${PBC1}"
make_bundle "${T}/pb/two" "${PBC2}"
L1="${PBC1:0:12}-release"; L2="${PBC2:0:12}-release"
no_stage() { [[ -z "$(find "$1" -maxdepth 1 -name '.cauldron.prebuilt.*' -print -quit 2>/dev/null)" ]]; }

printf '\n== 33. --prebuilt --dry-run: verifies the tarball, plans --artifact installs, writes nothing\n'
mkdir -p "${T}/home33" "${T}/pbstub"
# The unsealed dry run reads the fake binaries through the fake codesign and
# the od shim, too.
/bin/cp "${SEAL}/codesign" "${SEAL}/od" "${T}/pbstub/"
PATH="${T}/pbstub:${PATH}" EXEC_LOG=/dev/null RUN_HOME="${T}/home33" run "" 0 --dry-run --prebuilt "${T}/pb/one/${PBN}.tar.gz"
if [[ ${RC} -eq 0 ]] && has "checksum   OK: sha256 $(PBSHA "${T}/pb/one")" && has "signatures bin/cauldron, bin/tui, libexec/threads, libexec/cauldron-mcp: arm64, ad-hoc signed, verified"; then ok "33: rc 0; checksum from the SHA256SUMS beside it, signatures verified"; else bad "33: rc ${RC}" "${OUT}"; fi
if grep -qE '^install: \$ .*/bin/cauldron --env prod build threads --json --env-copy --artifact .*/libexec/threads$' <<<"${OUT}" \
   && grep -qE '^install: \$ .*/bin/cauldron --env prod build mcp --json --env-copy --artifact .*/libexec/cauldron-mcp$' <<<"${OUT}"; then ok "33: the plan installs threads and mcp from the bundle with --artifact"; else bad "33: no --artifact lines" "${OUT}"; fi
if has "signatures libexec/cauldron-inference, libexec/cauldron-ripples: arm64, ad-hoc signed, verified (local inference: laid down, not pinned, not started)" \
   && ! grep -qE ' (build|pin|start) (inference|ripples)' <<<"$(grep '^install: \$' <<<"${OUT}")" \
   && has "local:    local inference (the inference and ripples services) is installed OFF: nothing pinned, nothing started." \
   && has "To use it: ${T}/home33/.cauldron/bin/cauldron --env prod build inference   (and ripples; from the bundle's image (no cargo))"; then ok "33: inference and ripples verified, never built, pinned or started; the plan says how to turn them on"; else bad "33: local inference lines wrong" "${OUT}"; fi
if ! grep -qE 'cargo build|build-threads\.sh' <<<"${OUT}" && has "would place the bundle at ${T}/home33/.cauldron/share/cauldron/releases/${PBV}-${PBC1:0:12}"; then ok "33: no cargo build and no re-sign in the plan; names where the bundle goes"; else bad "33: plan wrong" "${OUT}"; fi
if [[ -z "$(find "${T}/home33" -mindepth 1 -print -quit)" ]]; then ok "33: dry run wrote nothing under HOME"; else bad "33: dry run wrote" "$(find "${T}/home33")"; fi

printf '\n== 34. --prebuilt on a FRESH machine (SEALED): no cargo, no git, the same layout and later steps\n'
mkdir -p "${T}/home34"; : >"${CLOG}"; : >"${EXEC_LOG}"; : >"${SIGN_LOG}"
RUN_HOME="${T}/home34" srun "" --prebuilt "${T}/pb/one/${PBN}.tar.gz"
R1="$(REL "${T}/home34" "${PBC1:0:12}")"
want34="$(printf '%s\n' '--env prod env create prod --auto-restart' "--env prod build threads --json --env-copy --artifact ${R1}/libexec/threads" "--env prod pin threads ${L1}+prod --addr auto" "--env prod build mcp --json --env-copy --artifact ${R1}/libexec/cauldron-mcp" "--env prod pin mcp ${L1}+prod" '--env prod install')"
if [[ ${RC} -eq 0 && "$(cat "${CLOG}")" == "${want34}" ]]; then ok "34: rc 0; env create, threads and mcp from the bundle (--artifact), pinned, then install"; else bad "34: rc ${RC}, cauldron calls wrong" "$(cat "${CLOG}")${OUT}"; fi
if [[ -f "${R1}/libexec/cauldron-inference" && -f "${R1}/libexec/cauldron-ripples" && -f "${T}/home34/.cauldron/share/cauldron/services/inference.toml" && -f "${T}/home34/.cauldron/share/cauldron/services/ripples.toml" ]] \
   && ! grep -qE 'services\.(inference|ripples)' "$(ENVT "${T}/home34")" && [[ ! -e "${T}/home34/.cauldron/data/shared/builds/inference" && ! -e "${T}/home34/.cauldron/data/shared/builds/ripples" ]] \
   && has "local:    local inference (the inference and ripples services) is installed OFF"; then ok "34: inference and ripples laid down with the bundle and their declarations; not built, not pinned"; else bad "34: local inference layout or pins wrong" "$(find "${T}/home34/.cauldron" 2>&1)$(cat "$(ENVT "${T}/home34")")"; fi
if [[ -f "${R1}/bin/cauldron" && -f "${R1}/libexec/threads" && -f "${T}/home34/.cauldron/share/cauldron/services/threads.toml" && -f "${T}/home34/.cauldron/share/cauldron/mcp/agents.toml" ]]; then ok "34: the bundle at share/cauldron/releases/<version>-<commit>, its declarations at share/cauldron/{services,mcp}"; else bad "34: layout wrong" "$(find "${T}/home34/.cauldron/share" 2>&1)"; fi
T34="${T}/home34/.cauldron/data/shared/builds/threads/${L1}+prod/threads"
if EXEC_LOG=/dev/null "${SEAL}/codesign" --verify --strict "${T34}" 2>/dev/null && grep -qx '\[services.threads\]' "$(ENVT "${T}/home34")" && grep -qx '\[services.mcp\]' "$(ENVT "${T}/home34")"; then ok "34: the threads version is the bundle's signed image; threads and mcp pinned"; else bad "34: version or pins wrong" "$(cat "$(ENVT "${T}/home34")" 2>&1)"; fi
if ! grep -qE '^(git|CARGO) ' "${EXEC_LOG}" && ! grep -q 'signer' "${SIGN_LOG}"; then ok "34: neither git nor cargo nor the threads signer ran"; else bad "34: git, cargo or the signer ran" "$(grep -E '^(git|CARGO) ' "${EXEC_LOG}")"; fi
if no_stage "${T}/home34" && [[ ! -e "$(MARK "${T}/home34")" ]] && has "services: threads pinned at ${L1}+prod" && has "services: mcp pinned at ${L1}+prod"; then ok "34: no scratch dir or marker left; the done lines name both pins"; else bad "34: leftovers or done lines" "$(ls -a "${T}/home34")${OUT}"; fi
if helpers_at "${T}/home34" "of bundle ${PBC1:0:4}"; then ok "34: the bundle's account helpers installed at share/cauldron/scripts/threads/, 755, directories 755"; else bad "34: helpers not installed" "$(ls -laR "${T}/home34/.cauldron/share/cauldron/scripts" 2>&1)"; fi
M34="${T}/home34/Library/Application Support/cauldron/cauldron-bin"
if [[ "$(cat "${M34}" 2>/dev/null)" == "${T}/home34/.cauldron/bin/cauldron" ]]; then ok "34: the install marker the plugin's launcher reads names <root>/bin/cauldron"; else bad "34: no install marker" "$(cat "${M34}" 2>&1)"; fi
sealed_ok "case 34"

printf '\n== 35. --prebuilt UPGRADE over a running job: refused without --replace-running; with it threads and mcp move, once (SEALED)\n'
cp "$(ENVT "${T}/home34")" "${T}/env35.before"; : >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${T}/home34" srun "ai.broomstick.cauldron:4242" --prebuilt "${T}/pb/two/${PBN}.tar.gz"
if [[ ${RC} -eq 2 ]] && has "REFUSED: a cauldron job for environment 'prod' is already loaded" && [[ ! -s "${CLOG}" ]] && cmp -s "${T}/env35.before" "$(ENVT "${T}/home34")" && no_stage "${T}/home34"; then ok "35a: refused at 2b; no cauldron call, env.toml byte-identical, nothing staged"; else bad "35a: rc ${RC}" "$(cat "${CLOG}")${OUT}"; fi
# F7: a staging copy a killed earlier run left behind.
mkdir -p "${T}/home34/.cauldron/share/cauldron/scripts/.threads.new.99999"
RUN_HOME="${T}/home34" srun "ai.broomstick.cauldron:4242" --prebuilt "${T}/pb/two/${PBN}.tar.gz" --replace-running
R2="$(REL "${T}/home34" "${PBC2:0:12}")"
if [[ -z "$(find "${T}/home34/.cauldron/share/cauldron/scripts" -maxdepth 1 -name '.threads.new.*' -print -quit)" ]]; then ok "35b (F7): a staging copy a killed run left is removed; none of this run's is left"; else bad "35b: staging left" "$(ls -la "${T}/home34/.cauldron/share/cauldron/scripts")"; fi
want35="$(printf '%s\n' "--env prod build threads --json --env-copy --artifact ${R2}/libexec/threads" "--env prod pin threads ${L2}+prod" "--env prod build mcp --json --env-copy --artifact ${R2}/libexec/cauldron-mcp" "--env prod pin mcp ${L2}+prod" '--env prod install --replace-running' '--env prod mcp grants')"
if [[ ${RC} -eq 0 && "$(cat "${CLOG}")" == "${want35}" ]] && has "mcp is pinned at ${L1}+prod, which is NOT this bundle's build" && has "threads is pinned at ${L1}+prod, which is NOT this bundle's build"; then ok "35b: threads and mcp installed from the new bundle and re-pinned (threads without --addr); install --replace-running"; else bad "35b: rc ${RC}" "$(cat "${CLOG}")${OUT}"; fi
if [[ "$(grep -c "^version = \"${L2}+prod\"\$" "$(ENVT "${T}/home34")")" == 2 ]] && ! grep -q "version = \"${L1}+prod\"" "$(ENVT "${T}/home34")" && /usr/bin/diff <(grep -v '^version = ' "${T}/env35.before") <(grep -v '^version = ' "$(ENVT "${T}/home34")") >/dev/null && [[ -d "${R1}" && -d "${R2}" ]] && grep -q "bundle ${PBC2:0:4}" "${T}/home34/.cauldron/share/cauldron/services/mcp.toml" && grep -q "bundle ${PBC1:0:4}" "${T}/home34/.cauldron/share/cauldron/services.prev/mcp.toml"; then ok "35b: threads and mcp at the new build, every other line of env.toml (the threads address too) kept; both releases kept; declarations replaced, the old ones kept as .prev"; else bad "35b: state wrong" "$(cat "$(ENVT "${T}/home34")")"; fi
if helpers_at "${T}/home34" "of bundle ${PBC2:0:4}" && /usr/bin/grep -q "of bundle ${PBC1:0:4}" "${T}/home34/.cauldron/share/cauldron/scripts/threads.prev/add-account"; then ok "35b: the upgrade replaced the helpers with the new bundle's, the old ones kept as threads.prev"; else bad "35b: helpers not upgraded" "$(ls -laR "${T}/home34/.cauldron/share/cauldron/scripts" 2>&1)"; fi
if has "services: threads re-pinned at ${L2}+prod (it was ${L1}+prod)" && has "services: mcp re-pinned at ${L2}+prod" && ! has "STILL THE OLD BUILD" && ! has "still the old build"; then ok "35b: done names both re-pins; no old-build warning"; else bad "35b: done lines" "${OUT}"; fi
sealed_ok "case 35b"
cp "$(ENVT "${T}/home34")" "${T}/env35.after"; : >"${CLOG}"
RUN_HOME="${T}/home34" srun "ai.broomstick.cauldron:4242" --prebuilt "${T}/pb/two/${PBN}.tar.gz" --replace-running
if [[ ${RC} -eq 0 && "$(cat "${CLOG}")" == "$(printf '%s\n' '--env prod install --replace-running' '--env prod mcp grants')" ]] && cmp -s "${T}/env35.after" "$(ENVT "${T}/home34")" && has "the bundle is already in place at ${R2} (same contents); kept"; then ok "35c: a re-run changes nothing (no build, no pin; the bundle kept)"; else bad "35c: rc ${RC}" "$(cat "${CLOG}")${OUT}"; fi
sealed_ok "case 35c"
# 35d (#31): without --replace-running, a threads pin at another bundle's
# build is left as it is, and the run says so with the two commands.
H35d="${T}/home35d"; mkdir -p "${H35d}/.cauldron/data/prod"; cp "${T}/env35.before" "$(ENVT "${H35d}")"
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H35d}" srun "" --prebuilt "${T}/pb/two/${PBN}.tar.gz"
R2d="$(REL "${H35d}" "${PBC2:0:12}")"
if [[ ${RC} -eq 0 ]] && ! grep -qE 'build threads|pin threads' "${CLOG}" && has "THE threads DAEMON IS STILL THE OLD BUILD: it is pinned at ${L1}+prod" && has "build threads --env-copy --artifact ${R2d}/libexec/threads" && has "pin threads <the label that build printed>" && has "threads:  the threads daemon is still the old build (${L1}+prod)"; then ok "35d: no flag: threads left at the old bundle's build, and said, with the two commands"; else bad "35d: rc ${RC}" "$(cat "${CLOG}")${OUT}"; fi
sealed_ok "case 35d"

pb_refused() {  # <tag> <home> <expected text>
    if [[ ${RC} -eq 2 ]] && has "$3" && has "Nothing was installed, written or loaded." && [[ ! -s "${CLOG}" ]] && [[ ! -e "$2/.cauldron" ]] && no_stage "$2"; then
        ok "$1: refused (rc 2) before any cauldron call; nothing under HOME, no scratch dir left"
    else bad "$1: rc ${RC}" "$(cat "${CLOG}")${OUT}$(find "$2" 2>&1)"; fi
}
printf '\n== 36. a bad checksum is refused before anything is unpacked (SEALED)\n'
mkdir -p "${T}/home36"; : >"${CLOG}"; : >"${EXEC_LOG}"
WRONG="$(PBSHA "${T}/pb/two")"
RUN_HOME="${T}/home36" srun "" --prebuilt "${T}/pb/one/${PBN}.tar.gz" --sha256 "${WRONG}"
pb_refused "36a (--sha256 of another file)" "${T}/home36" "it is not the file the checksum is for"
sealed_ok "case 36a"
make_bundle "${T}/pb/badsum" "${PBC1}"; (cd "${T}/pb/two" && shasum -a 256 "${PBN}.tar.gz") >"${T}/pb/badsum/SHA256SUMS"
RUN_HOME="${T}/home36" srun "" --prebuilt "${T}/pb/badsum/${PBN}.tar.gz"
pb_refused "36b (SHA256SUMS beside it names other bytes)" "${T}/home36" "it is not the file the checksum is for"
mkdir -p "${T}/pb/nosum"; cp "${T}/pb/one/${PBN}.tar.gz" "${T}/pb/nosum/"
RUN_HOME="${T}/home36" srun "" --prebuilt "${T}/pb/nosum/${PBN}.tar.gz"
pb_refused "36c (no checksum at all)" "${T}/home36" "no checksum for"

printf '\n== 37. a tampered binary is refused (SEALED)\n'
make_bundle "${T}/pb/tamper2" "${PBC1}" tamper-resummed
mkdir -p "${T}/home37"; : >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${T}/home37" srun "" --prebuilt "${T}/pb/tamper2/${PBN}.tar.gz"
pb_refused "37a (changed after signing, every sum made again)" "${T}/home37" "libexec/threads in the bundle does not carry a valid ad-hoc signature as com.harness.threads"
sealed_ok "case 37a"
make_bundle "${T}/pb/noinf" "${PBC1}" no-inference
RUN_HOME="${T}/home37" srun "" --prebuilt "${T}/pb/noinf/${PBN}.tar.gz"
pb_refused "37c (a bundle without the local-inference service)" "${T}/home37" "the bundle lacks: libexec/cauldron-inference."
make_bundle "${T}/pb/tamper1" "${PBC1}" tamper
RUN_HOME="${T}/home37" srun "" --prebuilt "${T}/pb/tamper1/${PBN}.tar.gz"
pb_refused "37b (changed after the bundle was built)" "${T}/home37" "does not match the bundle's own SHA256SUMS"

printf '\n== 38. a tarball with an entry escaping its directory (../), or a link, is refused before unpacking (SEALED)\n'
make_bundle "${T}/pb/escape" "${PBC1}" escape
if /usr/bin/tar -tzf "${T}/pb/escape/${PBN}.tar.gz" | grep -qx "${PBN}/../evil"; then ok "38: fixture has the entry ${PBN}/../evil"; else bad "38: fixture lacks the escaping entry" "$(/usr/bin/tar -tzf "${T}/pb/escape/${PBN}.tar.gz")"; fi
mkdir -p "${T}/home38"; : >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${T}/home38" srun "" --prebuilt "${T}/pb/escape/${PBN}.tar.gz"
pb_refused "38a (../)" "${T}/home38" "has entries outside ${PBN}/"
if [[ -z "$(find "${T}" -name evil -print -quit)" ]]; then ok "38a: no file named evil was written anywhere in the scratch dir"; else bad "38a: evil written" "$(find "${T}" -name evil)"; fi
sealed_ok "case 38a"
make_bundle "${T}/pb/link" "${PBC1}" link
RUN_HOME="${T}/home38" srun "" --prebuilt "${T}/pb/link/${PBN}.tar.gz"
pb_refused "38b (a symlink entry)" "${T}/home38" "entries that are not plain files or directories"
if has "${PBN}/share/cauldron/hosts" && ! has "/etc/hosts"; then ok "38b: the refusal names the link entry itself, not its target"; else bad "38b: the refusal does not name the entry" "${OUT}"; fi

printf '\n== 39. a machine that is not arm64 is refused with a clear message (SEALED), and a URL must be https with --sha256\n'
mkdir -p "${T}/home39"; : >"${CLOG}"; : >"${EXEC_LOG}"
STUB_ARCH=x86_64 RUN_HOME="${T}/home39" srun "" --prebuilt "${T}/pb/one/${PBN}.tar.gz"
pb_refused "39a (x86_64)" "${T}/home39" "the prebuilt bundle holds arm64 (Apple silicon) binaries only, and this machine (or this shell) is x86_64. On an Intel Mac, install from source"
sealed_ok "case 39a"
RUN_HOME="${T}/home39" srun "" --prebuilt "http://example.invalid/${PBN}.tar.gz" --sha256 "$(PBSHA "${T}/pb/one")"
pb_refused "39b (plain http)" "${T}/home39" "is plain http"
RUN_HOME="${T}/home39" srun "" --prebuilt "https://example.invalid/${PBN}.tar.gz"
pb_refused "39c (https without --sha256)" "${T}/home39" "a --prebuilt URL needs --sha256"
STUB_CURL_FILE="${T}/pb/one/${PBN}.tar.gz" RUN_HOME="${T}/home39" srun "" --prebuilt "https://example.invalid/${PBN}.tar.gz" --sha256 "$(PBSHA "${T}/pb/one")"
if [[ ${RC} -eq 0 ]] && grep -q "^curl --proto =https --proto-redir =https --tlsv1.2 --max-filesize 268435456 -fsSL -o ${T}/home39/.cauldron.prebuilt.[0-9]*/download.tar.gz https://example.invalid/${PBN}.tar.gz$" "${EXEC_LOG}" && grep -qx "\[services.mcp\]" "$(ENVT "${T}/home39")" && no_stage "${T}/home39"; then ok "39d: https + --sha256: downloaded (https only) into the scratch dir, verified, installed; scratch removed"; else bad "39d: rc ${RC}" "$(cat "${EXEC_LOG}")${OUT}"; fi
sealed_ok "case 39d"
make_bundle "${T}/pb/quar" "${PBC1}"; /usr/bin/xattr -w com.apple.quarantine "0081;00000000;Example;" "${T}/pb/quar/${PBN}.tar.gz"
: >"${CLOG}"; mkdir -p "${T}/home39q"
RUN_HOME="${T}/home39q" srun "" --prebuilt "${T}/pb/quar/${PBN}.tar.gz"
pb_refused "39f (a quarantined download)" "${T}/home39q" "carries macOS's quarantine attribute"
if has "xattr -d com.apple.quarantine ${T}/pb/quar/${PBN}.tar.gz"; then ok "39f: names the command that removes the attribute"; else bad "39f: no xattr command" "${OUT}"; fi
sealed_ok "case 39f"
RUN_HOME="${T}/home39" run "" 0 --dry-run --prebuilt "${T}/pb/one/${PBN}.tar.gz" --from "${T}/bin14"
if [[ ${RC} -eq 2 ]] && has "--prebuilt and --from are two different sources"; then ok "39e: --prebuilt with --from is refused"; else bad "39e: rc ${RC}" "${OUT}"; fi

printf '\n== 40. a bundle without the account helpers is refused before anything is installed (SEALED)\n'
make_bundle "${T}/pb/nohelpers" "${PBC1}" no-helpers
mkdir -p "${T}/home40"; : >"${CLOG}"
RUN_HOME="${T}/home40" srun "" --prebuilt "${T}/pb/nohelpers/${PBN}.tar.gz"
pb_refused "40" "${T}/home40" "the bundle lacks: share/cauldron/scripts/threads/add-account share/cauldron/scripts/threads/claude-accounts share/cauldron/scripts/threads/claude-login-browser."

printf '\n== 27. an unset variable aborts install with rc != 0 (bash 3.2 + EXIT trap, REL-4.S5e)\n'
# A copy with an unset variable read right before 4b's staging mkdir, where
# the EXIT trap has long been set (at 55f74d6b it was set two lines earlier).
M27="${T}/repo/scripts/install27"
/usr/bin/awk '$0 == "    run mkdir \"${STAGE}\"" { print "    : \"${conf_unbound_27}\"" } { print }' "${INSTALL}" >"${M27}"
if [[ "$(/usr/bin/grep -cxF '    : "${conf_unbound_27}"' "${M27}")" == 1 ]]; then ok "27: the copy has the injected unset read"; else bad "27: injection failed"; fi
# The rc-0 abort is a property of bash 3.2 (macOS's /bin/bash), so that arm
# runs under /bin/bash by name: the bash first on PATH may be a newer one
# (Homebrew's 5.x), whose `set -u` abort already exits 1 and reaches the trap
# as 1, which the trap passes through (no ABORTED line, nothing to convert).
# The second arm asserts only what holds under any bash: rc != 0.
mkdir -p "${T}/home27"; RUN_BASH=/bin/bash RUN_HOME="${T}/home27" RUN_AS="${M27}" run "" 0 --dry-run
if [[ ${RC} -ne 0 ]] && grep -qF 'conf_unbound_27: unbound variable' <<<"${ERR}" && ! has "== 5. supervisor"; then ok "27: the abort exits rc ${RC}, not 0, and stops before step 5"; else bad "27: a set -u abort read as rc ${RC}" "${OUT}"; fi
if [[ ${RC} -eq 70 ]] && has "install: ABORTED"; then ok "27: under /bin/bash (3.2): rc 70 and says ABORTED"; else bad "27: rc ${RC}, no ABORTED line" "${ERR}"; fi
RUN_HOME="${T}/home27" RUN_AS="${M27}" run "" 0 --dry-run
if [[ ${RC} -ne 0 ]] && grep -qF 'conf_unbound_27: unbound variable' <<<"${ERR}" && ! has "== 5. supervisor"; then ok "27: under the bash on PATH ($(bash -c 'echo ${BASH_VERSION}')): the abort exits rc ${RC}, not 0"; else bad "27: under the bash on PATH, a set -u abort read as rc ${RC}" "${OUT}"; fi

printf '\n== 28. an unset variable aborts THIS suite with rc != 0 (REL-4.S5e)\n'
M28="${T}/suite28.sh"
/usr/bin/awk '{ print } $0 == "mkdir -p \"${T}/stub\" \"${T}/gitstub\" \"${T}/home\" \"${T}/empty\"" { print ": \"${conf_unbound_28}\"" }' "${SUITE}" >"${M28}"
# Counted as the WHOLE injected line: the string also appears in this case's
# own source, which the copy carries.
if [[ "$(/usr/bin/grep -cxF ': "${conf_unbound_28}"' "${M28}")" == 1 ]]; then ok "28: the suite copy has the injected unset read"; else bad "28: injection failed"; fi
set +e; out28="$(INSTALL="${INSTALL}" /bin/bash "${M28}" 2>&1)"; rc28=$?; set -e
if [[ ${rc28} -ne 0 ]] && grep -qF 'conf_unbound_28: unbound variable' <<<"${out28}" && ! grep -q '^passed ' <<<"${out28}"; then ok "28: the aborted suite exits rc ${rc28}, not 0"; else bad "28: an aborted suite read as rc ${rc28}" "${out28}"; fi

printf '\n== 29. nothing planted in the directory install is run from is run\n'
# `.` first on PATH, run from a directory holding programs named like every
# tool the installer runs by name: PATH keeps only its absolute entries, so
# none of them runs and the stubs answer as in every other case.
P29="${T}/project29"; mkdir -p "${P29}" "${T}/home29"; : >"${T}/planted29.log"
for c in launchctl git shasum tar codesign cargo cauldron mkdir cp mv chmod find sed grep awk stat id uname basename dirname; do
    printf '#!/bin/sh\necho "planted %s $*" >> "%s"\nexit 0\n' "${c}" "${T}/planted29.log" >"${P29}/${c}"; /bin/chmod 0755 "${P29}/${c}"
done
RUN_PATH_HEAD=".:" RUN_CWD="${P29}" RUN_HOME="${T}/home29" run "" 0 --dry-run
if [[ ${RC} -eq 0 ]] && has "no cauldron job for 'prod' is loaded"; then ok "29: the dry run still answers from the stubs"; else bad "29: rc ${RC}" "${OUT}"; fi
if [[ ! -s "${T}/planted29.log" ]]; then ok "29: no program planted in its directory ran"; else bad "29: a planted program ran" "$(cat "${T}/planted29.log")"; fi

printf '\n--- refusal message (case 2) ---\n%s\n' "${SAVED_REFUSAL}"
printf '\npassed %d, failed %d\n' "${PASS}" "${FAIL}"
if [[ ${FAIL} -eq 0 ]]; then SUITE_DONE=1; exit 0; fi
exit 1
