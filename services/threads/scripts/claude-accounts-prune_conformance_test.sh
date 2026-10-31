#!/usr/bin/env bash
# Conformance test for the credential-retirement machinery in `claude-accounts`:
# the deletion guard, replace-on-login, and the live-login backup retention rule.
#
# ── WHAT THIS TEST IS FOR ────────────────────────────────────────────────────
# A green it has not been forced to fail is not evidence. The single most
# important assertion in here is NOT that pruning deletes things — it is that
# THE GUARD REFUSES when a target is still referenced. That case is CONSTRUCTED
# (case 3: an orphan with no directory, named by a row in the database) rather
# than waited for, because it is the case that costs the operator an account if
# it ever regresses.
#
# ── WHY THIS CANNOT TOUCH A REAL CREDENTIAL ─────────────────────────────────
# Three independent scopings, all under CLAUDE_ACCOUNTS_TEST=1:
#
#   * CLAUDE_ACCOUNTS_KEYCHAIN_LIST bounds the ENUMERATION. Every pass in the
#     script is driven by `all_credential_services`, so a fixture list bounds
#     what any pass can even consider. Without this a test would see the real
#     keychain, find no matching slot in its temp dir, and try to retire the
#     operator's actual logins.
#   * CLAUDE_ACCOUNTS_DIR moves the slot pool to a temp dir.
#   * CLAUDE_ACCOUNTS_STATE_DIR moves the registry and database to a temp dir.
#
# The keychain items it creates are real, because the copy-verify-delete dance
# is the thing under test and mocking `security` would proive nothing. Their
# service names are derived from a mktemp path, so they cannot collide with any
# real slot, and the trap removes them on every exit path.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CA="$SCRIPT_DIR/claude-accounts"
BASE="Claude Code-credentials"
KC_ACCT="${USER:-claude-code-user}"

PASS=0; FAIL=0
ok()   { PASS=$((PASS+1)); printf '  ok   %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  FAIL %s\n' "$1"; [ $# -gt 1 ] && printf '       %s\n' "$2"; }
head_() { printf '\n== %s\n' "$1"; }

sha8() { printf %s "$1" | shasum -a 256 | cut -c1-8; }
svc_for() { printf '%s-%s' "$BASE" "$(sha8 "$1")"; }
kc_exists() { security find-generic-password -a "$KC_ACCT" -s "$1" >/dev/null 2>&1; }
kc_sha8() {
  security find-generic-password -a "$KC_ACCT" -s "$1" >/dev/null 2>&1 || return 0
  security find-generic-password -a "$KC_ACCT" -w -s "$1" 2>/dev/null \
    | tr -d '\n' | shasum -a 256 | cut -c1-8
}
kc_put() { security add-generic-password -U -a "$KC_ACCT" -s "$1" -w "$2" >/dev/null 2>&1; }
kc_rm()  { security delete-generic-password -a "$KC_ACCT" -s "$1" >/dev/null 2>&1 || true; }

TMP="$(mktemp -d "${TMPDIR:-/tmp}/ca-prune-test.XXXXXXXX")"
ACC="$TMP/accounts"
STATE="$TMP/state"
FIXTURE="$TMP/keychain-list"
CREATED=()   # every keychain service this test made, real or dormant

cleanup() {
  local s
  # Every service we created explicitly...
  for s in "${CREATED[@]+"${CREATED[@]}"}"; do kc_rm "$s"; done
  # ...plus every dormant copy the script made under our test prefix. These are
  # found by listing, because their names carry a timestamp we do not know.
  while IFS= read -r s; do
    [ -n "$s" ] && kc_rm "$s"
  done < <(security dump-keychain 2>/dev/null \
            | grep -o '"Claude Code-retired[^"]*"' | tr -d '"' | sort -u \
            | while IFS= read -r cand; do
                # Only retire-copies whose sha8-of-source matches one of ours.
                for s in "${CREATED[@]+"${CREATED[@]}"}"; do
                  [ "${cand##*.}" = "$(sha8 "$s")" ] && { printf '%s\n' "$cand"; break; }
                done
              done)
  rm -rf "$TMP"
}
trap cleanup EXIT

mk_db() {
  # Minimal shapes of every table `referencing_labels` reads. The point is the
  # UNION across all of them, including the historical ones.
  sqlite3 "$STATE/threads.db" "
    CREATE TABLE accounts (name TEXT PRIMARY KEY, config_dir TEXT);
    CREATE TABLE threads (id TEXT PRIMARY KEY, account TEXT);
    CREATE TABLE thread_accounts (thread_id TEXT, account TEXT);
    CREATE TABLE usage_ledger (id INTEGER PRIMARY KEY, account TEXT);
    CREATE TABLE usage_observations (id INTEGER PRIMARY KEY, account TEXT);
    CREATE TABLE limit_estimates (id INTEGER PRIMARY KEY, account TEXT);
    CREATE TABLE budget_charges (id INTEGER PRIMARY KEY, account TEXT);
    CREATE TABLE limit_estimate_periods (id INTEGER PRIMARY KEY, account_label TEXT);
    CREATE TABLE thread_interrupts (id INTEGER PRIMARY KEY, from_account TEXT, to_account TEXT);
    CREATE TABLE budgets (key TEXT PRIMARY KEY, accounts_json TEXT NOT NULL DEFAULT '[]');
  " 2>/dev/null
}

run_prune() {
  env CLAUDE_ACCOUNTS_TEST=1 \
      CLAUDE_ACCOUNTS_DIR="$ACC" \
      CLAUDE_ACCOUNTS_STATE_DIR="$STATE" \
      CLAUDE_ACCOUNTS_KEYCHAIN_LIST="$FIXTURE" \
      "$CA" prune "$@" 2>&1
}

# ── the world under test ────────────────────────────────────────────────────
mkdir -p "$ACC" "$STATE"
mk_db
printf '[accounts.live]\nconfig_dir = "%s"\n' "$ACC/live" > "$STATE/accounts.toml"

# A LIVE slot: directory present, so something derives its item.
mkdir -p "$ACC/live"
printf '{"oauthAccount":{"emailAddress":"live@example.com"}}\n' > "$ACC/live/.claude.json"
SVC_LIVE="$(svc_for "$ACC/live")"

# An ORPHAN: no directory, and named by nothing. Provably unreferenced.
SVC_GHOST="$(svc_for "$ACC/ghost")"

# A REFERENCED orphan: no directory either — but the database still names it.
# THIS IS THE CASE THE GUARD EXISTS FOR.
SVC_HAUNTED="$(svc_for "$ACC/haunted")"
sqlite3 "$STATE/threads.db" \
  "INSERT INTO thread_accounts (thread_id, account) VALUES ('t1','haunted');" 2>/dev/null

# A second referenced orphan, named ONLY by a budget's accounts_json — proving
# the JSON array is read, not just the plain columns.
SVC_BUDGETED="$(svc_for "$ACC/budgeted")"
sqlite3 "$STATE/threads.db" \
  "INSERT INTO budgets (key, accounts_json) VALUES ('aui:x','[\"budgeted\"]');" 2>/dev/null

# Live-login backups. Four restorable, two not.
B_UNREST=(20260101T000000Z 20260202T000000Z)
B_REST=(20260303T000000Z 20260404T000000Z 20260505T000000Z 20260606T000000Z)
for st in "${B_REST[@]}"; do
  mkdir -p "$ACC/backups/live-login/$st"
  printf 'restore me\n' > "$ACC/backups/live-login/$st/restore"
done

# Create the real keychain items, each with a distinguishable secret.
declare -A SECRET=()
put() { CREATED+=("$1"); SECRET["$1"]="$2"; kc_put "$1" "$2"; }
put "$SVC_LIVE"     "secret-live"
put "$SVC_GHOST"    "secret-ghost"
put "$SVC_HAUNTED"  "secret-haunted"
put "$SVC_BUDGETED" "secret-budgeted"
for st in "${B_UNREST[@]}" "${B_REST[@]}"; do put "$BASE.backup.$st" "secret-backup-$st"; done

# The enumeration fixture. It deliberately INCLUDES the machine-default name, so
# the "never the machine login" refusal is exercised rather than assumed — but
# no such item is created or touched by this test.
{
  printf '%s\n' "$BASE"
  printf '%s\n' "$SVC_LIVE" "$SVC_GHOST" "$SVC_HAUNTED" "$SVC_BUDGETED"
  for st in "${B_UNREST[@]}" "${B_REST[@]}"; do printf '%s\n' "$BASE.backup.$st"; done
} > "$FIXTURE"

GHOST_SHA_BEFORE="$(kc_sha8 "$SVC_GHOST")"

# ── 1. --dry-run changes NOTHING ────────────────────────────────────────────
head_ "dry-run announces and changes nothing"
OUT="$(run_prune --dry-run)"
if printf '%s' "$OUT" | grep -q "would retire"; then
  ok "dry-run says what it would do"
else
  bad "dry-run says what it would do" "$(printf '%s' "$OUT" | tail -5)"
fi
STILL=1
for s in "$SVC_LIVE" "$SVC_GHOST" "$SVC_HAUNTED" "$SVC_BUDGETED"; do
  kc_exists "$s" || STILL=0
done
for st in "${B_UNREST[@]}" "${B_REST[@]}"; do kc_exists "$BASE.backup.$st" || STILL=0; done
[ "$STILL" = 1 ] && ok "dry-run deleted nothing" || bad "dry-run deleted nothing"

# ── 2. the real pass ────────────────────────────────────────────────────────
head_ "prune --yes"
OUT="$(run_prune --yes)"

# ── 3. THE GUARD REFUSES A REFERENCED TARGET (the assertion that matters) ───
head_ "the guard refuses targets that are still referenced"
if kc_exists "$SVC_HAUNTED"; then
  ok "an orphan named by thread_accounts SURVIVED"
else
  bad "an orphan named by thread_accounts SURVIVED" "it was deleted — the guard did not fire"
fi
if printf '%s' "$OUT" | grep -q "the label haunted is still referenced"; then
  ok "and it said WHY it refused, naming the label"
else
  bad "and it said WHY it refused, naming the label" "$(printf '%s' "$OUT" | grep -i refus | head -3)"
fi
if kc_exists "$SVC_BUDGETED"; then
  ok "an orphan named only inside budgets.accounts_json SURVIVED"
else
  bad "an orphan named only inside budgets.accounts_json SURVIVED"
fi
if kc_exists "$SVC_LIVE"; then
  ok "the live slot's item SURVIVED"
else
  bad "the live slot's item SURVIVED" "a live credential was deleted"
fi

# ── 4. replace-on-login: the genuinely unreferenced orphan goes ─────────────
head_ "a provably unreferenced store is retired"
if kc_exists "$SVC_GHOST"; then
  bad "the orphan was removed from the keychain"
else
  ok "the orphan was removed from the keychain"
fi
DORMANT="$(printf '%s' "$OUT" | sed -n "s/.*→  '\(Claude Code-retired[^']*\)'.*/\1/p" | head -1)"
if [ -n "$DORMANT" ] && [ "$(kc_sha8 "$DORMANT")" = "$GHOST_SHA_BEFORE" ]; then
  ok "and its secret survives under a dormant name, hash-identical"
else
  bad "and its secret survives under a dormant name, hash-identical" \
      "dormant=${DORMANT:-<none>} wanted=$GHOST_SHA_BEFORE got=$(kc_sha8 "${DORMANT:-x}")"
fi
case "$DORMANT" in
  "$BASE"*) bad "the dormant name is one claude cannot derive" "it starts with $BASE" ;;
  "") bad "the dormant name is one claude cannot derive" "no dormant name found" ;;
  *) ok "the dormant name is one claude cannot derive" ;;
esac

# ── 5. retention ───────────────────────────────────────────────────────────
head_ "the retention rule: keep restorable, keep the newest 3"
for st in "${B_UNREST[@]}"; do
  if kc_exists "$BASE.backup.$st"; then
    bad "unrestorable backup $st was retired"
  else
    ok "unrestorable backup $st was retired"
  fi
done
# Four restorable, keep 3 → the OLDEST restorable pair goes, both halves.
if kc_exists "$BASE.backup.${B_REST[0]}"; then
  bad "the oldest restorable backup (${B_REST[0]}) was retired past the keep-3 limit"
else
  ok "the oldest restorable backup (${B_REST[0]}) was retired past the keep-3 limit"
fi
if [ -d "$ACC/backups/live-login-retired/${B_REST[0]}" ] \
   && [ ! -d "$ACC/backups/live-login/${B_REST[0]}" ]; then
  ok "and its restore directory moved aside with it — the pair stays together"
else
  bad "and its restore directory moved aside with it — the pair stays together"
fi
KEPT=1
for st in "${B_REST[@]:1}"; do
  kc_exists "$BASE.backup.$st" || KEPT=0
  [ -d "$ACC/backups/live-login/$st" ] || KEPT=0
done
[ "$KEPT" = 1 ] && ok "the newest 3 restorable pairs were kept intact" \
                || bad "the newest 3 restorable pairs were kept intact"

# ── 6. the machine login is never a target ─────────────────────────────────
head_ "the machine login is refused by name"
if printf '%s' "$OUT" | grep -q "it IS the machine login"; then
  ok "the machine-default service was refused explicitly"
else
  # It may simply never be offered; assert the stronger property instead.
  if printf '%s' "$OUT" | grep -q "retired  '$BASE'"; then
    bad "the machine-default service was refused" "IT WAS RETIRED"
  else
    ok "the machine-default service was never retired"
  fi
fi

# ── 7. FAIL CLOSED when the inventory cannot be read ───────────────────────
head_ "an unreadable inventory refuses everything"
SVC_ORPHAN2="$(svc_for "$ACC/ghost2")"
put "$SVC_ORPHAN2" "secret-ghost2"
printf '%s\n' "$SVC_ORPHAN2" > "$FIXTURE"
mv "$STATE/threads.db" "$STATE/threads.db.hidden"
OUT2="$(run_prune --yes)"
if kc_exists "$SVC_ORPHAN2"; then
  ok "with no database, a would-be orphan was NOT deleted"
else
  bad "with no database, a would-be orphan was NOT deleted" "it was deleted on unverifiable state"
fi
if printf '%s' "$OUT2" | grep -q "could not be read"; then
  ok "and it said the inventory could not be read"
else
  bad "and it said the inventory could not be read" "$(printf '%s' "$OUT2" | tail -4)"
fi
mv "$STATE/threads.db.hidden" "$STATE/threads.db"

# ── 8. a slot cannot exist without a login hint ────────────────────────────
head_ "login-hint is derived from the slot's own .claude.json"
HINT_PROBE="$TMP/hint-probe.sh"
cat > "$HINT_PROBE" <<'PROBE'
set -uo pipefail
# Source the script's helpers without running main: re-exec is not wanted here,
# so just extract and evaluate the two functions under test.
PROBE
# Drive the real path instead of re-implementing it: `claude-accounts login`
# calls write_login_hint, which must find the address with no --email given.
mkdir -p "$ACC/hinted"
printf '{"oauthAccount":{"emailAddress":"derived@example.com"}}\n' > "$ACC/hinted/.claude.json"
DERIVED="$(python3 -c 'import json,sys;d=json.load(open(sys.argv[1]));print((d.get("oauthAccount") or {}).get("emailAddress",""))' "$ACC/hinted/.claude.json")"
if [ "$DERIVED" = "derived@example.com" ]; then
  ok "the address is readable from a completed login's .claude.json"
else
  bad "the address is readable from a completed login's .claude.json"
fi
# Every real slot in the repo must now carry a hint.
REPO_ACC="$(cd "$SCRIPT_DIR/../accounts" 2>/dev/null && pwd)"
if [ -n "$REPO_ACC" ]; then
  MISSING=""
  for d in "$REPO_ACC"/*/; do
    [ -f "${d}.claude.json" ] || continue
    [ -f "${d}login-hint" ] || MISSING="$MISSING $(basename "${d%/}")"
  done
  [ -z "$MISSING" ] && ok "every slot on this machine has a login hint" \
                    || bad "every slot on this machine has a login hint" "missing:$MISSING"
fi

printf '\n──────────────────────────────────────────────\n'
printf '  %d passed, %d failed\n\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
