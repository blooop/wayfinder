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

use std::cmp::Reverse;
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use tokio::process::Command;

use crate::model::{
    classify, Activity, Checks, Cluster, MapId, MapSet, PrLink, PrStatus, Review, Ticket,
    TicketType,
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

/// The inbox read: every open issue in the cached repos that is **yours or
/// nobody's**, in one round trip.
///
/// Two searches, not every open issue, and that is the shape of the feature
/// rather than a filter on it. A repo's whole open issue list is not curated —
/// dependabot, other people's work in flight, stale requests — and pouring it
/// in would not make a bigger wayfinder, it would bury the charted maps under
/// a worse `gh issue list`. What is left after dropping *somebody else's*
/// issues is the set you could actually pick up.
///
/// **Two searches because GitHub issue search has no `OR`.** Qualifiers `AND`
/// together, so `assignee:@me no:assignee` is a contradiction that matches
/// nothing. The two sets are disjoint by construction — an issue cannot be
/// both assigned to you and unassigned — so they need no dedup against each
/// other, only the same subtraction against the maps that every inbox row
/// gets. Aliased into one query rather than sent as two, so the cost stays one
/// round trip and one rate-limit point.
///
/// The selection is deliberately the *same* one [`MAP_QUERY`] makes of a map's
/// sub-issues, plus the two things a row outside a map has to carry itself:
/// which repo it is in, and when it was touched. Same selection means
/// [`parse_ticket`] reads both, so an issue cannot render one way in a map and
/// another in the inbox. It is a GraphQL fragment for that reason — one
/// selection written once, spread into both searches, so the two halves of the
/// inbox cannot drift from each other either.
///
/// `is:issue` is in both query strings because GitHub's `type: ISSUE` search
/// covers pull requests too, and a PR is not a ticket.
const INBOX_QUERY: &str = "\
fragment Row on Issue {
  number title state updatedAt
  repository { nameWithOwner }
  labels(first: 100) { nodes { name } pageInfo { hasNextPage } }
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
query($mine: String!, $unassigned: String!) {
  mine: search(query: $mine, type: ISSUE, first: 100) {
    nodes { ...Row }
    pageInfo { hasNextPage }
  }
  unassigned: search(query: $unassigned, type: ISSUE, first: 100) {
    nodes { ...Row }
    pageInfo { hasNextPage }
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
    /// Paged rather than bare nodes because the *inbox* reads the page flag:
    /// there, the map label's **absence** is what keeps a map issue out of its
    /// own inbox, and a page cut short cannot prove an absence (#184). A map's
    /// own sub-issues are read from the same struct and do not consult it — a
    /// label page cut short there costs a `[type]` suffix, not a row drawn
    /// twice — which is why [`MAP_QUERY`] does not ask for the flag and
    /// `Paged`'s default reads it as "no claim of more".
    labels: Paged<Label>,
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
pub async fn fetch_map(id: &MapId) -> Result<Cluster> {
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

/// Turn one `gh api graphql` response body into a [`Cluster`] — the whole parse
/// boundary, kept apart from the process call so it is testable without the
/// network. Every raw tracker string a ticket is derived from (state,
/// assignees, blocker states, labels) is interpreted exactly here.
fn parse_map(body: &[u8], id: &MapId) -> Result<Cluster> {
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
            parse_ticket(sub, &id.repo)
        })
        .collect();
    tickets.sort_by_key(|t| t.number);

    Ok(Cluster::map(
        issue.title,
        // Interpreted here and nowhere inward, like every other tracker string.
        issue.updated_at.as_deref().and_then(Activity::parse),
        tickets,
        truncated,
    ))
}

/// The two aliased searches, as the response carries them.
#[derive(Deserialize)]
struct InboxData {
    mine: Paged<InboxIssue>,
    unassigned: Paged<InboxIssue>,
}

/// One inbox row as the search answers: an ordinary issue selection plus the
/// two facts a row outside a map cannot borrow from one.
#[derive(Deserialize)]
struct InboxIssue {
    #[serde(flatten)]
    issue: SubIssue,
    /// Defaulted for the same reason every other optional selection here is:
    /// a response without it reads as "activity unknown" rather than failing
    /// the whole inbox.
    #[serde(rename = "updatedAt", default)]
    updated_at: Option<String>,
    repository: RepoRef,
}

/// Fetch the inbox live: every open issue across `repos` that is assigned to
/// the viewer or to nobody, grouped into one [`Cluster`] per repo — one
/// `gh api graphql` round trip.
///
/// Repos with nothing to show are simply absent, so a machine with a clean
/// inbox renders no inbox clusters at all rather than a row of empty headings.
///
/// Map issues themselves are dropped here: a `wayfinder:map` issue is a cluster
/// header on this screen, and listing it as a row of the inbox would draw the
/// same issue twice in two different meanings. Its *tickets* are dropped
/// elsewhere and deliberately so — see the fold in `picker`, which subtracts
/// them as the maps land, because this query cannot know which issues a map
/// has claimed.
///
/// # Errors
///
/// A `gh` that is missing or unauthenticated, a network failure, or a response
/// that does not parse. An empty `repos` is not an error — it is an empty
/// inbox, which is what a machine with nothing discovered yet has.
pub async fn fetch_inbox(repos: &[String]) -> Result<Vec<(String, Cluster)>> {
    if repos.is_empty() {
        return Ok(Vec::new());
    }
    let scope: Vec<String> = repos.iter().map(|r| format!("repo:{r}")).collect();
    // The two halves differ in exactly one qualifier, so they are built from
    // one string: a scope that drifted between them would silently make the
    // inbox mean two different things in its two halves.
    // `sort:updated-desc` is what makes the page cap survivable: one page of
    // 100 spans every repo asked about, so without an order *which* rows fall
    // off it is arbitrary — and the rows worth having are the ones something
    // just happened to. It also means the order the screen wants is the order
    // the tracker already ranked, rather than something recovered afterwards.
    let scoped = |who: &str| {
        format!(
            "{who} is:issue is:open sort:updated-desc {}",
            scope.join(" ")
        )
    };
    let output = Command::new("gh")
        .args([
            "api",
            "graphql",
            "-f",
            &format!("mine={}", scoped("assignee:@me")),
            "-f",
            &format!("unassigned={}", scoped("no:assignee")),
            "-f",
            &format!("query={INBOX_QUERY}"),
        ])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .context("failed to run `gh` for the inbox")?;
    if !output.status.success() {
        bail!(
            "`gh api graphql` failed for the inbox: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    parse_inbox(&output.stdout)
}

/// One repo's inbox mid-build.
///
/// Each row is kept beside its **own** stamp, because the inbox is ordered by
/// when something happened and a [`Ticket`] carries no time of its own — a
/// map's rows are numbered in the order its author charted them, so a ticket
/// has never needed one. `activity` is the cluster's stamp (the newest of the
/// rows'), which is a different question from any one row's.
struct Building {
    rows: Vec<(Option<Activity>, Ticket)>,
    activity: Option<Activity>,
    truncated: bool,
}

/// Turn one inbox response into one cluster per repo — the whole parse
/// boundary, kept apart from the process call so it is testable without the
/// network.
///
/// Truncation is per repo and folds the same way a map's does: the search page
/// being cut short means *every* repo's rows may be partial, because one page
/// spans them all and there is no way to tell which repo lost rows. Saying so
/// on all of them is the honest answer; picking one would be a guess.
fn parse_inbox(body: &[u8]) -> Result<Vec<(String, Cluster)>> {
    let resp: GraphQlResponse<InboxData> =
        serde_json::from_slice(body).context("unparseable GraphQL response from gh")?;
    if let Some(err) = resp.errors.first() {
        bail!("GraphQL error: {}", err.message);
    }
    let data = resp.data.context("no data in the inbox response")?;
    // Either half being cut short means rows are missing, and neither says
    // which repo lost them — so both halves fold into one claim, the same way
    // one search's page already does across the repos it spans.
    let page_cut_short =
        data.mine.page_info.has_next_page || data.unassigned.page_info.has_next_page;

    let mut by_repo: std::collections::BTreeMap<String, Building> =
        std::collections::BTreeMap::new();
    // Concatenated rather than merged: the two searches are disjoint by
    // construction — an issue is assigned to you or it is assigned to nobody,
    // never both — so there is nothing here to deduplicate. What each row
    // *is* still comes from its own `assignees` selection through
    // `parse_ticket`, not from which half it arrived in, so a row does not
    // depend on the query that found it.
    for node in data.mine.nodes.into_iter().chain(data.unassigned.nodes) {
        // A map is a heading on this screen, not a row — and a map is an open
        // issue nobody is assigned to, so `no:assignee` finds every one of
        // them. This is the only thing keeping them out.
        //
        // So the *absence* of the label has to be proven, not assumed (#184):
        // a page the tracker cut short cannot prove a label is gone, and a map
        // drawn as a row is a row offering to adopt a map. Unproven means
        // dropped, which costs at worst one heavily-labelled issue that has to
        // be reached from its own map — where a wrongly-drawn map row would
        // cost a reparenting.
        let not_a_map = !node.issue.labels.nodes.iter().any(|l| l.name == MAP_LABEL)
            && !node.issue.labels.page_info.has_next_page;
        if !not_a_map {
            continue;
        }
        let repo = node.repository.name_with_owner.clone();
        let activity = node.updated_at.as_deref().and_then(Activity::parse);
        let blockers_cut_short = node.issue.blocked_by.page_info.has_next_page;
        let ticket = parse_ticket(node.issue, &repo);
        let entry = by_repo.entry(repo).or_insert_with(|| Building {
            rows: Vec::new(),
            activity: None,
            truncated: page_cut_short,
        });
        entry.rows.push((activity, ticket));
        // The cluster's stamp is the newest of its rows': a pile of issues has
        // no single thing that was updated, and the order the screen wants is
        // "where has something happened".
        entry.activity = entry.activity.max(activity);
        entry.truncated |= blockers_cut_short;
    }

    Ok(by_repo
        .into_iter()
        .map(
            |(
                repo,
                Building {
                    mut rows,
                    activity,
                    truncated,
                },
            )| {
                // Newest first, and **not** by number: ascending number order puts
                // the oldest issue in the repo at the top, which is the inversion
                // of what an inbox is for. A stamp that did not parse sorts last
                // rather than being guessed into place — `None < Some` reversed —
                // the same answer the cluster order gives an unreadable map stamp.
                //
                // The number breaks ties newest-first too, so two issues touched in
                // the same second do not render in an order that shifts between
                // frames.
                rows.sort_by_key(|(stamp, ticket)| (Reverse(*stamp), Reverse(ticket.number)));
                let tickets = rows.into_iter().map(|(_, ticket)| ticket).collect();
                (repo, Cluster::inbox(activity, tickets, truncated))
            },
        )
        .collect())
}

/// Turn one issue selection into a [`Ticket`] — the only place the tracker's
/// raw strings for an issue become a ticket, whether that issue arrived as a
/// map's sub-issue or as a row of the inbox.
///
/// One function rather than one per caller: the two reads select the same
/// fields, and a second copy of this is how the inbox and a map would come to
/// disagree about the same issue's status. `repo` is passed in because the two
/// callers know it differently — a sub-issue takes its map's repo, an inbox
/// row carries its own.
fn parse_ticket(sub: SubIssue, repo: &str) -> Ticket {
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
        repo: repo.to_string(),
        number: sub.number,
        title: sub.title,
        status: classify(
            is_open(&sub.state),
            !sub.assignees.nodes.is_empty(),
            open_blockers,
        ),
        ticket_type: TicketType::from_labels(sub.labels.nodes.iter().map(|l| l.name.as_str())),
        blocked_by,
        prs,
    }
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

    /// One inbox response, shaped exactly like the live one: both aliased
    /// searches, two repos, a `wayfinder:map` issue that must not become a row,
    /// and a blocker page the tracker cut short.
    ///
    /// The `unassigned` half deliberately carries a map issue, because that is
    /// what the live one carries: a map is an open issue nobody is assigned to,
    /// so `no:assignee` finds every map in every repo asked about.
    const INBOX_RESPONSE: &str = r#"{"data": {
      "mine": {
        "nodes": [
          {"number": 191, "title": "Non-ASCII titles", "state": "OPEN",
           "updatedAt": "2026-08-22T16:06:12Z",
           "repository": {"nameWithOwner": "blooop/wayfinder"},
           "labels": {"nodes": [{"name": "wayfinder:build"}]},
           "assignees": {"nodes": [{"login": "blooop"}]},
           "blockedBy": {"nodes": [], "pageInfo": {"hasNextPage": false}},
           "closedByPullRequestsReferences": {
             "nodes": [{"number": 203, "state": "OPEN", "isDraft": false,
                        "reviewDecision": null,
                        "statusCheckRollup": {"state": "SUCCESS"},
                        "repository": {"nameWithOwner": "blooop/wayfinder"}}],
             "pageInfo": {"hasNextPage": false}}},
          {"number": 190, "title": "A red run files an issue", "state": "OPEN",
           "updatedAt": "2026-08-24T09:00:00Z",
           "repository": {"nameWithOwner": "blooop/wayfinder"},
           "labels": {"nodes": []},
           "assignees": {"nodes": [{"login": "blooop"}]},
           "blockedBy": {"nodes": [{"number": 12, "state": "OPEN"}],
                         "pageInfo": {"hasNextPage": true}},
           "closedByPullRequestsReferences": {"nodes": [], "pageInfo": {"hasNextPage": false}}},
          {"number": 4, "title": "Pin the toolchain", "state": "OPEN",
           "updatedAt": "2026-08-20T00:00:00Z",
           "repository": {"nameWithOwner": "blooop/dotfiles"},
           "labels": {"nodes": []},
           "assignees": {"nodes": [{"login": "blooop"}]},
           "blockedBy": {"nodes": [], "pageInfo": {"hasNextPage": false}},
           "closedByPullRequestsReferences": {"nodes": [], "pageInfo": {"hasNextPage": false}}}
        ],
        "pageInfo": {"hasNextPage": false}
      },
      "unassigned": {
        "nodes": [
          {"number": 207, "title": "test ticket", "state": "OPEN",
           "updatedAt": "2026-08-24T10:00:00Z",
           "repository": {"nameWithOwner": "blooop/wayfinder"},
           "labels": {"nodes": []},
           "assignees": {"nodes": []},
           "blockedBy": {"nodes": [], "pageInfo": {"hasNextPage": false}},
           "closedByPullRequestsReferences": {"nodes": [], "pageInfo": {"hasNextPage": false}}},
          {"number": 7, "title": "Map: the other project", "state": "OPEN",
           "updatedAt": "2026-08-23T00:00:00Z",
           "repository": {"nameWithOwner": "blooop/dotfiles"},
           "labels": {"nodes": [{"name": "wayfinder:map"}]},
           "assignees": {"nodes": []},
           "blockedBy": {"nodes": [], "pageInfo": {"hasNextPage": false}},
           "closedByPullRequestsReferences": {"nodes": [], "pageInfo": {"hasNextPage": false}}}
        ],
        "pageInfo": {"hasNextPage": false}
      }
    }}"#;

    #[test]
    fn a_map_wearing_more_labels_than_one_page_holds_is_still_not_a_row() {
        // The #184 shape, on the inbox side. A map is an open issue nobody is
        // assigned to, so `no:assignee` finds every one of them, and the only
        // thing keeping a map out of its own inbox is the label check. A label
        // page cut short cannot prove the label is gone — so it does not get to
        // — and the row is dropped rather than drawn as an issue offering to
        // adopt a map.
        let body = INBOX_RESPONSE.replace(
            r#""labels": {"nodes": [{"name": "wayfinder:map"}]}"#,
            r#""labels": {"nodes": [{"name": "bug"}], "pageInfo": {"hasNextPage": true}}"#,
        );
        assert_ne!(body, INBOX_RESPONSE, "the fixture's map label moved");
        let inbox = parse_inbox(body.as_bytes()).expect("parse");
        let dotfiles = inbox.iter().find(|(repo, _)| repo == "blooop/dotfiles");
        let numbers: Vec<u64> = dotfiles
            .map(|(_, cluster)| cluster.tickets.iter().map(|t| t.number).collect())
            .unwrap_or_default();
        assert!(
            !numbers.contains(&7),
            "#7's label page was cut short, so it cannot be proven not to be a map: {numbers:?}"
        );
    }

    #[test]
    fn a_complete_label_page_without_the_map_label_is_an_ordinary_row() {
        // The other half, so the rule above is not just "drop anything with
        // more labels than we asked for": a page the tracker says is all of it,
        // with no map label on it, is proof.
        let body = INBOX_RESPONSE.replace(
            r#""labels": {"nodes": [{"name": "wayfinder:map"}]}"#,
            r#""labels": {"nodes": [{"name": "bug"}], "pageInfo": {"hasNextPage": false}}"#,
        );
        let inbox = parse_inbox(body.as_bytes()).expect("parse");
        let numbers: Vec<u64> = inbox
            .iter()
            .find(|(repo, _)| repo == "blooop/dotfiles")
            .expect("dotfiles has rows")
            .1
            .tickets
            .iter()
            .map(|t| t.number)
            .collect();
        assert!(numbers.contains(&7), "{numbers:?}");
    }

    #[test]
    fn the_inbox_holds_what_is_yours_and_what_is_nobodys_in_one_cluster() {
        // The two searches exist only because GitHub issue search has no `OR`;
        // downstream there is one inbox per repo, and which half a row arrived
        // in is not a fact anything keeps.
        let inbox = parse_inbox(INBOX_RESPONSE.as_bytes()).expect("parse");
        let wayfinder = &inbox
            .iter()
            .find(|(repo, _)| repo == "blooop/wayfinder")
            .expect("wayfinder has rows")
            .1;
        let numbers: Vec<u64> = wayfinder.tickets.iter().map(|t| t.number).collect();
        assert_eq!(
            numbers,
            vec![207, 190, 191],
            "both halves land in one cluster, newest first"
        );
    }

    #[test]
    fn inbox_rows_come_back_newest_first() {
        // The inbox is ordered by *when something happened*, not by issue
        // number. A map's rows are numbered in the order its author charted
        // them, so number order is meaningful there; an inbox is a pile with no
        // author, and ascending number order puts the oldest thing in the repo
        // at the top — the exact inversion of what an inbox is for.
        let inbox = parse_inbox(INBOX_RESPONSE.as_bytes()).expect("parse");
        let wayfinder = &inbox
            .iter()
            .find(|(repo, _)| repo == "blooop/wayfinder")
            .expect("wayfinder has rows")
            .1;
        let numbers: Vec<u64> = wayfinder.tickets.iter().map(|t| t.number).collect();
        // #207 10:00 > #190 09:00 > #191 (two days earlier).
        assert_eq!(numbers, vec![207, 190, 191]);
    }

    #[test]
    fn an_inbox_row_whose_stamp_did_not_parse_sorts_last_rather_than_first() {
        // Unknown activity is not guessed into place — the same answer the
        // cluster order gives a map whose stamp did not parse. Sorting it first
        // would put whatever the tracker garbled at the top of the inbox.
        let body = INBOX_RESPONSE.replace(
            r#""updatedAt": "2026-08-24T10:00:00Z""#,
            r#""updatedAt": "not a timestamp""#,
        );
        assert_ne!(body, INBOX_RESPONSE, "the fixture's #207 stamp moved");
        let inbox = parse_inbox(body.as_bytes()).expect("parse");
        let numbers: Vec<u64> = inbox
            .iter()
            .find(|(repo, _)| repo == "blooop/wayfinder")
            .expect("wayfinder has rows")
            .1
            .tickets
            .iter()
            .map(|t| t.number)
            .collect();
        assert_eq!(numbers, vec![190, 191, 207], "#207's stamp is unreadable");
    }

    #[test]
    fn an_inbox_rows_status_comes_from_its_assignees_not_from_which_search_found_it() {
        // The reason both halves go through `parse_ticket`: an unassigned issue
        // is a *frontier* row and an assigned one is *claimed*, and that is the
        // difference the glyph column draws. Deriving it from the alias would
        // work today and be wrong the moment either query changes.
        let inbox = parse_inbox(INBOX_RESPONSE.as_bytes()).expect("parse");
        let by_number = |n: u64| {
            inbox
                .iter()
                .flat_map(|(_, cluster)| &cluster.tickets)
                .find(|t| t.number == n)
                .unwrap_or_else(|| panic!("#{n} is in the inbox"))
                .clone()
        };
        assert_eq!(by_number(191).status, Status::Claimed, "assigned to me");
        assert_eq!(
            by_number(207).status,
            Status::Frontier,
            "assigned to nobody"
        );
    }

    #[test]
    fn the_inbox_groups_by_repo_and_orders_each_cluster_by_its_newest_row() {
        let inbox = parse_inbox(INBOX_RESPONSE.as_bytes()).expect("parse");
        let repos: Vec<&str> = inbox.iter().map(|(repo, _)| repo.as_str()).collect();
        assert_eq!(repos, vec!["blooop/dotfiles", "blooop/wayfinder"]);

        let wayfinder = &inbox[1].1;
        // The cluster's stamp is the newest of its rows, whichever half that
        // row came from: #207 (unassigned, 10:00) was touched after #190
        // (mine, 09:00), and the cluster is ordered by where something
        // happened.
        assert_eq!(
            wayfinder.last_activity,
            Activity::parse("2026-08-24T10:00:00Z"),
        );
    }

    #[test]
    fn a_map_issue_is_a_heading_and_never_a_row_of_the_inbox() {
        // Sharper now than when the query was `assignee:@me`: a map is an open
        // issue with nobody assigned, so `no:assignee` finds *every* map in
        // every repo asked about, and without this every cluster header would
        // also be a row of the inbox below it.
        let inbox = parse_inbox(INBOX_RESPONSE.as_bytes()).expect("parse");
        let dotfiles = &inbox
            .iter()
            .find(|(repo, _)| repo == "blooop/dotfiles")
            .expect("dotfiles has an assigned issue")
            .1;
        let numbers: Vec<u64> = dotfiles.tickets.iter().map(|t| t.number).collect();
        assert_eq!(
            numbers,
            vec![4],
            "map issue #7 is drawn as a cluster, not listed under one"
        );
    }

    #[test]
    fn the_inbox_reads_an_issue_exactly_as_a_map_reads_its_sub_issue() {
        let inbox = parse_inbox(INBOX_RESPONSE.as_bytes()).expect("parse");
        let ticket = &inbox[1].1.tickets[2];
        assert_eq!(ticket.number, 191);
        assert_eq!(ticket.repo, "blooop/wayfinder", "its own repo, not a map's");
        assert_eq!(ticket.ticket_type, TicketType::Build, "labels still parse");
        assert_eq!(ticket.status, Status::Claimed, "this one is assigned");
        assert_eq!(ticket.prs.len(), 1, "the linked PR rides along");
        assert_eq!(ticket.prs[0].number, 203);
    }

    #[test]
    fn a_blocker_page_cut_short_makes_its_repos_inbox_partial() {
        let inbox = parse_inbox(INBOX_RESPONSE.as_bytes()).expect("parse");
        let wayfinder = &inbox[1].1;
        assert!(
            wayfinder.truncated,
            "#190's blocker page said there was more"
        );
        let dotfiles = &inbox[0].1;
        assert!(
            !dotfiles.truncated,
            "and the other repo's rows are not made partial by it"
        );
    }

    /// The same fixture with one search's *own* page flag flipped — not a
    /// blocker page's, and not the other half's.
    ///
    /// Anchored inside the named alias rather than by a global replace, which
    /// is the mistake this helper exists to make impossible: `replacen(.., 1)`
    /// always finds `mine`'s flag first, so a loop over both halves using it
    /// tests `mine` twice and passes while proving half of what it claims.
    fn with_page_cut_short(half: &str) -> String {
        const FLAG: &str = "],\n        \"pageInfo\": {\"hasNextPage\": false}";
        let alias = format!("\"{half}\": {{\n        \"nodes\"");
        let at = INBOX_RESPONSE
            .find(&alias)
            .unwrap_or_else(|| panic!("the fixture's `{half}` alias moved"));
        let rel = INBOX_RESPONSE[at..]
            .find(FLAG)
            .unwrap_or_else(|| panic!("the fixture's `{half}` page flag moved"));
        let cut = at + rel;
        format!(
            "{}{}{}",
            &INBOX_RESPONSE[..cut],
            FLAG.replace("false", "true"),
            &INBOX_RESPONSE[cut + FLAG.len()..]
        )
    }

    #[test]
    fn either_search_page_cut_short_makes_every_repos_inbox_partial() {
        // One page spans every repo, so a cut-short page cannot say which of
        // them lost rows — and with two searches it cannot say which *half*
        // either. Saying so on every cluster is the honest answer; picking one
        // would be a guess.
        //
        // Both halves are checked, because the rule reached by only one of
        // them is the rule half-implemented.
        for half in ["mine", "unassigned"] {
            let cut = with_page_cut_short(half);
            assert_ne!(cut, INBOX_RESPONSE, "the `{half}` flag did not move");
            let inbox = parse_inbox(cut.as_bytes()).expect("parse");
            assert!(
                inbox.iter().all(|(_, cluster)| cluster.truncated),
                "a cut-short `{half}` page makes every repo say its rows may be partial"
            );
        }
        // And the two really are different bytes — the guard on the helper
        // above, since both edits produce a response that parses.
        assert_ne!(
            with_page_cut_short("mine"),
            with_page_cut_short("unassigned"),
            "each half's own flag is what moved"
        );
    }

    #[test]
    fn an_empty_inbox_is_an_answer_and_not_an_error() {
        let body = r#"{"data": {
            "mine": {"nodes": [], "pageInfo": {"hasNextPage": false}},
            "unassigned": {"nodes": [], "pageInfo": {"hasNextPage": false}}
        }}"#;
        assert!(parse_inbox(body.as_bytes()).expect("parse").is_empty());
    }

    #[test]
    fn a_graphql_error_fails_the_inbox_rather_than_reading_as_empty() {
        let body = r#"{"errors": [{"message": "Bad credentials"}]}"#;
        assert!(parse_inbox(body.as_bytes()).is_err());
    }

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
        assert_eq!(map.map_title(), Some("Map: wf"));
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
        assert_eq!(
            map.map_title(),
            Some("Map: wf"),
            "the map renders as itself"
        );

        // And the label page being cut short is not the *tree* being cut
        // short: nothing ticket-bearing was truncated here.
        assert!(
            !map.truncated,
            "a truncated label page is not a partial tree"
        );
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
        let map =
            parse_map(truncated_response(false, false).as_bytes(), &wf_map_id()).expect("parse");
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

    /// One field of a GraphQL selection: the arguments it was called with, and
    /// the fields selected under it — which is to say, the shape an answer to
    /// that selection has.
    #[derive(Default)]
    struct Selected {
        arguments: String,
        fields: std::collections::BTreeMap<String, Selected>,
    }

    impl Selected {
        /// The field reached by following `path` from here, or `None` if the
        /// query does not select it *there*.
        ///
        /// Position is the whole point. The sub-issue block contains the
        /// linked-PR block, whose own `pageInfo` (#183) would answer on behalf
        /// of a connection that never asked if the question were "does this
        /// name appear anywhere below".
        fn at(&self, path: &[&str]) -> Option<&Selected> {
            match path {
                [] => Some(self),
                [step, rest @ ..] => self.fields.get(*step)?.at(rest),
            }
        }

        /// The field at `path`, created empty if the walk has not reached it
        /// yet — how the parse below fills the tree in as it descends.
        fn under(&mut self, path: &[String]) -> &mut Selected {
            let mut at = self;
            for step in path {
                at = at.fields.entry(step.clone()).or_default();
            }
            at
        }
    }

    /// Read the subset of GraphQL selection syntax [`MAP_QUERY`] is written in
    /// — names, optional `(arguments)`, optional nested `{ blocks }` — as the
    /// tree of fields it selects.
    ///
    /// Derived rather than matched, and that is the point of it. A guard that
    /// searches the query text for a spelling is satisfied by any text carrying
    /// that spelling: `pageInfo { endCursor }` passed a guard looking for
    /// `pageInfo` while answering none of the questions the parse asks of it.
    /// Walking the tree asks the parse's own question instead — is this field
    /// selected at this path — which no rewording can answer by accident.
    ///
    /// Panics on unbalanced braces, because a guard that gives up quietly on a
    /// query it cannot read is a guard that stops guarding the moment the query
    /// is reworded.
    fn selected(text: &str) -> Selected {
        let mut root = Selected::default();
        let mut path: Vec<String> = Vec::new();
        let mut last: Option<String> = None;
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '{' => path.push(last.take().expect("a block follows the field it selects")),
                '}' => {
                    path.pop().expect("a block was opened before it closed");
                    last = None;
                }
                // Arguments say nothing about the shape of the answer, but they
                // do say how much of it there is, which is its own assertion
                // below.
                '(' => {
                    let mut arguments = String::new();
                    for skipped in chars.by_ref() {
                        if skipped == ')' {
                            break;
                        }
                        arguments.push(skipped);
                    }
                    let named = last
                        .clone()
                        .expect("arguments follow the field they qualify");
                    root.under(&path).fields.entry(named).or_default().arguments = arguments;
                }
                ch if ch.is_alphanumeric() || ch == '_' => {
                    let mut name = String::from(ch);
                    while chars
                        .peek()
                        .is_some_and(|c| c.is_alphanumeric() || *c == '_')
                    {
                        name.push(chars.next().expect("peeked"));
                    }
                    root.under(&path).fields.entry(name.clone()).or_default();
                    last = Some(name);
                }
                _ => {}
            }
        }
        assert!(path.is_empty(), "unbalanced braces in {text}");
        root
    }

    #[test]
    fn the_query_asks_for_every_page_boundary_the_parse_reads() {
        // The truncation tests above feed `parse_map` hand-written fixtures, so
        // they cannot notice the live query never requesting the field — the
        // same hole #132 closed for reap's batch, guarded the same way: against
        // the query text. Drop the boundary from any of these three and every
        // map fetched live reads as complete forever, silently, which is
        // exactly the pre-#184 behaviour.
        //
        // `hasNextPage` by name, at the path the parse reads it from, because
        // that field *is* the answer: `Paged::page_info` and
        // `PageInfo::has_next_page` both default, so a `pageInfo` block
        // selecting anything else — `endCursor`, say — deserialises as a silent
        // "no claim of more". Asserting the connection asks for `pageInfo` is
        // not enough; that reword left all 443 tests green.
        let query = selected(MAP_QUERY);
        for path in [
            &[
                "query",
                "repository",
                "issue",
                "labels",
                "pageInfo",
                "hasNextPage",
            ][..],
            &[
                "query",
                "repository",
                "issue",
                "subIssues",
                "pageInfo",
                "hasNextPage",
            ][..],
            &[
                "query",
                "repository",
                "issue",
                "subIssues",
                "nodes",
                "blockedBy",
                "pageInfo",
                "hasNextPage",
            ][..],
        ] {
            assert!(
                query.at(path).is_some(),
                "the query no longer asks for {}, so nothing it fetches can \
                 report a page cut short: {MAP_QUERY}",
                path.join(".")
            );
        }
    }

    #[test]
    fn the_maps_own_label_page_is_as_deep_as_the_tracker_allows() {
        // The other half of the un-mapping fix: the parse forgives a truncated
        // label page, and the query makes truncation implausible in the first
        // place — 100 is the GraphQL page maximum, five times the old cap that
        // a label-heavy map actually overflowed. Addressed by path rather than
        // by "the first `labels` in the text", so the sub-issue labels selected
        // further down cannot be mistaken for the map issue's own.
        let query = selected(MAP_QUERY);
        let labels = query
            .at(&["query", "repository", "issue", "labels"])
            .expect("the map query selects the map issue's own labels");
        assert_eq!(
            labels.arguments.trim(),
            "first: 100",
            "the map's label page shrank below the tracker's maximum: {MAP_QUERY}"
        );
    }

    #[test]
    fn empty_search_result_means_no_maps() {
        let maps = parse_map_search(br#"{"items": []}"#).expect("parse");
        assert!(maps.is_empty());
    }
}
