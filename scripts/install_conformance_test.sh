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
        HOME="${RUN_HOME:-${T}/home}" PATH="${T}/stub:${RUN_REAL_GIT:+${T}/empty}${RUN_REAL_GIT:-${T}/gitstub}:${PATH}" \
        STUB_LOG="${STUB_LOG}" STUB_LOADED="${loaded}" STUB_LIST_HIDE="${hide}" \
        STUB_LIST_FAIL="${STUB_LIST_FAIL:-0}" \
        STUB_GIT_DIRTY="${STUB_GIT_DIRTY:-}" STUB_GIT_FAIL="${STUB_GIT_FAIL:-0}" \
        STUB_GIT_DIRTY_AT="${STUB_GIT_DIRTY_AT:-}" STUB_WS="${STUB_WS:-${T}/repo}" STUB_PROBE="${STUB_PROBE:-}" \
        CLOG="${CLOG:-${T}/cauldron.log}" GIT_DIRS_LOG="${T}/gitdirs.log" \
        bash "${RUN_AS:-${INSTALL}}" "$@" 2>"${T}/stderr")"
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
real_of() { local d; for d in /usr/bin /bin /usr/sbin /sbin; do if [[ -x "${d}/$1" ]]; then echo "${d}/$1"; return 0; fi; done; echo "no real $1" >&2; return 1; }
# Logging shims for the plain utilities. Mutating ones refuse (98) any
# absolute path argument outside ${T} (cp: its destination only, since it may
# read a source elsewhere).
for c in dirname basename sed uname sw_vers id stat awk grep wc tr cat mkdir cp mv rm chmod bash shasum; do
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
             printf '\nFAKESIGN:%s\n' "${id}" >>"${f}" ;;
    -dvvv)   id="$(/usr/bin/sed -n 's/^FAKESIGN://p' "${f}" 2>/dev/null | /usr/bin/tail -n 1)"
             if [[ -n "${id}" ]]; then printf 'Identifier=%s\nSignature=adhoc\n' "${id}" >&2; else echo "${f}: code object is not signed at all" >&2; exit 1; fi ;;
    *) echo "fake codesign: unexpected $*" >&2; exit 64 ;;
esac
FAKE
/bin/chmod +x "${SEAL}/launchctl" "${SEAL}/git" "${SEAL}/cargo" "${SEAL}/codesign"
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
    local SRUN_STUBS="STUB_GIT_DIRTY_AT=${STUB_GIT_DIRTY_AT:-} STUB_WS=${STUB_WS:-${T}/repo} STUB_PROBE=${STUB_PROBE:-} STUB_CREATE_KILL=${STUB_CREATE_KILL:-0} STUB_CREATE_FAIL=${STUB_CREATE_FAIL:-0} STUB_BUILD_TOUCH=${STUB_BUILD_TOUCH:-0} STUB_MV_KILL=${STUB_MV_KILL:-}"
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

printf '\n== 12. existing env.toml with threads pinned -> NOT rebuilt, NOT re-pinned\n'
mkdir -p "${T}/home12/.cauldron/data/prod"
printf '[supervisor]\nauto_restart = true\n\n[services.threads]\nversion = "aaaaaaaaaaaa-release"\naddr = "127.0.0.1:3753"\n' >"${T}/home12/.cauldron/data/prod/env.toml"
cp "${T}/home12/.cauldron/data/prod/env.toml" "${T}/env12.before"
RUN_HOME="${T}/home12" run "" 0 --dry-run
if [[ ${RC} -eq 0 ]] && has "threads is already pinned in"; then ok "rc 0, says the existing pin is kept"; else bad "rc ${RC}" "${OUT}"; fi
if [[ $(line_no "${RE_BUILD}") -eq 0 ]] && ! grep -qE -- "${RE_ANYPIN}" <<<"${OUT}"; then ok "no threads build or pin in the plan"; else bad "existing env would be rebuilt/re-pinned" "${OUT}"; fi
# F4: the byte check, on a REAL (sealed) run whose stub cauldron's `pin`
# really appends to env.toml — so a run that pinned WOULD change the bytes.
# Its positive sentinel is case 14, where the same stub pin does write.
mkdir -p "${T}/bin14"
CLOG="${T}/cauldron.log"
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
    "pin threads "*" --addr auto")
        printf '\n[services.threads]\nversion = "%s"\n' "$3" >>"${root}/data/${e}/env.toml" ;;
    "env remove ${e} --yes") /bin/rm -rf "${root}/data/${e}" ;;
    "install") ;;
    *) echo "stub cauldron: unexpected: $*" >&2; exit 64 ;;
esac
STUBC
/bin/chmod +x "${T}/bin14/cauldron"; : >"${T}/bin14/tui"; /bin/chmod +x "${T}/bin14/tui"
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${T}/home12" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 ]] && has "threads is already pinned in"; then ok "sealed real run: rc 0, says the existing pin is kept"; else bad "sealed real run: rc ${RC}" "${OUT}"; fi
if [[ "$(cat "${CLOG}")" == "--env prod install" ]]; then ok "sealed real run: cauldron called for install only (no build, no pin)"; else bad "existing env was rebuilt/re-pinned" "$(cat "${CLOG}")"; fi
if cmp -s "${T}/env12.before" "${T}/home12/.cauldron/data/prod/env.toml"; then ok "env.toml byte-identical after a REAL run"; else bad "env.toml changed" "$(diff "${T}/env12.before" "${T}/home12/.cauldron/data/prod/env.toml")"; fi
sealed_ok "case 12"

printf '\n== 13. existing env.toml WITHOUT threads -> told how, NOT pinned\n'
mkdir -p "${T}/home13/.cauldron/data/prod"
printf '[supervisor]\nauto_restart = true\n' >"${T}/home13/.cauldron/data/prod/env.toml"
RUN_HOME="${T}/home13" run "" 0 --dry-run
if [[ ${RC} -eq 0 ]] && has "threads is NOT pinned in the existing"; then ok "rc 0, says threads is not pinned"; else bad "rc ${RC}" "${OUT}"; fi
if [[ $(line_no "${RE_BUILD}") -eq 0 && $(line_no "${RE_PIN}") -eq 0 ]]; then ok "no threads build or pin step"; else bad "existing env would be edited" "${OUT}"; fi
# REL-4.S5c M3: the same, as a REAL (sealed) run whose stub `pin` really
# appends to env.toml. Its positive sentinel is 17b: the same stub, with the
# marker present, does pin and does change the bytes.
cp "${T}/home13/.cauldron/data/prod/env.toml" "${T}/env13.before"
[[ ! -e "$(MARK "${T}/home13")" ]] || bad "13: fixture has a marker"
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${T}/home13" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 ]] && has "threads is NOT pinned in the existing"; then ok "13 sealed real run: rc 0, says threads is not pinned"; else bad "13 sealed real run: rc ${RC}" "${OUT}"; fi
if [[ "$(cat "${CLOG}")" == "--env prod install" ]]; then ok "13 sealed real run: cauldron called for install only (no build, no pin)"; else bad "13: an env.toml without the marker was built/pinned" "$(cat "${CLOG}")"; fi
if cmp -s "${T}/env13.before" "${T}/home13/.cauldron/data/prod/env.toml"; then ok "13: env.toml byte-identical after a REAL run"; else bad "13: env.toml changed" "$(diff "${T}/env13.before" "${T}/home13/.cauldron/data/prod/env.toml")"; fi
sealed_ok "case 13"

printf '\n== 14. the step for real (SEALED): its own new version, signed in the env dir, shared build untouched\n'
B14() { echo "$1/.cauldron/data/shared/builds/threads/$2"; }
sha() { /usr/bin/shasum -a 256 "$1" 2>/dev/null | /usr/bin/awk '{print $1}'; }
# 14a. prod, fresh root: the shared label is not created at all.
: >"${CLOG}"; : >"${EXEC_LOG}"; : >"${SIGN_LOG}"; mkdir -p "${T}/home14"
RUN_HOME="${T}/home14" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 ]]; then ok "rc 0"; else bad "rc ${RC}, wanted 0" "${OUT}"; fi
want=$'--env prod versions install-workspace-probe\n--env prod env create prod --auto-restart\n--env prod build threads --json --env-copy\n--env prod pin threads feedc0de1234-release+prod --addr auto\n--env prod install'
if [[ "$(cat "${CLOG}")" == "${want}" ]]; then ok "cauldron called: workspace probe, env create, build --env-copy, pin threads <label>+prod --addr auto, install"; else bad "cauldron calls wrong" "$(cat "${CLOG}")"; fi
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
# Positive sentinel for sealed_ok's audit (N3): a copy of the script under
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
if [[ ${RC} -eq 0 ]] && has "step 4b will not build threads"; then ok "opposed: a dirty tree is NOT refused when 4b builds nothing (threads already pinned)"; else bad "refused although 4b builds nothing (rc ${RC})" "${OUT}"; fi

printf '\n== 17. a re-run COMPLETES an install that stopped before the threads pin (SEALED)\n'
H17="${T}/home17"; mkdir -p "${H17}"; : >"${CLOG}"; : >"${EXEC_LOG}"
STUB_BUILD_REFUSE=1 RUN_HOME="${H17}" srun "" --from "${T}/bin14"
if [[ ${RC} -ne 0 ]] && [[ -f "$(ENVT "${H17}")" ]] && ! grep -q 'services.threads' "$(ENVT "${H17}")" && [[ -e "$(MARK "${H17}")" ]]; then ok "17a partial: rc ${RC}, env.toml created, no threads pin, marker left"; else bad "17a: not the partial state (rc ${RC})" "$(find "${H17}" 2>&1)${OUT}"; fi
if [[ "$(cat "${CLOG}")" == $'--env prod versions install-workspace-probe\n--env prod env create prod --auto-restart\n--env prod build threads --json --env-copy' ]]; then ok "17a: env created, build refused, no pin, no install"; else bad "17a calls" "$(cat "${CLOG}")"; fi
sealed_ok "case 17a"
: >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H17}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 ]] && has "finishing step 4b now"; then ok "17b re-run: rc 0, says it finishes step 4b"; else bad "17b re-run did not finish 4b (rc ${RC})" "${OUT}"; fi
want17=$'--env prod versions install-workspace-probe\n--env prod build threads --json --env-copy\n--env prod pin threads feedc0de1234-release+prod --addr auto\n--env prod install'
if [[ "$(cat "${CLOG}")" == "${want17}" ]]; then ok "17b: build --env-copy, pin <label>+prod --addr auto, install; no second env create"; else bad "17b calls" "$(cat "${CLOG}")"; fi
if grep -qx '\[services.threads\]' "$(ENVT "${H17}")" && grep -qx 'version = "feedc0de1234-release+prod"' "$(ENVT "${H17}")"; then ok "17b: env.toml now pins threads"; else bad "17b: env.toml has no threads pin" "$(cat "$(ENVT "${H17}")")"; fi
if [[ ! -e "$(MARK "${H17}")" ]]; then ok "17b: marker cleared after the pin"; else bad "17b: marker left after the pin"; fi
if grep -q 'FAKESIGN:com.harness.threads' "$(B14 "${H17}" feedc0de1234-release+prod)/threads" 2>/dev/null; then ok "17b: the pinned version is the signed one"; else bad "17b: pinned version not signed"; fi
if has "PATH:     \`cauldron\` is not on your PATH yet (it is in ${H17}/.cauldron/bin)" && has "echo 'export PATH=\"${H17}/.cauldron/bin:\$PATH\"' >> ~/.zshrc"; then ok "17b: done says exactly how to put cauldron on PATH"; else bad "17b: no PATH instruction" "${OUT}"; fi
sealed_ok "case 17b"
cp "$(ENVT "${H17}")" "${T}/env17.before" 2>/dev/null || : >"${T}/env17.before"
mkdir -p "${H17}/.cauldron/data/prod" "${H17}/.cauldron/bin"; : >"$(MARK "${H17}")"
printf '#!/bin/bash\nexit 0\n' >"${H17}/.cauldron/bin/cauldron"; /bin/chmod +x "${H17}/.cauldron/bin/cauldron"
: >"${CLOG}"; : >"${EXEC_LOG}"
SRUN_PATH_EXTRA="${H17}/.cauldron/bin" RUN_HOME="${H17}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 && "$(cat "${CLOG}")" == "--env prod install" ]]; then ok "17c third run (pinned, stale marker): no rebuild, no re-pin"; else bad "17c rebuilt or re-pinned (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
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
# without threads: told the two commands, never pinned.
/bin/rm -f "$(MARK "${H19}")"; : >"${CLOG}"; : >"${EXEC_LOG}"
RUN_HOME="${H19}" srun "" --from "${T}/bin14"
if [[ ${RC} -eq 0 ]] && has "threads is NOT pinned in the existing" && [[ "$(cat "${CLOG}")" == "--env prod install" ]] && cmp -s "${T}/env19.before" "$(ENVT "${H19}")"; then ok "19b': after rm of the marker: rc 0, told how, env.toml byte-identical"; else bad "19b' (rc ${RC})" "$(cat "${CLOG}")${OUT}"; fi
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

printf '\n== 27. an unset variable aborts install with rc != 0 (bash 3.2 + EXIT trap, REL-4.S5e)\n'
# A copy with an unset variable read right before 4b's staging mkdir, where
# the EXIT trap has long been set (at 55f74d6b it was set two lines earlier).
M27="${T}/repo/scripts/install27"
/usr/bin/awk '$0 == "    run mkdir \"${STAGE}\"" { print "    : \"${conf_unbound_27}\"" } { print }' "${INSTALL}" >"${M27}"
if [[ "$(/usr/bin/grep -cxF '    : "${conf_unbound_27}"' "${M27}")" == 1 ]]; then ok "27: the copy has the injected unset read"; else bad "27: injection failed"; fi
mkdir -p "${T}/home27"; RUN_HOME="${T}/home27" RUN_AS="${M27}" run "" 0 --dry-run
if [[ ${RC} -ne 0 ]] && grep -qF 'conf_unbound_27: unbound variable' <<<"${ERR}" && ! has "== 5. supervisor"; then ok "27: the abort exits rc ${RC}, not 0, and stops before step 5"; else bad "27: a set -u abort read as rc ${RC}" "${OUT}"; fi
if [[ ${RC} -eq 70 ]] && has "install: ABORTED"; then ok "27: rc 70 and says ABORTED"; else bad "27: rc ${RC}, no ABORTED line" "${ERR}"; fi

printf '\n== 28. an unset variable aborts THIS suite with rc != 0 (REL-4.S5e)\n'
M28="${T}/suite28.sh"
/usr/bin/awk '{ print } $0 == "mkdir -p \"${T}/stub\" \"${T}/gitstub\" \"${T}/home\" \"${T}/empty\"" { print ": \"${conf_unbound_28}\"" }' "${SUITE}" >"${M28}"
# Counted as the WHOLE injected line: the string also appears in this case's
# own source, which the copy carries.
if [[ "$(/usr/bin/grep -cxF ': "${conf_unbound_28}"' "${M28}")" == 1 ]]; then ok "28: the suite copy has the injected unset read"; else bad "28: injection failed"; fi
set +e; out28="$(INSTALL="${INSTALL}" /bin/bash "${M28}" 2>&1)"; rc28=$?; set -e
if [[ ${rc28} -ne 0 ]] && grep -qF 'conf_unbound_28: unbound variable' <<<"${out28}" && ! grep -q '^passed ' <<<"${out28}"; then ok "28: the aborted suite exits rc ${rc28}, not 0"; else bad "28: an aborted suite read as rc ${rc28}" "${out28}"; fi

printf '\n--- refusal message (case 2) ---\n%s\n' "${SAVED_REFUSAL}"
printf '\npassed %d, failed %d\n' "${PASS}" "${FAIL}"
if [[ ${FAIL} -eq 0 ]]; then SUITE_DONE=1; exit 0; fi
exit 1
