//! **R5.S3 — the declaration keys that let cauldron host every service,
//! refused when they contradict themselves and accepted when they do not.**
//!
//! Each refusal is paired with the nearest declaration that LOADS, so a
//! validator that refused everything (or nothing) is red here.

use env::decl::{self, Declaration};

fn load(name: &str, body: &str) -> anyhow::Result<Declaration> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "r5s3-decl-{name}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join(format!("{name}.toml")), body).unwrap();
    let r = decl::load(&root, name);
    let _ = std::fs::remove_dir_all(&root);
    r
}

fn err(name: &str, body: &str) -> String {
    let e = load(name, body).expect_err("must be refused");
    let m = format!("{e:#}");
    println!("REFUSED {name}: {m}");
    m
}

const HOSTED: &str = "name = \"h\"\nregisters = false\n[build]\nartifact = \"/bin/true\"\nbin = \"true\"\n";

#[test]
fn the_shipped_ui_server_and_soup_backup_load_with_their_r5s3_keys() {
    let empty = std::env::temp_dir().join(format!("r5s3-shipped-{}", std::process::id()));
    // SENTINEL (R5.S4, BLOCK C): the declarations read below are the ones in
    // THIS test's worktree. Two things compared: the `env` LIBRARY's compiled-in
    // search path and this TEST crate's own manifest dir. In a target dir shared
    // by worktrees the lib can be another worktree's build (measured: an `env`
    // rlib whose shipped declarations were cauldron-wt-r5s3's), and every
    // assertion below would then be about the wrong tree.
    let own = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).unwrap().join("services");
    assert_eq!(decl::search_paths(&empty)[1], own, "the env lib under test was built from another worktree");
    let ui = decl::load(&empty, "ui-server").expect("ui-server loads");
    assert_eq!(ui.probe, "http");
    assert_eq!(ui.restart, "always");
    let l = ui.launch.as_ref().unwrap();
    assert_eq!(l.health.as_deref(), Some("http://127.0.0.1:9010/health"));
    // REL-4.S4b (REL-4.S4-I F3): the declaration says `~/code/harness`, and the
    // loaded value is that path under the real home — never a literal `~`,
    // which nothing downstream expands and which names a directory that does
    // not exist. The expected value is built from `$HOME` here, independently.
    let home = std::env::var("HOME").expect("HOME");
    assert!(home.starts_with('/') && home != "/", "SENTINEL: a real absolute HOME, got {home:?}");
    assert_eq!(l.cwd.as_deref(), Some(format!("{home}/code/harness").as_str()));
    assert_eq!(ui.build().unwrap().bin, "AUI.app/Contents/MacOS/ui-server");
    assert_eq!(
        ui.build().unwrap().artifact.as_deref(),
        Some(format!("{home}/Applications/AUI.app").as_str()),
        "REL-4.S4b: the bundle artifact's `~` is expanded at load"
    );

    let alarm = decl::load(&empty, "claude-account-expiry-alarm").expect("the alarm loads");
    let a = alarm.schedule.as_ref().expect("R23: the alarm declares its schedule");
    assert_eq!((a.at.as_deref(), a.run_at_load), (Some("09:30"), false));
    // R5.S4b: the cut-over (9c12b15d, Deploy 4) deleted `[migration]`; this
    // assertion still said `is_some()` and was red on `main` from that commit.
    assert!(alarm.migration.is_none(), "R5.S4 cut-over (9c12b15d): the alarm is a cauldron job, not launchd's");
    // R5.S4: the installed copy is TOLD where its reader and its store are —
    // without these two it runs from the version store and finds neither.
    assert_eq!(
        alarm.env.get("CLAUDE_ACCOUNTS_HOME").map(String::as_str),
        Some("${CAULDRON_SERVICE_DATA_DIR}/../threads/store"),
        "R5.S4: the alarm names its account store"
    );
    assert_eq!(
        alarm.env.get("EXPIRY_ALARM_CLAUDE_ACCOUNTS").map(String::as_str),
        Some(format!("{home}/code/cauldron/services/threads/scripts/claude-accounts").as_str()),
        "R5.S4: the alarm names the claude-accounts it reads through"
    );
    assert_eq!(
        alarm.build().unwrap().artifact.as_deref(),
        Some(format!("{home}/code/cauldron/services/threads/scripts/claude-account-expiry-alarm").as_str()),
        "REL-4.S4b: the alarm's artifact `~` is expanded at load"
    );

    let sb = decl::load(&empty, "soup-backup").expect("soup-backup loads");
    assert_eq!(sb.kind, "job");
    let s = sb.schedule.as_ref().expect("a schedule");
    assert_eq!((s.every_secs, s.args.clone(), s.health_args.clone()), (300, vec!["run".into()], vec!["status".into()]));
    assert_eq!(s.requires, vec!["${CAULDRON_SERVICE_DATA_DIR}/config".to_string()]);
    assert_eq!(sb.env["SOUP_BACKUP_CONFIG"], "${CAULDRON_SERVICE_DATA_DIR}/config");
    // r42 SB-1: the installed image states its commit. Without `stamp_args`
    // every build installs as `unstamped-<hash>` with an empty commit (how
    // 0232d4f5 went live as `unstamped-98b47d1e2adfe5de`); with it, an image
    // that cannot answer is refused at build rather than pinned unstamped.
    let b = sb.build().expect("soup-backup has a [build]");
    assert_eq!(b.stamp_args, vec!["--build-stamp".to_string()], "r42 SB-1: soup-backup's image must state its commit");
    // REL-4.S7g: the declaration says `~/.cache/...` (no literal home path in a
    // shipped file); the loaded value is that path under the real home, built
    // here from `$HOME`, and the SAME path under the passwd home, which does
    // not go through `$HOME` at all: on the operator's machine this is the
    // path the old literal named, so the live job still finds its image.
    let sb_art = format!("{home}/.cache/cauldron-artifacts/soup-backup/soup-backup");
    assert_eq!(
        b.artifact.as_deref(),
        Some(sb_art.as_str()),
        "r42 SB-1: the artifact is scripts/stamp-script's output, not a live working-tree file"
    );
    let pw = env::guard::passwd_home().expect("SENTINEL: a passwd home");
    assert_eq!(
        std::path::Path::new(&sb_art),
        pw.join(".cache/cauldron-artifacts/soup-backup/soup-backup"),
        "REL-4.S7g: soup-backup's artifact resolves under the real (passwd) home, not a scratch or fake one"
    );
}

#[test]
fn restart_is_always_or_never_and_nothing_else() {
    let base = format!("{HOSTED}[launch]\naddr = \"127.0.0.1:1\"\n");
    let with = |r: &str| format!("restart = \"{r}\"\nprobe = \"connect\"\n{base}");
    assert!(err("h", &with("sometimes")).contains("restart = \"sometimes\""));
    assert_eq!(load("h", &with("never")).unwrap().restart, "never");
    assert_eq!(load("h", &with("always")).unwrap().restart, "always");
    // absent = always
    assert_eq!(load("h", &format!("probe = \"connect\"\n{base}")).unwrap().restart, "always");
}

#[test]
fn http_probe_needs_a_health_url_and_a_health_url_needs_the_http_probe() {
    let no_url = format!("probe = \"http\"\n{HOSTED}[launch]\naddr = \"127.0.0.1:1\"\n");
    assert!(err("h", &no_url).contains("no `[launch] health`"));
    let unasked = format!(
        "probe = \"connect\"\n{HOSTED}[launch]\naddr = \"127.0.0.1:1\"\nhealth = \"http://127.0.0.1:2/h\"\n"
    );
    assert!(err("h", &unasked).contains("never asks it"));
    let not_http = format!("probe = \"http\"\n{HOSTED}[launch]\naddr = \"127.0.0.1:1\"\nhealth = \"https://x/h\"\n");
    assert!(err("h", &not_http).contains("not an `http://"));
    let good = format!("probe = \"http\"\n{HOSTED}[launch]\naddr = \"127.0.0.1:1\"\nhealth = \"http://127.0.0.1:2/h\"\ncwd = \"/tmp\"\n");
    let d = load("h", &good).expect("http + health loads");
    assert_eq!(d.launch.unwrap().cwd.as_deref(), Some("/tmp"));
}

const JOB: &str = "name = \"j\"\nkind = \"job\"\nregisters = false\nprobe = \"none\"\n\
                   [build]\nartifact = \"/bin/true\"\nbin = \"true\"\n";

#[test]
fn a_job_with_a_schedule_is_hosted_and_one_with_neither_is_refused() {
    let m = err("j", JOB);
    assert!(m.contains("[schedule]") && m.contains("[migration]"), "{m}");
    let d = load("j", &format!("{JOB}[schedule]\nevery_secs = 60\nargs = [\"run\"]\n")).expect("scheduled job loads");
    let s = d.schedule.unwrap();
    assert_eq!((s.every_secs, s.retry_secs, s.timeout_secs, s.run_at_load), (60, 30, 600, true));
    assert!(d.migration.is_none());
}

#[test]
fn a_schedule_contradicting_itself_is_refused() {
    assert!(err("j", &format!("{JOB}[schedule]\nevery_secs = 0\n")).contains("every_secs = 0"));
    // R23: the daily calendar form — one clock, a real time of day.
    assert!(err("j", &format!("{JOB}[schedule]\nevery_secs = 5\nat = \"09:30\"\n")).contains("One clock"));
    assert!(err("j", &format!("{JOB}[schedule]\nat = \"25:00\"\n")).contains("[schedule] at"));
    let d = load("j", &format!("{JOB}[schedule]\nat = \"09:30\"\nrun_at_load = false\n")).expect("a daily job loads");
    assert_eq!(d.schedule.unwrap().at.as_deref(), Some("09:30"));
    assert!(err("j", &format!("{JOB}[schedule]\nevery_secs = 5\ntimeout_secs = 0\n")).contains("timeout_secs = 0"));
    // a daemon with a schedule
    let daemon = format!("probe = \"connect\"\n{HOSTED}[launch]\naddr = \"127.0.0.1:1\"\n[schedule]\nevery_secs = 5\n");
    assert!(err("h", &daemon).contains("has a `[schedule]`"));
    // a job with a probe
    let probed = JOB.replace("probe = \"none\"", "probe = \"connect\"") + "[schedule]\nevery_secs = 5\n";
    assert!(err("j", &probed).contains("probe = \"connect\""));
}

#[test]
fn a_bundle_bin_must_start_with_the_bundle_and_stay_inside() {
    let b = |artifact: &str, bin: &str| {
        format!(
            "name = \"u\"\nregisters = false\nprobe = \"connect\"\n[launch]\naddr = \"127.0.0.1:1\"\n\
             [build]\nartifact = \"{artifact}\"\nbin = \"{bin}\"\n"
        )
    };
    assert!(load("u", &b("/x/AUI.app", "AUI.app/Contents/MacOS/ui-server")).is_ok());
    assert!(err("u", &b("/x/AUI.app", "Other.app/Contents/MacOS/ui-server")).contains("must start with"));
    assert!(err("u", &b("/x/AUI.app", "AUI.app/../../etc/passwd")).contains("no `..`"));
    assert!(err("u", &b("/x/AUI.app", "/abs/ui-server")).contains("relative"));
    // a path bin on a cargo source
    let cargo = "name = \"u\"\n[build]\npackage = \"p\"\nbin = \"a/b\"\n";
    assert!(err("u", cargo).contains("only a BUNDLE artifact"));
}

/// **R9.C6 (gap C-6), doctrine 10: WHO IS ON THE BIND-CHECK-ONLY PROBE.**
///
/// `probe = "connect"` is a degraded liveness regime — the kernel completes
/// the handshake for a wedged process — so the shipped population on it is
/// NAMED here rather than left to whoever reads the files. Threads left it in
/// R9.C6: it ships `http` against `/health` on its own chassis address, per
/// environment. Counterparts, so the predicate is not matching the directory
/// at large: ui-server is `http` on a different port, tts is `seam`, the jobs
/// are `none`.
#[test]
fn r9c6_threads_ships_the_http_probe_on_its_own_door_and_no_shipped_service_is_on_connect() {
    let empty = std::env::temp_dir().join(format!("r9c6-shipped-{}", std::process::id()));
    let own = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).unwrap().join("services");
    assert_eq!(decl::search_paths(&empty)[1], own, "the env lib under test was built from another worktree");

    let t = decl::load(&empty, "threads").expect("threads loads");
    assert_eq!(t.probe, "http", "C-6: threads must not be on a bind-check-only probe");
    let l = t.launch.as_ref().expect("threads is hosted");
    // R9.C7i: the health URL is threads' OFF-RUNTIME health door — the IPv6
    // loopback on the chassis's own port number, per env — not the chassis
    // door (`services/threads/tests/health_off_runtime.rs` G5 pins the rest).
    assert_eq!(
        l.health.as_deref(),
        Some("http://[::1]:${CAULDRON_HOSTED_PORT}/health"),
        "health on the off-runtime door, per env"
    );
    assert_eq!(l.addr, "127.0.0.1:3653");
    assert!(t.start_timeout_ms >= 120_000, "the http launch waits for the whole boot: {}", t.start_timeout_ms);

    // Every shipped declaration, by its probe.
    let mut by_probe: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for e in std::fs::read_dir(&own).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) != Some("toml") {
            continue;
        }
        let name = p.file_stem().unwrap().to_str().unwrap().to_string();
        let d = decl::load(&empty, &name).unwrap_or_else(|e| panic!("{name}: {e:#}"));
        by_probe.entry(d.probe.clone()).or_default().push(name);
    }
    for v in by_probe.values_mut() {
        v.sort();
    }
    println!("R9.C6 shipped probes: {by_probe:?}");
    assert!(by_probe.values().map(Vec::len).sum::<usize>() >= 7, "the scan reached the shipped set: {by_probe:?}");
    assert_eq!(by_probe.get("connect"), None, "a shipped service on `connect` (bind check only): {by_probe:?}");
    assert_eq!(by_probe.get("http").cloned().unwrap_or_default(), vec!["threads".to_string(), "ui-server".to_string()]);
    assert!(by_probe["seam"].contains(&"tts".to_string()), "{by_probe:?}");
    assert!(by_probe["none"].contains(&"soup-backup".to_string()), "{by_probe:?}");
}

/// **REL-4.S4b (REL-4.S4-I F3): `~` expands only where it means a home.**
/// A leading `~` / `~/` in `cwd`, `artifact` and an `[env]` value becomes the
/// home; `~user`, a mid-string `~`, and a `${...}` substitution are left
/// alone. Paired both ways, so neither "expand nothing" nor "expand every `~`"
/// passes.
#[test]
fn a_leading_tilde_expands_to_home_and_nothing_else_does() {
    let home = std::env::var("HOME").expect("HOME");
    assert!(home.starts_with('/') && home != "/", "SENTINEL: a real absolute HOME, got {home:?}");
    let body = "name = \"h\"\nregisters = false\nprobe = \"connect\"\n\
                [env]\nA = \"~/x/y\"\nB = \"~other/x\"\nC = \"a~/b\"\nD = \"${CAULDRON_SERVICE_DATA_DIR}/z\"\nE = \"~\"\n\
                [launch]\naddr = \"127.0.0.1:1\"\ncwd = \"~/code/harness\"\n\
                [build]\nartifact = \"~/Applications/AUI.app\"\nbin = \"AUI.app/Contents/MacOS/ui-server\"\n";
    let d = load("h", body).expect("loads");
    assert_eq!(d.launch.as_ref().unwrap().cwd.as_deref(), Some(format!("{home}/code/harness").as_str()));
    assert_eq!(d.build().unwrap().artifact.as_deref(), Some(format!("{home}/Applications/AUI.app").as_str()));
    assert_eq!(d.env["A"], format!("{home}/x/y"));
    assert_eq!(d.env["E"], home);
    // and NOT these
    assert_eq!(d.env["B"], "~other/x");
    assert_eq!(d.env["C"], "a~/b");
    assert_eq!(d.env["D"], "${CAULDRON_SERVICE_DATA_DIR}/z");
    assert!(!d.text.contains(&home), "the snapshot TEXT is the file as written, unexpanded");
}
