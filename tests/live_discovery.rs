//! Integration test for the Build 3 accretion path against reality: this
//! very checkout is discovered from its git metadata, registered into a
//! throwaway cache, its map found via the one-shot label search, and the
//! map fetched. Needs network, an authenticated `gh`, and this test running
//! from inside the wayfinder checkout (`CARGO_MANIFEST_DIR`).

use std::path::Path;

use wf::projects::{discover_checkout, ProjectsCache};

#[tokio::test]
async fn registering_this_checkout_finds_and_fetches_its_map() {
    // Discover: this checkout's toplevel and origin-derived slug.
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let (toplevel, slug) = discover_checkout(here)
        .await
        .expect("this test must run inside the wayfinder checkout");
    assert_eq!(slug, "blooop/wayfinder");
    assert!(
        toplevel.join("Cargo.toml").exists(),
        "toplevel: {toplevel:?}"
    );

    // Register: the explicit-open act, into a throwaway cache file.
    let cache_file = std::env::temp_dir()
        .join(format!("wf-live-discovery-{}", std::process::id()))
        .join("projects.json");
    let mut cache = ProjectsCache::load_or_default(&cache_file);
    cache.register(toplevel.clone(), slug.clone());
    cache.save(&cache_file).expect("save cache");
    let reloaded = ProjectsCache::load_or_default(&cache_file);
    assert_eq!(reloaded.repos(), vec![slug.clone()]);
    assert_eq!(reloaded.checkouts[0].path, toplevel);
    std::fs::remove_dir_all(cache_file.parent().unwrap()).ok();

    // Detect: one label search across the cached remotes finds every open map
    // (#50) — this repo keeps several open at once, and all of them must
    // survive rather than only the lowest-numbered.
    let maps = wf::fetch::find_maps(&reloaded.repos())
        .await
        .expect("map label search");
    let map_id = wf::model::MapId::new(slug.clone(), 1);
    assert!(
        maps.contains(&map_id),
        "blooop/wayfinder must have wayfinder:map issue #1; found {maps:?}"
    );
    assert!(
        maps.iter().filter(|id| id.repo == slug).count() > 1,
        "every open map must be found, not one per repo; found {maps:?}"
    );

    // Render-ready: the discovered map fetches with real tickets.
    let map = wf::fetch::fetch_map(&map_id)
        .await
        .expect("fetch the discovered map");
    assert!(
        map.tickets.len() >= 7,
        "expected the real map's tickets, got {}",
        map.tickets.len()
    );
}
