//! Noticing what [`reap`] would claim, without being asked (#137).
//!
//! `wf reap` already decides what is finished. What it lacks is a trigger: it
//! fires only when a person remembers to type it. This module is that trigger
//! for the **noticing** and deliberately not for the deleting — the picker says
//! what a reap would claim and points at the command, and a human still runs it.
//!
//! Three properties, and each one is why this module is shaped the way it is.
//!
//! **One vocabulary.** The surfaced set is [`reap::doomed`] over
//! [`reap::plan`], called rather than reimplemented. `doomed` stays the single
//! place that decides what goes (#129), so a display cannot drift into a
//! second, unaudited definition of "finished", and a [`Verdict::Warn`] row
//! keeps its never-acted-on posture (#128) — counted here, never named as
//! reclaimable.
//!
//! **Nothing here can delete.** `plan` is asked with `insist` false, so a
//! workspace holding work that exists nowhere else is not surfaced at all; and
//! the whole module reads facts and returns a sentence. It spawns no process of
//! its own, and never reaches `reap`'s deletion side. The reading is the
//! product; the waiver, the prompt and the deletion all stay with the human
//! typing `wf reap`.
//!
//! **It fails silent.** [`survey`] answers `Option`, not `Result`. No `dl` on
//! PATH, a `dl --ls --json` that failed, a GraphQL error, no network: the hint
//! is simply absent. A cleanup convenience is never worth an error, a stall, or
//! a degraded launcher.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;

use anyhow::Result;

use crate::reap::{self, Node, NodeFact, Verdict, Workspace};

/// How many workspaces the hint spells before it starts counting.
///
/// A bare count cannot be judged — "3 reclaimable" is a number a reader has no
/// way to agree or disagree with — so the hint names what it means. It stops at
/// three because this shares one line with the load state and the match count.
const NAMED: usize = 3;

/// What a reap would claim, ready to say on one line.
///
/// Constructible only from a set of [`Verdict`]s, through the private `read`
/// below — which answers `None` when [`reap::doomed`] is empty. So "there is nothing
/// to reclaim" is the absence of this value rather than a value claiming zero,
/// and no code path can render a hint about an empty set.
///
/// `warned` is a count and nothing more. The ids are the doomed set's, so a
/// warning has no way to reach the reclaimable list even by accident: it is
/// carried in a different field of a different type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reclaimable {
    /// The doomed workspaces' ids, in listing order. Never empty.
    ids: Vec<String>,
    /// How many rows `plan` warned about — printed as a separate clause,
    /// never counted into the reclaimable set (#128).
    warned: usize,
}

impl Reclaimable {
    /// The reading over one plan: [`reap::doomed`]'s ids, and the warnings
    /// counted beside them.
    ///
    /// `None` when nothing is doomed, which is the ordinary case and the one
    /// that must draw nothing at all.
    fn read(verdicts: &[Verdict]) -> Option<Self> {
        let ids: Vec<String> = reap::doomed(verdicts)
            .into_iter()
            .map(|verdict| verdict.id().to_string())
            .collect();
        if ids.is_empty() {
            return None;
        }
        let warned = verdicts
            .iter()
            .filter(|verdict| matches!(verdict, Verdict::Warn { .. }))
            .count();
        Some(Self { ids, warned })
    }

    /// A reading with these ids on it, for the tests of the screen and the
    /// channel that carry one. Test-only so that production has exactly one
    /// way in — [`Reclaimable::read`], over a plan.
    #[cfg(test)]
    pub(crate) fn for_test(ids: &[&str], warned: usize) -> Self {
        assert!(!ids.is_empty(), "a reading is never empty");
        Self {
            ids: ids.iter().map(|id| (*id).to_string()).collect(),
            warned,
        }
    }

    /// The workspaces a reap would claim, by the id `wf reap` and `dl` both
    /// name them by.
    pub fn ids(&self) -> &[String] {
        &self.ids
    }

    /// How many rows `wf` is uneasy about but will not act on.
    pub fn warned(&self) -> usize {
        self.warned
    }

    /// The count-line segment: how many, which ones, and what to type.
    ///
    /// The warned count is a parenthesised aside so the leading number cannot
    /// be read as including it — the whole point of the `Warn` arm is that
    /// those rows are not part of what would go.
    pub fn hint(&self) -> String {
        let mut named: Vec<String> = self.ids.iter().take(NAMED).cloned().collect();
        if self.ids.len() > NAMED {
            named.push(format!("+{} more", self.ids.len() - NAMED));
        }
        let aside = match self.warned {
            0 => String::new(),
            n => format!(" (+{n} to check by hand)"),
        };
        format!(
            "· {} reclaimable: {}{aside} — wf reap",
            self.ids.len(),
            named.join(", ")
        )
    }
}

/// What a reap would claim, given what `dl` listed and what the tracker said.
///
/// The one place the surfaced set is decided, and it decides nothing itself:
/// [`reap::plan`] then [`reap::doomed`], the same two calls `wf reap` makes.
///
/// `insist` is **false**, always. `-f` waives `dl`'s unsaved-work guard, and
/// that waiver is a human's to grant while looking at the plan — a hint that
/// quietly counted the workspaces `-f` would reach would be naming work that
/// exists nowhere else as disposable.
pub fn reading(workspaces: &[Workspace], known: &BTreeMap<Node, NodeFact>) -> Option<Reclaimable> {
    Reclaimable::read(&reap::plan(workspaces, known, false))
}

/// Take the reading, silently.
///
/// Generic over its two reads so the whole fail-silent path is exercisable
/// without `dl`, `gh` or a network — [`survey_live`] is the one-line binding to
/// the real pair.
///
/// Every failure is the same answer: no hint. A listing that would not run, a
/// tracker that would not answer, a repo slug that will not parse — none of
/// them is a fact about a workspace, and the only honest thing to show for one
/// is nothing.
pub async fn survey<L, LFut, F, FFut>(listing: L, facts: F) -> Option<Reclaimable>
where
    L: FnOnce() -> LFut,
    LFut: Future<Output = Result<Vec<Workspace>>>,
    F: FnOnce(BTreeSet<Node>) -> FFut,
    FFut: Future<Output = Result<BTreeMap<Node, NodeFact>>>,
{
    let workspaces = listing().await.ok()?;
    let nodes: BTreeSet<Node> = workspaces.iter().filter_map(reap::node_of).collect();
    if nodes.is_empty() {
        // Nothing of `wf`'s on this machine. Not a question worth a round trip,
        // and not a hint worth drawing.
        return None;
    }
    let known = facts(nodes).await.ok()?;
    reading(&workspaces, &known)
}

/// [`survey`] against the real `dl` listing and the real batched tracker query
/// — one subprocess and one GraphQL round trip, both `reap`'s own.
pub async fn survey_live() -> Option<Reclaimable> {
    survey(reap::workspaces, |nodes| async move {
        reap::node_facts(&nodes).await
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    fn workspace(id: &str, repo: &str, number: u64) -> Workspace {
        Workspace {
            id: id.to_string(),
            devlaunch: true,
            repo: Some(repo.to_string()),
            branch: Some(format!("wayfinder/{}-{number}", short(repo))),
            state: Some("Stopped".to_string()),
            unsaved: None,
        }
    }

    fn short(repo: &str) -> &str {
        repo.split('/').next_back().unwrap_or(repo)
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

    /// Every arm of `NodeFact`, so the table below cannot quietly stop covering
    /// one when a seventh is added — the match makes that a compile error.
    fn every_node_fact() -> Vec<NodeFact> {
        let all = vec![
            NodeFact::Closed,
            NodeFact::DoneByMerge { pr: 97 },
            NodeFact::Superseded { pr: 98 },
            NodeFact::InFlight { pr: 99 },
            NodeFact::Claimed,
            NodeFact::Unstarted,
        ];
        for fact in &all {
            match fact {
                NodeFact::Closed
                | NodeFact::DoneByMerge { .. }
                | NodeFact::Superseded { .. }
                | NodeFact::InFlight { .. }
                | NodeFact::Claimed
                | NodeFact::Unstarted => {}
            }
        }
        all
    }

    #[test]
    fn the_surfaced_set_is_exactly_the_doomed_set_over_every_node_fact() {
        // The guard against a second definition of "finished". The expectation
        // is not a literal list: it is `doomed(plan(…))` computed right here,
        // so a display that started deciding for itself — including one that
        // merely re-derived the same answer — fails the moment the two drift.
        for fact in every_node_fact() {
            for unsaved in [None, Some("1 uncommitted change(s) (pixi.lock)")] {
                for state in ["Stopped", "Running"] {
                    let mut ws = workspace("solo", "blooop/devlaunch", 80);
                    ws.unsaved = unsaved.map(str::to_string);
                    ws.state = Some(state.to_string());
                    let known = facts([(node("blooop/devlaunch", 80), fact.clone())]);
                    let listed = std::slice::from_ref(&ws);

                    let verdicts = reap::plan(listed, &known, false);
                    let expected: Vec<String> = reap::doomed(&verdicts)
                        .into_iter()
                        .map(|v| v.id().to_string())
                        .collect();
                    let surfaced: Vec<String> = reading(listed, &known)
                        .map(|r| r.ids().to_vec())
                        .unwrap_or_default();

                    assert_eq!(
                        surfaced, expected,
                        "{fact:?} / unsaved={unsaved:?} / {state}: the surfaced set \
                         must be `doomed(plan(…))` and nothing else"
                    );
                }
            }
        }
    }

    #[test]
    fn a_warned_workspace_is_never_surfaced_as_reclaimable() {
        // The mirror of reap's own
        // `a_warned_workspace_never_reaches_the_doomed_set_even_with_y_and_f`,
        // pointed at the display: the safety property of the whole `Warn` arm
        // is that no path turns a suspicion into something offered up for
        // deletion. There are no flags to vary here — that is the point, the
        // reading has none — so the variation is the two warning facts and
        // whether the clone holds work.
        for fact in [NodeFact::Superseded { pr: 97 }, NodeFact::Unstarted] {
            for unsaved in [None, Some("1 uncommitted change(s) (pixi.lock)")] {
                let mut ws = workspace("warned", "blooop/devlaunch", 80);
                ws.unsaved = unsaved.map(str::to_string);
                let known = facts([(node("blooop/devlaunch", 80), fact.clone())]);
                assert_eq!(
                    reading(std::slice::from_ref(&ws), &known),
                    None,
                    "{fact:?} / unsaved={unsaved:?} was surfaced as reclaimable"
                );
            }
        }
    }

    #[test]
    fn a_warning_beside_a_real_one_is_counted_and_still_not_named() {
        // The `Warn` count is allowed (#128 lets warnings be counted) and must
        // stay on its own side of the sentence: the reclaimable list is the
        // doomed ids, and the warning is an aside the leading number excludes.
        let workspaces = vec![
            workspace("doomed", "blooop/devlaunch", 80),
            workspace("warned", "blooop/devlaunch", 96),
        ];
        let known = facts([
            (node("blooop/devlaunch", 80), NodeFact::Closed),
            (node("blooop/devlaunch", 96), NodeFact::Unstarted),
        ]);
        let reading = reading(&workspaces, &known).expect("one workspace is doomed");
        assert_eq!(reading.ids(), ["doomed"]);
        assert_eq!(reading.warned(), 1);
        let hint = reading.hint();
        assert!(
            hint.starts_with("· 1 reclaimable: doomed"),
            "the count and the names are the doomed set's alone: {hint}"
        );
        assert!(
            !hint.contains("warned"),
            "a warned workspace must never be named as reclaimable: {hint}"
        );
        assert!(
            hint.contains("(+1 to check by hand)"),
            "the warning is counted, in its own clause: {hint}"
        );
    }

    #[test]
    fn the_reading_never_waives_the_guard_that_keeps_unpushed_work() {
        // `-f` is the human's to grant in front of the plan. A workspace whose
        // clone holds work that exists nowhere else is kept by `plan`, and the
        // hint must inherit that keep rather than quietly count it — otherwise
        // the picker is advertising a reap that would throw work away.
        let mut ws = workspace("dirty", "blooop/devlaunch", 80);
        ws.unsaved = Some("1 uncommitted change(s) (pixi.lock)".to_string());
        let known = facts([(node("blooop/devlaunch", 80), NodeFact::Closed)]);
        assert_eq!(reading(std::slice::from_ref(&ws), &known), None);
        // And the same node with a clean clone *is* surfaced, so the assertion
        // above is about the unsaved work and not about the fixture.
        ws.unsaved = None;
        assert_eq!(
            reading(&[ws], &known).map(|r| r.ids().to_vec()),
            Some(vec!["dirty".to_string()])
        );
    }

    #[test]
    fn nothing_to_reclaim_is_no_value_at_all_rather_than_a_zero() {
        let ws = workspace("live", "blooop/devlaunch", 80);
        let known = facts([(node("blooop/devlaunch", 80), NodeFact::InFlight { pr: 9 })]);
        assert_eq!(reading(&[ws], &known), None);
        // The same claim at the constructor, which is the only way in.
        assert_eq!(Reclaimable::read(&[]), None);
    }

    #[test]
    fn the_hint_names_what_it_found_and_the_command_that_acts_on_it() {
        // A count alone cannot be judged. The names are what make the hint
        // something a reader can disagree with, and `wf reap` is the only
        // action it points at.
        let workspaces: Vec<Workspace> = (0..5)
            .map(|i| workspace(&format!("ws-{i}"), "blooop/devlaunch", 80 + i))
            .collect();
        let known = facts([
            (node("blooop/devlaunch", 80), NodeFact::Closed),
            (node("blooop/devlaunch", 81), NodeFact::Closed),
            (node("blooop/devlaunch", 82), NodeFact::Closed),
            (node("blooop/devlaunch", 83), NodeFact::Closed),
            (node("blooop/devlaunch", 84), NodeFact::Closed),
        ]);
        let reading = reading(&workspaces, &known).expect("five closed tickets");
        assert_eq!(
            reading.hint(),
            "· 5 reclaimable: ws-0, ws-1, ws-2, +2 more — wf reap"
        );
        // One is spelt outright, with no count of the unspelt.
        let one = Reclaimable {
            ids: vec!["only".to_string()],
            warned: 0,
        };
        assert_eq!(one.hint(), "· 1 reclaimable: only — wf reap");
    }

    #[tokio::test]
    async fn a_failing_dl_listing_produces_no_hint_and_no_error() {
        // No `dl` on PATH, a `dl` too old for `--json`, a listing that will not
        // parse: all the same answer. The signature carries half the claim —
        // there is no `Result` to propagate — and this pins the other half,
        // that the failure is not a panic either.
        let surfaced = survey(
            || async {
                Err(anyhow!(
                    "failed to run `dl` — is devlaunch installed and on PATH?"
                ))
            },
            |_| async { unreachable!("the tracker must not be asked about a listing that failed") },
        )
        .await;
        assert_eq!(surfaced, None);
    }

    #[tokio::test]
    async fn a_failing_node_facts_query_produces_no_hint_and_no_error() {
        // The listing worked and the tracker did not: a GraphQL error, an
        // unauthenticated `gh`, no network. Same answer, and in particular not
        // a reading taken from the half of the evidence that did arrive.
        let surfaced = survey(
            || async { Ok(vec![workspace("doomed", "blooop/devlaunch", 80)]) },
            |_| async { Err(anyhow!("GraphQL error: Bad credentials")) },
        )
        .await;
        assert_eq!(surfaced, None);
    }

    #[tokio::test]
    async fn a_machine_with_no_wayfinder_workspaces_asks_the_tracker_nothing() {
        // The listing is `dl`'s whole machine, most of which is not `wf`'s. No
        // node means no question worth a round trip — and the `unreachable!`
        // is what pins that rather than merely observing an empty answer.
        let mut foreign = workspace("theirs", "blooop/devlaunch", 80);
        foreign.devlaunch = false;
        let surfaced = survey(
            || async { Ok(vec![foreign]) },
            |_| async { unreachable!("no nodes means no tracker query") },
        )
        .await;
        assert_eq!(surfaced, None);
    }

    #[tokio::test]
    async fn a_survey_that_lands_reads_exactly_what_a_reap_would_claim() {
        let listed = vec![
            workspace("doomed", "blooop/devlaunch", 80),
            workspace("alive", "blooop/devlaunch", 81),
        ];
        let known = facts([
            (node("blooop/devlaunch", 80), NodeFact::Closed),
            (node("blooop/devlaunch", 81), NodeFact::InFlight { pr: 9 }),
        ]);
        let asked = std::cell::RefCell::new(BTreeSet::new());
        let surfaced = survey(
            || async { Ok(listed.clone()) },
            |nodes| {
                *asked.borrow_mut() = nodes;
                async { Ok(known.clone()) }
            },
        )
        .await;
        assert_eq!(
            surfaced,
            reading(&listed, &known),
            "the survey's answer is the reading over what the two reads returned"
        );
        assert_eq!(
            asked.into_inner(),
            [node("blooop/devlaunch", 80), node("blooop/devlaunch", 81)]
                .into_iter()
                .collect::<BTreeSet<_>>(),
            "one batched question, covering every node the listing named"
        );
    }

    /// This module's own source, with the comments and the tests stripped —
    /// the guard below is about what the code can do, not about what the prose
    /// says it does.
    fn code_only() -> String {
        include_str!("reclaim.rs")
            .lines()
            .take_while(|line| !line.starts_with("#[cfg(test)]"))
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn no_deletion_is_reachable_from_the_surfacing_path() {
        // The whole safety claim of #137, asserted structurally because there
        // is no behaviour to observe: this module must have no way to destroy
        // anything, so what is pinned is that it does not name the means.
        // `reap::remove` is the one function that deletes, `dl <ws> rm` the one
        // command, `--force` the one waiver, and a `Command` of its own would
        // be a way around all three.
        let code = code_only();
        for forbidden in [
            "remove",
            "\"rm\"",
            "--force",
            "Command",
            "process::",
            "unsafe",
        ] {
            assert!(
                !code.contains(forbidden),
                "the surfacing path must not be able to delete: it names {forbidden:?}"
            );
        }
        // And the one call it *does* make into reap's planning is never the
        // insisting one — the waiver stays with the human reading the plan.
        assert!(
            code.contains("reap::plan(workspaces, known, false)"),
            "the reading must plan without insisting"
        );
    }

    #[test]
    fn the_surfaced_set_comes_from_doomed_rather_than_a_partition_written_twice() {
        // #129's single definition, pinned at the one site that could quietly
        // grow a second one. A `matches!(v, Verdict::Reap { .. })` here would
        // pass every behavioural test in this file on the day it was written
        // and drift the first time `doomed` changed.
        let code = code_only();
        assert!(
            code.contains("reap::doomed(verdicts)"),
            "the doomed set must be asked for, not re-derived"
        );
        assert!(
            !code.contains("Verdict::Reap"),
            "nothing here may decide what a reap would take: {}",
            "that is `doomed`'s job alone"
        );
    }
}
