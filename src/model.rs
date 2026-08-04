//! The wayfinder ticket model: tickets and their derived status.
//!
//! Status is derived, never stored (per the wayfinder model):
//! closed = done; open + assigned = claimed; open + unassigned with open
//! blockers = blocked; otherwise frontier.

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

    /// Position of this status's group on the main screen
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

/// Group headers, indexed by [`Status::group`].
pub const GROUP_LABELS: [&str; 4] = ["FRONTIER — ready to claim", "CLAIMED", "BLOCKED", "DONE"];

/// What *kind* of work a ticket is — the `wayfinder:*` type label, parsed once
/// at the `gh` boundary ([`TicketType::from_labels`]) and never re-sniffed from
/// strings afterwards.
///
/// Total over the four types the skill defines **plus** [`TicketType::Untyped`],
/// so a ticket that carries no type label is an ordinary value rather than a
/// missing one. Every site that decides something from a type matches all five
/// arms with no wildcard, which is what makes a fifth `wayfinder:*` type a
/// compile error instead of a silent "not auto-startable" (#19).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketType {
    /// `wayfinder:research` — AFK by definition, and the only type `wf` starts
    /// by itself (#18).
    Research,
    /// `wayfinder:task` — a build slice. Genuinely AFK-or-HITL, and therefore
    /// deliberately *not* auto-started: writing code and committing unattended
    /// is a judgement made on a keystroke now (`ctrl-a`), not on a label set
    /// weeks ago.
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
    /// [`TicketType::precedence`], HITL-first, so ambiguity can never *grant*
    /// auto-start: `wayfinder:research` + `wayfinder:task` is a `Task`.
    pub fn from_labels<'a, I: IntoIterator<Item = &'a str>>(labels: I) -> TicketType {
        labels
            .into_iter()
            .filter_map(TicketType::from_label)
            .min_by_key(|t| t.precedence())
            .unwrap_or(TicketType::Untyped)
    }

    /// Tie-break rank when an issue carries several type labels — lower wins.
    ///
    /// An exhaustive match rather than a `const` precedence array on purpose: a
    /// fifth variant left out of an array would make [`TicketType::from_labels`]
    /// unable to ever return it, which is precisely the silent mishandling
    /// exhaustiveness exists to prevent. Here the compiler demands the new type
    /// be ranked, and ranking it *is* deciding whether it suppresses auto-start.
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

    /// May `wf` start this kind of ticket's agent with nobody watching?
    ///
    /// Only `research` (#18). Listed arm by arm with no wildcard: this is the
    /// decision a new type must not slip past.
    pub fn auto_startable(self) -> bool {
        match self {
            TicketType::Research => true,
            TicketType::Task
            | TicketType::Grilling
            | TicketType::Prototype
            | TicketType::Untyped => false,
        }
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
    pub ticket_type: TicketType,
}

impl Ticket {
    /// The short repo name shown in the row's repo column (the slug's name
    /// half: "blooop/wayfinder" → "wayfinder"). Display only — never an
    /// identity key.
    pub fn short_repo(&self) -> &str {
        self.repo.split('/').next_back().unwrap_or(&self.repo)
    }
}

/// One project's map: the map issue plus its sub-issue tickets.
#[derive(Debug, Clone)]
pub struct Map {
    /// Full repo slug (e.g. "blooop/wayfinder"), shown in the header.
    pub repo: String,
    /// Title of the map issue itself.
    pub title: String,
    pub tickets: Vec<Ticket>,
}

/// Merge the latest map per repo (keyed by slug) into the single [`Map`]
/// the screen renders. Several checkouts of one repo share one entry here,
/// so each repo's tickets appear exactly once. Tickets sort by
/// (repo, number); the header names the one repo when there is one, or
/// counts projects otherwise.
pub fn merge_maps(maps: &std::collections::BTreeMap<String, Map>) -> Map {
    let repo = match maps.len() {
        0 => "no projects — run wf inside a checkout to register it".to_string(),
        1 => maps.keys().next().expect("len checked").clone(),
        n => format!("{n} projects"),
    };
    let mut tickets: Vec<Ticket> = maps.values().flat_map(|m| m.tickets.iter().cloned()).collect();
    tickets.sort_by(|a, b| (&a.repo, a.number).cmp(&(&b.repo, b.number)));
    Map {
        repo,
        title: "wf".to_string(),
        tickets,
    }
}

/// Derive a ticket's status from its raw tracker state.
///
/// `open_blockers` lists the numbers of *open* issues blocking this one;
/// closed blockers don't block.
pub fn classify(is_open: bool, is_assigned: bool, open_blockers: Vec<u64>) -> Status {
    if !is_open {
        Status::Done
    } else if is_assigned {
        Status::Claimed
    } else if !open_blockers.is_empty() {
        Status::Blocked { needs: open_blockers }
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

    fn map(slug: &str, numbers: &[u64]) -> Map {
        Map {
            repo: slug.to_string(),
            title: format!("Map: {slug}"),
            tickets: numbers
                .iter()
                .map(|&n| Ticket {
                    repo: slug.to_string(),
                    number: n,
                    title: format!("t{n}"),
                    status: Status::Frontier,
                    ticket_type: TicketType::Task,
                })
                .collect(),
        }
    }

    #[test]
    fn each_wayfinder_type_label_parses_to_its_type() {
        assert_eq!(
            TicketType::from_labels(["wayfinder:research"]),
            TicketType::Research
        );
        assert_eq!(TicketType::from_labels(["wayfinder:task"]), TicketType::Task);
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
        assert_eq!(TicketType::from_labels(Vec::<&str>::new()), TicketType::Untyped);
        // Labels, none of them types.
        assert_eq!(
            TicketType::from_labels(["bug", "documentation"]),
            TicketType::Untyped
        );
        // A `wayfinder:` label that is not a *type*: the map label itself, and
        // a type invented after this binary shipped.
        assert_eq!(TicketType::from_labels(["wayfinder:map"]), TicketType::Untyped);
        assert_eq!(TicketType::from_labels(["wayfinder:spike"]), TicketType::Untyped);
        // Near-misses are not fuzzy-matched: a type label is exact.
        assert_eq!(TicketType::from_labels(["research"]), TicketType::Untyped);
        assert_eq!(TicketType::from_labels(["Wayfinder:Research"]), TicketType::Untyped);
        assert_eq!(TicketType::from_label("wayfinder:research!"), None);
    }

    #[test]
    fn several_type_labels_resolve_hitl_first_so_ambiguity_never_grants_autostart() {
        // The rule that matters: research + anything else is not research, so
        // an ambiguous ticket is never started unattended.
        for other in [
            "wayfinder:task",
            "wayfinder:grilling",
            "wayfinder:prototype",
        ] {
            let both = TicketType::from_labels(["wayfinder:research", other]);
            assert_ne!(both, TicketType::Research, "research + {other}");
            assert!(!both.auto_startable(), "research + {other}");
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
    fn only_research_is_auto_startable() {
        assert!(TicketType::Research.auto_startable());
        // task is excluded on purpose: unattended code + commits stays a
        // ctrl-a judgement (#18).
        assert!(!TicketType::Task.auto_startable());
        assert!(!TicketType::Grilling.auto_startable());
        assert!(!TicketType::Prototype.auto_startable());
        assert!(!TicketType::Untyped.auto_startable());
    }

    #[test]
    fn merge_flattens_repos_sorted_by_repo_then_number() {
        let mut maps = std::collections::BTreeMap::new();
        maps.insert("kinisi/zeta".to_string(), map("kinisi/zeta", &[2, 1]));
        maps.insert("blooop/alpha".to_string(), map("blooop/alpha", &[9]));
        let merged = merge_maps(&maps);
        assert_eq!(merged.repo, "2 projects");
        let keys: Vec<(&str, u64)> = merged
            .tickets
            .iter()
            .map(|t| (t.repo.as_str(), t.number))
            .collect();
        assert_eq!(
            keys,
            vec![("blooop/alpha", 9), ("kinisi/zeta", 1), ("kinisi/zeta", 2)]
        );
    }

    #[test]
    fn short_repo_is_the_name_half_of_the_slug() {
        let t = &map("blooop/wayfinder", &[1]).tickets[0];
        assert_eq!(t.short_repo(), "wayfinder");
        assert_eq!(t.repo, "blooop/wayfinder");
    }

    #[test]
    fn merge_of_one_repo_names_it_in_the_header() {
        let mut maps = std::collections::BTreeMap::new();
        maps.insert("blooop/alpha".to_string(), map("blooop/alpha", &[1]));
        assert_eq!(merge_maps(&maps).repo, "blooop/alpha");
    }

    #[test]
    fn merge_of_nothing_says_so() {
        let merged = merge_maps(&std::collections::BTreeMap::new());
        assert!(merged.tickets.is_empty());
        assert!(merged.repo.contains("no projects"), "header: {}", merged.repo);
    }
}
