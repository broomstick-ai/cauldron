//! `tui --cauldron` — the Cauldron face (Cauldron, Threads, Status, Config). The
//! Threads tab reads the real threads daemon through the face's one connection
//! (`live`, REL-6); the other tabs are still on fake data.
//!
//! `--cauldron` is stripped by `main.rs` and every other argument arrives here.
//! The environment is resolved by the `env` crate exactly as every `cauldron`
//! verb resolves it (`cauldron --env dev tui --cauldron` works through the
//! router's `CAULDRON_ENV`); `--env <name>`, or a bare positional name, picks
//! one directly.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context};
use ratatui::crossterm::event;

use super::app::{resolve_env, App, CTab, Focus, KTab, Modal, Outbox, KEYMAP};
use super::{render_to_text, view};

const USAGE: &str = "\
tui --cauldron — the Cauldron face (Threads is live; the rest is fake data); `cauldron tui` opens it

    tui --cauldron [--env <name> | <name>] [--seeds <dir>]
    tui --cauldron --dump [--env <name>] [--tab <tab>] [--cluster <id>] [--cluster-tab <tab>]
                       [--status-tab <tab>]
                       [--aggregate] [--search <text>] [--region <id>] [--node <id>]
                       [--item <n>] [--tag] [--draft <text>]... [--input <text>]...
                       [--note <text>]... [--quote <tag>]... [--paste <text>]... [--dump-collapsed]
                       [--send]
                       [--modal cauldrons|new|new-cluster|palette|help|enable|paste|add-endpoint] [--new-name <text>]
                       [--parent <id>] [--boundaries <text>]
                       [--settings <path>] [--bus <page>]
                       [--keys <keys>] [--size <W>x<H>] [--scroll <n>]
                       [--thread <id>] [--agent <id>] [--settle <ms>]

  --env          the cauldron environment (default: $CAULDRON_ENV, $ENV, else the last opened,
                 else prod, else the first; with none at all, the first-run screen)
  --seeds        a fixture seed directory (tests only; default: none — an
                 environment shows only its own data)
  --dump         render ONE frame headlessly and print it; nothing is written to disk
  --tab          the environment bar: cauldron | threads | status | permissions | settings
                 the global bar: tui | devices | providers | accounts
  --cluster      the cluster to focus, by its minted id (`X-2`; default: the first)
  --cluster-tab  questions | actions | tasks | nodes | status | details (tasks: the execution graph)
  --status-tab   ledger | tokens | mesh | soup | bus (implies --tab status)
  --config-tab   DEPRECATED, accepted for this release only (Config is the global bar now):
                 cauldron = --tab settings; tui | devices | providers | permissions = that --tab
  --thread       open this thread on the Threads tab (its id or name; implies --tab threads)
  --agent        with --thread: show this subagent's transcript (its thread id)
  --settle       with --dump on the Threads tab: wait at most this long (ms, default 3000) for
                 the threads daemon to answer before the frame is drawn
  --aggregate    include descendants' items on the page
  --search       search the cluster's nodes (implies --cluster-tab nodes)
  --region       open this region on the cluster's Nodes tab
  --node         open this node, after --region if both
  --item         select item n (0-based): on a cluster's page, or a row of Status or a settings tab
  --tag          quote the selected item into the bucket, as `i` would
  --draft        type this text into the bucket (repeatable; not dumped)
  --note         a new note at the end of the bucket: a blank line, then this text (repeatable)
  --quote        `i` on this tag: a new note anchored to it (`--quote Q-1`)
  --paste        a paste chip at the caret holding this text
  --dump-collapsed   the bucket does not hold the selection
  --input        type this text into the bucket and dump it, as Enter would (repeatable)
  --send         with --dump: SEND the bucket the flags above built (--draft, --note, --quote,
                 --paste, --tag) through the send key's own function, append it to this
                 environment's dump log, and print that one log line instead of a frame. It
                 starts from an empty bucket (never his draft), writes the dump and its paste
                 bodies and nothing else, and is refused on cauldron-build (the demo is
                 read-only), on an empty bucket, with --seeds, --input or --keys, and without
                 --dump. `cauldron ctl dump` runs it (RV1.S12a)
  --modal        open the cauldrons dropdown, a modal naming a new cauldron or cluster, the key
                 palette, (help) what the selected setting means, (enable) the confirm that disables
                 or enables the cauldron, (paste) the bucket's first paste, or (add-endpoint) the
                 add modal of a global inference endpoint, on Providers
  --new-name     the name typed into the naming modal
  --parent       with --modal new-cluster: the cluster it is made under
  --boundaries   with --modal new-cluster: its boundary conditions, typed into the modal
  --settings     open a settings tab's bands by their path, as → would on each, selecting the
                 last: accounts/claude_code, providers/endpoints, cauldron/accounts (cauldron is
                 Settings); a last slug naming a table's row opens its modal: claude_code/acct2,
                 providers/endpoints/ep-1
  --bus          open the bus modal on a page (implies --status-tab bus): event:<id> | ripple:<id>
                 | trigger:<ripple>/<n> (n from 1) | run:<id>
  --keys         press these keys, in order, after everything above and before --tag:
                 space-separated, each one of up down left right enter esc tab home end
                 opt-up opt-down space, or a single character (`--keys 'right down right'`)
  --size         frame size for --dump (default 110x48)
  --scroll       page scroll offset for --dump

environment
  TUI_THREADS_CHASSIS_PORT  the threads daemon the Threads tab reads (default: the one this
                   environment resolves to, as `tui` resolves it)
  CAULDRON_OPENER  what `o` hands a path to: unset or empty = the system `open`; `none` = nothing
                   is run and the notice says `open (suppressed): <path>`; anything else = that
                   program, run with the path as its one argument. Every test, audit and pty check
                   sets `none` (IS-111). A --dump never opens anything, whatever it says.

keys (the philosophy: docs/navigation.md)
";

/// The usage text, with every key binding from [`KEYMAP`].
fn usage() -> String {
    let w = KEYMAP.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0) + 3;
    let mut s = USAGE.to_string();
    for (k, what) in KEYMAP {
        let pad = w.saturating_sub(k.chars().count());
        s.push_str(&format!("  {k}{}{what}\n", " ".repeat(pad)));
    }
    s
}

struct Args {
    env: Option<String>,
    /// A fixture seed directory (`--seeds`); none on a real environment.
    seeds: Option<PathBuf>,
    dump: bool,
    tab: Option<CTab>,
    cluster: Option<String>,
    cluster_tab: Option<KTab>,
    status_tab: Option<super::status::StatusTab>,
    config_tab: Option<super::menu::CfgTab>,
    aggregate: bool,
    search: Option<String>,
    region: Option<String>,
    node: Option<String>,
    item: Option<usize>,
    tag: bool,
    modal: Option<String>,
    new_name: Option<String>,
    inputs: Vec<String>,
    size: (u16, u16),
    scroll: usize,
    settings: Option<String>,
    parent: Option<String>,
    boundaries: Option<String>,
    bus: Option<super::bus::BusPage>,
    keys: Vec<event::Event>,
    /// The bucket's flags, in the order given.
    dump_ops: Vec<DumpOp>,
    /// `--send` (RV1.S12a): send the bucket the flags built, through the
    /// send key's function, and print the appended dump.
    send: bool,
    dump_collapsed: bool,
    thread: Option<String>,
    agent: Option<String>,
    settle: Duration,
}

/// One bucket flag (`--draft`, `--note`, `--quote`, `--paste`), applied by
/// [`apply_dump_ops`] in the order given.
#[derive(Debug, Clone)]
pub enum DumpOp {
    Draft(String),
    Note(String),
    Quote(String),
    Paste(String),
}

fn parse(argv: Vec<String>) -> anyhow::Result<Args> {
    let mut a = Args {
        env: None,
        seeds: None,
        dump: false,
        tab: None,
        cluster: None,
        cluster_tab: None,
        status_tab: None,
        config_tab: None,
        aggregate: false,
        search: None,
        region: None,
        node: None,
        item: None,
        tag: false,
        modal: None,
        new_name: None,
        inputs: Vec::new(),
        size: (110, 48),
        scroll: 0,
        settings: None,
        parent: None,
        boundaries: None,
        bus: None,
        keys: Vec::new(),
        dump_ops: Vec::new(),
        send: false,
        dump_collapsed: false,
        thread: None,
        agent: None,
        settle: Duration::from_millis(3000),
    };
    let mut it = argv.into_iter();
    while let Some(arg) = it.next() {
        let mut val = |name: &str| it.next().with_context(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--env" => a.env = Some(val("--env")?),
            "--seeds" | "--data" => a.seeds = Some(PathBuf::from(val("--seeds")?)),
            "--dump" => a.dump = true,
            "--tab" => {
                let v = val("--tab")?;
                if let Some(now) = CTab::renamed(&v) {
                    bail!("`--tab {v}`: that tab is called {} now (msg 457, REL-6)", now.label().to_lowercase());
                }
                if v.eq_ignore_ascii_case("config") {
                    bail!("`--tab config`: Config is the global bar now (V1.1) — --tab tui | devices | providers | accounts; this cauldron's own page is --tab settings");
                }
                a.tab = Some(CTab::parse(&v).with_context(|| {
                    format!("no tab `{v}` (cauldron | threads | status | permissions | settings | tui | devices | providers | accounts)")
                })?);
            }
            "--cluster" => a.cluster = Some(val("--cluster")?),
            "--cluster-tab" => {
                let v = val("--cluster-tab")?;
                a.cluster_tab = Some(KTab::parse(&v).with_context(|| format!("no cluster tab `{v}`"))?);
            }
            "--status-tab" => {
                let v = val("--status-tab")?;
                a.status_tab = Some(super::status::StatusTab::parse(&v).with_context(|| format!("no Status sub-tab `{v}`"))?);
            }
            "--config-tab" => {
                let v = val("--config-tab")?;
                // V1.1: kept one release, its old words mapped to the tabs
                // that show those pages (`accounts` was never one of them).
                a.config_tab = Some(
                    super::menu::CfgTab::parse(&v)
                        .filter(|t| *t != super::menu::CfgTab::Accounts)
                        .with_context(|| format!("no --config-tab `{v}` (cauldron | tui | devices | providers | permissions; deprecated: use --tab)"))?,
                );
            }
            "--modal" => a.modal = Some(val("--modal")?),
            "--new-name" => a.new_name = Some(val("--new-name")?),
            "--aggregate" => a.aggregate = true,
            "--search" => a.search = Some(val("--search")?),
            "--region" => a.region = Some(val("--region")?),
            "--node" => a.node = Some(val("--node")?),
            "--item" => a.item = Some(val("--item")?.parse().context("--item")?),
            "--tag" => a.tag = true,
            "--draft" => a.dump_ops.push(DumpOp::Draft(val("--draft")?)),
            "--note" => a.dump_ops.push(DumpOp::Note(val("--note")?)),
            "--quote" => a.dump_ops.push(DumpOp::Quote(val("--quote")?)),
            "--paste" => a.dump_ops.push(DumpOp::Paste(val("--paste")?)),
            "--dump-collapsed" => a.dump_collapsed = true,
            "--send" => a.send = true,
            "--thread" => a.thread = Some(val("--thread")?),
            "--agent" => a.agent = Some(val("--agent")?),
            "--settle" => a.settle = Duration::from_millis(val("--settle")?.parse().context("--settle")?),
            "--input" => a.inputs.push(val("--input")?),
            "--settings" => a.settings = Some(val("--settings")?),
            "--parent" => a.parent = Some(val("--parent")?),
            "--boundaries" => a.boundaries = Some(val("--boundaries")?),
            "--bus" => {
                let v = val("--bus")?;
                a.bus = Some(bus_page(&v).with_context(|| format!("--bus: `{v}` is not event:<id> | ripple:<id> | trigger:<ripple>/<n> | run:<id>"))?);
            }
            "--keys" => {
                for k in val("--keys")?.split_whitespace() {
                    a.keys.push(key_event(k)?);
                }
            }
            "--scroll" => a.scroll = val("--scroll")?.parse().context("--scroll")?,
            "--size" => {
                let v = val("--size")?;
                let (w, h) = v.split_once('x').context("--size is WxH")?;
                a.size = (w.parse().context("--size width")?, h.parse().context("--size height")?);
            }
            "-h" | "--help" => {
                print!("{}", usage());
                std::process::exit(0);
            }
            name if !name.starts_with('-') && a.env.is_none() => a.env = Some(name.to_string()),
            other => bail!("unknown argument `{other}`\n\n{}", usage()),
        }
    }
    Ok(a)
}

/// **WHAT `--send` MAY BE COMBINED WITH** (RV1.S12a). It is the face's one
/// headless WRITE, so it runs only inside a `--dump` (a run with a terminal
/// is his face, whose send is the key); it is refused beside a fixture
/// (`--seeds`, which is not the environment's log), beside `--input` (which
/// sends dumps of its own) and beside `--keys` (whose Enter would send one):
/// exactly one dump, built by the bucket flags alone.
fn validate(a: &Args) -> anyhow::Result<()> {
    if !a.send {
        return Ok(());
    }
    if !a.dump {
        bail!("--send is valid only with --dump: it sends the bucket a headless dump built (his face sends with the key)");
    }
    if a.seeds.is_some() {
        bail!("--send writes the environment's own dump log; a fixture (--seeds) is not one");
    }
    if !a.inputs.is_empty() {
        bail!("--send sends exactly one dump; --input sends dumps of its own");
    }
    if !a.keys.is_empty() {
        bail!("--send sends exactly one dump, built by --draft/--note/--quote/--paste/--tag; --keys could press Enter");
    }
    Ok(())
}

/// A `--bus` page: `event:EV-1`, `ripple:dump`, `trigger:dump/1`, `run:r-8f3a`.
fn bus_page(v: &str) -> Option<super::bus::BusPage> {
    use super::bus::BusPage;
    let (kind, id) = v.split_once(':')?;
    Some(match kind {
        "event" => BusPage::Event(id.to_string()),
        "ripple" => BusPage::Ripple(id.to_string()),
        "run" => BusPage::Run(id.to_string()),
        "trigger" => {
            let (r, n) = id.split_once('/')?;
            BusPage::Trigger { ripple: r.to_string(), trigger: n.parse::<usize>().ok()?.checked_sub(1)?, event: None }
        }
        _ => return None,
    })
}

/// One `--keys` token as the key event a terminal would deliver.
fn key_event(k: &str) -> anyhow::Result<event::Event> {
    use event::{KeyCode as C, KeyModifiers as M};
    let (code, m) = match k {
        "up" => (C::Up, M::NONE),
        "down" => (C::Down, M::NONE),
        "left" => (C::Left, M::NONE),
        "right" => (C::Right, M::NONE),
        "enter" => (C::Enter, M::NONE),
        "esc" => (C::Esc, M::NONE),
        "tab" => (C::Tab, M::NONE),
        "home" => (C::Home, M::NONE),
        "end" => (C::End, M::NONE),
        "space" => (C::Char(' '), M::NONE),
        "opt-up" => (C::Up, M::ALT),
        "opt-down" => (C::Down, M::ALT),
        // `f1`..`f12`: the cauldron bar's tabs (REL-6d).
        s if s.len() >= 2 && s.starts_with('f') && s[1..].parse::<u8>().is_ok_and(|n| (1..=12).contains(&n)) => (C::F(s[1..].parse().expect("checked")), M::NONE),
        // `ctrl-e`: a control chord (REL-6d: ^E opens the cauldrons dropdown).
        s if s.starts_with("ctrl-") && s.chars().count() == 6 => (C::Char(s.chars().nth(5).expect("one char")), M::CONTROL),
        s if s.chars().count() == 1 => (C::Char(s.chars().next().expect("one char")), M::NONE),
        other => bail!("--keys: no key `{other}`"),
    };
    Ok(event::Event::Key(event::KeyEvent::new(code, m)))
}

/// **A typo'd environment is never made silently** (audit 0118 F13). An
/// environment EXISTS when its data root does — the `env` crate's rule,
/// [`env::Environment::environments`] — and opening the face on a name that
/// does not exist would make it on the first write. So a name he typed (the
/// flag, a bare name, or `$CAULDRON_ENV` from `cauldron --env`) that does not
/// exist is asked about on a terminal and refused anywhere else (`--dump`, a
/// pipe). `prod` is no longer exempt (V1.2, Q-F1): it is never made behind his
/// back either. A name nobody typed is always one that exists
/// ([`super::store::default_env`]).
fn confirm_environment(env: &env::Environment, named: bool, dump: bool) -> anyhow::Result<()> {
    use std::io::{BufRead, IsTerminal, Write};
    let there = env.environments()?;
    if !named || there.contains(&env.name) {
        return Ok(());
    }
    let list = if there.is_empty() { "none yet".to_string() } else { there.join(" · ") };
    if dump || !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        bail!("no environment `{}` ({list})", env.name);
    }
    let mut err = std::io::stderr();
    write!(err, "no environment `{}` ({list}). make it? [y/N] ", env.name)?;
    err.flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    if matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
        return Ok(());
    }
    bail!("environment `{}` not made", env.name)
}

/// Whether the PROCESS named the environment (`$CAULDRON_ENV`, `$ENV`), not
/// the default. `cauldron tui` always hands the face `CAULDRON_ENV`, so it
/// also says where the name came from in `CAULDRON_ENV_SOURCE`: `default`
/// there means nobody named it (V1.2).
fn named_by_process() -> bool {
    let defaulted = std::env::var("CAULDRON_ENV_SOURCE").is_ok_and(|s| s == env::paths::Source::Default.as_str());
    !defaulted && env::Environment::resolve().is_ok_and(|e| e.name_source != env::paths::Source::Default)
}

/// Run the soup face with `argv` (the process arguments, `--cauldron` removed).
pub fn main(argv: Vec<String>) -> anyhow::Result<()> {
    let args = parse(argv)?;
    validate(&args)?;
    // **RULE A + B (REL-6a3).** A `--dump`, or a face with no terminal on
    // stdin/stdout, is not his interactive face: the whole process is marked
    // non-interactive (every connection constructor then refuses his ports,
    // and every child inherits CAULDRON_ISOLATED=1), and its launch door is
    // the headless one, which never falls back to the environment's daemon —
    // its Threads tab needs TUI_THREADS_CHASSIS_PORT, or the dump exits
    // non-zero having dialled nothing (below).
    let scripted = {
        use std::io::IsTerminal;
        args.dump || !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal()
    };
    if scripted {
        super::live::mark_non_interactive(if args.dump { "tui --cauldron --dump" } else { "tui --cauldron (no terminal)" });
    }
    // MERGE-V1i: the process is marked non-interactive BEFORE anything
    // resolves, so the first-run screen (V1.2, below) inherits it too.
    let base = resolve_env(args.env.as_deref())?;
    let named = args.env.is_some() || named_by_process();
    // V1.2: a root with NO environment opens on the first-run screen,
    // whatever name resolved — `prod` included — and makes nothing. REL-3.S2:
    // a named fixture (`--seeds`, tests only) is explicit data to show, so it
    // bypasses the first-run gate exactly as it bypasses `confirm_environment`
    // below — nothing real need exist on disk for a fixture to be drawn.
    if args.seeds.is_none() && base.environments()?.is_empty() {
        if args.send {
            bail!("--send: this root has no environment to dump into");
        }
        let seeds = super::store::no_seeds(&base);
        let mut app = App::open_first_run(base, seeds, !args.dump)?;
        for k in &args.keys {
            app.handle(k.clone());
        }
        match args.modal.as_deref() {
            None => {}
            Some("new") => app.modal = Some(Modal::NewCauldron { name: args.new_name.clone().unwrap_or_default(), error: None }),
            Some(other) => bail!("no modal `{other}` on the first-run screen (new)"),
        }
        return show(app, &args);
    }
    // Nobody named one: the last opened, else prod, else the first there is.
    let env = match (named, super::store::default_env(&base)?) {
        (false, Some(name)) => base.for_name(&name)?,
        _ => base,
    };
    // REL-3.S2: no repo seed, ever, unless a fixture is named — and a named
    // fixture needs no real environment on disk to confirm, the same
    // exemption the first-run gate above gives it.
    if args.seeds.is_none() {
        confirm_environment(&env, named, args.dump)?;
    }
    // RV1.S12a: the curated demo is read-only — nothing is ever dumped into it.
    if args.send && env.name == soup::demo::ENV {
        bail!("--send: `{}` is the curated demo and is read-only; dump into another environment", env.name);
    }
    let seeds = args.seeds.clone().unwrap_or_else(|| super::store::no_seeds(&env));
    let mut app = App::open(env, seeds, !args.dump)?;
    if args.send {
        arm_send(&mut app)?;
    }
    // First: `--aggregate` brings the whole cauldron, which `--cluster X-0`
    // may name (b964d571 msg 23).
    app.set_aggregate(args.aggregate);
    if let Some(key) = &args.cluster {
        let id = app.forest.find(key).with_context(|| format!("no cluster with id `{key}`"))?;
        app.select(id);
    }
    if let Some(t) = args.tab {
        app.set_ctab(t);
    }
    if let Some(t) = args.cluster_tab {
        app.set_ktab(t);
    }
    if let Some(t) = args.status_tab {
        app.set_ctab(CTab::Status);
        app.set_sttab(t);
    }
    if let Some(t) = args.config_tab {
        app.set_cfg_tab(t);
    }
    if let Some(q) = &args.search {
        app.start_search();
        app.searching = false;
        app.set_search(q);
    }
    if args.region.is_some() || args.node.is_some() {
        app.open_ids(args.region.as_deref(), args.node.as_deref())?;
    }
    if let Some(path) = &args.settings {
        match args.tab {
            Some(t) if !t.is_page() => bail!("--settings needs a settings tab: --tab permissions | settings | tui | devices | providers | accounts (a cluster has no settings, msg 235)"),
            // Its path's first slug picks the tab.
            _ if !app.ctab.is_page() => app.set_cfg_tab(super::menu::CfgTab::Cauldron),
            _ => {}
        }
        app.focus = Focus::Content;
        app.open_settings_path(path)?;
    }
    if let Some(i) = args.item {
        match app.ctab {
            CTab::Cauldron => app.item = i,
            CTab::Status => app.rows[0] = i,
            super::app::page_tabs!() => app.rows[super::menu::SETTINGS_ROW] = i,
            CTab::Threads => app.threads.list_item = i,
        }
        app.focus = Focus::Content;
        app.reveal = true;
    }
    // **THE ONE CONNECTION** (REL-6), started the first time the Threads tab
    // is shown — here, or by the live loop when he goes to it — and never
    // again. A face (or a dump) that never shows the tab dials nothing, so a
    // test driving another tab cannot reach a daemon through it. A dump
    // waits for the daemon's first answer.
    if args.thread.is_some() {
        app.set_ctab(CTab::Threads);
    }
    // The launch's door (REL-6a): an interactive face resolves the
    // environment's daemon; a `--dump` (or any isolated launch) only the port
    // it is pointed at, never one of his live ports.
    // REL-6d: the door keeps the lever AS THE LAUNCH SET IT (nothing has
    // pointed it yet), and the environment it opened.
    app.live_door = Some(super::live::Door::of_launch(scripted, &app.env, std::env::var("TUI_THREADS_CHASSIS_PORT").ok()));
    if app.ctab == CTab::Threads {
        app.start_live();
        // A headless Threads dump with no daemon to read is an error, not a
        // frame of empty bands (REL-6-I F2).
        if scripted && !app.live.is_started() {
            bail!("{}", app.live.why_not.as_deref().unwrap_or(super::live::NO_DOOR));
        }
    }
    if args.dump && app.ctab == CTab::Threads {
        settle_dump(&mut app, args.settle, |a| a.live.is_up() && !a.threads.rows.is_empty());
    }
    // V1.5: Status › Tokens reads the account pool off the same one
    // connection. Not refused when there is none: the band says why it has no
    // pool (`not connected: …`), and the endpoints under it are the face's own.
    if app.shows_live() && app.ctab != CTab::Threads {
        app.start_live();
        if args.dump && app.live.is_started() {
            settle_dump(&mut app, args.settle, |a| a.live.is_up() && a.live.pool().is_some_and(|p| p.known && p.loading.is_empty()));
        }
    }
    if let Some(t) = &args.thread {
        let id = app.threads.rows.iter().find(|x| &x.id == t || x.name.eq_ignore_ascii_case(t)).map(|x| x.id.clone()).unwrap_or_else(|| t.clone());
        app.threads.open_thread(&id);
        super::threads::settle(&mut app);
        if args.dump {
            settle_dump(&mut app, args.settle, |a| a.threads.transcript.first().is_some_and(|b| !matches!(b, super::threads::Block::Note { text } if text.starts_with("loading"))));
        }
        if let Some(a) = &args.agent {
            app.threads.agent(a).with_context(|| format!("--agent: no subagent `{a}` in that thread"))?;
            app.threads.set_subject(Some(a.clone()));
            super::threads::settle(&mut app);
        }
    }
    for k in &args.keys {
        app.handle(k.clone());
    }
    // REL-6d: keys that switched cauldron (`App::switch_env` tore the old
    // connection down) or went to a tab that reads the daemon start the
    // shown environment's connection through the launch's door — what the
    // live loop does on its next tick — and a Threads dump waits for its
    // answer as it waited for the launch's.
    if !args.keys.is_empty() && args.dump {
        if app.shows_live() && !app.live.is_started() {
            app.start_live();
        }
        if app.ctab == CTab::Threads && app.live.is_started() {
            settle_dump(&mut app, args.settle, |a| a.live.is_up() && !a.threads.rows.is_empty());
        }
    }
    for text in &args.inputs {
        app.dump(text);
    }
    apply_dump_ops(&mut app, &args.dump_ops)?;
    if args.dump_collapsed {
        app.focus = Focus::Tree;
    }
    if args.tag {
        // `i` in the content pane, after the drafts so the tag lands at the end.
        app.focus = Focus::Content;
        app.handle(event::Event::Key(event::KeyEvent::new(
            event::KeyCode::Char('i'),
            event::KeyModifiers::NONE,
        )));
    }
    if args.send {
        return send(app);
    }
    match args.modal.as_deref() {
        None => {}
        Some("cauldrons") => app.open_cauldrons(),
        Some("new") => {
            app.modal = Some(Modal::NewCauldron { name: args.new_name.clone().unwrap_or_default(), error: None })
        }
        Some("new-cluster") => {
            let parent = match &args.parent {
                Some(p) => Some(app.forest.find(p).with_context(|| format!("no cluster with id `{p}`"))?),
                None => None,
            };
            app.open_new_cluster(parent);
            if let Some(Modal::NewCluster { name, boundaries, at, .. }) = &mut app.modal {
                *name = args.new_name.clone().unwrap_or_default();
                if let Some(b) = &args.boundaries {
                    *boundaries = b.clone();
                    *at = 1;
                }
            }
        }
        Some("palette") => app.open_palette(),
        Some("enable") => app.ask_enable(),
        Some("add-endpoint") => {
            app.set_cfg_tab(crate::cauldron::menu::CfgTab::Providers);
            app.start_act(crate::cauldron::manage::Act::NewEndpoint { here: false });
        }
        Some("paste") => {
            let text = app.composer.text();
            let t = crate::cauldron::tags::scan(&text).into_iter().map(|(_, t)| t).find(|t| t.chip_id().is_some()).context("--modal paste: the bucket has no paste (--paste makes one)")?;
            app.open_chip(&t);
        }
        Some("help") => {
            if !app.ctab.is_page() {
                bail!("--modal help needs a settings tab (--tab permissions | settings | tui | devices | providers | accounts)");
            }
            app.settings_help();
        }
        Some(other) => bail!("no modal `{other}` (cauldrons | new | new-cluster | palette | help | enable | paste | add-endpoint)"),
    }
    if let Some(page) = &args.bus {
        app.set_ctab(CTab::Status);
        app.set_sttab(super::status::StatusTab::Bus);
        app.focus = Focus::Content;
        app.open_bus(page.clone());
    }

    show(app, &args)
}

/// **THE BUCKET FLAGS, APPLIED** — `--draft` types, `--note` starts a new
/// note, `--quote` is `i` on a tag, `--paste` puts a paste chip at the caret.
/// Shared by `--dump`, `--send` and the drift test that presses the send key
/// on a bucket built the same way (RV1.S12a).
pub fn apply_dump_ops(app: &mut App, ops: &[DumpOp]) -> anyhow::Result<()> {
    for op in ops {
        use crate::cauldron::dump;
        if let DumpOp::Draft(t) = op {
            app.type_text(t);
            continue;
        }
        app.focus_input();
        match op {
            DumpOp::Draft(_) => {}
            DumpOp::Note(t) => {
                if !app.composer.is_blank() {
                    app.composer.buffer_end();
                    app.composer.newline();
                    app.composer.newline();
                }
                app.type_text(t);
            }
            DumpOp::Quote(t) => {
                let tag = crate::cauldron::tags::scan(&format!("[{}]", t.trim_matches(['[', ']']))).into_iter().next().map(|(_, t)| t).with_context(|| format!("--quote: `{t}` is not a tag"))?;
                app.quote_tag(&tag);
            }
            DumpOp::Paste(t) => {
                let id = app.chips.fresh_id(&app.composer.text());
                app.chips.write(&id, t)?;
                let chip = dump::paste_chip(&id, t).render();
                let before = app.composer.text();
                let sp = if before.is_empty() || before.ends_with(char::is_whitespace) { "" } else { " " };
                app.type_text(&format!("{sp}{chip}"));
            }
        }
    }
    Ok(())
}

/// **`--send` ARMS THE ONE WRITE** (RV1.S12a). The App was opened writing
/// nothing (`--dump`); this gives it the environment's dump log and chip
/// store WRITABLE — read exactly as `App::open` reads them, the seed's dumps
/// first and then the file — and an EMPTY bucket, so a headless dump never
/// sends (or clears) the draft he has open. Nothing else becomes writable.
///
/// **THE BUCKET IS ISOLATED** (RV1.S12c). Emptying the composer is not
/// enough: the App's `DraftStore` still points at his `bucket.json`, and a
/// store that does not persist still MERGES a foreign write
/// (`DraftStore::save`), so a write from his face between `App::open`'s load
/// and the first `--draft` would ride out in this dump. The store is
/// therefore replaced by one on a path no face knows — a fresh, never-created
/// directory, not writable, so nothing is ever made there — and the
/// references' index by one that touches no file. Every `save_draft` this run
/// makes reads nothing and writes nothing; his bucket is never read again.
pub fn arm_send(app: &mut App) -> anyhow::Result<()> {
    let dir = super::store::soup_dir(&app.env)?;
    let mut sends = super::store::SendLog::new(&dir, true);
    sends.seed(super::store::DumpSeed::load(&app.seeds, &app.env.name)?.dumps);
    sends.poll()?;
    app.sends = sends;
    app.chips = super::dump::Artifacts::new(&dir, true);
    app.composer = tui_render::compose::Composer::new();
    let nowhere = std::env::temp_dir().join(format!("cauldron-ctl-send-{}", uuid::Uuid::new_v4().simple()));
    app.draft = super::store::DraftStore::new(&nowhere, false);
    app.refs = super::refs::Index::none();
    Ok(())
}

/// **SEND THE BUCKET THROUGH THE SEND KEY'S FUNCTION** (RV1.S12a) — `submit`,
/// what Enter in the bucket calls (`App::input_key`) — and return the dump it
/// appended, exactly the line it wrote to `dumps.jsonl`. No second write path:
/// the record is built where the key builds it.
pub fn send_bucket(app: &mut App) -> anyhow::Result<super::store::Send> {
    if app.composer.is_blank() {
        bail!("--send: the bucket is empty — nothing to dump (give --draft, --note, --quote or --paste)");
    }
    let text = app.composer.text();
    let before = app.sends.sends.len();
    app.error = None;
    app.focus_input();
    app.submit();
    if let Some(e) = app.error.take() {
        bail!("--send: {e}");
    }
    let env = app.env.name.clone();
    let sent = app.sends.sends.get(before..).unwrap_or_default().iter().rev().find(|s| s.text == text && s.env == env).context("--send: the dump was not appended")?;
    Ok(sent.clone())
}

/// `--send`: [`send_bucket`], then print the dump as its file line.
fn send(mut app: App) -> anyhow::Result<()> {
    let sent = send_bucket(&mut app)?;
    println!("{}", serde_json::to_string(&sent)?);
    Ok(())
}

/// Print the one frame (`--dump`), or run the live face.
fn show(mut app: App, args: &Args) -> anyhow::Result<()> {
    if args.dump {
        if args.item.is_none() {
            app.scroll = args.scroll;
        }
        for row in render_to_text(&mut app, args.size.0, args.size.1)? {
            println!("{row}");
        }
        return Ok(());
    }

    // The live face reads git for the Nodes tab's worktrees (off the UI
    // thread); `--dump` never does, so a dump is the seed's, every time.
    app.live_git = true;
    term_shell::install_panic_hook();
    let mut term = term_shell::enter_with(&[
        term_shell::ALT_SCREEN,
        term_shell::MOUSE_BUTTONS,
        term_shell::MOUSE_SGR,
        term_shell::BRACKETED_PASTE,
    ])?;
    // Ask whether the terminal speaks the kitty keyboard protocol —
    // crossterm's reader is this face's only reader, so the probe's reply
    // cannot be stolen and keys typed meanwhile are queued, not lost — and if
    // it does, push the "disambiguate" flag, which reports ⌥ chords and
    // ⇧Enter as CSI-u keys. (⌘ is no longer bound: ⇧Tab steps the cauldron
    // tabs.) `restore` pops it on every exit path.
    if ratatui::crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false) {
        term_shell::push_kitty();
    }
    let result = run(&mut term, &mut app);
    term_shell::restore();
    result
}

/// What `o` hands a path to: the ONE place this face execs anything on his
/// machine, read from `CAULDRON_OPENER` (IS-111 — a pty test pressed `o` and
/// the system `open` really ran on his machine).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opener {
    /// Unset or empty: the system `open`, as `o` has always done for him.
    System,
    /// `none`: never exec; the face says what it would have opened.
    Suppressed,
    /// Any other value: that program, with the path as its one argument.
    Program(std::ffi::OsString),
}

impl Opener {
    pub fn from_env() -> Self {
        Self::parse(std::env::var_os("CAULDRON_OPENER"))
    }

    pub fn parse(v: Option<std::ffi::OsString>) -> Self {
        match v {
            None => Opener::System,
            Some(v) if v.is_empty() => Opener::System,
            Some(v) if v == "none" => Opener::Suppressed,
            Some(v) => Opener::Program(v),
        }
    }

    /// The path as the opener's one argument: a relative path that starts
    /// with `-` would be read as an option, so it goes as `./-…`.
    pub fn arg(path: &str) -> String {
        if path.starts_with('-') { format!("./{path}") } else { path.to_string() }
    }

    /// Open `path`. `Ok(false)` is the suppressed opener: nothing was run.
    pub fn open(&self, path: &str) -> std::io::Result<bool> {
        let program: &std::ffi::OsStr = match self {
            Opener::Suppressed => return Ok(false),
            // REL-6a5: the real macOS `open` acts on his desktop; an isolated
            // process runs it only when CAULDRON_OPENER points somewhere.
            Opener::System if crate::cauldron::live::isolation(false).is_some() => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "refused: the real `open` from an isolated process; set CAULDRON_OPENER (none, or a recorder)",
                ))
            }
            Opener::System => "open".as_ref(),
            Opener::Program(p) => p,
        };
        use std::process::{Command, Stdio};
        Command::new(program).arg(Self::arg(path)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
        Ok(true)
    }
}

/// The quiet tick: how often a face with nothing arriving re-reads what other
/// faces on its environment wrote, and repaints.
const QUIET_TICK: Duration = Duration::from_millis(200);

/// **WAIT FOR THE DAEMON'S ANSWER, FOR A DUMP ONLY** — up to `most`, until
/// `done` holds and nothing has arrived for one frame. The live face never
/// waits: it draws loading rows first and fills them in.
fn settle_dump(app: &mut App, most: Duration, done: impl Fn(&App) -> bool) {
    let t0 = std::time::Instant::now();
    let mut quiet_since = std::time::Instant::now();
    while t0.elapsed() < most {
        if super::threads::tick(app) > 0 {
            quiet_since = std::time::Instant::now();
        } else if done(app) && quiet_since.elapsed() >= crate::app::FRAME_INTERVAL * 4 {
            return;
        }
        std::thread::sleep(crate::app::FRAME_INTERVAL);
    }
}

fn run(term: &mut term_shell::Tui, app: &mut App) -> anyhow::Result<()> {
    let opener = Opener::from_env();
    let mut dirty = true;
    let mut last_sync = std::time::Instant::now();
    while !app.quit {
        // What the face asked of the terminal: a copy (OSC 52), an `open`.
        let jobs = std::mem::take(&mut app.outbox);
        dirty |= !jobs.is_empty();
        for job in jobs {
            match job {
                Outbox::Copy(text) => match term_shell::copy_to_clipboard(&text) {
                    Ok(c) if c.is_warning() => app.error = Some(c.notice()),
                    Ok(c) => app.notice = Some(c.notice()),
                    Err(e) => app.error = Some(format!("NOT copied: {e}")),
                },
                Outbox::Open(path) => match opener.open(&path) {
                    Ok(true) => {}
                    Ok(false) => app.notice = Some(format!("open (suppressed): {}", crate::cauldron::artpage::tilde(&path))),
                    Err(e) => app.error = Some(format!("open {path}: {e}")),
                },
            }
        }
        if app.shows_live() && !app.live.is_started() {
            app.start_live();
            dirty = true;
        }
        if dirty {
            term.draw(|f| view::draw(f, app))?;
            dirty = false;
        }
        // **NEVER WAIT ON THE DAEMON.** The connection's events are drained
        // with `try_recv` (`threads::tick`); while they arrive the loop wakes
        // on the chat face's 16 ms frame grammar, and a quiet face keeps the
        // 200 ms tick it always had.
        let wait = if app.live.is_started() { crate::app::FRAME_INTERVAL } else { QUIET_TICK };
        if event::poll(wait)? {
            app.handle(event::read()?);
            // Drain a burst (a paste, a fast typist) before the next frame.
            while event::poll(Duration::ZERO)? {
                app.handle(event::read()?);
            }
            dirty = true;
        }
        if super::threads::tick(app) > 0 {
            dirty = true;
        }
        if last_sync.elapsed() >= QUIET_TICK {
            // A quiet tick: pick up what other faces on this environment wrote.
            app.sync();
            last_sync = std::time::Instant::now();
            dirty = true;
        }
    }
    super::threads::flush_draft(app, true);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse, USAGE};

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|x| x.to_string()).collect()
    }

    /// Every `--flag` the usage names (`--help` exits the process, so not it).
    fn usage_flags() -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for w in USAGE.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
            if w.starts_with("--") && w.len() > 2 && w != "--help" && !out.iter().any(|o| o == w) {
                out.push(w.to_string());
            }
        }
        out
    }

    fn needs_value(flag: &str) -> bool {
        match parse(v(&[flag])) {
            Ok(_) => false,
            Err(e) => format!("{e:#}").contains("needs a value"),
        }
    }

    /// **V1.2d: the router's list of value flags IS this parser's.**
    /// `env::face::VALUE_FLAGS` lets the router skip a flag's value so it is
    /// never read as a bare cauldron name. Each flag the usage names is put to
    /// THIS parser alone: one that asks for a value must be on the list (or be
    /// `--env`), one that does not must be off it — so a flag added to either
    /// side alone fails here. A positive sentinel first: the probe can see a
    /// value flag and a switch.
    #[test]
    fn the_routers_value_flags_are_exactly_this_parsers() {
        assert!(needs_value("--tab") && !needs_value("--dump"), "sentinel");
        let flags = usage_flags();
        assert!(flags.len() > 30, "usage flags found: {flags:?}");
        for f in &flags {
            let listed = f == "--env" || env::face::VALUE_FLAGS.contains(&f.as_str());
            assert_eq!(needs_value(f), listed, "`{f}`: parser needs a value = {}, VALUE_FLAGS lists it = {listed}", needs_value(f));
        }
        for f in env::face::VALUE_FLAGS {
            assert!(needs_value(f), "`{f}` is in env::face::VALUE_FLAGS but the face does not take a value for it");
        }
    }

    /// **RV1.S12a: `--send` is the one headless write, so it is valid only
    /// inside a `--dump` and only on its own.** Opposed: the same flags with
    /// `--dump`, and every other combination without `--send`, are accepted.
    #[test]
    fn rv1s12a_send_is_valid_only_with_dump_and_alone() {
        use super::validate;
        let ok = |a: &[&str]| validate(&parse(v(a)).expect("parses")).is_ok();
        let why = |a: &[&str]| format!("{:#}", validate(&parse(v(a)).expect("parses")).expect_err("refused"));
        assert!(ok(&["--dump", "--send", "--draft", "x"]));
        assert!(why(&["--send", "--draft", "x"]).contains("only with --dump"));
        assert!(why(&["--dump", "--send", "--seeds", "/tmp/x"]).contains("--seeds"));
        assert!(why(&["--dump", "--send", "--input", "x"]).contains("--input"));
        assert!(why(&["--dump", "--send", "--keys", "enter"]).contains("--keys"));
        for a in [&["--input", "x"][..], &["--keys", "enter"], &["--seeds", "/tmp/x"], &["--dump", "--input", "x"]] {
            assert!(ok(a), "{a:?} refused without --send");
        }
    }

    /// **V1.2d: the name the router reads is the name this parser opens.**
    #[test]
    fn named_in_agrees_with_this_parser_on_the_cauldron_named() {
        for argv in [
            &["prod"][..],
            &["--dump", "--tab", "status", "alpha"],
            &["--size", "80x24", "--env", "beta"],
            &["alpha", "--env", "beta"],
            &["--dump", "--draft", "--env"],
            &["--item", "2"],
        ] {
            let a = parse(v(argv)).expect("the face accepts it");
            assert_eq!(env::face::named_in(&v(argv)).ok().flatten(), a.env, "{argv:?}");
        }
        for argv in [&["--env"][..], &["alpha", "beta"]] {
            assert!(parse(v(argv)).is_err() && env::face::named_in(&v(argv)).is_err(), "{argv:?}");
        }
    }
}
