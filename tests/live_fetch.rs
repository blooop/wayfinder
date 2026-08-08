//! Integration test for the fetch layer against the real tracker: fetches one
//! of blooop/wayfinder's live maps through `gh api graphql`. Needs network
//! and an authenticated `gh`.
//!
//! The map is **looked up, not named** (see `common`), so every assertion
//! below is about what any live map must satisfy rather than about a
//! particular ticket on a particular map.

use wf::model::{Status, TicketType};

mod common;

#[tokio::test]
async fn fetches_the_live_wayfinder_map() {
    let map_id = common::a_live_map().await;
    let map = wf::fetch::fetch_map(&map_id)
        .await
        .unwrap_or_else(|e| panic!("live fetch of {map_id:?}: {e}"));

    assert!(
        map.title.starts_with("Map:"),
        "map issue title: {}",
        map.title
    );
    assert!(
        map.tickets.len() >= 7,
        "expected the real map's tickets, got {}",
        map.tickets.len()
    );

    // Ticket numbers are unique and sorted.
    let numbers: Vec<u64> = map.tickets.iter().map(|t| t.number).collect();
    let mut sorted = numbers.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(numbers, sorted);

    // The cluster sort key, proven against the real API: a fixture can only
    // show that *some* timestamp parses, and the thing that can actually go
    // wrong is the live `updatedAt` selection or its format — either of which
    // would silently demote every map to "activity unknown".
    assert!(
        map.last_activity.is_some(),
        "the live map issue's updatedAt must parse into an Activity"
    );

    // A map's charted decisions are closed tickets — DONE must be non-empty.
    assert!(
        map.tickets.iter().any(|t| t.status == Status::Done),
        "the live map has closed tickets; none classified as done"
    );

    // #50: the full edge set survives the parse. A map's tickets are chained by
    // blocking edges and the early ones close, so if closed blockers were still
    // being discarded the whole DAG would come back empty.
    assert!(
        map.tickets.iter().any(|t| !t.blocked_by.is_empty()),
        "the live map's blocking edges must survive their blockers closing"
    );

    // #19's addition: the `labels` selection is accepted by the real API and
    // every ticket's `wayfinder:*` type comes back parsed. A query GitHub
    // silently dropped labels from would show up as Untyped — and a map worth
    // charting mixes types, so a single-type answer is a parse collapsing
    // rather than a real map.
    let mut types: Vec<TicketType> = Vec::new();
    for ticket in &map.tickets {
        if !types.contains(&ticket.ticket_type) {
            types.push(ticket.ticket_type);
        }
    }
    assert!(
        !types.contains(&TicketType::Untyped),
        "every ticket on a map is labelled: {:?}",
        map.tickets
            .iter()
            .filter(|t| t.ticket_type == TicketType::Untyped)
            .map(|t| t.number)
            .collect::<Vec<_>>()
    );
    assert!(
        types.len() >= 2,
        "a real map mixes ticket types; got only {types:?}"
    );

    // The old `#17 is blocked only by #13, so it is Blocked while #13 is open`
    // case is **not** carried over, and not because it was hard to generalise.
    // Written generically it is a loop over whatever happens to be blocked
    // today, which on the map this now picks is nothing at all — it passed
    // vacuously the moment it was written, and a guard forcing it to be
    // non-vacuous would only re-pin the test to the tracker's contents. The
    // invariant itself loses nothing: classification from open blockers is
    // fixture-tested where it belongs, in `model::tests`
    // (`open_unassigned_with_open_blockers_is_blocked`,
    // `closed_is_done_even_if_assigned_or_blocked`) and at the parse boundary
    // in `fetch::tests`. What only the live API can prove is above.
}
