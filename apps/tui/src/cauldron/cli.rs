//! `tui --cauldron` — the Cauldron face (Cauldron, Agents, Status, Config), on fake data.
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
tui --cauldron — the Cauldron face (fake data); `cauldron tui` opens it

    tui --cauldron [--env <name> | <name>] [--seeds <dir>]
    tui --cauldron --dump [--env <name>] [--tab <tab>] [--cluster <id>] [--cluster-tab <tab>]
                       [--status-tab <tab>]
                       [--config-tab cauldron|tui|devices|providers|permissions]
                       [--aggregate] [--search <text>] [--region <id>] [--node <id>]
                       [--item <n>] [--tag] [--draft <text>]... [--input <text>]...
                       [--note <text>]... [--quote <tag>]... [--paste <text>]... [--dump-collapsed]
                       [--modal cauldrons|new|new-cluster|palette|help|enable|paste|add-endpoint] [--new-name <text>]
                       [--parent <id>] [--boundaries <text>]
                       [--settings <path>] [--bus <page>]
                       [--keys <keys>] [--size <W>x<H>] [--scroll <n>]
                       [--thread <id>] [--agent <id>]

  --env          the cauldron environment (default: $CAULDRON_ENV, $ENV, else prod)
  --seeds        a fixture seed directory (tests only; default: none — an
                 environment shows only its own data)
  --dump         render ONE frame headlessly and print it; nothing is written to disk
  --tab          cauldron | agents | status | config
  --cluster      the cluster to focus, by its minted id (`X-2`; default: the first)
  --cluster-tab  questions | actions | tasks | nodes | status | details (tasks: the execution graph)
  --status-tab   ledger | tokens | mesh | soup | bus (implies --tab status)
  --config-tab   cauldron (this cauldron) | tui | devices | providers | permissions (implies --tab config)
  --thread       open this thread on the Agents tab (its id or name; implies --tab agents)
  --agent        with --thread: show this subagent's transcript
  --aggregate    include descendants' items on the page
  --search       search the cluster's nodes (implies --cluster-tab nodes)
  --region       open this region on the cluster's Nodes tab
  --node         open this node, after --region if both
  --item         select item n (0-based): on a cluster's page, or a row of Status or Config
  --tag          quote the selected item into the bucket, as `i` would
  --draft        type this text into the bucket (repeatable; not dumped)
  --note         a new note at the end of the bucket: a blank line, then this text (repeatable)
  --quote        `i` on this tag: a new note anchored to it (`--quote Q-1`)
  --paste        a paste chip at the caret holding this text
  --dump-collapsed   the bucket does not hold the selection
  --input        type this text into the bucket and dump it, as Enter would (repeatable)
  --modal        open the cauldrons dropdown, a modal naming a new cauldron or cluster, the key
                 palette, (help) what the selected setting means, (enable) the confirm that disables
                 or enables the cauldron, (paste) the bucket's first paste, or (add-endpoint) the
                 add modal of a global inference endpoint, on Config › Providers
  --new-name     the name typed into the naming modal
  --parent       with --modal new-cluster: the cluster it is made under
  --boundaries   with --modal new-cluster: its boundary conditions, typed into the modal
  --settings     open Config's bands by their path, as → would on each, selecting the last:
                 providers/claude_code, providers/endpoints, cauldron/accounts; a last slug naming a
                 table's row opens its modal: claude_code/acct2, providers/endpoints/ep-1
  --bus          open the bus modal on a page (implies --status-tab bus): event:<id> | ripple:<id>
                 | trigger:<ripple>/<n> (n from 1) | run:<id>
  --keys         press these keys, in order, after everything above and before --tag:
                 space-separated, each one of up down left right enter esc tab home end
                 opt-up opt-down space, or a single character (`--keys 'right down right'`)
  --size         frame size for --dump (default 110x48)
  --scroll       page scroll offset for --dump

environment
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
    dump_collapsed: bool,
    thread: Option<String>,
    agent: Option<String>,
}

#[derive(Debug, Clone)]
enum DumpOp {
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
        dump_collapsed: false,
        thread: None,
        agent: None,
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
                    bail!("`--tab {v}`: that tab is called {} now (8e4fca10 msg 457)", now.label().to_lowercase());
                }
                a.tab = Some(CTab::parse(&v).with_context(|| format!("no cauldron tab `{v}`"))?);
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
                a.config_tab = Some(super::menu::CfgTab::parse(&v).with_context(|| format!("no Config tab `{v}`"))?);
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
            "--thread" => a.thread = Some(val("--thread")?),
            "--agent" => a.agent = Some(val("--agent")?),
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
/// pipe). `prod`, the name nobody has to type, always opens.
fn confirm_environment(env: &env::Environment, named: bool, dump: bool) -> anyhow::Result<()> {
    use std::io::{BufRead, IsTerminal, Write};
    let there = env.environments()?;
    if !named || env.name == "prod" || there.contains(&env.name) {
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

/// Run the soup face with `argv` (the process arguments, `--cauldron` removed).
pub fn main(argv: Vec<String>) -> anyhow::Result<()> {
    let args = parse(argv)?;
    let env = resolve_env(args.env.as_deref())?;
    let named = args.env.is_some() || env::Environment::resolve().is_ok_and(|e| e.name_source != env::paths::Source::Default);
    confirm_environment(&env, named, args.dump)?;
    // REL-3.S2: no repo seed, ever, unless a fixture is named.
    let seeds = args.seeds.clone().unwrap_or_else(|| super::store::no_seeds(&env));
    let mut app = App::open(env, seeds, !args.dump)?;
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
        app.set_ctab(CTab::Config);
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
        if app.ctab != CTab::Config {
            bail!("--settings needs --tab config (a cluster has no settings, msg 235)");
        }
        app.focus = Focus::Content;
        app.open_settings_path(path)?;
    }
    if let Some(i) = args.item {
        match app.ctab {
            CTab::Cauldron => app.item = i,
            CTab::Status => app.rows[0] = i,
            CTab::Config => app.rows[super::menu::SETTINGS_ROW] = i,
            CTab::Agents => app.threads.list_item = i,
        }
        app.focus = Focus::Content;
        app.reveal = true;
    }
    if let Some(t) = &args.thread {
        app.set_ctab(CTab::Agents);
        let i = app.threads.seed.threads.iter().position(|x| &x.id == t || x.name.eq_ignore_ascii_case(t)).with_context(|| format!("--thread: no thread `{t}`"))?;
        app.threads.open_thread(i);
        if let Some(a) = &args.agent {
            app.threads.agent(a).with_context(|| format!("--agent: no subagent `{a}` in that thread"))?;
            app.threads.set_subject(Some(a.clone()));
        }
    }
    for k in &args.keys {
        app.handle(k.clone());
    }
    for text in &args.inputs {
        app.dump(text);
    }
    for op in &args.dump_ops {
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
            app.set_ctab(CTab::Config);
            app.set_cfg_tab(crate::cauldron::menu::CfgTab::Providers);
            app.start_act(crate::cauldron::manage::Act::NewEndpoint { here: false });
        }
        Some("paste") => {
            let text = app.composer.text();
            let t = crate::cauldron::tags::scan(&text).into_iter().map(|(_, t)| t).find(|t| t.chip_id().is_some()).context("--modal paste: the bucket has no paste (--paste makes one)")?;
            app.open_chip(&t);
        }
        Some("help") => {
            if app.ctab != CTab::Config {
                bail!("--modal help needs --tab config");
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
            Opener::System => "open".as_ref(),
            Opener::Program(p) => p,
        };
        use std::process::{Command, Stdio};
        Command::new(program).arg(Self::arg(path)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
        Ok(true)
    }
}

fn run(term: &mut term_shell::Tui, app: &mut App) -> anyhow::Result<()> {
    let opener = Opener::from_env();
    while !app.quit {
        // What the face asked of the terminal: a copy (OSC 52), an `open`.
        for job in std::mem::take(&mut app.outbox) {
            match job {
                Outbox::Copy(text) => match term_shell::copy_to_clipboard(&text) {
                    Ok(c) if c.is_warning() => app.error = Some(c.notice()),
                    Ok(c) => app.notice = Some(c.notice()),
                    Err(e) => app.error = Some(format!("NOT copied: {e}")),
                },
                Outbox::Open(path) => match opener.open(&path) {
                    Ok(true) => {}
                    Ok(false) => app.notice = Some(format!("open (suppressed): {}", match std::env::var("HOME") {
                        Ok(h) if !h.is_empty() => path.replace(&h, "~"),
                        _ => path.clone(),
                    })),
                    Err(e) => app.error = Some(format!("open {path}: {e}")),
                },
            }
        }
        term.draw(|f| view::draw(f, app))?;
        if event::poll(Duration::from_millis(200))? {
            app.handle(event::read()?);
            // Drain a burst (a paste, a fast typist) before the next frame.
            while event::poll(Duration::ZERO)? {
                app.handle(event::read()?);
            }
        } else {
            // A quiet tick: pick up what other faces on this environment wrote.
            app.sync();
        }
    }
    Ok(())
}
