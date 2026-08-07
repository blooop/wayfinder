//! Nucleo fuzzy scoring over the ticket list, and where each match landed.
//!
//! A live query *sifts* the body (#51): the matcher scores every ticket, and
//! [`crate::view`] prunes the tree to what matched and orders what is left
//! best-score-first. This module answers only two questions about one ticket —
//! how well it matched, and which characters the query landed on — and the
//! screen is built out of the answers.
//!
//! Matching is scored against `"repo #num title"`, so typing a repo name
//! narrows to that project too. That haystack spans two things the screen draws
//! in different places: the repo appears once in the cluster header, the rest on
//! the row. [`Hit`] therefore reports the two halves separately rather than
//! handing out raw haystack offsets nobody could map back — a query that lands
//! entirely on the repo name (`dotf`) has nothing to underline on any row, and
//! would look like a screen full of matches with no matches in it if the header
//! could not answer for it.

use std::fmt;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::model::{Map, Ticket};

/// The part of the haystack the row itself draws: `#n title`. Public because
/// the screen underlines characters *by index* into it — the string drawn and
/// the string matched have to be the same string, and this is what makes that
/// true by construction rather than by two format strings agreeing.
pub fn row_text(ticket: &Ticket) -> String {
    format!("#{} {}", ticket.number, ticket.title)
}

/// The haystack a ticket is matched against: the short repo name (what the
/// cluster header shows) then the row's own text. The owner is left out —
/// typing an owner name is not how projects are picked, and including it would
/// let unrelated repos match on a shared owner.
fn haystack(ticket: &Ticket) -> String {
    format!("{} {}", ticket.short_repo(), row_text(ticket))
}

/// Where a query landed in one ticket, in char indices into each of the two
/// strings the screen draws — never into the haystack, which is drawn nowhere.
/// Both are sorted and unique. The space between the halves belongs to neither
/// and is dropped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hit {
    pub score: u32,
    /// Indices into the short repo name, as the cluster header draws it.
    pub in_repo: Vec<usize>,
    /// Indices into [`row_text`].
    pub in_row: Vec<usize>,
}

/// A parsed query, ready to score tickets and to say where it landed in them.
///
/// Built once and reused: [`Matcher`] owns the scratch buffers every match runs
/// in and is meant to be handed to one match after another, so building one per
/// row would allocate them again for every line on screen.
pub struct Query {
    matcher: Matcher,
    pattern: Pattern,
    /// Scratch for the `Utf32Str` conversion, reused for the same reason.
    buf: Vec<char>,
}

impl fmt::Debug for Query {
    /// Hand-written because [`Matcher`] has no `Debug` of its own — and the two
    /// fields left out are the reusable scratch buffers, whose contents are
    /// whatever the last match happened to leave in them. The pattern is the
    /// whole of what this value *is*, so the rest is `non_exhaustive`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Query")
            .field("pattern", &self.pattern)
            .finish_non_exhaustive()
    }
}

impl Query {
    /// `None` for the empty query — there is nothing to match and nothing to
    /// highlight, and a caller holding a `Query` is a caller with a live one.
    pub fn new(query: &str) -> Option<Query> {
        if query.is_empty() {
            return None;
        }
        Some(Query {
            matcher: Matcher::new(Config::DEFAULT),
            pattern: Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart),
            buf: Vec::new(),
        })
    }

    /// How well this ticket matched, or `None` for no match at all.
    pub fn score(&mut self, ticket: &Ticket) -> Option<u32> {
        let hay = haystack(ticket);
        self.pattern
            .score(Utf32Str::new(&hay, &mut self.buf), &mut self.matcher)
    }

    /// The same match, with the characters it landed on. Strictly more work
    /// than [`Query::score`], so the sieve asks for the score and only the draw
    /// asks for this.
    pub fn hit(&mut self, ticket: &Ticket) -> Option<Hit> {
        let hay = haystack(ticket);
        let mut indices = Vec::new();
        let score = self.pattern.indices(
            Utf32Str::new(&hay, &mut self.buf),
            &mut self.matcher,
            &mut indices,
        )?;
        // Nucleo makes no promise about order and can repeat an index across
        // atoms of a multi-atom pattern; the screen walks these in step with
        // the characters, so they have to be sorted and unique.
        indices.sort_unstable();
        indices.dedup();

        let repo_len = ticket.short_repo().chars().count();
        let mut hit = Hit {
            score,
            ..Hit::default()
        };
        for i in indices.into_iter().map(|i| i as usize) {
            if i < repo_len {
                hit.in_repo.push(i);
            } else if i > repo_len {
                hit.in_row.push(i - repo_len - 1);
            }
        }
        Some(hit)
    }

    /// Where the query landed in a cluster's repo name, taken from whichever of
    /// its tickets matched best. Every ticket in a map shares one repo name but
    /// not one match — `wf6` can reach the repo through one row and not
    /// another — and the header is drawn once, so it answers for the row the
    /// query liked most, which is the row it put at the top.
    pub fn in_repo(&mut self, map: &Map) -> Vec<usize> {
        map.tickets
            .iter()
            .filter_map(|ticket| self.hit(ticket))
            .max_by_key(|hit| hit.score)
            .map(|hit| hit.in_repo)
            .unwrap_or_default()
    }
}

/// Score every ticket against `query`, in input order: `None` is no match,
/// and a higher score is a better one. The empty query matches everything
/// (at an equal score), though no caller renders that case — an empty query
/// means the structured screen, not a sifted one.
pub fn scores(tickets: &[Ticket], query: &str) -> Vec<Option<u32>> {
    let Some(mut query) = Query::new(query) else {
        return vec![Some(0); tickets.len()];
    };
    tickets.iter().map(|t| query.score(t)).collect()
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

    /// A hit rendered against the strings it indexes: the characters it landed
    /// on wrapped in `«»`, repo first, then the row text. This is what the
    /// screen underlines, written so a test can read it.
    fn landed(ticket: &Ticket, query: &str) -> (String, String) {
        let hit = Query::new(query)
            .expect("a live query")
            .hit(ticket)
            .expect("a match");
        let mark = |text: &str, lit: &[usize]| {
            text.chars()
                .enumerate()
                .map(|(i, ch)| {
                    if lit.contains(&i) {
                        format!("«{ch}»")
                    } else {
                        ch.to_string()
                    }
                })
                .collect()
        };
        (
            mark(ticket.short_repo(), &hit.in_repo),
            mark(&row_text(ticket), &hit.in_row),
        )
    }

    #[test]
    fn a_hit_reports_where_it_landed_in_each_half_it_is_drawn_in() {
        let t = ticket("blooop/wayfinder", 6, "Re-entry breadcrumbs");
        assert_eq!(
            landed(&t, "bread"),
            (
                "wayfinder".to_string(),
                "#6 Re-entry «b»«r»«e»«a»«d»crumbs".to_string()
            ),
            "the title half is lit and the repo is untouched"
        );
    }

    #[test]
    fn a_query_that_lands_only_on_the_repo_lights_nothing_on_the_row() {
        // The case the header highlight exists for: `dotf` sifts the screen
        // down to one project while matching no character any row draws.
        let t = ticket("blooop/dotfiles", 103, "Prune legacy bash aliases");
        assert_eq!(
            landed(&t, "dotf"),
            (
                "«d»«o»«t»«f»iles".to_string(),
                "#103 Prune legacy bash aliases".to_string()
            )
        );
    }

    #[test]
    fn a_hit_spans_both_halves_and_never_the_space_between_them() {
        // "wayf6" reaches across the boundary: the repo name, then the number
        // on the row. The space that joins them in the haystack is drawn by
        // neither, so no index may point at it.
        let t = ticket("blooop/wayfinder", 6, "Re-entry breadcrumbs");
        assert_eq!(
            landed(&t, "wayf6"),
            (
                "«w»«a»«y»«f»inder".to_string(),
                "#«6» Re-entry breadcrumbs".to_string()
            )
        );
    }

    #[test]
    fn hit_indices_come_back_sorted_and_unique() {
        // The screen walks these in step with the characters, so out-of-order
        // or repeated indices would silently skip highlights. Nucleo promises
        // neither, which is why this is pinned rather than assumed.
        let t = ticket("blooop/wayfinder", 6, "Re-entry breadcrumbs");
        for query in ["bread", "e e", "wayf6", "re-entry breadcrumbs"] {
            let hit = Query::new(query)
                .expect("a live query")
                .hit(&t)
                .unwrap_or_else(|| panic!("{query} matches"));
            for half in [&hit.in_repo, &hit.in_row] {
                let mut sorted = half.clone();
                sorted.sort_unstable();
                sorted.dedup();
                assert_eq!(*half, sorted, "{query}");
            }
        }
    }

    #[test]
    fn a_hit_agrees_with_the_score_the_sieve_used() {
        // Two nucleo calls, one answer: the row the screen lights up has to be
        // the row the sieve kept, and by the same margin it ranked it on.
        let tickets = fixture();
        let mut query = Query::new("bread").expect("a live query");
        for ticket in &tickets {
            assert_eq!(
                query.hit(ticket).map(|hit| hit.score),
                query.score(ticket),
                "{}",
                ticket.title
            );
        }
    }

    #[test]
    fn the_empty_query_is_no_query_at_all() {
        assert!(Query::new("").is_none());
    }
}
