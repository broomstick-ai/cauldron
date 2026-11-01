//! The write-time check (V1.8 §2): a secret field with no destination never
//! enters the soup. The NON-routed secret action lives here, as a negative
//! fixture, never in the shipped dataset.

use soup::actions::{validate_action, Reject};
use soup::demo::Dataset;

fn fence(lines: &str) -> String {
    format!("## Step\n\n```field\n{lines}\n```\n")
}

/// The negative fixture: a multi-phase action whose secret step names no route.
const UNROUTED: &str = "## Step 1 — make it\n\n**Deliverable:** made.\n\n## Step 2 — paste it\n\n```field\nname: API_TOKEN\nlabel: The token\n```\n";

#[test]
fn an_unrouted_secret_field_is_refused() {
    assert_eq!(validate_action(UNROUTED), Err(Reject::NoRoute { field: "API_TOKEN".into() }));
}

#[test]
fn a_secret_routed_to_the_node_is_refused_and_a_plain_one_is_not() {
    assert_eq!(validate_action(&fence("name: K\nroute: node")), Err(Reject::SecretToNode { field: "K".into() }));
    assert_eq!(validate_action(&fence("name: K\nsecret: false\nroute: node")), Ok(()));
}

#[test]
fn an_unknown_route_is_refused() {
    for r in ["secret-store", "env:", "env:NOT-A-NAME", "file"] {
        assert!(matches!(validate_action(&fence(&format!("name: K\nroute: {r}"))), Err(Reject::UnknownRoute { .. })), "{r}");
    }
}

/// Opposes the refusals: every legal route is accepted, so the validator is not
/// a constant refusal.
#[test]
fn every_legal_route_is_accepted() {
    for r in ["env", "env:RELEASE_TOKEN", "keychain"] {
        assert_eq!(validate_action(&fence(&format!("name: K\nroute: {r}"))), Ok(()), "{r}");
    }
    assert_eq!(validate_action(&fence("name: K\nsecret: false")), Ok(()), "a plain field needs no route");
}

/// V1.8.S4: two fields with one name are refused (both copies, so the
/// first in the body's order is named), and so are two fields routed to
/// one env variable; opposed by the near misses that are accepted.
#[test]
fn duplicate_names_and_shared_env_variables_are_refused() {
    let two = |a: &str, b: &str| format!("## S\n\n```field\n{a}\n```\n\n```field\n{b}\n```\n");
    let dup = |f: &str| Err(Reject::DuplicateName { field: f.into() });
    assert_eq!(validate_action(&two("name: K\nroute: env", "name: K\nroute: keychain")), dup("K"));
    assert_eq!(validate_action(&two("name: K\nroute: keychain", "name: K\nroute: keychain")), dup("K"));
    assert_eq!(validate_action(&two("name: A\nroute: env:PAY", "name: B\nroute: env:PAY")), Err(Reject::SharedEnv { field: "A".into(), other: "B".into(), var: "PAY".into() }));
    assert_eq!(validate_action(&two("name: A\nroute: env:B", "name: B\nroute: env")), Err(Reject::SharedEnv { field: "A".into(), other: "B".into(), var: "B".into() }));
    // Accepted: names and variables compare exactly, so `K`/`k` and `PAY`/`pay` differ.
    assert_eq!(validate_action(&two("name: K\nroute: env", "name: k\nroute: env")), Ok(()));
    assert_eq!(validate_action(&two("name: A\nroute: env:PAY", "name: B\nroute: env:pay")), Ok(()));
    assert_eq!(validate_action(&two("name: A\nroute: keychain", "name: B\nroute: env:A")), Ok(()));
    assert_eq!(validate_action(&two("name: A\nsecret: false\nroute: node", "name: B\nsecret: false\nroute: node")), Ok(()));
}

/// A dataset holding the unrouted action fails its load, loudly, naming the
/// action and the field; the same dataset with the step routed loads.
#[test]
fn a_dataset_with_an_unroutable_action_fails_to_load() {
    let with = |body: &str| {
        serde_json::json!({"clusters": [{"id": "X-1", "name": "a", "description": "",
            "human_actions": [{"serial": 7, "ask": "a", "status": "open", "filed": "2026-10-30", "body": body}]}]})
        .to_string()
    };
    let bad = Dataset::parse(&with(UNROUTED), r#"{"artifacts":[]}"#, r#"{"dumps":[]}"#, []).unwrap();
    let err = format!("{:#}", bad.validate_actions().unwrap_err());
    assert!(err.contains("HA-7") && err.contains("API_TOKEN"), "{err}");
    let good = Dataset::parse(&with(&UNROUTED.replace("label: The token", "label: The token\nroute: env")), r#"{"artifacts":[]}"#, r#"{"dumps":[]}"#, []).unwrap();
    good.validate_actions().expect("the routed action loads");
}

/// A fresh directory under the system temp dir, removed on drop.
struct Tmp(std::path::PathBuf);

impl Tmp {
    fn new(tag: &str) -> Tmp {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let p = std::env::temp_dir().join(format!("soup-agree-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The SOUP WRITE PATH's verdict on `body`: the embedded dataset with one
/// more open human action holding `body`, through `install_from` into a
/// fresh directory. A refusal must have written nothing; an acceptance must
/// have written the action.
fn write_path_says(i: usize, body: &str) -> Result<(), Reject> {
    use soup::demo::{install_from, Source};
    let mut clusters: serde_json::Value = serde_json::from_str(Source::EMBEDDED.clusters).unwrap();
    let has = clusters["clusters"][0]["human_actions"].as_array_mut().expect("cluster 0 holds human actions");
    has.push(serde_json::json!({"serial": 990 + i, "ask": "agreement probe", "status": "open", "filed": "2026-10-31", "body": body}));
    let clusters = clusters.to_string();
    let t = Tmp::new("write");
    let soup_dir = t.0.join("tui").join("soup");
    match install_from(&soup_dir, &Source { clusters: &clusters, ..Source::EMBEDDED }) {
        Ok(got) => {
            let written = std::fs::read_to_string(got.seed_dir.join("clusters.json")).unwrap();
            assert!(written.contains(&format!("\"serial\":{}", 990 + i)), "shape {i}: install said ok and did not write the action");
            Ok(())
        }
        Err(e) => {
            let left: Vec<_> = std::fs::read_dir(&t.0).unwrap().map(|e| e.unwrap().path()).collect();
            assert!(left.is_empty(), "shape {i}: a refused install wrote {left:?}");
            Err(e.chain().find_map(|c| c.downcast_ref::<Reject>()).cloned().unwrap_or_else(|| panic!("shape {i}: refused, but not by the validator: {e:#}")))
        }
    }
}

/// THE FACE'S DISPLAY verdict on `body`: the fields the Actions tab draws
/// (`tickets::parse`), each through the face's display check.
fn face_says(body: &str) -> Result<(), Reject> {
    use tui::cauldron::tickets;
    match tickets::parse(body).steps.iter().flat_map(|s| s.fields().cloned().collect::<Vec<_>>()).find_map(|f| tickets::field_reject(&f)) {
        Some(r) => Err(r),
        None => Ok(()),
    }
}

/// V1.8.S3, "one PARSER": every body below goes through the soup's WRITE
/// PATH (`install_from`, which stores or refuses) and through the FACE's
/// DISPLAY (the fields `tickets::parse` draws, each checked), and the two
/// verdicts are the same, and are the one written beside the body. The face
/// draws exactly the fields the soup parsed.
///
/// The corpus holds acceptance and all five refusals, the two shapes on
/// which the S2 pair disagreed (V1.8.S2-I F1: a field fence left open at the
/// end of the body; a `route:` below a `---` line), and the boundary shapes
/// beside them: CRLF, nested fences, duplicate keys, a route with trailing
/// whitespace; and (V1.8.S4) two fields with one name or on one env
/// variable, beside their accepted near misses. Each risky shape is paired with its accepted twin, so a gate
/// that refused everything, or nothing, fails here.
#[test]
fn the_face_and_the_write_path_give_one_verdict() {
    use tui::cauldron::tickets;
    let no = |f: &str| Err(Reject::NoRoute { field: f.into() });
    let node = |f: &str| Err(Reject::SecretToNode { field: f.into() });
    let unknown = |f: &str, r: &str| Err(Reject::UnknownRoute { field: f.into(), route: r.into() });
    let dup = |f: &str| Err(Reject::DuplicateName { field: f.into() });
    let shared = |f: &str, o: &str, v: &str| Err(Reject::SharedEnv { field: f.into(), other: o.into(), var: v.into() });
    let two = |a: &str, b: &str| format!("## S\n\n```field\n{a}\n```\n\n```field\n{b}\n```\n");
    let ok: Result<(), Reject> = Ok(());
    let corpus: Vec<(&str, String, Result<(), Reject>)> = vec![
        // The S2 corpus.
        ("multi-phase unrouted", UNROUTED.to_string(), no("API_TOKEN")),
        ("secret to node", fence("name: K\nroute: node"), node("K")),
        ("plain to node", fence("name: K\nsecret: false\nroute: node"), ok.clone()),
        ("unknown route", fence("name: K\nroute: vault"), unknown("K", "vault")),
        ("bad env name", fence("name: K\nroute: env:NOT-A-NAME"), unknown("K", "env:NOT-A-NAME")),
        ("env: empty", fence("name: K\nroute: env:"), unknown("K", "env:")),
        ("empty route", fence("name: K\nroute:"), no("K")),
        ("env", fence("name: K\nroute: env"), ok.clone()),
        ("env renamed", fence("name: K\nroute: env:RELEASE_TOKEN"), ok.clone()),
        ("keychain", fence("name: K\nroute: keychain"), ok.clone()),
        ("plain, no route", fence("name: K\nsecret: false"), ok.clone()),
        ("intro field, then unrouted", "intro\n\n```field\nname: A\nroute: env\n```\n\n## Two\n\n```field\nname: B\n```\n".into(), no("B")),
        ("no field", "## Do it\n\n**Deliverable:** done.\n".into(), ok.clone()),
        // F1, shape 1: a field fence left open at the end of the body.
        ("open at EOF, unrouted", "## Step\n\n```field\nname: K\nlabel: k\n".into(), no("K")),
        ("open at EOF, routed", "## Step\n\n```field\nname: K\nroute: env\n".into(), ok.clone()),
        ("open at EOF, no newline", "## Step\n\n```field\nname: K".into(), no("K")),
        // F1, shape 2: keys below a `---` line are payload, not header.
        ("route below ---", fence("name: K\n---\nroute: env"), no("K")),
        ("route above ---", fence("name: K\nroute: env\n---\nroute: node"), ok.clone()),
        ("secret: false below ---", fence("name: K\nroute: node\n---\nsecret: false"), node("K")),
        // CRLF line endings.
        ("CRLF unrouted", "## Step\r\n\r\n```field\r\nname: K\r\n```\r\n".into(), no("K")),
        ("CRLF routed", "## Step\r\n\r\n```field\r\nname: K\r\nroute: keychain\r\n```\r\n".into(), ok.clone()),
        ("CRLF plain to node", "## Step\r\n\r\n```field\r\nname: K\r\nsecret: false\r\nroute: node\r\n```\r\n".into(), ok.clone()),
        ("CRLF route below ---", "## Step\r\n\r\n```field\r\nname: K\r\n---\r\nroute: env\r\n```\r\n".into(), no("K")),
        // Nested fences: any line starting ``` closes the open fence.
        ("field closed by a field opener", "## S\n\n```field\nname: A\n```field\nname: B\nroute: env\n```\n".into(), no("A")),
        ("field inside a text fence", "## S\n\n```text\nintro\n```field\nname: K\n```\n".into(), ok.clone()),
        ("four-backtick opener is no field", "## S\n\n````field\nname: K\n````\n".into(), ok.clone()),
        ("field after a closed text fence", "## S\n\n```text\n```\n```field\nname: K\n```\n".into(), no("K")),
        // Duplicate keys: a key's FIRST line wins, even an empty one.
        ("dup route, empty first", fence("name: K\nroute:\nroute: env"), no("K")),
        ("dup route, node first", fence("name: K\nroute: node\nroute: env"), node("K")),
        ("dup route, env first", fence("name: K\nroute: env\nroute: node"), ok.clone()),
        ("dup secret, false first", fence("name: K\nsecret: false\nsecret: true\nroute: node"), ok.clone()),
        ("dup secret, true first", fence("name: K\nsecret: true\nsecret: false\nroute: node"), node("K")),
        ("dup name", fence("name: A\nname: B"), no("A")),
        // Trailing (and leading) whitespace around a route.
        ("route env + spaces", fence("name: K\nroute: env   "), ok.clone()),
        ("route keychain + tab", fence("name: K\nroute:\tkeychain\t"), ok.clone()),
        ("route env:NAME + spaces", fence("name: K\nroute: env: PAY_KEY  "), ok.clone()),
        ("route whitespace only", fence("name: K\nroute:    "), no("K")),
        ("route node + spaces", fence("name: K\nroute: node  "), node("K")),
        ("route vault + spaces", fence("name: K\nroute:  vault  "), unknown("K", "vault")),
        ("secret: false + spaces", fence("name: K\nsecret:  false \nroute: node"), ok.clone()),
        // V1.8.S4 (S3-I N1): two fields with one name; two fields on one env variable.
        ("dup field, env + keychain", two("name: K\nroute: env", "name: K\nroute: keychain"), dup("K")),
        ("dup field, both env", two("name: K\nroute: env", "name: K\nroute: env"), dup("K")),
        ("dup field, both keychain", two("name: K\nroute: keychain", "name: K\nroute: keychain"), dup("K")),
        ("dup field, second unrouted", two("name: K\nroute: env", "name: K"), dup("K")),
        ("dup field, first unrouted", two("name: K", "name: K\nroute: env"), no("K")),
        ("dup field, across phases", "## One\n\n```field\nname: K\nroute: env\n```\n\n## Two\n\n```field\nname: K\nroute: keychain\n```\n".into(), dup("K")),
        ("dup field, name + spaces", two("name: K\nroute: env", "name:  K \nroute: keychain"), dup("K")),
        ("names differ in case", two("name: K\nroute: env", "name: k\nroute: env"), ok.clone()),
        ("shared env:NAME", two("name: A\nroute: env:PAY", "name: B\nroute: env:PAY"), shared("A", "B", "PAY")),
        ("env:B beside env named B", two("name: A\nroute: env:B", "name: B\nroute: env"), shared("A", "B", "B")),
        ("shared env, spaces", two("name: A\nroute: env: PAY ", "name: B\nroute: env:PAY"), shared("A", "B", "PAY")),
        ("env vars differ in case", two("name: A\nroute: env:PAY", "name: B\nroute: env:pay"), ok.clone()),
        ("keychain A beside env:A", two("name: A\nroute: keychain", "name: B\nroute: env:A"), ok.clone()),
        ("two plain node fields", two("name: A\nsecret: false\nroute: node", "name: B\nsecret: false\nroute: node"), ok.clone()),
    ];
    let mut seen = std::collections::BTreeSet::new();
    for (i, (what, body, want)) in corpus.iter().enumerate() {
        let wrote = write_path_says(i, body);
        let face = face_says(body);
        assert_eq!(face, wrote, "[{what}] the face's display and the soup's write path disagree on:\n{body:?}");
        assert_eq!(&wrote, want, "[{what}] the write path's verdict is not the one expected on:\n{body:?}");
        assert_eq!(validate_action(body), wrote, "[{what}] validate_action is not the write path's verdict");
        // (No `tickets::validate_action` check here: it is a re-export of
        // `validate_action`, so comparing the two would be `x == x`.)
        // The face draws exactly the fields the soup parsed (the same type,
        // the same values): there is no second parser to differ.
        let drawn: Vec<soup::actions::Field> = tickets::parse(body).steps.iter().flat_map(|s| s.fields().cloned().collect::<Vec<_>>()).collect();
        assert_eq!(drawn, soup::actions::fields(body), "[{what}] the face draws other fields than the soup parsed");
        seen.insert(match &wrote {
            Ok(()) => "ok",
            Err(Reject::NoRoute { .. }) => "no-route",
            Err(Reject::SecretToNode { .. }) => "secret-to-node",
            Err(Reject::UnknownRoute { .. }) => "unknown-route",
            Err(Reject::DuplicateName { .. }) => "duplicate-name",
            Err(Reject::SharedEnv { .. }) => "shared-env",
        });
    }
    assert_eq!(seen.len(), 6, "the corpus must exercise acceptance and all five refusals: {seen:?}");
}
