//! **THE SEND PATH, PROVEN AGAINST A SCRIPTED FAKE — AND NEVER AGAINST HIS
//! LIVE SYSTEM.**
//!
//! Every act the thread sheet offers changes something real and irreversible on
//! the other end of one of two sockets: a thread closed on his board, a title
//! overwritten, every pin renumbered, a turn he was waiting on killed. There is
//! no read-only rehearsal for any of them. So the whole path is proven here
//! against `fake9010`, on an ephemeral loopback port this suite opened itself,
//! and **the assertions are on the BYTES the client wrote** — read back the way
//! `apps/aui/src/v2/ws.rs:112-164` reads them: `from_str::<ClientMsg>`, then
//! `Mutation::from_envelope`.
//!
//! The pure half of the same proof — which socket each act leaves by, and what
//! its parameters are — is in `src/acts.rs`'s own tests, where it needs no
//! socket at all. This file proves the other half: that what leaves the process
//! is what that table said, and that **nothing leaves it when the link is
//! down.**

use crate::fake9010;

use std::time::Duration;

use serde_json::json;
use tui::acts::{self, Dir, ProjectAct, ThreadAct, ThreadRef, Verb, Wire};
use tui::aui::{self, AuiEvent, DownCause, Reach};

use aui_wire::proto::ClientMsg;
use aui_wire::server::Mutation;
use fake9010::{Fake9010, Script};

const TID: &str = "11111111-2222-3333-4444-555555555555";
const OTHER: &str = "99999999-8888-7777-6666-555555555555";

fn thr() -> ThreadRef {
    ThreadRef {
        project: "harness".into(),
        id: TID.into(),
        turn_count: 41,
        pinned: true,
        closed: false,
    }
}

fn cfg(url: &str) -> aui::Config {
    aui::Config {
        // The ladder runs in milliseconds here. It is CONFIGURATION and not
        // test scaffolding — 2s→30s is the live policy, written down in a
        // struct precisely so a reconnect proof does not take a minute.
        backoff_min: Duration::from_millis(5),
        backoff_max: Duration::from_millis(20),
        ack_timeout: Duration::from_secs(5),
        ..aui::Config::pinned(url)
    }
}

/// Send one act through the live client and return the fake's view of it.
async fn send(
    fake: &Fake9010,
    h: &aui::Handle,
    act: ThreadAct,
    order: &[String],
) -> Result<(), String> {
    let wire = acts::route(&act, &thr(), order).expect("routed");
    let Wire::Aui(m) = wire else {
        panic!("{act:?} left by the DAEMON socket; this test is about 9010")
    };
    let r = h.mutate(m).await;
    let _ = fake;
    r
}

async fn up(fake: &Fake9010, h: &aui::Handle) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if h.reach().is_up() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the client never came up; frames: {:#?}", fake.frames());
}

// ────────────────────────────────────────────────────────────────────────────
//  THE HANDSHAKE
// ────────────────────────────────────────────────────────────────────────────

/// **THE FIRST FRAME IS A `hello` AND IT DECLARES THIS FACE BY NAME.**
///
/// `proto.rs:85` already names `"tui" (terminal)` as a client kind and
/// `:94-96` documents an EMPTY `device_id` as the desk TUI's posture — this
/// face marks nothing delivered, so it has nothing to remember across
/// connections and the server treats each connection as its own anonymous
/// device. Both are asserted as bytes because both are wire contract.
#[tokio::test]
async fn the_client_introduces_itself_as_the_tui_with_no_device_id() {
    let fake = Fake9010::start(vec![Script::default()]).await;
    let (h, _rx) = aui::spawn(cfg(&fake.url));
    up(&fake, &h).await;

    let first = &fake.frames()[0];
    assert_eq!(first.kind(), "hello", "the first frame was not a hello");
    let v = first.json();
    assert_eq!(v["client"]["kind"], json!("tui"));
    assert_eq!(
        v["client"]["device_id"],
        json!(""),
        "the desk TUI must declare NO device id — it marks nothing delivered"
    );
    assert_eq!(v["proto_version"], json!(2));
    // …and the live server must be able to read it.
    assert!(
        matches!(
            serde_json::from_str::<ClientMsg>(&first.text).expect("the server could not parse it"),
            ClientMsg::Hello { .. }
        ),
        "the hello does not deserialize as the server's own ClientMsg"
    );
}

// ────────────────────────────────────────────────────────────────────────────
//  THE ACTS, AS BYTES
// ────────────────────────────────────────────────────────────────────────────

/// **EVERY AUI-OWNED ACT, AS THE BYTES THE SERVER WOULD READ.**
///
/// N >= 2 on purpose, and rather more than two: the envelope's `kind`, its
/// payload shape and its `thread_id` spelling are three separate ways to be
/// wrong, and one act cannot show that the mapping holds across the set.
///
/// FORCED RED three ways, each watched: change `Mutation::SetThreadPinned` to
/// `SetThreadClosed` in `acts::route` (the kind assertion fails), drop the
/// `project` field (the payload assertion fails), and send the `session_id`
/// instead of the `id` (the `thread_id` assertion fails).
#[tokio::test]
async fn every_aui_act_reaches_the_socket_as_the_envelope_the_server_parses() {
    let fake = Fake9010::start(vec![Script::default()]).await;
    let (h, _rx) = aui::spawn(cfg(&fake.url));
    up(&fake, &h).await;

    let order = vec![OTHER.to_string(), TID.to_string()];
    let acts = [
        ThreadAct::SetModel(Some(aui_wire::Model::Opus)),
        // (TUI-P1a: `Rename` left this socket for the daemon — TC-03/TC-28.)
        ThreadAct::SetPinned(true),
        ThreadAct::Move(Dir::Up),
        ThreadAct::SetClosed(true),
    ];
    for a in acts {
        send(&fake, &h, a, &order).await.expect("the fake acked it");
    }
    fake.until_mutations(4).await;

    let kinds: Vec<String> = (0..4)
        .map(|i| {
            fake.envelope(i)["kind"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            "set_thread_model",
            "set_thread_pinned",
            "move_pin",
            "set_thread_closed",
        ],
        "the acts did not arrive as the mutations the design binds them to"
    );

    // The payloads, exactly. The `kind` appears INSIDE the payload too, because
    // `Mutation` is an internally-tagged enum and `from_envelope` parses the
    // PAYLOAD — which is what the live server does.
    assert_eq!(
        fake.envelope(0)["payload"],
        json!({"kind": "set_thread_model", "project": "harness",
               "thread_id": TID, "model": "Opus"})
    );
    assert_eq!(
        fake.envelope(1)["payload"],
        json!({"kind": "set_thread_pinned", "project": "harness",
               "thread_id": TID, "pinned": true})
    );
    // **THE MOVE NAMES ITS NEIGHBOUR AND NOTHING ELSE.** No pin_order, no
    // index, no renumbering — the server renumbers, and this face never does.
    assert_eq!(
        fake.envelope(2)["payload"],
        json!({"kind": "move_pin", "project": "harness",
               "thread_id": TID, "before": OTHER})
    );
    assert_eq!(
        fake.envelope(3)["payload"],
        json!({"kind": "set_thread_closed", "project": "harness",
               "thread_id": TID, "closed": true})
    );

    // **AND THE SERVER'S OWN PARSER READS ALL FOUR.** This is the assertion
    // that makes the rest of them mean something: `v2/ws.rs:159` calls exactly
    // this, and a payload it refuses is a `MutResult::Err` on his machine and
    // an act that silently did not happen.
    for i in 0..4 {
        let env: aui_wire::node::MutationEnvelope =
            serde_json::from_value(fake.envelope(i)).expect("the envelope shape");
        Mutation::from_envelope(&env)
            .unwrap_or_else(|e| panic!("the AUI would have refused mutation #{i}: {e}"));
    }
}

/// **A REFUSAL COMES BACK IN THE AUI'S OWN WORDS, VERBATIM.**
///
/// `mutate.rs`'s refusals are already written for a human — *"is a real
/// directory here, not a link — removing it is not something a menu row may
/// do"* — and paraphrasing one would put this face between him and the only
/// party that knows what went wrong.
///
/// FORCED RED by wrapping the message in the client (`format!("9010: {msg}")`).
#[tokio::test]
async fn the_aui_s_refusal_is_relayed_word_for_word() {
    // (TUI-P1a: the rename that carried this left 9010 for the daemon; any
    // AUI act carries a refusal the same way.)
    let sentence = "is not a thread in harness — nothing was closed";
    let fake = Fake9010::start(vec![Script::refuses(sentence)]).await;
    let (h, _rx) = aui::spawn(cfg(&fake.url));
    up(&fake, &h).await;

    let err = send(
        &fake,
        &h,
        ThreadAct::SetClosed(true),
        &[],
    )
    .await
    .expect_err("the fake refused it and the client reported success");
    assert_eq!(err, sentence, "the refusal was re-worded on its way to him");
}

/// **A MISSING ACK IS "NOT PERFORMED", SAID OUT LOUD.**
///
/// `0060`: *"treat a missing ack after the reconnect backoff as 'not performed'
/// and say so — never assume."* The socket dies with the mutation in flight; the
/// caller must be ANSWERED, with an error, and never left hanging and never
/// told it worked.
///
/// FORCED RED by answering the waiter with `Ok(())` on teardown, or by dropping
/// the `pending` map without answering (the test then hangs and fails on its
/// own timeout rather than passing).
#[tokio::test]
async fn a_mutation_whose_ack_never_comes_is_reported_as_not_performed() {
    // Swallows the mutation, then closes the socket under it.
    let fake = Fake9010::start(vec![Script::swallows().close_after(1)]).await;
    let (h, _rx) = aui::spawn(cfg(&fake.url));
    up(&fake, &h).await;

    let err = tokio::time::timeout(
        Duration::from_secs(10),
        send(&fake, &h, ThreadAct::SetClosed(true), &[]),
    )
    .await
    .expect("the caller was never answered at all — it hung on a dead socket")
    .expect_err("an unacked mutation was reported as having happened");
    assert!(
        err.contains("NOT performed"),
        "the refusal must say it did not happen: {err}"
    );
    // The frame DID leave — this is about the answer, not the send.
    assert_eq!(fake.mutations().len(), 1);
}

// ────────────────────────────────────────────────────────────────────────────
//  UNREACHABLE IS A STATE
// ────────────────────────────────────────────────────────────────────────────

/// **NOTHING LEAVES THE PROCESS WHEN THE LINK IS DOWN, AND THE REFUSAL IS THE
/// ROW'S OWN SENTENCE.**
///
/// This is the test the whole arc's one rule is about. A mutation queued
/// against a dead link would fire into whatever socket exists minutes later —
/// a `MovePin` landing late renumbers an order he has since fixed by hand — so
/// it is refused immediately and never queued.
///
/// FORCED RED by deleting the `reach().is_up()` guard in `Handle::mutate`: the
/// frame then sits in the channel and the assertion on `frames()` fails as soon
/// as a socket appears.
#[tokio::test]
async fn with_no_link_nothing_is_sent_and_the_refusal_is_the_rows_own_words() {
    // A listener that never answers `hello`, so the client connects and never
    // comes up. This is the AUI up-but-not-serving shape; the never-connects
    // shape is covered below.
    let fake = Fake9010::start(vec![Script::silent()]).await;
    let (h, _rx) = aui::spawn(cfg(&fake.url));
    fake.until_connections(1).await;

    assert!(!h.reach().is_up(), "a silent server was read as usable");
    for act in [
        ThreadAct::SetPinned(true),
        ThreadAct::SetClosed(true),
        // UP, not down: this thread is LAST in the order below, so a move down
        // is refused by the router for a reason that has nothing to do with the
        // link — and this test is about the link.
        ThreadAct::Move(Dir::Up),
    ] {
        let wire =
            acts::route(&act, &thr(), &[OTHER.to_string(), TID.to_string()]).expect("routed");
        let Wire::Aui(m) = wire else {
            panic!("wrong socket")
        };
        // **BOUNDED, BECAUSE THE REGRESSION THIS GUARDS IS A HANG.** Delete the
        // `reach().is_up()` check in `Handle::mutate` and the job sits in the
        // outbound channel waiting for a handshake that never comes — so an
        // unbounded `await` here would make the forced failure a test that
        // never returns, which reads as a stuck suite rather than as a caught
        // defect. A timeout turns it back into a red.
        let err = tokio::time::timeout(Duration::from_secs(5), h.mutate(m))
            .await
            .expect("a mutation with no link was QUEUED instead of refused — it hung")
            .expect_err("a mutation went out with no link");
        // The SAME sentence the row wears. Two spellings of one refusal is how
        // a row and its notice come to disagree.
        assert!(
            err == DownCause::NotRunning.words() || err == DownCause::DoorShut.words(),
            "the refusal is not one of the two drawn sentences: {err}"
        );
    }
    // **THE ASSERTION THAT MATTERS: NOT ONE MUTATION LEFT THIS PROCESS.**
    assert!(
        fake.mutations().is_empty(),
        "a mutation reached the socket with the link down: {:#?}",
        fake.mutations()
    );
}

/// **A DOOR THAT IS NOT THERE IS A DRAWN STATE, NOT AN ERROR PATH.**
///
/// The AUI's listener used to bind the tailnet address only, and had on
/// record been unbound for DAYS as a result (`apps/aui/src/v2/mod.rs`'s
/// module header). It now also binds loopback unconditionally, but the AUI
/// process itself can still be down, mid-restart, or simply not yet started
/// — this test proves THAT absence is still a drawn state and not an
/// exception, by pointing at a loopback port nothing is listening on.
#[tokio::test]
async fn a_closed_door_is_a_state_with_a_sentence_and_not_an_error() {
    // Port 1 on loopback: nothing is listening and nothing can be.
    let (h, mut rx) = aui::spawn(cfg("ws://127.0.0.1:1/"));
    let mut saw_absent = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Some(AuiEvent::Reach(Reach::Absent { cause }))) => {
                // Connection refused on loopback: the route exists and the
                // listener does not, which is the "not running" sentence.
                assert_eq!(cause, DownCause::NotRunning);
                assert_eq!(
                    cause.words(),
                    "needs the audio interface, which is not running"
                );
                saw_absent = true;
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }
    assert!(saw_absent, "the client never reported the door as shut");
    assert!(!h.reach().is_up());
    // And it keeps trying, forever, quietly — unreachable is normal here.
    assert!(matches!(
        h.reach(),
        Reach::Absent { .. } | Reach::Connecting { .. }
    ));
}

/// **A LINK THAT DIES IS DOWN AT ONCE, NOT WHEN THE BACKOFF EXPIRES.**
///
/// FOUND BY READING, NOT BY A FAILING TEST, and it is the one real defect this
/// build shipped and repaired. The link state used to be stored AFTER the
/// reconnect sleep, so for up to thirty seconds after a live socket died
/// `reach()` still answered `Up`: every AUI act row drew itself as available,
/// and `mutate` passed its guard and ENQUEUED the job — which then outlived the
/// drain and fired into the NEXT session. A `MovePin` landing a minute late
/// renumbers a pin order he has since fixed by hand, which is exactly the
/// failure the drain exists to prevent, arriving through the front door.
///
/// The backoff here is THIRTY SECONDS on purpose: it is far longer than the
/// test's own deadline, so the assertion can only pass if the state went down
/// IMMEDIATELY rather than at the end of the ladder.
///
/// FORCED RED by moving the `h.state.store(S_ABSENT, ...)` back out of `down`
/// and into a post-sleep swap — the original code. The first assertion then
/// times out, because `reach()` stays `Up` for the whole thirty seconds.
#[tokio::test]
async fn a_link_that_dies_is_down_at_once_and_refuses_before_the_ladder_runs() {
    // Connection 1 answers one mutation and then closes under us. The second
    // script exists only so a reconnect has somewhere to go — with a 30s floor
    // it will not be reached inside this test, which is the point.
    let fake = Fake9010::start(vec![Script::default().close_after(1), Script::default()]).await;
    let (h, _rx) = aui::spawn(aui::Config {
        backoff_min: Duration::from_secs(30),
        backoff_max: Duration::from_secs(30),
        ..aui::Config::pinned(&fake.url)
    });
    up(&fake, &h).await;

    // One real act, acked, after which the fake hangs up.
    send(&fake, &h, ThreadAct::SetPinned(true), &[])
        .await
        .expect("the fake acked the first one");
    fake.until_mutations(1).await;

    // **DOWN WITHIN A COUPLE OF SECONDS**, not within thirty.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline && h.reach().is_up() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !h.reach().is_up(),
        "the link still reads as UP after the socket died — every act row is \
         drawing itself as available with no socket behind it"
    );

    // …and the next act is REFUSED rather than queued against the next socket.
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        send(&fake, &h, ThreadAct::SetClosed(true), &[]),
    )
    .await
    .expect("the second act was QUEUED against a dead link instead of refused")
    .expect_err("an act went out after the socket died");
    assert!(
        err == DownCause::NotRunning.words() || err == DownCause::DoorShut.words(),
        "the refusal is not one of the two drawn sentences: {err}"
    );
    // **AND IT NEVER REACHED THE WIRE**, on either connection.
    assert_eq!(
        fake.mutations().len(),
        1,
        "a second mutation reached a socket after the link died: {:#?}",
        fake.mutations()
    );
}

// ────────────────────────────────────────────────────────────────────────────
//  THE HINT
// ────────────────────────────────────────────────────────────────────────────

/// **AN `Invalidate` REACHES THE FACE**, which is what makes a pin made on the
/// phone land here without him pressing `^R`.
///
/// The re-read itself is proven in `app.rs`'s own tests, where a store exists
/// to re-read into; this proves the frame survives the wire and arrives named.
///
/// FORCED RED by dropping the `ServerMsg::Invalidate` arm in `aui::session`.
#[tokio::test]
async fn an_invalidate_frame_reaches_the_face_carrying_its_node_kind() {
    let fake = Fake9010::start(vec![Script::default()
        .push(fake9010::invalidate("pinned"))
        .push(fake9010::invalidate("thread_options"))])
    .await;
    let (h, mut rx) = aui::spawn(cfg(&fake.url));
    up(&fake, &h).await;

    let mut seen: Vec<String> = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline && seen.len() < 2 {
        if let Ok(Some(AuiEvent::Invalidate { node })) =
            tokio::time::timeout(Duration::from_millis(200), rx.recv()).await
        {
            seen.push(node);
        }
    }
    assert_eq!(seen, vec!["pinned", "thread_options"]);
    // The FILTER is the face's, not the wire's: both arrive, and only the one
    // that can change an index file costs a re-read.
    assert!(aui::invalidate_touches_index("pinned"));
    assert!(
        !aui::invalidate_touches_index("thread_options"),
        "thread_options changes no index file; re-reading on it is work with no effect"
    );
}

// ────────────────────────────────────────────────────────────────────────────
//  THE DAEMON HALF — proven on the daemon's own fake, next door
// ────────────────────────────────────────────────────────────────────────────

/// **THE TWO DAEMON-OWNED ACTS NEVER TOUCH 9010**, even while it is up and
/// working.
///
/// This is `0060`'s first binding condition — *"no convenience relay"* — and it
/// is asserted against a LIVE, HEALTHY fake AUI, because the interesting case
/// is not "the AUI is down so we fell back". It is "the AUI is right there and
/// we still did not use it".
///
/// FORCED RED by routing either act to `Mutation::InterruptThread` /
/// `Mutation::CompactThread`, both of which exist and both of which would work.
#[tokio::test]
async fn interrupt_and_compact_never_touch_the_aui_even_while_it_is_up() {
    let fake = Fake9010::start(vec![Script::default()]).await;
    let (h, _rx) = aui::spawn(cfg(&fake.url));
    up(&fake, &h).await;

    for act in [
        ThreadAct::Interrupt,
        ThreadAct::Compact {
            instructions: Some("keep the wire notes".into()),
        },
    ] {
        match acts::route(&act, &thr(), &[]).expect("routed") {
            Wire::Daemon(verb) => {
                // **THE VERB, AND ITS SUBJECT.** This used to assert on the
                // method PATH, because the id was part of the name a caller
                // built. `seam::commands` builds the path now, in one place, so
                // what is left to pin here is the pair a reader cares about:
                // which of the three things this face can say, and about which
                // thread.
                assert!(
                    verb.name() == "cancel" || verb.name() == "send",
                    "unexpected daemon verb {}",
                    verb.name()
                );
                assert_eq!(verb.thread(), TID, "a verb aimed at the wrong thread");
            }
            Wire::Aui(m) => panic!(
                "{act:?} was relayed through the AUI as {:?} — DIR-7's resolution \
                 forbids exactly that, and it would make the act fail when the AUI is down",
                m.kind()
            ),
        }
    }
    // Nothing reached 9010 at all, and the link was up the whole time.
    assert!(h.reach().is_up());
    assert!(
        fake.mutations().is_empty(),
        "a daemon-owned act was relayed to 9010: {:#?}",
        fake.mutations()
    );
}

/// **THE MODEL ACT IS THE ONE THAT MUST NOT GO TO THE DAEMON**, and here it is
/// proven at the socket rather than in the table: with BOTH owners reachable,
/// the model change arrives at 9010 and the daemon sees nothing.
#[tokio::test]
async fn the_model_change_arrives_at_the_aui_and_the_daemon_is_not_asked() {
    let fake = Fake9010::start(vec![Script::default()]).await;
    let (h, _rx) = aui::spawn(cfg(&fake.url));
    up(&fake, &h).await;

    send(
        &fake,
        &h,
        ThreadAct::SetModel(Some(aui_wire::Model::Fable)),
        &[],
    )
    .await
    .expect("acked");
    fake.until_mutations(1).await;
    assert_eq!(fake.envelope(0)["kind"], json!("set_thread_model"));
    assert_eq!(fake.envelope(0)["payload"]["model"], json!("Fable"));

    // No frame this client ever writes names the daemon's trap verb.
    for f in fake.frames() {
        assert!(
            !f.text.contains("/update"),
            "the daemon's silently-reverted model verb appeared on the wire: {}",
            f.text
        );
    }
}

/// The compaction bytes, asserted where they are actually built. The daemon
/// half of the sheet rides the transport that already exists, so its fake is
/// `tests/it/fake/mod.rs`; what is worth pinning HERE is that the payload the
/// router produces is the AUI's own, character for character.
#[test]
fn the_compaction_send_is_the_aui_s_own_prompt_including_its_trailing_newline() {
    let Wire::Daemon(Verb::Send {
        thread,
        text,
        intent,
        bare,
        headers,
        metadata,
    }) = acts::route(
        &ThreadAct::Compact {
            instructions: Some("drop the dead ends".into()),
        },
        &thr(),
        &[],
    )
    .expect("routed")
    else {
        panic!("compaction left by the AUI socket")
    };
    // **THE SUBJECT IS A FIELD, AND THAT IS WHY THERE IS NOTHING TO ASSERT
    // ABOUT THE PARAMS ANY MORE.** This used to pin the method path AND then
    // pin that `thread_id` was absent from the params, because the daemon
    // refuses a call that spells its subject twice. `Verb::Send` has one place
    // to put a thread, so the double-spelling this guarded against cannot be
    // expressed — and `tests/it/fake/mod.rs` still refuses it, for a caller that
    // reaches the link directly.
    assert_eq!(thread, TID);
    // `prompt_and_headers` (threads_client.rs:2304-2315) appends a newline to
    // EVERY prompt, and `bare: true` is what keeps the slash command first.
    assert_eq!(text, "/compact drop the dead ends\n");
    assert!(bare);
    assert!(headers.is_empty());
    assert_eq!(intent.as_deref(), Some("immediate"));
    assert_eq!(
        metadata["tui"]["compaction"]["reason"],
        json!("drop the dead ends")
    );
    // The whole payload was asserted field by field above; `Verb::Send` has no
    // untyped bag left to spot-check, so the `let _: Value = params` that stood
    // here has nothing to bind.
}

// ────────────────────────────────────────────────────────────────────────────
//  THE PROJECT HALF, AS BYTES — the destructive end of this face
// ────────────────────────────────────────────────────────────────────────────

/// Send one PROJECT act through the live client, exactly as `App::fire_project`
/// does, and return what the far end answered.
///
/// The `Wire::Daemon` arm panics rather than being handled: the daemon has no
/// project concept at all, so a project act arriving there is not a case to
/// cover, it is the routing table having been broken.
async fn send_project(h: &aui::Handle, act: ProjectAct, name: &str) -> Result<(), String> {
    let wire = acts::route_project(&act, name).expect("routed");
    let Wire::Aui(m) = wire else {
        panic!("{act:?} left by the DAEMON socket — the daemon has no project concept at all")
    };
    h.mutate(m).await
}

/// **EVERY PROJECT ACT, AS THE BYTES THE SERVER WOULD READ.**
///
/// The thread half has this proof already; the project half needs it MORE,
/// because these are the acts with no read-only rehearsal anywhere:
/// `remove_project` unlinks `~/minds/<name>` on his real machine,
/// `archive_project` takes a project off his real root, and `create_project`
/// makes a real directory. "Which bytes, for which act" therefore has to be
/// answerable without a single byte reaching his listener — so this runs
/// against `fake9010` on an ephemeral loopback port this suite opened itself.
///
/// N >= 2 several times over: six acts, five distinct `kind`s, and the
/// `project`/`name` field asymmetry crossing the set. One act could not show
/// that the mapping holds.
///
/// FORCED RED four ways, each watched: point `Archive` at
/// `Mutation::RemoveProject` (the `kinds` assertion fails naming the act),
/// spell `RemoveProject`'s field `name` (the payload assertion fails), send
/// `Some(Model::Fable)` for the `default` press (the model payload fails), and
/// normalize the created name in `route_project` (the raw-name assertion
/// fails).
#[tokio::test]
async fn every_project_act_reaches_the_socket_as_the_envelope_the_server_parses() {
    let fake = Fake9010::start(vec![Script::default()]).await;
    let (h, _rx) = aui::spawn(cfg(&fake.url));
    up(&fake, &h).await;

    let acts = [
        ProjectAct::SetModel(Some(aui_wire::Model::Fable)),
        ProjectAct::SetModel(None),
        ProjectAct::Archive,
        ProjectAct::Remove,
        ProjectAct::Unarchive,
        ProjectAct::Create {
            name: "New Project".into(),
        },
    ];
    for a in acts {
        send_project(&h, a, "harness")
            .await
            .expect("the fake acked it");
    }
    fake.until_mutations(6).await;

    let kinds: Vec<String> = (0..6)
        .map(|i| {
            fake.envelope(i)["kind"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            "set_project_model",
            "set_project_model",
            "archive_project",
            "remove_project",
            "unarchive_project",
            "create_project",
        ],
        "the project acts did not arrive as the mutations the design binds them to"
    );

    // **THE FIELD ASYMMETRY IS THE SERVER'S AND IT IS ON THE WIRE.**
    // `set_project_model` and `remove_project` spell the key `project`;
    // `archive_project`, `unarchive_project` and `create_project` spell it
    // `name`. A frame with the wrong key is a `MutResult::Err` he would read as
    // "the menu row is broken".
    assert_eq!(
        fake.envelope(0)["payload"],
        json!({"kind": "set_project_model", "project": "harness", "model": "Fable"})
    );
    // **`default` IS `null`, NOT A MODEL.** The press that CLEARS the override
    // and the press that sets one are the same mutation with different
    // payloads, and sending a model here would pin him to whatever this face
    // last guessed.
    assert_eq!(
        fake.envelope(1)["payload"],
        json!({"kind": "set_project_model", "project": "harness", "model": null})
    );
    assert_eq!(
        fake.envelope(2)["payload"],
        json!({"kind": "archive_project", "name": "harness"})
    );
    assert_eq!(
        fake.envelope(3)["payload"],
        json!({"kind": "remove_project", "project": "harness"})
    );
    assert_eq!(
        fake.envelope(4)["payload"],
        json!({"kind": "unarchive_project", "name": "harness"})
    );
    // **THE TYPED NAME GOES RAW.** `normalize_project_name` is the server's one
    // normalizer (`mutate.rs:443`); normalizing here too would be a second
    // answer to "what is this project called", and the second one is the one
    // his directory gets named by.
    assert_eq!(
        fake.envelope(5)["payload"],
        json!({"kind": "create_project", "name": "New Project"})
    );

    // **AND THE SERVER'S OWN PARSER READS ALL SIX.** This is what makes the
    // rest of them mean something: `v2/ws.rs:159` calls exactly this, and a
    // payload it refuses is an act that silently did not happen.
    for i in 0..6 {
        let env: aui_wire::node::MutationEnvelope =
            serde_json::from_value(fake.envelope(i)).expect("the envelope shape");
        Mutation::from_envelope(&env)
            .unwrap_or_else(|e| panic!("the AUI would have refused project mutation #{i}: {e}"));
    }
}

/// **THE SHARPEST REFUSAL IN THE ARC, RELAYED WORD FOR WORD.**
///
/// `remove` on a project that is a REAL DIRECTORY rather than a symlink is the
/// one refusal that explains a rule this face does not know — that `~/minds`
/// holds links, and that a row he can press must not be able to delete a
/// repository (`apps/aui/src/v2/mutate.rs:405-424`). Paraphrasing it would put
/// this face between him and the only party that knows what went wrong.
///
/// The sentence asserted here is the v2 handler's own, character for
/// character, including its em dash.
///
/// FORCED RED by paraphrasing it in the client, and by wrapping it
/// (`format!("9010: {msg}")`).
#[tokio::test]
async fn the_v2_refusal_for_a_real_directory_reaches_him_verbatim() {
    let sentence = "harness is a real directory here, not a link — \
                    removing it is not something a menu row may do";
    let fake = Fake9010::start(vec![Script::refuses(sentence)]).await;
    let (h, _rx) = aui::spawn(cfg(&fake.url));
    up(&fake, &h).await;

    let err = send_project(&h, ProjectAct::Remove, "harness")
        .await
        .expect_err("the fake refused it and the client reported success");
    assert_eq!(
        err, sentence,
        "the AUI's refusal was re-worded on its way to him"
    );

    // WITNESS, keyed on something the relay cannot move: the fake really did
    // receive a `remove_project` frame. Without it an equality against a
    // constant would pass on a client that never sent anything and answered
    // from a cache.
    fake.until_mutations(1).await;
    assert_eq!(fake.envelope(0)["kind"], json!("remove_project"));
}
