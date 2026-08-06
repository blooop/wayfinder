//! Integration test for the fetch layer against the real tracker: fetches
//! blooop/wayfinder's live map (#1) through `gh api graphql`. Needs network
//! and an authenticated `gh`.

use wf::model::{MapId, Status, TicketType};

#[tokio::test]
async fn fetches_the_live_wayfinder_map() {
    let map = wf::fetch::fetch_map(&MapId::new("blooop/wayfinder", 1))
        .await
        .expect("live fetch of blooop/wayfinder map #1");

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

    // The map's charted decisions are closed tickets — DONE must be non-empty.
    assert!(
        map.tickets.iter().any(|t| t.status == Status::Done),
        "the live map has closed tickets; none classified as done"
    );

    // #50: the full edge set survives the parse. This map's builds were chained
    // by blocking edges and are long closed, so if closed blockers were still
    // being discarded the whole DAG would come back empty.
    assert!(
        map.tickets.iter().any(|t| !t.blocked_by.is_empty()),
        "the live map's blocking edges must survive their blockers closing"
    );

    // #19's addition: the `labels` selection is accepted by the real API and
    // every ticket's `wayfinder:*` type comes back parsed. This map's types are
    // known facts — #3 is research, #19 is the build task, #18 is a grilling —
    // so a query GitHub silently dropped labels from would show up as Untyped.
    let typed = |n: u64| {
        map.tickets
            .iter()
            .find(|t| t.number == n)
            .map(|t| t.ticket_type)
    };
    assert_eq!(
        typed(3),
        Some(TicketType::Research),
        "#3 is wayfinder:research"
    );
    assert_eq!(typed(19), Some(TicketType::Task), "#19 is wayfinder:task");
    assert_eq!(
        typed(18),
        Some(TicketType::Grilling),
        "#18 is wayfinder:grilling"
    );
    assert_eq!(
        typed(9),
        Some(TicketType::Prototype),
        "#9 is wayfinder:prototype"
    );
    // The map issue itself is not a sub-issue, so nothing on the map carries
    // `wayfinder:map`; every ticket here is one of the four real types.
    assert!(
        map.tickets
            .iter()
            .all(|t| t.ticket_type != TicketType::Untyped),
        "every ticket on this map is labelled: {:?}",
        map.tickets
            .iter()
            .filter(|t| t.ticket_type == TicketType::Untyped)
            .map(|t| t.number)
            .collect::<Vec<_>>()
    );

    // Build 5 (#17) is blocked only by Build 1 (#13): while #13 is open it
    // must classify as blocked needing #13; once #13 closes it must not.
    if let Some(build5) = map.tickets.iter().find(|t| t.number == 17) {
        let build1_open = map
            .tickets
            .iter()
            .any(|t| t.number == 13 && t.status != Status::Done);
        match &build5.status {
            Status::Blocked { needs } => {
                assert!(build1_open, "#17 blocked but #13 is closed");
                assert!(needs.contains(&13), "#17 should need #13, needs {needs:?}");
            }
            other => assert!(
                !build1_open || *other == Status::Claimed || *other == Status::Done,
                "#13 is open and #17 unassigned, yet #17 is {other:?}"
            ),
        }
    }
}
