//! Shared fixture for the live integration tests.
//!
//! **Never name a map by number.** Every live test here used to pin
//! `blooop/wayfinder` map `#1`, which quietly assumed that a map reaching its
//! destination would never happen — and when #1 was closed, six tests across
//! three files failed at once, none of them for a reason connected to what
//! they test. `fetch_map` refusing a closed map is *correct* (#28: a moved or
//! finished map must render nothing rather than the wrong issue), so the
//! failure was the fixture's fault, not the code's.

use wf::model::MapId;

pub const THIS_REPO: &str = "blooop/wayfinder";

/// This repo's lowest-numbered open map, looked up live.
///
/// Lowest-numbered rather than arbitrary so a run is reproducible against a
/// tracker that is not moving, and looked up rather than named so the fixture
/// survives any individual map being finished. The only precondition left is
/// that the project has *a* map at all, which is the weakest one a test of the
/// map machinery can have.
pub async fn a_live_map() -> MapId {
    let maps = wf::fetch::find_maps(&[THIS_REPO.to_string()])
        .await
        .expect("map label search");
    maps.into_iter()
        .min_by_key(|id| id.number)
        .expect("blooop/wayfinder must have at least one open wayfinder:map issue")
}
