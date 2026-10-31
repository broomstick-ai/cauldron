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
//! What he has done is kept per environment beside the draft
//! (`<data>/<env>/tui/soup/tickets.json`): the steps marked done and the
//! fields an older build recorded as SUBMITTED, by name. This face takes no
//! value at all (0217 B2): the human-action contract delivers a value to its
//! project's registered write target, a cluster's ticket names none, and a
//! box that only recorded "submitted" while dropping the key was a lie — so a
//! field says where to give it instead, and an old `submitted` entry is
//! shown as NOT stored. Every entry names a step or a field of a human action in the
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

/// One input a step asks him to paste into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub label: String,
    pub secret: bool,
    pub note: Option<String>,
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

/// A fence's `key: value` header and its payload (after a `---` line, or
/// all of it when there is none and `payload` is asked for).
fn fence(lines: &[String], payload: bool) -> (Vec<(String, String)>, String) {
    let split = lines.iter().position(|l| l.trim() == "---");
    let (head, body): (&[String], &[String]) = match (split, payload) {
        (Some(i), _) => (&lines[..i], &lines[i + 1..]),
        (None, true) => (&[], lines),
        (None, false) => (lines, &[]),
    };
    let kv = head
        .iter()
        .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
        .collect();
    (kv, body.join("\n"))
}

fn get(kv: &[(String, String)], k: &str) -> Option<String> {
    kv.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone()).filter(|v| !v.is_empty())
}

/// Read a ticket's body.
pub fn parse(body: &str) -> Ticket {
    // Sections: (heading, blocks, deliverable).
    let mut sections: Vec<(String, Vec<Block>, Option<String>)> = vec![(String::new(), Vec::new(), None)];
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
            let sec = sections.last_mut().expect("one section at least");
            match kind {
                "field" => {
                    let (kv, _) = fence(&inner, false);
                    let name = get(&kv, "name").unwrap_or_default();
                    sec.1.push(Block::Field(Field {
                        label: get(&kv, "label").unwrap_or_else(|| name.clone()),
                        secret: get(&kv, "secret").is_none_or(|v| v != "false"),
                        note: get(&kv, "note"),
                        name,
                    }));
                }
                "copy" => {
                    let (kv, text) = fence(&inner, true);
                    sec.1.push(Block::Copy { label: get(&kv, "label").unwrap_or_else(|| "Text to paste".into()), text });
                }
                "open" => {
                    let (kv, _) = fence(&inner, false);
                    let url = get(&kv, "url").unwrap_or_default();
                    sec.1.push(Block::Open { label: get(&kv, "label").unwrap_or_else(|| "Open link".into()), url });
                }
                // Any other fence is text he reads.
                _ => sec.1.extend(inner.into_iter().map(|l| Block::Text(format!("    {l}")))),
            }
            continue;
        }
        if let Some(h) = t.strip_prefix("## ") {
            sections.push((h.trim().to_string(), Vec::new(), None));
            continue;
        }
        let sec = sections.last_mut().expect("one section at least");
        let bare = t.trim_start().trim_start_matches("**").trim_start();
        if let Some(d) = bare.strip_prefix("Deliverable:").or_else(|| bare.strip_prefix("Deliverable**:")) {
            sec.2 = Some(d.trim().trim_start_matches("**").trim().to_string());
            continue;
        }
        sec.1.push(Block::Text(t.to_string()));
    }
    let is_step = |s: &(String, Vec<Block>, Option<String>)| s.2.is_some() || s.1.iter().any(|b| matches!(b, Block::Field(_)));
    let any_step = sections.iter().any(is_step);
    let mut t = Ticket::default();
    let mut seen: Vec<String> = Vec::new();
    for (i, sec) in sections.into_iter().enumerate() {
        if !any_step || is_step(&sec) {
            let (title, blocks, deliverable) = sec;
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
            let (title, blocks, _) = sec;
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
}
