# The manager protocol — a node's lifecycle, one fresh subagent per stage

How a launched session drives a node (or a deferred subtree of nodes) through its stages without anyone watching. This is referenced by the wayfinder skill's deferred mode and honored by `/tdd` and `/review`; nothing launches "the manager" directly — **the launched session is the manager**. wf itself never is: it exec'd and exited.

## The stage lattice (decided on blooop/wayfinder#61)

One five-value lattice for every node: **ready → building → in review → needs attention → done**.

Each **open PR** linked to the node maps to one stage:

| PR facts | stage |
|---|---|
| checks failing **or** changes requested | needs attention |
| draft, **or** checks pending | building |
| otherwise open (incl. approved awaiting merge) | in review |

The node takes the **max over its open PRs** in the constant order `needs attention > in review > building`. No open PRs: any merged PR → done; otherwise derive from ticket state — ready (open, unblocked, unclaimed) / in progress (claimed) / done (closed). PR state dominates ticket state when present.

Routing — what a stage launches: build nodes at ready/building/needs-attention → `/tdd`; build nodes at in-review → `/review`; decision-type nodes → the wayfinder skill. Done is not launchable.

## The protocol, per node

1. **Fresh subagent per stage.** The manager never does stage work in its own context. Each stage subagent gets: the ticket body, the map's Decisions-so-far, the branch/PR pointers, and the stage skill to follow. Fresh context is what makes the review stage a real review — the reviewer never saw the code written.
2. **Gates between stages, checked by the manager from GitHub state alone:**
   - build → review: **PR exists and checks are green** (`gh pr checks <pr> --watch` to wait on a live run).
   - review → done: **the two-axis report is posted on the PR** with a verdict.
3. **No checks is not green — and not failing either.** Verify a run actually exists for the head SHA (`gh api "repos/$REPO/actions/runs?head_sha=<sha>" --jq .total_count`), because a dropped `pull_request` trigger leaves a PR looking clean while nothing ever ran. Distinguish three outcomes, exactly as the stage lattice's `Checks::Absent` does: **green** → advance; **red** → gate failure (below); **absent** → the gate is *unproven*, not passed. On absent, re-fire once (close then reopen the PR — an empty commit changes the SHA and loses the review's fixed point), and if still nothing, fall back to running the suite locally and say so explicitly in the ticket comment: the node advances on a **local** green with the CI gap named, never on silence mistaken for success.
4. **Gate failure**: feed the failure output to **one** fresh retry subagent. A second failure parks the node — a `### handoff` comment on its ticket with the failure attached — and the run moves on to other work. Before chasing a failure, check whether the same job fails on the default branch: a pre-existing or infrastructure failure is recorded, not fixed.
5. **Merging stays human.** An unattended lifecycle ends at *in review, approved*; the manager's last act on a node is a ticket comment saying so. A steering prompt may explicitly grant "merge when green" — only then squash-merge and close the ticket.
6. **Journaling**: the manager posts a `**breadcrumb:**` on the ticket at each stage transition and gate result, so a crashed lifecycle re-enters by trail exactly like any claimed ticket.
