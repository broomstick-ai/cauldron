#!/usr/bin/env bash
# Conformance test for `add-account --login`.
#
# WHY THIS EXISTS. The login flow has TWO implementations of the same three
# primitives: Rust (`src/accounts.rs`, the library home, unit-tested) and bash
# (`scripts/add-account`, the thing the operator actually runs, because the
# `threads accounts login` CLI arm does not exist yet — see the HANDOFF in
# multiaccount-signin-report.md). A silent drift between them is a silent
# multi-account failure: the wrong keychain service name produces "logged in"
# state no spawn can find. So the two are pinned against each other here.
#
# WHAT IS ASSERTED
#   1. bash `kc_service` == the shared golden vectors == Rust
#      `keychain_service_for` (Rust side: `golden_vectors_match_the_shared_file`).
#   2. bash `fingerprint` == Rust `IdentityFingerprint::from_config_json`.
#   3. The manifest.toml bash writes is one Rust can adopt — proven by running
#      the real `threads accounts adopt` against a throwaway THREADS_HOME.
#   4. Every refusal path refuses. In particular: the script must NEVER invoke
#      claude when CLAUDE_CODE_OAUTH_TOKEN or CLAUDE_CONFIG_DIR is set.
#
# SAFETY. This test never runs a login, never writes the machine-default
# keychain service, and never invokes `claude` at all. It works in a temp
# THREADS_HOME and asserts, at the end, that the live login's keychain item is
# byte-for-byte as it was.
set -uo pipefail

# ── INTERPRETER GUARD: this test needs bash ≥ 4.4 ────────────────────────────
# Same reason as the scripts it proves: `${x@Q}` is 4.4+, and `/usr/bin/env
# bash` finds macOS's 3.2 whenever /bin precedes homebrew on PATH. A test that
# dies with "bad substitution" reads as a failure of the thing under test.
if [ "${BASH_VERSINFO[0]:-0}" -lt 4 ] \
   || { [ "${BASH_VERSINFO[0]}" -eq 4 ] && [ "${BASH_VERSINFO[1]:-0}" -lt 4 ]; }; then
  for _newer_bash in /opt/homebrew/bin/bash /usr/local/bin/bash /bin/bash; do
    [ -x "$_newer_bash" ] || continue
    case "$("$_newer_bash" -c 'echo "${BASH_VERSINFO[0]}.${BASH_VERSINFO[1]}"' 2>/dev/null)" in
      4.4|4.[5-9]|[5-9].*|[1-9][0-9].*) exec "$_newer_bash" "${BASH_SOURCE[0]}" "$@" ;;
    esac
  done
  echo "add-account-login_conformance_test: needs bash 4.4 or newer (brew install bash)." >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
THREADS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$THREADS_DIR/../.." && pwd)"
ADD_ACCOUNT="$SCRIPT_DIR/add-account"
VECTORS="$SCRIPT_DIR/login-golden-vectors.txt"
KEYCHAIN_SERVICE_BASE="Claude Code-credentials"

# ── BUILD-DIR GUARD (HOLDS R26) ──────────────────────────────────────────────
# Section 3 runs `cargo run -p threads`. With CARGO_TARGET_DIR unset, cargo
# builds into `$REPO_ROOT/target` — a 1.4 GB in-repo tree (measured again by
# R4.S9: exactly 1.4G), on a machine where disk is the binding constraint. So
# the test REFUSES before it does anything, rather than quietly building there.
# A target dir inside the repository is the same defect spelled differently,
# and is refused too.
if [ -z "${CARGO_TARGET_DIR:-}" ]; then
  echo "add-account-login_conformance_test: REFUSED — CARGO_TARGET_DIR is unset; cargo would build into $REPO_ROOT/target. Export the shared target dir (outside the repo) and re-run." >&2
  exit 2
fi
case "$CARGO_TARGET_DIR" in
  /*) ;;
  *) echo "add-account-login_conformance_test: REFUSED — CARGO_TARGET_DIR=$CARGO_TARGET_DIR is relative; cargo resolves it against the cwd, which may be the repo. Give an absolute path." >&2
     exit 2 ;;
esac
_ctd_abs="$(cd -P "$CARGO_TARGET_DIR" 2>/dev/null && pwd -P)" || _ctd_abs="$CARGO_TARGET_DIR"
case "$_ctd_abs/" in
  "$(cd -P "$REPO_ROOT" && pwd -P)"/*)
    echo "add-account-login_conformance_test: REFUSED — CARGO_TARGET_DIR=$CARGO_TARGET_DIR is inside the repository ($REPO_ROOT). Use the shared target dir outside it." >&2
    exit 2 ;;
esac

PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); printf '  ok   %s\n' "$*"; }
bad()  { FAIL=$((FAIL+1)); printf '  FAIL %s\n' "$*" >&2; }
check() { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1: got ${2@Q} want ${3@Q}"; fi; }

TMP="$(mktemp -d)"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

# Witness the live login up front; re-checked at the end.
live_mdat() {
  security find-generic-password -a "${USER:-claude-code-user}" -s "$KEYCHAIN_SERVICE_BASE" 2>/dev/null \
    | sed -n 's/.*"mdat"<timedate>=0x[0-9A-F]*  *"\(.*\)\\000".*/\1/p' | head -1
}
LIVE_MDAT_BEFORE="$(live_mdat)"

echo "== 1. keychain service derivation: bash vs the shared golden vectors =="
# Source the helpers out of add-account without executing its body: everything
# above the `# ── argument parsing` banner is definitions only.
HELPERS="$TMP/helpers.sh"
# Drop add-account's own `set -euo pipefail`: inherited into this shell it would
# abort the test on the first intentional failure we are here to observe.
sed -n '1,/^# ── argument parsing/p' "$ADD_ACCOUNT" \
  | grep -v '^set -euo pipefail$' > "$HELPERS"
# shellcheck disable=SC1090
. "$HELPERS" 2>/dev/null
# add-account's header re-derives SCRIPT_DIR/THREADS_DIR/REPO_ROOT from
# ${BASH_SOURCE[0]}, which while sourced is the temp copy — so it just pointed
# all three at $TMP. Restore ours; we only wanted the FUNCTIONS.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
THREADS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$THREADS_DIR/../.." && pwd)"
if ! declare -F kc_service >/dev/null; then
  bad "could not source add-account's helpers (did the banner move?)"
else
  n=0
  while IFS=$'\t' read -r dir want; do
    case "${dir:-}" in ''|\#*) continue ;; esac
    check "kc_service ${dir}" "$(kc_service "$dir")" "$want"
    n=$((n+1))
  done < "$VECTORS"
  if [ "$n" -ge 4 ]; then ok "read $n golden vectors"; else bad "only $n vectors"; fi
fi

echo "== 2. identity fingerprint: bash vs Rust =="
CJ="$TMP/.claude.json"
cat > "$CJ" <<'JSON'
{"oauthAccount":{"accountUuid":"acct-uuid-1","emailAddress":"someone@example.com",
"organizationUuid":"org-uuid-1","billingType":"stripe_subscription"},"other":1}
JSON
# The Rust unit test `identity_fingerprint_hashes_and_never_leaks` pins these
# same three values via `sha8`, so agreeing with them is agreeing with Rust.
exp_acct="$(printf %s 'acct-uuid-1' | shasum -a 256 | cut -c1-8)"
exp_mail="$(printf %s 'someone@example.com' | shasum -a 256 | cut -c1-8)"
exp_org="$(printf %s 'org-uuid-1' | shasum -a 256 | cut -c1-8)"
check "fingerprint" "$(fingerprint "$CJ")" \
  "acct=$exp_acct email=$exp_mail org=$exp_org tier=stripe_subscription"
# No oauthAccount → empty, not an error (a fresh slot looks like this).
printf '{}' > "$TMP/empty.json"
check "fingerprint of a fresh slot" "$(fingerprint "$TMP/empty.json")" ""
check "fingerprint of a missing file" "$(fingerprint "$TMP/nope.json")" ""
# The invariant that matters: no raw value can appear in the output.
fp_out="$(fingerprint "$CJ")"
for secret in acct-uuid-1 someone@example.com org-uuid-1; do
  case "$fp_out" in
    *"$secret"*) bad "fingerprint leaked $secret" ;;
    *) ok "fingerprint does not leak $secret" ;;
  esac
done

echo "== 2b. a missing keychain entry reads as EMPTY, not as sha8(\"\") =="
# Regression: piping a missing entry's empty output into shasum yields
# e3b0c442 — a plausible-looking hash that would make "no credential" look like
# a real distinct secret and defeat the NoCredential refusal.
ABSENT="Claude Code-credentials-$(printf %s "/nonexistent/$$" | shasum -a 256 | cut -c1-8)"
check "kc_secret_sha8 of an absent service" "$(kc_secret_sha8 "$ABSENT")" ""
check "sha8 of the empty string is the value we must never mistake for a secret" \
  "$(sha8_str '')" "e3b0c442"

echo "== 3. the manifest bash writes is one Rust can adopt =="
FAKE_HOME="$TMP/threads-home"
LABEL="conformance-slot"
SLOT="$FAKE_HOME/accounts/$LABEL"
mkdir -p "$SLOT"
printf '*\n' > "$FAKE_HOME/accounts/.gitignore"
SVC="$(kc_service "$SLOT")"
# Byte-identical to the heredoc in add-account's register step.
cat > "$SLOT/manifest.toml" <<EOF
label = "$LABEL"
captured_at = "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
source_config_dir = "$SLOT"
source_config_json = "$SLOT/.claude.json"
spawn_env = "CLAUDE_CONFIG_DIR=$SLOT"
credential_mode = "own-login"
keychain_service = "$SVC"
keychain_account = "${USER:-claude-code-user}"
note = "own login (not a clone): this slot completed its own \`claude auth login\`, so it holds an independent OAuth grant with its own refresh token and cannot be invalidated by any other account's refresh"
EOF
ADOPT_OUT="$(THREADS_HOME="$FAKE_HOME" cargo run --quiet \
  --manifest-path "$REPO_ROOT/Cargo.toml" -p threads --bin threads \
  -- accounts adopt "$LABEL" 2>&1)"
ADOPT_RC=$?
if [ "$ADOPT_RC" -eq 0 ]; then
  ok "threads accounts adopt parsed the bash-written manifest"
else
  bad "threads accounts adopt rejected the bash-written manifest: $ADOPT_OUT"
fi
if grep -q "own-login" "$FAKE_HOME/accounts/$LABEL/manifest.toml" \
   && THREADS_HOME="$FAKE_HOME" cargo run --quiet \
      --manifest-path "$REPO_ROOT/Cargo.toml" -p threads --bin threads \
      -- accounts list 2>/dev/null | grep -q "$LABEL"; then
  ok "the account shows up in the registry as own-login"
else
  bad "the account did not land in the registry"
fi

echo "== 4. refusal paths (no claude invocation anywhere in here) =="
refuses() {  # refuses <name> <expected substring> <env assignments...> -- args...
  local name="$1" want="$2"; shift 2
  local out rc
  out="$(env "$@" 2>&1)"; rc=$?
  if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -q "$want"; then
    ok "$name"
  else
    bad "$name (rc=$rc): ${out:0:220}"
  fi
}
refuses "refuses when CLAUDE_CODE_OAUTH_TOKEN is set" "#37512" \
  CLAUDE_CODE_OAUTH_TOKEN=dummy "$ADD_ACCOUNT" --login "$LABEL" --dry-run
# ...and the message must NAME THE VARIABLE and the command that clears it. A
# guard whose remedy the operator has to guess at is a guard that costs him the
# same half hour the refusals did.
OAUTH_MSG="$(env CLAUDE_CODE_OAUTH_TOKEN=dummy "$ADD_ACCOUNT" --login "$LABEL" --dry-run 2>&1 || true)"
for want in "CLAUDE_CODE_OAUTH_TOKEN" "unset CLAUDE_CODE_OAUTH_TOKEN" "DESTROYS"; do
  case "$OAUTH_MSG" in
    *"$want"*) ok "the token refusal says ${want@Q}" ;;
    *) bad "the token refusal never said ${want@Q}" ;;
  esac
done
refuses "refuses a label with a path traversal" "invalid label" \
  "$ADD_ACCOUNT" --login "../evil" --dry-run
refuses "refuses an unknown option" "unknown option" \
  "$ADD_ACCOUNT" --login "$LABEL" --wat
refuses "refuses two labels" "usage" \
  "$ADD_ACCOUNT" --login a b

# ── THE LAW: PRIOR STATE NEVER REFUSES A LOGIN ───────────────────────────────
#
# Everything below used to be a refusal, or a refusal behind a flag. Each is
# driven against EXACTLY the state that used to block it. These were written
# against the previous script first and watched to fail; the recorded failures
# are in the commit message.

# The obsolete flags must still be ACCEPTED. Saved commands, the
# `claude-accounts` wrapper and `login.rs` all still pass them, and a hard error
# on an obsolete flag would be a brand-new way to refuse him a login.
accepts() {  # accepts <name> -- args...
  local name="$1"; shift
  local out rc
  out="$("$@" 2>&1)"; rc=$?
  if [ "$rc" -eq 0 ]; then ok "$name"; else bad "$name (rc=$rc): ${out:0:220}"; fi
}
RELOGIN_LABEL="conformance-relogin"
RELOGIN_DIR="$THREADS_DIR/accounts/$RELOGIN_LABEL"
rm -rf "$RELOGIN_DIR"

accepts "--force is still accepted (and needs to be)" \
  "$ADD_ACCOUNT" --login "$RELOGIN_LABEL" --force --dry-run
accepts "--allow-same-account is still accepted (and needs to be)" \
  "$ADD_ACCOUNT" --login "$RELOGIN_LABEL" --allow-same-account --dry-run
accepts "both obsolete flags together are accepted" \
  "$ADD_ACCOUNT" --login "$RELOGIN_LABEL" --force --allow-same-account --dry-run

# ── AN EXISTING SNAPSHOT DOES NOT REFUSE ─────────────────────────────────────
# The exact state he hit: a slot dir carrying a manifest.toml. Against the
# previous script this died with *"<label> already has a snapshot at <dir> —
# pass --force"*. WITHOUT the flag, it must now run.
mkdir -p "$RELOGIN_DIR"
cat > "$RELOGIN_DIR/manifest.toml" <<'MANIFEST'
label = "conformance-relogin"
captured_at = "2026-08-28T00:00:00Z"
source_config_dir = "/nonexistent"
source_config_json = "/nonexistent/.claude.json"
spawn_env = "CLAUDE_CONFIG_DIR=/nonexistent"
credential_mode = "own-login"
keychain_service = "Claude Code-credentials-deadbeef"
keychain_account = "nobody"
MANIFEST
accepts "an EXISTING SNAPSHOT does not refuse a login (no flag)" \
  "$ADD_ACCOUNT" --login "$RELOGIN_LABEL" --dry-run
SNAP_OUT="$("$ADD_ACCOUNT" --login "$RELOGIN_LABEL" --dry-run 2>&1 || true)"
case "$SNAP_OUT" in
  *"already has a snapshot at"*) bad "the snapshot refusal is still in the output" ;;
  *) ok "the words 'already has a snapshot at' are gone from the login path" ;;
esac
case "$SNAP_OUT" in
  *"RE-LOGIN"*) ok "a re-login is ANNOUNCED rather than refused" ;;
  *) bad "a re-login was not announced" ;;
esac
# A dead-looking slot must be no different: add a config json with an identity
# and a login hint, i.e. a slot that has plainly been used before.
printf '%s' '{"oauthAccount":{"accountUuid":"a","emailAddress":"e@x.com","organizationUuid":"o","billingType":"max"}}' \
  > "$RELOGIN_DIR/.claude.json"
printf 'e@x.com\n' > "$RELOGIN_DIR/login-hint"
accepts "a slot with a manifest, a config json AND a hint still does not refuse" \
  "$ADD_ACCOUNT" --login "$RELOGIN_LABEL" --dry-run
# And nothing of his was destroyed just by preparing to sign in again.
for f in manifest.toml .claude.json login-hint; do
  if [ -f "$RELOGIN_DIR/$f" ]; then ok "$f survives a re-login preflight"
  else bad "$f was destroyed by a re-login preflight"; fi
done
rm -rf "$RELOGIN_DIR"

# ── CLAUDE_CONFIG_DIR IS A WARNING, NOT A REFUSAL ────────────────────────────
# Against the previous script this died with "CLAUDE_CONFIG_DIR is already set".
CCD_OUT="$(env CLAUDE_CONFIG_DIR=/tmp/whatever "$ADD_ACCOUNT" --login "$LABEL" --dry-run 2>&1)"
CCD_RC=$?
if [ "$CCD_RC" -eq 0 ]; then ok "a preset CLAUDE_CONFIG_DIR does not refuse"
else bad "a preset CLAUDE_CONFIG_DIR still refuses (rc=$CCD_RC): ${CCD_OUT:0:220}"; fi
case "$CCD_OUT" in
  *"IGNORED"*) ok "a preset CLAUDE_CONFIG_DIR is announced as ignored" ;;
  *) bad "a preset CLAUDE_CONFIG_DIR was not announced" ;;
esac

# ── THE VERDICT TABLE ────────────────────────────────────────────────────────
# `login_verdict_action` is the whole policy; the `case` below it only
# dispatches. Driving it here drives the policy, with no login, no keychain read
# and no credential of any kind.
if ! declare -F login_verdict_action >/dev/null; then
  bad "could not source login_verdict_action (did it move below the banner?)"
else
  check "Distinct registers"      "$(login_verdict_action Distinct)"     "register"
  check "SameAccount WARNS"       "$(login_verdict_action SameAccount)"  "warn"
  check "NoCredential reports"    "$(login_verdict_action NoCredential)" "report"
  check "SameSession REFUSES"     "$(login_verdict_action SameSession)"  "refuse"
  # An unknown verdict must fail CLOSED. A typo in the verdict computation must
  # never read as "register".
  check "an unknown verdict fails closed" "$(login_verdict_action wat)"  "refuse"
  # Exactly one verdict may stop a registration.
  n_refuse=0
  for v in Distinct SameAccount NoCredential SameSession; do
    [ "$(login_verdict_action "$v")" = refuse ] && n_refuse=$((n_refuse+1))
  done
  check "exactly one verdict stops a registration" "$n_refuse" "1"
fi

# THE CLONE GUARD SURVIVES, and its prose must still name the danger.
if awk '/^  refuse\)/,/;;/' "$ADD_ACCOUNT" | grep -q 'byte-identical'; then
  ok "the clone refusal is still present and still explains itself"
else
  bad "the clone refusal lost its explanation"
fi
# ...and it must NOT be reachable by a flag.
if awk '/^  refuse\)/,/;;/' "$ADD_ACCOUNT" | grep -q 'ALLOW_SAME_ACCOUNT\|FORCE'; then
  bad "the clone refusal consults a flag"
else
  ok "the clone refusal has no opt-out"
fi
# The obsolete flags are parsed and never read anywhere else.
if grep -q -- '--allow-same-account) ALLOW_SAME_ACCOUNT=1' "$ADD_ACCOUNT"; then
  ok "--allow-same-account is still parsed"
else
  bad "--allow-same-account is no longer parsed; old commands would break"
fi
case "$("$ADD_ACCOUNT" --help 2>&1)" in
  *"--allow-same-account"*) ok "--allow-same-account is still documented" ;;
  *) bad "usage no longer mentions --allow-same-account" ;;
esac

echo "== 5. --dry-run announces without invoking claude or writing the slot =="
DRY_LABEL="conformance-dry"
DRY_DIR="$THREADS_DIR/accounts/$DRY_LABEL"
rm -rf "$DRY_DIR"
DRY_OUT="$("$ADD_ACCOUNT" --login "$DRY_LABEL" --email nobody@example.com --dry-run 2>&1)"
DRY_RC=$?
if [ "$DRY_RC" -eq 0 ]; then ok "--dry-run exits 0"; else bad "--dry-run rc=$DRY_RC: ${DRY_OUT:0:300}"; fi
for want in "slot dir" "keychain service" "You will NOT be logged out" "stopping before invoking claude"; do
  case "$DRY_OUT" in
    *"$want"*) ok "--dry-run says ${want@Q}" ;;
    *) bad "--dry-run never said ${want@Q}" ;;
  esac
done
# It must announce the DERIVED service, and must never claim the default one.
case "$DRY_OUT" in
  *"$(kc_service "$DRY_DIR")"*) ok "--dry-run announces the derived keychain service" ;;
  *) bad "--dry-run did not announce the derived service" ;;
esac
# No credential value, and no dir created before the announce is accepted.
if [ ! -d "$DRY_DIR" ]; then ok "--dry-run created no slot dir"; else bad "--dry-run created $DRY_DIR"; fi

# The flag is accepted on the real preflight path, and writes nothing either.
rm -rf "$DRY_DIR"
SAME_OUT="$("$ADD_ACCOUNT" --login "$DRY_LABEL" --email nobody@example.com \
  --allow-same-account --dry-run 2>&1)"
SAME_RC=$?
if [ "$SAME_RC" -eq 0 ]; then ok "--allow-same-account --dry-run exits 0"; else bad "--allow-same-account --dry-run rc=$SAME_RC: ${SAME_OUT:0:300}"; fi
if [ ! -d "$DRY_DIR" ]; then ok "--allow-same-account --dry-run created no slot dir"; else bad "--allow-same-account --dry-run created $DRY_DIR"; fi

echo "== 5b. invoked through a SYMLINKED scripts dir outside any git repo =="
# The cauldron layout (H179): the daemon's THREADS_HOME is a data dir OUTSIDE
# the repo whose `scripts` entry is a symlink back into services/threads/scripts, and
# it execs `<home>/scripts/add-account --login <name> --no-browser` with
# cwd=<home>. A logical `cd $(dirname ...) && pwd` put REPO_ROOT two levels
# above the data dir, `git check-ignore` ran outside any repo, and every board
# sign-in died with "fatal: not a git repository". REPO_ROOT must follow the
# PHYSICAL scripts dir; the data dir (where the slot lands) stays the home.
LINKED_HOME="$TMP/linked-store"
LINKED_LABEL="conformance-linked"
mkdir -p "$LINKED_HOME"
ln -s "$SCRIPT_DIR" "$LINKED_HOME/scripts"
if git -C "$LINKED_HOME" rev-parse --git-dir >/dev/null 2>&1; then
  bad "precondition: $LINKED_HOME is inside a git repo, so this case proves nothing"
else
  ok "precondition: the linked home is outside any git repo"
fi
LINKED_OUT="$(cd "$LINKED_HOME" && env THREADS_HOME="$LINKED_HOME" \
  "$LINKED_HOME/scripts/add-account" --login "$LINKED_LABEL" --no-browser --dry-run 2>&1)"
LINKED_RC=$?
if [ "$LINKED_RC" -eq 0 ]; then ok "a login through the symlinked scripts dir is not refused"
else bad "a login through the symlinked scripts dir rc=$LINKED_RC: ${LINKED_OUT:0:300}"; fi
case "$LINKED_OUT" in
  *"not a git repository"*) bad "the gitignore guard ran outside the repo (not a git repository)" ;;
  *) ok "no 'not a git repository' from the gitignore guard" ;;
esac
case "$LINKED_OUT" in
  *"stopping before invoking claude"*) ok "the symlinked preflight reached the dry-run stop" ;;
  *) bad "the symlinked preflight never reached the dry-run stop" ;;
esac
case "$LINKED_OUT" in
  *"$LINKED_HOME/accounts/$LINKED_LABEL"*) ok "the slot still lands under the invoking home" ;;
  *) bad "the slot no longer lands under the invoking home: ${LINKED_OUT:0:300}" ;;
esac

echo "== 6. the live login is exactly as we found it =="
check "live keychain mdat unchanged" "$(live_mdat)" "$LIVE_MDAT_BEFORE"

printf '\n%s\n' "add-account-login conformance: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
