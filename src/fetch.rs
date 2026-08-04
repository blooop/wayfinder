//! Live map fetch via `gh api graphql`.
//!
//! One GraphQL query per map (per the #3 data-plane resolution): the map
//! issue, its sub-issues, and each sub-issue's `blockedBy` edges with the
//! blocker's state, so open-blocker classification needs no further calls.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use tokio::process::Command;

use crate::model::{classify, Map, Ticket};

const MAP_QUERY: &str = "\
query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    issue(number: $number) {
      title
      subIssues(first: 100) {
        nodes {
          number title state
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
    assignees: Nodes<Assignee>,
    #[serde(rename = "blockedBy")]
    blocked_by: Nodes<Blocker>,
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

/// Fetch one repo's map live: the map issue `number` in `owner/name`, its
/// sub-issues, and their blocking edges — one `gh api graphql` round trip.
pub async fn fetch_map(owner: &str, name: &str, number: u64) -> Result<Map> {
    let output = Command::new("gh")
        .args([
            "api",
            "graphql",
            "-F",
            &format!("owner={owner}"),
            "-F",
            &format!("name={name}"),
            "-F",
            &format!("number={number}"),
            "-f",
            &format!("query={MAP_QUERY}"),
        ])
        .output()
        .await
        .context("failed to run `gh` — is the GitHub CLI installed and on PATH?")?;

    if !output.status.success() {
        bail!(
            "`gh api graphql` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let resp: GraphQlResponse =
        serde_json::from_slice(&output.stdout).context("unparseable GraphQL response from gh")?;
    if let Some(err) = resp.errors.first() {
        bail!("GraphQL error: {}", err.message);
    }
    let issue = resp
        .data
        .and_then(|d| d.repository)
        .and_then(|r| r.issue)
        .with_context(|| format!("map issue {owner}/{name}#{number} not found"))?;

    let mut tickets: Vec<Ticket> = issue
        .sub_issues
        .nodes
        .into_iter()
        .map(|sub| {
            let open_blockers: Vec<u64> = sub
                .blocked_by
                .nodes
                .iter()
                .filter(|b| is_open(&b.state))
                .map(|b| b.number)
                .collect();
            Ticket {
                repo: format!("{owner}/{name}"),
                number: sub.number,
                title: sub.title,
                status: classify(
                    is_open(&sub.state),
                    !sub.assignees.nodes.is_empty(),
                    open_blockers,
                ),
            }
        })
        .collect();
    tickets.sort_by_key(|t| t.number);

    Ok(Map {
        repo: format!("{owner}/{name}"),
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

/// Which of `repos` have a `wayfinder:map` issue — one label-scoped search
/// (per the #4 resolution: one query intersected with cached remotes, never
/// N probes). Returns `repo slug → map issue number`; repos without maps
/// are simply absent. Only open map issues count (a closed map is a
/// finished map), and if a repo somehow has several, the lowest issue
/// number wins — deterministic and almost always the original.
pub async fn find_maps(repos: &[String]) -> Result<HashMap<String, u64>> {
    if repos.is_empty() {
        return Ok(HashMap::new());
    }
    // Multiple `repo:` qualifiers OR together in GitHub issue search, so
    // the whole cached set is one query.
    let scope: Vec<String> = repos.iter().map(|r| format!("repo:{r}")).collect();
    let query = format!("label:\"wayfinder:map\" is:issue is:open {}", scope.join(" "));

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

/// Parse a `search/issues` response into `repo slug → lowest map issue`.
fn parse_map_search(body: &[u8]) -> Result<HashMap<String, u64>> {
    let resp: SearchResponse =
        serde_json::from_slice(body).context("unparseable search response from gh")?;
    let mut maps = HashMap::new();
    for item in resp.items {
        // repository_url is "https://api.github.com/repos/<owner>/<name>".
        let Some(slug) = item.repository_url.split("/repos/").nth(1) else {
            continue;
        };
        let entry = maps.entry(slug.to_string()).or_insert(item.number);
        *entry = (*entry).min(item.number);
    }
    Ok(maps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_search_parses_slug_and_keeps_lowest_number_per_repo() {
        let body = r#"{"items": [
            {"number": 9, "repository_url": "https://api.github.com/repos/blooop/wayfinder"},
            {"number": 1, "repository_url": "https://api.github.com/repos/blooop/wayfinder"},
            {"number": 4, "repository_url": "https://api.github.com/repos/kinisi/kinisi_ros"}
        ]}"#;
        let maps = parse_map_search(body.as_bytes()).expect("parse");
        assert_eq!(maps.len(), 2);
        assert_eq!(maps["blooop/wayfinder"], 1);
        assert_eq!(maps["kinisi/kinisi_ros"], 4);
    }

    #[test]
    fn empty_search_result_means_no_maps() {
        let maps = parse_map_search(br#"{"items": []}"#).expect("parse");
        assert!(maps.is_empty());
    }
}
