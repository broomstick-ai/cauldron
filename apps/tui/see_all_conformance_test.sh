#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  `see all` CONFORMANCE — THE INVERTED ANCHOR (`0133` I-15)
#
#  Band expansion is GONE. The design's fifteenth invariant is the only one in
#  the set whose shape is inverted: every other anchor in this crate FAILS when
#  a string it expects has MOVED, and this one fails when a string it forbids is
#  STILL THERE. So the label is `ANCHOR-PRESENT`, not `ANCHOR-MISSING`, and the
#  presence is the defect.
#
#  **WHY A GREP AND NOT A TEST.** A deleted name cannot be asserted about from
#  inside the crate: there is nothing left to call, and a Rust test that
#  compiles is a test whose subject compiles too. What has to be true is a
#  property of the SOURCE — that no reader of this tree can find a half-deleted
#  expansion path and build on it — and the only thing that can check that is
#  something reading the source. `0072`'s expansion build touched eleven
#  identifiers across four files; leaving one variant behind is the exact
#  failure this exists to catch, because one surviving variant is a dead arm
#  under a doc comment that argues for it.
#
#  It BUILDS NOTHING, so it needs no scratch target directory and cannot touch
#  a live binary. It is safe to run in any tree, at any time.
#
#  **AND IT FORCES ITSELF TO FAIL.** `--self-test` writes each forbidden name
#  into a scratch file inside a throwaway tree and asserts the detector fires on
#  it — because a grep that finds nothing and a grep that is looking in the
#  wrong place are indistinguishable from their output (`0065`). Run it; a
#  green here that has not been made to lie is evidence-shaped, not evidence.
#
#  Exit 0 green · 1 an anchor is present · 2 the self-test did not fire.
# ─────────────────────────────────────────────────────────────────────────────
set -u

CRATE_DIR="$(cd "$(dirname "$0")" && pwd)"

# ── WHAT MUST NOT COMPILE ────────────────────────────────────────────────────
# `T-32`'s whole band-expansion build, by name. Word-boundary matched, so a
# rename that merely re-cased something does not slip past and an unrelated
# identifier that merely CONTAINS one of these does not raise a false alarm.
#
#   ShowLess          the `show less — back to 10` row, both enums
#   expand_band       the shell's act behind `→` on a band header
#   collapse_band     its inverse, behind `←`
#   band_expanded     the shell's per-band flag and the projection's reader
#   BAND_EXPANDED_MAX the 500-row DRAWING bound
#
# `show_less_words`, `collapse_under_cursor`, `band_under_cursor` and
# `band_expandable` went with them and are listed too: the design named five,
# and the four the design did not name are in the same commit and would rot the
# same way.
FORBIDDEN=(
    ShowLess
    expand_band
    collapse_band
    band_expanded
    BAND_EXPANDED_MAX
    show_less_words
    collapse_under_cursor
    band_under_cursor
    band_expandable
)

# WHERE TO LOOK: the crate's Rust, and nothing generated. `target/` holds
# compiled artifacts of OTHER commits and `.mind/` holds the artifacts that
# DESCRIBE this deletion — an artifact that says "`expand_band` is deleted" is
# not a surviving `expand_band`, and a guard that could not tell those apart
# would be unusable the day it was written.
#
# **THIS SCRIPT ITSELF IS EXCLUDED, and that exclusion is load-bearing**: the
# list above is nine literal occurrences of exactly what it forbids.
# **COMMENT LINES ARE NOT HITS, and that is the invariant read precisely.** I-15
# says nothing named by the expansion build may COMPILE. A doc comment does not
# compile, and this crate's comments are where its own history is written — the
# very paragraphs explaining that `BAND_EXPANDED_MAX` is retired name it four
# times, and a guard that called those a violation would be a guard that forbade
# saying what was deleted. So a line whose first non-blank characters are `//`
# (which is `///` and `//!` too) is skipped, and a line of CODE with a trailing
# comment is not — the self-test below plants exactly that case.
scan() { # <needle>  ->  matching "<file>:<line>:<text>" lines, code only
    grep -rn --include='*.rs' -w -e "$1" \
        "$CRATE_DIR/src" "$CRATE_DIR/../../libs/tui-render/src" "$CRATE_DIR/../../libs/tui-render/tests" \
        "$CRATE_DIR/examples" "$CRATE_DIR/tests" 2>/dev/null \
        | grep -v -E '^[^:]*:[0-9]+:[[:space:]]*//'
}

check() { # <root-label>  ->  0 clean, 1 something is present
    local bad=0 name hits
    for name in "${FORBIDDEN[@]}"; do
        hits="$(scan "$name")"
        if [ -n "$hits" ]; then
            printf '  \033[31mANCHOR-PRESENT\033[0m %s\n' "$name"
            printf '%s\n' "$hits" | sed 's/^/        /'
            bad=1
        else
            printf '  \033[32mgone\033[0m           %s\n' "$name"
        fi
    done
    return $bad
}

# ── THE SELF-TEST: make the detector lie, then watch it stop ─────────────────
# It plants one forbidden name in a scratch file inside a COPY of the scan roots
# and asserts a hit. A detector that scans the wrong directory, or whose
# `-w` makes a name unmatchable, reports a triumphant green over a tree full of
# dead code — which is `0065`'s defect exactly, one level up.
if [ "${1:-}" = "--self-test" ]; then
    printf '\033[1mSELF-TEST — the detector must fire on a planted name\033[0m\n'
    tmp="$(mktemp -d "${TMPDIR:-/tmp}/see-all-anchor.XXXXXX")"
    trap 'rm -rf "$tmp"' EXIT INT TERM
    mkdir -p "$tmp/src" "$tmp/render/src" "$tmp/render/tests" "$tmp/examples" "$tmp/tests"
    fail=0
    for name in "${FORBIDDEN[@]}"; do
        # One per run, in a different scan root each time, so every root is
        # proven reachable rather than just the first.
        for root in src render/src render/tests examples tests; do
            printf 'fn f() { let _ = %s; }\n' "$name" > "$tmp/$root/planted.rs"
        done
        out="$(CRATE_DIR="$tmp"; scan "$name")"
        roots="$(printf '%s\n' "$out" | grep -c 'planted.rs')"
        if [ "$roots" -ne 5 ]; then
            printf '  \033[31mDID NOT FIRE\033[0m   %s (found in %s of 5 roots)\n' "$name" "$roots"
            fail=1
        else
            printf '  \033[32mfired\033[0m          %s (all 5 scan roots)\n' "$name"
        fi
        rm -f "$tmp"/src/planted.rs "$tmp"/render/src/planted.rs \
              "$tmp"/render/tests/planted.rs "$tmp"/examples/planted.rs \
              "$tmp"/tests/planted.rs
    done
    # And it must NOT fire on a COMMENT — the crate's own account of this
    # deletion names every one of these — while it MUST still fire on code that
    # carries a trailing one, or "skip comments" would become "skip any line
    # with a comment on it" and the guard would be trivial to hide from.
    for root in src render/src render/tests examples tests; do
        printf '/// `expand_band` used to live here.\n//! and `BAND_EXPANDED_MAX` bounded it\n    // ShowLess was its inverse\n' > "$tmp/$root/planted.rs"
    done
    for name in expand_band BAND_EXPANDED_MAX ShowLess; do
        if [ -n "$(CRATE_DIR="$tmp"; scan "$name")" ]; then
            printf '  \033[31mFALSE POSITIVE\033[0m %s matched a comment\n' "$name"
            fail=1
        else
            printf '  \033[32mprecise\033[0m        %s in a comment is not a hit\n' "$name"
        fi
    done
    for root in src render/src render/tests examples tests; do
        printf 'fn f() { expand_band(b); } // and a trailing comment\n' > "$tmp/$root/planted.rs"
    done
    if [ -z "$(CRATE_DIR="$tmp"; scan expand_band)" ]; then
        printf '  \033[31mDID NOT FIRE\033[0m   code with a trailing comment was skipped whole\n'
        fail=1
    else
        printf '  \033[32mfired\033[0m          code with a trailing comment is still a hit\n'
    fi

    # And it must NOT fire on a name that merely contains a forbidden one, or
    # the guard would refuse every future identifier with `band` in it.
    for root in src render/src render/tests examples tests; do
        printf 'fn f() { let _ = expanded_band_cost_of_nothing; }\n' > "$tmp/$root/planted.rs"
    done
    if [ -n "$(CRATE_DIR="$tmp"; scan expand_band)" ]; then
        printf '  \033[31mFALSE POSITIVE\033[0m expand_band matched inside a longer identifier\n'
        fail=1
    else
        printf '  \033[32mprecise\033[0m        a longer identifier is not a hit\n'
    fi
    printf '\n\033[1mresult\033[0m\n'
    if [ "$fail" -ne 0 ]; then
        printf '  \033[31mFAIL\033[0m — the detector cannot be trusted, so its green means nothing\n'
        exit 2
    fi
    printf '  \033[32mPASS\033[0m — the detector fires, and only on what it names\n'
    exit 0
fi

printf '\033[1mI-15 — BAND EXPANSION MUST NOT COMPILE\033[0m\n'
printf '  scanning %s\n\n' "$CRATE_DIR"
if check; then
    printf '\n\033[1mresult\033[0m\n'
    printf '  \033[32mPASS\033[0m — nothing named by `T-32`'"'"'s expansion build survives\n'
    printf '  (run with --self-test to make this detector lie before you trust it)\n'
    exit 0
fi
printf '\n\033[1mresult\033[0m\n'
printf '  \033[31mFAIL\033[0m — band expansion is half-deleted. A surviving variant is a\n'
printf '  dead arm under a doc comment that argues for it; delete it rather than\n'
printf '  leaving the next reader to build on it.\n'
exit 1
