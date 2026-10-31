//! **THE BOUNDARY PROOF FOR `DIR-15`: the core behaviors of the TUI are
//! reachable from outside the binary.**
//!
//! An integration test lives outside the crate and can only name what the
//! crate makes public — which is exactly the property under test, so **this
//! file is a COMPILE-TIME assertion first and a runtime one second**. Against
//! `lib.rs` as it stood before `0037` it does not compile at all: the terminal
//! lifecycle, the event loop, the input path and the internal event vocabulary
//! were `mod` in `main.rs`, and `main.rs` is not reachable from here. That is
//! the forced failure this proof rests on, and it is reproduced by reverting
//! `pub mod app; pub mod event; pub mod input; pub mod term;` in `lib.rs` and
//! running `cargo test -p tui --test it lib_surface::`.
//!
//! **What it deliberately does NOT do.** It never calls `term::enter`,
//! `term::restore` or `input::spawn_pump`, and it issues no RPC. Those touch the real terminal and the real daemon, and a test with a
//! side effect on the developer's screen is the defect `App::on_intent` was
//! shaped to avoid. Reachability is proven by NAMING; behaviour is proven by
//! the unit tests inside each module and by running the face.

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

/// §2's list, one assertion per behavior. Each line is a core behavior of the
/// TUI being named from a place that is not the binary.
#[tokio::test]
async fn every_core_behavior_is_reachable_from_outside_the_binary() {
    // ── terminal lifecycle ──
    // The claim set is a `const` a reader can enumerate, and every claim is
    // paired with the key that gives it back.
    assert!(!tui::term::CLAIMS.is_empty());
    for claim in tui::term::CLAIMS {
        assert!(
            claim.release_key.is_some(),
            "unreleasable claim reached the public surface: {}",
            claim.what
        );
        assert!(
            claim.set.starts_with('\x1b'),
            "a claim is a DECSET sequence"
        );
    }
    // Nothing above CLAIMED anything: entering is an explicit call, which is
    // what makes a library that CAN own a terminal safe to link.
    let _enter: fn() -> anyhow::Result<tui::term::Tui> = tui::term::enter;
    let _restore: fn() = tui::term::restore;
    let _panic_hook: fn() = tui::term::install_panic_hook;

    // ── input: the key table's one translation, and the mouse ──
    assert_eq!(
        tui::input::to_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::CONTROL)),
        Some(tui_render::Key::Ctrl('c'))
    );
    let mouse = |kind, column, row| MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    let mut gesture = tui::input::Gesture::default();
    let notch = gesture.feed(mouse(MouseEventKind::ScrollUp, 0, 0));
    assert!(matches!(notch, Some(tui::event::Input::Wheel { up: true })));

    // **THE GESTURE CLASSIFIER, FROM OUTSIDE THE CRATE.** A click and a drag
    // are distinct gestures and neither can produce the other's event — the
    // operator's literal requirement, reachable and checkable by anything that
    // links this library.
    gesture.feed(mouse(MouseEventKind::Down(MouseButton::Left), 7, 3));
    let click = gesture.feed(mouse(MouseEventKind::Up(MouseButton::Left), 7, 3));
    assert!(matches!(
        click,
        Some(tui::event::Input::Click { col: 7, row: 3 })
    ));
    gesture.feed(mouse(MouseEventKind::Down(MouseButton::Left), 7, 3));
    gesture.feed(mouse(MouseEventKind::Drag(MouseButton::Left), 20, 9));
    let done = gesture.feed(mouse(MouseEventKind::Up(MouseButton::Left), 20, 9));
    assert!(matches!(
        done,
        Some(tui::event::Input::DragEnd { col: 20, row: 9 })
    ));
    let _pump: fn(tokio::sync::mpsc::Sender<tui::event::Ev>) = tui::input::spawn_pump;

    // ── the clipboard wrapper, as a pure function ──
    // The DCS passthrough is the only reason a copy reaches his terminal at
    // all under `set-clipboard external`, and its SHAPE is checkable without
    // owning a pty.
    let wrapped = tui::term::clipboard_bytes("hi", true);
    assert!(wrapped.starts_with("\x1bPtmux;") && wrapped.ends_with("\x1b\\"));
    assert_eq!(
        tui::term::clipboard_bytes("hi", false),
        "\x1b]52;c;aGk=\x07"
    );

    // ── THE PROJECT INDEXES: the list's source, reachable and read-only ──
    let _roots: fn(&str) -> Vec<(String, std::path::PathBuf)> = tui::index::discover_roots;
    // The roots, and HIS ARCHIVE SET ITSELF — the root's `archived (N)` row is
    // a DOOR onto that set now, so the label and the room read one value.
    // Named, because the two readers below are what a caller must be able to
    // spell — and clippy is right that a bare four-deep tuple in a signature
    // pin is not a spelling anybody would read.
    type Universe = (
        Vec<tui::index::Project>,
        Vec<tui::index::Archived>,
        tui::index::RegistryState,
    );
    type Roots = (Vec<(String, std::path::PathBuf)>, tui::index::RegistryState);
    let _load: fn(&str) -> Universe = tui::index::load_projects;
    let _archived: fn(&str) -> Vec<tui::index::Archived> = tui::index::load_archived;
    // **WHERE THE LIST CAME FROM RIDES BACK WITH IT.** The face reads
    // `~/.cauldron/projects.json` now and `~/minds` is a relic; a caller has
    // to be able to tell "the registry says he has none" from "the registry
    // would not parse", because only one of those is a fact about his
    // projects.
    let _roots_src: fn(&str) -> Roots = tui::index::registry_roots;
    // A root that does not exist degrades to nothing rather than erroring,
    // which is the same tolerance one bad index file gets — and an absent
    // archive set hides NOTHING, which is the one direction it must never fail
    // in.
    let (ps, arch, src) = tui::index::load_projects("/nonexistent-home-for-the-boundary-proof");
    // A home with no `~/.cauldron` at all is the COEXISTENCE WINDOW, not a
    // corrupt file: the crate's own derivation ran and found nothing.
    assert_eq!(src, tui::index::RegistryState::NotCreatedYet);
    // ABSENT, not CORRUPT — a root that does not exist has no index file, and
    // that is an honest zero rather than something we failed to read.
    assert!(ps.iter().all(|p| p.index == tui::index::IndexState::Absent));
    assert!(arch.is_empty(), "a missing archive set must hide nothing");

    // ── the internal event vocabulary ──
    let _resize = tui::event::Input::Resize;
    // **THE BATCH IS THE UNIT OF TERMINAL INPUT**, and it is reachable from
    // outside the binary — which is what lets the paste defence be proven by a
    // test rather than only by a pty.
    let _batch = tui::event::Ev::Input(vec![tui::event::Input::Resize]);
    let _closed = tui::event::Ev::InputClosed;

    // ── the key table and the intents it resolves to (the pure crate) ──
    assert!(!tui_render::BINDINGS.is_empty());
    assert_eq!(
        tui_render::intent(tui_render::Key::Char('q'), tui_render::Mode::List),
        tui_render::Intent::Quit
    );

    // ── the pure frame: state in, lines out, exact width ──
    let vs = tui_render::ViewState::default();
    let lines = tui_render::frame(&vs, chrono::Utc::now(), 100, 24);
    assert_eq!(
        lines.len(),
        24,
        "the frame is exactly as tall as it was told"
    );

    // ── the frame's HIT MAP, and the ONE activation both gestures go through
    //    — which since `0052` takes the GESTURE as well, because a click on a
    //    plain row SELECTS where `→` OPENS.
    let (lines, hits) = tui_render::frame_with_hits(&vs, chrono::Utc::now(), 100, 24);
    assert_eq!(hits.height(), lines.len());
    assert_eq!(hits.hit(0, 0), tui_render::Target::Header);
    assert_eq!(
        tui_render::activation(
            tui_render::Target::ThreadRow(0),
            tui_render::Mode::List,
            tui_render::Via::Key
        ),
        Some(tui_render::Intent::Open)
    );
    assert_eq!(
        tui_render::activation(
            tui_render::Target::ThreadRow(0),
            tui_render::Mode::List,
            tui_render::Via::Click
        ),
        Some(tui_render::Intent::Select(0))
    );

    // ── geometry: how the frame divides, asked for rather than re-derived ──
    let b = tui_render::bands(&vs, 100, 24);
    assert_eq!(b.header + b.banner + b.content + b.composer + b.footer, 24);

    // ── viewport and follow-mode semantics ──
    assert!(tui_render::Viewport::at_bottom().follow);

    // ── connection state as a rendered fact ──
    let mut store = tui::store::Store::new("ws://127.0.0.1:1/ws".into(), "/Users/x".into());
    assert!(matches!(
        tui::store::header(&store, tui_render::Mode::List)
            .link
            .state,
        tui_render::LinkState::Connecting { .. }
    ));
    store.invalidate("the boundary proof");

    // ── the wire ──
    //
    // `reads::spawn_fleet` stood here and it is GONE: `threads/list` and
    // `threads/status` are not asked for any more, so there is no fleet read to
    // name. What a second face gets instead is the SEAM and the face's half of
    // it, both named below, and that is a bigger surface than the one this line
    // replaced — a mirror, a read vocabulary and three verbs, rather than a
    // function that issued two RPCs.
    let _parse: fn(serde_json::Value) -> tui::proto::ThreadEvent = tui::proto::ThreadEvent::parse;
    let _window: fn(&tui::seam::held::Held) -> Vec<String> = tui::mirror::window;
    let _transcript: fn(
        tui::mirror::Handle,
        tokio::sync::mpsc::Sender<tui::event::Ev>,
        String,
    ) = tui::mirror::spawn_transcript;
    let _queue: fn(
        tui::mirror::Handle,
        tokio::sync::mpsc::Sender<tui::event::Ev>,
        String,
    ) = tui::queue_reads::spawn_queue;
    let _link: fn(&tui::seam::held::Held) -> &tui::seam::held::LinkState = tui::seam::view::link;

    // ── the event loop and its coordinator ──
    // Constructed, never run: `run` takes a live terminal. The seam is pointed
    // at a chassis port nothing listens on, exactly as `app`'s own tests do, so
    // no daemon is asked anything.
    // **`spawn_dead`, NEVER `spawn`.** The bare one reads its port out of the
    // environment and falls back to 3653, which is the operator's live daemon;
    // this points both halves of the lever at port 1, which nothing listens on.
    // See `mirror::TestPorts`.
    let (handle, _rx) = tui::mirror::spawn_dead();
    let (tx, _erx) = tokio::sync::mpsc::channel::<tui::event::Ev>(16);
    let _app = tui::app::App::new(
        handle,
        tx,
        "ws://127.0.0.1:1/ws".into(),
        "/Users/x".into(),
        0,
    );
    let _run = tui::app::run;
}
