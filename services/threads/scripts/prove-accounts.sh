#!/usr/bin/env bash
# prove-accounts.sh — prove that each CAPTURED claude login can actually run a
# turn, without ever touching the operator's live session.
#
# Operator ask (msg #690): "we should also test that both the accounts actually
# work, even though I'm only signed in manually to one of them right now."
#
# WHAT IT DOES, per account, strictly one at a time:
#
#   1. fingerprints the identities (sha256 prefixes only — never a credential,
#      never an email, never a uuid) so the report can say WHICH captured
#      account the operator's live login currently is;
#   2. stands up a THROWAWAY threads daemon under $TMPDIR with its own port and
#      its own data dir, whose account registry contains NOTHING but that one
#      captured snapshot dir;
#   3. runs exactly ONE trivial haiku turn through it via `threads-stress`
#      (which holds an exclusive lock and refuses if anything else is active);
#   4. reads the isolated usage ledger back to confirm which account served the
#      turn, and classifies a 429 / usage limit as "the login works" — a clean
#      rate-limit observation still proves the credential authenticated;
#   5. tears the daemon down and deletes the scratch dir.
#
# ISOLATION RULES, absolute (each one is enforced below, not just documented):
#
#   - Data dir is always under $TMPDIR. `~/.threads` is NEVER created, read for
#     state, or written; the only thing this script does with that path is
#     CHECK whether a global stress lock is sitting there.
#   - Own port, verified free first. NEVER connects to the default 3653 (the
#     live daemon's only port since R2.S4 retired the legacy door) — refuses
#     outright if asked to.
#   - Exactly one execution at a time: runs the accounts sequentially, one
#     daemon at a time, and refuses to start if another threads daemon,
#     threads-stress, or stress lock is present.
#   - The operator's live login is never switched, re-authed or invalidated.
#     `default` in the throwaway registry is deliberately REMAPPED onto the same
#     captured snapshot dir, so even a failover has nowhere to go except the
#     account under test — the machine-default login (CLAUDE_CONFIG_DIR unset)
#     is unreachable from this daemon by construction.
#   - The machine-default keychain item's modification timestamp and
#     `~/.claude.json`'s size/mtime are recorded BEFORE and AFTER and reported;
#     any change is a loud failure of the isolation claim.
#
# WHAT IT NECESSARILY DOES WRITE: running a real turn under a captured config
# dir makes claude write its own session state INSIDE that snapshot dir
# (`projects/`, `sessions/`, and an updated `.claude.json`). That is inherent to
# spawning the login and is confined to `lib/threads/accounts/<label>/`, which
# is gitignored. Sizes/hashes of `.claude.json` are reported before and after.
#
#   scripts/prove-accounts.sh                    # dry run: preflight + fingerprints only
#   scripts/prove-accounts.sh --live             # actually spend (two haiku turns, cents)
#   scripts/prove-accounts.sh --live --accounts primary
#
# Exit codes: 0 ran · 2 bad usage · 3 REFUSED (preflight) · 1 error.
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

DEFAULT_ACCOUNTS="primary,secondary-live"
LIVE=0
KEEP=0
BASE_PORT=3671
MODEL="haiku"
TEXT="Reply with exactly one word: ok"
TIMEOUT_SECS=180
ACCOUNTS="$DEFAULT_ACCOUNTS"

usage() {
  sed -n '2,60p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
  exit "${1:-2}"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --live) LIVE=1; shift ;;
    --keep) KEEP=1; shift ;;
    --accounts) ACCOUNTS="${2:-}"; shift 2 ;;
    --model) MODEL="${2:-}"; shift 2 ;;
    --text) TEXT="${2:-}"; shift 2 ;;
    --port) BASE_PORT="${2:-}"; shift 2 ;;
    --timeout-secs) TIMEOUT_SECS="${2:-}"; shift 2 ;;
    -h|--help) usage 0 ;;
    *) echo "prove-accounts.sh: unknown flag $1" >&2; usage 2 ;;
  esac
done

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
threads_dir="$(cd "$here/.." && pwd)"
repo="$(cd "$threads_dir/../.." && pwd)"
accounts_root="$threads_dir/accounts"
tmp_root="${TMPDIR:-/tmp}"

refuse() { echo "prove-accounts.sh: REFUSED — $*" >&2; exit 3; }
die()    { echo "prove-accounts.sh: $*" >&2; exit 1; }
note()   { echo "prove-accounts.sh: $*" >&2; }

# ── preflight: nothing else may be running ───────────────────────────────────
[[ "$(uname -s)" == "Darwin" ]] || refuse "this machine is not macOS (keychain-mode capture)"
command -v claude   >/dev/null || refuse "no \`claude\` on PATH"
command -v sqlite3  >/dev/null || refuse "no \`sqlite3\` on PATH (needed to read the ledger back)"
command -v python3  >/dev/null || refuse "no \`python3\` on PATH"

[[ "$BASE_PORT" == "3653" ]] && refuse "port 3653 is the live daemon's port — pick another"

if [[ -e "$HOME/.threads/stress.lock" ]]; then
  refuse "a global stress lock exists at ~/.threads/stress.lock — another live run is in flight"
fi
if pgrep -f 'threads-stress' >/dev/null 2>&1; then
  refuse "a threads-stress process is already running"
fi
if pgrep -f '/threads --config' >/dev/null 2>&1; then
  refuse "a threads daemon is already running"
fi
if lsof -nP -iTCP -sTCP:LISTEN 2>/dev/null | grep -qE "127\.0\.0\.1:${BASE_PORT}\b"; then
  refuse "port $BASE_PORT is already listening"
fi

IFS=',' read -r -a want <<< "$ACCOUNTS"
for label in "${want[@]}"; do
  [[ -f "$accounts_root/$label/manifest.toml" ]] \
    || refuse "no captured snapshot at accounts/$label/ (manifest.toml missing)"
  mode="$(sed -n 's/^credential_mode *= *"\(.*\)"/\1/p' "$accounts_root/$label/manifest.toml")"
  [[ "$mode" == "manual-login-required" ]] \
    && refuse "accounts/$label is credential-less ($mode) — nothing to prove, log in under it first"
done

# ── the witness: the operator's live login state, before ──────────────────────
# The item's modification timestamp, as the raw hex `security` prints. Metadata
# only — the secret is never read.
keychain_mdat() {
  security find-generic-password -a "${USER}" -s "$1" 2>&1 |
    sed -n 's/.*"mdat"<timedate>=0x\([0-9A-F]*\).*/\1/p' | head -1
}
file_witness() { # path -> "<bytes>:<mtime-epoch>:<sha256[..12]>"
  if [[ -f "$1" ]]; then
    printf '%s:%s:%s' \
      "$(stat -f %z "$1")" "$(stat -f %m "$1")" \
      "$(shasum -a 256 "$1" | cut -c1-12)"
  else
    printf 'absent'
  fi
}

# ── the rotation hazard (learned the hard way, see the report) ───────────────
#
# `capture_snapshot`'s keychain clone copies the CURRENT OAuth token bytes into
# a new service name. That does NOT mint an independent login: both entries then
# hold the SAME accessToken/refreshToken pair, and Claude Code rotates the
# refresh token whenever it refreshes. Whichever copy refreshes first leaves the
# other holding a refresh token the server no longer accepts — at which point
# claude wipes the stale entry and reports "OAuth session expired and could not
# be refreshed".
#
# Two consequences this function exists to enforce:
#   - a clone that still SHARES the live login's bytes and whose access token is
#     ALREADY EXPIRED must not be spawned: the spawn would force a refresh, and
#     the rotation could invalidate the OPERATOR's live session. Refuse.
#   - a clone that shares the bytes but whose access token is still valid is safe
#     to spawn right now (no refresh, so no rotation).
#
# Reads the secret ONLY to hash it and to read `expiresAt`; the value never
# reaches stdout, argv, a file, or a log.
token_probe() { # service -> "<sha256[..12]> <seconds_until_expiry|unknown>"
  security find-generic-password -a "${USER}" -s "$1" -w 2>/dev/null |
    python3 -c '
import sys, json, hashlib, time
raw = sys.stdin.buffer.read()
if not raw:
    print("absent unknown"); raise SystemExit
print(hashlib.sha256(raw).hexdigest()[:12], end=" ")
try:
    o = json.loads(raw)
    o = o.get("claudeAiOauth") or o
    ea = o.get("expiresAt")
    ea = ea / 1000 if isinstance(ea, (int, float)) and ea > 1e11 else ea
    print(int(ea - time.time()) if isinstance(ea, (int, float)) else "unknown")
except Exception:
    print("unknown")
'
}

LIVE_TOKEN_PROBE="$(token_probe 'Claude Code-credentials')"
LIVE_TOKEN_HASH="${LIVE_TOKEN_PROBE%% *}"

declare -a HAZARD_LINES=()
for label in "${want[@]}"; do
  svc="$(sed -n 's/^keychain_service *= *"\(.*\)"/\1/p' "$accounts_root/$label/manifest.toml")"
  [[ -n "$svc" ]] || continue
  probe="$(token_probe "$svc")"
  hash="${probe%% *}"; left="${probe##* }"
  shared=no; [[ "$hash" == "$LIVE_TOKEN_HASH" && "$hash" != "absent" ]] && shared=yes
  HAZARD_LINES+=("$label|$svc|$hash|$left|$shared")
  note "[$label] token sha12=$hash shares_live_session=$shared access_token_seconds_left=$left"
  if [[ "$shared" == "yes" ]] && [[ "$left" =~ ^-?[0-9]+$ ]] && (( left <= 300 )); then
    refuse "accounts/$label's cloned token is the SAME session as the operator's live login \
and its access token expires in ${left}s — spawning it would force a refresh whose token \
rotation could invalidate the operator's live session. Wait for a fresh, independent /login \
under that config dir instead."
  fi
done

LIVE_KEYCHAIN_BEFORE="$(keychain_mdat 'Claude Code-credentials')"
LIVE_CONFIG_BEFORE="$(file_witness "$HOME/.claude.json")"
DOT_THREADS_BEFORE="$( [[ -e "$HOME/.threads" ]] && echo present || echo absent )"
# NOTE on the ~/.claude.json witness: its BYTES change constantly under the
# operator's own running Claude Code session (it stores per-project history,
# costs, tips state...), so a byte diff is expected and proves nothing. The
# invariant that matters — and that this script's spawns physically cannot break,
# since every spawn sets CLAUDE_CONFIG_DIR to a captured dir — is that the LIVE
# LOGIN IDENTITY is unchanged. That is what `live_identity_unchanged` asserts;
# the byte witness is reported as informational only.

# ── identity fingerprints: which captured account is the live login? ──────────
# Hashes only. No email, uuid, org name or credential ever leaves this function.
fingerprint() {
  python3 - "$1" <<'PY'
import hashlib, json, sys
try:
    oa = (json.load(open(sys.argv[1])) or {}).get("oauthAccount") or {}
except Exception:
    print("unreadable"); raise SystemExit
if not oa:
    print("no-oauth-account"); raise SystemExit
h = lambda k: hashlib.sha256(str(oa.get(k)).encode()).hexdigest()[:8] if oa.get(k) is not None else "none"
print("acct=%s email=%s org=%s tier=%s" % (
    h("accountUuid"), h("emailAddress"), h("organizationUuid"),
    oa.get("subscriptionType") or oa.get("billingType") or "unknown"))
PY
}

LIVE_FP="$(fingerprint "$HOME/.claude.json")"
note "live login fingerprint: $LIVE_FP"
LIVE_MATCH="(none)"
declare -a FP_LINES=()
for label in primary secondary-live secondary; do
  [[ -f "$accounts_root/$label/.claude.json" ]] || continue
  fp="$(fingerprint "$accounts_root/$label/.claude.json")"
  FP_LINES+=("$label|$fp")
  note "  $label: $fp"
  [[ "$fp" == "$LIVE_FP" ]] && LIVE_MATCH="$label"
done
note "the operator's live manual login currently matches: $LIVE_MATCH"

hazards_json() {
  python3 - "${HAZARD_LINES[@]:-}" <<'PY'
import json, sys
cols = ("account", "keychain_service", "token_sha12",
        "access_token_seconds_left", "shares_live_session")
print(json.dumps([dict(zip(cols, r.split("|"))) for r in sys.argv[1:] if r]))
PY
}

if [[ "$LIVE" -ne 1 ]]; then
  note "DRY RUN (pass --live to actually run one haiku turn per account). Nothing spent."
  python3 - "$LIVE_MATCH" "$LIVE_FP" "$(hazards_json)" "${FP_LINES[@]:-}" <<'PY'
import json, sys
m, live, hz, *rest = sys.argv[1:]
print(json.dumps({
    "mode": "dry_run", "live_login_matches": m, "live_fingerprint": live,
    "captured": [dict(zip(("label", "fingerprint"), r.split("|", 1)))
                 for r in rest if r],
    "token_hazard": json.loads(hz),
}, indent=2))
PY
  exit 0
fi

# ── the live half ────────────────────────────────────────────────────────────
scratch="$tmp_root/threads-prove-accounts.$$"
mkdir -p "$scratch"
daemon_pid=""
cleanup() {
  if [[ -n "$daemon_pid" ]] && kill -0 "$daemon_pid" 2>/dev/null; then
    kill "$daemon_pid" 2>/dev/null || true
    wait "$daemon_pid" 2>/dev/null || true
  fi
  if [[ "$KEEP" -eq 1 ]]; then
    note "kept scratch dir $scratch"
  else
    rm -rf "$scratch"
  fi
}
trap '__st=$?; cleanup; __abort_guard' EXIT

bin="$repo/target/debug/threads"
stress="$repo/target/debug/threads-stress"
[[ -x "$bin" && -x "$stress" ]] || {
  note "building threads + threads-stress"
  cargo build -q -p threads --manifest-path "$repo/Cargo.toml" >&2
  cargo build -q -p threads --bin threads-stress --manifest-path "$repo/Cargo.toml" >&2
}

results_json="$scratch/results.jsonl"
: > "$results_json"
port=$BASE_PORT

for label in "${want[@]}"; do
  snapshot="$accounts_root/$label"
  run="$scratch/$label"
  mkdir -p "$run/cwd"

  cfg_before="$(file_witness "$snapshot/.claude.json")"

  # The throwaway registry: BOTH names point at the same captured snapshot, so
  # `Auto` resolves to `capture-<label>` (it sorts before "default") and any
  # failover can only land on the identical config dir. The machine-default
  # login is not reachable from this daemon.
  cat > "$run/accounts.toml" <<TOML
[accounts.capture-${label}]
config_dir = "${snapshot}"

[accounts.default]
config_dir = "${snapshot}"
TOML

  # probe_interval_secs = 0: no background /usage poll, so the ONLY claude
  # invocation this whole run makes is the single turn under test.
  # `chassis_port`, not `port`: since R2.S4 the chassis door is the daemon's
  # only port, and a bare `port` key is ignored — it would leave this
  # throwaway daemon trying to bind the live daemon's default.
  cat > "$run/config.toml" <<TOML
chassis_port = ${port}
host = "127.0.0.1"
data_dir = "${run}"
probe_interval_secs = 0
TOML

  note "[$label] starting throwaway daemon on 127.0.0.1:$port (data_dir $run)"
  env -u THREADS_CHASSIS_PORT THREADS_HOME="$run" RUST_LOG=warn "$bin" --config "$run/config.toml" \
    > "$run/daemon.log" 2>&1 &
  daemon_pid=$!

  ready=0
  for _ in $(seq 1 60); do
    if curl -fsS "http://127.0.0.1:$port/health" >/dev/null 2>&1; then ready=1; break; fi
    kill -0 "$daemon_pid" 2>/dev/null || break
    sleep 0.5
  done
  [[ "$ready" -eq 1 ]] || { cat "$run/daemon.log" >&2; die "[$label] daemon never became healthy"; }

  # Isolation assertion: not one account in this daemon's pool may be the
  # machine default (a null config_dir).
  THREADS_HOME="$run" "$bin" --config "$run/config.toml" accounts status --json \
    > "$run/status-before.json"
  python3 - "$run/status-before.json" "$snapshot" <<'PY' >&2 || die "isolation assertion failed"
import json, sys
payload = json.load(open(sys.argv[1]))
rows, want = payload["accounts"], sys.argv[2]
assert rows, "empty account pool"
for r in rows:
    assert r["config_dir"] == want, f"{r['account']} -> {r['config_dir']!r}, expected {want!r}"
assert not payload["totals"]["busy"], "the pool is not quiet"
print("isolation ok: every registered account maps to the captured snapshot, pool quiet")
PY

  note "[$label] running ONE $MODEL turn"
  set +e
  THREADS_HOME="$run" "$stress" \
    --url "ws://127.0.0.1:$port/ws" \
    --lock "$run/stress.lock" \
    --new-thread --cwd "$run/cwd" --model "$MODEL" \
    --text "$TEXT" --timeout-secs "$TIMEOUT_SECS" \
    > "$run/stress.json" 2> "$run/stress.err"
  stress_status=$?
  set -e
  note "[$label] threads-stress exit $stress_status: $(tr -d '\n' < "$run/stress.err")"

  THREADS_HOME="$run" "$bin" --config "$run/config.toml" accounts status --json \
    > "$run/status-after.json" || true

  # ── the ledger: which account served the turn? ──────────────────────────
  ledger="$(sqlite3 -json "$run/threads.db" \
    "SELECT account, model, input_tokens, output_tokens, cache_read_tokens,
            cache_creation_tokens, cost_usd, ts
     FROM usage_ledger ORDER BY id;" 2>/dev/null || echo '[]')"
  limits="$(sqlite3 -json "$run/threads.db" \
    "SELECT kind, count(*) AS n FROM events
     WHERE kind IN ('limit_hit','turn_failed','rate_limit_event','account_switched')
     GROUP BY kind;" 2>/dev/null || echo '[]')"
  # The verbatim failure/limit payloads — what a BROKEN verdict has to explain.
  failures="$(sqlite3 -json "$run/threads.db" \
    "SELECT kind, payload_json, ts FROM events
     WHERE kind IN ('limit_hit','turn_failed','turn_ended','assistant_text')
     ORDER BY seq;" 2>/dev/null || echo '[]')"
  obs="$(sqlite3 -json "$run/threads.db" \
    "SELECT account, axis, pct, source, resets_at FROM usage_observations ORDER BY id;" \
    2>/dev/null || echo '[]')"

  kill "$daemon_pid" 2>/dev/null || true
  wait "$daemon_pid" 2>/dev/null || true
  daemon_pid=""

  cfg_after="$(file_witness "$snapshot/.claude.json")"

  python3 - "$label" "$snapshot" "$stress_status" "$run/stress.json" \
      "$ledger" "$limits" "$obs" "$cfg_before" "$cfg_after" "$run/daemon.log" \
      "$failures" \
      >> "$results_json" <<'PY'
import json, sys
(label, snapshot, status, stress_path, ledger, limits, obs,
 cfg_before, cfg_after, daemon_log, failures) = sys.argv[1:]
try:
    report = json.load(open(stress_path))
except Exception as e:
    report = {"outcome": "no_report", "error": str(e)}
ledger = json.loads(ledger or "[]")
limits = {r["kind"]: r["n"] for r in json.loads(limits or "[]")}
obs = json.loads(obs or "[]")
events = json.loads(failures or "[]")
for e in events:
    try:
        e["payload"] = json.loads(e.pop("payload_json"))
    except Exception:
        pass
try:
    log_tail = open(daemon_log).read()[-2000:]
except Exception:
    log_tail = ""

served = sorted({r["account"] for r in ledger if r.get("account")})
cost = round(sum(r.get("cost_usd") or 0 for r in ledger), 6)
tokens = sum((r.get("input_tokens") or 0) + (r.get("output_tokens") or 0)
             + (r.get("cache_read_tokens") or 0)
             + (r.get("cache_creation_tokens") or 0) for r in ledger)

blob = (json.dumps(events) + log_tail).lower()
turn_failed = bool(limits.get("turn_failed"))
# NOTE: `rate_limit_event` is the FREE per-turn gauge push (DEC-15 layer 1) and
# fires on every successful turn — it is NOT evidence of a limit being hit. Only
# a `limit_hit` event or claude's own 429 wording counts.
rate_limited = (bool(limits.get("limit_hit"))
                or any(o.get("source") == "limit_hit" for o in obs)
                or "429" in blob
                or "usage limit reached" in blob)
# An auth failure is the ONE outcome that means the captured login is dead. It
# is distinguishable from every other failure by claude's own wording.
auth_dead = any(s in blob for s in (
    "invalid api key", "please run /login", "run /login",
    "oauth session expired", "oauth token has expired", "could not be refreshed",
    "authentication_error", "credit balance is too low", "invalid bearer token"))

if ledger and not turn_failed:
    verdict, why = "works", (
        "the turn completed and the ledger recorded real usage on this account")
elif rate_limited and not auth_dead:
    verdict, why = "works_rate_limited", (
        "the login authenticated and the account answered with a usage limit — "
        "a clean 429/limit observation still proves the credential works")
elif auth_dead:
    verdict, why = "BROKEN_AUTH", (
        "the captured credential did NOT authenticate — expired or rotated token; "
        "this login needs a fresh /login under its own CLAUDE_CONFIG_DIR")
elif turn_failed:
    verdict, why = "BROKEN_OTHER", (
        "the turn failed for a reason that is NOT an auth rejection and NOT a "
        "usage limit — see failure_events / daemon_log_tail")
elif report.get("outcome") in ("finished", "tripped"):
    verdict, why = "works_no_ledger_row", (
        "the turn completed but wrote no usage row — check the stream parse")
else:
    verdict, why = "INCONCLUSIVE", f"outcome={report.get('outcome')!r} exit={status}"

print(json.dumps({
    "account": label, "config_dir": snapshot,
    "verdict": verdict, "why": why,
    "stress_outcome": report.get("outcome"),
    "stress_exit": int(status),
    "thread_id": report.get("thread_id"),
    "turn_started": report.get("turn_started"),
    "exit_code": report.get("exit_code"),
    "ledger_rows": ledger,
    "ledger_served_accounts": served,
    "tokens": tokens, "cost_usd": cost,
    "limit_events": limits, "usage_observations": obs,
    "failure_events": events,
    "daemon_log_tail": log_tail[-1200:],
    "snapshot_claude_json_before": cfg_before,
    "snapshot_claude_json_after": cfg_after,
}))
PY

  port=$((port + 1))
done

LIVE_KEYCHAIN_AFTER="$(keychain_mdat 'Claude Code-credentials')"
LIVE_CONFIG_AFTER="$(file_witness "$HOME/.claude.json")"
DOT_THREADS_AFTER="$( [[ -e "$HOME/.threads" ]] && echo present || echo absent )"
LIVE_FP_AFTER="$(fingerprint "$HOME/.claude.json")"

python3 - "$results_json" "$LIVE_MATCH" "$LIVE_FP" \
    "$LIVE_KEYCHAIN_BEFORE" "$LIVE_KEYCHAIN_AFTER" \
    "$LIVE_CONFIG_BEFORE" "$LIVE_CONFIG_AFTER" \
    "$DOT_THREADS_BEFORE" "$DOT_THREADS_AFTER" "$(hazards_json)" "$LIVE_FP_AFTER" <<'PY'
import json, sys
(path, match, live_fp, kc_b, kc_a, cfg_b, cfg_a, th_b, th_a, hz, live_fp_a) = sys.argv[1:]
runs = [json.loads(l) for l in open(path) if l.strip()]
isolation = {
    # The two claims that matter.
    "live_identity_unchanged": live_fp == live_fp_a,
    "default_keychain_mdat_unchanged": kc_b == kc_a,
    "dot_threads_untouched": th_b == th_a == "absent",
    # Supporting detail.
    "default_keychain_mdat": kc_b,
    "dot_threads_before": th_b, "dot_threads_after": th_a,
    "home_claude_json_bytes_changed": cfg_b != cfg_a,
    "home_claude_json_note": (
        "byte changes here come from the operator's OWN running session; this "
        "script's spawns all set CLAUDE_CONFIG_DIR and cannot write this file"),
}
print(json.dumps({
    "mode": "live",
    "live_login_matches": match, "live_fingerprint": live_fp,
    "token_hazard": json.loads(hz),
    "accounts": runs,
    "total_cost_usd": round(sum(r["cost_usd"] for r in runs), 6),
    "total_tokens": sum(r["tokens"] for r in runs),
    "all_work": bool(runs) and all(r["verdict"].startswith("works") for r in runs),
    "isolation": isolation,
}, indent=2))
PY
exit  # REL-4.S7i: a fall-through end is a deliberate exit (see __abort_guard)
