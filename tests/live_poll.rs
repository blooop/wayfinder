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

    // Cycle 1: no stored ETag, so the probe 200s and the full GraphQL
    // fetch reruns — must yield a real map, never Failed.
    match poller.poll_once().await {
        RefreshEvent::Updated(map) => assert_plausible_map(&map),
        other => panic!("first poll should fetch the map, got {other:?}"),
    }

    // Cycle 2: the stored ETag makes this conditional. Unchanged (304) if
    // the tracker sat still between polls, Updated if it moved — either is
    // healthy; only Failed is a bug.
    match poller.poll_once().await {
        RefreshEvent::Unchanged => {}
        RefreshEvent::Updated(map) => assert_plausible_map(&map),
        RefreshEvent::Failed => panic!("second (conditional) poll failed"),
    }
}
