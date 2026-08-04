//! Live map fetch via `gh api graphql`.
//!
//! One GraphQL query per map (per the #3 data-plane resolution): the map
//! issue, its sub-issues, and each sub-issue's `blockedBy` edges with the
//! blocker's state, so open-blocker classification needs no further calls.

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
                repo: name.to_string(),
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
