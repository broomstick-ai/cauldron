//! **THE EGRESS CHOKE POINT (REL-6a5).** Every outbound dial and every
//! daemon-control spawn that the face, the `cauldron` CLI or a library they
//! link can make is decided HERE, by one of three functions:
//!
//! - [`admit_url`] — a network dial (WebSocket, HTTP, a raw TCP socket written
//!   as `tcp://host:port`). The URL is PARSED with a real URL parser
//!   (`url::Url`), so a query, a fragment, userinfo, an IPv6 literal, a
//!   missing port (the scheme's default), `localhost` and `0.0.0.0` are each
//!   read as the dialler reads them — the hand-split `url_port` this replaces
//!   returned `None` for `ws://127.0.0.1:9010?x` and so let it through
//!   (REL-6a3-I B2).
//! - [`admit_external_api`] — a paid or third-party API (OpenRouter,
//!   Anthropic, any https host), directly or through a child that calls one
//!   (`type-one`). Refused UNCONDITIONALLY under isolation (REL-6a3-I B1).
//! - [`admit_daemon_control`] — a spawn that starts, stops or restarts a
//!   daemon (`cauldron start|stop|restart …`, `launchctl …`). Refused under
//!   isolation unless it is pointed explicitly at a scratch environment
//!   (REL-6a3-I F1).
//!
//! **Under isolation a dial is an ALLOWLIST of LISTENERS, not of ports
//! (REL-6a6, REL-6a8).** It is admitted only to a LOOPBACK host (`127.0.0.0/8`,
//! `::1`, `localhost`), on an ADDRESS AND PORT that was granted by a bind,
//! and only while every process listening on that port belongs to this
//! isolated process tree. A grant comes from exactly three places, and never
//! from a file:
//!
//! - [`grant_addr`] — a listener THIS process bound, recorded as
//!   `(address, port, our pid)` at bind time; every test fixture that binds a
//!   scratch listener grants it;
//! - [`ALLOW_ENV`] (`CAULDRON_ISOLATED_PORTS=<addr>:<port>[@<pid>],…`), set by
//!   the isolated harness that bound those listeners (the face helper, the
//!   drive scripts) and naming the pid that holds each one; a bare `<port>`
//!   means `127.0.0.1:<port>` held by a process of this tree;
//! - a listener held, at the moment of the dial, by a DESCENDANT of this
//!   process (the CLI dialling the service it spawned) — observed with
//!   `lsof`, never read from a seam table or a declaration.
//!
//! **REL-6a6's root grant is gone (REL-6a6-I B2).** It read every loopback
//! port written in any `.json` / `.toml` under a root it judged "scratch", and
//! a root under `/private/tmp` therefore granted his live supervisor, tts,
//! soup, events and ui-server ports, copied there by other agents.
//!
//! **A grant dies with its listener (REL-6a6-I F1), is an address and not a
//! port (F2), and is checked against the kernel (F3).** At every dial the
//! listeners on the port are read with `lsof` (which dials nothing), and the
//! dial is refused if ANY of them — on any address — is held by a process
//! outside this isolated tree ([`ProcTree`]): his daemon that took a freed
//! ephemeral port, his `[::1]` listener beside our `127.0.0.1` one, a port
//! somebody listed by mistake. [`NEVER_PORTS`] are refused even when granted.
//! [`DEAD_PORT`] (1, privileged, so no daemon of his can bind it) is the one
//! port admitted without a grant or a lookup. Anything this module cannot read
//! is refused: the default is closed. **Not isolated** (his interactive face,
//! his CLI) every function admits — this module never changes what he can do.
//!
//! **The root check compares IDENTITY, not text (REL-6a6-I B1).**
//! [`scratch_root_refusal`] refuses a root that IS or is INSIDE his real
//! `~/.cauldron` or his login home, decided by `(dev, ino)` of the root's
//! physical ancestor chain (walked with the kernel's `..`, not by string), so
//! the APFS firmlink `/System/Volumes/Data/…`, volfs `/.vol/<dev>/<ino>` and
//! `/.nofollow/…` spellings of his root are his root. The canonical,
//! case-folded text compare is kept as the second arm (it is the only one that
//! sees a login home that does not exist yet). His login homes are the passwd
//! entry AND `$HOME` ([`protected_homes`], REL-4.S4b).
//!
//! **What isolation is** ([`process_isolation`]): the marker
//! [`ISOLATED_ENV`]`=1` (set by every test binary at load, by every drive
//! script, and by any non-interactive launch, and inherited by children), or
//! a process that IS a cargo test harness ([`exe_is_test_harness`]), which
//! is decided structurally so a test that forgets the marker is isolated
//! anyway. Callers that know more (the face's `--headless`, `cfg!(test)`)
//! pass their own reason in.
//!
//! The workspace guard `apps/tui/tests/it/egress_guard.rs` fails if a dial
//! API is called anywhere outside the sites that route through here.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Ports an isolated process never dials: his live threads daemon (3651
/// health, 3653 chassis), the pool (3753) and the AUI (9010). Kept for the
/// callers that name them in a refusal; the decision is the allowlist.
pub const PROD_PORTS: [u16; 4] = [3651, 3653, 3753, 9010];

/// **REFUSED EVEN WHEN GIVEN** (REL-6a6): [`PROD_PORTS`] plus tts (3652). A
/// list naming one of these is a mistake, never a grant.
pub const NEVER_PORTS: [u16; 5] = [3651, 3652, 3653, 3753, 9010];

/// **THE GRANTS A HARNESS HANDS AN ISOLATED CHILD** (REL-6a6, REL-6a8): a
/// comma- or space-separated list of `<addr>:<port>[@<pid>]` — the scratch
/// listeners it bound and the pid holding each. Unreadable is empty.
pub const ALLOW_ENV: &str = "CAULDRON_ISOLATED_PORTS";

/// The conventional "nothing is listening" port tests point a lever at.
/// Privileged (binding it needs root), so no daemon of his is on it; admitted
/// without a grant so a face pointed at "nothing" draws the down state.
pub const DEAD_PORT: u16 = 1;

/// **ONE GRANT** (REL-6a8): a listener's bound address and, when known, the
/// pid that bound it. `pid = None` (a bare harness grant) means "held by a
/// process of this isolated tree".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Grant {
    /// The address the listener was bound to (`0.0.0.0` / `::` = any of
    /// that family).
    pub addr: SocketAddr,
    /// The pid holding it.
    pub pid: Option<u32>,
}

/// Listeners this process bound and registered ([`grant_addr`]).
static GRANTED: std::sync::Mutex<BTreeSet<Grant>> = std::sync::Mutex::new(BTreeSet::new());

/// **REGISTER A SCRATCH LISTENER THIS PROCESS BOUND** (REL-6a8), so an
/// isolated dial to it is admitted while — and only while — this process
/// still holds it. Call it right after the bind, with the listener's
/// `local_addr()`. A [`NEVER_PORTS`] port, port 0 or a non-loopback,
/// non-wildcard address is refused (and not recorded).
pub fn grant_addr(addr: SocketAddr) -> Result<(), String> {
    let ip = unmap(addr.ip());
    if addr.port() == 0 || NEVER_PORTS.contains(&addr.port()) {
        return Err(format!("refused: port {} cannot be granted to an isolated process", addr.port()));
    }
    if !(ip.is_loopback() || ip.is_unspecified()) {
        return Err(format!("refused: {addr} is not a loopback listener"));
    }
    let g = Grant { addr: SocketAddr::new(ip, addr.port()), pid: Some(std::process::id()) };
    GRANTED.lock().unwrap_or_else(|e| e.into_inner()).insert(g);
    Ok(())
}

/// The listeners this process granted itself ([`grant_addr`]), each with this
/// process's pid — what a harness hands a child in [`ALLOW_ENV`].
pub fn granted() -> BTreeSet<Grant> {
    GRANTED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// **PARSE AN [`ALLOW_ENV`] VALUE.** Each token is `<ip>:<port>`,
/// `[<ip6>]:<port>` or a bare `<port>` (= `127.0.0.1:<port>`), optionally
/// followed by `@<pid>`. Any token that does not read fails the WHOLE list
/// (closed), naming it.
pub fn parse_grants(v: &str) -> Result<BTreeSet<Grant>, String> {
    let mut out = BTreeSet::new();
    for t in v.split(|c: char| c == ',' || c.is_whitespace()).filter(|t| !t.is_empty()) {
        let bad = || format!("{ALLOW_ENV}: `{t}` is not <addr>:<port>[@<pid>]");
        let (a, pid) = match t.rsplit_once('@') {
            Some((a, p)) => (a, Some(p.parse::<u32>().ok().filter(|p| *p > 1).ok_or_else(bad)?)),
            None => (t, None),
        };
        let addr = match a.parse::<u16>() {
            Ok(p) if p > 0 => SocketAddr::from(([127, 0, 0, 1], p)),
            Ok(_) => return Err(bad()),
            Err(_) => a.parse::<SocketAddr>().map_err(|_| bad())?,
        };
        if addr.port() == 0 {
            return Err(bad());
        }
        out.insert(Grant { addr: SocketAddr::new(unmap(addr.ip()), addr.port()), pid });
    }
    Ok(out)
}

/// An [`ALLOW_ENV`] value for `grants` (what a harness sets on a child).
pub fn grants_value(grants: impl IntoIterator<Item = Grant>) -> String {
    let set: BTreeSet<Grant> = grants.into_iter().collect();
    set.iter()
        .map(|g| match g.pid {
            Some(p) => format!("{}@{p}", g.addr),
            None => g.addr.to_string(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// **EVERY GRANT THIS PROCESS WAS GIVEN**: [`ALLOW_ENV`] (unreadable is
/// nothing) plus [`granted`].
pub fn given() -> BTreeSet<Grant> {
    let mut out = std::env::var(ALLOW_ENV).ok().and_then(|v| parse_grants(&v).ok()).unwrap_or_default();
    out.extend(granted());
    out
}

/// **THE TEST-ONLY ISOLATION MARKER** (REL-6a2). The operator never sets it.
pub const ISOLATED_ENV: &str = "CAULDRON_ISOLATED";

/// **WHY THIS PROCESS IS ISOLATED**, if it is: the marker is `1`, or the
/// running executable is a cargo test harness. `None` is his own process.
pub fn process_isolation() -> Option<String> {
    let harness = std::env::current_exe().is_ok_and(|p| exe_is_test_harness(&p));
    isolation_from(harness, std::env::var(ISOLATED_ENV).ok().as_deref())
}

/// [`process_isolation`] over explicit inputs.
pub fn isolation_from(test_harness: bool, marker: Option<&str>) -> Option<String> {
    if test_harness {
        return Some("a cargo test harness".into());
    }
    if marker.is_some_and(|x| x.trim() == "1") {
        return Some(format!("{ISOLATED_ENV}=1"));
    }
    None
}

/// **IS THIS EXECUTABLE A CARGO TEST HARNESS?** cargo names every test
/// binary `<target>-<16 hex>` and runs it from the build dir's `deps/`;
/// nothing installs a face or a CLI there or under such a name.
pub fn exe_is_test_harness(exe: &Path) -> bool {
    let in_deps = exe.parent().and_then(|d| d.file_name()).is_some_and(|d| d == "deps");
    let name = exe.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let hashed = name.rsplit_once('-').is_some_and(|(stem, h)| {
        !stem.is_empty() && h.len() == 16 && h.chars().all(|c| c.is_ascii_hexdigit())
    });
    in_deps && hashed
}

/// An IPv4-mapped IPv6 address as the IPv4 address it is.
fn unmap(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(a) => a.to_ipv4_mapped().map_or(IpAddr::V6(a), IpAddr::V4),
        v4 => v4,
    }
}

/// Where a URL goes, as the dialler will read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The host, lower-cased, without IPv6 brackets.
    pub host: String,
    /// The host as an address (IPv4-mapped read as IPv4), or `None` for a
    /// name (`localhost`).
    pub ip: Option<IpAddr>,
    /// The explicit port, else the scheme's default.
    pub port: u16,
    /// Whether the host is loopback.
    pub loopback: bool,
}

/// **PARSE A DIAL TARGET** with `url::Url`. `tcp://host:port` is how a raw
/// socket is written. Errors name what could not be read.
pub fn target_of(url: &str) -> Result<Target, String> {
    let u = url::Url::parse(url.trim()).map_err(|e| format!("`{url}` is not a URL ({e})"))?;
    let (host, ip): (String, Option<IpAddr>) = match u.host() {
        None => return Err(format!("`{url}` names no host")),
        Some(url::Host::Ipv4(a)) => (a.to_string(), Some(IpAddr::V4(a))),
        Some(url::Host::Ipv6(a)) => (a.to_string(), Some(IpAddr::V6(a))),
        // A non-special scheme (`tcp://`) leaves an IPv4 literal opaque, so
        // it is read as an address here; `[..]` brackets are already gone.
        Some(url::Host::Domain(d)) => {
            let d = d.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
            let ip = d.parse::<IpAddr>().ok();
            (d, ip)
        }
    };
    let port = u
        .port_or_known_default()
        .ok_or_else(|| format!("`{url}` names no port and `{}:` has no default one", u.scheme()))?;
    let ip = ip.map(unmap);
    let loopback = match ip {
        Some(a) => a.is_loopback(),
        None => host == "localhost",
    };
    Ok(Target { host, ip, port, loopback })
}

/// Whether a listener bound to `bound` (`None` = lsof's `*`, any address)
/// answers a dial to `t`'s host.
fn addr_serves(bound: Option<IpAddr>, t: &Target) -> bool {
    let Some(b) = bound.map(unmap) else { return true };
    match t.ip {
        Some(ip) => b == ip || (b.is_unspecified() && b.is_ipv4() == ip.is_ipv4()),
        // `localhost` resolves to 127.0.0.1 or ::1, whichever answers.
        None => b.is_loopback() || b.is_unspecified(),
    }
}

/// **THE DIAL DECISION.** `what` names the connection in the refusal;
/// `isolated` is the caller's [`process_isolation`] (or a richer answer).
/// Not isolated: admitted, unread. Isolated: [`admit_live`] over [`given`].
pub fn admit_url(what: &str, url: &str, isolated: Option<&str>) -> Result<(), String> {
    let Some(why) = isolated else { return Ok(()) };
    admit_live(what, url, why, &given())
}

/// **THE ALLOWLIST, PURE** (REL-6a6, REL-6a8), over explicit granted
/// addresses. Isolated: admitted only to a loopback host whose address a
/// grant in `allowed` serves, on its port (or [`DEAD_PORT`]), and never to a
/// [`NEVER_PORTS`] port; anything unreadable is refused. This half reads no
/// kernel state — [`admit_live`] adds the listener check.
pub fn admit_url_with(what: &str, url: &str, isolated: Option<&str>, allowed: &BTreeSet<SocketAddr>) -> Result<(), String> {
    let Some(why) = isolated else { return Ok(()) };
    let t = target_of(url).map_err(|e| format!("refused: a {what} dial from an isolated process ({why}): {e}"))?;
    if !t.loopback {
        return Err(format!(
            "refused: a {what} connection to `{}`, which is not loopback, from an isolated process ({why}); an isolated \
             process dials only scratch listeners on 127.0.0.1 / ::1 / localhost",
            t.host
        ));
    }
    if NEVER_PORTS.contains(&t.port) {
        return Err(format!(
            "refused: a {what} connection to port {}, a live daemon of his (3651/3652/3653/3753/9010), from an isolated \
             process ({why})",
            t.port
        ));
    }
    if t.port == DEAD_PORT || allowed.iter().any(|a| a.port() == t.port && addr_serves(Some(a.ip()), &t)) {
        return Ok(());
    }
    Err(format!(
        "refused: a {what} connection to {}:{}, which this isolated process ({why}) was not given. An isolated \
         process dials only a listener it bound and granted, one its harness handed it ({ALLOW_ENV}), or one its own \
         child holds",
        t.host, t.port
    ))
}

/// One TCP listener the kernel reports: who holds it, on what address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listener {
    /// The holding process.
    pub pid: u32,
    /// The bound address; `None` is lsof's `*` (any).
    pub addr: Option<IpAddr>,
}

/// **WHO LISTENS ON `port`, ON ANY ADDRESS**, from `lsof` (which reads the
/// kernel's socket table and dials nothing). `Err` when lsof cannot be run or
/// read — the caller refuses.
pub fn listeners_on(port: u16) -> Result<Vec<Listener>, String> {
    let out = std::process::Command::new("/usr/sbin/lsof")
        .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fpn"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("lsof could not be run ({e}), so the listeners on port {port} are unknown"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    // lsof exits 1 with no output when nothing matches.
    if !out.status.success() && !text.trim().is_empty() {
        return Err(format!("lsof failed on port {port} ({})", out.status));
    }
    parse_lsof(&text, port)
}

/// [`listeners_on`]'s parse of `lsof -Fpn`.
pub fn parse_lsof(text: &str, port: u16) -> Result<Vec<Listener>, String> {
    let mut out = Vec::new();
    let mut pid: Option<u32> = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = Some(p.parse().map_err(|_| format!("lsof: unreadable pid line `{line}`"))?);
        } else if let Some(n) = line.strip_prefix('n') {
            let pid = pid.ok_or_else(|| format!("lsof: a name line before any pid: `{line}`"))?;
            let (host, p) = n.rsplit_once(':').ok_or_else(|| format!("lsof: unreadable address `{n}`"))?;
            if p.parse::<u16>().ok() != Some(port) {
                continue;
            }
            let host = host.trim_start_matches('[').trim_end_matches(']');
            let addr = if host == "*" {
                None
            } else {
                Some(host.parse::<IpAddr>().map_err(|_| format!("lsof: unreadable address `{n}`"))?)
            };
            out.push(Listener { pid, addr });
        }
    }
    Ok(out)
}

/// **THIS ISOLATED PROCESS TREE** (REL-6a8), from `ps`: the processes that
/// descend from its ROOT — the nearest ancestor-or-self that is a cargo test
/// harness ([`exe_is_test_harness`]), else this process. His daemons
/// (launchd's children) and his faces (his terminals') are never in it.
#[derive(Debug, Clone)]
pub struct ProcTree {
    parent: HashMap<u32, u32>,
    me: u32,
    root: u32,
}

impl ProcTree {
    /// Read the process table. `Err` when `ps` cannot be run or read.
    pub fn read() -> Result<ProcTree, String> {
        let out = std::process::Command::new("/bin/ps")
            .args(["-A", "-o", "pid=,ppid=,comm="])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("ps could not be run ({e}), so this process tree is unknown"))?;
        if !out.status.success() {
            return Err(format!("ps failed ({})", out.status));
        }
        Ok(Self::from_ps(&String::from_utf8_lossy(&out.stdout), std::process::id()))
    }

    /// [`ProcTree::read`] over a `ps -o pid=,ppid=,comm=` table.
    pub fn from_ps(text: &str, me: u32) -> ProcTree {
        let mut parent = HashMap::new();
        let mut comm = HashMap::new();
        for l in text.lines() {
            let mut it = l.split_whitespace();
            let (Some(p), Some(pp)) = (it.next().and_then(|x| x.parse().ok()), it.next().and_then(|x| x.parse().ok())) else {
                continue;
            };
            parent.insert(p, pp);
            let c = l.trim_start();
            let c = c.split_once(char::is_whitespace).map_or("", |(_, r)| r.trim_start());
            let c = c.split_once(char::is_whitespace).map_or("", |(_, r)| r.trim_start());
            comm.insert(p, c.to_string());
        }
        let mut root = me;
        let mut at = me;
        for _ in 0..512 {
            if comm.get(&at).is_some_and(|c| exe_is_test_harness(Path::new(c))) {
                root = at;
                break;
            }
            match parent.get(&at) {
                Some(&pp) if pp > 1 && pp != at => at = pp,
                _ => break,
            }
        }
        ProcTree { parent, me, root }
    }

    /// Whether `pid` is `anc` or descends from it.
    fn descends(&self, pid: u32, anc: u32) -> bool {
        let mut at = pid;
        for _ in 0..512 {
            if at == anc {
                return true;
            }
            match self.parent.get(&at) {
                Some(&pp) if pp != at && pp > 0 => at = pp,
                _ => return false,
            }
        }
        false
    }

    /// Whether `pid` is in this isolated tree (its root or a descendant of it).
    pub fn ours(&self, pid: u32) -> bool {
        pid > 1 && self.descends(pid, self.root)
    }

    /// Whether `pid` is a child (at any depth) of this process.
    pub fn is_descendant(&self, pid: u32) -> bool {
        pid != self.me && self.descends(pid, self.me)
    }

    /// The tree's root pid.
    pub fn root(&self) -> u32 {
        self.root
    }
}

/// What [`decide`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Dial it.
    Admit,
    /// Refused (the reason).
    Refuse(String),
    /// Not granted, and the kernel reports NO listener on the port at all:
    /// there is nothing to dial. [`connect_tcp`] answers `ConnectionRefused`
    /// WITHOUT dialling, so "is anything already on this declared address?"
    /// still reads "no"; [`admit_live`] refuses.
    NothingListens(String),
}

/// **THE LIVE DIAL DECISION** (REL-6a8), over explicit grants. Isolated:
/// [`admit_url_with`] over (the granted addresses ∪ the addresses this
/// process's own DESCENDANTS listen on — the service the CLI spawned), AND every listener on the port, on
/// any address, is held by this isolated tree or by the pid a grant names —
/// so a grant whose listener died and whose port his daemon took is refused,
/// and a `[::1]` listener of his beside a granted `127.0.0.1` one is refused.
/// This process's OWN listeners count only when granted ([`grant_addr`]); a
/// sibling's or an ancestor's only when a harness names it ([`ALLOW_ENV`]).
pub fn decide(what: &str, url: &str, why: &str, grants: &BTreeSet<Grant>) -> Decision {
    let explicit: BTreeSet<SocketAddr> = grants.iter().map(|g| g.addr).collect();
    // The pure half first: a non-loopback, never-port or unreadable target is
    // refused without asking the kernel, and the dead port needs no lookup.
    let t = match target_of(url) {
        Ok(t) => t,
        Err(e) => return Decision::Refuse(format!("refused: a {what} dial from an isolated process ({why}): {e}")),
    };
    if t.port == DEAD_PORT || !t.loopback || NEVER_PORTS.contains(&t.port) {
        return admit_url_with(what, url, Some(why), &explicit).map_or_else(Decision::Refuse, |_| Decision::Admit);
    }
    let refuse = |d: String| format!("refused: a {what} connection to {}:{} from an isolated process ({why}): {d}", t.host, t.port);
    let ls = match listeners_on(t.port) {
        Ok(l) => l,
        Err(e) => return Decision::Refuse(refuse(e)),
    };
    let tree = match ProcTree::read() {
        Ok(t) => t,
        Err(e) => return Decision::Refuse(refuse(e)),
    };
    let pid_granted = |pid: u32| grants.iter().any(|g| g.pid == Some(pid) && g.addr.port() == t.port);
    if let Some(l) = ls.iter().find(|l| !tree.ours(l.pid) && !pid_granted(l.pid)) {
        return Decision::Refuse(refuse(format!(
            "the port is held by pid {} ({}), which is not in this isolated process tree (root pid {}) and no grant \
             names it",
            l.pid,
            l.addr.map_or("*".to_string(), |a| a.to_string()),
            tree.root()
        )));
    }
    let mut allowed = explicit;
    for l in ls.iter().filter(|l| tree.is_descendant(l.pid)) {
        let ip = l.addr.unwrap_or(IpAddr::from([0, 0, 0, 0]));
        allowed.insert(SocketAddr::new(ip, t.port));
        if l.addr.is_none() {
            allowed.insert(SocketAddr::new(IpAddr::from([0u16; 8]), t.port));
        }
    }
    match admit_url_with(what, url, Some(why), &allowed) {
        Ok(()) => Decision::Admit,
        Err(e) if ls.is_empty() => Decision::NothingListens(format!("{e} — and nothing listens there (per the kernel), so nothing was dialled")),
        Err(e) => Decision::Refuse(e),
    }
}

/// [`decide`] as a `Result`: only [`Decision::Admit`] admits.
pub fn admit_live(what: &str, url: &str, why: &str, grants: &BTreeSet<Grant>) -> Result<(), String> {
    match decide(what, url, why, grants) {
        Decision::Admit => Ok(()),
        Decision::Refuse(e) | Decision::NothingListens(e) => Err(e),
    }
}

/// [`admit_url`] for a raw socket address.
pub fn admit_addr(what: &str, addr: &SocketAddr, isolated: Option<&str>) -> Result<(), String> {
    admit_url(what, &format!("tcp://{addr}"), isolated)
}

/// **THE WORKSPACE'S ONE RAW TCP DIAL** (REL-6a5). Every
/// `TcpStream::connect*` in the CLI and in this crate is this one. Isolated,
/// it is decided by [`decide`] over [`given`] — no file is read to grant it
/// (REL-6a6-I B2). A refusal is an `io::ErrorKind::PermissionDenied` error,
/// so a caller that reads a failed connect as "nothing is listening there"
/// reads a refusal the same way, and nothing is dialled. An ungranted port the
/// kernel says NOBODY listens on is `ConnectionRefused`, also undialled — the
/// truthful answer to the CLI's pre-fork "is anything already there?".
pub fn connect_tcp(what: &str, addr: &SocketAddr, timeout: std::time::Duration) -> std::io::Result<std::net::TcpStream> {
    if let Some(why) = process_isolation() {
        match decide(what, &format!("tcp://{addr}"), &why, &given()) {
            Decision::Admit => {}
            Decision::Refuse(e) => return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, e)),
            Decision::NothingListens(e) => return Err(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, e)),
        }
    }
    std::net::TcpStream::connect_timeout(addr, timeout)
}

// ── the root check (REL-6a6 §2, REL-6a8 B1) ──────────────────────────────

/// `p` with symlinks resolved through its nearest existing ancestor (so a
/// path not yet created compares like one that exists; `/var` is
/// `/private/var`), lexically normalised first. Not absolute: `None`.
pub fn canonical(p: &Path) -> Option<PathBuf> {
    let p = crate::guard::normalize(p).ok()?;
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut at: &Path = &p;
    loop {
        if let Ok(c) = at.canonicalize() {
            return Some(tail.iter().rev().fold(c, |acc, t| acc.join(t)));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_os_string());
                at = parent;
            }
            _ => return Some(p.clone()),
        }
    }
}

/// `p` canonical and CASE-FOLDED: APFS is case-insensitive by default, so
/// `~/.CAULDRON` is `~/.cauldron` (REL-6a5-I F1). For a path that exists the
/// identity arm already decides; the fold is what refuses a case variant of a
/// login home that does not exist (yet), which `realpath` cannot normalise.
fn folded(p: &Path) -> Option<PathBuf> {
    canonical(p).map(|c| PathBuf::from(c.to_string_lossy().to_lowercase()))
}

/// Whether `a` is `b` or inside it, canonical and case-folded.
pub fn same_or_inside(a: &Path, b: &Path) -> bool {
    match (folded(a), folded(b)) {
        (Some(a), Some(b)) => a.starts_with(&b),
        _ => false,
    }
}

/// A file's identity: `(device, inode)`, following symlinks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ident {
    /// `st_dev`.
    pub dev: u64,
    /// `st_ino`.
    pub ino: u64,
}

/// `p`'s identity, following symlinks; `None` if it cannot be stat'd.
pub fn ident(p: &Path) -> Option<Ident> {
    std::fs::metadata(p).ok().map(|m| Ident { dev: m.dev(), ino: m.ino() })
}

/// **THE PHYSICAL ANCESTOR CHAIN OF `p`**, by identity: its nearest existing
/// ancestor-or-self, then every directory above it up to `/`, walked with the
/// kernel's `..` — so a firmlink, volfs or `.nofollow` spelling yields the
/// same chain as the plain path. A non-directory leaf contributes its own
/// identity and its textual parent. `Err` when the chain cannot be read (the
/// caller refuses).
pub fn physical_chain(p: &Path) -> Result<Vec<Ident>, String> {
    let p = crate::guard::normalize(p).map_err(|e| e.to_string())?;
    let mut at: &Path = &p;
    let meta = loop {
        match std::fs::metadata(at) {
            Ok(m) => break m,
            Err(_) => at = at.parent().ok_or_else(|| format!("no ancestor of `{}` exists", p.display()))?,
        }
    };
    let mut out = Vec::new();
    let mut dir = if meta.is_dir() {
        at.to_path_buf()
    } else {
        out.push(Ident { dev: meta.dev(), ino: meta.ino() });
        at.parent().ok_or_else(|| format!("`{}` has no parent", at.display()))?.to_path_buf()
    };
    let top = ident(Path::new("/")).ok_or("`/` cannot be stat'd")?;
    for _ in 0..512 {
        let id = ident(&dir).ok_or_else(|| format!("`{}` cannot be stat'd", dir.display()))?;
        out.push(id);
        if id == top {
            return Ok(out);
        }
        dir.push("..");
    }
    Err(format!("`{}` is deeper than 512 directories", p.display()))
}

/// **WHY `root` IS NOT A SCRATCH ROOT**, or `None` if it is. It must be
/// absolute, and must not be — by identity of its physical ancestor chain, and
/// again canonical and case-folded — his real cauldron root
/// (`<login>/.cauldron`) or inside it, anywhere under his login home `login`,
/// nor an ancestor of either. `login = None` (unreadable) refuses: unknown is
/// closed, and so is a root whose physical ancestry cannot be read (a volfs
/// `/.vol/<dev>/<ino>` FILE address has none: `/.vol/<dev>` cannot be stat'd).
pub fn scratch_root_refusal(root: &Path, login: Option<&Path>) -> Option<String> {
    if !root.is_absolute() {
        return Some(format!("`{}` is not absolute", root.display()));
    }
    let Some(login) = login else {
        return Some("his login home could not be read, so no root is known to be scratch".into());
    };
    let Some(r) = folded(root) else { return Some(format!("`{}` cannot be read as a path", root.display())) };
    let Some(l) = folded(login) else { return Some("his login home cannot be read as a path".into()) };
    let real = l.join(".cauldron");
    // Text arm (canonical, case-folded).
    if r.starts_with(&real) {
        return Some(format!("`{}` resolves to his real cauldron root {}", root.display(), real.display()));
    }
    if r.starts_with(&l) {
        return Some(format!("`{}` resolves to a path under his login home {}", root.display(), l.display()));
    }
    if l.starts_with(&r) {
        return Some(format!("`{}` is an ancestor of his login home", root.display()));
    }
    // Identity arm (REL-6a8 B1): `(dev, ino)` of the root's physical chain
    // against his login home and his real root (which may be a symlink out of
    // the home, so it is a fence of its own).
    let real_path = login.join(".cauldron");
    let fences: Vec<(Ident, String)> = [(login, "his login home"), (real_path.as_path(), "his real cauldron root")]
        .into_iter()
        .filter_map(|(p, n)| ident(p).map(|i| (i, format!("{n} {}", p.display()))))
        .collect();
    let chain = match physical_chain(root) {
        Ok(c) => c,
        Err(e) => return Some(format!("`{}`: its physical ancestry cannot be read ({e})", root.display())),
    };
    for (f, name) in &fences {
        if chain.contains(f) {
            return Some(format!("`{}` IS or is INSIDE {name} (same device and inode)", root.display()));
        }
    }
    if let Some(me) = ident(root) {
        for p in [login, real_path.as_path()] {
            if physical_chain(p).is_ok_and(|c| c.contains(&me)) {
                return Some(format!("`{}` is (by identity) an ancestor of {}", root.display(), p.display()));
            }
        }
    }
    None
}

/// **HIS LOGIN HOMES** (REL-4.S4b's derivation, REL-6a8): the passwd entry,
/// and `$HOME` — unless `$HOME` is itself a scratch dir: outside the passwd
/// home ([`scratch_root_refusal`], by identity), existing, and owned by the
/// same user as the passwd home (so a `sudo` that kept his `$HOME` still
/// protects it, while a test's scratch `HOME=/private/tmp/x` does not fence
/// off its own scratch root). Empty when the passwd home is unreadable — every
/// caller then refuses.
pub fn protected_homes() -> Vec<PathBuf> {
    let Some(pw) = crate::guard::passwd_home() else { return Vec::new() };
    let mut out = vec![pw.clone()];
    if let Some(h) = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from) {
        if h.is_absolute() && !home_is_scratch(&h, &pw) && !out.contains(&h) {
            out.push(h);
        }
    }
    out
}

/// [`protected_homes`]'s exemption for a scratch `$HOME`.
fn home_is_scratch(home: &Path, passwd: &Path) -> bool {
    let (Ok(h), Ok(p)) = (std::fs::metadata(home), std::fs::metadata(passwd)) else { return false };
    h.is_dir() && h.uid() == p.uid() && scratch_root_refusal(home, Some(passwd)).is_none()
}

/// [`scratch_root_refusal`] against EVERY home in `homes`; empty refuses.
pub fn scratch_refusal_homes(root: &Path, homes: &[PathBuf]) -> Option<String> {
    if homes.is_empty() {
        return Some("his login home could not be read, so no root is known to be scratch".into());
    }
    homes.iter().find_map(|h| scratch_root_refusal(root, Some(h)))
}

/// **A PATH TO A DAEMON OR STORE, DERIVED UNDER ISOLATION** (REL-6a6 §2) —
/// a cauldron root, a seam's socket path, a store: refused unless it is a
/// scratch root by [`scratch_root_refusal`] against every one of
/// [`protected_homes`].
pub fn admit_daemon_path(what: &str, path: &Path, isolated: Option<&str>) -> Result<(), String> {
    let Some(why) = isolated else { return Ok(()) };
    match scratch_refusal_homes(path, &protected_homes()) {
        None => Ok(()),
        Some(d) => Err(format!("refused: {what} from an isolated process ({why}): {d}")),
    }
}


/// **A PAID OR THIRD-PARTY API** (OpenRouter, Anthropic, any https host),
/// reached directly or through a child process: refused under isolation,
/// whatever it is pointed at.
pub fn admit_external_api(what: &str, isolated: Option<&str>) -> Result<(), String> {
    match isolated {
        None => Ok(()),
        Some(why) => Err(format!(
            "refused: {what} reaches an external API (OpenRouter / Anthropic / https), and an isolated process ({why}) \
             never does"
        )),
    }
}

/// **A DAEMON-CONTROL SPAWN** (`cauldron start|stop|restart …`,
/// `launchctl …`). Under isolation it is admitted only when `scratch_home` —
/// the `CAULDRON_HOME` the spawn will act on — was given EXPLICITLY and is a
/// scratch root ([`scratch_root_refusal`]: canonical, case-folded, not his
/// real cauldron home `<passwd home>/.cauldron`, not inside it, not under his
/// login home at all). `launchctl` acts on his real launchd domain whatever the
/// home, so its callers pass `None` and it is always refused.
pub fn admit_daemon_control(what: &str, scratch_home: Option<&Path>, isolated: Option<&str>) -> Result<(), String> {
    let Some(why) = isolated else { return Ok(()) };
    admit_daemon_control_homes(what, scratch_home, &protected_homes(), why)
}

/// [`admit_daemon_control_with`] against EVERY home in `homes` (each one's
/// `.cauldron` is a real root); `homes` empty refuses (REL-6a8: the passwd
/// home AND `$HOME`, [`protected_homes`]).
pub fn admit_daemon_control_homes(what: &str, scratch_home: Option<&Path>, homes: &[PathBuf], why: &str) -> Result<(), String> {
    if homes.is_empty() {
        return admit_daemon_control_with(what, scratch_home, None, why);
    }
    for h in homes {
        admit_daemon_control_with(what, scratch_home, Some(&h.join(".cauldron")), why)?;
    }
    Ok(())
}

/// [`admit_daemon_control`] with his real cauldron home given, so a test can
/// name it. `real = None` (the passwd lookup failed) refuses: unknown is
/// closed.
pub fn admit_daemon_control_with(what: &str, scratch_home: Option<&Path>, real: Option<&Path>, why: &str) -> Result<(), String> {
    let refuse = |detail: String| {
        Err(format!(
            "refused: {what} controls a daemon, and this process is isolated ({why}); {detail}. Point it at a scratch \
             environment with an explicit CAULDRON_HOME outside ~/.cauldron"
        ))
    };
    let Some(home) = scratch_home else { return refuse("it is not pointed at a scratch CAULDRON_HOME".into()) };
    if !home.is_absolute() {
        return refuse(format!("`{}` is not absolute", home.display()));
    }
    let Some(real) = real else { return refuse("his real home could not be read, so no home is known to be scratch".into()) };
    // REL-6a6 §2 (REL-6a5-I F1): canonical (symlinks resolved through the
    // nearest existing ancestor) and case-folded (APFS), so a symlink to his
    // root or `~/.CAULDRON` is his root; and nothing under his login home
    // (`real`'s parent) is a scratch root.
    if same_or_inside(home, real) {
        return refuse(format!("`{}` resolves to his real cauldron home {}", home.display(), real.display()));
    }
    if let Some(d) = scratch_root_refusal(home, real.parent()) {
        return refuse(d);
    }
    Ok(())
}

/// **A SPAWN THAT WRITES HIS MACHINE LOGIN** (`claude-accounts
/// set-live-login`: `~/.claude.json` and the keychain, which no scratch HOME
/// isolates). REL-6a4b's local refusal, folded into this choke point by
/// MERGE-15 so there is ONE refusal path for it — together with REL-6a5's
/// own rule, kept as the first arm: `tool = None` means the caller was NOT
/// pointed at a stand-in (the shipped default), and that is refused wherever
/// the checkout lives. Under isolation a pointed `tool` is admitted only when
/// it is a scratch path ([`scratch_root_refusal`]:
/// absolute, canonical, case-folded, not under and not an ancestor of) against
/// EVERY home in `protected` — his passwd home plus any stand-in a test
/// protects. `protected` empty (his home unreadable) refuses: unknown is
/// closed. Not isolated (his board) it admits.
pub fn admit_login_tool(what: &str, tool: Option<&Path>, protected: &[PathBuf], isolated: Option<&str>) -> Result<(), String> {
    let Some(why) = isolated else { return Ok(()) };
    let shown = tool.map_or_else(|| "the default tool".to_string(), |t| t.display().to_string());
    let refuse = |detail: String| {
        Err(format!(
            "refused: {what} ({shown}) is not run from an isolated process ({why}) — it writes the machine login \
             (~/.claude.json, the keychain); only a stub in a scratch dir may stand in for it: {detail}"
        ))
    };
    let Some(tool) = tool else { return refuse("it is not pointed at a stand-in".into()) };
    if protected.is_empty() {
        return refuse("his login home could not be read, so no path is known to be scratch".into());
    }
    for h in protected {
        if let Some(d) = scratch_root_refusal(tool, Some(h)) {
            return refuse(d);
        }
    }
    Ok(())
}

/// The `CAULDRON_HOME` this process was EXPLICITLY given, if any.
pub fn explicit_home() -> Option<PathBuf> {
    std::env::var_os("CAULDRON_HOME").filter(|v| !v.is_empty()).map(PathBuf::from)
}


#[cfg(test)]
mod tests {
    use super::*;

    /// MERGE-15: the folded set-live refusal. Refuses the tool under a
    /// protected home and an ancestor of one; admits a scratch stub; admits
    /// everything for his board; refuses when no home is known.
    #[test]
    fn admit_login_tool_refuses_a_tool_under_any_protected_home_and_admits_scratch() {
        let base = std::env::temp_dir().join(format!("merge15-login-{}", std::process::id()));
        let his = base.join("his");
        let stand_in = base.join("stand-in");
        let scratch = base.join("scratch");
        let p = [his.clone(), stand_in.clone()];
        let w = "set-live-login";
        assert!(admit_login_tool(w, Some(&his.join("bin/claude-accounts")), &p, Some("t")).is_err(), "under his home");
        assert!(admit_login_tool(w, None, &p, Some("t")).is_err(), "unpointed (the default), wherever it lives");
        assert!(admit_login_tool(w, Some(&stand_in.join("claude-accounts")), &p, Some("t")).is_err(), "under the stand-in");
        assert!(admit_login_tool(w, Some(&base), &p, Some("t")).is_err(), "an ancestor of his home");
        assert!(admit_login_tool(w, Some(Path::new("claude-accounts")), &p, Some("t")).is_err(), "relative");
        assert!(admit_login_tool(w, Some(&scratch.join("claude-accounts")), &[], Some("t")).is_err(), "no home known");
        let e = admit_login_tool(w, Some(&his.join("x")), &p, Some("t")).unwrap_err();
        assert!(e.starts_with("refused: set-live-login"), "{e}");
        assert_eq!(admit_login_tool(w, Some(&scratch.join("claude-accounts")), &p, Some("t")), Ok(()), "a scratch stub");
        assert_eq!(admit_login_tool(w, Some(&his.join("bin/claude-accounts")), &p, None), Ok(()), "his board");
    }

    const ISO: Option<&str> = Some("a test");

    fn v4(p: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], p))
    }

    fn v6(p: u16) -> SocketAddr {
        SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], p))
    }

    /// REL-6a5 §4 + REL-6a6 §1 + REL-6a8 (a grant is an ADDRESS and a port):
    /// every URL shape, isolated, against the grants {127.0.0.1:53498,
    /// 127.0.0.1:3652, 127.0.0.1:3653, [::1]:53600}. `true` = admitted.
    #[test]
    fn rel6a8_every_url_shape_is_read_as_the_dialler_reads_it_and_only_a_granted_address_is_admitted() {
        let allowed: BTreeSet<SocketAddr> = [v4(53498), v4(3652), v4(3653), v6(53600)].into_iter().collect();
        let rows: &[(&str, bool)] = &[
            // his ports, every spelling — 3652/3653 refused although LISTED
            ("ws://127.0.0.1:9010/", false),
            ("ws://127.0.0.1:9010", false),
            ("ws://127.0.0.1:9010?x", false),
            ("ws://127.0.0.1:9010#f", false),
            ("ws://127.0.0.1:9010/?a=b#f", false),
            ("ws://localhost:9010?a=b", false),
            ("ws://LOCALHOST:3653/ws", false),
            ("ws://127.0.0.1:3653?x", false),
            ("ws://user:pw@127.0.0.1:3651/", false),
            ("ws://[::1]:3753/", false),
            ("ws://[::ffff:127.0.0.1]:3653/", false),
            ("http://127.0.0.1:3651/health", false),
            ("http://127.0.0.1:3652/", false),
            ("tcp://127.0.0.1:3653", false),
            ("tcp://[::1]:9010", false),
            ("ws://0.0.0.0:9010/", false),
            (" ws://127.0.0.1:9010/ ", false),
            // his OTHER loopback daemons and any port nobody gave
            ("tcp://127.0.0.1:49253", false),
            ("tcp://127.0.0.1:49332", false),
            ("ws://127.0.0.1:47310/", false),
            ("ws://127.0.0.1:47311/", false),
            ("http://127.0.0.1:9009/", false),
            ("ws://127.0.0.1:9001/", false),
            ("ws://127.0.0.1:53499/", false),
            ("ws://127.0.0.1/", false),  // default port 80, not given
            ("wss://localhost/", false), // 443, not given
            // REL-6a6-I F2: the port is granted on 127.0.0.1 only — another
            // address on it is another listener
            ("ws://[::1]:53498/", false),
            ("tcp://[::1]:53498", false),
            ("ws://127.0.0.2:53498/", false),
            // ...and a [::1] grant is not a 127.0.0.1 one
            ("ws://127.0.0.1:53600/", false),
            // not loopback, even on a given port
            ("ws://0.0.0.0:53498/", false),
            ("ws://192.0.2.10:53498/", false),
            ("ws://127.0.0.1:53498@evil.example/", false),
            ("https://openrouter.ai/api/v1/chat/completions", false),
            ("https://api.anthropic.com/v1/messages", false),
            ("ws://example.com/", false),
            // unreadable is closed
            ("127.0.0.1:53498", false),
            ("not a url", false),
            ("tcp://127.0.0.1", false),
            ("", false),
            // the granted addresses, every spelling: admitted
            ("ws://127.0.0.1:53498/", true),
            ("ws://127.0.0.1:53498?x", true),
            ("ws://localhost:53498/ws#f", true),
            ("ws://[::ffff:127.0.0.1]:53498/", true),
            ("http://127.0.0.1:53498/health", true),
            ("tcp://127.0.0.1:53498", true),
            ("ws://[::1]:53600/", true),
            ("ws://localhost:53600/", true),
            // the dead port, ungiven: admitted (nothing can listen there)
            ("ws://127.0.0.1:1/", true),
        ];
        let mut wrong = Vec::new();
        for (url, admit) in rows {
            let got = admit_url_with("test", url, ISO, &allowed);
            if got.is_ok() != *admit {
                wrong.push(format!("{url:?}: expected admit={admit}, got {got:?}"));
            }
        }
        assert!(wrong.is_empty(), "{} row(s) wrong:\n{}", wrong.len(), wrong.join("\n"));
        // Opposition: with an EMPTY allow set the given port is refused too.
        assert!(admit_url_with("test", "ws://127.0.0.1:53498/", ISO, &BTreeSet::new()).is_err());
        // A wildcard bind serves every address of its family.
        let any4: BTreeSet<SocketAddr> = [SocketAddr::from(([0, 0, 0, 0], 53498))].into_iter().collect();
        assert!(admit_url_with("test", "ws://127.0.0.1:53498/", ISO, &any4).is_ok());
        assert!(admit_url_with("test", "ws://[::1]:53498/", ISO, &any4).is_err());
    }

    /// The scheme default is the port: no port is not "no refusal".
    #[test]
    fn rel6a5_a_missing_port_is_the_schemes_default() {
        assert_eq!(target_of("ws://127.0.0.1/").unwrap().port, 80);
        assert_eq!(target_of("wss://127.0.0.1/").unwrap().port, 443);
        assert_eq!(target_of("ws://127.0.0.1:9010?x").unwrap().port, 9010);
        assert_eq!(target_of("ws://[::1]:9010#f").unwrap().host, "::1");
    }

    /// Not isolated, nothing is refused — his own face and CLI keep every
    /// connection (opposition: a constant `Err` fails here).
    #[test]
    fn rel6a5_his_own_process_is_never_refused() {
        for url in ["ws://127.0.0.1:9010/", "ws://127.0.0.1:3653/ws", "ws://127.0.0.1:49253/", "https://openrouter.ai/", "garbage"] {
            assert_eq!(admit_url("test", url, None), Ok(()), "{url}");
            assert_eq!(admit_url_with("test", url, None, &BTreeSet::new()), Ok(()), "{url}");
        }
        assert_eq!(admit_external_api("type-one", None), Ok(()));
        assert_eq!(admit_daemon_control("cauldron start", None, None), Ok(()));
        assert_eq!(admit_daemon_path("x", Path::new("/Users/x/.cauldron"), None), Ok(()));
    }

    #[test]
    fn rel6a5_external_apis_are_refused_unconditionally_under_isolation() {
        assert!(admit_external_api("type-one", ISO).is_err());
    }

    #[test]
    fn rel6a8_the_grant_list_parses_closed_and_carries_the_address_and_pid() {
        let g = parse_grants("53498, [::1]:40001@4242 127.0.0.1:40002").unwrap();
        let want: BTreeSet<Grant> = [
            Grant { addr: v4(53498), pid: None },
            Grant { addr: v6(40001), pid: Some(4242) },
            Grant { addr: v4(40002), pid: None },
        ]
        .into_iter()
        .collect();
        assert_eq!(g, want);
        assert_eq!(parse_grants(&grants_value(want.clone())).unwrap(), want, "round trip");
        for bad in ["53498,x", "0", "70000", "127.0.0.1:0", "127.0.0.1:5@", "127.0.0.1:5@1", "127.0.0.1:5@x", "host:5"] {
            assert!(parse_grants(bad).is_err(), "{bad:?} parsed");
        }
        assert!(parse_grants("").unwrap().is_empty());
        assert!(grant_addr(v4(3652)).is_err() && grant_addr(v4(0)).is_err(), "a never-port is not grantable");
        assert!(grant_addr(SocketAddr::from(([192, 0, 2, 1], 40001))).is_err(), "not loopback");
        assert!(!granted().iter().any(|g| g.addr.port() == 3652));
    }

    #[test]
    fn rel6a8_lsof_and_ps_parse() {
        let l = parse_lsof("p10\nf3\nn127.0.0.1:5000\np11\nf4\nn[::1]:5000\nf5\nn*:5000\nn127.0.0.1:5001\n", 5000).unwrap();
        assert_eq!(
            l,
            vec![
                Listener { pid: 10, addr: Some("127.0.0.1".parse().unwrap()) },
                Listener { pid: 11, addr: Some("::1".parse().unwrap()) },
                Listener { pid: 11, addr: None },
            ]
        );
        assert!(parse_lsof("n127.0.0.1:5000\n", 5000).is_err(), "a name before a pid");
        // root = the nearest test-harness ancestor; his daemon (launchd's
        // child) and a sibling of the harness are not in the tree.
        let ps = "1 0 /sbin/launchd\n 500 1 /Users/x/.cauldron/bin/cauldron\n 600 1 /bin/zsh\n 700 600 /x/target/debug/deps/it-0123456789abcdef\n 800 700 /x/target/debug/tui\n 900 800 /usr/bin/python3\n 950 600 /usr/bin/other\n";
        let t = ProcTree::from_ps(ps, 800);
        assert_eq!(t.root(), 700);
        assert!(t.ours(700) && t.ours(800) && t.ours(900));
        assert!(!t.ours(500) && !t.ours(600) && !t.ours(950) && !t.ours(1));
        assert!(t.is_descendant(900) && !t.is_descendant(800) && !t.is_descendant(700));
        // no harness above: the root is this process
        let t = ProcTree::from_ps("1 0 /sbin/launchd\n 600 1 /bin/zsh\n 800 600 /x/tui\n", 800);
        assert_eq!(t.root(), 800);
        assert!(!t.ours(600));
    }

    /// A scratch listener of THIS process: (listener port, accept count).
    /// Never granted here.
    fn listener() -> (std::net::TcpListener, u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::Ordering;
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let n = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = n.clone();
        let l2 = l.try_clone().unwrap();
        std::thread::spawn(move || {
            for s in l2.incoming() {
                c.fetch_add(1, Ordering::SeqCst);
                drop(s);
            }
        });
        (l, port, n)
    }

    /// **REL-6a6 §5, AT RUNTIME, THROUGH THE REAL DIAL.** Two live scratch
    /// listeners in this isolated process: the UNLISTED one is refused
    /// (`PermissionDenied`) and never sees a connection; the one this
    /// process GRANTED is dialled and does. (3652 is refused by the table
    /// above; it is never dialled here, because his tts is on it.)
    #[test]
    fn rel6a6_an_unlisted_scratch_listener_is_refused_and_a_granted_one_is_dialled() {
        use std::sync::atomic::Ordering;
        assert!(process_isolation().is_some(), "sentinel: this test binary is isolated");
        let (_a, unlisted, unlisted_hits) = listener();
        let (_b, listed, listed_hits) = listener();
        assert!(!given().iter().any(|g| g.addr.port() == unlisted), "sentinel: the unlisted port is not given");
        grant_addr(v4(listed)).unwrap();
        let t = std::time::Duration::from_secs(2);
        let refused = connect_tcp("test", &v4(unlisted), t).map(drop);
        let admitted = connect_tcp("test", &v4(listed), t).map(drop);
        let t0 = std::time::Instant::now();
        while listed_hits.load(Ordering::SeqCst) == 0 && t0.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        std::thread::sleep(t0.elapsed() * 2 + std::time::Duration::from_millis(50));
        let e = refused.expect_err("an UNLISTED scratch listener was dialled from an isolated process");
        assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied, "{e}");
        assert!(e.to_string().contains("was not given"), "{e}");
        assert_eq!(unlisted_hits.load(Ordering::SeqCst), 0, "the unlisted listener saw a connection");
        admitted.expect("a GRANTED scratch listener was refused");
        assert!(listed_hits.load(Ordering::SeqCst) > 0, "the granted listener was never dialled");
    }

    /// A python listener that counts accepts into `<dir>/hits` and writes its
    /// port to `<dir>/ready`. `orphan`: started through a shell that exits at
    /// once, so it is re-parented to launchd and is OUTSIDE this process tree —
    /// the stand-in for "his daemon". Returns (pid, port, child handle if not
    /// an orphan).
    fn py_listener(dir: &Path, v6only: bool, port: u16, orphan: bool) -> (u32, u16, Option<std::process::Child>) {
        // One element per line: a `\` continuation would strip python's indentation.
        const PY: &str = concat!(
            "import socket,sys,os,time\n",
            "fam,port,d=sys.argv[1],int(sys.argv[2]),sys.argv[3]\n",
            "s=socket.socket(socket.AF_INET6 if fam=='6' else socket.AF_INET)\n",
            "s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)\n",
            "if fam=='6': s.setsockopt(socket.IPPROTO_IPV6,socket.IPV6_V6ONLY,1)\n",
            "s.bind(('::1' if fam=='6' else '127.0.0.1',port)); s.listen(8)\n",
            "open(d+'/ready.tmp','w').write(str(s.getsockname()[1])); os.rename(d+'/ready.tmp',d+'/ready')\n",
            "s.settimeout(1); end=time.time()+40\n",
            "while time.time()<end:\n",
            "  try:\n",
            "    c,_=s.accept(); open(d+'/hits','a').write('x'); c.close()\n",
            "  except socket.timeout: pass\n",
        );
        std::fs::create_dir_all(dir).unwrap();
        let _ = std::fs::remove_file(dir.join("ready"));
        let fam = if v6only { "6" } else { "4" };
        let (pid, child) = if orphan {
            let out = std::process::Command::new("/bin/sh")
                .args(["-c", "python3 -c \"$0\" \"$1\" \"$2\" \"$3\" </dev/null >/dev/null 2>&1 & echo $!", PY, fam, &port.to_string()])
                .arg(dir)
                .output()
                .unwrap();
            (String::from_utf8_lossy(&out.stdout).trim().parse::<u32>().expect("the orphan's pid"), None)
        } else {
            let c = std::process::Command::new("python3").args(["-c", PY, fam, &port.to_string()]).arg(dir).spawn().unwrap();
            (c.id(), Some(c))
        };
        let t0 = std::time::Instant::now();
        while !dir.join("ready").exists() {
            assert!(t0.elapsed() < std::time::Duration::from_secs(15), "the python listener never bound");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let port = std::fs::read_to_string(dir.join("ready")).unwrap().trim().parse().unwrap();
        (pid, port, child)
    }

    fn hits(dir: &Path) -> usize {
        std::fs::read_to_string(dir.join("hits")).map(|s| s.len()).unwrap_or(0)
    }

    fn kill(pid: u32) {
        let _ = std::process::Command::new("/bin/kill").arg(pid.to_string()).status();
    }

    /// **REL-6a6-I F1 + F3: A GRANT DIES WITH ITS LISTENER, AND A LISTED PORT
    /// HELD BY A STRANGER IS NOT A GRANT.** This process binds and grants
    /// 127.0.0.1:P and is admitted (opposition). It drops the listener; an
    /// ORPHAN python (re-parented to launchd, outside this tree — the stand-in
    /// for "his daemon took the freed ephemeral port") binds the same P. The
    /// stale in-process grant, and a bare harness grant naming P, are both
    /// refused, naming the stranger's pid, and the stranger sees no
    /// connection. A grant that NAMES the stranger's pid (a drive script's
    /// `@pid`) is admitted — so the refusal is the pid check, not a constant.
    #[test]
    fn rel6a8_a_grant_dies_with_its_listener_and_a_strangers_listener_is_refused() {
        let d = std::env::temp_dir().join(format!("rel6a8-f1-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let l2 = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let p2 = l2.local_addr().unwrap().port();
        grant_addr(v4(p2)).unwrap();
        let url2 = format!("tcp://127.0.0.1:{p2}");
        assert_eq!(admit_live("test", &url2, "a test", &given()), Ok(()), "opposition: our own granted listener");
        drop(l2);
        let (stranger, sp, _) = py_listener(&d, false, p2, true);
        assert_eq!(sp, p2, "sentinel: the stranger holds the freed port");
        let tree = ProcTree::read().unwrap();
        assert!(!tree.ours(stranger), "sentinel: the orphan is outside this process tree");
        let stale = admit_live("test", &url2, "a test", &given());
        let bare = admit_live("test", &url2, "a test", &[Grant { addr: v4(p2), pid: None }].into_iter().collect());
        let e = connect_tcp("test", &v4(p2), std::time::Duration::from_secs(2)).map(drop);
        let named = admit_live("test", &url2, "a test", &[Grant { addr: v4(p2), pid: Some(stranger) }].into_iter().collect());
        std::thread::sleep(std::time::Duration::from_millis(1500)); // past the stranger's 1 s accept tick
        let seen = hits(&d);
        kill(stranger);
        let _ = std::fs::remove_dir_all(&d);
        for (n, r) in [("stale in-process grant", &stale), ("bare harness grant", &bare)] {
            let e = r.as_ref().expect_err(n);
            assert!(e.contains(&format!("pid {stranger}")) && e.contains("not in this isolated process tree"), "{n}: {e}");
        }
        let e = e.expect_err("connect_tcp dialled a stranger's listener on a granted port");
        assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied, "{e}");
        assert_eq!(seen, 0, "the stranger's listener saw a connection");
        assert_eq!(named, Ok(()), "opposition: a grant naming the holder's pid");
    }

    /// **REL-6a6-I F2: A GRANT IS AN ADDRESS.** This process holds and grants
    /// 127.0.0.1:P (admitted — opposition), then a stranger binds [::1]:P (his
    /// threads daemon binds both). Now the 127.0.0.1 dial is refused (a
    /// stranger holds the port), and a [::1] dial was never granted.
    #[test]
    fn rel6a8_a_strangers_v6_listener_beside_a_granted_v4_one_is_refused() {
        let d = std::env::temp_dir().join(format!("rel6a8-f2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (_l, p, _h) = listener();
        grant_addr(v4(p)).unwrap();
        let before = admit_live("test", &format!("ws://127.0.0.1:{p}/"), "a test", &given());
        let before_local = admit_live("test", &format!("ws://localhost:{p}/"), "a test", &given());
        let (stranger, sp, _) = py_listener(&d, true, p, true);
        assert_eq!(sp, p, "sentinel: the stranger holds [::1]:P");
        let v4_after = admit_live("test", &format!("ws://127.0.0.1:{p}/"), "a test", &given());
        let local_after = admit_live("test", &format!("ws://localhost:{p}/"), "a test", &given());
        let v6_after = admit_live("test", &format!("ws://[::1]:{p}/"), "a test", &given());
        kill(stranger);
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(before, Ok(()), "opposition: our granted v4 listener alone");
        assert_eq!(before_local, Ok(()), "opposition: localhost to our v4 listener alone");
        for (n, r) in [("127.0.0.1", v4_after), ("localhost", local_after), ("[::1]", v6_after)] {
            let e = r.expect_err(n);
            assert!(e.contains(&format!("pid {stranger}")), "{n}: {e}");
        }
    }

    /// **THE CLI'S CASE: A LISTENER ITS OWN CHILD HOLDS.** A
    /// python child of this process (as a service the CLI spawned is) listens;
    /// nobody granted it; the dial is admitted and the child sees it. The
    /// same python as an ORPHAN is refused (the F1 test), and this process's
    /// own ungranted listener is refused (the unlisted test), so this is the
    /// descendant check, not a constant.
    #[test]
    fn rel6a8_a_listener_held_by_our_own_child_is_admitted_without_a_grant() {
        let d = std::env::temp_dir().join(format!("rel6a8-child-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (pid, p, child) = py_listener(&d, false, 0, false);
        assert!(ProcTree::read().unwrap().is_descendant(pid), "sentinel: the python is our child");
        assert!(!given().iter().any(|g| g.addr.port() == p), "sentinel: nobody granted it");
        let r = connect_tcp("test", &v4(p), std::time::Duration::from_secs(2)).map(drop);
        let t0 = std::time::Instant::now();
        while hits(&d) == 0 && t0.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let seen = hits(&d);
        let mut child = child.unwrap();
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&d);
        r.expect("our own child's listener was refused");
        assert!(seen > 0, "the child's listener never saw the dial");
    }

    /// **THE CLI'S PRE-FORK PROBE: AN UNGRANTED PORT NOBODY LISTENS ON.**
    /// `connect_tcp` answers `ConnectionRefused` and dials nothing (the
    /// message says so — a real ECONNREFUSED does not), so "is anything
    /// already on this declared address?" reads "no"; `admit_live` (the face's
    /// question) still refuses. Opposition: the unlisted-listener test, where
    /// a listener IS present, is `PermissionDenied`.
    #[test]
    fn rel6a8_an_ungranted_port_nobody_listens_on_is_connection_refused_and_undialled() {
        let p = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        assert!(listeners_on(p).unwrap().is_empty(), "sentinel: nothing listens on {p}");
        let e = connect_tcp("test", &v4(p), std::time::Duration::from_secs(2)).map(drop).expect_err("a dial to nothing succeeded");
        assert_eq!(e.kind(), std::io::ErrorKind::ConnectionRefused, "{e}");
        assert!(e.to_string().contains("nothing was dialled"), "{e}");
        assert!(admit_live("test", &format!("tcp://127.0.0.1:{p}"), "a test", &BTreeSet::new()).is_err());
    }

    /// **REL-6a6-I B2: A PORT WRITTEN IN A SCRATCH ROOT IS NOT A GRANT.** A
    /// child copy of this test binary runs with `CAULDRON_HOME` = a verified
    /// scratch root; it binds an UNGRANTED listener of its own, writes its
    /// address into the root's seam table and a declaration (what REL-6a6's
    /// root grant read), and dials it: refused, "was not given", no accept.
    #[test]
    fn rel6a8_a_port_recorded_in_a_scratch_root_is_not_a_grant() {
        const CHILD: &str = "REL6A8_B2_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let root = PathBuf::from(std::env::var_os("CAULDRON_HOME").unwrap());
            let (_l, p, h) = listener();
            std::fs::create_dir_all(root.join("data/dev")).unwrap();
            std::fs::write(root.join("data/dev/seams.json"), format!(r#"{{"rows":[{{"addr":"127.0.0.1:{p}"}}]}}"#)).unwrap();
            std::fs::write(root.join("svc.toml"), format!("[launch]\naddr = \"localhost:{p}\"\n")).unwrap();
            let e = connect_tcp("test", &v4(p), std::time::Duration::from_secs(2)).map(drop);
            std::thread::sleep(std::time::Duration::from_millis(200));
            let e = e.expect_err("a port recorded in the scratch root was dialled");
            assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied, "{e}");
            assert!(e.to_string().contains("was not given"), "{e}");
            assert_eq!(h.load(std::sync::atomic::Ordering::SeqCst), 0, "the recorded listener saw a connection");
            return;
        }
        let d = std::env::temp_dir().join(format!("rel6a8-b2-{}", std::process::id()));
        let root = d.join("scratch/.cauldron");
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(scratch_refusal_homes(&root, &protected_homes()), None, "sentinel: it IS a scratch root");
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "egress::tests::rel6a8_a_port_recorded_in_a_scratch_root_is_not_a_grant", "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .env("CAULDRON_HOME", &root)
            .env_remove(ALLOW_ENV)
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&d);
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(text.contains("1 passed"), "sentinel: the child ran the test:\n{text}");
        assert!(out.status.success(), "the child dialled a root-recorded port:\n{text}");
    }

    /// A scratch stand-in for his login home, with `.cauldron` inside it.
    fn stand_in(tag: &str) -> (PathBuf, PathBuf) {
        let d = std::env::temp_dir().join(format!("rel6a6-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let login = d.join("login");
        std::fs::create_dir_all(login.join(".cauldron/data/prod")).unwrap();
        std::fs::create_dir_all(login.join(".cauldron/bin")).unwrap();
        std::fs::write(login.join(".cauldron/bin/cauldron"), "stand-in").unwrap();
        std::fs::create_dir_all(login.join("code")).unwrap();
        std::fs::create_dir_all(d.join("scratch/.cauldron/data/dev")).unwrap();
        (d.canonicalize().unwrap(), login.canonicalize().unwrap())
    }

    /// **REL-6a6 §2 (REL-6a5-I F1): A SYMLINK OR A CASE VARIANT OF HIS ROOT
    /// IS HIS ROOT.** Against a scratch stand-in for his login home, so
    /// nothing of his is touched. Sentinels: the link canonicalises to the
    /// real root, and the case variant exists (APFS is case-insensitive).
    #[test]
    fn rel6a6_a_symlink_or_case_variant_of_his_root_is_refused_and_a_scratch_root_is_not() {
        let (d, login) = stand_in("root");
        let real = login.join(".cauldron");
        let link = d.join("link-to-real");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let case = login.join(".CAULDRON");
        assert_eq!(link.canonicalize().unwrap(), real.canonicalize().unwrap(), "sentinel: the link IS his root");
        assert!(case.exists(), "sentinel: the case variant exists (APFS)");
        let refuse = |p: &Path| scratch_root_refusal(p, Some(&login));
        let mut wrong = Vec::new();
        for (p, want_refused) in [
            (real.clone(), true),
            (link.clone(), true),
            (link.join("data/prod"), true),
            (case.clone(), true),
            (case.join("data"), true),
            (d.join("LOGIN/.cauldron"), true), // his login home, another case
            (d.join("Login/.Cauldron/data"), true),
            (login.join("code/scratch/.cauldron"), true), // under his login home
            (d.clone(), true),                             // an ancestor of it
            (d.join("scratch/.cauldron"), false),
            (d.join("not-yet/.cauldron"), false),
        ] {
            if refuse(&p).is_some() != want_refused {
                wrong.push(format!("{}: want refused={want_refused}, got {:?}", p.display(), refuse(&p)));
            }
        }
        assert!(scratch_root_refusal(Path::new("rel/x"), Some(&login)).is_some(), "relative");
        assert!(scratch_root_refusal(&d.join("scratch/.cauldron"), None).is_some(), "unknown login is closed");
        let ctl = |p: &Path| admit_daemon_control_with("cauldron start", Some(p), Some(&real), "a test").is_ok();
        if ctl(&link) || ctl(&case) || !ctl(&d.join("scratch/.cauldron")) {
            wrong.push(format!("daemon control: link {} case {} scratch {}", ctl(&link), ctl(&case), ctl(&d.join("scratch/.cauldron"))));
        }
        let _ = std::fs::remove_dir_all(&d);
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// **REL-6a6-I B1: A FIRMLINK, VOLFS OR `.nofollow` SPELLING OF HIS ROOT
    /// IS HIS ROOT — BY IDENTITY.** Against a stand-in (nothing of his is
    /// stat'd). Sentinels: every alias has the stand-in root's (dev, ino), and
    /// `realpath` leaves each one unchanged (so the text arm alone cannot see
    /// it). Opposition: the firmlink spelling of the SCRATCH root is admitted.
    #[test]
    fn rel6a8_a_firmlink_volfs_or_nofollow_alias_of_his_root_is_refused_by_identity() {
        let (d, login) = stand_in("alias");
        let real = login.join(".cauldron");
        let fl = |p: &Path| PathBuf::from(format!("/System/Volumes/Data{}", p.display()));
        let nf = |p: &Path| PathBuf::from(format!("/.nofollow{}", p.display()));
        let vol = |p: &Path| {
            let m = std::fs::metadata(p).unwrap();
            PathBuf::from(format!("/.vol/{}/{}", m.dev(), m.ino()))
        };
        let mut sentinel = Vec::new();
        for a in [fl(&real), nf(&real), vol(&real)] {
            if ident(&a) != ident(&real) {
                sentinel.push(format!("{} is not his root by identity", a.display()));
            }
            if canonical(&a).is_some_and(|c| c.starts_with(&login)) {
                sentinel.push(format!("{} canonicalises into the login home — the text arm sees it", a.display()));
            }
        }
        assert!(sentinel.is_empty(), "sentinel: {}", sentinel.join("\n"));
        let scratch = d.join("scratch/.cauldron");
        let mut wrong = Vec::new();
        for (p, want_refused) in [
            (fl(&real), true),
            (fl(&real).join("data/prod"), true),
            (fl(&real).join("not-yet/seam.sock"), true),
            (fl(&real).join("bin/cauldron"), true),
            (fl(&login), true),
            (fl(&login).join("code/x/.cauldron"), true),
            (fl(&d), true), // an ancestor of his login home
            (nf(&real), true),
            (nf(&real).join("data/prod"), true),
            (vol(&real), true),
            (vol(&real).join("data/prod"), true),
            (vol(&real).join("not-yet"), true),
            (vol(&login.join("code")), true),         // a dir inside his home, by inode
            (vol(&real.join("bin/cauldron")), true), // a FILE by inode: no readable ancestry
            (scratch.clone(), false),
            (fl(&scratch), false),
            (fl(&scratch).join("data/dev"), false),
            (fl(&d).join("not-yet/.cauldron"), false),
        ] {
            let got = scratch_root_refusal(&p, Some(&login));
            if got.is_some() != want_refused {
                wrong.push(format!("{}: want refused={want_refused}, got {got:?}", p.display()));
            }
        }
        // Every gate reads the same rule.
        let ctl = |p: &Path| admit_daemon_control_with("cauldron start", Some(p), Some(&real), "a test").is_ok();
        if ctl(&fl(&real)) || ctl(&vol(&real)) || !ctl(&fl(&scratch)) {
            wrong.push(format!("daemon control: fl {} vol {} scratch {}", ctl(&fl(&real)), ctl(&vol(&real)), ctl(&fl(&scratch))));
        }
        if scratch_refusal_homes(&nf(&real), &[login.clone()]).is_none() || scratch_refusal_homes(&scratch, &[]).is_none() {
            wrong.push("scratch_refusal_homes".into());
        }
        let _ = std::fs::remove_dir_all(&d);
        assert!(wrong.is_empty(), "{} row(s) wrong:\n{}", wrong.len(), wrong.join("\n"));
    }

    /// **REL-6a6-I F4: THE CASE-FOLD'S OWN WITNESS.** For a path that exists,
    /// identity and `realpath` already decide, so the fold is invisible there.
    /// It is the only arm that refuses a case variant of a login home that
    /// does NOT exist (yet) — `realpath` cannot normalise what is not there.
    #[test]
    fn rel6a8_the_case_fold_refuses_a_case_variant_of_a_login_home_that_does_not_exist() {
        let d = std::env::temp_dir().join(format!("rel6a8-fold-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let ghost = d.join("ghost-login");
        assert!(!ghost.exists(), "sentinel: the login home does not exist");
        let refused = scratch_root_refusal(&d.join("GHOST-LOGIN/.cauldron"), Some(&ghost));
        let refused2 = scratch_root_refusal(&d.join("Ghost-Login/x"), Some(&ghost));
        let other = scratch_root_refusal(&d.join("ghost-login-2/.cauldron"), Some(&ghost));
        let _ = std::fs::remove_dir_all(&d);
        assert!(refused.is_some() && refused2.is_some(), "a case variant of the login home was admitted");
        assert_eq!(other, None, "opposition: a sibling name is scratch");
    }

    /// REL-4.S4b via REL-6a8: a `$HOME` that is a scratch dir does not fence
    /// off its own scratch root, while one under (or equal to) the passwd home
    /// is protected.
    #[test]
    fn rel6a8_a_scratch_home_is_not_a_fence_and_one_under_the_passwd_home_is() {
        let (d, login) = stand_in("homes");
        let scratch_home = d.join("scratch");
        assert!(home_is_scratch(&scratch_home, &login), "a scratch HOME");
        assert!(!home_is_scratch(&login, &login), "the passwd home itself");
        assert!(!home_is_scratch(&login.join("code"), &login), "under the passwd home");
        assert!(!home_is_scratch(&d.join("missing"), &login), "missing is protected (closed)");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rel6a5_daemon_control_needs_an_explicit_scratch_home() {
        let real = Path::new("/Users/someone/.cauldron");
        let w = |h: Option<&str>| admit_daemon_control_with("cauldron start", h.map(Path::new), Some(real), "a test");
        assert!(w(None).is_err(), "unpointed");
        assert!(w(Some("/Users/someone/.cauldron")).is_err(), "his home");
        assert!(w(Some("/Users/someone/.cauldron/envs/dev")).is_err(), "inside his home");
        assert!(w(Some("/Users/someone/.cauldron/../.cauldron")).is_err(), "his home, spelled around");
        assert!(w(Some("/Users/someone/.Cauldron")).is_err(), "his home, another case");
        assert!(w(Some("/Users/someone/.cauldron-scratch")).is_err(), "REL-6a6: under his login home");
        assert!(w(Some("relative/home")).is_err(), "relative");
        assert!(w(Some("/private/tmp/scratch/.cauldron")).is_ok(), "a scratch home");
        assert!(admit_daemon_control_with("x", Some(Path::new("/tmp/s")), None, "a test").is_err(), "unknown real home");
        assert!(admit_daemon_control_homes("x", Some(Path::new("/tmp/s")), &[], "a test").is_err(), "no homes");
    }

    #[test]
    fn rel6a5_isolation_signals() {
        assert!(isolation_from(true, None).is_some());
        assert!(isolation_from(false, Some("1")).is_some());
        assert!(isolation_from(false, Some("0")).is_none());
        assert!(isolation_from(false, None).is_none());
        assert!(exe_is_test_harness(Path::new("/x/target/debug/deps/it-0123456789abcdef")));
        assert!(!exe_is_test_harness(Path::new("/Users/someone/bin/cauldron")));
    }
}
