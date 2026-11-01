//! The loader: the dataset lands in the environment's OWN seed, in the face's
//! formats, with every artifact pointing at a real file; and it lands only in
//! `cauldron-build`.

use std::path::PathBuf;

use soup::demo::{install, install_from, seed_env, Dataset, Source, ENV, FILES_TOKEN};

/// A fresh directory under the system temp dir, removed on drop.
struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Tmp {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let p = std::env::temp_dir().join(format!("soup-it-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn env_named(root: &std::path::Path, name: &str) -> env::Environment {
    env::Environment {
        root: root.to_path_buf(),
        root_source: env::paths::Source::CauldronHome,
        name: name.to_string(),
        name_source: env::paths::Source::Default,
        data_root: root.join("data").join(name),
    }
}

#[test]
fn install_writes_the_seed_the_face_reads_with_every_file_real() {
    let t = Tmp::new("install");
    let soup_dir = t.0.join("tui").join("soup");
    let got = install(&soup_dir).expect("install");
    let seed = soup_dir.join("seed");
    assert_eq!(got.seed_dir, seed);
    assert_eq!(got.wrote.last().unwrap(), &seed.join("clusters.json"), "clusters.json is written last");

    // The clusters and ledger are the dataset, byte for byte.
    let ds = Dataset::embedded().unwrap();
    let read = |n: &str| serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(seed.join(n)).unwrap()).unwrap();
    assert_eq!(read("clusters.json"), ds.clusters);
    assert_eq!(read("ledger.json"), ds.ledger);

    // Every artifact points at an installed file whose bytes it verified.
    let arts = read("artifacts.json");
    let text = serde_json::to_string(&arts).unwrap();
    assert!(!text.contains(FILES_TOKEN), "a placeholder survived install");
    let list = arts["artifacts"].as_array().unwrap();
    assert!(!list.is_empty());
    for a in list {
        let loc = PathBuf::from(a["location"].as_str().unwrap());
        assert!(loc.starts_with(seed.join("files")), "{} is outside the environment's seed", loc.display());
        let bytes = std::fs::read(&loc).unwrap_or_else(|e| panic!("{}: {e}", loc.display()));
        assert_eq!(a["verified"]["bytes"].as_u64().unwrap() as usize, bytes.len(), "{}", loc.display());
        use sha2::Digest;
        let sha: String = sha2::Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(a["verified"]["sha256"].as_str().unwrap(), sha, "{}", loc.display());
    }
    let dir = arts["directories"][0]["path"].as_str().unwrap();
    assert_eq!(PathBuf::from(dir), seed.join("files"));

    // Idempotent: a second install leaves the same seed.
    install(&soup_dir).expect("re-install");
    assert_eq!(read("clusters.json"), ds.clusters);
}

/// The hook seeds `cauldron-build`, at `<data_root>/tui/soup/seed`.
#[test]
fn seed_env_seeds_cauldron_build() {
    let t = Tmp::new("seed-ok");
    let e = env_named(&t.0, ENV);
    let got = seed_env(&e).expect("cauldron-build is seeded");
    assert_eq!(got.seed_dir, t.0.join("data").join(ENV).join("tui").join("soup").join("seed"));
    assert!(got.seed_dir.join("clusters.json").is_file());
}

/// Opposes the test above: any other environment is refused and nothing is
/// written, so a fresh environment starts empty.
#[test]
fn seed_env_refuses_every_other_environment() {
    for name in ["prod", "dev", "demo", "my-first"] {
        let t = Tmp::new("seed-refused");
        let e = env_named(&t.0, name);
        let err = seed_env(&e).expect_err(name).to_string();
        assert!(err.contains("REFUSED") && err.contains(name), "{err}");
        assert!(!t.0.join("data").exists(), "{name}: something was written");
    }
}

/// V1.8: the curated secret steps route `env`, so install points the
/// environment's `secrets.env_file` inside its own data dir.
#[test]
fn install_points_secrets_env_file_inside_the_environment() {
    let t = Tmp::new("secrets");
    let soup_dir = t.0.join("tui").join("soup");
    let got = install(&soup_dir).unwrap();
    assert_eq!(got.env_file, soup_dir.join("secrets.env"));
    let text = std::fs::read_to_string(soup_dir.join("settings.toml")).unwrap();
    let v: toml_lite::Table = toml_lite::parse(&text);
    assert_eq!(v.get("secrets.env_file").map(String::as_str), Some(got.env_file.to_str().unwrap()), "{text}");
}

/// Opposes the test above: a settings file he already has keeps every line,
/// and one that already has a `[secrets]` table is not touched at all.
#[test]
fn install_keeps_his_settings() {
    let t = Tmp::new("settings");
    let soup_dir = t.0.join("tui").join("soup");
    std::fs::create_dir_all(&soup_dir).unwrap();
    let mine = "# mine\n[pools.claude_code.budget.weekly]\nthreshold_pct = 80\n";
    std::fs::write(soup_dir.join("settings.toml"), mine).unwrap();
    install(&soup_dir).unwrap();
    let text = std::fs::read_to_string(soup_dir.join("settings.toml")).unwrap();
    assert!(text.starts_with(mine) && text.contains("[secrets]"), "{text}");

    let his = "[secrets]\nenv_file = \"/elsewhere/.env\"\n";
    std::fs::write(soup_dir.join("settings.toml"), his).unwrap();
    install(&soup_dir).unwrap();
    assert_eq!(std::fs::read_to_string(soup_dir.join("settings.toml")).unwrap(), his);
}

/// The embedded dataset with its one secret `route: env` line replaced.
fn with_release_token_route(route_line: &str) -> String {
    let needle = r"name: RELEASE_TOKEN\nlabel: Release token\nroute: env\n";
    assert_eq!(Source::EMBEDDED.clusters.matches(needle).count(), 1, "the fixture's anchor moved");
    Source::EMBEDDED.clusters.replacen(needle, &format!(r"name: RELEASE_TOKEN\nlabel: Release token\n{route_line}"), 1)
}

/// The guard holds in `install()` itself, not only in `Dataset::embedded()`:
/// a dataset whose secret field is routed to the node, or names no route, is
/// refused and NOTHING is written (not even the seed directory).
#[test]
fn install_refuses_an_unroutable_action_and_writes_nothing() {
    for (case, line) in [("secret-to-node", r"route: node\n"), ("unrouted", ""), ("unknown-route", r"route: vault\n")] {
        let t = Tmp::new("install-refused");
        let soup_dir = t.0.join("tui").join("soup");
        let clusters = with_release_token_route(line);
        let src = Source { clusters: &clusters, ..Source::EMBEDDED };
        let err = format!("{:#}", install_from(&soup_dir, &src).expect_err(case));
        assert!(err.contains("REFUSED") && err.contains("RELEASE_TOKEN"), "{case}: {err}");
        let left: Vec<_> = std::fs::read_dir(&t.0).unwrap().map(|e| e.unwrap().path()).collect();
        assert!(left.is_empty(), "{case}: install wrote {left:?}");
    }
}

/// Opposes the test above: the same edit kept legal (another legal route)
/// installs, so the refusal is the validator's, not the edit's.
#[test]
fn install_from_accepts_a_variant_that_stays_routable() {
    let t = Tmp::new("install-legal-variant");
    let soup_dir = t.0.join("tui").join("soup");
    let clusters = with_release_token_route(r"route: keychain\n");
    let src = Source { clusters: &clusters, ..Source::EMBEDDED };
    let got = install_from(&soup_dir, &src).expect("a keychain route is legal");
    let written = std::fs::read_to_string(got.seed_dir.join("clusters.json")).unwrap();
    assert_eq!(written, clusters);
}

/// A two-level `[table]` / `key = "value"` reader, enough for the one line
/// install writes (the crate takes no TOML dependency for a test).
mod toml_lite {
    pub type Table = std::collections::BTreeMap<String, String>;
    pub fn parse(text: &str) -> Table {
        let (mut t, mut table) = (Table::new(), String::new());
        for l in text.lines().map(str::trim) {
            if let Some(n) = l.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                table = n.to_string();
            } else if let Some((k, v)) = l.split_once('=') {
                let v: String = serde_json::from_str(v.trim()).unwrap_or_else(|_| v.trim().to_string());
                t.insert(format!("{table}.{}", k.trim()), v);
            }
        }
        t
    }
}
