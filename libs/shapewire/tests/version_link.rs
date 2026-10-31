//! # RULE 2.6, PROVED: VERSION MISMATCH IS REFUSED AT LINK TIME, LOUDLY
//!
//! `docs/shapes.md` 2.6 said *"**Exists.** Nothing. The shape has no version and
//! the subscribe carries none."* This file is the producer half's evidence, and it
//! lives in `shapewire`'s own tests rather than a producer's because the rule
//! is the chassis's: a second producer must inherit it by linking the crate, not
//! by re-deriving it.
//!
//! ## What every test here is careful about, and why
//!
//! **Each test was forced RED by a named mutation before it was believed**, and
//! the mutation is in the test's own docstring. The docstring also says what the
//! red LOOKED like where the shape of the failure is the interesting part, because
//! a mutation that produces the wrong red is a test measuring the wrong thing.
//!
//! **Nothing here asserts "it did not link".** A negative assertion goes silent
//! when its subject is renamed — delete `ShapeError::VersionRefused` and
//! `assert!(result.is_err())` keeps passing on whatever error arrives next. So the
//! assertions are on the VALUE: the exact variant, the exact numbers it carries,
//! the exact refusal code, and the exact text. The one place a negative claim is
//! unavoidable (*nothing was queued*) is paired with a positive one about the
//! same connection, so the test cannot pass by never establishing a subscription
//! in the first place.
//!
//! **A test that needs a live export PANICS rather than skips.** `expect_export`
//! has no timeout-and-shrug path: a missing frame is a failure, never a test that
//! quietly asserted nothing. That is the lesson rule 2.4's own section records —
//! *"the tests never subscribed, so every one took the not-tracking early return
//! … a test that cannot fail is worse than no test, because it is reported as
//! evidence."*

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};

use shapewire::cell::Cells;
use shapewire::envelope::{codes, Coded, Refusal};
use shapewire::fold::{Outbox, Root};
use shapewire::hub::{Hub, Kind, Shape, Tracking, Tx};
use shapewire::operation::{DType::Scalar, Presence::Required, Scalar::Str};
use shapewire::ledger::ConsumerIdentity;
use shapewire::mirror::{Mirror, VersionMismatch};
use shapewire::wire::{
    Beat, CellState, ConnId, Detail, Link, Retain, ShapeError, ShapeFrame, Stamped, Superseded,
    Timestamp,
};

// ─────────────────────────────────────────────────────────────────────────────
// The smallest producer that can be linked to
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct Beat1;
impl Kind for Beat1 {
    fn kind(&self) -> &'static str {
        "beat"
    }
}

/// A keyed root holding whatever keys the shape put in it, on the pattern
/// `threads/tests/nesting.rs` uses: what is under test is the chassis's LINK
/// handling, and a root a test can set directly is the shortest way to drive it.
#[derive(Default)]
struct Keys {
    keys: BTreeSet<String>,
    dirty: BTreeSet<String>,
}

impl Root for Keys {
    fn keys(&self) -> Vec<String> {
        self.keys.iter().cloned().collect()
    }
    fn row(&self, k: &str) -> Option<Value> {
        self.keys.contains(k).then(|| json!({ "key": k }))
    }
    fn cells(&self, k: &str) -> Option<Value> {
        self.keys.contains(k).then(|| {
            json!({ "key": CellState::Known {
                complete: true,
                folded_at: chrono::Utc::now(),
                since_init: false,
                covers_since: None,
            }})
        })
    }
    fn shared(&self) -> Value {
        json!({})
    }
    fn shared_cells(&self) -> Value {
        json!({})
    }
    fn drain(&mut self) -> Outbox {
        Outbox {
            rows: std::mem::take(&mut self.dirty).into_iter().collect(),
            ..Default::default()
        }
    }
    fn mark_dead(&mut self, _: &str) {}
}

// The one cell `Keys::cells` emits, declared — see `shapewire::cell`. Every
// shape here carries it, and none carries a shape-level cell.
shapewire::declare_cells! {
    KEY_CELLS = [("key", Scalar(Str), Required, [])]
}

/// **THE SHAPE UNDER TEST: `VERSION = 4`, `SERVES = [3]`.**
///
/// Those are rule 2.6's own example numbers — the doc writes the refusal as
/// *"shape accounts: built against v2; this producer serves [4, 3]"* — so the
/// text asserted below is the doc's sentence and not a sentence invented here.
struct Versioned;
impl Shape<Beat1> for Versioned {
    const NAME: &'static str = "versioned";
    const TRACKING: Tracking = Tracking::Lazy;
    const CELLS: Cells = KEY_CELLS;
    const SHARED: Cells = Cells::NONE;
    const VERSION: u32 = 4;
    const SERVES: &'static [u32] = &[3];
    type Producer = ();
    type Root = Keys;
    fn root(&self) -> Keys {
        Keys::default()
    }
    fn hydrates(&mut self, _: &(), root: &mut Keys, _: Timestamp) {
        root.keys.insert("sam".into());
        // A hydrate publishes nothing; the export IS the publication.
        let _ = root.drain();
    }
    fn folds(&mut self, _: &Stamped<Beat1>, _: &mut Tx<'_, Keys>) {}
}

/// **TWO OLD VERSIONS SERVED**, so that "the last stated link wins" has a case at
/// all: with one entry in `SERVES` a connection cannot re-link on a DIFFERENT old
/// contract, and the rule would be unwitnessed.
struct Wide;
impl Shape<Beat1> for Wide {
    const NAME: &'static str = "wide";
    const TRACKING: Tracking = Tracking::Lazy;
    const CELLS: Cells = KEY_CELLS;
    const SHARED: Cells = Cells::NONE;
    const VERSION: u32 = 4;
    const SERVES: &'static [u32] = &[3, 2];
    type Producer = ();
    type Root = Keys;
    fn root(&self) -> Keys {
        Keys::default()
    }
    fn hydrates(&mut self, _: &(), root: &mut Keys, _: Timestamp) {
        root.keys.insert("sam".into());
        let _ = root.drain();
    }
    fn folds(&mut self, _: &Stamped<Beat1>, _: &mut Tx<'_, Keys>) {}
}

/// **THE CONTROL: A SHAPE THAT DECLARES NEITHER CONST.** Whatever this one does
/// is what every shape written before rule 2.6's consts existed must still do, so
/// it is tested rather than assumed — the discipline `threads/tests/nesting.rs`
/// applies to `HYDRATE` and `PARENT`.
struct Undeclared;
impl Shape<Beat1> for Undeclared {
    const NAME: &'static str = "undeclared";
    const TRACKING: Tracking = Tracking::Lazy;
    const CELLS: Cells = KEY_CELLS;
    const SHARED: Cells = Cells::NONE;
    type Producer = ();
    type Root = Keys;
    fn root(&self) -> Keys {
        Keys::default()
    }
    fn hydrates(&mut self, _: &(), root: &mut Keys, _: Timestamp) {
        root.keys.insert("sam".into());
        let _ = root.drain();
    }
    fn folds(&mut self, _: &Stamped<Beat1>, _: &mut Tx<'_, Keys>) {}
}

// ─────────────────────────────────────────────────────────────────────────────
// fixture
// ─────────────────────────────────────────────────────────────────────────────

fn a_hub() -> Arc<Hub<Beat1>> {
    let hub = Arc::new(Hub::<Beat1>::new());
    hub.register(Versioned, (), 64);
    hub.register(Wide, (), 64);
    hub.register(Undeclared, (), 64);
    hub
}

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
}

/// **A SUBSCRIBE AS A CONSUMER MAKES IT**, stating a version. This is
/// `Hub::subscribe_as` and NOT `subscribe_ask`: the whole point of rule 2.6 is
/// that a link states a version, and the mechanical form states none.
fn link(
    hub: &Hub<Beat1>,
    conn: ConnId,
    version: Option<u32>,
) -> Result<u64, ShapeError> {
    hub.subscribe_as(
        "versioned",
        Some("sam"),
        conn,
        None,
        Some(ConsumerIdentity {
            name: "a-consumer".into(),
            shape_version: "a-date".into(),
            pid: None,
            build: None,
        }),
        Detail::Rows,
        Retain::Max,
        Beat::Max,
        version,
        shapewire::wire::Paging::Default,
        None,
    )
}

/// Every frame buffered for this connection.
fn drain(f: &mut shapewire::hub::ConnFrames) -> Vec<Arc<ShapeFrame>> {
    let mut all = Vec::new();
    while let Ok(x) = f.try_recv() {
        all.push(x);
    }
    all
}

/// **PANICS IF NO EXPORT ARRIVED. There is no skip path, deliberately.**
///
/// A test whose precondition is a live subscription and whose body is guarded by
/// `if let Some(frame)` asserts nothing when the frame does not come, and reports
/// that as evidence. Rule 2.4's section records exactly that failure happening
/// twice in this repo.
fn expect_export(frames: &mut shapewire::hub::ConnFrames, why: &str) -> Arc<ShapeFrame> {
    let got = drain(frames);
    got.into_iter()
        .find(|f| f.wire.op == shapewire::wire::Op::Read)
        .unwrap_or_else(|| panic!("no export arrived, so nothing below was tested: {why}"))
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. THE THREE STATES
// ─────────────────────────────────────────────────────────────────────────────

/// **A MATCHING VERSION LINKS, AND SAYS NOTHING ABOUT IT.**
///
/// The positive control, and the one that makes every refusal below meaningful:
/// a test suite in which nothing links cannot tell "refuses the wrong version"
/// from "refuses everything".
///
/// It asserts the export's `epoch.superseded` is ABSENT rather than merely that
/// the subscribe returned `Ok`, because the second half of rule 2.6's first row
/// is that a current link is indistinguishable from the links this chassis has
/// always served — no new byte on the wire.
///
/// **THE MUTATION.** In `ShapeContract::link`, make the `wanted == self.version`
/// arm return `Link::Superseded(Superseded { wanted, current: self.version })`.
/// **THE RED:** `a current link must carry no superseded warning: Some(Superseded
/// { wanted: 4, current: 4 })` — the assertion fires on the VALUE that reached the
/// frame, so it names the nonsense the mutation produced rather than just failing.
#[tokio::test]
async fn a_matching_version_links_and_no_frame_mentions_a_version() {
    let hub = a_hub();
    let mut frames = hub.connect(1);
    let id = link(&hub, 1, Some(4)).expect("v4 is this producer's current version");
    settle().await;

    let export = expect_export(&mut frames, "a matching version must link and export");
    assert_eq!(
        export.wire.subscribes,
        vec![id],
        "the export answers the subscribe that asked for it"
    );
    assert_eq!(
        export.wire.epoch.superseded, None,
        "a current link must carry no superseded warning: {:?}",
        export.wire.epoch.superseded
    );
    // And the mirror a real consumer holds agrees.
    let mut m = Mirror::new("versioned");
    m.accept(&serde_json::to_value(&export.wire).unwrap());
    assert_eq!(m.superseded(), None);
    assert!(!m.terminal(), "a current link is healthy");
}

/// **A MEMBER OF `SERVES` LINKS — AND EVERY EXPORT SAYS SO.**
///
/// Rule 2.6's middle row: *"serves with warning: the export's `epoch` carries
/// `superseded: {wanted, current}`."* This is the test that distinguishes
/// ACCOMMODATION WITH A WARNING from a silent clamp, which is the whole reason
/// `SERVES` is permitted to exist at all: the link is served, and it announces
/// itself on every frame for its whole life.
///
/// The warning is asserted on the FIRST export and again after a RE-SUBSCRIBE, so
/// a mechanism that stamped the first frame and then forgot would be red.
///
/// **THE MUTATION.** In `ShapeTask::export`, drop `self.superseded_of(conn)` from
/// the grouping key and pass `self.info.epoch.clone()` instead of
/// `self.epoch_for(superseded)`. **THE RED:** `a superseded link must announce
/// itself on every export: None` on the first assertion — not on a later one,
/// which is what says the mutation was caught at the earliest frame rather than
/// by a downstream side effect.
#[tokio::test]
async fn a_superseded_version_links_and_every_export_names_both_versions() {
    let hub = a_hub();
    let mut frames = hub.connect(1);
    link(&hub, 1, Some(3)).expect("v3 is in SERVES, so the link is SERVED, not refused");
    settle().await;

    let export = expect_export(&mut frames, "a superseded version still links and exports");
    assert_eq!(
        export.wire.epoch.superseded,
        Some(Superseded { wanted: 3, current: 4 }),
        "a superseded link must announce itself on every export: {:?}",
        export.wire.epoch.superseded
    );
    // A SECOND export on the same link, so "stamped once and forgotten" is red.
    link(&hub, 1, Some(3)).expect("re-subscribing on the same old contract is still served");
    settle().await;
    let again = expect_export(&mut frames, "a re-subscribe on a superseded link exports too");
    assert_eq!(
        again.wire.epoch.superseded,
        Some(Superseded { wanted: 3, current: 4 }),
        "and on the NEXT one too, not only the first: {:?}",
        again.wire.epoch.superseded
    );

    // What a consumer does with it: it is a WARNING, not an end. Rule 2.6 says
    // this row SERVES, and a mirror that drew it as terminal would be refusing a
    // link the producer accepted.
    let mut m = Mirror::new("versioned");
    m.accept(&serde_json::to_value(&export.wire).unwrap());
    assert_eq!(m.superseded(), Some(Superseded { wanted: 3, current: 4 }));
    assert!(!m.terminal(), "serves with a warning is not a refusal");
    assert!(m.live_shape(), "and the rows are current");
}

/// **AN UNSERVED VERSION IS REFUSED AT LINK TIME, NAMING WHAT THE PRODUCER
/// SERVES — AND NOTHING IS QUEUED.**
///
/// This is the rule's whole content, and it asserts five separate things because
/// each is a different way the rule could be broken while the others held:
///
/// 1. the refusal is `ShapeError::VersionRefused` — the exact variant, so a rename
///    is red rather than silent;
/// 2. it carries the version WANTED and the versions SERVED, as values;
/// 3. its text is rule 2.6's own sentence, naming both;
/// 4. its wire CODE is `version_refused` and specifically NOT `malformed`, because
///    a mirror has to decide to go terminal off the code and cannot do that off
///    one that also means *"your `at` did not parse"*;
/// 5. **nothing was queued**: the connection receives no frame at all, and the
///    shape did not start tracking. A refusal that still armed a subscription
///    would be the ack-then-silence class this chassis is built against.
///
/// **THE MUTATION.** In `Hub::enqueue_subscribe`, delete the
/// `Link::Refused` arm — i.e. compute the link and ignore it. **THE RED:** `the
/// link must be refused, naming what this producer serves, got Ok(1)` — and then,
/// with the assertion on the variant removed to see how far the mutation got, the
/// connection DID receive an export and `tracked("versioned")` was true, which is
/// precisely the degraded-but-successful link assertion 5 exists to catch.
#[tokio::test]
async fn an_unserved_version_is_refused_at_link_naming_what_the_producer_serves() {
    let hub = a_hub();
    let mut frames = hub.connect(1);

    let got = link(&hub, 1, Some(2));
    // 1 and 2 — the variant and its numbers, as values.
    match &got {
        Err(ShapeError::VersionRefused {
            shape,
            wanted,
            serves,
        }) => {
            assert_eq!(shape, "versioned");
            assert_eq!(*wanted, 2);
            assert_eq!(
                *serves,
                vec![4, 3],
                "the refusal names EVERY version served, current first — not just \
                 that there was a mismatch"
            );
        }
        other => panic!(
            "the link must be refused, naming what this producer serves, got {other:?}"
        ),
    }
    // 3 — rule 2.6's own sentence.
    let text = got.as_ref().unwrap_err().to_string();
    assert!(
        text.contains("built against v2"),
        "the refusal names the version WANTED: {text}"
    );
    assert!(
        text.contains("[4, 3]"),
        "and the versions AVAILABLE, which is the actionable half: {text}"
    );
    // 4 — the code, and the one it must not be.
    let err = got.unwrap_err();
    assert_eq!(err.code(), codes::VERSION_REFUSED);
    assert_ne!(
        err.code(),
        codes::MALFORMED,
        "a wrong BUILD and a wrong REQUEST license different moves: the first is \
         terminal for the consumer chassis, the second is fixable in a param"
    );
    assert_eq!(Refusal::from(err).code, codes::VERSION_REFUSED);

    // 5 — NOTHING WAS QUEUED. Paired with a positive claim on the same hub below,
    // so this test cannot pass by never having a working subscribe at all.
    settle().await;
    assert!(
        drain(&mut frames).is_empty(),
        "a refused link must deliver no frame: not an export, not a death notice, \
         nothing"
    );
    assert!(
        !hub.tracked("versioned"),
        "and must not have started the shape tracking — a refusal that armed a \
         subscription is the ack-then-silence failure wearing an error"
    );
    // THE POSITIVE HALF, on the same hub and the same shape: a good link DOES all
    // of that. Without this, every assertion above would also pass on a hub that
    // refuses everything.
    let mut good = hub.connect(2);
    link(&hub, 2, Some(4)).expect("the current version still links on this hub");
    settle().await;
    expect_export(&mut good, "the control link must work, or the refusal proves nothing");
    assert!(hub.tracked("versioned"));
}

/// **THE REFUSAL IS NOT CLAMPED, NOT RETRIED AND NOT DOWNGRADED INTO A
/// SUCCESSFUL LINK WITH DEGRADED BEHAVIOUR.**
///
/// Rule 2.8's discipline, which 2.6 inherits: *"a clamped answer is a correction
/// the consumer did not ask for and cannot see."* The three ways a refusal could
/// be softened are each denied on a value:
///
/// * **not clamped to the nearest served version** — the refused connection gets
///   no export, and the hub's `shapes.list` still publishes `serves: [4, 3]`
///   unchanged, so nothing was negotiated down;
/// * **not downgraded to superseded** — a second connection asking for the same
///   unserved v2 is refused identically, rather than being served with a warning;
/// * **not retried into success** — asking twice produces the same refusal with
///   the same numbers, so there is no backoff path that eventually links.
///
/// **THE MUTATION.** In `Hub::enqueue_subscribe`, replace the `Link::Refused`
/// return with a clamp to the newest served version:
/// `let link = Link::Superseded(Superseded { wanted, current: slot.contract.version });`
/// **THE RED:** `an unserved version must be REFUSED, never served on a nearer
/// one: Ok(1)`, and then the superseded assertion fires with
/// `Some(Superseded { wanted: 2, current: 4 })` on a frame that should not exist —
/// which is the clamp made visible as the frame it produces, not as a missing
/// error.
#[tokio::test]
async fn a_refused_link_is_never_clamped_retried_or_downgraded() {
    let hub = a_hub();
    let mut frames = hub.connect(1);

    let first = link(&hub, 1, Some(2));
    assert!(
        matches!(first, Err(ShapeError::VersionRefused { wanted: 2, .. })),
        "an unserved version must be REFUSED, never served on a nearer one: {first:?}"
    );
    // ASK AGAIN. No backoff, no second chance, no eventual success.
    let second = link(&hub, 1, Some(2));
    assert_eq!(
        format!("{first:?}"),
        format!("{second:?}"),
        "asking twice answers the same refusal with the same numbers — there is no \
         retry path that eventually links"
    );
    settle().await;
    let got = drain(&mut frames);
    assert!(
        got.is_empty(),
        "and still no frame, so nothing was served on a nearer version: {:?}",
        got.iter().map(|f| f.wire.epoch.superseded).collect::<Vec<_>>()
    );

    // THE PUBLISHED CONTRACT IS UNMOVED. A producer that had quietly widened what
    // it serves to accommodate the ask would be the clamp one level up.
    let info = hub
        .list()
        .into_iter()
        .find(|i| i.name == "versioned")
        .expect("the shape is registered");
    assert_eq!(info.contract.version, 4);
    assert_eq!(
        info.contract.serves,
        vec![4, 3],
        "the contract a consumer reads is the contract the refusal named, and \
         neither moved because somebody asked"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. WHERE THE CHECK LIVES
// ─────────────────────────────────────────────────────────────────────────────

/// **AT LINK TIME, NOT PER FRAME — AND THE CHASSIS'S OWN REPAIR IS NOT A LINK.**
///
/// Two claims, both behavioural rather than read off source text:
///
/// 1. **The refusal arrives before any frame exists.** `subscribe_as` answers
///    `Err` synchronously, with no export, no live frame and nothing in the fold
///    path consulted — a per-frame check could not produce that ordering, because
///    there is no frame yet to carry it.
/// 2. **`resubscribe_lagged` states no version and cannot be refused.** Rule 2.5
///    makes re-subscribe the only repair for a lag, so a repair refusable on
///    version grounds would make recovery fail for a reason recovery cannot fix.
///    And it must not CLEAR an existing warning either, which is the
///    `SubscribeItem::retain` defect read onto this axis: the lag repair silently
///    improving the terms of the consumer that just fell behind.
///
/// **THE MUTATION.** In `ShapeTask::do_subscribe`, make the write unconditional
/// so the repair's `None` lands: add `self.superseded.remove(&s.conn);` above the
/// `if let Some(sup) = s.superseded`. **THE RED:** `the chassis's own lag repair
/// must not clear rule 2.6's warning: None`, left `None`, right
/// `Some(Superseded { wanted: 3, current: 4 })` — the warning gone on the one
/// consumer that had just fallen behind.
///
/// **What this assertion does NOT cover, recorded because it was measured.**
/// Changing that write from `entry(..).or_insert(..)` to `insert(..)` leaves the
/// whole suite GREEN — the `if let Some(..)` is the entire guarantee, so
/// `or_insert` was a rule nothing proved. It was REMOVED rather than left in with
/// a test invented to justify it; the behaviour that is witnessed is
/// `a_second_link_restating_a_different_served_version_replaces_the_warning`.
#[tokio::test]
async fn the_check_is_at_link_time_and_the_chassis_own_repair_states_no_version() {
    let hub = a_hub();
    let mut frames = hub.connect(1);
    link(&hub, 1, Some(3)).expect("a superseded link is served");
    settle().await;
    let first = expect_export(&mut frames, "the superseded link exports");
    assert_eq!(
        first.wire.epoch.superseded,
        Some(Superseded { wanted: 3, current: 4 })
    );

    // THE CHASSIS'S OWN REPAIR. It is refusable on nothing — it states no version
    // at all — and it leaves the warning exactly where it was.
    hub.resubscribe_lagged("versioned", Some("sam"), 1, Detail::Rows)
        .expect("rule 2.5's repair must never be refused on version grounds");
    settle().await;
    let repaired = expect_export(&mut frames, "the lag repair re-exports");
    assert_eq!(
        repaired.wire.epoch.superseded,
        Some(Superseded { wanted: 3, current: 4 }),
        "the chassis's own lag repair must not clear rule 2.6's warning: {:?}",
        repaired.wire.epoch.superseded
    );
}

/// **A SECOND LINK STATING A DIFFERENT SERVED VERSION REPLACES THE WARNING.**
///
/// The last stated link wins, exactly as the last `retain` does — a consumer that
/// says it is now a v2 build has made a new claim, and continuing to stamp v3 on
/// its frames would be the chassis telling it it is on a contract it just said it
/// is not on.
///
/// This test exists because the alternative rule (first-wins, via
/// `entry().or_insert()`) was in the code and NOTHING WITNESSED IT: the suite was
/// green either way, because the `if let Some(..)` is what protects the lag
/// repair. An unwitnessed rule in a delivery path is one the next reader
/// simplifies away without knowing what they changed, so the rule was replaced
/// with the consistent one and this is the test that holds it.
///
/// It needs a shape serving TWO old versions, because with one there is no second
/// old contract to re-link on.
///
/// **THE MUTATION.** `self.superseded.insert(s.conn, sup)` →
/// `self.superseded.entry(s.conn).or_insert(sup)`. **THE RED:** `a consumer that
/// re-states a different served version is on THAT contract now: Some(Superseded
/// { wanted: 3, current: 4 })`, left `Some(Superseded { wanted: 3, current: 4 })`,
/// right `Some(Superseded { wanted: 2, current: 4 })` — the stale claim, named.
#[tokio::test]
async fn a_second_link_restating_a_different_served_version_replaces_the_warning() {
    let hub = a_hub();
    let mut frames = hub.connect(1);
    hub.subscribe_as(
        "wide",
        Some("sam"),
        1,
        None,
        None,
        Detail::Rows,
        Retain::Max,
        Beat::Max,
        Some(3),
        shapewire::wire::Paging::Default,
        None,
    )
    .expect("v3 is served by `wide`");
    settle().await;
    let first = expect_export(&mut frames, "the v3 link exports");
    assert_eq!(
        first.wire.epoch.superseded,
        Some(Superseded { wanted: 3, current: 4 })
    );

    hub.subscribe_as(
        "wide",
        Some("sam"),
        1,
        None,
        None,
        Detail::Rows,
        Retain::Max,
        Beat::Max,
        Some(2),
        shapewire::wire::Paging::Default,
        None,
    )
    .expect("v2 is also served by `wide`");
    settle().await;
    let second = expect_export(&mut frames, "the re-stated link exports");
    assert_eq!(
        second.wire.epoch.superseded,
        Some(Superseded { wanted: 2, current: 4 }),
        "a consumer that re-states a different served version is on THAT contract \
         now: {:?}",
        second.wire.epoch.superseded
    );
}

/// **A KEYLESS UNSUBSCRIBE ENDS THE LINK, SO THE WARNING GOES WITH IT.**
///
/// Rule 2.6 is about a LINK. *"Stop sending me this shape"* ends one, and a LATER
/// subscribe from the same connection is a new link that states its own version —
/// so a warning kept across the gap would put a verdict about a finished link on a
/// new one's frames. The connection here re-links stating NOTHING, which is the
/// case that can only come out right if the entry was actually dropped.
///
/// **THE MUTATION.** In `ShapeTask`'s `Item::Unsubscribe` keyless arm, delete
/// `self.superseded.remove(&conn);`. **THE RED:** `a new link that states no
/// version must carry no verdict from the link before it: Some(Superseded {
/// wanted: 3, current: 4 })` — the stale claim riding the new link, named.
#[tokio::test]
async fn a_keyless_unsubscribe_ends_the_link_and_takes_the_warning_with_it() {
    let hub = a_hub();
    let mut frames = hub.connect(1);
    link(&hub, 1, Some(3)).expect("a superseded link is served");
    settle().await;
    assert_eq!(
        expect_export(&mut frames, "the superseded link exports")
            .wire
            .epoch
            .superseded,
        Some(Superseded { wanted: 3, current: 4 }),
        "the precondition: this connection really was warned"
    );

    hub.unsubscribe("versioned", None, 1);
    settle().await;
    let _ = drain(&mut frames);
    // A NEW LINK, STATING NOTHING.
    link(&hub, 1, None).expect("an un-versioned link is served");
    settle().await;
    let fresh = expect_export(&mut frames, "the new link exports");
    assert_eq!(
        fresh.wire.epoch.superseded, None,
        "a new link that states no version must carry no verdict from the link \
         before it: {:?}",
        fresh.wire.epoch.superseded
    );
}

/// **A CONSUMER THAT STATES NO VERSION IS SERVED, AND THE GAP IS NAMED.**
///
/// Rule 2.6 is written about *"a consumer [that] states the version it was built
/// against"*, and rule 2.1's `version` param does not exist on any consumer built
/// before it — including the board. Refusing those is the inversion
/// `ConsumerIdentity::parse` already ruled on for the identity half: *"a chassis
/// that refuses its only consumer to enforce its own bookkeeping has inverted what
/// it is for."*
///
/// **This test documents a HOLE as much as a behaviour.** An un-versioned consumer
/// cannot be refused, so rule 2.6 does not reach it. That is stated here, in the
/// suite, rather than discovered later: the state has a NAME (`Link::Unstated`) so
/// it is legible, and the test asserts the frames carry no warning — because
/// inventing one for a consumer that claimed nothing would be the producer making
/// a claim on the consumer's behalf.
///
/// **THE MUTATION.** In `ShapeContract::link`, make the `None` arm
/// `Link::Refused { wanted: 0, serves: self.serves.clone() }`. **THE RED:** `a
/// consumer that states no version must be served: Err(VersionRefused { shape:
/// "versioned", wanted: 0, serves: [4, 3] })` — and the whole rest of the suite
/// goes red with it, which is the evidence that this path is the one every
/// existing consumer is on.
#[tokio::test]
async fn a_consumer_that_states_no_version_is_served_and_warned_about_nothing() {
    let hub = a_hub();
    let mut frames = hub.connect(1);
    link(&hub, 1, None).expect("a consumer that states no version must be served");
    settle().await;
    let export = expect_export(&mut frames, "an un-versioned consumer still gets its export");
    assert_eq!(
        export.wire.epoch.superseded, None,
        "and is warned about nothing: it claimed no contract, so the producer \
         makes no claim on its behalf: {:?}",
        export.wire.epoch.superseded
    );

    // AND THE SHAPE THAT DECLARES NEITHER CONST behaves the same way, which is
    // what "nothing regresses" means. Asserted, not assumed.
    let und = hub
        .list()
        .into_iter()
        .find(|i| i.name == "undeclared")
        .expect("registered");
    assert_eq!(und.contract.version, 1, "the default VERSION");
    assert_eq!(
        und.contract.serves,
        vec![1],
        "and the default SERVES is EXACT-MATCH-ONLY — accommodation costs a \
         declaration, refusal is what you get for free"
    );
    assert_eq!(und.contract.link(Some(1)), Link::Current);
    assert_eq!(
        und.contract.link(Some(2)),
        Link::Refused {
            wanted: 2,
            serves: vec![1]
        },
        "so a shape whose author has not thought about versions refuses every \
         version but its own"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. THE LEDGER (rule 2.6: "the ledger records it")
// ─────────────────────────────────────────────────────────────────────────────

/// **THE SUPERSEDED LINK IS ON DISK, AND A CURRENT ONE IS NOT.**
///
/// Rule 2.6's middle row ends *"the ledger records it"*. Both halves matter: the
/// record is there when the link is superseded, and the line is UNCHANGED when it
/// is not — a `"superseded": null` on every line would be the field that says *I
/// did not look*, which rule 2.7 names as a collapse for `changed`.
///
/// `CAULDRON_HOME` is pointed at a temp dir, which is what that variable exists
/// for: *"without it every `cargo test` that spun a daemon would append to the
/// operator's real ledger."*
///
/// **THE MUTATION.** In `Hub::subscribe_as`, pass `None` for `superseded` to
/// `connected_link` — i.e. decide the link and forget to record it. **THE RED:**
/// `rule 2.6 says the ledger records a superseded link; no line carries it:
/// ["connect"]` — the assertion prints the events it DID find, so the red names
/// what the ledger has instead of what it lacks.
#[tokio::test]
async fn the_ledger_records_a_superseded_link_and_leaves_a_current_one_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = a_hub();
    hub.consumers.attach(
        tmp.path().join("chassis").join("t.producer.jsonl"),
        "t",
        "boot-1",
        "0.0.0",
    );
    let _f1 = hub.connect(1);
    let _f2 = hub.connect(2);
    link(&hub, 1, Some(3)).expect("superseded links are served");
    link(&hub, 2, Some(4)).expect("current links are served");
    settle().await;
    hub.consumers.flush();

    let text = std::fs::read_to_string(tmp.path().join("chassis").join("t.producer.jsonl"))
        .expect("the ledger file exists once something was recorded");
    let lines: Vec<Value> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("every line is one JSON record"))
        .collect();
    let events: Vec<&str> = lines
        .iter()
        .filter_map(|l| l.get("event").and_then(Value::as_str))
        .collect();

    let sup: Vec<&Value> = lines.iter().filter(|l| l.get("superseded").is_some()).collect();
    assert_eq!(
        sup.len(),
        1,
        "rule 2.6 says the ledger records a superseded link; no line carries it: \
         {events:?}"
    );
    assert_eq!(
        sup[0]["superseded"],
        json!({"wanted": 3, "current": 4}),
        "and it records BOTH versions, so a human reading the file knows what the \
         consumer should be rebuilt against"
    );
    assert_eq!(
        sup[0]["conn"], json!(1),
        "on the connection that actually asked for the old contract"
    );
    // THE OTHER HALF: the current link's line is untouched. Counted on the VALUE
    // (how many lines carry the key) rather than asserted as an absence, so a
    // mechanism that put the key on every line is red here and not merely
    // un-asserted.
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.get("conn") == Some(&json!(2)) && l.get("superseded").is_some())
            .count(),
        0,
        "a current link's record must be byte-identical to what this file has \
         always written"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. THE DECLARATION, CHECKED AT REGISTER
// ─────────────────────────────────────────────────────────────────────────────
//
// Each of these is a way the PUBLISHED `serves` list could disagree with itself,
// and a contract a consumer reads has to have one reading. They are panics in the
// producer's build function, which runs at boot — so a mis-declared contract is a
// daemon that does not start, never a consumer that links against something
// incoherent. The `PARENT` checks in the same function set that precedent.

macro_rules! bad_shape {
    ($name:ident, $wire:expr, $version:expr, $serves:expr) => {
        struct $name;
        impl Shape<Beat1> for $name {
            const NAME: &'static str = $wire;
            const TRACKING: Tracking = Tracking::Lazy;
            const CELLS: Cells = KEY_CELLS;
            const SHARED: Cells = Cells::NONE;
            const VERSION: u32 = $version;
            const SERVES: &'static [u32] = $serves;
            type Producer = ();
            type Root = Keys;
            fn root(&self) -> Keys {
                Keys::default()
            }
            fn hydrates(&mut self, _: &(), _: &mut Keys, _: Timestamp) {}
            fn folds(&mut self, _: &Stamped<Beat1>, _: &mut Tx<'_, Keys>) {}
        }
    };
}

bad_shape!(VersionZero, "zero", 0, &[]);
bad_shape!(ServesItsOwnVersion, "self", 4, &[4]);
bad_shape!(ServesAscending, "ascending", 4, &[2, 3]);
bad_shape!(ServesAboveVersion, "above", 4, &[5]);
bad_shape!(ServesDuplicate, "dup", 4, &[3, 3]);

/// Zero is reserved so that *"I did not think about this"* has no spelling on the
/// wire. **MUTATION:** delete the `S::VERSION > 0` assert.
#[tokio::test]
#[should_panic(expected = "declares VERSION 0")]
async fn version_zero_is_refused_at_register() {
    let hub = Arc::new(Hub::<Beat1>::new());
    hub.register(VersionZero, (), 8);
}

/// `SERVES` is *"the OLDER versions it still serves"*, so repeating the current
/// one would publish it twice. **MUTATION:** change `*v < floor` to `*v <= floor`.
#[tokio::test]
#[should_panic(expected = "strictly below VERSION")]
async fn serves_repeating_the_current_version_is_refused_at_register() {
    let hub = Arc::new(Hub::<Beat1>::new());
    hub.register(ServesItsOwnVersion, (), 8);
}

/// Current first, then descending — the order rule 2.6's own refusal text shows
/// (`[4, 3]`). **MUTATION:** `let mut floor = u32::MAX;` instead of `S::VERSION`,
/// which also lets the previous two through.
#[tokio::test]
#[should_panic(expected = "strictly descending")]
async fn serves_out_of_order_is_refused_at_register() {
    let hub = Arc::new(Hub::<Beat1>::new());
    hub.register(ServesAscending, (), 8);
}

/// A version NEWER than current is not an older version it still serves; it is a
/// contract nobody can read. **MUTATION:** as above.
#[tokio::test]
#[should_panic(expected = "strictly below VERSION")]
async fn serves_above_the_current_version_is_refused_at_register() {
    let hub = Arc::new(Hub::<Beat1>::new());
    hub.register(ServesAboveVersion, (), 8);
}

/// A duplicate would publish one version twice, so the list would not say how
/// many things it names. Caught by the SAME strictly-descending check rather than
/// a second one — one question, one number. **MUTATION:** `*v <= floor`.
#[tokio::test]
#[should_panic(expected = "strictly descending")]
async fn serves_with_a_duplicate_is_refused_at_register() {
    let hub = Arc::new(Hub::<Beat1>::new());
    hub.register(ServesDuplicate, (), 8);
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. THE CONSUMER'S END OF THE SAME REFUSAL
// ─────────────────────────────────────────────────────────────────────────────

/// **THE PRODUCER'S REFUSAL AND THE CONSUMER'S REPORT ARE THE SAME SENTENCE, AND
/// THE CONSUMER CAN READ THE ANSWER OFF `shapes.list` BEFORE IT ASKS.**
///
/// Rule 2.8's *"a consumer can read the contract it is allowed to use before it
/// uses it"*, applied to 2.6 — and the thing that keeps the two halves from
/// drifting, which is why `mirror` and `hub` call ONE function. Two
/// implementations of one three-way decision is the second source rule 2.4 exists
/// to delete.
///
/// **THE MUTATION.** Give `VersionMismatch::Display` its own hand-written
/// sentence (`"version mismatch on {shape}"`) instead of delegating to
/// `ShapeError::VersionRefused`. **THE RED:** `the consumer's own report and the
/// producer's refusal must be the same sentence: left "version mismatch on
/// versioned", right "shape versioned: built against v2; this producer serves
/// [4, 3]. …"` — the two strings side by side, which is the drift itself rather
/// than a proxy for it.
#[tokio::test]
async fn the_consumer_predicts_the_refusal_and_reports_it_in_the_producers_words() {
    let hub = a_hub();
    let info = hub
        .list()
        .into_iter()
        .find(|i| i.name == "versioned")
        .expect("registered");

    let mut m = Mirror::new("versioned");
    m.learn(&info);
    // THE CONSUMER KNOWS, OFF THE PUBLISHED CONTRACT, WITH NO LINK ATTEMPTED.
    assert_eq!(m.link(Some(4)), Link::Current);
    assert_eq!(
        m.link(Some(3)),
        Link::Superseded(Superseded { wanted: 3, current: 4 })
    );
    let predicted = m.link(Some(2));
    assert_eq!(
        predicted,
        Link::Refused {
            wanted: 2,
            serves: vec![4, 3]
        }
    );

    // AND WHEN IT ASKS ANYWAY, the producer says exactly what the consumer
    // predicted — the same numbers and the same sentence.
    let _f = hub.connect(1);
    let refusal = link(&hub, 1, Some(2)).expect_err("v2 is not served");
    let mism = VersionMismatch::from_link("versioned", &predicted)
        .expect("a refused link yields a mismatch a consumer can report");
    assert_eq!(
        mism.to_string(),
        refusal.to_string(),
        "the consumer's own report and the producer's refusal must be the same \
         sentence"
    );
    assert_eq!((mism.wanted, mism.serves.clone()), (2, vec![4, 3]));

    // THE TERMINAL STATE, which is rule 2.6's requirement on the consuming half:
    // *"the consumer chassis puts the shape in its terminal state with the refusal
    // as the reason, drawn the way a dead fold is drawn and never repainted as
    // healthy."*
    m.refused_version(&mism);
    assert!(m.terminal());
    assert!(!m.dead(), "a refused LINK is not a dead FOLD — different repairs");
    assert_eq!(m.version_mismatch().map(|v| v.wanted), Some(2));
    assert_eq!(
        m.version_mismatch().map(|v| v.serves.clone()),
        Some(vec![4, 3]),
        "and the consumer holds the served list, so it can report something \
         actionable rather than the word `mismatch`"
    );
}
