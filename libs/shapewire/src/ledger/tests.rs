//! Tests for the subscriber ledger.
//!
//! **File-backed rather than inline**, which is the one arrangement decision
//! here. substrate kept its six tests in a `#[cfg(test)] mod tests` at the foot
//! of `ledger.rs`; this suite is large enough that inline would have doubled the
//! length of the file a reader opens to find the module's argument, and keeping
//! it out also keeps the rewrite measurement against substrate honest — the
//! implementation file is compared with an implementation file.
//!
//! # What this suite is built around
//!
//! The ledger ARBITRATES BETWEEN SUBSCRIBERS. Its empty case and its
//! single-subscriber case cannot exercise the thing it exists to manage, so
//! every claim about registration, de-registration, key sets and ordering is
//! made with **two or more** subscribers present and asserts what EACH of them
//! sees — never a count, which is the number substrate's `shapes.list` was able
//! to report while being wrong about who was behind it.
//!
//! Each bound is forced at its REAL constant in both directions:
//! [`LEDGER_RUN_MAX`] at 1,024 (a run of 1,023 must not flush and 1,024 must),
//! [`LEDGER_MAX_BYTES`] at 4 MiB for real through [`ConsumerLedger::attach`]
//! rather than at a convenient small number, and [`LEDGER_GENERATIONS`] at 1
//! (one previous generation survives and the one before it must not).
//!
//! Every test's doc comment names its **MUTATION** — the change to the module
//! that reddens it. They were run; the output is in
//! an internal report in substrate-interface-layer.

use super::*;
use std::time::Duration;

// ─────────────────────────────────────────────────────────────────────────────
// Fixtures
// ─────────────────────────────────────────────────────────────────────────────

/// A consumer that states everything it can.
fn who(name: &str) -> ConsumerIdentity {
    ConsumerIdentity {
        name: name.into(),
        shape_version: "2026-09-20".into(),
        pid: Some(4411),
        build: Some("a1b2c3d".into()),
    }
}

/// The same consumer NAME at a different build — substrate's 2026-09-22
/// incident in one line: two processes agreeing about the contract and
/// disagreeing by three days of code.
fn built(name: &str, build: &str) -> ConsumerIdentity {
    ConsumerIdentity {
        build: Some(build.into()),
        ..who(name)
    }
}

/// A ledger with its journal wired into a fresh temporary directory.
///
/// It goes through [`ledger_path`], so every file-backed test below also
/// exercises the path arithmetic rather than asserting it once in isolation.
struct Wired {
    _dir: tempfile::TempDir,
    ledger: ConsumerLedger,
    path: PathBuf,
}

fn wire(chassis: &str, boot: &str) -> Wired {
    wired(chassis, boot, None)
}

fn wire_with_max(chassis: &str, boot: &str, max_bytes: u64) -> Wired {
    wired(chassis, boot, Some(max_bytes))
}

fn wired(chassis: &str, boot: &str, max_bytes: Option<u64>) -> Wired {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = ledger_path(dir.path(), chassis);
    let ledger = ConsumerLedger::new();
    match max_bytes {
        // The REAL rotation threshold, through the ordinary door.
        None => ledger.attach(path.clone(), chassis, boot, "test-1.0"),
        Some(m) => ledger.attach_with_max_bytes(path.clone(), chassis, boot, "test-1.0", m),
    }
    Wired {
        _dir: dir,
        ledger,
        path,
    }
}

impl Wired {
    /// Everything on disk, flushed first. A reader that asserted without
    /// flushing would be asserting on what is one event away from being there.
    fn records(&self) -> Vec<Value> {
        self.ledger.flush();
        records_at(&self.path)
    }

    fn events(&self) -> Vec<String> {
        self.records()
            .iter()
            .map(|r| event_of(r).to_string())
            .collect()
    }

    fn rotated(&self) -> PathBuf {
        previous_generation_path(&self.path, LEDGER_GENERATIONS)
    }
}

fn records_at(path: &Path) -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .map(|l| {
            serde_json::from_str::<Value>(l).unwrap_or_else(|e| panic!("line is not JSON: {l} ({e})"))
        })
        .collect()
}

fn event_of(record: &Value) -> &str {
    record
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or("<no event field>")
}

/// Every record of one event kind.
fn only<'a>(records: &'a [Value], event: &str) -> Vec<&'a Value> {
    records.iter().filter(|r| event_of(r) == event).collect()
}

fn text(record: &Value, field: &str) -> Option<String> {
    record.get(field).and_then(Value::as_str).map(str::to_string)
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. Identity — rule 2.1's "say who you are", and what it does NOT do
// ─────────────────────────────────────────────────────────────────────────────

/// **ANONYMITY IS REFUSED, IN THE ONE SENTENCE EVERY DOOR SHARES.**
///
/// A substrate ancestor (`an_absent_identity_is_refused_and_so_is_a_blank_one`),
/// split in two and extended: the ancestor asserted that a non-object `consumer`
/// is refused but never that it is refused DIFFERENTLY, and *you sent a string*
/// and *you sent nothing* are different mistakes with different repairs. If the
/// two sentences ever become one, a client that sent a string is told the param
/// exists — which it knew.
///
/// Claim 3 is the POSITIVE SENTINEL, without which the first two pass against a
/// `parse` that refuses everything there is.
///
/// **MUTATIONS.** (1) Make `parse` return `Ok` for an absent `consumer` and the
/// anonymous arm reddens. (2) Point `IDENTITY_SHAPE_REFUSAL` at
/// `IDENTITY_REQUIRED_REFUSAL` and the distinctness arm reddens. (3) Make
/// `parse` return `Err` unconditionally and claim 3 reddens — the mutation the
/// first two cannot see.
#[test]
fn anonymity_is_refused_in_the_one_sentence_every_door_shares() {
    for anonymous in [
        serde_json::json!({"shape": "accounts"}),
        serde_json::json!({"consumer": Value::Null}),
    ] {
        let refusal = ConsumerIdentity::parse(&anonymous).expect_err(
            "rule 2.1: a subscribe that states no consumer must be REFUSED, not \
             served and recorded as UNIDENTIFIED",
        );
        assert_eq!(
            refusal, IDENTITY_REQUIRED_REFUSAL,
            "and in the ONE sentence both doors share, for {anonymous}"
        );
    }

    // A `consumer` that is present but is not an object is a DIFFERENT mistake.
    let wrong_shape = ConsumerIdentity::parse(&serde_json::json!({"consumer": "threads-tui"}))
        .expect_err("a string is not an identity");
    assert_eq!(wrong_shape, IDENTITY_SHAPE_REFUSAL);
    assert_ne!(
        wrong_shape, IDENTITY_REQUIRED_REFUSAL,
        "`you sent a string` and `you sent nothing` need different repairs and \
         must not share a sentence"
    );

    // THE POSITIVE SENTINEL. A suite in which nothing parses cannot tell
    // "refuses anonymity" from "refuses every subscribe there is".
    let stated = ConsumerIdentity::parse(&serde_json::json!({
        "consumer": {"name": "threads-tui", "shape_version": "2026-09-20", "pid": 4411, "build": "a1b2c3d"}
    }))
    .expect("a fully-stated identity must still be ACCEPTED");
    assert_eq!(stated, who("threads-tui"));
}

/// **A HALF-FILLED IDENTITY IS REFUSED, AND SAID TO BE MISSING THE FIELD IT IS
/// MISSING.**
///
/// A substrate ancestor's second claim, extended from `is_err()` to the exact
/// sentence per field. `is_err()` alone cannot tell a `name` refusal from a
/// `shape_version` one, so it passes against a `parse` that blames the wrong
/// field — and the client then fills in the field that was already fine.
///
/// **MUTATION.** Drop `.filter(|s| !s.is_empty())` from `stated_text` and every
/// blank case stops being refused. Swap the two `ok_or_else` sentences and the
/// per-field arms redden while a bare `is_err()` suite stays green.
#[test]
fn a_half_filled_identity_is_refused_and_names_the_field_it_lacks() {
    for missing_name in [
        serde_json::json!({"consumer": {"shape_version": "1"}}),
        serde_json::json!({"consumer": {"name": "", "shape_version": "1"}}),
        serde_json::json!({"consumer": {"name": "   ", "shape_version": "1"}}),
    ] {
        assert_eq!(
            ConsumerIdentity::parse(&missing_name).expect_err("no name is no identity"),
            NAME_REQUIRED_REFUSAL,
            "for {missing_name}"
        );
    }
    for missing_version in [
        serde_json::json!({"consumer": {"name": "x"}}),
        serde_json::json!({"consumer": {"name": "x", "shape_version": ""}}),
        serde_json::json!({"consumer": {"name": "x", "shape_version": "  "}}),
    ] {
        assert_eq!(
            ConsumerIdentity::parse(&missing_version).expect_err("no claim is no identity"),
            SHAPE_VERSION_REQUIRED_REFUSAL,
            "for {missing_version}"
        );
    }
    assert_ne!(
        NAME_REQUIRED_REFUSAL, SHAPE_VERSION_REQUIRED_REFUSAL,
        "two missing fields, two sentences, or the client repairs the wrong one"
    );
    // The sentinel again, at this scope: both halves present is ACCEPTED.
    assert!(ConsumerIdentity::parse(&serde_json::json!({
        "consumer": {"name": "x", "shape_version": "1"}
    }))
    .is_ok());
}

/// **`shape_version` IS FREE TEXT THAT NOTHING COMPARES AND NOTHING
/// NORMALISES.**
///
/// A substrate ancestor (`shape_version_stays_uncompared_free_text`), extended
/// with the two optional fields, which the ancestor never asserted at all: a
/// non-numeric `pid` must be dropped rather than guessed at, and a blank `build`
/// must read as absent for the same reason a blank `name` does.
///
/// Making identity compulsory is about BEING IDENTIFIED. It is not permission to
/// start gating on what a consumer claims about its build — if any of these is
/// ever refused or rewritten, somebody has turned this field into the
/// negotiation the module doc forbids.
///
/// **MUTATION.** Add any comparison of `shape_version` to `parse` — a parse to
/// an integer, a prefix check, a known-value set — and the claim it rejects
/// reddens. Add a `.to_lowercase()` and the verbatim arm reddens.
#[test]
fn shape_version_is_free_text_that_nothing_compares_or_normalises() {
    for claim in [
        "2026-09-20",
        "290113e",
        "v2",
        "whatever-i-was-built-against",
        "1",
        "V2-RC1",
    ] {
        let parsed = ConsumerIdentity::parse(&serde_json::json!({
            "consumer": {"name": "a-consumer", "shape_version": claim}
        }))
        .unwrap_or_else(|e| panic!("`shape_version` is free text and {claim:?} must be accepted: {e}"));
        assert_eq!(
            parsed.shape_version, claim,
            "and recorded VERBATIM, never normalised"
        );
    }

    // Surrounding space is trimmed, because a trailing newline off a config file
    // is not a different consumer.
    let trimmed = ConsumerIdentity::parse(&serde_json::json!({
        "consumer": {"name": "  threads-tui  ", "shape_version": " v2\n"}
    }))
    .expect("whitespace around a stated field is not a refusal");
    assert_eq!(trimmed.name, "threads-tui");
    assert_eq!(trimmed.shape_version, "v2");

    // The two optional fields: absent, blank and wrongly-typed all read as
    // nothing said, and a pid is never guessed at.
    let sparse = ConsumerIdentity::parse(&serde_json::json!({
        "consumer": {"name": "x", "shape_version": "1", "pid": "4411", "build": "   "}
    }))
    .expect("a wrongly-typed optional is not a refusal");
    assert_eq!(
        sparse.pid, None,
        "a pid that is not a number is not a pid — a guessed one is worse than none"
    );
    assert_eq!(sparse.build, None, "a blank build reads as nothing said");
    // …and the counterpart, so "always None" cannot pass.
    let full = ConsumerIdentity::parse(&serde_json::json!({
        "consumer": {"name": "x", "shape_version": "1", "pid": 7, "build": "deadbee"}
    }))
    .unwrap();
    assert_eq!(full.pid, Some(7));
    assert_eq!(full.build.as_deref(), Some("deadbee"));
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. The registry at N >= 2 — the centre of this module
// ─────────────────────────────────────────────────────────────────────────────

/// **TWO CONSUMERS ON ONE SHAPE ARE TWO ROWS, EACH NAMING ITSELF.**
///
/// The `subscribers: 2` that substrate's `shapes.list` could report without
/// saying who either was is exactly the answer this asserts is not enough. So
/// nothing here asserts a count alone: each row is named, by connection, and
/// each field a human would read off it is checked on the row it belongs to.
///
/// Also the N = 2 form of *shapes do not bleed*: a third consumer on a second
/// shape must not appear in the first shape's answer.
///
/// **MUTATION.** Make `Registry::rows_for` drop its `.filter(|((_, s), _)| s ==
/// shape)` and the bleed arm reddens. Have `admit` key the registry on `conn`
/// alone and the two rows collapse into one.
#[test]
fn two_consumers_on_one_shape_are_two_rows_each_naming_itself() {
    let l = ConsumerLedger::new();
    l.connected("accounts", Some("cc1"), 7, Some(built("threads-tui", "old")));
    l.connected("accounts", Some("cc2"), 9, Some(built("aui", "new")));
    l.connected("budgets", None, 11, Some(who("a-reader")));

    let rows = l.consumers("accounts");
    assert_eq!(rows.len(), 2, "two consumers, two rows");

    let first = rows.iter().find(|r| r.conn == 7).expect("conn 7 is listed");
    assert_eq!(first.name.as_deref(), Some("threads-tui"));
    assert_eq!(first.build.as_deref(), Some("old"));
    assert_eq!(first.keys, vec!["cc1".to_string()]);
    assert!(!first.keyless);

    let second = rows.iter().find(|r| r.conn == 9).expect("conn 9 is listed");
    assert_eq!(second.name.as_deref(), Some("aui"));
    assert_eq!(
        second.build.as_deref(),
        Some("new"),
        "the whole point of `build`: two consumers the contract string cannot tell apart"
    );
    assert_eq!(second.keys, vec!["cc2".to_string()]);

    // Shapes do not bleed, asserted at N = 2 rather than against an empty
    // registry, where it cannot fail.
    assert!(
        !l.consumers("accounts").iter().any(|r| r.conn == 11),
        "the budgets subscriber must not appear under accounts"
    );
    let other = l.consumers("budgets");
    assert_eq!(other.len(), 1);
    assert_eq!(other[0].conn, 11);
}

/// **ONE CONSUMER LEAVING LEAVES THE SURVIVOR EXACTLY AS IT WAS.**
///
/// The two halves are asserted separately and reddened separately, because they
/// fail separately: a `disconnected` that removed too much reddens the SURVIVOR
/// arm, and one that removed nothing reddens the DEPARTED arm. A test that only
/// counted rows would pass against either.
///
/// `since` is on the survivor's row deliberately. It is where *"three days
/// old"* is read, so a de-registration that reset the survivor's age would have
/// erased the one number that identifies a stale consumer — while leaving the
/// row count right.
///
/// **MUTATION.** Have `Registry::remove` clear the whole map and the survivor
/// arm reddens. Have it return `None` without removing and the departed arm
/// reddens. Have `admit` recompute `since` for untouched entries and the age arm
/// reddens.
#[test]
fn one_consumer_leaving_leaves_the_survivor_exactly_as_it_was() {
    let l = ConsumerLedger::new();
    l.connected("accounts", Some("cc1"), 7, Some(who("stays")));
    l.connected("accounts", Some("cc2"), 9, Some(who("goes")));
    let survivor_before = l
        .consumers("accounts")
        .into_iter()
        .find(|r| r.conn == 7)
        .expect("conn 7 subscribed");

    l.disconnected("accounts", 9, CloseReason::Unsubscribed);
    let rows = l.consumers("accounts");

    // THE SURVIVOR — asserted whole, not counted.
    let survivor = rows.iter().find(|r| r.conn == 7).expect(
        "the consumer that did not leave must still be subscribed: a de-registration \
         that takes the neighbour with it is the defect this arbitrates",
    );
    assert_eq!(survivor.name.as_deref(), Some("stays"));
    assert_eq!(survivor.keys, vec!["cc1".to_string()]);
    assert_eq!(
        survivor.since, survivor_before.since,
        "`since` is the survivor's OWN age and another consumer's departure is not \
         an event in its life"
    );

    // THE DEPARTED — its absence is asserted, not inferred from the count.
    assert!(
        !rows.iter().any(|r| r.conn == 9),
        "the consumer that unsubscribed must be GONE from the registry immediately — \
         `shapes.list` must never report a subscription that has ended"
    );
    assert_eq!(rows.len(), 1);
}

/// **THREE EVENTS, SO THE REGISTRY'S VIEW IS DISTINGUISHABLE FROM A PAIRING.**
///
/// Two subscribers cannot tell *the registry tracks each consumer* from *the
/// registry holds whichever two it last saw*. A third arriving and leaving
/// between the first two's lifetimes can: the assertion is on the POPULATION at
/// each of the three steps, by connection id.
///
/// **MUTATION.** Have `admit` overwrite the map rather than insert into it and
/// step 2 reddens. Have `Registry::remove` take the lowest-keyed entry rather
/// than the named one and step 3 reddens.
#[test]
fn the_population_is_tracked_across_three_arrivals_and_a_departure() {
    let l = ConsumerLedger::new();
    let conns = |l: &ConsumerLedger| -> Vec<ConnId> {
        l.consumers("accounts").into_iter().map(|r| r.conn).collect()
    };

    l.connected("accounts", Some("cc1"), 7, Some(who("first")));
    assert_eq!(conns(&l), vec![7]);
    l.connected("accounts", Some("cc1"), 9, Some(who("second")));
    assert_eq!(conns(&l), vec![7, 9]);
    l.connected("accounts", Some("cc1"), 11, Some(who("third")));
    assert_eq!(conns(&l), vec![7, 9, 11], "three subscribers, three rows");

    l.disconnected("accounts", 9, CloseReason::ConnectionClosed);
    assert_eq!(
        conns(&l),
        vec![7, 11],
        "the one that left is gone and NEITHER neighbour went with it"
    );
}

/// **WIDENING ONE SUBSCRIPTION DOES NOT TOUCH ANOTHER'S KEYS.**
///
/// The key set is per subscription, and the only way to see that is with two of
/// them. A single-subscriber test of widening passes against a key set stored
/// per SHAPE.
///
/// **MUTATION.** Key `Registry.0` on `shape` alone and both consumers end up
/// holding `cc1`, `cc2` and `cc3`.
#[test]
fn widening_one_subscription_does_not_touch_another_consumers_keys() {
    let l = ConsumerLedger::new();
    l.connected("accounts", Some("cc1"), 7, Some(who("widens")));
    l.connected("accounts", Some("cc3"), 9, Some(who("sits-still")));

    l.connected("accounts", Some("cc2"), 7, Some(who("widens")));

    let rows = l.consumers("accounts");
    let widened = rows.iter().find(|r| r.conn == 7).unwrap();
    assert_eq!(
        widened.keys,
        vec!["cc1".to_string(), "cc2".to_string()],
        "both keys — a scalar could hold only the last one named"
    );
    let still = rows.iter().find(|r| r.conn == 9).unwrap();
    assert_eq!(
        still.keys,
        vec!["cc3".to_string()],
        "the OTHER consumer's interest is its own and nothing happened to it"
    );
}

/// **DROPPING ONE CONNECTION CLOSES ITS OWN SHAPES AND NOBODY ELSE'S.**
///
/// `dropped` is the only call that closes more than one entry, so it is the one
/// with something to be wrong about. Two shapes on the dying connection and one
/// of those shapes also held by a surviving connection is the smallest
/// arrangement in which "close everything for this conn" and "close everything
/// on these shapes" give different answers.
///
/// **MUTATION.** Have `Registry::shapes_on` drop its `.filter(|(c, _)| *c ==
/// conn)` and the survivor arm reddens. Have it return an empty list and the
/// departed arms redden.
#[test]
fn dropping_one_connection_closes_its_own_shapes_and_nobody_elses() {
    let l = ConsumerLedger::new();
    l.connected("accounts", Some("cc1"), 7, Some(who("dying")));
    l.connected("budgets", None, 7, Some(who("dying")));
    l.connected("accounts", Some("cc9"), 9, Some(who("surviving")));

    l.dropped(7);

    let accounts = l.consumers("accounts");
    assert_eq!(accounts.len(), 1, "one of the two accounts subscribers died");
    assert_eq!(accounts[0].conn, 9);
    assert_eq!(accounts[0].keys, vec!["cc9".to_string()]);
    assert!(
        l.consumers("budgets").is_empty(),
        "every shape the dead connection held closes, not just the first"
    );
}

/// **TWO SOCKETS CLAIMING ONE IDENTITY ARE TWO SUBSCRIPTIONS.**
///
/// Two processes of the same build, or one process that reconnected without the
/// old socket having closed yet, are two rows — and this is the case substrate
/// REFUSED to collapse: *"two subscriptions on two different connections are not
/// knowably the same consumer, and merging them would make the ledger assert
/// something it does not know."* The identity is not a key.
///
/// **MUTATION.** Add the identity to the registry's key, or de-duplicate
/// `rows_for` by name, and the second row disappears.
#[test]
fn two_sockets_claiming_one_identity_are_two_subscriptions() {
    let l = ConsumerLedger::new();
    l.connected("accounts", Some("cc1"), 7, Some(who("threads-tui")));
    l.connected("accounts", Some("cc1"), 9, Some(who("threads-tui")));

    let rows = l.consumers("accounts");
    assert_eq!(rows.len(), 2, "one identity, two sockets, TWO subscriptions");
    assert_eq!(rows[0].conn, 7);
    assert_eq!(rows[1].conn, 9);
    assert!(rows.iter().all(|r| r.name.as_deref() == Some("threads-tui")));
}

/// **OLDEST CONNECTION FIRST**, which is the order a human reads the column in.
///
/// Three connections, subscribed in an order that is NOT their id order, so the
/// assertion distinguishes *sorted by connection* from *insertion order* — two
/// would not.
///
/// **MUTATION.** Swap `Registry.0` for a `HashMap` and this fails
/// non-deterministically; reverse `rows_for`'s iteration and it fails every
/// time.
#[test]
fn the_registry_lists_consumers_oldest_connection_first() {
    let l = ConsumerLedger::new();
    for conn in [9, 3, 7] {
        l.connected("accounts", Some("cc1"), conn, Some(who("c")));
    }
    let order: Vec<ConnId> = l.consumers("accounts").into_iter().map(|r| r.conn).collect();
    assert_eq!(
        order,
        vec![3, 7, 9],
        "ascending connection id, which is arrival order — not the order they \
         happened to be inserted in"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. The key set — added to, never overwritten
// ─────────────────────────────────────────────────────────────────────────────

/// **A WIDENED SUBSCRIPTION REPORTS EVERY KEY IT HOLDS, AND A KEYLESS SUBSCRIBE
/// IS NOT A KEY CALLED `null`.**
///
/// A substrate ancestor (`a_widened_subscription_reports_every_key_it_holds`),
/// kept whole: `LiveEntry.key` was a scalar and was overwritten, so `shapes.list`
/// reported this consumer as watching `cc2` while the hub was quite correctly
/// sending it both.
///
/// **MUTATION.** Replace `keys.insert(member)` with a fresh set containing only
/// `member` and the first arm reddens. Put the keyless member into
/// `ConsumerEntry::keys` as a `null`, or drop the `keyless` field's read, and the
/// second arm reddens.
#[test]
fn a_widened_subscription_reports_every_key_and_keyless_is_its_own_ask() {
    let l = ConsumerLedger::new();
    let it = Some(who("threads-tui"));
    l.connected("accounts", Some("cc1"), 7, it.clone());
    l.connected("accounts", Some("cc2"), 7, it.clone());

    let rows = l.consumers("accounts");
    assert_eq!(rows.len(), 1, "still ONE subscription");
    assert_eq!(rows[0].keys, vec!["cc1".to_string(), "cc2".to_string()]);
    assert!(!rows[0].keyless);

    // …and a keyless subscribe on top is its own member, not a key spelled null.
    l.connected("accounts", None, 7, it.clone());
    let rows = l.consumers("accounts");
    assert_eq!(
        rows[0].keys,
        vec!["cc1".to_string(), "cc2".to_string()],
        "`every key you know about` is not a key in the list"
    );
    assert!(rows[0].keyless, "it is its own ask, and it is set");
}

/// **A DIFFERENT CONSUMER ON ONE SOCKET DOES NOT INHERIT THE KEYS OR THE AGE.**
///
/// A substrate ancestor (`subscribing_twice_replaces_rather_than_duplicating`).
/// A different build on one socket is a different SUBSCRIPTION: letting it
/// inherit the old one's key set would report today's consumer as watching what
/// yesterday's did, which is the same class of lie as inheriting its `since`.
///
/// **MUTATION.** Make `continuing` in `Registry::admit` ignore the identity (`let
/// continuing = resumes || held.is_some()`) and both the keyless arm and the key
/// arm redden.
#[test]
fn a_different_consumer_on_one_socket_inherits_nothing() {
    let l = ConsumerLedger::new();
    l.connected("accounts", None, 7, Some(built("threads-tui", "three-days-old")));
    l.connected("accounts", Some("cc1"), 7, Some(built("threads-tui", "today")));

    let rows = l.consumers("accounts");
    assert_eq!(rows.len(), 1, "one connection subscribed twice is ONE row");
    assert_eq!(rows[0].build.as_deref(), Some("today"));
    assert_eq!(rows[0].keys, vec!["cc1".to_string()]);
    assert!(
        !rows[0].keyless,
        "a DIFFERENT build on this socket starts fresh — it does not inherit the \
         keyless interest the old one had"
    );
}

/// **NARROWING DROPS ONE KEY AND KEEPS THE SUBSCRIPTION**, and says which
/// happened.
///
/// substrate's defect, in its own words: the producer called `disconnected` on
/// EVERY unsubscribe, and `disconnected` takes no key — so `shapes.unsubscribe
/// {shape, key: "b"}` from a connection holding `a` and `b` closed the entry
/// outright and `shapes.list` answered `subscribers: 1, consumers: 0` while key
/// `a`'s frames kept being delivered.
///
/// The return value is the half a caller branches on, so it is asserted in both
/// directions — `false` while keys remain, `true` when the last one goes — and
/// the N = 2 arm is there because the neighbour must not narrow with it.
///
/// **MUTATION.** Have `narrowed` call `disconnected` unconditionally (the
/// original defect) and the still-live arms redden. Have it return `false` always
/// and the last-key arm reddens. Have `Registry::narrow` clear the set and the
/// survivor arm reddens.
#[test]
fn narrowing_drops_one_key_keeps_the_subscription_and_says_which() {
    let l = ConsumerLedger::new();
    l.connected("accounts", Some("cc1"), 7, Some(who("narrows")));
    l.connected("accounts", Some("cc2"), 7, Some(who("narrows")));
    l.connected("accounts", Some("cc1"), 9, Some(who("neighbour")));

    let emptied = l.narrowed("accounts", "cc2", 7, CloseReason::Unsubscribed);
    assert!(!emptied, "a subscription that still holds `cc1` has not ended");
    let rows = l.consumers("accounts");
    assert_eq!(
        rows.iter().find(|r| r.conn == 7).unwrap().keys,
        vec!["cc1".to_string()],
        "one key dropped, the other kept"
    );
    assert_eq!(
        rows.iter().find(|r| r.conn == 9).unwrap().keys,
        vec!["cc1".to_string()],
        "and the neighbour holding the same key name did not narrow with it"
    );

    // The last key IS a close, and the caller is told so.
    let emptied = l.narrowed("accounts", "cc1", 7, CloseReason::Unsubscribed);
    assert!(
        emptied,
        "dropping the last key ends the subscription, and the caller must not have \
         to work out which of the two happened"
    );
    let rows = l.consumers("accounts");
    assert_eq!(rows.len(), 1, "only the neighbour is left");
    assert_eq!(rows[0].conn, 9);
}

/// **NARROWING A KEY A KEYLESS SUBSCRIPTION NEVER NAMED LEAVES IT LIVE.**
///
/// `None` is a member of the key set, so a subscription holding only the keyless
/// interest does not empty when a named key is dropped from it. The arm matters
/// because the keyless member is the one a `remove(&Some(..))` cannot reach, and
/// an implementation that counted named keys alone would close a subscription
/// that is still being fed every key the producer knows.
///
/// **MUTATION.** Make `narrow` report emptiness as `keys.iter().flatten().count()
/// == 0` and this reddens while every named-key test stays green.
#[test]
fn narrowing_a_key_a_keyless_subscription_never_named_leaves_it_live() {
    let l = ConsumerLedger::new();
    l.connected("accounts", None, 7, Some(who("keyless")));

    let emptied = l.narrowed("accounts", "cc1", 7, CloseReason::Unsubscribed);
    assert!(!emptied, "a keyless subscription did not end");
    let rows = l.consumers("accounts");
    assert_eq!(rows.len(), 1);
    assert!(rows[0].keyless, "it still wants every key the producer knows");
    assert!(rows[0].keys.is_empty());
}

/// **NARROWING SOMETHING NEVER SUBSCRIBED CHANGES NOTHING AND WRITES NOTHING.**
///
/// A `narrow` with no `connect` behind it would be a line about a subscription
/// that never was, and the sweep pairs on `connect`/`disconnect` — so the file
/// half matters here as much as the return value. The N = 2 arm is the one that
/// can fail: a neighbour IS subscribed to this shape, so "nothing is subscribed"
/// is not the reason the call is a no-op.
///
/// **MUTATION.** Have `Registry::narrow` insert a fresh entry on a miss and both
/// arms redden.
#[test]
fn narrowing_something_never_subscribed_changes_nothing_and_writes_nothing() {
    let w = wire("threads", "boot-a");
    w.ledger
        .connected("accounts", Some("cc1"), 7, Some(who("neighbour")));

    let emptied = w
        .ledger
        .narrowed("accounts", "cc1", 999, CloseReason::Unsubscribed);
    assert!(!emptied);
    let rows = w.ledger.consumers("accounts");
    assert_eq!(rows.len(), 1, "the registry is untouched");
    assert_eq!(rows[0].conn, 7);
    assert!(
        only(&w.records(), EVENT_NARROW).is_empty(),
        "and no `narrow` line was written for a subscription that never existed"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. `since` — the field "three days old" is read off
// ─────────────────────────────────────────────────────────────────────────────

/// **`since` SURVIVES A RECOVERY CYCLE AND A REPEAT SUBSCRIBE, AND RESETS FOR A
/// DIFFERENT BUILD.**
///
/// Both directions in one test on purpose: an implementation that always keeps
/// `since` passes the first two arms, one that always resets passes the third,
/// and only the pair rules both out.
///
/// **The comparison is between two things that have to be shown DIFFERENT.**
/// `since` is RFC3339 to the millisecond, so two subscribes inside one
/// millisecond produce the same string and a "reset" would be indistinguishable
/// from a "kept". The sentinel asserts the clock actually advanced across each
/// sleep before any `since` is compared — otherwise this test proves that `==` is
/// reflexive.
///
/// **MUTATION.** Delete `Deferred::since` and take `now_rfc3339()` in the `(None,
/// true)` arm and the recovery arm reddens. Make the `_ =>` arm clone the held
/// entry's `since` and the different-build arm reddens.
#[test]
fn since_survives_a_recovery_cycle_and_resets_for_a_different_build() {
    let l = ConsumerLedger::new();
    let tick = || {
        let before = now_rfc3339();
        std::thread::sleep(Duration::from_millis(5));
        let after = now_rfc3339();
        assert_ne!(
            before, after,
            "SENTINEL: the clock must advance across the sleep, or every `since` \
             comparison below is x == x and proves nothing"
        );
    };

    l.connected("accounts", Some("cc1"), 7, Some(built("threads-tui", "old")));
    let opened = l.consumers("accounts")[0].since.clone();

    // 1. THE REPEAT SUBSCRIBE — rule 2.5, one subscription throughout.
    tick();
    l.connected("accounts", Some("cc1"), 7, Some(built("threads-tui", "old")));
    assert_eq!(
        l.consumers("accounts")[0].since, opened,
        "a repeat subscribe is the SAME subscription and its age did not restart"
    );

    // 2. THE RECOVERY CYCLE — unsubscribe then subscribe, which is the consumer
    //    most likely to be cycling and therefore the one whose age matters most.
    tick();
    l.disconnected("accounts", 7, CloseReason::Unsubscribed);
    l.connected("accounts", Some("cc1"), 7, Some(built("threads-tui", "old")));
    assert_eq!(
        l.consumers("accounts")[0].since, opened,
        "`unsubscribe` + `subscribe` is one recovery cycle; resetting `since` here \
         erases the number that identifies a stale consumer"
    );

    // 3. A DIFFERENT BUILD ON THE SAME SOCKET — a different subscription, and it
    //    must NOT inherit three days of age it did not have.
    tick();
    l.connected("accounts", Some("cc1"), 7, Some(built("threads-tui", "today")));
    let replaced = l.consumers("accounts")[0].since.clone();
    assert_ne!(
        replaced, opened,
        "today's build must not be reported as three days old — that is the exact \
         reading the operator needed and the exact way to get it wrong"
    );
    assert!(replaced > opened, "and the new age is later, not earlier");
}

/// **A SURVIVING CONSUMER'S AGE IS NOT RESET BY THE NEIGHBOUR'S RECOVERY
/// CYCLE.**
///
/// The N = 2 form of the rule above. `since` is per subscription, and the only
/// way to see that is to churn one subscription while another sits still.
///
/// **MUTATION.** Have `admit` write its computed `since` to every entry on the
/// shape and this reddens while every single-subscriber `since` test stays
/// green.
#[test]
fn a_neighbours_recovery_cycle_does_not_reset_a_surviving_consumers_age() {
    let l = ConsumerLedger::new();
    l.connected("accounts", Some("cc1"), 7, Some(who("sits-still")));
    let still_since = l.consumers("accounts")[0].since.clone();

    let before = now_rfc3339();
    std::thread::sleep(Duration::from_millis(5));
    assert_ne!(before, now_rfc3339(), "SENTINEL: the clock advanced");

    for _ in 0..3 {
        l.connected("accounts", Some("cc2"), 9, Some(who("cycles")));
        l.disconnected("accounts", 9, CloseReason::ConnectionClosed);
    }
    l.connected("accounts", Some("cc2"), 9, Some(who("cycles")));

    let rows = l.consumers("accounts");
    assert_eq!(
        rows.iter().find(|r| r.conn == 7).unwrap().since,
        still_since,
        "the quiet consumer's age is its own and the neighbour's fit is not an \
         event in its life"
    );
    assert!(
        rows.iter().find(|r| r.conn == 9).unwrap().since >= still_since,
        "and the cycling one is no older than the quiet one"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. The journal opens lazily, and every line says which generation wrote it
// ─────────────────────────────────────────────────────────────────────────────

/// Write lines into a journal file by hand, as a dead generation would have left
/// them.
fn seed(path: &Path, lines: &[Value]) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(path, body).expect("seed");
}

/// A `connect` line left behind by a generation that died running no code.
fn stale_connect(boot: &str, conn: u64, shape: &str) -> Value {
    serde_json::json!({
        "at": "2026-10-01T00:00:00.000Z",
        "chassis": "threads",
        "boot": boot,
        "event": EVENT_CONNECT,
        "conn": conn,
        "shape": shape,
        "key": Value::Null,
        "keys": [Value::Null],
        "consumer": Value::Null,
    })
}

fn stale_disconnect(boot: &str, conn: u64, shape: &str) -> Value {
    serde_json::json!({
        "at": "2026-10-01T00:00:01.000Z",
        "chassis": "threads",
        "boot": boot,
        "event": EVENT_DISCONNECT,
        "conn": conn,
        "shape": shape,
        "reason": CloseReason::Unsubscribed.as_str(),
    })
}

/// **THE REGISTRY ANSWERS WITH NO FILE HALF AT ALL, AND THE SUITE NAMES WHO IS
/// IN THAT REGIME.**
///
/// A substrate ancestor (`the_registry_answers_without_a_file_half`) with the
/// doctrine-10 companion added. "Who is subscribed" is impossible to rot
/// precisely because it is never replayed from lines — but a test that proves
/// the degraded path works, with nothing asserting WHO is on it, is
/// indistinguishable from coverage. So the second half names the population: in
/// this ledger, EVERY subscriber is unrecorded, and that is asserted rather than
/// implied.
///
/// **MUTATION.** Make `consumers` read the file and the first arm reddens with
/// no file to read. Make `attach` run at construction and `path()` stops being
/// `None`.
#[test]
fn the_registry_answers_with_no_file_half_and_the_unrecorded_are_named() {
    let l = ConsumerLedger::new();
    assert!(
        l.path().is_none(),
        "nothing is attached, so there is no file half"
    );

    l.connected("accounts", Some("cc1"), 7, Some(who("threads-tui")));
    l.connected("accounts", None, 9, Some(who("aui")));

    let rows = l.consumers("accounts");
    assert_eq!(rows.len(), 2, "both are subscribed and both are reported");
    assert_eq!(rows[0].name.as_deref(), Some("threads-tui"));
    assert_eq!(rows[0].keys, vec!["cc1".to_string()]);
    assert!(!rows[0].keyless);
    assert_eq!(rows[1].name.as_deref(), Some("aui"));
    assert!(rows[1].keyless, "`every key you know about`");
    assert!(l.consumers("budgets").is_empty(), "shapes do not bleed");

    // ── DOCTRINE 10: WHO IS IN THE DEGRADED REGIME ───────────────────────────
    //
    // A ledger with no file half records nobody's history. Naming that
    // population is the assertion that stops "the registry still works" being
    // read as "nothing is lost".
    let unrecorded: Vec<ConnId> = rows.iter().map(|r| r.conn).collect();
    assert_eq!(
        unrecorded,
        vec![7, 9],
        "with no journal wired, EVERY live subscriber's history is unrecorded — \
         both of these, by name, and not merely `some`"
    );

    l.dropped(7);
    assert_eq!(
        l.consumers("accounts").len(),
        1,
        "and the registry half still closes entries without a file"
    );
}

/// **AN UNIDENTIFIED SUBSCRIBER IS A HOLE IN THE COLUMN, AND THE SUITE SAYS
/// WHICH ROW IS THE HOLE.**
///
/// A substrate ancestor (`an_unidentified_subscriber_is_visible_as_a_hole_rather
/// _than_absent`) with the doctrine-10 companion. The ancestor subscribed ONE
/// unidentified consumer and asserted it was listed — which is correct, is worth
/// having, and is green against a registry where EVERY row is a hole.
///
/// So this asserts the POPULATION, with counterparts: conn 7 and conn 11 are
/// genuinely named, conn 9 genuinely is not, and the set of holes is exactly
/// `{9}`. `ConsumerIdentity::parse` refuses anonymity at the door now, so a hole
/// can only arrive through a caller that passes `None` — which is why knowing
/// whether any row is in that regime is a question worth being able to answer.
///
/// **MUTATION.** Make `rows_for` omit rows with no identity and the "it is
/// LISTED" arm reddens. Have it substitute a placeholder name and the hole arm
/// reddens. Have it report every row as a hole and the population arm reddens
/// while the ancestor's single-row test stays green.
#[test]
fn an_unidentified_subscriber_is_a_hole_and_the_suite_names_which_row() {
    let l = ConsumerLedger::new();
    l.connected("accounts", Some("cc1"), 7, Some(who("threads-tui")));
    l.connected("accounts", None, 9, None);
    l.connected("accounts", Some("cc3"), 11, Some(who("aui")));

    let rows = l.consumers("accounts");
    assert_eq!(rows.len(), 3, "the unidentified one is LISTED, not omitted");

    let hole = rows.iter().find(|r| r.conn == 9).expect("conn 9 is listed");
    assert_eq!(hole.name, None, "a hole where the name goes");
    assert_eq!(hole.shape_version, None);
    assert_eq!(hole.pid, None);
    assert_eq!(hole.build, None);
    assert!(
        !hole.since.is_empty(),
        "it is still a subscription with an age, which is what makes it findable"
    );

    // THE POPULATION, with counterparts.
    let anonymous: Vec<ConnId> = rows
        .iter()
        .filter(|r| r.name.is_none())
        .map(|r| r.conn)
        .collect();
    assert_eq!(
        anonymous,
        vec![9],
        "exactly ONE row is in the unidentified regime, and it is conn 9"
    );
    assert_eq!(
        rows.iter().find(|r| r.conn == 7).unwrap().name.as_deref(),
        Some("threads-tui"),
        "counterpart: conn 7 genuinely IS named"
    );
    assert_eq!(
        rows.iter().find(|r| r.conn == 11).unwrap().name.as_deref(),
        Some("aui"),
        "counterpart: and so is conn 11"
    );
}

/// **A PRODUCER NOBODY SUBSCRIBES TO WRITES NO DIRECTORY AND NO FILE.**
///
/// The lazy-open rule. It is what keeps every unit test that merely builds a
/// producer out of the operator's home, and it is honest besides: a producer
/// with no consumers has no consumer history. Both directions, because "writes
/// nothing, ever" would pass the first half.
///
/// **MUTATION.** Create the directory in `attach` (or set `started: true` there)
/// and the first arm reddens.
#[test]
fn a_producer_nobody_subscribes_to_writes_no_directory_and_no_file() {
    let w = wire("threads", "boot-a");
    assert_eq!(w.ledger.path().as_deref(), Some(w.path.as_path()));
    assert!(
        !w.path.exists(),
        "attaching touches no filesystem at all"
    );
    assert!(
        !w.path.parent().unwrap().exists(),
        "and it does not create the `links` directory either"
    );
    w.ledger.flush();
    assert!(!w.path.exists(), "and neither does a flush with nothing held");

    // The counterpart: ONE subscribe does open it.
    w.ledger.connected("accounts", Some("cc1"), 7, Some(who("x")));
    assert!(
        w.path.exists(),
        "the FIRST record opens the file — or nothing would ever be recorded"
    );
}

/// **THE FIRST RECORD OPENS THE FILE WITH A `chassis_start` LINE, AND EVERY LINE
/// CARRIES `at`, `chassis` AND `boot`.**
///
/// `boot` is what makes the generation sweep possible at all: it is how a line
/// written by a chassis that is gone is told from one written by the chassis that
/// is running. A line missing it is a line the sweep cannot classify, so the
/// claim is asserted over EVERY record rather than over the first.
///
/// **MUTATION.** Drop any of the three inserts from `Journal::with_generation`
/// and the per-record arm reddens. Skip the `chassis_start` write and the first
/// arm reddens.
#[test]
fn the_first_record_opens_the_file_and_every_line_names_its_generation() {
    let w = wire("threads", "boot-a");
    w.ledger.connected("accounts", Some("cc1"), 7, Some(who("x")));
    w.ledger.disconnected("accounts", 7, CloseReason::Unsubscribed);

    let records = w.records();
    assert_eq!(
        event_of(&records[0]),
        EVENT_CHASSIS_START,
        "the generation announces itself before anything else it writes"
    );
    assert_eq!(records[0]["pid"], std::process::id());
    assert_eq!(text(&records[0], "version").as_deref(), Some("test-1.0"));

    assert!(records.len() >= 3, "start, connect, disconnect");
    for record in &records {
        assert_eq!(
            text(record, "chassis").as_deref(),
            Some("threads"),
            "every line says which producer wrote it: {record}"
        );
        assert_eq!(
            text(record, "boot").as_deref(),
            Some("boot-a"),
            "and which generation, or the sweep cannot classify it: {record}"
        );
        let at = text(record, "at").expect("every line is stamped");
        assert!(
            at.ends_with('Z') && at.contains('.'),
            "RFC3339 to the millisecond, in UTC: {at}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. The five admissions, as they reach the file
// ─────────────────────────────────────────────────────────────────────────────

/// **A RECOVERY CYCLE IS ONE `resubscribe` AND NOT A CLOSE FOLLOWED BY AN
/// OPEN.**
///
/// Mechanism 1 and 2 together, and the absence is the load-bearing half: a
/// `disconnect` line here would ALTERNATE with the `connect` that follows it, so
/// mechanism 3 could never match and a 10,000-cycle fit would put 20,000 lines
/// on disk. `after` names the verb that would have closed it, which is how a
/// reader tells a recovery from an unsubscribe-and-think-better-of-it.
///
/// **MUTATION.** Remove the `if !resumes` guard around `write_close` and the
/// no-disconnect arm reddens. Make `Admission::Resumed` pass `None` for `after`
/// and the `after` arm reddens. Make `resumes` ignore the shape and this still
/// passes — which is why `a_deferred_close_is_not_resumed_by_a_neighbour` exists
/// beside it.
#[test]
fn a_recovery_cycle_is_one_resubscribe_and_not_a_close_and_an_open() {
    let w = wire("threads", "boot-a");
    w.ledger.connected("accounts", Some("cc1"), 7, Some(who("tui")));
    w.ledger
        .disconnected("accounts", 7, CloseReason::Unsubscribed);
    w.ledger.connected("accounts", Some("cc1"), 7, Some(who("tui")));

    let records = w.records();
    assert_eq!(
        w.events(),
        vec![EVENT_CHASSIS_START, EVENT_CONNECT, EVENT_RESUBSCRIBE],
        "three lines, and the middle of a recovery cycle is NOT one of them"
    );
    assert!(
        only(&records, EVENT_DISCONNECT).is_empty(),
        "the subscription never ended, so nothing may say it did"
    );
    let resubscribe = only(&records, EVENT_RESUBSCRIBE)[0];
    assert_eq!(
        text(resubscribe, "after").as_deref(),
        Some("unsubscribed"),
        "and it names the close it completed, so a reader can tell a recovery \
         from a consumer that changed its mind"
    );
    assert_eq!(resubscribe["conn"], 7);
    assert_eq!(resubscribe["keys"], serde_json::json!(["cc1"]));
}

/// **A REPEAT SUBSCRIBE ON A KEY ALREADY HELD IS `resubscribe after:
/// subscribe`**, and a SECOND KEY IS `widen`.
///
/// Both in one test, because they are the two halves of the split substrate made
/// on 2026-09-26 and the whole value of the split is that they land in different
/// columns. `resubscribe` is rule 2.5's RECOVERY act — the symptom an operator
/// reads the column to find — and *this consumer now also wants key `b`* is a
/// consumer doing its job. Recording the second as the first puts a healthy
/// consumer in the same column as a looping one.
///
/// **MUTATION.** Have `Registry::admit` return `Repeated` where it returns
/// `Widened` and the widen arm reddens with a `resubscribe`; the inverse reddens
/// the repeat arm. Both are invisible to a test that only counts lines.
#[test]
fn a_repeat_subscribe_and_a_second_key_land_in_different_columns() {
    let w = wire("threads", "boot-a");
    let it = Some(who("tui"));
    w.ledger.connected("accounts", Some("cc1"), 7, it.clone());
    w.ledger.connected("accounts", Some("cc1"), 7, it.clone());
    w.ledger.connected("accounts", Some("cc2"), 7, it.clone());

    let records = w.records();
    assert_eq!(
        w.events(),
        vec![
            EVENT_CHASSIS_START,
            EVENT_CONNECT,
            EVENT_RESUBSCRIBE,
            EVENT_WIDEN
        ],
        "the same key again is a recovery; a new key is a widen"
    );

    let repeat = only(&records, EVENT_RESUBSCRIBE)[0];
    assert_eq!(text(repeat, "after").as_deref(), Some("subscribe"));
    assert_eq!(repeat["keys"], serde_json::json!(["cc1"]));

    let widen = only(&records, EVENT_WIDEN)[0];
    assert_eq!(widen["key"], "cc2", "the key that was newly taken an interest in");
    assert_eq!(
        widen["keys"],
        serde_json::json!(["cc1", "cc2"]),
        "and ALL of them after — the field substrate's scalar could not hold"
    );
    assert!(
        widen.get("after").is_none(),
        "a widen completes no close, so it carries no `after`"
    );
}

/// **A DIFFERENT BUILD ON ONE SOCKET GETS ITS OWN CLOSE AND ITS OWN OPEN.**
///
/// The case that must never be merged into a `resubscribe`: a consumer that came
/// back as a different build is exactly the fact substrate's operator could not
/// see on 2026-09-22, so its arrival is a `disconnect` with reason
/// `resubscribed` and then a `connect`. Four lines, in that order.
///
/// **MUTATION.** Return `Repeated` from `admit` where it returns `Replaced` and
/// the whole shape collapses to one `resubscribe`. Drop the close from the
/// `Replaced` arm and the pairing breaks — which the sweep then depends on.
#[test]
fn a_different_build_on_one_socket_gets_its_own_close_and_its_own_open() {
    let w = wire("threads", "boot-a");
    w.ledger
        .connected("accounts", Some("cc1"), 7, Some(built("tui", "three-days-old")));
    w.ledger
        .connected("accounts", Some("cc1"), 7, Some(built("tui", "today")));

    let records = w.records();
    assert_eq!(
        w.events(),
        vec![
            EVENT_CHASSIS_START,
            EVENT_CONNECT,
            EVENT_DISCONNECT,
            EVENT_CONNECT
        ],
        "a close and an open, in that order, so a reader pairing lines sees two \
         subscriptions and not one"
    );
    let closed = only(&records, EVENT_DISCONNECT)[0];
    assert_eq!(
        text(closed, "reason").as_deref(),
        Some("resubscribed"),
        "and the reason says which of the four kinds of close this was"
    );
    assert_eq!(closed["conn"], 7);
    assert!(
        closed.get("consumer").is_none(),
        "a close is about the subscription that ENDED and names no consumer"
    );
    let opened = only(&records, EVENT_CONNECT)[1];
    assert_eq!(
        opened["consumer"]["build"], "today",
        "and the new line carries the build that is actually connected now"
    );
}

/// **AN UNSUBSCRIBE NOBODY RESUMES IS WRITTEN AS A `disconnect`, WITH ITS
/// REASON.**
///
/// The opposing forcing for the recovery-cycle test: that test asserts no
/// `disconnect` is written, so without this one a module that NEVER writes a
/// disconnect would pass. Each of the three caller-supplied reasons is asserted,
/// because a reader tallying stale consumers cares a great deal which it is
/// looking at.
///
/// **AND IT NAMES NO CONSUMER.** A close is about the subscription that ENDED,
/// and the pairing (`Journal::close_orphans`) reads it by `(boot, conn, shape)`
/// alone. Putting an identity on it would assert, on the one line a reader
/// pairs, something the close does not know — the entry may have been taken over
/// since — and it would also make a close and a `narrow` the same shape, which
/// is the collapse the `widen`/`narrow` split was made against.
///
/// **THAT ARM EXISTS BECAUSE A FORCING FOUND IT MISSING.** The mutation that adds
/// `"consumer": d.identity` to `ConsumerLedger::write_close` came back GREEN
/// across all 47 tests: the only close whose field set was asserted anywhere was
/// the one the `Admission::Replaced` arm writes inline, and `write_close` — which
/// is the door every deferred close, every `dropped` and every emptied `narrow`
/// goes through — had nothing watching its shape. A guard I believed I had and
/// did not.
///
/// **MUTATION.** Have `flush` drop the deferral instead of writing it and all
/// three arms redden. Hard-code one reason in `write_close` and two of the three
/// redden. Add `"consumer"` to `write_close`'s record and the last arm reddens.
#[test]
fn an_unsubscribe_nobody_resumes_is_written_with_its_own_reason_and_no_consumer() {
    for (reason, wire_word) in [
        (CloseReason::Unsubscribed, "unsubscribed"),
        (CloseReason::ConnectionClosed, "connection_closed"),
    ] {
        let w = wire("threads", "boot-a");
        w.ledger.connected("accounts", Some("cc1"), 7, Some(who("tui")));
        w.ledger.disconnected("accounts", 7, reason);

        let records = w.records();
        let closed = only(&records, EVENT_DISCONNECT);
        assert_eq!(closed.len(), 1, "the close reaches disk once flushed");
        assert_eq!(text(closed[0], "reason").as_deref(), Some(wire_word));
        assert_eq!(closed[0]["conn"], 7);
        assert_eq!(text(closed[0], "shape").as_deref(), Some("accounts"));
        assert!(
            closed[0].get("consumer").is_none(),
            "a close names no consumer — it is about the subscription that ENDED, \
             and the pairing reads it by (boot, conn, shape) alone: {}",
            closed[0]
        );
        // The exact field set, so a field ADDED later reddens here rather than
        // changing the one line every reader pairs on without anyone noticing.
        let mut fields: Vec<&str> = closed[0]
            .as_object()
            .expect("a record is an object")
            .keys()
            .map(String::as_str)
            .collect();
        fields.sort_unstable();
        assert_eq!(
            fields,
            vec!["at", "boot", "chassis", "conn", "event", "reason", "shape"],
            "the close record's whole field set"
        );
    }
}

/// **EVERY CLOSE GOES THROUGH ONE DOOR AND THEY ALL HAVE THE SAME SHAPE.**
///
/// The companion to the test above, and the reason the gap it names was
/// invisible: there are FOUR ways a close is written — a deferred
/// `disconnected` flushed by the next event, a `dropped` connection, a `narrow`
/// that emptied its key set, and the `Admission::Replaced` arm's inline close —
/// and only the last had its field set asserted. A reader pairing connects with
/// disconnects sees all four on the same line type, so a field on one of them
/// and not the others is a difference nothing in the file explains.
///
/// **MUTATION.** Add any field to `write_close`'s record, or remove one from the
/// `Replaced` arm's, and the two field sets stop agreeing.
#[test]
fn all_four_kinds_of_close_write_the_same_field_set() {
    let fields_of = |record: &Value| -> Vec<String> {
        let mut f: Vec<String> = record
            .as_object()
            .expect("a record is an object")
            .keys()
            .cloned()
            .collect();
        f.sort();
        f
    };

    // 1. a deferred close, flushed.
    let w = wire("threads", "boot-a");
    w.ledger.connected("accounts", Some("cc1"), 7, Some(who("a")));
    w.ledger.disconnected("accounts", 7, CloseReason::Unsubscribed);
    let deferred = fields_of(only(&w.records(), EVENT_DISCONNECT)[0]);

    // 2. a dropped connection.
    let w = wire("threads", "boot-b");
    w.ledger.connected("accounts", Some("cc1"), 7, Some(who("a")));
    w.ledger.dropped(7);
    let dropped = fields_of(only(&w.records(), EVENT_DISCONNECT)[0]);

    // 3. a narrow that emptied its key set.
    let w = wire("threads", "boot-c");
    w.ledger.connected("accounts", Some("cc1"), 7, Some(who("a")));
    assert!(w.ledger.narrowed("accounts", "cc1", 7, CloseReason::Unsubscribed));
    let emptied = fields_of(only(&w.records(), EVENT_DISCONNECT)[0]);

    // 4. the Replaced arm's inline close.
    let w = wire("threads", "boot-d");
    w.ledger
        .connected("accounts", Some("cc1"), 7, Some(built("a", "old")));
    w.ledger
        .connected("accounts", Some("cc1"), 7, Some(built("a", "new")));
    let replaced = fields_of(only(&w.records(), EVENT_DISCONNECT)[0]);

    assert_eq!(deferred, dropped, "a deferred close and a dropped connection");
    assert_eq!(deferred, emptied, "…and an emptied narrow");
    assert_eq!(deferred, replaced, "…and a consumer replaced on its own socket");
    assert_eq!(
        deferred,
        vec!["at", "boot", "chassis", "conn", "event", "reason", "shape"],
        "and that one shape is this one — stated, so a new field cannot be added \
         to the line every reader pairs on without reddening a test"
    );
}

/// **A `narrow` IS ITS OWN EVENT AND IS INVISIBLE TO THE CONNECT/DISCONNECT
/// PAIRING.**
///
/// A reader pairing connects with disconnects must not see a subscription that
/// dropped one of its keys as one that ended. The last key IS a close, and that
/// writes a `disconnect` instead — both directions, because an implementation
/// that wrote `narrow` for everything would pass the first arm and one that
/// wrote `disconnect` for everything would pass the second.
///
/// **MUTATION.** Make `narrowed` emit `EVENT_DISCONNECT` and the first arm
/// reddens. Make it skip the `disconnected` call when the set empties and the
/// second arm reddens.
#[test]
fn a_narrow_is_its_own_event_and_the_last_key_is_a_close() {
    let w = wire("threads", "boot-a");
    let it = Some(who("tui"));
    w.ledger.connected("accounts", Some("cc1"), 7, it.clone());
    w.ledger.connected("accounts", Some("cc2"), 7, it.clone());

    assert!(!w.ledger.narrowed("accounts", "cc2", 7, CloseReason::Unsubscribed));
    let records = w.records();
    let narrowed = only(&records, EVENT_NARROW);
    assert_eq!(narrowed.len(), 1, "one key dropped, one `narrow` line");
    assert_eq!(narrowed[0]["key"], "cc2", "the key that went");
    assert_eq!(
        narrowed[0]["keys"],
        serde_json::json!(["cc1"]),
        "and the ones that remain"
    );
    assert_eq!(
        narrowed[0]["consumer"]["name"], "tui",
        "a narrow names the consumer, unlike a close — the subscription is still live"
    );
    assert!(
        only(&records, EVENT_DISCONNECT).is_empty(),
        "nothing ended, so the pairing must not see a close"
    );

    // The last key IS a close, through the one door that keeps the pairing
    // intact.
    assert!(w.ledger.narrowed("accounts", "cc1", 7, CloseReason::Unsubscribed));
    let records = w.records();
    assert_eq!(
        only(&records, EVENT_NARROW).len(),
        1,
        "and no second `narrow` was written for the key that ended it"
    );
    let closed = only(&records, EVENT_DISCONNECT);
    assert_eq!(closed.len(), 1, "dropping the last key is ONE close");
    assert_eq!(text(closed[0], "reason").as_deref(), Some("unsubscribed"));
}

// ─────────────────────────────────────────────────────────────────────────────
// 7. Order — asserted across three or more events, since two cannot tell
//    ordering from pairing
// ─────────────────────────────────────────────────────────────────────────────

/// **THE FILE'S ORDER IS THE ORDER THE EVENTS HAPPENED IN**, across four events
/// and two consumers.
///
/// Two events cannot distinguish *ordered* from *paired*: `connect` then
/// `disconnect` is the same list either way. Four, with the second consumer's
/// `connect` landing BETWEEN the first consumer's two lines, can — and that
/// middle position is exactly what mechanism 2's deferral puts at risk, because
/// the close is held while the next event is being decided.
///
/// **MUTATION.** Move `write_close` in `connected_link` to AFTER the `match` and
/// the disconnect lands behind conn 9's connect. Have `flush` write the deferral
/// first and the test still passes — which is why the position is asserted
/// against the neighbour's line rather than against the end of the file.
#[test]
fn the_file_order_is_the_event_order_across_four_events() {
    let w = wire("threads", "boot-a");
    w.ledger.connected("accounts", Some("cc1"), 7, Some(who("first")));
    w.ledger
        .disconnected("accounts", 7, CloseReason::ConnectionClosed);
    w.ledger.connected("accounts", Some("cc2"), 9, Some(who("second")));
    w.ledger
        .disconnected("accounts", 9, CloseReason::Unsubscribed);

    let records = w.records();
    assert_eq!(
        w.events(),
        vec![
            EVENT_CHASSIS_START,
            EVENT_CONNECT,
            EVENT_DISCONNECT,
            EVENT_CONNECT,
            EVENT_DISCONNECT
        ],
    );
    let conns: Vec<Value> = records
        .iter()
        .filter(|r| event_of(r) != EVENT_CHASSIS_START)
        .map(|r| r["conn"].clone())
        .collect();
    assert_eq!(
        conns,
        vec![
            serde_json::json!(7),
            serde_json::json!(7),
            serde_json::json!(9),
            serde_json::json!(9)
        ],
        "conn 7 closes BEFORE conn 9 opens, because that is the order it happened \
         — a deferral that outlived the next event would put it after"
    );
}

/// **TWO CLOSES IN A ROW CANNOT COLLAPSE INTO ONE.**
///
/// Only ONE close is ever held, so a second flushes the first. Without that, two
/// consumers leaving back to back would lose one `disconnect` line and the
/// pairing the sweep depends on would be permanently one short.
///
/// **MUTATION.** Delete the `if let Some(d) = self.take_deferred()` flush from
/// `disconnected` and the second close overwrites the first: one line instead of
/// two.
#[test]
fn two_closes_in_a_row_cannot_collapse_into_one() {
    let w = wire("threads", "boot-a");
    w.ledger.connected("accounts", Some("cc1"), 7, Some(who("a")));
    w.ledger.connected("accounts", Some("cc2"), 9, Some(who("b")));
    w.ledger.disconnected("accounts", 7, CloseReason::Unsubscribed);
    w.ledger
        .disconnected("accounts", 9, CloseReason::ConnectionClosed);

    let records = w.records();
    let closed = only(&records, EVENT_DISCONNECT);
    assert_eq!(closed.len(), 2, "two subscriptions ended, two lines");
    assert_eq!(closed[0]["conn"], 7);
    assert_eq!(text(closed[0], "reason").as_deref(), Some("unsubscribed"));
    assert_eq!(closed[1]["conn"], 9);
    assert_eq!(
        text(closed[1], "reason").as_deref(),
        Some("connection_closed"),
        "and each keeps its OWN reason — the second did not inherit the first's"
    );
}

/// **A DEFERRED CLOSE IS NOT RESUMED BY A NEIGHBOUR, NOR BY THE SAME CONSUMER ON
/// ANOTHER SHAPE.**
///
/// Mechanism 2's deferral matches on connection AND shape AND consumer, and only
/// a second subscriber can show that it does: with one consumer in the registry,
/// a deferral that matched on nothing at all would behave identically. Three
/// near-misses are tried, each of which must leave the close standing.
///
/// **MUTATION.** Drop `d.conn == conn` from the `resumes` predicate and arm 1
/// reddens. Drop `d.shape == shape` and arm 2 reddens. Drop `d.identity ==
/// identity` and arm 3 reddens. The positive case
/// (`a_recovery_cycle_is_one_resubscribe...`) is the counterpart that stops
/// `resumes = false` passing.
#[test]
fn a_deferred_close_is_not_resumed_by_a_neighbour_or_another_shape() {
    // Arm 1 — a DIFFERENT CONNECTION subscribing.
    let w = wire("threads", "boot-a");
    w.ledger.connected("accounts", Some("cc1"), 7, Some(who("a")));
    w.ledger.disconnected("accounts", 7, CloseReason::Unsubscribed);
    w.ledger.connected("accounts", Some("cc1"), 9, Some(who("a")));
    let records = w.records();
    assert_eq!(
        only(&records, EVENT_DISCONNECT).len(),
        1,
        "conn 7's close is a real close: conn 9 is a different subscription, even \
         claiming the same identity on the same shape and key"
    );
    assert!(only(&records, EVENT_RESUBSCRIBE).is_empty());

    // Arm 2 — the SAME connection, a DIFFERENT SHAPE.
    let w = wire("threads", "boot-b");
    w.ledger.connected("accounts", Some("cc1"), 7, Some(who("a")));
    w.ledger.disconnected("accounts", 7, CloseReason::Unsubscribed);
    w.ledger.connected("budgets", Some("cc1"), 7, Some(who("a")));
    let records = w.records();
    assert_eq!(
        only(&records, EVENT_DISCONNECT).len(),
        1,
        "a subscribe to another shape does not continue this shape's subscription"
    );
    assert!(only(&records, EVENT_RESUBSCRIBE).is_empty());

    // Arm 3 — the same connection and shape, a DIFFERENT BUILD.
    let w = wire("threads", "boot-c");
    w.ledger
        .connected("accounts", Some("cc1"), 7, Some(built("a", "old")));
    w.ledger.disconnected("accounts", 7, CloseReason::Unsubscribed);
    w.ledger
        .connected("accounts", Some("cc1"), 7, Some(built("a", "new")));
    let records = w.records();
    assert_eq!(
        only(&records, EVENT_DISCONNECT).len(),
        1,
        "a different build is a different subscription and the old one's close stands"
    );
    assert!(
        only(&records, EVENT_RESUBSCRIBE).is_empty(),
        "merging this into a recovery is what would HIDE the build change"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 8. Mechanism 3 — run-length compression, and LEDGER_RUN_MAX at its real value
// ─────────────────────────────────────────────────────────────────────────────

/// **THE FIRST OCCURRENCE IS ON DISK IMMEDIATELY; THE REPETITIONS ARE NOT.**
///
/// This is the property that makes a lost run harmless: a run held in memory
/// loses its COUNT to a `SIGKILL` and never the FACT that the event happened. So
/// the assertion is on the file BEFORE any flush — the state a crash would leave
/// — and it is read without flushing, which `Wired::records` would do.
///
/// **MUTATION.** Move the `write_record` in `Journal::push` to after the run
/// bookkeeping (so the first occurrence joins the run) and the "the fact is on
/// disk" arm reddens. Flush on every push and the "the count is not" arm
/// reddens.
#[test]
fn the_first_occurrence_is_on_disk_at_once_and_the_count_is_not() {
    let w = wire("threads", "boot-a");
    for _ in 0..6 {
        w.ledger.connected("accounts", Some("cc1"), 7, Some(who("fit")));
    }

    // Deliberately NOT `w.records()`: that flushes, and this is a claim about
    // what a crash would have left behind.
    let crashed = records_at(&w.path);
    assert_eq!(
        crashed
            .iter()
            .map(|r| event_of(r).to_string())
            .collect::<Vec<_>>(),
        vec![EVENT_CHASSIS_START, EVENT_CONNECT, EVENT_RESUBSCRIBE],
        "a six-cycle fit is three lines, and the FACT of the resubscribe is one \
         of them"
    );
    assert!(
        only(&crashed, EVENT_REPEAT).is_empty(),
        "the count of a run in progress is in memory, so a crash loses it"
    );

    // And the counterpart: flushing writes the count that was held.
    let flushed = w.records();
    let repeats = only(&flushed, EVENT_REPEAT);
    assert_eq!(repeats.len(), 1, "one run, one summary");
    assert_eq!(
        repeats[0]["repeats"], 4,
        "six subscribes: one connect, one resubscribe written, four repetitions"
    );
    assert!(
        !text(&repeats[0].clone(), "first_repeat_at")
            .unwrap_or_default()
            .is_empty(),
        "and the window the run covered is recorded, not just its size"
    );
    assert!(!text(&repeats[0].clone(), "last_repeat_at").unwrap_or_default().is_empty());
    assert_eq!(
        event_of(&repeats[0]["of"]),
        EVENT_RESUBSCRIBE,
        "`of` carries the record that repeated"
    );
}

/// **A `repeat` RECORD CARRIES NO TOP-LEVEL `conn` OR `shape`, SO THE SWEEP
/// CANNOT MISTAKE IT FOR THE EVENT IT SUMMARISES.**
///
/// The sweep pairs on `(boot, conn, shape)` and matches `connect` /
/// `disconnect`. A `repeat` that exposed those three at the top level would be
/// read as whichever of the two it was summarising — a run of `connect`s would
/// re-open an entry the sweep had just closed. Keeping them nested inside `of` is
/// what makes a `repeat` invisible to the pairing by CONSTRUCTION rather than by
/// a rule the sweep has to remember.
///
/// **MUTATION.** Add `"conn"` and `"shape"` to the `EVENT_REPEAT` record in
/// `Journal::settle_run` and this reddens; the positive half (`of` really does
/// hold them) is what stops "the repeat record is empty" passing.
#[test]
fn a_repeat_record_keeps_conn_and_shape_nested_where_the_sweep_cannot_pair_them() {
    let w = wire("threads", "boot-a");
    for _ in 0..5 {
        w.ledger.connected("accounts", Some("cc1"), 7, Some(who("fit")));
    }
    let records = w.records();
    let summary = only(&records, EVENT_REPEAT)[0];

    assert!(
        summary.get("conn").is_none(),
        "a top-level `conn` would make the sweep pair a summary as an event"
    );
    assert!(summary.get("shape").is_none());
    // The positive half: the information is not lost, it is one level down.
    assert_eq!(summary["of"]["conn"], 7);
    assert_eq!(summary["of"]["shape"], "accounts");
    // And the sweep is the thing that must be indifferent to it. Nothing in this
    // file is an unpaired `connect`, so a correct sweep writes no close at all.
    assert_eq!(
        only(&records, EVENT_DISCONNECT).len(),
        0,
        "no close is owed here, and a summary must not have created one"
    );
}

/// **LEDGER_RUN_MAX IS FORCED AT 1,024, IN BOTH DIRECTIONS.**
///
/// Not at a convenient small number and not at an injected maximum: at the
/// constant the running producer actually uses. A boundary tested at a
/// fabricated max is this arc's named shortcut.
///
/// The arithmetic, because it is worth stating rather than discovering: the
/// first subscribe writes a `connect`, the second writes the `resubscribe` that
/// becomes the run's key, and every subscribe after that is a repetition. So
/// after `k` subscribes the run holds `k - 2` repetitions, and
/// `Journal::push` flushes when that reaches `LEDGER_RUN_MAX`.
///
/// * 1,025 subscribes → 1,023 repetitions → **must not flush** (the value that
///   must be served).
/// * 1,026 subscribes → 1,024 repetitions → **must flush**, and the record must
///   say 1,024 (the value that must be refused further accumulation).
///
/// **MUTATION.** Change the comparison to `>` and the second arm reddens by one.
/// Change the constant to 1,023 and the FIRST arm reddens — which is the
/// direction a test written only at "it eventually flushes" cannot see.
#[test]
fn the_run_flushes_at_the_real_ledger_run_max_of_1024_and_not_one_short() {
    assert_eq!(
        LEDGER_RUN_MAX, 1024,
        "SENTINEL: this test is written against the real constant and must fail \
         loudly if the constant moves rather than quietly test the wrong number"
    );
    let w = wire("threads", "boot-a");
    let subscribe = || w.ledger.connected("accounts", Some("cc1"), 7, Some(who("fit")));

    // One short of the bound.
    for _ in 0..(LEDGER_RUN_MAX + 1) {
        subscribe();
    }
    let short = records_at(&w.path);
    assert!(
        only(&short, EVENT_REPEAT).is_empty(),
        "1,023 repetitions is BELOW LEDGER_RUN_MAX and must not have flushed — a \
         bound that fires early is a bound at the wrong number"
    );
    assert_eq!(
        short.len(),
        3,
        "still just start, connect and the one resubscribe"
    );

    // One more crosses it.
    subscribe();
    let crossed = records_at(&w.path);
    let summary = only(&crossed, EVENT_REPEAT);
    assert_eq!(
        summary.len(),
        1,
        "the 1,024th repetition flushes, with no flush() called"
    );
    assert_eq!(
        summary[0]["repeats"], 1024,
        "and the count is the constant itself, not an off-by-one near it"
    );
}

/// **A FLUSH SETTLES THE RUN AND THE SAME RECORD KEEPS COMPRESSING
/// AFTERWARDS.**
///
/// `settle_run` resets the count but KEEPS the key, so a fit that is still going
/// goes on costing nothing per cycle. If the key were dropped, every flush would
/// restart the run and the next cycle would write a full line — which is the
/// failure the whole mechanism exists to prevent, arriving once per flush
/// instead of once per cycle.
///
/// **MUTATION.** Set `self.run = None` in `settle_run` and the second summary
/// becomes a second full `resubscribe` line.
#[test]
fn a_flush_settles_the_run_and_the_same_record_keeps_compressing() {
    let w = wire("threads", "boot-a");
    let subscribe = || w.ledger.connected("accounts", Some("cc1"), 7, Some(who("fit")));

    for _ in 0..5 {
        subscribe();
    }
    let first = w.records();
    assert_eq!(only(&first, EVENT_REPEAT).len(), 1);
    assert_eq!(only(&first, EVENT_REPEAT)[0]["repeats"], 3);
    assert_eq!(
        only(&first, EVENT_RESUBSCRIBE).len(),
        1,
        "one full resubscribe line so far"
    );

    for _ in 0..3 {
        subscribe();
    }
    let second = w.records();
    assert_eq!(
        only(&second, EVENT_REPEAT).len(),
        2,
        "the run resumed after the flush and summarised again"
    );
    assert_eq!(only(&second, EVENT_REPEAT)[1]["repeats"], 3);
    assert_eq!(
        only(&second, EVENT_RESUBSCRIBE).len(),
        1,
        "and it did NOT write a second full line: the run kept its key across the \
         flush, which is the whole economy of the mechanism"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 9. Rule 2.6's link state rides the record
// ─────────────────────────────────────────────────────────────────────────────

/// **`superseded` NAMES BOTH VERSIONS AND RIDES THE CONNECT RECORD; A CURRENT
/// LINK WRITES NO SUCH FIELD AT ALL.**
///
/// Both directions, and the ABSENCE is the half that matters: a
/// `"superseded": null` on every line is the field that says *I did not look*,
/// which is the collapse rule 2.7 names for `changed`. It also means every line
/// this module has ever written is byte-identical unless a superseded link
/// actually happened.
///
/// **MUTATION.** Insert the field unconditionally in `with_link` and the absence
/// arm reddens. Drop the `wanted` key and the both-numbers arm reddens — *you
/// are old* is not actionable, *you asked for 3 and I serve 4* is.
#[test]
fn superseded_names_both_versions_and_is_absent_when_the_link_is_current() {
    let w = wire("threads", "boot-a");
    w.ledger.connected_link(
        "accounts",
        Some("cc1"),
        7,
        Some(who("stale")),
        Some(crate::wire::Superseded {
            wanted: 3,
            current: 4,
        }),
    );
    w.ledger
        .connected_link("accounts", Some("cc1"), 9, Some(who("current")), None);

    let records = w.records();
    let opened = only(&records, EVENT_CONNECT);
    assert_eq!(opened.len(), 2);

    assert_eq!(
        opened[0]["superseded"],
        serde_json::json!({"wanted": 3, "current": 4}),
        "BOTH numbers, so the consumer reports something a human can act on"
    );
    assert!(
        opened[1].get("superseded").is_none(),
        "a current link writes NO field — not `null`, which is the field that \
         says `I did not look`"
    );
}

/// **TWO RECORDS DIFFERING ONLY IN `superseded` DO NOT COLLAPSE INTO ONE
/// RUN.**
///
/// The link state is part of the run key, which is what makes riding the connect
/// record correct rather than merely tidy. A stale consumer reconnecting 10,000
/// times is one line plus `repeat` records — but the moment it links on a
/// different version it is a DIFFERENT fact and gets its own line.
///
/// Both directions: the first three subscribes must produce three distinct
/// lines, and a fourth identical to the third must produce none.
///
/// **MUTATION.** Apply `with_link` AFTER `self.record(...)` (so the run key is
/// computed without it) and the three-lines arm reddens: the superseded link
/// collapses into the current one and the file claims a link that never
/// happened.
#[test]
fn two_records_differing_only_in_superseded_do_not_collapse() {
    let w = wire("threads", "boot-a");
    let it = Some(who("tui"));
    let stale = Some(crate::wire::Superseded {
        wanted: 3,
        current: 4,
    });

    w.ledger
        .connected_link("accounts", Some("cc1"), 7, it.clone(), None);
    w.ledger
        .connected_link("accounts", Some("cc1"), 7, it.clone(), None);
    w.ledger
        .connected_link("accounts", Some("cc1"), 7, it.clone(), stale);

    let records = records_at(&w.path);
    assert_eq!(
        records
            .iter()
            .map(|r| event_of(r).to_string())
            .collect::<Vec<_>>(),
        vec![
            EVENT_CHASSIS_START,
            EVENT_CONNECT,
            EVENT_RESUBSCRIBE,
            EVENT_RESUBSCRIBE
        ],
        "the superseded resubscribe is a DIFFERENT fact from the current one and \
         gets its own line"
    );
    assert!(records[2].get("superseded").is_none());
    assert_eq!(records[3]["superseded"]["wanted"], 3);

    // The counterpart: identical again DOES collapse, so this is a statement
    // about the run key and not about compression being broken.
    w.ledger
        .connected_link("accounts", Some("cc1"), 7, it.clone(), stale);
    let again = records_at(&w.path);
    assert_eq!(
        again.len(),
        records.len(),
        "a repetition of the superseded record writes no new line"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 10. Rotation — LEDGER_MAX_BYTES at 4 MiB for real, LEDGER_GENERATIONS at 1
// ─────────────────────────────────────────────────────────────────────────────

/// **ROTATION IS FORCED AT THE REAL `LEDGER_MAX_BYTES`, IN BOTH DIRECTIONS.**
///
/// This test writes more than four megabytes. substrate's own comment conceded
/// the shortcut — *"forcing this one to fail at 4 MiB would mean writing 4
/// MiB"* — and made the threshold injectable instead, so the POLICY was asserted
/// and only the MECHANISM was ever exercised. A boundary tested at a fabricated
/// maximum is this arc's named shortcut, so this one goes through
/// [`ConsumerLedger::attach`], at 4 MiB, for real.
///
/// Both directions, which is the point:
///
/// * just under 4 MiB → **no rotation**, the value that must be served;
/// * crossing 4 MiB → **rotated**, the value that must be refused.
///
/// Records are made large (a long `build` claim, which is legitimate consumer
/// text) so the bound is crossed in a few thousand syscalls rather than twenty
/// thousand. The bound is on BYTES and is crossed in bytes.
///
/// **MUTATION.** Change `rotate_for`'s comparison to `>=` and the under arm
/// reddens by one record. Delete the `std::fs::rename` and the over arm reddens.
/// Divide `LEDGER_MAX_BYTES` by two and the under arm reddens, which is the
/// direction an "it eventually rotates" test cannot see.
#[test]
fn rotation_is_forced_at_the_real_four_mebibyte_bound_in_both_directions() {
    assert_eq!(
        LEDGER_MAX_BYTES,
        4 * 1024 * 1024,
        "SENTINEL: written against the real constant, so a change to it fails \
         loudly rather than quietly testing a different number"
    );
    let w = wire("threads", "boot-a");
    let rotated = w.rotated();
    // A long but entirely legitimate build claim, so each record is about a
    // kilobyte and 4 MiB is a few thousand of them.
    let padded = ConsumerIdentity {
        build: Some("b".repeat(900)),
        ..who("threads-tui")
    };
    let size = || std::fs::metadata(&w.path).map(|m| m.len()).unwrap_or(0);

    // ── DIRECTION 1: BELOW THE BOUND, NOTHING ROTATES ────────────────────────
    let mut conn: ConnId = 1;
    while size() + 4_000 < LEDGER_MAX_BYTES {
        w.ledger
            .connected("accounts", Some("cc1"), conn, Some(padded.clone()));
        conn += 1;
        // BOUNDED DELIBERATELY. An implementation that writes nothing would
        // leave `size()` at zero for ever, and an unbounded loop would HANG
        // rather than fail — a test that cannot report is worse than one that
        // reports wrongly. Found by forcing: `started: true` in `attach` makes
        // every write fail silently, and this loop span before the bound was
        // added.
        assert!(
            conn < 40_000,
            "the journal is not growing at all ({} bytes after {conn} records), \
             so the bound can never be approached",
            size()
        );
    }
    let under = size();
    assert!(
        under > LEDGER_MAX_BYTES - 8_000,
        "the file must actually have approached 4 MiB, or this proves nothing: {under}"
    );
    assert!(
        under <= LEDGER_MAX_BYTES,
        "and it must not have passed it yet: {under}"
    );
    assert!(
        !rotated.exists(),
        "{under} bytes is under LEDGER_MAX_BYTES and must NOT have rotated"
    );

    // ── DIRECTION 2: CROSSING IT ROTATES ─────────────────────────────────────
    while !rotated.exists() && conn < 40_000 {
        w.ledger
            .connected("accounts", Some("cc1"), conn, Some(padded.clone()));
        conn += 1;
    }
    assert!(
        rotated.exists(),
        "crossing LEDGER_MAX_BYTES rotates, unconditionally and with nobody's \
         attention"
    );
    let kept = std::fs::metadata(&rotated).unwrap().len();
    assert!(
        kept > LEDGER_MAX_BYTES - 8_000 && kept <= LEDGER_MAX_BYTES,
        "the generation that was closed off is the one that filled up: {kept}"
    );
    assert!(
        size() < kept,
        "and the live file started again from the record that would not fit"
    );
    assert!(
        !previous_generation_path(&w.path, 2).exists(),
        "LEDGER_GENERATIONS is 1, so there is no `.2` — the total is bounded at \
         8 MiB with nothing left to prune"
    );
}

/// **EXACTLY ONE PREVIOUS GENERATION SURVIVES; THE ONE BEFORE IT IS DROPPED.**
///
/// [`LEDGER_GENERATIONS`] at its real value of 1, in both directions: after one
/// rotation the `.1` holds the first generation, and after a second it holds the
/// SECOND — the first is gone, and there is no `.2` holding it instead. A test
/// that only checked `.1` exists would pass against a policy that kept
/// everything.
///
/// The threshold is injected here because the question is the GENERATION COUNT,
/// not the byte bound; the byte bound is crossed for real in the test above.
///
/// **MUTATION.** Change `previous_generation_path`'s suffix to use an
/// incrementing counter and the `.2` arm reddens. Have `rotate_for` copy instead
/// of rename and the dropped arm reddens.
#[test]
fn exactly_one_previous_generation_survives_and_the_older_is_dropped() {
    assert_eq!(LEDGER_GENERATIONS, 1, "SENTINEL: the real constant");
    let w = wire_with_max("threads", "boot-a", 600);
    let rotated = w.rotated();

    // Generation 1 fills and rotates.
    let mut conn: ConnId = 1;
    while !rotated.exists() {
        w.ledger.connected("accounts", Some("cc1"), conn, Some(who("a")));
        conn += 1;
        assert!(conn < 500, "it should rotate long before here");
    }
    let first_generation = std::fs::read_to_string(&rotated).unwrap();
    assert!(
        first_generation.contains("\"conn\":1"),
        "the first generation's own lines are what was kept"
    );

    // Generation 2 fills and rotates over it.
    let marker = conn;
    while std::fs::read_to_string(&rotated).unwrap() == first_generation {
        w.ledger.connected("accounts", Some("cc1"), conn, Some(who("a")));
        conn += 1;
        assert!(conn < 1_000, "it should rotate again long before here");
    }
    let second_generation = std::fs::read_to_string(&rotated).unwrap();
    assert!(
        second_generation.contains(&format!("\"conn\":{marker}")),
        "`.1` now holds the SECOND generation"
    );
    assert!(
        !second_generation.contains("\"conn\":1"),
        "and the first is GONE — one generation, not a growing pile"
    );
    assert!(
        !previous_generation_path(&w.path, 2).exists(),
        "and it was not moved aside into a `.2` either"
    );
}

/// **THE ROTATED NAME IS THE LIVE NAME PLUS `.1`.**
///
/// Appended to the whole name rather than replacing the extension, so the live
/// file and its generation sort together and `.jsonl` stays visible in both — an
/// `x.1.jsonl` would sort away from `x.jsonl` in the directory the operator
/// `ls`es.
///
/// **MUTATION.** Have `previous_generation_path` swap the extension and this
/// reddens.
#[test]
fn the_rotated_name_is_the_live_name_plus_dot_one() {
    let path = Path::new("/tmp/nowhere/links/threads.producer.jsonl");
    assert_eq!(
        previous_generation_path(path, LEDGER_GENERATIONS),
        Path::new("/tmp/nowhere/links/threads.producer.jsonl.1")
    );
    assert_eq!(
        previous_generation_path(path, 2),
        Path::new("/tmp/nowhere/links/threads.producer.jsonl.2"),
        "and the number is the generation, so a policy change needs no new function"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 11. The generation sweep — the answer to "a disconnect that never arrives"
// ─────────────────────────────────────────────────────────────────────────────

/// **THE SWEEP CLOSES AN ENTRY A DEAD GENERATION LEFT OPEN, AND LEAVES THE ONES
/// THAT WERE CLOSED ALONE.**
///
/// Two seeded entries from a generation that died running no code: one with a
/// `disconnect` behind it and one without. Both directions in one test, because
/// a sweep that closed everything it found would pass a test that only looked
/// at the orphan — and it would then write a second `disconnect` for every
/// subscription that ever ended cleanly, permanently breaking the pairing every
/// reader depends on.
///
/// `closed_by` names the generation that did the closing, which is how a reader
/// tells a close the transport witnessed from one the next boot inferred.
///
/// **MUTATION.** Delete the `Some(EVENT_DISCONNECT) => { open.remove(&entry); }`
/// arm from `close_orphans` and the paired arm reddens with a second close.
/// Delete the whole `for` loop over `open` and the orphan arm reddens.
#[test]
fn the_sweep_closes_what_a_dead_generation_left_open_and_only_that() {
    let w = wire("threads", "boot-new");
    seed(
        &w.path,
        &[
            stale_connect("boot-dead", 7, "accounts"),
            stale_connect("boot-dead", 9, "accounts"),
            stale_disconnect("boot-dead", 9, "accounts"),
        ],
    );

    // The first record of the new generation triggers the sweep.
    w.ledger.connected("budgets", None, 11, Some(who("alive")));

    let records = w.records();
    let swept: Vec<&Value> = only(&records, EVENT_DISCONNECT)
        .into_iter()
        .filter(|r| text(r, "reason").as_deref() == Some("chassis_restart"))
        .collect();
    assert_eq!(
        swept.len(),
        1,
        "ONE entry was left open; the one that was properly closed must not be \
         closed a second time"
    );
    assert_eq!(swept[0]["conn"], 7, "and it is the orphan, not the other one");
    assert_eq!(
        text(swept[0], "boot").as_deref(),
        Some("boot-dead"),
        "the close is stamped with the generation that OPENED it, so the pairing works"
    );
    assert_eq!(
        text(swept[0], "closed_by").as_deref(),
        Some("boot-new"),
        "and `closed_by` names the generation that inferred it — a reader can tell \
         this from a close the transport witnessed"
    );

    // Order: the sweep runs before the generation announces itself.
    let events = w.events();
    let restart = events.iter().position(|e| e == EVENT_DISCONNECT).unwrap();
    let start = events.iter().position(|e| e == EVENT_CHASSIS_START).unwrap();
    assert!(
        restart < start,
        "the previous generation's ledger is closed off BEFORE this one opens: \
         {events:?}"
    );
}

/// **THE SWEEP DOES NOT CLOSE THIS GENERATION'S OWN ENTRIES.**
///
/// The opposing forcing. Closing our own live entry would be the ledger lying
/// about the PRESENT, which is worse than a stale line about the past. Both arms
/// are in one test and differ only in the `boot` on the seeded line, so the
/// comparison is between two things that are provably different and the only
/// difference is the one under test.
///
/// **MUTATION.** Delete the `if boot == self.generation.boot { continue; }`
/// guard and the own-generation arm reddens while the stale arm stays green.
#[test]
fn the_sweep_does_not_close_this_generations_own_entries() {
    // Arm 1 — a line from OUR OWN boot is left alone.
    let w = wire("threads", "boot-same");
    seed(&w.path, &[stale_connect("boot-same", 7, "accounts")]);
    w.ledger.connected("budgets", None, 11, Some(who("alive")));
    let records = w.records();
    assert!(
        only(&records, EVENT_DISCONNECT)
            .iter()
            .all(|r| text(r, "reason").as_deref() != Some("chassis_restart")),
        "an entry carrying THIS generation's boot id is not stale — closing it \
         would be the ledger lying about the present"
    );

    // Arm 2 — the SENTINEL: the same seeded line under a different boot IS
    // closed, so arm 1 is not passing because the sweep does nothing.
    let w = wire("threads", "boot-same");
    seed(&w.path, &[stale_connect("boot-other", 7, "accounts")]);
    w.ledger.connected("budgets", None, 11, Some(who("alive")));
    let records = w.records();
    assert_eq!(
        only(&records, EVENT_DISCONNECT)
            .iter()
            .filter(|r| text(r, "reason").as_deref() == Some("chassis_restart"))
            .count(),
        1,
        "SENTINEL: the identical line under another boot IS swept, so the sweep \
         can come out differently"
    );
}

/// **THE SWEEP PAIRS A `connect` IN THE ROTATED GENERATION WITH A `disconnect`
/// IN THE LIVE FILE.**
///
/// It reads `.1` first and the live file second, so an entry whose `connect`
/// fell into the rotated file before its `disconnect` landed is still paired.
/// Both directions: conn 7's close is in the live file and must not be swept,
/// conn 9's never arrived and must be.
///
/// **MUTATION.** Drop the rotated path from `close_orphans`'s file list and
/// BOTH arms change — conn 7 is no longer paired (it reddens) and conn 9 is no
/// longer even seen (it reddens too). Reverse the order of the two files and the
/// pairing arm reddens, because a `disconnect` read before its `connect` does
/// not remove it.
#[test]
fn the_sweep_pairs_across_the_rotated_generation_and_the_live_file() {
    let w = wire("threads", "boot-new");
    seed(
        &w.rotated(),
        &[
            stale_connect("boot-dead", 7, "accounts"),
            stale_connect("boot-dead", 9, "accounts"),
        ],
    );
    seed(&w.path, &[stale_disconnect("boot-dead", 7, "accounts")]);

    w.ledger.connected("budgets", None, 11, Some(who("alive")));

    let records = w.records();
    let swept: Vec<Value> = only(&records, EVENT_DISCONNECT)
        .into_iter()
        .filter(|r| text(r, "reason").as_deref() == Some("chassis_restart"))
        .cloned()
        .collect();
    assert_eq!(
        swept.len(),
        1,
        "conn 7's close is in the LIVE file and pairs with its connect in `.1`; \
         only conn 9 is an orphan"
    );
    assert_eq!(swept[0]["conn"], 9);
}

/// **A HALF-WRITTEN LAST LINE IS SKIPPED AND THE REST IS STILL SWEPT.**
///
/// The last line of a file whose writer was `SIGKILL`ed mid-write is
/// legitimately half a line, and refusing to sweep because of it would be the
/// failure mode doing double duty. The positive sentinel is in the same file: a
/// VALID orphan sits beyond the broken line and must still be found, which is
/// what distinguishes *skipped the garbage* from *gave up at the garbage*.
///
/// **MUTATION.** Replace `let Ok(record) = ... else { continue }` with an
/// `.expect()` and the test panics; `break` instead of `continue` and the
/// sentinel arm reddens while the "did not crash" arm stays green.
#[test]
fn a_half_written_line_is_skipped_and_the_orphan_behind_it_is_still_found() {
    let w = wire("threads", "boot-new");
    std::fs::create_dir_all(w.path.parent().unwrap()).unwrap();
    let body = format!(
        "{}\n{}\n{}\n",
        stale_connect("boot-dead", 7, "accounts"),
        "{\"event\":\"conn",
        stale_connect("boot-dead", 9, "accounts"),
    );
    std::fs::write(&w.path, body).unwrap();

    w.ledger.connected("budgets", None, 11, Some(who("alive")));
    w.ledger.flush();

    // Read LENIENTLY here, and only here: the fixture deliberately contains a
    // line that is not JSON, and `records_at` is strict on purpose so that a
    // malformed line written by this module reddens the test that wrote it.
    let text_on_disk = std::fs::read_to_string(&w.path).unwrap();
    let records: Vec<Value> = text_on_disk
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect();
    assert!(
        text_on_disk.contains("{\"event\":\"conn\n"),
        "SENTINEL: the half-written line is still in the file — a sweep that had \
         rewritten or truncated the file would make this test vacuous"
    );
    let swept: Vec<Value> = only(&records, EVENT_DISCONNECT)
        .into_iter()
        .filter(|r| text(r, "reason").as_deref() == Some("chassis_restart"))
        .cloned()
        .collect();
    let conns: Vec<Value> = swept.iter().map(|r| r["conn"].clone()).collect();
    assert_eq!(
        conns,
        vec![serde_json::json!(7), serde_json::json!(9)],
        "BOTH orphans are closed — the one before the broken line and, which is \
         the sentinel, the one AFTER it"
    );
}

/// **THE SWEEP RUNS ONCE PER GENERATION, AT ITS FIRST RECORD.**
///
/// It replays up to `2 × LEDGER_MAX_BYTES`, so running it per record would make
/// every subscribe cost a file read of the whole history — and it would write a
/// fresh `chassis_restart` for the same orphan every time, which would make the
/// pairing worse the longer the producer ran.
///
/// **MUTATION.** Move `self.close_orphans()` out of the `if !self.started`
/// block and the count arm reddens with one close per record.
#[test]
fn the_sweep_runs_once_per_generation_and_not_once_per_record() {
    let w = wire("threads", "boot-new");
    seed(&w.path, &[stale_connect("boot-dead", 7, "accounts")]);

    for conn in [11, 13, 15] {
        w.ledger.connected("budgets", None, conn, Some(who("alive")));
    }
    w.ledger.disconnected("budgets", 11, CloseReason::Unsubscribed);

    let records = w.records();
    assert_eq!(
        only(&records, EVENT_DISCONNECT)
            .iter()
            .filter(|r| text(r, "reason").as_deref() == Some("chassis_restart"))
            .count(),
        1,
        "one orphan, one close, however many records follow"
    );
    assert_eq!(
        only(&records, EVENT_CHASSIS_START).len(),
        1,
        "and the generation announces itself once, for the same reason"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 12. There is no period anywhere in this module — doctrine 4
// ─────────────────────────────────────────────────────────────────────────────

/// **A DEFERRED CLOSE IS NOT WRITTEN BY 250 ms OF WALL CLOCK. IT IS WRITTEN BY
/// THE NEXT EVENT OR BY A FLUSH, AND BY NOTHING ELSE.**
///
/// **The period this outlasts is 250 ms, and the honest statement is that the
/// module declares no period at all** — no TTL on an entry, no heartbeat vote,
/// no liveness probe, no sweep timer, no flush interval. That is the design
/// (*"the chassis should handle this… we can detect disconnects &
/// reconnects"*), so the longest period in the path is zero and 250 ms outlasts
/// it by any margin that matters. A "nothing fired" asserted at 10 ms against a
/// thing that ticks at 30 s proves nothing; this asserts it against a thing that
/// does not tick, and says so.
///
/// It is read WITHOUT flushing, because `Wired::records` flushes and would be
/// the probe witnessing its own write.
///
/// The counterpart is both halves of "and by nothing else": the next EVENT
/// writes it, and so does an explicit flush. Without those this passes against
/// a module that never writes a close at all.
///
/// **MUTATION.** Add any timer that flushes the deferral and the first arm
/// reddens. Have `flush` drop the deferral and the third arm reddens.
#[test]
fn a_deferred_close_is_not_written_by_250ms_of_wall_clock_only_by_an_event() {
    let w = wire("threads", "boot-a");
    w.ledger.connected("accounts", Some("cc1"), 7, Some(who("a")));
    w.ledger.disconnected("accounts", 7, CloseReason::Unsubscribed);

    let before = std::time::Instant::now();
    std::thread::sleep(Duration::from_millis(250));
    let waited = before.elapsed();
    assert!(
        waited >= Duration::from_millis(250),
        "SENTINEL: the test actually waited ({waited:?}) — a negative test that \
         did not outlast its period proves nothing"
    );

    let held = records_at(&w.path);
    assert!(
        only(&held, EVENT_DISCONNECT).is_empty(),
        "250 ms is longer than every period in this module, because there are \
         none — so nothing wrote the deferred close in that time"
    );
    assert_eq!(
        held.iter()
            .map(|r| event_of(r).to_string())
            .collect::<Vec<_>>(),
        vec![EVENT_CHASSIS_START, EVENT_CONNECT],
        "and the registry's own removal was immediate, which the file does not \
         show and `shapes.list` does"
    );
    assert!(
        w.ledger.consumers("accounts").is_empty(),
        "the REGISTRY closed at once: `shapes.list` must never report a \
         subscription that has ended, not for one event and not for 250 ms"
    );

    // Counterpart A — the next EVENT writes it.
    w.ledger.connected("budgets", None, 9, Some(who("b")));
    let after_event = records_at(&w.path);
    assert_eq!(
        only(&after_event, EVENT_DISCONNECT).len(),
        1,
        "one event of lookahead, and the event is what flushes it"
    );

    // Counterpart B — and so does an explicit flush.
    w.ledger.disconnected("budgets", 9, CloseReason::Unsubscribed);
    assert_eq!(
        only(&records_at(&w.path), EVENT_DISCONNECT).len(),
        1,
        "the second close is held, as the first was"
    );
    w.ledger.flush();
    assert_eq!(
        only(&records_at(&w.path), EVENT_DISCONNECT).len(),
        2,
        "and a flush is the other thing that writes one"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 13. Degraded regimes — doctrine 10, each naming its population
// ─────────────────────────────────────────────────────────────────────────────

/// **A JOURNAL WHOSE DIRECTORY CANNOT BE MADE NEVER FAILS A SUBSCRIBE, AND THE
/// SUITE NAMES WHO IS UNRECORDED.**
///
/// Bookkeeping that can refuse service is a worse defect than the blindness it
/// was added to cure. Here the `links` directory cannot be created because its
/// parent is a FILE, which is a real failure mode (a half-restored data root, a
/// name collision) and not an injected one.
///
/// Doctrine 10's companion: the degraded path working is not the whole claim.
/// With TWO subscribers live and nothing on disk, the population whose history
/// is being lost is exactly both of them, and that is asserted by connection id
/// — because a correct test of a degraded path with nothing naming who is on it
/// is indistinguishable from coverage.
///
/// **MUTATION.** Make `Journal::push` propagate the `create_dir_all` error (or
/// `unwrap` it) and the "never fails a subscribe" arms redden. Make it retry the
/// directory on every record and the "wrote nothing" arm still holds — which is
/// why the registry arms are the load-bearing ones.
#[test]
fn an_unwritable_journal_never_fails_a_subscribe_and_the_unrecorded_are_named() {
    let dir = tempfile::tempdir().unwrap();
    // `blocker` is a FILE, so `blocker/links/` cannot be created.
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"not a directory").unwrap();
    let path = ledger_path(&blocker, "threads");

    let l = ConsumerLedger::new();
    l.attach(path.clone(), "threads", "boot-a", "test-1.0");

    l.connected("accounts", Some("cc1"), 7, Some(who("tui")));
    l.connected("accounts", None, 9, Some(who("aui")));
    l.disconnected("accounts", 9, CloseReason::Unsubscribed);
    l.flush();

    // The service was never refused.
    let rows = l.consumers("accounts");
    assert_eq!(
        rows.len(),
        1,
        "every call was served and the registry is exactly right: a ledger \
         failure must never fail a subscribe"
    );
    assert_eq!(rows[0].conn, 7);
    assert_eq!(rows[0].name.as_deref(), Some("tui"));
    assert!(!path.exists(), "and nothing was written, because nothing could be");
    assert!(
        !path.parent().unwrap().exists(),
        "the directory could not be made, which is the regime under test"
    );

    // ── WHO IS IN THE REGIME ─────────────────────────────────────────────────
    let unrecorded: Vec<ConnId> = rows.iter().map(|r| r.conn).collect();
    assert_eq!(
        unrecorded,
        vec![7],
        "conn 7's connect is unrecorded, by name — and so was conn 9's whole \
         lifetime, which is the history this regime silently costs"
    );
    assert_eq!(
        l.path().as_deref(),
        Some(path.as_path()),
        "the ledger still reports the path it is TRYING to write, so a \
         diagnostic can say where the history was supposed to go"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 14. The small exact things
// ─────────────────────────────────────────────────────────────────────────────

/// **EVERY `CloseReason` HAS ITS OWN WIRE STRING.**
///
/// A reader tallying stale consumers branches on these, so two reasons sharing a
/// word is two different facts arriving as one. Asserted exhaustively AND
/// pairwise-distinct: the exact-value arm catches a renamed word and the
/// distinctness arm catches a constant implementation that returns the same word
/// for everything, which the exact-value arm alone would not if the word it
/// returned happened to be the one being checked.
///
/// **MUTATION.** Return `"unsubscribed"` from every arm and the distinctness
/// assertion reddens. Rename one word and its exact-value row reddens.
#[test]
fn every_close_reason_has_its_own_wire_string() {
    let all = [
        (CloseReason::Unsubscribed, "unsubscribed"),
        (CloseReason::ConnectionClosed, "connection_closed"),
        (CloseReason::Resubscribed, "resubscribed"),
        (CloseReason::ChassisRestart, "chassis_restart"),
    ];
    for (reason, word) in all {
        assert_eq!(reason.as_str(), word, "{reason:?} is on the wire as {word}");
    }
    let words: BTreeSet<&str> = all.iter().map(|(r, _)| r.as_str()).collect();
    assert_eq!(
        words.len(),
        4,
        "four reasons, four distinct words — a shared word is two facts arriving \
         as one"
    );
}

/// **THE JOURNAL'S PATH IS `links/<chassis>.producer.jsonl` INSIDE THE SERVICE
/// DIRECTORY THE CALLER RESOLVED, AND IT RESOLVES NOTHING ITSELF.**
///
/// The cauldron-side departure from substrate, asserted rather than described:
/// substrate's `producer_ledger_path` read `$CAULDRON_HOME`, `$HOME`,
/// `$CAULDRON_ENV` and `$ENV` and re-implemented `libs/env`'s precedence and
/// name rule. This crate's root commits to the opposite and `Cargo.toml`
/// declares `ledger = []`, so there is no dependency it could read them WITH.
///
/// The second arm is the one that keeps it that way: the SAME chassis name under
/// two different service directories gives two different paths with nothing in
/// the process environment involved. A function that consulted the environment
/// would be unable to make that true.
///
/// **MUTATION.** Have `ledger_path` join `LEDGER_DIR` after the file name, or
/// drop the segment, and the shape arm reddens. Have it create the directory and
/// the third arm reddens.
#[test]
fn the_journal_path_is_links_inside_the_service_directory_and_resolves_nothing() {
    let service = Path::new("/var/empty/data/prod/threads");
    assert_eq!(
        ledger_path(service, "threads"),
        Path::new("/var/empty/data/prod/threads/links/threads.producer.jsonl"),
        "`<service_data>/links/<chassis>.producer.jsonl`"
    );
    assert_eq!(LEDGER_DIR, "links", "the kind segment names the thing in it");
    assert_eq!(
        ledger_file_name("tts"),
        "tts.producer.jsonl",
        "`.producer.` reserves `<consumer>.consumer.jsonl` for the other end"
    );

    // Two environments, one chassis, two paths — decided entirely by the
    // argument, which is what "it resolves nothing" means.
    assert_ne!(
        ledger_path(Path::new("/r/data/prod/threads"), "threads"),
        ledger_path(Path::new("/r/data/dev/threads"), "threads"),
        "the environment is the CALLER's answer, and this module has no second \
         copy of the precedence rule to drift from it"
    );

    // And it touches no filesystem.
    let dir = tempfile::tempdir().unwrap();
    let path = ledger_path(dir.path(), "threads");
    assert!(!path.exists());
    assert!(
        !path.parent().unwrap().exists(),
        "computing a path creates nothing; the journal creates its directory \
         lazily at its first record"
    );
}
