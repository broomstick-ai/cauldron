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
    assert_eq!(validate_action(UNROUTED), Err(Reject::Unrouted { field: "API_TOKEN".into() }));
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
