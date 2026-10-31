//! **R9.C7f7 (gap C-7, inspect R9.C7f6 finding 1) — A FAILED `env create`
//! NEVER HANDS A WAITING WRITER A LOCK ON AN ORPHAN INODE, AND REMOVES ONLY
//! WHAT IT WROTE.**
//!
//! `env create` holds the env.toml writers' lock (`data_root/.env.toml.lock`)
//! while it seeds. Its failure cleanup used to `remove_dir_all(data_root)`
//! while still holding that lock: it unlinked the very inode it held locked.
//! A writer already blocked on the lock holds an fd on that inode, so when
//! the create exited the waiter was granted a lock on an ORPHAN, and any later
//! writer made the directory again and locked a fresh inode — two processes
//! holding "the" lock at once. The same call deleted files other writers had
//! put in the directory. The cleanup now keeps the lock file (and the
//! directory holding it) and removes only what this create wrote.
//!
//! REAL PROCESSES, N = 3: A is a `cauldron env create --from` that fails
//! (the source holds a symlink, which `copy_tree` refuses) after taking the
//! lock, held there by `CAULDRON_TEST_CREATE_LOCKED_MS` so the others can
//! queue behind it; W is a separate process (python3, `fcntl.flock` — the
//! same BSD `flock` std's `File::lock` takes) blocked on the lock file, which
//! reports, once granted, whether the inode it holds is still the one the
//! path names; B is a `cauldron pin` into the same environment while W holds
//! the lock, which must wait for W if — and only if — W's lock is real.
//!
//! Every test runs a THROWAWAY cauldron under the system temp dir. Nothing
//! touches `~/.cauldron` and no daemon is started.

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[path = "support/launch_wait.rs"]
mod launch_wait;
use launch_wait::START_TIMEOUT_MS;

/// How long A holds the writers' lock before it fails.
const LOCKED_MS: u64 = 3000;
/// How long W holds the lock once it is granted.
const W_HOLD_MS: u64 = 3000;

const WAITER: &str = r#"
import fcntl, os, sys, time
p, hold_ms = sys.argv[1], int(sys.argv[2])
fd = os.open(p, os.O_RDWR)            # never O_CREAT: it must be A's inode
print("waiting", flush=True)
t0 = time.time()
fcntl.flock(fd, fcntl.LOCK_EX)
waited = int((time.time() - t0) * 1000)
st = os.fstat(fd)
try:
    at_path = os.stat(p).st_ino
except FileNotFoundError:
    at_path = -1
print("got ino=%d nlink=%d path_ino=%d waited_ms=%d" % (st.st_ino, st.st_nlink, at_path, waited), flush=True)
time.sleep(hold_ms / 1000.0)
print("released", flush=True)
"#;

struct Sandbox {
    root: PathBuf,
    env: String,
    src: String,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if std::env::var_os("KEEP_SANDBOX").is_none() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

impl Sandbox {
    /// A root declaring `s1`, a SOURCE environment that pins it and whose
    /// `s1` data holds a regular file and a symlink — so `env create --from`
    /// it fails inside its locked section, after copying has begun.
    fn new(tag: &str) -> Sandbox {
        let root = std::env::temp_dir().join(format!(
            "cauldron-c7f7-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        assert!(!root.starts_with(home.join(".cauldron")), "sandbox must not be ~/.cauldron");
        std::fs::create_dir_all(root.join("services")).unwrap();
        let sb = Sandbox { root, env: format!("c7f7{tag}"), src: format!("c7f7{tag}src") };
        std::fs::write(
            sb.root.join("services").join("s1.toml"),
            format!(
                "name = \"s1\"\nkind = \"daemon\"\nregisters = true\nprobe = \"seam\"\n\
                 stop_signal = \"TERM\"\nstart_timeout_ms = {START_TIMEOUT_MS}\n\
                 [build]\nartifact = \"{}\"\nbin = \"fake\"\n",
                sb.root.join("fake").display()
            ),
        )
        .unwrap();
        let (rc, out) = run(sb.cmd(&["--env", &sb.src, "pin", "s1", "latest"]));
        assert_eq!(rc, 0, "pin s1 into the source: {out}");
        let data = sb.root.join("data").join(&sb.src).join("s1");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("a-store.db"), "rows").unwrap();
        std::os::unix::fs::symlink("/etc/hosts", data.join("b-link")).unwrap();
        assert!(!sb.data_root().exists(), "the environment must not exist yet");
        sb
    }

    fn data_root(&self) -> PathBuf {
        self.root.join("data").join(&self.env)
    }

    fn lock_path(&self) -> PathBuf {
        self.data_root().join(".env.toml.lock")
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_cauldron"));
        c.args(args)
            .env("CAULDRON_HOME", &self.root)
            .env("CAULDRON_ENV", &self.env)
            .env_remove("CAULDRON_TEST_CREATE_WINDOW_MS")
            .env_remove("CAULDRON_TEST_CREATE_LOCKED_MS")
            .env_remove("CAULDRON_TEST_ENSURE_WINDOW_MS")
            .env_remove("CAULDRON_TEST_PIN_WINDOW_MS")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }

    fn failing_create(&self) -> Command {
        self.cmd(&["env", "create", &self.env, "--from", &self.src])
    }

    fn pinned(&self) -> Vec<String> {
        let t = std::fs::read_to_string(self.data_root().join("env.toml")).unwrap_or_default();
        let v: toml::Value = toml::from_str(&t).unwrap_or_else(|e| panic!("{e}: {t}"));
        let mut out: Vec<String> = v
            .get("services")
            .and_then(|s| s.as_table())
            .map(|t| t.keys().cloned().collect())
            .unwrap_or_default();
        out.sort();
        out
    }
}

impl Sandbox {
    /// `cauldron envs --json`'s environment list, asked from the SOURCE
    /// environment so `self.env` is not added as "the one standing in".
    fn listed(&self) -> Vec<String> {
        let o = self.cmd(&["--env", &self.src, "envs", "--json"]).output().expect("spawn envs");
        let out = String::from_utf8_lossy(&o.stdout).to_string();
        assert_eq!(o.status.code(), Some(0), "envs: {out}{}", String::from_utf8_lossy(&o.stderr));
        let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        v["environments"].as_array().expect("an environments array").iter().filter_map(|n| n.as_str().map(str::to_string)).collect()
    }
}

fn run(mut c: Command) -> (i32, String) {
    let o = c.output().expect("spawn cauldron");
    (o.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
}

/// A child whose output lines arrive on a channel as they are written.
struct Lines {
    child: Child,
    rx: mpsc::Receiver<String>,
    seen: String,
}

impl Lines {
    fn spawn(mut c: Command, stderr: bool) -> Lines {
        let mut child = c.spawn().expect("spawn");
        let (tx, rx) = mpsc::channel();
        let pipe: Box<dyn Read + Send> = if stderr {
            Box::new(child.stderr.take().unwrap())
        } else {
            Box::new(child.stdout.take().unwrap())
        };
        std::thread::spawn(move || {
            for line in BufReader::new(pipe).lines() {
                if tx.send(line.unwrap_or_default()).is_err() {
                    break;
                }
            }
        });
        Lines { child, rx, seen: String::new() }
    }

    /// The first line containing `needle`, within `budget`.
    fn until(&mut self, needle: &str, budget: Duration) -> String {
        let deadline = Instant::now() + budget;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(l) => {
                    self.seen.push_str(&l);
                    self.seen.push('\n');
                    if l.contains(needle) {
                        return l;
                    }
                }
                Err(_) => panic!("no line containing {needle:?} within {budget:?}; saw:\n{}", self.seen),
            }
        }
    }
}

fn field(line: &str, key: &str) -> i64 {
    line.split_whitespace()
        .find_map(|w| w.strip_prefix(&format!("{key}=")))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("no {key}= in {line:?}"))
}

/// THE CLAIM. A fails holding the lock; W, queued behind it, must be granted
/// a lock on the inode the path still names (not an orphan, nlink 0), and
/// that lock must be REAL: a `cauldron pin` B started while W holds it waits
/// for W. A's cleanup must also leave a file another writer put in the
/// directory. Forced red by restoring `remove_dir_all(data_root)`.
#[test]
fn c7f7_a_waiter_queued_behind_a_create_that_fails_holding_the_lock_3000ms_never_holds_an_orphan() {
    let sb = Sandbox::new("orph");
    let mut a_cmd = sb.failing_create();
    a_cmd.env("CAULDRON_TEST_CREATE_LOCKED_MS", LOCKED_MS.to_string());
    let mut a = Lines::spawn(a_cmd, true);
    a.until("env create holds the env.toml lock", Duration::from_secs(60));
    let a_locked = Instant::now();
    assert!(sb.lock_path().is_file(), "premise: A holds a lock file at {}", sb.lock_path().display());
    let lock_ino = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(sb.lock_path()).unwrap());
    // Another writer's file in the directory, as a holder's `supervisor.lock`.
    std::fs::write(sb.data_root().join("supervisor.lock"), "another writer's").unwrap();

    let mut w = Lines::spawn(
        {
            let mut c = Command::new("/usr/bin/env");
            c.args(["python3", "-c", WAITER])
                .arg(sb.lock_path())
                .arg(W_HOLD_MS.to_string())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit());
            c
        },
        false,
    );
    w.until("waiting", Duration::from_secs(60));
    assert!(
        a_locked.elapsed() < Duration::from_millis(LOCKED_MS),
        "premise: W queued after A's locked window closed ({:?})",
        a_locked.elapsed()
    );

    let st = a.child.wait().unwrap();
    let mut a_out = String::new();
    a.child.stdout.take().unwrap().read_to_string(&mut a_out).unwrap();
    while let Ok(l) = a.rx.recv_timeout(Duration::from_millis(500)) {
        a.seen.push_str(&l);
        a.seen.push('\n');
    }
    let a_text = format!("{a_out}{}", a.seen);
    assert_ne!(st.code(), Some(0), "premise: A was meant to fail:\n{a_text}");
    assert!(a_text.contains("was NOT created"), "premise: A did not take its failure path:\n{a_text}");

    let got = w.until("got", Duration::from_secs(30));
    let w_granted = Instant::now();
    // Sentinel: W really was blocked behind A (it queued inside A's window).
    assert!(field(&got, "waited_ms") >= 500, "premise: W was not blocked behind A: {got}");
    // THE CLAIM: the inode W holds is linked, and it is the one the path names.
    assert!(field(&got, "nlink") >= 1, "W was granted a lock on an ORPHAN inode (unlinked): {got}\nA:\n{a_text}");
    assert_eq!(field(&got, "path_ino"), field(&got, "ino"), "W's locked inode is not the one at the path: {got}");
    assert_eq!(field(&got, "ino") as u64, lock_ino, "W holds a different inode from the one A locked: {got}");
    // A removed only what it wrote: the copy, not another writer's file.
    assert!(sb.data_root().join("supervisor.lock").is_file(), "A's cleanup deleted another writer's file:\n{a_text}");
    assert!(!sb.data_root().join("s1").exists(), "A left its half-copied `s1` data behind:\n{a_text}");
    assert!(!sb.data_root().join("env.toml").exists(), "A left an env.toml behind");

    // AND THE LOCK IS REAL: B, a third process, must wait for W.
    let t_b = Instant::now();
    let (rc, out) = run(sb.cmd(&["pin", "s1", "latest"]));
    let b_took = t_b.elapsed();
    assert_eq!(rc, 0, "pin B: {out}");
    // W releases W_HOLD_MS after it was granted (granted no later than
    // `w_granted`, when its line was read), so B — started `t_b - w_granted`
    // after that — waits out the rest, less 300 ms of scheduling slack.
    let head = t_b - w_granted;
    assert!(
        b_took + head + Duration::from_millis(300) >= Duration::from_millis(W_HOLD_MS),
        "B did not wait for W's lock (took {b_took:?}, started {head:?} into W's {W_HOLD_MS}ms hold): two \
         processes held the writers' lock at once\nB:\n{out}"
    );
    w.until("released", Duration::from_secs(30));
    let _ = w.child.wait();
    assert_eq!(sb.pinned(), vec!["s1".to_string()], "B's pin is not in env.toml");
}

/// OPPOSED: a create that fails with NO waiter must still clean up — what it
/// left must not read as an environment, so creating it again SUCCEEDS. Red
/// under a cleanup that removes nothing (the half-copied `s1` makes the
/// directory an environment, and the re-create is refused "already exists").
#[test]
fn c7f7_b_a_failed_create_leaves_nothing_that_blocks_creating_it_again() {
    let sb = Sandbox::new("again");
    let (rc, out) = run(sb.failing_create());
    assert_ne!(rc, 0, "premise: the create was meant to fail: {out}");
    assert!(out.contains("was NOT created"), "{out}");
    assert!(!sb.data_root().join("s1").exists(), "the half-copied `s1` data was left behind: {out}");
    assert!(!sb.data_root().join("env.toml").exists(), "an env.toml was left behind: {out}");
    // F2 (inspect R9.C7f7): what the failed create left is not listed as an
    // environment. Sentinel: the directory IS there (the lock file is kept),
    // so a listing of every directory would show it.
    assert!(sb.data_root().join(".env.toml.lock").is_file(), "sentinel: the kept lock file is gone");
    let listed = sb.listed();
    assert!(listed.contains(&sb.src), "sentinel: `envs` does not list the source environment: {listed:?}");
    assert!(!listed.contains(&sb.env), "`envs` lists a failed create's leftover as an environment: {listed:?}");
    let (rc, out2) = run(sb.cmd(&["env", "create", &sb.env]));
    assert_eq!(rc, 0, "re-creating after a failed create was refused:\n{out2}\nfirst:\n{out}");
    assert!(sb.data_root().join("env.toml").is_file(), "the re-create wrote no env.toml: {out2}");
    // Opposed: once created, it IS listed.
    assert!(sb.listed().contains(&sb.env), "`envs` does not list a created environment");
}

/// OPPOSED, the other way: a directory holding anything beyond what a failed
/// create leaves (here another writer's file) is still an environment that
/// exists, and `env create` still refuses it — so `vacant` is not "always".
#[test]
fn c7f7_c_a_data_root_with_another_writers_file_is_still_refused() {
    let sb = Sandbox::new("occ");
    std::fs::create_dir_all(sb.data_root().join("logs")).unwrap();
    std::fs::write(sb.data_root().join(".env.toml.lock"), "").unwrap();
    std::fs::write(sb.data_root().join("supervisor.lock"), "another writer's").unwrap();
    let (rc, out) = run(sb.cmd(&["env", "create", &sb.env]));
    assert_ne!(rc, 0, "created over a directory another writer is using: {out}");
    assert!(out.contains("already exists"), "{out}");
    // ...and `envs` agrees it exists (the same predicate, the other answer).
    assert!(sb.listed().contains(&sb.env), "`envs` hides a data root another writer is using");
    // Its pair: the same directory minus that file IS vacant, and is created.
    std::fs::remove_file(sb.data_root().join("supervisor.lock")).unwrap();
    let (rc, out) = run(sb.cmd(&["env", "create", &sb.env]));
    assert_eq!(rc, 0, "a vacant directory (lock file + empty logs/) was refused: {out}");
}
