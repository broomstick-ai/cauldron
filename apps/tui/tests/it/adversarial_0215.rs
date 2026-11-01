//! **Click every control** (task 0215's adversarial pass, 0214's hunt for
//! controls that do nothing). Ignored by default: it runs against a COPY of a
//! real cauldron root, named by `CAULDRON_ADVERSARIAL_ROOT`, and writes what
//! every click did to `CAULDRON_ADVERSARIAL_OUT`:
//!
//! ```text
//! CAULDRON_TYPE_ONE=off CAULDRON_ADVERSARIAL_ROOT=/tmp/x/.cauldron CAULDRON_ADVERSARIAL_OUT=/tmp/x/clicks.tsv \
//!   cargo test -p tui --test it adversarial_0215 -- --ignored --nocapture
//! ```
//!
//! On every page of both cauldrons (Surface × every root cluster × its tabs,
//! Threads, every Status and Config sub-tab), every hit target the frame
//! records is pressed and released, one at a time, as the mouse would; a
//! modal it opens has every one of ITS targets pressed too. Each click is
//! recorded with what changed: the screen, the files under the face's own
//! dirs, a modal, a notice, the outbox. A click that changed none of them is
//! `DEAD` — the list a person then reads, because some are rightly inert (the
//! option already chosen). Finally a fresh face is opened on the root and the
//! file-changing clicks are checked to have persisted (the fresh face reads
//! the same files back without error).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui::cauldron::app::{App, CTab, Hit, KTab};
use tui::cauldron::menu::CfgTab;
use tui::cauldron::render_to_text;
use tui::cauldron::status::StatusTab;
use tui::cauldron::store::seed_dir;

const W: u16 = 140;
const H: u16 = 60;

fn env_in(root: &Path, name: &str) -> Environment {
    Environment { root: root.to_path_buf(), root_source: Source::CauldronHome, name: "prod".into(), name_source: Source::Default, data_root: root.join("data").join("prod") }
        .for_name(name)
        .unwrap()
}

fn files(root: &Path, env: &str) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(d: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if let Ok(b) = std::fs::read(&p) {
                out.insert(p, b);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(&root.join("data").join(env).join("tui"), &mut out);
    walk(&root.join("data/shared/tui"), &mut out);
    out
}

fn changed(a: &BTreeMap<PathBuf, Vec<u8>>, b: &BTreeMap<PathBuf, Vec<u8>>) -> Vec<String> {
    let mut out = Vec::new();
    for (k, v) in b {
        if a.get(k) != Some(v) {
            out.push(k.file_name().unwrap().to_string_lossy().to_string());
        }
    }
    for k in a.keys() {
        if !b.contains_key(k) {
            out.push(format!("-{}", k.file_name().unwrap().to_string_lossy()));
        }
    }
    out
}

/// The screen's text, then where the selection and the keys are (a click
/// that only selects changes a background, which text does not show).
fn screen(a: &mut App) -> Vec<String> {
    let mut s = render_to_text(a, W, H).unwrap();
    s.push(format!("selection {:?} {} {:?} {:?} {:?}", a.rows, a.item, a.focus, a.selected, a.threads.focus));
    s
}

/// The text a hit covers, and its row's.
fn label(rows: &[String], r: ratatui::layout::Rect) -> String {
    let Some(row) = rows.get(r.y as usize) else { return String::new() };
    let cells: Vec<char> = row.chars().collect();
    let pick: String = cells.iter().skip(r.x as usize).take(r.width.max(1) as usize).collect();
    let pick = pick.trim().to_string();
    let ctx: String = row.chars().filter(|c| !"│┃╭╮╰╯".contains(*c)).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{pick} ⟨{}⟩", ctx.chars().take(70).collect::<String>())
}

fn click(a: &mut App, r: ratatui::layout::Rect) {
    a.last_click = None;
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        a.handle(Event::Mouse(MouseEvent { kind, column: r.x, row: r.y, modifiers: KeyModifiers::NONE }));
    }
}

/// Navigation is a page of its own, and a drag is not a click.
fn skipped(h: &Hit) -> bool {
    matches!(h, Hit::CTab(_) | Hit::Strip(_) | Hit::KTab(_) | Hit::TreeRow(_) | Hit::Divider | Hit::BucketDivider | Hit::Env | Hit::Drop(_) | Hit::Modal)
}

fn in_modal(h: &Hit) -> bool {
    matches!(h, Hit::Choice(_) | Hit::ChipButton(_) | Hit::ConfirmButton(_) | Hit::BusLink(_) | Hit::MRow(_) | Hit::MOpt { .. } | Hit::MRevert(_) | Hit::MTip(_) | Hit::MPress(_))
}

struct Rec {
    out: std::fs::File,
    dead: usize,
    total: usize,
}

impl Rec {
    fn line(&mut self, env: &str, page: &str, what: &str, verdict: &str) {
        self.total += 1;
        if verdict.starts_with("DEAD") {
            self.dead += 1;
        }
        writeln!(self.out, "{env}\t{page}\t{what}\t{verdict}").unwrap();
    }
}

fn verdict(before: &(Vec<String>, BTreeMap<PathBuf, Vec<u8>>), a: &mut App, root: &Path, env: &str) -> String {
    let after = screen(a);
    let f = files(root, env);
    let mut v = Vec::new();
    let fc = changed(&before.1, &f);
    if !fc.is_empty() {
        v.push(format!("wrote {}", fc.join(",")));
    }
    if let Some(m) = &a.modal {
        v.push(format!("modal {}", format!("{m:?}").split([' ', '{', '(']).next().unwrap_or("")));
    }
    if let Some(n) = &a.notice {
        v.push(format!("notice «{}»", n.chars().take(80).collect::<String>()));
    }
    if let Some(e) = &a.error {
        v.push(format!("ERROR «{}»", e.chars().take(80).collect::<String>()));
    }
    if !a.outbox.is_empty() {
        v.push(format!("outbox {:?}", a.outbox.drain(..).map(|o| format!("{o:?}").chars().take(60).collect::<String>()).collect::<Vec<_>>()));
    }
    if after[..after.len() - 1] != before.0[..before.0.len() - 1] {
        let n = after.iter().zip(&before.0).filter(|(x, y)| x != y).count();
        v.push(format!("screen {n} rows"));
    } else if after.last() != before.0.last() {
        v.push("selects".into());
    }
    if v.is_empty() { "DEAD".into() } else { v.join(" · ") }
}

/// Press every hit of the page `setup` puts on screen, and every hit of each
/// modal one opens.
fn page(a: &mut App, rec: &mut Rec, root: &Path, env: &str, name: &str, setup: &dyn Fn(&mut App)) {
    let mut i = 0;
    loop {
        a.modal = None;
        a.dropdown = None;
        setup(a);
        let rows = screen(a);
        let hits: Vec<_> = a.hits.iter().copied().filter(|(_, h)| !skipped(h) && !in_modal(h)).collect();
        if i >= hits.len() || i > 400 {
            break;
        }
        let (r, h) = hits[i];
        let what = format!("{h:?} {}", label(&rows, r));
        let before = (rows, files(root, env));
        a.notice = None;
        a.error = None;
        click(a, r);
        let v = verdict(&before, a, root, env);
        rec.line(env, name, &what, &v);
        // A modal it opened: each of its targets, the modal opened afresh
        // for each.
        if a.modal.is_some() {
            let mut j = 0;
            loop {
                let mrows = screen(a);
                let mh: Vec<_> = a.hits.iter().copied().filter(|(_, h)| in_modal(h)).collect();
                if j >= mh.len() || j > 120 {
                    break;
                }
                let (mr, mhit) = mh[j];
                let mwhat = format!("  ↳ {mhit:?} {}", label(&mrows, mr));
                let before = (mrows, files(root, env));
                a.notice = None;
                a.error = None;
                click(a, mr);
                let v = verdict(&before, a, root, env);
                rec.line(env, name, &mwhat, &v);
                j += 1;
                // Back to the modal this hit was in, for the next one.
                a.modal = None;
                setup(a);
                let rows = screen(a);
                let again: Vec<_> = a.hits.iter().copied().filter(|(_, h)| !skipped(h) && !in_modal(h)).collect();
                match again.iter().find(|(_, x)| *x == h) {
                    Some(&(r2, _)) => {
                        let _ = rows;
                        click(a, r2);
                        a.outbox.clear();
                    }
                    None => break,
                }
                if a.modal.is_none() {
                    break;
                }
            }
        }
        i += 1;
    }
}

#[test]
#[ignore]
fn adversarial_0215_click_every_control() {
    let Ok(root) = std::env::var("CAULDRON_ADVERSARIAL_ROOT") else { panic!("set CAULDRON_ADVERSARIAL_ROOT") };
    let root = PathBuf::from(root);
    let out = std::env::var("CAULDRON_ADVERSARIAL_OUT").unwrap_or_else(|_| "/tmp/adversarial-0215.tsv".into());
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let mut rec = Rec { out: std::fs::File::create(&out).unwrap(), dead: 0, total: 0 };
    for env in ["prod", "design"] {
        let mut a = App::open(env_in(&root, env), seed_dir(), true).expect("opens");
        // Surface: every root cluster (and its first child), every tab.
        let mut clusters: Vec<usize> = a.forest.clusters.iter().enumerate().filter(|(_, g)| g.parent.is_none()).map(|(i, _)| i).collect();
        if let Some(c) = a.forest.clusters.iter().position(|g| g.parent.is_some()) {
            clusters.push(c);
        }
        for g in clusters {
            let key = a.forest.get(g).key.clone();
            for kt in KTab::ALL {
                {
                    let name = format!("Surface {key} {kt:?}");
                    let k2 = key.clone();
                    page(&mut a, &mut rec, &root, env, &name, &move |a: &mut App| {
                        a.set_ctab(CTab::Cauldron);
                        if let Some(g) = a.forest.find(&k2) {
                            if a.selected != g {
                                a.select(g);
                            }
                        }
                        a.set_ktab(kt);
                    });
                }
            }
        }
        page(&mut a, &mut rec, &root, env, "Threads", &|a: &mut App| a.set_ctab(CTab::Threads));
        for (i, t) in StatusTab::ALL.iter().enumerate() {
            page(&mut a, &mut rec, &root, env, &format!("Status {t:?}"), &move |a: &mut App| {
                a.set_ctab(CTab::Status);
                a.pick_strip(i);
            });
        }
        for t in CfgTab::ALL {
            page(&mut a, &mut rec, &root, env, &format!("Config {t:?}"), &move |a: &mut App| {
                a.set_ctab(CTab::Settings);
                if a.cfg_tab != t {
                    a.set_cfg_tab(t);
                }
            });
        }
        // The top bar: the dropdown, and each cauldron in it.
        a.modal = None;
        let rows = screen(&mut a);
        let (r, _) = *a.hits.iter().find(|(_, h)| *h == Hit::Env).unwrap();
        let before = (rows, files(&root, env));
        click(&mut a, r);
        let opened = a.dropdown.is_some();
        rec.line(env, "top bar", "Env (the cauldrons dropdown)", &if opened { "dropdown open".to_string() } else { verdict(&before, &mut a, &root, env) });
        a.handle(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        // Persisted: a fresh face reads every file back.
        let fresh = App::open(env_in(&root, env), seed_dir(), true);
        rec.line(env, "persist", "a fresh face opens on what the clicks wrote", if fresh.is_ok() { "ok" } else { "FAILED" });
    }
    eprintln!("adversarial: {} clicks, {} dead → {out}", rec.total, rec.dead);
}
