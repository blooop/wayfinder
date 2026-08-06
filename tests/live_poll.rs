//! Integration test for the background refresh loop against the real
//! tracker: two consecutive poll cycles on blooop/wayfinder map #1. Needs
//! network and an authenticated `gh`.

use wf::model::Map;
use wf::refresh::{Poller, RefreshEvent};

fn assert_plausible_map(map: &Map) {
    assert_eq!(map.repo, "blooop/wayfinder");
    assert!(
        map.tickets.len() >= 7,
        "expected the real map's tickets, got {}",
        map.tickets.len()
    );
}

#[tokio::test]
async fn two_consecutive_polls_succeed() {
    let mut poller = Poller::new("blooop", "wayfinder", 1);

    // Cycle 0 is the cold start, and since #27 it *is* the initial load: no
    // ETag exists, so the probe is skipped and the GraphQL fetch runs outright
    // — one round trip, and it must yield a real map, never Failed.
    match poller.poll_once().await {
        RefreshEvent::Updated(map) => assert_plausible_map(&map),
        other => panic!("first poll should fetch the map, got {other:?}"),
    }

    // Cycles 1 and 2 take the probe path. Cycle 1 has no ETag yet (the forced
    // fetch above is REST-free, so it stores none) and 200s into a fetch;
    // cycle 2 is the genuinely conditional one. Unchanged (304) if the tracker
    // sat still, Updated if it moved — either is healthy; only Failed is a bug.
    for cycle in 1..=2 {
        match poller.poll_once().await {
            RefreshEvent::Unchanged => {}
            RefreshEvent::Updated(map) => assert_plausible_map(&map),
            RefreshEvent::Failed => panic!("probe-path poll {cycle} failed"),
        }
    }
}
