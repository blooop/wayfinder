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
//! the whole module reads facts and returns a sentence. It cannot reach
//! `reap`'s deletion side even by trying: the function that removes a workspace
//! is private to `reap`'s own module, so `reap::remove(id, true).await` written
//! anywhere in this file is `E0603`, not a test failure. The reading is the
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

use crate::liveness::Liveness;
use crate::reap::{self, Node, NodeFact, Verdict, Workspace};

/// How many workspaces the hint spells before it starts counting.
///
/// A bare count cannot be judged — "3 reclaimable" is a number a reader has no
/// way to agree or disagree with — so the hint names what it means. It stops at
/// three because this shares one line with the load state and the match count.
const NAMED: usize = 3;

/// The shortest workspace name worth printing.
///
/// Below this an abbreviation has eaten the name — `de…c8` identifies nothing —
/// and the line is better spent on the command, which is the half a reader can
/// act on. Eight characters is enough for both ends of a `dl` id to show.
const READABLE: usize = 8;

/// The two fixed halves of the sentence. The pointer is what makes the hint
/// actionable and the aside is #128's whole posture, so neither is ever what
/// gets clipped: the names are budgeted around them.
const POINTER: &str = " — wf reap";

/// One id, shortened in the middle to `max` characters if it is longer.
///
/// Both ends survive because both carry meaning: `dl` puts the tool and the
/// repo at the front and the ticket number at the back, and the number is what
/// tells two workspaces of one project apart. The tail therefore gets the odd
/// character when the budget is odd.
fn abbreviate(id: &str, max: usize) -> String {
    let len = id.chars().count();
    if len <= max {
        return id.to_string();
    }
    let keep = max.saturating_sub(1);
    let tail = keep.div_ceil(2);
    let head = keep - tail;
    let front: String = id.chars().take(head).collect();
    let back: String = id.chars().skip(len - tail).collect();
    format!("{front}…{back}")
}

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

    /// The count-line segment, in `width` characters: how many, which ones,
    /// and what to type.
    ///
    /// The warned count is a parenthesised aside so the leading number cannot
    /// be read as including it — the whole point of the `Warn` arm is that
    /// those rows are not part of what would go.
    ///
    /// **The width is a budget, not a suggestion**, at every width and not
    /// merely at the comfortable ones. This shares one line with the load state
    /// and the match count, and a real `dl` id is ~40 characters, so three of
    /// them written out unconditionally push the `wf reap` pointer and the
    /// warned aside off the end of an 80-column terminal — leaving a hint that
    /// names things and says nothing about what to do with them, which is the
    /// one shape #137 rules out. So the two ends of the sentence are laid down
    /// first and the names take what is left: as many whole ones as fit, then
    /// one shortened in the middle, then — on a screen too narrow for any
    /// readable name — none.
    ///
    /// Below that the fixed halves themselves stop fitting, and the same
    /// argument decides the order they go in. The aside is a count of rows
    /// nobody has to act on today; the pointer is the half a reader can act on,
    /// the same reason a name below `READABLE` characters is dropped. So the
    /// aside is dropped before the pointer, and only a width too narrow for
    /// even `· N reclaimable` clips anything at all.
    ///
    /// **The cost of that order, named rather than hidden.** The aside is
    /// treated as fixed while the names are being budgeted, so it is only ever
    /// dropped *after* naming has already been given up. With one warning that
    /// costs 22 characters, and below about 78 columns the segment is the bare
    /// count — while spending those 22 characters on a name instead would have
    /// left room for an abbreviated one and a `+N more`. Both readings are
    /// defensible and this one is deliberate: `(+N to check by hand)` is the
    /// whole of #128's posture on the screen, and a reader who cannot see it
    /// has no way to know those rows were considered at all, whereas a reader
    /// who cannot see a name still has `wf reap`, which prints every name.
    /// A future that disagrees should change the order here, not the budget.
    pub fn hint(&self, width: usize) -> String {
        let head = format!("· {} reclaimable", self.ids.len());
        let aside = match self.warned {
            0 => String::new(),
            n => format!(" (+{n} to check by hand)"),
        };
        let fixed = head.chars().count() + aside.chars().count() + POINTER.chars().count();
        // ": " is the cost of naming anything at all.
        let room = width.saturating_sub(fixed + 2);
        let say = |names: &str| format!("{head}: {names}{aside}{POINTER}");

        // Whole names, as many as the room allows.
        for count in (1..=NAMED.min(self.ids.len())).rev() {
            let mut spelt: Vec<String> = self.ids.iter().take(count).cloned().collect();
            if self.ids.len() > count {
                spelt.push(format!("+{} more", self.ids.len() - count));
            }
            let names = spelt.join(", ");
            if names.chars().count() <= room {
                return say(&names);
            }
        }

        // Not even one whole name. One shortened one still says which project
        // and which ticket, which is what the reader is checking.
        let rest = match self.ids.len() {
            1 => String::new(),
            n => format!(", +{} more", n - 1),
        };
        let left = room.saturating_sub(rest.chars().count());
        if left >= READABLE {
            return say(&format!("{}{rest}", abbreviate(&self.ids[0], left)));
        }

        // No name at all, so the fixed halves are all there is — and they are
        // measured too, in the order that keeps the actionable one. The last
        // arm is the count alone; a width even that does not fit gets what
        // there is room for, because overflowing the budget is what clips the
        // *neighbouring* segments of the count line.
        for tail in [format!("{aside}{POINTER}"), POINTER.to_string()] {
            let line = format!("{head}{tail}");
            if line.chars().count() <= width {
                return line;
            }
        }
        head.chars().take(width).collect()
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

/// Everything one look at the machine and the tracker is worth.
///
/// Two observations, carried together because they come from **one** reading:
/// the same `dl --ls --json` and the same batched tracker query answer both, so
/// splitting them into two surveys would double the subprocess and the round
/// trip to learn two things about the same six workspaces.
///
/// They stay separate values inside it rather than merging into one verdict per
/// node, because they are answers to unrelated questions and are acted on by
/// different parts of the screen: [`Reclaimable`] is a sentence naming a
/// command, and [`Liveness`] is a per-row marking. A node can easily be in one
/// and not the other, and a type that made them one field would have to invent
/// a precedence between "this could be tidied away" and "this stopped moving"
/// that nothing actually wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    reclaimable: Option<Reclaimable>,
    liveness: Liveness,
}

impl Reading {
    /// `None` when neither half found anything — the ordinary quiet machine,
    /// and the case that must not wake the draw path at all.
    fn of(reclaimable: Option<Reclaimable>, liveness: Liveness) -> Option<Self> {
        if reclaimable.is_none() && liveness.is_empty() {
            return None;
        }
        Some(Self {
            reclaimable,
            liveness,
        })
    }

    /// Hand both halves to their separate homes on the app. Consuming, because
    /// there is exactly one consumer and nothing should be tempted to keep a
    /// second copy of a reading that arrives once.
    pub fn into_parts(self) -> (Option<Reclaimable>, Liveness) {
        (self.reclaimable, self.liveness)
    }

    /// A reading with these halves on it, for the tests of the channel that
    /// carries one.
    #[cfg(test)]
    pub(crate) fn for_test(reclaimable: Option<Reclaimable>, liveness: Liveness) -> Self {
        Self::of(reclaimable, liveness).expect("a reading is never empty")
    }
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
pub async fn survey<L, LFut, F, FFut>(listing: L, facts: F) -> Option<Reading>
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
    Reading::of(
        reading(&workspaces, &known),
        Liveness::read(&workspaces, &known),
    )
}

/// [`survey`] against the real `dl` listing and the real batched tracker query
/// — one subprocess and one GraphQL round trip, both `reap`'s own.
pub async fn survey_live() -> Option<Reading> {
    survey(reap::workspaces, |nodes| async move {
        reap::node_facts(&nodes).await
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reap::Unsaved;
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
                    ws.unsaved = unsaved.map(|u| Unsaved::WouldLose(u.to_string()));
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
                ws.unsaved = unsaved.map(|u| Unsaved::WouldLose(u.to_string()));
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
        //
        // The three `Keep`s are not scenery. "How many did we warn about" and
        // "how many did we not reap" are different numbers, and the second one
        // — every healthy workspace on the machine — is much the larger. An
        // aside computed as `verdicts.len() - doomed.len()` would read `+4` on
        // this fixture and tell the user to go and inspect four workspaces, of
        // which three are simply in use.
        let workspaces = vec![
            workspace("doomed", "blooop/devlaunch", 80),
            workspace("warned", "blooop/devlaunch", 96),
            workspace("claimed", "blooop/devlaunch", 97),
            workspace("in-flight", "blooop/devlaunch", 98),
            {
                let mut dirty = workspace("dirty", "blooop/devlaunch", 99);
                dirty.unsaved = Some(Unsaved::WouldLose(
                    "1 uncommitted change(s) (pixi.lock)".to_string(),
                ));
                dirty
            },
        ];
        let known = facts([
            (node("blooop/devlaunch", 80), NodeFact::Closed),
            (node("blooop/devlaunch", 96), NodeFact::Unstarted),
            (node("blooop/devlaunch", 97), NodeFact::Claimed),
            (node("blooop/devlaunch", 98), NodeFact::InFlight { pr: 9 }),
            (node("blooop/devlaunch", 99), NodeFact::Closed),
        ]);
        let reading = reading(&workspaces, &known).expect("one workspace is doomed");
        assert_eq!(reading.ids(), ["doomed"]);
        assert_eq!(
            reading.warned(),
            1,
            "one row was warned about; the other three are kept, which is not \
             the same thing and is not a person's problem"
        );
        let hint = reading.hint(120);
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
        ws.unsaved = Some(Unsaved::WouldLose(
            "1 uncommitted change(s) (pixi.lock)".to_string(),
        ));
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
            reading.hint(120),
            "· 5 reclaimable: ws-0, ws-1, ws-2, +2 more — wf reap"
        );
        // One is spelt outright, with no count of the unspelt.
        let one = Reclaimable {
            ids: vec!["only".to_string()],
            warned: 0,
        };
        assert_eq!(one.hint(120), "· 1 reclaimable: only — wf reap");
    }

    /// Three workspaces named the way `dl` actually names them: 41 characters
    /// each, because the id carries the host, the owner, the repo and the
    /// ticket. Every width claim below is measured against these rather than
    /// against `ws-0`, which fits anywhere and therefore proves nothing.
    fn realistic() -> Reclaimable {
        Reclaimable {
            ids: vec![
                "devlaunch-github-com-blooop-wayfinder-129".to_string(),
                "devlaunch-github-com-blooop-wayfinder-127".to_string(),
                "devlaunch-github-com-blooop-wayfinder-80x".to_string(),
            ],
            warned: 1,
        }
    }

    #[test]
    fn the_pointer_and_the_aside_survive_real_workspace_names_on_a_narrow_screen() {
        // The failure this exists for: three 41-character ids written out
        // unconditionally are 123 characters before the sentence reaches
        // `— wf reap`, so on an 80-column terminal the hint named some
        // workspaces and then stopped — no aside, no command, nothing to do.
        // Whatever the width, the reader can still see how many, how many need
        // a human, and what to type.
        for width in [60, 70, 80, 100, 120] {
            let hint = realistic().hint(width);
            assert!(
                hint.chars().count() <= width,
                "{width}: the hint must fit the budget it was given: {hint:?}"
            );
            assert!(
                hint.starts_with("· 3 reclaimable"),
                "{width}: the count leads: {hint:?}"
            );
            assert!(
                hint.ends_with("(+1 to check by hand) — wf reap"),
                "{width}: the aside and the pointer are never what gets cut: {hint:?}"
            );
        }
    }

    #[test]
    fn a_name_too_long_for_the_line_is_shortened_at_both_ends_rather_than_dropped() {
        // 80 columns has no room for a whole 41-character id beside the aside
        // and the pointer, and a bare "3 reclaimable" is exactly the
        // unjudgeable count #137 rules out. So one name is kept, shortened in
        // the middle — the front says whose it is, the back says which ticket.
        let hint = realistic().hint(80);
        assert!(hint.contains('…'), "{hint}");
        assert!(
            hint.contains("devlaunch"),
            "the front of the name survives: {hint}"
        );
        assert!(
            hint.contains("129"),
            "so does the ticket number, which is what identifies it: {hint}"
        );
        assert!(
            hint.contains("+2 more"),
            "and the two it could not name are still counted: {hint}"
        );
    }

    #[test]
    fn a_screen_too_narrow_for_any_readable_name_keeps_the_command() {
        // The last resort, and the right one: half a name is not something a
        // reader can check, while `wf reap` is still something they can run.
        // 47 characters is what the count, the aside and the pointer cost
        // together, so that is the narrowest screen all three survive on.
        let hint = realistic().hint(47);
        assert_eq!(hint, "· 3 reclaimable (+1 to check by hand) — wf reap");
        // One character narrower and something has to go. The aside counts
        // rows nobody has to act on today; the pointer is the only thing on
        // this line anyone can do — so the aside is what goes.
        assert_eq!(realistic().hint(46), "· 3 reclaimable — wf reap");
        assert_eq!(realistic().hint(25), "· 3 reclaimable — wf reap");
    }

    #[test]
    fn the_hint_fits_its_budget_at_every_width() {
        // The doc says the width is a budget and not a suggestion, and this is
        // the sentence that says it at *every* width rather than at the five
        // comfortable ones the test above picks. The count line is shared: a
        // hint that overruns does not merely lose its own tail, it clips the
        // segments beside it.
        for warned in [0, 1, 12] {
            for count in 1..=4 {
                let found = Reclaimable {
                    ids: (0..count)
                        .map(|i| format!("devlaunch-github-com-blooop-wayfinder-12{i}"))
                        .collect(),
                    warned,
                };
                for width in 0..=140 {
                    let hint = found.hint(width);
                    assert!(
                        hint.chars().count() <= width,
                        "{count} ids, {warned} warned, width {width}: {hint:?} is \
                         {} characters",
                        hint.chars().count()
                    );
                }
            }
        }
    }

    #[test]
    fn the_command_is_the_last_thing_the_budget_gives_up() {
        // The ordering the last-resort branch encodes, stated as its own
        // claim: as the screen narrows, the names go, then the aside, and
        // `wf reap` survives every width that can hold it at all.
        let found = realistic();
        let widest_without_the_command = (0..=140)
            .filter(|w| !found.hint(*w).contains("wf reap"))
            .max()
            .expect("some width is too narrow for the pointer");
        assert_eq!(
            widest_without_the_command, 24,
            "the pointer must survive every width `· N reclaimable — wf reap` fits in"
        );
    }

    #[test]
    fn abbreviating_keeps_both_ends_and_never_grows_a_name() {
        assert_eq!(abbreviate("short", 20), "short", "it already fits");
        assert_eq!(abbreviate("short", 5), "short", "exactly, and still whole");
        assert_eq!(abbreviate("devlaunch-wayfinder-127", 12), "devla…er-127");
        for max in 2..30 {
            let out = abbreviate("devlaunch-github-com-blooop-wayfinder-127", max);
            assert_eq!(out.chars().count(), max, "{max}: {out}");
        }
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
            Reading::of(reading(&listed, &known), Liveness::read(&listed, &known)),
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

    /// The reading taken for real, in a child process whose `dl` and `gh` are
    /// the fixtures above and whose every invocation is written down. The
    /// `#[ignore]` is what keeps it out of an ordinary run: without the shims
    /// on `PATH` it would be asking this machine about its actual workspaces.
    #[tokio::test]
    #[ignore = "run by `probe::record` from the two tests below, under recording shims"]
    async fn survey_live_probe() {
        if !crate::probe::is_child() {
            return;
        }
        let said = match survey_live().await {
            Some(found) => {
                let (reclaimable, liveness) = found.into_parts();
                let claim = reclaimable.map_or_else(
                    || "nothing reclaimable".to_string(),
                    |found| found.hint(120),
                );
                format!(
                    "{claim} | running {} stalled {}",
                    liveness.running(),
                    liveness.stalled()
                )
            }
            None => "no reading at all".to_string(),
        };
        println!("{}{said}", crate::probe::MARK);
    }

    #[test]
    fn the_surfacing_path_runs_two_reads_and_nothing_else() {
        // #137's safety claim, as a fact about a run rather than a grep over
        // the source. `reap::workspaces` and `reap::node_facts` both reach a
        // PATH-resolved binary, so everything this path is *capable* of doing
        // to a workspace shows up here as argv — whatever it was spelt as in
        // Rust, in whichever module, and whether or not it named any word a
        // reader thought to forbid. A `dl <ws> rm` added anywhere between
        // `survey_live` and the reading it returns fails this test.
        let run = crate::probe::record(
            "reclaim::tests::survey_live_probe",
            crate::probe::DL_LISTING,
            crate::probe::GH_FACTS,
        );
        run.destroyed_nothing();
        assert_eq!(
            run.argv.len(),
            2,
            "one listing and one batched query, and nothing else: {:?}",
            run.argv
        );
        assert_eq!(
            run.argv[0], "dl <--ls> <--json>",
            "the only thing this path asks `dl` for is what exists"
        );
        assert!(
            run.argv[1].starts_with("gh <api> <graphql> <-F> <owner=blooop> <-F> <name=wayfinder>"),
            "one batched read of the tracker: {}",
            run.argv[1]
        );
    }

    #[test]
    fn the_reading_over_a_real_dl_and_gh_says_what_a_reap_would_claim() {
        // The other half of the same run: not just that it destroyed nothing,
        // but that it did the work. A `survey_live` that answered `None`, or a
        // reading that never reached the sentence, leaves this with nothing to
        // match — which is the point, because "surfaces nothing, ever" is the
        // failure mode a guard on capability alone would call a pass.
        let run = crate::probe::record(
            "reclaim::tests::survey_live_probe",
            crate::probe::DL_LISTING,
            crate::probe::GH_FACTS,
        );
        assert_eq!(
            run.printed(),
            ["· 1 reclaimable: wf-129-closed (+1 to check by hand) — wf reap | running 1 stalled 1"],
            "the live reading is the same sentence the offline tests pin"
        );
    }

    #[test]
    fn the_surfacing_module_names_no_means_of_destruction() {
        // The structural half, which the run above cannot cover: a subprocess
        // is observable, and `std::fs::remove_dir_all` is not. So argv is
        // watched at run time and the means of destruction that leave no argv
        // are pinned here — this module reads two functions and formats a
        // sentence, and has no business naming any of these.
        //
        // A fact about this file's text, and named as one: it says nothing
        // about what a function it calls in another file may do. That is what
        // the recorded run above is for, within the bounds that run states.
        //
        // `unsafe` used to be on this list and is not: `unsafe_code = "deny"`
        // in `Cargo.toml` covers every target in the crate, so a copy here was
        // a second, weaker statement of a rule the compiler already enforces.
        //
        // `fs` bare rather than `fs::`, matching `picker.rs` and
        // [`crate::refresh`]: written `fs::`, the token is defeated by
        // `use std::fs as sys;`, which is a one-line edit that a reviewer used
        // to reopen an escape this PR had already closed once. The bare name
        // catches both spellings and costs nothing — `fs` occurs nowhere in
        // this file's code. What the bare name adds here is narrower than it
        // first looks, and the measurement is worth stating precisely: an
        // aliased `remove_dir_all` is caught either way, because `remove` is
        // already on this list — two reviewers measured that row at different
        // sites and got 356/1 and 355/2, the difference being how many guards
        // fire, not whether one does. The bare name earns its place on the
        // calls `remove` does not name: an aliased `fs::write` truncating a
        // file is red at `fs` and **fully green** at `fs::`. Note this is a
        // substring match, so it also forbids `offset`, `refs` and `prefs`
        // here — see the same note in [`crate::picker`].
        let code = crate::probe::code_only("reclaim.rs", include_str!("reclaim.rs"));
        for forbidden in ["remove", "\"rm\"", "--force", "Command", "process::", "fs"] {
            assert!(
                !code.contains(forbidden),
                "the surfacing path must not be able to delete: it names {forbidden:?}"
            );
        }
        // And the one call it *does* make into reap's planning is never the
        // insisting one — the waiver stays with the human reading the plan.
        // Read as "whatever the last argument is, it is `false`" rather than as
        // the whole call written out: renaming a parameter is not a change to
        // this claim, and a test that broke on one would be a test about
        // spelling.
        let insist = code
            .split("reap::plan(")
            .nth(1)
            .and_then(|call| call.split(')').next())
            .and_then(|args| args.rsplit(',').next())
            .map(str::trim);
        assert_eq!(
            insist,
            Some("false"),
            "the reading must plan without insisting"
        );
        // #129's single definition, pinned at the one site that could quietly
        // grow a second one. A `matches!(v, Verdict::Reap { .. })` here would
        // agree with `doomed` on the day it was written — so every behavioural
        // test in this file passes over it — and drift the first time `doomed`
        // changed. Nothing but source text can see a duplicate that agrees.
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
