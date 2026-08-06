//! Background refresh: the live-poll loop behind the TUI (Build 5, #17).
//!
//! Strategy (per the #3 data-plane research): `gh api graphql` has no ETags,
//! so the hot loop is a two-tier hybrid —
//!
//! 1. Every [`POLL_INTERVAL`], a conditional REST probe of the map's
//!    `sub_issues` endpoint with `If-None-Match`. A 304 costs zero rate
//!    limit and means nothing changed; only a 200 triggers rerunning the
//!    full GraphQL map query (2 points).
//! 2. Every [`FULL_REFRESH_EVERY`]th cycle, an unconditional GraphQL fetch
//!    regardless of the probe — the research left unverified whether
//!    edge-only changes (dependency add/remove) flip the `sub_issues` ETag,
//!    so this bounds that staleness at ~30 s.
//!
//! Two live-verified `gh` quirks shape the prober: `gh api` exits nonzero on
//! a 304 (it is not a 2xx), so the status line is parsed instead of the exit
//! code; and the ETag hashes the response body, so the probe requests
//! `per_page=100` — a truncated page could miss changes to later children.
//!
//! Failures never surface as errors: every outcome is a [`RefreshEvent`],
//! and a failed poll just leaves the UI on stale data with an indicator.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::fetch;
use crate::launch::MapIssues;
use crate::model::Map;
use crate::projects::ProjectsCache;

/// How often the background loop probes for changes.
pub const POLL_INTERVAL: Duration = Duration::from_secs(4);

/// Every Nth cycle skips the probe and fetches unconditionally (the
/// edge-only-change safety net; see module docs).
pub const FULL_REFRESH_EVERY: u32 = 8;

/// One poll cycle's outcome, sent to the UI loop. Never an `Err`: refresh
/// failure is a displayable state, not a crash.
#[derive(Debug)]
pub enum RefreshEvent {
    /// The tracker changed; here is the freshly fetched map.
    Updated(Map),
    /// The probe confirmed nothing changed (HTTP 304) — data verified fresh.
    Unchanged,
    /// The poll failed (network, auth, parse); keep showing stale data.
    Failed,
}

/// Everything the event loop learns from background work (#27).
///
/// One channel rather than several, because the loop drains it between frames
/// and every variant is "something arrived that the screen should reflect".
/// Note there is no separate notion of an *initial* fetch result: the initial
/// load and the steady-state poll produce the same per-repo [`RefreshEvent`],
/// because the initial load **is** each poller's first cycle.
#[derive(Debug)]
pub enum LoadEvent {
    /// The one `wayfinder:map` label search returned: these repos have maps.
    /// Empty is a real answer — none of the cached repos is mapped.
    Discovered(MapIssues),
    /// The label search failed. Nothing can load until it succeeds, so
    /// [`spawn_discovery`] keeps retrying and this is only ever a status report.
    SearchFailed,
    /// One repo's map fetch reported.
    Fetched { repo: String, outcome: RefreshEvent },
    /// The agent-tab recount finished — the AFK slot (#7), kept off the path to
    /// the first frame because it is `zellij` traffic and `zellij` can wedge
    /// (#21).
    AgentTabs(usize),
}

/// Has the `wayfinder:map` label search answered yet?
///
/// Its own axis since #28, because the cached seed makes maps arrive *before*
/// the search does: "which repos are mapped" and "have their maps landed" stop
/// happening in that order, so one cannot stand in for the other.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Search {
    /// Still out. A failed search waits here too, since discovery retries, so a
    /// network blip at startup simply looks like a slow start.
    #[default]
    Out,
    /// Answered — the set of mapped repos is now authoritative.
    Answered,
}

/// How far `wf`'s initial load has got (#27, reshaped by #28).
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
/// `arrived` is a set of repo slugs rather than a count because the pollers run
/// concurrently on a [`POLL_INTERVAL`] cadence: a fast repo's *second* cycle can
/// land before a slow repo's first, and a counter cannot tell that from the slow
/// one arriving. Keeping `expected` and `arrived` as sets — with pending their
/// difference — also makes the search's answer idempotent over a seed that has
/// already reported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Startup {
    search: Search,
    expected: BTreeSet<String>,
    arrived: BTreeSet<String>,
}

impl Startup {
    /// Start from the cached head start (#28): these maps are believed to exist
    /// and are already being fetched, but the search has yet to confirm them. An
    /// empty seed is the cold start, identical to the pre-cache behaviour.
    pub fn seeded(map_issues: &MapIssues) -> Self {
        Self {
            search: Search::Out,
            expected: map_issues.keys().cloned().collect(),
            arrived: BTreeSet::new(),
        }
    }

    /// Nothing is being waited on — for an [`crate::app::App`] handed a map it
    /// already has, and for tests.
    pub fn loaded() -> Self {
        Self {
            search: Search::Answered,
            ..Self::default()
        }
    }

    /// The search answered: `map_issues` is now the authoritative set of mapped
    /// repos. A seeded repo it omits stops being waited on; one it adds joins
    /// the wait unless it has already reported.
    pub fn searched(&mut self, map_issues: &MapIssues) {
        self.search = Search::Answered;
        self.expected = map_issues.keys().cloned().collect();
    }

    /// Record `repo`'s map reporting. A *failed* fetch counts as reported: the
    /// wait for that map is over either way, and its failure shows as staleness,
    /// not as loading.
    pub fn record_arrival(&mut self, repo: &str) {
        self.arrived.insert(repo.to_string());
    }

    /// Repos whose maps are still out.
    fn pending(&self) -> impl Iterator<Item = &String> {
        self.expected.difference(&self.arrived)
    }

    /// Is there nothing left to wait for? Every expected map has reported *and*
    /// the search has answered — a seeded start satisfies the first long before
    /// the second, and the search is what may still add a repo mapped since the
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

/// What the conditional probe learned.
enum Probe {
    /// 304 — the stored ETag still matches.
    Unchanged,
    /// 200 — something changed; carry the new ETag for the next cycle.
    Changed { etag: Option<String> },
}

/// The background poller for one map. Owns the ETag across cycles.
pub struct Poller {
    owner: String,
    repo: String,
    number: u64,
    etag: Option<String>,
    cycle: u32,
}

impl Poller {
    /// The full repo slug this poller watches (e.g. "blooop/wayfinder") —
    /// the tag on every event it emits.
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }

    pub fn new(owner: &str, repo: &str, number: u64) -> Self {
        Self {
            owner: owner.to_string(),
            repo: repo.to_string(),
            number,
            etag: None,
            cycle: 0,
        }
    }

    /// A poller for an `owner/name` slug — `None` for anything that is not one,
    /// so a malformed cache entry is skipped rather than panicking the loop.
    pub fn for_slug(slug: &str, number: u64) -> Option<Self> {
        let (owner, name) = slug.split_once('/')?;
        (!owner.is_empty() && !name.is_empty() && !name.contains('/'))
            .then(|| Self::new(owner, name, number))
    }

    /// Run one poll cycle: conditional probe, then a full GraphQL fetch only
    /// if needed. Infallible by design — errors become [`RefreshEvent::Failed`].
    ///
    /// Cycle **0** forces the full fetch, which is what makes this poller its
    /// repo's initial load (#27): the very first cycle skips the probe it would
    /// certainly fail (there is no ETag yet) and goes straight for the map, so a
    /// cold start costs one GraphQL round trip rather than a REST probe plus
    /// one. The unconditional refresh then recurs every [`FULL_REFRESH_EVERY`]
    /// cycles as before.
    pub async fn poll_once(&mut self) -> RefreshEvent {
        let force_full = forces_full_fetch(self.cycle);
        self.cycle = self.cycle.wrapping_add(1);

        if !force_full {
            match self.probe().await {
                Ok(Probe::Unchanged) => return RefreshEvent::Unchanged,
                Ok(Probe::Changed { etag }) => self.etag = etag,
                Err(_) => return RefreshEvent::Failed,
            }
        }

        match fetch::fetch_map(&self.owner, &self.repo, self.number).await {
            Ok(map) => RefreshEvent::Updated(map),
            Err(_) => RefreshEvent::Failed,
        }
    }

    /// Conditional REST probe of the map's `sub_issues` list. `-i` prints the
    /// status line and headers; the body is discarded (the GraphQL query is
    /// the single source of parsed truth).
    async fn probe(&self) -> Result<Probe> {
        let mut args = vec!["api".to_string(), "-i".to_string()];
        if let Some(etag) = &self.etag {
            args.push("-H".to_string());
            args.push(format!("If-None-Match: {etag}"));
        }
        args.push(format!(
            "repos/{}/{}/issues/{}/sub_issues?per_page=100",
            self.owner, self.repo, self.number
        ));

        let output = Command::new("gh")
            .args(&args)
            .output()
            .await
            .context("failed to run `gh` for the refresh probe")?;

        // `gh api` exits 1 on a 304 (non-2xx), so classify by status line.
        let head = String::from_utf8_lossy(&output.stdout);
        parse_probe(&head, output.status.success())
    }
}

/// Does this cycle skip the conditional probe and fetch outright?
///
/// Counting from **zero** is what makes a poller its own initial load (#27) —
/// cycle 0 is the cold start, where a probe has no ETag to send and so cannot
/// come back 304. Probing there would buy a guaranteed-200 REST round trip
/// before the GraphQL fetch it cannot avoid, doubling the time to first paint.
fn forces_full_fetch(cycle: u32) -> bool {
    cycle.is_multiple_of(FULL_REFRESH_EVERY)
}

/// Classify a `gh api -i` response: 304 → unchanged, 2xx → changed (with the
/// new ETag pulled from the headers). Anything else is a real failure.
fn parse_probe(response_head: &str, exit_ok: bool) -> Result<Probe> {
    let status_line = response_head.lines().next().unwrap_or_default();
    if status_line.contains(" 304") {
        return Ok(Probe::Unchanged);
    }
    if exit_ok {
        let etag = response_head
            .lines()
            .take_while(|l| !l.trim().is_empty()) // headers end at the blank line
            .find_map(|l| l.strip_prefix("Etag: ").or_else(|| l.strip_prefix("ETag: ")))
            .map(|v| v.trim().to_string());
        return Ok(Probe::Changed { etag });
    }
    bail!("probe failed: {status_line}");
}

/// One live poll loop and the map number it was started for.
struct Running {
    number: u64,
    task: JoinHandle<()>,
}

/// The live poll loops: **at most one per repo, at the map number currently
/// believed**. That invariant is the whole reason this owns the tasks (#28).
///
/// Before the cache, the map numbers were known once and never changed, so the
/// pollers could be fire-and-forget. A seeded start can be *wrong* — the number
/// came from the last run — and the correction arrives later, in the search's
/// answer. A corrected number that only landed in `App::map_issues` would fix
/// what `enter` launches while the task actually doing the fetching kept asking
/// for the old issue forever; so the poller set is reconciled against the truth
/// instead, and the repo recovers without a restart.
#[derive(Default)]
pub struct Pollers {
    running: BTreeMap<String, Running>,
}

impl Pollers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Make the live loops match `want`: stop any whose repo is no longer mapped
    /// or whose map number changed, start one for every repo not already polled
    /// at the right number, and leave the rest untouched — restarting a poller
    /// that is already correct would throw away its ETag and re-fetch for
    /// nothing.
    ///
    /// Every event is tagged with the emitting poller's repo slug so the UI loop
    /// knows which map to swap. The UI drains the channel with `try_recv`
    /// between frames; tasks also end when the receiver is dropped (quit).
    /// Probes are conditional 304s, so N repos cost nothing extra at rest.
    ///
    /// Each loop **polls before it sleeps** (#27), so this call *is* the load
    /// for the repos it starts: N maps arrive concurrently, one fetch of wall
    /// clock rather than N, and each one streams into a UI already on screen.
    pub fn reconcile(&mut self, want: &MapIssues, tx: &mpsc::UnboundedSender<LoadEvent>) {
        self.running.retain(|slug, running| {
            let still_wanted = want.get(slug) == Some(&running.number);
            if !still_wanted {
                running.task.abort();
            }
            still_wanted
        });
        for (slug, &number) in want {
            if self.running.contains_key(slug) {
                continue;
            }
            let Some(poller) = Poller::for_slug(slug, number) else {
                continue;
            };
            let task = spawn_poller(poller, tx.clone());
            self.running.insert(slug.clone(), Running { number, task });
        }
    }

    /// The repos being polled, and at which map issue — the reconciled truth,
    /// for tests and for anything that needs to know what is actually live.
    pub fn watching(&self) -> MapIssues {
        self.running
            .iter()
            .map(|(slug, running)| (slug.clone(), running.number))
            .collect()
    }
}

/// One repo's poll loop: poll, report, sleep, forever.
fn spawn_poller(mut poller: Poller, tx: mpsc::UnboundedSender<LoadEvent>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let repo = poller.slug();
        loop {
            let outcome = poller.poll_once().await;
            let event = LoadEvent::Fetched {
                repo: repo.clone(),
                outcome,
            };
            if tx.send(event).is_err() {
                return; // UI is gone
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
}

/// Find which cached repos have a `wayfinder:map`, off the path to the first
/// frame (#27). Nothing can load until this returns, so it is the one genuinely
/// serial step — but the screen no longer waits on it.
///
/// It **retries** on the poll cadence rather than giving up. A single failed
/// search would otherwise leave `wf` permanently empty with no way back:
/// `ctrl-r` refetches the maps it knows about, and after a failed search it
/// knows about none.
///
/// It runs **unconditionally**, warm cache or cold (#28). The cache is a head
/// start, never a skip: this is the one thing that can add a repo mapped since
/// the last run, drop one whose map was closed, and correct a number that moved
/// — so the seed is never trusted for longer than one search round trip. On
/// success it writes its findings back to `cache_path`, which is what makes the
/// *next* run warm; a failed write costs only that head start.
pub fn spawn_discovery(
    repos: Vec<String>,
    cache_path: PathBuf,
    tx: mpsc::UnboundedSender<LoadEvent>,
) {
    tokio::spawn(async move {
        loop {
            match fetch::find_maps(&repos).await {
                Ok(found) => {
                    let found: MapIssues = found.into_iter().collect();
                    let mut cache = ProjectsCache::load_or_default(&cache_path);
                    cache.record_search(&repos, &found);
                    let _ = cache.save(&cache_path);
                    let _ = tx.send(LoadEvent::Discovered(found));
                    return; // the set of mapped repos is fixed for this run
                }
                Err(_) if tx.send(LoadEvent::SearchFailed).is_err() => return, // UI is gone
                Err(_) => {}
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    });
}

/// Where the cursor lands after a refresh swaps the ticket list.
///
/// Identity wins over position: if the previously selected ticket (by
/// `(repo, number)`) still exists anywhere in the new order, the cursor
/// follows it. Only if it vanished does the cursor fall back to the same
/// index, clamped to the new length. A refresh must never teleport the
/// selection just because rows moved between groups.
pub fn preserve_cursor(
    old_selected: Option<(&str, u64)>,
    old_index: usize,
    new_order: &[(&str, u64)],
) -> usize {
    if let Some(sel) = old_selected {
        if let Some(idx) = new_order.iter().position(|k| *k == sel) {
            return idx;
        }
    }
    old_index.min(new_order.len().saturating_sub(1))
}

/// What the count line's refresh indicator knows: when data was last
/// verified fresh (an update or a 304), and whether the latest poll failed.
///
/// Stale is not the absence of freshness — it is a positive fact (a poll
/// failed since the last success), so it is its own variant rather than a
/// bool riding alongside a timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// No poll has completed yet (initial fetch only).
    Initial,
    /// The last poll succeeded `secs_ago` seconds ago.
    Fresh { secs_ago: u64 },
    /// Polls are failing; data was last verified `secs_ago` seconds ago
    /// (`None` if no poll ever succeeded).
    Stale { secs_ago: Option<u64> },
}

impl Freshness {
    /// The subtle indicator text for the count line. Empty before the first
    /// poll completes.
    pub fn indicator(&self) -> String {
        match self {
            Freshness::Initial => String::new(),
            Freshness::Fresh { secs_ago } if *secs_ago < 2 => "· ↻ just now".to_string(),
            Freshness::Fresh { secs_ago } => format!("· ↻ {}", ago(*secs_ago)),
            Freshness::Stale { secs_ago: Some(s) } => format!("· stale {}", ago(*s)),
            Freshness::Stale { secs_ago: None } => "· stale".to_string(),
        }
    }
}

fn ago(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s ago")
    } else {
        format!("{}m ago", secs / 60)
    }
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
        // B was selected at index 1; refresh moves it to the top.
        assert_eq!(preserve_cursor(Some(B), 1, &[B, A, C]), 0);
        // …or to the bottom.
        assert_eq!(preserve_cursor(Some(B), 1, &[A, C, B]), 2);
    }

    #[test]
    fn cursor_stays_put_when_nothing_moved() {
        assert_eq!(preserve_cursor(Some(B), 1, &[A, B, C]), 1);
    }

    #[test]
    fn identity_is_repo_and_number_not_number_alone() {
        // A ("wayfinder"#6) selected; new list has "other"#6 earlier — must
        // not match it.
        assert_eq!(preserve_cursor(Some(A), 2, &[D, B, A]), 2);
    }

    #[test]
    fn vanished_ticket_falls_back_to_same_index_clamped() {
        // C vanished; cursor was at 2, new list has 2 rows → clamp to 1.
        assert_eq!(preserve_cursor(Some(C), 2, &[A, B]), 1);
        // Cursor was at 0 and that ticket vanished → stay at 0.
        assert_eq!(preserve_cursor(Some(A), 0, &[B, C]), 0);
    }

    #[test]
    fn empty_new_list_pins_cursor_to_zero() {
        assert_eq!(preserve_cursor(Some(A), 2, &[]), 0);
    }

    #[test]
    fn no_prior_selection_clamps_index() {
        assert_eq!(preserve_cursor(None, 5, &[A, B]), 1);
        assert_eq!(preserve_cursor(None, 0, &[A, B]), 0);
    }

    #[test]
    fn indicator_renders_each_freshness_state() {
        assert_eq!(Freshness::Initial.indicator(), "");
        assert_eq!(Freshness::Fresh { secs_ago: 0 }.indicator(), "· ↻ just now");
        assert_eq!(Freshness::Fresh { secs_ago: 7 }.indicator(), "· ↻ 7s ago");
        assert_eq!(Freshness::Fresh { secs_ago: 130 }.indicator(), "· ↻ 2m ago");
        assert_eq!(
            Freshness::Stale { secs_ago: Some(42) }.indicator(),
            "· stale 42s ago"
        );
        assert_eq!(Freshness::Stale { secs_ago: None }.indicator(), "· stale");
    }

    #[test]
    fn the_cold_start_cycle_fetches_without_probing_first() {
        // Cycle 0 is the initial load: no ETag exists, so a probe could only
        // 200 and the GraphQL fetch would follow anyway.
        assert!(forces_full_fetch(0));
        // …and the safety-net cadence is unchanged: probe in between, full
        // fetch every FULL_REFRESH_EVERY cycles.
        for cycle in 1..FULL_REFRESH_EVERY {
            assert!(!forces_full_fetch(cycle), "cycle {cycle} should probe");
        }
        assert!(forces_full_fetch(FULL_REFRESH_EVERY));
        assert!(forces_full_fetch(FULL_REFRESH_EVERY * 2));
    }

    /// The `Discovered` payload for a set of repo slugs.
    fn found(slugs: &[&str]) -> MapIssues {
        slugs
            .iter()
            .enumerate()
            .map(|(i, slug)| ((*slug).to_string(), i as u64 + 1))
            .collect()
    }

    /// A cold start: no cached seed, search still out.
    fn cold() -> Startup {
        Startup::seeded(&found(&[]))
    }

    #[test]
    fn a_search_that_found_no_maps_is_loaded_not_loading() {
        // Zero mapped repos is an *answer*: there is nothing to wait for, so
        // the screen must stop claiming to be loading.
        let mut startup = cold();
        startup.searched(&found(&[]));
        assert!(startup.is_loaded());
        assert_eq!(startup.hint(), "");
    }

    #[test]
    fn loading_completes_when_every_discovered_map_has_reported() {
        let mut startup = cold();
        startup.searched(&found(&["a/one", "a/two", "a/three"]));
        assert_eq!(startup.hint(), "· loading maps 0/3");
        startup.record_arrival("a/one");
        assert_eq!(startup.hint(), "· loading maps 1/3");
        startup.record_arrival("a/two");
        assert_eq!(startup.hint(), "· loading maps 2/3");
        startup.record_arrival("a/three");
        assert!(startup.is_loaded());
    }

    #[test]
    fn a_fast_repo_polling_twice_does_not_complete_a_slow_repos_load() {
        // The pollers are concurrent on a 4s cadence, so a fast repo's second
        // cycle can beat a slow repo's first. Counting arrivals would call that
        // done; naming who is still pending cannot.
        let mut startup = cold();
        startup.searched(&found(&["fast/one", "slow/two"]));
        startup.record_arrival("fast/one");
        startup.record_arrival("fast/one");
        assert_eq!(
            startup.hint(),
            "· loading maps 1/2",
            "slow/two is still out"
        );
        startup.record_arrival("slow/two");
        assert!(startup.is_loaded());
    }

    #[test]
    fn arrivals_after_the_load_are_polls_not_startup() {
        // The pollers keep reporting for as long as wf runs; none of those
        // later fetches may push the screen back into a loading state.
        let mut startup = cold();
        startup.searched(&found(&["a/one"]));
        startup.record_arrival("a/one");
        startup.record_arrival("a/one");
        startup.record_arrival("b/two"); // never expected; not a regression
        assert!(startup.is_loaded());
        assert_eq!(startup.hint(), "");
    }

    #[test]
    fn the_hint_tells_the_two_pre_data_states_apart() {
        // Both draw an empty list; only the hint says why it is empty.
        assert_eq!(cold().hint(), "· searching for maps…");
        assert_eq!(
            Startup::seeded(&found(&["a/one", "a/two"])).hint(),
            "· loading maps 0/2"
        );
    }

    #[test]
    fn a_seeded_start_is_loading_its_cached_maps_not_searching() {
        // The #28 head start: the cache already named these maps, so the screen
        // says what it is waiting for from the first frame rather than spending
        // the search's ~2.5s claiming to be looking for maps at all.
        let mut startup = Startup::seeded(&found(&["a/one", "b/two"]));
        assert_eq!(startup.hint(), "· loading maps 0/2");
        startup.record_arrival("a/one");
        startup.record_arrival("b/two");
        // Every *cached* map is in, but the search has yet to confirm the set —
        // and confirming it is exactly what may add a repo mapped since the last
        // run, so the load is not over until both are true.
        assert!(!startup.is_loaded(), "the search has not answered yet");
        assert_eq!(startup.hint(), "· searching for maps…");
        startup.searched(&found(&["a/one", "b/two"]));
        assert!(startup.is_loaded());
    }

    #[test]
    fn the_search_overrules_a_seed_it_disagrees_with() {
        // Cached "b/two" lost its map and "c/three" gained one since last run.
        let mut startup = Startup::seeded(&found(&["a/one", "b/two"]));
        startup.record_arrival("a/one");
        let mut authoritative = found(&["a/one"]);
        authoritative.insert("c/three".to_string(), 9);
        startup.searched(&authoritative);
        assert_eq!(
            startup.hint(),
            "· loading maps 1/2",
            "b/two is no longer waited on; c/three now is"
        );
        startup.record_arrival("c/three");
        assert!(startup.is_loaded());
    }

    #[test]
    fn probe_classifies_304_as_unchanged_despite_nonzero_exit() {
        // gh api exits 1 on a 304; the status line is the truth.
        let head = "HTTP/2.0 304 Not Modified\nAccess-Control-Allow-Origin: *\n";
        assert!(matches!(parse_probe(head, false), Ok(Probe::Unchanged)));
    }

    #[test]
    fn probe_extracts_etag_on_200() {
        let head = "HTTP/2.0 200 OK\nEtag: W/\"abc123\"\nVary: Accept\n\n[{}]";
        match parse_probe(head, true) {
            Ok(Probe::Changed { etag }) => assert_eq!(etag.as_deref(), Some("W/\"abc123\"")),
            other => panic!("expected Changed, got {:?}", other.map(|_| ()).err()),
        }
    }

    #[test]
    fn probe_real_failure_is_an_error() {
        let head = "HTTP/2.0 502 Bad Gateway\n";
        assert!(parse_probe(head, false).is_err());
    }
}
