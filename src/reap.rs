//! Reaping: remove the workspaces whose tickets are finished.
//!
//! A workspace per ticket (#106) means workspaces accumulate as fast as tickets
//! are worked, and something has to clear the finished ones away. That
//! something is `wf`, and the division of labour is the same one the launch
//! already draws: **`dl` owns the containers, `wf` owns the tickets.**
//!
//! `dl` deliberately does not decide what is finished, and could not — it knows
//! about clones and containers, and "this work is over" is a fact about a
//! ticket. It infers nothing from the branch either; a squash-merged branch and
//! an abandoned one look identical from there. So it publishes what it knows
//! (`dl --ls --json`) and refuses to destroy work that exists nowhere else
//! (`dl <ws> rm`), and `wf` — which named those branches after its own tickets,
//! and knows which tickets are closed — decides.
//!
//! Which makes this module's job small and its shape strict: match workspaces
//! to nodes by the branch `wf` itself minted, ask the tracker which of those
//! nodes are closed, and hand the finished ones back to `dl`. Everything a
//! workspace holds that says "not yet" — unsaved work, a running container — is
//! `dl`'s fact, read here rather than argued with.

use std::collections::BTreeSet;
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use tokio::process::Command;

use crate::model::PrStatus;

/// The branch prefix `wf` mints its workspaces under (#106): the full branch is
/// `wayfinder/<short-repo>-<n>`, which is also the branch `/wf-tdd` works on.
const BRANCH_PREFIX: &str = "wayfinder/";

/// One workspace as `dl --ls --json` reports it.
///
/// Only the fields `wf` acts on are declared; serde ignores the rest, so a
/// newer `dl` adding fields does not break this and an older one missing the
/// optional ones still parses.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: String,
    /// Whether `dl` created it. Anything else is not `wf`'s to touch, whatever
    /// its branch looks like.
    #[serde(default)]
    pub devlaunch: bool,
    /// `owner/name`, from the record `dl` wrote when it made the clone.
    #[serde(default)]
    pub repo: Option<String>,
    /// The branch the workspace was made for.
    #[serde(default)]
    pub branch: Option<String>,
    /// devpod's state: `Running`, `Stopped`, …
    #[serde(default)]
    pub state: Option<String>,
    /// What deleting would destroy, in `dl`'s words, or `None`.
    #[serde(default)]
    pub unsaved: Option<String>,
}

/// What `wf` decided about one workspace. Three arms, each carrying a reason,
/// because the plan is printed before anything is deleted and a reason the
/// reader disagrees with is only useful while saying no is still possible.
///
/// `Warn` is display-only: a row `wf` wants read but will not act on. It is a
/// separate arm rather than a flag on `Keep` so that every site deciding what
/// to print — and, more to the point, what to delete — has to say at compile
/// time which of the three it means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Reap { id: String, reason: String },
    Warn { id: String, reason: String },
    Keep { id: String, reason: String },
}

impl Verdict {
    pub fn id(&self) -> &str {
        match self {
            Verdict::Reap { id, .. } | Verdict::Warn { id, .. } | Verdict::Keep { id, .. } => id,
        }
    }

    pub fn reason(&self) -> &str {
        match self {
            Verdict::Reap { reason, .. }
            | Verdict::Warn { reason, .. }
            | Verdict::Keep { reason, .. } => reason,
        }
    }
}

/// The workspaces a plan would actually delete.
///
/// The one definition of the doomed set, kept here rather than as a `matches!`
/// at the call site: "a warned workspace is never deleted" is the safety
/// property of the whole `Warn` arm, and a property nothing owns is a property
/// nothing can pin. Every caller partitions through this.
pub fn doomed(verdicts: &[Verdict]) -> Vec<&Verdict> {
    verdicts
        .iter()
        .filter(|v| matches!(v, Verdict::Reap { .. }))
        .collect()
}

/// What one linked PR says about whether its node is still alive — reap's
/// projection of the badge reading, not a second reading of the tracker.
///
/// Three arms, because reap asks less of a PR than the screen does: does this
/// PR keep the node alive, did it finish it, or does it say nothing. Checks,
/// review and draftness are all the same answer here.
///
/// Every arm carries its number, since that number is the whole content of the
/// row the human is about to approve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrOutcome {
    /// Open, draft — or unreadable. See [`PrOutcome::project`].
    InFlight { pr: u64 },
    Merged { pr: u64 },
    ClosedUnmerged { pr: u64 },
}

impl PrOutcome {
    /// Project one PR's badge reading, which is [`fetch`](crate::fetch)'s job
    /// and is not redone here — `status` is exactly what the badge parse made
    /// of it, and `None` is that parse declining a `state` this binary does not
    /// recognise.
    ///
    /// **An unreadable PR is in flight.** It is the only claim that stays true
    /// whatever a newer GitHub state turns out to mean, and it is the arm that
    /// keeps the workspace and prints nothing — so a state added after this
    /// binary shipped costs a workspace that stays, never one that goes. The
    /// alternative is worse than wrong: dropping it would leave the node
    /// looking like it has no PR at all, which reads as
    /// [`NodeFact::Unstarted`] — a warning invented out of a parse failure.
    pub fn project(number: u64, status: Option<&PrStatus>) -> PrOutcome {
        match status {
            Some(PrStatus::Merged) => PrOutcome::Merged { pr: number },
            Some(PrStatus::Closed) => PrOutcome::ClosedUnmerged { pr: number },
            Some(PrStatus::Draft | PrStatus::Open { .. }) | None => {
                PrOutcome::InFlight { pr: number }
            }
        }
    }
}

/// What the tracker says about one node, in the terms reap decides by.
///
/// Six unconfusable values where a boolean "is it closed" used to be, so that
/// "closed", "done by merge", "superseded", "in flight", "claimed" and
/// "nothing has come of it" cannot be mistaken for each other, and so that
/// every match site has to say out loud what it does with each.
///
/// Derived at every read from the batch the tracker just answered — never
/// stored, so it cannot go stale, be orphaned, or outlive the workspace it
/// describes. `Unstarted` in particular is a *positive observation* (open, no
/// PRs, nobody assigned) and is never reachable from a lookup that failed: a
/// node the batch did not answer for is an error, not an `Unstarted` node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeFact {
    /// The ticket is closed.
    Closed,
    /// Open, but a PR merged and nothing is still in flight — what wf's own
    /// stage lattice already calls Done, whatever the ticket still says.
    DoneByMerge { pr: u64 },
    /// Open, every linked PR closed unmerged. A human's "not this way", which
    /// is not the same as "this branch is disposable".
    Superseded { pr: u64 },
    /// Open, with an open or draft PR: the least dead thing `wf` manages.
    InFlight { pr: u64 },
    /// Open, no PRs, but someone has taken it up. An explicit claim is a
    /// person's statement of intent and reap does not overrule it.
    Claimed,
    /// Open, no PRs, unclaimed: nothing has come of this node.
    Unstarted,
}

/// Read one node the way reap decides by it — the stage lattice's table,
/// applied to the same fields the map query already returns.
///
/// Total over its inputs, and the order is the argument: a live PR dominates a
/// merged sibling (a multi-PR ticket between merges is still being worked), a
/// merge outranks the closed PRs beside it, and only a node with no PR
/// evidence at all falls through to the claim.
pub fn node_fact(is_open: bool, is_assigned: bool, prs: &[PrOutcome]) -> NodeFact {
    if !is_open {
        return NodeFact::Closed;
    }
    let earliest = |pick: fn(&PrOutcome) -> Option<u64>| prs.iter().filter_map(pick).min();
    if let Some(pr) = earliest(|o| match o {
        PrOutcome::InFlight { pr } => Some(*pr),
        _ => None,
    }) {
        return NodeFact::InFlight { pr };
    }
    if let Some(pr) = earliest(|o| match o {
        PrOutcome::Merged { pr } => Some(*pr),
        _ => None,
    }) {
        return NodeFact::DoneByMerge { pr };
    }
    // Everything left is closed-unmerged, so the highest number is the last
    // word anyone had on this node.
    if let Some(pr) = prs
        .iter()
        .filter_map(|o| match o {
            PrOutcome::ClosedUnmerged { pr } => Some(*pr),
            _ => None,
        })
        .max()
    {
        return NodeFact::Superseded { pr };
    }
    if is_assigned {
        NodeFact::Claimed
    } else {
        NodeFact::Unstarted
    }
}

/// The node a workspace belongs to: the repo it is in and the ticket number its
/// branch names. Only workspaces `wf` minted have one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Node {
    pub repo: String,
    pub number: u64,
}

/// Which node a workspace is for, or `None` if it is not one of `wf`'s.
///
/// Strict on purpose, and every clause is a way for this to be someone else's
/// workspace: `dl` must have made it (so the branch means what `wf` thinks),
/// the branch must carry the prefix, and the tail must be
/// `<short-repo>-<number>` for *this* workspace's repo — `wayfinder/hotfix-3`
/// in `blooop/devlaunch` is a hand-made branch that happens to sit under the
/// prefix, not ticket 3. A parse that guessed would delete a stranger's
/// container.
pub fn node_of(workspace: &Workspace) -> Option<Node> {
    if !workspace.devlaunch {
        return None;
    }
    let repo = workspace.repo.as_ref()?;
    let branch = workspace.branch.as_ref()?;
    let short = repo.split('/').next_back()?;
    let number = branch
        .strip_prefix(BRANCH_PREFIX)?
        .strip_prefix(short)?
        .strip_prefix('-')?
        .parse()
        .ok()?;
    Some(Node {
        repo: repo.clone(),
        number,
    })
}

/// Decide every workspace's fate, in listing order.
///
/// `finished` is the set of nodes the tracker says are closed — gathered once,
/// [`finished_nodes`], because that is one network call for the lot rather than
/// one per workspace.
///
/// The guards run before the reason to delete, and their order is the safety
/// argument: work that would be lost outranks a closed ticket, and a live
/// container outranks it too. Neither is `wf`'s judgement — the first is `dl`'s
/// fact and the second is devpod's — which is exactly why they are read here
/// instead of being left for `dl` to refuse on: a caller that argues with a
/// refusal it could have anticipated is a caller that will one day pass
/// `--force` to shut it up.
///
/// `insist` is that override, made explicit and given to the human instead. It
/// exists because of a case that is not hypothetical: a devcontainer whose
/// `postCreateCommand` installs packages leaves a tracked lockfile modified in
/// **every** workspace it builds, so without it those workspaces are unreapable
/// forever. It waives the unsaved-work guard **only** — a running container is
/// still kept, because that guard is about a session in progress rather than
/// about bytes on disk. The plan names what is being overridden either way, so
/// the waiver is read before it is granted, and `dl` gets `--force` only for the
/// workspaces the human just saw described.
pub fn plan(workspaces: &[Workspace], finished: &BTreeSet<Node>, insist: bool) -> Vec<Verdict> {
    let mut verdicts = Vec::new();
    for workspace in workspaces {
        let Some(node) = node_of(workspace) else {
            // Not one of ours. Not reported either: a listing full of "not
            // mine" lines buries the two rows that matter.
            continue;
        };
        let id = workspace.id.clone();
        if let (Some(unsaved), false) = (&workspace.unsaved, insist) {
            verdicts.push(Verdict::Keep {
                id,
                reason: format!("holds {unsaved}"),
            });
        } else if workspace.state.as_deref() == Some("Running") {
            verdicts.push(Verdict::Keep {
                id,
                reason: "still running — stop it first".to_string(),
            });
        } else if finished.contains(&node) {
            let node_name = format!("{}#{} is closed", short_repo(&node.repo), node.number);
            verdicts.push(Verdict::Reap {
                id,
                // Naming the waived work in the reap line, not only in the
                // keep line it replaced: this is the row the human is about to
                // approve, and "and discarding …" is the part they might stop
                // at.
                reason: match (&workspace.unsaved, insist) {
                    (Some(unsaved), true) => format!("{node_name}, discarding {unsaved}"),
                    _ => node_name,
                },
            });
        } else {
            verdicts.push(Verdict::Keep {
                id,
                reason: format!("{}#{} is still open", short_repo(&node.repo), node.number),
            });
        }
    }
    verdicts
}

fn short_repo(slug: &str) -> &str {
    slug.split('/').next_back().unwrap_or(slug)
}

/// Ask `dl` what workspaces exist and what they hold.
///
/// # Errors
///
/// No `dl` on PATH, a `dl` too old to know `--ls --json`, or output that does
/// not parse. All three mean the same thing to the caller — `wf` cannot see the
/// workspaces, so it must not delete any.
pub async fn workspaces() -> Result<Vec<Workspace>> {
    let output = Command::new("dl")
        .args(["--ls", "--json"])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .context("failed to run `dl` — is devlaunch installed and on PATH?")?;
    if !output.status.success() {
        bail!(
            "`dl --ls --json` failed: {}\n\
             (needs devlaunch 0.0.21 or newer, which is where --json arrived)",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    parse_workspaces(&output.stdout)
}

/// The parse boundary for `dl`'s listing, kept apart from the process call so
/// it is testable without devlaunch installed.
fn parse_workspaces(body: &[u8]) -> Result<Vec<Workspace>> {
    serde_json::from_slice(body).context("unparseable workspace listing from `dl --ls --json`")
}

/// Which of `nodes` the tracker says are closed.
///
/// One `gh` search per repo rather than one call per ticket: the numbers are
/// asked about together, so ten workspaces of one repo cost one round trip.
///
/// # Errors
///
/// A `gh` that is missing, unauthenticated or refused. Never partial: a repo
/// whose state could not be read makes the whole call fail, because the
/// alternative is treating "could not ask" as "not closed" for some workspaces
/// and deleting the rest — a half-answered question is the one shape this must
/// not act on.
pub async fn finished_nodes(nodes: &BTreeSet<Node>) -> Result<BTreeSet<Node>> {
    let mut finished = BTreeSet::new();
    let mut repos: BTreeSet<&str> = BTreeSet::new();
    for node in nodes {
        repos.insert(node.repo.as_str());
    }
    for repo in repos {
        let wanted: Vec<u64> = nodes
            .iter()
            .filter(|n| n.repo == repo)
            .map(|n| n.number)
            .collect();
        for number in wanted {
            if issue_is_closed(repo, number).await? {
                finished.insert(Node {
                    repo: repo.to_string(),
                    number,
                });
            }
        }
    }
    Ok(finished)
}

/// Whether one issue is closed. `gh api` rather than a search, because search
/// indexes lag and a stale "still open" only ever costs a workspace that stays.
async fn issue_is_closed(repo: &str, number: u64) -> Result<bool> {
    let output = Command::new("gh")
        .args([
            "api",
            &format!("repos/{repo}/issues/{number}"),
            "--jq",
            ".state",
        ])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .context("failed to run `gh` — is the GitHub CLI installed and on PATH?")?;
    if !output.status.success() {
        bail!(
            "could not read {repo}#{number}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim() == "closed")
}

/// Hand one finished workspace back to `dl`.
///
/// `insist` passes `dl`'s own `--force`, and is only ever the human's `-f`
/// reaching this far: without it, `dl` refuses when a clone holds work that
/// exists nowhere else, and that refusal is load-bearing — [`plan`] already
/// skipped those, so a refusal here means the clone changed under us between
/// the listing and now, which is precisely the moment to stop rather than
/// insist. With it, the human has read the plan naming what would be discarded.
///
/// # Errors
///
/// No `dl` on PATH, or a `dl <ws> rm` that failed — including the refusal
/// above, which is a failure `wf` reports rather than quietly overrides.
pub async fn remove(id: &str, insist: bool) -> Result<()> {
    let mut args = vec![id, "rm"];
    if insist {
        args.push("--force");
    }
    let output = Command::new("dl")
        .args(&args)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .context("failed to run `dl`")?;
    if !output.status.success() {
        bail!(
            "`dl {id} rm` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Checks, Review};

    fn workspace(id: &str, repo: &str, branch: &str) -> Workspace {
        Workspace {
            id: id.to_string(),
            devlaunch: true,
            repo: Some(repo.to_string()),
            branch: Some(branch.to_string()),
            state: Some("Stopped".to_string()),
            unsaved: None,
        }
    }

    fn node(repo: &str, number: u64) -> Node {
        Node {
            repo: repo.to_string(),
            number,
        }
    }

    fn open_pr() -> PrStatus {
        PrStatus::Open {
            checks: Checks::Passing,
            review: Review::NotRequired,
        }
    }

    #[test]
    fn a_pr_this_binary_cannot_read_still_counts_as_a_pr_in_flight() {
        // The badge parse yields nothing for a state this binary does not
        // recognise -- on purpose, since no badge beats a wrong one. Reap must
        // still see a PR there. "In flight" is the only claim that stays true
        // whatever the new state turns out to mean, and it is the arm that
        // keeps the workspace and prints nothing; the alternative is a node
        // that reads as having no PR at all and slides into `Unstarted`.
        assert_eq!(PrOutcome::project(44, None), PrOutcome::InFlight { pr: 44 });
    }

    #[test]
    fn the_pr_projection_is_total_over_every_reading_the_badge_parse_can_yield() {
        // Three arms over the four badge statuses: reap's question is only
        // "does this PR keep the node alive, finish it, or say nothing".
        assert_eq!(
            PrOutcome::project(1, Some(&PrStatus::Draft)),
            PrOutcome::InFlight { pr: 1 }
        );
        assert_eq!(
            PrOutcome::project(2, Some(&open_pr())),
            PrOutcome::InFlight { pr: 2 }
        );
        assert_eq!(
            PrOutcome::project(3, Some(&PrStatus::Merged)),
            PrOutcome::Merged { pr: 3 }
        );
        assert_eq!(
            PrOutcome::project(4, Some(&PrStatus::Closed)),
            PrOutcome::ClosedUnmerged { pr: 4 }
        );
    }

    #[test]
    fn node_fact_is_total_over_the_rollup_and_the_claim() {
        let cases: Vec<(&str, Vec<PrOutcome>, NodeFact, NodeFact)> = vec![
            // (rollup name, rollup, open+assigned, open+unassigned)
            ("no PRs", vec![], NodeFact::Claimed, NodeFact::Unstarted),
            (
                "a draft PR",
                vec![PrOutcome::project(42, Some(&PrStatus::Draft))],
                NodeFact::InFlight { pr: 42 },
                NodeFact::InFlight { pr: 42 },
            ),
            (
                "an open PR",
                vec![PrOutcome::project(40, Some(&open_pr()))],
                NodeFact::InFlight { pr: 40 },
                NodeFact::InFlight { pr: 40 },
            ),
            (
                "a merged PR",
                vec![PrOutcome::project(33, Some(&PrStatus::Merged))],
                NodeFact::DoneByMerge { pr: 33 },
                NodeFact::DoneByMerge { pr: 33 },
            ),
            (
                "a closed-unmerged PR",
                vec![PrOutcome::project(43, Some(&PrStatus::Closed))],
                NodeFact::Superseded { pr: 43 },
                NodeFact::Superseded { pr: 43 },
            ),
            (
                "a merged PR beside an open one",
                vec![
                    PrOutcome::project(33, Some(&PrStatus::Merged)),
                    PrOutcome::project(40, Some(&open_pr())),
                ],
                NodeFact::InFlight { pr: 40 },
                NodeFact::InFlight { pr: 40 },
            ),
            (
                // fetch's own unknown-state fixture, carried the whole way in:
                // it must not read as "no PR" and slide into `Unstarted`.
                "a PR this binary cannot read",
                vec![PrOutcome::project(44, None)],
                NodeFact::InFlight { pr: 44 },
                NodeFact::InFlight { pr: 44 },
            ),
        ];
        for (name, prs, claimed, unclaimed) in cases {
            assert_eq!(node_fact(true, true, &prs), claimed, "open, assigned, {name}");
            assert_eq!(
                node_fact(true, false, &prs),
                unclaimed,
                "open, unassigned, {name}"
            );
            for assigned in [true, false] {
                assert_eq!(
                    node_fact(false, assigned, &prs),
                    NodeFact::Closed,
                    "closed, assigned={assigned}, {name}"
                );
            }
        }
    }

    #[test]
    fn the_fact_names_the_pr_its_reason_line_will_quote() {
        // Which PR the arm carries is the whole content of the printed row, so
        // it is pinned rather than left to the iteration order: the earliest
        // live PR for work in flight, the earliest merge for the merge that
        // finished it, and the *last* word on a superseded node.
        let in_flight = [
            PrOutcome::project(40, Some(&open_pr())),
            PrOutcome::project(37, Some(&PrStatus::Draft)),
        ];
        assert_eq!(
            node_fact(true, false, &in_flight),
            NodeFact::InFlight { pr: 37 }
        );
        let merged = [
            PrOutcome::project(97, Some(&PrStatus::Merged)),
            PrOutcome::project(91, Some(&PrStatus::Merged)),
        ];
        assert_eq!(
            node_fact(true, false, &merged),
            NodeFact::DoneByMerge { pr: 91 }
        );
        let superseded = [
            PrOutcome::project(90, Some(&PrStatus::Closed)),
            PrOutcome::project(97, Some(&PrStatus::Closed)),
        ];
        assert_eq!(
            node_fact(true, false, &superseded),
            NodeFact::Superseded { pr: 97 }
        );
    }

    #[test]
    fn a_workspace_wf_minted_names_its_node() {
        assert_eq!(
            node_of(&workspace(
                "ws",
                "blooop/devlaunch",
                "wayfinder/devlaunch-80"
            )),
            Some(node("blooop/devlaunch", 80))
        );
        // The short name is the repo's, so a repo whose name repeats inside the
        // branch still parses to the right number.
        assert_eq!(
            node_of(&workspace(
                "ws",
                "blooop/wayfinder",
                "wayfinder/wayfinder-42"
            )),
            Some(node("blooop/wayfinder", 42))
        );
    }

    #[test]
    fn anything_wf_did_not_mint_names_no_node_and_is_never_touched() {
        // A workspace dl did not create: its branch means whatever its author
        // meant, and wf has no business reading it.
        let mut foreign = workspace("ws", "blooop/devlaunch", "wayfinder/devlaunch-80");
        foreign.devlaunch = false;
        assert_eq!(node_of(&foreign), None);
        // Under the prefix, but not <short-repo>-<n>: a hand-made branch, not
        // ticket 3.
        assert_eq!(
            node_of(&workspace("ws", "blooop/devlaunch", "wayfinder/hotfix-3")),
            None
        );
        // The right shape for a *different* repo's tickets.
        assert_eq!(
            node_of(&workspace("ws", "blooop/devlaunch", "wayfinder/bencher-3")),
            None
        );
        // Ordinary branches.
        assert_eq!(node_of(&workspace("ws", "blooop/devlaunch", "main")), None);
        assert_eq!(
            node_of(&workspace("ws", "blooop/devlaunch", "feat/something")),
            None
        );
        // A trailing non-number, which `parse` must refuse rather than round.
        assert_eq!(
            node_of(&workspace(
                "ws",
                "blooop/devlaunch",
                "wayfinder/devlaunch-80-retry"
            )),
            None
        );
        // No record at all: dl could not say what it was for.
        let mut unrecorded = workspace("ws", "blooop/devlaunch", "wayfinder/devlaunch-80");
        unrecorded.branch = None;
        assert_eq!(node_of(&unrecorded), None);
    }

    #[test]
    fn a_closed_ticket_is_reaped_and_an_open_one_is_kept() {
        let listing = vec![
            workspace("done", "blooop/devlaunch", "wayfinder/devlaunch-80"),
            workspace("open", "blooop/devlaunch", "wayfinder/devlaunch-81"),
        ];
        let finished = BTreeSet::from([node("blooop/devlaunch", 80)]);
        let verdicts = plan(&listing, &finished, false);
        assert_eq!(
            verdicts,
            vec![
                Verdict::Reap {
                    id: "done".to_string(),
                    reason: "devlaunch#80 is closed".to_string()
                },
                Verdict::Keep {
                    id: "open".to_string(),
                    reason: "devlaunch#81 is still open".to_string()
                },
            ]
        );
    }

    #[test]
    fn unsaved_work_keeps_a_workspace_whose_ticket_is_closed() {
        // dl's fact, read rather than argued with: dl would refuse this delete,
        // and a caller that walks into a refusal it could have anticipated is
        // one that eventually learns to pass --force.
        let mut ws = workspace("done", "blooop/devlaunch", "wayfinder/devlaunch-80");
        ws.unsaved = Some("2 uncommitted change(s)".to_string());
        let finished = BTreeSet::from([node("blooop/devlaunch", 80)]);
        let verdicts = plan(&[ws], &finished, false);
        assert_eq!(
            verdicts,
            vec![Verdict::Keep {
                id: "done".to_string(),
                reason: "holds 2 uncommitted change(s)".to_string()
            }]
        );
    }

    #[test]
    fn a_running_workspace_is_kept_even_when_its_ticket_is_closed() {
        // The ticket closing does not mean the session in the container ended.
        let mut ws = workspace("done", "blooop/devlaunch", "wayfinder/devlaunch-80");
        ws.state = Some("Running".to_string());
        let finished = BTreeSet::from([node("blooop/devlaunch", 80)]);
        assert_eq!(
            plan(&[ws], &finished, false),
            vec![Verdict::Keep {
                id: "done".to_string(),
                reason: "still running — stop it first".to_string()
            }]
        );
    }

    #[test]
    fn unsaved_work_outranks_a_running_container_so_the_worse_news_is_the_one_shown() {
        let mut ws = workspace("done", "blooop/devlaunch", "wayfinder/devlaunch-80");
        ws.state = Some("Running".to_string());
        ws.unsaved = Some("1 unpushed commit(s)".to_string());
        let finished = BTreeSet::from([node("blooop/devlaunch", 80)]);
        assert!(plan(&[ws], &finished, false)[0]
            .reason()
            .contains("unpushed"));
    }

    #[test]
    fn workspaces_that_are_not_wfs_are_left_out_of_the_plan_entirely() {
        // Not "kept with a reason": a plan padded with everything wf does not
        // manage buries the rows that matter.
        let mut foreign = workspace("theirs", "blooop/devlaunch", "wayfinder/devlaunch-80");
        foreign.devlaunch = false;
        let listing = vec![
            foreign,
            workspace("mine", "blooop/devlaunch", "wayfinder/devlaunch-81"),
            workspace("plain", "blooop/devlaunch", "main"),
        ];
        let verdicts = plan(&listing, &BTreeSet::new(), false);
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0].id(), "mine");
    }

    #[test]
    fn the_listing_parses_what_dl_prints_and_tolerates_fields_it_does_not_know() {
        let body = br#"[
          {"id": "a", "devlaunch": true, "repo": "blooop/devlaunch",
           "branch": "wayfinder/devlaunch-80", "checkedOut": "wayfinder/devlaunch-80",
           "path": "/cache/x", "state": "Stopped",
           "lastUsed": "2026-08-08T11:43:27Z", "unsaved": null},
          {"id": "b", "devlaunch": false, "repo": null, "branch": null,
           "checkedOut": null, "path": null, "state": null,
           "lastUsed": "", "unsaved": null, "somethingNewer": 7}
        ]"#;
        let parsed = parse_workspaces(body).expect("dl's listing parses");
        assert_eq!(parsed.len(), 2);
        assert_eq!(node_of(&parsed[0]), Some(node("blooop/devlaunch", 80)));
        assert_eq!(node_of(&parsed[1]), None);
    }

    #[test]
    fn insisting_waives_the_unsaved_guard_and_says_what_it_is_discarding() {
        // The case this exists for: a devcontainer that installs packages in
        // its postCreateCommand leaves a tracked lockfile modified in every
        // workspace it builds, so without an override those are unreapable
        // forever. The reap line has to name what is being thrown away, since
        // that is the row being approved.
        let mut ws = workspace("done", "blooop/devlaunch", "wayfinder/devlaunch-80");
        ws.unsaved = Some("1 uncommitted change(s) (pixi.lock)".to_string());
        let finished = BTreeSet::from([node("blooop/devlaunch", 80)]);
        assert_eq!(
            plan(std::slice::from_ref(&ws), &finished, true),
            vec![Verdict::Reap {
                id: "done".to_string(),
                reason: "devlaunch#80 is closed, discarding 1 uncommitted change(s) (pixi.lock)"
                    .to_string()
            }]
        );
        // And it is only ever a waiver of *that* guard: an open ticket is still
        // an open ticket.
        assert!(matches!(
            plan(&[ws], &BTreeSet::new(), true).as_slice(),
            [Verdict::Keep { .. }]
        ));
    }

    #[test]
    fn insisting_never_reaps_a_running_container() {
        // The unsaved guard is about bytes on disk and can be waived by someone
        // who has read what they are discarding. A running container is a
        // session in progress -- a different question, and not one `-f` was
        // given an answer to.
        let mut ws = workspace("done", "blooop/devlaunch", "wayfinder/devlaunch-80");
        ws.state = Some("Running".to_string());
        ws.unsaved = Some("1 uncommitted change(s) (pixi.lock)".to_string());
        let finished = BTreeSet::from([node("blooop/devlaunch", 80)]);
        assert_eq!(
            plan(&[ws], &finished, true),
            vec![Verdict::Keep {
                id: "done".to_string(),
                reason: "still running — stop it first".to_string()
            }]
        );
    }

    #[test]
    fn an_unreadable_listing_is_an_error_rather_than_an_empty_plan() {
        // Empty would mean "nothing to reap", which is indistinguishable from
        // success and would hide a broken or too-old `dl`.
        assert!(parse_workspaces(b"not json").is_err());
        assert!(parse_workspaces(b"").is_err());
    }
}
