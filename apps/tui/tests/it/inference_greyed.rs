//! V1.7 (R-V1 item 7: "Claude Code only. All other endpoints are greyed
//! out."; product spec §15): Claude Code is the only live inference source.
//! Every other token source, every endpoint and every template is shown
//! greyed — never hidden — and every act on one is refused with a notice and
//! changes nothing on disk. What a user already wrote of an endpoint is kept.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use tui::cauldron::app::{App, CTab, Focus};
use tui::cauldron::manage::Act;
use tui::cauldron::menu::{Bound, Entry, Nav, SPage, SETTINGS_ROW, TOKEN_SOURCES};
use tui::cauldron::settings::{inference_live, Ctx, Val, GREYED, SERVERS, TEMPLATE_IDS};
use tui::cauldron::store::seed_dir;
use tui::cauldron::{render_fg_at, render_to_text};
use tui_render::palette;

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "inference-greyed-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn open(root: &Path) -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let env = Environment { root: root.to_path_buf(), root_source: Source::CauldronHome, name: "prod".into(), name_source: Source::Default, data_root: root.join("data").join("prod") }
        .for_name("prod")
        .unwrap();
    App::open(env, seed_dir(), true).expect("seed loads")
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn cfg(a: &mut App, path: &str) {
    a.set_ctab(CTab::Settings);
    a.focus = Focus::Content;
    a.snav = Nav::cauldron();
    a.rows[SETTINGS_ROW] = 0;
    a.modal = None;
    a.notice = None;
    a.open_settings_path(path).unwrap_or_else(|e| panic!("{path}: {e}"));
}

/// The global settings file's bytes (empty when there is none), and its
/// hash — two reads of the file, never one value compared with itself.
fn global_bytes(a: &App) -> Vec<u8> {
    std::fs::read(a.settings.global_path()).unwrap_or_default()
}

fn hash(b: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.hash(&mut h);
    h.finish()
}

fn select(a: &mut App, f: impl Fn(&Entry) -> bool) {
    a.rows[SETTINGS_ROW] = a.selectable().iter().position(f).unwrap_or_else(|| panic!("not selectable: {:?}", a.selectable()));
}

/// The foreground of the first cell of `needle` on the frame (120×80).
fn fg_of(a: &mut App, needle: &str) -> ratatui::style::Color {
    let lines = render_to_text(a, 120, 80).unwrap();
    let (y, line) = lines.iter().enumerate().find(|(_, l)| l.contains(needle)).unwrap_or_else(|| panic!("`{needle}` not on screen:\n{}", lines.join("\n")));
    let x = line[..line.find(needle).unwrap()].chars().count();
    render_fg_at(a, 120, 80, x as u16, y as u16).unwrap()
}

/// G1: one live source. Over every pool, template, local server and the
/// endpoints themselves the app's tables hold, `inference_live` is true for
/// `claude_code` and for nothing else (a constant `false` fails the first
/// assert, a constant `true` the second).
#[test]
fn g1_claude_code_is_the_one_live_source() {
    let a = open(&scratch_root());
    assert!(inference_live("claude_code"), "Claude Code is live");
    let mut ids: Vec<String> = TOKEN_SOURCES.iter().map(|s| s.to_string()).collect();
    ids.extend(a.pool_ids());
    ids.extend(TEMPLATE_IDS.iter().chain(SERVERS).map(|s| s.to_string()));
    ids.extend(["local".to_string(), "endpoints".to_string()]);
    ids.extend(a.settings.endpoint_ids(false));
    assert!(ids.iter().any(|i| i == "anthropic") && ids.iter().any(|i| i == "codex"), "the tables were read: {ids:?}");
    let live: Vec<&String> = ids.iter().filter(|i| inference_live(i)).collect();
    assert!(live.iter().all(|i| *i == "claude_code") && !live.is_empty(), "only Claude Code is live: {live:?}");
    // The Status tab's Codex band and the endpoints' band are closed, greyed.
    assert!(tui::cauldron::status::source_built("claude_code") && !tui::cauldron::status::source_built("codex") && !tui::cauldron::status::source_built("anthropic"));
}

/// G2: `+ add endpoint` is inert — Enter on it says why, opens nothing and
/// writes nothing — and `add endpoint` on a Gemini endpoint drafted with
/// everything its template needs is refused with the file byte-identical.
/// Opposition: on the same Providers tab a Claude Code model's switch DOES
/// change the file, so the hash is an instrument that can move.
#[test]
fn g2_adding_an_endpoint_is_refused_and_the_file_is_unchanged() {
    let mut a = open(&scratch_root());
    // Opposition first: a Claude Code switch writes.
    cfg(&mut a, "providers/claude_code");
    let h0 = hash(&global_bytes(&a));
    let opus = Bound::new("pools.{pool}.model.{model}", Ctx::model("claude_code", "opus"));
    assert!(a.sget(&opus).val.as_bool());
    select(&mut a, |e| *e == Entry::Set(opus.clone()));
    a.handle(key(KeyCode::Char(' ')));
    assert!(!a.sget(&opus).val.as_bool(), "the switch flipped");
    let h1 = hash(&global_bytes(&a));
    assert_ne!(h0, h1, "a Claude Code toggle changes the file");

    // `+ add endpoint`, both places: refused, said, no modal, no write.
    for (path, here) in [("providers/endpoints", false), ("cauldron/endpoints", true)] {
        cfg(&mut a, path);
        let before = global_bytes(&a);
        let local_before = std::fs::read(a.settings.dir.join("settings.toml")).unwrap_or_default();
        select(&mut a, |e| matches!(e, Entry::Do { act: Act::NewEndpoint { here: h }, .. } if *h == here));
        a.handle(key(KeyCode::Enter));
        assert!(a.modal.is_none(), "{path}: no add modal: {:?}", a.modal);
        assert_eq!(a.notice.as_deref(), Some(GREYED), "{path}");
        assert_eq!(hash(&global_bytes(&a)), hash(&before), "{path}: global file unchanged");
        assert_eq!(std::fs::read(a.settings.dir.join("settings.toml")).unwrap_or_default(), local_before, "{path}: cauldron file unchanged");
    }

    // A Gemini endpoint drafted with a key — everything its template needs —
    // and `add endpoint` pressed: refused, nothing minted, nothing written.
    let before = global_bytes(&a);
    let ids = a.settings.endpoint_ids(false);
    a.settings.draft_endpoint(false, "gemini");
    a.settings.global.vals.insert("endpoints.ep-0.key".into(), Val::Str("sk-FAKE-test-0000000000000000000000009999".into()));
    assert_eq!(a.draft_missing(false), None, "the draft has what Gemini needs");
    a.notice = None;
    a.start_act(Act::AddEndpoint { here: false });
    assert_eq!(a.notice.as_deref(), Some(GREYED));
    assert_eq!(a.settings.endpoint_ids(false), ids, "no endpoint minted");
    assert_eq!(hash(&global_bytes(&a)), hash(&before), "file byte-identical");
    assert!(!String::from_utf8_lossy(&global_bytes(&a)).contains("FAKE-test"));
}

/// G3: greyed, not hidden. Providers lists every token source and every
/// endpoint, each greyed in Codex's muted colour, and Claude Code's band is
/// NOT greyed (its colour differs from a greyed row's).
#[test]
fn g3_every_other_source_is_shown_greyed_and_claude_code_is_not() {
    let mut a = open(&scratch_root());
    let muted = palette::c().muted;
    // V1.1: the sources' bands are on Accounts; the endpoints on Providers.
    cfg(&mut a, "accounts");
    let cc = fg_of(&mut a, "Claude Code");
    assert_ne!(cc, muted, "Claude Code is live, not greyed");
    for p in a.pool_ids().into_iter().filter(|p| p != "claude_code") {
        assert!(a.selectable().iter().any(|e| matches!(e, Entry::Sub { page: SPage::Pool(q), .. } if *q == p)), "{p} is listed");
        let label = tui::cauldron::menu::pool_label(&p);
        assert_eq!(fg_of(&mut a, &label), muted, "{label} greyed");
    }
    cfg(&mut a, "providers");
    assert_eq!(fg_of(&mut a, "Inference endpoints"), muted, "the endpoints' band greyed");
    cfg(&mut a, "providers/endpoints");
    let rows: Vec<SPage> = a.selectable().into_iter().filter_map(|e| if let Entry::Row(p @ SPage::Endpoint { .. }) = e { Some(p) } else { None }).collect();
    assert_eq!(rows.len(), a.settings.endpoint_ids(false).len(), "every endpoint is a row");
    assert!(rows.len() >= 5);
    for id in a.settings.endpoint_ids(false) {
        let name = a.ep_name(false, &id);
        assert_eq!(fg_of(&mut a, &format!(" {name} ")[1..]), muted, "{name} greyed");
    }
    assert_eq!(fg_of(&mut a, "+ add endpoint"), muted, "`+ add endpoint` greyed");
    // Its row still opens, for reading.
    select(&mut a, |e| matches!(e, Entry::Row(SPage::Endpoint { .. })));
    a.handle(key(KeyCode::Enter));
    assert!(a.modal.is_some(), "a greyed endpoint's modal opens for reading");
}

/// G4: old data kept. A settings file holding an OpenRouter endpoint he
/// wrote opens with it shown greyed; Space on its row and `x` on it are
/// refused; and after a save that DOES happen (a Claude Code switch, the
/// opposition) every line of his endpoint is still in the file.
#[test]
fn g4_an_endpoint_he_wrote_is_kept_and_shown_greyed() {
    let root = scratch_root();
    let probe = open(&root);
    let gdir = probe.settings.global_dir.clone();
    drop(probe);
    std::fs::create_dir_all(&gdir).unwrap();
    let mine = "[endpoints]\nlast_id = 9\n\n[endpoints.ep-9]\nenabled = true\nname = \"mine\"\ntemplate = \"openrouter\"\n";
    std::fs::write(gdir.join("settings.toml"), mine).unwrap();
    let mut a = open(&root);
    assert_eq!(std::fs::read_to_string(gdir.join("settings.toml")).unwrap(), mine, "opening writes nothing");
    cfg(&mut a, "providers/endpoints");
    let row = Entry::Row(SPage::Endpoint { here: false, id: "ep-9".into() });
    assert!(a.selectable().contains(&row), "his endpoint is listed");
    assert_eq!(fg_of(&mut a, "mine "), palette::c().muted, "and greyed");
    select(&mut a, |e| *e == row);
    a.handle(key(KeyCode::Char(' ')));
    a.handle(key(KeyCode::Char('x')));
    assert!(a.modal.is_none(), "no remove asked: {:?}", a.modal);
    assert_eq!(a.notice.as_deref(), Some(GREYED));
    assert_eq!(std::fs::read_to_string(gdir.join("settings.toml")).unwrap(), mine, "refused acts write nothing");
    // A save that happens keeps his endpoint whole.
    let opus = Bound::new("pools.{pool}.model.{model}", Ctx::model("claude_code", "opus"));
    a.sset(&opus, Val::Bool(false)).unwrap();
    let after = std::fs::read_to_string(gdir.join("settings.toml")).unwrap();
    assert_ne!(after, mine, "the save happened");
    for l in mine.lines().filter(|l| !l.is_empty()) {
        assert!(after.lines().any(|x| x == l), "`{l}` kept: {after}");
    }
    drop(a);
    let b = open(&root);
    assert!(b.settings.endpoint_ids(false).contains(&"ep-9".to_string()), "still there on reopen");
    assert_eq!(b.ep_name(false, "ep-9"), "mine");
}

/// V1.7b F4: every greying of a SETTING goes through `inference_live`, so
/// un-greying one source is what reaches its settings. Asked with a liveness
/// in which `endpoints` (the name the Config pages and acts on endpoints ask
/// about) is live, every endpoint's setting — global, this cauldron's, what
/// it takes of one, their defaults — is NOT greyed, while local inference
/// and Codex still are; asked with `local` live instead, the reverse. The
/// production `greyed_key` is that same function over `inference_live`, def
/// by def. Opposition: a constant-`true` endpoint branch (V1.7's) fails the
/// first half, a constant-`false` one the second.
#[test]
fn f4_ungreying_one_source_reaches_its_settings_and_only_its() {
    use tui::cauldron::settings::{greyed_key, greyed_key_by, greyed_value, greyed_value_by, inference_source, DEFS};
    let ctxs = |key: &str| -> Vec<Ctx> {
        if key.contains("{pool}") {
            vec![Ctx::account("claude_code", "acct1"), Ctx::account("codex", "acct1")]
        } else if key.contains("{ep}") {
            vec![Ctx::ep("ep-1")]
        } else {
            vec![Ctx::default()]
        }
    };
    let ep_keys: Vec<&str> = DEFS.iter().map(|d| d.key).filter(|k| k.starts_with("endpoints.") || k.starts_with("cauldron.endpoints.") || k.starts_with("cauldron.use.")).collect();
    let local_keys: Vec<&str> = DEFS.iter().map(|d| d.key).filter(|k| k.starts_with("local.")).collect();
    assert!(ep_keys.len() > 20 && ep_keys.iter().any(|k| k.starts_with("cauldron.use.")) && !local_keys.is_empty(), "the defs were read: {ep_keys:?} {local_keys:?}");
    let endpoints_live = |s: &str| s == "claude_code" || s == "endpoints";
    let local_live = |s: &str| s == "claude_code" || s == "local";
    for k in &ep_keys {
        for c in ctxs(k) {
            assert_eq!(inference_source(k, &c).as_deref(), Some("endpoints"), "{k}");
            assert!(!greyed_key_by(k, &c, endpoints_live), "un-greying `endpoints` reaches {k}");
            assert!(greyed_key_by(k, &c, local_live), "{k} stays greyed while endpoints are");
        }
    }
    for k in &local_keys {
        assert!(!greyed_key_by(k, &Ctx::default(), local_live), "un-greying `local` reaches {k}");
        assert!(greyed_key_by(k, &Ctx::default(), endpoints_live), "{k} stays greyed while local is");
    }
    // A pool's own: Codex greyed under both, Claude Code under neither.
    let codex = Ctx::account("codex", "acct1");
    let cc = Ctx::account("claude_code", "acct1");
    for live in [&endpoints_live as &dyn Fn(&str) -> bool, &local_live] {
        assert!(greyed_key_by("pools.{pool}.accounts.{account}.enabled", &codex, live));
        assert!(!greyed_key_by("pools.{pool}.accounts.{account}.enabled", &cc, live));
    }
    // A role bound to an endpoint: refused only while endpoints are greyed.
    let ep1 = Val::Str("ep-1".into());
    assert!(!greyed_value_by("roles.{role}.endpoint", &ep1, endpoints_live));
    assert!(greyed_value_by("roles.{role}.endpoint", &ep1, local_live));
    // The production verdicts ARE the routed ones over `inference_live`.
    let mut n = 0;
    for d in DEFS.iter() {
        for c in ctxs(d.key) {
            assert_eq!(greyed_key(d.key, &c), greyed_key_by(d.key, &c, inference_live), "{}", d.key);
            n += 1;
        }
    }
    assert!(n > 100, "{n} defs compared");
    assert!(greyed_key("endpoints.{ep}.key", &Ctx::ep("ep-1")) && greyed_value("roles.{role}.endpoint", &ep1), "today endpoints are greyed");
}
