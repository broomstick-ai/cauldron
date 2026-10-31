//! # Dirty state, reconcile, and the one enforcement point: placement
//!
//! `docs/proto-ledger.md` §4. After a turn the runtime's file holds lines the
//! wire never carried — the prompt as it delivered it, `attachment`s, `isMeta`
//! lines — and, if the operator opened the thread interactively, whole
//! exchanges the daemon never saw. Between the runtime's exit and a reconcile
//! the thread is **dirty**: the file holds something the ledger does not.
//!
//! **Reconcile** reads the file from where the render ended, and for each line:
//! an id the ledger already has is skipped; the runtime's copy of a prompt the
//! daemon delivered is **confirmed** (matched by text) and not ingested twice;
//! a conversation line the ledger does not have is **external** and is
//! ingested under the runtime's own uuid; everything else — attachments, meta
//! lines, side state — is a **named loss**, counted. It records
//! `system{what: reconciled}` with the counts and clears the mark. A file that
//! cannot be read records `status: lost` and still clears the mark: the ledger
//! states what could not be read, which is *"accept certain loss"*.
//!
//! **Placement is the only door that refuses.** [`DirtyMarks::check_placement`]
//! is the one function; it is not called at turn start, at a settings write,
//! or on a read. His words: *"It's just at placement that we need to enforce."*

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Value};
use sha2::Digest;

use super::claude_code::{self, classify_native, Outcome, RUNTIME};
use super::event::{AppendRefused, Draft, Kind, Ledger};

/// Why a thread is dirty: the turn that left the file, and where the render
/// ended — the byte the tail begins at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub since_turn: String,
    pub rendered_bytes: u64,
}

/// The per-thread marks. In-memory here; the daemon that adopts this keeps
/// them wherever it keeps per-thread state, and the contract is these four
/// functions.
#[derive(Debug, Default, Clone)]
pub struct DirtyMarks {
    marks: BTreeMap<String, Mark>,
}

/// **A DIRTY THREAD IS NOT PLACED.** The refusal names the thread, the turn it
/// has been dirty since, and where it was going.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementRefused {
    pub thread_id: String,
    pub since_turn: String,
    pub to_account: String,
}

impl std::fmt::Display for PlacementRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "thread {} is dirty since turn {}: its native file holds lines the ledger does not; \
             reconcile it before placing it on {}",
            self.thread_id, self.since_turn, self.to_account
        )
    }
}

impl DirtyMarks {
    /// The runtime exited; the file may hold what the ledger does not.
    pub fn mark(&mut self, thread_id: &str, since_turn: &str, rendered_bytes: u64) {
        self.marks.insert(
            thread_id.to_string(),
            Mark {
                since_turn: since_turn.to_string(),
                rendered_bytes,
            },
        );
    }

    pub fn is_dirty(&self, thread_id: &str) -> Option<&Mark> {
        self.marks.get(thread_id)
    }

    pub fn clear(&mut self, thread_id: &str) -> Option<Mark> {
        self.marks.remove(thread_id)
    }

    /// **THE ENFORCEMENT POINT.** Called by whatever places a thread on an
    /// account, before it does.
    pub fn check_placement(&self, thread_id: &str, to_account: &str) -> Result<(), PlacementRefused> {
        match self.marks.get(thread_id) {
            None => Ok(()),
            Some(m) => Err(PlacementRefused {
                thread_id: thread_id.to_string(),
                since_turn: m.since_turn.clone(),
                to_account: to_account.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileStatus {
    /// The tail was read.
    Read,
    /// The file could not be read; the loss is recorded and accepted.
    Lost,
}

/// What a reconcile found.
#[derive(Debug, Clone, PartialEq)]
pub struct Reconciled {
    pub status: ReconcileStatus,
    /// Lines whose id the ledger already held.
    pub already: usize,
    /// The runtime's copies of prompts the daemon delivered, matched by text.
    pub prompts_confirmed: usize,
    /// Conversation lines the ledger did not hold, now appended under the runtime's ids.
    pub ingested: usize,
    /// Named losses: `attachment`, `meta`, and every side-state `type`.
    pub losses: BTreeMap<String, usize>,
    pub tail_bytes: u64,
    /// sha256 of the WHOLE file as reconcile read it — render plus tail — so
    /// placement can tell "unchanged since the ledger last looked" from "someone
    /// wrote to it since" (`ledger::turn::check_placement`). `None` when the
    /// file was lost.
    pub file_digest: Option<String>,
}

/// **RECONCILE ONE DIRTY THREAD AGAINST ITS NATIVE FILE.** A thread with no
/// mark reconciles to nothing. `now_ts` stamps the `reconciled` event.
pub fn reconcile(
    ledger: &mut Ledger,
    marks: &mut DirtyMarks,
    native_path: &Path,
    now_ts: &str,
) -> std::io::Result<Option<Reconciled>> {
    let Some(mark) = marks.is_dirty(ledger.thread_id()).cloned() else {
        return Ok(None);
    };
    let thread_id = ledger.thread_id().to_string();
    let bytes = match std::fs::read(native_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let r = Reconciled {
                status: ReconcileStatus::Lost,
                already: 0,
                prompts_confirmed: 0,
                ingested: 0,
                losses: BTreeMap::new(),
                tail_bytes: 0,
                file_digest: None,
            };
            record(ledger, &mark, &r, now_ts);
            marks.clear(&thread_id);
            return Ok(Some(r));
        }
        Err(e) => return Err(e),
    };
    let start = (mark.rendered_bytes as usize).min(bytes.len());
    let mut r = Reconciled {
        status: ReconcileStatus::Read,
        already: 0,
        prompts_confirmed: 0,
        ingested: 0,
        losses: BTreeMap::new(),
        tail_bytes: (bytes.len() - start) as u64,
        file_digest: Some(format!("{:x}", sha2::Sha256::digest(&bytes))),
    };
    // Prompt texts the daemon delivered, each confirmable once.
    let mut unconfirmed: Vec<String> = ledger
        .events()
        .iter()
        .filter(|e| e.kind == Kind::User)
        .filter_map(|e| e.body.get("text").and_then(Value::as_str).map(str::to_string))
        .collect();
    for raw in bytes[start..].split(|b| *b == b'\n') {
        if raw.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(raw) else {
            *r.losses.entry("malformed".into()).or_insert(0) += 1;
            continue;
        };
        if let Some(uuid) = v.get("uuid").and_then(Value::as_str) {
            if ledger.contains_id(uuid) {
                r.already += 1;
                continue;
            }
        }
        let line = match classify_native(&v) {
            Ok(c) => c,
            Err(other) => {
                *r.losses.entry(other).or_insert(0) += 1;
                continue;
            }
        };
        if line.is_meta {
            *r.losses.entry("meta".into()).or_insert(0) += 1;
            continue;
        }
        let is_prompt = line.kind == "user" && !line.is_tool_result && !line.is_summary;
        if is_prompt {
            let text = prompt_text(&v);
            if let Some(i) = unconfirmed.iter().position(|t| *t == text) {
                unconfirmed.swap_remove(i);
                r.prompts_confirmed += 1;
                continue;
            }
            // External: a prompt the daemon never delivered, under the runtime's id.
            let d = Draft::new(Kind::User, line.timestamp.clone().unwrap_or_default(), json!({"text": text, "intent": "immediate"}))
                .with_id(line.uuid.clone())
                .native(RUNTIME, claude_code::file_to_wire(&v));
            if ledger.append(d).is_ok() {
                r.ingested += 1;
            }
            continue;
        }
        // Any other conversation line the ledger lacks: translate the file line
        // as the wire line it would have been.
        match claude_code::translate(&claude_code::file_to_wire(&v), None) {
            Outcome::Events(drafts) => {
                let mut any = false;
                for d in drafts {
                    match ledger.append(d) {
                        Ok(_) => any = true,
                        Err(AppendRefused::Replay { .. }) => {}
                        Err(AppendRefused::NoKind) => unreachable!(),
                    }
                }
                if any {
                    r.ingested += 1;
                } else {
                    r.already += 1;
                }
            }
            Outcome::Ephemeral(l) => *r.losses.entry(l.to_string()).or_insert(0) += 1,
            Outcome::Malformed(_) => *r.losses.entry("malformed".into()).or_insert(0) += 1,
        }
    }
    record(ledger, &mark, &r, now_ts);
    marks.clear(&thread_id);
    Ok(Some(r))
}

fn prompt_text(v: &Value) -> String {
    match v.get("message").and_then(|m| m.get("content")) {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn record(ledger: &mut Ledger, mark: &Mark, r: &Reconciled, now_ts: &str) {
    let status = match r.status {
        ReconcileStatus::Read => "read",
        ReconcileStatus::Lost => "lost",
    };
    ledger
        .append(Draft::new(
            Kind::System,
            now_ts,
            json!({
                "what": "reconciled",
                "detail": {
                    "status": status,
                    "since_turn": mark.since_turn,
                    "already": r.already,
                    "prompts_confirmed": r.prompts_confirmed,
                    "ingested": r.ingested,
                    "losses": r.losses,
                    "tail_bytes": r.tail_bytes,
                    "file_digest": r.file_digest,
                },
            }),
        ))
        .expect("a minted id is never a replay");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::claude_code::{render_to, RenderEnv};
    use crate::ledger::event::ThreadMeta;
    use std::io::Write;

    fn ledger() -> Ledger {
        let mut l = Ledger::new(ThreadMeta::root("t", RUNTIME, "2026-10-08T00:00:00Z"));
        l.append(Draft::new(Kind::User, "2026-10-08T00:00:01Z", json!({"text": "hello"})).with_id("u1")).unwrap();
        l.append(Draft::new(Kind::Assistant, "2026-10-08T00:00:02Z", json!({"text": "hi"})).with_id("a1")).unwrap();
        l
    }

    fn env() -> RenderEnv {
        RenderEnv { cwd: "/w".into(), version: "2.1.280".into(), git_branch: None }
    }

    /// **A DIRTY THREAD IS REFUSED AT PLACEMENT, NAMING THE TURN, AND NOWHERE
    /// ELSE IS THERE A CHECK TO CALL.** Marked → refused; cleared → placed; a
    /// thread never marked → placed.
    ///
    /// **THE MUTATION.** In `check_placement`, return `Ok(())` from the `Some`
    /// arm.
    ///
    /// Forced red:
    /// ```text
    /// a dirty thread is refused at placement
    /// ```
    #[test]
    fn a_dirty_thread_is_refused_at_placement() {
        let mut marks = DirtyMarks::default();
        assert_eq!(marks.check_placement("t", "cc2"), Ok(()), "a thread never marked is placed");
        marks.mark("t", "cookie-7", 1234);
        let err = marks.check_placement("t", "cc2").expect_err("a dirty thread is refused at placement");
        assert_eq!(
            err.to_string(),
            "thread t is dirty since turn cookie-7: its native file holds lines the ledger does not; \
             reconcile it before placing it on cc2"
        );
        assert_eq!(marks.is_dirty("t").map(|m| m.rendered_bytes), Some(1234));
        assert_eq!(marks.check_placement("other", "cc2"), Ok(()), "another thread is not refused for this one's mark");
        assert!(marks.clear("t").is_some());
        assert_eq!(marks.check_placement("t", "cc2"), Ok(()), "…and once cleared it is placed");
    }

    /// **RECONCILE READS THE TAIL, CONFIRMS OUR OWN PROMPT ONCE, INGESTS THE
    /// EXTERNAL EXCHANGE UNDER THE RUNTIME'S IDS, AND NAMES THE REST.** Render
    /// the ledger, mark dirty at the render's length, then append what the
    /// runtime would: its copy of our prompt, an attachment, a meta line, an
    /// assistant line the daemon never saw, a side-state line, and a line
    /// whose id the ledger already holds.
    ///
    /// **THE MUTATION.** In `reconcile`, replace the `position` match with
    /// `None` (never confirm a prompt).
    ///
    /// Forced red:
    /// ```text
    /// the runtime's copy of our own prompt is confirmed, not ingested twice
    /// ```
    #[test]
    fn reconcile_confirms_ingests_and_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let mut l = ledger();
        let rendered = render_to(&l, &env(), &path).unwrap();
        let mut marks = DirtyMarks::default();
        marks.mark("t", "cookie-1", rendered.bytes);
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            for line in [
                json!({"type": "user", "uuid": "run-p1", "parentUuid": "a1", "timestamp": "2026-10-08T00:00:03Z", "message": {"role": "user", "content": "hello"}}),
                json!({"type": "attachment", "uuid": "att-1", "parentUuid": "run-p1", "attachment": {"type": "environment"}}),
                json!({"type": "user", "uuid": "meta-1", "isMeta": true, "parentUuid": "att-1", "message": {"role": "user", "content": [{"type": "text", "text": "skill"}]}}),
                json!({"type": "assistant", "uuid": "ext-a2", "parentUuid": "meta-1", "timestamp": "2026-10-08T00:00:04Z", "requestId": "r",
                    "message": {"id": "msg_2", "model": "m", "content": [{"type": "text", "text": "typed by hand"}]}}),
                json!({"type": "user", "uuid": "ext-p2", "parentUuid": "ext-a2", "timestamp": "2026-10-08T00:00:05Z", "message": {"role": "user", "content": "an interactive prompt"}}),
                json!({"type": "queue-operation", "operation": "enqueue", "sessionId": "t"}),
                json!({"type": "assistant", "uuid": "a1", "message": {"content": [{"type": "text", "text": "hi"}]}}),
            ] {
                serde_json::to_writer(&mut f, &line).unwrap();
                f.write_all(b"\n").unwrap();
            }
        }
        let r = reconcile(&mut l, &mut marks, &path, "2026-10-08T00:00:09Z").unwrap().unwrap();
        assert_eq!(r.status, ReconcileStatus::Read);
        assert_eq!(r.prompts_confirmed, 1, "the runtime's copy of our own prompt is confirmed, not ingested twice");
        assert_eq!(r.ingested, 2, "the hand-typed exchange is ingested");
        assert_eq!(r.already, 1, "a line whose id the ledger holds is skipped");
        let mut want = BTreeMap::new();
        want.insert("attachment".to_string(), 1);
        want.insert("meta".to_string(), 1);
        want.insert("queue-operation".to_string(), 1);
        assert_eq!(r.losses, want, "the rest is named");
        assert!(r.tail_bytes > 0);
        let ids: Vec<&str> = l.events().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids[..4], ["u1", "a1", "ext-a2", "ext-p2"], "external lines keep the runtime's ids");
        assert_eq!(l.events()[3].kind, Kind::User);
        assert_eq!(l.events()[3].body["text"], "an interactive prompt");
        assert_eq!(l.events()[2].native.as_ref().unwrap().line["request_id"], "r", "the file line is kept in the wire spelling");
        let last = l.events().last().unwrap();
        assert_eq!((last.kind, last.body["what"].as_str()), (Kind::System, Some("reconciled")));
        assert_eq!(last.body["detail"]["status"], "read");
        assert_eq!(last.body["detail"]["since_turn"], "cookie-1");
        assert_eq!(marks.check_placement("t", "cc2"), Ok(()), "reconciled → placeable");
        assert_eq!(reconcile(&mut l, &mut marks, &path, "x").unwrap(), None, "a clean thread reconciles to nothing");
        // The next render includes the external exchange, chained.
        let again = render_to(&l, &env(), &path).unwrap();
        let back = claude_code::read_native(&path).unwrap();
        assert_eq!(back.chain.iter().map(|c| c.uuid.as_str()).collect::<Vec<_>>(), ["u1", "a1", "ext-a2", "ext-p2"]);
        assert_eq!(claude_code::chain_break(&back.chain), None);
        assert!(again.bytes > rendered.bytes);
    }

    /// **A FILE THAT IS GONE IS RECORDED AS LOST, AND THE MARK CLEARS.** The
    /// ledger states the loss; placement proceeds. "Accept certain loss."
    ///
    /// **THE MUTATION.** In the `NotFound` arm, delete `marks.clear(&thread_id)`.
    ///
    /// Forced red:
    /// ```text
    /// a lost file clears the mark, because the loss is recorded
    /// ```
    #[test]
    fn a_missing_file_is_recorded_as_lost_and_clears_the_mark() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = ledger();
        let mut marks = DirtyMarks::default();
        marks.mark("t", "cookie-2", 10);
        let r = reconcile(&mut l, &mut marks, &dir.path().join("absent.jsonl"), "2026-10-08T00:00:09Z")
            .unwrap()
            .unwrap();
        assert_eq!(r.status, ReconcileStatus::Lost);
        let last = l.events().last().unwrap();
        assert_eq!(last.body["detail"]["status"], "lost");
        assert_eq!(last.body["detail"]["since_turn"], "cookie-2");
        assert_eq!(marks.check_placement("t", "cc2"), Ok(()), "a lost file clears the mark, because the loss is recorded");
    }
}
