//! Shape checks on the operational snippets the skills ship (#123).
//!
//! The skills under `skills/` are wf's interface: agents copy-paste these
//! snippets verbatim, so their *shape* is behavior. The frontier query in the
//! GitHub tracker doc must be the same single-hop map query the binary issues
//! (`src/fetch.rs`), not a hand-rolled per-child loop — one round trip per
//! frontier read, not one per open child.
//!
//! Offline by design, unlike the `live_*` binaries: the seam is the doc text
//! itself, so no network is involved.

/// The fenced bash snippet under the tracker doc's `## Frontier query`
/// heading — the exact text an agent copies to derive the frontier.
fn frontier_snippet() -> String {
    let doc = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/skills/wf/GITHUB_TRACKER.md"
    ))
    .expect("the tracker doc ships in this repo");
    let section = doc
        .split("## Frontier query")
        .nth(1)
        .expect("the tracker doc documents the frontier query")
        .split("\n## ")
        .next()
        .expect("split always yields a first piece")
        .to_string();
    section
        .split("```bash")
        .nth(1)
        .expect("the frontier query is a fenced, copy-pasteable bash snippet")
        .split("```")
        .next()
        .expect("split always yields a first piece")
        .to_string()
}

/// The frontier costs one round trip (up to the query's `first: 100` cap,
/// same as the binary's) — not a serial REST call per open child.
#[test]
fn frontier_is_one_round_trip() {
    let snippet = frontier_snippet();
    assert_eq!(
        snippet.matches("gh api").count(),
        1,
        "the frontier snippet must issue exactly one gh call:\n{snippet}"
    );
    assert!(
        !snippet.contains("while read"),
        "no per-child loop — the map query already carries every edge:\n{snippet}"
    );
    assert!(
        !snippet.contains("dependencies/blocked_by"),
        "blocking edges come from the map query's blockedBy selection, \
         not a per-child REST endpoint:\n{snippet}"
    );
}

/// The one round trip is the map query the binary itself issues: sub-issues
/// with their `blockedBy` edges in a single GraphQL hop, pinned to the
/// explicit `$REPO` like every other snippet in the doc.
#[test]
fn frontier_mirrors_the_binary_map_query() {
    let snippet = frontier_snippet();
    assert!(
        snippet.contains("gh api graphql"),
        "the frontier is a GraphQL query, same as src/fetch.rs:\n{snippet}"
    );
    for selection in ["subIssues", "blockedBy", "assignees"] {
        assert!(
            snippet.contains(selection),
            "the query must select `{selection}` — the fields the frontier \
             is derived from:\n{snippet}"
        );
    }
    assert!(
        snippet.contains("$REPO") || snippet.contains("${REPO"),
        "every snippet in the doc targets $REPO explicitly:\n{snippet}"
    );
}
