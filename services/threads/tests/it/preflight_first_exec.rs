//! R9.U1 — A REAL DEPLOY IS NEVER REFUSED BECAUSE A FRESH FILE'S FIRST EXEC IS
//! SLOW, AND A BROKEN ONE IS STILL REFUSED.
//!
//! On this machine the FIRST exec of a newly written executable pays a
//! system-wide assessment (XProtect) that was measured at 15–36 s (R9.F2,
//! R9.C5b); the second exec of the same file takes ~0.1 s. A deploy writes a NEW
//! file and the preflight self-checks it, so before R9.U1 the preflight's 20 s
//! `--self-check` bound was that file's first exec, and a healthy build could be
//! refused "does not pass preflight" for no defect at all.
//!
//! These tests are deliberately UNWARMED. They do not use any fixture prewarm
//! (R9.F2's `first_exec` helpers): warming the candidate in the test is exactly
//! what hides this defect, so here the candidate's first exec happens INSIDE the
//! preflight under test.
//!
//! The slow first exec is injected, not hoped for: the candidate is a real
//! Mach-O wrapper, compiled here with the system `cc`, that sleeps on its first
//! run only and then `execv`s the real `threads` binary. The paired candidates
//! hang on every exec, or exit non-zero, and must still be refused.
//!
//! The env var `THREADS_FIRST_EXEC_BUDGET_MS` is process-global, so every test
//! in this file takes the serial gate.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use threads::update::{self, Preflight, RestartReason, UpdateGate};

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|p| p.into_inner())
}

/// The old bound on `--self-check`, which the injected delay must outlast.
const OLD_SELF_CHECK_BOUND: Duration = Duration::from_secs(20);

/// The injected first-exec delay: the old 20 s bound plus 10 s. It is the size
/// of the measured real first exec on this machine (15–36 s), and every test name
/// below that relies on it says "by 10s".
const INJECTED_FIRST_EXEC_DELAY_SECS: u64 = 30;

fn real_daemon_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_threads"))
}

/// How the wrapper candidate behaves.
#[derive(Clone, Copy)]
enum Wrapper {
    /// Sleeps `INJECTED_FIRST_EXEC_DELAY_SECS` on its first run only, then
    /// execs the real daemon. Every later run execs it straight away.
    SlowFirstExec,
    /// Never answers, on any exec.
    HangsEveryExec,
    /// Exits 3 on every exec without saying anything.
    ExitsNonZero,
}

const WRAPPER_C: &str = r#"
#include <fcntl.h>
#include <unistd.h>

/* > update::MIN_BINARY_BYTES (1 MiB), so the candidate is plausible by size. */
static volatile const char PAD[1200000] = {1};

static void note(const char *line, int n) {
    int fd = open(RUNS_LOG, O_WRONLY | O_CREAT | O_APPEND, 0644);
    if (fd >= 0) { write(fd, line, n); close(fd); }
}

int main(int argc, char **argv) {
    (void)argc;
    if (PAD[0] != 1) return 9;
    note("run\n", 4);
#if MODE == 1
    if (access(MARKER, F_OK) != 0) {
        int m = open(MARKER, O_WRONLY | O_CREAT, 0644);
        if (m >= 0) close(m);
        note("slept\n", 6);
        sleep(DELAY_S);
    }
#elif MODE == 2
    for (;;) pause();
#elif MODE == 3
    return 3;
#endif
    argv[0] = (char *)REAL;
    execv(REAL, argv);
    return 127;
}
"#;

struct Candidate {
    path: PathBuf,
    runs_log: PathBuf,
}

impl Candidate {
    fn lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.runs_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
    fn runs(&self) -> usize {
        self.lines().iter().filter(|l| *l == "run").count()
    }
    fn slept(&self) -> usize {
        self.lines().iter().filter(|l| *l == "slept").count()
    }
}

/// Compile the wrapper into `dir/threads-candidate`. The compiled file is the
/// candidate; it is never executed by the test itself, so the preflight under
/// test performs its FIRST exec.
fn build_wrapper(dir: &Path, kind: Wrapper) -> Candidate {
    let src = dir.join("wrapper.c");
    std::fs::write(&src, WRAPPER_C).unwrap();
    let path = dir.join("threads-candidate");
    let runs_log = dir.join("wrapper-runs.log");
    let marker = dir.join("wrapper-first-run-done");
    let mode = match kind {
        Wrapper::SlowFirstExec => 1,
        Wrapper::HangsEveryExec => 2,
        Wrapper::ExitsNonZero => 3,
    };
    let out = std::process::Command::new("/usr/bin/cc")
        .arg("-O0")
        .arg(format!("-DMODE={mode}"))
        .arg(format!("-DDELAY_S={INJECTED_FIRST_EXEC_DELAY_SECS}"))
        .arg(format!("-DREAL=\"{}\"", real_daemon_binary().display()))
        .arg(format!("-DRUNS_LOG=\"{}\"", runs_log.display()))
        .arg(format!("-DMARKER=\"{}\"", marker.display()))
        .arg("-o")
        .arg(&path)
        .arg(&src)
        .output()
        .expect("the system cc runs");
    assert!(
        out.status.success(),
        "compiling the wrapper candidate failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Sentinel: the candidate passes the cheap checks, so whatever the
    // preflight decides is decided by EXECUTING it, not by size or magic.
    assert!(
        update::plausible(&path),
        "the wrapper must be a plausible candidate (Mach-O, executable, >= MIN_BINARY_BYTES); \
         it is {} bytes",
        std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
    );
    assert!(!runs_log.exists(), "nothing has executed the candidate yet");
    Candidate { path, runs_log }
}

fn gate_for(dir: &Path, c: &Candidate) -> std::sync::Arc<UpdateGate> {
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let gate = UpdateGate::for_binary(data, c.path.clone());
    gate.request_restart(RestartReason::Operator {
        note: "deploy the new build".into(),
    });
    gate
}

fn events_in_log(gate: &UpdateGate) -> Vec<String> {
    std::fs::read_to_string(gate.log_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v["event"].as_str().map(str::to_string))
        .collect()
}

struct EnvGuard(&'static str);
impl EnvGuard {
    fn set(key: &'static str, val: &str) -> Self {
        std::env::set_var(key, val);
        EnvGuard(key)
    }
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        std::env::remove_var(self.0);
    }
}

/// THE DEFECT. A healthy candidate whose first exec takes 30 s — the old 20 s
/// bound plus 10 s — is accepted. Red before R9.U1: refused "does not pass
/// preflight" with no last-good to fall back to.
#[tokio::test(flavor = "multi_thread")]
async fn a_healthy_candidate_whose_first_exec_outlasts_the_20s_self_check_by_10s_is_accepted() {
    let _serial = serial();
    std::env::remove_var("THREADS_FIRST_EXEC_BUDGET_MS");
    let dir = tempfile::tempdir().unwrap();
    let c = build_wrapper(dir.path(), Wrapper::SlowFirstExec);
    let gate = gate_for(dir.path(), &c);

    let t0 = Instant::now();
    let got = gate.prepare().await;
    let took = t0.elapsed();

    // Non-vacuity: the delay was really injected, inside this preflight.
    assert_eq!(c.slept(), 1, "the candidate slept on its first exec, once: {:?}", c.lines());

    let ready = got.unwrap_or_else(|e| {
        panic!(
            "a healthy candidate with a slow FIRST exec must pass preflight, got: {e}\n\
             (took {took:?}; wrapper log {:?})",
            c.lines()
        )
    });
    // Non-vacuity: the accepted preflight sat through the WHOLE injected first
    // exec, which outlasts the old 20 s bound by 10 s.
    assert!(
        took >= Duration::from_secs(INJECTED_FIRST_EXEC_DELAY_SECS)
            && took > OLD_SELF_CHECK_BOUND + Duration::from_secs(9),
        "the preflight must have sat through a first exec longer than the old 20 s bound \
         (took {took:?})"
    );
    assert_eq!(ready.preflight, Preflight::Ok, "the candidate itself passed, nothing was restored");
    assert_eq!(ready.binary, c.path);
    assert!(ready.banner.contains(update::SELF_CHECK_OK), "banner: {}", ready.banner);
    // The bounded 20 s check still ran AFTER the first exec: at least two runs.
    assert!(c.runs() >= 2, "the timed self-check ran after the first exec: {:?}", c.lines());
    assert!(!events_in_log(&gate).contains(&"refused".to_string()));
}

/// PAIRED. A candidate that hangs on EVERY exec is still refused: the first exec
/// is bounded by `THREADS_FIRST_EXEC_BUDGET_MS` (25 s here, past the old 20 s
/// bound), and the hang outlasts it forever. Last-good is absent, so the gate
/// must refuse rather than exec it.
#[tokio::test(flavor = "multi_thread")]
async fn a_candidate_that_hangs_on_every_exec_is_still_refused_after_a_25s_first_exec_budget() {
    let _serial = serial();
    let budget = Duration::from_secs(25);
    let _env = EnvGuard::set("THREADS_FIRST_EXEC_BUDGET_MS", &budget.as_millis().to_string());
    let dir = tempfile::tempdir().unwrap();
    let c = build_wrapper(dir.path(), Wrapper::HangsEveryExec);
    let gate = gate_for(dir.path(), &c);

    let t0 = Instant::now();
    let got = gate.prepare().await;
    let took = t0.elapsed();

    let err = got.expect_err("a candidate that never answers must not pass preflight");
    assert!(err.contains("REFUSING"), "refused out loud: {err}");
    assert!(events_in_log(&gate).contains(&"refused".to_string()));
    assert!(c.runs() >= 1, "the candidate was really executed: {:?}", c.lines());
    // Bounded: the preflight gives up instead of waiting forever.
    assert!(
        took < budget + OLD_SELF_CHECK_BOUND + Duration::from_secs(30),
        "a hanging candidate must be given up on within the budget (took {took:?})"
    );
    assert!(gate.admitting(), "a refused restart leaves the daemon serving");
}

/// PAIRED. A candidate that exits non-zero is still refused, however quickly
/// or slowly its first exec ran.
#[tokio::test(flavor = "multi_thread")]
async fn a_candidate_that_exits_non_zero_is_still_refused() {
    let _serial = serial();
    std::env::remove_var("THREADS_FIRST_EXEC_BUDGET_MS");
    let dir = tempfile::tempdir().unwrap();
    let c = build_wrapper(dir.path(), Wrapper::ExitsNonZero);
    let gate = gate_for(dir.path(), &c);

    let err = gate
        .prepare()
        .await
        .expect_err("a candidate that exits non-zero must not pass preflight");
    assert!(err.contains("REFUSING"), "refused out loud: {err}");
    assert!(events_in_log(&gate).contains(&"refused".to_string()));
    assert!(c.runs() >= 1, "the candidate was really executed: {:?}", c.lines());
    assert!(gate.admitting());
}
