//! Iteration nine (task 0103): every gap the earlier build reports recorded
//! that the face can close on fake data, ported to the msg-235 face
//! (Surface, Threads, Status, Config; Config in collapsible bands). Real seed
//! files, real keys, real frames on ratatui's TestBackend, and real files
//! under a scratch cauldron root (never `~/.cauldron`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui::cauldron::app::{App, CTab, Focus, Hit, Item, Modal, Outbox, KTab};
use tui::cauldron::manage::Act;
use tui::cauldron::menu::{Entry, SPage, SETTINGS_ROW};
use tui::cauldron::render_to_text;
use tui::cauldron::store::{seed_dir, soup_dir};

// ── fixtures ────────────────────────────────────────────────────────────────

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "gap-sweep-{}-{}-{}",
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

fn open(root: &Path) -> App {
    // Hermetic: no test here may make a billed type-one call, whatever is on
    // PATH (a cluster made here seeds; tests/it/seed_live.rs is the live proof).
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    crate::live_pool::seeded(App::open(env_in(root, "prod"), seed_dir(), true).expect("seed loads"))
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

fn id(a: &App, key: &str) -> usize {
    a.forest.find(key).unwrap_or_else(|| panic!("no cluster {key}"))
}

/// The cluster named `name` (a cluster made here has a minted id, `X-<n>`).
fn named(a: &App, name: &str) -> Option<usize> {
    a.forest.clusters.iter().position(|g| g.name == name)
}

/// Make a cluster through the modal, as he would: `+` (or `+` under
/// `parent`), its name, ↓, its boundaries, Enter.
fn make_cluster(a: &mut App, parent: Option<&str>, name: &str, boundaries: &str) -> usize {
    let p = parent.map(|k| id(a, k));
    a.open_new_cluster(p);
    typed(a, name);
    a.handle(key(KeyCode::Down));
    typed(a, boundaries);
    a.handle(key(KeyCode::Enter));
    assert!(a.modal.is_none(), "{:?}", a.modal);
    assert!(a.wait_seeding(std::time::Duration::from_secs(10)), "the seeding landed");
    let g = named(a, name).unwrap_or_else(|| panic!("no cluster named {name}"));
    assert!(tui::cauldron::tags::is_cluster_id(&a.forest.get(g).key), "a minted id: {}", a.forest.get(g).key);
    g
}

/// Put the selection on the Config row for which `pick` holds.
fn select_row(a: &mut App, pick: impl Fn(&Entry) -> bool) {
    let i = a.selectable().iter().position(pick).unwrap_or_else(|| panic!("no such row (open: {:?}): {:?}", a.snav.open, a.selectable()));
    a.rows[SETTINGS_ROW] = i;
}

fn select_do(a: &mut App, label: &str) {
    select_row(a, |e| matches!(e, Entry::Do { label: l, .. } if l == label));
}

/// A made-here cluster's Overview, with the selection on its `rename` (0),
/// `move under` (1) or `delete` (2) button at its foot (msg 235: the cluster
/// has no Config tab).
fn manage(a: &mut App, g: usize, k: u8) {
    a.set_ctab(CTab::Cauldron);
    a.select(g);
    a.set_ktab(KTab::Details);
    a.focus = Focus::Content;
    a.item = a.items().iter().position(|i| *i == Item::Manage(k)).unwrap_or_else(|| panic!("no manage row {k}: {:?}", a.items()));
}

fn cauldron_settings(a: &mut App, path: &str) {
    a.set_ctab(CTab::Settings);
    a.focus = Focus::Content;
    a.snav = tui::cauldron::menu::Nav::cauldron();
    a.rows[SETTINGS_ROW] = 0;
    if !path.is_empty() {
        a.open_settings_path(path).unwrap();
    }
}

// ── the pre-rename aliases are retired at the source ────────────────────────

#[test]
fn the_seed_is_clusters_json_under_the_clusters_key_and_the_old_names_are_refused() {
    let seeds = seed_dir();
    assert!(seeds.join("clusters.json").is_file());
    assert!(!seeds.join("globs.json").exists() && !seeds.join("surfaces.json").exists(), "the old seed names are gone");
    for f in ["clusters.json", "soup/dev.json"] {
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(seeds.join(f)).unwrap()).unwrap();
        assert!(v.get("clusters").is_some() && v.get("globs").is_none() && v.get("surfaces").is_none(), "{f}");
    }
    // The `globs` key no longer reads.
    let e = serde_json::from_str::<tui::cauldron::model::DataFile>(r#"{"globs":[{"id":"X-1","name":"X","description":""}]}"#).unwrap_err();
    assert!(e.to_string().contains("clusters"), "{e}");
    // A seed directory holding only the old name has no prod seed.
    let only_old = scratch_root();
    std::fs::write(only_old.join("globs.json"), "{}").unwrap();
    assert_eq!(tui::cauldron::store::seed_path(&only_old, "prod"), None);
}

// ── another face's change arrives ───────────────────────────────────────────

#[test]
fn a_cluster_made_renamed_or_deleted_in_one_face_appears_in_another_on_the_same_environment() {
    let root = scratch_root();
    let mut a = open(&root);
    let mut b = open(&root);
    let g = make_cluster(&mut a, Some("X-1"), "Billing Watch", "invoices and stripe events");
    let made = a.forest.get(g).key.clone();
    assert!(b.forest.find(&made).is_none());
    // Where b is stays put across the change: its Tasks tab, on Broomstick.
    let bs = id(&b, "X-6");
    b.select(bs);
    b.set_ktab(KTab::Questions);
    b.sync();
    let gb = id(&b, &made);
    assert_eq!(b.forest.get(gb).parent.map(|p| b.forest.get(p).key.clone()).as_deref(), Some("X-1"));
    assert_eq!(b.forest.get(b.selected).key, "X-6");
    assert!(screen(&mut b).contains("Billing Watch"));
    // A rename arrives without moving b's page.
    manage(&mut a, g, 0);
    a.handle(key(KeyCode::Enter));
    a.handle(Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)));
    typed(&mut a, "Billing");
    a.handle(key(KeyCode::Enter));
    b.item = 1;
    b.sync();
    assert_eq!(b.forest.get(id(&b, &made)).name, "Billing");
    assert_eq!((b.ktab, b.item), (KTab::Questions, 1), "a rename keeps where he is");
    // A delete arrives too.
    let g = id(&a, &made);
    manage(&mut a, g, 2);
    a.handle(key(KeyCode::Enter));
    a.handle(key(KeyCode::Enter));
    b.sync();
    assert!(b.forest.find(&made).is_none());
    assert_eq!(b.forest.get(b.selected).key, "X-6");
}

#[test]
fn a_settings_edit_an_account_and_a_pool_in_one_face_reach_another() {
    let root = scratch_root();
    let mut a = open(&root);
    let mut b = open(&root);
    cauldron_settings(&mut a, "providers");
    assert!(a.select_setting("budgets.default.weekly.threshold_pct"));
    a.handle(key(KeyCode::Char('-')));
    let bd = tui::cauldron::menu::Bound::new("budgets.default.weekly.threshold_pct", Default::default());
    let now = a.sget(&bd).val;
    assert_ne!(b.sget(&bd).val, now);
    b.sync();
    assert_eq!(b.sget(&bd).val, now);
    // An account added, an inference endpoint added (design 0199).
    // V1.6: on a seed pool — Claude Code's roster is the daemon's.
    a.settings.add_account("codex", "fresh one", |_| false).unwrap();
    a.settings.draft_endpoint(false, "local");
    let ep = a.settings.add_endpoint(false).unwrap();
    b.sync();
    assert!(b.accounts("codex").iter().any(|x| x.label == "fresh one"));
    assert!(b.settings.endpoint_ids(false).contains(&ep), "{ep}");
    // The token sources are GLOBAL (msg 186): ANOTHER cauldron's face reads
    // the same account, the same threshold — and keeps its own switches.
    std::fs::create_dir_all(root.join("data/dev")).unwrap();
    let mut d = App::open(env_in(&root, "dev"), seed_dir(), true).unwrap();
    assert!(d.accounts("codex").iter().any(|x| x.label == "fresh one"));
    assert_eq!(d.sget(&bd).val, now);
    let wk = tui::cauldron::menu::Bound::new("pools.{pool}.budget.weekly.threshold_pct", tui::cauldron::settings::Ctx::pool("claude_code"));
    a.sset(&wk, tui::cauldron::settings::Val::Int(55)).unwrap();
    d.sync();
    assert_eq!(d.sget(&wk).val, tui::cauldron::settings::Val::Int(55), "a global edit in prod reaches dev");
    let use_acct = tui::cauldron::menu::Bound::new("cauldron.accounts.{pool}.{account}.allowed", tui::cauldron::settings::Ctx::account("claude_code", "acct2"));
    a.sset(&use_acct, tui::cauldron::settings::Val::Bool(false)).unwrap();
    d.sync();
    assert!(!a.account_allowed("claude_code", "acct2") && d.account_allowed("claude_code", "acct2"), "what a cauldron takes of them is its own");
}

#[test]
fn the_ripple_runs_reach_another_face() {
    let root = scratch_root();
    let mut a = open(&root);
    let mut b = open(&root);
    let n = b.rstore.runs().len();
    make_cluster(&mut a, None, "Seeded", "the cluster tree and the dump line");
    b.sync();
    assert_eq!(b.rstore.runs().len(), n + 1, "the seeding run");
}

// ── clusters made here: rename, delete, move ────────────────────────────────

#[test]
fn a_cluster_made_here_renames_moves_under_another_and_deletes_with_a_confirm() {
    let root = scratch_root();
    let mut a = open(&root);
    let x = make_cluster(&mut a, None, "Alpha", "the alpha work");
    let alpha = a.forest.get(x).key.clone();
    let y = make_cluster(&mut a, None, "Beta", "the beta work");
    let beta = a.forest.get(y).key.clone();
    // Gamma under Alpha: once Alpha moves, Gamma sits BEFORE its parent in
    // the file.
    let z = make_cluster(&mut a, Some(&alpha), "Gamma", "the gamma work");
    let gamma = a.forest.get(z).key.clone();
    // A seed cluster offers no such rows; its Cluster band says it is seeded.
    let h = id(&a, "X-9");
    a.select(h);
    a.set_ktab(KTab::Details);
    assert!(!a.items().iter().any(|i| matches!(i, Item::Manage(_))));
    // Rename.
    let x = id(&a, &alpha);
    manage(&mut a, x, 0);
    let s = screen(&mut a);
    assert!(s.contains("• Cluster") && line_with(&s, " rename ").contains("Alpha"), "{s}");
    a.handle(key(KeyCode::Right));
    assert!(matches!(&a.modal, Some(Modal::Name { name, .. }) if name == "Alpha"), "{:?}", a.modal);
    for _ in 0..5 {
        a.handle(key(KeyCode::Backspace));
    }
    typed(&mut a, "Alpha Prime");
    a.handle(key(KeyCode::Enter));
    assert!(a.modal.is_none());
    let x = id(&a, &alpha);
    assert_eq!(a.forest.get(x).name, "Alpha Prime", "the id stays, the name changes");
    // Move under Beta: a picker of every cluster not below it, and the root.
    manage(&mut a, x, 1);
    a.handle(key(KeyCode::Enter));
    let Some(Modal::Picker { options, .. }) = a.modal.clone() else { panic!("{:?}", a.modal) };
    assert_eq!(options[0], "the root");
    assert!(!options.iter().any(|o| o.ends_with("Alpha Prime")), "not under itself: {options:?}");
    let to = options.iter().position(|o| o == "Beta").unwrap();
    a.handle(key(KeyCode::Home));
    for _ in 0..to {
        a.handle(key(KeyCode::Down));
    }
    a.handle(key(KeyCode::Enter));
    let (x, y) = (id(&a, &alpha), id(&a, &beta));
    assert_eq!(a.forest.get(x).parent, Some(y));
    assert_eq!(a.selected, x, "the moved cluster stays selected");
    // Beta cannot now go under Alpha (below it): the picker never offers it,
    // and the store refuses it.
    let e = tui::cauldron::store::CreatedClusters::new(&soup_dir(&a.env).unwrap()).set_parent(&beta, Some(alpha.clone()), |_| true).unwrap_err();
    assert!(format!("{e:#}").contains("below it"), "{e:#}");
    // A restart reads the same tree, though Gamma now comes before Alpha.
    let list = tui::cauldron::store::CreatedClusters::new(&soup_dir(&a.env).unwrap()).list().unwrap();
    let pos = |k: &str| list.iter().position(|s| s.id == k).unwrap();
    assert!(pos(&gamma) < pos(&alpha), "the case this checks");
    let b = open(&root);
    assert_eq!(b.forest.get(id(&b, &alpha)).parent, Some(id(&b, &beta)));
    assert_eq!(b.forest.get(id(&b, &gamma)).parent, Some(id(&b, &alpha)));
    // Delete Beta: the confirm names what goes; Esc keeps it.
    manage(&mut a, y, 2);
    assert!(line_with(&screen(&mut a), " delete ").contains("and the 2 under it"));
    a.handle(key(KeyCode::Enter));
    let s = screen(&mut a);
    assert!(s.contains("Delete Beta?") && s.contains("The 2 clusters under it go with it.") && !s.contains("Esc keeps it"), "no key hints (F33): {s}");
    a.handle(key(KeyCode::Esc));
    assert!(a.modal.is_none() && a.forest.find(&beta).is_some());
    a.handle(key(KeyCode::Enter));
    a.handle(key(KeyCode::Right));
    assert!(a.modal.is_none());
    assert!(a.forest.find(&beta).is_none() && a.forest.find(&alpha).is_none() && a.forest.find(&gamma).is_none());
    assert_eq!(a.focus, Focus::Tree);
}

/// N5 (msg 235): a delete that leaves a region no cluster claims gives it to
/// the UNOWNED cluster, made at the root the first time, and kept after.
#[test]
fn n5_a_delete_that_orphans_regions_gives_them_to_an_unowned_cluster() {
    let root = scratch_root();
    let mut a = open(&root);
    let g = make_cluster(&mut a, None, "Orphan maker", "machines joining and leaving the mesh");
    let mut claims = a.forest.get(g).claims.clone();
    if claims.is_empty() {
        // The local matcher found nothing: give it a region no seed cluster claims.
        let free = a.corpus.regions.iter().map(|r| r.id.clone()).find(|r| !(0..a.forest.clusters.len()).any(|c| a.forest.get(c).claims.contains(r))).expect("an unclaimed region");
        let key = a.forest.get(g).key.clone();
        tui::cauldron::store::CreatedClusters::new(&soup_dir(&a.env).unwrap()).seeded(&key, tui::cauldron::store::Claims { regions: vec![free.clone()], ..Default::default() }, None).unwrap();
        a.sync();
        claims = vec![free];
    }
    let orphaned: Vec<String> = claims.iter().filter(|r| !(0..a.forest.clusters.len()).any(|c| a.forest.get(c).key != a.forest.get(g).key && a.forest.get(c).claims.contains(r))).cloned().collect();
    assert!(!orphaned.is_empty(), "the case this checks: {claims:?}");
    let g = id(&a, &a.forest.get(g).key.clone());
    manage(&mut a, g, 2);
    a.handle(key(KeyCode::Enter));
    a.handle(key(KeyCode::Enter));
    assert!(a.notice.as_deref().unwrap_or("").contains("to Unowned"), "{:?} {:?}", a.notice, a.error);
    let u = named(&a, "Unowned").expect("the Unowned cluster");
    assert!(a.forest.get(u).parent.is_none() && tui::cauldron::tags::is_cluster_id(&a.forest.get(u).key));
    for r in &orphaned {
        assert!(a.forest.get(u).claims.contains(r), "{r} went to Unowned");
    }
    // A restart reads it.
    let b = open(&root);
    assert!(named(&b, "Unowned").is_some());
}

// ── accounts and pools ──────────────────────────────────────────────────────

#[test]
fn an_account_renames_and_removes_with_its_settings_and_a_pool_is_added() {
    let root = scratch_root();
    let mut a = open(&root);
    // Rename a seed account, from its row's modal: its id (and its
    // settings) stay. Every action there is a button (msg 235).
    cauldron_settings(&mut a, "providers/claude_code/acct3");
    assert!(matches!(&a.modal, Some(Modal::Settings { .. })), "{:?}", a.modal);
    let buttons: Vec<String> = a.modal_selectable(&SPage::Account("claude_code".into(), "acct3".into())).into_iter().filter(|e| e.is_button()).map(|e| match e {
        Entry::Do { label, .. } => label,
        _ => String::new(),
    }).collect();
    // V1.6: one of the daemon's accounts — its pool switch and its login are
    // the daemon's; it is renamed here (a label) and never removed here.
    assert_eq!(buttons, ["take out of the pool", "log in", "rename"]);
    modal_do(&mut a, "rename");
    a.handle(key(KeyCode::Enter));
    assert!(matches!(&a.modal, Some(Modal::Name { .. })), "{:?}", a.modal);
    a.handle(Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)));
    typed(&mut a, "Backup card");
    a.handle(key(KeyCode::Enter));
    assert!(matches!(&a.modal, Some(Modal::Settings { .. })), "the rename goes back to the account's modal: {:?}", a.modal);
    let acct = a.accounts("claude_code").into_iter().find(|x| x.id == "acct3").unwrap();
    assert_eq!(acct.label, "Backup card");
    assert!(screen(&mut a).contains("┏ Claude Code · Backup card "), "{}", screen(&mut a));
    let note = tui::cauldron::menu::Bound::new("pools.{pool}.accounts.{account}.note", tui::cauldron::settings::Ctx::account("claude_code", "acct3"));
    assert!(a.sget(&note).val != tui::cauldron::settings::Val::Str(String::new()), "its settings stay through a rename");
    // V1.6: no `remove` here for one of the daemon's accounts (its roster is
    // the daemon's); the name he gave it outlives a restart.
    let b = open(&root);
    assert_eq!(b.accounts("claude_code").into_iter().find(|x| x.id == "acct3").map(|x| x.label).as_deref(), Some("Backup card"));
    a.handle(key(KeyCode::Esc));
    assert!(a.modal.is_none(), "{:?}", a.modal);
    // An account added here removes outright.
    a.settings.add_account("claude_code", "temp", |_| false).unwrap();
    a.settings.remove_account("claude_code", "temp").unwrap();
    assert!(a.settings.added.is_empty());
    // No `+ add token source` (design 0199): what is not a subscription is
    // an inference endpoint, added from a template.
    cauldron_settings(&mut a, "providers");
    assert!(!screen(&mut a).contains("+ add token source"));
    cauldron_settings(&mut a, "providers/endpoints");
    // V1.7 (R-V1 item 7): `+ add endpoint` is greyed — pressed, it says so,
    // opens nothing and adds nothing.
    let before = a.settings.endpoint_ids(false).len();
    select_do(&mut a, "+ add endpoint");
    a.handle(key(KeyCode::Enter));
    assert!(a.modal.is_none(), "{:?}", a.modal);
    assert_eq!(a.notice.as_deref(), Some(tui::cauldron::settings::GREYED));
    assert_eq!(a.settings.endpoint_ids(false).len(), before);
}

/// V1.7b F1: the body of `an_account_renames_and_removes_with_its_settings_and_a_pool_is_added` at 59c5ec8f, verbatim — the positive
/// spec the greyed endpoints lost (V1.7-I F1). Un-ignore when
/// `inference_live` ungreys them; `an_account_renames_and_removes_with_its_settings_and_a_pool_is_added` above is the refusal it pairs with.
#[test]
#[ignore = "endpoints greyed in V1.7; restore when un-greyed"]
fn an_account_renames_and_removes_with_its_settings_and_a_pool_is_added_ungreyed() {
    let root = scratch_root();
    let mut a = open(&root);
    // Rename a seed account, from its row's modal: its id (and its
    // settings) stay. Every action there is a button (msg 235).
    cauldron_settings(&mut a, "providers/claude_code/acct3");
    assert!(matches!(&a.modal, Some(Modal::Settings { .. })), "{:?}", a.modal);
    let buttons: Vec<String> = a.modal_selectable(&SPage::Account("claude_code".into(), "acct3".into())).into_iter().filter(|e| e.is_button()).map(|e| match e {
        Entry::Do { label, .. } => label,
        _ => String::new(),
    }).collect();
    assert_eq!(buttons, ["rename", "remove"]);
    modal_do(&mut a, "rename");
    a.handle(key(KeyCode::Enter));
    assert!(matches!(&a.modal, Some(Modal::Name { .. })), "{:?}", a.modal);
    a.handle(Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)));
    typed(&mut a, "Backup card");
    a.handle(key(KeyCode::Enter));
    assert!(matches!(&a.modal, Some(Modal::Settings { .. })), "the rename goes back to the account's modal: {:?}", a.modal);
    let acct = a.accounts("claude_code").into_iter().find(|x| x.id == "acct3").unwrap();
    assert_eq!(acct.label, "Backup card");
    assert!(screen(&mut a).contains("┏ Claude Code · Backup card "), "{}", screen(&mut a));
    let note = tui::cauldron::menu::Bound::new("pools.{pool}.accounts.{account}.note", tui::cauldron::settings::Ctx::account("claude_code", "acct3"));
    assert!(a.sget(&note).val != tui::cauldron::settings::Val::Str(String::new()), "its settings stay through a rename");
    // Remove it: asked first; its settings go with it, and its modal.
    modal_do(&mut a, "remove");
    a.handle(key(KeyCode::Enter));
    assert!(matches!(a.modal, Some(Modal::Confirm { act: Act::RemoveAccount { .. }, .. })));
    a.handle(key(KeyCode::Enter));
    assert!(a.modal.is_none(), "{:?}", a.modal);
    assert!(!a.accounts("claude_code").iter().any(|x| x.id == "acct3"));
    // Its row is gone; the source's band stays open, the selection in range.
    assert!(a.snav.is_open(&SPage::Pool("claude_code".into())), "back in the table");
    assert!(a.rows[SETTINGS_ROW] < a.selectable().len());
    assert!(!a.selectable().iter().any(|e| matches!(e, Entry::Row(SPage::Account(_, x))  if x == "acct3")));
    let text = std::fs::read_to_string(a.settings.global_path()).unwrap();
    assert!(!text.contains("acct3"), "{text}");
    // A restart agrees.
    let b = open(&root);
    assert!(!b.accounts("claude_code").iter().any(|x| x.id == "acct3"));
    // An account added here removes outright.
    a.settings.add_account("claude_code", "temp", |_| false).unwrap();
    a.settings.remove_account("claude_code", "temp").unwrap();
    assert!(a.settings.added.is_empty());
    // No `+ add token source` (design 0199): what is not a subscription is
    // an inference endpoint, added from a template.
    cauldron_settings(&mut a, "providers");
    assert!(!screen(&mut a).contains("+ add token source"));
    cauldron_settings(&mut a, "providers/endpoints");
    select_do(&mut a, "+ add endpoint");
    a.handle(key(KeyCode::Enter));
    assert!(matches!(&a.modal, Some(Modal::Settings { page: SPage::Endpoint { here: false, id }, .. }) if id == "ep-0"), "{:?}", a.modal);
    // It waits for its key: pressed now, it says so and adds nothing.
    let before = a.settings.endpoint_ids(false).len();
    modal_do(&mut a, "add endpoint");
    a.handle(key(KeyCode::Enter));
    assert!(a.notice.as_deref().is_some_and(|n| n.contains("needs a key")), "{:?}", a.notice);
    assert_eq!(a.settings.endpoint_ids(false).len(), before);
}

/// Put the row modal's selection on its button `label`.
fn modal_do(a: &mut App, label: &str) {
    let Some(Modal::Settings { page, .. }) = a.modal.clone() else { panic!("no row modal: {:?}", a.modal) };
    let i = a.modal_selectable(&page).iter().position(|e| matches!(e, Entry::Do { label: l, .. } if l == label)).unwrap_or_else(|| panic!("no button {label}"));
    if let Some(Modal::Settings { sel, .. }) = &mut a.modal {
        *sel = i;
    }
}

// ── `i` on settings rows ────────────────────────────────────────────────────

#[test]
fn i_on_a_settings_row_quotes_its_setting() {
    let root = scratch_root();
    let mut a = open(&root);
    // A row quotes its setting (the first under THIS cauldron's strip: its
    // switch, msg 186); a band header the prefix it governs.
    // V1.1: no sub-tab strip above it, so it is the page's first row.
    cauldron_settings(&mut a, "cauldron");
    a.handle(key(KeyCode::Char('i')));
    assert_eq!(a.composer.text(), "> [setting:cauldron.enabled]");
    assert_eq!(a.ctab, CTab::Settings, "the quote goes into the dump; he stays on Settings");
    a.composer = tui_render::compose::Composer::new();
    cauldron_settings(&mut a, "providers/claude_code");
    assert!(a.select_setting("pools.{pool}.budget.weekly.threshold_pct"));
    a.handle(key(KeyCode::Char('i')));
    assert_eq!(a.composer.text(), "> [setting:pools.claude_code.budget.weekly.threshold_pct]");
    // A table's row quotes the prefix its modal sets.
    a.composer = tui_render::compose::Composer::new();
    select_row(&mut a, |e| matches!(e, Entry::Row(SPage::Account(_, x)) if x == "acct2"));
    a.handle(key(KeyCode::Char('i')));
    assert_eq!(a.composer.text(), "> [setting:pools.claude_code.accounts.acct2]");
    a.composer = tui_render::compose::Composer::new();
    // An unclaimed repo quotes as before, from its band under UNCLAIMED
    // REPOS — on Status › Soup now (msg 186).
    a.set_ctab(CTab::Status);
    a.focus = Focus::Content;
    a.set_sttab(tui::cauldron::status::StatusTab::Soup);
    let at = |a: &App, k: &str| tui::cauldron::status::view(a, 140).items.iter().position(|i| i.key == k).unwrap();
    a.rows[0] = at(&a, "repo:orchard");
    a.handle(key(KeyCode::Char('i')));
    assert!(a.composer.text().ends_with("[repo:orchard]"), "{}", a.composer.text());
    // And an unclaimed region.
    a.composer = tui_render::compose::Composer::new();
    a.rows[0] = at(&a, "reg:R-9");
    a.handle(key(KeyCode::Char('i')));
    assert!(a.composer.text().ends_with("[region:R-9]"), "{}", a.composer.text());
}

// ── opportunistic accounts and the flip log ─────────────────────────────────

/// msg 235: an account is OPPORTUNISTIC when it is not on pace to spend its
/// week — "If you're in a procession from now to the end of the week in
/// order to completely consume the weekly allowance, then that account is
/// now flagged" — and whether it MAY be is a per-account switch inheriting
/// a global default (on). Every flip is logged with its cause.
#[test]
fn an_account_is_opportunistic_by_its_pressure_and_its_switch_and_every_flip_is_logged() {
    let root = scratch_root();
    let mut a = open(&root);
    let acct = |a: &App, id: &str| a.accounts("claude_code").into_iter().find(|x| x.id == id).unwrap();
    // sam-broomstick: 41 % of its week spent with 3d 4h of it left —
    // pressure 0.59 / 0.45 = 1.30 > 1: flagged. acct2: 0.41 < 1: not.
    assert_eq!(a.opportunistic("claude_code", &acct(&a, "sam-broomstick")), Some(true));
    assert_eq!(a.opportunistic("claude_code", &acct(&a, "acct2")), Some(false));
    assert_eq!(a.opportunistic("claude_code", &acct(&a, "acct3")), None, "no weekly reading: not known");
    // Its switch, on its row: `off` unflags it, logged with its cause.
    cauldron_settings(&mut a, "providers/claude_code");
    let row = a.selectable().iter().position(|e| matches!(e, Entry::Row(SPage::Account(_, x)) if x == "sam-broomstick")).unwrap();
    let opt = |a: &mut App, o: u8| {
        render_to_text(a, 140, 120).unwrap();
        let (r, _) = *a.hits.iter().find(|(_, h)| *h == Hit::Opt { row: row as u16, cell: 1, opt: o }).expect("opportunistic's option on the row");
        a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x + 1, row: r.y, modifiers: KeyModifiers::NONE }));
    };
    opt(&mut a, 2);
    assert_eq!(a.opportunistic("claude_code", &acct(&a, "sam-broomstick")), Some(false));
    let log = std::fs::read_to_string(tui::cauldron::store::global_dir(&a.env).join("flips.jsonl")).unwrap();
    let flip: serde_json::Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
    assert_eq!((flip["account"].as_str(), flip["on"].as_bool()), (Some("sam-broomstick"), Some(false)), "{log}");
    assert!(flip["why"].as_str().unwrap().contains("opportunistic on → off"), "{log}");
    // `inherit` gives it back the source's (on): flagged again.
    opt(&mut a, 0);
    let log = std::fs::read_to_string(tui::cauldron::store::global_dir(&a.env).join("flips.jsonl")).unwrap();
    assert!(log.lines().last().unwrap().contains("\"on\":true") && log.lines().last().unwrap().contains("inherits again"), "{log}");
    // The global default off reaches every account that inherits it.
    let global = tui::cauldron::menu::Bound::new("opportunistic.enabled", Default::default());
    a.sset(&global, tui::cauldron::settings::Val::Bool(false)).unwrap();
    assert_eq!(a.opportunistic("claude_code", &acct(&a, "sam-broomstick")), Some(false));
    // Status says it, on its card.
    a.sclear(&global).unwrap();
    a.set_ctab(CTab::Status);
    a.set_sttab(tui::cauldron::status::StatusTab::Tokens);
    let s = screen(&mut a);
    assert!(s.lines().any(|l| l.contains("opportunistic") && l.contains("flagged") && !l.contains("not flagged")), "{s}");
}

// ── 0071 S6/S7: the registry ────────────────────────────────────────────────

#[test]
fn config_repos_is_the_registry_band_with_register_unregister_and_trace_roots() {
    let root = scratch_root();
    let mut a = open(&root);
    cauldron_settings(&mut a, "devices");
    let s = screen(&mut a);
    let row = line_with(&s, "▸ Repos");
    assert!(row.contains("4 repos"), "{row}");
    // EVERY cauldron's (msg 235; task 0205): on Devices, after Mesh & services.
    assert!(s.find("▸ Mesh & services").unwrap() < s.find("▸ Repos").unwrap(), "{s}");
    a.open_settings_path("devices/repos").unwrap();
    let s = screen(&mut a);
    // Where each is: config. What it holds and who claims it: Status's.
    assert!(line_with(&s, "▸ harness").contains("/Users/demo/code/harness") && !line_with(&s, "▸ harness").contains("main @") && !line_with(&s, "▸ harness").contains("claimed"), "{s}");
    assert!(s.contains("+ register repo") && s.contains("▾ Trace roots") && s.contains("· the soup") && !s.contains(".mind/workspaces"), "{s}");
    // + register repo: a path field; the name is its last part.
    select_do(&mut a, "+ register repo");
    a.handle(key(KeyCode::Enter));
    typed(&mut a, "/tmp/somewhere/Side_Project");
    a.handle(key(KeyCode::Enter));
    assert!(a.modal.is_none(), "{:?}", a.modal);
    let r = a.repos.find_repo("side-project").expect("registered");
    assert!(a.repos.repos[r].local);
    let s = screen(&mut a);
    assert!(line_with(&s, "▸ side-project").contains("/tmp/somewhere/Side_Project"), "{s}");
    // Another face sees it.
    let mut b = open(&root);
    b.sync();
    assert!(b.repos.find_repo("side-project").is_some());
    // Its band, then unregister (asked first).
    select_row(&mut a, |e| matches!(e, Entry::Sub { page: SPage::Repo(n), .. } if n == "side-project"));
    a.handle(key(KeyCode::Right));
    assert!(a.snav.is_open(&SPage::Repo("side-project".into())));
    let s = screen(&mut a);
    assert!(!s.contains("claimed by") && !s.contains("no blueprint"), "{s}");
    select_do(&mut a, "unregister");
    a.handle(key(KeyCode::Enter));
    a.handle(key(KeyCode::Enter));
    assert!(a.repos.find_repo("side-project").is_none());
    assert!(!a.snav.is_open(&SPage::Repo("side-project".into())) && a.snav.is_open(&SPage::Repos), "its band is gone: the registry stays open");
    assert!(a.rows[SETTINGS_ROW] < a.selectable().len());
    // A claimed repo shows no `unregister` button — one would promise an
    // act it refuses (0217 B8) — only why, read-only.
    select_row(&mut a, |e| matches!(e, Entry::Sub { page: SPage::Repo(n), .. } if n == "harness"));
    a.handle(key(KeyCode::Right));
    assert!(!a.selectable().iter().any(|e| matches!(e, Entry::Do { label, .. } if label == "unregister")), "{:?}", a.selectable());
    assert!(line_with(&screen(&mut a), "unregister").contains("not while a cluster claims it"));
    assert!(a.repos.find_repo("harness").is_some());
    // ← climbs to harness's header; ← again closes it.
    a.handle(key(KeyCode::Left));
    assert!(matches!(a.settings_entry(), Some(Entry::Sub { page: SPage::Repo(n), .. }) if n == "harness"));
    a.handle(key(KeyCode::Left));
    assert!(!a.snav.is_open(&SPage::Repo("harness".into())));
    // A trace root is added like any list item.
    select_row(&mut a, |e| matches!(e, Entry::Add(_)));
    a.handle(key(KeyCode::Enter));
    typed(&mut a, "~/code/broomstick/.mind");
    a.handle(key(KeyCode::Enter));
    assert!(screen(&mut a).contains("· ~/code/broomstick/.mind"));
    // Taking it off asks first, on its row (0220 E4): one Backspace only
    // asks, any other key forgets the ask, and a second x takes it off.
    let roots = |a: &App| a.settings.global.vals.get("repos.trace_roots").map(|v| v.as_list().to_vec()).unwrap_or_default();
    let mine = |e: &Entry| matches!(e, Entry::Item { text, .. } if text == "~/code/broomstick/.mind");
    select_row(&mut a, mine);
    a.handle(key(KeyCode::Backspace));
    let s = screen(&mut a);
    assert!(line_with(&s, "· ~/code/broomstick/.mind").contains("take it off the list? x again"), "{s}");
    assert!(roots(&a).iter().any(|r| r == "~/code/broomstick/.mind"), "one Backspace never removes");
    a.handle(key(KeyCode::Down));
    select_row(&mut a, mine);
    a.handle(key(KeyCode::Char('x')));
    assert!(roots(&a).iter().any(|r| r == "~/code/broomstick/.mind"), "the ask was forgotten: this x asks again");
    a.handle(key(KeyCode::Char('x')));
    assert!(!roots(&a).iter().any(|r| r == "~/code/broomstick/.mind"), "x again takes it off");
    assert!(!screen(&mut a).contains("· ~/code/broomstick/.mind"));
}

#[test]
fn right_on_an_unclaimed_repo_expands_its_band_and_an_unclaimed_region_expands_to_its_nodes() {
    let root = scratch_root();
    let mut a = open(&root);
    // On Status › Soup now (msg 186), each an expandable band.
    a.set_ctab(CTab::Status);
    a.focus = Focus::Content;
    a.set_sttab(tui::cauldron::status::StatusTab::Soup);
    let at = |a: &App, k: &str| tui::cauldron::status::view(a, 140).items.iter().position(|i| i.key == k).unwrap_or_else(|| panic!("no row `{k}`"));
    a.rows[0] = at(&a, "repo:orchard");
    a.handle(key(KeyCode::Right));
    let s = screen(&mut a);
    assert!(line_with(&s, "▾ orchard").contains("14 modules"), "{s}");
    assert!(s.lines().any(|l| l.contains("branch")) && s.lines().any(|l| l.contains("module")), "{s}");
    a.handle(key(KeyCode::Char('i')));
    assert!(a.composer.text().ends_with("[repo:orchard]"), "{}", a.composer.text());
    a.handle(key(KeyCode::Left));
    assert!(screen(&mut a).contains("▸ orchard"));
    // The unclaimed region: → expands it (boundary, nodes); → a node's band
    // holds its body.
    a.rows[0] = at(&a, "reg:R-9");
    a.handle(key(KeyCode::Right));
    let s = screen(&mut a);
    assert!(s.contains("machines joining and leaving"), "{s}");
    let node = a.corpus.members[a.corpus.find_region("R-9").unwrap()].first().map(|&n| a.corpus.nodes[n].id.clone()).expect("R-9 has a node");
    a.rows[0] = at(&a, &format!("node:{node}"));
    a.handle(key(KeyCode::Char('i')));
    assert!(a.composer.text().ends_with(&format!("[{node}]")));
    a.focus = Focus::Content;
    a.handle(key(KeyCode::Right));
    let body = a.corpus.nodes[a.corpus.find_node(&node).unwrap()].body.clone();
    let s = render_to_text(&mut a, 400, 60).unwrap().join("\n");
    assert!(s.contains(&body[..body.len().min(40)]), "{s}");
    // ← closes the node, ← climbs to the region, ← closes it: all shut.
    a.handle(key(KeyCode::Left));
    a.handle(key(KeyCode::Left));
    a.handle(key(KeyCode::Left));
    let s = render_to_text(&mut a, 400, 60).unwrap().join("\n");
    assert!(!s.contains(&body[..body.len().min(40)]) && s.contains("▸ R-9"), "{s}");
    // Config holds none of it any more.
    a.set_ctab(CTab::Settings);
    assert!(!a.selectable().iter().any(|e| matches!(e, Entry::Sub { page: SPage::Region(_) | SPage::Repo(_), .. })));
}

#[test]
fn overview_has_no_repos_line_and_the_artifacts_tab_lists_and_copies_the_repos() {
    let mut a = open(&scratch_root());
    let g = id(&a, "X-10");
    a.select(g);
    a.set_ktab(KTab::Details);
    a.focus = Focus::Content;
    let s = screen(&mut a);
    assert!(!s.contains("harness main @ 8299dcc1") && !a.items().iter().any(|x| matches!(x, Item::Footer(_) | Item::Repo(_))), "no repos line on Overview: {s}");
    // The Artifacts tab lists the repos it claims; `c` copies one's path.
    a.set_ktab(KTab::Nodes);
    let repos: Vec<String> = a.items().iter().filter_map(|x| match x {
        Item::Repo(r) => Some(a.repos.repos[*r].name.clone()),
        _ => None,
    }).collect();
    assert_eq!(repos, vec!["harness".to_string(), "cauldron".to_string()]);
    a.item = a.items().iter().position(|x| matches!(x, Item::Repo(_))).unwrap();
    a.handle(key(KeyCode::Char('c')));
    let path = a.repos.repos[a.repos.find_repo("harness").unwrap()].path.clone();
    assert_eq!(a.outbox.last(), Some(&Outbox::Copy(path)));
    // A cluster with no repo lists none (every seeded cluster claims one,
    // audit 0177 T4, so this one's claims are taken away).
    let g = id(&a, "X-3");
    a.repos.claims[g].clear();
    a.select(g);
    a.set_ktab(KTab::Nodes);
    assert!(!a.items().iter().any(|x| matches!(x, Item::Repo(_))));
}

// ── counts include focus nags ───────────────────────────────────────────────

#[test]
fn the_below_footer_is_a_bare_count_that_includes_focus_nags() {
    let mut a = open(&scratch_root());
    // Find a child cluster that has focus nags.
    let child = (0..a.forest.clusters.len())
        .find(|&g| a.forest.get(g).parent.is_some() && !tui::cauldron::pages::focus_nags(&a, g).is_empty())
        .expect("the seed has a sub-cluster with a focus nag");
    let parent = a.forest.get(child).parent.unwrap();
    let nags = tui::cauldron::pages::focus_nags(&a, child).len();
    assert!(nags > 0);
    let total: usize = a.forest.subtree(parent)[1..].iter().map(|&g| a.forest.get(g).questions.len() + tui::cauldron::pages::focus_nags(&a, g).len()).sum();
    a.select(parent);
    a.set_ktab(KTab::Details);
    let s = screen(&mut a);
    let name = a.forest.get(child).name.clone();
    assert!(!s.lines().any(|l| l.contains(&name) && l.contains(" active · ")), "no child row on Overview (F10): {s}");
    a.set_ktab(KTab::Questions);
    let s = screen(&mut a);
    let footer = line_with(&s, " below").split('│').nth(2).unwrap().trim();
    assert_eq!(footer, format!("{total} below"), "{s}");
}

// ── the Bus (msg 235) ───────────────────────────────────────────────────────

fn bus_rows(a: &App) -> Vec<tui::cauldron::status::SItem> {
    tui::cauldron::status::view(a, 140).items
}

fn bus_at(a: &mut App, key: &str) {
    a.rows[0] = bus_rows(a).iter().position(|i| i.key == key).unwrap_or_else(|| panic!("no row {key}: {:?}", bus_rows(a).iter().map(|i| i.key.clone()).collect::<Vec<_>>()));
}

/// Events grouped by type, a timestamped row each, paged by `show more`; a
/// row opens the bus modal on its payload, time and the triggers it fired;
/// a trigger there opens its handler ripple and the execution it started,
/// and that execution its steps. ← walks back; Esc closes.
#[test]
fn the_bus_groups_events_by_type_pages_them_and_a_row_walks_event_trigger_execution() {
    use tui::cauldron::bus::BusPage;
    use tui::cauldron::status::SAct;
    let mut a = open(&scratch_root());
    a.set_ctab(CTab::Status);
    a.set_sttab(tui::cauldron::status::StatusTab::Bus);
    a.focus = Focus::Content;
    // The dump.received type: its rows, a page at a time.
    a.stnav.insert("ev:dump.received".into());
    let rows = |a: &App| bus_rows(a).iter().filter(|i| i.parent.as_deref() == Some("ev:dump.received") && i.key.starts_with("event:")).count();
    let total = a.rstore.events().iter().filter(|e| e.class == "dump.received").count();
    assert!(total > tui::cauldron::status::BUS_PAGE, "the seed has more than a page: {total}");
    assert_eq!(rows(&a), tui::cauldron::status::BUS_PAGE);
    let s = screen(&mut a);
    assert!(s.contains("show more"), "{s}");
    bus_at(&mut a, "more:dump.received");
    a.handle(key(KeyCode::Enter));
    assert!(rows(&a) > tui::cauldron::status::BUS_PAGE, "show more shows more");
    // Each row carries its time.
    let first = bus_rows(&a).into_iter().find(|i| i.parent.as_deref() == Some("ev:dump.received") && i.key.starts_with("event:")).unwrap();
    let SAct::Bus(BusPage::Event(ev)) = first.act.clone() else { panic!("{:?}", first.act) };
    // The row opens the modal: payload, time, triggers.
    a.rows[0] = bus_rows(&a).iter().position(|i| i.key == first.key).unwrap();
    a.handle(key(KeyCode::Enter));
    let laid = a.bus_laid(tui::cauldron::bus::BUS_WIDTH).expect("the bus modal");
    let text: String = laid.lines.iter().map(|l| l.to_string() + "\n").collect();
    assert!(text.contains(&ev) || laid.title.contains(&ev), "{} {text}", laid.title);
    let trig = laid.links.iter().position(|(_, p)| matches!(p, BusPage::Trigger { .. })).unwrap_or_else(|| panic!("a dump.received fired a trigger: {text}"));
    // Follow the trigger: its handler ripple and the execution it started.
    for _ in 0..trig {
        a.handle(key(KeyCode::Down));
    }
    a.handle(key(KeyCode::Enter));
    let laid = a.bus_laid(tui::cauldron::bus::BUS_WIDTH).unwrap();
    assert!(laid.links.iter().any(|(_, p)| matches!(p, BusPage::Ripple(_))), "the handler ripple: {:?}", laid.links);
    let run = laid.links.iter().position(|(_, p)| matches!(p, BusPage::Run(_))).expect("the execution it started");
    for _ in 0..run {
        a.handle(key(KeyCode::Down));
    }
    a.handle(key(KeyCode::Enter));
    let laid = a.bus_laid(tui::cauldron::bus::BUS_WIDTH).unwrap();
    assert!(laid.links.iter().any(|(_, p)| matches!(p, BusPage::Step(..))), "its steps: {}", laid.title);
    // ← back to the trigger, the event; Esc closes.
    a.handle(key(KeyCode::Left));
    assert!(matches!(&a.modal, Some(Modal::Bus { stack }) if stack.len() == 2));
    a.handle(key(KeyCode::Esc));
    assert!(a.modal.is_none());
    // Triggers and ripples are read-only: a row opens, nothing edits.
    a.stnav.insert("triggers".into());
    a.stnav.insert("ripples".into());
    assert!(bus_rows(&a).iter().filter(|i| i.parent.as_deref() == Some("triggers") || i.parent.as_deref() == Some("ripples")).all(|i| matches!(i.act, SAct::Bus(_))));
    // The executions: RECORDED pages too.
    a.stnav.insert("recorded".into());
    let runs = |a: &App| bus_rows(a).iter().filter(|i| i.key.starts_with("run:")).count();
    let n = runs(&a);
    if a.rstore.runs().len() > n {
        bus_at(&mut a, "more:runs");
        a.handle(key(KeyCode::Enter));
        assert!(runs(&a) > n);
    }
}

// ── the Engine draws cross-cluster waits ────────────────────────────────────

#[test]
fn the_aggregate_engine_band_draws_a_wait_that_crosses_clusters_and_each_end_shows_it() {
    let mut a = open(&scratch_root());
    let g = id(&a, "X-1");
    a.select(g);
    a.set_ktab(KTab::Tasks);
    let s = screen(&mut a);
    assert!(line_with(&s, "⇠ waits on T-13").contains("Intake shell ripple · ready · Ripples"), "N9: no slug, the owner named: {s}");
    assert!(!s.contains("ACROSS CLUSTERS"), "own scope: no section");
    a.aggregate = true;
    let s = screen(&mut a);
    assert!(s.contains("▾ Across clusters"), "{s}");
    assert!(line_with(&s, "○ Cauldron/T-4").contains("⇠ Ripples/T-13 Intake shell ripple"), "{s}");
    assert!(line_with(&s, "⇢ T-4").contains("waits on it · ready · Cauldron"), "{s}");
    // The section's row opens the waiting task's trace page.
    let i = a.items().iter().rposition(|x| *x == Item::Task { cluster: g, serial: 4 }).unwrap();
    a.item = i;
    a.focus = Focus::Content;
    a.handle(key(KeyCode::Right));
    assert_eq!(a.trace_top(), Some(Item::Task { cluster: g, serial: 4 }));
    // Bad waits are refused, loudly; a wait on a task in no cluster is
    // skipped and counted (REL-3.S2d).
    let json = r#"{"clusters":[{"id":"X-1","name":"X","description":"","tasks":[{"serial":1,"title":"a","state":"ready","waits_on":["T-9"]}]},{"id":"X-2","name":"Y","description":"","tasks":[{"serial":2,"title":"b","state":"ready"}]}]}"#;
    let soup = tui::cauldron::model::Soup::from_file(serde_json::from_str(json).unwrap()).expect("REL-3.S2d: skipped, not refused");
    assert!(soup.forest.stale.len() == 1 && soup.forest.stale[0].contains("which is in no cluster"), "{:?}", soup.forest.stale);
    assert!(soup.forest.clusters[0].tasks[0].waits_on.is_empty(), "skipped in the view");
    for (waits, want) in [
        (r#"["T-1"]"#, "waits on `T-1` in its own cluster"),
        (r#"["DIR-9"]"#, "not a task or a milestone"),
    ] {
        let json = format!(
            r#"{{"clusters":[{{"id":"X-1","name":"X","description":"","tasks":[{{"serial":1,"title":"a","state":"ready","waits_on":{waits}}}]}},{{"id":"X-2","name":"Y","description":"","tasks":[{{"serial":2,"title":"b","state":"ready"}}]}}]}}"#
        );
        let file: tui::cauldron::model::DataFile = serde_json::from_str(&json).unwrap();
        let e = format!("{:#}", tui::cauldron::model::Soup::from_file(file).unwrap_err());
        assert!(e.contains(want), "{e}");
    }
}

// ── a multi-line settings row is marked on every line ───────────────────────

#[test]
fn the_selection_bar_marks_every_line_of_a_settings_row_with_a_refusal_under_it() {
    let mut a = open(&scratch_root());
    cauldron_settings(&mut a, "machine");
    assert!(a.select_setting("machine.ram_max_gb"));
    a.handle(key(KeyCode::Enter));
    a.handle(Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)));
    typed(&mut a, "lots");
    a.handle(key(KeyCode::Esc));
    let (rows, sel) = tui::cauldron::render_with_selection(&mut a, 120, 70).unwrap();
    let row = rows.iter().position(|l| l.contains("RAM max")).unwrap();
    // The selection (a background, msg 186) covers the row and its refusal.
    assert!(sel.contains(&row) && rows[row + 1].contains("✗") && sel.contains(&(row + 1)), "{rows:?} {sel:?}");
}

// ── services' proven-good versions ──────────────────────────────────────────

#[test]
fn a_services_proven_good_versions_are_on_status_and_config_holds_no_reading_of_them() {
    // msg 235: a reading belongs in Status, never in Config.
    let mut a = open(&scratch_root());
    a.set_ctab(CTab::Status);
    a.set_sttab(tui::cauldron::status::StatusTab::Mesh);
    a.stnav.insert("svc:example-mac/threads".into());
    a.stnav.insert("svc:example-mac/echo".into());
    let s = screen(&mut a);
    assert!(s.lines().any(|l| l.contains("proven good") && l.contains("8299dcc1 (2026-10-18) · 5c1e07aa (2026-10-11)")), "{s}");
    assert!(s.lines().any(|l| l.contains("proven good") && l.contains("none recorded")), "{s}");
    // Config holds no service's version or address at all (0220 E8): they
    // are readings, the Mesh's, and nothing read an override of them.
    cauldron_settings(&mut a, "devices/mesh");
    let s = screen(&mut a);
    assert!(s.contains("▸ Supervisor") && !s.contains("▸ Services") && !s.contains("proven good"), "{s}");
    assert!(a.open_settings_path("devices/mesh/services").is_err());
}
