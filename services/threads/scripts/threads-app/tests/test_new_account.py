"""`n` — the new-account door, and the validation in front of it.

WHY THIS IS A TEST AND NOT A GLANCE. `label_refusal` mirrors two other
implementations of the same rule — `accounts::validate_account_name`
(lib/threads/src/accounts.rs) and `valid_label` in scripts/claude-accounts — and
the cost of the three drifting apart is paid at the worst possible moment: the
operator types a name, this modal accepts it, `add-account` creates a slot, he
opens a browser, signs in, and only THEN does the script refuse. Every case
below is one the other two implementations also refuse.

Pure: no screen, no daemon, no clock.
"""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from threads_tui.login import MAX_LABEL, label_refusal

FAILURES = []


def check(name, got, want_substr):
    if want_substr is None:
        ok = got is None
        detail = f"expected acceptance, got {got!r}"
    else:
        ok = got is not None and want_substr in got
        detail = f"expected {want_substr!r} in refusal, got {got!r}"
    print(f"  {'ok  ' if ok else 'FAIL'} {name}")
    if not ok:
        print(f"       {detail}")
        FAILURES.append(name)


print("label_refusal")
check("empty is refused", label_refusal(""), "type a name")
check("whitespace-only is refused", label_refusal("   "), "type a name")
check("a space is refused", label_refusal("work 2"), "use letters")
check("punctuation is refused", label_refusal("work!"), "use letters")
check("non-ascii is refused", label_refusal("wörk"), "use letters")
check("slash is refused (it would be a path)", label_refusal("a/b"), "use letters")
check("dot is refused (.. would escape the dir)", label_refusal(".."), "use letters")
check(f"over {MAX_LABEL} chars is refused", label_refusal("a" * (MAX_LABEL + 1)), "too long")
check(f"exactly {MAX_LABEL} chars is accepted", label_refusal("a" * MAX_LABEL), None)

# The machine login is not a slot, and the message must point at the key that
# DOES change it rather than just saying no.
check("'default' is refused", label_refusal("default"), "machine login")
check("'default' names the right key", label_refusal("default"), "Press s")
# accounts/backups/ holds the live-login undo snapshots.
check("'backups' is refused", label_refusal("backups"), "reserved")

# A collision is refused HERE rather than silently re-logging the slot in.
# `add-account`'s own law is that prior state never refuses a login, so it would
# have gone ahead — from a door labelled "new account", which is a surprise.
check("an existing label is refused", label_refusal("work", ["work", "home"]), "already exists")
check("the refusal names the other door", label_refusal("work", ["work"]), "press l")
check("a free label is accepted", label_refusal("work-2", ["work", "home"]), None)

print("\nshapes that must be accepted")
for good in ("work", "work-2", "work_2", "a", "A1", "alice-example-com", "9"):
    check(f"{good!r}", label_refusal(good), None)

print("\nsurrounding whitespace is trimmed, not refused")
check("' work '", label_refusal("  work  "), None)

print()
if FAILURES:
    print(f"test_new_account: {len(FAILURES)} FAILED: {', '.join(FAILURES)}")
    sys.exit(1)
print("test_new_account: all good")
