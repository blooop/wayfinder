//! Accretive project discovery (Build 3, per the #4 final resolution).
//!
//! No registry, no background scan: a project joins `wf` the first time
//! `wf` is explicitly used inside its checkout (the zoxide model). Touched
//! checkouts live in a per-machine cache — per-machine because checkout
//! existence is per-machine, and a *cache* because deleting it costs
//! nothing but re-opening each project once.
//!
//! Cache location: `~/.cache/wf/projects.json` (`$XDG_CACHE_HOME`
//! respected via the `dirs` crate; the home directory is resolved at
//! runtime, never hardcoded). A missing or corrupt file loads as empty.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::model::MapSet;

/// One touched checkout: where it lives, and which repo its `origin` points at.
///
/// It used to carry a third field — a short nickname derived from the path set,
/// which existed only to name the multiplexer session this checkout's tabs
/// lived in. Build 7 (#34) deleted the multiplexer, and nothing else ever read
/// it: a launch is *(checkout, ticket, map)*, and where two checkouts of one
/// repo must be told apart, the path is what tells them apart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkout {
    /// Absolute path to the checkout's toplevel.
    pub path: PathBuf,
    /// Full repo slug from `origin` (e.g. "blooop/wayfinder").
    pub repo: String,
    /// When `wf` was last *used* here, in seconds since the Unix epoch —
    /// opened in this checkout, or launched an agent from it.
    ///
    /// This is what orders the project list, and the reason it is a local
    /// stamp rather than the tracker's `updatedAt`: the list is drawn before
    /// any network call, so the only ordering it can honour at frame zero is
    /// one already on disk. Map activity says which project the *world* touched
    /// last; this says which one *you* did, which is the question a launcher is
    /// answering.
    ///
    /// `None` on entries written before the list existed, and on nothing else.
    /// Unknown sorts last rather than being guessed into place — the same
    /// answer [`crate::app::App::scoped_clusters`] gives an activity stamp it
    /// could not parse — and corrects itself the first time the project is
    /// opened.
    #[serde(default)]
    pub used: Option<u64>,
}

impl Checkout {
    /// A checkout registered now. The only constructor, so a registration can
    /// never forget to stamp itself and sink to the bottom of the list it just
    /// joined.
    #[must_use]
    pub fn new(path: PathBuf, repo: String) -> Self {
        Self {
            path,
            repo,
            used: Some(now_secs()),
        }
    }
}

/// Now, in seconds since the Unix epoch — `0` on a clock set before it, which
/// no ordering can do anything sensible with anyway and which sorts as the
/// oldest known use rather than as unknown.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The registered repos in most-recently-used order — the project list's body.
///
/// A *repo*, not a checkout: two checkouts of one repo (the `~/k1/kinisi_ros`,
/// `~/k2/kinisi_ros` pattern) are two places one project can run, not two
/// projects, and they share its maps. So a repo's stamp is the newest of its
/// checkouts' — the last time you used the project, wherever you used it from.
///
/// Newest first, unstamped last, then the slug: an order that never shuffles
/// between frames, because the cursor is walking it.
#[must_use]
pub fn mru_repos(checkouts: &[Checkout]) -> Vec<String> {
    let mut newest: std::collections::BTreeMap<&str, Option<u64>> =
        std::collections::BTreeMap::new();
    for checkout in checkouts {
        let slot = newest.entry(checkout.repo.as_str()).or_default();
        *slot = (*slot).max(checkout.used);
    }
    let mut repos: Vec<(Option<u64>, &str)> = newest.into_iter().map(|(r, u)| (u, r)).collect();
    // `Reverse` on the stamp puts the newest first, and `None < Some` reversed
    // puts the unstamped at the end — the same key shape the cluster order uses
    // for an activity timestamp that did not parse.
    repos.sort_by_key(|&(used, repo)| (std::cmp::Reverse(used), repo));
    repos
        .into_iter()
        .map(|(_, repo)| repo.to_string())
        .collect()
}

/// The per-machine cache of touched checkouts, plus the last map search's
/// findings (#28).
///
/// The findings are a set of [`MapId`](crate::model::MapId)s, not held on a
/// [`Checkout`]: which issues are a repo's maps is a property of the repo, and
/// two checkouts of one repo would otherwise each carry a copy that can
/// disagree with the other. A set of ids rather than a per-repo number because
/// a repo can hold several open maps at once (#50) — the old
/// `repo → one number` table could not even represent that.
///
/// There is deliberately no third "not yet searched" state. The search is
/// unconditional — the cache is a head start, never a skip (see
/// [`crate::refresh::spawn_discovery`]) — so nothing ever branches on *why* a
/// repo is absent from `open_maps`, only on the fact that it has no head
/// start. A state nothing can observe is a state not worth modelling.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectsCache {
    pub checkouts: Vec<Checkout>,
    /// Every open map the last successful search found. A fresh field name
    /// (not the pre-#50 `maps` table) on purpose: an older cache file's `maps`
    /// is skipped as an unknown field, so its checkouts survive the upgrade
    /// and only the head start is lost — once.
    #[serde(default)]
    pub open_maps: MapSet,
}

impl ProjectsCache {
    /// Load the cache, treating a missing or unparseable file as empty —
    /// it is a cache, not truth.
    pub fn load_or_default(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Persist the cache, creating parent directories as needed.
    ///
    /// # Errors
    ///
    /// An unwritable cache directory or file. The counterpart
    /// [`load_or_default`](Self::load_or_default) swallows its errors because a
    /// cache that will not load is merely empty; one that will not *save*
    /// silently loses the registration this run just made, so it is reported.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating cache dir {}", dir.display()))?;
        }
        let json = serde_json::to_vec_pretty(self)?;
        std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))
    }

    /// Register (or refresh) a touched checkout. Sorted by path so the
    /// which-checkout picker is stable between runs — the *project* list takes
    /// its own order from [`mru_repos`] rather than from this one.
    ///
    /// Registering is a use, so it stamps: opening `wf` in a checkout is what
    /// puts that project at the top of the list next time.
    pub fn register(&mut self, path: PathBuf, repo: String) {
        match self.checkouts.iter_mut().find(|c| c.path == path) {
            Some(entry) => {
                entry.repo = repo;
                entry.used = Some(now_secs());
            }
            None => self.checkouts.push(Checkout::new(path, repo)),
        }
        self.checkouts.sort_by(|a, b| a.path.cmp(&b.path));
        self.forget_unknown_maps();
    }

    /// Stamp the checkout an agent is about to be launched in.
    ///
    /// The second half of "use", and the half that matters for a project you
    /// reach through the list rather than through your shell: entering a
    /// project is navigation and might be a wrong turn, but launching from it
    /// is the act the ordering is trying to predict. Unknown paths are ignored
    /// — a launch resolves against this very cache, so there is no such thing.
    ///
    /// Returns whether anything changed, so the caller can skip a write on the
    /// path where the terminal is already being handed over.
    pub fn touch(&mut self, path: &Path) -> bool {
        match self.checkouts.iter_mut().find(|c| c.path == path) {
            Some(entry) => {
                entry.used = Some(now_secs());
                true
            }
            None => false,
        }
    }

    /// The head start (#28): every open map the last search found, for repos
    /// still in the cache. Read before the first frame, so the loaders start
    /// fetching at `t≈0` instead of after the ~2.5 s search.
    pub fn map_seed(&self) -> MapSet {
        self.open_maps.clone()
    }

    /// Record what a search over `searched` found, so the next run has a head
    /// start. A searched repo keeps exactly the maps the search named — a repo
    /// whose maps all closed is simply dropped, because absence *is* "no head
    /// start", and that is all this set means.
    pub fn record_search(&mut self, searched: &[String], found: &MapSet) {
        self.open_maps
            .retain(|id| !searched.contains(&id.repo) || found.contains(id));
        self.open_maps.extend(
            found
                .iter()
                .filter(|id| searched.contains(&id.repo))
                .cloned(),
        );
    }

    /// Drop findings for repos no checkout points at any more — the seed must
    /// not outlive the checkouts that justify fetching it.
    fn forget_unknown_maps(&mut self) {
        let repos = self.repos();
        self.open_maps
            .retain(|id| repos.binary_search(&id.repo).is_ok());
    }

    /// Drop checkouts whose directory no longer exists. A deleted checkout must
    /// stop offering itself as somewhere an agent could run.
    ///
    /// Returns whether anything was removed, so the caller can skip a write.
    /// Existence is one `stat` per entry: cheap enough for the path that runs
    /// before the first frame. `is_dir()` is also the deliberately lenient
    /// test — a checkout on an unmounted volume comes back when it mounts.
    pub fn prune_missing(&mut self) -> bool {
        let before = self.checkouts.len();
        self.checkouts.retain(|c| c.path.is_dir());
        let removed = self.checkouts.len() != before;
        if removed {
            self.forget_unknown_maps();
        }
        removed
    }

    /// Unique repo slugs across all cached checkouts, sorted. Several
    /// checkouts of one repo yield the slug once — they share its map.
    pub fn repos(&self) -> Vec<String> {
        let mut repos: Vec<String> = self.checkouts.iter().map(|c| c.repo.clone()).collect();
        repos.sort();
        repos.dedup();
        repos
    }
}

/// The default per-machine cache file: `<XDG cache dir>/wf/projects.json`.
pub fn default_cache_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("wf").join("projects.json"))
}

/// Parse a GitHub remote URL into an `owner/name` slug. Handles the ssh
/// scp-like form (`git@github.com:owner/name.git`), https
/// (`https://github.com/owner/name[.git]`), and `ssh://` URLs. Non-GitHub
/// remotes yield `None` — such checkouts are not wf projects.
pub fn parse_github_remote(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("https://github.com/"))
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("git://github.com/"))?;
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let mut parts = rest.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    if owner.is_empty() || name.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

async fn git_stdout(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Is `dir` inside a git checkout whose `origin` points at GitHub? Returns
/// the checkout toplevel and the repo slug — the registration act's input.
/// `None` for non-checkouts and non-GitHub remotes alike: wf ignores both.
pub async fn discover_checkout(dir: &Path) -> Option<(PathBuf, String)> {
    let toplevel = git_stdout(dir, &["rev-parse", "--show-toplevel"]).await?;
    let toplevel = PathBuf::from(toplevel);
    let origin = git_stdout(&toplevel, &["remote", "get-url", "origin"]).await?;
    let slug = parse_github_remote(&origin)?;
    Some((toplevel, slug))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MapId;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    /// A checkout with a stamp of its own, so an ordering assertion is not a
    /// race against the clock.
    fn used(path: &str, repo: &str, at: Option<u64>) -> Checkout {
        Checkout {
            path: p(path),
            repo: repo.to_string(),
            used: at,
        }
    }

    #[test]
    fn projects_are_listed_most_recently_used_first() {
        let repos = mru_repos(&[
            used("/a", "blooop/old", Some(100)),
            used("/b", "blooop/new", Some(300)),
            used("/c", "blooop/mid", Some(200)),
        ]);
        assert_eq!(repos, ["blooop/new", "blooop/mid", "blooop/old"]);
    }

    #[test]
    fn an_unstamped_checkout_sorts_last_rather_than_being_guessed_into_place() {
        // What an entry written before `used` existed looks like: unknown is
        // not zero and not now, it is *after* everything known — the same
        // answer the cluster order gives an activity stamp it could not parse.
        // It corrects itself the first time the project is opened.
        let repos = mru_repos(&[
            used("/a", "blooop/unstamped", None),
            used("/b", "blooop/ancient", Some(1)),
        ]);
        assert_eq!(repos, ["blooop/ancient", "blooop/unstamped"]);
    }

    #[test]
    fn two_checkouts_of_one_repo_are_one_project_stamped_by_the_newer() {
        // The `~/k1`, `~/k2` pattern: two places one project can run, sharing
        // its maps. Listing it twice would be listing one project twice, and
        // taking the older stamp would sink a project you used an hour ago.
        let repos = mru_repos(&[
            used("/k1/dotfiles", "blooop/dotfiles", Some(100)),
            used("/k2/dotfiles", "blooop/dotfiles", Some(400)),
            used("/proj/wayfinder", "blooop/wayfinder", Some(300)),
        ]);
        assert_eq!(repos, ["blooop/dotfiles", "blooop/wayfinder"]);
    }

    #[test]
    fn registering_stamps_and_touching_restamps() {
        // The two halves of "use": opening `wf` in a checkout, and launching
        // an agent from one. A project reached through the list only ever gets
        // the second, which is why it exists.
        let mut cache = ProjectsCache::default();
        cache.register(p("/data/proj/wayfinder"), "blooop/wayfinder".to_string());
        let stamped = cache.checkouts[0].used.expect("registering stamps");

        // An entry loaded from an older cache file carries no stamp; a launch
        // from it gives it one.
        cache.checkouts[0].used = None;
        assert!(cache.touch(&p("/data/proj/wayfinder")));
        assert!(cache.checkouts[0].used.expect("touching stamps") >= stamped);

        // A path the cache does not know is not silently added: a launch
        // resolves against this very cache, so there is no such path.
        assert!(!cache.touch(&p("/data/proj/elsewhere")));
        assert_eq!(cache.checkouts.len(), 1);
    }

    #[test]
    fn an_older_cache_file_without_stamps_still_loads() {
        // The upgrade path, which must not cost anyone their registry: the
        // field is new, so every existing entry lacks it.
        let cache: ProjectsCache = serde_json::from_str(
            r#"{"checkouts":[{"path":"/data/proj/wayfinder","repo":"blooop/wayfinder"}]}"#,
        )
        .expect("an older cache file parses");
        assert_eq!(cache.checkouts[0].used, None);
        assert_eq!(mru_repos(&cache.checkouts), ["blooop/wayfinder"]);
    }

    #[test]
    fn cache_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("wf-test-{}", std::process::id()));
        let file = dir.join("nested").join("projects.json");
        let mut cache = ProjectsCache::default();
        cache.register(p("/data/proj/wayfinder"), "blooop/wayfinder".to_string());
        cache.register(p("/data/k1/kinisi_ros"), "kinisi/kinisi_ros".to_string());
        cache.save(&file).expect("save");
        let loaded = ProjectsCache::load_or_default(&file);
        assert_eq!(loaded, cache);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_or_corrupt_cache_loads_as_empty() {
        assert_eq!(
            ProjectsCache::load_or_default(Path::new("/nonexistent/wf/projects.json")),
            ProjectsCache::default()
        );
        let dir = std::env::temp_dir().join(format!("wf-test-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("projects.json");
        std::fs::write(&file, b"{ not json").unwrap();
        assert_eq!(
            ProjectsCache::load_or_default(&file),
            ProjectsCache::default()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_older_cache_file_still_loads() {
        // Three shape changes so far, and a cache is not truth: the seed
        // shipped after the cache did (#28), so an older file simply has no
        // head start; the per-checkout session name went with the multiplexer
        // (#34); and the `repo → one map` table gave way to `open_maps` (#50).
        // A file carrying any of the old shapes is read straight past them —
        // the checkouts always survive.
        let dir = std::env::temp_dir().join(format!("wf-test-old-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("projects.json");
        std::fs::write(
            &file,
            br#"{"checkouts": [{"path": "/data/proj/wayfinder",
                 "repo": "blooop/wayfinder", "session": "wayfinder"}],
                 "maps": {"blooop/wayfinder": 1}}"#,
        )
        .unwrap();
        let cache = ProjectsCache::load_or_default(&file);
        assert_eq!(cache.checkouts.len(), 1, "the checkouts must survive");
        assert!(
            cache.map_seed().is_empty(),
            "the pre-#50 table is not a seed"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_seed_is_what_the_last_search_found_for_the_repos_it_searched() {
        let mut cache = ProjectsCache::default();
        cache.register(p("/data/proj/wayfinder"), "blooop/wayfinder".to_string());
        cache.register(p("/data/proj/dotfiles"), "blooop/dotfiles".to_string());
        let searched = cache.repos();

        // Two open maps on one repo both seed — the whole point of #50.
        let found: MapSet = [
            MapId::new("blooop/wayfinder", 1),
            MapId::new("blooop/wayfinder", 47),
        ]
        .into_iter()
        .collect();
        cache.record_search(&searched, &found);
        assert_eq!(cache.map_seed(), found, "an unmapped repo is simply absent");

        // One map closed, the other repo gained one: the search is the
        // authority, so the seed follows it rather than accumulating.
        let found: MapSet = [
            MapId::new("blooop/wayfinder", 47),
            MapId::new("blooop/dotfiles", 7),
        ]
        .into_iter()
        .collect();
        cache.record_search(&searched, &found);
        assert_eq!(cache.map_seed(), found);

        // And maps that have gone stop being a head start.
        cache.record_search(&searched, &MapSet::new());
        assert!(cache.map_seed().is_empty());
    }

    #[test]
    fn a_repo_with_no_checkouts_left_keeps_no_head_start() {
        // Nothing would ever fetch it, and a seed nothing fetches is just a
        // stale number waiting to be believed by a future version.
        let root = std::env::temp_dir().join(format!("wf-test-seed-{}", std::process::id()));
        let gone = root.join("wayfinder");
        std::fs::create_dir_all(&gone).unwrap();
        let mut cache = ProjectsCache::default();
        cache.register(gone.clone(), "blooop/wayfinder".to_string());
        cache.record_search(
            &cache.repos(),
            &[MapId::new("blooop/wayfinder", 1)].into_iter().collect(),
        );
        assert!(!cache.map_seed().is_empty());

        std::fs::remove_dir_all(&gone).unwrap();
        assert!(cache.prune_missing());
        assert!(cache.map_seed().is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn register_upserts_by_path_and_dedups_repos() {
        let mut cache = ProjectsCache::default();
        cache.register(p("/data/k1/kinisi_ros"), "kinisi/kinisi_ros".to_string());
        cache.register(p("/data/k2/kinisi_ros"), "kinisi/kinisi_ros".to_string());
        cache.register(p("/data/k1/kinisi_ros"), "kinisi/kinisi_ros".to_string());
        assert_eq!(
            cache.checkouts.len(),
            2,
            "re-registering must not duplicate"
        );
        assert_eq!(cache.repos(), vec!["kinisi/kinisi_ros".to_string()]);
    }

    #[test]
    fn pruning_forgets_deleted_checkouts() {
        let root = std::env::temp_dir().join(format!("wf-test-prune-{}", std::process::id()));
        let live = root.join("projects").join("wayfinder");
        let gone = root.join("wayfinder");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::create_dir_all(&gone).unwrap();

        let mut cache = ProjectsCache::default();
        cache.register(live.clone(), "blooop/wayfinder".to_string());
        cache.register(gone.clone(), "blooop/wayfinder".to_string());
        assert_eq!(cache.checkouts.len(), 2);

        std::fs::remove_dir_all(&gone).unwrap();
        assert!(cache.prune_missing(), "a deleted checkout is a change");
        assert_eq!(cache.checkouts.len(), 1);
        assert_eq!(cache.checkouts[0].path, live);

        assert!(
            !cache.prune_missing(),
            "nothing left to prune is not a change"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn checkouts_stay_sorted_by_path_so_the_picker_is_stable() {
        // The which-checkout modal lists them in cache order and the human
        // builds muscle memory on it, so registration order must not show.
        let mut cache = ProjectsCache::default();
        cache.register(p("/data/k2/kinisi_ros"), "kinisi/kinisi_ros".to_string());
        cache.register(p("/data/k1/kinisi_ros"), "kinisi/kinisi_ros".to_string());
        let paths: Vec<&Path> = cache.checkouts.iter().map(|c| c.path.as_path()).collect();
        assert_eq!(
            paths,
            vec![p("/data/k1/kinisi_ros"), p("/data/k2/kinisi_ros")]
        );
    }

    #[test]
    fn parses_ssh_scp_form() {
        assert_eq!(
            parse_github_remote("git@github.com:blooop/wayfinder.git").as_deref(),
            Some("blooop/wayfinder")
        );
        assert_eq!(
            parse_github_remote("git@github.com:blooop/wayfinder").as_deref(),
            Some("blooop/wayfinder")
        );
    }

    #[test]
    fn parses_https_and_ssh_url_forms() {
        assert_eq!(
            parse_github_remote("https://github.com/blooop/wayfinder.git").as_deref(),
            Some("blooop/wayfinder")
        );
        assert_eq!(
            parse_github_remote("https://github.com/blooop/wayfinder").as_deref(),
            Some("blooop/wayfinder")
        );
        assert_eq!(
            parse_github_remote("ssh://git@github.com/blooop/wayfinder.git").as_deref(),
            Some("blooop/wayfinder")
        );
    }

    #[test]
    fn rejects_non_github_and_malformed_remotes() {
        assert_eq!(parse_github_remote("git@gitlab.com:o/r.git"), None);
        assert_eq!(parse_github_remote("https://example.com/o/r"), None);
        assert_eq!(parse_github_remote("https://github.com/only-owner"), None);
        assert_eq!(parse_github_remote("https://github.com/a/b/c"), None);
    }
}
