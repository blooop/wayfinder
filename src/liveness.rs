//! Liveness: what is actually running, and what stopped without finishing.
//!
//! Every glyph on the picker until now was a **tracker** fact — the issue's
//! state, its assignees, its linked PRs. That is the right vocabulary for what
//! the work *is*, and it is silent about the half `wf` itself created: a launch
//! `exec`s an agent into a container and exits, so nothing has ever reported
//! back whether that agent is still there. `⏎` comes closest and deliberately
//! stops short — it records that a launch *happened*, which is the strongest
//! thing the launch record can honestly claim.
//!
//! `dl` knows the missing half and already publishes it. The reaper reads
//! `state` to protect a running container from deletion and then throws it
//! away; this module keeps it, and joins it to the tracker fact the same
//! reading already fetched.
//!
//! Two observations come out of that join, and the second is the one that
//! needed both halves:
//!
//! - **Running** — a container is up for this node.
//! - **Stalled** — the tracker says [`NodeFact::Claimed`] (open, assigned,
//!   nothing pushed) while no container of that node's is up. Somebody, almost
//!   always an agent, took this ticket and is no longer working on it.
//!
//! Neither half can say that alone, which is why this lives here rather than in
//! the fetch. A claim on its own is the ordinary look of work in progress. A
//! stopped container on its own is the ordinary look of work that finished. It
//! is the pair — claimed, nothing to show for it, and nothing running — that
//! means a lifecycle went down between its stages, and that is exactly the
//! state [`reap`](crate::reap) is right to refuse to collect and wrong to leave
//! unmentioned: a stale claim keeps a workspace, so the run that died is the
//! one thing on the machine nothing was pointing at.
//!
//! ## What this deliberately does not claim
//!
//! **A container being up is not an agent being alive.** `dl` reports the
//! container; nobody reports the process inside it, and `wf` is long gone by
//! then. `Running` means a container is up — a session you left, a session that
//! exited an hour ago inside a container nobody stopped, a `dl <ws> up` that
//! prewarmed and was never entered. It is a floor on activity, not a reading of
//! it.
//!
//! **A stall is not a crash.** The same shape is left by an agent that died
//! mid-slice, by one that handed off cleanly, by a `dl <ws> stop` you ran
//! yourself, and by a reboot — which stops every container on the machine at
//! once, so the morning after one, every claimed node with a workspace is
//! marked. Those are not false positives: each of those runs really has
//! stopped, and each really does want picking up. But they arrive together, so
//! the count line can read `12 stalled` for a reason that is about the host and
//! not about the work. The ticket's breadcrumb trail is what says which; the
//! claim here is only that nothing is moving.
//!
//! **A node launched on the host can still be marked**, and this is the one
//! outright false positive. A checkout with no devcontainer runs the agent on
//! the host, where there is no container to report — but if that node has a
//! workspace from *some other* launch (the repo grew a `.devcontainer/` later,
//! or `WF_PREWARM` built one at a staging you backed out of), the stopped
//! workspace and the live claim look exactly like a stall. `wf` cannot see host
//! processes, so it cannot tell. A node with no workspace at all is genuinely
//! invisible here rather than wrongly marked.
//!
//! **This machine only.** The listing is local, the same limit the resume
//! record carries. A ticket worked on another machine looks unstarted here.

use std::collections::BTreeMap;

use crate::reap::{node_of, Node, NodeFact, Workspace};

/// How many stalled nodes the count line names before it starts counting.
///
/// Two, where the reclaim hint allows three, because a node's name is the
/// short repo and its number — `wayfinder#133` — and two of those plus the
/// lead-in is already most of what a shared line can spare. The rows carry the
/// same marking, so the line is a summons rather than the full list.
const NAMED: usize = 2;

/// What this machine says about a node, beyond what the tracker knows.
///
/// Two arms and no `Idle`: a node with no workspace and a node whose workspace
/// is merely stopped are both "nothing to report", and giving that a name would
/// invite a marker on almost every row — `⏎` already says a launch happened
/// here, and a second badge repeating it in different words is noise on the one
/// screen whose whole job is picking out the rows that want you.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Life {
    /// A container of this node's is up.
    Running,
    /// Claimed, nothing pushed, and nothing of its running.
    Stalled,
}

/// The machine's half of the picture, per node.
///
/// **One map, not a set per arm.** A node has at most one liveness, and two
/// parallel sets can represent it having two — which then forces a precedence
/// rule at every read, to resolve a state that is not supposed to exist. The
/// map makes the question unaskable instead: the arms are values, so the
/// precedence lives once, in [`Liveness::read`], where the facts that decide it
/// are actually in hand.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Liveness {
    life: BTreeMap<Node, Life>,
}

impl Liveness {
    /// Join `dl`'s listing to the tracker's answers.
    ///
    /// Takes the same two values [`reap::plan`](crate::reap::plan) does, from
    /// the same reading, and reads different fields of them. Nothing here can
    /// delete: it borrows a listing and a set of tracker answers, and returns a
    /// map of numbers.
    ///
    /// Running is settled first and wins, because a node can own more than one
    /// workspace: with one container up and another down, the node is being
    /// worked on, and the down one is not evidence of anything.
    pub fn read(workspaces: &[Workspace], known: &BTreeMap<Node, NodeFact>) -> Self {
        // Not one of `wf`'s — a branch it never minted is no fact about a node.
        let ours = || workspaces.iter().filter_map(|w| Some((node_of(w)?, w)));
        let mut life: BTreeMap<Node, Life> = ours()
            .filter(|(_, w)| w.is_running())
            .map(|(node, _)| (node, Life::Running))
            .collect();
        for (node, workspace) in ours() {
            // `is_down`, not `!is_running`: a container mid-start, or a state
            // this `wf` cannot read, is not something to turn into a claim on
            // a row. A container devpod cannot find at all *is* down.
            if !workspace.is_down() || life.contains_key(&node) {
                continue;
            }
            if matches!(known.get(&node), Some(NodeFact::Claimed)) {
                life.insert(node, Life::Stalled);
            }
        }
        Self { life }
    }

    /// What to mark this node with, if anything.
    ///
    /// The per-row lookup, keyed the way the screen has a node to hand:
    /// `(repo, number)`. Same shape as [`App::resume`](crate::app::App::resume),
    /// because it is answering the same kind of question about the same rows.
    pub fn of(&self, repo: &str, number: u64) -> Option<Life> {
        self.life
            .get(&Node {
                repo: repo.to_string(),
                number,
            })
            .copied()
    }

    /// Is there anything here worth carrying to the screen at all?
    ///
    /// The reading is only sent when something has something to say, so this is
    /// what keeps an empty join from waking the draw path.
    pub fn is_empty(&self) -> bool {
        self.life.is_empty()
    }

    /// The nodes at one arm, in a stable order — `BTreeMap`'s, so the count
    /// line never reshuffles between frames.
    fn at(&self, arm: Life) -> impl Iterator<Item = &Node> {
        self.life
            .iter()
            .filter(move |(_, life)| **life == arm)
            .map(|(node, _)| node)
    }

    /// How many nodes are stalled — the count the line leads with.
    pub fn stalled(&self) -> usize {
        self.at(Life::Stalled).count()
    }

    /// How many nodes have a container up. Never drawn on the count line (see
    /// [`Liveness::hint`]); this is what the probe reads to prove the live path
    /// produces liveness at all.
    pub fn running(&self) -> usize {
        self.at(Life::Running).count()
    }

    /// The count-line segment: how many are stalled, and which.
    ///
    /// Named rather than counted for the reason the reclaim hint gives for
    /// naming its workspaces: `2 stalled` is a number nobody can agree or
    /// disagree with, and a name is what makes it checkable against what the
    /// reader already believes about their own work.
    ///
    /// **Only stalls reach this line.** Running containers are on their rows
    /// and nowhere else — they are the ordinary state of a machine in use, and
    /// a count of them is a status bar, not a summons. A stall is the anomaly,
    /// and the anomaly is what a shared line is for.
    ///
    /// No pointer clause, unlike the reclaim hint's ` — wf reap`: that one
    /// names a command a reader would otherwise have to remember, and the move
    /// here is `enter` on the row, which is the picker's whole verb. Telling
    /// somebody to press enter in a list is spending the scarcest characters on
    /// the line saying nothing.
    ///
    /// `width` is columns of terminal, not characters — [`cols`](crate::cols)
    /// is what the ladder below measures with. A node's name is
    /// `short_repo#number` and the repo half comes from GitHub, so a name two
    /// columns to the char is ordinary input, and counting its chars would
    /// hand the reclaim note columns this segment had already spent.
    ///
    /// The width is a budget, and this segment is laid down before the reclaim
    /// note — so on a narrowing line the reap pointer and the warned aside go
    /// while stalls are still naming nodes. Deliberate, and the one trade in
    /// here worth disagreeing with: work that has stopped moving outranks
    /// tidying that can wait.
    ///
    /// The caller holds back [`Reclaimable::min_width`](crate::reclaim::Reclaimable::min_width)
    /// before asking, which is where that priority stops. Below its own count
    /// the reclaim note clips rather than disappearing, so taking those last
    /// characters would not buy the line a stall name — it would spend them
    /// turning the neighbouring segment into a fragment.
    pub fn hint(&self, width: usize) -> String {
        if self.stalled() == 0 {
            return String::new();
        }
        let head = format!("· {} stalled", self.stalled());
        let names: Vec<String> = self.at(Life::Stalled).map(Node::name).collect();
        for count in (1..=NAMED.min(names.len())).rev() {
            let rest = names.len() - count;
            let more = if rest == 0 {
                String::new()
            } else {
                format!(", +{rest} more")
            };
            let line = format!("{head}: {}{more}", names[..count].join(", "));
            if crate::cols(&line) <= width {
                return line;
            }
        }
        if crate::cols(&head) <= width {
            return head;
        }
        String::new()
    }

    /// A liveness with these nodes on it, for the tests of the screen and the
    /// channel that carry one.
    #[cfg(test)]
    pub(crate) fn for_test(running: &[(&str, u64)], stalled: &[(&str, u64)]) -> Self {
        fn arm(nodes: &[(&str, u64)], life: Life) -> Vec<(Node, Life)> {
            nodes
                .iter()
                .map(|(repo, number)| {
                    (
                        Node {
                            repo: (*repo).to_string(),
                            number: *number,
                        },
                        life,
                    )
                })
                .collect()
        }
        Self {
            life: arm(running, Life::Running)
                .into_iter()
                .chain(arm(stalled, Life::Stalled))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(id: &str, repo: &str, number: u64, state: &str) -> Workspace {
        let short = repo.split('/').next_back().unwrap();
        Workspace {
            id: id.to_string(),
            devlaunch: true,
            repo: Some(repo.to_string()),
            branch: Some(format!("wayfinder/{short}-{number}")),
            state: Some(state.to_string()),
            unsaved: None,
        }
    }

    fn node(repo: &str, number: u64) -> Node {
        Node {
            repo: repo.to_string(),
            number,
        }
    }

    fn facts<const N: usize>(entries: [(Node, NodeFact); N]) -> BTreeMap<Node, NodeFact> {
        entries.into_iter().collect()
    }

    const REPO: &str = "blooop/wayfinder";

    #[test]
    fn a_claimed_node_whose_container_is_down_is_the_stall_neither_half_could_name() {
        let workspaces = [workspace("a", REPO, 133, "Stopped")];
        let known = facts([(node(REPO, 133), NodeFact::Claimed)]);
        let live = Liveness::read(&workspaces, &known);
        assert_eq!(live.of(REPO, 133), Some(Life::Stalled));
        assert_eq!(live.stalled(), 1);
    }

    #[test]
    fn the_same_claim_with_its_container_up_is_work_in_progress_and_not_a_stall() {
        let workspaces = [workspace("a", REPO, 133, "Running")];
        let known = facts([(node(REPO, 133), NodeFact::Claimed)]);
        let live = Liveness::read(&workspaces, &known);
        assert_eq!(live.of(REPO, 133), Some(Life::Running));
        assert_eq!(live.stalled(), 0, "a container that is up is not a stall");
    }

    /// The discriminator has to be the pair, so each half alone must stay
    /// quiet: this is the test that fails if either condition is dropped.
    #[test]
    fn neither_half_alone_makes_a_stall() {
        // Claimed, but nothing of `wf`'s on this machine at all — a host
        // launch, or another machine's work.
        let known = facts([(node(REPO, 133), NodeFact::Claimed)]);
        assert_eq!(Liveness::read(&[], &known).stalled(), 0);
        assert_eq!(Liveness::read(&[], &known).of(REPO, 133), None);

        // A stopped container under every other fact the tracker can report.
        // Only `Claimed` is a stall; `InFlight` in particular is not, because
        // the PR is the thing the run left behind and the screen already shows
        // it.
        let workspaces = [workspace("a", REPO, 133, "Stopped")];
        for fact in [
            NodeFact::Closed,
            NodeFact::DoneByMerge { pr: 1 },
            NodeFact::Superseded { pr: 1 },
            NodeFact::InFlight { pr: 1 },
            NodeFact::Unstarted,
        ] {
            let known = facts([(node(REPO, 133), fact.clone())]);
            assert_eq!(
                Liveness::read(&workspaces, &known).stalled(),
                0,
                "{fact:?} is not a stall"
            );
        }
    }

    /// The states that settle, and the states that say nothing.
    ///
    /// `NotFound` is the one worth pinning: a container devpod cannot find has
    /// certainly stopped, and reading only `Stopped` left the most definitely
    /// dead workspace on the machine unmarked.
    #[test]
    fn a_container_that_is_gone_is_as_down_as_one_that_is_stopped() {
        let known = facts([(node(REPO, 133), NodeFact::Claimed)]);
        for state in ["Stopped", "NotFound"] {
            let workspaces = [workspace("a", REPO, 133, state)];
            assert_eq!(
                Liveness::read(&workspaces, &known).of(REPO, 133),
                Some(Life::Stalled),
                "{state} is a stall"
            );
        }
        // Mid-transition, and a state dl could not read. Neither is a claim.
        for state in ["Busy", "SomethingDevpodAddedLater"] {
            let workspaces = [workspace("a", REPO, 133, state)];
            assert_eq!(
                Liveness::read(&workspaces, &known).of(REPO, 133),
                None,
                "{state} asserts nothing"
            );
        }
        let mut unanswered = workspace("a", REPO, 133, "Stopped");
        unanswered.state = None;
        assert_eq!(
            Liveness::read(&[unanswered], &known).of(REPO, 133),
            None,
            "a state dl would not answer for asserts nothing"
        );
    }

    #[test]
    fn a_node_with_two_workspaces_is_running_if_either_of_them_is() {
        let workspaces = [
            workspace("a", REPO, 133, "Stopped"),
            workspace("b", REPO, 133, "Running"),
        ];
        let known = facts([(node(REPO, 133), NodeFact::Claimed)]);
        let live = Liveness::read(&workspaces, &known);
        assert_eq!(live.of(REPO, 133), Some(Life::Running));
        assert_eq!(live.stalled(), 0);
    }

    #[test]
    fn a_workspace_wf_did_not_mint_is_no_fact_about_any_node() {
        let mut foreign = workspace("x", REPO, 133, "Running");
        foreign.branch = Some("hotfix/urgent".to_string());
        let known = facts([(node(REPO, 133), NodeFact::Claimed)]);
        let live = Liveness::read(&[foreign], &known);
        assert!(live.is_empty(), "a branch wf never named says nothing");
    }

    #[test]
    fn a_node_the_tracker_did_not_answer_for_is_not_read_as_claimed() {
        let workspaces = [workspace("a", REPO, 133, "Stopped")];
        let live = Liveness::read(&workspaces, &BTreeMap::new());
        assert_eq!(live.stalled(), 0);
        assert!(live.is_empty());
    }

    /// This module is on the picker's reading path, so it carries the path's
    /// denylist too.
    ///
    /// [`reclaim`](crate::reclaim), [`refresh`](crate::refresh),
    /// [`picker`](crate::picker) and `main` each hold one, and the point of
    /// having them in every file is stated in `reclaim`'s own module doc: a
    /// grep over N files cannot see the same call written in the N+1th. This
    /// module *is* that N+1th file — new, importing `reap`, and reached from
    /// the survey the picker spawns — so it arrived owing the guard rather than
    /// being exempt from it.
    ///
    /// `tokio::spawn` is the one worth spelling out here, and it is on this
    /// list for `picker`'s reason rather than `reclaim`'s: a task spawned in a
    /// derivation like this one outlives the run that recorded it, and the
    /// runtime probes watch a bounded window. `--force` and `"rm"` are the argv
    /// of a subprocess this file has no means to start, and cost nothing to
    /// forbid anyway. Substring match, as everywhere else: `fs` here also
    /// forbids `refs` and `offset`, which this file has no occasion to write.
    #[test]
    fn deriving_liveness_can_delete_nothing() {
        let code = crate::probe::code_only("liveness.rs", include_str!("liveness.rs"));
        for forbidden in [
            "remove",
            "\"rm\"",
            "--force",
            "Command",
            "process::",
            "tokio::spawn",
            "fs",
        ] {
            assert!(
                !code.contains(forbidden),
                "reading liveness is a join over what it was handed: it names {forbidden:?}"
            );
        }
    }

    #[test]
    fn the_hint_names_what_it_can_and_counts_the_rest() {
        let live = Liveness::for_test(&[], &[(REPO, 90), (REPO, 133), (REPO, 134)]);
        assert_eq!(
            live.hint(80),
            "· 3 stalled: wayfinder#90, wayfinder#133, +1 more"
        );
    }

    /// The ladder, width by width. A segment that overran its budget would push
    /// the reclaim note — or the match count — off the end of the line.
    #[test]
    fn the_hint_never_overruns_its_budget_at_any_width() {
        let live = Liveness::for_test(&[], &[(REPO, 90), (REPO, 133), (REPO, 134)]);
        for width in 0..60 {
            let hint = live.hint(width);
            assert!(
                hint.chars().count() <= width,
                "width {width} produced {} characters: {hint:?}",
                hint.chars().count()
            );
        }
        assert_eq!(live.hint(0), "", "no room is no segment, not a clipped one");
        assert_eq!(
            live.hint(12),
            "· 3 stalled",
            "the count survives after the names are gone"
        );
    }

    /// The same ladder against a wide name. The number the caller hands down is
    /// columns of terminal — it is what the count line has left — so a repo
    /// name whose chars are two columns wide spends twice what it was given,
    /// and the reclaim note beside it is what loses the difference: two of
    /// these plus a reclaimable set rendered `· 2 stalled: ..., +1 more · 2
    /// recla`, the fragment the count line's own test exists to forbid.
    #[test]
    fn the_budget_is_columns_of_terminal_even_when_a_stalled_name_is_wide() {
        let live = Liveness::for_test(
            &[],
            &[("blooop/测试仓库仓库", 9), ("blooop/测试仓库仓库", 14)],
        );
        for width in 0..60 {
            let hint = live.hint(width);
            assert!(
                crate::cols(&hint) <= width,
                "width {width} produced {} columns: {hint:?}",
                crate::cols(&hint)
            );
        }
    }

    #[test]
    fn nothing_stalled_is_no_segment_however_much_room_there_is() {
        let live = Liveness::for_test(&[(REPO, 133)], &[]);
        assert_eq!(live.hint(200), "");
        assert!(
            !live.is_empty(),
            "but a running container is still worth carrying"
        );
    }
}
