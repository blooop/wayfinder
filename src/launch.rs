//! The launch: a picked ticket becomes the agent, running right here.
//!
//! `wf` is a selector (#26/#34). There is no multiplexer, no tab, no session
//! and no supervision: the picked ticket resolves to a checkout, `wf` gives the
//! terminal back, and its own process image is replaced by
//! `claude --dangerously-skip-permissions "<skill> …"` in that checkout
//! ([`Launch::exec`]) — which skill is the (type, stage) [`route`], and the
//! launch line's mode rides the prompt as a suffix (#61/#62). Unattended work
//! is still not supervised here — a deferred launch is the same exec with
//! ` defer` in the prompt, watched from another terminal or not at all.
//!
//! The one thing that can go wrong is ordering: the terminal must be restored
//! *before* the image is replaced, because after that there is no `wf` left to
//! do it. So this module never restores anything and never `exec`s itself off
//! its own initiative — it hands [`Launch`] to the binary, which restores and
//! then calls [`Launch::exec`] as its last act.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::model::{Stage, Ticket, TicketType};
use crate::projects::Checkout;

/// Which skill the launched agent runs — the (type, stage) → skill table,
/// hardcoded in `wf` (#61): not per-ticket config, not a Notes-parsed table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// `/tdd <n>` — build work: ready, resuming, or acting on red checks and
    /// requested changes (the reviewer's comments live on the PR).
    Tdd,
    /// `/review <n>` — a build whose PR awaits its independent look.
    Review,
    /// `/wayfinder <map> <n>` — every decision session.
    Wayfinder,
}

impl Route {
    /// How the route reads on the launch line: the slash command it execs.
    pub fn label(self) -> &'static str {
        match self {
            Route::Tdd => "/tdd",
            Route::Review => "/review",
            Route::Wayfinder => "/wayfinder",
        }
    }
}

/// What the launch line's typed text meant — parsed once at the second enter
/// (parse, don't validate: the exec never re-reads a string). The four
/// meanings of #62's staged prompt step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchMode {
    /// An empty line: today's behavior, the default.
    Interactive,
    /// `defer` — the whole subtree, resolved unattended (#63).
    Deferred,
    /// `defer <text>` — deferred, with a steering prompt.
    DeferredSteered(String),
    /// Any other text — a steering prompt on an interactive launch.
    Steered(String),
}

impl LaunchMode {
    /// Parse the launch line. Total: every string means one of the four
    /// things. `defer` is matched as a *word* — `deferred work` is steering
    /// text that happens to start with the same letters, not a mode.
    pub fn parse(text: &str) -> LaunchMode {
        let text = text.trim();
        if text.is_empty() {
            return LaunchMode::Interactive;
        }
        match text.strip_prefix("defer") {
            Some("") => LaunchMode::Deferred,
            Some(rest) if rest.starts_with(char::is_whitespace) => {
                LaunchMode::DeferredSteered(rest.trim_start().to_string())
            }
            _ => LaunchMode::Steered(text.to_string()),
        }
    }

    /// The suffix appended to the exec'd slash command (#62/#63): nothing,
    /// ` defer`, ` defer: <text>`, or ` steer: <text>`.
    fn suffix(&self) -> String {
        match self {
            LaunchMode::Interactive => String::new(),
            LaunchMode::Deferred => " defer".to_string(),
            LaunchMode::DeferredSteered(text) => format!(" defer: {text}"),
            LaunchMode::Steered(text) => format!(" steer: {text}"),
        }
    }
}

/// Resolve which skill a (type, stage) launches. Total, with `None` as the
/// one honest refusal: done is not launchable, whatever the type. Blocked is
/// refused *before* this is consulted — blocked is [`crate::model::Status`],
/// not a stage, so an illegal blocked route is unrepresentable here.
///
/// Every arm is named on both axes: adding a stage or a type without deciding
/// its route is a compile error, not a silent fall-through.
pub fn route(ticket_type: TicketType, stage: Stage) -> Option<Route> {
    match stage {
        Stage::Done => None,
        // Build rows of the #61 table: in-review hands off to the fresh-eyes
        // reviewer; everything else on a build node is code work.
        Stage::InReview => match ticket_type {
            TicketType::Build => Some(Route::Review),
            TicketType::Research
            | TicketType::Task
            | TicketType::Grilling
            | TicketType::Prototype
            | TicketType::Untyped => Some(Route::Wayfinder),
        },
        Stage::Ready | Stage::Building | Stage::NeedsAttention => match ticket_type {
            TicketType::Build => Some(Route::Tdd),
            // Decision types (untyped riding along, as it always launched):
            // /wayfinder at every unfinished stage — PR-dominant derivation
            // can put a decision node past "in progress" (a prototype's PR
            // counts), and the skill owns its node's PR state.
            TicketType::Research
            | TicketType::Task
            | TicketType::Grilling
            | TicketType::Prototype
            | TicketType::Untyped => Some(Route::Wayfinder),
        },
    }
}

/// A launch the first enter staged but the machine has not answered yet (#62):
/// everything the launch line draws and the second enter needs, snapshotted
/// **index-free**.
///
/// Index-free is the whole point. `crate::app::Row` is positional — an index
/// into a `Vec` that the next fetch replaces — and the line stays up while
/// background map arrivals swap the clusters underneath it (#27). A `Row` held
/// here would draw, and then launch, whichever ticket had landed at that
/// index; a shorter map would panic on the next frame. So the staged launch
/// carries the ticket's own facts, the way [`Targets::Many`] carries complete
/// [`Launch`]es rather than a choice to re-resolve later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    /// The ticket's repo, full slug (`owner/name`) — what the checkout cache
    /// is matched on (#15).
    pub repo: String,
    /// The ticket the launch line names.
    pub ticket: u64,
    /// Its title as it read when the line opened — the line is showing the
    /// human what they picked, not re-reporting a row that may have moved.
    pub title: String,
    /// The map issue of the cluster the row was picked in (#50) — which map a
    /// ticket listed twice was launched from.
    pub map_issue: u64,
    /// Resolved from (type, stage) by [`route`] at the first enter, which is
    /// also where an unlaunchable node was refused: no `Route`, no `Staged`.
    pub route: Route,
}

impl Staged {
    /// Stage a launch of `ticket`, picked in the cluster of `map_issue`.
    pub fn new(ticket: &Ticket, map_issue: u64, route: Route) -> Staged {
        Staged {
            repo: ticket.repo.clone(),
            ticket: ticket.number,
            title: ticket.title.clone(),
            map_issue,
            route,
        }
    }
}

/// A fully-resolved launch: which checkout the agent runs in, which ticket of
/// which map it is handed, and — since the two-step (#62) — which skill it
/// runs ([`Route`]) and in what mode ([`LaunchMode`]).
///
/// The fields are private and [`plan`] is the only constructor, so a launch
/// whose checkout belongs to a different repo than its ticket is
/// unrepresentable rather than merely undocumented. The route arrives already
/// resolved from (type, stage) by [`route`], which is where an unlaunchable
/// node was refused — a `Launch` for a done node cannot be built, because no
/// `Route` for it exists.
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
    /// The skill this launch execs, resolved from (type, stage).
    route: Route,
    /// What the launch line said: interactive, deferred, steered.
    mode: LaunchMode,
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
    /// slash command, its arguments and the mode suffix are one argv entry,
    /// not several. Only `/wayfinder` takes the map argument — `/tdd` and
    /// `/review` resolve the repo from the checkout they run in.
    pub fn agent_argv(&self) -> Vec<String> {
        let prompt = match self.route {
            Route::Tdd | Route::Review => format!("{} {}", self.route.label(), self.ticket),
            Route::Wayfinder => format!("/wayfinder {} {}", self.map_issue, self.ticket),
        };
        vec![
            "claude".to_string(),
            SKIP_PERMISSIONS.to_string(),
            format!("{prompt}{}", self.mode.suffix()),
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
    ///
    /// # Panics
    ///
    /// Never in practice: [`agent_argv`](Self::agent_argv) builds the vector
    /// literally and always starts it with the program name, so the split below
    /// cannot come up empty. The `expect` is there to say so.
    pub fn exec(&self) -> anyhow::Error {
        let argv = self.agent_argv();
        let (program, rest) = argv.split_first().expect("agent argv is never empty");

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
            .args(rest)
            .current_dir(&self.cwd)
            .exec();
        // Quoted, so the prompt reads as the single argument it is — the whole
        // invariant `agent_argv` exists to hold.
        let quoted: Vec<String> = std::iter::once(program.display().to_string())
            .chain(rest.iter().cloned())
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
        .ok_or_else(|| anyhow::anyhow!("`{program}` is not on PATH — is the agent CLI installed?"))
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
/// never prompts. The route and mode arrive already settled — this function
/// only answers *where* the agent can run.
///
/// # Panics
///
/// Never: the `expect` in the one-candidate arm is guarded by the `match` on
/// the length immediately above it.
pub fn plan(checkouts: &[Checkout], staged: &Staged, mode: &LaunchMode) -> Targets {
    let launches: Vec<Launch> = candidate_checkouts(checkouts, &staged.repo)
        .into_iter()
        .map(|c| Launch {
            repo: staged.repo.clone(),
            ticket: staged.ticket,
            map_issue: staged.map_issue,
            cwd: c.path.clone(),
            route: staged.route,
            mode: mode.clone(),
        })
        .collect();
    match launches.len() {
        0 => Targets::Unregistered,
        1 => Targets::One(launches.into_iter().next().expect("len checked")),
        _ => Targets::Many(launches),
    }
}

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
            blocked_by: vec![],
            prs: vec![],
        }
    }

    fn checkout(path: &str, repo: &str) -> Checkout {
        Checkout {
            path: PathBuf::from(path),
            repo: repo.to_string(),
        }
    }

    /// An interactive `/wayfinder` plan — the default launch, and the shape
    /// every checkout-resolution test wants (route and mode are orthogonal to
    /// which trees are candidates).
    fn plan_wf(checkouts: &[Checkout], ticket: &Ticket, map_issue: u64) -> Targets {
        plan(
            checkouts,
            &Staged::new(ticket, map_issue, Route::Wayfinder),
            &LaunchMode::Interactive,
        )
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
        match plan_wf(&cache, &ticket("blooop/wayfinder", 16), 1) {
            Targets::One(launch) => {
                assert_eq!(launch.cwd(), Path::new("/data/proj/wayfinder"));
                assert_eq!(launch.key(), "wayfinder#16");
                assert_eq!(launch.map_issue, 1);
                assert_eq!(launch.ticket, 16);
            }
            other => panic!("expected One, got {other:?}"),
        }
        assert_eq!(
            plan_wf(&cache, &ticket("blooop/dotfiles", 3), 2),
            Targets::Unregistered
        );
        assert_eq!(
            plan_wf(&[], &ticket("blooop/wayfinder", 16), 1),
            Targets::Unregistered
        );
    }

    #[test]
    fn several_checkouts_of_one_repo_offer_a_choice_of_trees() {
        let launches = match plan_wf(&cache(), &ticket("kinisi/kinisi_ros", 42), 7) {
            Targets::Many(launches) => launches,
            other => panic!("expected Many, got {other:?}"),
        };
        assert_eq!(launches.len(), 2);
        assert_eq!(
            launches.iter().map(Launch::cwd).collect::<Vec<_>>(),
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
        let launch = match plan_wf(&cache(), &ticket("blooop/wayfinder", 16), 1) {
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
    fn the_launch_line_text_parses_to_its_mode() {
        // The four meanings of the typed line (#62): empty is the interactive
        // default, `defer` alone is the deferred subtree, `defer <text>` adds
        // steering to it, anything else steers an interactive launch.
        assert_eq!(LaunchMode::parse(""), LaunchMode::Interactive);
        assert_eq!(LaunchMode::parse("   "), LaunchMode::Interactive);
        assert_eq!(LaunchMode::parse("defer"), LaunchMode::Deferred);
        assert_eq!(LaunchMode::parse("defer "), LaunchMode::Deferred);
        assert_eq!(
            LaunchMode::parse("defer skip the flaky suite"),
            LaunchMode::DeferredSteered("skip the flaky suite".to_string())
        );
        assert_eq!(
            LaunchMode::parse("try the other approach"),
            LaunchMode::Steered("try the other approach".to_string())
        );
        // `defer` is a word, not a prefix: no fuzzy matching.
        assert_eq!(
            LaunchMode::parse("deferred work first"),
            LaunchMode::Steered("deferred work first".to_string())
        );
    }

    #[test]
    fn the_agent_command_is_the_route_plus_the_mode_suffix() {
        let launch = |route: Route, mode: LaunchMode| -> String {
            let staged = Staged::new(&ticket("blooop/wayfinder", 16), 1, route);
            match plan(&cache(), &staged, &mode) {
                Targets::One(l) => l.agent_argv().last().expect("a prompt").clone(),
                other => panic!("{other:?}"),
            }
        };
        // The route picks the skill; only /wayfinder takes the map argument.
        assert_eq!(launch(Route::Tdd, LaunchMode::Interactive), "/tdd 16");
        assert_eq!(launch(Route::Review, LaunchMode::Interactive), "/review 16");
        assert_eq!(
            launch(Route::Wayfinder, LaunchMode::Interactive),
            "/wayfinder 1 16"
        );
        // The mode rides as a suffix, whatever the route (#62/#63).
        assert_eq!(
            launch(Route::Wayfinder, LaunchMode::Deferred),
            "/wayfinder 1 16 defer"
        );
        assert_eq!(
            launch(
                Route::Wayfinder,
                LaunchMode::DeferredSteered("skip the flaky suite".to_string())
            ),
            "/wayfinder 1 16 defer: skip the flaky suite"
        );
        assert_eq!(
            launch(
                Route::Tdd,
                LaunchMode::Steered("try the other approach".to_string())
            ),
            "/tdd 16 steer: try the other approach"
        );
    }

    #[test]
    fn the_notice_names_the_ticket_and_the_tree_it_runs_in() {
        // With several checkouts, *which tree* is the only thing that varies —
        // so it is what the notice has to say.
        let launches = match plan_wf(&cache(), &ticket("kinisi/kinisi_ros", 42), 7) {
            Targets::Many(l) => l,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            launches[0].describe(),
            "kinisi_ros#42 in /data/k1/kinisi_ros"
        );
        assert_eq!(
            launches[1].describe(),
            "kinisi_ros#42 in /data/k2/kinisi_ros"
        );
    }

    #[test]
    fn the_key_is_the_short_repo_but_identity_stays_the_full_slug() {
        let launch = match plan_wf(&cache(), &ticket("upstream/dotfiles", 5), 4) {
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
    fn build_nodes_route_to_tdd_except_in_review_which_routes_to_review() {
        // The #61 routing table's build rows: failing checks and requested
        // changes are code work, so needs-attention goes back to /tdd.
        assert_eq!(route(TicketType::Build, Stage::Ready), Some(Route::Tdd));
        assert_eq!(route(TicketType::Build, Stage::Building), Some(Route::Tdd));
        assert_eq!(
            route(TicketType::Build, Stage::NeedsAttention),
            Some(Route::Tdd)
        );
        assert_eq!(
            route(TicketType::Build, Stage::InReview),
            Some(Route::Review)
        );
    }

    #[test]
    fn decision_types_route_to_wayfinder_at_every_unfinished_stage() {
        // The table lists decision types at ready/in-progress, but PR-dominant
        // derivation can put one at in-review or needs-attention (a
        // prototype's PR counts) — the skill owns its node's PR state, so the
        // route stays /wayfinder at every stage short of done. Untyped rides
        // along: launching untyped tickets is today's behavior, kept.
        for ticket_type in [
            TicketType::Research,
            TicketType::Task,
            TicketType::Grilling,
            TicketType::Prototype,
            TicketType::Untyped,
        ] {
            for stage in [
                Stage::Ready,
                Stage::Building,
                Stage::InReview,
                Stage::NeedsAttention,
            ] {
                assert_eq!(
                    route(ticket_type, stage),
                    Some(Route::Wayfinder),
                    "{ticket_type:?} at {stage:?}"
                );
            }
        }
    }

    #[test]
    fn done_is_not_launchable_whatever_the_type() {
        for ticket_type in [
            TicketType::Build,
            TicketType::Research,
            TicketType::Task,
            TicketType::Grilling,
            TicketType::Prototype,
            TicketType::Untyped,
        ] {
            assert_eq!(route(ticket_type, Stage::Done), None, "{ticket_type:?}");
        }
    }

    #[test]
    fn status_is_irrelevant_to_launching() {
        let done = Ticket {
            repo: "blooop/wayfinder".to_string(),
            number: 2,
            title: "done".to_string(),
            status: Status::Done,
            ticket_type: TicketType::Task,
            blocked_by: vec![],
            prs: vec![],
        };
        match plan_wf(&cache(), &done, 1) {
            Targets::One(launch) => assert_eq!(launch.key(), "wayfinder#2"),
            other => panic!("a done ticket still launches, got {other:?}"),
        }
    }
}
