//! The synthesis queue: one consumer over every cache key, priority first,
//! first-in as the tiebreaker.
//!
//! > "There's one consumer that's going to consume all of them in order.
//! > Starting with high priority, tiebreaker goes to first in… you actually
//! > need to treat each key, each cache like its own cue that can either be
//! > last-in, first-out or first-in, first-out… the first consideration is
//! > priority." — the operator
//!
//! ## The one order
//!
//! 1. Each key is its own queue and offers its best item: highest priority,
//!    and among equals the oldest (`fifo`) or the newest (`lifo`).
//! 2. The consumer takes the best offer across keys: highest priority, then
//!    first in, then key name, so the order is total. Keys at one priority
//!    therefore INTERLEAVE by arrival.
//!
//! [`Queue::pop`] and [`Queue::position`] both read that order, so what a
//! client is told about its place cannot drift from what it is served.
//!
//! ## Priority is one number
//!
//! An item's priority is what its caller asked for, or its cache's default
//! when the caller said nothing — plus [`INTERACTIVE`] when somebody is
//! blocked on it. That is not a second axis; it is a very large term in the
//! one number.

use std::cmp::{Ordering, Reverse};
use std::collections::{HashMap, VecDeque};

use crate::keys::Queueing;

/// Added for an item whose caller is waiting on it: far above anything a
/// client would send, so "he is waiting for this now" beats "he will probably
/// want this soon" whatever the ladder is set to.
pub const INTERACTIVE: i64 = 1_000_000;

/// The most items the queue holds. It had no bound, and that was the whole
/// saturation: entering a thread registered one item per message — 2,200 for
/// the longest — into a queue draining under two a second, half an hour
/// deep. 512 is about four minutes of work at the measured drain rate.
pub const MAX_QUEUED: usize = 512;

/// Synthesis permits kept free for a blocked caller. Priority alone did not
/// meet the bar: a `tts.request` reached the head of the queue at once and
/// then waited 1.4–2.5 s for a permit, because all four were held by warm
/// work. With one reserved, the row he is standing on always starts at once.
pub const RESERVED_FOR_INTERACTIVE: usize = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// A `tts.request`: a caller is blocked on the answer.
    Interactive,
    /// A `tts.register`: warming, fire and forget.
    Warm,
}

impl Lane {
    pub fn as_str(&self) -> &'static str {
        match self {
            Lane::Interactive => "interactive",
            Lane::Warm => "warm",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Lane::Interactive, Lane::Warm].into_iter().find(|l| l.as_str() == s)
    }
}

#[derive(Debug, Clone)]
pub struct Job {
    pub key: String,
    pub hash: String,
    pub text: String,
    pub lane: Lane,
    /// What the caller asked for; `None` takes the key's default at push.
    pub priority: Option<i64>,
    /// How many times this job's backend child has been killed from outside
    /// ([`crate::error::Error::Interrupted`]). Bounds one job's re-queue
    /// loop; a fresh request always starts at zero.
    pub attempt: u32,
}

/// How a key orders its own items. A snapshot, so the queue never holds the
/// config lock.
#[derive(Debug, Clone, Copy)]
pub struct KeyOrder {
    /// The cache's default item priority.
    pub priority: i64,
    pub queueing: Queueing,
}

#[derive(Debug, Clone)]
struct Queued {
    job: Job,
    /// Resolved at push (defaults applied), and only ever raised after.
    priority: i64,
    /// Monotonic; "first in" is the lowest.
    seq: u64,
}

impl Queued {
    fn effective(&self) -> i64 {
        match self.job.lane {
            Lane::Interactive => self.priority.saturating_add(INTERACTIVE),
            Lane::Warm => self.priority,
        }
    }

    /// The cross-key rank the consumer maximizes: priority, then first in,
    /// then key name.
    fn rank<'a>(&self, key: &'a str) -> (i64, Reverse<u64>, &'a str) {
        (self.effective(), Reverse(self.seq), key)
    }

    /// Within one key: priority first, the key's discipline second.
    fn within_key(a: &Queued, b: &Queued, newest_first: bool) -> Ordering {
        b.effective().cmp(&a.effective()).then_with(|| if newest_first { b.seq.cmp(&a.seq) } else { a.seq.cmp(&b.seq) })
    }
}

#[derive(Default)]
pub struct Queue {
    keys: HashMap<String, VecDeque<Queued>>,
    order: HashMap<String, KeyOrder>,
    next_seq: u64,
}

impl Queue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a key's order. Takes effect at the next pop; an item already
    /// queued keeps the priority it was resolved to at push.
    pub fn set_order(&mut self, key: &str, order: KeyOrder) {
        self.order.insert(key.to_string(), order);
    }

    fn default_priority(&self, key: &str) -> i64 {
        self.order.get(key).map_or(crate::priority::DEFAULT, |o| o.priority)
    }

    fn newest_first(&self, key: &str) -> bool {
        self.order.get(key).is_some_and(|o| o.queueing == Queueing::Lifo)
    }

    /// Enqueue, then enforce [`MAX_QUEUED`] — RETURNING every job the bound
    /// dropped. A job is more than a queue entry: by the time it is pushed the
    /// manager holds a single-flight claim and a `queued` row for it, and the
    /// queue can reach neither. Dropping one silently is how a sixteen-character
    /// row ("thread compacted") became permanently unspeakable: every later
    /// request joined a claim no job would ever finish.
    #[must_use = "an evicted job still holds a claim and a store row; release them"]
    pub fn push(&mut self, job: Job) -> Vec<Job> {
        let priority = job.priority.unwrap_or_else(|| self.default_priority(&job.key));
        let seq = self.next_seq;
        self.next_seq += 1;
        self.keys.entry(job.key.clone()).or_default().push_back(Queued { job, priority, seq });
        let mut evicted = Vec::new();
        while self.depth() as usize > MAX_QUEUED {
            // The warm item that would be served LAST. A blocked caller is
            // never evicted: that would turn a slow utterance into a silent
            // one. If only blocked callers are left, the queue stays over.
            let Some((key, seq)) = self.worst_warm() else { break };
            if let Some(q) = self.take(&key, |q| q.seq == seq) {
                evicted.push(q.job);
            }
        }
        evicted
    }

    /// Remove and return `(key, hash)` if it is still waiting: the dequeue
    /// half of `tts.cancel`.
    pub fn remove(&mut self, key: &str, hash: &str) -> Option<Job> {
        self.take(key, |q| q.job.hash == hash).map(|q| q.job)
    }

    fn take(&mut self, key: &str, which: impl Fn(&Queued) -> bool) -> Option<Queued> {
        let dq = self.keys.get_mut(key)?;
        let q = dq.remove(dq.iter().position(which)?)?;
        if dq.is_empty() {
            self.keys.remove(key);
        }
        Some(q)
    }

    fn worst_warm(&self) -> Option<(String, u64)> {
        self.keys
            .iter()
            .flat_map(|(k, dq)| dq.iter().filter(|q| q.job.lane == Lane::Warm).map(move |q| (k, q)))
            .min_by(|(ka, a), (kb, b)| a.rank(ka).cmp(&b.rank(kb)))
            .map(|(k, q)| (k.clone(), q.seq))
    }

    /// THE MAX RULE: a duplicate hash raises the queued item to the higher of
    /// the two priorities and never enqueues a second copy or lowers it. "If
    /// there's already something in there that has that hash, then you just
    /// take the max of the two priorities." `None` resolves to the key's
    /// default. The sequence number is untouched. Returns the priority now
    /// held, or `None` if the hash is not queued.
    pub fn raise_priority(&mut self, key: &str, hash: &str, incoming: Option<i64>) -> Option<i64> {
        let incoming = incoming.unwrap_or_else(|| self.default_priority(key));
        let q = self.keys.get_mut(key)?.iter_mut().find(|q| q.job.hash == hash)?;
        q.priority = q.priority.max(incoming);
        Some(q.priority)
    }

    /// Somebody is now waiting on a queued warm item: it picks up
    /// [`INTERACTIVE`], keeping its place among the waited-on. True if it moved.
    pub fn promote(&mut self, key: &str, hash: &str) -> bool {
        let Some(q) = self.keys.get_mut(key).and_then(|dq| dq.iter_mut().find(|q| q.job.hash == hash)) else {
            return false;
        };
        let moved = q.job.lane == Lane::Warm;
        q.job.lane = Lane::Interactive;
        moved
    }

    /// Each key's offer, and the best of them. Linear and allocation-free: it
    /// runs under the queue lock on every pop.
    ///
    /// With `interactive_only`, a key whose best item is warm offers nothing —
    /// and that hides nothing, because [`INTERACTIVE`] puts every waited-on
    /// item above every warm one, so such a key holds none.
    fn best(&self, interactive_only: bool) -> Option<(&str, u64)> {
        self.keys
            .iter()
            .filter_map(|(k, dq)| {
                let nf = self.newest_first(k);
                let cand = dq.iter().min_by(|a, b| Queued::within_key(a, b, nf))?;
                (!interactive_only || cand.job.lane == Lane::Interactive).then_some((k.as_str(), cand))
            })
            .max_by(|(ka, a), (kb, b)| a.rank(ka).cmp(&b.rank(kb)))
            .map(|(k, q)| (k, q.seq))
    }

    /// Every queued item in the order the consumer will serve them: each key
    /// sorted by its own discipline, then the heads merged by rank.
    fn serve_order(&self) -> Vec<&Queued> {
        let mut lanes: Vec<(&str, Vec<&Queued>)> = self
            .keys
            .iter()
            .map(|(k, dq)| {
                let nf = self.newest_first(k);
                let mut v: Vec<&Queued> = dq.iter().collect();
                v.sort_by(|a, b| Queued::within_key(a, b, nf));
                (k.as_str(), v)
            })
            .collect();
        let mut out = Vec::with_capacity(self.depth() as usize);
        loop {
            let best = lanes
                .iter()
                .enumerate()
                .filter_map(|(i, (k, v))| v.first().map(|q| (i, q.rank(k))))
                .max_by(|a, b| a.1.cmp(&b.1))
                .map(|(i, _)| i);
            let Some(i) = best else { break };
            out.push(lanes[i].1.remove(0));
        }
        out
    }

    pub fn pop(&mut self) -> Option<Job> {
        self.pop_where(false)
    }

    /// [`Queue::pop`], optionally only work somebody is blocked on. `None`
    /// while the queue is NOT empty means everything left is warm and the
    /// permit reserve forbids starting it, so the permit goes back.
    pub fn pop_where(&mut self, interactive_only: bool) -> Option<Job> {
        let (key, seq) = self.best(interactive_only).map(|(k, s)| (k.to_string(), s))?;
        self.take(&key, |q| q.seq == seq).map(|q| q.job)
    }

    /// 0-based place of `hash` in the serve order: what `not_ready` reports.
    pub fn position(&self, hash: &str) -> Option<u64> {
        self.serve_order().iter().position(|q| q.job.hash == hash).map(|n| n as u64)
    }

    /// The resolved priority of a queued hash.
    pub fn priority_of(&self, key: &str, hash: &str) -> Option<i64> {
        self.keys.get(key)?.iter().find(|q| q.job.hash == hash).map(|q| q.priority)
    }

    pub fn depth(&self) -> u64 {
        self.keys.values().map(VecDeque::len).sum::<usize>() as u64
    }

    pub fn depth_for(&self, key: &str) -> u64 {
        self.keys.get(key).map_or(0, VecDeque::len) as u64
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::priority::{look_ahead, DEFAULT, LOOK_AHEAD_DEPTH, SYSTEM};

    fn job(key: &str, hash: &str, lane: Lane, priority: Option<i64>) -> Job {
        Job { key: key.into(), hash: hash.into(), text: hash.into(), lane, priority, attempt: 0 }
    }

    /// A queue with the named keys at (default priority, discipline).
    fn queue(keys: &[(&str, i64, Queueing)]) -> Queue {
        let mut q = Queue::new();
        for (k, p, d) in keys {
            q.set_order(k, KeyOrder { priority: *p, queueing: *d });
        }
        q
    }

    fn content() -> Queue {
        queue(&[("content", 50, Queueing::Fifo)])
    }

    fn warm(q: &mut Queue, key: &str, hashes: &[&str], p: Option<i64>) {
        for h in hashes {
            assert!(q.push(job(key, h, Lane::Warm, p)).is_empty());
        }
    }

    fn drain(q: &mut Queue) -> Vec<String> {
        std::iter::from_fn(|| q.pop()).map(|j| j.hash).collect()
    }

    // ── the shipped orderings; equal priority degrades exactly to them ─────

    #[test]
    fn fifo_serves_oldest_first_one_job_per_pop() {
        let mut q = content();
        warm(&mut q, "content", &["a", "b", "c", "d"], None);
        assert_eq!(q.pop().unwrap().hash, "a");
        assert_eq!((q.depth(), q.position("b")), (3, Some(0)), "one pop took more than one job");
        assert_eq!(drain(&mut q), ["b", "c", "d"]);
        assert!(q.pop().is_none() && q.is_empty());
    }

    #[test]
    fn lifo_serves_newest_first() {
        let mut q = queue(&[("content", 50, Queueing::Lifo)]);
        warm(&mut q, "content", &["a", "b", "c"], None);
        assert_eq!(drain(&mut q), ["c", "b", "a"]);
    }

    #[test]
    fn a_content_burst_does_not_starve_the_interface_key() {
        let mut q = queue(&[("content", 50, Queueing::Fifo), ("interface", 100, Queueing::Fifo)]);
        for i in 0..400 {
            warm(&mut q, "content", &[&format!("c{i}")], None);
        }
        warm(&mut q, "interface", &["menu"], None);
        assert_eq!(q.pop().unwrap().hash, "menu");
    }

    #[test]
    fn a_blocked_caller_jumps_the_warm_backlog_without_reordering_it() {
        let mut q = content();
        warm(&mut q, "content", &["a", "b", "c"], None);
        assert!(q.push(job("content", "now", Lane::Interactive, None)).is_empty());
        assert_eq!((q.position("now"), q.position("a"), q.position("c")), (Some(0), Some(1), Some(3)));
        assert_eq!(q.position("missing"), None);
        assert_eq!(drain(&mut q), ["now", "a", "b", "c"]);
    }

    #[test]
    fn a_warm_item_somebody_starts_waiting_on_is_promoted() {
        let mut q = content();
        warm(&mut q, "content", &["a", "b", "c"], None);
        assert!(q.promote("content", "c"));
        assert!(!q.promote("content", "c"), "already interactive");
        assert!(!q.promote("content", "nope"));
        assert_eq!(drain(&mut q), ["c", "a", "b"]);
    }

    // ── per-item priority and the single consumer ─────────────────────────

    #[test]
    fn a_later_higher_priority_item_is_served_first_and_equals_stay_fifo() {
        let mut q = content();
        warm(&mut q, "content", &["a", "b", "c"], Some(4));
        warm(&mut q, "content", &["urgent"], Some(6));
        warm(&mut q, "content", &["d"], Some(4));
        assert_eq!(drain(&mut q), ["urgent", "a", "b", "c", "d"]);
    }

    #[test]
    fn the_look_ahead_ladder_serves_nearest_first_and_leaves_the_backlog_in_order() {
        let mut q = content();
        for i in 0..50 {
            warm(&mut q, "content", &[&format!("bulk{i}")], None);
        }
        for d in [3u32, 5, 1, 4, 2] {
            warm(&mut q, "content", &[&format!("m{d}")], Some(50 + look_ahead(d)));
        }
        let got = drain(&mut q);
        assert_eq!(&got[..6], ["m1", "m2", "m3", "m4", "m5", "bulk0"]);
    }

    #[test]
    fn a_duplicate_takes_the_max_of_the_two_priorities_and_never_a_second_slot() {
        let mut q = content();
        warm(&mut q, "content", &["later"], Some(0));
        warm(&mut q, "content", &["a", "b"], Some(2));
        assert_eq!(q.raise_priority("content", "later", Some(6)), Some(6));
        assert_eq!(q.raise_priority("content", "later", Some(0)), Some(6), "a lower duplicate never demotes");
        assert_eq!(q.depth(), 3);
        assert_eq!(drain(&mut q), ["later", "a", "b"]);
    }

    #[test]
    fn a_raise_keeps_first_in_order_among_equals() {
        let mut q = content();
        warm(&mut q, "content", &["a", "b", "c"], Some(0));
        q.raise_priority("content", "a", Some(3));
        q.raise_priority("content", "c", Some(3));
        assert_eq!(drain(&mut q), ["a", "c", "b"]);
    }

    #[test]
    fn raising_what_is_not_queued_reports_nothing() {
        let mut q = content();
        warm(&mut q, "content", &["a"], Some(0));
        assert_eq!(q.raise_priority("content", "gone", Some(9)), None);
        assert_eq!(q.raise_priority("nokey", "a", Some(9)), None);
        assert_eq!(q.raise_priority("content", "a", None), Some(50), "None means the key's default");
    }

    #[test]
    fn the_cache_default_applies_when_unstated_and_a_stated_priority_replaces_it() {
        let mut q = queue(&[("interface", SYSTEM, Queueing::Fifo), ("content", DEFAULT, Queueing::Fifo)]);
        warm(&mut q, "content", &["message"], None);
        warm(&mut q, "interface", &["label"], None);
        warm(&mut q, "content", &["next"], Some(look_ahead(LOOK_AHEAD_DEPTH)));
        assert_eq!(q.priority_of("content", "message"), Some(0));
        assert_eq!(q.priority_of("interface", "label"), Some(1));
        assert_eq!(q.priority_of("content", "next"), Some(2), "replaced, not stacked on the default");
        assert_eq!(drain(&mut q), ["next", "label", "message"]);
    }

    #[test]
    fn two_keys_at_one_priority_interleave_by_arrival() {
        let mut q = queue(&[("alpha", 0, Queueing::Fifo), ("beta", 0, Queueing::Fifo)]);
        for (k, h) in [("alpha", "a1"), ("beta", "b1"), ("alpha", "a2"), ("beta", "b2")] {
            warm(&mut q, k, &[h], None);
        }
        assert_eq!(drain(&mut q), ["a1", "b1", "a2", "b2"]);
    }

    #[test]
    fn a_lifo_key_and_a_fifo_key_keep_their_own_disciplines_under_one_consumer() {
        let mut q = queue(&[("newest", 0, Queueing::Lifo), ("oldest", 0, Queueing::Fifo)]);
        warm(&mut q, "newest", &["n1", "n2", "n3"], None);
        warm(&mut q, "oldest", &["o1", "o2", "o3"], None);
        let got = drain(&mut q);
        let n: Vec<&str> = got.iter().map(String::as_str).filter(|h| h.starts_with('n')).collect();
        let o: Vec<&str> = got.iter().map(String::as_str).filter(|h| h.starts_with('o')).collect();
        assert_eq!((n, o), (vec!["n3", "n2", "n1"], vec!["o1", "o2", "o3"]));
    }

    #[test]
    fn lifo_breaks_ties_and_does_not_override_priority() {
        let mut q = queue(&[("content", 0, Queueing::Lifo)]);
        warm(&mut q, "content", &["old-but-urgent"], Some(6));
        warm(&mut q, "content", &["a", "b"], Some(0));
        assert_eq!(drain(&mut q), ["old-but-urgent", "b", "a"]);
    }

    #[test]
    fn a_blocked_caller_outranks_the_top_of_the_ladder() {
        let mut q = content();
        for d in 1..=5u32 {
            warm(&mut q, "content", &[&format!("m{d}")], Some(look_ahead(d)));
        }
        assert!(q.push(job("interface", "pressed", Lane::Interactive, Some(0))).is_empty());
        assert_eq!(q.pop().unwrap().hash, "pressed");
    }

    /// `position` and `pop` read one order; checked on a mixed queue.
    #[test]
    fn position_never_disagrees_with_what_pop_serves() {
        let mut q = queue(&[("a", 1, Queueing::Lifo), ("b", 0, Queueing::Fifo)]);
        for i in 0..6 {
            warm(&mut q, "a", &[&format!("a{i}")], Some(i % 3));
            warm(&mut q, "b", &[&format!("b{i}")], None);
        }
        assert!(q.push(job("b", "waited-on", Lane::Interactive, Some(0))).is_empty());
        let expected: Vec<String> = q.serve_order().iter().map(|x| x.job.hash.clone()).collect();
        for (n, h) in expected.iter().enumerate() {
            assert_eq!(q.position(h), Some(n as u64), "{h}");
        }
        assert_eq!(drain(&mut q), expected);
    }

    // ── the bound ──────────────────────────────────────────────────────────

    #[test]
    fn the_bound_holds_and_hands_back_what_it_dropped() {
        let mut q = Queue::new();
        let mut dropped = Vec::new();
        for i in 0..(MAX_QUEUED * 2) {
            dropped.extend(q.push(job("content", &format!("h{i}"), Lane::Warm, None)));
        }
        assert_eq!(q.depth() as usize, MAX_QUEUED);
        assert_eq!(dropped.len(), MAX_QUEUED, "every eviction is returned to its caller");
    }

    #[test]
    fn eviction_drops_the_lowest_rung_and_keeps_the_nearest() {
        let mut q = Queue::new();
        assert!(q.push(job("content", "next-up", Lane::Warm, Some(6))).is_empty());
        for i in 0..(MAX_QUEUED * 2) {
            let _ = q.push(job("content", &format!("stale{i}"), Lane::Warm, Some(2)));
        }
        assert_eq!(q.pop().unwrap().hash, "next-up");
    }

    /// The forcing that matters is a queue over the bound on BLOCKED CALLERS
    /// alone: a warm flood can never reach an interactive item by rank, so a
    /// mixed queue cannot tell whether the lane filter is there. (The
    /// substrate test used the mixed queue and stayed green with the filter
    /// deleted; this one goes red.)
    #[test]
    fn eviction_never_drops_a_blocked_caller_even_over_the_bound() {
        let mut q = Queue::new();
        for i in 0..=MAX_QUEUED {
            let dropped = q.push(job("content", &format!("waiting{i}"), Lane::Interactive, Some(0)));
            assert!(dropped.is_empty(), "evicted a caller somebody is blocked on");
        }
        assert_eq!(q.depth() as usize, MAX_QUEUED + 1, "the excess is the backpressure");
        let dropped = q.push(job("content", "warm", Lane::Warm, Some(6)));
        assert_eq!(dropped.iter().map(|j| j.hash.as_str()).collect::<Vec<_>>(), ["warm"]);
    }

    // ── the permit reserve ─────────────────────────────────────────────────

    #[test]
    fn a_reserved_permit_sees_no_warm_work_and_loses_none() {
        let mut q = content();
        for i in 0..50 {
            warm(&mut q, "content", &[&format!("w{i}")], Some(6));
        }
        assert!(q.pop_where(true).is_none());
        assert_eq!(q.depth(), 50);
        assert!(q.pop_where(false).is_some());
    }

    #[test]
    fn the_row_he_pressed_comes_out_first_under_a_full_warm_queue() {
        let mut q = content();
        for i in 0..(MAX_QUEUED - 1) {
            warm(&mut q, "content", &[&format!("w{i}")], Some(6));
        }
        assert!(q.push(job("content", "pressed", Lane::Interactive, Some(0))).is_empty());
        assert_eq!(q.pop_where(true).map(|j| j.hash).as_deref(), Some("pressed"));
    }

    #[test]
    fn remove_takes_one_named_item_and_leaves_the_rest() {
        let mut q = content();
        warm(&mut q, "content", &["a", "b", "c"], None);
        assert_eq!(q.remove("content", "b").map(|j| j.hash).as_deref(), Some("b"));
        assert!(q.remove("content", "b").is_none());
        assert!(q.remove("other", "a").is_none());
        assert_eq!(drain(&mut q), ["a", "c"]);
    }
}
