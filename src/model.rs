//! The wayfinder ticket model: maps, tickets, and their derived status.
//!
//! Status is derived, never stored (per the wayfinder model):
//! closed = done; open + assigned = claimed; open + unassigned with open
//! blockers = blocked; otherwise frontier.

use serde::{Deserialize, Serialize};

/// Every variant of a sum type, one value per variant, with the compiler
/// holding the list complete.
///
/// The single variant list feeds both a wildcard-free `match` and the
/// returned `Vec`, so the proof and the iteration cannot disagree: a variant
/// missing from the list is a non-exhaustive match naming it, a variant
/// listed twice is an unreachable pattern (a warning CI denies), and there is
/// no way to satisfy the compiler without also entering the iteration. That
/// is what lets the launch matrix and the doc-vocabulary guards iterate *the
/// type* rather than a restatement of it (#133) — a hand-written array beside
/// a wildcard-free `match` reintroduces exactly the drift the `match`
/// removed: a probe variant compiled and greened while never being launched
/// and never being required of the docs.
///
/// A variant that carries data names one representative value after `=>` —
/// the `match` still covers its arm by pattern, the representative is checked
/// at run time to be the arm it stands for, and a caller that needs the full
/// payload grid expands it itself.
macro_rules! every_variant {
    ($ty:ident: $($variant:ident $(=> $value:expr)?),+ $(,)?) => {{
        let _list_is_complete = |value: &$ty| match value {
            $($ty::$variant { .. } => ()),+
        };
        vec![$(crate::model::every_variant!(@one $ty, $variant $(, $value)?)),+]
    }};
    (@one $ty:ident, $variant:ident) => { $ty::$variant };
    (@one $ty:ident, $variant:ident, $value:expr) => {{
        let representative = $value;
        assert!(
            matches!(representative, $ty::$variant { .. }),
            concat!(
                "the representative must be ",
                stringify!($ty),
                "::",
                stringify!($variant)
            )
        );
        representative
    }};
}
pub(crate) use every_variant;

/// The identity of one map: the repo it lives in and its map issue number.
///
/// A repo can hold several open maps at once (#50), so the slug alone stopped
/// being an identity — every place that used to say "this repo's map" (the
/// projects cache, the loaders, the failure set, the clusters on screen) now
/// says *which* map, and a second map on one repo is an ordinary value instead
/// of the one the lowest-number rule silently hid.
///
/// `Ord` is (repo, number) — the stable tie-break under the cluster order
/// ([`crate::app::App::scoped_clusters`]), which leads on activity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MapId {
    /// Full repo slug (e.g. "blooop/wayfinder") — the full slug, never the
    /// short name, because a fork and its upstream share a short name.
    pub repo: String,
    /// The map issue's number in that repo.
    pub number: u64,
}

impl MapId {
    pub fn new(repo: impl Into<String>, number: u64) -> Self {
        Self {
            repo: repo.into(),
            number,
        }
    }

    /// The short repo name shown in the cluster header (the slug's name half:
    /// "blooop/wayfinder" → "wayfinder"). Display only — never an identity key.
    pub fn short_repo(&self) -> &str {
        self.repo.split('/').next_back().unwrap_or(&self.repo)
    }
}

/// The set of maps believed open — what the search answers with, what the
/// cache seeds, and what the loaders reconcile against.
pub type MapSet = std::collections::BTreeSet<MapId>;

/// Derived state of a ticket on a map. `Blocked` carries the open blockers
/// (`needs`) so a blocked ticket without its blockers is unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Frontier,
    Claimed,
    Blocked { needs: Vec<u64> },
    Done,
}

/// Where a node stands in its lifecycle — one five-value lattice for every
/// node (#61), **derived, never declared**: from its linked PRs when any are
/// open or merged, from its ticket state otherwise. Blocked is deliberately
/// not here — it is [`Status`], and the glyph column overrides with `⊘` while
/// the stage underneath stays whatever the derivation says.
///
/// `Ord` follows the lattice `ready → building → in review → needs attention
/// → done`, which is also the constant max-over-open-PRs order (needs
/// attention > in review > building): an open PR can only ever contribute the
/// middle three, so `max` never has to special-case the ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Ready,
    /// Building for build nodes, "in progress" for decision nodes — one slot,
    /// two vocabularies, same glyph.
    Building,
    InReview,
    NeedsAttention,
    Done,
}

/// What a row's leading glyph column shows (#62): the node's stage, unless
/// the node is blocked — `⊘` overrides, because a blocked node's stage is
/// unactionable until its blockers clear. One sum type rather than a char, so
/// the rollup counts and the row draw share meanings, not symbols.
///
/// `Ord` is display order — `○ ◐ ◍ ! ● ⊘` — which the derive gives for free:
/// stages in lattice order, the blocked override after them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RowGlyph {
    Stage(Stage),
    Blocked,
}

impl RowGlyph {
    /// The glyph column for one ticket: its stage, blocked overriding.
    pub fn of(ticket: &Ticket) -> RowGlyph {
        match ticket.status {
            Status::Blocked { .. } => RowGlyph::Blocked,
            Status::Frontier | Status::Claimed | Status::Done => {
                RowGlyph::Stage(stage(&ticket.prs, &ticket.status))
            }
        }
    }

    /// Tally a set of tickets by glyph: pairs in display order, one entry per
    /// ticket, and glyphs nothing is in left out entirely.
    ///
    /// The one place counts-by-glyph is computed. Cluster headers and rollups
    /// are the same question asked of different sets of tickets, so they are
    /// the same call — a second tally would be a second glyph vocabulary, which
    /// is exactly what #78 exists to remove.
    pub fn tally<'a>(tickets: impl IntoIterator<Item = &'a Ticket>) -> Vec<(RowGlyph, usize)> {
        let mut counts: std::collections::BTreeMap<RowGlyph, usize> =
            std::collections::BTreeMap::new();
        for ticket in tickets {
            *counts.entry(RowGlyph::of(ticket)).or_default() += 1;
        }
        counts.into_iter().collect()
    }

    /// The character drawn: `○` ready · `◐` building/in progress · `◍` in
    /// review · `!` needs attention · `●` done · `⊘` blocked.
    pub fn char(self) -> char {
        match self {
            RowGlyph::Stage(Stage::Ready) => '○',
            RowGlyph::Stage(Stage::Building) => '◐',
            RowGlyph::Stage(Stage::InReview) => '◍',
            RowGlyph::Stage(Stage::NeedsAttention) => '!',
            RowGlyph::Stage(Stage::Done) => '●',
            RowGlyph::Blocked => '⊘',
        }
    }
}

/// What one **open** PR says about its node — the fixed per-PR table (#61).
/// `None` for merged and closed PRs, which are not open and speak elsewhere
/// (merged: the done fallback; closed: nothing at all).
///
/// The two axes are read separately and **exhaustively** — every `Checks` and
/// every `Review` variant is named, and so is every [`Signal`], so adding a
/// variant to any of the three is a compile error rather than a new value
/// silently reading as "in review".
///
/// Never `Ready` and never `Done`: an open PR is evidence of work in flight,
/// so [`stage`]'s `max` over open PRs can only ever land in the middle three.
fn open_pr_stage(status: &PrStatus) -> Option<Stage> {
    match status {
        PrStatus::Draft => Some(Stage::Building),
        PrStatus::Open { checks, review } => {
            let from_checks = match checks {
                Checks::Failing => Signal::Red,
                Checks::Pending => Signal::Moving,
                // Settled, or none configured at all: nothing left to wait on.
                Checks::Passing | Checks::Absent => Signal::Settled,
            };
            let from_review = match review {
                Review::ChangesRequested => Signal::Red,
                // Approved-awaiting-merge included, and a review nobody is
                // required to give: the PR is still up for its look.
                Review::Approved | Review::Required | Review::NotRequired => Signal::Settled,
            };
            Some(match from_checks.max(from_review) {
                Signal::Red => Stage::NeedsAttention,
                Signal::Moving => Stage::Building,
                Signal::Settled => Stage::InReview,
            })
        }
        PrStatus::Merged | PrStatus::Closed => None,
    }
}

/// What one axis of an open PR contributes to its stage — the #61 table read
/// as two independent readings rather than a chain of `if`s.
///
/// `Ord` is the table's precedence, which is *not* the [`Stage`] lattice: a
/// red signal wins ("checks `Failing` **or** review `ChangesRequested` → needs
/// attention"), then work still moving ("draft, or checks `Pending` →
/// building"), and a settled axis speaks last ("otherwise open → in review").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Signal {
    Settled,
    Moving,
    Red,
}

/// Derive a node's stage from its linked PRs and its derived status (#61).
///
/// The node takes the **max over its open PRs** — a red PR anywhere makes the
/// node red; nothing is silently ignored. With no open PRs, any merged PR
/// means done, whatever the ticket still says; with no PR evidence at all it
/// falls through to ticket state: ready (open, unblocked, unclaimed) / in
/// progress (claimed) / done (closed). PR state dominates when present — a
/// prototype's PR counts.
pub fn stage(prs: &[PrLink], status: &Status) -> Stage {
    if let Some(live) = prs.iter().filter_map(|pr| open_pr_stage(&pr.status)).max() {
        return live;
    }
    if prs.iter().any(|pr| pr.status == PrStatus::Merged) {
        return Stage::Done;
    }
    match status {
        // A blocked node's work has not begun: ready underneath, with the `⊘`
        // override and the launch refusal both keyed off status, not stage.
        Status::Frontier | Status::Blocked { .. } => Stage::Ready,
        Status::Claimed => Stage::Building,
        Status::Done => Stage::Done,
    }
}

/// What *kind* of work a ticket is — the `wayfinder:*` type label, parsed once
/// at the `gh` boundary ([`TicketType::from_labels`]) and never re-sniffed from
/// strings afterwards.
///
/// Total over the five types the skill suite defines **plus**
/// [`TicketType::Untyped`], so a ticket that carries no type label is an
/// ordinary value rather than a missing one. Every site that decides something
/// from a type matches all six arms with no wildcard, which is what makes a
/// new `wayfinder:*` type a compile error rather than a silent misreading.
///
/// Serialized as its own snake-case name in the launch context (#124), so a
/// launched agent is told how to open without re-reading the issue's labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketType {
    /// `wayfinder:build` — the one execution type (#61): a ticket that is a
    /// build contract, worked by `/tdd` and `/review` across its stages rather
    /// than resolved in a decision session.
    Build,
    /// `wayfinder:research` — away-from-keyboard by definition: reading sources
    /// to surface a fact a decision waits on.
    Research,
    /// `wayfinder:task` — manual work that unblocks a decision. Genuinely
    /// either-way: the agent drives it alone where it can, and hands the human
    /// a checklist where it cannot.
    Task,
    /// `wayfinder:grilling` — HITL by definition; the agent never stands in for
    /// the human's side of it.
    Grilling,
    /// `wayfinder:prototype` — HITL by definition (someone has to look at it).
    Prototype,
    /// The ticket carries none of the four types `wf` knows. Covers both a
    /// ticket with no `wayfinder:*` label at all and one labelled with
    /// something newer than this binary — one meaning ("no recognised type"),
    /// not a sentinel standing in for several.
    Untyped,
}

impl TicketType {
    /// Every type, in declaration order, with the compiler holding the list
    /// complete — see [`every_variant`]. What the launch matrix and the doc
    /// vocabulary iterate, so a new type cannot exist unlaunched (#133).
    pub fn every() -> Vec<TicketType> {
        every_variant!(TicketType: Build, Research, Task, Grilling, Prototype, Untyped)
    }

    /// Parse one label name. `None` for anything that is not a type label —
    /// the *only* wildcard match in the type's whole surface, and it belongs
    /// here because a label string genuinely is an open domain: any repo can
    /// carry `bug`, `enhancement`, or a `wayfinder:*` label invented after this
    /// binary shipped.
    pub fn from_label(label: &str) -> Option<TicketType> {
        match label.trim() {
            "wayfinder:build" => Some(TicketType::Build),
            "wayfinder:research" => Some(TicketType::Research),
            "wayfinder:task" => Some(TicketType::Task),
            "wayfinder:grilling" => Some(TicketType::Grilling),
            "wayfinder:prototype" => Some(TicketType::Prototype),
            _ => None,
        }
    }

    /// Parse an issue's labels into its one type. Total: a ticket with no
    /// recognised label is [`TicketType::Untyped`].
    ///
    /// Labels are a *set*, so several type labels on one issue is a
    /// representable input and needs a rule. It resolves by `precedence`
    /// (private, just below), HITL-first, so an ambiguous ticket never
    /// reads as the kind you could walk away from:
    /// `wayfinder:research` + `wayfinder:task` is a `Task`.
    pub fn from_labels<'a, I: IntoIterator<Item = &'a str>>(labels: I) -> TicketType {
        labels
            .into_iter()
            .filter_map(TicketType::from_label)
            .min_by_key(|t| t.precedence())
            .unwrap_or(TicketType::Untyped)
    }

    /// The short name shown on a row's `[type]` suffix (#51). `None` for
    /// [`TicketType::Untyped`]: an untyped ticket shows nothing rather than a
    /// placeholder — the suffix exists to say what kind of session the ticket
    /// wants, and "untyped" answers a question nobody asked.
    pub fn short_name(self) -> Option<&'static str> {
        match self {
            TicketType::Build => Some("build"),
            TicketType::Research => Some("research"),
            TicketType::Task => Some("task"),
            TicketType::Grilling => Some("grilling"),
            TicketType::Prototype => Some("prototype"),
            TicketType::Untyped => None,
        }
    }

    /// Tie-break rank when an issue carries several type labels — lower wins.
    ///
    /// An exhaustive match rather than a `const` precedence array on purpose: a
    /// fifth variant left out of an array would make [`TicketType::from_labels`]
    /// unable to ever return it, which is precisely the silent mishandling
    /// exhaustiveness exists to prevent. Here the compiler demands the new type
    /// be ranked, and ranking it *is* deciding how much of a human it needs.
    fn precedence(self) -> u8 {
        match self {
            TicketType::Grilling => 0,
            TicketType::Prototype => 1,
            TicketType::Task => 2,
            TicketType::Research => 3,
            // The most autonomous type of all — its whole lifecycle runs
            // unattended — so every other real type outranks it.
            TicketType::Build => 4,
            // Never returned by `from_label`, so this rank is only a
            // total-ordering formality — and last, so a real type always wins.
            TicketType::Untyped => 5,
        }
    }
}

/// One pull request linked to a ticket — GitHub's Development-panel link set
/// (closing keywords and manual links, mentions excluded; the #49 resolution),
/// shown as a `⇄` badge on the ticket's row. Evidence of progress, never a row
/// of its own (#47).
///
/// Serialized straight into the launch context a picked ticket hands its agent
/// (#124) — which PR to diff is `wf-review`'s whole rediscovery cost — rather
/// than copied into a wire type beside this one, so there is nothing here for a
/// second declaration to drift from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrLink {
    /// Full slug of the repo the PR lives in. Links are cross-repo capable,
    /// so this may differ from the ticket's repo — the badge says so when it
    /// does.
    pub repo: String,
    pub number: u64,
    pub status: PrStatus,
}

impl PrLink {
    /// The short repo name (display only), for badges on cross-repo PRs.
    pub fn short_repo(&self) -> &str {
        self.repo.split('/').next_back().unwrap_or(&self.repo)
    }
}

/// Where a linked PR stands — `state` + `isDraft` parsed together at the `gh`
/// boundary, so "draft" is a state of its own rather than a flag to remember
/// to check. Only an open, ready PR carries the live signals: checks and
/// review are questions about something still in flight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrStatus {
    Draft,
    Open { checks: Checks, review: Review },
    Merged,
    Closed,
}

impl PrStatus {
    /// One value of every arm, compiler-complete ([`every_variant`]). `Open`
    /// carries data, so a representative stands for it; a caller after the
    /// full signal grid expands it over [`Checks::every`] × [`Review::every`].
    pub fn every_arm() -> Vec<PrStatus> {
        every_variant!(PrStatus:
            Draft,
            Open => PrStatus::Open {
                checks: Checks::Absent,
                review: Review::NotRequired,
            },
            Merged,
            Closed,
        )
    }
}

/// The check rollup on an open PR. `Absent` is its own meaning — no checks
/// configured — parsed from the *nullable* `statusCheckRollup` (#49), not a
/// stand-in for "unknown".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Checks {
    Absent,
    Pending,
    Passing,
    Failing,
}

impl Checks {
    /// Every rollup, compiler-complete ([`every_variant`]).
    pub fn every() -> Vec<Checks> {
        every_variant!(Checks: Absent, Pending, Passing, Failing)
    }
}

/// The review decision on an open PR. A null `reviewDecision` means no review
/// is required (#49) — `NotRequired`, a settled state — where `Required` is a
/// review asked for and not yet given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Review {
    NotRequired,
    Required,
    Approved,
    ChangesRequested,
}

impl Review {
    /// Every decision, compiler-complete ([`every_variant`]).
    pub fn every() -> Vec<Review> {
        every_variant!(Review: NotRequired, Required, Approved, ChangesRequested)
    }
}

/// One ticket (sub-issue) on a map.
#[derive(Debug, Clone)]
pub struct Ticket {
    /// Full repo slug (e.g. "blooop/wayfinder"). The *full* slug, not the
    /// short name, because it is the ticket's identity half — with several
    /// projects aggregated, a fork and its upstream share a short name, and
    /// keying on that would merge two distinct repos into one row identity.
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub status: Status,
    /// The `wayfinder:*` type, parsed from the issue's labels at fetch time.
    ///
    /// Named after the label vocabulary it carries rather than shortened to
    /// `kind`: on the tracker this is literally the ticket's *type*, and
    /// `ticket.ticket_type` matching `wayfinder:type/*` is worth more than
    /// avoiding the repeated word.
    #[allow(clippy::struct_field_names)]
    pub ticket_type: TicketType,
    /// Every issue this ticket is blocked by — the full DAG edge set, closed
    /// blockers included (#50). This is *structure*, where
    /// [`Status::Blocked::needs`] is *status*: `needs` is the open subset at
    /// fetch time, and both are parsed once from the same `gh` response rather
    /// than one being re-derived from the other. Reverse (unblocks) edges are
    /// derived locally by inversion ([`Map::unblocks`]); the numbers may name
    /// issues outside the map, which inversion simply never visits.
    pub blocked_by: Vec<u64>,
    /// The PRs linked to this ticket (#52), in the tracker's order.
    pub prs: Vec<PrLink>,
}

impl Ticket {
    /// The short repo name (the slug's name half: "blooop/wayfinder" →
    /// "wayfinder"). Display and fuzzy-match only — never an identity key.
    pub fn short_repo(&self) -> &str {
        self.repo.split('/').next_back().unwrap_or(&self.repo)
    }
}

/// When the tracker last saw the map issue, reduced to the only thing wf asks
/// of it: an order. Packed decimal `YYYYMMDDhhmmss`, so `Ord` is chronological
/// and there is no second representation of the same instant to disagree with.
///
/// Constructible only by [`Activity::parse`] — a value of this type is a
/// timestamp that parsed, never a string hoped to be one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Activity(u64);

impl Activity {
    /// Parse a tracker timestamp (`2026-08-06T12:34:56Z`, which is the only
    /// shape GitHub's GraphQL `DateTime` emits).
    ///
    /// `None` for anything else. That is deliberately not a fallback instant:
    /// an unrecognised format would have to be *guessed* into an order, and a
    /// guess here silently reshuffles the screen. "Activity unknown" is a
    /// meaning of its own, and [`crate::app::App::scoped_clusters`] sorts it
    /// last among live maps rather than pretending it is ancient or fresh.
    pub fn parse(stamp: &str) -> Option<Activity> {
        let (date, time) = stamp.strip_suffix('Z')?.split_once('T')?;
        let mut fields = date.split('-').chain(time.split(':'));
        let mut packed = 0u64;
        // Fixed widths, so every field below the year is < 100 and decimal
        // packing keeps the ordering chronological. The year's own `* 100` is
        // harmless: it is the first field, with nothing yet packed under it.
        for width in [4, 2, 2, 2, 2, 2] {
            let field = fields.next()?;
            if field.len() != width || !field.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            packed = packed * 100 + field.parse::<u64>().ok()?;
        }
        fields.next().is_none().then_some(Activity(packed))
    }
}

/// One map's cluster: the map issue plus its sub-issue tickets.
///
/// Deliberately *without* its own [`MapId`]: a map is always held under its id
/// (in the clusters the screen renders, in a load event), so carrying a second
/// copy would let the two disagree.
#[derive(Debug, Clone)]
pub struct Map {
    /// Title of the map issue itself.
    pub title: String,
    /// When the map issue was last touched, if the tracker's timestamp parsed
    /// ([`Activity`]) — the cluster sort key.
    pub last_activity: Option<Activity>,
    pub tickets: Vec<Ticket>,
}

impl Map {
    /// Where `number`'s ticket sits in `tickets` — the row-index half of a
    /// [`crate::app::Row`]. `None` for a number that is not on this map (a
    /// blocking edge may name any issue).
    pub fn index_of(&self, number: u64) -> Option<usize> {
        self.tickets.iter().position(|t| t.number == number)
    }

    /// The tickets this ticket unblocks — the reverse of [`Ticket::blocked_by`],
    /// derived by inversion over the map's own tickets (#50). Direct dependents
    /// only; edges pointing outside the map never show up here because the
    /// tickets that would carry them are not on this map.
    pub fn unblocks(&self, number: u64) -> Vec<u64> {
        self.tickets
            .iter()
            .filter(|t| t.blocked_by.contains(&number))
            .map(|t| t.number)
            .collect()
    }

    /// Whether any ticket on this map is still open — whether the map is work
    /// or history. A map whose every ticket is done is *finished*, and so is a
    /// map with no tickets at all: both have nothing left to do, which is the
    /// one question the cluster order asks (finished maps sort last).
    pub fn has_open_work(&self) -> bool {
        self.tickets
            .iter()
            .any(|t| !matches!(t.status, Status::Done))
    }

    /// The whole map tallied by glyph — the cluster header's counts (#78).
    /// Stages, through the same [`RowGlyph`] the rows are drawn from, so a
    /// ticket drawn `!` is counted under `!`.
    pub fn tally(&self) -> Vec<(RowGlyph, usize)> {
        RowGlyph::tally(&self.tickets)
    }
}

/// Derive a ticket's status from its raw tracker state.
///
/// `open_blockers` lists the numbers of *open* issues blocking this one;
/// closed blockers don't block (they are structure, kept on
/// [`Ticket::blocked_by`], not status).
pub fn classify(is_open: bool, is_assigned: bool, open_blockers: Vec<u64>) -> Status {
    if !is_open {
        Status::Done
    } else if is_assigned {
        Status::Claimed
    } else if !open_blockers.is_empty() {
        Status::Blocked {
            needs: open_blockers,
        }
    } else {
        Status::Frontier
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_is_done_even_if_assigned_or_blocked() {
        assert_eq!(classify(false, true, vec![7]), Status::Done);
        assert_eq!(classify(false, false, vec![]), Status::Done);
    }

    #[test]
    fn open_assigned_is_claimed_even_with_open_blockers() {
        assert_eq!(classify(true, true, vec![7]), Status::Claimed);
        assert_eq!(classify(true, true, vec![]), Status::Claimed);
    }

    #[test]
    fn open_unassigned_with_open_blockers_is_blocked() {
        assert_eq!(
            classify(true, false, vec![6, 9]),
            Status::Blocked { needs: vec![6, 9] }
        );
    }

    #[test]
    fn open_unassigned_unblocked_is_frontier() {
        assert_eq!(classify(true, false, vec![]), Status::Frontier);
    }

    #[test]
    fn each_wayfinder_type_label_parses_to_its_type() {
        assert_eq!(
            TicketType::from_labels(["wayfinder:research"]),
            TicketType::Research
        );
        assert_eq!(
            TicketType::from_labels(["wayfinder:task"]),
            TicketType::Task
        );
        assert_eq!(
            TicketType::from_labels(["wayfinder:grilling"]),
            TicketType::Grilling
        );
        assert_eq!(
            TicketType::from_labels(["wayfinder:prototype"]),
            TicketType::Prototype
        );
        // Order in the label list is irrelevant, and unrelated labels alongside
        // a type label do not disturb it.
        assert_eq!(
            TicketType::from_labels(["enhancement", "wayfinder:research", "good first issue"]),
            TicketType::Research
        );
    }

    #[test]
    fn the_build_type_label_parses_and_ranks_last_among_real_types() {
        // The vocabulary grows exactly one type (#61): `wayfinder:build`.
        assert_eq!(
            TicketType::from_labels(["wayfinder:build"]),
            TicketType::Build
        );
        assert_eq!(TicketType::Build.short_name(), Some("build"));
        // HITL-first still holds: build is the most autonomous type of all —
        // its whole lifecycle runs unattended — so every other real type wins
        // an ambiguous ticket, and build still beats having no type.
        for other in [
            "wayfinder:research",
            "wayfinder:task",
            "wayfinder:grilling",
            "wayfinder:prototype",
        ] {
            let both = TicketType::from_labels(["wayfinder:build", other]);
            assert_ne!(both, TicketType::Build, "build + {other}");
        }
        assert_eq!(
            TicketType::from_labels(["bug", "wayfinder:build"]),
            TicketType::Build
        );
    }

    #[test]
    fn no_recognised_label_is_untyped_not_a_guess() {
        // No labels at all.
        assert_eq!(
            TicketType::from_labels(Vec::<&str>::new()),
            TicketType::Untyped
        );
        // Labels, none of them types.
        assert_eq!(
            TicketType::from_labels(["bug", "documentation"]),
            TicketType::Untyped
        );
        // A `wayfinder:` label that is not a *type*: the map label itself, and
        // a type invented after this binary shipped.
        assert_eq!(
            TicketType::from_labels(["wayfinder:map"]),
            TicketType::Untyped
        );
        assert_eq!(
            TicketType::from_labels(["wayfinder:spike"]),
            TicketType::Untyped
        );
        // Near-misses are not fuzzy-matched: a type label is exact.
        assert_eq!(TicketType::from_labels(["research"]), TicketType::Untyped);
        assert_eq!(
            TicketType::from_labels(["Wayfinder:Research"]),
            TicketType::Untyped
        );
        assert_eq!(TicketType::from_label("wayfinder:research!"), None);
    }

    #[test]
    fn several_type_labels_resolve_hitl_first() {
        // The rule that matters: research + anything else is not research, so
        // an ambiguous ticket never reads as the kind nobody has to sit with.
        for other in [
            "wayfinder:task",
            "wayfinder:grilling",
            "wayfinder:prototype",
        ] {
            let both = TicketType::from_labels(["wayfinder:research", other]);
            assert_ne!(both, TicketType::Research, "research + {other}");
            // …and the answer does not depend on which label GitHub lists first.
            assert_eq!(both, TicketType::from_labels([other, "wayfinder:research"]));
        }
        assert_eq!(
            TicketType::from_labels(["wayfinder:research", "wayfinder:task"]),
            TicketType::Task
        );
        assert_eq!(
            TicketType::from_labels(["wayfinder:task", "wayfinder:grilling"]),
            TicketType::Grilling
        );
    }

    fn pr(status: PrStatus) -> PrLink {
        PrLink {
            repo: "blooop/wayfinder".to_string(),
            number: 90,
            status,
        }
    }

    fn open_pr(checks: Checks, review: Review) -> PrLink {
        pr(PrStatus::Open { checks, review })
    }

    #[test]
    fn each_open_pr_maps_to_a_stage_by_the_fixed_table() {
        // Row 1 of the #61 table: failing checks or a changes-requested review
        // → needs attention. Either signal alone is enough.
        assert_eq!(
            stage(
                &[open_pr(Checks::Failing, Review::NotRequired)],
                &Status::Frontier
            ),
            Stage::NeedsAttention
        );
        assert_eq!(
            stage(
                &[open_pr(Checks::Passing, Review::ChangesRequested)],
                &Status::Frontier
            ),
            Stage::NeedsAttention
        );
        // Row 2: a draft, or checks still pending → building.
        assert_eq!(
            stage(&[pr(PrStatus::Draft)], &Status::Frontier),
            Stage::Building
        );
        assert_eq!(
            stage(
                &[open_pr(Checks::Pending, Review::Required)],
                &Status::Frontier
            ),
            Stage::Building
        );
        // Row 3: otherwise open — approved-awaiting-merge included → in review.
        assert_eq!(
            stage(
                &[open_pr(Checks::Passing, Review::Approved)],
                &Status::Frontier
            ),
            Stage::InReview
        );
        assert_eq!(
            stage(
                &[open_pr(Checks::Absent, Review::NotRequired)],
                &Status::Frontier
            ),
            Stage::InReview
        );
        assert_eq!(
            stage(
                &[open_pr(Checks::Passing, Review::Required)],
                &Status::Frontier
            ),
            Stage::InReview
        );
    }

    #[test]
    fn the_two_axes_of_one_pr_settle_by_the_tables_own_precedence() {
        // Where the rows overlap, row 1 wins: a changes-requested review is
        // needs-attention even while the checks are still moving, or already
        // red. Not the `Stage` lattice — building outranks in review *within
        // one PR*, which is why the axes have their own order.
        assert_eq!(
            stage(
                &[open_pr(Checks::Pending, Review::ChangesRequested)],
                &Status::Frontier
            ),
            Stage::NeedsAttention
        );
        assert_eq!(
            stage(
                &[open_pr(Checks::Failing, Review::ChangesRequested)],
                &Status::Frontier
            ),
            Stage::NeedsAttention
        );
        // Row 2 over row 3: pending checks hold the node at building however
        // settled the review axis is.
        assert_eq!(
            stage(
                &[open_pr(Checks::Pending, Review::Approved)],
                &Status::Frontier
            ),
            Stage::Building
        );
        assert_eq!(
            stage(
                &[open_pr(Checks::Pending, Review::NotRequired)],
                &Status::Frontier
            ),
            Stage::Building
        );
    }

    #[test]
    fn an_open_pr_never_speaks_the_ends_of_the_lattice() {
        // The invariant the max-over-open-PRs rule rests on: an open PR is
        // work in flight, so it can only ever contribute the middle three.
        // Were one to yield `Done`, a single open PR would mark the node
        // finished; were one to yield `Ready`, `max` would silently drop it.
        let every_open = [PrStatus::Draft].into_iter().chain(
            [
                Checks::Absent,
                Checks::Passing,
                Checks::Pending,
                Checks::Failing,
            ]
            .into_iter()
            .flat_map(|checks| {
                [
                    Review::NotRequired,
                    Review::Required,
                    Review::Approved,
                    Review::ChangesRequested,
                ]
                .into_iter()
                .map(move |review| PrStatus::Open { checks, review })
            }),
        );
        for status in every_open {
            let derived = open_pr_stage(&status).expect("an open PR always says something");
            assert!(
                matches!(
                    derived,
                    Stage::Building | Stage::InReview | Stage::NeedsAttention
                ),
                "{status:?} gave {derived:?}"
            );
        }
        // And the two that are not open say nothing at all here.
        assert_eq!(open_pr_stage(&PrStatus::Merged), None);
        assert_eq!(open_pr_stage(&PrStatus::Closed), None);
    }

    #[test]
    fn the_node_takes_the_max_over_its_open_prs() {
        // The constant order: needs attention > in review > building. A red PR
        // anywhere makes the node red, whichever way the tracker lists them.
        let building = || pr(PrStatus::Draft);
        let in_review = || open_pr(Checks::Passing, Review::Approved);
        let attention = || open_pr(Checks::Failing, Review::NotRequired);
        assert_eq!(
            stage(&[building(), attention()], &Status::Frontier),
            Stage::NeedsAttention
        );
        assert_eq!(
            stage(&[attention(), building()], &Status::Frontier),
            Stage::NeedsAttention
        );
        assert_eq!(
            stage(&[building(), in_review()], &Status::Frontier),
            Stage::InReview
        );
        assert_eq!(
            stage(&[in_review(), attention()], &Status::Frontier),
            Stage::NeedsAttention
        );
        // A merged PR alongside an open one is not silently ignored either —
        // the open PR is the live signal and wins.
        assert_eq!(
            stage(&[pr(PrStatus::Merged), building()], &Status::Frontier),
            Stage::Building
        );
    }

    #[test]
    fn with_no_open_prs_a_merged_pr_means_done() {
        // The work landed: done, whatever the ticket itself still says.
        assert_eq!(
            stage(&[pr(PrStatus::Merged)], &Status::Frontier),
            Stage::Done
        );
        assert_eq!(
            stage(&[pr(PrStatus::Merged)], &Status::Claimed),
            Stage::Done
        );
        assert_eq!(stage(&[pr(PrStatus::Merged)], &Status::Done), Stage::Done);
    }

    #[test]
    fn a_closed_unmerged_pr_is_no_evidence_at_all() {
        // Abandoned PRs neither advance nor hold the node: fall through to
        // ticket state as if they were never linked.
        assert_eq!(
            stage(&[pr(PrStatus::Closed)], &Status::Frontier),
            Stage::Ready
        );
        assert_eq!(
            stage(&[pr(PrStatus::Closed)], &Status::Claimed),
            Stage::Building
        );
    }

    #[test]
    fn with_no_prs_stage_derives_from_ticket_state() {
        // The decision-node half of the lattice: ready (open, unblocked,
        // unclaimed) / in progress (claimed — the building slot) / done.
        assert_eq!(stage(&[], &Status::Frontier), Stage::Ready);
        assert_eq!(stage(&[], &Status::Claimed), Stage::Building);
        assert_eq!(stage(&[], &Status::Done), Stage::Done);
        // A blocked node's work has not begun, so its stage is ready — the
        // glyph column overrides with ⊘ and enter refuses it, but blocked is
        // status, not a sixth stage.
        assert_eq!(
            stage(&[], &Status::Blocked { needs: vec![7] }),
            Stage::Ready
        );
    }

    #[test]
    fn the_glyph_column_shows_the_stage_with_blocked_overriding() {
        // The five stage glyphs (#62)…
        assert_eq!(RowGlyph::Stage(Stage::Ready).char(), '○');
        assert_eq!(RowGlyph::Stage(Stage::Building).char(), '◐');
        assert_eq!(RowGlyph::Stage(Stage::InReview).char(), '◍');
        assert_eq!(RowGlyph::Stage(Stage::NeedsAttention).char(), '!');
        assert_eq!(RowGlyph::Stage(Stage::Done).char(), '●');
        // …and the blocked override: whatever the PRs say, a blocked node's
        // stage is unactionable and the column says so.
        assert_eq!(RowGlyph::Blocked.char(), '⊘');
        let mut blocked = Ticket {
            repo: "blooop/wayfinder".to_string(),
            number: 7,
            title: "t7".to_string(),
            status: Status::Blocked { needs: vec![6] },
            ticket_type: TicketType::Build,
            blocked_by: vec![6],
            prs: vec![open_pr(Checks::Failing, Review::NotRequired)],
        };
        assert_eq!(RowGlyph::of(&blocked), RowGlyph::Blocked);
        // The same ticket unblocked reads as its PR's stage.
        blocked.status = Status::Frontier;
        assert_eq!(
            RowGlyph::of(&blocked),
            RowGlyph::Stage(Stage::NeedsAttention)
        );
    }

    #[test]
    fn pr_state_dominates_ticket_state_when_present() {
        // Two derivation sources, one type — a prototype's PR counts (#61),
        // and even a *closed* ticket with an open PR reads as that PR's stage.
        assert_eq!(
            stage(&[open_pr(Checks::Passing, Review::Approved)], &Status::Done),
            Stage::InReview
        );
        assert_eq!(
            stage(
                &[open_pr(Checks::Failing, Review::NotRequired)],
                &Status::Claimed
            ),
            Stage::NeedsAttention
        );
    }

    #[test]
    fn short_repo_is_the_name_half_of_the_slug() {
        assert_eq!(MapId::new("blooop/wayfinder", 1).short_repo(), "wayfinder");
        let t = Ticket {
            repo: "blooop/wayfinder".to_string(),
            number: 1,
            title: "t".to_string(),
            status: Status::Frontier,
            ticket_type: TicketType::Task,
            blocked_by: vec![],
            prs: vec![],
        };
        assert_eq!(t.short_repo(), "wayfinder");
    }

    #[test]
    fn activity_orders_chronologically_across_every_field() {
        let stamps = [
            "2025-12-31T23:59:59Z",
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:01Z",
            "2026-01-01T00:01:00Z",
            "2026-01-01T01:00:00Z",
            "2026-01-02T00:00:00Z",
            "2026-02-01T00:00:00Z",
        ];
        let parsed: Vec<Activity> = stamps
            .iter()
            .map(|s| Activity::parse(s).expect(s))
            .collect();
        let mut sorted = parsed.clone();
        sorted.sort();
        assert_eq!(parsed, sorted, "already in chronological order");
        // …and the ordering is strict: no two distinct instants collide.
        sorted.dedup();
        assert_eq!(sorted.len(), stamps.len());
    }

    #[test]
    fn an_unrecognised_timestamp_is_no_instant_rather_than_a_guessed_one() {
        for junk in [
            "",
            "2026-08-06",                // date only
            "2026-08-06T12:34:56",       // no zone marker
            "2026-08-06T12:34:56+01:00", // an offset wf cannot pack
            "2026-08-06T12:34:56.123Z",  // fractional seconds
            "2026-8-6T12:34:56Z",        // unpadded fields
            "26-08-06T12:34:56Z",        // two-digit year
            "2026-08-06T12:34Z",         // no seconds
            "2026-08-06T12:34:56:78Z",   // a seventh field
            "yyyy-mm-ddThh:mm:ssZ",      // not digits
            "2026-08-06 12:34:56Z",      // space where the T belongs
        ] {
            assert_eq!(Activity::parse(junk), None, "{junk:?} must not parse");
        }
    }

    #[test]
    fn a_map_has_open_work_until_every_ticket_is_done() {
        let live = Map {
            title: "Map: wf".to_string(),
            last_activity: None,
            tickets: vec![ticket(2, false, vec![]), ticket(6, true, vec![])],
        };
        assert!(live.has_open_work());
        let finished = Map {
            title: "Map: wf".to_string(),
            last_activity: None,
            tickets: vec![ticket(2, false, vec![]), ticket(6, false, vec![])],
        };
        assert!(!finished.has_open_work(), "every ticket done is finished");
        // A map with no tickets has nothing left to do either — the same answer
        // to the same question, not a third case.
        let empty = Map {
            title: "Map: wf".to_string(),
            last_activity: None,
            tickets: vec![],
        };
        assert!(!empty.has_open_work());
    }

    #[test]
    fn map_ids_order_by_repo_then_number_which_is_the_cluster_tie_break() {
        let mut ids = vec![
            MapId::new("kinisi/zeta", 4),
            MapId::new("blooop/wayfinder", 47),
            MapId::new("blooop/wayfinder", 1),
        ];
        ids.sort();
        assert_eq!(
            ids,
            vec![
                MapId::new("blooop/wayfinder", 1),
                MapId::new("blooop/wayfinder", 47),
                MapId::new("kinisi/zeta", 4),
            ]
        );
    }

    fn ticket(number: u64, open: bool, blocked_by: Vec<u64>) -> Ticket {
        Ticket {
            repo: "blooop/wayfinder".to_string(),
            number,
            title: format!("t{number}"),
            status: classify(open, false, vec![]),
            ticket_type: TicketType::Task,
            blocked_by,
            prs: vec![],
        }
    }

    #[test]
    fn unblocks_is_the_local_inversion_of_the_full_edge_set() {
        // #50 → #51 and #50 → #52, with #48 closed but its edge kept: the DAG
        // survives the blocker closing, which is exactly why closed edges stay.
        let map = Map {
            title: "Map: selection view".to_string(),
            last_activity: None,
            tickets: vec![
                ticket(48, false, vec![]),
                ticket(50, true, vec![48]),
                ticket(51, true, vec![50]),
                ticket(52, true, vec![50]),
            ],
        };
        assert_eq!(map.unblocks(50), vec![51, 52]);
        assert_eq!(
            map.unblocks(48),
            vec![50],
            "closed blockers keep their edges"
        );
        assert_eq!(map.unblocks(52), Vec::<u64>::new());
        // An edge pointing outside the map inverts to nothing rather than
        // panicking or inventing a ticket.
        assert_eq!(map.unblocks(999), Vec::<u64>::new());
    }

    #[test]
    fn a_map_tallies_by_glyph_in_display_order() {
        let mut done = ticket(2, false, vec![]);
        done.status = Status::Done;
        let mut claimed = ticket(9, true, vec![]);
        claimed.status = Status::Claimed;
        let mut blocked = ticket(7, true, vec![6]);
        blocked.status = Status::Blocked { needs: vec![6] };
        let map = Map {
            title: "Map: wf".to_string(),
            last_activity: None,
            tickets: vec![ticket(6, true, vec![]), claimed, blocked, done],
        };
        assert_eq!(
            map.tally(),
            vec![
                (RowGlyph::Stage(Stage::Ready), 1),
                (RowGlyph::Stage(Stage::Building), 1),
                (RowGlyph::Stage(Stage::Done), 1),
                (RowGlyph::Blocked, 1),
            ]
        );
    }

    #[test]
    fn the_full_glyph_vocabulary_sorts_into_display_order() {
        // `RowGlyph`'s derived `Ord` *is* the display order — the cluster
        // header, shut-group rollups, and branch-root rollups all render
        // tallies in it — so reordering the `Stage` declaration silently
        // reorders the screen while the tallies elsewhere in the suite stay
        // green (#86). Sorting the whole vocabulary pins the sequence.
        let mut glyphs = [
            RowGlyph::Blocked,
            RowGlyph::Stage(Stage::Done),
            RowGlyph::Stage(Stage::NeedsAttention),
            RowGlyph::Stage(Stage::InReview),
            RowGlyph::Stage(Stage::Building),
            RowGlyph::Stage(Stage::Ready),
        ];
        glyphs.sort();
        let sequence: String = glyphs.iter().map(|g| g.char()).collect();
        assert_eq!(sequence, "○◐◍!●⊘");
    }
}
