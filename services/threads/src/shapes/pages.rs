//! **NOT A SHAPE.** A thread's MESSAGE PAGES — the resource ids the `threads`
//! shape's `pages` cell carries, and the bytes the resource door serves for
//! each (R2.S3d, item C).
//!
//! ## What this replaces, and why it is a resource
//!
//! The harness's `thread-messages` reads HISTORICAL message ranges by id —
//! `--ids`, `--range`, `--tail`, `--grep`, `--stats` — and did it with
//! `threads/{id}/query`, a request with a cursor. The operator's ruling removes
//! the ability to request data; `threads/log` cannot serve a range either (it
//! is the newest 4,000 events and every streaming fragment is one). The chassis
//! already has the approved door for bytes too large for a cell —
//! `resource/{id}/read` and `GET /resource/{id}` (`docs/chassis-resources.md`,
//! operator H66) — and an id there is OPAQUE to the chassis and resolved by the
//! producer (`shapewire::resource::Resources::fetch`). So a page is addressable
//! with NO new verb: the shape names the ids, the door reads them.
//!
//! ## The id, and the page
//!
//! Message ids are partitioned into fixed pages of [`PAGE_MESSAGES`]: page `k`
//! covers `[k·P, k·P + P − 1]`, cut at the thread's newest message id. A page's
//! id is `<thread_id>.m<from>-<to>`:
//!
//! ```text
//! 7f3e.m0-255      every event with message_id 0..=255   (full: immutable)
//! 7f3e.m256-301    256..=301, the thread's newest is 301 (open: mutable —
//!                  the next message re-names it 7f3e.m256-302)
//! ```
//!
//! It cannot collide with a body id (`<thread_id>.<seq>`, `body.rs`): a body
//! id's last segment is all digits and a page id's starts with `m`. A consumer
//! never builds one — it reads `{id, from, to}` off the `pages` cell and picks
//! the pages whose range covers the ids it wants, which is the whole of
//! "address a page by thread + range".
//!
//! The bytes are JSON — `{thread_id, from, to, messages: [...]}`, every event
//! with a message id in the range, oldest first, each spelled exactly as
//! `threads/{id}/query` spelled a message (`server::cited_message_json`), so a
//! consumer moving off the request reads the same objects.
//!
//! ## Retention
//!
//! A FULL page is [`Retention::Fixed`]: message ids are allocated once and
//! never re-used, so once the thread has a message past `to` nothing can enter
//! `[from, to]`. The OPEN page — the one whose `to` is the thread's newest id
//! short of a full page — is [`Retention::Latest`]: the next message moves the
//! newest id and the shape publishes a new id for the page, which is the
//! resource model's own definition of mutable (*"the id changes on every
//! write"*). An id naming a range past the thread's newest message was never
//! published and is refused `unknown`.
//!
//! [`Retention::Fixed`]: shapewire::resource::Retention::Fixed
//! [`Retention::Latest`]: shapewire::resource::Retention::Latest

use serde_json::{json, Value};

/// Message ids per page. 256 keeps a 10,000-message thread's `pages` cell to 40
/// entries. It bounds a page by COUNT only: 256 long messages can exceed the
/// inline door's cap (`shapewire::resource::MAX_WS_INLINE_BYTES`), which then
/// answers `too_large`, and the same id is read whole through
/// `GET /resource/{id}`, which streams and has no such cap (R2.S3f;
/// `tests/message_pages.rs::an_oversize_page_reads_whole_through_the_by_id_door`).
pub const PAGE_MESSAGES: u64 = 256;

pub const PAGE_CONTENT_TYPE: &str = "application/json; charset=utf-8";

/// The id of the page `[from, to]` of `thread_id`, or `None` for a thread id the
/// door could not carry — `body::body_id`'s rule, applied once.
pub fn page_id(thread_id: &str, from: u64, to: u64) -> Option<String> {
    crate::shapes::body::body_id(thread_id, 0)?;
    Some(format!("{thread_id}.m{from}-{to}"))
}

fn canonical(n: &str) -> Option<u64> {
    if n.is_empty() || (n.len() > 1 && n.starts_with('0')) || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

/// `<thread_id>.m<from>-<to>` → `(thread_id, from, to)`, for exactly the ids
/// [`pages_of`] can publish: `from` on a page boundary, `to` inside that page.
pub fn parse_page_id(id: &str) -> Option<(&str, u64, u64)> {
    let (tid, tail) = id.rsplit_once('.')?;
    if tid.is_empty() {
        return None;
    }
    let (from, to) = tail.strip_prefix('m')?.split_once('-')?;
    let (from, to) = (canonical(from)?, canonical(to)?);
    if from % PAGE_MESSAGES != 0 || to < from || to >= from + PAGE_MESSAGES {
        return None;
    }
    Some((tid, from, to))
}

/// The page list for a thread whose newest message id is `last`: `[{id, from,
/// to}]`, oldest first. The ONE derivation — the `threads` row calls it at
/// export and nothing stores its answer.
pub fn pages_of(thread_id: &str, last: u64) -> Vec<Value> {
    let mut out = Vec::new();
    let mut from = 0;
    while from <= last {
        let to = (from + PAGE_MESSAGES - 1).min(last);
        let Some(id) = page_id(thread_id, from, to) else {
            return Vec::new();
        };
        out.push(json!({ "id": id, "from": from, "to": to }));
        from += PAGE_MESSAGES;
    }
    out
}

/// Is `[from, to]` a FULL page — one no future message can enter?
pub fn is_full(from: u64, to: u64) -> bool {
    to == from + PAGE_MESSAGES - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_round_trips_and_a_dotted_thread_id_survives_it() {
        for tid in ["7f3e", "bob.example.com", "a-b_c~d:e@f+g"] {
            let id = page_id(tid, 256, 301).unwrap();
            assert_eq!(parse_page_id(&id), Some((tid, 256, 301)));
        }
    }

    #[test]
    fn a_page_id_is_never_a_body_id_and_a_body_id_is_never_a_page_id() {
        let page = page_id("t", 0, 255).unwrap();
        assert_eq!(crate::shapes::body::parse_body_id(&page), None);
        assert_eq!(parse_page_id("t.255"), None);
    }

    #[test]
    fn only_the_ids_pages_of_publishes_parse() {
        for bad in ["t.m1-2", "t.m0-256", "t.m256-255", "t.m00-1", "t.m0-", "t.0-1", ".m0-1"] {
            assert_eq!(parse_page_id(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_pages_cover_every_id_once_and_the_last_is_cut_at_the_newest() {
        let p = pages_of("t", 600);
        let ranges: Vec<(u64, u64)> = p
            .iter()
            .map(|v| (v["from"].as_u64().unwrap(), v["to"].as_u64().unwrap()))
            .collect();
        assert_eq!(ranges, [(0, 255), (256, 511), (512, 600)]);
        assert!(is_full(0, 255) && !is_full(512, 600));
        assert_eq!(pages_of("t", 0).len(), 1);
    }
}
