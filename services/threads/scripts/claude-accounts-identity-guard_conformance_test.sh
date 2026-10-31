#!/usr/bin/env bash
# Conformance test for the IDENTITY GUARD in `claude-accounts` (R12.5): a login
# that authenticates as ANOTHER email is refused and reverted, by `login` and by
# `set-live-login`.
#
# THE GUARDS (doc ACCOUNT-EMAIL-ID.md §5, R12.5 row)
#   G5a CROSSED LOGIN  `login a@x.test` whose browser lands b@x.test ⇒ exit ≠ 0,
#                      `wanted a@x.test, got b@x.test`, the slot's files and the
#                      keychain restored byte for byte (fresh slot AND re-login),
#                      the crossed grant parked under a dormant name, not used.
#   G5a' STALE FILE    the login writes a `.claude.json` that SAYS a@ while the
#                      token is b@'s ⇒ still refused: the file is not evidence.
#   G5b                the same through `set-live-login` against a temp HOME:
#                      ~/.claude.json and the machine item restored, no pointer.
#   G5c                a MATCHING login passes, on both verbs (not constant-refuse),
#                      and case differences do not cross it (G1d normalisation);
#                      `a+1@` is NOT `a@`.
#   G5d                the profile source unreachable ⇒ refused (exit 4) and
#                      reverted, even though the login itself matched.
#   R10 (R12.10)       a login that PASSED asks the daemon to re-check the
#                      slot's flags exactly once (`threads accounts recheck
#                      <label>`, THREADS_HOME = the pool's home); a refused one
#                      (crossed, unreadable) never asks; a daemon that cannot be
#                      reached does not fail the login. The daemon's own re-check
#                      is guarded in Rust (`accounts::flag_recheck_tests`).
#   N4-N6 (R12.10)     the under-test gate: a pass-through wrapper or a copy of
#                      a `security` that does not answer the fakes' sentinel is
#                      refused (N4); HOME spelled in another case is still the
#                      real home (N5); seams set WITHOUT the test marker are
#                      refused before add-account runs (N6).
#
# WHY THIS CANNOT TOUCH A REAL CREDENTIAL. `security` and `claude` are FAKES at
# the front of PATH (the fake `security` refuses to run without its own store
# file); HOME, the slot pool and the state dir are temp dirs; the profile
# source is a local HTTP server whose answer is derived from the token the fake
# keychain actually holds — so the server only ever says what the stored
# credential says, never what a file says.
#
# FORCING: CA_UNDER_TEST=<a patched copy of claude-accounts> runs every case
# against that copy. R12.5's report pastes each forced red.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CA="${CA_UNDER_TEST:-$SCRIPT_DIR/claude-accounts}"
BASH5=""
for b in /opt/homebrew/bin/bash /usr/local/bin/bash; do [ -x "$b" ] && BASH5="$b" && break; done
[ -n "$BASH5" ] || { echo "needs bash >= 4.4 (brew install bash)" >&2; exit 2; }

PASS=0; FAIL=0
ok()  { PASS=$((PASS+1)); printf '  ok   %s\n' "$*"; }
bad() { FAIL=$((FAIL+1)); printf '  FAIL %s\n' "$*"; }
check() { if eval "$2"; then ok "$1"; else bad "$1"; fi; }

TMP="$(mktemp -d "${TMPDIR:-/tmp}/ca-identity-guard.XXXXXX")"
# Normalised: TMPDIR ends in `/`, and a `//` in a slot path changes its derived
# keychain service name, so the suite would compare against a name nothing uses.
TMP="$(cd "$TMP" && pwd)"
SERVER_PID=""
cleanup() { [ -z "$SERVER_PID" ] || kill "$SERVER_PID" 2>/dev/null; rm -rf "$TMP"; }
trap cleanup EXIT

BIN="$TMP/bin"; mkdir -p "$BIN"

# ── fake `security`: a JSON store, the subset of verbs claude-accounts uses ──
cat > "$BIN/security" <<'PY'
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
chmod +x "$BIN/security"

# ── fake `claude`: `auth login` lands FAKE_LOGIN_AS's grant in whichever store
#    CLAUDE_CONFIG_DIR names; `.claude.json` says FAKE_FILE_AS (default: the
#    same). `auth status` answers from that FILE, like the real one. ─────────
cat > "$BIN/claude" <<'PY'
#!/usr/bin/env python3
import hashlib, json, os, subprocess, sys, time
args = sys.argv[1:]
cfg = os.environ.get("CLAUDE_CONFIG_DIR")
svc = "Claude Code-credentials" + ("-" + hashlib.sha256(cfg.encode()).hexdigest()[:8] if cfg else "")
cj = os.path.join(cfg, ".claude.json") if cfg else os.path.join(os.environ["HOME"], ".claude.json")
if args[:2] == ["auth", "login"] and os.environ.get("FAKE_CONCURRENT"):
    # Not a login at all: a SESSION elsewhere rewrites ~/.claude.json (a
    # non-identity field, as running claude sessions do all the time) while
    # this "login" lands nothing.
    p = os.path.join(os.environ["HOME"], ".claude.json")
    d = json.load(open(p)); d["sessionWrite"] = os.environ["FAKE_CONCURRENT"]
    json.dump(d, open(p, "w"))
    # What the session wrote, kept OUTSIDE HOME so the run cannot touch it.
    json.dump(d, open(os.environ["FAKE_SESSION_COPY"], "w")); sys.exit(0)
if args[:2] == ["auth", "login"]:
    who = os.environ["FAKE_LOGIN_AS"]; fil = os.environ.get("FAKE_FILE_AS") or who
    n = int(time.time() * 1000)
    blob = json.dumps({"claudeAiOauth": {"accessToken": f"tok|{who}|{n}", "refreshToken": f"rt|{n}",
                       "expiresAt": n + 8 * 3600 * 1000, "refreshTokenExpiresAt": n + 30 * 86400 * 1000}})
    subprocess.run(["security", "add-generic-password", "-U", "-a", os.environ.get("USER", "x"),
                    "-s", svc, "-w", blob], check=True)
    try: d = json.load(open(cj))
    except Exception: d = {}
    d["oauthAccount"] = {"emailAddress": fil, "accountUuid": "uuid-" + fil, "organizationUuid": "org-1"}
    json.dump(d, open(cj, "w"))
    sys.exit(0)
if args[:2] == ["auth", "status"]:
    try: e = json.load(open(cj))["oauthAccount"]["emailAddress"]
    except Exception: print(json.dumps({"loggedIn": False})); sys.exit(1)
    print(json.dumps({"loggedIn": True, "email": e})); sys.exit(0)
sys.exit(2)
PY
chmod +x "$BIN/claude"

# ── fake add-account: what the real one does to a slot, minus cargo ─────────
cat > "$TMP/fake-add-account" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
label="" email=""
while [ $# -gt 0 ]; do case "$1" in
  --login) label="$2"; shift 2 ;; --email) email="$2"; shift 2 ;; *) shift ;; esac; done
# Where the REAL add-account signs in: <CLAUDE_ACCOUNTS_HOME>/accounts/<label>,
# else beside the script. Under the suite's own pool redirect, CLAUDE_ACCOUNTS_DIR.
# FAKE_IGNORE_HOME=1 is the pre-fix add-account, which ignored the home (B2).
if [ -n "${CLAUDE_ACCOUNTS_DIR:-}" ]; then base="$CLAUDE_ACCOUNTS_DIR"
elif [ -n "${CLAUDE_ACCOUNTS_HOME:-}" ] && [ -z "${FAKE_IGNORE_HOME:-}" ]; then base="$CLAUDE_ACCOUNTS_HOME/accounts"
else base="$FAKE_SCRIPT_HOME/accounts"; fi
slot="$base/$label"; mkdir -p "$slot"
CLAUDE_CONFIG_DIR="$slot" claude auth login --claudeai ${email:+--email "$email"}
printf 'label = "%s"\ncredential_mode = "own-login"\n' "$label" > "$slot/manifest.toml"
[ -z "$email" ] || printf '%s\n' "$email" > "$slot/login-hint"
SH
chmod +x "$TMP/fake-add-account"

# ── fake `threads` (R12.10): records every call, answers `accounts recheck` ──
# Every case points CLAUDE_ACCOUNTS_THREADS_BIN here, so no case can reach a
# real threads binary or a real daemon.
cat > "$TMP/fake-threads" <<'SH'
#!/usr/bin/env bash
printf '%s|%s\n' "${THREADS_HOME:-}" "$*" >> "${FAKE_THREADS_CALLS:-/dev/null}"
if [ "${1:-} ${2:-}" = "accounts recheck" ]; then
  # FAKE_RECHECK_USAGE=1: a threads binary OLDER than the verb — its usage
  # text on stderr and exit 2, exactly as `main.rs` `usage()` does.
  if [ "${FAKE_RECHECK_USAGE:-0}" = 1 ]; then
    printf 'usage:\n  threads [--config <path>]          run the daemon\n' >&2; exit 2
  fi
  echo "${3:-}: identity verified (fake daemon)"
  exit "${FAKE_RECHECK_RC:-0}"
fi
exit 0
SH
chmod +x "$TMP/fake-threads"

# ── the profile source: answers from the token it is SENT ───────────────────
cat > "$TMP/profile.py" <<'PY'
import http.server, json, sys
hits = sys.argv[2]
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        open(hits, "a").write("hit\n")
        auth = self.headers.get("Authorization", "")
        parts = auth[len("Bearer "):].split("|") if auth.startswith("Bearer ") else []
        if self.path != "/api/oauth/profile" or len(parts) != 3 or parts[0] != "tok":
            self.send_response(401); self.end_headers(); return
        body = json.dumps({"account": {"email": parts[1], "uuid": "uuid-" + parts[1]},
                           "organization": {"uuid": "org-1"}}).encode()
        self.send_response(200); self.send_header("Content-Type", "application/json")
        self.end_headers(); self.wfile.write(body)
s = http.server.HTTPServer(("127.0.0.1", 0), H)
open(sys.argv[1], "w").write(str(s.server_address[1]))
s.serve_forever()
PY
python3 "$TMP/profile.py" "$TMP/port" "$TMP/hits" & SERVER_PID=$!
for _ in $(seq 1 50); do [ -s "$TMP/port" ] && break; sleep 0.1; done
[ -s "$TMP/port" ] || { echo "profile server did not start" >&2; exit 2; }
URL_UP="http://127.0.0.1:$(cat "$TMP/port")/api/oauth/profile"
# A port that was just bound and released: nothing listens there.
URL_DOWN="http://127.0.0.1:$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')/api/oauth/profile"
hits() { [ -f "$TMP/hits" ] && wc -l < "$TMP/hits" | tr -d ' ' || echo 0; }

sha8() { printf %s "$1" | shasum -a 256 | cut -c1-8; }
svc_for() { printf 'Claude Code-credentials-%s' "$(sha8 "$1")"; }
# The keychain as "<service> <secret sha256>" lines, retired/prelogin excluded,
# so two dumps compare byte for byte.
kc_dump() { FAKE_KC="$1" python3 -c '
import hashlib, json, os
d = json.load(open(os.environ["FAKE_KC"]))["items"]
for k in sorted(d):
    if k.startswith("Claude Code-retired") or k.startswith("Claude Code-prelogin"): continue
    print(k, hashlib.sha256(bytes.fromhex(d[k]["hex"])).hexdigest())'; }
kc_names() { FAKE_KC="$1" python3 -c '
import json, os; print("\n".join(sorted(json.load(open(os.environ["FAKE_KC"]))["items"])))'; }
kc_secret_of() { FAKE_KC="$1" python3 -c '
import json, os, sys; it = json.load(open(os.environ["FAKE_KC"]))["items"].get(sys.argv[1])
print(bytes.fromhex(it["hex"]).decode() if it else "")' "$2"; }
retired_tokens_for() { FAKE_KC="$1" python3 -c '
import json, os, sys
d = json.load(open(os.environ["FAKE_KC"]))["items"]
print(sum(1 for k, v in d.items() if k.startswith("Claude Code-retired")
          and "|" + sys.argv[1] + "|" in bytes.fromhex(v["hex"]).decode()))' "$2"; }

# A fresh world per case: echo its root.
world() {
  local w="$TMP/w$1"; mkdir -p "$w/home" "$w/accounts" "$w/state"
  printf '{"items": {}, "clock": 0}' > "$w/kc.json"; : > "$w/kc-list"
  printf '%s' "$w"
}
# Run claude-accounts inside world $1 with the profile URL $2; rest = args.
ca() {
  local w="$1" url="$2"; shift 2
  env -u CLAUDE_CODE_OAUTH_TOKEN -u CLAUDE_SECURESTORAGE_CONFIG_DIR -u CLAUDE_CONFIG_DIR \
    -u ANTHROPIC_API_KEY \
    PATH="${PATH_FIRST:+$PATH_FIRST:}$BIN:$PATH" HOME="${HOME_AS:-$w/home}" FAKE_KC="$w/kc.json" \
    CLAUDE_ACCOUNTS_TEST=1 CLAUDE_ACCOUNTS_DIR="$w/accounts" CLAUDE_ACCOUNTS_STATE_DIR="$w/state" \
    CLAUDE_ACCOUNTS_KEYCHAIN_LIST="$w/kc-list" CLAUDE_ACCOUNTS_ADD_ACCOUNT="$TMP/fake-add-account" \
    CLAUDE_ACCOUNTS_PROFILE_URL="$url" CLAUDE_ACCOUNTS_HEALTH_JSON="$w/health.json" \
    CLAUDE_ACCOUNTS_LOG="$w/rotation.log" FAKE_LOGIN_AS="${AS:-}" FAKE_FILE_AS="${FILE_AS:-}" \
    CLAUDE_ACCOUNTS_THREADS_BIN="$TMP/fake-threads" FAKE_THREADS_CALLS="$w/threads-calls" \
    FAKE_RECHECK_RC="${RECHECK_RC:-0}" FAKE_RECHECK_USAGE="${RECHECK_USAGE:-0}" \
    "$BASH5" "$CA" "$@" < /dev/null
}
# Every file under a slot, with its sha256 — compared before/after.
slot_dump() { [ -d "$1" ] || { echo "<no slot dir>"; return; }
  (cd "$1" && find . -type f | sort | while read -r f; do printf '%s %s\n' "$f" "$(shasum -a 256 < "$f" | cut -c1-64)"; done); }

echo "claude-accounts identity guard conformance  ($CA)"

# ═══ G5a CROSSED LOGIN, fresh slot ══════════════════════════════════════════
echo "── G5a: login a@x.test lands b@x.test (fresh slot)"
W="$(world 1)"; SLOT="$W/accounts/a-x-test"; SVC="$(svc_for "$SLOT")"
kc_before="$(kc_dump "$W/kc.json")"; slot_before="$(slot_dump "$SLOT")"; h0="$(hits)"
out="$(AS=b@x.test ca "$W" "$URL_UP" login a@x.test 2>&1)"; rc=$?
check "G5a exits non-zero (rc=$rc)" '[ "$rc" -ne 0 ]'
check "G5a says: wanted a@x.test, got b@x.test" 'printf "%s" "$out" | grep -qF "wanted a@x.test, got b@x.test"'
check "G5a asked the profile SERVER (hits $h0 -> $(hits))" '[ "$(hits)" -gt "$h0" ]'
check "G5a keychain restored byte for byte (no slot item)" '[ "$(kc_dump "$W/kc.json")" = "$kc_before" ]'
check "G5a slot files restored (none created survive)" '[ "$(slot_dump "$SLOT")" = "$slot_before" ] || [ "$(slot_dump "$SLOT")" = "" ]'
check "G5a slot has no .claude.json naming b@" '! grep -qs "b@x.test" "$SLOT/.claude.json"'
check "G5a b@'s grant is PARKED (one retired item), not deleted" '[ "$(retired_tokens_for "$W/kc.json" b@x.test)" = 1 ]'
check "G5a no prelogin scratch item left" '! kc_names "$W/kc.json" | grep -q "^Claude Code-prelogin"'
check "G5a no verified-hook line on a refusal" '! printf "%s" "$out" | grep -q "identity verified"'

# ═══ G5a CROSSED RE-LOGIN: the previous good grant comes back ═══════════════
echo "── G5a: re-login of a slot that already holds a@'s grant, lands b@"
W="$(world 2)"; SLOT="$W/accounts/a-x-test"; SVC="$(svc_for "$SLOT")"
out="$(AS=a@x.test ca "$W" "$URL_UP" login a@x.test 2>&1)"; rc=$?
check "G5a setup: a@ signs in cleanly first (rc=$rc)" '[ "$rc" -eq 0 ]'
kc_before="$(kc_dump "$W/kc.json")"; slot_before="$(slot_dump "$SLOT")"
tok_before="$(kc_secret_of "$W/kc.json" "$SVC")"
check "G5a setup: the slot item exists and is a@'s" 'printf "%s" "$tok_before" | grep -q "tok|a@x.test|"'
sleep 0.01
out="$(AS=b@x.test ca "$W" "$URL_UP" login a-x-test 2>&1)"; rc=$?
check "G5a re-login exits non-zero (rc=$rc)" '[ "$rc" -ne 0 ]'
check "G5a re-login says: wanted a@x.test, got b@x.test" 'printf "%s" "$out" | grep -qF "wanted a@x.test, got b@x.test"'
check "G5a re-login: keychain restored byte for byte (a@'s grant back)" '[ "$(kc_dump "$W/kc.json")" = "$kc_before" ]'
check "G5a re-login: slot files restored byte for byte" '[ "$(slot_dump "$SLOT")" = "$slot_before" ]'
check "G5a re-login: b@'s grant parked, not in the slot" '[ "$(retired_tokens_for "$W/kc.json" b@x.test)" = 1 ]'
check "G5a re-login: no prelogin scratch item left" '! kc_names "$W/kc.json" | grep -q "^Claude Code-prelogin"'

# ═══ G5a' STALE FILE: the file says a@, the token is b@'s ════════════════════
echo "── G5a': the login writes a .claude.json naming a@ but stores b@'s token"
W="$(world 3)"; SLOT="$W/accounts/a-x-test"
kc_before="$(kc_dump "$W/kc.json")"
out="$(AS=b@x.test FILE_AS=a@x.test ca "$W" "$URL_UP" login a@x.test 2>&1)"; rc=$?
check "G5a' refused although the file says a@ (rc=$rc)" '[ "$rc" -ne 0 ] && printf "%s" "$out" | grep -qF "wanted a@x.test, got b@x.test"'
check "G5a' keychain restored" '[ "$(kc_dump "$W/kc.json")" = "$kc_before" ]'

# ═══ G5c MATCHING LOGIN passes; normalisation (G1d) ═════════════════════════
echo "── G5c: login a@x.test lands a@x.test"
W="$(world 4)"; SLOT="$W/accounts/a-x-test"; SVC="$(svc_for "$SLOT")"
out="$(AS=a@x.test ca "$W" "$URL_UP" login a@x.test 2>&1)"; rc=$?
check "G5c exits 0 (rc=$rc)" '[ "$rc" -eq 0 ]'
check "G5c the slot keeps a@'s grant" 'kc_secret_of "$W/kc.json" "$SVC" | grep -q "tok|a@x.test|"'
check "G5c the post-login hook ran once" '[ "$(printf "%s\n" "$out" | grep -c "identity verified by the server")" = 1 ]'
check "G5c nothing retired" '[ "$(retired_tokens_for "$W/kc.json" a@x.test)" = 0 ]'
check "G5c no prelogin scratch item, no snapshot dir left" '! kc_names "$W/kc.json" | grep -q "^Claude Code-prelogin" && [ -z "$(ls -A "$W/accounts/backups/pre-login" 2>/dev/null)" ]'
W="$(world 5)"
out="$(AS=a@x.test ca "$W" "$URL_UP" login "A@X.Test" 2>&1)"; rc=$?
check "G5c (G1d) A@X.Test asked, a@x.test served -> passes (rc=$rc)" '[ "$rc" -eq 0 ]'
W="$(world 6)"
out="$(AS=a@x.test ca "$W" "$URL_UP" login "a+1@x.test" 2>&1)"; rc=$?
check "G5c (G1d) a+1@x.test asked, a@x.test served -> refused (rc=$rc)" '[ "$rc" -ne 0 ] && printf "%s" "$out" | grep -qF "wanted a+1@x.test, got a@x.test"'

# ═══ G5d UNREACHABLE profile source ═════════════════════════════════════════
echo "── G5d: a matching login, but the profile source is unreachable"
W="$(world 7)"; SLOT="$W/accounts/a-x-test"
kc_before="$(kc_dump "$W/kc.json")"
out="$(AS=a@x.test ca "$W" "$URL_DOWN" login a@x.test 2>&1)"; rc=$?
check "G5d refused with exit 4 (rc=$rc)" '[ "$rc" -eq 4 ]'
check "G5d says why: wanted a@x.test, got <unreadable" 'printf "%s" "$out" | grep -qF "wanted a@x.test, got <unreadable"'
check "G5d reverted the keychain" '[ "$(kc_dump "$W/kc.json")" = "$kc_before" ]'
check "G5d not signed in by the cached file (no .claude.json left)" '[ ! -f "$SLOT/.claude.json" ]'

# ═══ G5b set-live-login against a temp HOME ═════════════════════════════════
# A pool with the outgoing live account `old-x-test` (its own slot) and the
# target `a-x-test`, whose login hint is a@x.test.
setup_live() {
  local w="$1"
  mkdir -p "$w/accounts/a-x-test" "$w/accounts/old-x-test"
  printf 'a@x.test\n' > "$w/accounts/a-x-test/login-hint"
  printf '{"oauthAccount": {"emailAddress": "old@x.test", "accountUuid": "uuid-old@x.test"}, "keep": 1}' > "$w/home/.claude.json"
  FAKE_KC="$w/kc.json" PATH="$BIN:$PATH" security add-generic-password -U -a "$USER" \
    -s "Claude Code-credentials" -w "tok|old@x.test|1"
  cat > "$w/health.json" <<JSON
{"slots": [
 {"label": "a-x-test", "is_machine_default": false, "is_live_login": false, "credential_mode": "own-login",
  "health": {"state": "live"}, "identity": {"acct": "$(sha8 uuid-a@x.test)", "email": "$(sha8 a@x.test)"}, "secret_sha8": "aaaaaaaa"},
 {"label": "old-x-test", "is_machine_default": false, "is_live_login": true, "credential_mode": "own-login",
  "health": {"state": "live"}, "identity": {"acct": "$(sha8 uuid-old@x.test)", "email": "$(sha8 old@x.test)"}, "secret_sha8": "bbbbbbbb"}
]}
JSON
}
echo "── G5b: set-live-login a@x.test lands b@x.test in the machine store"
W="$(world 8)"; setup_live "$W"
home_before="$(shasum -a 256 < "$W/home/.claude.json")"
machine_before="$(kc_secret_of "$W/kc.json" "Claude Code-credentials")"
out="$(AS=b@x.test ca "$W" "$URL_UP" set-live-login a@x.test --yes --no-browser 2>&1)"; rc=$?
check "G5b exits non-zero (rc=$rc)" '[ "$rc" -ne 0 ]'
check "G5b says: wanted a@x.test, got b@x.test" 'printf "%s" "$out" | grep -qF "wanted a@x.test, got b@x.test"'
check "G5b ~/.claude.json restored byte for byte" '[ "$(shasum -a 256 < "$W/home/.claude.json")" = "$home_before" ]'
check "G5b machine keychain item restored byte for byte" '[ "$(kc_secret_of "$W/kc.json" "Claude Code-credentials")" = "$machine_before" ]'
check "G5b no live-login pointer written" '[ ! -f "$W/accounts/live-login" ]'

echo "── G5c: set-live-login a@x.test lands a@x.test"
W="$(world 9)"; setup_live "$W"
out="$(AS=a@x.test ca "$W" "$URL_UP" set-live-login a@x.test --yes --no-browser 2>&1)"; rc=$?
check "G5c set-live-login exits 0 (rc=$rc)" '[ "$rc" -eq 0 ]'
check "G5c machine store now holds a@'s grant" 'kc_secret_of "$W/kc.json" "Claude Code-credentials" | grep -q "tok|a@x.test|"'
check "G5c live-login pointer names a-x-test" '[ "$(cat "$W/accounts/live-login" 2>/dev/null)" = "a-x-test" ]'
check "G5c set-live-login hook ran once" '[ "$(printf "%s\n" "$out" | grep -c "identity verified by the server")" = 1 ]'

echo "── G5d: set-live-login with the profile source unreachable"
W="$(world 10)"; setup_live "$W"
home_before="$(shasum -a 256 < "$W/home/.claude.json")"
machine_before="$(kc_secret_of "$W/kc.json" "Claude Code-credentials")"
out="$(AS=a@x.test ca "$W" "$URL_DOWN" set-live-login a@x.test --yes --no-browser 2>&1)"; rc=$?
check "G5d set-live-login refused with exit 4 (rc=$rc)" '[ "$rc" -eq 4 ]'
check "G5d set-live-login restored ~/.claude.json and the machine item" '[ "$(shasum -a 256 < "$W/home/.claude.json")" = "$home_before" ] && [ "$(kc_secret_of "$W/kc.json" "Claude Code-credentials")" = "$machine_before" ]'

echo "── G5b: set-live-login with NO address known: checked against the recorded identity"
W="$(world 11)"; setup_live "$W"; rm -f "$W/accounts/a-x-test/login-hint"
machine_before="$(kc_secret_of "$W/kc.json" "Claude Code-credentials")"
out="$(AS=b@x.test ca "$W" "$URL_UP" set-live-login a-x-test --yes --no-browser 2>&1)"; rc=$?
check "G5b (no hint) b@ landing is refused (rc=$rc)" '[ "$rc" -eq 3 ] && printf "%s" "$out" | grep -qF "got b@x.test"'
check "G5b (no hint) machine item restored" '[ "$(kc_secret_of "$W/kc.json" "Claude Code-credentials")" = "$machine_before" ]'
W="$(world 12)"; setup_live "$W"; rm -f "$W/accounts/a-x-test/login-hint"
out="$(AS=a@x.test ca "$W" "$URL_UP" set-live-login a-x-test --yes --no-browser 2>&1)"; rc=$?
check "G5c (no hint) a@ landing passes on the recorded identity (rc=$rc)" '[ "$rc" -eq 0 ]'

# ═══ H1 (inspect B1): never the operator's real login, and never a session's write ═
echo "── H1a: under test, a guarded write with HOME = the real home is REFUSED before anything"
W="$(world 13)"; setup_live "$W"
home_before="$(shasum -a 256 < "$W/home/.claude.json")"; kc_before="$(kc_dump "$W/kc.json")"
kc_raw_before="$(shasum -a 256 < "$W/kc.json")"
out="$(CLAUDE_ACCOUNTS_REAL_HOME="$W/home" AS=b@x.test ca "$W" "$URL_UP" set-live-login a@x.test --yes --no-browser 2>&1)"; rc=$?
check "H1a set-live-login refused (rc=$rc)" '[ "$rc" -ne 0 ] && printf "%s" "$out" | grep -qF "REFUSED under test: HOME is the real home"'
check "H1a ~/.claude.json byte-identical" '[ "$(shasum -a 256 < "$W/home/.claude.json")" = "$home_before" ]'
check "H1a keychain store byte-identical (not even an mdat moved)" '[ "$(shasum -a 256 < "$W/kc.json")" = "$kc_raw_before" ]'
check "H1a no backup dir was made" '[ ! -d "$W/accounts/backups/live-login" ]'
out="$(CLAUDE_ACCOUNTS_REAL_HOME="$W/home" AS=b@x.test ca "$W" "$URL_UP" login a@x.test 2>&1)"; rc=$?
check "H1a login refused too (rc=$rc), keychain byte-identical" '[ "$rc" -ne 0 ] && printf "%s" "$out" | grep -qF "REFUSED under test: HOME is the real home" && [ "$(shasum -a 256 < "$W/kc.json")" = "$kc_raw_before" ]'

echo "── H1b: a session rewrites ~/.claude.json mid-run; the unverified login landed nothing"
W="$(world 14)"; setup_live "$W"
mdat_before="$(FAKE_KC="$W/kc.json" python3 -c 'import json,os;print(json.load(open(os.environ["FAKE_KC"]))["items"]["Claude Code-credentials"]["mdat"])')"
out="$(FAKE_SESSION_COPY="$W/session-wrote.json" FAKE_CONCURRENT=written-by-a-session ca "$W" "$URL_DOWN" set-live-login a@x.test --yes --no-browser 2>&1)"; rc=$?
check "H1b refused, unreadable (rc=$rc)" '[ "$rc" -eq 4 ]'
check "H1b the session really wrote mid-run (sentinel: its copy exists)" '[ -s "$W/session-wrote.json" ]'
check "H1b ~/.claude.json is BYTE-IDENTICAL to what the session wrote" 'cmp -s "$W/home/.claude.json" "$W/session-wrote.json"'
check "H1b the machine keychain item was NOT rewritten (mdat $mdat_before unchanged)" '[ "$(FAKE_KC="$W/kc.json" python3 -c '"'"'import json,os;print(json.load(open(os.environ["FAKE_KC"]))["items"]["Claude Code-credentials"]["mdat"])'"'"')" = "$mdat_before" ]'
check "H1b no .claude.json.before-restore-* left behind" '[ -z "$(ls -A "$W/home" | grep before-restore)" ]'
echo "── H1c (opposition): the identity DID move ⇒ the revert still runs"
W="$(world 15)"; setup_live "$W"; home_before="$(shasum -a 256 < "$W/home/.claude.json")"
out="$(AS=b@x.test ca "$W" "$URL_DOWN" set-live-login a@x.test --yes --no-browser 2>&1)"; rc=$?
check "H1c refused and ~/.claude.json restored (rc=$rc)" '[ "$rc" -eq 4 ] && [ "$(shasum -a 256 < "$W/home/.claude.json")" = "$home_before" ]'

# ═══ H2 (inspect B2): a login under CLAUDE_ACCOUNTS_HOME is checked too ═══════
# No pool redirect: the pool is <CLAUDE_ACCOUNTS_HOME>/accounts, as in prod.
ca_home() {
  local w="$1" url="$2"; shift 2
  env -u CLAUDE_CODE_OAUTH_TOKEN -u CLAUDE_SECURESTORAGE_CONFIG_DIR -u CLAUDE_CONFIG_DIR \
    -u ANTHROPIC_API_KEY -u CLAUDE_ACCOUNTS_DIR -u CLAUDE_ACCOUNTS_STATE_DIR \
    PATH="$BIN:$PATH" HOME="$w/home" FAKE_KC="$w/kc.json" FAKE_SCRIPT_HOME="$w/script-home" \
    CLAUDE_ACCOUNTS_HOME="$w/pool" \
    CLAUDE_ACCOUNTS_TEST=1 CLAUDE_ACCOUNTS_KEYCHAIN_LIST="$w/kc-list" \
    CLAUDE_ACCOUNTS_ADD_ACCOUNT="$TMP/fake-add-account" CLAUDE_ACCOUNTS_PROFILE_URL="$url" \
    CLAUDE_ACCOUNTS_LOG="$w/rotation.log" FAKE_LOGIN_AS="${AS:-}" FAKE_IGNORE_HOME="${IGNORE:-}" \
    CLAUDE_ACCOUNTS_THREADS_BIN="$TMP/fake-threads" FAKE_THREADS_CALLS="$w/threads-calls" \
    "$BASH5" "$CA" "$@" < /dev/null
}
# H2 is only safe against a claude-accounts that HONOURS CLAUDE_ACCOUNTS_HOME:
# one that ignores it would use the pool beside the script, outside this temp
# dir. Refuse the whole suite rather than write there.
if ! grep -q 'THREADS_DIR="$(cd "$CLAUDE_ACCOUNTS_HOME" && pwd)"' "$CA"; then
  bad "H2 not run: $CA does not honour CLAUDE_ACCOUNTS_HOME, so H2 would write the pool beside it"
  echo "claude-accounts identity guard conformance: $PASS passed, $FAIL failed"; exit 1
fi
echo "── H2a: CLAUDE_ACCOUNTS_HOME set, login a@x.test lands b@x.test"
W="$(world 16)"; mkdir -p "$W/pool/accounts" "$W/script-home/accounts"
kc_before="$(kc_dump "$W/kc.json")"; h0="$(hits)"
out="$(AS=b@x.test ca_home "$W" "$URL_UP" login a@x.test 2>&1)"; rc=$?
check "H2a refused (rc=$rc): wanted a@x.test, got b@x.test" '[ "$rc" -eq 3 ] && printf "%s" "$out" | grep -qF "wanted a@x.test, got b@x.test"'
check "H2a the server was asked (hits $h0 -> $(hits))" '[ "$(hits)" -gt "$h0" ]'
check "H2a keychain restored" '[ "$(kc_dump "$W/kc.json")" = "$kc_before" ]'
echo "── H2b: an add-account that signs in somewhere ELSE (ignores the home) ⇒ refused, not passed"
W="$(world 17)"; mkdir -p "$W/pool/accounts" "$W/script-home/accounts"
out="$(IGNORE=1 AS=b@x.test ca_home "$W" "$URL_UP" login a@x.test 2>&1)"; rc=$?
check "H2b exits non-zero (rc=$rc)" '[ "$rc" -ne 0 ]'
check "H2b says the login landed where it was not checked" 'printf "%s" "$out" | grep -qF "landed where it was not checked"'
check "H2b wrote no login hint into the checked pool" '[ ! -f "$W/pool/accounts/a-x-test/login-hint" ]'
echo "── H2c (opposition): CLAUDE_ACCOUNTS_HOME set, a matching login passes"
W="$(world 18)"; mkdir -p "$W/pool/accounts" "$W/script-home/accounts"
out="$(AS=a@x.test ca_home "$W" "$URL_UP" login a@x.test 2>&1)"; rc=$?
check "H2c exits 0 (rc=$rc)" '[ "$rc" -eq 0 ]'
check "H2c the grant is in the CHECKED slot" 'kc_secret_of "$W/kc.json" "$(svc_for "$W/pool/accounts/a-x-test")" | grep -q "tok|a@x.test|"'
echo "── H2d: the REAL add-account signs in under CLAUDE_ACCOUNTS_HOME (--dry-run, writes no credential)"
AA="${AA_UNDER_TEST:-$SCRIPT_DIR/add-account}"
W="$(world 19)"; mkdir -p "$W/pool/accounts"
out="$(env -u CLAUDE_CODE_OAUTH_TOKEN -u CLAUDE_CONFIG_DIR PATH="$BIN:$PATH" HOME="$W/home" FAKE_KC="$W/kc.json" \
  CLAUDE_ACCOUNTS_HOME="$W/pool" "$BASH5" "$AA" --login a-x-test --dry-run --no-browser 2>&1)"; rc=$?
check "H2d add-account's slot dir is the home's (rc=$rc)" 'printf "%s" "$out" | grep -qF "slot dir         : $W/pool/accounts/a-x-test"'

# ═══ R10 (R12.10): a verified login asks the daemon to re-check, once ═══════
rechecks() { { cat "$1/threads-calls" 2>/dev/null || true; } | grep -c '|accounts recheck ' || true; }
echo "── R10a: a verified login asks the daemon to re-check that slot's flags"
W="$(world 20)"
out="$(AS=a@x.test ca "$W" "$URL_UP" login a@x.test 2>&1)"; rc=$?
check "R10a login exits 0 (rc=$rc)" '[ "$rc" -eq 0 ]'
check "R10a exactly one re-check ($(rechecks "$W"))" '[ "$(rechecks "$W")" = 1 ]'
check "R10a it names the slot and the pool's home" 'grep -qxF "$W/state|accounts recheck a-x-test" "$W/threads-calls"'
check "R10a the daemon's answer is shown" 'printf "%s" "$out" | grep -qF "a-x-test: identity verified (fake daemon)"'
echo "── R10b (opposition): a CROSSED login never asks"
W="$(world 21)"
out="$(AS=b@x.test ca "$W" "$URL_UP" login a@x.test 2>&1)"; rc=$?
check "R10b refused (rc=$rc) and no re-check asked ($(rechecks "$W"))" '[ "$rc" -eq 3 ] && [ "$(rechecks "$W")" = 0 ]'
echo "── R10c: an UNREADABLE login never asks"
W="$(world 22)"
out="$(AS=a@x.test ca "$W" "$URL_DOWN" login a@x.test 2>&1)"; rc=$?
check "R10c refused (rc=$rc) and no re-check asked ($(rechecks "$W"))" '[ "$rc" -eq 4 ] && [ "$(rechecks "$W")" = 0 ]'
echo "── R10d: a verified set-live-login asks for its target"
W="$(world 23)"; setup_live "$W"
out="$(AS=a@x.test ca "$W" "$URL_UP" set-live-login a@x.test --yes --no-browser 2>&1)"; rc=$?
check "R10d exits 0 (rc=$rc) and asks once for a-x-test" '[ "$rc" -eq 0 ] && [ "$(rechecks "$W")" = 1 ] && grep -qxF "$W/state|accounts recheck a-x-test" "$W/threads-calls"'
echo "── R10e: the daemon cannot be reached ⇒ the login still succeeds, and says so"
W="$(world 24)"
out="$(RECHECK_RC=4 AS=a@x.test ca "$W" "$URL_UP" login a@x.test 2>&1)"; rc=$?
check "R10e exits 0 (rc=$rc)" '[ "$rc" -eq 0 ]'
check "R10e says the flags were not re-checked now" 'printf "%s" "$out" | grep -qF "not re-checked now (threads accounts recheck rc=4)"'
echo "── R10f: a threads binary OLDER than the verb ⇒ a plain sentence naming what is missing, not \"usage:\""
W="$(world 25)"
out="$(RECHECK_USAGE=1 AS=a@x.test ca "$W" "$URL_UP" login a@x.test 2>&1)"; rc=$?
check "R10f exits 0 (rc=$rc)" '[ "$rc" -eq 0 ]'
check "R10f names the missing command" 'printf "%s" "$out" | grep -qF "has no '"'"'accounts recheck'"'"' command (it is older than the flag re-check, R12.10)"'
check "R10f says what happens to the flags instead" 'printf "%s" "$out" | grep -qF "a login bar only when its timer runs out"'
check "R10f does not print the bare usage line" '! printf "%s" "$out" | grep -qF "rc=2): usage:"'
echo "── R10g (opposition): rc=2 WITHOUT a usage text keeps the generic line"
W="$(world 26)"
out="$(RECHECK_RC=2 AS=a@x.test ca "$W" "$URL_UP" login a@x.test 2>&1)"; rc=$?
check "R10g exits 0 (rc=$rc) and does not claim a missing command" '[ "$rc" -eq 0 ] && ! printf "%s" "$out" | grep -qF "has no '"'"'accounts recheck'"'"' command" && printf "%s" "$out" | grep -qF "not re-checked now (threads accounts recheck rc=2)"'

# ═══ N4-N6 (R12.10, R12.5 inspect-2): the under-test gate cannot be walked past ═
# A `security` that does NOT answer the sentinel stands for the system binary:
# the store fake with its sentinel line removed. The real binary is never run.
NOSENT="$TMP/nosent/security-system"; mkdir -p "$TMP/nosent" "$TMP/wrapbin" "$TMP/copybin"
grep -v 'claude-accounts-fake' "$BIN/security" > "$NOSENT"; chmod +x "$NOSENT"
printf '#!/bin/bash\nexec "%s" "$@"\n' "$NOSENT" > "$TMP/wrapbin/security"; chmod +x "$TMP/wrapbin/security"
cp -p "$NOSENT" "$TMP/copybin/security"
check "N4 sentinel: the stand-in does not answer the verb, the fake does" '! "$NOSENT" --claude-accounts-fake && "$BIN/security" --claude-accounts-fake'
for shape in wrapbin copybin; do
  echo "── N4: under test, \`security\` is a $shape of a binary that is not a fake"
  W="$(world "n4-$shape")"; kc_raw_before="$(shasum -a 256 < "$W/kc.json")"
  out="$(PATH_FIRST="$TMP/$shape" AS=a@x.test ca "$W" "$URL_UP" login a@x.test 2>&1)"; rc=$?
  check "N4 $shape refused (rc=$rc)" '[ "$rc" -ne 0 ] && printf "%s" "$out" | grep -qF "is not a test fake"'
  check "N4 $shape store byte-identical, add-account never ran" '[ "$(shasum -a 256 < "$W/kc.json")" = "$kc_raw_before" ] && [ ! -d "$W/accounts/a-x-test" ]'
done
echo "── N5: HOME is the real home spelled in another case"
W="$(world n5)"; setup_live "$W"; kc_raw_before="$(shasum -a 256 < "$W/kc.json")"
check "N5 precondition: \$W/HOME is the same directory as \$W/home (case-insensitive volume)" '[ "$(stat -f "%d:%i" "$W/HOME" 2>/dev/null)" = "$(stat -f "%d:%i" "$W/home")" ]'
out="$(CLAUDE_ACCOUNTS_REAL_HOME="$W/home" HOME_AS="$W/HOME" AS=b@x.test ca "$W" "$URL_UP" set-live-login a@x.test --yes --no-browser 2>&1)"; rc=$?
check "N5 refused (rc=$rc): HOME is the real home" '[ "$rc" -ne 0 ] && printf "%s" "$out" | grep -qF "REFUSED under test: HOME is the real home"'
check "N5 keychain store byte-identical" '[ "$(shasum -a 256 < "$W/kc.json")" = "$kc_raw_before" ]'
echo "── N6: seams set WITHOUT CLAUDE_ACCOUNTS_TEST=1 ⇒ refused before add-account runs"
# A copy of claude-accounts in a throwaway tree, with a recording add-account
# beside it: without the marker every seam is ignored, so this is the only
# add-account it could reach. The pool is the production CLAUDE_ACCOUNTS_HOME.
W="$(world n6)"; R="$W/repo/services/threads/scripts"; mkdir -p "$R" "$W/pool/accounts"
cp -p "$CA" "$R/claude-accounts"
printf '#!/bin/bash\necho ran >> "%s/add-account-ran"\nexit 0\n' "$W" > "$R/add-account"; chmod +x "$R/add-account"
kc_raw_before="$(shasum -a 256 < "$W/kc.json")"
out="$(env -u CLAUDE_CODE_OAUTH_TOKEN -u CLAUDE_SECURESTORAGE_CONFIG_DIR -u CLAUDE_CONFIG_DIR -u CLAUDE_ACCOUNTS_TEST \
  PATH="$BIN:$PATH" HOME="$W/home" FAKE_KC="$W/kc.json" CLAUDE_ACCOUNTS_HOME="$W/pool" \
  CLAUDE_ACCOUNTS_THREADS_BIN="$TMP/fake-threads" FAKE_THREADS_CALLS="$W/threads-calls" \
  "$BASH5" "$R/claude-accounts" login a@x.test < /dev/null 2>&1)"; rc=$?
check "N6 refused (rc=$rc): a seam without the marker" '[ "$rc" -ne 0 ] && printf "%s" "$out" | grep -qF "is set but CLAUDE_ACCOUNTS_TEST=1 is not"'
check "N6 add-account never ran, store byte-identical" '[ ! -f "$W/add-account-ran" ] && [ "$(shasum -a 256 < "$W/kc.json")" = "$kc_raw_before" ]'
echo "── N6 (opposition): no seam set and no marker ⇒ not refused by the gate"
# FAKE_KC is itself on the list, so this arm's `security` is a stub that knows
# no item (exit 44): the login lands nothing and fails closed (rc 5), no read.
mkdir -p "$TMP/stub44"; printf '#!/bin/bash\nexit 44\n' > "$TMP/stub44/security"; chmod +x "$TMP/stub44/security"
W="$(world n6o)"; R="$W/repo/services/threads/scripts"; mkdir -p "$R" "$W/pool/accounts"
cp -p "$CA" "$R/claude-accounts"
printf '#!/bin/bash\necho ran >> "%s/add-account-ran"\nexit 0\n' "$W" > "$R/add-account"; chmod +x "$R/add-account"
out="$(env -u CLAUDE_CODE_OAUTH_TOKEN -u CLAUDE_SECURESTORAGE_CONFIG_DIR -u CLAUDE_CONFIG_DIR -u CLAUDE_ACCOUNTS_TEST \
  -u FAKE_KC PATH="$TMP/stub44:$BIN:$PATH" HOME="$W/home" CLAUDE_ACCOUNTS_HOME="$W/pool" \
  "$BASH5" "$R/claude-accounts" login a@x.test < /dev/null 2>&1)"; rc=$?
check "N6o the gate let it through: add-account ran (rc=$rc)" '[ -f "$W/add-account-ran" ] && ! printf "%s" "$out" | grep -qF "is set but CLAUDE_ACCOUNTS_TEST=1 is not"'

echo "──────────────────────────────────────────────"
echo "claude-accounts identity guard conformance: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
