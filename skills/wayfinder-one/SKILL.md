---
name: wayfinder-one
description: Run one piece of known work as a single-ticket wayfinder map — no planning, no fanning out — so it is visible in wf, resumable across sessions, and still built test-first and reviewed in fresh context. Use when the user has a task they already know the shape of and wants it tracked, resumable, or done unattended; when a job is small but too long for one sitting; or when they ask for a ticket without asking to be planned at.
---

For work whose shape is already known. There is nothing to decide and nothing to chart, but the job still wants three things a bare session doesn't give it: a row in `wf`, survival across a session ending, and the `/tdd` → `/review` gates. So it gets a **map with exactly one ticket** — and never a second.

Two issues, not one: wf's cluster header is a map and headers are not cursor stops, so the ticket is the row you land on and launch from. The map is the container that makes it visible.

## Set it up

Tracker mechanics — pinning the repo, creating and parenting issues, labels, claiming — are in [GITHUB_TRACKER.md](../wayfinder/GITHUB_TRACKER.md) in the sibling `wayfinder` skill. Pin the repo from the working repo's own remote and pass it explicitly on every `gh` call; push branches by name.

1. **The map** (`wayfinder:map`), titled as the work. Body is short, because a one-ticket map has no route to index:

   ```markdown
   ## Destination

   <the task, and what done looks like. One or two lines.>

   ## Notes

   Single-ticket map: known work, no fanning. <anything a resuming session needs — constraints, the skills this job calls for.>
   ```

2. **The ticket**, as the map's only child, claimed to yourself before any work. `wayfinder:build` for code — wf then derives the row's stage from its PRs, so the row shows real progress — or `wayfinder:task` for other doing. Body is one `## Work` section: what to do, and how you will know it's done.

File both **before** starting. That is what makes the run resumable rather than a session you hope survives.

## Run it

Through the lifecycle in [LIFECYCLE.md](../wayfinder/LIFECYCLE.md), which this skill follows unchanged — this session is the manager and never does stage work in its own context:

1. **Build** in a fresh subagent via `/tdd`: red before green, ending at a pushed branch with an open PR linking the ticket.
2. **Gate**: the PR exists and its checks are green. Absent checks are *unproven*, not green — verify a run exists for the **full 40-char** head SHA before believing it.
3. **Review** in a fresh subagent via `/review`: two-axis report posted on the PR. Fresh context is the point — the reviewer must not be whoever wrote the code.
4. A failed gate gets **one** retry subagent. A second failure parks the ticket.
5. **Stop at approved.** Merging is the human's unless they explicitly said merge-when-green.

Breadcrumb the ticket at each stage transition and gate result — `**breadcrumb:**`, one or two lines, append-only. On deliberate exit, one `### handoff` comment: where we are, the open thread, the first move on resume. Resuming reads the last handoff, then the breadcrumbs after it, and opens with a breadcrumb noting resumption.

## Don't let it fan out

The one rule that keeps this skill small: **this map never gets a second ticket.** If the work turns out to need decisions made, or to be too big for one ticket, that is the signal it was never single-ticket work. Say so, leave a `### handoff` on the ticket, and hand it to `/wayfinder` (to chart properly, with you) or `/wayfinder-auto` (to chart and drive it alone). Don't grow this map into a real one — a map charted backwards out of work already underway is the thing wayfinding exists to avoid.

Decisions small enough to take alone are yours, in this order when they collide: **long-term maintainability**, **simplicity** (smallest thing that works; deleting beats adding), **constructive modeling** (illegal states unrepresentable — `/constructive-modeling` where a type is at stake), **test-first**. Note in a breadcrumb which one decided anything non-obvious. Anything needing credentials, a purchase, or a human-only action parks with a handoff.

## Close it out

A resolution comment on the ticket saying what happened, then close the ticket — and **close the map with it**. A one-ticket map is done when its ticket is, and leaving it open leaves an empty cluster in `wf` forever.
