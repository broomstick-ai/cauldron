# shellcheck shell=bash
# ══════════════════════════════════════════════════════════════════════════
#  live-restart-guard.sh — THE GUARD ON THE OPERATOR'S LIVE SERVICE,
#  IN ONE FILE, SOURCED BY ALL THREE PROOFS.
# ══════════════════════════════════════════════════════════════════════════
#
# ── WHY THIS IS A SHARED FILE AND NOT THREE COPIES ────────────────────────
#
# Three times in this arc, one proof was migrated to new semantics and its two
# siblings were left behind. Most recently `prove-compile-gate`'s section 8b
# was DEAD and committed that way while `prove-wave1`'s equivalent carried a
# warning comment about exactly that failure. The guard below is the fourth
# thing that would have happened to, so it is not copied: `prove-wave1`,
# `prove-warnings` and `prove-compile-gate` all source this file, and a change
# here is a change in all three by construction rather than by diligence.
#
# ── WHAT IT REPLACED, AND WHY THAT GUARD COULD NOT WORK ───────────────────
#
# All three carried:
#
#     eq "the live daemon is the same process, never restarted" "$A_PIDS" "$B_PIDS"
#
# comparing `pgrep -x threads` before and after a run of 8–12 minutes. It is
# wrong in BOTH directions.
#
# **It goes red because the supervisor did its job.** Cauldron now hosts
# `threads` with `auto_restart` and `escalate = true`, so a legitimate re-pin
# inside the window restarts the service and the pid set changes. Measured, not
# hypothetical: `tts` went 38752 -> 80756 at 11:11:44 on 2026-10-09, PPID
# 15210, launched from
# `~/.cauldron/data/shared/builds/tts/2303409776b0-dev/tts`, absorbing a
# sibling's commit. `threads` held 11949 through every run, so it had not fired
# yet — the guard was one re-pin away from going red for the one reason that
# must never make a proof red. A proof that fails when the product works trains
# its reader to discount the red, and the only way to green it would have been
# to stop the operator's daemon.
#
# **And it goes GREEN when the service really did restart.** `threads`
# self-updates by `execv`: the process image is replaced while the pid, the
# ppid and the reported start time all stay the same. So "did this process
# restart" is NOT ANSWERABLE FROM THE PROCESS TABLE AT ALL. That is the deeper
# reason the guard could not be repaired by comparing something else about the
# process — there is nothing about the process to compare.
#
# A pid comparison also cannot distinguish *the proof restarted it* from *the
# supervisor re-pinned it*, because the observable is identical.
#
# ── WHAT IT ASSERTS INSTEAD ───────────────────────────────────────────────
#
# The remedy is on cauldron's side. The supervisor knows every act it
# performed and now records it: `libs/env/src/supervisions.rs`, one JSONL line
# per act in the environment's own data root, naming the service, the act, the
# version, the instance's BOOT TOKEN, the acting process and its cauldron ROOT,
# and — the field the service's own `restarts.jsonl` cannot have — WHY.
#
# So the claim becomes the honest one:
#
#   1. NO ACT ON THE OPERATOR'S SERVICE WAS PERFORMED BY A SUPERVISOR OF OURS.
#      Scoped on `by.root`, the cauldron root the acting process serves — the
#      same discrimination `prove-warnings`' section 6 makes on
#      `CAULDRON_HOME`, and for the same reason: a process name confuses one
#      root with another and a path does not. This is a claim about THIS PROOF,
#      which is the only thing this proof is in a position to claim.
#
#   2. AND NOTHING HAPPENED TO IT THAT NOTHING ACCOUNTS FOR. A pid change with
#      a supervision record behind it is the supervisor working. A pid change
#      with NO record behind it, or with no ledger to look in, is unexplained
#      and that is what goes red.
#
# THREE CAUSES SHARE ONE AFTERMATH, which is why (1) is read over his whole
# environment rather than over one service. `com.broomstick.cauldron.plist`
# runs `cauldron daemon` under `KeepAlive = true, ThrottleInterval = 10`, and
# the daemon exits on purpose when its launch image changes, so after any of —
# launchd restarting it after a SELF-UPDATE, launchd restarting it after a
# CRASH or at login, or the supervisor RE-PINNING a service — both the service
# and the supervisor are up and the process table looks identical. The ledger
# separates them: `exit/self_update` then `hold/started` is the first,
# `hold/started` with no preceding exit is the second, `stop/repin` plus
# `launch/repin` is the third. The supervisor's own records are filed under the
# SUPERVISOR's name, so a service-scoped read cannot see the first two at all.
# `prove-wave1` 9c is where that is forced.
#
# NOTHING HERE IDENTIFIES ANYTHING BY PROCESS NAME, deliberately. There are two
# unrelated binaries called `cauldron` on this machine — cauldron itself, and a
# project registry vendored at `harness/libs/cauldron/src/bin/cauldron.rs`,
# copied across four harness worktrees. `By::root` is the ownership field and
# it is the cauldron ROOT the acting process serves; an instance is identified
# by a BOOT TOKEN. The one name-matching thing left in this file is
# `pgrep -x $LIVE_SERVICE`, and it is the CORROBORATING half, never the claim.
#
# ── AND THE SENTINEL, WITHOUT WHICH BOTH OF THOSE ARE WORTHLESS ───────────
#
# Both are NEGATIVE assertions over a scan, and an empty scan is
# indistinguishable from a scan that cannot see anything — a mis-spelled
# filter, a jq path that never matches, a verb that silently answers nothing.
# Fifty-eight probes that could not fail have been found in this arc; the
# generalization is "a check that does not run, reporting as a result".
#
# So the same verb, with the same `--service`/`--since`/`--root` filters and
# the same jq, is run against THIS PROOF'S OWN SANDBOX LEDGER, which this run
# has written to by starting services. It must answer with records, every one
# of them rooted at the sandbox; and filtered to the operator's root it must
# answer with none. One ledger, two root filters, opposite answers — so the
# lens is demonstrated to be a lens in the same act in which it is relied on.
#
# ── AND AN ABSENT LEDGER IS NOT AN EMPTY ONE ──────────────────────────────
#
# `cauldron supervisions` exits 5 when there is no ledger at all, and this
# guard reads `.exists` rather than `.matched`. Until the operator's daemon
# picks up these bytes there IS no live ledger — he runs pid 15210 from an
# earlier commit — so that is the normal answer today and the guard says so in
# words instead of reporting the absence as a green.
#
# ── CONTRACT ──────────────────────────────────────────────────────────────
#
# The sourcing script must already define: $CAULDRON, $PROOF_ROOT, and the
# assertion helpers. ONLY THE INTERSECTION of what the three define may be
# used here — ok, bad, eq, ne, yes, has — because `prove-compile-gate` has no
# `no` and the first version of this file leaned on it and aborted that script
# at its first use. Call `restart_guard_before` in the section 0 snapshot and
# `restart_guard_after` in the final section.

# The operator's live environment. Not derived from $HOME: a guard about HIS
# daemon must not follow a $HOME that a proof may have repointed, and three
# sections of these proofs do repoint it. Not written literally either: a
# literal names one user's home, and when REL-4.S4 replaced it with a
# placeholder that exists nowhere, every read below answered "no ledger, no
# acts" and the guard passed vacuously (REL-4.S4-I F2). So it is DERIVED from
# the password database — `~` expanded with an EMPTY environment, which a
# POSIX shell resolves from passwd — the same derivation `env::guard` uses.
# `scripts/test-live-restart-guard` proves both halves.
LIVE_ROOT="$(env -i /bin/sh -c 'echo ~')/.cauldron"
case "$LIVE_ROOT" in
  /?*/.cauldron) ;;
  *) printf 'live-restart-guard: cannot derive the operator home (got %s)\n' "$LIVE_ROOT" >&2
     return 1 2>/dev/null || exit 1 ;;
esac
LIVE_ENV=prod
LIVE_SERVICE=threads

restart_guard_before() {
  if [ -z "${CAULDRON:-}" ] || [ -z "${PROOF_ROOT:-}" ]; then
    printf 'live-restart-guard: sourced without $CAULDRON or $PROOF_ROOT\n' >&2
    return 1
  fi
  # THE WINDOW OPENS HERE, in cauldron's own timestamp format so `--since` is
  # an exact string comparison and not a date library: RFC3339, UTC,
  # microseconds, fixed width. `supervisions::now_rfc3339` writes the same
  # shape, which is what makes a lexical compare total.
  #
  # `date` TRUNCATES to the second and this zero-fills the microseconds, so the
  # boundary admits every act that happened earlier in the same second. That is
  # deliberate HERE and it is the safe direction: a window one second too WIDE
  # catches more acts than it needs to, so the "accounted for" verdict below
  # has more evidence rather than less, and the "none of ours" claim is made
  # over a superset. A window too NARROW would silently miss an act, which is
  # the direction that produces a false green. `prove-wave1`'s section 9b, which
  # needs EXACT counts rather than a superset, sleeps a second first — and it
  # found this by going red.
  RG_SINCE=$(date -u +%Y-%m-%dT%H:%M:%S.000000+00:00)
  # `|| true` is LOAD-BEARING: `pgrep` exits 1 when nothing matches, and under
  # `set -e` with `pipefail` that status kills the script before one assertion
  # has run. Owed to whoever next edits this, and paid here.
  RG_PIDS_BEFORE=$( (pgrep -x "$LIVE_SERVICE" || true) | sort | tr '\n' ' ')
  printf 'live guard  window opens %s\n' "$RG_SINCE"
  printf 'live guard  %s pids before: [%s]\n' "$LIVE_SERVICE" "${RG_PIDS_BEFORE% }"
  echo "            (the pid set is CORROBORATION, never the claim: this daemon"
  echo "             self-updates by execv and keeps its pid across a real restart.)"
}

# One `cauldron supervisions --json` read. $1 = CAULDRON_HOME, $2 = env,
# and any further arguments are passed to the verb.
_rg_read() {
  local home="$1" e="$2"
  shift 2
  env CAULDRON_HOME="$home" CAULDRON_ENV="$e" "$CAULDRON" supervisions --json "$@" 2>/dev/null \
    || true
}

restart_guard_after() {
  local pids_after verdict live own own_rooted own_foreign live_mine live_exists live_acts
  pids_after=$( (pgrep -x "$LIVE_SERVICE" || true) | sort | tr '\n' ' ')
  printf 'live guard  %s pids after:  [%s]\n' "$LIVE_SERVICE" "${pids_after% }"

  # ── THE SENTINEL, FIRST, SO NOTHING BELOW IS TRUSTED BEFORE IT ──────────
  #
  # Our OWN sandbox ledger, through the identical verb and filters. This run
  # started services, so there is something to find; if there is not, the
  # query is broken and every empty answer below means nothing.
  own=$(_rg_read "$PROOF_ROOT" dev --since "$RG_SINCE")
  own_rooted=$(jq -r '[.records[]|select(.by.root=="'"$PROOF_ROOT"'")]|length' <<<"$own" 2>/dev/null || echo 0)
  own_foreign=$(jq -r '[.records[]|select(.by.root!="'"$PROOF_ROOT"'")]|length' <<<"$own" 2>/dev/null || echo 0)
  printf 'live guard  our own ledger this window: %s act(s), %s of them ours\n' \
    "$(jq -r '.matched // 0' <<<"$own" 2>/dev/null || echo 0)" "$own_rooted"
  ne "SENTINEL: our own supervision ledger has acts in this window, so the query runs" \
     "$own_rooted" 0
  eq "SENTINEL: and every act in it was performed by a supervisor rooted at THIS sandbox" \
     "$own_foreign" 0
  # THE LENS IS A LENS: the same ledger, filtered to the operator's root, is
  # empty. Without this the \`by.root\` scope below could be a filter that
  # matches nothing whatever you give it.
  eq "SENTINEL: and the same ledger filtered to the operator's root is EMPTY, so the scope is real" \
     "$(jq -r '.matched // 0' <<<"$(_rg_read "$PROOF_ROOT" dev --since "$RG_SINCE" --root "$LIVE_ROOT")" 2>/dev/null || echo 0)" \
     0

  # ── THE CLAIM: NOTHING OF OURS TOUCHED HIS SERVICE ─────────────────────
  live=$(_rg_read "$LIVE_ROOT" "$LIVE_ENV" --service "$LIVE_SERVICE" --since "$RG_SINCE")
  live_exists=$(jq -r '.exists // false' <<<"$live" 2>/dev/null || echo false)
  live_acts=$(jq -r '.matched // 0' <<<"$live" 2>/dev/null || echo 0)
  live_mine=$(jq -r '[.records[]|select(.by.root=="'"$PROOF_ROOT"'")]|length' <<<"$live" 2>/dev/null || echo 0)
  printf 'live guard  his ledger: exists=%s  acts on %s this window=%s  of them ours=%s\n' \
    "$live_exists" "$LIVE_SERVICE" "$live_acts" "$live_mine"
  if [ "$live_acts" != 0 ]; then
    jq -r '.records[]|"              \(.at)  \(.act)  why=\(.why)  pid=\(.pid)  v=\(.version)  root=\(.by.root)"' \
      <<<"$live" 2>/dev/null || true
  fi
  eq "no act on the operator's \`$LIVE_SERVICE\` was performed by a supervisor OF OURS" \
     "$live_mine" 0

  # ── AND THE SAME CLAIM OVER HIS WHOLE ENVIRONMENT, NOT ONE SERVICE ─────
  #
  # A supervisor's own lifecycle records (`hold`/`started`, `exit`/
  # `self_update`) are filed under the SUPERVISOR's service name, so the
  # `--service $LIVE_SERVICE` read above cannot see them — and those are
  # exactly the records that distinguish a launchd restart after a
  # self-update from a crash-restart from a service re-pin. A claim scoped to
  # one service would therefore have been silent about the two causes that do
  # not touch a service at all.
  #
  # So the claim is widened: nothing of ours acted on ANYTHING in his
  # environment. Strictly stronger, same lens, one assertion.
  local live_any live_any_mine
  live_any=$(_rg_read "$LIVE_ROOT" "$LIVE_ENV" --since "$RG_SINCE")
  live_any_mine=$(jq -r '[.records[]|select(.by.root=="'"$PROOF_ROOT"'")]|length' <<<"$live_any" 2>/dev/null || echo 0)
  printf 'live guard  his whole environment this window: %s act(s), %s of them ours\n' \
    "$(jq -r '.matched // 0' <<<"$live_any" 2>/dev/null || echo 0)" "$live_any_mine"
  # His supervisor's own life, named where a reader sees it. Not asserted on:
  # whether launchd replaced his daemon in this window is his business and not
  # this proof's doing. It is PRINTED because a reader of a green needs to know
  # which of the three causes was live while the green was being earned.
  jq -r '.records[]|select(.act=="hold" or .act=="exit")|"              his supervisor: \(.at)  \(.act)/\(.why)  boot=\(.boot)"' \
    <<<"$live_any" 2>/dev/null || true
  eq "and no act on ANY service in his environment was ours either" "$live_any_mine" 0

  # ── THE RED FOR THAT GREEN, IN THE FILE, ON EVERY RUN ──────────────────
  #
  # The assertion above is a ZERO, and a zero from this scan is
  # indistinguishable from a scan that would answer zero whatever it were
  # pointed at. The operator's own ledger cannot be made to go red — planting
  # a record under ~/.cauldron is forbidden and would be a lie in his store
  # besides — so the SAME CODE PATH is pointed at a ledger in THIS sandbox
  # that plays the part, and it is built to contain exactly what the green
  # denies.
  #
  # TWO records, deliberately: one claiming OUR root and one claiming HIS. A
  # single planted record would be caught by a filter that matches everything
  # as readily as by one that works. The lens has to return ONE out of TWO.
  local fake="$PROOF_ROOT/data/guard-red" planted planted_all at
  mkdir -p "$fake/data/prod"
  # `.500000` in the CURRENT second, so it is strictly after $RG_SINCE, which
  # was taken at the start of an earlier or equal second.
  at=$(date -u +%Y-%m-%dT%H:%M:%S.500000+00:00)
  {
    printf '{"at":"%s","env":"prod","service":"%s","act":"stop","why":"requested",' \
      "$at" "$LIVE_SERVICE"
    printf '"version":"planted","boot":"","pid":11949,'
    printf '"by":{"pid":99999,"since":"","commit":"planted","root":"%s"},' "$PROOF_ROOT"
    printf '"detail":"the guard'"'"'s own red: an act on his service, by a supervisor of OURS"}\n'
    printf '{"at":"%s","env":"prod","service":"%s","act":"launch","why":"repin",' \
      "$at" "$LIVE_SERVICE"
    printf '"version":"planted","boot":"","pid":11950,'
    printf '"by":{"pid":15210,"since":"","commit":"planted","root":"%s"},' "$LIVE_ROOT"
    printf '"detail":"and one by HIS supervisor, which the lens must leave alone"}\n'
  } > "$fake/data/prod/supervisions.jsonl"
  local red
  red=$(_rg_read "$fake" prod --service "$LIVE_SERVICE" --since "$RG_SINCE")
  planted_all=$(jq -r '.matched // 0' <<<"$red" 2>/dev/null || echo 0)
  planted=$(jq -r '[.records[]|select(.by.root=="'"$PROOF_ROOT"'")]|length' <<<"$red" 2>/dev/null || echo 0)
  printf 'live guard  THE RED: a planted ledger with %s acts on %s, %s of them ours\n' \
    "$planted_all" "$LIVE_SERVICE" "$planted"
  ne "THE RED: the identical scan DOES find an act of ours when a ledger names one" \
     "$planted" 0
  eq "and it finds EXACTLY the one of two that is ours, so the root lens is not matching everything" \
     "$planted/$planted_all" "1/2"
  rm -rf "$fake"

  # ── AND NOTHING UNEXPLAINED HAPPENED TO IT ─────────────────────────────
  #
  # The verdict is computed as a WORD rather than asserted as a pid equality,
  # because the four cases are not one claim. A pid change the supervisor
  # accounts for is the supervisor working and must be green; a pid change
  # nothing accounts for is the thing to catch.
  if [ "$pids_after" = "$RG_PIDS_BEFORE" ]; then
    if [ "$live_exists" = true ] && [ "$live_acts" != 0 ]; then
      verdict="unchanged, and his ledger names $live_acts act(s) — an execv restart keeps the pid"
    else
      verdict="unchanged"
    fi
  elif [ "$live_exists" != true ]; then
    verdict="UNEXPLAINED: the pid set changed and there is NO supervision ledger to attribute it to"
  elif [ "$live_acts" != 0 ]; then
    verdict="accounted for by $live_acts supervision record(s) the supervisor wrote itself"
  else
    verdict="UNEXPLAINED: the pid set changed and the supervisor recorded no act on this service"
  fi
  printf 'live guard  verdict: %s\n' "$verdict"
  # Asserted with `eq` against the EMPTY STRING rather than with `no` over a
  # grep, for two reasons. `prove-compile-gate` does not define `no` — found
  # by `no: command not found` aborting it at this line — and the three
  # scripts' helper sets are not identical, so a shared file must lean on the
  # intersection: ok/bad/eq/ne/yes/has. And on failure this prints the WHOLE
  # verdict, which names which of the two unexplained cases it was; the grep
  # form printed only that a grep succeeded.
  local unexplained=""
  case "$verdict" in UNEXPLAINED*) unexplained="$verdict" ;; esac
  eq "nothing happened to the operator's \`$LIVE_SERVICE\` that nothing accounts for" \
     "$unexplained" ""

  if [ "$live_exists" != true ]; then
    echo "            NOTE, AND IT IS NOT A PASS: there is no supervision ledger under"
    echo "            $LIVE_ROOT/data/$LIVE_ENV yet, because the operator's running supervisor"
    echo "            predates it. So the claim above rests on the pid set, which this daemon's"
    echo "            execv can leave untouched across a real restart. It becomes PROOF the"
    echo "            first time he runs a cauldron carrying \`supervisions.jsonl\`; until then"
    echo "            the sentinels say the query works and this says what it cannot yet see."
  fi
}
