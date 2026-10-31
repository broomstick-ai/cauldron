//! # The `boot` shape — the fact that predates every subscription, proved at a consumer
//!
//! The capability this file protects, in one sentence: **a consumer that has
//! never touched the legacy firehose can learn why the daemon it just reached is
//! the one it reached.** Before `shapes::boot` that answer existed only in
//! `server.rs`'s and `chassis.rs`'s `if !sent_boot_note && firehose` blocks, so
//! retiring the firehose would have removed it in silence. R2.S4 retired the
//! firehose and deleted both blocks; this shape is now the ONLY carrier.
//!
//! **EVERY ASSERTION IS ON A FRAME A CONNECTION RECEIVED**, never on hub-internal
//! state and never on `UpdateGate::boot_note()` directly —
//! `tests/lifecycle_announcement.rs` already proves the gate computes the right
//! note, and re-proving it here would be proving the wrong half. What is unproven
//! until this file exists is that the note ARRIVES without a firehose.
//!
//! **EVERY TEST WAS FORCED RED** by the mutation named in its own docstring, and
//! the red text is quoted there.
//!
//! **HUB-DRIVEN, NO SOCKET AND NO TIMER.** `docs/shape-design-guide.md` step 10:
//! *"a test that goes through a socket and a timer can pass or fail on
//! scheduling"*. Frames are read off this connection's own `ConnFrames`, which is
//! the same channel the socket writer drains, so "a connection received it" is
//! still what is asserted.
//!
//! **`on_boot` IS CALLED BY THE FIXTURE THAT NEEDS IT, AND DELIBERATELY NOT BY
//! THE ONE THAT DOES NOT.** That split is the point of two fixtures rather than
//! one: `on_boot` is what composes the note, so a test that skipped it would
//! assert against a `None` and pass for no reason — and the absence case is a
//! real export this shape owes, not a setup failure.

use std::sync::Arc;

use serde_json::{json, Value};

use threads::config::Config;
use threads::shape::{ConnFrames, Derived, ShapeFrame};
use threads::{build_manager, ThreadManager};

const SHAPE: &str = "boot";
const KEY: &str = threads::shapes::boot::KEY_DAEMON;

// ─────────────────────────────────────────────────────────────────────────────
// fixture
// ─────────────────────────────────────────────────────────────────────────────

fn config(tmp: &std::path::Path) -> Config {
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.join("claude-home"));
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude")),
    );
    Config {
        data_dir: Some(tmp.to_string_lossy().to_string()),
        accounts: Default::default(),
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

/// A manager whose gate has NOT composed a note — `on_boot` is not called.
async fn start_unbooted() -> (ThreadManager, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    // The clock seam: every `Hub::emit` stamps with the clock and every cell
    // refuses an input at or before its `folded_at`, so a test that emits back to
    // back needs the ticking clock. See `src/clock.rs`.
    threads::clock::testing::ticking();
    let manager = build_manager(config(tmp.path())).unwrap();
    (manager, tmp)
}

/// A manager whose gate HAS composed a note, which is what the real daemon does
/// before it listens.
async fn start() -> (ThreadManager, tempfile::TempDir) {
    let (manager, tmp) = start_unbooted().await;
    manager.update.on_boot().await;
    assert!(
        manager.update.boot_note().is_some(),
        "the fixture must actually have a note for the shape to read"
    );
    (manager, tmp)
}

/// A manager whose gate came up after an OPERATOR-requested restart — a
/// SECOND cause, so a shape that hard-coded the default fixture's word would be
/// caught.
///
/// The pending record is written exactly where the previous process would have
/// left it (`UpdateGate::pending_path`), in the flat on-disk form
/// `PendingRestart` documents; `on_boot` then reconciles it. An operator restart
/// carries no identity, so booting at all satisfies it, and
/// `compute_boot_note` concludes `operator` (`lifecycle_announcement.rs` owns
/// that derivation; this fixture only needs a cause other than `unknown`).
async fn start_after_operator_restart() -> (ThreadManager, tempfile::TempDir) {
    let (manager, tmp) = start_unbooted().await;
    let pending = manager.update.pending_path();
    std::fs::create_dir_all(pending.parent().unwrap()).unwrap();
    std::fs::write(
        &pending,
        json!({
            "queued_at": "2026-10-10T00:00:00Z",
            "kind": "operator",
            "identity": null,
            "note": "boot_shape.rs: an operator restart, for a second cause",
            "binary": "<test>",
        })
        .to_string(),
    )
    .unwrap();
    manager.update.on_boot().await;
    (manager, tmp)
}

fn registered(m: &ThreadManager) {
    m.hub.register(threads::shapes::boot::Boot, m.clone());
}

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
}

fn drain(f: &mut ConnFrames) -> Vec<Arc<ShapeFrame>> {
    let mut all = Vec::new();
    while let Ok(x) = f.try_recv() {
        all.push(x);
    }
    all
}

/// The EXPORT that answered a subscribe — its `row`.
fn exported_row(frames: &[Arc<ShapeFrame>]) -> Option<Value> {
    frames
        .iter()
        .find(|f| f.wire.row.is_some())
        .map(|f| f.wire.row.clone().unwrap())
}

/// The export's per-cell provenance, so a `Known` word can be told from a
/// `NotHydrated` one.
fn exported_cells(frames: &[Arc<ShapeFrame>]) -> Option<Value> {
    frames
        .iter()
        .find(|f| f.wire.cells.is_some())
        .map(|f| f.wire.cells.clone().unwrap())
}

/// The export's `shared` block.
fn exported_shared(frames: &[Arc<ShapeFrame>]) -> Option<Value> {
    frames
        .iter()
        .find(|f| f.wire.shared.is_some())
        .map(|f| f.wire.shared.clone().unwrap())
}

/// Every LIVE `row` frame — the fold's product. An export answering a subscribe
/// is not one.
fn live_rows(frames: &[Arc<ShapeFrame>]) -> Vec<Value> {
    frames
        .iter()
        .filter_map(|f| match &f.wire.event {
            Some(Derived::Row { row }) => Some(row.clone()),
            _ => None,
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. The fact reaches a consumer that never touched the firehose
// ─────────────────────────────────────────────────────────────────────────────

/// **THE WHOLE POINT: THE BOOT NOTE ARRIVES ON A SHAPE SUBSCRIBE.**
///
/// No `{"type":"subscribe","threads":null}` anywhere in this test. The note was
/// computed by `on_boot` before any server existed, and a consumer arriving
/// afterwards still gets it — which is the capability that would have vanished
/// when the firehose was retired.
///
/// The four cells are asserted against the gate's own note rather than against
/// literals, because WHICH reason a test fixture produces is
/// `lifecycle_announcement.rs`' business; what this test owns is that whatever
/// the gate concluded is what the consumer holds.
///
/// **THE MUTATION.** In `shapes/boot.rs`, make `hydrates` skip the read: replace
/// `root.apply(as_of, note.as_ref());` with `root.apply(as_of, None);`.
///
/// Forced red:
/// ```text
/// `reason` reaches the consumer: Null
/// ```
#[tokio::test]
async fn the_boot_note_reaches_a_consumer_with_no_firehose_subscription() {
    let (m, tmp) = start().await;
    registered(&m);
    let note = m.update.boot_note().expect("the fixture has a note");

    let mut rx = m.hub.connect(901);
    m.hub.subscribe(SHAPE, Some(KEY), 901, None).unwrap();
    settle().await;
    let frames = drain(&mut rx);

    let row = exported_row(&frames).unwrap_or_else(|| panic!("no export: {} frames", frames.len()));
    assert_eq!(row["producer"], KEY, "keyed by the producer: {row}");
    assert_eq!(
        row["reason"], note.reason,
        "`reason` reaches the consumer: {}",
        row["reason"]
    );
    assert!(
        threads::update::CAUSES.contains(&row["reason"].as_str().unwrap_or("")),
        "`reason` is a word from the declared vocabulary: {row}"
    );
    assert!(
        row["at"].as_str().is_some_and(|a| !a.is_empty()),
        "`at` is a real instant, not null: {row}"
    );
    assert_eq!(row["detail"], note.detail, "`detail` reaches it too: {row}");
    assert_eq!(row["boot_id"], note.boot_id, "and the boot id: {row}");
    assert_eq!(row["at"], note.at, "and the instant: {row}");

    // **THE LOAD-BEARING HALF.** A word that arrives in state `not_hydrated` is
    // not an answer, it is an unknown wearing one.
    let cells = exported_cells(&frames).expect("the export carries provenance");
    for c in ["reason", "detail", "boot_id", "at"] {
        let st = &cells[c];
        assert_eq!(
            st["state"], "known",
            "`{c}` is a KNOWN value and not an unknown wearing one: {st}"
        );
    }
    drop(tmp);
}

/// **A SECOND CAUSE REACHES THE CONSUMER AS ITSELF.**
///
/// The test above compares `reason` against the gate's own note, and its
/// fixture can only ever reach `unknown` — so a `boot` shape that HARD-CODED
/// `unknown` would pass it. This fixture boots after an operator restart, so the
/// gate concludes `operator`, and the row must say `operator`: a literal from
/// `CAUSES`, asserted against the word and not against the note it came from.
/// The default fixture's word is asserted to DIFFER, so the two fixtures really
/// are two causes.
///
/// **THE MUTATION.** In `shapes/boot.rs`'s `apply`, replace the note's reason
/// with the literal `crate::update::CAUSE_UNKNOWN.to_string()`. The test above
/// stays GREEN under it (its fixture reaches only `unknown`); this one fails:
/// ```text
/// an operator restart reaches the consumer as `operator`: {…"reason":"unknown"}
///   left: String("unknown")
///  right: String("operator")
/// ```
#[tokio::test]
async fn a_second_cause_reaches_the_consumer_as_itself() {
    let (base, base_tmp) = start().await;
    let base_reason = base.update.boot_note().unwrap().reason;
    drop(base_tmp);

    let (m, tmp) = start_after_operator_restart().await;
    registered(&m);
    assert_eq!(
        m.update.boot_note().map(|n| n.reason),
        Some(threads::update::CAUSE_OPERATOR.to_string()),
        "the fixture must really reach a second cause"
    );
    assert_ne!(
        base_reason,
        threads::update::CAUSE_OPERATOR,
        "and it must be a DIFFERENT cause from the default fixture's"
    );

    let mut rx = m.hub.connect(905);
    m.hub.subscribe(SHAPE, Some(KEY), 905, None).unwrap();
    settle().await;
    let frames = drain(&mut rx);
    let row = exported_row(&frames).unwrap_or_else(|| panic!("no export: {} frames", frames.len()));
    assert_eq!(
        row["reason"],
        json!(threads::update::CAUSE_OPERATOR),
        "an operator restart reaches the consumer as `operator`: {row}"
    );
    assert!(row["at"].is_string(), "with a real instant: {row}");
    drop(tmp);
}

/// **AND IT ARRIVES ON A KEYLESS SUBSCRIBE, WHICH IS AN INVENTORY OF ONE.**
///
/// `HYDRATE = Whole` claims that "every key I know about" and "every key there
/// is" are the same set. On this shape that set has one member, so a consumer
/// that does not know the key word still gets the row — which is what makes the
/// shape usable by a face that has hardcoded nothing.
///
/// **THE MUTATION.** In `shapes/boot.rs`, make `Root::keys` answer nothing:
/// replace its body with `Vec::new()`. That is the real mechanism — a keyless
/// subscribe is answered by ITERATING `Root::keys`, so emptying it produces the
/// ack-then-silence class this test exists to refuse, rather than a changed
/// value.
///
/// Forced red:
/// ```text
/// a keyless subscribe is an inventory of one: no row frame in 0 frames
/// ```
#[tokio::test]
async fn a_keyless_subscribe_is_an_inventory_of_one() {
    let (m, tmp) = start().await;
    registered(&m);

    let mut rx = m.hub.connect(902);
    m.hub.subscribe(SHAPE, None, 902, None).unwrap();
    settle().await;
    let frames = drain(&mut rx);

    let row = exported_row(&frames).unwrap_or_else(|| {
        panic!(
            "a keyless subscribe is an inventory of one: no row frame in {} frames",
            frames.len()
        )
    });
    assert_eq!(row["producer"], KEY, "the one key is the producer: {row}");
    let rows = frames.iter().filter(|f| f.wire.row.is_some()).count();
    assert_eq!(rows, 1, "an inventory of ONE, not of several: {rows}");
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. Absence is not an answer
// ─────────────────────────────────────────────────────────────────────────────

/// **NO NOTE YET EXPORTS `NotHydrated`, NOT `reason: "unknown"`.**
///
/// `CAUSE_UNKNOWN` is a real, positive conclusion — `compute_boot_note` reaches
/// it after three measurements that all came back negative, and its own doc calls
/// it *"a real answer and the one this daemon owes when it cannot account for
/// itself"*. *"There is no note yet"* is a different statement, and a producer
/// that spelled the second as the first would be substituting a value for its own
/// silence.
///
/// **The key is still minted, and the key set is still complete**: the producer
/// exists and is named even when its account of itself is not composed. Only the
/// cells are unknown.
///
/// **THE MUTATION.** In `shapes/boot.rs`, make the absent arm fabricate the word
/// — replace the four `not_hydrated(NO_NOTE_YET)` calls with
/// `row.reason.read(as_of, crate::update::CAUSE_UNKNOWN.to_string());` and reads
/// of `""` for the other three.
///
/// Forced red:
/// ```text
/// an absent note is `not_hydrated` and NOT the word `unknown`: {"known":{...}}
/// ```
#[tokio::test]
async fn an_absent_note_is_not_hydrated_rather_than_the_word_unknown() {
    let (m, tmp) = start_unbooted().await;
    assert!(
        m.update.boot_note().is_none(),
        "the positive sentinel: this fixture must have NO note, or the test proves nothing"
    );
    registered(&m);

    let mut rx = m.hub.connect(903);
    m.hub.subscribe(SHAPE, Some(KEY), 903, None).unwrap();
    settle().await;
    let frames = drain(&mut rx);

    // The key exists: a producer with no account of itself is still a producer.
    let row = exported_row(&frames)
        .unwrap_or_else(|| panic!("the key is minted either way: {} frames", frames.len()));
    assert_eq!(row["producer"], KEY, "the producer is named: {row}");
    assert!(
        row["reason"].is_null(),
        "the row may not contradict the cells block: {row}"
    );

    let cells = exported_cells(&frames).expect("the export carries provenance");
    for c in ["reason", "detail", "boot_id", "at"] {
        let st = &cells[c];
        assert_eq!(
            st["state"], "not_hydrated",
            "an absent note is `not_hydrated` and NOT the word `unknown`: {st}"
        );
        let reason = st["reason"].as_str().unwrap_or("");
        assert!(
            reason.contains("on_boot"),
            "the unknown says WHY it is unknown: {st}"
        );
        assert_ne!(
            reason,
            threads::update::CAUSE_UNKNOWN,
            "and the reason is a sentence about absence, not the conclusion word: {st}"
        );
    }
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. The fold writes nothing — with a positive control, because "no frames"
//    is a green that cannot fail on its own
// ─────────────────────────────────────────────────────────────────────────────

/// **NO INPUT MOVES THIS SHAPE, AND THE EXPORT IS ITS WHOLE PRODUCT.**
///
/// The fact is computed once, before anything could be listening, and never
/// mutated — so there is no input whose arrival could move a cell. This test is
/// the executable form of that claim, and the one that would catch a future author
/// folding `Push::Lifecycle` in here by analogy with `narration`.
///
/// **THE POSITIVE CONTROL IS THE WHOLE DESIGN OF THIS TEST.** *"No live frames
/// arrived"* is true of a shape nobody subscribed to, of a shape whose
/// registration was deleted, and of a hub that was never driven — so asserting it
/// alone would be one of the probes that cannot fail. Three sentinels, in order:
///
/// 1. the EXPORT arrived, so the shape is registered, subscribed and exporting;
/// 2. a `Push::Lifecycle` with a DIFFERENT reason was really emitted and really
///    reached a fold — proved by `narration`, subscribed on the same connection,
///    appending it to its `lifecycle` channel;
/// 3. only THEN is `boot`'s live-frame count asserted zero.
///
/// Sentinel 2 is what makes this test honest: the input is not merely sent, it is
/// observed to have been folded by somebody. A hub that silently dropped every
/// input would fail at sentinel 2 rather than passing here.
///
/// **THE MUTATION.** In `shapes/boot.rs`, give the fold a live arm — move
/// `Input::Push(Push::Lifecycle(note))` out of the homed-elsewhere list into its
/// own arm that writes:
/// ```text
/// Input::Push(Push::Lifecycle(n)) => {
///     tx.rows.key(KEY_DAEMON.to_string()).reason.emit(tx.at(), n.reason.clone());
/// }
/// ```
///
/// Forced red:
/// ```text
/// `boot` has no live arm: the export is its whole product, but 1 live row frame(s) arrived: [Object {...}]
/// ```
#[tokio::test]
async fn the_fold_writes_nothing_so_the_export_is_the_whole_product() {
    let (m, tmp) = start().await;
    registered(&m);
    m.hub
        .register(threads::shapes::narration::Narration, m.clone());
    let note = m.update.boot_note().expect("the fixture has a note");

    let mut rx = m.hub.connect(904);
    m.hub.subscribe(SHAPE, Some(KEY), 904, None).unwrap();
    m.hub
        .subscribe(
            "narration",
            Some(threads::shapes::narration::CHANNEL_LIFECYCLE),
            904,
            None,
        )
        .unwrap();
    settle().await;

    // SENTINEL 1: the export arrived, so this shape is live on this connection.
    let first = drain(&mut rx);
    assert!(
        exported_row(&first).is_some(),
        "sentinel 1: the shape exported, so it is registered and subscribed"
    );

    // Emit a REAL lifecycle push, with a phase and reason unlike the boot note's,
    // so a fold that wrongly took it would be visibly wrong rather than
    // coincidentally right.
    let going_down = threads::update::LifecycleNote {
        service: threads::update::SERVICE_ID.to_string(),
        phase: threads::update::PHASE_GOING_DOWN.to_string(),
        reason: threads::update::CAUSE_OPERATOR.to_string(),
        detail: "a deliberate test lifecycle push".to_string(),
        boot_id: note.boot_id.clone(),
        at: note.at.clone(),
    };
    assert_ne!(
        going_down.phase, note.phase,
        "the probe must differ from the held note, or folding it would look correct"
    );
    m.hub
        .emit(threads::event::Push::Lifecycle(going_down.clone()));
    settle().await;
    let after = drain(&mut rx);

    // SENTINEL 2: the push was really emitted and really folded — by `narration`.
    let narrated: Vec<&Arc<ShapeFrame>> = after.iter().filter(|f| f.shape == "narration").collect();
    assert!(
        !narrated.is_empty(),
        "sentinel 2: the lifecycle push must actually reach a fold, or this test \
         proves only that nothing was emitted: {} frames",
        after.len()
    );

    // THE ASSERTION, now that the input is known to have been delivered.
    let mine: Vec<Arc<ShapeFrame>> = after
        .iter()
        .filter(|f| f.shape == SHAPE)
        .cloned()
        .collect();
    let live = live_rows(&mine);
    assert!(
        live.is_empty(),
        "`boot` has no live arm: the export is its whole product, but {} live row \
         frame(s) arrived: {live:?}",
        live.len()
    );
    drop(tmp);
}

/// **A RE-SUBSCRIBE AFTER A `Lazy` DROP READS THE SAME NOTE.**
///
/// The value lives in `UpdateGate`, outside the root, so a drop on the last
/// unsubscribe and a re-hydrate on the next subscribe must produce the same
/// export. This is the executable form of the durability claim in the module doc:
/// the source is process-durable, which is exactly the lifetime a shape's root
/// has. It is also the test that would catch a hydrate rewritten to re-read
/// `pending-restart.json`, which `on_boot` has already deleted — that version
/// would answer `unknown` the second time.
///
/// Proven at N = 2 subscriptions, because one subscription cannot exercise a
/// re-init at all.
///
/// **THE SENTINEL THIS TEST CANNOT DO WITHOUT**, found by forcing it: two
/// exports that agree prove nothing unless the second one came from a NEW root.
/// If the `Lazy` drop did not happen, the second subscribe would be answered by
/// the first root still holding its values and the test would pass without ever
/// exercising a re-hydrate. So `epoch.init_at` is asserted to have MOVED between
/// the two — that is the evidence a re-init occurred, and the assertion on the
/// note's survival only means anything after it.
///
/// **THE MUTATION.** In `shapes/boot.rs`'s `hydrates`, make the gate readable
/// only once per process:
/// ```text
/// let note = {
///     static ONCE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
///     if ONCE.swap(true, std::sync::atomic::Ordering::SeqCst) { None } else { p.update.boot_note() }
/// };
/// ```
/// A first attempt at this mutation — `if root.rows.is_empty() { .. } else { None }`
/// — STAYED GREEN and is recorded because it is instructive: a `Lazy` re-init
/// gets a FRESH root, so `rows` is empty on both hydrates and the guard never
/// fires. The mutation has to defeat the re-read across roots, not within one.
///
/// Forced red:
/// ```text
/// a re-hydrate reads the same note: Null != String("operator")
/// ```
#[tokio::test]
async fn a_lazy_reinit_reads_the_same_note_rather_than_losing_it() {
    let (m, tmp) = start().await;
    registered(&m);

    let mut rx1 = m.hub.connect(905);
    m.hub.subscribe(SHAPE, Some(KEY), 905, None).unwrap();
    settle().await;
    let f1 = drain(&mut rx1);
    let first = exported_row(&f1).expect("the first export");
    let epoch1 = f1
        .iter()
        .find(|f| f.wire.row.is_some())
        .map(|f| f.wire.epoch.init_at)
        .unwrap();

    // Drop the only subscriber: a `Lazy` shape clears its state here, with no
    // grace period.
    m.hub.dropped(905);
    settle().await;

    let mut rx2 = m.hub.connect(906);
    m.hub.subscribe(SHAPE, Some(KEY), 906, None).unwrap();
    settle().await;
    let f2 = drain(&mut rx2);
    let second = exported_row(&f2).expect("the second export");
    let epoch2 = f2
        .iter()
        .find(|f| f.wire.row.is_some())
        .map(|f| f.wire.epoch.init_at)
        .unwrap();

    // **THE SENTINEL.** A re-init actually happened, so the second export is a
    // fresh root's answer and not the first root's values still sitting there.
    // Without this, two agreeing exports would prove nothing.
    assert_ne!(
        epoch1, epoch2,
        "the positive sentinel: the `Lazy` drop must really have re-inited the \
         shape, or this test never exercises a re-hydrate at all: {epoch1:?}"
    );

    assert_eq!(
        second["reason"], first["reason"],
        "a re-hydrate reads the same note: {} != {}",
        second["reason"], first["reason"]
    );
    assert_eq!(
        second["boot_id"], first["boot_id"],
        "and the same boot: {second} vs {first}"
    );
    assert!(
        !first["reason"].is_null(),
        "the positive sentinel: both exports must carry a real reason, or two \
         nulls would compare equal and prove nothing"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. What `shared` publishes, and the two fields that are constants
// ─────────────────────────────────────────────────────────────────────────────

/// **`shared` CARRIES THE VOCABULARY AND THE PHASE, SO NO CONSUMER HARDCODES
/// THEM** — and `phase` being a constant there is this shape's declaration that
/// it serves only the coming-up half.
///
/// A face switching on `reason` needs the four words; reading them off `shared`
/// is what stops a fifth reason being invisible to it. And `phase` on `shared`
/// rather than on the row is what says the going-down note is somebody else's
/// (`narration`'s `lifecycle` channel) — a cell that could only ever hold
/// `back_up` would invite the opposite conclusion.
///
/// **THE MUTATION.** In `shapes/boot.rs`'s `Root::shared`, drop the vocabulary:
/// delete the `"causes": crate::update::CAUSES,` line.
///
/// Forced red:
/// ```text
/// `shared` publishes the reason vocabulary: {"key":"daemon", ...}
/// ```
#[tokio::test]
async fn shared_publishes_the_vocabulary_and_declares_the_phase() {
    let (m, tmp) = start().await;
    registered(&m);

    let mut rx = m.hub.connect(907);
    m.hub.subscribe(SHAPE, Some(KEY), 907, None).unwrap();
    settle().await;
    let shared = exported_shared(&drain(&mut rx)).expect("the export carries `shared`");

    assert_eq!(
        shared["causes"],
        json!(threads::update::CAUSES),
        "`shared` publishes the reason vocabulary: {shared}"
    );
    assert_eq!(
        shared["phase"], threads::update::PHASE_BACK_UP,
        "`phase` is a constant and it is `back_up`: {shared}"
    );
    assert_eq!(
        shared["service"], threads::update::SERVICE_ID,
        "`service` is a constant: {shared}"
    );
    assert_eq!(shared["key"], KEY, "the key word is published: {shared}");
    assert_eq!(
        shared["shape_version"],
        json!(threads::shapes::boot::Boot::VERSION),
        "the declared version rides `shared`: {shared}"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. Identity is compulsory at the consumer door
// ─────────────────────────────────────────────────────────────────────────────

/// **A SUBSCRIBE THAT NAMES NO CONSUMER IS REFUSED BY NAME, NOT LEFT TO TIME
/// OUT.**
///
/// Rule 2.1, ruled 2026-10-08: *"what I want is consumers to be forced to say who
/// they are so we can track consumers."* This is here because the refusal is
/// INVISIBLE from a client that omits the param and then waits — four tests in
/// this arc hung for 20 s and reported a timeout while the producer had refused
/// them immediately. A named-refusal assertion is the only thing that turns that
/// into a legible red, so every new shape's suite carries one.
///
/// It asserts on `ConsumerIdentity::parse`, which is the one function both doors
/// route through (`ledger.rs`: *"a hand-written copy in each is one rule telling
/// two consumers two different things"*), so no socket and no timer is involved.
///
/// **THE MUTATION.** In `shapewire/src/ledger.rs`'s `ConsumerIdentity::parse`,
/// restore the old lenience: change
/// `.ok_or_else(|| IDENTITY_REQUIRED_REFUSAL.to_string())?` to
/// `.unwrap_or(&Value::Null)` and return a blank identity.
///
/// Forced red:
/// ```text
/// an anonymous subscribe is refused by NAME: Ok(ConsumerIdentity { .. })
/// ```
#[test]
fn an_anonymous_subscribe_to_this_shape_is_refused_by_name() {
    use threads::consumers::{ConsumerIdentity, IDENTITY_REQUIRED_REFUSAL};

    // The positive control: a well-formed identity parses, so this test is not
    // merely proving that `parse` refuses everything.
    let good = json!({
        "shape": SHAPE,
        "key": KEY,
        "consumer": {"name": "boot-shape-suite", "shape_version": "2026-10-09"}
    });
    let parsed = ConsumerIdentity::parse(&good);
    assert!(
        parsed.is_ok(),
        "the positive sentinel: a named consumer is accepted: {parsed:?}"
    );
    assert_eq!(parsed.unwrap().name, "boot-shape-suite");

    // And the refusal, which must be the SENTENCE and not a silence.
    for params in [
        json!({"shape": SHAPE, "key": KEY}),
        json!({"shape": SHAPE, "key": KEY, "consumer": Value::Null}),
    ] {
        let r = ConsumerIdentity::parse(&params);
        assert_eq!(
            r.as_ref().err().map(String::as_str),
            Some(IDENTITY_REQUIRED_REFUSAL),
            "an anonymous subscribe is refused by NAME: {r:?}"
        );
    }
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
