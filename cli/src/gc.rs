//! **DG-1 (implements DG-1): `cauldron gc --builds [--keep N] [--dry-run]`.**
//!
//! Builds are shared across environments (`data/shared/builds/<svc>/<label>`)
//! and until this verb nothing ever removed one: the census
//! (`CAULDRON-DATA-census.md` §3) measured 34 build dirs, ~495 MiB of them
//! obsolete, growing ~50 MB/day while deploying, with a manual `rm -rf` as the
//! only way out — which breaks the rule that the root is touched only through
//! cauldron.
//!
//! # What is KEPT, per service
//!
//! * every environment's PIN (a `latest` pin is resolved to the label it runs);
//! * every environment's ROLLBACK TARGET — [`Manifest::rollback_target`] given
//!   the pin (what `rollback` and the daemon's auto-rollback re-pin), and
//!   [`Manifest::previous_good`] (what `versions` labels "PREVIOUS — rollback
//!   target"). Both, because a gc must not disagree with either reader;
//! * the `--keep N` newest (by `built_at`) builds proven good in ANY
//!   environment, beyond the ones already kept (default 0);
//! * any build a running process has OPEN — its text (an executable running
//!   from it), its cwd, or any open file — found with `lsof +D` over the build
//!   directory, checked when the plan is made AND again immediately before the
//!   directory is removed. If `lsof` cannot be run, nothing is removed;
//! * **THE DEPLOY WINDOW (DG-1-1 ruling, r43 ledger):** every build NEWER
//!   (by `built_at`) than the newest pin in any environment, and every build
//!   younger than [`GRACE_SECS`] (1 h). A deploy builds first and pins later
//!   (DEPLOY-9n: built 02:02, pinned 02:41), so a build that is not pinned
//!   YET is the build the next step needs;
//! * any directory with no readable `build.json`: `cauldron build` writes the
//!   record LAST, so that is an install in progress or not a build at all.
//!
//! Every environment's `env.toml` must parse, or the verb refuses outright: a
//! pin it cannot read is a pin it cannot keep. A real run holds every
//! environment's `env.toml` lock while it removes, so a concurrent `pin` cannot
//! pin a build out from under it. `--dry-run` takes no lock and writes nothing;
//! it prints exactly the plan a real run would execute, and the bytes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use env::decl::{self, BuildRecord, Manifest};
use env::paths::Source;
use env::Environment;

/// A build younger than this is never collected, pinned or not (DG-1-1).
pub(crate) const GRACE_SECS: u64 = 3600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Opts {
    pub keep: usize,
    pub dry_run: bool,
}

pub(crate) fn parse_opts(args: &[String]) -> Result<Opts> {
    let mut builds = false;
    let mut o = Opts { keep: 0, dry_run: false };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--builds" => builds = true,
            "--dry-run" | "-n" => o.dry_run = true,
            "--keep" => {
                i += 1;
                let v = args.get(i).context("`--keep` needs a number after it")?;
                o.keep = v.parse().with_context(|| format!("`--keep {v}`: not a count"))?;
            }
            _ if a.starts_with("--keep=") => {
                let v = &a["--keep=".len()..];
                o.keep = v.parse().with_context(|| format!("`{a}`: not a count"))?;
            }
            other => bail!("`cauldron gc`: unknown argument `{other}`"),
        }
        i += 1;
    }
    if !builds {
        bail!(
            "`cauldron gc` collects nothing unless told what: `cauldron gc --builds [--keep N] \
             [--dry-run]` is the only collection that exists"
        );
    }
    Ok(o)
}

/// One build directory and what the plan decided about it.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub label: String,
    pub dir: PathBuf,
    pub bytes: u64,
    /// Empty = remove.
    pub keep_why: Vec<String>,
}

#[derive(Debug, Default)]
pub(crate) struct Plan {
    pub envs: Vec<String>,
    /// service -> its build dirs, sorted by label.
    pub services: BTreeMap<String, Vec<Entry>>,
}

impl Plan {
    pub fn removals(&self) -> impl Iterator<Item = (&String, &Entry)> {
        self.services
            .iter()
            .flat_map(|(s, es)| es.iter().filter(|e| e.keep_why.is_empty()).map(move |e| (s, e)))
    }
}

/// **Who has `dir` open.** `lsof +D` walks the directory and reports every
/// process holding a vnode inside it — `txt` (an executable running from it),
/// `cwd`, or an open file. `Err` when lsof could not run at all: the caller
/// treats that as "cannot prove nobody holds it" and keeps the build.
pub(crate) fn holders(dir: &Path) -> Result<Vec<(u32, String)>> {
    let canon = dir.canonicalize().with_context(|| format!("resolving {}", dir.display()))?;
    let out = Command::new("lsof")
        .args(["-n", "-P", "-w", "-F", "pn", "+D"])
        .arg(&canon)
        .output()
        .context("running `lsof` to find processes holding a build open")?;
    lsof_verdict(&canon, out.status.code(), &out.stdout, &out.stderr)
}

/// Inspect m3: lsof exits 1 both when nothing is open and when it fails.
/// Measured: an existing dir nobody holds gives rc 1 with EMPTY stderr; a bad
/// path gives rc 1 with its usage on stderr. So rc 1 is "nothing open" only
/// when stderr is empty, and anything else non-zero is an error (the caller
/// then keeps the build).
pub(crate) fn lsof_verdict(
    dir: &Path,
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<Vec<(u32, String)>> {
    let err = String::from_utf8_lossy(stderr);
    match code {
        Some(0) => {}
        Some(1) if err.trim().is_empty() => {}
        _ => bail!("`lsof +D {}` failed ({code:?}): {}", dir.display(), err.trim()),
    }
    let mut v = Vec::new();
    let mut pid = 0u32;
    for line in String::from_utf8_lossy(stdout).lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.parse().unwrap_or(0);
        } else if let Some(n) = line.strip_prefix('n') {
            if pid != 0 && pid != std::process::id() {
                v.push((pid, n.to_string()));
            }
        }
    }
    Ok(v)
}

/// Disk bytes under `p` (allocated blocks, as `du` counts), not following
/// symlinks.
pub(crate) fn disk_bytes(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let Ok(md) = std::fs::symlink_metadata(p) else { return 0 };
    let mut n = md.blocks() * 512;
    if md.is_dir() {
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                n += disk_bytes(&e.path());
            }
        }
    }
    n
}

fn root_env(root: &Path) -> Environment {
    Environment {
        root: root.to_path_buf(),
        root_source: Source::CauldronHome,
        name: "prod".into(),
        name_source: Source::Default,
        data_root: root.join("data").join("prod"),
    }
}

fn add(keep: &mut BTreeMap<String, Vec<String>>, label: &str, why: String) {
    keep.entry(label.to_string()).or_default().push(why);
}

/// **The plan.** Reads every environment's `env.toml` and every build dir
/// under `<root>/data/shared/builds`; removes nothing. `open` is the
/// held-open check ([`holders`] in production).
pub(crate) fn plan(
    root: &Path,
    keep_n: usize,
    now: u64,
    open: &dyn Fn(&Path) -> Result<Vec<(u32, String)>>,
) -> Result<Plan> {
    let base = root_env(root);
    let envs = base.environments()?;
    let mut manifests: Vec<(String, Manifest)> = Vec::new();
    for n in &envs {
        let e = base.for_name(n)?;
        let m = Manifest::load(&e.manifest()).with_context(|| {
            format!(
                "environment `{n}`'s env.toml does not parse, so its pins cannot be known and \
                 nothing is collected"
            )
        })?;
        manifests.push((n.clone(), m));
    }
    let builds = base.shared_root().join("builds");
    let mut plan = Plan { envs: envs.clone(), ..Plan::default() };
    let rd = match std::fs::read_dir(&builds) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(plan),
        Err(e) => return Err(e).with_context(|| format!("listing {}", builds.display())),
    };
    let mut services: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|s| !s.starts_with('.'))
        .collect();
    services.sort();
    for svc in services {
        let sdir = builds.join(&svc);
        let mut recs: BTreeMap<String, Option<BuildRecord>> = BTreeMap::new();
        for e in std::fs::read_dir(&sdir)?.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            let Some(l) = e.file_name().to_str().map(str::to_string) else { continue };
            let rec = std::fs::read_to_string(e.path().join("build.json"))
                .ok()
                .and_then(|t| serde_json::from_str::<BuildRecord>(&t).ok());
            recs.insert(l, rec);
        }
        let mut keep: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (l, r) in &recs {
            if r.is_none() {
                add(&mut keep, l, "no readable build.json (install in progress, or not a build)".into());
            }
        }
        let mut good_any: BTreeSet<String> = BTreeSet::new();
        let mut newest_pin: Option<u64> = None;
        for (n, m) in &manifests {
            let pin = match m.pin(&svc) {
                Some(p) if p == "latest" => decl::resolve_version(&sdir, "latest", n).ok().map(|(l, _)| {
                    add(&mut keep, &l, format!("pinned in {n} (as `latest`)"));
                    l
                }),
                Some(p) => {
                    add(&mut keep, &p, format!("pinned in {n}"));
                    Some(p)
                }
                None => None,
            };
            if let Some(p) = &pin {
                if let Some(t) = m.rollback_target(&svc, p) {
                    add(&mut keep, &t, format!("rollback target in {n}"));
                }
            }
            if let Some(t) = m.previous_good(&svc) {
                add(&mut keep, &t, format!("previous good in {n}"));
            }
            good_any.extend(m.good(&svc));
            if let Some(t) = pin.as_ref().and_then(|p| recs.get(p)).and_then(|r| r.as_ref()) {
                newest_pin = newest_pin.max(Some(t.built_at));
            }
        }
        // DG-1-1: the deploy window. Newer than every pin, or younger than 1 h.
        for (l, r) in &recs {
            let Some(r) = r else { continue };
            if newest_pin.is_some_and(|t| r.built_at > t) {
                add(&mut keep, l, "newer than the newest pin in any environment (a deploy in progress)".into());
            }
            if now.saturating_sub(r.built_at) < GRACE_SECS {
                add(&mut keep, l, format!("built under {}s ago", GRACE_SECS));
            }
        }
        // --keep N: the N newest good builds not already kept.
        let mut extra: Vec<(u64, String)> = good_any
            .iter()
            .filter(|l| !keep.contains_key(*l))
            .filter_map(|l| recs.get(l).and_then(|r| r.as_ref()).map(|r| (r.built_at, l.clone())))
            .collect();
        extra.sort_by(|a, b| b.cmp(a));
        for (i, (_, l)) in extra.into_iter().take(keep_n).enumerate() {
            add(&mut keep, &l, format!("good, #{} of --keep {keep_n}", i + 1));
        }
        let mut entries = Vec::new();
        for l in recs.keys() {
            let dir = sdir.join(l);
            let mut why = keep.get(l).cloned().unwrap_or_default();
            if why.is_empty() {
                match open(&dir) {
                    Ok(h) if h.is_empty() => {}
                    Ok(h) => {
                        let pids: BTreeSet<u32> = h.iter().map(|(p, _)| *p).collect();
                        why.push(format!(
                            "OPEN by running pid {}",
                            pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
                        ));
                    }
                    Err(e) => why.push(format!("cannot check who holds it open: {e:#}")),
                }
            }
            entries.push(Entry { label: l.clone(), bytes: disk_bytes(&dir), dir, keep_why: why });
        }
        plan.services.insert(svc, entries);
    }
    Ok(plan)
}

fn mib(b: u64) -> String {
    format!("{:.1} MiB", b as f64 / 1048576.0)
}

pub(crate) fn render(plan: &Plan, root: &Path, o: &Opts) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "cauldron gc --builds{}: root {}, environments: {}; --keep {}\n",
        if o.dry_run { " (DRY RUN — nothing is removed)" } else { "" },
        root.display(),
        if plan.envs.is_empty() { "(none)".to_string() } else { plan.envs.join(", ") },
        o.keep
    ));
    for (svc, es) in &plan.services {
        s.push_str(&format!("{svc}:\n"));
        for e in es {
            let (verb, why) = if e.keep_why.is_empty() {
                (if o.dry_run { "WOULD REMOVE" } else { "REMOVE" }, String::new())
            } else {
                ("KEEP", e.keep_why.join("; "))
            };
            s.push_str(&format!("  {verb:<12} {:<40} {:>12} bytes  {why}\n", e.label, e.bytes));
        }
    }
    let (n, b) = plan.removals().fold((0usize, 0u64), |(n, b), (_, e)| (n + 1, b + e.bytes));
    s.push_str(&format!(
        "{} {n} build(s), {b} bytes ({})\n",
        if o.dry_run { "would remove" } else { "to remove" },
        mib(b)
    ));
    s
}

/// Remove what the plan says, re-checking each directory's holders right
/// before it goes. Returns the (service, label, bytes) actually removed.
pub(crate) fn execute(
    plan: &Plan,
    open: &dyn Fn(&Path) -> Result<Vec<(u32, String)>>,
) -> Result<Vec<(String, String, u64)>> {
    let mut done = Vec::new();
    for (svc, e) in plan.removals() {
        match open(&e.dir) {
            Ok(h) if h.is_empty() => {}
            Ok(h) => {
                eprintln!(
                    "cauldron: KEPT {svc}/{}: opened by pid {} since the plan was made",
                    e.label, h[0].0
                );
                continue;
            }
            Err(err) => {
                eprintln!("cauldron: KEPT {svc}/{}: cannot check holders: {err:#}", e.label);
                continue;
            }
        }
        std::fs::remove_dir_all(&e.dir).with_context(|| format!("removing {}", e.dir.display()))?;
        done.push((svc.clone(), e.label.clone(), e.bytes));
    }
    Ok(done)
}

pub(crate) fn verb_gc(args: &[String]) -> Result<i32> {
    let o = match parse_opts(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("cauldron: {e:#}");
            return Ok(2);
        }
    };
    let root = Environment::resolve()?.root;
    // A real run holds every environment's env.toml lock from the plan to the
    // last removal, so no pin can land on a build being removed.
    let mut _locks = Vec::new();
    if !o.dry_run {
        let base = root_env(&root);
        for n in base.environments()? {
            _locks.push(Manifest::lock_within(&base.for_name(&n)?.manifest(), Duration::from_secs(30))?);
        }
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(u64::MAX);
    let p = plan(&root, o.keep, now, &holders)?;
    print!("{}", render(&p, &root, &o));
    if o.dry_run {
        return Ok(0);
    }
    let done = execute(&p, &holders)?;
    let b: u64 = done.iter().map(|d| d.2).sum();
    for (s, l, n) in &done {
        println!("removed {s}/{l} ({n} bytes)");
    }
    println!("removed {} build(s), {b} bytes ({})", done.len(), mib(b));
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Stdio};

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "dg1-gc-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    fn build(root: &Path, svc: &str, label: &str, built_at: u64) -> PathBuf {
        let d = root.join("data/shared/builds").join(svc).join(label);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("bin"), vec![7u8; 9000]).unwrap();
        let rec = serde_json::json!({
            "service": svc, "label": label, "bin": "bin", "package": svc, "profile": "release",
            "workspace": "/x", "built_at": built_at, "commit": label, "dirty": false,
            "content": "", "source_commit": label
        });
        std::fs::write(d.join("build.json"), rec.to_string()).unwrap();
        d
    }

    fn env_toml(root: &Path, env: &str, text: &str) {
        let d = root.join("data").join(env);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("env.toml"), text).unwrap();
    }

    /// Far past every fixture's `built_at`, so only the tests that mean to
    /// exercise the 1 h grace are inside it.
    const NOW: u64 = 1_000_000;

    /// Removes a test root on drop, pass or fail (inspect m4).
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Kills and reaps a holder child on drop, so a failing test does not
    /// leak it (inspect m4).
    struct Reap(Child);
    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn none_open(_: &Path) -> Result<Vec<(u32, String)>> {
        Ok(Vec::new())
    }

    fn removed(p: &Plan) -> Vec<String> {
        p.removals().map(|(s, e)| format!("{s}/{}", e.label)).collect()
    }

    /// N = 5 builds of one service in two environments; prod pins `d-release`
    /// and its history is [d, c, b]; stage pins `a-release`; `e-release` was
    /// never good and is OLDER than every pin (built_at 50).
    fn fixture(tag: &str) -> PathBuf {
        let r = tmp(tag);
        for (i, l) in ["a-release", "b-release", "c-release", "d-release"].iter().enumerate() {
            build(&r, "svc", l, 100 + i as u64);
        }
        build(&r, "svc", "e-release", 50);
        env_toml(
            &r,
            "prod",
            "[services.svc]\nversion = \"d-release\"\ngood = [\"d-release\", \"c-release\", \"b-release\"]\n",
        );
        env_toml(&r, "stage", "[services.svc]\nversion = \"a-release\"\ngood = [\"a-release\"]\n");
        r
    }

    #[test]
    fn dg1_a_every_envs_pin_is_kept_and_unkept_builds_are_removed() {
        let r = fixture("pin");
        let _c = Cleanup(r.clone());
        let p = plan(&r, 0, NOW, &none_open).unwrap();
        // Opposition: something IS removed (b: good #3 back, e: never good),
        // so a plan that keeps everything fails here.
        assert_eq!(removed(&p), vec!["svc/b-release", "svc/e-release"]);
        let done = execute(&p, &none_open).unwrap();
        assert_eq!(done.len(), 2);
        let svc = r.join("data/shared/builds/svc");
        assert!(svc.join("d-release").is_dir(), "prod's pin was removed");
        assert!(svc.join("a-release").is_dir(), "stage's pin was removed");
        assert!(!svc.join("b-release").exists() && !svc.join("e-release").exists());
    }

    #[test]
    fn dg1_b_rollback_target_is_kept_and_keep_n_adds_the_newest_good() {
        let r = fixture("rb");
        let _c = Cleanup(r.clone());
        let p = plan(&r, 0, NOW, &none_open).unwrap();
        let c = p.services["svc"].iter().find(|e| e.label == "c-release").unwrap();
        assert!(
            c.keep_why.iter().any(|w| w == "rollback target in prod"),
            "c-release is prod's rollback target and must be kept: {:?}",
            c.keep_why
        );
        // --keep 1 keeps the next good build (b) too; e (never good) still goes.
        let p1 = plan(&r, 1, NOW, &none_open).unwrap();
        assert_eq!(removed(&p1), vec!["svc/e-release"]);
    }

    #[test]
    fn dg1_c_a_build_held_open_by_a_live_process_is_kept_and_removed_once_it_exits() {
        let r = tmp("open");
        let _c = Cleanup(r.clone());
        let held = build(&r, "svc", "held-release", 100);
        build(&r, "svc", "pinned-release", 200);
        env_toml(&r, "prod", "[services.svc]\nversion = \"pinned-release\"\ngood = [\"pinned-release\"]\n");
        // A live child EXECUTING an image inside the build dir (its text).
        // The image is a copy of THIS test binary (a copied /bin/sleep is a
        // platform binary at a foreign path and is SIGKILLed at exec), run
        // as `dg1_sleeper` below, which sleeps when DG1_SLEEP is set.
        let img = held.join("sleeper");
        std::fs::copy(std::env::current_exe().unwrap(), &img).unwrap();
        let mut child = Reap(Command::new(&img)
            .args(["gc::tests::dg1_sleeper", "--exact", "--ignored", "--test-threads=1"])
            .env("DG1_SLEEP", "60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap());
        // lsof needs the exec to have happened.
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while holders(&held).unwrap().is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(child.0.try_wait().unwrap().is_none(), "the holder child died before the check");
        let p = plan(&r, 0, NOW, &holders).unwrap();
        let e = p.services["svc"].iter().find(|e| e.label == "held-release").unwrap();
        assert!(
            e.keep_why.iter().any(|w| w.contains(&format!("OPEN by running pid {}", child.0.id()))),
            "a build a live process executes from must be kept: {:?}",
            e.keep_why
        );
        assert!(removed(&p).is_empty());
        execute(&p, &holders).unwrap();
        assert!(held.is_dir(), "held build was removed while running");
        // Opposition: the same build with the process gone IS removed.
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        let p2 = plan(&r, 0, NOW, &holders).unwrap();
        assert_eq!(removed(&p2), vec!["svc/held-release"]);
        execute(&p2, &holders).unwrap();
        assert!(!held.exists(), "held build survived after its process exited");
    }

    /// Not a test of gc: the body the held-open test EXECUTES from inside a
    /// build dir. `#[ignore]`d so it is never counted as a test (inspect m1);
    /// the held-open test runs it with `--ignored`.
    #[test]
    #[ignore = "helper body for dg1_c, not a test"]
    fn dg1_sleeper() {
        if let Ok(s) = std::env::var("DG1_SLEEP") {
            std::thread::sleep(Duration::from_secs(s.parse().unwrap_or(60)));
        }
    }

    /// Inspect m2: this asserts only what the dry run PRINTS. That it removes
    /// nothing is guarded by the real binary in
    /// `tests/environments.rs::dg1_gc_builds_dry_run_removes_nothing_…`.
    #[test]
    fn dg1_d_dry_run_render_names_each_removal_and_the_bytes() {
        let r = fixture("dry");
        let _c = Cleanup(r.clone());
        let o = Opts { keep: 0, dry_run: true };
        let p = plan(&r, o.keep, NOW, &none_open).unwrap();
        let text = render(&p, &r, &o);
        let b: u64 = p.removals().map(|(_, e)| e.bytes).sum();
        assert!(b > 0);
        assert!(text.contains("WOULD REMOVE") && text.contains(&format!("would remove 2 build(s), {b} bytes")), "{text}");
    }

    /// DG-1-1, rule 1: an unpinned build NEWER than the newest pin is kept
    /// (a deploy built it and has not pinned it yet). Opposition: an old
    /// unpinned build in the same plan IS removed. Both are far outside the
    /// 1 h grace, so only the newer-than-pin rule can keep `fresh`.
    #[test]
    fn dg1_f_unpinned_build_newer_than_the_newest_pin_is_kept_old_one_removed() {
        let r = tmp("newer");
        let _c = Cleanup(r.clone());
        build(&r, "svc", "old-release", 100);
        build(&r, "svc", "pin-release", 200);
        build(&r, "svc", "fresh-release", 300);
        env_toml(&r, "prod", "[services.svc]\nversion = \"pin-release\"\ngood = [\"pin-release\"]\n");
        let p = plan(&r, 0, NOW, &none_open).unwrap();
        let f = p.services["svc"].iter().find(|e| e.label == "fresh-release").unwrap();
        assert!(
            f.keep_why.iter().any(|w| w.starts_with("newer than the newest pin")),
            "an unpinned build newer than the pin must be kept: {:?}",
            f.keep_why
        );
        assert_eq!(removed(&p), vec!["svc/old-release"]);
    }

    /// DG-1-1, rule 2: a build younger than 1 h is kept even when it is OLDER
    /// than the pin (so the newer-than-pin rule cannot be what keeps it).
    /// Opposition: the same shape built 2 h ago is removed.
    #[test]
    fn dg1_g_build_younger_than_one_hour_is_kept_one_older_is_removed() {
        let r = tmp("young");
        let _c = Cleanup(r.clone());
        let now = 10_000_000;
        build(&r, "svc", "young-release", now - 120);
        build(&r, "svc", "aged-release", now - 7200);
        build(&r, "svc", "pin-release", now - 60);
        env_toml(&r, "prod", "[services.svc]\nversion = \"pin-release\"\ngood = [\"pin-release\"]\n");
        let p = plan(&r, 0, now, &none_open).unwrap();
        let y = p.services["svc"].iter().find(|e| e.label == "young-release").unwrap();
        assert!(
            y.keep_why.iter().any(|w| w == "built under 3600s ago"),
            "a build younger than 1 h must be kept: {:?}",
            y.keep_why
        );
        assert_eq!(removed(&p), vec!["svc/aged-release"]);
    }

    /// Inspect m3: lsof rc 1 WITH stderr is an error (the build is kept);
    /// rc 1 with empty stderr is "nothing open"; rc 0 output is parsed.
    #[test]
    fn dg1_h_lsof_rc1_with_stderr_is_an_error_not_nothing_open() {
        let d = Path::new("/x");
        assert!(lsof_verdict(d, Some(1), b"", b"lsof 4.91\n usage: ...").is_err());
        assert!(lsof_verdict(d, Some(1), b"", b"").unwrap().is_empty());
        assert!(lsof_verdict(d, None, b"", b"").is_err());
        assert_eq!(lsof_verdict(d, Some(0), b"p42\nftxt\nn/x/bin\n", b"").unwrap(), vec![(42, "/x/bin".to_string())]);
        // The real lsof on a dir nobody holds: Ok(empty).
        let r = tmp("lsof");
        let _c = Cleanup(r.clone());
        assert!(holders(&r).unwrap().is_empty());
    }

    #[test]
    fn dg1_e_gc_without_builds_refuses_and_keep_parses() {
        let o = parse_opts(&["--builds".into(), "--dry-run".into(), "--keep".into(), "1".into()]).unwrap();
        assert_eq!(o, Opts { keep: 1, dry_run: true });
        assert_eq!(parse_opts(&["--builds".into(), "--keep=3".into()]).unwrap(), Opts { keep: 3, dry_run: false });
        assert!(parse_opts(&["--dry-run".into()]).is_err(), "gc without --builds must refuse");
        assert!(parse_opts(&["--builds".into(), "--keep".into()]).is_err());
    }
}
