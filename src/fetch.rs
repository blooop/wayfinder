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
//! becomes [`Status::Blocked`](crate::model::Status::Blocked)'s `needs`, the
//! whole set becomes [`Ticket::blocked_by`] (the DAG), and neither is
//! re-derived from the other afterwards.
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

use crate::model::{
    classify, Activity, Checks, Map, MapId, MapSet, PrLink, PrStatus, Review, Ticket, TicketType,
};

/// The label that makes an issue a map. Both the search that *finds* maps and
/// the fetch that *reads* one test for this, so a cached number can never be
/// believed on its own (#28).
pub const MAP_LABEL: &str = "wayfinder:map";

/// The map read, in one round trip.
///
/// `pub(crate)` for one reason: [`reap`](crate::reap) selects a subset of these
/// same fields into its own batched query, and the two are held to each other
/// by a test rather than by a comment. Nothing outside that test may read it.
///
/// That tie is why the linked-PR rollup asks for `pageInfo { hasNextPage }`
/// while nothing on the screen reads the answer yet: the reaper needs it (#183
/// — a rollup it cannot see all of must not read as done-by-merge), the two
/// selections are held byte-equal, so it lands here in the same breath. The
/// screen's own use of it is its own change; an unread nullable field costs
/// this query nothing and a divergent copy would cost it the guard.
pub(crate) const MAP_QUERY: &str = "\
query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    issue(number: $number) {
      title
      state
      updatedAt
      labels(first: 100) { nodes { name } pageInfo { hasNextPage } }
      subIssues(first: 100) {
        nodes {
          number title state
          labels(first: 10) { nodes { name } }
          assignees(first: 5) { nodes { login } }
          blockedBy(first: 50) { nodes { number state } pageInfo { hasNextPage } }
          closedByPullRequestsReferences(first: 5, includeClosedPrs: true) {
            nodes {
              number state isDraft reviewDecision
              statusCheckRollup { state }
              repository { nameWithOwner }
            }
            pageInfo { hasNextPage }
          }
        }
        pageInfo { hasNextPage }
      }
    }
  }
}";

/// The envelope every `gh api graphql` answer arrives in. Generic over the
/// selection because reap batches its own query through the same shape (#129),
/// and "errors instead of data" must mean the same thing to both readers.
#[derive(Deserialize)]
pub(crate) struct GraphQlResponse<T> {
    pub(crate) data: Option<T>,
    #[serde(default)]
    pub(crate) errors: Vec<GraphQlError>,
}

#[derive(Deserialize)]
pub(crate) struct GraphQlError {
    pub(crate) message: String,
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
    /// Defaulted so a response without the selection (an older fixture) parses
    /// as "activity unknown" rather than failing the whole map — the same rule
    /// the PR selection follows.
    #[serde(rename = "updatedAt", default)]
    updated_at: Option<String>,
    labels: Paged<Label>,
    #[serde(rename = "subIssues")]
    sub_issues: Paged<SubIssue>,
}

#[derive(Deserialize)]
pub(crate) struct Nodes<T> {
    pub(crate) nodes: Vec<T>,
}

/// A connection read *with* the tracker's word on whether the page was all of
/// it (#184) — for the ticket-bearing connections, where a missing node is a
/// missing ticket or a missing blocking edge, not a cosmetic gap. [`Nodes`]
/// stays the shape for the connections whose truncation changes nothing a
/// consumer would assert on; giving every reader the flag would invite readers
/// that do not need it.
#[derive(Deserialize)]
struct Paged<T> {
    nodes: Vec<T>,
    /// Defaulted so a response without the selection (an older fixture, a
    /// GitHub edition without it) reads as "no claim of more" — the same rule
    /// every other optional selection here follows.
    #[serde(rename = "pageInfo", default)]
    page_info: PageInfo,
}

#[derive(Deserialize, Default)]
struct PageInfo {
    #[serde(rename = "hasNextPage", default)]
    has_next_page: bool,
}

impl<T> Default for Nodes<T> {
    fn default() -> Self {
        Self { nodes: Vec::new() }
    }
}

#[derive(Deserialize)]
struct SubIssue {
    number: u64,
    title: String,
    state: String,
    labels: Nodes<Label>,
    assignees: Nodes<Assignee>,
    #[serde(rename = "blockedBy")]
    blocked_by: Paged<Blocker>,
    /// Defaulted so a response without the selection (older fixtures, a
    /// GitHub edition without the field) parses as "no linked PRs" rather
    /// than failing the whole map.
    #[serde(rename = "closedByPullRequestsReferences", default)]
    closed_by_prs: Nodes<PrNode>,
}

#[derive(Deserialize)]
struct Label {
    name: String,
}

#[derive(Deserialize)]
pub(crate) struct Assignee {
    #[allow(dead_code)]
    login: String,
}

#[derive(Deserialize)]
struct Blocker {
    number: u64,
    state: String,
}

#[derive(Deserialize)]
pub(crate) struct PrNode {
    pub(crate) number: u64,
    state: String,
    #[serde(rename = "isDraft")]
    is_draft: bool,
    /// Nullable with meaning: null is "no review required" (#49).
    #[serde(rename = "reviewDecision")]
    review_decision: Option<String>,
    /// Nullable with meaning: null is "no checks configured" (#49).
    #[serde(rename = "statusCheckRollup")]
    status_check_rollup: Option<Rollup>,
    repository: RepoRef,
}

#[derive(Deserialize)]
struct Rollup {
    state: String,
}

#[derive(Deserialize)]
struct RepoRef {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
}

/// Interpret one linked PR (#52). `None` for a PR whose `state` this binary
/// does not recognise — the badge is evidence, and no badge is better than a
/// wrong one. The inner strings are open GraphQL enums too: an unknown check
/// rollup or review decision reads as "in flight", the only claim that stays
/// true whatever the new value means.
pub(crate) fn parse_pr(pr: &PrNode) -> Option<PrLink> {
    let status = match (pr.state.as_str(), pr.is_draft) {
        ("MERGED", _) => PrStatus::Merged,
        ("CLOSED", _) => PrStatus::Closed,
        ("OPEN", true) => PrStatus::Draft,
        ("OPEN", false) => PrStatus::Open {
            checks: match pr.status_check_rollup.as_ref().map(|r| r.state.as_str()) {
                None => Checks::Absent,
                Some("SUCCESS") => Checks::Passing,
                Some("FAILURE" | "ERROR") => Checks::Failing,
                Some(_) => Checks::Pending, // EXPECTED, PENDING, or newer
            },
            review: match pr.review_decision.as_deref() {
                None => Review::NotRequired,
                Some("APPROVED") => Review::Approved,
                Some("CHANGES_REQUESTED") => Review::ChangesRequested,
                Some(_) => Review::Required, // REVIEW_REQUIRED, or newer
            },
        },
        _ => return None,
    };
    Some(PrLink {
        repo: pr.repository.name_with_owner.clone(),
        number: pr.number,
        status,
    })
}

/// What the tracker's `state` string says about whether a ticket is finished
/// with — the issue-side mirror of `parse_pr` below, and the reason an
/// unrecognised state cannot reach a deleting arm.
///
/// A two-value type rather than the `bool` this used to be. The bool was read
/// as "is it open", which made **not open** the finished condition, so every
/// state GitHub adds after this binary shipped would have arrived as a reason
/// to call a ticket done — and on [`reap`](crate::reap)'s side, a reason to
/// delete a workspace. Two named values force each reading to say which it is,
/// and make the inversion that caused it a compile error rather than a `!`.
///
/// Here in `fetch` and not in `reap`, because the whole point is that there is
/// **one** reading of this wire field. `reap` deriving "finished" from the same
/// fields the badge is drawn from is a stated invariant of that module; a
/// second copy of this rule living next to the reaper is how the screen and the
/// reaper come to disagree about the same ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketState {
    /// The tracker said `CLOSED`, in those letters. The only reading that may
    /// reach `NodeFact::Closed`, which is a deleting arm.
    Closed,
    /// Open — or a state this binary does not recognise, which is held to the
    /// same standard the PR reading is: the arm that stays true whatever the
    /// new value turns out to mean is the one that keeps the workspace and
    /// leaves the ticket on the screen. An unknown state therefore costs a
    /// ticket that stays, never one that silently goes.
    Live,
}

impl TicketState {
    /// Read the tracker's `state`. Positive about *closed*, and not the
    /// negation of "open": the two differ exactly on the values neither list
    /// names, and that difference is whether a workspace survives them.
    pub fn read(state: &str) -> TicketState {
        if state == "CLOSED" {
            TicketState::Closed
        } else {
            TicketState::Live
        }
    }
}

/// Whether a ticket is still live, for the readers that want it as a `bool`.
///
/// Delegates rather than restating, so the screen and the reaper cannot drift:
/// [`model::classify`](crate::model::classify) turns `false` into
/// `Status::Done`, which is the display-side twin of reap's deleting arm.
pub(crate) fn is_open(state: &str) -> bool {
    TicketState::read(state) == TicketState::Live
}

/// Fetch one map live: the map issue named by `id`, its sub-issues, and their
/// blocking edges — one `gh api graphql` round trip.
///
/// # Errors
///
/// A malformed repo slug, a `gh` that is missing or unauthenticated, a network
/// failure, or a response that does not parse. Every one of them is the same
/// thing to the caller — the cluster for this map does not arrive — and
/// [`refresh`](crate::refresh) turns it into the failure note on screen rather
/// than an exit.
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
    let resp: GraphQlResponse<ResponseData> =
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
    //
    // The label half of the proof needs the tracker's *whole* answer: a label
    // page cut short cannot prove the label is gone, only a complete page
    // without it can (#184). Without that qualifier, a map wearing more labels
    // than one page holds was rejected here as "no longer a map" on every
    // refresh, while discovery's label-scoped search — which no page cap can
    // blind — kept re-finding it.
    let map_label_disproven = !issue.labels.nodes.iter().any(|l| l.name == MAP_LABEL)
        && !issue.labels.page_info.has_next_page;
    if !is_open(&issue.state) || map_label_disproven {
        bail!(
            "{}#{} is no longer an open `{MAP_LABEL}` issue",
            id.repo,
            id.number
        );
    }

    // Truncation folds over the whole read: the sub-issue page itself, and
    // every ticket's blocker page. Any one of them cut short means the tree —
    // or the classification drawn from its edges — is partial (#184).
    let mut truncated = issue.sub_issues.page_info.has_next_page;
    let mut tickets: Vec<Ticket> = issue
        .sub_issues
        .nodes
        .into_iter()
        .map(|sub| {
            truncated |= sub.blocked_by.page_info.has_next_page;
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
            let prs: Vec<PrLink> = sub
                .closed_by_prs
                .nodes
                .iter()
                .filter_map(parse_pr)
                .collect();
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
                prs,
            }
        })
        .collect();
    tickets.sort_by_key(|t| t.number);

    Ok(Map {
        title: issue.title,
        // Interpreted here and nowhere inward, like every other tracker string.
        last_activity: issue.updated_at.as_deref().and_then(Activity::parse),
        tickets,
        truncated,
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
///
/// # Errors
///
/// The search itself failing: no `gh`, no credentials, no network, or a
/// response that does not parse. An empty `repos` is not an error — it is an
/// empty [`MapSet`], which is what a machine with nothing discovered yet has.
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

    /// One map whose sub-issue and whose blocker both carry a `state` this
    /// binary was never taught. `TRANSFERRED` is GitHub's own third issue
    /// state; the point is that it stands for whatever the fourth turns out
    /// to be.
    const UNREADABLE_STATE_RESPONSE: &str = r#"{"data": {"repository": {"issue": {
        "title": "Map: wf",
        "state": "OPEN",
        "labels": {"nodes": [{"name": "wayfinder:map"}]},
        "subIssues": {"nodes": [
            {"number": 19, "title": "Build 6", "state": "TRANSFERRED",
             "labels": {"nodes": [{"name": "wayfinder:task"}]},
             "assignees": {"nodes": []},
             "blockedBy": {"nodes": []}},
            {"number": 21, "title": "Blocked on it", "state": "OPEN",
             "labels": {"nodes": [{"name": "wayfinder:task"}]},
             "assignees": {"nodes": []},
             "blockedBy": {"nodes": [{"number": 19, "state": "TRANSFERRED"}]}}
        ]}
    }}}}"#;

    #[test]
    fn only_the_word_closed_reads_as_closed() {
        // The one place the tracker's `state` is turned into a fact, for the
        // screen and for the reaper alike. `Closed` routes to `Status::Done`
        // here and to `NodeFact::Closed` — a deletion — there, so what may
        // produce it is a closed list of one string rather than "anything that
        // is not the word OPEN". The states below are the shapes an answer can
        // actually take that this binary was never taught: GitHub's own third
        // issue state, a plausible future one, the empty string a defaulted
        // field would leave, and a lowercasing of the real value.
        assert_eq!(TicketState::read("CLOSED"), TicketState::Closed);
        for unknown in ["OPEN", "TRANSFERRED", "DUPLICATE", "", "closed", "Closed"] {
            assert_eq!(
                TicketState::read(unknown),
                TicketState::Live,
                "state {unknown:?} must not read as closed"
            );
            assert!(
                is_open(unknown),
                "state {unknown:?} must still read as live"
            );
        }
        assert!(!is_open("CLOSED"));
    }

    #[test]
    fn a_state_this_binary_cannot_read_neither_finishes_a_ticket_nor_unblocks_one() {
        // The screen's half of the same rule, and the reason the reading lives
        // in this module rather than beside the reaper. Both directions are
        // silent failures: a ticket wrongly shown Done drops out of the
        // frontier and is never offered, and a blocker wrongly read as settled
        // offers work that cannot actually be started. Neither prints an
        // error, and until this test nothing looked at either.
        let map = parse_map(UNREADABLE_STATE_RESPONSE.as_bytes(), &wf_map_id()).expect("parse");

        let unreadable = map.tickets.iter().find(|t| t.number == 19).expect("#19");
        assert_ne!(
            unreadable.status,
            Status::Done,
            "a state this binary cannot read is not evidence the ticket is finished"
        );
        assert_eq!(unreadable.status, Status::Frontier);

        let blocked = map.tickets.iter().find(|t| t.number == 21).expect("#21");
        assert_eq!(
            blocked.status,
            Status::Blocked { needs: vec![19] },
            "an unreadable blocker still blocks"
        );
    }

    fn wf_map_id() -> MapId {
        MapId::new("blooop/wayfinder", 1)
    }

    #[test]
    fn the_map_parse_carries_each_sub_issues_type_through_from_its_labels() {
        let map = parse_map(MAP_RESPONSE.as_bytes(), &wf_map_id()).expect("parse");
        assert_eq!(map.title, "Map: wf");
        let types: Vec<(u64, TicketType)> = map
            .tickets
            .iter()
            .map(|t| (t.number, t.ticket_type))
            .collect();
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
        assert_eq!(
            t19.status,
            Status::Frontier,
            "a closed blocker doesn't block"
        );
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
    fn a_maps_own_label_count_can_never_unmap_it() {
        // The worst of #184's three findings: a map wearing more labels than
        // one page holds, with `wayfinder:map` not on the page the fetch got,
        // used to fail re-verification as "no longer a map" — while discovery,
        // whose label-scoped search is immune to the cap, kept re-finding it.
        // Permanently, on every refresh.
        //
        // An unseen label on a *truncated* page proves nothing, so it keeps
        // the map. Only a complete page without the label — the tracker's
        // whole answer — may reject.
        let body = map_response_with("OPEN", r#"{"name": "enhancement"}"#).replacen(
            r#""labels": {"nodes": [{"name": "enhancement"}]},"#,
            r#""labels": {"nodes": [{"name": "enhancement"}], "pageInfo": {"hasNextPage": true}},"#,
            1,
        );
        let map = parse_map(body.as_bytes(), &wf_map_id())
            .expect("a label page the tracker cut short cannot prove the label is gone");
        assert_eq!(map.title, "Map: wf", "the map renders as itself");

        // And the label page being cut short is not the *tree* being cut
        // short: nothing ticket-bearing was truncated here.
        assert!(!map.truncated, "a truncated label page is not a partial tree");
    }

    /// One sub-issue carrying the full PR-badge matrix (#52).
    const PR_RESPONSE: &str = r#"{"data": {"repository": {"issue": {
        "title": "Map: wf",
        "state": "OPEN",
        "updatedAt": "2026-08-06T12:34:56Z",
        "labels": {"nodes": [{"name": "wayfinder:map"}]},
        "subIssues": {"nodes": [
            {"number": 30, "title": "Raw tty leak", "state": "OPEN",
             "labels": {"nodes": []},
             "assignees": {"nodes": []},
             "blockedBy": {"nodes": []},
             "closedByPullRequestsReferences": {"nodes": [
                {"number": 33, "state": "MERGED", "isDraft": false,
                 "reviewDecision": null, "statusCheckRollup": {"state": "SUCCESS"},
                 "repository": {"nameWithOwner": "blooop/wayfinder"}},
                {"number": 40, "state": "OPEN", "isDraft": false,
                 "reviewDecision": "CHANGES_REQUESTED", "statusCheckRollup": {"state": "FAILURE"},
                 "repository": {"nameWithOwner": "blooop/wayfinder"}},
                {"number": 41, "state": "OPEN", "isDraft": false,
                 "reviewDecision": null, "statusCheckRollup": null,
                 "repository": {"nameWithOwner": "blooop/dotfiles"}},
                {"number": 42, "state": "OPEN", "isDraft": true,
                 "reviewDecision": "REVIEW_REQUIRED", "statusCheckRollup": {"state": "PENDING"},
                 "repository": {"nameWithOwner": "blooop/wayfinder"}},
                {"number": 43, "state": "CLOSED", "isDraft": false,
                 "reviewDecision": null, "statusCheckRollup": null,
                 "repository": {"nameWithOwner": "blooop/wayfinder"}},
                {"number": 44, "state": "SOMETHING_NEW", "isDraft": false,
                 "reviewDecision": null, "statusCheckRollup": null,
                 "repository": {"nameWithOwner": "blooop/wayfinder"}}
             ]}}
        ]}
    }}}}"#;

    #[test]
    fn linked_prs_parse_into_badge_facts_at_the_boundary() {
        let map = parse_map(PR_RESPONSE.as_bytes(), &wf_map_id()).expect("parse");
        let prs = &map.tickets[0].prs;
        assert_eq!(
            prs,
            &vec![
                // Merged wins over whatever the rollup says: it is history.
                PrLink {
                    repo: "blooop/wayfinder".to_string(),
                    number: 33,
                    status: PrStatus::Merged,
                },
                PrLink {
                    repo: "blooop/wayfinder".to_string(),
                    number: 40,
                    status: PrStatus::Open {
                        checks: Checks::Failing,
                        review: Review::ChangesRequested,
                    },
                },
                // Nulls mean things (#49): no checks configured, no review
                // required — not missing data.
                PrLink {
                    repo: "blooop/dotfiles".to_string(),
                    number: 41,
                    status: PrStatus::Open {
                        checks: Checks::Absent,
                        review: Review::NotRequired,
                    },
                },
                // Draft is parsed with state, not left as a flag to remember.
                PrLink {
                    repo: "blooop/wayfinder".to_string(),
                    number: 42,
                    status: PrStatus::Draft,
                },
                PrLink {
                    repo: "blooop/wayfinder".to_string(),
                    number: 43,
                    status: PrStatus::Closed,
                },
                // #44's unrecognised state produced no badge at all.
            ]
        );
    }

    #[test]
    fn a_response_without_the_pr_selection_still_parses() {
        // MAP_RESPONSE predates the #52 selection: absent connection, no PRs.
        let map = parse_map(MAP_RESPONSE.as_bytes(), &wf_map_id()).expect("parse");
        assert!(map.tickets.iter().all(|t| t.prs.is_empty()));
    }

    #[test]
    fn the_map_issues_own_timestamp_becomes_its_last_activity() {
        // The cluster sort key, parsed at the boundary like every other tracker
        // string — nothing inward ever sees the ISO-8601 text.
        let map = parse_map(PR_RESPONSE.as_bytes(), &wf_map_id()).expect("parse");
        assert_eq!(map.last_activity, Activity::parse("2026-08-06T12:34:56Z"));
        assert!(map.last_activity.is_some(), "the fixture stamp parsed");
        // An absent selection is "activity unknown", not a fetch failure:
        // MAP_RESPONSE predates the field and still yields a usable map.
        let old = parse_map(MAP_RESPONSE.as_bytes(), &wf_map_id()).expect("parse");
        assert_eq!(old.last_activity, None);
        assert_eq!(old.tickets.len(), 3, "the rest of the map is unaffected");
    }

    /// The tracker's own word that a page was not all of it, on each
    /// ticket-bearing connection in turn — the two ways a map can arrive
    /// silently partial (#184).
    fn truncated_response(sub_issues_more: bool, blockers_more: bool) -> String {
        format!(
            r#"{{"data": {{"repository": {{"issue": {{
            "title": "Map: wf",
            "state": "OPEN",
            "labels": {{"nodes": [{{"name": "wayfinder:map"}}]}},
            "subIssues": {{
                "nodes": [
                    {{"number": 19, "title": "Build 6", "state": "OPEN",
                     "labels": {{"nodes": []}},
                     "assignees": {{"nodes": []}},
                     "blockedBy": {{"nodes": [{{"number": 3, "state": "OPEN"}}],
                                   "pageInfo": {{"hasNextPage": {blockers_more}}}}}}}
                ],
                "pageInfo": {{"hasNextPage": {sub_issues_more}}}
            }}
        }}}}}}}}"#
        )
    }

    #[test]
    fn a_map_the_tracker_could_not_send_all_of_says_it_arrived_truncated() {
        // A 101st sub-issue or a 51st blocker does not fit the page, and until
        // #184 the map rendered without it and without a trace. The parse now
        // keeps the tracker's own word on it: either connection reporting a
        // next page marks the whole map truncated.
        for (subs, blockers) in [(true, false), (false, true), (true, true)] {
            let map = parse_map(truncated_response(subs, blockers).as_bytes(), &wf_map_id())
                .expect("a truncated map still parses — partial beats absent");
            assert!(
                map.truncated,
                "subIssues more: {subs}, blockers more: {blockers} — the map must say so"
            );
        }
        let map = parse_map(truncated_response(false, false).as_bytes(), &wf_map_id())
            .expect("parse");
        assert!(!map.truncated, "full pages are not a truncation");
    }

    #[test]
    fn a_response_without_page_info_reads_as_complete() {
        // Older fixtures and GitHub editions without the selection: absent is
        // "no claim of more", not "more" — the same defaulting rule the PR
        // selection follows.
        let map = parse_map(MAP_RESPONSE.as_bytes(), &wf_map_id()).expect("parse");
        assert!(!map.truncated);
    }

    /// The brace-balanced block a named connection selects in `query` —
    /// `field(…) { … }` from the first `{` to its close. Panics if the field
    /// is missing or its braces never balance, because a guard that returns
    /// `None` quietly is a guard that stops guarding when the query is
    /// reworded.
    fn connection_block<'q>(query: &'q str, field: &str) -> &'q str {
        let from = query
            .find(field)
            .unwrap_or_else(|| panic!("the query no longer selects {field}"));
        let mut depth = 0usize;
        for (offset, ch) in query[from..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth = depth
                        .checked_sub(1)
                        .unwrap_or_else(|| panic!("unbalanced braces in {query}"));
                    if depth == 0 {
                        return &query[from..=from + offset];
                    }
                }
                _ => {}
            }
        }
        panic!("{field}'s block never closes in {query}")
    }

    /// Whether `block` selects `pageInfo` on the connection itself — at brace
    /// depth 1, not anywhere in the nested tree. Depth matters: the sub-issue
    /// block *contains* the linked-PR block, whose own `pageInfo` (#183) would
    /// otherwise satisfy this test on behalf of a connection that never asked.
    fn asks_its_own_page_info(block: &str) -> bool {
        let mut depth = 0usize;
        let mut at = 0;
        while let Some(found) = block[at..].find("pageInfo") {
            depth += block[at..at + found].matches('{').count();
            depth -= block[at..at + found].matches('}').count();
            if depth == 1 {
                return true;
            }
            at += found + "pageInfo".len();
        }
        false
    }

    #[test]
    fn the_query_asks_for_every_page_boundary_the_parse_reads() {
        // The truncation tests above feed `parse_map` hand-written fixtures,
        // so they cannot notice the live query never requesting the field —
        // the same hole #132 closed for reap's batch, guarded the same way:
        // against the query text. Drop `pageInfo` from either ticket-bearing
        // connection and every map fetched live reads as complete forever,
        // silently, which is exactly the pre-#184 behaviour.
        for field in ["subIssues", "blockedBy"] {
            let block = connection_block(MAP_QUERY, field);
            assert!(
                asks_its_own_page_info(block),
                "{field} no longer asks whether its page was all of it: {block}"
            );
        }
    }

    #[test]
    fn the_maps_own_label_page_is_as_deep_as_the_tracker_allows() {
        // The other half of the un-mapping fix: the parse forgives a truncated
        // label page, and the query makes truncation implausible in the first
        // place — 100 is the GraphQL page maximum, five times the old cap that
        // a label-heavy map actually overflowed. The first `labels` selection
        // in the query is the map issue's own, the one re-verification reads.
        let block = connection_block(MAP_QUERY, "labels");
        assert!(
            block.starts_with("labels(first: 100)"),
            "the map's label page shrank below the tracker's maximum: {block}"
        );
        assert!(
            asks_its_own_page_info(block),
            "the label page no longer says whether it was all of it: {block}"
        );
    }

    #[test]
    fn empty_search_result_means_no_maps() {
        let maps = parse_map_search(br#"{"items": []}"#).expect("parse");
        assert!(maps.is_empty());
    }
}
