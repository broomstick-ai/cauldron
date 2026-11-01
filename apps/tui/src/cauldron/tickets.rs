//! **A human action as its steps** — the ticket's markdown body read in the
//! real human-action format (`~/code/human-actions/SCHEMA.md`, "Body" and
//! "Phases"; `core:workspaces` §16), so the Actions tab can hold his hand
//! through it (8e4fca10 msg 165: "it needs to hold people's hand ... there
//! actually need to be like text boxes to like paste these in").
//!
//! A STEP is a phase: a `##` section that carries a `field` fence (a value
//! step) or a `Deliverable:` line (an instructional step). A section with
//! neither is prose. A ticket with no step at all is one implicit step, its
//! whole body. Everything before the first heading is section 0, and is a
//! step by the same test. Inside a step: its text, its `field` fences (an
//! input, masked when `secret`, which is the default), its `copy` fences (a
//! text to copy) and its `open` fences (a link to open). A heading or a
//! `Deliverable:` inside a fence is text.
//!
//! A field names where its value goes in its fence's `route:` key (R-V1
//! item 8, product spec §9, which supersedes 0217 B2's "not taken here"):
//! `env`, `env:<NAME>`, `keychain` or `node` ([`Route`]). There is no default
//! destination: [`validate_action`] refuses a secret field with no route, a
//! secret field routed to a node, a route it does not know, and (V1.8.S4)
//! two fields with one name or on one env variable — both copies. It is the
//! soup crate's (`soup::actions`), and the soup's write path is the gate;
//! this face calls the same function on display, so an action that somehow
//! holds an unroutable field shows that field as REFUSED, with no input.
//! And it is the soup's PARSER too (V1.8.S3): [`parse`] lays out the
//! sections [`soup::actions::sections`] read, and holds no fence reader of
//! its own, so what this face draws and what the gate checked cannot differ.
//!
//! What he has done is kept per environment beside the draft
//! (`<data>/<env>/tui/soup/tickets.json`): the steps marked done and the
//! fields SUBMITTED — routed to their destination — by NAME, never a value.
//! Every entry names a step or a field of a human action in the
//! soup, by its id now (`X-3/HA-4/step`). An entry that names nothing here —
//! an id from before msg 235, or an item of a seed this environment no longer
//! reads (REL-3.S2b: prod's own `tickets.json` named the repo seed's
//! `X-3/HA-4` after REL-3.S2 took that seed away) — never stops the face
//! opening: it is KEPT on disk untouched (every write re-reads the file under
//! its lock and changes only its own key), skipped in the view, and said once
//! in a quiet notice ([`Store::dangling_notice`]), so nothing he did is lost
//! and nothing is lost without a word.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub use crate::cauldron::route::Route;
use soup::actions::{self, Piece, Section};

/// One input a step asks him to paste into: the SOUP's parsed field
/// (V1.8.S3, "one parser"). This face never reads a fence itself; it draws
/// the fields [`soup::actions::sections`] found, which are the very fields
/// the soup's write path validated.
pub use soup::actions::Field;

/// Why an action's field is refused, and THE validator: both the soup
/// crate's (V1.8.S2, "one validator, two callers"). The soup's write path
/// calls [`validate_action`] before anything is stored; this face calls the
/// very same function, and [`field_reject`] applies the very same per-field
/// rule ([`soup::actions::field_reject`]) on display.
pub use soup::actions::{validate_action, Reject};

/// Why `field` is refused, or none: the soup's rule, on a parsed field.
pub fn field_reject(field: &Field) -> Option<Reject> {
    field.reject()
}

/// One block of a step, in its order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// A line of the step's text (markdown, one source line).
    Text(String),
    Field(Field),
    /// A text he is to paste somewhere: its label and the text itself.
    Copy { label: String, text: String },
    /// A page to open: its label and its URL.
    Open { label: String, url: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// Its heading, as written (empty for the implicit step).
    pub title: String,
    /// Its identity: the heading, slugged (the schema's rule).
    pub slug: String,
    pub blocks: Vec<Block>,
    pub deliverable: Option<String>,
}

impl Step {
    pub fn fields(&self) -> impl Iterator<Item = &Field> {
        self.blocks.iter().filter_map(|b| match b {
            Block::Field(f) => Some(f),
            _ => None,
        })
    }
}

/// A ticket's body: what comes before its steps, and its steps.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ticket {
    /// Prose before the first step (and any section that is not a step).
    pub intro: Vec<String>,
    pub steps: Vec<Step>,
}

/// One thing in a ticket a key acts on, in the order they are drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    /// Field `name` of step `step`: Enter types into it, Enter again submits.
    Field { step: usize, field: Field },
    /// Step `step`'s `done`.
    Done { step: usize },
    Copy { step: usize, label: String, text: String },
    Open { step: usize, label: String, url: String },
}

/// `## Section 2 — the webhook secret` → `section-2-the-webhook-secret`:
/// ASCII alphanumerics kept, every other run one `-`, trimmed, capped at 60.
pub fn slugify(heading: &str) -> String {
    let mut out = String::new();
    for c in heading.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let t = out.trim_matches('-');
    t.chars().take(60).collect::<String>().trim_end_matches('-').to_string()
}

/// One parsed piece as the blocks it draws as.
fn block_of(p: Piece) -> Vec<Block> {
    let f = match p {
        Piece::Text(t) => return vec![Block::Text(t)],
        Piece::Fence(f) => f,
    };
    if let Some(field) = f.field() {
        return vec![Block::Field(field)];
    }
    let (kv, payload) = f.split();
    let label = |or: &str| actions::value(&kv, "label").unwrap_or(or).to_string();
    match f.kind.as_str() {
        "copy" => vec![Block::Copy { label: label("Text to paste"), text: payload }],
        "open" => vec![Block::Open { label: label("Open link"), url: actions::value(&kv, "url").unwrap_or("").to_string() }],
        // Any other fence is text he reads.
        _ => f.lines.into_iter().map(|l| Block::Text(format!("    {l}"))).collect(),
    }
}

/// Read a ticket's body: the soup's sections ([`soup::actions::sections`],
/// the one parser), laid out as steps. A step is a section that is a phase.
pub fn parse(body: &str) -> Ticket {
    let sections = actions::sections(body);
    let any_step = sections.iter().any(Section::is_phase);
    let mut t = Ticket::default();
    let mut seen: Vec<String> = Vec::new();
    for (i, sec) in sections.into_iter().enumerate() {
        let is_step = sec.is_phase();
        let Section { heading: title, pieces, deliverable } = sec;
        let blocks: Vec<Block> = pieces.into_iter().flat_map(block_of).collect();
        if !any_step || is_step {
            // The implicit step: the whole body is one.
            if !any_step && i > 0 {
                let last = t.steps.last_mut().expect("the implicit step is first");
                if !title.is_empty() {
                    last.blocks.push(Block::Text(format!("**{title}**")));
                }
                last.blocks.extend(blocks);
                continue;
            }
            let base = if title.is_empty() { "step".to_string() } else { slugify(&title) };
            let n = seen.iter().filter(|s| **s == base).count();
            seen.push(base.clone());
            let slug = if n == 0 { base } else { format!("{base}-{}", n + 1) };
            t.steps.push(Step { title, slug, blocks: trim_blank(blocks), deliverable });
        } else {
            if !title.is_empty() {
                t.intro.push(format!("**{title}**"));
            }
            t.intro.extend(blocks.into_iter().filter_map(|b| match b {
                Block::Text(x) => Some(x),
                _ => None,
            }));
        }
    }
    while t.intro.last().is_some_and(|l| l.trim().is_empty()) {
        t.intro.pop();
    }
    while t.intro.first().is_some_and(|l| l.trim().is_empty()) {
        t.intro.remove(0);
    }
    t
}

fn trim_blank(mut b: Vec<Block>) -> Vec<Block> {
    let blank = |x: &Block| matches!(x, Block::Text(t) if t.trim().is_empty());
    while b.last().is_some_and(blank) {
        b.pop();
    }
    while b.first().is_some_and(blank) {
        b.remove(0);
    }
    b
}

impl Ticket {
    /// Every part a key acts on, in drawing order: each step's copies,
    /// opens and fields where they stand, then its `done`.
    pub fn parts(&self) -> Vec<Part> {
        let mut out = Vec::new();
        for (i, s) in self.steps.iter().enumerate() {
            for b in &s.blocks {
                match b {
                    Block::Field(f) => out.push(Part::Field { step: i, field: f.clone() }),
                    Block::Copy { label, text } => out.push(Part::Copy { step: i, label: label.clone(), text: text.clone() }),
                    Block::Open { label, url } => out.push(Part::Open { step: i, label: label.clone(), url: url.clone() }),
                    Block::Text(_) => {}
                }
            }
            out.push(Part::Done { step: i });
        }
        out
    }
}

// ── what he has done ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DoneFile {
    /// `<cluster>/HA-<n>/<step slug>`.
    #[serde(default)]
    pub done: Vec<String>,
    /// `<cluster>/HA-<n>/<field name>`: submitted, never the value.
    #[serde(default)]
    pub submitted: Vec<String>,
}

pub struct Store {
    path: PathBuf,
    writable: bool,
    pub file: DoneFile,
}

pub fn key(cluster: &str, serial: u32, what: &str) -> String {
    format!("{cluster}/HA-{serial}/{what}")
}

impl Store {
    pub fn open(dir: &Path, writable: bool) -> Result<Store> {
        let path = dir.join("tickets.json");
        let file = match std::fs::read_to_string(&path) {
            Ok(t) => serde_json::from_str(&t).with_context(|| format!("parse {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => DoneFile::default(),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        Ok(Store { path, writable, file })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Re-read the file (another face wrote it). An entry naming nothing in
    /// `forest` is adopted with the rest and skipped by the view; what to say
    /// about it is [`Store::dangling_notice`]'s.
    pub fn reload(&mut self) -> Result<()> {
        let fresh = Store::open(self.path.parent().unwrap_or(Path::new(".")), self.writable)?;
        self.file = fresh.file;
        Ok(())
    }

    /// Every entry naming no step (in `done`) or field (in `submitted`) of
    /// a human action in `forest`: no cluster by that id, no `HA-<n>` in it,
    /// or no step / field by that name in its ticket.
    pub fn unknown(&self, forest: &crate::cauldron::model::Forest) -> Vec<String> {
        let names = |entry: &str, done: bool| -> Option<()> {
            let mut it = entry.splitn(3, '/');
            let (cluster, ha, what) = (it.next()?, it.next()?, it.next()?);
            let serial: u32 = ha.strip_prefix("HA-")?.parse().ok()?;
            let h = forest.get(forest.find(cluster)?).human_action(serial)?;
            let t = parse(&h.body);
            let hit = if done { t.steps.iter().any(|s| s.slug == what) } else { t.steps.iter().flat_map(Step::fields).any(|f| f.name == what) };
            hit.then_some(())
        };
        let done = self.file.done.iter().filter(|e| names(e, true).is_none());
        let submitted = self.file.submitted.iter().filter(|e| names(e, false).is_none());
        done.chain(submitted).cloned().collect()
    }

    /// The one quiet line the face shows when entries name nothing in
    /// `forest` ([`Store::unknown`]): `2 tickets refer to items that no longer
    /// exist`. `None` when every entry names something. The entries stay in
    /// the file; nothing here writes.
    pub fn dangling_notice(&self, forest: &crate::cauldron::model::Forest) -> Option<String> {
        match self.unknown(forest).len() {
            0 => None,
            1 => Some("1 ticket refers to an item that no longer exists".into()),
            n => Some(format!("{n} tickets refer to items that no longer exist")),
        }
    }

    pub fn is_done(&self, cluster: &str, serial: u32, slug: &str) -> bool {
        self.file.done.contains(&key(cluster, serial, slug))
    }

    pub fn is_submitted(&self, cluster: &str, serial: u32, field: &str) -> bool {
        self.file.submitted.contains(&key(cluster, serial, field))
    }

    /// Change the file under its lock, from the file as it is inside the
    /// lock, and adopt the result. On the `--dump` path the change lives
    /// only in this face.
    fn update(&mut self, f: impl FnOnce(&mut DoneFile)) -> Result<()> {
        if !self.writable {
            f(&mut self.file);
            return Ok(());
        }
        let path = self.path.clone();
        let file = env::lock::with_lock(&path, || {
            let mut file: DoneFile = match std::fs::read_to_string(&path) {
                Ok(t) => serde_json::from_str(&t).with_context(|| format!("parse {}", path.display()))?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => DoneFile::default(),
                Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
            };
            f(&mut file);
            crate::cauldron::store::write_text_atomic(&path, &serde_json::to_string_pretty(&file)?)?;
            Ok(file)
        })?;
        self.file = file;
        Ok(())
    }

    /// Mark step `slug` done, or (`on` false) not done.
    pub fn set_done(&mut self, cluster: &str, serial: u32, slug: &str, on: bool) -> Result<()> {
        let k = key(cluster, serial, slug);
        self.update(|f| {
            f.done.retain(|x| *x != k);
            if on {
                f.done.push(k);
            }
        })
    }

    /// Record field `field` as submitted: routed to its destination. Its
    /// NAME only — a value never reaches this file.
    pub fn set_submitted(&mut self, cluster: &str, serial: u32, field: &str) -> Result<()> {
        let k = key(cluster, serial, field);
        self.update(|f| {
            if !f.submitted.contains(&k) {
                f.submitted.push(k);
            }
        })
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    const STRIPE: &str = "Two keys from the dashboard.\n\n## Section 1 — the secret key\n\n1. Open the API keys page.\n\n```open\nurl: https://dashboard.stripe.com/apikeys\nlabel: Open Stripe API keys\n```\n\n2. Paste it here:\n\n```field\nname: STRIPE_SECRET_KEY\nlabel: Stripe secret key\n```\n\n## A warning\n\nDo not paste it anywhere else.\n\n## Section 2 — turn on webhooks\n\nFlip the switch.\n\n**Deliverable:** webhooks are on.\n";

    #[test]
    fn sections_with_a_field_or_a_deliverable_are_the_steps() {
        let t = parse(STRIPE);
        assert_eq!(t.steps.len(), 2, "{t:?}");
        assert_eq!(t.steps[0].slug, "section-1-the-secret-key");
        assert_eq!(t.steps[0].fields().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["STRIPE_SECRET_KEY"]);
        assert!(t.steps[0].fields().all(|f| f.secret), "a field is secret unless it says not");
        assert_eq!(t.steps[1].deliverable.as_deref(), Some("webhooks are on."));
        assert!(t.intro.iter().any(|l| l.contains("Two keys")), "{:?}", t.intro);
        assert!(t.intro.iter().any(|l| l.contains("A warning")), "a section that is no step is prose");
        let parts = t.parts();
        assert!(matches!(parts[0], Part::Open { .. }));
        assert!(matches!(parts[1], Part::Field { .. }));
        assert!(matches!(parts[2], Part::Done { step: 0 }));
        assert!(matches!(parts[3], Part::Done { step: 1 }));
    }

    #[test]
    fn a_body_with_no_step_is_one_implicit_step() {
        let t = parse("1. Read report 0017.\n2. Answer its five questions in one message.");
        assert_eq!(t.steps.len(), 1);
        assert_eq!(t.steps[0].blocks.len(), 2);
        assert_eq!(t.parts(), vec![Part::Done { step: 0 }]);
    }

    #[test]
    fn a_heading_inside_a_fence_is_text_and_a_copy_keeps_its_payload() {
        let t = parse("## Send it\n\n```copy\nlabel: Subject\n---\n## not a heading\nDeliverable: not one\n```\n\nDeliverable: sent.");
        assert_eq!(t.steps.len(), 1);
        assert_eq!(t.steps[0].blocks, vec![Block::Copy { label: "Subject".into(), text: "## not a heading\nDeliverable: not one".into() }]);
        assert_eq!(t.steps[0].deliverable.as_deref(), Some("sent."));
    }

    fn one_field(head: &str) -> String {
        format!("## Give it\n\n```field\nname: K\n{head}```\n")
    }

    #[test]
    fn a_fields_route_is_read_from_its_fence() {
        let f = |head: &str| parse(&one_field(head)).steps[0].fields().next().cloned().unwrap();
        assert_eq!(f("route: env\n").route, Route::Env("K".into()));
        assert_eq!(f("route: env:pay_key\n").route, Route::Env("pay_key".into()));
        assert_eq!(f("route: keychain\n").route, Route::Keychain);
        assert_eq!(f("secret: false\nroute: node\n").route, Route::Node);
        assert_eq!(f("").route, Route::Unset);
        assert_eq!(f("route: vault\n").route, Route::Unknown("vault".into()));
    }

    /// G2 (and G1's rule, here at the face): a secret field needs a route,
    /// and a node takes only a non-secret one. Each refusal is paired with
    /// the accepted case beside it, so a constant answer fails.
    #[test]
    fn the_validator_refuses_an_unroutable_secret_and_accepts_a_routed_one() {
        let v = |head: &str| validate_action(&one_field(head));
        assert_eq!(v(""), Err(Reject::NoRoute { field: "K".into() }));
        assert_eq!(v("secret: true\n"), Err(Reject::NoRoute { field: "K".into() }));
        assert_eq!(v("route: keychain\n"), Ok(()));
        assert_eq!(v("route: env\n"), Ok(()));
        assert_eq!(v("route: node\n"), Err(Reject::SecretToNode { field: "K".into() }));
        assert_eq!(v("secret: true\nroute: node\n"), Err(Reject::SecretToNode { field: "K".into() }));
        assert_eq!(v("secret: false\nroute: node\n"), Ok(()));
        assert_eq!(v("route: vault\n"), Err(Reject::UnknownRoute { field: "K".into(), route: "vault".into() }));
        // A non-secret field with no route is not refused (it has no value
        // to leak); it simply takes none here.
        assert_eq!(v("secret: false\n"), Ok(()));
        // V1.8.S2-I F1's two shapes: a fence open at the end of the body is
        // still a field, and a `route:` below `---` is payload. The face draws
        // the field and refuses it, as the soup's gate does.
        let open = "## Give it\n\n```field\nname: K\nlabel: k\n";
        assert_eq!(parse(open).steps[0].fields().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["K"]);
        assert_eq!(validate_action(open), Err(Reject::NoRoute { field: "K".into() }));
        assert_eq!(v("---\nroute: env\n"), Err(Reject::NoRoute { field: "K".into() }));
        assert_eq!(v("route: env\n---\nnote\n"), Ok(()));
        // A body with no field at all is fine.
        assert_eq!(validate_action("## Do it\n\nDeliverable: done."), Ok(()));
        // The refusal names the field.
        assert!(v("").unwrap_err().to_string().starts_with("K "));
    }
}