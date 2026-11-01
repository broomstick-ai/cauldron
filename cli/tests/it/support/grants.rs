//! **CLI-ISO: THE GRANTS A TEST HANDS THE ISOLATED `cauldron` IT RUNS.**
//!
//! Since REL-6a8 an isolated process dials a loopback listener only when it
//! bound it itself, when one of its own DESCENDANTS holds it, or when its
//! harness hands it the listener in `CAULDRON_ISOLATED_PORTS`
//! (`<addr>:<port>@<pid>`). A `cauldron status` / `stop` / `restart` the test
//! runs is a SIBLING of the `cauldron daemon` the test spawned and of the
//! services that daemon launched, and a service a `cauldron start` forked
//! outlives that verb (its parent is then launchd). Neither is the verb's
//! descendant, so without a grant every seam and probe dial of the verb is
//! refused: "its seam does not answer", `stale:no-listener`, probe timeouts.
//!
//! [`owned`] is the harness's grant, and it grants ONLY what the test owns:
//! a TCP listener held by a process that is either one of `pids` (a child the
//! test spawned, e.g. its daemon) or a descendant of one, or a process whose
//! command line runs from under the test's own scratch `root` (a sandbox dir
//! unique to the test: the daemon's own launch path, a fake service's build
//! copy). Each grant names its holder's pid, read from the kernel (`lsof`)
//! at the moment the verb is spawned, never from a seam table or a
//! declaration. The gate still re-checks every dial against the kernel: a
//! grant does not admit a port once a stranger holds it, and nothing here
//! can name the user's own daemons, which neither descend from the test nor run from
//! its scratch root.

#![allow(dead_code)]

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::process::{Command, Stdio};

/// The variable the gate reads (`env::egress::ALLOW_ENV`).
pub const ALLOW_ENV: &str = env::egress::ALLOW_ENV;

/// The pids the test owns: `pids`, their descendants, and every process
/// whose command line names the scratch `root`.
pub fn owned_pids(root: &Path, pids: &[u32]) -> BTreeSet<u32> {
    let out = Command::new("/bin/ps")
        .args(["-A", "-o", "pid=,ppid=,args="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("ps");
    let text = String::from_utf8_lossy(&out.stdout);
    let root = root.display().to_string();
    // `/var/…` and `/private/var/…` are the same scratch dir.
    let root_alt = root.strip_prefix("/private").map(str::to_string).unwrap_or_else(|| format!("/private{root}"));
    let mut parent = HashMap::new();
    let mut own: BTreeSet<u32> = pids.iter().copied().filter(|p| *p > 1).collect();
    for l in text.lines() {
        let mut it = l.split_whitespace();
        let (Some(p), Some(pp)) = (it.next().and_then(|x| x.parse::<u32>().ok()), it.next().and_then(|x| x.parse::<u32>().ok())) else {
            continue;
        };
        parent.insert(p, pp);
        let args: Vec<&str> = it.collect();
        let args = args.join(" ");
        if p > 1 && (args.contains(&root) || args.contains(&root_alt)) {
            own.insert(p);
        }
    }
    // Close over descendants.
    loop {
        let more: Vec<u32> =
            parent.iter().filter(|(p, pp)| own.contains(pp) && !own.contains(p)).map(|(p, _)| *p).collect();
        if more.is_empty() {
            break;
        }
        own.extend(more);
    }
    own
}

/// **THE `CAULDRON_ISOLATED_PORTS` VALUE FOR WHAT THE TEST OWNS**: every TCP
/// listener held by an [`owned_pids`] process, as `<addr>:<port>@<pid>`.
pub fn owned(root: &Path, pids: &[u32]) -> String {
    let own = owned_pids(root, pids);
    if own.is_empty() {
        return String::new();
    }
    let list = own.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let out = Command::new("/usr/sbin/lsof")
        .args(["-nP", "-a", "-iTCP", "-sTCP:LISTEN", "-p", &list, "-Fptn"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("lsof");
    let text = String::from_utf8_lossy(&out.stdout);
    let mut grants = BTreeSet::new();
    let (mut pid, mut v6) = (None::<u32>, false);
    for line in text.lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.parse().ok();
        } else if let Some(t) = line.strip_prefix('t') {
            v6 = t == "IPv6";
        } else if let Some(n) = line.strip_prefix('n') {
            let (Some(pid), Some((host, port))) = (pid, n.rsplit_once(':')) else { continue };
            if !own.contains(&pid) {
                continue;
            }
            let host = match host {
                "*" if v6 => "[::]".to_string(),
                "*" => "0.0.0.0".to_string(),
                h => h.to_string(),
            };
            grants.insert(format!("{host}:{port}@{pid}"));
        }
    }
    grants.into_iter().collect::<Vec<_>>().join(",")
}
