//! The launch: a picked ticket becomes the agent, running right here.
//!
//! `wf` is a selector (#26/#34). There is no multiplexer, no tab, no session
//! and no supervision: the picked ticket resolves to a checkout, `wf` gives the
//! terminal back, and its own process image is replaced by
//! `claude --dangerously-skip-permissions "/wayfinder <map> <n>"` in that
//! checkout ([`Launch::exec`]). Unattended work is not a feature here — it is
//! another terminal session you start and switch away from.
//!
//! The one thing that can go wrong is ordering: the terminal must be restored
//! *before* the image is replaced, because after that there is no `wf` left to
//! do it. So this module never restores anything and never `exec`s itself off
//! its own initiative — it hands [`Launch`] to the binary, which restores and
//! then calls [`Launch::exec`] as its last act.

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::model::Ticket;
use crate::projects::Checkout;

/// A fully-resolved launch: which checkout the agent runs in, and which ticket
/// of which map it is handed.
///
/// The fields are private and [`plan`] is the only constructor, so a launch
/// whose checkout belongs to a different repo than its ticket is
/// unrepresentable rather than merely undocumented. It is also, now, exactly
/// *(checkout, ticket, map)* and nothing else: with the tab gone there is no
/// session, no label and no mode for it to carry, and so no way for it to name
/// a place it cannot run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// The ticket's repo, full slug (`owner/name`) — the identity half, kept
    /// whole because a fork and its upstream share a short name (#15).
    repo: String,
    /// The ticket being worked.
    ticket: u64,
    /// The repo's map issue — `/wayfinder`'s first argument.
    map_issue: u64,
    /// The checkout the agent works in: the process's working directory.
    cwd: PathBuf,
}

/// Agent sessions are started from a picker rather than from a shell someone is
/// watching, so they do not stop for permission prompts.
const SKIP_PERMISSIONS: &str = "--dangerously-skip-permissions";

impl Launch {
    /// The checkout the agent runs in.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// How this ticket reads on screen: `<short_repo>#<number>`, the same
    /// identity the picker's rows show.
    pub fn key(&self) -> String {
        format!("{}#{}", short_repo(&self.repo), self.ticket)
    }

    /// One-line description for the notice: what is being launched, and where.
    pub fn describe(&self) -> String {
        format!("{} in {}", self.key(), self.cwd.display())
    }

    /// What `wf` becomes. `claude` takes a single positional prompt, so the
    /// slash command and its arguments are one argv entry, not three.
    pub fn agent_argv(&self) -> Vec<String> {
        vec![
            "claude".to_string(),
            SKIP_PERMISSIONS.to_string(),
            format!("/wayfinder {} {}", self.map_issue, self.ticket),
        ]
    }

    /// Become the agent: replace `wf`'s process image with `claude`, in the
    /// checkout.
    ///
    /// Returns **only** on failure — on success there is no `wf` left to return
    /// to, which is why the return type is a bare error rather than a `Result`
    /// whose `Ok` nobody could ever observe. `exec` rather than spawn-and-wait
    /// is the shape #26 chose: `#5`'s "never `exec`" existed so `wf` could
    /// survive a detach, and with nothing left to survive for a lingering
    /// parent buys nothing while costing the agent its direct hold on the
    /// terminal, the exit code and the signals.
    ///
    /// **The caller must have restored the terminal first.** There is no second
    /// chance after the image is replaced, so that ordering lives in `main`,
    /// where it is one statement above the call, rather than in here.
    pub fn exec(&self) -> anyhow::Error {
        let argv = self.agent_argv();
        let (program, args) = argv.split_first().expect("agent argv is never empty");

        // Resolved against `$PATH` *before* the chdir, deliberately. `exec`
        // chdirs into `cwd` and only then runs `execvp`, so a `$PATH` holding
        // an empty entry — a leading, trailing or doubled `:`, which is an
        // everyday `.bashrc` accident — resolves the agent out of **the
        // checkout**. Cloning a repo and running `wf` in it would be enough to
        // run its `./claude` with `--dangerously-skip-permissions`. Empty
        // entries are dropped rather than read as `.`, which is the one place
        // this deliberately differs from `execvp`.
        let program = match resolve_on_path(program) {
            Ok(program) => program,
            Err(err) => return err,
        };
        // Checked because the two failures are both `ENOENT` and the fix for
        // each is completely different. The cache is pruned once at startup and
        // the picker holds a snapshot, so a `git worktree remove` in another
        // terminal during the session lands here.
        if !self.cwd.is_dir() {
            return anyhow::anyhow!(
                "the checkout {} is gone — nothing to run the agent in",
                self.cwd.display()
            );
        }

        // `CommandExt::exec` only ever returns on failure.
        let err = Command::new(&program)
            .args(args)
            .current_dir(&self.cwd)
            .exec();
        // Quoted, so the prompt reads as the single argument it is — the whole
        // invariant `agent_argv` exists to hold.
        let quoted: Vec<String> = std::iter::once(program.display().to_string())
            .chain(args.iter().cloned())
            .map(|a| format!("{a:?}"))
            .collect();
        anyhow::Error::new(err).context(format!(
            "running {} in {}",
            quoted.join(" "),
            self.cwd.display()
        ))
    }
}

/// Find `program` on `$PATH`, skipping empty entries.
///
/// A name containing a separator is a path already and is taken as given —
/// that is the caller naming a file, not `$PATH` resolution.
fn resolve_on_path(program: &str) -> Result<PathBuf, anyhow::Error> {
    if program.contains('/') {
        return Ok(PathBuf::from(program));
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            anyhow::anyhow!("`{program}` is not on PATH — is the agent CLI installed?")
        })
}

/// The name half of a repo slug (`blooop/wayfinder` → `wayfinder`). Display
/// only — never an identity key, because a fork and its upstream share it.
fn short_repo(slug: &str) -> &str {
    slug.split('/').next_back().unwrap_or(slug)
}

/// The checkouts that could host a ticket's agent: every registered checkout of
/// the ticket's repo, matched on the **full** slug (#15). Cache order (sorted
/// by path) is preserved so the picker is stable.
pub fn candidate_checkouts<'a>(checkouts: &'a [Checkout], repo: &str) -> Vec<&'a Checkout> {
    checkouts.iter().filter(|c| c.repo == repo).collect()
}

/// What a launch request resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Targets {
    /// No registered checkout of this repo on this machine — nothing to launch
    /// into. (Only reachable if the cache changed under us: map tickets exist
    /// because a cached checkout named their repo.)
    Unregistered,
    /// Exactly one candidate: launch straight away, no prompt.
    One(Launch),
    /// Several checkouts of one repo (the k1–k5 pattern): the human picks which
    /// one the agent runs in. The only reason the picker still exists — the
    /// agent must run in exactly one tree, and `wf` cannot guess which.
    Many(Vec<Launch>),
}

/// Resolve a launch request against the projects cache. Zero or one candidate
/// never prompts.
pub fn plan(checkouts: &[Checkout], ticket: &Ticket, map_issue: u64) -> Targets {
    let launches: Vec<Launch> = candidate_checkouts(checkouts, &ticket.repo)
        .into_iter()
        .map(|c| Launch {
            repo: ticket.repo.clone(),
            ticket: ticket.number,
            map_issue,
            cwd: c.path.clone(),
        })
        .collect();
    match launches.len() {
        0 => Targets::Unregistered,
        1 => Targets::One(launches.into_iter().next().expect("len checked")),
        _ => Targets::Many(launches),
    }
}

/// Map issue numbers by repo slug — `/wayfinder`'s first argument.
pub type MapIssues = BTreeMap<String, u64>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{classify, Status, TicketType};

    fn ticket(repo: &str, number: u64) -> Ticket {
        Ticket {
            repo: repo.to_string(),
            number,
            title: "the ticket".to_string(),
            status: classify(true, false, vec![]),
            ticket_type: TicketType::Task,
        }
    }

    fn checkout(path: &str, repo: &str) -> Checkout {
        Checkout {
            path: PathBuf::from(path),
            repo: repo.to_string(),
        }
    }

    fn cache() -> Vec<Checkout> {
        vec![
            checkout("/data/k1/kinisi_ros", "kinisi/kinisi_ros"),
            checkout("/data/k2/kinisi_ros", "kinisi/kinisi_ros"),
            checkout("/data/proj/wayfinder", "blooop/wayfinder"),
            checkout("/data/proj/dotfiles", "upstream/dotfiles"),
        ]
    }

    #[test]
    fn candidate_checkouts_filter_on_the_full_slug() {
        let cache = cache();
        let kinisi = candidate_checkouts(&cache, "kinisi/kinisi_ros");
        assert_eq!(
            kinisi.iter().map(|c| c.path.as_path()).collect::<Vec<_>>(),
            vec![
                Path::new("/data/k1/kinisi_ros"),
                Path::new("/data/k2/kinisi_ros")
            ]
        );
        // A fork and its upstream share a short name: matching must not mix
        // them, so "blooop/dotfiles" has no candidate here.
        assert!(candidate_checkouts(&cache, "blooop/dotfiles").is_empty());
        assert_eq!(candidate_checkouts(&cache, "upstream/dotfiles").len(), 1);
    }

    #[test]
    fn one_candidate_never_prompts_and_none_is_unregistered() {
        let cache = cache();
        match plan(&cache, &ticket("blooop/wayfinder", 16), 1) {
            Targets::One(launch) => {
                assert_eq!(launch.cwd(), Path::new("/data/proj/wayfinder"));
                assert_eq!(launch.key(), "wayfinder#16");
                assert_eq!(launch.map_issue, 1);
                assert_eq!(launch.ticket, 16);
            }
            other => panic!("expected One, got {other:?}"),
        }
        assert_eq!(
            plan(&cache, &ticket("blooop/dotfiles", 3), 2),
            Targets::Unregistered
        );
        assert_eq!(
            plan(&[], &ticket("blooop/wayfinder", 16), 1),
            Targets::Unregistered
        );
    }

    #[test]
    fn several_checkouts_of_one_repo_offer_a_choice_of_trees() {
        let launches = match plan(&cache(), &ticket("kinisi/kinisi_ros", 42), 7) {
            Targets::Many(launches) => launches,
            other => panic!("expected Many, got {other:?}"),
        };
        assert_eq!(launches.len(), 2);
        assert_eq!(
            launches.iter().map(|l| l.cwd()).collect::<Vec<_>>(),
            vec![
                Path::new("/data/k1/kinisi_ros"),
                Path::new("/data/k2/kinisi_ros")
            ]
        );
        // Same ticket either way: only the tree it runs in differs.
        assert!(launches.iter().all(|l| l.key() == "kinisi_ros#42"));
    }

    #[test]
    fn the_agent_runs_interactive_claude_with_one_prompt_argument() {
        let launch = match plan(&cache(), &ticket("blooop/wayfinder", 16), 1) {
            Targets::One(l) => l,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            launch.agent_argv(),
            vec![
                "claude".to_string(),
                SKIP_PERMISSIONS.to_string(),
                "/wayfinder 1 16".to_string()
            ]
        );
    }

    #[test]
    fn the_notice_names_the_ticket_and_the_tree_it_runs_in() {
        // With several checkouts, *which tree* is the only thing that varies —
        // so it is what the notice has to say.
        let launches = match plan(&cache(), &ticket("kinisi/kinisi_ros", 42), 7) {
            Targets::Many(l) => l,
            other => panic!("{other:?}"),
        };
        assert_eq!(launches[0].describe(), "kinisi_ros#42 in /data/k1/kinisi_ros");
        assert_eq!(launches[1].describe(), "kinisi_ros#42 in /data/k2/kinisi_ros");
    }

    #[test]
    fn the_key_is_the_short_repo_but_identity_stays_the_full_slug() {
        let launch = match plan(&cache(), &ticket("upstream/dotfiles", 5), 4) {
            Targets::One(l) => l,
            other => panic!("{other:?}"),
        };
        assert_eq!(launch.key(), "dotfiles#5");
        assert_eq!(launch.repo, "upstream/dotfiles");
        assert_eq!(short_repo("blooop/wayfinder"), "wayfinder");
        // Not a slug at all: the whole thing is the name.
        assert_eq!(short_repo("wayfinder"), "wayfinder");
    }

    #[test]
    fn status_is_irrelevant_to_launching() {
        let done = Ticket {
            repo: "blooop/wayfinder".to_string(),
            number: 2,
            title: "done".to_string(),
            status: Status::Done,
            ticket_type: TicketType::Task,
        };
        match plan(&cache(), &done, 1) {
            Targets::One(launch) => assert_eq!(launch.key(), "wayfinder#2"),
            other => panic!("a done ticket still launches, got {other:?}"),
        }
    }
}
