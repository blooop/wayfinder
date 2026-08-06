//! The wayfinder ticket model: maps, tickets, and their derived status.
//!
//! Status is derived, never stored (per the wayfinder model):
//! closed = done; open + assigned = claimed; open + unassigned with open
//! blockers = blocked; otherwise frontier.

use serde::{Deserialize, Serialize};

/// The identity of one map: the repo it lives in and its map issue number.
///
/// A repo can hold several open maps at once (#50), so the slug alone stopped
/// being an identity — every place that used to say "this repo's map" (the
/// projects cache, the loaders, the failure set, the clusters on screen) now
/// says *which* map, and a second map on one repo is an ordinary value instead
/// of the one the lowest-number rule silently hid.
///
/// `Ord` is (repo, number), which is also the on-screen cluster order.
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

impl Status {
    /// The state glyph shown at the start of every row.
    pub fn glyph(&self) -> char {
        match self {
            Status::Frontier => '○',
            Status::Claimed => '◐',
            Status::Blocked { .. } => '⊘',
            Status::Done => '●',
        }
    }

    /// Position of this status within the cluster header's counts
    /// (frontier / claimed / blocked / done).
    pub fn group(&self) -> usize {
        match self {
            Status::Frontier => 0,
            Status::Claimed => 1,
            Status::Blocked { .. } => 2,
            Status::Done => 3,
        }
    }
}

/// What *kind* of work a ticket is — the `wayfinder:*` type label, parsed once
/// at the `gh` boundary ([`TicketType::from_labels`]) and never re-sniffed from
/// strings afterwards.
///
/// Total over the four types the skill defines **plus** [`TicketType::Untyped`],
/// so a ticket that carries no type label is an ordinary value rather than a
/// missing one. Every site that decides something from a type matches all five
/// arms with no wildcard, which is what makes a fifth `wayfinder:*` type a
/// compile error rather than a silent misreading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketType {
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
    /// Parse one label name. `None` for anything that is not a type label —
    /// the *only* wildcard match in the type's whole surface, and it belongs
    /// here because a label string genuinely is an open domain: any repo can
    /// carry `bug`, `enhancement`, or a `wayfinder:*` label invented after this
    /// binary shipped.
    pub fn from_label(label: &str) -> Option<TicketType> {
        match label.trim() {
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
    /// representable input and needs a rule. It resolves by
    /// [`TicketType::precedence`], HITL-first, so an ambiguous ticket never
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
            // Never returned by `from_label`, so this rank is only a
            // total-ordering formality — and last, so a real type always wins.
            TicketType::Untyped => 4,
        }
    }
}

/// One pull request linked to a ticket — GitHub's Development-panel link set
/// (closing keywords and manual links, mentions excluded; the #49 resolution),
/// shown as a `⇄` badge on the ticket's row. Evidence of progress, never a row
/// of its own (#47).
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrStatus {
    Draft,
    Open { checks: Checks, review: Review },
    Merged,
    Closed,
}

/// The check rollup on an open PR. `Absent` is its own meaning — no checks
/// configured — parsed from the *nullable* `statusCheckRollup` (#49), not a
/// stand-in for "unknown".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checks {
    Absent,
    Pending,
    Passing,
    Failing,
}

/// The review decision on an open PR. A null `reviewDecision` means no review
/// is required (#49) — `NotRequired`, a settled state — where `Required` is a
/// review asked for and not yet given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Review {
    NotRequired,
    Required,
    Approved,
    ChangesRequested,
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

/// One map's cluster: the map issue plus its sub-issue tickets.
///
/// Deliberately *without* its own [`MapId`]: a map is always held under its id
/// (in the clusters the screen renders, in a load event), so carrying a second
/// copy would let the two disagree.
#[derive(Debug, Clone)]
pub struct Map {
    /// Title of the map issue itself.
    pub title: String,
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

    /// Ticket counts by status group (frontier / claimed / blocked / done) —
    /// the cluster header's `○n ◐n ⊘n ●n`.
    pub fn counts(&self) -> [usize; 4] {
        let mut counts = [0; 4];
        for t in &self.tickets {
            counts[t.status.group()] += 1;
        }
        counts
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
    fn map_ids_order_by_repo_then_number_which_is_cluster_order() {
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
    fn counts_tally_the_four_status_groups() {
        let mut done = ticket(2, false, vec![]);
        done.status = Status::Done;
        let mut claimed = ticket(9, true, vec![]);
        claimed.status = Status::Claimed;
        let mut blocked = ticket(7, true, vec![6]);
        blocked.status = Status::Blocked { needs: vec![6] };
        let map = Map {
            title: "Map: wf".to_string(),
            tickets: vec![ticket(6, true, vec![]), claimed, blocked, done],
        };
        assert_eq!(map.counts(), [1, 1, 1, 1]);
    }
}
