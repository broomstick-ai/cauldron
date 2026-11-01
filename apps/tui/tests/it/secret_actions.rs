//! V1.8 — a human action's secret field: a masked input, and a routing
//! adapter that sends the value only where its ticket's `route:` says (R-V1
//! item 8; product spec §9). Driven headlessly through `App::handle` against
//! a scratch cauldron root (never `~/.cauldron`), with an injected keychain
//! (never the login keychain) and a scratch git repo for the env file.
//!
//! Every "no value leaked" probe uses one SENTINEL, built at run time from
//! two halves so that the whole string appears nowhere in the source tree
//! (`sentinel_is_in_no_source_file` proves that first, with a positive
//! control over the same scan).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use tui::cauldron::app::{App, CTab, Focus, Item, KTab, Modal};
use tui::cauldron::render_to_text;
use tui::cauldron::route::{self, Keychain, Secret};
use tui::cauldron::settings::{def, Ctx, Val};
use tui::cauldron::store::seed_dir;
use tui::cauldron::tickets::Part;

fn sentinel() -> String {
    format!("{}-{}", "SENTINEL", "7f3a")
}

// ── fixtures ────────────────────────────────────────────────────────────────

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "v18-secret-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

const BODY: &str = "Three values for the payments service.\n\n## Step 1 — the keys\n\n```field\nname: STRIPE_SECRET_KEY\nlabel: Stripe secret key\nroute: keychain\n```\n\n```field\nname: SUPABASE_KEY\nlabel: Supabase key\nroute: env\n```\n\n## Step 2 — the project url\n\n```field\nname: PROJECT_URL\nlabel: Project URL\nsecret: false\nroute: node\n```\n\n## Step 3 — a loose key\n\n```field\nname: LOOSE_KEY\nlabel: Loose key\n```\n";

/// A cauldron `prod` in `root` whose own seed is one cluster holding one
/// open human action with the fields above.
fn write_seed(root: &Path) {
    let seed = root.join("data/prod/tui/soup/seed");
    std::fs::create_dir_all(&seed).unwrap();
    let clusters = serde_json::json!({
        "_note": "V1.8 fixture",
        "clusters": [{
            "id": "X-1",
            "name": "Payments",
            "description": "The payments service's keys.",
            "human_actions": [{"serial": 1, "ask": "Give the payments service its keys", "status": "open", "filed": "2026-10-30", "body": BODY}]
        }]
    });
    std::fs::write(seed.join("clusters.json"), serde_json::to_string_pretty(&clusters).unwrap()).unwrap();
}

fn env_in(root: &Path) -> Environment {
    Environment { root: root.to_path_buf(), root_source: Source::CauldronHome, name: "prod".into(), name_source: Source::Default, data_root: root.join("data").join("prod") }
}

#[derive(Default)]
struct FakeKeychain {
    items: Mutex<Vec<(String, String, String)>>,
}

impl Keychain for FakeKeychain {
    fn set(&self, service: &str, account: &str, value: &Secret) -> Result<(), String> {
        let mut v = self.items.lock().unwrap();
        v.retain(|(s, a, _)| !(s == service && a == account));
        v.push((service.into(), account.into(), value.expose().into()));
        Ok(())
    }
}

/// The face on `root`, on X-1's Actions tab, with a fake keychain.
fn open(root: &Path) -> (App, Arc<FakeKeychain>) {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let mut a = App::open(env_in(root), seed_dir(), true).expect("seed loads");
    let kc = Arc::new(FakeKeychain::default());
    a.keychain = kc.clone();
    a.set_ctab(CTab::Cauldron);
    let g = a.forest.find("X-1").expect("X-1");
    a.select(g);
    a.set_ktab(KTab::Actions);
    (a, kc)
}

/// Every notice, error and modal text the face showed, for the leak probes.
#[derive(Default)]
struct Said(Vec<String>);

fn ev(a: &mut App, e: Event, said: &mut Said) {
    a.handle(e);
    said.0.extend(a.notice.clone());
    said.0.extend(a.error.clone());
    if let Some(m) = &a.modal {
        said.0.push(format!("{m:?}"));
    }
}

fn key(a: &mut App, code: KeyCode, said: &mut Said) {
    ev(a, Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), said);
}

fn typed(a: &mut App, s: &str, said: &mut Said) {
    for c in s.chars() {
        key(a, KeyCode::Char(c), said);
    }
}

fn screen(a: &mut App) -> String {
    render_to_text(a, 120, 70).unwrap().join("\n")
}

/// The item of field `name`'s part.
fn field_item(a: &App, name: &str) -> Item {
    let g = a.forest.find("X-1").unwrap();
    let (_, t) = a.ticket(g, 1).unwrap();
    let ix = t.parts().iter().position(|p| matches!(p, Part::Field { field, .. } if field.name == name)).unwrap_or_else(|| panic!("no field {name}"));
    Item::HaPart { cluster: g, serial: 1, part: ix }
}

fn select(a: &mut App, it: Item) {
    a.item = a.items().iter().position(|x| *x == it).unwrap_or_else(|| panic!("{it:?} is not on the page: {:?}", a.items()));
    a.focus = Focus::Content;
}

/// Select field `name`, Enter into it, type `value`.
fn type_into(a: &mut App, name: &str, value: &str, said: &mut Said) {
    let it = field_item(a, name);
    select(a, it);
    key(a, KeyCode::Enter, said);
    assert!(a.ha_entry.as_ref().and_then(|e| e.editing.as_deref()) == Some(name), "{name} takes keys: {:?}", a.notice);
    typed(a, value, said);
}

/// Every file under `dir` whose bytes contain `needle`.
fn files_holding(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            let ft = e.file_type().unwrap();
            if ft.is_dir() {
                todo.push(p);
            } else if ft.is_file() {
                let b = std::fs::read(&p).unwrap_or_default();
                if b.windows(needle.len()).any(|w| w == needle.as_bytes()) {
                    out.push(p);
                }
            }
        }
    }
    out
}

/// A scratch git repo with `.gitignore` ignoring `ignored.env`.
fn git_repo(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let st = std::process::Command::new("git").args(["init", "-q"]).current_dir(&repo).status().unwrap();
    assert!(st.success());
    std::fs::write(repo.join(".gitignore"), "ignored.env\n").unwrap();
    repo
}

fn set_env_file(a: &mut App, path: &Path) {
    a.settings.set(def("secrets.env_file"), &Ctx::default(), Val::Str(path.to_string_lossy().into())).unwrap();
}

fn confirm_error(a: &App) -> Option<String> {
    match &a.modal {
        Some(Modal::Confirm { error, .. }) => error.clone(),
        _ => None,
    }
}

// ── the sentinel is nowhere in the source ──────────────────────────────────

/// Doctrine 3: the leak probes are only as good as their sentinel. The whole
/// string must be in no source file; the same scan finds a half of it in
/// this file (positive control: the scan can find).
#[test]
fn sentinel_is_in_no_source_file() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut hits = Vec::new();
    let mut halves = 0;
    for top in ["apps", "libs", "services", "data", "cli"] {
        let mut todo = vec![repo.join(top)];
        while let Some(d) = todo.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                let name = e.file_name().to_string_lossy().to_string();
                if e.file_type().unwrap().is_dir() {
                    if name != "target" && !name.starts_with('.') {
                        todo.push(p);
                    }
                    continue;
                }
                if !matches!(p.extension().and_then(|x| x.to_str()), Some("rs" | "json" | "toml" | "md" | "py" | "txt" | "jsonl")) {
                    continue;
                }
                let t = std::fs::read_to_string(&p).unwrap_or_default();
                if t.contains(&sentinel()) {
                    hits.push(p.clone());
                }
                if p.ends_with("tests/it/secret_actions.rs") && t.contains("\"7f3a\"") {
                    halves += 1;
                }
            }
        }
    }
    assert_eq!(halves, 1, "the scan reached this file and found the half it holds");
    assert!(hits.is_empty(), "the sentinel is written in {hits:?}");
}

// ── G3: masked ─────────────────────────────────────────────────────────────

/// G3: typing the sentinel into a secret field, no frame holds any 3-long
/// piece of it that the frame before typing did not; the line shows one
/// `•` per character. Opposition: a `secret: false` field shows its text.
#[test]
fn g3_a_secret_field_is_masked_in_every_frame_and_a_plain_one_is_not() {
    let root = scratch_root();
    write_seed(&root);
    let (mut a, _kc) = open(&root);
    let mut said = Said::default();
    let s = sentinel();
    let grams: Vec<String> = s.chars().collect::<Vec<_>>().windows(3).map(|w| w.iter().collect()).collect();
    let base = screen(&mut a);
    let watched: Vec<&String> = grams.iter().filter(|g| !base.contains(g.as_str())).collect();
    assert!(watched.iter().any(|g| g.as_str() == "7f3") && watched.len() >= 6, "the probe watches enough pieces: {watched:?}");
    let it = field_item(&a, "STRIPE_SECRET_KEY");
    select(&mut a, it);
    key(&mut a, KeyCode::Enter, &mut said);
    for c in s.chars() {
        key(&mut a, KeyCode::Char(c), &mut said);
        let f = screen(&mut a);
        for g in &watched {
            assert!(!f.contains(g.as_str()), "a frame shows `{g}`:\n{f}");
        }
    }
    let f = screen(&mut a);
    let dots = "•".repeat(s.chars().count());
    let line = f.lines().find(|l| l.contains("› •")).unwrap_or_else(|| panic!("no masked input:\n{f}"));
    assert!(line.contains(&format!("› {dots}▏")) && !line.contains(&format!("{dots}•")), "{} dots: {line}", s.chars().count());
    // Esc clears it.
    key(&mut a, KeyCode::Esc, &mut said);
    assert!(a.ha_entry.is_none());
    // Opposition: a non-secret field echoes what is typed.
    type_into(&mut a, "PROJECT_URL", "https://pay.example", &mut said);
    let f = screen(&mut a);
    assert!(f.contains("› https://pay.example▏"), "{f}");
    for t in &said.0 {
        assert!(!t.contains(&s), "said: {t}");
    }
}

/// A field the validator refuses has no input and takes no keys; a paste
/// into a secret field is masked like keys.
#[test]
fn an_unroutable_secret_field_is_refused_and_takes_no_keys_and_a_paste_is_masked() {
    let root = scratch_root();
    write_seed(&root);
    let (mut a, _kc) = open(&root);
    let mut said = Said::default();
    let f = screen(&mut a);
    assert!(f.contains("✗ refused: LOOSE_KEY is secret and names no destination"), "{f}");
    let it = field_item(&a, "LOOSE_KEY");
    select(&mut a, it);
    key(&mut a, KeyCode::Enter, &mut said);
    assert!(a.notice.as_deref().is_some_and(|n| n.starts_with("refused:")), "{:?}", a.notice);
    assert!(a.ha_entry.is_none());
    // A paste.
    let it = field_item(&a, "SUPABASE_KEY");
    select(&mut a, it);
    key(&mut a, KeyCode::Enter, &mut said);
    ev(&mut a, Event::Paste(format!("{}\n", sentinel())), &mut said);
    let f = screen(&mut a);
    assert!(f.contains(&format!("› {}▏", "•".repeat(13))) && !f.contains("7f3"), "{f}");
}

// ── G4 + G6: the env route, gated, and nothing leaks ───────────────────────

/// G4: into a scratch git repo — ignored, Submit writes `NAME=value` at mode
/// 0600; not ignored, Submit is refused and no file is made. G6: nothing
/// the face said or wrote, but the env file, holds the value; tickets.json
/// holds the field's NAME (the scan can find).
#[test]
fn g4_g6_the_env_route_writes_only_an_ignored_file_and_nothing_else_holds_the_value() {
    use std::os::unix::fs::PermissionsExt;
    let root = scratch_root();
    write_seed(&root);
    let repo = git_repo(&root);
    let (mut a, _kc) = open(&root);
    let mut said = Said::default();
    let s = sentinel();
    // Not ignored: refused, nothing made.
    let tracked = repo.join("tracked.env");
    set_env_file(&mut a, &tracked);
    type_into(&mut a, "SUPABASE_KEY", &s, &mut said);
    key(&mut a, KeyCode::Enter, &mut said);
    assert!(matches!(&a.modal, Some(Modal::Confirm { body, .. }) if body.len() == 1 && body[0].starts_with("SUPABASE_KEY → env file")), "{:?}", a.modal);
    key(&mut a, KeyCode::Enter, &mut said);
    let e = confirm_error(&a).unwrap_or_default();
    assert!(e.contains("NOT git-ignored"), "{e:?} / {:?}", a.modal);
    assert!(!tracked.exists(), "a trackable target is never written");
    key(&mut a, KeyCode::Esc, &mut said);
    assert!(!a.tickets.is_submitted("X-1", 1, "SUPABASE_KEY"));
    // Ignored: written, 0600, marked submitted by name.
    let ignored = repo.join("ignored.env");
    set_env_file(&mut a, &ignored);
    type_into(&mut a, "SUPABASE_KEY", &s, &mut said);
    key(&mut a, KeyCode::Enter, &mut said);
    key(&mut a, KeyCode::Enter, &mut said);
    assert!(a.modal.is_none(), "{:?}", a.modal);
    assert_eq!(std::fs::read_to_string(&ignored).unwrap(), format!("SUPABASE_KEY={s}\n"));
    assert_eq!(std::fs::metadata(&ignored).unwrap().permissions().mode() & 0o777, 0o600);
    assert!(a.tickets.is_submitted("X-1", 1, "SUPABASE_KEY"));
    assert!(a.ha_entry.is_none(), "cleared on Submit");
    let f = screen(&mut a);
    assert!(f.contains("•••• submitted"), "{f}");
    // G6.
    let tickets = a.tickets.path().to_path_buf();
    assert!(std::fs::read_to_string(&tickets).unwrap().contains("X-1/HA-1/SUPABASE_KEY"), "the name is recorded");
    let holding = files_holding(&root, &s);
    assert_eq!(holding, vec![ignored.clone()], "only the env file holds the value");
    assert!(!files_holding(&root, "X-1/HA-1/SUPABASE_KEY").is_empty());
    for t in &said.0 {
        assert!(!t.contains(&s), "said: {t}");
    }
    assert!(!f.contains("7f3"), "{f}");
    for argv in route::spawned() {
        assert!(!argv.iter().any(|x| x.contains(&s)), "{argv:?}");
    }
}

// ── G5: the keychain route ─────────────────────────────────────────────────

/// G5: with an injected keychain, Submit stores service `cauldron.prod`,
/// account the field's name, the value; no spawned argv holds it.
#[test]
fn g5_the_keychain_route_stores_the_item_and_no_argv_holds_the_value() {
    let root = scratch_root();
    write_seed(&root);
    let (mut a, kc) = open(&root);
    let mut said = Said::default();
    let s = sentinel();
    type_into(&mut a, "STRIPE_SECRET_KEY", &s, &mut said);
    key(&mut a, KeyCode::Enter, &mut said);
    assert!(matches!(&a.modal, Some(Modal::Confirm { body, .. }) if body == &vec!["STRIPE_SECRET_KEY → keychain cauldron.prod/STRIPE_SECRET_KEY".to_string()]), "{:?}", a.modal);
    assert!(kc.items.lock().unwrap().is_empty(), "nothing before the confirm");
    key(&mut a, KeyCode::Enter, &mut said);
    assert!(a.modal.is_none(), "{:?}", a.modal);
    assert_eq!(*kc.items.lock().unwrap(), vec![("cauldron.prod".to_string(), "STRIPE_SECRET_KEY".to_string(), s.clone())]);
    assert!(a.tickets.is_submitted("X-1", 1, "STRIPE_SECRET_KEY"));
    for argv in route::spawned() {
        assert!(!argv.iter().any(|x| x.contains(&s)), "{argv:?}");
    }
    for t in &said.0 {
        assert!(!t.contains(&s), "said: {t}");
    }
    assert!(files_holding(&root, &s).is_empty(), "{:?}", files_holding(&root, &s));
}

/// The node route (non-secret only): the value goes out as a dump addressed
/// to the action, and the field is marked submitted.
#[test]
fn the_node_route_sends_a_dump_addressed_to_the_action() {
    let root = scratch_root();
    write_seed(&root);
    let (mut a, _kc) = open(&root);
    let mut said = Said::default();
    type_into(&mut a, "PROJECT_URL", "https://pay.example", &mut said);
    key(&mut a, KeyCode::Enter, &mut said);
    key(&mut a, KeyCode::Enter, &mut said);
    assert!(a.modal.is_none(), "{:?} {:?}", a.modal, a.error);
    assert!(a.tickets.is_submitted("X-1", 1, "PROJECT_URL"));
    assert!(!files_holding(&root, "[HA-1.PROJECT_URL] https://pay.example").is_empty(), "the dump holds the reference and the value");
}

// ── G7: not persisted, cleared ─────────────────────────────────────────────

/// G7: type the sentinel, Esc, quit, reopen: the input is empty, and no file
/// under the scratch root holds it — the same scan first finds a planted one.
#[test]
fn g7_a_typed_value_is_never_persisted_and_esc_clears_it() {
    let root = scratch_root();
    write_seed(&root);
    let s = sentinel();
    // Positive control: the scan finds a planted file in the same tree.
    let plant = root.join("data/prod/tui/planted.txt");
    std::fs::create_dir_all(plant.parent().unwrap()).unwrap();
    std::fs::write(&plant, &s).unwrap();
    assert_eq!(files_holding(&root, &s), vec![plant.clone()]);
    std::fs::remove_file(&plant).unwrap();
    {
        let (mut a, _kc) = open(&root);
        let mut said = Said::default();
        type_into(&mut a, "STRIPE_SECRET_KEY", &s, &mut said);
        // Moving off the step clears it too: back on, it is empty.
        key(&mut a, KeyCode::Esc, &mut said);
        assert!(a.ha_entry.is_none(), "Esc clears");
        type_into(&mut a, "STRIPE_SECRET_KEY", &s, &mut said);
        a.quit = true;
    }
    let (mut b, _kc) = open(&root);
    assert!(b.ha_entry.is_none());
    let f = screen(&mut b);
    assert!(!f.contains("› •") && f.contains("Enter to type"), "{f}");
    assert!(files_holding(&root, &s).is_empty(), "{:?}", files_holding(&root, &s));
}

/// Moving the selection off the step drops what was typed in it.
#[test]
fn leaving_the_step_clears_its_values() {
    let root = scratch_root();
    write_seed(&root);
    let (mut a, _kc) = open(&root);
    let mut said = Said::default();
    type_into(&mut a, "STRIPE_SECRET_KEY", "abc", &mut said);
    // Down to the next field of the same step: kept, not taking keys.
    key(&mut a, KeyCode::Down, &mut said);
    let e = a.ha_entry.as_ref().expect("same step: kept");
    assert!(e.editing.is_none() && e.value("STRIPE_SECRET_KEY").is_some_and(|v| v.chars() == 3));
    // Into step 2: gone.
    let it = field_item(&a, "PROJECT_URL");
    select(&mut a, it);
    key(&mut a, KeyCode::Up, &mut said);
    key(&mut a, KeyCode::Down, &mut said);
    assert!(a.ha_entry.is_none(), "{:?}", a.ha_entry);
}

// ── G8: no default route ───────────────────────────────────────────────────

/// G8: `route: env` with `secrets.env_file` unset is refused naming the
/// setting, and writes nothing. Opposition: set, it writes.
#[test]
fn g8_an_env_route_with_no_env_file_is_refused_and_there_is_no_default() {
    let root = scratch_root();
    write_seed(&root);
    let (mut a, _kc) = open(&root);
    let mut said = Said::default();
    let s = sentinel();
    assert!(a.secrets_env_file().is_none());
    type_into(&mut a, "SUPABASE_KEY", &s, &mut said);
    key(&mut a, KeyCode::Enter, &mut said);
    key(&mut a, KeyCode::Enter, &mut said);
    let e = confirm_error(&a).unwrap_or_default();
    assert!(e.contains("secrets.env_file is not set"), "{e:?}");
    assert!(files_holding(&root, &s).is_empty(), "{:?}", files_holding(&root, &s));
    assert!(!a.tickets.is_submitted("X-1", 1, "SUPABASE_KEY"));
    key(&mut a, KeyCode::Esc, &mut said);
    // Opposition: with the setting, it writes (outside any git repo).
    let file = root.join("outside/vals.env");
    set_env_file(&mut a, &file);
    type_into(&mut a, "SUPABASE_KEY", &s, &mut said);
    key(&mut a, KeyCode::Enter, &mut said);
    key(&mut a, KeyCode::Enter, &mut said);
    assert!(a.modal.is_none(), "{:?}", a.modal);
    assert_eq!(files_holding(&root, &s), vec![file]);
}
