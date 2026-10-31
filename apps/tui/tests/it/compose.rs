//! **THE COMPOSER'S SEND PATH, PROVEN AGAINST A SCRIPTED FAKE DAEMON.**
//!
//! # The boundary this file exists to respect
//!
//! `threads/{id}/send` puts REAL WORDS IN HIS REAL THREADS. It is the one mutation
//! this face issues and it is the one thing a test must never fire at his
//! daemon. So the whole send path is exercised here, against a fake on an
//! ephemeral loopback port that this process opened itself — the same harness
//! `0031` built for the transport, which "is emphatically not the real daemon
//! and never talks to one".
//!
//! **What that buys is the thing the task asks for: the WIRE FRAME is proven
//! correct without one being fired.** The assertions are on the request the
//! daemon SAW — method, `thread_id`, `text`, `intent` — which is exactly what
//! would have reached the real one.
//!
//! # And the security-shaped half
//!
//! `a_paste_carrying_its_own_terminator_sends_nothing_at_all` is the app-level
//! statement of the defect reproduced on a pty: a clipboard containing
//! `ESC[201~` delivers its tail as keystrokes including an Enter, and under
//! "enter sends" that dispatches a message he did not write. `input::defuse`
//! proves the batch rule as a pure function; this proves the CONSEQUENCE —
//! that the daemon saw nothing.

use crate::fake;

use fake::{Conn, Fake};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tui::app::App;
use tui::event::{Ev, Input};
use tui::mirror;

const W: u16 = 100;
const H: u16 = 30;

fn key(code: KeyCode) -> Input {
    Input::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// **`ctrl-<letter>`, which is what every global answers to since round six.**
///
/// It replaced the `alt(c)` helper wholesale rather than sitting beside it:
/// macOS composes `opt-<letter>` into a glyph before any escape sequence exists
/// (`opt-q` is `œ`, `opt-m` is `µ`), so an `alt(c)` here would have been driving
/// a keystroke his keyboard cannot produce. `mic.rs`'s wire test keeps the
/// composed-glyph witness.
fn ctrl(c: char) -> Input {
    Input::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

fn typed(s: &str) -> Vec<Input> {
    s.chars().map(|c| key(KeyCode::Char(c))).collect()
}

/// One batch per keystroke, which is what a human produces: separate reads.
/// Feeding a whole sentence as ONE batch would trip the paste backstop, which
/// is correct behaviour and not what these cases are about.
fn type_into(app: &mut App, s: &str) {
    for i in typed(s) {
        app.apply(Ev::Input(vec![i]));
    }
}

/// A face wired to a fake daemon, with a thread open and one frame laid out so
/// the composer knows how wide it is.
async fn face(script: Conn) -> (Fake, App, mpsc::Receiver<Ev>) {
    let f = Fake::start(vec![script]).await;
    // **BOTH HALVES OF THE LEVER, SET TOGETHER, UNDER A PROCESS-WIDE LOCK.**
    // `_ports` must stay bound for the life of this seam: dropping it releases
    // the lock that stops another test in this binary pointing the process at
    // its own fake. See `mirror::TestPorts` for the bill an agent already paid
    // for setting one half and forgetting the other.
    let (handle, mut trx, _ports) = mirror::spawn_at(f.port);
    let (tx, rx) = mpsc::channel::<Ev>(64);
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = trx.recv().await {
                if tx.send(Ev::Transport(ev)).await.is_err() {
                    return;
                }
            }
        });
    }
    f.until_subscribed(1).await;
    // The handle reports live only once the socket is up; the send path refuses
    // loudly on a dead link, so this is a precondition and not a politeness.
    for _ in 0..200 {
        if handle.is_live() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(handle.is_live(), "the fake never came up");
    let mut app = App::new(handle, tx, f.url.clone(), "/Users/x".into(), 0);
    app.open("t1");
    app.lay(W, H);
    (f, app, rx)
}

/// Pump events until the fake has logged `n` requests, applying whatever comes
/// back so the app's state moves with it.
async fn settle(f: &Fake, app: &mut App, rx: &mut mpsc::Receiver<Ev>) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
            Ok(Some(ev)) => {
                let sent = matches!(ev, Ev::Sent { .. });
                app.apply(ev);
                if sent {
                    return;
                }
            }
            Ok(None) => return,
            Err(_) => {}
        }
    }
    panic!("nothing came back; the fake saw {:#?}", f.log());
}

/// The THREAD each send named — read off the method PATH, because that is
/// where the subject lives now. `params["thread_id"]` is not merely absent
/// here: sending it would be REFUSED by the daemon and by `tests/fake`.
fn send_subjects(f: &Fake) -> Vec<String> {
    f.log()
        .into_iter()
        .filter(|r| r.leaf() == "send")
        .map(|r| r.subject().unwrap_or_default().to_string())
        .collect()
}

fn sends(f: &Fake) -> Vec<Value> {
    f.log()
        .into_iter()
        .filter(|r| r.leaf() == "send")
        .map(|r| r.params)
        .collect()
}

fn accepting() -> Conn {
    // Keyed by the LEAF, so one script answers a send on whichever thread
    // the face is looking at. The daemon's reply shape did NOT change with the
    // re-pathing — `threads/{id}/send` still answers `accepted`.
    Conn::default().respond(
        "send",
        json!({"accepted": true, "thread_id": "t1", "message_id": 1311, "header_warnings": []}),
    )
}

/// **THE WIRE FRAME, PROVEN WITHOUT FIRING ONE AT HIM.**
#[tokio::test]
async fn enter_sends_exactly_one_message_with_the_frame_the_daemon_expects() {
    let (f, mut app, mut rx) = face(accepting()).await;
    type_into(&mut app, "hello there");
    assert!(sends(&f).is_empty(), "typing sent something");

    app.apply(Ev::Input(vec![key(KeyCode::Enter)]));
    settle(&f, &mut app, &mut rx).await;

    let s = sends(&f);
    assert_eq!(s.len(), 1, "expected exactly one send, got {s:#?}");
    assert_eq!(send_subjects(&f), vec!["t1".to_string()]);
    assert!(
        s[0].get("thread_id").is_none(),
        "the subject rides the PATH; spelling it in the params too is refused, \
         not reconciled: {:#?}",
        s[0]
    );
    assert_eq!(s[0]["text"], "hello there");
    // `immediate` is the daemon's default and the AUI's choice: deliver as soon
    // as no parent turn is in flight, never interrupting one mid-flight.
    assert_eq!(s[0]["intent"], "immediate");
    assert_eq!(s[0]["metadata"]["tui"]["client"], "harness-tui");
    assert_eq!(s[0]["metadata"]["tui"]["queued_parts"], 0);
    // …and the composer is empty ONLY because the daemon accepted it.
    assert!(
        app.state().composer.buf.is_blank(),
        "the composer kept the sent text"
    );
    assert!(!app.state().composer.sending);
}

/// **QUEUE AND SEND ARE TWO ACTS, and the daemon sees exactly one of them.**
///
/// His grammar: *"in the aui we use left to queue, and wildcard to send the
/// whole payload."* `^Q` is the left and nothing goes out; `enter` is the
/// wildcard and everything goes out at once, stitched the way the AUI stitches
/// it.
#[tokio::test]
async fn queue_reaches_no_daemon_and_the_send_carries_the_whole_payload() {
    let (f, mut app, mut rx) = face(accepting()).await;

    type_into(&mut app, "first");
    app.apply(Ev::Input(vec![ctrl('q')]));
    assert!(
        sends(&f).is_empty(),
        "^Q reached the daemon — it must not"
    );
    assert_eq!(app.state().composer.buf.pending().len(), 1);
    assert!(
        app.state().composer.buf.is_blank(),
        "queueing did not empty the box"
    );

    type_into(&mut app, "second");
    app.apply(Ev::Input(vec![ctrl('q')]));
    assert!(sends(&f).is_empty());
    assert_eq!(app.state().composer.buf.pending().len(), 2);

    type_into(&mut app, "third");
    app.apply(Ev::Input(vec![key(KeyCode::Enter)]));
    settle(&f, &mut app, &mut rx).await;

    let s = sends(&f);
    assert_eq!(s.len(), 1, "the whole payload must be ONE message: {s:#?}");
    let text = s[0]["text"].as_str().unwrap();
    assert!(text.starts_with("Queued messages:"), "{text}");
    assert!(text.contains("Message 1:\nfirst"), "{text}");
    assert!(text.contains("Message 2:\nsecond"), "{text}");
    assert!(text.contains("Message 3:\nthird"), "{text}");
    assert_eq!(s[0]["metadata"]["tui"]["queued_parts"], 2);
    assert!(
        app.state().composer.buf.pending().is_empty(),
        "the queue survived the send"
    );
}

/// **THE SECURITY-SHAPED ONE: A PASTE CANNOT SEND.**
///
/// The exact event sequence a pty probe produced from
/// `\x1b[200~hello\x1b[201~\x1b[201~ rm -rf\r`: a `Paste`, then the tail as
/// ordinary key presses, then ENTER. The daemon must see NOTHING, and his
/// clipboard must be in the box in full.
#[tokio::test]
async fn a_paste_carrying_its_own_terminator_sends_nothing_at_all() {
    let (f, mut app, mut rx) = face(accepting()).await;
    let mut batch = vec![Input::Paste("hello".into())];
    batch.extend(typed(" rm -rf"));
    batch.push(key(KeyCode::Enter));
    app.apply(Ev::Input(batch));

    // Give anything that WOULD have gone out a chance to arrive.
    for _ in 0..20 {
        if tokio::time::timeout(std::time::Duration::from_millis(10), rx.recv())
            .await
            .is_err()
        {
            break;
        }
    }
    assert!(
        sends(&f).is_empty(),
        "A PASTE SENT A MESSAGE. That is his words going out half-finished: {:#?}",
        f.log()
    );
    assert_eq!(
        app.state().composer.buf.text(),
        "hello rm -rf\n",
        "the clipboard's tail was lost rather than kept as text"
    );
    // …and a real `enter` afterwards still works, which is what makes the rule
    // a defence rather than a lock.
    app.apply(Ev::Input(vec![key(KeyCode::Enter)]));
    settle(&f, &mut app, &mut rx).await;
    assert_eq!(sends(&f).len(), 1);
}

/// **A SECOND `enter` WHILE ONE IS IN FLIGHT SENDS NOTHING.** The round trip is
/// long enough for two presses to fit inside it, and a second send is his words
/// arriving twice.
#[tokio::test]
async fn a_send_in_flight_refuses_a_second_one() {
    // `no_suback` leaves the socket connected and NOT usable, so a request sits
    // in the outbound channel — which is exactly the window a double press
    // falls into. Nothing ever answers, so the guard is held for the whole test.
    let f = Fake::start(vec![Conn::default().no_suback()]).await;
    let (handle, _trx, _ports) = mirror::spawn_at(f.port);
    let (tx, _rx) = mpsc::channel::<Ev>(64);
    f.until_subscribed(1).await;
    for _ in 0..200 {
        if handle.is_live() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let mut app = App::new(handle, tx, f.url.clone(), "/Users/x".into(), 0);
    app.open("t1");
    app.lay(W, H);
    type_into(&mut app, "once");
    app.apply(Ev::Input(vec![key(KeyCode::Enter)]));
    assert!(
        app.state().composer.sending,
        "the first send did not take the guard"
    );
    app.apply(Ev::Input(vec![key(KeyCode::Enter)]));
    app.apply(Ev::Input(vec![key(KeyCode::Enter)]));
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        sends(&f).len() <= 1,
        "a second enter fired a second send: {:#?}",
        f.log()
    );
    assert!(
        app.state()
            .notice
            .as_ref()
            .is_some_and(|t| t.text.contains("still sending")),
        "the refusal was silent — a key that declines without saying so is a key he presses again"
    );
}

/// **A REFUSED SEND KEEPS HIS WORDS.** The composer empties on an ACCEPTANCE
/// and on nothing else; a face that cleared the box optimistically and then
/// failed is a face that ate a message.
#[tokio::test]
async fn a_refused_send_leaves_the_text_in_the_box() {
    // The daemon answers, and answers WITHOUT accepting — a shape we did not
    // expect must not be allowed to look like a delivered message.
    let (f, mut app, mut rx) =
        face(Conn::default().respond("send", json!({"ok": "sure"}))).await;
    type_into(&mut app, "please keep this");
    app.apply(Ev::Input(vec![key(KeyCode::Enter)]));
    settle(&f, &mut app, &mut rx).await;
    assert_eq!(sends(&f).len(), 1, "the send never went out");
    assert_eq!(
        app.state().composer.buf.text(),
        "please keep this",
        "the composer was emptied by a response that did not accept anything"
    );
    assert!(app
        .state()
        .notice
        .as_ref()
        .is_some_and(|t| t.text.contains("not sent")));
    assert!(!app.state().composer.sending, "the guard was not released");
}

/// **A KEY REPEAT CANNOT FIRE A SECOND SEND.** Under the kitty protocol a key
/// held a fraction too long delivers `Repeat` events; each one reaching
/// `Intent::Send` would be another `threads/{id}/send`.
#[tokio::test]
async fn a_repeated_enter_sends_once() {
    let (f, mut app, mut rx) = face(accepting()).await;
    type_into(&mut app, "once only");
    let mut repeat = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    repeat.kind = KeyEventKind::Repeat;
    app.apply(Ev::Input(vec![
        key(KeyCode::Enter),
        Input::Key(repeat),
        Input::Key(repeat),
    ]));
    settle(&f, &mut app, &mut rx).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        sends(&f).len(),
        1,
        "a key repeat sent it again: {:#?}",
        f.log()
    );
}

/// Blank input is not a message. A stray space that fired a turn would be a
/// message he did not write, which is the same family as the paste defect.
#[tokio::test]
async fn whitespace_alone_is_never_sent() {
    let (f, mut app, mut rx) = face(accepting()).await;
    type_into(&mut app, "   ");
    app.apply(Ev::Input(vec![key(KeyCode::Enter)]));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(sends(&f).is_empty(), "whitespace was sent as a message");
    // And an `^Q` on it queues nothing either.
    app.apply(Ev::Input(vec![ctrl('q')]));
    assert!(app.state().composer.buf.pending().is_empty());
    let _ = &mut rx;
}
