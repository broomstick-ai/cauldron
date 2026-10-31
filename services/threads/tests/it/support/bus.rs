//! # Watching the daemon's push bus from a test, now that no wire carries it
//!
//! Until R2.S4 (2026-10-10) these tests sent `{"type":"subscribe"}` on the
//! legacy door and read the firehose's `event` / `liveness` / `status` /
//! `progress` / `lifecycle` / `info` frames off the socket to learn when the
//! daemon had DONE something — a turn ended, a retry was scheduled, a status
//! edge fired. The firehose is retired; no consumer reads those frames, and no
//! door serves them. Every fact they carried is a shape now (`threads/log`, the
//! `threads` shape's cells, `narration`, `boot`) and is tested as one where it
//! is a consumer contract.
//!
//! What these tests need is not the wire form — it is the daemon's own
//! account of what it did, in order. That is the in-process bus
//! (`ThreadManager::subscribe`, `Hub::firehose`), which is unchanged and is what
//! the shapes themselves fold. [`Bus`] reads it and renders each [`Push`] as the
//! JSON object the retired frame had, so a predicate written against
//! `f["type"] == "event" && f["event"]["kind"] == "turn_ended"` reads the same
//! fact it always did. **This rendering is a test-side convenience and is NOT a
//! wire contract**: nothing in the daemon produces it any more.
//!
//! The receiver is opened by [`Bus::open`] at the moment the old code sent its
//! subscribe, so nothing emitted after that point can be missed — which is what
//! the subscribe-before-act ordering in these tests was for.
//!
//! Included with `#[path = "support/bus.rs"] mod bus;` — `tests/support/` is not
//! a test target of its own.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::time::Duration;

use serde_json::{json, Value};
use threads::event::Push;
use threads::ThreadManager;
use tokio::sync::broadcast;

/// One push, as the JSON object the retired legacy frame carried. Optional
/// fields are omitted when absent, as the frame omitted them.
pub fn frame(p: Push) -> Value {
    fn put(o: &mut serde_json::Map<String, Value>, k: &str, v: Option<Value>) {
        if let Some(v) = v {
            o.insert(k.to_string(), v);
        }
    }
    let mut o = serde_json::Map::new();
    match p {
        Push::Event(se) => {
            o.insert("type".into(), json!("event"));
            o.insert("thread_id".into(), json!(se.thread_id));
            o.insert("seq".into(), json!(se.seq));
            put(&mut o, "message_id", se.message_id.map(|m| json!(m)));
            o.insert("ts".into(), json!(se.ts));
            o.insert("event".into(), serde_json::to_value(&se.event).unwrap());
        }
        Push::Liveness {
            thread_id,
            busy,
            parent_running,
            running_subagents,
        } => {
            o.insert("type".into(), json!("liveness"));
            o.insert("thread_id".into(), json!(thread_id));
            o.insert("busy".into(), json!(busy));
            o.insert("parent_running".into(), json!(parent_running));
            o.insert("running_subagents".into(), json!(running_subagents));
        }
        Push::Status {
            thread_id,
            state,
            executing,
            queued,
            unread,
            detail,
            account,
        } => {
            o.insert("type".into(), json!("status"));
            o.insert("thread_id".into(), json!(thread_id));
            o.insert("state".into(), json!(state));
            o.insert("executing".into(), json!(executing));
            o.insert("queued".into(), json!(queued));
            o.insert("unread".into(), json!(unread));
            put(&mut o, "detail", detail.map(|d| json!(d)));
            put(&mut o, "account", account.map(|a| json!(a)));
        }
        Push::Progress {
            op,
            id,
            account,
            phase,
            detail,
            done,
            ok,
        } => {
            o.insert("type".into(), json!("progress"));
            o.insert("op".into(), json!(op));
            o.insert("id".into(), json!(id));
            put(&mut o, "account", account.map(|a| json!(a)));
            o.insert("phase".into(), json!(phase));
            put(&mut o, "detail", detail.map(|d| json!(d)));
            o.insert("done".into(), json!(done));
            put(&mut o, "ok", ok.map(|b| json!(b)));
        }
        Push::Lifecycle(n) => {
            o.insert("type".into(), json!("lifecycle"));
            o.insert("service".into(), json!(n.service));
            o.insert("phase".into(), json!(n.phase));
            o.insert("reason".into(), json!(n.reason));
            o.insert("detail".into(), json!(n.detail));
            o.insert("boot_id".into(), json!(n.boot_id));
            o.insert("at".into(), json!(n.at));
        }
        Push::Info {
            domain,
            reason,
            account,
            detail,
            at,
        } => {
            o.insert("type".into(), json!("info"));
            o.insert("domain".into(), json!(domain));
            o.insert("reason".into(), json!(reason));
            put(&mut o, "account", account.map(|a| json!(a)));
            put(&mut o, "detail", detail.map(|d| json!(d)));
            o.insert("at".into(), json!(at));
        }
    }
    Value::Object(o)
}

/// A buffered reader over the bus.
pub struct Bus {
    rx: broadcast::Receiver<Push>,
    pending: VecDeque<Value>,
}

impl Bus {
    /// Open a receiver NOW. Everything emitted from here on is seen.
    pub fn open(m: &ThreadManager) -> Self {
        Self {
            rx: m.subscribe(),
            pending: VecDeque::new(),
        }
    }

    /// The next push, or `None` after `within`.
    pub async fn next_within(&mut self, within: Duration) -> Option<Value> {
        if let Some(f) = self.pending.pop_front() {
            return Some(f);
        }
        let deadline = tokio::time::Instant::now() + within;
        loop {
            match tokio::time::timeout_at(deadline, self.rx.recv()).await {
                Err(_) => return None,
                Ok(Ok(p)) => return Some(frame(p)),
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => return None,
            }
        }
    }

    /// The next push matching `pred`, holding back the ones that do not (they
    /// stay available to a later wait, in order). Panics after 10 s, as the
    /// socket reader it replaces did.
    pub async fn wait(&mut self, mut pred: impl FnMut(&Value) -> bool) -> Value {
        if let Some(i) = self.pending.iter().position(|f| pred(f)) {
            return self.pending.remove(i).unwrap();
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let p = match tokio::time::timeout_at(deadline, self.rx.recv()).await {
                Err(_) => panic!("timed out waiting for a push"),
                Ok(Ok(p)) => p,
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => panic!("the bus closed"),
            };
            let f = frame(p);
            if pred(&f) {
                return f;
            }
            self.pending.push_back(f);
        }
    }

    /// Everything that arrives within `within`, held-back frames first.
    pub async fn drain_for(&mut self, within: Duration) -> Vec<Value> {
        let mut out: Vec<Value> = self.pending.drain(..).collect();
        let deadline = tokio::time::Instant::now() + within;
        loop {
            match tokio::time::timeout_at(deadline, self.rx.recv()).await {
                Err(_) => return out,
                Ok(Ok(p)) => out.push(frame(p)),
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => return out,
            }
        }
    }

    /// Pull everything that arrives within `within` into the held buffer,
    /// without consuming any of it — what a socket reader's "drain" did.
    pub async fn absorb_for(&mut self, within: Duration) {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            match tokio::time::timeout_at(deadline, self.rx.recv()).await {
                Err(_) => return,
                Ok(Ok(p)) => self.pending.push_back(frame(p)),
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => return,
            }
        }
    }

    /// The pushes received and not yet consumed by a [`Bus::wait`].
    pub fn held(&self) -> &VecDeque<Value> {
        &self.pending
    }

    /// Forget everything held.
    pub fn clear(&mut self) {
        self.pending.clear();
    }
}
