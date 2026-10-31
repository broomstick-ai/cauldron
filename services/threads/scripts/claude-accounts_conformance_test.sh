#!/usr/bin/env bash
# Conformance test for `claude-accounts`.
#
# WHY THIS EXISTS. This script is the one thing on the machine that can, if it
# is wrong, take out the operator's live Claude login — not by leaking a secret,
# but by refreshing the wrong credential or by offering a verb it must never
# offer. Everything it refuses to do is asserted here, by observation, against a
# FAKE claude that records the environment it was spawned with.
#
# THE FOUR PROPERTIES UNDER TEST
#   1. `logout`, in every spelling, is REFUSED — by the script and by
#      `threads accounts logout`.
#   2. The unsafe environments (CLAUDE_SECURESTORAGE_CONFIG_DIR set — empty
#      string especially — and CLAUDE_CODE_OAUTH_TOKEN set) are refused before
#      anything runs.
#   3. Rotation probes EXACTLY the slots that own their login, and NOTHING else:
#      not the machine default, not a clone, not a stranded capture, not a slot
#      whose plan says refresh but whose config dir is missing. Proven by
#      counting and inspecting the fake claude's invocations.
#   4. `--dry-run` invokes claude zero times.
#
# SAFETY. This test never logs in, never logs out, never invokes the real
# `claude`, and works entirely from a fixture inventory in a temp dir. It
# witnesses the live login's keychain item at the start and asserts it is
# byte-for-byte the same at the end.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
THREADS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$THREADS_DIR/../.." && pwd)"
CA="$SCRIPT_DIR/claude-accounts"
KEYCHAIN_SERVICE_BASE="Claude Code-credentials"

PASS=0; FAIL=0
ok()  { PASS=$((PASS+1)); printf '  ok   %s\n' "$*"; }
bad() { FAIL=$((FAIL+1)); printf '  FAIL %s\n' "$*" >&2; }
# No ${x@Q} here: under macOS's /bin/bash 3.2 it is a "bad substitution" that
# aborts the helper BEFORE `bad` counts the failure, so a failing check passed.
check() { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1: got '$2' want '$3'"; fi; }
contains() { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1: ${2:0:200}" ;; esac; }
lacks() { case "$2" in *"$3"*) bad "$1: found '$3'" ;; *) ok "$1" ;; esac; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

live_mdat() {
  security find-generic-password -a "${USER:-claude-code-user}" -s "$KEYCHAIN_SERVICE_BASE" 2>/dev/null \
    | sed -n 's/.*"mdat"<timedate>=0x[0-9A-F]*  *"\(.*\)\\000".*/\1/p' | head -1
}
LIVE_MDAT_BEFORE="$(live_mdat)"

# ── the fixture inventory ────────────────────────────────────────────────────
# One slot of every shape the classifier can produce. Exactly ONE of them is
# eligible for a refresh; everything else must be left alone, each for its own
# reason. `days_left` is deliberately comfortable on the clones — a healthy
# countdown is exactly how a clone disguises itself.
#
# The machine default carries `folds_into` and work-2 carries `is_live_login`,
# which is the shape `accounts::mark_live_login` produces: the default store is
# signed in as work-2's account, so it is not a separate row. Rotation still
# sees it and must still refuse it — the collapse is a DISPLAY decision, and
# hiding a credential from the machinery would be a safety regression.
FIXTURE="$TMP/health.json"
SLOTS="$TMP/slots"
mkdir -p "$SLOTS/work-2" "$SLOTS/secondary-live" "$SLOTS/primary" "$SLOTS/half"
cat > "$FIXTURE" <<JSON
{"now_ms": 1785000000000, "keychain_account": "test", "warn_below_days": 7.0,
 "accounts_dir": "$SLOTS", "live_login": "work-2", "slots": [
  {"label":"default","config_dir":null,"credential_mode":null,
   "keychain_service":"$KEYCHAIN_SERVICE_BASE",
   "identity":{"acct":"deadbeef","email":"cafe0000","org":"12345678",
               "tier":"stripe_subscription"},
   "secret_sha8":"aaaaaaaa","health":{"state":"live"},"days_left":28.0,
   "access_expired":true,"subscription_type":"max","is_machine_default":true,
   "is_live_login":false,"folds_into":"work-2","warning":null,
   "rotation":{"action":"skip","reason":"the machine default is your live login"}},

  {"label":"secondary-live","config_dir":"$SLOTS/secondary-live",
   "credential_mode":"keychain-cloned","keychain_service":"svc-clone",
   "identity":null,"secret_sha8":"aaaaaaaa",
   "health":{"state":"clone-of","of":"the live login"},"days_left":28.0,
   "access_expired":true,"subscription_type":"max","is_machine_default":false,
   "warning":"meaningless",
   "rotation":{"action":"skip","reason":"clone of the live login"}},

  {"label":"primary","config_dir":"$SLOTS/primary",
   "credential_mode":"keychain-cloned","keychain_service":"svc-stranded",
   "identity":null,"secret_sha8":"bbbbbbbb","health":{"state":"stranded"},
   "days_left":26.0,"access_expired":true,"subscription_type":"max",
   "is_machine_default":false,"warning":"meaningless",
   "rotation":{"action":"skip","reason":"a capture that has diverged"}},

  {"label":"half","config_dir":"$SLOTS/half","credential_mode":null,
   "keychain_service":"svc-half","identity":null,"secret_sha8":null,
   "health":{"state":"no-login"},"days_left":null,"access_expired":true,
   "subscription_type":null,"is_machine_default":false,"warning":null,
   "rotation":{"action":"manual-login-required","reason":"no credential in this slot"}},

  {"label":"work-2","config_dir":"$SLOTS/work-2","credential_mode":"own-login",
   "keychain_service":"svc-work2","identity":{"acct":"deadbeef","email":"cafe0000",
   "org":"12345678","tier":"stripe_subscription"},"secret_sha8":"cccccccc",
   "health":{"state":"live"},"days_left":24.5,"access_expired":true,
   "subscription_type":"max","is_machine_default":false,"is_live_login":true,
   "live_store_days_left":28.0,"live_store_health":{"state":"live"},
   "warning":null,
   "rotation":{"action":"refresh","reason":""}}
 ]}
JSON

# The other shape: the machine login is an account no named slot carries. It
# must NOT be folded away — that would hide a live credential from the operator.
UNFOLDED="$TMP/unfolded.json"
python3 - "$FIXTURE" "$UNFOLDED" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
d["live_login"] = "default"
for s in d["slots"]:
    if s["label"] == "default":
        s["identity"] = {"acct": "0ddba11", "email": "0ddba11",
                         "org": "0ddba11", "tier": "stripe_subscription"}
        s["is_live_login"] = True
        s.pop("folds_into", None)
    elif s["label"] == "work-2":
        s["is_live_login"] = False
        s.pop("live_store_days_left", None)
        s.pop("live_store_health", None)
json.dump(d, open(sys.argv[2], "w"))
PY

# ── the fake claude: records, never acts ─────────────────────────────────────
FAKE="$TMP/fake-claude"
CALLS="$TMP/calls.log"
cat > "$FAKE" <<'FAKE_EOF'
#!/usr/bin/env bash
{
  printf 'INVOCATION\n'
  printf 'CLAUDE_CONFIG_DIR=%s\n' "${CLAUDE_CONFIG_DIR-<UNSET>}"
  printf 'CLAUDE_CODE_OAUTH_TOKEN=%s\n' "${CLAUDE_CODE_OAUTH_TOKEN-<UNSET>}"
  printf 'ANTHROPIC_API_KEY=%s\n' "${ANTHROPIC_API_KEY-<UNSET>}"
  printf 'CLAUDE_SECURESTORAGE_CONFIG_DIR=%s\n' "${CLAUDE_SECURESTORAGE_CONFIG_DIR-<UNSET>}"
  printf 'ARGV=%s\n' "$*"
} >> "$FAKE_CALLS"
exit 0
FAKE_EOF
chmod +x "$FAKE"

# How many times the fake claude ran. `grep -c` exits 1 on zero matches, which
# would fire a `||` fallback and print the count twice — awk just counts.
n_calls() {
  [ -f "$CALLS" ] || { printf '0'; return; }
  awk '/^INVOCATION$/ { n++ } END { printf "%d", n+0 }' "$CALLS"
}

run_ca() {  # run_ca <extra env assignments...> -- <args...>
  : > "$CALLS"
  env CLAUDE_ACCOUNTS_TEST=1 \
      CLAUDE_ACCOUNTS_HEALTH_JSON="$FIXTURE" \
      CLAUDE_ACCOUNTS_CLAUDE_BIN="$FAKE" \
      CLAUDE_ACCOUNTS_DIR="$TMP/work" \
      CLAUDE_ACCOUNTS_LOG="$TMP/rotation.log" \
      FAKE_CALLS="$CALLS" \
      "$@" 2>&1
}

echo "== 1. logout is refused, in every spelling and by both front doors =="
for verb in logout signout sign-out --logout; do
  out="$(run_ca "$CA" "$verb")"; rc=$?
  if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -q "REFUSED"; then
    ok "claude-accounts $verb is refused"
  else
    bad "claude-accounts $verb (rc=$rc): ${out:0:200}"
  fi
done
out="$(run_ca "$CA" logout)"
contains "the refusal explains the server-side revoke" "$out" "revoke the refresh token"
contains "the refusal says you never need it" "$out" "signed in at once"
lacks "the refusal never suggests running claude auth logout" "$out" "run \`claude auth logout\`"
# Refused BEFORE the env checks, so it is refused even in an unsafe shell —
# a user in a broken environment still must not be told logout is available.
out="$(env CLAUDE_CODE_OAUTH_TOKEN=x "$CA" logout 2>&1)"; rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -q "REFUSED"; then
  ok "logout is refused even in an otherwise-refused environment"
else bad "logout in a bad env (rc=$rc): ${out:0:200}"; fi

THREADS_BIN=""
# CARGO_TARGET_DIR first: under cauldron the build goes to a shared directory
# outside the repo, and `$REPO_ROOT/target` does not exist.
for b in ${CARGO_TARGET_DIR:+"$CARGO_TARGET_DIR/release/threads" "$CARGO_TARGET_DIR/debug/threads"} \
         "$REPO_ROOT/target/release/threads" "$REPO_ROOT/target/debug/threads"; do
  [ -x "$b" ] && THREADS_BIN="$b" && break
done
if [ -n "$THREADS_BIN" ]; then
  out="$(THREADS_HOME="$TMP/home" "$THREADS_BIN" accounts logout 2>&1)"; rc=$?
  if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -q "REFUSED"; then
    ok "threads accounts logout is refused too"
  else bad "threads accounts logout (rc=$rc): ${out:0:200}"; fi
else
  bad "no threads binary built — run: cargo build -p threads"
fi

echo "== 2. unsafe environments are refused before anything runs =="
refuses() {  # refuses <name> <want substring> <env...> -- <cmd...>
  local name="$1" want="$2"; shift 2
  local out rc; out="$(env "$@" 2>&1)"; rc=$?
  if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -q "$want"; then ok "$name"
  else bad "$name (rc=$rc): ${out:0:220}"; fi
}
# The empty string is the dangerous value: it drops the keychain suffix, which
# is what would let a slot's login overwrite the machine-default credential.
refuses "refuses CLAUDE_SECURESTORAGE_CONFIG_DIR='' (the dangerous one)" "EMPTY STRING" \
  CLAUDE_SECURESTORAGE_CONFIG_DIR= "$CA" list
refuses "refuses CLAUDE_SECURESTORAGE_CONFIG_DIR=<path>" "decouples the keychain service name" \
  CLAUDE_SECURESTORAGE_CONFIG_DIR=/tmp/x "$CA" list
refuses "refuses CLAUDE_CODE_OAUTH_TOKEN (upstream #37512)" "#37512" \
  CLAUDE_CODE_OAUTH_TOKEN=dummy "$CA" list
refuses "refuses an unknown command" "unknown command" "$CA" frobnicate
refuses "refuses an unknown login option" "unknown option" "$CA" login work-2 --wat
refuses "refuses a traversal-shaped label" "invalid label" "$CA" login "../evil"

echo "== 3. --dry-run invokes claude zero times =="
out="$(run_ca "$CA" rotate --dry-run)"; rc=$?
check "rotate --dry-run exits 0" "$rc" "0"
check "rotate --dry-run invoked claude 0 times" "$(n_calls)" "0"
contains "--dry-run names the slot it would refresh" "$out" "work-2"
contains "--dry-run says nothing was invoked" "$out" "claude was not invoked"

echo "== 4. rotation probes ONLY the slot that owns its login =="
out="$(run_ca "$CA" rotate)"; rc=$?
check "rotate exits 0" "$rc" "0"
check "claude was invoked exactly once" "$(n_calls)" "1"
calls="$(cat "$CALLS" 2>/dev/null)"

# The single most important assertion in this file.
check "the one probe ran under work-2's config dir" \
  "$(grep '^CLAUDE_CONFIG_DIR=' "$CALLS" | head -1)" "CLAUDE_CONFIG_DIR=$SLOTS/work-2"
lacks "no probe ever ran with CLAUDE_CONFIG_DIR unset (that is the live login)" \
  "$calls" "CLAUDE_CONFIG_DIR=<UNSET>"
for forbidden in "$SLOTS/secondary-live" "$SLOTS/primary" "$SLOTS/half"; do
  lacks "never probed $(basename "$forbidden")" "$calls" "CLAUDE_CONFIG_DIR=$forbidden"
done

# The probe is the free one, and it is not an auth verb.
contains "the probe asks for /usage" "$calls" "ARGV=-p /usage"
contains "the probe does not persist a session" "$calls" "--no-session-persistence"
lacks "the probe is never an auth subcommand" "$calls" "ARGV=auth"
lacks "the probe never says logout" "$calls" "logout"

# The child env is scrubbed of everything that could redirect the credential.
check "CLAUDE_CODE_OAUTH_TOKEN is unset in the child" \
  "$(grep '^CLAUDE_CODE_OAUTH_TOKEN=' "$CALLS" | head -1)" "CLAUDE_CODE_OAUTH_TOKEN=<UNSET>"
check "ANTHROPIC_API_KEY is unset in the child" \
  "$(grep '^ANTHROPIC_API_KEY=' "$CALLS" | head -1)" "ANTHROPIC_API_KEY=<UNSET>"
check "CLAUDE_SECURESTORAGE_CONFIG_DIR is unset in the child" \
  "$(grep '^CLAUDE_SECURESTORAGE_CONFIG_DIR=' "$CALLS" | head -1)" \
  "CLAUDE_SECURESTORAGE_CONFIG_DIR=<UNSET>"

echo "== 5. every refusal is reported, not silently dropped =="
contains "the machine default is reported as skipped" "$out" "default"
contains "the clone is reported as skipped" "$out" "secondary-live"
contains "the stranded capture is reported" "$out" "primary"
contains "the slot needing a human is named" "$out" "claude-accounts login half"
contains "the live login is witnessed" "$out" "your live login:"

echo "== 6. a plan that says 'refresh' with no config dir is refused =="
# Belt-and-braces: if the Rust side and this script ever disagree, the script
# must refuse rather than probe with CLAUDE_CONFIG_DIR unset — which IS the
# machine default, i.e. the operator's live login.
BAD_FIXTURE="$TMP/bad.json"
python3 - "$FIXTURE" "$BAD_FIXTURE" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
for s in d["slots"]:
    if s["label"] == "default":
        s["rotation"] = {"action": "refresh", "reason": "a plan that must not be obeyed"}
json.dump(d, open(sys.argv[2], "w"))
PY
: > "$CALLS"
out="$(env CLAUDE_ACCOUNTS_TEST=1 CLAUDE_ACCOUNTS_HEALTH_JSON="$BAD_FIXTURE" \
  CLAUDE_ACCOUNTS_CLAUDE_BIN="$FAKE" CLAUDE_ACCOUNTS_DIR="$TMP/work2" \
  CLAUDE_ACCOUNTS_LOG="$TMP/rotation2.log" FAKE_CALLS="$CALLS" \
  "$CA" rotate 2>&1)"
lacks "a config-dir-less 'refresh' never reaches claude" \
  "$(cat "$CALLS")" "CLAUDE_CONFIG_DIR=<UNSET>"
contains "and it says so out loud" "$out" "refused rather than probe the machine default"

echo "== 7. the machine-readable surfaces parse =="
out="$(run_ca "$CA" rotate --json)"
if printf '%s' "$out" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["slots"]; print("ok")' >/dev/null 2>&1; then
  ok "rotate --json emits parseable JSON"
else bad "rotate --json is not parseable: ${out:0:200}"; fi
out="$(run_ca "$CA" list)"
contains "list renders the fixture" "$out" "work-2"
contains "list flags the live login" "$out" "← LIVE LOGIN"
contains "list explains the stranded capture" "$out" "STRANDED CLONE"
# The listing is spoken aloud and lands in reports: it must be hash-only.
lacks "list prints no keychain service names" "$out" "svc-work2"

echo "== 8. the machine-default row is COLLAPSED, and never at the cost of hiding one =="
# §1 of task-accounts-cli-upgrade. The operator: "the default is always going to
# be one of the other two. So why don't we just indicate which one is currently
# set as the default and then just have two slots displayed."
out="$(run_ca "$CA" list)"
lacks "no separate 'default' row when it is one of the accounts" "$out" "
  default "
contains "the account it IS carries the marker" "$out" "work-2"
contains "and the marker is on work-2's line" \
  "$(printf '%s' "$out" | grep 'work-2' | head -1)" "← LIVE LOGIN"
contains "it says what being the live login means" "$out" "plain \`claude\` signs in as"
contains "and carries the machine store's own clock" "$out" "28.0 days left"
# Numbering is what the menu indexes by, so it must match the rows printed.
out_n="$(run_ca "$CA" list)"
check "the collapsed listing has one row per real account" \
  "$(printf '%s' "$out_n" | grep -cE '^  (work-2|secondary-live|primary|half) ')" "4"

# The other direction: an unregistered live login must stay visible.
run_unfolded() {
  env CLAUDE_ACCOUNTS_TEST=1 CLAUDE_ACCOUNTS_HEALTH_JSON="$UNFOLDED" \
      CLAUDE_ACCOUNTS_CLAUDE_BIN="$FAKE" CLAUDE_ACCOUNTS_DIR="$TMP/work-unfolded" \
      CLAUDE_ACCOUNTS_LOG="$TMP/rotation-unfolded.log" FAKE_CALLS="$CALLS" \
      "$@" 2>&1
}
out="$(run_unfolded "$CA" list)"
contains "an unregistered live login is still listed" "$out" "default"
contains "…and is the one marked live" \
  "$(printf '%s' "$out" | grep 'default' | head -1)" "← LIVE LOGIN"
contains "…and says how to give it a name" "$out" "gives it a slot of its own"

echo "== 9. set-live-login refuses everything it must, and invokes claude zero times =="
# This is the command that writes the operator's REAL machine credential. Every
# assertion here is about it declining to.
: > "$CALLS"
out="$(run_ca "$CA" set-live-login nope)"; rc=$?
check "an unknown account is refused" "$rc" "1"
contains "…and says so" "$out" "there is no account"
out="$(run_ca "$CA" set-live-login default)"; rc=$?
contains "the machine login itself cannot be 'set' to" "$out" "IS the machine login"
# A slot with no login of its own is not a thing to point the machine login at.
for slot in half primary secondary-live; do
  out="$(run_ca "$CA" set-live-login "$slot")"; rc=$?
  if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -q "no working login of its own"; then
    ok "set-live-login refuses $slot (no own login)"
  else bad "set-live-login $slot (rc=$rc): ${out:0:200}"; fi
done
# The outgoing side: if the account in the machine store has no slot, it is the
# only copy of that credential and overwriting it costs a browser trip.
out="$(run_unfolded "$CA" set-live-login work-2)"; rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -q "only copy of its credential"; then
  ok "refuses to overwrite a machine login with nowhere else to live"
else bad "unfolded set-live-login (rc=$rc): ${out:0:300}"; fi
contains "…and says exactly how to make it safe" "$out" "claude-accounts login"
# Already there = no browser trip at all.
out="$(run_ca "$CA" set-live-login work-2)"; rc=$?
check "setting it to the account it already is exits 0" "$rc" "0"
contains "…and says so instead of signing in again" "$out" "already signed in as that account"
# ── A PINNED CLAUDE_CONFIG_DIR IS A WARNING, NOT A REFUSAL ───────────────────
# This assertion used to be the opposite: `rc != 0` and "CLAUDE_CONFIG_DIR is
# set". That refusal stood between the operator and a login, and it fired
# constantly — ANY shell spawned from inside an account slot inherits the
# variable, so simply working in one of his own accounts locked him out of
# `set-live-login`.
#
# It was also guarding nothing: the login is invoked with `env -u
# CLAUDE_CONFIG_DIR`, so this shell's value could never reach the child. It is
# now announced as ignored and the run continues.
out="$(env CLAUDE_ACCOUNTS_TEST=1 CLAUDE_ACCOUNTS_HEALTH_JSON="$FIXTURE" \
  CLAUDE_ACCOUNTS_CLAUDE_BIN="$FAKE" CLAUDE_ACCOUNTS_DIR="$TMP/work3" \
  CLAUDE_CONFIG_DIR=/tmp/somewhere "$CA" set-live-login work-2 2>&1)"; rc=$?
if [ "$rc" -eq 0 ]; then
  ok "a pinned CLAUDE_CONFIG_DIR does NOT refuse the run"
else bad "pinned CLAUDE_CONFIG_DIR still refuses (rc=$rc): ${out:0:200}"; fi
contains "…and is announced as ignored rather than fatal" "$out" "IGNORED"
if printf '%s' "$out" | grep -q "REFUSING: CLAUDE_CONFIG_DIR"; then
  bad "the CLAUDE_CONFIG_DIR refusal is still in the output"
else
  ok "the CLAUDE_CONFIG_DIR refusal is gone from set-live-login"
fi
check "none of those refusals invoked claude" "$(n_calls)" "0"

# --dry-run: announces, and touches nothing at all.
: > "$CALLS"
DRYDIR="$TMP/work-dry"
out="$(env CLAUDE_ACCOUNTS_TEST=1 CLAUDE_ACCOUNTS_HEALTH_JSON="$FIXTURE" \
  CLAUDE_ACCOUNTS_CLAUDE_BIN="$FAKE" CLAUDE_ACCOUNTS_DIR="$DRYDIR" \
  FAKE_CALLS="$CALLS" "$CA" set-live-login primary --dry-run 2>&1)" || true
# `primary` is stranded, so it is refused before the announce — use the one
# eligible target instead, with the unfolded fixture where work-2 is not live.
out="$(env CLAUDE_ACCOUNTS_TEST=1 CLAUDE_ACCOUNTS_HEALTH_JSON="$UNFOLDED" \
  CLAUDE_ACCOUNTS_CLAUDE_BIN="$FAKE" CLAUDE_ACCOUNTS_DIR="$DRYDIR" \
  FAKE_CALLS="$CALLS" "$CA" set-live-login work-2 --force --dry-run 2>&1)"; rc=$?
check "set-live-login --dry-run exits 0" "$rc" "0"
check "set-live-login --dry-run invoked claude 0 times" "$(n_calls)" "0"
contains "…and says what it would have backed up" "$out" "Undo with:"
contains "…and states the fresh-login reason out loud" "$out" "signs in FRESH"
contains "…and promises no logout" "$out" "nothing is revoked"
if [ ! -d "$DRYDIR/backups" ]; then ok "--dry-run wrote no backup dir"
else bad "--dry-run created $DRYDIR/backups"; fi

echo "== 9a. set-live-login on a WIPED slot: identity known proceeds, unknown refuses (H179) =="
# The operator pressed `s` on a wiped account (alice-example-com) and nothing
# happened: the tool refused any non-live target. The machine store is signed
# in FRESH and verified against the recorded identity, so a wiped slot whose
# identity is known is a fine target. One whose identity is unknown is not.
WIPED="$TMP/wiped.json"
python3 - "$FIXTURE" "$WIPED" "$SLOTS" <<'PY2'
import json, sys
d = json.load(open(sys.argv[1]))
for label, ident in (("wiped-known", {"acct": "feedface", "email": "f00dcafe",
                                      "org": "12345678", "tier": "stripe_subscription"}),
                     ("wiped-anon", None)):
    d["slots"].append({"label": label, "config_dir": sys.argv[3] + "/" + label,
        "credential_mode": "own-login", "keychain_service": "svc-" + label,
        "identity": ident, "secret_sha8": "dddddddd", "health": {"state": "wiped"},
        "days_left": None, "access_expired": True, "subscription_type": "max",
        "is_machine_default": False, "is_live_login": False, "warning": "dead",
        "rotation": {"action": "manual-login-required", "reason": "wiped"}})
json.dump(d, open(sys.argv[2], "w"))
PY2
run_wiped() {
  : > "$CALLS"
  env CLAUDE_ACCOUNTS_TEST=1 CLAUDE_ACCOUNTS_HEALTH_JSON="$WIPED" \
      CLAUDE_ACCOUNTS_CLAUDE_BIN="$FAKE" CLAUDE_ACCOUNTS_DIR="$TMP/work-wiped" \
      CLAUDE_ACCOUNTS_LOG="$TMP/rotation-wiped.log" FAKE_CALLS="$CALLS" \
      "$@" 2>&1
}
out="$(run_wiped "$CA" set-live-login wiped-known --dry-run)"; rc=$?
check "a wiped slot with a recorded identity is NOT refused (--dry-run exits 0)" "$rc" "0"
contains "…and reaches the dry-run stop point" "$out" "--dry-run: stopping here"
lacks "…and does not say 'no working login of its own' as a refusal" "$out" "REFUSING"
contains "…and notes the slot's own login stays wiped" "$out" "own slot has no working login"
lacks "…and does not claim the slot keeps a separate login" "$out" "'wiped-known' keeps its own separate login"
check "…and invoked claude 0 times" "$(n_calls)" "0"
out="$(run_wiped "$CA" set-live-login wiped-anon --dry-run)"; rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -q "no working login of its own"; then
  ok "a wiped slot with NO recorded identity is still refused"
else bad "wiped-anon set-live-login (rc=$rc): ${out:0:300}"; fi
lacks "…and never reaches the dry-run stop point" "$out" "--dry-run: stopping here"

echo "== 9a'. already signed in prints a stable marker, dry or not (H179) =="
# The board's preflight is a --dry-run. On an account that already IS the
# machine login it exited 0 WITHOUT the dry-run marker, which the board reads
# as "never checked anything" and refuses. It must say so in a stable line.
out="$(run_ca "$CA" set-live-login work-2 --dry-run)"; rc=$?
check "--dry-run on the account already live exits 0" "$rc" "0"
contains "…and prints the already-live marker" "$out" "already-live:"
check "…and invoked claude 0 times" "$(n_calls)" "0"

echo "== 9b. the whole set-live-login flow, driven against a fake claude =="
# The one code path in this repo that deliberately writes the MACHINE-DEFAULT
# credential. Everything up to and including the `claude` invocation runs here
# for real — the backups, the keychain-to-keychain copy, the restore script and
# the verification — with a fake claude standing in for the browser round trip.
#
# SINCE R12.5 (inspect B1) IT RUNS AGAINST A TEMP HOME AND A FAKE KEYCHAIN, and
# never the operator's: the guard's revert can WRITE the machine item and
# ~/.claude.json, and a running session rewriting his real ~/.claude.json mid-run
# once made that revert fire on the real ones. The script itself now refuses a
# guarded write under test when HOME is the real home or `security` is the
# system binary (claude-accounts-identity-guard_conformance_test.sh, H1a).
: > "$CALLS"
LIVEDIR="$TMP/work-live"
KCBIN="$TMP/kcbin"; mkdir -p "$KCBIN" "$TMP/home-9b"
cat > "$KCBIN/security" <<'PY'
#!/usr/bin/env python3
import json, os, shlex, sys
# R12.10 (N4): the sentinel only a test fake answers; claude-accounts refuses a
# guarded write under test unless the `security` on PATH answers it.
if sys.argv[1:] == ["--claude-accounts-fake"]: sys.exit(0)
store_path = os.environ.get("FAKE_KC")
if not store_path:
    sys.stderr.write("fake security: FAKE_KC unset — refusing (never the real keychain)\n"); sys.exit(99)
def load():
    try: return json.load(open(store_path))
    except Exception: return {"items": {}, "clock": 0}
def save(d): json.dump(d, open(store_path, "w"), sort_keys=True)
def opts(args, valued):
    o, i = {}, 0
    while i < len(args):
        a = args[i]
        if a in valued and i + 1 < len(args): o[a] = args[i+1]; i += 2
        else: o[a] = True; i += 1
    return o
def run(argv):
    if not argv: return 1
    cmd, rest = argv[0], argv[1:]
    d = load()
    if cmd == "find-generic-password":
        o = opts(rest, {"-a", "-s"})
        it = d["items"].get(o.get("-s"))
        if it is None: return 44
        if o.get("-w"):
            sys.stdout.write(bytes.fromhex(it["hex"]).decode("utf-8", "replace") + "\n")
        else:
            m = "2026102%010dZ" % it["mdat"]
            sys.stdout.write('keychain: "fake"\nattributes:\n    "svce"<blob>="%s"\n'
                             '    "mdat"<timedate>=0x%s  "%s\\000"\n' % (o.get("-s"), m.encode().hex().upper(), m))
        return 0
    if cmd == "add-generic-password":
        o = opts(rest, {"-a", "-s", "-w", "-X"})
        hx = o["-X"] if "-X" in o else str(o.get("-w", "")).encode().hex()
        if o.get("-s") in d["items"] and not o.get("-U"): return 45
        d["clock"] += 1
        d["items"][o["-s"]] = {"hex": hx.lower(), "mdat": d["clock"]}
        save(d); return 0
    if cmd == "delete-generic-password":
        o = opts(rest, {"-a", "-s"})
        if d["items"].pop(o.get("-s"), None) is None: return 44
        save(d); return 0
    if cmd == "dump-keychain":
        for k in sorted(d["items"]): sys.stdout.write('    "svce"<blob>="%s"\n' % k)
        return 0
    return 1
if sys.argv[1:] == ["-i"]:
    rc = 0
    for line in sys.stdin:
        if line.strip(): rc = run(shlex.split(line)) or rc
    sys.exit(rc)
sys.exit(run(sys.argv[1:]))
PY
chmod +x "$KCBIN/security"
export FAKE_KC="$TMP/kc-9b.json"
printf '{"items": {}, "clock": 0}' > "$FAKE_KC"
printf '{"oauthAccount": {"emailAddress": "old@x.test", "accountUuid": "uuid-old"}}' > "$TMP/home-9b/.claude.json"
PATH="$KCBIN:$PATH" security add-generic-password -U -a "${USER:-claude-code-user}" \
  -s "$KEYCHAIN_SERVICE_BASE" -w '{"claudeAiOauth":{"accessToken":"fake-9b-machine-token-not-real"}}'
LIVE_SECRET_SHA8="$(PATH="$KCBIN:$PATH" security find-generic-password -a "${USER:-claude-code-user}" \
  -s "$KEYCHAIN_SERVICE_BASE" -w 2>/dev/null | tr -d '\n' | shasum -a 256 | cut -c1-8)"
out="$(env PATH="$KCBIN:$PATH" HOME="$TMP/home-9b" CLAUDE_ACCOUNTS_TEST=1 CLAUDE_ACCOUNTS_HEALTH_JSON="$UNFOLDED" \
  CLAUDE_ACCOUNTS_CLAUDE_BIN="$FAKE" CLAUDE_ACCOUNTS_DIR="$LIVEDIR" \
  FAKE_CALLS="$CALLS" "$CA" set-live-login work-2 --force --yes --no-browser 2>&1)"; rc=$?
check "claude was invoked exactly once" "$(n_calls)" "1"
# THE assertion for this command: the child must have CLAUDE_CONFIG_DIR UNSET.
# That is what "the machine login" means, and an empty string is not the same.
check "the login ran with CLAUDE_CONFIG_DIR unset — that IS the machine store" \
  "$(grep '^CLAUDE_CONFIG_DIR=' "$CALLS" | head -1)" "CLAUDE_CONFIG_DIR=<UNSET>"
contains "it is an auth login, with the claude.ai flow pinned" \
  "$(cat "$CALLS")" "ARGV=auth login --claudeai"
lacks "the login never says logout" "$(cat "$CALLS")" "logout"
check "CLAUDE_CODE_OAUTH_TOKEN is unset in the child (upstream #37512)" \
  "$(grep '^CLAUDE_CODE_OAUTH_TOKEN=' "$CALLS" | head -1)" "CLAUDE_CODE_OAUTH_TOKEN=<UNSET>"
check "CLAUDE_SECURESTORAGE_CONFIG_DIR is unset in the child" \
  "$(grep '^CLAUDE_SECURESTORAGE_CONFIG_DIR=' "$CALLS" | head -1)" \
  "CLAUDE_SECURESTORAGE_CONFIG_DIR=<UNSET>"
BK="$(ls -d "$LIVEDIR"/backups/live-login/*/ 2>/dev/null | head -1)"
if [ -n "$BK" ]; then ok "a backup directory was made BEFORE claude ran"
else bad "no backup directory"; fi
if [ -n "$BK" ] && [ -x "$BK/restore" ]; then ok "…with an executable one-command undo in it"
else bad "no executable restore script"; fi
if [ -n "$BK" ] && [ -f "$BK/what-this-is.txt" ]; then
  ok "…and a note saying what it is and how to use it"
else bad "no what-this-is.txt"; fi
contains "the note names the better undo" "$(cat "$BK/what-this-is.txt" 2>/dev/null)" \
  "claude-accounts set-live-login"
# The keychain backup is keychain→keychain: a dormant service name claude can
# never derive (its own suffixes are `-<8 hex>`), so nothing will refresh it.
BK_SVC="$(sed -n 's/^backup keychain   : //p' "$BK/what-this-is.txt" 2>/dev/null | head -1)"
if [ -n "$BK_SVC" ]; then
  case "$BK_SVC" in
    "$KEYCHAIN_SERVICE_BASE.backup."*) ok "the backup service name cannot collide with a slot's" ;;
    *) bad "unexpected backup service name: $BK_SVC" ;;
  esac
  BK_SHA8="$(PATH="$KCBIN:$PATH" security find-generic-password -a "${USER:-claude-code-user}" -s "$BK_SVC" -w 2>/dev/null \
    | tr -d '\n' | shasum -a 256 | cut -c1-8)"
  check "the backup holds the machine login byte-for-byte" "$BK_SHA8" "$LIVE_SECRET_SHA8"
else bad "the backup note did not name a keychain service"; fi
# No credential value anywhere on disk in the backup dir.
if [ -n "$BK" ] && ! grep -rqF "fake-9b-machine-token-not-real" "$BK" 2>/dev/null; then
  ok "no token material was written to the backup directory"
else bad "the backup directory contains token material"; fi
# The fake did not actually change the identity, so the verification must catch
# it and refuse rather than report success. Since R12.5 that verification is the
# SERVER's answer (the identity guard); under test with no fake profile source it
# is unreadable — no secret and no real server are ever asked — and unreadable
# is a refusal (exit 4). The full crossed/matching/unreachable matrix is
# claude-accounts-identity-guard_conformance_test.sh.
if [ "$rc" -ne 0 ]; then ok "a login that could not be verified as the asked-for account fails (rc=$rc)"
else bad "an unverified set-live-login exited 0"; fi
contains "…and says it refused, and what it got" "$out" "REFUSED"
contains "…and never consulted the real server under test" "$out" "test mode: no profile source"
contains "…and names the way out" "$out" "claude-accounts set-live-login work-2"
if [ ! -f "$LIVEDIR/live-login" ]; then
  ok "the live-login pointer is NOT written when the identity did not match"
else bad "a pointer was written for a login that landed the wrong account"; fi
unset FAKE_KC

echo "== 10. delete refuses the machine login and the live account =="
: > "$CALLS"
out="$(run_ca "$CA" delete default </dev/null)"; rc=$?
check "delete default is refused" "$rc" "1"
contains "…because it is not a slot" "$out" "not a slot"
out="$(run_ca "$CA" delete work-2 </dev/null)"; rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -q "machine login is signed in as"; then
  ok "delete refuses the live account without --force"
else bad "delete work-2 (rc=$rc): ${out:0:300}"; fi
contains "…and names the safe way round" "$out" "set-live-login"
contains "…and --force as the deliberate way through" "$out" "--force"
out="$(run_ca "$CA" delete nope </dev/null)"; rc=$?
contains "delete of an unknown account is refused" "$out" "there is no account"
# The confirmation is a real gate: an empty answer deletes nothing.
out="$(run_ca "$CA" delete half </dev/null)"; rc=$?
check "delete without the typed confirmation does nothing" "$rc" "1"
contains "…and says so" "$out" "nothing deleted"
contains "delete states that it revokes nothing" "$out" "Nothing is revoked"
check "no delete path invoked claude" "$(n_calls)" "0"

echo "== 11. rename and delete move the credential with the slot =="
# The sharp edge: the keychain service name is derived from the slot's PATH, so
# a rename that only edited the registry would orphan a live grant. Proven here
# with a REAL keychain item — a dummy secret under a service name derived from a
# temp path, which no `claude` will ever look up.
RHOME="$TMP/rename-home"
mkdir -p "$RHOME/accounts/work"
KC_ACCT="${USER:-claude-code-user}"
svc_for() { printf '%s-%s' "$KEYCHAIN_SERVICE_BASE" "$(printf %s "$1" | shasum -a 256 | cut -c1-8)"; }
OLD_SVC="$(svc_for "$RHOME/accounts/work")"
NEW_SVC="$(svc_for "$RHOME/accounts/work-renamed")"
cat > "$RHOME/accounts/work/manifest.toml" <<EOF
label = "work"
captured_at = "2026-07-30T00:00:00Z"
source_config_dir = "$RHOME/accounts/work"
source_config_json = "$RHOME/accounts/work/.claude.json"
spawn_env = "CLAUDE_CONFIG_DIR=$RHOME/accounts/work"
credential_mode = "own-login"
keychain_service = "$OLD_SVC"
keychain_account = "$KC_ACCT"
EOF
cat > "$RHOME/accounts.toml" <<EOF
[accounts.default]

[accounts.work]
config_dir = "$RHOME/accounts/work"
EOF
if [ -n "$THREADS_BIN" ]; then
  security add-generic-password -U -a "$KC_ACCT" -s "$OLD_SVC" -w "conformance-dummy-not-a-token" 2>/dev/null
  out="$(THREADS_HOME="$RHOME" "$THREADS_BIN" accounts rename work work-renamed 2>&1)"; rc=$?
  check "rename exits 0" "$rc" "0"
  if [ -d "$RHOME/accounts/work-renamed" ] && [ ! -e "$RHOME/accounts/work" ]; then
    ok "the slot directory moved"
  else bad "the slot directory did not move: ${out:0:200}"; fi
  if security find-generic-password -a "$KC_ACCT" -s "$NEW_SVC" >/dev/null 2>&1; then
    ok "the credential now lives under the service name derived from the NEW path"
  else bad "no keychain item at the new service name"; fi
  if security find-generic-password -a "$KC_ACCT" -s "$OLD_SVC" >/dev/null 2>&1; then
    bad "the OLD keychain item is still there — one token in two stores"
  else ok "the old keychain item is gone (no clone left behind)"; fi
  check "the moved secret is the same secret" \
    "$(security find-generic-password -a "$KC_ACCT" -s "$NEW_SVC" -w 2>/dev/null)" \
    "conformance-dummy-not-a-token"
  contains "the manifest follows the rename" \
    "$(cat "$RHOME/accounts/work-renamed/manifest.toml")" 'label = "work-renamed"'
  contains "…including the keychain service it declares" \
    "$(cat "$RHOME/accounts/work-renamed/manifest.toml")" "$NEW_SVC"
  contains "the registry follows the rename" "$(cat "$RHOME/accounts.toml")" "accounts.work-renamed"
  lacks "…and drops the old name" "$(cat "$RHOME/accounts.toml")" "accounts.work]"

  # And delete takes the keychain item with it, leaving nothing behind.
  out="$(THREADS_HOME="$RHOME" "$THREADS_BIN" accounts delete work-renamed 2>&1)"; rc=$?
  check "delete exits 0" "$rc" "0"
  if security find-generic-password -a "$KC_ACCT" -s "$NEW_SVC" >/dev/null 2>&1; then
    bad "delete left the keychain item behind"
    security delete-generic-password -a "$KC_ACCT" -s "$NEW_SVC" >/dev/null 2>&1 || true
  else ok "delete removed the keychain item too"; fi
  if [ ! -e "$RHOME/accounts/work-renamed" ]; then ok "delete removed the slot directory"
  else bad "delete left the slot directory"; fi
  lacks "delete removed the registry entry" "$(cat "$RHOME/accounts.toml")" "work-renamed"
  contains "delete says it revoked nothing" "$out" "revoked"
  # Belt and braces: neither verb may ever name the machine-default service.
  lacks "rename never names the machine-default keychain service" \
    "$out" "s '$KEYCHAIN_SERVICE_BASE'"
else
  bad "no threads binary built — cannot exercise rename/delete"
fi
# Whatever happened above, leave no dummy items in the operator's keychain.
security delete-generic-password -a "$KC_ACCT" -s "$OLD_SVC" >/dev/null 2>&1 || true
security delete-generic-password -a "$KC_ACCT" -s "$NEW_SVC" >/dev/null 2>&1 || true

echo "== 11b. the script finds threads where cauldron builds it, and never builds in-repo (R3.S2.9) =="
# The script is COPIED into a fake repo with no target/, so REPO_ROOT has no
# in-repo build and the only way rename can reach a threads is through the new
# candidates. No CLAUDE_ACCOUNTS_THREADS_BIN seam anywhere below: the LOOKUP
# itself is under test. CLAUDE_ACCOUNTS_TEST=1 stays on only so the state dir
# is a temp dir and cauldron comes from a stub, never the operator's real one.
FREPO="$TMP/fakerepo"
mkdir -p "$FREPO/services/threads/scripts"
cp "$CA" "$FREPO/services/threads/scripts/claude-accounts"
FCA="$FREPO/services/threads/scripts/claude-accounts"
LK="$TMP/lookup"; mkdir -p "$LK/state" "$LK/home" "$LK/bin"
mk_stub_threads() {  # mk_stub_threads <path> <tag>
  mkdir -p "$(dirname "$1")"
  cat > "$1" <<STUB
#!/usr/bin/env bash
printf '%s %s\n' "$2" "\$*" >> "$LK/threads-calls.log"
exit 0
STUB
  chmod +x "$1"
}
run_lookup() {  # run_lookup <env assignments...> -- via env
  : > "$LK/threads-calls.log"
  env -u CARGO_TARGET_DIR -u CLAUDE_ACCOUNTS_THREADS_BIN -u CLAUDE_ACCOUNTS_CAULDRON_BIN \
      CLAUDE_ACCOUNTS_TEST=1 CLAUDE_ACCOUNTS_STATE_DIR="$LK/state" \
      CLAUDE_ACCOUNTS_DIR="$LK/state/accounts" \
      "$@" 2>&1
}
if [ -e "$FREPO/target" ]; then bad "fake repo unexpectedly has a target/"; else ok "fake repo has no target/ (sentinel)"; fi

# Negative counterpart first: nothing built anywhere → "not built". Proves the
# fake repo cannot reach some other threads by accident.
out="$(run_lookup CARGO_TARGET_DIR="$LK/ctd-empty" "$FCA" rename aa bb)"; rc=$?
check "no build anywhere: rename refuses" "$rc" "1"
contains "…and says threads is not built" "$out" "not built"

# $CARGO_TARGET_DIR candidate: a stub threads in a temp target dir is reached.
mk_stub_threads "$LK/ctd/debug/threads" CTD
out="$(run_lookup CARGO_TARGET_DIR="$LK/ctd" "$FCA" rename aa bb)"; rc=$?
check "CARGO_TARGET_DIR stub: rename exits 0" "$rc" "0"
check "rename reached the CARGO_TARGET_DIR threads with its args" \
  "$(cat "$LK/threads-calls.log")" "CTD accounts rename aa bb"
# release beats debug in the same target dir
mk_stub_threads "$LK/ctd/release/threads" CTDREL
out="$(run_lookup CARGO_TARGET_DIR="$LK/ctd" "$FCA" rename aa bb)"; rc=$?
check "release is preferred over debug" "$(cat "$LK/threads-calls.log")" "CTDREL accounts rename aa bb"

# Cauldron-pinned candidate: a stub `cauldron status --json threads` names a
# root and a version; the image in that build dir wins over CARGO_TARGET_DIR.
CROOT="$LK/croot"
mk_stub_threads "$CROOT/data/shared/builds/threads/abc123-release/threads" CAULDRON
mk_cauldron() {  # mk_cauldron <running> <pinned>   (json literals)
  cat > "$LK/bin/cauldron" <<STUB
#!/usr/bin/env bash
[ "\$1 \$2 \$3" = "status --json threads" ] || exit 9
printf '%s' '{"environment":{"root":"$CROOT","name":"prod"},"services":[{"service":"threads","running_version":$1,"pinned":$2}]}'
STUB
  chmod +x "$LK/bin/cauldron"
}
mk_cauldron null '"abc123-release"'
out="$(run_lookup CARGO_TARGET_DIR="$LK/ctd" CLAUDE_ACCOUNTS_CAULDRON_BIN="$LK/bin/cauldron" "$FCA" rename aa bb)"; rc=$?
check "cauldron-pinned build: rename exits 0" "$rc" "0"
check "the cauldron-pinned build wins over CARGO_TARGET_DIR" \
  "$(cat "$LK/threads-calls.log")" "CAULDRON accounts rename aa bb"
out="$(run_lookup CARGO_TARGET_DIR="$LK/ctd" CLAUDE_ACCOUNTS_CAULDRON_BIN="$LK/bin/cauldron" "$FCA" delete aa --yes)"; rc=$?
# delete reads the inventory first through the same lookup: the stub prints no
# JSON, so it stops at the inventory — but it asked the CAULDRON image for it.
contains "delete's inventory read goes to the cauldron-pinned build" \
  "$(cat "$LK/threads-calls.log")" "CAULDRON accounts health --json"
# Counterpart: a `latest` pin names no concrete build → falls through.
mk_cauldron null '"latest"'
out="$(run_lookup CARGO_TARGET_DIR="$LK/ctd" CLAUDE_ACCOUNTS_CAULDRON_BIN="$LK/bin/cauldron" "$FCA" rename aa bb)"; rc=$?
check "a 'latest' pin falls through to CARGO_TARGET_DIR" "$(cat "$LK/threads-calls.log")" "CTDREL accounts rename aa bb"
# running_version beats pinned (a pin not yet cut over)
mk_stub_threads "$CROOT/data/shared/builds/threads/run999-release/threads" RUNNING
mk_cauldron '"run999-release"' '"abc123-release"'
out="$(run_lookup CARGO_TARGET_DIR="$LK/ctd" CLAUDE_ACCOUNTS_CAULDRON_BIN="$LK/bin/cauldron" "$FCA" rename aa bb)"; rc=$?
check "the running version beats the pin" "$(cat "$LK/threads-calls.log")" "RUNNING accounts rename aa bb"
# Under test, cauldron is NEVER taken from PATH: a stub on PATH is ignored.
out="$(run_lookup PATH="$LK/bin:$PATH" CARGO_TARGET_DIR="$LK/ctd" "$FCA" rename aa bb)"; rc=$?
check "under test, a cauldron on PATH is not consulted" "$(cat "$LK/threads-calls.log")" "CTDREL accounts rename aa bb"

# ── the cargo fallback refuses an unset / relative / in-repo target dir ──────
# A stub `cargo` on PATH records whether it was run; HOME is a temp dir so the
# script's "$HOME/.cargo/bin" prefix cannot put the real cargo in front of it.
cat > "$LK/bin/cargo" <<STUB
#!/usr/bin/env bash
printf 'CARGO CARGO_TARGET_DIR=%s %s\n' "\${CARGO_TARGET_DIR-<UNSET>}" "\$*" >> "$LK/cargo-calls.log"
exit 0
STUB
chmod +x "$LK/bin/cargo"
rm -f "$LK/bin/cauldron"
cargo_case() {  # cargo_case <desc> <want: refused|ran> [CARGO_TARGET_DIR value or <UNSET>]
  local desc="$1" want="$2" ctd="${3-<UNSET>}"
  : > "$LK/cargo-calls.log"
  if [ "$ctd" = "<UNSET>" ]; then
    out="$(cd "$FREPO" && run_lookup HOME="$LK/home" PATH="$LK/bin:$PATH" "$FCA" status --json)"; rc=$?
  else
    out="$(cd "$FREPO" && run_lookup HOME="$LK/home" PATH="$LK/bin:$PATH" CARGO_TARGET_DIR="$ctd" "$FCA" status --json)"; rc=$?
  fi
  local ncargo; ncargo="$(awk '/^CARGO /{n++} END{printf "%d", n+0}' "$LK/cargo-calls.log")"
  if [ "$want" = refused ]; then
    check "$desc: no cargo ran" "$ncargo" "0"
    contains "$desc: refusal is loud" "$out" "REFUSED to cargo-build threads"
  else
    check "$desc: cargo ran once" "$ncargo" "1"
    lacks "$desc: no refusal" "$out" "REFUSED"
  fi
}
cargo_case "CARGO_TARGET_DIR unset" refused
cargo_case "CARGO_TARGET_DIR empty" refused ""
cargo_case "CARGO_TARGET_DIR relative" refused "target"
cargo_case "CARGO_TARGET_DIR = repo/target" refused "$FREPO/target"
cargo_case "CARGO_TARGET_DIR inside repo subdir" refused "$FREPO/services/threads/x"
# Positive counterpart: an absolute dir outside the repo with no threads in it
# is allowed to reach cargo (the stub), so the guard is not a constant refusal.
cargo_case "CARGO_TARGET_DIR absolute, outside repo" ran "$LK/ctd-empty"
if [ -e "$FREPO/target" ]; then bad "something created $FREPO/target"; else ok "no in-repo target/ was created"; fi

echo "== 12. nothing was written outside the temp working dir =="
if [ ! -e "$THREADS_DIR/accounts/.rotate.lock" ]; then
  ok "the real accounts dir has no leftover rotation lock"
else bad "a rotation lock was left in $THREADS_DIR/accounts"; fi
if [ -f "$TMP/rotation.log" ]; then ok "the rotation log went to the temp dir"
else bad "no rotation log in the temp dir"; fi

echo "== 13. the live login is exactly as we found it =="
check "live keychain mdat unchanged" "$(live_mdat)" "$LIVE_MDAT_BEFORE"

printf '\n%s\n' "claude-accounts conformance: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
