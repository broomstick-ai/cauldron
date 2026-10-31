//! **SEVERAL CAULDRONS RUN SIDE BY SIDE, EACH WITH ITS OWN DATA.**
//!
//! The operator's sentence, as a test of the real binary: two throwaway
//! cauldrons, environments `alpha` and `beta`, under ONE throwaway `HOME`
//! (so one root, one shared build cache — the case where isolation could
//! actually fail), each holding its environment with `cauldron --env <name>
//! daemon` AT THE SAME TIME, each hosting the SAME two service names:
//!
//! * `echo` — a REGISTERING service (the `seam-probe` reference binary). It
//!   binds `127.0.0.1:0` and writes per-boot evidence into its own data dir.
//! * `hostedecho` — a HOSTED (foreign, non-registering) service with a
//!   DECLARED port. One declaration, shared by both environments; `beta`
//!   overrides the address in its own `env.toml` (`[services.hostedecho]
//!   addr`), which is the only way two environments can host one foreign
//!   binary without colliding.
//!
//! Then data is written into `alpha` only, and the test shows `beta` does not
//! see it — and that both daemons, both supervisor seams and both copies of
//! each service are up concurrently on distinct sockets.
//!
//! Nothing here touches `~/.cauldron`, launchd, or any live process: `HOME`
//! is a fresh temp dir, `CAULDRON_HOME`/`CAULDRON_ENV`/`ENV` are removed from
//! every child's environment, and every pid this test causes is killed on the
//! way out (including on panic).

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_wait.rs"]
mod launch_wait;
use launch_wait::START_TIMEOUT_MS;

#[path = "support/first_exec.rs"]
mod first_exec;

const BIN: &str = env!("CARGO_BIN_EXE_cauldron");
const LABEL: &str = "0123456789ab-dev";
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// Kills every process this test caused, and removes the throwaway `HOME`, on
/// drop — so a failed assertion leaves neither two daemons and four services
/// running nor a temp dir behind.
///
/// The daemons are killed through their `Child` handles. Every OTHER pid comes
/// from a file (a seam table, a boot line), and a file outlives the process it
/// names: if that service exited earlier, its pid may since have been reused
/// by an unrelated process — on this machine possibly a live service. So a pid
/// read from a file is killed ONLY after [`is_ours`] confirms the process now
/// holding it runs inside this test's own `HOME`.
struct Reaper {
    home: PathBuf,
    daemons: Vec<Child>,
}

impl Drop for Reaper {
    fn drop(&mut self) {
        for d in &mut self.daemons {
            let _ = d.kill();
            let _ = d.wait();
        }
        let mut pids: Vec<u32> = Vec::new();
        for env in ["alpha", "beta"] {
            if let Ok(t) = std::fs::read_to_string(data(&self.home, env).join("seams.json")) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                    if let Some(rows) = v["seams"].as_object() {
                        pids.extend(rows.values().filter_map(|r| r["pid"].as_u64()).map(|p| p as u32));
                    }
                }
            }
        }
        // AND every pid a service wrote into its own boot line. A forced run
        // can fork a child whose row never lands in a seam table (a refused
        // record, a launch killed half way); its boot line is the one place
        // that still names it. Found leaking once, from a forced red.
        for f in files_containing(&self.home, "pid=") {
            if !f.file_name().and_then(|n| n.to_str()).map(|n| n.ends_with("boots.log")).unwrap_or(false) {
                continue;
            }
            let text = std::fs::read_to_string(&f).unwrap_or_default();
            pids.extend(text.split_whitespace().filter_map(|t| t.strip_prefix("pid=")?.parse::<u32>().ok()));
        }
        pids.sort();
        pids.dedup();
        for pid in pids {
            if is_ours(pid, &self.home) {
                let _ = Command::new("kill").args(["-9", &pid.to_string()]).stderr(Stdio::null()).status();
            }
        }
        if std::env::var_os("KEEP_ENV_TEST_HOME").is_none() {
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }
}

/// Is the process NOW holding `pid` one this test caused? Every process
/// cauldron launches runs with its working directory set to its data dir
/// (`cli/src/main.rs`, both launch paths), which is under the throwaway
/// `HOME`. So: its cwd, as the kernel reports it (`lsof -d cwd`), is inside
/// `home`. A reused pid belongs to a process whose cwd is somewhere else, and
/// is left alone. Any failure to read the cwd answers NO: a process we cannot
/// identify is not killed.
fn is_ours(pid: u32, home: &Path) -> bool {
    let Ok(home) = home.canonicalize() else { return false };
    let Ok(out) = Command::new("lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
        .stderr(Stdio::null())
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix('n'))
        .any(|cwd| Path::new(cwd).starts_with(&home))
}

/// A fresh, unique throwaway `HOME`. Unique per call (pid + nanos), so a
/// second test in this binary, or a re-run, never inherits a previous one.
fn fresh_home(tag: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!(
        "cauldron-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&home).unwrap();
    home
}

fn data(home: &Path, env: &str) -> PathBuf {
    home.join(".cauldron").join("data").join(env)
}

fn cauldron(home: &Path, args: &[&str]) -> (i32, String, String) {
    let out = base(home).args(args).output().expect("spawn cauldron");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn base(home: &Path) -> Command {
    let mut c = Command::new(BIN);
    c.env("HOME", home).env_remove("CAULDRON_HOME").env_remove("CAULDRON_ENV").env_remove("ENV");
    c
}

/// The `seam-probe` binary, built from THIS worktree's source by cargo, every
/// time, and returned at the path cargo itself reports for that build.
///
/// It is built UNCONDITIONALLY (R6.S1-land inspect, G1). `cargo test
/// --workspace` does NOT build this bin: seam-probe has no integration test, so
/// a test run compiles only its unittest unit (`Compiling seam-probe (…)` is
/// printed for THAT, not for the bin), and `debug/seam-probe` is whatever the
/// last `cargo build` — from any worktree sharing the target dir — left there.
/// Building only when the file was absent therefore ran stale probes silently.
/// `cargo build` no-ops when fresh, and re-links `debug/seam-probe` from its
/// fingerprinted artifact even when fresh, so a file planted at that path is
/// replaced. The path is taken from cargo's own `compiler-artifact` message
/// rather than guessed. Built once per test process.
fn seam_probe() -> PathBuf {
    static PROBE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    PROBE
        .get_or_init(|| {
            let target = Path::new(BIN).parent().unwrap().parent().unwrap();
            let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
                .args(["build", "-p", "seam-probe", "--bin", "seam-probe"])
                .args(["--message-format=json-render-diagnostics", "--target-dir"])
                .arg(target)
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .stderr(Stdio::inherit())
                .output()
                .expect("spawn cargo build -p seam-probe");
            assert!(out.status.success(), "building seam-probe for the test: {:?}", out.status);
            let exe = String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .filter(|m| m["reason"] == "compiler-artifact" && m["target"]["name"] == "seam-probe")
                .find_map(|m| m["executable"].as_str().map(PathBuf::from))
                .expect("cargo reported no seam-probe executable");
            assert!(exe.exists(), "seam-probe at {}", exe.display());
            exe
        })
        .clone()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Install one fake-but-real build of `service` into the SHARED build dir:
/// the binary is real and runs; the record names a fixed commit.
fn install_build(home: &Path, service: &str, bin_name: &str, src: &Path) {
    let dir = home.join(".cauldron/data/shared/builds").join(service).join(LABEL);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(src, dir.join(bin_name)).unwrap();
    let rec = serde_json::json!({
        "service": service, "label": LABEL, "bin": bin_name, "package": bin_name,
        "profile": "dev", "workspace": "/nowhere", "built_at": 1,
        "commit": COMMIT, "dirty": false, "content": "0123456789abcdef", "source_commit": null
    });
    std::fs::write(dir.join("build.json"), serde_json::to_string_pretty(&rec).unwrap()).unwrap();
    // R9.F1: the copy is a NEW file, so its first exec is assessed again
    // (`support/first_exec.rs`); pay it here, outside every deadline. Neither
    // fixture has its environment here (`seam-probe` reads `CAULDRON_*`,
    // `hostedecho` reads `HOSTED_PORT`), so each exits before it binds.
    let mut once = Command::new(dir.join(bin_name));
    once.current_dir(home).env("HOME", home).env_remove("HOSTED_PORT");
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("CAULDRON_") {
            once.env_remove(k);
        }
    }
    first_exec::pay_first_exec(&mut once, &format!("{service}'s build copy"));
}

/// A foreign daemon: listens on `$HOSTED_PORT` (passed through the
/// declaration's `${CAULDRON_HOSTED_PORT}`), and writes a boot line into its
/// working directory, which cauldron sets to the service's data dir.
const HOSTED_SCRIPT: &str = r#"#!/bin/sh
exec /usr/bin/python3 -c '
import os, socket
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", int(os.environ["HOSTED_PORT"]))); s.listen(8)
open("hosted-boots.log", "a").write("env=%s port=%s pid=%d\n" % (os.environ["CAULDRON_ENV"], os.environ["HOSTED_PORT"], os.getpid()))
while True:
    c, _ = s.accept(); c.close()
'
"#;

fn hosted_declaration(port: u16) -> String {
    format!(
        r#"name        = "hostedecho"
description = "test fixture: a foreign daemon on a declared port"
kind        = "daemon"
registers   = false
probe       = "connect"
stop_signal = "TERM"
escalate    = true
start_timeout_ms = {START_TIMEOUT_MS}

[launch]
addr = "127.0.0.1:{port}"

[env]
HOSTED_PORT = "${{CAULDRON_HOSTED_PORT}}"

[build]
artifact = "/nowhere/hostedecho"
bin      = "hostedecho"
profile  = "release"
"#
    )
}

fn seams(home: &Path, env: &str) -> serde_json::Value {
    let p = data(home, env).join("seams.json");
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(serde_json::Value::Null)
}

fn row<'a>(v: &'a serde_json::Value, service: &str) -> &'a serde_json::Value {
    &v["seams"][service]
}

fn wait_for_rows(home: &Path, env: &str, services: &[&str], within: Duration) -> serde_json::Value {
    let t0 = Instant::now();
    loop {
        let v = seams(home, env);
        if services.iter().all(|s| row(&v, s)["addr"].is_string()) {
            return v;
        }
        if t0.elapsed() > within {
            let log = std::fs::read_to_string(home.join(format!("daemon-{env}.log"))).unwrap_or_default();
            panic!("environment `{env}` did not register {services:?} within {within:?}. seams: {v}\ndaemon log:\n{log}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// One request to a registering service's seam that makes it WRITE: `WARN
/// <nonce>` lands in that service's durable warnings record, inside ITS data
/// dir. This is the "data written in one environment".
fn write_into(addr: &str, nonce: &str) {
    let mut s = TcpStream::connect(addr).expect("connect to alpha's echo");
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(format!("WARN {nonce}\n").as_bytes()).unwrap();
    let mut line = String::new();
    let _ = BufReader::new(&s).read_line(&mut line);
    assert_eq!(line.trim(), "OK", "alpha's echo took the write");
}

/// The ONE directory holding a file named `file` with a line containing
/// `needle`, anywhere under `root` — i.e. where the process that wrote that
/// line keeps its data. Exactly one, or the test fails.
fn boot_dir(root: &Path, file: &str, needle: &str) -> PathBuf {
    // CANONICALISED and de-duplicated, so a directory reachable by two paths
    // (a symlink) is ONE directory here — which is exactly the sharing the
    // isolation assertion has to see, rather than two hits that look distinct.
    let mut hits: Vec<PathBuf> = files_containing(root, needle)
        .into_iter()
        .filter(|p| p.file_name().and_then(|n| n.to_str()) == Some(file))
        .map(|p| p.parent().unwrap().canonicalize().unwrap())
        .collect();
    hits.sort();
    hits.dedup();
    assert_eq!(hits.len(), 1, "exactly one `{file}` directory records `{needle}`: {hits:?}");
    hits.remove(0)
}

/// Every file under `dir` whose bytes contain `needle`.
fn files_containing(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if std::fs::read(&p).map(|b| String::from_utf8_lossy(&b).contains(needle)).unwrap_or(false) {
                out.push(p);
            }
        }
    }
    out
}

#[test]
fn two_environments_run_side_by_side_with_isolated_data_and_no_socket_collision() {
    let home = fresh_home("envs");
    let mut reaper = Reaper { home: home.clone(), daemons: Vec::new() };

    // ── THE SHARED HALF OF THE ROOT: builds and declarations ────────────
    // A forced slow first exec of the `echo` build copy (R9.F1, inert unless
    // `CAULDRON_TEST_SLOW_FIRST_EXEC_SECS` is set): a `#!/bin/sh` front that
    // pays the injected delay once per FILE and then `exec`s the real
    // seam-probe — same pid, same arguments, same environment.
    if first_exec::injected_secs() > 0 {
        let front = home.join("seam-probe-front.sh");
        let body = format!("#!/bin/sh\nexec '{}' \"$@\"\n", seam_probe().display());
        std::fs::write(&front, launch_wait::slow_launch_sh(&first_exec::slow_first_exec_sh(&body, &home.join("first-exec-marks")), &home.join("f2g-marks"))).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&front, std::fs::Permissions::from_mode(0o755)).unwrap();
        install_build(&home, "echo", "seam-probe", &front);
    } else {
        install_build(&home, "echo", "seam-probe", &seam_probe());
    }
    let script = home.join("hostedecho.sh");
    std::fs::write(&script, launch_wait::slow_launch_sh(&first_exec::slow_first_exec_sh(HOSTED_SCRIPT, &home.join("first-exec-marks")), &home.join("f2g-marks"))).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    install_build(&home, "hostedecho", "hostedecho", &script);
    let declared_port = free_port();
    let beta_port = free_port();
    assert_ne!(declared_port, beta_port, "sentinel: the two hosted ports differ");
    std::fs::create_dir_all(home.join(".cauldron/services")).unwrap();
    std::fs::write(home.join(".cauldron/services/hostedecho.toml"), hosted_declaration(declared_port)).unwrap();

    // ── TWO ENVIRONMENTS, CREATED BY THE VERB ───────────────────────────
    for env in ["alpha", "beta"] {
        let (rc, out, err) = cauldron(&home, &["env", "create", env, "--auto-restart"]);
        assert_eq!(rc, 0, "env create {env}: {out}{err}");
        assert!(out.contains(&format!("ai.broomstick.cauldron-{env}")), "its own launchd label: {out}");
        for svc in ["echo", "hostedecho"] {
            // `beta` hosts the foreign binary on ITS OWN address, through the
            // documented command (R6.S3: `pin --addr`). Without it, `pin`
            // REFUSES — alpha already claims the declared port.
            let addr = format!("127.0.0.1:{beta_port}");
            let mut args = vec!["--env", env, "pin", svc, LABEL];
            if env == "beta" && svc == "hostedecho" {
                args.extend(["--addr", addr.as_str()]);
            }
            let (rc, out, err) = cauldron(&home, &args);
            assert_eq!(rc, 0, "pin {svc} in {env}: {out}{err}");
        }
    }
    let t = std::fs::read_to_string(data(&home, "beta").join("env.toml")).unwrap();
    assert!(t.contains(&format!("127.0.0.1:{beta_port}")), "the override landed: {t}");

    // ── BOTH DAEMONS, AT ONCE ───────────────────────────────────────────
    // R9.F1: each environment's own launch path, placed the documented way
    // (`promote`, the same byte copy `daemon` makes at start) and exec'd once
    // here, outside the wait below (`support/first_exec.rs`).
    for env in ["alpha", "beta"] {
        let (rc, out, err) = cauldron(&home, &["--env", env, "promote"]);
        assert_eq!(rc, 0, "promote {env}: {out}{err}");
        let own = home.join(".cauldron/bin").join(format!("cauldron-{env}"));
        assert!(own.is_file(), "`promote` did not place {}: {out}", own.display());
        let mut once = Command::new(&own);
        once.env("HOME", &home).env_remove("CAULDRON_HOME").env_remove("CAULDRON_ENV").env_remove("ENV").arg("envs");
        first_exec::pay_first_exec(&mut once, &format!("{env}'s launch path"));
    }
    for env in ["alpha", "beta"] {
        let log = std::fs::File::create(home.join(format!("daemon-{env}.log"))).unwrap();
        let child = base(&home)
            .args(["--env", env, "daemon"])
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("spawn daemon");
        reaper.daemons.push(child);
    }
    // FIRST waits, sized (R9.F1) from the declared path to the last row: the
    // holder's liveness misses (`down_after` 3 x `poll_secs` 4 = 12 s, the
    // defaults `env create` writes), then `echo`'s launch, bounded by its
    // declared `start_timeout_ms` (5 s, `services/echo.toml`), then
    // `hostedecho`'s, bounded by `HOSTED_BIND_WAIT` (45 s, `cli/src/daemon.rs`)
    // = 62 s; plus the measured worst-case first exec of the three fresh files
    // on that path (the launch path and two build copies — all paid above,
    // still budgeted). Replaces a fixed 60 s that the first execs alone could
    // exceed (base r32 run 1: `hostedecho` row at +53 s, `echo` none at 60 s).
    let first_rows = first_exec::first_wait(Duration::from_secs(3 * 4 + 5 + 45), 3);
    let a = wait_for_rows(&home, "alpha", &["cauldron", "echo", "hostedecho"], first_rows);
    let b = wait_for_rows(&home, "beta", &["cauldron", "echo", "hostedecho"], first_rows);

    // ── CONCURRENT, NO COLLISION ────────────────────────────────────────
    for (i, d) in reaper.daemons.iter_mut().enumerate() {
        assert!(
            d.try_wait().unwrap().is_none(),
            "daemon #{i} is STILL RUNNING while the other is (it did not lose a hold or a bind)"
        );
    }
    for svc in ["cauldron", "echo", "hostedecho"] {
        let (ra, rb) = (row(&a, svc), row(&b, svc));
        assert_ne!(ra["addr"], rb["addr"], "`{svc}`: alpha and beta listen on DIFFERENT sockets");
        assert_ne!(ra["pid"], rb["pid"], "`{svc}`: two processes, not one");
        assert_eq!(ra["env"], "alpha", "alpha's `{svc}` row says alpha");
        assert_eq!(rb["env"], "beta", "beta's `{svc}` row says beta");
    }
    assert_eq!(row(&a, "hostedecho")["addr"], format!("127.0.0.1:{declared_port}"), "alpha: the declared address");
    assert_eq!(row(&b, "hostedecho")["addr"], format!("127.0.0.1:{beta_port}"), "beta: ITS address from env.toml");
    for env in ["alpha", "beta"] {
        let (rc, out, err) = cauldron(&home, &["--env", env, "status", "--json"]);
        assert_eq!(rc, 0, "status in {env} while both run: {out}{err}");
        let (rc, out, _) = cauldron(&home, &["env", "show", env]);
        assert_eq!(rc, 0);
        assert!(out.contains("supervisor     live"), "{env}'s daemon answers its handshake: {out}");
    }

    // ── DATA WRITTEN IN ONE IS NOT SEEN BY THE OTHER ────────────────────
    //
    // Each service's data dir is located by WHERE THE SERVICE ITSELF WROTE its
    // boot line — not by recomputing the layout here. A test that derived
    // beta's dir from the same rule the code uses would agree with the code
    // whatever the code did; this asks the running process.
    let root = home.join(".cauldron");
    let a_echo = boot_dir(&root, "boots.log", "service=echo env=alpha ");
    let b_echo = boot_dir(&root, "boots.log", "service=echo env=beta ");
    assert_eq!(a_echo, data(&home, "alpha").join("echo").canonicalize().unwrap(), "alpha's echo keeps its data under data/alpha/");
    let nonce = format!("isolation-nonce-{}", std::process::id());
    assert!(files_containing(&root, &nonce).is_empty(), "precondition: the nonce is new");
    write_into(row(&a, "echo")["addr"].as_str().unwrap(), &nonce);
    let t0 = Instant::now();
    while files_containing(&a_echo, &nonce).is_empty() && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(50));
    }
    // POSITIVE SENTINEL: the write is findable where it was made, by the same
    // search, so the absence asserted next is the absence of something that
    // exists.
    assert!(!files_containing(&a_echo, &nonce).is_empty(), "alpha's echo wrote the nonce into ALPHA's data");
    let seen_by_beta = files_containing(&b_echo, &nonce);
    assert!(
        a_echo != b_echo && seen_by_beta.is_empty(),
        "ISOLATION: beta's echo (data dir {}) sees alpha's write (alpha's data dir {}): {seen_by_beta:?}",
        b_echo.display(),
        a_echo.display()
    );
    let leaked = files_containing(&data(&home, "beta"), &nonce);
    assert!(leaked.is_empty(), "ISOLATION: alpha's write is anywhere in beta's data root: {leaked:?}");

    let a_hosted = boot_dir(&root, "hosted-boots.log", &format!("env=alpha port={declared_port} "));
    let b_hosted = boot_dir(&root, "hosted-boots.log", &format!("env=beta port={beta_port} "));
    assert_eq!(a_hosted, data(&home, "alpha").join("hostedecho").canonicalize().unwrap());
    assert_ne!(a_hosted, b_hosted, "ISOLATION, hosted: the two hosted instances keep separate data dirs");

    // ── `--env` reaches the right one, and the listing shows both ───────
    let (_, out, _) = cauldron(&home, &["env", "list"]);
    assert!(out.contains("alpha") && out.contains("beta"), "{out}");
    let (_, out, _) = cauldron(&home, &["--env", "beta", "status"]);
    assert!(!out.contains(row(&a, "echo")["addr"].as_str().unwrap()), "beta's status does not show alpha's echo: {out}");
    drop(reaper);
}

/// `--env` is validated at the flag and wins over `$CAULDRON_ENV`; the default
/// is still `prod` at `<root>/data/prod` — today's path, unchanged.
#[test]
fn env_flag_wins_and_the_default_is_prods_existing_path() {
    let home = fresh_home("envflag");
    // Removes the HOME on the way out, panic included (it used to leak one
    // `cauldron-envflag-<pid>` per failed run).
    let _reaper = Reaper { home: home.clone(), daemons: Vec::new() };
    let (rc, out, _) = cauldron(&home, &["env", "show"]);
    assert_eq!(rc, 0);
    assert!(out.contains("env prod (from default)"), "{out}");
    assert!(out.contains(&format!("data {}", home.join(".cauldron/data/prod").display())), "{out}");
    assert!(out.contains("launchd label  ai.broomstick.cauldron\n"), "prod keeps the plain label, under the neutral default prefix (REL-4.S3): {out}");

    let out = base(&home).env("CAULDRON_ENV", "gamma").args(["--env", "delta", "env", "show"]).output().unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("env delta (from --env)"), "the flag wins over the variable: {s}");
    let out = base(&home).env("CAULDRON_ENV", "gamma").args(["env", "show"]).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("env gamma (from $CAULDRON_ENV)"), "sentinel: without the flag the variable is read");

    let (rc, _, err) = cauldron(&home, &["--env", "shared", "env", "show"]);
    assert_ne!(rc, 0, "a reserved name is refused at the flag");
    assert!(err.contains("RESERVED"), "{err}");
    let (rc, _, err) = cauldron(&home, &["--env", "Bad/Name", "status"]);
    assert_ne!(rc, 0);
    assert!(err.contains("not usable"), "{err}");
    // The refusal is asserted BY ITS MESSAGE. `rc != 0` alone cannot fail:
    // before `--env` existed a leading `--env` went to the TUI, which also
    // exits non-zero with no terminal — so a constant "not handled" passed it.
    let (rc, out, err) = cauldron(&home, &["--env"]);
    assert_ne!(rc, 0, "an empty flag is refused, not read as prod: {out}{err}");
    assert!(
        err.contains("`--env` needs an environment name after it"),
        "the empty flag is refused BY THE FLAG PARSER, saying why: stdout {out:?} stderr {err:?}"
    );
    let (rc, _, _) = cauldron(&home, &["env", "create", "eps"]);
    assert_eq!(rc, 0);
    let (rc, _, err) = cauldron(&home, &["env", "create", "eps"]);
    assert_ne!(rc, 0, "creating an existing environment is refused: {err}");
}

/// The `Reaper`'s two promises, under a PANIC: it removes its `HOME`, and it
/// kills a pid named in a boot line ONLY when that process is ours.
///
/// Two `sleep`s, both named by `pid=` in a `boots.log` inside the HOME: one
/// running inside the HOME (stands for a service this test launched), one
/// running OUTSIDE it (stands for an unrelated process that was handed a
/// recycled pid). After the panic: the inside one is dead, the outside one is
/// alive, the HOME is gone. "Kill everything named" fails the second; "kill
/// nothing" fails the first.
#[test]
fn reaper_on_panic_removes_its_home_and_kills_only_processes_inside_it() {
    let home = fresh_home("reaper");
    let elsewhere = fresh_home("reaper-elsewhere");
    let spawn_in = |cwd: &Path| Command::new("/bin/sleep").arg("120").current_dir(cwd).spawn().expect("spawn sleep");
    std::fs::create_dir_all(data(&home, "alpha").join("echo")).unwrap();
    let mut inside = spawn_in(&data(&home, "alpha").join("echo"));
    let mut outside = spawn_in(&elsewhere);
    std::fs::write(
        data(&home, "alpha").join("echo/boots.log"),
        format!("service=echo env=alpha pid={}\nservice=echo env=beta pid={}\n", inside.id(), outside.id()),
    )
    .unwrap();
    // Positive sentinel: the probe CAN say yes, and says no for the outsider.
    assert!(is_ours(inside.id(), &home), "is_ours sees the process running inside the HOME");
    assert!(!is_ours(outside.id(), &home), "is_ours refuses the process running elsewhere");

    let h = home.clone();
    let r = std::panic::catch_unwind(move || {
        let _reaper = Reaper { home: h, daemons: Vec::new() };
        panic!("a failing assertion, mid-test");
    });
    assert!(r.is_err(), "sentinel: the closure did panic");

    let deadline = Instant::now() + Duration::from_secs(5);
    let inside_status = loop {
        if let Some(st) = inside.try_wait().unwrap() {
            break Some(st);
        }
        if Instant::now() > deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let outside_alive = outside.try_wait().unwrap().is_none();
    let home_left = home.exists();
    // Our own handles, so these kills cannot reach a stranger.
    let _ = inside.kill();
    let _ = inside.wait();
    let _ = outside.kill();
    let _ = outside.wait();
    let _ = std::fs::remove_dir_all(&elsewhere);
    let _ = std::fs::remove_dir_all(&home);

    assert!(inside_status.is_some(), "the process inside the HOME was killed by the reaper");
    assert!(outside_alive, "the process OUTSIDE the HOME (a recycled pid) was NOT killed");
    assert!(!home_left, "the HOME was removed on panic: {}", home.display());
}


/// **DG-1: `cauldron gc --builds --dry-run`, the real binary, removes NOTHING**
/// (a byte-for-byte listing of the whole throwaway root before and after), and
/// prints the build it would remove; the same command without `--dry-run`
/// then removes exactly that build and keeps the pin and rollback target.
#[test]
fn dg1_gc_builds_dry_run_removes_nothing_and_the_real_run_removes_what_it_listed() {
    fn listing(dir: &Path, out: &mut Vec<String>) {
        let mut es: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().collect();
        es.sort_by_key(|e| e.path());
        for e in es {
            let md = std::fs::symlink_metadata(e.path()).unwrap();
            let body = if md.is_file() { std::fs::read(e.path()).unwrap() } else { Vec::new() };
            out.push(format!("{} {} {:?} {:?}", e.path().display(), md.len(), md.modified().unwrap(), body));
            if md.is_dir() {
                listing(&e.path(), out);
            }
        }
    }
    let home = fresh_home("dg1-gc");
    let builds = home.join(".cauldron/data/shared/builds/svc");
    for (i, l) in ["a-release", "b-release", "c-release"].iter().enumerate() {
        let d = builds.join(l);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("bin"), vec![1u8; 5000]).unwrap();
        let rec = serde_json::json!({
            "service": "svc", "label": l, "bin": "bin", "package": "svc", "profile": "release",
            "workspace": "/x", "built_at": 100 + i, "commit": l, "source_commit": l
        });
        std::fs::write(d.join("build.json"), rec.to_string()).unwrap();
    }
    std::fs::create_dir_all(data(&home, "prod")).unwrap();
    std::fs::write(
        data(&home, "prod").join("env.toml"),
        "[services.svc]\nversion = \"c-release\"\ngood = [\"c-release\", \"b-release\"]\n",
    )
    .unwrap();
    let mut before = Vec::new();
    listing(&home, &mut before);
    let (rc, out, err) = cauldron(&home, &["gc", "--builds", "--dry-run"]);
    assert_eq!(rc, 0, "dry run failed: {out}\n{err}");
    let mut after = Vec::new();
    listing(&home, &mut after);
    assert_eq!(before, after, "`gc --builds --dry-run` changed the root:\n{out}");
    assert!(out.contains("WOULD REMOVE") && out.contains("a-release"), "{out}");
    assert!(out.contains("would remove 1 build(s)"), "{out}");
    // Opposition: the real run DOES remove, and only what the dry run listed.
    let (rc, out, err) = cauldron(&home, &["gc", "--builds"]);
    assert_eq!(rc, 0, "real run failed: {out}\n{err}");
    assert!(!builds.join("a-release").exists(), "{out}");
    assert!(builds.join("b-release").is_dir() && builds.join("c-release").is_dir(), "{out}");
    let _ = std::fs::remove_dir_all(&home);
}
