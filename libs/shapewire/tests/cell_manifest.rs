//! # THE CELL MANIFEST, END TO END: declared on the shape, published on
//! `shapes.list`, learned by the mirror, and then a cell that was never
//! declared reads as NEVER instead of NOT YET
//!
//! `shapewire::cell` is the module doc; this file is the evidence that the three
//! doors it names are one path. The unit tests beside each half prove the half;
//! this proves that what `Hub::register` reads off `Shape::CELLS` is what a
//! consumer's `Mirror` is told, with nothing hand-copied in between — which is
//! the whole claim: the manifest a consumer routes against is the producer's
//! declaration and not somebody's transcription of it.
//!
//! **Every test here was forced RED by a named mutation before it was believed**,
//! and the mutation is in the test's own docstring.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};

use shapewire::cell::Cells;
use shapewire::fold::{Outbox, Root};
use shapewire::hub::{Hub, Kind, Shape, Tracking, Tx};
use shapewire::mirror::{Declaration, Mirror};
use shapewire::operation::{
    DType::{Enum, Map, Opaque, Scalar, Struct},
    Presence::{Optional, Required},
    Scalar::{Bool, Int, Str},
};
use shapewire::wire::{CellState, ShapeInfo, Stamped, Timestamp};

// ─────────────────────────────────────────────────────────────────────────────
// The smallest producer with a manifest worth publishing
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct Beat1;
impl Kind for Beat1 {
    fn kind(&self) -> &'static str {
        "beat"
    }
}

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
        self.keys
            .contains(k)
            .then(|| json!({ "status": { "state": "active", "executing": true }, "queued": 0 }))
    }
    fn cells(&self, k: &str) -> Option<Value> {
        self.keys.contains(k).then(|| {
            let known = CellState::Known {
                complete: true,
                folded_at: chrono::Utc::now(),
                since_init: false,
                covers_since: None,
            };
            json!({ "status": known, "queued": known })
        })
    }
    fn shared(&self) -> Value {
        json!({ "fleet_size": 1 })
    }
    fn shared_cells(&self) -> Value {
        json!({ "fleet_size": CellState::Known {
            complete: true,
            folded_at: chrono::Utc::now(),
            since_init: false,
            covers_since: None,
        }})
    }
    fn drain(&mut self) -> Outbox {
        Outbox {
            rows: std::mem::take(&mut self.dirty).into_iter().collect(),
            ..Default::default()
        }
    }
    fn mark_dead(&mut self, _: &str) {}
}

shapewire::declare_cells! {
    /// What `Keys::cells` emits, declared: a struct with a closed vocabulary, a
    /// scalar, and — to show the twelfth dtype reaches the wire — a map.
    POOL_CELLS = [
        ("status", Struct("StateLevel"), Required, [
            ("state", Enum(&["active", "idle"]), Required),
            ("executing", Scalar(Bool), Required),
            ("detail", Scalar(Str), Optional),
        ]),
        ("queued", Scalar(Int), Required, []),
        ("app", Map(&Opaque), Required, []),
    ]
}

shapewire::declare_cells! {
    POOL_SHARED = [("fleet_size", Scalar(Int), Required, [])]
}

struct Pool;
impl Shape<Beat1> for Pool {
    const NAME: &'static str = "pool";
    const TRACKING: Tracking = Tracking::Lazy;
    const CELLS: Cells = POOL_CELLS;
    const SHARED: Cells = POOL_SHARED;
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

/// A shape that declares NOTHING at the shape level — the explicit `NONE`, which
/// publishes as `[]` and is read back as a statement.
struct Flat;
impl Shape<Beat1> for Flat {
    const NAME: &'static str = "flat";
    const TRACKING: Tracking = Tracking::Lazy;
    const CELLS: Cells = POOL_CELLS;
    const SHARED: Cells = Cells::NONE;
    type Producer = ();
    type Root = Keys;
    fn root(&self) -> Keys {
        Keys::default()
    }
    fn hydrates(&mut self, _: &(), _: &mut Keys, _: Timestamp) {}
    fn folds(&mut self, _: &Stamped<Beat1>, _: &mut Tx<'_, Beat1Keys>) {}
}
type Beat1Keys = Keys;

fn a_hub() -> Arc<Hub<Beat1>> {
    let hub = Arc::new(Hub::<Beat1>::new());
    hub.register(Pool, (), 64);
    hub.register(Flat, (), 64);
    hub
}

fn names(infos: &[ShapeInfo], shape: &str) -> (Vec<String>, Vec<String>) {
    let i = infos
        .iter()
        .find(|i| i.name == shape)
        .unwrap_or_else(|| panic!("`{shape}` is not on shapes.list"));
    (
        i.cells.iter().map(|c| c.name.clone()).collect(),
        i.shared_cells.iter().map(|c| c.name.clone()).collect(),
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. The declaration is what `shapes.list` publishes — one read, no copy
// ─────────────────────────────────────────────────────────────────────────────

/// **`shapes.list` CARRIES THE MANIFEST, VERBATIM, FOR EVERY REGISTERED SHAPE.**
///
/// Asserted against the shape's own `CELLS`/`SHARED` through `publish()` on
/// one side and the hub's `list()` on the other — so the thing a consumer
/// reads is provably the declaration and not a transcription. The `[]` of
/// `Flat::SHARED` is asserted too: an explicit empty manifest publishes as an
/// explicit empty list, never skipped.
///
/// **THE MUTATION.** In `ShapeInfo::declared_by`, delete the line
/// `info.cells = S::CELLS.publish();`.
///
/// **THE RED:** `the manifest on shapes.list is the declaration: left:
/// ([], ["fleet_size"]), right: (["status", "queued", "app"], ["fleet_size"])`.
#[tokio::test]
async fn shapes_list_publishes_the_manifest_the_shape_declared() {
    let hub = a_hub();
    let list = hub.list();
    assert_eq!(
        names(&list, "pool"),
        (
            Pool::CELLS.names().iter().map(|s| s.to_string()).collect(),
            Pool::SHARED.names().iter().map(|s| s.to_string()).collect(),
        ),
        "the manifest on shapes.list is the declaration"
    );
    assert_eq!(
        names(&list, "flat"),
        (vec!["status".into(), "queued".into(), "app".into()], vec![]),
        "an explicit `Cells::NONE` publishes as an explicit `[]`"
    );
    // And not only the names: the words, the fields, the dtype spellings.
    let pool = list.iter().find(|i| i.name == "pool").unwrap();
    assert_eq!(
        pool.cells,
        Pool::CELLS.publish(),
        "cell by cell, field by field"
    );
    assert_eq!(pool.cells[0].fields[0].words, vec!["active", "idle"]);
    assert_eq!(
        pool.cells[2].dtype, "map<opaque>",
        "the twelfth dtype reaches the wire"
    );
    // The declaration is the same read `register` made — `declared_by` IS the
    // seed, so a tool reading it offline reads what the daemon serves.
    let offline = ShapeInfo::declared_by::<Beat1, Pool>();
    assert_eq!(
        (offline.cells, offline.shared_cells),
        (pool.cells.clone(), pool.shared_cells.clone())
    );
    assert_eq!(
        (offline.max, offline.contract),
        (pool.max.clone(), pool.contract.clone())
    );
}

/// **A MIRROR LEARNS THE MANIFEST OFF THE SAME `shapes.list`, AND A CELL IT DOES
/// NOT CONTAIN READS AS NEVER.** The end of the path `shapewire::cell` exists
/// for, driven from a real hub rather than a hand-built `ShapeInfo`.
///
/// **THE MUTATION.** In `Mirror::learn`, delete the `self.manifest = …` line.
///
/// **THE RED:** `after shapes.list, an undeclared cell is Undeclared: Unstated`.
#[tokio::test]
async fn a_mirror_fed_shapes_list_tells_never_from_not_yet() {
    let hub = a_hub();
    let mut m = Mirror::new("pool");
    assert_eq!(
        m.declared("pending"),
        Declaration::Unstated,
        "before: nothing can be said"
    );
    for info in hub.list() {
        m.learn(&info);
    }
    assert_eq!(
        m.declared("pending"),
        Declaration::Undeclared {
            declares: vec!["status".into(), "queued".into(), "app".into()]
        },
        "after shapes.list, an undeclared cell is Undeclared"
    );
    match m.declared("status") {
        Declaration::Declared(c) => {
            assert_eq!(
                c.field("state").map(|f| f.words.clone()),
                Some(vec!["active".into(), "idle".into()])
            );
            assert!(
                c.field("pending").is_none(),
                "and a field is declared or it is not"
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        m.shared_declared("fleet_size"),
        Declaration::Declared(_)
    ));
    assert!(matches!(
        m.shared_declared("status"),
        Declaration::Undeclared { .. }
    ));
    // The three-valued state a renderer draws now says why.
    match m.cell("sam", "pending") {
        CellState::NotHydrated { reason: Some(r) } => assert!(
            r.contains("`pending` is not a cell `pool` declares")
                && r.contains("status, queued, app"),
            "{r}"
        ),
        other => panic!("{other:?}"),
    }
    // A shape whose shape-level manifest is `[]` says so, which is a statement:
    // nothing at this level is declared, and the reason names it.
    let mut flat = Mirror::new("flat");
    for info in hub.list() {
        flat.learn(&info);
    }
    assert_eq!(
        flat.shared_declared("anything"),
        Declaration::Undeclared { declares: vec![] },
        "an explicit `NONE` is not `Unstated`"
    );
}
