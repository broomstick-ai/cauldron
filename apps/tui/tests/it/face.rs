//! **THE ONE WAY A RUST TEST STARTS THE BUILT FACE** (REL-1.S1c).
//!
//! `drive_isolation` checks every shell and python launch of the face, but
//! Rust integration tests that spawned the built binary were outside every
//! isolation check (REL-1.S1b report §7): each was isolated only as far as
//! its author remembered to be — one launched the chat face with `AUI_V2_URL`
//! unset, i.e. at the live ui-server on 9010.
//!
//! So every test that runs the binary goes through [`Face`], and
//! `drive_isolation::every_rust_test_starts_the_face_through_the_helper`
//! fails if any file under `tests/` names the binary anywhere else.
//!
//! [`Face`] starts from the whole isolation set — a scratch HOME and
//! CAULDRON_HOME, opener `none`, TYPE_ONE `off`, port 1 for both threads
//! levers, a dead AUI port, a harmless CAULDRON_BIN, the test-only marker
//! CAULDRON_ISOLATED=1 (REL-6a2), no TMUX, no retired
//! TUI_THREADS_URL — and lets a test override any of it. It does not trust the
//! overrides: [`Face::check`] judges the FINAL environment immediately before
//! every spawn, and a live or missing value panics before a process exists.
//! The names it requires are pinned equal to `drive_isolation`'s required set.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Output, Stdio};
use std::os::unix::process::CommandExt;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// **EVERY FACE SPAWN IS BOUNDED** (REL-6a4, REL-1.S1c-I N2). A launch that
/// outlives this is killed — the process this helper spawned, its process
/// GROUP (every launch is its own group, REL-6a4b) and its descendants, never
/// anything by name ([`FaceChild`]) — and the test fails naming the launch. Before this,
/// a face that never exited held the whole suite for as long as cargo let it
/// (REL-6a3's full-1: over an hour, then a SIGTERM nobody sent). Override per
/// launch with [`Face::timeout`].
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(180);

/// The built face. The only place under `tests/` that names it.
const BIN: &str = env!("CARGO_BIN_EXE_tui");

/// Live listeners on this machine (as `drive_isolation::LIVE_PORTS`).
const LIVE_PORTS: &[u16] = &[3651, 3653, 3753, 9010];

/// The fixed half of the isolation set: (name, isolated default).
pub const FIXED: &[(&str, &str)] = &[
    ("TUI_THREADS_CHASSIS_PORT", "1"),
    ("TUI_THREADS_PORT", "1"),
    ("AUI_V2_URL", "ws://127.0.0.1:1/"),
    ("CAULDRON_BIN", "/usr/bin/false"),
    ("CAULDRON_OPENER", "none"),
    ("CAULDRON_TYPE_ONE", "off"),
    // REL-6a2's test-only isolation MARKER (`cauldron::live::ISOLATED_ENV` on
    // rel/rel6-threads-tab): every test launch says it is one. Not read on
    // this branch; required here so a Face launch carries it when it is.
    (MARKER, "1"),
];

/// The marker's name. Not an address, so `drive_isolation`'s required set
/// gains it only where the source reads it (REL-6a2's LAUNCH_ALSO row).
pub const MARKER: &str = "CAULDRON_ISOLATED";

/// The path half: each defaults to a fresh dir under the test's scratch dir.
pub const PATHS: &[(&str, &str)] = &[("HOME", "home"), ("CAULDRON_HOME", "cauldron")];

/// Never handed to the face (TMUX: terminal negotiation would query his tmux
/// server; TUI_THREADS_URL: retired, nothing reads it).
const DROPPED: &[&str] = &["TMUX", "TUI_THREADS_URL"];

/// First arguments after which the face reads its environment and exits
/// without connecting to anything. Only these may run with a required var
/// deliberately UNSET ([`Face::unset_for_offline_verb`]).
const OFFLINE_VERBS: &[&str] = &["--build-stamp", "--resolve", "--help"];

/// Every name [`Face::check`] requires.
pub fn names() -> Vec<&'static str> {
    FIXED.iter().chain(PATHS).map(|(n, _)| *n).collect()
}

pub struct Face {
    cmd: Command,
    defaults: Vec<(String, OsString)>,
    /// Arguments given to the FACE (not to a wrapper).
    face_args: Vec<String>,
    unset: Vec<&'static str>,
    cleared: bool,
    /// [`Face::unset_lever_resolving_to`]: the fake the launch must resolve to.
    probe: Option<u16>,
    /// The hard bound on this launch ([`DEFAULT_TIMEOUT`] unless set).
    limit: Duration,
    /// Which of stdin/stdout/stderr the test configured, so [`Face::output`]
    /// can default the rest exactly as `Command::output` does.
    set_io: [bool; 3],
}

impl Face {
    /// `BIN` itself, isolated under `scratch` (a dir the test owns).
    pub fn new(scratch: &Path) -> Face {
        Face::build(Command::new(BIN), scratch)
    }

    /// `program pre_args… BIN`, for a wrapper that execs its last argument
    /// with the environment it was given (`script(1)`, a python pty driver).
    pub fn wrapped(scratch: &Path, program: impl AsRef<OsStr>, pre_args: &[&str]) -> Face {
        let mut c = Command::new(program);
        c.args(pre_args).arg(BIN);
        Face::build(c, scratch)
    }

    fn build(mut cmd: Command, scratch: &Path) -> Face {
        // **ITS OWN PROCESS GROUP** (REL-6a4b, REL-6a4-I N6): the watchdog
        // kills the group, so a wrapper's face (a grandchild) dies with it.
        cmd.process_group(0);
        let mut defaults: Vec<(String, OsString)> =
            FIXED.iter().map(|(k, v)| (k.to_string(), OsString::from(v))).collect();
        for (k, sub) in PATHS {
            let p = scratch.join(sub);
            std::fs::create_dir_all(&p).unwrap_or_else(|e| panic!("scratch {}: {e}", p.display()));
            defaults.push((k.to_string(), p.into_os_string()));
        }
        let mut f = Face {
            cmd,
            defaults,
            face_args: Vec::new(),
            unset: Vec::new(),
            cleared: false,
            probe: None,
            limit: DEFAULT_TIMEOUT,
            set_io: [false; 3],
        };
        f.apply_defaults();
        f
    }

    fn apply_defaults(&mut self) {
        for (k, v) in &self.defaults {
            self.cmd.env(k, v);
        }
        for k in DROPPED {
            self.cmd.env_remove(k);
        }
    }

    pub fn arg(&mut self, a: impl AsRef<OsStr>) -> &mut Face {
        self.face_args.push(a.as_ref().to_string_lossy().into_owned());
        self.cmd.arg(a);
        self
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut Face
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for a in args {
            self.arg(a);
        }
        self
    }

    pub fn env(&mut self, k: impl AsRef<OsStr>, v: impl AsRef<OsStr>) -> &mut Face {
        self.cmd.env(k, v);
        self
    }

    pub fn envs<I, K, V>(&mut self, vars: I) -> &mut Face
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        for (k, v) in vars {
            self.env(k, v);
        }
        self
    }

    /// Removes a var that is NOT part of the isolation set. Removing one that
    /// is panics: say why with [`Face::unset_for_offline_verb`].
    pub fn env_remove(&mut self, k: &str) -> &mut Face {
        assert!(!names().contains(&k), "Face::env_remove({k}): {k} is isolation; use unset_for_offline_verb");
        self.cmd.env_remove(k);
        self
    }

    /// Nothing inherited from the test process; the isolation set is put back.
    pub fn env_clear(&mut self) -> &mut Face {
        self.cmd.env_clear();
        self.cleared = true;
        self.apply_defaults();
        self
    }

    /// Unset a required var for a verb that connects to nothing (a refusal
    /// probe). [`Face::check`] refuses it unless the face's first argument is
    /// one of [`OFFLINE_VERBS`].
    pub fn unset_for_offline_verb(&mut self, k: &'static str) -> &mut Face {
        assert!(names().contains(&k), "{k} is not an isolation var");
        self.cmd.env_remove(k);
        self.unset.push(k);
        self
    }

    /// **A RULE-A REFUSAL PROBE** (REL-6a3): unset BOTH halves of the threads
    /// lever, so a non-interactive launch can be shown to refuse because
    /// nothing points it. Safe only because, immediately before the spawn,
    /// [`Face::check_probe`] runs `--resolve` under the SAME environment and
    /// requires it to land on `fake` — a test daemon the test started, never
    /// one of his ports — so if the refusal were missing, the dial would reach
    /// that fake (seen by the test), not his daemon.
    pub fn unset_lever_resolving_to(&mut self, fake: u16) -> &mut Face {
        assert!(!LIVE_PORTS.contains(&fake) && fake != 0, "a probe resolves to a fake, never {fake}");
        for k in ["TUI_THREADS_CHASSIS_PORT", "TUI_THREADS_PORT"] {
            self.cmd.env_remove(k);
            self.unset.push(k);
        }
        self.probe = Some(fake);
        self
    }

    /// For a probe: `--resolve` under this launch's exact environment, which
    /// must name the probe's fake.
    fn check_probe(&self) -> Result<(), String> {
        let Some(fake) = self.probe else { return Ok(()) };
        let mut r = Command::new(BIN);
        if self.cleared {
            r.env_clear();
        }
        for (k, v) in self.cmd.get_envs() {
            match v {
                Some(v) => r.env(k, v),
                None => r.env_remove(k),
            };
        }
        let out = r.arg("--resolve").stdin(Stdio::null()).output().map_err(|e| format!("--resolve: {e}"))?;
        let said = String::from_utf8_lossy(&out.stdout).into_owned();
        let port = said
            .split_whitespace()
            .find_map(|w| w.strip_prefix("port="))
            .and_then(|p| p.parse::<u16>().ok());
        match port {
            Some(p) if p == fake && !LIVE_PORTS.contains(&p) => Ok(()),
            _ => Err(format!("an unpointed probe must resolve to its fake :{fake}; --resolve said {said:?} (stderr {:?})", String::from_utf8_lossy(&out.stderr))),
        }
    }

    pub fn stdin(&mut self, s: impl Into<Stdio>) -> &mut Face {
        self.set_io[0] = true;
        self.cmd.stdin(s);
        self
    }

    pub fn stdout(&mut self, s: impl Into<Stdio>) -> &mut Face {
        self.set_io[1] = true;
        self.cmd.stdout(s);
        self
    }

    pub fn stderr(&mut self, s: impl Into<Stdio>) -> &mut Face {
        self.set_io[2] = true;
        self.cmd.stderr(s);
        self
    }

    /// This launch's hard bound, instead of [`DEFAULT_TIMEOUT`].
    pub fn timeout(&mut self, limit: Duration) -> &mut Face {
        self.limit = limit;
        self
    }

    /// What a timeout message names: the program and the face's arguments.
    fn what(&self) -> String {
        format!("{} {:?}", self.cmd.get_program().to_string_lossy(), self.face_args)
    }

    /// Why this environment may not be handed to the face, or Ok. Judged on
    /// the Command's FINAL explicit environment, so it sees every override.
    pub fn check(&self) -> Result<(), String> {
        let envs: BTreeMap<String, Option<String>> = self
            .cmd
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned())))
            .collect();
        let get = |k: &str| envs.get(k).cloned().flatten();
        for k in DROPPED {
            if !self.cleared && envs.get(*k) != Some(&None) {
                return Err(format!("{k} is not removed"));
            }
            if get(k).is_some() {
                return Err(format!("{k} is set"));
            }
        }
        let homes: Vec<PathBuf> =
            ["HOME", "CAULDRON_HOME"].iter().filter_map(|k| get(k)).map(|v| canon(Path::new(&v))).collect();
        for name in names() {
            if self.unset.contains(&name) {
                let first = self.face_args.first().map(String::as_str).unwrap_or("");
                let probed = self.probe.is_some() && matches!(name, "TUI_THREADS_CHASSIS_PORT" | "TUI_THREADS_PORT");
                if !probed && !OFFLINE_VERBS.contains(&first) {
                    return Err(format!(
                        "{name} is unset, which only an offline verb ({OFFLINE_VERBS:?}) may do; the face's first argument is {first:?}"
                    ));
                }
                if get(name).is_some() {
                    return Err(format!("{name} was unset for an offline verb and then set again"));
                }
                continue;
            }
            let Some(v) = get(name) else {
                return Err(format!("{name} is not set on this launch"));
            };
            let v = v.trim();
            let bad = match name {
                "TUI_THREADS_CHASSIS_PORT" | "TUI_THREADS_PORT" => {
                    !matches!(v.parse::<u16>(), Ok(p) if p != 0 && !LIVE_PORTS.contains(&p))
                }
                "AUI_V2_URL" => !aui_ok(v),
                "CAULDRON_BIN" => v.is_empty() || Path::new(v).file_name().is_some_and(|f| f.to_string_lossy().starts_with("cauldron")),
                "CAULDRON_TYPE_ONE" => v != "off",
                MARKER => v != "1",
                // `none`, or a recorder the test planted in one of its own
                // scratch dirs (opener_guard_0327).
                "CAULDRON_OPENER" => v != "none" && !homes.iter().any(|h| canon(Path::new(v)).starts_with(h)),
                "HOME" | "CAULDRON_HOME" => !scratch_ok(Path::new(v)),
                other => unreachable!("an isolation name with no rule: {other}"),
            };
            if bad {
                return Err(format!("{name}={v:?} is live, empty or not a scratch value"));
            }
        }
        Ok(())
    }

    fn checked(&mut self) -> &mut Command {
        self.give_ports();
        if let Err(why) = self.check().and_then(|()| self.check_probe()) {
            panic!("REFUSED to start the face: {why}");
        }
        &mut self.cmd
    }

    /// **REL-6a6 / REL-6a8: HAND THE FACE THE SCRATCH LISTENERS THIS TEST
    /// OPENED.** Under isolation the face dials only listeners it was GIVEN
    /// (`env::egress::ALLOW_ENV`). The helper gives it exactly the listeners
    /// this test process bound and granted (`env::egress::granted` — every
    /// fixture grants its listener — each as `addr:port@<this pid>`, so the
    /// face re-checks at every dial that THIS process still holds it), plus
    /// any value the test set itself. A lever pointed anywhere else is
    /// refused by the face.
    fn give_ports(&mut self) {
        let k = env::egress::ALLOW_ENV;
        let mut grants = env::egress::granted();
        let set: Option<String> = self
            .cmd
            .get_envs()
            .find(|(n, _)| *n == OsStr::new(k))
            .and_then(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()));
        if let Some(v) = set {
            grants.extend(env::egress::parse_grants(&v).unwrap_or_else(|e| panic!("REFUSED to start the face: {e}")));
        }
        assert!(
            !grants.iter().any(|g| env::egress::NEVER_PORTS.contains(&g.addr.port())),
            "REFUSED to start the face: {k} names a live port: {grants:?}"
        );
        self.cmd.env(k, env::egress::grants_value(grants));
    }

    /// Spawn, bounded: the [`FaceChild`] kills its own process at the limit
    /// and its `wait`/`try_wait` then fail the test.
    pub fn spawn(&mut self) -> io::Result<FaceChild> {
        let (what, limit) = (self.what(), self.limit);
        let child = self.checked().spawn()?;
        Ok(FaceChild::watch(child, what, limit))
    }

    /// `Command::output`, bounded: stdin null and both outputs captured unless
    /// the test set them, read on threads while the bound runs; a launch that
    /// outlives it is killed and the test fails here, naming it.
    pub fn output(&mut self) -> io::Result<Output> {
        let [i, o, e] = self.set_io;
        if !i {
            self.cmd.stdin(Stdio::null());
        }
        if !o {
            self.cmd.stdout(Stdio::piped());
        }
        if !e {
            self.cmd.stderr(Stdio::piped());
        }
        let mut child = self.spawn()?;
        let read = |r: Option<Box<dyn io::Read + Send>>| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                if let Some(mut r) = r {
                    let _ = r.read_to_end(&mut buf);
                }
                buf
            })
        };
        let out = read(child.stdout.take().map(|s| Box::new(s) as Box<dyn io::Read + Send>));
        let err = read(child.stderr.take().map(|s| Box::new(s) as Box<dyn io::Read + Send>));
        let status = child.wait()?;
        // **THE READERS ARE BOUNDED TOO** (REL-6a4b, REL-6a4-I N6). The child
        // has exited, but a descendant that inherited a pipe keeps it open
        // and an unbounded `join` would wait for it forever. Past
        // [`PIPE_GRACE`], the launch's group is killed and the test fails
        // here, naming it — without joining a reader that may never finish.
        // KNOWN GAP (REL-6a4b-I F2, documented by MERGE-15): a descendant
        // that LEFT the group (`set -m`, `setsid`) and holds the pipe is not
        // killed — it is reparented, so neither parentage nor the group finds
        // it, and macOS `ps -E` does not show a platform binary's environment,
        // so an env tag cannot find it either (measured). And the group kill
        // is a residual race: if every member of the group is already gone,
        // `-<pgid>` names a free id that could have been reused. The test
        // still fails fast; this is a leak, not a hang.
        let deadline = Instant::now() + PIPE_GRACE;
        while !(out.is_finished() && err.is_finished()) {
            if Instant::now() >= deadline {
                kill_group(child.pid);
                panic!(
                    "FACE LEFT A DESCENDANT HOLDING ITS OUTPUT: `{}` (pid {}) exited {status}, but its stdout/stderr \
                     were still open {PIPE_GRACE:?} later; its process group was killed and this test fails here",
                    child.what, child.pid
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(Output { status, stdout: out.join().unwrap_or_default(), stderr: err.join().unwrap_or_default() })
    }
}

/// How long [`Face::output`] waits, after the child exits, for its pipes to
/// close before it kills the launch's group and fails the test.
pub const PIPE_GRACE: Duration = Duration::from_secs(5);

/// SIGKILL process group `pgid` (`kill -KILL -- -<pgid>`). Every [`Face`]
/// launch leads its own group, so its pid is its group id.
fn kill_group(pgid: u32) {
    let _ = Command::new("/bin/kill")
        .args(["-KILL", "--", &format!("-{pgid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// `kill <sig> <pids…>`, one call.
fn signal(pids: &[u32], sig: &str) {
    if pids.is_empty() {
        return;
    }
    let _ = Command::new("/bin/kill")
        .arg(sig)
        .args(pids.iter().map(u32::to_string))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Every descendant of `pid`, parents before children, from one `ps`
/// snapshot. Taken while `pid` is unreaped, so its children are still its
/// children (a reparented orphan is not found — the group kill is for those).
fn descendants(pid: u32) -> Vec<u32> {
    let Ok(out) = Command::new("/bin/ps").args(["-A", "-o", "pid=,ppid="]).stderr(Stdio::null()).output() else {
        return Vec::new();
    };
    let pairs: Vec<(u32, u32)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            Some((w.next()?.parse().ok()?, w.next()?.parse().ok()?))
        })
        .collect();
    let mut found = vec![pid];
    let mut i = 0;
    while i < found.len() {
        let p = found[i];
        found.extend(pairs.iter().filter(|(_, pp)| *pp == p).map(|(c, _)| *c));
        i += 1;
    }
    found.remove(0);
    found
}

/// **A FACE THE HELPER STARTED, AND A WATCHDOG THAT KILLS ONLY ITS OWN.**
///
/// The `Child` lives behind one lock shared with the watchdog thread. At the
/// limit the watchdog takes the lock and, if the child has not been reaped,
/// kills what that launch started (REL-6a4b): its descendants (one `ps`
/// snapshot, so a wrapper's face that `setsid`'d into its own session —
/// `script(1)`, a python pty — is found by parentage), its process group
/// (`kill -- -<pid>`: a launch from [`Face`] leads its own group, so an
/// orphan still in it dies too), and the child itself (`Child::kill`).
/// Never anything by name. Because every `wait` here is a `try_wait` under
/// the same lock, the pid cannot have been reaped (and reused by an unrelated
/// process, or its group id) between the check and the kill.
/// After a kill, `wait`/`try_wait` panic with the launch named, so the test
/// fails where it waited rather than hanging there.
pub struct FaceChild {
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
    pid: u32,
    watch: Arc<(Mutex<Watched>, Condvar)>,
    what: String,
    limit: Duration,
}

struct Watched {
    child: Child,
    reaped: bool,
    killed: bool,
}

impl FaceChild {
    pub fn watch(mut child: Child, what: String, limit: Duration) -> FaceChild {
        let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let pid = child.id();
        let watch = Arc::new((Mutex::new(Watched { child, reaped: false, killed: false }), Condvar::new()));
        let w = watch.clone();
        let deadline = Instant::now() + limit;
        std::thread::spawn(move || {
            let (lock, cv) = &*w;
            let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if g.reaped {
                    return;
                }
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                g = cv.wait_timeout(g, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
            }
            // Not reaped and out of time: this pid is still our child, and
            // so its group id and its children's pids are still theirs.
            if let Ok(None) = g.child.try_wait() {
                let pid = g.child.id();
                let below = descendants(pid);
                // STOP them first (a stopped parent reaps nothing, so no pid
                // in the snapshot is freed for reuse), then KILL leaf-first.
                signal(&below, "-STOP");
                let leaf_first: Vec<u32> = below.iter().rev().copied().collect();
                signal(&leaf_first, "-KILL");
                kill_group(pid);
                let _ = g.child.kill();
                g.killed = true;
                let _ = g.child.wait();
                g.reaped = true;
            }
        });
        FaceChild { stdin, stdout, stderr, pid, watch, what, limit }
    }

    fn timed_out(&self) -> ! {
        panic!(
            "FACE TIMED OUT: `{}` (pid {}) was still running after {:?}, the helper's hard limit (tests/it/face.rs \
             DEFAULT_TIMEOUT, or Face::timeout); it was killed — that launch, its group and its descendants, nothing by name — and this test fails here",
            self.what, self.pid, self.limit
        )
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let (lock, cv) = &*self.watch;
        let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
        if g.killed {
            drop(g);
            self.timed_out();
        }
        let r = g.child.try_wait();
        if let Ok(Some(_)) = r {
            g.reaped = true;
            cv.notify_all();
        }
        r
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        drop(self.stdin.take());
        loop {
            if let Some(s) = self.try_wait()? {
                return Ok(s);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The launch's pid (its process group id, for a [`Face`] launch).
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// SIGKILL this child (only it), as `Child::kill`.
    pub fn kill(&mut self) -> io::Result<()> {
        let (lock, _) = &*self.watch;
        let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
        g.child.kill()
    }
}

/// `p` with symlinks resolved through its nearest existing ancestor, so a
/// path not yet created compares like one that exists (`/var` is
/// `/private/var` on macOS).
fn canon(p: &Path) -> PathBuf {
    let mut tail = Vec::new();
    let mut at = p;
    loop {
        if let Ok(c) = at.canonicalize() {
            return tail.iter().rev().fold(c, |acc: PathBuf, t| acc.join(t));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_os_string());
                at = parent;
            }
            _ => return p.to_path_buf(),
        }
    }
}

fn aui_ok(v: &str) -> bool {
    let Some(rest) = v.strip_prefix("ws://127.0.0.1:") else { return false };
    let port: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    matches!(port.parse::<u16>(), Ok(p) if p != 0 && !LIVE_PORTS.contains(&p))
}

/// Absolute, and neither his home, inside it (his store is `~/.cauldron`),
/// nor an ancestor of it.
fn scratch_ok(p: &Path) -> bool {
    if !p.is_absolute() {
        return false;
    }
    // REL-6a6: case-folded too (APFS), so `~/.CAULDRON` is his home.
    let fold = |p: PathBuf| PathBuf::from(p.to_string_lossy().to_lowercase());
    let p = fold(canon(p));
    let Some(real) = std::env::var_os("HOME").map(|h| fold(canon(Path::new(&h)))) else { return false };
    !(p.starts_with(&real) || real.starts_with(&p))
}
