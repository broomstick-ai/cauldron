//! **R6.S6 (H179-1; the Deploy-3 finding, ledger E10-5) — a non-prod
//! environment's holder runs from its OWN launch path, so a deploy of the
//! default (prod-like) environment's file does not stop it.**
//!
//! Deploy 3 measured it: the stage holder, started as
//! `~/.cauldron/bin/cauldron --env stage daemon`, ran prod's FILE; the holder's
//! self-update watches the file it was launched from, so the prod holder swap
//! made stage's holder exit too, and nothing restarted it.
//!
//! Everything here runs in a throwaway root (`HOME` and `CAULDRON_HOME` under
//! the temp dir). The default environment's launch path is
//! `<root>/bin/cauldron`, exactly prod's shape; both holders are started FROM
//! that one file, exactly as stage was. Liveness is a child we spawned
//! (`try_wait`, so a reused pid cannot answer) and identity is the text inode
//! `lsof` reports for the pid — never `pgrep -f`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_cauldron");

#[path = "support/first_exec.rs"]
mod first_exec;

/// The default `poll_secs` (`libs/env/src/decl.rs`, `default_poll_secs`): the
/// period of the holder's self-update check, which every negative wait below
/// must outlast (doctrine 4).
const POLL_SECS: u64 = 4;

struct Home(PathBuf, Vec<Child>);
impl Drop for Home {
    fn drop(&mut self) {
        for c in &mut self.1 {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fresh(tag: &str) -> Home {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let h = std::env::temp_dir().join(format!(
        "cauldron-ownpath-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&h);
    std::fs::create_dir_all(&h).unwrap();
    Home(h, Vec::new())
}

fn root(h: &Path) -> PathBuf {
    h.join(".cauldron")
}

fn cmd(h: &Path, exe: &Path) -> Command {
    let mut c = Command::new(exe);
    c.env("HOME", h)
        .env("CAULDRON_HOME", root(h))
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .env_remove("CAULDRON_RELAUNCHED_FROM");
    c
}

fn run(h: &Path, exe: &Path, args: &[&str]) -> (i32, String, String) {
    let out = cmd(h, exe).args(args).output().expect("spawn cauldron");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// A NEW file at `dst` holding `src`'s bytes: written, then renamed over, so
/// it has a new inode and a new mtime — what a deploy does to a launch path.
fn deploy_onto(src: &Path, dst: &Path) {
    let bytes = std::fs::read(src).unwrap();
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    let tmp = dst.with_file_name(format!(".deploying.{}", std::process::id()));
    std::fs::write(&tmp, bytes).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::rename(&tmp, dst).unwrap();
}

fn ino(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p).unwrap_or_else(|e| panic!("stat {}: {e}", p.display())).ino()
}

/// Every inode the process `pid` has open as text (`lsof -d txt`).
fn txt_inodes(pid: u32) -> Vec<u64> {
    let out = Command::new("lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "txt", "-Fi"])
        .stderr(Stdio::null())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix('i')?.parse().ok())
        .collect()
}

fn spawn_holder(home: &mut Home, exe: &Path, args: &[&str], log: &str) -> usize {
    let f = std::fs::File::create(home.0.join(log)).unwrap();
    let c = cmd(&home.0, exe)
        .args(args)
        .stdout(Stdio::from(f.try_clone().unwrap()))
        .stderr(Stdio::from(f))
        .spawn()
        .unwrap();
    home.1.push(c);
    home.1.len() - 1
}

fn log(home: &Home, name: &str) -> String {
    std::fs::read_to_string(home.0.join(name)).unwrap_or_default()
}

fn wait_for(what: &str, within: Duration, mut f: impl FnMut() -> bool) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < within {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    eprintln!("timed out after {within:?} waiting for: {what}");
    false
}

fn running(home: &mut Home, i: usize) -> bool {
    matches!(home.1[i].try_wait(), Ok(None))
}

/// THE FINDING, AS A TEST. Two holders, both started from the default
/// environment's own file (`<root>/bin/cauldron`): the default env's and a
/// named env's. Replacing the default env's file makes the DEFAULT holder
/// self-update (exit) and leaves the NAMED holder running — same pid, same
/// text inode — past three self-update periods. Replacing the named env's own
/// file (`promote`) makes the NAMED holder self-update. Opposed both ways: a
/// holder that never self-updates fails at the first and last steps, and one
/// that self-updates on any change fails at the survival step.
#[test]
fn a_named_envs_holder_survives_a_default_env_deploy_past_3_polls_and_self_updates_on_its_own_file() {
    let mut home = fresh("survive");
    let h = home.0.clone();
    let bin = root(&h).join("bin");
    let shared = bin.join("cauldron"); // the default env's launch path
    let own = bin.join("cauldron-named"); // the named env's own launch path
    deploy_onto(Path::new(BIN), &shared);

    let (rc, _, err) = run(&h, &shared, &["env", "create", "named"]);
    assert_eq!(rc, 0, "env create: {err}");

    let d = spawn_holder(&mut home, &shared, &["daemon"], "default.log");
    let n = spawn_holder(&mut home, &shared, &["--env", "named", "daemon"], "named.log");
    let hold_line = "cauldron: holding environment";
    assert!(
        wait_for("both holders hold", Duration::from_secs(30), || log(&home, "default.log").contains(hold_line)
            && log(&home, "named.log").contains(hold_line)),
        "default:\n{}\nnamed:\n{}",
        log(&home, "default.log"),
        log(&home, "named.log")
    );
    let dpid = home.1[d].id();
    let npid = home.1[n].id();

    // SENTINEL: the two launch paths are two different files, so "the named
    // holder runs the named file and not the shared one" can come out either way.
    assert!(own.exists(), "the named env's own launch path was never placed:\n{}", log(&home, "named.log"));
    let (shared_ino, own_ino) = (ino(&shared), ino(&own));
    assert_ne!(shared_ino, own_ino, "the two launch paths are one file; nothing below could fail");
    let dtxt = txt_inodes(dpid);
    let ntxt = txt_inodes(npid);
    assert!(dtxt.contains(&shared_ino), "default holder {dpid} does not run {}: {dtxt:?}", shared.display());
    assert!(
        ntxt.contains(&own_ino) && !ntxt.contains(&shared_ino),
        "named holder {npid} must run its own file {} (ino {own_ino}), not the shared {} (ino {shared_ino}); \
         txt inodes {ntxt:?}\n{}",
        own.display(),
        shared.display(),
        log(&home, "named.log")
    );

    // ── DEPLOY THE DEFAULT ENV ───────────────────────────────────────────
    deploy_onto(Path::new(BIN), &shared);
    assert_ne!(ino(&shared), shared_ino, "the deploy must put a NEW file at the default launch path");
    let default_exited = wait_for("the default holder self-updates", Duration::from_secs(6 * POLL_SECS), || {
        !running(&mut home, d)
    });
    assert!(default_exited, "the default holder did NOT self-update on its own file's change:\n{}", log(&home, "default.log"));
    assert!(
        log(&home, "default.log").contains("has changed; exiting"),
        "the default holder exited, but not by self-update:\n{}",
        log(&home, "default.log")
    );
    // Outlast three self-update periods on the named holder.
    std::thread::sleep(Duration::from_secs(3 * POLL_SECS + 1));
    assert!(
        running(&mut home, n),
        "the named holder {npid} DIED on the default env's deploy (the Deploy-3 finding):\n{}",
        log(&home, "named.log")
    );
    let ntxt2 = txt_inodes(npid);
    assert!(ntxt2.contains(&own_ino), "named holder {npid} is no longer running ino {own_ino}: {ntxt2:?}");
    assert!(!log(&home, "named.log").contains("has changed; exiting"), "{}", log(&home, "named.log"));

    // ── PROMOTE THE NAMED ENV: its own file changes ──────────────────────
    let (rc, out, err) = run(&h, &shared, &["--env", "named", "promote"]);
    assert_eq!(rc, 0, "promote: {out}{err}");
    let own_ino2 = ino(&own);
    assert_ne!(own_ino2, own_ino, "promote must put a NEW file at the named launch path");
    let named_exited = wait_for("the named holder self-updates", Duration::from_secs(6 * POLL_SECS), || {
        !running(&mut home, n)
    });
    assert!(named_exited, "the named holder did NOT self-update on its own file's change:\n{}", log(&home, "named.log"));
    assert!(log(&home, "named.log").contains("has changed; exiting"), "{}", log(&home, "named.log"));

    // ── AND COMES BACK ON THE NEW BYTES ──────────────────────────────────
    // R9.F1: this holder's path runs two files nobody has exec'd yet — the
    // default env's launch path as re-deployed above, and the named env's
    // own file as `promote` just placed it — and a first exec of a fresh
    // cauldron copy is not free here (`support/first_exec.rs`; base r32 run 3
    // went red at the old fixed 30 s). Both are paid now, with no holder on
    // either (the default holder and the named holder have both exited), and
    // the wait is one self-update period plus the measured worst first exec
    // of each, still budgeted.
    for (exe, what) in [(&shared, "the re-deployed default launch path"), (&own, "the promoted named launch path")] {
        first_exec::pay_first_exec(cmd(&h, exe).arg("envs"), what);
    }
    let n2 = spawn_holder(&mut home, &shared, &["--env", "named", "daemon"], "named2.log");
    let within = first_exec::first_wait(Duration::from_secs(POLL_SECS), 2);
    assert!(
        wait_for("the named holder holds again", within, || log(&home, "named2.log").contains(hold_line)),
        "{}",
        log(&home, "named2.log")
    );
    let t = txt_inodes(home.1[n2].id());
    assert!(t.contains(&own_ino2) && !t.contains(&shared_ino), "restarted named holder runs {t:?}, not {own_ino2}");
}

/// **Prod is unchanged, and the relaunch is where it says it is.** In the
/// default environment `daemon` runs from wherever it was started and places
/// nothing; `promote` refuses it. In a named environment `daemon` places and
/// runs the env's own file, `promote` places it, the copy is byte-identical
/// (so a code signature on the source is on it: same designated requirement,
/// `codesign -v --strict` passes), and a relaunch that does not land on its own
/// file refuses rather than looping.
#[test]
fn prod_daemon_places_nothing_and_promote_refuses_prod_while_a_named_env_gets_a_signed_identical_copy() {
    let home = fresh("prod");
    let h = home.0.clone();
    let bin = root(&h).join("bin");
    let bin_exe = Path::new(BIN);

    // PROD: unchanged — runs from BIN, places nothing.
    // a REAL prod (env.toml present), so only the prod refusal can refuse
    // `promote` below, not the missing-environment one
    let (rc, out, err) = run(&h, bin_exe, &["env", "create", "prod"]);
    assert_eq!(rc, 0, "env create prod: {out}{err}");
    let (rc, out, err) = run(&h, bin_exe, &["daemon", "--once"]);
    assert_eq!(rc, 0, "prod daemon --once: {out}{err}");
    assert!(!bin.join("cauldron").exists(), "a prod `daemon` placed {}", bin.join("cauldron").display());
    assert!(!out.contains("OWN launch path"), "prod relaunched: {out}");
    assert!(!out.contains("exec'd onto its own launch path"), "prod exec'd: {out}");
    let (rc, out, err) = run(&h, bin_exe, &["promote"]);
    assert_ne!(rc, 0, "promote in prod was accepted: {out}");
    assert!(err.contains("REFUSED: `promote` is for a non-prod environment"), "refused, but not as prod: {err}");
    assert!(!bin.join("cauldron").exists());

    // NAMED: daemon places and execs the env's own file.
    assert_eq!(run(&h, bin_exe, &["env", "create", "named"]).0, 0);
    let own = bin.join("cauldron-named");
    let (rc, out, err) = run(&h, bin_exe, &["--env", "named", "daemon", "--once"]);
    assert_eq!(rc, 0, "named daemon --once: {out}{err}");
    assert!(own.exists(), "named daemon did not place its own launch path: {out}{err}");
    assert!(out.contains("OWN launch path"), "{out}");
    // R6.S6b (inspector F-I3): "OWN launch path" is printed BEFORE the exec and
    // "--once" by whichever image ran the pass, so neither witnesses an exec.
    // This line is printed only by the image the exec landed on (it reads the
    // marker the exec'ing image set; the test removed it from our env), and it
    // names that image's own path and pid.
    let landed = format!("exec'd onto its own launch path {}", own.display());
    let at = out.find(&landed).unwrap_or_else(|| panic!("no exec happened: the named holder never ran from {}:\n{out}", own.display()));
    assert!(
        out[at..].contains("--once, so one pass and out"),
        "the exec'd holder did not run its pass AFTER landing: {out}"
    );
    assert!(std::fs::read(&own).unwrap() == std::fs::read(bin_exe).unwrap(), "placed copy is not byte-identical");

    // promote of a never-created env is refused
    let (rc, _, err) = run(&h, bin_exe, &["--env", "ghost", "promote"]);
    assert_ne!(rc, 0);
    assert!(err.contains("REFUSED"), "{err}");
    assert!(!bin.join("cauldron-ghost").exists());

    // promote replaces the file (new inode), byte-identical
    let before = ino(&own);
    let (rc, out, err) = run(&h, bin_exe, &["--env", "named", "promote"]);
    assert_eq!(rc, 0, "{out}{err}");
    assert_ne!(ino(&own), before);
    assert!(std::fs::read(&own).unwrap() == std::fs::read(bin_exe).unwrap(), "promoted copy is not byte-identical");

    // AD-HOC ONLY, said plainly (R6.S6b, inspector F-I4): the test binary
    // carries the linker's ad-hoc signature, whose designated requirement is
    // `cdhash H"…"` — a hash of the bytes. So the comparison below is byte
    // identity measured a second time; it cannot fail for any reason the byte
    // `==` above does not already catch. What it adds is that the copy still
    // VERIFIES (`codesign -v --strict`). The REAL-identity case, where the
    // requirement names an identifier and a certificate and a byte compare
    // would not stand in for it, is
    // `a_copy_promoted_from_a_binary_signed_with_a_real_identity_keeps_that_identity`.
    let dr = |p: &Path| {
        let o = Command::new("/usr/bin/codesign").args(["-d", "-r-"]).arg(p).output().unwrap();
        assert!(o.status.success(), "codesign -d -r- {}: {}", p.display(), String::from_utf8_lossy(&o.stderr));
        let s = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
        s.lines().filter(|l| l.contains("designated =>")).collect::<String>()
    };
    let (a, b) = (dr(bin_exe), dr(&own));
    assert!(!a.is_empty(), "no designated requirement read off {BIN}");
    assert!(a.contains("cdhash"), "the test binary is no longer ad-hoc signed ({a}); update the comment above");
    assert_eq!(a, b, "the copy's designated requirement differs from the source's");
    let v = Command::new("/usr/bin/codesign").args(["-v", "--strict"]).arg(&own).status().unwrap();
    assert!(v.success(), "codesign -v --strict failed on the placed copy");

    // a relaunch that did not land on its own file refuses instead of looping
    let out = cmd(&h, bin_exe)
        .env("CAULDRON_RELAUNCHED_FROM", "/somewhere/else")
        .args(["--env", "named", "daemon", "--once"])
        .output()
        .unwrap();
    assert_ne!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stderr).contains("which would loop"), "{:?}", out);
    // ...while from its own file, with the marker set, it runs
    let (rc, out, err) = {
        let o = cmd(&h, &own)
            .env("CAULDRON_RELAUNCHED_FROM", "/somewhere/else")
            .args(["--env", "named", "daemon", "--once"])
            .output()
            .unwrap();
        (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
    };
    assert_eq!(rc, 0, "{out}{err}");
    assert!(!out.contains("OWN launch path"), "re-exec'd from its own file: {out}");
}

// ── R6.S6b (H179-1; inspector findings F-I2, F-I4 on R6.S6) ───────────────
//
// The holder's self-update used to compare its launch path's mtime alone, and
// `promote` copies with the source's mtime (APFS clone, to the nanosecond). So
// a new image with the SAME mtime went unseen. These tests put exactly that at
// the launch path, twice: as a new file (what `promote` does) and as an
// in-place rewrite that puts the old mtime back; and they oppose it with a
// metadata-only change that must NOT restart the holder.

fn mtime_ns(p: &Path) -> (i64, i64) {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(p).unwrap();
    (m.mtime(), m.mtime_nsec())
}

fn ctime_ns(p: &Path) -> (i64, i64) {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(p).unwrap();
    (m.ctime(), m.ctime_nsec())
}

/// Give `p` exactly `from`'s mtime, touching nothing else.
fn copy_mtime(p: &Path, from: &Path) {
    let t = std::fs::metadata(from).unwrap().modified().unwrap();
    std::fs::OpenOptions::new().write(true).open(p).unwrap().set_modified(t).unwrap();
}

/// The designated requirement line `codesign -d -r-` prints for `p`.
fn designated(p: &Path) -> String {
    let o = Command::new("/usr/bin/codesign").args(["-d", "-r-"]).arg(p).output().unwrap();
    assert!(o.status.success(), "codesign -d -r- {}: {}", p.display(), String::from_utf8_lossy(&o.stderr));
    let s = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    s.lines().filter(|l| l.contains("designated =>")).collect::<String>()
}

/// Start a named env's holder from BIN (so it places and execs its own file)
/// and wait until it holds. Returns (child index, own launch path).
fn named_holder(home: &mut Home, log_name: &str) -> (usize, PathBuf) {
    let h = home.0.clone();
    let own = root(&h).join("bin").join("cauldron-named");
    let (rc, out, err) = run(&h, Path::new(BIN), &["env", "create", "named"]);
    assert_eq!(rc, 0, "env create: {out}{err}");
    let n = spawn_holder(home, Path::new(BIN), &["--env", "named", "daemon"], log_name);
    assert!(
        wait_for("the named holder holds", Duration::from_secs(30), || log(home, log_name).contains("cauldron: holding environment")),
        "{}",
        log(home, log_name)
    );
    assert!(own.exists(), "no own launch path placed:\n{}", log(home, log_name));
    let pid = home.1[n].id();
    assert!(txt_inodes(pid).contains(&ino(&own)), "holder {pid} does not run {}", own.display());
    (n, own)
}

/// **A `promote` whose source has the SAME mtime as the file it replaces is
/// still seen** — by the new inode. The source is BIN re-signed ad-hoc under
/// another identifier (so its bytes differ) with its mtime set to the launch
/// path's. Sentinels prove the mtime really is unchanged after the promote, so
/// an mtime-only holder cannot pass; the holder is first shown to survive a
/// poll untouched, so one that always exits cannot pass either.
#[test]
fn a_promote_from_a_source_with_the_same_mtime_restarts_the_holder_by_inode() {
    let mut home = fresh("samemtime");
    let h = home.0.clone();
    let (n, own) = named_holder(&mut home, "named.log");
    let (own_ino, own_mtime) = (ino(&own), mtime_ns(&own));

    // a DIFFERENT image with the SAME mtime
    let src = h.join("src").join("cauldron");
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::copy(BIN, &src).unwrap();
    let o = Command::new("/usr/bin/codesign")
        .args(["--force", "-s", "-", "--identifier", "cauldron-r6s6b-reimage"])
        .arg(&src)
        .output()
        .unwrap();
    assert!(o.status.success(), "ad-hoc re-sign: {}", String::from_utf8_lossy(&o.stderr));
    copy_mtime(&src, &own);
    assert!(std::fs::read(&src).unwrap() != std::fs::read(&own).unwrap(), "the new image has the old bytes; nothing below could fail");
    assert_eq!(mtime_ns(&src), own_mtime, "the source's mtime differs; this is not the same-mtime case");

    // untouched, the holder stays (opposes "always exits")
    std::thread::sleep(Duration::from_secs(POLL_SECS + 1));
    assert!(running(&mut home, n), "the holder exited with nothing changed:\n{}", log(&home, "named.log"));

    let (rc, out, err) = run(&h, &src, &["--env", "named", "promote"]);
    assert_eq!(rc, 0, "promote: {out}{err}");
    // SENTINELS: the mtime an mtime-only check reads did NOT move; the file did.
    assert_eq!(mtime_ns(&own), own_mtime, "promote changed the launch path's mtime; an mtime-only check would see this");
    assert_ne!(ino(&own), own_ino, "promote did not put a new file at the launch path");
    assert!(std::fs::read(&own).unwrap() == std::fs::read(&src).unwrap(), "promote did not place the new bytes");

    let exited = wait_for("the holder self-updates", Duration::from_secs(6 * POLL_SECS), || !running(&mut home, n));
    let l = log(&home, "named.log");
    assert!(exited, "the holder did NOT see a same-mtime promote (F-I2):\n{l}");
    assert!(l.contains("has changed; exiting"), "exited, but not by self-update:\n{l}");
    assert!(l.contains("(a different file (inode) changed)"), "self-updated for another reason:\n{l}");
}

/// **An in-place rewrite that puts the old mtime back is seen by its bytes;
/// a metadata-only change is not a new image, past three polls.** Same inode,
/// same size, same mtime, one byte different: only the content can tell. The
/// opposing half comes first, on the same holder: a `chmod` moves the ctime
/// with identical bytes, and the holder must still be running three
/// self-update periods later — so "restart on any stat change" fails here and
/// "inode + mtime only" fails at the rewrite.
#[test]
fn an_in_place_rewrite_restoring_the_mtime_restarts_the_holder_but_a_chmod_past_3_polls_does_not() {
    let mut home = fresh("inplace");
    let (n, own) = named_holder(&mut home, "named.log");
    let bytes = std::fs::read(&own).unwrap();
    let (ino0, mtime0, ctime0) = (ino(&own), mtime_ns(&own), ctime_ns(&own));

    // ── METADATA ONLY ────────────────────────────────────────────────────
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&own, std::fs::Permissions::from_mode(0o775)).unwrap();
    }
    assert_ne!(ctime_ns(&own), ctime0, "chmod did not move the ctime; the content path is not reached");
    assert_eq!((ino(&own), mtime_ns(&own)), (ino0, mtime0));
    assert!(std::fs::read(&own).unwrap() == bytes);
    std::thread::sleep(Duration::from_secs(3 * POLL_SECS + 1));
    assert!(running(&mut home, n), "a chmod restarted the holder:\n{}", log(&home, "named.log"));
    assert!(!log(&home, "named.log").contains("has changed; exiting"), "{}", log(&home, "named.log"));

    // ── IN-PLACE REWRITE, MTIME RESTORED ─────────────────────────────────
    // The LAST byte: the tail of the code-signature blob, which the kernel
    // read at exec and never pages in again, so the running holder is not
    // disturbed by it.
    let last = bytes.len() as u64 - 1;
    {
        use std::os::unix::fs::FileExt;
        let f = std::fs::OpenOptions::new().write(true).open(&own).unwrap();
        f.write_at(&[bytes[last as usize] ^ 0xff], last).unwrap();
        // and put the original mtime back
        let t = std::time::UNIX_EPOCH + Duration::new(mtime0.0 as u64, mtime0.1 as u32);
        f.set_modified(t).unwrap();
    }
    // SENTINELS: everything but the bytes and the ctime is as it was.
    assert_eq!((ino(&own), std::fs::metadata(&own).unwrap().len(), mtime_ns(&own)), (ino0, bytes.len() as u64, mtime0), "the rewrite moved inode/size/mtime; the content path is not reached");
    assert!(std::fs::read(&own).unwrap() != bytes, "the rewrite did not change the bytes");

    let exited = wait_for("the holder self-updates", Duration::from_secs(6 * POLL_SECS), || !running(&mut home, n));
    let l = log(&home, "named.log");
    assert!(exited, "the holder did NOT see an mtime-restoring in-place rewrite:\n{l}");
    assert!(l.contains("has changed; exiting"), "exited, but not by self-update:\n{l}");
    assert!(l.contains("(content (same inode"), "self-updated for another reason:\n{l}");
}

/// **A promoted copy keeps a REAL signing identity** (R6.S6b, inspector F-I4):
/// BIN signed with a keychain codesigning identity under an identifier, then
/// promoted. Its designated requirement names that identifier and a
/// certificate — not a cdhash — so comparing it is NOT a second byte compare.
/// Sentinel: the ad-hoc BIN's requirement differs, so the equality can fail.
///
/// Needs a codesigning identity in the login keychain (`security
/// find-identity -v -p codesigning`; `harness-codesign` is preferred). Without
/// one it FAILS rather than passing on ad-hoc coverage; set
/// `CAULDRON_TEST_ADHOC_ONLY=1` to accept that this machine can only test the
/// ad-hoc case.
#[test]
fn a_copy_promoted_from_a_binary_signed_with_a_real_identity_keeps_that_identity() {
    let o = Command::new("/usr/bin/security").args(["find-identity", "-v", "-p", "codesigning"]).output().unwrap();
    let listing = String::from_utf8_lossy(&o.stdout).to_string();
    let hash_of = |l: &str| l.split_whitespace().nth(1).filter(|h| h.len() == 40).map(str::to_string);
    let identity = listing
        .lines()
        .find(|l| l.contains("\"harness-codesign\""))
        .and_then(hash_of)
        .or_else(|| listing.lines().find_map(hash_of));
    let Some(identity) = identity else {
        if std::env::var_os("CAULDRON_TEST_ADHOC_ONLY").is_some() {
            eprintln!("CAULDRON_TEST_ADHOC_ONLY: no codesigning identity; the real-identity case is NOT covered here");
            return;
        }
        panic!("no codesigning identity in the keychain:\n{listing}\nset CAULDRON_TEST_ADHOC_ONLY=1 to accept ad-hoc-only coverage");
    };

    let home = fresh("identity");
    let h = home.0.clone();
    assert_eq!(run(&h, Path::new(BIN), &["env", "create", "named"]).0, 0);
    let own = root(&h).join("bin").join("cauldron-named");
    let signed = h.join("src").join("cauldron");
    std::fs::create_dir_all(signed.parent().unwrap()).unwrap();
    std::fs::copy(BIN, &signed).unwrap();
    let ident = "ai.broomstick.cauldron-r6s6b-test";
    // the real HOME (this process's): codesign cannot find the login keychain
    // under the throwaway one (R6.S6 inspector, check 6)
    let o = Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", &identity, "--identifier", ident])
        .arg(&signed)
        .output()
        .unwrap();
    assert!(o.status.success(), "codesign with identity {identity}: {}", String::from_utf8_lossy(&o.stderr));

    let src_dr = designated(&signed);
    assert!(src_dr.contains(&format!("identifier \"{ident}\"")), "{src_dr}");
    assert!(src_dr.contains("certificate"), "not an identity requirement: {src_dr}");
    assert!(!src_dr.contains("cdhash"), "still a cdhash requirement: {src_dr}");
    assert_ne!(designated(Path::new(BIN)), src_dr, "the ad-hoc and identity requirements read the same; the equality below could not fail");

    let (rc, out, err) = run(&h, &signed, &["--env", "named", "promote"]);
    assert_eq!(rc, 0, "promote from the signed binary: {out}{err}");
    assert_eq!(designated(&own), src_dr, "the promoted copy lost the signing identity");
    let v = Command::new("/usr/bin/codesign").args(["-v", "--strict"]).arg(&own).output().unwrap();
    assert!(v.status.success(), "codesign -v --strict on the promoted copy: {}", String::from_utf8_lossy(&v.stderr));
    // and the identity-signed copy runs as the env's holder
    let (rc, out, err) = run(&h, &own, &["--env", "named", "daemon", "--once"]);
    assert_eq!(rc, 0, "{out}{err}");
    assert!(out.contains("--once, so one pass and out"), "{out}");
}
