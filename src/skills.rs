//! The prompts `wf` execs, shipped in the same package as the binary.
//!
//! `wf` does not merely *mention* `wf-tdd` and `wf-auto` — it hardcodes them
//! in [`crate::launch::route`] and execs them. That makes the skill files part
//! of `wf`'s interface, and an interface split across two repos on two release
//! cadences is one that drifts: `wf` reached 0.6.0 still routing `defer` at a
//! `wayfinder` section that `wayfinder-auto` had superseded weeks earlier, and
//! nothing anywhere could notice.
//!
//! So the skills live in this repo, under `skills/`, and the conda recipe
//! installs them beside the binary at `<prefix>/share/wf/skills`. One package,
//! one version, one `pixi global update wf`.
//!
//! What this module does *not* do is teach an agent where to look. Claude Code
//! reads `~/.claude/skills` and Codex reads `~/.codex/skills`, so the last step
//! is a **symlink** from each ([`install`]) — a link rather than a copy, so that
//! a name is `wf`'s only when `wf` put it there, and a real directory can go on
//! meaning *somebody else owns this*.
//!
//! **What the link points at is the part that is not obvious.** Not the package
//! bundle: an isolated launch ([`crate::launch::Isolation`], #80) may run the
//! agent in a container under a different home directory. A link into
//! `~/.pixi/envs/wf/share/wf/skills` is a perfectly good link on the host and a
//! dangling one in there, and a dangling skill is not an error anyone reports:
//! it is an unknown-skill prompt seconds after a launch, with nothing to say why
//! (#107).
//!
//! So [`install`] puts a **copy of the bundle beside the links** —
//! `<config>/wf-skills`, beside each agent's links — and links to it *relatively*
//! (`../wf-skills/wf-tdd`). The copy is then the thing this module has to keep
//! honest, and it does that in three places rather than trusting it:
//! [`install`] rewrites it and records where it came from, [`refresh`] brings it
//! back in step with *that* source at every launch — links and all, so a build
//! that ships a new skill needs no second command — and [`status`] reports it as
//! [`State::Outdated`] when it has drifted — so a stale copy is something that
//! gets *seen*, rather than a prompt that quietly runs a release behind.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::launch::Agent;

/// The skills this build ships — the ones [`crate::launch::Route`] can exec,
/// plus the single-ticket sibling they share their tracker notes with.
///
/// Named explicitly rather than read from the bundle directory: this list is
/// what the routing table promises, so a skill that vanishes from the package
/// should be a *reported* absence, not a silently shorter list.
///
/// Every name is prefixed `wf`, because each agent's skills directory is a flat
/// namespace shared with every other source of skills the user has. Unprefixed,
/// `tdd` and `review` are names `wf` *squats on* rather than merely occupies —
/// while it holds one, the user cannot have their own, and [`install`] would
/// refuse to link over theirs if they made one (#104).
///
/// Renaming this list is what [`sweep`] exists to clean up after.
pub const BUNDLED: [&str; 6] = ["wf", "wf-auto", "wf-mid", "wf-one", "wf-tdd", "wf-review"];

/// Overrides the bundle location. The escape hatch for a build that is not
/// installed — `wf-next` copied onto `PATH`, or a test — and the way to point
/// a released `wf` at a checkout you are editing.
pub const BUNDLE_ENV: &str = "WF_SKILLS_DIR";

/// Claude Code's own config-directory override, honoured so that `wf` installs
/// where the agent will actually read from rather than where it usually does.
const CLAUDE_CONFIG_ENV: &str = "CLAUDE_CONFIG_DIR";

/// Codex's configuration root. It is the parent of `skills/`, unlike Claude's
/// override which names the config directory whose child is `skills/`.
const CODEX_HOME_ENV: &str = "CODEX_HOME";

/// The copy of the bundle the links point at, as a name inside the selected
/// agent's config directory (`~/.claude/wf-skills` or `~/.codex/wf-skills`).
///
/// A sibling of `skills/` rather than a directory inside it, because the agents
/// read *every* directory under `skills/` — a copy in there would register five
/// more skills under a second set of names.
pub const MIRROR: &str = "wf-skills";

/// Where the copy came from, recorded inside it: the bundle directory the last
/// [`install`] copied. A file rather than an inference, because the answer
/// outlives the process that knew it — see [`installed_from`].
const SOURCE: &str = ".source";

/// Where this build's skills are, and how that was decided — carried together
/// because the answer is only actionable with the reason attached: "no bundle"
/// and "no bundle *and here is where I looked*" are different messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    pub path: PathBuf,
    pub found_by: FoundBy,
}

/// Which of the three resolutions answered. Ordered by precedence, and
/// exhaustive: there is no fourth place skills are ever looked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoundBy {
    /// `$WF_SKILLS_DIR` named it.
    Env,
    /// `<prefix>/share/wf/skills`, beside the binary — an installed package.
    Installed,
    /// `<repo>/skills`, two levels above a `target/<profile>/wf` — a build
    /// from this checkout, so `cargo run` and `target/release/wf` find the
    /// skills they were built alongside without being told.
    Checkout,
}

impl Bundle {
    /// Resolve this build's bundle, or say where it looked.
    ///
    /// The three candidates are tried in precedence order and the first that
    /// *exists* wins — existence, not merely a plausible path, because a
    /// released binary and a dev build disagree about which of the last two is
    /// even meaningful and neither can tell which it is.
    ///
    /// # Errors
    ///
    /// When none of the three candidates is a directory. The error names every
    /// path tried, because "not found" without them is unactionable.
    pub fn resolve() -> Result<Bundle> {
        let mut tried: Vec<PathBuf> = Vec::new();
        for (found_by, candidate) in Bundle::candidates() {
            let Some(candidate) = candidate else { continue };
            if candidate.is_dir() {
                return Ok(Bundle {
                    path: candidate,
                    found_by,
                });
            }
            tried.push(candidate);
        }
        let tried: Vec<String> = tried.iter().map(|p| format!("  {}", p.display())).collect();
        anyhow::bail!(
            "no bundled skills found for this build. Looked in:\n{}\n\
             Set {BUNDLE_ENV} to a skills directory — a checkout's `skills/` \
             works, and is what you want while editing them.",
            tried.join("\n")
        )
    }

    /// The candidate bundle paths in precedence order. `None` for a candidate
    /// this build cannot form at all — no env var set, or an executable with
    /// too few ancestors to sit in a prefix.
    fn candidates() -> Vec<(FoundBy, Option<PathBuf>)> {
        let exe = std::env::current_exe().ok();
        // `<bin>/wf` → `<prefix>` → `<prefix>/share/wf/skills`.
        let installed = exe
            .as_deref()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .map(|prefix| prefix.join("share").join("wf").join("skills"));
        // `<repo>/target/<profile>/wf` → `<repo>` → `<repo>/skills`.
        let checkout = exe
            .as_deref()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .and_then(Path::parent)
            .map(|repo| repo.join("skills"));
        vec![
            (
                FoundBy::Env,
                std::env::var_os(BUNDLE_ENV)
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from),
            ),
            (FoundBy::Installed, installed),
            (FoundBy::Checkout, checkout),
        ]
    }
}

/// Where `agent` reads personal skills from.
///
/// # Errors
///
/// When the relevant agent override is unset and the home directory cannot be
/// resolved — there is then nowhere to install to and nothing to guess.
pub fn skills_dir(agent: Agent) -> Result<PathBuf> {
    match agent {
        Agent::Claude => {
            if let Some(configured) = std::env::var_os(CLAUDE_CONFIG_ENV).filter(|v| !v.is_empty())
            {
                return Ok(PathBuf::from(configured).join("skills"));
            }
            let home = dirs::home_dir().context("cannot resolve the home directory")?;
            Ok(home.join(".claude").join("skills"))
        }
        Agent::Codex => {
            if let Some(configured) = std::env::var_os(CODEX_HOME_ENV).filter(|v| !v.is_empty()) {
                return Ok(PathBuf::from(configured).join("skills"));
            }
            let home = dirs::home_dir().context("cannot resolve the home directory")?;
            Ok(home.join(".codex").join("skills"))
        }
    }
}

/// Where an install goes: the selected agent's skills directory, and the copy
/// of the bundle that sits beside it.
///
/// One type rather than two paths passed around together, because they are not
/// independent. The links are relative — `../wf-skills/<name>` — so the copy is
/// a **sibling** of the links directory or the links resolve to nothing at all,
/// and that relationship is the only reason a link written on the host still
/// works inside a container that mounted the tree somewhere else.
/// [`Target::beside`] is the only constructor, so the pair cannot be built
/// disagreeing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    links: PathBuf,
    mirror: PathBuf,
}

impl Target {
    /// The pair formed around a links directory.
    ///
    /// # Errors
    ///
    /// When `links` has no parent to put the copy in — a bare relative name, or
    /// the filesystem root. There is then no sibling for `..` to reach.
    pub fn beside(links: &Path) -> Result<Target> {
        let parent = links
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .with_context(|| {
                format!(
                    "{} has no parent directory to keep the skills copy in",
                    links.display()
                )
            })?;
        Ok(Target {
            links: links.to_path_buf(),
            mirror: parent.join(MIRROR),
        })
    }

    /// The pair `wf` actually installs into on this machine.
    ///
    /// # Errors
    ///
    /// When this agent's skills directory cannot be resolved.
    pub fn resolve(agent: Agent) -> Result<Target> {
        Target::beside(&skills_dir(agent)?)
    }

    /// The directory the selected agent reads.
    pub fn links(&self) -> &Path {
        &self.links
    }

    /// The copy those links point at.
    pub fn mirror(&self) -> &Path {
        &self.mirror
    }

    /// The exact link [`install`] writes for `name`. Relative, and one `..`
    /// deep, which is the whole portability argument: nothing in it names a
    /// home directory, so the host and the container read the same link and
    /// both find the copy.
    fn link_target(name: &str) -> PathBuf {
        Path::new("..").join(MIRROR).join(name)
    }
}

/// What sits at `<links>/<name>` — a fact about the link alone, settled before
/// anything has been read about what it points at.
///
/// Separate from [`State`] on purpose: [`State::Outdated`] is a statement about
/// *content*, so it cannot be an answer this function is able to give, and
/// keeping the two apart is what lets [`install`] match exhaustively without an
/// arm for a case that cannot reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Link {
    /// The link `wf` writes, character for character.
    Current,
    /// A symlink, but not that one — an older `wf`'s link into a package
    /// prefix, a checkout someone pointed at once, or an absolute path into the
    /// copy that would dangle in a container. Relinking is safe: nothing but a
    /// link is lost. `None` when the target itself could not be read — still a
    /// link and still safely replaceable, but with nothing to report or to
    /// prove ownership of, and carried as the absence it is rather than as an
    /// empty path a report would print.
    Stale(Option<PathBuf>),
    /// A real directory. Somebody else owns this — chezmoi, a hand-edit, a
    /// plugin — so `wf` reports it and does not touch it. Deleting a directory
    /// it did not create is not `wf`'s call to make.
    Unmanaged,
    /// Nothing there.
    Missing,
}

impl Link {
    /// The link's state, plus what was found behind it: a link that is right
    /// still runs the wrong prompt if the copy it points at is not this
    /// build's.
    fn with_copy(self, copy_is_current: bool, copied_from: Option<PathBuf>) -> State {
        match self {
            Link::Current if copy_is_current => State::Current,
            Link::Current => State::Outdated { copied_from },
            Link::Stale(target) => State::Stale(target),
            Link::Unmanaged => State::Unmanaged,
            Link::Missing => State::Missing,
        }
    }
}

/// What one agent's `skills/<name>` amounts to right now — the link and the
/// prompt behind it, together, because "which prompt is going to run" is the
/// only question worth answering and neither half settles it alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// The link is `wf`'s and the copy behind it is this build's bundle.
    Current,
    /// The link is right, but the copy it points at is not this build's — a
    /// `pixi global update wf` since the last install, or an install made from
    /// somewhere else entirely. The prompt that runs is a real one, just not
    /// this one, so the answer is only useful with the *where* attached:
    /// `copied_from` is what [`install`] recorded, and `None` means a copy old
    /// enough to predate the record.
    Outdated { copied_from: Option<PathBuf> },
    /// A symlink pointing somewhere else entirely — or, `None`, one whose
    /// target could not be read at all.
    Stale(Option<PathBuf>),
    /// A real directory another tool owns.
    Unmanaged,
    /// Nothing there.
    Missing,
}

impl State {
    /// Whether this state means this build's prompt is what would run.
    fn is_current(&self) -> bool {
        matches!(self, State::Current)
    }
}

/// One skill's name and where it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub name: String,
    pub state: State,
}

/// Inspect every bundled skill against `target`.
pub fn status(bundle: &Bundle, target: &Target) -> Vec<Status> {
    let copied_from = installed_from(target);
    BUNDLED
        .iter()
        .map(|name| Status {
            name: (*name).to_string(),
            state: link_state(name, &target.links.join(name)).with_copy(
                same_tree(&bundle.path.join(name), &target.mirror.join(name)),
                copied_from.clone(),
            ),
        })
        .collect()
}

/// The directory the copy was last made from, as [`install`] recorded it.
///
/// The record is what keeps [`refresh`] from being a bully. The copy has one
/// source — the bundle of whichever `wf` installed it, which is the package's
/// for everyone and a checkout's for anyone who set `$WF_SKILLS_DIR` — and a
/// launch by a *different* `wf` has no business overwriting it with its own
/// prompts. Without the record, `wf skills install` from a checkout would be
/// undone by the first ordinary launch, silently.
///
/// `None` when nothing was ever installed here, or when the record is from a
/// `wf` old enough not to have written one.
pub fn installed_from(target: &Target) -> Option<PathBuf> {
    let recorded = std::fs::read_to_string(target.mirror.join(SOURCE)).ok()?;
    let recorded = recorded.trim();
    (!recorded.is_empty()).then(|| PathBuf::from(recorded))
}

/// Classify one link. `symlink_metadata` rather than `metadata`, so a link is
/// seen as a link instead of as whatever it points at — the whole distinction
/// this function exists to draw.
///
/// The target is compared *literally* against the one path [`install`] writes,
/// rather than canonically. Canonically was right while the link named a bundle
/// that could be reached by more than one route; it is wrong now, because an
/// absolute link into the copy canonicalises to the same place on this machine
/// and dangles in the container — which is exactly the state this shape exists
/// to stop calling healthy.
fn link_state(name: &str, link: &Path) -> Link {
    let Ok(meta) = std::fs::symlink_metadata(link) else {
        return Link::Missing;
    };
    if !meta.file_type().is_symlink() {
        return Link::Unmanaged;
    }
    match std::fs::read_link(link) {
        Ok(target) if target == Target::link_target(name) => Link::Current,
        Ok(target) => Link::Stale(Some(target)),
        Err(_) => Link::Stale(None),
    }
}

/// Whether two directory trees hold the same names and the same bytes.
///
/// The copy is a copy, so "is the prompt that will run this build's?" is a
/// content question, and no stat answers it: `pixi global update wf` rewrites
/// the bundle in the prefix without touching anything under an agent's config
/// directory. The
/// trees are a handful of small markdown files, so comparing bytes is cheaper
/// than being clever about it, and an unreadable side answers `false` — "not
/// known to match" is the only safe direction, and it costs one re-copy.
fn same_tree(a: &Path, b: &Path) -> bool {
    let (Ok(mut left), Ok(mut right)) = (listing(a), listing(b)) else {
        return false;
    };
    left.sort();
    right.sort();
    if left != right {
        return false;
    }
    left.iter().all(|name| {
        let (a, b) = (a.join(name), b.join(name));
        if a.is_dir() {
            same_tree(&a, &b)
        } else {
            matches!((std::fs::read(&a), std::fs::read(&b)), (Ok(x), Ok(y)) if x == y)
        }
    })
}

/// The names directly inside `dir`.
fn listing(dir: &Path) -> std::io::Result<Vec<std::ffi::OsString>> {
    std::fs::read_dir(dir)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect()
}

/// Copy `src` over `dst`, replacing whatever was there.
///
/// Whole-directory rather than file-by-file: a prompt the bundle *dropped* must
/// not survive in the copy, and a mirror that only ever grows would eventually
/// hold a file no build ships.
fn recopy(src: &Path, dst: &Path) -> Result<()> {
    clear(dst)?;
    copy_tree(src, dst)
}

/// Remove `path`, whatever it is, and say nothing if it was not there.
/// `symlink_metadata` so that a link is removed as a link rather than followed.
fn clear(path: &Path) -> Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
    .with_context(|| format!("cannot replace {}", path.display()))
}

/// Copy a tree of directories and regular files. The bundle is exactly that —
/// markdown beside markdown — so there is no symlink or special file case to
/// get right, and one appearing would be a packaging fault worth an error.
fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst).with_context(|| format!("cannot create {}", dst.display()))?;
    for entry in std::fs::read_dir(src).with_context(|| format!("cannot read {}", src.display()))? {
        let entry = entry.with_context(|| format!("cannot read {}", src.display()))?;
        let (from, to) = (entry.path(), dst.join(entry.file_name()));
        let kind = entry
            .file_type()
            .with_context(|| format!("cannot stat {}", from.display()))?;
        if kind.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .with_context(|| format!("cannot copy {} → {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

/// What one skill's installation did — reported rather than printed, so the
/// caller owns the wording and the tests can assert on the outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// It already pointed here, and the copy behind it was already this
    /// build's.
    AlreadyCurrent,
    /// The link was already right; the copy behind it was rewritten from this
    /// build's bundle. What a `pixi global update wf` leaves for the next
    /// install to pick up.
    Refreshed,
    /// A link was created, or repointed from `was`.
    Linked { was: Option<PathBuf> },
    /// A real directory is in the way; nothing was touched.
    Blocked,
    /// The bundle does not contain this skill — a packaging fault, reported
    /// rather than papered over with a dangling link.
    NotInBundle,
}

/// Copy every bundled skill beside `target`'s links and link each one to its
/// copy, creating both directories if needed.
///
/// Idempotent by construction: a link already pointing at its copy is left
/// alone rather than rewritten, and a copy that already matches the bundle is
/// not re-copied, so running it twice is indistinguishable from running it once.
///
/// # Errors
///
/// When a directory cannot be created, a copy cannot be written, or a link
/// cannot be replaced. A skill that is merely *blocked* or absent from the
/// bundle is not an error — it is an [`Outcome`], so one bad skill never costs
/// the other four their install.
pub fn install(bundle: &Bundle, target: &Target) -> Result<Vec<(String, Outcome)>> {
    for dir in [&target.links, &target.mirror] {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let mut done = Vec::new();
    for name in BUNDLED {
        let source = bundle.path.join(name);
        let link = target.links.join(name);
        let copy = target.mirror.join(name);
        if !source.is_dir() {
            done.push((name.to_string(), Outcome::NotInBundle));
            continue;
        }
        let state = link_state(name, &link);
        // A blocked skill's copy is never read, so it is never written: the
        // whole point of stopping here is to touch nothing on this skill's
        // behalf.
        if state == Link::Unmanaged {
            done.push((name.to_string(), Outcome::Blocked));
            continue;
        }
        let recopied = !same_tree(&source, &copy);
        if recopied {
            recopy(&source, &copy)?;
        }
        let outcome = match state {
            Link::Current if recopied => Outcome::Refreshed,
            Link::Current => Outcome::AlreadyCurrent,
            Link::Unmanaged => unreachable!("handled above"),
            state => {
                // A stale link is removed whether or not its target could be
                // read — either way it is only a link, and only a link is
                // lost. `was` keeps the target where there is one to name.
                let was = match state {
                    Link::Stale(target) => {
                        std::fs::remove_file(&link).with_context(|| {
                            format!("cannot replace the link {}", link.display())
                        })?;
                        target
                    }
                    _ => None,
                };
                std::os::unix::fs::symlink(Target::link_target(name), &link).with_context(
                    || format!("cannot link {} → {}", link.display(), copy.display()),
                )?;
                Outcome::Linked { was }
            }
        };
        done.push((name.to_string(), outcome));
    }
    prune_mirror(&target.mirror)?;
    // Written last, and unconditionally: from here on, *this* is the bundle the
    // copy tracks, and the launches that follow refresh it from there.
    let record = target.mirror.join(SOURCE);
    std::fs::write(&record, format!("{}\n", bundle.path.display()))
        .with_context(|| format!("cannot record the skills source in {}", record.display()))?;
    Ok(done)
}

/// What a launch put right on one skill's behalf — reported rather than
/// printed, for the same reason [`Outcome`] is: the caller owns the wording.
///
/// The distinction is which of the two is worth interrupting a launch to say.
/// Rewriting a copy is the errand [`refresh`] runs every time and would be
/// noise; creating a link changes *which prompts this machine has*, and a
/// release went by with that happening never rather than silently, which is
/// the same kind of invisible (#170).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Healed {
    /// The copy behind an existing link was rewritten from the recorded source.
    Copied,
    /// A link was created, or repointed from `was` — a link `wf` could prove it
    /// wrote itself.
    Linked { was: Option<PathBuf> },
}

/// Bring the installed skills back in step with the bundle they were installed
/// from — their contents *and* which of them this agent's directory has links
/// for at all — and name what moved.
///
/// This is what keeps "a copy" from meaning "a copy that goes stale". `wf` is
/// the thing that launches the agent, so the copy is rewritten by the very
/// process that is about to exec the prompt — a `pixi global update wf` cannot
/// get a launch ahead of it, and an edit to a checkout's `skills/` is live in
/// the next session exactly as it was when the link pointed at the checkout
/// itself.
///
/// The *set* is kept in step for the same reason the contents are, and it was
/// the larger hole. A skill a new build ships is one no install on this machine
/// ever saw, so a launch that considered only links already pointing at the
/// copy skipped it forever, and `pixi global update wf` rewrites the bundle in
/// the prefix while writing nothing under an agent's config directory that
/// could notice. The report that closed it: update, launch into a devcontainer,
/// and an unknown-skill prompt for one the release plainly shipped — with
/// nothing anywhere to connect the two (#170). So a link that is *missing* is
/// created, and a stale one is repointed only where its target
/// proves `wf` wrote it — the same test [`sweep`] removes links by, which is
/// what makes the pre-#110 link into the prefix safe to reclaim and a link
/// somebody else left impossible to touch.
///
/// Its source is [`installed_from`] and **not** this build's bundle, which is
/// the difference between refreshing and overwriting: a `WF_SKILLS_DIR` install
/// is a choice about which prompts run, and an ordinary launch has no standing
/// to undo it — nor may healing a link become the loophole that does, so a link
/// created here points at a copy of the recorded source like every other.
///
/// Nothing else is touched: a machine that never ran `wf skills install`, a
/// directory chezmoi owns, a link pointing somewhere `wf` never links, and a
/// recorded source that has since been deleted are all left exactly as they are
/// — a prompt one release behind still beats no prompt at all, and a name that
/// is not `wf`'s is not `wf`'s to take.
///
/// # Errors
///
/// When a copy cannot be rewritten, or a link cannot be created.
pub fn refresh(target: &Target) -> Result<Vec<(String, Healed)>> {
    let Some(bundle) = installed_from(target).filter(|source| source.is_dir()) else {
        return Ok(Vec::new());
    };
    let mut healed = Vec::new();
    for name in BUNDLED {
        let source = bundle.join(name);
        let copy = target.mirror.join(name);
        let link = target.links.join(name);
        if !source.is_dir() {
            continue;
        }
        // What the link needs, if anything. `None` leaves it exactly as it is;
        // `Some(was)` writes it, carrying whatever it displaced.
        let relink = match link_state(name, &link) {
            Link::Current => None,
            Link::Missing => Some(None),
            // Repointed only where `is_ours` can *prove* `wf` wrote it, which
            // is the whole permission argument: it covers the absolute link
            // into the package prefix every `wf` before #110 wrote — the one
            // that resolves on the host and dangles in the container — and it
            // can never match a link somebody else left, however dead it looks.
            // A target that could not be read is `Stale(None)`, and falls to
            // the arm below: with nothing to prove ownership of, the sweep has
            // no permission to touch it.
            Link::Stale(Some(points_at)) if is_ours(&points_at, &bundle) => Some(Some(points_at)),
            Link::Stale(_) | Link::Unmanaged => continue,
        };
        // Always the copy before the link, so a link this function creates
        // never points at a prompt that is not there yet.
        let copy_moved = !same_tree(&source, &copy);
        if copy_moved {
            recopy(&source, &copy)?;
        }
        match relink {
            Some(was) => {
                // `create_dir_all` because the links directory is Claude Code's
                // rather than `wf`'s: a machine that installed once can have
                // had it removed since, and a launch that cannot link is the
                // failure this is all here to stop.
                std::fs::create_dir_all(&target.links)
                    .with_context(|| format!("cannot create {}", target.links.display()))?;
                if was.is_some() {
                    std::fs::remove_file(&link)
                        .with_context(|| format!("cannot replace the link {}", link.display()))?;
                }
                std::os::unix::fs::symlink(Target::link_target(name), &link).with_context(
                    || format!("cannot link {} → {}", link.display(), copy.display()),
                )?;
                healed.push((name.to_string(), Healed::Linked { was }));
            }
            None if copy_moved => healed.push((name.to_string(), Healed::Copied)),
            None => {}
        }
    }
    Ok(healed)
}

/// Drop copies of skills this build no longer ships, leaving the source record.
///
/// Unlike [`sweep`], which has to *prove* a link is `wf`'s before removing it,
/// everything in here is `wf`'s by construction: the mirror is a directory `wf`
/// creates, fills and owns, and nothing else has a reason to write to it.
///
/// # Errors
///
/// When a copy cannot be removed. A missing mirror is not an error.
fn prune_mirror(mirror: &Path) -> Result<Vec<PathBuf>> {
    let Ok(entries) = std::fs::read_dir(mirror) else {
        return Ok(Vec::new());
    };
    let mut pruned = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("cannot read {}", mirror.display()))?;
        let name = entry.file_name();
        if name == std::ffi::OsStr::new(SOURCE)
            || BUNDLED.iter().any(|b| std::ffi::OsStr::new(b) == name)
        {
            continue;
        }
        clear(&entry.path())?;
        pruned.push(entry.path());
    }
    Ok(pruned)
}

/// Whether `target` is a link `wf` wrote — this build's, or one of the shapes an
/// older `wf` left behind.
///
/// This is the whole safety argument for [`sweep`]. A link is removed only when
/// it points at a place nothing but `wf` ever links to, so a skill the user
/// wrote, a plugin's, or a link some other tool left behind can never match
/// however dead it looks.
fn is_ours(target: &Path, bundle: &Path) -> bool {
    let Some(parent) = target.parent() else {
        return false;
    };
    // What this build writes: relative, into the copy beside the links.
    parent == Path::new("..").join(MIRROR)
        // What older builds wrote: straight into a bundle. The first covers
        // `$WF_SKILLS_DIR` pointing at a checkout, whose path ends in `skills`
        // and not in `share/wf/skills`; the suffix covers every installed
        // prefix, including the older ones this is here to clear.
        || parent == bundle
        || parent.ends_with("share/wf/skills")
}

/// Remove links `wf` wrote for skills it no longer ships.
///
/// A rename leaves residue that neither [`status`] nor [`install`] can see:
/// they iterate [`BUNDLED`], so the moment `wayfinder` left that list, the link
/// named `wayfinder` stopped being anything either function looks at — while
/// staying on disk, pointing into a bundle where its target no longer exists.
/// An agent would go on reading a dangling entry forever, and no amount of
/// `wf skills install` would mention it.
///
/// Scoped by where the link *points* rather than by a list of former names: a
/// hardcoded list would need editing at every rename and would still miss the
/// links left by a `wf` older than the list.
///
/// # Errors
///
/// When the links directory cannot be read, or a link cannot be removed. A
/// missing directory is not an error — there is nothing to sweep.
pub fn sweep(bundle: &Bundle, target: &Target) -> Result<Vec<PathBuf>> {
    let Ok(entries) = std::fs::read_dir(&target.links) else {
        return Ok(Vec::new());
    };
    let mut swept = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("cannot read {}", target.links.display()))?;
        let link = entry.path();
        let name = entry.file_name();
        if BUNDLED.iter().any(|b| std::ffi::OsStr::new(b) == name) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&link) else {
            continue;
        };
        if !meta.file_type().is_symlink() {
            continue;
        }
        let Ok(points_at) = std::fs::read_link(&link) else {
            continue;
        };
        if !is_ours(&points_at, &bundle.path) {
            continue;
        }
        std::fs::remove_file(&link)
            .with_context(|| format!("cannot remove the stale link {}", link.display()))?;
        swept.push(link);
    }
    Ok(swept)
}

/// The `wf skills` report: where the bundle is, where the links and the copy
/// they point at are, and the state of each — the answer to "which prompt is
/// actually going to run".
pub fn report(bundle: &Bundle, target: &Target) -> String {
    use std::fmt::Write;
    let source = match bundle.found_by {
        FoundBy::Env => BUNDLE_ENV,
        FoundBy::Installed => "installed beside the binary",
        FoundBy::Checkout => "this checkout",
    };
    let mut out = format!(
        "bundle  {} ({source})\nlinks   {}\ncopy    {} (what the links point at)\n\n",
        bundle.path.display(),
        target.links.display(),
        target.mirror.display()
    );
    // Inspected once and reported from that, rather than asked twice: the two
    // answers would be a stat apart, and a report whose list and whose verdict
    // disagreed would be the worst possible output for this command.
    let statuses = status(bundle, target);
    for Status { name, state } in &statuses {
        let line = state_line(state, &bundle.path);
        let _ = writeln!(out, "  {name:<15} {line}");
    }
    if statuses.iter().all(|s| s.state.is_current()) {
        out.push_str("\nEvery skill wf routes to is this build's own.\n");
    } else {
        out.push_str("\nRun `wf skills install` to link them.\n");
    }
    out
}

/// One state as the line `wf skills status` prints for it.
fn state_line(state: &State, bundle_path: &Path) -> String {
    match state {
        State::Current => "ok".to_string(),
        // Where from, when that is known and is not where this build's
        // bundle is: "outdated" alone leaves you guessing between a package
        // update you have not launched since and a checkout you installed
        // from months ago, and the fix is different.
        State::Outdated { copied_from } => match copied_from {
            Some(source) if source != bundle_path => {
                format!("outdated — the copy came from {}", source.display())
            }
            _ => "outdated — the copy is not this build's".to_string(),
        },
        State::Stale(Some(points_at)) => format!("stale — links to {}", points_at.display()),
        State::Stale(None) => "stale — the link's target could not be read".to_string(),
        State::Unmanaged => "not a link — another tool owns this one".to_string(),
        State::Missing => "missing".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch tree with a bundle and an empty config directory, removed on
    /// drop. No `tempfile` dependency for a handful of tests that need real
    /// paths — and these need *real* paths, because symlinks are what is under
    /// test.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!("wf-skills-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("config/skills")).expect("scratch");
            Scratch(dir)
        }

        /// A bundle holding every skill in [`BUNDLED`], minus any `omit`.
        fn bundle(&self, omit: &[&str]) -> Bundle {
            let path = self.0.join("bundle");
            for name in BUNDLED {
                if omit.contains(&name) {
                    continue;
                }
                write_skill(&path, name, "---\n---\n");
            }
            std::fs::create_dir_all(&path).expect("bundle");
            Bundle {
                path,
                found_by: FoundBy::Env,
            }
        }

        /// The config directory Claude Code would read: `skills/` and the copy
        /// beside it.
        fn target(&self) -> Target {
            Target::beside(&self.0.join("config/skills")).expect("a target")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Write one skill into a bundle — the way a package update rewrites one.
    fn write_skill(bundle: &Path, name: &str, body: &str) {
        let dir = bundle.join(name);
        std::fs::create_dir_all(&dir).expect("bundle skill");
        std::fs::write(dir.join("SKILL.md"), body).expect("SKILL.md");
    }

    /// Copy a tree the way a bind mount presents one: at a different absolute
    /// path, with symlinks carried across as symlinks rather than followed.
    /// This is how the container is reproduced in a unit test.
    fn remount(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).expect("mount point");
        for entry in std::fs::read_dir(from).expect("read") {
            let entry = entry.expect("entry");
            let (src, dst) = (entry.path(), to.join(entry.file_name()));
            let kind = entry.file_type().expect("file type");
            if kind.is_symlink() {
                let points_at = std::fs::read_link(&src).expect("link");
                std::os::unix::fs::symlink(points_at, &dst).expect("relink");
            } else if kind.is_dir() {
                remount(&src, &dst);
            } else {
                std::fs::copy(&src, &dst).expect("copy");
            }
        }
    }

    #[test]
    fn install_links_every_bundled_skill_and_is_idempotent() {
        let scratch = Scratch::new("fresh");
        let bundle = scratch.bundle(&[]);
        let target = scratch.target();

        let first = install(&bundle, &target).expect("install");
        assert!(
            first
                .iter()
                .all(|(_, o)| matches!(o, Outcome::Linked { was: None })),
            "{first:?}"
        );
        assert!(status(&bundle, &target)
            .iter()
            .all(|s| s.state == State::Current));

        // Twice is once: nothing is relinked and nothing is re-copied, so a
        // link is never briefly absent and no run reports work it did not do.
        let second = install(&bundle, &target).expect("reinstall");
        assert!(
            second.iter().all(|(_, o)| *o == Outcome::AlreadyCurrent),
            "{second:?}"
        );
    }

    #[test]
    fn the_links_are_relative_into_a_copy_that_survives_being_mounted_elsewhere() {
        // The #107 regression. A link into the package prefix is a good link on
        // the host and a dangling one inside the devcontainer, which mounts
        // `~/.claude` and nothing else, under a different home directory. So the
        // link names neither a prefix nor a home: one `..` into the copy beside
        // it, which rides the same mount.
        let scratch = Scratch::new("mounted");
        let bundle = scratch.bundle(&[]);
        let target = scratch.target();
        install(&bundle, &target).expect("install");

        for name in BUNDLED {
            assert_eq!(
                std::fs::read_link(target.links().join(name)).expect("a link"),
                PathBuf::from(format!("../{MIRROR}/{name}")),
                "{name} must be linked relatively"
            );
        }
        // Now be the container: the whole config directory appears at another
        // absolute path, with no bundle and no prefix anywhere in sight.
        let container = scratch.0.join("container-home/.claude");
        remount(&scratch.0.join("config"), &container);
        std::fs::remove_dir_all(&bundle.path).expect("no bundle in the container");
        for name in BUNDLED {
            let skill = container.join("skills").join(name).join("SKILL.md");
            assert!(
                skill.is_file(),
                "{} must resolve after the mount — this is the launch that says \
                 `Unknown command: /{name}` when it does not",
                skill.display()
            );
        }
    }

    #[test]
    fn a_stale_link_whose_target_cannot_be_read_is_reported_without_a_path() {
        // A failed `read_link` used to be carried as `Stale(PathBuf::new())`,
        // and the status line printed "stale — links to " with nothing after
        // it. There is no target to name, and the line says that instead.
        assert_eq!(
            state_line(&State::Stale(None), Path::new("/bundle")),
            "stale — the link's target could not be read"
        );
        // The ordinary stale link still names where it points.
        assert_eq!(
            state_line(
                &State::Stale(Some(PathBuf::from("/somewhere/else"))),
                Path::new("/bundle")
            ),
            "stale — links to /somewhere/else"
        );
    }

    #[test]
    fn a_link_into_the_package_bundle_is_stale_and_gets_repointed() {
        // What every `wf` up to this one wrote, and what is on every machine
        // that has run `wf skills install`: an absolute link into the prefix.
        // It resolves here, so nothing but the shape itself says it is wrong.
        let scratch = Scratch::new("stale");
        let bundle = scratch.bundle(&[]);
        let target = scratch.target();
        let old = bundle.path.join("wf");
        std::os::unix::fs::symlink(&old, target.links().join("wf")).expect("old link");

        assert_eq!(
            link_state("wf", &target.links().join("wf")),
            Link::Stale(Some(old.clone()))
        );
        let done = install(&bundle, &target).expect("install");
        let wf_skill = done.iter().find(|(n, _)| n == "wf").expect("entry");
        assert_eq!(wf_skill.1, Outcome::Linked { was: Some(old) });
        assert_eq!(link_state("wf", &target.links().join("wf")), Link::Current);
    }

    #[test]
    fn a_package_update_is_picked_up_by_install_and_by_refresh() {
        // The copy is the thing that can go stale, so it is the thing that gets
        // checked. A bundle rewritten under a running install (which is what
        // `pixi global update wf` is) must not leave a prompt a release behind.
        let scratch = Scratch::new("update");
        let bundle = scratch.bundle(&[]);
        let target = scratch.target();
        install(&bundle, &target).expect("install");

        write_skill(&bundle.path, "wf-tdd", "---\n---\nthe new prompt\n");
        assert_eq!(
            status(&bundle, &target)
                .iter()
                .find(|s| s.name == "wf-tdd")
                .map(|s| s.state.clone()),
            Some(State::Outdated {
                copied_from: Some(bundle.path.clone())
            }),
            "a copy that is not this build's is reported, not assumed fresh"
        );

        // `refresh` is what the launch path calls: it moves the copy and
        // nothing else.
        assert_eq!(
            refresh(&target).expect("refresh"),
            vec![("wf-tdd".to_string(), Healed::Copied)]
        );
        assert_eq!(
            std::fs::read_to_string(target.mirror().join("wf-tdd/SKILL.md")).expect("the copy"),
            "---\n---\nthe new prompt\n"
        );
        assert!(status(&bundle, &target)
            .iter()
            .all(|s| s.state == State::Current));
        assert!(
            refresh(&target).expect("refresh").is_empty(),
            "a copy already in step is not rewritten"
        );

        // And `install` says so rather than reporting a no-op.
        write_skill(&bundle.path, "wf-tdd", "---\n---\nnewer still\n");
        let done = install(&bundle, &target).expect("install");
        assert_eq!(
            done.iter().find(|(n, _)| n == "wf-tdd").expect("entry").1,
            Outcome::Refreshed
        );
    }

    #[test]
    fn a_launch_links_a_skill_the_update_newly_ships() {
        // The #170 report, in the order it happened: install, then a
        // `pixi global update wf` that rewrites the bundle with a skill this
        // machine has no link for, then a launch. Keeping only the *contents*
        // in step freezes the set of skills at whatever the last install saw,
        // so the new one stays unlinked through every launch that follows —
        // and the symptom lands in a container, as `Unknown command: /wf-one`,
        // nowhere near the update that caused it.
        let scratch = Scratch::new("newly-shipped");
        let bundle = scratch.bundle(&["wf-one"]);
        let target = scratch.target();
        install(&bundle, &target).expect("install");
        assert!(!target.links().join("wf-one").exists(), "not shipped yet");

        write_skill(&bundle.path, "wf-one", "---\n---\nthe new skill\n");
        refresh(&target).expect("refresh");
        assert_eq!(
            std::fs::read_link(target.links().join("wf-one")).expect("a link"),
            PathBuf::from(format!("../{MIRROR}/wf-one")),
            "a healed link is the same relative shape install writes"
        );
        // Healing once is healing: the link just written reads as this build's
        // own to the launch after it, so nothing is relinked and no launch
        // reports work it did not do. Asserted *here*, while the recorded
        // source is still on disk — past the removal below `refresh` returns
        // early and would say this of any state at all, healed or wrecked.
        assert!(
            refresh(&target).expect("refresh").is_empty(),
            "the launch after that has nothing left to heal"
        );

        // Which is the whole reason to heal it here rather than anywhere else:
        // it has to resolve as the container reads it, with no bundle and no
        // prefix in sight.
        let container = scratch.0.join("container-home/.claude");
        remount(&scratch.0.join("config"), &container);
        std::fs::remove_dir_all(&bundle.path).expect("no bundle in the container");
        assert_eq!(
            std::fs::read_to_string(container.join("skills/wf-one/SKILL.md")).expect("the prompt"),
            "---\n---\nthe new skill\n"
        );
    }

    #[test]
    fn a_launch_tells_a_link_it_created_apart_from_a_copy_it_rewrote() {
        // Silence is what let #170 survive a release: a launch that heals a
        // link has changed *which prompts this machine has*, which is worth a
        // word on the way past, while re-copying contents is the errand it runs
        // every single time and would only be noise. The two are told apart
        // here, where a test can hold them to it, rather than at the print.
        let scratch = Scratch::new("reported");
        let bundle = scratch.bundle(&["wf-one"]);
        let target = scratch.target();
        install(&bundle, &target).expect("install");

        write_skill(&bundle.path, "wf-one", "---\n---\nthe new skill\n");
        write_skill(&bundle.path, "wf-tdd", "---\n---\nthe new prompt\n");
        assert_eq!(
            refresh(&target).expect("refresh"),
            vec![
                ("wf-one".to_string(), Healed::Linked { was: None }),
                ("wf-tdd".to_string(), Healed::Copied),
            ]
        );
    }

    #[test]
    fn a_launch_repoints_a_stale_link_wf_wrote_and_leaves_one_it_did_not() {
        // The other half of #170, and what #104's rename left behind on every
        // machine that had the old names: a link into the package prefix, the
        // shape every `wf` before #110 wrote. It resolves on the host, so
        // nothing short of the shape itself says it is wrong — and it dangles
        // inside the container, which is where it is read. Repointing it costs
        // nothing but a link, so it needs no permission. A link `wf` cannot
        // prove it wrote is somebody's setup, and gets none.
        let scratch = Scratch::new("repoint");
        let bundle = scratch.bundle(&[]);
        let target = scratch.target();
        install(&bundle, &target).expect("install");

        let prefix = scratch.0.join("old-prefix/share/wf/skills/wf-tdd");
        std::fs::remove_file(target.links().join("wf-tdd")).expect("drop this build's link");
        std::os::unix::fs::symlink(&prefix, target.links().join("wf-tdd")).expect("an older link");
        let theirs = scratch.0.join("somewhere-else");
        std::fs::create_dir_all(&theirs).expect("another tool's tree");
        std::fs::remove_file(target.links().join("wf-review")).expect("drop this build's link");
        std::os::unix::fs::symlink(&theirs, target.links().join("wf-review")).expect("their link");

        assert_eq!(
            refresh(&target).expect("refresh"),
            vec![(
                "wf-tdd".to_string(),
                Healed::Linked {
                    was: Some(prefix.clone())
                }
            )],
            "one is wf's to reclaim and one is not"
        );
        assert_eq!(
            link_state("wf-tdd", &target.links().join("wf-tdd")),
            Link::Current
        );
        assert_eq!(
            std::fs::read_link(target.links().join("wf-review")).expect("still a link"),
            theirs,
            "a link wf cannot prove it wrote is left pointing where it points"
        );
    }

    #[test]
    fn refresh_touches_nothing_it_was_not_asked_to_own() {
        // A machine that never installed, and one where another tool owns the
        // directory: neither gets files written behind its back by a launch.
        let scratch = Scratch::new("untouched");
        let bundle = scratch.bundle(&[]);
        let target = scratch.target();
        assert!(refresh(&target).expect("refresh").is_empty());
        assert!(!target.mirror().exists(), "no links, so nothing to serve");

        std::fs::create_dir_all(target.links().join("wf-tdd")).expect("another tool's directory");
        install(&bundle, &target).expect("install");
        write_skill(&bundle.path, "wf-tdd", "---\n---\nnot yours to place\n");
        assert!(refresh(&target).expect("refresh").is_empty());
        assert!(!target.mirror().join("wf-tdd").exists());
    }

    #[test]
    fn a_launch_refreshes_the_prompts_that_were_installed_not_its_own() {
        // The `$WF_SKILLS_DIR` workflow, which the copy would otherwise break:
        // you install a checkout's prompts to edit them, and then launch with
        // the *released* `wf`, whose bundle is the package's. Overwriting the
        // copy with its own prompts would undo your install silently on the
        // next enter — so the copy tracks the bundle it was installed from, and
        // an edit to that checkout is live in the next session exactly as it
        // was when the link pointed straight at it.
        let scratch = Scratch::new("checkout");
        let checkout = scratch.bundle(&[]);
        let target = scratch.target();
        install(&checkout, &target).expect("install from the checkout");
        assert_eq!(installed_from(&target).as_deref(), Some(&*checkout.path));

        let released = Bundle {
            path: scratch.0.join("prefix/share/wf/skills"),
            found_by: FoundBy::Installed,
        };
        for name in BUNDLED {
            write_skill(&released.path, name, "---\n---\nthe released prompt\n");
        }
        write_skill(
            &checkout.path,
            "wf-tdd",
            "---\n---\nthe prompt being edited\n",
        );

        // The released `wf` launching: it refreshes from the checkout, because
        // that is what was installed.
        assert_eq!(
            refresh(&target).expect("refresh"),
            vec![("wf-tdd".to_string(), Healed::Copied)]
        );
        assert_eq!(
            std::fs::read_to_string(target.mirror().join("wf-tdd/SKILL.md")).expect("the copy"),
            "---\n---\nthe prompt being edited\n"
        );
        // And says whose prompts those are, rather than a bare "outdated".
        assert!(
            report(&released, &target).contains(&format!(
                "outdated — the copy came from {}",
                checkout.path.display()
            )),
            "{}",
            report(&released, &target)
        );
    }

    #[test]
    fn a_real_directory_blocks_rather_than_being_deleted() {
        // chezmoi's copy, or a hand-edit. `wf` did not create it, so `wf` does
        // not remove it — it says so and leaves it, and the *other* skills
        // still install rather than the whole run failing.
        let scratch = Scratch::new("unmanaged");
        let bundle = scratch.bundle(&[]);
        let target = scratch.target();
        let real = target.links().join("wf-tdd");
        std::fs::create_dir_all(&real).expect("real dir");
        std::fs::write(real.join("SKILL.md"), "someone else's").expect("write");

        let done = install(&bundle, &target).expect("install");
        let tdd = done.iter().find(|(n, _)| n == "wf-tdd").expect("entry");
        assert_eq!(tdd.1, Outcome::Blocked);
        assert!(real.join("SKILL.md").is_file(), "the file must survive");
        assert!(
            !target.mirror().join("wf-tdd").exists(),
            "a blocked skill's copy is never read, so it is never written"
        );
        assert!(
            done.iter()
                .filter(|(n, _)| n != "wf-tdd")
                .all(|(_, o)| matches!(o, Outcome::Linked { .. })),
            "one blocked skill must not stop the others: {done:?}"
        );

        // A launch heals links too now (#170), and it is the path nobody typed
        // anything to start — so the restraint matters there more, not less.
        assert!(refresh(&target).expect("refresh").is_empty());
        assert_eq!(
            std::fs::read_to_string(real.join("SKILL.md")).expect("the file must survive"),
            "someone else's"
        );
        assert!(
            !target.mirror().join("wf-tdd").exists(),
            "and a launch writes no copy on a blocked skill's behalf either"
        );
    }

    #[test]
    fn a_skill_missing_from_the_bundle_is_reported_not_dangling() {
        // A packaging fault. Linking anyway would produce a broken link that
        // reads as installed, which is worse than the absence it hides.
        let scratch = Scratch::new("incomplete");
        let bundle = scratch.bundle(&["wf-review"]);
        let target = scratch.target();

        let done = install(&bundle, &target).expect("install");
        let review = done.iter().find(|(n, _)| n == "wf-review").expect("entry");
        assert_eq!(review.1, Outcome::NotInBundle);
        assert!(!target.links().join("wf-review").exists());
        assert_eq!(
            status(&bundle, &target)
                .iter()
                .find(|s| s.name == "wf-review")
                .map(|s| s.state.clone()),
            Some(State::Missing)
        );
    }

    #[test]
    fn sweep_clears_links_a_rename_orphaned_and_nothing_else() {
        let scratch = Scratch::new("sweep");
        let bundle = scratch.bundle(&[]);
        let target = scratch.target();
        install(&bundle, &target).expect("install");

        // What #104's rename left behind: a link named for a skill no longer in
        // BUNDLED, pointing into a prefix whose bundle is already gone. Nothing
        // that iterates BUNDLED can see it, and it dangles. Both the old shape
        // and the one this build writes have to be recognised, or the next
        // rename leaves residue of its own.
        let dead = scratch.0.join("old-prefix/share/wf/skills/wayfinder");
        std::os::unix::fs::symlink(&dead, target.links().join("wayfinder")).expect("orphan");
        std::os::unix::fs::symlink(Target::link_target("wf-old"), target.links().join("wf-old"))
            .expect("orphan copy link");

        // Two things sweep must not touch, and the reason it can tell: neither
        // target is one `wf` writes.
        let mine = target.links().join("grill-me");
        std::fs::create_dir_all(&mine).expect("a skill of the user's own");
        let elsewhere = scratch.0.join("somewhere-else");
        std::fs::create_dir_all(&elsewhere).expect("another tool's tree");
        std::os::unix::fs::symlink(&elsewhere, target.links().join("other-tool"))
            .expect("other link");

        let mut swept = sweep(&bundle, &target).expect("sweep");
        swept.sort();

        assert_eq!(
            swept,
            vec![
                target.links().join("wayfinder"),
                target.links().join("wf-old")
            ]
        );
        assert!(mine.is_dir(), "a real directory is never swept");
        assert!(
            target.links().join("other-tool").symlink_metadata().is_ok(),
            "a link pointing where wf never links is never swept"
        );
        // The skills this build does ship are untouched: they are in BUNDLED,
        // so sweep skips them before it ever looks at where they point.
        assert!(status(&bundle, &target)
            .iter()
            .all(|s| s.state == State::Current));
    }

    #[test]
    fn the_copy_drops_what_this_build_no_longer_ships() {
        // The mirror is wf's own directory, so a rename's residue in there is
        // wf's to clear — and it must be, or a copy outlives every build that
        // shipped it.
        let scratch = Scratch::new("prune");
        let bundle = scratch.bundle(&[]);
        let target = scratch.target();
        install(&bundle, &target).expect("install");
        let orphan = target.mirror().join("wayfinder");
        std::fs::create_dir_all(&orphan).expect("an older build's copy");

        install(&bundle, &target).expect("reinstall");
        assert!(!orphan.exists());
        assert!(target.mirror().join("wf-tdd").is_dir(), "and only that");
    }

    #[test]
    fn the_report_names_the_bundle_the_links_the_copy_and_every_skill() {
        let scratch = Scratch::new("report");
        let bundle = scratch.bundle(&[]);
        let target = scratch.target();
        let before = report(&bundle, &target);
        assert!(before.contains("missing"), "{before}");
        assert!(before.contains("wf skills install"), "{before}");

        install(&bundle, &target).expect("install");
        let after = report(&bundle, &target);
        for name in BUNDLED {
            assert!(after.contains(name), "{name} missing from {after}");
        }
        // Where the prompts actually live is half the answer to "which prompt
        // runs", so the report says it rather than leaving it to be guessed.
        assert!(
            after.contains(&target.mirror().display().to_string()),
            "{after}"
        );
        assert!(!after.contains("missing"), "{after}");
        assert!(after.contains("this build's own"), "{after}");

        write_skill(&bundle.path, "wf", "---\n---\nchanged\n");
        assert!(report(&bundle, &target).contains("outdated"));
    }

    #[test]
    fn a_links_directory_with_no_parent_has_nowhere_to_keep_the_copy() {
        // Total rather than papered over: the copy is a sibling or the links do
        // not resolve, so a path that has no sibling is refused at the
        // constructor instead of producing links that dangle.
        assert!(Target::beside(Path::new("skills")).is_err());
        assert!(Target::beside(Path::new("/")).is_err());
        assert_eq!(
            Target::beside(Path::new("/home/you/.claude/skills"))
                .expect("a target")
                .mirror(),
            Path::new("/home/you/.claude/wf-skills")
        );
    }

    #[test]
    fn the_bundle_env_var_wins_and_an_empty_one_is_not_a_path() {
        // An empty env var is a common accident (`WF_SKILLS_DIR=` in a
        // profile) and taking it literally would resolve the bundle to the
        // current directory. It must read as unset, not as "".
        let candidates = Bundle::candidates();
        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0].0, FoundBy::Env);
        assert_eq!(candidates[1].0, FoundBy::Installed);
        assert_eq!(candidates[2].0, FoundBy::Checkout);
    }

    #[test]
    fn every_route_wf_can_exec_is_in_the_bundle() {
        // The invariant the whole module exists for: `route` cannot name a
        // skill the package does not ship. Swept over `Route::all`, not a list
        // written out here — a hand-written list is a second place to remember,
        // and a route added without being added to it is exactly the case this
        // is meant to catch. The invocation sigil belongs to the chosen agent;
        // the skill name is the shared bundle identity.
        use crate::launch::Route;
        for route in Route::all() {
            // `None` is the one route that invokes no skill (#112): there is no
            // prompt for the package to be missing.
            let Some(skill) = route.bundled_skill() else {
                continue;
            };
            assert!(
                BUNDLED.contains(&skill),
                "{} is routable but not bundled",
                route.label()
            );
        }
        // …and the sweep is only worth anything if it covers everything: a
        // route missing from the cycle would be silently skipped above.
        assert_eq!(
            Route::all().len(),
            7,
            "every route must be in the cycle `Route::all` walks"
        );
    }
}
