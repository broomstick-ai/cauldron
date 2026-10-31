//! # No threads test reads the real keychain (CON-28, R9.F2d)
//!
//! The ONE shared helper every threads test file that can reach a keychain
//! read includes. The real reader (`accounts::read_credential_facts`, which
//! spawns `/usr/bin/security`) is reached from a test in three ways:
//!
//! * the `accounts` shape's `LoginSlots` hydrate → `collect_slot_reports`,
//!   which runs `ensure_default` and so reads the MACHINE-DEFAULT store, the
//!   operator's live login;
//! * `accounts/{account}/delete` → the same inventory;
//! * the manager's default slot probe (`read_store_facts`) for a slot dir that
//!   holds a `.claude.json`, which also reads the machine default as a clone
//!   peer.
//!
//! All three go through `accounts::set_keychain_reader_for_test` (the probe's
//! production reader routes through the same process-wide override since
//! R9.F2d), so [`install`] covers them with one call. Every store reads as
//! ABSENT (`CredentialFacts::default()`): no test here depends on the
//! operator's real login, and one that did would be reading his machine.
//!
//! **The guard.** In a cargo test-harness process the real reader does not run:
//! it prints a `CON-28 tripwire` line and ABORTS the process, before any
//! `security` spawn (`accounts::real_keychain_tripwire_armed`). So a test file
//! that forgets [`install`] and reaches the real reader goes red rather than
//! reading the keychain. `tests/keychain_tripwire.rs` proves that it fires.
//!
//! Call [`install`] at the top of every test (or in the file's `start`
//! helper) that builds a `ThreadManager` or subscribes to `accounts`: the
//! override is process-wide and idempotent, and tests in one file run in one
//! process in no fixed order, so a single test installing it is not enough.
//!
//! Included with `#[path = "support/keychain.rs"] mod keychain;`.

#![allow(dead_code)]

use std::sync::Arc;
use std::sync::Once;

use threads::accounts::{
    set_keychain_entry_exists_for_test, set_keychain_reader_for_test, CredentialFacts,
};

/// Replace every keychain read in this process with the fixture: every store
/// absent, every item absent (R9.F2h: `keychain_entry_exists` too). Idempotent. Does not clobber a reader a test installed itself
/// AFTER the first call (it installs only once).
pub fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        set_keychain_reader_for_test(Some(Arc::new(|_service: &str, _account: &str| {
            CredentialFacts::default()
        })));
        // R9.F2h: the item operations (existence, delete, clone) too — no
        // item exists, so the delete and rename doors touch none.
        set_keychain_entry_exists_for_test(Some(Arc::new(|_service: &str, _account: &str| false)));
    });
}

/// **The population this fixture does NOT cover (doctrine 10; R9.F2h, from
/// R9.F2d inspect F2).** Every test file — top-level `tests/*.rs`, counting
/// the `support/` files it includes with `#[path = ...] mod` — that runs one
/// of this crate's binaries (`env!("CARGO_BIN_EXE_<bin>")`) as a SEPARATE
/// PROCESS. [`install`] is in-process only, and the CON-28 tripwire is armed
/// only in a `deps/<name>-<16 hex>` harness exe, so a daemon or CLI those
/// files spawn (`debug/threads`, `debug/threads-stress`) can still reach the
/// real keychain reader. The list names who is in that regime so it cannot
/// grow unseen: `tests/keychain_tripwire.rs`
/// `subprocess_population_is_exactly_the_files_that_spawn_a_crate_binary`
/// lexes every test file (comments and string contents excluded, so a doc
/// line mentioning the macro does not count) and fails if a spawning file is
/// missing here or a listed file no longer spawns. Sorted.
pub const RUNS_A_CRATE_BINARY_AS_A_SUBPROCESS: &[&str] = &[
    "account_disable_restart.rs",
    "attach_recover.rs",
    "bind_first_recovery.rs",
    // `shapes-manifest`, not the daemon: prints the shapes manifest.
    "cell_manifest.rs",
    "cutover_cli.rs",
    "delete_arm_disabled.rs",
    "drain_pin.rs",
    "health_liveness.rs",
    "health_off_runtime.rs",
    "lifecycle_announcement.rs",
    "migrate_behind_port.rs",
    "preflight_first_exec.rs",
    "self_update.rs",
    "stress_driver.rs",
    "ws_protocol.rs",
];
