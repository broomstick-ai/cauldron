//! **The Engine**: one cluster's planning graph — his directives, the objectives
//! that decompose them, the milestones of each objective and the tasks each
//! milestone holds — laid out as RAILS, one node per row, in the grammar of
//! `git log --graph` with time flowing down.
//!
//! The drawing rule (design 0069): rows are emitted directive by directive,
//! objective by objective, each milestone in dependency order followed by its
//! tasks in dependency order. Every milestone and task row is a NODE. A
//! `depends_on` between two nodes is a RAIL: the needed node's lane runs down
//! to the needing node's row. A node needed by two later nodes FORKS on the
//! row after it (`├─╮`); a node that needs two earlier ones is reached by both
//! rails, which JOIN on the row before it (`├─╯`). Lanes are allocated
//! lowest-free first. Containment (a milestone's tasks) is indentation and
//! never a rail; `serves` is grouping.
//!
//! The layout is pure data — [`Cell`]s per row — so a test can hold the rail
//! characters to the design row by row.

use crate::cauldron::model::{milestone_order, Cluster, Local, TaskState};

/// A node of the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Node {
    Milestone(u32),
    Task(u32),
}

/// One column-pair of the rail margin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    /// Nothing in this lane.
    Empty,
    /// A rail character, and what fills the column after it (`─` inside a
    /// fork or a join, else a space).
    Rail(char, char),
    /// This row's node sits in this lane (the view draws its state glyph).
    Node,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// The rule of the directive with this id.
    Directive(String),
    Objective(u32),
    Node(Node),
    /// A fork or a join: rails only.
    Connector,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub kind: Kind,
    pub cells: Vec<Cell>,
    /// The lanes that run on below this row: what a line drawn under it
    /// (a trace line) must carry through.
    pub below: Vec<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Graph {
    pub rows: Vec<Row>,
    /// The widest the margin gets: every row's margin is this many lanes.
    pub lanes: usize,
}

impl Graph {
    /// The margin for a line drawn UNDER row `i`: the rails running on.
    pub fn under(&self, i: usize) -> String {
        (0..self.lanes).map(|l| if self.rows[i].below.get(l).copied().unwrap_or(false) { "│ " } else { "  " }).collect()
    }

    /// The rail margin of row `i` as text (`Node` drawn as `glyph`), `2 *
    /// lanes` columns wide.
    pub fn margin(&self, i: usize, glyph: &str) -> String {
        let mut s = String::new();
        for l in 0..self.lanes {
            match self.rows[i].cells.get(l).copied().unwrap_or(Cell::Empty) {
                Cell::Empty => s.push_str("  "),
                Cell::Rail(c, fill) => {
                    s.push(c);
                    s.push(fill);
                }
                Cell::Node => {
                    s.push_str(glyph);
                    s.push(' ');
                }
            }
        }
        s
    }
}

/// The nodes a node needs, among the cluster's milestones and tasks.
fn needs(g: &Cluster, n: Node) -> Vec<Node> {
    match n {
        Node::Milestone(s) => g
            .milestone(s)
            .map(|m| {
                m.depends_on
                    .iter()
                    .filter_map(|d| match Local::parse(d) {
                        Some(Local::M(x)) => Some(Node::Milestone(x)),
                        Some(Local::T(x)) => Some(Node::Task(x)),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Node::Task(s) => g
            .tasks
            .iter()
            .find(|t| t.serial == s)
            .map(|t| t.depends_on.iter().map(|&d| Node::Task(d)).collect())
            .unwrap_or_default(),
    }
}

/// Tasks `serials` in dependency order among themselves, ties by serial.
fn task_order(g: &Cluster, serials: &[u32]) -> Vec<u32> {
    let mut rest: Vec<u32> = serials.to_vec();
    rest.sort_unstable();
    let mut out: Vec<u32> = Vec::new();
    while !rest.is_empty() {
        let pick = rest
            .iter()
            .position(|&s| {
                g.tasks
                    .iter()
                    .find(|t| t.serial == s)
                    .is_none_or(|t| t.depends_on.iter().all(|d| !rest.contains(d)))
            })
            // A cycle is refused at load; take the lowest rather than spin.
            .unwrap_or(0);
        out.push(rest.remove(pick));
    }
    out
}

/// The row sequence before any rails: directive rules, objective rows, and
/// the nodes.
fn sequence(g: &Cluster, directives: &[String]) -> Vec<Kind> {
    let mut out = Vec::new();
    let order = milestone_order(&g.milestones).unwrap_or_default();
    let mut placed: Vec<u32> = Vec::new();
    for d in directives {
        let objs: Vec<u32> = {
            let mut v: Vec<u32> = g.objectives.iter().filter(|o| &o.serves == d).map(|o| o.serial).collect();
            v.sort_unstable();
            v
        };
        if objs.is_empty() {
            continue;
        }
        out.push(Kind::Directive(d.clone()));
        for o in objs {
            out.push(Kind::Objective(o));
            for &mi in &order {
                let m = &g.milestones[mi];
                if m.objective != o {
                    continue;
                }
                out.push(Kind::Node(Node::Milestone(m.serial)));
                for t in task_order(g, &m.tasks) {
                    placed.push(t);
                    out.push(Kind::Node(Node::Task(t)));
                }
            }
        }
    }
    // Tasks no milestone holds come last, with no heading.
    let loose: Vec<u32> = g.tasks.iter().map(|t| t.serial).filter(|s| !placed.contains(s)).collect();
    for t in task_order(g, &loose) {
        out.push(Kind::Node(Node::Task(t)));
    }
    out
}

/// The directives whose objectives this cluster holds, in the order they bind
/// it (ancestors' first).
pub fn directives_used(binding: &[String], g: &Cluster) -> Vec<String> {
    binding.iter().filter(|d| g.objectives.iter().any(|o| &o.serves == *d)).cloned().collect()
}

/// Lay cluster `g` out. `binding` is the directive ids that bind it, in order.
pub fn layout(g: &Cluster, binding: &[String]) -> Graph {
    let seq = sequence(g, &directives_used(binding, g));
    let nodes: Vec<Node> = seq.iter().filter_map(|k| if let Kind::Node(n) = k { Some(*n) } else { None }).collect();
    let index = |n: Node| nodes.iter().position(|&x| x == n);
    // dependents[i]: the LATER nodes that need node i, in emission order.
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (j, &n) in nodes.iter().enumerate() {
        for need in needs(g, n) {
            if let Some(i) = index(need).filter(|&i| i < j) {
                dependents[i].push(j);
            }
        }
    }
    // lanes[l]: the edge (to-node index) the lane carries.
    let mut lanes: Vec<Option<usize>> = Vec::new();
    let mut rows: Vec<Row> = Vec::new();
    let pass = |lanes: &[Option<usize>]| -> Vec<Cell> {
        lanes.iter().map(|l| if l.is_some() { Cell::Rail('│', ' ') } else { Cell::Empty }).collect()
    };
    // Record, on the row just pushed, which lanes run on below it.
    let mark = |rows: &mut Vec<Row>, lanes: &[Option<usize>]| {
        if let Some(r) = rows.last_mut() {
            r.below = lanes.iter().map(Option::is_some).collect();
        }
    };
    let mut ni = 0;
    for kind in seq {
        let Kind::Node(_) = kind else {
            rows.push(Row { cells: pass(&lanes), kind, below: Vec::new() });
            mark(&mut rows, &lanes);
            continue;
        };
        let me = ni;
        ni += 1;
        let incoming: Vec<usize> = (0..lanes.len()).filter(|&l| lanes[l] == Some(me)).collect();
        let lane = if let Some(&first) = incoming.first() {
            if incoming.len() > 1 {
                // JOIN: every other rail into me curves into the first.
                let hi = *incoming.last().expect("non-empty");
                let mut cells = pass(&lanes);
                for (l, cell) in cells.iter_mut().enumerate().take(hi + 1).skip(first) {
                    let fill = if l < hi { '─' } else { ' ' };
                    *cell = if l == first {
                        Cell::Rail('├', fill)
                    } else if l == hi {
                        Cell::Rail('╯', ' ')
                    } else if incoming.contains(&l) {
                        Cell::Rail('┴', fill)
                    } else if lanes[l].is_some() {
                        Cell::Rail('┼', fill)
                    } else {
                        Cell::Rail('─', fill)
                    };
                }
                rows.push(Row { kind: Kind::Connector, cells, below: Vec::new() });
                for &l in &incoming[1..] {
                    lanes[l] = None;
                }
                mark(&mut rows, &lanes);
            }
            first
        } else {
            match lanes.iter().position(Option::is_none) {
                Some(l) => l,
                None => {
                    lanes.push(None);
                    lanes.len() - 1
                }
            }
        };
        lanes[lane] = None;
        let mut cells = pass(&lanes);
        if cells.len() <= lane {
            cells.resize(lane + 1, Cell::Empty);
        }
        cells[lane] = Cell::Node;
        rows.push(Row { kind, cells, below: Vec::new() });
        let deps = &dependents[me];
        if let Some(&d0) = deps.first() {
            lanes[lane] = Some(d0);
        }
        mark(&mut rows, &lanes);
        if let Some((_, more)) = deps.split_first() {
            let mut new = Vec::new();
            for &d in more {
                let l = match lanes.iter().position(Option::is_none) {
                    Some(l) => l,
                    None => {
                        lanes.push(None);
                        lanes.len() - 1
                    }
                };
                lanes[l] = Some(d);
                new.push(l);
            }
            if !new.is_empty() {
                // FORK: my lane, and a new rail for each further dependent.
                let lo = new.iter().copied().chain([lane]).min().expect("non-empty");
                let hi = new.iter().copied().chain([lane]).max().expect("non-empty");
                let mut cells = pass(&lanes);
                for (l, cell) in cells.iter_mut().enumerate().take(hi + 1).skip(lo) {
                    let fill = if l < hi { '─' } else { ' ' };
                    *cell = if l == lane {
                        let c = match (lo < lane, hi > lane) {
                            (true, true) => '┼',
                            (true, false) => '┤',
                            _ => '├',
                        };
                        Cell::Rail(c, fill)
                    } else if new.contains(&l) {
                        let c = if l == hi {
                            '╮'
                        } else if l == lo {
                            '╭'
                        } else {
                            '┬'
                        };
                        Cell::Rail(c, fill)
                    } else if lanes[l].is_some() {
                        Cell::Rail('┼', fill)
                    } else {
                        Cell::Rail('─', fill)
                    };
                }
                rows.push(Row { kind: Kind::Connector, cells, below: Vec::new() });
                mark(&mut rows, &lanes);
            }
        }
    }
    let lanes = rows.iter().map(|r| r.cells.len()).max().unwrap_or(0).max(1);
    Graph { rows, lanes }
}

/// The state glyph of a task on the Engine.
pub fn task_glyph(s: TaskState) -> &'static str {
    s.glyph()
}
