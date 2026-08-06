//! Nucleo fuzzy scoring over the ticket list.
//!
//! A live query *flattens* the body (#51, retiring the 2a groups-survive-typing
//! rule with the groups themselves): the matcher scores every ticket, and the
//! flattened screen orders rows best-score-first. This module only scores; the
//! ordering — and everything else about what the query does to the screen —
//! lives in [`crate::view`]. Matching is scored against `"repo #num title"`,
//! so typing a repo name narrows to that project too.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::model::Ticket;

/// The haystack a ticket is matched against: the short repo name (what the
/// flattened row shows) plus the number and title. The owner is left out —
/// typing an owner name is not how projects are picked, and including it would
/// let unrelated repos match on a shared owner.
fn haystack(ticket: &Ticket) -> String {
    format!(
        "{} #{} {}",
        ticket.short_repo(),
        ticket.number,
        ticket.title
    )
}

/// Score every ticket against `query`, in input order: `None` is no match,
/// and a higher score is a better one. The empty query matches everything
/// (at an equal score), though no caller renders that case — an empty query
/// means the structured screen, not a flattened one.
pub fn scores(tickets: &[Ticket], query: &str) -> Vec<Option<u32>> {
    if query.is_empty() {
        return vec![Some(0); tickets.len()];
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf = Vec::new();
    tickets
        .iter()
        .map(|t| pattern.score(Utf32Str::new(&haystack(t), &mut buf), &mut matcher))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Status, TicketType};

    fn ticket(repo: &str, number: u64, title: &str) -> Ticket {
        Ticket {
            repo: repo.to_string(),
            number,
            title: title.to_string(),
            status: Status::Frontier,
            ticket_type: TicketType::Task,
            blocked_by: vec![],
            prs: vec![],
        }
    }

    fn fixture() -> Vec<Ticket> {
        vec![
            ticket("blooop/wayfinder", 6, "Re-entry breadcrumbs"),
            ticket("blooop/wayfinder", 9, "Main screen design"),
            ticket("blooop/dotfiles", 103, "Prune legacy bash aliases"),
        ]
    }

    /// Indices of the matching tickets, in input order.
    fn matching(tickets: &[Ticket], query: &str) -> Vec<usize> {
        scores(tickets, query)
            .into_iter()
            .enumerate()
            .filter_map(|(i, s)| s.map(|_| i))
            .collect()
    }

    #[test]
    fn empty_query_matches_all() {
        assert_eq!(matching(&fixture(), ""), vec![0, 1, 2]);
    }

    #[test]
    fn query_narrows_to_fuzzy_title_matches() {
        assert_eq!(matching(&fixture(), "bread"), vec![0]);
    }

    #[test]
    fn repo_name_and_number_are_matchable() {
        assert_eq!(matching(&fixture(), "dotf"), vec![2]);
        assert_eq!(matching(&fixture(), "#9"), vec![1]);
    }

    #[test]
    fn the_owner_half_of_the_slug_is_not_matched() {
        // Every fixture ticket is owned by blooop; matching on the owner
        // would make a shared owner narrow to nothing useful.
        assert!(matching(&fixture(), "blooop").is_empty());
    }

    #[test]
    fn hopeless_query_matches_nothing() {
        assert!(matching(&fixture(), "zzzzqx").is_empty());
    }

    #[test]
    fn a_tighter_match_scores_higher() {
        let tickets = vec![
            ticket("blooop/wayfinder", 1, "breadcrumbs"),
            ticket("blooop/wayfinder", 2, "b r e a d spelled out, crumbs later"),
        ];
        let scored = scores(&tickets, "bread");
        assert!(scored[0].expect("exact-ish match") > scored[1].expect("scattered match"));
    }
}
