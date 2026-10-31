//! # The CON-28 tripwire fires, and only without the fixture (R9.F2d)
//!
//! `tests/support/keychain.rs` installs the fixture keychain reader in every
//! threads test file that can reach a keychain read. The GUARD behind it is
//! the tripwire in `accounts::read_credential_facts`: in a cargo test-harness
//! process the real reader aborts the process before any `/usr/bin/security`
//! spawn. A guard nobody has seen fire proves nothing (doctrine 1, 5), so this
//! file fires it, in a CHILD process (the abort would otherwise take this
//! whole test binary with it):
//!
//! * RED arm — the child calls the real path (`collect_slot_reports`, which
//!   reads the machine-default store) with NO fixture: it must die by SIGABRT
//!   with the tripwire line on stderr.
//! * OPPOSED arm — the same child, same call, WITH the fixture installed: it
//!   must exit 0, and the fixture must have been asked (so the green is not a
//!   path that read nothing at all).
//!
//! The child is this same test binary, re-run with `--exact` on [`child`] and
//! an env var naming the arm; without that env var `child` returns at once,
//! so in an ordinary run it is a no-op.

#[path = "support/keychain.rs"]
mod keychain;

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use threads::accounts::{collect_slot_reports, set_keychain_reader_for_test, CredentialFacts, Registry};

const ARM: &str = "THREADS_F2D_TRIPWIRE_ARM";

static FIXTURE_READS: AtomicUsize = AtomicUsize::new(0);

/// The child body. A no-op unless the parent set [`ARM`].
#[test]
fn child() {
    let Ok(arm) = std::env::var(ARM) else { return };
    match arm.as_str() {
        "no-fixture" | "exists-no-fixture" => {}
        "exists-fixture" => {
            threads::accounts::set_keychain_entry_exists_for_test(Some(Arc::new(|_: &str, _: &str| {
                FIXTURE_READS.fetch_add(1, Ordering::SeqCst);
                false
            })));
        }
        "fixture" => {
            set_keychain_reader_for_test(Some(Arc::new(|_: &str, _: &str| {
                FIXTURE_READS.fetch_add(1, Ordering::SeqCst);
                CredentialFacts::default()
            })));
        }
        other => panic!("unknown arm {other}"),
    }
    if arm.starts_with("exists-") {
        // R9.F2h: the delete/rename doors' existence check — a slot service,
        // never the machine default.
        let exists = threads::accounts::keychain_entry_exists(F2H_SERVICE, "f2h-sentinel");
        let n = FIXTURE_READS.load(Ordering::SeqCst);
        eprintln!("F2D-CHILD-DONE exists={exists} fixture_reads={n}");
        assert!(n >= 1, "the fixture arm must actually be asked: {n}");
        assert!(!exists, "the fixture reads every store absent");
        return;
    }
    let data = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let reports = collect_slot_reports(&Registry::default(), data.path(), home.path(), 0, 7.0);
    // Reached only when the tripwire did not fire.
    let n = FIXTURE_READS.load(Ordering::SeqCst);
    eprintln!("F2D-CHILD-DONE reports={} fixture_reads={n}", reports.len());
    assert!(n >= 1, "the fixture arm must actually be asked: {n}");
}

/// A slot-shaped keychain service no real login uses (R9.F2h).
const F2H_SERVICE: &str = "Claude Code-credentials-f2hsentinel";

fn run_child(arm: &str) -> (std::process::ExitStatus, String) {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "keychain_tripwire::child", "--nocapture", "--test-threads=1"])
        .env(ARM, arm)
        .output()
        .unwrap();
    (out.status, String::from_utf8_lossy(&out.stderr).to_string())
}

#[test]
fn tripwire_is_armed_in_a_test_harness_process() {
    assert!(
        threads::accounts::real_keychain_tripwire_armed(),
        "this is a cargo test binary ({:?}); the tripwire must be armed here",
        std::env::current_exe()
    );
}

#[test]
fn real_reader_without_the_fixture_aborts_the_test_process() {
    let (status, stderr) = run_child("no-fixture");
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(
        status.signal(),
        Some(6),
        "the real keychain reader ran in a test process and nothing stopped it: {status:?}\n{stderr}"
    );
    assert!(stderr.contains("CON-28 tripwire"), "{stderr}");
    assert!(!stderr.contains("F2D-CHILD-DONE"), "{stderr}");
}

#[test]
fn with_the_fixture_the_same_call_completes_and_reads_the_fixture() {
    let (status, stderr) = run_child("fixture");
    assert!(status.success(), "{status:?}\n{stderr}");
    assert!(!stderr.contains("CON-28 tripwire"), "{stderr}");
    assert!(stderr.contains("F2D-CHILD-DONE"), "{stderr}");
}

/// **R9.F2h (R9.F2d inspect F1): the existence check the delete and rename
/// doors run is under the same sentinel.** Without the fixture it aborts
/// before any `security` spawn; with it, it asks the fixture and completes.
#[test]
fn entry_exists_without_the_fixture_aborts_the_test_process() {
    let (status, stderr) = run_child("exists-no-fixture");
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(
        status.signal(),
        Some(6),
        "keychain_entry_exists reached the real keychain in a test process and nothing stopped it: {status:?}\n{stderr}"
    );
    assert!(stderr.contains("CON-28 tripwire") && stderr.contains(F2H_SERVICE), "{stderr}");
    assert!(!stderr.contains("F2D-CHILD-DONE"), "{stderr}");
}

#[test]
fn entry_exists_with_the_fixture_asks_the_fixture_and_completes() {
    let (status, stderr) = run_child("exists-fixture");
    assert!(status.success(), "{status:?}\n{stderr}");
    assert!(!stderr.contains("CON-28 tripwire"), "{stderr}");
    assert!(stderr.contains("F2D-CHILD-DONE exists=false"), "{stderr}");
}

/// The shared helper itself is a fixture the tripwire accepts.
#[test]
fn the_shared_helper_is_a_fixture() {
    keychain::install();
    let data = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let reports = collect_slot_reports(&Registry::default(), data.path(), home.path(), 0, 7.0);
    assert!(!reports.is_empty(), "the machine default is always reported");
}

// ---------------------------------------------------------------------------
// R9.F2h (R9.F2d inspect F2): the SUBPROCESS population, named and censused.
//
// `keychain::RUNS_A_CRATE_BINARY_AS_A_SUBPROCESS` lists the test files outside
// the in-process fixture and the tripwire. The census below re-derives that set
// from the test sources and the list must equal it. It reads TOKENS, not text
// (doctrine 3): a minimal Rust lexer drops comments and keeps string literals
// as single tokens, so only a real `env!("CARGO_BIN_EXE_…")` invocation counts
// — a doc comment or a string that mentions the macro does not.
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Punct(char),
}

/// Tokenise Rust source: identifiers, string literals (normal, byte, raw), and
/// single punctuation chars. Comments (line, doc, nested block) and char
/// literals are dropped; numbers and lifetimes are dropped or reduced to
/// punctuation. Enough to find macro invocations and `#[path]` attributes.
fn lex(src: &str) -> Vec<Tok> {
    let c: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch == '/' && c.get(i + 1) == Some(&'/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else if ch == '/' && c.get(i + 1) == Some(&'*') {
            let mut depth = 1;
            i += 2;
            while i < c.len() && depth > 0 {
                if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if ch == '"' {
            let (s, next) = lex_str(&c, i + 1);
            out.push(Tok::Str(s));
            i = next;
        } else if ch == '\'' {
            // char literal ('x', '\n', '\'') or a lifetime ('a).
            if c.get(i + 1) == Some(&'\\') {
                i += 2;
                while i < c.len() && c[i] != '\'' {
                    i += 1;
                }
                i += 1;
            } else if c.get(i + 2) == Some(&'\'') {
                i += 3;
            } else {
                out.push(Tok::Punct('\''));
                i += 1;
            }
        } else if ch.is_alphabetic() || ch == '_' {
            let start = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            let id: String = c[start..i].iter().collect();
            let raw_prefix = id == "r" || id == "br";
            if raw_prefix && matches!(c.get(i), Some('"') | Some('#')) {
                let mut hashes = 0;
                while c.get(i) == Some(&'#') {
                    hashes += 1;
                    i += 1;
                }
                // `r#ident` (a raw identifier) has no quote here.
                if c.get(i) != Some(&'"') {
                    continue;
                }
                i += 1;
                let start = i;
                loop {
                    if i >= c.len() {
                        break;
                    }
                    if c[i] == '"' && (0..hashes).all(|k| c.get(i + 1 + k) == Some(&'#')) {
                        break;
                    }
                    i += 1;
                }
                out.push(Tok::Str(c[start..i.min(c.len())].iter().collect()));
                i += 1 + hashes;
            } else if id == "b" && c.get(i) == Some(&'"') {
                let (s, next) = lex_str(&c, i + 1);
                out.push(Tok::Str(s));
                i = next;
            } else {
                out.push(Tok::Ident(id));
            }
        } else if ch.is_ascii_digit() {
            while i < c.len()
                && (c[i].is_alphanumeric()
                    || c[i] == '_'
                    || (c[i] == '.' && c.get(i + 1).map_or(false, |d| d.is_ascii_digit())))
            {
                i += 1;
            }
        } else {
            out.push(Tok::Punct(ch));
            i += 1;
        }
    }
    out
}

/// A `"…"` body from `i` (just past the opening quote); returns the contents
/// and the index past the closing quote. Escapes are kept verbatim.
fn lex_str(c: &[char], mut i: usize) -> (String, usize) {
    let start = i;
    while i < c.len() && c[i] != '"' {
        if c[i] == '\\' {
            i += 1;
        }
        i += 1;
    }
    (c[start..i.min(c.len())].iter().collect(), i + 1)
}

/// Does this token stream invoke `env!("CARGO_BIN_EXE_…")` (or
/// `option_env!`)?
fn spawns_crate_binary(toks: &[Tok]) -> bool {
    toks.windows(4).any(|w| {
        matches!(&w[0], Tok::Ident(m) if m == "env" || m == "option_env")
            && w[1] == Tok::Punct('!')
            && w[2] == Tok::Punct('(')
            && matches!(&w[3], Tok::Str(s) if s.starts_with("CARGO_BIN_EXE_"))
    })
}

/// The files this token stream includes with `#[path = "…"] mod name;`.
fn path_includes(toks: &[Tok]) -> Vec<String> {
    let mut out = Vec::new();
    for (k, w) in toks.windows(6).enumerate() {
        if w[0] == Tok::Punct('#')
            && w[1] == Tok::Punct('[')
            && matches!(&w[2], Tok::Ident(p) if p == "path")
            && w[3] == Tok::Punct('=')
            && w[5] == Tok::Punct(']')
        {
            if let Tok::Str(p) = &w[4] {
                // The attribute must sit on a `mod` (possibly `pub mod`).
                let rest = &toks[k + 6..];
                let is_mod = rest.iter().take(3).any(|t| matches!(t, Tok::Ident(m) if m == "mod"));
                if is_mod {
                    out.push(p.clone());
                }
            }
        }
    }
    out
}

/// Does `file` (or any file it includes, transitively) spawn a crate binary?
fn file_spawns(file: &std::path::Path, seen: &mut Vec<std::path::PathBuf>) -> bool {
    let canon = file.canonicalize().unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    if seen.contains(&canon) {
        return false;
    }
    seen.push(canon);
    let src = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    let toks = lex(&src);
    if spawns_crate_binary(&toks) {
        return true;
    }
    let dir = file.parent().unwrap();
    path_includes(&toks).iter().any(|p| file_spawns(&dir.join(p), seen))
}

/// The census: every top-level test file that spawns a crate binary.
fn subprocess_census() -> std::collections::BTreeSet<String> {
    let tests = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("it");
    let mut found = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(&tests).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        if file_spawns(&path, &mut Vec::new()) {
            found.insert(path.file_name().unwrap().to_string_lossy().to_string());
        }
    }
    found
}

/// THE GUARD. The named population equals the measured one, both ways.
#[test]
fn subprocess_population_is_exactly_the_files_that_spawn_a_crate_binary() {
    let listed: std::collections::BTreeSet<String> = keychain::RUNS_A_CRATE_BINARY_AS_A_SUBPROCESS
        .iter()
        .map(|s| s.to_string())
        .collect();
    let found = subprocess_census();
    // Positive sentinel over the same predicate and scope: a census that finds
    // nothing is a dead search, not a clear one (doctrine 9).
    assert!(
        found.contains("ws_protocol.rs") && found.len() >= 5,
        "the census is not finding known spawning files: {found:?}"
    );
    let missing: Vec<_> = found.difference(&listed).collect();
    let stale: Vec<_> = listed.difference(&found).collect();
    assert!(
        missing.is_empty() && stale.is_empty(),
        "tests/it/support/keychain.rs RUNS_A_CRATE_BINARY_AS_A_SUBPROCESS is out of date.\n\
         spawning a crate binary but NOT listed: {missing:?}\n\
         listed but no longer spawning: {stale:?}"
    );
}

/// The lexer counts a real invocation and nothing that merely mentions it —
/// the opposed arm of the census (doctrine 2, 3).
#[test]
fn the_census_counts_invocations_not_mentions() {
    let real = r#"fn f() { let _ = std::process::Command::new(env!("CARGO_BIN_EXE_threads")); }"#;
    assert!(spawns_crate_binary(&lex(real)));
    let spaced = "fn f() { let p = env ! ( \"CARGO_BIN_EXE_threads-stress\" ); }";
    assert!(spawns_crate_binary(&lex(spaced)));
    let mentions = r##"
        //! Runs `env!("CARGO_BIN_EXE_threads")` — a doc line, not a spawn.
        // env!("CARGO_BIN_EXE_threads")
        /* env!("CARGO_BIN_EXE_threads") /* nested */ env!("CARGO_BIN_EXE_x") */
        const S: &str = "env!(\"CARGO_BIN_EXE_threads\")";
        const R: &str = r#"env!("CARGO_BIN_EXE_threads")"#;
        fn g<'a>(x: &'a str) -> char { let _ = env!("CARGO_MANIFEST_DIR"); '"' }
    "##;
    assert!(!spawns_crate_binary(&lex(mentions)), "{:?}", lex(mentions));
    // The include graph: `#[path] mod` is followed, a mention of it is not.
    let inc = "#[path = \"support/first_exec.rs\"]\nmod first_exec;\n// #[path = \"x.rs\"] mod x;";
    assert_eq!(path_includes(&lex(inc)), vec!["support/first_exec.rs".to_string()]);
}
