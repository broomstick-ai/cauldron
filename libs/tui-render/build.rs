//! Tells the frame-budget test whether it is measuring OPTIMISED code.
//!
//! `[profile.release]` keeps `debug-assertions` on (workspace Cargo.toml), so
//! `cfg!(debug_assertions)` cannot tell an optimised build from a debug one.
//! `OPT_LEVEL` can, and only a build script is handed it.
fn main() {
    println!("cargo:rustc-check-cfg=cfg(tui_render_optimized)");
    println!("cargo:rerun-if-env-changed=OPT_LEVEL");
    if std::env::var("OPT_LEVEL").map(|o| o != "0").unwrap_or(false) {
        println!("cargo:rustc-cfg=tui_render_optimized");
    }
}
