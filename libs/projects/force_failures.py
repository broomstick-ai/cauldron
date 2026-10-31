#!/usr/bin/env python3
"""FORCE EVERY GREEN IN `libs/projects` TO FAIL.

    python3 libs/projects/force_failures.py     (from the workspace root)

A green you have not forced to fail is not evidence. This is the machine that
forces them, kept in the repo rather than described in a report, so the next
worker can re-run it instead of believing a table.

For each mutation: apply an exact string replacement to the crate source, run
the named test, restore the pristine source, and report whether the test went
RED. A mutation that leaves the test green is a test that does not cover the
behaviour it claims to cover.

The last entry is a deliberate NO-OP. It must come back "DID NOT BITE" --
that is the witness that this script can tell a biting mutation from a
non-biting one, rather than printing RED for everything it runs.
"""
import os, shutil, subprocess, sys, tempfile

CRATE = os.path.dirname(os.path.abspath(__file__))
SRC = [f"{CRATE}/src/lib.rs", f"{CRATE}/src/snapshot.rs", f"{CRATE}/src/bin/projects.rs"]

MUTATIONS = [
    ("F1 idempotence: delete the stat short-circuit", "src/lib.rs",
     "        if final_path.exists() {\n            return Ok(Bootstrapped::AlreadyPresent(load_existing(home)?));\n        }",
     "        if false {\n            return Ok(Bootstrapped::AlreadyPresent(load_existing(home)?));\n        }",
     "bootstrap", "the_second_bootstrap_is_a_stat_and_a_return_even_when_minds_cannot_be_read"),

    ("F2 halfway: skip the stray sweep", "src/lib.rs",
     "        sweep_stray_temps(&dir, STRAY_TEMP_AGE);",
     "        let _ = STRAY_TEMP_AGE;",
     "bootstrap", "a_crash_between_the_temp_and_the_link_leaves_no_registry_and_the_next_run_is_correct"),

    ("F2b sweep: make it sweep live writers' temps too", "src/lib.rs",
     "const STRAY_TEMP_AGE: Duration = Duration::from_secs(60);",
     "const STRAY_TEMP_AGE: Duration = Duration::from_secs(0);",
     "bootstrap", "a_fresh_temp_is_not_swept_because_it_might_belong_to_a_live_writer"),

    ("F3 creation race: hard_link -> rename, so losers overwrite the winner", "src/lib.rs",
     "        match fs::hard_link(&tmp, &final_path) {",
     "        match fs::rename(&tmp, &final_path) {",
     "concurrency", "n_processes_bootstrapping_one_home_produce_one_document_and_one_winner"),

    ("F4 the lock: drop the guard, leaving an unsynchronised read-modify-write", "src/lib.rs",
     "        let mut lock = Lock::acquire(home)?;\n        let _guard = lock.write()?;\n\n        let mut reg = match Registry::load(home) {",
     "        let mut lock = Lock::acquire(home)?;\n        drop(lock.write()?);\n\n        let mut reg = match Registry::load(home) {",
     "concurrency", "four_processes_doing_a_hundred_registrations_lose_nothing"),

    ("F4b the lock, in-process", "src/lib.rs",
     "        let mut lock = Lock::acquire(home)?;\n        let _guard = lock.write()?;\n\n        let mut reg = match Registry::load(home) {",
     "        let mut lock = Lock::acquire(home)?;\n        drop(lock.write()?);\n\n        let mut reg = match Registry::load(home) {",
     "concurrency", "eight_threads_in_one_process_also_lose_nothing"),

    ("F5 tearing: write straight to projects.json, no temp and no rename", "src/lib.rs",
     "        let tmp = temp_path(dir);\n        {\n            let mut f = fs::File::create(&tmp)?;",
     "        let tmp = dir.join(\"projects.json\");\n        {\n            let mut f = fs::File::create(&tmp)?;",
     "concurrency", "a_reader_racing_a_hundred_writes_never_sees_a_torn_document"),

    ("F6 fail-open: treat an unreadable registry as an empty one", "src/lib.rs",
     "            Err(e) => return Err(UpdateError::Load(e)),",
     "            Err(_) => Registry::empty(),",
     "registry", "an_unparseable_registry_is_an_error_and_a_write_against_it_is_refused"),

    ("F7 the move_pin defect: a stale `before` falls to the end", "src/lib.rs",
     "            if !self.projects.iter().any(|e| e.name == b) {\n                return Err(stale_before(b));\n            }",
     "            let _ = b;",
     "registry", "move_before_refuses_a_stale_neighbour_instead_of_sending_the_row_to_the_end"),

    ("F8 de-register deletes the project directory", "src/lib.rs",
     "        match self.projects.iter().position(|e| e.name == name) {\n            Some(i) => Ok(self.projects.remove(i)),",
     "        match self.projects.iter().position(|e| e.name == name) {\n            Some(i) => { let _ = fs::remove_dir_all(&self.projects[i].path); Ok(self.projects.remove(i)) },",
     "registry", "de_register_leaves_the_directory_its_mind_and_its_threads_alone"),

    ("F9 the derivation normalizes: type_3 becomes type-3", "src/lib.rs",
     "                projects.push(Entry {\n                    name,",
     "                projects.push(Entry {\n                    name: name.replace('_', \"-\"),",
     "bootstrap", "bootstrapped_names_are_the_basename_verbatim"),

    ("F10 dangling: remove ONLY the canonicalize-failure skip (one of two layers)", "src/lib.rs",
     "                let path = if is_link {\n                    match fs::canonicalize(&entry_path) {\n                        Ok(p) => p,",
     "                let path = if is_link && false {\n                    match fs::canonicalize(&entry_path) {\n                        Ok(p) => p,",
     "bootstrap", "dangling_links_are_skipped_so_nothing_appears_that_was_not_there_yesterday"),

    ("F10b dangling: remove BOTH layers - the follow-metadata gate and the canonicalize skip", "src/lib.rs",
     "                match fs::metadata(&entry_path) {\n                    Ok(md) if md.is_dir() => {}\n                    _ => continue,\n                }\n                let is_link = fs::symlink_metadata(&entry_path)\n                    .map(|m| m.file_type().is_symlink())\n                    .unwrap_or(false);\n                let path = if is_link {",
     "                match fs::metadata(&entry_path) {\n                    Ok(md) if md.is_dir() => {}\n                    Err(_) => {}\n                    _ => continue,\n                }\n                let is_link = fs::symlink_metadata(&entry_path)\n                    .map(|m| m.file_type().is_symlink())\n                    .unwrap_or(false);\n                let path = if is_link && false {",
     "bootstrap", "dangling_links_are_skipped_so_nothing_appears_that_was_not_there_yesterday"),

    ("F11 the reserved name is allowed to be registered", "src/lib.rs",
     "        if is_reserved_name(&name) {\n            return Err(name_reserved(&name));\n        }",
     "        if false {\n            return Err(name_reserved(&name));\n        }",
     "registry", "register_refuses_every_spelling_of_the_bucket"),

    ("F12 the bucket is not synthesized", "src/lib.rs",
     "        rows.push(Row {\n            name: BUCKET.to_string(),",
     "        rows.truncate(usize::MAX); rows.push(Row {\n            name: String::new(),",
     "registry", "the_bucket_is_synthesized_last_and_is_not_counted_among_the_projects"),

    ("F13 the same checkout may be registered twice", "src/lib.rs",
     "        if let Some(existing) = self.projects.iter().find(|e| e.path == canonical) {\n            return Err(path_already_registered(&canonical, &existing.name));\n        }",
     "        if let Some(existing) = self.projects.iter().find(|e| e.path == canonical) {\n            let _ = &existing.name;\n        }",
     "registry", "register_refuses_the_same_checkout_under_a_second_spelling"),

    ("F14 the snapshot caches forever and never re-stats", "src/snapshot.rs",
     "        let mut inner = self.0.lock().expect(\"snapshot mutex\");\n        if !inner.watched {\n            return;\n        }",
     "        let mut inner = self.0.lock().expect(\"snapshot mutex\");\n        if !inner.watched || inner.stamp.is_some() || true {\n            return;\n        }",
     "registry", "the_snapshot_sees_a_registration_made_behind_its_back_without_a_restart"),

    ("F15 missing-row honesty: a gone directory is dropped from his list", "src/lib.rs",
     "                missing: !e.path.is_dir(),",
     "                missing: false,",
     "registry", "a_registered_path_that_stops_existing_keeps_its_row_and_is_drawn_missing"),

    ("F16 resolve invents a path for an unknown name", "src/lib.rs",
     "        self.get(name).map(|e| e.path.clone())",
     "        Some(self.get(name).map(|e| e.path.clone()).unwrap_or_else(|| home.join(name)))",
     "registry", "resolve_answers_the_bucket_a_registered_name_and_nothing_else"),

    ("F17 derivation conformance vs the real ~/minds, normalized", "src/lib.rs",
     "                projects.push(Entry {\n                    name,",
     "                projects.push(Entry {\n                    name: name.replace('_', \"-\"),",
     "derivation_matches_tui", "the_derivation_yields_exactly_the_list_the_tui_draws_today"),

    # ── THE CLI (`src/bin/projects.rs`, suite `cli`) ───────────────────────
    # It is a SECOND WRITER of a file whose crate names one writer, so its
    # guards are the ones worth forcing. The lock itself is F4/F5 above; these
    # are the binary's own.

    ("F18 the CLI mints anyway: drop the not-the-minter guard", "src/bin/projects.rs",
     "    if registry_path(home).exists() {\n        return Ok(());\n    }",
     "    if true {\n        return Ok(());\n    }",
     "cli", "register_refuses_to_write_before_the_registry_is_minted_when_minds_has_something_to_lose"),

    ("F19 the guard counts with its OWN read_dir instead of the crate's derivation", "src/bin/projects.rs",
     "    let at_stake = Registry::derive_from_minds(home, \"cauldron-cli-guard\").len();",
     "    let at_stake = std::fs::read_dir(minds_root(home)).map(|rd| rd.count()).unwrap_or(0);",
     "cli", "the_guard_counts_what_the_bootstrap_would_actually_derive_not_what_read_dir_returns"),

    ("F20 `list` hides a row whose checkout has gone", "src/bin/projects.rs",
     "        } else if r.missing {",
     "        } else if false {",
     "cli", "list_draws_the_registered_rows_then_the_bucket_and_marks_a_missing_one"),

    ("F21 a refusal exits 0, so a shell script would sail on", "src/bin/projects.rs",
     "        Err(sentence) => {\n            eprintln!(\"{sentence}\");\n            ExitCode::FAILURE\n        }",
     "        Err(sentence) => {\n            eprintln!(\"{sentence}\");\n            ExitCode::SUCCESS\n        }",
     "cli", "register_speaks_the_crates_refusals_on_stderr_exits_one_and_writes_nothing"),

    ("F22 a bad command line is indistinguishable from a refusal", "src/bin/projects.rs",
     "            return ExitCode::from(2);",
     "            return ExitCode::FAILURE;",
     "cli", "a_command_line_it_does_not_understand_is_exit_two_and_never_exit_one"),

    # ── THE CONTROL. A comment-only edit. If this reports RED, the machine
    # above is not measuring what it claims to measure.
    ("CONTROL no-op: a comment-only edit that changes no behaviour", "src/lib.rs",
     "/// The schema version this crate writes and the only one it reads.",
     "/// The schema version this crate writes and the only one it reads. (control)",
     "registry", "move_before_refuses_a_stale_neighbour_instead_of_sending_the_row_to_the_end"),
]

def run(suite, test, ignored):
    args = ["cargo", "test", "-p", "projects", "--test", suite, "--", "--exact", test]
    if ignored:
        args.append("--ignored")
    r = subprocess.run(args, cwd=os.path.dirname(os.path.dirname(CRATE)),
                       capture_output=True, text=True)
    return r.returncode, (r.stdout + r.stderr)

def main():
    backup = tempfile.mkdtemp(prefix="cauldron-pristine-")
    for f in SRC:
        shutil.copy(f, os.path.join(backup, os.path.basename(f)))
    results = []
    try:
        for label, rel, old, new, suite, test in MUTATIONS:
            path = os.path.join(CRATE, rel)
            s = open(path).read()
            if s.count(old) != 1:
                results.append((label, "PATCH-DID-NOT-APPLY", test))
                print(f"!! {label}: anchor found {s.count(old)} times", flush=True)
                continue
            open(path, "w").write(s.replace(old, new))
            ignored = suite == "derivation_matches_tui"
            code, out = run(suite, test, ignored)
            # restore immediately
            for f in SRC:
                shutil.copy(os.path.join(backup, os.path.basename(f)), f)
            if code == 0:
                verdict = "DID NOT BITE (green survived)"
            elif "error[E" in out or "error: could not compile" in out:
                verdict = "RED (compile)"
            else:
                verdict = "RED (assertion)"
            results.append((label, verdict, test))
            print(f"{verdict:28} {label}\n{'':28} -> {suite}::{test}", flush=True)
    finally:
        for f in SRC:
            shutil.copy(os.path.join(backup, os.path.basename(f)), f)
    print("\n==== SUMMARY ====")
    for label, verdict, test in results:
        print(f"{verdict:28} {label}")

main()
