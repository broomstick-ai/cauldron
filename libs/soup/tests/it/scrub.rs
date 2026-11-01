//! No string in the dataset matches a home path, an e-mail address, the
//! operator's personal names (private pattern file) or a secret pattern; and the guard does find each of
//! them when one is planted.

use std::path::{Path, PathBuf};

use soup_seed::demo::all_files;
use soup_seed::scrub::{load_personal, violations, violations_with, Personal};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// True in the PRIVATE tree: some path `.gitattributes` export-ignores is on
/// disk. A public checkout (a `git archive` cut) has none of them.
fn is_private_tree(root: &Path) -> bool {
    let attrs = std::fs::read_to_string(root.join(".gitattributes")).unwrap_or_default();
    attrs.lines().filter(|l| l.contains("export-ignore") && !l.trim_start().starts_with('#')).any(|l| {
        let p = l.split_whitespace().next().unwrap_or("").trim_start_matches('/');
        !p.is_empty() && !p.contains('*') && !p.ends_with("personal-patterns.private") && root.join(p).exists()
    })
}

/// The personal names to scrub, from the private pattern file the public tree
/// does not ship. FAILS CLOSED in the private tree when the file is missing;
/// `None` (skip) only in a stranger's checkout.
fn personal() -> Option<(Vec<Personal>, Vec<(String, String)>)> {
    let root = repo_root();
    match load_personal(&root.join("scripts/personal-patterns.private")) {
        Ok(Some(p)) => Some(p),
        Ok(None) if is_private_tree(&root) => {
            panic!("private tree without scripts/personal-patterns.private: the personal-name scrub cannot run")
        }
        Ok(None) => {
            eprintln!("skipping personal-name scrub: no private pattern file (public checkout)");
            None
        }
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn the_dataset_carries_no_personal_data_or_secret() {
    let personal = personal().map(|p| p.0).unwrap_or_default();
    let mut hits = Vec::new();
    for f in all_files() {
        for v in violations_with(f.text, &personal) {
            hits.push(format!("{}: {} at byte {}", f.name, v.class, v.at));
        }
    }
    assert!(hits.is_empty(), "the curated dataset is not scrubbed:\n{}", hits.join("\n"));
}

/// The guard must be able to find what the test above says is absent: each
/// class, planted alone in otherwise clean text, is found as that class.
#[test]
fn every_class_is_found_when_planted() {
    // Each plant is assembled at run time, so this file never holds one whole.
    let plants: Vec<(String, &str)> = vec![
        (format!("open {}x/notes.md", "/Users/"), "home-path"),
        (format!("write to {}{}{}", "someone", "@", "example.org"), "email"),
        (format!("key {}{}", "sk-ant-", "api03-AbCdEfGhIjKlMnOp"), "secret:anthropic"),
        (format!("key {}{}", "sk-", "abcdefghijklmnopqrstuvwx1234"), "secret:api-key"),
        (format!("{}{}", "ghp_", "abcdefghijklmnopqrstuvwxyz0123456789"), "secret:github"),
        (format!("{}{}", "xoxb-", "123456789012-abcdefghij"), "secret:slack"),
        (format!("{}{}", "AKIA", "ABCDEFGHIJKLMNOP"), "secret:aws"),
        (format!("{}{}", "AIza", "SyA1234567890abcdefghijklmnopqrstuv"), "secret:google"),
        (format!("-----BEGIN {} KEY-----", "OPENSSH PRIVATE"), "secret:private-key"),
        (format!("{}: {}", "password", "hunter2hunter2xyz9"), "secret:assignment"),
    ];
    for (text, class) in &plants {
        let v = violations(text);
        assert!(v.iter().any(|x| x.class == *class), "planted {class} was not found (found {v:?})");
    }
    // The personal names: each sample of the private file is found as its kind.
    if let Some((pats, samples)) = personal() {
        assert!(!samples.is_empty(), "the private pattern file holds no sample");
        for (kind, text) in &samples {
            let v = violations_with(text, &pats);
            let want = format!("personal:{kind}");
            assert!(v.iter().any(|x| x.class == want), "sample of kind {kind} was not found (found {v:?})");
        }
    }
}

/// Opposes the plants: the words a human-action fence and a curated cauldron
/// legitimately use are NOT violations, so a guard that flags everything fails.
#[test]
fn clean_text_is_not_flagged() {
    let clean = "secret: false\nroute: secret-store\nname: RELEASE_TOKEN\nlabel: Release token\n\
                 token: short\nweekly 80% on every account\nthe user at home\n/usr/local/bin\nactive (+2)\n";
    let v = violations(clean);
    assert!(v.is_empty(), "clean text flagged: {v:?}");
}
