//! Integration test for the streaming startup path (#27) and the cached head
//! start (#28) against the real tracker. Needs network and an authenticated
//! `gh`.
//!
//! What it pins down is the *shape* `main` now relies on: nothing is fetched
//! before the loop exists, and everything the loop needs — which repos have
//! maps, and each map — arrives afterwards through one channel. Before #27 the
//! same information could only be had by awaiting it up front, which is exactly
//! why the terminal stayed blank for it; before #28 the *search* still had to
//! answer before a single map could be asked for.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use wf::launch::MapIssues;
use wf::projects::ProjectsCache;
use wf::refresh::{spawn_discovery, LoadEvent, Loaders, MapFetch};

const THIS_REPO: &str = "blooop/wayfinder";

/// Take the next event, failing loudly rather than hanging a CI run forever.
async fn next(rx: &mut mpsc::UnboundedReceiver<LoadEvent>) -> LoadEvent {
    tokio::time::timeout(Duration::from_secs(30), rx.recv())
        .await
        .expect("timed out waiting for a load event")
        .expect("senders dropped")
}

/// A scratch cache file of this test's own — the real one belongs to the user.
fn scratch_cache(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wf-it-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir.join("projects.json")
}

#[tokio::test]
async fn discovery_then_every_map_arrives_through_one_channel() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let cache_path = scratch_cache("discovery");

    // Nothing is awaited here: discovery runs on its own task, so a caller
    // could be drawing a screen instead of blocking on this.
    spawn_discovery(vec![THIS_REPO.to_string()], cache_path.clone(), tx.clone());

    let map_issues = loop {
        match next(&mut rx).await {
            LoadEvent::Discovered(found) => break found,
            // The search retries, so a blip is survivable rather than fatal.
            LoadEvent::SearchFailed => continue,
            other => panic!("expected discovery first, got {other:?}"),
        }
    };
    assert_eq!(
        map_issues.get(THIS_REPO),
        Some(&1),
        "this repo's map is issue #1"
    );

    // Reconciling the loaders *is* the initial load: no separate fetch happens
    // anywhere, and the one thing each one emits is its repo's map.
    let mut loaders = Loaders::new();
    loaders.reconcile(&map_issues, &tx);
    assert_eq!(loaders.targets(), map_issues);

    match next(&mut rx).await {
        LoadEvent::Fetched {
            repo,
            outcome: MapFetch::Loaded(map),
        } => {
            assert_eq!(repo, THIS_REPO);
            assert_eq!(map.repo, THIS_REPO);
            assert!(
                map.tickets.len() >= 7,
                "expected the real map's tickets, got {}",
                map.tickets.len()
            );
        }
        other => panic!("a loader's one event must be its map, got {other:?}"),
    }

    // The search's findings are written back — that is what makes the *next*
    // run warm, and it is the only thing that ever populates the seed.
    let saved = ProjectsCache::load_or_default(&cache_path);
    assert_eq!(
        saved.map_seed().get(THIS_REPO),
        Some(&1),
        "the search must leave a head start behind"
    );
    std::fs::remove_dir_all(cache_path.parent().expect("scratch dir")).ok();
}

#[tokio::test]
async fn a_cached_seed_fetches_the_map_without_waiting_for_the_search() {
    // The #28 claim, end to end: with the map number already in hand, the map
    // itself lands in one round trip — no ~2.5s search in front of it.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let seed: MapIssues = [(THIS_REPO.to_string(), 1)].into_iter().collect();

    let started = Instant::now();
    let mut loaders = Loaders::new();
    loaders.reconcile(&seed, &tx);

    match next(&mut rx).await {
        LoadEvent::Fetched {
            repo,
            outcome: MapFetch::Loaded(map),
        } => {
            assert_eq!(repo, THIS_REPO);
            assert!(!map.tickets.is_empty());
        }
        other => panic!("the seeded loader must fetch the map, got {other:?}"),
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(1500),
        "a warm start must show real tickets in well under a second of network; took {elapsed:?}"
    );
}

#[tokio::test]
async fn a_stale_seed_reports_failure_and_is_replaced_by_the_search() {
    // A cached number that no longer names a map — here the map's own *first
    // ticket*, an issue that exists and is a sub-issue rather than a map. The
    // fetch must refuse it (a wrong map is worse than no map) and the search's
    // answer must move the load onto the real number.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let stale: MapIssues = [(THIS_REPO.to_string(), 2)].into_iter().collect();

    let mut loaders = Loaders::new();
    loaders.reconcile(&stale, &tx);
    match next(&mut rx).await {
        LoadEvent::Fetched {
            outcome: MapFetch::Failed,
            ..
        } => {}
        other => panic!("a non-map must not fetch as a map, got {other:?}"),
    }

    let truth: MapIssues = [(THIS_REPO.to_string(), 1)].into_iter().collect();
    loaders.reconcile(&truth, &tx);
    assert_eq!(
        loaders.targets(),
        truth,
        "the corrected number must reach the task doing the fetching"
    );
    loop {
        match next(&mut rx).await {
            LoadEvent::Fetched {
                outcome: MapFetch::Loaded(map),
                ..
            } => {
                assert!(!map.tickets.is_empty());
                break;
            }
            // The aborted load may have queued one last failure first.
            LoadEvent::Fetched { .. } => continue,
            other => panic!("unexpected event {other:?}"),
        }
    }
}
