//! **Tags**: the id-shaped references he drops into the BUCKET so one dump
//! can answer many things across many clusters.
//!
//! The grammar is ONE function each way — [`Tag::render`] writes a tag and
//! [`scan`] finds them — so what the content pane inserts and what a send
//! parses back out cannot drift apart.
//!
//! An item's id is the SOUP's, unique across the cauldron, so a reference
//! names no cluster (8e4fca10 msg 235: "you don't need to preface it with a
//! cluster. There's only one soup"); a cluster itself is its MINTED id,
//! never a slug of its name ("If you want to use an ID, use a minted ID"),
//! so no name — with spaces or without — is ever inside a reference.
//!
//! ```text
//! [Q-1]                   a question (its cluster's Questions tab)
//! [Q-1.opt3]              option 3 of that question
//! [T-4]                   a task (its cluster's Tasks tab's engine)
//! [M-2]                   a milestone (Details, and the engine)
//! [OBJ-1]                 an objective (Details, and the engine)
//! [HA-1]                  a human action (its cluster's Actions tab)
//! [HA-1.step2]            step 2 of that human action
//! [DIR-1]                 a directive; [CON-1] a constraint (Details) —
//!                         soup items, bare like every other (task 0209
//!                         D16); the old `[node:DIR-1]` still reads
//! [X-3]                   a cluster itself (its scope, on Details), by
//!                         its minted id, bare (0217 B6); the old
//!                         `[cluster:X-3]` still reads
//! [msg:8e4fca10#21]       one of his messages (a trace quotes it)
//! [repo:harness]          a repo a cluster builds against (its Artifacts tab)
//! [service:tts]           a service (the Status tab, under its device)
//! [device:example-mac]    a device of the mesh (the Status tab)
//! [region:R-3]            a region of the soup (a cluster's Nodes tab; Status › Soup for unclaimed ones)
//! [IS-12]                 any other node of the soup — a
//!                         blueprint entry
//!                         (`MOD-tui-4f1c9a02`) included — bare like an
//!                         item (0217 B6: one soup, one reference shape);
//!                         the old `[node:IS-12]` still reads
//! [ripple:adjudicate]     a ripple (Status › Bus)
//! [run:r-8f3a]            one run of a ripple — an execution; [run:r-8f3a/author] one step of it
//! [event:EV-12]           one event on the bus (Status › Bus)
//! [dump:12]               one of his dumps, by its number (a cluster's log);
//! [dump:12.3]             its third note ("dump twelve, note three"). The
//!                         old `[send:12]` still reads, as the same dump.
//! [path:~/code/x/Cargo.toml]  a plain file or directory (the Nodes tab)
//! [setting:pools.claude_code.budget.weekly.threshold_pct]
//!                         a setting (the Config tab): its key, or the key
//!                         prefix a band governs (`pools.claude_code`)
//! ```
//!
//! **Chips** (design 0138 §4.3): a PASTE in the bucket is a tag too, stored
//! as it is drawn, so the bucket's text, its caret and its wrap are the
//! composer's own and nothing is hidden (msg 235: every paste is one, and a
//! speech-to-text tool's words arrive as a paste — there is no dictation):
//!
//! ```text
//! [paste b21c: "fn main() {" 14 lines · 412 chars]               a paste
//! ```
//!
//! The four hex characters are the chip's id: its body lives beside the
//! dump (`store::Artifacts`). A chip is ONE unit to the caret and to
//! Backspace and Delete, as every tag is.
//!
//! `node` means a SOUP node — the corpus's word. A mesh member is a `device`
//! (it was `[node:…]` in iteration two; the soup's nodes took the word).

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tag {
    /// `[<item>]` — a soup item: `Q-1`, `Q-1.opt3`, `T-4`, `M-2`, `OBJ-1`,
    /// `HA-1`, `HA-1.step2`, `DIR-1` or `CON-1`.
    Item { item: String },
    /// `[<kind>:<name>]` — a thing in the cauldron outside the soup.
    Thing { kind: String, name: String },
    /// `[<kind> <id><label>]` — a chip in the bucket: `kind` is `paste`,
    /// `id` its four hex characters, `label` what it shows (from its first
    /// `:` or space on, as drawn).
    Chip { kind: String, id: String, label: String },
}

/// The chip kinds.
pub const CHIPS: [&str; 1] = ["paste"];

impl Tag {
    pub fn item(item: impl Into<String>) -> Self {
        Tag::Item { item: item.into() }
    }

    pub fn thing(kind: &str, name: &str) -> Self {
        Tag::Thing { kind: kind.to_string(), name: name.to_string() }
    }

    pub fn render(&self) -> String {
        match self {
            Tag::Item { item } => format!("[{item}]"),
            // A soup node and a cluster are named by their ids alone
            // (0217 B6), exactly as an item is.
            Tag::Thing { kind, name } if (kind == "node" && is_node_id(name)) || (kind == "cluster" && is_cluster_id(name)) => format!("[{name}]"),
            Tag::Thing { kind, name } => format!("[{kind}:{name}]"),
            Tag::Chip { kind, id, label } => format!("[{kind} {id}{label}]"),
        }
    }

    pub fn chip(kind: &str, id: &str, label: &str) -> Self {
        // A chip's label never holds a bracket: it would end the tag.
        let label = label.replace('[', "(").replace(']', ")");
        Tag::Chip { kind: kind.to_string(), id: id.to_string(), label }
    }

    /// `(kind, id)` when this is a chip.
    pub fn chip_id(&self) -> Option<(&str, &str)> {
        match self {
            Tag::Chip { kind, id, .. } => Some((kind, id)),
            _ => None,
        }
    }

    /// The soup item this tag names (`Q-1.opt3`), if it names one.
    pub fn item_id(&self) -> Option<&str> {
        match self {
            Tag::Item { item } => Some(item),
            Tag::Thing { .. } | Tag::Chip { .. } => None,
        }
    }
}

fn is_key(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_item(s: &str) -> bool {
    // `Q-1`, `T-12`, `HA-3`; a question's `.opt<n>`, a human action's `.step<n>`.
    let (base, opt) = match s.split_once('.') {
        Some((b, o)) => (b, Some(o)),
        None => (s, None),
    };
    let ok_base = match base.split_once('-') {
        Some((p, n)) => {
            matches!(p, "Q" | "T" | "HA" | "M" | "OBJ" | "DIR" | "CON") && !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    };
    let ok_opt = match opt {
        None => true,
        Some(o) => {
            let digits = |n: &str| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit());
            (base.starts_with("Q-") && o.strip_prefix("opt").is_some_and(digits))
                || (base.starts_with("HA-") && o.strip_prefix("step").is_some_and(digits))
        }
    };
    ok_base && ok_opt
}

fn is_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The `[<kind>:<name>]` kinds.
pub const KINDS: [&str; 14] = ["service", "device", "region", "node", "cluster", "msg", "repo", "ripple", "run", "event", "setting", "send", "dump", "path"];

/// A minted cluster id: `X-<n>`.
pub fn is_cluster_id(s: &str) -> bool {
    s.strip_prefix("X-").is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// A soup node's id: an upper-case prefix, then `-` and its name
/// (`IS-12`, `ART-1`, `WT-3`, `MOD-tui-4f1c9a02`). A region (`R-3`) and an
/// event (`EV-12`) keep their kind: they are named that way on their pages.
pub fn is_node_id(s: &str) -> bool {
    let Some((p, rest)) = s.split_once('-') else { return false };
    !p.is_empty()
        && p.len() <= 8
        && p.chars().all(|c| c.is_ascii_uppercase())
        && !matches!(p, "R" | "EV" | "X" | "SUB")
        && !rest.is_empty()
        && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !rest.ends_with('-')
}

/// A dump is `<n>`, a note of it `<n>.<m>`.
fn is_dump(s: &str) -> bool {
    let digits = |n: &str| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit());
    match s.split_once('.') {
        Some((d, n)) => digits(d) && digits(n),
        None => digits(s),
    }
}

/// A message is `<thread>#<n>`.
fn is_msg(s: &str) -> bool {
    match s.split_once('#') {
        Some((t, n)) => is_name(t) && !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()),
        None => false,
    }
}

/// Parse the inside of one `[...]`.
fn parse_inner(inner: &str) -> Option<Tag> {
    // A chip: `paste b21c: …`.
    for kind in CHIPS {
        if let Some(rest) = inner.strip_prefix(kind).and_then(|r| r.strip_prefix(' ')) {
            let id: String = rest.chars().take_while(char::is_ascii_hexdigit).collect();
            let label = &rest[id.len()..];
            let ok = id.len() == 4 && id.chars().all(|c| c.is_ascii_digit() || c.is_ascii_lowercase()) && (label.is_empty() || label.starts_with([':', ' ']));
            return ok.then(|| Tag::Chip { kind: kind.into(), id, label: label.into() });
        }
    }
    // `[path:~/code/x]`: a path may hold `/`, `~` and spaces are not in one.
    if let Some(name) = inner.strip_prefix("path:") {
        let ok = !name.is_empty() && !name.contains(char::is_whitespace);
        return ok.then(|| Tag::thing("path", name));
    }
    // `[run:r-8f3a/author]`: a run's step carries a `/` after its kind.
    if let Some(name) = inner.strip_prefix("run:") {
        let mut parts = name.split('/');
        let ok = parts.next().is_some_and(is_name) && parts.next().is_none_or(is_name) && parts.next().is_none();
        return ok.then(|| Tag::thing("run", name));
    }
    // `[Q-1]`, `[HA-1.step2]`: a soup item, by its soup id.
    if is_item(inner) {
        return Some(Tag::item(inner));
    }
    // `[X-3]` a cluster, `[IS-12]` any other soup node: bare (0217 B6).
    if is_cluster_id(inner) {
        return Some(Tag::thing("cluster", inner));
    }
    if is_node_id(inner) {
        return Some(Tag::thing("node", inner));
    }
    if inner.contains('/') {
        return None;
    }
    if let Some((kind, name)) = inner.split_once(':') {
        let ok = match kind {
            "msg" => is_msg(name),
            "send" => !name.is_empty() && name.chars().all(|c| c.is_ascii_digit()),
            "dump" => is_dump(name),
            "cluster" => is_cluster_id(name),
            "repo" => is_key(name),
            _ => is_name(name),
        };
        if KINDS.contains(&kind) && ok {
            return Some(Tag::thing(kind, name));
        }
    }
    None
}

/// Every tag in `text`, with its BYTE range, in order.
pub fn scan(text: &str) -> Vec<(std::ops::Range<usize>, Tag)> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            if let Some(close) = text[i + 1..].find(']') {
                let end = i + 1 + close;
                let inner = &text[i + 1..end];
                if !inner.contains('[') {
                    if let Some(tag) = parse_inner(inner) {
                        out.push((i..end + 1, tag));
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_rejects() {
        let tags = [
            Tag::item("Q-1"),
            Tag::item("Q-12.opt3"),
            Tag::item("T-2"),
            Tag::item("HA-1"),
            Tag::item("HA-1.step2"),
            Tag::thing("service", "tts"),
            Tag::thing("device", "example-mac"),
            Tag::thing("region", "R-3"),
            Tag::thing("region", "SUB-4"),
            Tag::thing("node", "IS-12"),
            Tag::item("M-2"),
            Tag::item("OBJ-1"),
            Tag::thing("cluster", "X-3"),
            Tag::thing("msg", "8e4fca10#21"),
            Tag::thing("repo", "harness"),
            Tag::thing("node", "MOD-tui-4f1c9a02"),
            Tag::thing("ripple", "adjudicate"),
            Tag::thing("run", "r-8f3a"),
            Tag::thing("run", "r-8f3a/author"),
            Tag::thing("event", "EV-12"),
            Tag::thing("setting", "pools.claude_code.budget.weekly.threshold_pct"),
            Tag::thing("dump", "12"),
            Tag::thing("dump", "12.3"),
            Tag::thing("path", "~/code/cauldron/Cargo.toml"),
            Tag::chip("paste", "b21c", ": \"fn main() {\" 14 lines · 412 chars"),
        ];
        let text: String = tags.iter().map(|t| format!("x {} y", t.render())).collect();
        let found: Vec<Tag> = scan(&text).into_iter().map(|(_, t)| t).collect();
        assert_eq!(found, tags);
        // msg 235: a reference carries no cluster (one soup), and a cluster
        // is its minted id, never a slug of its name.
        for bad in ["[cauldron/Q-1]", "[Q-]", "[X-]", "[R-3]", "[ab-1]", "[IS-]", "[T-1.opt2]", "[x:y]", "[]", "[msg:8e4fca10]", "[msg:8e4fca10#x]", "[cluster:cauldron]", "[cluster:X-]", "[cluster:\"Ripple trials\"]", "[M-2.opt1]", "[run:r-1/a/b]", "[run:/x]", "[setting:a/b]", "[setting:]", "[Q-1.step1]", "[HA-1.opt1]", "[thread:8e4fca10]"] {
            assert!(scan(bad).is_empty(), "{bad} parsed");
        }
        for bad in ["[audio a7f3: x]", "[paste b21]", "[paste b21cx]", "[paste B21C: x]", "[paste]", "[dump:1.]", "[dump:x]", "[path:]", "[path:a b]"] {
            assert!(scan(bad).is_empty(), "{bad} parsed");
        }
        assert_eq!(Tag::chip("paste", "b21c", ": \"a[0]\"").render(), "[paste b21c: \"a(0)\"]", "no bracket inside a chip");
        // 0217 B6: a node and a cluster render bare; the old prefixed
        // shape still reads, as the same tag.
        assert_eq!(Tag::thing("node", "ART-1").render(), "[ART-1]");
        assert_eq!(Tag::thing("cluster", "X-2").render(), "[X-2]");
        assert_eq!(Tag::thing("region", "R-3").render(), "[region:R-3]");
        for (old, new) in [("[node:ART-1]", "[ART-1]"), ("[cluster:X-2]", "[X-2]"), ("[node:VIO-second-interface-3b6d8e1f]", "[VIO-second-interface-3b6d8e1f]")] {
            assert_eq!(scan(old)[0].1, scan(new)[0].1, "{old}");
        }
        let s = "a [Q-2] b";
        assert_eq!(scan(s)[0].0, 2..7);
    }
}
