//! `decl::all` is a SCAN, not a `Result` collect: one declaration that does
//! not load is named in `broken` with its reason, and every other one still
//! comes back in `ok`. Before this, one unparsable file killed every bare
//! verb (`cauldron status` with no service named died with it).
//!
//! Proven at N >= 2 on the failure side — two DIFFERENT ways to be broken
//! (unparsable TOML; a file whose `name` disagrees with its filename) — and
//! with a POSITIVE SENTINEL on the success side: the shipped `tally` and
//! `echo` declarations must still load through the same scan, so an empty
//! `ok` is distinguishable from a scan that cannot see anything.

use std::path::PathBuf;

fn fixture_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "cauldron-decl-scan-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let services = root.join("services");
    std::fs::create_dir_all(&services).unwrap();

    // A GOOD one: the shipped `tally` text, renamed. Nothing else about it
    // changes, so if `tally` loads, this loads, and if this fails to load
    // the scan is wrong, not the fixture.
    let shipped = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .join("services")
        .join("tally.toml");
    let tally = std::fs::read_to_string(&shipped).unwrap();
    assert!(tally.contains("name        = \"tally\""), "fixture premise: shipped tally.toml names itself");
    let good = tally.replacen("name        = \"tally\"", "name = \"zz-scan-good\"", 1);
    std::fs::write(services.join("zz-scan-good.toml"), good).unwrap();

    // BROKEN one: not TOML at all.
    std::fs::write(
        services.join("zz-scan-unparsable.toml"),
        "name = \"zz-scan-unparsable\"\nkind = \"daemon\"\nthis is not toml ==\n",
    )
    .unwrap();

    // BROKEN two: valid TOML whose `name` disagrees with its filename.
    let misnamed = tally.replacen("name        = \"tally\"", "name = \"somebody-else\"", 1);
    std::fs::write(services.join("zz-scan-misnamed.toml"), misnamed).unwrap();

    services
}

#[test]
fn a_broken_declaration_is_named_and_the_rest_still_load() {
    let services = fixture_root();
    let scan = env::decl::all(&services);

    let ok: Vec<&str> = scan.ok.iter().map(|d| d.name.as_str()).collect();
    let broken: Vec<&str> = scan.broken.iter().map(|(n, _)| n.as_str()).collect();

    // POSITIVE SENTINEL: the scan can see things at all. Shipped declarations
    // resolve through the same search path, so they must be here.
    assert!(ok.contains(&"tally"), "sentinel: shipped tally must load; ok = {ok:?}");
    assert!(ok.contains(&"echo"), "sentinel: shipped echo must load; ok = {ok:?}");

    // The good root declaration loads beside them.
    assert!(ok.contains(&"zz-scan-good"), "the loadable root declaration is in ok; ok = {ok:?}");

    // Both broken ones are NAMED, not swallowed and not fatal.
    assert_eq!(
        broken,
        vec!["zz-scan-misnamed", "zz-scan-unparsable"],
        "both broken declarations are named, sorted, and nothing else is"
    );
    assert!(!ok.contains(&"zz-scan-unparsable"));
    assert!(!ok.contains(&"zz-scan-misnamed"));

    // Each carries ITS reason, and the two reasons differ: two failure modes,
    // not one message for everything.
    let why = |name: &str| -> String {
        scan.broken.iter().find(|(n, _)| n == name).map(|(_, e)| format!("{e:#}")).unwrap()
    };
    assert!(
        why("zz-scan-unparsable").contains("parsing the service declaration"),
        "unparsable names the parse: {}",
        why("zz-scan-unparsable")
    );
    assert!(
        why("zz-scan-misnamed").contains("names service `somebody-else`"),
        "misnamed names the disagreement: {}",
        why("zz-scan-misnamed")
    );

    // `ok` is sorted by name, as `all` always was.
    let mut sorted = ok.clone();
    sorted.sort();
    assert_eq!(ok, sorted);

    let _ = std::fs::remove_dir_all(services.parent().unwrap());
}
