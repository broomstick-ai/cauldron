#!/usr/bin/env python3
"""**THE FORCED-FAILURE MACHINE FOR ROUND FIVE'S ROW WORK.**

A green you have not forced to fail is not evidence. This file is in the repo
rather than in a report so the NEXT worker can re-run the table instead of
believing it — the shape `apps/aui/force_failures.py` established in `0077`.

Each mutation applies ONE exact source edit, runs ONE named test, restores the
source, and reports RED (the check bit) or GREEN (the check proves nothing).
A mutation whose anchor has moved prints `ANCHOR MOVED` and stops, rather than
silently proving nothing about a line that is no longer there.

**THE CONTROL IS THE POINT.** `CTRL` changes a comment and nothing else and
must come back GREEN. Without it, a script that reddens everything it touches
— a syntax error, a stale binary, a mistyped test name — would score a row of
triumphant proofs.

    ./force_failures.py            # the whole table
    ./force_failures.py M4 M7      # named mutations only

It builds into the ordinary package target directory of whatever checkout it
is run from. It never writes outside this repo and never talks to a daemon.
"""

import re
import subprocess
import sys
from pathlib import Path

CRATE = Path(__file__).resolve().parent
REPO = CRATE.parent.parent

INDEX = CRATE / "src/index.rs"
STORE = CRATE / "src/store.rs"
APP = CRATE / "src/app.rs"
KEYS = CRATE / "render/src/keys.rs"

# (name, file, old, new, package, test, expect_red)
MUTATIONS = [
    (
        "M1",
        "the registry read is replaced by the ~/minds derivation",
        INDEX,
        "match cauldron::Registry::load_or_derive(home) {",
        'match cauldron::Registry::load(home).map(|_| {\n        (\n            cauldron::Registry::derive_from_minds(home, "forced"),\n            cauldron::Source::DerivedFromMinds,\n        )\n    }) {',
        "tui",
        "index::tests::the_registry_is_the_list_and_the_minds_scan_is_gone",
        True,
    ),
    (
        "M2",
        "an unreadable registry falls back to the relic",
        INDEX,
        "vec![(cauldron::BUCKET.to_string(), cauldron::bucket_root(home))],",
        'cauldron::Registry::derive_from_minds(home, "forced")\n                    .view(home)\n                    .into_iter()\n                    .map(|r| (r.name, r.path))\n                    .collect(),',
        "tui",
        "index::tests::an_unreadable_registry_is_the_bucket_alone_and_carries_the_reason",
        True,
    ),
    (
        "M3",
        "a registered path that is gone is dropped from the list",
        INDEX,
        "                .map(|r| (r.name, r.path))\n                .collect();",
        "                .filter(|r| !r.missing)\n                .map(|r| (r.name, r.path))\n                .collect();",
        "tui",
        "index::tests::a_registered_path_that_is_gone_keeps_its_row",
        True,
    ),
    (
        "M4",
        "the unreadable-registry banner never fires",
        STORE,
        "if let index::RegistryState::Unreadable(why) = &store.registry {",
        "if let index::RegistryState::Unreadable(why) = &index::RegistryState::Read {",
        "tui",
        "store::tests::an_unreadable_registry_banner_outranks_even_a_dead_daemon",
        True,
    ),
    (
        "M5",
        "bare r goes back to Reload on the root",
        KEYS,
        "            mode: Mode::Projects,\n            intent: Intent::Rename,",
        "            mode: Mode::Projects,\n            intent: Intent::Reload,",
        "tui-render",
        "keys::tests::bare_r_renames_on_a_thread_surface_and_alt_r_is_still_reload_everywhere",
        True,
    ),
    (
        "M6",
        "alt-r loses its binding in the body modes",
        KEYS,
        # **THE ANCHOR CARRIES THE COMMENT ABOVE IT, and that is not
        # decoration.** `common_rows!` and `alt_common_rows!` now hold
        # BYTE-IDENTICAL reload rows — which is exactly what this round's
        # change produced — so the row alone matches TWICE and this machine
        # refuses rather than mutating the wrong macro. The doc comment that
        # explains why the bare `r` left is unique to `common_rows!`, and a
        # `short: "queue"` anchor does NOT work: a comment block sits between
        # that row and this one.
        "            // one_mode` being weakened to allow a shadow.\n            Binding {\n                keys: KeySet::These(&[Key::Alt('r')]),",
        "            // one_mode` being weakened to allow a shadow.\n            Binding {\n                keys: KeySet::These(&[Key::Alt('R')]),",
        "tui",
        "alt_r_still_reloads_and_does_not_rename",
        True,
    ),
    (
        "M7",
        "the shell stops handling Intent::Rename (the key still resolves)",
        APP,
        "            Intent::Rename => {\n                self.on_intent(Intent::Options);",
        "            Intent::Rename => {\n                #[allow(clippy::needless_return)]\n                return;\n                #[allow(unreachable_code)]\n                self.on_intent(Intent::Options);",
        "tui",
        "r_on_a_thread_row_opens_the_rename_field_prefilled_with_the_summary",
        True,
    ),
    (
        "M8",
        "the rename key fires whatever row is first instead of the rename row",
        STORE,
        "pub fn sheet_row_for(view: &SheetView, act: SheetAct) -> Option<usize> {\n    view.rows.iter().position(|r| r.act == act)",
        "pub fn sheet_row_for(view: &SheetView, act: SheetAct) -> Option<usize> {\n    let _ = act;\n    view.rows.first().map(|_| 0)",
        "tui",
        "r_on_a_thread_row_opens_the_rename_field_prefilled_with_the_summary",
        True,
    ),
    (
        "M9",
        "an Invalidate re-reads the store and never re-projects",
        APP,
        "        self.store.load_projects();\n        // **AND THE RE-READ HAS TO REACH THE SCREEN.**",
        "        self.store.load_projects();\n        if true {\n            return;\n        }\n        // **AND THE RE-READ HAS TO REACH THE SCREEN.**",
        "tui",
        "an_invalidate_puts_the_new_list_on_the_screen_and_not_only_in_the_store",
        True,
    ),
    (
        "CTRL",
        "a comment that changes nothing — MUST come back GREEN",
        INDEX,
        "/// [`discover_roots`] with the answer to",
        "/// (control mutation: this comment was edited and nothing else)\n/// [`discover_roots`] with the answer to",
        "tui",
        "index::tests::the_registry_is_the_list_and_the_minds_scan_is_gone",
        False,
    ),
]


def run_test(package: str, test: str) -> bool:
    """True when the named test PASSED."""
    r = subprocess.run(
        ["cargo", "test", "-p", package, test, "--", "--exact"]
        if "::" in test
        else ["cargo", "test", "-p", package, test],
        cwd=REPO,
        capture_output=True,
        text=True,
    )
    out = r.stdout + r.stderr
    # **AN EMPTY RUN IS NEVER A PASS.** `cargo test -p tui <filter>` runs every
    # test target in the package and most of them match nothing, so the tallies
    # are SUMMED rather than read one line at a time: a filter that matched
    # nowhere reports zero on both counts across every target, and a mutation
    # proves nothing against a test that never ran.
    tallies = re.findall(r"test result: \w+\. (\d+) passed; (\d+) failed", out)
    ran = sum(int(a) + int(b) for a, b in tallies)
    if not tallies or ran == 0:
        print(f"      NO SUCH TEST: {test}")
        return False
    if r.returncode != 0 and "error[" in out:
        print("      BUILD BROKE — a compile error is not evidence; rewrite the mutation")
        return False
    return r.returncode == 0


def main() -> int:
    wanted = set(sys.argv[1:])
    rows = [m for m in MUTATIONS if not wanted or m[0] in wanted]
    results = []
    for name, why, path, old, new, package, test, expect_red in rows:
        src = path.read_text()
        if src.count(old) != 1:
            print(f"{name}  ANCHOR MOVED in {path.name} ({src.count(old)} matches)")
            print(f"      {old.splitlines()[0][:78]}")
            return 2
        print(f"{name}  {why}")
        path.write_text(src.replace(old, new, 1))
        try:
            passed = run_test(package, test)
        finally:
            path.write_text(src)
        red = not passed
        ok = red == expect_red
        verdict = "RED" if red else "GREEN"
        print(f"      {verdict:5}  {test}   {'OK' if ok else '** THE CHECK PROVES NOTHING **'}")
        results.append((name, ok))

    bad = [n for n, ok in results if not ok]
    print()
    print(f"{len(results) - len(bad)}/{len(results)} mutations behaved as expected")
    if bad:
        print("FAILED: " + ", ".join(bad))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
