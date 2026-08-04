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
                })
                .collect(),
        }
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
