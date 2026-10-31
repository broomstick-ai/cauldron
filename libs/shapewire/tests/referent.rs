//! # The key-level nesting edge, proved — `Shape::REFERS`, `ShapeInfo::refs`, `Mirror::refers`
//!
//! The operator's law, stated as a law and not a fix: *"children should be
//! nested shapes. You don't usually want all descendents visible. You want a
//! count on a single agent of how many descendents it has, and the ability to
//! traverse its children … it's a graph traversal on nodes in a flat list."*
//!
//! The chassis already held the SHAPE-level edge (`Shape::PARENT`,
//! `ShapeInfo::parent`, `Hub::children`, `Epoch::parent_epoch`). What it did
//! not hold was the KEY-level one: a cell declared `Ref("node")` carried ids,
//! and WHICH shape those ids were keys of was prose. `REFERS` makes it a
//! declaration a consumer reads off `shapes.list`, and this file walks it.
//!
//! Every red below was forced with its real text before it was believed; the
//! mutation is in each test's docstring.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use serde_json::{json, Value};

use shapewire::cell::Cells;
use shapewire::fold::{Outbox, Root};
use shapewire::hub::{Hub, Kind, Shape, Tracking, Tx};
use shapewire::mirror::Mirror;
use shapewire::operation::{
    DType::{List, Ref, Scalar},
    Presence::{Optional, Required},
    Scalar::Int,
};
use shapewire::wire::{CellState, Op, ShapeFrame, Stamped, Timestamp};

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

shapewire::declare_cells! {
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

fn exports(f: &mut shapewire::hub::ConnFrames) -> Vec<Arc<ShapeFrame>> {
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
