//! Integration test for the streaming startup path (#27) and the cached head
//! start (#28) against the real tracker. Needs network and an authenticated
//! `gh`.
//!
//! What it pins down is the *shape* `main` now relies on: nothing is fetched
//! before the loop exists, and everything the loop needs — which maps are
//! open, and each map — arrives afterwards through one channel. Before #27 the
//! same information could only be had by awaiting it up front, which is exactly
//! why the terminal stayed blank for it; before #28 the *search* still had to
//! answer before a single map could be asked for. Since #50 every load is
//! keyed by [`MapId`], and a repo with several open maps is several loads.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use wf::model::{MapId, MapSet};
use wf::projects::ProjectsCache;
use wf::refresh::{spawn_discovery, LoadEvent, Loaders, MapFetch};

mod common;

use common::THIS_REPO;

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

    let found = loop {
        match next(&mut rx).await {
            LoadEvent::Discovered(found) => break found,
            // The search retries, so a blip is survivable rather than fatal:
            // go round and wait for the next event.
            LoadEvent::SearchFailed => {}
            // Named rather than `_`, so a seventh kind of arrival is a compile
            // error here and a decision someone makes — this file is compiled
            // but never run by CI, which makes a wildcard in it the least
            // observed thing in the tree.
            other @ (LoadEvent::Fetched { .. } | LoadEvent::Surveyed(_)) => {
                panic!("expected discovery first, got {other:?}")
            }
        }
    };
    assert!(
        found.len() > 1,
        "every open map is discovered, not one per repo (#50); found {found:?}"
    );

    // Reconciling the loaders *is* the initial load: no separate fetch happens
    // anywhere, and the one thing each one emits is its map.
    let mut loaders = Loaders::new();
    loaders.reconcile(&found, &tx);
    assert_eq!(loaders.targets(), found);

    // Every discovered map reports — including the several on this one repo.
    let mut arrived = MapSet::new();
    while arrived.len() < found.len() {
        match next(&mut rx).await {
            LoadEvent::Fetched {
                id,
                outcome: MapFetch::Loaded(map),
            } => {
                assert!(found.contains(&id), "unexpected map {id:?}");
                assert!(
                    !map.tickets.is_empty(),
                    "every open map on this repo has tickets"
                );
                arrived.insert(id);
            }
            other => panic!("a loader's one event must be its map, got {other:?}"),
        }
    }
    assert_eq!(arrived, found);

    // The search's findings are written back — that is what makes the *next*
    // run warm, and it is the only thing that ever populates the seed.
    let saved = ProjectsCache::load_or_default(&cache_path);
    assert_eq!(
        saved.map_seed(),
        found,
        "the search must leave the full head start behind"
    );
    std::fs::remove_dir_all(cache_path.parent().expect("scratch dir")).ok();
}

#[tokio::test]
async fn a_cached_seed_fetches_the_map_without_waiting_for_the_search() {
    // The #28 claim, end to end: with the map id already in hand, the map
    // itself lands in one round trip — no ~2.5s search in front of it.
    let (tx, mut rx) = mpsc::unbounded_channel();
    // Looked up *before* the clock starts, because that is what a seed is: an
    // id already in hand when the run begins.
    let live = common::a_live_map().await;
    let seed: MapSet = [live.clone()].into_iter().collect();

    let started = Instant::now();
    let mut loaders = Loaders::new();
    loaders.reconcile(&seed, &tx);

    match next(&mut rx).await {
        LoadEvent::Fetched {
            id,
            outcome: MapFetch::Loaded(map),
        } => {
            assert_eq!(id, live);
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
    // A cached id that no longer names a map — here `#2`, a closed sub-issue
    // that exists and is not a map, which is refused for either reason alone.
    // The fetch must refuse it (a wrong map is worse than no map) and the
    // search's answer must move the load onto the real id.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let stale: MapSet = [MapId::new(THIS_REPO, 2)].into_iter().collect();

    let mut loaders = Loaders::new();
    loaders.reconcile(&stale, &tx);
    match next(&mut rx).await {
        LoadEvent::Fetched {
            outcome: MapFetch::Failed,
            ..
        } => {}
        other => panic!("a non-map must not fetch as a map, got {other:?}"),
    }

    let truth: MapSet = [common::a_live_map().await].into_iter().collect();
    loaders.reconcile(&truth, &tx);
    assert_eq!(
        loaders.targets(),
        truth,
        "the corrected id must reach the task doing the fetching"
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
            // The aborted load may have queued one last failure first: go round
            // and keep waiting for the loaded one.
            LoadEvent::Fetched { .. } => {}
            other => panic!("unexpected event {other:?}"),
        }
    }
}

#[tokio::test]
async fn restarting_the_loaders_refetches_and_cannot_be_beaten_by_the_load_it_replaced() {
    // `ctrl-r`'s path. It goes through `Loaders` rather than fetching alongside
    // them for one reason: a refetch started at t₁ and a load started at t₀ < t₁
    // both write the same map's cluster, and the *older* one can land second.
    // Nothing polls any more, so that stale map would be the last word. Every
    // result reaching the UI through one channel in send order is what makes
    // the newest write win, and that is what this pins.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let live = common::a_live_map().await;
    let seed: MapSet = [live.clone()].into_iter().collect();

    let mut loaders = Loaders::new();
    loaders.reconcile(&seed, &tx);
    match next(&mut rx).await {
        LoadEvent::Fetched {
            outcome: MapFetch::Loaded(_),
            ..
        } => {}
        other => panic!("the initial load must land first, got {other:?}"),
    }

    // The refresh: same map, and it must fetch again rather than skip a map it
    // has already loaded — the `continue` in `reconcile` is exactly what
    // `restart` exists to get past.
    loaders.restart(&seed, &tx);
    assert_eq!(loaders.targets(), seed);
    match next(&mut rx).await {
        LoadEvent::Fetched {
            id,
            outcome: MapFetch::Loaded(map),
        } => {
            assert_eq!(id, live);
            assert!(!map.tickets.is_empty());
        }
        other => panic!("ctrl-r must refetch, got {other:?}"),
    }

    // And the channel is empty: exactly one result per load, so no third event
    // is queued behind the refresh waiting to overwrite it.
    assert!(
        rx.try_recv().is_err(),
        "a superseded load must not still be queued to clobber the refresh"
    );
}

#[tokio::test]
async fn shutdown_leaves_nothing_in_flight() {
    // The launch path awaits this immediately before `exec`. An in-flight `gh`
    // that outlives the exec is inherited by the agent as a zombie holding its
    // terminal, and `abort()` alone does not kill it — the child dies when the
    // task's `Child` is dropped, which only happens if someone waits for the
    // cancellation to actually run.
    let (tx, _rx) = mpsc::unbounded_channel();
    let seed: MapSet = [common::a_live_map().await].into_iter().collect();

    let mut loaders = Loaders::new();
    loaders.reconcile(&seed, &tx);
    assert_eq!(loaders.targets(), seed);

    // Bounded, because the failure mode is a *hang*: awaiting a cancellation
    // that never completes would park the launch forever with the terminal
    // already half handed over. It must also not wait out the `gh` round trip
    // it just cancelled.
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(10), loaders.shutdown())
        .await
        .expect("shutdown must not hang the launch");
    assert!(loaders.targets().is_empty(), "nothing may still be loading");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "shutdown waited out the fetch it cancelled; took {elapsed:?}"
    );
}
