//! **A DRIVEN FACE CANNOT REACH A LIVE SERVICE** (R7.S5, H179-1).
//!
//! The crate's drive scripts (`pty_drive.py` and its users, plus the ones that
//! start the face themselves under a pty or a private tmux server) all said a
//! face they drove "cannot reach his daemon even by accident". Each did it by
//! setting `TUI_THREADS_URL`, which NOTHING in `apps/tui/src` reads. The seam
//! reads `TUI_THREADS_CHASSIS_PORT`, default 3653 = PRODUCTION threads. So the
//! claim was false for every one of them, and nothing could have noticed,
//! because nothing compared the variables the scripts set against the ones the
//! face reads.
//!
//! This file is that comparison, in three parts:
//!
//! 1. **The endpoint set is the set `apps/tui/src` reads.** Every env-var-shaped
//!    name in the source is classified — endpoint or not, with the reason — so
//!    a new variable is red until someone decides which it is; and every
//!    endpoint name must be READ (an env read, not merely mentioned), which is
//!    the exact property `TUI_THREADS_URL` lacked. `pty_drive.py`'s own
//!    `ENDPOINT_VARS` must equal it.
//! 2. **Every LAUNCH of the face sets every endpoint var, and HOME, to a
//!    non-live value** (R7.S5b: per launch site, not per file — a tmux
//!    `new-session`, a direct shell command, a python spawn — and HOME, which
//!    the face reads through `dirs::home_dir()` and part 1 therefore cannot
//!    see). Handing the binary to a python driver is not a launch; the
//!    driver's spawn is judged where it happens.
//! 3. **`pty_drive.py` behaves**: run against a stub "face" that dumps its
//!    environment, under a HOSTILE parent environment (every endpoint pointed at
//!    production), the child sees only isolated values; and asked to point the
//!    face at a live port, or at his real HOME, it refuses before the stub runs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The variables that decide where the face connects or what it runs against
/// a live system. Each entry: the name, and where the source reads it.
const ENDPOINT: &[(&str, &str)] = &[
    ("TUI_THREADS_CHASSIS_PORT", "seam::link::chassis_port + cauldron_env::override_port; unset/0/junk -> 3653, prod threads"),
    ("TUI_THREADS_PORT", "seam::link::health_port; mirror::check_ports refuses it alone"),
    ("AUI_V2_URL", "aui::resolve_endpoint; unset -> ws://127.0.0.1:9010/, the live ui-server"),
    ("CAULDRON_BIN", "cauldron_env::cauldron_bin: the restart button runs `<it> --env <env> start threads`"),
    // REL-1.S1b: the `--cauldron` face (same binary, `main.rs` routes on the
    // flag; `cauldron tui` reaches it too) arrived with globs2 and reads two
    // more levers whose UNSET value acts on his live machine. A launch is
    // judged per launch, not per mode, so every launch sets both.
    ("CAULDRON_OPENER", "cauldron::cli::Opener::from_env; unset -> `o` execs the system `open` on his desktop (IS-111); isolated value `none`"),
    ("CAULDRON_TYPE_ONE", "cauldron::seeding::real_call (via SWITCH_ENV); unset -> the `type-one` CLI, Haiku through his threads daemon or Jev through OpenRouter; isolated value `off`"),
    // REL-1.S1c (REL-1.S1b-I F-A): the `--cauldron` face's STORE ROOT. It
    // resolves through `Environment::resolve()` (libs/env, read there, see
    // DEP_READS) and WRITES under it — bucket.json, dumps.jsonl, the global
    // repo registry data/shared/tui/repos.json. Set, it beats HOME, so a
    // parent with his root in CAULDRON_HOME would have a scratch-HOME drive
    // write his live store. Isolated value: a scratch dir.
    ("CAULDRON_HOME", "env::Environment::resolve (libs/env/src/paths.rs), reached from cauldron::app::resolve_env and cauldron_env::resolve; set -> the store root the --cauldron face writes; isolated value a scratch dir"),
];

/// ENDPOINT vars whose READ is in a dependency the face calls, not in
/// `apps/tui/src` (so `scan_src` cannot see it): (name, file relative to the
/// crate, the read's exact text, the call in `apps/tui/src` that reaches it).
/// Each row is VERIFIED, not trusted: the file must contain the read, and
/// `apps/tui/src` must contain the call.
const DEP_READS: &[(&str, &str, &str, &str)] = &[(
    "CAULDRON_HOME",
    "../../libs/env/src/paths.rs",
    "var(\"CAULDRON_HOME\")",
    "Environment::resolve()",
)];

/// Every other env-var-shaped name in `apps/tui/src`, and why it is not an
/// endpoint. A name in neither table is RED: decide, then add it.
const NOT_ENDPOINT: &[(&str, &str)] = &[
    ("TUI_THREADS", "the seam's ENV_PREFIX, not a variable"),
    ("AUI_THREADS", "the AUI's prefix, named in a doc table"),
    ("TUI_THREADS_URL", "RETIRED: nothing reads it (asserted below); a script setting it isolates nothing"),
    ("TUI_THREADS_SECRET", "a credential, not an address"),
    ("THREADS_SECRET", "a credential, not an address"),
    // REL-1.S1c re-justified (REL-1.S1b-I F-A): it names WHICH environment
    // under the root — `data/<name>/` — so once CAULDRON_HOME (ENDPOINT) is a
    // scratch root every name it can pick is a scratch dir; and for the chat
    // face TUI_THREADS_CHASSIS_PORT supersedes it (Source::Override).
    ("CAULDRON_ENV", "picks data/<name>/ under the root; harmless once CAULDRON_HOME is isolated; superseded by TUI_THREADS_CHASSIS_PORT for the seam"),
    // MERGE-V1e: rel/v1 classified CAULDRON_HOME here; rel/halloween's
    // REL-1.S1c moved it to ENDPOINT (stricter: every launch must set it to a
    // scratch root), so it lives there only. V1.2's CAULDRON_ENV_SOURCE stays.
    ("CAULDRON_ENV_SOURCE", "V1.2: whether CAULDRON_ENV was defaulted by the router; picks which existing cauldron opens under the (isolated) CAULDRON_HOME root, never an address"),
    ("CAULDRON_ISOLATED", "the test-only isolation MARKER (REL-6a2, cauldron::live::ISOLATED_ENV): not an address, but required =1 on every launch (LAUNCH_ALSO)"),
    ("CAULDRON_TEST_NOW", "UPTIME-DST (JOBS-DST's pattern): supervise::now's pinned unix seconds, read ONLY in an isolated process; a time, never an address"),
    ("CARGO_PKG_VERSION", "compile time"),
    ("CARGO_MANIFEST_DIR", "compile time"),
    ("TUI_BUILD_COMMIT", "compile time"),
    ("TUI_BUILD_DIRTY", "compile time"),
    ("TUI_BUILD_PROVENANCE", "compile time"),
    ("TUI_BUILD_DIRT_SCOPE", "compile time"),
    ("TUI_LOG", "log filter"),
    ("TUI_DIAG", "flight-recorder path"),
    ("CAULDRON_PROTECTED_HOME", "REL-6a4b: ADDS a home an isolated process may not write under (cauldron::live::PROTECTED_HOME_ENV); can only protect more, points at nothing live"),
    ("TMUX", "terminal negotiation"),
    ("TUI_MIC_CAPTURE", "local capture command"),
    ("TUI_MIC_STT", "local transcription command"),
    ("TUI_MIC_MODEL", "local model path"),
    ("TUI_MIC_PLAY", "local playback command"),
    ("RETRANSCRIBE_MODEL", "local model name"),
    ("TUI_POOL_ACCOUNTS_TOOL", "`tui pool` only; no drive script runs the pool verb"),
    ("CLAUDE_CONFIG_DIR", "`tui pool` only; account config dir"),
    // REL-1.S1b (globs2's `--cauldron` face). These three are read only by
    // `seeding::real_call` AFTER its `CAULDRON_TYPE_ONE=off` early return,
    // which every launch sets (ENDPOINT above), so isolating the switch
    // isolates them.
    ("CAULDRON_TYPE_ONE_BIN", "type_one::find_bin: which `type-one` to run; behind CAULDRON_TYPE_ONE=off"),
    ("CAULDRON_TYPE_ONE_TIMEOUT_S", "type_one::TypeOneCli::new: the child's timeout; behind CAULDRON_TYPE_ONE=off"),
    ("PATH", "type_one::find_bin's fallback search for `type-one`; behind CAULDRON_TYPE_ONE=off"),
    ("STRIPE_SECRET_KEY", "a ticket field NAME in cauldron/tickets.rs's test fixture; never read"),
];

/// Ports with a live listener on this machine: prod threads' chassis, the
/// retired legacy door, stage threads, the AUI's proto-v2 listener.
const LIVE_PORTS: &[u16] = &[3651, 3653, 3753, 9010];

/// Scripts that start the face on purpose against the live daemon, by name,
/// with the reason. Exempt from part 2 — and required to still be launchers,
/// so an exemption cannot outlive its file.
const LIVE_BY_DESIGN: &[(&str, &str)] = &[(
    "live_frames.sh",
    "its whole purpose is frames from the LIVE daemon (its header); ledger R7.S5-2",
)];

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn env_shaped(s: &str) -> bool {
    let mut parts = s.split('_');
    let first = parts.next().unwrap_or("");
    !first.is_empty()
        && first.starts_with(|c: char| c.is_ascii_uppercase())
        && s.contains('_')
        && s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && s.split('_').all(|p| !p.is_empty())
}

/// What the source mentions and what it READS. A read is a literal passed
/// straight to an env reader (`env::var(` / `env::var_os(` / `set(` —
/// `check_ports`'s closure — / `try_from_env(`), or a `{ENV_PREFIX}_X` format
/// string, or (REL-1.S1b) a same-file `const NAME: &str = "X";` passed to
/// `env::var(NAME)` / `env::var_os(NAME)` — the `--cauldron` face reads its
/// switches that way (`seeding::SWITCH_ENV`). `set_var(` / `remove_var(` are
/// writes and do not count.
struct Scan {
    mentioned: BTreeMap<String, String>, // name -> first file
    read: BTreeSet<String>,
}

fn scan_src() -> Scan {
    let src = crate_dir().join("src");
    let link = std::fs::read_to_string(src.join("seam/link.rs")).unwrap();
    assert!(
        link.contains("const ENV_PREFIX: &str = \"TUI_THREADS\";"),
        "the seam's prefix moved; the `{{ENV_PREFIX}}_X` expansion below would be wrong"
    );
    let mut files = Vec::new();
    rs_files(&src, &mut files);
    files.sort();
    let mut mentioned = BTreeMap::new();
    let mut read = BTreeSet::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        let rel = f.strip_prefix(crate_dir()).unwrap().display().to_string();
        // Same-file `const IDENT: &str = "LIT";`, and whether IDENT is handed
        // to an env reader anywhere in the file.
        for line in text.lines() {
            let t = line.trim_start().trim_start_matches("pub ").trim_start_matches("pub(crate) ");
            let Some(rest) = t.strip_prefix("const ") else { continue };
            let Some((ident, rest)) = rest.split_once(": &str = \"") else { continue };
            let Some((lit, _)) = rest.split_once('"') else { continue };
            if is_ident(ident)
                && ["env::var(", "env::var_os("].iter().any(|r| text.contains(&format!("{r}{ident})")))
            {
                read.insert(lit.to_string());
            }
        }
        for line in text.lines() {
            let segs: Vec<&str> = line.split('"').collect();
            // Odd segments are inside quotes (good enough for this source:
            // asserted by the sentinels in `the_endpoint_set_is_what_src_reads`).
            for i in (1..segs.len()).step_by(2) {
                let lit = segs[i];
                let before = segs[i - 1];
                // `env::var(` — NOT bare `var(`, which `remove_var(` and
                // `set_var(` also end with (a forcing caught exactly that:
                // a test's `remove_var("CAULDRON_BIN")` passed for a read).
                // ` set(`/`(set(` is `check_ports`'s reader closure.
                let is_read = ["env::var(", "env::var_os(", " set(", "(set(", "try_from_env("]
                    .iter()
                    .any(|r| before.ends_with(r));
                if let Some(rest) = lit.strip_prefix("{ENV_PREFIX}_") {
                    let name = format!("TUI_THREADS_{rest}");
                    mentioned.entry(name.clone()).or_insert(rel.clone());
                    read.insert(name);
                    continue;
                }
                let shaped = env_shaped(lit)
                    || (is_read && !lit.is_empty() && lit.chars().all(|c| c.is_ascii_uppercase()));
                if shaped {
                    mentioned.entry(lit.to_string()).or_insert(rel.clone());
                    if is_read {
                        read.insert(lit.to_string());
                    }
                }
            }
        }
    }
    Scan { mentioned, read }
}

#[test]
fn the_endpoint_set_is_what_src_reads() {
    let scan = scan_src();
    // LAUNCH_ALSO counts as classified: HOME is set on every launch (part 2)
    // and refused by pty_drive.py when it is his; the `--cauldron` face reads
    // it by name (`tilde()`, REL-1.S1b), not only through dirs::home_dir().
    let classified: BTreeSet<&str> =
        ENDPOINT.iter().chain(NOT_ENDPOINT).chain(LAUNCH_ALSO).map(|(n, _)| *n).collect();

    // Sentinels: the scanner can find a read, can find a mention that is not
    // a read, and expands the prefix format — or every assertion below is
    // measuring nothing.
    assert!(scan.read.contains("AUI_V2_URL"), "scanner found no plain `var(\"..\")` read");
    assert!(scan.read.contains("TMUX"), "scanner missed an underscore-free `var_os(\"TMUX\")`");
    assert!(scan.read.contains("TUI_THREADS_PORT"), "scanner did not expand `{{ENV_PREFIX}}_PORT`");
    assert!(
        scan.read.contains("CAULDRON_TYPE_ONE") && scan.read.contains("CAULDRON_TYPE_ONE_TIMEOUT_S"),
        "scanner did not resolve a `const X: &str` handed to `env::var(X)` (seeding::SWITCH_ENV)"
    );
    assert!(
        scan.mentioned.contains_key("TUI_THREADS_URL") && !scan.read.contains("TUI_THREADS_URL"),
        "TUI_THREADS_URL must be MENTIONED (main.rs's refusal) and NOT READ — that is the bug \
         this file exists for; if it is read now, the drive scripts need it again"
    );

    let unclassified: Vec<String> = scan
        .mentioned
        .iter()
        .filter(|(n, _)| !classified.contains(n.as_str()))
        .map(|(n, f)| format!("{n} (first in {f})"))
        .collect();
    assert!(
        unclassified.is_empty(),
        "apps/tui/src names env vars this guard has not classified. Decide whether each one \
         points the face at a live system; if so add it to ENDPOINT here AND to pty_drive.py \
         and every script part 2 checks: {unclassified:?}"
    );

    // Reads in a dependency (DEP_READS), each verified where it lives.
    let mut src_text = String::new();
    {
        let mut files = Vec::new();
        rs_files(&crate_dir().join("src"), &mut files);
        for f in files {
            src_text.push_str(&std::fs::read_to_string(f).unwrap());
        }
    }
    let mut read = scan.read.clone();
    for (name, file, needle, call) in DEP_READS {
        let dep = std::fs::read_to_string(crate_dir().join(file))
            .unwrap_or_else(|e| panic!("DEP_READS {name}: cannot read {file}: {e}"));
        assert!(dep.contains(needle), "DEP_READS {name}: {file} no longer contains `{needle}`; re-decide the row");
        assert!(src_text.contains(call), "DEP_READS {name}: apps/tui/src no longer calls `{call}`; re-decide the row");
        read.insert(name.to_string());
    }
    let not_read: Vec<&str> =
        ENDPOINT.iter().map(|(n, _)| *n).filter(|n| !read.contains(*n)).collect();
    assert!(
        not_read.is_empty(),
        "these ENDPOINT vars are not READ anywhere in apps/tui/src, so setting them isolates \
         nothing (the TUI_THREADS_URL defect): {not_read:?}"
    );

    // pty_drive.py carries the same list.
    let py = std::fs::read_to_string(crate_dir().join("pty_drive.py")).unwrap();
    let line = py
        .lines()
        .find(|l| l.starts_with("ENDPOINT_VARS = ("))
        .expect("pty_drive.py lost its ENDPOINT_VARS line");
    let py_set: BTreeSet<&str> = line.split('"').skip(1).step_by(2).collect();
    let ours: BTreeSet<&str> = ENDPOINT.iter().map(|(n, _)| *n).collect();
    assert_eq!(py_set, ours, "pty_drive.py's ENDPOINT_VARS and this file disagree");
}

// ── part 2: every LAUNCH of the face carries the isolation set ──────────────
//
// R7.S5's first cut checked per FILE: "this file names the binary, and
// somewhere in it every endpoint var is assigned". Its inspector forced three
// holes through that (`R7.S5-inspect.md`, G1/G3/G4): a second, bare
// `new-session "$BIN"` in a file whose first launch was isolated stayed green;
// a file that merely MENTIONED `pty_drive.py` was waved through even when it
// also ran `"$BIN" --headless` itself; and a binary path that did not end in
// `/debug/tui"` (unquoted, `release/tui`, `cargo run -p tui`) was invisible.
// And HOME was outside the set altogether.
//
// So this part now finds every LAUNCH SITE — every simple shell command and
// every python spawn call — and judges each one on its own words:
//
// - **tmux**: a `new-session`/`new-window`/`split-window`/`respawn-*` whose
//   command names the binary. The set must be `-e NAME=value` ON THAT
//   COMMAND: the pane's environment is the server's unless `-e` overrides it.
// - **direct**: any other shell command that names the binary — `"$BIN"`,
//   `exec $BIN`, `sh -c "$BIN"`, `cargo run -p tui`, `cauldron tui` — unless
//   its command word is a known-inert one (`[`, `echo`, …) or `python3`
//   (a hand-off: the python spawn is judged where it happens). The set must be
//   `NAME=value` prefixes or `env NAME=value` on THAT command, and TMUX must be
//   dropped (`env -u TMUX`).
// - **python**: an `os.exec*`/`subprocess.*`/`Popen`/`pty.spawn` call whose
//   target is the face. The env it is handed must be assigned
//   `env["NAME"] = value` for every name BETWEEN THE PREVIOUS SPAWN AND THIS
//   ONE, and must `pop("TMUX")` there; a spawn that inherits the environment
//   (`execv`, `execvp`, no `env=`) is red outright. A spawn whose target is not
//   recognisably the face and not provably something else is red too, unless
//   it is named in `SPAWN_NOT_A_FACE` with the reason.

/// Set on every launch besides the ENDPOINT vars, with why. The chat face
/// reads HOME through `dirs::home_dir()`, which part 1 cannot see, so it is
/// named here and the source's use of it is asserted below rather than
/// assumed. (The `--cauldron` face also reads `env::var("HOME")` by name, only
/// to write paths as `~`; part 1 counts LAUNCH_ALSO as classified.)
const LAUNCH_ALSO: &[(&str, &str)] = &[
    (
        "HOME",
        "dirs::home_dir(): under it the face reads ~/.cauldron (the registry that a registry act \
         REWRITES — projects.json, read by the live ui-server) and writes ~/.tui/log + diag.log \
         beside his live face's (R7.S5-inspect finding 1)",
    ),
    (
        "CAULDRON_ISOLATED",
        "the test-only isolation marker (REL-6a2): with it =1 every connection the face makes \
         refuses his live ports and every unpointed fallback, and every child it spawns inherits \
         it (REL-6a3 rule B); =1 is the only non-live value",
    ),
];

/// Python spawns whose target is not the face, by (file, first argument), with
/// the reason. A spawn whose target this file cannot place is RED until it is
/// either recognisably the face (and isolated) or listed here.
const SPAWN_NOT_A_FACE: &[(&str, &str, &str)] = &[(
    "drag_frames.sh",
    "cmd[0]",
    "outer.py: the tmux CLIENT attached to the private server as the outer terminal; \
     the face runs in that server's pane, launched (and checked) in start_face",
)];

/// Commands that may name the binary without running it.
const INERT: &[&str] =
    &["[", "[[", "test", "echo", "printf", "stat", "ls", "file", "codesign", "shasum", "cmp", "du", "xattr", "strings"];

const TMUX_LAUNCH: &[&str] =
    &["new-session", "new", "new-window", "neww", "split-window", "splitw", "respawn-pane", "respawnp", "respawn-window", "respawnw"];

const PY_SPAWNS: &[&str] = &[
    "os.execve(", "os.execv(", "os.execvp(", "os.execvpe(", "os.execl(", "os.execle(", "os.execlp(",
    "os.execlpe(", "os.spawnv(", "os.spawnve(", "os.spawnvp(", "os.spawnvpe(", "os.spawnl(",
    "os.spawnle(", "os.posix_spawn(", "os.posix_spawnp(", "os.system(", "os.popen(", "subprocess.run(",
    "subprocess.call(", "subprocess.check_call(", "subprocess.check_output(", "subprocess.Popen(",
    "Popen(", "pty.spawn(",
];

/// Python spawns whose LAST positional argument is the environment.
const PY_ENV_LAST: &[&str] =
    &["os.execve(", "os.execvpe(", "os.execle(", "os.execlpe(", "os.spawnve(", "os.spawnvpe(", "os.spawnle("];

/// Python spawns that take `env=`.
const PY_ENV_KW: &[&str] = &[
    "os.posix_spawn(", "os.posix_spawnp(", "subprocess.run(", "subprocess.call(", "subprocess.check_call(",
    "subprocess.check_output(", "subprocess.Popen(", "Popen(",
];

/// Every `name=value` / `["name"] = value` assignment of `name` in `text`,
/// value unquoted up to whitespace, quote, `;` or `\`.
fn assignments(text: &str, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (pat, bare) in [(format!("{name}="), true), (format!("[\"{name}\"] = "), false)] {
        let mut from = 0;
        while let Some(at) = text[from..].find(&pat) {
            let start = from + at;
            from = start + pat.len();
            // `FOO_TUI_THREADS_PORT=` is not `TUI_THREADS_PORT=`.
            let prev = text[..start].chars().last();
            if bare && prev.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            let v: String = text[from..]
                .trim_start_matches(['"', '\''])
                .chars()
                .take_while(|c| !c.is_whitespace() && !"\"';\\".contains(*c))
                .collect();
            out.push(v);
        }
    }
    out
}

fn live_value(v: &str) -> bool {
    v.is_empty() || LIVE_PORTS.iter().any(|p| v.contains(&p.to_string())) || v.contains("cauldron")
}

/// A HOME value that is, or may well be, his real one.
fn live_home(v: &str) -> bool {
    v.is_empty()
        || v.contains("$HOME")
        || v.contains("${HOME")
        || v.starts_with('~')
        || v.contains("/Users/")
        || v.contains("/home/")
        || v.contains("/var/root")
        || v.contains("environ")
        || v.contains("expanduser")
        || v.contains("$USER")
}

fn is_live(var: &str, v: &str) -> bool {
    match var {
        "HOME" => live_home(v),
        // The marker is "live" (absent in effect) unless it is exactly 1.
        "CAULDRON_ISOLATED" => v.trim() != "1",
        // Only `off` stops the real model call; any other value leaves it on.
        "CAULDRON_TYPE_ONE" => v.trim() != "off",
        // REL-1.S1c (REL-1.S1b-I F-B): exactly pty_drive.py's rule — only
        // `none` is safe. (It used to accept any program not named `open`, so
        // `xdg-open` or a wrapper script passed here and was refused there.)
        "CAULDRON_OPENER" => v.trim() != "none",
        // REL-1.S1c (F-A): a scratch root; his home, `~`, or any `cauldron`
        // path (his root is `~/.cauldron`) is live.
        "CAULDRON_HOME" => live_home(v) || live_value(v),
        _ => live_value(v),
    }
}

fn required() -> Vec<&'static str> {
    ENDPOINT.iter().chain(LAUNCH_ALSO).map(|(n, _)| *n).collect()
}

/// Does `s` name the face's binary by path? `debug/tui`, `release/tui`,
/// `bin/tui` as a whole path component — quoted or not.
fn names_tui_path(s: &str) -> bool {
    for lead in ["debug/tui", "release/tui", "bin/tui"] {
        let mut from = 0;
        while let Some(at) = s[from..].find(lead) {
            let end = from + at + lead.len();
            let before_ok = from + at == 0 || !s[..from + at].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-');
            let after_ok = s[end..].chars().next().is_none_or(|c| !(c.is_ascii_alphanumeric() || "_-./".contains(c)));
            if before_ok && after_ok {
                return true;
            }
            from = end;
        }
    }
    false
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Does `word` reference one of the shell variables in `vars` (`$X`, `${X…`)?
fn refs_var(word: &str, vars: &BTreeSet<String>) -> bool {
    vars.iter().any(|v| {
        let mut from = 0;
        while let Some(at) = word[from..].find('$') {
            let i = from + at + 1;
            let rest = &word[i..];
            let name: &str = rest.strip_prefix('{').unwrap_or(rest);
            if let Some(tail) = name.strip_prefix(v.as_str()) {
                if !tail.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
                    return true;
                }
            }
            from = i;
        }
        false
    })
}

/// One logical shell line split into simple commands, each a list of words
/// with quotes removed. Enough shell for these scripts: quotes, escapes,
/// `${…}`, `$((…))`, `$(…)` (its inside is its OWN command, even inside double
/// quotes), `;` `&` `|` `(` `)` `{` `}` as separators, a `#` comment.
#[allow(unused_assignments)] // the macros reset state the last flush never reads
fn simple_commands(line: &str) -> Vec<Vec<String>> {
    let c: Vec<char> = line.chars().collect();
    let n = c.len();
    let mut cmds: Vec<Vec<String>> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut dq = false;
    // Saved (words, cur, in_word, dq) at each `$(`.
    let mut stack: Vec<(Vec<String>, String, bool, bool)> = Vec::new();
    let mut i = 0;
    // Copies a balanced `open … close` run starting at i (which is at `$`).
    let balanced = |i: usize, open: char, close: char, skip: usize| -> usize {
        let mut depth = 0i32;
        let mut j = i + skip;
        while j < n {
            if c[j] == open {
                depth += 1;
            } else if c[j] == close {
                depth -= 1;
                if depth <= 0 {
                    return j + 1;
                }
            }
            j += 1;
        }
        n
    };
    macro_rules! flush_word {
        () => {
            if in_word {
                words.push(std::mem::take(&mut cur));
                in_word = false;
            }
        };
    }
    macro_rules! flush_cmd {
        () => {
            flush_word!();
            if !words.is_empty() {
                cmds.push(std::mem::take(&mut words));
            }
        };
    }
    while i < n {
        let ch = c[i];
        if ch == '\\' && i + 1 < n {
            cur.push(c[i + 1]);
            in_word = true;
            i += 2;
            continue;
        }
        if ch == '\'' && !dq {
            in_word = true;
            let mut j = i + 1;
            while j < n && c[j] != '\'' {
                cur.push(c[j]);
                j += 1;
            }
            i = j + 1;
            continue;
        }
        if ch == '$' && i + 2 < n && c[i + 1] == '(' && c[i + 2] == '(' {
            let end = balanced(i, '(', ')', 1);
            cur.extend(&c[i..end]);
            in_word = true;
            i = end;
            continue;
        }
        if ch == '$' && i + 1 < n && c[i + 1] == '{' {
            let end = balanced(i, '{', '}', 1);
            cur.extend(&c[i..end]);
            in_word = true;
            i = end;
            continue;
        }
        if ch == '$' && i + 1 < n && c[i + 1] == '(' {
            cur.push_str("$(…)");
            in_word = true;
            stack.push((std::mem::take(&mut words), std::mem::take(&mut cur), in_word, dq));
            in_word = false;
            dq = false;
            i += 2;
            continue;
        }
        if ch == '"' {
            dq = !dq;
            in_word = true;
            i += 1;
            continue;
        }
        if dq {
            cur.push(ch);
            i += 1;
            continue;
        }
        if ch == ')' && !stack.is_empty() {
            flush_cmd!();
            let (w, s, iw, d) = stack.pop().unwrap();
            words = w;
            cur = s;
            in_word = iw;
            dq = d;
            i += 1;
            continue;
        }
        if ch == '#' && !in_word {
            break;
        }
        if ch.is_whitespace() {
            flush_word!();
        } else if ";&|(){}`".contains(ch) {
            flush_cmd!();
        } else {
            cur.push(ch);
            in_word = true;
        }
        i += 1;
    }
    flush_cmd!();
    while let Some((w, s, iw, _)) = stack.pop() {
        // An unclosed `$(` (a heredoc's opening line): the outer words stand.
        words = w;
        cur = s;
        in_word = iw;
        flush_cmd!();
    }
    cmds
}

fn split_assignment(w: &str) -> Option<(&str, &str)> {
    let (k, v) = w.split_once('=')?;
    is_ident(k).then_some((k, v))
}

#[derive(Debug)]
struct Launch {
    line: usize,
    kind: &'static str,
    set: BTreeMap<String, String>,
    /// Whether TMUX is dropped on this launch (`None`: not required — tmux).
    drops_tmux: Option<bool>,
}

#[derive(Debug, Default)]
struct Analysis {
    launches: Vec<Launch>,
    /// Lines that hand the binary to a python driver.
    handoffs: Vec<usize>,
    problems: Vec<String>,
}

/// Logical lines (continuations joined) with their first line number, and
/// whether each sits inside a heredoc body; heredoc bodies are also returned
/// whole, as python regions.
struct Lines {
    shell: Vec<(usize, String)>,
    /// (first body line number, body text)
    heredocs: Vec<(usize, String)>,
}

fn heredoc_tag(line: &str) -> Option<String> {
    let mut from = 0;
    while let Some(at) = line[from..].find("<<") {
        let i = from + at;
        from = i + 2;
        if line[i + 2..].starts_with('<') || line[..i].ends_with('<') {
            from = i + 3;
            continue;
        }
        let rest = line[i + 2..].trim_start_matches('-').trim_start();
        let rest = rest.trim_start_matches(['\'', '"']);
        let tag: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        // A tag starts with a letter: `$(( 1 << 2 ))` is not a heredoc.
        if tag.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
            return Some(tag);
        }
    }
    None
}

fn split_lines(text: &str, python_file: bool) -> Lines {
    if python_file {
        return Lines { shell: Vec::new(), heredocs: vec![(1, text.to_string())] };
    }
    let mut shell = Vec::new();
    let mut heredocs = Vec::new();
    let raw: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < raw.len() {
        let start = i + 1;
        let mut logical = String::new();
        loop {
            let l = raw[i];
            if let Some(stripped) = l.strip_suffix('\\') {
                logical.push_str(stripped);
                logical.push(' ');
                i += 1;
                if i < raw.len() {
                    continue;
                }
            } else {
                logical.push_str(l);
                i += 1;
            }
            break;
        }
        let tag = if logical.trim_start().starts_with('#') { None } else { heredoc_tag(&logical) };
        shell.push((start, logical));
        if let Some(tag) = tag {
            let body_start = i + 1;
            let mut body = String::new();
            while i < raw.len() && raw[i].trim() != tag {
                body.push_str(raw[i]);
                body.push('\n');
                i += 1;
            }
            i += 1; // the terminator
            heredocs.push((body_start, body));
        }
    }
    Lines { shell, heredocs }
}

/// The shell variables (and python names) that hold the face's binary.
fn tui_vars(lines: &Lines) -> BTreeSet<String> {
    let mut vars = BTreeSet::new();
    loop {
        let before = vars.len();
        for (_, l) in &lines.shell {
            for cmd in simple_commands(l) {
                for w in &cmd {
                    if let Some((k, v)) = split_assignment(w) {
                        if names_tui_path(v) || refs_var(v, &vars) {
                            vars.insert(k.to_string());
                        }
                    }
                }
            }
        }
        for (_, body) in &lines.heredocs {
            for l in body.lines() {
                if let Some((k, v)) = l.trim().split_once('=') {
                    let k = k.trim();
                    if is_ident(k) && !v.starts_with('=') && names_tui_path(v) {
                        vars.insert(k.to_string());
                    }
                }
            }
        }
        if vars.len() == before {
            return vars;
        }
    }
}

/// Skip `if`/`exec`/`env …`/`timeout …`/assignments to the command word.
/// Returns (index of the command word, assignments, TMUX dropped).
fn command_word(cmd: &[String]) -> (Option<usize>, BTreeMap<String, String>, bool) {
    let mut set = BTreeMap::new();
    let mut drops_tmux = false;
    let mut i = 0;
    while i < cmd.len() {
        let w = cmd[i].as_str();
        if let Some((k, v)) = split_assignment(w) {
            set.insert(k.to_string(), v.to_string());
            i += 1;
            continue;
        }
        match w {
            "if" | "then" | "else" | "elif" | "do" | "while" | "until" | "!" | "time" | "exec" | "command"
            | "nohup" | "setsid" | "builtin" | "local" | "export" | "readonly" | "sudo" => i += 1,
            "env" => {
                i += 1;
                while i < cmd.len() {
                    let a = cmd[i].as_str();
                    if a == "-u" || a == "--unset" {
                        if cmd.get(i + 1).is_some_and(|x| x == "TMUX") {
                            drops_tmux = true;
                        }
                        i += 2;
                    } else if let Some(x) = a.strip_prefix("--unset=").or_else(|| a.strip_prefix("-u")) {
                        drops_tmux |= x == "TMUX";
                        i += 1;
                    } else if a == "-i" || a == "-" || a == "--ignore-environment" {
                        // An empty environment: nothing of his reaches it,
                        // TMUX included; the set must still be given.
                        drops_tmux = true;
                        i += 1;
                    } else if let Some((k, v)) = split_assignment(a) {
                        set.insert(k.to_string(), v.to_string());
                        i += 1;
                    } else if a.starts_with('-') {
                        i += 1;
                    } else {
                        break;
                    }
                }
            }
            "timeout" | "gtimeout" => {
                i += 1;
                while i < cmd.len() && cmd[i].starts_with('-') {
                    let takes = matches!(cmd[i].as_str(), "-s" | "-k" | "--signal" | "--kill-after");
                    i += if takes { 2 } else { 1 };
                }
                i += 1; // the duration
            }
            _ => return (Some(i), set, drops_tmux),
        }
    }
    (None, set, drops_tmux)
}

/// Top-level comma-split of a python call's arguments, starting just after
/// its `(`. Returns (args, index just past the closing `)`).
fn py_args(s: &str) -> (Vec<String>, usize) {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for (i, ch) in s.char_indices() {
        if let Some(q) = quote {
            cur.push(ch);
            if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => {
                quote = Some(ch);
                cur.push(ch);
            }
            '(' | '[' | '{' => {
                depth += 1;
                cur.push(ch);
            }
            ')' | ']' | '}' if depth == 0 => {
                if !cur.trim().is_empty() {
                    args.push(cur.trim().to_string());
                }
                return (args, i + 1);
            }
            ')' | ']' | '}' => {
                depth -= 1;
                cur.push(ch);
            }
            ',' if depth == 0 => args.push(std::mem::take(&mut cur).trim().to_string()),
            _ => cur.push(ch),
        }
    }
    (args, s.len())
}

/// `env["NAME"] = value` assignments of `envname` in `block`, and whether it
/// drops TMUX there.
fn py_env(block: &str, envname: &str) -> (BTreeMap<String, String>, bool) {
    let mut set = BTreeMap::new();
    for q in ['"', '\''] {
        let pat = format!("{envname}[{q}");
        let mut from = 0;
        while let Some(at) = block[from..].find(&pat) {
            let start = from + at;
            from = start + pat.len();
            if block[..start].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
                continue;
            }
            let rest = &block[from..];
            let Some(close) = rest.find(q) else { continue };
            let name = &rest[..close];
            let after = rest[close + 1..].trim_start();
            let Some(after) = after.strip_prefix(']') else { continue };
            let after = after.trim_start();
            let Some(val) = after.strip_prefix('=') else { continue };
            if val.starts_with('=') {
                continue;
            }
            let val: String = val.chars().take_while(|c| *c != ';' && *c != '\n').collect();
            let val = val.trim().trim_matches(['"', '\'']).to_string();
            set.insert(name.to_string(), val);
        }
    }
    let drops = [format!("{envname}.pop(\"TMUX\""), format!("{envname}.pop('TMUX'"), format!("del {envname}[\"TMUX\"]")]
        .iter()
        .any(|p| block.contains(p.as_str()));
    (set, drops)
}

/// Every launch site in one script, judged.
fn analyse(name: &str, text: &str) -> Analysis {
    let python_file = name.ends_with(".py");
    let lines = split_lines(text, python_file);
    let vars = tui_vars(&lines);
    let mut a = Analysis::default();

    // ── shell commands ──
    for (ln, l) in &lines.shell {
        if l.trim_start().starts_with('#') {
            continue;
        }
        for cmd in simple_commands(l) {
            // A word that STARTS with the binary — `"$BIN"`, `"$BIN --x"`,
            // `/…/debug/tui` — runs it if anything runs that word; one that
            // merely mentions it mid-sentence (`bad "no binary at $BIN"`) is
            // a message. Red on the safe side: a message that opens with the
            // path is flagged and must be reworded.
            let refs = |w: &String| {
                let head = w.split_whitespace().next().unwrap_or("");
                (head.starts_with('$') && refs_var(head, &vars)) || names_tui_path(head)
            };
            let (ci, prefix, drops_tmux) = command_word(&cmd);
            let Some(ci) = ci else { continue };
            let word = cmd[ci].as_str();
            let base = word.rsplit('/').next().unwrap_or(word);
            let cargo_run = base == "cargo"
                && cmd.get(ci + 1).is_some_and(|w| w == "run" || w == "r")
                && cmd.windows(2).any(|p| {
                    matches!(p[0].as_str(), "-p" | "--package" | "--bin") && p[1] == "tui"
                })
                || base == "cargo"
                    && cmd.iter().any(|w| matches!(w.as_str(), "-ptui" | "--package=tui" | "--bin=tui"))
                    && cmd.get(ci + 1).is_some_and(|w| w == "run" || w == "r");
            let cauldron_tui = base == "cauldron" && cmd[ci + 1..].iter().any(|w| w == "tui");
            if !cargo_run && !cauldron_tui && !cmd.iter().any(refs) {
                continue;
            }
            if INERT.contains(&base) {
                continue;
            }
            if base.starts_with("python") {
                a.handoffs.push(*ln);
                continue;
            }
            if let Some(sub) = cmd[ci..].iter().position(|w| TMUX_LAUNCH.contains(&w.as_str())) {
                let sub = ci + sub;
                if cmd[sub + 1..].iter().any(refs) {
                    let mut set = BTreeMap::new();
                    let mut j = sub + 1;
                    while j < cmd.len() {
                        let w = cmd[j].as_str();
                        let pair = if w == "-e" {
                            j += 1;
                            cmd.get(j).map(String::as_str)
                        } else {
                            w.strip_prefix("-e").filter(|r| r.contains('='))
                        };
                        if let Some((k, v)) = pair.and_then(split_assignment) {
                            set.insert(k.to_string(), v.to_string());
                        }
                        j += 1;
                    }
                    a.launches.push(Launch { line: *ln, kind: "tmux", set, drops_tmux: None });
                    continue;
                }
            }
            a.launches.push(Launch { line: *ln, kind: "direct", set: prefix, drops_tmux: Some(drops_tmux) });
        }
    }

    // ── python spawns: heredoc bodies, whole .py files, and each shell line
    //    (a `python3 -c "…"`) ──
    let mut regions: Vec<(usize, String)> = lines.heredocs.clone();
    regions.extend(lines.shell.iter().cloned());
    for (first_line, body) in &regions {
        // Comment lines out; keep the line count so a site has its number.
        let body: String = body
            .lines()
            .map(|l| if l.trim_start().starts_with('#') { "" } else { l })
            .collect::<Vec<_>>()
            .join("\n");
        let mut sites: Vec<(usize, &str)> = Vec::new();
        for pat in PY_SPAWNS {
            let pat: &'static str = pat;
            let mut from = 0;
            while let Some(at) = body[from..].find(pat) {
                let pos = from + at;
                from = pos + pat.len();
                if pat == "Popen(" && body[..pos].ends_with("subprocess.") {
                    continue;
                }
                if body[..pos].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
                    continue;
                }
                sites.push((pos, pat));
            }
        }
        sites.sort();
        let mut block_start = 0;
        for (pos, pat) in sites {
            let (args, len) = py_args(&body[pos + pat.len()..]);
            let line = first_line + body[..pos].matches('\n').count();
            let block = &body[block_start..pos];
            block_start = pos + pat.len() + len;
            let first = args.first().cloned().unwrap_or_default();
            let target = first.trim_start_matches(['[', '(']).trim();
            let target_ident: String = target.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
            let static_target = target.starts_with(['"', '\'']);
            let is_face = if target_ident == "BIN" || vars.contains(&target_ident) {
                true
            } else if static_target {
                names_tui_path(&first) || (first.contains("\"run\"") && first.contains("\"tui\""))
            } else if SPAWN_NOT_A_FACE.iter().any(|(f, t, _)| *f == name && *t == first) {
                false
            } else {
                a.problems.push(format!(
                    "{name}:{line}: `{pat}{first}, …)` spawns something this guard cannot place: if it is \
                     the face, name it BIN and isolate it; if not, add it to SPAWN_NOT_A_FACE with the reason"
                ));
                continue;
            };
            if !is_face {
                continue;
            }
            let envname = if PY_ENV_LAST.contains(&pat) {
                args.last().cloned()
            } else if PY_ENV_KW.contains(&pat) {
                args.iter().find_map(|x| x.strip_prefix("env=").map(|e| e.trim().to_string()))
            } else {
                None
            };
            let Some(envname) = envname.filter(|e| is_ident(e)) else {
                a.problems.push(format!(
                    "{name}:{line}: `{pat}…)` starts the face with the PARENT's environment (no env it \
                     builds) — nothing below can be isolated"
                ));
                continue;
            };
            let (set, drops) = py_env(block, &envname);
            a.launches.push(Launch { line, kind: "python", set, drops_tmux: Some(drops) });
        }
    }

    if LIVE_BY_DESIGN.iter().any(|(n, _)| *n == name) {
        // Its launches are still SEEN (the stale-exemption sentinel needs
        // them); nothing about them is judged.
        a.problems.clear();
        return a;
    }
    for l in &a.launches {
        for var in required() {
            match l.set.get(var) {
                None => a.problems.push(format!(
                    "{name}:{}: a {} launch of the face does not set {var}",
                    l.line, l.kind
                )),
                Some(v) if is_live(var, v) => a.problems.push(format!(
                    "{name}:{}: a {} launch sets {var}={v:?}, a live or empty value",
                    l.line, l.kind
                )),
                _ => {}
            }
        }
        if l.drops_tmux == Some(false) {
            a.problems.push(format!(
                "{name}:{}: a {} launch keeps TMUX — the face's terminal negotiation would query his \
                 tmux server",
                l.line, l.kind
            ));
        }
    }
    a
}

#[test]
fn every_launch_of_the_face_carries_the_whole_isolation_set() {
    // HOME matters because the face reads it; if `apps/tui/src` stops calling
    // `dirs::home_dir()`, this requirement needs re-deciding, not deleting.
    let mut src = Vec::new();
    rs_files(&crate_dir().join("src"), &mut src);
    assert!(
        src.iter().any(|f| std::fs::read_to_string(f).unwrap().contains("dirs::home_dir()")),
        "apps/tui/src no longer calls dirs::home_dir(); LAUNCH_ALSO's HOME needs re-deciding"
    );

    let mut scripts: Vec<PathBuf> = std::fs::read_dir(crate_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "sh" || x == "py"))
        .collect();
    scripts.sort();

    let mut launchers: BTreeMap<String, Vec<(usize, &'static str)>> = BTreeMap::new();
    let mut handoffs = BTreeSet::new();
    let mut problems = Vec::new();
    for p in &scripts {
        let name = p.file_name().unwrap().to_str().unwrap().to_string();
        let text = std::fs::read_to_string(p).unwrap();
        if name != "pty_drive.py" && !assignments(&text, "TUI_THREADS_URL").is_empty() {
            problems.push(format!("{name} sets TUI_THREADS_URL, which nothing reads"));
        }
        let a = analyse(&name, &text);
        if !a.launches.is_empty() {
            launchers.insert(name.clone(), a.launches.iter().map(|l| (l.line, l.kind)).collect());
        }
        if !a.handoffs.is_empty() {
            handoffs.insert(name.clone());
        }
        problems.extend(a.problems);
    }

    // The verdict first, so a bad launch is reported as itself and not as a
    // changed launch count in the sentinels below.
    assert!(problems.is_empty(), "drive scripts that can reach a live system:\n  {}", problems.join("\n  "));

    // Sentinels: the detector finds exactly the launches that exist today, of
    // every kind, so an empty `problems` is not an empty search.
    let kind_of = |f: &str| launchers.get(f).map(|v| v.iter().map(|(_, k)| *k).collect::<Vec<_>>());
    for (f, want) in [
        ("click_frames.sh", vec!["tmux"]),
        ("drag_frames.sh", vec!["tmux"]),
        ("tmux_keys_probe.sh", vec!["tmux"]),
        ("live_frames.sh", vec!["tmux"]),
        ("conformance_test.sh", vec!["python"]),
        ("composer_frames.sh", vec!["python"]),
        ("pty_drive.py", vec!["python"]),
    ] {
        assert_eq!(kind_of(f), Some(want), "launch detector on {f}: {launchers:?}");
    }
    for want in [
        "agents_frames.sh",
        "composer_frames.sh",
        "conformance_test.sh",
        "hold_space_frames.sh",
        "pointer_frames.sh",
        "wheel_rate_probe.sh",
    ] {
        assert!(handoffs.contains(want), "hand-off detector missed {want}: {handoffs:?}");
    }
    for (n, why) in LIVE_BY_DESIGN {
        assert!(launchers.contains_key(*n), "stale exemption {n} ({why}): it no longer starts the face");
    }
}

/// The detector, on the shapes that slipped past R7.S5's per-file check, and
/// on their isolated twins — so a detector that flags everything, or nothing,
/// is red here (doctrine 2).
#[test]
fn the_launch_detector_judges_each_launch_on_its_own_words() {
    const ISO_E: &str = "-e \"HOME=$WORK/home\" -e TUI_THREADS_CHASSIS_PORT=1 -e TUI_THREADS_PORT=1 \
                         -e \"AUI_V2_URL=ws://127.0.0.1:1/\" -e CAULDRON_BIN=/usr/bin/false \
                         -e CAULDRON_OPENER=none -e CAULDRON_TYPE_ONE=off -e \"CAULDRON_HOME=$WORK/chome\" -e CAULDRON_ISOLATED=1";
    const ISO_ENV: &str = "env -u TMUX HOME=\"$WORK/home\" TUI_THREADS_CHASSIS_PORT=1 TUI_THREADS_PORT=1 \
                           AUI_V2_URL=ws://127.0.0.1:1/ CAULDRON_BIN=/usr/bin/false \
                           CAULDRON_OPENER=none CAULDRON_TYPE_ONE=off CAULDRON_HOME=\"$WORK/chome\" CAULDRON_ISOLATED=1";
    let bin = "BIN=\"${CARGO_TARGET_DIR}/debug/tui\"\n";
    let py_ok = "import os, pty\nBIN = sys.argv[1]\npid, fd = pty.fork()\nif pid == 0:\n    env = dict(os.environ)\n    \
                 env[\"HOME\"] = H\n    env[\"TUI_THREADS_CHASSIS_PORT\"] = \"1\"; env[\"TUI_THREADS_PORT\"] = \"1\"\n    \
                 env[\"AUI_V2_URL\"] = \"ws://127.0.0.1:1/\"; env[\"CAULDRON_BIN\"] = \"/usr/bin/false\"\n    \
                 env[\"CAULDRON_OPENER\"] = \"none\"; env[\"CAULDRON_TYPE_ONE\"] = \"off\"\n    \
                 env[\"CAULDRON_HOME\"] = os.path.join(H, \"chome\")\n    \
                 env[\"CAULDRON_ISOLATED\"] = \"1\"\n    \
                 env.pop(\"TMUX\", None)\n    os.execve(BIN, [BIN], env)\n";
    // A launch with NOTHING set misses every required var; a direct or python
    // one also keeps TMUX. Pinned at 8 so a shrunk set cannot pass quietly
    // (4 ENDPOINT before REL-1.S1b + 2 `--cauldron` levers + REL-1.S1c's
    // CAULDRON_HOME + HOME + REL-6a2's CAULDRON_ISOLATED marker).
    let r = required().len();
    assert_eq!(r, 9, "required set changed: {:?}", required());
    let cases: Vec<(&str, String, usize)> = vec![
        // ── isolated: no problems, and the launch IS seen (count below) ──
        ("ok tmux", format!("{bin}$S new-session -d -s p {ISO_E} \"$BIN\"\n"), 0),
        ("ok direct", format!("{bin}{ISO_ENV} \"$BIN\" --headless\n"), 0),
        ("ok python", py_ok.to_string(), 0),
        ("ok python handoff", format!("{bin}python3 \"$DRIVE_PY\" 100 24 \"$BIN\" \"$H\" '[]'\n"), 0),
        ("ok inert", format!("{bin}[ -x \"$BIN\" ] || {{ echo \"no binary at $BIN\"; exit 1; }}\n"), 0),
        // ── G1: a second, bare launch in an isolated file ──
        ("G1", format!("{bin}$S new-session -d -s p {ISO_E} \"$BIN\"\n$S new-session -d -s naked \"$BIN\"\n"), r),
        // ── G3: mentions the driver, starts the face itself ──
        ("G3", format!("{bin}DRIVE_PY=\"pty_drive.py\"\n\"$BIN\" --headless\n"), r + 1),
        // ── G4: the path in other spellings ──
        ("G4 unquoted exec", "BIN=/x/target/debug/tui\nexec $BIN\n".to_string(), r + 1),
        ("G4 release", "F=\"$ROOT/target/release/tui\"\n\"$F\"\n".to_string(), r + 1),
        ("G4 cargo run", "cargo run -p tui -- --headless\n".to_string(), r + 1),
        ("G4 alias", format!("{bin}FACE=\"$BIN\"\n$S new-session -d \"$FACE --x\"\n"), r),
        // ── HOME ──
        ("no HOME", format!("{bin}$S new-session -d -e TUI_THREADS_CHASSIS_PORT=1 -e TUI_THREADS_PORT=1 \
                             -e AUI_V2_URL=ws://127.0.0.1:1/ -e CAULDRON_BIN=/usr/bin/false \
                             -e CAULDRON_OPENER=none -e CAULDRON_TYPE_ONE=off \
                             -e CAULDRON_HOME=/tmp/w/chome -e CAULDRON_ISOLATED=1 \"$BIN\"\n"), 1),
        ("real HOME", format!("{bin}$S new-session -d -e \"HOME=$HOME\" -e TUI_THREADS_CHASSIS_PORT=1 \
                               -e TUI_THREADS_PORT=1 -e AUI_V2_URL=ws://127.0.0.1:1/ \
                               -e CAULDRON_BIN=/usr/bin/false -e CAULDRON_OPENER=none \
                               -e CAULDRON_TYPE_ONE=off -e CAULDRON_HOME=/tmp/w/chome -e CAULDRON_ISOLATED=1 \"$BIN\"\n"), 1),
        // ── REL-1.S1b: the --cauldron levers, live (one each) and missing ──
        ("live opener", format!("{bin}$S new-session -d -s p {} \"$BIN\"\n", ISO_E.replace("CAULDRON_OPENER=none", "CAULDRON_OPENER=/usr/bin/open")), 1),
        ("live type one", format!("{bin}$S new-session -d -s p {} \"$BIN\"\n", ISO_E.replace("CAULDRON_TYPE_ONE=off", "CAULDRON_TYPE_ONE=on")), 1),
        // ── REL-1.S1c F-B: the opener rule is pty_drive's — only `none` ──
        ("live opener wrapper", format!("{bin}$S new-session -d -s p {} \"$BIN\"\n", ISO_E.replace("CAULDRON_OPENER=none", "CAULDRON_OPENER=/usr/bin/xdg-open")), 1),
        // ── REL-1.S1c F-A: CAULDRON_HOME, his root and missing ──
        ("live cauldron home", format!("{bin}$S new-session -d -s p {} \"$BIN\"\n", ISO_E.replace("CAULDRON_HOME=$WORK/chome", "CAULDRON_HOME=$HOME/.cauldron")), 1),
        ("no cauldron home", format!("{bin}$S new-session -d -s p {} \"$BIN\"\n", ISO_E.replace(" -e \"CAULDRON_HOME=$WORK/chome\"", "")), 1),
        ("no cauldron levers", format!("{bin}$S new-session -d -s p {} \"$BIN\"\n", ISO_E.replace(" -e CAULDRON_OPENER=none -e CAULDRON_TYPE_ONE=off", "")), 2),
        // ── python: TMUX kept; a second spawn reusing the first's env; inherits; unplaceable ──
        ("py TMUX", py_ok.replace("    env.pop(\"TMUX\", None)\n", ""), 1),
        ("py second", format!("{py_ok}    os.execve(BIN, [BIN], env)\n"), r + 1),
        ("py inherits", "BIN = sys.argv[1]\nos.execv(BIN, [BIN])\n".to_string(), 1),
        ("py unplaced", "FACE = sys.argv[1]\nos.execve(FACE, [FACE], env)\n".to_string(), 1),
        // ── REL-6a2: the isolation marker, absent or not 1 ──
        ("no marker tmux", format!("{bin}$S new-session -d -s p {} \"$BIN\"\n", ISO_E.replace(" -e CAULDRON_ISOLATED=1", "")), 1),
        ("marker 0 direct", format!("{bin}{} \"$BIN\" --headless\n", ISO_ENV.replace("CAULDRON_ISOLATED=1", "CAULDRON_ISOLATED=0")), 1),
        ("py no marker", py_ok.replace("    env[\"CAULDRON_ISOLATED\"] = \"1\"\n", ""), 1),
        // ── direct with the set but TMUX kept ──
        ("direct TMUX", format!("{bin}{} \"$BIN\"\n", ISO_ENV.replace("env -u TMUX ", "env ")), 1),
    ];
    for (label, text, want) in &cases {
        let file = if label.starts_with("py") || *label == "ok python" { "zz.py" } else { "zz.sh" };
        let a = analyse(file, text);
        assert_eq!(a.problems.len(), *want, "{label}: {:#?}\n{text}", a.problems);
        if label.starts_with("ok") && !label.contains("handoff") && !label.contains("inert") {
            assert_eq!(a.launches.len(), 1, "{label}: the launch was not SEEN: {a:#?}");
        }
    }
    assert_eq!(analyse("zz.sh", &format!("{bin}python3 \"$DRIVE_PY\" \"$BIN\"\n")).handoffs.len(), 1);
}

// ── part 3: pty_drive.py, run ───────────────────────────────────────────────

/// A hostile parent: every endpoint var pointed at production.
const HOSTILE: &[(&str, &str)] = &[
    ("TUI_THREADS_CHASSIS_PORT", "3653"),
    ("TUI_THREADS_PORT", "3653"),
    ("AUI_V2_URL", "ws://127.0.0.1:9010/"),
    ("CAULDRON_BIN", "/Users/nobody/.cauldron/bin/cauldron"),
    ("TUI_THREADS_URL", "ws://127.0.0.1:1/ws"),
    ("CAULDRON_OPENER", "/usr/bin/open"),
    ("CAULDRON_TYPE_ONE", "on"),
    ("CAULDRON_HOME", "/Users/nobody/.cauldron"),
];

struct Run {
    rc: Option<i32>,
    stderr: String,
    child_env: Option<BTreeMap<String, String>>,
}

fn drive(url: Option<&str>) -> Run {
    drive_home(url, None)
}

/// `drive`, with the HOME argument overridden (`None`: the scratch dir).
fn drive_home(url: Option<&str>, home: Option<&Path>) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let dump = dir.path().join("env.txt");
    let stub = dir.path().join("face");
    std::fs::write(
        &stub,
        format!("#!/bin/sh\nenv > '{}'\necho painted\n", dump.display()),
    )
    .unwrap();
    Command::new("chmod").arg("+x").arg(&stub).status().unwrap();
    let mut c = Command::new("python3");
    c.arg(crate_dir().join("pty_drive.py"))
        .args(["40", "10"])
        .arg(&stub)
        .arg(home.unwrap_or(dir.path()))
        .arg("[]");
    if let Some(u) = url {
        c.arg(u);
    }
    for (k, v) in HOSTILE {
        c.env(k, v);
    }
    let o = c.output().expect("spawn python3");
    let child_env = std::fs::read_to_string(&dump).ok().map(|t| {
        t.lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    });
    Run { rc: o.status.code(), stderr: String::from_utf8_lossy(&o.stderr).into(), child_env }
}

#[test]
fn pty_drive_overrides_a_hostile_parent_and_the_face_sees_no_live_endpoint() {
    let r = drive(None);
    assert_eq!(r.rc, Some(0), "stderr: {}", r.stderr);
    let env = r.child_env.expect("the stub face never ran");
    assert_eq!(env.get("TUI_THREADS_CHASSIS_PORT").map(String::as_str), Some("1"));
    assert_eq!(env.get("TUI_THREADS_PORT").map(String::as_str), Some("1"));
    assert_eq!(env.get("AUI_V2_URL").map(String::as_str), Some("ws://127.0.0.1:1/"));
    assert_eq!(env.get("CAULDRON_BIN").map(String::as_str), Some("/usr/bin/false"));
    assert_eq!(env.get("CAULDRON_OPENER").map(String::as_str), Some("none"));
    assert_eq!(env.get("CAULDRON_TYPE_ONE").map(String::as_str), Some("off"));
    // REL-1.S1c F-A: a scratch root under the scratch HOME, not the parent's.
    let home = env.get("HOME").expect("HOME reached the face").clone();
    assert_eq!(env.get("CAULDRON_HOME").cloned(), Some(format!("{home}/chome")), "CAULDRON_HOME: {env:?}");
    assert!(!env.contains_key("TUI_THREADS_URL"), "the retired variable leaked through");
    assert_eq!(env.get("CAULDRON_ISOLATED").map(String::as_str), Some("1"), "the face was driven without the isolation marker");

    // A fake daemon's URL reaches the face as its PORT, both halves.
    let r = drive(Some("ws://127.0.0.1:8823/ws"));
    assert_eq!(r.rc, Some(0), "stderr: {}", r.stderr);
    let env = r.child_env.expect("the stub face never ran");
    assert_eq!(env.get("TUI_THREADS_CHASSIS_PORT").map(String::as_str), Some("8823"));
    assert_eq!(env.get("TUI_THREADS_PORT").map(String::as_str), Some("8823"));
}

#[test]
fn pty_drive_refuses_a_live_port_or_a_url_the_face_would_not_honour_before_the_face_starts() {
    for url in ["ws://127.0.0.1:3653/ws", "ws://127.0.0.1:3753/ws", "ws://127.0.0.1:9010/ws"] {
        let r = drive(Some(url));
        assert_eq!(r.rc, Some(3), "{url} must be refused with exit 3; stderr: {}", r.stderr);
        assert!(r.stderr.contains("REFUSED"), "{url}: {}", r.stderr);
        assert!(r.child_env.is_none(), "{url}: the face STARTED before the refusal");
    }
    // Host/path the face would silently ignore: refused too (any non-zero).
    for url in ["ws://192.0.2.1:5000/ws", "ws://127.0.0.1:5000/other"] {
        let r = drive(Some(url));
        assert_ne!(r.rc, Some(0), "{url} must be refused; stderr: {}", r.stderr);
        assert!(r.child_env.is_none(), "{url}: the face STARTED");
    }
}

/// R7.S5b: HOME is part of the set, and the driver's own refusal covers it —
/// his home by its plain name, and by a spelling that only `realpath` sees
/// through. The opposed case is every other test here: a scratch HOME runs.
#[test]
fn pty_drive_refuses_his_real_home_before_the_face_starts() {
    let real = PathBuf::from(std::env::var("HOME").expect("cargo test runs with a HOME"));
    for home in [real.clone(), real.join(".")] {
        let r = drive_home(None, Some(&home));
        assert_eq!(r.rc, Some(3), "HOME={} must be refused with exit 3; stderr: {}", home.display(), r.stderr);
        assert!(r.stderr.contains("REFUSED") && r.stderr.contains("HOME"), "{}", r.stderr);
        assert!(r.child_env.is_none(), "HOME={}: the face STARTED", home.display());
    }
    let r = drive(None);
    assert_eq!(r.rc, Some(0), "a scratch HOME must run; stderr: {}", r.stderr);
    let env = r.child_env.expect("the stub face never ran");
    assert_ne!(env.get("HOME").map(PathBuf::from), Some(real), "the face saw his real HOME");
}

/// REL-1.S1c (REL-1.S1b-I F-A): CAULDRON_HOME is the `--cauldron` face's store
/// root, and pty_drive.py sets it to `<HOME>/chome`. A HOME that is not his
/// home but lies INSIDE it passes the HOME refusal (which refuses his home
/// itself) and must then be refused by the CAULDRON_HOME one — that is the
/// reachable case for the new arm. The opposed case is `drive(None)`: a
/// scratch HOME runs, with CAULDRON_HOME under it (asserted in the hostile
/// parent test).
#[test]
fn pty_drive_refuses_a_cauldron_home_inside_his_real_home_before_the_face_starts() {
    let real = PathBuf::from(std::env::var("HOME").expect("cargo test runs with a HOME"));
    let inside = real.join("rel-1-s1c-not-a-scratch-dir");
    assert!(!inside.exists(), "sentinel: {} must not exist (nothing is created there)", inside.display());
    let r = drive_home(None, Some(&inside));
    assert_eq!(r.rc, Some(3), "must be refused with exit 3; stderr: {}", r.stderr);
    assert!(r.stderr.contains("REFUSED") && r.stderr.contains("CAULDRON_HOME="), "{}", r.stderr);
    assert!(r.child_env.is_none(), "the face STARTED");
    assert!(!inside.exists(), "the refusal created {}", inside.display());
    let ok = drive(None);
    assert_eq!(ok.rc, Some(0), "a scratch HOME must run; stderr: {}", ok.stderr);
}

// ── part 4 (REL-1.S1c): Rust tests start the face only through `face::Face` ──
//
// Parts 2 and 3 judge shell and python launches. A Rust integration test that
// spawned the built binary was judged by nothing (REL-1.S1b report §7). Now
// `tests/it/face.rs` is the only file under `tests/` allowed to name the
// binary, and its `Face` forces the isolation set at every spawn.

/// Files under `tests/` that may name the binary, with why.
const FACE_SPAWN_ALLOWED: &[(&str, &str)] = &[
    ("it/face.rs", "the helper itself: its `BIN` is the one name"),
    (
        "it/drive_isolation.rs",
        "this guard: its detector data and fixtures spell the binary's paths; it spawns only \
         pty_drive.py against a stub (part 3). Checked separately below for the one Rust spelling",
    ),
];

/// Every place in one Rust test file that names the face's binary, outside
/// comments: `CARGO_BIN_EXE_` (cargo's handle to any built bin of this
/// crate), a path ending `debug/tui` / `release/tui` / `bin/tui`, or
/// `Command::new("tui")` (which runs whatever `tui` is on PATH — his install).
pub(crate) fn face_spawn_sites(text: &str) -> Vec<(usize, String)> {
    let exe = concat!("CARGO_BIN_", "EXE_");
    let bare = concat!("Command::new(\"", "tui\")");
    // REL-6a3: the same binary installed as `cauldron` (his
    // `~/.cauldron/bin/cauldron`, `~/bin/cauldron`) — by PATH or by path.
    let bare_cauldron = concat!("Command::new(\"", "cauldron\")");
    let cauldron_path = concat!("/bin/", "cauldron\"");
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let t = line.trim_start();
        if t.starts_with("//") {
            continue;
        }
        if line.contains(exe) || line.contains(bare) || line.contains(bare_cauldron) || line.contains(cauldron_path) || names_tui_path(line) {
            out.push((i + 1, t.chars().take(120).collect()));
        }
    }
    out
}

#[test]
fn every_rust_test_starts_the_face_through_the_helper() {
    // The detector, opposed (doctrine 2): it fires on each spelling of a raw
    // spawn and not on a comment or on the helper's use.
    let exe = concat!("CARGO_BIN_", "EXE_tui");
    for (label, text, want) in [
        ("raw exe", format!("let c = Command::new(env!(\"{exe}\"));\n"), 1),
        ("path", "let c = Command::new(\"target/debug/tui\");\n".to_string(), 1),
        ("PATH tui", format!("let c = {}tui\");\n", "Command::new(\""), 1),
        ("PATH cauldron", format!("let c = {}cauldron\");\n", "Command::new(\""), 1),
        ("his cauldron", format!("let c = Command::new(\"/Users/x/.cauldron{}\");\n", "/bin/cauldron"), 1),
        ("a cauldron dir", "let p = home.join(\".cauldron\");\n".to_string(), 0),
        ("comment", format!("// was Command::new(env!(\"{exe}\"))\n/// target/debug/tui\n"), 0),
        ("helper", "let c = crate::face::Face::new(&dir);\n".to_string(), 0),
    ] {
        assert_eq!(face_spawn_sites(&text).len(), want, "{label}: {text}");
    }

    let tests = crate_dir().join("tests");
    let mut files = Vec::new();
    rs_files(&tests, &mut files);
    files.sort();
    let mut offenders = Vec::new();
    let mut helper_sites = 0;
    let mut users = BTreeSet::new();
    for f in &files {
        let rel = f.strip_prefix(&tests).unwrap().display().to_string();
        let text = std::fs::read_to_string(f).unwrap();
        if text.contains("face::Face") || text.contains("use crate::face::") {
            users.insert(rel.clone());
        }
        let sites = face_spawn_sites(&text);
        if rel == "it/face.rs" {
            helper_sites = sites.len();
            continue;
        }
        if rel == "it/drive_isolation.rs" {
            assert!(
                !text.contains(concat!("env!(\"CARGO_BIN_", "EXE_")),
                "drive_isolation.rs is exempt for its path DATA, not for running the binary"
            );
            continue;
        }
        offenders.extend(sites.into_iter().map(|(l, s)| format!("tests/{rel}:{l}: {s}")));
    }
    assert!(
        offenders.is_empty(),
        "these tests name the face's binary outside tests/it/face.rs; start it with \
         `crate::face::Face` so the isolation set is forced:\n  {}",
        offenders.join("\n  ")
    );
    for (f, why) in FACE_SPAWN_ALLOWED {
        assert!(files.iter().any(|p| p.ends_with(f)), "stale allowance {f} ({why})");
    }
    // Sentinels: the scan reached the helper and found its one name, so an
    // empty `offenders` is not an empty search; and the converted spawners
    // still use it (each was a raw spawn before REL-1.S1c, or, for the four
    // REL-3.S2 files, before MERGE-9b; for V1.2's first_run_v1, before MERGE-V1e).
    assert_eq!(helper_sites, 1, "the scan did not find the helper's own `BIN` in tests/it/face.rs");
    for want in [
        "it/build_stamp.rs",
        "it/feedback_audit.rs",
        "it/first_run_v1.rs",
        "it/opener_guard_0327.rs",
        "it/reaudit7_0256.rs",
        "it/real_data_rel3s2.rs",
        "it/stale_links_rel3s2c.rs",
        "it/stale_refs_rel3s2d.rs",
        "it/stale_state_rel3s2b.rs",
    ] {
        assert!(users.contains(want), "{want} no longer starts the face through the helper: {users:?}");
    }
}

/// The helper's own judgment, opposed: its defaults pass, and each live or
/// missing value it exists to stop is refused BEFORE a process exists. And
/// its required names are this file's required set, so the two cannot drift.
#[test]
fn the_face_helper_forces_the_isolation_set_and_refuses_a_live_override() {
    use crate::face::{names, Face, MARKER};
    // Equal up to REL-6a2's marker, which face.rs always requires and this
    // file requires only where the source reads it (so the two branches merge
    // without this assertion flipping either way).
    let ours: BTreeSet<&str> = required().into_iter().filter(|n| *n != MARKER).collect();
    let its: BTreeSet<&str> = names().into_iter().filter(|n| *n != MARKER).collect();
    assert_eq!(its, ours, "face.rs's isolation names and drive_isolation's required set disagree");
    assert!(names().contains(&MARKER), "face.rs no longer requires {MARKER}=1");

    let dir = tempfile::tempdir().unwrap();
    let real = std::env::var("HOME").expect("cargo test runs with a HOME");
    let ok = Face::new(dir.path());
    assert_eq!(ok.check(), Ok(()), "the defaults must pass");
    let recorder = dir.path().join("cauldron").join("rec.sh");
    let fake_daemon = || {
        let mut f = Face::new(dir.path());
        f.env("TUI_THREADS_CHASSIS_PORT", "8823").env("TUI_THREADS_PORT", "8823");
        f
    };
    assert_eq!(fake_daemon().check(), Ok(()), "a fake daemon's port must pass");
    let mut opener = Face::new(dir.path());
    opener.env("CAULDRON_OPENER", &recorder);
    assert_eq!(opener.check(), Ok(()), "a recorder in its own scratch must pass");
    let mut offline = Face::new(dir.path());
    offline.arg("--build-stamp").unset_for_offline_verb("TUI_THREADS_CHASSIS_PORT");
    assert_eq!(offline.check(), Ok(()), "an offline verb may run with a lever unset");

    let bad: Vec<(&str, Box<dyn Fn(&mut Face)>)> = vec![
        ("prod chassis", Box::new(|f: &mut Face| { f.env("TUI_THREADS_CHASSIS_PORT", "3653"); })),
        ("prod health", Box::new(|f: &mut Face| { f.env("TUI_THREADS_PORT", "3651"); })),
        ("live aui", Box::new(|f: &mut Face| { f.env("AUI_V2_URL", "ws://127.0.0.1:9010/"); })),
        ("cauldron bin", Box::new(|f: &mut Face| { f.env("CAULDRON_BIN", "/Users/x/.cauldron/bin/cauldron"); })),
        ("system opener", Box::new(|f: &mut Face| { f.env("CAULDRON_OPENER", "/usr/bin/open"); })),
        ("type one on", Box::new(|f: &mut Face| { f.env("CAULDRON_TYPE_ONE", "on"); })),
        ("marker off", Box::new(|f: &mut Face| { f.env(MARKER, "0"); })),
        ("his home", { let r = real.clone(); Box::new(move |f: &mut Face| { f.env("HOME", &r); }) }),
        ("his store", { let r = format!("{real}/.cauldron"); Box::new(move |f: &mut Face| { f.env("CAULDRON_HOME", &r); }) }),
        ("an ancestor of his home", Box::new(|f: &mut Face| { f.env("CAULDRON_HOME", "/"); })),
        ("TMUX", Box::new(|f: &mut Face| { f.env("TMUX", "/tmp/tmux-501/default,1,0"); })),
        ("unset, not offline", Box::new(|f: &mut Face| { f.arg("--cauldron").unset_for_offline_verb("CAULDRON_HOME"); })),
    ];
    for (label, apply) in &bad {
        let mut f = Face::new(dir.path());
        apply(&mut f);
        assert!(f.check().is_err(), "{label}: the helper let it through");
    }
    // And the refusal is at spawn: a live override never starts a process.
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut f = Face::new(dir.path());
        f.env("TUI_THREADS_CHASSIS_PORT", "3653").arg("--build-stamp");
        f.output()
    }));
    assert!(r.is_err(), "Face::output() ran with a live chassis port");
}

/// **REL-6a3 RULE B, the census: every test binary of this crate exports the
/// marker to its children.** Each `[[test]]` target's root file (read from
/// `Cargo.toml`, so a new target is in scope the day it is added) must invoke
/// `test_isolation::install!()` (testkit/test-isolation) outside a comment. Opposed: the detector
/// fires on the invocation and not on a comment naming it; and the census
/// found the four targets this crate has, so an empty miss list is not an
/// empty search.
#[test]
fn rel6a3_every_test_binary_exports_the_isolation_marker_to_its_children() {
    let call = concat!("test_isolation::", "install!()");
    let invokes = |text: &str| text.lines().any(|l| !l.trim_start().starts_with("//") && l.contains(call));
    assert!(invokes(&format!("{call};\n")));
    assert!(!invokes(&format!("// {call};\n/// {call}\n")));
    let manifest = std::fs::read_to_string(crate_dir().join("Cargo.toml")).unwrap();
    let mut roots = Vec::new();
    let mut in_test = false;
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_test = t == "[[test]]";
        } else if in_test {
            if let Some(v) = t.strip_prefix("path").map(|r| r.trim_start()).and_then(|r| r.strip_prefix('=')) {
                roots.push(v.trim().trim_matches('"').to_string());
            }
        }
    }
    roots.sort();
    assert_eq!(
        roots,
        ["tests/it/main.rs", "tests/mic.rs", "tests/pool_daemon.rs", "tests/pool_setlive.rs"],
        "the [[test]] census changed; check each new root below"
    );
    let missing: Vec<&String> = roots
        .iter()
        .filter(|r| !invokes(&std::fs::read_to_string(crate_dir().join(r)).unwrap()))
        .collect();
    assert!(missing.is_empty(), "test binaries whose children would not inherit CAULDRON_ISOLATED=1: {missing:?}");
}
