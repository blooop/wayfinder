//! Nucleo fuzzy filtering over the ticket list.
//!
//! Query behavior is 2a per the #9 resolution — groups survive typing: the
//! matcher only decides *which* rows survive; the caller keeps group
//! structure and order. Matching is scored against `"repo #num title"`, so
//! typing a repo name narrows to that project too.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::model::Ticket;

/// The haystack a ticket is matched against: the short repo name (what the
/// row shows) plus the number and title. The owner is left out — typing an
/// owner name is not how projects are picked, and including it would let
/// unrelated repos match on a shared owner.
fn haystack(ticket: &Ticket) -> String {
    format!("{} #{} {}", ticket.short_repo(), ticket.number, ticket.title)
}

/// Indices into `tickets` of the rows matching `query`, in input order.
/// The empty query matches everything.
pub fn matching_indices(tickets: &[Ticket], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return (0..tickets.len()).collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf = Vec::new();
    tickets
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            pattern
                .score(Utf32Str::new(&haystack(t), &mut buf), &mut matcher)
                .is_some()
        })
        .map(|(i, _)| i)
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
        }
    }

    fn fixture() -> Vec<Ticket> {
        vec![
            ticket("blooop/wayfinder", 6, "Re-entry breadcrumbs"),
            ticket("blooop/wayfinder", 9, "Main screen design"),
            ticket("blooop/dotfiles", 103, "Prune legacy bash aliases"),
        ]
    }

    #[test]
    fn empty_query_matches_all() {
        assert_eq!(matching_indices(&fixture(), ""), vec![0, 1, 2]);
    }

    #[test]
    fn query_narrows_to_fuzzy_title_matches() {
        assert_eq!(matching_indices(&fixture(), "bread"), vec![0]);
    }

    #[test]
    fn repo_name_and_number_are_matchable() {
        assert_eq!(matching_indices(&fixture(), "dotf"), vec![2]);
        assert_eq!(matching_indices(&fixture(), "#9"), vec![1]);
    }

    #[test]
    fn the_owner_half_of_the_slug_is_not_matched() {
        // Every fixture ticket is owned by blooop; matching on the owner
        // would make a shared owner narrow to nothing useful.
        assert!(matching_indices(&fixture(), "blooop").is_empty());
    }

    #[test]
    fn hopeless_query_matches_nothing() {
        assert!(matching_indices(&fixture(), "zzzzqx").is_empty());
    }
}
