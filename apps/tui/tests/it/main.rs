//! **ONE integration-test binary for the `tui` crate.** Every file under
//! `tests/it/` is a module of this one binary, because every top-level file in
//! `tests/` links a full copy of the crate and its dependencies (see
//! `core:cargo-lease`, "One test binary per crate"). A new integration test is
//! a new module here, never a new top-level file; filter one with
//! `cargo test -p tui --test it <module>::`. `pool_daemon` and `pool_setlive`
//! stay separate binaries because each sets process environment other
//! modules would read.

// REL-6a3 rule B: every child this test binary spawns inherits CAULDRON_ISOLATED=1.
test_isolation::install!();

// Shared fixtures, used by more than one module through `crate::<name>`.
mod face;
mod fake;
mod fake9010;
mod live_fixture;
mod own_seed_fixture;

mod adversarial_0215;
mod audit_partials_0180;
mod aui;
mod band_metadata_183;
mod build_stamp;
mod cancel_log;
mod cauldron_render;
mod cluster_tabs;
mod compose;
mod d1_untouched_0209;
mod design_cauldron_186;
mod designs_165;
mod drive_isolation;
mod egress_guard;
mod feedback_audit;
mod gap_sweep;
mod lib_surface;
mod light_global_0205;
mod msg235_0190;
mod msg235_0191;
mod msg23_0253;
mod pool_actions;
mod pool_render;
mod reaudit3_0218;
mod reaudit_0212;
mod real_data_rel3s2;
mod stale_state_rel3s2b;
mod stale_links_rel3s2c;
mod stale_refs_rel3s2d;
mod rename_0203;
mod review_165;
mod rows_registry_and_rename;
mod seam_conformance;
mod seam_reference;
mod seed_live;
mod settings;
mod structure_186;
mod threads_186;
mod threads_live;
mod tree_subject_mark;
mod visual_186;
mod reaudit4_0223;
mod reaudit5_0250;
mod g1_merge_0256;
mod reaudit7_0256;
mod refs_0259;
mod msg403_0263;
mod msg457_0310;
mod reaudit8_0303;
mod reaudit9_k_0310;
mod opener_guard_0327;
mod rel6a4;
mod rel6a4b;
mod fix_rv10_b;
