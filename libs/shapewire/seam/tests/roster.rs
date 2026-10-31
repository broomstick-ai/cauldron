//! **`view::roster` IS THE WHOLE `threads/non-idle` ROSTER, AND `running` IS NOT.**
//!
//! Replayed off the PRODUCER'S OWN recording, `conformance/fleet_non_idle.jsonl`
//! (three threads parked on `dev-ratelimited`, two executing on `dev-healthy`),
//! so the counts below are what a real daemon emitted rather than what this file
//! believes. The two readers must DISAGREE on this frame — 5 rows against 2 — so
//! neither can be a copy of the other: a `roster` that returned `running`'s rows
//! is red on the count, and a `roster` that invented five rows is red on the
//! state words and the placements, which are the frame's.

use std::path::Path;

use seam_reference::held::{Held, LinkState};
use seam_reference::view;
use serde_json::Value;

fn folded() -> Held {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance/fleet_non_idle.jsonl");
    let text = std::fs::read_to_string(&path).expect("the recorded transcript");
    let mut h = Held::new();
    h.link = LinkState::Up {
        boot_id: "transcript".into(),
        since: "2026-10-07T00:00:00Z".parse().expect("a fixed stamp"),
    };
    let mut frames = 0;
    for raw in text.lines() {
        let v: Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(f) = v.get("frame") {
            h.fold(f);
            frames += 1;
        }
    }
    assert!(frames > 0, "the transcript carried no frame, so nothing below is evidence");
    h
}

#[test]
fn the_roster_holds_every_non_idle_thread_and_running_only_the_executing_ones() {
    let h = folded();
    let roster = view::roster(&h).known().expect("the roster frame is known");
    let running = view::running(&h).known().expect("running is known off the same frame");

    assert_eq!(running.len(), 2, "the recording executes two threads");
    assert_eq!(roster.len(), 5, "the recording names five non-idle threads");

    // The state words and placements are the FRAME's, so five rows made up from
    // nothing cannot pass.
    let parked: Vec<&String> = roster
        .iter()
        .filter(|(_, r)| r.state == "parked" && !r.executing && r.account == "dev-ratelimited")
        .map(|(id, _)| id)
        .collect();
    assert_eq!(
        parked,
        ["dev-parked-00", "dev-parked-01", "dev-parked-02"],
        "the three parked rows, as recorded"
    );
    for (id, account) in &running {
        let r = roster.get(id).unwrap_or_else(|| panic!("`{id}` runs but is not in the roster"));
        assert_eq!(&r.account, account, "one placement, read twice");
        assert!(r.executing, "`{id}` is in `running` and its roster row says not executing");
        assert_eq!(r.state, "active");
        assert_eq!(r.queued, 3, "the recording queues three on each foreign-held thread");
    }
}

#[test]
fn no_roster_frame_is_no_roster_not_an_empty_one() {
    let h = Held::new();
    assert!(
        !view::roster(&h).is_known(),
        "nothing has arrived, so the roster is unknown — an empty map would say the fleet is idle"
    );
}
