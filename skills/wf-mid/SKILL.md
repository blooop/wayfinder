---
name: wf-mid
description: Chart and drive a wayfinder map mostly alone — research and prototype first, settle whatever the guiding principles settle, and spend a small number of high-value questions on the decisions that are genuinely the human's: taste, scope, and anything expensive to reverse. Use when the user wants a map worked with few interruptions, wants the obvious calls made without being asked, or when wf launches a node in mid mode.
---

## Cross-agent invocation

This bundle runs in both Claude Code and Codex. Mention a wayfinder skill with
`/` in Claude Code and `$` in Codex; skill names below are written without a
sigil.

Same artifact as `wf` and `wf-auto` — one map issue with decision tickets as its children — worked from between them. `wf` asks the human about every decision; `wf-auto` asks about none. Most decisions on a real map have one defensible answer under the principles below, and two or three do not. This skill is for spending the human's attention on those two or three.

Two things change from `wf-auto`, and everything else follows:

- **Escalation is earned, not default.** Every decision gets an honest attempt against the principles first. The human is asked only where the attempt genuinely fails — and the [escalation test](#the-escalation-test) is what "genuinely" means, so it is a rule rather than a mood.
- **Planning is the default, as in `wf`.** The map ends where the way is clear. Execution is in scope only when the map's `## Notes` or a `steer:` line says so — that grant is the difference between a run that plans a map and a run that builds it, and it is the largest single thing this skill spends.

Not for work with no fog in it: one known piece of work is `wf-one`, and a map whose every decision you want taken for you is `wf-auto`.

## The principles

In order. When two pull opposite ways, the earlier wins. Identical to `wf-auto`'s — the same standing voice, so a map can be handed between the two skills without its route changing character mid-way.

1. **Long-term maintainability** — decide for whoever reads this in a year, not for the session in progress.
2. **Simplicity** — the smallest thing that resolves the question. Deleting beats adding; one concept beats two that overlap.
3. **Constructive modeling** — illegal states unrepresentable, sum types over tag-plus-parallel-fields, no sentinels. Run `/constructive-modeling` on any ticket that shapes a type.
4. **Test-first** — behavior arrives with a failing test that proves it was absent. Build tickets go through `wf-tdd`; a decision ticket says how its outcome will be tested.

A map's `## Notes` may add principles or reorder these for that effort; a ticket may not. Where the principles are silent, the route already walked is the tiebreak: pick what the map's Decisions-so-far already implies.

## The escalation test

A decision is the human's **only** if it survives all three of these. Anything that fails one is yours, and taking it is the job — an agent that escalates a decision the principles already answered has only moved its own work onto the human.

1. **The legwork is done.** Never ask what a read can answer or a prototype can show. Research subagents fire and *land* before any question they touch; a prototype gets built and judged before its question is asked. A question asked over missing evidence is answered by whoever guesses best, which is the opposite of why it was asked.
2. **The principles do not settle it.** Applied in order, two or more options are still live, and the choice between them is not implied by the route already walked. Write the attempt down before deciding it failed — most "ambiguous" decisions resolve the moment simplicity is applied honestly.
3. **And it is one of:**
   - a **taste or product** call — how the thing should feel, user-visible naming and wording, which of two real needs matters more;
   - **expensive to reverse** — a public interface, an on-disk or wire format, a dependency, a released name, a data migration: cheap-to-reverse decisions are yours even when they are close, because the recovery is a later commit;
   - or it **redraws the destination**, or contradicts a Decisions-so-far entry rather than extending it.

Everything else is agent-decided. A **close call is recorded as a close call** — a breadcrumb saying what nearly won and why it lost — so a human reopening an agent-decided ticket to re-decide it is normal, not a conflict. That trail is what makes deciding alone safe: the decision is cheap to revisit because the reasoning is on the ticket, not in a session that ended.

## How to ask

An escalation is a **decision brief**, not an interrogation. `wf`'s job is to grill; this skill has already done the thinking, and what it needs is a ruling. One question, one decision, carrying:

- **what was already tried** — the research read, the prototype built, the options ruled out and by which principle;
- **the live options**, and the consequence of each in the terms the choice actually turns on;
- **the recommendation** and the principle behind it;
- **the cost of being wrong** — what a later reversal costs, which is usually what makes it worth asking at all.

"Your call" is a valid answer and means the recommendation lands: say so, record it as human-confirmed, and move on. Never ask two things at once, never ask a question whose answer would not change what happens next, and never ask one whose answer you already intend to override.

**The budget.** Aim for a small number of escalations per run — one or two on most maps. If the count keeps climbing, that is not a reason to keep asking: it is evidence the **destination** is wrong, and grinding through its consequences one question at a time will not fix it. Stop, say so, and confirm the destination instead.

## The map

One issue labelled `wayfinder:map`; its tickets are its child issues. The map is an **index**: it gists each closed ticket in one line and links it, so the detail lives in exactly one place — the ticket. Refer to maps and tickets by their titles, never bare numbers; a name wraps its link.

```markdown
## Destination

<what reaching the end of this map looks like. One or two lines; every run orients to it first.>

## Notes

<domain; skills every run should consult; extra or reordered principles for this effort; whether execution is in scope>

## Decisions so far

- [<closed ticket title>](link) — <one-line gist of the answer> *(agent)*

## Not yet specified

<!-- in-scope fog you can't phrase sharply yet; graduates into tickets as the frontier advances -->

## Out of scope

<!-- work ruled past the destination; closed, never graduates -->
```

A ticket is a child issue whose body is one `## Question`, sized to a single session, labelled `wayfinder:<type>`. Escalated decisions need no label of their own — a `grilling` ticket is a `grilling` ticket whether the answer came from the principles or from the human, and the resolution comment already says which.

Blocking is the tracker's native dependency edge, so the frontier renders in the tracker's own UI. The **frontier** is the open, unblocked, unclaimed children. Claiming is assigning the ticket to yourself, before any work on it.

**Not yet specified** is in-scope fog you can't yet phrase sharply; **Out of scope** is work past the destination. Ticket it when the question is already sharp, even if blocked and unactionable; leave it as fog when it isn't.

A launch from `wf` reads `<sigil>wf-mid <map> [<ticket>] ctx: <json> [steer: <text>]`. The block is the snapshot `wf` already held — the map's title, and for a ticket its type, stage and linked PRs — so a run can open on the node it was handed without rediscovering it. It is an **accelerator, never a precondition**: orient from it, verify live before any write, ignore it entirely if it is absent or disagrees with your arguments, and never let it survive into a subagent's own invocation unexamined. See **The launch context** in [GITHUB_TRACKER.md](../wf/GITHUB_TRACKER.md).

Tracker mechanics — pinning the repo, creating and parenting issues, blocking edges, the frontier query, the local-markdown fallback — live in [GITHUB_TRACKER.md](../wf/GITHUB_TRACKER.md) in the sibling `wf` skill. Pin the repo from the working repo's own remote, pass it explicitly on every call, and push branches by name: a fork's map belongs to the fork, never its parent. State the repo and its visibility in your first line of output — a map on a public repo publishes the whole fog sketch, and a run that will make most of its own decisions is not one the human is watching closely enough to catch that later.

## Where the work happens

The human is in the loop here, so the split is not `wf-auto`'s. Subagents are for work whose **bulk** would otherwise crowd the run's context; the run keeps the decisions, because a conversation cannot be held through a subagent.

| ticket type | resolved | why |
| --- | --- | --- |
| **research** | fresh `/research` subagent, in parallel | bulky reading, and it must land before any question it touches |
| **prototype** | built alone via `/prototype`, judged against the principles | the human sees it only when the judgement is a taste call — and then the artifact *is* the question, which is the cheapest question there is |
| **grilling** | in this run's own context | the attempt is yours; the escalation, if it is earned, is a live exchange |
| **task** | alone where it can be; a precise checklist where it needs human hands | |
| **build** | only under the execution grant, then [LIFECYCLE.md](../wf/LIFECYCLE.md) — `wf-tdd` and `wf-review` in fresh subagents, gates checked between | fresh review context is a correctness requirement, not a luxury: the reviewer must never have written the code |

That split is the cost story: subagents where context isolation is load-bearing, inline where the human already is, and no build fan-out at all unless the map asked for it.

## Chart

`wf-mid <loose idea>`

1. **Draft the destination alone** — from the idea, the repo, its docs, its recent history and its existing maps. Don't grill for it. If the idea admits several, take the smallest one still worth a map (principle 2) and note which ones you set aside.
2. **Sweep breadth-first** for the open decisions: fan out across the space rather than deep on one thread.
3. **No fog found** — the way is already clear and the whole job fits one session — means no map. Say so and stop; `wf-one` is the shape that work wants.
4. **Fire the research subagents** for whatever the sweep showed is unknown-and-readable. They run while step 5 happens.
5. **Confirm the destination — once.** This is the highest-value question on any map, and the one escalation worth making before a single ticket exists: the destination as drafted, the fog sketch, and what you set aside, in the decision-brief form above. Everything downstream inherits it, so an hour of agent-decided tickets under the wrong destination is the most expensive mistake available.
6. **Create the map**, then the tickets you can phrase now, then wire the blocking edges in a **second pass** (issues need ids before they can reference each other). Everything you can't phrase yet stays as fog.
7. Stop, and print the command to work the map. Charting resolves nothing — a run that invents a question and answers it in the same context hasn't used the map, it has written a transcript.

## Work the map

`wf-mid <map> [<ticket>] [steer: <text>]`

A named ticket scopes the run to it and its unblocks-subtree; no ticket means the whole map. Either way, work in **dependency order** and resolve **as many tickets as the run can hold** — that is what this skill is for, and the reason it is not `wf`, which stops after one.

Per ticket:

1. Load the map (low-res; zoom into closed tickets on demand). Claim the ticket. If it was already claimed, read its trail first — the last `### handoff`, then the breadcrumbs after it — and open with a breadcrumb noting resumption.
2. Resolve it where the table above says it is resolved. Run the escalation test before asking anything; the attempt against the principles is written down either way, because it is the resolution comment when it succeeds and the brief when it does not.
3. Breadcrumb each decision-grade moment on the ticket — a sub-decision settling, a close call and what nearly won, a direction changing, a stage transition, a gate result.
4. **Record** the resolution, and say where it came from:
   - `**agent-decided (<principle>):**` — yours, carrying the reasoning that would have been the grilling. Gist it into Decisions-so-far with an *(agent)* suffix.
   - `**human-decided:**` — escalated, carrying the question as asked and the answer as given. No suffix: the map should show at a glance which decisions were bought cheaply and which cost the human something.

   Then close the ticket.
5. **Advance the map**: graduate whatever fog the answer made specifiable into fresh tickets — slicing `wayfinder:build` tickets only where execution is granted — clear those fog patches, and retire tickets the answer invalidated.

Then take the next unblocked ticket. The run ends when the frontier is empty, everything left is parked, or the context is too full to hold another ticket honestly — and it ends with a summary: what closed, what was asked, what parked, what is still open.

A ticket found **effectively complete** — overtaken by another decision, or done as a side effect — skips to step 4: comment why, close, index.

## Where it stops

Park a ticket — a `### handoff` comment on it, then carry on with other unblocked work — when resolving it would:

- need an **answer that hasn't come**. An earned escalation that goes unanswered parks; it does not fall through to being decided alone. That fall-through is the whole difference between this skill and `wf-auto`, and quietly taking it would make every question this skill asks theatre.
- need **human hands**: credentials, a purchase, physical access, an account only they can open;
- **redraw a destination a human wrote** — including the one they confirmed at charting;
- **merge**: the lifecycle ends at *in review, approved* unless the steering line explicitly grants merge-when-green.

Report every park in the closing summary.

Ruling work **out of scope** is not a park and not an escalation: close the ticket, leave a line in **Out of scope** saying why, and keep going. Scope calls come up constantly and blocking on each would spend the budget on the cheapest questions on the map.

A `steer:` line composes *after* the principles — it narrows scope, states preferences, or grants execution. It cannot lift these stops.

## Journal

Breadcrumbs are append-only ticket comments prefixed `**breadcrumb:**`, one or two lines, never edited, never deleted — decision-grade moments only, not narration. On deliberate exit, one `### handoff` comment: where we are, the open thread, the first move on resume. A crash leaves no handoff and the breadcrumb trail is the fallback. Re-entry from the trail is idempotent.

The trail carries more weight here than in `wf`, because most decisions on this map were taken without the human watching: the breadcrumb saying what nearly won is what lets them audit a run they weren't in, and reopen the one call they'd have made differently.

## Clean up what this run finished

Only where the run executed. If it closed build tickets of its own, its last act — once the summary is settled — is to collect those workspaces and nothing else:

```
wf reap --finished <owner/repo#n> [<owner/repo#n> ...]
```

Name every ticket this run closed, in full, owner included. A run that closed no build tickets runs nothing. Report what was removed and what was kept, both: a workspace stays when its branch never reached a remote, and that line is the only notice anyone gets that the work exists in one place only. **Never** reach past a refusal — not `-f`, which waives the guard the step rests on, and not an unscoped `wf reap -y`, which is a person's command over their whole machine rather than one run tidying up after itself.
