//! The load: everything the picker needs, arriving after the screen is already
//! up (#27, #28).
//!
//! `wf` is on screen for seconds and restarts warm in ~0.6 s, so there is
//! nothing here to keep fresh — [Build 7](https://github.com/blooop/wayfinder/issues/34)
//! deleted the two-tier ETag poll ([Build 5](https://github.com/blooop/wayfinder/issues/17))
//! along with the event loop it served. What is left is a **one-shot load per
//! map**, streamed:
//!
//! 1. The cached map ids (#28) start their fetches before the first frame.
//! 2. One `wayfinder:map` label search runs unconditionally alongside them and
//!    reconciles that set — the cache is a head start, never a skip.
//! 3. Each map lands on screen as it arrives; `ctrl-r` is how you ask again.
//!
//! Everything is keyed by [`MapId`] rather than repo slug (#50): a repo can
//! hold several open maps, and each is its own load, its own arrival, and its
//! own possible failure.
//!
//! Failures never surface as errors: every outcome is a [`MapFetch`], and a
//! failed load leaves the picker up with a notice rather than taking it down.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::fetch;
use crate::model::{Map, MapId, MapSet};
use crate::projects::ProjectsCache;

/// How long the map search waits before trying again. The only recurring timer
/// left: nothing polls, but a search that never answers would leave `wf`
/// permanently empty.
pub const RETRY_INTERVAL: Duration = Duration::from_secs(4);

/// One map's load. Two states, because with the conditional probe gone
/// there is no third: a fetch either produced a map or it did not.
#[derive(Debug)]
pub enum MapFetch {
    /// The map came back.
    Loaded(Map),
    /// The fetch failed (network, auth, parse); nothing to show for this map.
    Failed,
}

/// Everything the event loop learns from background work (#27).
///
/// One channel rather than several, because the loop drains it between frames
/// and every variant is "something arrived that the screen should reflect".
#[derive(Debug)]
pub enum LoadEvent {
    /// The one `wayfinder:map` label search returned: these are the open maps.
    /// Empty is a real answer — none of the cached repos is mapped.
    Discovered(MapSet),
    /// The label search failed. [`spawn_discovery`] keeps retrying, so this is
    /// only ever a status report.
    SearchFailed,
    /// One map's load reported.
    Fetched { id: MapId, outcome: MapFetch },
}

/// Has the `wayfinder:map` label search answered yet?
///
/// Its own axis since #28, because the cached seed makes maps arrive *before*
/// the search does: "which maps exist" and "have they landed" stop happening
/// in that order, so one cannot stand in for the other.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Search {
    /// Still out. A failed search waits here too, since discovery retries, so a
    /// network blip at startup simply looks like a slow start.
    #[default]
    Out,
    /// Answered — the set of open maps is now authoritative.
    Answered,
}

/// How far `wf`'s load has got (#27, reshaped by #28).
///
/// The screen is up before any of it has landed, so "no tickets" has to stay
/// distinguishable from "not loaded yet" — one empty list would otherwise mean
/// both, which is exactly the sentinel this project's modelling rules out.
///
/// A struct rather than a `Searching | Loading | Loaded` enum because the cache
/// seed splits what used to be one sequence into two independent facts: maps can
/// be known (and even fully arrived) while the search is still out. The old enum
/// could represent "loaded" with the search still running — a state a seeded
/// start reaches immediately and which would then have claimed the load was
/// finished before anything had confirmed the map set. Here `is_loaded` is
/// *derived* from both facts, so it cannot disagree with either.
///
/// `arrived` is a set of [`MapId`]s rather than a count because the loads run
/// concurrently: naming who is still pending is the only thing that cannot be
/// fooled by one map reporting twice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Startup {
    search: Search,
    expected: BTreeSet<MapId>,
    arrived: BTreeSet<MapId>,
}

impl Startup {
    /// Start from the cached head start (#28): these maps are believed to exist
    /// and are already being fetched, but the search has yet to confirm them. An
    /// empty seed is the cold start, identical to the pre-cache behaviour.
    pub fn seeded(maps: &MapSet) -> Self {
        Self {
            search: Search::Out,
            expected: maps.clone(),
            arrived: BTreeSet::new(),
        }
    }

    /// Nothing is being waited on — for an [`crate::app::App`] handed its maps
    /// already, and for tests.
    pub fn loaded() -> Self {
        Self {
            search: Search::Answered,
            ..Self::default()
        }
    }

    /// The search answered: `maps` is now the authoritative set of open maps.
    /// A seeded map it omits stops being waited on; one it adds joins the wait
    /// unless it has already reported.
    pub fn searched(&mut self, maps: &MapSet) {
        self.search = Search::Answered;
        self.expected = maps.clone();
    }

    /// Record `id`'s map reporting. A *failed* fetch counts as reported: the
    /// wait for that map is over either way, and its failure is carried by
    /// [`crate::app::App::failed`] rather than by looking unfinished forever.
    pub fn record_arrival(&mut self, id: &MapId) {
        self.arrived.insert(id.clone());
    }

    /// `ctrl-r`: every map is being fetched again, so nothing has arrived yet.
    ///
    /// The same state a load uses, because it is the same question — how many
    /// of the maps we expect are in — and answering it once means the count
    /// line reports a manual refresh exactly as it reports a start, instead of
    /// a refresh being a silent pause with a stale hint.
    pub fn reloading(&mut self) {
        self.arrived.clear();
    }

    /// Maps still out.
    fn pending(&self) -> impl Iterator<Item = &MapId> {
        self.expected.difference(&self.arrived)
    }

    /// Is there nothing left to wait for? Every expected map has reported *and*
    /// the search has answered — a seeded start satisfies the first long before
    /// the second, and the search is what may still add a map opened since the
    /// last run, so both are required.
    pub fn is_loaded(&self) -> bool {
        self.pending().next().is_none() && self.search == Search::Answered
    }

    /// The count line's loading hint, empty once loaded — this is what keeps an
    /// empty list from reading as "nothing to do" while the fetch is still out.
    pub fn hint(&self) -> String {
        if self.is_loaded() {
            return String::new();
        }
        let total = self.expected.len();
        let arrived = total - self.pending().count();
        if arrived == total {
            // Nothing left to fetch, so the search is the only thing still out —
            // whether that is the cold start with nothing known, or a seeded one
            // whose cached maps are all on screen already.
            return "· searching for maps…".to_string();
        }
        format!("· loading maps {arrived}/{total}")
    }
}

/// The map loads: **at most one per map id**. That invariant is the whole
/// reason this owns the tasks (#28).
///
/// A seeded start can be *wrong* — the ids came from the last run — and the
/// correction arrives later, in the search's answer. A corrected set that only
/// landed in `App::open_maps` would fix what the screen believes while the
/// tasks actually doing the fetching kept asking for stale issues; so the load
/// set is reconciled against the truth instead. The id *is* the whole target
/// — repo and number both — so "same repo, different map number" is simply a
/// different key, not a number to compare.
#[derive(Default)]
pub struct Loaders {
    running: BTreeMap<MapId, JoinHandle<()>>,
}

impl Loaders {
    pub fn new() -> Self {
        Self::default()
    }

    /// Make the loads match `want`: drop any map no longer in the set, start
    /// one for every map not already loading, and leave the rest untouched — a
    /// map that has already arrived has nothing to fetch again.
    ///
    /// Every event is tagged with its [`MapId`] so the UI loop knows which
    /// cluster to swap. This call **is** the load for the maps it starts: N
    /// maps are fetched concurrently, one round trip of wall clock rather than
    /// N, and each one streams into a UI already on screen.
    pub fn reconcile(&mut self, want: &MapSet, tx: &mpsc::UnboundedSender<LoadEvent>) {
        self.running.retain(|id, task| {
            let still_wanted = want.contains(id);
            if !still_wanted {
                // A no-op for a load that already finished, and the point for
                // one still in flight against a map that has since closed.
                task.abort();
            }
            still_wanted
        });
        for id in want {
            if self.running.contains_key(id) {
                continue;
            }
            let task = spawn_load(id.clone(), tx.clone());
            self.running.insert(id.clone(), task);
        }
    }

    /// `ctrl-r`: throw every load away and start them all again.
    ///
    /// The refetch has to go through *this* rather than fetching alongside it,
    /// and the reason is ordering. A load started at t₀ and a refetch started
    /// at t₁ > t₀ both write the same map's cluster, and the load can land
    /// second — so a refetch racing the initial load used to be overwritten by
    /// an older snapshot while the screen said `refreshed`. Nothing polls now,
    /// so that stale map would be final. Restarting means every result reaches
    /// the UI through one channel, in send order, and the newest write wins by
    /// construction.
    pub fn restart(&mut self, want: &MapSet, tx: &mpsc::UnboundedSender<LoadEvent>) {
        self.abort_all();
        self.reconcile(want, tx);
    }

    /// Stop every load and wait for it to actually be gone.
    ///
    /// Awaited, not fired and forgotten, because `abort()` only *schedules*
    /// cancellation: the `gh` child is killed when the task's `Child` is
    /// dropped, and that drop happens when the runtime next polls the task. On
    /// the launch path the very next thing this process does is `exec`, so
    /// without the await there is no "next poll" — the `gh` would survive into
    /// the agent as a zombie holding its terminal.
    pub async fn shutdown(&mut self) {
        self.abort_all();
        for (_, task) in std::mem::take(&mut self.running) {
            // A load that already finished joins immediately; an aborted one
            // resolves to a `JoinError::Cancelled`. Both mean "gone".
            let _ = task.await;
        }
    }

    fn abort_all(&mut self) {
        for task in self.running.values() {
            task.abort();
        }
        self.running.clear();
    }

    /// The maps being loaded — the reconciled truth, for tests and for
    /// anything that needs to know what is actually live.
    pub fn targets(&self) -> MapSet {
        self.running.keys().cloned().collect()
    }
}

/// Fetch one map and report it. One shot: there is no loop, because there is
/// nothing to stay fresh for (#26). A malformed cached id (a slug that is not
/// `owner/name`) fails the fetch and reports as [`MapFetch::Failed`], which is
/// visible on the count line rather than silently skipped.
fn spawn_load(id: MapId, tx: mpsc::UnboundedSender<LoadEvent>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let outcome = match fetch::fetch_map(&id).await {
            Ok(map) => MapFetch::Loaded(map),
            Err(_) => MapFetch::Failed,
        };
        let _ = tx.send(LoadEvent::Fetched { id, outcome });
    })
}

/// Find every open `wayfinder:map` across the cached repos, off the path to
/// the first frame (#27).
///
/// It **retries** on [`RETRY_INTERVAL`] rather than giving up. A single failed
/// search would otherwise leave `wf` permanently empty with no way back:
/// `ctrl-r` refetches the maps it knows about, and after a failed search it
/// knows about none.
///
/// It runs **unconditionally**, warm cache or cold (#28). The cache is a head
/// start, never a skip: this is the one thing that can add a map opened since
/// the last run, drop one that was closed, and correct an id that moved — so
/// the seed is never trusted for longer than one search round trip. On success
/// it writes its findings back to `cache_path`, which is what makes the *next*
/// run warm; a failed write costs only that head start.
///
/// Returns its handle so the launch path can stop it and wait: it holds a `gh`
/// child of its own, and the same reasoning as [`Loaders::shutdown`] applies.
pub fn spawn_discovery(
    repos: Vec<String>,
    cache_path: PathBuf,
    tx: mpsc::UnboundedSender<LoadEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match fetch::find_maps(&repos).await {
                Ok(found) => {
                    let mut cache = ProjectsCache::load_or_default(&cache_path);
                    cache.record_search(&repos, &found);
                    let _ = cache.save(&cache_path);
                    let _ = tx.send(LoadEvent::Discovered(found));
                    return; // the set of open maps is fixed for this run
                }
                Err(_) if tx.send(LoadEvent::SearchFailed).is_err() => return, // UI is gone
                Err(_) => {}
            }
            tokio::time::sleep(RETRY_INTERVAL).await;
        }
    })
}

/// Where the cursor lands after a load or a refresh swaps the ticket list.
///
/// Identity wins over position: if the previously selected row still exists
/// anywhere in the new order, the cursor follows it. Only if it vanished does
/// the cursor fall back to the same index, clamped to the new length. A map
/// arriving must never teleport the selection just because rows moved.
///
/// Generic over the key so the caller decides what identity *is* — since #50
/// that is `(MapId, ticket number)`: the same ticket listed on two maps of one
/// repo is two rows, and the map half is what keeps the cursor on the right one.
pub fn preserve_cursor<K: PartialEq>(
    old_selected: Option<&K>,
    old_index: usize,
    new_order: &[K],
) -> usize {
    if let Some(sel) = old_selected {
        if let Some(idx) = new_order.iter().position(|k| k == sel) {
            return idx;
        }
    }
    old_index.min(new_order.len().saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: (&str, u64) = ("wayfinder", 6);
    const B: (&str, u64) = ("wayfinder", 7);
    const C: (&str, u64) = ("wayfinder", 14);
    const D: (&str, u64) = ("other", 6); // same number, different repo

    #[test]
    fn cursor_follows_ticket_identity_when_rows_reorder() {
        // B was selected at index 1; a fresh map moves it to the top.
        assert_eq!(preserve_cursor(Some(&B), 1, &[B, A, C]), 0);
        // …or to the bottom.
        assert_eq!(preserve_cursor(Some(&B), 1, &[A, C, B]), 2);
    }

    #[test]
    fn cursor_stays_put_when_nothing_moved() {
        assert_eq!(preserve_cursor(Some(&B), 1, &[A, B, C]), 1);
    }

    #[test]
    fn identity_is_repo_and_number_not_number_alone() {
        // A ("wayfinder"#6) selected; new list has "other"#6 earlier — must
        // not match it.
        assert_eq!(preserve_cursor(Some(&A), 2, &[D, B, A]), 2);
    }

    #[test]
    fn the_same_ticket_under_two_maps_is_two_distinct_rows() {
        // #50: identity carries the map, so the cursor stays on the cluster it
        // was in rather than jumping to the other map's copy of the ticket.
        // Against the real `RowKey`, since that is the K this is generic for.
        let on = |map: u64| crate::app::RowKey {
            map: MapId::new("blooop/wayfinder", map),
            ticket: 6,
        };
        let order = [on(1), on(47)];
        assert_eq!(preserve_cursor(Some(&on(47)), 0, &order), 1);
        assert_eq!(preserve_cursor(Some(&on(1)), 1, &order), 0);
    }

    #[test]
    fn vanished_ticket_falls_back_to_same_index_clamped() {
        // C vanished; cursor was at 2, new list has 2 rows → clamp to 1.
        assert_eq!(preserve_cursor(Some(&C), 2, &[A, B]), 1);
        // Cursor was at 0 and that ticket vanished → stay at 0.
        assert_eq!(preserve_cursor(Some(&A), 0, &[B, C]), 0);
    }

    #[test]
    fn empty_new_list_pins_cursor_to_zero() {
        assert_eq!(preserve_cursor(Some(&A), 2, &[]), 0);
    }

    #[test]
    fn no_prior_selection_clamps_index() {
        assert_eq!(preserve_cursor::<(&str, u64)>(None, 5, &[A, B]), 1);
        assert_eq!(preserve_cursor::<(&str, u64)>(None, 0, &[A, B]), 0);
    }

    /// A `MapSet` from (slug, number) pairs.
    fn found(maps: &[(&str, u64)]) -> MapSet {
        maps.iter().map(|&(slug, n)| MapId::new(slug, n)).collect()
    }

    /// A cold start: no cached seed, search still out.
    fn cold() -> Startup {
        Startup::seeded(&found(&[]))
    }

    #[test]
    fn a_search_that_found_no_maps_is_loaded_not_loading() {
        // Zero open maps is an *answer*: there is nothing to wait for, so
        // the screen must stop claiming to be loading.
        let mut startup = cold();
        startup.searched(&found(&[]));
        assert!(startup.is_loaded());
        assert_eq!(startup.hint(), "");
    }

    #[test]
    fn loading_completes_when_every_discovered_map_has_reported() {
        // Three maps, two of them on one repo — each is its own arrival (#50).
        let mut startup = cold();
        startup.searched(&found(&[("a/one", 1), ("a/one", 9), ("b/two", 2)]));
        assert_eq!(startup.hint(), "· loading maps 0/3");
        startup.record_arrival(&MapId::new("a/one", 1));
        assert_eq!(
            startup.hint(),
            "· loading maps 1/3",
            "the repo's other map is still out"
        );
        startup.record_arrival(&MapId::new("a/one", 9));
        assert_eq!(startup.hint(), "· loading maps 2/3");
        startup.record_arrival(&MapId::new("b/two", 2));
        assert!(startup.is_loaded());
    }

    #[test]
    fn one_map_reporting_twice_does_not_complete_another_maps_load() {
        // The loads are concurrent, and `ctrl-r` can make a map report again
        // while a slow one is still out. Counting arrivals would call that
        // done; naming who is still pending cannot.
        let mut startup = cold();
        startup.searched(&found(&[("fast/one", 1), ("slow/two", 2)]));
        startup.record_arrival(&MapId::new("fast/one", 1));
        startup.record_arrival(&MapId::new("fast/one", 1));
        assert_eq!(startup.hint(), "· loading maps 1/2", "slow/two is still out");
        startup.record_arrival(&MapId::new("slow/two", 2));
        assert!(startup.is_loaded());
    }

    #[test]
    fn arrivals_after_the_load_do_not_push_the_screen_back_into_loading() {
        let mut startup = cold();
        startup.searched(&found(&[("a/one", 1)]));
        startup.record_arrival(&MapId::new("a/one", 1));
        startup.record_arrival(&MapId::new("a/one", 1));
        startup.record_arrival(&MapId::new("b/two", 2)); // never expected; not a regression
        assert!(startup.is_loaded());
        assert_eq!(startup.hint(), "");
    }

    #[test]
    fn the_hint_tells_the_two_pre_data_states_apart() {
        // Both draw an empty list; only the hint says why it is empty.
        assert_eq!(cold().hint(), "· searching for maps…");
        assert_eq!(
            Startup::seeded(&found(&[("a/one", 1), ("a/two", 2)])).hint(),
            "· loading maps 0/2"
        );
    }

    #[test]
    fn a_seeded_start_is_loading_its_cached_maps_not_searching() {
        // The #28 head start: the cache already named these maps, so the screen
        // says what it is waiting for from the first frame rather than spending
        // the search's ~2.5s claiming to be looking for maps at all.
        let mut startup = Startup::seeded(&found(&[("a/one", 1), ("b/two", 2)]));
        assert_eq!(startup.hint(), "· loading maps 0/2");
        startup.record_arrival(&MapId::new("a/one", 1));
        startup.record_arrival(&MapId::new("b/two", 2));
        // Every *cached* map is in, but the search has yet to confirm the set —
        // and confirming it is exactly what may add a map opened since the last
        // run, so the load is not over until both are true.
        assert!(!startup.is_loaded(), "the search has not answered yet");
        assert_eq!(startup.hint(), "· searching for maps…");
        startup.searched(&found(&[("a/one", 1), ("b/two", 2)]));
        assert!(startup.is_loaded());
    }

    #[test]
    fn ctrl_r_puts_the_load_back_on_the_count_line() {
        // A refresh refetches every map, so nothing has arrived until it does.
        // Without this the hint stays empty and `ctrl-r` is a silent pause.
        let mut startup = cold();
        let maps = found(&[("a/one", 1), ("b/two", 2)]);
        startup.searched(&maps);
        startup.record_arrival(&MapId::new("a/one", 1));
        startup.record_arrival(&MapId::new("b/two", 2));
        assert!(startup.is_loaded());

        startup.reloading();
        assert!(!startup.is_loaded());
        assert_eq!(startup.hint(), "· loading maps 0/2");
        startup.record_arrival(&MapId::new("a/one", 1));
        assert_eq!(startup.hint(), "· loading maps 1/2");
        startup.record_arrival(&MapId::new("b/two", 2));
        assert!(startup.is_loaded(), "the search already answered; it stays answered");
        assert_eq!(startup.hint(), "");
    }

    #[test]
    fn the_search_overrules_a_seed_it_disagrees_with() {
        // Cached "b/two" lost its map and "c/three" gained one since last run.
        let mut startup = Startup::seeded(&found(&[("a/one", 1), ("b/two", 2)]));
        startup.record_arrival(&MapId::new("a/one", 1));
        startup.searched(&found(&[("a/one", 1), ("c/three", 9)]));
        assert_eq!(
            startup.hint(),
            "· loading maps 1/2",
            "b/two is no longer waited on; c/three now is"
        );
        startup.record_arrival(&MapId::new("c/three", 9));
        assert!(startup.is_loaded());
    }

    #[test]
    fn a_moved_map_number_is_a_different_map_to_wait_on() {
        // The seed said #2; the search says the repo's map is #1. Same repo,
        // different id — the arrival of the stale fetch must not satisfy the
        // wait for the corrected one.
        let mut startup = Startup::seeded(&found(&[("a/one", 2)]));
        startup.record_arrival(&MapId::new("a/one", 2));
        startup.searched(&found(&[("a/one", 1)]));
        assert!(!startup.is_loaded(), "#1 has not arrived; #2 no longer counts");
        assert_eq!(startup.hint(), "· loading maps 0/1");
        startup.record_arrival(&MapId::new("a/one", 1));
        assert!(startup.is_loaded());
    }
}
