//! Integration test for the fetch layer against the real tracker: fetches one
//! of blooop/wayfinder's live maps through `gh api graphql`. Needs network
//! and an authenticated `gh`.
//!
//! The map is **looked up, not named** (see `common`), so no assertion below
//! names a ticket. They do assume a *mature* map — seven tickets, some closed,
//! some blocking edges — which is why the lookup takes the oldest open one
//! rather than any open one.

use wf::model::{Status, TicketType};

mod common;

#[tokio::test]
#[ignore = "live: needs network + an authenticated gh"]
async fn fetches_the_live_wayfinder_map() {
    let map_id = common::a_live_map().await;
    let map = wf::fetch::fetch_map(&map_id)
        .await
        .unwrap_or_else(|e| panic!("live fetch of {map_id:?}: {e:#}"));

    // Not the `Map:` title convention: nothing enforces it — issue #67 carries
    // `wayfinder:map` without it — and the fixture no longer knows which map it
    // has. That an issue is a map at all is checked where it can be: `fetch_map`
    // refuses anything that is not an open `wayfinder:map` (#28).
    assert!(
        map.map_title().is_some_and(|t| !t.is_empty()),
        "the map issue's title must come back"
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
    // silently dropped labels from shows up here as Untyped. Which *specific*
    // label maps to which type is fixture-tested in `model::tests`, so this
    // does not also demand that the live map happen to mix types.
    assert!(
        map.tickets
            .iter()
            .all(|t| t.ticket_type != TicketType::Untyped),
        "every ticket on a map is labelled: {:?}",
        map.tickets
            .iter()
            .filter(|t| t.ticket_type == TicketType::Untyped)
            .map(|t| t.number)
            .collect::<Vec<_>>()
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

/// The inbox read against the real tracker: the one thing a fixture cannot
/// prove is that the `search`-based GraphQL query is *valid* — a mistyped
/// selection, a field GitHub renamed, or `blockedBy` not existing on a bare
/// `Issue` all come back as a GraphQL error rather than a parse failure, and
/// the whole feature would be an empty heading nobody could debug from a unit
/// test.
///
/// Deliberately makes **no claim about what is in it**. Whose inbox this runs
/// as depends on the token — a maintainer locally, `GH_TOKEN` in CI — and the
/// unassigned half depends on whatever is untriaged today, so an empty answer
/// is a correct answer and the assertions are about shape, not contents. The
/// one thing asserted unconditionally is that both searches *ran*.
#[tokio::test]
#[ignore = "live: needs network + an authenticated gh"]
async fn reads_the_live_inbox() {
    let inbox = wf::fetch::fetch_inbox(&[common::THIS_REPO.to_string()])
        .await
        .unwrap_or_else(|e| panic!("live inbox read: {e:#}"));

    for (repo, cluster) in &inbox {
        assert_eq!(
            repo,
            common::THIS_REPO,
            "only the repos asked for come back"
        );
        assert!(
            cluster.map_title().is_none(),
            "an inbox cluster has no map issue to be titled by"
        );
        assert!(
            !cluster.tickets.is_empty(),
            "a repo with nothing assigned is absent, never an empty heading"
        );
        for ticket in &cluster.tickets {
            assert_eq!(ticket.repo, *repo, "each row carries its own repo");
            // Both halves of the query ask `is:open`, so nothing here is done.
            // Which of the other three a row is depends on which half found it
            // — assigned is claimed, unassigned is frontier, or blocked when
            // something open blocks it — and asserting one of them would only
            // hold while the tracker happened to be in that state. What is
            // asserted is the invariant the *query* guarantees.
            assert_ne!(
                ticket.status,
                Status::Done,
                "#{} came back done from an is:open search",
                ticket.number
            );
        }
        // A map is an open issue with nobody assigned, so `no:assignee` finds
        // every map in every repo asked about. Live proof that the label drop
        // works, since this repo always has an open map: without it every
        // cluster header would also be a row of the inbox below it.
        let maps = wf::fetch::find_maps(&[common::THIS_REPO.to_string()])
            .await
            .expect("the map search answers");
        for id in &maps {
            assert!(
                !cluster.tickets.iter().any(|t| t.number == id.number),
                "map #{} is a heading, not a row of the inbox",
                id.number
            );
        }
        assert!(
            cluster.last_activity.is_some(),
            "the live updatedAt selection must parse, or every inbox sorts as unknown"
        );
    }
}
