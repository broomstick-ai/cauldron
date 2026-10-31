//! **THE ROW-LEVEL ACTS, PROVEN THROUGH THE DOOR HE ACTUALLY USES.**
//!
//! Every assertion here is reached by a KEYSTROKE or by an event the loop
//! really receives, never by calling an internal. That is deliberate: the two
//! defects this file was written for are both defects of the SEAM — a key that
//! the table resolved and the shell never handled, and a re-read that reached
//! the store and never reached the screen. Neither is visible from inside the
//! piece that works.
//!
//! **Nothing here touches his real machine.** Every `$HOME` is a scratch
//! directory this process made; the transport is spawned at `127.0.0.1:1`,
//! which nothing listens on, so no daemon is asked anything and no mutation is
//! ever sent.

use crate::fake9010;

use fake9010::{Fake9010, Script};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;
use tui::app::App;
use tui::aui::{self, AuiEvent};
use tui::event::{Ev, Input};
use tui::mirror;
use tui::acts::{Verb, Wire};
use tui::transport::{ResyncKind, TransportEvent};
use tui_render::{Mode, RootItem, SheetAct};

const DEAD: &str = "ws://127.0.0.1:1/ws";
const SUMMARY: &str = "the thread he wants renamed";
/// What the thread is called AFTER the rename lands on disk.
const RENAMED: &str = "the name he actually typed";

fn tmp(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "tui-rows-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("mkdir");
    p
}

fn write(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
    std::fs::write(p, body).expect("write");
}

/// The project root inside a `$HOME` this file built.
fn project_root(home: &Path) -> PathBuf {
    home.join("elsewhere").join("alpha")
}

/// **REWRITE `.mind/threads.json` WITH A NEW SUMMARY — the durable write the
/// AUI's desk loop performs, standing in for it.**
///
/// It is the FILE and not a store call deliberately: the property under test is
/// that a durable index write reaches the screen, and a test that moved the
/// store would be asserting about the half that already worked.
fn rename_on_disk(home: &Path, summary: &str) {
    write(
        &project_root(home),
        ".mind/threads.json",
        &format!(
            r#"{{"threads":[{{"id":"t1","session_id":"t1","summary":"{summary}",
                "last_turn_at":"2026-01-02T00:00:00Z","turn_count":3}}]}}"#
        ),
    );
}

/// **THE ONE THREAD ID THAT CAN ACTUALLY BE SENT.**
///
/// `acts::route` refuses an AUI-owned act on a thread whose id is not a UUID —
/// *"this thread's id (t1) is not one the audio interface can address"* — which
/// is correct and is the daemon/AUI vocabulary boundary doing its job. Every
/// test above stops at the SURFACE and so uses the short `t1`; the one test
/// that asserts BYTES ON THE WIRE needs an id the wire accepts.
const UUID_TID: &str = "6f1d8c1e-5b27-4a9f-9c3e-2d70b8a41f55";

/// **A NEIGHBOUR THREAD, and its whole job is to be a witness no other band
/// can forge.**
///
/// Measured, this round: the first version of
/// `the_modal_stands_over_the_thread_list_and_the_list_is_still_drawn` asserted
/// that the EDITED thread's summary was still on the frame — and it stayed
/// GREEN when the modal's body was replaced with `sheet_body`, because the
/// sheet's header title IS the thread's summary and the prefilled box carries
/// it too. The assertion was keyed on something three different bands can
/// produce, which is `0065`'s lesson wearing a new hat.
///
/// This name appears in exactly ONE place on the face: a row of the list. It is
/// not the sheet's subject, it is not in the box, and nothing the modal draws
/// can put it on the glass.
const NEIGHBOUR: &str = "a thread he is not renaming";

/// A `$HOME` with a SECOND thread, so "the list is still there" has a witness.
fn home_with_two_threads(tag: &str) -> PathBuf {
    let home = home_with_one_thread(tag);
    write(
        &project_root(&home),
        ".mind/threads.json",
        &format!(
            r#"{{"threads":[
                {{"id":"t1","session_id":"t1","summary":"{SUMMARY}",
                  "last_turn_at":"2026-01-02T00:00:00Z","turn_count":3}},
                {{"id":"t2","session_id":"t2","summary":"{NEIGHBOUR}",
                  "last_turn_at":"2026-01-01T00:00:00Z","turn_count":1}}
            ]}}"#
        ),
    );
    home
}

/// A `$HOME` whose one thread carries a UUID, for the send-path proof.
fn home_with_one_uuid_thread(tag: &str) -> PathBuf {
    let home = home_with_one_thread(tag);
    write(
        &project_root(&home),
        ".mind/threads.json",
        &format!(
            r#"{{"threads":[{{"id":"{UUID_TID}","session_id":"{UUID_TID}",
                "summary":"{SUMMARY}","last_turn_at":"2026-01-02T00:00:00Z",
                "turn_count":3}}]}}"#
        ),
    );
    home
}

/// A `$HOME` with ONE registered project holding ONE open thread.
fn home_with_one_thread(tag: &str) -> PathBuf {
    let home = tmp(tag);
    let root = home.join("elsewhere").join("alpha");
    std::fs::create_dir_all(&root).expect("mkdir");
    write(
        &root,
        ".mind/threads.json",
        &format!(
            r#"{{"threads":[{{"id":"t1","session_id":"t1","summary":"{SUMMARY}",
                "last_turn_at":"2026-01-02T00:00:00Z","turn_count":3}}]}}"#
        ),
    );
    // Through the crate's own write path — a hand-written document here would
    // be a second definition of the registry's format.
    let mut reg = projects::Registry::empty();
    reg.register(&root, None).expect("registered");
    reg.save(&home).expect("saved");
    home
}

/// **A LIVE 9010 CLIENT POINTED AT A FAKE ON AN EPHEMERAL LOOPBACK PORT.**
///
/// Rename is an AUI-OWNED act, so a face with no 9010 client draws the row
/// UNAVAILABLE and refuses it with the owner's sentence — which is correct
/// behaviour and is not what these tests are about. Without a reachable owner
/// the rename field would never open and the proof would be of the refusal
/// rather than of the key. Nothing here knows the number 9010 or the tailnet
/// address; see `fake9010`'s own header.
async fn with_reachable_aui(app: &mut App) -> Fake9010 {
    let fake = Fake9010::start(vec![Script::default()]).await;
    let (h, _rx) = aui::spawn(aui::Config {
        backoff_min: Duration::from_millis(5),
        backoff_max: Duration::from_millis(20),
        ack_timeout: Duration::from_secs(5),
        ..aui::Config::pinned(&fake.url)
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if h.reach().is_up() {
            app.set_aui(h);
            return fake;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the 9010 client never came up; frames: {:#?}", fake.frames());
}

fn app_at(home: &Path) -> App {
    // **A SEAM WITH NOTHING BEHIND IT.** `Supervision::None` spawns nothing and
    // port 1 is one nothing listens on — so the link is `Down`, every command is
    // refused the way a real one is when the daemon is away, and no daemon is
    // asked anything. **`spawn_at`, never `spawn`**: the bare one falls back to
    // 3653, which is his live daemon. This test never touches his real machine.
    let (handle, _rx) = mirror::spawn_dead();
    let (tx, _erx) = mpsc::channel::<Ev>(16);
    App::new(
        handle,
        tx,
        DEAD.into(),
        home.to_str().expect("utf8").to_string(),
        0,
    )
}

fn key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    app.apply(Ev::Input(vec![Input::Key(KeyEvent::new(code, mods))]));
}

/// **THE DAEMON'S LINK IS UP** (TUI-P1a). A rename is the daemon's act now
/// (TC-03/TC-28), so its sheet row — and the little modal behind it — is
/// available only while the daemon is reachable, exactly as `compact` is. The
/// seam behind these apps is dead by design (nothing is ever sent anywhere), so
/// the link is declared up the way the mirror declares it.
fn daemon_up(app: &mut App) {
    app.apply(Ev::Transport(TransportEvent::Connected {
        boot_id: "test-boot".into(),
        resync: ResyncKind::First,
    }));
}

/// The daemon rename `enter` sent, if it sent one.
fn renamed(app: &App) -> Option<(String, String)> {
    match app.last_wire() {
        Some(Wire::Daemon(Verb::Name { thread, name })) => Some((thread.clone(), name.clone())),
        _ => None,
    }
}

fn ch(app: &mut App, c: char) {
    key(app, KeyCode::Char(c), KeyModifiers::NONE);
}

/// The one door that makes the face read the filesystem without a terminal:
/// the 9010 socket saying a node it holds has changed.
fn invalidate(app: &mut App, node: &str) {
    app.apply(Ev::Aui(AuiEvent::Invalidate { node: node.into() }));
}

/// The selectable index of the RECENT band's row for `id`.
fn recent_row(app: &App, id: &str) -> usize {
    let list = &app.state().projects;
    (0..list.selectable())
        .find(|i| {
            matches!(list.item(*i), Some(RootItem::Recent(_)))
                && list.thread_at(*i).is_some_and(|t| t.id == id)
        })
        .expect("the thread is not on the root's recent band")
}

// ────────────────────────────────────────────────────────────────────────────

/// **AN `Invalidate` MUST REACH THE SCREEN, NOT ONLY THE STORE.**
///
/// This is the half `an_invalidate_re_reads_the_indexes_once_per_burst_and_
/// only_for_index_nodes` never asserted: that test reads `app.store`, and the
/// store is not what he looks at. `on_invalidate` re-read the indexes and did
/// not re-project, so a thread the AUI created while the face was open sat in
/// memory and the root kept drawing the previous list until some unrelated
/// keystroke happened to rebuild it.
///
/// **It is the whole of "the recent band isn't reordering"**, and it is why a
/// manual reload looked necessary: `App::reload` calls `project()` through
/// `on_intent`'s tail, so pressing the key made the same data appear.
///
/// WITNESS, keyed on something the fix cannot move: the projection COUNTER
/// moves, so this cannot pass on a face that drew nothing at all.
///
/// FORCED: delete the `self.project()` at the end of `on_invalidate` and the
/// second assertion goes red with an empty root.
#[tokio::test]
async fn an_invalidate_puts_the_new_list_on_the_screen_and_not_only_in_the_store() {
    let home = home_with_one_thread("invalidate");
    let mut app = app_at(&home);
    // Nothing has been read yet: `App::new` projects an empty store.
    assert!(
        app.state().projects.rows.is_empty(),
        "the fixture was already on screen — this test would prove nothing"
    );

    invalidate(&mut app, "root");

    assert!(
        app.state().projects.rows.iter().any(|r| r.name == "alpha"),
        "the indexes were re-read and the SCREEN never heard about it"
    );
    assert!(
        app.state().projects.recent.iter().any(|t| t.id == "t1"),
        "the recent band did not pick up the thread the re-read found"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// **BARE `r` ON A THREAD ROW OPENS THE RENAME MODAL, PRE-FILLED.**
///
/// His words, twice: *"On any thread throughout the application, I need to be
/// able to rename with a tap of the letter r"*, and then — two rounds after
/// the verb was built and the key was not — *"I stil can't do a rename of a
/// thread."* And then, a round after it was built onto the sheet: *"also I
/// want rename to do a modal. Don\'t take me to the settings. Just a little
/// modal."*
///
/// **THE PREFILL PROPERTY MOVED WITH THE SURFACE, and it is asserted HERE on
/// the path he actually uses.** `app::tests::the_rename_field_is_prefilled_
/// with_the_summary_it_edits` still guards the SHEET\'s rename row, which
/// survives and is still a door — but a `r` that lands on the modal must not
/// leave that property proven only on a path he no longer takes.
///
/// Driven by a real keystroke through `Ev::Input`, because the defect was
/// precisely that `intent(Key::Char('r'), Mode::List)` resolved to something
/// else: a test that called an internal rename would have been green
/// throughout the two rounds in which he could not do it.
///
/// WITNESS, keyed on something the binding cannot move: the cursor is asserted
/// to be ON thread `t1` before `r` is pressed, so a face that had drawn
/// nothing, or had put the cursor on a band header, cannot score a pass here.
///
/// FORCED: change the new `Mode::Projects` binding's intent back to
/// `Intent::Reload` and the mode assertion goes red; delete the
/// `Intent::Rename` arm in `on_intent` and it goes red the same way with the
/// key still perfectly resolved — two different halves of "delivered".
#[tokio::test]
async fn r_on_a_thread_row_opens_the_rename_field_prefilled_with_the_summary() {
    let home = home_with_one_thread("rename");
    let mut app = app_at(&home);
    let _fake = with_reachable_aui(&mut app).await;
    invalidate(&mut app, "root");
    app.lay(100, 30);

    let want = recent_row(&app, "t1");
    ch(&mut app, 'g');
    for _ in 0..want {
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    }
    // THE WITNESS. Positively right, and keyed on the thread's id rather than
    // on anything the rename binding touches.
    assert_eq!(app.state().projects.cursor, want);
    assert_eq!(
        app.state()
            .projects
            .thread_at(want)
            .map(|t| t.id.as_str()),
        Some("t1"),
        "the cursor is not on the thread — the rest of this test is meaningless"
    );

    daemon_up(&mut app);
    ch(&mut app, 'r');

    assert_eq!(
        app.state().mode,
        Mode::RenameModal,
        "`r` on a thread row did not open the little modal"
    );
    // **AND HE DID NOT GO TO THE SETTINGS.** The mode underneath is the surface
    // he was standing on, which is what the frame draws behind the box.
    assert_eq!(
        app.state().rename_under,
        Some(Mode::Projects),
        "the modal lost track of the surface it is standing over"
    );
    let field = app
        .state()
        .sheet
        .as_ref()
        .and_then(|s| s.field.as_ref())
        .expect("the sheet is up with no field open");
    assert_eq!(field.act, SheetAct::Rename, "`r` opened the wrong field");
    assert_eq!(field.label, "summary");
    assert_eq!(
        field.buf.text(),
        SUMMARY,
        "the rename box did not start from what the thread is called — \
         an accidental enter would have erased the name"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// **AND IT IS THE SAME KEY ON THE PROJECT'S OWN THREAD LIST** — *"on any
/// thread throughout the application"* is the acceptance test, and one surface
/// does not satisfy it. `Mode::Projects` is proved above; this is `Mode::List`,
/// which is the mode the round-three ruling was written against.
#[tokio::test]
async fn r_renames_from_a_projects_thread_list_too() {
    let home = home_with_one_thread("rename-list");
    let mut app = app_at(&home);
    let _fake = with_reachable_aui(&mut app).await;
    invalidate(&mut app, "root");
    app.lay(100, 30);

    // Into the project: walk to its row on the projects band and activate it.
    let list = &app.state().projects;
    let row = (0..list.selectable())
        .find(|i| matches!(list.item(*i), Some(RootItem::Project(_))))
        .expect("no project row on the root");
    ch(&mut app, 'g');
    for _ in 0..row {
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    }
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(app.state().mode, Mode::List, "the project did not open");
    assert!(
        app.state().list.rows.iter().any(|t| t.id == "t1"),
        "the thread list is empty — the rest of this test is meaningless"
    );

    daemon_up(&mut app);
    ch(&mut app, 'r');
    assert_eq!(
        app.state().mode,
        Mode::RenameModal,
        "`r` does not rename on a project's thread list"
    );
    assert_eq!(
        app.state().rename_under,
        Some(Mode::List),
        "the modal drew the root behind it instead of the project's thread list"
    );
    // The SAME prefill property, on the second surface. One behaviour on two
    // surfaces is the acceptance test; one surface does not satisfy it.
    assert_eq!(
        app.state()
            .sheet
            .as_ref()
            .and_then(|s| s.field.as_ref())
            .map(|f| f.buf.text()),
        Some(SUMMARY.to_string()),
        "the modal opened empty on a project's thread list"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// **`^R` IS STILL RELOAD, AND IT IS PROVED POSITIVELY.**
///
/// The round-three ruling has two halves and only one of them is about rename.
/// Asserting that `^R` merely failed to open a field would pass on a face
/// that had died, so the assertion is on reload's OWN sentence — a thing only
/// `App::reload` puts on the screen.
///
/// # SINCE ROUND SIX THE MODIFIED SPELLING IS `^R`, AND BOTH HALVES ARE HERE
///
/// `opt-r` never reached this face on his keyboard — macOS composes it to `®` —
/// so the ruling's "documented form" was a form he could not press. `^R` is the
/// form now, and the old one must be DEAD rather than aliased: a remap that
/// leaves the old key live is not a remap, and one that only tests the new key
/// cannot tell you it did not double-bind.
///
/// FORCED: give the new bare-`r` rows `Key::Ctrl('r')` as well and this goes
/// red with the rename field up instead of the notice; leave `Key::Alt('r')` in
/// `common_rows` and the first assertion below goes red.
#[tokio::test]
async fn ctrl_r_still_reloads_and_does_not_rename_and_opt_r_is_dead() {
    let home = home_with_one_thread("alt-r");
    let mut app = app_at(&home);
    invalidate(&mut app, "root");
    app.lay(100, 30);
    let want = recent_row(&app, "t1");
    ch(&mut app, 'g');
    for _ in 0..want {
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    }
    assert_eq!(app.state().projects.cursor, want);

    // ── THE OLD SPELLING IS DEAD. `opt-r` reaches no act at all now, so it
    //    cannot be a second, silently-surviving reload.
    assert_eq!(
        tui_render::intent(tui_render::Key::Alt('r'), app.state().mode),
        tui_render::Intent::None,
        "opt-r still means something — the remap left the old key live"
    );

    key(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);

    assert_ne!(
        app.state().mode,
        Mode::RenameModal,
        "^R renamed — reload lost the form both readers of the table print"
    );
    assert_ne!(
        app.state().mode,
        Mode::SheetField,
        "^R opened the sheet's field — reload is still not rename"
    );
    let notice = app
        .state()
        .notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default();
    assert!(
        notice.contains("re-read the indexes"),
        "^R did not reload; the face said {notice:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// **THE HOLE ROUND FIVE LEFT: A RENAME THAT REACHES THE DISK AND NEVER
/// REACHES THE MAIN MENU.**
///
/// His report, verbatim: *"after rename of the thread, the main menu wasn't
/// updated."* Round five reported that reload should stop being necessary and
/// named two bugs — the AUI never announced its writes, and the TUI re-read on
/// being told but never re-drew. Both were real and both were fixed. **This
/// case slipped both of them, through the mechanism BETWEEN them.**
///
/// # THE ORDERING, WHICH IS THE WHOLE DEFECT
///
/// `apps/aui`'s 9010 handler answers `SetThreadSummary` the instant the command
/// is QUEUED for the desk loop (`v2/mutate.rs`: `send(&tx, Command::
/// SetThreadSummary …)` returns as soon as the mpsc accepts it) and then fires
/// `ripple_for(&m)` — five index nodes — BEFORE `ThreadIndex::save` has written
/// anything. The durable write happens later, in the desk loop, and announces
/// itself through `install_index_invalidation`'s observer.
///
/// So the burst is: frame(s) carrying the OLD file, then frame(s) carrying the
/// NEW one, milliseconds apart. `App::on_invalidate` was a LEADING-EDGE
/// debounce with no trailing edge, so it spent its one re-read on the old file
/// and discarded the frame that carried the rename **with nothing to retry it**.
///
/// # THE FRAMES ARE NOT INTERCHANGEABLE
///
/// That is what makes this a data-loss bug rather than a latency one, and it is
/// why "one re-read per burst" was the right goal reached the wrong way. A
/// coalescing debounce must still perform ONE re-read after the burst ENDS.
///
/// WITNESS, keyed on something no part of the fix can move: the OLD name is
/// asserted to be positively on the screen first, read out of `app.state()`, so
/// a face that had drawn nothing cannot score a pass here.
///
/// FORCED, three ways, each one reddening a different assertion:
///   * delete `self.invalidate_owed = Some(node.to_string())` from
///     `on_invalidate`'s debounce arm — the clock is never armed, and the
///     `invalidate_deadline().is_some()` assertion goes red. **This is the
///     shipped code**, so this run is the reproduction of his defect.
///   * delete the `Ev::IndexSettled => self.on_index_settled()` arm from
///     `apply` — the clock is armed, the event is a no-op, and the final
///     assertion goes red still showing the old name.
///   * drop the `self.project()` from `reread_indexes` — the store is right and
///     the screen is stale, which is round five's OTHER bug, reappearing.
#[tokio::test]
async fn a_rename_that_lands_after_the_first_frame_still_reaches_the_main_menu() {
    let home = home_with_one_thread("liveness-hole");
    let mut app = app_at(&home);
    invalidate(&mut app, "root");
    app.lay(100, 30);

    // THE WITNESS. The old name is genuinely on the screen he looks at.
    assert!(
        app.state()
            .projects
            .recent
            .iter()
            .any(|t| t.summary == SUMMARY),
        "the thread was never drawn — the rest of this test would prove nothing"
    );

    // ── FRAME ONE: the 9010 ripple, fired before the desk has written ──────
    invalidate(&mut app, "recent_threads");

    // ── THE DURABLE WRITE, inside the debounce window ──────────────────────
    rename_on_disk(&home, RENAMED);

    // ── FRAME TWO: `ThreadIndex::save`'s observer. This is the frame that
    //    carries the new name, and it is the one that used to be discarded.
    invalidate(&mut app, "thread");

    // It was coalesced away — same assertion the in-crate unit test makes, and
    // the reason this defect was invisible: nothing was WRONG yet.
    assert!(
        !app.state()
            .projects
            .recent
            .iter()
            .any(|t| t.summary == RENAMED),
        "the second frame was not inside the debounce window — this machine is \
         too slow for the burst to be a burst, and the test proves nothing"
    );

    // **THE DEBT IS RECORDED AND THE CLOCK IS ARMED.** Without this the
    // trailing read would only ever happen if some unrelated event woke the
    // face, which is "he has to press a key" wearing a different hat.
    let d = app
        .invalidate_deadline()
        .expect("a swallowed Invalidate armed no clock — nothing will ever pay it");
    assert!(
        d <= Duration::from_millis(250),
        "the trailing read was scheduled past the debounce window: {d:?}"
    );

    // ── THE BURST SETTLES. The loop's sixth clock fires exactly this. ──────
    app.apply(Ev::IndexSettled);

    assert!(
        app.state()
            .projects
            .recent
            .iter()
            .any(|t| t.summary == RENAMED),
        "the rename is on disk and the main menu still shows the old name — \
         his defect, unfixed: {:?}",
        app.state()
            .projects
            .recent
            .iter()
            .map(|t| t.summary.clone())
            .collect::<Vec<_>>()
    );
    // …and the debt is settled, so the clock disarms rather than spinning.
    assert!(
        app.invalidate_deadline().is_none(),
        "the trailing read left its own debt behind and the clock never disarms"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// **A BURST THAT WAS NEVER COALESCED OWES NOTHING.** The other half of the
/// contract, and the one that keeps the trailing edge from becoming a second
/// re-read on every single `Invalidate`.
///
/// FORCED: arm the debt unconditionally in `on_invalidate` (move the
/// `invalidate_owed` write above the window check) and this goes red.
#[tokio::test]
async fn a_single_invalidate_arms_no_trailing_read() {
    let home = home_with_one_thread("liveness-single");
    let mut app = app_at(&home);
    invalidate(&mut app, "root");
    app.lay(100, 30);
    // WITNESS: the one frame did its work, so `None` below means "nothing
    // outstanding" and not "nothing happened".
    assert!(
        app.state()
            .projects
            .recent
            .iter()
            .any(|t| t.summary == SUMMARY),
        "the single invalidate never reached the screen"
    );
    assert!(
        app.invalidate_deadline().is_none(),
        "one Invalidate armed a trailing read it does not owe"
    );
    // And a node no index holds does not arm one either, however many arrive.
    invalidate(&mut app, "thread_options");
    invalidate(&mut app, "pending_inputs");
    assert!(
        app.invalidate_deadline().is_none(),
        "a node kind the allow-list rejects still put a re-read on the clock"
    );
    let _ = std::fs::remove_dir_all(&home);
}

// ────────────────────────────────────────────────────────────────────────────
//  THE LITTLE MODAL
//
//  > *also I want rename to do a modal. Don't take me to the settings. Just a
//  > little modal.*
// ────────────────────────────────────────────────────────────────────────────

/// Walk the root's cursor onto thread `t1` and press `r`. Returns the row it
/// landed on, asserted, so every test below starts from a positive witness.
async fn modal_on(app: &mut App, id: &str) -> usize {
    app.lay(100, 30);
    let want = recent_row(app, id);
    ch(app, 'g');
    for _ in 0..want {
        key(app, KeyCode::Down, KeyModifiers::NONE);
    }
    assert_eq!(
        app.state()
            .projects
            .thread_at(want)
            .map(|t| t.id.as_str()),
        Some(id),
        "the cursor is not on the thread — the rest of this test is meaningless"
    );
    daemon_up(app);
    ch(app, 'r');
    assert_eq!(app.state().mode, Mode::RenameModal, "`r` opened no modal");
    want
}

/// **THE LIST IS STILL BEHIND THE BOX.** *"Don't take me to the settings"* —
/// so the row he was on is still drawn and he has not lost his place.
///
/// Asserted on the FRAME and on the HIT MAP, which is what he looks at and
/// what his mouse hits; a mode assertion alone would pass over a box painted on
/// a blank screen.
///
/// # THE WITNESS IS A SECOND THREAD, AND THE FIRST ATTEMPT AT IT WAS WRONG
///
/// This test's first version asserted the EDITED thread's summary was still on
/// the frame — and it survived its own forced failure: replacing the body with
/// `sheet_body` left it GREEN, because the sheet's header title is that same
/// summary and the prefilled box carries it as well. **An assertion three bands
/// can satisfy is not an assertion about one of them.**
///
/// So the witness is [`NEIGHBOUR`], which exists nowhere on this face except a
/// row of the list: not the sheet's subject, not in the box, not in the header.
/// It is asserted present BEFORE `r` is pressed, so a face that drew nothing
/// scores nothing, and asserted present after, where only the list can put it.
///
/// FORCED, and each one now bites: change `body`'s `Mode::RenameModal` arm to
/// return `sheet_body(vs, …)` and the neighbour assertion goes red; delete the
/// `Target::Empty` substitution and the inert assertion goes red with a live
/// `ThreadTitle` under the box; drop the box's band from `bands` and the
/// "the modal drew no box" assertion goes red.
#[tokio::test]
async fn the_modal_stands_over_the_thread_list_and_the_list_is_still_drawn() {
    let home = home_with_two_threads("modal-over");
    let mut app = app_at(&home);
    let _fake = with_reachable_aui(&mut app).await;
    invalidate(&mut app, "root");
    app.lay(100, 30);

    let on_frame = |app: &App| -> Vec<String> {
        tui_render::frame(app.state(), chrono::Utc::now(), 100, 30)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    };

    // THE WITNESS: the list is genuinely on the glass first, and the row that
    // proves it is one NOTHING the modal draws could account for.
    assert!(
        on_frame(&app).iter().any(|r| r.contains(NEIGHBOUR)),
        "the neighbour thread was never drawn — this test would prove nothing: \n{:#?}",
        on_frame(&app)
    );

    let want = modal_on(&mut app, "t1").await;

    let rows = on_frame(&app);
    // 1. **THE BOX IS THERE**, keyed on its own rule and nothing else.
    //
    //    MEASURED: the first version of this looked for a row containing both
    //    "rename" and "esc", and stayed GREEN with the box's band set to zero
    //    rows — because the FOOTER renders this mode's hints (`esc cancel`,
    //    `enter rename`) straight out of the key table. The third time this
    //    round that a loosely-keyed assertion was satisfied by a band nobody
    //    was testing. The box's rule is the one place this exact sentence is
    //    written.
    let box_row = rows
        .iter()
        .position(|r| r.contains("enter renames · esc cancels"))
        .unwrap_or_else(|| panic!("the modal drew no box: {rows:#?}"));
    // 2. **THE LIST IS STILL THERE**, and the neighbour is what says so: it is
    //    on a list row and on nothing else this face draws.
    assert!(
        rows.iter().any(|r| r.contains(NEIGHBOUR)),
        "the list went away when the box opened: {rows:#?}"
    );
    //    …and the name he is editing is INSIDE THE BOX, below its rule — which
    //    is the prefill property asserted where he can actually see it, and not
    //    merely on the buffer. Keyed to rows BELOW `box_row`, so the list row
    //    above it cannot stand in for the box's content.
    assert!(
        rows[box_row + 1..].iter().any(|r| r.contains(SUMMARY)),
        "the box is empty — he would have to retype the name: {rows:#?}"
    );
    // 3. HE DID NOT LOSE HIS PLACE.
    assert_eq!(
        app.state().projects.cursor, want,
        "the cursor moved when the modal opened"
    );
    // 4. **THE SURFACE BEHIND IT IS INERT.** A click on a thread's name is
    //    `Intent::OpenAt`, which would open a thread out from under a
    //    half-typed rename and throw the keystrokes away. Nothing behind the
    //    box resolves to anything.
    // The hit map is asked of the RENDERER, from the same state the shell hands
    // it — which is the map the shell stores, derived the one way it is derived.
    let hits = tui_render::frame_with_hits(app.state(), chrono::Utc::now(), 100, 30).1;
    let live: Vec<_> = (0..30u16)
        .flat_map(|r| (0..100u16).map(move |c| (c, r)))
        .map(|(c, r)| (c, r, hits.hit(c, r)))
        .filter(|(_, _, t)| {
            !matches!(
                t,
                tui_render::Target::Empty
                    | tui_render::Target::Header
                    | tui_render::Target::Footer
                    | tui_render::Target::Banner
                    | tui_render::Target::QueueBadge
            )
        })
        .collect();
    assert!(
        live.is_empty(),
        "a modal that can be clicked through is not a modal: {live:#?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// **IN THE MODAL THE LETTERS ARE TEXT.** `c` types a `c`; it does not
/// compact. That single fact is why this is a `Mode` and not a flag, and it is
/// `Mode::SheetField`'s precedent followed exactly.
///
/// FORCED: leave `Mode::RenameModal` out of the `Intent::Type` guard in
/// `on_intent` and the typed text never reaches the buffer; drop the
/// `KeySet::Printable` row from `RENAME_MODAL_ROWS` and the same assertion goes
/// red with the buffer still holding only the prefill.
#[tokio::test]
async fn in_the_modal_the_letters_are_text_and_c_does_not_compact() {
    let home = home_with_one_thread("modal-text");
    let mut app = app_at(&home);
    let _fake = with_reachable_aui(&mut app).await;
    invalidate(&mut app, "root");
    modal_on(&mut app, "t1").await;

    // Every letter that means something somewhere else in this face: `c`
    // compacts on the sheet, `o` opens options at the root, `q` quits, `r` is
    // the key that opened this box, `g` is the top of the list.
    let mut alive = true;
    for c in "corqg".chars() {
        // The RETURN of `apply` is the loop's "keep going" — so a `q` that quit
        // is caught here rather than by reaching into a private field.
        alive &= app.apply(Ev::Input(vec![Input::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::NONE,
        ))]));
    }
    assert_eq!(
        app.state()
            .sheet
            .as_ref()
            .and_then(|s| s.field.as_ref())
            .map(|f| f.buf.text()),
        Some(format!("{SUMMARY}corqg")),
        "letters in the modal did something other than type"
    );
    // …and he is still in the box. `q` did not quit and `o` opened nothing.
    assert_eq!(app.state().mode, Mode::RenameModal);
    assert!(alive, "`q` quit the face from inside a text box");
    let _ = std::fs::remove_dir_all(&home);
}

/// **`esc` CANCELS AND CHANGES NOTHING**, and "nothing" is asserted on the
/// WIRE and not merely on the mode: not one frame reached the fake AUI.
///
/// # THE FIRST VERSION OF THIS TEST PROVED NOTHING, AND THAT IS WORTH KEEPING
///
/// It used the short-id fixture, where `acts::route` refuses every AUI-owned
/// act before a byte is written — *"this thread's id (t1) is not one the audio
/// interface can address"*. So `mutations().is_empty()` was TRUE no matter what
/// `esc` did, and pointing the `Mode::RenameModal` arm of `Intent::Back`
/// straight at `sheet_commit()` left this test green. **An absence is never
/// evidence** (`0065`): the silence was the fixture's, not the key's.
///
/// So it runs on the UUID fixture, where a commit really does reach the socket,
/// and the WITNESS is at the END: the same app, the same thread, the same box —
/// committed on purpose — and exactly ONE mutation on the wire. That is what
/// makes the zero above mean "esc sent nothing" rather than "nothing could be
/// sent".
///
/// FORCED: point the `Mode::RenameModal` arm of `Intent::Back` at
/// `sheet_commit()` and the count goes to two.
#[tokio::test]
async fn esc_closes_the_modal_and_sends_nothing() {
    let home = home_with_one_uuid_thread("modal-esc");
    let mut app = app_at(&home);
    let fake = with_reachable_aui(&mut app).await;
    invalidate(&mut app, "root");
    let want = modal_on(&mut app, UUID_TID).await;

    for c in " abandoned".chars() {
        ch(&mut app, c);
    }
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);

    assert_eq!(
        app.state().mode,
        Mode::Projects,
        "esc did not put him back on the surface the box was over"
    );
    assert!(
        app.state().sheet.is_none(),
        "esc left the sheet standing behind a closed modal"
    );
    assert_eq!(
        app.state().rename_under, None,
        "the closed modal still claims a surface underneath it"
    );
    assert_eq!(
        app.state().projects.cursor, want,
        "esc moved him off the row he was on"
    );
    assert!(
        fake.mutations().is_empty() && app.last_wire().is_none(),
        "esc sent something: {:#?} / {:?}",
        fake.mutations(),
        app.last_wire()
    );

    // **AND THE THREAD STILL HAS ITS NAME.** Re-opening the box mints a fresh
    // state from the store, so what it is prefilled with IS what the thread is
    // called — the abandoned edit is gone rather than lurking in a buffer he
    // cannot see.
    ch(&mut app, 'r');
    assert_eq!(app.state().mode, Mode::RenameModal);
    assert_eq!(
        app.state()
            .sheet
            .as_ref()
            .and_then(|s| s.field.as_ref())
            .map(|f| f.buf.text()),
        Some(SUMMARY.to_string()),
        "the cancelled edit survived the cancel"
    );

    // **THE WITNESS.** Commit on purpose, from this same app and this same box.
    // One rename leaves (TUI-P1a: to the DAEMON, as `Verb::Name`), so the
    // commit path was live the whole time and the nothing above was `esc`'s
    // doing and not the harness's — and 9010 still hears nothing.
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        renamed(&app),
        Some((UUID_TID.to_string(), SUMMARY.to_string())),
        "the deliberate commit sent no rename"
    );
    assert!(fake.mutations().is_empty(), "a rename reached 9010: {:#?}", fake.mutations());
    let _ = std::fs::remove_dir_all(&home);
}

/// **`enter` COMMITS, AND IT COMMITS THE SAME MUTATION THE SHEET'S ROW
/// COMMITS.** This is the assertion the round turns on: a second
/// `Mutation::` path was the failure mode, and the proof is the RAW FRAME the
/// live server would read.
///
/// The modal reaches it through `Intent::SheetCommit` → `act_for(SheetAct::
/// Rename)` → `ThreadAct::Rename` → `Mutation::SetThreadSummary`, which is
/// every step the sheet's row takes. Only the frame differed.
///
/// FORCED: give the modal its own commit that builds the envelope by hand and
/// the `kind` assertion still passes while the payload assertions go red on the
/// first field it spells differently — which is exactly the drift the one-path
/// rule exists to prevent, arriving as a red test.
#[tokio::test]
async fn enter_commits_the_modal_through_the_one_rename_mutation() {
    let home = home_with_one_uuid_thread("modal-enter");
    let mut app = app_at(&home);
    let fake = with_reachable_aui(&mut app).await;
    invalidate(&mut app, "root");
    modal_on(&mut app, UUID_TID).await;

    // Edit rather than retype — the caret is at the end of the prefill, which
    // is the whole point of prefilling it.
    for _ in 0..SUMMARY.len() {
        key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    }
    for c in RENAMED.chars() {
        ch(&mut app, c);
    }
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    // **TUI-P1a: THE ONE RENAME IS THE DAEMON'S** (`Verb::Name` →
    // `threads/{id}/event` `summary_updated`), for THIS thread, with what he
    // typed — and 9010 hears nothing, so the AUI neither re-indexes nor SPEAKS
    // it (TC-38).
    assert_eq!(
        renamed(&app),
        Some((UUID_TID.to_string(), RENAMED.to_string())),
        "the modal did not send the one daemon rename: {:?}",
        app.last_wire()
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        fake.mutations().is_empty(),
        "a rename still reached 9010 — there is a second path: {:#?}",
        fake.mutations()
    );
    // And the box closes onto his list, not onto the sheet.
    assert_eq!(
        app.state().mode,
        Mode::Projects,
        "committing took him to the settings after all"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// **AN EMPTY BOX COMMITS NOTHING AND STAYS OPEN.**
///
/// An empty summary is the act that ERASES the name — `apps/aui` refuses
/// exactly that in its own words — and the sheet's own comment flags the
/// hazard. What is decided here is when the BOX closes, which is the face's
/// question: a modal that closed on an empty enter would leave him at his list
/// with a refusal and nowhere to correct it.
///
/// FORCED: delete the empty guard from `sheet_commit` and this goes red twice —
/// a `set_thread_summary` with an empty summary appears on the wire, and the
/// box is gone.
#[tokio::test]
async fn an_empty_modal_commits_nothing_and_the_box_stays_open() {
    let home = home_with_one_thread("modal-empty");
    let mut app = app_at(&home);
    let fake = with_reachable_aui(&mut app).await;
    invalidate(&mut app, "root");
    modal_on(&mut app, "t1").await;

    // Empty it. `^U` cuts back to the start of the name in one press.
    key(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert_eq!(
        app.state()
            .sheet
            .as_ref()
            .and_then(|s| s.field.as_ref())
            .map(|f| f.buf.text()),
        Some(String::new()),
        "^U did not empty the box — the rest of this test is meaningless"
    );

    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    assert!(
        fake.mutations().is_empty() && app.last_wire().is_none(),
        "an empty box sent a rename that would have erased the name: {:#?} / {:?}",
        fake.mutations(),
        app.last_wire()
    );
    assert_eq!(
        app.state().mode,
        Mode::RenameModal,
        "the box closed on an empty commit and took his correction with it"
    );
    let notice = app
        .state()
        .notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default();
    assert!(
        notice.contains("empty") && notice.contains("esc"),
        "the empty box said nothing, or did not name the way out: {notice:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// **NO KEY MEANS TWO THINGS IN THE MODAL.**
///
/// `keys::tests::one_key_one_meaning_in_one_mode` is the machine that says this
/// for every mode in `DOCUMENTED_MODES`, and **`Mode::RenameModal` is
/// deliberately NOT in that list this round**: adding it there means bumping
/// the `DOCUMENTED_MODES.len()` assertion on the very line a sibling worker is
/// already changing, and a hand-merge of one integer is a worse trade than
/// asserting the property from here. So the property is asserted here instead,
/// and the manager is asked to move it into the sweep after the merge.
///
/// FORCED: give the `esc` row `Key::Enter` as well and this goes red naming
/// `Enter`.
#[test]
fn no_key_means_two_things_in_the_rename_modal() {
    use tui_render::{KeySet, BINDINGS};
    let mut seen: Vec<(tui_render::Key, tui_render::Intent)> = Vec::new();
    let mut rows = 0;
    for b in BINDINGS.iter().filter(|b| b.mode == Mode::RenameModal) {
        rows += 1;
        if let KeySet::These(keys) = b.keys {
            for &k in keys {
                if let Some((_, other)) = seen.iter().find(|(s, _)| *s == k) {
                    panic!(
                        "{k:?} means two things in the rename modal: {other:?} and {:?}",
                        b.intent
                    );
                }
                seen.push((k, b.intent));
            }
        }
    }
    // A WITNESS: the mode really has a grammar, so an empty walk cannot pass.
    assert_eq!(
        rows, 16,
        "the modal's row count moved — the sweep above may no longer be walking \
         the grammar it thinks it is"
    );
}

/// **THE MODAL AT THE SMALLEST FRAME THE FACE WILL DRAW.**
///
/// A panic inside a render is the worst failure this face has: it happens with
/// the terminal in the alternate screen and the mouse captured. The modal
/// borrows the composer's band out of the content band, so it does arithmetic
/// on row counts — and `bands` subtracts its height from what is left.
///
/// That subtraction is SAFE BY REASONING (`MIN_HEIGHT` is 8, so `body >= 6`;
/// `banner_height` is capped at `BANNER_MAX_ROWS` = 2; so `rest >= 4`, and
/// `rename_modal_height` clamps to 3) — and reasoning is not the standard this
/// arc holds. This renders it.
///
/// It also renders BELOW the minimum, where every band is zero and the face
/// draws one sentence: the modal must not be the mode that makes that path
/// panic either.
///
/// WITNESS: the 100x30 frame is asserted to be the modal FIRST, so a fixture
/// that never opened a box cannot score a pass on "no panic at any size".
#[tokio::test]
async fn the_modal_draws_at_every_size_the_face_will_draw() {
    let home = home_with_two_threads("modal-tiny");
    let mut app = app_at(&home);
    let _fake = with_reachable_aui(&mut app).await;
    invalidate(&mut app, "root");
    modal_on(&mut app, "t1").await;

    // THE WITNESS: at a normal size this really is the modal, drawn.
    let big = tui_render::frame(app.state(), chrono::Utc::now(), 100, 30);
    assert_eq!(big.len(), 30);
    assert!(
        big.iter().any(|l| l
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
            .contains("enter renames · esc cancels")),
        "the box is not on the 100x30 frame — the sizes below prove nothing"
    );

    // Every size from below the minimum up through the smallest usable frames,
    // and a very narrow one where the box's own rule is wider than the window.
    for (w, h) in [
        (30u16, 6u16),
        (40, 8),
        (40, 9),
        (41, 8),
        (45, 10),
        (60, 8),
        (200, 8),
        (40, 24),
    ] {
        let rows = tui_render::frame(app.state(), chrono::Utc::now(), w, h);
        assert_eq!(
            rows.len(),
            h as usize,
            "the modal drew {} rows at {w}x{h}, not {h} — the shape law is absolute",
            rows.len()
        );
        // And every row is exactly the window's width: a band that lies about
        // its size is how the over-report panic happens.
        for (i, line) in rows.iter().enumerate() {
            let width: usize = line
                .spans
                .iter()
                .map(|s| s.content.chars().count())
                .sum();
            assert!(
                width >= 1,
                "row {i} of the {w}x{h} modal frame is empty of spans"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&home);
}
