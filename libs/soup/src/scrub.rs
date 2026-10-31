//! **The scrub guard**: what may never appear in the curated dataset.
//!
//! The dataset depicts cauldron's own development and ships in a public
//! repository, so every byte of it is checked for:
//!
//! - a macOS home path (`/Users/`);
//! - an e-mail address;
//! - the operator's personal names, read at check time from the private
//!   pattern file the public tree does not ship ([`load_personal`]);
//! - a secret: a known token prefix, a private key block, or a credential
//!   assigned a value.
//!
//! A hit is reported with its byte offset and its CLASS only. The matched text
//! is never echoed, so a planted or leaked secret is not printed by the guard
//! that found it.

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

/// One thing the guard found: its class and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// `home-path`, `email`, `personal:<kind>` or `secret:<kind>`.
    pub class: String,
    /// Byte offset of the match in the text checked.
    pub at: usize,
}

struct Rule {
    class: &'static str,
    re: Regex,
}

fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let r = |class: &'static str, pat: &str| Rule { class, re: Regex::new(pat).expect("scrub rule compiles") };
        vec![
            r("home-path", r"/Users/"),
            r("email", r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9\-]+(\.[A-Za-z0-9\-]+)*\.[A-Za-z]{2,}"),
            r("secret:anthropic", r"sk-ant-[A-Za-z0-9_\-]{8,}"),
            r("secret:api-key", r"\bsk-(?:proj-|or-)?[A-Za-z0-9_\-]{20,}"),
            r("secret:github", r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})"),
            r("secret:slack", r"\bxox[abprs]-[A-Za-z0-9\-]{10,}"),
            r("secret:aws", r"\bAKIA[0-9A-Z]{16}\b"),
            r("secret:google", r"\bAIza[0-9A-Za-z_\-]{35}"),
            r("secret:private-key", r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
            // A credential-named key assigned a long opaque value. A field
            // fence's `secret: false` or `route: secret-store` is not one: the
            // value must be 12+ characters with a digit in it.
            r(
                "secret:assignment",
                r#"(?i)\b(?:password|passwd|secret|token|api[_\-]?key|access[_\-]?key)\s*[:=]\s*["']?[A-Za-z0-9/+_\-.]*[0-9][A-Za-z0-9/+_\-.]*"#,
            ),
        ]
    })
}

/// One personal pattern: a kind (`name`, `home`, `email`, `bundle`, ...) and
/// its regex, matched case-insensitively.
pub struct Personal {
    pub kind: String,
    pub re: Regex,
}

/// The personal patterns and the positive samples of the private pattern file
/// (`scripts/personal-patterns.private`, tab-separated
/// `personal<TAB><kind><TAB><regex>` / `sample<TAB><kind><TAB><text>`). The
/// file is export-ignored, so the names it holds never ship in this source.
/// `Ok(None)` when the file is absent (a stranger's checkout); an unreadable
/// file or a bad regex is an error, never a silent skip.
pub fn load_personal(path: &Path) -> Result<Option<(Vec<Personal>, Vec<(String, String)>)>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("personal pattern file unreadable: {e}")),
    };
    let (mut pats, mut samples) = (Vec::new(), Vec::new());
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.splitn(3, '\t').collect();
        match f.as_slice() {
            ["personal", kind, pat] => pats.push(Personal {
                kind: kind.to_string(),
                re: Regex::new(&format!("(?i){pat}")).map_err(|_| format!("line {}: bad regex", n + 1))?,
            }),
            ["sample", kind, s] => samples.push((kind.to_string(), s.to_string())),
            _ => return Err(format!("line {}: not a pattern line", n + 1)),
        }
    }
    if pats.is_empty() {
        return Err("personal pattern file holds no pattern".into());
    }
    Ok(Some((pats, samples)))
}

/// [`violations`] plus every personal pattern, reported as `personal:<kind>`.
pub fn violations_with(text: &str, personal: &[Personal]) -> Vec<Violation> {
    let mut out = violations(text);
    for p in personal {
        for m in p.re.find_iter(text) {
            out.push(Violation { class: format!("personal:{}", p.kind), at: m.start() });
        }
    }
    out
}

/// Every violation in `text`, in rule order then position.
pub fn violations(text: &str) -> Vec<Violation> {
    let mut out = Vec::new();
    for rule in rules() {
        for m in rule.re.find_iter(text) {
            if rule.class == "secret:assignment" && !assignment_is_long(m.as_str()) {
                continue;
            }
            out.push(Violation { class: rule.class.to_string(), at: m.start() });
        }
    }
    out
}

/// The assigned value of a `key: value` match is 12 characters or more.
fn assignment_is_long(m: &str) -> bool {
    let value = m.split_once([':', '=']).map_or("", |(_, v)| v).trim().trim_start_matches(['"', '\'']);
    value.chars().count() >= 12
}
