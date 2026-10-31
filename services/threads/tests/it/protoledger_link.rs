//! R6.S1 (protoledger compatibility check, R-1..R-7): proves `protoledger`
//! (no default features + `store`, `sync`, `sqlite`; PL-0 pin) is linked into this crate's test
//! build and its store runs against the same unified `rusqlite`/`serde_json`
//! that `threads` itself uses. Deliberately the only code this check adds.

use protoledger::{Agent, Store};

fn agent(id: &str) -> Agent {
    Agent { id: id.into(), created_at: None, source: None, parent: None, links: vec![], meta: Default::default() }
}

#[test]
fn protoledger_store_round_trips_one_agent_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut s = Store::open(dir.path()).unwrap();
        assert!(s.agents().unwrap().is_empty(), "a fresh store must hold no agents");
        s.create_agent(agent("pl-compat")).unwrap();
    }
    // Reopen: the agent must come back from disk, not from the first handle.
    let s = Store::open(dir.path()).unwrap();
    let ids: Vec<String> = s.agents().unwrap().into_iter().map(|a| a.id).collect();
    assert_eq!(ids, vec!["pl-compat".to_string()]);
    let info = s.agent("pl-compat").unwrap().expect("agent indexed");
    assert_eq!(info.path, "ledgers/pl-compat.jsonl");
    assert!(dir.path().join(&info.path).is_file(), "ledger file written");
}
