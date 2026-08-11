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
//! to nodes by the branch `wf` itself minted, ask the tracker what has become
//! of those nodes, and hand the finished ones back to `dl`. Everything a
//! workspace holds that says "not yet" — unsaved work, a running container — is
//! `dl`'s fact, read here rather than argued with.
//!
//! "Finished" is not `wf`'s own invention either. It is exactly what the stage
//! lattice already calls Done — a closed ticket, or an open one whose PR merged
//! with nothing still in flight — read off the same fields, through the same
//! per-PR interpretation, as the badge on the screen. Reap claims that end of
//! the lattice and only warns at the other: a ticket every PR of which closed
//! unmerged, and a ticket nobody claimed that nothing came of, are named and
//! left alone. A suspicion is worth printing and never worth acting on, which
//! is why [`Verdict::Warn`] exists and why [`doomed`] is the single place that
//! decides what actually goes.
//!
//! The *deciding* is all here; the *noticing* is [`reclaim`](crate::reclaim),
//! which calls [`plan`] and [`doomed`] behind the picker so `wf` can say what a
//! reap would claim without being asked (#137). It adds no deletion path — it
//! reads these two functions and renders a sentence, and everything that
//! destroys anything is still reached only by a human typing `wf reap`.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use tokio::process::Command;

/// Re-exported because `node_fact` takes one: the reading itself belongs to
/// [`fetch`](crate::fetch), where the screen reads it too.
pub use crate::fetch::TicketState;
use crate::fetch::{parse_pr, Assignee, GraphQlResponse, Nodes, PrNode};
use crate::launch::devlaunch_answers_unsaved;
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
    /// What deleting would destroy, in `dl`'s words.
    ///
    /// `None` is "no answer": the field was `null` or absent, and what that
    /// means depends on which `dl` wrote it. On devlaunch ≤ 0.0.23 `null` on a
    /// workspace `dl` made *is* the ordinary clean case. From 0.0.24 the same
    /// `null` appears exactly where `devlaunch` is false, so one on a clone
    /// `dl` made means `dl`'s own inspection fell over — and reading that as
    /// "clean" is devlaunch#171's bug, which is the bug the object form was a
    /// breaking change to prevent.
    ///
    /// No single row can tell the two apart, so the version is asked instead:
    /// `answered_where_dl_answers` turns the second reading into
    /// [`Unsaved::Unanswered`] before [`plan`] ever sees the row, and leaves
    /// the first alone. A `None` still standing here is therefore a `dl` whose
    /// version said the field was allowed to be empty, or one `wf` could not
    /// probe at all — and it permits a reap, which is what those releases meant
    /// by it.
    #[serde(default)]
    pub unsaved: Option<Unsaved>,
}

impl Workspace {
    /// Is devpod holding a container up for this workspace?
    ///
    /// One predicate, because two decisions turn on it — [`plan`] keeps a
    /// running workspace, and [`Liveness`](crate::liveness::Liveness) marks its
    /// node — and they were spelt as the same string literal in two files. A
    /// state `dl` renamed would then have made `reap` quietly stop protecting
    /// containers *and* the picker quietly call every claimed node stalled,
    /// with no test able to notice because both sides hardcoded the same word.
    pub fn is_running(&self) -> bool {
        self.state.as_deref() == Some("Running")
    }

    /// Is it *definitely* down — settled, with nothing of it running?
    ///
    /// Deliberately not `!is_running()`, and deliberately not `Stopped` alone
    /// either. devpod's vocabulary is `Running`, `Busy`, `Stopped` and
    /// `NotFound`, and `dl` writes `null` when `devpod status` will not answer:
    ///
    /// - `Stopped` and `NotFound` are both settled and both down. `NotFound` is
    ///   the *stronger* of the two — the container does not exist, having been
    ///   pruned or removed by hand — and it appears on ordinary machines; the
    ///   listing on the one this was written on carries a `NotFound` row.
    /// - `Busy` is a container mid-start or mid-stop, which is a transition and
    ///   not a fact about whether work is happening.
    /// - `null`, and any state a later devpod adds, are states `wf` cannot read.
    ///
    /// The last two answer `false` here *and* `false` to
    /// [`is_running`](Workspace::is_running), which is the point of having two
    /// predicates rather than one negation: a marking that asserts something on
    /// a row must be silent where it does not know, and only `!is_running()`
    /// would have quietly turned every unreadable state into a claim.
    pub fn is_down(&self) -> bool {
        matches!(self.state.as_deref(), Some("Stopped" | "NotFound"))
    }
}

/// What deleting a workspace's clone would destroy, as `dl` reports it.
///
/// Three arms because `dl` has three answers, and the third is not a flavour of
/// either other one: **it could not tell.** A clone whose `.git` is half-removed
/// or truncated is still full of files, and nothing has established that those
/// files exist anywhere else — so "could not read it" has to refuse a reap
/// exactly as "would lose work" does, and for a reason that reads differently
/// on the plan. `dl` learned that distinction the hard way (devlaunch#171,
/// where the guard walked out of the clone into an ancestor repository and
/// reported *its* cleanliness); `wf` is the caller that has to not re-flatten
/// it.
///
/// The old sentinel is what this replaces. `unsaved` was a string-or-null, and
/// null carried two unrelated meanings — "clean" and "not `dl`'s clone" — with
/// the reaper reading both as *go ahead*. The arm that says nothing is at risk
/// now says so in its own name, and the absence of an answer is the `Option`
/// around this type rather than a value inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsaved {
    /// Every byte of the clone exists on a remote too. Deleting costs nothing.
    NothingToLose,
    /// Uncommitted changes, unpushed commits, or both — named, not counted.
    WouldLose(String),
    /// `git` could not read the clone as a repository, and says why.
    CouldNotTell(String),
    /// `dl` answered something this `wf` does not understand, quoted back.
    ///
    /// Two things reach here: a key from a `dl` newer than this binary, and the
    /// documented `nothingToLose` carrying a payload it is not documented to
    /// carry. Both are `wf` failing to read `dl` rather than `dl` failing to
    /// read a clone, which is why the row's wording hedges at a newer `dl`
    /// rather than asserting one. It refuses a reap for the
    /// same reason [`Unsaved::CouldNotTell`] does — nothing has established
    /// that this clone exists anywhere else — and it is a separate arm because
    /// the two are separate facts: one is `git` failing on the clone, the other
    /// is `wf` failing on `dl`, and a row that said the first when it meant the
    /// second would send somebody to look at the wrong thing.
    Unrecognized(String),
    /// `dl` made this clone and then said nothing about what it holds.
    ///
    /// **Never parsed from the wire** — there is no spelling of it, because it
    /// is the *absence* of one. It is put here by `answered_where_dl_answers`
    /// after asking the `dl` on PATH its version, and only for a `dl` that
    /// documents an answer on every clone of its own. On such a release `null`
    /// appears exactly where `devlaunch` is false, so a `null` on a clone `dl`
    /// made is `dl`'s own inspection having fallen over — which is
    /// devlaunch#171's bug, the one the object form exists to prevent, and
    /// reaping on it would walk straight back into it.
    ///
    /// Separate from [`Unsaved::CouldNotTell`] because `dl` saying "I could not
    /// read this clone" and `dl` saying nothing at all are different failures
    /// with different places to look: the first is a broken clone, the second
    /// is a broken `dl`.
    Unanswered,
}

impl Unsaved {
    /// Why this workspace is kept, or `None` when it is no obstacle to a reap.
    ///
    /// The two refusing arms phrase themselves rather than sharing one
    /// "holds …" sentence: a clone `dl` could not read holds nothing anybody
    /// has established, and saying it *holds* something would be inventing the
    /// very reading that failed.
    fn refusal(&self) -> Option<String> {
        match self {
            Self::NothingToLose => None,
            Self::WouldLose(what) => Some(format!("holds {what}")),
            Self::CouldNotTell(why) => Some(format!("dl cannot read its clone: {why}")),
            Self::Unrecognized(said) => Some(format!(
                "this wf cannot read what dl said about it ({said}) — newer dl?"
            )),
            Self::Unanswered => {
                Some("dl made this clone but did not say what it holds".to_string())
            }
        }
    }

    /// What a `-f` row appends to say what the reap it advises throws away.
    fn discard(&self) -> Option<String> {
        match self {
            Self::NothingToLose => None,
            Self::WouldLose(what) => Some(format!(", discarding {what}")),
            Self::CouldNotTell(why) => Some(format!(", discarding a clone dl cannot read: {why}")),
            Self::Unrecognized(said) => Some(format!(
                ", discarding a clone this wf cannot read dl's answer about ({said})"
            )),
            Self::Unanswered => {
                Some(", discarding a clone dl did not say anything about".to_string())
            }
        }
    }
}

/// `dl`'s two spellings of the same field, both accepted.
///
/// Both are live on real machines: devlaunch through 0.0.23 emits a bare
/// sentence or `null`, and 0.0.24 emits the one-key object.
///
/// `wf` does now hold `dl` to a floor, but that floor governs what a *launch*
/// may ask of it, and reaping is the other direction — it reads a listing from
/// whichever `dl` is on `PATH`, including one too old to isolate with. A
/// machine can also have reaped its workspaces with an older `dl` and upgraded
/// since. Refusing to read the older spelling would strand exactly the
/// workspaces most in need of collecting.
#[derive(Deserialize)]
#[serde(untagged)]
enum UnsavedWire {
    /// devlaunch ≤ 0.0.23. A sentence there was written when `git` reported
    /// something; `wf` reads it as work at risk, which is the safe direction if
    /// that release ever wrote a sentence for another reason. The "clean" case
    /// was `null`, and stays [`None`] here.
    Sentence(String),
    /// devlaunch ≥ 0.0.24. Read as a **map**, not as an externally-tagged enum,
    /// and that is the whole design of this arm.
    ///
    /// An enum would require the object to hold exactly one key, so the day
    /// `dl` adds a sibling — a `path`, a `checkedAt`, a count — *every* row
    /// stops being readable at once. Every workspace then refuses, `wf reap`
    /// collects nothing, and the picker's hint disappears without a word.
    /// Refusing is the safe direction, but losing the whole feature to one
    /// added field is not degradation, and this arm's job is degradation.
    ///
    /// Reading the keys `wf` knows and ignoring the rest costs nothing and
    /// survives that release. What it cannot survive — a *renamed* or genuinely
    /// new answer — is what [`UnsavedWire::Unknown`] is for.
    Reported(serde_json::Map<String, serde_json::Value>),
    /// Anything else `dl` might one day put here.
    ///
    /// **This arm is why the field is a type and not a string.** Without it, a
    /// value `wf` cannot read makes the *whole listing* fail to parse —
    /// [`parse_workspaces`] is all or nothing, so one such row takes `wf reap`
    /// and the picker's reading down with it, silently in the picker's case.
    /// That is the exact failure this field already caused once, and pinning
    /// the spellings that have shipped would only have moved the next one a
    /// release away. The value is kept rather than ignored so the refusal can
    /// name what it did not understand.
    Unknown(serde_json::Value),
}

/// The keys `dl` documents, in the order they are read.
///
/// Order is load-bearing, and only in one direction: the two refusing keys are
/// looked for **before** the permitting one, so an object carrying both — which
/// `dl` should never write — refuses rather than reaps. Nothing else about the
/// order matters, because the arms are mutually exclusive in practice.
const WOULD_LOSE: &str = "wouldLose";
const COULD_NOT_TELL: &str = "couldNotTell";
const NOTHING_TO_LOSE: &str = "nothingToLose";

/// How much of an unrecognised value to quote back. Enough to recognise, short
/// enough to sit in a plan row beside a workspace id.
const UNKNOWN_QUOTED: usize = 60;

impl<'de> Deserialize<'de> for Unsaved {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match UnsavedWire::deserialize(deserializer)? {
            UnsavedWire::Sentence(what) => Self::WouldLose(what),
            UnsavedWire::Reported(object) => Self::from_object(&object),
            UnsavedWire::Unknown(value) => Self::unrecognized(&value),
        })
    }
}

impl Unsaved {
    /// Read the object form: the first documented key that answers, and a
    /// refusal when none does.
    fn from_object(object: &serde_json::Map<String, serde_json::Value>) -> Self {
        if let Some(what) = object.get(WOULD_LOSE).and_then(serde_json::Value::as_str) {
            return Self::WouldLose(what.to_string());
        }
        if let Some(why) = object
            .get(COULD_NOT_TELL)
            .and_then(serde_json::Value::as_str)
        {
            return Self::CouldNotTell(why.to_string());
        }
        // The one key that permits a delete, and the only value it may carry.
        // `false` is `dl` contradicting itself, and anything else is a payload
        // it does not document: both refuse rather than being read as clean.
        if object.get(NOTHING_TO_LOSE) == Some(&serde_json::Value::Bool(true)) {
            return Self::NothingToLose;
        }
        Self::unrecognized(&serde_json::Value::Object(object.clone()))
    }

    /// Name what could not be read, short enough to sit in a plan row.
    ///
    /// An object is reported by its **keys**, because the key is what `wf`
    /// failed to recognise and a reader is going to compare it against `dl`'s
    /// own documentation. Anything else is rendered whole. Either way this is
    /// `wf`'s re-rendering rather than `dl`'s bytes back — `serde_json` sorts
    /// an object's keys and normalises its numbers — which is why the row says
    /// `dl` said something *this wf* cannot read, and does not offer the text
    /// as a quotation to grep for.
    fn unrecognized(value: &serde_json::Value) -> Self {
        let said = match value {
            serde_json::Value::Object(object) if !object.is_empty() => {
                object.keys().cloned().collect::<Vec<_>>().join(", ")
            }
            other => other.to_string(),
        };
        let mut quoted: String = said.chars().take(UNKNOWN_QUOTED).collect();
        if said.chars().count() > UNKNOWN_QUOTED {
            quoted.push('…');
        }
        Self::Unrecognized(quoted)
    }
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
/// row the human is about to approve. `InFlight` covers open, draft — and
/// unreadable; see [`PrOutcome::project`] for why that third one belongs there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrOutcome {
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
/// describes. `Unstarted` in particular is never reachable from a lookup that
/// *failed*: a node the batch did not answer for is an error, not an
/// `Unstarted` node.
///
/// It is a reading of an answer that arrived, then — not quite the "positive
/// observation" this said before [`TicketState`] existed. A ticket whose state
/// this binary cannot read, with no PRs and nobody assigned, lands here too,
/// because `Live` is where an unrecognised state deliberately goes and this is
/// where a live ticket with no other evidence ends up. That is the intended
/// resolution and not a gap — every alternative is worse. An arm of its own
/// would owe an answer at every match site for a case that has never occurred;
/// the deleting arm is the hazard #132 exists to close; and `Warn` is where a
/// node this binary is *unsure* about belongs. The cost is that the row says
/// "unclaimed and no PR" about a ticket that might be closed, which overstates
/// what was observed by one word — and it is a row that advises rather than
/// acts, which is the arm that can afford to be wrong.
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
pub fn node_fact(state: TicketState, is_assigned: bool, prs: &[PrOutcome]) -> NodeFact {
    if state == TicketState::Closed {
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

impl Node {
    /// How a node is named to a human — `wayfinder#133`.
    ///
    /// One spelling, shared by the reap plan's rows and the picker's stall
    /// segment, because they are naming the same things on the same terminal
    /// and two formats would read as two kinds of object.
    pub fn name(&self) -> String {
        format!("{}#{}", short_repo(&self.repo), self.number)
    }
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
/// `known` is what the tracker said about each node — gathered once,
/// [`node_facts`], because that is one network call per repo rather than one
/// per workspace. A node appears once however many workspaces name it: the fact
/// is the node's, and the guards below are each workspace's own.
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
///
/// The `Warn` rows never delete, which is why they do not borrow the reap
/// row's "discarding" cadence: nothing is discarded on a row `wf` will not act
/// on. Under `-f` they still name the unsaved work, because the reap they are
/// advising would throw it away — the discard belongs to the by-hand reap, and
/// the sentence says so.
pub fn plan(
    workspaces: &[Workspace],
    known: &BTreeMap<Node, NodeFact>,
    insist: bool,
) -> Vec<Verdict> {
    let mut verdicts = Vec::new();
    for workspace in workspaces {
        let Some(node) = node_of(workspace) else {
            // Not one of ours. Not reported either: a listing full of "not
            // mine" lines buries the two rows that matter.
            continue;
        };
        let id = workspace.id.clone();
        let name = node.name();
        // Naming the waived work in the acted-on line, not only in the keep
        // line it replaced: this is the row the human is about to approve, and
        // "and discarding …" is the part they might stop at.
        let discard = match (&workspace.unsaved, insist) {
            (Some(unsaved), true) => unsaved.discard().unwrap_or_default(),
            _ => String::new(),
        };
        if let (Some(unsaved), false) = (&workspace.unsaved, insist) {
            if let Some(refusal) = unsaved.refusal() {
                verdicts.push(Verdict::Keep {
                    id,
                    reason: refusal,
                });
                continue;
            }
        }
        if workspace.is_running() {
            verdicts.push(Verdict::Keep {
                id,
                reason: "still running — stop it first".to_string(),
            });
            continue;
        }
        let Some(fact) = known.get(&node) else {
            // Unreachable while the fetch stays never-partial, and deliberately
            // not an `Unstarted`: a question that went unanswered is not an
            // observation about the node.
            verdicts.push(Verdict::Keep {
                id,
                reason: format!("no tracker answer for {name}"),
            });
            continue;
        };
        verdicts.push(match fact {
            NodeFact::Closed => Verdict::Reap {
                id,
                reason: format!("{name} is closed{discard}"),
            },
            NodeFact::DoneByMerge { pr } => Verdict::Reap {
                id,
                reason: format!("{name} open but its PR #{pr} merged{discard}"),
            },
            NodeFact::Superseded { pr } => Verdict::Warn {
                id,
                reason: format!(
                    "{name}'s PR #{pr} closed unmerged — superseded? reap by hand if so{discard}"
                ),
            },
            NodeFact::Unstarted => Verdict::Warn {
                id,
                reason: format!(
                    "{name} unclaimed and no PR — an abandoned stage? reap by hand if so{discard}"
                ),
            },
            NodeFact::InFlight { .. } | NodeFact::Claimed => Verdict::Keep {
                id,
                reason: format!("{name} is still open"),
            },
        });
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
    let mut workspaces = parse_workspaces(&output.stdout)?;
    answered_where_dl_answers(&mut workspaces, devlaunch_answers_unsaved());
    Ok(workspaces)
}

/// The parse boundary for `dl`'s listing, kept apart from the process call so
/// it is testable without devlaunch installed.
fn parse_workspaces(body: &[u8]) -> Result<Vec<Workspace>> {
    serde_json::from_slice(body).context("unparseable workspace listing from `dl --ls --json`")
}

/// Give a missing answer its meaning, once the `dl` that wrote it is known.
///
/// The one place a `dl` version reaches this module, and it is applied here —
/// at the boundary that already ran the process — so that [`plan`] stays a pure
/// function of rows it is handed. A version probe inside the parser or the
/// planner would make every test of either depend on what is installed on the
/// machine running it.
///
/// `answers` is [`devlaunch_answers_unsaved`] in production and both values in
/// the tests. When it is false nothing happens at all: on those releases `null`
/// on `dl`'s own clone is the ordinary clean case, and rewriting it would make
/// `wf reap` refuse every workspace on the machine.
///
/// Keyed on `devlaunch` rather than on [`node_of`] because that is the
/// distinction `dl` documents — `unsaved` is null exactly where `devlaunch` is
/// false — and reading a row `dl` disclaims as an unanswered question would
/// invent a failure out of a workspace that was never `dl`'s to inspect. Rows
/// that are `dl`'s but not `wf`'s are left marked anyway; [`plan`] drops them
/// for not being ours, and this function has no business knowing that.
fn answered_where_dl_answers(workspaces: &mut [Workspace], answers: bool) {
    if !answers {
        return;
    }
    for workspace in workspaces {
        if workspace.devlaunch && workspace.unsaved.is_none() {
            workspace.unsaved = Some(Unsaved::Unanswered);
        }
    }
}

/// What the tracker says about each of `nodes`.
///
/// **One `gh api graphql` call per repo**, and none per workspace: every wanted
/// number of a repo is aliased into the same query, so ten workspaces of one
/// repo cost one round trip. (The per-ticket REST loop this replaced cost one
/// each, which is what the old version of this comment claimed it did not.)
///
/// The selection is the reap-relevant subset of the map query's own — issue
/// state, `assignees`, and the linked-PR rollup — and the PR nodes are read
/// back through [`fetch`](crate::fetch)'s badge parse rather than re-read here,
/// so "done" means one thing across the screen and the reaper.
///
/// # Errors
///
/// A `gh` that is missing, unauthenticated or refused, and any node the batch
/// did not answer for. Never partial: an unanswered repo or issue fails the
/// whole call, because the alternative is treating "could not ask" as a fact
/// about the node — and both directions of that are wrong, one deleting a
/// workspace on no evidence and the other warning about one.
pub async fn node_facts(nodes: &BTreeSet<Node>) -> Result<BTreeMap<Node, NodeFact>> {
    let mut facts = BTreeMap::new();
    let repos: BTreeSet<&str> = nodes.iter().map(|n| n.repo.as_str()).collect();
    for repo in repos {
        let numbers: Vec<u64> = nodes
            .iter()
            .filter(|n| n.repo == repo)
            .map(|n| n.number)
            .collect();
        let (owner, name) = repo
            .split_once('/')
            .with_context(|| format!("malformed repo slug {repo:?}"))?;
        let output = Command::new("gh")
            .args([
                "api",
                "graphql",
                "-F",
                &format!("owner={owner}"),
                "-F",
                &format!("name={name}"),
                "-f",
                &format!("query={}", node_facts_query(&numbers)),
            ])
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .context("failed to run `gh` — is the GitHub CLI installed and on PATH?")?;
        if !output.status.success() {
            bail!(
                "could not read {repo}'s tickets: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        facts.extend(parse_node_facts(&output.stdout, repo, &numbers)?);
    }
    Ok(facts)
}

/// The GraphQL alias one node's answer comes back under. GraphQL has no
/// variable field aliases, so the numbers are written into the query text —
/// safe by type, since they are `u64` and nothing else can arrive here.
fn alias(number: u64) -> String {
    format!("i{number}")
}

/// What one node's answer has to carry: the reap-relevant subset of the map
/// query's own sub-issue selection, field for field. `assignees` costs nothing
/// extra here — it is already selected there, and `classify` already reads it
/// as the claim, which is what makes the claimed/unstarted split free.
const NODE_FACT_SELECTION: &str = "\
      state
      assignees(first: 5) { nodes { login } }
      closedByPullRequestsReferences(first: 5, includeClosedPrs: true) {
        nodes {
          number state isDraft reviewDecision
          statusCheckRollup { state }
          repository { nameWithOwner }
        }
      }";

/// One repo's batch: every wanted issue in one query.
fn node_facts_query(numbers: &[u64]) -> String {
    use std::fmt::Write;
    let mut query = String::from(
        "query($owner: String!, $name: String!) {\n  repository(owner: $owner, name: $name) {\n",
    );
    for &number in numbers {
        let _ = write!(
            query,
            "    {}: issue(number: {number}) {{\n{NODE_FACT_SELECTION}\n    }}\n",
            alias(number)
        );
    }
    query.push_str("  }\n}");
    query
}

#[derive(Deserialize)]
struct BatchData {
    /// Aliased issues, keyed by [`alias`]. A node the query asked about and
    /// this map has no live entry for is the never-partial case.
    repository: Option<BTreeMap<String, Option<IssueFacts>>>,
}

#[derive(Deserialize)]
struct IssueFacts {
    state: String,
    assignees: Nodes<Assignee>,
    /// Defaulted for the same reason the map query defaults it: a GitHub
    /// edition without the field reads as "no linked PRs" rather than failing.
    #[serde(rename = "closedByPullRequestsReferences", default)]
    closed_by_prs: Nodes<PrNode>,
}

/// The parse boundary for one repo's batch, kept apart from the process call so
/// it is testable without `gh`.
fn parse_node_facts(body: &[u8], repo: &str, numbers: &[u64]) -> Result<BTreeMap<Node, NodeFact>> {
    let resp: GraphQlResponse<BatchData> =
        serde_json::from_slice(body).context("unparseable GraphQL response from gh")?;
    if let Some(err) = resp.errors.first() {
        bail!("GraphQL error: {}", err.message);
    }
    let answered = resp
        .data
        .and_then(|d| d.repository)
        .with_context(|| format!("the tracker did not answer for {repo}"))?;
    let mut facts = BTreeMap::new();
    for &number in numbers {
        let issue = answered
            .get(&alias(number))
            .and_then(Option::as_ref)
            .with_context(|| format!("the tracker did not answer for {repo}#{number}"))?;
        // The badge reading is fetch's, projected rather than repeated: an
        // unreadable state yields no badge there and an in-flight PR here.
        let prs: Vec<PrOutcome> = issue
            .closed_by_prs
            .nodes
            .iter()
            .map(|pr| {
                let link = parse_pr(pr);
                PrOutcome::project(pr.number, link.as_ref().map(|l| &l.status))
            })
            .collect();
        facts.insert(
            Node {
                repo: repo.to_string(),
                number,
            },
            node_fact(
                TicketState::read(&issue.state),
                !issue.assignees.nodes.is_empty(),
                &prs,
            ),
        );
    }
    Ok(facts)
}

/// Hand one finished workspace back to `dl`.
///
/// **Private to this module, and that is the strongest part of #137's
/// separation.** The picker is a module of the *binary*; this is the library.
/// No line of the binary can name this function — not under an alias, not from
/// a submodule, not through a helper in another of its files — and the edit
/// that tries is a compile error rather than a thing a test has to notice.
/// Three review rounds each found a different way past a source-text denylist
/// over the picker's own file, and each of those routes ended here.
///
/// What privacy does *not* close is [`run`] below: it is public because `main`
/// must dispatch `wf reap`, it calls this, and the binary may name it from
/// anywhere. That half is guarded by greps over two of the binary's files and
/// by a recorded run of the picker — weaker things, and described as such where
/// they live.
///
/// The one caller is [`run`] below, which is `wf reap` — a human typing the
/// command, reading the plan and answering the prompt.
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
async fn remove(id: &str, insist: bool) -> Result<()> {
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

/// `wf reap`: remove the workspaces whose nodes the tracker calls finished.
///
/// The whole command, here rather than in the binary, because `remove` above
/// is private to this module and this is the only thing that may call it. What
/// `main.rs` keeps is the argv that chooses this — `wf reap [-y] [-f]` — and
/// nothing else; the picker, in the same crate as that argv, can no more reach
/// a deletion than it can reach a private function of a library it depends on,
/// which is exactly what it now is.
///
/// The division of labour is the one the launch already draws — `dl` owns the
/// containers, `wf` owns the tickets — so this asks `dl` what exists, asks the
/// tracker what has become of those nodes, prints the plan, and hands the
/// finished ones back to `dl`. No terminal is taken: this is a stream command
/// like `wf skills`, not a second TUI.
///
/// The plan is printed **before** the prompt and includes what is being kept,
/// because a workspace someone expected to go and that stayed is the thing they
/// most need told about, and a reason they disagree with ("still running" when
/// they thought they had stopped it) is only actionable while no is an answer.
///
/// `warn` rows are the same argument pointed the other way: workspaces `wf`
/// suspects are dead weight on evidence too weak to act on — a superseded
/// ticket, or a node nothing has come of. They are printed and never counted
/// into the prompt, because the only safe thing to do with a suspicion is say
/// it out loud.
///
/// # Errors
///
/// A listing or a tracker query that failed, an unreadable answer to the
/// prompt, or one or more workspaces `dl` would not remove.
pub async fn run(yes: bool, insist: bool) -> Result<()> {
    use std::fmt::Write;
    use std::io::Write as _;

    use crate::emit;

    let workspaces = workspaces().await?;
    let nodes: BTreeSet<Node> = workspaces.iter().filter_map(node_of).collect();
    if nodes.is_empty() {
        emit("no wayfinder workspaces on this machine — nothing to reap\n");
        return Ok(());
    }
    let known = node_facts(&nodes).await?;
    let verdicts = plan(&workspaces, &known, insist);

    // The deletion set is asked for rather than re-derived here: `doomed` is
    // the one definition of what goes, so a warning row cannot become a
    // deletion by way of a partition written twice.
    let going = doomed(&verdicts);
    // Grouped rather than in listing order, and in this order: what stays,
    // then what `wf` is uneasy about, then — last, immediately above the
    // prompt — what the y/N is actually about.
    let mut out = String::new();
    for label in ["keep", "warn", "reap"] {
        for verdict in &verdicts {
            let row = match verdict {
                Verdict::Keep { .. } => "keep",
                Verdict::Warn { .. } => "warn",
                Verdict::Reap { .. } => "reap",
            };
            if row == label {
                let _ = writeln!(out, "  {label}  {}  ({})", verdict.id(), verdict.reason());
            }
        }
    }
    emit(&out);
    if going.is_empty() {
        emit("nothing to reap\n");
        return Ok(());
    }

    if !yes {
        emit(&format!("\ndelete {} workspace(s)? [y/N] ", going.len()));
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .context("cannot read the answer")?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            emit("aborted\n");
            return Ok(());
        }
    }

    // One at a time, reporting each: `dl <ws> rm` tears down a container, and a
    // failure part-way through leaves a set the next run has to be able to make
    // sense of. Failures are collected rather than propagated at the first one,
    // so a single wedged workspace does not strand the rest.
    let mut failed = Vec::new();
    for verdict in &going {
        match remove(verdict.id(), insist).await {
            Ok(()) => emit(&format!("removed {}\n", verdict.id())),
            Err(e) => {
                emit(&format!("could not remove {}: {e}\n", verdict.id()));
                failed.push(verdict.id().to_string());
            }
        }
    }
    if !failed.is_empty() {
        bail!("{} workspace(s) could not be removed", failed.len());
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
            (
                // The rollup the table used to have no row for, and the one the
                // precedence rule is entirely about: a merge and a rejection on
                // the same node. Every other row has one kind of PR in it, so
                // every other row reads the same whichever of the two arms is
                // tried first. #132's second mutation — swapping them — lived
                // in that gap, and it is a live difference rather than a
                // cosmetic one: `DoneByMerge` reaps and `Superseded` only warns.
                "a merged PR beside a closed-unmerged one",
                vec![
                    PrOutcome::project(33, Some(&PrStatus::Merged)),
                    PrOutcome::project(43, Some(&PrStatus::Closed)),
                ],
                NodeFact::DoneByMerge { pr: 33 },
                NodeFact::DoneByMerge { pr: 33 },
            ),
        ];
        for (name, prs, claimed, unclaimed) in cases {
            assert_eq!(
                node_fact(TicketState::Live, true, &prs),
                claimed,
                "open, assigned, {name}"
            );
            assert_eq!(
                node_fact(TicketState::Live, false, &prs),
                unclaimed,
                "open, unassigned, {name}"
            );
            for assigned in [true, false] {
                assert_eq!(
                    node_fact(TicketState::Closed, assigned, &prs),
                    NodeFact::Closed,
                    "closed, assigned={assigned}, {name}"
                );
            }
        }
    }

    #[test]
    fn a_state_this_binary_cannot_read_is_decided_by_the_prs_and_the_claim() {
        // The reading itself belongs to `fetch`, where `only_the_word_closed_
        // reads_as_closed` pins it for the screen and the reaper together. What
        // is reap's own is what an unrecognised state then *derives to*, and
        // that is stated as literal facts rather than as equality with the
        // `TicketState::Live` call — the two are the same call, so an assertion
        // written that way holds under every possible mutation of `node_fact`
        // and pins nothing at all.
        //
        // Every arm the derivation has, none of them `Closed`: an unknown state
        // must fall all the way through to the reading an open ticket gets, or
        // it would need an arm of its own and every match site would owe it an
        // answer.
        let unknown = TicketState::read("TRANSFERRED");
        for (prs, claimed, unclaimed) in [
            (vec![], NodeFact::Claimed, NodeFact::Unstarted),
            (
                vec![PrOutcome::project(40, Some(&open_pr()))],
                NodeFact::InFlight { pr: 40 },
                NodeFact::InFlight { pr: 40 },
            ),
            (
                vec![PrOutcome::project(33, Some(&PrStatus::Merged))],
                NodeFact::DoneByMerge { pr: 33 },
                NodeFact::DoneByMerge { pr: 33 },
            ),
            (
                vec![PrOutcome::project(43, Some(&PrStatus::Closed))],
                NodeFact::Superseded { pr: 43 },
                NodeFact::Superseded { pr: 43 },
            ),
        ] {
            assert_eq!(node_fact(unknown, true, &prs), claimed, "assigned, {prs:?}");
            assert_eq!(
                node_fact(unknown, false, &prs),
                unclaimed,
                "unassigned, {prs:?}"
            );
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
            node_fact(TicketState::Live, false, &in_flight),
            NodeFact::InFlight { pr: 37 }
        );
        let merged = [
            PrOutcome::project(97, Some(&PrStatus::Merged)),
            PrOutcome::project(91, Some(&PrStatus::Merged)),
        ];
        assert_eq!(
            node_fact(TicketState::Live, false, &merged),
            NodeFact::DoneByMerge { pr: 91 }
        );
        let superseded = [
            PrOutcome::project(90, Some(&PrStatus::Closed)),
            PrOutcome::project(97, Some(&PrStatus::Closed)),
        ];
        assert_eq!(
            node_fact(TicketState::Live, false, &superseded),
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

    /// The fact map `plan` decides by, keyed the way the batch answers.
    fn facts<const N: usize>(entries: [(Node, NodeFact); N]) -> BTreeMap<Node, NodeFact> {
        BTreeMap::from(entries)
    }

    #[test]
    fn a_closed_ticket_is_reaped_and_an_open_one_is_kept() {
        // The regression pin: the verdict that existed before reap could read
        // anything but "closed", said in the same words.
        let listing = vec![
            workspace("done", "blooop/devlaunch", "wayfinder/devlaunch-80"),
            workspace("open", "blooop/devlaunch", "wayfinder/devlaunch-81"),
        ];
        let known = facts([
            (node("blooop/devlaunch", 80), NodeFact::Closed),
            (node("blooop/devlaunch", 81), NodeFact::InFlight { pr: 91 }),
        ]);
        let verdicts = plan(&listing, &known, false);
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
    fn an_open_ticket_whose_only_pr_merged_is_reaped_as_done_by_merge() {
        // What the stage lattice already calls Done: with nothing in flight, a
        // merge means done whatever the ticket still says. The row names both
        // halves, because the open ticket is the part a reader might stop at.
        let ws = workspace("done", "blooop/devlaunch", "wayfinder/devlaunch-80");
        let known = facts([(
            node("blooop/devlaunch", 80),
            NodeFact::DoneByMerge { pr: 97 },
        )]);
        assert_eq!(
            plan(&[ws], &known, false),
            vec![Verdict::Reap {
                id: "done".to_string(),
                reason: "devlaunch#80 open but its PR #97 merged".to_string()
            }]
        );
    }

    #[test]
    fn an_open_or_draft_pr_still_keeps_and_never_warns() {
        // The half of the old `Open` catch-all that is the least dead thing wf
        // manages: in-review is where review fixes happen.
        let ws = workspace("live", "blooop/devlaunch", "wayfinder/devlaunch-80");
        for pr in [40, 42] {
            let known = facts([(node("blooop/devlaunch", 80), NodeFact::InFlight { pr })]);
            assert_eq!(
                plan(std::slice::from_ref(&ws), &known, false),
                vec![Verdict::Keep {
                    id: "live".to_string(),
                    reason: "devlaunch#80 is still open".to_string()
                }]
            );
        }
    }

    #[test]
    fn all_prs_closed_unmerged_warns_and_never_lands_in_the_doomed_set() {
        // A closed unmerged PR is a human's "not this way", not "this branch is
        // disposable" — wf's model refuses to read it as evidence and reap must
        // not invent a stronger reading. So: named, never deleted.
        let ws = workspace("super", "blooop/devlaunch", "wayfinder/devlaunch-80");
        let known = facts([(
            node("blooop/devlaunch", 80),
            NodeFact::Superseded { pr: 97 },
        )]);
        let verdicts = plan(&[ws], &known, false);
        assert_eq!(
            verdicts,
            vec![Verdict::Warn {
                id: "super".to_string(),
                reason: "devlaunch#80's PR #97 closed unmerged — superseded? reap by hand if so"
                    .to_string()
            }]
        );
        assert!(doomed(&verdicts).is_empty());
    }

    #[test]
    fn an_unclaimed_node_with_no_pr_warns_instead_of_keeping_quietly() {
        // Nothing has come of this node: nobody took it up and nothing came
        // out. The row says only what was observed — never "prewarmed",
        // "never entered" or "never attached", none of which wf knows.
        let ws = workspace("ghost", "blooop/devlaunch", "wayfinder/devlaunch-80");
        let known = facts([(node("blooop/devlaunch", 80), NodeFact::Unstarted)]);
        let verdicts = plan(&[ws], &known, false);
        assert_eq!(
            verdicts,
            vec![Verdict::Warn {
                id: "ghost".to_string(),
                reason: "devlaunch#80 unclaimed and no PR — an abandoned stage? reap by hand if so"
                    .to_string()
            }]
        );
        for forbidden in ["prewarm", "never entered", "never attached"] {
            assert!(!verdicts[0].reason().contains(forbidden));
        }
    }

    #[test]
    fn a_claimed_node_with_no_pr_is_kept_silently() {
        // The same workspace and the same empty rollup, one assignee apart.
        // That bit is the whole decision, so it gets its own test: an explicit
        // claim is a person's statement of intent, and a claim left behind by
        // an agent that died is settled by the re-entry ritual, not by reap.
        let ws = workspace("claimed", "blooop/devlaunch", "wayfinder/devlaunch-80");
        let known = facts([(node("blooop/devlaunch", 80), NodeFact::Claimed)]);
        assert_eq!(
            plan(&[ws], &known, false),
            vec![Verdict::Keep {
                id: "claimed".to_string(),
                reason: "devlaunch#80 is still open".to_string()
            }]
        );
    }

    #[test]
    fn a_warned_workspace_never_reaches_the_doomed_set_even_with_y_and_f() {
        // The safety property of the whole Warn arm, and the reason the doomed
        // set has one definition instead of a partition at the call site: no
        // combination of flags turns a warning into a deletion. `-y` only skips
        // the prompt, so `-f` is the whole flag surface plan can even see.
        for fact in [NodeFact::Superseded { pr: 97 }, NodeFact::Unstarted] {
            let mut ws = workspace("warned", "blooop/devlaunch", "wayfinder/devlaunch-80");
            ws.unsaved = Some(Unsaved::WouldLose(
                "1 uncommitted change(s) (pixi.lock)".to_string(),
            ));
            let known = facts([(node("blooop/devlaunch", 80), fact)]);
            for insist in [false, true] {
                let verdicts = plan(std::slice::from_ref(&ws), &known, insist);
                assert!(
                    doomed(&verdicts).is_empty(),
                    "insist={insist} put a warned workspace in the doomed set"
                );
            }
        }
    }

    #[test]
    fn the_warning_rows_advise_a_reap_by_hand_rather_than_announcing_a_discard() {
        // A row that never deletes must not borrow the reap row's cadence:
        // nothing is being discarded here. Under -f the discard is real but it
        // belongs to the by-hand reap being advised, which is where the clause
        // sits — and the wording is the same for both warning facts.
        for fact in [NodeFact::Superseded { pr: 97 }, NodeFact::Unstarted] {
            let mut ws = workspace("warned", "blooop/devlaunch", "wayfinder/devlaunch-80");
            ws.unsaved = Some(Unsaved::WouldLose(
                "1 uncommitted change(s) (pixi.lock)".to_string(),
            ));
            let known = facts([(node("blooop/devlaunch", 80), fact)]);
            let quiet = plan(std::slice::from_ref(&ws), &known, false);
            assert!(!quiet[0].reason().contains("discarding"));
            let loud = plan(std::slice::from_ref(&ws), &known, true);
            assert!(loud[0]
                .reason()
                .ends_with("reap by hand if so, discarding 1 uncommitted change(s) (pixi.lock)"));
        }
    }

    #[test]
    fn the_lockfile_dirty_prewarm_shows_its_warning_only_under_f() {
        // A postCreateCommand that dirties a tracked lockfile hides the warning
        // behind the unsaved guard. That under-fire is the existing -f contract
        // doing its job, and the deliberate direction: the advisory row is the
        // one that can afford to be missed.
        let mut ws = workspace("ghost", "blooop/devlaunch", "wayfinder/devlaunch-80");
        ws.unsaved = Some(Unsaved::WouldLose(
            "1 uncommitted change(s) (pixi.lock)".to_string(),
        ));
        let known = facts([(node("blooop/devlaunch", 80), NodeFact::Unstarted)]);
        assert_eq!(
            plan(std::slice::from_ref(&ws), &known, false),
            vec![Verdict::Keep {
                id: "ghost".to_string(),
                reason: "holds 1 uncommitted change(s) (pixi.lock)".to_string()
            }]
        );
        assert_eq!(
            plan(&[ws], &known, true),
            vec![Verdict::Warn {
                id: "ghost".to_string(),
                reason: "devlaunch#80 unclaimed and no PR — an abandoned stage? \
                         reap by hand if so, discarding 1 uncommitted change(s) (pixi.lock)"
                    .to_string()
            }]
        );
    }

    #[test]
    fn a_running_prewarm_reads_as_running_first() {
        // A just-abandoned prewarm's container is Running — its own `dl up`
        // started it — so the guard wins the row with or without -f, and the
        // warning surfaces on the next run. That is the correct next action.
        let mut ws = workspace("ghost", "blooop/devlaunch", "wayfinder/devlaunch-80");
        ws.state = Some("Running".to_string());
        let known = facts([(node("blooop/devlaunch", 80), NodeFact::Unstarted)]);
        for insist in [false, true] {
            assert_eq!(
                plan(std::slice::from_ref(&ws), &known, insist),
                vec![Verdict::Keep {
                    id: "ghost".to_string(),
                    reason: "still running — stop it first".to_string()
                }]
            );
        }
    }

    #[test]
    fn done_by_merge_yields_to_unsaved_without_f_and_to_running_always() {
        // The new facts are a second way for the tracker to say "finished
        // enough", orthogonal to the guards and beneath them.
        let known = facts([(
            node("blooop/devlaunch", 80),
            NodeFact::DoneByMerge { pr: 97 },
        )]);
        let mut dirty = workspace("merged", "blooop/devlaunch", "wayfinder/devlaunch-80");
        dirty.unsaved = Some(Unsaved::WouldLose("2 uncommitted change(s)".to_string()));
        assert_eq!(
            plan(&[dirty], &known, false),
            vec![Verdict::Keep {
                id: "merged".to_string(),
                reason: "holds 2 uncommitted change(s)".to_string()
            }]
        );
        let mut running = workspace("merged", "blooop/devlaunch", "wayfinder/devlaunch-80");
        running.state = Some("Running".to_string());
        for insist in [false, true] {
            assert_eq!(
                plan(std::slice::from_ref(&running), &known, insist),
                vec![Verdict::Keep {
                    id: "merged".to_string(),
                    reason: "still running — stop it first".to_string()
                }]
            );
        }
    }

    #[test]
    fn insisting_on_done_by_merge_names_the_discard() {
        let mut ws = workspace("merged", "blooop/devlaunch", "wayfinder/devlaunch-80");
        ws.unsaved = Some(Unsaved::WouldLose(
            "1 uncommitted change(s) (pixi.lock)".to_string(),
        ));
        let known = facts([(
            node("blooop/devlaunch", 80),
            NodeFact::DoneByMerge { pr: 97 },
        )]);
        assert_eq!(
            plan(&[ws], &known, true),
            vec![Verdict::Reap {
                id: "merged".to_string(),
                reason: "devlaunch#80 open but its PR #97 merged, \
                         discarding 1 uncommitted change(s) (pixi.lock)"
                    .to_string()
            }]
        );
    }

    #[test]
    fn two_workspaces_of_one_node_share_the_fact_but_keep_their_own_guards() {
        // The fact is computed once per node and applied identically to every
        // workspace whose branch names it; the per-workspace guards are what
        // differentiate them.
        let mut running = workspace("live", "blooop/devlaunch", "wayfinder/devlaunch-80");
        running.state = Some("Running".to_string());
        let stopped = workspace("idle", "blooop/devlaunch", "wayfinder/devlaunch-80");
        let known = facts([(
            node("blooop/devlaunch", 80),
            NodeFact::DoneByMerge { pr: 97 },
        )]);
        assert_eq!(
            plan(&[running, stopped], &known, false),
            vec![
                Verdict::Keep {
                    id: "live".to_string(),
                    reason: "still running — stop it first".to_string()
                },
                Verdict::Reap {
                    id: "idle".to_string(),
                    reason: "devlaunch#80 open but its PR #97 merged".to_string()
                },
            ]
        );
    }

    #[test]
    fn a_node_the_batch_did_not_answer_for_is_never_acted_on() {
        // The fetch is never-partial, so this should be unreachable — but the
        // one shape it could degrade into is the sentinel this whole design
        // exists to refuse: a missing answer read as "nothing has come of it".
        // It is neither reaped nor warned about; it says the tracker went
        // unanswered, which is the truth.
        let ws = workspace("unknown", "blooop/devlaunch", "wayfinder/devlaunch-80");
        for insist in [false, true] {
            let verdicts = plan(std::slice::from_ref(&ws), &BTreeMap::new(), insist);
            assert_eq!(
                verdicts,
                vec![Verdict::Keep {
                    id: "unknown".to_string(),
                    reason: "no tracker answer for devlaunch#80".to_string()
                }]
            );
        }
    }

    #[test]
    fn unsaved_work_keeps_a_workspace_whose_ticket_is_closed() {
        // dl's fact, read rather than argued with: dl would refuse this delete,
        // and a caller that walks into a refusal it could have anticipated is
        // one that eventually learns to pass --force.
        let mut ws = workspace("done", "blooop/devlaunch", "wayfinder/devlaunch-80");
        ws.unsaved = Some(Unsaved::WouldLose("2 uncommitted change(s)".to_string()));
        let known = facts([(node("blooop/devlaunch", 80), NodeFact::Closed)]);
        let verdicts = plan(&[ws], &known, false);
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
        let known = facts([(node("blooop/devlaunch", 80), NodeFact::Closed)]);
        assert_eq!(
            plan(&[ws], &known, false),
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
        ws.unsaved = Some(Unsaved::WouldLose("1 unpushed commit(s)".to_string()));
        let known = facts([(node("blooop/devlaunch", 80), NodeFact::Closed)]);
        assert!(plan(&[ws], &known, false)[0].reason().contains("unpushed"));
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
        let known = facts([(node("blooop/devlaunch", 81), NodeFact::InFlight { pr: 91 })]);
        let verdicts = plan(&listing, &known, false);
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

    /// The two `dl` releases are read the same way, arm for arm.
    ///
    /// This is the assertion whose absence let the field change under `wf`:
    /// both sides documented `unsaved` in prose and neither ever executed the
    /// other's output, so the string→object change in devlaunch 0.0.24 was
    /// invisible until a listing failed to parse — which takes the reap *and*
    /// the picker's hint down together, since [`parse_workspaces`] is all or
    /// nothing.
    #[test]
    fn every_shape_dl_has_emitted_for_unsaved_is_read_back_as_the_arm_it_means() {
        let parsed = parse_workspaces(crate::probe::DL_LISTING_UNSAVED.as_bytes())
            .expect("dl's listing parses, in either release's shape");
        let read: Vec<(&str, Option<&Unsaved>)> = parsed
            .iter()
            .map(|w| (w.id.as_str(), w.unsaved.as_ref()))
            .collect();
        assert_eq!(
            read,
            vec![
                ("wf-1-clean", Some(&Unsaved::NothingToLose)),
                (
                    "wf-2-dirty",
                    Some(&Unsaved::WouldLose(
                        "2 uncommitted change(s) (pixi.lock, notes.md) and 1 unpushed commit(s)"
                            .to_string()
                    ))
                ),
                (
                    "wf-3-unreadable",
                    Some(&Unsaved::CouldNotTell(
                        "fatal: not a git repository".to_string()
                    ))
                ),
                // 0.0.23 and older wrote a sentence only when there was
                // something to lose, so it lands on the one arm it can mean.
                (
                    "wf-4-legacy-dirty",
                    Some(&Unsaved::WouldLose(
                        "1 uncommitted change(s) (pixi.lock)".to_string()
                    ))
                ),
                // `null` on either release: no answer, which is not the same
                // fact as `nothingToLose` and must not become it.
                ("wf-5-legacy-clean", None),
                // A key this wf has never heard of, and the one key that
                // permits a delete carrying a payload dl does not document.
                // Both land on the arm that refuses, and — the whole point —
                // neither stops the six rows around them from parsing.
                // Named by its key: that is the half `wf` failed to recognise
                // and the half a reader compares against dl's own docs.
                (
                    "wf-6-newer-dl",
                    Some(&Unsaved::Unrecognized("someAnswerFromALaterDl".to_string()))
                ),
                (
                    "wf-7-odd-payload",
                    Some(&Unsaved::Unrecognized("nothingToLose".to_string()))
                ),
                // A documented key beside an undocumented sibling: still read.
                // The day `dl` adds a field to this object, every row would
                // otherwise stop parsing at once and `wf reap` would collect
                // nothing at all.
                ("wf-8-sibling-key", Some(&Unsaved::NothingToLose)),
                ("not-ours", None),
            ]
        );
    }

    /// The failure this whole type exists to prevent, stated as a fact about
    /// the boundary rather than about one value.
    ///
    /// `unsaved` broke `wf` once by changing shape. Accepting the two shapes
    /// `dl` has shipped would have fixed that one incident and left the next
    /// one identical: [`parse_workspaces`] is all or nothing, so one row `wf`
    /// cannot read costs every row — `wf reap` aborts, and the picker's reading
    /// fails silently, taking the reclaim hint and every liveness marking with
    /// it and saying nothing on screen about why.
    #[test]
    fn an_answer_from_a_later_dl_costs_that_row_a_reap_and_costs_the_others_nothing() {
        let workspaces = parse_workspaces(crate::probe::DL_LISTING_UNSAVED.as_bytes())
            .expect("a field this wf cannot read must not take the listing down with it");
        assert_eq!(
            workspaces.len(),
            9,
            "every row survives one unreadable field"
        );

        let known: BTreeMap<Node, NodeFact> = (1..=8)
            .map(|n| (node("blooop/wayfinder", n), NodeFact::Closed))
            .collect();
        let verdicts = plan(&workspaces, &known, false);
        let kept: Vec<&str> = verdicts
            .iter()
            .filter_map(|v| match v {
                Verdict::Keep { id, reason } if id.starts_with("wf-6") => Some(reason.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            kept,
            vec!["this wf cannot read what dl said about it (someAnswerFromALaterDl) — newer dl?"],
            "the row says dl is ahead of this wf, not that the clone is broken"
        );
        // And the rows either side of it are decided on their own merits.
        let reaped: Vec<&str> = doomed(&verdicts).into_iter().map(Verdict::id).collect();
        assert_eq!(
            reaped,
            vec!["wf-1-clean", "wf-5-legacy-clean", "wf-8-sibling-key"]
        );
    }

    /// Only the arm that says the clone is clean lets a finished ticket go.
    ///
    /// The same `null`, read as the two opposite things it means either side of
    /// devlaunch 0.0.24.
    ///
    /// `wf-5-legacy-clean` is the row that carries the whole argument: on the
    /// release that wrote it, `null` on `dl`'s own clone is *clean* and it must
    /// reap, and on a release that answers every clone it made, the same `null`
    /// is `dl`'s inspection having fallen over and it must not. One row cannot
    /// say which, which is why the version is asked once and applied here.
    ///
    /// `not-ours` pins the other edge: `unsaved` is null on it under every
    /// release, because there is no clone of `dl`'s own there to inspect. It is
    /// never an unanswered question, and marking it as one would invent a
    /// failure out of a workspace `dl` explicitly disclaims.
    #[test]
    fn a_missing_answer_reads_as_clean_or_as_a_broken_dl_depending_on_which_dl_wrote_it() {
        let read = |answers| {
            let mut workspaces =
                parse_workspaces(crate::probe::DL_LISTING_UNSAVED.as_bytes()).unwrap();
            answered_where_dl_answers(&mut workspaces, answers);
            workspaces
        };
        let unsaved_of = |workspaces: &[Workspace], id: &str| {
            workspaces
                .iter()
                .find(|w| w.id == id)
                .expect("the fixture row")
                .unsaved
                .clone()
        };

        // A `dl` that documents an answer on every clone of its own.
        let answering = read(true);
        assert_eq!(
            unsaved_of(&answering, "wf-5-legacy-clean"),
            Some(Unsaved::Unanswered),
            "a clone dl made and said nothing about is a question it failed to answer"
        );
        assert_eq!(
            unsaved_of(&answering, "not-ours"),
            None,
            "a workspace dl did not make is not one it failed to answer for"
        );

        // A `dl` from before the field changed, or one `wf` could not probe.
        // Nothing is rewritten: on those releases the `null` meant clean, and
        // reading it as a failure would refuse every workspace on the machine.
        let legacy = read(false);
        assert_eq!(unsaved_of(&legacy, "wf-5-legacy-clean"), None);
        assert_eq!(unsaved_of(&legacy, "not-ours"), None);

        // Rows that did answer are untouched by either reading — the upgrade
        // fills a gap and never overwrites what `dl` actually said.
        for answers in [true, false] {
            let workspaces = read(answers);
            assert_eq!(
                unsaved_of(&workspaces, "wf-1-clean"),
                Some(Unsaved::NothingToLose)
            );
            assert_eq!(
                unsaved_of(&workspaces, "wf-3-unreadable"),
                Some(Unsaved::CouldNotTell("fatal: not a git repository".into()))
            );
        }

        // And the whole point, at the level the human sees: the same listing
        // reaps that workspace under one `dl` and refuses it under the other.
        let known: BTreeMap<Node, NodeFact> = (1..=5)
            .map(|n| (node("blooop/wayfinder", n), NodeFact::Closed))
            .collect();
        assert!(
            doomed(&plan(&legacy, &known, false))
                .into_iter()
                .any(|v| v.id() == "wf-5-legacy-clean"),
            "the release that meant clean by it still collects it"
        );
        let refused = plan(&answering, &known, false);
        assert!(
            !doomed(&refused)
                .into_iter()
                .any(|v| v.id() == "wf-5-legacy-clean"),
            "the release that promised an answer does not get a reap out of silence"
        );
        assert!(
            refused.iter().any(|v| matches!(
                v,
                Verdict::Keep { id, reason }
                    if id == "wf-5-legacy-clean"
                        && reason == "dl made this clone but did not say what it holds"
            )),
            "and it says which of dl and the clone is the thing to go and look at: {refused:?}"
        );
    }

    /// `couldNotTell` refusing is the point: those files are still on disk and
    /// nothing has established they exist anywhere else, so it is `wouldLose`'s
    /// neighbour and not `nothingToLose`'s. Flattening the three answers back
    /// onto "is it `Some`?" would reap the unreadable clone.
    #[test]
    fn a_clean_clone_reaps_and_an_unreadable_one_refuses_like_dirty_work_does() {
        let workspaces = parse_workspaces(crate::probe::DL_LISTING_UNSAVED.as_bytes()).unwrap();
        let known: BTreeMap<Node, NodeFact> = (1..=5)
            .map(|n| (node("blooop/wayfinder", n), NodeFact::Closed))
            .collect();

        let verdicts = plan(&workspaces, &known, false);
        let reaped: Vec<&str> = doomed(&verdicts).into_iter().map(Verdict::id).collect();
        assert_eq!(
            reaped,
            vec!["wf-1-clean", "wf-5-legacy-clean"],
            "a clean clone and an unanswered one go; nothing else does"
        );
        let refusals: Vec<&str> = verdicts
            .iter()
            .filter_map(|v| match v {
                Verdict::Keep { id, reason } if id.starts_with("wf-3") => Some(reason.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            refusals,
            vec!["dl cannot read its clone: fatal: not a git repository"],
            "the unreadable clone says so, rather than claiming to hold work"
        );

        // `-f` waives both refusing arms, because that is what it hands `dl`:
        // the flag exists so a lockfile every container dirties cannot make a
        // workspace unreapable forever, and `dl rm --force` overrides its own
        // guard in both cases too. What changes is that the row says which.
        let insisted = plan(&workspaces, &known, true);
        let claimed: Vec<&str> = doomed(&insisted).into_iter().map(Verdict::id).collect();
        assert_eq!(
            claimed,
            vec![
                "wf-1-clean",
                "wf-2-dirty",
                "wf-3-unreadable",
                "wf-4-legacy-dirty",
                "wf-5-legacy-clean"
            ]
        );
        let unreadable = insisted
            .iter()
            .find(|v| v.id().starts_with("wf-3"))
            .expect("the unreadable clone is planned");
        assert!(
            unreadable
                .reason()
                .contains("discarding a clone dl cannot read: fatal: not a git repository"),
            "a forced reap names what it cannot account for: {}",
            unreadable.reason()
        );
    }

    #[test]
    fn insisting_waives_the_unsaved_guard_and_says_what_it_is_discarding() {
        // The case this exists for: a devcontainer that installs packages in
        // its postCreateCommand leaves a tracked lockfile modified in every
        // workspace it builds, so without an override those are unreapable
        // forever. The reap line has to name what is being thrown away, since
        // that is the row being approved.
        let mut ws = workspace("done", "blooop/devlaunch", "wayfinder/devlaunch-80");
        ws.unsaved = Some(Unsaved::WouldLose(
            "1 uncommitted change(s) (pixi.lock)".to_string(),
        ));
        let known = facts([(node("blooop/devlaunch", 80), NodeFact::Closed)]);
        assert_eq!(
            plan(std::slice::from_ref(&ws), &known, true),
            vec![Verdict::Reap {
                id: "done".to_string(),
                reason: "devlaunch#80 is closed, discarding 1 uncommitted change(s) (pixi.lock)"
                    .to_string()
            }]
        );
        // And it is only ever a waiver of *that* guard: an open ticket someone
        // is working is still an open ticket.
        let open = facts([(node("blooop/devlaunch", 80), NodeFact::Claimed)]);
        assert!(matches!(
            plan(&[ws], &open, true).as_slice(),
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
        ws.unsaved = Some(Unsaved::WouldLose(
            "1 uncommitted change(s) (pixi.lock)".to_string(),
        ));
        let known = facts([(node("blooop/devlaunch", 80), NodeFact::Closed)]);
        assert_eq!(
            plan(&[ws], &known, true),
            vec![Verdict::Keep {
                id: "done".to_string(),
                reason: "still running — stop it first".to_string()
            }]
        );
    }

    /// One repo's batch, shaped exactly like the live one: seven aliased
    /// issues covering every arm the fact derivation can land on, plus the two
    /// answers #132 found nothing reading — a state this binary was never
    /// taught, and a node carrying both a merge and a rejection.
    const BATCH_RESPONSE: &str = r#"{"data": {"repository": {
        "i80": {"state": "OPEN", "assignees": {"nodes": []},
                "closedByPullRequestsReferences": {"nodes": [
                  {"number": 97, "state": "MERGED", "isDraft": false,
                   "reviewDecision": null, "statusCheckRollup": {"state": "SUCCESS"},
                   "repository": {"nameWithOwner": "blooop/devlaunch"}}]}},
        "i81": {"state": "OPEN", "assignees": {"nodes": [{"login": "blooop"}]},
                "closedByPullRequestsReferences": {"nodes": []}},
        "i82": {"state": "OPEN", "assignees": {"nodes": []},
                "closedByPullRequestsReferences": {"nodes": []}},
        "i83": {"state": "CLOSED", "assignees": {"nodes": []},
                "closedByPullRequestsReferences": {"nodes": []}},
        "i84": {"state": "OPEN", "assignees": {"nodes": []},
                "closedByPullRequestsReferences": {"nodes": [
                  {"number": 44, "state": "SOMETHING_NEW", "isDraft": false,
                   "reviewDecision": null, "statusCheckRollup": null,
                   "repository": {"nameWithOwner": "blooop/devlaunch"}}]}},
        "i85": {"state": "TRANSFERRED", "assignees": {"nodes": []},
                "closedByPullRequestsReferences": {"nodes": []}},
        "i86": {"state": "OPEN", "assignees": {"nodes": []},
                "closedByPullRequestsReferences": {"nodes": [
                  {"number": 33, "state": "MERGED", "isDraft": false,
                   "reviewDecision": null, "statusCheckRollup": {"state": "SUCCESS"},
                   "repository": {"nameWithOwner": "blooop/devlaunch"}},
                  {"number": 43, "state": "CLOSED", "isDraft": false,
                   "reviewDecision": null, "statusCheckRollup": null,
                   "repository": {"nameWithOwner": "blooop/devlaunch"}}]}}
    }}}"#;

    #[test]
    fn one_batch_answers_every_node_of_a_repo() {
        let facts = parse_node_facts(
            BATCH_RESPONSE.as_bytes(),
            "blooop/devlaunch",
            &[80, 81, 82, 83, 84, 85, 86],
        )
        .expect("the batch parses");
        assert_eq!(
            facts,
            BTreeMap::from([
                (
                    node("blooop/devlaunch", 80),
                    NodeFact::DoneByMerge { pr: 97 }
                ),
                (node("blooop/devlaunch", 81), NodeFact::Claimed),
                (node("blooop/devlaunch", 82), NodeFact::Unstarted),
                (node("blooop/devlaunch", 83), NodeFact::Closed),
                // The unknown PR state travels the whole way: it reached the
                // fact as a PR in flight rather than being dropped on the floor
                // and leaving #84 looking like a node nothing came of.
                (node("blooop/devlaunch", 84), NodeFact::InFlight { pr: 44 }),
                // A state this binary was never taught reads as the open
                // ticket it might be, not as a closed one.
                (node("blooop/devlaunch", 85), NodeFact::Unstarted),
                // A merge outranks the rejection beside it.
                (
                    node("blooop/devlaunch", 86),
                    NodeFact::DoneByMerge { pr: 33 }
                ),
            ])
        );
    }

    #[test]
    fn what_the_tracker_says_decides_deletion_all_the_way_from_the_wire() {
        // #132's two invisible mutations, asserted where they are dangerous
        // rather than where they are convenient. Every test above this one
        // hands `plan` a `NodeFact` that a test author wrote down, which is
        // precisely why both mutations survived: the derivation was covered and
        // the *reading* was not, so nothing connected a tracker answer to a
        // workspace being deleted.
        //
        // So this one starts at the bytes `gh` would print and ends at
        // `doomed`, the single definition of what gets destroyed. Under the
        // unknown-state hazard, #85 joins the deletion set on a word this
        // binary cannot read; under the swapped precedence, #86 leaves it
        // despite a merged PR. Both are one assertion here.
        let numbers = [80, 83, 84, 85, 86];
        let known = parse_node_facts(BATCH_RESPONSE.as_bytes(), "blooop/devlaunch", &numbers)
            .expect("the batch parses");
        let mut workspaces: Vec<Workspace> = numbers
            .iter()
            .map(|n| {
                workspace(
                    &format!("wf-{n}"),
                    "blooop/devlaunch",
                    &format!("wayfinder/devlaunch-{n}"),
                )
            })
            .collect();

        // `-f` too, because the question is what may be destroyed at all: a
        // hazard that only the unsaved-work guard was hiding is still a hazard
        // the moment someone types the flag that waives it.
        //
        // That means one of these has to *hold* unsaved work, or the flag is
        // read on no workspace and the second pass is the first one again:
        // `plan` consults `insist` only inside `match (&workspace.unsaved, …)`,
        // so with every clone clean both iterations compute the same list and
        // the loop is decoration. #85 carries it — the unreadable state, so the
        // waiver is being applied to precisely the node this test is about.
        let dirty = "1 uncommitted change(s) (pixi.lock)";
        workspaces[3].unsaved = Some(Unsaved::WouldLose(dirty.to_string()));

        for insist in [false, true] {
            let verdicts = plan(&workspaces, &known, insist);
            let reaped: Vec<&str> = doomed(&verdicts).into_iter().map(Verdict::id).collect();
            assert_eq!(
                reaped,
                vec!["wf-80", "wf-83", "wf-86"],
                "insist={insist}: the closed ticket and the two merged nodes go, \
                 and nothing else does"
            );
        }

        // And the flag really did reach the node, rather than the loop above
        // passing because nothing consulted it: under `-f` the dirty clone's
        // row names what a by-hand reap would discard, and under neither flag
        // it is the refusal instead. Either way #85 is not in the doomed set.
        let forced = plan(&workspaces, &known, true);
        let row = forced
            .iter()
            .find(|v| v.id() == "wf-85")
            .expect("the unreadable node is planned");
        assert!(
            row.reason().contains(dirty),
            "-f did not reach the unsaved arm: {}",
            row.reason()
        );
    }

    #[test]
    fn assignees_reach_the_reap_fact_from_the_same_batch() {
        // The one bit separating "nobody took this up" from "someone is on it
        // and has not opened a PR yet" — and without it the warning would fire
        // on every grilling ticket and every pre-PR build session. It rides in
        // on a call already being made: zero extra round trips.
        let facts = parse_node_facts(BATCH_RESPONSE.as_bytes(), "blooop/devlaunch", &[81, 82])
            .expect("parse");
        assert_eq!(facts[&node("blooop/devlaunch", 81)], NodeFact::Claimed);
        assert_eq!(facts[&node("blooop/devlaunch", 82)], NodeFact::Unstarted);
    }

    #[test]
    fn a_node_the_batch_did_not_answer_for_is_an_error_not_unstarted() {
        // The one sentinel this design could grow: a missing answer quietly
        // becoming "nothing has come of it". Half an answer is the one shape
        // reap must not act on, so the whole call fails instead.
        let missing = parse_node_facts(BATCH_RESPONSE.as_bytes(), "blooop/devlaunch", &[80, 99])
            .expect_err("an unanswered node fails the batch");
        assert!(missing.to_string().contains("99"), "{missing}");
        for body in [
            // The alias came back null: the number names no issue there.
            r#"{"data": {"repository": {"i80": null}}}"#,
            // The repo itself came back null.
            r#"{"data": {"repository": null}}"#,
            // An error instead of data — never read as "nothing is closed".
            r#"{"errors": [{"message": "Could not resolve to a Repository"}]}"#,
            "not json",
        ] {
            assert!(
                parse_node_facts(body.as_bytes(), "blooop/devlaunch", &[80]).is_err(),
                "{body} parsed as an answer"
            );
        }
    }

    #[test]
    fn the_batch_asks_one_question_per_node_and_asks_it_about_the_claim() {
        // The wire contract: every wanted node aliased into the same round
        // trip, and the same field subset the map query already reads — the
        // claim included, which is what makes the split free.
        let query = node_facts_query(&[80, 81]);
        assert!(query.contains("i80: issue(number: 80)"), "{query}");
        assert!(query.contains("i81: issue(number: 81)"), "{query}");
        assert!(query.contains("assignees(first: 5)"), "{query}");
        assert!(query.contains("includeClosedPrs: true"), "{query}");
    }

    /// Collapse every run of whitespace to one space, so two spellings of the
    /// same selection at two indentation depths compare equal.
    fn flattened(selection: &str) -> String {
        selection.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The `closedByPullRequestsReferences(…) { … }` block of a query, brace
    /// balanced out of it, flattened. `None` if the query has no such block.
    fn pr_selection(query: &str) -> Option<String> {
        let from = query.find("closedByPullRequestsReferences")?;
        let mut depth = 0usize;
        let mut seen_one = false;
        for (offset, ch) in query[from..].char_indices() {
            match ch {
                '{' => {
                    depth += 1;
                    seen_one = true;
                }
                '}' => {
                    // `checked_sub` rather than `-=`: on a `}` before any `{`
                    // — what a `closedByPullRequestsReferences` reduced to a
                    // leaf leaves behind, inside its enclosing block — a
                    // `usize` decrement panics with "attempt to subtract with
                    // overflow", and whoever caused that drift would get an
                    // arithmetic panic in a test helper instead of the message
                    // this helper exists to give them.
                    depth = depth
                        .checked_sub(1)
                        .unwrap_or_else(|| panic!("unbalanced braces in {query}"));
                    if depth == 0 {
                        return Some(flattened(&query[from..=from + offset]));
                    }
                }
                _ => {}
            }
        }
        assert!(!seen_one, "unbalanced braces in {query}");
        None
    }

    #[test]
    fn both_queries_ask_for_a_linked_pr_in_exactly_the_same_words() {
        // #132's third item. `fetch::parse_pr` and `fetch::PrNode` are one
        // vocabulary — reap projects the badge reading rather than repeating
        // it — but the *query text* that feeds them was copied, and a copy with
        // nothing holding it can lose a field silently. What goes wrong is
        // quiet in both directions: drop `statusCheckRollup` here and every
        // reap-side PR reads as having no checks configured, which is a badge
        // this side would never show; drop it there and the screen does the
        // same. Neither parse fails, because both fields are nullable with
        // meaning.
        //
        // Byte equality after flattening, not containment: containment would
        // let reap's copy shrink, which is exactly the drift being forbidden.
        let map = pr_selection(crate::fetch::MAP_QUERY).expect("the map query selects linked PRs");
        let batch = pr_selection(NODE_FACT_SELECTION).expect("the batch selects linked PRs");
        assert_eq!(
            batch, map,
            "reap's linked-PR selection has drifted from the map query's"
        );
    }

    /// A selection text as the tree of fields it is: each name mapped to what
    /// is selected under it, empty for a leaf. The shape a GraphQL answer to
    /// that selection has, which is what makes it a filter.
    #[derive(Default, Debug)]
    struct Selected(BTreeMap<String, Selected>);

    /// Read the subset of GraphQL selection syntax these two queries use:
    /// names, optional `(arguments)`, optional nested `{ blocks }`.
    fn parse_selection(text: &str) -> Selected {
        let mut root = Selected::default();
        let mut path: Vec<String> = Vec::new();
        let mut last: Option<String> = None;
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '{' => path.push(last.take().expect("a block follows the field it selects")),
                '}' => {
                    path.pop().expect("a block was opened before it closed");
                    last = None;
                }
                // Arguments say nothing about the shape of the answer.
                '(' => {
                    for skipped in chars.by_ref() {
                        if skipped == ')' {
                            break;
                        }
                    }
                }
                ch if ch.is_alphanumeric() || ch == '_' => {
                    let mut name = String::from(ch);
                    while chars
                        .peek()
                        .is_some_and(|c| c.is_alphanumeric() || *c == '_')
                    {
                        name.push(chars.next().expect("peeked"));
                    }
                    let mut at = &mut root;
                    for step in &path {
                        at = at.0.entry(step.clone()).or_default();
                    }
                    at.0.entry(name.clone()).or_default();
                    last = Some(name);
                }
                _ => {}
            }
        }
        assert!(path.is_empty(), "unbalanced braces in {text}");
        root
    }

    /// Drop from `value` every field `selection` did not ask for, at the depth
    /// it did not ask for it — the answer a server honouring that selection
    /// would have sent back.
    fn prune(value: &mut serde_json::Value, selection: &Selected) {
        match value {
            serde_json::Value::Object(fields) => {
                fields.retain(|name, _| selection.0.contains_key(name.as_str()));
                for (name, field) in fields.iter_mut() {
                    prune(field, &selection.0[name]);
                }
            }
            // A connection's `nodes` list: the selection under `nodes` applies
            // to each element rather than to the list.
            serde_json::Value::Array(items) => {
                for item in items {
                    prune(item, selection);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn the_linked_pr_selection_asks_for_every_field_the_badge_reads() {
        // The hole the two tests above leave between them. `both_queries_ask…`
        // catches a field dropped from *one* copy of the PR selection, and the
        // prune test below catches a field the reap parse cannot survive losing
        // — but the badge fields are neither. `PrOutcome::project` collapses
        // Draft, Open and unreadable into `InFlight`, so `statusCheckRollup`
        // and `reviewDecision` are invisible to any assertion made in
        // `NodeFact`s, and dropping either one from *both* queries at once left
        // all 388 tests green.
        //
        // What reads them is `fetch::parse_pr`, so that is what this asserts
        // against: a PR whose badge depends on every nullable field, pruned to
        // the selection, must still produce the same badge. Both fields are
        // nullable *with meaning* — absent is a real answer, not a parse error
        // — which is exactly why nothing else notices them going missing, and
        // what goes wrong is quiet: the screen draws every open PR as having
        // no checks configured, and the reviewer trusts a green CI that was
        // never asked about.
        const A_PR: &str = r#"{"number": 40, "state": "OPEN", "isDraft": false,
            "reviewDecision": "CHANGES_REQUESTED",
            "statusCheckRollup": {"state": "FAILURE"},
            "repository": {"nameWithOwner": "blooop/wayfinder"}}"#;
        let full: PrNode = serde_json::from_str(A_PR).expect("the fixture is a PR node");
        let badge = parse_pr(&full).expect("the fixture is a PR the badge parse can read");
        assert_eq!(
            badge.status,
            PrStatus::Open {
                checks: Checks::Failing,
                review: Review::ChangesRequested,
            },
            "the fixture has to depend on both nullable fields, or this proves nothing"
        );

        let asked = parse_selection(NODE_FACT_SELECTION);
        let pr_fields = &asked.0["closedByPullRequestsReferences"].0["nodes"];
        let mut answered: serde_json::Value =
            serde_json::from_str(A_PR).expect("the fixture is json");
        prune(&mut answered, pr_fields);
        let selected: PrNode = serde_json::from_value(answered)
            .expect("a PR carrying exactly the selected fields still parses");

        assert_eq!(
            parse_pr(&selected).map(|link| link.status),
            Some(badge.status),
            "the selection no longer asks for a field the badge is drawn from"
        );
    }

    #[test]
    fn the_batch_parse_reads_no_field_the_batch_forgot_to_ask_for() {
        // The other half of #132's items 2 and 3: gutting `NODE_FACT_SELECTION`
        // stayed green, because every fixture in this file is written by hand
        // and so carries fields the query may no longer be requesting. A test
        // whose input is independent of the query cannot notice the query
        // losing a field — it just keeps parsing a response the server would
        // never have sent.
        //
        // So the input is derived from the query instead: the fixture is
        // filtered down to the fields `NODE_FACT_SELECTION` actually names,
        // which is the answer a server honouring that selection would return.
        // Take `state` out of the selection and the issue arrives without one;
        // take the linked-PR block out and #80 arrives looking like a node
        // nothing came of. Both are this assertion going red.
        let asked = parse_selection(NODE_FACT_SELECTION);
        // The filter has to be by *path*, not by field name: `state` is a field
        // of the issue and also a field of `statusCheckRollup`, so a name-set
        // filter keeps the issue's `state` on the strength of the rollup's and
        // the one mutation that matters most goes unseen. Asserted rather than
        // commented, since it is the property the whole test rests on.
        for field in ["state", "assignees", "closedByPullRequestsReferences"] {
            assert!(
                asked.0.contains_key(field),
                "the selection parse did not read the issue's {field}: {asked:?}"
            );
        }
        assert!(asked.0["assignees"].0.contains_key("nodes"));
        // The property the filter rests on, and the one a *name*-keyed filter
        // would fail: `state` is a field of the issue and also a field of
        // `statusCheckRollup`, so the two must be at their own depths and the
        // rollup's must not be visible at the issue's. Asserted rather than
        // commented, since it is what makes dropping the issue's `state` — the
        // field that decides deletion — a red test.
        //
        // Presence and not an exact key set: this is a check on the parser, and
        // a legitimate new field in the selection (`title` for a better reason
        // line, `updatedAt` for staleness) should not fail with a message that
        // sends its author looking for a bug in `parse_selection`.
        let rollup = &asked.0["closedByPullRequestsReferences"].0["nodes"].0["statusCheckRollup"];
        assert!(rollup.0.contains_key("state"), "{asked:?}");

        let mut body: serde_json::Value =
            serde_json::from_str(BATCH_RESPONSE).expect("the fixture is json");
        let issues = body["data"]["repository"]
            .as_object_mut()
            .expect("the fixture answers for a repo");
        for issue in issues.values_mut() {
            prune(issue, &asked);
        }

        let numbers = [80, 81, 82, 83, 84, 85, 86];
        let from_the_selection =
            parse_node_facts(body.to_string().as_bytes(), "blooop/devlaunch", &numbers)
                .expect("a response carrying exactly the selected fields parses");
        let from_the_fixture =
            parse_node_facts(BATCH_RESPONSE.as_bytes(), "blooop/devlaunch", &numbers)
                .expect("the fixture parses");
        assert_eq!(
            from_the_selection, from_the_fixture,
            "the selection no longer asks for everything the parse reads"
        );
    }

    #[test]
    fn an_unreadable_listing_is_an_error_rather_than_an_empty_plan() {
        // Empty would mean "nothing to reap", which is indistinguishable from
        // success and would hide a broken or too-old `dl`.
        assert!(parse_workspaces(b"not json").is_err());
        assert!(parse_workspaces(b"").is_err());
    }

    #[test]
    fn the_readme_states_the_keep_rule_the_derivation_implements() {
        // #132's fourth item. The list of what reap keeps *always* named
        // "tickets someone has claimed" without qualification, and that is not
        // a rule this code has: the claim is read only after every PR arm has
        // declined, so a claimed ticket whose PR merged is `DoneByMerge` and is
        // collected. `AGENTS.md` makes the README part of `wf`'s interface, so
        // a keep rule that overstates what is kept is a defect of the same kind
        // as a wrong reason line — and a worse one to be wrong about, since it
        // is what someone reads before deciding to trust `wf reap` with a
        // machine.
        //
        // Two halves, and the pairing is the test: the derivation fact that
        // makes the qualification necessary, and the sentence that has to carry
        // it. The sentence is matched exactly, so rewording it fails here and
        // whoever rewords it re-reads the rule above before saying so again.
        let claimed_and_merged = [PrOutcome::project(33, Some(&PrStatus::Merged))];
        assert_eq!(
            node_fact(TicketState::Live, true, &claimed_and_merged),
            NodeFact::DoneByMerge { pr: 33 },
            "a claim does not outrank a merge"
        );
        assert_eq!(
            node_fact(TicketState::Live, true, &[]),
            NodeFact::Claimed,
            "and it does settle a ticket nothing has come of"
        );

        let always = include_str!("../README.md")
            .split_once("Kept, always:")
            .expect("the README lists what a reap keeps unconditionally")
            .1
            .split_once("\n\n")
            .expect("that list is one paragraph")
            .0;
        assert!(
            always.contains("tickets someone has claimed that no PR has come of"),
            "the keep list must qualify the claim it names: {always}"
        );
    }

    /// `wf reap -y` taken for real, in a child process whose `dl` and `gh` are
    /// the fixtures the parent hands it and whose every invocation is written
    /// down. The `#[ignore]` is what keeps it out of an ordinary run: without
    /// the shims on `PATH` this is `wf reap -y` against the machine running
    /// the suite, and it is the one path in this crate that deletes.
    ///
    /// `-y` and not `-f`: the prompt is a human's, so a recorded run has to
    /// skip it, but the unsaved-work waiver is a separate decision and the
    /// tests below say which of them they are asking about.
    #[tokio::test]
    #[ignore = "run by `probe::record` from the two tests below, under recording shims"]
    async fn reap_run_probe() {
        if !crate::probe::is_child() {
            return;
        }
        let said = match run(true, false).await {
            Ok(()) => "the reap finished".to_string(),
            Err(e) => format!("the reap failed: {e}"),
        };
        println!("{}{said}", crate::probe::MARK);
    }

    /// Every call of a recorded run that could destroy a workspace.
    ///
    /// The same four tokens [`probe::Recording::destroyed_nothing`] forbids,
    /// and for the same reason: what matters is not how the deletion was spelt
    /// in Rust but that *something* on this path reached for a destructive
    /// command. The reaping probe cannot use that assertion — one `<rm>` is
    /// the feature it exists to watch — so it uses this and then says exactly
    /// which calls it expected. Filtering on `<rm>` alone would have let a
    /// `devpod delete <id>` or a `dl <id> remove` through unremarked, which is
    /// precisely the substitution the recording exists to catch.
    fn destructive(run: &crate::probe::Recording) -> Vec<&str> {
        run.argv
            .iter()
            .filter(|call| {
                ["<rm>", "<--force>", "<remove>", "<delete>"]
                    .iter()
                    .any(|token| call.contains(token))
            })
            .map(String::as_str)
            .collect()
    }

    #[test]
    fn a_reap_hands_dl_the_workspaces_its_plan_doomed_and_no_others() {
        // The deleting path as a fact about a run. Every other test of this
        // module stops at a `Verdict`, which is a value in this process — and
        // a plan that is right about what should go is not the same claim as a
        // run that destroys only that. Between the two sits `run`: the loop
        // over `doomed`, the id it passes, and the flag it does or does not
        // add. This is the only place any of that is observed.
        //
        // The four workspaces in the listing are laid out as directories in the
        // child's scratch home, so `wf` naming the wrong one would show up
        // here as an id, and `wf` deleting one itself would show up as a
        // disturbed path.
        let run = crate::probe::record(
            "reap::tests::reap_run_probe",
            crate::probe::DL_LISTING,
            crate::probe::GH_FACTS,
        );
        assert_eq!(
            run.printed(),
            ["the reap finished"],
            "the run reached the end: {}",
            run.stdout
        );
        // Everything this run did that could destroy anything, as one list. A
        // single `dl <id> rm`, no `--force` beside it — `--force` is `dl`'s own
        // waiver of its uncommitted-work guard, and a `wf` that passed it
        // unasked would be overriding a guard belonging to the other program —
        // and no second spelling of a deletion anywhere.
        assert_eq!(
            destructive(&run),
            ["dl <wf-129-closed> <rm>"],
            "the closed ticket's workspace goes, alone and by id: {:?}",
            run.argv
        );
        run.touched_no_files();
    }

    #[test]
    fn a_ticket_state_this_binary_cannot_read_costs_no_workspace() {
        // #132's first item, at the far end: not "the derivation keeps it" but
        // "the run deletes nothing". The same fixtures as above with one word
        // changed — the state of the one ticket whose workspace the run above
        // removes — so the difference between the two recordings is exactly
        // the difference between `CLOSED` and a word this binary was never
        // taught. Before the fix this run removed `wf-129-closed` on the
        // strength of that word.
        let unreadable = crate::probe::GH_FACTS.replace(
            r#""i129":{"state":"CLOSED""#,
            r#""i129":{"state":"TRANSFERRED""#,
        );
        assert_ne!(
            unreadable,
            crate::probe::GH_FACTS,
            "the fixture no longer says what this test edits"
        );
        let run = crate::probe::record(
            "reap::tests::reap_run_probe",
            crate::probe::DL_LISTING,
            &unreadable,
        );
        run.destroyed_nothing();

        // Not `stdout.contains("nothing to reap")` on its own: `run`'s
        // no-workspaces-at-all early return prints "no wayfinder workspaces on
        // this machine — nothing to reap", which contains that phrase too, and
        // `destroyed_nothing` is trivially satisfied by a run that saw nothing.
        // So a `wf reap` gone blind to every workspace on the machine would
        // read as a pass here, which is the opposite of what this test is for.
        //
        // The plan rows say which it was. The workspace the sibling test above
        // removes has to appear, and appear as a row that keeps it.
        let rows: Vec<&str> = run
            .stdout
            .lines()
            .filter(|line| line.contains("wf-129-closed"))
            .collect();
        assert_eq!(
            rows,
            ["  warn  wf-129-closed  (wayfinder#129 unclaimed and no PR — an abandoned stage? reap by hand if so)"],
            "the run saw the workspace and kept it, rather than never seeing it: {}",
            run.stdout
        );
        assert!(
            run.stdout.contains("nothing to reap"),
            "and nothing was left to delete: {}",
            run.stdout
        );
    }
}
