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
//!
//! Session names are a pure function of the cached path set, recomputed on
//! every registration (see [`derive_sessions`] for the rule).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::launch::MapIssues;

/// One touched checkout: where it lives, which repo its `origin` points at,
/// and the zellij session name derived for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkout {
    /// Absolute path to the checkout's toplevel.
    pub path: PathBuf,
    /// Full repo slug from `origin` (e.g. "blooop/wayfinder").
    pub repo: String,
    /// Derived zellij session name (Build 4 reads this at launch time).
    pub session: String,
}

/// The per-machine cache of touched checkouts, plus the last map search's
/// findings (#28).
///
/// The findings are keyed by **repo slug**, not held on a [`Checkout`]: which
/// issue is a repo's map is a property of the repo, and two checkouts of one
/// repo would otherwise each carry a copy that can disagree with the other.
///
/// There is deliberately no third "not yet searched" state. The search is
/// unconditional — the cache is a head start, never a skip (see
/// [`crate::refresh::spawn_discovery`]) — so nothing ever branches on *why* a
/// repo is absent from `maps`, only on the fact that it has no head start.
/// A state nothing can observe is a state not worth modelling.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectsCache {
    pub checkouts: Vec<Checkout>,
    /// `repo slug → map issue number`, as of the last successful search.
    /// Absent from an older cache file, which simply means no head start.
    #[serde(default)]
    pub maps: MapIssues,
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

    /// Register (or refresh) a touched checkout, then recompute every
    /// session name so names stay a pure function of the cached path set.
    pub fn register(&mut self, path: PathBuf, repo: String) {
        match self.checkouts.iter_mut().find(|c| c.path == path) {
            Some(entry) => entry.repo = repo,
            None => self.checkouts.push(Checkout {
                path,
                repo,
                session: String::new(),
            }),
        }
        self.checkouts.sort_by(|a, b| a.path.cmp(&b.path));
        self.recompute_sessions();
        self.forget_unknown_maps();
    }

    /// The head start (#28): every map issue number the last search found, for
    /// repos still in the cache. Read before the first frame, so the pollers
    /// start fetching at `t≈0` instead of after the ~2.5 s search.
    pub fn map_seed(&self) -> MapIssues {
        self.maps.clone()
    }

    /// Record what a search over `searched` found, so the next run has a head
    /// start. Repos that were searched and have no map are simply dropped —
    /// absence *is* "no head start", and that is all this table means.
    pub fn record_search(&mut self, searched: &[String], found: &MapIssues) {
        for repo in searched {
            match found.get(repo) {
                Some(&number) => {
                    self.maps.insert(repo.clone(), number);
                }
                None => {
                    self.maps.remove(repo);
                }
            }
        }
    }

    /// Drop findings for repos no checkout points at any more — the seed must
    /// not outlive the checkouts that justify fetching it.
    fn forget_unknown_maps(&mut self) {
        let repos = self.repos();
        self.maps
            .retain(|repo, _| repos.binary_search(repo).is_ok());
    }

    /// Drop checkouts whose directory no longer exists, then recompute the
    /// session names. A deleted checkout must stop offering itself as a launch
    /// host — and because names are a pure function of the *surviving* path
    /// set, the last checkout of a repo goes back to its plain directory name
    /// (`~/proj/wayfinder` → `wayfinder`, not `proj`).
    ///
    /// Returns whether anything was removed, so the caller can skip a write.
    /// Existence is one `stat` per entry: cheap enough for the path that runs
    /// before the first frame. `exists()` is also the deliberately lenient
    /// test — a checkout on an unmounted volume comes back when it mounts.
    pub fn prune_missing(&mut self) -> bool {
        let before = self.checkouts.len();
        self.checkouts.retain(|c| c.path.is_dir());
        let removed = self.checkouts.len() != before;
        if removed {
            self.recompute_sessions();
            self.forget_unknown_maps();
        }
        removed
    }

    fn recompute_sessions(&mut self) {
        let paths: Vec<PathBuf> = self.checkouts.iter().map(|c| c.path.clone()).collect();
        for (checkout, session) in self.checkouts.iter_mut().zip(derive_sessions(&paths)) {
            checkout.session = session;
        }
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

/// Derive zellij session names for a set of checkout paths.
///
/// The rule (deterministic, order-independent):
/// 1. A checkout's session is its directory name (`~/proj/wayfinder` →
///    `wayfinder`).
/// 2. If several checkouts share that directory name, each uses its
///    *parent* directory name instead — the k1–k5 pattern:
///    `~/k1/kinisi_ros` → `k1`.
/// 3. Any names still colliding after step 2 fall back to the checkout's
///    path relative to home (or the filesystem root) with `/` → `-`.
pub fn derive_sessions(paths: &[PathBuf]) -> Vec<String> {
    fn component(path: &Path) -> Option<String> {
        path.file_name().map(|s| s.to_string_lossy().into_owned())
    }
    let mut names: Vec<String> = paths
        .iter()
        .map(|p| component(p).unwrap_or_else(|| path_slug(p)))
        .collect();

    // Step 2: duplicate leaf names → parent directory name.
    let counts = tally(&names);
    for (i, path) in paths.iter().enumerate() {
        if counts[&names[i]] > 1 {
            if let Some(parent) = path.parent().and_then(component) {
                names[i] = parent;
            }
        }
    }

    // Step 3: anything still colliding → full path slug.
    let counts = tally(&names);
    for (i, path) in paths.iter().enumerate() {
        if counts[&names[i]] > 1 {
            names[i] = path_slug(path);
        }
    }
    names
}

fn tally(names: &[String]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for name in names {
        *counts.entry(name.clone()).or_insert(0) += 1;
    }
    counts
}

/// A path flattened to a session-safe name: relative to home when under it
/// (resolved at runtime), separators replaced by `-`.
fn path_slug(path: &Path) -> String {
    let rel = dirs::home_dir()
        .and_then(|home| path.strip_prefix(&home).ok())
        .unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .filter(|c| c != "/")
        .collect::<Vec<_>>()
        .join("-")
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
    fn a_cache_file_written_before_the_seed_existed_still_loads() {
        // The seed shipped after the cache did (#28); an older file simply has
        // no head start, which is the one thing absence from `maps` ever means.
        let dir = std::env::temp_dir().join(format!("wf-test-old-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("projects.json");
        std::fs::write(
            &file,
            br#"{"checkouts": [{"path": "/data/proj/wayfinder",
                 "repo": "blooop/wayfinder", "session": "wayfinder"}]}"#,
        )
        .unwrap();
        let cache = ProjectsCache::load_or_default(&file);
        assert_eq!(cache.checkouts.len(), 1, "the checkouts must survive");
        assert!(cache.map_seed().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_seed_is_what_the_last_search_found_for_the_repos_it_searched() {
        let mut cache = ProjectsCache::default();
        cache.register(p("/data/proj/wayfinder"), "blooop/wayfinder".to_string());
        cache.register(p("/data/proj/dotfiles"), "blooop/dotfiles".to_string());
        let searched = cache.repos();

        let found: MapIssues = [("blooop/wayfinder".to_string(), 1)].into_iter().collect();
        cache.record_search(&searched, &found);
        assert_eq!(cache.map_seed(), found, "an unmapped repo is simply absent");

        // The map moved and the other repo gained one: the search is the
        // authority, so the seed follows it rather than accumulating.
        let found: MapIssues = [
            ("blooop/wayfinder".to_string(), 42),
            ("blooop/dotfiles".to_string(), 7),
        ]
        .into_iter()
        .collect();
        cache.record_search(&searched, &found);
        assert_eq!(cache.map_seed(), found);

        // And a map that has gone stops being a head start.
        cache.record_search(&searched, &MapIssues::new());
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
            &[("blooop/wayfinder".to_string(), 1)].into_iter().collect(),
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
    fn pruning_forgets_deleted_checkouts_and_renames_the_survivor() {
        // Two checkouts of one repo, so both sessions are disambiguated by
        // parent dir; delete one and the survivor gets its plain name back.
        let root = std::env::temp_dir().join(format!("wf-test-prune-{}", std::process::id()));
        let live = root.join("projects").join("wayfinder");
        let gone = root.join("wayfinder");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::create_dir_all(&gone).unwrap();

        let mut cache = ProjectsCache::default();
        cache.register(live.clone(), "blooop/wayfinder".to_string());
        cache.register(gone.clone(), "blooop/wayfinder".to_string());
        let root_name = root.file_name().unwrap().to_string_lossy().into_owned();
        let sessions: Vec<&str> = cache.checkouts.iter().map(|c| c.session.as_str()).collect();
        assert_eq!(
            sessions,
            vec!["projects", root_name.as_str()],
            "colliding leaf names are disambiguated by parent dir"
        );

        std::fs::remove_dir_all(&gone).unwrap();
        assert!(cache.prune_missing(), "a deleted checkout is a change");
        assert_eq!(cache.checkouts.len(), 1);
        assert_eq!(cache.checkouts[0].path, live);
        assert_eq!(cache.checkouts[0].session, "wayfinder");

        assert!(!cache.prune_missing(), "nothing left to prune is not a change");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unique_dir_names_are_the_session_names() {
        assert_eq!(
            derive_sessions(&[p("/data/proj/wayfinder"), p("/data/proj/dotfiles")]),
            vec!["wayfinder", "dotfiles"]
        );
    }

    #[test]
    fn duplicate_dir_names_use_the_parent_dir() {
        // The k1–k5 pattern: independent checkouts of one repo.
        assert_eq!(
            derive_sessions(&[
                p("/data/k1/kinisi_ros"),
                p("/data/k2/kinisi_ros"),
                p("/data/proj/wayfinder"),
            ]),
            vec!["k1", "k2", "wayfinder"]
        );
    }

    #[test]
    fn residual_collisions_fall_back_to_path_slugs() {
        // Parents collide too: /a/x/proj and /b/x/proj both yield "x".
        let sessions = derive_sessions(&[p("/a/x/proj"), p("/b/x/proj")]);
        assert_eq!(sessions, vec!["a-x-proj", "b-x-proj"]);
    }

    #[test]
    fn registration_recomputes_existing_session_names() {
        let mut cache = ProjectsCache::default();
        cache.register(p("/data/k1/kinisi_ros"), "kinisi/kinisi_ros".to_string());
        assert_eq!(cache.checkouts[0].session, "kinisi_ros");
        cache.register(p("/data/k2/kinisi_ros"), "kinisi/kinisi_ros".to_string());
        let sessions: Vec<&str> = cache.checkouts.iter().map(|c| c.session.as_str()).collect();
        assert_eq!(sessions, vec!["k1", "k2"]);
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
