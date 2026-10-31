//! The feedback audit (task 0117, report 0118): every PARTIAL and DRIFTED ask
//! it found, closed (task 0119), each proven here on real seed files, real
//! keys and mouse events, and real frames on ratatui's TestBackend, under a
//! scratch cauldron root (never `~/.cauldron`). The text cuts (F33, F37) are
//! held by the LEGENDS sweep in `clusters_render.rs`; F5, F10, F14 and F53 by
//! the tests of the tabs they changed.
//!
//! The last three (task 0123): F4, a cluster's LOG; F13, a typo'd environment
//! never made silently; F52 (the grabber-only drag) is held by the divider
//! test in `clusters_render.rs`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui::cauldron::app::{App, CTab, Focus, Footer, Hit, Item, Modal, KTab};
use tui::cauldron::pages::{log_entries, LogEntry};
use tui::cauldron::menu::{Bound, Nav, SETTINGS_ROW};
use tui::cauldron::render_to_text;
use tui::cauldron::settings::{Ctx, Prov, Val};
use tui::cauldron::store::seed_dir;

// ── fixtures ────────────────────────────────────────────────────────────────

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "feedback-audit-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn env_in(root: &Path, name: &str) -> Environment {
    Environment { root: root.to_path_buf(), root_source: Source::CauldronHome, name: "prod".into(), name_source: Source::Default, data_root: root.join("data").join("prod") }
        .for_name(name)
        .unwrap()
}

fn open_env(root: &Path, name: &str) -> App {
    // Hermetic: no billed type-one call (tests/it/seed_live.rs is the live proof).
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    App::open(env_in(root, name), seed_dir(), true).expect("seed loads")
}

fn open(root: &Path) -> App {
    open_env(root, "prod")
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn typed(a: &mut App, s: &str) {
    for c in s.chars() {
        a.handle(key(KeyCode::Char(c)));
    }
}

fn screen(a: &mut App) -> String {
    render_to_text(a, 140, 120).unwrap().join("\n")
}

fn line_with<'a>(s: &'a str, needle: &str) -> &'a str {
    s.lines().find(|l| l.contains(needle)).unwrap_or_else(|| panic!("no line with `{needle}`:\n{s}"))
}

/// Runs of spaces as one: columns are layout, not content.
fn c(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn click_on(a: &mut App, hit: Hit) {
    render_to_text(a, 140, 60).unwrap();
    let (r, _) = *a.hits.iter().find(|(_, h)| *h == hit).unwrap_or_else(|| panic!("{hit:?} not on screen"));
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x, row: r.y, modifiers: KeyModifiers::NONE }));
}

fn cauldron_settings(a: &mut App, path: &str) {
    a.set_ctab(CTab::Config);
    a.focus = Focus::Content;
    a.snav = Nav::cauldron();
    a.rows[SETTINGS_ROW] = 0;
    if !path.is_empty() {
        a.open_settings_path(path).unwrap();
    }
}

fn get(a: &App, key: &'static str, ctx: Ctx) -> tui::cauldron::settings::Resolved {
    a.sget(&Bound::new(key, ctx))
}

fn modal_fields(a: &App) -> (String, String, u8) {
    match &a.modal {
        Some(Modal::NewCluster { name, boundaries, at, .. }) => (name.clone(), boundaries.clone(), *at),
        m => panic!("not the new-cluster modal: {m:?}"),
    }
}

// ── F41: prod's 80% weekly threshold is a real default ──────────────────────

/// msg 64: an 80% weekly threshold on prod's claude max accounts, inherited
/// by new accounts, overridable per account. It was seed data read only
/// while no settings file existed; it is now prod's own default, in code.
#[test]
fn prods_weekly_80_is_a_real_default_that_survives_a_settings_file_and_new_accounts_inherit() {
    let seed = std::fs::read_to_string(seed_dir().join("settings").join("prod.toml")).unwrap();
    assert!(!seed.contains("threshold_pct"), "the 80 is not seed data:\n{seed}");
    let root = scratch_root();
    let mut a = open(&root);
    let pool = || Ctx::pool("claude_code");
    let r = get(&a, "pools.{pool}.budget.weekly.threshold_pct", pool());
    assert_eq!((r.val.clone(), r.prov.clone()), (Val::Int(80), Prov::Default));
    // Any edit writes the settings file; the 80 still holds.
    a.sset(&Bound::new("local.enabled", Ctx::default()), Val::Bool(true)).unwrap();
    let file = root.join("data/prod/tui/soup/settings.toml");
    assert!(file.is_file());
    assert!(!std::fs::read_to_string(&file).unwrap().contains("threshold_pct"));
    let mut b = open(&root);
    assert_eq!(get(&b, "pools.{pool}.budget.weekly.threshold_pct", pool()).val, Val::Int(80));
    // An account inherits it from the pool; a new one too.
    let acct = get(&b, "pools.{pool}.accounts.{account}.budget.weekly.threshold_pct", Ctx::account("claude_code", "acct2"));
    assert_eq!((acct.val, acct.prov), (Val::Int(80), Prov::Inherited("pool".into())));
    let id = b.settings.add_account("claude_code", "fresh one", |_| false).unwrap();
    assert_eq!(get(&b, "pools.{pool}.accounts.{account}.budget.weekly.threshold_pct", Ctx::account("claude_code", &id)).val, Val::Int(80));
    // An account overrides it; the pool overridden and cleared returns to 80.
    let wk = Bound::new("pools.{pool}.accounts.{account}.budget.weekly.threshold_pct", Ctx::account("claude_code", "acct2"));
    b.sset(&wk, Val::Int(60)).unwrap();
    assert_eq!(b.sget(&wk).val, Val::Int(60));
    let pb = Bound::new("pools.{pool}.budget.weekly.threshold_pct", pool());
    b.sset(&pb, Val::Int(70)).unwrap();
    assert_eq!(b.sinherited(&pb).val, Val::Int(80), "an override is shown against 80");
    b.sclear(&pb).unwrap();
    assert_eq!((b.sget(&pb).val, b.sget(&pb).prov), (Val::Int(80), Prov::Default));
    // Another pool keeps the default 95. The accounts are GLOBAL now (msg
    // 186): another environment reads the same 80.
    assert_eq!(get(&b, "pools.{pool}.budget.weekly.threshold_pct", Ctx::pool("codex")).val, Val::Int(95));
    let d = open_env(&root, "dev");
    assert_eq!(get(&d, "pools.{pool}.budget.weekly.threshold_pct", pool()).val, Val::Int(80));
}

// ── F40: budget caps beside the thresholds ──────────────────────────────────

#[test]
fn session_weekly_and_fable_caps_sit_beside_the_thresholds_at_pool_and_account() {
    let mut a = open(&scratch_root());
    cauldron_settings(&mut a, "providers");
    let s = screen(&mut a);
    for l in ["session cap", "weekly cap"] {
        assert!(c(line_with(&s, l)).contains(&format!("{l} off")), "{s}");
    }
    // Fable is Claude Code's: its rows are on that band, not at the shared
    // head (0214 A10).
    let head = &s[..s.find("Claude Code").expect(&s)];
    assert!(!head.contains("fable"), "{head}");
    // The cauldron's default → the token source → the account. A value from
    // the cauldron's level is just the `default` (msg 165).
    a.sset(&Bound::new("budgets.default.weekly.cap_pct", Ctx::default()), Val::Int(90)).unwrap();
    cauldron_settings(&mut a, "providers/claude_code");
    let full = screen(&mut a);
    // Below its band's title: the shared defaults stand flat above it.
    let s = full[full.find("Claude Code").unwrap()..].to_string();
    let t = s.find("weekly threshold").unwrap();
    let cap = s.find("weekly cap").expect(&s);
    assert!(t < cap, "the cap beside the threshold:\n{s}");
    assert!(c(line_with(&s, "weekly cap")).contains("weekly cap 90 % inherits 90 %"), "{s}");
    a.sset(&Bound::new("pools.{pool}.budget.fable.cap_pct", Ctx::pool("claude_code")), Val::Int(50)).unwrap();
    // An account, in its row's modal.
    cauldron_settings(&mut a, "providers/claude_code/acct2");
    let s = screen(&mut a);
    let in_modal = |k: &str| c(s.lines().find(|l| l.contains('┃') && l.contains(k)).unwrap_or_else(|| panic!("{k} not in the modal:\n{s}")));
    assert!(in_modal("fable cap").contains("fable cap 50 % inherits 50 %"), "{s}");
    assert!(in_modal("weekly cap").contains("weekly cap 90 % inherits 90 %"), "{s}");
    // An account sets its own.
    while a.modal_entry().is_some_and(|e| !matches!(e, tui::cauldron::menu::Entry::Set(b) if b.key == "pools.{pool}.accounts.{account}.budget.session.cap_pct")) {
        a.handle(key(KeyCode::Down));
    }
    a.handle(key(KeyCode::Enter));
    a.handle(Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)));
    typed(&mut a, "40");
    a.handle(key(KeyCode::Enter));
    let s = screen(&mut a);
    assert!(c(s.lines().find(|l| l.contains('┃') && l.contains("session cap")).unwrap()).contains("session cap 40 % inherits off ↺"), "{s}");
}

// ── msg 186: budgets apply to the cauldron only ─────────────────────────────

/// "we should not be able to configure resources to specific surfaces ...
/// Really, the budgets should only apply to the cauldron": F47's cluster
/// budgets are rolled back at the source — no setting, no band, no default.
#[test]
fn a_cluster_has_no_budget_token_or_resource_setting_and_the_defaults_tab_holds_none_for_clusters() {
    let mut a = open(&scratch_root());
    for k in tui::cauldron::settings::DEFS.iter().map(|d| d.key) {
        assert!(!k.starts_with("cluster.budget") && !k.starts_with("budgets.cluster_default") && k != "cluster.pools", "{k}");
        assert!(!matches!(k, "cluster.ripples.max_concurrent" | "cluster.ripples.max_steps" | "cluster.ripples.opportunistic_eligible"), "{k}");
    }
    cauldron_settings(&mut a, "providers");
    assert!(!screen(&mut a).contains("CLUSTER DEFAULTS"));
    // msg 235: a cluster has no Config tab at all.
    assert_eq!(KTab::parse("config"), None);
    assert!(tui::cauldron::settings::DEFS.iter().all(|d| !d.key.starts_with("cluster.")));
}

// ── F44: local inference's disk bound; RAM from the machine ─────────────────

#[test]
fn local_inference_has_a_disk_cap_and_the_ram_note_is_this_machines() {
    let mut a = open(&scratch_root());
    cauldron_settings(&mut a, "local");
    let s = screen(&mut a);
    assert!(c(line_with(&s, "disk cap")).contains("disk cap 20 GB"), "{s}");
    // Bounded by the machine's disk cap.
    let disk = Bound::new("local.disk_max_gb", Ctx::default());
    a.sset(&Bound::new("machine.disk.cap_gb", Ctx::default()), Val::Int(30)).unwrap();
    assert!(a.sset(&disk, Val::Int(31)).unwrap_err().contains("at most 30 GB"));
    a.sset(&disk, Val::Int(25)).unwrap();
    // RAM: the default and its note come from THIS machine, never `of 64 GB`.
    cauldron_settings(&mut a, "machine");
    let s = screen(&mut a);
    let row = c(line_with(&s, "RAM max"));
    match tui::cauldron::settings::machine_ram_gb() {
        Some(total) => {
            assert!(row.contains(&format!("RAM max {} GB", total * 3 / 4)), "{row}");
            assert!(row.contains(&format!("75% of {total} GB")), "{row}");
        }
        None => assert!(row.contains("RAM max 48 GB") && !row.contains(" of "), "{row}"),
    }
}

// ── F15: the services are one band and their metadata lines up ─────────────

/// msg 235: Status › Mesh — each service a band NESTED in its device's band
/// (after the threads daemon's), its status and uptime in columns, ⚠ and ✕
/// flush right, so every service's columns line up.
#[test]
fn the_status_services_sit_in_their_device_and_their_metadata_lines_up() {
    let mut a = open(&scratch_root());
    a.set_ctab(CTab::Status);
    a.set_sttab(tui::cauldron::status::StatusTab::Mesh);
    let s = screen(&mut a);
    let lines: Vec<&str> = s.lines().collect();
    let head = lines.iter().position(|l| l.contains(&format!("▾ {}", a.dash.mesh[0].node))).expect(&s);
    let n = a.dash.mesh[0].services.len();
    // The device's band holds its services, in order.
    let mut from = head;
    let svcs: Vec<&str> = a.dash.mesh[0].services.iter().map(|x| {
        let at = from + lines[from..].iter().position(|l| l.contains(&format!("▸ {} ", x.name)) && !l.contains("threads daemon")).unwrap_or_else(|| panic!("{} under its device:\n{s}", x.name));
        from = at + 1;
        lines[at]
    }).collect();
    assert_eq!(svcs.len(), n);
    for (l, x) in svcs.iter().zip(&a.dash.mesh[0].services) {
        assert!(l.contains(&format!("▸ {}", x.name)), "every service right under its device, in order: {l}\n{s}");
        assert!(l.contains(&x.status), "a status on every service: {l}");
    }
    let col = |l: &str, w: &str| l.find(w).map(|b| l[..b].chars().count());
    let status_cols: Vec<Option<usize>> = svcs.iter().zip(&a.dash.mesh[0].services).map(|(l, x)| col(l, &format!(" {} ", x.status))).collect();
    assert!(status_cols.iter().all(|c| c.is_some() && *c == status_cols[0]), "the status column lines up:\n{s}");
    let ends: Vec<usize> = svcs.iter().filter(|l| l.contains('⚠')).map(|l| l.trim_end_matches(['│', ' ']).chars().count()).collect();
    assert!(ends.len() >= 2 && ends.iter().all(|e| *e == ends[0]), "the ⚠ ✕ metadata is flush right:\n{s}");
}

// ── F32: a second click opens, in the sidebar and in a modal's list ─────────

#[test]
fn a_second_click_on_the_selected_cluster_opens_it_and_a_modal_list_selects_first() {
    let mut a = open(&scratch_root());
    let ripples = a.forest.find("X-3").unwrap();
    // Collapse it, then: a click selects; a second expands; a third enters.
    a.select(ripples);
    a.collapsed.insert(ripples);
    a.select(a.forest.find("X-6").unwrap());
    click_on(&mut a, Hit::TreeRow(ripples));
    assert_eq!((a.selected, a.focus), (ripples, Focus::Tree));
    assert!(!a.is_expanded(ripples));
    click_on(&mut a, Hit::TreeRow(ripples));
    assert!(a.is_expanded(ripples), "the second click expands it");
    assert_eq!(a.focus, Focus::Tree);
    click_on(&mut a, Hit::TreeRow(ripples));
    assert_eq!(a.focus, Focus::Content, "a click on an open cluster steps into its page");
    // A picker: ONE click on an option chooses it (0214 A4).
    // A role is a table's row: its modal, its model first.
    cauldron_settings(&mut a, "roles/curator");
    assert!(matches!(&a.modal, Some(Modal::Settings { .. })), "{:?}", a.modal);
    a.handle(key(KeyCode::Enter));
    let Some(Modal::Picker { options, .. }) = &a.modal else { panic!("{:?}", a.modal) };
    let opus = options.iter().position(|o| o == "opus").unwrap();
    click_on(&mut a, Hit::Choice(opus));
    assert!(matches!(a.modal, Some(Modal::Settings { .. })), "back to the role's modal: {:?}", a.modal);
    assert_eq!(get(&a, "roles.{role}.model", Ctx::role("curator")).val, Val::Str("opus".into()));
}

// ── F25: Home and End in the cauldrons dropdown ─────────────────────────────

#[test]
fn home_and_end_move_in_the_cauldrons_dropdown() {
    let mut a = open(&scratch_root());
    a.handle(Event::Key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL)));
    assert!(a.modal.is_none(), "a dropdown, not a modal: {:?}", a.modal);
    let n = a.dropdown.as_ref().expect("the cauldrons dropdown").entries.len();
    a.handle(key(KeyCode::End));
    assert_eq!(a.dropdown.as_ref().map(|d| d.selected), Some(n), "End is `+`");
    a.handle(key(KeyCode::Home));
    assert_eq!(a.dropdown.as_ref().map(|d| d.selected), Some(0));
}

// ── F38: the new-cluster modal's fields have a caret ────────────────────────

#[test]
fn the_new_cluster_fields_have_a_caret_and_nothing_typed_is_thrown_away() {
    let mut a = open(&scratch_root());
    a.open_new_cluster(None);
    typed(&mut a, "Alpa");
    // ← moves in the text; typing inserts at the caret.
    a.handle(key(KeyCode::Left));
    typed(&mut a, "h");
    assert_eq!(modal_fields(&a).0, "Alpha");
    let s = screen(&mut a);
    assert!(line_with(&s, "› Alph").contains("› Alph▏a"), "the caret is drawn where it is:\n{s}");
    // ← at the start of a CHANGED field stays in the modal.
    a.handle(key(KeyCode::Home));
    a.handle(key(KeyCode::Left));
    assert!(a.modal.is_some(), "← at column 0 of a typed field does not escape");
    // → moves; → at the end goes on to the boundaries.
    a.handle(key(KeyCode::End));
    a.handle(key(KeyCode::Right));
    assert_eq!(modal_fields(&a).2, 1);
    typed(&mut a, "first line");
    a.handle(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)));
    typed(&mut a, "second");
    // ↑ moves between the boundaries' lines, keeping the column; ↑ past the
    // first goes to the name.
    a.handle(key(KeyCode::Up));
    typed(&mut a, "!");
    assert_eq!(modal_fields(&a).1, "first !line\nsecond");
    a.handle(key(KeyCode::Down));
    a.handle(key(KeyCode::End));
    typed(&mut a, "?");
    assert_eq!(modal_fields(&a).1, "first !line\nsecond?");
    a.handle(key(KeyCode::Up));
    a.handle(key(KeyCode::Up));
    assert_eq!(modal_fields(&a).2, 0, "↑ on the first line goes to the name");
    // Esc closes it and KEEPS what was typed: the next one opened at the
    // root comes back with it.
    a.handle(key(KeyCode::Esc));
    assert!(a.modal.is_none());
    a.open_new_cluster(None);
    assert_eq!(modal_fields(&a), ("Alpha".into(), "first !line\nsecond?".into(), 0));
    // A click outside keeps it too.
    a.modal = None;
    a.new_cluster_kept = None;
    a.open_new_cluster(None);
    typed(&mut a, "Beta");
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 0, row: 0, modifiers: KeyModifiers::NONE }));
    assert!(a.modal.is_none());
    a.open_new_cluster(None);
    assert_eq!(modal_fields(&a).0, "Beta");
    // ← at the start of a field as it OPENED leaves (and keeps it).
    a.handle(key(KeyCode::Home));
    a.handle(key(KeyCode::Left));
    assert!(a.modal.is_none(), "unchanged since it opened: ← leaves");
    // An empty one leaves on ← at once, and keeps nothing.
    a.new_cluster_kept = None;
    a.open_new_cluster(None);
    a.handle(key(KeyCode::Left));
    assert!(a.modal.is_none() && a.new_cluster_kept.is_none());
    // Made, it is cleared.
    a.open_new_cluster(None);
    typed(&mut a, "Gamma");
    a.handle(key(KeyCode::Enter));
    typed(&mut a, "the gamma work");
    a.handle(key(KeyCode::Enter));
    assert!(a.modal.is_none(), "{:?}", a.modal);
    assert!(a.new_cluster_kept.is_none());
    assert!(a.wait_seeding(std::time::Duration::from_secs(10)));
}

// ── F61 retired: msg 235 made the ripples read-only ─────────────────────────
//
// `+ ripple`, a made ripple's prompt and question template, and its
// fragment lived on the System tab, which msg 235 took away ("I know what
// triggers are present and I can explore them, but I don't want to be able
// to modify them"): the Bus shows the ripples and nothing edits them. Their
// tests went with the feature (gap_sweep holds the Bus's).

// ── F4: the log (msg 1: "I can look at a log and understand what happened") ─

fn select_item(a: &mut App, it: Item) {
    a.focus = Focus::Content;
    a.item = a.items().iter().position(|x| *x == it).unwrap_or_else(|| panic!("{it:?} not on the page: {:?}", a.items()));
}

/// The instant a log entry is placed by.
fn when(e: &LogEntry) -> Option<chrono::DateTime<chrono::Local>> {
    match e {
        LogEntry::Sent(s) => chrono::DateTime::parse_from_rfc3339(&s.at).ok().map(|t| t.with_timezone(&chrono::Local)),
        LogEntry::Run(_, r) => r.instant(),
    }
}

#[test]
fn a_clusters_log_reads_its_sends_and_the_runs_that_touched_it_newest_first_and_each_opens() {
    let root = scratch_root();
    let mut a = open(&root);
    let cauldron = a.forest.find("X-1").unwrap();
    a.select(cauldron);
    // msg 235: an untagged dump lands on no cluster, so this one names it.
    a.dump("a plain note, landing where it names [cluster:X-1]");
    a.dump("> [Q-1] option four, with the floor, and a long tail that runs well past one row of the page so that opening it shows all of it, every word, down to the last one here");
    a.dump("only for [cluster:X-2]");
    // Overview: no send list and no log (msg 165: the log is at the foot of
    // the Status tab); there, one `log ▸` row with the count.
    let s = screen(&mut a);
    assert!(!s.contains("a plain note") && !s.contains("SENT") && !s.contains("log ▸"), "{s}");
    a.set_ktab(KTab::Status);
    let s = screen(&mut a);
    assert!(!s.contains("a plain note"), "{s}");
    let entries = log_entries(&a);
    let n = entries.len();
    assert!(c(line_with(&s, "log ▸")).trim_end_matches(['│', ' ']).ends_with(&format!("log ▸ {n}")), "{}", line_with(&s, "log ▸"));
    // What is on it: the two sends that landed on Cauldron, and the runs that
    // ran on it — not Broomstick's, and not the send that landed below.
    let sends: Vec<u64> = entries.iter().filter_map(|e| if let LogEntry::Sent(s) = e { Some(s.seq) } else { None }).collect();
    assert_eq!(sends, vec![13, 12, 11], "newest first, and only those that landed here (11 is the seed's)");
    let runs: Vec<&str> = entries.iter().filter_map(|e| if let LogEntry::Run(_, r) = e { Some(r.id.as_str()) } else { None }).collect();
    for id in ["r-9c2e", "r-8f3a", "r-5b07", "r-44c1", "r-0f1d"] {
        assert!(runs.contains(&id), "{id} ran on Cauldron: {runs:?}");
    }
    assert!(!runs.contains(&"r-2e90") && !runs.contains(&"r-61d0"), "Broomstick's and Harness's are not: {runs:?}");
    let times: Vec<_> = entries.iter().map(when).collect();
    assert!(times.windows(2).all(|w| w[0] >= w[1]), "newest first: {times:?}");
    drop(entries);
    // → opens it as a trace page, in the trace grammar: the crumb, day rules.
    select_item(&mut a, Item::Footer(Footer::Log));
    a.handle(key(KeyCode::Right));
    assert_eq!(a.trace_top(), Some(Item::Log));
    let s = screen(&mut a);
    assert!(s.contains("Status › log"), "{s}");
    assert!(s.contains("▾ Today "), "{s}");
    let row = c(line_with(&s, "#13"));
    assert!(row.contains("› ") && row.contains("> [Q-1] option four") && row.trim_end_matches(['│', ' ']).ends_with("Cauldron"), "time, number, text, where it landed: {row}");
    let rows: Vec<&str> = s.lines().collect();
    let at13 = rows.iter().position(|l| l.contains("#13")).unwrap();
    // The seed's runs keep today's clock times, so whether #13 is the first
    // (selected, its background on both its lines) row depends on the hour.
    assert_eq!(c(rows[at13 + 1].split('│').nth(2).unwrap().trim_start().trim_start_matches('▌')), "[Q-1]", "its tags on the line under it");
    assert!(!s.contains("last one here"), "collapsed, it is one row");
    let r8 = c(line_with(&s, "6 atoms"));
    assert!(r8.contains("✓") && r8.contains("dump") && r8.contains("8e4fca10#96 →") && r8.trim_end_matches(['│', ' ']).ends_with("r-8f3a"), "the runs page's row: {r8}");
    assert!(!s.contains("only for"), "{s}");
    // → on a send opens its whole text in place; ← closes it.
    select_item(&mut a, Item::Sent(13));
    a.handle(key(KeyCode::Right));
    let s = screen(&mut a);
    assert!(s.contains("Status › log › #13"), "{s}");
    assert!(s.contains("last one here"), "the whole text: {s}");
    // `i` quotes it.
    a.handle(key(KeyCode::Char('i')));
    assert_eq!(a.composer.text(), "> [dump:13]");
    a.focus = Focus::Content;
    a.handle(key(KeyCode::Left));
    assert_eq!(a.trace_top(), Some(Item::Log));
    // → on a run opens its run page; ← comes back to the log, on that row.
    let ix = a.rstore.indexed_runs().find(|(_, r)| r.id == "r-8f3a").unwrap().0;
    select_item(&mut a, Item::Run(ix));
    let at = a.item;
    a.handle(key(KeyCode::Right));
    // msg 235: a run opens in the Bus modal; closing it leaves him on the row.
    assert!(matches!(&a.modal, Some(Modal::Bus { stack }) if matches!(stack.last(), Some((tui::cauldron::bus::BusPage::Run(r), _, _)) if r == "r-8f3a")), "{:?}", a.modal);
    a.handle(key(KeyCode::Left));
    assert!(a.modal.is_none());
    assert_eq!((a.ctab, a.trace_top(), a.item), (CTab::Cauldron, Some(Item::Log), at));
    a.handle(key(KeyCode::Char('i')));
    assert!(a.composer.text().ends_with("> [run:r-8f3a]"), "{}", a.composer.text());
    // Under ^G it covers everything below: the send to Interface, and the
    // run on Ripples.
    a.focus = Focus::Content;
    a.toggle_aggregate();
    let s = screen(&mut a);
    assert!(s.contains("only for"), "{s}");
    assert!(s.contains("r-3c77"), "{s}");
    // Back to Overview.
    a.trace.clear();
    a.aggregate = false;
    a.set_ktab(KTab::Details);
    // A run that ran elsewhere but WROTE a node in a region a cluster claims
    // touched it: r-5b07 ran on Cauldron and wrote T-9, in R-3, which
    // Curation claims.
    a.select(a.forest.find("X-5").unwrap());
    let ids: Vec<String> = log_entries(&a).iter().filter_map(|e| if let LogEntry::Run(_, r) = e { Some(r.id.clone()) } else { None }).collect();
    assert!(ids.contains(&"r-5b07".to_string()), "{ids:?}");
    assert!(!ids.contains(&"r-44c1".to_string()), "a Cauldron run that wrote nothing here is not: {ids:?}");
    // A send tagged with a run lands on the cluster the run ran on.
    assert_eq!(a.clusters_of(&tui::cauldron::tags::Tag::thing("run", "r-2e90")), vec!["X-6".to_string()]);
    assert_eq!(a.clusters_of(&tui::cauldron::tags::Tag::thing("send", "14")), vec!["X-2".to_string()]);
}

#[test]
fn a_run_recorded_here_keeps_its_cluster_by_key_and_its_day_by_instant() {
    let root = scratch_root();
    let mut a = open(&root);
    a.open_new_cluster(None);
    typed(&mut a, "Gamma");
    a.handle(key(KeyCode::Enter));
    typed(&mut a, "the gamma work");
    a.handle(key(KeyCode::Enter));
    assert!(a.wait_seeding(std::time::Duration::from_secs(10)));
    let run = a.rstore.local_runs.last().expect("the seeding run").clone();
    let key = a.forest.clusters.iter().find(|g| g.name == "Gamma").unwrap().key.clone();
    assert_eq!(run.cluster, key, "the minted id, not the name");
    let at = run.at.as_deref().expect("an instant");
    assert!(chrono::DateTime::parse_from_rfc3339(at).is_ok(), "{at}");
    assert_eq!(run.day, chrono::Local::now().format("%Y-%m-%d").to_string(), "a date, not the word `today`");
    assert_eq!(run.day_word(), "today");
    // A day later it is yesterday's, and the runs page groups it so.
    let mut old = run.clone();
    old.at = Some((chrono::Local::now() - chrono::Duration::days(1)).to_rfc3339());
    assert_eq!(old.day_word(), "yesterday");
    // Renamed, the cluster still finds its run: its log has it.
    let g = a.forest.find(&key).unwrap();
    let dir = tui::cauldron::store::soup_dir(&a.env).unwrap();
    tui::cauldron::store::CreatedClusters::new(&dir).rename(&key, "Gamma Prime").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    a.sync();
    assert_eq!(a.forest.get(g).name, "Gamma Prime");
    a.select(a.forest.find(&key).unwrap());
    assert!(log_entries(&a).iter().any(|e| matches!(e, LogEntry::Run(_, r) if r.id == run.id)));
    // And its run page on the Bus names it by its name now.
    a.open_bus(tui::cauldron::bus::BusPage::Run(run.id.clone()));
    let laid = a.bus_laid(tui::cauldron::bus::BUS_WIDTH).unwrap();
    let text: String = std::iter::once(laid.title.clone()).chain(laid.lines.iter().map(|l| l.to_string())).collect::<Vec<_>>().join("\n");
    assert!(text.contains("Gamma Prime"), "{text}");
}

// ── F13: a typo'd environment is never made silently ────────────────────────

#[test]
fn a_typod_environment_is_refused_off_a_terminal_and_nothing_is_made() {
    let root = scratch_root();
    std::fs::create_dir_all(root.join("data").join("dev")).unwrap();
    let run = |args: &[&str], env: Option<&str>| {
        let mut c = crate::face::Face::new(&root.with_extension("face"));
        c.arg("--cauldron").args(args).env("CAULDRON_HOME", &root).env_remove("ENV").stdin(std::process::Stdio::null());
        match env {
            Some(e) => c.env("CAULDRON_ENV", e),
            None => c.env_remove("CAULDRON_ENV"),
        };
        let o = c.output().unwrap();
        (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr))
    };
    for (args, env) in [(vec!["--dump", "--env", "prdo"], None), (vec!["--dump", "prdo"], None), (vec!["--dump"], Some("prdo")), (vec!["prdo"], None)] {
        let (ok, out) = run(&args, env);
        assert!(!ok && out.contains("no environment `prdo` (dev)"), "{args:?} {env:?}: {out}");
    }
    assert!(!root.join("data").join("prdo").exists(), "nothing was made");
    // One that exists opens; so does prod, named or not, made or not.
    for (args, env) in [(vec!["--dump", "--env", "dev"], None), (vec!["--dump", "--env", "prod"], None), (vec!["--dump"], Some("prod")), (vec!["--dump"], None)] {
        let (ok, out) = run(&args, env);
        assert!(ok, "{args:?} {env:?}: {out}");
    }
}
