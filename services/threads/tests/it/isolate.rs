//! DG-5: ONE test binary, and still ONE PROCESS PER TEST FILE.
//!
//! Until DG-5 every `tests/*.rs` here was its own binary, so each file ran in
//! a process of its own. 124 of the 173 files set process environment
//! (`THREADS_RESUME_TICK_MS`, `THREADS_CLAUDE_HOME`, `FAKE_STATE_DIR`, ... with
//! values that differ file to file), and the crate has process-global test
//! seams (`clock::testing::ticking`, the keychain tripwire's abort). Run in one
//! process, those files would read each other's settings. So this module keeps
//! the old isolation inside the one binary:
//!
//! Before `main`, a constructor runs. Unless it is already in a child (the
//! [`CHILD_ENV`] variable is set), it asks this same binary for `--list` under
//! the caller's own arguments, groups the selected tests by their first path
//! segment (the module, i.e. the old file), and runs each group in a child
//! process of this binary, one after the other, with `--exact <names>`. Each
//! child prints libtest's own `running N tests` / `test result:` lines, exactly
//! as each old binary did. The parent exits 101 if any child failed.
//!
//! A child is judged by more than its exit code (DG-5 inspect, f856951). It is
//! red if it exits non-zero; if it exits 0 without a `test result:` line, or
//! with one whose passed + failed + ignored is below the number of tests it
//! was handed (a silent skip: an `exit(0)` mid-module); or if it runs past
//! [`TIMEOUT_ENV`] seconds (default 600). Every red names its module on the
//! `it-isolate:` line.
//!
//! DG-5b: each child is spawned as the leader of its own process group
//! (`process_group(0)`), never the driver's. On timeout the driver signals
//! that whole GROUP — SIGTERM, a short grace, then SIGKILL — not just the
//! child's own pid, then reaps the child (r43 DG-5 inspect-2's "Timeout and
//! orphans" finding: a child-only kill leaves anything the hung test spawned
//! re-parented to launchd, running with its Drop guards never having fired).
//!
//! `--list`, `--help` and anything run inside a child go straight to libtest,
//! so `cargo test --test it -- --list` and a test that re-runs this binary
//! (`durable_barrier`, `keychain_tripwire`) behave as before.

use std::ffi::{CStr, OsString};
use std::os::raw::c_char;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::CommandExt;
use std::process::Command;

/// Set in every child; its value is the module that child runs.
pub const CHILD_ENV: &str = "IT_ISOLATE_MODULE";

/// Guard-only hook (not used by the real driver or any shipped module): a
/// process that sees this env var spawns `/bin/sleep 901`, writes its pid to
/// the named file, and hangs — a stand-in for "a module that spawns a
/// grandchild then hangs", so `group_kill_reaches_grandchild_outlasting_sleep_901s`
/// can drive the real `run_module`/`kill_group` path without actually
/// registering a test that would hang every normal run.
const HANG_TEST_PIDFILE_ENV: &str = "IT_ISOLATE_TEST_HANG_PIDFILE";

/// libtest options that take a separate value.
const VALUE_OPTS: &[&str] = &[
    "--logfile",
    "--test-threads",
    "--skip",
    "--color",
    "--format",
    "-Z",
    "--shuffle-seed",
];

/// Arguments that are not a run: libtest answers them itself.
const NOT_A_RUN: &[&str] = &["--list", "--help", "-h", "--bench"];

#[used]
#[cfg_attr(target_os = "macos", link_section = "__DATA,__mod_init_func")]
#[cfg_attr(target_os = "linux", link_section = ".init_array")]
static DRIVER: extern "C" fn(i32, *const *const c_char) = driver_entry;

extern "C" fn driver_entry(argc: i32, argv: *const *const c_char) {
    if let Ok(pidfile) = std::env::var(HANG_TEST_PIDFILE_ENV) {
        let gc = Command::new("/bin/sleep").arg("901").spawn().expect("spawn /bin/sleep 901");
        std::fs::write(&pidfile, gc.id().to_string()).expect("write grandchild pid");
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }
    if std::env::var_os(CHILD_ENV).is_some() || argv.is_null() {
        return;
    }
    let mut args: Vec<OsString> = Vec::new();
    for i in 1..argc.max(0) as isize {
        // SAFETY: the loader hands an initializer the process's own argv.
        let p = unsafe { *argv.offset(i) };
        if p.is_null() {
            break;
        }
        let bytes = unsafe { CStr::from_ptr(p) }.to_bytes().to_vec();
        args.push(OsString::from_vec(bytes));
    }
    let strs: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    if let Some(code) = drive(&strs) {
        std::process::exit(code);
    }
}

/// The parsed command line: (flags kept for every child, positional filters).
/// `--skip` and `--exact` are dropped from the kept flags: the `--list` call
/// has already applied them, and each child gets `--exact` of its own.
pub fn split_args(args: &[String]) -> (Vec<String>, Vec<String>) {
    let mut kept = Vec::new();
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if VALUE_OPTS.contains(&a.as_str()) {
            let v = args.get(i + 1).cloned();
            if a != "--skip" {
                kept.push(a.clone());
                kept.extend(v);
            }
            i += 2;
            continue;
        }
        if a.starts_with("--skip=") || a == "--exact" {
            // dropped
        } else if a.starts_with('-') {
            kept.push(a.clone());
        } else {
            positional.push(a.clone());
        }
        i += 1;
    }
    (kept, positional)
}

/// The module a test name belongs to: its first path segment.
fn module_of(name: &str) -> &str {
    name.split("::").next().unwrap_or(name)
}

/// `None` = let libtest run in this process; `Some(rc)` = the driver ran it.
fn drive(args: &[String]) -> Option<i32> {
    if args.iter().any(|a| NOT_A_RUN.contains(&a.as_str())) {
        return None;
    }
    let me = std::env::current_exe().ok()?;
    let listed = Command::new(&me)
        .args(args)
        .arg("--list")
        .env(CHILD_ENV, "")
        .output();
    let listed = match listed {
        Ok(o) => o,
        Err(e) => {
            eprintln!("it-isolate: could not list the tests: {e}");
            return Some(101);
        }
    };
    if !listed.status.success() {
        eprint!("{}", String::from_utf8_lossy(&listed.stderr));
        return Some(listed.status.code().unwrap_or(101));
    }
    let mut groups: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for line in String::from_utf8_lossy(&listed.stdout).lines() {
        if let Some(name) = line.strip_suffix(": test") {
            groups.entry(module_of(name).to_string()).or_default().push(name.to_string());
        }
    }
    if groups.is_empty() {
        // Nothing selected: libtest prints its own "running 0 tests".
        return None;
    }
    let (kept, _) = split_args(args);
    let limit = timeout();
    let mut failed = Vec::new();
    for (module, names) in &groups {
        if let Err(why) = run_module(&me, &kept, module, names, limit) {
            failed.push(format!("{module} ({why})"));
        }
    }
    println!(
        "it-isolate: {} modules, each in its own process; {} failed{}",
        groups.len(),
        failed.len(),
        if failed.is_empty() { String::new() } else { format!(": {}", failed.join(", ")) }
    );
    Some(if failed.is_empty() { 0 } else { 101 })
}

/// Per-module wall-clock bound, seconds. The slowest module measured on
/// fac6bb8a was `self_update` at 67.3 s (`bind_first_recovery` 60.2 s), so the
/// default is 600 s: about 9x that, room for a loaded machine, and still a
/// bound. Override with `IT_ISOLATE_TIMEOUT_S=<seconds>`.
pub const TIMEOUT_ENV: &str = "IT_ISOLATE_TIMEOUT_S";
const DEFAULT_TIMEOUT_S: u64 = 600;

/// Grace between SIGTERM and SIGKILL when a group is killed on timeout. Short
/// on purpose: this fires only after a module has already run past its own
/// (minutes-scale) bound, so there is no reason to wait long for it to exit
/// cleanly, and `/bin/sleep` and friends die on SIGTERM with no handler at
/// all.
const KILL_GRACE: std::time::Duration = std::time::Duration::from_millis(300);

/// SIGTERM the whole process group `pid` leads, wait [`KILL_GRACE`], then
/// SIGKILL it. `pid` must be a child spawned with `.process_group(0)`, so its
/// group contains only it and whatever it spawned — never the driver, which
/// runs in a group of its own.
fn kill_group(pid: u32, grace: std::time::Duration) {
    let pgid = pid as i32;
    unsafe {
        libc::kill(-pgid, libc::SIGTERM);
    }
    std::thread::sleep(grace);
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
}

fn timeout() -> std::time::Duration {
    let s = std::env::var(TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(DEFAULT_TIMEOUT_S);
    std::time::Duration::from_secs(s)
}

/// libtest's own summary: passed + failed + ignored.
fn ran_count(line: &str) -> Option<usize> {
    let rest = line.strip_prefix("test result: ")?;
    let mut total = 0;
    for key in ["passed;", "failed;", "ignored;"] {
        let i = rest.find(&format!(" {key}"))?;
        let n = rest[..i].rsplit(|c: char| !c.is_ascii_digit()).next()?.parse::<usize>().ok()?;
        total += n;
    }
    Some(total)
}

/// Run one module in its own child. `Err` names why it is red: a non-zero
/// exit, a timeout (the child, and only the child, is killed by its pid), or
/// a child that exited 0 without running every test it was handed (no
/// summary line, or a summary that counts fewer than `names.len()`).
fn run_module(
    me: &std::path::Path,
    kept: &[String],
    module: &str,
    names: &[String],
    limit: std::time::Duration,
) -> Result<(), String> {
    use std::io::{BufRead, Write};
    use std::sync::{Arc, Mutex};
    let mut child = Command::new(me)
        .args(kept)
        .arg("--exact")
        .args(names)
        .env(CHILD_ENV, module)
        // DG-5b: this child leads its own process group (pgid = its own
        // pid), never the driver's, so a group-wide signal on timeout can
        // never reach the driver itself.
        .process_group(0)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn: {e}"))?;
    let pid = child.id();
    // (last `test result:` count seen, reader reached EOF)
    let seen: Arc<Mutex<(Option<Option<usize>>, bool)>> = Arc::new(Mutex::new((None, false)));
    let out = child.stdout.take().expect("piped stdout");
    let seen_r = Arc::clone(&seen);
    std::thread::spawn(move || {
        let mut r = std::io::BufReader::new(out);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match r.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let mut so = std::io::stdout().lock();
                    let _ = so.write_all(&buf);
                    let _ = so.flush();
                    let line = String::from_utf8_lossy(&buf);
                    if line.starts_with("test result: ") {
                        seen_r.lock().unwrap().0 = Some(ran_count(line.trim_end()));
                    }
                }
            }
        }
        seen_r.lock().unwrap().1 = true;
    });
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() >= limit => {
                kill_group(pid, KILL_GRACE);
                let _ = child.wait();
                return Err(format!(
                    "timed out after {}s; killed process group {pid} (SIGTERM, {}ms grace, \
                     then SIGKILL — the whole group, not just pid {pid}); set {TIMEOUT_ENV} to change",
                    limit.as_secs(),
                    KILL_GRACE.as_millis()
                ));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(e) => return Err(format!("wait: {e}")),
        }
    };
    // Drain what the child wrote before it exited. A grandchild that outlived
    // it may hold the pipe open, so stop at EOF, or once a summary has been
    // seen and nothing more arrives, or after 5 s.
    let drain = std::time::Instant::now();
    loop {
        let (got, eof) = *seen.lock().unwrap();
        if eof || drain.elapsed() > std::time::Duration::from_secs(5) {
            break;
        }
        if got.is_some() && drain.elapsed() > std::time::Duration::from_millis(300) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if !status.success() {
        return Err(status.to_string());
    }
    let got = seen.lock().unwrap().0;
    match got {
        None => Err(format!("exited 0 with no `test result:` line; ran ? of {}", names.len())),
        Some(None) => Err("unparseable `test result:` line".to_string()),
        Some(Some(n)) if n < names.len() => Err(format!("ran {n} of {}", names.len())),
        Some(Some(_)) => Ok(()),
    }
}

/// THE GUARD: this test runs in a child the driver started for THIS module
/// alone. A driver that did not run, or that ran several modules in one
/// process, fails it.
#[test]
fn each_module_runs_in_its_own_process() {
    assert_eq!(
        std::env::var(CHILD_ENV).as_deref(),
        Ok("isolate"),
        "not in a per-module child: the isolating driver did not run"
    );
    let args: Vec<String> = std::env::args().skip(1).collect();
    assert!(args.iter().any(|a| a == "--exact"), "the child was not given --exact: {args:?}");
    let (_, names) = split_args(&args);
    assert!(
        !names.is_empty() && names.iter().all(|n| module_of(n) == "isolate"),
        "this child was handed tests of other modules: {names:?}"
    );
}

/// `true` iff a process with this pid exists and answers to a signal from
/// us (`kill(pid, 0)`; ESRCH/EPERM both read as gone-or-unreachable here,
/// which is what "orphan cleaned up" needs).
fn pid_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// THE GUARD for DG-5b: a module's grandchild must die with the module.
///
/// Stands in for "a module that spawns a grandchild then hangs" without
/// actually registering a test that would hang every normal run: this
/// process spawns a child exactly as [`run_module`] does — same binary, same
/// `.process_group(0)`, same [`kill_group`]/[`KILL_GRACE`] on timeout — but
/// handed [`HANG_TEST_PIDFILE_ENV`] instead of real test args, so that child
/// hits the hang hook at the top of `driver_entry` (spawns `/bin/sleep 901`,
/// writes its pid, hangs) before it ever looks at `--exact`/[`CHILD_ENV`].
/// A 2 s wait stands in for the module's own timeout firing — 450x shorter
/// than the 901 s the grandchild would otherwise still be sleeping through.
///
/// Opposition is `each_module_runs_in_its_own_process`, run beside this test
/// in the same `isolate` child: a module that does not hang still passes,
/// unaffected by this change.
#[test]
fn group_kill_reaches_grandchild_outlasting_sleep_901s() {
    let me = std::env::current_exe().expect("current_exe");
    let pidfile =
        std::env::temp_dir().join(format!("it-isolate-pgroup-guard-{}.pid", std::process::id()));
    let _ = std::fs::remove_file(&pidfile);

    let mut hung = Command::new(&me)
        .env(HANG_TEST_PIDFILE_ENV, &pidfile)
        .process_group(0) // same leader-of-its-own-group spawn run_module uses
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("spawn the hung-module stand-in");
    let hung_pid = hung.id();

    // Stand-in for the module's own wall-clock bound firing.
    std::thread::sleep(std::time::Duration::from_secs(2));
    assert!(
        matches!(hung.try_wait(), Ok(None)),
        "the stand-in should still be hanging at 2s — it never calls exit"
    );
    kill_group(hung_pid, KILL_GRACE); // the exact action run_module takes on timeout
    let _ = hung.wait();

    let grandchild_pid: i32 = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Ok(s) = std::fs::read_to_string(&pidfile) {
                if let Ok(p) = s.trim().parse() {
                    break p;
                }
            }
            assert!(std::time::Instant::now() < deadline, "grandchild pid never appeared");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    };
    let _ = std::fs::remove_file(&pidfile);

    let mut dead = false;
    for _ in 0..150 {
        if !pid_alive(grandchild_pid) {
            dead = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // Belt-and-suspenders: never leave a sleeper behind even if the
    // assertion below is about to fail the test.
    if !dead {
        unsafe {
            libc::kill(grandchild_pid, libc::SIGKILL);
        }
    }
    assert!(
        dead,
        "grandchild pid {grandchild_pid} (/bin/sleep 901, spawned by the hung module) survived \
         the group kill — the exact orphan r43's DG-5 inspect-2 found when only the child's own \
         pid was killed"
    );
}
