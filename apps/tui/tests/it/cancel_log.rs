//! **TC-46 (TUI-P1f): EVERY INTERRUPT THE FACE SENDS IS IN THE FLIGHT
//! RECORDER, WITH THE DAEMON'S ANSWER.**
//!
//! The operator's "can't interrupt" incident on an internal thread could not be
//! rebuilt from `~/.tui/diag.log`: the face recorded no cancel at all. These
//! tests drive a cancel through the face's own `mirror::Handle` against the
//! FAKE chassis in `fake/mod.rs` (an ephemeral loopback port; it can never
//! reach his daemons) and read what the recorder wrote through a
//! `diag::tap`, which receives exactly the lines the file would.
//!
//! Each test filters on a thread id only it uses, because a tap also receives
//! the lines of tests running beside it.

#[path = "fake/mod.rs"]
mod fake;

use std::time::Duration;

use fake::{Conn, Fake};
use serde_json::json;
use tui::acts::Verb;
use tui::diag;
use tui::mirror;

async fn live(f: &Fake) -> (mirror::Handle, mirror::TestPorts) {
    let (handle, _trx, ports) = mirror::spawn_at(f.port);
    f.until_subscribed(1).await;
    for _ in 0..400 {
        if handle.is_live() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(handle.is_live(), "the fake never came up");
    (handle, ports)
}

/// The `cancel` lines that name `thread`, in the order written.
fn cancel_lines(tap: &diag::Tap, thread: &str) -> Vec<String> {
    let needle = format!("thread={thread} ");
    tap.lines()
        .into_iter()
        .filter(|l| l.contains(" cancel ") && l.contains(&needle))
        .collect()
}

/// The `rid=` value on a line.
fn rid(line: &str) -> String {
    line.split_whitespace()
        .find_map(|w| w.strip_prefix("rid="))
        .unwrap_or_else(|| panic!("no rid= on {line:?}"))
        .to_string()
}

#[tokio::test]
async fn an_interrupt_logs_its_send_and_the_ok_reply_under_one_request_id() {
    let thread = "p1f-ok-thread-0001";
    let f = Fake::start(vec![Conn::default().respond("cancel", json!({"cancelled": true}))]).await;
    let (handle, _ports) = live(&f).await;
    let tap = diag::tap();

    let r = handle
        .verb_via(Verb::Cancel { thread: thread.into() }, "test · interrupt row")
        .await;
    assert!(r.is_ok(), "the fake accepts the cancel: {r:?}");

    let lines = cancel_lines(&tap, thread);
    assert_eq!(lines.len(), 2, "one SEND and one REPLY, got {lines:#?}");
    let (send, reply) = (&lines[0], &lines[1]);
    assert!(
        send.contains(" SEND ")
            && send.contains("verb=cancel")
            && send.contains(&format!("endpoint=threads/{thread}/cancel"))
            && send.contains(r#"via="test · interrupt row""#),
        "the send line names the verb, endpoint and path: {send}"
    );
    assert!(
        reply.contains(" REPLY ")
            && reply.contains("result=ok")
            && !reply.contains("result=error")
            && reply.contains("latency_ms=")
            && reply.contains("cancelled"),
        "the reply line says ok, the latency and the body: {reply}"
    );
    assert_eq!(rid(send), rid(reply), "send and reply are correlated");
    // A second cancel gets a NEW id: a constant rid would join nothing.
    let _ = handle.cancel(thread, "again").await;
    let lines = cancel_lines(&tap, thread);
    assert_eq!(lines.len(), 4, "{lines:#?}");
    assert_ne!(rid(&lines[0]), rid(&lines[2]), "each cancel has its own id");

    // **WHAT IS SENT IS UNCHANGED**: the path, empty params, once per call.
    let sent: Vec<_> = f
        .log()
        .into_iter()
        .filter(|q| q.leaf() == "cancel")
        .collect();
    assert_eq!(sent.len(), 2, "{sent:#?}");
    assert_eq!(sent[0].method, format!("threads/{thread}/cancel"));
    assert_eq!(sent[0].params, json!({}));
}

#[tokio::test]
async fn a_refused_interrupt_logs_the_error_reply() {
    let thread = "p1f-err-thread-0002";
    let f = Fake::start(vec![Conn::default().refuse(
        "cancel",
        "not_running",
        "p1f-no-turn-is-running",
    )])
    .await;
    let (handle, _ports) = live(&f).await;
    let tap = diag::tap();

    let r = handle
        .verb_via(Verb::Cancel { thread: thread.into() }, "test")
        .await;
    assert!(r.is_err(), "the fake refuses the cancel: {r:?}");

    let lines = cancel_lines(&tap, thread);
    assert_eq!(lines.len(), 2, "one SEND and one REPLY, got {lines:#?}");
    let (send, reply) = (&lines[0], &lines[1]);
    assert!(send.contains(" SEND "), "{send}");
    assert!(
        reply.contains(" REPLY ")
            && reply.contains("result=error")
            && !reply.contains("result=ok")
            && reply.contains("p1f-no-turn-is-running"),
        "the reply line records the refusal: {reply}"
    );
    assert_eq!(rid(send), rid(reply));
}

/// The SEND line is written before the request leaves, not reconstructed
/// after: with the fake withholding every answer, the line is already there
/// while the cancel is still in flight, and no REPLY line exists yet.
#[tokio::test]
async fn the_send_line_is_written_before_any_reply_arrives() {
    let thread = "p1f-hang-thread-0003";
    let f = Fake::start(vec![Conn::default().no_suback()]).await;
    let (handle, _ports) = live(&f).await;
    let tap = diag::tap();

    let h = handle.clone();
    let t = thread.to_string();
    let task = tokio::spawn(async move { h.cancel(&t, "test").await });
    f.until("the cancel to reach the fake", |f| {
        f.log().iter().any(|q| q.leaf() == "cancel")
    })
    .await;
    let lines = cancel_lines(&tap, thread);
    task.abort();
    assert_eq!(lines.len(), 1, "only the SEND while in flight: {lines:#?}");
    assert!(lines[0].contains(" SEND "), "{}", lines[0]);
}
