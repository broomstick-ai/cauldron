//! **MIC MODE, PROVEN END TO END WITHOUT EVER OPENING THE MICROPHONE.**
//!
//! # The two boundaries this file respects
//!
//! 1. **`threads/{id}/send` puts real words in his real threads.** Every send here
//!    goes to the scripted fake daemon on an ephemeral loopback port that this
//!    process opened itself — the same harness `tests/it/compose.rs` uses. The
//!    assertions are on the request the daemon SAW, which is exactly what would
//!    have reached the real one.
//!
//! 2. **Acquiring the microphone reconfigures the audio route and invalidates
//!    the AUI's already-open output handle** (`apps/aui/src/dispatch/
//!    thread.rs:3875-3884`), and the operator is on that live audio interface.
//!    So the capture, transcription and playback commands are SUBSTITUTED at
//!    their process boundary and the pipeline is driven with real files.
//!
//! **What that is, stated plainly so nobody mistakes it for something else: it
//! is dependency injection at a process boundary, not a stub.** Nothing here
//! invents a transcript or synthesises audio; the WAVs are real WAVs, written
//! to disk by this file, and the code under test reads them with the same
//! parser it uses in production. With nothing injected, mic mode runs `rec`,
//! `retranscribe` and `afplay`.
//!
//! **THE LIVE-DEVICE LEG IS NOT PROVEN HERE AND IS NOT CLAIMED TO BE.** That
//! `rec` can actually open the microphone on his machine needs a one-time
//! macOS permission grant that no agent can supply, and it is reported as
//! unproven.
//!
//! # Why the tests are serialized
//!
//! The injection is through environment variables, which are process-global,
//! and cargo runs a test binary's tests on parallel threads. Every test here
//! takes `LOCK` before touching them. A shared mutable process resource with
//! more than one writer is exactly the thing that has to be arbitrated rather
//! than hoped about.

#[path = "it/fake/mod.rs"]
mod fake;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tokio::sync::Mutex;

use fake::{Conn, Fake};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tui::app::App;
use tui::event::{Ev, Input};
use tui::mirror;
use tui_render::compose::ItemState;
use tui_render::{MicState, Mode, Region};

const W: u16 = 100;
const H: u16 = 30;

/// The env-var lock.
///
/// **A `tokio::sync::Mutex` and not a `std` one**, because every test here
/// holds it across `await` points — that IS the serialization — and a `std`
/// guard held across an await is a lint and a deadlock hazard rather than a
/// style question. It also has no poisoning, which is the second thing this
/// needs: a test that fails while holding the lock has corrupted nothing (the
/// next test overwrites every variable it reads before using it), and a
/// poisoned `std` mutex turned one real failure into seven identical
/// `PoisonError`s that hid it on the first run of this file.
async fn lock() -> tokio::sync::MutexGuard<'static, ()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(())).lock().await
}

// ── the fixtures, written as real WAVs ───────────────────────────────────────

/// A 16 kHz mono Int16 WAV holding `samples`. The same shape `rec` produces and
/// the same shape `mic::parse_wav` reads.
fn write_wav(path: &Path, samples: &[i16]) {
    let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut v = Vec::new();
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&16_000u32.to_le_bytes());
    v.extend_from_slice(&32_000u32.to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&(data.len() as u32).to_le_bytes());
    v.extend_from_slice(&data);
    std::fs::write(path, v).expect("fixture written");
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("tui-mic-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("scratch dir");
    d
}

/// **DIGITAL SILENCE** — one second of it, exactly what a blocked, ungranted or
/// dead microphone delivers.
fn silent_wav(dir: &Path) -> PathBuf {
    let p = dir.join("silence.wav");
    write_wav(&p, &[0i16; 16_000]);
    p
}

/// Audio with real signal in it. The gate must let this through.
fn loud_wav(dir: &Path) -> PathBuf {
    let p = dir.join("loud.wav");
    let s: Vec<i16> = (0..16_000)
        .map(|i| (((i as f32) * 0.1).sin() * 9000.0) as i16)
        .collect();
    write_wav(&p, &s);
    p
}

/// A file whose contents are the transcript, so `/bin/cat` can BE the
/// transcriber. No shell, no script file: `Cmd` substitutes `{}` only where it
/// appears, so a command without one simply ignores the audio path.
fn transcript_file(dir: &Path, text: &str) -> PathBuf {
    let p = dir.join("transcript.txt");
    let mut f = std::fs::File::create(&p).unwrap();
    f.write_all(text.as_bytes()).unwrap();
    p
}

/// Point mic mode's three legs wherever this test wants them.
fn inject(capture: &str, stt: &str, play: &str) {
    std::env::set_var(tui::mic::CAPTURE_VAR, capture);
    std::env::set_var(tui::mic::STT_VAR, stt);
    std::env::set_var(tui::mic::PLAY_VAR, play);
}

// ── the harness (the composer suite's, unchanged) ────────────────────────────

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

fn ch(c: char) -> Input {
    Input::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
}

fn type_into(app: &mut App, s: &str) {
    for c in s.chars() {
        app.apply(Ev::Input(vec![ch(c)]));
    }
}

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

/// Pump until `done` is true of the app, applying everything that arrives.
async fn pump(app: &mut App, rx: &mut mpsc::Receiver<Ev>, what: &str, done: impl Fn(&App) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if done(app) {
            return;
        }
        match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
            Ok(Some(ev)) => {
                app.apply(ev);
            }
            Ok(None) => break,
            Err(_) => {}
        }
    }
    assert!(done(app), "timed out waiting for {what}");
}

fn sends(f: &Fake) -> Vec<Value> {
    f.log()
        .into_iter()
        .filter(|r| r.leaf() == "send")
        .map(|r| r.params)
        .collect()
}

fn accepting() -> Conn {
    Conn::default().respond(
        "send",
        json!({"accepted": true, "thread_id": "t1", "message_id": 1311, "header_warnings": []}),
    )
}

/// Enter mic mode, record once, and stop. Returns when the capture has JOINED
/// the queue — measured as the count GROWING, not as the queue being non-empty,
/// because a typed entry may already be in it.
async fn record_once(app: &mut App, rx: &mut mpsc::Receiver<Ev>) {
    if app.state().mode != Mode::Mic {
        app.apply(Ev::Input(vec![ctrl('t')]));
        assert_eq!(app.state().mode, Mode::Mic, "^T did not enter mic mode");
    }
    let before = app.state().composer.buf.pending().len();
    app.apply(Ev::Input(vec![ch('r')]));
    assert_eq!(
        app.state().mic,
        MicState::Recording,
        "r did not start recording"
    );
    // **HOLD THE KEY DOWN, as a human does.** Stopping microseconds after
    // starting kills the recorder before it has written anything, and mic mode
    // then correctly reports a capture that came back empty — which is what
    // `a_capture_that_produced_no_bytes_is_reported_and_not_queued` asserts
    // deliberately. This wait is the difference between the two cases, and
    // leaving it out made four tests fail intermittently on the first run of
    // this file.
    //
    // **IT IS NOW ALSO LONGER THAN `mic::HOLD_FIRST_GAP`, AND THAT IS THE
    // POINT OF THE NUMBER.** Two `record` presses closer together than that
    // window cannot be told from the first auto-repeat of a HOLD — there is no
    // key-release event in his terminal to separate them — so the second one
    // is DEFERRED and lands on the loop's idle timer, which this harness does
    // not run. These tests are about the TAP, so they tap at a distance a hand
    // actually taps at. The deferred path has its own test in `app.rs`
    // (`a_second_record_press_too_fast_to_tell_from_a_repeat_is_late_not_lost`).
    tokio::time::sleep(tui::mic::HOLD_FIRST_GAP + std::time::Duration::from_millis(100)).await;
    app.apply(Ev::Input(vec![ch('r')]));
    pump(app, rx, "the capture to be queued", move |a| {
        a.state().composer.buf.pending().len() > before
    })
    .await;
}

// ─────────────────────────────────────────────────────────────────────────────
//  THE ONE THAT MATTERS
// ─────────────────────────────────────────────────────────────────────────────

/// **A SILENT CAPTURE IS REFUSED, AND THE ROW SAYS IT HEARD NOTHING.**
///
/// This is the whole task. `retranscribe` returns the word **`you`** at **exit
/// 0** on digital silence — measured on this machine, on both models, by the
/// scout and again by this build. So an ungranted or dead microphone does not
/// error: it fabricates a plausible English word that this face would show him
/// as his own speech.
///
/// The transcriber here is `/bin/cat` over a file containing the word `you`,
/// which is EXACTLY what the real one would have produced for this audio. **The
/// gate must refuse before the transcriber is reached, so the word never
/// arrives.** If it ever did, the queue row would read `you` and the assertion
/// below would catch it by name.
#[tokio::test]
async fn a_silent_capture_is_refused_by_name_and_never_becomes_the_word_you() {
    let _g = lock().await;
    let d = scratch("silence");
    let silence = silent_wav(&d);
    // The transcriber, if it were ever reached, answers exactly as the real one
    // does on silence. This is the trap, armed.
    let trap = transcript_file(&d, "you");
    inject(
        &format!("/bin/cp {} {{}}", silence.display()),
        &format!("/bin/cat {}", trap.display()),
        "/usr/bin/true",
    );

    let (f, mut app, mut rx) = face(accepting()).await;
    record_once(&mut app, &mut rx).await;
    // Wait for the entry to RESOLVE either way — not for it to fail. Waiting
    // only for the failure would make this test time out when the gate is
    // removed, and a timeout says "something is slow", not "the face just took
    // a fabricated word as his speech". Resolving first means the mutation
    // lands on the assertion below, which names the defect.
    pump(&mut app, &mut rx, "the capture to resolve", |a| {
        a.state().composer.buf.pending()[0].state != ItemState::Transcribing
    })
    .await;

    let item = &app.state().composer.buf.pending()[0];
    // **THE FABRICATED WORD MUST NEVER ARRIVE.** This is the assertion the
    // whole task is about.
    assert_ne!(
        item.text, "you",
        "THE SILENCE TRAP FIRED: the face took the word `you` as something he said"
    );
    let ItemState::Failed(why) = &item.state else {
        panic!(
            "the silent capture was ACCEPTED, state {:?}, text {:?}",
            item.state, item.text
        )
    };
    assert!(
        why.contains("heard nothing"),
        "the row does not say it heard nothing: {why:?}"
    );
    assert!(
        item.text.is_empty(),
        "a refused capture has no text: {:?}",
        item.text
    );
    // The audio is KEPT, so he can hear for himself what it recorded.
    assert!(
        Path::new(item.audio.as_deref().unwrap()).exists(),
        "the refused capture was deleted"
    );
    // And the whole thing reached NO daemon.
    assert!(sends(&f).is_empty(), "a refused capture reached the daemon");
    let _ = std::fs::remove_dir_all(&d);
}

/// **A SEND IS REFUSED WHILE A CAPTURE IS UNRESOLVED**, so a spoken message can
/// never be silently dropped out of the payload.
#[tokio::test]
async fn a_failed_capture_blocks_the_send_rather_than_being_skipped() {
    let _g = lock().await;
    let d = scratch("blocks");
    let silence = silent_wav(&d);
    inject(
        &format!("/bin/cp {} {{}}", silence.display()),
        "/usr/bin/false",
        "/usr/bin/true",
    );

    let (f, mut app, mut rx) = face(accepting()).await;
    record_once(&mut app, &mut rx).await;
    pump(&mut app, &mut rx, "the gate to fire", |a| {
        matches!(
            a.state().composer.buf.pending()[0].state,
            ItemState::Failed(_)
        )
    })
    .await;

    app.apply(Ev::Input(vec![key(KeyCode::Enter)]));
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert!(
        sends(&f).is_empty(),
        "the send went out with an unresolved recording in the queue"
    );
    let n = app
        .state()
        .notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default();
    assert!(
        n.contains("did not transcribe"),
        "the refusal was not spoken: {n:?}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// **THE WHOLE PIPELINE, GREEN: record → gate → transcribe → queue row → SEND.**
///
/// The spoken entry goes out through the EXACTLY same send path `enter` uses in
/// a thread, stitched into the AUI's `Queued messages:` block beside a typed
/// one, and nothing in the payload says which was which.
#[tokio::test]
async fn a_spoken_message_transcribes_and_sends_exactly_as_a_typed_one() {
    let _g = lock().await;
    let d = scratch("green");
    let loud = loud_wav(&d);
    let text = transcript_file(&d, "sweep the resize set points\n");
    inject(
        &format!("/bin/cp {} {{}}", loud.display()),
        &format!("/bin/cat {}", text.display()),
        "/usr/bin/true",
    );

    let (f, mut app, mut rx) = face(accepting()).await;

    // A typed entry first, so the payload has both kinds in it.
    type_into(&mut app, "typed first");
    app.apply(Ev::Input(vec![ctrl('q')]));
    assert!(sends(&f).is_empty(), "queueing reached the daemon");

    record_once(&mut app, &mut rx).await;
    pump(&mut app, &mut rx, "the transcript", |a| {
        a.state()
            .composer
            .buf
            .pending()
            .iter()
            .all(|p| p.is_ready())
    })
    .await;

    let items = app.state().composer.buf.pending().to_vec();
    assert_eq!(items.len(), 2);
    assert_eq!(items[1].state, ItemState::Ready);
    assert_eq!(items[1].text, "sweep the resize set points");
    // **THE PLAY BUTTON IS THE ONLY DIFFERENCE.** One is playable, one is not,
    // and their states are otherwise the same kind of ready.
    assert!(
        !items[0].is_playable(),
        "a typed entry offered a play button"
    );
    assert!(items[1].is_playable(), "a spoken entry has no play button");
    assert!(sends(&f).is_empty(), "transcribing reached the daemon");

    // SEND — through the existing path, exactly as `enter` does in a thread.
    app.apply(Ev::Input(vec![key(KeyCode::Enter)]));
    until_sent(&f).await;

    let s = sends(&f);
    assert_eq!(s.len(), 1, "the whole queue must be ONE message: {s:#?}");
    let sent = s[0]["text"].as_str().unwrap();
    assert!(sent.starts_with("Queued messages:"), "{sent}");
    assert!(sent.contains("Message 1:\ntyped first"), "{sent}");
    assert!(
        sent.contains("Message 2:\nsweep the resize set points"),
        "{sent}"
    );
    // Nothing on the wire says message 2 was spoken.
    assert!(
        !sent.contains(".wav"),
        "the payload leaked a capture path: {sent}"
    );
    assert_eq!(s[0]["metadata"]["tui"]["queued_parts"], 2);
    let _ = std::fs::remove_dir_all(&d);
}

/// **A FAILED TRANSCRIPTION KEEPS ITS AUDIO AND THE RETRY WORKS.**
#[tokio::test]
async fn a_failed_transcription_keeps_its_audio_and_retry_recovers_it() {
    let _g = lock().await;
    let d = scratch("retry");
    let loud = loud_wav(&d);
    inject(
        &format!("/bin/cp {} {{}}", loud.display()),
        "/usr/bin/false",
        "/usr/bin/true",
    );

    let (_f, mut app, mut rx) = face(accepting()).await;
    record_once(&mut app, &mut rx).await;
    pump(&mut app, &mut rx, "the failure", |a| {
        matches!(
            a.state().composer.buf.pending()[0].state,
            ItemState::Failed(_)
        )
    })
    .await;

    let wav = app.state().composer.buf.pending()[0].audio.clone().unwrap();
    assert!(Path::new(&wav).exists(), "a failure deleted his recording");

    // Now make the transcriber work, and retry.
    let text = transcript_file(&d, "second time lucky");
    std::env::set_var(tui::mic::STT_VAR, format!("/bin/cat {}", text.display()));
    app.apply(Ev::Input(vec![ch('t')]));
    pump(&mut app, &mut rx, "the retry", |a| {
        a.state().composer.buf.pending()[0].state == ItemState::Ready
    })
    .await;
    assert_eq!(
        app.state().composer.buf.pending()[0].text,
        "second time lucky"
    );

    // And `d` takes a failed row out, capture and all.
    std::env::set_var(tui::mic::STT_VAR, "/usr/bin/false");
    record_once(&mut app, &mut rx).await;
    pump(&mut app, &mut rx, "the second failure", |a| {
        a.state().composer.buf.failed() == 1
    })
    .await;
    let doomed = app
        .state()
        .composer
        .buf
        .pending()
        .iter()
        .find(|p| matches!(p.state, ItemState::Failed(_)))
        .and_then(|p| p.audio.clone())
        .unwrap();
    app.apply(Ev::Input(vec![ch('d')]));
    assert_eq!(
        app.state().composer.buf.failed(),
        0,
        "`d` did not drop the failed row"
    );
    assert!(!Path::new(&doomed).exists(), "`d` left the capture on disk");
    let _ = std::fs::remove_dir_all(&d);
}

/// **THE PLAY BUTTON, AND ITS FAILURE — forced with the capture deleted.**
///
/// The player here is the REAL `/usr/bin/afplay`. A missing file is its exit 1
/// (`AudioFileOpen failed ('wht?')`), which is the case the task names, and it
/// must reach the info area rather than the `▶` quietly going out — which would
/// look like a clip that played silently.
#[tokio::test]
async fn playing_a_capture_that_is_gone_says_so_rather_than_failing_silently() {
    let _g = lock().await;
    let d = scratch("play");
    let loud = loud_wav(&d);
    let text = transcript_file(&d, "play me");
    inject(
        &format!("/bin/cp {} {{}}", loud.display()),
        &format!("/bin/cat {}", text.display()),
        // The real player. Nothing audible happens: the file it is pointed at
        // will not exist.
        "/usr/bin/afplay {}",
    );

    let (_f, mut app, mut rx) = face(accepting()).await;
    record_once(&mut app, &mut rx).await;
    pump(&mut app, &mut rx, "the transcript", |a| {
        a.state().composer.buf.pending()[0].state == ItemState::Ready
    })
    .await;

    // Delete the capture out from under the play button.
    let wav = app.state().composer.buf.pending()[0].audio.clone().unwrap();
    std::fs::remove_file(&wav).unwrap();

    app.apply(Ev::Input(vec![ch('p')]));
    pump(&mut app, &mut rx, "afplay to refuse", |a| {
        a.state().playing.is_none()
    })
    .await;
    let n = app
        .state()
        .notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default();
    assert!(
        n.contains("could not be played"),
        "a missing capture failed silently; the notice said {n:?}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// **THE GATE INTO MIC MODE REFUSES WHILE THE BOX HAS TEXT, AND SAYS SO.**
///
/// `DIR-5`, his words and his label: *"don't toggle it if there is content in
/// the textarea, to avoid out of order concerns. That's a naive constraint
/// we'll apply for now."*
#[tokio::test]
async fn mic_mode_refuses_to_open_while_the_textarea_holds_text() {
    let _g = lock().await;
    let (_f, mut app, _rx) = face(accepting()).await;

    type_into(&mut app, "half a sentence");
    app.apply(Ev::Input(vec![ctrl('t')]));
    assert_eq!(
        app.state().mode,
        Mode::Thread(Region::Composer),
        "mic mode opened over a draft"
    );
    let n = app
        .state()
        .notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default();
    assert!(
        n.contains("the box has text"),
        "the refusal was silent: {n:?}"
    );

    // Queue what is in the box and it opens.
    app.apply(Ev::Input(vec![ctrl('q')]));
    app.apply(Ev::Input(vec![ctrl('t')]));
    assert_eq!(app.state().mode, Mode::Mic, "mic mode refused an empty box");
    // …and esc leaves it again.
    app.apply(Ev::Input(vec![key(KeyCode::Esc)]));
    assert_eq!(
        app.state().mode,
        Mode::Thread(Region::Composer),
        "esc did not leave mic mode"
    );
}

/// A capture that produced no bytes at all is a named failure, not an empty
/// queue row. `/usr/bin/true` records nothing; the file stays zero-length.
#[tokio::test]
async fn a_capture_that_produced_no_bytes_is_reported_and_not_queued() {
    let _g = lock().await;
    let d = scratch("empty");
    inject("/usr/bin/true", "/usr/bin/false", "/usr/bin/true");

    let (_f, mut app, mut rx) = face(accepting()).await;
    app.apply(Ev::Input(vec![ctrl('t')]));
    app.apply(Ev::Input(vec![ch('r')]));
    // Far enough apart to be two TAPS and not a hold's first repeat — see
    // `record_once`. `/usr/bin/true` records nothing either way, so the
    // capture this stops is still the zero-byte one this test is about.
    tokio::time::sleep(tui::mic::HOLD_FIRST_GAP + std::time::Duration::from_millis(100)).await;
    app.apply(Ev::Input(vec![ch('r')]));
    pump(&mut app, &mut rx, "the empty capture to be reported", |a| {
        matches!(a.state().mic, MicState::Refused(_))
    })
    .await;

    assert!(
        app.state().composer.buf.pending().is_empty(),
        "an empty capture was queued as if it were a message"
    );
    let MicState::Refused(why) = &app.state().mic else {
        unreachable!()
    };
    assert!(why.contains("came back empty"), "{why:?}");
    let _ = std::fs::remove_dir_all(&d);
}

/// Wait until the fake daemon has actually SEEN a `threads/{id}/send`.
///
/// The assertion is on the request the daemon received, not on anything the app
/// believes about itself — which is the whole point of proving a send path
/// against a fake rather than against the face's own state.
async fn until_sent(f: &Fake) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if !sends(f).is_empty() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("no send arrived; the fake saw {:#?}", f.log());
}

// ─────────────────────────────────────────────────────────────────────────────
//  THE DOOR INTO MIC MODE, PROVEN FROM THE BYTES A TERMINAL SENDS
// ─────────────────────────────────────────────────────────────────────────────

/// **FROM RAW BYTES, not from a `Key` a test made up.**
///
/// Every other test in this file starts at `Input::Key(KeyEvent{..})`, which is
/// a `crossterm` value a test constructs by hand. That layer is exactly the one
/// that cannot fail for `opt-m`: `intent(Key::Alt('m'), …)` has been asserted
/// and green for a whole arc while `opt-m` did nothing on his machine, because
/// the mapping was never the question — DELIVERY was. So this helper starts one
/// layer lower, at the bytes, and runs them through `vt::Parser` and
/// `input::to_key` exactly as the pump does.
fn deliver(app: &mut tui::app::App, bytes: &[u8]) {
    let mut p = tui::vt::Parser::new();
    let mut out = Vec::new();
    p.feed(bytes, std::time::Instant::now(), &mut out);
    let batch: Vec<Input> = out
        .into_iter()
        .filter_map(|o| match o {
            tui::vt::Out::Event(ratatui::crossterm::event::Event::Key(k)) => Some(Input::Key(k)),
            _ => None,
        })
        .collect();
    assert!(
        !batch.is_empty(),
        "those bytes produced no key event at all: {bytes:?}"
    );
    app.apply(Ev::Input(batch));
}

/// **`^T` REACHES THE MICROPHONE, FROM THE BYTE TO THE SPAWN.**
///
/// This is the round's quiet blocker, and it is not about a glyph. macOS offers
/// the microphone grant only when an app ATTEMPTS capture. Mic mode's only door
/// was `opt-m`, and on his stack — iTerm2 with `Option Key Sends = Normal`,
/// read from his own preferences — macOS resolves `opt-m` to `µ` in the
/// KEYBOARD LAYER, so no escape sequence is generated and `Key::Alt('m')` can
/// never arrive. He has therefore never been able to make the attempt, so the
/// grant has never been possible, so the capture leg has never been verified.
///
/// `^T` is the byte `0x14`. No layout composes it, tmux's prefix is `^B`, and
/// it is one of the C0 controls every terminal has always sent.
///
/// **WHAT THIS PROVES: the whole chain up to and including the SPAWN of the
/// capture command.** The injected command writes a marker file, so the
/// assertion is on a side effect of the process actually having run — not on
/// any state the app keeps about itself. **WHERE THE PROOF STOPS:** the
/// injected command is not `rec`, and only `rec` opening the real device can
/// produce the macOS grant. That last hop is his hardware and his consent.
#[tokio::test]
async fn ctrl_t_opens_mic_mode_and_reaches_the_capture_spawn() {
    let _g = lock().await;
    let d = scratch("ctrl_t");
    let marker = d.join("it-ran");
    // A capture command that RECORDS THAT IT RAN. `{}` is the WAV path the
    // real `rec` would write; this writes the marker and then the file, so the
    // rest of mic mode behaves exactly as it does with `/usr/bin/true`.
    let script = d.join("capture.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\n: > '{}'\nsleep 5\n", marker.display()),
    )
    .unwrap();
    let mut perm = std::fs::metadata(&script).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perm.set_mode(0o755);
    std::fs::set_permissions(&script, perm).unwrap();
    // Run BY `/bin/sh`, not exec'd directly: macOS assesses a freshly-written
    // executable before its first exec (8-12 s measured by the R7.S1b
    // inspector), while `/bin/sh <script>` reads it as data and reaches the
    // marker in ~0.02 s. So the 5 s wait below is a 7x margin, not a race.
    inject(
        &format!("/bin/sh {} {{}}", script.display()),
        "/usr/bin/false",
        "/usr/bin/true",
    );

    let (_f, mut app, _rx) = face(accepting()).await;
    assert_eq!(app.state().mode, Mode::Thread(Region::Composer));

    // ── the byte, and nothing else ───────────────────────────────────────
    deliver(&mut app, b"\x14");
    assert_eq!(
        app.state().mode,
        Mode::Mic,
        "^T did not open mic mode from the byte the terminal sends"
    );

    // ── and the capture attempt itself ───────────────────────────────────
    assert!(!marker.exists(), "the capture ran before he asked for it");
    deliver(&mut app, b"r");
    assert_eq!(app.state().mic, MicState::Recording, "r did not start it");
    // CAULDRON (R7.S1e): 5 s again. R7.S1b raised it to 30 s because the
    // script was exec'd directly and macOS's first-exec assessment of a new
    // file took 6.8-12.3 s; the script now runs under `/bin/sh` (see `inject`
    // above), which removes that latency. The wait is a ceiling, not a delay —
    // the loop exits the moment the marker appears.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !marker.exists() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        marker.exists(),
        "mic mode opened but the capture command was never spawned — the \
         microphone would never have been asked for"
    );
    // Two taps, at a distance a hand taps at: closer than `HOLD_FIRST_GAP`
    // this would be a hold's first repeat, which is held rather than acted on.
    tokio::time::sleep(tui::mic::HOLD_FIRST_GAP + std::time::Duration::from_millis(100)).await;
    deliver(&mut app, b"r");

    // **THE WITNESS.** `^T` closes what it opened, so a face that had simply
    // stopped answering keys after the first byte cannot score this test.
    deliver(&mut app, b"\x14");
    assert_eq!(
        app.state().mode,
        Mode::Thread(Region::Composer),
        "^T did not leave mic mode"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// **THE REMAP, PROVEN AT THE WIRE — the composed glyph still types, `opt-m`
/// no longer opens anything, and the ctrl byte does.**
///
/// # What this replaced, and why the replacement is the stronger test
///
/// It was `the_bytes_an_esc_plus_option_key_sends_open_mic_mode`, and its whole
/// premise was a HUMAN ACTION we are no longer asking for: set iTerm2's option
/// key to `Esc+` and fourteen dead keys come alive. **He answered that by
/// refusing to depend on the setting at all** — *"ctrl-* might be better
/// because it doesn't have keybindings and it's universally understood to
/// work"* — which is a better answer than ours, so the ask is RETIRED and this
/// file no longer asserts the behaviour it was waiting on.
///
/// What survives unchanged is the WITNESS, which is the measurement the whole
/// diagnosis rests on: the byte his keyboard actually sends for `opt-m` is the
/// composed glyph `µ`, and a glyph is text. The two new halves are the remap
/// itself, and BOTH halves are needed — a remap that leaves the old spelling
/// live is not a remap, and a remap that only tests the new key cannot tell you
/// it did not double-bind.
///
/// FORCED: put `Key::Alt('m')` back in the composer's mic row and the middle
/// assertion goes red; take `Key::Ctrl('t')` out of it and the last one does.
#[tokio::test]
async fn the_composed_glyph_still_types_and_only_the_ctrl_byte_opens_mic_mode() {
    let _g = lock().await;
    inject("/usr/bin/true", "/usr/bin/false", "/usr/bin/true");
    let (_f, mut app, _rx) = face(accepting()).await;

    // ── THE WITNESS: what his keyboard really sends is a character, and it
    //    types. The defect stated as a measurement rather than as a belief.
    deliver(&mut app, "µ".as_bytes());
    assert_eq!(
        app.state().mode,
        Mode::Thread(Region::Composer),
        "the composed glyph did something other than type"
    );
    assert_eq!(
        app.state().composer.buf.text(),
        "µ",
        "the composed glyph did not reach the box as text"
    );
    for _ in 0.."µ".chars().count() {
        deliver(&mut app, b"\x7f");
    }
    assert_eq!(app.state().composer.buf.text(), "");

    // ── THE OLD SPELLING IS GONE. `ESC m` is exactly what iTerm2 sends for
    //    `opt-m` with the option key set to `Esc+`, and it is what every
    //    terminal treating option as meta has always sent. It must now do
    //    NOTHING — the row that answered it is deleted, not aliased.
    deliver(&mut app, b"\x1bm");
    assert_eq!(
        app.state().mode,
        Mode::Thread(Region::Composer),
        "ESC m still opens mic mode — the opt spelling outlived the remap"
    );
    assert_eq!(
        app.state().composer.buf.text(),
        "",
        "ESC m typed something into the box"
    );

    // ── AND THE CTRL BYTE DOES. `^T` is `0x14`: a C0 byte no layout composes
    //    and no tmux prefix claims (his is `^B`, measured).
    deliver(&mut app, b"\x14");
    assert_eq!(
        app.state().mode,
        Mode::Mic,
        "^T did not open mic mode from the byte the terminal sends"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
//  HE CANNOT RECORD TWICE
// ─────────────────────────────────────────────────────────────────────────────

/// **RECORD, LET IT LAND IN THE QUEUE, RECORD AGAIN — end to end.**
///
/// > after doing r to record then it went into the queue and then I no longer
/// > could do r to record another one. I need opt-r to let me do that
/// > regardless.
///
/// # THE STANDING DIAGNOSIS WAS WRONG, AND THIS TEST IS HOW THAT WAS SETTLED
///
/// The cause on the record was `App::toggle_mic`'s `!is_blank()` guard: a
/// finished capture goes into `composer.buf` via `queue_audio`, so the first
/// recording was said to make the composer non-blank and close the door it came
/// in through. **`Composer::is_blank` reads `self.lines` and nothing else**, and
/// `queue_audio` pushes onto `pending` — a different field — so a queued
/// capture, transcribed or not, leaves the buffer blank. This test drives the
/// whole sequence INCLUDING the transcription landing, leaves mic mode and comes
/// back, and records again. It passed the day it was written, before one line of
/// the guard was touched, which is what says the guard was not the defect.
///
/// **What IS true is structural**, and it is what his sentence describes:
/// `Intent::MicRecord` exists in `Mode::Mic` and nowhere else. From the composer
/// he is returned to, `r` types an `r`; in the queue VIEW `r` is RELOAD. So
/// `record_now_from_the_composer_queues_what_is_typed_and_records` is the test
/// that covers his actual ask, and this one is the regression fence under it.
///
/// FORCED: put `if !self.vs.composer.buf.is_blank() { return }` at the top of
/// `App::toggle_record` and the second `record_once` hangs its pump and fails;
/// make `Composer::is_blank` answer `self.lines.is_empty() && self.pending
/// .is_empty()` and the re-entry assertion goes red naming the notice.
#[tokio::test]
async fn record_twice_end_to_end() {
    let _g = lock().await;
    let d = scratch("twice");
    let loud = loud_wav(&d);
    let text = transcript_file(&d, "one\n");
    inject(
        &format!("/bin/cp {} {{}}", loud.display()),
        &format!("/bin/cat {}", text.display()),
        "/usr/bin/true",
    );
    let (_f, mut app, mut rx) = face(accepting()).await;

    record_once(&mut app, &mut rx).await;
    assert_eq!(app.state().composer.buf.pending().len(), 1, "first capture");
    assert_eq!(
        app.state().mode,
        Mode::Mic,
        "mic mode did not survive a capture"
    );
    // LET THE TRANSCRIPTION LAND, which is the state he was actually in when he
    // pressed the second `r`.
    pump(&mut app, &mut rx, "the transcript", |a| {
        a.state().composer.buf.pending().iter().all(|p| p.is_ready())
    })
    .await;
    // **THE MEASUREMENT THAT CORRECTS THE DIAGNOSIS**, stated as an assertion
    // so it cannot quietly stop being true.
    assert!(
        app.state().composer.buf.is_blank(),
        "is_blank() is FALSE with only a transcribed capture in the queue: {:?}",
        app.state().composer.buf.text()
    );

    // LEAVE and come back, which is what he did.
    app.apply(Ev::Input(vec![key(KeyCode::Esc)]));
    assert_eq!(app.state().mode, Mode::Thread(Region::Composer));
    app.apply(Ev::Input(vec![ctrl('t')]));
    assert_eq!(
        app.state().mode,
        Mode::Mic,
        "mic mode refused to re-open with a capture in the queue; notice={:?}",
        app.state().notice.as_ref().map(|n| n.text.clone())
    );

    record_once(&mut app, &mut rx).await;
    assert_eq!(
        app.state().composer.buf.pending().len(),
        2,
        "the SECOND capture never joined the queue"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// **HIS "REGARDLESS": ONE KEYSTROKE RECORDS FROM THE COMPOSER, AND TYPED TEXT
/// IS QUEUED RATHER THAN REFUSED.**
///
/// > I need opt-r to let me do that regardless.
///
/// `^V` — and the letter is the one place his literal words are not executed:
/// `^R` is `Compose(Redo)` in this very mode, its only spelling, and is RELOAD
/// in every mode that has one, so `R` was not available. See
/// `Intent::MicRecordNow`.
///
/// **THE DECISION THIS ASSERTS is the typed-text answer.** `DIR-5`'s guard
/// refuses mic mode over a non-blank box; a key he asked to work "regardless"
/// may not answer with that refusal. Of the two defensible answers this takes
/// QUEUE-THEN-RECORD, because it preserves the order the guard exists to
/// protect — the typed words are in the queue BEFORE the recording — and it is
/// undoable with `^K`.
///
/// FORCED: make `App::record_now` `return` when the buffer is not blank and the
/// mode assertion goes red; drop its `do_queue()` call and the queue-depth
/// assertion goes red naming 0.
#[tokio::test]
async fn record_now_from_the_composer_queues_what_is_typed_and_records() {
    let _g = lock().await;
    let d = scratch("record_now");
    let loud = loud_wav(&d);
    let text = transcript_file(&d, "spoken\n");
    inject(
        &format!("/bin/cp {} {{}}", loud.display()),
        &format!("/bin/cat {}", text.display()),
        "/usr/bin/true",
    );
    let (_f, mut app, mut rx) = face(accepting()).await;

    // ── THE WITNESS: `r` in the composer is TEXT, which is the whole of why he
    //    could not record again. A test that only proved `^V` works would not
    //    have said what was wrong.
    app.apply(Ev::Input(vec![ch('r')]));
    assert_eq!(
        app.state().composer.buf.text(),
        "r",
        "`r` in the composer is not text — this test is about the wrong defect"
    );
    type_into(&mut app, "est of the sentence");
    let typed = app.state().composer.buf.text();
    assert_eq!(typed, "rest of the sentence");

    // ── ONE KEYSTROKE, FROM THE BOX, WITH TEXT IN IT.
    app.apply(Ev::Input(vec![ctrl('v')]));
    assert_eq!(
        app.state().mode,
        Mode::Mic,
        "^V refused instead of recording; notice={:?}",
        app.state().notice.as_ref().map(|n| n.text.clone())
    );
    assert_eq!(
        app.state().mic,
        MicState::Recording,
        "^V entered mic mode but opened no microphone"
    );
    // The typed words were QUEUED, not dropped and not refused — and they are
    // FIRST, which is the ordering the guard existed to protect.
    let q = app.state().composer.buf.pending().to_vec();
    assert_eq!(q.len(), 1, "the typed text was not queued: {q:?}");
    assert_eq!(q[0].text, typed);
    assert_eq!(
        app.state().composer.buf.text(),
        "",
        "the box still holds the text that was queued"
    );

    // ── AND IT IS IDEMPOTENT, OUT LOUD, rather than a stop button.
    app.apply(Ev::Input(vec![ctrl('v')]));
    assert_eq!(
        app.state().mic,
        MicState::Recording,
        "a second ^V stopped the capture — `record` must not be a stop key"
    );
    let n = app
        .state()
        .notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default();
    assert!(
        n.contains("already recording"),
        "the second ^V declined silently: {n:?}"
    );

    // ── the capture still completes and joins the queue behind the typed part.
    tokio::time::sleep(tui::mic::HOLD_FIRST_GAP + std::time::Duration::from_millis(100)).await;
    app.apply(Ev::Input(vec![ch('r')]));
    pump(&mut app, &mut rx, "the capture to be queued", |a| {
        a.state().composer.buf.pending().len() > 1
    })
    .await;
    pump(&mut app, &mut rx, "the transcript", |a| {
        a.state().composer.buf.pending().iter().all(|p| p.is_ready())
    })
    .await;
    let q = app.state().composer.buf.pending().to_vec();
    assert_eq!(q.len(), 2);
    assert_eq!(q[0].text, typed, "the spoken part jumped the typed part");
    assert_eq!(q[1].text, "spoken");
    let _ = std::fs::remove_dir_all(&d);
}

/// **AND THE SAME KEY REACHES IT FROM EVERY REGION OF A THREAD** — his
/// "wherever I am", read as the four surfaces he can be standing on inside one.
///
/// Driven through the KEY TABLE per region rather than by calling the shell
/// method, because the thing at risk is a missing row, not a missing branch.
///
/// FORCED: take the `^V` row out of `queue_rows!` and this names
/// `Thread(Queue)`.
#[test]
fn record_now_is_reachable_from_every_region_of_a_thread_and_from_mic_mode() {
    use tui_render::{intent, Intent, Key};
    for m in [
        Mode::Thread(Region::Composer),
        Mode::Thread(Region::Chat),
        Mode::Thread(Region::Queue),
        Mode::Thread(Region::Agents),
        Mode::Mic,
    ] {
        assert_eq!(
            intent(Key::Ctrl('v'), m),
            Intent::MicRecordNow,
            "^V does not record in {m:?}"
        );
    }
}
