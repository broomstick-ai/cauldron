#!/usr/bin/env bash
# Conformance test for build-threads.sh's SIGNER CHOICE (REL-4.S3).
#
# The guard: with HARNESS_SIGN_SCRIPT unset the default signer is AD-HOC
# (`codesign -s -`), never a script at a path in somebody's home directory;
# with it set, THAT script is the signer. Both arms are run, so a script that
# always signs ad-hoc fails case 2 and one that always calls a script fails
# case 1.
#
# It never builds (`--sign-only` on a scratch copy of a system binary), never
# touches a live daemon's path, and needs no signing identity.
set -euo pipefail
# REL-4.S7i: bash 3.2 (this machine's /bin/bash) exits 0 from a `set -u`
# abort under `set -e` when an EXIT trap is set. Every deliberate `exit`
# marks itself; the EXIT trap ends in __abort_guard, which turns an UNMARKED
# exit whose status reads 0 into 2.
__deliberate=
exit() { local s=$?; __deliberate=1; builtin exit "${1:-$s}"; }
__abort_guard() {
  if [ -z "$__deliberate" ] && [ "${__st:-0}" -eq 0 ]; then
    echo "$(basename "$0"): ABORTED (set -e/-u abort; not a pass)" >&2; builtin exit 2
  fi
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BT="${SCRIPT_DIR}/build-threads.sh"
BUNDLE_ID="com.harness.threads"

PASS=0; FAIL=0
ok()  { PASS=$((PASS+1)); printf '  ok   %s\n' "$1"; }
bad() { FAIL=$((FAIL+1)); printf '  FAIL %s\n' "$1"; if [[ $# -gt 1 ]]; then printf '       %s\n' "$2"; fi; }

T="$(mktemp -d "${TMPDIR:-/tmp}/build-threads-conf.XXXXXX")"
trap '__st=$?; rm -rf "${T}"; __abort_guard' EXIT

fixture() { cp /bin/ls "$1"; chmod u+w "$1"; }

# ── 1. HARNESS_SIGN_SCRIPT unset -> ad-hoc, with our identifier ──────────────
printf '\n== 1. default signer (HARNESS_SIGN_SCRIPT unset)\n'
fixture "${T}/threads"
set +e
out="$(env -u HARNESS_SIGN_SCRIPT bash "${BT}" --sign-only "${T}/threads" 2>&1)"; rc=$?
set -e
info="$(/usr/bin/codesign -dvvv "${T}/threads" 2>&1 || true)"
if [[ ${rc} -eq 0 ]]; then ok "rc 0"; else bad "rc ${rc}, wanted 0" "${out}"; fi
if grep -q '^\[build-threads\] mode      adhoc$' <<<"${out}"; then ok "mode adhoc"; else bad "mode is not adhoc" "${out}"; fi
if grep -q '^Signature=adhoc$' <<<"${info}"; then ok "the IMAGE is ad-hoc signed"; else bad "image not ad-hoc" "${info}"; fi
if grep -q "^Identifier=${BUNDLE_ID}\$" <<<"${info}"; then ok "the image carries ${BUNDLE_ID}"; else bad "identifier missing" "${info}"; fi
if grep -q '/Users/' <<<"$(grep '^\[build-threads\] signer' <<<"${out}")"; then
    bad "the default signer names a user directory" "${out}"
else
    ok "the default signer names no user directory"
fi

# ── 2. HARNESS_SIGN_SCRIPT set -> that script is called, with (binary, id) ──
printf '\n== 2. override signer (HARNESS_SIGN_SCRIPT set)\n'
fixture "${T}/threads2"
cat > "${T}/stub-signer.sh" <<STUB
#!/bin/sh
printf '%s %s\n' "\$1" "\$2" > "${T}/stub-called"
exit 0
STUB
chmod +x "${T}/stub-signer.sh"
set +e
out2="$(HARNESS_SIGN_SCRIPT="${T}/stub-signer.sh" bash "${BT}" --sign-only "${T}/threads2" 2>&1)"; rc2=$?
set -e
if grep -q '^\[build-threads\] mode      identity$' <<<"${out2}"; then ok "mode identity"; else bad "mode is not identity" "${out2}"; fi
if [[ "$(cat "${T}/stub-called" 2>/dev/null)" == "${T}/threads2 ${BUNDLE_ID}" ]]; then
    ok "the override script was called with (binary, ${BUNDLE_ID})"
else
    bad "the override script was not called as the signer" "${out2}"
fi
# The stub signs nothing, so identity-mode verify must REFUSE: the DR does not
# name the bundle id. A pass here would mean the verify is decorative.
if [[ ${rc2} -ne 0 ]]; then ok "identity verify refuses an image the signer did not sign (rc ${rc2})"; else bad "identity verify passed an unsigned image" "${out2}"; fi

printf '\npassed %d, failed %d\n' "${PASS}" "${FAIL}"
[[ ${FAIL} -eq 0 ]]
exit  # REL-4.S7i: a fall-through end is a deliberate exit (see __abort_guard)
