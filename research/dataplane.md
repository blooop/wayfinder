# GitHub Issues as the live data plane: call pattern, limits, caching

Resolves #3. Measurements taken 2026-08-04 against this repo's real map (#1, 7 sub-issues,
2 dependency edges) with a single user's `gh` auth, plus GitHub's primary docs.

**Verdict: yes — GitHub Issues works as `wf`'s live data plane.** One GraphQL query renders a
whole map (2 points, ~0.5 s); REST + ETag makes a 3–5 s poll on the focused map effectively
free (304s cost zero rate limit). The model fits better than expected: the API's dependency
summary counts *open* blockers separately, so frontier status falls out of a single call.

## The read surface

| Data | REST | GraphQL |
|---|---|---|
| Sub-issues of a map | `GET /repos/{o}/{r}/issues/{n}/sub_issues` (max `per_page=100`) | `Issue.subIssues(first:)` |
| Parent of a ticket | `GET .../issues/{n}/parent` (also `parent_issue_url` on every issue object) | `Issue.parent` |
| Blockers | `GET .../issues/{n}/dependencies/blocked_by` | `Issue.blockedBy(first:)` |
| Blocked | `GET .../issues/{n}/dependencies/blocking` | `Issue.blocking(first:)` |
| Progress rollup | `sub_issues_summary {total, completed, percent_completed}` on every issue object | `Issue.subIssuesSummary` |
| Dependency rollup | `issue_dependencies_summary {blocked_by, blocking, total_blocked_by, total_blocking}` on every issue object | `Issue.issueDependenciesSummary` |
| Cross-repo map discovery | `GET /search/issues?q=owner:{me} label:"wayfinder:map" is:open&advanced_search=true` | `search(query:…, type: ISSUE)` |

Two facts that shape everything downstream (both verified against live payloads and GraphQL
schema descriptions):

1. **Every REST issue object already embeds both summaries.** The single `sub_issues` response
   carries each child's state, assignees, labels, `sub_issues_summary`, and
   `issue_dependencies_summary`. You only need per-issue dependency calls to learn *which*
   issues block — never *whether* something blocks.
2. **`blocked_by` counts open blockers only; `total_blocked_by` includes closed ones**
   (per the GraphQL schema: "Count of issues this issue is blocked by" vs "Total count …
   (open and closed)"). So `state == open && issue_dependencies_summary.blocked_by == 0`
   **is** the frontier test — computable from one call, no edge fetches.

## (b) Render one map's graph

### GraphQL: 1 round trip, 2 points, ~0.5 s (measured)

```graphql
{
  rateLimit { cost remaining }
  repository(owner: "blooop", name: "wayfinder") {
    issue(number: 1) {
      title state
      subIssuesSummary { total completed percentCompleted }
      subIssues(first: 50) {
        nodes {
          number title state stateReason
          assignees(first: 5) { nodes { login } }
          labels(first: 10) { nodes { name } }
          blockedBy(first: 20) { nodes { number } }
          blocking(first: 20) { nodes { number } }
          comments { totalCount }
        }
      }
    }
  }
}
```

Measured on map #1: `cost: 2`, wall time 0.52 s via `gh api graphql`. Returns everything a
starmap render needs — nodes, states, both edge directions, claims (assignees), rollups.

### REST: 1 + K calls (K = children with `blocked_by > 0` or `blocking > 0`)

1. `GET /repos/{o}/{r}/issues/1/sub_issues` — 0.48 s measured. Nodes, states, frontier
   status, and rollups all come from this one call.
2. For each child whose `issue_dependencies_summary` shows nonzero counts, fetch one
   direction of edges: `GET .../issues/{n}/dependencies/blocked_by`. Each `blocked_by` edge
   implies the reverse `blocking` edge, so one direction suffices.

For this repo's map (edges 2→8, 3→7): **3 REST calls** total. Worst case for an N-ticket
map where every ticket is blocked: 1 + N.

## (a) List every ticket across all projects

### GraphQL: 1 round trip (measured: 3 maps / 2 repos / 25 tickets, ~4 s)

```graphql
{
  search(query: "owner:blooop label:\"wayfinder:map\" is:issue is:open",
         type: ISSUE, first: 25) {
    issueCount
    nodes { ... on Issue {
      number title state
      repository { nameWithOwner }
      subIssuesSummary { total completed }
      subIssues(first: 50) { nodes {
        number title state stateReason
        blockedBy(first: 20) { nodes { number } }
        assignees(first: 5) { nodes { login } }
      } }
    } }
  }
}
```

Measured: discovers all maps and renders all their tickets in one call. Wall time 4.0 s
(search dominates; direct reads are ~0.5 s). **Cost scales with `first` on the search
connection**: `first: 25` cost 25 points, `first: 5` cost 3 points — set `first` to the real
map count, not a generous default. (Cost formula: unique connection fetches ÷ 100, min 1 —
[GraphQL rate limit docs](https://docs.github.com/en/graphql/overview/rate-limits-and-node-limits-for-the-graphql-api).)

### REST: 1 search + 1 `sub_issues` per map

`GET /search/issues?q=owner:{me}+label:"wayfinder:map"+is:open&advanced_search=true`
(3.4 s measured), then one `sub_issues` call per map found. Search result items are full
issue objects with both summaries, so the dashboard row (progress %, blocked counts) needs no
further calls.

**Scoping is mandatory.** The unscoped search `label:"wayfinder:map"` matched **4,249 issues**
across GitHub — the label convention is in use by many wayfinder users. Always qualify with
`owner:`/`user:`/`org:` (multiple `owner:` qualifiers OR together for multi-org users).

## Rate limits (single user's `gh` auth)

Documented at [REST rate limits](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api)
and the GraphQL page above; bucket separation verified live via `gh api rate_limit`:

| Bucket | Limit | Notes |
|---|---|---|
| REST core | 5,000 req/hr | `sub_issues`, `dependencies/*` land here (`X-Ratelimit-Resource: core`) |
| REST search | **30 req/min**, separate bucket | verified: `X-Ratelimit-Resource: search` |
| GraphQL | 5,000 points/hr, separate bucket | search-in-GraphQL spends GraphQL points, not the search bucket |
| Secondary | 900 pts/min REST, 2,000 pts/min GraphQL, ≤100 concurrent | GETs cost 1 pt |

## Conditional requests / ETags — measured

- Issue endpoints return `ETag` (weak, `W/"…"`) and `Cache-Control: private, max-age=60`.
- `gh api` does **not** cache or send `If-None-Match` itself; pass it explicitly:
  `gh api -H "If-None-Match: $ETAG" repos/…/issues/1/sub_issues` → `HTTP 304`.
- **304s cost zero rate limit — verified.** Three consecutive 304s left `X-Ratelimit-Used`
  pinned at 41. Docs: "Making a conditional request does not count against your primary rate
  limit if a 304 response is returned and the request was made while correctly authorized"
  ([best practices](https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api)).
- **GraphQL has no conditional requests** — verified: the GraphQL response carries no `ETag`
  or `Cache-Control` header. Every GraphQL poll pays full price.
- The search endpoints returned no `ETag` either — search polls always pay.

## Recommended polling/caching strategy

Split the loop by heat, and put the hot path on REST because only REST has free 304s:

1. **Focused map (the one on screen): REST `sub_issues` with `If-None-Match`, every 3–5 s.**
   Unchanged polls are 304s and cost nothing; a change costs 1 call (+ K edge calls only for
   children whose `issue_dependencies_summary` changed). Node states, frontier, claims, and
   rollups all arrive in that one response. Even a pathological hour of constant change is
   ~720 + edges ≪ 5,000.
2. **Background maps: one batched GraphQL render every 30–60 s** (the query in section (a),
   `first` sized to the real map count). At cost ~3–5 that's ≤600 points/hr.
3. **Discovery (which maps exist): on startup, on manual refresh, and a slow timer (≥60 s).**
   New maps are rare; don't spend the 30/min search bucket (or GraphQL points) in the hot loop.
4. **Cache shape:** per-URL ETag store + parsed graph keyed `(repo, issue_number)` with each
   node's `updated_at`. On a 200, diff children by `updated_at` to re-fetch edges selectively.
5. **Write-through on own mutations:** when `wf` itself closes/claims/links a ticket, update
   the local cache from the mutation response and drop the stale ETag — don't wait a poll cycle.

Total steady-state budget for a TUI watching 1 focused + 5 background maps: effectively free
on REST core, ~500 GraphQL points/hr. Headroom is enormous.

## Gaps: where the API expresses the wayfinder model poorly

- **Frontier is derived, not queryable.** No search qualifier selects "open and unblocked";
  search can find maps but not frontier tickets. Mitigated well: the open-only `blocked_by`
  count makes frontier a client-side filter on data you already fetched — but you can never ask
  GitHub "give me every frontier ticket across all projects" in one search.
- **Claims are not atomic.** Assignee is the natural claim primitive, but read-then-assign
  races: two sessions can both see an unassigned ticket and both assign. There is no
  compare-and-swap, no lease, no expiry — a crashed session holds its claim until someone
  notices. Claim *time* requires reading timeline events (extra calls). `wf` needs its own
  convention (e.g. treat oldest `assigned` event as winner, or breadcrumb comments per #6).
- **No push channel for a local TUI.** Webhooks need a public endpoint; polling is the only
  serverless option, hence the ETag strategy above. `X-Poll-Interval` exists only on the
  events endpoints, which don't cover sub-issue/dependency changes.
- **One nesting level per query.** GraphQL can't recurse: maps-of-maps need one extra round
  trip per depth level. GitHub caps sub-issue trees (100 sub-issues per parent, 8 levels), so
  depth stays small, but the flat "one map = one level" shape is what the API prices at 2 points.
- **Invalidation edge case (unverified):** whether adding/removing a dependency edge bumps the
  *child's* `updated_at` (and therefore flips the parent's `sub_issues` ETag) was not tested —
  it requires mutating the live graph. If it doesn't, a pure-ETag loop could miss edge-only
  changes; the 30–60 s GraphQL background render bounds that staleness either way.
- **Label discovery is a convention, not a namespace.** 4,249 strangers' issues share the
  `wayfinder:map` label. Owner scoping fixes reads, but nothing stops a fork's map from
  appearing in an org-scoped search.

## Raw measurements

| Call | Wall time | Cost |
|---|---|---|
| REST `GET issues/1/sub_issues` (7 children) | 0.48 s | 1 core |
| REST `GET issues/{n}/dependencies/blocked_by` | ~0.4 s | 1 core |
| REST `GET search/issues` (owner-scoped) | 3.4 s | 1 search (30/min bucket) |
| GraphQL one-map render (query above) | 0.52 s | 2 points |
| GraphQL all-maps render, `first: 25` | 4.0 s | 25 points |
| GraphQL all-maps render, `first: 5` | — | 3 points |
| REST conditional re-poll → 304 | ~0.4 s | **0** |
