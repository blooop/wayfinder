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
}

/// The per-machine cache of touched checkouts, plus the last map search's
/// findings (#28).
///
/// The findings are a set of [`MapId`]s, not held on a [`Checkout`]: which
/// issues are a repo's maps is a property of the repo, and two checkouts of
/// one repo would otherwise each carry a copy that can disagree with the
/// other. A set of ids rather than a per-repo number because a repo can hold
/// several open maps at once (#50) — the old `repo → one number` table could
/// not even represent that.
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
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating cache dir {}", dir.display()))?;
        }
        let json = serde_json::to_vec_pretty(self)?;
        std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))
    }

    /// Register (or refresh) a touched checkout. Sorted by path so the
    /// which-checkout picker is stable between runs.
    pub fn register(&mut self, path: PathBuf, repo: String) {
        match self.checkouts.iter_mut().find(|c| c.path == path) {
            Some(entry) => entry.repo = repo,
            None => self.checkouts.push(Checkout { path, repo }),
        }
        self.checkouts.sort_by(|a, b| a.path.cmp(&b.path));
        self.forget_unknown_maps();
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
        self.open_maps
            .extend(found.iter().filter(|id| searched.contains(&id.repo)).cloned());
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
        assert_eq!(ProjectsCache::load_or_default(&file), ProjectsCache::default());
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
        assert!(cache.map_seed().is_empty(), "the pre-#50 table is not a seed");
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
        assert_eq!(cache.checkouts.len(), 2, "re-registering must not duplicate");
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

        assert!(!cache.prune_missing(), "nothing left to prune is not a change");
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
