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
    /// Short repo name shown in the row's repo column (e.g. "wayfinder").
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub status: Status,
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
}
