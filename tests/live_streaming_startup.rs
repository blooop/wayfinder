//! Integration test for the streaming startup path (#27) against the real
//! tracker. Needs network and an authenticated `gh`.
//!
//! What it pins down is the *shape* `main` now relies on: nothing is fetched
//! before the loop exists, and everything the loop needs — which repos have
//! maps, and each map — arrives afterwards through one channel. Before #27 the
//! same information could only be had by awaiting it up front, which is exactly
//! why the terminal stayed blank for it.

use std::time::Duration;

use tokio::sync::mpsc;
use wf::refresh::{spawn_all, spawn_discovery, LoadEvent, Poller, RefreshEvent};

/// Take the next event, failing loudly rather than hanging a CI run forever.
async fn next(rx: &mut mpsc::UnboundedReceiver<LoadEvent>) -> LoadEvent {
    tokio::time::timeout(Duration::from_secs(30), rx.recv())
        .await
        .expect("timed out waiting for a load event")
        .expect("senders dropped")
}

#[tokio::test]
async fn discovery_then_every_map_arrives_through_one_channel() {
    let (tx, mut rx) = mpsc::unbounded_channel();

    // Nothing is awaited here: discovery runs on its own task, so a caller
    // could be drawing a screen instead of blocking on this.
    spawn_discovery(vec!["blooop/wayfinder".to_string()], tx.clone());

    let map_issues = loop {
        match next(&mut rx).await {
            LoadEvent::Discovered(found) => break found,
            // The search retries, so a blip is survivable rather than fatal.
            LoadEvent::SearchFailed => continue,
            other => panic!("expected discovery first, got {other:?}"),
        }
    };
    assert_eq!(
        map_issues.get("blooop/wayfinder"),
        Some(&1),
        "this repo's map is issue #1"
    );

    // Spawning the pollers *is* the initial load: no separate fetch happens
    // anywhere, and the first thing each one emits is its repo's map.
    let pollers: Vec<Poller> = map_issues
        .iter()
        .map(|(slug, &number)| {
            let (owner, name) = slug.split_once('/').expect("slug is owner/name");
            Poller::new(owner, name, number)
        })
        .collect();
    spawn_all(pollers, tx.clone());

    match next(&mut rx).await {
        LoadEvent::Fetched {
            repo,
            outcome: RefreshEvent::Updated(map),
        } => {
            assert_eq!(repo, "blooop/wayfinder");
            assert_eq!(map.repo, "blooop/wayfinder");
            assert!(
                map.tickets.len() >= 7,
                "expected the real map's tickets, got {}",
                map.tickets.len()
            );
        }
        other => panic!("a poller's first event must be its map, got {other:?}"),
    }
}
