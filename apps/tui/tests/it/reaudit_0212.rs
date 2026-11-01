//! Task 0212 (8e4fca10 msg 235, the 0211 re-audit). R1 first: a control on a
//! global key his cauldron's file still held does what it shows, on his real
//! file shapes, and his files take his EDITS and never the seed's data.
//! Hermetic: scratch roots holding his file shapes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use tui::cauldron::app::App;
use tui::cauldron::menu::Bound;
use tui::cauldron::render_to_text;
use tui::cauldron::settings::{Ctx, Val};
use tui::cauldron::store::seed_dir;

const HIS_GLOBAL: &str = "[pools.claude_code]\nnew_accounts_enabled = false\n\n[pools.claude_code.accounts.acct2]\nshare = 30\n\n[pools.claude_code.accounts.acct3]\nenabled = false\nnote = \"out of the pool, 3d ago · card expired\"\nshare = 20\n\n[pools.claude_code.accounts.sam-broomstick]\nenabled = true\nmax_concurrent = -1\nshare = 50\n\n[pools.claude_code.accounts.sam-broomstick.budget.session]\nthreshold_pct = \"off\"\n\n[pools.claude_code.budget.session]\nthreshold_pct = \"off\"\n\n[pools.claude_code.opportunistic]\nenabled = false\n\n[pools.openrouter]\nmodels = [\"jev\"]\nprovider = \"openrouter\"\n";
const HIS_PROD: &str = "[cauldron]\nenabled = true\n\n[identity]\nlive_login = \"claude_code/sam-broomstick\"\n\n[supervisor]\nauto_restart = true\n";

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "t0212-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn open(root: &Path) -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let env = Environment { root: root.to_path_buf(), root_source: Source::CauldronHome, name: "prod".into(), name_source: Source::Default, data_root: root.join("data").join("prod") };
    crate::live_pool::seeded(App::open(env, seed_dir(), true).expect("opens"))
}

fn root_with(global: &str, prod: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = scratch_root();
    let g = root.join("data/shared/tui/settings.toml");
    let c = root.join("data/prod/tui/soup/settings.toml");
    for (p, t) in [(&g, global), (&c, prod)] {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, t).unwrap();
    }
    (root, g, c)
}

/// His file shapes: the embedded copies of his files. Never his live files
/// themselves (0225 F4): a test that reads his `$HOME` passes or fails on
/// what he did today, not on the code.
fn shapes() -> Vec<(String, String)> {
    vec![(HIS_GLOBAL.to_string(), HIS_PROD.to_string())]
}

fn auto() -> Bound {
    Bound::new("supervisor.auto_restart", Ctx::default())
}

/// No seed value ever reaches his files: no endpoint, no role binding, no use.
fn no_seed_data(g: &Path, c: &Path) {
    let (gt, ct) = (std::fs::read_to_string(g).unwrap(), std::fs::read_to_string(c).unwrap_or_default());
    assert!(!gt.contains("endpoints") && !gt.contains("FAKE") && !gt.contains("roles"), "seed data in the global file: {gt}");
    assert!(!ct.contains("endpoints") && !ct.contains("roles") && !ct.contains("[cauldron.use"), "seed data in prod's file: {ct}");
}

/// R1: ↺ on auto restart sticks across a reopen, and so do `off` and `on`,
/// whichever change comes first. His other values are kept verbatim.
#[test]
fn r1_auto_restart_reset_off_and_on_stick_on_his_file_shapes() {
    for (global, prod) in shapes() {
        for first_elsewhere in [false, true] {
            let (root, g, c) = root_with(&global, &prod);
            let mut a = open(&root);
            assert_eq!(a.sget(&auto()).val, Val::Bool(true), "his value read");
            if first_elsewhere {
                // A change to a non-global setting moves the held keys first.
                a.sset(&Bound::new("cauldron.opportunistic", Ctx::default()), Val::Bool(false)).unwrap();
            }
            // ↺
            a.sclear(&auto()).unwrap();
            assert!(!a.sis_set(&auto()));
            let mut b = open(&root);
            assert_eq!(b.sget(&auto()).val, Val::Bool(false), "↺ stuck across a reopen");
            assert!(!b.sis_set(&auto()), "no ↺ shown after a reopen");
            let (gt, ct) = (std::fs::read_to_string(&g).unwrap(), std::fs::read_to_string(&c).unwrap());
            assert!(!gt.contains("auto_restart") && !ct.contains("auto_restart"), "global:\n{gt}\nprod:\n{ct}");
            assert!(ct.contains("[cauldron]\nenabled = true"), "his own prod value kept: {ct}");
            assert!(gt.contains("live_login = \"claude_code/sam-broomstick\""), "the other held key moved, not lost: {gt}");
            assert!(gt.contains("[pools.openrouter]") && gt.contains("models = [\"jev\"]") && gt.contains("note = \"out of the pool, 3d ago · card expired\""), "{gt}");
            // on, then off
            b.sset(&auto(), Val::Bool(true)).unwrap();
            assert_eq!(open(&root).sget(&auto()).val, Val::Bool(true), "`on` stuck");
            b.sset(&auto(), Val::Bool(false)).unwrap();
            let c2 = open(&root);
            assert_eq!(c2.sget(&auto()).val, Val::Bool(false), "`off` stuck");
            assert!(c2.sis_set(&auto()) && !c2.reverts(&auto()), "`off` is the default: kept as his choice (0225 F1), no ↺");
            no_seed_data(&g, &c);
        }
    }
}

/// R1, the identity half: clearing live login sticks too.
#[test]
fn r1_identity_live_login_cleared_sticks() {
    let (root, g, c) = root_with(HIS_GLOBAL, HIS_PROD);
    let mut a = open(&root);
    let ll = Bound::new("identity.live_login", Ctx::default());
    assert!(a.sis_set(&ll));
    a.sclear(&ll).unwrap();
    let b = open(&root);
    assert!(!b.sis_set(&ll), "cleared live login came back");
    assert_eq!(b.sget(&auto()).val, Val::Bool(true), "auto restart moved, not lost");
    no_seed_data(&g, &c);
}

/// R2: no explanatory caption on the frame of an open of his files.
#[test]
fn r2_opening_his_files_says_nothing_on_the_border() {
    for (global, prod) in shapes() {
        let (root, _, _) = root_with(&global, &prod);
        let mut a = open(&root);
        assert!(a.notice.is_none(), "{:?}", a.notice);
        let s = render_to_text(&mut a, 120, 40).unwrap().join("\n");
        assert!(!s.contains("no longer kept") && !s.contains("kept globally"), "{s}");
    }
}

fn mouse(a: &mut App, kind: ratatui::crossterm::event::MouseEventKind, x: u16, y: u16) {
    use ratatui::crossterm::event::{Event, KeyModifiers, MouseEvent};
    a.handle(Event::Mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }));
}

/// R11: a press that missed everything and is released over a card does
/// nothing to the card: a release is never a click.
#[test]
fn r11_a_release_over_a_card_is_not_a_click() {
    use ratatui::crossterm::event::{MouseButton, MouseEventKind as K};
    let mut a = open(&scratch_root());
    a.set_ktab(tui::cauldron::app::KTab::Details);
    let s = render_to_text(&mut a, 120, 50).unwrap();
    let top = s.iter().rposition(|l| l.trim_end().ends_with("bucket ╮")).unwrap_or_else(|| panic!("{}", s.join("\n")));
    let (cy, cl) = s.iter().enumerate().find(|(_, l)| l.contains("OBJ-1")).unwrap_or_else(|| panic!("{}", s.join("\n")));
    let cx = cl[..cl.find("OBJ-1").unwrap()].chars().count() as u16;
    let before = a.composer.text();
    // The bucket's top border, well away from its grabber.
    let bx = 30u16;
    mouse(&mut a, K::Down(MouseButton::Left), bx, top as u16);
    mouse(&mut a, K::Drag(MouseButton::Left), cx, cy as u16);
    mouse(&mut a, K::Up(MouseButton::Left), cx, cy as u16);
    render_to_text(&mut a, 120, 50).unwrap();
    assert_eq!(a.composer.text(), before, "a release inserted a reference");
    // A press ON the card dragged off it does nothing either; a press and
    // release on it inserts.
    mouse(&mut a, K::Down(MouseButton::Left), cx, cy as u16);
    mouse(&mut a, K::Drag(MouseButton::Left), bx, top as u16);
    mouse(&mut a, K::Up(MouseButton::Left), bx, top as u16);
    assert_eq!(a.composer.text(), before, "a press dragged off the card acted");
    mouse(&mut a, K::Down(MouseButton::Left), cx, cy as u16);
    mouse(&mut a, K::Up(MouseButton::Left), cx, cy as u16);
    assert!(a.composer.text().contains("[OBJ-1]"), "a click on the card: {}", a.composer.text());
}

fn config(a: &mut App, tab: tui::cauldron::menu::CfgTab, path: Option<&str>) {
    a.set_ctab(tui::cauldron::app::CTab::Settings);
    a.set_cfg_tab(tab);
    a.snav = tui::cauldron::menu::Nav::cauldron();
    a.rows[tui::cauldron::menu::SETTINGS_ROW] = 0;
    if let Some(p) = path {
        a.open_settings_path(p).unwrap_or_else(|e| panic!("{p}: {e}"));
    }
}

/// Every box on screen is a rectangle: each `╭` has its `╮` in the column of
/// the `│` and `╯` of its own body. Returns the corners' columns.
fn corners(s: &[String], from: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (y, l) in s.iter().enumerate().skip(from) {
        let ch: Vec<char> = l.chars().collect();
        for (x, c) in ch.iter().enumerate() {
            if *c == '╭' {
                let right = (x + 1..ch.len()).find(|&i| ch[i] == '╮').unwrap_or_else(|| panic!("an open top at row {y}: {l}"));
                let bottom = (y + 1..s.len()).find(|&j| s[j].chars().nth(x) == Some('╰'));
                if let Some(b) = bottom {
                    assert_eq!(s[b].chars().nth(right), Some('╯'), "box at ({x},{y}) is not rectangular:\n{l}\n{}", s[b]);
                    for row in &s[y + 1..b] {
                        assert_eq!(row.chars().nth(right), Some('│'), "box at ({x},{y}): its body is not under its top:\n{l}\n{row}");
                    }
                }
                out.push((x, y));
            }
        }
    }
    out
}

/// R4: a question card on Tasks › Active starts two columns in from the
/// band's title, as an Overview card does.
#[test]
fn r4_question_cards_sit_two_columns_in_from_the_band_title() {
    let mut a = open(&scratch_root());
    a.set_ktab(tui::cauldron::app::KTab::Questions);
    let s = render_to_text(&mut a, 140, 60).unwrap();
    let (y, l) = s.iter().enumerate().find(|(_, l)| l.contains("▾ Active")).unwrap_or_else(|| panic!("{}", s.join("\n")));
    let title = l[..l.find("Active").unwrap()].chars().count();
    let card = &s[y + 1];
    let corner = card.chars().position(|c| c == '╭').unwrap_or_else(|| panic!("{card}"));
    assert_eq!(corner, title + 2, "\n{l}\n{card}");
    corners(&s, y);
}

/// R4 + R5: Status › Soup's node cards, every region and node open: two in
/// from the region's title, and rectangular. Tokens' cards too.
#[test]
fn r4_r5_soup_and_tokens_cards_are_rectangles_two_in_from_their_band() {
    use tui::cauldron::status::StatusTab;
    let mut a = open(&scratch_root());
    a.set_ctab(tui::cauldron::app::CTab::Status);
    a.set_sttab(StatusTab::Soup);
    open_all(&mut a);
    let s = render_to_text(&mut a, 140, 400).unwrap();
    let (y, l) = s.iter().enumerate().find(|(_, l)| l.contains("▾ R-")).unwrap_or_else(|| panic!("no open region:\n{}", s.join("\n")));
    let title = l[..l.find("R-").unwrap()].chars().count();
    let card = s[y + 1..].iter().find(|l| l.contains('╭')).unwrap();
    assert_eq!(card.chars().position(|c| c == '╭').unwrap(), title + 2, "\n{l}\n{card}");
    assert!(!corners(&s, 0).is_empty());
    a.set_sttab(StatusTab::Tokens);
    let s = render_to_text(&mut a, 120, 200).unwrap();
    let (y, l) = s.iter().enumerate().find(|(_, l)| l.contains("▾ Claude Code")).unwrap_or_else(|| panic!("{}", s.join("\n")));
    let title = l[..l.find("Claude").unwrap()].chars().count();
    assert_eq!(s[y + 1].chars().position(|c| c == '╭').unwrap(), title + 2, "\n{l}\n{}", s[y + 1]);
    corners(&s, 0);
}

/// R6: a Bus event row opens its page at ONE click.
#[test]
fn r6_a_bus_row_is_a_one_click_button() {
    use tui::cauldron::status::{self, SAct, StatusTab};
    let mut a = open(&scratch_root());
    a.set_ctab(tui::cauldron::app::CTab::Status);
    a.set_sttab(StatusTab::Bus);
    open_all(&mut a);
    let v = status::view(&a, 120);
    let i = v.items.iter().position(|i| matches!(i.act, SAct::Bus(_))).expect("a bus row");
    a.rows[0] = i;
    assert!(a.status_is_button(), "a bus row needs a second click");
}

/// R7: narrowed table columns keep their gap at 80 columns.
#[test]
fn r7_narrowed_columns_never_abut() {
    let mut a = open(&scratch_root());
    config(&mut a, tui::cauldron::menu::CfgTab::Providers, Some("providers/endpoints"));
    let s = render_to_text(&mut a, 80, 120).unwrap();
    let rows: Vec<&String> = s.iter().filter(|l| l.contains('…') && l.contains('›')).collect();
    assert!(!rows.is_empty(), "nothing narrowed at 80:\n{}", s.join("\n"));
    for r in rows {
        for (i, _) in r.match_indices('…') {
            assert!(r[i + '…'.len_utf8()..].starts_with("  "), "a narrowed value abuts the next: {r}");
        }
    }
}

/// R8: Garbage collection is store-wide, under none of the stores' heads.
#[test]
fn r8_garbage_collection_is_a_sibling_of_the_stores() {
    use tui::cauldron::menu::{Entry, SPage};
    let a = open(&scratch_root());
    let e = a.page_entries(&SPage::Storage);
    let gc = e.iter().position(|x| matches!(x, Entry::Sub { page: SPage::Gc, .. })).expect("gc");
    let head = e.iter().position(|x| matches!(x, Entry::Head(..))).expect("a head");
    assert!(gc < head, "gc sits under a head: {e:?}");
}

/// R9: a role's source by its name; live login says its default.
#[test]
fn r9_roles_say_the_source_by_name_and_live_login_its_default() {
    let mut a = open(&scratch_root());
    config(&mut a, tui::cauldron::menu::CfgTab::Cauldron, Some("cauldron/roles"));
    let s = render_to_text(&mut a, 130, 200).unwrap().join("\n");
    assert!(s.contains("Claude Code") && !s.contains("claude_code"), "{s}");
    let mut a = open(&scratch_root());
    config(&mut a, tui::cauldron::menu::CfgTab::Permissions, Some("permissions/identity"));
    let s = render_to_text(&mut a, 130, 60).unwrap();
    let row = s.iter().find(|l| l.contains("live login")).unwrap_or_else(|| panic!("{}", s.join("\n")));
    assert!(row.contains("default none"), "{row}");
}

/// R10: a confirm shows what it asks and cancel, each pressed at one click.
#[test]
fn r10_a_confirm_has_its_buttons() {
    use ratatui::crossterm::event::{MouseButton, MouseEventKind as K};
    use tui::cauldron::app::Hit;
    let (root, _, _) = root_with(HIS_GLOBAL, HIS_PROD);
    let mut a = open(&root);
    let on = Bound::new("cauldron.enabled", Ctx::default());
    for (button, after) in [(1u8, true), (0u8, false)] {
        a.start_act(tui::cauldron::manage::Act::Enable(false));
        let s = render_to_text(&mut a, 100, 30).unwrap().join("\n");
        assert!(s.contains(" disable ") && s.contains(" cancel "), "{s}");
        let r = a.hits.iter().find(|(_, h)| *h == Hit::ConfirmButton(button)).map(|(r, _)| *r).expect("the button");
        mouse(&mut a, K::Down(MouseButton::Left), r.x, r.y);
        mouse(&mut a, K::Up(MouseButton::Left), r.x, r.y);
        assert!(a.modal.is_none(), "button {button} left the modal open");
        assert_eq!(a.sget(&on).val, Val::Bool(after), "button {button}");
    }
}

/// R12: a value is never cut while its row has room for it.
#[test]
fn r12_no_cut_with_room_to_spare() {
    let mut a = open(&scratch_root());
    config(&mut a, tui::cauldron::menu::CfgTab::Permissions, None);
    let s = render_to_text(&mut a, 200, 40).unwrap();
    let row = s.iter().find(|l| l.contains("plugin roots")).unwrap();
    assert!(!row.contains('…'), "{row}");
    let mut a = open(&scratch_root());
    config(&mut a, tui::cauldron::menu::CfgTab::Cauldron, Some("cauldron/directories"));
    let s = render_to_text(&mut a, 130, 80).unwrap();
    for l in s.iter().filter(|l| l.contains("~/") && l.contains('…')) {
        let tail = l.trim_end_matches(['│', ' ', 'ⓘ']);
        assert!(display_width(l) - display_width(tail) < 6, "cut with room left: {l}");
    }
}

fn display_width(s: &str) -> usize {
    s.chars().count()
}

/// Every band on the Status tab shown, opened (the set holds the toggled).
fn open_all(a: &mut App) {
    use tui::cauldron::status::{self, SAct};
    for _ in 0..5 {
        let keys: Vec<String> = status::view(a, 140).items.iter().filter(|i| matches!(i.act, SAct::Band) && !status::is_open(a, &i.key)).map(|i| i.key.clone()).collect();
        for k in keys {
            if !a.stnav.remove(&k) {
                a.stnav.insert(k);
            }
        }
    }
}

// ── the adversarial pass: every control on every Config page ───────────────

/// Every setting a Config page shows, every tab, every band and every row's
/// modal walked: (the page it is on, the setting).
fn every_control(a: &App) -> Vec<(String, Bound)> {
    use tui::cauldron::menu::{CfgTab, Cell, Entry, SPage};
    let mut out: Vec<(String, Bound)> = Vec::new();
    let mut seen_pages: Vec<SPage> = Vec::new();
    let mut stack: Vec<SPage> = CfgTab::ALL.iter().map(|t| SPage::Tab(*t)).collect();
    while let Some(page) = stack.pop() {
        if seen_pages.contains(&page) {
            continue;
        }
        seen_pages.push(page.clone());
        for e in a.page_entries(&page) {
            match e {
                Entry::Sub { page: p, .. } => stack.push(p),
                Entry::Row(p) => {
                    for c in a.row_cells(&p) {
                        if let Cell::Toggle(bd) = c {
                            out.push((format!("{page:?} row"), bd));
                        }
                    }
                    stack.push(p);
                }
                Entry::Set(bd) => out.push((format!("{page:?}"), bd)),
                _ => {}
            }
        }
    }
    let mut keys = std::collections::HashSet::new();
    out.retain(|(_, bd)| keys.insert(bd.concrete()));
    out
}

/// Change `bd` the way its control does; Err with why it was not changed.
fn change(a: &mut App, bd: &Bound) -> Result<(), String> {
    use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use tui::cauldron::menu::Entry;
    use tui::cauldron::settings::Ty;
    let d = bd.def();
    let cur = a.sget(bd).val;
    let toggle = matches!(d.ty, Ty::Toggle | Ty::LiveLogin) || a.strip_words(bd).is_some();
    if toggle {
        let t = a.toggle_of(bd);
        // `inherit` (a tri-state's first option) is the ↺'s job: pick a value.
        let opt = (0..t.options.len()).find(|&o| o != t.selected && !(tui::cauldron::menu::is_tri(bd) && o == 0)).ok_or("one option")?;
        a.set_option(bd, opt);
        if matches!(a.modal, Some(tui::cauldron::app::Modal::Confirm { .. })) {
            a.handle(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        }
        return Ok(());
    }
    let v = match d.ty {
        Ty::Action => return Err("not a value".into()),
        Ty::Secret => Val::Str("sk-adversarial-00000000c0de".into()),
        _ if bd.key.ends_with(".template") => return Err("what an endpoint IS".into()),
        Ty::Percent { .. } | Ty::Number { .. } | Ty::Duration { .. } => {
            a.nudge_entry(Entry::Set(bd.clone()), true);
            if a.sget(bd).val == cur {
                a.nudge_entry(Entry::Set(bd.clone()), false);
            }
            return Ok(());
        }
        Ty::Choice(_) | Ty::Model { .. } | Ty::PoolRef { .. } | Ty::AccountRef => {
            let pick = a.choices(bd).into_iter().find(|c| Val::Str(c.clone()) != cur).ok_or("no other choice")?;
            Val::Str(pick)
        }
        Ty::EndpointRef => {
            let (id, _) = a.endpoint_refs().into_iter().find(|(id, _)| Val::Str(id.clone()) != cur).ok_or("no endpoint to bind")?;
            Val::Str(id)
        }
        Ty::List => Val::List(vec!["adversarial".into()]),
        Ty::Text => Val::Str("adversarial".into()),
        Ty::Url => Val::Str("https://adversarial.invalid".into()),
        Ty::Path => Val::Str("/tmp/adversarial".into()),
        Ty::Price => Val::Str("0.25".into()),
        Ty::Hours => Val::Str("01:00-02:00".into()),
        _ => return Err("unhandled".into()),
    };
    a.sset(bd, v)
}

/// What a setting's row shows: its value, its switch's position, whether
/// it says ↺, and its default note.
fn shown(a: &App, bd: &Bound) -> (String, usize, bool, String) {
    (a.value_text(bd), a.toggle_of(bd).selected, a.reverts(bd), a.default_note(bd))
}

/// THE ADVERSARIAL PASS (task 0212): on his real file shapes, every control
/// on every Config page changes what its row shows, the change survives a
/// reopen, its ↺ (where the row shows one) takes it back and THAT survives a
/// reopen — and his files never take a seed value along the way.
#[test]
fn adversarial_every_config_control_changes_shows_and_persists() {
    use tui::cauldron::menu::Entry;
    let (root, _, _) = root_with(HIS_GLOBAL, HIS_PROD);
    let controls = every_control(&open(&root));
    assert!(controls.len() > 100, "only {} controls found", controls.len());
    let (mut ok, mut skipped, mut refused, mut bad, mut greyed) = (0, Vec::new(), Vec::new(), Vec::new(), 0usize);
    for (page, bd) in controls {
        // Each on fresh copies of his files: no earlier write may mask it.
        let (root, g, c) = root_with(HIS_GLOBAL, HIS_PROD);
        let mut a = open(&root);
        let key = bd.concrete();
        let before = shown(&a, &bd);
        // V1.7 (R-V1 item 7): a greyed source's control is refused, and his
        // files stay byte-identical.
        if a.entry_greyed(&Entry::Set(bd.clone())) {
            let files = |g: &std::path::Path, c: &std::path::Path| (std::fs::read(g).unwrap_or_default(), std::fs::read(c).unwrap_or_default());
            let disk0 = files(&g, &c);
            let _ = change(&mut a, &bd);
            if files(&g, &c) != disk0 || shown(&a, &bd) != before {
                bad.push(format!("{key} ({page}): greyed, but the control changed it"));
            }
            greyed += 1;
            continue;
        }
        if let Err(why) = change(&mut a, &bd) {
            skipped.push(format!("{key} ({page}): {why}"));
            continue;
        }
        if let Some((_, why)) = a.refusal.take() {
            // Said on the row, under it: the control did what it shows.
            refused.push(format!("{key}: {why}"));
            continue;
        }
        let after = shown(&a, &bd);
        if after == before {
            bad.push(format!("{key} ({page}): the control changed nothing it shows: {before:?}"));
            continue;
        }
        let (val, set) = (a.sget(&bd).val, a.sis_set(&bd));
        let b = open(&root);
        if (b.sget(&bd).val, b.sis_set(&bd)) != (val.clone(), set) || shown(&b, &bd) != after {
            bad.push(format!("{key} ({page}): {after:?} did not survive a reopen (read {:?})", shown(&b, &bd)));
        }
        a = b;
        // ↺, where the row shows it.
        if a.reverts(&bd) {
            a.clear_entry(Entry::Set(bd.clone()));
            let back = shown(&a, &bd);
            if back.2 {
                bad.push(format!("{key} ({page}): ↺ left it at {back:?}"));
            }
            let b = open(&root);
            if shown(&b, &bd) != back {
                bad.push(format!("{key} ({page}): ↺ to {back:?} did not survive a reopen (read {:?})", shown(&b, &bd)));
            }
            a = b;
        }
        let (gt, ct) = (std::fs::read_to_string(&g).unwrap(), std::fs::read_to_string(&c).unwrap_or_default());
        if gt.contains("FAKE") || ct.contains("FAKE") || !gt.contains("provider = \"openrouter\"") {
            bad.push(format!("{key} ({page}): his files took a seed value or lost his own:\n{gt}\n{ct}"));
        }
        ok += 1;
    }
    eprintln!("adversarial: {ok} controls proved; {} refused on their row: {refused:?}; {} skipped:\n  {}", refused.len(), skipped.len(), skipped.join("\n  "));
    assert!(bad.is_empty(), "{} controls failed:\n  {}", bad.len(), bad.join("\n  "));
    assert!(greyed > 0 && ok > 100, "{greyed} greyed, {ok} proved");
}

/// The adversarial pass's other half: every BUTTON on every Config page
/// (`+ add`, rename, remove, register, run) does something he can see at one
/// press — a modal asking what it needs, or a line saying what happened.
#[test]
fn adversarial_every_config_button_does_something_visible() {
    use tui::cauldron::menu::{CfgTab, Entry, SPage};
    let (root, _, _) = root_with(HIS_GLOBAL, HIS_PROD);
    let a0 = open(&root);
    let mut buttons: Vec<(SPage, Entry)> = Vec::new();
    let mut seen: Vec<SPage> = Vec::new();
    let mut stack: Vec<SPage> = CfgTab::ALL.iter().map(|t| SPage::Tab(*t)).collect();
    while let Some(page) = stack.pop() {
        if seen.contains(&page) {
            continue;
        }
        seen.push(page.clone());
        for e in a0.page_entries(&page) {
            match &e {
                Entry::Sub { page: p, .. } | Entry::Row(p) => stack.push(p.clone()),
                _ if e.is_button() => buttons.push((page.clone(), e)),
                _ => {}
            }
        }
    }
    assert!(buttons.len() >= 5, "{buttons:?}");
    let mut bad = Vec::new();
    for (page, e) in &buttons {
        let mut a = open(&root);
        let before = render_to_text(&mut a, 120, 60).unwrap();
        a.open_entry(e.clone());
        let after = render_to_text(&mut a, 120, 60).unwrap();
        if a.modal.is_none() && a.notice.is_none() && a.error.is_none() && after == before {
            bad.push(format!("{page:?}: {e:?}"));
        }
    }
    eprintln!("adversarial: {} buttons pressed", buttons.len());
    assert!(bad.is_empty(), "buttons that do nothing visible:\n  {}", bad.join("\n  "));
}
