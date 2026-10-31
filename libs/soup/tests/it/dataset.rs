//! The curated dataset loads, and shows every kind the demo must show: ONE
//! test per kind, each forced red by removing that kind from the dataset
//! (REL-3.S1 report, guards).

use soup::demo::{Dataset, Kind};

fn present(k: Kind) {
    let ds = Dataset::embedded().expect("the embedded dataset parses");
    let census = ds.census();
    assert!(census.contains(&k), "the curated dataset shows no {k:?}; it shows {census:?}");
}

macro_rules! kind_tests {
    ($($name:ident => $kind:ident),* $(,)?) => {
        $( #[test] fn $name() { present(Kind::$kind) } )*

        /// The list above names every kind: a kind added to `Kind::ALL`
        /// without its own test fails here.
        #[test]
        fn every_kind_has_its_own_test() {
            let tested = [$(Kind::$kind),*];
            for k in Kind::ALL {
                assert!(tested.contains(&k), "{k:?} has no presence test");
            }
        }
    };
}

kind_tests! {
    shows_clusters => Clusters,
    shows_the_aggregate_cluster => AggregateCluster,
    shows_questions => Questions,
    shows_an_assumed_question => AssumedQuestion,
    shows_actions => Actions,
    shows_a_multi_phase_action => MultiPhaseAction,
    shows_a_secret_action => SecretAction,
    shows_directives => Directives,
    shows_constraints => Constraints,
    shows_objectives => Objectives,
    shows_milestones => Milestones,
    shows_tasks => Tasks,
    shows_dumps => Dumps,
    shows_ledger_phases => LedgerPhases,
    shows_a_ripple_tree => RippleTree,
    shows_a_thread_fold => ThreadFold,
    shows_a_rolled_back_tranche => RolledBackTranche,
    shows_opening_a_file => OpenFile,
}

/// Opposes every presence test at once: a census that answers "present" for
/// anything (a constant) finds every kind in an empty dataset and fails here.
#[test]
fn an_empty_dataset_shows_no_kind() {
    let ds = Dataset::parse(r#"{"clusters":[]}"#, r#"{"artifacts":[]}"#, r#"{"dumps":[]}"#, []).unwrap();
    assert!(ds.census().is_empty(), "an empty dataset was counted as showing {:?}", ds.census());
}

/// A one-phase action is not multi-phase, and an unmasked or unrouted field is
/// not a secret action: the two predicates discriminate, they do not merely
/// detect an action.
#[test]
fn a_plain_action_is_neither_multi_phase_nor_secret() {
    let body = "Do it.\n\n```field\nname: X\nsecret: false\nroute: env-file\n```\n\n```field\nname: Y\n```\n\n**Deliverable:** done.\n";
    let clusters = serde_json::json!({"clusters": [{"id": "X-1", "name": "a", "description": "",
        "human_actions": [{"serial": 1, "ask": "a", "status": "open", "filed": "2026-10-30", "body": body}]}]});
    let ds = Dataset::parse(&clusters.to_string(), r#"{"artifacts":[]}"#, r#"{"dumps":[]}"#, []).unwrap();
    let c = ds.census();
    assert!(c.contains(&Kind::Actions), "{c:?}");
    assert!(!c.contains(&Kind::MultiPhaseAction), "one phase counted as multi-phase: {c:?}");
    assert!(!c.contains(&Kind::SecretAction), "an unmasked field, or a masked one with no route, counted as secret: {c:?}");
}

/// The phase rule is the human-action format's: a `##` section is a phase
/// when it has a `field` fence or a `Deliverable:` line; prose is not.
#[test]
fn phases_follow_the_human_action_format() {
    let body = "intro prose\n\n## A\n\n**Deliverable:** a.\n\n## prose only\n\ntext\n\n## B\n\n```field\nname: K\nroute: secret-store\n```\n\n## C\n\n```text\nDeliverable: inside a fence is text\n```\n";
    let p = soup::demo::phases(body);
    assert_eq!(p.len(), 2, "{p:?}");
    assert!(p[0].is_empty());
    assert_eq!(p[1], vec![vec![("name".to_string(), "K".to_string()), ("route".to_string(), "secret-store".to_string())]]);
}
