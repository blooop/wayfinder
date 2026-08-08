---
name: wayfinder
description: Plan a huge chunk of work — more than one agent session can hold — as a shared map of decision tickets on your issue tracker, and resolve them one at a time until the way to the destination is clear.
disable-model-invocation: true
---

A loose idea has arrived — too big for one agent session, and wrapped in fog: the way from here to the **destination** isn't visible yet. Wayfinding is about finding that way, not charging at the destination. This skill charts the way as a **shared map** on the repo's issue tracker, then works its **decision tickets** — questions whose resolution is a decision, not slices of a build to execute — one at a time until the route is clear.

The destination varies per effort, and naming it is the first act of charting — it shapes every ticket. It might be a spec to hand off and iterate on, a decision to lock before planning starts, or a change made in place like a data-structure migration. The map is domain-agnostic — engineering work, course content, whatever fits the shape.

## Plan, don't do

Wayfinder is **planning** by default: each ticket resolves a decision, and the map is done when the way is clear — nothing left to decide before someone goes and does the thing. The pull to just do the work is usually the signal you've reached the edge of the map and it's time to hand off. An effort can override this in its **Notes** — carrying execution into the map itself — but absent that, produce decisions, not deliverables.

## Refer by name

Every map and ticket is an issue, so it has a **name** — its title. In everything the human reads — narration, the map's Decisions-so-far — refer to it by that name, never by a bare id, number, or slug. A wall of `#42, #43, #44` is illegible; names read at a glance. The id and URL don't vanish — a name wraps its link — but they ride *inside* the name, never stand in for it.

## The Map

The map is a single issue on this repo's issue tracker, labelled `wayfinder:map` — the canonical artifact. Its tickets are child issues of the map.

The map is an **index**, not a store. It lists the decisions made and points at the tickets that hold their detail; a decision lives in exactly one place — its ticket — so the map never restates it, only gists it and links.

**Where the map, its child tickets, blocking, and frontier queries physically live is tracker-specific.** The default tracker is **GitHub Issues via `gh`** — consult [GITHUB_TRACKER.md](GITHUB_TRACKER.md) in this skill for the exact operations. If the repo has no GitHub remote, or has issues disabled and the human declines to enable them, fall back to the local-markdown tracker described at the bottom of that file.

### Keep the map inside its repo

The map belongs to **one** repo — the one being mapped — and every write goes there explicitly, never wherever the tooling happens to resolve to. Resolve that repo once at the start of a session, from the working repo's own remote, name it to the human before the first write, and pass it to every issue command and every branch push; see **Pin the repo first** in [GITHUB_TRACKER.md](GITHUB_TRACKER.md). Never move a map, a ticket, or a resolution to a second repo — no mirroring to a public planning repo, no cross-repo issue references carrying the substance of a decision.

**A fork is its own repo.** Its map, tickets, and research branches belong on the fork being mapped, never on its parent — `gh` and `git push` both drift upstream when left to their defaults, and the parent is the repo you have least claim on. See **Forks** in [GITHUB_TRACKER.md](GITHUB_TRACKER.md).

A map is thinking out loud — a Destination, a fog sketch, half-formed questions — and the tracker of a public repo publishes all of it. If the resolved repo is **public**, say so and confirm with the human before the first write; see **Pin the repo first** in [GITHUB_TRACKER.md](GITHUB_TRACKER.md). That's a check on where tickets are *filed*, not on what they may discuss: research tickets search the open web with the real question, identifiers and all.

### The map body

The whole map at low resolution, loaded once per session. Open tickets are **not** listed — they are open child issues, found by query.

```markdown
## Destination

<what reaching the end of this map looks like — the spec, decision, or change this effort is finding its way to. One or two lines; every session orients to it before choosing a ticket.>

## Notes

<domain; skills every session should consult; standing preferences for this effort>

## Decisions so far

<!-- the index — one line per closed ticket: enough to judge relevance, then zoom the link for the detail the ticket holds -->

- [<closed ticket title>](link) — <one-line gist of the answer>

## Not yet specified

<!-- see "Fog of war": in-scope fog you can't ticket yet; graduates as the frontier advances -->

## Out of scope

<!-- see "Out of scope": work ruled beyond the destination; closed, never graduates -->
```

### Tickets

Each ticket is a **child issue** of the map; the tracker's issue id is its identity. Its body is the question, sized to one 100K token agent session:

```markdown
## Question

<the decision or investigation this ticket resolves>
```

Each ticket carries a `wayfinder:<type>` label — one of `research`, `prototype`, `grilling`, `task`, `build` (see [Ticket Types](#ticket-types)).

A session **claims** a ticket by assigning it to the dev driving the map, **first**, before any work. That assignee _is_ the claim: an open, unassigned ticket is unclaimed. A claim gates only *autonomous* choice — a session picking its own work skips claimed tickets to avoid colliding with a concurrent session; a human naming any open ticket is always valid, claimed or not (resuming a claimed ticket starts with the [re-entry ritual](#breadcrumbs-handoffs-re-entry)).

Blocking uses the tracker's **native** dependency relationship — essential because it renders the frontier _visually_ in the tracker's own UI, so the human sees what's takeable without opening the map. Only a tracker that lacks native blocking falls back to a body convention. A ticket is **unblocked** when every ticket blocking it is closed; the **frontier** is the open, unblocked, unclaimed children — the edge of the known.

The answer isn't part of the body — it's recorded on resolution (see [Work through the map](#work-through-the-map)). Assets created while resolving a ticket are linked from the issue, not pasted in.

## Ticket Types

Every ticket is either **HITL** — human in the loop, worked *with* a human who speaks for themselves — or **AFK**, driven by the agent alone. A HITL ticket only resolves through that live exchange; the agent never stands in for the human's side of it (a grilling agent that answers its own questions has broken this). The single exception is a **deferred launch** (see [Deferred mode](#deferred-mode)), where the human has explicitly handed judgement to the agent for a subtree.

- **Research** (AFK): Reading documentation, third-party APIs, or local resources like knowledge bases to surface a fact a decision waits on. Resolved by a `/research` **subagent**. Use when knowledge outside the current working directory is required.
- **Prototype** (HITL): Raise the fidelity of the discussion by making a cheap, rough, concrete artifact to react to — an outline, a rough take, a stub, or UI/logic code via the /prototype skill. Links the prototype as an asset. Use when "how should it look" or "how should it behave" is the key question.
- **Grilling** (HITL): Conversation via the /grill-me and /constructive-modeling skills, one question at a time; pull in /ubiquitous-language when the decision hinges on naming or contested terms. The default case.
- **Task** (HITL or AFK): Manual work that must happen before a *decision* can be made — nothing to decide, prototype, or research, but the discussion is blocked until it's done. Signing up for a service so its API can be judged, provisioning access, moving data so its shape can be seen. This is the one type that *does* rather than decides — and it earns its place by unblocking a decision, not by delivering the destination. The agent drives it alone where it can (AFK); otherwise it hands the human a precise checklist (HITL). Resolved when the work is done; the answer records what was done and any resulting facts (credentials location, new URLs, row counts) later tickets depend on.
- **Build** (AFK): An execution slice — code to write, sized to one fresh agent session, on a map whose Notes carry the execution override. Worked via the `/tdd` skill and reviewed via `/review`; its lifecycle position (**ready → building → in review → needs attention → done**) is *derived from its linked PRs*, never declared — see [LIFECYCLE.md](LIFECYCLE.md) for the stage lattice, gates, and the manager protocol. Review is a **stage** of a build ticket, not a ticket type.

## Fog of war

The map is _deliberately_ incomplete: don't chart what you can't yet see. Beyond the live tickets lies the **fog of war** — the dim view of decisions and investigations you can tell are coming but can't yet pin down, because they hang on questions still open. Resolving a ticket clears the fog ahead of it, graduating whatever's now specifiable into fresh tickets — one at a time, until the way to the destination is clear and no tickets remain.

The map's **Not yet specified** section is where that dim view is written down: the suspected question, the area to revisit later. It's the undiscovered frontier _toward_ the destination — everything here is in scope, just not sharp enough to ticket. Write as loosely or as fully as the view allows; it doubles as a signpost for collaborators reading where the effort is headed.

**Fog or ticket?** The test is whether you can state the question precisely now — _not_ whether you can answer it now.

- **Ticket when** the question is already sharp — even if it's blocked and you can't act on it yet.
- **Not yet specified when** you can't yet phrase it that sharply. Don't pre-slice the fog into ticket-sized pieces: it's coarser than a ticket, and one patch may graduate into several tickets, or none, once the frontier reaches it.

**Not yet specified** excludes what's already decided (Decisions so far), what's already a live ticket, and what's out of scope (the next section).

## Out of scope

Fog only ever gathers _toward_ the destination. The destination fixes the scope, so work beyond it is **out of scope** — it isn't fog, and it doesn't belong in **Not yet specified**. It gets its own **Out of scope** section on the map: work you've consciously ruled out of _this_ effort. Scope, not sharpness, lands it here.

Out-of-scope work never graduates — the frontier stops at the destination — so it returns only if the destination is redrawn, and then as a fresh effort, not a resumption.

Ruling something out of scope is a scoping act, not a step on the route. When a ticket that already exists turns out to sit past the destination — mis-scoped in while charting, or exposed by a resolution — **close it** (a closed ticket is unambiguously off the frontier) and leave one line in the **Out of scope** section: the gist plus why it's out of scope, linking the closed ticket. It stays out of **Decisions so far**, which records the route actually walked — a scope boundary isn't a step on it.

## Breadcrumbs, handoffs, re-entry

Continuation is re-entry from tracked state, so a working session journals onto its ticket — see [GITHUB_TRACKER.md](GITHUB_TRACKER.md) for the operations:

- **Breadcrumbs** — append-only comments, prefixed `**breadcrumb:**`, at decision-grade moments: a sub-decision settles, the direction changes, a blocking question is surfaced and parked. One or two lines each, not narration. Never edited, never deleted.
- **Handoff** — on deliberate exit (the human ends or detaches the session on purpose), one comment headed `### handoff` with three parts: *where we are*, *the open thread*, *what to do first on resume*. A crash simply means no handoff — the breadcrumb trail is the fallback.
- **Re-entry ritual** — a session resuming a claimed ticket reads: ticket body → the **last** handoff → any breadcrumbs after it (the whole trail if no handoff exists) → the map's Decisions-so-far as needed. Its first write is a breadcrumb noting resumption; its first words to the human confirm the open thread in one line. Re-entry is idempotent — resuming a ticket whose session actually died costs nothing.

Resolution comments are unchanged — the answer still lands once, at close. And close what is actually done: a session that finds a ticket **effectively complete** — overtaken by another decision, or done as a side effect — records why in a comment and closes it.

## Invocation

Two modes, plus a deferred variant of the second. **Never resolve more than one ticket per session** — with two exceptions: research tickets, and a **deferred launch**, which may work its named ticket's whole subtree.

The invocation grammar (what `wf`'s launch line produces):

- `/wayfinder <map> [<ticket>]` — interactive (everything below as written)
- `/wayfinder <map> <ticket> defer` — deferred subtree (see [Deferred mode](#deferred-mode))
- `/wayfinder <map> <ticket> defer: <steering>` — deferred, with a steering prompt
- `/wayfinder <map> <ticket> steer: <steering>` — interactive, with a steering prompt

### Chart the map

User invokes with a loose idea.

1. **Name the destination.** Run a `/grill-me` and `/constructive-modeling` session to pin down what this map is finding its way to — the spec, decision, or change. The destination fixes the scope, so it's settled first.
2. **Map the frontier.** Grill again, **breadth-first** this time: fan out across the whole space rather than deep on any one thread, surfacing the open decisions and the first steps takeable now. **If this surfaces no fog** — the way to the destination is already clear, the whole journey small enough for one session — you don't need a map. Stop and ask the user how they'd like to proceed.
3. **Create the map** (label `wayfinder:map`): Destination and Notes filled in, Decisions-so-far empty, the fog sketched into **Not yet specified**. **Pin and name the target repo first** — this is the session's first write, and it carries the Destination and the whole fog sketch (see [Keep the map inside its repo](#keep-the-map-inside-its-repo)).
4. **Create the tickets you can specify now** as child issues of the map — then wire blocking edges in a **second pass** (issues need ids before they can reference each other). Wiring sorts them into the frontier and the blocked; everything you can't yet specify stays in the fog — the **Not yet specified** section.
5. **Fire the research subagents.** For each `research` ticket you just created, spin up a `/research` subagent to resolve it in parallel, capturing its findings on a throwaway `research/<name>` branch — pushed by name to the pinned remote, never with a bare `git push` — with a context pointer from the ticket.
6. Stop — charting is one session's work; it hand-resolves nothing.

### Work through the map

User invokes with a map (URL or number). A ticket is **optional** — without one, you pick the next decision, not the user.

1. Pin the repo the map lives in, then load the **map** — the low-res view, not every ticket body. A map given as a bare number resolves against the pinned repo; a map given as a URL names its own repo, and that repo is the one every write of this session targets.
2. Choose the ticket. If the user named one, use it — a human may name any open ticket, claimed or not; claims only steer the autonomous case. Otherwise take the first frontier ticket in order. **Claim it**: assign it to yourself before any work. If the ticket was already claimed, run the **re-entry ritual** first (see [Breadcrumbs, handoffs, re-entry](#breadcrumbs-handoffs-re-entry)).
3. Resolve it — **zoom as needed**: fetch the full body of any related or closed ticket on demand; invoke the skills the `## Notes` block names. If in doubt, use `/grill-me` and `/constructive-modeling`. Drop a `**breadcrumb:**` comment at each decision-grade moment along the way. A ticket found **effectively complete** skips to closing: comment why, close it, and index it as usual.
4. Record the resolution: post the answer as a **resolution comment**, **close** the issue, and **append a context pointer** to the map's Decisions-so-far.
5. Add newly-surfaced tickets (create-then-wire); graduate any fog the answer has made specifiable, clearing each graduated patch from **Not yet specified** so it lives only as its new ticket. If the answer reveals a ticket — this one or another — sits beyond the destination, **rule it out of scope** rather than resolving it on the route. If the decision invalidates other parts of the map, update or delete those tickets.

If the session ends deliberately before the resolution lands, post the `### handoff` comment on the ticket before you go — where we are, the open thread, the first move on resume.

The user may run unblocked tickets in parallel, so expect other sessions to be editing the tracker concurrently.

### Deferred mode

A **deferred launch** (`defer` in the invocation) hands judgement to the agent for the named ticket **plus everything in its rendered unblocks-subtree**, worked in dependency order. The human chose this at launch time, standing in front of the tree — that consent is what lifts the HITL rules below, and *only* for this run.

**The standing default prompt** for every deferred run: decide with best judgment in the spirit of the map's Decisions-so-far; prefer the smallest decision that unblocks the subtree; record every resolution flagged `**agent-decided:**` with the reasoning that would have been the grilling. A steering prompt (`defer: <text>`) composes *after* this default — it narrows scope or states preferences; it cannot lift stop conditions.

**Claiming is as-you-go**: the launch claims the root; each subtree ticket is claimed when the agent reaches it — a crash leaves only live claims stale, and concurrent sessions still see honest assignees.

**HITL types under defer:** grilling → the agent answers its own questions from the map, repo, and research, flagged agent-decided (this is precisely what deferring judgement means — the self-answering ban is lifted *only* here); prototype → build the artifact and pick, recording the reaction as judgment; a task needing human hands → park it (stop condition c). Build tickets run their lifecycle under the manager protocol in [LIFECYCLE.md](LIFECYCLE.md) — fresh subagent per stage, gates between.

**Stop conditions — park instead of deciding** when a resolution would: (a) redraw the Destination, (b) rule work out of scope, (c) need credentials, purchases, or human-only action, (d) contradict an existing Decisions-so-far entry. Parking = a `### handoff` comment on that ticket; the run continues with other unblocked subtree tickets and reports every park at the end.

**Audit trail:** resolution comments open with `**agent-decided:**`; the map's Decisions-so-far line gets an *(agent)* suffix. A human later re-opening an agent-decided ticket to re-decide it is normal, not a conflict.
