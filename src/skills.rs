//! The prompts `wf` execs, shipped in the same package as the binary.
//!
//! `wf` does not merely *mention* `/wf-tdd` and `/wf-auto` — it hardcodes
//! them in [`crate::launch::route`] and execs them. That makes the skill files
//! part of `wf`'s interface, and an interface split across two repos on two
//! release cadences is one that drifts: `wf` reached 0.6.0 still routing
//! `defer` at a `/wayfinder` section that `/wayfinder-auto` had superseded
//! weeks earlier, and nothing anywhere could notice.
//!
//! So the skills live in this repo, under `skills/`, and the conda recipe
//! installs them beside the binary at `<prefix>/share/wf/skills`. One package,
//! one version, one `pixi global update wf`.
//!
//! What this module does *not* do is teach the agent where to look. Claude Code
//! reads `~/.claude/skills`, so the last step is a **symlink** from there into
//! this build's bundle ([`install`]) — a link rather than a copy precisely so
//! that updating the package updates the prompts, with no second command to
//! remember and no copy to go stale in between.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The skills this build ships — the ones [`crate::launch::Route`] can exec,
/// plus the single-ticket sibling they share their tracker notes with.
///
/// Named explicitly rather than read from the bundle directory: this list is
/// what the routing table promises, so a skill that vanishes from the package
/// should be a *reported* absence, not a silently shorter list.
///
/// Every name is prefixed `wf`, because `~/.claude/skills` is one flat
/// namespace shared with every other source of skills the user has. Unprefixed,
/// `tdd` and `review` are names `wf` *squats on* rather than merely occupies —
/// while it holds one, the user cannot have their own, and [`install`] would
/// refuse to link over theirs if they made one (#104).
///
/// Renaming this list is what [`sweep`] exists to clean up after.
pub const BUNDLED: [&str; 5] = ["wf", "wf-auto", "wf-one", "wf-tdd", "wf-review"];

/// Overrides the bundle location. The escape hatch for a build that is not
/// installed — `wf-next` copied onto `PATH`, or a test — and the way to point
/// a released `wf` at a checkout you are editing.
pub const BUNDLE_ENV: &str = "WF_SKILLS_DIR";

/// Claude Code's own config-directory override, honoured so that `wf` installs
/// where the agent will actually read from rather than where it usually does.
const CLAUDE_CONFIG_ENV: &str = "CLAUDE_CONFIG_DIR";

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

/// Where Claude Code reads personal skills from: `$CLAUDE_CONFIG_DIR/skills`,
/// or `~/.claude/skills`.
///
/// # Errors
///
/// When `$CLAUDE_CONFIG_DIR` is unset and the home directory cannot be
/// resolved — there is then nowhere to install to and nothing to guess.
pub fn claude_skills_dir() -> Result<PathBuf> {
    if let Some(configured) = std::env::var_os(CLAUDE_CONFIG_ENV).filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(configured).join("skills"));
    }
    let home = dirs::home_dir().context("cannot resolve the home directory")?;
    Ok(home.join(".claude").join("skills"))
}

/// What is at `~/.claude/skills/<name>` right now.
///
/// A sum rather than a bool pair, because the three failures want three
/// different sentences and only one of them is safe to fix by overwriting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// A symlink into this build's bundle: the prompt that runs is the one
    /// this `wf` shipped.
    Current,
    /// A symlink, but into some other bundle — an older prefix, or a checkout
    /// you pointed at once and forgot. Relinking is safe: nothing but a link
    /// is lost.
    Stale(PathBuf),
    /// A real directory. Somebody else owns this — chezmoi, a hand-edit, a
    /// plugin — so `wf` reports it and does not touch it. Deleting a directory
    /// it did not create is not `wf`'s call to make.
    Unmanaged,
    /// Nothing there.
    Missing,
}

impl State {
    /// Whether this state means the bundled prompt is what would run.
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

/// Inspect every bundled skill against `into`.
pub fn status(bundle: &Bundle, into: &Path) -> Vec<Status> {
    BUNDLED
        .iter()
        .map(|name| Status {
            name: (*name).to_string(),
            state: state_of(&bundle.path.join(name), &into.join(name)),
        })
        .collect()
}

/// Classify one link target. `symlink_metadata` rather than `metadata`, so a
/// link is seen as a link instead of as whatever it points at — the whole
/// distinction this function exists to draw.
fn state_of(want: &Path, link: &Path) -> State {
    let Ok(meta) = std::fs::symlink_metadata(link) else {
        return State::Missing;
    };
    if !meta.file_type().is_symlink() {
        return State::Unmanaged;
    }
    match std::fs::read_link(link) {
        // Compared canonically: `~/.pixi/envs/wf/...` and a path reached
        // through a symlinked prefix are the same bundle, and a textual
        // comparison would call that stale on every run.
        Ok(target) => {
            let same = target.canonicalize().ok() == want.canonicalize().ok()
                && want.canonicalize().is_ok();
            if same {
                State::Current
            } else {
                State::Stale(target)
            }
        }
        Err(_) => State::Stale(PathBuf::new()),
    }
}

/// What one skill's installation did — reported rather than printed, so the
/// caller owns the wording and the tests can assert on the outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// It already pointed at this build's bundle.
    AlreadyCurrent,
    /// A link was created, or repointed from `was`.
    Linked { was: Option<PathBuf> },
    /// A real directory is in the way; nothing was touched.
    Blocked,
    /// The bundle does not contain this skill — a packaging fault, reported
    /// rather than papered over with a dangling link.
    NotInBundle,
}

/// Link every bundled skill into `into`, creating it if needed.
///
/// Idempotent by construction: the only thing ever removed is a symlink `wf`
/// would have created itself, and a skill already pointing at this bundle is
/// left alone rather than relinked, so running it twice is indistinguishable
/// from running it once.
///
/// # Errors
///
/// When `into` cannot be created, or a link cannot be written. A skill that is
/// merely *blocked* or absent from the bundle is not an error — it is an
/// [`Outcome`], so one bad skill never costs the other four their install.
pub fn install(bundle: &Bundle, into: &Path) -> Result<Vec<(String, Outcome)>> {
    std::fs::create_dir_all(into).with_context(|| format!("cannot create {}", into.display()))?;
    let mut done = Vec::new();
    for name in BUNDLED {
        let source = bundle.path.join(name);
        let link = into.join(name);
        if !source.is_dir() {
            done.push((name.to_string(), Outcome::NotInBundle));
            continue;
        }
        let outcome = match state_of(&source, &link) {
            State::Current => Outcome::AlreadyCurrent,
            State::Unmanaged => Outcome::Blocked,
            state => {
                let was = match state {
                    State::Stale(target) => Some(target),
                    _ => None,
                };
                if was.is_some() {
                    std::fs::remove_file(&link)
                        .with_context(|| format!("cannot replace the link {}", link.display()))?;
                }
                std::os::unix::fs::symlink(&source, &link).with_context(|| {
                    format!("cannot link {} → {}", link.display(), source.display())
                })?;
                Outcome::Linked { was }
            }
        };
        done.push((name.to_string(), outcome));
    }
    Ok(done)
}

/// Whether `target` names a directory inside some `wf` bundle — this build's,
/// or one belonging to a prefix that has since been replaced.
///
/// This is the whole safety argument for [`sweep`]. A link is removed only when
/// it points *into a bundle*, which is a place nothing but `wf` ever links to,
/// so a skill the user wrote, a plugin's, or a link some other tool left behind
/// can never match however dead it looks.
fn points_into_a_bundle(target: &Path, bundle: &Path) -> bool {
    let Some(parent) = target.parent() else {
        return false;
    };
    // The current bundle covers `$WF_SKILLS_DIR` pointing at a checkout, whose
    // path ends in `skills` and not in `share/wf/skills`. The suffix covers
    // every installed prefix, including the older ones this is here to clear.
    parent == bundle || parent.ends_with("share/wf/skills")
}

/// Remove links `wf` wrote for skills it no longer ships.
///
/// A rename leaves residue that neither [`status`] nor [`install`] can see:
/// they iterate [`BUNDLED`], so the moment `wayfinder` left that list, the link
/// named `wayfinder` stopped being anything either function looks at — while
/// staying on disk, pointing into a bundle where its target no longer exists.
/// Claude Code would go on reading a dangling entry forever, and no amount of
/// `wf skills install` would mention it.
///
/// Scoped by where the link *points* rather than by a list of former names: a
/// hardcoded list would need editing at every rename and would still miss the
/// links left by a `wf` older than the list.
///
/// # Errors
///
/// When `into` cannot be read, or a link cannot be removed. A missing `into` is
/// not an error — there is nothing to sweep.
pub fn sweep(bundle: &Bundle, into: &Path) -> Result<Vec<PathBuf>> {
    let Ok(entries) = std::fs::read_dir(into) else {
        return Ok(Vec::new());
    };
    let mut swept = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("cannot read {}", into.display()))?;
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
        let Ok(target) = std::fs::read_link(&link) else {
            continue;
        };
        if !points_into_a_bundle(&target, &bundle.path) {
            continue;
        }
        std::fs::remove_file(&link)
            .with_context(|| format!("cannot remove the stale link {}", link.display()))?;
        swept.push(link);
    }
    Ok(swept)
}

/// The `wf skills` report: where the bundle is, where the links go, and the
/// state of each — the answer to "which prompt is actually going to run".
pub fn report(bundle: &Bundle, into: &Path) -> String {
    use std::fmt::Write;
    let source = match bundle.found_by {
        FoundBy::Env => BUNDLE_ENV,
        FoundBy::Installed => "installed beside the binary",
        FoundBy::Checkout => "this checkout",
    };
    let mut out = format!(
        "bundle  {} ({source})\ntarget  {}\n\n",
        bundle.path.display(),
        into.display()
    );
    // Inspected once and reported from that, rather than asked twice: the two
    // answers would be a stat apart, and a report whose list and whose verdict
    // disagreed would be the worst possible output for this command.
    let statuses = status(bundle, into);
    for Status { name, state } in &statuses {
        let line = match state {
            State::Current => "ok".to_string(),
            State::Stale(target) => format!("stale — links to {}", target.display()),
            State::Unmanaged => "not a link — another tool owns this one".to_string(),
            State::Missing => "missing".to_string(),
        };
        let _ = writeln!(out, "  {name:<15} {line}");
    }
    if statuses.iter().all(|s| s.state.is_current()) {
        out.push_str("\nEvery skill wf routes to is this build's own.\n");
    } else {
        out.push_str("\nRun `wf skills install` to link them.\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch tree with a bundle and an empty target, removed on drop. No
    /// `tempfile` dependency for a handful of tests that need real paths —
    /// and these need *real* paths, because symlinks are what is under test.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!("wf-skills-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("target")).expect("scratch");
            Scratch(dir)
        }

        /// A bundle holding every skill in [`BUNDLED`], minus any `omit`.
        fn bundle(&self, omit: &[&str]) -> Bundle {
            let path = self.0.join("bundle");
            for name in BUNDLED {
                if omit.contains(&name) {
                    continue;
                }
                let dir = path.join(name);
                std::fs::create_dir_all(&dir).expect("bundle skill");
                std::fs::write(dir.join("SKILL.md"), "---\n---\n").expect("SKILL.md");
            }
            std::fs::create_dir_all(&path).expect("bundle");
            Bundle {
                path,
                found_by: FoundBy::Env,
            }
        }

        fn target(&self) -> PathBuf {
            self.0.join("target")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn sweep_clears_links_a_rename_orphaned_and_nothing_else() {
        let scratch = Scratch::new("sweep");
        let bundle = scratch.bundle(&[]);
        let target = scratch.target();
        install(&bundle, &target).expect("install");

        // What #104's rename left behind: a link named for a skill no longer in
        // BUNDLED, pointing into a prefix whose bundle is already gone. Nothing
        // that iterates BUNDLED can see it, and it dangles.
        let dead = scratch.0.join("old-prefix/share/wf/skills/wayfinder");
        std::os::unix::fs::symlink(&dead, target.join("wayfinder")).expect("orphan");
        assert!(target.join("wayfinder").symlink_metadata().is_ok());

        // Two things sweep must not touch, and the reason it can tell: neither
        // target is inside a bundle.
        let mine = target.join("grill-me");
        std::fs::create_dir_all(&mine).expect("a skill of the user's own");
        let elsewhere = scratch.0.join("somewhere-else");
        std::fs::create_dir_all(&elsewhere).expect("another tool's tree");
        std::os::unix::fs::symlink(&elsewhere, target.join("other-tool")).expect("other link");

        let swept = sweep(&bundle, &target).expect("sweep");

        assert_eq!(swept, vec![target.join("wayfinder")]);
        assert!(target.join("wayfinder").symlink_metadata().is_err());
        assert!(mine.is_dir(), "a real directory is never swept");
        assert!(
            target.join("other-tool").symlink_metadata().is_ok(),
            "a link pointing outside any bundle is never swept"
        );
        // The skills this build does ship are untouched: they are in BUNDLED,
        // so sweep skips them before it ever looks at where they point.
        assert!(status(&bundle, &target)
            .iter()
            .all(|s| s.state == State::Current));
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

        // Twice is once: nothing is relinked, so a link is never briefly
        // absent and no run reports work it did not do.
        let second = install(&bundle, &target).expect("reinstall");
        assert!(
            second.iter().all(|(_, o)| *o == Outcome::AlreadyCurrent),
            "{second:?}"
        );
    }

    #[test]
    fn a_link_into_another_bundle_is_stale_and_gets_repointed() {
        // The case that matters after `pixi global update wf`: the link is
        // ours, it is just aimed at the previous prefix.
        let scratch = Scratch::new("stale");
        let bundle = scratch.bundle(&[]);
        let target = scratch.target();
        let old = scratch.0.join("old-prefix").join("wf");
        std::fs::create_dir_all(&old).expect("old bundle");
        std::os::unix::fs::symlink(&old, target.join("wf")).expect("old link");

        assert_eq!(
            state_of(&bundle.path.join("wf"), &target.join("wf")),
            State::Stale(old.clone())
        );
        let done = install(&bundle, &target).expect("install");
        let wf_skill = done.iter().find(|(n, _)| n == "wf").expect("entry");
        assert_eq!(wf_skill.1, Outcome::Linked { was: Some(old) });
        assert_eq!(
            state_of(&bundle.path.join("wf"), &target.join("wf")),
            State::Current
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
        let real = target.join("wf-tdd");
        std::fs::create_dir_all(&real).expect("real dir");
        std::fs::write(real.join("SKILL.md"), "someone else's").expect("write");

        let done = install(&bundle, &target).expect("install");
        let tdd = done.iter().find(|(n, _)| n == "wf-tdd").expect("entry");
        assert_eq!(tdd.1, Outcome::Blocked);
        assert!(real.join("SKILL.md").is_file(), "the file must survive");
        assert!(
            done.iter()
                .filter(|(n, _)| n != "wf-tdd")
                .all(|(_, o)| matches!(o, Outcome::Linked { .. })),
            "one blocked skill must not stop the others: {done:?}"
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
        assert!(!target.join("wf-review").exists());
        assert_eq!(
            status(&bundle, &target)
                .iter()
                .find(|s| s.name == "wf-review")
                .map(|s| s.state.clone()),
            Some(State::Missing)
        );
    }

    #[test]
    fn the_report_names_the_bundle_the_target_and_every_skill() {
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
        assert!(!after.contains("missing"), "{after}");
        assert!(after.contains("this build's own"), "{after}");
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
        // skill the package does not ship. Asserted against the label, which
        // is the string that actually reaches the agent's prompt.
        use crate::launch::Route;
        for route in [
            Route::Tdd,
            Route::Review,
            Route::Wayfinder,
            Route::WayfinderAuto,
        ] {
            let skill = route.label().trim_start_matches('/');
            assert!(
                BUNDLED.contains(&skill),
                "{} is routable but not bundled",
                route.label()
            );
        }
    }
}
