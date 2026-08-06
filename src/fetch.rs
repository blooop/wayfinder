//! Live map fetch via `gh api graphql`.
//!
//! One GraphQL query per map (per the #3 data-plane resolution): the map
//! issue, its sub-issues, and each sub-issue's `blockedBy` edges with the
//! blocker's state, so open-blocker classification needs no further calls.
//!
//! Labels ride along in the same selection (#19) at the same 2 rate-limit
//! points, and this is the one place they are ever looked at as strings: a
//! sub-issue's labels become a [`TicketType`] here and nothing inward re-sniffs
//! them (parse, don't validate).
//!
//! The `blockedBy` selection already carries the *full* edge set — closed
//! blockers included — and since #50 the parse keeps it: the open subset
//! becomes [`Status::Blocked`]'s `needs`, the whole set becomes
//! [`Ticket::blocked_by`] (the DAG), and neither is re-derived from the other
//! afterwards.
//!
//! Both invocations are `stdin`-nulled and `kill_on_drop`. Neither is
//! decoration. `tokio`'s `Command::output()` pipes only stdout and stderr and
//! leaves **stdin inherited** — a silent divergence from `std`'s, which nulls
//! it — so without the first, every `gh` here holds `wf`'s terminal, which is
//! exactly the fd leak that broke #30. Without the second, a `gh` still in
//! flight when `wf` `exec`s into the agent is inherited by the agent as a
//! zombie it will never reap: aborting the task drops the `Child`, and only
//! `kill_on_drop` turns that into a signal.

use std::process::Stdio;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use tokio::process::Command;

use crate::model::{classify, Map, MapId, MapSet, Ticket, TicketType};

/// The label that makes an issue a map. Both the search that *finds* maps and
/// the fetch that *reads* one test for this, so a cached number can never be
/// believed on its own (#28).
pub const MAP_LABEL: &str = "wayfinder:map";

const MAP_QUERY: &str = "\
query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    issue(number: $number) {
      title
      state
      labels(first: 20) { nodes { name } }
      subIssues(first: 100) {
        nodes {
          number title state
          labels(first: 10) { nodes { name } }
          assignees(first: 5) { nodes { login } }
          blockedBy(first: 50) { nodes { number state } }
        }
      }
    }
  }
}";

#[derive(Deserialize)]
struct GraphQlResponse {
    data: Option<ResponseData>,
    #[serde(default)]
    errors: Vec<GraphQlError>,
}

#[derive(Deserialize)]
struct GraphQlError {
    message: String,
}

#[derive(Deserialize)]
struct ResponseData {
    repository: Option<Repository>,
}

#[derive(Deserialize)]
struct Repository {
    issue: Option<MapIssue>,
}

#[derive(Deserialize)]
struct MapIssue {
    title: String,
    state: String,
    labels: Nodes<Label>,
    #[serde(rename = "subIssues")]
    sub_issues: Nodes<SubIssue>,
}

#[derive(Deserialize)]
struct Nodes<T> {
    nodes: Vec<T>,
}

#[derive(Deserialize)]
struct SubIssue {
    number: u64,
    title: String,
    state: String,
    labels: Nodes<Label>,
    assignees: Nodes<Assignee>,
    #[serde(rename = "blockedBy")]
    blocked_by: Nodes<Blocker>,
}

#[derive(Deserialize)]
struct Label {
    name: String,
}

#[derive(Deserialize)]
struct Assignee {
    #[allow(dead_code)]
    login: String,
}

#[derive(Deserialize)]
struct Blocker {
    number: u64,
    state: String,
}

fn is_open(state: &str) -> bool {
    state == "OPEN"
}

/// Fetch one map live: the map issue named by `id`, its sub-issues, and their
/// blocking edges — one `gh api graphql` round trip.
pub async fn fetch_map(id: &MapId) -> Result<Map> {
    let (owner, name) = id
        .repo
        .split_once('/')
        .with_context(|| format!("malformed repo slug {:?}", id.repo))?;
    let output = Command::new("gh")
        .args([
            "api",
            "graphql",
            "-F",
            &format!("owner={owner}"),
            "-F",
            &format!("name={name}"),
            "-F",
            &format!("number={}", id.number),
            "-f",
            &format!("query={MAP_QUERY}"),
        ])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .context("failed to run `gh` — is the GitHub CLI installed and on PATH?")?;

    if !output.status.success() {
        bail!(
            "`gh api graphql` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    parse_map(&output.stdout, id)
}

/// Turn one `gh api graphql` response body into a [`Map`] — the whole parse
/// boundary, kept apart from the process call so it is testable without the
/// network. Every raw tracker string a ticket is derived from (state,
/// assignees, blocker states, labels) is interpreted exactly here.
fn parse_map(body: &[u8], id: &MapId) -> Result<Map> {
    let resp: GraphQlResponse =
        serde_json::from_slice(body).context("unparseable GraphQL response from gh")?;
    if let Some(err) = resp.errors.first() {
        bail!("GraphQL error: {}", err.message);
    }
    let issue = resp
        .data
        .and_then(|d| d.repository)
        .and_then(|r| r.issue)
        .with_context(|| format!("map issue {}#{} not found", id.repo, id.number))?;

    // The number may have come from the cache (#28), so the issue it names has
    // to prove it is still a map rather than be taken at its word. A map that
    // was closed, relabelled, or never was one fails here — the repo then shows
    // as stale, which is honest, instead of rendering some unrelated issue's
    // sub-issues as its map. The unconditional search corrects the number
    // moments later.
    if !is_open(&issue.state) || !issue.labels.nodes.iter().any(|l| l.name == MAP_LABEL) {
        bail!(
            "{}#{} is no longer an open `{MAP_LABEL}` issue",
            id.repo,
            id.number
        );
    }

    let mut tickets: Vec<Ticket> = issue
        .sub_issues
        .nodes
        .into_iter()
        .map(|sub| {
            // One pass over the same edges yields both facts: the open subset
            // is status, the whole set is structure (#50).
            let open_blockers: Vec<u64> = sub
                .blocked_by
                .nodes
                .iter()
                .filter(|b| is_open(&b.state))
                .map(|b| b.number)
                .collect();
            let blocked_by: Vec<u64> = sub.blocked_by.nodes.iter().map(|b| b.number).collect();
            Ticket {
                repo: id.repo.clone(),
                number: sub.number,
                title: sub.title,
                status: classify(
                    is_open(&sub.state),
                    !sub.assignees.nodes.is_empty(),
                    open_blockers,
                ),
                ticket_type: TicketType::from_labels(
                    sub.labels.nodes.iter().map(|l| l.name.as_str()),
                ),
                blocked_by,
            }
        })
        .collect();
    tickets.sort_by_key(|t| t.number);

    Ok(Map {
        title: issue.title,
        tickets,
    })
}

/// One item of a `search/issues` response — just what map detection needs.
#[derive(Deserialize)]
struct SearchItem {
    number: u64,
    repository_url: String,
}

#[derive(Deserialize)]
struct SearchResponse {
    items: Vec<SearchItem>,
}

/// Every open `wayfinder:map` issue across `repos` — one label-scoped search
/// (per the #4 resolution: one query intersected with cached remotes, never
/// N probes). Returns the full [`MapSet`]: a repo with several open maps
/// contributes several ids (#50 — the lowest-number-per-slug rule that used to
/// hide all but one is gone), and repos without maps are simply absent. Only
/// open map issues count — a closed map is a finished map.
pub async fn find_maps(repos: &[String]) -> Result<MapSet> {
    if repos.is_empty() {
        return Ok(MapSet::new());
    }
    // Multiple `repo:` qualifiers OR together in GitHub issue search, so
    // the whole cached set is one query.
    let scope: Vec<String> = repos.iter().map(|r| format!("repo:{r}")).collect();
    let query = format!("label:\"{MAP_LABEL}\" is:issue is:open {}", scope.join(" "));

    let output = Command::new("gh")
        .args([
            "api",
            "-X",
            "GET",
            "search/issues",
            "-f",
            &format!("q={query}"),
            "-F",
            "per_page=100",
        ])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .context("failed to run `gh` for the map search")?;
    if !output.status.success() {
        bail!(
            "`gh api search/issues` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    parse_map_search(&output.stdout)
}

/// Parse a `search/issues` response into the set of open maps it names.
fn parse_map_search(body: &[u8]) -> Result<MapSet> {
    let resp: SearchResponse =
        serde_json::from_slice(body).context("unparseable search response from gh")?;
    let mut maps = MapSet::new();
    for item in resp.items {
        // repository_url is "https://api.github.com/repos/<owner>/<name>".
        let Some(slug) = item.repository_url.split("/repos/").nth(1) else {
            continue;
        };
        maps.insert(MapId::new(slug, item.number));
    }
    Ok(maps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Status;

    #[test]
    fn map_search_keeps_every_open_map_including_several_on_one_repo() {
        // The #50 change in one fixture: wayfinder has two open maps and both
        // survive the parse — the old lowest-number rule would have kept only
        // #1 and silently hidden #9.
        let body = r#"{"items": [
            {"number": 9, "repository_url": "https://api.github.com/repos/blooop/wayfinder"},
            {"number": 1, "repository_url": "https://api.github.com/repos/blooop/wayfinder"},
            {"number": 4, "repository_url": "https://api.github.com/repos/kinisi/kinisi_ros"}
        ]}"#;
        let maps = parse_map_search(body.as_bytes()).expect("parse");
        let expected: MapSet = [
            MapId::new("blooop/wayfinder", 1),
            MapId::new("blooop/wayfinder", 9),
            MapId::new("kinisi/kinisi_ros", 4),
        ]
        .into_iter()
        .collect();
        assert_eq!(maps, expected);
    }

    /// A response shaped exactly like the live one, with the `labels` selection
    /// #19 added — the sub-issue types wf now reads.
    const MAP_RESPONSE: &str = r#"{"data": {"repository": {"issue": {
        "title": "Map: wf",
        "state": "OPEN",
        "labels": {"nodes": [{"name": "wayfinder:map"}]},
        "subIssues": {"nodes": [
            {"number": 19, "title": "Build 6", "state": "OPEN",
             "labels": {"nodes": [{"name": "wayfinder:task"}]},
             "assignees": {"nodes": []},
             "blockedBy": {"nodes": [{"number": 18, "state": "CLOSED"}]}},
            {"number": 3, "title": "GitHub Issues as the live data plane", "state": "OPEN",
             "labels": {"nodes": [{"name": "enhancement"}, {"name": "wayfinder:research"}]},
             "assignees": {"nodes": []},
             "blockedBy": {"nodes": []}},
            {"number": 21, "title": "Unlabelled fog", "state": "OPEN",
             "labels": {"nodes": []},
             "assignees": {"nodes": []},
             "blockedBy": {"nodes": [{"number": 18, "state": "CLOSED"}, {"number": 3, "state": "OPEN"}]}}
        ]}
    }}}}"#;

    fn wf_map_id() -> MapId {
        MapId::new("blooop/wayfinder", 1)
    }

    #[test]
    fn the_map_parse_carries_each_sub_issues_type_through_from_its_labels() {
        let map = parse_map(MAP_RESPONSE.as_bytes(), &wf_map_id()).expect("parse");
        assert_eq!(map.title, "Map: wf");
        let types: Vec<(u64, TicketType)> =
            map.tickets.iter().map(|t| (t.number, t.ticket_type)).collect();
        assert_eq!(
            types,
            vec![
                (3, TicketType::Research),
                (19, TicketType::Task),
                // No labels at all is Untyped — one meaning ("no recognised
                // type"), never a stand-in for several.
                (21, TicketType::Untyped),
            ]
        );
        // The type is a *separate* axis from derived status: #3 is frontier
        // *and* research, and neither fact is read off the other.
        let research = map.tickets.iter().find(|t| t.number == 3).expect("#3");
        assert_eq!(research.status, Status::Frontier);
    }

    #[test]
    fn the_parse_keeps_closed_blocker_edges_as_structure_not_status() {
        // #19 is blocked only by the closed #18: frontier for status, but the
        // edge survives on `blocked_by` — the DAG the selection views draw.
        let map = parse_map(MAP_RESPONSE.as_bytes(), &wf_map_id()).expect("parse");
        let t19 = map.tickets.iter().find(|t| t.number == 19).expect("#19");
        assert_eq!(t19.status, Status::Frontier, "a closed blocker doesn't block");
        assert_eq!(t19.blocked_by, vec![18], "…but its edge is kept");
        // #21 mixes one closed and one open blocker: status sees only the open
        // one, structure sees both.
        let t21 = map.tickets.iter().find(|t| t.number == 21).expect("#21");
        assert_eq!(t21.status, Status::Blocked { needs: vec![3] });
        assert_eq!(t21.blocked_by, vec![18, 3]);
        // And the reverse edges fall out by inversion, closed blocker included.
        assert_eq!(map.unblocks(18), vec![19, 21]);
        assert_eq!(map.unblocks(3), vec![21]);
    }

    /// The same response with the map issue's own state/labels swapped out —
    /// what a cached number that has gone stale actually fetches back.
    fn map_response_with(state: &str, labels: &str) -> String {
        MAP_RESPONSE
            .replace(r#""state": "OPEN","#, &format!(r#""state": "{state}","#))
            .replacen(
                r#""labels": {"nodes": [{"name": "wayfinder:map"}]},"#,
                &format!(r#""labels": {{"nodes": [{labels}]}},"#),
                1,
            )
    }

    #[test]
    fn a_stale_cached_number_is_rejected_rather_than_rendered_as_a_map() {
        // The three ways a cached number goes wrong (#28): the map was closed,
        // it lost its label, or the number now names a wholly unrelated issue.
        // None may render — a wrong map is worse than no map.
        for (state, labels) in [
            ("CLOSED", r#"{"name": "wayfinder:map"}"#),
            ("OPEN", r#"{"name": "enhancement"}"#),
            ("OPEN", ""),
        ] {
            let body = map_response_with(state, labels);
            let err = parse_map(body.as_bytes(), &wf_map_id())
                .expect_err("a non-map must not parse as a map");
            assert!(
                err.to_string().contains("no longer an open"),
                "unexpected error: {err}"
            );
        }
    }

    #[test]
    fn empty_search_result_means_no_maps() {
        let maps = parse_map_search(br#"{"items": []}"#).expect("parse");
        assert!(maps.is_empty());
    }
}
