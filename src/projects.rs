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

use crate::launch::{Agent, Isolation};
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

/// A conversation a previous launch left behind on this machine, and the way
/// back into it (#35).
///
/// Deliberately small, because resuming needs almost nothing. Neither agent
/// resumes by session id: `claude --continue` continues "the most recent
/// conversation in the current directory", and `codex resume --last` filters
/// by cwd unless told otherwise. `wf` already gives every node a cwd of its
/// own — that is what the per-node workspace *is* — so the way back to a
/// conversation is simply to exec in the same place. Which leaves three facts
/// worth keeping and no fourth — the three that between them say *where the
/// conversation is*: which tree, whether it ran in that tree or in the node's
/// container, and which CLI ran there.
///
/// The agent is here because it cannot be re-derived at all: a Claude
/// conversation is not rejoinable by Codex, and a resume that guessed would
/// open an empty session in the right directory and call it a rejoin. The
/// workspace name is *not* here for the opposite reason — it is a pure
/// function of the node, so a copy could only ever disagree with a fresh
/// derivation.
///
/// What this does **not** claim is that a conversation exists. It says `wf`
/// launched an agent here, which is the strongest thing `wf` can know without
/// reading the agent's own store — a container's store, for an isolated
/// launch, at a path devpod chose. A launch that died before its agent wrote
/// anything leaves a resume row that lands on the agent's own "no conversation
/// found", and that is the honest failure: the row promises what `wf` did, not
/// what the agent managed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resume {
    /// Which CLI ran, and therefore which one can rejoin it.
    pub agent: Agent,
    /// The tree the launch exec'd in — the checkout on the host, and the
    /// tree whose devcontainer an isolated launch's container was built from.
    pub checkout: PathBuf,
    /// Whether the agent ran on the host or inside the node's container.
    ///
    /// Recorded rather than re-detected, unlike everything else here, because
    /// it is a fact about **where the conversation is** and not about the
    /// tree's current shape. An isolated launch's history lives at a cwd
    /// inside `dl`'s own clone; re-detecting from a checkout that has since
    /// lost its `.devcontainer/` would answer Host and silently resume the
    /// checkout's own, different conversation. A `dl` that has gone missing
    /// makes the exec fail loudly instead, which is the better of the two.
    pub isolation: Isolation,
    /// When it was launched, seconds since the Unix epoch — what the picker
    /// row reads back as "20m ago".
    pub at: u64,
}

/// A [`Resume`], keyed to the node whose conversation it is.
///
/// Flattened on the wire, so a session reads as one object rather than a node
/// with a nested record — and so the file stays legible to the human who
/// deletes it, this being a cache and not truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// The node's repo, full slug — matched whole, since a fork and its
    /// upstream share a short name (#15).
    pub repo: String,
    /// The ticket or map number the launch was aimed at.
    pub number: u64,
    #[serde(flatten)]
    pub resume: Resume,
}

impl Session {
    /// A session recorded now. The only constructor, for the same reason
    /// [`Checkout::new`] is: a record that forgot to stamp itself would read
    /// as a resume from the epoch.
    #[must_use]
    pub fn new(
        repo: String,
        number: u64,
        agent: Agent,
        checkout: PathBuf,
        isolation: Isolation,
    ) -> Self {
        Self {
            repo,
            number,
            resume: Resume {
                agent,
                checkout,
                isolation,
                at: now_secs(),
            },
        }
    }
}

/// A scratch path in the same directory as `path`, for a write that will be
/// renamed over it.
///
/// Same directory on purpose: a rename is only atomic within one filesystem,
/// and a temp directory elsewhere would silently degrade to copy-then-replace —
/// exactly the torn write this seam exists to remove. Hidden, so a cache
/// directory does not visibly grow scratch files while a save is in flight.
///
/// # Errors
///
/// A path with no file name — a directory, or a root — which is not something
/// a cache can be saved to.
fn scratch_beside(path: &Path) -> Result<PathBuf> {
    /// Distinguishes two saves *within* one process. The pid alone separates
    /// instances; the picker also saves from a discovery task while the launch
    /// path may be saving from the main thread.
    static SAVES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    let name = path.file_name().with_context(|| {
        format!(
            "{} is not a file the cache can be written to",
            path.display()
        )
    })?;
    let nth = SAVES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(path.with_file_name(format!(
        ".{}.{}.{nth}.tmp",
        name.to_string_lossy(),
        std::process::id()
    )))
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
    /// The conversations previous launches left on this machine (#35), at most
    /// one per node.
    ///
    /// Here rather than in a file of its own because it is the same *kind* of
    /// thing the rest of this cache is: a per-machine fact `wf` accumulates by
    /// being used, cheap to lose, rebuilt by the next launch. It also lands
    /// where the write already happens — the handover already loads this cache
    /// to stamp the checkout it is about to exec in.
    #[serde(default)]
    pub sessions: Vec<Session>,
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
    /// **Written beside and renamed over**, never into the file itself, because
    /// several writers share this path and `load_or_default` cannot tell a torn
    /// read from a corrupt file. Truncate-then-write left a window in which
    /// another `wf` — the discovery task, a launch handing over, a second
    /// instance — loaded the empty default, and *its* next save wrote that
    /// emptiness back over every checkout, seed and session on the machine. A
    /// rename within one directory is atomic, so a reader sees the whole
    /// previous registry or the whole new one and there is no third answer.
    ///
    /// The temp name carries the writing process and a counter within it, so
    /// two savers racing cannot land on the same scratch file and hand each
    /// other half a registry through it.
    ///
    /// What this deliberately does *not* buy is durability across a crash:
    /// there is no `fsync` before the rename, so a machine that loses power
    /// mid-save may come back to either version. That is the right trade for a
    /// cache whose whole cost of loss is re-opening each project once, and
    /// paying for it would put a disk flush on the path to the first frame.
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
        let temp = scratch_beside(path)?;
        std::fs::write(&temp, json).with_context(|| format!("writing {}", temp.display()))?;
        if let Err(err) = std::fs::rename(&temp, path) {
            // Leaving the scratch file behind would litter a cache directory
            // nobody ever cleans, and the failure being reported is the rename.
            std::fs::remove_file(&temp).ok();
            return Err(err).with_context(|| format!("replacing {}", path.display()));
        }
        Ok(())
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

    /// Record that an agent was launched on a node, here — the fact a later
    /// resume is offered on.
    ///
    /// **Upserts by node**, so a node has at most one resume. Both agents
    /// resume by cwd, so one node in one tree has exactly one conversation to
    /// come back to; keeping the older record would offer a second door to the
    /// same place, and — once the launch moved to another checkout — a door
    /// into the wrong one. The newest launch is the one the human means.
    pub fn record_session(&mut self, session: Session) {
        self.sessions
            .retain(|s| !(s.repo == session.repo && s.number == session.number));
        self.sessions.push(session);
    }

    /// The conversation a previous launch of this node left, if there is one
    /// to rejoin.
    pub fn resume(&self, repo: &str, number: u64) -> Option<&Resume> {
        self.sessions
            .iter()
            .find(|s| s.repo == repo && s.number == number)
            .map(|s| &s.resume)
    }

    /// Drop resumes into trees that are gone. Called by
    /// [`prune_missing`](Self::prune_missing), because a resume's whole content
    /// is a place to exec in.
    fn forget_unreachable_sessions(&mut self) {
        self.sessions.retain(|s| s.resume.checkout.is_dir());
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
        let before = (self.checkouts.len(), self.sessions.len());
        self.checkouts.retain(|c| c.path.is_dir());
        self.forget_unreachable_sessions();
        let removed = (self.checkouts.len(), self.sessions.len()) != before;
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

    /// A registry big enough that a truncate-then-write is observably torn: a
    /// reader that lands inside the window sees an empty or half-parsed file,
    /// which [`ProjectsCache::load_or_default`] reports as "no projects at
    /// all".
    fn a_crowded_registry() -> ProjectsCache {
        let mut cache = ProjectsCache::default();
        for i in 0..500 {
            cache.register(
                p(&format!("/data/proj/checkout-{i}")),
                format!("blooop/r{i}"),
            );
        }
        cache
    }

    #[test]
    fn a_reader_in_another_instance_never_sees_a_half_written_registry() {
        // The loss this guards against is total, not partial: a torn read
        // parses as corrupt, loads as empty, and that instance's next save
        // writes the empty registry back over every checkout, seed and session
        // on the machine. So the claim is not "rarely torn" but "never": a
        // reader only ever sees a whole registry, whatever a writer is doing.
        let dir = std::env::temp_dir().join(format!("wf-test-torn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("projects.json");
        let cache = a_crowded_registry();
        let expected = cache.checkouts.len();
        cache.save(&file).expect("seed");

        let writing = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let writer = {
            let (file, writing) = (file.clone(), std::sync::Arc::clone(&writing));
            std::thread::spawn(move || {
                for _ in 0..150 {
                    cache.save(&file).expect("save");
                }
                writing.store(false, std::sync::atomic::Ordering::Release);
            })
        };
        let mut torn = 0;
        let mut reads = 0;
        while writing.load(std::sync::atomic::Ordering::Acquire) {
            reads += 1;
            if ProjectsCache::load_or_default(&file).checkouts.len() != expected {
                torn += 1;
            }
        }
        writer.join().expect("writer");
        std::fs::remove_dir_all(&dir).ok();
        assert!(
            reads > 0,
            "the reader never got a turn, so nothing was tested"
        );
        assert_eq!(
            torn, 0,
            "{torn} of {reads} reads saw a registry that was not there"
        );
    }

    #[test]
    fn a_save_replaces_the_file_rather_than_rewriting_it_in_place() {
        // The deterministic half of the claim above, and the reason it holds:
        // the old file is never opened for writing, so a reader already
        // holding it keeps reading the whole previous registry until it lets
        // go. A hard link stands in for that reader.
        let dir = std::env::temp_dir().join(format!("wf-test-replace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("projects.json");
        let mut before = ProjectsCache::default();
        before.register(p("/data/proj/wayfinder"), "blooop/wayfinder".to_string());
        before.save(&file).expect("save");

        let held = dir.join("held-open.json");
        std::fs::hard_link(&file, &held).expect("hard link");
        let mut after = before.clone();
        after.register(p("/data/k1/kinisi_ros"), "kinisi/kinisi_ros".to_string());
        after.save(&file).expect("save");

        assert_eq!(ProjectsCache::load_or_default(&file), after);
        assert_eq!(
            ProjectsCache::load_or_default(&held),
            before,
            "the previous registry was overwritten under a reader still holding it"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_save_leaves_nothing_behind_beside_the_cache() {
        // The temp file a save writes through is an implementation detail of
        // this seam and must not become litter in a cache directory nobody
        // ever cleans.
        let dir = std::env::temp_dir().join(format!("wf-test-litter-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("projects.json");
        let mut cache = ProjectsCache::default();
        cache.register(p("/data/proj/wayfinder"), "blooop/wayfinder".to_string());
        cache.save(&file).expect("save");
        cache.save(&file).expect("save again");
        let left: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(left, vec![file]);
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
    fn a_launch_leaves_a_resume_the_node_can_be_rejoined_by() {
        // The one fact resuming needs, and one `wf` creates rather than
        // discovers: an agent ran on this node, in this tree, on this machine.
        let mut cache = ProjectsCache::default();
        cache.record_session(Session::new(
            "blooop/wayfinder".to_string(),
            117,
            Agent::Claude,
            p("/data/proj/wayfinder"),
            Isolation::Host,
        ));
        let resume = cache
            .resume("blooop/wayfinder", 117)
            .expect("the node it was recorded for finds it");
        assert_eq!(resume.agent, Agent::Claude);
        assert_eq!(resume.checkout, p("/data/proj/wayfinder"));
        // Every other node is untouched: a resume belongs to one node, and
        // offering a neighbour's conversation would rejoin the wrong work.
        assert_eq!(cache.resume("blooop/wayfinder", 118), None);
        assert_eq!(cache.resume("blooop/devlaunch", 117), None);
    }

    #[test]
    fn relaunching_a_node_replaces_its_resume_rather_than_stacking_them() {
        // Both agents resume by *cwd* (`claude -c`, `codex resume --last`), so
        // one node in one tree has exactly one conversation to come back to.
        // Keeping the older record would offer a second door to the same
        // place — and, when the tree changed, the wrong place.
        let mut cache = ProjectsCache::default();
        let node = || ("blooop/wayfinder".to_string(), 117);
        cache.record_session(Session::new(
            node().0,
            node().1,
            Agent::Claude,
            p("/data/k1/wayfinder"),
            Isolation::Host,
        ));
        cache.record_session(Session::new(
            node().0,
            node().1,
            Agent::Codex,
            p("/data/k2/wayfinder"),
            Isolation::Host,
        ));
        assert_eq!(cache.sessions.len(), 1);
        let resume = cache.resume("blooop/wayfinder", 117).expect("the newer");
        assert_eq!(resume.agent, Agent::Codex);
        assert_eq!(resume.checkout, p("/data/k2/wayfinder"));
    }

    #[test]
    fn a_resume_whose_checkout_is_gone_is_pruned_with_it() {
        // The resume names a tree to exec in. Once that tree is deleted the
        // conversation is unreachable, so the row must stop being offered —
        // the same rule the checkout it points at already obeys.
        let root = std::env::temp_dir().join(format!("wf-test-resume-{}", std::process::id()));
        let live = root.join("live");
        let gone = root.join("gone");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::create_dir_all(&gone).unwrap();

        let mut cache = ProjectsCache::default();
        cache.register(live.clone(), "blooop/wayfinder".to_string());
        cache.register(gone.clone(), "blooop/wayfinder".to_string());
        cache.record_session(Session::new(
            "blooop/wayfinder".to_string(),
            117,
            Agent::Claude,
            live.clone(),
            Isolation::Devlaunch,
        ));
        cache.record_session(Session::new(
            "blooop/wayfinder".to_string(),
            121,
            Agent::Claude,
            gone.clone(),
            Isolation::Host,
        ));

        std::fs::remove_dir_all(&gone).unwrap();
        assert!(cache.prune_missing());
        assert!(cache.resume("blooop/wayfinder", 117).is_some());
        assert_eq!(
            cache.resume("blooop/wayfinder", 121),
            None,
            "a resume into a deleted tree must stop being offered"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_cache_file_written_before_resume_existed_still_loads() {
        // The fourth shape change, and the same rule as the other three: the
        // checkouts survive, and the only thing missing is a resume nobody
        // could have recorded yet.
        let cache: ProjectsCache = serde_json::from_str(
            r#"{"checkouts":[{"path":"/data/proj/wayfinder","repo":"blooop/wayfinder"}]}"#,
        )
        .expect("a cache file without sessions parses");
        assert!(cache.sessions.is_empty());
        assert_eq!(cache.resume("blooop/wayfinder", 117), None);
    }

    #[test]
    fn a_resume_round_trips_through_disk_with_the_agent_that_ran() {
        // Which agent ran is the half that cannot be re-derived: a Claude
        // conversation cannot be rejoined by Codex, so if this field does not
        // survive the write, resume comes back offering the wrong CLI.
        let dir = std::env::temp_dir().join(format!("wf-test-sessions-{}", std::process::id()));
        let file = dir.join("projects.json");
        let mut cache = ProjectsCache::default();
        cache.register(p("/data/proj/wayfinder"), "blooop/wayfinder".to_string());
        cache.record_session(Session::new(
            "blooop/wayfinder".to_string(),
            117,
            Agent::Codex,
            p("/data/proj/wayfinder"),
            Isolation::Host,
        ));
        cache.save(&file).expect("save");
        assert_eq!(ProjectsCache::load_or_default(&file), cache);
        std::fs::remove_dir_all(&dir).ok();
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
