//! The mirror's behaviour, one test per rule it is holding up.
//!
//! **EVERY TEST HERE GOES IN THROUGH [`Mirror::accept`], NOT THROUGH
//! [`Mirror::apply`].** Two reasons, and both of them matter. `accept` is the
//! door a socket actually uses, so a behaviour proven here is a behaviour the
//! deployed path has. And `accept` is the only door that exercises the repair
//! pass, so a frame carrying a word this build does not know can be driven
//! through the same harness as one that parses.
//!
//! Frames are built with [`Frame`] rather than written out as a `json!`
//! literal per case. That is not only brevity: a literal per case is how one
//! test ends up asserting against a frame shape no producer emits, and the
//! builder has exactly one definition of what an export and a live frame look
//! like.

use serde_json::{json, Value};

use super::*;
use crate::wire::{reject, Link, ShapeContract, ShapeInfo, ShapeState};

// ─────────────────────────────────────────────────────────────────────────────
// The harness
// ─────────────────────────────────────────────────────────────────────────────

/// When the fold in these tests initialised, and when it last folded.
const INIT: &str = "2026-10-12T08:59:59Z";
const FOLD: &str = "2026-10-12T09:00:00Z";

fn accounts() -> Mirror {
    Mirror::new("accounts")
}

/// One frame under construction. `op`, `shape` and `epoch` are always present
/// because no real frame lacks them; everything else is added by name.
struct Frame(Value);

impl Frame {
    /// An `op: read` export, folded at `FOLD`, on a live epoch.
    fn read(shape: &str) -> Self {
        Self(json!({
            "shape": shape,
            "op": "read",
            "as_of": FOLD,
            "epoch": {"init_at": INIT, "reinit": 0, "lost": 0, "state": "known"},
        }))
    }

    /// An `op: live` frame stamped `at`, on a live epoch. A live frame carries
    /// no `init_at`, exactly as the producer emits it.
    fn live(shape: &str, at: &str) -> Self {
        Self(json!({
            "shape": shape,
            "op": "live",
            "at": at,
            "epoch": {"reinit": 0, "lost": 0, "state": "known"},
        }))
    }

    /// Any top-level field, by name. The escape hatch, and the thing every
    /// named setter below is built out of.
    fn set(mut self, field: &str, value: Value) -> Self {
        self.0[field] = value;
        self
    }

    fn key(self, key: &str) -> Self {
        self.set("key", json!(key))
    }

    fn row(self, row: Value) -> Self {
        self.set("row", row)
    }

    fn cells(self, cells: Value) -> Self {
        self.set("cells", cells)
    }

    fn shared(self, shared: Value) -> Self {
        self.set("shared", shared)
    }

    fn shared_cells(self, cells: Value) -> Self {
        self.set("shared_cells", cells)
    }

    fn event(self, event: Value) -> Self {
        self.set("event", event)
    }

    fn members(self, members: Value) -> Self {
        self.set("members", members)
    }

    fn epoch(self, epoch: Value) -> Self {
        self.set("epoch", epoch)
    }

    fn subscribes(self, ids: &[u64]) -> Self {
        self.set("subscribes", json!(ids))
    }

    fn promises(self, ids: &[u64]) -> Self {
        self.set("promises", json!(ids))
    }

    fn complete(self, complete: bool) -> Self {
        self.set("complete", json!(complete))
    }
}

/// Hand one frame to a mirror. A free function rather than a method on `Frame`
/// so that the mirror is on the left at every call site, where the reader looks
/// for it.
fn feed(m: &mut Mirror, frame: Frame) -> Receipt {
    m.accept(&frame.0)
}

/// A `known` cell block folded at `at`.
fn fresh(at: &str) -> Value {
    json!({"state": "known", "complete": true, "folded_at": at})
}

/// An ordinary keyed export of one `share` field with its provenance.
fn share(key: &str, value: f64) -> Frame {
    Frame::read("accounts")
        .key(key)
        .row(json!({"share": value}))
        .cells(json!({"share": fresh(FOLD)}))
}

/// A live transition appending `item` to `cell`.
fn appends(key: &str, at: &str, cell: &str, item: Value) -> Frame {
    Frame::live("accounts", at)
        .key(key)
        .event(json!({"kind": "transition", "cell": cell, "item": item}))
}

/// A `shapes.list` entry for `accounts`, with everything a mirror learns off
/// one. Built from the pieces each test wants to vary rather than from a
/// literal, so that a field added to `ShapeInfo` breaks one function here
/// instead of a dozen tests.
struct Listing {
    name: String,
    parent: Option<String>,
    max: ShapeMax,
    contract: ShapeContract,
    cells: Vec<CellInfo>,
    shared_cells: Vec<CellInfo>,
    refs: BTreeMap<String, String>,
    page: Option<PageContract>,
}

impl Listing {
    fn new() -> Self {
        Self {
            name: "accounts".into(),
            parent: Some("fleet".into()),
            max: ceilings(),
            // What a shape gets for free: version 1, serving only itself, so
            // exact-match-only.
            contract: ShapeContract {
                version: 1,
                serves: vec![1],
            },
            cells: vec![],
            shared_cells: vec![],
            refs: BTreeMap::new(),
            page: None,
        }
    }

    fn info(self) -> ShapeInfo {
        ShapeInfo {
            name: self.name,
            parent: self.parent,
            refs: self.refs,
            page: self.page,
            keys: vec![],
            tracking: "lazy".into(),
            tracked: true,
            subscribers: 0,
            epoch: Epoch::default(),
            fifo_high_water: 0,
            bursts: 0,
            counters: Default::default(),
            consumers: vec![],
            max: self.max,
            contract: self.contract,
            cells: self.cells,
            shared_cells: self.shared_cells,
        }
    }
}

/// The ceilings `accounts` publishes in these tests. `open_keys_per_conn` is
/// `None` because a shape that declares no key ceiling publishes none — which
/// is why adding that field moved no golden bytes in `wire.rs`.
fn ceilings() -> ShapeMax {
    ShapeMax {
        rows_per_subscribe: 500,
        keys_per_frame: 500,
        future_at_secs: 86_400,
        armed_timers_per_conn: 4,
        retained_items: 0,
        beat_interval_ms: 0,
        open_keys_per_conn: None,
    }
}

/// A cell the producer declares, with the fields it says the cell carries.
fn declares(name: &str, fields: &[&str]) -> CellInfo {
    use crate::wire::FieldInfo;
    CellInfo {
        name: name.to_string(),
        dtype: if fields.is_empty() {
            "scalar:Int".into()
        } else {
            "struct:X".into()
        },
        required: true,
        words: vec![],
        fields: fields
            .iter()
            .map(|f| FieldInfo {
                name: f.to_string(),
                dtype: "scalar:Bool".into(),
                required: true,
                words: vec![],
            })
            .collect(),
    }
}

fn stamp(s: &str) -> Option<Timestamp> {
    parse_ts(s)
}

// ─────────────────────────────────────────────────────────────────────────────
// Hold the export, apply everything after it
// ─────────────────────────────────────────────────────────────────────────────

/// **AN EXPORT REPLACES. IT DOES NOT MERGE.** A field the producer has stopped
/// stating is a field the consumer has to stop showing, and a merged row would
/// keep it on the screen for ever.
#[test]
fn an_export_replaces_the_row_wholesale_and_never_merges_it() {
    let mut m = accounts();
    feed(
        &mut m,
        Frame::read("accounts")
            .key("sam")
            .row(json!({"share": 0.25, "disabled": false}))
            .cells(json!({"share": fresh(FOLD)})),
    );
    assert_eq!(m.row("sam"), &json!({"share": 0.25, "disabled": false}));
    assert!(m.cell("sam", "share").is_known());

    // A second export carries fewer fields.
    feed(&mut m, share("sam", 0.5));
    assert_eq!(
        m.row("sam"),
        &json!({"share": 0.5}),
        "a merged row would still be showing the `disabled` the producer dropped"
    );
    assert_eq!(m.exports(), 2);
    assert!(m.exported() && m.live_shape());
}

/// A cell no export carried is unhydrated WITH ITS REASON NAMED — never a
/// guess, and never `Known`.
#[test]
fn a_cell_nobody_exported_names_the_reason_rather_than_guessing() {
    let mut m = accounts();
    feed(&mut m, share("sam", 0.25));
    match m.cell("sam", "gauge") {
        CellState::NotHydrated { reason } => assert_eq!(reason.as_deref(), Some(UNSEEN)),
        other => panic!("a cell nobody populated is not {other:?}"),
    }
    // And a key nothing was ever exported for answers the same way, rather
    // than panicking or inventing an empty row.
    assert!(matches!(
        m.cell("nobody", "share"),
        CellState::NotHydrated { .. }
    ));
    assert_eq!(m.row("nobody"), &Value::Null);
}

/// **THE THREE-VALUED UNKNOWN, ALL THREE KEPT APART.** An empty `Known`, a
/// `NotHydrated` and a `Dead` are three statements; collapsing any two of them
/// is the lie the vocabulary exists to stop.
#[test]
fn empty_known_unhydrated_and_dead_remain_three_answers() {
    let mut m = accounts();
    feed(
        &mut m,
        Frame::read("accounts")
            .key("sam")
            .row(json!({"threads": []}))
            .cells(json!({
                "threads": fresh(FOLD),
                "credential": {"state": "not_hydrated", "reason": "no login dir"},
                "gauge": {"state": "dead", "reason": "the gauge fold was removed"},
            })),
    );
    // Zero rows is a FACT. An empty `Known` is known.
    assert!(m.cell("sam", "threads").is_known());
    assert_eq!(m.row("sam").get("threads"), Some(&json!([])));
    assert!(matches!(
        m.cell("sam", "credential"),
        CellState::NotHydrated { .. }
    ));
    assert!(matches!(m.cell("sam", "gauge"), CellState::Dead { .. }));
    // And the fourth answer, which is not a cell state at all: nobody has said
    // anything about this name.
    assert!(matches!(
        m.cell("sam", "nothing_said"),
        CellState::NotHydrated { .. }
    ));
}

/// Every field a `known` block may omit has a defined answer, and `folded_at`
/// falls back to the epoch rather than to *now* — because this module reads no
/// clock and a fabricated `now` would be the staleness lie in reverse.
#[test]
fn a_known_cell_block_has_an_answer_for_every_field_it_omits() {
    let mut m = accounts();
    feed(
        &mut m,
        Frame::read("accounts")
            .key("sam")
            .row(json!({"share": 0.25, "window": 1}))
            .cells(json!({
                "share": {"state": "known"},
                "window": {"state": "known", "complete": false, "folded_at": FOLD,
                           "since_init": true, "covers_since": INIT},
            })),
    );
    match m.cell("sam", "share") {
        CellState::Known {
            complete,
            folded_at,
            since_init,
            covers_since,
        } => {
            assert!(complete, "a block that says nothing about completeness is whole");
            assert_eq!(folded_at, Timestamp::UNIX_EPOCH, "and it does not invent a stamp");
            assert!(!since_init);
            assert_eq!(covers_since, None);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        m.cell("sam", "window"),
        CellState::Known {
            complete: false,
            folded_at: stamp(FOLD).unwrap(),
            since_init: true,
            covers_since: stamp(INIT),
        },
        "and every field it DOES state is read"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// The derived events
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_live_row_overwrites_the_row_and_leaves_the_provenance_where_it_was() {
    let mut m = accounts();
    feed(&mut m, share("sam", 0.25));
    let r = feed(
        &mut m,
        Frame::live("accounts", "2026-10-12T09:01:00Z")
            .key("sam")
            .event(json!({"kind": "row", "row": {"share": 0.75}})),
    );
    assert!(r.applied() && r.moved);
    assert_eq!(m.row("sam"), &json!({"share": 0.75}));
    assert!(
        m.cell("sam", "share").is_known(),
        "a live row carries no `cells`, so the export's provenance stands"
    );
    let held = m.held("sam").unwrap();
    assert_eq!(
        held.at(),
        stamp("2026-10-12T09:01:00Z"),
        "the live frame's `at` is the newest touch"
    );
    assert_eq!(
        held.as_of(),
        stamp(FOLD),
        "`as_of` is the export's fold point, and a live frame does not move it"
    );
}

/// A `row` for a key this connection was never exported: **the row is real and
/// its provenance is not.** It is held with no cells at all, so every cell
/// under it reads unhydrated.
#[test]
fn a_row_for_a_key_never_exported_is_held_with_no_provenance_at_all() {
    let mut m = accounts();
    let r = feed(
        &mut m,
        Frame::live("accounts", "2026-10-12T09:01:00Z")
            .key("stranger")
            .event(json!({"kind": "row", "row": {"share": 0.1}})),
    );
    assert!(r.applied(), "the row is real");
    assert_eq!(m.row("stranger"), &json!({"share": 0.1}));
    assert_eq!(m.key_state("stranger"), KeyState::Held);
    assert!(
        matches!(m.cell("stranger", "share"), CellState::NotHydrated { .. }),
        "inventing `Known` here is exactly the lie the three-valued state prevents"
    );
    assert_eq!(
        m.exports(),
        0,
        "and it is not an export: nothing exported this key"
    );
    assert!(!m.exported());
}

/// A `row` event whose `row` is not an object states nothing a row can hold,
/// and is refused rather than folded as an empty one.
#[test]
fn a_row_event_that_is_not_an_object_is_refused() {
    let mut m = accounts();
    feed(&mut m, share("sam", 0.25));
    let r = feed(
        &mut m,
        Frame::live("accounts", "2026-10-12T09:01:00Z")
            .key("sam")
            .event(json!({"kind": "row", "row": ["not", "a", "row"]})),
    );
    assert_eq!(r.disposition(), Disposition::NotApplied);
    assert_eq!(
        m.row("sam"),
        &json!({"share": 0.25}),
        "and the row the export stated is untouched"
    );
}

/// `gone` unlists the key AND records that the producer unsaid it, which are
/// two different things and the second is the one a reader needs.
#[test]
fn a_gone_unlists_the_key_and_records_that_it_was_unsaid() {
    let mut m = accounts();
    feed(&mut m, share("sam", 0.25));
    assert!(m.members().contains("sam"));

    let r = feed(
        &mut m,
        Frame::live("accounts", "2026-10-12T09:02:00Z")
            .key("sam")
            .event(json!({"kind": "gone"})),
    );
    assert!(r.moved);
    assert!(m.held("sam").is_none());
    assert!(!m.members().contains("sam"));
    assert_eq!(m.key_state("sam"), KeyState::Gone);
    assert_eq!(m.vanished(), vec!["sam".to_string()]);

    // A `gone` for a key this connection never held is APPLIED — it is not a
    // disagreement with the producer — and it MOVES, because a reader that had
    // `Unknown` for this key now has `Gone`, which is the transition somebody
    // waiting for a row they will never get needs to see.
    let r = feed(
        &mut m,
        Frame::live("accounts", "2026-10-12T09:03:00Z")
            .key("ghost")
            .event(json!({"kind": "gone"})),
    );
    assert!(r.applied(), "{:?}", r.rejected);
    assert!(r.moved, "Unknown -> Gone is a fact the reader did not have");
    assert_eq!(m.key_state("ghost"), KeyState::Gone);

    // Saying it a second time moves nothing: the reader already knew.
    let again = feed(
        &mut m,
        Frame::live("accounts", "2026-10-12T09:04:00Z")
            .key("ghost")
            .event(json!({"kind": "gone"})),
    );
    assert!(!again.moved);
}

/// **A TRANSITION NEVER WRITES THE ROW.** substrate's `shapes.py` appends to
/// `row[cell]`; for `threads` the cell is `"status"` while the row spells the
/// level as `status` and the log as `status_transitions`, so that rule replaces
/// a held level OBJECT with a one-item list on the first transition frame.
#[test]
fn a_transition_never_writes_the_row_field_of_the_same_name() {
    let mut m = Mirror::new("threads");
    feed(
        &mut m,
        Frame::read("threads").key("t1").cells(json!({})).row(json!({
            "status": {"level": "running"},
            "status_transitions": [{"to": "running"}],
        })),
    );
    feed(
        &mut m,
        Frame::live("threads", "2026-10-12T09:01:00Z")
            .key("t1")
            .event(json!({"kind": "transition", "cell": "status", "item": {"to": "idle"}})),
    );
    assert_eq!(
        m.row("t1").get("status"),
        Some(&json!({"level": "running"})),
        "the LEVEL survived; the Python rule would have replaced it with a list"
    );
    let held = m.held("t1").unwrap();
    assert_eq!(held.transitions("status").len(), 1, "and the item was kept");
    assert_eq!(held.transitioned(), vec!["status".to_string()]);
    assert_eq!(
        held.items("status"),
        vec![json!({"to": "idle", "at": "2026-10-12T09:01:00+00:00"})],
        "`items` merges only where the row field is absent or already a list, so \
         the level is not swallowed into the merged view either"
    );
}

/// **THE STAMP IS ON THE FRAME, NOT ON THE ITEM — AND IT IS SPELLED THE WAY
/// THE EXPORT SPELLS IT.** An appended item has to be indistinguishable from an
/// exported one, and substrate's `fold::Sequence::to_json` writes a retained
/// item's stamp with `to_rfc3339()`: `+00:00`, not `Z`.
#[test]
fn an_appended_item_is_stamped_from_its_frame_in_the_exports_own_spelling() {
    let mut m = accounts();
    feed(
        &mut m,
        Frame::read("accounts")
            .key("sam")
            .row(json!({"moves": [{"from": "idle", "to": "busy", "at": "2026-10-12T08:00:00Z"}]}))
            .cells(json!({"moves": fresh(FOLD)})),
    );
    feed(
        &mut m,
        appends(
            "sam",
            "2026-10-12T09:01:00Z",
            "moves",
            json!({"from": "busy", "to": "idle"}),
        ),
    );
    let items = m.held("sam").unwrap().items("moves");
    assert_eq!(items.len(), 2, "the export's item, then the appended one");
    assert_eq!(
        items[1]["at"],
        json!("2026-10-12T09:01:00+00:00"),
        "`…Z` here would make an appended item tellable from an exported one"
    );
    assert_eq!(
        m.row("sam").get("moves").and_then(Value::as_array).map(Vec::len),
        Some(1),
        "and the ROW still says exactly what the export said"
    );
}

/// An item that arrived already stamped keeps its own: the graft fills a hole,
/// it does not overwrite a fact.
#[test]
fn an_item_that_arrived_stamped_keeps_its_own_stamp() {
    let mut m = accounts();
    feed(&mut m, Frame::read("accounts").key("sam").row(json!({})).cells(json!({})));
    feed(
        &mut m,
        appends(
            "sam",
            "2026-10-12T09:01:00Z",
            "moves",
            json!({"to": "idle", "at": "2026-10-12T06:00:00Z"}),
        ),
    );
    assert_eq!(
        m.held("sam").unwrap().transitions("moves")[0]["at"],
        json!("2026-10-12T06:00:00Z")
    );
}

/// A transition for a key with no export under it is the one place substrate's
/// Python mirror silently drops an item. Here it is a COUNTABLE rejection, on
/// the ack rather than spent at the door as a `False`.
#[test]
fn a_transition_with_no_export_under_it_is_counted_not_dropped() {
    let mut m = accounts();
    let r = feed(
        &mut m,
        appends("stranger", "2026-10-12T09:01:00Z", "moves", json!({"to": "idle"})),
    );
    assert_eq!(r.disposition(), Disposition::NotApplied);
    assert_eq!(r.rejected[0].reason, reject::NO_EXPORT);
    assert_eq!(
        r.rejected[0].detail.as_deref(),
        Some("moves"),
        "and it names the cell whose item was lost"
    );
    assert_eq!(m.rejections().len(), 1);
    // A keyless transition has no key to append to either, and says so.
    let r = feed(
        &mut m,
        Frame::live("accounts", "2026-10-12T09:02:00Z")
            .event(json!({"kind": "transition", "cell": "moves", "item": {}})),
    );
    assert_eq!(r.rejected[0].reason, reject::NO_EXPORT);
}

#[test]
fn a_live_shared_moves_the_value_and_not_its_provenance() {
    let mut m = accounts();
    feed(
        &mut m,
        Frame::read("accounts")
            .key("sam")
            .row(json!({}))
            .cells(json!({}))
            .shared(json!({"warn_below_days": 3}))
            .shared_cells(json!({"warn_below_days": fresh(FOLD)})),
    );
    assert!(m.shared_cell("warn_below_days").is_known());

    let r = feed(
        &mut m,
        Frame::live("accounts", "2026-10-12T09:01:00Z")
            .event(json!({"kind": "shared", "shared": {"warn_below_days": 5}})),
    );
    assert!(r.moved);
    assert_eq!(m.shared(), &json!({"warn_below_days": 5}));
    assert!(
        m.shared_cell("warn_below_days").is_known(),
        "only an export refreshes a shape-level scalar's provenance"
    );
    // The same value again moves nothing a reader can see.
    let same = feed(
        &mut m,
        Frame::live("accounts", "2026-10-12T09:02:00Z")
            .event(json!({"kind": "shared", "shared": {"warn_below_days": 5}})),
    );
    assert!(!same.moved);
}

// ─────────────────────────────────────────────────────────────────────────────
// Membership
// ─────────────────────────────────────────────────────────────────────────────

/// **A MEMBERSHIP WALK UNIONS ITS PAGES.** The producer emits a walk as N
/// frames in one burst and only the last can claim `complete`; a page that
/// replaced would leave the consumer holding page N and nothing else.
#[test]
fn membership_pages_union_and_only_the_last_one_closes_the_walk() {
    let mut m = Mirror::new("threads");
    let page = |keys: Value, complete: bool, next: Option<&str>| {
        let mut members = json!({"state": "known", "keys": keys, "complete": complete});
        if let Some(n) = next {
            members["next"] = json!(n);
        }
        let f = Frame::read("threads").members(members).subscribes(&[9]);
        if complete {
            f.complete(true)
        } else {
            f
        }
    };
    feed(&mut m, page(json!(["a", "b"]), false, Some("c")));
    assert_eq!(m.members().keys(), vec!["a", "b"]);
    assert!(!m.members().complete());
    assert_eq!(
        m.members().resume_from(),
        Some("c"),
        "an unfinished walk says where the hole starts"
    );
    assert_eq!(m.subscribe(9), SubscribeStatus::Open);

    // The last page claims `complete` AND still carries a `next`, which is what
    // a producer that pages by cursor emits. A finished walk has no hole, so
    // the cursor is DISCARDED rather than left for a reader to act on.
    feed(&mut m, page(json!(["c"]), true, Some("d")));
    assert_eq!(
        m.members().keys(),
        vec!["a", "b", "c"],
        "page two UNIONS; replacing would have lost a and b"
    );
    assert!(m.members().complete());
    assert_eq!(
        m.members().resume_from(),
        None,
        "a finished walk has no hole, whatever cursor rode along with it"
    );
    assert_eq!(m.subscribe(9), SubscribeStatus::Complete);

    // A membership page exports no ROW, so it is not a key export.
    assert_eq!(m.exports(), 0);
    assert!(m.keys().is_empty());
    assert!(!m.exported());
}

/// An empty pool is a COMPLETE answer of zero. It is not a missing frame, and
/// it is not the same statement as *I cannot enumerate this*.
#[test]
fn an_empty_pool_is_not_the_same_answer_as_an_unenumerable_one() {
    let mut empty = Mirror::new("threads");
    assert_eq!(
        empty.members().enumeration(),
        Enumeration::NotAsked,
        "and before any frame, neither of those two has been said"
    );
    feed(
        &mut empty,
        Frame::read("threads").members(json!({"state": "known", "keys": [], "complete": true})),
    );
    assert_eq!(
        empty.members().enumeration(),
        Enumeration::Known { complete: true }
    );
    assert!(empty.members().keys().is_empty());
    assert_eq!(empty.members().reason(), None);

    let mut opaque = Mirror::new("threads");
    feed(
        &mut opaque,
        Frame::read("threads")
            .members(json!({"state": "not_hydrated", "reason": "opaque remote namespace"})),
    );
    assert_eq!(opaque.members().enumeration(), Enumeration::NotHydrated);
    assert_eq!(opaque.members().reason(), Some("opaque remote namespace"));
    assert!(!opaque.members().complete());
}

/// **"I CANNOT ENUMERATE" DOES NOT UNSAY AN `added` ALREADY SENT.** It says the
/// WALK is unavailable, which is a claim about the walk and not about the keys.
#[test]
fn i_cannot_enumerate_leaves_the_keys_it_was_already_told_about() {
    let mut m = Mirror::new("threads");
    feed(
        &mut m,
        Frame::live("threads", "2026-10-12T09:01:00Z")
            .key("t9")
            .event(json!({"kind": "added"})),
    );
    feed(
        &mut m,
        Frame::read("threads").members(json!({"state": "not_hydrated", "reason": "sharded"})),
    );
    assert_eq!(m.members().enumeration(), Enumeration::NotHydrated);
    assert!(
        m.members().contains("t9"),
        "the producer said this key exists and has not unsaid it"
    );
    assert_eq!(m.members().resume_from(), None);
}

/// An `added` states EXISTENCE and carries no row. Into a roster nobody has
/// enumerated, it claims nothing about completeness.
#[test]
fn an_added_states_existence_and_invents_no_row() {
    let mut m = Mirror::new("threads");
    let arrival = || {
        Frame::live("threads", "2026-10-12T09:01:00Z")
            .key("t9")
            .event(json!({"kind": "added"}))
    };
    let r = feed(&mut m, arrival());
    assert!(r.applied() && r.moved);
    assert!(m.members().contains("t9"));
    assert!(
        m.held("t9").is_none(),
        "a member is a key the producer says is there, not a row"
    );
    assert_eq!(
        m.key_state("t9"),
        KeyState::Unknown,
        "and nothing has been said about whether a row for it exists"
    );
    assert_eq!(
        m.members().enumeration(),
        Enumeration::Known { complete: false },
        "\"this key exists\" is not \"and these are all of them\""
    );
    assert!(!feed(&mut m, arrival()).moved, "the same `added` twice moves nothing");
}

/// Rule 2.8's fourth clause: where the ceiling is smaller than the whole
/// answer, the frame SAYS SO structurally. A short answer is not a failure, and
/// a frame that is not the last of a subscribe never downgrades one that has
/// already settled.
#[test]
fn a_short_answer_is_structural_and_never_downgraded_afterwards() {
    let mut m = accounts();
    assert_eq!(m.subscribe(4), SubscribeStatus::Unknown);
    let r = feed(&mut m, share("sam", 0.25).subscribes(&[4]).complete(false));
    assert_eq!(r.complete, Some(false));
    assert_eq!(r.subscribes, vec![4]);
    assert_eq!(m.subscribe(4), SubscribeStatus::Short);
    assert!(r.applied(), "short is stated, not refused");

    // A later frame naming the same id with no `complete` must not reopen it.
    feed(&mut m, share("kim", 0.75).subscribes(&[4]));
    assert_eq!(
        m.subscribe(4),
        SubscribeStatus::Short,
        "a settled subscribe is not downgraded by a frame that claims nothing"
    );
    assert_eq!(m.subscribe(999), SubscribeStatus::Unknown);
}

/// `next` on the last frame of a short keyless answer is the consumer's `from`
/// for the next ask, and it is passed through untouched.
#[test]
fn the_next_key_of_a_short_keyless_answer_reaches_the_receipt() {
    let mut m = accounts();
    let r = feed(
        &mut m,
        share("sam", 0.25)
            .subscribes(&[11])
            .complete(false)
            .set("next", json!("tom")),
    );
    assert_eq!(r.next.as_deref(), Some("tom"));
    assert_eq!(m.subscribe(11), SubscribeStatus::Short);
}

// ─────────────────────────────────────────────────────────────────────────────
// Death, and the other terminal
// ─────────────────────────────────────────────────────────────────────────────

/// A keyless export on a dead epoch. The supervisor emits exactly one of these
/// per death, to every connection holding any key of the shape.
fn death(reason: Option<&str>) -> Frame {
    let mut epoch = json!({"init_at": INIT, "reinit": 0, "lost": 0, "state": "dead"});
    if let Some(reason) = reason {
        epoch["dead_reason"] = json!(reason);
    }
    Frame::read("accounts")
        .set("as_of", json!("2026-10-12T09:10:00Z"))
        .epoch(epoch)
}

/// **THE KEYLESS EXPORT IS THE DEATH NOTICE, AND THE ROWS ARE KEPT.** A board
/// that empties itself on a dead fold looks exactly like a board whose keys all
/// went away, and an operator cannot tell those apart.
#[test]
fn the_keyless_death_export_marks_every_cell_dead_and_keeps_every_row() {
    let mut m = accounts();
    feed(
        &mut m,
        share("sam", 0.25)
            .shared(json!({"warn_below_days": 3}))
            .shared_cells(json!({"warn_below_days": fresh(FOLD)})),
    );
    feed(&mut m, death(Some("the shape task panicked")));

    assert!(m.dead() && m.terminal() && !m.live_shape());
    assert_eq!(m.row("sam"), &json!({"share": 0.25}), "the rows are KEPT");
    match m.cell("sam", "share") {
        CellState::Dead { reason } => assert_eq!(reason, "the shape task panicked"),
        other => panic!("a dead fold leaves nothing current: {other:?}"),
    }
    assert!(
        matches!(m.cell("sam", "gauge"), CellState::Dead { .. }),
        "including a cell no export carried — a dead fold does not leave some \
         fields trustworthy"
    );
    assert_eq!(m.held("sam").unwrap().dead_reason(), Some("the shape task panicked"));
    assert!(matches!(
        m.shared_cell("warn_below_days"),
        CellState::Dead { .. }
    ));
    assert_eq!(
        m.last_frame_at(),
        stamp("2026-10-12T09:10:00Z"),
        "the age of the board is a FACT ABOUT THE FOLD, not a poll clock"
    );
}

/// A death notice that names no reason still says something a human can act
/// on.
#[test]
fn a_death_that_names_no_reason_still_names_one() {
    let mut m = accounts();
    feed(&mut m, share("sam", 0.25));
    feed(&mut m, death(None));
    assert_eq!(m.dead_reason(), Some(DIED));
    match m.cell("sam", "share") {
        CellState::Dead { reason } => assert_eq!(reason, DIED),
        other => panic!("{other:?}"),
    }
}

/// **A REFUSED LINK IS TERMINAL TOO, AND IT CANNOT ARRIVE AS A FRAME.** Rule
/// 2.6's refusal reaches a consumer as an `envelope::Outcome`, so a mirror with
/// no door for it would sit healthy for ever drawing a fold it can no longer
/// see.
#[test]
fn a_refused_link_is_terminal_without_being_a_death() {
    let mut m = accounts();
    feed(
        &mut m,
        share("sam", 0.25).shared_cells(json!({"warn_below_days": fresh(FOLD)})),
    );
    assert!(m.live_shape() && !m.terminal());

    m.refused("this build states rev4 and the producer no longer serves it");
    assert!(m.terminal() && !m.live_shape());
    assert_eq!(
        m.refusal(),
        Some("this build states rev4 and the producer no longer serves it")
    );
    assert!(
        !m.dead(),
        "a refusal is not a death: the producer's fold is fine, the LINK is not"
    );
    assert_eq!(
        m.row("sam"),
        &json!({"share": 0.25}),
        "the rows are kept for exactly the reason death keeps them"
    );
    assert!(matches!(m.cell("sam", "share"), CellState::Dead { .. }));
    assert!(matches!(
        m.shared_cell("warn_below_days"),
        CellState::Dead { .. }
    ));

    // Only a reconnect clears it.
    m.reset();
    assert!(!m.terminal() && m.refusal().is_none());
}

/// **A VERSION REFUSAL REPORTS NUMBERS, NOT THE WORD `mismatch`** — and it
/// reports them through the same sentence the PRODUCER's own refusal uses, so
/// the two cannot drift into describing one served list two ways.
#[test]
fn a_version_refusal_carries_what_the_producer_serves() {
    let mut m = accounts();
    let mut listing = Listing::new();
    listing.contract = ShapeContract {
        version: 4,
        serves: vec![4, 3],
    };
    m.learn(&listing.info());

    // Read the contract BEFORE linking — rule 2.8's discipline applied to rule
    // 2.6 — and the answer is the refusal, in advance.
    assert_eq!(m.link(None), Link::Unstated, "stating no version is served");
    assert_eq!(m.link(Some(4)), Link::Current);
    assert!(matches!(m.link(Some(3)), Link::Superseded(_)));
    let refusal = m.link(Some(2));
    assert_eq!(
        refusal,
        Link::Refused {
            wanted: 2,
            serves: vec![4, 3]
        }
    );

    let mismatch = VersionMismatch::from_link("accounts", &refusal)
        .expect("v2 is not served, so this link is refused");
    assert_eq!(mismatch.wanted, 2);
    assert_eq!(mismatch.serves, vec![4, 3]);
    assert!(
        VersionMismatch::from_link("accounts", &Link::Current).is_none(),
        "a link that is NOT refused yields no mismatch"
    );

    let rendered = mismatch.to_string();
    assert_eq!(
        rendered,
        crate::wire::ShapeError::VersionRefused {
            shape: "accounts".into(),
            wanted: 2,
            serves: vec![4, 3],
        }
        .to_string(),
        "the consumer's report and the producer's refusal are one sentence"
    );
    assert!(rendered.contains('4') && rendered.contains('3'), "{rendered}");

    m.refused_version(&mismatch);
    assert!(m.terminal());
    assert_eq!(m.version_mismatch().map(|v| v.wanted), Some(2));
    assert_eq!(m.refusal(), Some(rendered.as_str()));
    assert!(!m.dead(), "still not a death");
}

/// A keyless export that is NOT dead is a shape-level read: `shared` and
/// nothing else.
#[test]
fn a_keyless_live_export_takes_shared_and_touches_no_key() {
    let mut m = accounts();
    feed(&mut m, share("sam", 0.25));
    let r = feed(
        &mut m,
        Frame::read("accounts")
            .set("as_of", json!("2026-10-12T09:10:00Z"))
            .shared(json!({"warn_below_days": 9})),
    );
    assert!(r.applied() && r.moved);
    assert_eq!(m.shared(), &json!({"warn_below_days": 9}));
    assert_eq!(m.row("sam"), &json!({"share": 0.25}), "the keys are untouched");
    assert!(m.cell("sam", "share").is_known());
    assert!(!m.dead());
    assert_eq!(m.exports(), 1, "a shape-level read exports no row");
}

// ─────────────────────────────────────────────────────────────────────────────
// The epoch, nesting, and the reconnect
// ─────────────────────────────────────────────────────────────────────────────

/// `init_at` is KEPT when a frame omits it; `reinit` and `lost` overwrite. And
/// the epoch lands before `op` is looked at, which is what makes the death
/// notice legible at all.
#[test]
fn the_epoch_lands_before_the_body_and_init_at_survives_a_live_frame() {
    let mut m = accounts();
    feed(&mut m, share("sam", 0.25));
    assert_eq!(m.epoch().init_at, stamp(INIT));

    feed(
        &mut m,
        Frame::live("accounts", "2026-10-12T09:01:00Z")
            .key("sam")
            .epoch(json!({"reinit": 3, "lost": 41, "state": "known"}))
            .event(json!({"kind": "row", "row": {}})),
    );
    assert_eq!(
        m.epoch().init_at,
        stamp(INIT),
        "a live frame carries no init stamp and must not erase the export's"
    );
    assert_eq!(m.epoch().reinit, 3);
    assert_eq!(m.epoch().lost, 41);
    assert_eq!(m.state(), &ShapeState::Known);
}

/// **`parent_epoch` IS WHY NESTED EPOCHS CAN BE INDEPENDENT.** A parent
/// re-init does not invalidate the child; it invalidates what the consumer
/// inferred FROM the parent ABOUT the child.
#[test]
fn a_child_sees_when_its_parents_era_moved_on() {
    let mut parent = Mirror::new("threads");
    let parent_at = |reinit: u64, lost: u64, summary: &str| {
        Frame::read("threads")
            .key("t1")
            .epoch(json!({"reinit": reinit, "lost": lost, "state": "known"}))
            .row(json!({"summary": summary}))
            .cells(json!({}))
    };
    feed(&mut parent, parent_at(4, 0, "two children"));

    let mut child = Mirror::new("threads/summary");
    feed(
        &mut child,
        Frame::read("threads/summary")
            .key("t1")
            .epoch(json!({"reinit": 0, "lost": 0, "state": "known", "parent_epoch": 4}))
            .row(json!({"idx": 1}))
            .cells(json!({})),
    );
    assert_eq!(child.era(&parent), Era::Same);

    // The parent re-inits. The CHILD is untouched — that is the ruling.
    feed(&mut parent, parent_at(5, 3, "one child"));
    assert_eq!(
        child.era(&parent),
        Era::ParentMovedOn {
            child_saw: 4,
            parent_now: 5
        },
        "and the repair is to re-read the PARENT, never the child"
    );
    assert_eq!(
        child.row("t1"),
        &json!({"idx": 1}),
        "the child is still valid on its own terms"
    );

    // A root shape never asks the question at all.
    let mut root = accounts();
    feed(&mut root, share("sam", 0.25));
    assert_eq!(root.era(&parent), Era::NoParent);
}

/// A nested shape stamps the parent era on every frame it emits, so an absence
/// means *root* — and a later frame that omits it must not erase what an
/// earlier one said.
#[test]
fn a_parent_epoch_once_stamped_is_not_erased_by_a_frame_that_omits_it() {
    let mut child = Mirror::new("threads/summary");
    feed(
        &mut child,
        Frame::read("threads/summary")
            .key("t1")
            .epoch(json!({"reinit": 0, "lost": 0, "state": "known", "parent_epoch": 7}))
            .row(json!({}))
            .cells(json!({})),
    );
    feed(
        &mut child,
        Frame::live("threads/summary", "2026-10-12T09:01:00Z")
            .key("t1")
            .event(json!({"kind": "row", "row": {"idx": 2}})),
    );
    assert_eq!(child.epoch().parent_epoch, Some(7));
}

/// **RULE 2.6's WARNING LATCHES; IT DOES NOT TRACK.**
///
/// `Epoch::superseded` is kept when a frame omits it, and the reason is
/// stronger than `init_at`'s: the DEATH FRAME is built outside the shape task
/// and addresses every connection at once, so it carries no `superseded`.
/// Tracking the field would repaint a superseded link as clean on precisely the
/// last frame it will ever receive — and rule 2.6's requirement, in its own
/// words, is that a consumer on a contract the producer no longer calls current
/// must not be able to look healthy.
///
/// **A superseded link is NOT terminal**: rule 2.6 says it SERVES, with a
/// warning, and a consumer that drew it as dead would be refusing a link the
/// producer accepted.
#[test]
fn a_superseded_warning_latches_and_the_death_frame_cannot_wash_it_off() {
    let mut m = accounts();
    assert_eq!(m.superseded(), None, "no frame has said so yet");

    feed(
        &mut m,
        share("sam", 0.25).epoch(json!({
            "init_at": INIT, "reinit": 0, "lost": 0, "state": "known",
            "superseded": {"wanted": 3, "current": 4},
        })),
    );
    assert_eq!(
        m.superseded().map(|s| (s.wanted, s.current)),
        Some((3, 4)),
        "both numbers, because \"you are old\" is not actionable"
    );
    assert!(!m.terminal(), "superseded SERVES; it is not a terminal state");
    assert!(m.live_shape());

    // An ordinary live frame omits the field. The warning stands.
    feed(
        &mut m,
        Frame::live("accounts", "2026-10-12T09:01:00Z")
            .key("sam")
            .event(json!({"kind": "row", "row": {"share": 0.3}})),
    );
    assert!(m.superseded().is_some(), "an omitting frame does not clear it");

    // And the death frame, which is the one that would otherwise wash it off.
    feed(&mut m, death(Some("supervisor gave up")));
    assert!(
        m.superseded().is_some(),
        "the last frame this link will ever see must not repaint it clean"
    );

    // Only a NEW link clears it, because a new link states its own version and
    // gets its own answer.
    m.reset();
    assert_eq!(m.superseded(), None);
}

/// **A MIRROR IS VALID ONLY FOR THE CONNECTION THAT WAS EXPORTED TO IT** — and
/// what survives a reset is exactly what `shapes.list` taught it, because every
/// one of those is a fact about the PRODUCER while a reset is about the
/// CONNECTION.
#[test]
fn reset_drops_the_connection_and_keeps_everything_shapes_list_taught_it() {
    let mut m = accounts();
    let mut listing = Listing::new();
    listing.cells = vec![declares("share", &[])];
    listing.refs = [("owner".to_string(), "accounts".to_string())]
        .into_iter()
        .collect();
    listing.page = Some(PageContract {
        default: Some(50),
        max: 500,
    });
    m.learn(&listing.info());

    feed(
        &mut m,
        share("sam", 0.25).promises(&[7]).subscribes(&[1]).complete(true),
    );
    assert!(m.resolved(7));
    assert_eq!(m.received(), 1);

    m.reset();

    // Gone: everything the CONNECTION held.
    assert!(m.keys().is_empty());
    assert!(!m.exported());
    assert_eq!(m.exports(), 0);
    assert_eq!(m.state(), &ShapeState::NotHydrated);
    assert!(m.members().keys().is_empty());
    assert_eq!(m.members().enumeration(), Enumeration::NotAsked);
    assert_eq!(m.last_frame_at(), None);
    assert_eq!(m.subscribe(1), SubscribeStatus::Unknown);
    assert_eq!(m.received(), 0);
    assert_eq!(
        m.key_state("sam"),
        KeyState::Unknown,
        "and the new connection has been told nothing about any key"
    );
    assert!(
        !m.resolved(7),
        "which ids this mirror has SEEN resolved is connection-scoped bookkeeping"
    );

    // Kept: everything `shapes.list` said. All six, named, because a seventh
    // added to `learn` and forgotten here is the defect this list exists to
    // catch.
    assert_eq!(m.name(), "accounts");
    assert_eq!(
        m.max().map(|x| x.rows_per_subscribe),
        Some(500),
        "the CEILINGS are a fact about the producer"
    );
    assert_eq!(m.contract().map(|c| c.version), Some(1));
    assert_eq!(m.parent(), Some("fleet"));
    assert_eq!(m.refers("owner"), Some("accounts"));
    assert_eq!(m.page().map(|p| p.max), Some(500));
    assert!(matches!(m.declared("share"), Declaration::Declared(_)));
}

/// **RULE 2.6 ACROSS A RESET: THE CONTRACT SURVIVES AND THE REFUSAL DOES NOT.**
///
/// The asymmetry is the whole content. Which versions the producer serves is a
/// fact about the producer, so it survives exactly as the ceilings do — and a
/// reconnecting consumer would otherwise be blind to the one thing it most
/// needs before re-linking, having to be refused a second time to re-learn what
/// it was already told. A version REFUSAL is a fact about a LINK, and a reset
/// ends that link, so it must not survive: a mirror left terminal across the
/// reconnect that exists to repair it is the opposite of what a reset is for.
#[test]
fn a_reset_keeps_the_version_contract_and_drops_the_version_refusal() {
    let mut m = accounts();
    let mut listing = Listing::new();
    listing.contract = ShapeContract {
        version: 4,
        serves: vec![4, 3],
    };
    m.learn(&listing.info());
    let mismatch = VersionMismatch::from_link("accounts", &m.link(Some(2)))
        .expect("v2 is not served, so the link is refused");
    m.refused_version(&mismatch);
    assert!(m.terminal(), "the precondition: this mirror really did go terminal");

    m.reset();
    assert_eq!(
        m.version_mismatch().map(|v| v.wanted),
        None,
        "a reset ends the link, so the refusal that ended it must not survive it: {:?}",
        m.version_mismatch().map(|v| v.wanted)
    );
    assert!(
        !m.terminal(),
        "and the mirror is usable again — a reset is the repair, not a second death"
    );
    assert_eq!(
        m.contract().map(|c| c.serves.clone()),
        Some(vec![4, 3]),
        "the CONTRACT is a fact about the producer, so a reconnect must not have \
         to re-learn it"
    );
    assert_eq!(
        m.link(Some(2)),
        Link::Refused {
            wanted: 2,
            serves: vec![4, 3]
        },
        "and it still predicts the same refusal, off the contract it kept"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Rule 2.4 — promises
// ─────────────────────────────────────────────────────────────────────────────

/// **AN EXPORT RE-OFFERS IDS A LOST FRAME ALREADY CARRIED**, so resolving one
/// twice is the ordinary case and has to be a no-op.
#[test]
fn re_offering_a_resolved_promise_resolves_nothing_twice() {
    let mut m = accounts();
    let r = feed(&mut m, share("sam", 0.25).promises(&[7, 9]));
    assert_eq!(r.promises, vec![7, 9]);
    assert!(m.resolved(7) && m.resolved(9) && !m.resolved(11));

    let r = feed(&mut m, share("sam", 0.5).promises(&[7, 9, 11]));
    assert_eq!(
        r.promises,
        vec![11],
        "the re-offer resolves only what was still owed"
    );
    assert_eq!(m.resolved_promises(), vec![7, 9, 11]);
}

/// **A FRAME THIS CONSUMER COULD NOT APPLY RESOLVES NOTHING.** A promise
/// resolves when the fact it is about is VISIBLE; a rejected frame put no fact
/// in the mirror, so the id stays pending and the promise stays visibly owed.
/// It is safe because an export re-offers every id still outstanding, and that
/// re-offer is asserted here rather than assumed.
#[test]
fn a_frame_that_did_not_apply_leaves_its_promise_visibly_owed() {
    let mut m = accounts();
    let r = feed(
        &mut m,
        appends("stranger", "2026-10-12T09:01:00Z", "moves", json!({})).promises(&[7]),
    );
    assert_eq!(r.disposition(), Disposition::NotApplied);
    assert!(r.promises.is_empty());
    assert!(
        !m.resolved(7),
        "an unresolved promise is visible; a silently discarded one is not"
    );

    // The export that follows re-offers it, which is the repair.
    let repair = feed(&mut m, share("stranger", 0.1).promises(&[7]));
    assert_eq!(repair.promises, vec![7]);
    assert!(m.resolved(7));
}

// ─────────────────────────────────────────────────────────────────────────────
// Rule 2.6 — a word this build does not know
// ─────────────────────────────────────────────────────────────────────────────

/// **AN UNKNOWN `event.kind` MUST NOT TAKE THE FRAME WITH IT.** `Derived` is
/// `#[serde(tag = "kind")]`, so a typed parse of a frame carrying a variant
/// this build predates fails outright, and losing the whole frame is rule 2.6's
/// soft spot 2 arriving as data loss instead of as a countable rejection.
#[test]
fn an_unknown_event_kind_is_counted_and_the_frame_around_it_survives() {
    let mut m = accounts();
    feed(&mut m, share("sam", 0.25));

    let frame = Frame::live("accounts", "2026-10-12T09:01:00Z")
        .key("sam")
        .epoch(json!({"reinit": 6, "lost": 2, "state": "known"}))
        .event(json!({"kind": "gauge_swept", "whatever": 1}))
        .promises(&[12]);
    // A typed parse cannot read it at all. That is the thing being repaired,
    // asserted rather than asserted-about.
    assert!(serde_json::from_value::<ShapeWire>(frame.0.clone()).is_err());

    let r = m.accept(&frame.0);
    assert_eq!(r.disposition(), Disposition::AppliedInPart);
    assert_eq!(r.rejected[0].reason, reject::UNKNOWN_KIND);
    assert_eq!(r.rejected[0].detail.as_deref(), Some("gauge_swept"));
    assert_eq!(
        m.epoch().reinit,
        6,
        "the epoch on the frame is still absorbed; only the event was unreadable"
    );
    assert_eq!(
        r.promises,
        vec![12],
        "and the frame's own bookkeeping lands, which is the whole point of not \
         losing the frame"
    );
    assert_eq!(m.row("sam"), &json!({"share": 0.25}));
}

/// The same thing one storey up: an `epoch.state` word this build does not know
/// leaves the state it had standing, and is counted.
#[test]
fn an_unknown_epoch_state_leaves_the_state_it_had_standing() {
    let mut m = accounts();
    feed(&mut m, share("sam", 0.25));
    assert_eq!(m.state(), &ShapeState::Known);

    let r = feed(
        &mut m,
        Frame::read("accounts").epoch(json!({"reinit": 1, "lost": 0, "state": "quiescing"})),
    );
    assert_eq!(r.disposition(), Disposition::AppliedInPart);
    assert_eq!(r.rejected[0].reason, reject::UNKNOWN_SHAPE_STATE);
    assert_eq!(r.rejected[0].detail.as_deref(), Some("quiescing"));
    assert_eq!(
        m.state(),
        &ShapeState::Known,
        "guessing that an unknown word means `fine` is how a board shows a \
         confident number it has no right to"
    );
    assert!(!m.dead(), "and it is certainly not read as a death");
    assert_eq!(m.epoch().reinit, 1, "the rest of the epoch still landed");

    // **AND IT LEAVES WHATEVER STATE IT HAD, NOT THE ONE THAT HAPPENS TO BE
    // COMMONEST.** A mirror that has had no export is `NotHydrated`, and a
    // substitution that wrote the literal `known` would be indistinguishable
    // from this behaviour above while inventing a hydrated fold here.
    let mut fresh_mirror = accounts();
    assert_eq!(fresh_mirror.state(), &ShapeState::NotHydrated);
    let r = feed(
        &mut fresh_mirror,
        Frame::read("accounts").epoch(json!({"reinit": 0, "lost": 0, "state": "quiescing"})),
    );
    assert_eq!(r.rejected[0].reason, reject::UNKNOWN_SHAPE_STATE);
    assert_eq!(
        fresh_mirror.state(),
        &ShapeState::NotHydrated,
        "a lazy shape nobody has subscribed to must not be painted as folding"
    );

    // And a DEAD fold stays dead: the one direction where guessing `known`
    // repaints a terminal state as healthy.
    let mut dead_mirror = accounts();
    feed(&mut dead_mirror, share("sam", 0.25));
    feed(&mut dead_mirror, death(Some("the task panicked")));
    assert!(dead_mirror.dead());
    let r = feed(
        &mut dead_mirror,
        Frame::read("accounts").epoch(json!({"reinit": 0, "lost": 0, "state": "quiescing"})),
    );
    assert_eq!(r.rejected[0].reason, reject::UNKNOWN_SHAPE_STATE);
    assert!(
        dead_mirror.dead(),
        "a terminal state is not repainted healthy by a word nobody knows"
    );
}

/// A cell whose `state` word this build does not know is held as a HOLE with
/// the word quoted — never as `Known`. The value still arrived; it is the
/// provenance that is unknown.
#[test]
fn an_unknown_cell_state_word_is_held_as_a_hole_never_as_known() {
    let mut m = accounts();
    let r = feed(
        &mut m,
        Frame::read("accounts")
            .key("sam")
            .row(json!({"share": 0.25}))
            .cells(json!({"share": {"state": "estimated", "folded_at": FOLD}})),
    );
    assert_eq!(r.disposition(), Disposition::AppliedInPart);
    assert_eq!(r.rejected[0].reason, reject::UNKNOWN_CELL_STATE);
    assert_eq!(r.rejected[0].detail.as_deref(), Some("estimated"));
    match m.cell("sam", "share") {
        CellState::NotHydrated { reason } => assert_eq!(
            reason.as_deref(),
            Some(r#"unrecognised cell state "estimated""#)
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        m.row("sam"),
        &json!({"share": 0.25}),
        "the VALUE still arrived; it is the provenance that is unknown"
    );
    // The same leniency on the shape-level block, which is a separate path.
    let r = feed(
        &mut m,
        Frame::read("accounts")
            .key("sam")
            .row(json!({}))
            .cells(json!({}))
            .shared_cells(json!({"warn_below_days": {"state": "guessed"}})),
    );
    assert_eq!(r.rejected[0].reason, reject::UNKNOWN_CELL_STATE);
    assert_eq!(r.rejected[0].detail.as_deref(), Some("guessed"));
    assert!(matches!(
        m.shared_cell("warn_below_days"),
        CellState::NotHydrated { .. }
    ));
}

/// **AN UNKNOWN `members.state` LEAVES THE ROSTER STANDING AND STILL SETTLES
/// THE SUBSCRIBE.** `MembersPage` is `serde(tag = "state")` over a closed enum,
/// and a membership frame is an `op: read` answering a `detail: keys`
/// subscribe — so what used to go with the lost frame was that subscribe's id,
/// and the observable defect was a subscribe that never settled, with nothing
/// to wait on and no refusal to render.
#[test]
fn an_unknown_members_state_leaves_the_roster_standing_and_settles_the_subscribe() {
    let mut m = Mirror::new("threads");
    feed(
        &mut m,
        Frame::read("threads").members(json!({"state": "known", "keys": ["a"], "complete": false})),
    );

    let r = feed(
        &mut m,
        Frame::read("threads")
            .members(json!({"state": "sharded", "shards": 4}))
            .subscribes(&[9])
            .complete(true),
    );
    assert_eq!(r.disposition(), Disposition::AppliedInPart);
    assert_eq!(r.rejected[0].reason, reject::UNKNOWN_MEMBERS_STATE);
    assert_eq!(r.rejected[0].detail.as_deref(), Some("sharded"));
    assert_eq!(
        m.members().keys(),
        vec!["a"],
        "an unheard-of word is neither \"here are the keys\" nor \"I cannot \
         enumerate\", so the roster this consumer holds STANDS"
    );
    assert_eq!(
        m.members().enumeration(),
        Enumeration::Known { complete: false },
        "and the claim it already held is not overwritten either"
    );
    assert_eq!(
        m.subscribe(9),
        SubscribeStatus::Complete,
        "the subscribe SETTLES — that is what not losing the frame buys"
    );
    assert_eq!(
        m.exports(),
        0,
        "and it was not mistaken for a shape-level read or for a death notice"
    );
}

/// **AN UNKNOWN `op` APPLIES NO BODY AND STILL LANDS THE BOOKKEEPING.** Every
/// frame has an `op` and no frame survives one that does not parse. *Replace
/// one key* and *append an occurrence* are what an `op` chooses between and
/// there is no safe third guess, so the body is skipped — but the epoch, rule
/// 2.7's stages and the promises all land, because a consumer wedged by one
/// word it does not recognise is strictly worse than one that can SEE it missed
/// a body.
#[test]
fn an_unknown_op_applies_no_body_and_still_lands_the_bookkeeping() {
    let mut m = accounts();
    feed(&mut m, share("sam", 0.25));

    let r = feed(
        &mut m,
        share("sam", 0.99)
            .set("op", json!("merge"))
            .epoch(json!({"reinit": 8, "lost": 1, "state": "known"}))
            .promises(&[21])
            .subscribes(&[22])
            .complete(true),
    );
    assert_eq!(r.disposition(), Disposition::AppliedInPart);
    assert_eq!(r.rejected[0].reason, reject::UNKNOWN_OP);
    assert_eq!(r.rejected[0].detail.as_deref(), Some("merge"));
    assert_eq!(
        m.row("sam"),
        &json!({"share": 0.25}),
        "NO BODY: the 0.99 was not applied"
    );
    assert_eq!(m.epoch().reinit, 8, "the epoch landed");
    assert_eq!(r.promises, vec![21], "the promise resolved");
    assert_eq!(m.subscribe(22), SubscribeStatus::Complete, "the stage settled");

    // An unknown `op` on a frame whose `event.kind` is ALSO unknown counts
    // both, because each is a separate fact about the seam.
    let r = feed(
        &mut m,
        Frame::live("accounts", "2026-10-12T09:02:00Z")
            .key("sam")
            .set("op", json!("merge"))
            .event(json!({"kind": "gauge_swept"})),
    );
    let codes: Vec<&str> = r.rejected.iter().map(|x| x.reason.as_str()).collect();
    assert_eq!(codes, vec![reject::UNKNOWN_OP, reject::UNKNOWN_KIND]);
}

/// A field this build predates is IGNORED, not refused — rule 2.6's obligation
/// on every consumer, at the one level where it costs nothing.
#[test]
fn a_field_this_build_predates_is_ignored_rather_than_refused() {
    let mut m = accounts();
    let r = feed(
        &mut m,
        Frame::read("accounts")
            .key("sam")
            .row(json!({"share": 0.25, "a_column_added_next_quarter": "x"}))
            .cells(json!({"share": fresh(FOLD)}))
            .set("cadence_ms", json!(250)),
    );
    assert_eq!(
        r.disposition(),
        Disposition::Applied,
        "an unknown top-level field is not even a seam fact"
    );
    assert_eq!(
        m.row("sam").get("a_column_added_next_quarter"),
        Some(&json!("x")),
        "an unknown ROW field is held verbatim: the shape owns the row, not the \
         chassis"
    );
}

/// Another shape's frame is not this mirror's business, and must not throw. It
/// is reported and NOT counted: a frame that was never this mirror's was never
/// this mirror's to receive, so it reaches no ack.
#[test]
fn another_shapes_frame_is_reported_and_never_raised() {
    let mut m = accounts();
    let r = feed(
        &mut m,
        Frame::read("narration").key("x").row(json!({})).cells(json!({})),
    );
    assert_eq!(r.disposition(), Disposition::NotApplied);
    assert_eq!(r.rejected[0].reason, reject::NOT_MINE);
    assert_eq!(r.rejected[0].detail.as_deref(), Some("narration"));
    assert_eq!(m.received(), 0);
    assert!(
        m.rejections().is_empty(),
        "and it is not on the ack either — the producer is not owed a complaint \
         about a frame this mirror was never the audience for"
    );
}

/// Bytes that are not a frame at all are RECEIVED and not applied, and the
/// producer can count that — which is why the reason is a code and not prose.
#[test]
fn bytes_that_are_not_a_frame_are_still_a_receipt() {
    let mut m = accounts();
    let r = m.accept(&json!({"shape": "accounts", "op": "sideways"}));
    assert_eq!(r.disposition(), Disposition::NotApplied);
    assert_eq!(r.rejected[0].reason, reject::UNREADABLE);
    assert!(
        r.rejected[0].detail.is_some(),
        "serde's own words ride along: they are the one thing here a human can act on"
    );
    assert_eq!(m.received(), 1);
    assert_eq!(m.rejections().len(), 1, "and this one IS on the ack");
}

/// A live frame with no event states nothing at all, which is NOT the same as a
/// word this build failed to know — so it is unreadable rather than an unknown
/// kind.
#[test]
fn a_live_frame_with_no_event_is_not_an_unknown_word() {
    let mut m = accounts();
    let r = feed(&mut m, Frame::live("accounts", "2026-10-12T09:01:00Z").key("sam"));
    assert_eq!(r.disposition(), Disposition::NotApplied);
    assert_eq!(r.rejected[0].reason, reject::UNREADABLE);
    assert_eq!(
        r.rejected[0].detail.as_deref(),
        Some("a live frame with no event")
    );
}

/// Every code this module can mint is in `reject::ALL`, so every one of them
/// can be tallied by a producer that knows nothing else about this consumer.
#[test]
fn every_code_this_module_mints_is_in_the_published_vocabulary() {
    let mut m = accounts();
    // One frame per code, in one run, so the set is collected and not asserted
    // about one at a time.
    feed(&mut m, Frame::read("narration").key("x").row(json!({})).cells(json!({})));
    feed(&mut m, appends("ghost", "2026-10-12T09:01:00Z", "moves", json!({})));
    feed(
        &mut m,
        Frame::read("accounts")
            .key("sam")
            .row(json!({}))
            .cells(json!({"share": {"state": "estimated"}})),
    );
    feed(
        &mut m,
        Frame::read("accounts").epoch(json!({"reinit": 0, "lost": 0, "state": "quiescing"})),
    );
    feed(&mut m, Frame::read("accounts").members(json!({"state": "sharded"})));
    feed(&mut m, share("sam", 0.1).set("op", json!("merge")));
    feed(
        &mut m,
        Frame::live("accounts", "2026-10-12T09:02:00Z")
            .key("sam")
            .event(json!({"kind": "gauge_swept"})),
    );
    m.accept(&json!({"shape": "accounts", "op": "read"}));

    let seen: BTreeSet<&str> = m.rejections().iter().map(|r| r.reason.as_str()).collect();
    for code in &seen {
        assert!(
            reject::ALL.contains(code),
            "{code} is not in the published vocabulary"
        );
    }
    // And the run really did exercise most of it — a vacuous pass here would be
    // an empty set satisfying the loop above.
    assert!(
        seen.len() >= 6,
        "this run should have produced six or more distinct codes, got {seen:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Rule 2.3 — the ack
// ─────────────────────────────────────────────────────────────────────────────

/// An ack says what receipt did, and it DRAINS: a rejection is reported once,
/// so a consumer that never acks is not one that accumulates the same complaint
/// for ever.
#[test]
fn an_ack_drains_so_a_rejection_is_reported_exactly_once() {
    let mut m = accounts();
    feed(&mut m, share("sam", 0.25));
    feed(&mut m, appends("ghost", "2026-10-12T09:01:00Z", "moves", json!({})));

    let ack = m.ack();
    assert_eq!(ack.shape, "accounts");
    assert_eq!(ack.received_through, 2);
    assert_eq!(ack.rejected.len(), 1);
    assert_eq!(ack.rejected[0].reason, reject::NO_EXPORT);
    assert_eq!(ack.rejected[0].serial, 2, "and it names the frame it was about");

    let ack = m.ack();
    assert!(
        ack.rejected.is_empty(),
        "a consumer that acks twice does not complain twice"
    );
    assert_eq!(
        ack.received_through, 2,
        "but the receipt count is monotonic, because it is what a retention \
         bound is computed against"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Rule 2.8 — the published ceilings
// ─────────────────────────────────────────────────────────────────────────────

/// **A CONSUMER READS THE CONTRACT BEFORE IT USES IT**, and an ask beyond a
/// ceiling is refused NAMING the ceiling — never clamped.
///
/// **`Mirror` ARBITRATES, SO EVERY AXIS IS CHECKED AT N ≥ 2 AND AT THE REAL
/// CONSTANT.** Each of the five is driven at the ceiling (allowed) and at the
/// ceiling plus one (refused), against the number `shapes.list` actually
/// published rather than a fabricated small one — a boundary asserted at a
/// fabricated `max` is a boundary that moves when the real constant does and
/// says nothing about it.
#[test]
fn an_ask_over_a_published_ceiling_is_refused_naming_the_ceiling() {
    let mut m = accounts();
    assert!(
        m.check(&Ask {
            rows: Some(1_000_000),
            ..Ask::default()
        })
        .is_ok(),
        "a mirror with no `shapes.list` has no contract to judge against, and \
         must not invent one"
    );

    m.learn(&Listing::new().info());
    let max = ceilings();
    assert_eq!(m.max().unwrap().rows_per_subscribe, max.rows_per_subscribe);
    assert_eq!(m.parent(), Some("fleet"));
    assert!(
        m.check(&Ask::default()).is_ok(),
        "asking for nothing MEANS the ceiling"
    );

    // Every axis, both arms, at the real published constant. `at` has to carry
    // its own pair because it is an `i64` of seconds rather than a count.
    let counted: [(fn(usize) -> Ask, usize, fn(usize, usize) -> OverCeiling); 4] = [
        (
            |n| Ask {
                rows: Some(n),
                ..Ask::default()
            },
            max.rows_per_subscribe,
            |asked, ceiling| OverCeiling::Rows { asked, ceiling },
        ),
        (
            |n| Ask {
                keys_per_frame: Some(n),
                ..Ask::default()
            },
            max.keys_per_frame,
            |asked, ceiling| OverCeiling::KeysPerFrame { asked, ceiling },
        ),
        (
            |n| Ask {
                armed_timers: Some(n),
                ..Ask::default()
            },
            max.armed_timers_per_conn,
            |asked, ceiling| OverCeiling::ArmedTimers { asked, ceiling },
        ),
        (
            // A `page` IS rows, so it rides the row ceiling rather than having
            // a dial of its own: a second number would be a second policy.
            |n| Ask {
                page: Some(n),
                ..Ask::default()
            },
            max.rows_per_subscribe,
            |asked, ceiling| OverCeiling::Page { asked, ceiling },
        ),
    ];
    for (ask, ceiling, expected) in counted {
        assert!(
            m.check(&ask(ceiling)).is_ok(),
            "asking for exactly the ceiling is inside it: {:?}",
            ask(ceiling)
        );
        let err = m.check(&ask(ceiling + 1)).unwrap_err();
        assert_eq!(err, expected(ceiling + 1, ceiling));
        assert!(
            err.to_string().contains(&ceiling.to_string()),
            "the refusal NAMES the ceiling: {err}"
        );
    }

    let secs = max.future_at_secs;
    assert!(m
        .check(&Ask {
            future_at_secs: Some(secs),
            ..Ask::default()
        })
        .is_ok());
    let err = m
        .check(&Ask {
            future_at_secs: Some(secs + 1),
            ..Ask::default()
        })
        .unwrap_err();
    assert_eq!(
        err,
        OverCeiling::FutureAt {
            asked: secs + 1,
            ceiling: secs
        }
    );
    assert!(err.to_string().contains(&secs.to_string()), "{err}");
}

/// The ceilings are a fact about THIS shape. A `shapes.list` entry for another
/// one is not this mirror's contract, and a mirror that learned a neighbour's
/// would refuse against the wrong numbers.
#[test]
fn a_mirror_never_learns_another_shapes_ceilings() {
    let mut m = accounts();
    let mut other = Listing::new();
    other.name = "threads".into();
    other.max = ShapeMax {
        rows_per_subscribe: 1,
        keys_per_frame: 1,
        future_at_secs: 1,
        armed_timers_per_conn: 1,
        ..ceilings()
    };
    other.cells = vec![declares("body", &[])];
    m.learn(&other.info());

    assert!(m.max().is_none());
    assert!(m.contract().is_none());
    assert_eq!(m.parent(), None);
    assert_eq!(
        m.declared("body"),
        Declaration::Unstated,
        "and it did not learn the neighbour's manifest either"
    );
    assert!(m
        .check(&Ask {
            rows: Some(400),
            ..Ask::default()
        })
        .is_ok());
}

// ─────────────────────────────────────────────────────────────────────────────
// The cell manifest — EXISTENCE, kept apart from DELIVERY
// ─────────────────────────────────────────────────────────────────────────────

/// **A CELL THE PRODUCER DOES NOT DECLARE READS AS "NEVER", NOT AS "NOT
/// YET".**
///
/// The live instance, out of substrate: `pending`, read off `threads` rows for
/// months, never declared, never carried, always `NotHydrated { "no export has
/// carried this cell" }` — which the reader took for patience. Before `learn`
/// both answers are the same `Unstated`; after it, an undeclared name is told
/// apart from a declared one that has merely not arrived, in the typed answer
/// AND in the reason a human reads.
#[test]
fn a_cell_the_producer_does_not_declare_reads_as_never_not_as_not_yet() {
    let mut m = accounts();
    // Before any manifest: nothing can be said either way, about either name.
    assert_eq!(m.declared("share"), Declaration::Unstated);
    assert_eq!(m.declared("pending"), Declaration::Unstated);
    assert!(matches!(
        m.cell("sam", "pending"),
        CellState::NotHydrated { reason: Some(r) } if r == UNSEEN
    ));

    let mut listing = Listing::new();
    listing.cells = vec![
        declares("share", &[]),
        declares("status", &["state", "executing"]),
    ];
    listing.shared_cells = vec![declares("warn_below_days", &[])];
    m.learn(&listing.info());
    assert_eq!(
        m.manifest().map(|(cells, shared)| (cells.len(), shared.len())),
        Some((2, 1))
    );
    feed(&mut m, share("sam", 0.25));

    // A DECLARED cell that has not arrived is still "not yet".
    assert!(matches!(m.declared("status"), Declaration::Declared(c) if c.name == "status"));
    assert!(matches!(
        m.cell("sam", "status"),
        CellState::NotHydrated { reason: Some(r) } if r == UNSEEN
    ));
    match m.declared("status") {
        Declaration::Declared(c) => {
            assert!(c.field("executing").is_some());
            assert!(
                c.field("pending").is_none(),
                "a field is declared or it is not"
            );
        }
        other => panic!("{other:?}"),
    }

    // An UNDECLARED cell is `Undeclared`, naming what IS declared.
    assert_eq!(
        m.declared("pending"),
        Declaration::Undeclared {
            declares: vec!["share".into(), "status".into()]
        },
        "an undeclared cell is `Undeclared`, naming what IS declared"
    );
    // …and the three-valued state a renderer draws says NEVER in its reason,
    // for a held key and for a key never exported alike.
    for key in ["sam", "nobody"] {
        match m.cell(key, "pending") {
            CellState::NotHydrated { reason: Some(r) } => assert!(
                r.contains("`pending` is not a cell `accounts` declares")
                    && r.contains("no export will ever carry it")
                    && r.contains("share, status"),
                "{key}: {r}"
            ),
            other => panic!("{key}: {other:?}"),
        }
    }

    // The shape-level manifest is a question of its own: a per-key cell is not
    // a shape-level one.
    assert!(matches!(
        m.shared_declared("warn_below_days"),
        Declaration::Declared(_)
    ));
    assert_eq!(
        m.shared_declared("share"),
        Declaration::Undeclared {
            declares: vec!["warn_below_days".into()]
        }
    );
    assert!(matches!(
        m.shared_cell("share"),
        CellState::NotHydrated { reason: Some(r) } if r.contains("no export will ever carry it")
    ));

    // A producer that declares NOTHING at a level says so in those words,
    // rather than printing an empty list.
    let mut bare = accounts();
    bare.learn(&Listing::new().info());
    assert!(matches!(
        bare.cell("sam", "share"),
        CellState::NotHydrated { reason: Some(r) } if r.contains("nothing at this level")
    ));
}

/// **A DECLARED CELL THAT HAS ARRIVED IS NEVER OVERWRITTEN BY THE MANIFEST.**
/// The manifest says what can arrive; the frame says what did. This is the
/// pairing that stops `absence` being read as a cell state source.
#[test]
fn the_manifest_never_overrides_a_cell_that_actually_arrived() {
    let mut m = accounts();
    let mut listing = Listing::new();
    listing.cells = vec![declares("gauge", &[])];
    m.learn(&listing.info());
    // `share` is NOT declared, and it arrives anyway.
    feed(&mut m, share("sam", 0.25));
    assert!(
        m.cell("sam", "share").is_known(),
        "a cell that arrived is `Known` whatever the manifest says it declares"
    );
    assert!(matches!(
        m.declared("share"),
        Declaration::Undeclared { .. }
    ));
}

/// **THE MANIFEST SURVIVES A RESET, AS THE CEILINGS AND THE CONTRACT DO**: it
/// is a fact about the producer, and a reconnect is about the connection.
#[test]
fn the_manifest_survives_a_reset_as_the_ceilings_do() {
    let mut m = accounts();
    let mut listing = Listing::new();
    listing.cells = vec![declares("share", &[])];
    m.learn(&listing.info());
    m.reset();
    assert_eq!(
        m.declared("pending"),
        Declaration::Undeclared {
            declares: vec!["share".into()]
        },
        "a reconnect must not make every cell read as `Unstated` again"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// What is known about one key's existence
// ─────────────────────────────────────────────────────────────────────────────

/// **A KEYED EXPORT WITH NO ROW AND NO CELLS IS "I DO NOT HOLD THIS KEY", AND
/// THE MIRROR SAYS SO** — `Absent`, not `Held`.
///
/// This was the contradiction LOCKED into substrate's seam kit as
/// `conformance/absent_key.jsonl`: `hub::export` emits `row: None, cells: None`
/// for a key the root does not hold, `wire::Derived::Added`'s doc says so, and
/// the mirror folded that frame as a bare row — `key_state` answered `Held`
/// while every cell under it answered *no export has carried this cell*, for
/// ever.
#[test]
fn a_row_less_keyed_export_is_absent_and_not_held() {
    let mut m = accounts();
    let r = feed(&mut m, Frame::read("accounts").key("ghost").subscribes(&[27]).complete(true));
    assert!(r.applied(), "the export is applied — it answers the subscribe");
    assert_eq!(r.subscribes, vec![27], "…by name");
    assert_eq!(r.complete, Some(true));
    assert!(r.moved, "the mirror learned something: the key is absent");
    assert_eq!(
        m.key_state("ghost"),
        KeyState::Absent,
        "a row-less keyed export is Absent, never Held"
    );
    assert_eq!(m.absent(), vec!["ghost".to_string()]);
    assert!(m.keys().is_empty(), "no row is held for it");
    assert!(
        !m.members().contains("ghost"),
        "and the producer did NOT say the key exists"
    );
    assert!(
        m.exported() && m.exports() == 1,
        "an answered subscribe is an export, even one with no row"
    );
    assert!(matches!(
        m.cell("ghost", "share"),
        CellState::NotHydrated { .. }
    ));
    // Saying it twice moves nothing.
    assert!(!feed(&mut m, Frame::read("accounts").key("ghost")).moved);
}

/// **HELD, GONE, UNKNOWN AND ABSENT ARE FOUR DIFFERENT ANSWERS ABOUT ONE
/// KEY**, and the transitions between them are what a reader watches. All four
/// in one run, on one connection, because a type that arbitrates between states
/// is not proven by one of them.
#[test]
fn held_gone_unknown_and_absent_are_four_answers_about_one_key() {
    let mut m = accounts();
    assert_eq!(m.key_state("k"), KeyState::Unknown);

    // Absent: the producer answered with no row.
    feed(&mut m, Frame::read("accounts").key("k"));
    assert_eq!(m.key_state("k"), KeyState::Absent);
    assert_eq!(m.vanished(), Vec::<String>::new());

    // Held: a later export WITH a row is the producer now holding it.
    feed(&mut m, share("k", 0.5));
    assert_eq!(m.key_state("k"), KeyState::Held);
    assert!(m.absent().is_empty(), "and it is no longer absent");
    assert!(m.members().contains("k"));

    // Gone: and a `gone` after that is Gone, not Absent. The two are kept
    // apart because a key that was never held did not leave.
    feed(
        &mut m,
        Frame::live("accounts", "2026-10-12T09:02:00Z")
            .key("k")
            .event(json!({"kind": "gone"})),
    );
    assert_eq!(m.key_state("k"), KeyState::Gone);
    assert!(m.absent().is_empty());
    assert_eq!(m.vanished(), vec!["k".to_string()]);

    // Held again: an export of a key that had been unsaid is the producer
    // unsaying the removal, and a mirror that kept the `Gone` would be the
    // stale claim in the other direction.
    feed(&mut m, share("k", 0.75));
    assert_eq!(m.key_state("k"), KeyState::Held);
    assert!(m.vanished().is_empty());

    // Absent again: and an absence after a row drops the row.
    feed(&mut m, Frame::read("accounts").key("k"));
    assert_eq!(m.key_state("k"), KeyState::Absent);
    assert!(m.keys().is_empty());
}

/// A live `row` for a key the producer had unsaid, or had answered with no row,
/// is the producer taking it back — through the OTHER door, which is a separate
/// code path from the export's.
#[test]
fn a_live_row_takes_back_both_a_gone_and_an_absence() {
    for first in ["gone", "absent"] {
        let mut m = accounts();
        match first {
            "gone" => {
                feed(&mut m, share("k", 0.25));
                feed(
                    &mut m,
                    Frame::live("accounts", "2026-10-12T09:01:00Z")
                        .key("k")
                        .event(json!({"kind": "gone"})),
                );
                assert_eq!(m.key_state("k"), KeyState::Gone);
            }
            _ => {
                feed(&mut m, Frame::read("accounts").key("k"));
                assert_eq!(m.key_state("k"), KeyState::Absent);
            }
        }
        feed(
            &mut m,
            Frame::live("accounts", "2026-10-12T09:02:00Z")
                .key("k")
                .event(json!({"kind": "row", "row": {"share": 1.0}})),
        );
        assert_eq!(m.key_state("k"), KeyState::Held, "after {first}");
        assert!(m.vanished().is_empty() && m.absent().is_empty(), "after {first}");
        assert!(m.members().contains("k"), "after {first}");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Nesting, and the whole lifecycle
// ─────────────────────────────────────────────────────────────────────────────

/// **WHICH SHAPE A `Ref` CELL'S IDS ARE KEYS OF**, and the two statements
/// `None` collapses into, told apart with `max()` exactly as the doc says.
#[test]
fn a_ref_cell_names_the_shape_its_ids_are_keys_of() {
    let blind = accounts();
    assert_eq!(blind.refers("owner"), None);
    assert!(
        blind.max().is_none(),
        "a mirror never handed a `ShapeInfo` knows nothing, and says so twice"
    );

    let mut m = accounts();
    let mut listing = Listing::new();
    listing.refs = [
        ("owner".to_string(), "accounts".to_string()),
        ("thread".to_string(), "threads".to_string()),
    ]
    .into_iter()
    .collect();
    m.learn(&listing.info());

    assert_eq!(m.refers("thread"), Some("threads"));
    assert_eq!(
        m.refers("nowhere"),
        None,
        "a mirror that WAS handed a `ShapeInfo` and still answers None has been \
         told this name holds ids in a space no shape serves"
    );
    assert!(m.max().is_some(), "which is how the caller tells the two apart");
    assert_eq!(
        m.refs().collect::<Vec<_>>(),
        vec![("owner", "accounts"), ("thread", "threads")],
        "in name order"
    );
}

/// One connection, end to end: hydrate, burst, a key joining, a key leaving,
/// the task dying. No daemon, no clock, nothing sleeping — which is the
/// property that makes a mirror testable at all, and the evidence behind this
/// module living in the floor rather than behind `hub`.
#[test]
fn one_connections_whole_life_runs_with_no_daemon_and_no_clock() {
    let mut m = accounts();
    feed(
        &mut m,
        Frame::read("accounts")
            .key("sam")
            .row(json!({"share": 0.25, "moves": []}))
            .cells(json!({"share": fresh(FOLD)}))
            .shared(json!({"warn_below_days": 3}))
            .subscribes(&[1]),
    );
    feed(
        &mut m,
        Frame::read("accounts")
            .key("kim")
            .row(json!({"share": 0.75, "moves": []}))
            .cells(json!({}))
            .subscribes(&[1])
            .complete(true),
    );
    assert_eq!(m.subscribe(1), SubscribeStatus::Complete);
    assert_eq!(m.keys(), vec!["kim", "sam"], "sorted, not in arrival order");

    for n in 0..5 {
        feed(
            &mut m,
            appends(
                "sam",
                &format!("2026-10-12T09:0{}:00Z", n + 1),
                "moves",
                json!({"n": n}),
            ),
        );
    }
    assert_eq!(m.held("sam").unwrap().items("moves").len(), 5);

    feed(
        &mut m,
        Frame::live("accounts", "2026-10-12T09:06:00Z")
            .key("ada")
            .event(json!({"kind": "added"})),
    );
    assert!(m.members().contains("ada") && m.held("ada").is_none());

    feed(
        &mut m,
        Frame::live("accounts", "2026-10-12T09:07:00Z")
            .key("kim")
            .event(json!({"kind": "gone"})),
    );
    assert_eq!(m.keys(), vec!["sam"]);

    feed(&mut m, death(Some("supervisor gave up")));
    assert!(m.dead());
    assert_eq!(
        m.keys(),
        vec!["sam"],
        "the rows are kept for the operator to read"
    );
    assert!(matches!(m.cell("sam", "share"), CellState::Dead { .. }));
    assert_eq!(m.last_frame_at(), stamp("2026-10-12T09:10:00Z"));
    assert!(
        m.ack().rejected.is_empty(),
        "and nothing in that whole run was a seam fact"
    );
}

/// **EVERY GOLDEN FRAME `wire.rs` PINS GOES THROUGH THE MIRROR.** If the
/// producer's bytes move, the consumer that reads them moves with them.
///
/// These three strings are the pinned bytes themselves, copied from
/// `wire::round_trip`'s `GOLD_LOADED`, `GOLD_LIVE` and `GOLD_DEAD` — which are
/// private to that module, so they cannot be imported. They are the CONTRACT
/// and are deliberately not reworded.
#[test]
fn the_frames_wire_rs_pins_as_golden_all_apply() {
    let mut m = accounts();
    let loaded: Value = serde_json::from_str(
        r#"{"promises":[7,9],"shape":"accounts","key":"anthropic:sam","op":"read","as_of":"2026-09-25T12:00:00Z","epoch":{"init_at":"2026-09-25T11:59:59Z","reinit":2,"lost":41,"state":"known"},"cells":{"share":{"complete":true,"folded_at":"2026-09-25T12:00:00Z","state":"known"}},"row":{"account":"anthropic:sam","share":0.25},"shared":{"warn_below_days":3},"shared_cells":{"warn_below_days":{"state":"known"}}}"#,
    )
    .expect("the golden export is JSON");
    let r = m.accept(&loaded);
    assert_eq!(r.disposition(), Disposition::Applied);
    assert_eq!(r.promises, vec![7, 9]);
    assert_eq!(m.row("anthropic:sam")["share"], json!(0.25));
    assert_eq!(m.epoch().lost, 41);
    assert_eq!(m.epoch().reinit, 2);
    // `shared_cells.warn_below_days` is `{"state":"known"}` with no
    // `folded_at` — the golden's own bytes. A mirror holds it as `Known`
    // rather than refusing the frame over a missing stamp.
    assert!(m.shared_cell("warn_below_days").is_known());

    let live: Value = serde_json::from_str(
        r#"{"shape":"accounts","op":"live","at":"2026-09-25T12:00:00.123Z","epoch":{"reinit":0,"lost":0,"state":"not_hydrated"},"event":{"kind":"transition","cell":"moves","item":{"from":"idle","to":"busy"}}}"#,
    )
    .expect("the golden live frame is JSON");
    assert_eq!(
        m.accept(&live).rejected[0].reason,
        reject::NO_EXPORT,
        "the golden live frame is KEYLESS, so there is no key to append to"
    );

    let dead: Value = serde_json::from_str(
        r#"{"shape":"accounts","op":"read","as_of":"2026-09-25T12:00:00Z","epoch":{"init_at":"2026-09-25T11:00:00Z","reinit":0,"lost":0,"state":"dead","dead_reason":"the shape task panicked"}}"#,
    )
    .expect("the golden death notice is JSON");
    m.accept(&dead);
    assert!(m.dead());
    assert_eq!(m.dead_reason(), Some("the shape task panicked"));
}

/// **`apply` AND `accept` AGREE ON ANY FRAME THAT PARSES.** `accept` is
/// `apply` plus the repair pass, and nothing else: if the repair pass could
/// change the outcome of a frame it had nothing to repair, every test in this
/// file would be measuring the repair pass rather than the state machine.
#[test]
fn accept_is_apply_plus_the_repair_pass_and_nothing_else() {
    let frames = [
        share("sam", 0.25).promises(&[7]).subscribes(&[1]).complete(true),
        appends("sam", "2026-10-12T09:01:00Z", "moves", json!({"n": 1})),
        Frame::read("accounts").key("ghost"),
        death(Some("the task panicked")),
    ];
    let mut through_accept = accounts();
    let mut through_apply = accounts();
    for frame in frames {
        let typed: ShapeWire =
            serde_json::from_value(frame.0.clone()).expect("these frames all parse");
        let a = through_accept.accept(&frame.0);
        let b = through_apply.apply(&typed);
        assert_eq!(a, b, "the two doors disagree on {:?}", frame.0);
    }
    assert_eq!(through_accept.keys(), through_apply.keys());
    assert_eq!(through_accept.row("sam"), through_apply.row("sam"));
    assert_eq!(
        through_accept.key_state("ghost"),
        through_apply.key_state("ghost")
    );
    assert_eq!(through_accept.received(), through_apply.received());
}
