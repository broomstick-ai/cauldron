//! **REL-6a3 RULE B: A TEST BINARY'S CHILDREN ARE ISOLATED.**
//!
//! A cargo test binary is an isolated process (`tui::cauldron::live::isolation`
//! sees it as a test harness), but that decision is made from ITS OWN
//! `current_exe` and does not reach a child it spawns: a test that runs the
//! built face without the marker used to start his interactive face
//! (REL-6a2-I B1). This crate closes that at the process level. Linked into a
//! test binary (it is only ever a `[dev-dependencies]` entry, and lives under
//! `testkit/`, which is structurally never deployed), it registers a
//! load-time initializer — run by the loader before `main`, so before any test
//! thread exists — that sets `CAULDRON_ISOLATED=1` in the process
//! environment. Every child spawned afterwards inherits it, whatever spawned it.
//!
//! It is its own crate because the face crate forbids `unsafe_code`, and a
//! `link_section` is exactly that. Nothing here is reachable from a binary
//! that is not a test: the face has no dependency on it.
//!
//! Each test binary invokes [`install!`] once at its root; that adds one test
//! asserting the initializer ran, and that reference is also what makes the
//! linker keep this object (an unreferenced rlib object is dropped).

use std::sync::atomic::{AtomicBool, Ordering};

/// The marker `tui::cauldron::live::ISOLATED_ENV` names.
pub const MARKER: &str = "CAULDRON_ISOLATED";

static RAN: AtomicBool = AtomicBool::new(false);

extern "C" fn at_load() {
    if std::env::var(MARKER).ok().as_deref() != Some("1") {
        std::env::set_var(MARKER, "1");
    }
    RAN.store(true, Ordering::SeqCst);
}

#[used]
#[cfg_attr(target_os = "macos", link_section = "__DATA,__mod_init_func")]
#[cfg_attr(target_os = "linux", link_section = ".init_array")]
static AT_LOAD: extern "C" fn() = at_load;

/// Whether the load-time initializer ran in this process. Reads [`AT_LOAD`]
/// so that referencing this keeps the initializer linked.
pub fn ran() -> bool {
    std::hint::black_box(AT_LOAD as usize);
    RAN.load(Ordering::SeqCst)
}

/// Invoke once at a test binary's root: adds a test that fails unless the
/// load-time initializer ran and the marker is in this process's environment.
#[macro_export]
macro_rules! install {
    () => {
        #[test]
        fn rel6a3_this_test_binary_exports_cauldron_isolated_to_its_children() {
            assert!($crate::ran(), "the load-time initializer did not run in this test binary");
            assert_eq!(std::env::var($crate::MARKER).as_deref(), Ok("1"), "the marker is not in this process's environment");
        }
    };
}
