//! **PL-0 guard: protoledger is linked at the pinned public release with
//! exactly `store` + `sync` + `sqlite`, and never `serve` or `cli`.**
//! (docs/PROTOLEDGER-INTEGRATION.md rev 3, §3 and the PL-0 row.)
//!
//! `serve` pulls tokio + axum into the daemon and `cli` pulls clap; neither may
//! ever reach this crate. The check reads what cargo actually RESOLVED for this
//! workspace (`cargo metadata`, the same resolution `cargo tree` prints), not
//! the text of `Cargo.toml`: a feature turned on by any route (this crate, a
//! sibling crate, a dev-dependency) lands in the resolved set and turns this red.
//!
//! The test reads the manifest at RUN time, so the instrument is the resolver,
//! and the forcings in an internal report edit `Cargo.toml` (adding
//! `serve`, then `cli`, then dropping `sync`) and watch this go red.

use serde_json::Value;

const PINNED_REPO: &str = "git+https://github.com/broomstick-ai/protoledger";
/// GitHub 9cfec2d4: tree-identical (14ade0b9) to protoledger main 6b794ae8.
const PINNED_REV: &str = "9cfec2d49672e5d113ddc5f79d1ae5836a21e892";
const WANTED: [&str; 3] = ["sqlite", "store", "sync"];
const BANNED: [&str; 2] = ["serve", "cli"];

/// The host triple, so `--offline` resolves only packages this machine builds
/// (an unfiltered resolve wants every platform's crates downloaded).
fn host_triple() -> String {
    use std::env::consts::{ARCH, OS};
    match OS {
        "macos" => format!("{ARCH}-apple-darwin"),
        "linux" => format!("{ARCH}-unknown-linux-gnu"),
        other => panic!("add a host triple for {other}"),
    }
}

fn metadata() -> Value {
    let host = host_triple();
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml");
    let out = std::process::Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--offline", "--locked", "--filter-platform", &host])
        .args(["--manifest-path", manifest])
        .output()
        .expect("run cargo metadata");
    assert!(
        out.status.success(),
        "cargo metadata failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("cargo metadata emits JSON")
}

/// Every resolved package named `name`, as (package id, resolved features).
fn resolved(meta: &Value, name: &str) -> Vec<(String, Vec<String>)> {
    let ids: Vec<String> = meta["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .filter(|p| p["name"] == name)
        .map(|p| p["id"].as_str().expect("id").to_string())
        .collect();
    let nodes = meta["resolve"]["nodes"].as_array().expect("resolve.nodes");
    ids.into_iter()
        .map(|id| {
            let node = nodes.iter().find(|n| n["id"] == id.as_str()).expect("a resolve node per package");
            let mut f: Vec<String> =
                node["features"].as_array().expect("features").iter().map(|x| x.as_str().unwrap().to_string()).collect();
            f.sort();
            (id, f)
        })
        .collect()
}

#[test]
fn protoledger_is_the_pinned_release_with_store_sync_sqlite_and_never_serve_or_cli() {
    let meta = metadata();
    let pl = resolved(&meta, "protoledger");
    assert!(!pl.is_empty(), "no protoledger in the build: the instrument reads nothing");

    // THE BAN IS BY ANY ROUTE (module doc): every protoledger the workspace
    // resolves, whoever links it. The build may hold more than one — since
    // RV1.S3 another workspace member may link its own pin (model types only,
    // `default-features = false`) beside this crate's — and `serve`/`cli` must reach
    // NONE of them. Stated on its own so a red names the package and the
    // feature.
    for (id, features) in &pl {
        for banned in BANNED {
            assert!(
                !features.iter().any(|f| f == banned),
                "{id}: protoledger feature `{banned}` is enabled in this build (resolved: {features:?}); \
                 the daemon must never link `serve` (tokio + axum) or `cli` (clap), by any route"
            );
        }
    }

    // THE PIN AND THE EXACT SET are judged on the protoledger THIS crate
    // depends on — the one `threads`' own resolve node names — not on
    // whichever protoledger the resolve listed first.
    let packages = meta["packages"].as_array().unwrap();
    let own_manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
    let threads_id = packages
        .iter()
        .find(|p| p["name"] == "threads" && p["manifest_path"] == own_manifest)
        .and_then(|p| p["id"].as_str())
        .expect("this crate is in the resolve");
    let threads_node = meta["resolve"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == threads_id)
        .expect("a resolve node for this crate");
    let mine: Vec<&(String, Vec<String>)> = pl
        .iter()
        .filter(|(id, _)| {
            threads_node["deps"].as_array().unwrap().iter().any(|d| d["pkg"] == id.as_str())
        })
        .collect();
    assert_eq!(mine.len(), 1, "exactly one protoledger reaches this crate, got {mine:?} (all resolved: {pl:?})");
    let (id, features) = mine[0];

    // The pinned rev, and only it: the package source carries both the rev
    // asked for (`?rev=`) and the commit Cargo.lock resolved it to (`#`).
    let source = packages
        .iter()
        .find(|p| p["id"] == id.as_str())
        .and_then(|p| p["source"].as_str())
        .expect("protoledger has a source");
    assert!(
        source == format!("{PINNED_REPO}?rev={PINNED_REV}#{PINNED_REV}") && source.ends_with(&format!("?rev={PINNED_REV}#{PINNED_REV}")),
        "protoledger is not the pinned public release {PINNED_REV}: {source}"
    );
    // And the exact set: a constant "no features" passes the ban above but not this.
    assert_eq!(features, &WANTED.map(String::from).to_vec(), "protoledger features drifted");
}

#[test]
fn serde_json_is_built_without_preserve_order_or_arbitrary_precision() {
    let meta = metadata();
    let sj = resolved(&meta, "serde_json");
    assert!(!sj.is_empty(), "serde_json not found in the resolve: the instrument reads nothing");
    for (id, features) in &sj {
        // Positive sentinel: protoledger itself requires `raw_value`, so a
        // resolve that lacks it is not the build protoledger is in.
        assert!(features.iter().any(|f| f == "raw_value"), "{id}: raw_value missing ({features:?})");
        for banned in ["preserve_order", "arbitrary_precision"] {
            assert!(!features.iter().any(|f| f == banned), "{id}: serde_json `{banned}` is on ({features:?})");
        }
    }
}
