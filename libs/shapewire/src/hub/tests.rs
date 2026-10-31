//! # Tests for `hub` — the runtime's own suite, carried in whole from four
//! substrate integration-test files and one inline module
//!
//! **32 assertions, every one of them PORTED rather than authored.** Substrate
//! kept them in five places: `mod wire_tests` at the foot of `hub.rs` (3), and
//! `tests/cell_manifest.rs` (2), `tests/referent.rs` (5), `tests/paging.rs` (7)
//! and `tests/version_link.rs` (15) in a crate-level `tests/` directory. This
//! crate has no `tests/` directory and every landed module here tests itself
//! through `<module>/tests.rs` — `fold`, `ledger` and `mirror` all do — so the
//! four files arrive here as four submodules instead, one per source file, and
//! `wire_tests` arrives as [`frame`].
//!
//! **Each source file is its own submodule because each brings its own
//! fixtures.** All four declare a `Beat1`, a `Keys`/`Nodes` root, a `settle()`
//! and an `exports()`/`drain()`, and three of them declare a `KEY_CELLS`. Those
//! are not the same fixtures — `paging`'s `Keys::row` emits `{"key": k}` where
//! `cell_manifest`'s emits a status struct — so merging them would have meant
//! choosing one and silently re-pointing the other file's assertions at it.
//! One submodule per source file keeps every test reading against the fixture
//! its author wrote for it.
//!
//! **What the conversion changed, and it is only addresses.** An integration
//! test reaches the crate from outside as `shapewire::hub::Hub`; from inside it
//! is `crate::hub::Hub`, and inside `hub` itself it is `Hub`. So every
//! `shapewire::` became `crate::`, and the `use shapewire::hub::{..}` line of
//! each file became `use crate::hub::*;`. **No assertion, no fixture, no
//! `#[should_panic]` string and no forcing comment was touched.** Three names
//! the integration files reached as `shapewire::hub::X` — `ConnFrames`,
//! `subscribe_page`, `dangling_refs` — are now reached unqualified through that
//! glob; they are public either way and no test's meaning moves with them.
//!
//! **The counts here come from cargo's own summary line and never from a grep.**
//! `grep -c '#\[test\]\|#\[tokio::test\]'` over substrate's `hub.rs`
//! returns 6 where the truth is 3: the three phantoms are the literal
//! `#[tokio::test]` sitting inside `//` prose at `hub.rs:3167`, `:3298` and
//! `:3407`, explaining why `block_in_place` panics on a current-thread runtime.

/// # THE CELL MANIFEST, END TO END: declared on the shape, published on
/// `shapes.list`, learned by the mirror, and then a cell that was never
/// declared reads as NEVER instead of NOT YET
///
/// `shapewire::cell` is the module doc; this file is the evidence that the three
/// doors it names are one path. The unit tests beside each half prove the half;
/// this proves that what `Hub::register` reads off `Shape::CELLS` is what a
/// consumer's `Mirror` is told, with nothing hand-copied in between — which is
/// the whole claim: the manifest a consumer routes against is the producer's
/// declaration and not somebody's transcription of it.
///
/// **Every test here was forced RED by a named mutation before it was believed**,
/// and the mutation is in the test's own docstring.
mod cell_manifest {
    use crate::hub::*;

    use std::collections::BTreeSet;
    use std::sync::Arc;

    use serde_json::{json, Value};

    use crate::cell::Cells;
    use crate::fold::{Outbox, Root};
    use crate::mirror::{Declaration, Mirror};
    use crate::operation::{
        DType::{Enum, Map, Opaque, Scalar, Struct},
        Presence::{Optional, Required},
        Scalar::{Bool, Int, Str},
    };
    use crate::wire::{CellState, ShapeInfo, Stamped, Timestamp};

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

    crate::declare_cells! {
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

    crate::declare_cells! {
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
    /// NOT CONTAIN READS AS NEVER.** The end of the path `crate::cell` exists
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

}

/// # The key-level nesting edge, proved — `Shape::REFERS`, `ShapeInfo::refs`, `Mirror::refers`
///
/// The operator's law, stated as a law and not a fix: *"children should be
/// nested shapes. You don't usually want all descendents visible. You want a
/// count on a single agent of how many descendents it has, and the ability to
/// traverse its children … it's a graph traversal on nodes in a flat list."*
///
/// The chassis already held the SHAPE-level edge (`Shape::PARENT`,
/// `ShapeInfo::parent`, `Hub::children`, `Epoch::parent_epoch`). What it did
/// not hold was the KEY-level one: a cell declared `Ref("node")` carried ids,
/// and WHICH shape those ids were keys of was prose. `REFERS` makes it a
/// declaration a consumer reads off `shapes.list`, and this file walks it.
///
/// Every red below was forced with its real text before it was believed; the
/// mutation is in each test's docstring.
mod referent {
    use crate::hub::*;

    use std::collections::{BTreeMap, BTreeSet};
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::Arc;

    use serde_json::{json, Value};

    use crate::cell::Cells;
    use crate::fold::{Outbox, Root};
    use crate::mirror::Mirror;
    use crate::operation::{
        DType::{List, Ref, Scalar},
        Presence::{Optional, Required},
        Scalar::Int,
    };
    use crate::wire::{CellState, Op, ShapeFrame, Stamped, Timestamp};

    #[derive(Clone)]
    struct Beat1;
    impl Kind for Beat1 {
        fn kind(&self) -> &'static str {
            "beat"
        }
    }

    /// One node of a small tree, held flat: a parent id, the ids of its children,
    /// and the COUNT of everything beneath it. Never the rows beneath it.
    #[derive(Clone)]
    struct Node {
        parent: Option<&'static str>,
        children: Vec<&'static str>,
        descendants: usize,
    }

    /// `a` → {`b`, `c`}; `b` → {`d`}. Four nodes, in one flat map.
    fn tree() -> BTreeMap<String, Node> {
        let mut m = BTreeMap::new();
        let mut put = |k: &str, parent, children: &[&'static str], descendants| {
            m.insert(
                k.to_string(),
                Node {
                    parent,
                    children: children.to_vec(),
                    descendants,
                },
            );
        };
        put("a", None, &["b", "c"], 3);
        put("b", Some("a"), &["d"], 1);
        put("c", Some("a"), &[], 0);
        put("d", Some("b"), &[], 0);
        m
    }

    #[derive(Default)]
    struct Nodes {
        nodes: BTreeMap<String, Node>,
        dirty: BTreeSet<String>,
    }

    impl Root for Nodes {
        fn keys(&self) -> Vec<String> {
            self.nodes.keys().cloned().collect()
        }
        fn row(&self, k: &str) -> Option<Value> {
            self.nodes.get(k).map(|n| {
                json!({
                    "parent": n.parent,
                    "children": n.children,
                    "descendants": n.descendants,
                })
            })
        }
        fn cells(&self, k: &str) -> Option<Value> {
            self.nodes.contains_key(k).then(|| {
                let known = CellState::Known {
                    complete: true,
                    folded_at: chrono::Utc::now(),
                    since_init: false,
                    covers_since: None,
                };
                json!({ "parent": known, "children": known, "descendants": known })
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

    crate::declare_cells! {
        /// The nesting discipline in one manifest: an edge up, the edges down as
        /// IDS, and a count — and never a child's row.
        NODE_CELLS = [
            ("parent", Ref("node"), Optional, []),
            ("children", List(&Ref("node")), Required, []),
            ("descendants", Scalar(Int), Required, []),
        ]
    }

    /// The well-formed case: `node` ids are keys of this very shape.
    struct Graph;
    impl Shape<Beat1> for Graph {
        const NAME: &'static str = "nodes";
        const TRACKING: Tracking = Tracking::Lazy;
        const CELLS: Cells = NODE_CELLS;
        const SHARED: Cells = Cells::NONE;
        const REFERS: &'static [(&'static str, &'static str)] = &[("node", "nodes")];
        type Producer = ();
        type Root = Nodes;
        fn root(&self) -> Nodes {
            Nodes::default()
        }
        fn hydrates(&mut self, _: &(), root: &mut Nodes, _: Timestamp) {
            root.nodes = tree();
            let _ = root.drain();
        }
        fn folds(&mut self, _: &Stamped<Beat1>, _: &mut Tx<'_, Nodes>) {}
    }

    /// Declares no referent, and carries the same cells: the control for
    /// "nothing changes for a shape that has not thought about this".
    struct Plain;
    impl Shape<Beat1> for Plain {
        const NAME: &'static str = "plain";
        const TRACKING: Tracking = Tracking::Lazy;
        const CELLS: Cells = NODE_CELLS;
        const SHARED: Cells = Cells::NONE;
        type Producer = ();
        type Root = Nodes;
        fn root(&self) -> Nodes {
            Nodes::default()
        }
        fn hydrates(&mut self, _: &(), root: &mut Nodes, _: Timestamp) {
            root.nodes = tree();
            let _ = root.drain();
        }
        fn folds(&mut self, _: &Stamped<Beat1>, _: &mut Tx<'_, Nodes>) {}
    }

    /// A referent for a `Ref` the manifest does not carry.
    struct AboutNothing;
    impl Shape<Beat1> for AboutNothing {
        const NAME: &'static str = "about-nothing";
        const TRACKING: Tracking = Tracking::Lazy;
        const CELLS: Cells = NODE_CELLS;
        const SHARED: Cells = Cells::NONE;
        const REFERS: &'static [(&'static str, &'static str)] = &[("thread", "nodes")];
        type Producer = ();
        type Root = Nodes;
        fn root(&self) -> Nodes {
            Nodes::default()
        }
        fn hydrates(&mut self, _: &(), _: &mut Nodes, _: Timestamp) {}
        fn folds(&mut self, _: &Stamped<Beat1>, _: &mut Tx<'_, Nodes>) {}
    }

    /// A referent naming a shape nobody registered.
    struct ToNowhere;
    impl Shape<Beat1> for ToNowhere {
        const NAME: &'static str = "to-nowhere";
        const TRACKING: Tracking = Tracking::Lazy;
        const CELLS: Cells = NODE_CELLS;
        const SHARED: Cells = Cells::NONE;
        const REFERS: &'static [(&'static str, &'static str)] = &[("node", "unregistered")];
        type Producer = ();
        type Root = Nodes;
        fn root(&self) -> Nodes {
            Nodes::default()
        }
        fn hydrates(&mut self, _: &(), _: &mut Nodes, _: Timestamp) {}
        fn folds(&mut self, _: &Stamped<Beat1>, _: &mut Tx<'_, Nodes>) {}
    }

    async fn settle() {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    fn exports(f: &mut ConnFrames) -> Vec<Arc<ShapeFrame>> {
        let mut all = Vec::new();
        while let Ok(x) = f.try_recv() {
            if x.wire.op == Op::Read {
                all.push(x);
            }
        }
        all
    }

    /// The manifest's own census of its `Ref` names — the positive sentinel for the
    /// register check, which looks things up in exactly this list.
    #[test]
    fn a_manifest_names_every_ref_it_carries_once() {
        assert_eq!(NODE_CELLS.ref_names(), vec!["node"]);
        assert!(Cells::NONE.ref_names().is_empty());
    }

    /// **A DECLARED REFERENT RIDES `shapes.list`, AND AN UNDECLARED ONE LEAVES NO
    /// TRACE.**
    ///
    /// **THE MUTATION.** In `ShapeInfo::declared_by`, drop the `REFERS` loop.
    ///
    /// Forced red:
    /// ```text
    /// `shapes.list` must publish the declared referent: {}
    /// ```
    #[tokio::test]
    async fn the_referent_rides_shapes_list_and_nothing_rides_for_a_shape_without_one() {
        let hub = Arc::new(Hub::<Beat1>::new());
        hub.register(Graph, (), 64);
        hub.register(Plain, (), 64);
        let list = hub.list();
        let graph = list.iter().find(|s| s.name == "nodes").unwrap();
        let plain = list.iter().find(|s| s.name == "plain").unwrap();
        assert_eq!(
            graph.refs.get("node").map(String::as_str),
            Some("nodes"),
            "`shapes.list` must publish the declared referent: {:?}",
            graph.refs
        );
        let on_wire = serde_json::to_value(graph).unwrap();
        assert_eq!(on_wire["refs"], json!({ "node": "nodes" }));
        // The control: a shape that declares none has no `refs` key AT ALL — the
        // wire is byte-identical for every shape written before the const existed.
        let plain_wire = serde_json::to_value(plain).unwrap();
        assert!(
            plain_wire.get("refs").is_none(),
            "a shape without REFERS must not grow a key: {plain_wire}"
        );
        assert!(plain.refs.is_empty());
    }

    /// **A CONSUMER WALKS THE EDGE: id → shape → subscribe → row → ids.** The
    /// traversal the operator described, done by a mirror that learned which shape
    /// a `node` id belongs to off `shapes.list` and nothing else.
    ///
    /// **THE MUTATION.** In `Mirror::learn`, drop `self.refs = info.refs.clone()`.
    ///
    /// Forced red:
    /// ```text
    /// the mirror must learn which shape `node` ids are keys of
    /// ```
    #[tokio::test]
    async fn a_consumer_traverses_children_by_the_declared_edge_and_the_count_matches() {
        let hub = Arc::new(Hub::<Beat1>::new());
        hub.register(Graph, (), 64);
        let mut frames = hub.connect(1);

        let mut m = Mirror::new("nodes");
        let info = hub.list().into_iter().find(|s| s.name == "nodes").unwrap();
        m.learn(&info);
        let target = m
            .refers("node")
            .expect("the mirror must learn which shape `node` ids are keys of")
            .to_string();
        assert_eq!(target, "nodes");
        assert_eq!(
            m.refers("thread"),
            None,
            "a name the producer did not map is `None` — an id in a space no shape serves"
        );

        // Walk: subscribe the root, read its children, subscribe each, repeat.
        // Depth-first over a flat pool; nothing is fetched that was not named.
        let mut visited: Vec<String> = Vec::new();
        let mut frontier = vec!["a".to_string()];
        while let Some(k) = frontier.pop() {
            hub.subscribe(&target, Some(&k), 1, None).unwrap();
            settle().await;
            let got = exports(&mut frames);
            let export = got
                .iter()
                .find(|f| f.wire.key.as_deref() == Some(k.as_str()))
                .unwrap_or_else(|| panic!("no export for `{k}`; the walk cannot continue"));
            m.accept(&serde_json::to_value(&export.wire).unwrap());
            visited.push(k.clone());
            let children: Vec<String> =
                serde_json::from_value(m.row(&k)["children"].clone()).unwrap();
            frontier.extend(children);
        }
        let mut seen = visited.clone();
        seen.sort();
        assert_eq!(seen, vec!["a", "b", "c", "d"], "the walk visits the whole tree once");
        // THE COUNT: the root says how many are beneath it, and the walk agrees.
        let descendants = m.row("a")["descendants"].as_u64().unwrap() as usize;
        assert_eq!(
            descendants,
            visited.len() - 1,
            "`descendants` on the root is the number of rows the walk found beneath it"
        );
        // And the walk held only what it asked for: one row per visited key, no
        // flattened roster arrived on anyone's behalf.
        assert_eq!(m.keys().len(), 4);
        // A re-init keeps the declaration, as it keeps `parent` and the manifest.
        m.reset();
        assert_eq!(m.refers("node"), Some("nodes"), "a referent survives a re-init");
    }

    /// **A REFERENT FOR A `Ref` THE MANIFEST DOES NOT CARRY IS REFUSED AT BOOT.**
    ///
    /// **THE MUTATION.** In `Hub::register`, make the first referent `assert!`
    /// unconditionally true.
    ///
    /// Forced red:
    /// ```text
    /// a referent for a reference nothing carries must refuse at register
    /// ```
    #[tokio::test]
    async fn a_referent_for_a_ref_nothing_carries_is_refused() {
        let hub = Arc::new(Hub::<Beat1>::new());
        hub.register(Graph, (), 64);
        let r = catch_unwind(AssertUnwindSafe(|| hub.register(AboutNothing, (), 64)));
        let msg = r
            .err()
            .and_then(|e| e.downcast::<String>().ok())
            .map(|s| *s)
            .expect("a referent for a reference nothing carries must refuse at register");
        assert!(
            msg.contains("no cell or field in its manifest is a `Ref(\"thread\")`")
                && msg.contains("[\"node\"]"),
            "the refusal names the ref and lists what the manifest carries: {msg}"
        );
    }

    /// Rows naming keys of a shape registered LATER — one half of a mutual edge,
    /// which is why existence is a graph question and not a register-time one.
    struct Early;
    impl Shape<Beat1> for Early {
        const NAME: &'static str = "early";
        const TRACKING: Tracking = Tracking::Lazy;
        const CELLS: Cells = NODE_CELLS;
        const SHARED: Cells = Cells::NONE;
        const REFERS: &'static [(&'static str, &'static str)] = &[("node", "nodes")];
        type Producer = ();
        type Root = Nodes;
        fn root(&self) -> Nodes {
            Nodes::default()
        }
        fn hydrates(&mut self, _: &(), _: &mut Nodes, _: Timestamp) {}
        fn folds(&mut self, _: &Stamped<Beat1>, _: &mut Tx<'_, Nodes>) {}
    }

    /// **A REFERENT NAMING A SHAPE NOBODY REGISTERED IS A DANGLING EDGE THE HUB
    /// NAMES — AND A MUTUAL EDGE RESOLVES WHEN ITS OTHER END REGISTERS.** The
    /// second half is the reason this is not a register-time refusal: `accounts`
    /// rows name threads and `threads` rows name accounts, and no order can see
    /// both ends first.
    ///
    /// **THE MUTATION.** In `Hub::dangling_refs`, drop the `!` on the `any`.
    ///
    /// Forced red:
    /// ```text
    /// a referent naming an unregistered shape is reported dangling
    /// ```
    #[tokio::test]
    async fn a_dangling_referent_is_named_and_a_mutual_edge_resolves_when_its_other_end_registers() {
        let hub = Arc::new(Hub::<Beat1>::new());
        hub.register(ToNowhere, (), 64);
        assert_eq!(
            hub.dangling_refs(),
            vec![("to-nowhere", "node".to_string(), "unregistered".to_string())],
            "a referent naming an unregistered shape is reported dangling"
        );
        // The forward edge: `early` names `nodes` before `nodes` exists …
        hub.register(Early, (), 64);
        assert!(
            hub.dangling_refs().iter().any(|(s, _, t)| *s == "early" && t == "nodes"),
            "a forward reference is dangling until its target registers"
        );
        // … and stops dangling the moment it does. Only the genuinely unresolvable
        // edge remains, which is the positive control that the list is not simply
        // emptied by a second registration.
        hub.register(Graph, (), 64);
        assert_eq!(
            hub.dangling_refs(),
            vec![("to-nowhere", "node".to_string(), "unregistered".to_string())],
            "a mutual edge resolves; a dangling one stays named"
        );
    }

}

/// # Paging, proved — `page`, `from`, `next`, and the three-level chain
///
/// The operator: *"we should make large list shapes page by built-in mechanism
/// so it's just a field you set … a global default of 100, a producer-set
/// default informed by the size of the thing being paginated, and then a
/// consumer can set their own page size since they know their context. null
/// being no pagination."*
///
/// What this file holds to:
///
/// * **Rule 2.5** — a page token is a KEY the consumer holds (`next`, sent
///   back verbatim as `from`), not a
///   position the producer remembers. The same ask yields the same keys, so
///   re-asking IS recovery and the producer keeps nothing per subscriber.
/// * **Rule 2.8** — a `page` above `rows_per_subscribe` is refused NAMING the
///   ceiling, never clamped; a short answer says `complete: false` and where it
///   stopped.
/// * **Rule 2.6** — a consumer that sends nothing gets the published default;
///   no frame of a complete answer grew a byte (`next` is absent, asserted).
///
/// Every red was forced with its real text; the mutation is in the docstring.
mod paging {
    use crate::hub::*;

    use std::collections::{BTreeSet, HashSet};
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::Arc;

    use serde_json::{json, Value};

    use crate::cell::Cells;
    use crate::fold::{Outbox, Root};
    use crate::mirror::{Ask, Mirror, OverCeiling};
    use crate::operation::{DType::Scalar, Presence::Required, Scalar::Str};
    use crate::wire::{
        CellState, Op, Paging, ShapeError, ShapeFrame, Stamped, Timestamp, DEFAULT_PAGE_ROWS,
    };

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

    crate::declare_cells! {
        KEY_CELLS = [("key", Scalar(Str), Required, [])]
    }

    /// Zero-padded so key order is numeric order and the walk is checkable.
    fn key(i: usize) -> String {
        format!("k{i:04}")
    }

    /// One shape per declaration under test; each hydrates `N` keys.
    macro_rules! pool {
        ($name:ident, $wire:literal, $n:expr, $page:expr, $max:expr) => {
            struct $name;
            impl Shape<Beat1> for $name {
                const NAME: &'static str = $wire;
                const TRACKING: Tracking = Tracking::Lazy;
                const CELLS: Cells = KEY_CELLS;
                const SHARED: Cells = Cells::NONE;
                const PAGE: Paging = $page;
                const MAX_ROWS_PER_SUBSCRIBE: usize = $max;
                type Producer = ();
                type Root = Keys;
                fn root(&self) -> Keys {
                    Keys::default()
                }
                fn hydrates(&mut self, _: &(), root: &mut Keys, _: Timestamp) {
                    for i in 0..$n {
                        root.keys.insert(key(i));
                    }
                    let _ = root.drain();
                }
                fn folds(&mut self, _: &Stamped<Beat1>, _: &mut Tx<'_, Keys>) {}
            }
        };
    }

    // 250 keys, nothing declared: the chassis default of 100 applies.
    pool!(Silent, "silent", 250, Paging::Default, 500);
    // 250 keys, the producer says 10: "informed by the size of the thing".
    pool!(Tens, "tens", 250, Paging::Rows(10), 500);
    // 250 keys, the producer says no pagination: the ceiling, as before.
    pool!(Whole, "whole", 250, Paging::Whole, 500);
    // 40 keys, a ceiling BELOW the chassis default: the default is the ceiling.
    pool!(Small, "small", 40, Paging::Default, 30);
    // 7 keys and nothing declared: smaller than any page — the pre-paging case.
    pool!(Tiny, "tiny", 7, Paging::Default, 500);
    // Two declarations that contradict the shape's own contract.
    pool!(ZeroPage, "zero-page", 7, Paging::Rows(0), 500);
    pool!(OverPage, "over-page", 7, Paging::Rows(600), 500);

    fn a_hub() -> Arc<Hub<Beat1>> {
        let hub = Arc::new(Hub::<Beat1>::new());
        hub.register(Silent, (), 64);
        hub.register(Tens, (), 64);
        hub.register(Whole, (), 64);
        hub.register(Small, (), 64);
        hub.register(Tiny, (), 64);
        hub
    }

    async fn settle() {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    fn exports(f: &mut ConnFrames) -> Vec<Arc<ShapeFrame>> {
        let mut all = Vec::new();
        while let Ok(x) = f.try_recv() {
            if x.wire.op == Op::Read {
                all.push(x);
            }
        }
        all
    }

    /// Ask one page; return `(keys in order, complete, next)` off the frames that
    /// answered THIS subscribe id and no other.
    async fn page(
        hub: &Hub<Beat1>,
        frames: &mut ConnFrames,
        shape: &str,
        conn: u64,
        ask: Paging,
        from: Option<&str>,
    ) -> (Vec<String>, Option<bool>, Option<String>) {
        let id = hub.subscribe_page(shape, conn, ask, from).expect("inside the ceiling");
        settle().await;
        let got: Vec<_> = exports(frames)
            .into_iter()
            .filter(|f| f.wire.subscribes.contains(&id))
            .collect();
        let keys = got.iter().filter_map(|f| f.wire.key.clone()).collect();
        let last = got.last().expect("a page answers with at least one frame");
        assert!(
            got.iter().take(got.len() - 1).all(|f| f.wire.complete.is_none() && f.wire.next.is_none()),
            "only the LAST frame of a page says whether it is complete and where it stopped"
        );
        (keys, last.wire.complete, last.wire.next.clone())
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // The chain
    // ─────────────────────────────────────────────────────────────────────────────

    /// **THE CHAIN, RESOLVED** — pure, so every arm is one line.
    ///
    /// **THE MUTATION.** In `Paging::resolve`, make the `Default`/`Default` arm
    /// return `ceiling`.
    ///
    /// Forced red:
    /// ```text
    /// nothing declared, nothing asked: the chassis default
    /// ```
    #[test]
    fn the_chain_resolves_consumer_then_producer_then_chassis() {
        use Paging::*;
        assert_eq!(
            Paging::resolve(Default, Default, 500),
            DEFAULT_PAGE_ROWS,
            "nothing declared, nothing asked: the chassis default"
        );
        assert_eq!(Paging::resolve(Default, Rows(10), 500), 10, "the producer's default");
        assert_eq!(Paging::resolve(Default, Whole, 500), 500, "the producer said no pagination");
        assert_eq!(Paging::resolve(Rows(25), Rows(10), 500), 25, "the consumer wins over the producer");
        assert_eq!(Paging::resolve(Whole, Rows(10), 500), 500, "null is the ceiling by name");
        assert_eq!(
            Paging::resolve(Default, Default, 30),
            30,
            "the chassis default against a smaller ceiling is the ceiling — nobody's ask was cut"
        );
        // The wire spellings, including the two refusals.
        assert_eq!(Paging::parse(None), Some(Default));
        assert_eq!(Paging::parse(Some(&json!(null))), Some(Whole));
        assert_eq!(Paging::parse(Some(&json!(25))), Some(Rows(25)));
        assert_eq!(Paging::parse(Some(&json!(0))), None, "a page of nothing is not an ask");
        assert_eq!(Paging::parse(Some(&json!("lots"))), None);
    }

    /// **`shapes.list` PUBLISHES THE PRODUCER'S TWO LINKS, RESOLVED.**
    ///
    /// **THE MUTATION.** In `ShapeInfo::declared_by`, publish `max` as the
    /// default for every declaration.
    ///
    /// Forced red:
    /// ```text
    /// a silent shape publishes the chassis default
    /// ```
    #[tokio::test]
    async fn shapes_list_publishes_the_page_contract_resolved() {
        let hub = a_hub();
        let list = hub.list();
        let page = |n: &str| list.iter().find(|s| s.name == n).unwrap().page.unwrap();
        assert_eq!(page("silent").default, Some(100), "a silent shape publishes the chassis default");
        assert_eq!(page("silent").max, 500);
        assert_eq!(page("tens").default, Some(10));
        assert_eq!(page("whole").default, None, "`None` is *no pagination*, stated");
        assert_eq!(page("small").default, Some(30), "the default cannot exceed the ceiling");
        // And the mirror reads it before it asks.
        let mut m = Mirror::new("silent");
        assert_eq!(m.page(), None, "a mirror never handed a `ShapeInfo` knows nothing");
        m.learn(list.iter().find(|s| s.name == "silent").unwrap());
        assert_eq!(m.page().unwrap().default, Some(100));
        m.reset();
        assert_eq!(m.page().unwrap().max, 500, "a re-init keeps the contract");
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // The walk
    // ─────────────────────────────────────────────────────────────────────────────

    /// **A SILENT CONSUMER WALKS 250 KEYS IN THREE PAGES OF 100, BY `next`**, and
    /// the union is the pool with no overlap. This is the positive sentinel for
    /// every claim about `next`: a walk that could not fail would not visit 250.
    ///
    /// **THE MUTATION.** In `do_subscribe`, compute `next` as `rest[cap - 1]`
    /// (the last key exported, not the first not exported).
    ///
    /// Forced red — on the first page's `next`, before the walk even starts; the
    /// partition assertion would catch the same mutation one page later:
    /// ```text
    /// `next` is the first key NOT exported
    /// ```
    #[tokio::test]
    async fn a_silent_consumer_pages_the_pool_by_next_and_the_pages_partition_it() {
        let hub = a_hub();
        let mut frames = hub.connect(1);
        let mut seen: Vec<String> = Vec::new();
        let mut from: Option<String> = None;
        let mut pages = 0;
        loop {
            let (keys, complete, next) =
                page(&hub, &mut frames, "silent", 1, Paging::Default, from.as_deref()).await;
            pages += 1;
            if pages == 1 {
                assert_eq!(keys.len(), 100, "nothing asked: the published default of 100");
                assert_eq!(keys[0], key(0));
                assert_eq!(complete, Some(false));
                assert_eq!(next.as_deref(), Some(key(100).as_str()), "`next` is the first key NOT exported");
            }
            seen.extend(keys);
            match (complete, next) {
                (Some(true), None) => break,
                (Some(false), Some(n)) => from = Some(n),
                other => panic!("a page is complete with no next, or short with one: {other:?}"),
            }
            assert!(pages < 10, "the walk must terminate");
        }
        assert_eq!(pages, 3, "250 keys at 100 a page is three pages");
        let distinct: HashSet<&String> = seen.iter().collect();
        assert_eq!(distinct.len(), 250, "the pages partition the pool: no key twice, none missing");
        assert_eq!(seen, (0..250).map(key).collect::<Vec<_>>(), "and in key order");
        // Rule 2.5: the same ask yields the same keys. Re-asking page two IS
        // recovery, and the producer held nothing to make it so.
        let (again, c, n) = page(&hub, &mut frames, "silent", 1, Paging::Default, Some(&key(100))).await;
        assert_eq!(again, (100..200).map(key).collect::<Vec<_>>());
        assert_eq!((c, n.as_deref()), (Some(false), Some(key(200).as_str())));
    }

    /// **THE PRODUCER'S DEFAULT, THE CONSUMER'S OVERRIDE, AND `null`.**
    ///
    /// **THE MUTATION.** In `enqueue_subscribe`, resolve with `Paging::Default`
    /// in place of the consumer's `page`.
    ///
    /// Forced red:
    /// ```text
    /// the consumer's 25 wins over the producer's 10
    /// ```
    #[tokio::test]
    async fn the_producer_default_the_consumer_override_and_null() {
        let hub = a_hub();
        let mut frames = hub.connect(1);
        let (keys, complete, next) = page(&hub, &mut frames, "tens", 1, Paging::Default, None).await;
        assert_eq!(keys.len(), 10, "the producer declared 10");
        assert_eq!((complete, next.as_deref()), (Some(false), Some(key(10).as_str())));
        let (keys, _, _) = page(&hub, &mut frames, "tens", 1, Paging::Rows(25), None).await;
        assert_eq!(keys.len(), 25, "the consumer's 25 wins over the producer's 10");
        let (keys, complete, next) = page(&hub, &mut frames, "tens", 1, Paging::Whole, None).await;
        assert_eq!(keys.len(), 250, "`null` is the ceiling by name: the whole pool, under 500");
        assert_eq!((complete, next), (Some(true), None));
        // A producer that declared `Whole` serves the ceiling to a silent consumer —
        // the rule-2.6 lever for a shape whose faces already read more than 100.
        let (keys, complete, _) = page(&hub, &mut frames, "whole", 1, Paging::Default, None).await;
        assert_eq!((keys.len(), complete), (250, Some(true)));
        // A ceiling below the chassis default: the default IS the ceiling.
        let (keys, complete, next) = page(&hub, &mut frames, "small", 1, Paging::Default, None).await;
        assert_eq!((keys.len(), complete, next.as_deref()), (30, Some(false), Some(key(30).as_str())));
    }

    /// **A PAGE ABOVE THE CEILING IS REFUSED NAMING THE CEILING, ON THE REQUEST,
    /// WITH NOTHING QUEUED** — rule 2.8, and A4: never clamped. The ceiling itself
    /// is served.
    ///
    /// **THE MUTATION.** In `enqueue_subscribe`, change `n > rows_per_subscribe`
    /// to `n >= rows_per_subscribe`.
    ///
    /// Forced red:
    /// ```text
    /// a page equal to the ceiling is inside it and is served
    /// ```
    #[tokio::test]
    async fn a_page_above_the_ceiling_is_refused_naming_it_and_the_ceiling_is_served() {
        let hub = a_hub();
        let mut frames = hub.connect(1);
        let err = hub.subscribe_page("silent", 1, Paging::Rows(501), None).unwrap_err();
        match err {
            ShapeError::AboveMax { shape, param, asked, max } => {
                assert_eq!((shape.as_str(), param.as_str(), asked.as_str(), max.as_str()), ("silent", "page", "501", "500"));
            }
            other => panic!("refused as AboveMax naming the ceiling, not {other:?}"),
        }
        settle().await;
        assert!(exports(&mut frames).is_empty(), "a refused ask queues nothing");
        hub.subscribe_page("silent", 1, Paging::Rows(500), None)
            .expect("a page equal to the ceiling is inside it and is served");
        // And the consuming half refuses the same ask in-process, before it is sent.
        let mut m = Mirror::new("silent");
        m.learn(hub.list().iter().find(|s| s.name == "silent").unwrap());
        let ask = Ask { rows: None, keys_per_frame: None, future_at_secs: None, armed_timers: None, page: Some(501) };
        assert_eq!(m.check(&ask), Err(OverCeiling::Page { asked: 501, ceiling: 500 }));
        assert_eq!(m.check(&Ask { page: Some(500), ..ask }), Ok(()));
    }

    /// **A POOL SMALLER THAN ANY PAGE IS THE PRE-PAGING CASE, BYTE FOR BYTE.** The
    /// answer is complete and no frame carries `next` — asserted on the JSON, with
    /// the short-page frame as the positive control that the key CAN appear.
    ///
    /// **THE MUTATION.** In `do_subscribe`, compute `complete` as `true`
    /// unconditionally. The claim (no `next` on a complete answer) cannot go red
    /// on its own — a 7-key pool has no key to name — so the red lands on the
    /// control, which is why the control is there:
    /// ```text
    /// the control: a short page's last frame DOES carry `next` on the wire
    /// ```
    #[tokio::test]
    async fn a_pool_under_the_page_is_complete_and_grows_no_byte() {
        let hub = a_hub();
        let mut frames = hub.connect(1);
        let id = hub.subscribe("tiny", None, 1, None).unwrap();
        settle().await;
        let got: Vec<_> = exports(&mut frames).into_iter().filter(|f| f.wire.subscribes.contains(&id)).collect();
        assert_eq!(got.len(), 7);
        assert_eq!(got.last().unwrap().wire.complete, Some(true));
        for f in &got {
            let v = serde_json::to_value(&f.wire).unwrap();
            assert!(v.get("next").is_none(), "no frame of a complete answer carries `next`: {v}");
        }
        // The control.
        let id = hub.subscribe("silent", None, 1, None).unwrap();
        settle().await;
        let got: Vec<_> = exports(&mut frames).into_iter().filter(|f| f.wire.subscribes.contains(&id)).collect();
        let v = serde_json::to_value(&got.last().unwrap().wire).unwrap();
        assert_eq!(
            v.get("next"),
            Some(&json!(key(100))),
            "the control: a short page's last frame DOES carry `next` on the wire"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // The declaration
    // ─────────────────────────────────────────────────────────────────────────────

    /// **A PRODUCER DEFAULT OF ZERO, OR ABOVE THE CEILING, IS REFUSED AT BOOT.**
    ///
    /// **THE MUTATION.** In `Hub::register`, make the page `assert!`s
    /// unconditionally true.
    ///
    /// Forced red:
    /// ```text
    /// a PAGE of Rows(0) must refuse at register
    /// ```
    #[tokio::test]
    async fn a_page_declaration_that_contradicts_the_contract_is_refused() {
        let hub = Arc::new(Hub::<Beat1>::new());
        let zero = catch_unwind(AssertUnwindSafe(|| hub.register(ZeroPage, (), 64)))
            .err()
            .and_then(|e| e.downcast::<String>().ok())
            .expect("a PAGE of Rows(0) must refuse at register");
        assert!(zero.contains("declares PAGE `Rows(0)`"), "{zero}");
        let over = catch_unwind(AssertUnwindSafe(|| hub.register(OverPage, (), 64)))
            .err()
            .and_then(|e| e.downcast::<String>().ok())
            .expect("a PAGE above the ceiling must refuse at register");
        assert!(over.contains("`Rows(600)` above its own MAX_ROWS_PER_SUBSCRIBE of 500"), "{over}");
        // The positive control: the well-formed declarations register.
        hub.register(Tens, (), 64);
        assert!(hub.list().iter().any(|s| s.name == "tens"));
    }

}

/// # RULE 2.6, PROVED: VERSION MISMATCH IS REFUSED AT LINK TIME, LOUDLY
///
/// `docs/shapes.md` 2.6 said *"**Exists.** Nothing. The shape has no version and
/// the subscribe carries none."* This file is the producer half's evidence, and it
/// lives in `shapewire`'s own tests rather than a producer's because the rule
/// is the chassis's: a second producer must inherit it by linking the crate, not
/// by re-deriving it.
///
/// ## What every test here is careful about, and why
///
/// **Each test was forced RED by a named mutation before it was believed**, and
/// the mutation is in the test's own docstring. The docstring also says what the
/// red LOOKED like where the shape of the failure is the interesting part, because
/// a mutation that produces the wrong red is a test measuring the wrong thing.
///
/// **Nothing here asserts "it did not link".** A negative assertion goes silent
/// when its subject is renamed — delete `ShapeError::VersionRefused` and
/// `assert!(result.is_err())` keeps passing on whatever error arrives next. So the
/// assertions are on the VALUE: the exact variant, the exact numbers it carries,
/// the exact refusal code, and the exact text. The one place a negative claim is
/// unavoidable (*nothing was queued*) is paired with a positive one about the
/// same connection, so the test cannot pass by never establishing a subscription
/// in the first place.
///
/// **A test that needs a live export PANICS rather than skips.** `expect_export`
/// has no timeout-and-shrug path: a missing frame is a failure, never a test that
/// quietly asserted nothing. That is the lesson rule 2.4's own section records —
/// *"the tests never subscribed, so every one took the not-tracking early return
/// … a test that cannot fail is worse than no test, because it is reported as
/// evidence."*
mod version_link {
    use crate::hub::*;

    use std::collections::BTreeSet;
    use std::sync::Arc;

    use serde_json::{json, Value};

    use crate::cell::Cells;
    use crate::envelope::{codes, Coded, Refusal};
    use crate::fold::{Outbox, Root};
    use crate::operation::{DType::Scalar, Presence::Required, Scalar::Str};
    use crate::ledger::ConsumerIdentity;
    use crate::mirror::{Mirror, VersionMismatch};
    use crate::wire::{
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

    // The one cell `Keys::cells` emits, declared — see `crate::cell`. Every
    // shape here carries it, and none carries a shape-level cell.
    crate::declare_cells! {
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
            crate::wire::Paging::Default,
            None,
        )
    }

    /// Every frame buffered for this connection.
    fn drain(f: &mut ConnFrames) -> Vec<Arc<ShapeFrame>> {
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
    fn expect_export(frames: &mut ConnFrames, why: &str) -> Arc<ShapeFrame> {
        let got = drain(frames);
        got.into_iter()
            .find(|f| f.wire.op == crate::wire::Op::Read)
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
            crate::wire::Paging::Default,
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
            crate::wire::Paging::Default,
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

}

/// # `ShapeInfo` on the wire — the three assertions substrate kept inline
///
/// Carried from `mod wire_tests` at the foot of substrate's `hub.rs`, renamed
/// `frame` because the frame types themselves left this module for
/// [`crate::wire`] and `wire_tests` is now a name about where the subject used
/// to live. **All three are plain `#[test]`** — they build a `ShapeInfo` and
/// serialise it, so nothing here needs a runtime.
///
/// **This is the trio the census's grep got wrong.** `grep -c` over substrate's
/// `hub.rs` answers 6 for three tests, because the literal `#[tokio::test]`
/// appears in three `//` prose comments further up the file. The number below
/// is cargo's.
mod frame {
    use crate::hub::*;
    use crate::wire::{ShapeContract, ShapeInfo, ShapeMax};

    /// The ceilings a shape that overrides nothing declares.
    fn a_max() -> ShapeMax {
        ShapeMax {
            rows_per_subscribe: 500,
            keys_per_frame: 500,
            future_at_secs: MAX_FUTURE_AT.num_seconds(),
            armed_timers_per_conn: MAX_ARMED_TIMERS_PER_CONN,
            retained_items: 0,
            beat_interval_ms: 0,
            open_keys_per_conn: None,
        }
    }

    /// Rule 2.6's contract as `Shape`'s defaults produce it: version 1, serving
    /// only itself. The default is exact-match-only, so this is what a shape whose
    /// author has not thought about versions publishes.
    fn a_contract() -> ShapeContract {
        ShapeContract {
            version: 1,
            serves: vec![1],
        }
    }

    /// **The board's wire must not have changed.**
    ///
    /// `unplaced_status` was a named field on `ShapeInfo` and is now an entry in
    /// a shape-named counter map. `#[serde(flatten)]` is what keeps it at the
    /// TOP LEVEL of `shapes.list`, where `threads_tui/render.py:168` reads it.
    /// If this test goes red the board silently loses the number — which is
    /// exactly the failure mode the counter had before it was wired at all.
    #[test]
    fn a_shape_named_counter_lands_at_the_top_level_of_shapes_list() {
        let mut info = ShapeInfo::fresh("accounts", Tracking::Lazy, a_max(), a_contract());
        info.counters.insert("unplaced_status".into(), 3);
        let v = serde_json::to_value(&info).unwrap();

        assert_eq!(
            v.get("unplaced_status").and_then(|x| x.as_u64()),
            Some(3),
            "flattened to the top level, where the board reads it"
        );
        assert!(
            v.get("counters").is_none(),
            "the map itself must NOT appear — that would be a second spelling"
        );
    }

    /// A shape that names no counter must not emit an empty object either.
    #[test]
    fn a_shape_with_no_counters_adds_no_keys() {
        let info = ShapeInfo::fresh("quiet", Tracking::Eager, a_max(), a_contract());
        let v = serde_json::to_value(&info).unwrap();
        assert!(v.get("counters").is_none());
        assert!(v.get("unplaced_status").is_none());
    }

    /// **A DECLARED counter is present at ZERO, before anything increments it.**
    ///
    /// This is the regression the first version of this module shipped, and the
    /// test above BLESSED it: a flattened empty map emits no key, so
    /// `unplaced_status` vanished from `shapes.list` until the first unplaced
    /// status arrived, having always been present as `0`. Caught on the LIVE
    /// daemon after promotion, by reading the real payload — not by this suite,
    /// which asserted the wrong thing confidently.
    #[test]
    fn a_declared_counter_is_present_at_zero_before_it_ever_increments() {
        // Seeded as `declared_by` seeds a declared counter: present, at zero.
        let mut info = ShapeInfo::fresh("accounts", Tracking::Lazy, a_max(), a_contract());
        info.counters.insert("unplaced_status".into(), 0);
        let v = serde_json::to_value(&info).unwrap();
        assert_eq!(
            v.get("unplaced_status").and_then(|x| x.as_u64()),
            Some(0),
            "present, and zero, on a daemon where nothing has gone wrong: {v}"
        );
        assert!(v.get("counters").is_none(), "still flattened, not nested");
    }
}

/// # The two promises — **AUTHORED, NOT PORTED, because nothing in the 32 guards them**
///
/// The module doc's first paragraph says what this file exists for: the chassis
/// *stamps each input exactly once* and *hands them to folds in strict arrival
/// order*. **Neither promise is guarded by any of the 32 tests above, and that
/// is measured rather than suspected.**
///
/// * `grep -cE '\.emit(_at|_resolving)?\('` over all four substrate source
///   files, over substrate's `mirror_oracle.rs` and `resubscribe_identity.rs`,
///   and over `mod wire_tests`, is **0** for every one of them. The same
///   predicate finds the call in a one-line control file and finds 12 files
///   under `lib/threads/tests/` that do drive inputs — so it is a search shown
///   capable of finding something.
/// * **Every `fn folds` body in all 32 is `{}`.** Twelve of them, all empty.
///   A fold that does nothing cannot report what it was handed, so no
///   assertion above could tell one stamp from two, or arrival order from any
///   other order.
///
/// The guards for both promises live in **`lib/threads/tests/`**, which the
/// dispatch judged out of scope and which does not travel with this crate. So
/// shapewire's own suite could be fully green against a chassis that stamped
/// twice, stamped never, re-stamped per shape, or folded LIFO. These three
/// tests close that, and each is forced in BOTH directions — see the report.
///
/// **The instrument is a fold that WRITES DOWN WHAT IT SAW.** `Recorder::folds`
/// pushes `(input.at, sequence)` into an `Arc<Mutex<Vec<_>>>` the test holds.
/// The writer is the spawned shape task inside the hub and the reader is the
/// test, so this is not a fixture witnessing its own write: nothing the test
/// does puts an entry in that vector.
mod promises {
    use crate::hub::*;

    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};

    use serde_json::{json, Value};

    use crate::cell::Cells;
    use crate::fold::{Outbox, Root};
    use crate::operation::{DType::Scalar, Presence::Required, Scalar::Int};
    use crate::wire::{CellState, Op, ShapeFrame, Stamped, Timestamp};

    /// One producer input, carrying a SEQUENCE NUMBER the test orders by.
    ///
    /// Deliberately not ordered by the stamp: every input in the arrival-order
    /// test carries the SAME stamp, so an implementation that sorted, bucketed
    /// or raced has nothing to sort by and the sequence is the only witness.
    #[derive(Clone, Debug)]
    struct Tick(u64);
    impl Kind for Tick {
        fn kind(&self) -> &'static str {
            "tick"
        }
    }

    /// What the fold saw, in the order it saw it: `(stamp, sequence)`.
    type Seen = Arc<Mutex<Vec<(Timestamp, u64)>>>;

    /// One key, one counter: how many inputs this shape has folded.
    #[derive(Default)]
    struct Counted {
        folded: u64,
        dirty: BTreeSet<String>,
    }

    impl Root for Counted {
        fn keys(&self) -> Vec<String> {
            vec!["n".to_string()]
        }
        fn row(&self, k: &str) -> Option<Value> {
            (k == "n").then(|| json!({ "folded": self.folded }))
        }
        fn cells(&self, k: &str) -> Option<Value> {
            (k == "n").then(|| {
                json!({ "folded": CellState::Known {
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

    crate::declare_cells! {
        COUNTED_CELLS = [("folded", Scalar(Int), Required, [])]
    }

    /// **A shape whose `folds` RECORDS WHAT IT WAS HANDED**, which is the one
    /// thing none of the 32 ported fixtures does.
    macro_rules! recorder {
        ($t:ident, $wire:literal) => {
            struct $t(Seen);
            impl Shape<Tick> for $t {
                const NAME: &'static str = $wire;
                const TRACKING: Tracking = Tracking::Lazy;
                const CELLS: Cells = COUNTED_CELLS;
                const SHARED: Cells = Cells::NONE;
                type Producer = ();
                type Root = Counted;
                fn root(&self) -> Counted {
                    Counted::default()
                }
                fn hydrates(&mut self, _: &(), root: &mut Counted, _: Timestamp) {
                    // A hydrate publishes nothing; the export is the publication.
                    let _ = root.drain();
                }
                fn folds(&mut self, input: &Stamped<Tick>, tx: &mut Tx<'_, Counted>) {
                    self.0.lock().unwrap().push((input.at, input.value.0));
                    tx.folded += 1;
                    tx.dirty.insert("n".to_string());
                }
            }
        };
    }

    // TWO shapes on ONE hub, because "stamped once" is a claim about a fan-out
    // and a single-shape hub cannot exercise it: with one FIFO, "one stamp" and
    // "one stamp per shape" are the same sentence.
    recorder!(One, "one");
    recorder!(Two, "two");

    async fn settle() {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    fn exports(f: &mut ConnFrames) -> Vec<Arc<ShapeFrame>> {
        let mut all = Vec::new();
        while let Ok(x) = f.try_recv() {
            if x.wire.op == Op::Read {
                all.push(x);
            }
        }
        all
    }

    /// Both shapes registered, both tracking, both folds empty — **and that is
    /// asserted, not assumed.**
    ///
    /// A `Tracking::Lazy` shape's FIFO accepts no input until somebody
    /// subscribes ([`Fifo::push_input`] returns early), so every assertion in
    /// this module would hold just as well on a hub that dropped every emit on
    /// the floor. The sentinel is checked before any emit, so that case is a
    /// failure here rather than a green below.
    async fn tracking() -> (Arc<Hub<Tick>>, Seen, Seen, ConnFrames) {
        tracking_on(Hub::<Tick>::new()).await
    }

    /// [`tracking`] on a hub the caller built — for the one test that installs
    /// its own clock ([`Hub::with_clock`]).
    async fn tracking_on(hub: Hub<Tick>) -> (Arc<Hub<Tick>>, Seen, Seen, ConnFrames) {
        let hub = Arc::new(hub);
        let one: Seen = Arc::new(Mutex::new(Vec::new()));
        let two: Seen = Arc::new(Mutex::new(Vec::new()));
        hub.register(One(one.clone()), (), 1024);
        hub.register(Two(two.clone()), (), 1024);
        let frames = hub.connect(1);
        hub.subscribe("one", None, 1, None).unwrap();
        hub.subscribe("two", None, 1, None).unwrap();
        settle().await;
        assert!(
            hub.tracked("one") && hub.tracked("two"),
            "THE SENTINEL: both shapes must be tracking, or an untracked FIFO \
             drops every emit and nothing below is tested"
        );
        assert_eq!(
            (one.lock().unwrap().len(), two.lock().unwrap().len()),
            (0, 0),
            "and nothing has been folded yet, so every count below is caused by \
             an emit in the test body"
        );
        (hub, one, two, frames)
    }

    /// **ONE EMIT IS ONE STAMP, APPLIED ONCE, AND EVERY SHAPE SEES THAT SAME
    /// ONE.**
    ///
    /// Four separate claims, because each can break while the others hold:
    ///
    /// 1. one emit is folded exactly ONCE per shape — not twice, not never;
    /// 2. the stamp the fold sees is the one the EMIT carried, not a clock read
    ///    further down (asserted against a stamp seven seconds in the past, so a
    ///    `Utc::now()` anywhere downstream cannot coincide with it);
    /// 3. **both shapes see the IDENTICAL stamp** — a per-shape re-stamp would
    ///    satisfy 1 and 2 and give two different values, and this is the only
    ///    assertion that catches it;
    /// 4. [`Hub::emit`] reads the clock ONCE for the whole fan-out, and reads it
    ///    inside the call (bracketed by two readings the test takes).
    #[tokio::test]
    async fn an_input_is_stamped_exactly_once_and_every_shape_sees_that_one_stamp() {
        let (hub, one, two, _f) = tracking().await;

        let t0 = chrono::Utc::now() - chrono::Duration::seconds(7);
        hub.emit_at(t0, Tick(1));
        settle().await;
        let a = one.lock().unwrap().clone();
        let b = two.lock().unwrap().clone();
        assert_eq!(
            a.len(),
            1,
            "one emit is folded ONCE by `one` — not twice, not never: {a:?}"
        );
        assert_eq!(b.len(), 1, "and once by `two`: {b:?}");
        assert_eq!(
            a[0].0, t0,
            "the stamp a fold sees is the one the EMIT carried, not a later clock \
             read: {:?} vs {t0:?}",
            a[0].0
        );
        assert_eq!(
            a[0].0, b[0].0,
            "ONE stamp, shared by every shape of the fan-out — a per-shape clock \
             read gives two: {:?} vs {:?}",
            a[0].0, b[0].0
        );

        // Ten more, in case "exactly once" was really "the first one only".
        for i in 2..12 {
            hub.emit_at(t0, Tick(i));
        }
        settle().await;
        assert_eq!(
            one.lock().unwrap().len(),
            11,
            "eleven emits, eleven folds — the count tracks the emits and is not a \
             constant"
        );
        assert_eq!(two.lock().unwrap().len(), 11);

        // `emit` takes the stamp itself. Once, and inside the call.
        let before = chrono::Utc::now();
        hub.emit(Tick(99));
        let after = chrono::Utc::now();
        settle().await;
        let (at_a, at_b) = (one.lock().unwrap()[11].0, two.lock().unwrap()[11].0);
        assert_eq!(
            at_a, at_b,
            "`emit` reads the clock ONCE for the whole fan-out: {at_a} vs {at_b}"
        );
        assert!(
            at_a >= before && at_a <= after,
            "and reads it inside the call: {before} <= {at_a} <= {after}"
        );
    }

    /// **FIFTY INPUTS ON ONE IDENTICAL STAMP ARE FOLDED IN ARRIVAL ORDER.**
    ///
    /// Every input carries the SAME `at`, which is what makes this a test of
    /// ARRIVAL order rather than of stamp order: there is nothing to sort by, so
    /// an implementation that reversed, bucketed, or let two pops race has no
    /// way to reproduce the sequence by accident.
    ///
    /// Asserted on BOTH shapes, because one queue in order says nothing about
    /// the other, and the discipline is the FIFO's rather than the shape's.
    ///
    /// Opens with the sentinel doctrine 9 asks for: the expected sequence and
    /// its own reverse are asserted DIFFERENT, so the `assert_eq!` below is a
    /// claim about an order and not a restatement that `==` is reflexive.
    #[tokio::test]
    async fn inputs_are_folded_in_strict_arrival_order_on_one_identical_stamp() {
        let expected: Vec<u64> = (0..50).collect();
        let mut reversed = expected.clone();
        reversed.reverse();
        assert_ne!(
            reversed, expected,
            "THE SENTINEL: 50 in order and 50 reversed are different sequences, so \
             the comparison below is between two different things"
        );

        let (hub, one, two, _f) = tracking().await;
        // Seven seconds in the past, so a re-stamp anywhere downstream cannot
        // coincide with it at any clock resolution (R3.S2.10, HOLDS R8).
        let t0 = chrono::Utc::now() - chrono::Duration::seconds(7);
        for i in 0..50 {
            hub.emit_at(t0, Tick(i));
        }
        settle().await;

        let a: Vec<u64> = one.lock().unwrap().iter().map(|(_, n)| *n).collect();
        let b: Vec<u64> = two.lock().unwrap().iter().map(|(_, n)| *n).collect();
        assert_eq!(a, expected, "`one` folded in strict arrival order");
        assert_eq!(
            b, expected,
            "and so did `two` — the discipline is the FIFO's, not the shape's"
        );
        assert!(
            one.lock().unwrap().iter().all(|(at, _)| *at == t0),
            "and all fifty carry the one stamp they were emitted with, so order \
             was not recoverable from the stamps"
        );
        // **BOTH shapes, not only `one`** (R3.S2.10, HOLDS R8). Reading `one`
        // alone left a re-stamp of every shape BUT THE FIRST caught by a single
        // assertion in the whole suite (the r21 inspector's M6).
        assert!(
            two.lock().unwrap().iter().all(|(at, _)| *at == t0),
            "and `two` carries that same stamp on all fifty — a shape re-stamped \
             after the first is caught here too"
        );
    }

    /// **`emit` READS ITS CLOCK EXACTLY ONCE PER FAN-OUT — COUNTED, NOT TIMED.**
    ///
    /// R3.S2.10 (HOLDS R8, Q-22-5). The real-clock assertion above
    /// (`an_input_is_stamped_exactly_once…`, "reads the clock ONCE") compares two
    /// `Utc::now()` reads and was only as strong as the microseconds between
    /// them: the r21 inspector measured a per-shape re-stamp inside `emit` (M5)
    /// red 8/8 with the reads 7–15 µs apart in a debug build, and said so. This
    /// guard needs no separation at all. The hub's clock is a counter
    /// ([`Hub::with_clock`]) that returns a DIFFERENT instant in 2001 on every
    /// call, so:
    ///
    /// * the read COUNT per emit is asserted to be 1 — a second read is a 2,
    ///   whatever the hardware;
    /// * the stamp every shape sees is asserted to be the instant that one read
    ///   returned — a stamp from the real clock (a `Utc::now()` that bypassed
    ///   the hub's clock) is 25 years off, and a per-shape read is a different
    ///   2001 instant on the second shape.
    ///
    /// Opens with doctrine 9's sentinel: two consecutive reads of the fake clock
    /// are asserted DIFFERENT, so "both shapes saw one stamp" below is a claim
    /// a second read could have broken.
    #[tokio::test]
    async fn emit_reads_its_clock_exactly_once_per_fan_out_counted_not_timed() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static READS: AtomicU64 = AtomicU64::new(0);
        fn base() -> Timestamp {
            chrono::DateTime::parse_from_rfc3339("2001-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc)
        }
        fn counted() -> Timestamp {
            let n = READS.fetch_add(1, Ordering::SeqCst);
            base() + chrono::Duration::seconds(n as i64)
        }
        let (r0, r1) = (counted(), counted());
        assert_ne!(r0, r1, "THE SENTINEL: two reads of this clock are two instants");
        assert!(r0 < chrono::Utc::now() - chrono::Duration::days(365 * 20));

        let (hub, one, two, _f) = tracking_on(Hub::<Tick>::new().with_clock(counted)).await;
        let before = READS.load(Ordering::SeqCst);
        hub.emit(Tick(1));
        let after = READS.load(Ordering::SeqCst);
        settle().await;
        assert_eq!(
            after - before,
            1,
            "ONE emit reads the hub's clock ONCE, for every shape of the fan-out"
        );
        let want = base() + chrono::Duration::seconds(before as i64);
        let (a, b) = (one.lock().unwrap()[0].0, two.lock().unwrap()[0].0);
        assert_eq!(a, want, "`one` carries the instant that one read returned");
        assert_eq!(b, want, "and so does `two` — the same read, not its own");

        // Three more: three reads, three distinct stamps, each shared.
        for i in 2..5 {
            hub.emit(Tick(i));
        }
        settle().await;
        assert_eq!(READS.load(Ordering::SeqCst) - before, 4, "four emits, four reads");
        let sa: Vec<_> = one.lock().unwrap().iter().map(|(at, _)| *at).collect();
        let sb: Vec<_> = two.lock().unwrap().iter().map(|(at, _)| *at).collect();
        assert_eq!(sa, sb, "every emit's one stamp is shared by both shapes");
        let mut dedup = sa.clone();
        dedup.dedup();
        assert_eq!(dedup.len(), 4, "and the four emits carry four instants: {sa:?}");
    }

    /// **A CONTROL ITEM IS FOLDED WHERE IT ARRIVED — an export is placed among
    /// the events it must reflect, not raced against them.**
    ///
    /// This is the half of the arrival-order promise that the sequence test
    /// cannot see, and it is the reason [`Fifo`] is a `VecDeque` under a mutex
    /// instead of an `mpsc` with control on a side channel: subscribes share the
    /// inputs' queue.
    ///
    /// THREE inputs, then a subscribe, then SEVEN more, all pushed from one
    /// thread so the queue order is the program order. The export answering that
    /// subscribe must show **exactly 3** folded. `0` would mean the control item
    /// jumped the queue; `10` would mean it sank behind inputs that arrived
    /// after it. Both are the same defect read from the two ends, and both are
    /// forced.
    ///
    /// **3 AND 7, NOT 5 AND 5** (R3.S2.10, HOLDS R8, Q-22-5). It was five and
    /// five, which is SYMMETRIC: a LIFO queue pops `I9..I5, S, I4..I0` and the
    /// subscribe still sees five, so fold order rested on ONE test in the suite
    /// (the r21 inspector's M4). Asymmetric, LIFO places the subscribe after the
    /// seven and the export says 7 — a second, independent guard on order.
    #[tokio::test]
    async fn a_subscribe_sees_exactly_the_inputs_that_arrived_before_it() {
        let (hub, one, _two, _f) = tracking().await;
        let t0 = chrono::Utc::now();

        let mut late = hub.connect(2);
        for i in 0..3 {
            hub.emit_at(t0, Tick(i));
        }
        let id = hub.subscribe("one", None, 2, None).unwrap();
        for i in 3..10 {
            hub.emit_at(t0, Tick(i));
        }
        settle().await;

        let got: Vec<_> = exports(&mut late)
            .into_iter()
            .filter(|f| f.wire.subscribes.contains(&id))
            .collect();
        let export = got
            .first()
            .expect("the subscribe must produce an export, or nothing below is tested");
        let folded = export.wire.row.as_ref().expect("a keyed export carries a row")["folded"]
            .as_u64()
            .expect("`folded` is a count");
        assert_eq!(
            folded, 3,
            "the export reflects exactly the three inputs that arrived BEFORE the \
             subscribe: 0 would be a control item that jumped the queue, 10 one \
             that sank behind inputs emitted after it, 7 a queue popped LIFO"
        );
        // And the positive half on the same hub: all ten were folded, so the `3`
        // above is a statement about ORDER and not about inputs being lost.
        assert_eq!(
            one.lock().unwrap().len(),
            10,
            "all ten inputs were folded, so the export's 3 is about placement and \
             not about a dropped emit"
        );
    }
}
