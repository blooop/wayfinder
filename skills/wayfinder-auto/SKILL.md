---
name: wayfinder-auto
description: Chart and drive a wayfinder map alone — same map, same decision tickets on the tracker, but decisions are settled against declared guiding principles instead of a conversation, and execution is in scope, so one run can walk a map from open questions to merged PRs. Use when the user wants an effort mapped, worked, or implemented autonomously, unattended, or AFK; when they say to use your best judgement on a map; or when wf launches a node in autonomous mode.
---

Same artifact as `/wayfinder` — one map issue with decision tickets as its children — with nobody in the loop. Two things change, and everything else follows from them:

- **Answers come from principles, not questions.** The principles below are the human's standing voice. Every resolution cites the one that decided it.
- **Execution is in scope by default.** `/wayfinder` plans and hands off; here the map runs all the way to merged work, so `wayfinder:build` tickets are ordinary citizens and the map is the single place every ticket — decision and build alike — is tracked.

This is for work with **fog in it** — open questions between here and the destination. One piece of already-known work wants `/wayfinder-one` instead: a single-ticket map, same gates, no charting.

## The principles

In order. When two pull opposite ways, the earlier wins.

1. **Long-term maintainability** — decide for whoever reads this in a year, not for the session in progress.
2. **Simplicity** — the smallest thing that resolves the question. Deleting beats adding; one concept beats two that overlap.
3. **Constructive modeling** — illegal states unrepresentable, sum types over tag-plus-parallel-fields, no sentinels. Run `/constructive-modeling` on any ticket that shapes a type.
4. **Test-first** — behavior arrives with a failing test that proves it was absent. Build tickets go through `/tdd`; a decision ticket says how its outcome will be tested.

A map's `## Notes` may add principles or reorder these for that effort; a ticket may not. Where the principles are silent, pick what the map's Decisions-so-far already implies — consistency with the route already walked is itself the tiebreak.

## The map

One issue labelled `wayfinder:map`; its tickets are its child issues. The map is an **index**: it gists each closed ticket in one line and links it, so the detail lives in exactly one place — the ticket. Refer to maps and tickets by their titles, never bare numbers; a name wraps its link.

```markdown
## Destination

<what reaching the end of this map looks like. One or two lines; every run orients to it first.>

## Notes

<domain; skills every run should consult; extra or reordered principles for this effort>

## Decisions so far

- [<closed ticket title>](link) — <one-line gist of the answer> *(agent)*

## Not yet specified

<!-- in-scope fog you can't phrase sharply yet; graduates into tickets as the frontier advances -->

## Out of scope

<!-- work ruled past the destination; closed, never graduates -->
```

A ticket is a child issue whose body is one `## Question`, sized to a single session, labelled `wayfinder:<type>`:

- **research** — knowledge from outside this working directory. Resolved by a `/research` subagent; findings onto a `research/<name>` branch, linked from the ticket.
- **prototype** — make a cheap rough artifact, judge it against the principles, record the judgement and link the artifact.
- **grilling** — the default decision ticket. Settled by reasoning against the principles and the repo, not by asking.
- **task** — work that must happen before a decision can be made (provisioning, moving data). Records what was done and any facts later tickets depend on.
- **build** — an execution slice. `/tdd` to build it, `/review` to review it; its stage is *derived from its PRs*, never declared — see [LIFECYCLE.md](../wayfinder/LIFECYCLE.md) for the lattice, the gates, and the manager protocol.

Blocking is the tracker's native dependency edge, so the frontier renders in the tracker's own UI. The **frontier** is the open, unblocked, unclaimed children. Claiming is assigning the ticket to yourself, before any work on it.

**Not yet specified** is in-scope fog you can't yet phrase sharply; **Out of scope** is work past the destination. Ticket it when the question is already sharp, even if blocked and unactionable; leave it as fog when it isn't.

Tracker mechanics — pinning the repo, creating and parenting issues, blocking edges, the frontier query, the local-markdown fallback — live in [GITHUB_TRACKER.md](../wayfinder/GITHUB_TRACKER.md) in the sibling `wayfinder` skill. Pin the repo from the working repo's own remote, pass it explicitly on every call, and push branches by name: a fork's map belongs to the fork, never its parent. State the repo and its visibility in your first line of output — an autonomous run on a public repo publishes the whole fog sketch, and the invocation is the only consent there is for that.

## A run is a manager

One run may walk a whole map, but it never does the work in its own context: it is a **manager** that spawns a **fresh subagent per ticket** — and per build *stage* — checking the gates in between from tracker state alone. That is what keeps clean context per ticket while a single agent implements the lot, and it is what makes the review stage a real review: the reviewer never saw the code written. The manager's own context holds the map, the principles, and pointers — never a ticket's working detail.

## Chart

`/wayfinder-auto <loose idea>`

1. **Name the destination** — what reaching the end looks like, derived from the idea, the repo, and the principles. Don't grill for it. If the idea admits several destinations, take the smallest one still worth a map (principle 2) and note in Notes which ones you set aside.
2. **Sweep breadth-first** for the open decisions: the repo, its docs, its recent history, its existing maps. Fan out across the space rather than deep on one thread.
3. **No fog found** — the way is already clear and the whole job fits one session — means no map. Say so and stop.
4. **Create the map**, then the tickets you can phrase now, then wire the blocking edges in a second pass (issues need ids before they can reference each other). Everything you can't phrase yet stays as fog.
5. **Fire the research subagents** in parallel, one per research ticket.
6. Stop, and print the command to work the map. Charting resolves nothing — a run that invents a question and answers it in the same context hasn't used the map, it has written a transcript.

## Work the map

`/wayfinder-auto <map> [<ticket>] [steer: <text>]`

A named ticket scopes the run to it and its unblocks-subtree; no ticket means the whole map. Either way, work in **dependency order**, claiming each ticket as you reach it — a crash then leaves only live claims stale.

Per ticket:

1. Load the map (low-res; zoom into closed tickets on demand). Claim the ticket. If it was already claimed, read its trail first — the last `### handoff`, then the breadcrumbs after it — and open with a breadcrumb noting resumption.
2. Hand it to a fresh subagent with the ticket body, the map's Decisions-so-far, the principles, and the skill its type calls for. Build tickets run the [LIFECYCLE.md](../wayfinder/LIFECYCLE.md) stages with gates between; everything else resolves in one subagent.
3. Breadcrumb each decision-grade moment on the ticket — a sub-decision settling, a direction changing, a stage transition, a gate result.
4. **Record**: a resolution comment opening `**agent-decided (<principle>):**` and carrying the reasoning that would have been the grilling; close the ticket; append its one-line gist to Decisions-so-far with an *(agent)* suffix.
5. **Advance the map**: graduate whatever fog the answer made specifiable into fresh tickets — including slicing new `wayfinder:build` tickets — clear those fog patches, and retire tickets the answer invalidated.

Then take the next unblocked ticket. The run ends when the frontier is empty, everything left is parked, or the budget runs out — and it ends with a summary: what closed, what parked, what is still open.

A ticket found **effectively complete** — overtaken by another decision, or done as a side effect — skips to step 4: comment why, close, index.

## Where it stops

Park a ticket — a `### handoff` comment on it, then carry on with other unblocked work — when resolving it would:

- need **human hands**: credentials, a purchase, physical access, an account only they can open;
- **redraw a destination a human wrote** (one you charted yourself you may redraw, saying so on the map);
- **contradict** a Decisions-so-far entry rather than extend it;
- **merge** — an unattended lifecycle ends at *in review, approved*, unless the steering line explicitly grants merge-when-green.

Report every park in the closing summary.

Ruling work **out of scope** is not a park: scope calls come up constantly and blocking on each would stall the run. Close the ticket, leave a line in **Out of scope** saying why, and keep going.

A `steer:` line composes *after* the principles — it narrows scope or states preferences. It cannot lift these stops.

## Journal

Breadcrumbs are append-only ticket comments prefixed `**breadcrumb:**`, one or two lines, never edited, never deleted — decision-grade moments only, not narration. On deliberate exit, one `### handoff` comment: where we are, the open thread, the first move on resume. A crash leaves no handoff and the breadcrumb trail is the fallback. Unattended runs are the case this exists for: the trail is the only witness, and re-entry from it is idempotent.
