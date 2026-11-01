//! **REL-6a5: NOTHING ISOLATED DIALS OUT, AND ONE CHOKE POINT DECIDES IT.**
//!
//! Every network dial the face, the CLI and the libraries they link can make
//! routes through `env::egress` (`admit_url` / `connect_tcp`) or the face's
//! `cauldron::live::dial_ws`; every daemon-control spawn through
//! `env::egress::admit_daemon_control`; every external-API child through
//! `env::egress::admit_external_api`. This module proves it two ways:
//!
//! - RUNTIME tables: every constructor against every URL shape of a live
//!   port (B2), the seam's reconnect re-check (dial-time, not spawn-time), the
//!   type-one child (B1), the opener and the account tool — each opposed by an
//!   admitted case that really dials a SCRATCH listener;
//! - TEXT guards over the whole workspace (F3): no dial API outside the
//!   allow-list below, and no test source anywhere spawns the face, the CLI
//!   or a daemon-control / external binary except where named.
//!
//! No test here dials 3651/3653/3753/9010 or any external host: a "live" port
//! in the reconnect test is a scratch listener the gate is told to treat as
//! live.

use std::collections::{BTreeMap, BTreeSet};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tui::cauldron::live;

fn panic_text(e: Box<dyn std::any::Any + Send>) -> String {
    e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default()
}

/// A scratch listener on an ephemeral loopback port that counts accepts and
/// drops each connection at once.
fn scratch() -> (u16, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    assert!(!live::PROD_PORTS.contains(&port), "the kernel handed out a live port");
    // REL-6a6: given — the dial under test is the GATE's decision, not the
    // allowlist's (the allowlist has its own table below).
    env::egress::grant_addr(l.local_addr().unwrap()).unwrap();
    let n = Arc::new(AtomicUsize::new(0));
    let c = n.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            c.fetch_add(1, Ordering::SeqCst);
            drop(s);
        }
    });
    (port, n)
}

/// Every spelling of a live port the inspector found or the brief names,
/// plus non-loopback hosts (any external API).
fn refused_shapes() -> Vec<String> {
    let mut v = Vec::new();
    for p in live::PROD_PORTS {
        for u in [
            format!("ws://127.0.0.1:{p}/"),
            format!("ws://127.0.0.1:{p}"),
            format!("ws://127.0.0.1:{p}?x"),
            format!("ws://127.0.0.1:{p}#f"),
            format!("ws://localhost:{p}?a=b"),
            format!("ws://[::1]:{p}/"),
            format!("ws://u:pw@127.0.0.1:{p}/"),
            format!("ws://0.0.0.0:{p}/"),
        ] {
            v.push(u);
        }
    }
    v.push("ws://192.0.2.1:53498/".into());
    v.push("wss://openrouter.ai/".into());
    v.push("wss://api.anthropic.com/".into());
    v
}

/// **B2 + §4: EVERY CONSTRUCTOR REFUSES EVERY SHAPE.** Rows: the decision
/// (`refuse_isolated_url`), the seam's gate (`mirror::gate`), `aui::spawn`,
/// `pool::client::spawn`. Opposed: the same shapes on a scratch port are
/// admitted by every row, and the AUI really dials `?x` and `#f` forms of a
/// scratch listener (so the refusal is about the PORT, not the spelling).
#[test]
fn rel6a5_every_constructor_refuses_every_url_shape_of_a_live_port() {
    assert!(live::isolation(false).is_some(), "sentinel: the `it` harness is isolated");
    let mut wrong = Vec::new();
    for url in refused_shapes() {
        if live::refuse_isolated_url("t", &url).is_none() {
            wrong.push(format!("refuse_isolated_url admitted {url}"));
        }
        if tui::mirror::gate(&url).is_ok() {
            wrong.push(format!("mirror::gate admitted {url}"));
        }
        let aui = std::panic::catch_unwind(|| drop(tui::aui::spawn(tui::aui::Config::pinned(url.clone()))));
        match aui.map_err(panic_text) {
            Err(t) if t.contains("refused") => {}
            Err(t) => wrong.push(format!("aui::spawn {url}: panicked without a refusal: {t}")),
            Ok(()) => wrong.push(format!("aui::spawn {url}: returned (admitted)")),
        }
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let pool = std::panic::catch_unwind(|| drop(tui::pool::client::spawn(url.clone(), tx)));
        match pool.map_err(panic_text) {
            Err(t) if t.contains("refused") => {}
            Err(t) => wrong.push(format!("pool::client::spawn {url}: panicked without a refusal: {t}")),
            Ok(()) => wrong.push(format!("pool::client::spawn {url}: returned (admitted)")),
        }
    }
    // Opposition (doctrine 2): the same spellings on a scratch port.
    let (port, accepts) = scratch();
    for url in [
        format!("ws://127.0.0.1:{port}/"),
        format!("ws://127.0.0.1:{port}?x"),
        format!("ws://localhost:{port}#f"),
    ] {
        if let Some(r) = live::refuse_isolated_url("t", &url) {
            wrong.push(format!("a scratch url was refused: {url}: {r}"));
        }
        if let Err(r) = tui::mirror::gate(&url) {
            wrong.push(format!("mirror::gate refused a scratch url: {url}: {r}"));
        }
    }
    // REL-6a8 (REL-6a6-I F2): the scratch listener is bound on 127.0.0.1
    // only, so `[::1]` on its port is ANOTHER listener's address (his threads
    // daemon binds both) and is refused. REL-6a6 listed it as admitted — the
    // port-not-address hole, written into the opposition as the right answer.
    let v6 = format!("ws://[::1]:{port}/");
    if live::refuse_isolated_url("t", &v6).is_none() || tui::mirror::gate(&v6).is_ok() {
        wrong.push(format!("a scratch PORT on an address nobody granted was admitted: {v6}"));
    }
    assert!(wrong.is_empty(), "{} row(s) wrong:\n{}", wrong.len(), wrong.join("\n"));
    // ...and the AUI really dials the query and fragment spellings of it.
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _a = tui::aui::spawn(tui::aui::Config::pinned(format!("ws://127.0.0.1:{port}?x")));
        let _b = tui::aui::spawn(tui::aui::Config::pinned(format!("ws://127.0.0.1:{port}#f")));
        let t0 = Instant::now();
        while accepts.load(Ordering::SeqCst) < 2 && t0.elapsed() < Duration::from_secs(5) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    assert!(accepts.load(Ordering::SeqCst) >= 2, "the AUI did not dial the admitted scratch spellings");
}

/// **THE SEAM RE-CHECKS AT DIAL TIME, ON EVERY RECONNECT** (was REL-6a4
/// item 1). An isolated seam is up against scratch listener A; the port lever
/// changes mid-life to scratch B, which the gate is told is live. Every later
/// attempt is refused at the gate, and B is never dialled — outlasting the
/// seam's backoff ladder for three attempts after the first refusal (the
/// ladder doubles from 500 ms; the window below is > 2 × its longest step
/// reached here). Opposed: A is dialled (the seam does dial), and the
/// refusal is SEEN in the link state (the gate ran at reconnect, it did not
/// merely fail to connect). FORCED RED by building the seam with no gate.
#[test]
fn rel6a5_the_seam_refuses_a_live_port_at_reconnect_outlasting_the_backoff() {
    let (a_port, a_hits) = scratch();
    let (b_port, b_hits) = scratch();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (state, b) = rt.block_on(async {
        let (h, _rx, ports) = tui::mirror::spawn_gated_at(a_port, b_port);
        let t0 = Instant::now();
        while a_hits.load(Ordering::SeqCst) == 0 && t0.elapsed() < Duration::from_secs(5) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(a_hits.load(Ordering::SeqCst) > 0, "sentinel: the seam never dialled scratch A");
        ports.repoint(b_port);
        // Until a refusal is seen, or 12 s (past the ladder's 500+1000+2000+4000).
        let t0 = Instant::now();
        let mut refused_at = None;
        while t0.elapsed() < Duration::from_secs(12) {
            if let tui::seam::held::LinkState::Down { reason } = h.link_state() {
                if reason.contains("refused") && reason.contains(&b_port.to_string()) {
                    refused_at = Some(Instant::now());
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // Then three more ladder steps' worth with B still listening.
        if refused_at.is_some() {
            tokio::time::sleep(Duration::from_millis(4_500)).await;
        }
        let st = h.link_state();
        drop(h);
        drop(ports);
        (st, b_hits.load(Ordering::SeqCst))
    });
    assert_eq!(b, 0, "the seam dialled the port declared live after it changed mid-life (dial-time gate missing); link: {state:?}");
    match state {
        tui::seam::held::LinkState::Down { reason } => {
            assert!(reason.contains("refused"), "down, but not refused at the gate: {reason}")
        }
        other => panic!("the link was never refused at reconnect: {other:?}"),
    }
}

/// **B1: AN ISOLATED PROCESS NEVER RUNS type-one**, which reaches his threads
/// daemon (haiku) or OpenRouter (jev, billed) and ignores the marker. With
/// `CAULDRON_TYPE_ONE` UNSET (the default state of any process that never
/// set it) and the binary pointed at a recorder, `real_call` and the spawn
/// itself both refuse, and the recorder never runs. FORCED RED by removing
/// the gate in `type_one::run` (the recorder then runs).
#[test]
fn rel6a5_type_one_is_refused_under_isolation_whatever_the_switch_says() {
    use env::paths::Source;
    use tui::cauldron::seeding::{self, TypeOneCall, SEED_RIPPLE};
    let dir = std::env::temp_dir().join(format!("rel6a5-t1-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let rec = dir.join("ran");
    let _ = std::fs::remove_file(&rec);
    let bin = dir.join("type-one");
    std::fs::write(&bin, format!("#!/bin/sh\necho \"$@\" >> {}\necho '{{}}'\n", rec.display())).unwrap();
    assert!(std::process::Command::new("/bin/chmod").arg("+x").arg(&bin).status().unwrap().success());
    let env = env::Environment {
        root: dir.clone(),
        root_source: Source::CauldronHome,
        name: "prod".into(),
        name_source: Source::Default,
        data_root: dir.join("data").join("prod"),
    };
    let a = tui::cauldron::app::App::open(env, tui::cauldron::store::seed_dir(), false).expect("seed loads");
    let policy = a.rstore.live.clone();
    let ripple = policy.get(SEED_RIPPLE).expect("the seeding ripple").clone();
    std::env::set_var("CAULDRON_TYPE_ONE_BIN", &bin);
    std::env::remove_var("CAULDRON_TYPE_ONE");
    let real = seeding::real_call(&ripple, &a.seed_live()).map(|c| c.adapter());
    let cli = tui::cauldron::seeding::type_one::TypeOneCli::new(bin.clone(), tui::cauldron::seeding::type_one::Adapter::Jev);
    let items = a.seed_candidates();
    let asked = cli.ask(&ripple, "q", "boundaries", &items[..items.len().min(2)], "r", "p").map(|_| ());
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    std::env::remove_var("CAULDRON_TYPE_ONE_BIN");
    let ran = std::fs::read_to_string(&rec).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!items.is_empty(), "sentinel: the seed has candidates, so `ask` reaches the spawn");
    assert!(ran.is_empty(), "the type-one recorder RAN from an isolated process: {ran}");
    let real = real.expect_err("real_call admitted the real type-one call");
    assert!(real.contains("external API"), "real_call refused, but not at the choke point: {real}");
    let asked = asked.expect_err("TypeOneCli::ask spawned type-one");
    assert!(asked.contains("external API"), "the spawn refused, but not at the choke point: {asked}");
}

/// The real macOS `open` and the real `claude-accounts` are refused under
/// isolation unless pointed at a stand-in; pointed, they are admitted.
#[test]
fn rel6a5_the_opener_and_the_account_tool_need_a_stand_in_under_isolation() {
    use tui::cauldron::cli::Opener;
    let e = Opener::System.open("/nonexistent/rel6a5-never-opened").expect_err("the real `open` ran");
    assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied, "{e}");
    assert_eq!(Opener::Suppressed.open("/x").unwrap(), false, "opposition: a stand-in is admitted");
    assert!(std::env::var_os("TUI_POOL_ACCOUNTS_TOOL").is_none(), "sentinel: nothing in `it` points the tool");
    let why = tui::pool::action::isolation_refusal().expect("the real claude-accounts was admitted");
    assert!(why.contains("refused"), "{why}");
}

// ── F3: the text guards ────────────────────────────────────────────────────

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// The workspace's members, read from its manifest (so a new member is
/// scanned without editing this file).
fn members(ws: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(ws.join("Cargo.toml")).unwrap();
    let start = text.find("members = [").expect("a members list");
    let end = start + text[start..].find(']').unwrap();
    text[start..end]
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            (!l.starts_with('#')).then(|| l.split('"').nth(1)).flatten().map(str::to_string)
        })
        .collect()
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd {
        let p = e.unwrap().path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// **SPLIT A SRC FILE INTO ITS SHIPPED AND ITS `#[cfg(test)]` TEXT**, line
/// numbers kept (the other half's lines are blanked). Every `#[cfg(test)]`
/// item is cut out by brace matching that skips string, raw-string and char
/// literals and comments — not "everything after the first `#[cfg(test)]`",
/// which hid 3,600 shipped lines of `cli/src/main.rs` (REL-6a5 forcing F5
/// found it: a raw dial planted at line 3,400 went unseen).
fn split_cfg_test(text: &str) -> (String, String) {
    let attr = concat!("#[cfg(", "test)]");
    let b = text.as_bytes();
    let mut test_ranges: Vec<(usize, usize)> = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find(attr) {
        let start = from + at;
        let mut i = start + attr.len();
        // The item ends at its matching `}` or, braceless, at `;`.
        let mut depth = 0usize;
        let mut end = b.len();
        while i < b.len() {
            let c = b[i];
            if c == b'/' && b.get(i + 1) == Some(&b'/') {
                while i < b.len() && b[i] != b'\n' { i += 1; }
                continue;
            }
            if c == b'/' && b.get(i + 1) == Some(&b'*') {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') { i += 1; }
                i += 2;
                continue;
            }
            if c == b'r' && (b.get(i + 1) == Some(&b'"') || b.get(i + 1) == Some(&b'#')) && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) {
                let mut j = i + 1;
                let mut hashes = 0;
                while b.get(j) == Some(&b'#') { hashes += 1; j += 1; }
                if b.get(j) == Some(&b'"') {
                    j += 1;
                    loop {
                        if j >= b.len() { break; }
                        if b[j] == b'"' && (0..hashes).all(|k| b.get(j + 1 + k) == Some(&b'#')) { j += 1 + hashes; break; }
                        j += 1;
                    }
                    i = j;
                    continue;
                }
            }
            if c == b'"' {
                i += 1;
                while i < b.len() && b[i] != b'"' { if b[i] == b'\\' { i += 1; } i += 1; }
                i += 1;
                continue;
            }
            if c == b'\'' {
                // a char literal ('x', '\n', '{') — not a lifetime ('a ident)
                if b.get(i + 2) == Some(&b'\'') { i += 3; continue; }
                if b.get(i + 1) == Some(&b'\\') {
                    let mut j = i + 2;
                    while j < b.len() && b[j] != b'\'' { j += 1; }
                    i = j + 1;
                    continue;
                }
                i += 1;
                continue;
            }
            if c == b'{' { depth += 1; }
            if c == b'}' {
                depth = depth.saturating_sub(1);
                if depth == 0 { end = i + 1; break; }
            }
            if c == b';' && depth == 0 { end = i + 1; break; }
            i += 1;
        }
        test_ranges.push((start, end));
        from = end.max(start + attr.len());
        if from >= b.len() { break; }
    }
    let mut shipped = String::with_capacity(text.len());
    let mut test = String::with_capacity(text.len());
    for (k, ch) in text.char_indices() {
        let in_test = test_ranges.iter().any(|(a, z)| k >= *a && k < *z);
        let keep_nl = ch == '\n';
        shipped.push(if in_test && !keep_nl { ' ' } else { ch });
        test.push(if !in_test && !keep_nl { ' ' } else { ch });
    }
    (shipped, test)
}

#[test]
fn rel6a5_the_cfg_test_splitter_cuts_items_not_tails() {
    let src = concat!(
        "fn a() { dial(); }\n",
        "#[cfg(", "test)]\n",
        "mod t { fn x() { let s = \"}\"; let c = '{'; let r = r#\"}}\"#; } // }\n }\n",
        "fn b() { DIAL(); }\n",
        "#[cfg(", "test)]\n",
        "use foo::bar;\n",
        "fn c() { LATE(); }\n",
    );
    let (shipped, test) = split_cfg_test(src);
    assert!(shipped.contains("dial()") && shipped.contains("DIAL()") && shipped.contains("LATE()"), "shipped lost code: {shipped}");
    assert!(!shipped.contains("mod t") && !shipped.contains("use foo"), "shipped kept test code: {shipped}");
    assert!(test.contains("mod t") && test.contains("use foo") && !test.contains("DIAL"), "test half wrong: {test}");
    assert_eq!(shipped.lines().count(), src.lines().count(), "line numbers moved");
}

/// Dial APIs. Split with `concat!` so this file's own text holds none.
///
/// REL-6a6 (REL-6a5-I F2) widened this from call SPELLINGS to the shapes the
/// inspector planted and the old list missed: an aliased `TcpStream`, any
/// `UdpSocket` / `TcpSocket`, the SYNC tungstenite connect, and a spawn of
/// curl / wget by absolute path. It is still a text scan, and a text scan is
/// never complete (a macro, a re-export, an FFI call): **the protection is the
/// RUNTIME gate** (`env::egress::admit_url_with` / `connect_tcp`, an
/// allowlist of given ports), which refuses a dial however it was spelled.
/// This guard only keeps new dial sites from appearing unnoticed.
fn dial_needles() -> Vec<&'static str> {
    vec![
        concat!("connect_", "async"),
        concat!("client_", "async"),
        concat!("TcpStream::", "connect"),
        concat!("TcpStream", " as "),
        concat!("Tcp", "Socket"),
        concat!("Udp", "Socket"),
        concat!("UnixStream::", "connect"),
        concat!("UnixStream", " as "),
        concat!("tungstenite::", "connect("),
        concat!("tungstenite::", "client("),
        concat!("reqwest", "::"),
        concat!("ureq", "::"),
        concat!("hyper", "::"),
        concat!("Command::new(\"", "curl\")"),
        concat!("/", "curl\""),
        concat!("/", "wget\""),
    ]
}

/// **REL-6a6 §4: THE SIX SHAPES REL-6a5-I PLANTED ARE CAUGHT.** Each line of
/// the inspector's plant (`cli/src/jobs.rs`, plant-1) is a fixture here, and
/// each must be flagged on its own line; the sentinel the old guard did catch
/// is flagged too. Opposed: a type in a signature, a non-dial use of the
/// socket module and a comment naming every shape are NOT flagged, so
/// "flag everything" fails. (Built with `concat!` so this file's own text is
/// not a plant.)
#[test]
fn rel6a6_the_six_planted_dial_shapes_are_caught_and_harmless_lines_are_not() {
    let shapes: Vec<(&str, String)> = vec![
        ("sentinel", format!("let _ = std::net::{}(\"127.0.0.1:49253\");", concat!("TcpStream::", "connect"))),
        ("alias", format!("use std::net::{}Sock; let _ = Sock::connect(\"127.0.0.1:49253\");", concat!("TcpStream", " as "))),
        ("udp", format!("let u = std::net::{}::bind(\"127.0.0.1:0\").unwrap(); u.connect(\"127.0.0.1:3652\").unwrap();", concat!("Udp", "Socket"))),
        ("sync ws", format!("let _ = tokio_{}\"ws://127.0.0.1:3653/ws\");", concat!("tungstenite::tungstenite::", "connect("))),
        ("curl", format!("let _ = std::process::Command::new(\"/usr/bin{}).arg(\"https://openrouter.ai\").status();", concat!("/", "curl\""))),
        ("open", format!("let _ = std::process::Command::new(\"/usr{}).arg(\"/tmp/x\").status();", concat!("/bin/", "open\""))),
        ("tcp socket", format!("let s = tokio::net::{}::new_v4().unwrap(); let _ = s.connect(\"127.0.0.1:3653\".parse().unwrap());", concat!("Tcp", "Socket"))),
    ];
    let mut needles = dial_needles();
    needles.extend(control_needles());
    let mut missed = Vec::new();
    for (name, line) in &shapes {
        if find(line, &needles).is_empty() {
            missed.push(format!("{name}: {line}"));
        }
    }
    assert!(missed.is_empty(), "the guard misses {} planted shape(s):\n{}", missed.len(), missed.join("\n"));
    let harmless = [
        "fn ask(s: &std::net::TcpStream) -> std::io::Result<()> { Ok(()) }",
        "let a: std::net::SocketAddr = \"127.0.0.1:1\".parse().unwrap();",
        &format!("// {} {} {} {}", concat!("Udp", "Socket"), concat!("Tcp", "Socket"), concat!("/", "curl\""), concat!("TcpStream", " as ")),
    ];
    for h in harmless {
        assert!(find(h, &needles).is_empty(), "flagged a harmless line: {h}");
    }
}

/// HTTP client crates. A dependency on one — directly, or RENAMED with
/// `package = "<it>"` (REL-6a5-I F2) — in a member in reach is a dial path
/// the needles above cannot see.
const HTTP_CRATES: &[&str] = &["reqwest", "ureq", "hyper", "curl", "isahc", "surf", "attohttpc", "minreq", "awc", "http-req"];

/// The HTTP client crates `manifest` depends on (key or renamed `package`).
fn http_deps(manifest: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_deps = false;
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_deps = t.contains("dependencies");
            continue;
        }
        if !in_deps || t.starts_with('#') {
            continue;
        }
        let key = t.split(['=', ' ', '.']).next().unwrap_or("").trim_matches('"');
        let pkg = t.split("package").nth(1).and_then(|r| r.split('"').nth(1)).unwrap_or("");
        for c in HTTP_CRATES {
            if key == *c || pkg == *c {
                out.push(format!("{t} ({c})"));
            }
        }
    }
    out
}

/// **REL-6a6: NO MEMBER IN REACH DEPENDS ON AN HTTP CLIENT, RENAMED OR
/// NOT.** Opposed: the detector finds a direct and a renamed dependency in a
/// planted manifest, and not a crate that merely contains the name.
#[test]
fn rel6a6_no_member_in_reach_depends_on_an_http_client_even_renamed() {
    let planted = "[package]\nname = \"x\"\n[dependencies]\nreqwest = \"0.12\"\nweb = { package = \"ureq\", version = \"2\" }\nhyperlocal-ish = \"1\"\n[dev-dependencies]\nnet = { version = \"1\", package = \"curl\" }\n";
    assert_eq!(http_deps(planted).len(), 3, "the detector, planted: {:?}", http_deps(planted));
    let ws = workspace();
    let mut wrong = Vec::new();
    let mut read = 0;
    for m in members(&ws) {
        if OUT_OF_REACH.iter().any(|(p, _)| *p == m) {
            continue;
        }
        let text = std::fs::read_to_string(ws.join(&m).join("Cargo.toml")).unwrap();
        read += 1;
        wrong.extend(http_deps(&text).into_iter().map(|d| format!("{m}/Cargo.toml: {d}")));
    }
    assert!(read >= 10, "sentinel: only {read} member manifests read");
    assert!(wrong.is_empty(), "an HTTP client in reach of the face/CLI:\n{}", wrong.join("\n"));
}

/// Daemon-control and external spawns.
fn control_needles() -> Vec<&'static str> {
    vec![
        concat!("\"/bin/", "launchctl\""),
        concat!("Command::new(\"", "launchctl\")"),
        concat!("Command::new(\"", "open\")"),
        concat!("/bin/", "open\""),
        concat!("Command::new(\"", "type-one\")"),
        concat!("\"/usr/bin/", "security\""),
        concat!("Command::new(\"", "security\")"),
        concat!("cauldron_", "bin()"),
    ]
}

fn find(text: &str, needles: &[&'static str]) -> Vec<(usize, &'static str)> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        for n in needles {
            if line.contains(n) {
                out.push((i + 1, *n));
            }
        }
    }
    out
}

/// **THE SHIPPED ALLOW-LIST**: `(workspace path, needle, count, a string the
/// file must also contain (its gate), why)`. A count that differs — a site
/// added or removed — fails, so every exception is named here with its
/// reason and cannot outlive or outgrow it.
const SHIPPED_ALLOWED: &[(&str, &str, usize, &str, &str)] = &[
    ("libs/env/src/egress.rs", concat!("TcpStream::", "connect"), 1, "admit_addr(", "THE choke point: connect_tcp admits, then dials"),
    ("apps/tui/src/cauldron/live.rs", concat!("connect_", "async"), 1, "refuse_isolated_url(what, url)", "the face's websocket choke: dial_ws"),
    (
        "apps/tui/src/seam/link.rs",
        concat!("connect_", "async"),
        1,
        "supervise(&supervision)",
        "the vendored seam (seamdiff); every attempt, reconnects included, is gated first by mirror::EgressGate through Supervisor::before_connect",
    ),
    (
        "cli/src/launchd.rs",
        concat!("\"/bin/", "launchctl\""),
        4,
        "admit_daemon_control(",
        "one is launchctl_control (bootstrap/bootout; refused under isolation); three are read-only print/list queries",
    ),
    ("apps/tui/src/app.rs", concat!("cauldron_", "bin()"), 1, "admit_daemon_control(", "restart_in: gated by admit_daemon_control"),
    (
        "cli/src/ctl.rs",
        concat!("tungstenite::", "client("),
        1,
        "egress::connect_tcp(",
        "RV1.S12a ctl node: a websocket HANDSHAKE over the TcpStream env::egress::connect_tcp already admitted; it dials nothing itself",
    ),
    ("apps/tui/src/cauldron_env.rs", concat!("cauldron_", "bin()"), 1, "pub fn cauldron_bin", "the resolver's definition, not a spawn"),
];

/// Members whose shipped code is NOT in the face/CLI's reach, with why.
const OUT_OF_REACH: &[(&str, &str)] = &[
    ("services/threads", "the threads DAEMON: neither the face nor the CLI links it (tui has it as a dev-dependency for pool_daemon only); its own dials are its job"),
    ("services/tts", "the tts DAEMON: linked by nothing the face or the CLI ships"),
];

/// **RV1.S1d: THE OUT_OF_REACH POPULATION THAT `tui`/`cauldron` REACH ONLY
/// THROUGH DEV-DEPENDENCIES**, by `(package, the member whose
/// [dev-dependencies] carries the edge, why)`. A dev edge is never linked into
/// a shipped binary, so these are still out of reach; but each is named, its
/// edge must really be dev-only, and a row nothing reaches is stale.
const DEV_ONLY_REACH: &[(&str, &str, &str)] = &[(
    "threads",
    "tui",
    "tui's [dev-dependencies] (pool_daemon runs the daemon in-process); `cauldron` meets it only through soup-seed's own \
     dev-dependency on tui — neither edge is in a shipped binary",
)];

/// Package names one dependency table declares, renames resolved through
/// `package =` and, for `workspace = true`, through `[workspace.dependencies]`.
fn dep_table_names(t: Option<&toml::Value>, ws_deps: &toml::Table, out: &mut BTreeSet<String>) {
    let Some(t) = t.and_then(|v| v.as_table()) else { return };
    for (k, v) in t {
        let inherited = v.get("workspace").and_then(|w| w.as_bool()) == Some(true);
        let renamed = v
            .get("package")
            .and_then(|p| p.as_str())
            .or_else(|| if inherited { ws_deps.get(k).and_then(|w| w.get("package")).and_then(|p| p.as_str()) } else { None });
        out.insert(renamed.unwrap_or(k).to_string());
    }
}

/// A manifest's (normal + build, dev) dependency package names, every
/// `[target.*]` section included.
fn manifest_deps(manifest: &str, ws_deps: &toml::Table) -> (BTreeSet<String>, BTreeSet<String>) {
    let m: toml::Table = manifest.parse().expect("a parseable manifest");
    let (mut ship, mut dev) = (BTreeSet::new(), BTreeSet::new());
    let mut scopes = vec![&m];
    if let Some(t) = m.get("target").and_then(|t| t.as_table()) {
        scopes.extend(t.values().filter_map(|v| v.as_table()));
    }
    for s in scopes {
        for k in ["dependencies", "build-dependencies", "build_dependencies"] {
            dep_table_names(s.get(k), ws_deps, &mut ship);
        }
        for k in ["dev-dependencies", "dev_dependencies"] {
            dep_table_names(s.get(k), ws_deps, &mut dev);
        }
    }
    (ship, dev)
}

/// `Cargo.lock` as a graph: package names, and per package its SHIPPED edges
/// (normal + build) and its DEV edges. The lock does not say which is which,
/// so a package with a manifest in `manifests` (a workspace member) has each
/// lock edge classified by that manifest, and an edge no manifest section
/// accounts for is an error (the parser missed a form). A registry package's
/// lock edges are all shipped (cargo does not resolve a dependency's dev-deps).
struct DepGraph {
    names: Vec<String>,
    ship: Vec<Vec<usize>>,
    dev: Vec<Vec<usize>>,
}

fn dep_graph(lock: &str, manifests: &BTreeMap<String, String>, ws_deps: &toml::Table) -> Result<DepGraph, Vec<String>> {
    let l: toml::Table = lock.parse().expect("a parseable Cargo.lock");
    // (name, version, source, dependencies). A package is identified by all
    // three: RV1.M4-Q2 — one lock may hold two same-named, same-versioned
    // packages from different sources (two `protoledger` git pins), and a
    // lookup by name alone cannot tell them apart.
    let pkgs: Vec<(String, String, String, Vec<String>)> = l
        .get("package")
        .and_then(|p| p.as_array())
        .expect("Cargo.lock has [[package]]")
        .iter()
        .map(|p| {
            let s = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let deps = p.get("dependencies").and_then(|d| d.as_array()).map(|a| a.iter().filter_map(|d| d.as_str().map(str::to_string)).collect());
            (s("name"), s("version"), s("source"), deps.unwrap_or_default())
        })
        .collect();
    // "name", "name version" or "name version (source)". A lock reference's
    // source omits the package's `#<precise>` fragment (a git source is
    // `git+<url>?rev=<r>#<sha>` on the package, `git+<url>?rev=<r>` in a
    // reference), so the package's source is compared with it stripped.
    let resolve = |d: &str| {
        let (head, src) = match d.split_once(" (") {
            Some((h, rest)) => (h, Some(rest.strip_suffix(')').unwrap_or(rest))),
            None => (d, None),
        };
        let mut w = head.split_whitespace();
        let (n, v) = (w.next().unwrap_or(""), w.next());
        let hit: Vec<usize> = (0..pkgs.len())
            .filter(|&i| {
                let (pn, pv, ps, _) = &pkgs[i];
                pn == n && v.is_none_or(|v| pv == v) && src.is_none_or(|s| ps.split('#').next() == Some(s))
            })
            .collect();
        (hit.len() == 1).then(|| hit[0])
    };
    let mut errs = Vec::new();
    let (mut ship, mut dev) = (vec![Vec::new(); pkgs.len()], vec![Vec::new(); pkgs.len()]);
    for (i, (name, _, source, deps)) in pkgs.iter().enumerate() {
        // Only a source-less (path / workspace) package is a member; a git or
        // registry package that shares a member's name is not classified by
        // that member's manifest.
        let declared = if source.is_empty() { manifests.get(name).map(|m| manifest_deps(m, ws_deps)) } else { None };
        for d in deps {
            let Some(j) = resolve(d) else {
                errs.push(format!("Cargo.lock: {name} -> `{d}` resolves to no single package"));
                continue;
            };
            match &declared {
                None => ship[i].push(j),
                Some((s, _)) if s.contains(&pkgs[j].0) => ship[i].push(j),
                Some((_, v)) if v.contains(&pkgs[j].0) => dev[i].push(j),
                Some(_) => errs.push(format!("Cargo.lock edge {name} -> {d}: no section of {name}'s manifest declares it")),
            }
        }
    }
    let names = pkgs.into_iter().map(|p| p.0).collect();
    if errs.is_empty() { Ok(DepGraph { names, ship, dev }) } else { Err(errs) }
}

impl DepGraph {
    /// Every package reachable from `root` (shipped edges, plus dev edges
    /// when `with_dev`), each mapped to the package it was reached from.
    fn closure(&self, root: &str, with_dev: bool) -> BTreeMap<String, Option<String>> {
        let r = self.names.iter().position(|n| n == root).unwrap_or_else(|| panic!("{root} is not in Cargo.lock"));
        let mut seen: BTreeMap<usize, Option<usize>> = BTreeMap::from([(r, None)]);
        let mut stack = vec![r];
        while let Some(x) = stack.pop() {
            let dev = if with_dev { self.dev[x].as_slice() } else { &[] };
            for &y in self.ship[x].iter().chain(dev) {
                if let std::collections::btree_map::Entry::Vacant(e) = seen.entry(y) {
                    e.insert(Some(x));
                    stack.push(y);
                }
            }
        }
        seen.into_iter().map(|(k, v)| (self.names[k].clone(), v.map(|v| self.names[v].clone()))).collect()
    }
}

/// `root -> … -> pkg`, read back off a closure's parent map.
fn chain(c: &BTreeMap<String, Option<String>>, pkg: &str) -> String {
    let mut out = vec![pkg.to_string()];
    let mut at = pkg.to_string();
    while let Some(Some(p)) = c.get(&at) {
        out.push(p.clone());
        at = p.clone();
    }
    out.reverse();
    out.join(" -> ")
}

/// **RV1.S1d (RV1.S1-I F1): THE SKIP LIST IS CHECKED, NOT TRUSTED.** Every
/// `OUT_OF_REACH` member is excluded from the http-dependency and dial scans
/// above; this proves, from `Cargo.lock` and the members' manifests, that no
/// such member is in the normal + build dependency closure of `tui` (the face)
/// or `cauldron` (the CLI), and that every one reached through a dev edge is
/// named in `DEV_ONLY_REACH`. Opposed on a planted lock: a renamed and a
/// target-scoped shipped edge ARE found, a dev-only edge is NOT, and a lock
/// edge no manifest section declares is an error, not a silent skip.
#[test]
fn rv1s1d_no_out_of_reach_member_is_in_the_face_or_cli_dependency_closure() {
    let planted_lock = "version = 3\n\
        [[package]]\nname = \"face\"\nversion = \"0.1.0\"\ndependencies = [\"devtool\", \"lib\", \"web 1.0.0\"]\n\
        [[package]]\nname = \"lib\"\nversion = \"0.1.0\"\ndependencies = [\"daemon\"]\n\
        [[package]]\nname = \"daemon\"\nversion = \"0.1.0\"\n\
        [[package]]\nname = \"devtool\"\nversion = \"0.1.0\"\ndependencies = [\"daemon2\"]\n\
        [[package]]\nname = \"daemon2\"\nversion = \"0.1.0\"\n\
        [[package]]\nname = \"web\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
        [[package]]\nname = \"web\"\nversion = \"2.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
    let face = "[package]\nname = \"face\"\n[dependencies]\nlib = { path = \"../lib\" }\n\
        [target.'cfg(unix)'.dependencies]\nweb = \"1\"\n[dev-dependencies]\ndevtool = { path = \"../devtool\" }\n";
    let lib = "[package]\nname = \"lib\"\n[dependencies]\nd = { package = \"daemon\", path = \"../daemon\" }\n";
    let none = toml::Table::new();
    let mut ms = BTreeMap::from([("face".to_string(), face.to_string()), ("lib".to_string(), lib.to_string())]);
    let g = dep_graph(planted_lock, &ms, &none).expect("planted graph");
    let (s, a) = (g.closure("face", false), g.closure("face", true));
    assert!(s.contains_key("daemon") && s.contains_key("web"), "planted: the renamed / target-scoped shipped edges are missed: {s:?}");
    assert_eq!(chain(&s, "daemon"), "face -> lib -> daemon");
    assert!(!s.contains_key("daemon2") && a.contains_key("daemon2"), "planted: the dev-only edge is misclassified: {s:?} / {a:?}");
    ms.insert("face".into(), face.replace("lib = { path = \"../lib\" }\n", ""));
    assert!(dep_graph(planted_lock, &ms, &none).is_err(), "planted: an undeclared lock edge must be an error");

    // RV1.M4-Q2: two same-named, same-versioned packages from different git
    // sources. Each reference resolves to its own package by source: the one
    // `lib` ships reaches the face's shipped closure (with ITS dependency), the
    // one only `devtool` carries does not.
    let twin_lock = "version = 3\n\
        [[package]]\nname = \"face\"\nversion = \"0.1.0\"\ndependencies = [\"devtool\", \"lib\"]\n\
        [[package]]\nname = \"lib\"\nversion = \"0.1.0\"\ndependencies = [\"pl 0.1.0 (git+https://a/pl?rev=1)\"]\n\
        [[package]]\nname = \"devtool\"\nversion = \"0.1.0\"\ndependencies = [\"pl 0.1.0 (git+file:///b/pl?rev=2)\"]\n\
        [[package]]\nname = \"pl\"\nversion = \"0.1.0\"\nsource = \"git+file:///b/pl?rev=2#2222\"\ndependencies = [\"daemon2\"]\n\
        [[package]]\nname = \"pl\"\nversion = \"0.1.0\"\nsource = \"git+https://a/pl?rev=1#1111\"\ndependencies = [\"daemon\"]\n\
        [[package]]\nname = \"daemon\"\nversion = \"0.1.0\"\n\
        [[package]]\nname = \"daemon2\"\nversion = \"0.1.0\"\n";
    let twin_face = "[package]\nname = \"face\"\n[dependencies]\nlib = { path = \"../lib\" }\n[dev-dependencies]\ndevtool = { path = \"../devtool\" }\n";
    let twin_ms = BTreeMap::from([
        ("face".to_string(), twin_face.to_string()),
        ("lib".to_string(), "[package]\nname = \"lib\"\n[dependencies]\npl = { git = \"https://a/pl\" }\n".to_string()),
        ("devtool".to_string(), "[package]\nname = \"devtool\"\n[dependencies]\npl = { git = \"file:///b/pl\" }\n".to_string()),
    ]);
    let g = dep_graph(twin_lock, &twin_ms, &none).unwrap_or_else(|e| panic!("planted twins: {e:?}"));
    let (s, a) = (g.closure("face", false), g.closure("face", true));
    assert!(s.contains_key("pl") && s.contains_key("daemon"), "planted twins: the shipped twin's closure is missed: {s:?}");
    assert!(!s.contains_key("daemon2") && a.contains_key("daemon2"), "planted twins: the dev-only twin leaked into the shipped closure: {s:?} / {a:?}");
    // A source-less reference to the twinned name is ambiguous: an error, never a guess.
    let ambiguous = twin_lock.replace("\"pl 0.1.0 (git+https://a/pl?rev=1)\"", "\"pl 0.1.0\"");
    assert!(dep_graph(&ambiguous, &twin_ms, &none).is_err(), "planted twins: an ambiguous reference must be an error");

    let ws = workspace();
    let root: toml::Table = std::fs::read_to_string(ws.join("Cargo.toml")).unwrap().parse().unwrap();
    let ws_deps = root.get("workspace").and_then(|w| w.get("dependencies")).and_then(|d| d.as_table()).cloned().unwrap_or_default();
    let mut manifests = BTreeMap::new();
    let mut pkg_of = BTreeMap::new();
    for m in members(&ws) {
        let text = std::fs::read_to_string(ws.join(&m).join("Cargo.toml")).unwrap();
        let t: toml::Table = text.parse().unwrap();
        let name = t.get("package").and_then(|p| p.get("name")).and_then(|n| n.as_str()).unwrap_or_else(|| panic!("{m}: no package name")).to_string();
        pkg_of.insert(m, name.clone());
        manifests.insert(name, text);
    }
    let lock = std::fs::read_to_string(ws.join("Cargo.lock")).unwrap();
    let g = dep_graph(&lock, &manifests, &ws_deps).unwrap_or_else(|e| panic!("the workspace graph:\n{}", e.join("\n")));
    let mut wrong = Vec::new();
    let mut dev_reached = BTreeSet::new();
    for face in ["tui", "cauldron"] {
        let (ship, all) = (g.closure(face, false), g.closure(face, true));
        // SENTINEL (doctrine 9): the shipped closure is a real one — it holds
        // the choke point's crate and dozens of packages (`cargo tree -e
        // normal,build` at RV1.S1d: 67 distinct names for cauldron, 218 for tui),
        // and the dev closure is strictly larger (the dev edges are seen).
        assert!(ship.len() > 50 && ship.contains_key("env"), "sentinel: {face}'s shipped closure is {} packages", ship.len());
        assert!(all.len() > ship.len(), "sentinel: {face}'s dev edges add nothing ({} packages)", all.len());
        for (path, why) in OUT_OF_REACH {
            let p = pkg_of.get(*path).unwrap_or_else(|| panic!("stale out-of-reach member {path} ({why})"));
            if ship.contains_key(p) {
                wrong.push(format!("{path} (`{p}`) IS in {face}'s shipped dependency closure: {} — it is not out of reach ({why})", chain(&ship, p)));
            } else if all.contains_key(p) {
                dev_reached.insert(p.clone());
                if !DEV_ONLY_REACH.iter().any(|(d, ..)| d == p) {
                    wrong.push(format!("{path} (`{p}`) reaches {face} through dev-dependencies ({}) and no DEV_ONLY_REACH row names it", chain(&all, p)));
                }
            }
        }
    }
    for (pkg, from, why) in DEV_ONLY_REACH {
        assert!(pkg_of.values().any(|n| n == pkg) && OUT_OF_REACH.iter().any(|(p, _)| pkg_of.get(*p).is_some_and(|n| n == pkg)), "{pkg} is not an OUT_OF_REACH member ({why})");
        let (s, d) = manifest_deps(manifests.get(*from).unwrap_or_else(|| panic!("{from} is not a member")), &ws_deps);
        if !d.contains(*pkg) || s.contains(*pkg) {
            wrong.push(format!("DEV_ONLY_REACH {pkg}: {from} does not carry it as a dev-dependency only ({why})"));
        }
        if !dev_reached.contains(*pkg) {
            wrong.push(format!("stale DEV_ONLY_REACH {pkg}: neither face reaches it, even through dev edges ({why})"));
        }
    }
    assert!(wrong.is_empty(), "OUT_OF_REACH does not hold:\n{}", wrong.join("\n"));
}

/// **F3, SHIPPED: NO DIAL API AND NO DAEMON-CONTROL SPAWN OUTSIDE THE
/// CHOKE POINT.** Scans the shipped half of every `src/**/*.rs` of every
/// workspace member in reach. Opposed: the detector fires on each needle and
/// not on a comment; the scan finds the choke point itself (a non-empty
/// search).
#[test]
fn rel6a5_no_dial_or_control_api_outside_the_choke_point() {
    let mut all = dial_needles();
    all.extend(control_needles());
    let probe = format!("let s = std::net::{}(a);\n// {}(b)\n", concat!("TcpStream::", "connect"), concat!("connect_", "async"));
    assert_eq!(find(&probe, &all).len(), 1, "the detector, opposed: one code site, one comment");

    let ws = workspace();
    let mut counts: BTreeMap<(String, &str), usize> = BTreeMap::new();
    let mut texts: BTreeMap<String, String> = BTreeMap::new();
    let mut scanned = 0;
    for m in members(&ws) {
        if OUT_OF_REACH.iter().any(|(p, _)| *p == m) {
            continue;
        }
        let mut files = Vec::new();
        rs_files(&ws.join(&m).join("src"), &mut files);
        for f in files {
            let rel = f.strip_prefix(&ws).unwrap().display().to_string();
            let text = std::fs::read_to_string(&f).unwrap();
            scanned += 1;
            let (shipped, _) = split_cfg_test(&text);
            for (_, n) in find(&shipped, &all) {
                *counts.entry((rel.clone(), n)).or_default() += 1;
            }
            texts.insert(rel, text);
        }
    }
    assert!(scanned > 100, "sentinel: only {scanned} src files scanned");
    // SENTINEL (doctrine 9): the shipped text of cli/src/main.rs reaches PAST
    // its first `#[cfg(test)]` (~line 1,800): its connect_tcp calls in
    // outside_state and launch_hosted (and ask_supervisor) all sit after it.
    let main = texts.get("cli/src/main.rs").expect("cli/src/main.rs scanned");
    let (main_shipped, _) = split_cfg_test(main);
    let first_test = main.find(concat!("#[cfg(", "test)]")).map(|i| main[..i].lines().count()).unwrap_or(0);
    let late = find(&main_shipped, &[concat!("egress::", "connect_tcp(")]).into_iter().filter(|(l, _)| *l > first_test).count();
    assert!(first_test > 0 && late >= 2, "the shipped scan does not reach past line {first_test} of cli/src/main.rs ({late} late connect_tcp)");
    let mut wrong = Vec::new();
    for ((file, needle), n) in &counts {
        match SHIPPED_ALLOWED.iter().find(|(f, nd, ..)| f == file && nd == needle) {
            None => wrong.push(format!("{file}: {n} × `{needle}` outside the choke point (route it through env::egress / live::dial_ws, or allow-list it with a reason)")),
            Some((_, _, want, _, why)) if want != n => wrong.push(format!("{file}: {n} × `{needle}`, the allow-list says {want} ({why})")),
            _ => {}
        }
    }
    for (file, needle, want, gate, why) in SHIPPED_ALLOWED {
        if !counts.contains_key(&(file.to_string(), *needle)) {
            wrong.push(format!("stale allowance: {file} has no `{needle}` (want {want}: {why})"));
        }
        if !texts.get(*file).is_some_and(|t| t.contains(gate)) {
            wrong.push(format!("{file}: its gate `{gate}` is gone ({why})"));
        }
    }
    for (m, why) in OUT_OF_REACH {
        assert!(members(&ws).iter().any(|x| x == m), "stale out-of-reach member {m} ({why})");
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Test sources that may spawn the face / the CLI / a control or external
/// binary, by workspace path prefix, with why.
const TEST_SPAWN_ALLOWED: &[(&str, &str)] = &[
    ("apps/tui/tests/it/face.rs", "THE face helper: forces the isolation set at every spawn"),
    ("apps/tui/tests/it/drive_isolation.rs", "detector data and fixtures only; it spawns pty_drive.py against a stub"),
    (
        "cli/tests/",
        "the CLI's own suite runs the built `cauldron` under a scratch CAULDRON_HOME/HOME; its `it` binary exports \
         CAULDRON_ISOLATED (test_isolation::install!, checked below), so each child refuses his root, his launchd \
         domain and his live ports itself; own_launch_path.rs's `security find-identity` is read-only",
    ),
    ("libs/projects/tests/", "runs the `projects` registry CLI under a scratch CAULDRON_HOME; it controls no daemon and dials nothing"),
    ("cli/src/launchd.rs", "test half: a `bin/cauldron` path in an assertion MESSAGE (install must not overwrite prod's bin); spawns nothing"),
    ("libs/env/src/cli_config.rs", "test half: `.../bin/cauldron` PATH DATA for the CLI-config parser; spawns nothing"),
    ("libs/env/src/egress.rs", "test half: `/Users/x/bin/cauldron` as exe_is_test_harness's negative row; spawns nothing"),
    ("cli/src/bin/cauldron-launcher.rs", "test half (REL-6a6): `…/bin/cauldron` PATH DATA for admit_forward's refusal rows; spawns nothing"),
    (
        "services/",
        "the daemons' own suites start their own binary on ephemeral ports (and read keychain identities read-only); \
         outside the face/CLI reach — the weaker ones are listed in REL-6a5's report",
    ),
];

/// A dial line in a test source that names a live port.
fn live_port_dials(text: &str) -> Vec<usize> {
    let dials = dial_needles();
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim_start().starts_with("//"))
        .filter(|(_, l)| dials.iter().any(|n| l.contains(n)) && live::PROD_PORTS.iter().any(|p| l.contains(&format!(":{p}"))))
        .map(|(i, _)| i + 1)
        .collect()
}

/// **F3, TESTS: EVERY TEST SOURCE IN THE WORKSPACE** — every member's
/// `tests/**` and the `#[cfg(test)]` half of every `src/**` — is scanned for
/// a spawn of the face or the CLI (the REL-1.S1c detector), a daemon-control
/// or external binary, and a dial written against a live port. Opposed:
/// the scan reaches each allowed file and finds sites in it (a non-empty
/// search), and the detector fires on a planted line and not on a comment.
#[test]
fn rel6a5_every_test_source_in_the_workspace_spawns_and_dials_only_where_named() {
    let me = "apps/tui/tests/it/egress_guard.rs";
    let planted = format!("let c = Command::new(\"{}\");\nTcpStream::{}(\"127.0.0.1:3653\");\n", concat!("/bin/", "launchctl"), "connect");
    assert_eq!(find(&planted, &control_needles()).len(), 1, "the control detector, planted");
    assert_eq!(live_port_dials(&planted), vec![2], "the live-dial detector, planted");
    assert_eq!(live_port_dials("// TcpStream::connect(\"127.0.0.1:3653\")\n"), Vec::<usize>::new());

    let ws = workspace();
    let mut wrong = Vec::new();
    let mut hits: BTreeMap<&str, usize> = BTreeMap::new();
    let mut files_seen = 0;
    for m in members(&ws) {
        let mut files = Vec::new();
        rs_files(&ws.join(&m).join("tests"), &mut files);
        let mut srcs = Vec::new();
        rs_files(&ws.join(&m).join("src"), &mut srcs);
        for (f, is_src) in files.into_iter().map(|f| (f, false)).chain(srcs.into_iter().map(|f| (f, true))) {
            let rel = f.strip_prefix(&ws).unwrap().display().to_string();
            if rel == me {
                continue;
            }
            let full = std::fs::read_to_string(&f).unwrap();
            let (_, test) = split_cfg_test(&full);
            let (text, off) = if is_src { (test.as_str(), 0) } else { (full.as_str(), 0) };
            if text.is_empty() {
                continue;
            }
            files_seen += 1;
            let mut found: Vec<String> =
                crate::drive_isolation::face_spawn_sites(text).into_iter().map(|(l, s)| format!("{rel}:{}: {s}", l + off)).collect();
            found.extend(find(text, &control_needles()).into_iter().map(|(l, n)| format!("{rel}:{}: `{n}`", l + off)));
            for l in live_port_dials(text) {
                wrong.push(format!("{rel}:{}: a dial written against a live port", l + off));
            }
            if found.is_empty() {
                continue;
            }
            match TEST_SPAWN_ALLOWED.iter().find(|(p, _)| rel.starts_with(p)) {
                Some((p, _)) => *hits.entry(p).or_default() += found.len(),
                None => wrong.extend(found),
            }
        }
    }
    assert!(files_seen > 200, "sentinel: only {files_seen} test sources scanned");
    for (p, why) in TEST_SPAWN_ALLOWED {
        if !hits.contains_key(p) {
            wrong.push(format!("stale allowance {p}: no site found ({why})"));
        }
    }
    assert!(wrong.is_empty(), "test sources that spawn or dial outside the named places:\n{}", wrong.join("\n"));
}

/// The CLI's test binary exports the marker to every `cauldron` it spawns
/// (REL-6a5), as the face's four do (drive_isolation checks those).
#[test]
fn rel6a5_the_cli_test_binary_exports_the_isolation_marker() {
    let ws = workspace();
    let root = std::fs::read_to_string(ws.join("cli/tests/it/main.rs")).unwrap();
    let call = concat!("test_isolation::", "install!()");
    assert!(
        root.lines().any(|l| !l.trim_start().starts_with("//") && l.contains(call)),
        "cli/tests/it/main.rs does not install the isolation marker"
    );
    let manifest = std::fs::read_to_string(ws.join("cli/Cargo.toml")).unwrap();
    assert!(manifest.contains("test-isolation"), "cli does not depend on test-isolation");
}
