//! # What `store::db_trips()` actually measures, pinned against the source
//!
//! `DB_TRIPS` is incremented once per `Store::conn()` and once per
//! `Store::read()` — a CONNECTION ACQUISITION. Its own doc claimed more than
//! that: *"every store method but two runs EXACTLY ONE SQL statement, so this is
//! the statement count."* It is not the statement count, and it was false for
//! ELEVEN methods rather than two.
//!
//! The cost that found it: `summary`'s hydrate calls `newest_app_events`, which
//! ran the same group-by over `events` TWICE — once wrapped in `COUNT(*)` for the
//! total, once for the page. Neither can use `idx_events_thread_kind`
//! (`(thread_id, kind)`) for a fleet-wide read on `kind`, so both were full
//! scans, ~3.5 s each cold on the 2.46M-row production store. `summary.rs` said
//! "one statement", `store.rs` said the counter proved it, and every cost test in
//! the repo agreed with the wrong number — because one acquisition is what they
//! all counted.
//!
//! ## Why this test scrapes the source
//!
//! **The runtime counter cannot measure what this test asserts.** A statement
//! counter would mean touching every statement site in a 4,700-line file, and a
//! wall-clock bound measures the page cache. So the claim is pinned where it is
//! made: this test parses `src/store.rs`, counts acquisitions and statements per
//! method, and asserts the set where they disagree is EXACTLY the pinned list —
//! the same technique `ws_protocol.rs::the_ws_method_inventory_is_pinned` uses
//! for the method surface, and for the same reason: a claim no machine checks is
//! a claim that drifts.

use std::collections::BTreeMap;

const SRC: &str = include_str!("../../src/store.rs");

/// A STATEMENT is one piece of SQL handed to SQLite. `prepare`/`prepare_cached`
/// counts once and the `query_map`/`query` that runs it does not — they are the
/// same statement — while `execute`, `query_row`, `execute_batch` and the pragma
/// helpers each carry their own SQL.
const STATEMENT_CALLS: &[&str] = &[
    ".execute(",
    ".execute_batch(",
    ".query_row(",
    ".prepare(",
    ".prepare_cached(",
    ".pragma_update(",
    ".pragma_query_value(",
];

/// Every method that acquires a connection, with `(acquisitions, statements)`.
fn methods() -> BTreeMap<String, (usize, usize)> {
    let lines: Vec<&str> = SRC.lines().collect();
    let mut out = BTreeMap::new();
    let mut i = 0usize;
    while i < lines.len() {
        // Impl-level methods only: exactly four spaces, then an optional `pub`.
        let t = lines[i];
        let head = t.strip_prefix("    ").unwrap_or("");
        let after_pub = head
            .strip_prefix("pub(crate) ")
            .or_else(|| head.strip_prefix("pub "))
            .unwrap_or(head);
        let Some(rest) = after_pub.strip_prefix("fn ") else {
            i += 1;
            continue;
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
            .collect();
        if name.is_empty() || t.starts_with("     ") {
            i += 1;
            continue;
        }
        // The body, by brace depth.
        let mut depth = 0i32;
        let mut started = false;
        let mut body: Vec<&str> = Vec::new();
        let mut j = i;
        while j < lines.len() {
            depth += lines[j].matches('{').count() as i32;
            depth -= lines[j].matches('}').count() as i32;
            body.push(lines[j]);
            if lines[j].contains('{') {
                started = true;
            }
            if started && depth <= 0 {
                break;
            }
            j += 1;
        }
        let code: Vec<&&str> = body
            .iter()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect();
        let acq: usize = code
            .iter()
            .map(|l| l.matches("self.conn()").count() + l.matches("self.read()").count())
            .sum();
        let stmts: usize = code
            .iter()
            .map(|l| {
                STATEMENT_CALLS
                    .iter()
                    .map(|c| l.matches(c).count())
                    .sum::<usize>()
            })
            .sum();
        if acq > 0 {
            out.insert(name, (acq, stmts));
        }
        i = j + 1;
    }
    out
}

/// **THE METHODS `db_trips()` UNDERCOUNTS, PINNED.**
///
/// These run more SQL statements than they acquire connections, so each of them
/// adds ONE to `DB_TRIPS` and issues more than one statement. Every one is
/// deliberate; the list exists so nothing joins it silently, and so the
/// counter's own doc can name them without going stale.
const UNDERCOUNTED: &[(&str, usize, usize)] = &[
    // At open only, never on a request path.
    // R9.C7h: plus the "did this store exist before this open" read that
    // decides whether a migration is announced as `migrating`.
    ("apply_schema", 1, 4),
    ("migrate_share_zero_disable", 1, 2),
    ("rekey_pool_shares_on_name", 1, 2),
    // R10.M1: the queue's two records — column add, backfill and the
    // `cancelled` sweep, in one transaction at open.
    ("migrate_queue_two_records", 1, 10),
    // Request paths. Each is a read plus its own count or its own follow-up.
    ("query_messages", 1, 2),
    ("stranded_deliveries", 1, 2),
    ("reselect_failed", 1, 2),
    ("record_turn_usage", 1, 2),
    ("claim_notice", 1, 4),
    ("set_account_disabled", 1, 2),
    ("account_tokens_between", 1, 2),
    // The delivery clock (an internal thread). Retry/failure paths only, once per
    // attempt: a read of the recorded clock, then at most one write (and a
    // conditional id-list update for a folded-in delivery).
    ("begin_delivery_attempt", 1, 3),
    ("note_delivery_failure", 1, 2),
];

/// **`DB_TRIPS` COUNTS ACQUISITIONS, AND THE METHODS WHERE THAT IS NOT THE
/// STATEMENT COUNT ARE EXACTLY THE PINNED ONES.**
///
/// `mark_seen` is deliberately NOT here: it acquires twice for two statements,
/// which is 1:1 and is what the counter's old doc called its second exception.
///
/// **THE MUTATION.** Add a second statement to any method not on the list — or,
/// for the case this was found by, put `newest_app_events` back to its two-scan
/// shape:
/// ```text
/// let total: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM ({PER_THREAD_NEWEST})"), …)?;
/// let mut stmt = conn.prepare(&format!("… WHERE id IN ({PER_THREAD_NEWEST}) …"))?;
/// ```
///
/// Forced red:
/// ```text
/// thread 'db_trips_counts_acquisitions_and_the_gap_is_exactly_these_methods' panicked:
/// `store.rs` has methods running more SQL statements than DB_TRIPS counts that
/// are NOT in the pinned list. Each one makes every "N statements" bound that
/// reaches it an undercount. Add it with its reason, or collapse the statements.
/// unpinned: [("newest_app_events", 1, 2)]
/// ```
#[test]
fn db_trips_counts_acquisitions_and_the_gap_is_exactly_these_methods() {
    let m = methods();
    assert!(
        m.len() > 100,
        "the parser found only {} acquiring methods, which means it stopped \
         matching `store.rs` — a green here would be nothing ran",
        m.len()
    );

    let pinned: BTreeMap<&str, (usize, usize)> = UNDERCOUNTED
        .iter()
        .map(|(n, a, s)| (*n, (*a, *s)))
        .collect();

    let found: Vec<(String, usize, usize)> = m
        .iter()
        .filter(|(_, (a, s))| s > a)
        .map(|(n, (a, s))| (n.clone(), *a, *s))
        .collect();

    let unpinned: Vec<&(String, usize, usize)> = found
        .iter()
        .filter(|(n, _, _)| !pinned.contains_key(n.as_str()))
        .collect();
    assert!(
        unpinned.is_empty(),
        "`store.rs` has methods running more SQL statements than DB_TRIPS counts \
         that are NOT in the pinned list. Each one makes every \"N statements\" \
         bound that reaches it an undercount. Add it with its reason, or collapse \
         the statements. unpinned: {unpinned:?}"
    );

    for (name, acq, stmts) in UNDERCOUNTED {
        let (a, s) = m.get(*name).copied().unwrap_or_else(|| {
            panic!("`{name}` is pinned here and no longer acquires a connection — \
                    delete the entry rather than leaving a pin on nothing")
        });
        assert_eq!(
            (a, s),
            (*acq, *stmts),
            "`{name}`'s statement count moved: pinned {acq} trip(s)/{stmts} \
             statement(s), source says {a}/{s}"
        );
    }

    // ── THE ONE THAT WAS FIXED, NAMED ────────────────────────────────────────
    //
    // `summary`'s hydrate is this read, and both `summary.rs` and the cost
    // meter's doc claimed it was one statement while it was two full scans of
    // `events`. The claim is now true, and this is what keeps it true.
    assert_eq!(
        m.get("newest_app_events").copied(),
        Some((1, 1)),
        "`newest_app_events` is `summary`'s whole hydrate and it must be ONE \
         statement: it cannot use `idx_events_thread_kind` for a fleet-wide read \
         on `kind`, so every statement here is a full scan of `events` (~3.5 s \
         cold on the production store) and `DB_TRIPS` shows one trip either way"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// And the behaviour the collapse had to preserve
// ─────────────────────────────────────────────────────────────────────────────

/// **`COUNT(*) OVER ()` RIDES ON A ROW, SO THE EMPTY CASE HAD TO BE RESTATED.**
///
/// The old shape got `total` from a separate `COUNT(*)`, which answers `0` for an
/// empty table. A window function is computed per row, so an empty result set
/// carries no `total` at all — the one thing the collapse could silently get
/// wrong, and the one thing `thread_shapes.rs`'s cap test (501 events, 500 rows)
/// cannot see.
///
/// **ONE MUTATION CAME BACK GREEN, AND IT IS RECORDED HERE RATHER THAN
/// REPLACED.** `let mut total: i64 = -1;` passes: `Ok((out, total.max(0) as
/// usize))` clamps it, so the clamp is a real second defence and this test
/// cannot see past it. That is worth knowing — a reviewer reading `-1` would
/// expect a panic or a vast `usize` and there is neither.
///
/// **THE MUTATION THAT DOES BITE.** `let mut total: i64 = 1;` — a nonzero
/// default, which is the realistic version of this regression (a count seeded
/// from somewhere other than the rows).
///
/// Forced red:
/// ```text
/// thread 'the_newest_app_event_read_answers_zero_for_a_pool_with_no_named_thread' panicked:
/// assertion `left == right` failed: and the TOTAL is zero rather than absent,
/// inherited or panicking — a window function has no row to ride on here
///   left: 1
///  right: 0
/// ```
#[test]
fn the_newest_app_event_read_answers_zero_for_a_pool_with_no_named_thread() {
    let tmp = tempfile::tempdir().unwrap();
    let store = threads::store::Store::open(&tmp.path().join("t.db")).unwrap();

    let (rows, total) = store.newest_app_events("summary_updated", 500).unwrap();
    assert!(rows.is_empty(), "nothing has been named");
    assert_eq!(
        total, 0,
        "and the TOTAL is zero rather than absent, inherited or panicking — a \
         window function has no row to ride on here"
    );
    drop(tmp);
}
