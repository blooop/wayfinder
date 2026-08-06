# Research: the PR↔ticket link in the map query (#49)

**Question.** What does GitHub's GraphQL API expose for linking PRs to the issues
they close or reference, and can PR status ride along in wf's existing
one-GraphQL-query-per-map pattern at acceptable rate-limit cost?

**Answer.** Yes. `Issue.closedByPullRequestsReferences` is the selection that means
"this PR is the work for this ticket" — it carries exactly the manually-linked and
closing-keyword-linked PRs (what the Development sidebar shows) and excludes mere
mentions. Nested inside `subIssues.nodes` in `MAP_QUERY` with `state`, `isDraft`,
`reviewDecision`, and `statusCheckRollup { state }`, it costs **+1 rate-limit point
per map fetch** (measured 2026-08-06: baseline `MAP_QUERY` cost=3, with the PR
selection cost=4), and the cost is independent of the `first:` value chosen on the
PR connection. Recommended pagination: `first: 5`.

All claims below are against primary sources only: the official docs
(docs.github.com), GitHub's published schema
(<https://docs.github.com/public/fpt/schema.docs.graphql>, fetched 2026-08-06 —
identical to live introspection via `gh api graphql`), and live measured queries
against `blooop/wayfinder`.

---

## 1. Which selection carries the link

Three candidates exist on `Issue`:

### `closedByPullRequestsReferences` — the right one

Schema (schema.docs.graphql, `type Issue`):

> `"""List of open pull requests referenced from this issue"""`
> `closedByPullRequestsReferences(after, before, first, last, includeClosedPrs: Boolean = false, orderByState: Boolean = false, userLinkedOnly: Boolean = false): PullRequestConnection`

- `includeClosedPrs: Boolean = false` — "Include closed PRs in results". Without it
  only OPEN PRs return; **wf must pass `true`**, since merged PRs are precisely the
  interesting ones for a done ticket. Measured 2026-08-06: with `true`, issue #34
  returns merged PR #46 and issue #30 returns closed-unmerged PR #33.
- `userLinkedOnly: Boolean = false` — "Return only manually linked PRs". The default
  `false` therefore includes **both** manual Development-panel links **and**
  closing-keyword links ("Fixes #N" / "Closes #N"). This is the semantic wf wants;
  leave it at the default.
- `orderByState: Boolean = false` — "Return results ordered by state" (order
  direction undocumented; not relied on here).

This field is a *current-state* view of the same link set the Development sidebar
panel shows (links are created either by closing keywords in the PR body or by the
Development-section manual link — see
<https://docs.github.com/en/issues/tracking-your-work-with-issues/using-issues/linking-a-pull-request-to-an-issue>).

Measured semantics (2026-08-06, blooop/wayfinder):

| Issue | PR relationship | in `closedByPullRequestsReferences(includeClosedPrs: true)`? |
|---|---|---|
| #34 | PR #46 body says "Closes #34" (keyword link), merged | **yes** — `{number: 46, state: MERGED}` |
| #30 | PR #33 body says "Fixes #30" (keyword link), closed unmerged | **yes** — `{number: 33, state: CLOSED}` |
| #30 | PR #46 merely *mentions* #30 (no keyword) | **no** — mention correctly excluded |
| #3 | four cross-reference mentions, no linked PR | **no** — empty list |

So the field matches "this PR is the work for this ticket" exactly: keyword links
and manual links in, mere mentions out.

### `timelineItems(itemTypes: [CONNECTED_EVENT, CROSS_REFERENCED_EVENT])` — rejected

- `ConnectedEvent` — "Represents a 'connected' event on a given issue or pull
  request" (schema) — fires only on *manual* linking, and has a paired
  `DisconnectedEvent` ("Represents a 'disconnected' event…"), so the current link
  set must be reconstructed by replaying connect/disconnect pairs. Keyword links
  never produce a `ConnectedEvent`: measured 2026-08-06, issues #30 and #34 have
  **zero** `ConnectedEvent`s despite both having linked PRs.
- `CrossReferencedEvent` — "Represents a mention made by one issue or pull request
  to another" (schema) — fires for *any* mention, so it over-matches, and its
  `willCloseTarget: Boolean!` ("Checks if the target will be closed when the source
  is merged") is stated in the present/future tense and measured **false** on issue
  #34's event for PR #46 — the PR that *did* close it — so it cannot be used
  retrospectively. Measured 2026-08-06.

Timeline events are history, not state. Rejected.

### `linkedBranches` — out of scope

Exists on `Issue` (live introspection 2026-08-06) but carries branch links, not PRs.

## 2. What PR facts ride along

Field names confirmed against the published schema and exercised live (2026-08-06):

| Field | Type (schema) | Meaning |
|---|---|---|
| `number` | `Int!` | PR number |
| `state` | `PullRequestState!` = `OPEN \| CLOSED \| MERGED` | closed-unmerged vs merged are distinct states |
| `isDraft` | `Boolean!` | draft flag |
| `reviewDecision` | `PullRequestReviewDecision` = `APPROVED \| CHANGES_REQUESTED \| REVIEW_REQUIRED`, nullable | **null** when the repo requires no review — measured null on both #33 and #46 |
| `statusCheckRollup` | `StatusCheckRollup`, nullable; its `state: StatusState!` = `ERROR \| EXPECTED \| FAILURE \| PENDING \| SUCCESS` | "The combined status for the commit" — CI rollup for the PR's head commit; measured `SUCCESS` on #33 and #46. Nullable (no checks configured ⇒ null) |
| `repository { nameWithOwner }` | `Repository!` | which repo the PR lives in (see §4) |

`statusCheckRollup` lives directly on `PullRequest` (live introspection 2026-08-06),
so no `commits(last: 1)` detour is needed. Selecting only its `state` (not
`contexts`) adds no connection, hence no cost.

## 3. Cost

GitHub's documented formula
(<https://docs.github.com/en/graphql/overview/rate-limits-and-node-limits-for-the-graphql-api>):
"Add up the number of requests needed to fulfill each unique connection in the
call. Assume every request will reach the `first` or `last` argument limits.
Divide the number by 100 and round the result to the nearest whole number." Budget:
5,000 points/hour/user.

For `MAP_QUERY` (`subIssues(first: 100)` is the multiplier):

| Query | Connections (requests) | Formula | Measured (`rateLimit { cost }`, 2026-08-06, blooop/wayfinder #1) |
|---|---|---|---|
| current `MAP_QUERY` | issue 1 + labels 1 + subIssues 1 + 100×(labels + assignees + blockedBy) = 303 | 3 | **cost = 3** |
| + `closedByPullRequestsReferences(first: 5)` per sub-issue | 303 + 100 = 403 | 4 | **cost = 4** |

(Issue #3's resolution said ~2 points; the map and its selection have grown since —
today's measured baseline is 3. The PR selection's marginal cost is **+1** either way.)

The per-sub-issue request count is 100 (the parent `first`) **regardless of the PR
connection's own `first`** — `first` on a leaf connection only affects the
500,000-node limit, which this query doesn't approach (100 × 5 = 500 extra nodes).
So the `first` value is free to choose on cost grounds. **Pick `first: 5`**: a
ticket with more than a couple of linked PRs is already pathological, and 5 leaves
slack without inviting wf to render a PR list.

## 4. Cross-repo

Link *creation* is cross-repo-capable per the docs
(<https://docs.github.com/en/issues/tracking-your-work-with-issues/using-issues/linking-a-pull-request-to-an-issue>):

- Keywords: `KEYWORD OWNER/REPOSITORY#ISSUE-NUMBER` (e.g. `Fixes octo-org/octo-repo#100`)
  links an issue in a different repository; keywords are interpreted only when the
  PR targets its repository's **default branch**.
- Manual linking from the **issue** sidebar: "The issue can be in a different
  repository than the linked pull request or branch."
- Manual linking from the **PR** sidebar: "The issue and pull request must be in
  the same repository."

On the read side, `closedByPullRequestsReferences` has no repo-scoping argument and
returns full `PullRequest` nodes carrying their own `repository` (schema), so
nothing restricts results to the issue's repo. **Not live-verified** — no
pre-existing cross-repo-linked pair was available in blooop's repos and creating
PRs is out of scope for research. The recommended fragment selects
`repository { nameWithOwner }` so wf can render (or filter) a foreign-repo PR
correctly either way.

## Recommended query fragment

Add inside the `subIssues.nodes` selection of `MAP_QUERY`
(`src/fetch.rs`), after `blockedBy`:

```graphql
closedByPullRequestsReferences(first: 5, includeClosedPrs: true) {
  nodes {
    number
    state
    isDraft
    reviewDecision
    statusCheckRollup { state }
    repository { nameWithOwner }
  }
}
```

Measured cost of the full `MAP_QUERY` with this fragment: **4 points** (baseline 3;
measured 2026-08-06 against blooop/wayfinder map issue #1 via
`gh api graphql` with `rateLimit { cost }`). At 5,000 points/hour that is ~1,250
map fetches/hour — no change to wf's call pattern needed.

Model note for the implementing ticket: when several PRs return, prefer
`MERGED` > `OPEN` > `CLOSED` (a closed-unmerged PR, like #33 here, is superseded
work, not the work). `reviewDecision` and `statusCheckRollup` are both nullable
and null is common (no required reviews / no checks) — null must mean "no signal",
never be conflated with a failing or pending state.
