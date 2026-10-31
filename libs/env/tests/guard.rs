//! The guard, forced red BOTH ways: it refuses production, and it does not
//! refuse everything. A guard that refuses all is not a guard.
//!
//! Every assertion here is on a value, and every refusal's text is printed so
//! the proof script can record it.

use std::path::Path;

use env::guard;

/// The real home this test runs under, read from `$HOME` here — NOT from the
/// guard's own derivation, so the two are independent instruments — joined
/// with `rel`. Before REL-4.S4 every path below was a literal naming the
/// operator's home; REL-4.S4 swapped them for a home that exists nowhere and
/// the suite stayed green while the guard stopped protecting anything real
/// (REL-4.S4-I F1). Deriving it is what makes every assertion here about the
/// real tree on whatever machine runs it.
fn h(rel: &str) -> &'static str {
    let home = std::env::var("HOME").expect("HOME must be set for the guard tests");
    assert!(home.starts_with('/') && home != "/", "SENTINEL: a real absolute HOME, got {home:?}");
    Box::leak(format!("{}/{rel}", home.trim_end_matches('/')).into_boxed_str())
}

/// The OLD store — the byte-identical rollback copy left behind by the
/// 2026-10-07 move. Nothing in wave 1 may touch it.
#[allow(non_snake_case)]
fn PROD_DB() -> &'static str {
    h("code/substrate-interface-layer/lib/threads/threads.db")
}
#[allow(non_snake_case)]
fn PROD_TURNS() -> &'static str {
    h("code/substrate-interface-layer/lib/threads/turns")
}
#[allow(non_snake_case)]
fn PROD_TARGET() -> &'static str {
    h("code/substrate-interface-layer/target/debug/threads")
}

/// A copy of the store relocated or symlinked WITHIN the old repo, where
/// neither [`guard::PROTECTED`] prefix reaches. This is the case the name rule
/// exists for, and it is still refused.
#[allow(non_snake_case)]
fn PROD_DB_RELOCATED_IN_REPO() -> &'static str {
    h("code/substrate-interface-layer/lib/other/threads.db")
}

/// **The live store's NEW home** (moved 2026-10-07). It is production, and it
/// is a cauldron data root — the guard must not refuse it by filename, because
/// the filename is the same filename.
#[allow(non_snake_case)]
fn LIVE_STORE_DB() -> &'static str {
    h(".cauldron/data/prod/threads/store/threads.db")
}

/// A legitimate `dev` environment, which is the whole point of the environment
/// mechanism and must be admitted.
#[allow(non_snake_case)]
fn DEV_DATA_ROOT() -> &'static str {
    h(".cauldron/data/dev")
}

#[test]
fn refuses_production() {
    for p in [PROD_DB(), PROD_TURNS(), PROD_TARGET()] {
        let err = guard::refuse_protected(Path::new(p))
            .expect_err(&format!("THE GUARD DID NOT REFUSE {p} — that is a failure"));
        let text = format!("{err:#}");
        println!("RED (refused, as it must): {p}\n    {text}");
        assert!(text.contains("REFUSED"), "refusal text must say REFUSED: {text}");
        assert!(text.contains(p) || text.contains("threads"), "refusal must name the path: {text}");
    }
}

#[test]
fn refuses_a_relocated_copy_of_the_store_inside_the_old_repo() {
    // The denylist is prefix-based, so it is backed up by a name rule: inside
    // the OLD repo, a path that names threads.db is refused wherever it sits —
    // including the places the two protected prefixes do not reach.
    let err = guard::refuse_protected(Path::new(PROD_DB_RELOCATED_IN_REPO()))
        .expect_err("a threads.db inside the old repo must be refused wherever it sits");
    let text = format!("{err:#}");
    println!("RED (refused by name, inside the old repo): {}\n    {text}", PROD_DB_RELOCATED_IN_REPO());
    assert!(text.contains("REFUSED"), "{text}");
    assert!(text.contains("threads.db"), "{text}");

    // And the same for the turns store, by name, inside the old repo.
    let err = guard::refuse_protected(Path::new(
        h("code/substrate-interface-layer/lib/other/turns"),
    ))
    .expect_err("a `turns` inside the old repo must be refused");
    println!("RED (turns refused by name, inside the old repo):\n    {err:#}");
}

/// **THE RED THE MOVE CREATED, FORCED FROM THE OTHER SIDE.**
///
/// The name rule used to match `threads.db` wherever it appeared. After
/// 2026-10-07 that meant it refused production's own new home, which is a
/// cauldron data root — the guard refusing the thing it exists to protect. The
/// name rule is scoped to the old repo now, and these are the paths that proves
/// it on.
#[test]
fn admits_the_live_store_at_its_new_home_and_its_siblings() {
    for p in [
        LIVE_STORE_DB(),
        h(".cauldron/data/prod/threads/store"),
        h(".cauldron/data/prod/threads/store/threads.db-wal"),
        h(".cauldron/data/prod/threads/store/turns"),
        h(".cauldron/data/prod/threads/warnings/warnings.jsonl"),
        // A `dev` service's store, same filename, same shape, no production
        // anywhere near it.
        h(".cauldron/data/dev/threads/store/threads.db"),
    ] {
        let ok = guard::refuse_protected(Path::new(p)).unwrap_or_else(|e| {
            panic!("THE GUARD REFUSED PRODUCTION'S OWN CORRECT HOME {p}: {e:#}")
        });
        println!("GREEN (allowed, as it must be): {}", ok.display());
    }
}

/// The other half of that red: scoping the NAME rule did not open the old tree.
/// Every path under it is still refused, by prefix.
#[test]
fn the_old_tree_is_still_closed_by_prefix() {
    for p in [
        h("code/substrate-interface-layer/lib/threads"),
        h("code/substrate-interface-layer/lib/threads/accounts.toml"),
        h("code/substrate-interface-layer/lib/threads/scripts/claude-accounts"),
        h("code/substrate-interface-layer/lib/threads/turns/0001"),
    ] {
        let err = guard::refuse_protected(Path::new(p))
            .expect_err(&format!("THE GUARD DID NOT REFUSE {p} — that is a failure"));
        println!("RED (old tree still refused by prefix): {p}\n    {err:#}");
    }
}

/// **A LEGITIMATE `dev` ENVIRONMENT IS ADMITTED, END TO END.**
///
/// Not just the data root: the service directory and a kind inside it, in the
/// decided `data/<env>/<service>/<kind>/` shape, through BOTH halves of the
/// guard. A guard that refuses the environment mechanism's own output is an
/// outage, and this is the assertion that says so on a value.
#[test]
fn a_legitimate_environment_data_root_is_admitted() {
    let root = Path::new(DEV_DATA_ROOT());
    let ok = guard::refuse_protected(root).expect("a dev data root must be admitted");
    println!("GREEN (dev data root): {}", ok.display());

    for kind in ["store", "warnings", "links"] {
        let p = root.join("threads").join(kind);
        let ok = guard::require_inside(root, &p).unwrap_or_else(|e| {
            panic!("THE GUARD REFUSED A LEGITIMATE dev SERVICE DIRECTORY {}: {e:#}", p.display())
        });
        println!("GREEN (dev, data/<env>/<service>/<kind>): {}", ok.display());
    }

    // And the environment's own files.
    for leaf in ["seams.json", "env.toml", "logs/threads.log"] {
        let p = root.join(leaf);
        let ok = guard::require_inside(root, &p)
            .unwrap_or_else(|e| panic!("THE GUARD REFUSED {}: {e:#}", p.display()));
        println!("GREEN (dev, environment's own file): {}", ok.display());
    }
}

#[test]
fn does_not_refuse_everything() {
    // The other half of the red: ordinary paths pass. A guard that refuses all
    // is not a guard, it is an outage.
    for p in [
        h(".cauldron/data/dev/seams.json"),
        h(".cauldron/data/prod/seams.json"),
        "/tmp/cauldron-proof/.cauldron/data/dev/echo",
        // A file called `threads.db` outside the old repo is somebody's
        // ordinary file. It used to be refused here, which is how the move
        // broke production's own home.
        "/tmp/somewhere/else/threads.db",
        h("code/cauldron/libs"),
        // Same repo as the protected trees, but not a protected tree.
        h("code/substrate-interface-layer/README.md"),
        h("code/substrate-interface-layer/lib/other/thing"),
    ] {
        let ok = guard::refuse_protected(Path::new(p))
            .unwrap_or_else(|e| panic!("THE GUARD REFUSED AN ORDINARY PATH {p}: {e:#}"));
        println!("GREEN (allowed, as it must be): {}", ok.display());
    }
}

#[test]
fn containment_is_the_second_half() {
    let root = Path::new(h(".cauldron/data/dev"));
    // inside: fine
    let inside = guard::require_inside(root, &root.join("echo/store"))
        .expect("a path inside the data root must be allowed");
    println!("GREEN (inside the data root): {}", inside.display());

    // another environment's data root: refused, which is the structural half
    // of "a consumer cannot be pointed at the wrong environment".
    let err = guard::require_inside(root, Path::new(h(".cauldron/data/other/seams.json")))
        .expect_err("another environment's path must be refused");
    let text = format!("{err:#}");
    println!("RED (cross-environment path refused):\n    {text}");
    assert!(text.contains("outside this environment's data root"), "{text}");

    // an unlisted danger: containment catches what the denylist was never told
    let err = guard::require_inside(root, Path::new("/etc/passwd"))
        .expect_err("an unlisted outside path must be refused by containment");
    println!("RED (unlisted path refused by containment): /etc/passwd\n    {err:#}");
}

#[test]
fn dot_dot_is_refused_rather_than_resolved() {
    let err = guard::refuse_protected(Path::new(
        h(".cauldron/data/dev/../../../code/substrate-interface-layer/lib/threads/threads.db"),
    ))
    .expect_err("a path containing .. must be refused, not quietly normalised");
    let text = format!("{err:#}");
    println!("RED (`..` refused rather than resolved):\n    {text}");
    assert!(text.contains(".."), "{text}");
}

#[test]
fn relative_paths_are_refused() {
    let err = guard::refuse_protected(Path::new("data/dev"))
        .expect_err("a relative path must be refused");
    println!("RED (relative path refused): data/dev\n    {err:#}");
}

// ═══════════════════ ADOPTION AND THE PRODUCTION PATH ═══════════════════════
//
// **The question `cauldron adopt` had to answer: is `~/.cauldron/data/prod/`
// refused as a service data root, and if so, is `adopt` exempt?**
//
// The answer turned out to be that the PREMISE IS WRONG, and these tests are
// here so nobody has to re-derive it. `~/.cauldron/data/prod/<service>/` is
// NOT refused by either half of the guard, so `adopt` needs no exemption and
// has none. Writing the finding down as assertions rather than as a comment,
// because a comment about what a guard does is exactly the thing that goes
// stale — this file's own `PROTECTED_NAME_SCOPE` story is the precedent.

/// **`prod` is an ordinary environment name.** The only reserved name under
/// `data/` is `shared`, and `prod` is in fact the DEFAULT, so an environment
/// resolving onto it is the normal case and not an edge one.
#[test]
fn prod_is_not_a_refused_environment_name() {
    assert_eq!(
        env::paths::RESERVED_ENV_NAMES,
        &[env::paths::SHARED_NAME],
        "`shared` is the whole reserved list; if `prod` is ever added, adoption's data dir has \
         to be reconsidered and this assertion is where that conversation starts"
    );
    assert!(!env::paths::RESERVED_ENV_NAMES.contains(&"prod"));
}

/// **The production data root and a service directory inside it are both
/// ADMITTED, by both halves of the guard.**
///
/// This is what makes `cauldron adopt threads` possible without an exemption.
/// The denylist does not name it, and the containment half is satisfied because
/// the service directory really is inside the environment's data root.
#[test]
fn the_production_service_data_dir_is_admitted_by_both_halves() {
    let home = std::env::var("HOME").unwrap();
    let data_root = std::path::PathBuf::from(&home).join(".cauldron/data/prod");
    let svc = data_root.join("threads");

    let r = env::guard::refuse_protected(&data_root);
    println!("refuse_protected({}) -> {:?}", data_root.display(), r.is_ok());
    assert!(r.is_ok(), "the denylist does not name the production data root");

    let r = env::guard::refuse_protected(&svc);
    println!("refuse_protected({}) -> {:?}", svc.display(), r.is_ok());
    assert!(r.is_ok(), "nor a service directory inside it");

    let r = env::guard::require_inside(&data_root, &svc);
    println!("require_inside -> {:?}", r.as_ref().map(|p| p.display().to_string()));
    assert!(r.is_ok(), "and containment is satisfied: the service dir IS inside the data root");

    // The live store itself, at its post-2026-10-07 home. Admitted, which is
    // the whole point of `PROTECTED_NAME_SCOPE` existing: an unscoped
    // `threads.db` name rule used to refuse production's own correct home.
    let store = svc.join("store/threads.db");
    assert!(
        env::guard::refuse_protected(&store).is_ok(),
        "production's own store at its own home must not be refused -- a guard that refuses the \
         thing it protects is an outage with a good reason attached"
    );
}

/// **And the guard is NOT bypassed by adoption.** The old repository is still
/// refused, by prefix and by name, so an adoption pointed at the ROLLBACK copy
/// is refused with the guard's own words rather than quietly recorded.
#[test]
fn adoption_cannot_point_at_the_protected_rollback_tree() {
    for p in [
        h("code/substrate-interface-layer/lib/threads"),
        h("code/substrate-interface-layer/lib/threads/threads.db"),
        h("code/substrate-interface-layer/lib/threads/turns"),
        h("code/substrate-interface-layer/target/debug"),
    ] {
        let e = env::guard::refuse_protected(std::path::Path::new(p)).unwrap_err();
        let t = format!("{e:#}");
        println!("refused -> {t}");
        assert!(t.contains("REFUSED"));
    }
}

/// **The symlinks, and why they do not bite.**
///
/// The live store contains symlinks pointing INTO the protected tree
/// (`store/accounts -> .../lib/threads/accounts`). They do not make the store
/// refused, because `guard::normalize` deliberately never resolves symlinks —
/// a path that does not exist yet must be checkable BEFORE it is created,
/// which is the whole point of a guard.
///
/// That is the right answer here for a second reason: `adopt` never follows
/// them. It records one directory and reads one socket. A guard that resolved
/// symlinks would refuse the adoption on the strength of a link cauldron is
/// never going to traverse.
#[test]
fn normalize_does_not_resolve_symlinks_so_the_stores_links_do_not_refuse_it() {
    let home = std::env::var("HOME").unwrap();
    let linked = std::path::PathBuf::from(&home).join(".cauldron/data/prod/threads/store/accounts");
    let n = env::guard::normalize(&linked).unwrap();
    println!("normalize -> {}", n.display());
    assert_eq!(n, linked, "normalisation is LEXICAL; nothing is resolved");
    assert!(
        env::guard::refuse_protected(&linked).is_ok(),
        "a symlink INTO the protected tree does not make its own path protected, and adopt \
         never traverses it"
    );
}

// ───────────────────────── tts, moved 2026-10-08 ────────────────────────────

/// The tts store's OLD home — the crate directory inside the git worktree. As
/// of 2026-10-08 it is the rollback AND, because the daemon was deliberately not
/// restarted, still the directory the live process is writing to. Both reasons
/// point the same way.
#[allow(non_snake_case)]
fn TTS_OLD_DB() -> &'static str {
    h("code/substrate-interface-layer/lib/tts/tts.db")
}
#[allow(non_snake_case)]
fn TTS_OLD_AUDIO() -> &'static str {
    h("code/substrate-interface-layer/lib/tts/audio/f1/cd")
}

/// A copy of the tts store relocated WITHIN the old repo, where the
/// [`guard::PROTECTED`] prefix for `lib/tts` does not reach. The name rule.
#[allow(non_snake_case)]
fn TTS_DB_RELOCATED_IN_REPO() -> &'static str {
    h("code/substrate-interface-layer/lib/other/tts.db")
}

/// **The tts store's NEW home.** It is production and it is a cauldron data
/// root, so the name rule must not refuse it — exactly the regression the
/// threads move produced once already and that [`guard::PROTECTED_NAME_SCOPE`]
/// exists to prevent.
#[allow(non_snake_case)]
fn TTS_NEW_DB() -> &'static str {
    h(".cauldron/data/prod/tts/store/tts.db")
}

#[test]
fn refuses_the_old_tts_store_by_prefix() {
    for p in [
        h("code/substrate-interface-layer/lib/tts"),
        TTS_OLD_DB(),
        TTS_OLD_AUDIO(),
        h("code/substrate-interface-layer/lib/tts/tts.last-good"),
    ] {
        let err = guard::refuse_protected(Path::new(p))
            .expect_err(&format!("THE GUARD DID NOT REFUSE {p} — that is a failure"));
        let text = format!("{err:#}");
        println!("RED (refused, as it must): {p}\n    {text}");
        assert!(text.contains("REFUSED"), "refusal text must say REFUSED: {text}");
        assert!(text.contains("tts"), "refusal must name tts: {text}");
    }
}

#[test]
fn refuses_a_relocated_copy_of_the_tts_store_inside_the_old_repo() {
    for p in [
        TTS_DB_RELOCATED_IN_REPO(),
        h("code/substrate-interface-layer/lib/other/tts.db-wal"),
        h("code/substrate-interface-layer/lib/other/tts.db-shm"),
    ] {
        let err = guard::refuse_protected(Path::new(p))
            .expect_err(&format!("a tts.db inside the old repo must be refused: {p}"));
        let text = format!("{err:#}");
        println!("RED (refused by name, inside the old repo): {p}\n    {text}");
        assert!(text.contains("REFUSED"), "{text}");
        assert!(text.contains("tts.db"), "{text}");
    }
}

/// THE OTHER DIRECTION, and the one that would actually take the voice down.
/// `tts.db` under a cauldron data root is production's own new, correct home,
/// and `cauldron adopt tts` runs `refuse_protected` on it. A guard that
/// refused it would refuse the thing it exists to protect.
#[test]
fn does_not_refuse_the_tts_store_at_its_new_home() {
    for p in [
        TTS_NEW_DB(),
        h(".cauldron/data/prod/tts/store/tts.db-wal"),
        h(".cauldron/data/prod/tts"),
        h(".cauldron/data/prod/tts/store/audio/f1/cd"),
        // and a dev environment's tts, which the environment mechanism exists for
        h(".cauldron/data/dev/tts/store/tts.db"),
    ] {
        let got = guard::refuse_protected(Path::new(p))
            .unwrap_or_else(|e| panic!("THE GUARD REFUSED PRODUCTION'S OWN HOME {p}: {e:#}"));
        println!("GREEN (admitted, as it must be): {}", got.display());
    }
}

/// The adoption path end to end, in guard terms: `adopt_one` calls
/// `refuse_protected` on the DECLARED dir and then compares it to
/// `service_data`, which is `require_inside(data_root, data_root/tts)`.
#[test]
fn the_tts_adoption_path_is_admitted_and_lands_where_the_layout_says() {
    let root = Path::new(h(".cauldron/data/prod"));
    let svc = root.join("tts");
    let got = guard::require_inside(root, &svc)
        .unwrap_or_else(|e| panic!("prod/tts must be admitted: {e:#}"));
    assert_eq!(got, Path::new(h(".cauldron/data/prod/tts")));
    println!("GREEN (the layout path adopt compares against): {}", got.display());

    // And a dev environment may not reach into prod's tts, which is the half a
    // denylist cannot provide.
    let err = guard::require_inside(Path::new(DEV_DATA_ROOT()), &svc)
        .expect_err("dev must not be handed prod's tts store");
    let text = format!("{err:#}");
    println!("RED (containment): {text}");
    assert!(text.contains("REFUSED"), "{text}");
}

// ═══════════════ REL-4.S4b: THE DENYLIST IS DERIVED, NOT WRITTEN ════════════
//
// REL-4.S4-I F1: the denylist named a home that exists nowhere, so on the
// operator's machine it refused nothing real, and every test above had been
// edited to the same placeholder and stayed green. The two tests below are the
// ones that would have caught it.

/// **On THIS machine, the real old store under the real home is refused.**
/// The home is read from `$HOME` by the test (see [`h`]) and compared with the
/// guard's own derivation as a sentinel, so the claim is about the real tree.
#[test]
fn refuses_the_real_derived_live_store_on_this_machine() {
    let home = std::path::PathBuf::from(std::env::var("HOME").unwrap());
    let homes = guard::operator_homes();
    println!("operator_homes() = {homes:?}");
    assert!(homes.contains(&home), "SENTINEL: the guard's homes {homes:?} include this HOME {home:?}");
    assert!(
        home.join("code").is_dir(),
        "SENTINEL: {}/code exists, so this home has the operator's layout and the paths below are \
         the real ones, not a name for nothing",
        home.display()
    );
    for p in [
        home.join("code/substrate-interface-layer/lib/threads/threads.db"),
        home.join("code/substrate-interface-layer/lib/tts/tts.db"),
        home.join("code/substrate-interface-layer/target/release/threads"),
    ] {
        let e = guard::refuse_protected(&p)
            .expect_err(&format!("THE GUARD DID NOT REFUSE THE REAL {} — F1 is back", p.display()));
        println!("RED (real derived path refused): {}\n    {e:#}", p.display());
    }
    // and the image door refuses the real store too
    assert!(guard::refuse_as_image(&home.join("code/substrate-interface-layer/lib/threads/threads.db")).is_err());
    // opposed: the same layout under a directory that is NOT a home is ordinary
    for p in ["/tmp/not-a-home/code/substrate-interface-layer/lib/threads/threads.db"] {
        guard::refuse_protected(Path::new(p))
            .unwrap_or_else(|e| panic!("refused a path under no home at all {p}: {e:#}"));
    }
}

const CHILD_FLAG: &str = "REL4_S4B_GUARD_CHILD";

/// **A scratch HOME that mirrors the layout is guarded on the same terms, AND
/// repointing HOME does not unguard the real one.**
///
/// Runs this test binary again as a child with `HOME` set to a scratch
/// directory holding `code/substrate-interface-layer/lib/{threads,tts}` and
/// `.cauldron/data/dev`, executing only [`child_under_scratch_home`]. The
/// child refuses the scratch tree (the `$HOME` half of the derivation) and the
/// parent's real tree (the passwd half — a proof that repoints HOME must not
/// be able to hand cauldron the operator's store), and admits the scratch dev
/// root. The parent asserts the child exited 0 AND printed its marker, so a
/// child that skipped is not mistaken for one that passed.
#[test]
fn a_scratch_home_is_guarded_and_repointing_home_does_not_unguard_the_real_one() {
    let real = std::env::var("HOME").unwrap();
    let scratch = std::env::temp_dir().join(format!("rel4s4b-guard-home-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    for d in [
        "code/substrate-interface-layer/lib/threads/turns",
        "code/substrate-interface-layer/lib/tts/audio",
        "code/substrate-interface-layer/target/debug",
        ".cauldron/data/dev",
    ] {
        std::fs::create_dir_all(scratch.join(d)).unwrap();
    }
    std::fs::write(scratch.join("code/substrate-interface-layer/lib/threads/threads.db"), b"").unwrap();
    let scratch = scratch.canonicalize().unwrap();
    assert_ne!(scratch, std::path::PathBuf::from(&real), "SENTINEL: the scratch home is not the real one");
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_under_scratch_home", "--nocapture", "--test-threads=1"])
        .env("HOME", &scratch)
        .env(CHILD_FLAG, &real)
        .output()
        .unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    println!("child (HOME={}):\n{text}", scratch.display());
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(out.status.success(), "the child under a scratch HOME failed:\n{text}");
    assert!(text.contains("CHILD-RAN-ALL-ASSERTIONS"), "the child did not run its assertions:\n{text}");
}

/// The child half of the test above. Run on its own (no flag) it has nothing
/// to check and says so; the parent requires its marker.
#[test]
fn child_under_scratch_home() {
    let Ok(real) = std::env::var(CHILD_FLAG) else {
        println!("not a child run: nothing to do (the parent test asserts the child's marker)");
        return;
    };
    let scratch = std::path::PathBuf::from(std::env::var("HOME").unwrap());
    assert_ne!(scratch.to_str().unwrap(), real, "the child must run under the scratch HOME");
    println!("child: HOME={} operator_homes={:?}", scratch.display(), guard::operator_homes());
    // $HOME half: the scratch tree mirrors the layout and is refused.
    for rel in [
        "code/substrate-interface-layer/lib/threads/threads.db",
        "code/substrate-interface-layer/lib/threads/turns",
        "code/substrate-interface-layer/lib/tts/audio",
        "code/substrate-interface-layer/target/debug/threads",
        "code/substrate-interface-layer/lib/other/threads.db",
    ] {
        let p = scratch.join(rel);
        let e = guard::refuse_protected(&p)
            .expect_err(&format!("the scratch HOME's {} was not refused", p.display()));
        println!("RED (scratch HOME, refused): {}\n    {e:#}", p.display());
    }
    // passwd half: HOME is repointed, and the REAL store is still refused.
    let realp = std::path::Path::new(&real).join("code/substrate-interface-layer/lib/threads/threads.db");
    let e = guard::refuse_protected(&realp).expect_err(&format!(
        "with HOME repointed to a scratch dir, the REAL {} was admitted",
        realp.display()
    ));
    println!("RED (real store, HOME repointed): {}\n    {e:#}", realp.display());
    // opposed: the scratch HOME's dev data root is ordinary and admitted.
    let dev = scratch.join(".cauldron/data/dev");
    guard::require_inside(&dev, &dev.join("echo/store"))
        .unwrap_or_else(|e| panic!("the scratch dev data root was refused: {e:#}"));
    println!("CHILD-RAN-ALL-ASSERTIONS");
}
