//! **Human actions: the write-time check** (V1.8 spec §2, "one validator, two
//! callers"). A secret field with no destination never enters the soup.
//!
//! A human action's body is the human-action format: `##` sections are its
//! phases, and a `field` fence is one input (`name`, `label`, `secret` default
//! TRUE, `route`, `note`). [`validate_action`] refuses:
//!
//! - a secret field (the default) with no `route`;
//! - a secret field routed `node` (v1: the graph node is for non-secret values
//!   only, being "less secure", PRODUCT-SPEC §9);
//! - a route that is not one of `env`, `env:<NAME>`, `keychain`, `node`.
//!
//! The soup crate's own dataset passes through it on every load
//! ([`crate::demo::Dataset::embedded`]), so a curated action that could not
//! be routed fails loudly instead of rendering. The face calls the same
//! function on display (V1.8), as defence in depth.

use std::fmt;

/// One `field` fence: its keys and values, in order.
pub type Fence = Vec<(String, String)>;

/// Why an action was refused. Names the field, never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reject {
    /// A secret field with no `route`.
    Unrouted { field: String },
    /// A secret field routed to the graph node.
    SecretToNode { field: String },
    /// A `route` that is not one of the four.
    UnknownRoute { field: String, route: String },
}

impl fmt::Display for Reject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reject::Unrouted { field } => write!(f, "field `{field}` is secret and names no route (env, env:<NAME>, keychain); it never enters the soup"),
            Reject::SecretToNode { field } => write!(f, "field `{field}` is secret and routed to `node`; in v1 a node takes non-secret values only"),
            Reject::UnknownRoute { field, route } => write!(f, "field `{field}` names route `{route}`, which is not env, env:<NAME>, keychain or node"),
        }
    }
}

impl std::error::Error for Reject {}

/// Where a field's value goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// The environment's `secrets.env_file`, as `NAME=value` (the field's name,
    /// or the one given after `env:`).
    Env { var: String },
    /// The macOS login keychain: service `cauldron.<env>`, account = the field.
    Keychain,
    /// The action's own graph node (non-secret values only).
    Node,
}

fn get<'a>(fence: &'a Fence, key: &str) -> Option<&'a str> {
    fence.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// Whether a fence's field is secret (the default: anything but `secret: false`).
pub fn is_secret(fence: &Fence) -> bool {
    get(fence, "secret") != Some("false")
}

/// A fence's route, if it names one; an unknown one is refused.
pub fn route_of(fence: &Fence) -> Result<Option<Route>, Reject> {
    let field = get(fence, "name").unwrap_or("").to_string();
    let Some(r) = get(fence, "route") else { return Ok(None) };
    Ok(Some(match r {
        "env" => Route::Env { var: field },
        "keychain" => Route::Keychain,
        "node" => Route::Node,
        r => match r.strip_prefix("env:").filter(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')) {
            Some(var) => Route::Env { var: var.to_string() },
            None => return Err(Reject::UnknownRoute { field, route: r.to_string() }),
        },
    }))
}

/// One phase per `##` section (or the text before the first heading) that
/// carries a `field` fence or a `Deliverable:` line; each with its field
/// fences. A heading or a `Deliverable:` inside a fence is text.
pub fn phases(body: &str) -> Vec<Vec<Fence>> {
    let mut sections: Vec<(bool, Vec<Fence>)> = vec![(false, Vec::new())];
    let mut fence: Option<(String, Fence)> = None;
    for line in body.lines() {
        if let Some((kind, kv)) = fence.as_mut() {
            if line.trim_start().starts_with("```") {
                let (kind, kv) = (std::mem::take(kind), std::mem::take(kv));
                fence = None;
                if kind == "field" {
                    let sec = sections.last_mut().expect("one section");
                    sec.0 = true;
                    sec.1.push(kv);
                }
            } else if let Some((k, v)) = line.split_once(':') {
                kv.push((k.trim().to_string(), v.trim().to_string()));
            }
            continue;
        }
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("```") {
            fence = Some((rest.trim().to_string(), Vec::new()));
        } else if t.starts_with("## ") {
            sections.push((false, Vec::new()));
        } else if t.trim_start_matches('*').starts_with("Deliverable:") {
            sections.last_mut().expect("one section").0 = true;
        }
    }
    sections.into_iter().filter(|(is, _)| *is).map(|(_, f)| f).collect()
}

/// Every `field` fence of `body`, in order (a section with a field is a phase).
pub fn fields(body: &str) -> Vec<Fence> {
    phases(body).into_iter().flatten().collect()
}

/// **The write-time check.** `Ok` when every field of `body` can be routed.
pub fn validate_action(body: &str) -> Result<(), Reject> {
    for f in fields(body) {
        let name = get(&f, "name").unwrap_or("").to_string();
        match (is_secret(&f), route_of(&f)?) {
            (true, None) => return Err(Reject::Unrouted { field: name }),
            (true, Some(Route::Node)) => return Err(Reject::SecretToNode { field: name }),
            _ => {}
        }
    }
    Ok(())
}
