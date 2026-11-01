//! **Human actions: the write-time check** (V1.8 spec §2, "one validator, two
//! callers"). A secret field with no destination never enters the soup.
//!
//! A human action's body is the human-action format: `##` sections are its
//! phases, and a `field` fence is one input (`name`, `label`, `secret` default
//! TRUE, `route`, `note`). [`validate_action`] refuses:
//!
//! - a secret field (the default) with no `route`;
//! - a secret field routed `node` (v1: the graph node is for non-secret values
//!   only, being "less secure", product spec §9);
//! - a route that is not one of `env`, `env:<NAME>`, `keychain`, `node`;
//! - two fields with one name (V1.8.S4): what he submitted is kept by name
//!   (`tickets.json`, `<cluster>/HA-n/<name>`), and a keychain item's account
//!   IS the name, so both copies would share one record and one item;
//! - two fields routed to one env variable (V1.8.S4: `env:B` beside a field
//!   `B` routed `env`), which would write the same `NAME=` line twice.
//!
//! Names compare EXACTLY (case-sensitive, after the fence's trim), as an env
//! file's keys do (`tui::cauldron::route::sets`), as a route word does
//! (`ENV` is not `env`) and as `tickets.json`'s keys do: `K` and `k` are two
//! fields and two variables. Both copies of a clash are refused, so the face
//! takes no value for either.
//!
//! The soup crate's own dataset passes through it on every load
//! ([`crate::demo::Dataset::embedded`]), so a curated action that could not
//! be routed fails loudly instead of rendering. The face calls the same
//! function on display (V1.8), as defence in depth, and reads a body through
//! the same PARSER, [`sections`] (V1.8.S3): its `tickets::Field` is
//! [`Field`], so the fields it draws are the fields this gate checked.

use std::fmt;

/// One `field` fence: its keys and values, in order.
pub type Fence = Vec<(String, String)>;

/// Why an action's field was refused (R-V1 item 8: "A secret action with no
/// destination is REJECTED at write time"). Names the field, never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reject {
    /// A secret field (the default) with no `route:`.
    NoRoute { field: String },
    /// A secret field routed to a node: v1 sends only non-secret values
    /// there (product spec §9, "less secure").
    SecretToNode { field: String },
    /// A `route:` that is not `env`, `env:<NAME>`, `keychain` or `node`.
    UnknownRoute { field: String, route: String },
    /// Two fields of one action named alike (exactly, case-sensitive).
    DuplicateName { field: String },
    /// Two fields (differently named) routed to one env variable `var`;
    /// `other` is the other field's name.
    SharedEnv { field: String, other: String, var: String },
}

impl fmt::Display for Reject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reject::NoRoute { field } => write!(f, "{field} is secret and names no destination (route: env, keychain)"),
            Reject::SecretToNode { field } => write!(f, "{field} is secret and routed to a node, which takes non-secret values only"),
            Reject::UnknownRoute { field, route } => write!(f, "{field} names route `{route}`, which is not env, env:<NAME>, keychain or node"),
            Reject::DuplicateName { field } => write!(f, "{field} is the name of two fields of this action; each field needs its own"),
            Reject::SharedEnv { field, other, var } => write!(f, "{field} and {other} are both routed to env variable {var}; each field needs its own"),
        }
    }
}

impl std::error::Error for Reject {}

/// Where a field's value goes, as its fence's `route:` names it. The face's
/// `tickets::Field::route` is this type (re-exported by `tui::cauldron::route`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// No `route:` key (or an empty one).
    Unset,
    /// `env` or `env:<NAME>`: the environment's env file, as `NAME=value`
    /// (`NAME` the field's name unless renamed).
    Env(String),
    /// `keychain`: the macOS keychain, service `cauldron.<env>`, account the
    /// field's name.
    Keychain,
    /// `node`: the action's graph node (non-secret only in v1).
    Node,
    /// Anything else, as written: refused.
    Unknown(String),
}

impl Route {
    /// Read a fence's `route:` value for field `field`.
    pub fn parse(v: Option<&str>, field: &str) -> Route {
        let Some(v) = v.map(str::trim).filter(|v| !v.is_empty()) else { return Route::Unset };
        match v {
            "env" => Route::Env(field.to_string()),
            "keychain" => Route::Keychain,
            "node" => Route::Node,
            _ => match v.strip_prefix("env:").map(str::trim) {
                Some(n) if env_name_ok(n) => Route::Env(n.to_string()),
                _ => Route::Unknown(v.to_string()),
            },
        }
    }

    /// The destination's word, the only thing the screen says of it.
    pub fn word(&self) -> &'static str {
        match self {
            Route::Unset => "no destination",
            Route::Env(_) => "env file",
            Route::Keychain => "keychain",
            Route::Node => "node",
            Route::Unknown(_) => "unknown destination",
        }
    }
}

/// `[A-Za-z_][A-Za-z0-9_]*`: what an env file's variable may be called.
pub fn env_name_ok(n: &str) -> bool {
    let mut c = n.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_') && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

/// A fence key's value: the first `key:` line's, trimmed, and `None` when
/// it is absent or empty (so `route:` alone names no route).
pub fn value<'a>(fence: &'a Fence, key: &str) -> Option<&'a str> {
    fence.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()).filter(|v| !v.is_empty())
}

/// Whether a fence's field is secret (the default: anything but `secret: false`).
pub fn is_secret(fence: &Fence) -> bool {
    value(fence, "secret") != Some("false")
}

/// A fence's route ([`Route::Unset`] when it names none).
pub fn route_of(fence: &Fence) -> Route {
    Route::parse(value(fence, "route"), value(fence, "name").unwrap_or(""))
}

/// **The rule**, for one field: why it is refused, or none. The face's
/// display-side check (`tickets::field_reject`) is this function.
pub fn field_reject(name: &str, secret: bool, route: &Route) -> Option<Reject> {
    let field = name.to_string();
    match route {
        Route::Unknown(r) => Some(Reject::UnknownRoute { field, route: r.clone() }),
        Route::Unset if secret => Some(Reject::NoRoute { field }),
        Route::Node if secret => Some(Reject::SecretToNode { field }),
        _ => None,
    }
}

/// One input a `field` fence asks for, as THIS parser reads it. The face's
/// `tui::cauldron::tickets::Field` is this type: the face draws what the
/// soup validated and never parses a fence itself (V1.8.S3, "one parser").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub label: String,
    pub secret: bool,
    pub note: Option<String>,
    /// Where its value goes (its fence's `route:`); [`Route::Unset`] when
    /// it names none.
    pub route: Route,
    /// Its clash with another field of the same action ([`Reject::DuplicateName`],
    /// [`Reject::SharedEnv`]), found by [`sections`] over the whole body;
    /// `None` for a fence read on its own ([`Field::from_fence`]).
    pub clash: Option<Reject>,
}

impl Field {
    /// The field a `field` fence's header declares.
    pub fn from_fence(kv: &Fence) -> Field {
        let name = value(kv, "name").unwrap_or("").to_string();
        Field {
            label: value(kv, "label").map_or_else(|| name.clone(), str::to_string),
            secret: is_secret(kv),
            note: value(kv, "note").map(str::to_string),
            route: route_of(kv),
            name,
            clash: None,
        }
    }

    /// Why this field is refused, or none: its own rule ([`field_reject`])
    /// first, then its clash with another field of the action.
    pub fn reject(&self) -> Option<Reject> {
        field_reject(&self.name, self.secret, &self.route).or_else(|| self.clash.clone())
    }
}

/// A fenced block: the word after its opening backticks, and its inner lines.
/// It is closed by the next line that starts with three backticks, or by the
/// END OF THE BODY: a fence left open is still a fence (V1.8.S3 — the old
/// soup parser dropped it, so a secret field the face drew passed the gate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FenceBlock {
    pub kind: String,
    pub lines: Vec<String>,
    /// A `field` fence's clash with another field of the body (V1.8.S4),
    /// set by [`sections`]; carried into [`FenceBlock::field`].
    pub clash: Option<Reject>,
}

impl FenceBlock {
    /// Its `key: value` header and its payload. A `---` line splits them;
    /// with none, a `copy` fence is all payload and any other all header.
    /// Keys BELOW a `---` are payload, never header (V1.8.S3).
    pub fn split(&self) -> (Fence, String) {
        let split = self.lines.iter().position(|l| l.trim() == "---");
        let (head, body): (&[String], &[String]) = match split {
            Some(i) => (&self.lines[..i], &self.lines[i + 1..]),
            None if self.kind == "copy" => (&[], &self.lines),
            None => (&self.lines, &[]),
        };
        let kv = head.iter().filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect();
        (kv, body.join("\n"))
    }

    /// The field this fence declares, when it is a `field` fence.
    pub fn field(&self) -> Option<Field> {
        (self.kind == "field").then(|| Field { clash: self.clash.clone(), ..Field::from_fence(&self.split().0) })
    }
}

/// One block of a section, in its order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// One source line (trailing whitespace trimmed).
    Text(String),
    Fence(FenceBlock),
}

/// One `##` section of a body (the first is what comes before any heading,
/// with an empty heading).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Section {
    /// Its heading as written after `## `, trimmed.
    pub heading: String,
    /// Its text lines and fences, in order. A `Deliverable:` line is not here.
    pub pieces: Vec<Piece>,
    /// Its `Deliverable:` line's text (`**` emphasis stripped), if any.
    pub deliverable: Option<String>,
}

impl Section {
    /// A phase (a step): it carries a `field` fence or a `Deliverable:` line.
    pub fn is_phase(&self) -> bool {
        self.deliverable.is_some() || self.fields().next().is_some()
    }

    /// Its fields, in order.
    pub fn fields(&self) -> impl Iterator<Item = Field> + '_ {
        self.pieces.iter().filter_map(|p| match p {
            Piece::Fence(f) => f.field(),
            Piece::Text(_) => None,
        })
    }
}

/// **THE parser** of a human action's body (V1.8.S3: the soup's write-time
/// gate and the face's display both read a body through this one function,
/// so they cannot disagree about what fields it holds). A `##` heading opens
/// a section; a line starting with three backticks opens a fence, which runs
/// to the next such line or the end of the body; a heading or a
/// `Deliverable:` inside a fence is text.
pub fn sections(body: &str) -> Vec<Section> {
    let mut out = vec![Section::default()];
    let mut lines = body.lines();
    while let Some(line) = lines.next() {
        let t = line.trim_end();
        if let Some(kind) = t.trim_start().strip_prefix("```").map(str::trim) {
            let mut inner = Vec::new();
            for l in lines.by_ref() {
                if l.trim_start().starts_with("```") {
                    break;
                }
                inner.push(l.to_string());
            }
            out.last_mut().expect("one section").pieces.push(Piece::Fence(FenceBlock { kind: kind.to_string(), lines: inner, clash: None }));
            continue;
        }
        if let Some(h) = t.strip_prefix("## ") {
            out.push(Section { heading: h.trim().to_string(), ..Section::default() });
            continue;
        }
        let sec = out.last_mut().expect("one section");
        let bare = t.trim_start().trim_start_matches("**").trim_start();
        if let Some(d) = bare.strip_prefix("Deliverable:").or_else(|| bare.strip_prefix("Deliverable**:")) {
            sec.deliverable = Some(d.trim().trim_start_matches("**").trim().to_string());
            continue;
        }
        sec.pieces.push(Piece::Text(t.to_string()));
    }
    mark_clashes(&mut out);
    out
}

/// V1.8.S4: mark every `field` fence that shares its name, or its env
/// variable, with another field fence of the body. Exact comparison (see
/// the module doc). A keychain item is keyed by the field's name, so two
/// fields on one keychain item are a [`Reject::DuplicateName`].
fn mark_clashes(sections: &mut [Section]) {
    let fields: Vec<Field> = sections.iter().flat_map(|s| s.fields().collect::<Vec<_>>()).collect();
    let var = |f: &Field| match &f.route {
        Route::Env(v) => Some(v.clone()),
        _ => None,
    };
    let clash = |i: usize| -> Option<Reject> {
        let me = &fields[i];
        let others = || fields.iter().enumerate().filter(move |(j, _)| *j != i).map(|(_, f)| f);
        if others().any(|o| o.name == me.name) {
            return Some(Reject::DuplicateName { field: me.name.clone() });
        }
        let v = var(me)?;
        others().find(|o| var(o).as_ref() == Some(&v)).map(|o| Reject::SharedEnv { field: me.name.clone(), other: o.name.clone(), var: v })
    };
    let clashes: Vec<Option<Reject>> = (0..fields.len()).map(clash).collect();
    let mut it = clashes.into_iter();
    for sec in sections.iter_mut() {
        for p in sec.pieces.iter_mut() {
            if let Piece::Fence(f) = p {
                if f.kind == "field" {
                    f.clash = it.next().expect("one clash per field fence");
                }
            }
        }
    }
}

/// One phase per section that is one ([`Section::is_phase`]); each with its
/// `field` fences' headers.
pub fn phases(body: &str) -> Vec<Vec<Fence>> {
    sections(body)
        .into_iter()
        .filter(Section::is_phase)
        .map(|s| {
            s.pieces
                .iter()
                .filter_map(|p| match p {
                    Piece::Fence(f) if f.kind == "field" => Some(f.split().0),
                    _ => None,
                })
                .collect()
        })
        .collect()
}

/// Every field of `body`, in order (a section with a field is a phase).
pub fn fields(body: &str) -> Vec<Field> {
    sections(body).iter().flat_map(|s| s.fields().collect::<Vec<_>>()).collect()
}

/// **The write-time check** — the ONE validator, with two callers: the
/// soup's write path ([`crate::demo::install_from`], [`crate::demo::Dataset::embedded`])
/// before anything is stored, and the face on display (`tui::cauldron::tickets`
/// re-exports this very function). `Ok` when every field of `body` can be
/// routed; otherwise the first field it refuses, in the body's order.
pub fn validate_action(body: &str) -> Result<(), Reject> {
    match fields(body).iter().find_map(Field::reject) {
        Some(r) => Err(r),
        None => Ok(()),
    }
}
