---
name: wf-one
description: Run one piece of known work as a single-ticket wayfinder map — no planning, no fanning out — so it is visible in wf, resumable across sessions, and still built test-first and reviewed in fresh context. Use when the user has a task they already know the shape of and wants it tracked, resumable, or done unattended; when a job is small but too long for one sitting; or when they ask for a ticket without asking to be planned at.
---

## Cross-agent invocation

This bundle runs in both Claude Code and Codex. Mention a wayfinder skill with
`/` in Claude Code and `$` in Codex; skill names below are written without a
sigil.

For work whose shape is already known. There is nothing to decide and nothing to chart, but the job still wants three things a bare session doesn't give it: a row in `wf`, survival across a session ending, and the `wf-tdd` → `wf-review` gates. So it gets a **map with exactly one ticket** — and never a second.

Two issues, not one: wf's cluster header is a map and headers are not cursor stops, so the ticket is the row you land on and launch from. The map is the container that makes it visible.

## Two ways in

**`<sigil>wf-one <task>`** — the work does not exist yet. File both issues below.

**`<sigil>wf-one adopt <n>`** — issue `<n>` already exists and no map claims it. `wf` draws it in the *yours or unclaimed* cluster and `enter` sends it here. **Do not create a second issue for it.** File the map, parent the existing issue under it, and pick up at **Run it** — everything after setup is identical, which is the whole reason adoption routes here rather than to a skill of its own.

Read the issue first (`gh issue view <n> --repo "$REPO"`) — its title and body are the ticket, already written by whoever filed it. Do not retitle or rewrite it to look like something you filed; you are giving it a map, not taking it over. Type labels are the one thing to add if it has none: `wayfinder:build` for code, `wayfinder:task` for other doing, so the row's stage starts deriving from its PRs.

If the issue turns out to be too big for one ticket, that is the same signal the **Don't let it fan out** section below describes, and it has the same answer: say so, leave a `### handoff`, and hand it to `wf`. Do not chart around it.

## Set it up

A `wf-one` launch is a **creation** either way: even adopting, the map it needs does not exist on the tracker yet, so its line is `<sigil>wf-one <task>` or `<sigil>wf-one adopt <n>` and never carries a `ctx: <json>` block. Nor may you synthesize one when handing the ticket to `wf-tdd` or `wf-review` below — the block is `wf`'s record of what it fetched, and a hand-written one would be a claim about tracker state nobody read. Those subagents discover from the ticket, exactly as a hand-typed invocation does. See **The launch context** in [GITHUB_TRACKER.md](../wf/GITHUB_TRACKER.md).

Tracker mechanics — pinning the repo, creating and parenting issues, labels, claiming — are in [GITHUB_TRACKER.md](../wf/GITHUB_TRACKER.md) in the sibling `wf` skill. Pin the repo from the working repo's own remote and pass it explicitly on every `gh` call; push branches by name.

1. **The map** (`wayfinder:map`), titled as the work. Body is short, because a one-ticket map has no route to index:

   ```markdown
   ## Destination

   <the task, and what done looks like. One or two lines.>

   ## Notes

   Single-ticket map: known work, no fanning. <anything a resuming session needs — constraints, the skills this job calls for.>
   ```

2. **The ticket**, as the map's only child, claimed to yourself before any work. `wayfinder:build` for code — wf then derives the row's stage from its PRs, so the row shows real progress — or `wayfinder:task` for other doing. Body is one `## Work` section: what to do, and how you will know it's done.

   **Adopting**: the ticket is issue `<n>`, which exists. Parent it under the map you just made and claim it — that is the whole of this step. See **Create a ticket (create, then parent)** in [GITHUB_TRACKER.md](../wf/GITHUB_TRACKER.md); the parenting call takes the issue's database id and does not care that the issue is old.

File both **before** starting. That is what makes the run resumable rather than a session you hope survives.

## Run it

Through the lifecycle in [LIFECYCLE.md](../wf/LIFECYCLE.md), which this skill follows unchanged — this session is the manager and never does stage work in its own context:

1. **Build** in a fresh subagent via `wf-tdd`: red before green, ending at a pushed branch with an open PR linking the ticket.
2. **Gate**: the PR exists and its checks are green. Absent checks are *unproven*, not green — verify a run exists for the **full 40-char** head SHA before believing it.
3. **Review** in a fresh subagent via `wf-review`: two-axis report posted on the PR. Fresh context is the point — the reviewer must not be whoever wrote the code.
4. A failed gate gets **one** retry subagent. A second failure parks the ticket.
5. **Stop at approved.** Merging is the human's unless they explicitly said merge-when-green.

Breadcrumb the ticket at each stage transition and gate result — `**breadcrumb:**`, one or two lines, append-only. On deliberate exit, one `### handoff` comment: where we are, the open thread, the first move on resume. Resuming reads the last handoff, then the breadcrumbs after it, and opens with a breadcrumb noting resumption.

## Don't let it fan out

The one rule that keeps this skill small: **this map never gets a second ticket.** If the work turns out to need decisions made, or to be too big for one ticket, that is the signal it was never single-ticket work. Say so, leave a `### handoff` on the ticket, and hand it to `wf` (to chart properly, with you), `wf-mid` (to chart it mostly alone, asking only about the decisions that are genuinely yours) or `wf-auto` (to chart and drive it alone). Don't grow this map into a real one — a map charted backwards out of work already underway is the thing wayfinding exists to avoid.

Decisions small enough to take alone are yours, in this order when they collide: **long-term maintainability**, **simplicity** (smallest thing that works; deleting beats adding), **constructive modeling** (illegal states unrepresentable — `/constructive-modeling` where a type is at stake), **test-first**. Note in a breadcrumb which one decided anything non-obvious. Anything needing credentials, a purchase, or a human-only action parks with a handoff.

## Close it out

A resolution comment on the ticket saying what happened, then close the ticket — and **close the map with it**. A one-ticket map is done when its ticket is, and leaving it open leaves an empty cluster in `wf` forever. An adopted ticket closes the same way: it was somebody's issue before it was a ticket, so the resolution comment is what they will read.
