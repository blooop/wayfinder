# The manager protocol — a node's lifecycle, one fresh subagent per stage

How a launched session drives a node (or a whole subtree of nodes) through its stages without anyone watching. This is referenced by the `wf-auto` skill and honored by `wf-tdd` and `wf-review`; nothing launches "the manager" directly — **the launched session is the manager**. wf itself never is: it exec'd and exited.

## The stage lattice (decided on blooop/wayfinder#61)

One five-value lattice for every node: **ready → building → in review → needs attention → done**.

Each **open PR** linked to the node maps to one stage:

| PR facts | stage |
|---|---|
| checks failing **or** changes requested | needs attention |
| draft, **or** checks pending | building |
| otherwise open (incl. approved awaiting merge) | in review |

The node takes the **max over its open PRs** in the constant order `needs attention > in review > building`. No open PRs: any merged PR → done; otherwise derive from ticket state — ready (open, unblocked, unclaimed) / in progress (claimed) / done (closed). PR state dominates ticket state when present.

Routing — what a stage launches: build nodes at ready/building/needs-attention → `wf-tdd`; build nodes at in-review → `wf-review`; decision-type nodes → the `wf` skill. Done is not launchable.

## The protocol, per node

1. **Fresh subagent per stage — handed pointers, never readings.** The manager never does stage work in its own context, and passing on its own reading of a ticket *is* stage work done in the manager's context. Each stage subagent is handed exactly what a launch's `ctx:` block carries one level down (see **The launch context** in [GITHUB_TRACKER.md](GITHUB_TRACKER.md)) — the repo, the map reference, the ticket's number, type and stage, and its PR links — plus the stage skill to follow and the human's `steer:` line **verbatim** if there is one. Nothing the manager composed: no summary of the ticket, no digest of its trail, no account of the diff or of what an earlier stage claims to have done.

   ```handoff
   repo — the pinned repo
   map — the map reference
   number — the ticket's number
   ticket_type — the ticket's type
   stage — the ticket's stage
   prs — its PR links
   skill — no ctx counterpart: the stage skill to follow
   steer — no ctx counterpart: the human's line, verbatim
   ```
2. **Gates between stages, checked by the manager from GitHub state alone:**
   - build → review: **PR exists and checks are green** (`gh pr checks <pr> --watch` to wait on a live run).
   - review → done: **the two-axis report is posted on the PR** with a verdict.
3. **No checks is not green — and not failing either.** Verify a run actually exists for the head SHA (`gh api "repos/$REPO/actions/runs?head_sha=<sha>" --jq .total_count`), because a dropped `pull_request` trigger leaves a PR looking clean while nothing ever ran. Distinguish three outcomes, exactly as the stage lattice's `Checks::Absent` does: **green** → advance; **red** → gate failure (below); **absent** → the gate is *unproven*, not passed. On absent, re-fire once (close then reopen the PR — an empty commit changes the SHA and loses the review's fixed point), and if still nothing, fall back to running the suite locally and say so explicitly in the ticket comment: the node advances on a **local** green with the CI gap named, never on silence mistaken for success.
4. **Gate failure**: feed the failure output to **one** fresh retry subagent. A second failure parks the node — a `### handoff` comment on its ticket with the failure attached — and the run moves on to other work. Before chasing a failure, check whether the same job fails on the default branch: a pre-existing or infrastructure failure is recorded, not fixed.
5. **Merging stays human.** An unattended lifecycle ends at *in review, approved*; the manager's last act on a node is a ticket comment saying so. A steering prompt may explicitly grant "merge when green" — only then squash-merge and close the ticket.
6. **Journaling**: the manager posts a `**breadcrumb:**` on the ticket at each stage transition and gate result, so a crashed lifecycle re-enters by trail exactly like any claimed ticket.
