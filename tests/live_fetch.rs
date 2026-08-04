//! Integration test for the fetch layer against the real tracker: fetches
//! blooop/wayfinder's live map (#1) through `gh api graphql`. Needs network
//! and an authenticated `gh`.

use wf::model::Status;

#[tokio::test]
async fn fetches_the_live_wayfinder_map() {
    let map = wf::fetch::fetch_map("blooop", "wayfinder", 1)
        .await
        .expect("live fetch of blooop/wayfinder map #1");

    assert_eq!(map.repo, "blooop/wayfinder");
    assert!(map.title.starts_with("Map:"), "map issue title: {}", map.title);
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

    // The map's charted decisions are closed tickets — DONE must be non-empty.
    assert!(
        map.tickets.iter().any(|t| t.status == Status::Done),
        "the live map has closed tickets; none classified as done"
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
